use std::ffi::CString;

use ffmpeg_next::{filter, format, frame, ChannelLayout};

use super::reader::{db_to_gain, gain_to_db, Frame};
use super::{BLOCK_FRAMES, SAMPLE_RATE};

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
        for (value, min, max) in [
            (self.threshold_db, -60.0, 0.0),
            (self.ratio, 1.0, 20.0),
            (self.attack_ms, 0.1, 2000.0),
            (self.release_ms, 1.0, 9000.0),
            (self.knee_db, 0.0, 18.0),
            (self.makeup_db, -12.0, 24.0),
        ] {
            if !value.is_finite() || !(min..=max).contains(&value) {
                return Err("Compressor values must be finite and within their supported ranges");
            }
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
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct DynamicsSnapshot {
    pub config: CompressorConfig,
    pub pre_db: [f32; 2],
    pub post_db: [f32; 2],
    pub reduction_db: f32,
    pub ready: bool,
    pub error: Option<String>,
}

impl Default for DynamicsSnapshot {
    fn default() -> Self {
        Self {
            config: CompressorConfig::default(),
            pre_db: [-96.0; 2],
            post_db: [-96.0; 2],
            reduction_db: 0.0,
            ready: false,
            error: None,
        }
    }
}

pub(super) struct Processor {
    graph: filter::Graph,
    source: filter::Context,
    sink: filter::Context,
    input: frame::Audio,
    output: frame::Audio,
    pts: i64,
}

unsafe impl Send for Processor {}

impl Processor {
    pub fn new(config: CompressorConfig) -> Result<Self, String> {
        let mut graph = filter::Graph::new();
        unsafe { (*graph.as_mut_ptr()).nb_threads = 1 };
        let find =
            |name| filter::find(name).ok_or_else(|| format!("FFmpeg filter {name} unavailable"));
        graph
            .add(
                &find("abuffer")?,
                "in",
                "time_base=1/48000:sample_rate=48000:sample_fmt=dbl:channel_layout=stereo",
            )
            .map_err(|error| error.to_string())?;
        graph
            .add(&find("abuffersink")?, "out", "")
            .map_err(|error| error.to_string())?;
        find("acompressor")?;
        graph
            .output("in", 0)
            .and_then(|parser| parser.input("out", 0))
            .and_then(|parser| {
                parser.parse("acompressor@comp=makeup=1:mix=1:link=maximum:detection=peak")
            })
            .map_err(|error| error.to_string())?;
        graph.validate().map_err(|error| error.to_string())?;
        let source = graph.get("in").ok_or("FFmpeg source missing")?;
        let sink = graph.get("out").ok_or("FFmpeg sink missing")?;
        let mut input = frame::Audio::new(
            format::Sample::F64(format::sample::Type::Packed),
            BLOCK_FRAMES,
            ChannelLayout::STEREO,
        );
        input.set_rate(SAMPLE_RATE);
        if input.planes() != 1 {
            return Err("FFmpeg audio buffer allocation failed".into());
        }
        let mut processor = Self {
            graph,
            source,
            sink,
            input,
            output: frame::Audio::empty(),
            pts: 0,
        };
        processor.update(config)?;
        Ok(processor)
    }

    fn update(&mut self, config: CompressorConfig) -> Result<(), String> {
        config.validate().map_err(str::to_owned)?;
        for (name, value) in [
            (c"threshold", f64::from(db_to_gain(config.threshold_db))),
            (c"ratio", f64::from(config.ratio)),
            (c"attack", f64::from(config.attack_ms)),
            (c"release", f64::from(config.release_ms)),
            (c"knee", 10_f64.powf(f64::from(config.knee_db) / 20.0)),
        ] {
            let value = CString::new(format!("{value:.10}")).map_err(|error| error.to_string())?;
            let result = unsafe {
                ffmpeg_next::ffi::avfilter_graph_send_command(
                    self.graph.as_mut_ptr(),
                    c"acompressor@comp".as_ptr(),
                    name.as_ptr(),
                    value.as_ptr(),
                    std::ptr::null_mut(),
                    0,
                    0,
                )
            };
            if result < 0 {
                return Err(ffmpeg_next::Error::from(result).to_string());
            }
        }
        Ok(())
    }

    fn gains(&mut self, input: &[Frame], gains: &mut [f64; BLOCK_FRAMES]) -> Result<(), String> {
        if input.len() != BLOCK_FRAMES {
            return Err("Unexpected compressor block size".into());
        }
        for (target, source) in self.input.plane_mut::<(f64, f64)>(0).iter_mut().zip(input) {
            let finite = |sample: f32| {
                if sample.is_finite() {
                    f64::from(sample)
                } else {
                    0.0
                }
            };
            *target = (finite(source[0]), finite(source[1]));
        }
        self.input.set_pts(Some(self.pts));
        self.source
            .source()
            .add(&self.input)
            .map_err(|error| error.to_string())?;
        self.sink
            .sink()
            .frame(&mut self.output)
            .map_err(|error| error.to_string())?;
        if self.output.samples() != BLOCK_FRAMES
            || self.output.pts() != Some(self.pts)
            || self.output.format() != format::Sample::F64(format::sample::Type::Packed)
            || self.output.channels() != 2
        {
            return Err("FFmpeg compressor changed frame timing or format".into());
        }
        for ((gain, source), wet) in gains
            .iter_mut()
            .zip(input)
            .zip(self.output.plane::<(f64, f64)>(0))
        {
            if !wet.0.is_finite() || !wet.1.is_finite() {
                return Err("FFmpeg compressor returned non-finite audio".into());
            }
            let (dry, processed) = if source[0].abs() >= source[1].abs() {
                (source[0], wet.0)
            } else {
                (source[1], wet.1)
            };
            *gain = if dry.is_finite() && dry.abs() > 1e-20 {
                (processed / f64::from(dry)).clamp(0.0, 1.0)
            } else {
                1.0
            };
            if !gain.is_finite() {
                return Err("FFmpeg compressor returned non-finite audio".into());
            }
        }
        self.pts = self
            .pts
            .saturating_add(i64::try_from(BLOCK_FRAMES).unwrap_or(i64::MAX));
        std::mem::swap(&mut self.input, &mut self.output);
        Ok(())
    }
}

pub(super) struct Dynamics {
    config: CompressorConfig,
    processor: Option<Processor>,
    error: Option<String>,
    compression_gain: f64,
    makeup_gain: f64,
    transition: usize,
    gains: [f64; BLOCK_FRAMES],
    pre: Frame,
    post: Frame,
    reduction_db: f32,
}

impl Default for Dynamics {
    fn default() -> Self {
        Self {
            config: CompressorConfig::default(),
            processor: None,
            error: None,
            compression_gain: 1.0,
            makeup_gain: 1.0,
            transition: 0,
            gains: [1.0; BLOCK_FRAMES],
            pre: [0.0; 2],
            post: [0.0; 2],
            reduction_db: 0.0,
        }
    }
}

impl Dynamics {
    pub fn needs_prepare(&self, config: CompressorConfig) -> bool {
        config.enabled && self.processor.is_none() && self.config != config
    }

    pub fn configure(
        &mut self,
        config: CompressorConfig,
        prepared: Option<Result<Processor, String>>,
    ) {
        let config = config.sanitized();
        if config == self.config {
            return;
        }
        if let Some(prepared) = prepared {
            match prepared {
                Ok(processor) => {
                    self.processor = Some(processor);
                    self.error = None;
                }
                Err(error) => {
                    self.processor = None;
                    self.error = Some(error);
                }
            }
        } else if let Some(processor) = &mut self.processor {
            if let Err(error) = processor.update(config) {
                self.processor = None;
                self.error = Some(error);
            }
        }
        self.config = config;
        if !config.enabled {
            self.error = None;
        }
        self.transition = 960;
    }

    pub fn process(&mut self, block: &mut [Frame]) {
        let mut pre = [0.0_f32; 2];
        let mut post = [0.0_f32; 2];
        let mut minimum_gain = 1.0_f64;
        if let Some(processor) = &mut self.processor {
            if let Err(error) = processor.gains(block, &mut self.gains) {
                self.error = Some(error);
                self.processor = None;
                self.transition = 960;
            }
        }
        let active = self.config.enabled && self.processor.is_some();
        let makeup = if active {
            f64::from(db_to_gain(self.config.makeup_db))
        } else {
            1.0
        };
        for (position, frame) in block.iter_mut().enumerate() {
            let audible = frame
                .iter()
                .any(|sample| sample.is_finite() && sample.abs() > 1e-20);
            let target = if active { self.gains[position] } else { 1.0 };
            if self.transition > 0 {
                self.compression_gain += (target - self.compression_gain) / self.transition as f64;
                self.makeup_gain += (makeup - self.makeup_gain) / self.transition as f64;
                self.transition -= 1;
            } else {
                self.compression_gain = target;
                self.makeup_gain = makeup;
            }
            for channel in 0..2 {
                pre[channel] = pre[channel].max(frame[channel].abs());
                if self.processor.is_some() || self.transition > 0 {
                    let sample = if frame[channel].is_finite() {
                        f64::from(frame[channel])
                    } else {
                        0.0
                    };
                    frame[channel] = (sample * self.compression_gain * self.makeup_gain) as f32;
                }
                post[channel] = post[channel].max(frame[channel].abs());
            }
            if audible {
                minimum_gain = minimum_gain.min(self.compression_gain);
            }
        }
        self.pre = std::array::from_fn(|channel| pre[channel].max(self.pre[channel] * 0.97));
        self.post = std::array::from_fn(|channel| post[channel].max(self.post[channel] * 0.97));
        self.reduction_db = (-gain_to_db(minimum_gain as f32)).max(self.reduction_db * 0.85);
    }

    pub fn snapshot(&self) -> DynamicsSnapshot {
        DynamicsSnapshot {
            config: self.config,
            pre_db: self.pre.map(gain_to_db),
            post_db: self.post.map(gain_to_db),
            reduction_db: self.reduction_db,
            ready: self.processor.is_some(),
            error: self.error.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> CompressorConfig {
        CompressorConfig {
            enabled: true,
            threshold_db: 20.0 * 0.125_f32.log10(),
            ratio: 4.0,
            attack_ms: 0.1,
            release_ms: 10.0,
            knee_db: 0.0,
            ..CompressorConfig::default()
        }
    }

    fn enabled(config: CompressorConfig) -> Dynamics {
        let mut dynamics = Dynamics::default();
        dynamics.configure(config, Some(Processor::new(config)));
        assert!(dynamics.snapshot().ready, "{:?}", dynamics.snapshot().error);
        dynamics
    }

    fn settle(dynamics: &mut Dynamics) -> Vec<Frame> {
        let mut block = vec![[0.5, -0.25]; BLOCK_FRAMES];
        for _ in 0..100 {
            block.fill([0.5, -0.25]);
            dynamics.process(&mut block);
        }
        block
    }

    #[test]
    fn disabled_is_bit_exact_and_creates_no_filter() {
        let mut dynamics = Dynamics::default();
        let mut block: Vec<_> = (0..BLOCK_FRAMES)
            .map(|index| [index as f32 / 1000.0, -0.125])
            .collect();
        let dry = block.clone();
        dynamics.process(&mut block);
        assert_eq!(block, dry);
        assert!(!dynamics.snapshot().ready);
        assert_eq!(dynamics.snapshot().reduction_db, 0.0);
    }

    #[test]
    fn static_curve_stereo_link_and_meters_match_the_reference() {
        let mut dynamics = enabled(settings());
        let block = settle(&mut dynamics);
        let expected = 0.125_f32 * 4.0_f32.powf(0.25);
        for frame in block {
            assert!((frame[0] - expected).abs() < 1e-5, "{frame:?}");
            assert!((frame[1] + expected * 0.5).abs() < 1e-5);
        }
        let snapshot = dynamics.snapshot();
        assert!((snapshot.pre_db[0] - gain_to_db(0.5)).abs() < 1e-4);
        assert!((snapshot.post_db[0] - gain_to_db(expected)).abs() < 0.1);
        assert!((snapshot.reduction_db - 9.0309).abs() < 0.01);
    }

    #[test]
    fn makeup_does_not_change_the_detector_or_reduction_meter() {
        let mut dynamics = enabled(CompressorConfig {
            makeup_db: 6.0,
            ..settings()
        });
        let block = settle(&mut dynamics);
        assert!((block[BLOCK_FRAMES - 1][0] - 0.1767767 * db_to_gain(6.0)).abs() < 1e-5);
        assert!((dynamics.snapshot().reduction_db - 9.0309).abs() < 0.01);
    }

    #[test]
    fn attack_and_release_advance_across_blocks() {
        let mut fast = Processor::new(settings()).unwrap();
        let mut slow = Processor::new(CompressorConfig {
            attack_ms: 2000.0,
            ..settings()
        })
        .unwrap();
        let loud = vec![[0.5; 2]; BLOCK_FRAMES];
        let mut fast_gain = [1.0; BLOCK_FRAMES];
        let mut slow_gain = [1.0; BLOCK_FRAMES];
        fast.gains(&loud, &mut fast_gain).unwrap();
        slow.gains(&loud, &mut slow_gain).unwrap();
        assert!(fast_gain[BLOCK_FRAMES - 1] < 0.4);
        assert!(slow_gain[BLOCK_FRAMES - 1] > 0.99);
        let mut release = Processor::new(CompressorConfig {
            release_ms: 100.0,
            ..settings()
        })
        .unwrap();
        for _ in 0..20 {
            release.gains(&loud, &mut fast_gain).unwrap();
        }
        let quiet = vec![[0.01; 2]; BLOCK_FRAMES];
        release.gains(&quiet, &mut fast_gain).unwrap();
        assert!(fast_gain[0] < 0.8);
        for _ in 0..100 {
            release.gains(&quiet, &mut fast_gain).unwrap();
        }
        assert!(fast_gain[BLOCK_FRAMES - 1] > 0.99);
    }

    #[test]
    fn makeup_edits_ramp_instead_of_stepping() {
        let config = CompressorConfig {
            ratio: 1.0,
            ..settings()
        };
        let mut dynamics = enabled(config);
        settle(&mut dynamics);
        dynamics.configure(
            CompressorConfig {
                makeup_db: 12.0,
                ..config
            },
            None,
        );
        let mut block = vec![[0.5, -0.25]; BLOCK_FRAMES];
        dynamics.process(&mut block);
        assert!((block[0][0] - 0.5).abs() < 0.002);
        assert!(block
            .windows(2)
            .all(|pair| (pair[1][0] - pair[0][0]).abs() < 0.002));
        block.fill([0.5, -0.25]);
        dynamics.process(&mut block);
        assert!((block[BLOCK_FRAMES - 1][0] - 0.5 * db_to_gain(12.0)).abs() < 1e-5);
    }

    #[test]
    fn bypass_ramps_to_exact_dry_and_unchanged_settings_preserve_history() {
        let mut dynamics = enabled(settings());
        let before = settle(&mut dynamics)[BLOCK_FRAMES - 1][0];
        let pts = dynamics.processor.as_ref().unwrap().pts;
        dynamics.configure(settings(), None);
        assert_eq!(dynamics.processor.as_ref().unwrap().pts, pts);
        assert_eq!(dynamics.transition, 0);
        dynamics.configure(
            CompressorConfig {
                enabled: false,
                ..settings()
            },
            None,
        );
        let mut block = vec![[0.5, -0.25]; BLOCK_FRAMES];
        dynamics.process(&mut block);
        assert!((block[0][0] - before).abs() < 0.001);
        assert!(block
            .windows(2)
            .all(|pair| (pair[1][0] - pair[0][0]).abs() < 0.001));
        block.fill([0.5, -0.25]);
        dynamics.process(&mut block);
        assert_eq!(block[BLOCK_FRAMES - 1], [0.5, -0.25]);
    }

    #[test]
    fn initialization_failure_is_visible_and_dry_and_can_be_retried() {
        let config = settings();
        let mut dynamics = Dynamics::default();
        dynamics.configure(config, Some(Err("filter unavailable".into())));
        let mut block = vec![[0.5, -0.25]; BLOCK_FRAMES];
        dynamics.process(&mut block);
        assert_eq!(block, vec![[0.5, -0.25]; BLOCK_FRAMES]);
        assert_eq!(
            dynamics.snapshot().error.as_deref(),
            Some("filter unavailable")
        );
        dynamics.configure(
            CompressorConfig {
                enabled: false,
                ..config
            },
            None,
        );
        assert!(dynamics.snapshot().error.is_none());
        assert!(dynamics.needs_prepare(config));
        dynamics.configure(config, Some(Processor::new(config)));
        assert!(dynamics.snapshot().ready);
    }

    #[test]
    fn midstream_failure_ramps_to_dry_without_silencing_the_source() {
        let mut dynamics = enabled(settings());
        let before = settle(&mut dynamics)[BLOCK_FRAMES - 1][0];
        dynamics
            .processor
            .as_mut()
            .unwrap()
            .source
            .source()
            .flush()
            .unwrap();
        let mut block = vec![[0.5, -0.25]; BLOCK_FRAMES];
        dynamics.process(&mut block);
        assert!(dynamics.snapshot().error.is_some());
        assert!(!dynamics.snapshot().ready);
        assert!((block[0][0] - before).abs() < 0.001);
        block.fill([0.5, -0.25]);
        dynamics.process(&mut block);
        assert_eq!(block[BLOCK_FRAMES - 1], [0.5, -0.25]);
    }

    #[test]
    fn silence_and_invalid_samples_do_not_poison_the_filter() {
        let mut dynamics = enabled(settings());
        let mut block = vec![[f32::NAN, f32::INFINITY]; BLOCK_FRAMES];
        dynamics.process(&mut block);
        assert!(block.iter().flatten().all(|sample| sample.is_finite()));
        block.fill([0.0, 0.0]);
        for _ in 0..100 {
            dynamics.process(&mut block);
        }
        assert_eq!(block, vec![[0.0, 0.0]; BLOCK_FRAMES]);
        assert!(dynamics.snapshot().reduction_db < 0.01);
        assert!(dynamics.snapshot().error.is_none());
    }

    #[test]
    fn frame_timing_and_pcm_storage_are_reused() {
        let mut processor = Processor::new(settings()).unwrap();
        let pointer = processor.input.data(0).as_ptr();
        let block = vec![[0.5, -0.25]; BLOCK_FRAMES];
        let mut gains = [1.0; BLOCK_FRAMES];
        for index in 1..=100 {
            processor.gains(&block, &mut gains).unwrap();
            assert_eq!(processor.pts, index * i64::try_from(BLOCK_FRAMES).unwrap());
            assert_eq!(processor.input.data(0).as_ptr(), pointer);
        }
    }

    #[test]
    fn invalid_saved_config_fails_closed_and_old_config_defaults_off() {
        for value in [f32::NAN, f32::INFINITY, -100.0, 1.0] {
            let config = CompressorConfig {
                threshold_db: value,
                ..settings()
            };
            assert!(config.validate().is_err());
            assert_eq!(config.sanitized(), CompressorConfig::default());
        }
        assert_eq!(
            serde_json::from_str::<CompressorConfig>("{}").unwrap(),
            CompressorConfig::default()
        );
        let config = settings();
        assert_eq!(
            serde_json::from_str::<CompressorConfig>(&serde_json::to_string(&config).unwrap())
                .unwrap(),
            config
        );
    }
}
