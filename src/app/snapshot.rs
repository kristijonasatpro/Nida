//! Snapshot builder — constructs the framework-free `EngineState` from live
//! `VardaApp` state. Presentation mapping (`UIData`, which names `egui::TextureId`)
//! lives in `usecases::ui::snapshot` — see `/spec/app-presentation-boundary.md`.
//!
//! PERF: Every frame clones the full `EngineState` (params, effects, FFT data,
//! modulation assignments). At 8+ decks with effects, this is dozens of heap
//! allocations per frame. Not a bottleneck at 60fps with current deck counts,
//! but worth profiling if deck/effect counts grow significantly (16+ decks).
//! Mitigation options: dirty-flag retained snapshots, arena allocation, or
//! COW wrappers on heavy fields.

use super::VardaApp;
use crate::channel::{DeckTransitionPhase, DurationSpec, TransitionTrigger};
use crate::engine::types::{
    AutoTransitionSnapshot, CameraSnapshot, ChannelSnapshot, ClockSnapshot, DeckSnapshot,
    DepthPreproParamsSnapshot, EffectSnapshot, EngineState, MidiDeviceSnapshot,
    MidiMappingSnapshot, MidiSnapshot, MixerSnapshot, ParamSnapshot, PointCloudParamsSnapshot,
    RegistrySnapshot, RunningAnalyzerSnapshot, ScreenCaptureDeckSnapshot, SequenceSnapshot,
    SequenceStepKindSnapshot, SequenceStepSnapshot, ShaderParamsSnapshot, TapDeckSnapshot,
    VideoPlaybackSnapshot,
};

