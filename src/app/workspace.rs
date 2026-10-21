//! Workspace persistence — save/load from `.varda/` directory.

use super::VardaApp;
use crate::usecases::ui::UILayoutState;

/// Outcome of loading `.varda/`. Hard failures are listed so the command
/// bus cannot report `Ok` when a file that exists could not be restored.
/// Layout is still returned when stage.json loaded, even if scene.json failed.
pub struct WorkspaceLoad {
    pub layout: Option<UILayoutState>,
    errors: Vec<String>,
}

impl WorkspaceLoad {
    /// Whether every existing workspace file loaded.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }

    /// Joined hard-failure messages, if any.
    #[must_use]
    pub fn error_message(&self) -> Option<String> {
        if self.errors.is_empty() {
            None
        } else {
            Some(format!(
                "Failed to load workspace: {}",
                self.errors.join("; ")
            ))
        }
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

/// An answer for `reply` once a save has landed: what did not, or that all
/// did — an HTTP request waits for its files, the engine does not.
pub(crate) fn answer_when_landed(reply: super::Reply) -> crate::persistence::writer::Then {
    Box::new(move |landed| {
        let _ = reply.send(match landed.error_message() {
            None => crate::engine::CommandResult::Ok,
            Some(message) => crate::engine::CommandResult::Err {
                code: crate::engine::ErrorCode::InternalError,
                message,
            },
        });
    })
}

impl VardaApp {
    /// The UI's layout as it is now, for a save that arrives without one —
    /// the API's `SaveWorkspace` — so it does not write back the panels as
    /// they were at the last load.
    pub fn remember_layout(&mut self, layout: &UILayoutState) {
        self.session.last_layout = layout.clone();
    }

    /// Save the entire workspace to `.varda/` — now, on this thread, after
    /// any save still on its way ([`VardaApp::save_in_background`]). For
    /// what must find the files written: Nida quitting, a show opened, made
    /// or moved, tests. A save made while the show is on air goes in the
    /// background, so the outputs never wait for the disk.
    /// `layout` is UI-consumer-owned state persisted in stage.json.
    ///
    /// # Errors
    ///
    /// Returns an error if `.varda/` cannot be created or if any workspace
    /// file fails to write. Files that succeeded stay on disk; the error
    /// lists every failure. The operator is toasted on failure.
    pub fn save_workspace(&mut self, layout: &UILayoutState) -> anyhow::Result<()> {
        self.flush_saves();
        let save = self.show_save(layout, false, None);
        match crate::persistence::writer::write(save).error_message() {
            None => Ok(()),
            Some(msg) => {
                self.session.notifications.error(msg.clone());
                Err(anyhow::anyhow!("{msg}"))
            }
        }
    }

    /// Save the show without the outputs waiting for the disk: the show is
    /// turned into text here, in a millisecond, and written on a thread of
    /// its own after any save before it (see `persistence::writer`). That
    /// it is saved is said once the files have landed — or what did not.
    pub fn save_in_background(&mut self, layout: &UILayoutState) {
        let save = self.show_save(layout, true, None);
        self.show_writer.submit(save);
    }

    /// `EngineCommand::SaveWorkspace`: saved in the background, with the
    /// layout the engine saw last — the command carries none, and defaults
    /// must not be written over the operator's panels. `reply` is answered
    /// once the files have landed: an HTTP request waits for its save (a
    /// backup taken after it copies what was saved), the engine does not.
    pub(crate) fn save_for_command(&mut self, reply: Option<super::Reply>) {
        let layout = self.session.last_layout.clone();
        let then = reply.map(answer_when_landed);
        let save = self.show_save(&layout, false, then);
        self.show_writer.submit(save);
    }

    /// Write a file of a command's — a preset, a screen's name — after the
    /// saves before it; `reply` is answered once it has landed, and the
    /// operator told then.
    pub(crate) fn write_for_command(
        &mut self,
        mut file: crate::persistence::writer::FileSave,
        reply: Option<super::Reply>,
    ) -> u64 {
        file.then = reply.map(answer_when_landed);
        self.show_writer.submit_file(file)
    }

    /// The next `files` files of their own handed to the writer are not
    /// written, as a disk refusing them would leave them — for tests.
    #[cfg(test)]
    pub(crate) fn refuse_files_for_test(&self, files: u32) {
        self.show_writer.refuse_files_for_test(files);
    }

    /// Writing the next `files` files of their own handed to the writer
    /// panics — for tests.
    #[cfg(test)]
    pub(crate) fn break_files_for_test(&self, files: u32) {
        self.show_writer.break_files_for_test(files);
    }

    /// Saves made in the background from now wait `wait` before they are
    /// written, as on the laptop whose disk was nearly full — for tests.
    #[doc(hidden)]
    pub fn slow_saves_for_test(&mut self, wait: std::time::Duration) {
        self.show_writer.slow_for_test(wait);
    }

    /// Wait for every save on its way to land, and say what came of it.
    pub fn flush_saves(&mut self) {
        self.show_writer.flush();
        self.report_saves();
    }

    /// Say what came of the saves that have landed since the last time —
    /// and move with a Save As that has (see `VardaApp::save_show_as`).
    pub(crate) fn report_saves(&mut self) {
        for mut landed in self.show_writer.landed() {
            if self.saving_as.as_ref().is_some_and(|s| s.is(landed.id)) {
                self.saved_as(&landed);
                continue;
            }
            self.preset_landed(&mut landed);
            match landed.error_message() {
                Some(msg) => self.session.notifications.error(msg),
                None => {
                    if let Some(said) = landed.said {
                        self.session.notifications.info(said);
                    }
                }
            }
        }
    }

    /// The show as a save writes it: each file's text, made here.
    pub(crate) fn show_save(
        &mut self,
        layout: &UILayoutState,
        announce: bool,
        then: Option<crate::persistence::writer::Then>,
    ) -> crate::persistence::writer::ShowSave {
        let into = crate::persistence::Workspace::new(self.show_folder().to_path_buf());
        self.show_save_into(&into, layout, announce, then)
    }

    /// The show as a save into the show folder `into` writes it — its own,
    /// or the one a Save As goes to.
    pub(crate) fn show_save_into(
        &mut self,
        into: &crate::persistence::Workspace,
        layout: &UILayoutState,
        announce: bool,
        then: Option<crate::persistence::writer::Then>,
    ) -> crate::persistence::writer::ShowSave {
        use crate::persistence::writer::{ShowFile, ShowSave};
        self.session.last_layout = layout.clone();
        let text = |made: anyhow::Result<String>| made.map_err(|e| e.to_string());
        let mut files = Vec::new();

        let mut scene = crate::persistence::snapshot_scene(
            &self.mixer,
            Some(&self.transport_config()),
            self.render_width,
            self.render_height,
        );
        scene.render_resolution_auto = self.render_auto;
        scene.frames_in_flight = (self.frames_in_flight != 1).then_some(self.frames_in_flight);
        scene.free_control_screen = !self.control_screen_rate();
        files.push(ShowFile {
            key: "scene",
            label: "scene",
            path: into.scene_path(),
            content: text(scene.to_json()),
        });

        if let Some(midi) = &self.input.midi_devices {
            let midi_config = self
                .input
                .midi_mappings
                .to_config(&midi.devices, &self.input.auto_map_engine);
            files.push(ShowFile {
                key: "midi",
                label: "MIDI mappings",
                path: into.midi_path(),
                content: text(midi_config.to_json()),
            });
        }

        let mut stage = crate::persistence::snapshot_stage(
            &self.output.surface_manager,
            &self.output.outputs,
            &self.production_buses,
            layout.stage_editor_grid_size,
            layout.stage_editor_snap,
            layout.library_panel_open,
            layout.right_panel_open,
            layout.stage_editor_open,
            layout.dome_preview_open,
            layout.dome_mode_active,
            layout.dome_preset,
            layout.dome_geometry,
            self.output.domemaster_resolution,
        );
        stage.timecode = self.timecode_config();
        stage.rundown = self.rundown_config();
        stage.audio_mix = Some(self.audio_mix.config().clone());
        stage.stream = Some(self.stream_settings().clone());
        stage.record = Some(self.record_settings().clone());
        stage.screen_layout = serde_json::to_value(&layout.screen).ok();
        stage
            .collapsed_channels
            .clone_from(&layout.collapsed_channels);
        files.push(ShowFile {
            key: "stage",
            label: "stage",
            path: into.stage_path(),
            content: text(stage.to_json()),
        });

        files.push(ShowFile {
            key: "keymap",
            label: "keymap",
            path: into.keymap_path(),
            content: text(self.input.keymap.to_config().to_json()),
        });
        files.push(ShowFile {
            key: "osc",
            label: "OSC config",
            path: into.osc_path(),
            content: text(self.input.osc_config.to_json()),
        });

        ShowSave {
            dir: into.varda_dir(),
            files,
            show: super::shows::name_of(into.root()),
            announce,
            then,
        }
    }

    /// Load workspace from `.varda/` if it exists.
    /// If a scene is found, replaces the default mixer with the restored one.
    /// Returns layout preferences loaded from stage.json (if any), plus any
    /// hard failures so the command bus cannot report success on a partial load.
    pub fn load_workspace(&mut self) -> WorkspaceLoad {
        // What is being saved lands first: a load reads the files back.
        self.flush_saves();
        // A rundown step being waited on belongs to the show before: it must
        // not be taken into this one — whatever becomes of the load, a show
        // that is not there or opens only in part included.
        self.rundown.hold();
        if !self.session.workspace.exists() {
            log::info!("No .varda/ directory found, starting fresh");
            return WorkspaceLoad {
                layout: None,
                errors: Vec::new(),
            };
        }
        // Loading a workspace replaces the live scene/stage, so the undo/redo
        // timeline (which references the previous state) must be cleared.
        self.session.history.clear();
        let mut loaded_layout: Option<UILayoutState> = None;
        let mut errors: Vec<String> = Vec::new();
        // Whether the bus bank now in place came from this load. Buses live in
        // stage.json, so a missing or unparseable stage leaves the *previous*
        // bank live while the scene may still be replaced underneath it.
        let mut buses_are_from_this_load = false;
        // Whether that bank is the lone PGM made up for a stage without buses
        // (a new show's, or one from before buses), which must cover the
        // whole master the scene below sets.
        let mut buses_are_made_up = false;
        if self.session.workspace.has_stage() {
            match crate::persistence::StagePrefs::load(self.session.workspace.stage_path()) {
                Ok(prefs) => {
                    loaded_layout = Some(UILayoutState {
                        stage_editor_grid_size: prefs.grid_size,
                        stage_editor_snap: prefs.snap,
                        library_panel_open: prefs.library_panel_open,
                        right_panel_open: prefs.right_panel_open,
                        stage_editor_open: prefs.stage_editor_open,
                        dome_preview_open: prefs.dome_preview_open,
                        dome_mode_active: prefs.dome_mode_active,
                        dome_preset: prefs.dome_preset,
                        dome_geometry: prefs.dome_geometry,
                        screen: prefs
                            .screen_layout
                            .clone()
                            .and_then(|value| {
                                serde_json::from_value::<
                                    crate::usecases::ui::screen_layout::ScreenLayout,
                                >(value)
                                .inspect_err(|e| {
                                    log::warn!("Screen layout unreadable, opening Classic: {e}");
                                })
                                .ok()
                            })
                            .map(crate::usecases::ui::screen_layout::ScreenLayout::sanitized)
                            .unwrap_or_default(),
                        collapsed_channels: prefs.collapsed_channels.clone(),
                        ..UILayoutState::default()
                    });
                    self.apply_timecode_config(&prefs.timecode);
                    self.audio_mix.load(
                        prefs
                            .audio_mix
                            .clone()
                            .unwrap_or_else(crate::audio_mix::AudioMixConfig::with_master),
                    );
                    self.load_stream_settings(prefs.stream.clone());
                    self.load_record_settings(prefs.record.clone());
                    // Buses are restored here but reconciled against the scene
                    // further down, since the stage loads before the scene and
                    // their Preview/Program references name scene sources.
                    // `None` means the field was absent (a stage file written
                    // before buses existed). Assign the legacy default rather
                    // than leaving whatever happens to be live: on a second
                    // load into the same process, "leave it alone" would keep
                    // buses the loaded stage never described.
                    buses_are_from_this_load = true;
                    self.production_buses = if let Some(bus_configs) = prefs.buses {
                        let (bank, rejected) =
                            crate::bus::BusBank::from_configs_reporting(bus_configs);
                        for reason in rejected {
                            log::error!("Rejected bus on load: {reason}");
                            errors.push(format!("stage: {reason}"));
                        }
                        bank
                    } else {
                        buses_are_made_up = true;
                        crate::bus::BusBank::with_legacy_default()
                    };
                    // With the buses its steps set; what they name of the
                    // scene is checked when a step is taken.
                    self.rundown = prefs
                        .rundown
                        .map(crate::rundown::Rundown::from_config)
                        .unwrap_or_default();
                    self.output.surface_manager = prefs.surfaces;
                    // Set before `ensure_domemaster` below, which builds at
                    // whatever this says.
                    self.output.domemaster_resolution = prefs.domemaster_resolution;
                    let listed: Vec<String> =
                        prefs.outputs.iter().map(|c| c.uuid.clone()).collect();
                    self.close_outputs_not_in(&listed);
                    for (index, output_config) in prefs.outputs.iter().enumerate() {
                        // A screen already open — a show opened while running,
                        // made from this one — stays as it is, not opened twice;
                        // and one written twice in the stage is opened once.
                        if self
                            .output
                            .outputs
                            .iter()
                            .any(|o| o.uuid() == output_config.uuid)
                            || prefs.outputs[..index]
                                .iter()
                                .any(|earlier| earlier.uuid == output_config.uuid)
                        {
                            continue;
                        }
                        // Migrate legacy target_display field to new target config
                        let mut config = output_config.clone();
                        if matches!(config.target, crate::scene::OutputTargetConfig::Windowed) {
                            if let Some(ref display_name) = config.target_display {
                                config.target = crate::scene::OutputTargetConfig::Display {
                                    name: display_name.clone(),
                                };
                            }
                        }
                        self.output.pending_output_creates.push(config);
                    }
                    log::info!(
                        "Loaded stage with {} surfaces, {} outputs",
                        self.output.surface_manager.surfaces.len(),
                        prefs.outputs.len()
                    );

                    // If any surface uses Domemaster source, ensure the renderer exists
                    let has_dome_surfaces = self.output.surface_manager.surfaces.iter().any(|s| {
                        matches!(s.source, crate::renderer::context::OutputSource::Domemaster)
                    });
                    if has_dome_surfaces {
                        self.ensure_domemaster();
                    }
                }
                Err(e) => {
                    log::warn!("Failed to load stage: {e}");
                    errors.push(format!("stage: {e}"));
                }
            }
        }
        // Whether the scene now live came from this load.
        //
        // Starts false even when there is no `scene.json`: an absent scene means
        // the mixer still holds whatever was there before, which is exactly the
        // provenance bus routing must not trust. Treating "no scene file" as
        // "the scene is current" let a stage-only load resolve its routes
        // against a stale mixer — the mirror of the stale-bank case below.
        let mut scene_is_the_requested_one = false;
        if self.session.workspace.has_scene() {
            match crate::scene::SceneConfig::load(self.session.workspace.scene_path()) {
                Ok(scene_config) => {
                    // Apply render resolution from scene if present
                    if let (Some(w), Some(h)) =
                        (scene_config.render_width, scene_config.render_height)
                    {
                        if w > 0 && h > 0 {
                            self.render_width = w;
                            self.render_height = h;
                            log::info!("Scene render resolution: {w}×{h}");
                        }
                    }
                    self.set_render_auto(scene_config.render_resolution_auto);
                    self.set_frames_in_flight(scene_config.frames_in_flight.unwrap_or(1));
                    self.set_control_screen_rate(!scene_config.free_control_screen);
                    match self.replace_scene(&scene_config) {
                        Ok(()) => {
                            scene_is_the_requested_one = true;
                            log::info!(
                                "Loaded scene with {} channels",
                                scene_config.channels.len()
                            );
                        }
                        Err(e) => {
                            log::error!("Failed to restore scene: {e}");
                            errors.push(format!("scene: {e}"));
                        }
                    }
                }
                Err(e) => {
                    log::warn!("Failed to load scene file: {e}");
                    errors.push(format!("scene: {e}"));
                }
            }
        }
        // The made-up PGM is the single program buses replaced, so it shows
        // all of Main: at 1080p in a 4K show it would show a quarter of it.
        if buses_are_made_up {
            self.production_buses.rescale_canvases(
                (crate::bus::DEFAULT_BUS_WIDTH, crate::bus::DEFAULT_BUS_HEIGHT),
                (self.render_width, self.render_height),
            );
        }
        // Both stage and scene are settled by this point, so bus routing can be
        // checked against the sources that actually exist. A reference the scene
        // does not contain falls back to black: an unresolvable source must
        // never resolve to a different one, because that would put unintended
        // content on air.
        //
        // When the requested scene failed to load, the mixer still holds the
        // *previous* scene. Validating against it would let a route resolve to
        // whatever happens to share that UUID there, so routing fails closed
        // instead: nothing resolves, every bus shows black, and the operator is
        // told why. The configured routes are retained, so loading the intended
        // scene restores them.
        {
            // Routing may only resolve when *both* halves came from this load.
            //
            // A stale bank (stage missing or unparseable) validated against a
            // freshly loaded scene is the more dangerous direction: a retained
            // route naming a UUID that also exists in the new scene would
            // resolve to different content and put it on air. Either half being
            // wrong fails routing closed.
            if scene_is_the_requested_one && buses_are_from_this_load {
                // Provenance established: this is the only place allowed to
                // trust the live mixer, and the only thing that lifts a
                // quarantine set by an earlier partial load.
                // A route to a source the show no longer has is shown black
                // and reported — as a warning: the rest of the show loaded.
                let known = self.known_source_uuids();
                let dropped = self.production_buses.reconcile_trusted(&known);
                for route in &dropped {
                    log::warn!("Bus routing does not resolve on load: {route}");
                }
                if !dropped.is_empty() {
                    self.session.notifications.warn(format!(
                        "Shown black — its source is not in the show: {}",
                        dropped.join("; ")
                    ));
                }
            } else {
                // One half of the workspace is not from this load, so nothing
                // can vouch for the other. Quarantine rather than merely
                // marking unresolved: a later reconciliation — a deck removal,
                // an undo — recomputes from whatever mixer is live and would
                // otherwise re-arm a stale route.
                let why = if buses_are_from_this_load {
                    "the scene it refers to did not load"
                } else {
                    "its stage did not load, so these routes are from a previous workspace"
                };
                errors.push(format!("bus routing: held black because {why}"));
                self.production_buses.quarantine_routing();
                for dropped in self
                    .production_buses
                    .reconcile(&crate::bus::KnownSources::default())
                {
                    log::warn!("Bus routing quarantined on load: {dropped}");
                }
            }
        }
        if self.session.workspace.has_midi() {
            match crate::midi::MidiConfig::load(self.session.workspace.midi_path()) {
                Ok(midi_config) => {
                    if let Some(midi) = &self.input.midi_devices {
                        self.input
                            .midi_mappings
                            .load_from_config(&midi_config, &midi.devices);
                        log::info!("Loaded {} MIDI mappings", midi_config.mappings.len());
                    } else {
                        log::info!(
                            "MIDI config found but no MIDI devices connected, mappings deferred"
                        );
                    }
                }
                Err(e) => {
                    log::warn!("Failed to load MIDI config: {e}");
                    errors.push(format!("midi: {e}"));
                }
            }
        }
        // Load keyboard shortcuts
        if self.session.workspace.has_keymap() {
            match crate::keymap::KeymapConfig::load(self.session.workspace.keymap_path()) {
                Ok(keymap_config) => {
                    self.input.keymap.load_config(&keymap_config);
                    log::info!("Loaded {} keyboard shortcuts", keymap_config.bindings.len());
                }
                Err(e) => {
                    log::warn!("Failed to load keymap config: {e}");
                    errors.push(format!("keymap: {e}"));
                }
            }
        }
        // Load OSC config (already loaded in new(), but refresh feedback targets on workspace load)
        if self.session.workspace.has_osc() {
            match crate::osc::OscConfig::load(self.session.workspace.osc_path()) {
                Ok(config) => {
                    // Update feedback targets
                    if let Some(ref mut sender) = self.input.osc_feedback {
                        for target in &config.feedback_targets {
                            if let Err(e) = sender.add_target(target) {
                                log::warn!("Failed to add OSC feedback target '{target}': {e}");
                            }
                        }
                    }
                    self.input.osc_config = config;
                    log::info!(
                        "Loaded OSC config: port={}, enabled={}, {} feedback target(s)",
                        self.input.osc_config.in_port,
                        self.input.osc_config.enabled,
                        self.input.osc_config.feedback_targets.len()
                    );
                }
                Err(e) => {
                    log::warn!("Failed to load OSC config: {e}");
                    errors.push(format!("osc: {e}"));
                }
            }
        }
        if let Some(layout) = &loaded_layout {
            self.session.last_layout = layout.clone();
        }
        if !errors.is_empty() {
            let msg = format!("Failed to load workspace: {}", errors.join("; "));
            self.session.notifications.error(msg);
        }
        WorkspaceLoad {
            layout: loaded_layout,
            errors,
        }
    }

    /// Put `scene_config` in place of the scene now live: its channels,
    /// layers, effects and Main, with the devices and receivers the outgoing
    /// scene alone held let go.
    ///
    /// # Errors
    ///
    /// Returns an error if the scene cannot be restored; the live scene is
    /// then left as it was.
    pub(crate) fn replace_scene(
        &mut self,
        scene_config: &crate::scene::SceneConfig,
    ) -> anyhow::Result<()> {
        let result = crate::persistence::restore_scene(
            scene_config,
            &self.context,
            &self.registry,
            &mut self.camera_manager,
            &mut self.screen_capture_manager,
            &mut self.depth_manager,
            &mut self.external_io.ndi_manager,
            &mut self.external_io.stream_manager,
            &mut self.external_io.html_manager,
            self.render_width,
            self.render_height,
        )?;
        // Retire the outgoing mixer's external sessions.
        //
        // `restore_scene` acquires through the same
        // managers, so a reused camera or depth sensor has
        // its reference count incremented for the new deck
        // while the old holder never decrements — after
        // which removing the new deck cannot close the
        // device. Old-only NDI, Syphon and stream receivers
        // would simply keep running.
        //
        // Reconciled against the incoming mixer, so a
        // stream receiver shared by URL across the reload is
        // not stopped out from under the new decks.
        let outgoing: Vec<super::HeldResources> = self
            .mixer
            .channels()
            .iter()
            .flat_map(|ch| ch.decks.iter())
            .map(|slot| super::HeldResources::of(&slot.deck))
            .collect();
        let incoming: Vec<super::HeldResources> = result
            .mixer
            .channels()
            .iter()
            .flat_map(|ch| ch.decks.iter())
            .map(|slot| super::HeldResources::of(&slot.deck))
            .collect();
        let still_held = super::HeldResources::union(&incoming);
        let releasing: Vec<super::HeldResources> =
            outgoing.into_iter().map(|h| h.minus(&still_held)).collect();

        self.mixer = result.mixer;
        for h in releasing {
            self.release_held_resources(h);
        }
        // How the show counts frames and where it loops are
        // authored; where it was stopped is not, so the
        // position stays at zero and the arrangement stays
        // inert until someone starts it.
        self.transport
            .set_timecode_rate(scene_config.transport.timecode_rate);
        self.transport
            .set_loop_region(scene_config.transport.loop_region);
        for warn in &result.warnings {
            self.session.notifications.warn(warn.clone());
        }
        // Syphon decks that could not resolve at restore time
        // (producer not publishing yet) — the render thread
        // auto-binds them as their servers appear.
        #[cfg(target_os = "macos")]
        {
            if !result.pending_syphon.is_empty() {
                log::info!(
                    "{} Syphon deck(s) deferred to late-bind",
                    result.pending_syphon.len()
                );
            }
            self.external_io.pending_syphon = result.pending_syphon;
        }

        // Start preprocessor analyzers for active decks restored from save.
        // A deck is "active" when it is not muted and has non-zero opacity.
        for ch in self.mixer.channels_mut() {
            let any_solo = ch.decks.iter().any(|s| s.solo);
            for slot in &mut ch.decks {
                let active = !slot.mute && (!any_solo || slot.solo) && slot.opacity > 0.0;
                if active {
                    slot.deck
                        .ensure_preprocessor_analyzers(&self.analyzer_registry);
                }
            }
        }

        Ok(())
    }

    /// Apply a scene diff: compare current mixer state to a target `SceneConfig`
    /// and patch only what changed. Returns (warnings, `structural_changed`).
    /// `structural_changed` is true when channels/decks were added/removed/rebuilt
    /// (requiring texture re-registration).
    /// Named for its one caller: this is the *history* restore path, not a
    /// general "apply this scene". That distinction is load-bearing — live
    /// control (crossfader position, macro knob values) is deliberately not
    /// restored here, because undoing an unrelated edit must not change what is
    /// on air. A full workspace load goes through `restore_scene`, which does
    /// restore the persisted values.
    ///
    /// # Panics
    ///
    /// Panics if a pooled entity vanishes between the reuse check and the
    /// removal that follows it, which cannot happen: the pool is local and not
    /// mutated in between.
    pub fn apply_history_scene(
        &mut self,
        target: &crate::scene::SceneConfig,
        rw: u32,
        rh: u32,
    ) -> (Vec<String>, bool) {
        let mut warnings = Vec::new();
        let mut structural = false;

        // Preflight before *any* mutation.
        //
        // The merge below reconciles by identity, so it cannot run against a
        // target whose identities are unusable. Checking later would leave the
        // crossfader, modulation, macros and arrangement already changed while
        // reporting that nothing structural happened — a half-applied restore
        // the history bookkeeping has no way to represent.
        let identity_errors = target.validate_identities();
        if !identity_errors.is_empty() {
            for e in &identity_errors {
                warnings.push(format!("refusing to restore: {e}"));
            }
            return (warnings, structural);
        }

        // (a) Crossfader — deliberately NOT restored.
        //
        // `SetCrossfader` is denied a history entry as live control, but that
        // only stops it *creating* one: every snapshot still records the
        // position, so restoring it here would rewind the live fader when an
        // unrelated edit is undone. Same reasoning as the transport position
        // below, and as production buses, which keep PVW/PGM across a
        // structural restore.

        // (b) Modulation — cheap clone
        self.mixer.set_modulation(target.modulation.clone());

        // (b2) Macros — restored whole, control and targets together.
        //
        // Preserving only the knob position was tried and reverted. It kept the
        // control where the operator left it while the targets it drove were
        // rewritten from the snapshot, so the control read 0.25 while the
        // picture showed 1.0 — and the operator's next nudge would then snap
        // the picture, because a turn fans the preserved value out immediately.
        // A half-preserved macro is worse than a fully rewound one.
        //
        // Rewinding the whole bank retains the pre-existing defect that undo
        // rewinds live macro output, but keeps control and picture agreeing.
        // Fixing it properly needs last-writer authority per target, or a
        // live-override layer the restore does not write through, plus a
        // parameter read-back that does not exist yet. The crossfader is
        // different and stays preserved: it is one well-known value, not an
        // arbitrary target set. See docs/conference/known-upstream-defects.md.
        self.mixer.set_macros(target.macros.clone());

        // (b3) Arrangement — cheap clone. Regions and lanes are edited through
        // the undo stack like any other scene data; the transport's *position*
        // is not, since undoing an edit must not also rewind the show.
        self.mixer.set_arrangement(target.arrangement.clone());
        self.transport
            .set_timecode_rate(target.transport.timecode_rate);
        self.transport.set_loop_region(target.transport.loop_region);

        // (c) Transition shader — compare names, only recreate if changed
        {
            let current_name = self.mixer.active_transition().map(|t| t.name.clone());
            let target_name = target.active_transition.as_deref();
            if current_name.as_deref() != target_name {
                match target_name {
                    Some(name) => {
                        if let Some(shader) = self
                            .registry
                            .transitions()
                            .iter()
                            .find(|s| s.name() == name)
                        {
                            if let Err(e) =
                                self.mixer.set_transition(&self.context, (*shader).clone())
                            {
                                warnings
                                    .push(format!("Failed to restore transition '{name}': {e}"));
                            }
                        } else {
                            warnings.push(format!("Transition '{name}' not found in registry"));
                        }
                    }
                    None => self.mixer.clear_transition(),
                }
            }
        }

        // (d) Channels and decks — reconcile by identity, in target order.
        //
        // Restoring positionally would *transplant* an identity onto a
        // different live runtime: for channels `A,B,C`, removing `B` and
        // undoing pairs the surviving `C` with target `B`, repurposes it, and
        // constructs a replacement `C`. Everything the snapshot does not carry
        // — video decode position above all — then belongs to the wrong UUID,
        // and a bus, MIDI mapping or API caller addressing `C` silently gets
        // different content. Reordering alone cannot fix it, because a
        // survivor may need to move past a gap the removed entity left.
        //
        // So: harvest every live runtime into identity pools, then build the
        // target list by taking each UUID's own runtime, constructing only
        // what has no live counterpart. A survivor never needs `set_uuid` —
        // it was chosen *because* it already carries that identity.
        //
        // The deck pool spans every channel, because `move_deck` transfers the
        // real `DeckSlot` between channels; a per-channel pool would fail to
        // find a moved deck and reconstruct it while the destination discarded
        // the original.
        let mut channel_pool: std::collections::HashMap<String, crate::channel::Channel> = self
            .mixer
            .channels_mut()
            .drain(..)
            .map(|ch| (ch.uuid().to_string(), ch))
            .collect();
        let mut deck_pool: std::collections::HashMap<String, crate::channel::DeckSlot> =
            channel_pool
                .values_mut()
                .flat_map(|ch| ch.decks.drain(..))
                .map(|slot| (slot.deck.uuid().to_string(), slot))
                .collect();

        // Runtimes displaced by a successful rebuild. They are retired with the
        // rest, so a replaced camera or receiver is not left held.
        let mut replaced: Vec<crate::channel::DeckSlot> = Vec::new();
        let mut rebuilt: Vec<crate::channel::Channel> = Vec::with_capacity(target.channels.len());
        for ch_config in &target.channels {
            let mut ch = if let Some(existing) = channel_pool.remove(&ch_config.uuid) {
                existing
            } else {
                {
                    structural = true;
                    match crate::channel::Channel::new(
                        ch_config.name.clone(),
                        &self.context,
                        rw,
                        rh,
                    ) {
                        Ok(mut fresh) => {
                            // `Channel::new` mints a throwaway UUID; the
                            // restore owns the identity.
                            fresh.set_uuid(ch_config.uuid.clone());
                            fresh
                        }
                        Err(e) => {
                            warnings.push(format!(
                                "Failed to restore channel '{}': {e}",
                                ch_config.name
                            ));
                            continue;
                        }
                    }
                }
            };

            ch.name.clone_from(&ch_config.name);
            ch.opacity = ch_config.opacity;
            ch.blend_mode = ch_config.blend_mode.into();
            ch.overlay = ch_config.overlay;

            for deck_config in &ch_config.decks {
                // A survivor whose source configuration still matches keeps its
                // runtime; one whose source changed has to be rebuilt even
                // though the identity is the same.
                let reusable = deck_pool.get(&deck_config.uuid).is_some_and(|slot| {
                    crate::persistence::source_configs_match(&slot.deck, &deck_config.source)
                });
                let existing = deck_pool.remove(&deck_config.uuid);
                let mut slot = if reusable {
                    existing.expect("checked present above")
                } else {
                    structural = true;
                    match crate::persistence::restore_deck(
                        deck_config,
                        &self.context,
                        &self.registry,
                        &mut self.camera_manager,
                        &mut self.screen_capture_manager,
                        &mut self.depth_manager,
                        &mut self.external_io.ndi_manager,
                        &mut self.external_io.stream_manager,
                        &mut self.external_io.html_manager,
                        rw,
                        rh,
                    ) {
                        Ok(deck) => {
                            // Whatever this replaced still has to be retired;
                            // dropping it here would leak its manager sessions.
                            if let Some(old) = existing {
                                replaced.push(old);
                            }
                            crate::channel::DeckSlot::new(deck)
                        }
                        // A failed replacement must not also destroy what was
                        // working: keep the running runtime if there was one,
                        // and drop the deck only when there is nothing to keep.
                        Err(e) => {
                            if let Some(kept) = existing {
                                warnings.push(format!(
                                    "Failed to rebuild deck '{}': {e} — keeping the running one",
                                    deck_config.name
                                ));
                                kept
                            } else {
                                warnings.push(format!(
                                    "Failed to restore deck '{}': {e}",
                                    deck_config.name
                                ));
                                continue;
                            }
                        }
                    }
                };
                Self::patch_deck_slot(&mut slot, deck_config, &self.context, &self.registry);
                ch.decks.push(slot);
            }

            // Channel effect chain. An earlier revision of this merge dropped
            // it, which left a channel-effect add un-undoable, a removal
            // unrestorable, and a freshly constructed channel with no effects.
            Self::diff_effects(
                &mut ch.effects,
                &ch_config.effects,
                &self.context,
                self.context.compositing_format,
                &mut warnings,
            );

            rebuilt.push(ch);
        }

        // Anything left in a pool was removed by the state being restored.
        if !channel_pool.is_empty() || !deck_pool.is_empty() {
            structural = true;
        }
        // Retire what the restore removed.
        //
        // Dropping a `DeckSlot` frees its GPU textures, but camera,
        // depth-sensor, SRT/NDI and Syphon sessions live in managers outside
        // the deck and would otherwise stay held — a capture thread and USB
        // handle outliving the last deck that used them.
        //
        // Release by *holder reconciliation*, not per-deck ownership: managers
        // deliberately share a session between decks that ask for the same
        // thing (`StreamManager::start_receive` returns the existing receiver
        // index for a URL already running). Stopping one unconditionally as a
        // deck retires would freeze an on-air deck still using it. Only a
        // session no surviving deck holds is released. Screen capture is
        // reference counted like cameras, so it is released here too; its
        // per-frame `reconcile_holders` pass repairs any holder this misses.
        let mut retired: Vec<crate::channel::DeckSlot> = deck_pool.into_values().collect();
        retired.extend(
            channel_pool
                .into_values()
                .flat_map(|mut ch| std::mem::take(&mut ch.decks)),
        );
        retired.extend(replaced);

        let survivors: Vec<super::HeldResources> = rebuilt
            .iter()
            .flat_map(|ch| ch.decks.iter())
            .map(|slot| super::HeldResources::of(&slot.deck))
            .collect();
        let still_held = super::HeldResources::union(&survivors);
        let releasing: Vec<super::HeldResources> = retired
            .iter()
            .map(|slot| super::HeldResources::of(&slot.deck).minus(&still_held))
            .collect();
        drop(retired);
        for h in releasing {
            self.release_held_resources(h);
        }

        *self.mixer.channels_mut() = rebuilt;

        // Update next_channel_index
        let max_idx = self
            .mixer
            .channels()
            .iter()
            .filter_map(|ch| {
                ch.name
                    .strip_prefix("Ch ")
                    .and_then(|s| s.parse::<usize>().ok())
            })
            .max()
            .map_or(self.mixer.channels().len(), |n| n + 1);
        self.mixer.set_next_channel_index(max_idx);

        // (e) Master effects — diff
        Self::diff_effects(
            self.mixer.master_effects_mut(),
            &target.master_effects,
            &self.context,
            self.context.compositing_format,
            &mut warnings,
        );

        // (f) Transition sequences — cheap clone
        let channel_uuids: Vec<String> = self
            .mixer
            .channels()
            .iter()
            .map(|ch| ch.uuid().to_string())
            .collect();
        self.mixer.transition_sequences_mut().clear();
        for seq_config in &target.transition_sequences {
            let steps = crate::persistence::restore_sequence_steps(
                &seq_config.steps,
                &channel_uuids,
                &mut warnings,
            );
            self.mixer.transition_sequences_mut().push(
                crate::mixer::TransitionSequence::with_uuid(
                    seq_config.uuid.clone(),
                    seq_config.name.clone(),
                    steps,
                    seq_config.enabled,
                ),
            );
        }

        (warnings, structural)
    }

    /// The persisted half of the transport: how this show counts frames and
    /// where it loops, without its position or run state.
    pub fn transport_config(&self) -> crate::scene::TransportConfig {
        crate::scene::TransportConfig {
            timecode_rate: self.transport.timecode_rate(),
            loop_region: self.transport.loop_region(),
        }
    }

    /// The timecode patch with its devices named, ready to persist.
    pub fn timecode_config(&self) -> crate::timecode::TimecodeConfig {
        self.input.timecode.to_config(
            |id| {
                self.audio_manager
                    .devices()
                    .iter()
                    .find(|d| d.id == id)
                    .map(|d| d.name.clone())
            },
            |id| {
                self.input
                    .midi_devices
                    .as_ref()
                    .and_then(|midi| midi.devices.get(&id))
                    .map(|d| d.name.clone())
            },
        )
    }

    /// Restore a saved timecode patch against the devices present now,
    /// reporting anything the rig no longer has.
    pub fn apply_timecode_config(&mut self, config: &crate::timecode::TimecodeConfig) {
        let audio: Vec<(crate::audio::AudioSourceId, String)> = self
            .audio_manager
            .devices()
            .iter()
            .map(|d| (d.id, d.name.clone()))
            .collect();
        let midi: Vec<(crate::midi::DeviceId, String)> = self
            .input
            .midi_devices
            .as_ref()
            .map(|m| m.devices.values().map(|d| (d.id, d.name.clone())).collect())
            .unwrap_or_default();

        let warnings = self.input.timecode.apply_config(
            config,
            |name| {
                audio
                    .iter()
                    .find(|(_, device)| device == name)
                    .map(|(id, _)| *id)
            },
            |name| {
                midi.iter()
                    .find(|(_, device)| device == name)
                    .map(|(id, _)| *id)
            },
        );
        for warning in warnings {
            self.session.notifications.warn(warning);
        }
    }

    /// Build a combined history snapshot (scene + stage) of current engine
    /// state using neutral/default editor prefs.
    ///
    /// Used by the headless/API undo path (`EngineCommand::Undo`/`Redo`), which
    /// has no UI layout to source cosmetic editor prefs or dome layout flags
    /// from. `apply_stage_diff` ignores those cosmetic fields anyway, so the
    /// defaults are inconsequential to what undo actually restores. The windowed
    /// runner builds its own snapshot with real layout prefs.
    pub fn history_snapshot_default(&self) -> super::history::HistorySnapshot {
        let scene = crate::persistence::snapshot_scene(
            &self.mixer,
            Some(&self.transport_config()),
            self.render_width,
            self.render_height,
        );
        let d = crate::persistence::StagePrefs::default();
        let stage = crate::persistence::snapshot_stage(
            &self.output.surface_manager,
            &self.output.outputs,
            &self.production_buses,
            d.grid_size,
            d.snap,
            d.library_panel_open,
            d.right_panel_open,
            d.stage_editor_open,
            d.dome_preview_open,
            d.dome_mode_active,
            d.dome_preset,
            d.dome_geometry,
            self.output.domemaster_resolution,
        );
        let stage = crate::persistence::StagePrefs {
            rundown: Some(self.rundown.to_config()),
            ..stage
        };
        super::history::HistorySnapshot { scene, stage }
    }

    /// The rundown as the stage file keeps it: nothing, for one never
    /// touched — no steps, the fade time as it comes — so a show that does
    /// not use it is written as before.
    fn rundown_config(&self) -> Option<crate::rundown::RundownConfig> {
        let rundown = &self.rundown;
        let untouched = rundown.steps().is_empty()
            && (rundown.fade_secs() - crate::rundown::DEFAULT_FADE_SECS).abs() < f32::EPSILON;
        (!untouched).then(|| rundown.to_config())
    }

    /// Build a combined history snapshot (scene + stage) of current engine
    /// state, sourcing cosmetic editor prefs and dome layout flags from the UI
    /// `layout`. Used by the windowed runner's undo/redo push and restore.
    pub fn history_snapshot(&self, layout: &UILayoutState) -> super::history::HistorySnapshot {
        let scene = crate::persistence::snapshot_scene(
            &self.mixer,
            Some(&self.transport_config()),
            self.render_width,
            self.render_height,
        );
        let stage = crate::persistence::snapshot_stage(
            &self.output.surface_manager,
            &self.output.outputs,
            &self.production_buses,
            layout.stage_editor_grid_size,
            layout.stage_editor_snap,
            layout.library_panel_open,
            layout.right_panel_open,
            layout.stage_editor_open,
            layout.dome_preview_open,
            layout.dome_mode_active,
            layout.dome_preset,
            layout.dome_geometry,
            self.output.domemaster_resolution,
        );
        let stage = crate::persistence::StagePrefs {
            rundown: Some(self.rundown.to_config()),
            ..stage
        };
        super::history::HistorySnapshot { scene, stage }
    }

    // ── Unified undo/redo timeline ─────────────────────────────────────────
    //
    // The engine owns the single `HistoryManager` (on `SessionState`). Both
    // consumers push into and restore from it through these methods, so the
    // windowed UI and the HTTP/headless command bus share one timeline. See
    // [undo-redo.md](/spec/undo-redo.md) → "Recording Points".

    /// True if there is an undoable action on the shared timeline.
    pub fn history_can_undo(&self) -> bool {
        self.session.history.can_undo()
    }

    /// True if there is a redoable action on the shared timeline.
    pub fn history_can_redo(&self) -> bool {
        self.session.history.can_redo()
    }

    /// Record `snapshot` as the pre-mutation state for one undoable step.
    pub fn push_history(&mut self, snapshot: super::history::HistorySnapshot) {
        self.session.history.push(snapshot);
    }

    /// Clear the undo/redo timeline (e.g. on workspace load).
    pub fn clear_history(&mut self) {
        self.session.history.clear();
    }

    /// Restore a history snapshot onto live state (scene + stage), returning
    /// what changed so a windowed caller can refresh GPU preview textures.
    fn restore_history_snapshot(
        &mut self,
        snapshot: super::history::HistorySnapshot,
    ) -> super::history::HistoryRestore {
        let rw = self.render_width;
        let rh = self.render_height;
        // Scene half — diff-apply (patches only what changed).
        let (warnings, structural_changed) = self.apply_history_scene(&snapshot.scene, rw, rh);
        // Stage half — restore surfaces + assignments (no window lifecycle).
        // Cosmetic editor prefs are intentionally left untouched; dome layout
        // flags live in UI layout and are restored by the windowed caller.
        self.apply_stage_diff(&snapshot.stage);
        self.mixer.clear_sub_mix_cache();
        for w in &warnings {
            log::warn!("History restore warning: {w}");
        }
        super::history::HistoryRestore {
            snapshot,
            structural_changed,
        }
    }

    /// Undo the most recent undoable action. `current` is the live state to
    /// place on the redo stack. Returns `None` when the undo stack is empty.
    pub fn history_undo(
        &mut self,
        current: super::history::HistorySnapshot,
    ) -> Option<super::history::HistoryRestore> {
        let snapshot = self.session.history.undo(current)?;
        Some(self.restore_history_snapshot(snapshot))
    }

    /// Redo the most recently undone action. `current` is the live state to
    /// place on the undo stack. Returns `None` when the redo stack is empty.
    pub fn history_redo(
        &mut self,
        current: super::history::HistorySnapshot,
    ) -> Option<super::history::HistoryRestore> {
        let snapshot = self.session.history.redo(current)?;
        Some(self.restore_history_snapshot(snapshot))
    }

    /// Apply a stage diff: restore the authored stage state from a `StagePrefs`
    /// snapshot onto live state for undo/redo.
    ///
    /// Restores surfaces (geometry, warp, holes, combine, stacking, `dome_setup`)
    /// and per-output surface assignments. Deliberately does NOT recreate,
    /// remove, move, or resize output windows/monitors, and does NOT touch
    /// cosmetic editor prefs (grid size, snap, panel-open flags) — those are not
    /// authored content. Dome layout flags (mode/preset/geometry) live in UI
    /// layout state and are restored by the caller (the runner).
    ///
    /// GPU-derived caches rebuild automatically from the restored surface data:
    /// hole masks are content-hash keyed and warp meshes are tessellated per
    /// frame, so no explicit cache invalidation is needed here.
    pub fn apply_stage_diff(&mut self, target: &crate::persistence::StagePrefs) {
        // (a) Surfaces — plain data, swap wholesale.
        self.output.surface_manager = target.surfaces.clone();

        // (a2) Production buses — plain data, swap wholesale. Undo covers the
        //      authored side of a bus (existence, name, canvas, default take
        //      duration); the live take controls are denied a history entry in
        //      `command_is_undoable`, so restoring the bank cannot rewind
        //      what is on air out from under an operator.
        if let Some(bus_configs) = &target.buses {
            // Structural merge, not wholesale replacement. Undo restores what
            // was *authored* — a bus existing, its name, canvas, default take
            // duration. PVW, PGM and any take in flight are live state: undoing
            // an unrelated edit must not change the picture going out, nor
            // cancel a take mid-flight.
            self.production_buses.apply_structural_diff(bus_configs);
            let known = self.known_source_uuids();
            for unresolved in self.production_buses.reconcile(&known) {
                log::warn!("Bus routing does not resolve after undo/redo: {unresolved}");
            }
        }

        // (a3) The rundown's steps — authored, put back whole, after the
        //      buses they set. Where the show is and the fade time are live
        //      and stay; a step being waited on is no longer waited on, since
        //      what it led to may have changed (see `Rundown::restore_steps`).
        if let Some(rundown) = &target.rundown {
            self.rundown.restore_steps(rundown);
        }

        // (b) Per-output surface assignments — patch by matching uuid to the
        //     snapshot's OutputConfig. Never create/destroy/reposition windows;
        //     outputs present in the snapshot but not live (or vice versa) are
        //     ignored for lifecycle.
        let mut restored_output_indices = Vec::new();
        for (idx, output) in self.output.outputs.iter_mut().enumerate() {
            let uuid = output.uuid().to_string();
            if let Some(cfg) = target.outputs.iter().find(|c| c.uuid == uuid) {
                // Its name, title too: a bus renamed takes its own screen's
                // with it (see `cmd_rename_bus`), and undone, gives it back.
                match output {
                    crate::renderer::context::UnifiedOutput::Window(window) => {
                        if window.name != cfg.name {
                            window.rename(&cfg.name);
                        }
                    }
                    // A bus's Blackmagic output, renamed with the bus.
                    crate::renderer::context::UnifiedOutput::Headless(headless) => {
                        if headless.name != cfg.name {
                            headless.name.clone_from(&cfg.name);
                        }
                    }
                }
                *output.surface_assignments_mut() = cfg
                    .surface_assignments
                    .iter()
                    .map(|a| crate::renderer::context::SurfaceAssignment {
                        surface_uuid: a.surface_uuid.clone(),
                        enabled: a.enabled,
                        overlap_zones: crate::renderer::edge_blend::SurfaceOverlapZones::default(),
                    })
                    .collect();
                match output.set_presentation_request(&self.context, cfg.presentation) {
                    Ok(()) => {
                        if let crate::renderer::context::UnifiedOutput::Headless(headless) = output
                        {
                            if matches!(
                                &headless.target,
                                crate::renderer::context::OutputTarget::NdiSend { .. }
                            ) {
                                let resolved = self
                                    .external_io
                                    .ndi_manager
                                    .resolve_presentation(cfg.presentation);
                                headless.set_resolved_presentation(&self.context.device, resolved);
                            }
                        }
                        restored_output_indices.push(idx);
                    }
                    Err(error) => {
                        log::warn!(
                            "Could not restore presentation precision for output '{uuid}': {error}"
                        );
                    }
                }
            }
        }
        for idx in restored_output_indices {
            self.refresh_presentation_notification(idx);
        }

        // (c) Recompute Auto-mode edge-blend overlap zones for the restored
        //     surface topology.
        self.recompute_auto_edge_blend();
    }

    /// Patch a `DeckSlot`'s properties from config without rebuilding the source.
    pub(crate) fn patch_deck_slot(
        slot: &mut crate::channel::DeckSlot,
        config: &crate::scene::DeckConfig,
        context: &crate::renderer::GpuContext,
        registry: &crate::registry::ShaderRegistry,
    ) {
        // No `set_uuid` here: the merge selects a runtime *because* it already
        // carries this identity, and a newly constructed one takes the saved
        // UUID from `restore_deck`. Reassigning would only mask a mispairing.
        // Transparency and render FPS are part of a deck's persisted look and
        // cost, and the full restore path applies both. Omitting them here made
        // a history-restored or late-bound deck visually and temporally
        // different from the same deck loaded normally.
        crate::persistence::apply_slot_settings(slot, config, context);

        // Patch generator params for shader sources
        if let crate::scene::SourceConfig::Shader { params, .. } = &config.source {
            slot.deck.generator_params.values.clone_from(params);
        }

        // Patch solid color
        if let crate::scene::SourceConfig::SolidColor { color } = &config.source {
            slot.deck.set_solid_color(*color);
        }

        // Patch a camera's colour; the frame loop passes it on to the
        // capture (see `CameraManager::set_colours`).
        if let crate::scene::SourceConfig::Camera {
            colour,
            profile,
            lowest_latency,
            ..
        } = &config.source
        {
            slot.deck.camera_colour = *colour;
            slot.deck.camera_profile.clone_from(profile);
            slot.deck.camera_lowest_latency = *lowest_latency;
        }

        // Patch a playlist, which plays on from what it shows.
        if let crate::scene::SourceConfig::Playlist {
            items,
            settings,
            scaling_mode,
        } = &config.source
        {
            if let Some(list) = slot.deck.playlist_mut() {
                if list.items() != items.as_slice() {
                    list.set_items(items.clone());
                }
                list.set_settings(*settings);
                list.set_scaling_mode(*scaling_mode);
            }
        }
        if let crate::scene::SourceConfig::Timer { settings, .. } = &config.source {
            if let Some(timer) = slot.deck.timer_mut() {
                if timer.settings() != settings {
                    timer.set_settings(settings.clone());
                }
            }
        }

        // Patch deck effects
        Self::diff_effects(
            &mut slot.deck.effects,
            &config.effects,
            context,
            context.compositing_format,
            &mut Vec::new(),
        );
        slot.deck.settle_layer_mask();

        // Patch auto-transition config
        if let Some(at_config) = &config.auto_transition {
            use crate::channel::TransitionTrigger;
            let mut at = slot.auto_transition.take().unwrap_or_default();
            at.enabled = at_config.enabled;
            at.trigger = match at_config.trigger {
                crate::scene::TriggerConfig::Timer => TransitionTrigger::Timer,
                crate::scene::TriggerConfig::ClipEnd => TransitionTrigger::ClipEnd,
            };
            at.play_duration = duration_config_to_spec(&at_config.play_duration);
            at.transition_duration = duration_config_to_spec(&at_config.transition_duration);
            at.transition_shader_name
                .clone_from(&at_config.transition_shader);
            slot.auto_transition = Some(at);

            // Compile transition shader if specified
            if let Some(shader_name) = &at_config.transition_shader {
                // Only recompile if the shader name changed
                let needs_compile = slot
                    .transition_effect
                    .as_ref()
                    .is_none_or(|te| te.shader.name() != *shader_name);
                if needs_compile {
                    if let Some(shader) = registry
                        .transitions()
                        .iter()
                        .find(|s| s.name() == *shader_name)
                    {
                        if let Err(e) = slot.set_transition_shader(context, (*shader).clone()) {
                            log::warn!(
                                "Failed to restore deck transition shader '{shader_name}': {e}"
                            );
                        }
                    }
                }
            } else {
                slot.transition_effect = None;
            }
        } else {
            slot.auto_transition = None;
            slot.transition_effect = None;
        }
    }

    /// Diff an effect chain: patch params for matching effects, rebuild for mismatches.
    /// Reconcile an effect chain by identity, in target order.
    ///
    /// Same reasoning as channels and decks: a positional pass repurposes a
    /// surviving effect as a different identity. For effects the consequence is
    /// sharper, because `param_prefix` is derived from the UUID and modulation
    /// is keyed on that prefix — removing a middle effect from three sharing a
    /// shader and undoing used to leave a duplicate UUID and a missing one,
    /// after which first-match routing addressed the wrong effect.
    fn diff_effects(
        effects: &mut Vec<crate::deck::Effect>,
        target: &[crate::scene::EffectConfig],
        context: &crate::renderer::GpuContext,
        target_format: wgpu::TextureFormat,
        warnings: &mut Vec<String>,
    ) {
        let mut pool: std::collections::HashMap<String, crate::deck::Effect> = effects
            .drain(..)
            .map(|eff| (eff.uuid().to_string(), eff))
            .collect();

        for cfg in target {
            // A survivor on the same shader keeps its runtime; one whose shader
            // changed is rebuilt even though the identity is unchanged.
            let reusable = pool
                .get(&cfg.uuid)
                .is_some_and(|eff| eff.shader.file_path.as_deref().unwrap_or("") == cfg.path);
            let existing = pool.remove(&cfg.uuid);
            if reusable {
                let mut eff = existing.expect("checked present above");
                eff.enabled = cfg.enabled;
                eff.params.values.clone_from(&cfg.params);
                eff.mask.clone_from(&cfg.mask);
                effects.push(eff);
                continue;
            }
            match crate::persistence::restore_effect(cfg, context, target_format) {
                Ok(eff) => effects.push(eff),
                // As with decks: a failed rebuild must not also discard the
                // effect that was working.
                Err(e) => match existing {
                    Some(kept) => {
                        warnings.push(format!(
                            "Failed to rebuild effect '{}': {e} — keeping the running one",
                            cfg.path
                        ));
                        effects.push(kept);
                    }
                    None => {
                        warnings.push(format!("Failed to restore effect '{}': {e}", cfg.path));
                    }
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::traits::*;
    use clap::Parser;
    use tempfile::TempDir;

    fn parse_args(args: &[&str]) -> super::super::AppConfig {
        super::super::AppConfig::parse_from(std::iter::once("varda").chain(args.iter().copied()))
    }

    fn headless_app_in(workspace: &std::path::Path) -> Option<super::super::VardaApp> {
        // Without a GPU the test has nothing to run on — unless one is
        // required, when a green run must have run it.
        let required = std::env::var_os("VARDA_REQUIRE_GPU").is_some();
        let gpu = match crate::renderer::context::GpuContext::new_headless() {
            Ok(gpu) => gpu,
            Err(e) => {
                assert!(!required, "no GPU: {e}");
                return None;
            }
        };
        let ws = workspace.to_str().unwrap();
        let config = parse_args(&[
            "--headless",
            "--no-osc",
            "--no-ndi",
            "--no-syphon",
            // Tests build apps side by side; each would open every camera.
            "--no-camera",
            "--workspace",
            ws,
        ]);
        match super::super::VardaApp::new(gpu, &config) {
            Ok(app) => Some(app),
            Err(e) => {
                assert!(!required, "no app: {e}");
                None
            }
        }
    }

    #[test]
    fn load_workspace_no_varda_dir() {
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        // No .varda/ exists → load_workspace returns None
        let result = app.load_workspace();
        assert!(result.layout.is_none());
        assert!(result.is_ok());
    }

    #[test]
    fn save_load_roundtrip_scene() {
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        let ch = app.mixer_snapshot().channels[0].uuid.clone();
        app.add_solid_color_deck(&ch, [1.0, 0.0, 0.0, 1.0]).unwrap();
        app.set_crossfader(0.6);
        app.save_workspace(&UILayoutState::default())
            .expect("save workspace");

        let Some(mut app2) = headless_app_in(tmp.path()) else {
            return;
        };
        let _ = app2.load_workspace();
        let snap = app2.mixer_snapshot();
        assert!(
            !snap.channels[0].decks.is_empty(),
            "deck should survive roundtrip"
        );
        assert!((snap.crossfader - 0.6).abs() < 1e-4);
    }

    #[test]
    fn save_load_roundtrip_stage() {
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        let _uuid = app.add_surface(
            "Test Surface",
            crate::renderer::context::OutputSource::Master,
        );
        app.save_workspace(&UILayoutState::default())
            .expect("save workspace");

        let Some(mut app2) = headless_app_in(tmp.path()) else {
            return;
        };
        let _ = app2.load_workspace();
        let surfaces = app2.surface_snapshot();
        assert!(
            surfaces.iter().any(|s| s.name == "Test Surface"),
            "surface should survive roundtrip"
        );
    }

    #[test]
    fn save_load_roundtrip_keeps_requested_presentation() {
        use crate::engine::value::render::{PresentationDepth, PresentationRequest};
        use crate::engine::{CommandResult, EngineCommand};
        use crate::renderer::context::OutputTarget;

        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        assert!(matches!(
            app.execute_command(EngineCommand::CreateHeadlessOutput {
                target: OutputTarget::SyphonServer {
                    server_name: "Precision".into(),
                },
            }),
            CommandResult::Ok
        ));
        let output_uuid = app.build_engine_state().outputs.windows[0].uuid.clone();
        assert!(matches!(
            app.execute_command(EngineCommand::SetOutputPresentation {
                output_uuid: output_uuid.clone(),
                request: PresentationRequest {
                    depth: PresentationDepth::Sdr10,
                    dither: false,
                    video_levels: false,
                },
            }),
            CommandResult::Ok
        ));
        app.save_workspace(&UILayoutState::default())
            .expect("save workspace");

        let stage: crate::persistence::StagePrefs = serde_json::from_str(
            &std::fs::read_to_string(tmp.path().join(".varda").join("stage.json")).unwrap(),
        )
        .unwrap();
        let output = stage.outputs.first().expect("saved headless output");
        assert_eq!(output.uuid, output_uuid);
        assert_eq!(output.presentation.depth, PresentationDepth::Sdr10);
        assert!(!output.presentation.dither);
    }

    /// A stage file that has an output written twice opens it once: two
    /// windows of one output would both show, and both be saved again.
    #[test]
    fn an_output_written_twice_in_the_stage_opens_once() {
        let tmp = TempDir::new().unwrap();
        let varda_dir = tmp.path().join(".varda");
        std::fs::create_dir_all(&varda_dir).unwrap();
        let led = crate::scene::OutputConfig {
            name: "LED".into(),
            ..crate::scene::OutputConfig::default_windowed()
        };
        let tv = crate::scene::OutputConfig {
            name: "TV".into(),
            ..crate::scene::OutputConfig::default_windowed()
        };
        let stage = crate::persistence::StagePrefs {
            outputs: vec![led.clone(), tv, led],
            ..crate::persistence::StagePrefs::default()
        };
        std::fs::write(
            varda_dir.join("stage.json"),
            serde_json::to_string(&stage).unwrap(),
        )
        .unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        let _ = app.load_workspace();
        let names: Vec<&str> = app.pending_outputs().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["LED", "TV"]);
    }

    /// A show opened closes the outputs open before that it does not list,
    /// and lets go of such outputs waiting to open; one it lists stays up as
    /// it is. "Yes, should close" — the user, 2026-10-04, asked whether the
    /// last show's outputs should stay up.
    #[test]
    fn opening_a_show_closes_the_outputs_it_does_not_list() {
        use crate::engine::{CommandResult, EngineCommand, OutputCommands as _};
        use crate::renderer::context::OutputTarget;

        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        for name in ["Kept", "Closed"] {
            assert!(matches!(
                app.execute_command(EngineCommand::CreateHeadlessOutput {
                    target: OutputTarget::SyphonServer {
                        server_name: name.into(),
                    },
                }),
                CommandResult::Ok
            ));
        }
        let open: Vec<String> = app
            .build_engine_state()
            .outputs
            .windows
            .iter()
            .map(|w| w.uuid.clone())
            .collect();
        assert_eq!(open.len(), 2);
        // One more waiting to open, from the show before.
        app.request_create_output();

        let varda_dir = tmp.path().join(".varda");
        std::fs::create_dir_all(&varda_dir).unwrap();
        let kept = crate::scene::OutputConfig {
            uuid: open[0].clone(),
            name: "Kept".into(),
            ..crate::scene::OutputConfig::default_windowed()
        };
        let new = crate::scene::OutputConfig {
            name: "New".into(),
            ..crate::scene::OutputConfig::default_windowed()
        };
        let stage = crate::persistence::StagePrefs {
            outputs: vec![kept, new],
            ..crate::persistence::StagePrefs::default()
        };
        std::fs::write(
            varda_dir.join("stage.json"),
            serde_json::to_string(&stage).unwrap(),
        )
        .unwrap();
        let _ = app.load_workspace();

        let still: Vec<String> = app
            .build_engine_state()
            .outputs
            .windows
            .iter()
            .map(|w| w.uuid.clone())
            .collect();
        assert_eq!(still, [open[0].clone()], "the unlisted one closed");
        let waiting: Vec<&str> = app.pending_outputs().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(waiting, ["New"], "the last show's waiting one let go");
    }

    #[test]
    fn load_corrupt_scene_json() {
        let tmp = TempDir::new().unwrap();
        let varda_dir = tmp.path().join(".varda");
        std::fs::create_dir_all(&varda_dir).unwrap();
        // Write corrupt JSON
        std::fs::write(varda_dir.join("scene.json"), "not valid json {{{").unwrap();

        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        let result = app.load_workspace();
        assert!(!result.is_ok(), "corrupt scene.json must be a load failure");
        assert!(result.error_message().expect("error").contains("scene"));
        let snap = app.mixer_snapshot();
        assert_eq!(snap.channels.len(), 2);
        assert!(
            app.session.notifications.visible().iter().any(|n| n.level
                == crate::notifications::NotificationLevel::Error
                && n.message.contains("scene")),
            "operator must see the load failure"
        );
    }

    #[test]
    fn load_missing_scene_file() {
        let tmp = TempDir::new().unwrap();
        let varda_dir = tmp.path().join(".varda");
        std::fs::create_dir_all(&varda_dir).unwrap();
        // .varda/ exists but no scene.json

        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        let _ = app.load_workspace();
        // Should skip scene loading gracefully
        let snap = app.mixer_snapshot();
        assert_eq!(snap.channels.len(), 2);
    }

    #[test]
    fn save_creates_varda_dir() {
        let tmp = TempDir::new().unwrap();
        let varda_dir = tmp.path().join(".varda");
        assert!(!varda_dir.exists());
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        app.save_workspace(&UILayoutState::default())
            .expect("save workspace");
        assert!(varda_dir.exists());
    }

    #[test]
    fn save_failure_toasts_and_returns_err() {
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        app.save_workspace(&UILayoutState::default())
            .expect("save workspace");
        let scene = tmp.path().join(".varda").join("scene.json");
        std::fs::remove_file(&scene).unwrap();
        std::fs::create_dir(&scene).unwrap();
        let err = app
            .save_workspace(&UILayoutState::default())
            .expect_err("a directory named scene.json must fail the save");
        assert!(err.to_string().contains("scene"));
        assert!(
            app.session.notifications.visible().iter().any(|n| n.level
                == crate::notifications::NotificationLevel::Error
                && n.message.contains("scene")),
            "operator must see the save failure"
        );
    }

    #[test]
    fn load_workspace_command_returns_err_on_corrupt_scene() {
        let tmp = TempDir::new().unwrap();
        let varda_dir = tmp.path().join(".varda");
        std::fs::create_dir_all(&varda_dir).unwrap();
        std::fs::write(varda_dir.join("scene.json"), "not valid json {{{").unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        match app.execute_command(crate::engine::EngineCommand::LoadWorkspace) {
            crate::engine::CommandResult::Err {
                code: crate::engine::ErrorCode::InternalError,
                message,
            } => assert!(message.contains("scene")),
            other => panic!("expected InternalError, got {other:?}"),
        }
    }

    #[test]
    fn load_workspace_returns_layout() {
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        let layout = UILayoutState {
            stage_editor_open: true,
            library_panel_open: true,
            ..Default::default()
        };
        app.save_workspace(&layout).expect("save workspace");

        let Some(mut app2) = headless_app_in(tmp.path()) else {
            return;
        };
        let loaded = app2.load_workspace();
        assert!(loaded.is_ok(), "should load without hard errors");
        let loaded = loaded.layout.expect("should return layout");
        assert!(loaded.stage_editor_open);
        assert!(loaded.library_panel_open);
    }

    /// `EngineCommand::SaveWorkspace` carries no layout, so it must reuse the one
    /// loaded from disk instead of writing defaults over the user's panels.
    #[test]
    fn command_save_preserves_loaded_layout() {
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        app.save_workspace(&UILayoutState {
            stage_editor_open: true,
            library_panel_open: true,
            ..Default::default()
        })
        .expect("save workspace");

        let Some(mut app2) = headless_app_in(tmp.path()) else {
            return;
        };
        let _ = app2.load_workspace();
        app2.execute_command(crate::engine::EngineCommand::SaveWorkspace);
        app2.flush_saves();

        let Some(mut app3) = headless_app_in(tmp.path()) else {
            return;
        };
        let reloaded = app3.load_workspace().layout.expect("layout should survive");
        assert!(reloaded.stage_editor_open);
        assert!(reloaded.library_panel_open);
    }

    fn notices(app: &super::super::VardaApp) -> Vec<String> {
        app.session
            .notifications
            .visible()
            .iter()
            .map(|n| n.message.clone())
            .collect()
    }

    fn fade_time_in(stage: &std::path::Path) -> Option<f64> {
        let text = std::fs::read_to_string(stage.join(".varda/stage.json")).ok()?;
        let stage: serde_json::Value = serde_json::from_str(&text).ok()?;
        stage["rundown"]["fade_secs"].as_f64()
    }

    fn set_fade(app: &mut super::super::VardaApp, secs: f32) {
        assert!(matches!(
            app.execute_command(crate::engine::EngineCommand::SetRundownFade { secs }),
            crate::engine::CommandResult::Ok
        ));
    }

    const SLOW_DISK: std::time::Duration = std::time::Duration::from_millis(400);

    /// Save, on air: the show is handed to the writer and the engine goes
    /// on — the outputs do not wait for a slow disk. "Saved" is said once
    /// the files are there, not before.
    #[test]
    fn a_save_on_air_does_not_wait_for_the_disk() {
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        app.slow_saves_for_test(SLOW_DISK);
        set_fade(&mut app, 2.5);
        let t = std::time::Instant::now();
        app.save_in_background(&UILayoutState::default());
        let handed_over = t.elapsed();
        assert!(
            handed_over < std::time::Duration::from_millis(150),
            "the save held the engine {handed_over:?}"
        );
        app.update_notifications();
        assert!(!notices(&app).iter().any(|m| m.contains("saved")), "{:?}", notices(&app));
        assert_eq!(fade_time_in(tmp.path()), None, "not written yet");
        // Said by a frame once the files are there, as the engine's frames
        // look for what has landed.
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !notices(&app).iter().any(|m| m.contains("saved")) {
            assert!(std::time::Instant::now() < until, "never said");
            std::thread::sleep(std::time::Duration::from_millis(5));
            app.update_notifications();
        }
        assert!(t.elapsed() >= SLOW_DISK);
        assert_eq!(fade_time_in(tmp.path()), Some(2.5));
        let show = app.show_name();
        assert_eq!(
            notices(&app)
                .iter()
                .filter(|m| m.contains("saved"))
                .collect::<Vec<_>>(),
            [&format!("💾 “{show}” saved")]
        );
    }

    /// A save that cannot be written says so once it is known, as a save
    /// always has; nothing says it was saved.
    #[test]
    fn a_save_in_the_background_that_fails_says_so() {
        let tmp = TempDir::new().unwrap();
        // A file where the show's folder should be: nothing can be written.
        std::fs::write(tmp.path().join(".varda"), "not a folder").unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        app.save_in_background(&UILayoutState::default());
        app.flush_saves();
        let said = notices(&app);
        assert!(
            said.iter().any(|m| m.starts_with("Failed to save workspace: cannot make")),
            "{said:?}"
        );
        assert!(!said.iter().any(|m| m.contains("saved")), "{said:?}");
    }

    /// A save through the API is answered once its files are written — a
    /// backup taken after it copies what was saved — while the engine went
    /// on at once.
    #[test]
    fn an_api_save_is_answered_once_its_files_are_written() {
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        app.slow_saves_for_test(SLOW_DISK);
        set_fade(&mut app, 3.5);
        let (reply, mut answer) = tokio::sync::oneshot::channel();
        app.command_sender()
            .send((crate::engine::EngineCommand::SaveWorkspace, Some(reply)))
            .unwrap();
        let t = std::time::Instant::now();
        app.process_commands();
        assert!(t.elapsed() < std::time::Duration::from_millis(150));
        assert!(answer.try_recv().is_err(), "answered before the files were written");
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let answered = loop {
            if let Ok(result) = answer.try_recv() {
                break result;
            }
            assert!(std::time::Instant::now() < until, "never answered");
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert!(matches!(answered, crate::engine::CommandResult::Ok));
        // Every file is there when the answer comes, the last included.
        for file in ["scene.json", "stage.json", "keymap.json", "osc.json"] {
            assert!(tmp.path().join(".varda").join(file).is_file(), "{file}");
        }
        assert_eq!(fade_time_in(tmp.path()), Some(3.5), "written when answered");
        // Nothing to say on the operator's screen: the API asked.
        app.flush_saves();
        assert!(!notices(&app).iter().any(|m| m.contains("saved")));
    }

    /// One file that cannot be written: the window says which, an API save
    /// is answered with it, and the other files land.
    #[test]
    fn a_file_that_cannot_be_written_in_the_background_is_said() {
        let tmp = TempDir::new().unwrap();
        // A folder where keymap.json goes: renaming over it fails.
        std::fs::create_dir_all(tmp.path().join(".varda/keymap.json")).unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        app.save_in_background(&UILayoutState::default());
        app.flush_saves();
        let said = notices(&app);
        assert!(
            said.iter().any(|m| m.starts_with("Failed to save workspace: keymap:")),
            "{said:?}"
        );
        assert!(!said.iter().any(|m| m.contains("saved")), "{said:?}");
        let (reply, mut answer) = tokio::sync::oneshot::channel();
        app.command_sender()
            .send((crate::engine::EngineCommand::SaveWorkspace, Some(reply)))
            .unwrap();
        app.process_commands();
        app.flush_saves();
        match answer.try_recv() {
            Ok(crate::engine::CommandResult::Err {
                code: crate::engine::ErrorCode::InternalError,
                message,
            }) => assert!(message.contains("keymap:"), "{message}"),
            other => panic!("{other:?}"),
        }
        assert!(tmp.path().join(".varda/scene.json").is_file());
        assert!(tmp.path().join(".varda/osc.json").is_file());
    }

    /// Save As copies the show as saved: a save still on its way lands
    /// before the folder is copied.
    #[test]
    fn save_as_copies_what_was_being_saved() {
        let tmp = TempDir::new().unwrap();
        let first = tmp.path().join("first");
        std::fs::create_dir_all(&first).unwrap();
        let Some(mut app) = headless_app_in(&first) else {
            return;
        };
        app.save_workspace(&UILayoutState::default()).unwrap();
        app.slow_saves_for_test(SLOW_DISK);
        // A file Save As copies and no save writes, landing late.
        let late = first.join(".varda/presets/decks/late.json");
        app.show_writer.submit(crate::persistence::writer::ShowSave {
            dir: first.join(".varda/presets/decks"),
            files: vec![crate::persistence::writer::ShowFile {
                key: "preset",
                label: "preset",
                path: late,
                content: Ok("{}".into()),
            }],
            show: "first".into(),
            announce: false,
            then: None,
        });
        let second = tmp.path().join("second");
        app.save_show_as(&second, &UILayoutState::default()).unwrap();
        app.flush_saves();
        assert_eq!(app.show_name(), "second");
        assert!(second.join(".varda/presets/decks/late.json").is_file());
    }

    /// Quitting answers a command still waiting — an HTTP request would
    /// otherwise keep the API server, and Nida, from closing — and takes
    /// none after.
    #[test]
    fn quitting_answers_commands_still_waiting() {
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        let sender = app.command_sender();
        let (reply, mut answer) = tokio::sync::oneshot::channel();
        sender
            .send((crate::engine::EngineCommand::SaveWorkspace, Some(reply)))
            .unwrap();
        app.refuse_commands();
        match answer.try_recv() {
            Ok(crate::engine::CommandResult::Err {
                code: crate::engine::ErrorCode::Unavailable,
                message,
            }) => assert_eq!(message, "Nida is quitting"),
            other => panic!("{other:?}"),
        }
        assert!(
            sender
                .send((crate::engine::EngineCommand::SaveWorkspace, None))
                .is_err(),
            "a command taken after"
        );
    }

    /// An API save that cannot be written is answered with why.
    #[test]
    fn an_api_save_that_fails_is_answered_with_why() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".varda"), "not a folder").unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        let (reply, mut answer) = tokio::sync::oneshot::channel();
        app.command_sender()
            .send((crate::engine::EngineCommand::SaveWorkspace, Some(reply)))
            .unwrap();
        app.process_commands();
        app.flush_saves();
        match answer.try_recv() {
            Ok(crate::engine::CommandResult::Err { message, .. }) => assert!(
                message.starts_with("Failed to save workspace: cannot make"),
                "{message}"
            ),
            other => panic!("{other:?}"),
        }
    }

    /// A save that must be written now — Nida quitting, a show opened —
    /// lands after the one still on its way, never under it.
    #[test]
    fn a_save_made_now_lands_after_the_one_on_its_way() {
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        app.slow_saves_for_test(SLOW_DISK);
        set_fade(&mut app, 2.0);
        app.save_in_background(&UILayoutState::default());
        set_fade(&mut app, 3.0);
        app.save_workspace(&UILayoutState::default()).unwrap();
        app.flush_saves();
        assert_eq!(fade_time_in(tmp.path()), Some(3.0));
    }

    /// A load reads the files back: what is being written lands first.
    #[test]
    fn a_load_reads_what_was_saved_in_the_background() {
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        set_fade(&mut app, 1.5);
        app.save_workspace(&UILayoutState::default()).unwrap();
        app.slow_saves_for_test(SLOW_DISK);
        set_fade(&mut app, 4.0);
        app.save_in_background(&UILayoutState::default());
        let loaded = app.load_workspace();
        assert!(loaded.is_ok(), "{:?}", loaded.error_message());
        assert_eq!(app.rundown_snapshot().fade_secs, 4.0);
    }

    /// Saves still on their way land before the show is let go of.
    #[test]
    fn a_save_on_its_way_lands_when_nida_lets_go_of_the_show() {
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        app.slow_saves_for_test(SLOW_DISK);
        set_fade(&mut app, 5.0);
        app.save_in_background(&UILayoutState::default());
        drop(app);
        assert_eq!(fade_time_in(tmp.path()), Some(5.0));
    }

    /// Frames, as the engine runs them, until `done` — at most ten seconds.
    fn frames_until(
        app: &mut super::super::VardaApp,
        mut done: impl FnMut(&mut super::super::VardaApp) -> bool,
    ) {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !done(app) {
            assert!(std::time::Instant::now() < until, "never came");
            std::thread::sleep(std::time::Duration::from_millis(5));
            app.update_notifications();
        }
    }

    fn a_deck(app: &mut super::super::VardaApp) -> String {
        let channel_uuid = app.build_engine_state().mixer.channels[0].uuid.clone();
        match app.execute_command(crate::engine::EngineCommand::AddSolidColorDeck {
            channel_uuid,
            color: [1.0, 0.0, 0.0, 1.0],
        }) {
            crate::engine::CommandResult::OkWithId { uuid } => uuid,
            other => panic!("no deck: {other:?}"),
        }
    }

    /// Save As, on air: the show is copied on the writer's thread and the
    /// engine goes on — the outputs do not wait for a slow disk. The show
    /// stays in its folder until the copy has landed, and moves then,
    /// saying so.
    #[test]
    fn a_save_as_on_air_does_not_wait_for_the_disk() {
        let tmp = TempDir::new().unwrap();
        let (first, second) = (tmp.path().join("first"), tmp.path().join("second"));
        std::fs::create_dir_all(first.join(".varda/presets/decks")).unwrap();
        std::fs::write(first.join(".varda/presets/decks/logo.json"), "{}").unwrap();
        let Some(mut app) = headless_app_in(&first) else {
            return;
        };
        app.slow_saves_for_test(SLOW_DISK);
        set_fade(&mut app, 2.5);
        let t = std::time::Instant::now();
        app.save_show_as(&second, &UILayoutState::default())
            .unwrap();
        let handed_over = t.elapsed();
        assert!(
            handed_over < std::time::Duration::from_millis(150),
            "the Save As held the engine {handed_over:?}"
        );
        app.update_notifications();
        assert_eq!(
            app.show_name(),
            "first",
            "it moves once the copy has landed"
        );
        frames_until(&mut app, |app| app.show_name() == "second");
        assert!(t.elapsed() >= SLOW_DISK);
        assert_eq!(fade_time_in(&second), Some(2.5));
        assert!(
            second.join(".varda/presets/decks/logo.json").is_file(),
            "its presets go with it"
        );
        let said = notices(&app);
        assert!(
            said.contains(&"💾 Saved as “second”".to_string()),
            "{said:?}"
        );
    }

    /// What the operator saves while Save As copies the show goes into the
    /// new show, as they meant: the show saved from stays as it was — a
    /// template is not written over. A preset saved meanwhile goes along.
    #[test]
    fn saves_made_while_saving_as_go_into_the_new_show() {
        let tmp = TempDir::new().unwrap();
        let (template, day) = (tmp.path().join("Template"), tmp.path().join("Day 2"));
        std::fs::create_dir_all(&template).unwrap();
        let Some(mut app) = headless_app_in(&template) else {
            return;
        };
        set_fade(&mut app, 1.5);
        app.save_workspace(&UILayoutState::default()).unwrap();
        let deck = a_deck(&mut app);
        app.slow_saves_for_test(SLOW_DISK);
        app.save_show_as(&day, &UILayoutState::default()).unwrap();
        set_fade(&mut app, 3.0);
        app.save_in_background(&UILayoutState::default());
        assert!(matches!(
            app.execute_command(crate::engine::EngineCommand::SaveDeckPreset {
                deck_uuid: deck,
                name: "Logo".into(),
            }),
            crate::engine::CommandResult::Ok
        ));
        app.flush_saves();
        assert_eq!(app.show_name(), "Day 2");
        assert_eq!(fade_time_in(&day), Some(3.0));
        assert_eq!(
            fade_time_in(&template),
            Some(1.5),
            "the template stays as it was"
        );
        assert!(day.join(".varda/presets/decks/Logo.json").is_file());
        assert!(!template.join(".varda/presets").exists());
        let said = notices(&app);
        assert!(said.contains(&"💾 “Day 2” saved".to_string()), "{said:?}");
    }

    /// Once moved, the show it left is a show like any other: opened again,
    /// its saves go into it.
    #[test]
    fn the_show_left_by_a_save_as_saves_into_itself_again() {
        let tmp = TempDir::new().unwrap();
        let (a, b) = (tmp.path().join("A"), tmp.path().join("B"));
        std::fs::create_dir_all(&a).unwrap();
        let Some(mut app) = headless_app_in(&a) else {
            return;
        };
        set_fade(&mut app, 1.5);
        app.save_workspace(&UILayoutState::default()).unwrap();
        app.save_show_as(&b, &UILayoutState::default()).unwrap();
        app.flush_saves();
        assert_eq!(app.show_name(), "B");
        app.open_show(&a, &UILayoutState::default()).unwrap();
        set_fade(&mut app, 4.0);
        app.save_in_background(&UILayoutState::default());
        app.flush_saves();
        assert_eq!(fade_time_in(&a), Some(4.0));
        assert_eq!(fade_time_in(&b), Some(1.5));
    }

    /// A Save As that cannot land — a folder of someone else's files —
    /// says why once that is known, and the show stays where it was.
    #[test]
    fn a_save_as_that_cannot_land_stays_and_says_why() {
        let tmp = TempDir::new().unwrap();
        let (first, taken) = (tmp.path().join("first"), tmp.path().join("Taxes"));
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&taken).unwrap();
        std::fs::write(taken.join("2025.pdf"), b"").unwrap();
        let Some(mut app) = headless_app_in(&first) else {
            return;
        };
        app.save_show_as(&taken, &UILayoutState::default()).unwrap();
        app.flush_saves();
        assert_eq!(app.show_name(), "first");
        assert!(!taken.join(".varda").exists());
        let said = notices(&app);
        let why = format!(
            "Show not saved as {0}: {0} already holds other files",
            taken.display()
        );
        assert!(said.iter().any(|m| m.starts_with(&why)), "{said:?}");
        assert!(!said.iter().any(|m| m.contains("Saved as")), "{said:?}");
    }

    /// One Save As at a time: another asked for while the first is on its
    /// way is refused, saying why, and the first lands as asked.
    #[test]
    fn a_save_as_waits_for_the_one_on_its_way() {
        let tmp = TempDir::new().unwrap();
        let (a, b, c) = (
            tmp.path().join("A"),
            tmp.path().join("B"),
            tmp.path().join("C"),
        );
        std::fs::create_dir_all(&a).unwrap();
        let Some(mut app) = headless_app_in(&a) else {
            return;
        };
        app.slow_saves_for_test(SLOW_DISK);
        app.save_show_as(&b, &UILayoutState::default()).unwrap();
        assert!(app.save_show_as(&c, &UILayoutState::default()).is_err());
        let said = notices(&app);
        assert!(
            said.iter().any(|m| m.starts_with(&format!(
                "Show not saved as {}: it is still being saved as {}",
                c.display(),
                b.display()
            ))),
            "{said:?}"
        );
        app.flush_saves();
        assert_eq!(app.show_name(), "B");
        assert!(!c.exists());
        app.save_show_as(&c, &UILayoutState::default()).unwrap();
        app.flush_saves();
        assert_eq!(app.show_name(), "C", "once it has landed, another may go");
    }

    /// Save As through the API is answered once the show has moved: what
    /// the API reads next is the new show. The engine goes on meanwhile.
    #[test]
    fn an_api_save_as_is_answered_once_the_show_has_moved() {
        let tmp = TempDir::new().unwrap();
        let (a, b) = (tmp.path().join("A"), tmp.path().join("B"));
        std::fs::create_dir_all(&a).unwrap();
        let Some(mut app) = headless_app_in(&a) else {
            return;
        };
        app.slow_saves_for_test(SLOW_DISK);
        let sender = app.command_sender();
        let (reply, mut answer) = tokio::sync::oneshot::channel();
        sender
            .send((
                crate::engine::EngineCommand::SaveShowAs { folder: b.clone() },
                Some(reply),
            ))
            .unwrap();
        let t = std::time::Instant::now();
        app.process_commands();
        assert!(t.elapsed() < std::time::Duration::from_millis(150));
        assert!(answer.try_recv().is_err(), "not before the copy has landed");
        // The copy written, but no frame since to take it in: the show has
        // not moved, so no answer yet.
        app.show_writer.flush();
        assert!(b.join(".varda/scene.json").is_file());
        assert!(
            answer.try_recv().is_err(),
            "answered once the show has moved"
        );
        app.update_notifications();
        let result = answer.try_recv().expect("answered by the frame that moved");
        assert!(
            matches!(result, crate::engine::CommandResult::Ok),
            "{result:?}"
        );
        assert_eq!(app.show_name(), "B");
    }

    /// Save As into the folder the show is in is a save like any other: on
    /// its way at once, said once it lands; through the API, answered then.
    #[test]
    fn a_save_as_into_its_own_folder_is_a_save() {
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        app.slow_saves_for_test(SLOW_DISK);
        set_fade(&mut app, 2.5);
        let t = std::time::Instant::now();
        app.save_show_as(tmp.path(), &UILayoutState::default())
            .unwrap();
        assert!(t.elapsed() < std::time::Duration::from_millis(150));
        assert_eq!(fade_time_in(tmp.path()), None, "not written yet");
        let show = app.show_name();
        frames_until(&mut app, |app| {
            notices(app).contains(&format!("💾 “{show}” saved"))
        });
        assert_eq!(fade_time_in(tmp.path()), Some(2.5));

        let (reply, answer) = tokio::sync::oneshot::channel();
        app.command_sender()
            .send((
                crate::engine::EngineCommand::SaveShowAs {
                    folder: tmp.path().to_path_buf(),
                },
                Some(reply),
            ))
            .unwrap();
        set_fade(&mut app, 3.5);
        app.process_commands();
        let result = answer.blocking_recv().unwrap();
        assert!(
            matches!(result, crate::engine::CommandResult::Ok),
            "{result:?}"
        );
        assert_eq!(fade_time_in(tmp.path()), Some(3.5), "written when answered");
    }

    /// Save As into the show's own folder by another name — here a link to
    /// it — copies nothing onto itself: its files keep what they hold.
    #[cfg(unix)]
    #[test]
    fn a_save_as_into_its_own_folder_by_another_name_empties_nothing() {
        let tmp = TempDir::new().unwrap();
        let (show, link) = (tmp.path().join("Show"), tmp.path().join("Link"));
        std::fs::create_dir_all(show.join(".varda/presets/decks")).unwrap();
        std::fs::write(show.join(".varda/presets/decks/logo.json"), "{\"kept\":1}").unwrap();
        std::os::unix::fs::symlink(&show, &link).unwrap();
        let Some(mut app) = headless_app_in(&show) else {
            return;
        };
        set_fade(&mut app, 2.5);
        app.save_show_as(&link, &UILayoutState::default()).unwrap();
        app.flush_saves();
        assert_eq!(
            std::fs::read_to_string(show.join(".varda/presets/decks/logo.json")).unwrap(),
            "{\"kept\":1}"
        );
        assert_eq!(fade_time_in(&show), Some(2.5));
        assert_eq!(app.show_name(), "Link");
    }

    /// A Save As into the folder the show is in waits for one on its way
    /// as any other: it would be written where that one moves the show.
    #[test]
    fn a_save_as_into_its_own_folder_waits_for_one_on_its_way() {
        let tmp = TempDir::new().unwrap();
        let (a, b) = (tmp.path().join("A"), tmp.path().join("B"));
        std::fs::create_dir_all(&a).unwrap();
        let Some(mut app) = headless_app_in(&a) else {
            return;
        };
        app.slow_saves_for_test(SLOW_DISK);
        app.save_show_as(&b, &UILayoutState::default()).unwrap();
        assert!(app.save_show_as(&a, &UILayoutState::default()).is_err());
        app.flush_saves();
        assert_eq!(app.show_name(), "B");
    }

    /// Quitting while Save As copies the show: the save made as Nida quits
    /// goes into the new show, after the copy.
    #[test]
    fn quitting_while_saving_as_saves_into_the_new_show() {
        let tmp = TempDir::new().unwrap();
        let (a, b) = (tmp.path().join("A"), tmp.path().join("B"));
        std::fs::create_dir_all(&a).unwrap();
        let Some(mut app) = headless_app_in(&a) else {
            return;
        };
        app.slow_saves_for_test(SLOW_DISK);
        app.save_show_as(&b, &UILayoutState::default()).unwrap();
        set_fade(&mut app, 5.0);
        app.save_workspace(&UILayoutState::default()).unwrap();
        assert_eq!(app.show_name(), "B");
        assert_eq!(fade_time_in(&b), Some(5.0));
        assert_eq!(fade_time_in(&a), None, "the show it left is not written");
    }

    /// Presets as names and settings.
    type Named = Vec<(String, serde_json::Value)>;

    /// The deck presets and channel presets kept, as names and settings.
    fn presets_of(library: &crate::persistence::presets::PresetLibrary) -> (Named, Named) {
        let deck = library
            .deck_presets
            .iter()
            .map(|p| (p.name.clone(), serde_json::to_value(&p.config).unwrap()))
            .collect();
        let channel = library
            .channel_presets
            .iter()
            .map(|p| (p.name.clone(), serde_json::to_value(&p.config).unwrap()))
            .collect();
        (deck, channel)
    }

    fn save_preset_as(app: &mut super::super::VardaApp, deck: &str, name: &str) {
        let result = app.execute_command(crate::engine::EngineCommand::SaveDeckPreset {
            deck_uuid: deck.into(),
            name: name.into(),
        });
        assert!(
            matches!(result, crate::engine::CommandResult::Ok),
            "{result:?}"
        );
    }

    fn deck_name(app: &super::super::VardaApp, deck: &str) -> String {
        app.build_engine_state()
            .mixer
            .channels
            .iter()
            .flat_map(|c| &c.decks)
            .find(|d| d.uuid == deck)
            .map(|d| d.name.clone())
            .unwrap()
    }

    /// The presets kept at once are those the disk has once the files have
    /// landed, as a rescan reads them: one saved twice, two names for one
    /// file, one saved again in other letters, in order.
    #[test]
    fn presets_kept_at_once_are_those_the_disk_has() {
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        let deck = a_deck(&mut app);
        let channel = app.build_engine_state().mixer.channels[0].uuid.clone();
        save_preset_as(&mut app, &deck, "Logo");
        save_preset_as(&mut app, &deck, "b c");
        save_preset_as(&mut app, &deck, "b_c");
        app.execute_command(crate::engine::EngineCommand::SetDeckOpacity {
            deck_uuid: deck.clone(),
            opacity: 0.25,
        });
        save_preset_as(&mut app, &deck, "Logo");
        // Saved again in other letters, the first still on its way and once
        // on disk: on a disk blind to case, into the one file.
        save_preset_as(&mut app, &deck, "Case");
        save_preset_as(&mut app, &deck, "case");
        app.flush_saves();
        save_preset_as(&mut app, &deck, "CASE");
        save_preset_as(&mut app, &deck, "A");
        // A preset file that is a link to another: the save replaces the
        // link, as a rescan reads it, and the file it led to keeps its own.
        #[cfg(unix)]
        {
            app.flush_saves();
            let decks = tmp.path().join(".varda/presets/decks");
            std::os::unix::fs::symlink(decks.join("A.json"), decks.join("Alias.json")).unwrap();
            app.execute_command(crate::engine::EngineCommand::SetDeckOpacity {
                deck_uuid: deck.clone(),
                opacity: 0.5,
            });
            save_preset_as(&mut app, &deck, "Alias");
        }
        for _ in 0..2 {
            app.execute_command(crate::engine::EngineCommand::SaveChannelPreset {
                channel_uuid: channel.clone(),
                name: "Band".into(),
            });
        }
        app.flush_saves();
        let disk = crate::persistence::presets::PresetLibrary::load(&app.session.workspace);
        assert_eq!(presets_of(&app.session.preset_library), presets_of(&disk));
    }

    /// A preset whose file does not land is not kept: the library and the
    /// deck named after it are as they were — as a restart would find them —
    /// and it says so. Saved again before the first failed, the last to
    /// fail puts back what the disk has.
    #[test]
    fn a_preset_that_does_not_land_is_put_back() {
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        let deck = a_deck(&mut app);
        save_preset_as(&mut app, &deck, "Logo");
        app.flush_saves();
        assert_eq!(deck_name(&app, &deck), "Logo");
        let on_disk = presets_of(&app.session.preset_library);
        // From now these cannot be written: their temporary files are
        // folders.
        let decks = tmp.path().join(".varda/presets/decks");
        std::fs::create_dir(decks.join("Logo.tmp")).unwrap();
        std::fs::create_dir(decks.join("Fresh.tmp")).unwrap();
        app.slow_saves_for_test(SLOW_DISK);
        app.execute_command(crate::engine::EngineCommand::SetDeckOpacity {
            deck_uuid: deck.clone(),
            opacity: 0.25,
        });
        save_preset_as(&mut app, &deck, "Logo");
        save_preset_as(&mut app, &deck, "Logo");
        save_preset_as(&mut app, &deck, "Fresh");
        assert_eq!(deck_name(&app, &deck), "Fresh", "named at once");
        assert!(app
            .session
            .preset_library
            .deck_presets
            .iter()
            .any(|p| p.name == "Fresh"));
        app.flush_saves();
        assert_eq!(presets_of(&app.session.preset_library), on_disk);
        let disk = crate::persistence::presets::PresetLibrary::load(&app.session.workspace);
        assert_eq!(presets_of(&disk), on_disk);
        assert_eq!(deck_name(&app, &deck), "Logo");
        let said = notices(&app);
        assert!(
            said.iter().any(|m| m.starts_with("Failed to save preset:")),
            "{said:?}"
        );
        // Saved as two names from one deck, neither landing: the deck keeps
        // the name it had before both.
        std::fs::create_dir(decks.join("X.tmp")).unwrap();
        std::fs::create_dir(decks.join("Y.tmp")).unwrap();
        save_preset_as(&mut app, &deck, "X");
        save_preset_as(&mut app, &deck, "Y");
        app.flush_saves();
        assert_eq!(deck_name(&app, &deck), "Logo");
        assert_eq!(presets_of(&app.session.preset_library), on_disk);
    }

    /// A preset still on its way stays in the library when one before it
    /// lands and the library settles on what the disk has.
    #[test]
    fn a_preset_on_its_way_stays_while_one_before_it_lands() {
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        let deck = a_deck(&mut app);
        app.slow_saves_for_test(SLOW_DISK);
        save_preset_as(&mut app, &deck, "First");
        save_preset_as(&mut app, &deck, "Second");
        frames_until(&mut app, |app| {
            notices(app).contains(&"💾 Saved deck preset 'First'".to_string())
        });
        let names: Vec<String> = app
            .session
            .preset_library
            .deck_presets
            .iter()
            .map(|p| p.name.clone())
            .collect();
        assert_eq!(names, ["First", "Second"]);
        assert!(!tmp.path().join(".varda/presets/decks/Second.json").exists());
    }

    /// A preset whose writing breaks still has its folder read again: with
    /// one refused after it, both landing before a frame takes them in, the
    /// library is as the disk has it.
    #[test]
    fn a_preset_that_breaks_and_one_refused_after_it_leave_the_library_as_the_disk() {
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        let deck = a_deck(&mut app);
        save_preset_as(&mut app, &deck, "Keep");
        app.flush_saves();
        // Alone: nothing after it reads the folder again.
        app.break_files_for_test(1);
        save_preset_as(&mut app, &deck, "Alone");
        app.flush_saves();
        let disk = crate::persistence::presets::PresetLibrary::load(&app.session.workspace);
        assert_eq!(presets_of(&app.session.preset_library), presets_of(&disk));
        app.break_files_for_test(1);
        save_preset_as(&mut app, &deck, "Broken");
        app.show_writer.flush();
        app.refuse_files_for_test(1);
        save_preset_as(&mut app, &deck, "Refused");
        app.show_writer.flush();
        // Both landed; one frame takes them in.
        app.update_notifications();
        let disk = crate::persistence::presets::PresetLibrary::load(&app.session.workspace);
        assert_eq!(presets_of(&app.session.preset_library), presets_of(&disk));
        assert_eq!(disk.deck_presets.len(), 1, "only Keep was written");
    }

    /// A preset saved new, its file refused, and saved again at once in
    /// other letters, landing: the library has it as the disk does.
    #[test]
    fn a_preset_refused_then_saved_in_other_letters_is_as_the_disk_has_it() {
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        let deck = a_deck(&mut app);
        app.refuse_files_for_test(1);
        save_preset_as(&mut app, &deck, "Case");
        save_preset_as(&mut app, &deck, "case");
        app.flush_saves();
        let disk = crate::persistence::presets::PresetLibrary::load(&app.session.workspace);
        assert_eq!(presets_of(&app.session.preset_library), presets_of(&disk));
        assert_eq!(disk.deck_presets.len(), 1);
    }

    /// A deck renamed between two saves that do not land — taken out, and
    /// brought back by a preset under its own address and another name —
    /// gets back the name it had when the later save was made.
    #[test]
    fn a_deck_renamed_between_failed_saves_gets_back_its_newer_name() {
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        let deck = a_deck(&mut app);
        let channel = app.build_engine_state().mixer.channels[0].uuid.clone();
        save_preset_as(&mut app, &deck, "Alternate");
        save_preset_as(&mut app, &deck, "Second");
        app.flush_saves();
        assert_eq!(deck_name(&app, &deck), "Second");
        let decks = tmp.path().join(".varda/presets/decks");
        std::fs::create_dir(decks.join("X.tmp")).unwrap();
        std::fs::create_dir(decks.join("Y.tmp")).unwrap();
        app.slow_saves_for_test(SLOW_DISK);
        save_preset_as(&mut app, &deck, "X");
        assert!(matches!(
            app.execute_command(crate::engine::EngineCommand::RemoveDeck {
                deck_uuid: deck.clone(),
            }),
            crate::engine::CommandResult::Ok
        ));
        assert!(matches!(
            app.execute_command(crate::engine::EngineCommand::LoadDeckPreset {
                channel_uuid: channel,
                preset_name: "Alternate".into(),
            }),
            crate::engine::CommandResult::Ok
        ));
        assert_eq!(
            deck_name(&app, &deck),
            "Alternate",
            "back under its address"
        );
        save_preset_as(&mut app, &deck, "Y");
        app.flush_saves();
        assert_eq!(deck_name(&app, &deck), "Alternate");
    }

    /// A preset saved on air is kept at once — it loads straight away — and
    /// written in the background: the outputs do not wait for the disk, and
    /// "saved" is said once the file is there.
    #[test]
    fn a_preset_saved_on_air_does_not_wait_for_the_disk() {
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        let deck = a_deck(&mut app);
        let channel_uuid = app.build_engine_state().mixer.channels[0].uuid.clone();
        app.slow_saves_for_test(SLOW_DISK);
        let t = std::time::Instant::now();
        assert!(matches!(
            app.execute_command(crate::engine::EngineCommand::SaveDeckPreset {
                deck_uuid: deck,
                name: "Logo".into(),
            }),
            crate::engine::CommandResult::Ok
        ));
        let handed_over = t.elapsed();
        assert!(
            handed_over < std::time::Duration::from_millis(150),
            "the preset held the engine {handed_over:?}"
        );
        let file = tmp.path().join(".varda/presets/decks/Logo.json");
        assert!(!file.exists(), "not written yet");
        assert!(matches!(
            app.execute_command(crate::engine::EngineCommand::LoadDeckPreset {
                channel_uuid,
                preset_name: "Logo".into(),
            }),
            crate::engine::CommandResult::Ok
        ));
        app.update_notifications();
        assert!(!notices(&app)
            .iter()
            .any(|m| m.contains("Saved deck preset")));
        frames_until(&mut app, |app| {
            notices(app).contains(&"💾 Saved deck preset 'Logo'".to_string())
        });
        assert!(file.is_file());
    }

    /// A preset saved through the API is answered once its file is written;
    /// one that cannot be is answered with why, and said.
    #[test]
    fn an_api_preset_is_answered_once_its_file_is_written() {
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        let deck = a_deck(&mut app);
        let channel = app.build_engine_state().mixer.channels[0].uuid.clone();
        let sender = app.command_sender();
        app.slow_saves_for_test(SLOW_DISK);
        let (reply, mut answer) = tokio::sync::oneshot::channel();
        sender
            .send((
                crate::engine::EngineCommand::SaveDeckPreset {
                    deck_uuid: deck,
                    name: "Logo".into(),
                },
                Some(reply),
            ))
            .unwrap();
        app.process_commands();
        assert!(answer.try_recv().is_err(), "not before the file is written");
        let result = answer.blocking_recv().unwrap();
        assert!(
            matches!(result, crate::engine::CommandResult::Ok),
            "{result:?}"
        );
        assert!(tmp.path().join(".varda/presets/decks/Logo.json").is_file());

        app.slow_saves_for_test(std::time::Duration::ZERO);
        // A file where the channel presets' folder should be.
        std::fs::write(tmp.path().join(".varda/presets/channels"), "").unwrap();
        let (reply, answer) = tokio::sync::oneshot::channel();
        sender
            .send((
                crate::engine::EngineCommand::SaveChannelPreset {
                    channel_uuid: channel,
                    name: "Band".into(),
                },
                Some(reply),
            ))
            .unwrap();
        app.process_commands();
        match answer.blocking_recv().unwrap() {
            crate::engine::CommandResult::Err {
                code: crate::engine::ErrorCode::InternalError,
                message,
            } => assert!(
                message.starts_with("Failed to save preset: cannot make"),
                "{message}"
            ),
            other => panic!("{other:?}"),
        }
        app.update_notifications();
        let said = notices(&app);
        assert!(
            said.iter()
                .any(|m| m.starts_with("Failed to save preset: cannot make")),
            "{said:?}"
        );
        assert!(
            !said.iter().any(|m| m.contains("Saved channel preset")),
            "{said:?}"
        );
    }

    /// The panel arrangement is kept with the workspace, including one the
    /// UI changed after the load and a save that arrives through the API.
    #[test]
    fn the_screen_layout_survives_a_save_and_reload() {
        use crate::usecases::ui::screen_layout::{Builtin, Pane, ScreenLayout};
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        let mut screen = ScreenLayout::builtin(Builtin::Broadcast);
        crate::usecases::ui::screen_layout::remove_pane(&mut screen.dock, Pane::Setup);
        screen.locked = true;
        app.remember_layout(&UILayoutState {
            screen,
            collapsed_channels: vec!["ch000001".into()],
            ..Default::default()
        });
        app.execute_command(crate::engine::EngineCommand::SaveWorkspace);
        app.flush_saves();

        let Some(mut app2) = headless_app_in(tmp.path()) else {
            return;
        };
        let loaded = app2.load_workspace().layout.expect("layout");
        assert_eq!(loaded.screen.name, "Broadcast");
        assert!(loaded.screen.locked);
        assert!(loaded.screen.has(Pane::Monitors));
        assert!(!loaded.screen.has(Pane::Setup));
        assert_eq!(loaded.collapsed_channels, vec!["ch000001".to_string()]);
    }

    /// A stage from before layouts opens Classic.
    #[test]
    fn a_stage_without_a_screen_layout_opens_classic() {
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        app.save_workspace(&UILayoutState::default()).unwrap();
        let stage_path = tmp.path().join(".varda").join("stage.json");
        let mut stage: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&stage_path).unwrap()).unwrap();
        assert!(stage.get("screen_layout").is_some(), "written");
        stage.as_object_mut().unwrap().remove("screen_layout");
        std::fs::write(&stage_path, stage.to_string()).unwrap();
        let Some(mut app2) = headless_app_in(tmp.path()) else {
            return;
        };
        let loaded = app2.load_workspace().layout.expect("layout");
        assert_eq!(loaded.screen.name, "Classic");
    }

