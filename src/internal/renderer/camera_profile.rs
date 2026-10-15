//! A camera's log or HLG picture turned into Rec.709 video for the show — a
//! camera's picture profile (`CameraProfile`).
//!
//! A camera set to S-Log3, Blackmagic Film or HLG sends a flat, grey picture
//! that keeps the scene's highlights for grading; shown as it comes it looks
//! washed out next to every other source. Its picture is converted on the
//! GPU, each new frame, into Rec.709 video the way a Rec.709 camera would have
//! made it: the curve undone to the light the scene had (18% grey 0.18), the
//! camera's colours put on Rec.709's primaries, then coded with Rec.709's
//! camera curve — 18% grey at 41% — and a knee above 85% that bends the
//! highlights the log picture kept smoothly into the top instead of clipping
//! them. HLG is brought down to SDR as ITU-R BT.2408 says broadcasters do:
//! through its display light, with HDR reference white (75% HLG, 203 cd/m²)
//! made SDR white. This is Nida's neutral conversion; a manufacturer's own
//! look (Sony's s709, Blackmagic's Film to Video) comes from its LUT file.
//!
//! The converted picture is written video-coded into an `Rgba8Unorm` picture
//! that decks read through an sRGB view, so it is made linear exactly as the
//! show makes any video source linear — a log camera and a Rec.709 camera then
//! match in the mix.
//!
//! The reference functions here are the shader's (`shaders/camera_profile.wgsl`),
//! one for one, and the tests hold them to the published reference points.

use std::num::NonZeroU64;

use crate::engine::value::camera::CameraProfile;

/// Chromaticities of a camera's or a standard's primaries and white.
#[derive(Debug, Clone, Copy)]
pub struct Primaries {
    pub red: [f64; 2],
    pub green: [f64; 2],
    pub blue: [f64; 2],
    pub white: [f64; 2],
}

/// CIE 1931 2° D65, as ITU-R BT.709 and BT.2020 give it.
const D65: [f64; 2] = [0.3127, 0.3290];

/// ITU-R BT.709.
pub const REC709: Primaries = Primaries {
    red: [0.64, 0.33],
    green: [0.30, 0.60],
    blue: [0.15, 0.06],
    white: D65,
};

/// ITU-R BT.2020 (and BT.2100, HLG's).
pub const REC2020: Primaries = Primaries {
    red: [0.708, 0.292],
    green: [0.170, 0.797],
    blue: [0.131, 0.046],
    white: D65,
};

/// Sony S-Gamut, which S-Gamut3 shares (Sony, "Technical Summary for
/// S-Gamut3.Cine/S-Log3 and S-Gamut3/S-Log3"; colour-science `PRIMARIES_S_GAMUT`).
pub const S_GAMUT: Primaries = Primaries {
    red: [0.730, 0.280],
    green: [0.140, 0.855],
    blue: [0.100, -0.050],
    white: D65,
};

/// Sony S-Gamut3.Cine (same source; colour-science `PRIMARIES_S_GAMUT3_CINE`).
pub const S_GAMUT3_CINE: Primaries = Primaries {
    red: [0.766, 0.275],
    green: [0.225, 0.800],
    blue: [0.089, -0.087],
    white: D65,
};

/// Blackmagic Wide Gamut, Generations 4 and 5 (Blackmagic Design,
/// "Blackmagic Generation 5 Color Science", 2021; colour-science
/// `PRIMARIES_BLACKMAGIC_WIDE_GAMUT`, with its own rounding of D65).
pub const BLACKMAGIC_WIDE_GAMUT: Primaries = Primaries {
    red: [0.717_721_5, 0.317_118_1],
    green: [0.228_041_0, 0.861_569_0],
    blue: [0.100_584_1, -0.082_045_2],
    white: [0.312_717_0, 0.329_031_2],
};

type Matrix = [[f64; 3]; 3];

