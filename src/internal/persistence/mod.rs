//! Workspace persistence — save/load `.varda/` directory.
//!
//! The workspace is the current working directory. All state lives in `.varda/`:
//! - `scene.json` — channels, decks, effects, modulation (show-specific, shareable)
//! - `stage.json` — surfaces, outputs, warp, editor prefs (venue-specific)
//! - `midi.json`  — MIDI controller mappings (device-name-keyed)
//! - `presets/`   — saved deck and channel presets

pub mod presets;
pub mod writer;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Atomic file write: writes to a `.tmp` sibling then renames into place.
/// Prevents data loss if the process crashes mid-write.
///
/// # Errors
///
/// Returns an error if the temporary sibling file cannot be written (missing
/// parent directory, permissions, disk full) or if renaming it over `path`
/// fails.
pub fn atomic_write<P: AsRef<Path>>(path: P, content: &str) -> Result<()> {
    let path = path.as_ref();
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, content)
        .with_context(|| format!("Failed to write temp file: {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("Failed to rename {} → {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Stage configuration persisted in `.varda/stage.json`.
/// Contains venue-specific data: surfaces, outputs, and editor preferences.
/// Kept separate from scene.json so users can share deck layouts without stage geometry.
// Flags are independent persisted UI toggles; bundling them would change scene/stage JSON.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagePrefs {
    #[serde(default = "default_grid_size")]
    pub grid_size: f32,
    #[serde(default = "default_true")]
    pub snap: bool,
    #[serde(default)]
    pub library_panel_open: bool,
    #[serde(default = "default_true")]
    pub right_panel_open: bool,
    #[serde(default)]
    pub stage_editor_open: bool,
    #[serde(default)]
    pub dome_preview_open: bool,
    /// Whether the stage editor is in 3D Dome mode
    #[serde(default)]
    pub dome_mode_active: bool,
    /// Active dome preset
    #[serde(default = "default_dome_preset")]
    pub dome_preset: crate::renderer::slicer::DomePreset,
    /// Active dome geometry
    #[serde(default)]
    pub dome_geometry: crate::renderer::slicer::DomeGeometry,
    /// Size the domemaster is rendered at. Stage-level because it belongs to the
    /// dome being projected onto, not to the scene playing on it.
    #[serde(default)]
    pub domemaster_resolution: crate::renderer::dome::DomemasterResolution,
    /// 2D stage surface layout
    #[serde(default)]
    pub surfaces: crate::surface::SurfaceManager,
    /// Output window configurations (surface assignments, warp calibration)
    #[serde(default)]
    pub outputs: Vec<crate::scene::OutputConfig>,
    /// Which incoming SMPTE signal the transport follows, and where LTC is
    /// patched. Venue data: the cable belongs to the room, not to the show.
    #[serde(default)]
    pub timecode: crate::timecode::TimecodeConfig,
    /// Production buses. Venue data for the same reason as
    /// `domemaster_resolution`: a bus canvas is dictated by the LED processor
    /// or screen it feeds, not by the show playing on it.
    ///
    /// `None` means the field was absent, i.e. a stage file written before
    /// buses existed — the loader keeps its default bank. `Some(vec![])` means
    /// the operator deliberately removed every bus, which must reload as empty
    /// rather than silently regaining the legacy bus.
    #[serde(default)]
    pub buses: Option<Vec<crate::bus::BusConfig>>,
    /// The rundown: the steps of the show in order, and where it is. Kept
    /// with the buses its steps set. `None` (a stage from before the
    /// rundown, or with no steps) opens an empty one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rundown: Option<crate::rundown::RundownConfig>,
    /// The audio mixer: buses and the devices they play on, inputs, and every
    /// strip's fader, mute, sends and follow. Venue data — the sound devices
    /// belong to the room. `None` (a stage from before the mixer) loads a
    /// fresh mixer with one Master bus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_mix: Option<crate::audio_mix::AudioMixConfig>,
    /// The stream: what is sent, and the four destinations with their URLs,
    /// keys and logins. Venue data — the client's streaming accounts belong
    /// to the event, not to the show. Kept in the clear, as vMix keeps them,
    /// in the workspace folder only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<crate::streaming::StreamSettings>,
    /// Recording: folder, format, codec, quality. Venue data, like the
    /// stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<crate::record::RecordSettings>,
    /// Which panels the control window shows and where, in the UI's own
    /// format (`usecases::ui::screen_layout::ScreenLayout`) — held as plain
    /// JSON so the stage stays free of presentation types. The operator's
    /// saved layouts are kept for the user instead. `None` (a stage from
    /// before layouts) opens Classic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen_layout: Option<serde_json::Value>,
    /// Channels folded to a narrow strip in the Broadcast layout's Inputs, by UUID.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collapsed_channels: Vec<String>,
}

fn default_grid_size() -> f32 {
    0.05
}
fn default_true() -> bool {
    true
}
fn default_dome_preset() -> crate::renderer::slicer::DomePreset {
    crate::renderer::slicer::DomePreset::Quad
}

impl Default for StagePrefs {
    fn default() -> Self {
        Self {
            grid_size: 0.05,
            snap: true,
            library_panel_open: false,
            right_panel_open: true,
            stage_editor_open: false,
            dome_preview_open: false,
            dome_mode_active: false,
            dome_preset: crate::renderer::slicer::DomePreset::Quad,
            dome_geometry: crate::renderer::slicer::DomeGeometry::default(),
            domemaster_resolution: crate::renderer::dome::DomemasterResolution::default(),
            surfaces: crate::surface::SurfaceManager::default(),
            outputs: Vec::new(),
            timecode: crate::timecode::TimecodeConfig::default(),
            buses: None,
            rundown: None,
            audio_mix: None,
            stream: None,
            record: None,
            screen_layout: None,
            collapsed_channels: Vec::new(),
        }
    }
}

impl StagePrefs {
    /// Validate stage prefs for semantic correctness. Returns a list of errors.
    /// An empty list means the config is valid.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if !self.grid_size.is_finite() || self.grid_size <= 0.0 {
            errors.push(format!(
                "grid_size {} must be > 0 and finite",
                self.grid_size
            ));
        }
        for (i, output) in self.outputs.iter().enumerate() {
            let prefix = format!("outputs[{i}]");
            if output.name.trim().is_empty() {
                errors.push(format!("{prefix}: name is empty"));
            }
        }
        // Warp now lives on surfaces — validate their corner-pin finiteness.
        for (i, surface) in self.surfaces.surfaces.iter().enumerate() {
            if let Some(crate::renderer::warp::WarpMode::CornerPin { corners }) = &surface.warp {
                for (c, corner) in corners.iter().enumerate() {
                    for (k, v) in corner.iter().enumerate() {
                        if !v.is_finite() {
                            errors.push(format!(
                                "surfaces[{i}]: warp corner[{c}][{k}] is not finite"
                            ));
                        }
                    }
                }
            }
        }
        // Bus identity is what every bus-addressed command and HTTP route uses.
        // An empty or duplicated UUID produces a bus nothing can address, so it
        // is a load-boundary error rather than a warning.
        if let Some(buses) = &self.buses {
            let mut seen_uuid = std::collections::HashSet::new();
            let mut seen_audio = std::collections::HashSet::new();
            for bus in buses {
                if bus.uuid.trim().is_empty() {
                    errors.push(format!("bus '{}' has an empty uuid", bus.name));
                } else if !seen_uuid.insert(bus.uuid.clone()) {
                    errors.push(format!("duplicate bus uuid '{}'", bus.uuid));
                }
                // Compare *effective* identity: an absent `audio_mix` derives
                // from the bus UUID, so `{uuid:"m", audio_mix:null}` and
                // `{uuid:"b", audio_mix:"m"}` collide even though neither
                // states "m" twice.
                let effective_mix = bus
                    .audio_mix
                    .as_ref()
                    .map_or(bus.uuid.as_str(), crate::bus::AudioMixId::as_str);
                if !seen_audio.insert(effective_mix.to_string()) {
                    errors.push(format!(
                        "bus '{}' shares audio mix identity '{effective_mix}' with another bus",
                        bus.name
                    ));
                }
                if !bus.auto_duration.is_finite() || bus.auto_duration <= 0.0 {
                    errors.push(format!(
                        "bus '{}' has a non-positive auto duration ({})",
                        bus.name, bus.auto_duration
                    ));
                }
            }
        }
        errors
    }

    /// Load stage prefs from a JSON file.
    ///
    /// # Errors
    ///
    /// Returns an error if `path` cannot be read (missing file, permissions) or
    /// if its contents are not valid JSON for a [`StagePrefs`]. Validation
    /// problems in an otherwise-parseable file are logged as warnings only.
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = std::fs::read_to_string(path.as_ref())
            .with_context(|| format!("Failed to read stage prefs: {}", path.as_ref().display()))?;
        let prefs: StagePrefs = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse stage prefs: {}", path.as_ref().display()))?;
        let warnings = prefs.validate();
        for w in &warnings {
            log::warn!("Stage prefs {}: {}", path.as_ref().display(), w);
        }
        Ok(prefs)
    }

    /// Save stage prefs to a JSON file.
    ///
    /// # Errors
    ///
    /// Returns an error if the prefs cannot be serialized to JSON, or if the
    /// atomic write fails (temp file write or rename).
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        atomic_write(path.as_ref(), &self.to_json()?)
    }

    /// The text [`StagePrefs::save`] writes, its problems logged.
    ///
    /// # Errors
    ///
    /// Returns an error if the stage cannot be serialized to JSON.
    pub fn to_json(&self) -> Result<String> {
        let errors = self.validate();
        for e in &errors {
            log::error!("Stage prefs save: {e}");
        }
        serde_json::to_string_pretty(self).context("Failed to serialize stage prefs")
    }
}

/// Workspace directory manager — handles `.varda/` paths and directory creation.
pub struct Workspace {
    /// Root of the workspace (current working directory)
    root: PathBuf,
}

impl Workspace {
    /// Create a workspace rooted at the given directory.
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Create a workspace rooted at the current working directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the current working directory cannot be determined
    /// (e.g. it was deleted, or the process lacks permission to read it).
    pub fn from_cwd() -> Result<Self> {
        let cwd = std::env::current_dir().context("Failed to get current directory")?;
        Ok(Self::new(cwd))
    }

    /// Path to the `.varda/` directory.
    pub fn varda_dir(&self) -> PathBuf {
        self.root.join(".varda")
    }

    /// Path to `scene.json`.
    pub fn scene_path(&self) -> PathBuf {
        self.varda_dir().join("scene.json")
    }

    /// Path to `midi.json`.
    pub fn midi_path(&self) -> PathBuf {
        self.varda_dir().join("midi.json")
    }

    /// Path to `stage.json`.
    pub fn stage_path(&self) -> PathBuf {
        self.varda_dir().join("stage.json")
    }

    /// Path to the `luts/` directory inside `.varda/`.
    pub fn luts_dir(&self) -> PathBuf {
        self.varda_dir().join("luts")
    }

    /// Path to `keymap.json`.
    pub fn keymap_path(&self) -> PathBuf {
        self.varda_dir().join("keymap.json")
    }

    /// Path to `osc.json`.
    pub fn osc_path(&self) -> PathBuf {
        self.varda_dir().join("osc.json")
    }

    /// Check if a keymap config file exists.
    pub fn has_keymap(&self) -> bool {
        self.keymap_path().is_file()
    }

    /// Check if an OSC config file exists.
    pub fn has_osc(&self) -> bool {
        self.osc_path().is_file()
    }

    /// Path to `controller-profiles/` directory for MIDI controller profiles.
    pub fn controller_profiles_dir(&self) -> PathBuf {
        self.varda_dir().join("controller-profiles")
    }

    /// Path to `presets/` directory.
    pub fn presets_dir(&self) -> PathBuf {
        self.varda_dir().join("presets")
    }

