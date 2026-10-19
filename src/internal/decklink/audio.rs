//! Embedded stereo PCM, read nonblocking against the output's 48 kHz clock.

use crate::audio::PcmChunk;
use std::time::Instant;

/// A post-fader, post-mute, post-delay mixer-bus tap: stereo at 48 kHz.
pub struct AudioInput {
    pub receiver: crossbeam_channel::Receiver<PcmChunk>,
    pub opened: Instant,
}

#[cfg(any(target_os = "macos", target_os = "windows", test))]
pub(super) struct Pull {
    input: AudioInput,
    origin: Option<Instant>,
    position: u64,
    gap: u64,
    chunk: Vec<f32>,
    offset: usize,
}

#[cfg(any(target_os = "macos", target_os = "windows", test))]
impl Pull {
    pub(super) fn new(input: AudioInput) -> Self {
        Self {
            input,
            origin: None,
            position: 0,
            gap: 0,
            chunk: Vec::new(),
            offset: 0,
        }
    }

    pub(super) fn begin(&mut self, now: Instant, preroll_frames: u32) {
        if self.origin.is_none() {
            let delay = std::time::Duration::from_secs_f64(f64::from(preroll_frames) / 48_000.0);
            self.origin = Some(now.checked_sub(delay).unwrap_or(now));
        }
    }

    fn next(&mut self) -> bool {
        if self.gap > 0 || self.offset < self.chunk.len() {
            return true;
        }
        let Ok(chunk) = self.input.receiver.try_recv() else {
            return false;
        };
        self.gap = chunk.lost_before;
        self.chunk = chunk.samples;
        self.offset = 0;
        true
    }

    /// `frame` is an absolute hardware sample-frame timestamp. Source gaps
    /// occupy their real positions; underflow is silence and late samples are
    /// discarded on the next read, never replayed late or accumulated forever.
    pub(super) fn read(&mut self, frame: i64, out: &mut [i32]) {
        out.fill(0);
        let Some(origin) = self.origin else {
            return;
        };
        let difference = if origin >= self.input.opened {
            i128::try_from(origin.duration_since(self.input.opened).as_nanos()).unwrap_or(i128::MAX)
        } else {
            -i128::try_from(self.input.opened.duration_since(origin).as_nanos())
                .unwrap_or(i128::MAX)
        };
        // Convert time once at the read boundary; do not accumulate a rounded
        // "samples per video frame" at 29.97/59.94 Hz.
        let start = difference.saturating_mul(48_000) / 1_000_000_000 + i128::from(frame);
        let start = start.saturating_mul(2);
        let mut reads = 0;
        for (index, dst) in out.iter_mut().enumerate() {
            let wanted = start.saturating_add(index as i128);
            if wanted < 0 {
                continue;
            }
            let wanted = u64::try_from(wanted).unwrap_or(u64::MAX);
            loop {
                if self.gap == 0 && self.offset == self.chunk.len() {
                    reads += 1;
                    if reads > 512 || !self.next() {
                        break;
                    }
                    if self.gap == 0 && self.offset == self.chunk.len() {
                        continue;
                    }
                }
                if self.position < wanted {
                    let skip = wanted - self.position;
                    let taken = if self.gap > 0 {
                        let n = skip.min(self.gap);
                        self.gap -= n;
                        n
                    } else {
                        let n = skip.min((self.chunk.len() - self.offset) as u64);
                        self.offset += n as usize;
                        n
                    };
                    self.position += taken;
                    continue;
                }
                if self.position > wanted {
                    break;
                }
                if self.gap > 0 {
                    self.gap -= 1;
                } else {
                    let sample = self.chunk[self.offset];
                    self.offset += 1;
                    if sample.is_finite() {
                        *dst = (sample.clamp(-1.0, 1.0) * 2_147_483_648.0).round() as i32;
                    }
                }
                self.position += 1;
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input() -> (crossbeam_channel::Sender<PcmChunk>, Pull, Instant) {
        let (tx, receiver) = crossbeam_channel::bounded(8);
        let opened = Instant::now();
        let mut pull = Pull::new(AudioInput { receiver, opened });
        pull.begin(opened, 0);
        (tx, pull, opened)
    }
    #[test]
    fn stereo_order_clipping_and_nonfinite_samples() {
        let (tx, mut pull, _) = input();
        tx.send(PcmChunk {
            samples: vec![1.0, -1.0, 0.5, -0.5, f32::NAN, f32::INFINITY],
            lost_before: 0,
        })
        .unwrap();
        let mut out = [0; 6];
        pull.read(0, &mut out);
        assert_eq!(
            out,
            [i32::MAX, i32::MIN, 1_073_741_824, -1_073_741_824, 0, 0]
        );
    }
    #[test]
    fn loss_is_silence_before_its_chunk() {
        let (tx, mut pull, _) = input();
        tx.send(PcmChunk {
            samples: vec![0.5; 2],
            lost_before: 0,
        })
        .unwrap();
        tx.send(PcmChunk {
            samples: vec![1.0; 2],
            lost_before: 2,
        })
        .unwrap();
        let mut out = [0; 6];
        pull.read(0, &mut out);
        assert_eq!(
            out,
            [1_073_741_824, 1_073_741_824, 0, 0, i32::MAX, i32::MAX]
        );
    }
    #[test]
    fn underrun_does_not_replay_late_audio() {
        let (tx, mut pull, _) = input();
        let mut out = [1; 4];
        pull.read(0, &mut out);
        assert_eq!(out, [0; 4]);
        tx.send(PcmChunk {
            samples: vec![0.5; 4],
            lost_before: 0,
        })
        .unwrap();
        tx.send(PcmChunk {
            samples: vec![1.0; 4],
            lost_before: 0,
        })
        .unwrap();
        pull.read(2, &mut out);
        assert_eq!(out, [i32::MAX; 4]);
    }
    #[test]
    fn preroll_and_open_time_share_one_timeline() {
        let (tx, receiver) = crossbeam_channel::bounded(1);
        let opened = Instant::now();
        let mut pull = Pull::new(AudioInput { receiver, opened });
        pull.begin(opened, 2400);
        tx.send(PcmChunk {
            samples: vec![1.0; 4],
            lost_before: 0,
        })
        .unwrap();
        let mut out = [1; 4];
        pull.read(0, &mut out);
        assert_eq!(out, [0; 4]);
        pull.read(2400, &mut out);
        assert_eq!(out, [i32::MAX; 4]);
    }
    #[test]
    fn long_timestamp_gaps_are_bounded_and_do_not_allocate_silence() {
        let (tx, mut pull, _) = input();
        tx.send(PcmChunk {
            samples: vec![1.0; 2],
            lost_before: 9_600_000_000,
        })
        .unwrap();
        let mut out = [1; 4];
        pull.read(0, &mut out);
        assert_eq!(out, [0; 4]);
        pull.read(4_800_000_000, &mut out);
        assert_eq!(out, [i32::MAX, i32::MAX, 0, 0]);
    }
}
