//! Preview texture pipeline: gamma-encoding engine textures into egui-ready
//! targets, and keeping egui's registrations in step with the live deck, channel
//! and output set.
//!
//! Split out of the runner because it is a self-contained concern: given the
//! engine's current textures, produce the `TextureId`s the UI needs this frame.

use super::UIRunner;

/// Which preview a `PreviewEncoder` target belongs to.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) enum PreviewSlot {
    Deck(String),
    Channel(usize),
    Main,
    Output(usize),
}

impl PreviewSlot {
    pub(super) fn key(&self) -> String {
        match self {
            PreviewSlot::Deck(uuid) => format!("deck:{uuid}"),
            PreviewSlot::Channel(idx) => format!("ch:{idx}"),
            PreviewSlot::Main => "main".to_string(),
            PreviewSlot::Output(idx) => format!("out:{idx}"),
        }
    }
}

/// One preview, encoded: the source sampled at each target pixel's centre,
/// linear light to sRGB — what the blit did for each, a fragment at a time.
const ENCODE_SHADER: &str = r"
@group(0) @binding(0) var texture_sampler: sampler;
@group(0) @binding(1) var source_texture: texture_2d<f32>;
@group(0) @binding(2) var preview: texture_storage_2d<rgba8unorm, write>;

// Linear to sRGB (IEC 61966-2-1), as the blit shader's `gamma_from_linear_rgb`.
fn gamma_from_linear_rgb(rgb: vec3<f32>) -> vec3<f32> {
    let c = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    let lower = c * 12.92;
    let higher = 1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(higher, lower, c < vec3<f32>(0.0031308));
}

@compute @workgroup_size(8, 8)
fn encode_preview(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(preview);
    if (id.x >= size.x || id.y >= size.y) {
        return;
    }
    let uv = (vec2<f32>(id.xy) + vec2<f32>(0.5)) / vec2<f32>(size);
    let color = textureSampleLevel(source_texture, texture_sampler, uv, 0.0);
    textureStore(
        preview,
        vec2<i32>(id.xy),
        vec4<f32>(gamma_from_linear_rgb(color.rgb), clamp(color.a, 0.0, 1.0)),
    );
}
";

/// Gamma-encodes linear engine textures so egui previews match the real output.
///
/// egui assumes every texture handed to it is already gamma-encoded ("We expect
/// 'normal' textures that are NOT sRGB-aware" — egui-wgpu's `egui.wgsl`), and
/// compensates by applying the inverse transfer function before writing to its
/// sRGB framebuffer. Handing it a linear texture therefore round-trips to raw
/// linear light on screen, which reads as far too dark: linear 0.2 displays as
/// 0.2 where the output window correctly shows 0.48.
///
/// Fix: blit each preview source through an explicit linear→sRGB encode into a
/// plain (non-sRGB) `Rgba8Unorm` texture, and register *that* with egui. The
/// target must not be `*UnormSrgb` or the hardware would decode on sample and
/// cancel the encode out.
///
/// One mechanism covers both kinds of source: linear float color-path textures
/// are sampled as-is, and already-sRGB output previews are hardware-decoded to
/// linear on sample. Either way the shader receives linear and emits gamma.
///
/// Targets are cached per key and recreated only when the source size changes.
/// They are capped to `MAX_DIM` on the long edge — previews are thumbnails, so
/// this also makes egui sample far less than the full render resolution.
///
/// All of a frame's previews are encoded in one compute pass, a dispatch
/// each, not a render pass each: wgpu gives every pass a command buffer of
/// its own, and a pass costs the GPU ~0.15 ms however little it draws —
/// measured on a laptop, a dozen previews a frame at ~0.2 ms each, all
/// but the floor (`renderer::gpu_time`).
pub(super) struct PreviewEncoder {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    targets: std::collections::HashMap<String, (wgpu::Texture, wgpu::TextureView)>,
    /// What each channel preview was last encoded from: the channel, and
    /// how many times it had been drawn (see `changed`).
    encoded: std::collections::HashMap<String, (String, u32)>,
}

impl PreviewEncoder {
    /// Long-edge cap for preview targets.
    const MAX_DIM: u32 = 960;

    fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Preview Encode"),
            source: wgpu::ShaderSource::Wgsl(ENCODE_SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Preview Encode"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        // Non-sRGB target on purpose — see the type-level comment.
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Preview Encode"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Preview Encode"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("encode_preview"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        // As the blit samples: linear, clamped to the edge.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Preview Encode"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        Self {
            pipeline,
            layout,
            sampler,
            targets: std::collections::HashMap::new(),
            encoded: std::collections::HashMap::new(),
        }
    }

    /// Whether the preview `key`, now `stamp` — its source and how many
    /// times that has been drawn — needs encoding: not already encoded at
    /// that. A channel off air is drawn a third of the frames (for its tile),
    /// so its preview is encoded as often, not every frame of the control
    /// window. Counted as encoded from here.
    fn changed(&mut self, key: &str, stamp: (String, u32)) -> bool {
        if self.encoded.get(key) == Some(&stamp) {
            return false;
        }
        self.encoded.insert(key.to_owned(), stamp);
        true
    }

    /// Scale `(w, h)` down so the long edge is at most `MAX_DIM`, preserving aspect.
    fn preview_size(w: u32, h: u32) -> (u32, u32) {
        let long = w.max(h);
        if long <= Self::MAX_DIM || long == 0 {
            return (w.max(1), h.max(1));
        }
        let scale = Self::MAX_DIM as f32 / long as f32;
        (
            ((w as f32 * scale).round() as u32).max(1),
            ((h as f32 * scale).round() as u32).max(1),
        )
    }

    /// Widths a preview target is made at, for a picture drawn smaller than
    /// the full preview: a tile is a fraction of the width of the Program
    /// picture, and the encode samples every target pixel — so each target
    /// is made the size it is drawn at, a step up.
    const WIDTHS: [u32; 5] = [160, 240, 320, 480, 640];

    /// The size to make a preview target: `full` (the source capped at
    /// `MAX_DIM`), or the smallest of `WIDTHS` (keeping `full`'s shape) that
    /// covers `drawn` — the largest it was drawn at, in pixels, if it was.
    /// Grown at once; shrunk only once drawn two-thirds of `current` or
    /// less, so a window being resized does not remake it every frame. Not
    /// drawn, it stays as it is.
    fn target_size(
        full: (u32, u32),
        drawn: Option<[u32; 2]>,
        current: Option<[u32; 2]>,
    ) -> [u32; 2] {
        let full = [full.0, full.1];
        let Some([drawn_w, drawn_h]) = drawn else {
            return current.unwrap_or(full);
        };
        let shaped = |width: u32| {
            let height =
                (u64::from(width) * u64::from(full[1])).div_ceil(u64::from(full[0].max(1)));
            [width, u32::try_from(height).unwrap_or(u32::MAX).max(1)]
        };
        let wanted = Self::WIDTHS
            .iter()
            .map(|&width| shaped(width))
            .find(|&[w, h]| w < full[0] && w >= drawn_w && h >= drawn_h)
            .unwrap_or(full);
        match current {
            Some(now) if wanted[0] < now[0] && wanted[0] * 3 > now[0] * 2 => now,
            _ => wanted,
        }
    }

    /// Create or resize the target for `key`. Returns true when the target was
    /// (re)created, meaning the caller must re-register it with egui — a
    /// `TextureId` is bound to a specific texture, so a resize invalidates it.
    fn ensure_target(
        &mut self,
        context: &crate::renderer::GpuContext,
        key: &str,
        src_w: u32,
        src_h: u32,
        drawn: Option<[u32; 2]>,
    ) -> bool {
        let current = self.targets.get(key).map(|(t, _)| [t.width(), t.height()]);
        let [w, h] = Self::target_size(Self::preview_size(src_w, src_h), drawn, current);
        let stale = self
            .targets
            .get(key)
            .is_none_or(|(t, _)| t.width() != w || t.height() != h);
        if stale {
            let texture = context.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Preview Encode Target"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.targets.insert(key.to_string(), (texture, view));
            // A new target holds nothing yet.
            self.encoded.remove(key);
        }
        stale
    }

    fn target_view(&self, key: &str) -> Option<&wgpu::TextureView> {
        self.targets.get(key).map(|(_, v)| v)
    }

