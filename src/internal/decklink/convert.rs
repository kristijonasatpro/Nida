//! Portable RGBA to UYVY conversion for DeckLink. NDI keeps its video range.

pub(super) struct Converter {
    full_range: bool,
}

impl Converter {
    #[allow(clippy::unnecessary_wraps)] // Same contract as Accelerate's constructor.
    pub(super) fn new(_height: u32, full_range: bool) -> Result<Self, String> {
        Ok(Self { full_range })
    }

    #[allow(clippy::unnecessary_wraps)] // Same contract as Accelerate's run method.
    pub(super) fn run(
        &mut self,
        rgba: &[u8],
        uyvy: &mut [u8],
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        if !self.full_range {
            crate::ndi::rgba_to_uyvy(rgba, uyvy, width, height);
            return Ok(());
        }
        // Match vImage's full-range matrix and clamp, rather than expanding
        // already quantized video-range bytes (which loses precision).
        let (kr, kb) = if height >= 720 {
            (0.2126, 0.0722)
        } else {
            (0.299, 0.114)
        };
        let kg = 1.0 - kr - kb;
        let luma = |p: &[u8]| kr * f32::from(p[0]) + kg * f32::from(p[1]) + kb * f32::from(p[2]);
        let code = |v: f32| (v + 0.5).clamp(1.0, 254.0) as u8;
        for (pair, out) in rgba.chunks_exact(8).zip(uyvy.chunks_exact_mut(4)) {
            let (a, b) = pair.split_at(4);
            let (y0, y1) = (luma(a), luma(b));
            let y = f32::midpoint(y0, y1);
            let cb = (f32::midpoint(f32::from(a[2]), f32::from(b[2])) - y)
                / (2.0 * (1.0 - kb));
            let cr = (f32::midpoint(f32::from(a[0]), f32::from(b[0])) - y)
                / (2.0 * (1.0 - kr));
            out.copy_from_slice(&[code(128.0 + cb), code(y0), code(128.0 + cr), code(y1)]);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Converter;

    fn pair(pixels: [[u8; 4]; 2], height: u32, full: bool) -> [u8; 4] {
        let rgba = pixels.concat().repeat(height as usize);
        let mut uyvy = vec![0; 4 * height as usize];
        Converter::new(height, full)
            .unwrap()
            .run(&rgba, &mut uyvy, 2, height)
            .unwrap();
        uyvy[..4].try_into().unwrap()
    }

    #[test]
    fn full_range_keeps_each_pixels_luma_and_averages_chroma() {
        for height in [576, 1080] {
            assert_eq!(
                pair([[0, 0, 0, 255], [255; 4]], height, true),
                [128, 1, 128, 254]
            );
            assert_eq!(
                pair([[0, 0, 0, 255], [255; 4]], height, false),
                [128, 16, 128, 235]
            );
        }
    }

    #[test]
    fn full_range_uses_the_hd_or_sd_matrix_and_ignores_alpha() {
        let red = [[255, 0, 0, 0], [255, 0, 0, 255]];
        assert_eq!(pair(red, 1080, true), [99, 54, 254, 54]);
        assert_eq!(pair(red, 576, true), [85, 76, 254, 76]);
        assert_eq!(pair(red, 1080, false), [102, 63, 240, 63]);
        assert_eq!(pair(red, 576, false), [90, 81, 240, 81]);
    }
}