fn xyz_of([x, y]: [f64; 2]) -> [f64; 3] {
    [x / y, 1.0, (1.0 - x - y) / y]
}

fn multiply(a: &Matrix, b: &Matrix) -> Matrix {
    let mut out = [[0.0; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    out
}

fn apply(m: &Matrix, v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

fn invert(m: &Matrix) -> Matrix {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    let c =
        |r0: usize, c0: usize, r1: usize, c1: usize| m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0];
    [
        [
            c(1, 1, 2, 2) / det,
            -c(0, 1, 2, 2) / det,
            c(0, 1, 1, 2) / det,
        ],
        [
            -c(1, 0, 2, 2) / det,
            c(0, 0, 2, 2) / det,
            -c(0, 0, 1, 2) / det,
        ],
        [
            c(1, 0, 2, 1) / det,
            -c(0, 0, 2, 1) / det,
            c(0, 0, 1, 1) / det,
        ],
    ]
}

/// The matrix from linear RGB on `p`'s primaries to CIE XYZ, white at Y 1
/// (SMPTE RP 177's normalised primary matrix).
pub fn rgb_to_xyz(space: &Primaries) -> Matrix {
    let [red, green, blue] = [space.red, space.green, space.blue].map(xyz_of);
    let primaries = [
        [red[0], green[0], blue[0]],
        [red[1], green[1], blue[1]],
        [red[2], green[2], blue[2]],
    ];
    let scales = apply(&invert(&primaries), xyz_of(space.white));
    let mut npm = primaries;
    for row in &mut npm {
        for (cell, scale) in row.iter_mut().zip(scales) {
            *cell *= scale;
        }
    }
    npm
}

/// The matrix from linear RGB on `p`'s primaries to linear Rec.709.
pub fn to_rec709(p: &Primaries) -> Matrix {
    multiply(&invert(&rgb_to_xyz(&REC709)), &rgb_to_xyz(p))
}

/// The curve a profile undoes, as the shader numbers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Curve {
    SLog2 = 1,
    SLog3 = 2,
    FilmGen5 = 3,
    Hlg = 4,
}

/// What the shader is told for a built-in profile: its curve, its gamut, and
/// the light scale. `None` for one with nothing to do here (Normal, or a LUT).
pub fn built_in(profile: &CameraProfile) -> Option<(Curve, Primaries, f64)> {
    Some(match profile {
        CameraProfile::SonyHlg => (Curve::Hlg, REC2020, 1.0 / hlg_reference_white()),
        CameraProfile::SonySLog2 => (Curve::SLog2, S_GAMUT, 1.0),
        CameraProfile::SonySLog3Cine => (Curve::SLog3, S_GAMUT3_CINE, 1.0),
        CameraProfile::SonySLog3 => (Curve::SLog3, S_GAMUT, 1.0),
        CameraProfile::BlackmagicFilmGen5 => (Curve::FilmGen5, BLACKMAGIC_WIDE_GAMUT, 1.0),
        CameraProfile::Normal | CameraProfile::Lut { .. } => return None,
    })
}

// ── The curves, one for one with the shader ──────────────────────────────

/// Sony S-Log2 code value — video levels taken out, 0 at 64 and 1 at 940 of
/// 1023, as a limited-range camera is read — to linear reflection (Sony,
/// "S-Log2 Technical Paper", 2012; colour-science `log_decoding_SLog2`).
pub fn s_log2(x: f64) -> f64 {
    let lin = if x >= 0.030_001_222_851_889_303 {
        10_f64.powf((x - 0.616_596 - 0.03) / 0.432_699) - 0.037_584
    } else {
        (x - 0.030_001_222_851_889_303) / 5.0
    };
    lin * 0.9 * 219.0 / 155.0
}

/// Linear reflection to Sony S-Log2, the code value over 1023 (video levels
/// in): the inverse of [`s_log2`], for its reference points.
pub fn s_log2_encode(lin: f64) -> f64 {
    let x = lin * 155.0 / 219.0 / 0.9;
    let full = if x >= 0.0 {
        0.432_699 * (x + 0.037_584).log10() + 0.616_596 + 0.03
    } else {
        x * 5.0 + 0.030_001_222_851_889_303
    };
    (876.0 * full + 64.0) / 1023.0
}

/// Sony S-Log3 code value, video levels taken out, to linear reflection
/// (Sony, "Technical Summary for S-Gamut3.Cine/S-Log3 and S-Gamut3/S-Log3";
/// colour-science `log_decoding_SLog3`), defined on the code value over 1023.
pub fn s_log3(x: f64) -> f64 {
    let cv = 876.0 * x + 64.0;
    if cv >= 171.210_294_692_9 {
        10_f64.powf((cv - 420.0) / 261.5) * (0.18 + 0.01) - 0.01
    } else {
        (cv - 95.0) * 0.011_25 / (171.210_294_692_9 - 95.0)
    }
}

/// Linear reflection to Sony S-Log3, the code value over 1023 (colour-science
/// `log_encoding_SLog3`).
pub fn s_log3_encode(lin: f64) -> f64 {
    if lin >= 0.011_25 {
        (420.0 + ((lin + 0.01) / (0.18 + 0.01)).log10() * 261.5) / 1023.0
    } else {
        (lin * (171.210_294_692_9 - 95.0) / 0.011_25 + 95.0) / 1023.0
    }
}

/// Blackmagic Film Generation 5 constants (Blackmagic Design, "Blackmagic
/// Generation 5 Color Science", 2021; colour-science
/// `CONSTANTS_BLACKMAGIC_FILM_GENERATION_5`).
const GEN5_A: f64 = 0.086_928_760_654_912_24;
const GEN5_B: f64 = 0.005_494_072_432_257_808;
const GEN5_C: f64 = 0.530_013_339_229_193_9;
const GEN5_D: f64 = 8.283_605_932_402_494;
const GEN5_E: f64 = 0.092_465_753_424_657_53;
const GEN5_LIN_CUT: f64 = 0.005;

/// Blackmagic Film Generation 5 value to scene linear.
pub fn film_gen5(v: f64) -> f64 {
    let log_cut = GEN5_D * GEN5_LIN_CUT + GEN5_E;
    if v < log_cut {
        (v - GEN5_E) / GEN5_D
    } else {
        ((v - GEN5_C) / GEN5_A).exp() - GEN5_B
    }
}

/// Scene linear to Blackmagic Film Generation 5.
pub fn film_gen5_encode(lin: f64) -> f64 {
    if lin < GEN5_LIN_CUT {
        GEN5_D * lin + GEN5_E
    } else {
        GEN5_A * (lin + GEN5_B).ln() + GEN5_C
    }
}

/// HLG constants (ITU-R BT.2100; ARIB STD-B67; colour-science
/// `CONSTANTS_ARIBSTDB67`).
const HLG_A: f64 = 0.178_832_77;
const HLG_B: f64 = 0.284_668_92;
const HLG_C: f64 = 0.559_910_73;

/// HLG signal to normalised scene light, 0–1 (ITU-R BT.2100 inverse OETF).
pub fn hlg_inverse(e: f64) -> f64 {
    let e = e.max(0.0);
    if e <= 0.5 {
        e * e / 3.0
    } else {
        (((e - HLG_C) / HLG_A).exp() + HLG_B) / 12.0
    }
}

/// Normalised scene light to HLG (ITU-R BT.2100 OETF).
pub fn hlg_encode(l: f64) -> f64 {
    if l <= 1.0 / 12.0 {
        (3.0 * l).sqrt()
    } else {
        HLG_A * (12.0 * l - HLG_B).ln() + HLG_C
    }
}

/// HLG to display light for a 1 000 cd/m² display, peak 1 (ITU-R BT.2100
/// OOTF: system gamma 1.2, BT.2020 luminance).
pub fn hlg_display(code: [f64; 3]) -> [f64; 3] {
    let scene = code.map(hlg_inverse);
    let ys = 0.2627 * scene[0] + 0.6780 * scene[1] + 0.0593 * scene[2];
    scene.map(|c| ys.max(1e-9).powf(0.2) * c)
}

/// The display light of HDR reference white, 75% HLG — 203 cd/m² of a
/// 1 000 cd/m² display (ITU-R BT.2408 Table 1), made SDR white (BT.2408
/// § 5.1.3.3: 0.265^1.2 = 0.203).
pub fn hlg_reference_white() -> f64 {
    hlg_display([0.75; 3])[0]
}

/// Rec.709 camera OETF (ITU-R BT.709-6), carried on above 1.
pub fn rec709(l: f64) -> f64 {
    if l < 0.018 {
        4.5 * l
    } else {
        1.099 * l.powf(0.45) - 0.099
    }
}

/// sRGB coding (IEC 61966-2-1), carried on above 1.
pub fn srgb(l: f64) -> f64 {
    if l <= 0.003_130_8 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

/// sRGB decoding: how Nida makes video linear (an sRGB texture view).
pub fn srgb_decode(v: f64) -> f64 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// Where the knee starts: video above it bends towards 100%.
const KNEE: f64 = 0.85;

/// A camera's knee: smooth, continuous in slope, never above 1.
pub fn knee(v: f64) -> f64 {
    if v <= KNEE {
        v
    } else {
        KNEE + (1.0 - KNEE) * (1.0 - (-(v - KNEE) / (1.0 - KNEE)).exp())
    }
}

/// The whole conversion of one pixel's code values, as the shader makes it:
/// the video value written, 0–1.
pub fn convert(curve: Curve, matrix: &Matrix, scale: f64, code: [f64; 3]) -> [f64; 3] {
    let lin = match curve {
        Curve::SLog2 => code.map(s_log2),
        Curve::SLog3 => code.map(s_log3),
        Curve::FilmGen5 => code.map(film_gen5),
        Curve::Hlg => hlg_display(code),
    }
    .map(|c| c * scale);
    let rgb = apply(matrix, lin).map(|c| c.max(0.0));
    let video = match curve {
        Curve::Hlg => rgb.map(srgb),
        _ => rgb.map(rec709),
    };
    video.map(|v| knee(v).clamp(0.0, 1.0))
}

// ── On the GPU ───────────────────────────────────────────────────────────

/// What the shader is told (see `Profile` in `camera_profile.wgsl`).
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ProfileUniform {
    rows: [[f32; 4]; 3],
    curve: u32,
    scale: f32,
    _pad: [u32; 2],
}

impl ProfileUniform {
    #[allow(clippy::cast_possible_truncation)]
    fn of(curve: Curve, primaries: &Primaries, scale: f64) -> Self {
        let m = to_rec709(primaries);
        Self {
            rows: m.map(|row| [row[0] as f32, row[1] as f32, row[2] as f32, 0.0]),
            curve: curve as u32,
            scale: scale as f32,
            _pad: [0; 2],
        }
    }
}

/// The format the converted picture is written in, and read through.
const WRITTEN: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const READ: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// The pipelines converting cameras' pictures: one for the built-in
/// profiles, one for LUT files — made once, shared by every camera.
pub struct ProfilePasses {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    lut: super::lut::LutPipeline,
}

impl ProfilePasses {
    /// # Errors
    ///
    /// When the LUT pipeline cannot be made.
    pub fn new(device: &wgpu::Device) -> anyhow::Result<Self> {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Camera Profile Bind Group Layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(
                            std::mem::size_of::<ProfileUniform>() as u64
                        ),
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Camera Profile Pipeline Layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Camera Profile Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/camera_profile.wgsl").into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Camera Profile Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: WRITTEN,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Ok(Self {
            pipeline,
            layout,
            lut: super::lut::LutPipeline::new(device, WRITTEN)?,
        })
    }
}

