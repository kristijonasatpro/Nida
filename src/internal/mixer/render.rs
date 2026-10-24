//! Mixer render pipeline — compositing, master effects, sub-mixes.

use super::{AutoCrossfade, CrossfadeEasing, FrameTiming, Mixer};
use crate::renderer::{GpuContext, ISFUniforms};
use anyhow::Result;

/// Stack-friendly container for per-channel compositing opacities.
///
/// The common 2-channel case uses a fixed-size array on the stack, avoiding a
/// heap allocation.  N-channel mode falls back to `Vec`.  Derefs to `&[f32]`
/// so callers can use `.iter()`, `.get()`, indexing, etc. unchanged.
enum CompositingOpacities {
    Two([f32; 2]),
    Many(Vec<f32>),
}

impl std::ops::Deref for CompositingOpacities {
    type Target = [f32];

    fn deref(&self) -> &[f32] {
        match self {
            CompositingOpacities::Two(arr) => arr,
            CompositingOpacities::Many(vec) => vec,
        }
    }
}

/// Resolve one deck-scoped modulation key, or `None` when nothing is assigned.
///
/// `scratch` is reused across every deck and every frame, so a pass over a scene
/// full of video decks allocates nothing in the steady state. The
/// `has_modulation` check comes first because the overwhelmingly common answer
/// is "nothing", and answering it should not cost a resolve.
fn resolve_deck_key(
    modulation: &crate::modulation::ModulationEngine,
    scratch: &mut String,
    prefix: &str,
    name: &str,
) -> Option<crate::modulation::ResolvedModulation> {
    scratch.clear();
    scratch.push_str(prefix);
    scratch.push(':');
    scratch.push_str(name);
    if !modulation.has_modulation(scratch) {
        return None;
    }
    Some(modulation.resolve(scratch, None))
}

impl Mixer {
    /// Resolve every timebase for this frame.
    ///
    /// `beat_time` is the clock's monotonic beat position, or `None` when no
    /// clock source is active, which freezes beat-locked sources. `transport`
    /// is `None` until the transport has run, which freezes transport-locked
    /// sources. `free_run_time` is `None` whenever frames are paced by the wall,
    /// which is every live frame. See /spec/timebase.md.
    fn resolve_timebases(
        &mut self,
        free_run_time: Option<f32>,
        beat_time: Option<f64>,
        transport: Option<crate::timebase::TransportSample>,
    ) -> crate::timebase::TimebaseSet {
        self.timebases.resolve(crate::timebase::TimebaseInput {
            free_run_time: free_run_time.unwrap_or_else(|| self.start_time.elapsed().as_secs_f32()),
            beat_time,
            transport,
        })
    }

    /// Pre-update modulation engine with latest audio + analyzer data.
    pub fn update_modulation(
        &mut self,
        beat_time: Option<f64>,
        transport: Option<crate::timebase::TransportSample>,
        audio_values: &crate::modulation::AudioValues,
        analyzer_values: &crate::modulation::AnalyzerValues,
    ) {
        let timebases = self.resolve_timebases(None, beat_time, transport);
        self.modulation
            .update(&timebases, audio_values, analyzer_values);
    }

    /// Drive knob/fader macros from any modulation assigned to their value, then
    /// fan the modulated value out to every target. The macro's stored value is
    /// the base (manual set point); modulation rides on top as an offset. Only
    /// macros with an active assignment on `macro_<uuid>:value` pay the cost.
    /// See `/spec/macro-controls.md` §Macro Value Modulation.
    ///
    /// Must run after `ModulationEngine::update` and before compositing so
    /// opacity/param targets take effect the same frame.
    pub fn apply_macro_modulation(&mut self) {
        if self.macros.macros().is_empty() || self.modulation.source_count() == 0 {
            return;
        }
        // Gather writes first (shared borrows of macros + modulation), then apply
        // them mutably through the router — mirrors the `macro/<uuid>/value` route.
        let mut writes: Vec<(String, f32)> = Vec::new();
        for m in self.macros.macros() {
            let key = crate::macros::Macro::value_mod_key(&m.uuid);
            if !self.modulation.has_modulation(&key) {
                continue;
            }
            let resolved = self.modulation.resolve(&key, None);
            // An absolute source replaces the macro's manual set point, so it is
            // expressed as the offset that lands on it. `modulated_fanout` keeps
            // the stored base untouched either way.
            let offset = resolved
                .absolute
                .map_or(resolved.additive, |v| v - m.value + resolved.additive);
            writes.extend(m.modulated_fanout(offset));
        }
        for (path, value) in writes {
            if let Err(e) = crate::param_router::apply_param_by_path(self, &path, value) {
                log::debug!("macro modulation target '{path}' skipped: {e}");
            }
        }
    }

    /// Let modulation drive video playback.
    ///
    /// Speed and playhead go to the decode thread as levels, because they are
    /// continuous and only the newest value matters. Play, loop mode, and
    /// scaling mode are discrete, so they are written only when the option the
    /// modulator points at differs from the one in force, which keeps a settled
    /// modulator from generating any cross-thread traffic at all.
    ///
    /// Must run after [`crate::modulation::ModulationEngine::update`] and before
    /// [`Self::publish_video_chase`], so a chasing clip weighs this frame's
    /// modulation against this frame's transport rather than the next one's.
    ///
    /// See /spec/video-playback-modulation.md.
    pub fn apply_video_modulation(&mut self, transport_running: bool) {
        use crate::video::modulation as vm;

        if !self.modulation.has_modulation_for_any() {
            return;
        }
        // Disjoint field borrows: the engine is read while the decks it drives
        // are written.
        let modulation = &self.modulation;
        let mut key = String::with_capacity(48);

        for channel in &mut self.channels {
            for slot in &mut channel.decks {
                // A sleeping clip is frozen rather than played on silently, so
                // nothing may drive it: the same show position has to look the
                // same on the second run. See /spec/deck-residency.md.
                let awake = slot.source_demand.wants_frames();
                let mut scaling = None;

                {
                    let deck = &slot.deck;
                    let prefix = deck.param_prefix();

                    // Scaling is a property of any deck with a source texture,
                    // not just a video one.
                    if let (Some(current), Some(resolved)) = (
                        deck.scaling_mode(),
                        resolve_deck_key(modulation, &mut key, prefix, vm::SCALING_MODE),
                    ) {
                        let next = crate::param_router::scaling_mode_from_value(
                            vm::discrete_value(&resolved),
                        );
                        if next != current {
                            scaling = Some(next);
                        }
                    }

                    if let Some(snap) = deck.playback_snapshot() {
                        // The servo owns the timeline while chasing, so neither
                        // rate nor playhead is a modulator's to hold there. See
                        // /spec/video-playback-modulation.md § Authority.
                        let chasing = deck
                            .video_transport_sync()
                            .is_some_and(|s| s.mode.is_chasing(transport_running));

                        let speed = if chasing {
                            None
                        } else {
                            resolve_deck_key(modulation, &mut key, prefix, vm::SPEED)
                                .map(|r| vm::effective_speed(snap.speed, &r))
                        };

                        let position = if chasing {
                            vm::PositionTarget::Free
                        } else {
                            resolve_deck_key(modulation, &mut key, prefix, vm::POSITION).map_or(
                                vm::PositionTarget::Free,
                                |r| {
                                    vm::position_target(
                                        &r,
                                        snap.in_point,
                                        snap.effective_out(),
                                        snap.duration,
                                    )
                                },
                            )
                        };

                        deck.publish_video_modulation(if awake {
                            vm::PlaybackModulation { speed, position }
                        } else {
                            vm::PlaybackModulation::default()
                        });

                        if awake {
                            if let Some(r) =
                                resolve_deck_key(modulation, &mut key, prefix, vm::PLAY)
                            {
                                let want = vm::play_gate(&r, snap.playing);
                                if want != snap.playing {
                                    deck.video_set_playing(want);
                                }
                            }
                            if let Some(r) =
                                resolve_deck_key(modulation, &mut key, prefix, vm::LOOP_MODE)
                            {
                                let next = crate::param_router::loop_mode_from_value(
                                    vm::discrete_value(&r),
                                );
                                if next != snap.loop_mode {
                                    deck.video_set_loop_mode(next);
                                }
                            }
                        }
                    }
                }

                if let Some(mode) = scaling {
                    slot.deck.set_scaling_mode(mode);
                }
            }
        }
    }

