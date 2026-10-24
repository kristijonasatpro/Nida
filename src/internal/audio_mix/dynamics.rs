//! A source's and a bus's processing chains, built from Nida's own DSP
//! (`dsp.rs`), in the order a conference needs them.
//!
//! A source: low cut (the rumble goes first, so nothing after it reacts to
//! it), then the leveler (each speaker brought to the same loudness), then
//! the compressor (what is left evened out — after the leveler, so it acts
//! alike on a soft speaker and a loud one). A bus: its compressor before its
//! fader, then — after the fader — the limiter, the last thing before every
//! recording, stream and screen, and the loudness meter, which measures what
//! actually goes out.
//!
//! The panel and meters these feed came with pull request #11 (Augustas,
//! pingvis); its FFmpeg compressor gave way to `dsp.rs`.

use super::dsp::{
    Compressor, CompressorConfig, Leveler, LevelerConfig, Limiter, LimiterConfig, LoudnessMeter,
    LoudnessSnapshot, LowCut, LowCutConfig,
};
use super::reader::{gain_to_db, Frame};

/// Meter fall per block, as the strips' own meters fall.
const METER_DECAY: f32 = 0.97;

/// A strip's processing as the panel shows it: its settings, and what it is
/// doing now.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct DynamicsSnapshot {
    /// The compressor's settings.
    pub config: CompressorConfig,
    /// A source's low cut and leveler (`None` on a bus).
    pub low_cut: Option<LowCutConfig>,
    pub leveler: Option<LevelerConfig>,
    /// A bus's limiter (`None` on a source).
    pub limiter: Option<LimiterConfig>,
    /// Stereo sample peaks before and after the processing, dBFS.
    pub pre_db: [f32; 2],
    pub post_db: [f32; 2],
    /// The compressor's gain reduction now, dB (≥ 0).
    pub reduction_db: f32,
    /// The gain the leveler applies now, dB.
    pub leveler_gain_db: f32,
    /// The limiter's gain reduction now, dB (≥ 0).
    pub limiter_reduction_db: f32,
    /// A bus's loudness, LUFS (`None` on a source).
    pub loudness: Option<LoudnessSnapshot>,
}

impl Default for DynamicsSnapshot {
    fn default() -> Self {
        Self {
            config: CompressorConfig::default(),
            low_cut: None,
            leveler: None,
            limiter: None,
            pre_db: [-96.0; 2],
            post_db: [-96.0; 2],
            reduction_db: 0.0,
            leveler_gain_db: 0.0,
            limiter_reduction_db: 0.0,
            loudness: None,
        }
    }
}

fn block_peak(block: &[Frame]) -> Frame {
    let mut peak = [0.0_f32; 2];
    for frame in block {
        for (p, s) in peak.iter_mut().zip(frame) {
            if s.is_finite() {
                *p = p.max(s.abs());
            }
        }
    }
    peak
}

fn held(meter: Frame, peak: Frame) -> Frame {
    std::array::from_fn(|c| peak[c].max(meter[c] * METER_DECAY))
}

/// A source's chain: low cut → leveler → compressor.
#[derive(Debug, Clone, Default)]
pub struct SourceChain {
    low_cut: LowCut,
    leveler: Leveler,
    compressor: Compressor,
    low_cut_config: LowCutConfig,
    leveler_config: LevelerConfig,
    compressor_config: CompressorConfig,
    pre: Frame,
    post: Frame,
}

impl SourceChain {
    pub fn configure(
        &mut self,
        low_cut: LowCutConfig,
        leveler: LevelerConfig,
        compressor: CompressorConfig,
    ) {
        let (low_cut, leveler, compressor) = (
            low_cut.sanitized(),
            leveler.sanitized(),
            compressor.sanitized(),
        );
        self.low_cut.configure(low_cut);
        self.leveler.configure(leveler);
        self.compressor.configure(compressor);
        self.low_cut_config = low_cut;
        self.leveler_config = leveler;
        self.compressor_config = compressor;
    }

    /// The mixer stalled: what the filters and the compressor remembered is
    /// stale. The leveler holds, as through any pause.
    pub fn skip(&mut self) {
        self.low_cut.clear();
        self.compressor.release();
    }