    #[test]
    fn apply_history_scene_crossfader() {
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        // Build a minimal SceneConfig with different crossfader
        let scene = crate::scene::SceneConfig {
            main: None,
            version: 3,
            channels: vec![
                crate::scene::ChannelConfig {
                    overlay: false,
                    uuid: crate::deck::generate_short_uuid(),
                    name: "Ch 0".into(),
                    opacity: 1.0,
                    blend_mode: crate::scene::BlendModeConfig::Normal,
                    decks: vec![],
                    effects: vec![],
                    modulation: vec![],
                },
                crate::scene::ChannelConfig {
                    overlay: false,
                    uuid: crate::deck::generate_short_uuid(),
                    name: "Ch 1".into(),
                    opacity: 1.0,
                    blend_mode: crate::scene::BlendModeConfig::Normal,
                    decks: vec![],
                    effects: vec![],
                    modulation: vec![],
                },
            ],
            crossfader: 0.42,
            active_transition: None,
            master_effects: vec![],
            modulation: crate::modulation::ModulationEngine::default(),
            macros: crate::macros::MacroBank::default(),
            transition_sequences: vec![],
            render_width: Some(1920),
            render_height: Some(1080),
            render_resolution_auto: false,
            frames_in_flight: None,
            free_control_screen: false,
            tonemap_mode: crate::renderer::tonemap::TonemapMode::default(),
            active_lut: None,
            arrangement: None,
            transport: crate::scene::TransportConfig::default(),
        };
        let before = app.mixer_snapshot().crossfader;
        let (warnings, _structural) = app.apply_history_scene(&scene, 1920, 1080);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        let snap = app.mixer_snapshot();

        // The crossfader is deliberately NOT applied by the diff, even though
        // the snapshot carries it.
        //
        // This assertion was inverted: it previously required the diff to apply
        // 0.42. `apply_history_scene` has exactly one production caller —
        // `restore_history_snapshot` — so "applied via diff" only ever means
        // "applied by undo/redo". `SetCrossfader` is denied a history entry as
        // live control, but that only stops it *creating* one; every snapshot
        // still records the position, so applying it here rewound the live
        // fader whenever an unrelated edit was undone. The transport position
        // and production buses already follow the same rule.
        assert!(
            (snap.crossfader - before).abs() < 1e-4,
            "undo must not move the live crossfader: {before} -> {}",
            snap.crossfader
        );
        assert!(
            (snap.crossfader - 0.42).abs() > 1e-4,
            "and specifically must not adopt the snapshot's 0.42"
        );
    }

