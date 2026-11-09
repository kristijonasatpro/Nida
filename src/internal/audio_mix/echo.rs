//! Echo removal: a microphone's sound with what the room's speakers play
//! taken out of it, for a call's bus, so a guest on Zoom or Teams does not
//! hear themselves come back from the room's microphones.
//!
//! The call app's own echo cancelling cannot do it here. The call plays
//! through Nida to the room, and the microphones reach the call through a
//! virtual cable, so the app never sees its sound go out and come back.
//! Nida has both: what each bus plays and what each microphone hears. As a
//! conference DSP does, each microphone gets a canceller of its own, and its
//! reference is what the room's bus plays from everything but microphones
//! (see `core::EchoPath`).
//!
//! The canceller is WebRTC's AEC3, as in Chrome and most call apps
//! (`vendor/webrtc-aec3`). Its adaptive filter learns the way from the
//! speakers to the microphone, the room and the delay (up to about half a
//! second) included, and takes the echo out. Its suppressor then takes out
//! what is left, ducking the talker a little while both talk, as any call
//! does. On a simulated room (research/2026-11-08-echo), an echo 12 dB under
//! the call was 25 dB down over its first 20 s and 42 dB once learnt. A voice
//! alone comes through clean (28-32 dB over what is changed in it) at full
//! band, 9 ms late. It costs about 46 µs of a 10 ms block per microphone on
//! an M4.

use super::core::BLOCK_FRAMES;

/// What a canceller says of the echo it is taking out.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EchoStats {
    /// How far the echo comes after the sound that makes it, in
    /// milliseconds, once found.
    pub delay_ms: Option<i32>,
    /// How much of the echo its filter takes out, in dB (ERLE).
    pub removed_db: Option<f32>,
}

/// The filter's length in 4 ms blocks: 160 ms of a room's echo (the
/// library's 52 ms was too short for a room's tail, Kmynas klauso).
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const FILTER_BLOCKS: i32 = 40;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod ffi {
    use std::os::raw::{c_char, c_double, c_float, c_int};

    #[repr(C)]
    pub struct KkAec {
        _private: [u8; 0],
    }

    #[repr(C)]
    #[derive(Default)]
    pub struct KkAecConfig {
        pub capture_rate_hz: c_int,
        pub render_rate_hz: c_int,
        pub output_rate_hz: c_int,
        pub noise_suppression: c_int,
        pub agc: c_int,
        pub export_linear: c_int,
        pub stream_delay_ms: c_int,
        pub filter_blocks: c_int,
        pub erle_max_lf: c_float,
        pub erle_max_hf: c_float,
    }

    #[repr(C)]
    #[derive(Default)]
    pub struct KkAecStats {
        pub has_delay: c_int,
        pub delay_ms: c_int,
        pub has_erl: c_int,
        pub erl_db: c_double,
        pub has_erle: c_int,
        pub erle_db: c_double,
        pub has_divergent: c_int,
        pub divergent_filter_fraction: c_double,
    }

    unsafe extern "C" {
        pub fn kk_aec_config_default(cfg: *mut KkAecConfig);
        pub fn kk_aec_create(cfg: *const KkAecConfig) -> *mut KkAec;
        pub fn kk_aec_destroy(aec: *mut KkAec);
        pub fn kk_aec_capture_frame_samples(aec: *const KkAec) -> c_int;
        pub fn kk_aec_render_frame_samples(aec: *const KkAec) -> c_int;
        pub fn kk_aec_output_frame_samples(aec: *const KkAec) -> c_int;
        pub fn kk_aec_process_render(aec: *mut KkAec, far_frame: *const c_float) -> c_int;
        pub fn kk_aec_process_capture(
            aec: *mut KkAec,
            near_frame: *const c_float,
            out_frame: *mut c_float,
        ) -> c_int;
        pub fn kk_aec_get_stats(aec: *mut KkAec, stats: *mut KkAecStats) -> c_int;
        pub fn kk_aec_version() -> *const c_char;
    }
}