    pub fn process(&mut self, block: &mut [Frame]) {
        self.pre = held(self.pre, block_peak(block));
        self.low_cut.process(block);
        self.leveler.process(block);
        self.compressor.process(block);
        self.post = held(self.post, block_peak(block));
    }

    pub fn snapshot(&self) -> DynamicsSnapshot {
        DynamicsSnapshot {
            config: self.compressor_config,
            low_cut: Some(self.low_cut_config),
            leveler: Some(self.leveler_config),
            limiter: None,
            pre_db: self.pre.map(gain_to_db),
            post_db: self.post.map(gain_to_db),
            reduction_db: self.compressor.reduction_db(),
            leveler_gain_db: self.leveler.gain_db(),
            limiter_reduction_db: 0.0,
            loudness: None,
        }
    }
}

/// A bus's chain: compressor before its fader; limiter and loudness meter
/// after it.
#[derive(Debug, Clone, Default)]
pub struct BusChain {
    compressor: Compressor,
    limiter: Limiter,
    loudness: LoudnessMeter,
    compressor_config: CompressorConfig,
    limiter_config: LimiterConfig,
    pre: Frame,
    post: Frame,
}

impl BusChain {
    pub fn configure(&mut self, compressor: CompressorConfig, limiter: LimiterConfig) {
        let (compressor, limiter) = (compressor.sanitized(), limiter.sanitized());
        self.compressor.configure(compressor);
        self.limiter.configure(limiter);
        self.compressor_config = compressor;
        self.limiter_config = limiter;
    }

    /// Before the bus's fader: its compressor.
    pub fn process_pre(&mut self, block: &mut [Frame]) {
        self.pre = held(self.pre, block_peak(block));
        self.compressor.process(block);
    }

    /// After the bus's fader: the limiter, then the loudness of what goes out.
    pub fn process_post(&mut self, block: &mut [Frame]) {
        self.limiter.process(block);
        self.loudness.measure(block);
        self.post = held(self.post, block_peak(block));
    }

    /// Start the loudness readings again.
    pub fn reset_loudness(&mut self) {
        self.loudness.reset();
    }

    /// The mixer stalled for `blocks` blocks: what the limiter held is
    /// dropped (the time it stood for is gone), and the loudness windows
    /// count the gap as silence, as the recordings do.
    pub fn skip(&mut self, blocks: usize) {
        self.compressor.release();
        self.limiter.clear();
        self.loudness.skip(blocks);
    }

    /// As new, in place: no allocation, so it can be done with the mixer
    /// held (a show loaded).
    pub fn reset(&mut self) {
        // Everything but the loudness tables is plain data: made anew. The
        // settings come back with the show's own (`configure`).
        self.compressor = Compressor::default();
        self.limiter = Limiter::default();
        self.loudness.restart();
        self.pre = [0.0; 2];
        self.post = [0.0; 2];
    }