    // ── The timecode patch in stage.json ────────────────────────

    /// Warnings the load raised, as the performer would read them.
    fn warnings(app: &super::super::VardaApp) -> Vec<String> {
        app.session
            .notifications
            .visible()
            .iter()
            .filter(|n| n.level == crate::notifications::NotificationLevel::Warning)
            .map(|n| n.message.clone())
            .collect()
    }

    /// The patch is written down as the name of a box, not the slot it happened
    /// to enumerate in. Ids are handed out at enumeration and shift whenever the
    /// rig changes between load-ins, so a saved id would point at whatever
    /// interface came up in that slot the next night.
    #[test]
    fn a_saved_ltc_patch_names_the_interface_rather_than_its_slot() {
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        let Some(device) = app.audio_manager.devices().first().cloned() else {
            return;
        };
        app.input
            .timecode
            .set_ltc_input(Some(crate::timecode::LtcInput {
                source_id: device.id,
                channel: 1,
                rate: Some(crate::transport::TimecodeRate::Fps2997),
            }));

        let saved = app.timecode_config().ltc_input.expect("a patched input");

        assert_eq!(saved.device, device.name);
        assert_eq!(
            saved.channel, 1,
            "which channel carries timecode is the patch"
        );
        assert_eq!(
            saved.rate,
            Some(crate::transport::TimecodeRate::Fps2997),
            "29.97 non-drop has to be named, so the naming must survive"
        );
    }

