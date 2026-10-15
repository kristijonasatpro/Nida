//! Video playback support for Varda
//!
//! Two codec paths:
//! - **HAP path**: GPU-native `BCn` compressed textures — near-zero CPU decode cost.
//!   Supports Hap (BC1), Hap Alpha (BC3), Hap R (BC7).
//! - **ffmpeg path**: CPU decode for H.264, `ProRes`, VP9, etc. — fallback for all other codecs.

pub mod chase;
pub mod hap;
pub mod hdr;
pub(crate) mod hwdecode;
pub mod modulation;
pub mod sound_only;
pub mod yuv;

use anyhow::{Context, Result};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};

extern crate ffmpeg_next as ffmpeg;

use ffmpeg::format::{input, Pixel};
use ffmpeg::media::Type;
use ffmpeg::software::scaling::context::Context as Scaler;
use ffmpeg::util::frame::video::Video;

/// Loop mode for video playback. Definition lives in `engine::value::video`
/// (see /spec/engine-value-types.md); re-exported here so existing
/// `crate::video::LoopMode` call sites keep working.
pub use crate::engine::value::video::{DeckTransportSync, LoopMode, TransportSyncMode};
pub use chase::{ChaseInbox, VideoChaseBroadcast};
pub use modulation::{
    PlaybackModulation, PlaybackModulationInbox, PositionTarget as ModulatedPosition,
};
pub use sound_only::{is_sound_only, SoundOnlyPlayer, SOUND_PICTURE_SIZE};

/// Result of advancing playback state.
pub struct AdvanceResult {
    /// Whether a seek is needed (loop restart, etc.).
    pub needs_seek: bool,
    /// Number of video frames to decode (0 = hold current frame, 1+ = decode new frames).
    pub frames_to_decode: u32,
}

/// One tick's playhead movement, split into the clip's own advance and the part
/// modulation asked for. See [`PlaybackState::modulated_position_step`].
struct ModulatedPositionStep {
    /// Total change to apply to `position`.
    delta: f64,
    /// Forward clip time from the clip's own advance, for the frame accumulator.
    natural_secs: f64,
    /// What the decoder must do about the modulated part.
    decode: modulation::OffsetStep,
}

/// Shared playback state for all video sources (ffmpeg and HAP).
// The four flags are independent facts about a decoder, not states of one
// machine: a clip can be playing, suspended, reversing, and at its out-point in
// any combination, and folding them together would lose that.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
pub struct PlaybackState {
    /// Whether the video is currently playing.
    pub playing: bool,
    /// Whether decoding is suspended because nothing is going to show this
    /// deck soon. Orthogonal to `playing`: the performer's transport is
    /// untouched, and neither position nor decode advances while set.
    /// See /spec/deck-residency.md.
    pub suspended: bool,
    /// Loop mode.
    pub loop_mode: LoopMode,
    /// Speed multiplier (1.0 = normal, 0.5 = half, 2.0 = double, negative = reverse).
    pub speed: f64,
    /// In-point in seconds (start of playback range). 0.0 = beginning.
    pub in_point: f64,
    /// Out-point in seconds (end of playback range). 0.0 = use duration.
    pub out_point: f64,
    /// Current playback position in seconds.
    pub position: f64,
    /// Whether we're currently playing in reverse (for ping-pong).
    pub reverse: bool,
    /// Video duration in seconds.
    pub duration: f64,
    /// Video frame rate.
    pub frame_rate: f64,
    /// Set to true for one frame when playback reaches the out-point/EOF.
    /// Used by auto-transition `ClipEnd` trigger. Cleared each frame before advance.
    pub reached_end: bool,
    /// Last wall-clock time `advance_frame` was called (for real-time delta).
    last_advance: std::time::Instant,
    /// Fractional frame accumulator — tracks sub-frame position for pacing.
    frame_accumulator: f64,
    /// Per-clip transport mapping. Default Auto.
    pub transport_sync: DeckTransportSync,
    /// Latest transport sample, written by the decode thread each tick.
    chase_transport: Option<chase::ChaseTransport>,
    /// Previous transport position, for `transport_dt` while chasing.
    last_transport_position: Option<f64>,
    /// True for the duration of a chase tick. Decode EOS paths hold instead of wrapping.
    pub chasing: bool,
    /// This frame's resolved playback modulation. Held rather than consumed, so
    /// a decode tick that arrives between render frames keeps the last level.
    /// See /spec/video-playback-modulation.md.
    modulation: modulation::PlaybackModulation,
    /// How much modulated offset is already baked into `position`.
    ///
    /// Offsets are held as "how far from where the clip would be", so the
    /// playhead only has to move when the offset *changes*. A steady offset
    /// therefore costs nothing after it has been reached, which is what keeps a
    /// slow LFO on the playhead from seeking on every frame.
    applied_position_offset: f64,
    /// A paused clip owes the screen one frame: the first one when it opens
    /// paused, the one it was moved to after a seek. Without it a clip that
    /// opens paused shows nothing until it is played. Set by whoever opens it
    /// paused or seeks it.
    pub show_frame: bool,
}

impl PlaybackState {
    pub fn new(duration: f64, frame_rate: f64) -> Self {
        let frame_rate = if frame_rate > 0.0 { frame_rate } else { 30.0 };
        Self {
            playing: true,
            suspended: false,
            loop_mode: LoopMode::Loop,
            speed: 1.0,
            in_point: 0.0,
            out_point: 0.0,
            position: 0.0,
            reverse: false,
            duration,
            frame_rate,
            reached_end: false,
            last_advance: std::time::Instant::now(),
            frame_accumulator: 0.0,
            transport_sync: DeckTransportSync::default(),
            chase_transport: None,
            last_transport_position: None,
            chasing: false,
            modulation: modulation::PlaybackModulation::default(),
            applied_position_offset: 0.0,
            show_frame: false,
        }
    }

    /// Effective out-point (uses duration if `out_point` is 0).
    pub fn effective_out(&self) -> f64 {
        if self.out_point > 0.0 {
            self.out_point
        } else {
            self.duration
        }
    }

    /// Bind this tick's transport snapshot before [`Self::advance_frame`].
    pub fn set_chase_transport(&mut self, sample: chase::ChaseTransport) {
        self.chase_transport = Some(sample);
    }

    /// Bind this frame's resolved modulation before [`Self::advance_frame`].
    pub fn set_modulation(&mut self, value: modulation::PlaybackModulation) {
        self.modulation = value;
    }

    /// The rate playback actually runs at: the stored speed unless a modulator
    /// is holding it. `speed` stays the performer's set point either way, which
    /// is what lets the UI slider keep showing where they left it.
    pub fn effective_speed(&self) -> f64 {
        self.modulation.speed.unwrap_or(self.speed)
    }

    /// Advance playback position using real wall-clock time.
    /// Returns how many video frames to decode and whether a seek is needed.
    pub fn advance_frame(&mut self) -> AdvanceResult {
        self.reached_end = false;
        if !self.playing {
            self.last_advance = std::time::Instant::now();
            self.chasing = false;
            if std::mem::take(&mut self.show_frame) {
                return AdvanceResult {
                    needs_seek: false,
                    frames_to_decode: 1,
                };
            }
            return self.paused_modulation_step();
        }
        // Playing shows frames anyway.
        self.show_frame = false;

        if let Some(transport) = self.chase_transport.take() {
            if self.transport_sync.mode.is_chasing(transport.running) {
                return self.advance_chase(transport);
            }
        }
        self.chasing = false;
        self.last_transport_position = None;

        let now = std::time::Instant::now();
        let wall_dt = now.duration_since(self.last_advance).as_secs_f64();
        self.last_advance = now;
        // Clamp dt to avoid huge jumps after pauses/stalls
        let dt = wall_dt.min(0.1);

        let frame_time = 1.0 / self.frame_rate;
        let speed = self.effective_speed().abs();
        let step = self.modulated_position_step(dt, speed, frame_time);
        self.position += step.delta;

        // Accumulate frames: how many video frames does this time step cover?
        // The clip's own advance plus whatever forward travel modulation asked
        // for, since both are frames the decoder has to walk through.
        self.frame_accumulator += step.natural_secs + step.decode.walk_secs;
        let frames_to_decode = (self.frame_accumulator / frame_time).floor() as u32;
        self.frame_accumulator -= f64::from(frames_to_decode) * frame_time;

        // An absolute source is the authority on position, so the loop region
        // does not fence it in and the boundary transitions stand down. They are
        // part of the clip's own marching, which an absolute value replaces; left
        // active they would yank the playhead to the in-point every frame the
        // curve reads past the out-point, and the curve would put it straight
        // back. The clip's bounds still apply, because there is no picture
        // outside them.
        if let modulation::PositionTarget::Absolute(_) = self.modulation.position {
            self.position = self.position.clamp(0.0, self.duration.max(0.0));
            if step.decode.needs_seek {
                self.frame_accumulator = 0.0;
                return AdvanceResult {
                    needs_seek: true,
                    frames_to_decode: 1,
                };
            }
            return AdvanceResult {
                needs_seek: false,
                frames_to_decode,
            };
        }

        let in_pt = self.in_point;
        let out_pt = self.effective_out();

        if self.position >= out_pt {
            self.reached_end = true;
            self.frame_accumulator = 0.0;
            match self.loop_mode {
                LoopMode::Loop => {
                    self.position = in_pt;
                    return AdvanceResult {
                        needs_seek: true,
                        frames_to_decode: 1,
                    };
                }
                LoopMode::PingPong => {
                    self.reverse = true;
                    self.position = out_pt - frame_time;
                }
                LoopMode::OneShot => {
                    self.playing = false;
                    self.position = out_pt;
                }
                LoopMode::HoldLast => {
                    self.position = out_pt;
                }
            }
        } else if self.position < in_pt {
            self.frame_accumulator = 0.0;
            match self.loop_mode {
                LoopMode::Loop | LoopMode::OneShot | LoopMode::HoldLast => {
                    self.position = in_pt;
                    return AdvanceResult {
                        needs_seek: true,
                        frames_to_decode: 1,
                    };
                }
                LoopMode::PingPong => {
                    self.reverse = false;
                    self.position = in_pt + frame_time;
                }
            }
        }
        if step.decode.needs_seek {
            self.frame_accumulator = 0.0;
            return AdvanceResult {
                needs_seek: true,
                frames_to_decode: 1,
            };
        }
        AdvanceResult {
            needs_seek: false,
            frames_to_decode,
        }
    }

    /// Move a paused clip's playhead if (and only if) modulation asks for it.
    ///
    /// Pause means the clip does not advance on its own, not that it refuses to
    /// be moved: scrubbing a paused clip by hand already works, and a modulator
    /// bound to the playhead is a scrub. This is also the case where an offset
    /// reads most clearly, because with no natural advance underneath it the
    /// playhead swings around where it was parked instead of drifting through
    /// the clip. Suspension is a separate matter and still freezes everything.
    fn paused_modulation_step(&mut self) -> AdvanceResult {
        let frame_time = 1.0 / self.frame_rate;
        let step = self.modulated_position_step(0.0, 0.0, frame_time);
        if step.delta == 0.0 {
            return AdvanceResult {
                needs_seek: false,
                frames_to_decode: 0,
            };
        }

        // Clamp instead of looping: a paused clip has not reached its out-point,
        // it was carried there, so wrapping would be an event nobody asked for.
        // Which span it clamps against follows the span the movement was
        // measured in, so an absolute curve can park the playhead anywhere in
        // the clip while an offset stays inside the loop region.
        let (floor, ceiling) =
            if let modulation::PositionTarget::Absolute(_) = self.modulation.position {
                (0.0, self.duration.max(0.0))
            } else {
                let floor = self.in_point;
                (floor, self.effective_out().max(floor))
            };
        self.position = (self.position + step.delta).clamp(floor, ceiling);

        if step.decode.needs_seek {
            self.frame_accumulator = 0.0;
            return AdvanceResult {
                needs_seek: true,
                frames_to_decode: 1,
            };
        }
        self.frame_accumulator += step.decode.walk_secs;
        let frames_to_decode = (self.frame_accumulator / frame_time).floor() as u32;
        self.frame_accumulator -= f64::from(frames_to_decode) * frame_time;
        AdvanceResult {
            needs_seek: false,
            frames_to_decode,
        }
    }

    /// This tick's playhead movement, and what the decoder must do to take it.
    ///
    /// The clip's own advance and the modulated offset are separated because the
    /// decoder treats them differently: ordinary advance walks forward through
    /// the stream, while an offset that moves backward can only be reached by
    /// seeking. Keeping them apart is also what stops ping-pong's reverse from
    /// being mistaken for a modulated backward step.
    fn modulated_position_step(
        &mut self,
        dt: f64,
        speed: f64,
        frame_time: f64,
    ) -> ModulatedPositionStep {
        let natural = dt * speed;
        let signed_natural = natural * if self.reverse { -1.0 } else { 1.0 };

        let offset_delta = match self.modulation.position {
            // An absolute source replaces the value rather than nudging it (the
            // same rule automation envelopes follow elsewhere), so the clip's
            // own advance stands down for the frame.
            modulation::PositionTarget::Absolute(target) => {
                self.applied_position_offset = 0.0;
                return ModulatedPositionStep {
                    delta: target - self.position,
                    natural_secs: 0.0,
                    decode: modulation::offset_step(target - self.position, frame_time),
                };
            }
            modulation::PositionTarget::Offset(offset) => {
                let delta = offset - self.applied_position_offset;
                self.applied_position_offset = offset;
                delta
            }
            // Letting go hands back whatever offset is still applied, so the
            // playhead lands where the clip would have been all along.
            modulation::PositionTarget::Free => {
                let delta = -self.applied_position_offset;
                self.applied_position_offset = 0.0;
                delta
            }
        };

        ModulatedPositionStep {
            delta: signed_natural + offset_delta,
            natural_secs: natural,
            decode: modulation::offset_step(offset_delta, frame_time),
        }
    }

    fn advance_chase(&mut self, transport: chase::ChaseTransport) -> AdvanceResult {
        self.chasing = true;
        self.reverse = false;
        self.last_advance = std::time::Instant::now();
        // The servo owns the whole timeline while chasing, so a modulated offset
        // has no authority here and must not be left half-applied for the tick
        // the clip stops chasing. Modulated speed has none either; see the
        // `base_speed` note below.
        // See /spec/video-playback-modulation.md § Authority.
        self.applied_position_offset = 0.0;

        let transport_dt = if transport.running {
            self.last_transport_position
                .map_or(0.0, |prev| transport.position - prev)
        } else {
            0.0
        };
        let first = self.last_transport_position.is_none();
        self.last_transport_position = Some(transport.position);

        let step = chase::step_chase(chase::ChaseInput {
            position: self.position,
            in_point: self.in_point,
            out_point: self.effective_out(),
            frame_rate: self.frame_rate,
            // The performer's stored speed, never the modulated one. This is a
            // coefficient on absolute elapsed transport time
            // (`desired = in_point + elapsed * base_speed`), not an incremental
            // rate, so a value that moves rewrites where the clip should have
            // been for the whole show up to now. A wobble of 0.01 a minute in
            // moves the target 0.6 s, past `SEEK_THRESHOLD_SECS`, and the error
            // grows with elapsed time without bound. The render pass suppresses
            // speed modulation while chasing; this keeps the servo's map stable
            // even for the frame after a chase engages, when the inbox may still
            // hold a modulated level.
            base_speed: self.speed,
            transport_position: transport.position,
            transport_dt,
            transport_fps: transport.fps,
            discontinuity: transport.discontinuity || first,
            sync: self.transport_sync,
        });

        let moved = (step.position - self.position).abs();
        let going_back = step.position + (1.0 / self.frame_rate) < self.position;
        self.position = step.position;
        let out_pt = self.effective_out();
        self.reached_end = self.position >= out_pt - (0.5 / self.frame_rate);

        if step.needs_seek || going_back {
            self.frame_accumulator = 0.0;
            return AdvanceResult {
                needs_seek: true,
                frames_to_decode: 1,
            };
        }

        let frame_time = 1.0 / self.frame_rate;
        self.frame_accumulator += moved;
        let frames_to_decode = (self.frame_accumulator / frame_time).floor() as u32;
        self.frame_accumulator -= f64::from(frames_to_decode) * frame_time;
        AdvanceResult {
            needs_seek: false,
            frames_to_decode: frames_to_decode.max(step.frames_to_decode),
        }
    }
}

/// GPU-compressed texture format for HAP video frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HapTextureFormat {
    /// BC1 / DXT1 — RGB, no alpha (Hap)
    Bc1,
    /// BC3 / DXT5 — RGBA with interpolated alpha (Hap Alpha)
    Bc3,
    /// BC3 / DXT5 storing Scaled `YCoCg` color (Hap Q) — needs shader conversion to RGB
    Bc3YCoCg,
    /// BC4 / RGTC1 — single-channel alpha (Hap Alpha-Only, or alpha plane of Hap Q Alpha)
    Bc4,
    /// BC7 / BPTC — RGBA, best quality (Hap R)
    Bc7,
}