    /// Let the arrangement drive the decks it owns, and mark those decks so
    /// their auto-transitions stand down for the frame.
    ///
    /// Authority is per lane and gated on the transport having actually run
    /// ([`crate::arrangement::Authority`]), so a cold start renders the saved
    /// scene rather than an arrangement's pre-show state. A lane whose opacity
    /// the performer has grabbed resolves to no absolute value at all, which is
    /// how an override leaves the deck exactly where they put it.
    ///
    /// See /spec/arrangement.md § Authority.
    pub(super) fn apply_arrangement(
        &mut self,
        transport: Option<crate::timebase::TransportSample>,
        preview_channels: &[usize],
    ) {
        // Moved out rather than borrowed so the deck writes below can take
        // `&mut self`; this is an `Option` move, not a deep clone.
        let Some(arrangement) = self.arrangement.take() else {
            self.clear_arrangement_authority();
            return;
        };

        // Cleared unconditionally first: a lane deleted or emptied since the
        // last frame must give its deck back rather than leave it pinned.
        self.clear_arrangement_authority();
        self.arrangement_blacked_out = false;

        let engaged =
            crate::arrangement::Authority::resolve(Some(&arrangement), transport.as_ref())
                .is_engaged();
        // Outside the arranged range the arrangement has said nothing, so idle
        // behaviour speaks for it. `HoldPerformance` means exactly that: stay
        // out of the way. Gaps *inside* the range are authored silence and are
        // not idle.
        let outside_range = transport.is_some_and(|t| !arrangement.within_range(t.position));
        let idle_outside_range =
            outside_range && arrangement.idle == crate::arrangement::IdleBehaviour::HoldPerformance;

        if engaged && !idle_outside_range {
            // A Fade step targets a *pair* of channels, so transition sequences
            // cannot partition per lane the way auto-transitions do. The rule is
            // therefore blunt: a sequence still free-running when the
            // arrangement takes authority is stopped rather than left to fight
            // the crossfader automation.
            self.stop_free_running_sequences();

            let mut key = String::with_capacity(48);
            let mut driven = 0_u32;
            let mut visible = 0_u32;
            for lane in &arrangement.lanes {
                if !lane.drives_anything() {
                    continue;
                }
                let Some((ch, dk)) = self.find_deck_by_uuid(&lane.deck_uuid) else {
                    continue;
                };
                self.channels[ch].decks[dk].arrangement_authority = true;

                crate::arrangement::write_opacity_param_key(&mut key, &lane.deck_uuid);

                // A previewed or tapped channel is being watched off-air, which
                // is the same exemption the opacity cull already makes; one on
                // Main, or this deck alone there, is on air whatever the
                // schedule says.
                let watched = preview_channels.contains(&ch)
                    || self.channels[ch].tapped
                    || self.main.shows(&super::MainSource::Deck(lane.deck_uuid.clone()))
                    || self
                        .main
                        .shows(&super::MainSource::Channel(self.channels[ch].uuid().to_string()));
                self.channels[ch].decks[dk].source_demand = match transport {
                    Some(t) if !watched => self.opacity_demand(&key, t.position),
                    _ => crate::arrangement::SourceDemand::Unscheduled,
                };

                if let Some(value) = self.modulation.resolve(&key, None).absolute {
                    let opacity = value.clamp(0.0, 1.0);
                    self.channels[ch].decks[dk].opacity = opacity;
                    driven += 1;
                    if opacity > 0.0 {
                        visible += 1;
                    }
                }
            }

            if let crate::arrangement::IdleBehaviour::ShowDeck { deck_uuid } = &arrangement.idle {
                // "Run this loop until the schedule starts" is a normal
                // installation requirement, so the pre-show state needs
                // something to *be* rather than something to fail at.
                if outside_range {
                    if let Some((ch, dk)) = self.find_deck_by_uuid(deck_uuid) {
                        self.channels[ch].decks[dk].opacity = 1.0;
                        self.channels[ch].decks[dk].arrangement_authority = true;
                        // The one deck on screen before the show starts, which
                        // its own lane may have just called idle.
                        self.channels[ch].decks[dk].source_demand =
                            crate::arrangement::SourceDemand::Unscheduled;
                        visible += 1;
                    }
                }
            }

            // Idle and broken look identical on the output, so the fact that the
            // arrangement is driving everything to nothing gets reported rather
            // than inferred. Never an intervention: deliberate blackouts are
            // legitimate.
            self.arrangement_blacked_out =
                driven > 0 && visible == 0 && transport.is_some_and(|t| t.running);
        }

        self.arrangement = Some(arrangement);
    }

    /// Hand every deck back to Performance mode.
    fn clear_arrangement_authority(&mut self) {
        for channel in &mut self.channels {
            for slot in &mut channel.decks {
                slot.arrangement_authority = false;
                slot.source_demand = crate::arrangement::SourceDemand::Unscheduled;
            }
        }
    }

    /// What the arrangement predicts about the source behind one opacity key.
    ///
    /// Deliberately conservative, and every uncertain answer is `Unscheduled`:
    /// gating one deck too few costs a decode thread nobody needed, and gating
    /// one too many is a black frame in front of an audience.
    ///
    /// See /spec/deck-residency.md § When a deck is scheduled.
    fn opacity_demand(&self, key: &str, position: f64) -> crate::arrangement::SourceDemand {
        use crate::arrangement::SourceDemand;

        // A hand on the fader is the least predictable driver there is.
        if self.modulation.is_overridden(key) {
            return SourceDemand::Unscheduled;
        }

        crate::arrangement::residency::demand(
            self.modulation.assignments_for(key).iter().map(|a| {
                let entry = self.modulation.find_source_by_uuid(&a.source_id)?;
                // A curve on any other timebase is positioned in beats or in
                // wall-clock seconds, so a window in show seconds says nothing
                // about it.
                if entry.timebase != crate::timebase::Timebase::Transport {
                    return None;
                }
                match &entry.source {
                    crate::modulation::ModulationSource::Envelope { breakpoints, .. } => {
                        Some(breakpoints.as_slice())
                    }
                    _ => None,
                }
            }),
            position,
        )
    }