    /// The same patch coming back finds the interface by name, whatever id it
    /// holds tonight.
    #[test]
    fn a_saved_ltc_patch_is_restored_against_the_interface_of_that_name() {
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        let Some(device) = app.audio_manager.devices().last().cloned() else {
            return;
        };
        // Written by a previous load-in, when nobody recorded an id at all.
        let config = crate::timecode::TimecodeConfig {
            preference: crate::timecode::PreferenceConfig::Auto,
            ltc_input: Some(crate::timecode::LtcInputConfig {
                device: device.name.clone(),
                channel: 1,
                rate: None,
            }),
        };

        app.apply_timecode_config(&config);

        assert_eq!(
            app.input.timecode.ltc_input(),
            Some(crate::timecode::LtcInput {
                source_id: device.id,
                channel: 1,
                rate: None,
            })
        );
        assert!(warnings(&app).is_empty(), "the interface is right there");
    }

    /// A rig one interface short has to say so. Pointing the patch at whatever
    /// box now holds that slot would read timecode off the wrong cable, and
    /// saying nothing looks identical to a show that has not started.
    #[test]
    fn an_ltc_patch_naming_an_absent_interface_is_reported_and_left_unset() {
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        let config = crate::timecode::TimecodeConfig {
            preference: crate::timecode::PreferenceConfig::Auto,
            ltc_input: Some(crate::timecode::LtcInputConfig {
                device: "Scarlett 2i2 That Stayed Home".to_string(),
                channel: 1,
                rate: None,
            }),
        };

        app.apply_timecode_config(&config);

        assert_eq!(
            app.input.timecode.ltc_input(),
            None,
            "unset beats pointing at the wrong box"
        );
        assert!(
            warnings(&app)
                .iter()
                .any(|w| w.contains("Scarlett 2i2 That Stayed Home")),
            "the performer is told which interface is missing: {:?}",
            warnings(&app)
        );
    }

