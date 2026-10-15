//! HAP colour through the GPU: a clip's gamma-encoded colour comes out in
//! the linear light Nida mixes in, as any other clip's does — read as
//! linear, mid-grey came out light grey — and only an alpha plane makes a
//! Hap Q clip transparent (its own A is luma: dark parts turned see-through).

use std::sync::mpsc;

use varda::renderer::context::GpuContext;
use varda::renderer::HapConvertPipeline;
use varda::video::HapTextureFormat;

mod common;
use common::headless_gpu;

const TARGET: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// One 4×4 block of `format` filling a 4×4 texture.
fn block_texture(ctx: &GpuContext, format: wgpu::TextureFormat, block: &[u8]) -> wgpu::TextureView {
    let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 4,
            height: 4,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    ctx.queue.write_texture(
        texture.as_image_copy(),
        block,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(u32::try_from(block.len()).unwrap()),
            rows_per_image: Some(1),
        },
        texture.size(),
    );
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// The pixel a HAP frame of one block comes out as, linear RGBA.
fn convert(
    ctx: &GpuContext,
    format: HapTextureFormat,
    block: &[u8],
    alpha_block: Option<&[u8]>,
) -> [f32; 4] {
    let colour = block_texture(ctx, format.wgpu_format(), block);
    convert_view(ctx, format, &colour, alpha_block, [1.0, 1.0])[1]
}

/// Every pixel of a 4×4 picture from the colour texture `colour`, filling
/// `extent` of it: row 1, left to right.
fn convert_view(
    ctx: &GpuContext,
    format: HapTextureFormat,
    colour: &wgpu::TextureView,
    alpha_block: Option<&[u8]>,
    extent: [f32; 2],
) -> [[f32; 4]; 4] {
    let alpha = match alpha_block {
        Some(a) => block_texture(ctx, HapTextureFormat::Bc4.wgpu_format(), a),
        // What a deck binds without an alpha plane.
        None => block_texture(ctx, wgpu::TextureFormat::Rgba8Unorm, &[255; 64]),
    };
    let target = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 4,
            height: 4,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TARGET,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let pipeline = HapConvertPipeline::new(&ctx.device, TARGET).unwrap();
    pipeline.set_params_with_uv(
        &ctx.queue,
        1.0,
        format.needs_ycocg_convert(),
        alpha_block.is_some(),
        [1.0, 1.0],
        [0.0, 0.0],
        extent,
    );
    let bind_group = pipeline.create_bind_group(&ctx.device, colour, &alpha);
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pipeline.draw(&mut pass, &bind_group);
    }
    let padded = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(padded * 4),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        target.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(4),
            },
        },
        target.size(),
    );
    ctx.submit(std::iter::once(encoder.finish()));
    let (tx, rx) = mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    ctx.device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .ok();
    rx.recv().unwrap().unwrap();
    let data = buffer.slice(..).get_mapped_range().unwrap();
    std::array::from_fn(|x| {
        let at = (padded + 8 * x as u32) as usize;
        std::array::from_fn(|c| {
            half::f16::from_le_bytes([data[at + c * 2], data[at + c * 2 + 1]]).to_f32()
        })
    })
}

fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// A DXT1 block of one 5:6:5 colour, all its pixels that colour.
fn bc1(colour: u16) -> Vec<u8> {
    let mut block = Vec::new();
    block.extend_from_slice(&colour.to_le_bytes());
    block.extend_from_slice(&colour.to_le_bytes());
    block.extend_from_slice(&[0; 4]);
    block
}

/// Mid-grey in 5:6:5: 16, 32, 16 — decoded 132, 130, 132.
const GREY_565: u16 = (16 << 11) | (32 << 5) | 16;

#[test]
fn hap_colour_is_decoded_to_linear_light() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    if !ctx
        .device
        .features()
        .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
    {
        eprintln!("no BC textures: HAP plays in software here");
        return;
    }
    let [r, g, b, a] = convert(&ctx, HapTextureFormat::Bc1, &bc1(GREY_565), None);
    let want = srgb_to_linear(132.0 / 255.0);
    assert!(
        (r - want).abs() < 0.01,
        "red {r}, want {want} (not {})",
        132.0 / 255.0
    );
    assert!(
        (g - srgb_to_linear(130.0 / 255.0)).abs() < 0.01,
        "green {g}"
    );
    assert!((b - want).abs() < 0.01, "blue {b}");
    assert!((a - 1.0).abs() < 1e-3);
}

/// Hap Q: DXT5 holding scaled `YCoCg` — luma in its alpha. A mid-grey luma
/// is mid-grey, in linear light, and opaque; an alpha plane (Hap Q Alpha)
/// is what makes it transparent.
#[test]
fn hap_q_is_opaque_grey_unless_its_alpha_plane_says_otherwise() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    if !ctx
        .device
        .features()
        .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
    {
        return;
    }
    // Luma 128 in the alpha half; neutral chroma (Co, Cg 128), scale 1.
    let mut block = vec![128, 128, 0, 0, 0, 0, 0, 0];
    block.extend(bc1((16 << 11) | (32 << 5)));
    let [r, g, b, a] = convert(&ctx, HapTextureFormat::Bc3YCoCg, &block, None);
    let grey = srgb_to_linear(0.5);
    for (name, v) in [("red", r), ("green", g), ("blue", b)] {
        assert!((v - grey).abs() < 0.03, "{name} {v}, want about {grey}");
    }
    assert!((a - 1.0).abs() < 1e-3, "opaque, not its luma: {a}");

    // A BC4 block of alpha 64 all over.
    let alpha = [64, 64, 0, 0, 0, 0, 0, 0];
    let [.., a] = convert(&ctx, HapTextureFormat::Bc3YCoCg, &block, Some(&alpha));
    assert!((a - 64.0 / 255.0).abs() < 0.01, "the alpha plane's: {a}");
}

/// A picture 6 wide comes in a texture 8 wide — compressed textures come in
/// whole 4×4 blocks — and fills only its part: the padding past it (blue
/// here) is not stretched into the picture.
#[test]
fn a_picture_not_a_whole_number_of_blocks_fills_its_part() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    if !ctx
        .device
        .features()
        .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
    {
        return;
    }
    // Two blocks side by side: red, then blue.
    let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 8,
            height: 4,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: HapTextureFormat::Bc1.wgpu_format(),
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let blocks = [bc1(0xF800), bc1(0x001F)].concat();
    ctx.queue.write_texture(
        texture.as_image_copy(),
        &blocks,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(16),
            rows_per_image: Some(1),
        },
        texture.size(),
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    // Only the red block's half, stretched over the picture.
    let row = convert_view(&ctx, HapTextureFormat::Bc1, &view, None, [0.5, 1.0]);
    for (x, [r, _, b, _]) in row.iter().enumerate() {
        assert!(*r > 0.9 && *b < 0.1, "pixel {x}: {:?}", row[x]);
    }
}
