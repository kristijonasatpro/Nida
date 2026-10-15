//! Effect (ISF filter) implementation and `PassBuffer` for multi-pass effects.

use super::source::load_imported_textures;
use super::{Deck, Effect, PassBuffer};
use crate::isf::{compile_glsl_to_spirv, ISFPass, ISFShader};
use crate::params::ShaderParams;
use crate::renderer::{GpuContext, ISFUniforms, UnifiedPipeline};
use anyhow::{Context, Result};
use std::collections::HashMap;

impl Effect {
    /// Create a new effect from an ISF filter shader.
    ///
    /// Targets `compositing_format`, the same as channel and master effects, so
    /// an effect behaves identically at any of the three tiers. See
    /// spec/unified-color-pipeline.md.
    ///
    /// # Errors
    ///
    /// Returns an error if the ISF fragment source fails to compile to SPIR-V,
    /// or if the render pipeline cannot be created for the target format.
    pub fn new(context: &GpuContext, shader: ISFShader) -> Result<Self> {
        Self::new_with_format(context, shader, context.compositing_format)
    }

    /// Create a new effect with a specific target format
    ///
    /// # Errors
    ///
    /// Returns an error if the ISF fragment source fails to compile to SPIR-V,
    /// or if the render pipeline cannot be created for `target_format`.
    pub fn new_with_format(
        context: &GpuContext,
        shader: ISFShader,
        target_format: wgpu::TextureFormat,
    ) -> Result<Self> {
        let spirv = compile_glsl_to_spirv(&shader.fragment_source, &shader.name())
            .context("Failed to compile filter shader to SPIR-V")?;

        let passes: Vec<ISFPass> = shader.metadata.passes.clone().unwrap_or_default();
        let num_passes = passes.iter().filter(|p| p.target.is_some()).count();

        // Load ISF IMPORTED images
        let imported_textures =
            load_imported_textures(&shader.metadata, shader.file_path.as_deref(), context);

        // Create preprocessor texture slots from ISF PREPROCESSORS declarations
        let preprocessor_textures: Vec<crate::deck::PreprocessorSlot> = shader
            .metadata
            .preprocessors
            .iter()
            .map(|pp| {
                // Create a 1×1 placeholder texture (will be resized when analyzer provides data)
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
                crate::deck::PreprocessorSlot {
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
            target_format,
            true, // has_input_image — it's a filter
            num_passes,
            imported_textures.len(),
            preprocessor_textures.len(),
        )
        .context("Failed to create effect pipeline")?;

        // Create pass buffers for multi-pass effects
        let width = 1920u32; // Internal resolution
        let height = 1080u32;
        let mut pass_buffers = HashMap::new();

        for pass in &passes {
            let target_name = match &pass.target {
                Some(name) => name.clone(),
                None => continue,
            };

            let pass_width = Deck::parse_size_expression(pass.width.as_deref(), width);
            let pass_height = Deck::parse_size_expression(pass.height.as_deref(), height);
            let is_persistent = pass.persistent.unwrap_or(false);

            // Pass buffers follow the effect's own target format so a deck,
            // channel, and master instance of the same shader behave identically.
            let format = target_format;

            let tex_a = context.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(&format!("Effect Pass Buffer A: {target_name}")),
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

            // Double-buffered whether persistent or not — see the note in
            // source.rs: a single-textured pass target aliases COLOR_TARGET with
            // RESOURCE in its own pass and wgpu rejects it.
            let (tex_b, view_b) = {
                let tex = context.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(&format!("Effect Pass Buffer B: {target_name}")),
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
                    texture_b: tex_b,
                    view_b,
                    persistent: is_persistent,
                    read_idx: 0,
                },
            );
        }

        // Initialize parameters from shader inputs
        let inputs = shader.metadata.inputs.as_deref().unwrap_or(&[]);
        let params = ShaderParams::from_inputs(inputs);
        let phase_inputs_config = shader.metadata.phase_inputs.clone();

        let uuid = crate::deck::generate_short_uuid();
        let param_prefix = format!("fx_{uuid}");
        let pointwise = shader.is_pointwise();

        Ok(Self {
            uuid,
            param_prefix,
            shader,
            pipeline,
            enabled: true,
            params,
            pass_buffers,
            passes,
            target_format,
            imported_textures,
            preprocessor_textures,
            phase_accumulators: [0.0; 4],
            phase_inputs_config,
            runs: 0,
            pointwise,
            mask: None,
            mask_pass: None,
        })
    }

    /// Frames this effect has been run on a deck. Neutral crop and transform
    /// are skipped, so theirs stays at zero until a setting moves.
    pub fn runs(&self) -> u64 {
        self.runs
    }

    /// This effect's stable UUID.
    pub fn uuid(&self) -> &str {
        &self.uuid
    }

    /// The cached `fx_{uuid}` prefix modulation targets are keyed under.
    pub fn param_prefix(&self) -> &str {
        &self.param_prefix
    }

    /// Whether this is crop or transform with nothing modulating it: a pure
    /// function of its settings, so its output only changes when they do.
    pub fn is_static_framing(&self, modulation: &crate::modulation::ModulationEngine) -> bool {
        matches!(self.shader.name().as_str(), "crop" | "transform")
            && !self.is_modulated(modulation)
            && self.mask.is_none()
    }

    /// Whether any modulation targets one of this effect's parameters.
    fn is_modulated(&self, modulation: &crate::modulation::ModulationEngine) -> bool {
        let key = format!("{}:", self.param_prefix);
        modulation
            .assignments
            .iter()
            .any(|(target, mods)| !mods.is_empty() && target.starts_with(&key))
    }

    /// Whether this is crop or transform at its neutral settings with nothing
    /// modulating it: every pixel stays where it was and as opaque as it came
    /// in (see [`Deck::covers_frame`]). Every new deck carries both, so without
    /// this no deck would ever count as covering the frame.
    pub fn is_neutral_framing(&self, modulation: &crate::modulation::ModulationEngine) -> bool {
        use crate::params::ParamValue;
        self.is_static_framing(modulation)
            && self.params.param_order.iter().all(|name| {
                let default = self
                    .params
                    .definitions
                    .get(name)
                    .and_then(|d| d.default.as_ref());
                match (self.params.values.get(name), default) {
                    (Some(ParamValue::Float(value)), Some(default)) => default
                        .as_f64()
                        .is_some_and(|d| (f64::from(*value) - d).abs() < 1e-6),
                    // Transform's "Separate X / Y scale" switch, the crop's
                    // Border.
                    (Some(ParamValue::Bool(value)), Some(default)) => {
                        default.as_bool() == Some(*value)
                    }
                    // The border's colour: every crop has one.
                    (Some(ParamValue::Color(value)), Some(default)) => {
                        default.as_array().is_some_and(|d| {
                            d.len() == value.len()
                                && d.iter().zip(value).all(|(d, v)| {
                                    d.as_f64().is_some_and(|d| (d - f64::from(*v)).abs() < 1e-6)
                                })
                        })
                    }
                    _ => false,
                }
            })
    }

    /// A float setting as it stands, unmodulated.
    fn float(&self, name: &str) -> Option<f32> {
        match self.params.values.get(name)? {
            crate::params::ParamValue::Float(v) => Some(*v),
            _ => None,
        }
    }

    /// Whether the crop's border is a circle: not, for a crop saved before
    /// there was one; `None` for a value that is not a switch, which no
    /// sum here can take for either.
    fn circle(&self) -> Option<bool> {
        match self.params.values.get("border_circle") {
            None => Some(false),
            Some(crate::params::ParamValue::Bool(v)) => Some(*v),
            Some(_) => None,
        }
    }

    /// A layer's own mask with no shape: it does nothing, rather than hide
    /// the whole layer.
    pub(crate) fn is_idle_layer_mask(&self) -> bool {
        self.mask.is_none() && self.shader.name() == crate::engine::value::mask::LAYER_MASK_EFFECT
    }

    fn switch(&self, name: &str) -> Option<bool> {
        match self.params.values.get(name)? {
            crate::params::ParamValue::Bool(v) => Some(*v),
            _ => None,
        }
    }

    /// Whether Transform mirrors the picture, left for right and top for
    /// bottom. One saved before there was a flip has none.
    pub(crate) fn flips(&self) -> [bool; 2] {
        let on = |name| {
            matches!(
                self.params.values.get(name),
                Some(crate::params::ParamValue::Bool(true))
            )
        };
        [on("flip_x"), on("flip_y")]
    }

    /// Transform's settings, when it is one and nothing modulates it:
    /// position, scale per axis, rotation, and whether it wraps.
    fn placement(
        &self,
        modulation: &crate::modulation::ModulationEngine,
    ) -> Option<([f32; 2], [f32; 2], f32, bool)> {
        if self.shader.name() != "transform" || self.is_modulated(modulation) {
            return None;
        }
        let scale = self.float("scale")?;
        let scale = if self.switch("scale_mode")? {
            [
                scale * self.float("scale_x")?,
                scale * self.float("scale_y")?,
            ]
        } else {
            [scale, scale]
        };
        Some((
            [self.float("pos_x")?, self.float("pos_y")?],
            scale.map(|s| s.max(1e-4)),
            self.float("rotation")?,
            self.float("wrap")?.round() != 0.0,
        ))
    }

    /// A transform that only moves and scales — not turned, not wrapped, not
    /// masked or modulated: where the picture's centre goes (offset from the
    /// frame's, in frame units) and its scale. Such a move can be done by the
    /// pass that draws the picture (see `Deck::folded_placement`).
    pub(crate) fn plain_placement(
        &self,
        modulation: &crate::modulation::ModulationEngine,
    ) -> Option<([f32; 2], [f32; 2])> {
        if self.mask.is_some() {
            return None;
        }
        let (pos, scale, rotation, wraps) = self.placement(modulation)?;
        (rotation.abs() < 1e-6 && !wraps).then_some((pos, scale))
    }

    /// A crop not inverted, masked or modulated, as the pass that draws the
    /// picture can do it (see `Deck::fold`).
    pub(crate) fn plain_crop(
        &self,
        modulation: &crate::modulation::ModulationEngine,
    ) -> Option<crate::renderer::placed_picture::Crop> {
        if self.shader.name() != "crop" || self.is_modulated(modulation) || self.mask.is_some() {
            return None;
        }
        if self.float("invert_crop")? > 0.5 {
            return None;
        }
        let border = if self.switch("border_mode")? {
            let color = match self.params.values.get("border_color")? {
                crate::params::ParamValue::Color(c) => *c,
                _ => return None,
            };
            Some(crate::renderer::placed_picture::Border {
                color,
                width: self.float("border_width")?,
                radius: self.float("border_radius")?,
                circle: if self.circle()? { 1.0 } else { 0.0 },
                strength: 1.0,
            })
        } else {
            None
        };
        Some(crate::renderer::placed_picture::Crop {
            left: self.float("crop_left")?,
            right: self.float("crop_right")?,
            bottom: self.float("crop_bottom")?,
            top: self.float("crop_top")?,
            feather: self.float("feather")?,
            border,
            strength: 1.0,
        })
    }

    /// Where a picture opaque in `area` (UV) is still certain to be opaque
    /// after this effect, on a frame `aspect` wide per unit high: cut down by
    /// crop, moved and scaled by transform. `None` for anything else, or
    /// framing that is modulated, inverted, rotated or wrapped.
    ///
    /// `fringe` is how many texels an edge may be soft by — crop
    /// anti-aliases its edges, and transform resamples them.
    pub(crate) fn opaque_after(
        &self,
        area: crate::renderer::region::Area,
        aspect: f32,
        modulation: &crate::modulation::ModulationEngine,
        fringe: &mut f32,
    ) -> Option<crate::renderer::region::Area> {
        use crate::renderer::region::Area;
        if self.mask.is_some() {
            return None;
        }
        if self.shader.name() == "crop" {
            if self.is_modulated(modulation) || self.float("invert_crop")? > 0.5 {
                return None;
            }
            // Past the feather, and inside the rounded corners. A circle's
            // is not worked out: nothing is hidden behind it.
            if self.switch("border_mode")? && self.circle()? {
                return None;
            }
            let feather = self.float("feather")?.max(0.0);
            let radius = if self.switch("border_mode")? {
                self.float("border_radius")?.max(0.0) / 1080.0
            } else {
                0.0
            };
            // Even an edge left at the frame's own is worked out per pixel,
            // and may leave the outermost pixels a hair short of opaque.
            // Top and Bottom measured up from the bottom; areas run down.
            let kept = Area {
                x0: self.float("crop_left")?,
                y0: 1.0 - self.float("crop_top")?,
                x1: self.float("crop_right")?,
                y1: 1.0 - self.float("crop_bottom")?,
            }
            .shrunk(feather + radius / aspect.max(1e-4), feather + radius);
            *fringe = fringe.max(1.0);
            return Some(area.intersect(&kept));
        }
        let (pos, scale, rotation, wraps) = self.placement(modulation)?;
        if wraps || rotation.abs() > 1e-6 {
            return None;
        }
        // The shader shows source point p at (p − ½)·scale + ½ + position —
        // its mirror, flipped — and nothing for a point off the source's
        // frame, whose edge is then an edge of the picture.
        let flip = self.flips();
        let mirrored = |v: f32, axis: usize| if flip[axis] { 1.0 - v } else { v };
        let place = |[x, y]: [f32; 2]| {
            [
                (mirrored(x, 0) - 0.5) * scale[0] + 0.5 + pos[0],
                (mirrored(y, 1) - 0.5) * scale[1] + 0.5 + pos[1],
            ]
        };
        let within = area.intersect(&Area::FRAME);
        // Nothing opaque stays nothing: an empty area's corners, mirrored,
        // would span what lies between them (a flipped crop keeping none).
        if within.is_empty() {
            return Some(within);
        }
        *fringe = *fringe * scale[0].max(scale[1]) + 1.0;
        Some(Area::around(within.corners().map(place)))
    }

    /// The parts of its input this effect reads to put out `areas` (UV) —
    /// the same for a pointwise filter, moved back through a transform on
    /// a frame `aspect` wide per unit high — or `None` when it may read
    /// anywhere.
    pub(crate) fn input_needed(
        &self,
        areas: &[crate::renderer::region::Area],
        aspect: f32,
        modulation: &crate::modulation::ModulationEngine,
    ) -> Option<Vec<crate::renderer::region::Area>> {
        use crate::renderer::region::Area;
        if self.pointwise && self.passes.iter().all(|p| p.target.is_none()) {
            // A mask mixes each pixel with the same one before: pointwise too.
            return Some(areas.to_vec());
        }
        if self.mask.is_some() {
            return None;
        }
        let (pos, scale, rotation, wraps) = self.placement(modulation)?;
        if wraps {
            return None;
        }
        // The shader's own steps back from an output point to its source.
        let (sin, cos) = (-rotation).sin_cos();
        let flip = self.flips();
        let mirrored = |v: f32, axis: usize| if flip[axis] { 1.0 - v } else { v };
        let back = |[x, y]: [f32; 2]| {
            let d = [(x - 0.5 - pos[0]) * aspect, y - 0.5 - pos[1]];
            let d = [d[0] * cos - d[1] * sin, d[0] * sin + d[1] * cos];
            [
                mirrored(d[0] / aspect / scale[0] + 0.5, 0),
                mirrored(d[1] / scale[1] + 0.5, 1),
            ]
        };
        Some(
            areas
                .iter()
                .filter(|area| !area.is_empty())
                .map(|area| Area::around(area.corners().map(back)).intersect(&Area::FRAME))
                .filter(|area| !area.is_empty())
                .collect(),
        )
    }

    /// Set the UUID, used during scene restore to preserve identity.
    ///
    /// Rebuilds the modulation prefix in step. Assignments are stored as
    /// `fx_{uuid}:{param}`, so an effect that comes back under a different
    /// prefix than it was saved with loses every modulation routed at it.
    pub fn set_uuid(&mut self, uuid: String) {
        self.param_prefix = format!("fx_{uuid}");
        self.uuid = uuid;
    }

    /// Apply this effect to an input texture, outputting to target texture
    /// Optionally applies modulation to effect parameters using the given prefix
    ///
    /// # Errors
    ///
    /// Propagates any error from [`Effect::apply_with_modulation`], which is
    /// currently infallible.
    pub fn apply(
        &mut self,
        context: &GpuContext,
        input_view: &wgpu::TextureView,
        output_view: &wgpu::TextureView,
        uniforms: &ISFUniforms,
        cmd_buffers: &mut Vec<wgpu::CommandBuffer>,
    ) -> Result<()> {
        self.apply_with_modulation(
            context,
            input_view,
            output_view,
            uniforms,
            None,
            cmd_buffers,
        )
    }

    /// Apply this effect with modulation support
    ///
    /// # Errors
    ///
    /// Never fails today — recording the render passes is infallible. The
    /// `Result` is kept so callers stay source-compatible if a fallible step
    /// (pipeline recreation, pass-buffer reallocation) is added later.
    ///
    /// # Panics
    ///
    /// Panics if the user parameter buffer is absent immediately after
    /// `ensure_buffer` created it.
    pub fn apply_with_modulation(
        &mut self,
        context: &GpuContext,
        input_view: &wgpu::TextureView,
        output_view: &wgpu::TextureView,
        uniforms: &ISFUniforms,
        modulation: Option<&crate::modulation::ModulationEngine>,
        cmd_buffers: &mut Vec<wgpu::CommandBuffer>,
    ) -> Result<()> {
        self.apply_within(
            context,
            input_view,
            output_view,
            uniforms,
            modulation,
            cmd_buffers,
            None,
        )
    }

    /// [`Self::apply_with_modulation`], putting out only the parts `within`
    /// (scissor rectangles) and leaving the rest of `output_view` as it was
    /// — or all of it for `None`. A multi-pass effect still runs its own
    /// passes whole; only the last, into `output_view`, is limited.
    ///
    /// # Errors
    ///
    /// As [`Self::apply_with_modulation`].
    ///
    /// # Panics
    ///
    /// As [`Self::apply_with_modulation`].
    #[allow(clippy::too_many_arguments)]
    pub fn apply_within(
        &mut self,
        context: &GpuContext,
        input_view: &wgpu::TextureView,
        output_view: &wgpu::TextureView,
        uniforms: &ISFUniforms,
        modulation: Option<&crate::modulation::ModulationEngine>,
        cmd_buffers: &mut Vec<wgpu::CommandBuffer>,
        within: Option<&[crate::renderer::region::Pixels]>,
    ) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let Some(mask) = self.mask.clone() else {
            self.apply_passes(
                context,
                input_view,
                output_view,
                uniforms,
                modulation,
                cmd_buffers,
                within,
            );
            return Ok(());
        };
        // Masked: the effect draws aside, and the mask mixes it in.
        let (width, height) = (
            uniforms.render_size[0].max(1.0) as u32,
            uniforms.render_size[1].max(1.0) as u32,
        );
        let pass = match self.mask_pass.take() {
            Some(pass) if pass.fits(self.target_format, width, height) => pass,
            _ => crate::renderer::mask::MaskPass::new(
                &context.device,
                self.target_format,
                width,
                height,
            ),
        };
        self.apply_passes(
            context,
            input_view,
            pass.scratch_view(),
            uniforms,
            modulation,
            cmd_buffers,
            within,
        );
        pass.draw(
            &context.device,
            &context.queue,
            cmd_buffers,
            &mask,
            input_view,
            output_view,
            within,
        );
        self.mask_pass = Some(pass);
        Ok(())
    }

