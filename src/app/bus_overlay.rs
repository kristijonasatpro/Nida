//! Lower thirds and text inputs shown on chosen buses, over whatever each
//! bus shows.
//!
//! A bus with a lower third coming in, on, or going out gets a target of its
//! own at the master's size for the frame: its picture copied in (as a
//! dissolve leaves it, mid-`AUTO`), the lower third drawn over it in the
//! bus's names area, laid out for that area's shape. Every destination
//! following the bus — screens, LED walls, Blackmagic, the stream — then
//! reads that target, at the bus's own region of it as before. A bus with
//! none live reads its source as ever: no cost, nothing changed.

use std::collections::HashMap;

use super::{BusFeed, FeedPicture};
use crate::deck::lower_third::LowerThird;
use crate::deck::text::Text;
use crate::engine::value::render::OutputSource;
use crate::renderer::blit::BlitPipeline;
use crate::renderer::context::GpuContext;

struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

/// A names area: x, y, width, height in master pixels.
pub(crate) type Area = (u32, u32, u32, u32);

/// What is drawn over a bus's picture, in an area of it: a lower third, a
/// text input sent to the bus.
pub(crate) trait OverBus {
    /// Drawn in `pass`, in `area`, laid out for it; true when anything of it
    /// was.
    fn render_over(&self, pass: &mut wgpu::RenderPass<'_>, area: Area) -> bool;
}

impl OverBus for LowerThird {
    fn render_over(&self, pass: &mut wgpu::RenderPass<'_>, area: Area) -> bool {
        LowerThird::render_over(self, pass, area)
    }
}

impl OverBus for Text {
    fn render_over(&self, pass: &mut wgpu::RenderPass<'_>, area: Area) -> bool {
        Text::render_over(self, pass, area)
    }
}

/// A bus with something to draw over it this frame: its UUID, its region of
/// the master (the UV scale and offset of a feed of it), and each thing to
/// draw with the area it goes in, in the order drawn.
pub(crate) struct BusOverlay<'a> {
    pub bus: String,
    pub region: ([f32; 2], [f32; 2]),
    pub over: Vec<(&'a dyn OverBus, Area)>,
}

#[derive(Default)]
pub(crate) struct BusOverlays {
    /// Built on first use, so a show that never sends a lower third to a
    /// bus never compiles it.
    copy: Option<BlitPipeline>,
    targets: HashMap<String, Target>,
}

impl BusOverlays {
    /// Draw each bus's lower thirds over its picture and swap its feed for
    /// the result. `resolve` maps a concrete source to the view it reads
    /// from; `black` is a bus with nothing on air. Targets of buses with no
    /// lower third live are released.
    pub(crate) fn render<'a>(
        &mut self,
        context: &GpuContext,
        (width, height): (u32, u32),
        feeds: &mut HashMap<String, BusFeed>,
        resolve: impl Fn(&OutputSource) -> Option<&'a wgpu::TextureView>,
        black: &'a wgpu::TextureView,
        overlays: &[BusOverlay<'_>],
    ) {
        self.targets.retain(|uuid, _| {
            overlays
                .iter()
                .any(|o| o.bus == *uuid && !o.over.is_empty())
        });
        if overlays.iter().all(|o| o.over.is_empty()) {
            return;
        }
        let copy = match &mut self.copy {
            Some(copy) => copy,
            None => match BlitPipeline::new(&context.device, context.compositing_format) {
                Ok(copy) => self.copy.insert(copy),
                Err(e) => {
                    log::warn!("Lower thirds on buses not drawn: {e}");
                    return;
                }
            },
        };
        let live: Vec<&BusOverlay<'_>> = overlays.iter().filter(|o| !o.over.is_empty()).collect();
        copy.ensure_ring_slots(&context.device, live.len());
        let mut encoder = context
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Bus Overlay Encoder"),
            });
        for (slot, overlay) in live.into_iter().enumerate() {
            // What the bus shows now: its source, the dissolve already
            // rendered for it, or black.
            let base: Option<wgpu::TextureView> = match feeds.get(&overlay.bus).map(|f| &f.picture)
            {
                Some(FeedPicture::Source(source)) => resolve(source).cloned(),
                Some(FeedPicture::Rendered(view)) => Some(view.clone()),
                Some(FeedPicture::Dissolve { .. }) | None => None,
            };
            let base = base.unwrap_or_else(|| black.clone());
            // A deck holds straight colour; composites, the master and a
            // dissolve premultiplied — as the lower third is blended over.
            let straight = matches!(
                feeds.get(&overlay.bus).map(|f| &f.picture),
                Some(FeedPicture::Source(OutputSource::Deck(..)))
            );
            let stale = self.targets.get(&overlay.bus).is_none_or(|t| {
                t.texture.width() != width.max(1) || t.texture.height() != height.max(1)
            });
            if stale {
                let texture = context.create_compositing_texture(width.max(1), height.max(1));
                let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                self.targets
                    .insert(overlay.bus.clone(), Target { texture, view });
            }
            let target = &self.targets[&overlay.bus];
            {
                copy.write_params_slot_mode(
                    &context.queue,
                    slot,
                    1.0,
                    [1.0, 1.0],
                    [0.0, 0.0],
                    if straight { 2 } else { 0 },
                );
                let bind = copy.create_ring_bind_group(&context.device, &base, slot);
                // The copy and the lower thirds over it in one pass: the
                // bus's whole picture written once, not written then read
                // back and written again.
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Bus Overlay"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &target.view,
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
                copy.render(&mut pass, &bind);
                for (over, area) in &overlay.over {
                    over.render_over(&mut pass, *area);
                }
            }
            let (uv_scale, uv_offset) = feeds
                .get(&overlay.bus)
                .map_or(overlay.region, |f| (f.uv_scale, f.uv_offset));
            feeds.insert(
                overlay.bus.clone(),
                BusFeed {
                    picture: FeedPicture::Rendered(target.view.clone()),
                    uv_scale,
                    uv_offset,
                },
            );
        }
        context.submit(Some(encoder.finish()));
    }
}
