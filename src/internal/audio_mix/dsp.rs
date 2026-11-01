//! Nida's own sound processing for talks: what a conference's microphones
//! need, on the mixing thread a block at a time.
//!
//! Conference sound goes wrong in a few ways again and again: rumble and hum
//! under a lapel or lectern microphone, speakers far louder or softer than one
//! another (and than themselves, turning from the microphone), and a voice
//! whose peaks jump out of the stream. So a source has a **low cut**, a
//! **leveler** that rides each voice slowly to the same loudness, as radio
//! does, and a **compressor** that evens out what is left; a bus has a
//! **limiter** that keeps every peak under its ceiling, and a **loudness
//! meter** (ITU-R BS.1770, as streaming platforms and broadcasters measure).
//!
//! Nothing here locks or calls out once built, and nothing allocates but the
//! loudness meter's store of blocks, a small chunk every seven minutes — it
//! runs inside the mixer's lock, ahead of every device — and every change of
//! setting, and
//! every bypass, glides across a block: a step in gain on a waveform is a
//! click. FFmpeg's filters, tried first (pull request #11), allocate per frame
//! and could fail mid-show; these are a few hundred lines of arithmetic with
//! tests that drive them sample by sample.
//!
//! The mixer hands them 10 ms blocks; a live bus's callback, 1.3 ms ones. A
//! glide or a measurement counts samples, not calls ([`Glide`], the
//! leveler's 10 ms windows), so both sound alike: a bypass glided over one
//! 1.3 ms callback is a click.

use super::reader::{db_to_gain, Frame};
use super::{BLOCK_FRAMES, SAMPLE_RATE};

const RATE: f64 = SAMPLE_RATE as f64;
/// The length of a block, in seconds.
const BLOCK_SECS: f64 = BLOCK_FRAMES as f64 / RATE;
/// Below this a level is silence, in dB.
const FLOOR_DB: f64 = -120.0;

fn to_db(power_or_amplitude: f64, power: bool) -> f64 {
    if power_or_amplitude <= 0.0 || !power_or_amplitude.is_finite() {
        return FLOOR_DB;
    }
    let db = if power {
        10.0 * power_or_amplitude.log10()
    } else {
        20.0 * power_or_amplitude.log10()
    };
    db.max(FLOOR_DB)
}

/// The one-pole coefficient that moves `1 - 1/e` of the way in `secs`, for a
/// step of `step_secs`.
fn coefficient(secs: f64, step_secs: f64) -> f64 {
    if secs <= 0.0 {
        0.0
    } else {
        (-step_secs / secs).exp()
    }
}

/// A finite sample, or silence: a stray NaN must not poison a filter.
fn clean(sample: f32) -> f64 {
    if sample.is_finite() {
        f64::from(sample)
    } else {
        0.0
    }
}

/// How long a change of setting, or a bypass, glides: 10 ms, a mixer's
/// block, however the sound comes cut.
const GLIDE_FRAMES: usize = BLOCK_FRAMES;

/// A value gliding in a straight line to a new one over [`GLIDE_FRAMES`]
/// samples, from wherever it was when the new one came — the same fed in
/// 10 ms blocks or 1.3 ms ones.
#[derive(Debug, Clone, Copy)]
struct Glide {
    from: f64,
    to: f64,
    /// Samples into the glide; [`GLIDE_FRAMES`] once there.
    done: usize,
}

impl Default for Glide {
    fn default() -> Self {
        Self::at(0.0)
    }
}

// A setting is compared as given: any change, however small, glides anew,
// and a glide rests exactly where it was sent.
#[allow(clippy::float_cmp)]
impl Glide {
    const fn at(value: f64) -> Self {
        Self {
            from: value,
            to: value,
            done: GLIDE_FRAMES,
        }
    }

    /// The value now.
    fn now(&self) -> f64 {
        if self.done >= GLIDE_FRAMES {
            self.to
        } else {
            self.from + (self.to - self.from) * (self.done as f64 / GLIDE_FRAMES as f64)
        }
    }

    /// Whether it rests at `value`: there, not gliding.
    fn rests_at(&self, value: f64) -> bool {
        self.done >= GLIDE_FRAMES && self.to == value
    }

    /// The value for the next sample, on its way to `to`.
    fn next(&mut self, to: f64) -> f64 {
        if to != self.to {
            self.from = self.now();
            self.to = to;
            self.done = 0;
        }
        if self.done < GLIDE_FRAMES {
            self.done += 1;
        }
        self.now()
    }
}

/// Check each `(value, min, max)` is a finite number in its range.
fn in_ranges(checks: &[(f32, f32, f32)], what: &'static str) -> Result<(), &'static str> {
    for &(value, min, max) in checks {
        if !value.is_finite() || !(min..=max).contains(&value) {
            return Err(what);
        }
    }
    Ok(())
}

/// A saved or sent setting that is out of range falls back to the default
/// rather than reaching the sound.
macro_rules! sanitized {
    ($($config:ty),*) => {$(
        impl $config {
            #[must_use]
            pub fn sanitized(self) -> Self {
                if self.validate().is_ok() {
                    self
                } else {
                    Self::default()
                }
            }
        }
    )*};
}
sanitized!(LowCutConfig, GateConfig, LevelerConfig, CompressorConfig, LimiterConfig, DeEsserConfig);

// ── Filters ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Coeffs {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

/// A second-order filter, stereo, in transposed direct form II.
#[derive(Debug, Clone, Copy, Default)]
struct Biquad {
    c: Coeffs,
    z1: [f64; 2],
    z2: [f64; 2],
}

impl Biquad {
    fn new(c: Coeffs) -> Self {
        Self {
            c,
            ..Self::default()
        }
    }

    fn run(&mut self, channel: usize, x: f64) -> f64 {
        let c = self.c;
        let y = c.b0 * x + self.z1[channel];
        self.z1[channel] = c.b1 * x - c.a1 * y + self.z2[channel];
        self.z2[channel] = c.b2 * x - c.a2 * y;
        // Tiny leftovers decaying for ever cost time on some processors.
        for z in [&mut self.z1[channel], &mut self.z2[channel]] {
            if z.abs() < 1e-30 {
                *z = 0.0;
            }
        }
        y
    }
}

/// A source's low cut: the rumble, handling noise and hum under a voice.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct LowCutConfig {
    pub enabled: bool,
    /// Where it starts to cut, 20–400 Hz. Speech has little below 100 Hz.
    pub hz: f32,
    /// How steeply below it: 12 or 24 dB an octave.
    pub slope_db: u8,
}

impl Default for LowCutConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            hz: 100.0,
            slope_db: 24,
        }
    }
}

impl LowCutConfig {
    /// # Errors
    /// Returns an error for a frequency out of 20–400 Hz or a slope other
    /// than 12 or 24 dB an octave.
    pub fn validate(&self) -> Result<(), &'static str> {
        in_ranges(&[(self.hz, 20.0, 400.0)], "Low cut: frequency 20–400 Hz")?;
        if self.slope_db == 12 || self.slope_db == 24 {
            Ok(())
        } else {
            Err("Low cut: slope 12 or 24 dB an octave")
        }
    }
}

/// One stage of a state-variable filter, stereo, solved by trapezoidal
/// integration (Andy Simper, Cytomic): its memory is two integrators' state,
/// which stays right whatever its tuning does, so it can be retuned while it
/// runs — a slider dragged, a large jump — without a click. A biquad's
/// memory is tied to its coefficients and was not.
#[derive(Debug, Clone, Copy, Default)]
struct Svf {
    ic1: [f64; 2],
    ic2: [f64; 2],
}

impl Svf {
    /// The high-pass output for `v0`, tuned by `g` = tan(π·hz/rate) and
    /// damped by `k` = 1/Q.
    fn highpass(&mut self, channel: usize, v0: f64, g: f64, k: f64) -> f64 {
        let (v1, v2) = self.run(channel, v0, g, k);
        v0 - k * v1 - v2
    }

    /// One sample through: its band-pass (`v1`) and low-pass (`v2`)
    /// outputs, which a high-pass, a bell or a shelf mixes with the input.
    fn run(&mut self, channel: usize, v0: f64, g: f64, k: f64) -> (f64, f64) {
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        let a3 = g * a2;
        let v3 = v0 - self.ic2[channel];
        let v1 = a1 * self.ic1[channel] + a2 * v3;
        let v2 = self.ic2[channel] + a2 * self.ic1[channel] + a3 * v3;
        self.ic1[channel] = 2.0 * v1 - self.ic1[channel];
        self.ic2[channel] = 2.0 * v2 - self.ic2[channel];
        for z in [&mut self.ic1[channel], &mut self.ic2[channel]] {
            if z.abs() < 1e-30 {
                *z = 0.0;
            }
        }
        (v1, v2)
    }
}

/// Butterworth damping (1/Q): one stage for 12 dB an octave; two for 24.
const K_12: f64 = std::f64::consts::SQRT_2;
const K_24: [f64; 2] = [1.0 / 0.541_196_100_146_197, 1.0 / 1.306_562_964_876_376_5];
/// How quickly the tuning glides to a new setting: 10 ms.
const LOW_CUT_GLIDE_SECS: f64 = 0.01;

/// A Butterworth high-pass of the second or fourth order, faded in and out,
/// gliding to a new frequency or slope.
#[derive(Debug, Clone)]
pub struct LowCut {
    config: LowCutConfig,
    stages: [Svf; 2],
    /// The tuning now, gliding to the setting's: g = tan(π·hz/rate), and
    /// each stage's damping.
    g: f64,
    k: [f64; 2],
    /// How much of the second stage is heard, 0–1: all of it at 24 dB an
    /// octave, none at 12. It runs either way, so a change of slope fades.
    second: Glide,
    /// How much of the filtered sound is heard, 0–1.
    mix: Glide,
}

impl Default for LowCut {
    fn default() -> Self {
        let config = LowCutConfig::default();
        let (g, k, second) = Self::tuning(config);
        Self {
            config,
            stages: [Svf::default(); 2],
            g,
            k,
            second: Glide::at(second),
            mix: Glide::at(0.0),
        }
    }
}

impl LowCut {
    /// The tuning a setting asks for: g, the stages' damping, the second
    /// stage's share.
    fn tuning(config: LowCutConfig) -> (f64, [f64; 2], f64) {
        let hz = f64::from(config.hz.clamp(20.0, 400.0));
        let g = (std::f64::consts::PI * hz / RATE).tan();
        if config.slope_db == 12 {
            (g, [K_12, K_24[1]], 0.0)
        } else {
            (g, K_24, 1.0)
        }
    }

    pub fn configure(&mut self, config: LowCutConfig) {
        self.config = config;
    }

    /// Silence: the filters' memory cleared, as after the mixer stalled.
    pub fn clear(&mut self) {
        self.stages = [Svf::default(); 2];
    }

    /// Filter `block` in place. The filter runs whether heard or not, so it
    /// is warm the moment it is switched on.
    pub fn process(&mut self, block: &mut [Frame]) {
        let (g_to, k_to, second_to) = Self::tuning(self.config);
        let glide = 1.0 - coefficient(LOW_CUT_GLIDE_SECS, 1.0 / RATE);
        let target = if self.config.enabled { 1.0 } else { 0.0 };
        for frame in block.iter_mut() {
            self.g += (g_to - self.g) * glide;
            self.k[0] += (k_to[0] - self.k[0]) * glide;
            self.k[1] += (k_to[1] - self.k[1]) * glide;
            let second = self.second.next(second_to);
            let mix = self.mix.next(target);
            for (channel, sample) in frame.iter_mut().enumerate() {
                let dry = clean(*sample);
                let one = self.stages[0].highpass(channel, dry, self.g, self.k[0]);
                let two = self.stages[1].highpass(channel, one, self.g, self.k[1]);
                let wet = one + (two - one) * second;
                if mix > 0.0 {
                    *sample = (dry + (wet - dry) * mix) as f32;
                }
            }
        }
    }
}

// ── Gate ────────────────────────────────────────────────────────────────

/// A source's gate: in a pause it turns the room down — the hum, the
/// shuffle, the other speakers' spill — and opens the moment the voice
/// comes back. A range gate, not a hard one: closed, the sound is only
/// `range_db` down, so a voice's soft tail is turned down, not cut off.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct GateConfig {
    pub enabled: bool,
    /// It opens when the sound comes over this, dBFS, −80 to −10; it closes
    /// once it has stayed 3 dB under it for the hold.
    pub threshold_db: f32,
    /// How far down it turns the sound while closed, 0–30 dB.
    pub range_db: f32,
    /// How quickly it opens, 0.1–50 ms: a time constant, in dB.
    pub attack_ms: f32,
    /// How long it stays open after the sound fell under it, 0–500 ms.
    pub hold_ms: f32,
    /// How slowly it closes, 10–2000 ms: a time constant, in dB — two thirds
    /// of the way down in it, about 95% in three.
    pub release_ms: f32,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold_db: -50.0,
            range_db: 15.0,
            // Quick: with no look-ahead (no delay), a word's first
            // millisecond is turned down by what the gain has not yet caught.
            attack_ms: 0.5,
            hold_ms: 120.0,
            release_ms: 250.0,
        }
    }
}

impl GateConfig {
    /// # Errors
    /// Returns an error for a value that is not finite or out of its range.
    pub fn validate(&self) -> Result<(), &'static str> {
        in_ranges(
            &[
                (self.threshold_db, -80.0, -10.0),
                (self.range_db, 0.0, 30.0),
                (self.attack_ms, 0.1, 50.0),
                (self.hold_ms, 0.0, 500.0),
                (self.release_ms, 10.0, 2000.0),
            ],
            "Gate: threshold −80–−10 dBFS, range 0–30 dB, attack 0.1–50 ms, hold 0–500 ms, release 10–2000 ms",
        )
    }
}

/// How far under its threshold the sound must fall before a gate starts
/// to close: a voice hovering at the threshold does not chatter.
const GATE_HYSTERESIS_DB: f32 = 3.0;

/// How quickly a gate's key falls back after a peak: its detector follows
/// the waveform's peaks, not each zero crossing.
const GATE_KEY_DECAY_SECS: f64 = 0.008;

#[derive(Debug, Clone)]
pub struct Gate {
    config: GateConfig,
    /// The key's peak envelope, linear.
    key: f64,
    open: bool,
    /// Samples it stays open yet, the key under its closing level.
    hold_left: u64,
    /// Its gain now, dB: 0 open, minus its range closed. Followed in dB, so
    /// its release turns a pause down as far in its time whatever the range
    /// (followed in linear gain, 30 dB took seconds to reach).
    gain_db: f64,
    /// The floor, dB, gliding over 10 ms to a new range: moved at once, a
    /// closed gate's gain followed it at the attack's speed (0.1 ms at the
    /// quickest) — a click.
    floor_db: Glide,
    /// How much of the gated sound is heard, 0–1.
    mix: Glide,
    /// The deepest gain heard in the last call.
    deepest: f64,
}

impl Default for Gate {
    fn default() -> Self {
        Self {
            config: GateConfig::default(),
            key: 0.0,
            open: false,
            hold_left: 0,
            gain_db: 0.0,
            floor_db: Glide::at(-f64::from(GateConfig::default().range_db)),
            mix: Glide::at(0.0),
            deepest: 1.0,
        }
    }
}

