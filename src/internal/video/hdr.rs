//! HDR clips shown in Nida's SDR picture: HDR10 (PQ) and HLG — what an
//! iPhone records by default. Read as SDR they come out dim (PQ puts SDR's
//! white at about half signal) and flat (their BT.2020 colours squeezed
//! into BT.709's).
//!
//! Each frame, decoded to 16-bit RGB still in its transfer, is taken to
//! light — in SDR-relative units, 1.0 the 203 nits BT.2408 makes reference
//! white — its colours to BT.709, its highlights rolled off to fit under
//! SDR's white, and encoded sRGB, as any clip's frames are.

extern crate ffmpeg_next as ffmpeg;

use ffmpeg::util::color::TransferCharacteristic;
use ffmpeg::util::frame::video::Video;

/// An HDR transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transfer {
    /// SMPTE ST 2084, HDR10: signal is absolute light, to 10 000 nits.
    Pq,
    /// ARIB STD-B67: signal is scene light, relative.
    Hlg,
}

impl Transfer {
    /// The HDR transfer a frame is tagged with; `None` for SDR.
    pub fn of(frame: &Video) -> Option<Self> {
        match frame.color_transfer_characteristic() {
            TransferCharacteristic::SMPTE2084 => Some(Self::Pq),
            TransferCharacteristic::ARIB_STD_B67 => Some(Self::Hlg),
            _ => None,
        }
    }
}

/// SDR's reference white, in nits (BT.2408).
const REFERENCE_WHITE: f32 = 203.0;
/// The display HDR is taken to be mastered for: HLG's nominal peak, and
/// HDR10's most common.
const PEAK: f32 = 1000.0;
/// Light up to this (SDR-relative) is left as it is; above it, rolled off.
const KNEE: f32 = 0.75;
/// Entries in the tables indexed by light from 0 to 1.
const TABLE: usize = 4096;

/// BT.2020 to BT.709 primaries, in linear light.
const TO_709: [[f32; 3]; 3] = [
    [1.660_491, -0.587_641, -0.072_850],
    [-0.124_550, 1.132_9, -0.008_349],
    [-0.018_151, -0.100_579, 1.118_73],
];

/// Takes one transfer's frames to SDR.
pub struct ToneMap {
    transfer: Transfer,
    /// 16-bit signal to light: for PQ, SDR-relative; for HLG, scene light
    /// from 0 to 1.
    signal: Vec<f32>,
    /// HLG: scene luminance (0 to 1) to the power the display applies to
    /// it (its OOTF: gamma 1.2 at 1000 nits), less one.
    ootf: Vec<f32>,
    /// SDR-relative light from 0 to 1, to sRGB.
    srgb: Vec<u8>,
}

impl ToneMap {
    pub fn new(transfer: Transfer) -> Self {
        let signal = (0..=u16::MAX)
            .map(|code| {
                let v = f32::from(code) / f32::from(u16::MAX);
                match transfer {
                    Transfer::Pq => pq_to_nits(v) / REFERENCE_WHITE,
                    Transfer::Hlg => hlg_to_scene(v),
                }
            })
            .collect();
        let ootf = (0..TABLE)
            .map(|i| (i as f32 / (TABLE - 1) as f32).powf(0.2))
            .collect();
        let srgb = (0..TABLE)
            .map(|i| {
                let encoded = linear_to_srgb(i as f32 / (TABLE - 1) as f32);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let byte = (encoded * 255.0).round().clamp(0.0, 255.0) as u8;
                byte
            })
            .collect();
        Self {
            transfer,
            signal,
            ootf,
            srgb,
        }
    }

    pub fn transfer(&self) -> Transfer {
        self.transfer
    }

