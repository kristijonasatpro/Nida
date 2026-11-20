//! Colour parameters shared by the Windows GPU converter and its CPU-only tests.
use ffmpeg_next::util::{
    color::{Range, Space},
    frame::video::Video,
};

/// Packed as six vec4s, matching `Params` in `gpu_video.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct Params {
    pub size: [u32; 4],
    pub layout: [u32; 4],
    pub levels: [f32; 4],
    pub red: [f32; 4],
    pub green: [f32; 4],
    pub blue: [f32; 4],
}

impl Params {
    pub fn new(frame: &Video, ten_bit: bool, output: (u32, u32), turns: u8) -> Self {
        use ffmpeg_next::util::chroma::Location;
        let (kr, kb): (f32, f32) = match frame.color_space() {
            Space::BT709 => (0.2126, 0.0722),
            Space::BT470BG | Space::SMPTE170M => (0.299, 0.114),
            Space::SMPTE240M => (0.212, 0.087),
            Space::FCC => (0.30, 0.11),
            Space::BT2020NCL | Space::BT2020CL => (0.2627, 0.0593),
            _ if frame.height() >= 720 => (0.2126, 0.0722),
            _ => (0.299, 0.114),
        };
        let kg = 1.0 - kr - kb;
        let full = frame.color_range() == Range::JPEG;
        let bits = if ten_bit { 10 } else { 8 };
        let factor = (1_u32 << (bits - 8)) as f32;
        let max = ((1_u32 << bits) - 1) as f32;
        let (black, y_span, uv_span) = if full {
            (0.0, max, max)
        } else {
            (16.0 * factor, 219.0 * factor, 224.0 * factor)
        };
        let transfer = match super::hdr::Transfer::of(frame) {
            Some(super::hdr::Transfer::Pq) => 1,
            Some(super::hdr::Transfer::Hlg) => 2,
            None => 0,
        };
        // AVFrame cropping is in luma pixels, independently of allocation padding.
        let raw = unsafe { &*frame.as_ptr() };
        let crop = (raw.crop_left as u32, raw.crop_top as u32);
        let chroma = match frame.chroma_location() {
            Location::Left => (0.0, 0.5),
            Location::TopLeft => (0.0, 0.0),
            Location::Top => (0.5, 0.0),
            Location::BottomLeft => (0.0, 1.0),
            Location::Bottom => (0.5, 1.0),
            Location::Center | Location::Unspecified => (0.5, 0.5),
        };
        Self {
            size: [
                frame.width() - crop.0 - raw.crop_right as u32,
                frame.height() - crop.1 - raw.crop_bottom as u32,
                output.0,
                output.1,
            ],
            layout: [
                u32::from(turns % 4),
                if ten_bit { 6 } else { 0 },
                transfer,
                crop.0 | (crop.1 << 16),
            ],
            levels: [black, y_span, 128.0 * factor, uv_span],
            red: [1.0, 0.0, 2.0 * (1.0 - kr), chroma.0],
            green: [
                1.0,
                -2.0 * kb * (1.0 - kb) / kg,
                -2.0 * kr * (1.0 - kr) / kg,
                chroma.1,
            ],
            blue: [1.0, 2.0 * (1.0 - kb), 0.0, 0.0],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ffmpeg_next::format::Pixel;

    #[test]
    fn limited_p010_uses_high_bits_and_video_levels() {
        let mut frame = Video::new(Pixel::P010LE, 1920, 1080);
        frame.set_color_space(Space::BT709);
        let p = Params::new(&frame, true, (1080, 1920), 1);
        assert_eq!(p.layout[..2], [1, 6]);
        assert_eq!(p.levels, [64.0, 876.0, 512.0, 896.0]);
        assert_eq!(p.size, [1920, 1080, 1080, 1920]);
        assert!((p.red[2] - 1.5748).abs() < 0.0001);
    }

    #[test]
    fn full_range_and_untagged_sd_match_the_cpu_defaults() {
        let mut frame = Video::new(Pixel::NV12, 640, 480);
        frame.set_color_range(Range::JPEG);
        let p = Params::new(&frame, false, (640, 480), 0);
        assert_eq!(p.levels, [0.0, 255.0, 128.0, 255.0]);
        assert!((p.red[2] - 1.402).abs() < 0.0001);
    }

    #[test]
    fn hdr_tags_and_cropping_survive_the_handoff() {
        let mut frame = Video::new(Pixel::P010LE, 1920, 1088);
        frame.set_color_transfer_characteristic(
            ffmpeg_next::util::color::TransferCharacteristic::SMPTE2084,
        );
        unsafe {
            (*frame.as_mut_ptr()).crop_bottom = 8;
        }
        let p = Params::new(&frame, true, (1920, 1080), 0);
        assert_eq!(p.size[1], 1080);
        assert_eq!(p.layout[2], 1);
    }
}