/// One microphone's echo canceller: mono, 48 kHz, a block of
/// [`BLOCK_FRAMES`] (10 ms, the canceller's own frame) at a time.
pub struct Canceller {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    aec: std::ptr::NonNull<ffi::KkAec>,
    out: Vec<f32>,
}

// The canceller is used from one thread at a time (the mixer's, or the one
// that made it before handing it over); the library keeps nothing tied to
// the thread that made it.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
unsafe impl Send for Canceller {}

impl Canceller {
    /// A canceller that has heard nothing yet, or `None` where there is none
    /// (not Apple silicon, or the library refused).
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    pub fn new() -> Option<Self> {
        let rate = super::core::SAMPLE_RATE.cast_signed();
        let mut cfg = ffi::KkAecConfig::default();
        // SAFETY: fills the plain struct it is given.
        unsafe { ffi::kk_aec_config_default(&raw mut cfg) };
        cfg.capture_rate_hz = rate;
        cfg.render_rate_hz = rate;
        cfg.output_rate_hz = rate;
        cfg.filter_blocks = FILTER_BLOCKS;
        // SAFETY: a valid config; null is checked.
        let aec = std::ptr::NonNull::new(unsafe { ffi::kk_aec_create(&raw const cfg) })?;
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)] // 480.
        let frames = BLOCK_FRAMES as i32;
        // SAFETY: a live canceller.
        let sizes = unsafe {
            [
                ffi::kk_aec_capture_frame_samples(aec.as_ptr()),
                ffi::kk_aec_render_frame_samples(aec.as_ptr()),
                ffi::kk_aec_output_frame_samples(aec.as_ptr()),
            ]
        };
        if sizes != [frames; 3] {
            // SAFETY: made above, not used again.
            unsafe { ffi::kk_aec_destroy(aec.as_ptr()) };
            return None;
        }
        Some(Self {
            aec,
            out: vec![0.0; BLOCK_FRAMES],
        })
    }

    /// No canceller here: microphones reach every bus as they are.
    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    pub fn new() -> Option<Self> {
        None
    }

    /// The library's name and version, for the log.
    pub fn version() -> &'static str {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            // SAFETY: a static NUL-terminated string.
            unsafe { std::ffi::CStr::from_ptr(ffi::kk_aec_version()) }
                .to_str()
                .unwrap_or("kk_aec")
        }
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            "none"
        }
    }

    /// One block of what the speakers play (mono, [`BLOCK_FRAMES`] long;
    /// anything else is not taken).
    pub fn render(&mut self, block: &[f32]) {
        if block.len() != BLOCK_FRAMES {
            return;
        }
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        // SAFETY: a live canceller and BLOCK_FRAMES samples, its frame.
        unsafe {
            ffi::kk_aec_process_render(self.aec.as_ptr(), block.as_ptr());
        }
    }

    /// One block of the microphone (mono, [`BLOCK_FRAMES`] long), echo
    /// taken out in place. A block the library refuses is left as it was.
    pub fn capture(&mut self, block: &mut [f32]) {
        if block.len() != BLOCK_FRAMES {
            return;
        }
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            // SAFETY: a live canceller; both buffers are its frame long.
            let r = unsafe {
                ffi::kk_aec_process_capture(self.aec.as_ptr(), block.as_ptr(), self.out.as_mut_ptr())
            };
            if r == 0 {
                block.copy_from_slice(&self.out);
            }
        }
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        let _ = &self.out;
    }

    /// What it says of the echo, as of its last block.
    pub fn stats(&mut self) -> EchoStats {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            let mut s = ffi::KkAecStats::default();
            // SAFETY: a live canceller and a plain struct to fill.
            if unsafe { ffi::kk_aec_get_stats(self.aec.as_ptr(), &raw mut s) } != 0 {
                return EchoStats::default();
            }
            #[allow(clippy::cast_possible_truncation)] // dB.
            EchoStats {
                delay_ms: (s.has_delay != 0).then_some(s.delay_ms),
                removed_db: (s.has_erle != 0 && s.erle_db.is_finite()).then_some(s.erle_db as f32),
            }
        }
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        EchoStats::default()
    }
}