    /// `src` — RGBA, 16 bits a channel little-endian, rows `stride` bytes
    /// apart, `width` × `height` — into `dst` as packed RGBA, 8 bits sRGB.
    /// On every core.
    pub fn apply(&self, src: &[u8], stride: usize, (width, height): (u32, u32), dst: &mut Vec<u8>) {
        let (cols, rows) = (width as usize, height as usize);
        dst.resize(cols * rows * 4, 0);
        let threads = std::thread::available_parallelism().map_or(4, usize::from);
        let rows_each = rows.div_ceil(threads.max(1)).max(1);
        std::thread::scope(|scope| {
            for (band, out) in dst.chunks_mut(rows_each * cols * 4).enumerate() {
                scope.spawn(move || {
                    for (i, row) in out.chunks_exact_mut(cols * 4).enumerate() {
                        let at = (band * rows_each + i) * stride;
                        let line = &src[at..at + cols * 8];
                        for (px, rgba) in row
                            .as_chunks_mut::<4>()
                            .0
                            .iter_mut()
                            .zip(line.as_chunks::<8>().0)
                        {
                            let channel =
                                |c: usize| u16::from_le_bytes([rgba[c * 2], rgba[c * 2 + 1]]);
                            let rgb = self.pixel(channel(0), channel(1), channel(2));
                            *px = [rgb[0], rgb[1], rgb[2], 255];
                        }
                    }
                });
            }
        });
    }

    /// One pixel's 16-bit signal to 8-bit sRGB.
    fn pixel(&self, r: u16, g: u16, b: u16) -> [u8; 3] {
        let rgb = [
            self.signal[usize::from(r)],
            self.signal[usize::from(g)],
            self.signal[usize::from(b)],
        ];
        let rgb = match self.transfer {
            Transfer::Pq => rgb,
            Transfer::Hlg => {
                // Scene light to display light (BT.2100's OOTF), then
                // SDR-relative: HLG's reference white comes to 203 nits.
                let ys = 0.2627 * rgb[0] + 0.6780 * rgb[1] + 0.0593 * rgb[2];
                let gain = lookup(&self.ootf, ys) * (PEAK / REFERENCE_WHITE);
                rgb.map(|c| c * gain)
            }
        };
        let rgb = TO_709.map(|row| (row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2]).max(0.0));
        // Highlights rolled off by the brightest channel, all three scaled
        // alike, so hues hold.
        let brightest = rgb[0].max(rgb[1]).max(rgb[2]);
        let scale = if brightest > KNEE {
            roll_off(brightest) / brightest
        } else {
            1.0
        };
        rgb.map(|c| {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let at = ((c * scale).clamp(0.0, 1.0) * (TABLE - 1) as f32).round() as usize;
            self.srgb[at]
        })
    }
}

/// `table`'s entry for `v` from 0 to 1.
fn lookup(table: &[f32], v: f32) -> f32 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let at = (v.clamp(0.0, 1.0) * (table.len() - 1) as f32).round() as usize;
    table[at]
}

/// SDR-relative light above the knee, rolled off to reach SDR's white at
/// the peak: unchanged at the knee, and as steep there, so nothing bends.
fn roll_off(light: f32) -> f32 {
    let room = 1.0 - KNEE;
    let most = (PEAK / REFERENCE_WHITE - KNEE) / room;
    let over = ((light - KNEE) / room).min(most);
    KNEE + room * over * (1.0 + over / (most * most)) / (1.0 + over)
}

/// PQ signal (0 to 1) to nits (ST 2084's EOTF).
fn pq_to_nits(signal: f32) -> f32 {
    const M1: f32 = 2610.0 / 16384.0;
    const M2: f32 = 2523.0 / 4096.0 * 128.0;
    const C1: f32 = 3424.0 / 4096.0;
    const C2: f32 = 2413.0 / 4096.0 * 32.0;
    const C3: f32 = 2392.0 / 4096.0 * 32.0;
    let p = signal.max(0.0).powf(1.0 / M2);
    10_000.0 * ((p - C1).max(0.0) / (C2 - C3 * p)).powf(1.0 / M1)
}

