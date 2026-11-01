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

/// A processing setting for [`VardaApp::cmd_set_audio_processing`].
#[derive(Debug, Clone, Copy)]
pub enum AudioProcessing {
    LowCut(crate::audio_mix::LowCutConfig),
    Gate(crate::audio_mix::GateConfig),
    Eq(crate::audio_mix::EqConfig),
    DeEsser(crate::audio_mix::DeEsserConfig),
    Leveler(crate::audio_mix::LevelerConfig),
    Limiter(crate::audio_mix::LimiterConfig),
}

impl VardaApp {
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
        self.relist_audio_inputs();
        // The captures opened on their own threads since, taken.
        self.audio_manager.poll_opens();
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
        // A device unplugged: its capture let go, the operator told; its
        // inputs are taken again as soon as it is back.
        for name in self.audio_manager.forget_lost() {
            self.notify_error(format!(
                "Sound from '{name}' stopped — unplugged? It is taken again as soon as it is back."
            ));
        }
        // The devices having changed, a capture sending only zeros — its
        // device made anew by its driver — is opened again, once.
        self.audio_manager.reopen_silent();
        self.audio_manager.reopen_pins();
        self.audio_mix.connect_inputs(&mut self.audio_manager);
        self.audio_mix.poll_devices();
    }

    /// The sound devices changed (see `audio::device_watch`): list the
    /// inputs again, half a second on — a device plugged in is said several
    /// times — on a thread of their own, and take the listing as it comes.
    /// The mixer's lists of devices are made again too.
    fn relist_audio_inputs(&mut self) {
        let now = std::time::Instant::now();
        if crate::audio::device_watch::take_changed() {
            self.audio_rescan_due = Some(now + std::time::Duration::from_millis(500));
        }
        if let Some(listing) = &self.audio_inputs_rescan {
            match listing.try_recv() {
                Ok(inputs) => {
                    self.audio_manager.readopt(inputs);
                    // Silent captures opened again from the devices listed
                    // now, not the list from before they changed.
                    self.audio_manager.devices_changed();
                    self.audio_inputs_rescan = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.audio_inputs_rescan = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        let idle = self.audio_inputs_rescan.is_none() && self.audio_inputs_pending.is_none();
        if idle && self.audio_rescan_due.is_some_and(|at| now >= at) {
            self.audio_rescan_due = None;
            self.audio_inputs_rescan = crate::audio::AudioManager::list_in_background();
            self.audio_mix.rescan_devices();
        }
    }

    /// The sound of every NDI source and stream input (SRT, RTMP, HLS) in
    /// the scene that has sent some — and of every camera whose sound comes
    /// with its pictures (a Blackmagic input), given to the first of its
    /// layers — with a fresh feed for each the mixer has not picked up under
    /// its key yet.
    fn live_sounds(&self) -> Vec<LiveSound> {
        use std::hash::{Hash, Hasher};
        let ndi = &self.external_io.ndi_manager;
        let streams = &self.external_io.stream_manager;
        let mut sounds: Vec<LiveSound> = self
            .mixer
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
            .collect();
        let cameras = &self.camera_manager;
        let layers: Vec<(crate::camera::CameraId, &crate::deck::Deck)> = self
            .mixer
            .channels()
            .iter()
            .flat_map(|ch| &ch.decks)
            .filter(|slot| slot.deck.source_type() == "camera")
            .filter_map(|slot| {
                let id = slot
                    .deck
                    .camera_id()
                    .filter(|id| *id != crate::camera::UNBOUND_CAMERA)?;
                Some((id, &slot.deck))
            })
            .collect();
        let mut heard = std::collections::HashSet::new();
        for &(id, first) in &layers {
            if !heard.insert(id) {
                continue;
            }
            // The layer whose strip the mixer has already keeps it — its
            // settings with it — else the first.
            let deck = layers
                .iter()
                .filter(|(other, _)| *other == id)
                .map(|(_, deck)| *deck)
                .find(|deck| self.audio_mix.live_key(deck.uuid()).is_some())
                .unwrap_or(first);
            let Some((rate, channels, generation)) = cameras.sound_format(id) else {
                continue;
            };
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            ("camera", id, rate, channels, generation).hash(&mut hasher);
            let key = hasher.finish();
            let deck_uuid = deck.uuid().to_string();
            let feed = (self.audio_mix.live_key(&deck_uuid) != Some(key)).then(|| {
                let (tx, rx) = crossbeam_channel::bounded(64);
                cameras.set_sound_feed(id, tx);
                LiveFeed { rx, rate, channels }
            });
            sounds.push(LiveSound {
                name: super::inputs::without_icon(deck.source_name()).to_string(),
                deck_uuid,
                key,
                feed,
            });
        }
        sounds
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

    /// Set a source's low cut or leveler, or a bus's limiter: refused, the
    /// sound untouched, when a value is out of range.
    pub fn cmd_set_audio_processing(
        &mut self,
        uuid: &str,
        change: AudioProcessing,
    ) -> CommandResult {
        let valid = match &change {
            AudioProcessing::LowCut(config) => config.validate(),
            AudioProcessing::Gate(config) => config.validate(),
            AudioProcessing::Eq(config) => config.validate(),
            AudioProcessing::DeEsser(config) => config.validate(),
            AudioProcessing::Leveler(config) => config.validate(),
            AudioProcessing::Limiter(config) => config.validate(),
        };
        if let Err(message) = valid {
            return CommandResult::Err {
                code: ErrorCode::InvalidInput,
                message: message.into(),
            };
        }
        match change {
            AudioProcessing::LowCut(config) => {
                self.cmd_edit_audio_source(uuid, |target| target.low_cut = config)
            }
            AudioProcessing::Gate(config) => {
                self.cmd_edit_audio_source(uuid, |target| target.gate = config)
            }
            AudioProcessing::Eq(config) => {
                self.cmd_edit_audio_source(uuid, |target| target.eq = config)
            }
            AudioProcessing::DeEsser(config) => {
                self.cmd_edit_audio_source(uuid, |target| target.de_esser = config)
            }
            AudioProcessing::Leveler(config) => {
                self.cmd_edit_audio_source(uuid, |target| target.leveler = config)
            }
            AudioProcessing::Limiter(config) => {
                self.cmd_edit_audio_bus(uuid, |target| target.limiter = config)
            }
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

    /// A bus's Audio Unit insert set (`component` codes) or taken out;
    /// named as this Mac lists it, else by its codes.
    pub fn cmd_set_audio_insert(&mut self, uuid: &str, component: Option<String>) -> CommandResult {
        if let Some(c) = &component {
            if crate::audio_mix::component_codes(c).is_none() {
                return CommandResult::Err {
                    code: ErrorCode::InvalidInput,
                    message: format!("not an effect's codes: '{c}' (as \"aufx:lpas:appl\")"),
                };
            }
        }
        done(self.audio_mix.set_insert(uuid, component))
    }

    /// Open a bus insert's window on this Mac's screen.
    pub fn cmd_open_audio_insert(&mut self, uuid: &str) -> CommandResult {
        match self.audio_mix.open_insert_window(uuid) {
            Ok(()) => CommandResult::Ok,
            Err(message) => CommandResult::Err {
                code: if message.starts_with("no audio bus") {
                    ErrorCode::NotFound
                } else {
                    ErrorCode::Unavailable
                },
                message,
            },
        }
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
    pub fn cmd_add_app_sound(
        &mut self,
        app: &str,
        name: &str,
        deck: Option<&str>,
    ) -> CommandResult {
        if app.trim().is_empty() {
            return CommandResult::Err {
                code: crate::engine::ErrorCode::InvalidInput,
                message: "no app named".into(),
            };
        }
        if let Some(deck) = deck {
            if let Err(e) = self.resolve_deck(deck) {
                return CommandResult::Err {
                    code: crate::engine::ErrorCode::NotFound,
                    message: e.to_string(),
                };
            }
        }
        // PowerPoint's sound is one: for a PowerPoint input, the strip of it
        // there is already is it.
        if app == crate::presentation::powerpoint::POWERPOINT_ID {
            let powerpoint = self.powerpoint_decks();
            if deck.is_some_and(|d| powerpoint.iter().any(|p| p == d)) {
                if let Some(uuid) = self.powerpoint_sound_strip(&powerpoint) {
                    return CommandResult::OkWithId { uuid };
                }
            }
        }
        // So is web talks', for a Slides input.
        if app == crate::audio::app_sound::WEB_TALKS {
            let slides = self.slides_decks();
            if deck.is_some_and(|d| slides.iter().any(|s| s == d)) {
                if let Some(uuid) = self.web_sound_strip(&slides) {
                    return CommandResult::OkWithId { uuid };
                }
            }
        }
        let name = if name.trim().is_empty() { app } else { name };
        CommandResult::OkWithId {
            uuid: self.audio_mix.add_app_input_of(name, app, deck),
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
            // Sent before, from now on, on or off — one on by default or from
            // an older show was not marked: turned off and on again on a live
            // bus, it stays among the bus's mics (see `live_direct`).
            if (on || strip.sends.contains(&bus)) && !strip.sent_before.contains(&bus) {
                strip.sent_before.push(bus.clone());
            }
            strip.sends.retain(|s| s != &bus);
            if on {
                strip.sends.push(bus);
            }
        })
    }

    /// A source's send to `bus`: on or off, level and point, each as it was
    /// when not given. Checked whole before anything changes.
    pub fn cmd_set_audio_send_settings(
        &mut self,
        uuid: &str,
        bus: &str,
        on: Option<bool>,
        gain_db: Option<f32>,
        mode: Option<crate::audio_mix::SendMode>,
    ) -> CommandResult {
        if !self.audio_mix.has_bus(bus) {
            return AudioMixError::NoSuchBus(bus.into()).into();
        }
        if let Some(db) = gain_db {
            let check = crate::audio_mix::SendConfig { gain_db: db, ..Default::default() };
            if let Err(message) = check.validate() {
                return CommandResult::Err {
                    code: ErrorCode::InvalidInput,
                    message: message.to_string(),
                };
            }
        }
        let bus = bus.to_string();
        self.cmd_edit_audio_source(uuid, move |strip| {
            if gain_db.is_some() || mode.is_some() {
                let send = strip.send_settings.entry(bus.clone()).or_default();
                if let Some(db) = gain_db {
                    send.gain_db = db;
                }
                if let Some(mode) = mode {
                    send.mode = mode;
                }
            }
            if let Some(on) = on {
                if (on || strip.sends.contains(&bus)) && !strip.sent_before.contains(&bus) {
                    strip.sent_before.push(bus.clone());
                }
                strip.sends.retain(|s| s != &bus);
                if on {
                    strip.sends.push(bus);
                }
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

#[cfg(test)]
mod tests {
    use crate::app::VardaApp;

    fn headless_app() -> Option<VardaApp> {
        let gpu = crate::renderer::context::GpuContext::new_headless().ok()?;
        VardaApp::new(gpu, &crate::testing::headless_config()).ok()
    }

    /// A camera that opens at once and sends sound with its pictures, as a
    /// Blackmagic input does, till told to stop.
    fn sounding(
        _: &crate::camera::CameraDeviceInfo,
        _: Option<crate::camera::CameraMode>,
        sink: &std::sync::Arc<crate::camera::Sink>,
        stop: &std::sync::atomic::AtomicBool,
        opened: &std::sync::mpsc::Sender<anyhow::Result<crate::camera::CameraMode>>,
    ) {
        let _ = opened.send(Ok(crate::camera::CameraMode {
            width: 64,
            height: 36,
            fps: 30,
        }));
        while !stop.load(std::sync::atomic::Ordering::Relaxed) {
            sink.put_sound(&[0; 4], 2);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// A camera whose sound comes with its pictures has it in the mixer as
    /// a live source's, given to the first of its layers only.
    #[test]
    fn a_cameras_own_sound_goes_to_its_first_layer_only() {
        use crate::engine::{CommandResult, EngineCommand};
        let Some(mut app) = headless_app() else {
            return;
        };
        app.camera_manager = crate::camera::CameraManager::with_devices(
            &[("decklink:1", "UltraStudio Recorder 3G (Blackmagic)")],
            sounding,
        );
        let id = app.camera_manager.devices()[0].id;
        assert_eq!(app.camera_manager.with_own_sound(), vec![id]);
        let channel = app.mixer.channels()[0].uuid().to_string();
        let add = |app: &mut VardaApp| match app.execute_command(EngineCommand::AddCameraDeck {
            channel_uuid: channel.clone(),
            camera_id: id,
            mode: None,
            audio: None,
        }) {
            CommandResult::OkWithId { uuid } => uuid,
            other => panic!("expected a layer, got {other:?}"),
        };
        let first = add(&mut app);
        let start = std::time::Instant::now();
        while app.camera_manager.sound_format(id).is_none() {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(5),
                "never opened"
            );
            app.reconcile_cameras();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let second = add(&mut app);
        let sounds = app.live_sounds();
        let theirs: Vec<_> = sounds
            .iter()
            .filter(|s| s.deck_uuid == first || s.deck_uuid == second)
            .collect();
        assert_eq!(theirs.len(), 1, "once");
        assert_eq!(theirs[0].deck_uuid, first);
        let feed = theirs[0].feed.as_ref().expect("a feed for the mixer");
        assert_eq!((feed.rate, feed.channels), (48_000, 2));
        assert!(
            feed.rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .is_ok(),
            "its sound comes"
        );
    }

    /// A camera's own sound stays with the layer whose strip the mixer has —
    /// its settings with it — when another layer on it comes first.
    #[test]
    fn a_cameras_own_sound_stays_with_the_layer_that_has_its_strip() {
        use crate::engine::{CommandResult, EngineCommand};
        let Some(mut app) = headless_app() else {
            return;
        };
        app.camera_manager = crate::camera::CameraManager::with_devices(
            &[("decklink:1", "UltraStudio Recorder 3G (Blackmagic)")],
            sounding,
        );
        let id = app.camera_manager.devices()[0].id;
        if app.mixer.channels().len() < 2 {
            return;
        }
        let add = |app: &mut VardaApp, ch: usize| {
            let channel_uuid = app.mixer.channels()[ch].uuid().to_string();
            match app.execute_command(EngineCommand::AddCameraDeck {
                channel_uuid,
                camera_id: id,
                mode: None,
                audio: None,
            }) {
                CommandResult::OkWithId { uuid } => uuid,
                other => panic!("expected a layer, got {other:?}"),
            }
        };
        let later = add(&mut app, 1);
        let start = std::time::Instant::now();
        while app.audio_mix.live_key(&later).is_none() {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(5),
                "never in the mixer"
            );
            app.reconcile_cameras();
            app.update_audio_mix();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let earlier = add(&mut app, 0);
        let sounds = app.live_sounds();
        let theirs: Vec<&str> = sounds
            .iter()
            .map(|s| s.deck_uuid.as_str())
            .filter(|uuid| *uuid == later || *uuid == earlier)
            .collect();
        assert_eq!(theirs, vec![later.as_str()], "kept where its strip is");
    }

    /// A sound input whose device is not open has it opened on a thread of
    /// its own, and the capture taken at a frame once it has opened.
    #[test]
    fn a_sound_inputs_device_opened_on_its_own_thread_is_taken_at_a_frame() {
        let Some(mut app) = headless_app() else {
            return;
        };
        app.audio_manager = crate::audio::AudioManager::listing(&["Mic (test)"]);
        app.audio_manager.open_without_devices_for_test();
        app.audio_mix.add_input("Lectern", "Mic (test)", 0, 1);
        let asked = std::time::Instant::now();
        app.update_audio_mix();
        while !app.audio_manager.active_source_ids().contains(&0) {
            assert!(
                asked.elapsed() < std::time::Duration::from_secs(5),
                "never taken"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
            app.update_audio_mix();
        }
    }

    /// The devices changed: listed again half a second on, and the listing
    /// taken as it comes, numbered as before.
    #[test]
    fn sound_inputs_are_listed_again_as_devices_change() {
        let Some(mut app) = headless_app() else {
            return;
        };
        crate::audio::device_watch::note_change_for_tests();
        app.relist_audio_inputs();
        assert!(app.audio_rescan_due.is_some(), "due half a second on");
        assert!(
            !app.audio_manager.changed_for_tests(),
            "silent captures wait for the new listing"
        );
        assert!(app.audio_inputs_rescan.is_none(), "not at once");
        // The listing, as a thread sends it.
        let (tx, rx) = std::sync::mpsc::channel();
        let known: Vec<crate::audio::AudioDeviceInfo> = app.audio_manager.devices().to_vec();
        let mut devices = known.clone();
        devices.insert(
            0,
            crate::audio::AudioDeviceInfo {
                id: 0,
                name: "USB Capture HDMI (test)".into(),
            },
        );
        for (at, device) in devices.iter_mut().enumerate() {
            device.id = u32::try_from(at).expect("a few devices");
        }
        tx.send((devices, None)).expect("sent");
        app.audio_rescan_due = None;
        app.audio_inputs_rescan = Some(rx);
        app.relist_audio_inputs();
        assert!(app.audio_inputs_rescan.is_none(), "taken");
        assert!(
            app.audio_manager.changed_for_tests(),
            "silent captures may be opened again"
        );
        let now = app.audio_manager.devices();
        assert!(now.iter().any(|d| d.name == "USB Capture HDMI (test)"));
        for device in &known {
            assert!(
                now.iter()
                    .any(|d| d.name == device.name && d.id == device.id),
                "{} kept its number",
                device.name
            );
        }
    }
}
