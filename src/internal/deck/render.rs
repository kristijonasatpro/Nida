//! Deck rendering — source rendering, effect chain, video frame updates, and resize.

use super::{
    Deck, DeckSource, Effect, ExternalSourceKind, PassBuffer, PreprocessorSlot, ScalingMode,
    VideoStagingBuffers,
};
use crate::analyzer::traits::TextureData;
use crate::analyzer::{AnalyzerRegistry, DeckAnalyzers, PreprocessorCategory};
use crate::audio::AudioData;
use crate::isf::{ISFPass, PhaseInput};
use crate::modulation::ModulationEngine;
use crate::params::ShaderParams;
use crate::renderer::region::{draw_within, scissors_for, Area, Pixels};
use crate::renderer::BlitPipeline;
use crate::renderer::{GpuContext, ISFUniforms, UnifiedPipeline};
use anyhow::Result;
use std::collections::HashMap;
use std::time::Instant;

/// A deck's whole picture as one placed draw of its live source (see
/// [`Deck::framing`]).
pub(crate) struct Framing<'a> {
    pub placed: crate::renderer::placed_picture::Placed,
    /// The source, once one is bound.
    pub source: Option<&'a wgpu::TextureView>,
    /// The deck's own frame, as its render scale makes it: the picture
    /// drawn at this size, then shown at the master's, as the deck is.
    pub size: (u32, u32),
}

/// The effects the pass drawing a deck's source does too (see
/// `Deck::fold`): a crop, then a move and scale.
struct Fold {
    crop: Option<crate::renderer::placed_picture::Crop>,
    /// The picture's centre moved by this, and its scale (none: 0 and 1).
    pos: [f32; 2],
    scale: [f32; 2],
    /// Mirrored left for right, top for bottom.
    flip: [bool; 2],
    /// 1: the source drawn over black, as the deck flattens it (see
    /// `Deck::source_draws`); 0: as it comes.
    over_black: f32,
    /// How many of the enabled effects, from the first, it does.
    effects: usize,
}

/// Pixels drawn round each part a pass is limited to, beyond those it
/// touches: the next pass, or the composite, reads it with filtering.
const PART_MARGIN: u32 = 2;

/// How a pass drawing only `within` starts: keeping what its target held
/// outside those parts — the last picture drawn there, which is what the
/// deck's thumbnail shows of a part covered on air — or cleared when it
/// draws everything.
fn keep_or_clear(within: Option<&[Pixels]>, clear: wgpu::Color) -> wgpu::LoadOp<wgpu::Color> {
    if within.is_some() {
        wgpu::LoadOp::Load
    } else {
        wgpu::LoadOp::Clear(clear)
    }
}

/// Upload analyzer texture data to a preprocessor slot's GPU texture.
///
/// If dimensions changed, recreates the texture and view. Otherwise writes data in place.
fn upload_texture_to_slot(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    slot: &mut PreprocessorSlot,
    tex_data: &TextureData,
) {
    if tex_data.width == 0 || tex_data.height == 0 || tex_data.data.is_empty() {
        return;
    }

    let current_size = slot.texture.size();
    if current_size.width != tex_data.width || current_size.height != tex_data.height {
        // Dimensions changed — recreate texture
        let new_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(&format!("Preprocessor: {}", slot.name)),
            size: wgpu::Extent3d {
                width: tex_data.width,
                height: tex_data.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // Data texture (packed analyzer output) — NOT part of the color path.
            // Format is the encoding; do not make this float.
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        slot.view = new_texture.create_view(&wgpu::TextureViewDescriptor::default());
        slot.texture = new_texture;
    }

    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &slot.texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &tex_data.data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4 * tex_data.width),
            rows_per_image: Some(tex_data.height),
        },
        wgpu::Extent3d {
            width: tex_data.width,
            height: tex_data.height,
            depth_or_array_layers: 1,
        },
    );
}

/// Accumulate phase times: for each `PhaseInput`, adds
/// `dt * param_value * multiply_by * scale` to the accumulator.
///
/// Parameter values are the modulated ones, not the stored bases. A shader that declares
/// `PHASE_INPUTS` reads `PHASE_TIME_N` rather than the raw speed uniform, so integrating
/// the base value would make modulation of that parameter invisible.
///
/// See [/spec/phase-accumulators.md](/spec/phase-accumulators.md).
fn accumulate_phase_times(
    accumulators: &mut [f32; 4],
    dt: f32,
    phase_inputs: Option<&[PhaseInput]>,
    params: &mut ShaderParams,
    modulation: &ModulationEngine,
    param_prefix: &str,
) {
    let Some(inputs) = phase_inputs else {
        return;
    };
    for pi in inputs {
        if pi.index < 4 {
            let mut rate = params
                .get_float_modulated(&pi.param, modulation, Some(param_prefix))
                .unwrap_or(1.0);
            for factor in &pi.multiply_by {
                rate *= params
                    .get_float_modulated(factor, modulation, Some(param_prefix))
                    .unwrap_or(1.0);
            }
            accumulators[pi.index] += dt * rate * pi.scale;
        }
    }
}

impl Deck {
    /// Update video frame using double-buffered staging uploads.
    /// Takes the latest decoded frame from the background decode thread
    /// and uploads it to the GPU texture via a pre-allocated mapped buffer.
    ///
    /// # Errors
    ///
    /// Never fails today — the upload path is infallible and non-video sources
    /// are a no-op. The `Result` is kept so callers stay source-compatible if
    /// staging-buffer allocation becomes fallible.
    pub fn update_video_frame(&mut self, encoder: &mut wgpu::CommandEncoder) -> Result<()> {
        match &mut self.source {
            // A frame is taken only when it can be uploaded; one taken while
            // the staging buffer is still on its way back would be lost. A
            // playing clip sends another at once, but a paused one has sent
            // its only frame — the one it is paused on.
            DeckSource::Video {
                ref handle,
                ref texture,
                ref mut staging,
                ..
            } if staging.ready() => {
                if let Some(frame) = handle.take_frame() {
                    let width = handle.width;
                    let height = handle.height;
                    staging.upload(&frame.color_data, texture, width, height, encoder);
                    handle.recycle(frame);
                }
            }
            DeckSource::HapVideo {
                ref mut handle,
                ref texture,
                ref alpha_texture,
                ref mut staging,
                ref mut alpha_staging,
                ..
            } if staging.ready()
                && alpha_staging
                    .as_ref()
                    .is_none_or(VideoStagingBuffers::ready) =>
            {
                if let Some(frame) = handle.take_frame() {
                    // Whole 4×4 blocks, as the texture was made.
                    let width = handle.width.div_ceil(4) * 4;
                    let height = handle.height.div_ceil(4) * 4;
                    staging.upload(&frame.color_data, texture, width, height, encoder);

                    if let (Some(alpha_data), Some(_alpha_fmt), Some(alpha_tex)) = (
                        frame.alpha_data.as_ref(),
                        frame.alpha_format,
                        alpha_texture.as_ref(),
                    ) {
                        if let Some(ref mut a_staging) = alpha_staging {
                            a_staging.upload(alpha_data, alpha_tex, width, height, encoder);
                            // Hap Q Alpha: its transparency from here on.
                            handle.is_dual_plane = true;
                        }
                    }
                    handle.recycle(frame);
                }
            }
            DeckSource::Playlist(list) => list.tick(encoder),
            DeckSource::Timer(timer) => timer.tick(),
            DeckSource::Presenter(presenter) => presenter.tick(),
            _ => {}
        }
        Ok(())
    }

    /// Request re-mapping of staging buffers after `queue.submit()`.
    pub fn request_video_remap(&mut self) {
        match &mut self.source {
            DeckSource::Video {
                ref mut staging, ..
            } => staging.request_remap(),
            DeckSource::HapVideo {
                ref mut staging,
                ref mut alpha_staging,
                ..
            } => {
                staging.request_remap();
                if let Some(ref mut a) = alpha_staging {
                    a.request_remap();
                }
            }
            DeckSource::Playlist(list) => list.for_each_deck_mut(Deck::request_video_remap),
            _ => {}
        }
    }

    /// Render the deck to its texture (source + effect chain)
    ///
    /// # Errors
    ///
    /// Propagates any error from [`Deck::render_with_prefix`].
    pub fn render(
        &mut self,
        context: &GpuContext,
        audio_data: &AudioData,
        modulation: &ModulationEngine,
        deck_idx: usize,
        cmd_buffers: &mut Vec<wgpu::CommandBuffer>,
    ) -> Result<()> {
        let prefix = format!("deck{deck_idx}");
        self.render_with_prefix(context, audio_data, modulation, &prefix, cmd_buffers, None)
    }

