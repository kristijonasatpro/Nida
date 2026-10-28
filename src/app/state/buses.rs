//! Production bus state mutations.
//!
//! Every method here names an explicit bus. There is deliberately no
//! "current bus" and no unaddressed variant, so a command cannot switch a bus
//! the caller did not name — an MVP acceptance criterion.

use super::super::{BusFeed, FeedPicture, VardaApp};
use crate::bus::{AutoRejected, Bus, BusCanvas, BusInputRef, KnownSources};
use crate::engine::value::render::OutputSource;
use crate::engine::{CommandResult, ErrorCode};

/// The error returned when a command names a bus that does not exist.
fn no_such_bus(uuid: &str) -> CommandResult {
    CommandResult::Err {
        code: ErrorCode::NotFound,
        message: format!("no bus with uuid {uuid}"),
    }
}

impl VardaApp {
    /// Create a bus with the given name and canvas size, showing Main until
    /// something else is taken to it (see `Bus::new_on_main`).
    pub fn cmd_add_bus(&mut self, name: &str, width: u32, height: u32) -> CommandResult {
        let bus = Bus::new_on_main(name, BusCanvas::new(width, height));
        match self.production_buses.add(bus) {
            Ok(uuid) => {
                log::info!("Added bus '{name}' ({width}x{height}) uuid={uuid}");
                CommandResult::OkWithId { uuid }
            }
            // A freshly generated UUID colliding means the generator is broken,
            // not that the caller did anything wrong.
            Err(rejected) => CommandResult::Err {
                code: ErrorCode::InternalError,
                message: format!("generated a bus uuid already in use: {}", rejected.uuid()),
            },
        }
    }

    /// Remove a bus.
    ///
    /// Destinations following it are left untouched here; resolving a dangling
    /// feed is the renderer's job, and it falls back to black rather than
    /// silently switching to another bus's program.
    pub fn cmd_remove_bus(&mut self, uuid: &str) -> CommandResult {
        match self.production_buses.remove(uuid) {
            Some(bus) => {
                log::info!("Removed bus '{}' uuid={uuid}", bus.name);
                self.rundown_forget_bus(uuid);
                CommandResult::Ok
            }
            None => no_such_bus(uuid),
        }
    }

    /// Rename a bus.
    /// Rename a bus — and its own screen, the surface and output window made
    /// for it (see [`Self::cmd_show_bus_on_screen`]) under its name, while
    /// they still have that name: an output called "TV" showing a bus called
    /// something else since would be a puzzle. Renamed by hand, they keep
    /// their own.
    pub fn cmd_rename_bus(&mut self, uuid: &str, name: &str) -> CommandResult {
        use crate::renderer::context::UnifiedOutput;
        let Some(bus) = self.production_buses.get_mut(uuid) else {
            return no_such_bus(uuid);
        };
        let old = std::mem::replace(&mut bus.name, name.to_string());
        let follows = self.surfaces_following(uuid);
        let shows_it = |assigned: &[crate::renderer::context::SurfaceAssignment]| {
            assigned.iter().any(|a| follows.contains(&a.surface_uuid))
        };
        for surface in &mut self.output.surface_manager.surfaces {
            if surface.name == old && follows.contains(&surface.uuid) {
                surface.name = name.to_string();
            }
        }
        for output in &mut self.output.outputs {
            match output {
                UnifiedOutput::Window(window) => {
                    if window.name == old && shows_it(&window.surface_assignments) {
                        window.rename(name);
                    }
                }
                // Its Blackmagic output (see `cmd_show_bus_on_blackmagic`).
                UnifiedOutput::Headless(h) => {
                    if h.name == old && matches!(&h.source, OutputSource::BusProgram(b) if b == uuid) {
                        h.name = name.to_string();
                    }
                }
            }
        }
        for config in &mut self.output.pending_output_creates {
            if config.name == old
                && config
                    .surface_assignments
                    .iter()
                    .any(|a| follows.contains(&a.surface_uuid))
            {
                config.name = name.to_string();
            }
        }
        CommandResult::Ok
    }

    /// Stage an input on one bus's Preview. Never touches Program.
    ///
    /// The source is resolved before the write, variant-aware: a `Channel`
    /// reference must name a channel and a `Deck` reference a deck. An
    /// unresolvable UUID is a `NotFound`, never a silent write — see
    /// `src/app/resolve.rs` and `/spec/api-addressing.md`. Accepting one would
    /// let a show be authored against sources that do not exist and only fail
    /// on air.
    pub fn cmd_set_bus_preview(&mut self, uuid: &str, input: BusInputRef) -> CommandResult {
        // While routing is quarantined there is no trustworthy mixer to
        // validate against, so staging anything but black is refused.
        //
        // Without this the quarantine has a second door: staging resolves
        // against the stale mixer and marks the endpoint resolved, and a CUT
        // then swaps that bit into Program — putting on air exactly the content
        // the load refused to trust. Clearing black is always allowed, since it
        // cannot resolve to anything.
        if self.production_buses.routing_quarantined() && input != BusInputRef::Black {
            return CommandResult::Err {
                code: ErrorCode::Unavailable,
                message: "bus routing is held black until a workspace loads with                           matching stage and scene; reload before re-routing"
                    .into(),
            };
        }
        if let Err(unknown) = self.resolve_bus_input(&input) {
            return unknown.into();
        }
        match self.production_buses.get_mut(uuid) {
            Some(bus) => {
                bus.set_preview(input);
                CommandResult::Ok
            }
            None => no_such_bus(uuid),
        }
    }

