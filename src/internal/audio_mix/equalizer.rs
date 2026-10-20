use ffmpeg_next::{filter, format, frame, ChannelLayout};

use super::reader::{db_to_gain, gain_to_db, Frame};
use super::{BLOCK_FRAMES, SAMPLE_RATE};

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct EqualizerShelf {
    pub enabled: bool,
    pub frequency_hz: f32,
    pub gain_db: f32,
}

impl Default for EqualizerShelf {
    fn default() -> Self {
        Self {
            enabled: true,
            frequency_hz: 120.0,
            gain_db: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct EqualizerBand {
    pub enabled: bool,
    pub frequency_hz: f32,
    pub gain_db: f32,
    pub q: f32,
}

impl Default for EqualizerBand {
    fn default() -> Self {
        Self {
            enabled: true,
            frequency_hz: 1000.0,
            gain_db: 0.0,
            q: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct EqualizerConfig {
    pub enabled: bool,
    pub high_pass: bool,
    pub high_pass_hz: f32,
    pub low_shelf: EqualizerShelf,
    pub bands: [EqualizerBand; 3],
    pub high_shelf: EqualizerShelf,
    pub output_db: f32,
}

impl Default for EqualizerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            high_pass: false,
            high_pass_hz: 80.0,
            low_shelf: EqualizerShelf::default(),
            bands: [250.0, 1000.0, 4000.0].map(|frequency_hz| EqualizerBand {
                frequency_hz,
                ..EqualizerBand::default()
            }),
            high_shelf: EqualizerShelf {
                frequency_hz: 8000.0,
                ..EqualizerShelf::default()
            },
            output_db: 0.0,
        }
    }
}

impl EqualizerConfig {
    pub fn response_db(&self, frequency_hz: f32) -> f32 {
        if self.validate().is_err()
            || !frequency_hz.is_finite()
            || !(20.0..=20_000.0).contains(&frequency_hz)
        {
            return 0.0;
        }
        let angular = std::f64::consts::TAU * f64::from(frequency_hz) / f64::from(SAMPLE_RATE);
        let magnitude = |numerator: [f64; 3], denominator: [f64; 3]| {
            let power = |coefficients: [f64; 3]| {
                let real = coefficients[0]
                    + coefficients[1] * angular.cos()
                    + coefficients[2] * (2.0 * angular).cos();
                let imaginary =
                    coefficients[1] * angular.sin() + coefficients[2] * (2.0 * angular).sin();
                real * real + imaginary * imaginary
            };
            10.0 * (power(numerator) / power(denominator)).max(1e-20).log10()
        };
        let mut db = f64::from(self.output_db);
        if self.high_pass {
            let omega =
                std::f64::consts::TAU * f64::from(self.high_pass_hz) / f64::from(SAMPLE_RATE);
            let cosine = omega.cos();
            let alpha = omega.sin() / std::f64::consts::SQRT_2;
            db += magnitude(
                [
                    f64::midpoint(1.0, cosine),
                    -(1.0 + cosine),
                    f64::midpoint(1.0, cosine),
                ],
                [1.0 + alpha, -2.0 * cosine, 1.0 - alpha],
            );
        }
        for (high, shelf) in [(false, self.low_shelf), (true, self.high_shelf)] {
            if !shelf.enabled {
                continue;
            }
            let omega =
                std::f64::consts::TAU * f64::from(shelf.frequency_hz) / f64::from(SAMPLE_RATE);
            let cosine = omega.cos();
            let amplitude = 10_f64.powf(f64::from(shelf.gain_db) / 40.0);
            let beta = amplitude.sqrt() * omega.sin() * std::f64::consts::SQRT_2;
            let plus = amplitude + 1.0;
            let minus = amplitude - 1.0;
            let (numerator, denominator) = if high {
                (
                    [
                        amplitude * (plus + minus * cosine + beta),
                        -2.0 * amplitude * (minus + plus * cosine),
                        amplitude * (plus + minus * cosine - beta),
                    ],
                    [
                        plus - minus * cosine + beta,
                        2.0 * (minus - plus * cosine),
                        plus - minus * cosine - beta,
                    ],
                )
            } else {
                (
                    [
                        amplitude * (plus - minus * cosine + beta),
                        2.0 * amplitude * (minus - plus * cosine),
                        amplitude * (plus - minus * cosine - beta),
                    ],
                    [
                        plus + minus * cosine + beta,
                        -2.0 * (minus + plus * cosine),
                        plus + minus * cosine - beta,
                    ],
                )
            };
            db += magnitude(numerator, denominator);
        }
        for band in &self.bands {
            if !band.enabled {
                continue;
            }
            let omega =
                std::f64::consts::TAU * f64::from(band.frequency_hz) / f64::from(SAMPLE_RATE);
            let cosine = omega.cos();
            let alpha = omega.sin() / (2.0 * f64::from(band.q));
            let amplitude = 10_f64.powf(f64::from(band.gain_db) / 40.0);
            db += magnitude(
                [
                    1.0 + alpha * amplitude,
                    -2.0 * cosine,
                    1.0 - alpha * amplitude,
                ],
                [
                    1.0 + alpha / amplitude,
                    -2.0 * cosine,
                    1.0 - alpha / amplitude,
                ],
            );
        }
        db as f32
    }

    /// # Errors
    /// Rejects non-finite values or parameters outside the supported bounds.
    pub fn validate(&self) -> Result<(), &'static str> {
        let valid =
            |value: f32, min: f32, max: f32| value.is_finite() && (min..=max).contains(&value);
        if !valid(self.high_pass_hz, 20.0, 500.0)
            || !valid(self.output_db, -24.0, 12.0)
            || !valid(self.low_shelf.frequency_hz, 20.0, 2000.0)
            || !valid(self.high_shelf.frequency_hz, 1000.0, 20_000.0)
            || !valid(self.low_shelf.gain_db, -12.0, 12.0)
            || !valid(self.high_shelf.gain_db, -12.0, 12.0)
            || self.bands.iter().any(|band| {
                !valid(band.frequency_hz, 20.0, 20_000.0)
                    || !valid(band.gain_db, -12.0, 12.0)
                    || !valid(band.q, 0.25, 10.0)
            })
        {
            return Err("EQ values must be finite and within their supported ranges");
        }
        Ok(())
    }

    #[must_use]
    pub fn sanitized(self) -> Self {
        if self.validate().is_ok() {
            self
        } else {
            Self::default()
        }
    }

    fn filters(&self) -> Vec<String> {
        let mut filters = Vec::new();
        if self.high_pass {
            filters.push(format!(
                "highpass=f={}:p=2:t=q:w=0.7071067811865476",
                self.high_pass_hz
            ));
        }
        for (name, shelf) in [("lowshelf", self.low_shelf), ("highshelf", self.high_shelf)] {
            if shelf.enabled && shelf.gain_db.abs() > f32::EPSILON {
                filters.push(format!(
                    "{name}=f={}:t=s:w=1:g={}",
                    shelf.frequency_hz, shelf.gain_db
                ));
            }
        }
        for band in &self.bands {
            if band.enabled && band.gain_db.abs() > f32::EPSILON {
                filters.push(format!(
                    "equalizer=f={}:t=q:w={}:g={}",
                    band.frequency_hz, band.q, band.gain_db
                ));
            }
        }
        filters
    }
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct EqualizerSnapshot {
    pub config: EqualizerConfig,
    pub pre_db: [f32; 2],
    pub post_db: [f32; 2],
    pub ready: bool,
    pub updating: bool,
    pub error: Option<String>,
}

impl Default for EqualizerSnapshot {
    fn default() -> Self {
        Self {
            config: EqualizerConfig::default(),
            pre_db: [-96.0; 2],
            post_db: [-96.0; 2],
            ready: false,
            updating: false,
            error: None,
        }
    }
}

pub(super) struct Processor {
    _graph: filter::Graph,
    source: filter::Context,
    sink: filter::Context,
    input: frame::Audio,
    output: frame::Audio,
    pts: i64,
}

unsafe impl Send for Processor {}

impl Processor {
    pub fn prepare(config: EqualizerConfig) -> Result<Option<Self>, String> {
        config.validate().map_err(str::to_owned)?;
        if !config.enabled || config.filters().is_empty() {
            return Ok(None);
        }
        Self::new(config).map(Some)
    }

    pub fn new(config: EqualizerConfig) -> Result<Self, String> {
        config.validate().map_err(str::to_owned)?;
        let mut graph = filter::Graph::new();
        unsafe {
            (*graph.as_mut_ptr()).nb_threads = 1;
        }
        let find =
            |name| filter::find(name).ok_or_else(|| format!("FFmpeg filter {name} unavailable"));
        graph
            .add(
                &find("abuffer")?,
                "in",
                "time_base=1/48000:sample_rate=48000:sample_fmt=dblp:channel_layout=stereo",
            )
            .map_err(|error| format!("EQ input: {error:?}"))?;
        graph
            .add(&find("abuffersink")?, "out", "")
            .map_err(|error| format!("EQ output: {error:?}"))?;
        let filters = config.filters();
        let spec = if filters.is_empty() {
            "anull".into()
        } else {
            filters
                .iter()
                .map(|filter| format!("{filter}:precision=f64:transform=tdii:blocksize=0"))
                .collect::<Vec<_>>()
                .join(",")
        };
        graph
            .output("in", 0)
            .and_then(|parser| parser.input("out", 0))
            .and_then(|parser| {
                parser.parse(&format!(
                    "{spec},aformat=sample_fmts=dblp:sample_rates=48000:channel_layouts=stereo"
                ))
            })
            .map_err(|error| format!("EQ filter graph: {error:?}"))?;
        graph
            .validate()
            .map_err(|error| format!("EQ format negotiation: {error:?}"))?;
        let source = graph.get("in").ok_or("FFmpeg EQ source missing")?;
        let sink = graph.get("out").ok_or("FFmpeg EQ sink missing")?;
        let mut input = frame::Audio::new(
            format::Sample::F64(format::sample::Type::Planar),
            BLOCK_FRAMES,
            ChannelLayout::STEREO,
        );
        input.set_rate(SAMPLE_RATE);
        if input.planes() != 2 {
            return Err("FFmpeg EQ buffer allocation failed".into());
        }
        Ok(Self {
            _graph: graph,
            source,
            sink,
            input,
            output: frame::Audio::empty(),
            pts: 0,
        })
    }

    fn render(
        &mut self,
        input: &[Frame],
        output: &mut [Frame; BLOCK_FRAMES],
    ) -> Result<(), String> {
        if input.len() != BLOCK_FRAMES {
            return Err("Unexpected EQ block size".into());
        }
        for channel in 0..2 {
            for (target, source) in self.input.plane_mut::<f64>(channel).iter_mut().zip(input) {
                *target = if source[channel].is_finite() {
                    f64::from(source[channel])
                } else {
                    0.0
                };
            }
        }
        self.input.set_pts(Some(self.pts));
        self.source
            .source()
            .add(&self.input)
            .map_err(|error| format!("EQ input frame: {error:?}"))?;
        self.sink
            .sink()
            .frame(&mut self.output)
            .map_err(|error| format!("EQ output frame: {error:?}"))?;
        if self.output.samples() != BLOCK_FRAMES
            || self.output.pts() != Some(self.pts)
            || self.output.rate() != SAMPLE_RATE
            || self.output.channels() != 2
            || self.output.format() != format::Sample::F64(format::sample::Type::Planar)
        {
            return Err("FFmpeg EQ changed frame timing or format".into());
        }
        for channel in 0..2 {
            for (target, sample) in output.iter_mut().zip(self.output.plane::<f64>(channel)) {
                if !sample.is_finite() || sample.abs() > f64::from(f32::MAX) {
                    return Err("FFmpeg EQ returned non-finite audio".into());
                }
                target[channel] = *sample as f32;
            }
        }
        self.pts = self
            .pts
            .saturating_add(i64::try_from(BLOCK_FRAMES).unwrap_or(i64::MAX));
        std::mem::swap(&mut self.input, &mut self.output);
        Ok(())
    }
}

struct Prepared {
    result: Result<Option<Processor>, String>,
    enabled: bool,
    gain: f64,
}

pub(super) struct Equalizer {
    config: EqualizerConfig,
    processor: Option<Processor>,
    pending: Option<Prepared>,
    retired: [Option<Processor>; 2],
    enabled: bool,
    gain: f64,
    mix: f64,
    target_mix: f64,
    remaining: usize,
    wet: [Frame; BLOCK_FRAMES],
    pre: Frame,
    post: Frame,
    error: Option<String>,
}

impl Default for Equalizer {
    fn default() -> Self {
        Self {
            config: EqualizerConfig::default(),
            processor: None,
            pending: None,
            retired: [None, None],
            enabled: false,
            gain: 1.0,
            mix: 0.0,
            target_mix: 0.0,
            remaining: 0,
            wet: [[0.0; 2]; BLOCK_FRAMES],
            pre: [0.0; 2],
            post: [0.0; 2],
            error: None,
        }
    }
}

impl Equalizer {
    fn bypassed(&self) -> bool {
        !self.config.enabled
            && self.pending.is_none()
            && self.processor.is_none()
            && self.mix <= 0.0
    }

    pub fn needs_prepare(&self, config: EqualizerConfig) -> bool {
        self.config != config
    }

    pub fn configure(
        &mut self,
        config: EqualizerConfig,
        prepared: Option<Result<Option<Processor>, String>>,
    ) -> [Option<Processor>; 3] {
        let [first, second] = std::mem::take(&mut self.retired);
        let config = config.sanitized();
        if config == self.config {
            return [first, second, None];
        }
        let superseded = self
            .pending
            .take()
            .and_then(|pending| pending.result.ok().flatten());
        self.config = config;
        self.pending = Some(Prepared {
            result: prepared.unwrap_or_else(|| Err("EQ was not prepared".into())),
            enabled: config.enabled,
            gain: if config.enabled {
                f64::from(db_to_gain(config.output_db))
            } else {
                1.0
            },
        });
        self.target_mix = 0.0;
        self.remaining = if self.mix > 0.0 { BLOCK_FRAMES } else { 0 };
        [first, second, superseded]
    }

    fn retire(&mut self, processor: Option<Processor>) {
        if let Some(processor) = processor {
            if let Some(slot) = self.retired.iter_mut().find(|slot| slot.is_none()) {
                *slot = Some(processor);
            }
        }
    }

    pub fn process(&mut self, block: &mut [Frame]) {
        if self.remaining == 0 && self.mix <= 0.0 {
            if let Some(prepared) = self.pending.take() {
                let old = self.processor.take();
                self.retire(old);
                self.error = None;
                match prepared.result {
                    Ok(processor) => {
                        self.processor = processor;
                        self.enabled = prepared.enabled;
                        self.gain = prepared.gain;
                    }
                    Err(error) => {
                        self.error = Some(error);
                        self.enabled = false;
                        self.gain = 1.0;
                    }
                }
                self.target_mix = if self.enabled { 1.0 } else { 0.0 };
                self.remaining = if self.enabled { BLOCK_FRAMES } else { 0 };
            }
        }
        if self.bypassed() {
            return;
        }
        if let Some(processor) = &mut self.processor {
            if let Err(error) = processor.render(block, &mut self.wet) {
                self.error = Some(error);
                let failed = self.processor.take();
                self.retire(failed);
                self.enabled = false;
                self.mix = 0.0;
                self.target_mix = 0.0;
                self.remaining = 0;
            }
        }
        let mut pre = [0.0_f32; 2];
        let mut post = [0.0_f32; 2];
        for (index, frame) in block.iter_mut().enumerate() {
            if self.remaining > 0 {
                self.mix += (self.target_mix - self.mix) / self.remaining as f64;
                self.remaining -= 1;
            }
            for channel in 0..2 {
                pre[channel] = pre[channel].max(frame[channel].abs());
                if self.mix > 0.0 || self.config.enabled {
                    let dry = if frame[channel].is_finite() {
                        f64::from(frame[channel])
                    } else {
                        0.0
                    };
                    let wet = if self.processor.is_some() {
                        f64::from(self.wet[index][channel])
                    } else {
                        dry
                    };
                    let value = dry + self.mix * (wet * self.gain - dry);
                    if value.is_finite() && value.abs() <= f64::from(f32::MAX) {
                        frame[channel] = value as f32;
                    } else {
                        frame[channel] = dry as f32;
                        self.error = Some("EQ output exceeds supported range".into());
                        self.enabled = false;
                        self.mix = 0.0;
                        self.target_mix = 0.0;
                        self.remaining = 0;
                    }
                }
                post[channel] = post[channel].max(frame[channel].abs());
            }
        }
        self.pre = std::array::from_fn(|channel| pre[channel].max(self.pre[channel] * 0.97));
        self.post = std::array::from_fn(|channel| post[channel].max(self.post[channel] * 0.97));
    }

    pub fn snapshot(&self) -> EqualizerSnapshot {
        EqualizerSnapshot {
            config: self.config,
            pre_db: self.pre.map(gain_to_db),
            post_db: self.post.map(gain_to_db),
            ready: self.enabled,
            updating: self.pending.is_some() || self.remaining > 0,
            error: self.error.clone(),
        }
    }

    pub fn snapshot_with_bypass(&self, levels: [f32; 2]) -> EqualizerSnapshot {
        let mut snapshot = self.snapshot();
        if self.bypassed() {
            snapshot.pre_db = levels;
            snapshot.post_db = levels;
        }
        snapshot
    }
}

#[cfg(test)]
pub(super) fn fixture_config() -> EqualizerConfig {
    let mut config = EqualizerConfig {
        enabled: true,
        high_pass: true,
        output_db: -3.0,
        ..EqualizerConfig::default()
    };
    config.low_shelf.gain_db = 3.0;
    config.high_shelf.gain_db = 3.0;
    for (band, gain) in config.bands.iter_mut().zip([-3.0, 3.0, -3.0]) {
        band.gain_db = gain;
        band.q = 1.4;
    }
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active(config: EqualizerConfig) -> Equalizer {
        let mut eq = Equalizer::default();
        drop(eq.configure(config, Some(Processor::prepare(config))));
        let mut silence = [[0.0; 2]; BLOCK_FRAMES];
        eq.process(&mut silence);
        assert!(eq.snapshot().error.is_none(), "{:?}", eq.snapshot().error);
        assert!(eq.snapshot().ready);
        eq
    }

    fn tone(block: &mut [Frame], index: usize, frequency: f32) {
        for (offset, frame) in block.iter_mut().enumerate() {
            let phase = std::f64::consts::TAU
                * f64::from(frequency)
                * (index * BLOCK_FRAMES + offset) as f64
                / f64::from(SAMPLE_RATE);
            *frame = [(phase.sin() * 0.05) as f32, (phase.sin() * 0.0125) as f32];
        }
    }

    fn measured_db(config: EqualizerConfig, frequency: f32) -> f32 {
        let mut eq = active(config);
        let mut block = [[0.0; 2]; BLOCK_FRAMES];
        let mut dry = 0.0;
        let mut wet = 0.0;
        for index in 0..300 {
            tone(&mut block, index, frequency);
            if index >= 200 {
                dry += block
                    .iter()
                    .map(|frame| f64::from(frame[0]).powi(2))
                    .sum::<f64>();
            }
            eq.process(&mut block);
            if index >= 200 {
                wet += block
                    .iter()
                    .map(|frame| f64::from(frame[0]).powi(2))
                    .sum::<f64>();
            }
        }
        assert!(eq.snapshot().error.is_none(), "{:?}", eq.snapshot().error);
        (10.0 * (wet / dry).log10()) as f32
    }

    #[test]
    fn disabled_and_enabled_flat_are_bit_exact_without_a_graph() {
        for enabled in [false, true] {
            let config = EqualizerConfig {
                enabled,
                ..EqualizerConfig::default()
            };
            assert!(Processor::prepare(config).unwrap().is_none());
            let mut eq = Equalizer::default();
            drop(eq.configure(config, Some(Ok(None))));
            let mut block = std::array::from_fn::<_, BLOCK_FRAMES, _>(|index| {
                [index as f32 / 1000.0, -0.12345]
            });
            let dry = block;
            eq.process(&mut block);
            assert_eq!(block, dry);
            assert!(eq.processor.is_none());
        }
        let eq = Equalizer::default();
        let snapshot = eq.snapshot_with_bypass([-6.0, -12.0]);
        assert_eq!(snapshot.pre_db, [-6.0, -12.0]);
        assert_eq!(snapshot.post_db, snapshot.pre_db);
    }

    #[test]
    fn bell_centre_gain_cut_and_output_trim_match_the_settings() {
        for gain in [-6.0, 6.0] {
            let mut config = EqualizerConfig {
                enabled: true,
                output_db: -3.0,
                ..EqualizerConfig::default()
            };
            config.bands[1].gain_db = gain;
            config.bands[1].q = 2.0;
            assert!((measured_db(config, 1000.0) - (gain - 3.0)).abs() < 0.01);
        }
    }

    #[test]
    fn high_pass_is_butterworth_twelve_db_per_octave_with_no_extra_delay() {
        let config = EqualizerConfig {
            enabled: true,
            high_pass: true,
            high_pass_hz: 80.0,
            ..EqualizerConfig::default()
        };
        assert!((measured_db(config, 80.0) + 3.0103).abs() < 0.02);
        assert!(measured_db(config, 20.0) < -23.0);
        assert!(measured_db(config, 1000.0).abs() < 0.01);
        let mut processor = Processor::new(config).unwrap();
        let mut impulse = [[0.0; 2]; BLOCK_FRAMES];
        impulse[0] = [0.25; 2];
        let mut output = [[0.0; 2]; BLOCK_FRAMES];
        processor.render(&impulse, &mut output).unwrap();
        assert!(output[0][0] > 0.2);
        assert_eq!(processor.input.pts(), Some(0));
    }

    #[test]
    fn response_graph_matches_the_actual_six_filter_chain() {
        let config = fixture_config();
        for frequency in [40.0, 120.0, 250.0, 1000.0, 4000.0, 8000.0, 16_000.0] {
            let measured = measured_db(config, frequency);
            let plotted = config.response_db(frequency);
            assert!(
                (measured - plotted).abs() < 0.04,
                "{frequency}: measured {measured}, plotted {plotted}"
            );
        }
    }

    #[test]
    fn shelf_plateaus_and_corner_gains_match_their_labels() {
        let mut low = EqualizerConfig {
            enabled: true,
            ..EqualizerConfig::default()
        };
        low.low_shelf.gain_db = 6.0;
        low.low_shelf.frequency_hz = 200.0;
        assert!((measured_db(low, 20.0) - 6.0).abs() < 0.03);
        assert!((measured_db(low, 200.0) - 3.0).abs() < 0.03);
        let mut high = EqualizerConfig {
            enabled: true,
            ..EqualizerConfig::default()
        };
        high.high_shelf.gain_db = -6.0;
        high.high_shelf.frequency_hz = 4000.0;
        assert!((measured_db(high, 4000.0) + 3.0).abs() < 0.03);
        assert!((measured_db(high, 20_000.0) + 6.0).abs() < 0.03);
    }

    #[test]
    fn stereo_channels_are_filtered_independently_with_matching_response() {
        let mut eq = active(fixture_config());
        let mut block = [[0.0; 2]; BLOCK_FRAMES];
        for index in 0..100 {
            tone(&mut block, index, 1000.0);
            eq.process(&mut block);
            assert!(block
                .iter()
                .all(|frame| (frame[1] - frame[0] * 0.25).abs() < 1e-6));
        }
        let mut left_only = active(fixture_config());
        for index in 0..50 {
            tone(&mut block, index, 1000.0);
            for frame in &mut block {
                frame[1] = 0.0;
            }
            left_only.process(&mut block);
            assert!(block.iter().all(|frame| frame[1] == 0.0));
        }
    }

    #[test]
    fn unchanged_settings_preserve_filter_history_and_pending_changes() {
        let config = fixture_config();
        let mut eq = active(config);
        let mut block = [[0.0; 2]; BLOCK_FRAMES];
        tone(&mut block, 0, 80.0);
        eq.process(&mut block);
        let pts = eq.processor.as_ref().unwrap().pts;
        drop(eq.configure(config, None));
        assert_eq!(eq.processor.as_ref().unwrap().pts, pts);
        assert_eq!(eq.remaining, 0);
        let changed = EqualizerConfig {
            output_db: -9.0,
            ..config
        };
        drop(eq.configure(changed, Some(Processor::prepare(changed))));
        drop(eq.configure(changed, None));
        assert!(eq.pending.is_some());
        assert_eq!(eq.remaining, BLOCK_FRAMES);
    }

    #[test]
    fn edits_coalesce_and_fade_through_dry_instead_of_stepping() {
        let mut config = EqualizerConfig {
            enabled: true,
            output_db: 6.0,
            ..EqualizerConfig::default()
        };
        let mut eq = active(config);
        let mut block = [[0.2; 2]; BLOCK_FRAMES];
        eq.process(&mut block);
        let before = block[BLOCK_FRAMES - 1][0];
        config.output_db = -6.0;
        drop(eq.configure(config, Some(Processor::prepare(config))));
        config.output_db = -12.0;
        drop(eq.configure(config, Some(Processor::prepare(config))));
        block.fill([0.2; 2]);
        eq.process(&mut block);
        assert!((block[0][0] - before).abs() < 0.001);
        assert_eq!(block[BLOCK_FRAMES - 1], [0.2; 2]);
        let previous = block[BLOCK_FRAMES - 1][0];
        block.fill([0.2; 2]);
        eq.process(&mut block);
        assert!((block[0][0] - previous).abs() < 0.001);
        assert!(block
            .windows(2)
            .all(|pair| (pair[1][0] - pair[0][0]).abs() < 0.001));
        assert!((block[BLOCK_FRAMES - 1][0] - 0.2 * db_to_gain(-12.0)).abs() < 1e-7);
        assert!(!eq.snapshot().updating);
    }

    #[test]
    fn bypass_releases_processing_and_retires_graphs_for_the_control_thread() {
        let config = fixture_config();
        let mut eq = active(config);
        let bypassed = EqualizerConfig {
            enabled: false,
            ..config
        };
        drop(eq.configure(bypassed, Some(Processor::prepare(bypassed))));
        let mut block = [[0.125; 2]; BLOCK_FRAMES];
        eq.process(&mut block);
        block.fill([0.125; 2]);
        eq.process(&mut block);
        assert_eq!(block, [[0.125; 2]; BLOCK_FRAMES]);
        assert!(eq.processor.is_none());
        let retired = eq.configure(bypassed, None);
        assert!(retired.iter().any(Option::is_some));
        assert!(eq.retired.iter().all(Option::is_none));
    }

    #[test]
    fn init_and_midstream_failures_remain_audible_and_report_dry_fallback() {
        let config = fixture_config();
        let mut eq = Equalizer::default();
        drop(eq.configure(config, Some(Err("missing equalizer filter".into()))));
        let mut block = [[0.125; 2]; BLOCK_FRAMES];
        eq.process(&mut block);
        assert_eq!(block, [[0.125; 2]; BLOCK_FRAMES]);
        assert!(eq.snapshot().error.is_some());
        assert!(!eq.needs_prepare(config));
        drop(eq.configure(EqualizerConfig::default(), Some(Ok(None))));
        eq.process(&mut block);
        assert!(eq.snapshot().error.is_none());
        drop(eq.configure(config, Some(Processor::prepare(config))));
        eq.process(&mut block);
        eq.processor
            .as_mut()
            .unwrap()
            .source
            .source()
            .flush()
            .unwrap();
        block.fill([0.125; 2]);
        eq.process(&mut block);
        assert_eq!(block, [[0.125; 2]; BLOCK_FRAMES]);
        assert!(!eq.snapshot().ready);
        assert!(eq.snapshot().error.is_some());
    }

    #[test]
    fn frame_timestamps_formats_and_pcm_storage_are_reused() {
        let mut processor = Processor::new(fixture_config()).unwrap();
        let pointers = [
            processor.input.data(0).as_ptr(),
            processor.input.data(1).as_ptr(),
        ];
        let mut block = [[0.0; 2]; BLOCK_FRAMES];
        let mut output = block;
        for index in 0..100 {
            tone(&mut block, index, 1000.0);
            processor.render(&block, &mut output).unwrap();
            assert_eq!(processor.input.data(0).as_ptr(), pointers[0]);
            assert_eq!(processor.input.data(1).as_ptr(), pointers[1]);
            assert_eq!(
                processor.pts,
                i64::try_from((index + 1) * BLOCK_FRAMES).unwrap()
            );
        }
        assert!(!processor._graph.dump().contains("auto_aresample"));
    }

    #[test]
    fn invalid_values_fail_closed_and_legacy_config_defaults_off() {
        for value in [f32::NAN, f32::INFINITY, 0.0, 24_000.0] {
            let mut config = fixture_config();
            config.bands[0].frequency_hz = value;
            assert!(config.validate().is_err());
            assert_eq!(config.sanitized(), EqualizerConfig::default());
        }
        for value in [-1.0, 0.0, 10.1, f32::NAN] {
            let mut config = fixture_config();
            config.bands[0].q = value;
            assert!(config.validate().is_err());
        }
        assert_eq!(
            serde_json::from_str::<EqualizerConfig>("{}").unwrap(),
            EqualizerConfig::default()
        );
        let config = fixture_config();
        assert_eq!(
            serde_json::from_str::<EqualizerConfig>(&serde_json::to_string(&config).unwrap())
                .unwrap(),
            config
        );
    }

    #[test]
    fn extreme_valid_settings_and_invalid_samples_stay_finite_and_release_to_silence() {
        for frequency in [20.0, 20_000.0] {
            let mut config = fixture_config();
            config.output_db = 12.0;
            config.low_shelf.gain_db = 12.0;
            config.high_shelf.gain_db = -12.0;
            for band in &mut config.bands {
                band.frequency_hz = frequency;
                band.q = 10.0;
                band.gain_db = 12.0;
            }
            let mut eq = active(config);
            let mut block = [[f32::NAN, f32::INFINITY]; BLOCK_FRAMES];
            eq.process(&mut block);
            assert!(block.iter().flatten().all(|sample| sample.is_finite()));
            for index in 0..100 {
                tone(&mut block, index, frequency);
                eq.process(&mut block);
                assert!(block
                    .iter()
                    .flatten()
                    .all(|sample| sample.is_finite() && sample.abs() < 10_000.0));
            }
            for _ in 0..1000 {
                block.fill([0.0; 2]);
                eq.process(&mut block);
            }
            assert!(block.iter().flatten().all(|sample| sample.abs() < 1e-10));
            assert!(eq.snapshot().error.is_none(), "{:?}", eq.snapshot().error);
        }
    }
}
