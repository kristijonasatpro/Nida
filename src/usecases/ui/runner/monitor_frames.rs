//! The Main and Preview monitors, one show frame each control-window frame.
//!
//! The engine draws on the refresh of the screen the show goes to; the
//! control window on its own screen's. At 60 Hz both, they still drift
//! apart: reading "the newest frame" when the two refreshes fell close
//! together, the window got frame N, then N again, then N + 2 — the Main
//! monitor stuttered while the outputs were smooth (Max5 on the Studio: the
//! frame clock on the LED screen, the window on the 4K one).
//!
//! So the engine copies each frame's monitored pictures, small, into a few
//! pictures in turn, numbered ([`Frames`]); the window steps through them
//! one a frame, a frame behind ([`step`]), whichever way the refreshes fall
//! — copying the one it picks into a picture of its own while it holds the
//! engine ([`Picker`]): drawn later, the frame itself could have been
//! written over by then.

use std::sync::{Arc, Mutex, PoisonError};

use super::preview::PreviewEncoder;

/// Pictures each monitor's frames go into, in turn: the window picks the
/// newest or the one before, and copies it before the engine goes on.
pub(super) const SLOTS: u64 = 4;

/// The monitors that step through frames, by their place in [`Shared`].
pub(super) const MAIN: usize = 0;
pub(super) const PREVIEW: usize = 1;

/// What the window wants of a monitor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Want {
    /// The input shown, by UUID (Preview); `None` for the master.
    pub channel: Option<String>,
    /// The largest it was drawn at, in pixels.
    pub drawn: Option<[u32; 2]>,
}

/// A monitor's frames, as the engine leaves them.
pub(super) struct Frames {
    /// The pictures, all one size; frame `n` is in `targets[n % SLOTS]`.
    pub targets: Vec<(wgpu::Texture, wgpu::TextureView)>,
    /// Counted up each time the pictures are made anew (a new size).
    pub generation: u64,
    /// The newest frame copied.
    pub newest: Option<u64>,
    /// What the frames show: the input's UUID, or none for the master.
    pub source: Option<String>,
    /// The first frame of `source` (frames before it showed something else).
    pub first_of_source: u64,
}

/// Between the engine and the window.
#[derive(Default)]
pub(super) struct Shared {
    pub wants: [Option<Want>; 2],
    pub frames: [Option<Frames>; 2],
    generations: u64,
}

pub(super) type MonitorShare = Arc<Mutex<Shared>>;

pub(super) fn lock(share: &MonitorShare) -> std::sync::MutexGuard<'_, Shared> {
    share.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The frame the window shows next, having shown `shown`, with frames
/// `oldest..=newest` there: the one after it — never back, never two on —
/// so each refresh of the window shows the next frame whichever side of the
/// engine's refresh it fell. The newest again when there is none newer (a
/// window faster than the show); one before the newest when it has fallen
/// two behind (slower), or on starting.
pub(super) fn step(shown: Option<u64>, oldest: u64, newest: u64) -> u64 {
    let behind = newest.saturating_sub(1).max(oldest);
    match shown {
        Some(shown) if shown >= oldest && shown <= newest => {
            let next = shown + 1;
            if next > newest {
                shown
            } else if newest - next >= 2 {
                behind
            } else {
                next
            }
        }
        _ => behind,
    }
}

/// The frames still to be had of `frames`, oldest first: those of its
/// source not yet written over (two slots kept clear of the engine).
pub(super) fn available(frames: &Frames) -> Option<(u64, u64)> {
    let newest = frames.newest?;
    let oldest = newest
        .saturating_sub(SLOTS - 2)
        .max(frames.first_of_source);
    Some((oldest, newest))
}