    /// Check that a bus input names an existing source of the right kind.
    pub(super) fn resolve_bus_input(
        &self,
        input: &BusInputRef,
    ) -> Result<(), super::super::resolve::UnknownEntity> {
        match input {
            BusInputRef::Black | BusInputRef::Master => Ok(()),
            BusInputRef::Channel(uuid) => self.resolve_channel(uuid).map(|_| ()),
            BusInputRef::Deck(uuid) => self.resolve_deck(uuid).map(|_| ()),
        }
    }

    /// Resize one bus's canvas.
    pub fn cmd_set_bus_canvas(&mut self, uuid: &str, width: u32, height: u32) -> CommandResult {
        match self.production_buses.get_mut(uuid) {
            Some(bus) => {
                // Resizing a region does not move it.
                let placed = bus.canvas();
                bus.set_canvas(BusCanvas::new(width, height).at(placed.origin_x, placed.origin_y));
                CommandResult::Ok
            }
            None => no_such_bus(uuid),
        }
    }

    /// Place one bus's region within the master raster.
    ///
    /// A region larger than the master, or hanging off its edge, is accepted
    /// and clamped when sampled rather than rejected: the master resolution can
    /// change under a saved show, and refusing to load a region that was valid
    /// when it was authored would be worse than showing a clamped one. The
    /// warning names it so the operator can see what happened.
    pub fn cmd_set_bus_canvas_region(
        &mut self,
        uuid: &str,
        width: u32,
        height: u32,
        origin_x: u32,
        origin_y: u32,
    ) -> CommandResult {
        let canvas = BusCanvas::new(width, height).at(origin_x, origin_y);
        if !canvas.fits_within(self.render_width, self.render_height) {
            log::warn!(
                "Bus region {}x{} at ({origin_x},{origin_y}) does not fit the {}x{} master; \
                 it will be clamped when sampled",
                canvas.width,
                canvas.height,
                self.render_width,
                self.render_height
            );
        }
        match self.production_buses.get_mut(uuid) {
            Some(bus) => {
                bus.set_canvas(canvas);
                CommandResult::Ok
            }
            None => no_such_bus(uuid),
        }
    }

    /// Set the default `AUTO` duration for one bus.
    pub fn cmd_set_bus_auto_duration(&mut self, uuid: &str, duration_secs: f32) -> CommandResult {
        if !duration_secs.is_finite() || duration_secs <= 0.0 {
            return CommandResult::Err {
                code: ErrorCode::InvalidInput,
                message: format!("auto duration must be finite and positive, got {duration_secs}"),
            };
        }
        match self.production_buses.get_mut(uuid) {
            Some(bus) => {
                bus.auto_duration = duration_secs;
                CommandResult::Ok
            }
            None => no_such_bus(uuid),
        }
    }

    /// Cut Preview to Program on one bus.
    pub fn cmd_bus_cut(&mut self, uuid: &str) -> CommandResult {
        match self.production_buses.get_mut(uuid) {
            Some(bus) => {
                bus.cut();
                CommandResult::Ok
            }
            None => no_such_bus(uuid),
        }
    }

    /// Leave one mixer layer out of a bus's Master, or put it back.
    ///
    /// Hiding needs the channel to exist now, so a typo cannot silently do
    /// nothing; showing a layer again is always allowed, so a hidden layer
    /// whose channel was since removed can still be cleared.
    pub fn cmd_set_bus_layer_hidden(
        &mut self,
        uuid: &str,
        channel_uuid: &str,
        hidden: bool,
    ) -> CommandResult {
        if hidden {
            if let Err(unknown) = self.resolve_channel(channel_uuid) {
                return unknown.into();
            }
        }
        match self.production_buses.get_mut(uuid) {
            Some(bus) => {
                bus.set_layer_hidden(channel_uuid, hidden);
                CommandResult::Ok
            }
            None => no_such_bus(uuid),
        }
    }

    /// Take a source's button off a bus's switcher row, or put it back. The
    /// row offers Black, Master and channels, so a deck is refused; so is a
    /// channel that is not there to hide.
    pub fn cmd_set_bus_source_hidden(
        &mut self,
        uuid: &str,
        input: &BusInputRef,
        hidden: bool,
    ) -> CommandResult {
        match input {
            BusInputRef::Deck(_) => {
                return CommandResult::Err {
                    code: ErrorCode::InvalidInput,
                    message: "A bus's row has buttons for Black, Master and channels, not layers"
                        .into(),
                };
            }
            BusInputRef::Channel(channel_uuid) if hidden => {
                if let Err(unknown) = self.resolve_channel(channel_uuid) {
                    return unknown.into();
                }
            }
            _ => {}
        }
        match self.production_buses.get_mut(uuid) {
            Some(bus) => {
                bus.set_source_hidden(input, hidden);
                CommandResult::Ok
            }
            None => no_such_bus(uuid),
        }
    }