/// Build a `MixerSnapshot` from the current `VardaApp` state.
pub(crate) fn build_mixer_snapshot(app: &VardaApp) -> MixerSnapshot {
    let mixer = &app.mixer;
    let channel_labels = app.channel_labels();

    let channels = mixer
        .channels()
        .iter()
        .enumerate()
        .map(|(ch_idx, ch)| {
            let decks = ch
                .decks
                .iter()
                .enumerate()
                .map(|(deck_idx, slot)| {
                    let gen_params =
                        build_shader_params(slot.deck.source_name(), &slot.deck.generator_params);
                    let effects = slot
                        .deck
                        .effects
                        .iter()
                        .map(|e| EffectSnapshot {
                            uuid: e.uuid().to_owned(),
                            name: e.shader.name(),
                            enabled: e.enabled,
                            load_state: e.load_state(),
                            params: build_shader_params(&e.shader.name(), &e.params),
                            mask: e.mask.clone(),
                        })
                        .collect();

                    let video_playback =
                        slot.deck
                            .playback_snapshot()
                            .map(|ps| VideoPlaybackSnapshot {
                                playing: ps.playing,
                                position: ps.position,
                                duration: ps.duration,
                                speed: ps.speed,
                                effective_speed: ps.effective_speed,
                                position_offset: ps.position_offset,
                                loop_mode: ps.loop_mode,
                                in_point: ps.in_point,
                                out_point: ps.out_point,
                                frame_rate: ps.frame_rate,
                                transport_sync: slot
                                    .deck
                                    .video_transport_sync()
                                    .unwrap_or_default(),
                                hardware_decode: ps.hardware_decode,
                                auto: slot.deck.auto_playback,
                                sound_only: slot.deck.is_sound_only(),
                            });

                    let auto_transition =
                        slot.auto_transition
                            .as_ref()
                            .map(|at| AutoTransitionSnapshot {
                                enabled: at.enabled,
                                trigger_is_clip_end: at.trigger == TransitionTrigger::ClipEnd,
                                play_duration_value: at.play_duration.value(),
                                play_duration_is_beats: matches!(
                                    at.play_duration,
                                    DurationSpec::Beats(_)
                                ),
                                transition_duration_value: at.transition_duration.value(),
                                transition_duration_is_beats: matches!(
                                    at.transition_duration,
                                    DurationSpec::Beats(_)
                                ),
                                transition_shader_name: at.transition_shader_name.clone(),
                                phase: at.phase,
                            });

                    let point_cloud_params = matches!(
                        slot.deck.external_source_kind(),
                        Some(crate::deck::ExternalSourceKind::DepthSensor(_))
                    )
                    .then(|| {
                        let p = |name: &str| slot.deck.depth_param(name).unwrap_or_default();
                        PointCloudParamsSnapshot {
                            orbit_yaw: p("orbit_yaw"),
                            orbit_pitch: p("orbit_pitch"),
                            zoom: p("zoom"),
                            point_size: p("point_size"),
                            depth_min: p("depth_min"),
                            depth_max: p("depth_max"),
                            seed: p("seed"),
                            drift: p("drift"),
                            disruption: p("disruption"),
                            color_mode: slot.deck.point_cloud_params.color_mode.as_u8(),
                        }
                    });

                    let depth_prepro_params =
                        slot.deck
                            .depth_prepro
                            .as_ref()
                            .map(|s| DepthPreproParamsSnapshot {
                                sensor_name: s.sensor_name.clone(),
                                near: normalized_prepro_param(&s.params, "near"),
                                far: normalized_prepro_param(&s.params, "far"),
                                smoothing: normalized_prepro_param(&s.params, "smoothing"),
                                hole_fill: normalized_prepro_param(&s.params, "hole_fill"),
                                mask_feather: normalized_prepro_param(&s.params, "mask_feather"),
                                motion_gain: normalized_prepro_param(&s.params, "motion_gain"),
                                mirror: s.params.mirror,
                            });

                    let screen_capture = slot.deck.screen_capture.as_ref().map(|s| {
                        use crate::screen_capture::backend::{MAX_CAPTURE_RATE, MIN_CAPTURE_RATE};
                        ScreenCaptureDeckSnapshot {
                            target_label: crate::scene::CaptureTargetConfig::from(&s.identity)
                                .label(),
                            is_display: matches!(
                                s.identity,
                                crate::screen_capture::backend::TargetIdentity::Display { .. }
                            ),
                            // Normalized so the UI slider and the MIDI router
                            // speak the same units on the same path.
                            rate_norm: ((s.config.rate - MIN_CAPTURE_RATE)
                                / (MAX_CAPTURE_RATE - MIN_CAPTURE_RATE))
                                .clamp(0.0, 1.0),
                            rate_fps: s.config.rate,
                            crop: [
                                s.config.crop.x,
                                s.config.crop.y,
                                s.config.crop.w,
                                s.config.crop.h,
                            ],
                            show_cursor: s.config.show_cursor,
                            exclude_varda: s.config.exclude_varda,
                            bound: s.capture_id != crate::screen_capture::UNBOUND_CAPTURE_ID,
                            connected: app.screen_capture_manager().is_connected(s.capture_id),
                        }
                    });

                    let tap = slot.deck.tap.as_ref().map(|t| {
                        use crate::deck::TapSource;
                        TapDeckSnapshot {
                            kind: match t.source {
                                TapSource::MasterProgram => "master_program".into(),
                                TapSource::Channel(_) => "channel".into(),
                            },
                            channel_uuid: match &t.source {
                                TapSource::MasterProgram => None,
                                TapSource::Channel(uuid) => Some(uuid.clone()),
                            },
                            label: t.source.label(&channel_labels),
                            bound: match &t.source {
                                TapSource::MasterProgram => true,
                                TapSource::Channel(uuid) => {
                                    channel_labels.iter().any(|(u, _)| u == uuid)
                                }
                            },
                        }
                    });

                    let effective_opacity = match slot.transition_phase() {
                        DeckTransitionPhase::Transitioning { progress } => {
                            slot.opacity * (1.0 - progress as f32)
                        }
                        _ => slot.opacity,
                    };

                    DeckSnapshot {
                        idx: deck_idx,
                        uuid: slot.deck.uuid().to_string(),
                        name: slot.deck.source_name().to_string(),
                        is_html: matches!(
                            slot.deck.external_source_kind(),
                            Some(crate::deck::ExternalSourceKind::Html(_))
                        ),
                        is_depth_sensor: matches!(
                            slot.deck.external_source_kind(),
                            Some(crate::deck::ExternalSourceKind::DepthSensor(_))
                        ),
                        point_cloud_params,
                        has_depth_prepro: slot.deck.depth_prepro.is_some(),
                        depth_prepro_params,
                        screen_capture,
                        tap,
                        playlist: slot
                            .deck
                            .playlist()
                            .map(crate::deck::playlist::Playlist::snapshot),
                        timer: slot.deck.timer().map(|timer| {
                            timer.snapshot(crate::engine::value::timer::panel_of(slot.deck.uuid()))
                        }),
                        camera_colour: (slot.deck.source_type() == "camera")
                            .then_some(slot.deck.camera_colour),
                        camera_profile: (slot.deck.source_type() == "camera")
                            .then(|| slot.deck.camera_profile.clone()),
                        camera_lowest_latency: (slot.deck.source_type() == "camera")
                            .then_some(slot.deck.camera_lowest_latency),
                        is_html_interactive: {
                            #[cfg(feature = "html")]
                            {
                                app.interactive_active_deck() == Some(slot.deck.uuid())
                            }
                            #[cfg(not(feature = "html"))]
                            {
                                false
                            }
                        },
                        opacity: slot.opacity,
                        effective_opacity,
                        blend_mode: slot.blend_mode,
                        solo: slot.solo,
                        mute: slot.mute,
                        transparent: slot.deck.transparent(),
                        scaling_mode: slot.deck.scaling_mode(),
                        source_size: slot.deck.source_size(),
                        solid_color: slot.deck.solid_color(),
                        generator: gen_params,
                        effects,
                        video_playback,
                        auto_transition,
                        render_fps: slot.render_fps,
                        render_scale: slot.deck.render_scale(),
                        effective_render_fps: if slot.render_cost_us > 0.0 {
                            1_000_000.0 / slot.render_cost_us
                        } else {
                            0.0
                        },
                        render_cost_us: slot.render_cost_us,
                        gpu_render_cost_us: slot.gpu_render_cost_us,
                        fps: slot.deck.fps(),
                        still: slot.deck.still_frame(mixer.modulation()).is_some(),
                        source_asleep: !slot.source_demand.wants_frames(),
                        running_analyzers: slot
                            .deck
                            .analyzers
                            .running_types()
                            .into_iter()
                            .map(|t| RunningAnalyzerSnapshot { analyzer_type: t })
                            .collect(),
                    }
                })
                .collect();

            let ch_effects = ch
                .effects
                .iter()
                .map(|e| EffectSnapshot {
                    uuid: e.uuid().to_owned(),
                    name: e.shader.name(),
                    enabled: e.enabled,
                    load_state: e.load_state(),
                    params: build_shader_params(&e.shader.name(), &e.params),
                    mask: e.mask.clone(),
                })
                .collect();

            ChannelSnapshot {
                idx: ch_idx,
                uuid: ch.uuid().to_string(),
                name: ch.name.clone(),
                opacity: ch.opacity,
                blend_mode: ch.blend_mode,
                decks,
                effects: ch_effects,
                render_time_ms: ch.render_time_ms,
                active_deck_count: ch.active_deck_count,
                overlay: ch.overlay,
            }
        })
        .collect();

    let master_effects = mixer
        .master_effects()
        .iter()
        .map(|e| EffectSnapshot {
            uuid: e.uuid().to_owned(),
            name: e.shader.name(),
            enabled: e.enabled,
            load_state: e.load_state(),
            params: build_shader_params(&e.shader.name(), &e.params),
            mask: e.mask.clone(),
        })
        .collect();

    let auto_crossfade_active = mixer.is_crossfading();
    let auto_crossfade_progress = mixer
        .auto_crossfade()
        .as_ref()
        .map_or(0.0, |a| a.progress());

    let transition_names = app
        .registry
        .transitions()
        .iter()
        .map(|s| s.name())
        .collect();
    let active_transition_name = mixer.active_transition().as_ref().map(|t| t.name.clone());

    let sequences = build_sequence_snapshots(mixer);

    MixerSnapshot {
        channels,
        crossfader: mixer.crossfader(),
        auto_crossfade_active,
        auto_crossfade_progress,
        master_effects,
        active_transition_name,
        transition_names,
        sequences,
        tonemap_mode: mixer.tonemap_mode(),
        active_lut: mixer
            .active_lut_filename()
            .map(std::string::ToString::to_string),
        main: crate::mixer::MainSnapshot::of(mixer.mix_mode(), mixer.main()),
    }
}