    /// Path to `shaders/` directory for workspace-local ISF shaders.
    pub fn shaders_dir(&self) -> PathBuf {
        self.varda_dir().join("shaders")
    }

    /// Path to `presets/decks/` directory.
    pub fn deck_presets_dir(&self) -> PathBuf {
        self.presets_dir().join("decks")
    }

    /// Path to `presets/channels/` directory.
    pub fn channel_presets_dir(&self) -> PathBuf {
        self.presets_dir().join("channels")
    }

    /// Ensure preset directories exist.
    ///
    /// # Errors
    ///
    /// Returns an error if `.varda/` or either of the `presets/decks/` and
    /// `presets/channels/` directories cannot be created.
    pub fn ensure_preset_dirs(&self) -> Result<()> {
        self.ensure_dir()?;
        let dirs = [self.deck_presets_dir(), self.channel_presets_dir()];
        for dir in &dirs {
            if !dir.exists() {
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("Failed to create preset dir: {}", dir.display()))?;
            }
        }
        Ok(())
    }

    /// Whether `.varda/` exists in this workspace.
    pub fn exists(&self) -> bool {
        self.varda_dir().is_dir()
    }

    /// Ensure the `.varda/` directory exists.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory does not exist and cannot be created
    /// (permissions, or a non-directory file already at that path).
    pub fn ensure_dir(&self) -> Result<()> {
        let dir = self.varda_dir();
        if !dir.exists() {
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("Failed to create .varda directory: {}", dir.display()))?;
            log::info!("Created workspace directory: {}", dir.display());
        }
        Ok(())
    }

    /// Check if a scene file exists.
    pub fn has_scene(&self) -> bool {
        self.scene_path().is_file()
    }

    /// Check if a MIDI config file exists.
    pub fn has_midi(&self) -> bool {
        self.midi_path().is_file()
    }

    /// Check if stage prefs file exists.
    pub fn has_stage(&self) -> bool {
        self.stage_path().is_file()
    }

    /// Root directory path.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

// ── Snapshot: Live State → Config ───────────────────────────────────

use crate::mixer::Mixer;
use crate::renderer::context::{OutputTarget, RecordingCodec, UnifiedOutput};
use crate::scene::{
    AutoTransitionConfig, ChannelConfig, DeckConfig, EffectConfig, OutputConfig,
    OutputTargetConfig, SceneConfig, SourceConfig, SurfaceAssignmentConfig, TriggerConfig,
};

// ── DurationSpec ↔ DurationSpecConfig helpers ───────────────────────

fn duration_spec_to_config(
    spec: &crate::channel::DurationSpec,
) -> crate::scene::DurationSpecConfig {
    use crate::channel::DurationSpec;
    use crate::scene::DurationSpecConfig;
    match spec {
        DurationSpec::Beats(v) => DurationSpecConfig::Beats(*v),
        DurationSpec::Seconds(v) => DurationSpecConfig::Seconds(*v),
        DurationSpec::Minutes(v) => DurationSpecConfig::Minutes(*v),
        DurationSpec::Hours(v) => DurationSpecConfig::Hours(*v),
    }
}

fn duration_config_to_spec(
    config: &crate::scene::DurationSpecConfig,
) -> crate::channel::DurationSpec {
    use crate::channel::DurationSpec;
    use crate::scene::DurationSpecConfig;
    match config {
        DurationSpecConfig::Beats(v) => DurationSpec::Beats(*v),
        DurationSpecConfig::Seconds(v) => DurationSpec::Seconds(*v),
        DurationSpecConfig::Minutes(v) => DurationSpec::Minutes(*v),
        DurationSpecConfig::Hours(v) => DurationSpec::Hours(*v),
    }
}

/// Convert persisted sequence steps into runtime steps, resolving fade steps'
/// channel references against the channels as restored.
///
/// A fade whose channels no longer resolve degrades to a `Wait` of the same
/// duration rather than being dropped. `GoTo` steps address other steps by
/// position, so removing a step would silently retarget every jump past it —
/// keeping the slot preserves both the jump targets and the sequence's timing.
pub fn restore_sequence_steps(
    steps: &[crate::scene::TransitionStepConfig],
    channel_uuids: &[String],
    warnings: &mut Vec<String>,
) -> Vec<crate::mixer::TransitionStep> {
    use crate::mixer::{StepKind, TransitionStep};
    use crate::scene::TransitionStepConfig;

    steps
        .iter()
        .map(|step| {
            let kind = match step {
                TransitionStepConfig::Fade {
                    from_ch,
                    to_ch,
                    duration,
                    easing,
                    transition_shader,
                    target_amount,
                } => {
                    let resolved = from_ch
                        .resolve(channel_uuids)
                        .zip(to_ch.resolve(channel_uuids))
                        .filter(|(from, to)| {
                            channel_uuids.contains(from) && channel_uuids.contains(to)
                        });
                    if let Some((from_ch, to_ch)) = resolved {
                        StepKind::Fade {
                            from_ch,
                            to_ch,
                            duration: duration_config_to_spec(duration),
                            easing: (*easing).into(),
                            transition_shader: transition_shader.clone(),
                            target_amount: *target_amount,
                        }
                    } else {
                        let msg = "Fade step references a channel that no longer exists; \
                                   kept as a wait so later GoTo targets stay valid"
                            .to_string();
                        log::warn!("{msg}");
                        warnings.push(msg);
                        StepKind::Wait {
                            duration: duration_config_to_spec(duration),
                        }
                    }
                }
                TransitionStepConfig::Wait { duration } => StepKind::Wait {
                    duration: duration_config_to_spec(duration),
                },
                TransitionStepConfig::GoTo { step_index } => StepKind::GoTo {
                    step_index: *step_index,
                },
            };
            TransitionStep { kind }
        })
        .collect()
}

/// Serialize a deck's depth-sensor preprocessor binding, if it has one.
///
/// Stores the device *name* rather than its id, matching how cameras and
/// depth-sensor decks restore — ids shift when devices are replugged.
fn depth_prepro_config(deck: &Deck) -> Option<crate::scene::DepthPreproConfig> {
    let state = deck.depth_prepro.as_ref()?;
    let p = &state.params;
    Some(crate::scene::DepthPreproConfig {
        sensor_name: state.sensor_name.clone(),
        near_mm: p.near_mm,
        far_mm: p.far_mm,
        smoothing: p.smoothing,
        hole_fill: p.hole_fill,
        mask_feather: p.mask_feather,
        motion_gain: p.motion_gain,
        mirror: p.mirror,
    })
}

/// Build a `SceneConfig` snapshot from live app state (show-specific: channels, effects, modulation).
///
/// `transport` contributes only its authored settings (frame rate, loop range);
/// position and run state are deliberately not persisted, so a scene opens
/// where it was authored to start rather than wherever it was stopped.
pub fn snapshot_scene(
    mixer: &Mixer,
    transport: Option<&crate::scene::TransportConfig>,
    render_width: u32,
    render_height: u32,
) -> SceneConfig {
    let channels = mixer
        .channels()
        .iter()
        .map(|ch| {
            let decks = ch
                .decks
                .iter()
                .filter_map(|slot| {
                    let source = match slot.deck.source_type() {
                        "shader" => {
                            let path = slot.deck.source_path().unwrap_or_default().to_string();
                            SourceConfig::Shader {
                                path,
                                params: slot.deck.generator_params.values.clone(),
                                depth_prepro: depth_prepro_config(&slot.deck),
                            }
                        }
                        "video" => {
                            let pb = slot.deck.playback_snapshot();
                            SourceConfig::Video {
                                path: slot.deck.source_path().unwrap_or_default().to_string(),
                                loop_mode: pb.as_ref().map(|p| p.loop_mode).unwrap_or_default(),
                                speed: pb.as_ref().map_or(1.0, |p| p.speed),
                                in_point: pb.as_ref().map_or(0.0, |p| p.in_point),
                                out_point: pb.as_ref().map_or(0.0, |p| p.out_point),
                                scaling_mode: slot.deck.scaling_mode().unwrap_or_default(),
                                transport_sync: slot
                                    .deck
                                    .video_transport_sync()
                                    .unwrap_or_default(),
                                auto: slot.deck.auto_playback,
                            }
                        }
                        "image" => SourceConfig::Image {
                            path: slot.deck.source_path().unwrap_or_default().to_string(),
                            scaling_mode: slot.deck.scaling_mode().unwrap_or_default(),
                        },
                        "solid_color" => {
                            let color = slot.deck.solid_color().unwrap_or([0.0, 0.0, 0.0, 1.0]);
                            SourceConfig::SolidColor { color }
                        }
                        "camera" => {
                            // Store the camera display name (strip the 📹 prefix we add)
                            let name = slot
                                .deck
                                .source_name()
                                .trim_start_matches("📹 ")
                                .to_string();
                            SourceConfig::Camera {
                                name,
                                mode: slot.deck.camera_mode,
                                device: slot.deck.camera_device.clone(),
                                colour: slot.deck.camera_colour,
                                profile: slot.deck.camera_profile.clone(),
                                lowest_latency: slot.deck.camera_lowest_latency,
                            }
                        }
                        "ndi" => {
                            // Store the NDI source name (strip the 📡 prefix we add)
                            let name = slot
                                .deck
                                .source_name()
                                .trim_start_matches("📡 ")
                                .to_string();
                            SourceConfig::Ndi { name }
                        }
                        "syphon" => {
                            // Store the Syphon server name (strip the 🔗 prefix we add)
                            let name = slot
                                .deck
                                .source_name()
                                .trim_start_matches("🔗 ")
                                .to_string();
                            SourceConfig::Syphon { name }
                        }
                        "srt" => {
                            let url = slot
                                .deck
                                .source_name()
                                .trim_start_matches("📺 ")
                                .to_string();
                            let mode = "caller".to_string();
                            SourceConfig::Srt { url, mode }
                        }
                        "hls" => {
                            let url = slot
                                .deck
                                .source_name()
                                .trim_start_matches("📡 ")
                                .to_string();
                            SourceConfig::Hls { url }
                        }
                        "dash" => {
                            let url = slot
                                .deck
                                .source_name()
                                .trim_start_matches("📡 ")
                                .to_string();
                            SourceConfig::Dash { url }
                        }
                        "rtmp" => {
                            let url = slot
                                .deck
                                .source_name()
                                .trim_start_matches("📺 ")
                                .to_string();
                            SourceConfig::Rtmp {
                                url,
                                mode: "pull".to_string(),
                            }
                        }
                        "html" => {
                            let url = slot
                                .deck
                                .source_name()
                                .trim_start_matches("🌐 ")
                                .to_string();
                            SourceConfig::Html { url }
                        }
                        "depth_sensor" => {
                            // Store the sensor's name, not the layer's label
                            let name = sensor_of(slot.deck.source_name()).to_string();
                            let p = &slot.deck.point_cloud_params;
                            let params = Some(crate::scene::DepthParamsConfig {
                                orbit_yaw: p.orbit_yaw,
                                orbit_pitch: p.orbit_pitch,
                                zoom: p.zoom,
                                point_size: p.point_size,
                                color_mode: p.color_mode.as_f32() as u8,
                                depth_min_mm: p.depth_min_mm,
                                depth_max_mm: p.depth_max_mm,
                                solid_color: p.solid_color,
                                seed: p.seed,
                                drift: p.drift,
                                disruption: p.disruption,
                            });
                            SourceConfig::DepthSensor { name, params }
                        }
                        "screen_capture" => {
                            // The live binding is held on the deck, so the target
                            // and settings serialize without reaching into the
                            // capture manager. A capture deck can only exist with
                            // this state set, so its absence is unrecoverable.
                            let state = slot.deck.screen_capture.as_ref()?;
                            let crop = state.config.crop;
                            SourceConfig::ScreenCapture {
                                target: crate::scene::CaptureTargetConfig::from(&state.identity),
                                rate: state.config.rate,
                                crop: if crop.is_full_frame() {
                                    None
                                } else {
                                    Some(crop.into())
                                },
                                show_cursor: state.config.show_cursor,
                                exclude_varda: Some(state.config.exclude_varda),
                                scaling_mode: slot.deck.scaling_mode().unwrap_or_default(),
                            }
                        }
                        "tap" => {
                            // As with screen capture, the binding lives on the
                            // deck, so no mixer lookup is needed here.
                            let state = slot.deck.tap.as_ref()?;
                            SourceConfig::Tap {
                                source: crate::scene::TapSourceConfig::from(&state.source),
                                scaling_mode: slot.deck.scaling_mode().unwrap_or_default(),
                            }
                        }
                        "playlist" => {
                            let list = slot.deck.playlist()?;
                            SourceConfig::Playlist {
                                items: list.items().to_vec(),
                                settings: list.settings(),
                                scaling_mode: list.scaling_mode(),
                            }
                        }
                        "timer" => {
                            let timer = slot.deck.timer()?;
                            SourceConfig::Timer {
                                settings: timer.settings().clone(),
                                elapsed: timer.elapsed(),
                            }
                        }
                        _ => return None,
                    };

                    let effects = slot.deck.effects.iter().map(EffectConfig::of).collect();

                    // Snapshot auto-transition config
                    let auto_transition = slot
                        .auto_transition
                        .as_ref()
                        .filter(|at| at.enabled)
                        .map(|at| {
                            use crate::channel::TransitionTrigger;
                            AutoTransitionConfig {
                                enabled: at.enabled,
                                trigger: match at.trigger {
                                    TransitionTrigger::Timer => TriggerConfig::Timer,
                                    TransitionTrigger::ClipEnd => TriggerConfig::ClipEnd,
                                },
                                play_duration: duration_spec_to_config(&at.play_duration),
                                transition_duration: duration_spec_to_config(
                                    &at.transition_duration,
                                ),
                                transition_shader: at.transition_shader_name.clone(),
                            }
                        });

                    Some(DeckConfig {
                        uuid: slot.deck.uuid().to_string(),
                        name: slot.deck.source_name().to_string(),
                        source,
                        effects,
                        opacity: slot.opacity,
                        transparent: slot.deck.transparent(),
                        blend_mode: slot.blend_mode.into(),
                        mute: slot.mute,
                        solo: slot.solo,
                        z_index: slot.z_index,
                        render_fps: slot.render_fps,
                        render_scale: slot.deck.render_scale(),
                        auto_transition,
                        modulation: vec![],
                    })
                })
                .collect();

            let effects = ch.effects.iter().map(EffectConfig::of).collect();

            ChannelConfig {
                uuid: ch.uuid().to_string(),
                name: ch.name.clone(),
                opacity: ch.opacity,
                blend_mode: ch.blend_mode.into(),
                overlay: ch.overlay,
                decks,
                effects,
                // A scene serializes the modulation engine whole, so recipes are
                // only filled when a channel travels alone. See
                // /spec/clipboard.md.
                modulation: Vec::new(),
            }
        })
        .collect();

    let master_effects = mixer
        .master_effects()
        .iter()
        .map(EffectConfig::of)
        .collect();

    let active_transition = mixer.active_transition().as_ref().map(|t| t.name.clone());

    // Snapshot transition sequences
    let transition_sequences = mixer
        .transition_sequences()
        .iter()
        .map(|seq| {
            use crate::scene::{TransitionSequenceConfig, TransitionStepConfig};
            TransitionSequenceConfig {
                uuid: seq.uuid.clone(),
                name: seq.name.clone(),
                enabled: seq.enabled,
                steps: seq
                    .steps
                    .iter()
                    .map(|step| match &step.kind {
                        crate::mixer::StepKind::Fade {
                            from_ch,
                            to_ch,
                            duration,
                            easing,
                            transition_shader,
                            target_amount,
                        } => TransitionStepConfig::Fade {
                            from_ch: from_ch.clone().into(),
                            to_ch: to_ch.clone().into(),
                            duration: duration_spec_to_config(duration),
                            easing: (*easing).into(),
                            transition_shader: transition_shader.clone(),
                            target_amount: *target_amount,
                        },
                        crate::mixer::StepKind::Wait { duration } => TransitionStepConfig::Wait {
                            duration: duration_spec_to_config(duration),
                        },
                        crate::mixer::StepKind::GoTo { step_index } => TransitionStepConfig::GoTo {
                            step_index: *step_index,
                        },
                    })
                    .collect(),
            }
        })
        .collect();

    SceneConfig {
        version: SceneConfig::CURRENT_VERSION,
        channels,
        crossfader: mixer.crossfader(),
        active_transition,
        master_effects,
        modulation: mixer.modulation().clone(),
        macros: mixer.macros().clone(),
        transition_sequences,
        render_width: Some(render_width),
        render_height: Some(render_height),
        render_resolution_auto: false,
        frames_in_flight: None,
        free_control_screen: false,
        tonemap_mode: mixer.tonemap_mode(),
        active_lut: mixer
            .active_lut_filename()
            .map(std::string::ToString::to_string),
        arrangement: mixer.arrangement().cloned(),
        transport: transport.cloned().unwrap_or_default(),
        main: snapshot_main(mixer),
    }
}

