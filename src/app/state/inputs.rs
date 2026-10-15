//! New inputs, as vMix's Add Input makes them: a camera, an NDI source, a
//! screen, a stream, a colour, a generator or a copy of another input
//! becomes a channel of its own at the end of the row, called after what it
//! shows.

use crate::engine::{CommandResult, ErrorCode, InputAudio, InputSource, MixerCommands};

use super::super::VardaApp;

impl VardaApp {
    /// A channel at the end of the row showing `source`, named `name` or
    /// after the source, with `audio` as its own sound. A source that fails
    /// takes its channel with it.
    pub fn cmd_add_input(
        &mut self,
        source: &InputSource,
        name: Option<&str>,
        audio: Option<&InputAudio>,
    ) -> CommandResult {
        let channel_uuid = match self.add_channel() {
            Ok(uuid) => uuid,
            Err(e) => {
                return CommandResult::Err {
                    code: ErrorCode::InternalError,
                    message: e.to_string(),
                }
            }
        };
        let added = self.execute_command(source.deck_command(&channel_uuid));
        let deck_uuid = match added {
            CommandResult::OkWithId { uuid } => uuid,
            CommandResult::Err { .. } => {
                if let Err(e) = self.remove_channel(&channel_uuid) {
                    log::warn!("Removing the channel of an input that failed: {e}");
                }
                return added;
            }
            _ => {
                let _ = self.remove_channel(&channel_uuid);
                return CommandResult::Err {
                    code: ErrorCode::InternalError,
                    message: "The source added no layer".into(),
                };
            }
        };
        let name = name
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map_or_else(|| self.input_name(source, &deck_uuid), str::to_string);
        let _ = self.cmd_rename_channel(&channel_uuid, &name);
        if let Some(audio) = audio {
            self.audio_mix.add_input_of_deck(
                &name,
                &audio.device,
                (audio.left, audio.right),
                &deck_uuid,
            );
        }
        CommandResult::OkWithId { uuid: channel_uuid }
    }

    /// What a new input is called when nobody says: the camera, NDI source
    /// or window it shows, as its layer is named.
    fn input_name(&self, source: &InputSource, deck_uuid: &str) -> String {
        match source {
            InputSource::Colour { .. } => return "Colour".to_string(),
            // A copy of an input goes by the input's own name.
            InputSource::Input { channel_uuid } => {
                if let Some(ch) = self
                    .mixer
                    .channels()
                    .iter()
                    .find(|c| c.uuid() == channel_uuid)
                {
                    return format!("{} (copy)", ch.name);
                }
            }
            _ => {}
        }
        self.mixer
            .channels()
            .iter()
            .flat_map(|ch| &ch.decks)
            .find(|slot| slot.deck.uuid() == deck_uuid)
            .map(|slot| without_icon(slot.deck.source_name()).to_string())
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| "Input".to_string())
    }
}

impl VardaApp {
    /// Change a colour layer's colour.
    pub fn cmd_set_deck_color(&mut self, deck_uuid: &str, color: [f32; 4]) -> CommandResult {
        let Some((ch, dk)) = self.mixer.find_deck_by_uuid(deck_uuid) else {
            return CommandResult::Err {
                code: ErrorCode::NotFound,
                message: format!("No deck {deck_uuid}"),
            };
        };
        let deck = &mut self.mixer.channels_mut()[ch].decks[dk].deck;
        if deck.solid_color().is_none() {
            return CommandResult::Err {
                code: ErrorCode::InvalidInput,
                message: "Only a colour layer has a colour to change".into(),
            };
        }
        deck.set_solid_color(color.map(|c| {
            if c.is_finite() {
                c.clamp(0.0, 1.0)
            } else {
                0.0
            }
        }));
        CommandResult::Ok
    }

    /// Read a camera layer's pictures into colours as `colour` says — and
    /// those of every other layer on that camera, which share its capture,
    /// so what they show and what the show saves agree. The frame loop
    /// keeps the capture in step with its layers (see
    /// `CameraManager::set_colours`); it is told here as well, so the very
    /// next frame is read so.
    pub fn cmd_set_camera_colour(
        &mut self,
        deck_uuid: &str,
        colour: crate::engine::value::camera::CameraColour,
    ) -> CommandResult {
        match self.each_layer_on_camera(deck_uuid, |deck| deck.camera_colour = colour) {
            Ok(camera) => {
                if let Some(id) = camera {
                    self.camera_manager.set_colour(id, colour);
                }
                // The camera's open-time line says how it was read then;
                // this says how it is read from now on.
                log::info!(
                    "Camera layer {deck_uuid}: colour range {:?}, matrix {:?}",
                    colour.range,
                    colour.matrix
                );
                CommandResult::Ok
            }
            Err(not_found) => not_found,
        }
    }