impl Drop for Canceller {
    fn drop(&mut self) {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        // SAFETY: made by kk_aec_create, dropped once.
        unsafe {
            ffi::kk_aec_destroy(self.aec.as_ptr());
        }
    }
}

/// A noise of its own for each seed, as loud as a voice, mostly low and
/// coming and going as a voice does (the canceller leaves a steady noise
/// alone: 11 dB taken out of one, 52 dB out of this), in the blocks `on`
/// says.
#[cfg(test)]
pub(super) fn test_voice(seed: u64, blocks: usize, on: impl Fn(usize) -> bool) -> Vec<f32> {
    let mut state = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    let mut out = Vec::with_capacity(blocks * BLOCK_FRAMES);
    let mut low = 0.0f32;
    for b in 0..blocks {
        for _ in 0..BLOCK_FRAMES {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            #[allow(clippy::cast_precision_loss)]
            let white = ((state >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0;
            low = 0.9 * low + 0.1 * white;
            // Syllables: a level of its own each block, a pause every third
            // quarter second.
            #[allow(clippy::cast_precision_loss)]
            let level = 0.3 + 0.7 * (b as f32 * 0.37 + seed as f32).sin().abs();
            let sounding = on(b) && (b / 25) % 3 != 0;
            out.push(if sounding { level * (0.3 * low + 0.02 * white) } else { 0.0 });
        }
    }
    out
}

#[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
mod tests {
    use super::*;

    fn power(x: &[f32]) -> f64 {
        x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len().max(1) as f64
    }

    fn db(p: f64) -> f64 {
        10.0 * (p + 1e-20).log10()
    }

    /// The speakers' sound reaching the mic: 40 ms later, a third as loud,
    /// with a short tail.
    fn room(far: &[f32]) -> Vec<f32> {
        let mut echo = vec![0.0f32; far.len()];
        for (i, e) in echo.iter_mut().enumerate() {
            for (delay, gain) in [(1920, 0.33f32), (2100, 0.15), (2600, 0.08), (3500, 0.04)] {
                if i >= delay {
                    *e += far[i - delay] * gain;
                }
            }
        }
        echo
    }

    #[test]
    fn the_speakers_sound_is_taken_out_of_the_microphone() {
        let blocks = 1500; // 15 s
        let far = test_voice(1, blocks, |_| true);
        let echo = room(&far);
        let mut c = Canceller::new().expect("canceller");
        let mut out = Vec::new();
        for b in 0..blocks {
            let span = b * BLOCK_FRAMES..(b + 1) * BLOCK_FRAMES;
            c.render(&far[span.clone()]);
            let mut mic = echo[span].to_vec();
            c.capture(&mut mic);
            out.extend(mic);
        }
        let last = (blocks - 300) * BLOCK_FRAMES..;
        let removed = db(power(&echo[last.clone()])) - db(power(&out[last]));
        assert!(removed > 30.0, "only {removed:.1} dB of the echo removed");
        let stats = c.stats();
        assert!(stats.delay_ms.is_some_and(|d| (20..=60).contains(&d)), "{stats:?}");
    }

    #[test]
    fn a_voice_with_nothing_playing_comes_through() {
        let blocks = 600;
        let near = test_voice(2, blocks, |_| true);
        let mut c = Canceller::new().expect("canceller");
        let mut out = Vec::new();
        for b in 0..blocks {
            let span = b * BLOCK_FRAMES..(b + 1) * BLOCK_FRAMES;
            c.render(&[0.0; BLOCK_FRAMES]);
            let mut mic = near[span].to_vec();
            c.capture(&mut mic);
            out.extend(mic);
        }
        let last = (blocks - 300) * BLOCK_FRAMES..;
        let change = db(power(&out[last.clone()])) - db(power(&near[last]));
        assert!(change.abs() < 1.5, "the voice changed by {change:.1} dB");
    }
}
