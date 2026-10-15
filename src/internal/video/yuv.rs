//! Telling swscale how a decoded frame's YUV was encoded.
//!
//! Left alone, swscale converts every YUV frame with the BT.601 matrix and
//! assumes limited range. HD material is BT.709, so reds came out orange-ish
//! and greens shifted, and full-range sources (phones, screen recordings)
//! lost their blacks and whites. The frame carries its own colour tags; this
//! reads them and configures the scaler to match.

extern crate ffmpeg_next as ffmpeg;

use ffmpeg::software::scaling::context::Context as Scaler;
use ffmpeg::util::color::{Range, Space};
use ffmpeg::util::format::Pixel;
use ffmpeg::util::frame::video::Video;

/// Flags for the YUV → RGBA scalers.
///
/// Plain bilinear keeps swscale on its SIMD fast path (0.36 ms for a 1080p
/// frame on an M4). `ACCURATE_RND | FULL_CHR_H_INT` interpolates chroma per
/// pixel for slightly cleaner coloured edges, but costs 5.2 ms a frame — too
/// much beside a software decode at 60 fps on a show laptop, and 4× that for
/// 4K. The matrix and range set by [`YuvSetup`] apply on the fast path too.
pub fn rgba_flags() -> ffmpeg::software::scaling::Flags {
    ffmpeg::software::scaling::Flags::BILINEAR
}

/// How a frame's YUV maps to RGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YuvEncoding {
    /// A `SWS_CS_*` matrix.
    matrix: i32,
    full_range: bool,
}

impl YuvEncoding {
    /// Read the encoding from a frame's tags, filling untagged fields the way
    /// players do: BT.709 from 720 lines up, BT.601 below; limited range.
    /// `None` for RGB frames, which have no matrix to choose.
    pub fn of(frame: &Video) -> Option<Self> {
        let format = frame.format();
        if !is_yuv(format) {
            return None;
        }
        let matrix = match frame.color_space() {
            Space::BT709 => ffmpeg::ffi::SWS_CS_ITU709,
            Space::BT470BG | Space::SMPTE170M => ffmpeg::ffi::SWS_CS_ITU601,
            Space::SMPTE240M => ffmpeg::ffi::SWS_CS_SMPTE240M,
            Space::FCC => ffmpeg::ffi::SWS_CS_FCC,
            Space::BT2020NCL | Space::BT2020CL => ffmpeg::ffi::SWS_CS_BT2020,
            _ if frame.height() >= 720 => ffmpeg::ffi::SWS_CS_ITU709,
            _ => ffmpeg::ffi::SWS_CS_ITU601,
        };
        // The deprecated `yuvj*` formats mean full range whatever the tag says.
        let full_range = frame.color_range() == Range::JPEG || is_yuvj(format);
        Some(Self { matrix, full_range })
    }
}

/// Keeps one scaler configured for the frames going through it, touching
/// swscale only when the encoding changes.
#[derive(Debug, Default)]
pub struct YuvSetup {
    applied: Option<YuvEncoding>,
}

impl YuvSetup {
    /// Configure `scaler` for `frame` if it is not already.
    pub fn apply(&mut self, scaler: &mut Scaler, frame: &Video) {
        let Some(encoding) = YuvEncoding::of(frame) else {
            return;
        };
        if self.applied == Some(encoding) {
            return;
        }
        // Safety: the scaler's context is valid for its lifetime, and
        // `sws_getCoefficients` returns a pointer into a static table.
        let result = unsafe {
            let coefficients = ffmpeg::ffi::sws_getCoefficients(encoding.matrix);
            ffmpeg::ffi::sws_setColorspaceDetails(
                scaler.as_mut_ptr(),
                coefficients,
                i32::from(encoding.full_range),
                coefficients,
                1, // RGBA out is always full range
                0,
                1 << 16,
                1 << 16,
            )
        };
        if result < 0 {
            log::warn!("swscale refused YUV encoding {encoding:?}; colours may be off");
        }
        // Recorded either way: retrying every frame would not change the answer.
        self.applied = Some(encoding);
    }

    /// Forget what was applied, for a scaler that has just been rebuilt.
    pub fn reset(&mut self) {
        self.applied = None;
    }
}