    /// Render all channels and composite them via crossfader, then apply master effects.
    /// `target_fps` is used for adaptive deck render skipping budget calculation.
    /// `preview_channels` are force-rendered even when culled by opacity, so their
    /// off-air previews update live — without affecting the compositor (see
    /// /spec/channel-preview.md).
    ///
    /// # Errors
    ///
    /// Returns an error if any channel fails to render its decks, or if master
    /// effect / tonemap / LUT compositing fails on the GPU device.
    ///
    /// # Panics
    ///
    /// Panics if a timestamp callback marks a staging buffer ready after its
    /// mapped range has become unavailable.
    pub fn render(
        &mut self,
        context: &GpuContext,
        inputs: &super::FrameInputs<'_>,
        target_fps: u32,
        preview_channels: &[usize],
    ) -> Result<()> {
        let super::FrameInputs {
            audio_data,
            audio_values,
            analyzer_values,
            beat_time,
            transport,
            free_run_time,
        } = *inputs;
        let now = std::time::Instant::now();
        let dt = (now - self.last_render_time).as_secs_f32();
        self.last_render_time = now;
        self.main.tick(dt);

        // Tick auto-crossfade
        if let Some(auto) = &mut self.auto_crossfade {
            if let Some(value) = auto.tick(dt) {
                self.crossfader = value;
            } else {
                let target = auto.to;
                self.crossfader = target;
                self.auto_crossfade = None;
                log::info!("Auto-crossfade complete, crossfader = {target:.2}");
            }
        }

        // Handle beat-synced crossfade
        if let Some(bsc) = &mut self.beat_sync_crossfade {
            if !bsc.started {
                let phase = audio_data.beat_phase();
                if phase < 0.05 && audio_data.bpm.is_some() {
                    let bpm = audio_data.bpm.unwrap_or(120.0);
                    let duration_secs = bsc.beats * 60.0 / bpm;
                    bsc.auto = Some(AutoCrossfade::new(
                        self.crossfader,
                        bsc.to,
                        duration_secs,
                        CrossfadeEasing::EaseInOut,
                    ));
                    bsc.started = true;
                    log::info!(
                        "Beat-synced crossfade started: {:.1} beats at {:.0} BPM = {:.2}s",
                        bsc.beats,
                        bpm,
                        duration_secs
                    );
                }
            }

            if let Some(auto) = &mut bsc.auto {
                if let Some(value) = auto.tick(dt) {
                    self.crossfader = value;
                } else {
                    let target = bsc.to;
                    self.crossfader = target;
                    self.beat_sync_crossfade = None;
                    log::info!("Beat-synced crossfade complete, crossfader = {target:.2}");
                }
            }
        }

        // Tick transition sequence
        let bpm = audio_data.bpm.map(f64::from);
        self.tick_sequence(dt, bpm);

        // ── GPU Timestamp: read previous frame results ──────────────────
        // Only read if the map_async callback has fired (buffer is actually mapped).
        // staging_mapped_idx holds the index of the ready buffer, or usize::MAX if none.
        let ready_idx = self
            .staging_mapped_idx
            .load(std::sync::atomic::Ordering::Acquire);
        if ready_idx != usize::MAX {
            if let Some(ref staging) = self.staging_buffers {
                let buf = &staging[ready_idx];
                {
                    let slice = buf.slice(..);
                    let mapped = slice
                        .get_mapped_range()
                        .expect("timestamp staging callback only marks mapped buffers ready");
                    let timestamps: &[u64] = bytemuck::cast_slice(&mapped);
                    let period_us = self.timestamp_period / 1000.0; // ns → µs
                    self.last_frame_gpu_times.clear();
                    for &(ch_idx, deck_idx, begin, end) in &self.prev_timing_allocations {
                        if (end as usize) < timestamps.len() {
                            let begin_ts = timestamps[begin as usize];
                            let end_ts = timestamps[end as usize];
                            if end_ts > begin_ts {
                                let gpu_us = (end_ts - begin_ts) as f32 * period_us;
                                self.last_frame_gpu_times.insert((ch_idx, deck_idx), gpu_us);
                            }
                        }
                    }
                    drop(mapped);
                }
                buf.unmap();
                self.staging_mapped_idx
                    .store(usize::MAX, std::sync::atomic::Ordering::Release);
                // The map has been consumed and the buffer unmapped — clear the
                // in-flight guard so the resolve path may issue the next map.
                self.timing_map_inflight = false;
            }
        }

        // Apply GPU timing results to deck slots (EMA smoothing)
        if !self.last_frame_gpu_times.is_empty() {
            for (ch_idx, channel) in self.channels.iter_mut().enumerate() {
                for (dk_idx, slot) in channel.decks.iter_mut().enumerate() {
                    if let Some(&gpu_us) = self.last_frame_gpu_times.get(&(ch_idx, dk_idx)) {
                        if slot.gpu_render_cost_us > 0.0 {
                            slot.gpu_render_cost_us = 0.2 * gpu_us + 0.8 * slot.gpu_render_cost_us;
                        } else {
                            slot.gpu_render_cost_us = gpu_us;
                        }
                    }
                }
            }
        }

        // Update global modulation engine
        let t_modulation = std::time::Instant::now();
        let timebases = self.resolve_timebases(free_run_time, beat_time, transport);
        self.modulation
            .update(&timebases, audio_values, analyzer_values);
        // Shader uniforms follow the free-running clock: `TIME` is expected to advance
        // smoothly regardless of what any modulator is locked to.
        let time = timebases.free_run().time;
        // Drive any modulation-assigned macros and fan their values out to targets
        // before compositing reads opacities/params this frame.
        self.apply_macro_modulation();
        // The arrangement runs after macros so a scheduled region wins over a
        // macro fan-out on the same deck: a macro turn is a live gesture, and
        // taking a deck back from the show is what an override is for.
        self.apply_arrangement(transport, preview_channels);
        // Last, because whether a clip is chasing decides whether a modulator
        // may touch its playhead, and the arrangement is what puts it to sleep.
        self.apply_video_modulation(transport.is_some_and(|t| t.running));
        let modulation_us = t_modulation.elapsed().as_micros();

        // Compute effective opacity per channel (stack-allocated for the common 2-channel case)
        let channel_count = self.channels.len();
        let two_ch_buf: [f32; 2];
        let n_ch_buf: Vec<f32>;
        let effective_opacities: &[f32] = if self.mix_mode == super::MixMode::Program {
            n_ch_buf = self.program_weights();
            &n_ch_buf
        } else if channel_count == 2 {
            two_ch_buf = [
                (1.0 - self.crossfader) * self.channel_opacity(0),
                self.crossfader * self.channel_opacity(1),
            ];
            &two_ch_buf
        } else {
            n_ch_buf = (0..channel_count)
                .map(|i| self.channel_opacity(i))
                .collect();
            &n_ch_buf
        };

        // Always tick video frames on every channel so players stay in sync
        // even when a channel is fully faded out by the crossfader.
        // Uses a dedicated encoder for double-buffered staging uploads
        // (copy_buffer_to_texture) to avoid per-frame staging allocation stalls.
        // The finished command buffer is NOT submitted here — it is passed as a
        // prefix to the first channel's deck submit, eliminating a separate
        // queue.submit() call that would stall under GPU pressure.
        let t_video_tick = std::time::Instant::now();
        let chase = transport.map_or_else(crate::video::VideoChaseBroadcast::default, |t| {
            crate::video::VideoChaseBroadcast {
                position: t.position,
                running: t.running,
                fps: t.fps,
            }
        });
        let chase_disc = transport.is_some_and(|t| t.discontinuity);
        self.publish_video_chase(chase, chase_disc);
        let mut video_encoder =
            context
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Video Upload Encoder"),
                });
        let FrameTiming { refresh_at, held } = self.frame_timing;
        for channel in &mut self.channels {
            channel.tick_video_frames(&mut video_encoder, target_fps, refresh_at, held);
        }
        let mut prefix_cmds = vec![video_encoder.finish()];
        let video_tick_us = t_video_tick.elapsed().as_micros();

        // Count total active decks from last frame for per-deck budget calculation
        let total_active_decks: u32 = self.channels.iter().map(|ch| ch.active_deck_count).sum();

        // Snapshot the current GPU load ratio for this frame's skip decisions.
        // This is updated at the end of the frame based on actual vs CPU-measured time.
        let gpu_load_ratio = self.gpu_load_ratio;
        // Only when last frame's decks together ran over a frame's time may a
        // deck on Auto be drawn less often (see `channel::auto_draws`).
        let frame_over_budget = crate::channel::decks_over_budget(
            self.channels
                .iter()
                .filter(|ch| ch.active_deck_count > 0)
                .flat_map(|ch| ch.decks.iter().map(move |slot| (ch, slot)))
                .filter(|(ch, slot)| {
                    (!slot.mute && slot.opacity > 0.0)
                        || ch.shown_alone.iter().any(|s| s == slot.deck.uuid())
                })
                .map(|(_, slot)| slot.render_cost_us * gpu_load_ratio),
            target_fps,
        );

        // Allocate per-frame GPU timing context (128 queries = 64 deck measurements)
        let mut timing_frame = if self.query_set.is_some() {
            Some(super::GpuTimingFrame::new(128))
        } else {
            None
        };

        // Decks shown on their own, which render even where a covering deck
        // hides them in their channel: the app's, and any alone on Main.
        let mut shown_alone = self.decks_shown_alone.clone();
        let main = &self.main;
        for source in std::iter::once(&main.program).chain(
            main.fade
                .iter()
                .flat_map(|f| [&f.from, &main.program].into_iter()),
        ) {
            if let super::MainSource::Deck(uuid) = source {
                shown_alone.push(uuid.clone());
            }
        }
        let merging = self.merging_channels();
        for (idx, channel) in self.channels.iter_mut().enumerate() {
            channel.shown_alone.clone_from(&shown_alone);
            channel.draw_visible_parts = self.draw_visible_parts;
            channel.draw_every_deck = merging[idx];
        }

        let profiling = self.perf_profile_frames > 0;
        let t_channels = std::time::Instant::now();
        let mut rendered_channels: u32 = 0;
        let mut per_ch_gpu_us: Vec<(String, u128)> = Vec::new();
        // A channel shown in another renders first (see `prepare_taps`).
        for ch_idx in self.channel_render_order() {
            let channel = &mut self.channels[ch_idx];
            let culled = effective_opacities.get(ch_idx).copied().unwrap_or(0.0) < 0.001;
            // Cued channels are force-rendered so their off-air previews update
            // live; the compositor stays opacity-gated so they never leak to output.
            // A tapped channel is force-rendered for a stronger reason: some
            // visible deck reads its composite, so its own opacity says nothing
            // about whether it reaches the program. Culling it would show that
            // deck black. See spec/program-tap.md.
            let spared = preview_channels.contains(&ch_idx) || channel.tapped;
            if culled && !spared {
                // Shown only as a tile in the control window: drawn a few
                // times a second, the channels taking turns so no frame
                // carries them all.
                if self.tile_previews {
                    let every = (target_fps / super::TILE_FPS).max(1);
                    let turn = u32::try_from(ch_idx).unwrap_or(0);
                    if self.frame_count.wrapping_add(turn) % every != 0 {
                        channel.dt_owed += dt;
                        continue;
                    }
                } else {
                    // Reset stats so culled channels don't show stale render metrics
                    channel.render_time_ms = 0.0;
                    channel.active_deck_count = 0;
                    channel.dt_owed = 0.0;
                    continue;
                }
            }
            let channel_dt = dt + std::mem::take(&mut channel.dt_owed);
            if let Err(e) = channel.render(
                context,
                audio_data,
                &self.modulation,
                ch_idx,
                time,
                channel_dt,
                target_fps,
                total_active_decks,
                gpu_load_ratio,
                frame_over_budget,
                &mut prefix_cmds,
                timing_frame.as_mut(),
                self.query_set.as_ref(),
            ) {
                log::error!("Channel {ch_idx} render failed, skipping: {e}");
                continue;
            }
            rendered_channels += 1;

            // Per-channel GPU drain when profiling
            if profiling {
                let t = std::time::Instant::now();
                let _ = context.device.poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: Some(std::time::Duration::from_millis(200)),
                });
                per_ch_gpu_us.push((channel.name.clone(), t.elapsed().as_micros()));
            }
        }
        let channels_us = t_channels.elapsed().as_micros();

        // If no channel consumed the video upload prefix (all channels faded
        // out or errored), submit it now so video players still advance.
        if !prefix_cmds.is_empty() {
            context.submit(prefix_cmds);
        }

        // Request re-mapping of staging buffers AFTER the submit that
        // included the video upload commands. map_async can complete
        // synchronously on Metal/UMA so must not be called before that submit.
        for channel in &mut self.channels {
            channel.request_video_remap();
        }

        // GPU profiling: drain remaining channel GPU work (per-channel drains
        // already happened inside the loop above when profiling)
        let gpu_channels_us = if profiling {
            let t = std::time::Instant::now();
            let _ = context.device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(std::time::Duration::from_millis(200)),
            });
            t.elapsed().as_micros()
        } else {
            0
        };

        self.sync_transition_progress();
        // With a master tap the composite and master FX write into the tap
        // target instead, so the pre-tonemap program survives into next frame
        // without a copy. Swapping the fields keeps those two passes unaware.
        self.swap_master_tap();
        let t_mixer_composite = std::time::Instant::now();
        let composite_cmds = self.composite_channels(context);
        let mixer_composite_us = t_mixer_composite.elapsed().as_micros();

        let t_master_fx = std::time::Instant::now();
        let master_fx = self.apply_master_effects(context, audio_data, time, composite_cmds);
        let master_fx_us = t_master_fx.elapsed().as_micros();
        // Unconditional, so a failed effect chain cannot leave the composite
        // and tap fields permanently transposed.
        self.swap_master_tap();
        master_fx?;

        // Tonemap pass: compress HDR composite into displayable [0,1] range.
        // Bypass mode is a no-op (values clamp at the output boundary anyway).
        self.apply_tonemap(context);
        self.apply_lut(context);

        // GPU profiling: drain composite + master FX GPU work
        let gpu_composite_us = if profiling {
            let t = std::time::Instant::now();
            let _ = context.device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(std::time::Duration::from_millis(200)),
            });
            t.elapsed().as_micros()
        } else {
            0
        };

        // ── GPU Timestamp: resolve + readback ───────────────────────────
        // Only issue a new resolve/copy/map when no map is in flight.
        // `timing_map_inflight` is set the moment a map_async is *issued* (not
        // when its callback fires), so the pending window between issue and
        // callback is covered. Deriving this from `staging_mapped_idx` alone is
        // unsound: it stays `MAX` until the callback runs, which would let a
        // second map_async be issued on the other buffer, leaving one buffer
        // permanently mapped and crashing the next submit with "still mapped".
        // Dropping an occasional measurement is harmless; a stuck map is fatal.
        if !self.timing_map_inflight {
            if let (Some(ref qs), Some(ref resolve_buf), Some(ref staging)) =
                (&self.query_set, &self.resolve_buffer, &self.staging_buffers)
            {
                if let Some(ref timing) = timing_frame {
                    let query_count = timing.query_count();
                    if query_count > 0 {
                        let mut enc = context.device.create_command_encoder(
                            &wgpu::CommandEncoderDescriptor {
                                label: Some("GPU Timing Resolve"),
                            },
                        );
                        enc.resolve_query_set(qs, 0..query_count, resolve_buf, 0);
                        let byte_count = u64::from(query_count) * 8;
                        let write_idx = self.staging_index;
                        enc.copy_buffer_to_buffer(
                            resolve_buf,
                            0,
                            &staging[write_idx],
                            0,
                            byte_count,
                        );
                        context.submit(std::iter::once(enc.finish()));

                        // Map the staging buffer for reading next frame.
                        // The callback stores the buffer index so the read
                        // path knows exactly which buffer to unmap.
                        let mapped_flag = self.staging_mapped_idx.clone();
                        staging[write_idx].slice(..).map_async(
                            wgpu::MapMode::Read,
                            move |result| {
                                if result.is_ok() {
                                    mapped_flag
                                        .store(write_idx, std::sync::atomic::Ordering::Release);
                                }
                            },
                        );

                        // Mark the map in flight from the moment it is issued so
                        // no second map_async can be started before the read path
                        // consumes and unmaps this buffer.
                        self.timing_map_inflight = true;

                        // Save allocations for readback next frame
                        self.prev_timing_allocations.clone_from(&timing.allocations);
                        self.staging_index = 1 - self.staging_index;
                    }
                }
            }
        }

        self.frame_count += 1;

        // Update GPU load ratio: how much CPU-measured render cost underestimates
        // true GPU cost.
        let frame_budget_us = if target_fps > 0 {
            1_000_000.0 / target_fps as f32
        } else {
            f32::MAX
        };
        let actual_frame_us = dt * 1_000_000.0;
        // A frame that encoded essentially nothing carries no evidence either
        // way, and its `dt` is whatever wall-clock gap preceded it — a paused or
        // hidden window would otherwise drive the ratio up and leave decks
        // skipping for the first moments after they come back.
        let mixer_cpu_us = channels_us + mixer_composite_us + master_fx_us;
        let (deck_gpu_us, deck_encode_us) = self.active_deck_costs();
        if mixer_cpu_us > 100 {
            if let Some(raw_ratio) = raw_gpu_load_ratio(
                deck_gpu_us,
                deck_encode_us,
                actual_frame_us,
                frame_budget_us,
            ) {
                // EMA smoothing (α = 0.15) — responsive but not jittery
                self.gpu_load_ratio = 0.15 * raw_ratio + 0.85 * self.gpu_load_ratio;
            }
        }

        // Update GPU utilization %: sum of per-deck GPU costs / frame budget.
        // Prefer GPU timestamp data; fall back to CPU cost × gpu_load_ratio.
        if frame_budget_us < f32::MAX {
            let total_gpu_us: f32 = self
                .channels
                .iter()
                .flat_map(|ch| ch.decks.iter())
                .filter(|s| !s.mute && s.opacity > 0.0)
                .map(|s| {
                    if s.gpu_render_cost_us > 0.0 {
                        s.gpu_render_cost_us
                    } else {
                        s.render_cost_us * self.gpu_load_ratio
                    }
                })
                .sum();
            let raw_util = (total_gpu_us / frame_budget_us) * 100.0;
            let clamped = raw_util.clamp(0.0, 999.0);
            self.gpu_utilization = 0.15 * clamped + 0.85 * self.gpu_utilization;
        }

        // GPU profiling: detailed per-frame log
        if profiling {
            self.perf_profile_frames -= 1;
            // Per-channel GPU drain breakdown
            let ch_gpu_str: String = per_ch_gpu_us
                .iter()
                .map(|(name, us)| format!("{name}={us}us"))
                .collect::<Vec<_>>()
                .join(", ");
            let total_per_ch_gpu: u128 = per_ch_gpu_us.iter().map(|(_, us)| us).sum();
            log::info!(
                "[PERF_PROFILE] frame={} | \
                 cpu_encode: channels={}us composite={}us master_fx={}us video={}us | \
                 gpu_drain: per_ch=[{}] ch_total={}us residual={}us composite={}us | \
                 channels_rendered={} total_decks={} | \
                 remaining={}",
                self.frame_count,
                channels_us,
                mixer_composite_us,
                master_fx_us,
                video_tick_us,
                ch_gpu_str,
                total_per_ch_gpu,
                gpu_channels_us,
                gpu_composite_us,
                rendered_channels,
                total_active_decks,
                self.perf_profile_frames,
            );
        }

        // Log mixer-level timing every 120 frames
        if self.frame_count.is_multiple_of(120) {
            let total_us = now.elapsed().as_micros();
            log::debug!(
                "[PERF] mixer | channels_rendered={} channels={}us | \
                 mixer_composite={}us master_fx={}us | \
                 modulation={}us video_tick={}us | \
                 gpu_load_ratio={:.1}x gpu_util={:.0}% | \
                 total={}us ({:.1}ms)",
                rendered_channels,
                channels_us,
                mixer_composite_us,
                master_fx_us,
                modulation_us,
                video_tick_us,
                self.gpu_load_ratio,
                self.gpu_utilization,
                total_us,
                total_us as f64 / 1000.0,
            );
        }

        Ok(())
    }

    fn composite_channels(&mut self, context: &GpuContext) -> Vec<wgpu::CommandBuffer> {
        self.release_merge_frames();
        if self.mix_mode == super::MixMode::Program {
            return self.composite_program(context);
        }
        let mut cmd_buffers: Vec<wgpu::CommandBuffer> = Vec::new();
        let channel_count = self.channels.len();
        if channel_count == 0 {
            let mut encoder =
                context
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("Mixer Clear Encoder"),
                    });
            {
                let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Mixer Clear Pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &self.composite_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
            }
            cmd_buffers.push(encoder.finish());
            return cmd_buffers;
        }

        // If we have exactly 2 channels and a transition shader is active, use it
        if channel_count == 2 {
            if let Some(transition) = &mut self.active_transition {
                let width = self.composite_texture.width();
                let height = self.composite_texture.height();

                let uniforms = ISFUniforms {
                    time: self.start_time.elapsed().as_secs_f32(),
                    time_delta: 1.0 / 60.0,
                    frame_index: self.frame_count,
                    pass_index: 0,
                    render_size: [width as f32, height as f32],
                    phase_times: [0.0; 4],
                    ..Default::default()
                };

                transition.params.build_buffer_data();
                if let Some(buf) = transition.params.buffer() {
                    context
                        .queue
                        .write_buffer(buf, 0, transition.params.scratch());
                }

                transition.pipeline.render_to(
                    context,
                    &self.channels[0].composite_view,
                    &self.channels[1].composite_view,
                    &self.composite_view,
                    &uniforms,
                    transition.params.buffer(),
                );

                return Vec::new();
            }
        }

        // Fallback: opacity-based crossfade
        //
        // For 2-channel mode the first channel is blitted onto a cleared-to-
        // transparent target using ALPHA_BLENDING.  The hardware blend applies
        // SrcAlpha to the RGB output, so if the blit shader also multiplies alpha
        // by opacity, the effective weight becomes opacity² (double-application).
        //
        // To avoid this, the first channel is always blitted at full opacity and
        // the crossfader value is used solely as the second channel's composite
        // opacity.  The composite shader performs `mix(dst, src, src_a)`, which
        // yields the correct linear crossfade: (1-cf)·A + cf·B.
        //
        // The clear is TRANSPARENT (not BLACK) so the program output carries the
        // channels' alpha through to alpha-capable outputs. Because the clear RGB
        // is zero either way, RGB is byte-identical to the old over-black result
        // (the program becomes premultiplied-alpha); opaque content (alpha=1) and
        // the over-black display path are unchanged. See /spec/html-source.md §2.
        let opacities = self.compositing_opacities();

        // Batch channel compositing into command buffers for deferred submission.
        // Every channel may draw through the blit ring (Normal ones blend in
        // hardware), so it needs a slot per channel before any bind group.
        for pipeline in [
            &self.blit_pipeline,
            &self.add_pipeline,
            &self.screen_pipeline,
        ] {
            pipeline.ensure_ring_slots(&context.device, self.channels.len());
        }
        let mut is_first = true;
        let mut slot: usize = 0;
        for (channel, &opacity) in self.channels.iter().zip(opacities.iter()) {
            if opacity <= 0.0 {
                continue;
            }

            if is_first {
                // First visible channel: per-draw params blit.
                // Channel composites are premultiplied-alpha (see blit_pipeline blend).
                self.blit_pipeline.write_params_slot(
                    &context.queue,
                    slot,
                    opacity,
                    [1.0, 1.0],
                    [0.0, 0.0],
                    true,
                );
                let bind_group = self.blit_pipeline.create_ring_bind_group(
                    &context.device,
                    &channel.composite_view,
                    slot,
                );
                let mut encoder =
                    context
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("Mixer Composite Encoder (first)"),
                        });
                {
                    let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("Mixer Composite Pass (first)"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &self.composite_view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                store: wgpu::StoreOp::Store,
                            },
                            depth_slice: None,
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    self.blit_pipeline
                        .render_at_slot(&mut render_pass, &bind_group);
                }
                cmd_buffers.push(encoder.finish());
                is_first = false;
            } else if let Some(pipeline) = match channel.blend_mode {
                crate::channel::BlendMode::Normal => Some(&self.blit_pipeline),
                crate::channel::BlendMode::Add => Some(&self.add_pipeline),
                crate::channel::BlendMode::Screen => Some(&self.screen_pipeline),
                _ => None,
            } {
                // Normal, Add and Screen are what the GPU's blend unit computes
                // itself, exactly as the composite shader does for these
                // premultiplied composites, so the channel is drawn straight
                // onto the mix — no snapshot copy and re-read, ~130 MB at 4K.
                pipeline.write_params_slot(
                    &context.queue,
                    slot,
                    opacity,
                    [1.0, 1.0],
                    [0.0, 0.0],
                    true,
                );
                cmd_buffers.push(pipeline.blend_onto(
                    &context.device,
                    &channel.composite_view,
                    &self.composite_view,
                    slot,
                    "Mixer Composite (blend unit)",
                ));
            } else {
                // Subsequent channels: snapshot + per-draw params composite
                let mut copy_encoder =
                    context
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("Mixer Snapshot Copy"),
                        });
                copy_encoder.copy_texture_to_texture(
                    self.composite_texture.as_image_copy(),
                    self.effect_ping_texture.as_image_copy(),
                    self.composite_texture.size(),
                );

                let blend_mode = channel.blend_mode;
                self.composite_pipeline.write_params_slot(
                    &context.queue,
                    slot,
                    opacity,
                    blend_mode.to_index(),
                    [1.0, 1.0],
                    [0.0, 0.0],
                    true,
                );
                let bind_group = self.composite_pipeline.create_ring_bind_group(
                    &context.device,
                    &channel.composite_view,
                    &self.effect_ping_view,
                    slot,
                );
                let mut encoder =
                    context
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("Mixer Composite Encoder"),
                        });
                {
                    let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("Mixer Composite Pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &self.composite_view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                store: wgpu::StoreOp::Store,
                            },
                            depth_slice: None,
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    self.composite_pipeline
                        .render_at_slot(&mut render_pass, &bind_group);
                }
                cmd_buffers.push(copy_encoder.finish());
                cmd_buffers.push(encoder.finish());
            }
            slot += 1;
        }

        cmd_buffers
    }

    /// The layers of Main, bottom first: what is on it (during a fade, what
    /// it is leaving and what it is taking, weighted by the fade and added —
    /// a true dissolve, whatever either leaves transparent; or the picture a
    /// transition drew, when `transition_drawn`), then every overlay that
    /// shows, at its level times its fader, with its own blend mode.
    fn program_layers(&self, transition_drawn: bool) -> Vec<ProgramLayer> {
        use super::MainSource;
        let find = |source: &MainSource| -> Option<LayerSource> {
            match source {
                MainSource::Black => None,
                MainSource::Channel(uuid) => {
                    self.find_channel_by_uuid(uuid).map(LayerSource::Channel)
                }
                MainSource::Deck(uuid) => self
                    .find_deck_by_uuid(uuid)
                    .map(|(ch, d)| LayerSource::Deck(ch, d)),
            }
        };
        let normal = crate::channel::BlendMode::Normal;
        let mut layers = Vec::new();
        let program = find(&self.main.program);
        match (&self.main.fade, program) {
            (Some(_), _) if transition_drawn => {
                layers.push(ProgramLayer::new(LayerSource::Transition, 1.0, normal));
            }
            (Some(fade), program) => {
                // Weighted, not the new over the old at full: outside what
                // the new one covers the old fades out with it, rather than
                // staying whole and going at the end.
                let shown = fade.shown();
                match (find(&fade.from), program) {
                    (Some(from), Some(to)) => {
                        layers.push(ProgramLayer::new(from, 1.0 - shown, normal));
                        layers.push(ProgramLayer::added(to, shown));
                    }
                    // Fading up from black, or down to it.
                    (None, Some(to)) => layers.push(ProgramLayer::new(to, shown, normal)),
                    (Some(from), None) => {
                        layers.push(ProgramLayer::new(from, 1.0 - shown, normal));
                    }
                    (None, None) => {}
                }
            }
            (None, Some(program)) => layers.push(ProgramLayer::new(program, 1.0, normal)),
            (None, None) => {}
        }
        // In the transition's picture already, when it drew one.
        let in_transition = |idx: usize| {
            transition_drawn
                && self.main.fade.as_ref().is_some_and(|fade| {
                    [&fade.from, &self.main.program]
                        .into_iter()
                        .any(|s| matches!(s, MainSource::Channel(u) if u == self.channels[idx].uuid()))
                })
        };
        for (idx, ch) in self.channels.iter().enumerate() {
            if !ch.overlay
                || layers.iter().any(|l| l.source == LayerSource::Channel(idx))
                || in_transition(idx)
            {
                continue;
            }
            let level = self.main.overlay_level(ch.uuid()) * self.channel_opacity(idx);
            if level > 0.0 {
                layers.push(ProgramLayer::new(
                    LayerSource::Channel(idx),
                    level,
                    ch.blend_mode,
                ));
            }
        }
        layers
    }

    /// Composite Main ([`Self::program_layers`]) into the master.
    fn composite_program(&mut self, context: &GpuContext) -> Vec<wgpu::CommandBuffer> {
        let drawn = self.render_main_transition(context);
        let layers = self.program_layers(drawn.is_some());
        let mut cmd_buffers = drawn.unwrap_or_default();
        for pipeline in [
            &self.blit_pipeline,
            &self.add_pipeline,
            &self.screen_pipeline,
            &self.sum_pipeline,
        ] {
            pipeline.ensure_ring_slots(&context.device, layers.len().max(1));
        }
        self.composite_pipeline
            .ensure_ring_slots(&context.device, layers.len().max(1));
        if layers.is_empty() {
            let mut encoder =
                context
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("Main Clear Encoder"),
                    });
            {
                let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Main Clear Pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &self.composite_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
            }
            cmd_buffers.push(encoder.finish());
            return cmd_buffers;
        }
        for (slot, layer) in layers.iter().enumerate() {
            // Channel composites are premultiplied; a deck's own texture is
            // straight alpha and is premultiplied as it is drawn.
            let (view, premultiplied) = match layer.source {
                LayerSource::Channel(idx) => (&self.channels[idx].composite_view, true),
                LayerSource::Deck(ch, d) => (&self.channels[ch].decks[d].deck.texture_view, false),
                LayerSource::Transition => match &self.main_transitions.targets {
                    Some(targets) => (&targets.out_view, true),
                    None => continue,
                },
            };
            let write = |pipeline: &crate::renderer::BlitPipeline| {
                if premultiplied {
                    pipeline.write_params_slot(
                        &context.queue,
                        slot,
                        layer.opacity,
                        [1.0, 1.0],
                        [0.0, 0.0],
                        true,
                    );
                } else {
                    pipeline.write_params_slot_premultiplying(&context.queue, slot, layer.opacity);
                }
            };
            if slot == 0 {
                write(&self.blit_pipeline);
                let bind_group =
                    self.blit_pipeline
                        .create_ring_bind_group(&context.device, view, slot);
                let mut encoder =
                    context
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("Main Composite Encoder (first)"),
                        });
                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("Main Composite Pass (first)"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &self.composite_view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                store: wgpu::StoreOp::Store,
                            },
                            depth_slice: None,
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    self.blit_pipeline.render_at_slot(&mut pass, &bind_group);
                }
                cmd_buffers.push(encoder.finish());
            } else if let Some(pipeline) = match layer.blend {
                _ if layer.sum => Some(&self.sum_pipeline),
                crate::channel::BlendMode::Normal => Some(&self.blit_pipeline),
                crate::channel::BlendMode::Add => Some(&self.add_pipeline),
                crate::channel::BlendMode::Screen => Some(&self.screen_pipeline),
                _ => None,
            } {
                write(pipeline);
                cmd_buffers.push(pipeline.blend_onto(
                    &context.device,
                    view,
                    &self.composite_view,
                    slot,
                    "Main Composite (blend unit)",
                ));
            } else {
                // Other blend modes read what is below: snapshot, then the
                // composite shader. Only overlay channels get here.
                let mut copy_encoder =
                    context
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("Main Snapshot Copy"),
                        });
                copy_encoder.copy_texture_to_texture(
                    self.composite_texture.as_image_copy(),
                    self.effect_ping_texture.as_image_copy(),
                    self.composite_texture.size(),
                );
                self.composite_pipeline.write_params_slot(
                    &context.queue,
                    slot,
                    layer.opacity,
                    layer.blend.to_index(),
                    [1.0, 1.0],
                    [0.0, 0.0],
                    premultiplied,
                );
                let bind_group = self.composite_pipeline.create_ring_bind_group(
                    &context.device,
                    view,
                    &self.effect_ping_view,
                    slot,
                );
                let mut encoder =
                    context
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("Main Composite Encoder"),
                        });
                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("Main Composite Pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &self.composite_view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                store: wgpu::StoreOp::Store,
                            },
                            depth_slice: None,
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    self.composite_pipeline
                        .render_at_slot(&mut pass, &bind_group);
                }
                cmd_buffers.push(copy_encoder.finish());
                cmd_buffers.push(encoder.finish());
            }
        }
        cmd_buffers
    }

    /// Prepare sub-mix textures for all unique multi-channel surface sources.
    pub fn prepare_sub_mixes(&mut self, sources: &[Vec<usize>], context: &GpuContext) {
        let needed: std::collections::HashSet<Vec<usize>> = sources.iter().cloned().collect();
        self.sub_mix_cache.retain(|k, _| needed.contains(k));

        for mut indices in sources.iter().cloned() {
            indices.sort_unstable();
            indices.dedup();
            if !self.sub_mix_cache.contains_key(&indices) {
                let width = self.composite_texture.width();
                let height = self.composite_texture.height();
                let tex = context.create_compositing_texture(width, height);
                let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
                self.sub_mix_cache.insert(indices.clone(), (tex, view));
            }
            self.composite_sub_mix(&indices, context);

            // Tonemap the sub-mix in-place (same pattern as main composite).
            // Uses effect_ping as scratch — safe because composite_sub_mix has
            // already finished with it by the time we get here.
            if let Some((sub_tex, sub_view)) = self.sub_mix_cache.get(&indices) {
                tonemap_in_place(
                    self.tonemap_mode,
                    &self.tonemap_pipeline,
                    sub_tex,
                    sub_view,
                    &self.effect_ping_texture,
                    &self.effect_ping_view,
                    context,
                );
            }
            if let Some((sub_tex, sub_view)) = self.sub_mix_cache.get(&indices) {
                apply_lut_in_place(
                    &self.lut_pipeline,
                    self.active_lut.as_ref(),
                    sub_tex,
                    sub_view,
                    &self.effect_ping_texture,
                    &self.effect_ping_view,
                    context,
                );
            }
        }
    }

    /// Composite a specific subset of channels into the cached sub-mix texture.
    fn composite_sub_mix(&self, indices: &[usize], context: &GpuContext) {
        let Some((sub_tex, sub_view)) = self.sub_mix_cache.get(indices) else {
            return;
        };

        let opacities = self.compositing_opacities();

        let mut cmd_buffers: Vec<wgpu::CommandBuffer> = Vec::new();
        let mut is_first = true;
        let mut slot: usize = 0;
        for &ch_idx in indices {
            if ch_idx >= self.channels.len() {
                continue;
            }
            let channel = &self.channels[ch_idx];
            let opacity = opacities.get(ch_idx).copied().unwrap_or(0.0);
            if opacity <= 0.0 {
                continue;
            }

            if is_first {
                // First visible channel: per-draw params blit.
                // Channel composites are premultiplied-alpha (see blit_pipeline blend).
                self.blit_pipeline.write_params_slot(
                    &context.queue,
                    slot,
                    opacity,
                    [1.0, 1.0],
                    [0.0, 0.0],
                    true,
                );
                let bind_group = self.blit_pipeline.create_ring_bind_group(
                    &context.device,
                    &channel.composite_view,
                    slot,
                );
                let mut encoder =
                    context
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("Sub-mix Composite Encoder (first)"),
                        });
                {
                    let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("Sub-mix Composite Pass (first)"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: sub_view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                store: wgpu::StoreOp::Store,
                            },
                            depth_slice: None,
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    self.blit_pipeline
                        .render_at_slot(&mut render_pass, &bind_group);
                }
                cmd_buffers.push(encoder.finish());
                is_first = false;
            } else {
                // Subsequent channels: snapshot sub-mix → effect_ping, per-draw params composite
                let mut copy_encoder =
                    context
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("Sub-mix Snapshot Copy"),
                        });
                copy_encoder.copy_texture_to_texture(
                    sub_tex.as_image_copy(),
                    self.effect_ping_texture.as_image_copy(),
                    sub_tex.size(),
                );

                let blend_mode = channel.blend_mode;
                self.composite_pipeline.write_params_slot(
                    &context.queue,
                    slot,
                    opacity,
                    blend_mode.to_index(),
                    [1.0, 1.0],
                    [0.0, 0.0],
                    true,
                );
                let bind_group = self.composite_pipeline.create_ring_bind_group(
                    &context.device,
                    &channel.composite_view,
                    &self.effect_ping_view,
                    slot,
                );
                let mut encoder =
                    context
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("Sub-mix Composite Encoder"),
                        });
                {
                    let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("Sub-mix Composite Pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: sub_view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                store: wgpu::StoreOp::Store,
                            },
                            depth_slice: None,
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    self.composite_pipeline
                        .render_at_slot(&mut render_pass, &bind_group);
                }
                cmd_buffers.push(copy_encoder.finish());
                cmd_buffers.push(encoder.finish());
            }
            slot += 1;
        }

        if is_first {
            let mut encoder =
                context
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("Sub-mix Clear Encoder"),
                    });
            {
                let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Sub-mix Clear Pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: sub_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
            }
            cmd_buffers.push(encoder.finish());
        }

        if !cmd_buffers.is_empty() {
            context.submit(cmd_buffers);
        }
    }

    /// Compute per-channel compositing opacities.
    ///
    /// For 2-channel mode the first channel is always 1.0 (blitted at full
    /// opacity) and the crossfader value drives the second channel's composite
    /// weight.  For N-channel mode each channel uses its own opacity.
    fn compositing_opacities(&self) -> CompositingOpacities {
        if self.channels.len() == 2 {
            CompositingOpacities::Two([1.0, self.crossfader])
        } else {
            CompositingOpacities::Many(self.channels.iter().map(|ch| ch.opacity).collect())
        }
    }

    /// Get the sub-mix texture view for a given set of channel indices.
    pub fn get_sub_mix_view(&self, indices: &[usize]) -> Option<&wgpu::TextureView> {
        self.sub_mix_cache.get(indices).map(|(_, v)| v)
    }

    fn apply_master_effects(
        &mut self,
        context: &GpuContext,
        audio_data: &crate::audio::AudioData,
        time: f32,
        mut cmd_buffers: Vec<wgpu::CommandBuffer>,
    ) -> Result<()> {
        if self.master_effects.is_empty() {
            if !cmd_buffers.is_empty() {
                context.submit(cmd_buffers);
            }
            return Ok(());
        }

        let width = self.composite_texture.width();
        let height = self.composite_texture.height();

        let uniforms = ISFUniforms {
            time,
            time_delta: 1.0 / 60.0,
            frame_index: self.frame_count,
            pass_index: 0,
            render_size: [width as f32, height as f32],
            audio_level: audio_data.level,
            audio_bass: audio_data.bass(),
            audio_mid: audio_data.mid(),
            audio_treble: audio_data.treble(),
            audio_bpm: audio_data.bpm.unwrap_or(0.0),
            audio_beat_phase: audio_data.beat_phase(),
            date: crate::deck::get_current_date(),
            phase_times: [0.0; 4],
        };

        let mut read_from_composite = true;

        for effect in &mut self.master_effects {
            // An offline effect is kept, not run.
            if !effect.enabled || effect.offline().is_some() {
                continue;
            }

            let (input_view, output_view) = if read_from_composite {
                (&self.composite_view, &self.effect_ping_view)
            } else {
                (&self.effect_ping_view, &self.composite_view)
            };

            effect.apply(
                context,
                input_view,
                output_view,
                &uniforms,
                &mut cmd_buffers,
            )?;
            read_from_composite = !read_from_composite;
        }

        if !read_from_composite {
            let mut encoder =
                context
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("Master Effect Final Copy Encoder"),
                    });
            encoder.copy_texture_to_texture(
                self.effect_ping_texture.as_image_copy(),
                self.composite_texture.as_image_copy(),
                self.composite_texture.size(),
            );
            cmd_buffers.push(encoder.finish());
        }

        if !cmd_buffers.is_empty() {
            context.submit(cmd_buffers);
        }

        Ok(())
    }

    /// Exchange `composite_*` with the master tap target, when one exists.
    /// Called in pairs around the composite and master-FX passes.
    fn swap_master_tap(&mut self) {
        if let Some((mut tex, mut view)) = self.master_tap.take() {
            std::mem::swap(&mut self.composite_texture, &mut tex);
            std::mem::swap(&mut self.composite_view, &mut view);
            self.master_tap = Some((tex, view));
        }
    }

    /// Tonemap the master program.
    ///
    /// Without a tap this is in-place on the composite. With one, the
    /// pre-tonemap program is already in a separate target, so the pass reads
    /// straight from it and the in-place scratch copy disappears. Bypass still
    /// has to move the pixels across, which is the one case a tap costs a copy.
    fn apply_tonemap(&self, context: &GpuContext) {
        let Some((tap_texture, tap_view)) = &self.master_tap else {
            tonemap_in_place(
                self.tonemap_mode,
                &self.tonemap_pipeline,
                &self.composite_texture,
                &self.composite_view,
                &self.effect_ping_texture,
                &self.effect_ping_view,
                context,
            );
            return;
        };

        let mut encoder = context
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Master Tap Tonemap Encoder"),
            });
        if self.tonemap_mode == crate::renderer::tonemap::TonemapMode::Bypass {
            encoder.copy_texture_to_texture(
                tap_texture.as_image_copy(),
                self.composite_texture.as_image_copy(),
                self.composite_texture.size(),
            );
        } else {
            self.tonemap_pipeline.render(
                &context.device,
                &mut encoder,
                tap_view,
                &self.composite_view,
            );
        }
        context.submit(Some(encoder.finish()));
    }

    /// Apply LUT to the main composite texture in-place.
    /// Runs after tonemap, before outputs read the composite.
    fn apply_lut(&self, context: &GpuContext) {
        apply_lut_in_place(
            &self.lut_pipeline,
            self.active_lut.as_ref(),
            &self.composite_texture,
            &self.composite_view,
            &self.effect_ping_texture,
            &self.effect_ping_view,
            context,
        );
    }

    /// Prepare tonemapped copies of individual channel composites.
    /// Called for channels used as direct `OutputSource::Channel(idx)` sources.
    /// Channel composites can't be tonemapped in-place because they feed into
    /// the mixer composite on subsequent frames.
    pub fn prepare_channel_tonemaps(&mut self, channel_indices: &[usize], context: &GpuContext) {
        use crate::renderer::tonemap::TonemapMode;

        if self.tonemap_mode == TonemapMode::Bypass {
            self.tonemapped_channel_cache.clear();
            return;
        }

        // Remove stale entries
        self.tonemapped_channel_cache
            .retain(|idx, _| channel_indices.contains(idx));

        for &ch_idx in channel_indices {
            let ch_view = match self.channels.get(ch_idx) {
                Some(ch) => &ch.composite_view,
                None => continue,
            };

            // Create cached texture if needed
            if !self.tonemapped_channel_cache.contains_key(&ch_idx) {
                let width = self.composite_texture.width();
                let height = self.composite_texture.height();
                let tex = context.create_compositing_texture(width, height);
                let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
                self.tonemapped_channel_cache.insert(ch_idx, (tex, view));
            }

            // Tonemap directly: channel composite → cached texture
            // (no copy needed since source and target are different textures)
            let tonemap_target = &self.tonemapped_channel_cache[&ch_idx].1;
            let mut encoder =
                context
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("Channel Tonemap Encoder"),
                    });
            self.tonemap_pipeline
                .render(&context.device, &mut encoder, ch_view, tonemap_target);
            context.submit(Some(encoder.finish()));

            // Apply LUT to the tonemapped channel copy
            if let Some(lut) = &self.active_lut {
                let (cache_tex, cache_view) = &self.tonemapped_channel_cache[&ch_idx];
                let mut lut_encoder =
                    context
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("Channel LUT Encoder"),
                        });
                // Copy cache → effect_ping, then LUT ping → cache
                lut_encoder.copy_texture_to_texture(
                    cache_tex.as_image_copy(),
                    self.effect_ping_texture.as_image_copy(),
                    cache_tex.size(),
                );
                self.lut_pipeline.render(
                    &context.device,
                    &mut lut_encoder,
                    &self.effect_ping_view,
                    cache_view,
                    lut,
                );
                context.submit(Some(lut_encoder.finish()));
            }
        }
    }

    /// Summed GPU and CPU render costs over the decks that are actually drawing
    /// *and* have GPU timing for this frame.
    ///
    /// Both sums come from the same set of decks so their quotient stays a
    /// like-for-like comparison; a deck still waiting on its first timestamp
    /// would otherwise contribute CPU cost with no GPU cost and understate the
    /// ratio. Returns `(0.0, 0.0)` when no deck has timing yet, which is what
    /// puts [`raw_gpu_load_ratio`] onto its fallback.
    fn active_deck_costs(&self) -> (f32, f32) {
        self.channels
            .iter()
            .flat_map(|ch| ch.decks.iter())
            .filter(|s| !s.mute && s.opacity > 0.0 && s.gpu_render_cost_us > 0.0)
            .fold((0.0, 0.0), |(gpu, cpu), s| {
                (gpu + s.gpu_render_cost_us, cpu + s.render_cost_us)
            })
    }
}

