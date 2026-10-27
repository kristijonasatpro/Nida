//! Deck constructors — creating decks from shaders, videos, images, cameras, and solid colors.

use super::{
    svg, Deck, DeckSource, Effect, ExternalSourceKind, PassBuffer, ScalingMode, VideoStagingBuffers,
};
use crate::isf::{compile_glsl_compute_to_spirv, compile_glsl_to_spirv, ISFMetadata, ISFShader};
use crate::params::ShaderParams;
use crate::renderer::{
    BlitPipeline, ComputePipeline, DispatchMode, GpuContext, HapConvertPipeline, UnifiedPipeline,
};
use crate::video::{hap::HapPlayer, HapTextureFormat, VideoDecodeHandle, VideoPlayer};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

/// Create the pair of blit pipelines used by every external/live source deck:
/// `(replace, over_black)`. The REPLACE pipeline writes the source's straight
/// RGBA verbatim (used when the deck is flagged `transparent`, preserving alpha);
/// the `ALPHA_BLENDING` pipeline flattens the source over an opaque black clear (the
/// default, so an HTML source with alpha<1 stays opaque). See /spec/html-source.md §2.
fn external_blit_pipelines(context: &GpuContext) -> Result<(BlitPipeline, BlitPipeline)> {
    let replace = BlitPipeline::new(&context.device, context.compositing_format)?;
    let over_black = BlitPipeline::with_blend(
        &context.device,
        context.compositing_format,
        wgpu::BlendState::ALPHA_BLENDING,
    )?;
    Ok((replace, over_black))
}

/// Upload straight-alpha RGBA pixels as an image deck's source texture.
///
/// Shared with the SVG re-rasterization path, which replaces this texture
/// whenever the master resolution changes.
pub(super) fn upload_image_texture(
    context: &GpuContext,
    rgba: &image::RgbaImage,
) -> (wgpu::Texture, wgpu::TextureView) {
    let (img_w, img_h) = rgba.dimensions();
    let texture = context.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Image Source Texture"),
        size: wgpu::Extent3d {
            width: img_w,
            height: img_h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    context.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4 * img_w),
            rows_per_image: Some(img_h),
        },
        wgpu::Extent3d {
            width: img_w,
            height: img_h,
            depth_or_array_layers: 1,
        },
    );

    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