impl Gate {
    pub fn configure(&mut self, config: GateConfig) {
        self.config = config;
    }

    /// How far down it turns the sound now, dB (≥ 0).
    pub fn reduction_db(&self) -> f32 {
        -(to_db(self.deepest, false) as f32)
    }

    /// The mixer stalled: what it was keyed by is long gone. Open, so the
    /// first word after is not cut.
    pub fn clear(&mut self) {
        self.key = 0.0;
        self.open = false;
        self.hold_left = 0;
        self.gain_db = 0.0;
    }

    /// Gate `block` in place, keyed by `block` itself as it comes (its
    /// input: after the low cut, before anything after it).
    pub fn process(&mut self, block: &mut [Frame]) {
        let c = self.config;
        let target_mix = if c.enabled { 1.0 } else { 0.0 };
        let floor_to = -f64::from(c.range_db);
        let opens = f64::from(db_to_gain(c.threshold_db));
        let closes = f64::from(db_to_gain(c.threshold_db - GATE_HYSTERESIS_DB));
        let decay = coefficient(GATE_KEY_DECAY_SECS, 1.0 / RATE);
        let attack = coefficient(f64::from(c.attack_ms) / 1000.0, 1.0 / RATE);
        let release = coefficient(f64::from(c.release_ms) / 1000.0, 1.0 / RATE);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // ≤ 24 000.
        let hold = (f64::from(c.hold_ms) / 1000.0 * RATE).round() as u64;
        let mut deepest = 1.0_f64;
        for frame in block.iter_mut() {
            let peak = clean(frame[0]).abs().max(clean(frame[1]).abs());
            self.key = peak.max(self.key * decay);
            if self.key < 1e-30 {
                self.key = 0.0;
            }
            if self.key >= opens {
                self.open = true;
                self.hold_left = hold;
            } else if self.open && self.key >= closes {
                self.hold_left = hold;
            } else if self.open {
                if self.hold_left == 0 {
                    self.open = false;
                } else {
                    self.hold_left -= 1;
                }
            }
            let floor = self.floor_db.next(floor_to);
            let target = if self.open { 0.0 } else { floor };
            let k = if target > self.gain_db { attack } else { release };
            self.gain_db = self.gain_db * k + target * (1.0 - k);
            if self.gain_db.abs() < 1e-9 {
                self.gain_db = 0.0;
            }
            let gain = if self.gain_db == 0.0 { 1.0 } else { 10f64.powf(self.gain_db / 20.0) };
            let mix = self.mix.next(target_mix);
            let g = 1.0 + (gain - 1.0) * mix;
            deepest = deepest.min(g);
            if mix > 0.0 {
                for sample in frame.iter_mut() {
                    *sample = (clean(*sample) * g) as f32;
                }
            }
        }
        self.deepest = deepest;
    }
}

// ── EQ ──────────────────────────────────────────────────────────────────

/// One band of a source's EQ.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct EqBandConfig {
    pub enabled: bool,
    /// Its frequency, 20–20 000 Hz: a bell's middle, a shelf's turn.
    pub hz: f32,
    /// How far up or down, −18 to 18 dB.
    pub gain_db: f32,
    /// A bell's width, 0.3 (wide) to 10 (narrow); a shelf's is fixed.
    pub q: f32,
}

impl Default for EqBandConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            hz: 1_000.0,
            gain_db: 0.0,
            q: 1.0,
        }
    }
}

impl EqBandConfig {
    /// # Errors
    /// Returns an error for a value that is not finite or out of its range.
    pub fn validate(&self) -> Result<(), &'static str> {
        in_ranges(
            &[
                (self.hz, 20.0, 20_000.0),
                (self.gain_db, -18.0, 18.0),
                (self.q, 0.3, 10.0),
            ],
            "EQ band: 20–20 000 Hz, −18–18 dB, Q 0.3–10",
        )
    }
}

/// A source's EQ: a low shelf, two bells and a high shelf, in that order.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct EqConfig {
    pub enabled: bool,
    pub bands: [EqBandConfig; 4],
}

impl Default for EqConfig {
    fn default() -> Self {
        let band = |hz, q| EqBandConfig {
            hz,
            q,
            ..EqBandConfig::default()
        };
        Self {
            enabled: false,
            bands: [
                band(150.0, SHELF_Q as f32),
                band(350.0, 1.0),
                band(3_000.0, 1.0),
                band(8_000.0, SHELF_Q as f32),
            ],
        }
    }
}

impl EqConfig {
    /// # Errors
    /// Returns an error for a band out of its range.
    pub fn validate(&self) -> Result<(), &'static str> {
        self.bands.iter().try_for_each(EqBandConfig::validate)
    }

    /// A saved or sent setting with a band out of range: that band as new,
    /// the others kept.
    #[must_use]
    pub fn sanitized(self) -> Self {
        let fresh = Self::default();
        let mut bands = self.bands;
        for (band, new) in bands.iter_mut().zip(fresh.bands) {
            if band.validate().is_err() {
                *band = EqBandConfig { hz: new.hz, q: new.q, ..EqBandConfig::default() };
            }
        }
        Self { bands, ..self }
    }
}

/// A shelf's damping: Butterworth, a slope of 1.
const SHELF_Q: f64 = std::f64::consts::FRAC_1_SQRT_2;

/// How quickly an EQ band's tuning glides to a new setting: 10 ms, as the
/// low cut's.
const EQ_GLIDE_SECS: f64 = 0.01;

/// A band's tuning: the state-variable filter's g and k, and how its
/// outputs mix (Andy Simper's bell and shelves).
#[derive(Debug, Clone, Copy, PartialEq)]
struct BandTuning {
    g: f64,
    k: f64,
    m: [f64; 3],
}

impl BandTuning {
    /// Passes the sound as it is.
    const FLAT: Self = Self { g: 0.1, k: 1.0, m: [1.0, 0.0, 0.0] };

    /// The tuning for band `index` (0 low shelf, 1–2 bells, 3 high shelf).
    fn of(index: usize, band: EqBandConfig) -> Self {
        let hz = f64::from(band.hz.clamp(20.0, 20_000.0)).min(RATE * 0.45);
        let a = 10f64.powf(f64::from(band.gain_db.clamp(-18.0, 18.0)) / 40.0);
        let t = (std::f64::consts::PI * hz / RATE).tan();
        match index {
            0 => Self { g: t / a.sqrt(), k: 1.0 / SHELF_Q, m: [1.0, (a - 1.0) / SHELF_Q, a * a - 1.0] },
            3 => Self {
                g: t * a.sqrt(),
                k: 1.0 / SHELF_Q,
                m: [a * a, (1.0 - a) * a / SHELF_Q, 1.0 - a * a],
            },
            _ => {
                let q = f64::from(band.q.clamp(0.3, 10.0));
                Self { g: t, k: 1.0 / (q * a), m: [1.0, (a * a - 1.0) / (q * a), 0.0] }
            }
        }
    }

    fn glide_to(&mut self, to: &Self, by: f64) {
        self.g += (to.g - self.g) * by;
        self.k += (to.k - self.k) * by;
        for (m, t) in self.m.iter_mut().zip(to.m) {
            *m += (t - *m) * by;
        }
        // Landed: exactly, so a flat band passes the sound bit for bit.
        let close = |a: f64, b: f64| (a - b).abs() < 1e-12;
        if close(self.g, to.g) && close(self.k, to.k) && self.m.iter().zip(to.m).all(|(a, b)| close(*a, b)) {
            *self = *to;
        }
    }
}

#[derive(Debug, Clone)]
pub struct Eq {
    config: EqConfig,
    /// What each band glides to, worked out when the settings change.
    targets: [BandTuning; 4],
    /// Each band's tuning now.
    now: [BandTuning; 4],
    stages: [Svf; 4],
}

impl Default for Eq {
    fn default() -> Self {
        Self {
            config: EqConfig::default(),
            targets: [BandTuning::FLAT; 4],
            now: [BandTuning::FLAT; 4],
            stages: [Svf::default(); 4],
        }
    }
}

impl Eq {
    pub fn configure(&mut self, config: EqConfig) {
        if config == self.config {
            return;
        }
        self.config = config;
        for (i, (target, band)) in self.targets.iter_mut().zip(config.bands).enumerate() {
            // Off, or the whole EQ off: flat, gliding there — the filter runs
            // on, so it is warm when switched back.
            let heard = config.enabled && band.enabled;
            *target = BandTuning::of(i, EqBandConfig { gain_db: if heard { band.gain_db } else { 0.0 }, ..band });
        }
    }

    /// The mixer stalled: what the filters held is stale.
    pub fn clear(&mut self) {
        self.stages = [Svf::default(); 4];
    }

    pub fn process(&mut self, block: &mut [Frame]) {
        // Flat (off, or every band at 0 dB): the sound passes as it is,
        // whatever the filters hold. Settled there, nothing is worked out;
        // leaving it, the filters start afresh — kept, a band switched back
        // on rang with what it heard before — and so the same however the
        // sound comes cut, flatness falling on the same sample.
        if self.now.iter().all(|t| t.m == BandTuning::FLAT.m) {
            if self.now == self.targets {
                return;
            }
            self.stages = [Svf::default(); 4];
        }
        let by = 1.0 - coefficient(EQ_GLIDE_SECS, 1.0 / RATE);
        for frame in block.iter_mut() {
            for (band, now) in self.now.iter_mut().enumerate() {
                now.glide_to(&self.targets[band], by);
                let t = *now;
                for (channel, sample) in frame.iter_mut().enumerate() {
                    let v0 = clean(*sample);
                    let (v1, v2) = self.stages[band].run(channel, v0, t.g, t.k);
                    *sample = (t.m[0] * v0 + t.m[1] * v1 + t.m[2] * v2) as f32;
                }
            }
        }
    }
}

// ── De-esser ────────────────────────────────────────────────────────────

/// A source's de-esser: an "s" over its threshold turns the top down.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct DeEsserConfig {
    pub enabled: bool,
    /// Where it listens, 3000–10 000 Hz. The shelf it turns down is
    /// halfway down half an octave under this: an "s" here loses about 80%
    /// of the reduction, everything above all of it, the voice's presence
    /// (an octave under) under a fifth.
    pub hz: f32,
    /// It acts once the band around its frequency comes over this, dBFS,
    /// −60 to 0 (4:1 over it, eased in over 6 dB).
    pub threshold_db: f32,
    /// How far down it turns the top at most, 0–20 dB.
    pub range_db: f32,
}

impl Default for DeEsserConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            hz: 6_500.0,
            threshold_db: -30.0,
            range_db: 8.0,
        }
    }
}

impl DeEsserConfig {
    /// # Errors
    /// Returns an error for a value that is not finite or out of its range.
    pub fn validate(&self) -> Result<(), &'static str> {
        in_ranges(
            &[
                (self.hz, 3_000.0, 10_000.0),
                (self.threshold_db, -60.0, 0.0),
                (self.range_db, 0.0, 20.0),
            ],
            "De-esser: 3000–10 000 Hz, threshold −60–0 dBFS, range 0–20 dB",
        )
    }
}

/// Its detector's width: about an octave and a half around its frequency.
const DE_ESSER_Q: f64 = 1.5;
/// Over its threshold, the band rises a quarter as much.
const DE_ESSER_RATIO: f64 = 4.0;
/// Eased in over 6 dB around its threshold.
const DE_ESSER_KNEE_DB: f64 = 6.0;
/// It catches an "s" in about a millisecond…
const DE_ESSER_ATTACK_SECS: f64 = 0.001;
/// …and lets go over 60 ms (in dB), before the next vowel is dulled.
const DE_ESSER_RELEASE_SECS: f64 = 0.06;
/// Its detector's peak envelope falls back over 3 ms: it follows an "s",
/// not each of its cycles.
const DE_ESSER_KEY_DECAY_SECS: f64 = 0.003;

/// Started afresh, its detector runs 2 ms before it may act: a band-pass
/// starting from silence rings at its frequency for its first millisecond,
/// and near the threshold that turned the top down for a release (Max5: up
/// to 2.3 dB at 10 kHz, switched on mid-sound).
const DE_ESSER_SETTLE_FRAMES: usize = 96;

/// Its meters hold their peak a second, to be read and set by: an "s" is
/// over in a tenth of that.
const DE_ESSER_METER_HOLD_FRAMES: usize = 48_000;

#[derive(Debug, Clone)]
pub struct DeEsser {
    config: DeEsserConfig,
    /// Listens: a band-pass around its frequency, unity at the middle.
    detector: Svf,
    /// Turns the top down: a high shelf half an octave under its frequency.
    shelf: Svf,
    /// tan(π·f/rate) for the detector and the shelf, gliding to a new
    /// frequency over 10 ms.
    t: Glide,
    shelf_t: Glide,
    /// The band's peak envelope, linear.
    key: f64,
    /// How far down it turns the top now, dB (≥ 0).
    reduction_db: f64,
    /// How much of the de-essed sound is heard, 0–1.
    mix: Glide,
    /// Frames its detector runs yet before it may act (see
    /// [`DE_ESSER_SETTLE_FRAMES`]); and whether it is waking — switched on —
    /// so its bypass fades in after the settling too, as it would have
    /// without it.
    settle_left: usize,
    waking: bool,
    /// The deepest reduction, dB, and the band's loudest, linear — what to
    /// set the threshold by — each held a second (see
    /// [`DE_ESSER_METER_HOLD_FRAMES`]), and how long held.
    deepest_db: f64,
    deepest_for: usize,
    loudest: f64,
    loudest_for: usize,
}

impl Default for DeEsser {
    fn default() -> Self {
        let config = DeEsserConfig::default();
        Self {
            config,
            detector: Svf::default(),
            shelf: Svf::default(),
            t: Glide::at(tan_of(config.hz)),
            shelf_t: Glide::at(tan_of(shelf_hz(config.hz))),
            key: 0.0,
            reduction_db: 0.0,
            mix: Glide::at(0.0),
            settle_left: 0,
            waking: false,
            deepest_db: 0.0,
            deepest_for: 0,
            loudest: 0.0,
            loudest_for: 0,
        }
    }
}

/// A meter's `held` value: `now` if higher, or once held a second.
fn hold(held: &mut f64, held_for: &mut usize, now: f64, frames: usize) {
    *held_for += frames;
    if now >= *held || *held_for >= DE_ESSER_METER_HOLD_FRAMES {
        *held = now;
        *held_for = 0;
    }
}

/// Where a de-esser listening at `hz` has its shelf halfway down.
fn shelf_hz(hz: f32) -> f32 {
    hz * std::f32::consts::FRAC_1_SQRT_2
}

/// tan(π·hz/rate), the filters' tuning for `hz` (kept under 0.45 of the rate).
fn tan_of(hz: f32) -> f64 {
    (std::f64::consts::PI * f64::from(hz.clamp(20.0, 20_000.0)).min(RATE * 0.45) / RATE).tan()
}

impl DeEsser {
    pub fn configure(&mut self, config: DeEsserConfig) {
        self.config = config;
    }

    /// How far down it turns the top now, dB (≥ 0).
    pub fn reduction_db(&self) -> f32 {
        self.deepest_db as f32
    }