    /// Show a camera layer's newest picture every frame, or each in turn at
    /// the delay that keeps them even — and so every other layer on that
    /// camera, which share its capture. Told to the capture at once as well
    /// as each frame (see `CameraManager::set_lowest_latencies`).
    pub fn cmd_set_camera_lowest_latency(&mut self, deck_uuid: &str, on: bool) -> CommandResult {
        match self.each_layer_on_camera(deck_uuid, |deck| deck.camera_lowest_latency = on) {
            Ok(camera) => {
                if let Some(id) = camera {
                    self.camera_manager.set_lowest_latency(id, on);
                }
                log::info!(
                    "Camera layer {deck_uuid}: {}",
                    if on { "lowest latency" } else { "even motion" }
                );
                CommandResult::Ok
            }
            Err(not_found) => not_found,
        }
    }

    /// Code a camera layer's picture as `profile` says — and that of every
    /// other layer on that camera, which share its capture. The frame loop
    /// converts the camera's picture as its layers say, from the next frame
    /// (see `CameraManager::set_profiles`).
    pub fn cmd_set_camera_profile(
        &mut self,
        deck_uuid: &str,
        profile: &crate::engine::value::camera::CameraProfile,
    ) -> CommandResult {
        match self.each_layer_on_camera(deck_uuid, |deck| deck.camera_profile.clone_from(profile)) {
            Ok(_) => {
                log::info!("Camera layer {deck_uuid}: picture profile {profile:?}");
                CommandResult::Ok
            }
            Err(not_found) => not_found,
        }
    }

    /// Do `set` to camera layer `deck_uuid` and every other layer on its
    /// camera (the same one open, or the same device). The camera, if open;
    /// not found, for a layer that is no camera.
    fn each_layer_on_camera(
        &mut self,
        deck_uuid: &str,
        mut set: impl FnMut(&mut crate::deck::Deck),
    ) -> Result<Option<crate::camera::CameraId>, CommandResult> {
        let Some((ch, dk)) = self.mixer.find_deck_by_uuid(deck_uuid) else {
            return Err(CommandResult::Err {
                code: ErrorCode::NotFound,
                message: format!("No deck {deck_uuid}"),
            });
        };
        let deck = &self.mixer.channels()[ch].decks[dk].deck;
        if deck.source_type() != "camera" {
            return Err(CommandResult::Err {
                code: ErrorCode::NotFound,
                message: format!("No camera layer {deck_uuid}"),
            });
        }
        let camera = deck
            .camera_id()
            .filter(|id| *id != crate::camera::UNBOUND_CAMERA);
        let device = deck.camera_device.clone();
        for slot in self
            .mixer
            .channels_mut()
            .iter_mut()
            .flat_map(|channel| channel.decks.iter_mut())
        {
            let deck = &mut slot.deck;
            let same_camera = deck.source_type() == "camera"
                && ((camera.is_some() && deck.camera_id() == camera)
                    || (device.is_some() && deck.camera_device == device));
            if deck.uuid() == deck_uuid || same_camera {
                set(deck);
            }
        }
        Ok(camera)
    }
}

impl VardaApp {
    /// Read a new layer on a camera already in the show as that camera's
    /// other layers are read. They share one capture, and how it is read
    /// follows its layers: a new layer at its defaults put an ATEM's colour
    /// back to Auto — crushed blacks again — when it was added to another
    /// channel.
    pub(crate) fn read_as_its_camera_is(&self, deck: &mut crate::deck::Deck) {
        let camera = deck
            .camera_id()
            .filter(|id| *id != crate::camera::UNBOUND_CAMERA);
        let device = deck.camera_device.clone();
        let other = self
            .mixer
            .channels()
            .iter()
            .flat_map(|channel| channel.decks.iter())
            .map(|slot| &slot.deck)
            .find(|other| {
                other.uuid() != deck.uuid()
                    && other.source_type() == "camera"
                    && ((camera.is_some() && other.camera_id() == camera)
                        || (device.is_some() && other.camera_device == device))
            });
        if let Some(other) = other {
            deck.camera_colour = other.camera_colour;
            deck.camera_profile.clone_from(&other.camera_profile);
            deck.camera_lowest_latency = other.camera_lowest_latency;
        }
    }