/// Load ISF IMPORTED images from metadata and create GPU textures.
/// Returns (name, texture, view) sorted alphabetically by name for deterministic binding order.
///
/// PNG decoding runs in parallel across threads; GPU uploads are sequential.
pub(crate) fn load_imported_textures(
    metadata: &ISFMetadata,
    shader_file_path: Option<&str>,
    context: &GpuContext,
) -> Vec<(String, wgpu::Texture, wgpu::TextureView)> {
    let imported = match &metadata.imported {
        Some(map) if !map.is_empty() => map,
        _ => return Vec::new(),
    };

    let shader_dir = shader_file_path.map_or(std::path::Path::new("."), |p| {
        std::path::Path::new(p)
            .parent()
            .unwrap_or(std::path::Path::new("."))
    });

    let mut entries: Vec<_> = imported.iter().collect();
    entries.sort_by_key(|(name, _)| (*name).clone());

    // Collect paths for parallel decode
    let load_list: Vec<_> = entries
        .iter()
        .filter_map(|(name, import_def)| {
            let rel_path = import_def.path.as_ref()?;
            Some(((*name).clone(), shader_dir.join(rel_path)))
        })
        .collect();

    if load_list.is_empty() {
        return Vec::new();
    }

    let t0 = Instant::now();

    // Parallel PNG decode on threads, sequential GPU upload
    let decoded: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = load_list
            .iter()
            .map(|(name, path)| {
                let name = name.clone();
                let path = path.clone();
                s.spawn(move || match image::open(&path) {
                    Ok(img) => Some((name, img.to_rgba8())),
                    Err(e) => {
                        log::warn!(
                            "IMPORTED '{}': failed to load '{}': {}",
                            name,
                            path.display(),
                            e
                        );
                        None
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|h| h.join().ok().flatten())
            .collect()
    });

    // GPU upload (fast, sequential)
    let mut result = Vec::with_capacity(decoded.len());
    for (name, img) in &decoded {
        let (w, h) = img.dimensions();
        let texture = context.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(&format!("Imported: {name}")),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        context.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            img,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * w),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        result.push((name.clone(), texture, view));
    }

    // Sort by name for deterministic binding order (threads may complete out of order)
    result.sort_by(|a, b| a.0.cmp(&b.0));

    let elapsed = t0.elapsed();
    log::info!(
        "IMPORTED: loaded {} textures in {:.0?} (parallel decode)",
        result.len(),
        elapsed
    );

    result
}

impl Deck {
    /// Create a new deck from an ISF shader
    ///
    /// # Errors
    ///
    /// Returns an error if the ISF fragment source fails to compile to SPIR-V,
    /// or if the render pipeline cannot be created.
    pub fn new(context: &GpuContext, shader: ISFShader, width: u32, height: u32) -> Result<Self> {
        // Compile to SPIR-V
        let spirv = compile_glsl_to_spirv(&shader.fragment_source, &shader.name())
            .context("Failed to compile shader to SPIR-V")?;

        // Extract passes from metadata
        let passes = shader.metadata.passes.clone().unwrap_or_default();

        // Create render target textures (two for ping-pong effect chain)
        let texture = context.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Deck Texture (Linear)"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: context.compositing_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let texture_b = context.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Deck Texture B (Linear)"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: context.compositing_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let texture_b_view = texture_b.create_view(&wgpu::TextureViewDescriptor::default());

        // Create pass buffers (with ping-pong for persistent buffers)
        let mut pass_buffers = HashMap::new();

        for pass in &passes {
            let target_name = match &pass.target {
                Some(name) => name.clone(),
                None => continue,
            };

            let pass_width = Self::parse_size_expression(pass.width.as_deref(), width);
            let pass_height = Self::parse_size_expression(pass.height.as_deref(), height);
            let is_persistent = pass.persistent.unwrap_or(false);

            // All pass buffers use the unified color-path format. ISF `"FLOAT": true`
            // is still parsed but no longer selects a format — Rgba16Float is both
            // filterable and blendable, which Rgba32Float was not.
            let format = context.compositing_format;

            let tex_a = context.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(&format!("Pass Buffer A: {target_name}")),
                size: wgpu::Extent3d {
                    width: pass_width,
                    height: pass_height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view_a = tex_a.create_view(&wgpu::TextureViewDescriptor::default());

            // Every pass buffer is double-buffered, persistent or not.
            //
            // The bind group binds *all* pass buffers as sampled textures on
            // every pass, so a single-textured target would be simultaneously
            // COLOR_TARGET and RESOURCE in its own pass — which wgpu rejects
            // outright. Persistent buffers were already safe because they
            // ping-pong; non-persistent ones aliased and aborted the process
            // the first time anyone declared one.
            let (tex_b_buf, view_b) = {
                let tex = context.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(&format!("Pass Buffer B: {target_name}")),
                    size: wgpu::Extent3d {
                        width: pass_width,
                        height: pass_height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
                (Some(tex), Some(view))
            };

            pass_buffers.insert(
                target_name.clone(),
                PassBuffer {
                    name: target_name,
                    texture_a: tex_a,
                    view_a,
                    texture_b: tex_b_buf,
                    view_b,
                    persistent: is_persistent,
                    read_idx: 0,
                },
            );
        }

        // Load ISF IMPORTED images
        let imported_textures =
            load_imported_textures(&shader.metadata, shader.file_path.as_deref(), context);

        // Create preprocessor texture slots from ISF PREPROCESSORS declarations
        let preprocessor_textures: Vec<super::PreprocessorSlot> = shader
            .metadata
            .preprocessors
            .iter()
            .map(|pp| {
                let texture = context.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(&format!("Preprocessor: {}", pp.name)),
                    size: wgpu::Extent3d {
                        width: 1,
                        height: 1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    // Data texture (packed analyzer output) — NOT part of the
                    // color path. Format is the encoding; do not make this float.
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                super::PreprocessorSlot {
                    name: pp.name.clone(),
                    analyzer_type: pp.preprocessor_type.clone(),
                    options: pp.options.clone(),
                    texture,
                    view,
                }
            })
            .collect();

        let pipeline = UnifiedPipeline::new(
            &context.device,
            &spirv,
            context.compositing_format,
            false,
            pass_buffers.len(),
            imported_textures.len(),
            preprocessor_textures.len(),
        )
        .context("Failed to create shader pipeline")?;

        let now = Instant::now();
        let source_name = shader.name();

        let inputs = shader.metadata.inputs.as_deref().unwrap_or(&[]);
        let generator_params = ShaderParams::from_inputs(inputs);
        let generator_phase_inputs = shader.metadata.phase_inputs.clone();

        let source = DeckSource::Shader {
            shader,
            pipeline,
            pass_buffers,
            passes,
            imported_textures,
            preprocessor_textures,
        };

        let source_path = match &source {
            DeckSource::Shader { shader, .. } => shader.file_path.clone(),
            _ => None,
        };

        let uuid = super::generate_short_uuid();
        let param_prefix = format!("deck_{uuid}");

        Ok(Self {
            uuid,
            param_prefix,
            source_name,
            source_path,
            source,
            generator_params,
            texture,
            texture_view,
            texture_b,
            texture_b_view,
            effects: Vec::new(),
            opacity: 1.0,
            transparent: false,
            auto_playback: crate::engine::value::video::AutoPlayback::default(),
            camera_mode: None,
            camera_device: None,
            camera_colour: crate::engine::value::camera::CameraColour::default(),
            camera_lowest_latency: false,
            camera_profile: crate::engine::value::camera::CameraProfile::Normal,
            render_time: 0.0,
            render_dt: 1.0 / 60.0,
            frame_count: 0,
            last_frame_time: now,
            external_source_view: None,
            depth_rgb_view: None,
            depth_intrinsics: None,
            depth_source_size: None,
            point_cloud_params: crate::depth::point_cloud::PointCloudParams::default(),
            point_cloud_pipeline: None,
            depth_prepro: None,
            screen_capture: None,
            tap: None,
            fps_smoothed: 0.0,
            phase_accumulators: [0.0; 4],
            generator_phase_inputs,
            analyzers: crate::analyzer::DeckAnalyzers::new(),
            gpu_error: None,
            rendered_still: None,
            texture_generation: 0,
            visible: None,
            drawn_visible: None,
            drawn_share: 1.0,
            placed: None,
            frames_uploaded: 0,
            render_scale: super::DeckRenderScale::Full,
            master_size: (width, height),
        })
    }

    /// Parse ISF size expressions like "$WIDTH", "$WIDTH/2", "1024", etc.
    pub(crate) fn parse_size_expression(expr: Option<&str>, base_size: u32) -> u32 {
        match expr {
            None => base_size,
            Some(s) => {
                let s = s.trim();
                if s == "$WIDTH" || s == "$HEIGHT" {
                    base_size
                } else if s.starts_with("$WIDTH/") || s.starts_with("$HEIGHT/") {
                    let divisor: u32 = s
                        .split('/')
                        .nth(1)
                        .and_then(|d| d.trim().parse().ok())
                        .unwrap_or(1);
                    base_size / divisor.max(1)
                } else if s.starts_with("$WIDTH*") || s.starts_with("$HEIGHT*") {
                    let multiplier: u32 = s
                        .split('*')
                        .nth(1)
                        .and_then(|m| m.trim().parse().ok())
                        .unwrap_or(1);
                    base_size * multiplier
                } else {
                    s.parse().unwrap_or(base_size)
                }
            }
        }
    }

    /// Add an effect (ISF filter) to this deck's effect chain: at its end,
    /// but before the layer's own mask, which stays last.
    pub fn add_effect(&mut self, effect: Effect) {
        self.effects.push(effect);
        self.settle_layer_mask();
    }

    /// Keep the layer's own mask whole, whatever moved, pasted or restored
    /// the chain: one effect carrying it — the first with a mask, else the
    /// first — last in the chain, the hole going through all before it.
    pub fn settle_layer_mask(&mut self) {
        use crate::engine::value::mask::LAYER_MASK_EFFECT;
        let mut carriers = Vec::new();
        let mut at = 0;
        while at < self.effects.len() {
            if self.effects[at].shader.name() == LAYER_MASK_EFFECT {
                carriers.push(self.effects.remove(at));
            } else {
                at += 1;
            }
        }
        if carriers.is_empty() {
            return;
        }
        let kept = carriers.iter().position(|e| e.mask.is_some()).unwrap_or(0);
        self.effects.push(carriers.swap_remove(kept));
    }

    /// Remove an effect from this deck's effect chain
    pub fn remove_effect(&mut self, index: usize) -> Option<Effect> {
        if index < self.effects.len() {
            Some(self.effects.remove(index))
        } else {
            None
        }
    }

    /// Create a new deck from a video file.
    /// Auto-detects HAP codec and uses GPU-native `BCn` path when available.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened as a HAP or generic video
    /// stream, or if the blit/HAP-convert pipelines cannot be created.
    pub fn new_from_video<P: AsRef<Path>>(
        context: &GpuContext,
        path: P,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        let source_path_str = path.as_ref().to_string_lossy().to_string();
        let source_name = path
            .as_ref()
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("video")
            .to_string();

        let gpu_has_bc = context
            .device
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC);
        let hap_format = if gpu_has_bc {
            crate::video::detect_hap_codec(&path).ok().flatten()
        } else {
            None
        };

        let source = if let Some(hap_fmt) = hap_format {
            let mut player = HapPlayer::new(&path, hap_fmt)?;
            // A clip waits on its first frame until it is played or goes on
            // air, as vMix adds one.
            player.set_playing(false);
            player.playback.show_frame = true;
            let vid_w = player.width();
            let vid_h = player.height();
            let tex_format = hap_fmt.wgpu_format();
            // Compressed textures come in whole 4×4 blocks: a picture of
            // another size (1918 wide, say) fills part of a larger one.
            let (tex_w, tex_h) = (vid_w.div_ceil(4) * 4, vid_h.div_ceil(4) * 4);

            let video_texture = context.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("HAP Video Texture"),
                size: wgpu::Extent3d {
                    width: tex_w,
                    height: tex_h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: tex_format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let video_texture_view =
                video_texture.create_view(&wgpu::TextureViewDescriptor::default());

            let (alpha_texture, alpha_texture_view) =
                if matches!(hap_fmt, HapTextureFormat::Bc3YCoCg) {
                    let alpha_tex = context.device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("HAP Alpha Texture"),
                        size: wgpu::Extent3d {
                            width: tex_w,
                            height: tex_h,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Bc4RUnorm,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                        view_formats: &[],
                    });
                    let alpha_view = alpha_tex.create_view(&wgpu::TextureViewDescriptor::default());
                    (Some(alpha_tex), Some(alpha_view))
                } else {
                    (None, None)
                };

            let dummy_alpha = context.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("HAP Dummy Alpha"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            context.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &dummy_alpha,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &[255u8],
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(1),
                    rows_per_image: Some(1),
                },
                wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
            let dummy_alpha_view = dummy_alpha.create_view(&wgpu::TextureViewDescriptor::default());

            let convert_pipeline =
                HapConvertPipeline::new(&context.device, context.compositing_format)?;
            let blit_pipeline = BlitPipeline::new(&context.device, context.compositing_format)?;

            log::info!("Using HAP GPU path for '{source_name}' ({hap_fmt:?})");

            let blocks_x = vid_w.div_ceil(4);
            let blocks_y = vid_h.div_ceil(4);
            let color_bpr = blocks_x * hap_fmt.block_bytes();
            let staging =
                VideoStagingBuffers::new(&context.device, color_bpr, blocks_y, "HAP Color");

            let alpha_staging = if matches!(hap_fmt, HapTextureFormat::Bc3YCoCg) {
                let alpha_bpr = blocks_x * HapTextureFormat::Bc4.block_bytes();
                Some(VideoStagingBuffers::new(
                    &context.device,
                    alpha_bpr,
                    blocks_y,
                    "HAP Alpha",
                ))
            } else {
                None
            };

            let handle = VideoDecodeHandle::spawn_hap(player);
            let audio = open_clip_audio(path.as_ref(), &handle);

            DeckSource::HapVideo {
                handle,
                audio,
                texture: video_texture,
                texture_view: video_texture_view,
                alpha_texture,
                alpha_texture_view,
                dummy_alpha_view,
                convert_pipeline,
                blit_pipeline,
                hap_format: hap_fmt,
                source_width: vid_w,
                source_height: vid_h,
                // A new picture fits the frame whole, as vMix adds it: nothing
                // cut off, the bars left transparent.
                scaling_mode: ScalingMode::Fit,
                staging,
                alpha_staging,
            }
        } else {
            // A clip waits on its first frame until it is played or goes on
            // air, as vMix adds one. A sound file is a clip with a clear
            // picture.
            let sound_only = crate::video::is_sound_only(path.as_ref());
            let (handle, vid_w, vid_h) = if sound_only {
                let mut player = crate::video::SoundOnlyPlayer::open(path.as_ref())?;
                player.playback.playing = false;
                let side = crate::video::SOUND_PICTURE_SIZE;
                (VideoDecodeHandle::spawn_sound_only(player), side, side)
            } else {
                let mut player = VideoPlayer::new(&path)?;
                player.set_playing(false);
                player.playback.show_frame = true;
                let (w, h) = (player.width(), player.height());
                (VideoDecodeHandle::spawn_video(player), w, h)
            };
            let video_texture = context.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Video Frame Texture"),
                size: wgpu::Extent3d {
                    width: vid_w,
                    height: vid_h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let video_texture_view =
                video_texture.create_view(&wgpu::TextureViewDescriptor::default());
            let blit_pipeline = BlitPipeline::new(&context.device, context.compositing_format)?;

            let staging = VideoStagingBuffers::new(
                &context.device,
                vid_w * 4, // RGBA = 4 bytes per pixel
                vid_h,
                "Video",
            );

            let audio = open_clip_audio(path.as_ref(), &handle);

            DeckSource::Video {
                handle,
                audio,
                sound_only,
                texture: video_texture,
                texture_view: video_texture_view,
                blit_pipeline,
                source_width: vid_w,
                source_height: vid_h,
                // A new picture fits the frame whole, as vMix adds it: nothing
                // cut off, the bars left transparent.
                scaling_mode: ScalingMode::Fit,
                staging,
            }
        };

        Ok(Self::build_media_deck(
            context,
            source_name,
            Some(source_path_str),
            source,
            width,
            height,
        ))
    }

    /// Create a new deck from an image file (PNG, JPG, BMP, SVG, etc.)
    ///
    /// SVG is rasterized rather than decoded: it has no native pixel size, so
    /// it is rendered to fill a `width × height` deck and re-rendered if that
    /// changes. See [`svg`](crate::deck::svg).
    ///
    /// # Errors
    ///
    /// Returns an error if the image cannot be opened, decoded or rasterized,
    /// or if the blit pipeline cannot be created.
    pub fn new_from_image<P: AsRef<Path>>(
        context: &GpuContext,
        path: P,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        let path = path.as_ref();
        let source_path_str = path.to_string_lossy().to_string();
        let source_name = path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("image")
            .to_string();

        if svg::is_svg_path(path) {
            let tree = svg::parse_file(path)?;
            let rgba = svg::rasterize(&tree, width, height)?;
            return Self::new_image_deck(
                context,
                &rgba,
                Some(tree),
                source_name,
                source_path_str,
                width,
                height,
            );
        }

        let img = image::open(path)
            .with_context(|| format!("Failed to load image: {}", path.display()))?;
        let rgba = img.to_rgba8();
        Self::new_image_deck(
            context,
            &rgba,
            None,
            source_name,
            source_path_str,
            width,
            height,
        )
    }

    /// A picture deck of `rgba`, named `name`, its file `path` (a PDF
    /// talk's page is put in it as drawn).
    ///
    /// # Errors
    ///
    /// Returns an error if the blit pipeline cannot be created.
    pub fn new_from_pixels(
        context: &GpuContext,
        rgba: &image::RgbaImage,
        name: String,
        path: &str,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        Self::new_image_deck(context, rgba, None, name, path.to_string(), width, height)
    }

    /// A Slides layer (see [`super::slides`]): `talks` in running order,
    /// nothing on — or `resume` started at once, a saved show opened.
    pub fn new_slides(
        context: &GpuContext,
        talks: Vec<crate::engine::value::slides::Talk>,
        settings: crate::engine::value::slides::SlidesSettings,
        resume: Option<super::slides::Resume>,
        next_pick: Option<String>,
        width: u32,
        height: u32,
    ) -> Self {
        let slides = super::slides::Slides::new(
            context,
            talks,
            settings,
            (width, height),
            resume,
            next_pick,
        );
        Self::build_media_deck(
            context,
            "📽 Slides".to_string(),
            None,
            DeckSource::Slides(Box::new(slides)),
            width,
            height,
        )
    }

    /// A playlist of clips and pictures (see [`super::playlist`]), stopped
    /// on its first item.
    pub fn new_playlist(
        context: &GpuContext,
        items: Vec<crate::engine::value::playlist::PlaylistItem>,
        settings: crate::engine::value::playlist::PlaylistSettings,
        width: u32,
        height: u32,
    ) -> Self {
        let list = super::playlist::Playlist::new(context, items, settings, (width, height));
        Self::build_media_deck(
            context,
            "Playlist".to_string(),
            None,
            DeckSource::Playlist(Box::new(list)),
            width,
            height,
        )
    }

    /// A timer (see [`super::timer`]) that has run `elapsed` seconds, paused.
    ///
    /// # Errors
    ///
    /// Returns an error if the pipeline that places its numbers cannot be
    /// made.
    pub fn new_timer(
        context: &GpuContext,
        settings: crate::engine::value::timer::TimerSettings,
        elapsed: f64,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        let name = format!("⏱ {}", settings.mode.label());
        let timer = super::timer::Timer::new(context, settings, elapsed, (width, height))?;
        Ok(Self::build_media_deck(
            context,
            name,
            None,
            DeckSource::Timer(Box::new(timer)),
            width,
            height,
        ))
    }

    /// A presenter view (see [`super::presenter`]) of the PowerPoint input
    /// whose layer is `follows`.
    ///
    /// # Errors
    ///
    /// Returns an error if its drawing pipelines cannot be made.
    pub fn new_presenter(
        context: &GpuContext,
        follows: String,
        name: &str,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        let presenter = super::presenter::Presenter::new(context, follows, (width, height))?;
        Ok(Self::build_media_deck(
            context,
            format!("🗒 {name}"),
            None,
            DeckSource::Presenter(Box::new(presenter)),
            width,
            height,
        ))
    }

    /// A layer whose input could not be opened (see [`super::OfflineSource`]):
    /// `name` as saved, drawn clear. A file's path stays its source path, to
    /// show which file is missing.
    pub fn new_offline(
        context: &GpuContext,
        name: String,
        saved: crate::scene::SourceConfig,
        reason: String,
        width: u32,
        height: u32,
    ) -> Self {
        use crate::scene::SourceConfig;
        let path = match &saved {
            SourceConfig::Video { path, .. }
            | SourceConfig::Image { path, .. }
            | SourceConfig::Shader { path, .. } => Some(path.clone()),
            _ => None,
        };
        Self::build_media_deck(
            context,
            name,
            path,
            DeckSource::Offline(Box::new(super::OfflineSource { saved, reason })),
            width,
            height,
        )
    }

    /// Create a new deck from decoded pixels. `tree` is carried by SVG decks so
    /// a later resolution change can re-render the artwork instead of scaling
    /// up the pixels it happened to be rasterized at first.
    fn new_image_deck(
        context: &GpuContext,
        rgba: &image::RgbaImage,
        tree: Option<usvg::Tree>,
        source_name: String,
        source_path_str: String,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        let (img_w, img_h) = rgba.dimensions();
        let (img_texture, img_texture_view) = upload_image_texture(context, rgba);
        let blit_pipeline = BlitPipeline::new(&context.device, context.compositing_format)?;

        let source = DeckSource::Image {
            texture: img_texture,
            texture_view: img_texture_view,
            blit_pipeline,
            source_width: img_w,
            source_height: img_h,
            // A new picture fits the frame whole, as vMix adds it: nothing
            // cut off, the bars left transparent.
            scaling_mode: ScalingMode::Fit,
            // Vector art is redrawn at other sizes, where antialiased edges
            // can turn translucent, so only raster images are trusted.
            opaque: tree.is_none() && rgba.pixels().all(|p| p[3] == u8::MAX),
            svg: tree.map(Box::new),
        };

        Ok(Self::build_media_deck(
            context,
            source_name,
            Some(source_path_str),
            source,
            width,
            height,
        ))
    }

    /// Create a new deck with a solid color fill
    ///
    /// # Errors
    ///
    /// Never fails today — a solid-color source needs no pipeline. The `Result`
    /// keeps this constructor uniform with the other `new_from_*` constructors.
    pub fn new_solid_color(
        context: &GpuContext,
        color: [f32; 4],
        width: u32,
        height: u32,
    ) -> Result<Self> {
        let source_name = super::colour_name(color);

        let source = DeckSource::SolidColor {
            color: [
                f64::from(color[0]),
                f64::from(color[1]),
                f64::from(color[2]),
                f64::from(color[3]),
            ],
        };

        Ok(Self::build_media_deck(
            context,
            source_name,
            None,
            source,
            width,
            height,
        ))
    }

    /// Create a new deck from a camera source.
    /// The camera is managed by `CameraManager` — this deck reads from the shared texture.
    ///
    /// # Errors
    ///
    /// Returns an error if the external-source blit pipelines cannot be created.
    pub fn new_from_camera(
        context: &GpuContext,
        camera_id: crate::camera::CameraId,
        camera_name: &str,
        source_width: u32,
        source_height: u32,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        let source_name = format!("📹 {camera_name}");
        let (blit_pipeline, blit_pipeline_over_black) = external_blit_pipelines(context)?;

        let source = DeckSource::ExternalSource {
            kind: ExternalSourceKind::Camera(camera_id),
            blit_pipeline,
            blit_pipeline_over_black,
            source_width,
            source_height,
            scaling_mode: ScalingMode::default(),
        };

        Ok(Self::build_media_deck(
            context,
            source_name,
            None,
            source,
            width,
            height,
        ))
    }

    /// Create a new deck from a screen or window capture.
    /// The capture is managed by `ScreenCaptureManager` — this deck reads from
    /// the shared texture. See spec/screen-capture.md.
    ///
    /// # Errors
    ///
    /// Returns an error if the external-source blit pipelines cannot be created.
    pub fn new_from_screen_capture(
        context: &GpuContext,
        state: crate::deck::ScreenCaptureState,
        target_label: &str,
        source_width: u32,
        source_height: u32,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        let source_name = format!("🖥 {target_label}");
        let (blit_pipeline, blit_pipeline_over_black) = external_blit_pipelines(context)?;

        let source = DeckSource::ExternalSource {
            kind: ExternalSourceKind::ScreenCapture(state.capture_id),
            blit_pipeline,
            blit_pipeline_over_black,
            source_width,
            source_height,
            scaling_mode: ScalingMode::default(),
        };

        let mut deck = Self::build_media_deck(context, source_name, None, source, width, height);
        deck.screen_capture = Some(state);
        Ok(deck)
    }

    /// Create a new deck that re-enters Varda's own output as a source.
    ///
    /// The tap owns no device and holds no handle: the mixer supplies a texture
    /// view each frame, and a source that cannot be resolved renders black.
    /// See spec/program-tap.md.
    ///
    /// # Errors
    ///
    /// Returns an error if the external-source blit pipelines cannot be created.
    pub fn new_from_tap(
        context: &GpuContext,
        source: crate::deck::TapSource,
        label: &str,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        let source_name = format!("🔁 {label}");
        let (blit_pipeline, blit_pipeline_over_black) = external_blit_pipelines(context)?;

        let deck_source = DeckSource::ExternalSource {
            kind: ExternalSourceKind::Tap,
            blit_pipeline,
            blit_pipeline_over_black,
            // A tap always matches the render resolution, so the default
            // scaling mode is an exact 1:1 blit until the user changes it.
            source_width: width,
            source_height: height,
            scaling_mode: ScalingMode::default(),
        };

        let mut deck =
            Self::build_media_deck(context, source_name, None, deck_source, width, height);
        deck.tap = Some(crate::deck::TapState { source });
        Ok(deck)
    }

    /// Create a new deck from a depth sensor (Kinect/LIDAR point cloud).
    /// The sensor is managed by `DepthSensorManager`; this deck reprojects the
    /// shared depth texture as a point cloud. See spec/depth-sensors.md.
    ///
    /// # Errors
    ///
    /// Returns an error if the external-source blit pipelines cannot be created.
    pub fn new_from_depth_sensor(
        context: &GpuContext,
        sensor_id: crate::depth::DepthSensorId,
        sensor_name: &str,
        source_width: u32,
        source_height: u32,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        let source_name = format!("📡 {sensor_name}");
        let (blit_pipeline, blit_pipeline_over_black) = external_blit_pipelines(context)?;

        let source = DeckSource::ExternalSource {
            kind: ExternalSourceKind::DepthSensor(sensor_id),
            blit_pipeline,
            blit_pipeline_over_black,
            source_width,
            source_height,
            scaling_mode: ScalingMode::default(),
        };

        Ok(Self::build_media_deck(
            context,
            source_name,
            None,
            source,
            width,
            height,
        ))
    }

    /// Shared helper to build a Deck from a pre-built `DeckSource` with standard render targets.
    fn build_media_deck(
        context: &GpuContext,
        source_name: String,
        source_path: Option<String>,
        source: DeckSource,
        width: u32,
        height: u32,
    ) -> Self {
        let texture = context.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Deck Texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: context.compositing_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let texture_b = context.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Deck Texture B"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: context.compositing_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let texture_b_view = texture_b.create_view(&wgpu::TextureViewDescriptor::default());

        let now = Instant::now();
        let generator_params = ShaderParams::from_inputs(&[]);

        let uuid = super::generate_short_uuid();
        let param_prefix = format!("deck_{uuid}");

        Self {
            uuid,
            param_prefix,
            source_name,
            source_path,
            source,
            generator_params,
            texture,
            texture_view,
            texture_b,
            texture_b_view,
            effects: Vec::new(),
            opacity: 1.0,
            transparent: false,
            auto_playback: crate::engine::value::video::AutoPlayback::default(),
            camera_mode: None,
            camera_device: None,
            camera_colour: crate::engine::value::camera::CameraColour::default(),
            camera_lowest_latency: false,
            camera_profile: crate::engine::value::camera::CameraProfile::Normal,
            render_time: 0.0,
            render_dt: 1.0 / 60.0,
            frame_count: 0,
            last_frame_time: now,
            external_source_view: None,
            depth_rgb_view: None,
            depth_intrinsics: None,
            depth_source_size: None,
            point_cloud_params: crate::depth::point_cloud::PointCloudParams::default(),
            point_cloud_pipeline: None,
            depth_prepro: None,
            screen_capture: None,
            tap: None,
            fps_smoothed: 0.0,
            phase_accumulators: [0.0; 4],
            generator_phase_inputs: None,
            analyzers: crate::analyzer::DeckAnalyzers::new(),
            gpu_error: None,
            rendered_still: None,
            texture_generation: 0,
            visible: None,
            drawn_visible: None,
            drawn_share: 1.0,
            placed: None,
            frames_uploaded: 0,
            render_scale: super::DeckRenderScale::Full,
            master_size: (width, height),
        }
    }

    /// Create a new deck from an external source (NDI, Syphon, SRT, HLS, DASH, RTMP).
    ///
    /// # Errors
    ///
    /// Returns an error if the external-source blit pipelines cannot be created.
    pub fn new_from_external(
        context: &GpuContext,
        kind: ExternalSourceKind,
        display_name: String,
        source_width: u32,
        source_height: u32,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        let (blit_pipeline, blit_pipeline_over_black) = external_blit_pipelines(context)?;

        let source = DeckSource::ExternalSource {
            kind,
            blit_pipeline,
            blit_pipeline_over_black,
            source_width,
            source_height,
            scaling_mode: ScalingMode::default(),
        };

        Ok(Self::build_media_deck(
            context,
            display_name,
            None,
            source,
            width,
            height,
        ))
    }

    /// Create a new deck from an NDI network source.
    ///
    /// # Errors
    ///
    /// Propagates errors from [`Deck::new_from_external`].
    pub fn new_from_ndi(
        context: &GpuContext,
        receiver_idx: usize,
        source_name: &str,
        source_width: u32,
        source_height: u32,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        Self::new_from_external(
            context,
            ExternalSourceKind::Ndi(receiver_idx),
            format!("📡 {source_name}"),
            source_width,
            source_height,
            width,
            height,
        )
    }

    /// Create a new deck from a Syphon server (macOS inter-app sharing).
    ///
    /// # Errors
    ///
    /// Propagates errors from [`Deck::new_from_external`].
    pub fn new_from_syphon(
        context: &GpuContext,
        client_idx: usize,
        server_name: &str,
        source_width: u32,
        source_height: u32,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        Self::new_from_external(
            context,
            ExternalSourceKind::Syphon(client_idx),
            format!("🔗 {server_name}"),
            source_width,
            source_height,
            width,
            height,
        )
    }

    /// Create a new deck from an SRT network source.
    ///
    /// # Errors
    ///
    /// Propagates errors from [`Deck::new_from_external`].
    pub fn new_from_srt(
        context: &GpuContext,
        receiver_idx: usize,
        url: &str,
        source_width: u32,
        source_height: u32,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        Self::new_from_external(
            context,
            ExternalSourceKind::Srt(receiver_idx),
            format!("📺 {url}"),
            source_width,
            source_height,
            width,
            height,
        )
    }

    /// Create a new deck from an HLS stream source.
    ///
    /// # Errors
    ///
    /// Propagates errors from [`Deck::new_from_external`].
    pub fn new_from_hls(
        context: &GpuContext,
        receiver_idx: usize,
        url: &str,
        source_width: u32,
        source_height: u32,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        Self::new_from_external(
            context,
            ExternalSourceKind::Hls(receiver_idx),
            format!("📡 {url}"),
            source_width,
            source_height,
            width,
            height,
        )
    }

    /// Create a new deck from a DASH stream source.
    ///
    /// # Errors
    ///
    /// Propagates errors from [`Deck::new_from_external`].
    pub fn new_from_dash(
        context: &GpuContext,
        receiver_idx: usize,
        url: &str,
        source_width: u32,
        source_height: u32,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        Self::new_from_external(
            context,
            ExternalSourceKind::Dash(receiver_idx),
            format!("📡 {url}"),
            source_width,
            source_height,
            width,
            height,
        )
    }

    /// Create a new deck from an RTMP stream source.
    ///
    /// # Errors
    ///
    /// Propagates errors from [`Deck::new_from_external`].
    pub fn new_from_rtmp(
        context: &GpuContext,
        receiver_idx: usize,
        url: &str,
        source_width: u32,
        source_height: u32,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        Self::new_from_external(
            context,
            ExternalSourceKind::Rtmp(receiver_idx),
            format!("📺 {url}"),
            source_width,
            source_height,
            width,
            height,
        )
    }

    /// Create a new deck from an HTML source (Servo offscreen render).
    ///
    /// # Errors
    ///
    /// Propagates errors from [`Deck::new_from_external`].
    pub fn new_from_html(
        context: &GpuContext,
        instance_idx: usize,
        url: &str,
        source_width: u32,
        source_height: u32,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        Self::new_from_external(
            context,
            ExternalSourceKind::Html(instance_idx),
            format!("🌐 {url}"),
            source_width,
            source_height,
            width,
            height,
        )
    }

    /// Create a new deck from a GLSL compute shader (.comp file)
    ///
    /// # Errors
    ///
    /// Returns an error if the shader has no `COMPUTE` configuration block, if
    /// the GLSL fails to compile to SPIR-V, or if the compute pipeline cannot
    /// be created.
    pub fn new_from_compute_shader(
        context: &GpuContext,
        shader: ISFShader,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        let compute_config = shader
            .metadata
            .compute
            .as_ref()
            .context("Compute shader missing COMPUTE configuration")?;

        // Compile GLSL compute to SPIR-V
        let spirv = compile_glsl_compute_to_spirv(&shader.fragment_source, &shader.name())
            .context("Failed to compile compute shader to SPIR-V")?;

        let workgroup_size = compute_config.workgroup_size;
        let dispatch_mode = match compute_config.dispatch.as_str() {
            "custom" => {
                log::warn!("Custom dispatch mode not yet implemented, using resolution");
                DispatchMode::Resolution
            }
            _ => DispatchMode::Resolution,
        };

        let num_passes = compute_config.num_passes;

        let pipeline = ComputePipeline::new(
            &context.device,
            &spirv,
            width,
            height,
            &shader.metadata.buffers,
            workgroup_size,
            dispatch_mode,
            num_passes,
        )
        .context("Failed to create compute pipeline")?;

        let source_name = shader.name();
        let source_path = shader.file_path.clone();

        let inputs = shader.metadata.inputs.as_deref().unwrap_or(&[]);
        let generator_params = ShaderParams::from_inputs(inputs);
        let generator_phase_inputs = shader.metadata.phase_inputs.clone();

        let source = DeckSource::ComputeShader { shader, pipeline };

        // Create render target textures
        let texture = context.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Deck Texture (Compute)"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: context.compositing_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let texture_b = context.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Deck Texture B (Compute)"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: context.compositing_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let texture_b_view = texture_b.create_view(&wgpu::TextureViewDescriptor::default());

        let now = Instant::now();
        let uuid = super::generate_short_uuid();
        let param_prefix = format!("deck_{uuid}");

        Ok(Self {
            uuid,
            param_prefix,
            source_name,
            source_path,
            source,
            generator_params,
            texture,
            texture_view,
            texture_b,
            texture_b_view,
            effects: Vec::new(),
            opacity: 1.0,
            transparent: false,
            auto_playback: crate::engine::value::video::AutoPlayback::default(),
            camera_mode: None,
            camera_device: None,
            camera_colour: crate::engine::value::camera::CameraColour::default(),
            camera_lowest_latency: false,
            camera_profile: crate::engine::value::camera::CameraProfile::Normal,
            render_time: 0.0,
            render_dt: 1.0 / 60.0,
            frame_count: 0,
            last_frame_time: now,
            external_source_view: None,
            depth_rgb_view: None,
            depth_intrinsics: None,
            depth_source_size: None,
            point_cloud_params: crate::depth::point_cloud::PointCloudParams::default(),
            point_cloud_pipeline: None,
            depth_prepro: None,
            screen_capture: None,
            tap: None,
            fps_smoothed: 0.0,
            phase_accumulators: [0.0; 4],
            generator_phase_inputs,
            analyzers: crate::analyzer::DeckAnalyzers::new(),
            gpu_error: None,
            rendered_still: None,
            texture_generation: 0,
            visible: None,
            drawn_visible: None,
            drawn_share: 1.0,
            placed: None,
            frames_uploaded: 0,
            render_scale: super::DeckRenderScale::Full,
            master_size: (width, height),
        })
    }
}

/// The soundtrack of a video deck, following its playhead, or `None` for a
/// silent file. A soundtrack that cannot be opened costs the clip its sound,
/// never its picture.
fn open_clip_audio(path: &Path, handle: &VideoDecodeHandle) -> Option<crate::audio_mix::ClipAudio> {
    match crate::audio_mix::ClipAudio::open(path, handle.snapshot_cell()) {
        Ok(audio) => audio,
        Err(e) => {
            log::warn!("No sound for '{}': {e}", path.display());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_size_none_returns_base() {
        assert_eq!(Deck::parse_size_expression(None, 1920), 1920);
    }

    #[test]
    fn parse_size_width_variable() {
        assert_eq!(Deck::parse_size_expression(Some("$WIDTH"), 1920), 1920);
        assert_eq!(Deck::parse_size_expression(Some("$HEIGHT"), 1080), 1080);
    }

    #[test]
    fn parse_size_divide() {
        assert_eq!(Deck::parse_size_expression(Some("$WIDTH/2"), 1920), 960);
        assert_eq!(Deck::parse_size_expression(Some("$HEIGHT/4"), 1080), 270);
    }

    #[test]
    fn parse_size_multiply() {
        assert_eq!(Deck::parse_size_expression(Some("$WIDTH*2"), 960), 1920);
        assert_eq!(Deck::parse_size_expression(Some("$HEIGHT*3"), 360), 1080);
    }

    #[test]
    fn parse_size_literal() {
        assert_eq!(Deck::parse_size_expression(Some("512"), 1920), 512);
        assert_eq!(Deck::parse_size_expression(Some("1024"), 1080), 1024);
    }

    #[test]
    fn parse_size_invalid_literal_falls_back() {
        assert_eq!(Deck::parse_size_expression(Some("abc"), 1920), 1920);
    }

    #[test]
    fn parse_size_divide_by_zero_safe() {
        assert_eq!(Deck::parse_size_expression(Some("$WIDTH/0"), 1920), 1920);
    }

    #[test]
    fn parse_size_whitespace_trim() {
        assert_eq!(Deck::parse_size_expression(Some(" $WIDTH "), 1920), 1920);
    }
}