    /// How loud the band it listens to was, dBFS.
    pub fn band_db(&self) -> f32 {
        to_db(self.loudest, false) as f32
    }

    /// The mixer stalled: what it heard is long gone. Its detector settles
    /// again before it acts.
    pub fn clear(&mut self) {
        self.detector = Svf::default();
        self.shelf = Svf::default();
        self.key = 0.0;
        self.reduction_db = 0.0;
        self.settle_left = DE_ESSER_SETTLE_FRAMES;
    }

    /// How far it would turn the top down for the band at `key`, dB.
    fn wanted_db(&self, key: f64) -> f64 {
        let half = DE_ESSER_KNEE_DB / 2.0;
        let threshold = f64::from(self.config.threshold_db);
        // Under the knee: nothing, and no logarithm worked out.
        if key < 10f64.powf((threshold - half) / 20.0) {
            return 0.0;
        }
        let over = to_db(key, false) - threshold;
        let slope = 1.0 - 1.0 / DE_ESSER_RATIO;
        let cut = if over >= half {
            over * slope
        } else {
            (over + half).powi(2) / (2.0 * DE_ESSER_KNEE_DB) * slope
        };
        cut.min(f64::from(self.config.range_db))
    }

    pub fn process(&mut self, block: &mut [Frame]) {
        let config = self.config;
        let target_mix = if config.enabled { 1.0 } else { 0.0 };
        let t_to = tan_of(config.hz);
        let shelf_to = tan_of(shelf_hz(config.hz));
        // Bypassed and settled: off, it rests; on, it starts afresh — the
        // same however the sound comes cut, its bypass settling on the same
        // sample.
        if self.mix.rests_at(0.0) && !self.waking {
            if !config.enabled {
                self.deepest_db = 0.0;
                self.loudest = 0.0;
                self.deepest_for = 0;
                self.loudest_for = 0;
                return;
            }
            self.clear();
            self.waking = true;
            self.t = Glide::at(t_to);
            self.shelf_t = Glide::at(shelf_to);
        }
        let decay = coefficient(DE_ESSER_KEY_DECAY_SECS, 1.0 / RATE);
        let attack = coefficient(DE_ESSER_ATTACK_SECS, 1.0 / RATE);
        let release = coefficient(DE_ESSER_RELEASE_SECS, 1.0 / RATE);
        let kd = 1.0 / DE_ESSER_Q;
        let ks = 1.0 / SHELF_Q;
        let mut deepest = 0.0_f64;
        let mut loudest = 0.0_f64;
        for frame in block.iter_mut() {
            let listen = self.t.next(t_to);
            let turn = self.shelf_t.next(shelf_to);
            let dry = [clean(frame[0]), clean(frame[1])];
            let mut peak = 0.0_f64;
            for (channel, v0) in dry.iter().enumerate() {
                let (v1, _) = self.detector.run(channel, *v0, listen, kd);
                peak = peak.max((kd * v1).abs());
            }
            if self.settle_left > 0 {
                self.settle_left -= 1;
            } else {
                self.key = peak.max(self.key * decay);
                self.waking = false;
            }
            if self.key < 1e-30 {
                self.key = 0.0;
            }
            loudest = loudest.max(self.key);
            let wanted = self.wanted_db(self.key);
            let follow = if wanted > self.reduction_db { attack } else { release };
            self.reduction_db = self.reduction_db * follow + wanted * (1.0 - follow);
            if self.reduction_db < 1e-9 {
                self.reduction_db = 0.0;
            }
            let mix = self.mix.next(if self.waking { 0.0 } else { target_mix });
            deepest = deepest.max(self.reduction_db * mix);
            // A high shelf down by the reduction (Simper's): flat, it passes
            // the sound as it is, and runs on to stay warm.
            let (tuning, outs) = if self.reduction_db == 0.0 {
                (turn, [1.0, 0.0, 0.0])
            } else {
                let a = 10f64.powf(-self.reduction_db / 40.0);
                (turn * a.sqrt(), [a * a, ks * (1.0 - a) * a, 1.0 - a * a])
            };
            for (channel, sample) in frame.iter_mut().enumerate() {
                let v0 = dry[channel];
                let (v1, v2) = self.shelf.run(channel, v0, tuning, ks);
                let wet = outs[0] * v0 + outs[1] * v1 + outs[2] * v2;
                if mix > 0.0 {
                    *sample = (v0 + (wet - v0) * mix) as f32;
                }
            }
        }
        hold(&mut self.deepest_db, &mut self.deepest_for, deepest, block.len());
        hold(&mut self.loudest, &mut self.loudest_for, loudest, block.len());
    }
}

// ── Leveler ─────────────────────────────────────────────────────────────

/// A source's leveler: brings each speaker slowly to the same loudness, as a
/// radio station's processor does, so a soft speaker after a loud one needs no
/// hand on the fader. It holds still through pauses, so it never pumps the
/// room's noise up between sentences.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct LevelerConfig {
    pub enabled: bool,
    /// The level it rides the voice to: the average (RMS) level, dBFS,
    /// −40 to −6.
    pub target_db: f32,
    /// The most it turns a soft voice up, 0–24 dB.
    pub max_boost_db: f32,
    /// The most it turns a loud voice down, 0–24 dB.
    pub max_cut_db: f32,
    /// How slowly it moves, 0.5–20 s: about the time it takes to make up most
    /// of a difference. It turns down three times as fast as it turns up.
    pub speed_secs: f32,
    /// Below this average level, dBFS (−80 to −20), it takes the sound for a
    /// pause or the room and holds still.
    pub hold_below_db: f32,
}

impl Default for LevelerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            target_db: -20.0,
            max_boost_db: 12.0,
            max_cut_db: 12.0,
            speed_secs: 3.0,
            hold_below_db: -50.0,
        }
    }
}

impl LevelerConfig {
    /// # Errors
    /// Returns an error for a value that is not finite or out of its range.
    pub fn validate(&self) -> Result<(), &'static str> {
        in_ranges(
            &[
                (self.target_db, -40.0, -6.0),
                (self.max_boost_db, 0.0, 24.0),
                (self.max_cut_db, 0.0, 24.0),
                (self.speed_secs, 0.5, 20.0),
                (self.hold_below_db, -80.0, -20.0),
            ],
            "Leveler values must be finite and within their ranges",
        )
    }
}

/// The time the leveler averages a voice over, in seconds: a few syllables.
const LEVELER_WINDOW_SECS: f64 = 0.4;

/// It measures in windows of a mixer's block, 10 ms, however the sound
/// comes cut: a live bus's 1.3 ms callbacks ride a voice as the mixer does.
#[derive(Debug, Clone)]
pub struct Leveler {
    config: LevelerConfig,
    /// The voice's average power, before the leveler.
    power: f64,
    /// The gain it rides to, dB, as of the last window.
    gain_db: f64,
    /// This window's power so far, summed, and its frames.
    window_power: f64,
    window_frames: usize,
    /// The gain applied, linear, gliding to `gain_db`'s.
    gain: Glide,
}

impl Default for Leveler {
    fn default() -> Self {
        Self {
            config: LevelerConfig::default(),
            power: 0.0,
            gain_db: 0.0,
            window_power: 0.0,
            window_frames: 0,
            gain: Glide::at(1.0),
        }
    }
}

impl Leveler {
    pub fn configure(&mut self, config: LevelerConfig) {
        self.config = config;
    }

    /// The gain it rides to, dB.
    pub fn gain_db(&self) -> f32 {
        self.gain_db as f32
    }

    /// Ride on from `gain_db` (within its limits), as a leveler already
    /// riding this voice: a live bus's callback opened again (its buffer
    /// changed, Live switched on) picks up where the mixer's own leveler
    /// for the strip is, rather than starting at 0 dB and climbing for
    /// seconds. The voice's average is taken as what that gain levels.
    pub fn start_at(&mut self, gain_db: f32) {
        let c = self.config;
        if !c.enabled || !gain_db.is_finite() {
            return;
        }
        let db = f64::from(gain_db).clamp(-f64::from(c.max_cut_db), f64::from(c.max_boost_db));
        self.gain_db = db;
        self.gain = Glide::at(f64::from(db_to_gain(db as f32)));
        self.power = 10f64.powf((f64::from(c.target_db) - db) / 10.0);
    }

    pub fn process(&mut self, block: &mut [Frame]) {
        let mut rest = block;
        while !rest.is_empty() {
            // Up to the end of this window. The part is measured as it came,
            // given the gain already decided, and only then is a finished
            // window's measure taken — for the samples after it: so a window
            // is heard the same however the sound comes cut (applied to the
            // part that finished it, a 1.3 ms call heard it a window early).
            let take = (BLOCK_FRAMES - self.window_frames).min(rest.len());
            let (part, after) = std::mem::take(&mut rest).split_at_mut(take);
            let power = part
                .iter()
                .map(|f| f64::midpoint(clean(f[0]).powi(2), clean(f[1]).powi(2)))
                .sum::<f64>();
            self.apply(part);
            self.window_power += power;
            self.window_frames += take;
            if self.window_frames >= BLOCK_FRAMES {
                self.measured(self.window_power / self.window_frames as f64);
                self.window_power = 0.0;
                self.window_frames = 0;
            }
            rest = after;
        }
    }

    /// A window's average power: where the gain rides to now.
    fn measured(&mut self, block_power: f64) {
        let c = self.config;
        // Only a voice is measured: in a pause the average stays where the
        // voice left it, so the next sentence does not start with a jump.
        let voiced = to_db(block_power, true) > f64::from(c.hold_below_db);
        if voiced {
            if self.power == 0.0 {
                self.power = block_power;
            } else {
                let k = coefficient(LEVELER_WINDOW_SECS, BLOCK_SECS);
                self.power = self.power * k + block_power * (1.0 - k);
            }
        }

        if c.enabled {
            let target = if voiced {
                (f64::from(c.target_db) - to_db(self.power, true))
                    .clamp(-f64::from(c.max_cut_db), f64::from(c.max_boost_db))
            } else {
                // A pause, or only the room: stay where the voice left it.
                self.gain_db
            };
            let speed = f64::from(c.speed_secs.max(0.5));
            // Down faster than up: a speaker leaning in must not blast the
            // room for long; a quiet one can be found more gently.
            let secs = if target < self.gain_db {
                speed / 3.0
            } else {
                speed
            };
            let k = coefficient(secs, BLOCK_SECS);
            self.gain_db = self.gain_db * k + target * (1.0 - k);
            // Limits changed take hold at once, in a pause too.
            self.gain_db = self
                .gain_db
                .clamp(-f64::from(c.max_cut_db), f64::from(c.max_boost_db));
        } else {
            // Switched off: back to unity within this block.
            self.gain_db = 0.0;
        }
    }

    /// The gain to `part`: a straight line from the last window's to this
    /// one's — no step, and no power function per sample.
    // Unity exactly: the sound passes untouched, bit for bit.
    #[allow(clippy::float_cmp)]
    fn apply(&mut self, part: &mut [Frame]) {
        // Switched off: back to unity at once, not at the window's end.
        let to = if !self.config.enabled || self.gain_db == 0.0 {
            1.0
        } else {
            f64::from(db_to_gain(self.gain_db as f32))
        };
        if to == 1.0 && self.gain.rests_at(1.0) {
            return;
        }
        for frame in part.iter_mut() {
            let g = self.gain.next(to);
            for sample in frame.iter_mut() {
                *sample = (clean(*sample) * g) as f32;
            }
        }
    }
}

// ── Compressor ──────────────────────────────────────────────────────────

/// A compressor: above its threshold a voice rises only 1/ratio as much,
/// stereo linked, from the louder channel's peak.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct CompressorConfig {
    pub enabled: bool,
    pub threshold_db: f32,
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub knee_db: f32,
    pub makeup_db: f32,
}

impl Default for CompressorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold_db: -18.0,
            ratio: 3.0,
            attack_ms: 10.0,
            release_ms: 100.0,
            knee_db: 6.0,
            makeup_db: 0.0,
        }
    }
}

impl CompressorConfig {
    /// # Errors
    /// Returns an error for non-finite values or unsupported parameter ranges.
    pub fn validate(&self) -> Result<(), &'static str> {
        in_ranges(
            &[
                (self.threshold_db, -60.0, 0.0),
                (self.ratio, 1.0, 20.0),
                (self.attack_ms, 0.1, 2000.0),
                (self.release_ms, 1.0, 9000.0),
                (self.knee_db, 0.0, 18.0),
                (self.makeup_db, -12.0, 24.0),
            ],
            "Compressor values must be finite and within their supported ranges",
        )
    }

    /// The gain reduction, dB (≤ 0), for a peak `level_db` — the static
    /// curve, with a soft knee `knee_db` wide.
    fn reduction_db(&self, level_db: f64) -> f64 {
        let t = f64::from(self.threshold_db);
        let r = f64::from(self.ratio.max(1.0));
        let w = f64::from(self.knee_db.max(0.0));
        let over = level_db - t;
        let out = if 2.0 * over < -w {
            level_db
        } else if w > 0.0 && 2.0 * over.abs() <= w {
            level_db + (1.0 / r - 1.0) * (over + w / 2.0).powi(2) / (2.0 * w)
        } else {
            t + over / r
        };
        (out - level_db).min(0.0)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Compressor {
    config: CompressorConfig,
    /// The reduction now, dB (≤ 0), smoothed by attack and release.
    envelope_db: f64,
    /// Makeup heard now, dB, gliding to the setting.
    makeup_db: Glide,
    /// How much of the compressed sound is heard, 0–1.
    mix: Glide,
    /// The deepest reduction in the last block, dB (≥ 0).
    deepest_db: f64,
}

impl Compressor {
    pub fn configure(&mut self, config: CompressorConfig) {
        self.config = config;
    }

    /// The deepest reduction in the last block, dB (≥ 0).
    pub fn reduction_db(&self) -> f32 {
        self.deepest_db as f32
    }

    /// Let go at once, as after the mixer stalled: what it was reacting to
    /// is long gone.
    pub fn release(&mut self) {
        self.envelope_db = 0.0;
        self.deepest_db = 0.0;
    }

    pub fn process(&mut self, block: &mut [Frame]) {
        let c = self.config;
        let target_mix = if c.enabled { 1.0 } else { 0.0 };
        if target_mix == 0.0 && self.mix.rests_at(0.0) {
            self.envelope_db = 0.0;
            self.deepest_db = 0.0;
            return;
        }
        let attack = coefficient(f64::from(c.attack_ms) / 1000.0, 1.0 / RATE);
        let release = coefficient(f64::from(c.release_ms) / 1000.0, 1.0 / RATE);
        let makeup_to = f64::from(c.makeup_db);
        let mut deepest = 0.0_f64;
        for frame in block.iter_mut() {
            let peak = clean(frame[0]).abs().max(clean(frame[1]).abs());
            let wanted = c.reduction_db(to_db(peak, false));
            let k = if wanted < self.envelope_db {
                attack
            } else {
                release
            };
            self.envelope_db = self.envelope_db * k + wanted * (1.0 - k);
            deepest = deepest.min(self.envelope_db);
            let makeup = self.makeup_db.next(makeup_to);
            let compressed = f64::from(db_to_gain((self.envelope_db + makeup) as f32));
            // One gain, faded between unity and the compressor's: never more
            // than either, whatever the makeup.
            let mix = self.mix.next(target_mix);
            let g = 1.0 + (compressed - 1.0) * mix;
            for sample in frame.iter_mut() {
                *sample = (clean(*sample) * g) as f32;
            }
        }
        self.deepest_db = -deepest;
    }
}

// ── Limiter ─────────────────────────────────────────────────────────────

/// A bus's limiter: the last thing before its recordings, streams and
/// screens, so a shout, a dropped microphone or a loud clip can never clip
/// them. It looks a little ahead to turn down before a peak arrives.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct LimiterConfig {
    pub enabled: bool,
    /// The highest a sample may go, dBFS, −12 to 0.
    pub ceiling_db: f32,
    /// How quickly it lets go after a peak, 10–1000 ms.
    pub release_ms: f32,
}