/// Ceiling on the load ratio. Generous enough to express a genuinely
/// GPU-bound shader — a 10 ms pass encoded in 200 µs really is 50× — while
/// still bounding how far one bad frame can push every deck toward skipping.
const MAX_GPU_LOAD_RATIO: f32 = 64.0;

/// How much to scale CPU-measured deck cost to estimate true GPU cost.
///
/// `None` means the frame carried no usable signal, leaving the smoothed ratio
/// untouched rather than dragging it toward an invented value.
///
/// GPU timestamps are the honest answer: they time the same work the CPU figure
/// times, so their quotient is the underestimation factor and nothing else.
/// Without them, how far the frame overran its budget is a coarser stand-in,
/// but it is proportionate and bounded by construction.
///
/// What this must not be is whole-frame wall time over the mixer's encode time.
/// That quotient is roughly `frame_budget / encode_time` — around 20× on a
/// perfectly healthy frame — so any render-thread work outside the mixer (a
/// screen-capture upload, say) that pushes one frame past budget made every
/// deck look twentyfold more expensive than it is, ratcheting the smoothed
/// ratio up until unrelated shader decks started skipping while the frame loop
/// still reported its target rate.
fn raw_gpu_load_ratio(
    deck_gpu_us: f32,
    deck_encode_us: f32,
    actual_frame_us: f32,
    frame_budget_us: f32,
) -> Option<f32> {
    if deck_gpu_us > 0.0 && deck_encode_us > 0.0 {
        return Some((deck_gpu_us / deck_encode_us).clamp(1.0, MAX_GPU_LOAD_RATIO));
    }
    if frame_budget_us >= f32::MAX || actual_frame_us <= 0.0 {
        return None;
    }
    // `dt` includes the vsync idle wait, so a frame at or under budget is
    // evidence of no GPU pressure rather than evidence of none being measurable.
    Some(if actual_frame_us > frame_budget_us * 1.05 {
        (actual_frame_us / frame_budget_us).clamp(1.0, MAX_GPU_LOAD_RATIO)
    } else {
        1.0
    })
}