    /// Give decks waiting for their camera — not plugged in, or not allowed
    /// yet, when the show was opened — their camera as soon as it opens:
    /// looked for by name every two seconds while any deck waits.
    pub(crate) fn reconcile_cameras(&mut self) {
        type Waiting = (
            String,
            String,
            Option<String>,
            Option<crate::camera::CameraMode>,
        );
        // Cameras started for waiting decks that run now: shown.
        for opened in self.camera_manager.poll_opening(&self.context.device) {
            match opened.result {
                Ok(size) => self.show_opened_camera(opened.id, &opened.decks, size),
                Err(e) => log::warn!("{e:#}"),
            }
        }
        let waiting: Vec<Waiting> = self
            .mixer
            .channels()
            .iter()
            .flat_map(|channel| &channel.decks)
            .filter_map(|slot| {
                let name = slot.deck.camera_waiting_for()?.to_string();
                let device = slot.deck.camera_device.clone();
                Some((
                    slot.deck.uuid().to_string(),
                    name,
                    device,
                    slot.deck.camera_mode,
                ))
            })
            .collect();
        if waiting.is_empty() {
            return;
        }
        let now = std::time::Instant::now();
        if self.camera_retry_at.is_some_and(|at| now < at) {
            return;
        }
        self.camera_retry_at = Some(now + std::time::Duration::from_secs(2));
        self.camera_manager.scan_devices();
        // Each camera found: open already (another input shows it), its decks
        // shown at once; else started in the background — starting takes the
        // OS a moment the render loop does not wait for.
        let mut starting: Vec<(
            crate::camera::CameraId,
            Option<crate::camera::CameraMode>,
            Vec<String>,
        )> = Vec::new();
        for (uuid, name, device, mode) in waiting {
            let Some(id) = self.camera_manager.find(device.as_deref(), &name) else {
                continue;
            };
            if self.camera_manager.is_active(id) {
                if let Ok(size) = self
                    .camera_manager
                    .open_camera(id, &self.context.device, mode)
                {
                    self.bind_waiting_camera(&uuid, id, size);
                }
            } else if !self.camera_manager.is_opening(id) {
                match starting.iter_mut().find(|(i, ..)| *i == id) {
                    Some((.., decks)) => decks.push(uuid),
                    None => starting.push((id, mode, vec![uuid])),
                }
            }
        }
        for (id, mode, decks) in starting {
            if let Err(e) = self.camera_manager.open_in_background(id, mode, decks) {
                log::warn!("{e:#}");
            }
        }
    }

    /// A camera started in the background runs: the decks that waited for
    /// it show it — the first on the reference its start holds, the rest on
    /// one each. Let go again when none is left to show it.
    fn show_opened_camera(
        &mut self,
        id: crate::camera::CameraId,
        decks: &[String],
        size: (u32, u32),
    ) {
        let mut shown = 0;
        for uuid in decks {
            if shown > 0
                && self
                    .camera_manager
                    .open_camera(id, &self.context.device, None)
                    .is_err()
            {
                continue;
            }
            if self.bind_waiting_camera(uuid, id, size) {
                shown += 1;
            } else if shown > 0 {
                self.camera_manager.release_camera(id);
            }
        }
        if shown == 0 {
            self.camera_manager.release_camera(id);
        }
    }

    /// Point the deck `uuid`, if it still waits for a camera, at camera `id`.
    fn bind_waiting_camera(
        &mut self,
        uuid: &str,
        id: crate::camera::CameraId,
        size: (u32, u32),
    ) -> bool {
        let unique_id = self.camera_manager.unique_id(id).map(str::to_string);
        let Some(slot) = self
            .mixer
            .channels_mut()
            .iter_mut()
            .flat_map(|c| c.decks.iter_mut())
            .find(|slot| slot.deck.uuid() == uuid && slot.deck.camera_waiting_for().is_some())
        else {
            return false;
        };
        let name = slot
            .deck
            .camera_waiting_for()
            .unwrap_or_default()
            .to_string();
        slot.deck.bind_camera(id, size);
        slot.deck.camera_device = unique_id;
        log::info!("Camera '{name}' is here: showing it");
        self.session
            .notifications
            .info(format!("📹 {name} connected"));
        true
    }
}

/// A layer's name without the symbol some sources put in front ("📹 ", "📡 ").
pub(crate) fn without_icon(name: &str) -> &str {
    match name.split_once(' ') {
        Some((icon, rest))
            if !icon.is_empty() && icon.chars().all(|c| !c.is_ascii() && !c.is_alphanumeric()) =>
        {
            rest
        }
        _ => name,
    }
}

#[cfg(test)]
mod tests {
    use super::without_icon;
    use crate::app::VardaApp;
    use crate::engine::value::camera::{CameraColour, CameraMatrix, CameraRange};
    use crate::engine::{CommandResult, EngineCommand, ErrorCode};