    /// Put one bus fullscreen on a monitor, or take it off screen with `None`.
    ///
    /// A bus's screen is an output window showing a single full-frame surface
    /// that follows the bus. The first call makes both; later calls move that
    /// same window, so a bus never lands on a second screen by accident. The
    /// monitor is addressed by name, and one that is not connected is waited
    /// for exactly as a restored output waits.
    pub fn cmd_show_bus_on_screen(&mut self, uuid: &str, monitor: Option<&str>) -> CommandResult {
        use crate::engine::{OutputCommands, SurfaceCommands};
        use crate::renderer::context::{OutputTarget, UnifiedOutput};
        use crate::scene::{OutputConfig, OutputTargetConfig, SurfaceAssignmentConfig};

        let Some(bus) = self.production_buses.get(uuid) else {
            return no_such_bus(uuid);
        };
        let bus_name = bus.name.clone();
        let follows = self.surfaces_following(uuid);
        let shows = |assigned: &str| follows.iter().any(|s| s == assigned);

        let live: Vec<String> = self
            .output
            .outputs
            .iter()
            .filter_map(|o| match o {
                UnifiedOutput::Window(w)
                    if w.surface_assignments.iter().any(|a| shows(&a.surface_uuid)) =>
                {
                    Some(w.uuid.clone())
                }
                _ => None,
            })
            .collect();
        let pending =
            |c: &OutputConfig| c.surface_assignments.iter().any(|a| shows(&a.surface_uuid));

        // One place a bus goes from its card: a screen, or a Blackmagic
        // device (see `cmd_show_bus_on_blackmagic`) — off the device either way.
        self.take_bus_off_blackmagic(uuid, None);
        let Some(monitor) = monitor else {
            for output in &live {
                if let Err(e) = self.close_output(output) {
                    log::warn!("Taking bus '{bus_name}' off screen: {e}");
                }
            }
            self.output.pending_output_creates.retain(|c| !pending(c));
            return CommandResult::Ok;
        };

        if let Some(output) = live.first() {
            return match self.resolve_output(output) {
                Ok(idx) => self.cmd_set_output_target(
                    idx,
                    OutputTarget::Display {
                        name: monitor.to_string(),
                        monitor_index: 0,
                    },
                ),
                Err(e) => CommandResult::Err {
                    code: ErrorCode::NotFound,
                    message: e.to_string(),
                },
            };
        }
        if let Some(config) = self
            .output
            .pending_output_creates
            .iter_mut()
            .find(|c| pending(c))
        {
            config.target = OutputTargetConfig::Display {
                name: monitor.to_string(),
            };
            return CommandResult::OkWithId {
                uuid: config.uuid.clone(),
            };
        }

        let surface = self.add_polygon_surface(
            &bus_name,
            &[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            OutputSource::BusProgram(uuid.to_string()),
        );
        let mut config = OutputConfig::default_windowed();
        config.name = bus_name;
        config.target = OutputTargetConfig::Display {
            name: monitor.to_string(),
        };
        config.surface_assignments = vec![SurfaceAssignmentConfig {
            surface_uuid: surface,
            legacy_warp_mode: None,
            enabled: true,
        }];
        let output = config.uuid.clone();
        self.output.pending_output_creates.push(config);
        CommandResult::OkWithId { uuid: output }
    }

    /// Put a bus on a Blackmagic device, by name — its HDMI and SDI — as on a
    /// screen, or take it off its device with `None`. The device's output,
    /// whatever it showed, becomes the bus's: named after it, sending it, and
    /// started; made the first time, in 1080p at the show's rate. As a screen
    /// chosen does, it takes the bus off its screen and any other device.
    pub fn cmd_show_bus_on_blackmagic(&mut self, uuid: &str, device: Option<&str>) -> CommandResult {
        self.show_bus_on_blackmagic_among(uuid, device, &crate::decklink::known_devices())
    }

    /// [`Self::cmd_show_bus_on_blackmagic`], the devices connected being
    /// `devices`.
    pub(crate) fn show_bus_on_blackmagic_among(
        &mut self,
        uuid: &str,
        device: Option<&str>,
        devices: &[crate::decklink::DeckLinkDevice],
    ) -> CommandResult {
        use crate::engine::OutputCommands as _;
        use crate::renderer::context::{HeadlessOutput, OutputTarget, UnifiedOutput};

        let Some(bus) = self.production_buses.get(uuid) else {
            return no_such_bus(uuid);
        };
        let bus_name = bus.name.clone();
        let Some(device) = device else {
            self.take_bus_off_blackmagic(uuid, None);
            return CommandResult::Ok;
        };
        let Some(found) = devices.iter().find(|d| d.name == device) else {
            return CommandResult::Err {
                code: ErrorCode::NotFound,
                message: format!("no Blackmagic device named '{device}' is connected"),
            };
        };
        // Off its screen, and off another device.
        let follows = self.surfaces_following(uuid);
        let on_screen: Vec<String> = self
            .output
            .outputs
            .iter()
            .filter_map(|o| match o {
                UnifiedOutput::Window(w)
                    if w.surface_assignments.iter().any(|a| follows.contains(&a.surface_uuid)) =>
                {
                    Some(w.uuid.clone())
                }
                _ => None,
            })
            .collect();
        for output in &on_screen {
            if let Err(e) = self.close_output(output) {
                log::warn!("Taking bus '{bus_name}' off screen: {e}");
            }
        }
        self.output.pending_output_creates.retain(|c| {
            !c.surface_assignments
                .iter()
                .any(|a| follows.contains(&a.surface_uuid))
        });
        self.take_bus_off_blackmagic(uuid, Some(device));
        let on_device = |o: &UnifiedOutput| {
            matches!(o, UnifiedOutput::Headless(h)
                if matches!(&h.target, OutputTarget::DeckLink { device: d, .. } if d == device))
        };
        let output = if let Some(idx) = self.output.outputs.iter().position(on_device) {
            if let UnifiedOutput::Headless(h) = &mut self.output.outputs[idx] {
                h.source = OutputSource::BusProgram(uuid.to_string());
                h.name.clone_from(&bus_name);
            }
            self.output.outputs[idx].uuid().to_string()
        } else {
            let Some(format) = found.mode_for(self.target_fps) else {
                return CommandResult::Err {
                    code: ErrorCode::Unavailable,
                    message: format!("'{device}' says it has no video formats"),
                };
            };
            let headless = HeadlessOutput::new(
                &self.context.device,
                bus_name.clone(),
                OutputSource::BusProgram(uuid.to_string()),
                OutputTarget::DeckLink {
                    device: device.to_string(),
                    mode: format.name.clone(),
                    full_range: false,
                    follow_show: true,
                },
                self.render_width,
                self.render_height,
            );
            let output = headless.uuid.clone();
            log::info!("Bus '{bus_name}': a Blackmagic output on {device}");
            self.output.outputs.push(UnifiedOutput::Headless(headless));
            output
        };
        let running = self
            .output
            .outputs
            .iter()
            .any(|o| o.uuid() == output && matches!(o, UnifiedOutput::Headless(h) if h.active));
        if !running {
            if let error @ CommandResult::Err { .. } = self.cmd_start_output(&output) {
                return error;
            }
        }
        CommandResult::OkWithId { uuid: output }
    }

    /// Close the Blackmagic outputs showing a bus — but the one on `keep`,
    /// about to show it still.
    fn take_bus_off_blackmagic(&mut self, uuid: &str, keep: Option<&str>) {
        use crate::engine::OutputCommands as _;
        use crate::renderer::context::{OutputTarget, UnifiedOutput};
        let showing: Vec<String> = self
            .output
            .outputs
            .iter()
            .filter_map(|o| match o {
                UnifiedOutput::Headless(h)
                    if matches!(&h.source, OutputSource::BusProgram(b) if b == uuid) =>
                {
                    match &h.target {
                        OutputTarget::DeckLink { device, .. } if Some(device.as_str()) != keep => {
                            Some(h.uuid.clone())
                        }
                        _ => None,
                    }
                }
                _ => None,
            })
            .collect();
        for output in showing {
            if let Err(e) = self.close_output(&output) {
                log::warn!("Taking a bus off its Blackmagic device: {e}");
            }
        }
    }

    /// The monitor a bus is shown on (see [`Self::cmd_show_bus_on_screen`]),
    /// including one it is still waiting for or about to open on — or the
    /// Blackmagic device it is on, as [`crate::decklink::screen_label`] names
    /// it.
    pub fn bus_screen(&self, uuid: &str) -> Option<String> {
        use crate::renderer::context::{OutputTarget, UnifiedOutput};
        use crate::scene::OutputTargetConfig;

        let follows = self.surfaces_following(uuid);
        let shows = |assigned: &str| follows.iter().any(|s| s == assigned);
        let live = self.output.outputs.iter().find_map(|o| match o {
            UnifiedOutput::Window(w)
                if w.surface_assignments.iter().any(|a| shows(&a.surface_uuid)) =>
            {
                match w.configured_target() {
                    OutputTarget::Display { name, .. } => Some(name),
                    _ => None,
                }
            }
            _ => None,
        });
        live.or_else(|| {
            self.output
                .pending_output_creates
                .iter()
                .filter(|c| c.surface_assignments.iter().any(|a| shows(&a.surface_uuid)))
                .find_map(|c| match &c.target {
                    OutputTargetConfig::Display { name } => Some(name.clone()),
                    _ => None,
                })
        })
        .or_else(|| {
            self.output.outputs.iter().find_map(|o| match o {
                UnifiedOutput::Headless(h)
                    if matches!(&h.source, OutputSource::BusProgram(b) if b == uuid) =>
                {
                    match &h.target {
                        OutputTarget::DeckLink { device, .. } => {
                            Some(crate::decklink::screen_label(device))
                        }
                        _ => None,
                    }
                }
                _ => None,
            })
        })
    }

    /// Surfaces whose source is this bus's program.
    fn surfaces_following(&self, uuid: &str) -> Vec<String> {
        self.output
            .surface_manager
            .surfaces
            .iter()
            .filter(|s| matches!(&s.source, OutputSource::BusProgram(b) if b == uuid))
            .map(|s| s.uuid.clone())
            .collect()
    }

    /// Begin a timed transition from Program to Preview on one bus.
    ///
    /// A rejected take reports the reason rather than doing something else:
    /// an invalid duration is not downgraded to a cut, and a second `AUTO`
    /// during a take does not restart it.
    pub fn cmd_bus_auto(&mut self, uuid: &str, duration_secs: Option<f32>) -> CommandResult {
        // Read the clock *now*, not the frame-top sample. Native UI commands
        // execute after the frame's bus tick, so the cached reading can predate
        // the operator's action by a whole stalled frame — and the take would
        // then be charged for time before it existed.
        let now = self.frame_stats.now_secs();
        let Some(bus) = self.production_buses.get_mut(uuid) else {
            return no_such_bus(uuid);
        };
        match bus.auto(duration_secs, now) {
            Ok(()) => CommandResult::Ok,
            Err(rejected) => CommandResult::Err {
                code: match rejected {
                    AutoRejected::InvalidDuration => ErrorCode::InvalidInput,
                    AutoRejected::AlreadyTransitioning => ErrorCode::Unavailable,
                },
                message: rejected.to_string(),
            },
        }
    }

    /// Every channel and deck UUID a bus is allowed to reference.
    ///
    /// Buses address sources by stable UUID, so this is the set that decides
    /// whether persisted routing still resolves after a scene loads.
    pub fn known_source_uuids(&self) -> KnownSources {
        let mut known = KnownSources {
            master: true,
            ..KnownSources::default()
        };
        for channel in self.mixer_ref().channels() {
            known.channels.insert(channel.uuid().to_string());
            for slot in &channel.decks {
                known.decks.insert(slot.deck.uuid().to_string());
            }
        }
        known
    }

    /// Channels the mixer must render even when the crossfader has faded them
    /// out: the operator's cued previews, plus every channel a bus depends on —
    /// what it has on air, both ends of a running `AUTO`, and what it has staged
    /// on Preview, so a live camera there is already running when it is cut.
    ///
    /// The mixer skips a channel at zero effective opacity. That suits a single
    /// VJ mix, but a bus shows its channel whatever the crossfader says, and a
    /// skipped channel is never drawn — the bus would read black.
    pub fn watched_channels(&self) -> Vec<usize> {
        let mut watched = self.preview_channels.clone();
        for bus in &self.production_buses {
            let mut refs = vec![bus.effective_program(), bus.effective_preview()];
            if let Some(t) = bus.transition() {
                if t.from_resolved {
                    refs.push(&t.from);
                }
                if t.to_resolved {
                    refs.push(&t.to);
                }
            }
            for input in refs {
                // The master renders what it needs by itself; the master with
                // layers hidden needs every layer it still shows, since one the
                // crossfader has faded out of the master can be whole here.
                let channels = match self.input_source(input, bus.hidden_layers()) {
                    Some(OutputSource::Channel(channel) | OutputSource::Deck(channel, _)) => {
                        vec![channel]
                    }
                    Some(OutputSource::Channels(shown)) => shown,
                    _ => continue,
                };
                for channel in channels {
                    if !watched.contains(&channel) {
                        watched.push(channel);
                    }
                }
            }
        }
        watched
    }

    /// Decks shown on their own outside Main — alone on a bus's Program or
    /// Preview or at either end of its take, selected in the UI, or fed by
    /// themselves to an output, an LED screen or a surface — by UUID.
    /// They keep rendering where a deck covering their channel hides them.
    pub fn decks_shown_alone(&self) -> Vec<String> {
        let mut shown: Vec<String> = self
            .ui_selected_deck
            .and_then(|(ch, dk)| self.mixer.channels().get(ch)?.decks.get(dk))
            .map(|slot| slot.deck.uuid().to_string())
            .into_iter()
            .collect();
        for bus in &self.production_buses {
            let mut refs = vec![bus.effective_program(), bus.effective_preview()];
            if let Some(t) = bus.transition() {
                refs.push(&t.from);
                refs.push(&t.to);
            }
            for input in refs {
                if let BusInputRef::Deck(uuid) = input {
                    shown.push(uuid.clone());
                }
            }
        }
        // A deck an output, an LED screen or a surface shows by itself.
        let mut sources: Vec<&OutputSource> = self
            .output
            .surface_manager
            .surfaces
            .iter()
            .map(|surface| &surface.source)
            .collect();
        for output in &self.output.outputs {
            let (source, led) = match output {
                crate::renderer::UnifiedOutput::Window(w) => (None, w.led.as_ref()),
                crate::renderer::UnifiedOutput::Headless(h) => (Some(&h.source), h.led.as_ref()),
            };
            sources.extend(source);
            sources.extend(
                led.into_iter()
                    .flat_map(|l| l.screens.iter().map(|s| &s.source)),
            );
        }
        for source in sources {
            if let OutputSource::Deck(ch, dk) = source {
                if let Some(slot) = self
                    .mixer
                    .channels()
                    .get(*ch)
                    .and_then(|c| c.decks.get(*dk))
                {
                    shown.push(slot.deck.uuid().to_string());
                }
            }
        }
        shown
    }

    /// Resolve every bus's on-air content to a concrete source, once per frame.
    ///
    /// Destinations name a bus by UUID; the renderer needs an index. Doing it
    /// once per frame keeps the per-output path a lookup, and keeps the
    /// UUID-to-index resolution in one place where the rules are visible:
    ///
    /// - the *effective* program is used, so a route the bus cannot resolve —
    ///   or one a partial load quarantined — contributes nothing and the
    ///   destination reads black;
    /// - a UUID that no longer names anything is likewise absent, rather than
    ///   silently resolving to whatever now sits at that index;
    /// - while an `AUTO` runs the bus feeds a dissolve between its two ends at
    ///   the eased mix, an end that does not resolve reading black.
    pub fn bus_feeds(&self) -> std::collections::HashMap<String, BusFeed> {
        let mut feeds = std::collections::HashMap::new();
        for bus in &self.production_buses {
            let hidden = bus.hidden_layers();
            let picture = if let Some(t) = bus.transition() {
                let from = t
                    .from_resolved
                    .then(|| self.input_source(&t.from, hidden))
                    .flatten();
                let to = t
                    .to_resolved
                    .then(|| self.input_source(&t.to, hidden))
                    .flatten();
                if from.is_none() && to.is_none() {
                    continue;
                }
                FeedPicture::Dissolve {
                    from,
                    to,
                    mix: t.mix(),
                }
            } else {
                match self.input_source(bus.effective_program(), hidden) {
                    Some(source) => FeedPicture::Source(source),
                    None => continue,
                }
            };
            let (uv_scale, uv_offset) = bus.canvas().uv(self.render_width, self.render_height);
            feeds.insert(
                bus.uuid().to_string(),
                BusFeed {
                    picture,
                    uv_scale,
                    uv_offset,
                },
            );
        }
        feeds
    }

    /// The concrete source a bus input reads: `None` for black, or for a
    /// reference that no longer names anything. `hidden` is the bus's hidden
    /// layers, which only the master mix honours.
    fn input_source(&self, input: &BusInputRef, hidden: &[String]) -> Option<OutputSource> {
        match input {
            BusInputRef::Black => None,
            BusInputRef::Master => self.master_without(hidden),
            BusInputRef::Channel(uuid) => {
                self.resolve_channel(uuid).ok().map(OutputSource::Channel)
            }
            BusInputRef::Deck(uuid) => self
                .resolve_deck(uuid)
                .ok()
                .map(|(ch, dk)| OutputSource::Deck(ch, dk)),
        }
    }

    /// The picture a bus has on air, for a preview thumbnail: its settled
    /// program (the outgoing side during an `AUTO`), black when it has none.
    /// Shows the whole source; the bus region is applied by real outputs only.
    pub fn bus_program_view(&self, uuid: &str) -> Option<&wgpu::TextureView> {
        let bus = self.production_buses.get(uuid)?;
        let source = self.input_source(bus.effective_program(), bus.hidden_layers())?;
        Self::resolve_direct(self.mixer_ref(), &source, None)
    }

    /// The master mix with some layers (channel UUIDs) left out.
    ///
    /// Hiding nothing — or only layers that no longer exist — is the master
    /// itself, master effects and all. Otherwise it is the sub-mix of the
    /// layers still shown, composited with the same opacities and crossfade as
    /// the master; master effects are not applied to it, since they run once
    /// per frame on the master alone. Hiding every layer is black.
    fn master_without(&self, hidden: &[String]) -> Option<OutputSource> {
        let channels = self.mixer_ref().channels();
        let shown: Vec<usize> = channels
            .iter()
            .enumerate()
            .filter(|(_, ch)| !hidden.iter().any(|h| h == ch.uuid()))
            .map(|(idx, _)| idx)
            .collect();
        if shown.len() == channels.len() {
            Some(OutputSource::Master)
        } else if shown.is_empty() {
            None
        } else {
            Some(OutputSource::Channels(shown))
        }
    }

    /// Advance per-frame engine state by the frame's own `dt`.
    ///
    /// Called exactly once per frame, **after** queued commands and inputs are
    /// drained and **before** any render encoding. Two reasons for that
    /// position:
    ///
    /// - Advancing before commands would give a take started this frame a tick
    ///   of time it was never running for.
    /// - Advancing inside a render path would tick once per encode, and a bus
    ///   program is encoded to every destination that follows it (and, once
    ///   per-bus canvases land, at more than one resolution), so the buses
    ///   would drift apart.
    pub fn advance_frame_state(&mut self) {
        // The tick uses a live reading too, so a take started after the
        // previous tick is measured from its own start rather than from a
        // frame boundary it never saw.
        let now = self.frame_stats.now_secs();
        for uuid in self.production_buses.tick(now) {
            log::debug!("Bus {uuid} transition complete");
        }
        // After the buses, and at the same reading of the clock: a step that
        // goes on by itself is taken on the very frame the fade before it
        // lands, never a frame early by a later reading.
        self.advance_rundown(now);
    }

    /// Re-check every bus route against the sources that currently exist.
    ///
    /// Must run after anything that removes a channel or deck: a route that
    /// resolved a moment ago does not any more, and a bus that still believes
    /// it is resolved would keep a dangling source on air.
    pub fn reconcile_buses(&mut self) {
        let known = self.known_source_uuids();
        for unresolved in self.production_buses.reconcile(&known) {
            log::warn!("Bus routing no longer resolves: {unresolved}");
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::engine::{CommandResult, EngineCommand, SurfaceQueries as _};

    fn headless_app() -> Option<super::super::super::VardaApp> {
        let gpu = crate::renderer::context::GpuContext::new_headless().ok()?;
        super::super::super::VardaApp::new(gpu, &crate::testing::headless_config()).ok()
    }

    /// A bus renamed renames its own screen — the output and surface made
    /// for it under its name — so an output called "TV" never goes on
    /// showing a bus called something else; one renamed by hand keeps its
    /// own name.
    #[test]
    fn renaming_a_bus_renames_its_own_screen_but_not_one_named_by_hand() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let CommandResult::OkWithId { uuid: tv } = app.execute_command(EngineCommand::AddBus {
            name: "TV".into(),
            width: 1920,
            height: 1080,
        }) else {
            panic!("a bus");
        };
        let shown = app.execute_command(EngineCommand::ShowBusOnScreen {
            uuid: tv.clone(),
            monitor_name: Some("LG TV".into()),
        });
        assert!(matches!(shown, CommandResult::OkWithId { .. }), "{shown:?}");
        let names = |app: &super::super::super::VardaApp| {
            let outputs: Vec<String> = app.pending_outputs().iter().map(|c| c.name.clone()).collect();
            let surfaces: Vec<String> = app.surface_snapshot().into_iter().map(|s| s.name).collect();
            (outputs, surfaces)
        };
        assert_eq!(names(&app), (vec!["TV".to_string()], vec!["TV".to_string()]));

        let rename = |app: &mut super::super::super::VardaApp, name: &str| {
            app.execute_command(EngineCommand::RenameBus {
                uuid: tv.clone(),
                name: name.into(),
            })
        };
        assert!(matches!(rename(&mut app, "Stage TV"), CommandResult::Ok));
        assert_eq!(
            names(&app),
            (vec!["Stage TV".to_string()], vec!["Stage TV".to_string()])
        );

        app.output.pending_output_creates[0].name = "Left wall".into();
        assert!(matches!(rename(&mut app, "Hall TV"), CommandResult::Ok));
        assert_eq!(
            names(&app),
            (vec!["Left wall".to_string()], vec!["Hall TV".to_string()]),
            "the output renamed by hand keeps its name"
        );
    }

    /// A bus goes to a Blackmagic device as to a screen, from its card: the
    /// device's output becomes the bus's — made the first time, named after
    /// it, in 1080p at the show's rate; one device, one bus. A screen chosen
    /// takes it off the device, the device chosen takes it off its screen,
    /// Off takes it off both; renamed, its output is too.
    #[test]
    fn a_bus_goes_to_a_blackmagic_device_as_to_a_screen() {
        use crate::renderer::context::{OutputTarget, UnifiedOutput};
        let Some(mut app) = headless_app() else {
            return;
        };
        let add = |app: &mut super::super::super::VardaApp, name: &str| {
            let CommandResult::OkWithId { uuid } = app.execute_command(EngineCommand::AddBus {
                name: name.into(),
                width: 1920,
                height: 1080,
            }) else {
                panic!("a bus");
            };
            uuid
        };
        let (tv, projector) = (add(&mut app, "TV"), add(&mut app, "Projector"));
        // A device no machine has, so no test opens a real one: each start
        // is tried and fails, leaving the output in place.
        let mode = |name: &str, fps| crate::decklink::DeckLinkMode {
            name: name.into(),
            width: 1920,
            height: 1080,
            fps,
            code: 0,
        };
        let devices = [crate::decklink::DeckLinkDevice {
            name: "Test Box".into(),
            arrival: 0,
            modes: vec![mode("1080p50", 50.0), mode("1080p60", 60.0)],
        }];
        let blackmagic = |app: &super::super::super::VardaApp| -> Vec<(String, String, String)> {
            app.output
                .outputs
                .iter()
                .filter_map(|o| match o {
                    UnifiedOutput::Headless(h) => match &h.target {
                        OutputTarget::DeckLink { device, mode, .. } => {
                            Some((h.name.clone(), format!("{device} {mode}"), h.source.to_string()))
                        }
                        _ => None,
                    },
                    UnifiedOutput::Window(_) => None,
                })
                .collect()
        };
        let on_box = Some("Test Box (Blackmagic)".to_string());

        let sent = app.show_bus_on_blackmagic_among(&tv, Some("Test Box"), &devices);
        assert!(matches!(sent, CommandResult::Err { .. }), "the start is tried: {sent:?}");
        let tv_source = crate::engine::value::render::OutputSource::BusProgram(tv.clone()).to_string();
        assert_eq!(
            blackmagic(&app),
            [("TV".to_string(), "Test Box 1080p60".to_string(), tv_source.clone())]
        );
        assert_eq!(app.bus_screen(&tv), on_box);
        let api = app.build_engine_state();
        let reported = api.buses.iter().find(|b| b.uuid == tv).and_then(|b| b.screen.clone());
        assert_eq!(reported, on_box, "and the API says so");
        // Again: the same output, not a second.
        let _ = app.show_bus_on_blackmagic_among(&tv, Some("Test Box"), &devices);
        assert_eq!(blackmagic(&app).len(), 1);

        // Another bus to the same device: the device's output is now its.
        let _ = app.show_bus_on_blackmagic_among(&projector, Some("Test Box"), &devices);
        let projector_source =
            crate::engine::value::render::OutputSource::BusProgram(projector.clone()).to_string();
        assert_eq!(
            blackmagic(&app),
            [("Projector".to_string(), "Test Box 1080p60".to_string(), projector_source)]
        );
        assert_eq!((app.bus_screen(&tv), app.bus_screen(&projector)), (None, on_box.clone()));

        // Renamed, its output follows.
        let renamed = app.execute_command(EngineCommand::RenameBus {
            uuid: projector.clone(),
            name: "Hall".into(),
        });
        assert!(matches!(renamed, CommandResult::Ok));
        assert_eq!(blackmagic(&app)[0].0, "Hall");

        // A screen chosen: off the device.
        let _ = app.execute_command(EngineCommand::ShowBusOnScreen {
            uuid: projector.clone(),
            monitor_name: Some("LG TV".into()),
        });
        assert!(blackmagic(&app).is_empty());
        assert_eq!(app.bus_screen(&projector).as_deref(), Some("LG TV"));

        // The device chosen: off the screen.
        let _ = app.show_bus_on_blackmagic_among(&projector, Some("Test Box"), &devices);
        assert_eq!(blackmagic(&app).len(), 1);
        assert!(app.pending_outputs().is_empty(), "its screen's output let go");
        assert_eq!(app.bus_screen(&projector), on_box);

        // Off: off both.
        let _ = app.execute_command(EngineCommand::ShowBusOnScreen {
            uuid: projector.clone(),
            monitor_name: None,
        });
        assert!(blackmagic(&app).is_empty());
        assert_eq!(app.bus_screen(&projector), None);

        // A device not connected: nothing made.
        let missing = app.show_bus_on_blackmagic_among(&tv, Some("Gone Box"), &devices);
        assert!(matches!(missing, CommandResult::Err { .. }));
        assert!(blackmagic(&app).is_empty());
    }

    /// A bus rename undone puts its Blackmagic output's name back too, as it
    /// does its screen's.
    #[test]
    fn undoing_a_bus_rename_renames_its_blackmagic_output_back() {
        use crate::renderer::context::UnifiedOutput;
        let Some(mut app) = headless_app() else {
            return;
        };
        let CommandResult::OkWithId { uuid: bus } = app.execute_command(EngineCommand::AddBus {
            name: "PGM".into(),
            width: 1920,
            height: 1080,
        }) else {
            panic!("a bus");
        };
        let devices = [crate::decklink::DeckLinkDevice {
            name: "Test Box".into(),
            arrival: 0,
            modes: vec![crate::decklink::DeckLinkMode {
                name: "1080p60".into(),
                width: 1920,
                height: 1080,
                fps: 60.0,
                code: 0,
            }],
        }];
        let _ = app.show_bus_on_blackmagic_among(&bus, Some("Test Box"), &devices);
        let name = |app: &super::super::super::VardaApp| {
            app.output.outputs.iter().find_map(|o| match o {
                UnifiedOutput::Headless(h) => Some(h.name.clone()),
                UnifiedOutput::Window(_) => None,
            })
        };
        assert_eq!(name(&app).as_deref(), Some("PGM"));
        // As the command bus does for an undoable command.
        let before = app.history_snapshot_default();
        app.push_history(before);
        let renamed = app.execute_command(EngineCommand::RenameBus {
            uuid: bus.clone(),
            name: "PGM test".into(),
        });
        assert!(matches!(renamed, CommandResult::Ok));
        assert_eq!(name(&app).as_deref(), Some("PGM test"));
        app.execute_command(EngineCommand::Undo);
        assert_eq!(name(&app).as_deref(), Some("PGM"), "renamed back with the bus");
    }
}