    /// Ensure analyzers are running for all preprocessor slots that need them.
    ///
    /// Called once at deck creation or when effects change. Automatically
    /// requests analyzer types declared in PREPROCESSORS blocks.
    pub(crate) fn ensure_preprocessor_analyzers(&mut self, registry: &AnalyzerRegistry) {
        // Collect all (analyzer_type, options) needed by preprocessor slots
        let mut needed: Vec<(String, serde_json::Value)> = Vec::new();
        if let DeckSource::Shader {
            preprocessor_textures,
            ..
        } = &self.source
        {
            for slot in preprocessor_textures {
                needed.push((slot.analyzer_type.clone(), slot.options.clone()));
            }
        }
        for effect in &self.effects {
            for slot in &effect.preprocessor_textures {
                needed.push((slot.analyzer_type.clone(), slot.options.clone()));
            }
        }

        // Deduplicate by analyzer_type and request each
        let mut seen = std::collections::HashSet::new();
        for (analyzer_type, options) in &needed {
            // GPU-inline preprocessors have no factory and no worker thread —
            // their passes are driven from the deck render path. Handing one to
            // `DeckAnalyzers` would log a spurious failure every load.
            // See /spec/effect-preprocessing.md § Preprocessor Categories.
            if registry
                .category_for(analyzer_type)
                .is_some_and(PreprocessorCategory::is_gpu)
            {
                continue;
            }
            if seen.insert(analyzer_type.clone())
                && self.analyzers.latest_snapshot(analyzer_type).is_none()
            {
                if self
                    .analyzers
                    .request(analyzer_type, registry, options)
                    .is_some()
                {
                    log::info!(
                        "Deck '{}': auto-started analyzer '{}'",
                        self.uuid,
                        analyzer_type
                    );
                } else {
                    log::warn!(
                        "Deck '{}': failed to start analyzer '{}'",
                        self.uuid,
                        analyzer_type
                    );
                }
            }
        }
    }

