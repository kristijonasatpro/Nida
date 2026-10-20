//! The audio mixer as the rest of the app sees it: kept in step with the scene
//! every frame, and edited by commands.

use super::super::VardaApp;
use crate::audio_mix::{AudioMixError, ClipSource, LiveFeed, LiveSound};
use crate::engine::{CommandResult, ErrorCode};

impl From<AudioMixError> for CommandResult {
    fn from(e: AudioMixError) -> Self {
        CommandResult::Err {
            code: ErrorCode::NotFound,
            message: e.to_string(),
        }
    }
}

fn done(result: Result<(), AudioMixError>) -> CommandResult {
    match result {
        Ok(()) => CommandResult::Ok,
        Err(e) => e.into(),
    }
}

impl VardaApp {
    pub fn cmd_set_audio_equalizer(
        &mut self,
        uuid: &str,
        bus: bool,
        config: crate::audio_mix::EqualizerConfig,
    ) -> CommandResult {
        if let Err(message) = config.validate() {
            return CommandResult::Err {
                code: ErrorCode::InvalidInput,
                message: message.into(),
            };
        }
        if bus {
            self.cmd_edit_audio_bus(uuid, |target| target.equalizer = config)
        } else {
            self.cmd_edit_audio_source(uuid, |target| target.equalizer = config)
        }
    }
    /// Bring the mixer in line with this frame: which decks have sound, what
    /// is on air (see `on_air`), input devices and sound devices. Called once
    /// per rendered frame.
    pub fn update_audio_mix(&mut self) {
        // Sound inputs listed late (macOS was asking for the microphone as
        // the app started): taken as soon as they are.
        if let Some(listing) = &self.audio_inputs_pending {
            match listing.try_recv() {
                Ok(inputs) => {
                    self.audio_manager.adopt(inputs);
                    self.audio_inputs_pending = None;
                    self.session.notifications.info("Sound inputs ready");
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.audio_inputs_pending = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        let clips: Vec<ClipSource> = self
            .mixer
            .channels()
            .iter()
            .flat_map(|ch| &ch.decks)
            .filter_map(|slot| {
                slot.deck.clip_audio().map(|shared| ClipSource {
                    deck_uuid: slot.deck.uuid().to_string(),
                    name: slot.deck.source_name().to_string(),
                    shared,
                })
            })
            .collect();
        self.audio_mix.sync_clips(clips);
        let live = self.live_sounds();
        self.audio_mix.sync_live(live);
        self.update_on_air();
        self.audio_mix.connect_inputs(&mut self.audio_manager);
        self.audio_mix.poll_devices();
    }

    /// The sound of every NDI source and stream input (SRT, RTMP, HLS) in
    /// the scene that has sent some, with a fresh feed for each the mixer
    /// has not picked up under its key yet.
    fn live_sounds(&self) -> Vec<LiveSound> {
        use std::hash::{Hash, Hasher};
        let ndi = &self.external_io.ndi_manager;
        let streams = &self.external_io.stream_manager;
        self.mixer
            .channels()
            .iter()
            .flat_map(|ch| &ch.decks)
            .filter_map(|slot| {
                let (from_ndi, idx) =
                    match (slot.deck.ndi_receiver_idx(), slot.deck.srt_receiver_idx()) {
                        (Some(idx), _) => (true, idx),
                        (None, Some(idx)) => (false, idx),
                        (None, None) => return None,
                    };
                let (rate, channels, generation) = if from_ndi {
                    ndi.audio_format(idx)?
                } else {
                    streams.audio_format(idx)?
                };
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                (from_ndi, idx, rate, channels, generation).hash(&mut hasher);
                let key = hasher.finish();
                let deck_uuid = slot.deck.uuid().to_string();
                let feed = (self.audio_mix.live_key(&deck_uuid) != Some(key)).then(|| {
                    let (tx, rx) = crossbeam_channel::bounded(64);
                    if from_ndi {
                        ndi.set_audio_sink(idx, tx);
                    } else {
                        streams.set_audio_sink(idx, tx);
                    }
                    LiveFeed { rx, rate, channels }
                });
                Some(LiveSound {
                    name: super::inputs::without_icon(slot.deck.source_name()).to_string(),
                    deck_uuid,
                    key,
                    feed,
                })
            })
            .collect()
    }

    // ── Commands ────────────────────────────────────────────────────

    pub fn cmd_set_audio_compressor(
        &mut self,
        uuid: &str,
        bus: bool,
        config: crate::audio_mix::CompressorConfig,
    ) -> CommandResult {
        if let Err(message) = config.validate() {
            return CommandResult::Err {
                code: ErrorCode::InvalidInput,
                message: message.into(),
            };
        }
        if bus {
            self.cmd_edit_audio_bus(uuid, |target| target.compressor = config)
        } else {
            self.cmd_edit_audio_source(uuid, |target| target.compressor = config)
        }
    }

    pub fn cmd_add_audio_bus(&mut self, name: &str) -> CommandResult {
        CommandResult::OkWithId {
            uuid: self.audio_mix.add_bus(name),
        }
    }

    pub fn cmd_remove_audio_bus(&mut self, uuid: &str) -> CommandResult {
        done(self.audio_mix.remove_bus(uuid))
    }

    pub fn cmd_edit_audio_bus(
        &mut self,
        uuid: &str,
        edit: impl FnOnce(&mut crate::audio_mix::AudioBusConfig),
    ) -> CommandResult {
        done(self.audio_mix.edit_bus(uuid, edit))
    }

    pub fn cmd_add_audio_input(
        &mut self,
        name: &str,
        device: &str,
        left: u16,
        right: u16,
    ) -> CommandResult {
        CommandResult::OkWithId {
            uuid: self.audio_mix.add_input(name, device, left, right),
        }
    }

    /// Add an app's sound as a source (see [`EngineCommand::AddAppSound`]).
    ///
    /// [`EngineCommand::AddAppSound`]: crate::engine::EngineCommand::AddAppSound
    pub fn cmd_add_app_sound(&mut self, app: &str, name: &str) -> CommandResult {
        if app.trim().is_empty() {
            return CommandResult::Err {
                code: crate::engine::ErrorCode::InvalidInput,
                message: "no app named".into(),
            };
        }
        let name = if name.trim().is_empty() { app } else { name };
        CommandResult::OkWithId {
            uuid: self.audio_mix.add_app_input(name, app),
        }
    }

    pub fn cmd_remove_audio_input(&mut self, uuid: &str) -> CommandResult {
        done(self.audio_mix.remove_input(uuid))
    }

    pub fn cmd_edit_audio_source(
        &mut self,
        uuid: &str,
        edit: impl FnOnce(&mut crate::audio_mix::AudioStripConfig),
    ) -> CommandResult {
        done(self.audio_mix.edit_source(uuid, edit))
    }

    pub fn cmd_set_audio_solo(&mut self, uuid: &str, solo: bool) -> CommandResult {
        done(self.audio_mix.set_solo(uuid, solo))
    }

    pub fn cmd_edit_audio_monitor(
        &mut self,
        edit: impl FnOnce(&mut crate::audio_mix::AudioMonitorConfig),
    ) -> CommandResult {
        done(self.audio_mix.edit_monitor(edit))
    }

    /// Send a source to a bus, or stop sending it.
    pub fn cmd_set_audio_send(&mut self, uuid: &str, bus: &str, on: bool) -> CommandResult {
        if on && !self.audio_mix.has_bus(bus) {
            return AudioMixError::NoSuchBus(bus.into()).into();
        }
        let bus = bus.to_string();
        self.cmd_edit_audio_source(uuid, move |strip| {
            strip.sends.retain(|s| s != &bus);
            if on {
                strip.sends.push(bus);
            }
        })
    }

    /// Where a source's Audio Auto looks: a video bus, Main
    /// ([`crate::audio_mix::FOLLOW_MAIN`]), or `None` for Main and every bus.
    pub fn cmd_set_audio_follow(&mut self, uuid: &str, follow: Option<String>) -> CommandResult {
        if let Some(bus) = &follow {
            if bus != crate::audio_mix::FOLLOW_MAIN && self.production_buses.get(bus).is_none() {
                return CommandResult::Err {
                    code: ErrorCode::NotFound,
                    message: format!("no video bus with uuid '{bus}'"),
                };
            }
        }
        self.cmd_edit_audio_source(uuid, move |strip| strip.follow = follow)
    }
}