    /// Same for the MIDI port a forced MTC patch names: it falls back to
    /// following whatever arrives, and says that it did.
    #[test]
    fn a_forced_mtc_port_that_is_absent_is_reported_and_falls_back() {
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        let config = crate::timecode::TimecodeConfig {
            preference: crate::timecode::PreferenceConfig::ForceMtc {
                device: "Tascam DA-6400 On The Other Truck".to_string(),
            },
            ltc_input: None,
        };

        app.apply_timecode_config(&config);

        assert_eq!(
            app.input.timecode.preference(),
            crate::timecode::TimecodePreference::Auto
        );
        assert!(
            warnings(&app)
                .iter()
                .any(|w| w.contains("Tascam DA-6400 On The Other Truck")),
            "the performer is told which port is missing: {:?}",
            warnings(&app)
        );
    }

    /// A MIDI port that is present is found by name and forced by id, which is
    /// the whole reason `midi.json` and the timecode patch both store names.
    #[test]
    fn a_forced_mtc_port_that_is_present_is_restored_by_name() {
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        let Some((id, name)) = app
            .input
            .midi_devices
            .as_ref()
            .and_then(|midi| midi.devices.values().next())
            .map(|device| (device.id, device.name.clone()))
        else {
            return;
        };
        let config = crate::timecode::TimecodeConfig {
            preference: crate::timecode::PreferenceConfig::ForceMtc { device: name },
            ltc_input: None,
        };

        app.apply_timecode_config(&config);

        assert_eq!(
            app.input.timecode.preference(),
            crate::timecode::TimecodePreference::ForceMtc { device_id: id }
        );
        assert!(warnings(&app).is_empty());
    }