    #[test]
    fn an_input_is_named_without_the_symbol_its_layer_carries() {
        assert_eq!(without_icon("📹 Blackmagic Design"), "Blackmagic Design");
        assert_eq!(without_icon("📡 STUDIO (Camera 1)"), "STUDIO (Camera 1)");
        assert_eq!(without_icon("🖥\u{fe0f} Display 1"), "Display 1");
        assert_eq!(without_icon("Display 1"), "Display 1");
        assert_eq!(without_icon("Ärger Cam"), "Ärger Cam");
    }

    fn headless_app() -> Option<VardaApp> {
        let gpu = crate::renderer::context::GpuContext::new_headless().ok()?;
        VardaApp::new(gpu, &crate::testing::headless_config()).ok()
    }

    /// A layer on camera `id` (none opened) of device `device`, in the first
    /// channel; its UUID.
    fn camera_layer(app: &mut VardaApp, id: crate::camera::CameraId, device: &str) -> String {
        let mut deck = crate::deck::Deck::new_from_camera(
            &app.context,
            id,
            "Blackmagic Design",
            1920,
            1080,
            64,
            64,
        )
        .expect("a camera layer");
        deck.camera_device = Some(device.into());
        let uuid = deck.uuid().to_string();
        app.mixer.channels_mut()[0].add_deck(deck);
        uuid
    }

    fn colour_of(app: &VardaApp, uuid: &str) -> CameraColour {
        let (ch, dk) = app.mixer.find_deck_by_uuid(uuid).expect("the layer");
        app.mixer.channels()[ch].decks[dk].deck.camera_colour
    }

    /// A camera's colour is set on every layer of that camera — they share
    /// its capture — and on no other camera's; a layer that is no camera's,
    /// or none at all, is not found.
    #[test]
    fn a_cameras_colour_is_set_on_every_layer_of_that_camera() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let first = camera_layer(&mut app, 3, "0xatem");
        let second = camera_layer(&mut app, 3, "0xatem");
        let other = camera_layer(&mut app, 4, "0xfacetime");
        let full = CameraColour {
            range: CameraRange::Full,
            matrix: CameraMatrix::Bt709,
        };
        let set = |app: &mut VardaApp, deck_uuid: &str| {
            app.execute_command(EngineCommand::SetCameraColour {
                deck_uuid: deck_uuid.to_string(),
                colour: full,
            })
        };
        assert!(matches!(set(&mut app, &first), CommandResult::Ok));
        assert_eq!(colour_of(&app, &first), full);
        assert_eq!(colour_of(&app, &second), full, "the same camera");
        assert!(colour_of(&app, &other).is_auto(), "another camera");