/// On the engine thread, after the mixer: copy each wanted monitor's
/// picture into its next frame. `source` gives a monitor's picture — the
/// master, or the input of that UUID — and its size.
pub(super) fn capture<'a>(
    share: &MonitorShare,
    encoder: &PreviewEncoder,
    context: &crate::renderer::GpuContext,
    source: impl Fn(Option<&str>) -> Option<(&'a wgpu::TextureView, u32, u32)>,
) {
    let mut pairs: Vec<(wgpu::TextureView, wgpu::Texture, wgpu::TextureView)> = Vec::new();
    let mut written: Vec<(usize, u64)> = Vec::new();
    {
        let mut shared = lock(share);
        for at in 0..2 {
            let Some(want) = shared.wants[at].clone() else {
                shared.frames[at] = None;
                continue;
            };
            let Some((view, w, h)) = source(want.channel.as_deref()) else {
                shared.frames[at] = None;
                continue;
            };
            let current = shared.frames[at]
                .as_ref()
                .and_then(|f| f.targets.first())
                .map(|(t, _)| [t.width(), t.height()]);
            let [tw, th] = PreviewEncoder::target_size(
                PreviewEncoder::preview_size(w, h),
                want.drawn,
                current,
            );
            if current != Some([tw, th]) {
                shared.generations += 1;
                let generation = shared.generations;
                shared.frames[at] = Some(Frames {
                    targets: (0..SLOTS)
                        .map(|_| PreviewEncoder::new_target(&context.device, tw, th))
                        .collect(),
                    generation,
                    newest: None,
                    source: want.channel.clone(),
                    first_of_source: 0,
                });
            }
            let Some(frames) = shared.frames[at].as_mut() else {
                continue;
            };
            let seq = frames.newest.map_or(0, |n| n + 1);
            if frames.source != want.channel || frames.newest.is_none() {
                frames.source.clone_from(&want.channel);
                frames.first_of_source = seq;
            }
            #[allow(clippy::cast_possible_truncation)]
            let (target, target_view) = &frames.targets[(seq % SLOTS) as usize];
            pairs.push((view.clone(), target.clone(), target_view.clone()));
            written.push((at, seq));
        }
    }
    if pairs.is_empty() {
        return;
    }
    let borrowed: Vec<_> = pairs.iter().map(|(s, t, v)| (s, t, v)).collect();
    encoder.encode_into(context, &borrowed);
    // Shown from here: submitted, so a window's draw of it comes after.
    let mut shared = lock(share);
    for (at, seq) in written {
        if let Some(frames) = shared.frames[at].as_mut() {
            frames.newest = Some(seq);
        }
    }
}

/// The window's side: the frame each monitor showed, and its own copy of
/// it, registered with egui.
#[derive(Default)]
pub(super) struct Picker {
    shown: [Option<(u64, u64)>; 2],
    copies: [Option<(wgpu::Texture, egui::TextureId)>; 2],
    steps: [crate::engine::MonitorSteps; 2],
}

impl Picker {
    /// How each monitor has stepped through the frames (for the API).
    pub(super) fn stats(&self) -> crate::engine::ControlMonitorsStats {
        crate::engine::ControlMonitorsStats {
            main: self.steps[MAIN],
            preview: self.steps[PREVIEW],
        }
    }

    /// Monitor `at`'s next frame of `frames`, copied into its own picture —
    /// submitted now, while the engine is held, so the engine's later
    /// writes come after it — and that picture's texture.
    pub(super) fn pick(
        &mut self,
        at: usize,
        frames: &Frames,
        gpu: &crate::renderer::GpuContext,
        egui_renderer: &mut egui_wgpu::Renderer,
    ) -> Option<egui::TextureId> {
        let (oldest, newest) = available(frames)?;
        // Frames of another generation are numbered afresh.
        let shown = self.shown[at]
            .filter(|(generation, _)| *generation == frames.generation)
            .map(|(_, n)| n);
        let pick = step(shown, oldest, newest);
        self.shown[at] = Some((frames.generation, pick));
        let steps = &mut self.steps[at];
        steps.shown += 1;
        match shown {
            Some(before) if pick == before => steps.same_as_last += 1,
            Some(before) if pick > before + 1 => steps.skipped += 1,
            _ => {}
        }
        #[allow(clippy::cast_possible_truncation)]
        let (source, _) = frames.targets.get((pick % SLOTS) as usize)?;
        let size = wgpu::Extent3d {
            width: source.width(),
            height: source.height(),
            depth_or_array_layers: 1,
        };
        let fits = self.copies[at]
            .as_ref()
            .is_some_and(|(t, _)| t.width() == size.width && t.height() == size.height);
        if !fits {
            if let Some((_, old)) = self.copies[at].take() {
                egui_renderer.free_texture(&old);
            }
            let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Monitor Shown"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                // (Read back by the tests.)
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let id = egui_renderer.register_native_texture(&gpu.device, &view, wgpu::FilterMode::Linear);
            self.copies[at] = Some((texture, id));
        }
        let (copy, id) = self.copies[at].as_ref()?;
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("Monitor Shown") });
        encoder.copy_texture_to_texture(source.as_image_copy(), copy.as_image_copy(), size);
        gpu.submit(std::iter::once(encoder.finish()));
        Some(*id)
    }
}