impl Default for LimiterConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            ceiling_db: -1.0,
            release_ms: 100.0,
        }
    }
}

impl LimiterConfig {
    /// # Errors
    /// Returns an error for a value that is not finite or out of its range.
    pub fn validate(&self) -> Result<(), &'static str> {
        in_ranges(
            &[
                (self.ceiling_db, -12.0, 0.0),
                (self.release_ms, 10.0, 1000.0),
            ],
            "Limiter: ceiling −12–0 dBFS, release 10–1000 ms",
        )
    }
}

/// How far the limiter looks ahead: 1.5 ms. The bus's sound is that much
/// later, on or off, so switching it never shifts the timing — and 1.5 ms is
/// far inside what anyone hears as out of step with the picture.
pub const LIMITER_LOOKAHEAD: usize = 72;

#[derive(Debug, Clone)]
pub struct Limiter {
    config: LimiterConfig,
    /// The sound waiting its turn.
    delay: [Frame; LIMITER_LOOKAHEAD],
    at: usize,
    /// The gain each sample needs, for the one going out and every one still
    /// waiting: one more than the delay holds. Kept whether on or off, so
    /// switched on it knows at once what is coming.
    needs: [f64; LIMITER_LOOKAHEAD + 1],
    need_at: usize,
    gain: f64,
    /// The ceiling now, linear, gliding over 10 ms to a new setting: a
    /// clamp moved from −1 to −12 dBFS at once jumped half a full scale.
    ceiling: Glide,
    /// How much of the limited sound is heard, 0–1: switching on or off
    /// crossfades over 10 ms between the sound as it comes and limited,
    /// rather than cutting a loud sound down in one sample.
    mix: Glide,
    deepest: f64,
}

impl Default for Limiter {
    fn default() -> Self {
        Self {
            config: LimiterConfig::default(),
            delay: [[0.0; 2]; LIMITER_LOOKAHEAD],
            at: 0,
            needs: [1.0; LIMITER_LOOKAHEAD + 1],
            need_at: 0,
            gain: 1.0,
            ceiling: Glide::at(f64::from(db_to_gain(LimiterConfig::default().ceiling_db))),
            // On from the start, as a bus is made with it on.
            mix: Glide::at(1.0),
            deepest: 1.0,
        }
    }
}

impl Limiter {
    pub fn configure(&mut self, config: LimiterConfig) {
        self.config = config;
    }

    /// The deepest reduction in the last block, dB (≥ 0).
    pub fn reduction_db(&self) -> f32 {
        -(to_db(self.deepest, false) as f32)
    }

    /// Silence: what was waiting dropped, as after the mixer stalled — the
    /// time it held is gone.
    pub fn clear(&mut self) {
        self.delay = [[0.0; 2]; LIMITER_LOOKAHEAD];
        self.needs = [1.0; LIMITER_LOOKAHEAD + 1];
        self.gain = 1.0;
        self.deepest = 1.0;
    }

    pub fn process(&mut self, block: &mut [Frame]) {
        let c = self.config;
        let ceiling_to = f64::from(db_to_gain(c.ceiling_db));
        // Down within the look-ahead, so the gain is there before the peak.
        let attack = coefficient(LIMITER_LOOKAHEAD as f64 / 5.0, 1.0);
        let release = coefficient(f64::from(c.release_ms) / 1000.0, 1.0 / RATE);
        let target = if c.enabled { 1.0 } else { 0.0 };
        let mut deepest = 1.0_f64;
        for frame in block.iter_mut() {
            let ceiling = self.ceiling.next(ceiling_to);
            let input = [clean(frame[0]), clean(frame[1])];
            let peak = input[0].abs().max(input[1].abs());
            let need = if peak > ceiling { ceiling / peak } else { 1.0 };
            let out = self.delay[self.at];
            self.delay[self.at] = [input[0] as f32, input[1] as f32];
            self.at = (self.at + 1) % LIMITER_LOOKAHEAD;
            self.needs[self.need_at] = need;
            self.need_at = (self.need_at + 1) % (LIMITER_LOOKAHEAD + 1);
            let wanted = self.needs.iter().copied().fold(1.0_f64, f64::min);
            let k = if wanted < self.gain { attack } else { release };
            self.gain = self.gain * k + wanted * (1.0 - k);
            let mix = self.mix.next(target);
            if mix > 0.0 {
                deepest = deepest.min(1.0 + (self.gain - 1.0) * mix);
            }
            for (channel, sample) in frame.iter_mut().enumerate() {
                let x = clean(out[channel]);
                // What the smoothing could not catch in time, cut: rare,
                // and never an over once fully on.
                let limited = (x * self.gain).clamp(-ceiling, ceiling);
                *sample = (x + (limited - x) * mix) as f32;
            }
        }
        self.deepest = deepest;
    }
}

// ── Loudness ────────────────────────────────────────────────────────────

/// What a bus's loudness meter reads: LUFS, as streaming platforms and
/// broadcasters measure (ITU-R BS.1770-4, EBU R 128).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, utoipa::ToSchema)]
pub struct LoudnessSnapshot {
    /// Over the last 400 ms, updated every 10 ms.
    pub momentary_lufs: f32,
    /// Over the last 3 s, updated every 10 ms.
    pub short_term_lufs: f32,
    /// Since the meter was last reset, gated as BS.1770 says (silence and
    /// what is far quieter than the rest left out); `None` before there is
    /// anything to measure.
    pub integrated_lufs: Option<f32>,
}

/// The windows, in blocks of 10 ms: momentary 400 ms, short-term 3 s.
const MOMENTARY_BLOCKS: usize = 40;
const SHORT_TERM_BLOCKS: usize = 300;
/// The integrated reading's gating step: 100 ms, so its 400 ms blocks
/// overlap by 75%.
const GATING_STEP_BLOCKS: usize = 10;
/// The absolute gate: blocks no louder than −70 LUFS are left out.
const ABSOLUTE_GATE_LUFS: f64 = -70.0;
/// The integrated reading keeps every gating block, in chunks of this many
/// (about 7 minutes, 32 KB each): a new chunk is the only allocation, small
/// and rare though the mixer is held, and a 12-hour day keeps 3.5 MB.
const GATED_CHUNK: usize = 4_096;

/// BS.1770's K-weighting at 48 kHz: a high shelf for the head, then a
/// high-pass (RLB).
const K_SHELF: Coeffs = Coeffs {
    b0: 1.535_124_859_586_97,
    b1: -2.691_696_189_406_38,
    b2: 1.198_392_810_852_85,
    a1: -1.690_659_293_182_41,
    a2: 0.732_480_774_215_85,
};
const K_HIGHPASS: Coeffs = Coeffs {
    b0: 1.0,
    b1: -2.0,
    b2: 1.0,
    a1: -1.990_047_454_833_98,
    a2: 0.990_072_250_366_21,
};

fn lufs(mean_power: f64) -> f64 {
    -0.691 + to_db(mean_power, true)
}

/// A BS.1770 loudness meter.
#[derive(Debug, Clone)]
pub struct LoudnessMeter {
    shelf: Biquad,
    highpass: Biquad,
    /// The weighted power of the last blocks, newest at `at`; before the
    /// meter has run that long, silence.
    blocks: [f64; SHORT_TERM_BLOCKS],
    at: usize,
    /// Blocks measured since the last reset (up to a gating block's worth),
    /// and since the last gating step.
    measured: usize,
    since_step: usize,
    /// Every gating block past the absolute gate, by its mean power, in
    /// order — gated one by one as BS.1770 says. A histogram of them (0.01
    /// LU bins) misread a crowd of nearly equal blocks at the relative gate
    /// by several LU, however each bin was split.
    gated: Vec<Box<[f64]>>,
    gated_len: usize,
    gated_sum: f64,
    /// The integrated reading, made again as each block is kept.
    integrated: Option<f64>,
}

impl Default for LoudnessMeter {
    fn default() -> Self {
        Self {
            shelf: Biquad::new(K_SHELF),
            highpass: Biquad::new(K_HIGHPASS),
            blocks: [0.0; SHORT_TERM_BLOCKS],
            at: 0,
            measured: 0,
            since_step: 0,
            gated: Vec::new(),
            gated_len: 0,
            gated_sum: 0.0,
            integrated: None,
        }
    }
}

impl LoudnessMeter {
    /// Start every reading again, clearing in place: it is called with the
    /// mixer held.
    pub fn reset(&mut self) {
        self.blocks = [0.0; SHORT_TERM_BLOCKS];
        self.at = 0;
        self.measured = 0;
        self.since_step = 0;
        // The first chunk is kept for the blocks to come.
        self.gated.truncate(1);
        self.gated_len = 0;
        self.gated_sum = 0.0;
        self.integrated = None;
    }

    /// Measure `block` (unchanged).
    pub fn measure(&mut self, block: &[Frame]) {
        let mut sum = 0.0;
        for frame in block {
            for (channel, &sample) in frame.iter().enumerate() {
                let y = self
                    .highpass
                    .run(channel, self.shelf.run(channel, clean(sample)));
                // BS.1770 weights left and right alike: 1.0 each.
                sum += y * y;
            }
        }
        self.push(sum / block.len().max(1) as f64);
    }

    /// As new, the K-weighting filters' memory too: a show loaded.
    pub fn restart(&mut self) {
        self.reset();
        self.shelf = Biquad::new(K_SHELF);
        self.highpass = Biquad::new(K_HIGHPASS);
    }

    /// `blocks` blocks of silence: the time the mixer stalled. Past the
    /// longest window, more silence changes nothing.
    pub fn skip(&mut self, blocks: usize) {
        for _ in 0..blocks.min(SHORT_TERM_BLOCKS + GATING_STEP_BLOCKS) {
            self.push(0.0);
        }
        self.shelf = Biquad::new(K_SHELF);
        self.highpass = Biquad::new(K_HIGHPASS);
    }

    fn push(&mut self, power: f64) {
        self.at = (self.at + 1) % SHORT_TERM_BLOCKS;
        self.blocks[self.at] = power;
        self.measured = (self.measured + 1).min(MOMENTARY_BLOCKS);
        self.since_step += 1;
        // Every 100 ms, once 400 ms have been measured, a gating block.
        if self.since_step >= GATING_STEP_BLOCKS {
            self.since_step = 0;
            if self.measured >= MOMENTARY_BLOCKS {
                self.record(self.window(MOMENTARY_BLOCKS));
            }
        }
    }

    /// Keep a 400 ms gating block of mean power `power` for the integrated
    /// reading, past the absolute gate (−70 LUFS), and read it again.
    fn record(&mut self, power: f64) {
        if lufs(power) <= ABSOLUTE_GATE_LUFS {
            return;
        }
        let (chunk, at) = (self.gated_len / GATED_CHUNK, self.gated_len % GATED_CHUNK);
        if chunk == self.gated.len() {
            self.gated.push(vec![0.0; GATED_CHUNK].into_boxed_slice());
        }
        self.gated[chunk][at] = power;
        self.gated_len += 1;
        self.gated_sum += power;
        // Relative gate: 10 LU under the loudness of all that passed the
        // absolute one — a tenth of their mean power. Each block above it
        // counts: one pass, 0.6 ms at the end of a 12-hour day (measured).
        let gate = self.gated_sum / self.gated_len as f64 / 10.0;
        let (mut count, mut sum) = (0_usize, 0.0_f64);
        let mut left = self.gated_len;
        for chunk in &self.gated {
            for &p in &chunk[..left.min(GATED_CHUNK)] {
                if p > gate {
                    count += 1;
                    sum += p;
                }
            }
            left = left.saturating_sub(GATED_CHUNK);
        }
        self.integrated = (count > 0).then(|| lufs(sum / count as f64));
    }

    /// The mean power over the last `blocks` blocks.
    fn window(&self, blocks: usize) -> f64 {
        (0..blocks)
            .map(|back| self.blocks[(self.at + SHORT_TERM_BLOCKS - back) % SHORT_TERM_BLOCKS])
            .sum::<f64>()
            / blocks as f64
    }

    fn integrated(&self) -> Option<f64> {
        self.integrated
    }

    pub fn snapshot(&self) -> LoudnessSnapshot {
        LoudnessSnapshot {
            momentary_lufs: lufs(self.window(MOMENTARY_BLOCKS)) as f32,
            short_term_lufs: lufs(self.window(SHORT_TERM_BLOCKS)) as f32,
            integrated_lufs: self.integrated().map(|l| l as f32),
        }
    }
}

// ── The Speech preset ───────────────────────────────────────────────────

impl LowCutConfig {
    /// A talk's microphone: rumble and handling noise below 100 Hz out.
    #[must_use]
    pub fn speech() -> Self {
        Self {
            enabled: true,
            hz: 100.0,
            slope_db: 24,
        }
    }
}

impl LevelerConfig {
    /// Each speaker to −20 dB average, up to 12 dB either way.
    #[must_use]
    pub fn speech() -> Self {
        Self {
            enabled: true,
            ..Self::default()
        }
    }
}