fn build_shader_params(
    shader_name: &str,
    params: &crate::params::ShaderParams,
) -> ShaderParamsSnapshot {
    let params_vec = params
        .param_order
        .iter()
        .filter_map(|name| {
            let value = params.values.get(name)?;
            let def = params.definitions.get(name);
            Some(ParamSnapshot {
                name: name.clone(),
                label: def.and_then(|d| d.label.clone()),
                value: *value,
                min: def.and_then(|d| d.min),
                max: def.and_then(|d| d.max),
            })
        })
        .collect();

    ShaderParamsSnapshot {
        shader_name: shader_name.to_string(),
        params: params_vec,
    }
}

/// Normalized (`0..1`) value of a depth-preprocessor param. `name` is one of the
/// router names in `src/internal/depth/preprocess.rs`; unknown names read as `0`.
fn normalized_prepro_param(
    params: &crate::depth::preprocess::DepthPreprocessParams,
    name: &str,
) -> f32 {
    params.normalized_param(name).unwrap_or_default()
}

fn build_sequence_snapshots(mixer: &crate::mixer::Mixer) -> Vec<SequenceSnapshot> {
    let channel_names: std::collections::HashMap<&str, &str> = mixer
        .channels()
        .iter()
        .map(|c| (c.uuid(), c.name.as_str()))
        .collect();
    mixer
        .transition_sequences()
        .iter()
        .map(|seq| {
            let steps = seq
                .steps
                .iter()
                .map(|step| {
                    let (label, kind) = match &step.kind {
                        crate::mixer::StepKind::Fade {
                            from_ch,
                            to_ch,
                            duration,
                            easing,
                            transition_shader,
                            target_amount,
                        } => {
                            let unit_label = duration.unit().label();
                            let easing_name = format!("{easing:?}");
                            let label = format!(
                                "Fade {} -> {} ({:.1}{})",
                                channel_names.get(from_ch.as_str()).copied().unwrap_or("?"),
                                channel_names.get(to_ch.as_str()).copied().unwrap_or("?"),
                                duration.value(),
                                unit_label
                            );
                            (
                                label,
                                SequenceStepKindSnapshot::Fade {
                                    from_ch: from_ch.clone(),
                                    to_ch: to_ch.clone(),
                                    duration_val: duration.value(),
                                    duration_unit: duration.unit(),
                                    easing: easing_name,
                                    transition_shader: transition_shader.clone(),
                                    target_amount: *target_amount,
                                },
                            )
                        }
                        crate::mixer::StepKind::Wait { duration } => {
                            let unit_label = duration.unit().label();
                            let label = format!("Wait {:.1}{}", duration.value(), unit_label);
                            (
                                label,
                                SequenceStepKindSnapshot::Wait {
                                    duration_val: duration.value(),
                                    duration_unit: duration.unit(),
                                },
                            )
                        }
                        crate::mixer::StepKind::GoTo { step_index } => {
                            let label = format!("GoTo step {step_index}");
                            (
                                label,
                                SequenceStepKindSnapshot::GoTo {
                                    step_index: *step_index,
                                },
                            )
                        }
                    };
                    SequenceStepSnapshot { label, kind }
                })
                .collect();
            SequenceSnapshot {
                uuid: seq.uuid.clone(),
                name: seq.name.clone(),
                enabled: seq.enabled,
                playing: seq.state.playing,
                current_step: seq.state.current_step,
                step_elapsed: seq.state.step_elapsed,
                steps,
            }
        })
        .collect()
}