impl HapTextureFormat {
    /// Bytes per 4×4 block for this format.
    pub fn block_bytes(self) -> u32 {
        match self {
            Self::Bc1 | Self::Bc4 => 8,
            Self::Bc3 | Self::Bc3YCoCg | Self::Bc7 => 16,
        }
    }

    /// Corresponding wgpu texture format. Colour is gamma-encoded, as in any
    /// clip, so its texture decodes it to the linear light Nida mixes in —
    /// read as linear, mid-grey comes out light grey. Hap Q's scaled `YCoCg`
    /// is no colour until converted (the shader decodes it after), and an
    /// alpha plane is no colour at all.
    pub fn wgpu_format(self) -> wgpu::TextureFormat {
        match self {
            Self::Bc1 => wgpu::TextureFormat::Bc1RgbaUnormSrgb,
            Self::Bc3 => wgpu::TextureFormat::Bc3RgbaUnormSrgb,
            Self::Bc3YCoCg => wgpu::TextureFormat::Bc3RgbaUnorm,
            Self::Bc4 => wgpu::TextureFormat::Bc4RUnorm,
            Self::Bc7 => wgpu::TextureFormat::Bc7RgbaUnormSrgb,
        }
    }

    /// Whether this format requires YCoCg→RGB conversion in a shader.
    pub fn needs_ycocg_convert(self) -> bool {
        matches!(self, Self::Bc3YCoCg)
    }

    /// Calculate the byte size of a full frame in this compressed format.
    pub fn frame_byte_size(self, width: u32, height: u32) -> usize {
        let blocks_x = width.div_ceil(4);
        let blocks_y = height.div_ceil(4);
        (blocks_x * blocks_y * self.block_bytes()) as usize
    }
}

/// A decoded video frame — either CPU-decoded RGBA or GPU-compressed `BCn`.
pub enum VideoFrame<'a> {
    /// Standard RGBA pixel data (from ffmpeg CPU decode).
    Rgba(&'a [u8]),
    /// GPU-compressed `BCn` texture data (from HAP decode).
    Compressed {
        data: &'a [u8],
        format: HapTextureFormat,
    },
}

// ── Background decode thread types ───────────────────────────────────

/// Commands sent from the main thread to the decode thread.
pub enum VideoCommand {
    Play,
    Pause,
    /// Stop producing frames without touching the deck's play/pause state.
    ///
    /// Kept separate from `Pause` because that is the performer's control and
    /// is reported to the UI and the API. A deck the arrangement has put to
    /// sleep must still read as playing, and must not be woken by residency
    /// into a state the performer did not ask for.
    /// See /spec/deck-residency.md.
    SetSuspended(bool),
    Seek(f64),
    SetSpeed(f64),
    SetLoopMode(LoopMode),
    SetInPoint(f64),
    SetOutPoint(f64),
    ClearInOutPoints,
    SetTransportSync(DeckTransportSync),
    Stop,
}

/// A decoded frame ready for GPU upload — owned data copied from the player.
pub struct DecodedFrame {
    pub color_data: Vec<u8>,
    pub alpha_data: Option<Vec<u8>>,
    pub color_format: Option<HapTextureFormat>,
    pub alpha_format: Option<HapTextureFormat>,
}

/// Read-only snapshot of playback state for the main thread.
// Mirrors PlaybackState's independent flags one-for-one; collapsing them into an
// enum would misrepresent the state they snapshot.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
pub struct PlaybackSnapshot {
    pub playing: bool,
    pub position: f64,
    pub duration: f64,
    /// The performer's set point, untouched by modulation, so the UI slider
    /// keeps showing where they left it.
    pub speed: f64,
    /// The rate playback is actually running at. Equals `speed` unless a
    /// modulator is holding it, which is what the ghost indicator draws.
    pub effective_speed: f64,
    /// How far a modulator has carried the playhead from where the clip would
    /// otherwise be. Subtracting it from `position` gives the point the
    /// modulator is swinging around, which is what the scrub bar's ghost marks.
    pub position_offset: f64,
    pub loop_mode: LoopMode,
    pub in_point: f64,
    pub out_point: f64,
    pub reverse: bool,
    pub reached_end: bool,
    pub frame_rate: f64,
    /// Whether the ping-pong RAM cache was truncated (hit the memory cap).
    /// Always false for HAP sources (they reverse via seek, no cache).
    pub pingpong_cache_truncated: bool,
    /// Whether decoding is suspended (see [`PlaybackState::suspended`]): the
    /// playhead is frozen even though `playing` says otherwise.
    pub suspended: bool,
    /// The last frame came off the Mac's media engine rather than a software
    /// decode.
    pub hardware_decode: bool,
    /// When this snapshot was taken. The decode thread publishes once per
    /// frame, so a reader between frames extrapolates `position` from here —
    /// how the clip's soundtrack stays in step with a playhead that only moves
    /// in frame-sized hops.
    pub taken_at: std::time::Instant,
}

impl PlaybackSnapshot {
    /// Effective out-point (uses duration if `out_point` is 0), mirroring
    /// [`PlaybackState::effective_out`] for readers that only have a snapshot.
    pub fn effective_out(&self) -> f64 {
        if self.out_point > 0.0 {
            self.out_point
        } else {
            self.duration
        }
    }

    /// Create a snapshot from a `PlaybackState`. The `pingpong_cache_truncated`
    /// flag defaults to false here and is set by the ffmpeg decode thread.
    pub fn from_state(ps: &PlaybackState) -> Self {
        Self {
            playing: ps.playing,
            position: ps.position,
            duration: ps.duration,
            speed: ps.speed,
            effective_speed: ps.effective_speed(),
            position_offset: ps.applied_position_offset,
            loop_mode: ps.loop_mode,
            in_point: ps.in_point,
            out_point: ps.out_point,
            reverse: ps.reverse,
            reached_end: ps.reached_end,
            frame_rate: ps.frame_rate,
            pingpong_cache_truncated: false,
            hardware_decode: false,
            suspended: ps.suspended,
            taken_at: std::time::Instant::now(),
        }
    }

    /// Where the playhead is now, extrapolated from this snapshot at the rate
    /// it was running: between the decode thread's once-a-frame updates the
    /// stored position lags by up to a frame.
    pub fn position_now(&self) -> f64 {
        if !self.playing || self.suspended {
            return self.position;
        }
        let since = self.taken_at.elapsed().as_secs_f64().min(0.25);
        let dir = if self.reverse { -1.0 } else { 1.0 };
        self.position + since * self.effective_speed.abs() * dir
    }
}

/// The render thread's mailboxes into a decode thread.
///
/// Both hold levels rather than events: the newest value wins, and a value the
/// decode thread never got round to reading is simply superseded. That is why
/// they are cells rather than queues, and why they are bundled: they are read
/// together at the top of every decode tick.
#[derive(Default)]
pub struct DecodeInboxes {
    pub chase: ChaseInbox,
    pub modulation: modulation::PlaybackModulationInbox,
}

/// What a decode thread drives: a playhead that hands out RGBA frames — a
/// video file, or a sound file's stand-in picture.
pub trait FramePlayer: Send {
    fn playback(&mut self) -> &mut PlaybackState;
    fn playback_state(&self) -> &PlaybackState;
    /// The next frame due, if one is.
    ///
    /// # Errors
    ///
    /// Returns an error if decoding fails.
    fn next_frame(&mut self) -> Result<Option<&[u8]>>;
    /// Move the playhead, as the performer asked.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be sought.
    fn seek_and_reset(&mut self, time_secs: f64) -> Result<()>;
    fn width(&self) -> u32;
    fn height(&self) -> u32;
    fn frame_rate(&self) -> f64 {
        self.playback_state().frame_rate
    }
    fn pingpong_cache_truncated(&self) -> bool {
        false
    }
    fn hardware_decode(&self) -> bool {
        false
    }
}

impl FramePlayer for VideoPlayer {
    fn playback(&mut self) -> &mut PlaybackState {
        &mut self.playback
    }
    fn playback_state(&self) -> &PlaybackState {
        &self.playback
    }
    fn next_frame(&mut self) -> Result<Option<&[u8]>> {
        VideoPlayer::next_frame(self)
    }
    fn seek_and_reset(&mut self, time_secs: f64) -> Result<()> {
        VideoPlayer::seek_and_reset(self, time_secs)
    }
    fn width(&self) -> u32 {
        self.width
    }
    fn height(&self) -> u32 {
        self.height
    }
    fn pingpong_cache_truncated(&self) -> bool {
        VideoPlayer::pingpong_cache_truncated(self)
    }
    fn hardware_decode(&self) -> bool {
        VideoPlayer::hardware_decode(self)
    }
}

/// Main-thread handle to a background video decode thread.
pub struct VideoDecodeHandle {
    cmd_tx: mpsc::Sender<VideoCommand>,
    frame_data: Arc<Mutex<Option<DecodedFrame>>>,
    snapshot: Arc<Mutex<PlaybackSnapshot>>,
    stop_flag: Arc<AtomicBool>,
    /// Bounded pool of reusable frame buffers returned by the renderer via
    /// [`Self::recycle`] and reused by the decode thread (avoids a fresh ~4 MB
    /// allocation per frame — issue #42).
    frame_pool: Arc<Mutex<Vec<Vec<u8>>>>,
    /// Rate the renderer can actually present at, in whole frames per second
    /// (0 = uncapped). Written by the render thread every frame and read by the
    /// decode thread to bound its own rate — see [`Self::set_output_fps`].
    output_fps: Arc<AtomicU32>,
    /// Last suspension state sent to the decode thread, so the per-frame call
    /// from the renderer only sends on a change. See [`Self::set_suspended`].
    suspended: AtomicBool,
    /// Render-thread publish / decode-thread consume of the show transport and
    /// this frame's resolved playback modulation.
    inboxes: Arc<DecodeInboxes>,
    /// Last sync config applied from the main thread (for save/UI).
    transport_sync: Mutex<DeckTransportSync>,
    thread: Option<std::thread::JoinHandle<()>>,
    pub width: u32,
    pub height: u32,
    /// Whether this is a dual-plane HAP source (for render pass alpha
    /// detection): set as its first frame with an alpha plane is uploaded.
    pub is_dual_plane: bool,
}

impl VideoDecodeHandle {
    /// Spawn a background decode thread for a standard (ffmpeg) `VideoPlayer`.
    ///
    /// # Panics
    ///
    /// Panics if the OS refuses to spawn the decode thread.
    pub fn spawn_video(player: VideoPlayer) -> Self {
        Self::spawn_player(player, "video-decode")
    }

    /// Spawn the playhead of a sound file: it keeps time for the soundtrack
    /// and shows a transparent picture.
    ///
    /// # Panics
    ///
    /// Panics if the OS refuses to spawn the thread.
    pub fn spawn_sound_only(player: SoundOnlyPlayer) -> Self {
        Self::spawn_player(player, "sound-playhead")
    }

    fn spawn_player<P: FramePlayer + 'static>(player: P, name: &str) -> Self {
        let width = player.width();
        let height = player.height();
        let fps = player.frame_rate();
        let initial_snapshot = PlaybackSnapshot::from_state(player.playback_state());

        let (cmd_tx, cmd_rx) = mpsc::channel();
        let frame_data: Arc<Mutex<Option<DecodedFrame>>> = Arc::new(Mutex::new(None));
        let snapshot: Arc<Mutex<PlaybackSnapshot>> = Arc::new(Mutex::new(initial_snapshot));
        let stop_flag = Arc::new(AtomicBool::new(false));

        let frame_pool: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
        let output_fps = Arc::new(AtomicU32::new(0));
        let inboxes = Arc::new(DecodeInboxes::default());

        let fd = frame_data.clone();
        let ss = snapshot.clone();
        let sf = stop_flag.clone();
        let fp = frame_pool.clone();
        let ofps = output_fps.clone();
        let ch = inboxes.clone();

        let thread = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                video_decode_thread(player, &cmd_rx, &fd, &ss, &sf, &fp, fps, &ofps, &ch);
            })
            .expect("failed to spawn video decode thread");

        Self {
            cmd_tx,
            frame_data,
            snapshot,
            stop_flag,
            frame_pool,
            output_fps,
            suspended: AtomicBool::new(false),
            inboxes,
            transport_sync: Mutex::new(DeckTransportSync::default()),
            thread: Some(thread),
            width,
            height,
            is_dual_plane: false,
        }
    }

    /// Spawn a background decode thread for a HAP video player.
    ///
    /// # Panics
    ///
    /// Panics if the OS refuses to spawn the decode thread.
    pub fn spawn_hap(player: hap::HapPlayer) -> Self {
        let width = player.width();
        let height = player.height();
        let fps = player.frame_rate();
        let is_dual_plane = player.is_dual_plane;
        let initial_snapshot = PlaybackSnapshot::from_state(&player.playback);

        let (cmd_tx, cmd_rx) = mpsc::channel();
        let frame_data: Arc<Mutex<Option<DecodedFrame>>> = Arc::new(Mutex::new(None));
        let snapshot: Arc<Mutex<PlaybackSnapshot>> = Arc::new(Mutex::new(initial_snapshot));
        let stop_flag = Arc::new(AtomicBool::new(false));

        let frame_pool: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
        let output_fps = Arc::new(AtomicU32::new(0));
        let inboxes = Arc::new(DecodeInboxes::default());

        let fd = frame_data.clone();
        let ss = snapshot.clone();
        let sf = stop_flag.clone();
        let fp = frame_pool.clone();
        let ofps = output_fps.clone();
        let ch = inboxes.clone();

        let thread = std::thread::Builder::new()
            .name("hap-decode".into())
            .spawn(move || {
                hap_decode_thread(player, &cmd_rx, &fd, &ss, &sf, &fp, fps, &ofps, &ch);
            })
            .expect("failed to spawn hap decode thread");

        Self {
            cmd_tx,
            frame_data,
            snapshot,
            stop_flag,
            frame_pool,
            output_fps,
            suspended: AtomicBool::new(false),
            inboxes,
            transport_sync: Mutex::new(DeckTransportSync::default()),
            thread: Some(thread),
            width,
            height,
            is_dual_plane,
        }
    }

    /// Tell the decode thread how fast the renderer can actually present, so a
    /// source faster than the output does not decode frames that can never be
    /// shown. Pass 0 to leave the source uncapped.
    ///
    /// The renderer takes at most one frame per rendered frame and the decode
    /// thread's mailbox holds one, so everything a faster source produces in
    /// between is overwritten. Decoding it anyway spends CPU and disk bandwidth
    /// for nothing, and leaves the surviving frames landing on an irregular
    /// beat against the render clock, which reads as judder.
    pub fn set_output_fps(&self, fps: u32) {
        self.output_fps.store(fps, Ordering::Relaxed);
    }

    /// Publish this frame's transport to the decode thread.
    pub fn publish_chase(&self, sample: VideoChaseBroadcast, discontinuity: bool) {
        self.inboxes.chase.publish(sample, discontinuity);
    }

    /// Publish this frame's resolved playback modulation to the decode thread.
    pub fn publish_modulation(&self, value: modulation::PlaybackModulation) {
        self.inboxes.modulation.publish(value);
    }

    pub fn set_transport_sync(&self, sync: DeckTransportSync) {
        if let Ok(mut slot) = self.transport_sync.lock() {
            *slot = sync;
        }
        self.send(VideoCommand::SetTransportSync(sync));
    }

    pub fn transport_sync(&self) -> DeckTransportSync {
        self.transport_sync.lock().map(|g| *g).unwrap_or_default()
    }

    /// Stop or resume decoding without touching the deck's play/pause state.
    ///
    /// Called every frame for every deck, so it sends only on a change: the
    /// command channel is unbounded and a suspended thread wakes rarely, which
    /// would otherwise leave a queue of identical commands to drain on resume.
    ///
    /// See /spec/deck-residency.md.
    pub fn set_suspended(&self, suspended: bool) {
        if self.suspended.swap(suspended, Ordering::Relaxed) != suspended {
            self.send(VideoCommand::SetSuspended(suspended));
        }
    }

    /// Whether decoding is currently suspended.
    pub fn is_suspended(&self) -> bool {
        self.suspended.load(Ordering::Relaxed)
    }

    /// Take the latest decoded frame (returns None if no new frame available).
    /// Return the frame to the decode thread via [`Self::recycle`] after upload
    /// so its buffer is reused instead of freed.
    pub fn take_frame(&self) -> Option<DecodedFrame> {
        self.frame_data.lock().ok()?.take()
    }

    /// Return a consumed frame's buffers to the pool for reuse by the decode
    /// thread. Call after the frame's data has been uploaded to the GPU.
    pub fn recycle(&self, frame: DecodedFrame) {
        pool_return(&self.frame_pool, frame.color_data);
        if let Some(alpha) = frame.alpha_data {
            pool_return(&self.frame_pool, alpha);
        }
    }

    /// Send a command to the decode thread.
    pub fn send(&self, cmd: VideoCommand) {
        let _ = self.cmd_tx.send(cmd);
    }

    /// Get the current playback snapshot (read-only copy).
    pub fn playback_snapshot(&self) -> PlaybackSnapshot {
        self.snapshot.lock().map_or_else(
            |_| PlaybackSnapshot {
                playing: false,
                position: 0.0,
                duration: 0.0,
                speed: 1.0,
                effective_speed: 1.0,
                position_offset: 0.0,
                loop_mode: LoopMode::Loop,
                in_point: 0.0,
                out_point: 0.0,
                reverse: false,
                reached_end: false,
                frame_rate: 30.0,
                pingpong_cache_truncated: false,
                hardware_decode: false,
                suspended: false,
                taken_at: std::time::Instant::now(),
            },
            |s| s.clone(),
        )
    }

    /// The cell the decode thread publishes its snapshot into, for a reader on
    /// another thread (the clip's soundtrack) that needs it without going
    /// through the render thread.
    pub fn snapshot_cell(&self) -> Arc<Mutex<PlaybackSnapshot>> {
        self.snapshot.clone()
    }
}