impl CompressorConfig {
    /// Gentle compression after the leveler: peaks evened, 3 dB made up.
    #[must_use]
    pub fn speech() -> Self {
        Self {
            enabled: true,
            threshold_db: -18.0,
            ratio: 3.0,
            attack_ms: 5.0,
            release_ms: 150.0,
            knee_db: 6.0,
            makeup_db: 3.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `secs` of a stereo sine at `hz`, peak `amplitude`, in blocks.
    fn sine(hz: f64, amplitude: f64, secs: f64) -> Vec<Vec<Frame>> {
        let frames = (secs * RATE) as usize;
        let samples: Vec<Frame> = (0..frames)
            .map(|i| {
                let v = (amplitude * (std::f64::consts::TAU * hz * i as f64 / RATE).sin()) as f32;
                [v, v]
            })
            .collect();
        samples
            .chunks(BLOCK_FRAMES)
            .map(<[Frame]>::to_vec)
            .collect()
    }

    fn peak(blocks: &[Vec<Frame>]) -> f64 {
        blocks
            .iter()
            .flatten()
            .map(|f| f64::from(f[0].abs().max(f[1].abs())))
            .fold(0.0, f64::max)
    }

    fn rms_db(blocks: &[Vec<Frame>]) -> f64 {
        let (sum, n) = blocks.iter().flatten().fold((0.0, 0usize), |(s, n), f| {
            (s + f64::from(f[0]).powi(2), n + 1)
        });
        to_db(sum / n as f64, true)
    }

    /// Run `blocks` through `process`, returning what comes out.
    fn run(blocks: &[Vec<Frame>], mut process: impl FnMut(&mut [Frame])) -> Vec<Vec<Frame>> {
        blocks
            .iter()
            .map(|b| {
                let mut b = b.clone();
                process(&mut b);
                b
            })
            .collect()
    }

    #[test]
    fn the_low_cut_takes_out_rumble_and_leaves_the_voice() {
        for (slope, at_50) in [(12_u8, -11.0), (24, -23.0)] {
            let mut low_cut = LowCut::default();
            low_cut.configure(LowCutConfig {
                enabled: true,
                hz: 100.0,
                slope_db: slope,
            });
            let hum = run(&sine(50.0, 0.5, 2.0), |b| low_cut.process(b));
            let voice = run(&sine(1000.0, 0.5, 2.0), |b| low_cut.process(b));
            // Settled: the last second.
            let hum_db = to_db(peak(&hum[100..]) / 0.5, false);
            let voice_db = to_db(peak(&voice[100..]) / 0.5, false);
            // An octave under 100 Hz: about 12 or 24 dB down (Butterworth's
            // −3 dB point is at 100 Hz, so a little less below it).
            assert!(
                hum_db < at_50 + 1.0 && hum_db > at_50 - 3.0,
                "{slope}: {hum_db}"
            );
            assert!(voice_db.abs() < 0.1, "{slope}: voice {voice_db}");
        }
    }

    #[test]
    fn a_low_cut_switched_on_or_off_fades_rather_than_clicks() {
        let mut low_cut = LowCut::default();
        let tone = sine(40.0, 0.5, 0.5);
        let mut config = LowCutConfig {
            enabled: false,
            ..LowCutConfig::default()
        };
        let mut out = Vec::new();
        for (i, block) in tone.iter().enumerate() {
            if i == 20 {
                config.enabled = true;
                low_cut.configure(config);
            }
            let mut b = block.clone();
            low_cut.process(&mut b);
            out.extend(b);
        }
        // No sample jumps further than the tone itself moves in a sample
        // plus the fade's share of it.
        let biggest_step = out
            .windows(2)
            .map(|w| (w[1][0] - w[0][0]).abs())
            .fold(0.0_f32, f32::max);
        assert!(biggest_step < 0.01, "step {biggest_step}");
    }

    /// Speech-like: a 300 Hz tone in 1.5 s phrases with 0.5 s pauses of
    /// near-silence, at `level_db` RMS while talking.
    fn talk(level_db: f64, secs: f64) -> Vec<Vec<Frame>> {
        let amplitude = f64::from(db_to_gain(level_db as f32)) * std::f64::consts::SQRT_2;
        let frames = (secs * RATE) as usize;
        let samples: Vec<Frame> = (0..frames)
            .map(|i| {
                let t = i as f64 / RATE;
                let talking = t % 2.0 < 1.5;
                let v = if talking {
                    amplitude * (std::f64::consts::TAU * 300.0 * t).sin()
                } else {
                    0.0005 * (std::f64::consts::TAU * 1234.5 * t).sin()
                };
                [v as f32, v as f32]
            })
            .collect();
        samples
            .chunks(BLOCK_FRAMES)
            .map(<[Frame]>::to_vec)
            .collect()
    }

    /// `input` through `process` in calls of `size` frames, cut also at
    /// each of `changes` (where a setting changes, between calls); the
    /// frame each call starts at, to it.
    fn cut_run(
        input: &[Frame],
        size: usize,
        changes: &[usize],
        mut process: impl FnMut(&mut [Frame], usize),
    ) -> Vec<Frame> {
        let mut out = input.to_vec();
        let mut at = 0;
        while at < out.len() {
            let change = changes.iter().copied().filter(|&c| c > at).min();
            let end = (at + size).min(out.len()).min(change.unwrap_or(usize::MAX));
            process(&mut out[at..end], at);
            at = end;
        }
        out
    }

    fn largest_difference(a: &[Frame], b: &[Frame]) -> f32 {
        a.iter()
            .zip(b)
            .map(|(x, y)| (x[0] - y[0]).abs().max((x[1] - y[1]).abs()))
            .fold(0.0, f32::max)
    }

    /// The low cut, compressor and limiter sound the same fed a live bus's
    /// 64-frame callbacks (or an odd 37) as the mixer's 10 ms blocks, their
    /// settings switched and changed on the way: each glide takes 10 ms
    /// either way. Gliding over a call, a bypass in a 1.3 ms callback was
    /// eight times as steep: a click.
    #[test]
    fn small_blocks_sound_as_the_mixers_blocks_do() {
        let input: Vec<Frame> = (0..48_000)
            .map(|i| {
                let t = f64::from(i) / RATE;
                let a = if t < 0.5 { 0.3 } else { 1.8 };
                let v = a * (std::f64::consts::TAU * 70.0 * t).sin();
                [v as f32, (v * 0.5) as f32]
            })
            .collect();
        let changes = [4_800, 14_400, 24_000, 33_600];
        // The same with an "s" on it, for the de-esser to hear.
        let hissed: Vec<Frame> = input
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let s = (0.2 * (std::f64::consts::TAU * 7_000.0 * i as f64 / RATE).sin()) as f32;
                [f[0] * 0.1 + s, f[1] * 0.1 + s]
            })
            .collect();
        let low_cut = |size| {
            let mut f = LowCut::default();
            let mut c = LowCutConfig::default();
            cut_run(&input, size, &changes, |b, at| {
                match at {
                    4_800 => c.enabled = true,
                    14_400 => c.slope_db = 12,
                    24_000 => c.hz = 150.0,
                    33_600 => c.enabled = false,
                    _ => {}
                }
                f.configure(c);
                f.process(b);
            })
        };
        let compressor = |size| {
            let mut f = Compressor::default();
            let mut c = CompressorConfig {
                threshold_db: -20.0,
                ratio: 4.0,
                ..CompressorConfig::default()
            };
            cut_run(&input, size, &changes, |b, at| {
                match at {
                    4_800 => c.enabled = true,
                    14_400 => c.makeup_db = 6.0,
                    24_000 => c.makeup_db = -3.0,
                    33_600 => c.enabled = false,
                    _ => {}
                }
                f.configure(c);
                f.process(b);
            })
        };
        let limiter = |size| {
            let mut f = Limiter::default();
            let mut c = LimiterConfig::default();
            cut_run(&input, size, &changes, |b, at| {
                match at {
                    4_800 => c.ceiling_db = -6.0,
                    14_400 => c.enabled = false,
                    24_000 => c.enabled = true,
                    33_600 => c.ceiling_db = -1.0,
                    _ => {}
                }
                f.configure(c);
                f.process(b);
            })
        };
        let gate = |size| {
            let mut f = Gate::default();
            // The first half (0.3, −10.5 dBFS) under it: closed there.
            let mut c = GateConfig {
                threshold_db: -10.0,
                ..GateConfig::default()
            };
            cut_run(&input, size, &changes, |b, at| {
                match at {
                    4_800 => c.enabled = true,
                    14_400 => c.range_db = 25.0,
                    24_000 => c.hold_ms = 0.0,
                    33_600 => c.enabled = false,
                    _ => {}
                }
                f.configure(c);
                f.process(b);
            })
        };
        let eq = |size| {
            let mut f = Eq::default();
            let mut c = EqConfig::default();
            cut_run(&input, size, &changes, |b, at| {
                match at {
                    4_800 => {
                        c.enabled = true;
                        c.bands[1].gain_db = 9.0;
                    }
                    14_400 => c.bands[0].gain_db = -6.0,
                    24_000 => c.bands[1].hz = 120.0,
                    33_600 => c.enabled = false,
                    _ => {}
                }
                f.configure(c);
                f.process(b);
            })
        };
        let de_esser = |size| {
            let mut f = DeEsser::default();
            let mut c = DeEsserConfig::default();
            cut_run(&hissed, size, &changes, |b, at| {
                match at {
                    4_800 => c.enabled = true,
                    14_400 => c.threshold_db = -40.0,
                    24_000 => c.hz = 4_000.0,
                    33_600 => c.enabled = false,
                    _ => {}
                }
                f.configure(c);
                f.process(b);
            })
        };
        {
            let blocks = de_esser(BLOCK_FRAMES);
            assert!(largest_difference(&blocks, &hissed) > 0.01, "the de-esser did something");
            for size in [64, 37] {
                let d = largest_difference(&de_esser(size), &blocks);
                assert!(d < 1e-6, "the de-esser in {size}-frame calls differs by {d}");
            }
        }
        for (name, run) in [
            ("low cut", &low_cut as &dyn Fn(usize) -> Vec<Frame>),
            ("gate", &gate),
            ("eq", &eq),
            ("compressor", &compressor),
            ("limiter", &limiter),
        ] {
            let blocks = run(BLOCK_FRAMES);
            assert!(largest_difference(&blocks, &input) > 0.01, "{name} did something");
            for size in [64, 37] {
                let d = largest_difference(&run(size), &blocks);
                assert!(d < 1e-6, "{name} in {size}-frame calls differs by {d}");
            }
        }
    }

    /// The leveler rides a voice the same fed 64- or 37-frame calls as 10 ms
    /// blocks: it measures in 10 ms windows whatever the call, so its gain is
    /// the same at every 10 ms, and it brings the voice to the same level.
    /// Measured per call, a 1.3 ms call counted as 10 ms and it rode seven
    /// times too fast.
    #[test]
    fn the_leveler_rides_alike_in_small_blocks() {
        let speech: Vec<Frame> = talk(-30.0, 12.0).concat();
        let ride = |size: usize| {
            let mut leveler = Leveler::default();
            leveler.configure(LevelerConfig::speech());
            let mut gains = std::collections::BTreeMap::new();
            let out = cut_run(&speech, size, &[], |b, at| {
                leveler.process(b);
                gains.insert(at + b.len(), leveler.gain_db());
            });
            (out, gains)
        };
        let (blocks, by_block) = ride(BLOCK_FRAMES);
        for size in [64, 37] {
            let (out, by_call) = ride(size);
            let d = largest_difference(&out, &blocks);
            assert!(d < 1e-6, "{size}-frame calls differ by {d}");
            let mut compared = 0;
            for (at, gain) in &by_call {
                if let Some(want) = by_block.get(at) {
                    assert!((gain - want).abs() < 1e-9, "{size}: at {at} {gain} not {want}");
                    compared += 1;
                }
            }
            assert!(compared >= 25, "{size}: {compared}");
            // The last phrase, at the same level.
            let tail = |o: &[Frame]| rms_db(&[o[500_000..560_000].to_vec()]);
            assert!(
                (tail(&out) - tail(&blocks)).abs() < 0.1,
                "{size}: {} not {}",
                tail(&out),
                tail(&blocks)
            );
        }
    }

    /// A leveler started at a gain applies it at once and rides on from
    /// there, not from 0 dB; within its limits; switched off, it stays at
    /// unity.
    #[test]
    fn a_leveler_started_at_a_gain_rides_on_from_it() {
        let mut leveler = Leveler::default();
        leveler.configure(LevelerConfig::speech());
        leveler.start_at(6.0);
        assert!((leveler.gain_db() - 6.0).abs() < 1e-6);
        let mut block = vec![[0.1_f32, 0.1]; BLOCK_FRAMES];
        leveler.process(&mut block);
        let want = 0.1 * db_to_gain(6.0);
        assert!((block[0][0] - want).abs() < 1e-4, "{} not {want}", block[0][0]);
        leveler.start_at(99.0);
        let most = LevelerConfig::speech().max_boost_db;
        assert!((leveler.gain_db() - most).abs() < 1e-6, "within its limits");
        let mut off = Leveler::default();
        off.start_at(6.0);
        assert!(off.gain_db().abs() < 1e-6, "switched off: unity");
    }

    /// A tone at `db` peak for `secs`, both sides.
    fn tone(db: f32, secs: f64) -> Vec<Frame> {
        let a = f64::from(db_to_gain(db));
        (0..(secs * RATE) as usize)
            .map(|i| {
                let v = (a * (std::f64::consts::TAU * 440.0 * i as f64 / RATE).sin()) as f32;
                [v, v]
            })
            .collect()
    }

    fn peak_of(frames: &[Frame]) -> f32 {
        frames.iter().map(|f| f[0].abs().max(f[1].abs())).fold(0.0, f32::max)
    }

    fn gated(config: GateConfig, sound: &[Frame]) -> Vec<Frame> {
        let mut gate = Gate::default();
        gate.configure(config);
        cut_run(sound, BLOCK_FRAMES, &[], |b, _| gate.process(b))
    }

    fn speech_gate() -> GateConfig {
        GateConfig {
            enabled: true,
            threshold_db: -40.0,
            range_db: 15.0,
            attack_ms: 2.0,
            hold_ms: 50.0,
            release_ms: 100.0,
        }
    }

    /// A voice over the threshold passes; the pause after it is turned down
    /// by the range — not cut off — once the hold and release have passed.
    #[test]
    fn a_gate_turns_the_pause_down_by_its_range() {
        let sound = [tone(-20.0, 0.5), tone(-60.0, 1.0)].concat();
        let out = gated(speech_gate(), &sound);
        let voice = peak_of(&out[12_000..24_000]);
        assert!((voice - db_to_gain(-20.0)).abs() < 0.005, "the voice passes: {voice}");
        let pause = peak_of(&out[out.len() - 12_000..]);
        let want = db_to_gain(-60.0 - 15.0);
        assert!((pause / want - 1.0).abs() < 0.06, "the pause 15 dB down: {pause} not {want}");
    }

    /// Its release is in dB: a whole 30 dB range is all but reached a second
    /// into a pause (in linear gain it was only 26 dB down then).
    #[test]
    fn a_gates_release_turns_a_pause_down_in_db() {
        let config = GateConfig {
            enabled: true,
            threshold_db: -40.0,
            range_db: 30.0,
            hold_ms: 0.0,
            release_ms: 250.0,
            ..GateConfig::default()
        };
        let sound = [tone(-20.0, 0.3), tone(-60.0, 1.1)].concat();
        let out = gated(config, &sound);
        // 1 s after the voice stopped (and its 8 ms key fell away).
        let at = 14_400 + 48_000;
        let down = to_db(f64::from(peak_of(&out[at..at + 960]) / db_to_gain(-60.0)), false);
        assert!(down < -29.0, "a second in: {down:.1} dB");
    }

    /// Fallen under its threshold, it stays open for its hold, then closes.
    #[test]
    fn a_gate_holds_then_closes() {
        let sound = [tone(-20.0, 0.2), tone(-60.0, 0.6)].concat();
        let out = gated(speech_gate(), &sound);
        let at = |ms: usize| peak_of(&out[9_600 + ms * 48..9_600 + (ms + 10) * 48]);
        let open = db_to_gain(-60.0);
        assert!((at(30) / open - 1.0).abs() < 0.05, "held open 30 ms in: {}", at(30));
        assert!(at(450) < open * 0.25, "closed later: {}", at(450));
    }

