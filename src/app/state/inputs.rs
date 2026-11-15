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

    /// Show a camera layer at the lowest latency (see
    /// `FramePicker::set_newest`), or each picture in turn at the delay that
    /// keeps them even — and so every other layer on that
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

    /// Open a camera layer's camera at `mode` — `None`, its best — and so
    /// every other layer on that camera, which share its capture; kept with
    /// the show.
    pub fn cmd_set_camera_mode(
        &mut self,
        deck_uuid: &str,
        mode: Option<crate::engine::value::camera::CameraMode>,
    ) -> CommandResult {
        // A layer still waiting for its camera — starting, or not plugged
        // in — finds it by its device.
        let waiting = self
            .mixer
            .find_deck_by_uuid(deck_uuid)
            .and_then(|(ch, dk)| {
                let deck = &self.mixer.channels()[ch].decks[dk].deck;
                Some((
                    deck.camera_device.clone(),
                    deck.camera_waiting_for()?.to_string(),
                ))
            });
        match self.each_layer_on_camera(deck_uuid, |deck| deck.camera_mode = mode) {
            Ok(camera) => {
                let camera = camera.or_else(|| {
                    let (device, name) = waiting?;
                    self.camera_manager.find(device.as_deref(), &name)
                });
                // Not open nor starting, it opens at this format when found.
                if let Some(id) = camera {
                    self.camera_manager.set_mode(id, mode);
                }
                log::info!(
                    "Camera layer {deck_uuid}: format {}",
                    mode.map_or_else(|| "best".to_string(), |m| m.label())
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

    /// Reset camera layer `deck_uuid`'s input (see
    /// [`EngineCommand::ResetInput`]): its capture closes and opens again
    /// as it is — the device found afresh, its formats read again — whoever
    /// else holds it; one not open yet is looked for at once; the sound
    /// taken with its layers is opened again; the devices are listed again.
    pub(crate) fn cmd_reset_input(&mut self, deck_uuid: &str) -> CommandResult {
        let mut layers: Vec<String> = Vec::new();
        let camera = match self.each_layer_on_camera(deck_uuid, |deck| {
            layers.push(deck.uuid().to_string());
        }) {
            Ok(camera) => camera,
            Err(not_found) => return not_found,
        };
        if let Some(id) = camera {
            self.camera_manager.restart(id);
        }
        self.camera_retry_at = None;
        for uuid in &layers {
            self.audio_mix
                .reopen_inputs_of_deck(uuid, &mut self.audio_manager);
        }
        self.audio_rescan_due = Some(std::time::Instant::now());
        CommandResult::Ok
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
            // They share its capture, at one format.
            deck.camera_mode = other.camera_mode;
        }
    }

    /// Give decks waiting for their camera — not plugged in, or not allowed
    /// yet, when the show was opened — their camera as soon as it opens:
    /// looked for by name every two seconds while any deck waits.
    pub(crate) fn reconcile_cameras(&mut self) {
        // The cameras come this look, and how many inputs each is shown on:
        // said once each, whichever way its inputs came by it. An input
        // added started a camera two others waited for, and it was said
        // connected for the one and again for the two.
        let mut here: Vec<(crate::camera::CameraId, String, usize)> = Vec::new();
        self.show_cameras_come(&mut here);
        for (_, name, inputs) in here {
            self.say_camera_here(&name, inputs);
        }
    }

    /// The look of `reconcile_cameras`: cameras started in the background
    /// that run now, and cameras open already, shown on the inputs that
    /// waited for them — each counted in `here`, to be said.
    fn show_cameras_come(&mut self, here: &mut Vec<(crate::camera::CameraId, String, usize)>) {
        type Waiting = (
            String,
            String,
            Option<String>,
            Option<crate::camera::CameraMode>,
        );
        // A Blackmagic box plugged in or out, or told of by its discovery
        // after the first look: the cameras looked for again.
        if self.camera_manager.blackmagic_changed() {
            self.camera_manager.refresh_blackmagic();
        }
        let moves = self.camera_manager.take_moves();
        self.follow_moved_cameras(&moves);
        // Cameras started for waiting decks that run now: shown.
        for opened in self.camera_manager.poll_opening(&self.context.device) {
            match opened.result {
                Ok(size) => {
                    if let Some((name, inputs)) =
                        self.show_opened_camera(opened.id, &opened.decks, size)
                    {
                        count_camera_here(here, opened.id, name, inputs);
                    }
                    // Inputs that waited for it while another started it —
                    // a show's, one more of it added — show it this look,
                    // not at the next in two seconds, and are said with it.
                    self.camera_retry_at = None;
                }
                // Its input waits for it, tried again every two seconds; the
                // operator who just added it is told once.
                Err(e) if opened.asked => {
                    log::warn!("{e:#}");
                    self.notify_error(format!("{e:#} — its input waits for it"));
                }
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
        self.warn_camera_access(self.camera_manager.access_denied());
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
                    if let Some(name) = self.bind_waiting_camera(&uuid, id, size) {
                        count_camera_here(here, id, name, 1);
                    }
                }
            } else if !self.camera_manager.is_opening(id) {
                match starting.iter_mut().find(|(i, ..)| *i == id) {
                    Some((.., decks)) => decks.push(uuid),
                    None => starting.push((id, mode, vec![uuid])),
                }
            }
        }
        for (id, mode, decks) in starting {
            if let Err(e) = self
                .camera_manager
                .open_in_background(id, mode, decks, false)
            {
                log::warn!("{e:#}");
            }
        }
    }

    /// Cameras plugged back into another port, found there by their
    /// captures: their inputs keep the device's new unique ID, so the show
    /// saved finds it there next time — not another of the same model.
    fn follow_moved_cameras(&mut self, moves: &[(crate::camera::CameraId, String)]) {
        for (id, unique_id) in moves {
            for channel in self.mixer.channels_mut() {
                for slot in &mut channel.decks {
                    if slot.deck.camera_id() == Some(*id) {
                        slot.deck.camera_device = Some(unique_id.clone());
                    }
                }
            }
        }
    }

    /// Inputs wait for their cameras: say so, once, while macOS does not let
    /// Nida use them — refused, or still asking. macOS asks again after
    /// each update of an app not signed by a developer, and a show's camera
    /// inputs stayed black with only the log saying why (an M5 Max, 0.9.6,
    /// 2026-10-28). Forgotten once access is given.
    fn warn_camera_access(&mut self, denied: bool) {
        const KEY: &str = "camera-access";
        if !denied {
            self.session.notifications.clear_once_key_prefix(KEY);
            return;
        }
        self.session.notifications.notify_once(
            KEY,
            crate::notifications::NotificationLevel::Warning,
            "Cameras are black: macOS is not letting Nida use them. Click Allow on its \
             prompt (it can hide behind full-screen windows), or turn Nida on in System \
             Settings → Privacy & Security → Camera. After an update macOS asks again.",
        );
    }

    /// Display inputs the show has by their name alone — "Display 1", saved
    /// before displays were kept by their own identity — show whichever
    /// screen has that name today: said when one is new, where there is more
    /// than one screen to have it. A show's two such inputs, made of its 4K
    /// screen, showed a capture box's own screen with nothing said (a copy
    /// of a show, an M5 Max, 2026-11-10). Each is looked at every frame, and
    /// said again only once another comes.
    pub(crate) fn warn_captures_by_name(&mut self) {
        use crate::screen_capture::backend::CaptureTargetKind;
        let manager = &self.screen_capture_manager;
        // One screen alone is the one of its name.
        let several = manager
            .targets()
            .iter()
            .filter(|t| t.kind == CaptureTargetKind::Display)
            .nth(1)
            .is_some();
        let by_name = || {
            self.mixer
                .channels()
                .iter()
                .flat_map(|channel| &channel.decks)
                .filter(|slot| {
                    several
                        && slot
                            .deck
                            .screen_capture
                            .as_ref()
                            .is_some_and(|s| manager.goes_by_name(&s.identity))
                })
        };
        if by_name()
            .map(|slot| slot.deck.uuid())
            .eq(self.captures_by_name.iter().map(String::as_str))
        {
            return;
        }
        let now: Vec<String> = by_name().map(|slot| slot.deck.uuid().to_string()).collect();
        let mut names: Vec<String> = by_name()
            .filter_map(|slot| slot.deck.screen_capture.as_ref())
            .map(|s| crate::scene::CaptureTargetConfig::from(&s.identity).label())
            .collect();
        names.sort();
        names.dedup();
        let new = now.iter().any(|uuid| !self.captures_by_name.contains(uuid));
        let inputs = now.len();
        self.captures_by_name = now;
        if !new {
            return;
        }
        let message = format!(
            "{inputs} screen {} by a display's number ({}), not the screen itself: {} whichever \
             screen has that number today. Check {}, and pick its screen in its settings to \
             keep that screen.",
            if inputs == 1 { "input goes" } else { "inputs go" },
            names.join(", "),
            if inputs == 1 { "it shows" } else { "they show" },
            if inputs == 1 { "it" } else { "each" },
        );
        self.session.notifications.warn(message);
    }

    /// Capture settings given to inputs — a rate, a crop, the cursor — sent
    /// down to their captures, once; every input then shows and saves what
    /// its capture runs with. A screen or window is captured once however
    /// many inputs show it, so these settings are its inputs' in common:
    /// the capture is given what they were given, in the order they were,
    /// and runs as if each had been set on it there and then — the last
    /// word on a setting the last one given, whichever input that was
    /// given to.
    ///
    /// An input's own copy of them does not go down: an input that joined
    /// a screen the same frame another cropped it kept its copy uncropped,
    /// and put the whole screen back with the next thing changed on it. Nor
    /// is that copy compared with what the capture runs with, to send what
    /// differs: a cursor hidden on one input the frame it was shown on
    /// another was as the capture ran, and stayed shown; a crop moved right
    /// on one, its own older width narrowed to fit, took away the width
    /// another had just been given.
    pub(crate) fn send_capture_settings(&mut self) {
        use crate::screen_capture::{
            backend::{CaptureConfig, CaptureGiven},
            CaptureId,
        };
        let captures = &mut self.screen_capture_manager;
        // What the inputs were given, each for its capture; and the
        // captures given any, each as it runs.
        let mut given: Vec<(CaptureId, CaptureGiven)> = Vec::new();
        let mut changed: Vec<(CaptureId, CaptureConfig)> = Vec::new();
        for channel in self.mixer.channels_mut() {
            for slot in &mut channel.decks {
                let Some(state) = &mut slot.deck.screen_capture else {
                    continue;
                };
                if state.given.is_empty() {
                    continue;
                }
                let its = std::mem::take(&mut state.given);
                let id = state.capture_id;
                // Its screen or window not there: kept on the input.
                let Some(runs) = captures.config(id) else {
                    continue;
                };
                if state.presentation.is_some() {
                    // A PowerPoint show's window is captured apart, as its
                    // presentation has it: its settings whole, but for its
                    // cap (see `Deck::cap_capture_to_show`).
                    let mut config = state.config.clone();
                    config.scale_to = runs.scale_to;
                    captures.set_config(id, config);
                    continue;
                }
                if !changed.iter().any(|(i, _)| *i == id) {
                    changed.push((id, runs));
                }
                given.extend(its.into_iter().map(|g| (id, g)));
            }
        }
        // In the order given, whichever input each was given to.
        given.sort_by_key(|(_, g)| g.at);
        for (id, g) in given {
            if let Some((_, runs)) = changed.iter_mut().find(|(i, _)| *i == id) {
                runs.give(g.setting, g.value);
            }
        }
        for (id, runs) in changed {
            captures.set_config(id, runs);
        }
        for channel in self.mixer.channels_mut() {
            for slot in &mut channel.decks {
                let Some(state) = &mut slot.deck.screen_capture else {
                    continue;
                };
                if state.presentation.is_some() {
                    continue;
                }
                if let Some(runs) = captures.config(state.capture_id) {
                    state.config = state.config.as_runs(&runs);
                }
            }
        }
    }

    /// Each screen or window captured comes sized as its picture is drawn
    /// (see `ScreenCaptureManager::size_as_drawn`), every frame.
    /// `NIDA_CAPTURES_AS_DRAWN=0`, read once, leaves each as it came before:
    /// fitted within the show's size whatever it is drawn at.
    pub(crate) fn size_captures_as_drawn(&mut self) {
        static FOLLOWED: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
            std::env::var("NIDA_CAPTURES_AS_DRAWN").map_or(true, |v| v.trim() != "0")
        });
        if *FOLLOWED {
            self.size_captures_as_drawn_at(std::time::Instant::now());
        }
    }

    /// Each capture sized, at `now`, as the most of its inputs draw it: one
    /// fitted beside one that fills its frame, or is zoomed into, comes as
    /// that one needs — an input off air among them, which is sharp from
    /// the frame it is cut to.
    pub(crate) fn size_captures_as_drawn_at(&mut self, now: std::time::Instant) {
        use crate::screen_capture::{backend::Drawn, CaptureId};
        let modulation = self.mixer.modulation();
        let mut most: Vec<(CaptureId, Drawn)> = Vec::new();
        for slot in self.mixer.channels().iter().flat_map(|c| &c.decks) {
            let Some((id, drawn)) = slot.deck.capture_drawn(modulation) else {
                continue;
            };
            match most.iter_mut().find(|(i, _)| *i == id) {
                Some((_, of_all)) => *of_all = of_all.or(drawn),
                None => most.push((id, drawn)),
            }
        }
        for (id, drawn) in most {
            self.screen_capture_manager.size_as_drawn(id, drawn, now);
        }
    }

    /// `config` for an input come to the capture `id`, as that capture runs
    /// (see `CaptureConfig::as_runs`): one opened new, as this input asked;
    /// one another input runs already, as that one has it — what is on air
    /// does not change under it.
    pub(crate) fn as_its_capture_runs(
        &self,
        id: crate::screen_capture::CaptureId,
        config: crate::screen_capture::backend::CaptureConfig,
    ) -> crate::screen_capture::backend::CaptureConfig {
        match self.screen_capture_manager.config(id) {
            Some(runs) => config.as_runs(&runs),
            None => config,
        }
    }

    /// A show opened runs its captures as it saved them. A screen the show
    /// before captured too goes on in the one capture there is of it, which
    /// ran on with that show's rate, crop and cursor — and the inputs would
    /// take those for theirs (see `send_capture_settings`). Each capture is
    /// set as its first input in the show has it: as one opened new is, by
    /// that input.
    pub(crate) fn run_captures_as_saved(&mut self) {
        let mut set: Vec<crate::screen_capture::CaptureId> = Vec::new();
        for slot in self.mixer.channels().iter().flat_map(|c| &c.decks) {
            let Some(state) = &slot.deck.screen_capture else {
                continue;
            };
            if state.presentation.is_some() || set.contains(&state.capture_id) {
                continue;
            }
            set.push(state.capture_id);
            self.screen_capture_manager
                .set_config(state.capture_id, state.config.clone());
        }
    }

    /// A camera started in the background runs: the decks that waited for
    /// it show it — the first on the reference its start holds, the rest on
    /// one each. Let go again when none is left to show it. Gives the name
    /// it was waited for by and how many show it, to be said.
    fn show_opened_camera(
        &mut self,
        id: crate::camera::CameraId,
        decks: &[String],
        size: (u32, u32),
    ) -> Option<(String, usize)> {
        let mut shown = 0;
        let mut name = None;
        for uuid in decks {
            if shown > 0
                && self
                    .camera_manager
                    .open_camera(id, &self.context.device, None)
                    .is_err()
            {
                continue;
            }
            if let Some(waited_for) = self.bind_waiting_camera(uuid, id, size) {
                shown += 1;
                name.get_or_insert(waited_for);
            } else if shown > 0 {
                self.camera_manager.release_camera(id);
            }
        }
        if name.is_none() {
            self.camera_manager.release_camera(id);
        }
        name.map(|name| (name, shown))
    }

    /// A camera is here and the `inputs` that waited for it show it: said
    /// once, however many they are — a show's four inputs on one camera
    /// said "connected" four times as it opened.
    fn say_camera_here(&mut self, name: &str, inputs: usize) {
        if inputs == 1 {
            log::info!("Camera '{name}' is here: showing it");
        } else {
            log::info!("Camera '{name}' is here: showing it on {inputs} inputs");
        }
        self.session
            .notifications
            .info(format!("📹 {name} connected"));
    }

    /// Point the deck `uuid`, if it still waits for a camera, at camera `id`:
    /// the name it waited for it by, to be said (see `say_camera_here`).
    fn bind_waiting_camera(
        &mut self,
        uuid: &str,
        id: crate::camera::CameraId,
        size: (u32, u32),
    ) -> Option<String> {
        let unique_id = self.camera_manager.unique_id(id).map(str::to_string);
        let slot = self
            .mixer
            .channels_mut()
            .iter_mut()
            .flat_map(|c| c.decks.iter_mut())
            .find(|slot| slot.deck.uuid() == uuid && slot.deck.camera_waiting_for().is_some())?;
        let name = slot
            .deck
            .camera_waiting_for()
            .unwrap_or_default()
            .to_string();
        slot.deck.bind_camera(id, size);
        slot.deck.camera_device = unique_id;
        Some(name)
    }
}