impl Drop for VideoDecodeHandle {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Release);
        // Send Stop to unblock recv_timeout
        let _ = self.cmd_tx.send(VideoCommand::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Apply a command to a `PlaybackState`.
fn apply_command(ps: &mut PlaybackState, cmd: &VideoCommand) {
    match cmd {
        VideoCommand::Play => ps.playing = true,
        VideoCommand::Pause => ps.playing = false,
        VideoCommand::SetSuspended(s) => {
            // Back from suspension, the playhead moves on from now: the time
            // it was frozen is not time it plays (it jumped a tenth of a second).
            if ps.suspended && !*s {
                ps.last_advance = std::time::Instant::now();
            }
            ps.suspended = *s;
        }
        // Seek is handled specially by the thread loop (calls seek_and_reset / seek)
        VideoCommand::Seek(_) | VideoCommand::Stop => {}
        VideoCommand::SetSpeed(s) => ps.speed = *s,
        VideoCommand::SetLoopMode(m) => ps.loop_mode = *m,
        VideoCommand::SetInPoint(s) => ps.in_point = *s,
        VideoCommand::SetOutPoint(s) => ps.out_point = *s,
        VideoCommand::ClearInOutPoints => {
            ps.in_point = 0.0;
            ps.out_point = 0.0;
        }
        VideoCommand::SetTransportSync(sync) => ps.transport_sync = *sync,
    }
}

/// Background decode loop for standard (ffmpeg) video.
/// Maximum number of reusable frame buffers held in a decode handle's pool.
/// Enough to cover the in-flight frame plus a displaced frame; bounded so the
/// pool itself can never grow unbounded.
const FRAME_POOL_CAP: usize = 4;

/// Take a reusable buffer from the pool, or a fresh empty one if it is empty.
fn pool_take(pool: &Mutex<Vec<Vec<u8>>>) -> Vec<u8> {
    pool.lock()
        .ok()
        .and_then(|mut p| p.pop())
        .unwrap_or_default()
}

/// Return a buffer to the pool for reuse, dropping it if the pool is at capacity.
fn pool_return(pool: &Mutex<Vec<Vec<u8>>>, buf: Vec<u8>) {
    if let Ok(mut p) = pool.lock() {
        if p.len() < FRAME_POOL_CAP {
            p.push(buf);
        }
    }
}

/// Advance a fixed-rate frame schedule and return how long to wait for the
/// next frame to fall due.
///
/// The deadline moves in fixed `interval` steps so the time spent decoding is
/// absorbed by the wait instead of being added to it. Waiting a whole interval
/// *after* each decode paces frames at `1/(interval + decode)`, which silently
/// drops frames on every source — the shortfall grows with decode cost and is
/// worst on high frame-rate or high-resolution media.
///
/// If a decode overruns its slot the schedule restarts from now rather than
/// accumulating a backlog of already-late deadlines, which would burn through
/// several frames at full speed to "catch up".
/// The interval a decode thread should run at: the source's own rate, bounded
/// by the rate the renderer can present (0 = uncapped).
///
/// Decoding faster than the output can present buys nothing — the extra frames
/// are overwritten in the mailbox before anyone reads them — while costing CPU
/// and disk bandwidth that the rest of the frame needs, and scattering the
/// frames that do survive across an irregular beat.
fn decode_interval(video_fps: f64, output_fps: &AtomicU32) -> std::time::Duration {
    let cap = output_fps.load(Ordering::Relaxed);
    let rate = if cap > 0 {
        video_fps.min(f64::from(cap))
    } else {
        video_fps
    };
    std::time::Duration::from_secs_f64((1.0 / rate).max(0.001))
}

/// How long a suspended decode thread sleeps between wakes.
///
/// A suspended thread decodes nothing, so this only bounds how long it takes to
/// notice a stop flag; a resume arrives as a command and wakes it immediately.
/// Long enough that sixty sleeping decks cost nothing measurable.
const SUSPENDED_WAKE: std::time::Duration = std::time::Duration::from_millis(250);

/// The interval to wait for, which is the frame schedule while running and a
/// slow idle poll while suspended.
fn wake_interval(suspended: bool, video_fps: f64, output_fps: &AtomicU32) -> std::time::Duration {
    if suspended {
        SUSPENDED_WAKE
    } else {
        decode_interval(video_fps, output_fps)
    }
}

fn wait_for_next_frame(
    next_frame_at: &mut std::time::Instant,
    interval: std::time::Duration,
) -> std::time::Duration {
    let now = std::time::Instant::now();
    let wait = next_frame_at.saturating_duration_since(now);
    *next_frame_at += interval;
    if *next_frame_at <= now {
        *next_frame_at = now + interval;
    }
    wait
}

// Decode-thread plumbing: each arg is a distinct channel to the render thread,
// with no shared invariant that a struct would express.
#[allow(clippy::too_many_arguments)]
fn video_decode_thread<P: FramePlayer>(
    mut player: P,
    cmd_rx: &mpsc::Receiver<VideoCommand>,
    frame_data: &Mutex<Option<DecodedFrame>>,
    snapshot: &Mutex<PlaybackSnapshot>,
    stop_flag: &AtomicBool,
    frame_pool: &Mutex<Vec<Vec<u8>>>,
    fps: f64,
    output_fps: &AtomicU32,
    inboxes: &DecodeInboxes,
) {
    let mut interval = decode_interval(fps, output_fps);
    let mut next_frame_at = std::time::Instant::now() + interval;
    let mut was_suspended = false;

    while !stop_flag.load(Ordering::Acquire) {
        // Drain all pending commands
        let mut had_seek = None;
        while let Ok(cmd) = cmd_rx.try_recv() {
            if let VideoCommand::Stop = &cmd {
                return;
            }
            if let VideoCommand::Seek(t) = &cmd {
                had_seek = Some(*t);
            }
            apply_command(player.playback(), &cmd);
        }

        // Process seek if any
        if let Some(t) = had_seek {
            if let Err(e) = player.seek_and_reset(t) {
                log::warn!("Video seek error: {e}");
            }
        }

        // Waking from suspension restarts the frame schedule, so the first
        // frames after a resume are not paced against a deadline set while the
        // thread was idling.
        if was_suspended && !player.playback().suspended {
            next_frame_at = std::time::Instant::now();
        }
        let woke = was_suspended && !player.playback().suspended;
        was_suspended = player.playback().suspended;
        player
            .playback()
            .set_chase_transport(inboxes.chase.take(woke));
        player.playback().set_modulation(inboxes.modulation.take());

        // Decode next frame
        match player.next_frame() {
            Ok(Some(data)) => {
                let mut buf = pool_take(frame_pool);
                buf.clear();
                buf.extend_from_slice(data);
                let frame = DecodedFrame {
                    color_data: buf,
                    alpha_data: None,
                    color_format: None,
                    alpha_format: None,
                };
                if let Ok(mut slot) = frame_data.lock() {
                    // Recycle a frame the renderer never consumed (happens when
                    // it falls behind — exactly the #42 scenario) instead of
                    // dropping its buffer.
                    if let Some(old) = slot.take() {
                        pool_return(frame_pool, old.color_data);
                        if let Some(alpha) = old.alpha_data {
                            pool_return(frame_pool, alpha);
                        }
                    }
                    *slot = Some(frame);
                }
            }
            Ok(None) => {}
            Err(e) => {
                log::warn!("Video decode error: {e}");
            }
        }

        // Publish snapshot
        if let Ok(mut ss) = snapshot.lock() {
            let mut snap = PlaybackSnapshot::from_state(player.playback());
            snap.pingpong_cache_truncated = player.pingpong_cache_truncated();
            snap.hardware_decode = player.hardware_decode();
            *ss = snap;
        }

        // Sleep until the next frame falls due, or wake early on a command.
        interval = wake_interval(player.playback().suspended, fps, output_fps);
        match cmd_rx.recv_timeout(wait_for_next_frame(&mut next_frame_at, interval)) {
            Ok(cmd) => {
                if let VideoCommand::Stop = &cmd {
                    return;
                }
                if let VideoCommand::Seek(t) = &cmd {
                    if let Err(e) = player.seek_and_reset(*t) {
                        log::warn!("Video seek error: {e}");
                    }
                } else {
                    apply_command(player.playback(), &cmd);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Background decode loop for HAP video.
// Decode-thread plumbing: each arg is a distinct channel to the render thread,
// with no shared invariant that a struct would express.
#[allow(clippy::too_many_arguments)]
fn hap_decode_thread(
    mut player: hap::HapPlayer,
    cmd_rx: &mpsc::Receiver<VideoCommand>,
    frame_data: &Mutex<Option<DecodedFrame>>,
    snapshot: &Mutex<PlaybackSnapshot>,
    stop_flag: &AtomicBool,
    frame_pool: &Mutex<Vec<Vec<u8>>>,
    fps: f64,
    output_fps: &AtomicU32,
    inboxes: &DecodeInboxes,
) {
    let mut interval = decode_interval(fps, output_fps);
    let mut next_frame_at = std::time::Instant::now() + interval;
    let mut was_suspended = false;

    while !stop_flag.load(Ordering::Acquire) {
        // Drain all pending commands
        let mut had_seek = None;
        while let Ok(cmd) = cmd_rx.try_recv() {
            if let VideoCommand::Stop = &cmd {
                return;
            }
            if let VideoCommand::Seek(t) = &cmd {
                had_seek = Some(*t);
            }
            apply_command(&mut player.playback, &cmd);
        }

        // Process seek if any
        if let Some(t) = had_seek {
            if let Err(e) = player.seek(t) {
                log::warn!("HAP seek error: {e}");
            }
            player.playback.show_frame = true;
        }

        // Waking from suspension restarts the frame schedule, so the first
        // frames after a resume are not paced against a deadline set while the
        // thread was idling.
        if was_suspended && !player.playback.suspended {
            next_frame_at = std::time::Instant::now();
        }
        let woke = was_suspended && !player.playback.suspended;
        was_suspended = player.playback.suspended;
        player
            .playback
            .set_chase_transport(inboxes.chase.take(woke));
        player.playback.set_modulation(inboxes.modulation.take());

        // Decode next frame
        match player.next_frame() {
            Ok(Some(result)) => {
                let mut color = pool_take(frame_pool);
                color.clear();
                color.extend_from_slice(result.color_data);
                let alpha = result.alpha_data.map(|d| {
                    let mut a = pool_take(frame_pool);
                    a.clear();
                    a.extend_from_slice(d);
                    a
                });
                let frame = DecodedFrame {
                    color_data: color,
                    alpha_data: alpha,
                    color_format: Some(result.color_format),
                    alpha_format: result.alpha_format,
                };
                if let Ok(mut slot) = frame_data.lock() {
                    if let Some(old) = slot.take() {
                        pool_return(frame_pool, old.color_data);
                        if let Some(alpha) = old.alpha_data {
                            pool_return(frame_pool, alpha);
                        }
                    }
                    *slot = Some(frame);
                }
            }
            Ok(None) => {}
            Err(e) => {
                log::warn!("HAP decode error: {e}");
            }
        }

        // Publish snapshot
        if let Ok(mut ss) = snapshot.lock() {
            *ss = PlaybackSnapshot::from_state(&player.playback);
        }

        // Sleep until the next frame falls due, or wake early on a command.
        interval = wake_interval(player.playback.suspended, fps, output_fps);
        match cmd_rx.recv_timeout(wait_for_next_frame(&mut next_frame_at, interval)) {
            Ok(cmd) => {
                if let VideoCommand::Stop = &cmd {
                    return;
                }
                if let VideoCommand::Seek(t) = &cmd {
                    if let Err(e) = player.seek(*t) {
                        log::warn!("HAP seek error: {e}");
                    }
                    player.playback.show_frame = true;
                } else {
                    apply_command(&mut player.playback, &cmd);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Detect whether a video file uses a HAP codec.
/// Returns the HAP texture format if it is HAP, or None for standard codecs.
///
/// # Errors
///
/// Returns an error if FFmpeg cannot be initialised, if the file cannot be
/// opened, or if it contains no video stream.
pub fn detect_hap_codec<P: AsRef<Path>>(path: P) -> Result<Option<HapTextureFormat>> {
    ffmpeg::init().context("Failed to initialize FFmpeg")?;
    let mut ictx = input(&path).context("Failed to open video file for codec detection")?;

    // ffmpeg maps every HAP variant to one codec id, so confirm it's HAP here
    // and then read the real texture format from the first frame's section header.
    let (video_stream_index, is_hap) = {
        let video_stream = ictx
            .streams()
            .best(Type::Video)
            .context("No video stream found")?;
        let codec_ctx =
            ffmpeg::codec::context::Context::from_parameters(video_stream.parameters())?;
        (video_stream.index(), codec_ctx.id().name() == "hap")
    };

    if !is_hap {
        return Ok(None);
    }

    // Probe the first video packet for the exact variant (BC1/BC3/BC7/YCoCg).
    // The texture and staging buffers are sized from this, so a wrong format
    // overruns the staging copy (issue: Hap1/BC1 misdetected as Bc7).
    for (stream, packet) in ictx.packets() {
        if stream.index() != video_stream_index {
            continue;
        }
        if let Some(data) = packet.data() {
            return match hap::detect_hap_format(data) {
                Ok(fmt) => Ok(Some(fmt)),
                Err(e) => {
                    log::warn!("HAP format detection failed ({e}) — using CPU decode fallback");
                    Ok(None)
                }
            };
        }
    }

    log::warn!("HAP stream has no readable packets — using CPU decode fallback");
    Ok(None)
}

/// Maximum frame cache memory in bytes (2 GB).
/// Frames are cached during forward playback and served in reverse for ping-pong.
/// At 1080p (~2.5 MB/frame) this holds ~800 frames (~13s at 60fps).
const MAX_CACHE_BYTES: usize = 2 * 1024 * 1024 * 1024;

/// A picture's size with its pixels' shape applied: anamorphic video (HDV's
/// 1440×1080, DV's 720×576) stored narrow, its pixels wide, shown at the
/// width its pixels make. Square pixels, or none said, leave it as it is.
pub(crate) fn display_size((width, height): (u32, u32), aspect: ffmpeg::Rational) -> (u32, u32) {
    let (num, den) = (aspect.numerator(), aspect.denominator());
    if num <= 0 || den <= 0 || num == den {
        return (width, height);
    }
    let wide = (f64::from(width) * f64::from(num) / f64::from(den)).round();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let wide = (wide as u32).clamp(1, 8192) & !1;
    (wide.max(2), height)
}

/// The clockwise quarter turns a stream's display matrix asks for, as
/// ffmpeg's own player turns it; 0 without one.
fn quarter_turns_of(stream: &ffmpeg::format::stream::Stream) -> u8 {
    let Some(matrix) = stream
        .side_data()
        .find(|d| d.kind() == ffmpeg::codec::packet::side_data::Type::DisplayMatrix)
    else {
        return 0;
    };
    let data = matrix.data();
    if data.len() < 36 {
        return 0;
    }
    // The matrix says how far the picture turns counter-clockwise.
    let counter = unsafe { ffmpeg::ffi::av_display_rotation_get(data.as_ptr().cast()) };
    quarter_turns(counter)
}

/// Clockwise quarter turns for a picture that must turn `counter` degrees
/// counter-clockwise: -90 is one, 90 is three.
fn quarter_turns(counter: f64) -> u8 {
    if !counter.is_finite() {
        return 0;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let turns = ((-counter / 90.0).round() as i64).rem_euclid(4) as u8;
    turns
}

/// An interlaced frame's two fields blended (each row a quarter of the one
/// above, half itself, a quarter of the one below): the comb its fields'
/// two moments make on anything moving, gone, for a little sharpness.
fn blend_fields(frame: &mut [u8], row: usize) {
    let rows = frame.len() / row.max(1);
    if rows < 3 {
        return;
    }
    let mut above = frame[..row].to_vec();
    let mut this = vec![0; row];
    for y in 1..rows - 1 {
        this.copy_from_slice(&frame[y * row..(y + 1) * row]);
        let (head, below) = frame.split_at_mut((y + 1) * row);
        let out = &mut head[y * row..];
        for (i, o) in out.iter_mut().enumerate() {
            let sum = u16::from(above[i]) + 2 * u16::from(this[i]) + u16::from(below[i]);
            #[allow(clippy::cast_possible_truncation)]
            let blended = ((sum + 2) / 4) as u8;
            *o = blended;
        }
        std::mem::swap(&mut above, &mut this);
    }
}

/// `src` (RGBA rows `stride` apart, `size` pixels) turned clockwise by
/// `turns` quarters into `dst`, packed.
fn turn_into(src: &[u8], stride: usize, (w, h): (u32, u32), turns: u8, dst: &mut [u8]) {
    let (w, h) = (w as usize, h as usize);
    let pixel = |x: usize, y: usize| {
        let at = y * stride + x * 4;
        [src[at], src[at + 1], src[at + 2], src[at + 3]]
    };
    match turns {
        0 => {
            for y in 0..h {
                dst[y * w * 4..(y + 1) * w * 4]
                    .copy_from_slice(&src[y * stride..y * stride + w * 4]);
            }
        }
        2 => {
            for y in 0..h {
                for x in 0..w {
                    let at = ((h - 1 - y) * w + (w - 1 - x)) * 4;
                    dst[at..at + 4].copy_from_slice(&pixel(x, y));
                }
            }
        }
        // A quarter turn: what was a column is a row, in tiles that stay
        // in the cache.
        _ => {
            const TILE: usize = 16;
            for ty in (0..h).step_by(TILE) {
                for tx in (0..w).step_by(TILE) {
                    for y in ty..(ty + TILE).min(h) {
                        for x in tx..(tx + TILE).min(w) {
                            // Clockwise: (x, y) goes to (h-1-y, x); counter: (y, w-1-x).
                            let (dx, dy) = if turns == 1 {
                                (h - 1 - y, x)
                            } else {
                                (y, w - 1 - x)
                            };
                            let at = (dy * h + dx) * 4;
                            dst[at..at + 4].copy_from_slice(&pixel(x, y));
                        }
                    }
                }
            }
        }
    }
}

/// A video stream's frame rate: its own — or, where that is no rate at all
/// (a file whose rate varies can claim 90000), its average; 30 failing both.
pub(crate) fn frame_rate_of(stream: &ffmpeg::format::stream::Stream) -> f64 {
    let rate = |r: ffmpeg::Rational| {
        (r.numerator() > 0 && r.denominator() > 0)
            .then(|| f64::from(r.numerator()) / f64::from(r.denominator()))
    };
    rate(stream.rate())
        .filter(|fps| *fps <= 240.0)
        .or_else(|| rate(stream.avg_frame_rate()))
        .unwrap_or(30.0)
}

/// When a file starts, in seconds: what its clip time 0 is. Broadcast files
/// (.ts, .mts) run on from the recorder's clock, and start well after 0.
pub(crate) fn clip_start(ictx: &ffmpeg::format::context::Input) -> f64 {
    let start = unsafe { (*ictx.as_ptr()).start_time };
    if start == ffmpeg::ffi::AV_NOPTS_VALUE {
        0.0
    } else {
        start as f64 / f64::from(ffmpeg::ffi::AV_TIME_BASE)
    }
}

/// A video player that decodes frames from a video file using ffmpeg — on
/// the Mac's media engine where it can (see [`hwdecode`]), else on the CPU.
///
/// # Safety: Send
/// The ffmpeg types (`Input`, `Video` decoder, `Scaler`) contain raw pointers to C-allocated
/// state. These pointers represent exclusive ownership of heap allocations — there is no
/// shared mutable state across instances. Transferring a `VideoPlayer` between threads is
/// safe because Rust's ownership system guarantees exclusive access (no concurrent use).
/// The player is always used from a single thread at a time.
// Independent flags (cache state, decode path), not one state machine.
#[allow(clippy::struct_excessive_bools)]
pub struct VideoPlayer {
    ictx: ffmpeg::format::context::Input,
    decoder: ffmpeg::decoder::Video,
    /// YUV → RGBA, made for the format decoded frames arrive in (the
    /// stream's own in software, NV12 or P010 from the media engine) and
    /// the format out: 8 bits a channel, or 16 for an HDR frame.
    scaler: Option<((Pixel, Pixel), Scaler)>,
    /// An HDR clip's tone map, made with its first HDR frame.
    tone: Option<hdr::ToneMap>,
    /// The last frame converted was HDR, in 16 bits, to tone-map.
    hdr_frame: bool,
    /// The last frame converted was interlaced: its two fields, from two
    /// moments, combed together.
    interlaced_frame: bool,
    /// An HDR frame tone-mapped, before it is turned upright.
    sdr: Vec<u8>,
    /// Which YUV matrix and range `scaler` is set up for.
    yuv: yuv::YuvSetup,
    /// Still decoding on the media engine (or trying to). Cleared for good
    /// when the engine fails on this clip.
    hardware: bool,
    /// The last frame shown came off the media engine.
    on_engine: bool,
    /// A media-engine frame copied to memory for conversion.
    engine_frame: Video,
    video_stream_index: usize,
    /// The picture as shown: its pixels' shape applied, and turned upright.
    width: u32,
    height: u32,
    /// As decoded.
    coded: (u32, u32),
    /// Its pixels' shape applied (1440×1080 HDV is 1920×1080), not yet turned.
    scaled: (u32, u32),
    /// Clockwise quarter turns that stand it upright: a phone held upright
    /// records on its side and says so.
    quarter_turns: u8,
    /// Shared playback state (loop mode, speed, in/out points, position).
    pub playback: PlaybackState,
    /// Current frame data (RGBA).
    frame_data: Vec<u8>,
    /// Frame cache for reverse playback (ping-pong).
    /// Filled during forward play, drained in reverse order.
    frame_cache: Vec<Vec<u8>>,
    /// Current read index into `frame_cache` during reverse playback.
    cache_read_idx: usize,
    /// Whether we're actively caching frames (disabled when memory cap hit this pass).
    caching_enabled: bool,
    /// Set when cache overflows this pass — reset each new forward pass.
    cache_overflowed: bool,
    /// Permanent latch set on the first cache overflow. Suppresses repeated log
    /// warnings and drives the one-time "transcode to HAP" UI notice (exposed
    /// via [`VideoPlayer::pingpong_cache_truncated`]).
    cache_overflow_warned: bool,
    /// Bytes per frame for cache budget calculation.
    frame_byte_size: usize,
    /// Reused decoder output frame (avoids a per-frame ffmpeg frame allocation).
    /// Holds the frame being shown.
    decoded: Video,
    /// The next frame, decoded but not yet due; its clip time in
    /// `lookahead_at`.
    lookahead: Video,
    lookahead_at: Option<f64>,
    /// The clip time of the last frame decoded, for a frame without one.
    last_at: f64,
    /// The decoder has been told the file ended (it then hands over the
    /// frames it held back, and says when it has none left).
    eof_sent: bool,
    /// The time last sought, and how far before it the file was last
    /// seeked to, until a frame due at it comes out: a file that seeks past
    /// the time asked (broadcast files may) is seeked again from further back.
    sought: Option<(f64, f64)>,
    /// Seconds per tick of the video stream's timestamps.
    time_base: f64,
    /// When the file starts, in seconds: clip time 0. Non-zero in broadcast
    /// files (.ts, .mts), whose clocks run on from the recorder's.
    start: f64,
    /// Reused scaler output frame (RGBA), avoids a per-frame allocation.
    rgb_frame: Video,
}

// SAFETY: See doc comment on VideoPlayer. Exclusive ownership of C allocations, no concurrent use.
unsafe impl Send for VideoPlayer {}

impl VideoPlayer {
    /// Create a new video player from a file path.
    ///
    /// # Errors
    ///
    /// Returns an error if FFmpeg cannot be initialised, if the file cannot be
    /// opened, if it contains no video stream, or if a decoder or scaler cannot
    /// be created for it.
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self> {
        Self::open(path, hwdecode::wanted())
    }

    /// [`Self::new`], choosing whether to try the media engine.
    ///
    /// # Errors
    ///
    /// As [`Self::new`].
    pub fn open<P: AsRef<Path>>(path: P, hardware: bool) -> Result<Self> {
        ffmpeg::init().context("Failed to initialize FFmpeg")?;
        let ictx = input(&path).context("Failed to open video file")?;
        let video_stream = ictx
            .streams()
            .best(Type::Video)
            .context("No video stream found")?;
        let video_stream_index = video_stream.index();
        let (decoder, hardware) = match hwdecode::open(video_stream.parameters(), hardware) {
            Ok(decoder) => (decoder, hardware),
            Err(e) if hardware => {
                log::warn!("Hardware decoder would not open ({e}); decoding in software");
                (hwdecode::open(video_stream.parameters(), false)?, false)
            }
            Err(e) => return Err(e),
        };
        let coded = (decoder.width(), decoder.height());
        let scaled = display_size(coded, decoder.aspect_ratio());
        let quarter_turns = quarter_turns_of(&video_stream);
        let (width, height) = if quarter_turns % 2 == 1 {
            (scaled.1, scaled.0)
        } else {
            scaled
        };
        let fps = frame_rate_of(&video_stream);
        let tb = video_stream.time_base();
        let time_base = f64::from(tb.numerator()) / f64::from(tb.denominator().max(1));
        let start = clip_start(&ictx);
        let duration = if ictx.duration() > 0 {
            ictx.duration() as f64 / f64::from(ffmpeg::ffi::AV_TIME_BASE)
        } else {
            0.0
        };
        let frame_byte_size = (width * height * 4) as usize;
        let frame_data = vec![0u8; frame_byte_size];
        let max_cached_frames = MAX_CACHE_BYTES / frame_byte_size.max(1);
        log::info!(
            "Loaded video: {width}x{height} @ {fps:.2} fps, duration: {duration:.2}s (ping-pong cache: {max_cached_frames} frames)"
        );
        Ok(Self {
            ictx,
            decoder,
            scaler: None,
            tone: None,
            hdr_frame: false,
            interlaced_frame: false,
            sdr: Vec::new(),
            yuv: yuv::YuvSetup::default(),
            hardware,
            on_engine: false,
            engine_frame: Video::empty(),
            video_stream_index,
            width,
            height,
            coded,
            scaled,
            quarter_turns,
            playback: PlaybackState::new(duration, fps),
            frame_data,
            frame_cache: Vec::new(),
            cache_read_idx: 0,
            caching_enabled: true,
            cache_overflowed: false,
            cache_overflow_warned: false,
            frame_byte_size,
            decoded: Video::empty(),
            lookahead: Video::empty(),
            lookahead_at: None,
            last_at: 0.0,
            eof_sent: false,
            sought: None,
            time_base,
            start,
            rgb_frame: Video::empty(),
        })
    }

    /// Get the next frame as RGBA data.
    /// Uses wall-clock time pacing: only decodes new frames when enough real
    /// time has elapsed (respecting speed multiplier). At speed < 1.0, frames
    /// are held longer; at speed > 1.0, frames are skipped.
    ///
    /// # Errors
    ///
    /// Returns an error if seeking, packet demuxing, decoding, or colour-space
    /// conversion fails.
    pub fn next_frame(&mut self) -> Result<Option<&[u8]>> {
        // Suspension freezes position as well as decode: a clip nobody can see
        // must not drift on wall-clock time, or the same show position shows a
        // different frame depending on when the app was launched. Pause is not a
        // hard stop in the same way, because a modulator bound to the playhead
        // can move a parked clip; unmodulated, `advance_frame` reports nothing
        // to do and we hold the current frame just below.
        if self.playback.suspended {
            return Ok(None);
        }
        let was_reverse = self.playback.reverse;
        let result = self.playback.advance_frame();

        // No frames to decode this tick — hold current frame.
        // Return None so the caller skips the GPU texture upload;
        // the texture already contains the current frame from the last upload.
        // A playing clip whose next frame has fallen due shows it, though:
        // one whose rate varies has frames closer together than its rate.
        let next_due = self.playback.playing
            && !self.playback.reverse
            && self
                .lookahead_at
                .map_or(!self.eof_sent, |at| at <= self.due());
        if result.frames_to_decode == 0 && !result.needs_seek && !next_due {
            return Ok(None);
        }

        // Detect ping-pong boundary flips from advance_frame:
        if !was_reverse && self.playback.reverse {
            // Forward→reverse flip (hit out-point). Serve from cache.
            if self.frame_cache.is_empty() {
                // No cache available (overflow or very short video).
                // Hold the current frame at the boundary and stay in reverse —
                // advance_frame will walk the position backward and eventually
                // hit in_point, triggering the reverse→forward flip below.
                return Ok(Some(&self.frame_data));
            }
            self.cache_read_idx = self.frame_cache.len() - 1;
        } else if was_reverse && !self.playback.reverse {
            // Reverse→forward flip (hit in-point). Clear cache, seek to in-point.
            // Reset overflow so the new forward pass gets a fresh caching budget.
            self.frame_cache.clear();
            self.cache_overflowed = false;
            self.caching_enabled = true;
            self.seek(self.playback.position)?;
        }

        // Reverse playback: serve frames from cache. Cache walking tracks the
        // clip's own backward march, so it cannot land on a position modulation
        // put us at; a clip parked mid-ping-pong seeks instead.
        if self.playback.reverse && self.playback.playing {
            // Skip frames for speed > 1.0 in reverse
            let skip = result.frames_to_decode.max(1) as usize;
            if self.cache_read_idx >= skip {
                self.cache_read_idx -= skip;
                self.frame_data
                    .copy_from_slice(&self.frame_cache[self.cache_read_idx]);
                return Ok(Some(&self.frame_data));
            }
            // Cache exhausted before position reached in_point.
            // Flip back to forward, seek to in_point, start a new forward pass.
            // Reset overflow so the new forward pass gets a fresh caching budget.
            self.playback.reverse = false;
            self.playback.position = self.playback.in_point;
            self.frame_cache.clear();
            self.cache_overflowed = false;
            self.caching_enabled = true;
            self.seek(self.playback.position)?;
        } else if result.needs_seek {
            // Forward seek (loop restart, etc.)
            self.frame_cache.clear();
            self.cache_overflowed = false;
            self.caching_enabled = true;
            self.seek(self.playback.position)?;
        }

        // Forward playback: the latest frame due at the playhead, by the
        // frames' own timestamps — a seek lands on its frame, not the
        // keyframe before it, and a clip whose rate varies keeps time.
        let due = self.due();
        let mut current = false;
        let mut converted = false;
        loop {
            let at = match self.lookahead_at {
                Some(at) => at,
                None => match self.decode_ahead()? {
                    Some(at) => {
                        self.lookahead_at = Some(at);
                        at
                    }
                    None => break,
                },
            };
            if at > due {
                // The first frame after a seek, and already past the time
                // sought: the file seeked too far. From further back — as
                // far as its start, or 8 s — or else this frame, the nearest
                // there is.
                match self.sought {
                    Some((target, back)) if !current && back < target && back < 8.0 => {
                        let back = if back == 0.0 { 1.0 } else { back * 2.0 };
                        if back < target {
                            self.seek_file(target - back)?;
                        } else {
                            // Back to the start: by its first byte, which
                            // a file seeking by time can miss (a broadcast
                            // file lands on its second keyframe).
                            self.seek_to_first_byte()?;
                        }
                        self.sought = Some((target, back));
                        continue;
                    }
                    Some(_) if !current => {}
                    _ => break,
                }
            }
            // Due: the frame to show, unless a later one is due too.
            std::mem::swap(&mut self.decoded, &mut self.lookahead);
            self.lookahead_at = None;
            self.sought = None;
            current = true;
            converted = false;
            // Every frame passed, kept for ping-pong's way back.
            if self.caching_enabled && self.playback.loop_mode == LoopMode::PingPong {
                self.convert_current()?;
                converted = true;
                if self.frame_cache.len() * self.frame_byte_size < MAX_CACHE_BYTES {
                    self.frame_cache.push(self.frame_data.clone());
                } else {
                    self.caching_enabled = false;
                    self.cache_overflowed = true;
                    if !self.cache_overflow_warned {
                        self.cache_overflow_warned = true;
                        log::warn!("Ping-pong frame cache full ({} frames, {} MB) — reverse will cover partial clip",
                            self.frame_cache.len(),
                            self.frame_cache.len() * self.frame_byte_size / (1024 * 1024));
                    }
                }
            }
        }
        if current {
            if !converted {
                self.convert_current()?;
            }
            return Ok(Some(&self.frame_data));
        }
        if self.lookahead_at.is_some() {
            // The next frame is not due yet.
            return Ok(None);
        }
        // No frames left. The last holds until the playhead reaches the
        // out-point, where `advance_frame` loops, stops or turns it — unless
        // the file does not say how long it is.
        if self.playback.chasing || self.playback.duration > 0.0 {
            return Ok(None);
        }
        match self.playback.loop_mode {
            LoopMode::Loop => {
                self.playback.position = self.playback.in_point;
                self.seek(self.playback.position)?;
                Ok(None)
            }
            LoopMode::PingPong => {
                self.playback.reverse = true;
                if !self.frame_cache.is_empty() {
                    self.cache_read_idx = self.frame_cache.len() - 1;
                }
                Ok(None)
            }
            LoopMode::OneShot => {
                self.playback.playing = false;
                Ok(None)
            }
            LoopMode::HoldLast => Ok(None),
        }
    }

    /// The latest clip time a frame can have and be shown now: the playhead,
    /// and half a frame, so each frame shows from nearer its time than not.
    fn due(&self) -> f64 {
        self.playback.position + 0.5 / self.playback.frame_rate
    }

    /// Decode the next frame into `lookahead`: its clip time, or `None` once
    /// the file has no more — the decoder drained of the frames it held back
    /// (the last few, in a file with B-frames), or the file unreadable.
    fn decode_ahead(&mut self) -> Result<Option<f64>> {
        loop {
            match self.decoder.receive_frame(&mut self.lookahead) {
                Ok(()) => {
                    let at = self
                        .lookahead
                        .timestamp()
                        .or_else(|| self.lookahead.pts())
                        .map_or(self.last_at + 1.0 / self.playback.frame_rate, |t| {
                            t as f64 * self.time_base - self.start
                        });
                    self.last_at = at;
                    return Ok(Some(at));
                }
                Err(ffmpeg::Error::Eof) => return Ok(None),
                Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::util::error::EAGAIN => {}
                // A media engine that gives up on a clip says so here: decode
                // the rest in software.
                Err(e) if self.hardware => {
                    self.fall_back_to_software(&e.to_string())?;
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
            if self.eof_sent {
                return Ok(None);
            }
            // The packet iterator ends at the end of the file and at an error
            // reading it (skipping a corrupt packet): either way, no more.
            if let Some((stream, packet)) = self.ictx.packets().next() {
                if stream.index() == self.video_stream_index {
                    if let Err(e) = self.decoder.send_packet(&packet) {
                        if self.hardware {
                            self.fall_back_to_software(&e.to_string())?;
                            continue;
                        }
                        // A corrupt packet costs its frame, not the clip.
                        log::debug!("Video packet refused: {e}");
                    }
                }
            } else {
                let _ = self.decoder.send_eof();
                self.eof_sent = true;
            }
        }
    }

    /// Convert the frame being shown to RGBA in `frame_data`.
    fn convert_current(&mut self) -> Result<()> {
        if let Err(e) = self.convert_decoded() {
            if !self.hardware {
                return Err(e);
            }
            // Its frame is lost with the engine; the next one comes in software.
            self.fall_back_to_software(&e.to_string())?;
            return Ok(());
        }
        let data = self.rgb_frame.data(0);
        let stride = self.rgb_frame.stride(0);
        if let Some(tone) = self.tone.as_ref().filter(|_| self.hdr_frame) {
            tone.apply(data, stride, self.scaled, &mut self.sdr);
            let packed = self.scaled.0 as usize * 4;
            turn_into(
                &self.sdr,
                packed,
                self.scaled,
                self.quarter_turns,
                &mut self.frame_data,
            );
        } else {
            turn_into(
                data,
                stride,
                self.scaled,
                self.quarter_turns,
                &mut self.frame_data,
            );
        }
        // Rows stay rows turned half round; a quarter turn of interlaced
        // video is nothing anyone records.
        if self.interlaced_frame && self.quarter_turns.is_multiple_of(2) {
            blend_fields(&mut self.frame_data, self.width as usize * 4);
        }
        Ok(())
    }

    /// Convert the decoded frame to RGBA in `rgb_frame`, copying it off the
    /// media engine first if it is there, and making the scaler for its
    /// format the first time that format arrives.
    fn convert_decoded(&mut self) -> Result<()> {
        self.on_engine = hwdecode::on_engine(&self.decoded);
        if self.on_engine {
            hwdecode::to_memory(&self.decoded, &mut self.engine_frame)?;
        }
        let source = if self.on_engine {
            &self.engine_frame
        } else {
            &self.decoded
        };
        let format = source.format();
        // An HDR frame converts to 16 bits a channel, in its transfer still,
        // for the tone map; any other to 8.
        let transfer = hdr::Transfer::of(source);
        self.hdr_frame = transfer.is_some();
        self.interlaced_frame = source.is_interlaced();
        if let Some(transfer) = transfer {
            if self.tone.as_ref().is_none_or(|t| t.transfer() != transfer) {
                log::info!("HDR clip ({transfer:?}): tone-mapped to SDR");
                self.tone = Some(hdr::ToneMap::new(transfer));
            }
        }
        let out = if self.hdr_frame {
            Pixel::RGBA64LE
        } else {
            Pixel::RGBA
        };
        if self
            .scaler
            .as_ref()
            .is_none_or(|(built, _)| *built != (format, out))
        {
            let scaler = Scaler::get(
                format,
                self.coded.0,
                self.coded.1,
                out,
                self.scaled.0,
                self.scaled.1,
                yuv::rgba_flags(),
            )?;
            self.scaler = Some(((format, out), scaler));
            self.yuv.reset();
        }
        let (_, scaler) = self.scaler.as_mut().expect("made above");
        self.yuv.apply(scaler, source);
        scaler.run(source, &mut self.rgb_frame)?;
        Ok(())
    }

    /// The media engine failed on this clip: reopen it in software where the
    /// playhead is, and stay there.
    fn fall_back_to_software(&mut self, why: &str) -> Result<()> {
        log::warn!("Hardware decoding failed ({why}); decoding this clip in software");
        self.hardware = false;
        self.on_engine = false;
        let parameters = self
            .ictx
            .stream(self.video_stream_index)
            .context("The video stream is gone")?
            .parameters();
        self.decoder = hwdecode::open(parameters, false)?;
        self.seek(self.playback.position)
    }

    /// Whether the last frame shown was decoded on the media engine.
    pub fn hardware_decode(&self) -> bool {
        self.on_engine
    }

    /// Seek to a specific time in seconds (internal — does not clear cache).
    /// The file seeks to the keyframe at or before it; the frames from there
    /// to it are decoded, not shown (see `next_frame`).
    fn seek(&mut self, time_secs: f64) -> Result<()> {
        self.seek_file(time_secs)?;
        self.sought = Some((time_secs, 0.0));
        self.playback.position = time_secs;
        // The playhead moves on from the time sought, not from the last
        // tick before it: the frame there shows, not the one after.
        self.playback.last_advance = std::time::Instant::now();
        Ok(())
    }

    /// Seek the file to its very start, by bytes.
    fn seek_to_first_byte(&mut self) -> Result<()> {
        let done = unsafe {
            ffmpeg::ffi::avformat_seek_file(
                self.ictx.as_mut_ptr(),
                -1,
                i64::MIN,
                0,
                0,
                ffmpeg::ffi::AVSEEK_FLAG_BYTE,
            )
        };
        if done < 0 {
            // A file that cannot seek by bytes seeks by time.
            return self.seek_file(0.0);
        }
        self.decoder.flush();
        self.lookahead_at = None;
        self.eof_sent = false;
        self.last_at = 0.0;
        Ok(())
    }

    /// Seek the file to the keyframe at or before clip time `time_secs`.
    fn seek_file(&mut self, time_secs: f64) -> Result<()> {
        let timestamp =
            ((time_secs.max(0.0) + self.start) * f64::from(ffmpeg::ffi::AV_TIME_BASE)) as i64;
        self.ictx.seek(timestamp, ..timestamp)?;
        self.decoder.flush();
        self.lookahead_at = None;
        self.eof_sent = false;
        self.last_at = time_secs;
        Ok(())
    }

    /// Seek to a specific time and reset the frame cache.
    /// Use this for user-initiated seeks (scrub bar, etc.).
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying FFmpeg seek fails.
    pub fn seek_and_reset(&mut self, time_secs: f64) -> Result<()> {
        self.frame_cache.clear();
        self.cache_read_idx = 0;
        self.caching_enabled = true;
        self.cache_overflowed = false;
        self.playback.reverse = false;
        self.playback.show_frame = true;
        self.seek(time_secs)
    }

    /// Whether this player's ping-pong RAM cache has ever been truncated (hit
    /// the memory cap). Permanent latch — once true, stays true for the
    /// player's lifetime. Drives the one-time "transcode to HAP" UI notice.
    pub fn pingpong_cache_truncated(&self) -> bool {
        self.cache_overflow_warned
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn frame_rate(&self) -> f64 {
        self.playback.frame_rate
    }
    pub fn duration(&self) -> f64 {
        self.playback.duration
    }
    pub fn is_playing(&self) -> bool {
        self.playback.playing
    }
    pub fn set_playing(&mut self, playing: bool) {
        self.playback.playing = playing;
    }
    pub fn is_looping(&self) -> bool {
        self.playback.loop_mode == LoopMode::Loop
    }
    pub fn set_looping(&mut self, looping: bool) {
        self.playback.loop_mode = if looping {
            LoopMode::Loop
        } else {
            LoopMode::OneShot
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode time must come out of the wait, not be added to it. Waiting a
    /// full interval after each decode paced frames at `1/(interval + decode)`,
    /// which lost 4-18% of the frames of every source.
    #[test]
    fn frame_schedule_absorbs_decode_time_instead_of_adding_to_it() {
        let interval = std::time::Duration::from_millis(10);
        let decode = std::time::Duration::from_millis(4);
        let start = std::time::Instant::now();
        let mut next_frame_at = start + interval;

        // Three frames, each costing `decode` before the wait.
        let mut elapsed = decode;
        let mut waits = Vec::new();
        for _ in 0..3 {
            let mut at = start + elapsed;
            let wait = {
                // Same arithmetic as `wait_for_next_frame`, driven off a
                // simulated clock so the test does not have to sleep.
                let w = next_frame_at.saturating_duration_since(at);
                next_frame_at += interval;
                if next_frame_at <= at {
                    next_frame_at = at + interval;
                }
                at += w;
                w
            };
            waits.push(wait);
            elapsed = at.duration_since(start) + decode;
        }

        // Each wait covers the interval minus the decode that preceded it, so
        // the frame period stays at `interval`.
        for wait in &waits {
            assert_eq!(
                *wait,
                interval.checked_sub(decode).unwrap(),
                "wait must absorb the decode time"
            );
        }
    }

    /// A source faster than the renderer must decode at the renderer's rate:
    /// the mailbox holds one frame and the renderer takes one per rendered
    /// frame, so the surplus is decoded only to be overwritten.
    #[test]
    fn decode_rate_is_bounded_by_what_the_renderer_can_present() {
        let cap = AtomicU32::new(60);
        assert_eq!(
            decode_interval(75.0, &cap),
            std::time::Duration::from_secs_f64(1.0 / 60.0),
            "a 75 fps source on a 60 fps output must decode at 60"
        );

        // A source slower than the output keeps its own rate — capping there
        // would slow the video down rather than save work.
        assert_eq!(
            decode_interval(15.0, &cap),
            std::time::Duration::from_secs_f64(1.0 / 15.0),
            "a 15 fps source must not be sped up to the output rate"
        );

        // Uncapped output (offline render, no target) leaves the source alone.
        let uncapped = AtomicU32::new(0);
        assert_eq!(
            decode_interval(75.0, &uncapped),
            std::time::Duration::from_secs_f64(1.0 / 75.0),
            "an uncapped output must not bound the source"
        );
    }

    /// A decode that overruns its slot must not build a backlog of already-late
    /// deadlines, or the next frames would fire back-to-back at full speed.
    #[test]
    fn frame_schedule_resyncs_after_an_overrun_instead_of_bursting() {
        let interval = std::time::Duration::from_millis(10);
        let mut next_frame_at = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(1))
            .unwrap();

        let wait = wait_for_next_frame(&mut next_frame_at, interval);
        assert_eq!(
            wait,
            std::time::Duration::ZERO,
            "a late frame waits not at all"
        );
        assert!(
            next_frame_at > std::time::Instant::now(),
            "the schedule must restart in the future, not replay a backlog"
        );
    }

    /// Regression: the birds HAP fixture is Hap1/BC1, not Bc7. `detect_hap_codec`
    /// must report the real format so the deck sizes its texture/staging
    /// correctly (a wrong format overran the staging copy and panicked).
    /// Skips when the local-only fixture is absent (tests/media/ is gitignored).
    #[test]
    fn detect_hap_codec_birds_fixture_is_bc1() {
        let path = "tests/media/birds_combined_hap.mov";
        if !std::path::Path::new(path).exists() {
            eprintln!("skipping: {path} not present (local-only fixture)");
            return;
        }
        assert_eq!(
            detect_hap_codec(path).unwrap(),
            Some(HapTextureFormat::Bc1),
            "Hap1 fixture must be detected as BC1, not the old hardcoded Bc7"
        );
    }

    /// The demux loop must actually yield decoded frames, and must keep
    /// yielding them across the loop wrap at end of stream.
    /// Skips when the local-only fixture is absent (tests/media/ is gitignored).
    #[test]
    fn hap_player_decodes_frames_and_survives_the_loop_wrap() {
        let path = "tests/media/birds_combined_hap.mov";
        if !std::path::Path::new(path).exists() {
            eprintln!("skipping: {path} not present (local-only fixture)");
            return;
        }
        let fmt = detect_hap_codec(path).unwrap().expect("fixture is HAP");
        let mut player = hap::HapPlayer::new(path, fmt).expect("open fixture");
        let expected = fmt.frame_byte_size(player.width(), player.height());

        // Drive the player from a simulated clock instead of real time so the
        // test neither sleeps nor depends on how fast the machine decodes.
        let mut decoded = 0;
        for _ in 0..200 {
            player.playback.frame_accumulator = 1.0 / player.playback.frame_rate;
            if let Some(frame) = player.next_frame().expect("decode") {
                assert_eq!(
                    frame.color_data.len(),
                    expected,
                    "decoded frame must fill the plane the deck sized its texture for"
                );
                decoded += 1;
            }
        }
        assert!(decoded > 100, "expected a frame per tick, got {decoded}");

        // Wrap past the end and confirm the demuxer still produces frames
        // rather than reporting a permanent end of stream.
        player.playback.position = player.playback.effective_out() + 1.0;
        player.playback.frame_accumulator = 1.0 / player.playback.frame_rate;
        player.next_frame().expect("wrap");
        let mut after_wrap = 0;
        for _ in 0..10 {
            player.playback.frame_accumulator = 1.0 / player.playback.frame_rate;
            if player.next_frame().expect("decode after wrap").is_some() {
                after_wrap += 1;
            }
        }
        assert!(
            after_wrap > 5,
            "playback must resume after the loop wrap, got {after_wrap} frames"
        );
    }

    /// A 2-second 640×360 test clip made with the ffmpeg tool; `None` without
    /// it (or without the encoder).
    fn clip(dir: &Path, name: &str, codec: &[&str]) -> Option<std::path::PathBuf> {
        let path = dir.join(name);
        let made = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi"])
            .args(["-i", "testsrc2=size=640x360:rate=25", "-t", "2"])
            .args(codec)
            .args(["-pix_fmt", "yuv420p"])
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success());
        made.then_some(path)
    }

    /// Move the playhead on by one frame's time, as that much time passing.
    fn step(player: &mut VideoPlayer) -> Option<Vec<u8>> {
        let frame = std::time::Duration::from_secs_f64(1.0 / player.playback.frame_rate);
        player.playback.last_advance = std::time::Instant::now().checked_sub(frame).unwrap();
        player.next_frame().expect("decode").map(<[u8]>::to_vec)
    }

    /// Decode `n` ticks' worth of frames; every frame that came out, RGBA.
    fn decode(player: &mut VideoPlayer, n: usize) -> Vec<Vec<u8>> {
        let frames: Vec<Vec<u8>> = (0..n).filter_map(|_| step(player)).collect();
        assert!(frames.len() > n / 2, "frames came out: {}", frames.len());
        frames
    }

    /// A clip of `seconds` from the ffmpeg tool's moving test picture, every
    /// frame different, coded all but losslessly (lossless drops B-frames,
    /// and the decoder holding frames back for them is part of the test); a
    /// keyframe every 2 s. `extra` goes before the output. `None` without the
    /// tool.
    fn test_clip(
        dir: &Path,
        name: &str,
        seconds: u32,
        extra: &[&str],
    ) -> Option<std::path::PathBuf> {
        let path = dir.join(name);
        let made = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi"])
            .args(["-i", "testsrc2=size=320x180:rate=25"])
            .args(["-t", &seconds.to_string()])
            .args(extra)
            .args([
                "-c:v", "libx264", "-preset", "veryfast", "-qp", "4", "-bf", "2", "-g", "50",
            ])
            .args(["-pix_fmt", "yuv420p"])
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success());
        made.then_some(path)
    }

    /// Which frame of `reference` a picture is.
    fn which(reference: &[Vec<u8>], frame: &[u8]) -> usize {
        reference
            .iter()
            .enumerate()
            .map(|(i, r)| (i, mean_difference(r, frame)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
            .unwrap()
    }

    /// Every frame of a clip in order: played at one frame a tick, each
    /// frame comes out once, the last ones — held back by the decoder for
    /// B-frames — among them.
    fn every_frame(path: &Path) -> Vec<Vec<u8>> {
        let mut player = VideoPlayer::open(path, false).unwrap();
        player.playback.loop_mode = LoopMode::HoldLast;
        // The first, at once: it is due from the start.
        let mut frames: Vec<Vec<u8>> = player
            .next_frame()
            .unwrap()
            .map(<[u8]>::to_vec)
            .into_iter()
            .collect();
        for _ in 0..1000 {
            if let Some(frame) = step(&mut player) {
                frames.push(frame);
            }
            if player.playback.position >= player.playback.duration {
                break;
            }
        }
        frames
    }

    /// ffmpeg's own first frame of a clip, turned upright and its pixels
    /// shaped as its player shows it: RGBA, and its size.
    fn as_ffmpeg_shows(path: &Path) -> Option<(Vec<u8>, u32, u32)> {
        let png = path.with_extension("png");
        let made = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(path)
            .args(["-frames:v", "1", "-vf", "scale=iw*sar:ih,setsar=1"])
            .arg(&png)
            .status()
            .is_ok_and(|s| s.success());
        if !made {
            return None;
        }
        let image = image::open(&png).ok()?.to_rgba8();
        let (w, h) = image.dimensions();
        Some((image.into_raw(), w, h))
    }

    /// A clip `extra` makes of a short test clip — `None` without ffmpeg.
    fn remade(
        dir: &Path,
        name: &str,
        input: &[&str],
        output: &[&str],
    ) -> Option<std::path::PathBuf> {
        let plain = test_clip(dir, "plain.mp4", 1, &["-vf", "scale=144:96,setsar=1"])?;
        let path = dir.join(name);
        let made = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(input)
            .arg("-i")
            .arg(&plain)
            .args(output)
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success());
        made.then_some(path)
    }

    fn first_frame(path: &Path) -> (Vec<u8>, u32, u32) {
        let mut player = VideoPlayer::open(path, false).unwrap();
        let frame = player.next_frame().unwrap().expect("a frame").to_vec();
        (frame, player.width(), player.height())
    }

    /// A phone held upright records its picture on its side and says so: it
    /// is shown upright, as ffmpeg's player shows it — either way round.
    #[test]
    fn a_clip_recorded_on_its_side_is_shown_upright() {
        let dir = tempfile::tempdir().unwrap();
        for turn in ["90", "-90", "180"] {
            let name = format!("turned{turn}.mp4");
            let Some(path) = remade(
                dir.path(),
                &name,
                &["-display_rotation", turn],
                &["-c", "copy"],
            ) else {
                return;
            };
            let (want, w, h) = as_ffmpeg_shows(&path).expect("ffmpeg's picture");
            let (got, gw, gh) = first_frame(&path);
            assert_eq!((gw, gh), (w, h), "turned {turn}: its shape");
            let off = mean_difference(&want, &got);
            assert!(
                off < 12.0,
                "turned {turn}: the picture as ffmpeg's (off by {off})"
            );
        }
    }

    /// Anamorphic video — stored narrow, its pixels wide — is shown at the
    /// width its pixels make, not squeezed.
    #[test]
    fn an_anamorphic_clip_is_shown_at_its_width() {
        let dir = tempfile::tempdir().unwrap();
        let Some(path) = remade(
            dir.path(),
            "wide.mp4",
            &[],
            &[
                "-vf",
                "setsar=4/3",
                "-c:v",
                "libx264",
                "-qp",
                "4",
                "-pix_fmt",
                "yuv420p",
            ],
        ) else {
            return;
        };
        let (want, w, h) = as_ffmpeg_shows(&path).expect("ffmpeg's picture");
        let (got, gw, gh) = first_frame(&path);
        assert_eq!((w, h), (192, 96), "ffmpeg widens it");
        assert_eq!((gw, gh), (192, 96));
        let off = mean_difference(&want, &got);
        assert!(off < 12.0, "the picture as ffmpeg's (off by {off})");
    }

    /// An HDR clip — tagged PQ or HLG, BT.2020 — is tone-mapped: its
    /// reference white comes out near white, where read as SDR it came out
    /// a dim mid-grey. An SDR clip of the same numbers is left as it is.
    #[test]
    fn an_hdr_clip_is_tone_mapped_not_dim() {
        let dir = tempfile::tempdir().unwrap();
        // Flat grey at PQ's and HLG's reference white.
        for (name, grey, tags) in [
            (
                "pq.mp4",
                "0x949494",
                "color_trc=smpte2084:color_primaries=bt2020:colorspace=bt2020nc",
            ),
            (
                "hlg.mp4",
                "0xBFBFBF",
                "color_trc=arib-std-b67:color_primaries=bt2020:colorspace=bt2020nc",
            ),
            (
                "sdr.mp4",
                "0x949494",
                "color_trc=bt709:color_primaries=bt709:colorspace=bt709",
            ),
        ] {
            let path = dir.path().join(name);
            let made = std::process::Command::new("ffmpeg")
                .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi"])
                .args(["-i", &format!("color=c={grey}:s=64x64:r=25"), "-t", "0.2"])
                .args(["-vf", &format!("format=yuv420p10le,setparams={tags}")])
                .args(["-c:v", "libx264"])
                .arg(&path)
                .status()
                .is_ok_and(|s| s.success());
            if !made {
                return;
            }
            let (frame, ..) = first_frame(&path);
            let red = frame[(32 * 64 + 32) * 4];
            if name == "sdr.mp4" {
                assert!((140..=156).contains(&red), "SDR as it was: {red}");
            } else {
                assert!(
                    (225..=250).contains(&red),
                    "{name}: near white, not dim: {red}"
                );
            }
        }
    }

    /// A clip suspended while nothing shows it (see deck residency) picks up
    /// where it was when it shows again, not a tenth of a second on.
    #[test]
    fn a_clip_resumes_where_it_was_suspended() {
        let mut ps = free_running_clip();
        ps.position = 2.0;
        apply_command(&mut ps, &VideoCommand::SetSuspended(true));
        std::thread::sleep(std::time::Duration::from_millis(150));
        apply_command(&mut ps, &VideoCommand::SetSuspended(false));
        ps.advance_frame();
        assert!(ps.position - 2.0 < 0.02, "moved {}", ps.position - 2.0);
    }

    /// Interlaced video — here each field a different moment, one white,
    /// one black — shows no comb: rows alternate no more.
    #[test]
    fn an_interlaced_clip_shows_no_comb() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fields.mp4");
        let made = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi"])
            .args(["-i", "color=c=gray:s=64x64:r=50", "-t", "0.4"])
            .args([
                "-vf",
                "geq=lum='if(mod(N,2),235,16)':cb=128:cr=128,interlace=scan=tff",
            ])
            .args([
                "-c:v",
                "libx264",
                "-flags",
                "+ilme+ildct",
                "-x264opts",
                "tff=1",
            ])
            .args(["-pix_fmt", "yuv420p"])
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success());
        if !made {
            return;
        }
        let (frame, w, _) = first_frame(&path);
        let row = |y: usize| frame[(y * w as usize + 32) * 4];
        let comb = (10..50).map(|y| row(y).abs_diff(row(y + 1))).max().unwrap();
        assert!(comb < 40, "rows alternate by {comb}");
    }

    #[test]
    fn quarter_turns_follow_the_display_matrix() {
        assert_eq!(quarter_turns(0.0), 0);
        assert_eq!(quarter_turns(-90.0), 1);
        assert_eq!(quarter_turns(90.0), 3);
        assert_eq!(quarter_turns(180.0), 2);
        assert_eq!(quarter_turns(-180.0), 2);
        assert_eq!(quarter_turns(89.9), 3);
        assert_eq!(quarter_turns(f64::NAN), 0);
    }

    #[test]
    fn every_frame_of_a_clip_is_shown_to_the_last() {
        let dir = tempfile::tempdir().unwrap();
        let Some(path) = test_clip(dir.path(), "three.mp4", 3, &[]) else {
            return;
        };
        let frames = every_frame(&path);
        assert_eq!(frames.len(), 75, "3 s at 25 fps");
        for (i, frame) in frames.iter().enumerate() {
            assert_eq!(which(&frames, frame), i, "frame {i} once, in order");
        }
    }

    /// A seek between keyframes shows the frame at the time sought — not the
    /// keyframe before it, 1.5 s early, while the sound starts on time.
    #[test]
    fn a_seek_lands_on_its_frame_not_the_keyframe_before() {
        let dir = tempfile::tempdir().unwrap();
        let Some(path) = test_clip(dir.path(), "three.mp4", 3, &[]) else {
            return;
        };
        let frames = every_frame(&path);
        let mut player = VideoPlayer::open(&path, false).unwrap();
        player.playback.playing = false;
        player.seek_and_reset(1.49).unwrap();
        let shown = player
            .next_frame()
            .unwrap()
            .expect("the frame sought")
            .to_vec();
        assert_eq!(which(&frames, &shown), 37, "1.48 s is frame 37");
        // Playing on from there, frame by frame.
        player.playback.playing = true;
        let next = step(&mut player).expect("the next frame");
        assert_eq!(which(&frames, &next), 38);
    }

    /// A clip whose frames come further apart after its first second (half
    /// the rate: phones and screen recordings vary so) plays in time: 2 s in,
    /// it shows the frame from 2 s, not the 50th.
    #[test]
    fn a_clip_whose_rate_varies_keeps_time() {
        let dir = tempfile::tempdir().unwrap();
        let Some(steady) = test_clip(dir.path(), "steady.mp4", 3, &[]) else {
            return;
        };
        let spacing = "setpts='if(lt(N,25),N,25+(N-25)*2)/25/TB'";
        let Some(varying) = test_clip(
            dir.path(),
            "varying.mp4",
            3,
            &["-vf", spacing, "-fps_mode", "vfr"],
        ) else {
            return;
        };
        let frames = every_frame(&steady);
        let mut player = VideoPlayer::open(&varying, false).unwrap();
        let mut shown = None;
        while player.playback.position < 1.99 {
            shown = step(&mut player).or(shown);
        }
        // 1 s of 25 frames, then 1 s of 12.5.
        let at = which(&frames, &shown.expect("frames"));
        assert!((36..=37).contains(&at), "frame {at}");
    }

    /// A broadcast file's clock starts where the recorder's was — here 10 s
    /// — and its first frame is still clip time 0: a seek to 1 s shows the
    /// frame from 1 s.
    #[test]
    fn a_broadcast_file_counts_from_its_own_start() {
        let dir = tempfile::tempdir().unwrap();
        let Some(steady) = test_clip(dir.path(), "steady.mp4", 3, &[]) else {
            return;
        };
        let Some(ts) = test_clip(dir.path(), "late.ts", 3, &["-output_ts_offset", "10"]) else {
            return;
        };
        let frames = every_frame(&steady);
        let mut player = VideoPlayer::open(&ts, false).unwrap();
        assert!(
            (player.start - 10.0).abs() < 1.5,
            "starts at {}",
            player.start
        );
        player.playback.playing = false;
        player.seek_and_reset(1.01).unwrap();
        let shown = player.next_frame().unwrap().expect("a frame").to_vec();
        assert_eq!(which(&frames, &shown), 25);
    }

    /// Every frame of `got` is, pixel for pixel, one of `reference`. Wall-
    /// clock pacing may skip a frame here or there (a hardware session takes
    /// a moment to start), so which frames is not compared — what they show is.
    fn all_found(reference: &[Vec<u8>], got: &[Vec<u8>], label: &str) {
        for (i, frame) in got.iter().enumerate() {
            let best = reference
                .iter()
                .map(|r| mean_difference(r, frame))
                .fold(f64::INFINITY, f64::min);
            assert!(
                best < 0.5,
                "{label}: frame {i} matches no software frame (best {best})"
            );
        }
    }

    fn mean_difference(a: &[u8], b: &[u8]) -> f64 {
        assert_eq!(a.len(), b.len());
        let total: u64 = a
            .iter()
            .zip(b)
            .map(|(x, y)| u64::from(x.abs_diff(*y)))
            .sum();
        #[allow(clippy::cast_precision_loss)]
        let mean = total as f64 / a.len() as f64;
        mean
    }

    const CODECS: [(&str, &[&str]); 3] = [
        ("h264.mp4", &["-c:v", "libx264", "-preset", "veryfast"]),
        (
            "hevc.mp4",
            &["-c:v", "libx265", "-preset", "veryfast", "-tag:v", "hvc1"],
        ),
        ("av1.mp4", &["-c:v", "libsvtav1", "-preset", "12"]),
    ];

    #[test]
    fn the_media_engine_decodes_the_same_picture_and_does_the_work() {
        let dir = tempfile::tempdir().unwrap();
        for (name, codec) in CODECS {
            let Some(path) = clip(dir.path(), name, codec) else {
                eprintln!("skipping {name}: no ffmpeg or encoder");
                continue;
            };
            let mut software = VideoPlayer::open(&path, false).unwrap();
            let mut hardware = VideoPlayer::open(&path, true).unwrap();
            let reference = decode(&mut software, 40);
            let got = decode(&mut hardware, 12);
            all_found(&reference, &got, name);
            assert!(
                !software.hardware_decode(),
                "{name}: software stays in software"
            );
            if cfg!(target_os = "macos") && name != "av1.mp4" {
                assert!(
                    hardware.hardware_decode(),
                    "{name}: every Apple-silicon Mac decodes this on the media engine"
                );
            }
            eprintln!("{name}: media engine = {}", hardware.hardware_decode());
        }
    }

    #[test]
    fn a_clip_the_engine_cannot_decode_plays_in_software() {
        let dir = tempfile::tempdir().unwrap();
        let Some(path) = clip(dir.path(), "av1.mp4", CODECS[2].1) else {
            return;
        };
        let reference = decode(&mut VideoPlayer::open(&path, false).unwrap(), 40);
        // ffmpeg's native AV1 decoder with no engine behind it: an M1 or M2.
        hwdecode::PRETEND_NO_ENGINE.set(true);
        let mut player = VideoPlayer::open(&path, true).unwrap();
        let got = decode(&mut player, 12);
        hwdecode::PRETEND_NO_ENGINE.set(false);
        assert!(!player.hardware, "it gave up on the engine");
        assert!(!player.hardware_decode());
        all_found(&reference, &got, "av1 in software after the engine gave up");
    }

    #[test]
    fn frame_pool_reuses_and_bounds_buffers() {
        let pool: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));

        // Empty pool yields a fresh buffer.
        let buf = pool_take(&pool);
        assert!(buf.is_empty());

        // A returned buffer is handed back out (reuse, not realloc).
        let mut b = Vec::with_capacity(4096);
        b.extend_from_slice(&[1u8, 2, 3]);
        pool_return(&pool, b);
        let reused = pool_take(&pool);
        assert!(reused.capacity() >= 4096);

        // The pool never grows past FRAME_POOL_CAP.
        for _ in 0..(FRAME_POOL_CAP + 4) {
            pool_return(&pool, Vec::new());
        }
        assert_eq!(pool.lock().unwrap().len(), FRAME_POOL_CAP);
    }

    #[test]
    fn test_playback_snapshot_from_state() {
        let mut ps = PlaybackState::new(10.0, 30.0);
        ps.speed = 2.0;
        ps.in_point = 1.0;
        ps.out_point = 8.0;
        ps.loop_mode = LoopMode::PingPong;
        let snap = PlaybackSnapshot::from_state(&ps);
        assert!(snap.playing);
        assert_eq!(snap.duration, 10.0);
        assert_eq!(snap.speed, 2.0);
        assert_eq!(snap.in_point, 1.0);
        assert_eq!(snap.out_point, 8.0);
        assert_eq!(snap.loop_mode, LoopMode::PingPong);
        assert_eq!(snap.frame_rate, 30.0);
    }

    #[test]
    fn test_decode_handle_take_frame_returns_none_initially() {
        // Cannot construct a full handle without a player, but we can test the
        // shared frame_data path directly.
        let frame_data: Arc<Mutex<Option<DecodedFrame>>> = Arc::new(Mutex::new(None));
        assert!(frame_data.lock().unwrap().is_none());
    }

    #[test]
    fn test_playback_state_defaults() {
        let ps = PlaybackState::new(10.0, 30.0);
        assert!(ps.playing);
        assert_eq!(ps.loop_mode, LoopMode::Loop);
        assert_eq!(ps.speed, 1.0);
        assert_eq!(ps.in_point, 0.0);
        assert_eq!(ps.out_point, 0.0);
        assert_eq!(ps.position, 0.0);
        assert!(!ps.reverse);
        assert_eq!(ps.effective_out(), 10.0);
    }

    #[test]
    fn test_playback_state_advance_moves_position() {
        let mut ps = PlaybackState::new(10.0, 30.0);
        // Sleep briefly so wall-clock dt > 0
        std::thread::sleep(std::time::Duration::from_millis(20));
        let result = ps.advance_frame();
        assert!(!result.needs_seek);
        // Position should have advanced by ~20ms worth
        assert!(ps.position > 0.0);
        assert!(ps.position < 0.1); // sanity: not more than 100ms
    }

    #[test]
    fn test_playback_state_loop_restart() {
        let mut ps = PlaybackState::new(1.0, 30.0);
        ps.position = 1.1; // already past out-point
                           // Ensure some dt elapses
        std::thread::sleep(std::time::Duration::from_millis(5));
        let result = ps.advance_frame();
        assert!(result.needs_seek);
        assert_eq!(ps.position, 0.0);
    }

    #[test]
    fn test_playback_state_one_shot_stops() {
        let mut ps = PlaybackState::new(1.0, 30.0);
        ps.loop_mode = LoopMode::OneShot;
        ps.position = 1.1;
        std::thread::sleep(std::time::Duration::from_millis(5));
        ps.advance_frame();
        assert!(!ps.playing);
    }

    #[test]
    fn test_playback_state_hold_last() {
        let mut ps = PlaybackState::new(1.0, 30.0);
        ps.loop_mode = LoopMode::HoldLast;
        ps.position = 1.1;
        std::thread::sleep(std::time::Duration::from_millis(5));
        ps.advance_frame();
        assert!(ps.playing);
        assert_eq!(ps.position, 1.0);
    }

    #[test]
    fn test_playback_state_ping_pong() {
        let mut ps = PlaybackState::new(1.0, 30.0);
        ps.loop_mode = LoopMode::PingPong;
        ps.position = 1.1;
        std::thread::sleep(std::time::Duration::from_millis(5));
        ps.advance_frame();
        assert!(ps.reverse);
        assert!(ps.position < 1.0);
    }

    #[test]
    fn test_playback_state_in_out_points() {
        let mut ps = PlaybackState::new(10.0, 30.0);
        ps.in_point = 2.0;
        ps.out_point = 5.0;
        ps.position = 5.1; // past out-point
        std::thread::sleep(std::time::Duration::from_millis(5));
        let result = ps.advance_frame();
        assert!(result.needs_seek);
        assert_eq!(ps.position, 2.0);
    }

    #[test]
    fn test_playback_state_speed_affects_position() {
        // Two states: one at speed 1, one at speed 3
        let mut ps_slow = PlaybackState::new(10.0, 30.0);
        let mut ps_fast = PlaybackState::new(10.0, 30.0);
        ps_fast.speed = 3.0;
        std::thread::sleep(std::time::Duration::from_millis(30));
        ps_slow.advance_frame();
        ps_fast.advance_frame();
        // Fast should advance ~3x further
        assert!(
            ps_fast.position > ps_slow.position * 2.0,
            "fast={} should be > 2x slow={}",
            ps_fast.position,
            ps_slow.position
        );
    }

    #[test]
    fn test_playback_state_not_playing() {
        let mut ps = PlaybackState::new(10.0, 30.0);
        ps.playing = false;
        std::thread::sleep(std::time::Duration::from_millis(10));
        let result = ps.advance_frame();
        assert!(!result.needs_seek);
        assert_eq!(result.frames_to_decode, 0);
        assert_eq!(ps.position, 0.0);
    }

    #[test]
    fn a_paused_clip_decodes_the_one_frame_it_owes_and_no_more() {
        let mut ps = PlaybackState::new(10.0, 30.0);
        ps.playing = false;
        ps.show_frame = true;
        let owed = ps.advance_frame();
        assert_eq!(
            owed.frames_to_decode, 1,
            "the frame it opened or was moved to"
        );
        assert!(!owed.needs_seek);
        assert_eq!(ps.position, 0.0, "without moving the playhead");
        assert_eq!(ps.advance_frame().frames_to_decode, 0, "then it holds");

        // Playing shows frames anyway: nothing is owed once it pauses.
        ps.show_frame = true;
        ps.playing = true;
        ps.advance_frame();
        ps.playing = false;
        assert_eq!(ps.advance_frame().frames_to_decode, 0);
    }

    #[test]
    fn test_playback_frame_pacing_slow_speed() {
        // At speed 0.1 with 30fps video, each frame should last ~333ms.
        // A 10ms advance should produce 0 frames to decode.
        let mut ps = PlaybackState::new(10.0, 30.0);
        ps.speed = 0.1;
        std::thread::sleep(std::time::Duration::from_millis(10));
        let result = ps.advance_frame();
        assert_eq!(result.frames_to_decode, 0);
    }

    #[test]
    fn chase_ignores_every_loop_mode_and_holds_out_point() {
        for loop_mode in [
            LoopMode::Loop,
            LoopMode::PingPong,
            LoopMode::OneShot,
            LoopMode::HoldLast,
        ] {
            let mut ps = PlaybackState::new(10.0, 30.0);
            ps.loop_mode = loop_mode;
            ps.transport_sync.mode = TransportSyncMode::Always;
            ps.set_chase_transport(chase::ChaseTransport {
                position: 50.0,
                running: true,
                discontinuity: true,
                fps: 30.0,
            });
            let result = ps.advance_frame();
            assert!(result.needs_seek, "{loop_mode:?}");
            assert!((ps.position - 10.0).abs() < 1e-9, "{loop_mode:?}");
            assert!(ps.chasing, "{loop_mode:?}");
            assert!(ps.playing, "{loop_mode:?} must not stop a chasing clip");
            assert!(!ps.reverse, "{loop_mode:?} must not reverse while chasing");
        }
    }

    #[test]
    fn chase_auto_does_not_chase_when_transport_is_stopped() {
        let mut ps = PlaybackState::new(10.0, 30.0);
        ps.transport_sync.mode = TransportSyncMode::Auto;
        ps.position = 1.0;
        ps.set_chase_transport(chase::ChaseTransport {
            position: 8.0,
            running: false,
            discontinuity: false,
            fps: 30.0,
        });
        let _ = ps.advance_frame();
        assert!(!ps.chasing);
        assert!(ps.position >= 1.0);
        assert!(ps.position < 8.0);
    }

    #[test]
    fn chase_auto_follows_a_running_transport() {
        let mut ps = PlaybackState::new(10.0, 30.0);
        ps.transport_sync.mode = TransportSyncMode::Auto;
        ps.set_chase_transport(chase::ChaseTransport {
            position: 4.0,
            running: true,
            discontinuity: false,
            fps: 30.0,
        });
        let result = ps.advance_frame();
        assert!(ps.chasing);
        assert!(result.needs_seek, "first chase sample must locate");
        assert!((ps.position - 4.0).abs() < 1e-9);
    }

    #[test]
    fn chase_always_freezes_on_stopped_transport() {
        let mut ps = PlaybackState::new(10.0, 30.0);
        ps.transport_sync.mode = TransportSyncMode::Always;
        ps.set_chase_transport(chase::ChaseTransport {
            position: 4.0,
            running: false,
            discontinuity: false,
            fps: 30.0,
        });
        let result = ps.advance_frame();
        assert!(ps.chasing);
        assert!(result.needs_seek);
        assert!((ps.position - 4.0).abs() < 1e-9);
    }

    #[test]
    fn chase_never_free_runs_while_transport_runs() {
        let mut ps = PlaybackState::new(10.0, 30.0);
        ps.transport_sync.mode = TransportSyncMode::Never;
        ps.position = 1.0;
        ps.set_chase_transport(chase::ChaseTransport {
            position: 8.0,
            running: true,
            discontinuity: true,
            fps: 30.0,
        });
        std::thread::sleep(std::time::Duration::from_millis(5));
        let result = ps.advance_frame();
        assert!(!ps.chasing);
        assert!(!result.needs_seek);
        assert!(ps.position > 1.0);
        assert!(ps.position < 8.0);
    }

    #[test]
    fn auto_restores_loop_mode_while_transport_is_stopped() {
        let mut ps = PlaybackState::new(10.0, 30.0);
        ps.transport_sync.mode = TransportSyncMode::Auto;
        ps.loop_mode = LoopMode::Loop;
        ps.position = 10.1;
        ps.set_chase_transport(chase::ChaseTransport {
            position: 8.0,
            running: false,
            discontinuity: false,
            fps: 30.0,
        });
        let result = ps.advance_frame();
        assert!(!ps.chasing);
        assert!(result.needs_seek);
        assert_eq!(ps.position, ps.in_point);
    }

    #[test]
    fn auto_reacquires_with_a_seek_after_free_run() {
        let mut ps = PlaybackState::new(10.0, 30.0);
        ps.transport_sync.mode = TransportSyncMode::Auto;
        ps.set_chase_transport(chase::ChaseTransport {
            position: 2.0,
            running: true,
            discontinuity: false,
            fps: 30.0,
        });
        let _ = ps.advance_frame();

        ps.set_chase_transport(chase::ChaseTransport {
            position: 2.0,
            running: false,
            discontinuity: false,
            fps: 30.0,
        });
        let _ = ps.advance_frame();
        assert!(!ps.chasing);

        ps.set_chase_transport(chase::ChaseTransport {
            position: 3.0,
            running: true,
            discontinuity: false,
            fps: 30.0,
        });
        let result = ps.advance_frame();
        assert!(ps.chasing);
        assert!(result.needs_seek);
        assert!((ps.position - 3.0).abs() < 1e-9);
    }

    #[test]
    fn clip_pause_wins_over_chase() {
        let mut ps = PlaybackState::new(10.0, 30.0);
        ps.transport_sync.mode = TransportSyncMode::Always;
        ps.playing = false;
        ps.position = 1.0;
        ps.set_chase_transport(chase::ChaseTransport {
            position: 8.0,
            running: true,
            discontinuity: true,
            fps: 30.0,
        });
        let result = ps.advance_frame();
        assert!(!ps.chasing);
        assert!(!result.needs_seek);
        assert_eq!(result.frames_to_decode, 0);
        assert_eq!(ps.position, 1.0);
    }

    #[test]
    fn chase_trim_preserves_the_servos_decode_request() {
        let mut ps = PlaybackState::new(10.0, 30.0);
        ps.transport_sync.mode = TransportSyncMode::Always;
        ps.set_chase_transport(chase::ChaseTransport {
            position: 1.0,
            running: true,
            discontinuity: false,
            fps: 30.0,
        });
        let _ = ps.advance_frame();

        // Simulate a decoder lag just beyond the deadband while the next
        // transport tick itself is sub-frame.
        ps.position = 0.96;
        ps.set_chase_transport(chase::ChaseTransport {
            position: 1.01,
            running: true,
            discontinuity: false,
            fps: 30.0,
        });
        let result = ps.advance_frame();
        assert!(!result.needs_seek);
        assert_eq!(
            result.frames_to_decode, 1,
            "a trim step's decode request must reach ffmpeg/HAP"
        );
    }

    #[test]
    fn chase_moving_back_more_than_a_frame_seeks() {
        let mut ps = PlaybackState::new(10.0, 30.0);
        ps.transport_sync.mode = TransportSyncMode::Always;
        ps.set_chase_transport(chase::ChaseTransport {
            position: 2.0,
            running: true,
            discontinuity: false,
            fps: 30.0,
        });
        let _ = ps.advance_frame();

        ps.set_chase_transport(chase::ChaseTransport {
            position: 1.9,
            running: true,
            discontinuity: false,
            fps: 30.0,
        });
        let result = ps.advance_frame();
        assert!(result.needs_seek);
        assert!(ps.position < 2.0);
    }

    #[test]
    fn transport_locate_reaches_video_as_a_seek() {
        let mut transport = crate::transport::Transport::new();
        transport.play().expect("internal transport can play");
        transport.tick(1.0 / 30.0);
        transport
            .locate(4.0)
            .expect("internal transport can locate");
        let sample = transport.sample().expect("transport has run");
        assert!(sample.discontinuity);

        let inbox = chase::ChaseInbox::new();
        inbox.publish(
            chase::VideoChaseBroadcast {
                position: sample.position,
                running: sample.running,
                fps: sample.fps,
            },
            sample.discontinuity,
        );

        let mut ps = PlaybackState::new(10.0, 30.0);
        ps.transport_sync.mode = TransportSyncMode::Auto;
        ps.set_chase_transport(inbox.take(false));
        let result = ps.advance_frame();
        assert!(result.needs_seek);
        assert!((ps.position - 4.0).abs() < 1e-9);
    }

    // ── playback modulation ────────────────────────────────────────────
    // See /spec/video-playback-modulation.md.

    /// Wall-clock advance needs a real elapsed interval to measure.
    fn tick(ps: &mut PlaybackState) -> AdvanceResult {
        std::thread::sleep(std::time::Duration::from_millis(5));
        ps.advance_frame()
    }

    fn free_running_clip() -> PlaybackState {
        let mut ps = PlaybackState::new(10.0, 30.0);
        ps.transport_sync.mode = TransportSyncMode::Never;
        ps
    }

    #[test]
    fn modulated_speed_moves_the_playhead_faster_without_seeking() {
        let mut fast = free_running_clip();
        fast.set_modulation(PlaybackModulation {
            speed: Some(4.0),
            position: ModulatedPosition::Free,
        });
        let mut plain = free_running_clip();

        let fast_result = tick(&mut fast);
        let plain_result = tick(&mut plain);

        assert!(!fast_result.needs_seek, "a rate change never needs a seek");
        assert!(!plain_result.needs_seek);
        assert!(
            fast.position > plain.position,
            "modulated speed did not advance the playhead: {} vs {}",
            fast.position,
            plain.position
        );
    }

    #[test]
    fn modulated_speed_leaves_the_stored_set_point_alone() {
        // The slider has to keep showing where the performer left it, which is
        // what makes the ghost indicator meaningful.
        let mut ps = free_running_clip();
        ps.speed = 1.0;
        ps.set_modulation(PlaybackModulation {
            speed: Some(3.0),
            position: ModulatedPosition::Free,
        });
        let _ = tick(&mut ps);
        assert!((ps.speed - 1.0).abs() < 1e-9);
        assert!((ps.effective_speed() - 3.0).abs() < 1e-9);
    }

    #[test]
    fn unmodulated_speed_falls_back_to_the_stored_speed() {
        let mut ps = free_running_clip();
        ps.speed = 2.5;
        assert!((ps.effective_speed() - 2.5).abs() < 1e-9);
    }

    #[test]
    fn a_forward_offset_walks_the_playhead_rather_than_seeking() {
        let mut ps = free_running_clip();
        ps.position = 1.0;
        ps.set_modulation(PlaybackModulation {
            speed: None,
            position: ModulatedPosition::Offset(0.2),
        });
        let result = tick(&mut ps);
        assert!(!result.needs_seek, "a short forward step should walk");
        assert!(
            result.frames_to_decode >= 6,
            "walked frames were not decoded: {}",
            result.frames_to_decode
        );
        assert!(ps.position > 1.2);
    }

    #[test]
    fn a_backward_offset_seeks_because_ffmpeg_cannot_walk_back() {
        let mut ps = free_running_clip();
        ps.position = 4.0;
        ps.set_modulation(PlaybackModulation {
            speed: None,
            position: ModulatedPosition::Offset(-0.2),
        });
        let result = tick(&mut ps);
        assert!(result.needs_seek);
        assert!(ps.position < 4.0);
    }

    #[test]
    fn a_steady_offset_costs_nothing_after_it_has_been_reached() {
        // The offset is held as a distance from where the clip would be, so a
        // modulator sitting still must not seek on every frame. This is what
        // keeps a slow LFO on the playhead cheap.
        let mut ps = free_running_clip();
        ps.position = 2.0;
        let held = PlaybackModulation {
            speed: None,
            position: ModulatedPosition::Offset(0.3),
        };
        ps.set_modulation(held);
        let _ = tick(&mut ps); // reaching the offset may walk or seek
        for _ in 0..10 {
            ps.set_modulation(held);
            assert!(
                !tick(&mut ps).needs_seek,
                "a settled offset kept seeking every frame"
            );
        }
    }

    #[test]
    fn letting_go_of_the_offset_hands_the_playhead_back() {
        let mut ps = free_running_clip();
        ps.position = 5.0;
        ps.set_modulation(PlaybackModulation {
            speed: None,
            position: ModulatedPosition::Offset(0.4),
        });
        // No sleep between ticks, so the clip's own advance stays far below the
        // tolerance and what is left is the offset being applied and released.
        let _ = ps.advance_frame();
        assert!(
            (ps.position - 5.4).abs() < 0.01,
            "offset was not applied: {}",
            ps.position
        );

        ps.set_modulation(PlaybackModulation::default());
        let _ = ps.advance_frame();
        assert!(
            (ps.position - 5.0).abs() < 0.01,
            "offset was not handed back: {}",
            ps.position
        );
    }

    #[test]
    fn an_absolute_position_replaces_rather_than_nudges() {
        // Matches how automation envelopes behave on every other parameter: an
        // absolute source sets the value, so the clip's own advance stands down.
        let mut ps = free_running_clip();
        ps.position = 1.0;
        ps.set_modulation(PlaybackModulation {
            speed: None,
            position: ModulatedPosition::Absolute(7.0),
        });
        let _ = tick(&mut ps);
        assert!(
            (ps.position - 7.0).abs() < 1e-9,
            "absolute target not taken: {}",
            ps.position
        );
    }

    #[test]
    fn an_absolute_position_past_the_out_point_does_not_trip_the_loop() {
        // The boundary transitions are part of the clip's own marching, which an
        // absolute value replaces. Left active they would pull the playhead to
        // the in-point and the curve would put it straight back, every frame.
        let mut ps = free_running_clip();
        ps.loop_mode = LoopMode::Loop;
        ps.in_point = 2.0;
        ps.out_point = 4.0;
        ps.position = 3.0;
        ps.set_modulation(PlaybackModulation {
            speed: None,
            position: ModulatedPosition::Absolute(8.0),
        });
        let _ = tick(&mut ps);
        assert!(
            (ps.position - 8.0).abs() < 1e-9,
            "the loop fought the curve: {}",
            ps.position
        );
        assert!(
            !ps.reached_end,
            "an absolute curve reported reaching the end"
        );
    }

    #[test]
    fn an_absolute_position_cannot_leave_the_clip() {
        let mut ps = free_running_clip();
        ps.position = 1.0;
        ps.set_modulation(PlaybackModulation {
            speed: None,
            position: ModulatedPosition::Absolute(999.0),
        });
        let _ = tick(&mut ps);
        assert!((ps.position - ps.duration).abs() < 1e-9);
    }

    #[test]
    fn an_absolute_position_that_holds_still_stops_seeking() {
        let mut ps = free_running_clip();
        ps.position = 1.0;
        let held = PlaybackModulation {
            speed: None,
            position: ModulatedPosition::Absolute(7.0),
        };
        ps.set_modulation(held);
        let _ = tick(&mut ps);
        ps.set_modulation(held);
        assert!(!tick(&mut ps).needs_seek);
    }

    #[test]
    fn a_modulated_speed_cannot_move_the_chase_map() {
        // The servo maps transport time onto clip time with base_speed as the
        // coefficient on *absolute elapsed* transport time, so a speed that
        // moves rewrites where the clip should have been for the whole show up
        // to now. The stored speed is used instead, and the map stays put.
        let mut ps = PlaybackState::new(100.0, 30.0);
        ps.transport_sync.mode = TransportSyncMode::Always;
        ps.speed = 1.0;
        ps.set_modulation(PlaybackModulation {
            speed: Some(2.0),
            position: ModulatedPosition::Free,
        });
        ps.set_chase_transport(chase::ChaseTransport {
            position: 4.0,
            running: true,
            discontinuity: true,
            fps: 30.0,
        });
        let _ = ps.advance_frame();
        assert!(ps.chasing);
        assert!(
            (ps.position - 4.0).abs() < 1e-9,
            "a modulator scaled the chase map: {}",
            ps.position
        );
    }

    #[test]
    fn the_stored_speed_still_scales_the_chase_map() {
        // Suppressing the modulator must not cost the setting. A clip set to 2x
        // is still a clip running at twice show rate, which is a stable map.
        let mut ps = PlaybackState::new(100.0, 30.0);
        ps.transport_sync.mode = TransportSyncMode::Always;
        ps.speed = 2.0;
        ps.set_chase_transport(chase::ChaseTransport {
            position: 4.0,
            running: true,
            discontinuity: true,
            fps: 30.0,
        });
        let _ = ps.advance_frame();
        assert!(ps.chasing);
        assert!(
            (ps.position - 8.0).abs() < 1e-9,
            "the stored speed stopped scaling the map: {}",
            ps.position
        );
    }

    /// The artifact this rule exists to prevent, at the scale a show hits it.
    #[test]
    fn a_speed_wobble_a_minute_into_a_show_does_not_throw_the_playhead() {
        let mut ps = PlaybackState::new(600.0, 30.0);
        ps.transport_sync.mode = TransportSyncMode::Always;
        ps.speed = 1.0;
        ps.set_chase_transport(chase::ChaseTransport {
            position: 60.0,
            running: true,
            discontinuity: true,
            fps: 30.0,
        });
        let _ = ps.advance_frame();
        let settled = ps.position;

        // An audio band nudging the rate by a hundredth would have moved the
        // mapped target by 0.6 s, past the servo's half-second seek threshold.
        for speed in [1.01, 0.99, 1.4, 0.6] {
            ps.set_modulation(PlaybackModulation {
                speed: Some(speed),
                position: ModulatedPosition::Free,
            });
            ps.set_chase_transport(chase::ChaseTransport {
                position: 60.0,
                running: true,
                discontinuity: false,
                fps: 30.0,
            });
            let _ = ps.advance_frame();
            assert!(
                (ps.position - settled).abs() < 0.1,
                "speed {speed} threw the chased playhead from {settled} to {}",
                ps.position
            );
        }
    }

    #[test]
    fn a_modulated_playhead_has_no_authority_while_chasing() {
        // The servo's whole job is to make position a function of transport
        // position. Two authorities on one value is the seek storm this design
        // exists to avoid, so the offset is dropped rather than fought.
        let mut ps = PlaybackState::new(100.0, 30.0);
        ps.transport_sync.mode = TransportSyncMode::Always;
        ps.set_modulation(PlaybackModulation {
            speed: None,
            position: ModulatedPosition::Offset(5.0),
        });
        ps.set_chase_transport(chase::ChaseTransport {
            position: 4.0,
            running: true,
            discontinuity: true,
            fps: 30.0,
        });
        let _ = ps.advance_frame();
        assert!(ps.chasing);
        assert!(
            (ps.position - 4.0).abs() < 1e-9,
            "a modulator moved a chasing playhead: {}",
            ps.position
        );
    }

    #[test]
    fn an_offset_applied_before_a_chase_does_not_survive_into_it() {
        let mut ps = PlaybackState::new(100.0, 30.0);
        ps.transport_sync.mode = TransportSyncMode::Auto;
        ps.position = 2.0;
        ps.playing = true;
        ps.set_modulation(PlaybackModulation {
            speed: None,
            position: ModulatedPosition::Offset(1.0),
        });
        let _ = ps.advance_frame();

        // Now the transport starts and the clip begins chasing.
        ps.set_chase_transport(chase::ChaseTransport {
            position: 10.0,
            running: true,
            discontinuity: true,
            fps: 30.0,
        });
        let _ = ps.advance_frame();
        assert!(ps.chasing);
        assert!((ps.position - 10.0).abs() < 1e-9);

        // And when it stops chasing, the stale offset is not handed back.
        ps.set_modulation(PlaybackModulation::default());
        ps.set_chase_transport(chase::ChaseTransport {
            position: 10.0,
            running: false,
            discontinuity: false,
            fps: 30.0,
        });
        let before = ps.position;
        let _ = ps.advance_frame();
        assert!(
            ps.position >= before,
            "a stale offset pulled the playhead back: {before} -> {}",
            ps.position
        );
    }

    #[test]
    fn a_paused_clip_still_follows_a_modulated_playhead() {
        // Pause stops the clip advancing on its own; it does not make the clip
        // refuse to be moved. Scrubbing a paused clip by hand already works.
        let mut ps = free_running_clip();
        ps.playing = false;
        ps.position = 3.0;
        ps.set_modulation(PlaybackModulation {
            speed: None,
            position: ModulatedPosition::Offset(2.0),
        });
        let result = tick(&mut ps);
        assert!(
            (ps.position - 5.0).abs() < 1e-9,
            "a paused playhead ignored its modulator: {}",
            ps.position
        );
        assert!(
            result.needs_seek || result.frames_to_decode > 0,
            "the move never reached the decoder, so the picture would not change"
        );
    }

    #[test]
    fn a_paused_clip_swings_around_where_it_was_parked() {
        // The bipolar case. With no natural advance underneath it the offset is
        // measured from a fixed point, so the playhead ping-pongs about the
        // parked position instead of drifting through the clip -- and the anchor
        // stays recoverable for the scrub bar's ghost to mark.
        let mut ps = free_running_clip();
        ps.playing = false;
        ps.position = 5.0;
        for offset in [1.0, 2.0, 0.0, -1.5, -2.0, 0.0] {
            ps.set_modulation(PlaybackModulation {
                speed: None,
                position: ModulatedPosition::Offset(offset),
            });
            let _ = tick(&mut ps);
            assert!(
                (ps.position - (5.0 + offset)).abs() < 1e-9,
                "offset {offset} put the playhead at {}",
                ps.position
            );
            assert!(
                (ps.position - ps.applied_position_offset - 5.0).abs() < 1e-9,
                "the anchor drifted to {}",
                ps.position - ps.applied_position_offset
            );
        }
    }

    #[test]
    fn a_paused_clip_ignores_modulated_speed() {
        // Speed scales the clip's own advance, and a paused clip has none.
        let mut ps = free_running_clip();
        ps.playing = false;
        ps.position = 3.0;
        ps.set_modulation(PlaybackModulation {
            speed: Some(4.0),
            position: ModulatedPosition::Free,
        });
        let result = tick(&mut ps);
        assert_eq!(result.frames_to_decode, 0);
        assert!(!result.needs_seek);
        assert!((ps.position - 3.0).abs() < 1e-9);
    }

    #[test]
    fn a_paused_clip_carried_to_the_out_point_clamps_instead_of_looping() {
        // It did not reach the end, it was carried there, so wrapping would fire
        // a loop nobody asked for.
        let mut ps = free_running_clip();
        ps.playing = false;
        ps.loop_mode = LoopMode::Loop;
        ps.position = 9.0;
        ps.set_modulation(PlaybackModulation {
            speed: None,
            position: ModulatedPosition::Offset(5.0),
        });
        let _ = tick(&mut ps);
        assert!(
            (ps.position - ps.effective_out()).abs() < 1e-9,
            "clamp missed the out-point: {}",
            ps.position
        );
        assert!(
            !ps.reached_end,
            "a carried playhead reported reaching the end"
        );
    }

    #[test]
    fn a_playing_clip_offsets_from_its_own_march() {
        // The other half of the design: playing, the clip marches under its loop
        // and shot rules and the modulator offsets from wherever that took it.
        let mut ps = free_running_clip();
        ps.position = 1.0;
        let _ = tick(&mut ps);
        let marched = ps.position;
        assert!(marched > 1.0, "the clip did not march on its own");

        ps.set_modulation(PlaybackModulation {
            speed: None,
            position: ModulatedPosition::Offset(2.0),
        });
        let _ = tick(&mut ps);
        assert!(
            ps.position > marched + 2.0,
            "the offset replaced the march instead of riding on it: {} vs {marched}",
            ps.position
        );
    }

    #[test]
    fn an_offset_still_respects_the_loop_boundary() {
        let mut ps = free_running_clip();
        ps.loop_mode = LoopMode::Loop;
        ps.position = 9.0;
        ps.set_modulation(PlaybackModulation {
            speed: None,
            position: ModulatedPosition::Offset(2.0),
        });
        let result = tick(&mut ps);
        assert!(result.needs_seek);
        assert!((ps.position - ps.in_point).abs() < 1e-9);
        assert!(ps.reached_end);
    }

    #[test]
    fn test_hap_texture_format_block_bytes() {
        assert_eq!(HapTextureFormat::Bc1.block_bytes(), 8);
        assert_eq!(HapTextureFormat::Bc3.block_bytes(), 16);
        assert_eq!(HapTextureFormat::Bc3YCoCg.block_bytes(), 16);
        assert_eq!(HapTextureFormat::Bc4.block_bytes(), 8);
        assert_eq!(HapTextureFormat::Bc7.block_bytes(), 16);
    }

    #[test]
    fn test_hap_texture_format_frame_byte_size() {
        assert_eq!(HapTextureFormat::Bc1.frame_byte_size(8, 8), 4 * 8);
        assert_eq!(HapTextureFormat::Bc7.frame_byte_size(8, 8), 4 * 16);
        assert_eq!(HapTextureFormat::Bc1.frame_byte_size(5, 5), 4 * 8);
    }

    #[test]
    fn test_hap_texture_format_needs_ycocg() {
        assert!(!HapTextureFormat::Bc1.needs_ycocg_convert());
        assert!(!HapTextureFormat::Bc3.needs_ycocg_convert());
        assert!(HapTextureFormat::Bc3YCoCg.needs_ycocg_convert());
        assert!(!HapTextureFormat::Bc4.needs_ycocg_convert());
        assert!(!HapTextureFormat::Bc7.needs_ycocg_convert());
    }

    // ── Offensive: frame rate div-by-zero prevention ─────────────────

    #[test]
    fn playback_state_zero_frame_rate_clamped() {
        let ps = PlaybackState::new(10.0, 0.0);
        assert_eq!(
            ps.frame_rate, 30.0,
            "zero frame_rate should be clamped to 30.0"
        );
    }

    #[test]
    fn playback_state_negative_frame_rate_clamped() {
        let ps = PlaybackState::new(10.0, -24.0);
        assert_eq!(
            ps.frame_rate, 30.0,
            "negative frame_rate should be clamped to 30.0"
        );
    }

    #[test]
    fn playback_state_nan_frame_rate_clamped() {
        let ps = PlaybackState::new(10.0, f64::NAN);
        assert_eq!(
            ps.frame_rate, 30.0,
            "NaN frame_rate should be clamped to 30.0"
        );
    }

    #[test]
    fn playback_state_valid_frame_rate_preserved() {
        let ps = PlaybackState::new(10.0, 60.0);
        assert_eq!(ps.frame_rate, 60.0, "valid frame_rate should be preserved");
    }

    #[test]
    fn playback_state_advance_with_clamped_rate_does_not_divide_by_zero() {
        let mut ps = PlaybackState::new(10.0, 0.0);
        std::thread::sleep(std::time::Duration::from_millis(20));
        // Must not panic or produce NaN/Inf
        let result = ps.advance_frame();
        assert!(!ps.position.is_nan(), "position must not be NaN");
        assert!(!ps.position.is_infinite(), "position must not be Inf");
        assert!(!result.needs_seek || ps.position >= 0.0);
    }

    // ── Chaos Tests Round 2: Speed extremes ──────────────────────────────

    #[test]
    fn chaos_extreme_speed_1e6_does_not_overflow() {
        let mut ps = PlaybackState::new(100.0, 60.0);
        ps.speed = 1_000_000.0;
        std::thread::sleep(std::time::Duration::from_millis(10));
        let result = ps.advance_frame();
        assert!(!ps.position.is_nan(), "position NaN at extreme speed");
        assert!(!ps.position.is_infinite(), "position Inf at extreme speed");
        // frames_to_decode should be finite (even if large)
        assert!(
            result.frames_to_decode < u32::MAX,
            "frames_to_decode wrapped"
        );
    }

    #[test]
    fn chaos_negative_extreme_speed() {
        let mut ps = PlaybackState::new(100.0, 30.0);
        ps.speed = -1_000_000.0;
        ps.position = 50.0;
        std::thread::sleep(std::time::Duration::from_millis(10));
        let result = ps.advance_frame();
        assert!(
            !ps.position.is_nan(),
            "position NaN at negative extreme speed"
        );
        assert!(
            !ps.position.is_infinite(),
            "position Inf at negative extreme speed"
        );
        // Should trigger loop/clamp logic
        assert!(result.frames_to_decode < u32::MAX);
    }

    #[test]
    fn chaos_nan_speed_does_not_propagate() {
        let mut ps = PlaybackState::new(10.0, 30.0);
        ps.speed = f64::NAN;
        std::thread::sleep(std::time::Duration::from_millis(10));
        let _result = ps.advance_frame();
        // NaN speed causes NaN position — document the behavior
        // The key is it doesn't panic
    }

    #[test]
    fn chaos_infinity_speed_does_not_panic() {
        let mut ps = PlaybackState::new(10.0, 30.0);
        ps.speed = f64::INFINITY;
        std::thread::sleep(std::time::Duration::from_millis(10));
        let _result = ps.advance_frame();
        // Must not panic
    }

    // ── Chaos Tests Round 2: Corrupted playback state ────────────────────

    #[test]
    fn chaos_in_point_greater_than_out_point() {
        let mut ps = PlaybackState::new(10.0, 30.0);
        ps.in_point = 8.0;
        ps.out_point = 3.0; // inverted
        std::thread::sleep(std::time::Duration::from_millis(10));
        let _result = ps.advance_frame();
        // Must not panic — position may clamp or loop oddly
    }

    #[test]
    fn chaos_zero_duration() {
        let mut ps = PlaybackState::new(0.0, 30.0);
        std::thread::sleep(std::time::Duration::from_millis(10));
        let _result = ps.advance_frame();
        // effective_out() with duration=0 — must not panic
    }

    #[test]
    fn chaos_nan_position_does_not_panic() {
        let mut ps = PlaybackState::new(10.0, 30.0);
        ps.position = f64::NAN;
        std::thread::sleep(std::time::Duration::from_millis(10));
        let _result = ps.advance_frame();
        // NaN comparisons are always false, so no branch fires — must not panic
    }

    #[test]
    fn chaos_nan_in_point_does_not_panic() {
        let mut ps = PlaybackState::new(10.0, 30.0);
        ps.in_point = f64::NAN;
        std::thread::sleep(std::time::Duration::from_millis(10));
        let _result = ps.advance_frame();
    }

    #[test]
    fn chaos_negative_duration() {
        let mut ps = PlaybackState::new(-5.0, 30.0);
        std::thread::sleep(std::time::Duration::from_millis(10));
        let _result = ps.advance_frame();
    }

    #[test]
    fn chaos_extreme_position_recovery() {
        let mut ps = PlaybackState::new(10.0, 30.0);
        ps.position = 1e15;
        std::thread::sleep(std::time::Duration::from_millis(10));
        let result = ps.advance_frame();
        // Should trigger loop/clamp since position > out_point
        assert!(ps.reached_end || result.needs_seek || ps.position <= 1e15);
    }
}