fn is_yuvj(format: Pixel) -> bool {
    matches!(
        format,
        Pixel::YUVJ420P | Pixel::YUVJ422P | Pixel::YUVJ444P | Pixel::YUVJ440P | Pixel::YUVJ411P
    )
}

fn is_yuv(format: Pixel) -> bool {
    let Some(descriptor) = format.descriptor() else {
        return false;
    };
    // Safety: a descriptor from `av_pix_fmt_desc_get` points at a static table.
    let flags = unsafe { (*descriptor.as_ptr()).flags };
    let rgb = flags & u64::from(ffmpeg::ffi::AV_PIX_FMT_FLAG_RGB.unsigned_abs()) != 0;
    descriptor.nb_components() >= 3 && !rgb
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame of one flat YUV colour.
    fn flat(format: Pixel, space: Space, range: Range, (y, u, v): (u8, u8, u8)) -> Video {
        let mut frame = Video::new(format, 64, 720);
        frame.set_color_space(space);
        frame.set_color_range(range);
        for (plane, value) in [(0, y), (1, u), (2, v)] {
            frame.data_mut(plane).fill(value);
        }
        frame
    }

    fn convert(frame: &Video, configure: bool) -> [u8; 3] {
        let mut scaler = Scaler::get(
            frame.format(),
            frame.width(),
            frame.height(),
            Pixel::RGBA,
            frame.width(),
            frame.height(),
            rgba_flags(),
        )
        .expect("scaler");
        if configure {
            YuvSetup::default().apply(&mut scaler, frame);
        }
        let mut out = Video::empty();
        scaler.run(frame, &mut out).expect("convert");
        let px = &out.data(0)[..3];
        [px[0], px[1], px[2]]
    }

    fn assert_rgb(got: [u8; 3], want: [u8; 3], what: &str) {
        let off = (0..3)
            .map(|c| (i32::from(got[c]) - i32::from(want[c])).abs())
            .max();
        assert!(off <= Some(2), "{what}: got {got:?}, want {want:?}");
    }

    #[test]
    fn hd_red_is_red() {
        // BT.709 limited-range pure red. Through swscale's default BT.601
        // matrix it comes out (233, 0, 3).
        let red = flat(Pixel::YUV420P, Space::BT709, Range::MPEG, (63, 102, 240));
        assert_rgb(convert(&red, true), [255, 0, 0], "BT.709 red");
        assert!(
            convert(&red, false)[0] < 240,
            "precondition: unconfigured swscale gets this wrong"
        );
    }

    #[test]
    fn full_range_levels_are_not_stretched() {
        // Read as limited range, full-range 235 would be stretched to white
        // and 20 crushed toward black.
        let light = flat(Pixel::YUV420P, Space::BT709, Range::JPEG, (235, 128, 128));
        assert_rgb(
            convert(&light, true),
            [235, 235, 235],
            "full-range light grey",
        );
        let dark = flat(Pixel::YUV420P, Space::BT709, Range::JPEG, (20, 128, 128));
        assert_rgb(convert(&dark, true), [20, 20, 20], "full-range dark grey");
    }

    #[test]
    fn a_yuvj_frame_is_full_range_whatever_its_tag() {
        let light = flat(
            Pixel::YUVJ420P,
            Space::BT709,
            Range::Unspecified,
            (235, 128, 128),
        );
        assert_rgb(convert(&light, true), [235, 235, 235], "yuvj light grey");
    }

    #[test]
    fn limited_range_grey_keeps_its_level() {
        // Mid grey: 16 + 219 * 0.5 ≈ 126 → 128 of 255.
        let grey = flat(Pixel::YUV420P, Space::BT709, Range::MPEG, (126, 128, 128));
        assert_rgb(convert(&grey, true), [128, 128, 128], "limited mid grey");
    }

    #[test]
    fn an_untagged_hd_frame_is_taken_as_bt709() {
        let red = flat(
            Pixel::YUV420P,
            Space::Unspecified,
            Range::Unspecified,
            (63, 102, 240),
        );
        assert_rgb(convert(&red, true), [255, 0, 0], "untagged HD red");
    }

    #[test]
    fn rgb_frames_are_left_alone() {
        let frame = Video::new(Pixel::RGBA, 4, 4);
        assert_eq!(YuvEncoding::of(&frame), None);
    }
}