/// Main as a scene saves it; `None` for the layered mix with nothing set,
/// so a scene that never used Main is written as before.
fn snapshot_main(mixer: &Mixer) -> Option<crate::scene::MainConfig> {
    let main = mixer.main();
    let untouched = mixer.mix_mode() == crate::mixer::MixMode::Layered
        && main.program == crate::mixer::MainSource::Black
        && main.overlays_on().is_empty()
        && (main.fade_secs - crate::mixer::DEFAULT_FADE_SECS).abs() < f32::EPSILON
        && main.transition == crate::mixer::MainTransition::Fade;
    (!untouched).then(|| crate::scene::MainConfig {
        mode: mixer.mix_mode(),
        program: main.program.clone(),
        overlays_on: main.overlays_on(),
        fade_secs: main.fade_secs,
        transition: main.transition,
    })
}

/// Put saved Main back: the mode, the program, the overlays that were on.
pub fn restore_main(mixer: &mut Mixer, config: Option<&crate::scene::MainConfig>) {
    let config = config.cloned().unwrap_or(crate::scene::MainConfig {
        mode: crate::mixer::MixMode::Layered,
        program: crate::mixer::MainSource::Black,
        overlays_on: Vec::new(),
        fade_secs: crate::mixer::DEFAULT_FADE_SECS,
        transition: crate::mixer::MainTransition::Fade,
    });
    mixer.set_mix_mode(config.mode);
    let main = mixer.main_mut();
    main.fade_secs = config.fade_secs;
    main.transition = config.transition;
    main.take(config.program, false);
    main.restore_overlays(&config.overlays_on);
}

/// Convert a live `OutputTarget` to a serializable `OutputTargetConfig`.
fn target_to_config(target: &OutputTarget) -> OutputTargetConfig {
    match target {
        // The stream and the recording save their own settings and are never
        // among the outputs (`snapshot_stage` leaves them out); they are named
        // here only to keep the match total.
        OutputTarget::Windowed | OutputTarget::Stream | OutputTarget::Record => {
            OutputTargetConfig::Windowed
        }
        OutputTarget::Display { name, .. } => OutputTargetConfig::Display { name: name.clone() },
        OutputTarget::Recording {
            path,
            codec,
            audio_device,
        } => OutputTargetConfig::Recording {
            path: path.clone(),
            codec: codec.to_string(),
            audio_device: audio_device.clone(),
        },
        OutputTarget::SrtStream {
            url,
            codec,
            audio_device,
        } => OutputTargetConfig::SrtStream {
            url: url.clone(),
            codec: codec.to_string(),
            audio_device: audio_device.clone(),
        },
        OutputTarget::HlsStream {
            name,
            codec,
            low_latency,
            audio_device,
        } => OutputTargetConfig::HlsStream {
            name: name.clone(),
            codec: codec.to_string(),
            low_latency: *low_latency,
            audio_device: audio_device.clone(),
        },
        OutputTarget::DashStream {
            name,
            codec,
            audio_device,
        } => OutputTargetConfig::DashStream {
            name: name.clone(),
            codec: codec.to_string(),
            audio_device: audio_device.clone(),
        },
        OutputTarget::RtmpStream {
            url,
            codec,
            codec_contract,
            audio_device,
        } => OutputTargetConfig::RtmpStream {
            url: url.clone(),
            codec: codec.to_string(),
            codec_contract: *codec_contract,
            audio_device: audio_device.clone(),
        },
        OutputTarget::NdiSend { sender_name } => OutputTargetConfig::NdiSend {
            sender_name: sender_name.clone(),
        },
        OutputTarget::SyphonServer { server_name } => OutputTargetConfig::SyphonServer {
            server_name: server_name.clone(),
        },
        OutputTarget::DeckLink {
            device,
            mode,
            full_range,
        } => OutputTargetConfig::DeckLink {
            device: device.clone(),
            mode: mode.clone(),
            full_range: *full_range,
        },
    }
}

/// Convert a serializable `OutputTargetConfig` back to a live `OutputTarget`.
/// Public variant for use from outputs.rs.
pub fn config_to_target_pub(config: &OutputTargetConfig) -> OutputTarget {
    config_to_target(config)
}

fn config_to_target(config: &OutputTargetConfig) -> OutputTarget {
    match config {
        OutputTargetConfig::Windowed => OutputTarget::Windowed,
        OutputTargetConfig::Display { name } => OutputTarget::Display {
            name: name.clone(),
            monitor_index: 0, // Will be matched at runtime
        },
        OutputTargetConfig::Recording {
            path,
            codec,
            audio_device,
        } => OutputTarget::Recording {
            path: path.clone(),
            codec: match codec.as_str() {
                "prores" | "ProRes" | "ProRes 422" => RecordingCodec::ProRes,
                "prores_4444" | "ProRes4444" | "ProRes 4444" => RecordingCodec::ProRes4444,
                "h265" | "H265" | "H.265 (HEVC)" => RecordingCodec::H265,
                "av1" | "AV1" => RecordingCodec::AV1,
                "hap" | "Hap" | "HAP" => RecordingCodec::Hap,
                "hap_alpha" | "HapAlpha" | "HAP Alpha" => RecordingCodec::HapAlpha,
                "hapq" | "HapQ" | "HAP Q" => RecordingCodec::HapQ,
                _ => RecordingCodec::H264,
            },
            audio_device: audio_device.clone(),
        },
        OutputTargetConfig::SrtStream {
            url,
            codec,
            audio_device,
        } => OutputTarget::SrtStream {
            url: url.clone(),
            codec: match codec.as_str() {
                "H.265 (HEVC)" | "H265" | "h265" => crate::renderer::context::SrtCodec::H265,
                _ => crate::renderer::context::SrtCodec::H264,
            },
            audio_device: audio_device.clone(),
        },
        OutputTargetConfig::HlsStream {
            name,
            codec,
            low_latency,
            audio_device,
        } => OutputTarget::HlsStream {
            name: name.clone(),
            codec: match codec.as_str() {
                "H.265 (HEVC)" | "H265" | "h265" => crate::renderer::context::StreamingCodec::H265,
                "AV1" | "av1" => crate::renderer::context::StreamingCodec::AV1,
                _ => crate::renderer::context::StreamingCodec::H264,
            },
            low_latency: *low_latency,
            audio_device: audio_device.clone(),
        },
        OutputTargetConfig::DashStream {
            name,
            codec,
            audio_device,
        } => OutputTarget::DashStream {
            name: name.clone(),
            codec: match codec.as_str() {
                "H.265 (HEVC)" | "H265" | "h265" => crate::renderer::context::StreamingCodec::H265,
                "AV1" | "av1" => crate::renderer::context::StreamingCodec::AV1,
                _ => crate::renderer::context::StreamingCodec::H264,
            },
            audio_device: audio_device.clone(),
        },
        OutputTargetConfig::RtmpStream {
            url,
            codec,
            codec_contract,
            audio_device,
        } => OutputTarget::RtmpStream {
            url: url.clone(),
            codec: match codec.as_str() {
                "H.265 (HEVC)" | "H265" | "h265" => crate::renderer::context::StreamingCodec::H265,
                "AV1" | "av1" => crate::renderer::context::StreamingCodec::AV1,
                _ => crate::renderer::context::StreamingCodec::H264,
            },
            codec_contract: *codec_contract,
            audio_device: audio_device.clone(),
        },
        OutputTargetConfig::NdiSend { sender_name } => OutputTarget::NdiSend {
            sender_name: sender_name.clone(),
        },
        OutputTargetConfig::SyphonServer { server_name } => OutputTarget::SyphonServer {
            server_name: server_name.clone(),
        },
        OutputTargetConfig::DeckLink {
            device,
            mode,
            full_range,
        } => OutputTarget::DeckLink {
            device: device.clone(),
            mode: mode.clone(),
            full_range: *full_range,
        },
    }
}