/// One more camera come, shown on `inputs` inputs: counted in `here` with
/// the inputs it is shown on already this look.
fn count_camera_here(
    here: &mut Vec<(crate::camera::CameraId, String, usize)>,
    id: crate::camera::CameraId,
    name: String,
    inputs: usize,
) {
    match here.iter_mut().find(|(i, ..)| *i == id) {
        Some((.., shown)) => *shown += inputs,
        None => here.push((id, name, inputs)),
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

    /// Camera inputs waiting while macOS refuses the cameras are said once,
    /// not every look; given access and refused again, said again.
    #[test]
    fn refused_cameras_are_said_once() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let said = |app: &VardaApp| {
            app.session
                .notifications
                .visible()
                .iter()
                .filter(|n| n.message.contains("macOS is not letting Nida use them"))
                .count()
        };
        app.warn_camera_access(false);
        assert_eq!(said(&app), 0, "allowed");
        app.warn_camera_access(true);
        app.warn_camera_access(true);
        assert_eq!(said(&app), 1, "once");
        app.warn_camera_access(false);
        app.warn_camera_access(true);
        assert_eq!(said(&app), 2, "refused again");
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

    /// Reset input: a camera not open yet is looked for at once, and the
    /// sound devices are listed again; its layers stay on it; a layer not
    /// there is not found.
    #[test]
    fn a_reset_input_looks_for_its_camera_and_sound_again() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let first = camera_layer(&mut app, 3, "usb-atem");
        app.camera_retry_at = Some(std::time::Instant::now() + std::time::Duration::from_secs(2));
        assert!(matches!(app.cmd_reset_input(&first), CommandResult::Ok));
        assert!(app.camera_retry_at.is_none(), "looked for at once");
        assert!(app.audio_rescan_due.is_some(), "sound devices listed again");
        let still = app
            .mixer
            .channels()
            .iter()
            .flat_map(|c| &c.decks)
            .find(|slot| slot.deck.uuid() == first)
            .and_then(|slot| slot.deck.camera_id());
        assert_eq!(still, Some(3), "kept on its camera");
        assert!(matches!(
            app.cmd_reset_input("no-such-layer"),
            CommandResult::Err { .. }
        ));
    }

    /// A camera found on another port: the inputs on it keep its new
    /// unique ID, others theirs.
    #[test]
    fn inputs_follow_their_camera_to_another_port() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let moved = camera_layer(&mut app, 3, "usb-old");
        let other = camera_layer(&mut app, 4, "usb-other");
        app.follow_moved_cameras(&[(3, "usb-new".to_string())]);
        let device = |app: &VardaApp, uuid: &str| {
            app.mixer
                .channels()
                .iter()
                .flat_map(|c| &c.decks)
                .find(|slot| slot.deck.uuid() == uuid)
                .and_then(|slot| slot.deck.camera_device.clone())
        };
        assert_eq!(device(&app, &moved).as_deref(), Some("usb-new"));
        assert_eq!(device(&app, &other).as_deref(), Some("usb-other"));
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

    /// A camera's format is set on every layer of that camera and no other
    /// camera's — kept with the show — and is no step to undo: the camera
    /// opened again stays as it is.
    #[test]
    fn a_cameras_format_is_set_on_every_layer_of_that_camera() {
        use crate::engine::value::camera::CameraMode;
        let Some(mut app) = headless_app() else {
            return;
        };
        let first = camera_layer(&mut app, 3, "0xatem");
        let second = camera_layer(&mut app, 3, "0xatem");
        let other = camera_layer(&mut app, 4, "0xfacetime");
        let mode_of = |app: &VardaApp, uuid: &str| {
            let (ch, dk) = app.mixer.find_deck_by_uuid(uuid).expect("the layer");
            app.mixer.channels()[ch].decks[dk].deck.camera_mode
        };
        let fifty = CameraMode {
            width: 1920,
            height: 1080,
            fps: 50,
        };
        let set = EngineCommand::SetCameraMode {
            deck_uuid: first.clone(),
            mode: Some(fifty),
        };
        assert!(!crate::app::commands::command_is_undoable(&set));
        assert!(matches!(app.execute_command(set), CommandResult::Ok));
        assert_eq!(mode_of(&app, &first), Some(fifty));
        assert_eq!(mode_of(&app, &second), Some(fifty), "the same camera");
        assert_eq!(mode_of(&app, &other), None, "another camera");
        let best = EngineCommand::SetCameraMode {
            deck_uuid: second.clone(),
            mode: None,
        };
        assert!(matches!(app.execute_command(best), CommandResult::Ok));
        assert_eq!(mode_of(&app, &first), None, "its best again");
    }

    /// A new layer on a camera takes the format the camera is opened at:
    /// they share its capture.
    #[test]
    fn a_new_layer_on_a_camera_takes_its_format() {
        use crate::engine::value::camera::CameraMode;
        let Some(mut app) = headless_app() else {
            return;
        };
        let first = camera_layer(&mut app, 3, "0xatem");
        let fifty = Some(CameraMode {
            width: 1920,
            height: 1080,
            fps: 50,
        });
        let set = EngineCommand::SetCameraMode {
            deck_uuid: first,
            mode: fifty,
        };
        assert!(matches!(app.execute_command(set), CommandResult::Ok));
        let mut deck =
            crate::deck::Deck::new_from_camera(&app.context, 3, "Camera", 1920, 1080, 64, 64)
                .expect("a camera layer");
        deck.camera_device = Some("0xatem".into());
        app.read_as_its_camera_is(&mut deck);
        assert_eq!(deck.camera_mode, fifty);
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

    /// A camera the OS starts at once, capturing until told to stop.
    fn starts_at_once(
        _: &crate::camera::CameraDeviceInfo,
        _: Option<crate::camera::CameraMode>,
        _: &std::sync::Arc<crate::camera::Sink>,
        stop: &std::sync::atomic::AtomicBool,
        opened: &std::sync::mpsc::Sender<anyhow::Result<crate::camera::CameraMode>>,
    ) {
        let _ = opened.send(Ok(crate::camera::CameraMode {
            width: 64,
            height: 36,
            fps: 60,
        }));
        while !stop.load(std::sync::atomic::Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// A layer of a show waiting for the camera `name` of device `device`,
    /// in the first channel; its UUID.
    fn waiting_layer(app: &mut VardaApp, name: &str, device: &str) -> String {
        let mut deck = crate::deck::Deck::new_from_camera(
            &app.context,
            crate::camera::UNBOUND_CAMERA,
            name,
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

    /// A show's four inputs on one camera: the camera come, each shows it,
    /// and it is said once that it is connected — not four times (an M5
    /// Max, 2026-11-10). Two more inputs put on it later, the camera running
    /// already, are said once between them.
    #[test]
    fn a_camera_come_is_said_once_however_many_inputs_show_it() {
        let Some(mut app) = headless_app() else {
            return;
        };
        app.camera_manager =
            crate::camera::CameraManager::with_devices(&[("a", "ATEM")], starts_at_once);
        let id = app.camera_manager.devices()[0].id;
        let said = |app: &VardaApp| {
            app.session
                .notifications
                .history()
                .iter()
                .filter(|n| n.message == "📹 ATEM connected")
                .count()
        };
        let waits = |app: &VardaApp| {
            app.mixer
                .channels()
                .iter()
                .flat_map(|c| &c.decks)
                .filter(|slot| slot.deck.camera_waiting_for().is_some())
                .count()
        };
        let reconcile_until_shown = |app: &mut VardaApp| {
            let start = std::time::Instant::now();
            while waits(app) > 0 && start.elapsed() < std::time::Duration::from_secs(3) {
                app.reconcile_cameras();
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert_eq!(waits(app), 0, "every input shows its camera");
        };

        for _ in 0..4 {
            waiting_layer(&mut app, "ATEM", "a");
        }
        assert_eq!(said(&app), 0);
        reconcile_until_shown(&mut app);
        assert!(app.camera_manager.is_active(id));
        assert_eq!(said(&app), 1, "once for the four");

        // The camera running: two more inputs of it, at the next look.
        waiting_layer(&mut app, "ATEM", "a");
        waiting_layer(&mut app, "ATEM", "a");
        app.camera_retry_at = None;
        reconcile_until_shown(&mut app);
        assert_eq!(said(&app), 2, "once for the two");
    }

    /// Two inputs of a show wait for their camera — looked for again only
    /// in a while, or (`due`) at every look — and a third input of it is
    /// added, which starts it. Looked at until it runs: the inputs still
    /// waiting then, and the times it was said connected.
    fn camera_started_by_a_third_input(due: bool) -> Option<(usize, usize)> {
        use crate::engine::MixerCommands;
        let mut app = headless_app()?;
        app.camera_manager =
            crate::camera::CameraManager::with_devices(&[("a", "ATEM")], starts_at_once);
        let id = app.camera_manager.devices()[0].id;
        let waits = |app: &VardaApp| {
            app.mixer
                .channels()
                .iter()
                .flat_map(|c| &c.decks)
                .filter(|slot| slot.deck.camera_waiting_for().is_some())
                .count()
        };
        waiting_layer(&mut app, "ATEM", "a");
        waiting_layer(&mut app, "ATEM", "a");
        app.camera_retry_at =
            Some(std::time::Instant::now() + std::time::Duration::from_secs(3600));
        let channel = app.mixer.channels()[0].uuid().to_string();
        app.add_camera_deck(&channel, id, None).expect("added");
        assert_eq!(waits(&app), 3, "started in the background");

        let start = std::time::Instant::now();
        while waits(&app) == 3 && start.elapsed() < std::time::Duration::from_secs(3) {
            if due {
                app.camera_retry_at = None;
            }
            app.reconcile_cameras();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let said = app
            .session
            .notifications
            .history()
            .iter()
            .filter(|n| n.message == "📹 ATEM connected")
            .count();
        Some((waits(&app), said))
    }

    /// A camera started by an input just added shows on the show's inputs
    /// that waited for it in the same look — not two seconds on, when they
    /// are looked at next — and is said connected once for them all.
    #[test]
    fn a_camera_started_by_one_input_shows_on_those_that_waited_at_once() {
        let Some(shown) = camera_started_by_a_third_input(false) else {
            return;
        };
        assert_eq!(shown, (0, 1), "(inputs still waiting, times said)");
    }

    /// The same as the waiting inputs are looked at anyway: the camera was
    /// said connected for the input that started it, and again in that
    /// look for the two that found it open.
    #[test]
    fn a_camera_started_as_its_inputs_are_looked_at_is_said_once() {
        let Some(shown) = camera_started_by_a_third_input(true) else {
            return;
        };
        assert_eq!(shown, (0, 1), "(inputs still waiting, times said)");
    }
}