/// Build a `RegistrySnapshot` from the current `VardaApp` state.
pub(crate) fn build_registry_snapshot(app: &VardaApp) -> RegistrySnapshot {
    let mut generators: Vec<(String, usize)> = app
        .registry
        .generators()
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name(), i))
        .collect();
    generators.sort_by_key(|a| a.0.to_lowercase());
    // A layer's own mask is made from Input Settings, not dropped from the
    // Library: alone, it would just make the layer vanish.
    let mut filters: Vec<(String, usize)> = app
        .registry
        .filters()
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name(), i))
        .filter(|(name, _)| name != crate::engine::value::mask::LAYER_MASK_EFFECT)
        .collect();
    filters.sort_by_key(|a| a.0.to_lowercase());
    RegistrySnapshot {
        generators,
        filters,
        shader_count: app.registry.count(),
    }
}

/// Build a `MidiSnapshot` from the current `VardaApp` state.
pub(crate) fn build_midi_snapshot(app: &VardaApp) -> MidiSnapshot {
    let devices = app
        .input
        .midi_devices
        .as_ref()
        .map(|mgr| {
            mgr.device_list()
                .iter()
                .map(|d| MidiDeviceSnapshot {
                    id: d.id,
                    name: d.name.clone(),
                    enabled: d.enabled,
                    has_output: d.has_output,
                    profile: d.profile_name().to_string(),
                })
                .collect()
        })
        .unwrap_or_default();

    let mappings = {
        let sorted = app.input.midi_mappings.sorted_mappings();
        sorted
            .iter()
            .map(|(key, path)| {
                let dev_name = app
                    .input
                    .midi_devices
                    .as_ref()
                    .and_then(|mgr| mgr.device(key.device_id()))
                    .map_or_else(|| format!("Device {}", key.device_id()), |d| d.name.clone());
                MidiMappingSnapshot {
                    key: *key,
                    key_display: format!("{key}"),
                    device_name: dev_name,
                    param_path: path.clone(),
                }
            })
            .collect()
    };

    MidiSnapshot {
        devices,
        mappings,
        learn_active: app.input.midi_mappings.learn_mode,
        learn_target: app.input.midi_mappings.learn_target.clone(),
    }
}

/// Build a `CameraSnapshot` from the current `VardaApp` state.
pub(crate) fn build_camera_snapshot(app: &VardaApp) -> CameraSnapshot {
    CameraSnapshot {
        devices: app
            .camera_manager
            .devices()
            .iter()
            .map(|d| (d.name.clone(), d.id))
            .collect(),
        modes: app
            .camera_manager
            .devices()
            .iter()
            .map(|d| (d.id, d.modes()))
            .collect(),
        access_denied: app.camera_manager.access_denied(),
        video_effects: app
            .camera_manager
            .video_effects()
            .iter()
            .map(|e| (*e).to_string())
            .collect(),
    }
}

/// Build a `DepthSensorSnapshot` from the current `VardaApp` state.
pub(crate) fn build_depth_sensor_snapshot(app: &VardaApp) -> crate::engine::DepthSensorSnapshot {
    crate::engine::DepthSensorSnapshot {
        devices: app
            .depth_manager()
            .devices()
            .iter()
            .map(|d| (d.name.clone(), d.id))
            .collect(),
    }
}

/// Build a `ScreenCaptureSnapshot` from the current `VardaApp` state.
pub(crate) fn build_screen_capture_snapshot(
    app: &VardaApp,
) -> crate::engine::ScreenCaptureSnapshot {
    let mgr = app.screen_capture_manager();
    crate::engine::ScreenCaptureSnapshot {
        targets: mgr
            .targets()
            .iter()
            .map(|t| crate::engine::CaptureTargetSnapshot {
                kind: t.kind.as_str().to_string(),
                label: t.label.clone(),
                app: t.app.clone(),
                title: t.title.clone(),
                width: t.width,
                height: t.height,
                is_varda: t.is_varda,
            })
            .collect(),
        permission: mgr.permission_state().as_str().to_string(),
        available: mgr.is_available(),
        backend: mgr.backend_name().to_string(),
        active_captures: mgr.active_ids().len(),
    }
}