/// Tonemap a texture in-place using a scratch texture for the copy.
/// Skips the pass in Bypass mode.
fn tonemap_in_place(
    mode: crate::renderer::tonemap::TonemapMode,
    pipeline: &crate::renderer::tonemap::TonemapPipeline,
    target_tex: &wgpu::Texture,
    target_view: &wgpu::TextureView,
    scratch_tex: &wgpu::Texture,
    scratch_view: &wgpu::TextureView,
    context: &GpuContext,
) {
    use crate::renderer::tonemap::TonemapMode;

    if mode == TonemapMode::Bypass {
        return;
    }

    let mut encoder = context
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Tonemap Encoder"),
        });

    // Copy target → scratch so the shader can read scratch and write back to target.
    encoder.copy_texture_to_texture(
        target_tex.as_image_copy(),
        scratch_tex.as_image_copy(),
        target_tex.size(),
    );

    pipeline.render(&context.device, &mut encoder, scratch_view, target_view);
    context.submit(Some(encoder.finish()));
}

/// Apply a LUT to a texture in-place using a scratch texture.
/// No-op if no LUT is loaded.
fn apply_lut_in_place(
    pipeline: &crate::renderer::lut::LutPipeline,
    lut: Option<&crate::renderer::lut::LoadedLut>,
    target_tex: &wgpu::Texture,
    target_view: &wgpu::TextureView,
    scratch_tex: &wgpu::Texture,
    scratch_view: &wgpu::TextureView,
    context: &GpuContext,
) {
    let Some(lut) = lut else {
        return;
    };

    let mut encoder = context
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("LUT Encoder"),
        });

    // Copy target → scratch so the shader can read scratch and write back to target.
    encoder.copy_texture_to_texture(
        target_tex.as_image_copy(),
        scratch_tex.as_image_copy(),
        target_tex.size(),
    );

    pipeline.render(
        &context.device,
        &mut encoder,
        scratch_view,
        target_view,
        lut,
    );
    context.submit(Some(encoder.finish()));
}