    /// Encode all `sources` into their cached targets in a single command buffer.
    ///
    /// Must run every frame *after* the engine has rendered, and after output
    /// windows have drawn (window previews source their intermediate texture).
    /// Registration is separate: `TextureId`s must already exist when the UI is
    /// built earlier in the frame.
    ///
    /// One encoder and one submit for every preview — a submit per thumbnail
    /// would add a dozen per frame, which is exactly the cost the compositing
    /// path batches away for weaker GPUs. All previews share the same blit
    /// params, so those are written once up front.
    fn encode_all(
        &self,
        context: &crate::renderer::GpuContext,
        sources: &[(PreviewSlot, &wgpu::TextureView, u32, u32)],
    ) {
        if sources.is_empty() {
            return;
        }
        let mut encoder = context
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Preview Encode"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("Preview Encode Pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            for (slot, src, ..) in sources {
                let key = slot.key();
                let Some((target, target_view)) = self.targets.get(&key) else {
                    continue;
                };
                let bind_group = context
                    .device
                    .create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("Preview Encode"),
                        layout: &self.layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::Sampler(&self.sampler),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::TextureView(src),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: wgpu::BindingResource::TextureView(target_view),
                            },
                        ],
                    });
                pass.set_bind_group(0, &bind_group, &[]);
                pass.dispatch_workgroups(
                    target.width().div_ceil(8),
                    target.height().div_ceil(8),
                    1,
                );
            }
        }
        context.submit(std::iter::once(encoder.finish()));
    }

    /// Drop cached targets whose key is no longer live (removed decks/outputs).
    fn retain_keys(&mut self, live: &std::collections::HashSet<String>) {
        self.targets.retain(|k, _| live.contains(k));
        self.encoded.retain(|k, _| live.contains(k));
    }
}

impl UIRunner {
    /// Register GPU textures with egui for deck/channel/output previews and main output.
    pub(super) fn register_preview_textures(&mut self) {
        self.sync_preview_registrations();

        let Some(varda) = &self.varda else { return };
        let Some(egui_renderer) = &mut self.egui_renderer else {
            return;
        };
        let context = varda.gpu_context();

        // Dome preview renderer + texture
        if self.dome_preview_renderer.is_none() {
            match crate::renderer::dome_preview::DomePreviewRenderer::new(
                &context.device,
                wgpu::TextureFormat::Bgra8UnormSrgb,
            ) {
                Ok(renderer) => {
                    let tid = egui_renderer.register_native_texture(
                        &context.device,
                        &renderer.output_view,
                        wgpu::FilterMode::Linear,
                    );
                    self.dome_preview_texture = Some(tid);
                    self.dome_preview_renderer = Some(renderer);
                }
                Err(e) => log::error!("Failed to create dome preview renderer: {e}"),
            }
        }
    }