/// Build a `ClockSnapshot` from the current clock manager state.
pub(crate) fn build_clock_snapshot(app: &VardaApp) -> ClockSnapshot {
    use crate::engine::types::DetectedClockSourceSnapshot;

    let clock = app.input.clock_manager.state();
    let (source_label, device_name) = match &clock.source {
        crate::clock::ClockSource::Audio => ("Audio".to_string(), None),
        crate::clock::ClockSource::MidiClock { device_name, .. } => {
            ("MIDI".to_string(), Some(device_name.clone()))
        }
        crate::clock::ClockSource::OscClock => ("OSC".to_string(), None),
        crate::clock::ClockSource::Manual => ("Manual".to_string(), None),
    };

    let detected_midi_sources = app
        .input
        .clock_manager
        .detected_midi_sources()
        .into_iter()
        .map(|s| DetectedClockSourceSnapshot {
            device_id: s.device_id,
            device_name: s.device_name,
            bpm: s.bpm,
        })
        .collect();

    let preference = app.input.clock_manager.preference();
    let (preference_label, preference_force_device_id) = match preference {
        crate::clock::ClockPreference::Auto => ("Auto".to_string(), None),
        crate::clock::ClockPreference::ForceMidi { device_id } => {
            (format!("ForceMidi({device_id})"), Some(*device_id))
        }
        crate::clock::ClockPreference::ForceOsc => ("ForceOsc".to_string(), None),
        crate::clock::ClockPreference::ForceAudio => ("ForceAudio".to_string(), None),
        crate::clock::ClockPreference::ForceManual { .. } => ("ForceManual".to_string(), None),
    };

    ClockSnapshot {
        bpm: if clock.active { Some(clock.bpm) } else { None },
        beat_phase: clock.beat_phase,
        source_label,
        device_name,
        active: clock.active,
        detected_midi_sources,
        osc_active: app.input.clock_manager.osc_active(),
        osc_bpm: app.input.clock_manager.osc_bpm(),
        audio_bpm: if clock.active && matches!(clock.source, crate::clock::ClockSource::Audio) {
            Some(clock.bpm)
        } else {
            None
        },
        preference_label,
        preference_force_device_id,
        manual_bpm: app.input.clock_manager.manual_bpm(),
        beat_followers: app
            .mixer
            .modulation()
            .followers_of(crate::timebase::Timebase::Beat),
    }
}

/// Build the transport snapshot, adding what the `From` impl cannot see: the
/// follower count, which lives in the modulation engine, and the record state,
/// which is the session's.
pub(crate) fn build_transport_snapshot(app: &VardaApp) -> crate::engine::types::TransportSnapshot {
    let mut snapshot: crate::engine::types::TransportSnapshot = (&app.transport).into();
    snapshot.followers = app
        .mixer
        .modulation()
        .followers_of(crate::timebase::Timebase::Transport);
    snapshot.record_armed = app.record_armed();
    snapshot.recording_params = app.recording_params();
    snapshot
}

/// Build the timecode diagnostics: every input heard, and which one drives.
pub(crate) fn build_timecode_snapshot(app: &VardaApp) -> crate::engine::types::TimecodeSnapshot {
    let manager = &app.input.timecode;
    crate::engine::types::TimecodeSnapshot {
        inputs: manager
            .inputs()
            .iter()
            .map(|input| crate::engine::types::TimecodeInputSnapshot {
                key: input.source.key(),
                label: input.source.label(),
                position: input.position,
                timecode: input.label(),
                rate: input.rate,
                running: input.running,
                freewheeling: input.freewheeling,
                speed: input.speed,
            })
            .collect(),
        resolved: manager.resolved_key(),
        preference: manager.preference(),
        ltc_input: manager.ltc_input(),
    }
}

/// Build the arrangement snapshot, or `None` for a Performance-only scene.
pub(crate) fn build_arrangement_snapshot(
    app: &VardaApp,
) -> Option<crate::engine::types::ArrangementSnapshot> {
    let config = app.mixer.arrangement()?;
    Some(crate::engine::types::ArrangementSnapshot {
        engaged: app.arrangement_authority().is_engaged(),
        overridden_params: app
            .mixer
            .modulation()
            .overridden_params()
            .map(String::from)
            .collect(),
        duration: config.duration(),
        config: config.clone(),
    })
}

/// Build a full `EngineState` from all subsystem snapshots.
/// Snapshot every production bus, in configured order.
fn build_bus_snapshots(app: &VardaApp) -> Vec<crate::engine::types::BusSnapshot> {
    use crate::engine::types::{BusSnapshot, BusTransitionSnapshot};
    app.production_buses()
        .iter()
        .map(|bus| BusSnapshot {
            uuid: bus.uuid().to_string(),
            name: bus.name.clone(),
            preview: bus.preview().clone(),
            program: bus.program().clone(),
            preview_resolved: bus.preview_resolved(),
            program_resolved: bus.program_resolved(),
            canvas: bus.canvas(),
            audio_mix: bus.audio_mix().clone(),
            auto_duration: bus.auto_duration,
            hidden_layers: bus.hidden_layers().to_vec(),
            transition: bus.transition().map(|t| BusTransitionSnapshot {
                from: t.from.clone(),
                to: t.to.clone(),
                duration: t.duration as f32,
                progress: t.progress(),
                mix: t.mix(),
                from_resolved: t.from_resolved,
                to_resolved: t.to_resolved,
            }),
            screen: app.bus_screen(bus.uuid()),
        })
        .collect()
}