    /// Render the deck with a custom param prefix for modulation key lookup
    /// Render this deck, containing any GPU error it raises.
    ///
    /// wgpu reports validation errors through a device-wide handler rather than
    /// a `Result`, and the default handler panics — which on the render thread
    /// ends the show. A malformed shader is a user-authored input, so it must be
    /// survivable: the deck is quarantined, keeps displaying its last good
    /// frame, and everything else carries on rendering.
    /// See spec/error-handling.md § Shader Errors.
    ///
    /// # Errors
    ///
    /// Propagates errors raised while encoding the source and effect chain
    /// (for example a failed effect application). GPU validation errors are
    /// *not* propagated: they quarantine the deck and return `Ok(())`.
    pub fn render_with_prefix(
        &mut self,
        context: &GpuContext,
        audio_data: &AudioData,
        modulation: &ModulationEngine,
        param_prefix: &str,
        cmd_buffers: &mut Vec<wgpu::CommandBuffer>,
        gpu_timing: Option<(&wgpu::QuerySet, u32, u32)>,
    ) -> Result<()> {
        if self.gpu_error.is_some() {
            // Quarantined. The deck's texture still holds its last good frame,
            // which is the "freeze the last good frame" fallback the spec allows.
            return Ok(());
        }

        // A still image whose inputs have not changed already has this
        // frame's picture in its texture.
        let still = self.still_frame(modulation);
        if still.is_some() && still == self.rendered_still {
            return Ok(());
        }

        let before = cmd_buffers.len();
        let result = {
            let scope = context.errors.scope(&format!("deck {}", self.uuid));
            // A still image is drawn whole: it is kept until its settings
            // change, whatever comes to cover or uncover it meanwhile.
            let result = self.render_with_prefix_inner(
                context,
                audio_data,
                modulation,
                param_prefix,
                cmd_buffers,
                gpu_timing,
                still.is_none(),
            );
            match scope.faulted() {
                Some(message) => Err(message),
                None => Ok(result),
            }
        };

        match result {
            Ok(inner) => {
                // Remembered only once it rendered, so a failure retries.
                self.rendered_still = if inner.is_ok() { still } else { None };
                inner
            }
            Err(message) => {
                self.rendered_still = None;
                // Drop anything this deck encoded before failing: submitting a
                // partial frame from a deck we are about to quarantine risks
                // raising the same error again downstream.
                cmd_buffers.truncate(before);
                log::error!(
                    "Deck '{}' ({}) raised a GPU error and was disabled: {}",
                    self.uuid,
                    self.source_name,
                    message
                );
                self.gpu_error = Some(message);
                Ok(())
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_with_prefix_inner(
        &mut self,
        context: &GpuContext,
        audio_data: &AudioData,
        modulation: &ModulationEngine,
        param_prefix: &str,
        cmd_buffers: &mut Vec<wgpu::CommandBuffer>,
        gpu_timing: Option<(&wgpu::QuerySet, u32, u32)>,
        in_parts: bool,
    ) -> Result<()> {
        // Update preprocessor textures from analyzer snapshots before rendering
        if self.analyzers.has_active_instances() {
            Self::update_preprocessor_textures(
                &self.analyzers,
                &context.device,
                &context.queue,
                &mut self.source,
                &mut self.effects,
            );
        }

        // Write begin GPU timestamp if timing is enabled
        if let Some((query_set, begin_idx, _)) = gpu_timing {
            let mut enc = context
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("GPU Timing Begin"),
                });
            enc.write_timestamp(query_set, begin_idx);
            cmd_buffers.push(enc.finish());
        }

        // Advance render_time by a fixed dt so skipped frames don't cause
        // animation jumps. The shader sees smooth, consistent time steps
        // regardless of how many frames were skipped.
        let time_delta = self.render_dt;
        self.render_time += time_delta;
        let time = self.render_time;
        self.frame_count += 1;

        // Derive per-deck FPS from wall-clock render interval (for UI display only)
        let now = Instant::now();
        let wall_dt = (now - self.last_frame_time).as_secs_f32();
        self.last_frame_time = now;
        if wall_dt > 0.0 && wall_dt < 1.0 {
            let instant_fps = 1.0 / wall_dt;
            self.fps_smoothed = 0.1 * instant_fps + 0.9 * self.fps_smoothed;
        }

        // Accumulate generator phase times using the fixed dt
        accumulate_phase_times(
            &mut self.phase_accumulators,
            time_delta,
            self.generator_phase_inputs.as_deref(),
            &mut self.generator_params,
            modulation,
            param_prefix,
        );
        let generator_phase_times = self.phase_accumulators;

        let mut enabled_effects = self.drawn_effects(modulation);
        // A picture cropped, moved and scaled is drawn straight into its
        // place: the pass that draws it does those, not passes after it.
        let folded = self.fold(&enabled_effects, modulation);
        if let Some(fold) = &folded {
            for effect in enabled_effects.drain(..fold.effects) {
                self.effects[effect].runs += 1;
            }
        }

        let source_to_b = enabled_effects.len() % 2 == 1;

        // Each pass draws only what the frame shows of the deck, when its
        // channel has worked that out; see `draw_plan`.
        let plan = self.draw_plan(&enabled_effects, in_parts, modulation);
        self.drawn_visible = plan
            .iter()
            .any(Option::is_some)
            .then(|| self.visible.clone())
            .flatten();
        let source_within = plan[0].as_deref();
        self.drawn_share = source_within.map_or(1.0, |rects| {
            let all = Pixels::all(self.texture.width(), self.texture.height()).count();
            (rects.iter().map(Pixels::count).sum::<u64>() as f64 / all.max(1) as f64) as f32
        });

        // Depth-sensor decks render via a point-cloud pass, which needs mutable
        // access to `self.point_cloud_pipeline` (a field separate from
        // `self.source`). Handle it before the `&mut self.source` match to avoid
        // a split-borrow, then skip the ExternalSource blit arm below.
        let is_depth = matches!(
            self.external_source_kind(),
            Some(super::ExternalSourceKind::DepthSensor(_))
        );
        if is_depth {
            self.render_point_cloud(context, source_to_b, time, cmd_buffers);
        }

        // Depth-sensor preprocessor passes run before the shader so its bindings
        // hold this frame's fields. See spec/depth-sensor-preprocessor.md.
        self.run_depth_preprocess(context, cmd_buffers);

        let generator_target = if source_to_b {
            &self.texture_b_view
        } else {
            &self.texture_view
        };

        match &mut self.source {
            DeckSource::Shader {
                pipeline,
                pass_buffers,
                passes,
                imported_textures,
                preprocessor_textures,
                ..
            } => {
                let imported_views: Vec<&wgpu::TextureView> =
                    imported_textures.iter().map(|(_, _, v)| v).collect();
                let preprocessor_views: Vec<&wgpu::TextureView> =
                    preprocessor_textures.iter().map(|pp| &pp.view).collect();
                if pipeline.num_pass_buffers > 0 {
                    Self::render_multi_pass_static(
                        context,
                        pipeline,
                        passes,
                        pass_buffers,
                        time,
                        time_delta,
                        self.frame_count,
                        self.texture.width(),
                        self.texture.height(),
                        generator_target,
                        audio_data,
                        &mut self.generator_params,
                        modulation,
                        param_prefix,
                        &imported_views,
                        &preprocessor_views,
                        generator_phase_times,
                        cmd_buffers,
                    );
                } else {
                    Self::render_simple_static(
                        context,
                        pipeline,
                        &self.texture,
                        time,
                        time_delta,
                        self.frame_count,
                        generator_target,
                        audio_data,
                        &mut self.generator_params,
                        modulation,
                        param_prefix,
                        &imported_views,
                        &preprocessor_views,
                        generator_phase_times,
                        cmd_buffers,
                        source_within,
                    );
                }
            }

            DeckSource::Video {
                ref texture_view,
                ref blit_pipeline,
                source_width,
                source_height,
                scaling_mode,
                ..
            } => {
                let (uv_scale, uv_offset) = scaling_mode.compute_uv_transform(
                    *source_width,
                    *source_height,
                    // The master, not this deck's texture: at a reduced
                    // render scale, Center still means native size on air.
                    self.master_size.0,
                    self.master_size.1,
                );
                blit_pipeline.set_uv_transform(&context.queue, 1.0, uv_scale, uv_offset);

                let bind_group = blit_pipeline.create_bind_group(&context.device, texture_view);
                let mut encoder =
                    context
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("Video Blit Encoder"),
                        });
                {
                    let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("Video Blit Pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: generator_target,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: keep_or_clear(source_within, wgpu::Color::BLACK),
                                store: wgpu::StoreOp::Store,
                            },
                            depth_slice: None,
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    blit_pipeline.render_within(&mut render_pass, &bind_group, source_within);
                }
                cmd_buffers.push(encoder.finish());
            }
            DeckSource::HapVideo {
                ref texture_view,
                ref alpha_texture_view,
                ref dummy_alpha_view,
                ref convert_pipeline,
                ref hap_format,
                ref handle,
                source_width,
                source_height,
                scaling_mode,
                ..
            } => {
                let needs_ycocg = hap_format.needs_ycocg_convert();
                let has_alpha = handle.is_dual_plane && alpha_texture_view.is_some();
                let (uv_scale, uv_offset) = scaling_mode.compute_uv_transform(
                    *source_width,
                    *source_height,
                    // The master, not this deck's texture: at a reduced
                    // render scale, Center still means native size on air.
                    self.master_size.0,
                    self.master_size.1,
                );
                // The picture's part of its texture, made in whole 4×4 blocks.
                let extent = |size: u32| size as f32 / (size.div_ceil(4) * 4).max(1) as f32;
                convert_pipeline.set_params_with_uv(
                    &context.queue,
                    1.0,
                    needs_ycocg,
                    has_alpha,
                    uv_scale,
                    uv_offset,
                    [extent(*source_width), extent(*source_height)],
                );

                let alpha_view = if let Some(ref av) = alpha_texture_view {
                    av
                } else {
                    dummy_alpha_view
                };
                let bind_group =
                    convert_pipeline.create_bind_group(&context.device, texture_view, alpha_view);
                let mut encoder =
                    context
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("HAP Convert Encoder"),
                        });
                {
                    let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("HAP Convert Pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: generator_target,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: keep_or_clear(source_within, wgpu::Color::BLACK),
                                store: wgpu::StoreOp::Store,
                            },
                            depth_slice: None,
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    convert_pipeline.draw_within(&mut render_pass, &bind_group, source_within);
                }
                cmd_buffers.push(encoder.finish());
            }
            DeckSource::Image {
                texture_view,
                blit_pipeline,
                source_width,
                source_height,
                scaling_mode,
                ..
            } => {
                let (uv_scale, uv_offset) = scaling_mode.compute_uv_transform(
                    *source_width,
                    *source_height,
                    // The master, not this deck's texture: at a reduced
                    // render scale, Center still means native size on air.
                    self.master_size.0,
                    self.master_size.1,
                );
                blit_pipeline.set_uv_transform(&context.queue, 1.0, uv_scale, uv_offset);

                let bind_group = blit_pipeline.create_bind_group(&context.device, texture_view);
                let mut encoder =
                    context
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("Image Blit Encoder"),
                        });
                {
                    let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("Image Blit Pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: generator_target,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: keep_or_clear(source_within, wgpu::Color::BLACK),
                                store: wgpu::StoreOp::Store,
                            },
                            depth_slice: None,
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    blit_pipeline.render_within(&mut render_pass, &bind_group, source_within);
                }
                cmd_buffers.push(encoder.finish());
            }
            DeckSource::SolidColor { color } => {
                let mut encoder =
                    context
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("SolidColor Clear Encoder"),
                        });
                {
                    let _render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("SolidColor Clear Pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: generator_target,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color {
                                    r: color[0],
                                    g: color[1],
                                    b: color[2],
                                    a: color[3],
                                }),
                                store: wgpu::StoreOp::Store,
                            },
                            depth_slice: None,
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                }
                cmd_buffers.push(encoder.finish());
            }
            // Its input not opened: nothing to draw, so clear.
            DeckSource::Offline(_) => {
                let mut encoder =
                    context
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("Offline Clear Encoder"),
                        });
                {
                    let _render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("Offline Clear Pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: generator_target,
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
                }
                cmd_buffers.push(encoder.finish());
            }
            DeckSource::ExternalSource {
                kind,
                blit_pipeline,
                blit_pipeline_over_black,
                source_width,
                source_height,
                scaling_mode,
            } => {
                // DepthSensor decks were already reprojected above via the
                // point-cloud pass; the plain blit only applies to flat sources.
                if !matches!(kind, ExternalSourceKind::DepthSensor(_)) {
                    if let (Some(ext_view), Some(fold)) = (&self.external_source_view, &folded) {
                        let (uv_scale, uv_offset) = scaling_mode.compute_uv_transform(
                            *source_width,
                            *source_height,
                            self.master_size.0,
                            self.master_size.1,
                        );
                        let placed = crate::renderer::placed_picture::Placed {
                            uv_scale,
                            uv_offset,
                            pos: fold.pos,
                            scale: fold.scale,
                            flip: fold.flip,
                            crop: fold.crop,
                            premultiplied: matches!(kind, ExternalSourceKind::Tap),
                            over_black: fold.over_black,
                        };
                        let pipeline = self.placed.get_or_insert_with(|| {
                            crate::renderer::placed_picture::PlacedPicturePipeline::new(
                                &context.device,
                                context.compositing_format,
                            )
                        });
                        let frame = (self.texture.width(), self.texture.height());
                        let drawn = pipeline.draw(
                            &context.device,
                            &context.queue,
                            cmd_buffers,
                            &placed,
                            ext_view,
                            generator_target,
                            frame,
                            source_within,
                        );
                        let all = Pixels::all(frame.0, frame.1).count().max(1);
                        self.drawn_share = (drawn as f64 / all as f64) as f32;
                    } else if let Some(ext_view) = &self.external_source_view {
                        Self::blit_external_source(
                            context,
                            blit_pipeline,
                            blit_pipeline_over_black,
                            self.transparent,
                            ext_view,
                            *source_width,
                            *source_height,
                            // The master: Center means native size on air.
                            self.master_size.0,
                            self.master_size.1,
                            *scaling_mode,
                            generator_target,
                            kind.label(),
                            cmd_buffers,
                            source_within,
                            // Nida's own pictures, composited premultiplied.
                            matches!(kind, ExternalSourceKind::Tap),
                        );
                    }
                }
            }
            DeckSource::ComputeShader { pipeline, .. } => {
                self.generator_params.ensure_buffer(&context.device);
                self.generator_params.update_buffer_with_modulation(
                    &context.queue,
                    modulation,
                    Some(param_prefix),
                );

                let user_params_buffer = self
                    .generator_params
                    .buffer()
                    .expect("Buffer should exist after ensure_buffer");

                let (dispatch_x, dispatch_y, dispatch_z) =
                    pipeline.dispatch_counts(self.texture.width(), self.texture.height());

                let num_passes = pipeline.num_passes;

                // Multi-pass compute dispatch loop.
                // Each pass is submitted separately so the GPU completes it before
                // the next pass begins (implicit barrier between queue submits).
                for pass_idx in 0..num_passes {
                    let uniforms = ISFUniforms {
                        time,
                        time_delta,
                        frame_index: self.frame_count,
                        pass_index: i32::try_from(pass_idx).unwrap_or(i32::MAX),
                        render_size: [self.texture.width() as f32, self.texture.height() as f32],
                        audio_level: audio_data.level,
                        audio_bass: audio_data.bass(),
                        audio_mid: audio_data.mid(),
                        audio_treble: audio_data.treble(),
                        audio_bpm: audio_data.bpm.unwrap_or(0.0),
                        audio_beat_phase: audio_data.beat_phase(),
                        date: get_current_date(),
                        phase_times: generator_phase_times,
                    };
                    pipeline.update_uniforms(&context.queue, &uniforms);

                    let bind_group =
                        pipeline.create_bind_group(&context.device, Some(user_params_buffer));

                    let mut encoder =
                        context
                            .device
                            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some("Compute Shader Dispatch Encoder"),
                            });

                    // Clear non-persistent storage buffers before the first pass
                    if pass_idx == 0 {
                        pipeline.clear_non_persistent_buffers(&mut encoder);
                    }

                    {
                        let mut compute_pass =
                            encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                                label: Some("Compute Shader Pass"),
                                timestamp_writes: None,
                            });
                        compute_pass.set_pipeline(&pipeline.compute_pipeline);
                        compute_pass.set_bind_group(0, &bind_group, &[]);
                        compute_pass.dispatch_workgroups(dispatch_x, dispatch_y, dispatch_z);
                    }

                    // Submit each pass individually for implicit GPU synchronization
                    context.submit(std::iter::once(encoder.finish()));
                }

                // Copy final compute output to the generator target texture
                let dest_texture = if source_to_b {
                    &self.texture_b
                } else {
                    &self.texture
                };
                let mut copy_encoder =
                    context
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("Compute Output Copy Encoder"),
                        });
                copy_encoder.copy_texture_to_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &pipeline.output_texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyTextureInfo {
                        texture: dest_texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::Extent3d {
                        width: self.texture.width(),
                        height: self.texture.height(),
                        depth_or_array_layers: 1,
                    },
                );

                cmd_buffers.push(copy_encoder.finish());
            }
            DeckSource::Playlist(list) => {
                list.render(
                    audio_data,
                    modulation,
                    param_prefix,
                    cmd_buffers,
                    generator_target,
                );
            }
            DeckSource::Timer(timer) => timer.render(cmd_buffers, generator_target),
            DeckSource::Presenter(presenter) => presenter.render(
                cmd_buffers,
                generator_target,
                (self.texture.width(), self.texture.height()),
            ),
        }

        // Apply effect chain (ping-pong between textures)
        let mut read_from_b = source_to_b;
        for (run, &effect_idx) in enabled_effects.iter().enumerate() {
            // Accumulate phase times for this effect
            let effect = &mut self.effects[effect_idx];
            effect.runs += 1;
            let Effect {
                phase_accumulators,
                phase_inputs_config,
                params,
                param_prefix,
                ..
            } = effect;
            accumulate_phase_times(
                phase_accumulators,
                time_delta,
                phase_inputs_config.as_deref(),
                params,
                modulation,
                param_prefix,
            );
            let effect_phase_times = effect.phase_accumulators;

            let uniforms = ISFUniforms {
                time,
                time_delta,
                frame_index: self.frame_count,
                pass_index: 0,
                render_size: [self.texture.width() as f32, self.texture.height() as f32],
                audio_level: audio_data.level,
                audio_bass: audio_data.bass(),
                audio_mid: audio_data.mid(),
                audio_treble: audio_data.treble(),
                audio_bpm: audio_data.bpm.unwrap_or(0.0),
                audio_beat_phase: audio_data.beat_phase(),
                date: get_current_date(),
                phase_times: effect_phase_times,
            };
            let (input_view, output_view) = if read_from_b {
                (&self.texture_b_view, &self.texture_view)
            } else {
                (&self.texture_view, &self.texture_b_view)
            };
            self.effects[effect_idx].apply_within(
                context,
                input_view,
                output_view,
                &uniforms,
                Some(modulation),
                cmd_buffers,
                plan[run + 1].as_deref(),
            )?;
            read_from_b = !read_from_b;
        }

        // Capture frame for analyzer pipeline (non-blocking, one-frame latency)
        if let Some(readback_cmd) = self.analyzers.capture_frame(&context.device, &self.texture) {
            cmd_buffers.push(readback_cmd);
        }

        // Write end GPU timestamp if timing is enabled
        if let Some((query_set, _, end_idx)) = gpu_timing {
            let mut enc = context
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("GPU Timing End"),
                });
            enc.write_timestamp(query_set, end_idx);
            cmd_buffers.push(enc.finish());
        }

        Ok(())
    }

    /// The scissor rectangles each pass draws through this frame — the
    /// source first, then each effect run — so that between them they get
    /// right the parts of the frame that show of the deck (see
    /// [`Deck::set_visible`]); `None` for a pass that draws whole.
    ///
    /// Worked back from the last pass: a pointwise effect needs its input in
    /// the same parts, a transform in the parts it moves them from, and any
    /// other effect all of it, which leaves every pass before it whole. The
    /// source draws in parts only when its shader or blit reads nothing of
    /// its own from before — not a multi-pass or compute shader, a depth
    /// sensor or a plain colour. Nothing is limited while analyzers read the
    /// texture, or when `in_parts` is off.
    fn draw_plan(
        &self,
        enabled_effects: &[usize],
        in_parts: bool,
        modulation: &ModulationEngine,
    ) -> Vec<Option<Vec<Pixels>>> {
        let mut plan = vec![None; enabled_effects.len() + 1];
        let Some(visible) = self.visible.as_ref().filter(|_| in_parts) else {
            return plan;
        };
        if self.analyzers.has_active_instances() {
            return plan;
        }
        let (width, height) = (self.texture.width(), self.texture.height());
        let aspect = self.master_size.0 as f32 / self.master_size.1.max(1) as f32;
        let mut needed: Vec<Area> = visible.clone();
        for stage in (0..plan.len()).rev() {
            plan[stage] = scissors_for(&needed, width, height, PART_MARGIN);
            let Some(&effect) = stage.checked_sub(1).and_then(|i| enabled_effects.get(i)) else {
                break;
            };
            if let Some(input) = self.effects[effect].input_needed(&needed, aspect, modulation) {
                needed = input;
            } else {
                // Everything before it draws whole, as it already stands.
                break;
            }
        }
        let source_in_parts = match &self.source {
            DeckSource::Shader { pipeline, .. } => pipeline.num_pass_buffers == 0,
            DeckSource::Video { .. } | DeckSource::HapVideo { .. } | DeckSource::Image { .. } => {
                true
            }
            DeckSource::ExternalSource { kind, .. } => {
                !matches!(kind, ExternalSourceKind::DepthSensor(_))
            }
            // Mixed from the items' own pictures, whole.
            DeckSource::SolidColor { .. }
            | DeckSource::ComputeShader { .. }
            | DeckSource::Playlist(_)
            | DeckSource::Timer(_)
            | DeckSource::Presenter(_)
            | DeckSource::Offline(_) => false,
        };
        if !source_in_parts {
            plan[0] = None;
        }
        plan
    }

    /// What the pass that draws the source can do of the effects after it,
    /// when the source is a plain picture — another input, a camera, a
    /// screen or a stream: a crop (with its border and rounded corners) and
    /// a move and scale, in that order, first in the chain. The picture is
    /// then drawn straight into its place (see
    /// [`crate::renderer::placed_picture`]) rather than whole, then cropped
    /// whole, then shrunk. A picture-in-picture a third across draws a
    /// ninth of the frame, once, instead of all of it three times.
    ///
    /// A source the deck flattens over black is flattened there too, as the
    /// blit does it — black bars and see-through parts black — so a layer
    /// draws the same whether it keeps its source's transparency or not,
    /// and a Merge's picture of it (see `framing`) ends where the layer is.
    fn fold(&self, enabled_effects: &[usize], modulation: &ModulationEngine) -> Option<Fold> {
        let over_black = self.source_draws().ok()?;
        self.external_source_view.as_ref()?;
        let fold = self.foldable(enabled_effects, modulation)?;
        Some(Fold { over_black, ..fold })
    }

    /// The effects a deck draws, by index. Crop and transform at their
    /// neutral settings leave every pixel as it was (`render_correctness`
    /// proves it), so they are not run: every new deck carries both, and on
    /// anything that moves — video, generators, cameras — each was a
    /// full-frame pass per frame, ~130 MB at 4K.
    fn drawn_effects(&self, modulation: &ModulationEngine) -> Vec<usize> {
        // An offline input draws nothing at all: an effect that fills the
        // frame — feedback trails, a colour wash — would cover the layers
        // under it with its clear picture.
        if matches!(self.source, DeckSource::Offline(_)) {
            return Vec::new();
        }
        self.effects
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                e.enabled
                    && e.offline().is_none()
                    && !e.is_neutral_framing(modulation)
                    && !e.is_idle_layer_mask()
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// The crop, then move and scale, first among `enabled_effects` that the
    /// pass drawing the source does too (see [`Self::fold`]).
    fn foldable(&self, enabled_effects: &[usize], modulation: &ModulationEngine) -> Option<Fold> {
        let crop = enabled_effects
            .first()
            .and_then(|&effect| self.effects[effect].plain_crop(modulation));
        let mut effects = usize::from(crop.is_some());
        let placement = enabled_effects.get(effects).and_then(|&effect| {
            let effect = &self.effects[effect];
            effect
                .plain_placement(modulation)
                .map(|(pos, scale)| (pos, scale, effect.flips()))
        });
        effects += usize::from(placement.is_some());
        let (pos, scale, flip) = placement.unwrap_or(([0.0; 2], [1.0; 2], [false; 2]));
        (effects > 0).then_some(Fold {
            crop,
            pos,
            scale,
            flip,
            over_black: 0.0,
            effects,
        })
    }

    /// How the pass drawing a picture into its place (see [`Self::fold`])
    /// draws this deck's live source as the deck shows it: as it comes (0) —
    /// its transparency kept, or a picture with none filling the frame — or
    /// over black (1), as the deck flattens one with see-through parts or
    /// not filling the frame. Else why it cannot: the source not flat, or
    /// not live.
    fn source_draws(&self) -> Result<f32, &'static str> {
        let DeckSource::ExternalSource {
            kind,
            source_width,
            source_height,
            scaling_mode,
            ..
        } = &self.source
        else {
            return Err("not a live input — a picture, clip or generator moves only when drawn alike in both");
        };
        if self.analyzers.has_active_instances() {
            return Err("analysed for modulation");
        }
        let opaque = matches!(
            kind,
            ExternalSourceKind::Camera(_)
                | ExternalSourceKind::ScreenCapture(_)
                | ExternalSourceKind::Srt(_)
                | ExternalSourceKind::Hls(_)
                | ExternalSourceKind::Dash(_)
                | ExternalSourceKind::Rtmp(_)
        ) && super::fills_the_frame(
            *scaling_mode,
            (*source_width, *source_height),
            self.master_size,
        );
        if matches!(kind, ExternalSourceKind::DepthSensor(_)) {
            return Err("a depth sensor's picture, drawn in 3D");
        }
        Ok(if self.transparent || opaque { 0.0 } else { 1.0 })
    }

    /// This deck's whole picture as one placed draw of its live source — the
    /// source fitted, then cropped, bordered, moved and scaled — where that
    /// is all it is: no other effect, nothing modulated, a source drawn as the
    /// deck shows it — flattened over black where the deck flattens it. What
    /// a Merge draws a pair showing one source from, at every framing
    /// between theirs (see `mixer::main_transition`) — known before the
    /// source is bound, as when a take starts on a deck just added. Else why
    /// not, for the operator.
    pub(crate) fn framing(&self, modulation: &ModulationEngine) -> Result<Framing<'_>, &'static str> {
        let over_black = self.source_draws()?;
        let DeckSource::ExternalSource {
            kind,
            source_width,
            source_height,
            scaling_mode,
            ..
        } = &self.source
        else {
            return Err("not a live input");
        };
        let drawn = self.drawn_effects(modulation);
        let others = "effects other than its crop and Transform, or a crop or Transform modulated, \
                      masked, inverted, turned or wrapped";
        let (crop, pos, scale, flip) = if drawn.is_empty() {
            (None, [0.0; 2], [1.0; 2], [false; 2])
        } else {
            let fold = self.foldable(&drawn, modulation).ok_or(others)?;
            if fold.effects != drawn.len() {
                return Err(others);
            }
            (fold.crop, fold.pos, fold.scale, fold.flip)
        };
        // As the deck fits it: the master's size, where Center is native.
        let (uv_scale, uv_offset) = scaling_mode.compute_uv_transform(
            *source_width,
            *source_height,
            self.master_size.0,
            self.master_size.1,
        );
        Ok(Framing {
            placed: crate::renderer::placed_picture::Placed {
                uv_scale,
                uv_offset,
                pos,
                scale,
                flip,
                crop,
                premultiplied: matches!(kind, ExternalSourceKind::Tap),
                over_black,
            },
            source: self.external_source_view.as_ref(),
            size: (self.texture.width(), self.texture.height()),
        })
    }

    /// Upload analyzer texture data into preprocessor slots.
    ///
    /// For each preprocessor slot (on source and effects), looks up the matching
    /// analyzer snapshot and uploads texture data via `queue.write_texture()`.
    /// If the texture dimensions changed, recreates the GPU texture.
    fn update_preprocessor_textures(
        analyzers: &DeckAnalyzers,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: &mut DeckSource,
        effects: &mut [super::Effect],
    ) {
        if let DeckSource::Shader {
            preprocessor_textures,
            ..
        } = source
        {
            for slot in preprocessor_textures.iter_mut() {
                if let Some(snapshot) = analyzers.latest_snapshot(&slot.analyzer_type) {
                    if let Some(tex_data) = snapshot.textures.get(&slot.name) {
                        upload_texture_to_slot(device, queue, slot, tex_data);
                    }
                }
            }
        }

        for effect in effects.iter_mut() {
            for slot in &mut effect.preprocessor_textures {
                if let Some(snapshot) = analyzers.latest_snapshot(&slot.analyzer_type) {
                    if let Some(tex_data) = snapshot.textures.get(&slot.name) {
                        upload_texture_to_slot(device, queue, slot, tex_data);
                    }
                }
            }
        }
    }

    /// Render simple (non-multi-pass) shader (static version)
    // Hot-path render helper; args are distinct GPU inputs with no shared invariant.
    #[allow(clippy::too_many_arguments)]
    fn render_simple_static(
        context: &GpuContext,
        pipeline: &UnifiedPipeline,
        texture: &wgpu::Texture,
        time: f32,
        time_delta: f32,
        frame_count: u32,
        target_view: &wgpu::TextureView,
        audio_data: &AudioData,
        generator_params: &mut ShaderParams,
        modulation: &ModulationEngine,
        param_prefix: &str,
        imported_views: &[&wgpu::TextureView],
        preprocessor_views: &[&wgpu::TextureView],
        phase_times: [f32; 4],
        cmd_buffers: &mut Vec<wgpu::CommandBuffer>,
        within: Option<&[Pixels]>,
    ) {
        let uniforms = ISFUniforms {
            time,
            time_delta,
            frame_index: frame_count,
            pass_index: 0,
            render_size: [texture.width() as f32, texture.height() as f32],
            audio_level: audio_data.level,
            audio_bass: audio_data.bass(),
            audio_mid: audio_data.mid(),
            audio_treble: audio_data.treble(),
            audio_bpm: audio_data.bpm.unwrap_or(0.0),
            audio_beat_phase: audio_data.beat_phase(),
            date: get_current_date(),
            phase_times,
        };

        pipeline.update_uniforms(&context.queue, &uniforms);

        generator_params.ensure_buffer(&context.device);
        generator_params.update_buffer_with_modulation(
            &context.queue,
            modulation,
            Some(param_prefix),
        );

        let user_params_buffer = generator_params
            .buffer()
            .expect("Buffer should exist after ensure_buffer");
        let bind_group = pipeline.create_bind_group(
            &context.device,
            None,
            &[],
            imported_views,
            preprocessor_views,
            Some(user_params_buffer),
        );

        let mut encoder = context
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Deck Source Render Encoder"),
            });

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Deck Source Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: keep_or_clear(within, wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            render_pass.set_pipeline(&pipeline.pipeline);
            render_pass.set_bind_group(0, &bind_group, &[]);
            draw_within(&mut render_pass, within);
        }

        cmd_buffers.push(encoder.finish());
    }

    /// Render multi-pass shader with proper ping-pong buffers
    // Hot-path render helper; args are distinct GPU inputs with no shared invariant.
    #[allow(clippy::too_many_arguments)]
    fn render_multi_pass_static(
        context: &GpuContext,
        multi_pass: &UnifiedPipeline,
        passes: &[ISFPass],
        pass_buffers: &mut HashMap<String, PassBuffer>,
        time: f32,
        time_delta: f32,
        frame_count: u32,
        render_width: u32,
        render_height: u32,
        final_target: &wgpu::TextureView,
        audio_data: &AudioData,
        generator_params: &mut ShaderParams,
        modulation: &ModulationEngine,
        param_prefix: &str,
        imported_views: &[&wgpu::TextureView],
        preprocessor_views: &[&wgpu::TextureView],
        phase_times: [f32; 4],
        cmd_buffers: &mut Vec<wgpu::CommandBuffer>,
    ) {
        const SIMULATION_ITERATIONS: usize = 4;

        generator_params.ensure_buffer(&context.device);
        generator_params.update_buffer_with_modulation(
            &context.queue,
            modulation,
            Some(param_prefix),
        );
        let user_params_buffer = generator_params
            .buffer()
            .expect("Buffer should exist after ensure_buffer");

        for pass_idx in 0..passes.len() {
            let pass = &passes[pass_idx];

            let iterations = if pass.persistent.unwrap_or(false) {
                SIMULATION_ITERATIONS
            } else {
                1
            };

            let Some(target_name) = &pass.target else {
                continue;
            };

            // Use the pass buffer's actual dimensions as RENDERSIZE so
            // shaders that store per-pixel state (e.g. particle buffers)
            // can address their own texels correctly.
            let pass_render_size = pass_buffers.get(target_name).map_or(
                [render_width as f32, render_height as f32],
                |pb| {
                    let sz = pb.texture_a.size();
                    [sz.width as f32, sz.height as f32]
                },
            );

            for iter in 0..iterations {
                let effective_frame = frame_count * SIMULATION_ITERATIONS as u32 + iter as u32;

                let uniforms = ISFUniforms {
                    time,
                    time_delta: time_delta / SIMULATION_ITERATIONS as f32,
                    frame_index: effective_frame,
                    pass_index: i32::try_from(pass_idx).unwrap_or(i32::MAX),
                    render_size: pass_render_size,
                    audio_level: audio_data.level,
                    audio_bass: audio_data.bass(),
                    audio_mid: audio_data.mid(),
                    audio_treble: audio_data.treble(),
                    audio_bpm: audio_data.bpm.unwrap_or(0.0),
                    audio_beat_phase: audio_data.beat_phase(),
                    date: get_current_date(),
                    phase_times,
                };

                multi_pass.update_uniforms(&context.queue, &uniforms);

                let pass_buffer_views: Vec<&wgpu::TextureView> = passes
                    .iter()
                    .filter_map(|p| p.target.as_ref().and_then(|t| pass_buffers.get(t)))
                    .map(super::PassBuffer::read_view)
                    .collect();

                let bind_group = multi_pass.create_bind_group(
                    &context.device,
                    None,
                    &pass_buffer_views,
                    imported_views,
                    preprocessor_views,
                    Some(user_params_buffer),
                );

                let target_view = pass_buffers
                    .get(target_name)
                    .map_or(final_target, super::PassBuffer::write_view);

                let mut encoder =
                    context
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("Sim Pass Encoder"),
                        });

                {
                    let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("Sim Pass Render"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: target_view,
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

                    render_pass.set_pipeline(&multi_pass.pipeline);
                    render_pass.set_bind_group(0, &bind_group, &[]);
                    render_pass.draw(0..3, 0..1);
                }

                // Multipass intermediate passes MUST submit immediately —
                // update_uniforms() overwrites the same buffer each iteration,
                // so batching would cause all passes to see the last pass's data.
                context.submit(std::iter::once(encoder.finish()));

                if let Some(pb) = pass_buffers.get_mut(target_name) {
                    pb.swap();
                }
            }
        }

        // Final render pass to screen
        {
            let uniforms = ISFUniforms {
                time,
                time_delta,
                frame_index: frame_count,
                pass_index: i32::try_from(passes.len()).unwrap_or(i32::MAX),
                render_size: [render_width as f32, render_height as f32],
                audio_level: audio_data.level,
                audio_bass: audio_data.bass(),
                audio_mid: audio_data.mid(),
                audio_treble: audio_data.treble(),
                audio_bpm: audio_data.bpm.unwrap_or(0.0),
                audio_beat_phase: audio_data.beat_phase(),
                date: get_current_date(),
                phase_times,
            };

            multi_pass.update_uniforms(&context.queue, &uniforms);

            let pass_buffer_views: Vec<&wgpu::TextureView> = passes
                .iter()
                .filter_map(|p| p.target.as_ref().and_then(|t| pass_buffers.get(t)))
                .map(super::PassBuffer::read_view)
                .collect();

            let bind_group = multi_pass.create_bind_group(
                &context.device,
                None,
                &pass_buffer_views,
                imported_views,
                preprocessor_views,
                Some(user_params_buffer),
            );

            let mut encoder =
                context
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("Final Pass Encoder"),
                    });

            {
                let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Final Pass Render"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: final_target,
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

                render_pass.set_pipeline(&multi_pass.pipeline);
                render_pass.set_bind_group(0, &bind_group, &[]);
                render_pass.draw(0..3, 0..1);
            }

            cmd_buffers.push(encoder.finish());
        }
    }

    /// Reproject a depth-sensor deck's point cloud into `target`.
    ///
    /// Needs `self.external_source_view` (`R16Uint` depth), `self.depth_rgb_view`,
    /// `self.depth_intrinsics`, and `self.depth_source_size` — all set once per
    /// frame by the app render loop. Lazily builds the pipeline on first use.
    /// No-op until a depth frame + intrinsics are available.
    fn render_point_cloud(
        &mut self,
        context: &GpuContext,
        source_to_b: bool,
        time: f32,
        cmd_buffers: &mut Vec<wgpu::CommandBuffer>,
    ) {
        let (Some(intr), Some((sw, sh))) = (self.depth_intrinsics, self.depth_source_size) else {
            return;
        };
        if self.external_source_view.is_none() || self.depth_rgb_view.is_none() {
            return;
        }

        if self.point_cloud_pipeline.is_none() {
            self.point_cloud_pipeline = Some(crate::depth::point_cloud::PointCloudPipeline::new(
                &context.device,
                self.texture.format(),
            ));
        }
        // Take the pipeline out so we can borrow other `self` fields freely.
        let pipeline = self.point_cloud_pipeline.take().unwrap();
        let target = if source_to_b {
            &self.texture_b_view
        } else {
            &self.texture_view
        };
        let depth_view = self.external_source_view.as_ref().unwrap();
        let rgb_view = self.depth_rgb_view.as_ref().unwrap();

        pipeline.update_uniform(
            &context.queue,
            intr,
            sw,
            sh,
            self.texture.width(),
            self.texture.height(),
            time,
            &self.point_cloud_params,
        );
        pipeline.render(
            &context.device,
            depth_view,
            rgb_view,
            target,
            sw * sh,
            cmd_buffers,
        );
        self.point_cloud_pipeline = Some(pipeline);
    }

    /// Run the depth-sensor preprocessor's conversion passes for this frame.
    ///
    /// No-op unless a sensor frame has actually arrived since the last run: the
    /// sensor is ~30 Hz and the deck is typically 60, so gating on the manager's
    /// upload counter halves this work. Also a no-op while the sensor reports
    /// disconnected, which freezes the outputs at their last good values rather
    /// than tearing down a live deck. See spec/depth-sensor-preprocessor.md.
    fn run_depth_preprocess(
        &mut self,
        context: &GpuContext,
        cmd_buffers: &mut Vec<wgpu::CommandBuffer>,
    ) {
        let Some(state) = &mut self.depth_prepro else {
            return;
        };
        let Some(input) = &state.input else {
            return;
        };

        if !input.connected {
            if !state.warned_disconnected {
                log::warn!(
                    "Deck '{}': depth sensor {} disconnected — preprocessor outputs frozen",
                    self.uuid,
                    state.sensor_id
                );
                state.warned_disconnected = true;
            }
            return;
        }
        if state.warned_disconnected {
            log::info!(
                "Deck '{}': depth sensor {} reconnected",
                self.uuid,
                state.sensor_id
            );
            state.warned_disconnected = false;
        }

        if state.last_generation == Some(input.generation) {
            return;
        }
        state.last_generation = Some(input.generation);

        state
            .pipeline
            .update_uniform(&context.queue, &state.params, input.frame_dt);
        let rgb = state.wants_rgb.then_some(&input.rgb_view);
        // Split the borrow: `run` needs `&mut pipeline` while `input` is behind
        // the same `&mut state`, so clone the cheap view handles first.
        let depth_view = input.depth_view.clone();
        let rgb_view = rgb.cloned();
        state
            .pipeline
            .run(&context.device, &depth_view, rgb_view.as_ref(), cmd_buffers);
    }

    /// Blit an external source (Camera, NDI, Syphon) with scaling to the generator target.
    // Hot-path blit helper; args are distinct GPU inputs with no shared invariant.
    #[allow(clippy::too_many_arguments)]
    fn blit_external_source(
        context: &GpuContext,
        blit_pipeline: &BlitPipeline,
        blit_pipeline_over_black: &BlitPipeline,
        transparent: bool,
        source_view: &wgpu::TextureView,
        source_width: u32,
        source_height: u32,
        target_width: u32,
        target_height: u32,
        scaling_mode: ScalingMode,
        generator_target: &wgpu::TextureView,
        label: &str,
        cmd_buffers: &mut Vec<wgpu::CommandBuffer>,
        within: Option<&[Pixels]>,
        premultiplied: bool,
    ) {
        // Flagged transparent → REPLACE over a transparent clear, preserving the
        // source's straight alpha (and leaving letterbox transparent). Default →
        // ALPHA_BLENDING over an opaque black clear, flattening to opaque so an
        // HTML source with alpha<1 doesn't punch holes (/spec/html-source.md §2).
        let (pipeline, load) = if transparent {
            (
                blit_pipeline,
                keep_or_clear(within, wgpu::Color::TRANSPARENT),
            )
        } else {
            // Blended over black, so black is cleared under the part drawn
            // too: drawn over last frame's picture it would come out wrong.
            (
                blit_pipeline_over_black,
                wgpu::LoadOp::Clear(wgpu::Color::BLACK),
            )
        };

        let (uv_scale, uv_offset) = scaling_mode.compute_uv_transform(
            source_width,
            source_height,
            target_width,
            target_height,
        );
        pipeline.set_uv_transform_of(&context.queue, 1.0, uv_scale, uv_offset, premultiplied);

        let bind_group = pipeline.create_bind_group(&context.device, source_view);
        let mut encoder = context
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some(&format!("{label} Blit Encoder")),
            });
        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(&format!("{label} Blit Pass")),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: generator_target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load,
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pipeline.render_within(&mut render_pass, &bind_group, within);
        }
        cmd_buffers.push(encoder.finish());
    }

    /// Resize the deck's render targets
    /// Resize for a master of `width × height`; the deck's own textures come
    /// out at its render scale of that.
    pub fn resize(&mut self, context: &GpuContext, width: u32, height: u32) {
        self.master_size = (width, height);
        // A playlist's items are drawn at the master's size.
        match &mut self.source {
            DeckSource::Playlist(list) => list.resize(width, height),
            // Its numbers are sized and placed against the master.
            DeckSource::Timer(timer) => timer.resize(width, height),
            // So is a presenter view's panel.
            DeckSource::Presenter(presenter) => presenter.resize(width, height),
            // A tap shows a channel's picture or the master's, the master's
            // size now: left at the old one, it would fit the new picture as
            // if it were that size — bars round it, or cut off.
            DeckSource::ExternalSource {
                kind: ExternalSourceKind::Tap,
                source_width,
                source_height,
                ..
            } => {
                *source_width = width;
                *source_height = height;
            }
            _ => {}
        }
        let (width, height) = self.render_scale.apply(width.max(1), height.max(1));
        // Fresh textures are empty, whatever their size: a still image must
        // render into them again.
        self.rendered_still = None;
        self.texture_generation = self.texture_generation.wrapping_add(1);
        // Include COPY_DST so compute shader decks survive resize
        let usage = wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST;
        self.texture = context.device.create_texture(&wgpu::TextureDescriptor {
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
            usage,
            view_formats: &[],
        });
        self.texture_view = self
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.texture_b = context.device.create_texture(&wgpu::TextureDescriptor {
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
            usage,
            view_formats: &[],
        });
        self.texture_b_view = self
            .texture_b
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.rerasterize_svg(context, width, height);
    }

    /// Re-render an SVG image source for a `width × height` deck.
    ///
    /// This is what makes vector art resolution-independent in practice: the
    /// same file that was rasterized for a 720p stage is redrawn at 4K when the
    /// master resolution goes up, instead of the blit magnifying the pixels it
    /// was first rendered at. Raster images and every other source are skipped.
    /// A failed re-render keeps the existing texture, so the deck goes soft
    /// rather than black.
    fn rerasterize_svg(&mut self, context: &GpuContext, width: u32, height: u32) {
        // The artwork texture is about to be replaced.
        self.rendered_still = None;
        let DeckSource::Image {
            texture,
            texture_view,
            source_width,
            source_height,
            svg: Some(tree),
            ..
        } = &mut self.source
        else {
            return;
        };
        let (raster_w, raster_h) = crate::deck::svg::raster_size(tree, width, height);
        if (raster_w, raster_h) == (*source_width, *source_height) {
            return;
        }
        let rgba = match crate::deck::svg::rasterize(tree, width, height) {
            Ok(rgba) => rgba,
            Err(e) => {
                log::warn!(
                    "Could not re-rasterize SVG deck '{}': {e}",
                    self.source_name
                );
                return;
            }
        };
        let (new_texture, new_view) = super::source::upload_image_texture(context, &rgba);
        *texture = new_texture;
        *texture_view = new_view;
        *source_width = raster_w;
        *source_height = raster_h;
    }

    /// Get the final output texture view (after effect chain)
    pub fn output_view(&self) -> &wgpu::TextureView {
        &self.texture_view
    }
}