/// Build a `StagePrefs` snapshot from live app state (venue-specific: surfaces, outputs, editor prefs).
// Aggregates many independent live-state sources into one snapshot; no shared invariant to bundle.
// The bools mirror independent persisted `StagePrefs` toggles one-for-one.
#[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
pub fn snapshot_stage(
    surface_manager: &crate::surface::SurfaceManager,
    outputs_list: &[UnifiedOutput],
    buses: &crate::bus::BusBank,
    grid_size: f32,
    snap: bool,
    library_panel_open: bool,
    right_panel_open: bool,
    stage_editor_open: bool,
    dome_preview_open: bool,
    dome_mode_active: bool,
    dome_preset: crate::renderer::slicer::DomePreset,
    dome_geometry: crate::renderer::slicer::DomeGeometry,
    domemaster_resolution: crate::renderer::dome::DomemasterResolution,
) -> StagePrefs {
    let outputs = outputs_list
        .iter()
        .filter(|unified| {
            !matches!(
                unified,
                UnifiedOutput::Headless(h)
                    if matches!(h.target, OutputTarget::Stream | OutputTarget::Record)
            )
        })
        .map(output_config)
        .collect();

    StagePrefs {
        buses: Some(buses.to_configs()),
        grid_size,
        snap,
        library_panel_open,
        right_panel_open,
        stage_editor_open,
        dome_preview_open,
        dome_mode_active,
        dome_preset,
        dome_geometry,
        domemaster_resolution,
        surfaces: surface_manager.clone(),
        outputs,
        // Filled by the caller: the timecode patch is not derived from the
        // stage geometry this function is given.
        timecode: crate::timecode::TimecodeConfig::default(),
        // Filled by the caller, like the timecode.
        rundown: None,
        audio_mix: None,
        stream: None,
        record: None,
        screen_layout: None,
        collapsed_channels: Vec::new(),
    }
}

/// An output as the stage saves it — and as it is made again: on the next
/// start, or in place when its window has stopped showing (see
/// `VardaApp::reopen_unshown_outputs`).
pub fn output_config(unified: &UnifiedOutput) -> OutputConfig {
    let (name, target, surface_assignments, window_position, window_size) = match unified {
        UnifiedOutput::Window(w) => {
            // Capture window position and size for restoration, as
            // the main thread last saw them: a save can run on the
            // engine thread, which may not ask the window itself.
            let pos = w.position.map(|p| [p.x, p.y]);
            let sz = {
                let s = w.size;
                if s.width > 0 && s.height > 0 {
                    Some([s.width, s.height])
                } else {
                    None
                }
            };
            (
                w.name.clone(),
                target_to_config(&w.configured_target()),
                w.surface_assignments
                    .iter()
                    .map(|a| SurfaceAssignmentConfig {
                        surface_uuid: a.surface_uuid.clone(),
                        legacy_warp_mode: None,
                        enabled: a.enabled,
                    })
                    .collect(),
                pos,
                sz,
            )
        }
        UnifiedOutput::Headless(h) => (
            h.name.clone(),
            target_to_config(&h.target),
            h.surface_assignments
                .iter()
                .map(|a| SurfaceAssignmentConfig {
                    surface_uuid: a.surface_uuid.clone(),
                    legacy_warp_mode: None,
                    enabled: a.enabled,
                })
                .collect(),
            None,
            None,
        ),
    };
    let edge_blend_mode = unified.edge_blend_mode();
    let edge_blend = unified.edge_blend();
    OutputConfig {
        uuid: unified.uuid().to_string(),
        name,
        target,
        target_display: None,
        surface_assignments,
        window_position,
        window_size,
        edge_blend_mode,
        edge_blend,
        rotation: unified.rotation(),
        presentation: unified.presentation_request(),
        source: unified
            .source()
            .cloned()
            .unwrap_or(crate::renderer::context::OutputSource::Master),
        audio_bus: match unified {
            UnifiedOutput::Headless(h) => h.audio_bus.clone(),
            UnifiedOutput::Window(_) => None,
        },
        led: unified.led().cloned(),
        running: matches!(
            unified,
            UnifiedOutput::Headless(h)
                if h.keep_running
                    && matches!(h.target, crate::renderer::context::OutputTarget::DeckLink { .. })
        ),
    }
}

// ── Restore: Config → Live State ────────────────────────────────────

use crate::deck::{Deck, Effect};
use crate::isf::ISFShader;
use crate::renderer::GpuContext;

/// A Syphon deck whose source could not be resolved at restore time, deferred
/// for late binding. Varda is the *client* of externally-owned Syphon servers;
/// on restart the producer may not be publishing yet, so the named server is not in
/// `SyphonServerDirectory`. Rather than fail the restore (the old behaviour:
/// "restoration not yet implemented" → `black_hole` placeholder), we record the
/// intent here and let `VardaApp::reconcile_syphon` auto-attach the real deck
/// the moment the server appears. Startup order becomes irrelevant.
#[derive(Debug, Clone)]
pub struct PendingSyphonDeck {
    /// UUID of the channel this deck belongs to. Binding happens seconds to
    /// minutes after restore, by which point a positional index may point at a
    /// different channel.
    pub channel_uuid: String,
    /// Full persisted deck config (carries the `Syphon { name }` source plus
    /// opacity / blend / mute / solo / z-index to re-apply on bind).
    pub config: crate::scene::DeckConfig,
}

/// Restore result — contains reconstructed mixer.
/// Surfaces and outputs are loaded separately from stage.json.
pub struct RestoreResult {
    pub mixer: Mixer,
    pub warnings: Vec<String>,
    /// Syphon decks deferred for late binding (see `PendingSyphonDeck`).
    pub pending_syphon: Vec<PendingSyphonDeck>,
}

/// Reconstruct live state from a `SceneConfig`.
///
/// # Errors
///
/// Returns an error if the mixer or any deck/effect in `config` cannot be
/// constructed on the GPU — for example a shader or media file that fails to
/// load or compile. Individually recoverable problems are collected into
/// [`RestoreResult::warnings`] instead.
// Writes back into many independent live-state targets; no shared invariant to bundle.
#[allow(clippy::too_many_arguments)]
pub fn restore_scene(
    config: &SceneConfig,
    context: &GpuContext,
    registry: &crate::registry::ShaderRegistry,
    camera_manager: &mut crate::camera::CameraManager,
    screen_capture_manager: &mut crate::screen_capture::ScreenCaptureManager,
    depth_manager: &mut crate::depth::DepthSensorManager,
    ndi_manager: &mut crate::ndi::NdiManager,
    stream_manager: &mut crate::stream::StreamManager,
    html_manager: &mut crate::html::HtmlManager,
    render_width: u32,
    render_height: u32,
) -> Result<RestoreResult> {
    let mut warnings = Vec::new();
    // Only pushed to under #[cfg(target_os = "macos")]; on other platforms it
    // stays empty, so `mut` would be flagged as unused there.
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut pending_syphon: Vec<PendingSyphonDeck> = Vec::new();
    let mut mixer = Mixer::new(context, render_width, render_height)?;

    // Clear default channels — we'll create from config
    mixer.channels_mut().clear();

    for ch_config in &config.channels {
        let mut channel = crate::channel::Channel::new(
            ch_config.name.clone(),
            context,
            render_width,
            render_height,
        )?;
        if !ch_config.uuid.is_empty() {
            channel.set_uuid(ch_config.uuid.clone());
        }
        channel.opacity = ch_config.opacity;
        channel.blend_mode = ch_config.blend_mode.into();
        channel.overlay = ch_config.overlay;

        for deck_config in &ch_config.decks {
            // Externally-owned Syphon decks are resolved at runtime, not at
            // restore time — the producer may not be publishing yet. Defer to a
            // pending binding the render thread auto-attaches
            // once the named server appears (see VardaApp::reconcile_syphon).
            // This replaces the old hard-fail stub that dropped the channel to a
            // black_hole placeholder and spammed "restoration not yet implemented".
            if let SourceConfig::Syphon { name } = &deck_config.source {
                #[cfg(target_os = "macos")]
                {
                    log::info!(
                        "Syphon deck '{}' on channel {} deferred to late-bind \
                         (auto-attaches when the server appears)",
                        name,
                        channel.name
                    );
                    pending_syphon.push(PendingSyphonDeck {
                        channel_uuid: channel.uuid().to_string(),
                        config: deck_config.clone(),
                    });
                }
                #[cfg(not(target_os = "macos"))]
                {
                    log::debug!("Skipping Syphon deck '{name}' on non-macOS restore");
                }
                continue;
            }
            match restore_deck(
                deck_config,
                context,
                registry,
                camera_manager,
                screen_capture_manager,
                depth_manager,
                ndi_manager,
                stream_manager,
                html_manager,
                render_width,
                render_height,
            ) {
                Ok(deck) => {
                    let slot = slot_from_config(deck, deck_config, context, registry);
                    channel.add_deck_slot(slot);
                }
                Err(e) => {
                    let msg = format!("Failed to restore deck '{}': {}", deck_config.name, e);
                    log::warn!("{msg}");
                    warnings.push(msg);
                }
            }
        }

        // Restore channel effects
        for eff_config in &ch_config.effects {
            match restore_effect(eff_config, context, context.compositing_format) {
                Ok(eff) => channel.add_effect(eff),
                Err(e) => {
                    let msg = format!(
                        "Failed to restore channel effect '{}': {}",
                        eff_config.path, e
                    );
                    log::warn!("{msg}");
                    warnings.push(msg);
                }
            }
        }

        mixer.channels_mut().push(channel);
    }

    // A layer showing another input is named after that input's channel,
    // which may come later in the scene: named now, every channel known,
    // not "Channel 485bdbc5".
    let labels: Vec<(String, String)> = mixer
        .channels()
        .iter()
        .map(|ch| (ch.uuid().to_string(), ch.name.clone()))
        .collect();
    for slot in mixer.channels_mut().iter_mut().flat_map(|ch| &mut ch.decks) {
        if let Some(tap) = &slot.deck.tap {
            let name = format!("🔁 {}", tap.source.label(&labels));
            slot.deck.set_source_name(name);
        }
    }

    // Update next_channel_index so new channels don't get duplicate names.
    // Parse existing channel names to find the highest "Ch N" index.
    let max_idx = mixer
        .channels()
        .iter()
        .filter_map(|ch| {
            ch.name
                .strip_prefix("Ch ")
                .and_then(|s| s.parse::<usize>().ok())
        })
        .max()
        .map_or(mixer.channel_count(), |n| n + 1);
    mixer.set_next_channel_index(max_idx);

    // Restore master effects
    for eff_config in &config.master_effects {
        match restore_effect(eff_config, context, context.compositing_format) {
            Ok(eff) => mixer.master_effects_mut().push(eff),
            Err(e) => {
                let msg = format!(
                    "Failed to restore master effect '{}': {}",
                    eff_config.path, e
                );
                log::warn!("{msg}");
                warnings.push(msg);
            }
        }
    }

    // Restore crossfader
    mixer.set_crossfader(config.crossfader);

    // Restore Main: the mode, and what was on air.
    restore_main(&mut mixer, config.main.as_ref());

    // Restore modulation engine
    mixer.set_modulation(config.modulation.clone());

    // Restore macro controls
    mixer.set_macros(config.macros.clone());

    // Restore the arrangement. This also drops live overrides, since a reload
    // returns full authority to the show.
    mixer.set_arrangement(config.arrangement.clone());

    // Restore active transition
    if let Some(transition_name) = &config.active_transition {
        if let Some(shader) = registry
            .transitions()
            .iter()
            .find(|s| s.name() == *transition_name)
        {
            match mixer.set_transition(context, (*shader).clone()) {
                Ok(()) => {}
                Err(e) => {
                    let msg = format!("Failed to restore transition '{transition_name}': {e}");
                    log::warn!("{msg}");
                    warnings.push(msg);
                }
            }
        } else {
            warnings.push(format!(
                "Transition '{transition_name}' not found in registry"
            ));
        }
    }

    // Restore transition sequences. Fade steps address channels by UUID; scenes
    // at v4 and earlier stored indices, which `ChannelRef::resolve` maps through
    // the restored channel order.
    let channel_uuids: Vec<String> = mixer
        .channels()
        .iter()
        .map(|ch| ch.uuid().to_string())
        .collect();
    for seq_config in &config.transition_sequences {
        let steps = restore_sequence_steps(&seq_config.steps, &channel_uuids, &mut warnings);
        mixer
            .transition_sequences_mut()
            .push(crate::mixer::TransitionSequence::with_uuid(
                seq_config.uuid.clone(),
                seq_config.name.clone(),
                steps,
                seq_config.enabled,
            ));
    }

    // Restore tonemap mode
    mixer.set_tonemap_mode(&context.queue, config.tonemap_mode);

    // Restore active LUT
    if let Some(lut_filename) = &config.active_lut {
        let lut_path = std::env::current_dir()
            .unwrap_or_default()
            .join(".varda/luts")
            .join(lut_filename);
        match crate::renderer::lut::parse_lut_file(&lut_path) {
            Ok(parsed) => {
                mixer.load_lut(
                    &context.device,
                    &context.queue,
                    &parsed,
                    lut_filename.clone(),
                );
                log::info!("Restored LUT: {lut_filename}");
            }
            Err(e) => {
                let msg = format!("Failed to restore LUT '{lut_filename}': {e}");
                log::warn!("{msg}");
                warnings.push(msg);
            }
        }
    }

    Ok(RestoreResult {
        mixer,
        warnings,
        pending_syphon,
    })
}