        let not_found =
            |result: CommandResult| matches!(result, CommandResult::Err { code: ErrorCode::NotFound, .. });
        assert!(not_found(set(&mut app, "no-such-layer")));
        let channel_uuid = app.mixer.channels()[0].uuid().to_string();
        let CommandResult::OkWithId { uuid: colour_layer } =
            app.execute_command(EngineCommand::AddSolidColorDeck {
                channel_uuid,
                color: [1.0, 0.0, 0.0, 1.0],
            })
        else {
            panic!("a colour layer");
        };
        assert!(not_found(set(&mut app, &colour_layer)), "no camera there");
    }

    /// A new layer on a camera already in the show is read as its other
    /// layers are — colour and picture profile — and one on another camera
    /// at its own defaults.
    #[test]
    fn a_new_layer_on_a_camera_is_read_as_its_other_layers_are() {
        use crate::engine::value::camera::CameraProfile;
        let Some(mut app) = headless_app() else {
            return;
        };
        let first = camera_layer(&mut app, 3, "0xatem");
        let full = CameraColour {
            range: CameraRange::Full,
            matrix: CameraMatrix::Bt709,
        };
        for command in [
            EngineCommand::SetCameraColour {
                deck_uuid: first.clone(),
                colour: full,
            },
            EngineCommand::SetCameraProfile {
                deck_uuid: first,
                profile: CameraProfile::SonyHlg,
            },
        ] {
            assert!(matches!(app.execute_command(command), CommandResult::Ok));
        }
        let new_layer = |app: &VardaApp, id, device: &str| {
            let mut deck =
                crate::deck::Deck::new_from_camera(&app.context, id, "Camera", 1920, 1080, 64, 64)
                    .expect("a camera layer");
            deck.camera_device = Some(device.into());
            app.read_as_its_camera_is(&mut deck);
            deck
        };
        let same = new_layer(&app, 3, "0xatem");
        assert_eq!(same.camera_colour, full);
        assert_eq!(same.camera_profile, CameraProfile::SonyHlg);
        let by_device = new_layer(&app, 7, "0xatem");
        assert_eq!(by_device.camera_colour, full, "the same device, opened again");
        let other = new_layer(&app, 4, "0xfacetime");
        assert!(other.camera_colour.is_auto() && other.camera_profile.is_normal());
    }

    /// A camera's lowest latency is set on every layer of that camera and no
    /// other camera's, taken by a new layer on it, and put back by an undo.
    #[test]
    fn a_cameras_lowest_latency_is_set_on_every_layer_of_that_camera() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let first = camera_layer(&mut app, 3, "0xatem");
        let second = camera_layer(&mut app, 3, "0xatem");
        let other = camera_layer(&mut app, 4, "0xfacetime");
        let lowest = |app: &VardaApp, uuid: &str| {
            let (ch, dk) = app.mixer.find_deck_by_uuid(uuid).expect("the layer");
            app.mixer.channels()[ch].decks[dk].deck.camera_lowest_latency
        };
        let set = |app: &mut VardaApp, deck_uuid: &str, on: bool| {
            app.execute_command(EngineCommand::SetCameraLowestLatency {
                deck_uuid: deck_uuid.to_string(),
                on,
            })
        };
        assert!(matches!(set(&mut app, &first, true), CommandResult::Ok));
        assert!(lowest(&app, &first) && lowest(&app, &second), "the same camera");
        assert!(!lowest(&app, &other), "another camera");

        let mut new_layer =
            crate::deck::Deck::new_from_camera(&app.context, 3, "Camera", 1920, 1080, 64, 64)
                .expect("a camera layer");
        new_layer.camera_device = Some("0xatem".into());
        app.read_as_its_camera_is(&mut new_layer);
        assert!(new_layer.camera_lowest_latency, "a new layer on it takes it");

        // As the command bus does for an undoable command.
        let before = app.history_snapshot_default();
        app.push_history(before);
        set(&mut app, &second, false);
        assert!(!lowest(&app, &first));
        app.execute_command(EngineCommand::Undo);
        assert!(lowest(&app, &first) && lowest(&app, &second), "undone");

        assert!(matches!(
            set(&mut app, "no-such-layer", true),
            CommandResult::Err {
                code: ErrorCode::NotFound,
                ..
            }
        ));
    }

    /// A camera's picture profile is set on every layer of that camera and
    /// no other camera's, saved with the show, and put back by an undo.
    #[test]
    fn a_cameras_profile_is_set_on_every_layer_of_that_camera() {
        use crate::engine::value::camera::CameraProfile;
        let Some(mut app) = headless_app() else {
            return;
        };
        let first = camera_layer(&mut app, 3, "0xatem");
        let second = camera_layer(&mut app, 3, "0xatem");
        let other = camera_layer(&mut app, 4, "0xfacetime");
        let profile_of = |app: &VardaApp, uuid: &str| {
            let (ch, dk) = app.mixer.find_deck_by_uuid(uuid).expect("the layer");
            app.mixer.channels()[ch].decks[dk].deck.camera_profile.clone()
        };
        let s_log3 = CameraProfile::SonySLog3Cine;
        let result = app.execute_command(EngineCommand::SetCameraProfile {
            deck_uuid: first.clone(),
            profile: s_log3.clone(),
        });
        assert!(matches!(result, CommandResult::Ok), "{result:?}");
        assert_eq!(profile_of(&app, &first), s_log3);
        assert_eq!(profile_of(&app, &second), s_log3, "the same camera");
        assert!(profile_of(&app, &other).is_normal(), "another camera");

        let lut = CameraProfile::Lut {
            file: "/Volumes/Show/URSA Broadcast Film to Video.cube".into(),
        };
        // As the command bus does for an undoable command.
        let before = app.history_snapshot_default();
        app.push_history(before);
        app.execute_command(EngineCommand::SetCameraProfile {
            deck_uuid: second.clone(),
            profile: lut.clone(),
        });
        assert_eq!(profile_of(&app, &first), lut);
        app.execute_command(EngineCommand::Undo);
        assert_eq!(profile_of(&app, &first), s_log3, "undone");
        assert_eq!(profile_of(&app, &second), s_log3, "undone on the other too");

        let missing = app.execute_command(EngineCommand::SetCameraProfile {
            deck_uuid: "no-such-layer".into(),
            profile: s_log3,
        });
        assert!(matches!(
            missing,
            CommandResult::Err {
                code: ErrorCode::NotFound,
                ..
            }
        ));
    }
}