#[cfg(test)]
mod gpu_load_ratio_tests {
    use super::{raw_gpu_load_ratio, MAX_GPU_LOAD_RATIO};

    const BUDGET_60FPS: f32 = 1_000_000.0 / 60.0;

    #[test]
    fn timestamps_give_the_plain_gpu_over_cpu_underestimation_factor() {
        // 4 ms of GPU work encoded in 500 us: deck cost is understated 8x.
        let ratio =
            raw_gpu_load_ratio(4000.0, 500.0, BUDGET_60FPS, BUDGET_60FPS).expect("timing present");
        assert!((ratio - 8.0).abs() < 1e-3, "expected 8x, got {ratio}");
    }

    #[test]
    fn timestamps_never_report_less_than_parity() {
        // Encoding can cost more than execution for a trivial pass; that is not
        // evidence decks are cheaper than measured, so the floor is 1.0.
        let ratio = raw_gpu_load_ratio(100.0, 900.0, BUDGET_60FPS, BUDGET_60FPS).expect("timing");
        assert!((ratio - 1.0).abs() < f32::EPSILON, "got {ratio}");
    }

    /// The regression this pins. A screen-capture upload costs ~1 ms of
    /// render-thread time outside the mixer, which pushes the frame a little
    /// over budget. The old formula divided whole-frame wall time by the
    /// mixer's encode time and called the result GPU pressure, yielding ~24x
    /// for a frame only 1.14x over — enough, once smoothed, to force unrelated
    /// shader decks into skipping while the loop still hit 60 fps.
    #[test]
    fn an_over_budget_frame_caused_by_non_mixer_work_stays_proportionate() {
        let actual = BUDGET_60FPS + 2425.0; // ~19.1 ms, as measured on an M2 Max
        let mixer_encode_us = 800.0;

        let old = actual / mixer_encode_us;
        assert!(
            old > 20.0,
            "precondition: the old formula really did explode ({old}x)"
        );

        // No GPU timing available, so this is the fallback path.
        let ratio = raw_gpu_load_ratio(0.0, 0.0, actual, BUDGET_60FPS).expect("over budget");
        assert!(
            (ratio - actual / BUDGET_60FPS).abs() < 1e-3,
            "overage should be reported as itself, got {ratio}"
        );
        assert!(ratio < 1.2, "a 14% overrun must not read as {ratio}x");
    }