    /// A Blackmagic output running when the stage was saved is started again
    /// as the stage opens — a third screen comes back with the show. Its
    /// device not there, it stays stopped, waits for the device, and is still
    /// saved running; one saved stopped is not started.
    #[test]
    fn a_blackmagic_output_saved_running_is_started_again_as_the_stage_opens() {
        use crate::renderer::context::UnifiedOutput;
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        // A device no machine has: no test ever opens a real one.
        let config = |name: &str, running| crate::scene::OutputConfig {
            name: name.into(),
            target: crate::scene::OutputTargetConfig::DeckLink {
                device: "No Such Blackmagic Device".into(),
                mode: "1080p60".into(),
                full_range: false,
                embedded_audio: false,
            },
            running,
            ..crate::scene::OutputConfig::default_windowed()
        };
        let told = |app: &super::super::VardaApp| {
            app.session.notifications.visible().iter().any(|n| {
                n.level == crate::notifications::NotificationLevel::Warning
                    && n.message.contains("'No Such Blackmagic Device' is not connected")
                    && n.message.contains("it starts once it is")
            })
        };
        let stopped = config("Stopped", false);
        app.restore_headless_output(
            &stopped,
            &stopped.name,
            crate::persistence::config_to_target_pub(&stopped.target),
        );
        assert!(!told(&app), "an output saved stopped is not started");
        let running = config("Box", true);
        app.restore_headless_output(
            &running,
            &running.name,
            crate::persistence::config_to_target_pub(&running.target),
        );
        // Discovery tells of a device a moment after the stage opens: not
        // said missing at once, but if it stays so.
        assert!(!told(&app), "not said missing at once");
        app.tell_outputs_still_waiting(std::time::Instant::now() + std::time::Duration::from_secs(1));
        assert!(!told(&app), "nor a second on");
        app.tell_outputs_still_waiting(std::time::Instant::now() + std::time::Duration::from_secs(6));
        assert!(told(&app), "said missing once it stays so");
        let state = app.build_engine_state();
        let made: Vec<(&str, bool)> = state
            .outputs
            .windows
            .iter()
            .map(|w| (w.name.as_str(), w.is_active))
            .collect();
        assert_eq!(made, [("Stopped", false), ("Box", false)]);
        let saved: Vec<bool> = app
            .output
            .outputs
            .iter()
            .map(|o| crate::persistence::output_config(o).running)
            .collect();
        assert_eq!(saved, [false, true], "still saved running while it waits");
        let waiting = |app: &super::super::VardaApp| {
            app.output
                .outputs
                .iter()
                .map(|o| matches!(o, UnifiedOutput::Headless(h) if h.keep_running && !h.active))
                .collect::<Vec<_>>()
        };
        assert_eq!(waiting(&app), [false, true]);
    }