    pub fn snapshot(&self) -> DynamicsSnapshot {
        DynamicsSnapshot {
            config: self.compressor_config,
            low_cut: None,
            leveler: None,
            limiter: Some(self.limiter_config),
            pre_db: self.pre.map(gain_to_db),
            post_db: self.post.map(gain_to_db),
            reduction_db: self.compressor.reduction_db(),
            leveler_gain_db: 0.0,
            limiter_reduction_db: self.limiter.reduction_db(),
            loudness: Some(self.loudness.snapshot()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::dsp::LIMITER_LOOKAHEAD;
    use super::super::reader::db_to_gain;
    use super::super::BLOCK_FRAMES;
    use super::*;

    fn tone(amplitude: f32, hz: f32, at: usize) -> Vec<Frame> {
        (0..BLOCK_FRAMES)
            .map(|i| {
                let t = (at * BLOCK_FRAMES + i) as f32 / 48_000.0;
                let v = amplitude * (std::f32::consts::TAU * hz * t).sin();
                [v, v]
            })
            .collect()
    }

    #[test]
    fn a_new_chain_passes_sound_through_untouched() {
        let mut source = SourceChain::default();
        let mut block = tone(0.5, 440.0, 0);
        let before = block.clone();
        source.process(&mut block);
        assert_eq!(block, before);
        let snapshot = source.snapshot();
        assert!(!snapshot.config.enabled);
        assert!(snapshot.low_cut.is_some_and(|l| !l.enabled));
        assert!(snapshot.leveler.is_some_and(|l| !l.enabled));
        assert!(snapshot.limiter.is_none() && snapshot.loudness.is_none());
    }

    #[test]
    fn a_source_cuts_rumble_before_the_leveler_hears_it() {
        // Loud 40 Hz rumble under a quiet voice: with the low cut first, the
        // leveler hears the voice and turns it up; were the rumble left in, it
        // would take the rumble for a loud voice and turn everything down.
        let mut chain = SourceChain::default();
        chain.configure(
            LowCutConfig {
                enabled: true,
                ..LowCutConfig::default()
            },
            LevelerConfig {
                enabled: true,
                ..LevelerConfig::default()
            },
            CompressorConfig::default(),
        );
        for at in 0..1500 {
            let mut block: Vec<Frame> = tone(0.4, 40.0, at)
                .iter()
                .zip(tone(0.02, 500.0, at))
                .map(|(r, v)| [r[0] + v[0], r[1] + v[1]])
                .collect();
            chain.process(&mut block);
        }
        assert!(
            chain.snapshot().leveler_gain_db > 3.0,
            "{}",
            chain.snapshot().leveler_gain_db
        );
    }

    #[test]
    fn a_bus_limits_after_its_fader_and_measures_what_goes_out() {
        let mut bus = BusChain::default();
        bus.configure(CompressorConfig::default(), LimiterConfig::default());
        let ceiling = db_to_gain(-1.0);
        for at in 0..400 {
            let mut block = tone(0.7, 1000.0, at);
            bus.process_pre(&mut block);
            // A fader pushed +6 dB: over full scale until the limiter.
            for frame in &mut block {
                frame[0] *= 2.0;
                frame[1] *= 2.0;
            }
            bus.process_post(&mut block);
            if at > 0 {
                assert!(block.iter().all(|f| f[0].abs() <= ceiling + 1e-6));
            }
        }
        let snapshot = bus.snapshot();
        assert!(snapshot.limiter_reduction_db > 3.0, "{snapshot:?}");
        let loudness = snapshot.loudness.expect("a bus is measured");
        // Limited to −1 dBFS peaks: about −1 LUFS for a sine, never more.
        assert!(loudness.momentary_lufs < -0.5, "{loudness:?}");
        assert!(loudness.integrated_lufs.is_some());
        bus.reset_loudness();
        assert_eq!(bus.snapshot().loudness.unwrap().integrated_lufs, None);
        let _ = LIMITER_LOOKAHEAD;
    }

    #[test]
    fn a_show_loaded_starts_a_bus_afresh() {
        // Review: a reset that kept the compressor's makeup and mix let the
        // first block after loading a show without compression come out at
        // 1.57 for 0.1.
        let mut bus = BusChain::default();
        bus.configure(
            CompressorConfig {
                enabled: true,
                makeup_db: 24.0,
                ..CompressorConfig::default()
            },
            LimiterConfig {
                enabled: false,
                ..LimiterConfig::default()
            },
        );
        for at in 0..20 {
            let mut block = tone(0.1, 440.0, at);
            bus.process_pre(&mut block);
        }
        bus.reset();
        bus.configure(CompressorConfig::default(), LimiterConfig::default());
        let mut block = tone(0.1, 440.0, 20);
        let before = block.clone();
        bus.process_pre(&mut block);
        assert_eq!(block, before, "the compressor came back as new");
    }

    #[test]
    fn saved_settings_out_of_range_fall_back_to_safe_ones() {
        let mut chain = SourceChain::default();
        chain.configure(
            LowCutConfig {
                enabled: true,
                hz: 5_000.0,
                slope_db: 24,
            },
            LevelerConfig {
                enabled: true,
                target_db: f32::NAN,
                ..LevelerConfig::default()
            },
            CompressorConfig {
                enabled: true,
                ratio: 0.0,
                ..CompressorConfig::default()
            },
        );
        let snapshot = chain.snapshot();
        assert_eq!(snapshot.low_cut, Some(LowCutConfig::default()));
        assert_eq!(snapshot.leveler, Some(LevelerConfig::default()));
        assert_eq!(snapshot.config, CompressorConfig::default());
    }
}