/// Get current date as [year, month, day, `seconds_in_day`]
pub fn get_current_date() -> [f32; 4] {
    use std::time::SystemTime;

    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();

    let total_seconds = now.as_secs();
    let seconds_in_day = (total_seconds % 86400) as f32;

    let days_since_epoch = total_seconds / 86400;
    let year = 1970.0 + (days_since_epoch as f32 / 365.25);
    let day_of_year = (days_since_epoch % 365) as f32;
    let month = (day_of_year / 30.0).floor() + 1.0;
    let day = (day_of_year % 30.0) + 1.0;

    [year, month, day, seconds_in_day]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::isf::ISFInput;
    use crate::isf::PhaseInput;
    use crate::modulation::{ModulationSource, StepInterpolation};
    use crate::params::ShaderParams;

    /// Empty engine — every parameter reads back its base value.
    fn no_modulation() -> ModulationEngine {
        ModulationEngine::new()
    }

    /// A unipolar step sequencer at 0 Hz holds a constant +1.0, so modulation
    /// depth is exactly `amount` regardless of when the engine is ticked.
    ///
    /// Unipolar matters: bipolar sources carry a range-scale weight of 0.5, so a
    /// bipolar stand-in would halve the depth under test.
    fn constant_modulation(target: &str, amount: f32) -> ModulationEngine {
        let mut engine = ModulationEngine::new();
        let uuid = engine.add_source(ModulationSource::StepSequencer {
            steps: vec![1.0; 2],
            rate: 0.0,
            interpolation: StepInterpolation::None,
            bipolar: false,
        });
        engine.assign(target, &uuid, amount, None);
        engine.update_free_running(
            0.0,
            &crate::modulation::AudioValues::default(),
            &crate::modulation::AnalyzerValues::default(),
        );
        engine
    }

    fn phase(param: &str, index: usize, scale: f32) -> PhaseInput {
        PhaseInput {
            param: param.into(),
            index,
            scale,
            multiply_by: Vec::new(),
        }
    }

    fn phase_product(param: &str, multiply_by: &[&str], index: usize, scale: f32) -> PhaseInput {
        PhaseInput {
            multiply_by: multiply_by.iter().map(|s| (*s).to_string()).collect(),
            ..phase(param, index, scale)
        }
    }

    fn float_input(name: &str, default: f64, min: f32, max: f32) -> ISFInput {
        ISFInput {
            name: name.into(),
            input_type: "float".into(),
            default: Some(serde_json::json!(default)),
            min: Some(min),
            max: Some(max),
            label: None,
            values: None,
            labels: None,
            identity: None,
        }
    }

    #[test]
    fn isf_uniforms_size_is_80_bytes() {
        assert_eq!(
            std::mem::size_of::<ISFUniforms>(),
            80,
            "ISFUniforms should be 80 bytes (64 original + 16 for phase_times)"
        );
    }

    #[test]
    fn accumulate_phase_times_basic() {
        let mut accum = [0.0f32; 4];
        let inputs = vec![phase("speed", 0, 1.0)];
        let isf_inputs = vec![float_input("speed", 2.0, 0.0, 5.0)];
        let mut params = ShaderParams::from_inputs(&isf_inputs);
        let modulation = no_modulation();

        // dt=0.1, speed=2.0, scale=1.0 → accumulate 0.2
        accumulate_phase_times(
            &mut accum,
            0.1,
            Some(&inputs),
            &mut params,
            &modulation,
            "deck0",
        );
        assert!((accum[0] - 0.2).abs() < 1e-5);
        assert_eq!(accum[1], 0.0);

        // Accumulate again: 0.2 + 0.2 = 0.4
        accumulate_phase_times(
            &mut accum,
            0.1,
            Some(&inputs),
            &mut params,
            &modulation,
            "deck0",
        );
        assert!((accum[0] - 0.4).abs() < 1e-5);
    }

    #[test]
    fn accumulate_phase_times_with_scale() {
        let mut accum = [0.0f32; 4];
        let inputs = vec![phase("speed", 0, 0.3)];
        let isf_inputs = vec![float_input("speed", 1.0, 0.0, 5.0)];
        let mut params = ShaderParams::from_inputs(&isf_inputs);

        // dt=0.5, speed=1.0, scale=0.3 → 0.15
        accumulate_phase_times(
            &mut accum,
            0.5,
            Some(&inputs),
            &mut params,
            &no_modulation(),
            "deck0",
        );
        assert!((accum[0] - 0.15).abs() < 1e-5);
    }

    #[test]
    fn accumulate_phase_times_speed_change_is_continuous() {
        let mut accum = [0.0f32; 4];
        let inputs = vec![phase("speed", 0, 1.0)];
        let isf_inputs = vec![float_input("speed", 1.0, 0.0, 5.0)];
        let mut params = ShaderParams::from_inputs(&isf_inputs);
        let modulation = no_modulation();

        // Run 10 frames at speed=1.0, dt=0.016
        for _ in 0..10 {
            accumulate_phase_times(
                &mut accum,
                0.016,
                Some(&inputs),
                &mut params,
                &modulation,
                "deck0",
            );
        }
        let before_change = accum[0];

        // Change speed to 3.0 — no jump should occur
        params.set_float("speed", 3.0);
        accumulate_phase_times(
            &mut accum,
            0.016,
            Some(&inputs),
            &mut params,
            &modulation,
            "deck0",
        );
        let after_change = accum[0];

        // Value should increase by dt*3.0, not jump to TIME*3.0
        let expected_delta = 0.016 * 3.0;
        assert!(
            (after_change - before_change - expected_delta).abs() < 1e-5,
            "Phase time should be continuous: before={before_change}, after={after_change}, expected delta={expected_delta}"
        );
    }

    #[test]
    fn accumulate_phase_times_multi_index() {
        let mut accum = [0.0f32; 4];
        let inputs = vec![
            phase("speed", 0, 1.0),
            phase("rot_x", 1, 1.0),
            phase("rot_y", 2, 1.0),
            phase("rot_z", 3, 1.0),
        ];
        let isf_inputs = vec![
            float_input("speed", 1.0, 0.0, 5.0),
            float_input("rot_x", 0.5, -1.0, 1.0),
            float_input("rot_y", 0.3, -1.0, 1.0),
            float_input("rot_z", 0.0, -1.0, 1.0),
        ];
        let mut params = ShaderParams::from_inputs(&isf_inputs);

        accumulate_phase_times(
            &mut accum,
            0.1,
            Some(&inputs),
            &mut params,
            &no_modulation(),
            "deck0",
        );
        assert!((accum[0] - 0.1).abs() < 1e-5); // speed=1.0 * 0.1
        assert!((accum[1] - 0.05).abs() < 1e-5); // rot_x=0.5 * 0.1
        assert!((accum[2] - 0.03).abs() < 1e-5); // rot_y=0.3 * 0.1
        assert!((accum[3] - 0.0).abs() < 1e-5); // rot_z=0.0 * 0.1
    }

    #[test]
    fn accumulate_phase_times_none_is_noop() {
        let mut accum = [0.0f32; 4];
        let mut params = ShaderParams::from_inputs(&[]);
        accumulate_phase_times(
            &mut accum,
            0.1,
            None,
            &mut params,
            &no_modulation(),
            "deck0",
        );
        assert_eq!(accum, [0.0; 4]);
    }

    #[test]
    fn accumulate_phase_times_uses_modulated_value() {
        let mut accum = [0.0f32; 4];
        let inputs = vec![phase("speed", 0, 1.0)];
        let isf_inputs = vec![float_input("speed", 1.0, 0.0, 5.0)];
        let mut params = ShaderParams::from_inputs(&isf_inputs);

        // amount 0.5 over a 0..5 range lifts the effective speed by 2.5 → 3.5
        let modulation = constant_modulation("deck0:speed", 0.5);
        accumulate_phase_times(
            &mut accum,
            0.1,
            Some(&inputs),
            &mut params,
            &modulation,
            "deck0",
        );
        assert!(
            (accum[0] - 0.35).abs() < 1e-5,
            "modulated speed 3.5 over dt 0.1 should accumulate 0.35, got {}",
            accum[0]
        );
    }

    #[test]
    fn accumulate_phase_times_ignores_modulation_of_other_prefixes() {
        let mut accum = [0.0f32; 4];
        let inputs = vec![phase("speed", 0, 1.0)];
        let isf_inputs = vec![float_input("speed", 1.0, 0.0, 5.0)];
        let mut params = ShaderParams::from_inputs(&isf_inputs);

        let modulation = constant_modulation("deck9:speed", 0.5);
        accumulate_phase_times(
            &mut accum,
            0.1,
            Some(&inputs),
            &mut params,
            &modulation,
            "deck0",
        );
        assert!((accum[0] - 0.1).abs() < 1e-5);
    }

    #[test]
    fn accumulate_phase_times_modulation_onset_is_continuous() {
        let inputs = vec![phase("speed", 0, 1.0)];
        let isf_inputs = vec![float_input("speed", 1.0, 0.0, 5.0)];
        let mut params = ShaderParams::from_inputs(&isf_inputs);

        let unmodulated = no_modulation();
        let mut accum = [0.0f32; 4];
        for _ in 0..10 {
            accumulate_phase_times(
                &mut accum,
                0.016,
                Some(&inputs),
                &mut params,
                &unmodulated,
                "deck0",
            );
        }
        let before = accum[0];

        // Modulation kicking in changes the rate, never the phase itself.
        let modulated = constant_modulation("deck0:speed", 0.4);
        accumulate_phase_times(
            &mut accum,
            0.016,
            Some(&inputs),
            &mut params,
            &modulated,
            "deck0",
        );
        let expected_delta = 0.016 * (1.0 + 0.4 * 5.0);
        assert!(
            (accum[0] - before - expected_delta).abs() < 1e-5,
            "phase should stay continuous across modulation onset: before={before}, after={}, expected delta={expected_delta}",
            accum[0]
        );
    }

    #[test]
    fn accumulate_phase_times_multiplies_second_param_into_rate() {
        let mut accum = [0.0f32; 4];
        let inputs = vec![phase_product("speed", &["rot_speed"], 0, 0.5)];
        let isf_inputs = vec![
            float_input("speed", 2.0, 0.0, 5.0),
            float_input("rot_speed", 3.0, 0.0, 5.0),
        ];
        let mut params = ShaderParams::from_inputs(&isf_inputs);

        // dt 0.1 × speed 2.0 × rot_speed 3.0 × scale 0.5 → 0.3
        accumulate_phase_times(
            &mut accum,
            0.1,
            Some(&inputs),
            &mut params,
            &no_modulation(),
            "deck0",
        );
        assert!((accum[0] - 0.3).abs() < 1e-5, "got {}", accum[0]);
    }

    #[test]
    fn accumulate_phase_times_product_responds_to_modulating_either_operand() {
        let inputs = vec![phase_product("speed", &["rot_speed"], 0, 1.0)];
        let isf_inputs = vec![
            float_input("speed", 1.0, 0.0, 5.0),
            float_input("rot_speed", 1.0, 0.0, 5.0),
        ];

        // Modulating the second operand must move the rate just as the first does.
        for target in ["deck0:speed", "deck0:rot_speed"] {
            let mut params = ShaderParams::from_inputs(&isf_inputs);
            let mut accum = [0.0f32; 4];
            accumulate_phase_times(
                &mut accum,
                0.1,
                Some(&inputs),
                &mut params,
                &constant_modulation(target, 0.2),
                "deck0",
            );
            // The modulated operand becomes 1.0 + 0.2 * 5.0 = 2.0, the other stays 1.0.
            assert!(
                (accum[0] - 0.2).abs() < 1e-5,
                "modulating {target} should double the rate, got {}",
                accum[0]
            );
        }
    }

    #[test]
    fn accumulate_phase_times_multiplies_every_listed_factor() {
        let mut accum = [0.0f32; 4];
        let inputs = vec![phase_product(
            "speed",
            &["time_scale", "flow_speed"],
            0,
            1.0,
        )];
        let isf_inputs = vec![
            float_input("speed", 2.0, 0.0, 5.0),
            float_input("time_scale", 1.5, 0.0, 5.0),
            float_input("flow_speed", 3.0, 0.0, 5.0),
        ];
        let mut params = ShaderParams::from_inputs(&isf_inputs);

        // dt 0.1 × 2.0 × 1.5 × 3.0 → 0.9
        accumulate_phase_times(
            &mut accum,
            0.1,
            Some(&inputs),
            &mut params,
            &no_modulation(),
            "deck0",
        );
        assert!((accum[0] - 0.9).abs() < 1e-5, "got {}", accum[0]);
    }

    #[test]
    fn accumulate_phase_times_missing_multiply_by_target_is_unit_factor() {
        let mut accum = [0.0f32; 4];
        let inputs = vec![phase_product("speed", &["not_a_param"], 0, 1.0)];
        let isf_inputs = vec![float_input("speed", 2.0, 0.0, 5.0)];
        let mut params = ShaderParams::from_inputs(&isf_inputs);

        accumulate_phase_times(
            &mut accum,
            0.1,
            Some(&inputs),
            &mut params,
            &no_modulation(),
            "deck0",
        );
        assert!((accum[0] - 0.2).abs() < 1e-5, "got {}", accum[0]);
    }

    #[test]
    fn accumulate_phase_times_product_is_continuous_across_operand_change() {
        let inputs = vec![phase_product("speed", &["rot_speed"], 0, 1.0)];
        let isf_inputs = vec![
            float_input("speed", 1.0, 0.0, 5.0),
            float_input("rot_speed", 1.0, 0.0, 5.0),
        ];
        let mut params = ShaderParams::from_inputs(&isf_inputs);
        let modulation = no_modulation();

        let mut accum = [0.0f32; 4];
        for _ in 0..10 {
            accumulate_phase_times(
                &mut accum,
                0.016,
                Some(&inputs),
                &mut params,
                &modulation,
                "deck0",
            );
        }
        let before = accum[0];

        params.set_float("rot_speed", 4.0);
        accumulate_phase_times(
            &mut accum,
            0.016,
            Some(&inputs),
            &mut params,
            &modulation,
            "deck0",
        );
        let expected_delta = 0.016 * 4.0;
        assert!(
            (accum[0] - before - expected_delta).abs() < 1e-5,
            "phase should stay continuous when the second operand changes: before={before}, after={}",
            accum[0]
        );
    }

    #[test]
    fn accumulate_phase_times_modulation_clamps_to_param_range() {
        let mut accum = [0.0f32; 4];
        let inputs = vec![phase("speed", 0, 1.0)];
        let isf_inputs = vec![float_input("speed", 4.0, 0.0, 5.0)];
        let mut params = ShaderParams::from_inputs(&isf_inputs);

        // 4.0 + 1.0 * 5.0 would be 9.0; the parameter max caps it at 5.0
        let modulation = constant_modulation("deck0:speed", 1.0);
        accumulate_phase_times(
            &mut accum,
            0.1,
            Some(&inputs),
            &mut params,
            &modulation,
            "deck0",
        );
        assert!((accum[0] - 0.5).abs() < 1e-5, "got {}", accum[0]);
    }

    // ── An offline input draws nothing ───────────────────────────────

    /// An offline input runs none of its effects: one that fills the frame
    /// would cover the layers under it with its clear picture.
    #[test]
    fn an_offline_input_runs_none_of_its_effects() {
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        let invert = || {
            let shader = crate::isf::ISFShader::from_file(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("shaders/invert.fs"),
            )
            .unwrap();
            crate::deck::Effect::new_with_format(&gpu, shader, gpu.compositing_format).unwrap()
        };
        let mut online =
            crate::deck::Deck::new_solid_color(&gpu, [1.0, 0.0, 0.0, 1.0], 64, 64).unwrap();
        online.effects.push(invert());
        assert_eq!(online.drawn_effects(&no_modulation()), vec![0]);

        let mut offline = crate::deck::Deck::new_offline(
            &gpu,
            "logo".into(),
            crate::scene::SourceConfig::Image {
                path: "/gone/logo.png".into(),
                scaling_mode: ScalingMode::default(),
            },
            "not found".into(),
            64,
            64,
        );
        offline.effects.push(invert());
        assert!(offline.drawn_effects(&no_modulation()).is_empty());
    }

    // ── Offensive: zero-size texture guard on resize ─────────────────

    #[test]
    fn deck_resize_zero_dimensions_does_not_panic() {
        let gpu = crate::renderer::GpuContext::new_headless();
        let Ok(gpu) = gpu else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        let mut deck = crate::deck::Deck::new_solid_color(&gpu, [1.0, 0.0, 0.0, 1.0], 64, 64)
            .expect("solid color deck creation should succeed");

        // Zero width — must not panic (clamped to 1)
        deck.resize(&gpu, 0, 64);

        // Zero height — must not panic (clamped to 1)
        deck.resize(&gpu, 64, 0);

        // Both zero — must not panic (clamped to 1x1)
        deck.resize(&gpu, 0, 0);

        // Normal resize still works
        deck.resize(&gpu, 128, 128);
    }

    // ── SVG image decks rasterize against the master resolution ──────

    /// A 4:1 drawing, so a stretched rasterization is distinguishable from a
    /// fitted one and the deck's own scaling mode has something to work with.
    const WIDE_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg"
        viewBox="0 0 200 50" width="200" height="50">
        <rect width="200" height="50" fill="#3050ff"/></svg>"##;

    fn image_source_size(deck: &crate::deck::Deck) -> (u32, u32) {
        match &deck.source {
            DeckSource::Image {
                source_width,
                source_height,
                ..
            } => (*source_width, *source_height),
            _ => panic!("expected an image deck"),
        }
    }

    #[test]
    fn an_svg_deck_is_drawn_at_the_deck_size_not_the_files_own_size() {
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("art.svg");
        std::fs::write(&path, WIDE_SVG).expect("write svg");

        let deck = crate::deck::Deck::new_from_image(&gpu, &path, 1920, 1080).expect("svg deck");
        // Not (200, 50): vector art is drawn for the stage it lands on, and it
        // fills the width without being stretched to the deck's 16:9.
        assert_eq!(image_source_size(&deck), (1920, 480));
    }

    #[test]
    fn changing_the_master_resolution_redraws_the_svg() {
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("art.svg");
        std::fs::write(&path, WIDE_SVG).expect("write svg");

        let mut deck = crate::deck::Deck::new_from_image(&gpu, &path, 640, 360).expect("svg deck");
        assert_eq!(image_source_size(&deck), (640, 160));

        // Going up to 4K must redraw rather than magnify the 640 px raster.
        deck.resize(&gpu, 3840, 2160);
        assert_eq!(image_source_size(&deck), (3840, 960));

        deck.resize(&gpu, 1280, 720);
        assert_eq!(image_source_size(&deck), (1280, 320));
    }

    #[test]
    fn a_raster_image_keeps_its_own_pixels_across_a_resize() {
        // The counterpart to the SVG behaviour: a PNG has real pixels and there
        // is nothing to redraw, so resizing must leave the source alone.
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("art.png");
        image::RgbaImage::from_pixel(80, 40, image::Rgba([10, 20, 30, 255]))
            .save(&path)
            .expect("write png");

        let mut deck = crate::deck::Deck::new_from_image(&gpu, &path, 640, 360).expect("png deck");
        assert_eq!(image_source_size(&deck), (80, 40));
        deck.resize(&gpu, 3840, 2160);
        assert_eq!(image_source_size(&deck), (80, 40));
    }
}