/// HLG signal (0 to 1) to scene light (0 to 1): its OETF undone.
fn hlg_to_scene(signal: f32) -> f32 {
    const A: f32 = 0.178_832_77;
    const B: f32 = 1.0 - 4.0 * A;
    const C: f32 = 0.559_910_7;
    if signal <= 0.5 {
        signal * signal / 3.0
    } else {
        (((signal - C) / A).exp() + B) / 12.0
    }
}

fn linear_to_srgb(light: f32) -> f32 {
    if light <= 0.003_130_8 {
        light * 12.92
    } else {
        1.055 * light.powf(1.0 / 2.4) - 0.055
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(signal: f32) -> u16 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let code = (signal * f32::from(u16::MAX)).round() as u16;
        code
    }

    /// How long a 4K frame takes, by hand:
    /// `cargo test --release --lib hdr::tests::timing -- --ignored --nocapture`.
    #[test]
    #[ignore = "a timing, not a test"]
    fn timing_of_a_4k_frame() {
        let map = ToneMap::new(Transfer::Hlg);
        let (w, h) = (3840_u32, 2160_u32);
        let src: Vec<u8> = (0..w as usize * h as usize * 8)
            .map(|i| (i * 7) as u8)
            .collect();
        let mut dst = Vec::new();
        map.apply(&src, w as usize * 8, (w, h), &mut dst);
        let start = std::time::Instant::now();
        for _ in 0..10 {
            map.apply(&src, w as usize * 8, (w, h), &mut dst);
        }
        println!("4K frame: {:.1} ms", start.elapsed().as_secs_f64() * 100.0);
    }

    #[test]
    fn pq_puts_reference_white_at_its_known_signal() {
        assert!(
            (pq_to_nits(0.5807) - 203.0).abs() < 2.0,
            "{}",
            pq_to_nits(0.5807)
        );
        assert!((pq_to_nits(1.0) - 10_000.0).abs() < 1.0);
        assert!(pq_to_nits(0.0) < 1e-3);
    }

    /// Reference white — 58 % signal in PQ, 75 % in HLG — comes out a near
    /// white, not the mid-grey SDR would read it as; black stays black.
    #[test]
    fn reference_white_comes_out_near_white() {
        for (transfer, white) in [(Transfer::Pq, 0.5807), (Transfer::Hlg, 0.75)] {
            let map = ToneMap::new(transfer);
            let [r, g, b] = map.pixel(code(white), code(white), code(white));
            assert!((228..=248).contains(&r), "{transfer:?}: {r}");
            assert_eq!((r, g), (g, b), "{transfer:?}: grey stays grey");
            assert_eq!(map.pixel(0, 0, 0), [0, 0, 0], "{transfer:?}");
        }
    }

    /// The brightest highlight reaches SDR's white, and brighter ones do
    /// not go past it: they are rolled off, not clipped harshly below it.
    #[test]
    fn highlights_roll_off_to_white() {
        let map = ToneMap::new(Transfer::Pq);
        let at = |nits: f32| {
            // The PQ signal for `nits`, found by halving.
            let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
            for _ in 0..40 {
                let mid = f32::midpoint(lo, hi);
                if pq_to_nits(mid) < nits {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            map.pixel(code(lo), code(lo), code(lo))[0]
        };
        assert!(at(400.0) > at(203.0), "brighter still reads brighter");
        assert!(at(1000.0) >= 253, "the peak is white: {}", at(1000.0));
        assert_eq!(at(4000.0), 255);
        assert!((roll_off(KNEE) - KNEE).abs() < 1e-6, "no step at the knee");
    }

    /// A pure BT.2020 green is outside BT.709: it comes out the most
    /// saturated green there is, not a washed one.
    #[test]
    fn bt2020_colours_come_to_bt709() {
        let map = ToneMap::new(Transfer::Pq);
        let [r, g, b] = map.pixel(0, code(0.5807), 0);
        assert!(g > 200 && r == 0 && b == 0, "{:?}", [r, g, b]);
    }
}
