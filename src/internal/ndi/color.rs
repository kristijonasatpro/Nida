/// Convert UYVY 4:2:2 to RGBA.
pub(super) fn uyvy_to_rgba(uyvy: &[u8], rgba: &mut [u8], w: u32, h: u32, stride: usize) {
    // As `rgba_to_uyvy` writes it: video levels, BT.709 for HD and up.
    let (kr, kb) = if h >= 720 {
        (0.2126, 0.0722)
    } else {
        (0.299, 0.114)
    };
    let kg = 1.0 - kr - kb;
    let luma = |y: u8| (f32::from(y) - 16.0) * 255.0 / 219.0;
    let chroma = |c: u8| (f32::from(c) - 128.0) * 255.0 / 224.0;
    let to_u8 = |v: f32| (v + 0.5).clamp(0.0, 255.0) as u8;
    let rgb = |luma: f32, cb: f32, cr: f32| {
        let red = luma + 2.0 * (1.0 - kr) * cr;
        let blue = luma + 2.0 * (1.0 - kb) * cb;
        let green = (luma - kr * red - kb * blue) / kg;
        [to_u8(red), to_u8(green), to_u8(blue), 255]
    };
    for y in 0..h as usize {
        for x in (0..w as usize).step_by(2) {
            let src = y * stride + x * 2;
            if src + 3 >= uyvy.len() {
                break;
            }
            let cb = chroma(uyvy[src]);
            let cr = chroma(uyvy[src + 2]);

            let dst0 = (y * w as usize + x) * 4;
            let dst1 = dst0 + 4;
            rgba[dst0..dst0 + 4].copy_from_slice(&rgb(luma(uyvy[src + 1]), cb, cr));
            if x + 1 < w as usize && dst1 + 3 < rgba.len() {
                rgba[dst1..dst1 + 4].copy_from_slice(&rgb(luma(uyvy[src + 3]), cb, cr));
            }
        }
    }
}