/// Restore a single deck from config.
// Needs many independent GPU/context inputs to rebuild a deck; no shared invariant to bundle.
/// Reacquire and attach a shader deck's depth-sensor preprocessor on restore.
///
/// Resolves the sensor by saved name when the scene recorded one, falling back
/// to the ISF header's device selection for scenes written before the binding
/// was persisted. Returns `Err` when the shader needs a sensor and none is
/// available, so the caller skips the deck.
fn restore_depth_preprocessor(
    deck: &mut Deck,
    saved: Option<&crate::scene::DepthPreproConfig>,
    metadata: &crate::isf::ISFMetadata,
    shader_path: &str,
    depth_manager: &mut crate::depth::DepthSensorManager,
    context: &GpuContext,
) -> Result<()> {
    use crate::depth::preprocess::{DepthPreprocessParams, DepthPreprocessPipeline};

    if crate::depth::preprocess::requested_device(metadata).is_none() {
        return Ok(());
    }

    let params = saved.map_or_else(DepthPreprocessParams::default, |c| DepthPreprocessParams {
        near_mm: c.near_mm,
        far_mm: c.far_mm.max(c.near_mm + 1.0),
        smoothing: c.smoothing,
        hole_fill: c.hole_fill,
        mask_feather: c.mask_feather,
        motion_gain: c.motion_gain,
        mirror: c.mirror,
    });

    // Prefer the saved device name; fall back to the header's selection.
    let by_name = saved.and_then(|c| {
        depth_manager
            .devices()
            .iter()
            .find(|d| d.name == c.sensor_name)
            .cloned()
    });

    let (id, name, width, height) = if let Some(info) = by_name {
        let (w, h) = crate::depth::open_depth_sensor(depth_manager, info.id, &context.device)
            .with_context(|| format!("Failed to open depth sensor '{}'", info.name))?;
        (info.id, info.name, w, h)
    } else {
        let sensor = crate::depth::preprocess::acquire_for_shader(
            depth_manager,
            &context.device,
            metadata,
            shader_path,
        )?
        .context("depth_sensor preprocessor declared but not acquired")?;
        deck.attach_depth_preprocessor(sensor.id, sensor.name, sensor.pipeline, params);
        return Ok(());
    };

    deck.attach_depth_preprocessor(
        id,
        name,
        DepthPreprocessPipeline::new(&context.device, width, height),
        params,
    );
    Ok(())
}

/// A slot for `deck`, built from its saved `config` with the slot's own
/// settings put back — how strong, see-through, blended and seen it is, the
/// size and pace it is drawn at, and its auto-transition. Every path that
/// builds a deck from a saved one — a load, a paste or duplicate, a preset —
/// goes through here, so none forgets one: a duplicated tap came back
/// flattening, the paste path having left its transparency out.
pub(crate) fn slot_from_config(
    deck: crate::deck::Deck,
    config: &crate::scene::DeckConfig,
    context: &GpuContext,
    registry: &crate::registry::ShaderRegistry,
) -> crate::channel::DeckSlot {
    let mut slot = crate::channel::DeckSlot::new(deck);
    apply_slot_settings(&mut slot, config, context);
    if let Some(at_config) = &config.auto_transition {
        use crate::channel::{DeckAutoTransition, TransitionTrigger};
        let mut at = DeckAutoTransition::new();
        at.enabled = at_config.enabled;
        at.trigger = match at_config.trigger {
            TriggerConfig::Timer => TransitionTrigger::Timer,
            TriggerConfig::ClipEnd => TransitionTrigger::ClipEnd,
        };
        at.play_duration = duration_config_to_spec(&at_config.play_duration);
        at.transition_duration = duration_config_to_spec(&at_config.transition_duration);
        at.transition_shader_name
            .clone_from(&at_config.transition_shader);
        slot.auto_transition = Some(at);

        // Compile transition shader if specified
        if let Some(shader_name) = &at_config.transition_shader {
            if let Some(shader) = registry
                .transitions()
                .iter()
                .find(|s| s.name() == *shader_name)
            {
                if let Err(e) = slot.set_transition_shader(context, (*shader).clone()) {
                    log::warn!("Failed to restore deck transition shader '{shader_name}': {e}");
                }
            } else {
                log::warn!("Deck transition shader '{shader_name}' not found in registry");
            }
        }
    }
    slot
}