/// How evenly frames have been started, and each output window's presents.
fn build_pacing_snapshot(app: &VardaApp) -> crate::engine::PacingSnapshot {
    let stats = &app.frame_stats;
    // The first interval after a pause (a launch, a dialog) is not a frame.
    let intervals: Vec<f32> = stats
        .intervals
        .iter()
        .copied()
        .filter(|ms| *ms > 0.0 && *ms < 1000.0)
        .collect();
    let count = intervals.len().max(1) as f32;
    let mean = intervals.iter().sum::<f32>() / count;
    let variance = intervals.iter().map(|ms| (ms - mean).powi(2)).sum::<f32>() / count;
    let frame_ms = if app.target_fps > 0 {
        1000.0 / app.target_fps as f32
    } else {
        mean
    };
    let outputs = app
        .output
        .outputs
        .iter()
        .filter_map(|output| match output {
            crate::renderer::context::UnifiedOutput::Window(w) => {
                Some(crate::engine::OutputPacing {
                    name: w.name.clone(),
                    presents: w.presents,
                    pace_skipped: w.pace_skipped,
                    unavailable: w.unavailable,
                    follows_clock: w.clock_display.is_some() && w.follows_clock(),
                    on_screen: w.on_screen.stats(),
                })
            }
            crate::renderer::context::UnifiedOutput::Headless(_) => None,
        })
        .collect();
    #[allow(clippy::cast_possible_truncation)]
    let inputs = app
        .camera_manager
        .picture_delays()
        .into_iter()
        .chain(app.screen_capture_manager.picture_delays())
        .map(|(name, behind)| crate::engine::InputPacing {
            name,
            behind_ms: (behind * 1000.0) as f32,
        })
        .collect();
    crate::engine::PacingSnapshot {
        clock: stats.clock.clone(),
        frames_in_flight: app.frames_in_flight(),
        frame_pacing: app.frame_pacing(),
        interval_mean_ms: mean,
        interval_stdev_ms: variance.sqrt(),
        interval_min_ms: intervals
            .iter()
            .copied()
            .fold(f32::INFINITY, f32::min)
            .min(mean),
        interval_max_ms: intervals.iter().copied().fold(0.0, f32::max),
        intervals: u32::try_from(intervals.len()).unwrap_or(u32::MAX),
        late_frames: u32::try_from(intervals.iter().filter(|ms| **ms > frame_ms * 1.5).count())
            .unwrap_or(u32::MAX),
        outputs,
        inputs,
        parts: stats.parts.stats(),
        slow_frames: stats.parts.slow_frames(),
        clock_timestamps: app.clock_timestamps(),
        frame_hold: app.frame_hold(),
        control_vsync: app.control_vsync(),
        control_queue: stats.control_queue.clone(),
        display_link: stats.display_link.clone(),
        control_on_screen: stats.control_on_screen.clone(),
        control_parts: stats.control_parts.clone(),
        control_slow_frames: stats.control_slow_frames.clone(),
        gpu_busy_pct: stats.system_monitor.gpu_busy(),
        gpu_time: stats.gpu_time.snapshot(),
    }
}

pub(crate) fn build_engine_state(app: &VardaApp) -> EngineState {
    use crate::engine::traits::{
        AnalyzerQueries, AudioQueries, MacroQueries, MixerQueries, ModulationQueries, OutputQueries,
    };
    EngineState {
        mixer: app.mixer_snapshot(),
        buses: build_bus_snapshots(app),
        rundown: app.rundown_snapshot(),
        audio: app.audio_snapshot(),
        audio_mix: app.audio_mix().snapshot(),
        stream: crate::streaming::StreamSnapshot::of(app.stream_settings(), app.stream_status()),
        record: crate::record::RecordSnapshot {
            options: app.record_settings().options.clone(),
            status: app.record_status(),
        },
        modulation: app.modulation_snapshot(),
        outputs: app.output_snapshot(),
        registry: build_registry_snapshot(app),
        midi: build_midi_snapshot(app),
        cameras: build_camera_snapshot(app),
        depth_sensors: build_depth_sensor_snapshot(app),
        screen_capture: build_screen_capture_snapshot(app),
        clock: build_clock_snapshot(app),
        transport: build_transport_snapshot(app),
        timecode: build_timecode_snapshot(app),
        arrangement: build_arrangement_snapshot(app),
        fps: app.frame_stats.fps_smoothed,
        frame_count: app.frame_stats.frame_count,
        target_fps: app.target_fps,
        pacing: build_pacing_snapshot(app),
        ndi_sources: app.external_io.ndi_manager.discovered_sources(),
        decklink_devices: crate::decklink::known_devices(),
        ndi_available: app.external_io.ndi_manager.is_available(),
        #[cfg(target_os = "macos")]
        syphon_sources: app.external_io.syphon_manager.discovered_sources(),
        #[cfg(target_os = "macos")]
        syphon_available: app.external_io.syphon_manager.is_available(),
        #[cfg(not(target_os = "macos"))]
        syphon_sources: vec![],
        #[cfg(not(target_os = "macos"))]
        syphon_available: false,
        stream_receivers: build_stream_receiver_snapshots(app),
        analyzers: app.available_analyzers(),
        macros: app.macro_snapshot(),
        can_undo: app.history_can_undo(),
        can_redo: app.history_can_redo(),
    }
}