#[cfg(test)]
mod tests {
    use super::step;

    /// A 16×16 picture of one colour, as the engine's frames are made, and
    /// writable here.
    fn frame_of(gpu: &crate::renderer::GpuContext, rgba: [u8; 4]) -> (wgpu::Texture, wgpu::TextureView) {
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: 16, height: 16, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        paint(gpu, &texture, rgba);
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        (texture, view)
    }

    fn paint(gpu: &crate::renderer::GpuContext, texture: &wgpu::Texture, rgba: [u8; 4]) {
        let pixels: Vec<u8> = rgba.iter().copied().cycle().take(16 * 16 * 4).collect();
        gpu.queue.write_texture(
            texture.as_image_copy(),
            &pixels,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(64), rows_per_image: Some(16) },
            texture.size(),
        );
    }

    /// The first pixel of `texture`.
    fn first_pixel(gpu: &crate::renderer::GpuContext, texture: &wgpu::Texture) -> [u8; 4] {
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256 * 16,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(16),
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
        [data[0], data[1], data[2], data[3]]
    }

    /// The window shows its own copy of the frame it picked: the frame one
    /// behind the newest, stepped one a call; written over by the engine
    /// after the pick, the copy keeps what was picked.
    #[test]
    fn the_window_shows_its_own_copy_of_the_frame_it_picked() {
        let gpu = crate::renderer::GpuContext::new_headless().expect("a GPU");
        let mut renderer = egui_wgpu::Renderer::new(
            &gpu.device,
            wgpu::TextureFormat::Bgra8Unorm,
            egui_wgpu::RendererOptions::default(),
        );
        let colour = |n: u8| [n * 40, 0, 0, 255];
        let mut frames = super::Frames {
            targets: (0..4).map(|n| frame_of(&gpu, colour(n))).collect(),
            generation: 1,
            newest: Some(5),
            source: None,
            first_of_source: 0,
        };
        let mut picker = super::Picker::default();
        let id = picker.pick(super::MAIN, &frames, &gpu, &mut renderer).expect("a frame");
        let copy = &picker.copies[super::MAIN].as_ref().expect("its copy").0;
        // Frame 4, one behind the newest: slot 0.
        assert_eq!(first_pixel(&gpu, copy), colour(0));
        // The engine writes over slot 0 (frame 8, say): the copy stays.
        paint(&gpu, &frames.targets[0].0, [9, 9, 9, 255]);
        assert_eq!(first_pixel(&gpu, copy), colour(0), "the picked frame kept");
        // The next call steps to frame 5 (slot 1), into the same picture.
        frames.newest = Some(6);
        let again = picker.pick(super::MAIN, &frames, &gpu, &mut renderer).expect("a frame");
        assert_eq!(again, id, "one picture, registered once");
        let copy = &picker.copies[super::MAIN].as_ref().expect("its copy").0;
        assert_eq!(first_pixel(&gpu, copy), colour(1));
        // Frames 4, 5 so far; then 6, then 6 again (nothing newer), then
        // three newer at once: a skip to 8.
        for newest in [6, 6, 9] {
            frames.newest = Some(newest);
            let _ = picker.pick(super::MAIN, &frames, &gpu, &mut renderer);
        }
        let steps = picker.stats().main;
        assert_eq!((steps.shown, steps.same_as_last, steps.skipped), (5, 1, 1));
    }

    /// The engine copies each wanted monitor into its next picture, in
    /// turn, sized to what the window drew; a new cued input starts its
    /// frames afresh; a monitor not wanted is let go.
    #[test]
    fn the_engine_copies_each_wanted_monitor_into_its_next_frame() {
        let gpu = crate::renderer::GpuContext::new_headless().expect("a GPU");
        let encoder = super::PreviewEncoder::new(&gpu.device);
        let (_source, view) = super::PreviewEncoder::new_target(&gpu.device, 1920, 1080);
        let share = super::MonitorShare::default();
        let source = |_: Option<&str>| Some((&view, 1920_u32, 1080_u32));
        super::lock(&share).wants = [
            Some(super::Want { channel: None, drawn: Some([600, 338]) }),
            Some(super::Want { channel: Some("ch-a".into()), drawn: Some([600, 338]) }),
        ];
        for _ in 0..6 {
            super::capture(&share, &encoder, &gpu, source);
        }
        {
            let shared = super::lock(&share);
            let main = shared.frames[super::MAIN].as_ref().expect("Main's frames");
            assert_eq!(main.newest, Some(5));
            assert_eq!(main.targets.len(), 4);
            let (w, h) = (main.targets[0].0.width(), main.targets[0].0.height());
            assert!((600..1920).contains(&w) && h >= 338, "{w}×{h}: sized to what was drawn");
            assert_eq!(super::available(main), Some((3, 5)), "two kept clear of the engine");
        }
        super::lock(&share).wants[super::PREVIEW] =
            Some(super::Want { channel: Some("ch-b".into()), drawn: Some([600, 338]) });
        super::capture(&share, &encoder, &gpu, source);
        {
            let shared = super::lock(&share);
            let preview = shared.frames[super::PREVIEW].as_ref().expect("Preview's frames");
            assert_eq!(preview.source.as_deref(), Some("ch-b"));
            assert_eq!(super::available(preview), Some((6, 6)), "only the new input's frames");
        }
        super::lock(&share).wants[super::MAIN] = None;
        super::capture(&share, &encoder, &gpu, source);
        assert!(super::lock(&share).frames[super::MAIN].is_none(), "not wanted: let go");
    }

    /// The window steps one frame each refresh whichever side of the
    /// engine's refresh it falls: with the newest flapping between N and
    /// N + 1 as the two refreshes cross, what is shown still goes 1, 2, 3…
    #[test]
    fn the_window_steps_one_frame_a_refresh_as_the_refreshes_cross() {
        // Each window refresh j sees the newest at j + 5, or j + 4 when it
        // fell just before the engine's.
        let mut shown = None;
        let mut seen = Vec::new();
        for j in 0_u64..12 {
            let newest = if j % 3 == 1 { j + 4 } else { j + 5 };
            let pick = step(shown, newest.saturating_sub(2), newest);
            seen.push(pick);
            shown = Some(pick);
        }
        for pair in seen.windows(2) {
            assert_eq!(pair[1], pair[0] + 1, "{seen:?}");
        }
    }

    /// A window twice as fast as the show shows each frame twice, evenly;
    /// one half as fast, every other frame; it starts a frame behind and
    /// catches up when it falls two behind.
    #[test]
    fn faster_and_slower_windows_keep_an_even_step() {
        // 120 Hz window, 60 fps show: the newest goes up every other refresh.
        let mut shown = None;
        let mut seen = Vec::new();
        for j in 0_u64..10 {
            let newest = 10 + j / 2;
            let pick = step(shown, newest.saturating_sub(2), newest);
            seen.push(pick);
            shown = Some(pick);
        }
        let repeats = seen.windows(2).filter(|p| p[0] == p[1]).count();
        assert!(seen.windows(2).all(|p| p[1] - p[0] <= 1), "{seen:?}");
        assert!((4..=5).contains(&repeats), "{seen:?}");
        // 30 Hz window, 60 fps show.
        let mut shown = None;
        let mut seen = Vec::new();
        for j in 0_u64..8 {
            let newest = 10 + 2 * j;
            let pick = step(shown, newest.saturating_sub(2), newest);
            seen.push(pick);
            shown = Some(pick);
        }
        assert!(seen.windows(2).all(|p| p[1] - p[0] == 2), "{seen:?}");
        assert_eq!(step(None, 3, 9), 8, "starts a frame behind");
        assert_eq!(step(Some(2), 3, 9), 8, "written over: back to a frame behind");
        assert_eq!(step(Some(9), 7, 9), 9, "nothing newer: the same");
    }
}