    /// The effect's own passes, from `input_view` into `output_view`.
    #[allow(clippy::too_many_arguments)]
    fn apply_passes(
        &mut self,
        context: &GpuContext,
        input_view: &wgpu::TextureView,
        output_view: &wgpu::TextureView,
        uniforms: &ISFUniforms,
        modulation: Option<&crate::modulation::ModulationEngine>,
        cmd_buffers: &mut Vec<wgpu::CommandBuffer>,
        within: Option<&[crate::renderer::region::Pixels]>,
    ) {
        let load = if within.is_some() {
            wgpu::LoadOp::Load
        } else {
            wgpu::LoadOp::Clear(wgpu::Color::BLACK)
        };
        if !self.enabled {
            return;
        }

        // Ensure user params buffer exists and update it (with modulation if available)
        self.params.ensure_buffer(&context.device);
        if let Some(mod_engine) = modulation {
            self.params.update_buffer_with_modulation(
                &context.queue,
                mod_engine,
                Some(&self.param_prefix),
            );
        } else {
            self.params.update_buffer(&context.queue);
        }
        let user_params_buffer = self
            .params
            .buffer()
            .expect("Buffer should exist after ensure_buffer");

        let imported_views: Vec<&wgpu::TextureView> =
            self.imported_textures.iter().map(|(_, _, v)| v).collect();
        let preprocessor_views: Vec<&wgpu::TextureView> = self
            .preprocessor_textures
            .iter()
            .map(|pp| &pp.view)
            .collect();

        let has_targeted_passes = self.passes.iter().any(|p| p.target.is_some());

        if has_targeted_passes {
            // Multi-pass effect: run targeted passes first, then final pass to output
            for pass_idx in 0..self.passes.len() {
                let pass = &self.passes[pass_idx];

                let target_name = match &pass.target {
                    Some(name) => name.clone(),
                    None => continue, // Final pass handled below
                };

                let iterations = 1;

                for _iter in 0..iterations {
                    let mut pass_uniforms = *uniforms;
                    pass_uniforms.pass_index = i32::try_from(pass_idx).unwrap_or(i32::MAX);
                    self.pipeline
                        .update_uniforms(&context.queue, &pass_uniforms);

                    let pass_buffer_views: Vec<&wgpu::TextureView> = self
                        .passes
                        .iter()
                        .filter_map(|p| p.target.as_ref().and_then(|t| self.pass_buffers.get(t)))
                        .map(super::PassBuffer::read_view)
                        .collect();

                    let bind_group = self.pipeline.create_bind_group(
                        &context.device,
                        Some(input_view),
                        &pass_buffer_views,
                        &imported_views,
                        &preprocessor_views,
                        Some(user_params_buffer),
                    );

                    let target_view = self
                        .pass_buffers
                        .get(&target_name)
                        .map_or(output_view, super::PassBuffer::write_view);

                    let mut encoder =
                        context
                            .device
                            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some(&format!("Effect Pass {pass_idx} Encoder")),
                            });

                    {
                        let mut render_pass =
                            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                                label: Some(&format!("Effect Pass {pass_idx} Render")),
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

                        render_pass.set_pipeline(&self.pipeline.pipeline);
                        render_pass.set_bind_group(0, &bind_group, &[]);
                        render_pass.draw(0..3, 0..1);
                    }

                    // Multipass intermediate passes MUST submit immediately —
                    // update_uniforms() overwrites the same buffer each iteration,
                    // so batching would cause all passes to see the last pass's data.
                    context.submit(std::iter::once(encoder.finish()));

                    if let Some(pb) = self.pass_buffers.get_mut(&target_name) {
                        pb.swap();
                    }
                }
            }

