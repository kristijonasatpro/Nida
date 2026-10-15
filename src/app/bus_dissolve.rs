//! The fade a bus shows on air while an `AUTO` runs.
//!
//! Each dissolving bus renders into a target of its own at the master's size,
//! and every destination following that bus reads the result — so the fade is
//! computed once per bus, not once per screen. A target lives only as long as
//! its bus is dissolving.

use std::collections::HashMap;

use super::{BusFeed, FeedPicture};
use crate::engine::value::render::OutputSource;
use crate::renderer::context::GpuContext;
use crate::renderer::dissolve::DissolvePipeline;

struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    params: wgpu::Buffer,
}

#[derive(Default)]
pub(crate) struct BusDissolves {
    /// Built on first use, so a show that never fades never compiles it.
    pipeline: Option<DissolvePipeline>,
    targets: HashMap<String, Target>,
}

impl BusDissolves {
    /// Render every dissolving feed and swap it for the rendered picture.
    ///
    /// `resolve` maps a concrete source to the view it reads from; `black`
    /// stands in for a `None` end. Targets of buses no longer dissolving are
    /// released.
    pub(crate) fn render<'a>(
        &mut self,
        context: &GpuContext,
        (width, height): (u32, u32),
        feeds: &mut HashMap<String, BusFeed>,
        resolve: impl Fn(&OutputSource) -> Option<&'a wgpu::TextureView>,
        black: &'a wgpu::TextureView,
    ) {
        self.targets.retain(|uuid, _| {
            feeds
                .get(uuid)
                .is_some_and(|f| matches!(f.picture, FeedPicture::Dissolve { .. }))
        });
        if !feeds
            .values()
            .any(|f| matches!(f.picture, FeedPicture::Dissolve { .. }))
        {
            return;
        }

        let pipeline = self.pipeline.get_or_insert_with(|| {
            DissolvePipeline::new(&context.device, context.compositing_format)
        });
        let mut encoder = context
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Bus Dissolve Encoder"),
            });
        for (uuid, feed) in feeds.iter_mut() {
            let FeedPicture::Dissolve { from, to, mix } = &feed.picture else {
                continue;
            };
            let from_view = from.as_ref().and_then(&resolve).unwrap_or(black);
            let to_view = to.as_ref().and_then(&resolve).unwrap_or(black);

            let stale = self.targets.get(uuid).is_none_or(|t| {
                t.texture.width() != width.max(1) || t.texture.height() != height.max(1)
            });
            if stale {
                let texture = context.create_compositing_texture(width.max(1), height.max(1));
                let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                let params = pipeline.params_buffer(&context.device);
                self.targets.insert(
                    uuid.clone(),
                    Target {
                        texture,
                        view,
                        params,
                    },
                );
            }
            let target = &self.targets[uuid];
            pipeline.render(
                &context.device,
                &context.queue,
                &mut encoder,
                from_view,
                to_view,
                *mix,
                &target.params,
                &target.view,
            );
            feed.picture = FeedPicture::Rendered(target.view.clone());
        }
        context.submit(Some(encoder.finish()));
    }
}