    /// Resolve an output preview's source view together with its dimensions, so
    /// the preview encoder can size its target to the right aspect ratio.
    ///
    /// Windowed outputs preview their own intermediate texture (which carries
    /// surface geometry and warp, and is the window's size); every headless
    /// source is a render-resolution composite, deck, or sub-mix.
    fn output_preview_source<'a>(
        output: &'a crate::renderer::context::UnifiedOutput,
        varda: &'a crate::app::VardaApp,
    ) -> (&'a wgpu::TextureView, u32, u32) {
        use crate::renderer::context::UnifiedOutput;
        let mixer = varda.mixer_ref();
        let view = Self::output_preview_view(output, varda);
        let (w, h) = match output {
            UnifiedOutput::Window(w) => (w.preview_texture.width(), w.preview_texture.height()),
            UnifiedOutput::Headless(_) => {
                let ct = mixer.composite_texture();
                (ct.width(), ct.height())
            }
        };
        (view, w, h)
    }

    /// Resolve the texture view to use for an output preview.
    /// Windowed outputs use their intermediate render texture (shows surface geometry + warp).
    /// Headless outputs resolve their source.
    pub(super) fn output_preview_view<'a>(
        output: &'a crate::renderer::context::UnifiedOutput,
        varda: &'a crate::app::VardaApp,
    ) -> &'a wgpu::TextureView {
        use crate::renderer::context::{OutputSource, UnifiedOutput};
        let mixer = varda.mixer_ref();
        match output {
            UnifiedOutput::Window(w) => &w.preview_texture_view,
            UnifiedOutput::Headless(h) => match &h.source {
                OutputSource::Master => mixer.composite_view(),
                // What the bus has on air, resolved every frame; black for a
                // bus with nothing on air or one that no longer exists.
                OutputSource::BusProgram(uuid) => varda
                    .bus_program_view(uuid)
                    .unwrap_or_else(|| mixer.black_view()),
                OutputSource::Channel(idx) => mixer
                    .channels()
                    .get(*idx)
                    .map_or_else(|| mixer.composite_view(), |c| &c.composite_view),
                OutputSource::Deck(ch, dk) => mixer
                    .channels()
                    .get(*ch)
                    .and_then(|c| c.decks.get(*dk))
                    .map_or_else(|| mixer.composite_view(), |s| &s.deck.texture_view),
                OutputSource::Channels(indices) => {
                    let mut sorted = indices.clone();
                    sorted.sort_unstable();
                    sorted.dedup();
                    mixer
                        .get_sub_mix_view(&sorted)
                        .unwrap_or_else(|| mixer.composite_view())
                }
                OutputSource::Domemaster => {
                    // Domemaster preview falls back to composite view;
                    // the actual domemaster texture is rendered in the output pipeline.
                    mixer.composite_view()
                }
            },
        }
    }

    /// Re-register GPU textures when deck/channel/output layout changes.
    /// Enumerate every live preview source: its slot, source view, and source size.
    ///
    /// Single source of truth shared by registration and encoding so the two
    /// passes cannot drift out of step.
    fn preview_sources(
        varda: &crate::app::VardaApp,
    ) -> Vec<(PreviewSlot, &wgpu::TextureView, u32, u32)> {
        let mixer = varda.mixer_ref();
        let mut out: Vec<(PreviewSlot, &wgpu::TextureView, u32, u32)> = Vec::new();
        for (ch_idx, ch) in mixer.channels().iter().enumerate() {
            for slot in &ch.decks {
                out.push((
                    PreviewSlot::Deck(slot.deck.uuid().to_string()),
                    &slot.deck.texture_view,
                    slot.deck.texture.width(),
                    slot.deck.texture.height(),
                ));
            }
            out.push((
                PreviewSlot::Channel(ch_idx),
                &ch.composite_view,
                ch.composite_texture.width(),
                ch.composite_texture.height(),
            ));
        }
        let ct = mixer.composite_texture();
        out.push((
            PreviewSlot::Main,
            mixer.composite_view(),
            ct.width(),
            ct.height(),
        ));
        for (out_idx, output) in varda.outputs_ref().iter().enumerate() {
            let (view, w, h) = Self::output_preview_source(output, varda);
            out.push((PreviewSlot::Output(out_idx), view, w, h));
        }
        out
    }

    /// Create/resize preview encode targets and keep egui registrations in sync.
    ///
    /// Runs early in the frame because the UI is built (and needs `TextureId`s)
    /// before any GPU work is submitted. Pixel content is filled in later by
    /// `encode_previews`.
    fn sync_preview_registrations(&mut self) {
        let Some(varda) = &self.varda else { return };
        let context = varda.gpu_context();

        if self.preview_encoder.is_none() {
            self.preview_encoder = Some(PreviewEncoder::new(&context.device));
        }
        let Some(egui_renderer) = self.egui_renderer.as_mut() else {
            return;
        };
        let Some(encoder) = self.preview_encoder.as_mut() else {
            return;
        };

        let sources = Self::preview_sources(varda);
        let mut live: std::collections::HashSet<String> =
            std::collections::HashSet::with_capacity(sources.len());

        for (slot, _src, w, h) in &sources {
            let key = slot.key();
            live.insert(key.clone());
            let texture = match slot {
                PreviewSlot::Deck(uuid) => self.deck_preview_textures.get(uuid).copied(),
                PreviewSlot::Channel(idx) => self.channel_preview_textures.get(idx).copied(),
                PreviewSlot::Main => self.main_output_texture,
                PreviewSlot::Output(idx) => self.output_preview_textures.get(idx).copied(),
            };
            let drawn = texture.and_then(|id| self.drawn_px.get(&id).copied());
            let recreated = encoder.ensure_target(context, &key, *w, *h, drawn);
            let known = match slot {
                PreviewSlot::Deck(uuid) => self.deck_preview_textures.contains_key(uuid),
                PreviewSlot::Channel(idx) => self.channel_preview_textures.contains_key(idx),
                PreviewSlot::Main => self.main_output_texture.is_some(),
                PreviewSlot::Output(idx) => self.output_preview_textures.contains_key(idx),
            };
            if !recreated && known {
                continue;
            }
            let Some(view) = encoder.target_view(&key) else {
                continue;
            };
            let tid = egui_renderer.register_native_texture(
                &context.device,
                view,
                wgpu::FilterMode::Linear,
            );
            // Retire the previous registration for this slot, if any — a resized
            // target leaves the old TextureId dangling.
            let stale = match slot {
                PreviewSlot::Deck(uuid) => self.deck_preview_textures.insert(uuid.clone(), tid),
                PreviewSlot::Channel(idx) => self.channel_preview_textures.insert(*idx, tid),
                PreviewSlot::Main => self.main_output_texture.replace(tid),
                PreviewSlot::Output(idx) => self.output_preview_textures.insert(*idx, tid),
            };
            if let Some(old) = stale {
                egui_renderer.free_texture(&old);
            }
        }

        // Retire registrations and targets for decks/outputs that are gone. This
        // is the only place they are retired, so skipping it leaks one texture
        // per removed entity.
        let live_decks: std::collections::HashSet<String> = sources
            .iter()
            .filter_map(|(s, ..)| match s {
                PreviewSlot::Deck(u) => Some(u.clone()),
                _ => None,
            })
            .collect();
        let stale_decks: Vec<String> = self
            .deck_preview_textures
            .keys()
            .filter(|u| !live_decks.contains(*u))
            .cloned()
            .collect();
        for uuid in stale_decks {
            if let Some(tid) = self.deck_preview_textures.remove(&uuid) {
                egui_renderer.free_texture(&tid);
            }
        }
        let live_outputs: std::collections::HashSet<usize> = sources
            .iter()
            .filter_map(|(s, ..)| match s {
                PreviewSlot::Output(i) => Some(*i),
                _ => None,
            })
            .collect();
        let stale_outputs: Vec<usize> = self
            .output_preview_textures
            .keys()
            .copied()
            .filter(|i| !live_outputs.contains(i))
            .collect();
        for idx in stale_outputs {
            if let Some(tid) = self.output_preview_textures.remove(&idx) {
                egui_renderer.free_texture(&tid);
            }
        }
        encoder.retain_keys(&live);
    }

    /// Gamma-encode the previews this frame's UI draws (`textures_drawn`) —
    /// a channel's only when it has been drawn since (see
    /// `PreviewEncoder::changed`).
    ///
    /// Every preview of every deck, channel, output and the main picture used
    /// to be encoded every frame of the control window, shown or not: on the
    /// laptop that was the GPU's biggest cost by far, ~1.4 s of pass time a
    /// second, ten times the control window's own drawing.
    ///
    /// Must run after the mixer render *and* after output windows have drawn, but
    /// before egui paints. See the frame sequence in `render_frame`.
    pub(super) fn encode_previews(&mut self) {
        let Some(varda) = &self.varda else { return };
        let Some(encoder) = &mut self.preview_encoder else {
            return;
        };
        let context = varda.gpu_context();
        let channels = varda.mixer_ref().channels();
        let sources: Vec<_> = Self::preview_sources(varda)
            .into_iter()
            .filter(|(slot, ..)| {
                let texture = match slot {
                    PreviewSlot::Deck(uuid) => self.deck_preview_textures.get(uuid).copied(),
                    PreviewSlot::Channel(idx) => self.channel_preview_textures.get(idx).copied(),
                    PreviewSlot::Main => self.main_output_texture,
                    PreviewSlot::Output(idx) => self.output_preview_textures.get(idx).copied(),
                };
                texture.is_some_and(|id| self.textures_drawn.contains(&id))
            })
            .filter(|(slot, ..)| match slot {
                PreviewSlot::Channel(idx) => channels.get(*idx).is_none_or(|ch| {
                    encoder.changed(&slot.key(), (ch.uuid().to_string(), ch.frames_drawn()))
                }),
                _ => true,
            })
            .collect();
        encoder.encode_all(context, &sources);
    }

    /// Per-frame egui texture sync. Previews are gamma-encoded into dedicated
    /// targets, so this only keeps targets and registrations in step; the pixels
    /// are written later by `encode_previews`.
    pub(super) fn refresh_textures(&mut self) {
        self.sync_preview_registrations();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Registrations ───────────────────────────────────────────────

    /// Previews are registered with egui only here, pointing at their
    /// gamma-encoded copies. The frame loop once re-pointed them at the
    /// engine's linear textures after a resolution change and after an undo
    /// that added or removed decks — and every preview showed dark and
    /// over-contrasty until a restart. It must not register previews at all.
    #[test]
    fn the_frame_loop_leaves_preview_registrations_to_the_preview_sync() {
        let runner = include_str!("mod.rs");
        for call in [
            "update_egui_texture_from_wgpu_texture",
            "register_native_texture",
        ] {
            assert!(
                !runner.contains(call),
                "runner/mod.rs calls {call}: previews must stay on their encoded copies"
            );
        }
    }

    // ── Encoding ────────────────────────────────────────────────────

    /// Read an `Rgba8Unorm` texture back, row by row.
    fn read_rgba8(gpu: &crate::renderer::GpuContext, texture: &wgpu::Texture) -> Vec<u8> {
        let (w, h) = (texture.width(), texture.height());
        let row = (w * 4).div_ceil(256) * 256;
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Preview readback"),
            size: u64::from(row * h),
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
                    rows_per_image: Some(h),
                },
            },
            texture.size(),
        );
        gpu.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv().expect("mapped").expect("mapped");
        let data = buffer.slice(..).get_mapped_range().expect("mapped range");
        (0..h as usize)
            .flat_map(|y| data[y * row as usize..y * row as usize + w as usize * 4].to_vec())
            .collect()
    }

    /// `source` (w × h) encoded by the compute pass, and blitted as before,
    /// both into a preview target's size: the two read back.
    fn encoded_and_blitted(
        gpu: &crate::renderer::GpuContext,
        source: &wgpu::Texture,
    ) -> (Vec<u8>, Vec<u8>) {
        let (w, h) = (source.width(), source.height());
        let source_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = PreviewEncoder::new(&gpu.device);
        encoder.ensure_target(gpu, "ch:0", w, h, None);
        encoder.encode_all(gpu, &[(PreviewSlot::Channel(0), &source_view, w, h)]);
        let target = &encoder.targets["ch:0"].0;
        let computed = read_rgba8(gpu, target);

        let blit = crate::renderer::BlitPipeline::new(&gpu.device, wgpu::TextureFormat::Rgba8Unorm)
            .expect("a blit pipeline");
        blit.set_srgb_encode(&gpu.queue, true);
        let blitted = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Blitted preview"),
            size: target.size(),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let blitted_view = blitted.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = blit.create_bind_group(&gpu.device, &source_view);
        let mut commands = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = commands.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Blitted preview"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &blitted_view,
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
            blit.render(&mut pass, &bind_group);
        }
        gpu.submit([commands.finish()]);
        (computed, read_rgba8(gpu, &blitted))
    }

    /// A linear-light gradient (w × h) with alpha, red past 1.0 and blue
    /// below 0.
    fn gradient(gpu: &crate::renderer::GpuContext, w: u32, h: u32) -> wgpu::Texture {
        let source = gpu.create_compositing_texture(w, h);
        let mut texels: Vec<u16> = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let (u, v) = (x as f32 / (w - 1) as f32, y as f32 / (h - 1) as f32);
                for c in [1.5 * u, v, 0.4 * v - 0.1, 0.25 + 0.75 * u] {
                    texels.push(half::f16::from_f32(c).to_bits());
                }
            }
        }
        gpu.queue.write_texture(
            source.as_image_copy(),
            bytemuck::cast_slice(&texels),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 8),
                rows_per_image: Some(h),
            },
            source.size(),
        );
        source
    }

    /// The previews look as they did when each was blitted on its own: the
    /// compute pass gives the blit's pixels — the source sampled at each
    /// pixel's centre, linear light to sRGB, out-of-range light clamped —
    /// at full size and scaled down to a preview's (1920 × 1080 to 960 × 540).
    #[test]
    fn a_preview_encoded_in_the_compute_pass_matches_the_blit() {
        let Ok(gpu) = crate::renderer::context::GpuContext::new_headless() else {
            return;
        };
        for (w, h) in [(128, 72), (1920, 1080)] {
            let source = gradient(&gpu, w, h);
            let (computed, expected) = encoded_and_blitted(&gpu, &source);
            assert!(
                gpu.errors.fault_count() == 0,
                "{:?}",
                gpu.errors.take_faults()
            );
            let worst = computed
                .iter()
                .zip(&expected)
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .expect("pixels");
            assert!(
                worst <= 1,
                "{w}×{h}: differs from the blit by up to {worst}"
            );
            assert!(computed.contains(&255), "light past 1.0 clamps to white");
        }
    }

    // ── Sizing ──────────────────────────────────────────────────────

    /// A preview target is made the size it is drawn at, a step up, never
    /// above the full preview or its source; it grows at once, shrinks only
    /// once drawn two-thirds the size or less, and stays as it is while not
    /// drawn.
    #[test]
    fn a_preview_is_made_the_size_it_is_drawn_at() {
        let full = (960, 540);
        let size = |drawn, current| PreviewEncoder::target_size(full, drawn, current);
        assert_eq!(size(None, None), [960, 540], "not drawn yet: full");
        assert_eq!(size(Some([300, 169]), None), [320, 180], "a tile");
        assert_eq!(
            size(Some([1400, 788]), Some([320, 180])),
            [960, 540],
            "the Program picture: full, at once"
        );
        assert_eq!(
            size(Some([600, 338]), Some([960, 540])),
            [640, 360],
            "drawn two-thirds the size: made smaller"
        );
        assert_eq!(
            size(Some([450, 253]), Some([640, 360])),
            [640, 360],
            "a little smaller: kept"
        );
        assert_eq!(
            size(None, Some([320, 180])),
            [320, 180],
            "not drawn: as it is"
        );
        assert_eq!(
            PreviewEncoder::target_size((200, 112), Some([300, 169]), None),
            [200, 112],
            "never bigger than its source"
        );
    }

    // ── Encoding only what changed ──────────────────────────────────

    /// A channel's preview is encoded when the channel has been drawn since,
    /// or another channel now stands in its place, or its target is new —
    /// not again for the same picture.
    #[test]
    fn a_channel_preview_is_encoded_only_when_its_picture_changed() {
        let Ok(gpu) = crate::renderer::context::GpuContext::new_headless() else {
            return;
        };
        let mut encoder = PreviewEncoder::new(&gpu.device);
        let stamp = |channel: &str, drawn: u32| (channel.to_owned(), drawn);
        assert!(encoder.changed("ch:0", stamp("a", 1)));
        assert!(!encoder.changed("ch:0", stamp("a", 1)), "the same picture");
        assert!(encoder.changed("ch:0", stamp("a", 2)), "drawn since");
        assert!(
            encoder.changed("ch:0", stamp("b", 2)),
            "another channel in its place"
        );
        assert!(encoder.ensure_target(&gpu, "ch:0", 64, 36, None));
        assert!(
            encoder.changed("ch:0", stamp("b", 2)),
            "a new target holds nothing yet"
        );
    }

    // ── PreviewSlot keys ────────────────────────────────────────────

    /// Encoder targets are cached by this key, so two slots must never collide —
    /// a collision would show one preview's pixels in another's panel.
    #[test]
    fn preview_slot_keys_are_distinct_across_variants() {
        let keys = [
            PreviewSlot::Deck("0".to_string()).key(),
            PreviewSlot::Channel(0).key(),
            PreviewSlot::Main.key(),
            PreviewSlot::Output(0).key(),
        ];
        let unique: std::collections::HashSet<&String> = keys.iter().collect();
        assert_eq!(unique.len(), keys.len(), "keys collide: {keys:?}");
    }

    #[test]
    fn preview_slot_keys_are_stable_and_namespaced() {
        assert_eq!(PreviewSlot::Deck("a1b2".to_string()).key(), "deck:a1b2");
        assert_eq!(PreviewSlot::Channel(3).key(), "ch:3");
        assert_eq!(PreviewSlot::Main.key(), "main");
        assert_eq!(PreviewSlot::Output(2).key(), "out:2");
    }

    #[test]
    fn preview_slot_keys_differ_per_index_and_uuid() {
        assert_ne!(PreviewSlot::Channel(1).key(), PreviewSlot::Channel(2).key());
        assert_ne!(PreviewSlot::Output(1).key(), PreviewSlot::Output(2).key());
        assert_ne!(
            PreviewSlot::Deck("a".to_string()).key(),
            PreviewSlot::Deck("b".to_string()).key()
        );
    }
}