    /// An output kept running but stopped — its Blackmagic device missing
    /// as the stage opened, or unplugged — is started again when the device
    /// appears among those connected: when it appears, not every look, so a
    /// device that will not start is not tried again and again. Stopped by
    /// hand, it is not started again, and is saved stopped.
    #[test]
    fn an_output_kept_running_starts_again_when_its_blackmagic_device_appears() {
        use crate::renderer::context::UnifiedOutput;
        let tmp = TempDir::new().unwrap();
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        let config = |name: &str| crate::scene::OutputConfig {
            name: name.into(),
            target: crate::scene::OutputTargetConfig::DeckLink {
                device: "No Such Blackmagic Device".into(),
                mode: "1080p60".into(),
                full_range: false,
                embedded_audio: false,
            },
            running: true,
            ..crate::scene::OutputConfig::default_windowed()
        };
        for name in ["Box", "Other"] {
            let c = config(name);
            app.restore_headless_output(&c, &c.name, crate::persistence::config_to_target_pub(&c.target));
        }
        let waiting = |app: &super::super::VardaApp| {
            app.output
                .outputs
                .iter()
                .map(|o| matches!(o, UnifiedOutput::Headless(h) if h.keep_running && !h.active))
                .collect::<Vec<_>>()
        };
        assert_eq!(waiting(&app), [true, true]);
        // All given, not the few on screen at once.
        let warnings = |app: &super::super::VardaApp| {
            app.session
                .notifications
                .history()
                .iter()
                .filter(|n| n.level == crate::notifications::NotificationLevel::Warning)
                .count()
        };
        let devices = |names: &[&str]| -> Vec<crate::decklink::DeckLinkDevice> {
            names
                .iter()
                .map(|n| crate::decklink::DeckLinkDevice {
                    name: (*n).into(),
                    arrival: 0,
            modes: vec![],
                })
                .collect()
        };
        let before = warnings(&app);
        // Other devices come and go: nothing tried.
        app.start_outputs_whose_device_is_back(&devices(&["Some Other Box"]));
        assert_eq!(warnings(&app), before);
        // Its device appears: both started — here they still cannot open,
        // and say so, and are kept running, waiting.
        app.start_outputs_whose_device_is_back(&devices(&["No Such Blackmagic Device"]));
        assert_eq!(warnings(&app), before + 2);
        assert_eq!(waiting(&app), [true, true]);
        // Still there at the next look: not tried again.
        app.start_outputs_whose_device_is_back(&devices(&["No Such Blackmagic Device"]));
        assert_eq!(warnings(&app), before + 2);
        // Unplugged and plugged in again: tried again.
        app.start_outputs_whose_device_is_back(&devices(&[]));
        app.start_outputs_whose_device_is_back(&devices(&["No Such Blackmagic Device"]));
        assert_eq!(warnings(&app), before + 4);
        // Stopped by hand: not kept running, saved stopped, not tried again.
        let uuid = app.output.outputs[1].uuid().to_string();
        assert!(matches!(app.cmd_stop_output(&uuid), crate::engine::CommandResult::Ok));
        assert_eq!(waiting(&app), [true, false]);
        assert!(!crate::persistence::output_config(&app.output.outputs[1]).running);
        app.start_outputs_whose_device_is_back(&devices(&[]));
        app.start_outputs_whose_device_is_back(&devices(&["No Such Blackmagic Device"]));
        assert_eq!(warnings(&app), before + 5, "only the one kept running");
    }
}