    /// A voice hovering at the threshold — a dB over, a dB under, turn by
    /// turn — keeps it open: it closes only 3 dB under.
    #[test]
    fn a_voice_at_the_threshold_does_not_chatter() {
        let mut config = speech_gate();
        config.hold_ms = 0.0;
        let sound: Vec<Frame> = (0..20).flat_map(|i| tone(if i % 2 == 0 { -39.0 } else { -41.0 }, 0.02)).collect();
        let out = gated(config, &sound);
        let low = db_to_gain(-41.0);
        let quietest = (1..20).step_by(2).map(|i| peak_of(&out[i * 960 + 480..(i + 1) * 960])).fold(1.0, f32::min);
        assert!(quietest > low * 0.95, "never closed: {quietest} against {low}");
    }

    /// Its range changed while it is closed — at a waveform's peak, with the
    /// quickest attack — glides: no click.
    #[test]
    fn changing_the_gates_range_while_closed_glides() {
        // 100 Hz peaking at 0.2 (−14 dBFS), under a −10 dBFS threshold:
        // closed. A peak every 480 frames, at 120 in.
        let sound: Vec<Frame> = (0..48_000)
            .map(|i| {
                let v = (0.2 * (std::f64::consts::TAU * 100.0 * f64::from(i) / RATE).sin()) as f32;
                [v, v]
            })
            .collect();
        let mut gate = Gate::default();
        let mut config = GateConfig {
            enabled: true,
            threshold_db: -10.0,
            range_db: 30.0,
            attack_ms: 0.1,
            ..GateConfig::default()
        };
        let at_peak = 40 * 480 + 120;
        let out = cut_run(&sound, BLOCK_FRAMES, &[at_peak], |b, at| {
            if at == at_peak {
                config.range_db = 0.0;
            }
            gate.configure(config);
            gate.process(b);
        });
        let step = out[at_peak - 4..at_peak + 600]
            .windows(2)
            .map(|w| (w[1][0] - w[0][0]).abs())
            .fold(0.0, f32::max);
        assert!(step < 0.004, "a step of {step}");
    }

    /// Switched on while the room is quiet, it fades down; switched off, it
    /// fades back up: no step.
    #[test]
    fn switching_the_gate_fades_rather_than_clicks() {
        // Under its threshold, loud enough that a step would show.
        let sound = tone(-45.0, 1.0);
        let mut gate = Gate::default();
        let mut config = speech_gate();
        config.range_db = 30.0;
        config.enabled = false;
        let mut out = Vec::new();
        for (i, block) in sound.chunks(BLOCK_FRAMES).enumerate() {
            // Not where the tone crosses nought (every 30 blocks of 440 Hz):
            // a step there would not show.
            if i == 31 {
                config.enabled = true;
            }
            if i == 71 {
                config.enabled = false;
            }
            gate.configure(config);
            let mut b = block.to_vec();
            gate.process(&mut b);
            out.extend(b);
        }
        let step = out.windows(2).map(|w| (w[1][0] - w[0][0]).abs()).fold(0.0, f32::max);
        assert!(step < 0.001, "a step of {step}");
    }

    /// `hz`'s gain through `eq`, settled, dB.
    fn gain_through(eq: &mut Eq, hz: f64) -> f64 {
        let sound: Vec<Frame> = (0..24_000)
            .map(|i| {
                let v = (0.1 * (std::f64::consts::TAU * hz * f64::from(i) / RATE).sin()) as f32;
                [v, v]
            })
            .collect();
        let out = cut_run(&sound, BLOCK_FRAMES, &[], |b, _| eq.process(b));
        to_db(f64::from(peak_of(&out[12_000..])) / f64::from(peak_of(&sound[12_000..])), false)
    }

    fn eq_with(index: usize, hz: f32, gain_db: f32, q: f32) -> Eq {
        let mut config = EqConfig {
            enabled: true,
            ..EqConfig::default()
        };
        config.bands[index] = EqBandConfig { enabled: true, hz, gain_db, q };
        let mut eq = Eq::default();
        eq.configure(config);
        eq
    }

    /// A bell lifts or cuts its own frequency by its gain and leaves those
    /// far from it; a shelf lifts what is past its turn by its gain.
    #[test]
    fn the_eqs_bands_shape_as_set() {
        assert!((gain_through(&mut eq_with(1, 1_000.0, 6.0, 1.0), 1_000.0) - 6.0).abs() < 0.2);
        assert!((gain_through(&mut eq_with(2, 3_000.0, -9.0, 2.0), 3_000.0) + 9.0).abs() < 0.2);
        assert!(gain_through(&mut eq_with(1, 1_000.0, 6.0, 1.0), 100.0).abs() < 0.3, "far from it");
        assert!((gain_through(&mut eq_with(0, 300.0, -6.0, 0.7), 30.0) + 6.0).abs() < 0.3, "low shelf, under it");
        assert!(gain_through(&mut eq_with(0, 300.0, -6.0, 0.7), 5_000.0).abs() < 0.3, "low shelf, over it");
        assert!((gain_through(&mut eq_with(3, 4_000.0, 6.0, 0.7), 16_000.0) - 6.0).abs() < 0.4, "high shelf");
        assert!(gain_through(&mut eq_with(3, 4_000.0, 6.0, 0.7), 200.0).abs() < 0.3);
    }

    /// Flat, or off, the EQ passes the sound bit for bit.
    #[test]
    fn a_flat_or_off_eq_passes_the_sound_untouched() {
        let sound = tone(-10.0, 0.5);
        for config in [EqConfig::default(), EqConfig { enabled: true, ..EqConfig::default() }] {
            let mut eq = Eq::default();
            eq.configure(config);
            let out = cut_run(&sound, BLOCK_FRAMES, &[], |b, _| eq.process(b));
            assert_eq!(out, sound, "{config:?}");
        }
        // Lifted, then set flat: untouched again once it has glided there.
        let mut eq = eq_with(1, 1_000.0, 9.0, 1.0);
        let mut config = eq.config;
        let out = cut_run(&sound, BLOCK_FRAMES, &[4_800], |b, at| {
            if at == 4_800 {
                config.bands[1].gain_db = 0.0;
            }
            eq.configure(config);
            eq.process(b);
        });
        assert_ne!(out[..4_800], sound[..4_800], "lifted at first");
        assert_eq!(out[19_200..], sound[19_200..], "flat again: untouched");
    }

    /// A bell dragged across octaves while a loud tone plays, or switched
    /// off, makes no click: its whole tuning glides.
    #[test]
    fn moving_an_eq_band_glides() {
        let sound = tone(-6.0, 1.0);
        let mut eq = eq_with(1, 200.0, 12.0, 4.0);
        let mut config = eq.config;
        let changes = [9_600, 19_200, 28_800];
        let out = cut_run(&sound, BLOCK_FRAMES, &changes, |b, at| {
            match at {
                9_600 => config.bands[1].hz = 2_000.0,
                19_200 => config.bands[1].gain_db = -12.0,
                28_800 => config.enabled = false,
                _ => {}
            }
            eq.configure(config);
            eq.process(b);
        });
        // The second difference: a 440 Hz tone, lifted up to 12 dB as the
        // bell passes it, bends 0.007 a sample at most; a jump of a hundredth
        // or more would show here.
        let bend = out
            .windows(3)
            .map(|w| (w[2][0] - 2.0 * w[1][0] + w[0][0]).abs())
            .fold(0.0, f32::max);
        assert!(bend < 0.012, "a bend of {bend}");
    }

    /// Switched off after a loud sound, left resting through silence, then
    /// switched back on: it does not ring with what it heard before.
    #[test]
    fn an_eq_woken_from_rest_does_not_ring_with_the_past() {
        let mut eq = eq_with(1, 100.0, 18.0, 10.0);
        let mut config = eq.config;
        let mut loud: Vec<Frame> = (0..24_000)
            .map(|i| {
                let v = (0.1 * (std::f64::consts::TAU * 100.0 * f64::from(i) / RATE).sin()) as f32;
                [v, v]
            })
            .collect();
        for b in loud.chunks_mut(BLOCK_FRAMES) {
            eq.process(b);
        }
        config.enabled = false;
        eq.configure(config);
        let mut quiet = vec![[0.0_f32; 2]; 24_000];
        for b in quiet.chunks_mut(BLOCK_FRAMES) {
            eq.process(b);
        }
        config.enabled = true;
        eq.configure(config);
        let mut woken = vec![[0.0_f32; 2]; 4_800];
        for b in woken.chunks_mut(BLOCK_FRAMES) {
            eq.process(b);
        }
        assert!(peak_of(&woken) < 1e-6, "rang at {}", peak_of(&woken));
    }

    fn tone_at(hz: f64, db: f32, secs: f64) -> Vec<Frame> {
        let a = f64::from(db_to_gain(db));
        (0..(secs * RATE) as usize)
            .map(|i| {
                let v = (a * (std::f64::consts::TAU * hz * i as f64 / RATE).sin()) as f32;
                [v, v]
            })
            .collect()
    }

    fn de_essed(config: DeEsserConfig, sound: &[Frame]) -> (Vec<Frame>, DeEsser) {
        let mut d = DeEsser::default();
        d.configure(config);
        let out = cut_run(sound, BLOCK_FRAMES, &[], |b, _| d.process(b));
        (out, d)
    }

    fn de_esser_at(threshold_db: f32) -> DeEsserConfig {
        DeEsserConfig {
            enabled: true,
            threshold_db,
            ..DeEsserConfig::default()
        }
    }

    /// An "s" well over its threshold is turned down by about its range
    /// (8 dB): most of it at the frequency it listens at, all of it above.
    #[test]
    fn a_de_esser_turns_an_s_down_by_its_range() {
        // (The detector hears 10 kHz about 6 dB down: −35 is well under both.)
        for (hz, least, most) in [(6_500.0, 6.0, 7.0), (10_000.0, 7.4, 8.1)] {
            let s = tone_at(hz, -10.0, 0.5);
            let (out, d) = de_essed(de_esser_at(-35.0), &s);
            let down = -to_db(f64::from(peak_of(&out[12_000..]) / peak_of(&s[12_000..])), false);
            assert!((least..most).contains(&down), "{hz} Hz down by {down:.2} dB");
            assert!((d.reduction_db() - 8.0).abs() < 0.05, "meter {}", d.reduction_db());
        }
        let (_, d) = de_essed(de_esser_at(-35.0), &tone_at(6_500.0, -10.0, 0.5));
        assert!((d.band_db() + 10.0).abs() < 0.5, "band at its frequency {}", d.band_db());
    }

    /// The voice under it, as loud as the "s", is not touched: its
    /// detector hears 1 kHz 20 dB down, under the threshold.
    #[test]
    fn a_de_esser_leaves_the_voice_under_it() {
        let voice = tone_at(1_000.0, -10.0, 0.5);
        let (out, d) = de_essed(de_esser_at(-25.0), &voice);
        assert_eq!(out, voice, "untouched, bit for bit");
        assert!(d.reduction_db() < 0.01);
    }

    /// After an "s", it lets go over its 60 ms: the next vowel's top is
    /// back within a few tenths of a second.
    #[test]
    fn a_de_esser_lets_go_after_an_s() {
        let sound = [tone_at(8_000.0, -10.0, 0.3), tone_at(8_000.0, -40.0, 0.4)].concat();
        let (out, _) = de_essed(de_esser_at(-25.0), &sound);
        let quiet = &sound[sound.len() - 4_800..];
        let after = &out[out.len() - 4_800..];
        let down = -to_db(f64::from(peak_of(after) / peak_of(quiet)), false);
        assert!(down < 0.1, "still down {down:.2} dB");
    }

    /// Its meters hold an "s" a second, to read and set the threshold by,
    /// then show what is there now.
    #[test]
    fn a_de_essers_meters_hold_a_second() {
        let mut s = [tone_at(8_000.0, -10.0, 0.1), tone_at(8_000.0, -40.0, 1.2)].concat();
        let mut d = DeEsser::default();
        d.configure(de_esser_at(-25.0));
        let mut held = (0.0_f32, 0.0_f32);
        for (i, b) in s.chunks_mut(BLOCK_FRAMES).enumerate() {
            d.process(b);
            if i == 40 {
                // 0.3 s after the "s".
                held = (d.reduction_db(), d.band_db());
            }
        }
        assert!(held.0 > 7.0 && held.1 > -14.0, "held: {held:?}");
        assert!(d.reduction_db() < 0.1 && d.band_db() < -35.0, "now: {} {}", d.reduction_db(), d.band_db());
    }

    /// Switched on mid-sound, a little under its threshold: no dip — its
    /// detector settles before it acts (from silence, a band-pass rings).
    #[test]
    fn a_de_esser_switched_on_near_its_threshold_does_not_dip() {
        // A low voice, 200 Hz at −10 dBFS, switched on at its peak: the band
        // hears it near −44, the threshold at −40 — but a band-pass started
        // from silence on a sample that loud rings well over it.
        let voice: Vec<Frame> = (0..24_000)
            .map(|i| {
                let v = (0.316 * (std::f64::consts::TAU * 200.0 * f64::from(i) / RATE).cos()) as f32;
                [v, v]
            })
            .collect();
        let mut d = DeEsser::default();
        let mut c = de_esser_at(-40.0);
        c.enabled = false;
        let mut deepest = 0.0_f32;
        let out = cut_run(&voice, BLOCK_FRAMES, &[9_600], |b, at| {
            if at == 9_600 {
                c.enabled = true;
            }
            d.configure(c);
            d.process(b);
            deepest = deepest.max(d.reduction_db());
        });
        assert!(deepest < 0.05, "dipped {deepest:.2} dB");
        let worst = largest_difference(&out, &voice);
        assert!(worst < 1e-3, "changed by {worst}");
    }