    #[test]
    fn meeting_target_decays_toward_parity() {
        // dt includes vsync idle, so an on-time frame carries no GPU pressure.
        let ratio = raw_gpu_load_ratio(0.0, 0.0, BUDGET_60FPS, BUDGET_60FPS).expect("on time");
        assert!((ratio - 1.0).abs() < f32::EPSILON, "got {ratio}");
    }

    #[test]
    fn ratio_is_bounded_even_for_a_pathological_frame() {
        let stalled = raw_gpu_load_ratio(0.0, 0.0, 5_000_000.0, BUDGET_60FPS).expect("stalled");
        assert!((stalled - MAX_GPU_LOAD_RATIO).abs() < f32::EPSILON);
        let lopsided = raw_gpu_load_ratio(1_000_000.0, 1.0, BUDGET_60FPS, BUDGET_60FPS)
            .expect("timing present");
        assert!((lopsided - MAX_GPU_LOAD_RATIO).abs() < f32::EPSILON);
    }

    #[test]
    fn an_uncapped_target_with_no_timing_yields_no_signal() {
        assert!(raw_gpu_load_ratio(0.0, 0.0, BUDGET_60FPS, f32::MAX).is_none());
        assert!(raw_gpu_load_ratio(0.0, 0.0, 0.0, BUDGET_60FPS).is_none());
    }
}

/// Where one of Main's layers comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LayerSource {
    /// A channel's composite, by index.
    Channel(usize),
    /// One deck's own picture: channel index, deck index.
    Deck(usize, usize),
    /// The picture of the transition running (see `main_transition`).
    Transition,
}

/// One layer of Main, as [`Mixer::program_layers`] lists them.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ProgramLayer {
    source: LayerSource,
    opacity: f32,
    blend: crate::channel::BlendMode,
    /// Added to what is under it, weighted, rather than drawn over it: the
    /// second half of a dissolve.
    sum: bool,
}

impl ProgramLayer {
    fn new(source: LayerSource, opacity: f32, blend: crate::channel::BlendMode) -> Self {
        Self {
            source,
            opacity,
            blend,
            sum: false,
        }
    }

    /// `source` at `weight`, added to what is under it.
    fn added(source: LayerSource, weight: f32) -> Self {
        Self {
            sum: true,
            ..Self::new(source, weight, crate::channel::BlendMode::Normal)
        }
    }
}