/// Build stream library snapshots: library entries merged with active receiver status.
fn build_stream_receiver_snapshots(
    app: &VardaApp,
) -> Vec<crate::engine::types::StreamReceiverSnapshot> {
    let mut result: Vec<crate::engine::types::StreamReceiverSnapshot> = Vec::new();

    // Add library entries (configured but possibly not connected)
    for (url, mode) in &app.external_io.stream_library {
        let connected = (0..app.external_io.stream_manager.receiver_count()).any(|i| {
            app.external_io.stream_manager.receiver_url(i) == Some(url.as_str())
                && app.external_io.stream_manager.is_connected(i)
        });
        result.push(crate::engine::types::StreamReceiverSnapshot {
            url: url.clone(),
            mode: format!("{mode}").to_lowercase(),
            connected,
        });
    }

    // Add active receivers not already in the library (e.g. restored from scene)
    for i in 0..app.external_io.stream_manager.receiver_count() {
        if let (Some(url), Some(mode)) = (
            app.external_io.stream_manager.receiver_url(i),
            app.external_io.stream_manager.receiver_mode(i),
        ) {
            if !result.iter().any(|r| r.url == url) {
                result.push(crate::engine::types::StreamReceiverSnapshot {
                    url: url.to_string(),
                    mode: format!("{mode}").to_lowercase(),
                    connected: app.external_io.stream_manager.is_connected(i),
                });
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::traits::*;

    fn headless_app() -> Option<super::super::VardaApp> {
        let gpu = crate::renderer::context::GpuContext::new_headless().ok()?;
        let config = crate::testing::headless_config();
        super::super::VardaApp::new(gpu, &config).ok()
    }

    #[test]
    fn snapshot_default_mixer_two_channels() {
        let Some(app) = headless_app() else {
            return;
        };
        let snap = build_mixer_snapshot(&app);
        assert_eq!(snap.channels.len(), 2);
        assert_eq!(snap.crossfader, 0.0);
        for ch in &snap.channels {
            assert!(ch.decks.is_empty());
        }
    }

    #[test]
    fn snapshot_deck_opacity_and_effective() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let ch = build_mixer_snapshot(&app).channels[0].uuid.clone();
        let deck_uuid = app.add_solid_color_deck(&ch, [1.0, 0.0, 0.0, 1.0]).unwrap();
        app.set_deck_opacity(&deck_uuid, 0.5).unwrap();
        let snap = build_mixer_snapshot(&app);
        let deck = &snap.channels[0].decks[0];
        assert!((deck.opacity - 0.5).abs() < 1e-5);
        // No transition → effective == opacity
        assert!((deck.effective_opacity - 0.5).abs() < 1e-5);
    }

    #[test]
    fn snapshot_deck_with_effects() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let ch = build_mixer_snapshot(&app).channels[0].uuid.clone();
        let deck_uuid = app.add_solid_color_deck(&ch, [1.0, 0.0, 0.0, 1.0]).unwrap();
        let target = crate::engine::types::EffectTarget::Deck(deck_uuid);
        let effect_uuid = app.add_effect(target, "invert").unwrap();

        let snap = build_mixer_snapshot(&app);
        let effects = &snap.channels[0].decks[0].effects;
        let names: Vec<&str> = effects.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["crop", "transform", "invert"], "after the defaults");
        assert_eq!(effects[2].uuid, effect_uuid);
    }

    #[test]
    fn snapshot_empty_channel_has_no_decks() {
        let Some(app) = headless_app() else {
            return;
        };
        let snap = build_mixer_snapshot(&app);
        assert!(snap.channels[0].decks.is_empty());
    }

    #[test]
    fn build_shader_params_filters_missing() {
        // param_order has "brightness" but values doesn't → filtered out
        let mut params = crate::params::ShaderParams::from_inputs(&[]);
        params.param_order.push("brightness".into());
        // values map is empty, so "brightness" has no value → should be filtered
        let snap = build_shader_params("test_shader", &params);
        assert!(snap.params.is_empty());
    }

    #[test]
    fn build_shader_params_missing_definition() {
        // Value exists but no definition → label/min/max are None
        let mut params = crate::params::ShaderParams::from_inputs(&[]);
        params.param_order.push("mystery".into());
        params
            .values
            .insert("mystery".into(), crate::params::ParamValue::Float(0.5));
        let snap = build_shader_params("test_shader", &params);
        assert_eq!(snap.params.len(), 1);
        let p = &snap.params[0];
        assert!(p.label.is_none());
        assert!(p.min.is_none());
        assert!(p.max.is_none());
    }

    /// A layer's own mask is made in Input Settings: the Library does not
    /// offer the effect that carries it, which alone would hide the layer.
    #[test]
    fn the_library_leaves_out_the_layer_mask_effect() {
        let Some(app) = headless_app() else {
            return;
        };
        let name = crate::engine::value::mask::LAYER_MASK_EFFECT;
        assert!(app.registry.filters().iter().any(|s| s.name() == name));
        let snap = build_registry_snapshot(&app);
        assert!(!snap.filters.iter().any(|(n, _)| n == name));
        assert!(snap.filters.iter().any(|(n, _)| n == "invert"));
    }

    #[test]
    fn build_registry_snapshot_sorted() {
        let Some(app) = headless_app() else {
            return;
        };
        let snap = build_registry_snapshot(&app);
        // Verify generators are sorted case-insensitively
        for pair in snap.generators.windows(2) {
            assert!(
                pair[0].0.to_lowercase() <= pair[1].0.to_lowercase(),
                "generators not sorted: {} > {}",
                pair[0].0,
                pair[1].0,
            );
        }
        for pair in snap.filters.windows(2) {
            assert!(
                pair[0].0.to_lowercase() <= pair[1].0.to_lowercase(),
                "filters not sorted: {} > {}",
                pair[0].0,
                pair[1].0,
            );
        }
    }

    #[test]
    fn build_clock_snapshot_inactive() {
        let Some(app) = headless_app() else {
            return;
        };
        let snap = build_clock_snapshot(&app);
        // Default clock is inactive → bpm is None
        assert!(snap.bpm.is_none() || !snap.active);
    }

    #[test]
    fn build_clock_snapshot_source_labels() {
        let Some(app) = headless_app() else {
            return;
        };
        let snap = build_clock_snapshot(&app);
        // Source label should be one of the known values
        let valid = ["Audio", "MIDI", "OSC", "Manual"];
        assert!(
            valid.contains(&snap.source_label.as_str()),
            "unexpected source label: {}",
            snap.source_label
        );
    }

    /// The diagnostics panel and the API both read this snapshot and nothing
    /// else, so every input the reader is listening to has to appear in it with
    /// its own position and run state. A performer chasing a bad cable is
    /// looking for the input that is *not* resolving.
    #[test]
    fn build_timecode_snapshot_lists_every_input_it_heard() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let now = std::time::Instant::now();
        let at = |seconds| {
            crate::timecode::TimecodeFrame::at(seconds, crate::transport::TimecodeRate::Fps25)
        };
        app.input.timecode.ingest(
            crate::timecode::TimecodeSource::Ltc {
                source_id: 3,
                channel: 1,
            },
            at(90.0),
            now,
        );
        app.input.timecode.ingest(
            crate::timecode::TimecodeSource::Mtc {
                device_id: 7,
                device_name: "Tascam DA-6400".to_string(),
            },
            at(5.0),
            now,
        );
        app.input.timecode.update(now);

        let snap = build_timecode_snapshot(&app);

        assert_eq!(
            snap.inputs
                .iter()
                .map(|i| i.key.as_str())
                .collect::<Vec<_>>(),
            vec!["ltc", "mtc:7"]
        );
        assert_eq!(
            snap.inputs
                .iter()
                .map(|i| i.label.as_str())
                .collect::<Vec<_>>(),
            vec!["LTC (channel 2)", "MTC (Tascam DA-6400)"],
            "the labels are what a performer reads off the panel"
        );
        let ltc = &snap.inputs[0];
        assert!((ltc.position - 90.0).abs() < 0.05);
        assert_eq!(ltc.timecode, "00:01:30:00");
        assert_eq!(ltc.rate, crate::transport::TimecodeRate::Fps25);
        assert!(ltc.running);
        assert!(!ltc.freewheeling, "a frame just arrived");
        assert!((ltc.speed - 1.0).abs() < 1e-9);
        assert_eq!(
            snap.resolved.as_deref(),
            Some("ltc"),
            "LTC outranks MTC, and the snapshot has to say which one is driving"
        );

        // A master that stops is still listed, holding where it stopped.
        app.input
            .timecode
            .update(now + std::time::Duration::from_secs(2));
        let stopped = build_timecode_snapshot(&app);
        assert_eq!(stopped.inputs.len(), 2, "a silent input is still an input");
        assert!(stopped.inputs.iter().all(|i| !i.running));
        assert!(stopped.inputs.iter().all(|i| !i.freewheeling));
    }

    /// The patch is read back out of the same snapshot the positions come from,
    /// so the popover cannot show one interface while the reader listens to
    /// another.
    #[test]
    fn build_timecode_snapshot_reports_the_patch_it_is_reading() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let patch = crate::timecode::LtcInput {
            source_id: 2,
            channel: 1,
            rate: Some(crate::transport::TimecodeRate::Fps2997),
        };
        app.input.timecode.set_ltc_input(Some(patch));
        app.input
            .timecode
            .set_preference(crate::timecode::TimecodePreference::ForceLtc);

        let snap = build_timecode_snapshot(&app);

        assert_eq!(snap.ltc_input, Some(patch));
        assert_eq!(
            snap.preference,
            crate::timecode::TimecodePreference::ForceLtc
        );
        assert!(snap.inputs.is_empty(), "nothing has arrived on it yet");
        assert_eq!(snap.resolved, None);
    }

    #[test]
    fn build_stream_receiver_dedup() {
        let Some(app) = headless_app() else {
            return;
        };
        let receivers = build_stream_receiver_snapshots(&app);
        // All URLs should be unique
        let urls: Vec<&str> = receivers.iter().map(|r| r.url.as_str()).collect();
        let mut deduped = urls.clone();
        deduped.sort_unstable();
        deduped.dedup();
        assert_eq!(
            urls.len(),
            deduped.len(),
            "duplicate stream receivers found"
        );
    }
}