/// How a camera's picture is converted: by a built-in profile's shader, or
/// through its LUT file.
enum Conversion {
    BuiltIn { bind_group: wgpu::BindGroup },
    Lut(Box<super::lut::LoadedLut>),
}

/// A camera's picture converted for the show: what its decks read while a
/// profile is set.
pub struct ConvertedPicture {
    /// What it was made for: the profile, and the camera's picture size.
    pub profile: CameraProfile,
    pub size: (u32, u32),
    /// The camera's picture as code values (a non-sRGB view of it).
    raw: wgpu::TextureView,
    target: wgpu::TextureView,
    /// What decks read: the converted picture, made linear as video is.
    shown: wgpu::TextureView,
    conversion: Conversion,
    texture: wgpu::Texture,
}

impl ConvertedPicture {
    /// Ready to convert `camera`'s picture as `profile` says; for a LUT, its
    /// file at `lut_path`.
    ///
    /// # Errors
    ///
    /// For a profile with nothing to convert (Normal), or a LUT file that
    /// cannot be read.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        passes: &ProfilePasses,
        camera: &wgpu::Texture,
        profile: &CameraProfile,
        lut_path: Option<&std::path::Path>,
    ) -> anyhow::Result<Self> {
        let size = (camera.width(), camera.height());
        let raw = camera.create_view(&wgpu::TextureViewDescriptor {
            label: Some("Camera Code Values"),
            format: Some(WRITTEN),
            ..Default::default()
        });
        let conversion = if let Some((curve, primaries, scale)) = built_in(profile) {
            let uniform = ProfileUniform::of(curve, &primaries, scale);
            let buffer = wgpu::util::DeviceExt::create_buffer_init(
                device,
                &wgpu::util::BufferInitDescriptor {
                    label: Some("Camera Profile Uniform"),
                    contents: bytemuck::bytes_of(&uniform),
                    usage: wgpu::BufferUsages::UNIFORM,
                },
            );
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Camera Profile Bind Group"),
                layout: &passes.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&raw),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: buffer.as_entire_binding(),
                    },
                ],
            });
            Conversion::BuiltIn { bind_group }
        } else if let (CameraProfile::Lut { file }, Some(path)) = (profile, lut_path) {
            let parsed = super::lut::parse_lut_file(path)?;
            Conversion::Lut(Box::new(super::lut::LoadedLut::from_parsed(
                device,
                queue,
                &parsed,
                file.clone(),
            )))
        } else {
            anyhow::bail!("nothing to convert for {}", profile.label());
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Camera Converted Picture"),
            size: camera.size(),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: WRITTEN,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[READ],
        });
        let target = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let shown = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("Camera Converted Picture (as video)"),
            format: Some(READ),
            ..Default::default()
        });
        Ok(Self {
            profile: profile.clone(),
            size,
            raw,
            target,
            shown,
            conversion,
            texture,
        })
    }

    /// What decks read.
    pub fn view(&self) -> &wgpu::TextureView {
        &self.shown
    }

    /// The converted picture, as written (video-coded).
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    /// Convert the camera's picture as it is now.
    pub fn encode(
        &self,
        device: &wgpu::Device,
        passes: &ProfilePasses,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        match &self.conversion {
            Conversion::BuiltIn { bind_group } => {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Camera Profile Pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &self.target,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&passes.pipeline);
                pass.set_bind_group(0, bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
            Conversion::Lut(lut) => {
                passes
                    .lut
                    .render(device, encoder, &self.raw, &self.target, lut);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f64, b: f64, tolerance: f64) -> bool {
        (a - b).abs() <= tolerance
    }

    /// S-Log3 puts 0%, 18% and 90% reflectance at code values 95, 420 and
    /// 598 of 1023, as Sony's summary says, and comes back to the same light.
    #[test]
    fn s_log3_meets_sonys_reference_points() {
        let cv = |lin: f64| (s_log3_encode(lin) * 1023.0).round();
        assert_eq!([cv(0.0), cv(0.18), cv(0.90)], [95.0, 420.0, 598.0]);
        assert!(near(s_log3_encode(0.18), 0.410_557_1, 1e-7));
        for lin in [0.0, 0.005, 0.18, 0.9, 5.0] {
            // Nida reads the camera with video levels taken out.
            let x = (s_log3_encode(lin) * 1023.0 - 64.0) / 876.0;
            assert!(near(s_log3(x), lin, 1e-9), "{lin}");
        }
    }

    /// S-Log2 puts 0%, 18% and 90% at code values 90, 347 and 582, as
    /// Sony's paper does (colour-science's worked example), and round trips.
    #[test]
    fn s_log2_meets_sonys_reference_points() {
        let cv = |lin: f64| (s_log2_encode(lin) * 1023.0).round();
        assert_eq!([cv(0.0), cv(0.18), cv(0.90)], [90.0, 347.0, 582.0]);
        assert!(near(s_log2_encode(0.18), 0.339_532_5, 1e-7));
        for lin in [0.0, 0.01, 0.18, 0.9, 4.0] {
            let x = (s_log2_encode(lin) * 1023.0 - 64.0) / 876.0;
            assert!(near(s_log2(x), lin, 1e-9), "{lin}");
        }
    }

    /// Blackmagic Film Gen 5 codes 18% grey as 0.3835616, as Blackmagic's
    /// paper does, and round trips on both sides of its cut.
    #[test]
    fn film_gen5_meets_blackmagics_reference_point() {
        assert!(near(film_gen5_encode(0.18), 0.383_561_6, 1e-7));
        for lin in [0.0, 0.002, 0.005, 0.18, 1.0, 20.0] {
            assert!(near(film_gen5(film_gen5_encode(lin)), lin, 1e-9), "{lin}");
        }
    }

    /// HLG: half signal is a twelfth of scene light (BT.2100's join), and
    /// a 1 000 cd/m² display shows 75% HLG at 203 cd/m² and an 18% grey card
    /// (38% HLG) at 26, as ITU-R BT.2408 Table 1 has them.
    #[test]
    fn hlg_meets_bt2100_and_bt2408() {
        assert!(near(hlg_inverse(0.5), 1.0 / 12.0, 1e-12));
        assert!(near(hlg_encode(1.0 / 12.0), 0.5, 1e-12));
        assert!(near(hlg_encode(0.18 / 12.0), 0.212_132_0, 1e-7));
        for l in [0.0, 0.01, 1.0 / 12.0, 0.3, 1.0] {
            assert!(near(hlg_inverse(hlg_encode(l)), l, 1e-12), "{l}");
        }
        assert_eq!((hlg_reference_white() * 1000.0).round(), 203.0);
        assert_eq!((hlg_display([0.38; 3])[0] * 1000.0).round(), 26.0);
        assert!(near(hlg_inverse(0.75), 0.265, 5e-4), "BT.2408's 0.265");
    }

    /// Each gamut's matrix is the one colour-science publishes, and every
    /// matrix to Rec.709 keeps white white.
    #[test]
    fn gamut_matrices_match_the_published_ones_and_keep_white() {
        let close = |m: Matrix, row0: [f64; 3], tolerance: f64| {
            m[0].iter().zip(row0).all(|(a, b)| near(*a, b, tolerance))
        };
        assert!(close(
            rgb_to_xyz(&S_GAMUT3_CINE),
            [0.599_083_920_8, 0.248_925_516_1, 0.102_446_490_2],
            1e-6
        ));
        assert!(close(
            rgb_to_xyz(&S_GAMUT),
            [0.706_482_713_2, 0.128_801_049_8, 0.115_172_164_1],
            1e-6
        ));
        assert!(close(
            rgb_to_xyz(&BLACKMAGIC_WIDE_GAMUT),
            [0.606_530, 0.220_408, 0.123_479],
            1e-6
        ));
        for p in [
            S_GAMUT,
            S_GAMUT3_CINE,
            BLACKMAGIC_WIDE_GAMUT,
            REC2020,
            REC709,
        ] {
            let white = apply(&to_rec709(&p), [1.0; 3]);
            assert!(white.iter().all(|c| near(*c, 1.0, 1e-3)), "{white:?}");
        }
    }

    /// 18% grey from a log camera lands where a Rec.709 camera puts it (41%
    /// video), HLG's grey card and white where BT.2408 maps them, and
    /// highlights bend under 100% rather than clip.
    #[test]
    fn grey_and_white_land_where_video_has_them() {
        let grey = |profile: &CameraProfile, code: f64| {
            let (curve, p, scale) = built_in(profile).expect("built in");
            convert(curve, &to_rec709(&p), scale, [code; 3])[0]
        };
        let video_levels = |cv: f64| (cv - 64.0) / 876.0;
        let s3 = grey(&CameraProfile::SonySLog3Cine, video_levels(420.0));
        assert!(near(s3, rec709(0.18), 2e-3), "S-Log3 grey {s3}");
        let s2 = grey(&CameraProfile::SonySLog2, video_levels(347.0));
        assert!(near(s2, rec709(0.18), 4e-3), "S-Log2 grey {s2}");
        let g5 = grey(&CameraProfile::BlackmagicFilmGen5, film_gen5_encode(0.18));
        assert!(near(g5, rec709(0.18), 2e-3), "Gen 5 grey {g5}");
        let hlg_grey = grey(&CameraProfile::SonyHlg, 0.38);
        assert!(
            near(srgb_decode(hlg_grey), 0.026 / 0.203, 2e-3),
            "HLG grey {hlg_grey}"
        );
        let hlg_white = grey(&CameraProfile::SonyHlg, 0.75);
        assert!(
            near(hlg_white, knee(1.0), 1e-6),
            "HLG reference white {hlg_white}"
        );
        let highlight = grey(&CameraProfile::SonySLog3Cine, video_levels(900.0));
        assert!(highlight > hlg_white && highlight < 1.0, "{highlight}");
        assert!(knee(0.85) == 0.85 && knee(10.0) <= 1.0);
    }

    /// A picture's code values through the GPU pass come out as the
    /// reference conversion says, to within a code value.
    #[test]
    fn the_gpu_pass_converts_as_the_reference_does() {
        let Ok(gpu) = crate::renderer::context::GpuContext::new_headless() else {
            return;
        };
        let passes = ProfilePasses::new(&gpu.device).expect("passes");
        let (width, height) = (16_u32, 4_u32);
        let camera = camera_texture(&gpu, width, height);
        // A ramp of code values on each row, a channel at a time.
        let code = |x: u32, y: u32| -> [u8; 4] {
            let v = u8::try_from((x * 255 / (width - 1)).min(255)).expect("byte");
            match y % 4 {
                0 => [v, v, v, 255],
                1 => [v, 40, 200, 255],
                2 => [120, v, 30, 255],
                _ => [60, 180, v, 255],
            }
        };
        upload(&gpu, &camera, width, height, &code);
        for profile in [
            CameraProfile::SonySLog3Cine,
            CameraProfile::SonySLog2,
            CameraProfile::BlackmagicFilmGen5,
            CameraProfile::SonyHlg,
        ] {
            let converted =
                ConvertedPicture::new(&gpu.device, &gpu.queue, &passes, &camera, &profile, None)
                    .expect("converted");
            let mut encoder = gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            converted.encode(&gpu.device, &passes, &mut encoder);
            gpu.submit([encoder.finish()]);
            let out = read_back(&gpu, converted.texture());
            let (curve, p, scale) = built_in(&profile).expect("built in");
            let matrix = to_rec709(&p);
            for y in 0..height {
                for x in 0..width {
                    let c = code(x, y);
                    let want = convert(
                        curve,
                        &matrix,
                        scale,
                        [0, 1, 2].map(|i| f64::from(c[i]) / 255.0),
                    );
                    let at = ((y * width + x) * 4) as usize;
                    for i in 0..3 {
                        let got = f64::from(out[at + i]) / 255.0;
                        assert!(
                            near(got, want[i], 1.5 / 255.0),
                            "{profile:?} at ({x},{y}) channel {i}: {got} vs {}",
                            want[i]
                        );
                    }
                }
            }
        }
    }

    /// A camera's LUT file is applied to its code values as they are: an
    /// identity LUT gives the picture back.
    #[test]
    fn an_identity_lut_gives_the_camera_picture_back() {
        use std::fmt::Write as _;
        let Ok(gpu) = crate::renderer::context::GpuContext::new_headless() else {
            return;
        };
        let dir = tempfile::tempdir().expect("folder");
        let path = dir.path().join("identity.cube");
        let size = 17;
        let mut cube = format!("LUT_3D_SIZE {size}\n");
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    let n = f64::from(size - 1);
                    writeln!(
                        cube,
                        "{} {} {}",
                        f64::from(r) / n,
                        f64::from(g) / n,
                        f64::from(b) / n
                    )
                    .expect("a string takes it");
                }
            }
        }
        std::fs::write(&path, cube).expect("written");
        let passes = ProfilePasses::new(&gpu.device).expect("passes");
        let (width, height) = (32_u32, 2_u32);
        let camera = camera_texture(&gpu, width, height);
        let code = |x: u32, y: u32| -> [u8; 4] {
            let v = u8::try_from(x * 8 + 3).expect("byte");
            if y == 0 {
                [v, 255 - v, v / 2, 255]
            } else {
                [17, v, 250, 255]
            }
        };
        upload(&gpu, &camera, width, height, &code);
        let profile = CameraProfile::Lut {
            file: "identity.cube".into(),
        };
        let converted = ConvertedPicture::new(
            &gpu.device,
            &gpu.queue,
            &passes,
            &camera,
            &profile,
            Some(&path),
        )
        .expect("converted");
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        converted.encode(&gpu.device, &passes, &mut encoder);
        gpu.submit([encoder.finish()]);
        let out = read_back(&gpu, converted.texture());
        for y in 0..height {
            for x in 0..width {
                let c = code(x, y);
                let at = ((y * width + x) * 4) as usize;
                for i in 0..3 {
                    let (got, want) = (i32::from(out[at + i]), i32::from(c[i]));
                    assert!(
                        (got - want).abs() <= 1,
                        "({x},{y}) channel {i}: {got} vs {want}"
                    );
                }
            }
        }
    }

    fn camera_texture(
        gpu: &crate::renderer::context::GpuContext,
        width: u32,
        height: u32,
    ) -> wgpu::Texture {
        gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("test camera"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[wgpu::TextureFormat::Rgba8Unorm],
        })
    }

    fn upload(
        gpu: &crate::renderer::context::GpuContext,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
        code: &dyn Fn(u32, u32) -> [u8; 4],
    ) {
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                data.extend_from_slice(&code(x, y));
            }
        }
        gpu.queue.write_texture(
            texture.as_image_copy(),
            &data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            texture.size(),
        );
    }

    fn read_back(gpu: &crate::renderer::context::GpuContext, texture: &wgpu::Texture) -> Vec<u8> {
        let (width, height) = (texture.width(), texture.height());
        let row = (width * 4).div_ceil(256) * 256;
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("read back"),
            size: u64::from(row * height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(height),
                },
            },
            texture.size(),
        );
        gpu.submit([encoder.finish()]);
        buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
        let mapped = buffer.slice(..).get_mapped_range().expect("mapped");
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            let start = (y * row) as usize;
            pixels.extend_from_slice(&mapped[start..start + (width * 4) as usize]);
        }
        pixels
    }
}