    /// Switched on or off while an "s" is being turned down, it glides
    /// there over 10 ms: no millisecond is more than a dB from the last.
    #[test]
    fn switching_a_de_esser_glides() {
        let s = tone_at(10_000.0, -10.0, 0.6);
        let mut d = DeEsser::default();
        let mut c = de_esser_at(-25.0);
        c.enabled = false;
        let out = cut_run(&s, BLOCK_FRAMES, &[9_600, 19_200], |b, at| {
            match at {
                9_600 => c.enabled = true,
                19_200 => c.enabled = false,
                _ => {}
            }
            d.configure(c);
            d.process(b);
        });
        let ms: Vec<f64> = out
            .chunks(48)
            .zip(s.chunks(48))
            .map(|(o, i)| to_db(f64::from(peak_of(o) / peak_of(i)), false))
            .collect();
        let step = ms.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f64::max);
        assert!(step < 1.0, "a millisecond's step of {step:.2} dB");
        assert!(ms[300] < -6.0, "on, it was down: {:.1}", ms[300]);
    }

    /// Off long enough to rest, then on again: it starts afresh, not
    /// turning down a quiet top with what it last heard.
    #[test]
    fn a_de_esser_woken_from_rest_starts_afresh() {
        let mut d = DeEsser::default();
        let mut c = de_esser_at(-25.0);
        d.configure(c);
        let mut loud = tone_at(10_000.0, -10.0, 0.3);
        for b in loud.chunks_mut(BLOCK_FRAMES) {
            d.process(b);
        }
        c.enabled = false;
        d.configure(c);
        let mut gap = vec![[0.0_f32; 2]; 4_800];
        for b in gap.chunks_mut(BLOCK_FRAMES) {
            d.process(b);
        }
        c.enabled = true;
        d.configure(c);
        let quiet = tone_at(10_000.0, -50.0, 0.1);
        let out = cut_run(&quiet, BLOCK_FRAMES, &[], |b, _| d.process(b));
        assert_eq!(out, quiet, "nothing turned down");
    }

    /// Bypassed briefly and back, at any point, the EQ and the de-esser
    /// sound the same in small calls as in the mixer's blocks: whether they
    /// start afresh does not hang on where the calls fell.
    #[test]
    fn a_short_bypass_sounds_the_same_in_any_calls() {
        let hissed: Vec<Frame> = (0..48_000)
            .map(|i| {
                let t = f64::from(i) / RATE;
                let v = 0.3 * (std::f64::consts::TAU * 150.0 * t).sin()
                    + 0.2 * (std::f64::consts::TAU * 7_000.0 * t).sin();
                [v as f32, (v * 0.7) as f32]
            })
            .collect();
        for back in (9_600 + 400..9_600 + 700).step_by(37).chain((9_600 + 12_000..9_600 + 16_000).step_by(250)) {
            let changes = [4_800, 9_600, back];
            let de_esser = |size| {
                let mut f = DeEsser::default();
                let mut c = DeEsserConfig { threshold_db: -45.0, ..DeEsserConfig::default() };
                cut_run(&hissed, size, &changes, |b, at| {
                    if at == 4_800 || at == back {
                        c.enabled = true;
                    } else if at == 9_600 {
                        c.enabled = false;
                    }
                    f.configure(c);
                    f.process(b);
                })
            };
            let eq = |size| {
                let mut f = Eq::default();
                let mut c = EqConfig::default();
                c.bands[1] = EqBandConfig { enabled: true, hz: 150.0, gain_db: 12.0, q: 8.0 };
                cut_run(&hissed, size, &changes, |b, at| {
                    if at == 4_800 || at == back {
                        c.enabled = true;
                    } else if at == 9_600 {
                        c.enabled = false;
                    }
                    f.configure(c);
                    f.process(b);
                })
            };
            for (name, run) in [("de-esser", &de_esser as &dyn Fn(usize) -> Vec<Frame>), ("eq", &eq)] {
                let blocks = run(BLOCK_FRAMES);
                for size in [64, 37] {
                    let d = largest_difference(&run(size), &blocks);
                    assert!(d < 1e-6, "{name}, back at {back}, in {size}-frame calls: {d}");
                }
            }
        }
    }

    /// A saved EQ with one band out of range keeps the others.
    #[test]
    fn an_eq_band_out_of_range_alone_is_reset() {
        let mut config = EqConfig {
            enabled: true,
            ..EqConfig::default()
        };
        config.bands[1].gain_db = 6.0;
        config.bands[2].gain_db = 99.0;
        let clean = config.sanitized();
        assert!((clean.bands[1].gain_db - 6.0).abs() < 1e-6, "kept");
        assert!(clean.bands[2].gain_db.abs() < 1e-6 && (clean.bands[2].hz - 3_000.0).abs() < 1e-3, "reset");
        assert!(clean.enabled);
    }

    #[test]
    fn the_leveler_brings_soft_and_loud_speakers_to_the_same_level() {
        let config = LevelerConfig {
            enabled: true,
            ..LevelerConfig::default()
        };
        for level in [-30.0, -26.0, -14.0] {
            let mut leveler = Leveler::default();
            leveler.configure(config);
            let out = run(&talk(level, 30.0), |b| leveler.process(b));
            // Settled, while talking: the last phrase.
            let phrase: Vec<_> = out[2850..2950].to_vec();
            let got = rms_db(&phrase);
            assert!(
                (got - f64::from(config.target_db)).abs() < 1.5,
                "{level} dB came out at {got:.1}"
            );
        }
    }

    #[test]
    fn the_leveler_holds_still_through_pauses_rather_than_raising_the_room() {
        let mut leveler = Leveler::default();
        leveler.configure(LevelerConfig {
            enabled: true,
            ..LevelerConfig::default()
        });
        // A soft talker brings the gain up; then ten seconds of room noise.
        run(&talk(-30.0, 20.0), |b| leveler.process(b));
        let before = leveler.gain_db();
        let room = sine(1234.5, 0.0005, 10.0);
        run(&room, |b| leveler.process(b));
        assert!(
            (leveler.gain_db() - before).abs() < 0.5,
            "drifted from {before} to {}",
            leveler.gain_db()
        );
    }

    #[test]
    fn the_leveler_never_goes_past_its_limits() {
        let mut leveler = Leveler::default();
        leveler.configure(LevelerConfig {
            enabled: true,
            max_boost_db: 6.0,
            max_cut_db: 4.0,
            ..LevelerConfig::default()
        });
        run(&talk(-40.0, 40.0), |b| leveler.process(b));
        assert!(leveler.gain_db() <= 6.0 + 1e-3, "{}", leveler.gain_db());
        run(&talk(-6.0, 40.0), |b| leveler.process(b));
        assert!(leveler.gain_db() >= -4.0 - 1e-3, "{}", leveler.gain_db());
    }

    #[test]
    fn the_compressor_follows_its_static_curve() {
        let config = CompressorConfig {
            enabled: true,
            threshold_db: -20.0,
            ratio: 4.0,
            knee_db: 0.0,
            attack_ms: 1.0,
            release_ms: 50.0,
            makeup_db: 0.0,
        };
        // 12 dB over the threshold comes out 3 dB over it; under it, as is.
        // A steady level, so the detector settles exactly on the curve.
        for (input_db, expected_db) in [(-8.0_f32, -17.0), (-26.0, -26.0)] {
            let mut compressor = Compressor::default();
            compressor.configure(config);
            let level = db_to_gain(input_db);
            let steady = vec![vec![[level, -level]; BLOCK_FRAMES]; 50];
            let out = run(&steady, |b| compressor.process(b));
            let got = to_db(peak(&out[40..]), false);
            assert!((got - expected_db).abs() < 0.05, "{input_db}: {got:.2}");
        }
        // On a 1 kHz tone a 1 ms attack lags each peak a little: a few
        // tenths of a dB above the curve, as any peak compressor does.
        let mut compressor = Compressor::default();
        compressor.configure(config);
        let out = run(&sine(1000.0, f64::from(db_to_gain(-8.0)), 1.0), |b| {
            compressor.process(b);
        });
        let got = to_db(peak(&out[60..]), false);
        assert!(got > -17.0 && got < -16.0, "tone: {got:.2}");
    }

    #[test]
    fn the_compressors_soft_knee_eases_in_around_the_threshold() {
        let config = CompressorConfig {
            enabled: true,
            threshold_db: -20.0,
            ratio: 4.0,
            knee_db: 6.0,
            ..CompressorConfig::default()
        };
        // At the threshold, half the knee in: (1/4 − 1)·3² / 12 = −0.5625 dB.
        assert!((config.reduction_db(-20.0) + 0.5625).abs() < 1e-9);
        assert!(config.reduction_db(-23.0).abs() < 1e-9);
        assert!((config.reduction_db(-17.0) + 2.25).abs() < 1e-9);
    }

    #[test]
    fn bypassing_the_compressor_fades_and_never_boosts_past_either_end() {
        // The contributed compressor ramped its two gains separately and
        // multiplied them, so a bypass with a large makeup overshot by up to
        // +9.6 dB. One gain faded between unity and the compressor's cannot.
        let config = CompressorConfig {
            enabled: true,
            threshold_db: -40.0,
            ratio: 20.0,
            knee_db: 0.0,
            attack_ms: 0.1,
            makeup_db: 24.0,
            ..CompressorConfig::default()
        };
        let mut compressor = Compressor::default();
        compressor.configure(config);
        let tone = sine(1000.0, 0.5, 1.0);
        let settled = run(&tone[..50], |b| compressor.process(b));
        let on = peak(&settled[40..]);
        compressor.configure(CompressorConfig {
            enabled: false,
            ..config
        });
        let fading = run(&tone[50..52], |b| compressor.process(b));
        let ceiling = on.max(0.5) + 1e-3;
        assert!(peak(&fading) <= ceiling, "{} over {ceiling}", peak(&fading));
        let off = run(&tone[52..60], |b| compressor.process(b));
        assert!((peak(&off) - 0.5).abs() < 1e-3);
    }

    #[test]
    fn the_limiter_never_lets_a_sample_past_its_ceiling() {
        let mut limiter = Limiter::default();
        limiter.configure(LimiterConfig::default());
        let ceiling = f64::from(db_to_gain(-1.0));
        // A burst at +6 dBFS out of quiet, then a run of single-sample
        // spikes: the hardest cases for a limiter.
        let mut blocks = sine(440.0, 0.1, 0.2);
        blocks.extend(sine(440.0, 2.0, 0.3));
        let mut spikes = sine(440.0, 0.05, 0.3);
        for (i, frame) in spikes.iter_mut().flatten().enumerate() {
            if i % 997 == 0 {
                *frame = [3.0, -3.0];
            }
        }
        blocks.extend(spikes);
        let out = run(&blocks, |b| limiter.process(b));
        assert!(peak(&out) <= ceiling + 1e-6, "{}", peak(&out));
        assert!(limiter.reduction_db() >= 0.0);
    }

    #[test]
    fn under_its_ceiling_the_limiter_only_delays_the_sound() {
        let mut limiter = Limiter::default();
        limiter.configure(LimiterConfig::default());
        let input = sine(440.0, 0.5, 0.2);
        let out = run(&input, |b| limiter.process(b));
        let flat_in: Vec<Frame> = input.into_iter().flatten().collect();
        let flat_out: Vec<Frame> = out.into_iter().flatten().collect();
        for i in LIMITER_LOOKAHEAD..flat_in.len() {
            assert!(
                (flat_out[i][0] - flat_in[i - LIMITER_LOOKAHEAD][0]).abs() < 1e-6,
                "sample {i}"
            );
        }
    }

    #[test]
    fn a_limiter_switched_off_still_holds_the_same_delay() {
        let mut limiter = Limiter::default();
        limiter.configure(LimiterConfig {
            enabled: false,
            ..LimiterConfig::default()
        });
        let input = sine(440.0, 2.0, 0.1);
        // A new limiter is on: switched off, it fades out over the first
        // block; from the second, the sound passes a look-ahead late.
        let out = run(&input, |b| limiter.process(b));
        let flat_in: Vec<Frame> = input.into_iter().flatten().collect();
        let flat_out: Vec<Frame> = out.into_iter().flatten().collect();
        for i in BLOCK_FRAMES..flat_in.len() {
            assert!(
                (flat_out[i][0] - flat_in[i - LIMITER_LOOKAHEAD][0]).abs() < 1e-6,
                "sample {i}"
            );
        }
        assert!(peak(&[flat_out]) > 1.9, "off: not limited");
    }

    /// The largest jump between neighbouring samples on the left channel.
    fn biggest_step(blocks: &[Vec<Frame>]) -> f32 {
        let flat: Vec<Frame> = blocks.iter().flatten().copied().collect();
        flat.windows(2)
            .map(|w| (w[1][0] - w[0][0]).abs())
            .fold(0.0_f32, f32::max)
    }

    #[test]
    fn changing_the_low_cuts_slope_or_frequency_fades_rather_than_clicks() {
        // Review: a 24 dB slope made 12 left the second stage passing a stale
        // state out — 0.001 to 0.54 between two samples.
        let mut low_cut = LowCut::default();
        let mut config = LowCutConfig::speech();
        low_cut.configure(config);
        let tone = sine(100.0, 0.5, 4.0);
        let mut out = run(&tone[..100], |b| low_cut.process(b));
        let steady = biggest_step(&out[50..]);
        for (at, change) in [(100, 12_u8), (150, 24), (200, 24)] {
            if change == 24 && at == 200 {
                config.hz = 300.0;
            } else {
                config.slope_db = change;
            }
            low_cut.configure(config);
            out = run(&tone[at..at + 50], |b| low_cut.process(b));
            let step = biggest_step(&out);
            assert!(
                step < steady * 2.0 + 0.002,
                "at {at}: {step} against {steady}"
            );
        }
    }

    #[test]
    fn retuning_the_low_cut_far_does_not_spike() {
        // Review: a 12 dB cut at 400 Hz moved to 20 Hz under a settled
        // 400 Hz tone peaked at 3.44 with the new design on the old memory.
        let mut low_cut = LowCut::default();
        let mut config = LowCutConfig {
            enabled: true,
            hz: 400.0,
            slope_db: 12,
        };
        low_cut.configure(config);
        let tone = sine(400.0, 0.5, 2.0);
        run(&tone[..100], |b| low_cut.process(b));
        config.hz = 20.0;
        low_cut.configure(config);
        let out = run(&tone[100..150], |b| low_cut.process(b));
        assert!(peak(&out) <= 0.55, "{}", peak(&out));
    }

    #[test]
    fn lowering_the_levelers_limits_in_a_pause_takes_hold_at_once() {
        let mut config = LevelerConfig::speech();
        let mut leveler = Leveler::default();
        leveler.configure(config);
        run(&talk(-30.0, 20.0), |b| leveler.process(b));
        assert!(leveler.gain_db() > 8.0, "{}", leveler.gain_db());
        // A pause; then the most boost is lowered to nothing.
        config.max_boost_db = 0.0;
        leveler.configure(config);
        run(&sine(1234.5, 0.0005, 0.02), |b| leveler.process(b));
        assert!(leveler.gain_db() <= 1e-3, "{}", leveler.gain_db());
    }

    #[test]
    fn switching_the_leveler_off_returns_to_unity_within_a_block() {
        let mut leveler = Leveler::default();
        leveler.configure(LevelerConfig::speech());
        let speech = talk(-30.0, 20.0);
        run(&speech, |b| leveler.process(b));
        leveler.configure(LevelerConfig::default());
        let out = run(&speech[..2], |b| leveler.process(b));
        assert!(leveler.gain_db().abs() < 1e-6, "{}", leveler.gain_db());
        // The second block passes untouched.
        assert_eq!(out[1], speech[1]);
    }

    #[test]
    fn switching_the_limiter_off_lets_go_within_a_block() {
        let mut limiter = Limiter::default();
        let loud = sine(440.0, 3.0, 1.0);
        run(&loud[..50], |b| limiter.process(b));
        assert!(limiter.reduction_db() > 9.0);
        limiter.configure(LimiterConfig {
            enabled: false,
            ..LimiterConfig::default()
        });
        run(&loud[50..51], |b| limiter.process(b));
        // The next block is the input as it was, a look-ahead late.
        let out = run(&loud[51..52], |b| limiter.process(b));
        assert!(
            limiter.reduction_db().abs() < 1e-6,
            "{}",
            limiter.reduction_db()
        );
        let flat_in: Vec<Frame> = loud[50..52].iter().flatten().copied().collect();
        for (i, frame) in out[0].iter().enumerate() {
            let source = flat_in[BLOCK_FRAMES + i - LIMITER_LOOKAHEAD];
            assert!((frame[0] - source[0]).abs() < 1e-6, "sample {i}");
        }
    }

    #[test]
    fn switching_the_limiter_on_under_a_loud_sound_fades_it_down() {
        // Review: switched on under a steady 2.0 it cut to 0.891 in one
        // sample. Faded over a block, then held under the ceiling.
        let mut limiter = Limiter::default();
        limiter.configure(LimiterConfig {
            enabled: false,
            ..LimiterConfig::default()
        });
        let steady = vec![vec![[2.0_f32, 2.0]; BLOCK_FRAMES]; 20];
        let mut out = run(&steady[..10], |b| limiter.process(b));
        limiter.configure(LimiterConfig::default());
        out.extend(run(&steady[10..], |b| limiter.process(b)));
        // Past the first block, where the look-ahead fills from silence.
        assert!(
            biggest_step(&out[2..]) < 0.01,
            "{}",
            biggest_step(&out[2..])
        );
        let ceiling = db_to_gain(-1.0);
        assert!(out[15..].iter().flatten().all(|f| f[0] <= ceiling + 1e-6));
    }

    #[test]
    fn integrated_loudness_gates_each_block_by_its_own_loudness() {
        // Review: with bins gated by their middle, 100.4 s just under the
        // relative gate then 10 s at −10 read −20.06 LUFS; gating each block
        // by its own loudness gives −10.06.
        let mut meter = LoudnessMeter::default();
        for block in sine(1000.0, f64::from(db_to_gain(-30.09)), 100.4) {
            meter.measure(&block);
        }
        for block in sine(1000.0, f64::from(db_to_gain(-10.0)), 10.0) {
            meter.measure(&block);
        }
        let integrated = meter.snapshot().integrated_lufs.expect("measured");
        assert!((f64::from(integrated) + 10.06).abs() < 0.1, "{integrated}");
    }

    #[test]
    fn integrated_loudness_splits_the_block_group_the_gate_falls_in() {
        // Review: 50 s each at −30.009 and −30.001 LUFS lie within 0.01 LU,
        // with the relative gate between them; gated block by block they
        // read −17.58 with 10 s at −9.954.
        let mut meter = LoudnessMeter::default();
        for (level, secs) in [(-30.009, 50.0), (-30.001, 50.0), (-9.954, 10.0)] {
            for block in sine(1000.0, f64::from(db_to_gain(level)), secs) {
                meter.measure(&block);
            }
        }
        let integrated = meter.snapshot().integrated_lufs.expect("measured");
        assert!((f64::from(integrated) + 17.58).abs() < 0.3, "{integrated}");
    }

    #[test]
    fn the_momentary_reading_catches_a_short_burst_between_steps() {
        // A 400 ms burst starting 40 ms into a 100 ms step: windows moving
        // every 10 ms see all but a block of it.
        let mut meter = LoudnessMeter::default();
        let mut most = f32::NEG_INFINITY;
        let silence = vec![[0.0_f32; 2]; BLOCK_FRAMES];
        for _ in 0..104 {
            meter.measure(&silence);
        }
        for block in sine(1000.0, f64::from(db_to_gain(-20.0)), 0.4) {
            meter.measure(&block);
            most = most.max(meter.snapshot().momentary_lufs);
        }
        for _ in 0..50 {
            meter.measure(&silence);
            most = most.max(meter.snapshot().momentary_lufs);
        }
        assert!((most + 20.0).abs() < 0.1, "{most}");
    }

    #[test]
    fn after_a_stall_the_limiter_replays_nothing_and_loudness_counts_silence() {
        let mut limiter = Limiter::default();
        run(&sine(440.0, 0.5, 0.1), |b| limiter.process(b));
        limiter.clear();
        let silent = vec![vec![[0.0_f32; 2]; BLOCK_FRAMES]];
        let out = run(&silent, |b| limiter.process(b));
        assert!(out[0].iter().all(|f| f[0] == 0.0), "old sound came back");

        let mut meter = LoudnessMeter::default();
        for block in sine(1000.0, f64::from(db_to_gain(-20.0)), 5.0) {
            meter.measure(&block);
        }
        meter.skip(1_000);
        let reading = meter.snapshot();
        assert!(reading.momentary_lufs < -70.0 && reading.short_term_lufs < -70.0);
        let integrated = reading.integrated_lufs.expect("kept");
        assert!((f64::from(integrated) + 20.0).abs() < 0.2, "{integrated}");
    }

    #[test]
    fn dragging_the_low_cut_slowly_does_not_distort() {
        // Review: each step of a slow drag starting the new design from
        // silence raised a 50 Hz tone under a 100 Hz cut from 0.022 to
        // 0.038 RMS.
        let mut low_cut = LowCut::default();
        let mut config = LowCutConfig::speech();
        low_cut.configure(config);
        let tone = sine(50.0, 0.5, 4.0);
        let settled = run(&tone[..150], |b| low_cut.process(b));
        let rms = |blocks: &[Vec<Frame>]| {
            let flat: Vec<f64> = blocks.iter().flatten().map(|f| f64::from(f[0])).collect();
            (flat.iter().map(|x| x * x).sum::<f64>() / flat.len() as f64).sqrt()
        };
        let steady = rms(&settled[100..]);
        let mut dragged = Vec::new();
        for (i, block) in tone[150..200].iter().enumerate() {
            config.hz = 100.0 + 0.01 * (i + 1) as f32;
            low_cut.configure(config);
            let mut b = block.clone();
            low_cut.process(&mut b);
            dragged.push(b);
        }
        let during = rms(&dragged);
        assert!(during < steady * 1.1, "{during} against {steady}");
    }

    #[test]
    fn dragging_the_low_cut_slowly_at_the_bottom_of_its_range_does_not_distort() {
        // Review: at 20 Hz, rewarming each step on 100 ms let a 5 Hz tone
        // through at nearly twice its RMS; small moves keep the memory.
        let mut low_cut = LowCut::default();
        let mut config = LowCutConfig {
            enabled: true,
            hz: 20.0,
            slope_db: 24,
        };
        low_cut.configure(config);
        let tone = sine(5.0, 0.5, 6.0);
        let settled = run(&tone[..350], |b| low_cut.process(b));
        let rms = |blocks: &[Vec<Frame>]| {
            let flat: Vec<f64> = blocks.iter().flatten().map(|f| f64::from(f[0])).collect();
            (flat.iter().map(|x| x * x).sum::<f64>() / flat.len() as f64).sqrt()
        };
        let steady = rms(&settled[250..]);
        let mut dragged = Vec::new();
        for (i, block) in tone[350..400].iter().enumerate() {
            config.hz = 20.0 + 0.01 * (i + 1) as f32;
            low_cut.configure(config);
            let mut b = block.clone();
            low_cut.process(&mut b);
            dragged.push(b);
        }
        let during = rms(&dragged);
        assert!(during < steady * 1.1, "{during} against {steady}");
    }

    #[test]
    fn far_over_full_scale_each_level_is_still_gated_apart() {
        // Review: with everything from +5 LUFS up in one bin, 1,000 blocks at
        // +6 and 100 at +27 (gate +6.9) read +16.9 rather than +27.
        let power = |lufs: f64| 10f64.powf((lufs + 0.691) / 10.0);
        let mut meter = LoudnessMeter::default();
        for _ in 0..1_000 {
            meter.record(power(6.0));
        }
        for _ in 0..100 {
            meter.record(power(27.0));
        }
        let integrated = meter.integrated().expect("measured");
        assert!((integrated - 27.0).abs() < 1e-6, "{integrated}");
    }

    #[test]
    fn nearly_equal_crowds_either_side_of_the_gate_are_told_apart() {
        // Review: 999 blocks at −30.009, 1 at −30.001 and 100 at −10.0046
        // put the relative gate between the first two. Split in proportion
        // within their 0.01 LU bin they read −17.574; block by block only
        // the one at −30.001 and the loud hundred count: −10.047.
        let power = |lufs: f64| 10f64.powf((lufs + 0.691) / 10.0);
        let mut meter = LoudnessMeter::default();
        for (lufs, n) in [(-30.009, 999), (-30.001, 1), (-10.0046, 100)] {
            for _ in 0..n {
                meter.record(power(lufs));
            }
        }
        let expected = lufs((power(-30.001) + 100.0 * power(-10.0046)) / 101.0);
        let integrated = meter.integrated().expect("measured");
        assert!(
            (integrated - expected).abs() < 1e-9,
            "{integrated} against {expected}"
        );
        assert!((integrated + 10.047).abs() < 0.001, "{integrated}");
    }

    #[test]
    fn a_reset_meter_keeps_counting_past_its_first_chunk() {
        // Blocks are kept in chunks; a reset keeps the first. Fill past it,
        // reset, fill past it again: the reading is the new blocks' alone.
        let power = |lufs: f64| 10f64.powf((lufs + 0.691) / 10.0);
        let mut meter = LoudnessMeter::default();
        for _ in 0..GATED_CHUNK + 10 {
            meter.record(power(-40.0));
        }
        meter.reset();
        for _ in 0..GATED_CHUNK * 2 + 7 {
            meter.record(power(-23.0));
        }
        let integrated = meter.integrated().expect("measured");
        assert!((integrated + 23.0).abs() < 1e-9, "{integrated}");
        assert_eq!(meter.gated_len, GATED_CHUNK * 2 + 7);
    }

    #[test]
    fn a_crowd_of_identical_blocks_at_the_gate_is_gated_exactly() {
        // Review: 1,000 blocks at −30.009 and 100 at −10.0046 put the gate at
        // −30.005, inside the first group's bin: split in proportion they
        // read −17.57; gated block by block, −10.0046. The blocks go straight
        // in at exactly those loudnesses, so the answer is exact.
        let power = |lufs: f64| 10f64.powf((lufs + 0.691) / 10.0);
        let mut meter = LoudnessMeter::default();
        for _ in 0..1_000 {
            meter.record(power(-30.009));
        }
        for _ in 0..100 {
            meter.record(power(-10.0046));
        }
        let integrated = meter.integrated().expect("measured");
        assert!((integrated + 10.0046).abs() < 1e-6, "{integrated}");
        // And all on one side of the gate, either way, is counted whole.
        let mut meter = LoudnessMeter::default();
        for _ in 0..100 {
            meter.record(power(-20.0));
        }
        let all = meter.integrated().expect("measured");
        assert!((all + 20.0).abs() < 1e-6, "{all}");
    }

    #[test]
    fn a_bus_far_over_full_scale_still_has_an_integrated_loudness() {
        // Review: everything from +5 LUFS up shares the top bin; read as
        // [+5, +5.01) a +12 LUFS programme left nothing above its own gate.
        let mut meter = LoudnessMeter::default();
        for block in sine(1000.0, 4.0, 10.0) {
            meter.measure(&block);
        }
        let integrated = meter.snapshot().integrated_lufs.expect("measured");
        assert!((f64::from(integrated) - 12.04).abs() < 0.15, "{integrated}");
    }

    #[test]
    fn a_small_retune_under_heavy_rumble_leaves_no_transient() {
        // Review: a settled 5 Hz tone at 0.5 under a 400 Hz, 24 dB cut, moved
        // to 392.4 Hz in place, left a 0.000421 transient — −31.5 dBFS after
        // the most makeup and gain allowed. Retuned while it runs, it stays
        // far under hearing wherever in the rumble's cycle it is moved: tried
        // every 20 ms of it (at a zero crossing there is nothing to disturb,
        // and moved only there the old filter passed too).
        let rumble = sine(5.0, 0.5, 4.0);
        let mut worst = 0.0_f64;
        for settle in (300..320).step_by(2) {
            let mut low_cut = LowCut::default();
            let mut config = LowCutConfig {
                enabled: true,
                hz: 400.0,
                slope_db: 24,
            };
            low_cut.configure(config);
            run(&rumble[..settle], |b| low_cut.process(b));
            config.hz = 392.4;
            low_cut.configure(config);
            let out = run(&rumble[settle..settle + 40], |b| low_cut.process(b));
            worst = worst.max(peak(&out));
        }
        assert!(worst < 1e-5, "{worst}");
    }

    #[test]
    fn lowering_the_limiters_ceiling_while_it_works_glides() {
        // Review: −1 → −12 dBFS on a 0.8 tone jumped 0.542 between two
        // samples. It does when the change finds the delayed sound near its
        // peak; at a zero crossing the cut is as gentle as a clamp, and tried
        // only there the old limiter passed too. So: every 4th sample of the
        // cycle, counting the step across the change.
        let ceiling = db_to_gain(-12.0);
        for offset in (0..48).step_by(4) {
            let frames: Vec<Frame> = sine(1000.0, 0.8, 1.0).concat()[offset..].to_vec();
            let tone: Vec<Vec<Frame>> =
                frames.chunks(BLOCK_FRAMES).map(<[Frame]>::to_vec).collect();
            let mut limiter = Limiter::default();
            let before = run(&tone[..20], |b| limiter.process(b));
            let steady = biggest_step(&before[5..]);
            limiter.configure(LimiterConfig {
                ceiling_db: -12.0,
                ..LimiterConfig::default()
            });
            let after = run(&tone[20..40], |b| limiter.process(b));
            let mut across = vec![before[19].clone()];
            across.extend(after.iter().cloned());
            let step = biggest_step(&across);
            assert!(
                step < steady * 1.1,
                "offset {offset}: {step} against {steady}"
            );
            assert!(after[5..]
                .iter()
                .flatten()
                .all(|f| f[0].abs() <= ceiling + 1e-6));
        }
    }

    #[test]
    fn a_sine_measures_its_bs_1770_loudness() {
        // A 1 kHz sine with peak A in both channels reads 20·log10(A) LUFS:
        // K-weighting's +0.691 dB at 1 kHz is what BS.1770's −0.691 undoes.
        for level in [-23.0_f32, -16.0] {
            let mut meter = LoudnessMeter::default();
            let level64 = f64::from(level);
            for block in sine(1000.0, f64::from(db_to_gain(level)), 10.0) {
                meter.measure(&block);
            }
            let reading = meter.snapshot();
            assert!(
                (f64::from(reading.momentary_lufs) - level64).abs() < 0.1,
                "{reading:?}"
            );
            assert!(
                (f64::from(reading.short_term_lufs) - level64).abs() < 0.1,
                "{reading:?}"
            );
            let integrated = reading.integrated_lufs.expect("measured");
            assert!(
                (f64::from(integrated) - level64).abs() < 0.15,
                "{reading:?}"
            );
        }
    }

    #[test]
    fn integrated_loudness_leaves_out_silence_and_much_quieter_passages() {
        let mut meter = LoudnessMeter::default();
        // 10 s at −20, 10 s of silence, 10 s at −40 (20 LU under: gated out).
        for block in sine(1000.0, f64::from(db_to_gain(-20.0)), 10.0) {
            meter.measure(&block);
        }
        for _ in 0..1000 {
            meter.measure(&[[0.0; 2]; BLOCK_FRAMES]);
        }
        for block in sine(1000.0, f64::from(db_to_gain(-40.0)), 10.0) {
            meter.measure(&block);
        }
        let integrated = meter.snapshot().integrated_lufs.expect("measured");
        assert!((f64::from(integrated) + 20.0).abs() < 0.3, "{integrated}");
        meter.reset();
        assert_eq!(meter.snapshot().integrated_lufs, None);
    }
}