/// A slot's own saved settings put back from `config`: its opacity,
/// transparency, blend, mute, solo and place, and the pace and size it is
/// drawn at (see [`slot_from_config`]).
pub(crate) fn apply_slot_settings(
    slot: &mut crate::channel::DeckSlot,
    config: &crate::scene::DeckConfig,
    context: &GpuContext,
) {
    slot.opacity = config.opacity;
    slot.deck.set_transparent(config.transparent);
    slot.blend_mode = config.blend_mode.into();
    slot.mute = config.mute;
    slot.solo = config.solo;
    slot.z_index = config.z_index;
    slot.render_fps = config.render_fps;
    slot.deck.set_render_scale(context, config.render_scale);
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn restore_deck(
    config: &DeckConfig,
    context: &GpuContext,
    _registry: &crate::registry::ShaderRegistry,
    camera_manager: &mut crate::camera::CameraManager,
    screen_capture_manager: &mut crate::screen_capture::ScreenCaptureManager,
    depth_manager: &mut crate::depth::DepthSensorManager,
    ndi_manager: &mut crate::ndi::NdiManager,
    stream_manager: &mut crate::stream::StreamManager,
    html_manager: &mut crate::html::HtmlManager,
    render_width: u32,
    render_height: u32,
) -> Result<Deck> {
    let mut deck = match &config.source {
        SourceConfig::Shader {
            path,
            params,
            depth_prepro,
        } => {
            let shader = ISFShader::from_file(path)
                .with_context(|| format!("Failed to load shader: {path}"))?;
            let metadata = shader.metadata.clone();
            let mut deck = if shader.metadata.is_compute() {
                Deck::new_from_compute_shader(context, shader, render_width, render_height)?
            } else {
                Deck::new(context, shader, render_width, render_height)?
            };
            // Restore parameter values
            for (name, value) in params {
                deck.generator_params.set(name, *value);
            }
            // Reacquire the depth sensor this shader needs. `depth_sensor` is a
            // required preprocessor, so a missing device fails the restore and
            // the caller skips the deck with a warning — the same handling a
            // missing camera or depth-sensor deck already gets.
            restore_depth_preprocessor(
                &mut deck,
                depth_prepro.as_ref(),
                &metadata,
                path,
                depth_manager,
                context,
            )?;
            deck
        }
        SourceConfig::Video {
            path,
            loop_mode,
            speed,
            in_point,
            out_point,
            scaling_mode,
            transport_sync,
            auto,
        } => {
            let mut deck = Deck::new_from_video(context, path, render_width, render_height)?;
            deck.video_set_loop_mode(*loop_mode);
            deck.video_set_speed(*speed);
            deck.video_set_in_point(*in_point);
            deck.video_set_out_point(*out_point);
            if *in_point > 0.0 {
                // It opens paused: waiting on its in-point, not the file's
                // first frame.
                deck.video_seek(*in_point);
            }
            deck.set_scaling_mode(*scaling_mode);
            deck.video_set_transport_sync(*transport_sync);
            deck.auto_playback = *auto;
            deck
        }
        SourceConfig::Image { path, scaling_mode } => {
            let mut deck = Deck::new_from_image(context, path, render_width, render_height)?;
            deck.set_scaling_mode(*scaling_mode);
            deck
        }
        SourceConfig::SolidColor { color } => {
            Deck::new_solid_color(context, *color, render_width, render_height)?
        }
        SourceConfig::Camera {
            name,
            mode,
            device,
            colour,
            profile,
            lowest_latency,
        } => {
            // Found by its device's unique ID, or else by name. One not
            // plugged in, or not allowed yet, is kept as a black deck that
            // takes its camera as soon as it can be opened — dropping it
            // would lose the input, and its sound's link to it, with the
            // next save.
            let opened = camera_manager
                .find(device.as_deref(), name)
                .ok_or_else(|| anyhow::anyhow!("not found"))
                .and_then(|id| {
                    camera_manager
                        .open_camera(id, &context.device, *mode)
                        .map(|size| (id, size))
                });
            let (camera_id, (src_w, src_h)) = match opened {
                Ok(opened) => opened,
                Err(e) => {
                    log::warn!("Camera '{name}' not available ({e}) — waiting for it");
                    let size = mode.map_or((1920, 1080), |m| (m.width, m.height));
                    (crate::camera::UNBOUND_CAMERA, size)
                }
            };
            let mut deck = Deck::new_from_camera(
                context,
                camera_id,
                name,
                src_w,
                src_h,
                render_width,
                render_height,
            )?;
            deck.camera_mode = *mode;
            deck.camera_device = camera_manager
                .unique_id(camera_id)
                .map(str::to_string)
                .or_else(|| device.clone());
            // From its first frame, not only once the frame loop passes it on.
            deck.camera_colour = *colour;
            camera_manager.set_colour(camera_id, *colour);
            deck.camera_lowest_latency = *lowest_latency;
            camera_manager.set_lowest_latency(camera_id, *lowest_latency);
            // Converted by the frame loop, which tells the camera each frame
            // (see `CameraManager::set_profiles`).
            deck.camera_profile.clone_from(profile);
            deck
        }
        SourceConfig::ScreenCapture {
            target,
            rate,
            crop,
            show_cursor,
            exclude_varda,
            scaling_mode,
        } => {
            let identity = crate::screen_capture::backend::TargetIdentity::from(target);
            let found = screen_capture_manager.find_target(&identity).cloned();
            let config = crate::screen_capture::backend::CaptureConfig {
                rate: *rate,
                crop: crop.map(Into::into).unwrap_or_default(),
                show_cursor: *show_cursor,
                exclude_varda: exclude_varda.unwrap_or_else(|| target.is_display()),
                // Capped to the deck but kept in the target's own shape, so the
                // deck's scaling mode still has an aspect mismatch to resolve.
                // Falls back to the deck box when the target is missing; the
                // real size is picked up if it comes back and rebinds.
                scale_to: Some(
                    found
                        .as_ref()
                        .map_or((render_width, render_height), |info| {
                            crate::screen_capture::resample::fit_within(
                                info.width,
                                info.height,
                                render_width,
                                render_height,
                            )
                        }),
                ),
            }
            .sanitized();

            // Unlike a camera, a missing capture target does **not** drop the
            // deck. Windows come and go constantly, and silently losing a deck
            // (with its effect chain, opacity, and MIDI mappings) because an app
            // was closed would be a bad live-performance failure. The deck is
            // restored unbound and renders black until the target reappears or
            // the user repoints it.
            // See spec/screen-capture.md § Configuration and Persistence.
            let opened = found.and_then(|info| {
                match screen_capture_manager.open(&info, config.clone(), &context.device) {
                    Ok(bound) => Some((info.label.clone(), bound)),
                    Err(e) => {
                        log::warn!(
                            "Capture target '{}' found but could not be opened: {e}",
                            info.label
                        );
                        None
                    }
                }
            });

            let (label, capture_id, src_w, src_h) = opened.map_or_else(
                || {
                    log::warn!(
                        "Capture target '{}' not available — deck restored unbound",
                        target.label()
                    );
                    (
                        target.label(),
                        crate::screen_capture::UNBOUND_CAPTURE_ID,
                        render_width,
                        render_height,
                    )
                },
                |(label, (id, w, h))| (label, id, w, h),
            );

            let mut deck = Deck::new_from_screen_capture(
                context,
                crate::deck::ScreenCaptureState {
                    capture_id,
                    identity,
                    config,
                    config_dirty: false,
                },
                &label,
                src_w,
                src_h,
                render_width,
                render_height,
            )?;
            deck.set_scaling_mode(*scaling_mode);
            deck
        }
        SourceConfig::Tap {
            source,
            scaling_mode,
        } => {
            // A tap holds no handle and acquires nothing, so restore cannot
            // fail. A source that no longer exists simply renders black until
            // the channel returns. See spec/program-tap.md.
            let tap_source = crate::deck::TapSource::from(source);
            let label = tap_source.label(&[]);
            let mut deck =
                Deck::new_from_tap(context, tap_source, &label, render_width, render_height)?;
            deck.set_scaling_mode(*scaling_mode);
            deck
        }
        // Its files are opened as it plays; one missing is skipped then.
        SourceConfig::Playlist {
            items,
            settings,
            scaling_mode,
        } => {
            let mut deck = Deck::new_playlist(
                context,
                items.clone(),
                *settings,
                render_width,
                render_height,
            );
            deck.set_scaling_mode(*scaling_mode);
            deck
        }
        SourceConfig::Timer { settings, elapsed } => Deck::new_timer(
            context,
            settings.clone(),
            *elapsed,
            render_width,
            render_height,
        )?,
        SourceConfig::Ndi { name } => match ndi_manager.start_receive(name, &context.device) {
            Some(receiver_idx) => {
                let (src_w, src_h) = ndi_manager
                    .receiver_dimensions(receiver_idx)
                    .unwrap_or((1920, 1080));
                Deck::new_from_ndi(
                    context,
                    receiver_idx,
                    name,
                    src_w,
                    src_h,
                    render_width,
                    render_height,
                )?
            }
            None => {
                return Err(anyhow::anyhow!(
                    "NDI source '{name}' not available for restore"
                ));
            }
        },
        SourceConfig::Syphon { name } => {
            // Syphon sources are resolved at runtime — skip if not on macOS
            log::warn!(
                "Syphon source '{name}' restoration not yet implemented (needs SyphonManager)"
            );
            return Err(anyhow::anyhow!(
                "Syphon source '{name}' not available for restore"
            ));
        }
        SourceConfig::Srt { url, mode } => {
            let srt_mode = match mode.as_str() {
                "listener" => crate::stream::SrtMode::Listener,
                "caller" => crate::stream::SrtMode::Caller,
                other => {
                    log::warn!("Unknown SRT mode '{other}', defaulting to Caller");
                    crate::stream::SrtMode::Caller
                }
            };
            match stream_manager.start_srt_receive(url, srt_mode, &context.device) {
                Some(receiver_idx) => {
                    let (src_w, src_h) = stream_manager
                        .receiver_dimensions(receiver_idx)
                        .unwrap_or((1920, 1080));
                    Deck::new_from_srt(
                        context,
                        receiver_idx,
                        url,
                        src_w,
                        src_h,
                        render_width,
                        render_height,
                    )?
                }
                None => {
                    return Err(anyhow::anyhow!(
                        "SRT source '{url}' not available for restore"
                    ));
                }
            }
        }
        SourceConfig::Hls { url } => {
            match stream_manager.start_receive(
                url,
                crate::stream::StreamProtocol::Hls,
                &context.device,
            ) {
                Some(receiver_idx) => {
                    let (src_w, src_h) = stream_manager
                        .receiver_dimensions(receiver_idx)
                        .unwrap_or((1920, 1080));
                    Deck::new_from_hls(
                        context,
                        receiver_idx,
                        url,
                        src_w,
                        src_h,
                        render_width,
                        render_height,
                    )?
                }
                None => {
                    return Err(anyhow::anyhow!(
                        "HLS source '{url}' not available for restore"
                    ));
                }
            }
        }
        SourceConfig::Dash { url } => {
            match stream_manager.start_receive(
                url,
                crate::stream::StreamProtocol::Dash,
                &context.device,
            ) {
                Some(receiver_idx) => {
                    let (src_w, src_h) = stream_manager
                        .receiver_dimensions(receiver_idx)
                        .unwrap_or((1920, 1080));
                    Deck::new_from_dash(
                        context,
                        receiver_idx,
                        url,
                        src_w,
                        src_h,
                        render_width,
                        render_height,
                    )?
                }
                None => {
                    return Err(anyhow::anyhow!(
                        "DASH source '{url}' not available for restore"
                    ));
                }
            }
        }
        SourceConfig::Rtmp { url, mode } => {
            let rtmp_mode = match mode.as_str() {
                "listen" | "Listen" => crate::stream::RtmpMode::Listen,
                _ => crate::stream::RtmpMode::Pull,
            };
            match stream_manager.start_rtmp_receive(url, rtmp_mode, &context.device) {
                Some(receiver_idx) => {
                    let (src_w, src_h) = stream_manager
                        .receiver_dimensions(receiver_idx)
                        .unwrap_or((1920, 1080));
                    Deck::new_from_rtmp(
                        context,
                        receiver_idx,
                        url,
                        src_w,
                        src_h,
                        render_width,
                        render_height,
                    )?
                }
                None => {
                    return Err(anyhow::anyhow!(
                        "RTMP source '{url}' not available for restore"
                    ));
                }
            }
        }
        SourceConfig::Html { url } => {
            match html_manager.start_render(url, render_width, render_height, &context.device) {
                Some(instance_idx) => {
                    let (src_w, src_h) = html_manager
                        .instance_dimensions(instance_idx)
                        .unwrap_or((1920, 1080));
                    Deck::new_from_html(
                        context,
                        instance_idx,
                        url,
                        src_w,
                        src_h,
                        render_width,
                        render_height,
                    )?
                }
                None => {
                    return Err(anyhow::anyhow!(
                        "HTML source '{url}' not available for restore"
                    ));
                }
            }
        }
        SourceConfig::DepthSensor { name, params } => {
            // Match the sensor by name in the manager's device list, then open it.
            // If absent (e.g. `depth` feature off or unplugged), skip with error.
            let device = depth_manager
                .devices()
                .iter()
                .find(|d| d.name == *name)
                .cloned()
                .ok_or_else(|| {
                    anyhow::anyhow!("Depth sensor '{name}' not found — is it connected?")
                })?;
            let (src_w, src_h) =
                crate::depth::open_depth_sensor(depth_manager, device.id, &context.device)
                    .with_context(|| format!("Failed to open depth sensor '{name}'"))?;
            let mut deck = Deck::new_from_depth_sensor(
                context,
                device.id,
                &device.name,
                src_w,
                src_h,
                render_width,
                render_height,
            )?;
            if let Some(p) = params {
                use crate::depth::point_cloud::{ColorMode, PointCloudParams};
                deck.point_cloud_params = PointCloudParams {
                    orbit_yaw: p.orbit_yaw,
                    orbit_pitch: p.orbit_pitch,
                    zoom: p.zoom,
                    point_size: p.point_size,
                    color_mode: ColorMode::from_u8(p.color_mode),
                    depth_min_mm: p.depth_min_mm,
                    depth_max_mm: p.depth_max_mm,
                    solid_color: p.solid_color,
                    seed: p.seed,
                    drift: p.drift,
                    disruption: p.disruption,
                };
            }
            deck
        }
    };

    // Restore UUID from config
    if !config.uuid.is_empty() {
        deck.set_uuid(config.uuid.clone());
    }

    // Restore effects
    for eff_config in &config.effects {
        match restore_effect(eff_config, context, context.compositing_format) {
            Ok(eff) => deck.effects.push(eff),
            Err(e) => log::warn!("Failed to restore deck effect '{}': {}", eff_config.path, e),
        }
    }
    deck.settle_layer_mask();

    Ok(deck)
}

/// Restore a single effect from config.
/// `target_format` is `context.compositing_format` for every tier — deck,
/// channel, and master effects all target the unified color-path format.
pub(crate) fn restore_effect(
    config: &EffectConfig,
    context: &GpuContext,
    target_format: wgpu::TextureFormat,
) -> Result<Effect> {
    let shader = ISFShader::from_file(&config.path)
        .with_context(|| format!("Failed to load effect shader: {}", config.path))?;
    let mut effect = Effect::new_with_format(context, shader, target_format)?;
    effect.set_uuid(config.uuid.clone());
    effect.enabled = config.enabled;
    effect.mask = config
        .mask
        .clone()
        .and_then(crate::engine::value::mask::EffectMask::sanitized);
    let mut params = config.params.clone();
    if effect.shader.name() == "transform" {
        migrate_legacy_transform(&mut params);
    }
    // Restore parameter values
    for (name, value) in &params {
        effect.params.set(name, *value);
    }
    Ok(effect)
}

/// Bring a transform saved before `scale` existed onto the current shader,
/// so a show looks the same after the update.
///
/// The old shader moved the picture in scaled, rotated units — its centre sat
/// at `0.5 + S·R(rotation)·pos` — and had no uniform scale. Position is now in
/// frame units whatever the scale, so the old position is carried through the
/// old scale and rotation; equal X/Y scale becomes the uniform `scale`, and
/// unequal X/Y turns on "Separate X / Y scale" to keep the stretch. Rotation
/// now keeps the picture's shape, so a rotated picture is corrected rather
/// than reproduced.
pub(crate) fn migrate_legacy_transform(
    params: &mut std::collections::HashMap<String, crate::params::ParamValue>,
) {
    use crate::params::ParamValue;
    if params.contains_key("scale") {
        return;
    }
    let get = |name: &str, default: f32| match params.get(name) {
        Some(ParamValue::Float(v)) => *v,
        _ => default,
    };
    let (sx, sy) = (get("scale_x", 1.0), get("scale_y", 1.0));
    let (px, py) = (get("pos_x", 0.0), get("pos_y", 0.0));
    let (sin, cos) = get("rotation", 0.0).sin_cos();
    params.insert(
        "pos_x".into(),
        ParamValue::Float(sx * (px * cos - py * sin)),
    );
    params.insert(
        "pos_y".into(),
        ParamValue::Float(sy * (px * sin + py * cos)),
    );
    if (sx - sy).abs() < 1e-6 {
        params.insert("scale".into(), ParamValue::Float(sx));
        params.insert("scale_x".into(), ParamValue::Float(1.0));
        params.insert("scale_y".into(), ParamValue::Float(1.0));
        params.insert("scale_mode".into(), ParamValue::Bool(false));
    } else {
        params.insert("scale".into(), ParamValue::Float(1.0));
        params.insert("scale_mode".into(), ParamValue::Bool(true));
    }
}

/// Check if a live deck's source matches a target `SourceConfig` (same type + same path/name).
/// Used by diff-apply to decide whether a deck can be patched in place or must be rebuilt.
// Each arm pairs one source_type string with its matching config variant; merging
// same-bodied arms would let mismatched type/config pairs compare equal.
#[allow(clippy::match_same_arms)]
pub(crate) fn source_configs_match(deck: &Deck, config: &SourceConfig) -> bool {
    match (deck.source_type(), config) {
        ("shader", SourceConfig::Shader { path, .. }) => deck.source_path() == Some(path.as_str()),
        ("video", SourceConfig::Video { path, .. }) => deck.source_path() == Some(path.as_str()),
        ("image", SourceConfig::Image { path, .. }) => deck.source_path() == Some(path.as_str()),
        ("solid_color", SourceConfig::SolidColor { .. }) => true,
        ("camera", SourceConfig::Camera { name, .. }) => {
            deck.source_name().trim_start_matches("📹 ") == name
        }
        ("ndi", SourceConfig::Ndi { name }) => deck.source_name().trim_start_matches("📡 ") == name,
        ("syphon", SourceConfig::Syphon { name }) => {
            deck.source_name().trim_start_matches("🔗 ") == name
        }
        ("srt", SourceConfig::Srt { url, .. }) => {
            deck.source_name().trim_start_matches("📺 ") == url
        }
        ("hls", SourceConfig::Hls { url }) => deck.source_name().trim_start_matches("📡 ") == url,
        ("dash", SourceConfig::Dash { url }) => deck.source_name().trim_start_matches("📡 ") == url,
        ("rtmp", SourceConfig::Rtmp { url, .. }) => {
            deck.source_name().trim_start_matches("📺 ") == url
        }
        ("html", SourceConfig::Html { url }) => deck.source_name().trim_start_matches("🌐 ") == url,
        ("depth_sensor", SourceConfig::DepthSensor { name, .. }) => {
            sensor_of(deck.source_name()) == name
        }
        // Compare the stored identity rather than the display label: a window
        // whose title changed is still the same source and must be patched in
        // place, not torn down and rebuilt mid-show.
        ("screen_capture", SourceConfig::ScreenCapture { target, .. }) => deck
            .screen_capture
            .as_ref()
            .is_some_and(|s| crate::scene::CaptureTargetConfig::from(&s.identity) == *target),
        // Compared by tap point, not by label: renaming a channel must patch
        // the deck in place rather than rebuild it.
        ("tap", SourceConfig::Tap { source, .. }) => deck
            .tap
            .as_ref()
            .is_some_and(|t| crate::scene::TapSourceConfig::from(&t.source) == *source),
        // Its items and settings are patched in place: an undo does not
        // stop the list or send it back to its first item.
        ("playlist", SourceConfig::Playlist { .. }) => true,
        // Its settings are patched in place: an undo does not reset it.
        ("timer", SourceConfig::Timer { .. }) => true,
        _ => false,
    }
}

/// What starts a depth-sensor layer's label, before its sensor's name
/// (`deck::source` makes it).
const SENSOR_PREFIX: &str = "📡 ";

/// The prefix before that one, which no font Nida draws with has — it drew
/// as a box. Older saved presets still carry it.
const OLD_SENSOR_PREFIX: &str = "🛰 ";

/// The sensor a depth-sensor layer's label names, after either prefix.
fn sensor_of(label: &str) -> &str {
    label
        .strip_prefix(SENSOR_PREFIX)
        .or_else(|| label.strip_prefix(OLD_SENSOR_PREFIX))
        .unwrap_or(label)
}

/// A saved layer label as it is drawn now: an older preset's depth-sensor
/// prefix replaced by today's.
pub(crate) fn label_now(label: &str) -> String {
    label
        .strip_prefix(OLD_SENSOR_PREFIX)
        .map_or_else(|| label.to_string(), |sensor| format!("{SENSOR_PREFIX}{sensor}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::GpuContext;
    use std::collections::HashMap;

    /// A depth sensor is found by its name whichever prefix its layer's
    /// label carries, and an older preset's label is drawn with today's.
    #[test]
    fn a_sensor_is_found_under_either_prefix() {
        assert_eq!(sensor_of("📡 Kinect"), "Kinect");
        assert_eq!(sensor_of("🛰 Kinect"), "Kinect", "an older preset's label");
        assert_eq!(sensor_of("Kinect"), "Kinect");
        assert_eq!(label_now("🛰 Kinect"), "📡 Kinect");
        assert_eq!(label_now("Intro"), "Intro");
    }

    fn headless_gpu() -> GpuContext {
        GpuContext::new_headless().expect("headless GPU required for tests")
    }

    fn legacy(pairs: &[(&str, f32)]) -> HashMap<String, crate::params::ParamValue> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), crate::params::ParamValue::Float(*v)))
            .collect()
    }

    fn float(params: &HashMap<String, crate::params::ParamValue>, name: &str) -> f32 {
        match params.get(name) {
            Some(crate::params::ParamValue::Float(v)) => *v,
            other => panic!("{name}: {other:?}"),
        }
    }

    /// A saved picture-in-picture must land where it did: the old shader
    /// moved it by position × scale, so 0.3-scale at (0.05, 1.0) sat at
    /// (0.015, 0.3) from the centre, and equal X/Y becomes the uniform scale.
    #[test]
    fn a_legacy_uniform_transform_keeps_its_place_and_size() {
        let mut p = legacy(&[
            ("scale_x", 0.3),
            ("scale_y", 0.3),
            ("pos_x", 0.05),
            ("pos_y", 1.0),
        ]);
        migrate_legacy_transform(&mut p);
        assert!((float(&p, "scale") - 0.3).abs() < 1e-6);
        assert!((float(&p, "pos_x") - 0.015).abs() < 1e-6);
        assert!((float(&p, "pos_y") - 0.3).abs() < 1e-6);
        assert_eq!(float(&p, "scale_x"), 1.0);
        assert_eq!(float(&p, "scale_y"), 1.0);
        assert!(matches!(
            p.get("scale_mode"),
            Some(crate::params::ParamValue::Bool(false))
        ));
    }

    #[test]
    fn a_legacy_stretched_transform_stays_stretched() {
        let mut p = legacy(&[("scale_x", 2.0), ("scale_y", 1.0), ("pos_x", 0.1)]);
        migrate_legacy_transform(&mut p);
        assert_eq!(float(&p, "scale"), 1.0);
        assert_eq!(float(&p, "scale_x"), 2.0);
        assert!((float(&p, "pos_x") - 0.2).abs() < 1e-6);
        assert!(matches!(
            p.get("scale_mode"),
            Some(crate::params::ParamValue::Bool(true))
        ));
    }

    #[test]
    fn a_legacy_rotated_transform_keeps_its_centre() {
        // The old shader rotated the offset too: (0.2, 0) at 90° put the
        // centre at (0, 0.2).
        let mut p = legacy(&[("pos_x", 0.2), ("rotation", std::f32::consts::FRAC_PI_2)]);
        migrate_legacy_transform(&mut p);
        assert!(float(&p, "pos_x").abs() < 1e-6);
        assert!((float(&p, "pos_y") - 0.2).abs() < 1e-6);
    }

    #[test]
    fn a_current_transform_is_left_alone() {
        let mut p = legacy(&[("scale", 0.5), ("pos_x", 0.4), ("scale_x", 3.0)]);
        let before = p.clone();
        migrate_legacy_transform(&mut p);
        assert_eq!(format!("{p:?}").len(), format!("{before:?}").len());
        assert_eq!(float(&p, "pos_x"), 0.4);
        assert_eq!(float(&p, "scale_x"), 3.0);
        assert!(!p.contains_key("scale_mode"));
    }

    /// A restored effect must answer to the UUID it was saved under.
    ///
    /// Modulation targets an effect parameter by the key `fx_{uuid}:{param}`,
    /// and the render path looks that key up through the effect's cached
    /// `param_prefix`. Restore used to write `uuid` directly, leaving the prefix
    /// built from the throwaway UUID `Effect::new` mints. Everything that keys
    /// off `uuid` — the UI, assignment, removal — agreed with the saved scene,
    /// so the modulation still looked attached, while the one consumer that used
    /// the prefix silently found nothing. Effect modulation therefore worked
    /// until you reloaded, and never again after.
    #[test]
    fn restored_effect_keeps_the_uuid_its_modulation_is_keyed_on() {
        let gpu = headless_gpu();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("shaders/invert.fs");
        assert!(path.exists(), "shaders/invert.fs missing");

        let cfg = EffectConfig {
            uuid: "fxsaved1".to_string(),
            path: path.to_string_lossy().into_owned(),
            enabled: true,
            params: HashMap::new(),
            mask: None,
        };
        let effect = restore_effect(&cfg, &gpu, crate::renderer::COLOR_PATH_FORMAT)
            .expect("invert.fs restores");

        assert_eq!(effect.uuid(), "fxsaved1");
        assert_eq!(
            effect.param_prefix(),
            "fx_fxsaved1",
            "the prefix modulation is looked up under must follow the restored UUID"
        );
    }

    /// `set_uuid` is the only way to move an effect's identity, so it has to
    /// carry the derived prefix with it.
    #[test]
    fn setting_an_effect_uuid_moves_its_modulation_prefix() {
        let gpu = headless_gpu();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("shaders/invert.fs");
        let shader = crate::isf::ISFShader::from_file(&path).expect("parse invert.fs");
        let mut effect = Effect::new(&gpu, shader).expect("build effect");

        effect.set_uuid("abcd1234".to_string());
        assert_eq!(effect.uuid(), "abcd1234");
        assert_eq!(effect.param_prefix(), "fx_abcd1234");
    }

    #[test]
    fn source_configs_match_solid_color() {
        let gpu = headless_gpu();
        let deck = crate::deck::Deck::new_solid_color(&gpu, [1.0, 0.0, 0.0, 1.0], 64, 64).unwrap();
        // Any solid color config matches a solid color deck
        assert!(source_configs_match(
            &deck,
            &SourceConfig::SolidColor {
                color: [0.0, 1.0, 0.0, 1.0]
            }
        ));
        // But not other types
        assert!(!source_configs_match(
            &deck,
            &SourceConfig::Video {
                path: "test.mp4".into(),
                loop_mode: crate::video::LoopMode::default(),
                speed: 1.0,
                in_point: 0.0,
                out_point: 0.0,
                scaling_mode: crate::deck::ScalingMode::default(),
                transport_sync: crate::video::DeckTransportSync::default(),
                auto: crate::engine::value::video::AutoPlayback::default(),
            }
        ));
        assert!(!source_configs_match(
            &deck,
            &SourceConfig::Shader {
                path: "test.fs".into(),
                params: HashMap::new(),
                depth_prepro: None
            }
        ));
    }

    #[test]
    fn source_configs_match_type_mismatch() {
        let gpu = headless_gpu();
        let deck = crate::deck::Deck::new_solid_color(&gpu, [1.0, 0.0, 0.0, 1.0], 64, 64).unwrap();
        assert!(!source_configs_match(
            &deck,
            &SourceConfig::Image {
                path: "test.png".into(),
                scaling_mode: crate::deck::ScalingMode::default()
            }
        ));
        assert!(!source_configs_match(
            &deck,
            &SourceConfig::Camera {
                name: "cam".into(),
                mode: None,
                device: None,
                colour: crate::engine::value::camera::CameraColour::default(),
                profile: crate::engine::value::camera::CameraProfile::Normal,
                lowest_latency: false,
            }
        ));
        assert!(!source_configs_match(
            &deck,
            &SourceConfig::Ndi { name: "src".into() }
        ));
    }

    #[test]
    fn snapshot_and_match_solid_color_roundtrip() {
        let gpu = headless_gpu();
        let mut mixer = Mixer::new(&gpu, 64, 64).unwrap();
        // Clear default channels and add one with a solid color deck
        mixer.channels_mut().clear();
        let mut ch = crate::channel::Channel::new("Ch 0".into(), &gpu, 64, 64).unwrap();
        let deck = crate::deck::Deck::new_solid_color(&gpu, [1.0, 0.5, 0.0, 1.0], 64, 64).unwrap();
        ch.add_deck(deck);
        mixer.channels_mut().push(ch);

        // Snapshot and verify source match
        let config = snapshot_scene(&mixer, None, 64, 64);
        let deck_ref = &mixer.channels()[0].decks[0].deck;
        assert!(source_configs_match(
            deck_ref,
            &config.channels[0].decks[0].source
        ));
    }

    #[test]
    fn a_deck_waiting_for_its_camera_takes_it_once_opened() {
        let gpu = headless_gpu();
        let mut deck = crate::deck::Deck::new_from_camera(
            &gpu,
            crate::camera::UNBOUND_CAMERA,
            "Blackmagic Design",
            1920,
            1080,
            64,
            64,
        )
        .unwrap();
        assert_eq!(deck.camera_waiting_for(), Some("Blackmagic Design"));
        deck.bind_camera(2, (1280, 720));
        assert_eq!(deck.camera_waiting_for(), None);
        assert_eq!(deck.camera_id(), Some(2));
    }

    #[test]
    fn a_camera_is_saved_with_the_format_chosen_for_it() {
        let gpu = headless_gpu();
        let mut mixer = Mixer::new(&gpu, 64, 64).unwrap();
        mixer.channels_mut().clear();
        let mut ch = crate::channel::Channel::new("Stage cam".into(), &gpu, 64, 64).unwrap();
        // No camera is opened: the deck is only named after one.
        let mut deck =
            crate::deck::Deck::new_from_camera(&gpu, 1, "Blackmagic Design", 1920, 1080, 64, 64)
                .unwrap();
        let mode = crate::engine::value::camera::CameraMode {
            width: 1920,
            height: 1080,
            fps: 50,
        };
        deck.camera_mode = Some(mode);
        deck.camera_device = Some("0x1100001edb4100".into());
        let colour = crate::engine::value::camera::CameraColour {
            range: crate::engine::value::camera::CameraRange::Full,
            matrix: crate::engine::value::camera::CameraMatrix::Bt709,
        };
        deck.camera_colour = colour;
        deck.camera_profile = crate::engine::value::camera::CameraProfile::BlackmagicFilmGen5;
        deck.camera_lowest_latency = true;
        ch.add_deck(deck);
        mixer.channels_mut().push(ch);

        let config = snapshot_scene(&mixer, None, 64, 64);
        match &config.channels[0].decks[0].source {
            SourceConfig::Camera {
                name,
                mode: saved,
                device,
                colour: saved_colour,
                profile: saved_profile,
                lowest_latency,
            } => {
                assert!(*lowest_latency, "and its lowest latency");
                assert_eq!(name, "Blackmagic Design");
                assert_eq!(*saved, Some(mode));
                assert_eq!(device.as_deref(), Some("0x1100001edb4100"));
                assert_eq!(*saved_colour, colour, "and the colour set for it");
                assert_eq!(
                    *saved_profile,
                    crate::engine::value::camera::CameraProfile::BlackmagicFilmGen5,
                    "and its profile"
                );
            }
            other => panic!("expected a camera, got {other:?}"),
        }
    }

    #[test]
    fn restore_effect_pub_crate_accessible() {
        // Just verify the function signature is accessible at pub(crate) level
        let gpu = headless_gpu();
        let cfg = EffectConfig {
            uuid: "test0001".to_string(),
            path: "nonexistent.fs".into(),
            enabled: true,
            params: HashMap::new(),
            mask: None,
        };
        // Should fail (file doesn't exist) but shouldn't be a compile error
        assert!(restore_effect(&cfg, &gpu, wgpu::TextureFormat::Rgba8Unorm).is_err());
    }

    #[test]
    fn validate_stage_prefs_valid() {
        let prefs = StagePrefs::default();
        assert!(prefs.validate().is_empty());
    }

    #[test]
    fn validate_stage_prefs_grid_size_invalid() {
        let mut prefs = StagePrefs {
            grid_size: 0.0,
            ..Default::default()
        };
        assert!(prefs.validate().iter().any(|e| e.contains("grid_size")));
        prefs.grid_size = f32::NAN;
        assert!(prefs.validate().iter().any(|e| e.contains("grid_size")));
        prefs.grid_size = -1.0;
        assert!(prefs.validate().iter().any(|e| e.contains("grid_size")));
    }

    #[test]
    fn validate_stage_prefs_output_name_empty() {
        let mut prefs = StagePrefs::default();
        prefs
            .outputs
            .push(crate::scene::OutputConfig::default_windowed());
        let errors = prefs.validate();
        assert!(errors.iter().any(|e| e.contains("name is empty")));
    }

    #[test]
    fn validate_stage_prefs_warp_corners_non_finite() {
        // Warp is per-surface now — a non-finite corner on a surface's warp
        // must be reported by validation.
        let mut prefs = StagePrefs::default();
        let uuid = prefs
            .surfaces
            .add_surface("s".into(), crate::renderer::context::OutputSource::Master);
        if let Some((_, s)) = prefs.surfaces.find_by_uuid_mut(&uuid) {
            s.warp = Some(crate::renderer::warp::WarpMode::CornerPin {
                corners: [[0.0, 0.0], [1.0, 0.0], [f32::INFINITY, 1.0], [0.0, 1.0]],
            });
        }
        let errors = prefs.validate();
        assert!(errors.iter().any(|e| e.contains("warp corner")));
    }

    #[test]
    fn workspace_shaders_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = Workspace::new(tmp.path().to_path_buf());
        let shaders_dir = ws.shaders_dir();
        assert_eq!(shaders_dir, tmp.path().join(".varda").join("shaders"));
    }

    #[test]
    fn output_audio_device_survives_target_config_roundtrip() {
        // audio_device must round-trip live OutputTarget ↔ persisted config.
        let recording = OutputTarget::Recording {
            path: "set.mov".into(),
            codec: RecordingCodec::ProRes,
            audio_device: Some("Scarlett 2i2".into()),
        };
        let back = config_to_target(&target_to_config(&recording));
        assert_eq!(back.audio_device(), Some("Scarlett 2i2"));

        // None (video-only) round-trips as None.
        let silent = OutputTarget::RtmpStream {
            url: "rtmp://x".into(),
            codec: crate::renderer::context::StreamingCodec::H264,
            codec_contract: crate::renderer::context::RtmpCodecContract::Legacy,
            audio_device: None,
        };
        assert_eq!(
            config_to_target(&target_to_config(&silent)).audio_device(),
            None
        );
    }

    /// A Blackmagic output is kept with the show: its device and format, by
    /// name, as `"type": "deck_link"`.
    #[test]
    fn a_blackmagic_output_survives_target_config_roundtrip() {
        let target = OutputTarget::DeckLink {
            device: "UltraStudio Monitor 3G".into(),
            mode: "1080p60".into(),
            full_range: false,
        };
        let json = serde_json::to_string(&target_to_config(&target)).unwrap();
        assert_eq!(
            json,
            r#"{"type":"deck_link","device":"UltraStudio Monitor 3G","mode":"1080p60"}"#
        );
        let back: OutputTargetConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config_to_target(&back), target);
        // At full levels, said so.
        let full = OutputTarget::DeckLink {
            device: "UltraStudio Monitor 3G".into(),
            mode: "1080p60".into(),
            full_range: true,
        };
        let json = serde_json::to_string(&target_to_config(&full)).unwrap();
        assert!(json.contains(r#""full_range":true"#), "{json}");
        let back: OutputTargetConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config_to_target(&back), full);
    }

    /// A Blackmagic output saves whether the operator left it running — to
    /// be started again as the stage opens; no other output does, and a
    /// stopped one writes nothing of it.
    #[test]
    fn a_running_blackmagic_output_is_saved_running() {
        use crate::renderer::context::{HeadlessOutput, OutputSource, UnifiedOutput};
        let gpu = headless_gpu();
        let make = |target, kept| {
            let mut output = HeadlessOutput::new(
                &gpu.device,
                "Box".into(),
                OutputSource::Master,
                target,
                64,
                36,
            );
            output.keep_running = kept;
            UnifiedOutput::Headless(output)
        };
        let blackmagic = || OutputTarget::DeckLink {
            device: "UltraStudio Monitor 3G".into(),
            mode: "1080p60".into(),
            full_range: false,
        };
        let running = output_config(&make(blackmagic(), true));
        assert!(running.running);
        let json = serde_json::to_value(&running).unwrap();
        assert_eq!(json["running"], true);
        let stopped = output_config(&make(blackmagic(), false));
        assert!(!stopped.running);
        assert!(serde_json::to_value(&stopped).unwrap().get("running").is_none());
        let ndi = output_config(&make(
            OutputTarget::NdiSend {
                sender_name: "Nida NDI".into(),
            },
            true,
        ));
        assert!(!ndi.running, "only a Blackmagic output comes back on by itself");
    }

    /// A screen in video levels is saved so — `"video_levels": true` beside
    /// its precision — and opens so; one at full levels writes nothing.
    #[test]
    fn a_screen_in_video_levels_is_saved_so() {
        let mut screen = crate::scene::OutputConfig::default_windowed();
        let full = serde_json::to_value(&screen).unwrap();
        assert!(full.get("video_levels").is_none(), "{full}");
        screen.presentation.video_levels = true;
        let video = serde_json::to_value(&screen).unwrap();
        assert_eq!(video["video_levels"], true);
        let back: crate::scene::OutputConfig = serde_json::from_value(video).unwrap();
        assert!(back.presentation.video_levels);
        assert!(back.presentation.dither, "the rest as it was");
    }

    #[test]
    fn enhanced_rtmp_contract_survives_target_config_roundtrip() {
        let target = OutputTarget::RtmpStream {
            url: "rtmps://example/live".into(),
            codec: crate::renderer::context::StreamingCodec::H265,
            codec_contract: crate::renderer::context::RtmpCodecContract::Enhanced,
            audio_device: None,
        };
        let restored = config_to_target(&target_to_config(&target));
        assert!(matches!(
            restored,
            OutputTarget::RtmpStream {
                codec_contract: crate::renderer::context::RtmpCodecContract::Enhanced,
                ..
            }
        ));
    }
}