            // Final pass: render to output_view using pass buffer results + input
            let mut final_uniforms = *uniforms;
            final_uniforms.pass_index = i32::try_from(self.passes.len()).unwrap_or(i32::MAX);
            self.pipeline
                .update_uniforms(&context.queue, &final_uniforms);

            let pass_buffer_views: Vec<&wgpu::TextureView> = self
                .passes
                .iter()
                .filter_map(|p| p.target.as_ref().and_then(|t| self.pass_buffers.get(t)))
                .map(super::PassBuffer::read_view)
                .collect();

            let bind_group = self.pipeline.create_bind_group(
                &context.device,
                Some(input_view),
                &pass_buffer_views,
                &imported_views,
                &preprocessor_views,
                Some(user_params_buffer),
            );

            let mut encoder =
                context
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("Effect Final Pass Encoder"),
                    });

            {
                let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Effect Final Pass Render"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: output_view,
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

                render_pass.set_pipeline(&self.pipeline.pipeline);
                render_pass.set_bind_group(0, &bind_group, &[]);
                crate::renderer::region::draw_within(&mut render_pass, within);
            }

            cmd_buffers.push(encoder.finish());
        } else {
            // Simple single-pass effect
            self.pipeline.update_uniforms(&context.queue, uniforms);

            let bind_group = self.pipeline.create_bind_group(
                &context.device,
                Some(input_view),
                &[],
                &imported_views,
                &preprocessor_views,
                Some(user_params_buffer),
            );

            let mut encoder =
                context
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("Effect Render Encoder"),
                    });

            {
                let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Effect Render Pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: output_view,
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

                render_pass.set_pipeline(&self.pipeline.pipeline);
                render_pass.set_bind_group(0, &bind_group, &[]);
                crate::renderer::region::draw_within(&mut render_pass, within);
            }

            cmd_buffers.push(encoder.finish());
        }
    }
}

impl PassBuffer {
    /// Get the current read texture view
    pub fn read_view(&self) -> &wgpu::TextureView {
        if self.read_idx == 0 {
            &self.view_a
        } else {
            self.view_b.as_ref().unwrap_or(&self.view_a)
        }
    }

    /// Get the current write texture view — always the one not being read.
    pub fn write_view(&self) -> &wgpu::TextureView {
        if self.read_idx == 0 {
            self.view_b.as_ref().unwrap_or(&self.view_a)
        } else {
            &self.view_a
        }
    }

    /// Swap read/write buffers. Call after rendering the pass that targets this
    /// buffer, so later passes (and the next frame) read what was just written.
    ///
    /// Unconditional, including for non-persistent buffers: the read and write
    /// views must never be the same texture, or the pass that targets it binds
    /// it as both a colour attachment and a sampled resource. `persistent` now
    /// governs only whether the contents carry meaning across frames — which is
    /// what the ISF key actually means — not the buffering strategy.
    pub fn swap(&mut self) {
        self.read_idx = 1 - self.read_idx;
    }
}
