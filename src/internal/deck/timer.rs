//! A timer layer: a count down, a count to a time of day, a count up or the
//! time of day, as numbers over clear (see [`crate::engine::value::timer`]).
//!
//! The numbers are drawn by the SVG renderer — any font installed, an
//! outline, a box behind — on a thread of the layer's own, each time what
//! they read changes, about once a second. No frame waits for text: a frame
//! places the last drawing where the look puts it, at its own pixel size,
//! so it stays sharp.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Instant;

use crate::engine::value::timer::{
    AtZero, Reading, TimerAction, TimerLook, TimerMode, TimerSettings, TimerSnapshot,
};
use crate::renderer::blit::BlitPipeline;
use crate::renderer::GpuContext;

/// Time counted while running, kept across pauses.
#[derive(Debug, Clone, Copy, Default)]
struct Stopwatch {
    banked: f64,
    since: Option<Instant>,
}

impl Stopwatch {
    fn elapsed_at(&self, now: Instant) -> f64 {
        self.banked
            + self
                .since
                .map_or(0.0, |since| now.saturating_duration_since(since).as_secs_f64())
    }

    fn running(&self) -> bool {
        self.since.is_some()
    }

    fn start(&mut self, now: Instant) {
        self.since.get_or_insert(now);
    }

    fn pause(&mut self, now: Instant) {
        self.banked = self.elapsed_at(now);
        self.since = None;
    }

    /// Run `elapsed` so far, running on if it was.
    fn set(&mut self, elapsed: f64, now: Instant) {
        self.banked = elapsed;
        if self.since.is_some() {
            self.since = Some(now);
        }
    }
}

/// What a drawing of the numbers is of: whatever changes it.
#[derive(Debug, Clone, PartialEq)]
struct FaceSpec {
    text: String,
    color: [u8; 3],
    look: TimerLook,
    frame_height: u32,
}

/// The numbers drawn, on the GPU.
struct Face {
    size: (u32, u32),
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

type Painted = (FaceSpec, Option<image::RgbaImage>);

/// The thread the numbers are drawn on.
struct Painter {
    jobs: Option<mpsc::Sender<FaceSpec>>,
    done: mpsc::Receiver<Painted>,
    /// Drawn here, for want of a thread.
    done_here: Option<Painted>,
}

impl Painter {
    fn start() -> Self {
        let (jobs_tx, jobs_rx) = mpsc::channel::<FaceSpec>();
        let (done_tx, done) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("timer-paint".into())
            .spawn(move || {
                let fonts = super::svg::system_fonts();
                let mut measures = Measures::default();
                while let Ok(mut spec) = jobs_rx.recv() {
                    // Only the latest is worth drawing.
                    while let Ok(newer) = jobs_rx.try_recv() {
                        spec = newer;
                    }
                    let face = paint_with(&spec, &fonts, &mut measures);
                    if done_tx.send((spec, face)).is_err() {
                        break;
                    }
                }
            });
        let jobs = match spawned {
            Ok(_) => Some(jobs_tx),
            Err(e) => {
                log::warn!("Timer drawing thread not started, drawing in the frame: {e}");
                None
            }
        };
        Self {
            jobs,
            done,
            done_here: None,
        }
    }

    fn ask(&mut self, spec: FaceSpec) {
        let unsent = match &self.jobs {
            Some(jobs) => jobs.send(spec).err().map(|mpsc::SendError(spec)| spec),
            None => Some(spec),
        };
        if let Some(spec) = unsent {
            let face = paint_with(&spec, &super::svg::system_fonts(), &mut Measures::default());
            self.done_here = Some((spec, face));
        }
    }

    /// The latest drawing finished since the last look.
    fn take(&mut self) -> Option<Painted> {
        let mut latest = self.done_here.take();
        while let Ok(painted) = self.done.try_recv() {
            latest = Some(painted);
        }
        latest
    }
}

pub struct Timer {
    context: GpuContext,
    settings: TimerSettings,
    /// How this run counts, as a rundown step started it — down from a
    /// length of its own, or up — over `settings` till they are set by hand:
    /// not saved with the show, nor put back by an undo.
    run: Option<TimerSettings>,
    /// The master's size, which the numbers are sized and placed against.
    frame: (u32, u32),
    watch: Stopwatch,
    blit: BlitPipeline,
    face: Option<Face>,
    /// What the painter was last asked to draw.
    asked: Option<FaceSpec>,
    painter: Painter,
}

impl Timer {
    /// A timer that has run `elapsed` seconds (a count down or up), paused.
    ///
    /// # Errors
    ///
    /// Returns an error if the pipeline that places the numbers cannot be made.
    pub fn new(
        context: &GpuContext,
        settings: TimerSettings,
        elapsed: f64,
        frame: (u32, u32),
    ) -> anyhow::Result<Self> {
        Ok(Self {
            context: context.clone(),
            settings: settings.clamped(),
            run: None,
            frame,
            watch: Stopwatch {
                banked: if elapsed.is_finite() { elapsed } else { 0.0 },
                since: None,
            },
            blit: BlitPipeline::new(&context.device, context.compositing_format)?,
            face: None,
            asked: None,
            painter: Painter::start(),
        })
    }

    /// Its settings, as saved with the show.
    pub fn settings(&self) -> &TimerSettings {
        &self.settings
    }

    /// How it counts now: as a rundown step started this run, else as set.
    fn current(&self) -> &TimerSettings {
        self.run.as_ref().unwrap_or(&self.settings)
    }

    /// What it shows now: as a rundown step started this run, else as set.
    pub fn mode(&self) -> TimerMode {
        self.current().mode
    }

    /// New settings; the time run so far stays — but a count down stopped
    /// on zero and given a new length starts again from its top, rather
    /// than showing what the new length leaves after the old. A step's way
    /// of counting this run gives way to them.
    pub fn set_settings(&mut self, settings: TimerSettings) {
        let now = Instant::now();
        self.settle(now);
        let done = !self.watch.running() && self.at_zero(now);
        let old_length = self.current().duration_secs;
        self.settings = settings.clamped();
        self.run = None;
        if done && self.settings.duration_secs != old_length {
            self.watch = Stopwatch::default();
        }
        self.settle(now);
    }

    /// Settings put back by an undo or a show's history: as
    /// [`Self::set_settings`], but a step's way of counting this run — its
    /// count down or up — stays, the rest taken from them: undoing a colour
    /// does not stop a talk's count.
    pub fn restore_settings(&mut self, settings: TimerSettings) {
        let Some(run) = self.run.clone() else {
            self.set_settings(settings);
            return;
        };
        // Settled by the run's own count throughout, never the saved one's:
        // a count of 900 s at 400 s is not stopped by a saved 300.
        let now = Instant::now();
        self.settle(now);
        self.settings = settings.clamped();
        self.run = Some(TimerSettings {
            mode: run.mode,
            duration_secs: run.duration_secs,
            ..self.settings.clone()
        });
        self.settle(now);
    }

    /// Count this run as `settings` say — a rundown step's count down of a
    /// length of its own, or count up — leaving the saved settings as they
    /// are: an undo, or the show saved, does not take them up.
    pub fn run_as(&mut self, settings: TimerSettings) {
        let now = Instant::now();
        self.settle(now);
        self.run = Some(settings.clamped());
        self.settle(now);
    }

    /// Start, pause, reset or adjust it. The time of day and a count to a
    /// time run by the clock and take none of these.
    pub fn control(&mut self, action: TimerAction) {
        if !self.current().mode.has_transport() {
            return;
        }
        let now = Instant::now();
        self.settle(now);
        match action {
            TimerAction::Start => {
                if self.at_zero(now) {
                    self.watch = Stopwatch::default();
                }
                self.watch.start(now);
            }
            TimerAction::Pause => self.watch.pause(now),
            TimerAction::Reset => self.watch = Stopwatch::default(),
            TimerAction::Adjust { secs } => {
                #[allow(clippy::cast_precision_loss)]
                let secs = secs as f64;
                let elapsed = self.watch.elapsed_at(now);
                let elapsed = match self.current().mode {
                    // More time left is less time run.
                    TimerMode::Countdown => elapsed - secs,
                    _ => (elapsed + secs).max(0.0),
                };
                self.watch.set(elapsed, now);
                self.settle(now);
            }
        }
    }

    /// A count down stopping on zero stops there, rather than running on
    /// unseen — which a longer length set later would show.
    fn settle(&mut self, now: Instant) {
        let current = self.current();
        let length = f64::from(current.duration_secs);
        if current.mode == TimerMode::Countdown
            && current.at_zero == AtZero::Stop
            && self.watch.running()
            && self.watch.elapsed_at(now) >= length
        {
            self.watch = Stopwatch {
                banked: length,
                since: None,
            };
        }
    }

    /// A count down stopped on zero.
    fn at_zero(&self, now: Instant) -> bool {
        let current = self.current();
        current.mode == TimerMode::Countdown
            && self.watch.elapsed_at(now) >= f64::from(current.duration_secs)
    }

    /// Counting: a count down or up started and not paused, or a clock.
    pub fn running(&self) -> bool {
        !self.current().mode.has_transport() || self.watch.running()
    }

    /// How long a count down or up has run, in seconds.
    pub fn elapsed(&self) -> f64 {
        self.watch.elapsed_at(Instant::now())
    }

    fn reading(&self, now: Instant) -> Reading {
        self.current()
            .reading(self.watch.elapsed_at(now), time_of_day())
    }

    /// The state for the UI and the API; `panel` is its control panel's
    /// number.
    pub fn snapshot(&self, panel: u64) -> TimerSnapshot {
        let now = Instant::now();
        let reading = self.reading(now);
        TimerSnapshot {
            // As it counts now: a panel changing it from there sets it so.
            settings: self.current().clone(),
            running: self.running(),
            text: reading.text,
            state: reading.state,
            remaining: reading.remaining,
            elapsed: self.watch.elapsed_at(now),
            panel,
        }
    }

    /// Each frame: ask for the numbers to be drawn again if what they read
    /// has changed, and take a drawing that is ready.
    pub fn tick(&mut self) {
        let now = Instant::now();
        self.settle(now);
        let reading = self.reading(now);
        let current = self.current();
        let spec = FaceSpec {
            color: current.color(reading.state),
            text: reading.text,
            look: current.look.clone(),
            frame_height: self.frame.1,
        };
        if self.asked.as_ref() != Some(&spec) {
            self.painter.ask(spec.clone());
            self.asked = Some(spec);
        }
        if let Some((_, image)) = self.painter.take() {
            self.show(image.as_ref());
        }
    }

    /// Put a drawing on the GPU, into the texture there when it fits.
    fn show(&mut self, image: Option<&image::RgbaImage>) {
        let Some(image) = image else {
            self.face = None;
            return;
        };
        let size = image.dimensions();
        match &self.face {
            Some(face) if face.size == size => {
                self.context.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &face.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    image,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(4 * size.0),
                        rows_per_image: Some(size.1),
                    },
                    wgpu::Extent3d {
                        width: size.0,
                        height: size.1,
                        depth_or_array_layers: 1,
                    },
                );
            }
            _ => {
                let (texture, view) = super::source::upload_image_texture(&self.context, image);
                let bind_group = self.blit.create_bind_group(&self.context.device, &view);
                self.face = Some(Face {
                    size,
                    texture,
                    bind_group,
                });
            }
        }
    }

    /// Draw the frame: clear, and the numbers where the look puts them.
    pub fn render(&self, cmd_buffers: &mut Vec<wgpu::CommandBuffer>, output: &wgpu::TextureView) {
        let mut encoder =
            self.context
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Timer Encoder"),
                });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Timer Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: output,
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
            if let Some(face) = &self.face {
                let (scale, offset) = self.placement(face.size);
                self.blit
                    .set_uv_transform(&self.context.queue, 1.0, scale, offset);
                self.blit.render(&mut pass, &face.bind_group);
            }
        }
        cmd_buffers.push(encoder.finish());
    }

    /// The UV transform that puts a `size` drawing at its place on the
    /// frame, at its own size.
    #[allow(clippy::cast_precision_loss)]
    fn placement(&self, size: (u32, u32)) -> ([f32; 2], [f32; 2]) {
        let frame = (self.frame.0.max(1) as f32, self.frame.1.max(1) as f32);
        let size = (size.0.max(1) as f32, size.1.max(1) as f32);
        let look = &self.current().look;
        let (x, y) = look.anchor.place(size, frame, look.margin * frame.1);
        (
            [frame.0 / size.0, frame.1 / size.1],
            [-x / size.0, -y / size.1],
        )
    }

    /// The master's size changed: the numbers are drawn again to suit.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.frame = (width.max(1), height.max(1));
    }

    /// Wait (a while) for the painter's drawing of what the timer reads now,
    /// for tests: true once it is on the GPU.
    pub fn wait_drawn(&mut self, timeout: std::time::Duration) -> bool {
        let until = Instant::now() + timeout;
        self.tick();
        while Instant::now() < until {
            if let Ok(painted) = self.painter.done.recv_timeout(std::time::Duration::from_millis(20)) {
                let current = self.asked.as_ref() == Some(&painted.0);
                self.show(painted.1.as_ref());
                if current {
                    return self.face.is_some();
                }
            }
        }
        false
    }
}

/// Seconds after local midnight, now.
pub(crate) fn time_of_day() -> f64 {
    use chrono::Timelike as _;
    let now = chrono::Local::now().time();
    f64::from(now.num_seconds_from_midnight()) + f64::from(now.nanosecond().min(999_999_999)) / 1e9
}

// ── Drawing the numbers ─────────────────────────────────────────────

/// A caption's height, as a share of the numbers'.
const CAPTION_SIZE: f32 = 0.38;

/// Ink bounds of a line of text set at the origin, baseline at y = 0.
#[derive(Debug, Clone, Copy)]
struct Ink {
    left: f32,
    top: f32,
    width: f32,
    height: f32,
}

impl Ink {
    fn bottom(self) -> f32 {
        self.top + self.height
    }

    fn scaled(self, k: f32) -> Self {
        Self {
            left: self.left * k,
            top: self.top * k,
            width: self.width * k,
            height: self.height * k,
        }
    }
}

/// Measurements made, by text, size and font: a timer measures the same
/// few strings every second.
#[derive(Default)]
struct Measures(HashMap<(String, u32, String, bool, bool), Option<Ink>>);

impl Measures {
    fn get(
        &mut self,
        text: &str,
        size: f32,
        look: &TimerLook,
        fonts: &Arc<usvg::fontdb::Database>,
    ) -> Option<Ink> {
        let key = (
            text.to_string(),
            size.to_bits(),
            look.font.clone(),
            look.bold,
            look.italic,
        );
        if let Some(ink) = self.0.get(&key) {
            return *ink;
        }
        if self.0.len() > 256 {
            self.0.clear();
        }
        let ink = measure(text, size, look, fonts);
        self.0.insert(key, ink);
        ink
    }
}

/// The numbers of `spec`, drawn: straight-alpha RGBA, just big enough for
/// them and their box. `None` when there is nothing to draw.
#[cfg(test)]
fn paint(spec: &FaceSpec, fonts: &Arc<usvg::fontdb::Database>) -> Option<image::RgbaImage> {
    paint_with(spec, fonts, &mut Measures::default())
}

fn paint_with(
    spec: &FaceSpec,
    fonts: &Arc<usvg::fontdb::Database>,
    measures: &mut Measures,
) -> Option<image::RgbaImage> {
    let look = &spec.look;
    // The digits stand `size` of the frame high, whatever the font: all is
    // measured set at that size, then scaled by how high a 0 came out.
    #[allow(clippy::cast_precision_loss)]
    let tall = (look.size * spec.frame_height as f32).max(6.0);
    let scale = measures
        .get("0", tall, look, fonts)
        .map_or(1.0 / 0.72, |zero| tall / zero.height);
    let px = tall * scale;
    let mut at = |text: &str| {
        measures
            .get(text, tall, look, fonts)
            .map(|ink| ink.scaled(scale))
    };
    // Digits all measured as 0 place the numbers, so the box keeps its size
    // and the digits their places while they change; every digit measured
    // keeps a font's low-hanging 5s and 9s inside it.
    let probe: String = spec
        .text
        .chars()
        .map(|c| if c.is_ascii_digit() { '0' } else { c })
        .collect();
    let placed = at(&probe);
    let shown = at(&spec.text);
    let digits = at("0123456789");
    #[allow(clippy::cast_precision_loss)]
    let guess = Ink {
        left: 0.0,
        top: -tall,
        width: 0.45 * tall * probe.chars().count() as f32,
        height: tall,
    };
    // Digits' ink differs by a pixel or two at the ends, which the room
    // round them takes; a font whose digits differ in width can outgrow
    // the 0s by more.
    let across = match (placed, shown) {
        (Some(p), Some(s)) if s.width > p.width + 0.1 * tall => s,
        (Some(p), _) => p,
        (None, Some(s)) => s,
        (None, None) => guess,
    };
    let inks = [placed, shown, digits];
    let top = inks.iter().flatten().map(|i| i.top).fold(across.top, f32::min);
    let bottom = inks
        .iter()
        .flatten()
        .map(|i| i.bottom())
        .fold(across.bottom(), f32::max);
    let numbers = Ink {
        top,
        height: bottom - top,
        ..across
    };
    let caption_px = px * CAPTION_SIZE;
    let caption = (!look.caption.trim().is_empty())
        .then(|| measures.get(look.caption.trim(), caption_px, look, fonts))
        .flatten();
    let stroke = look.outline * tall;
    let boxed = look.box_opacity > 0.0;
    let pad = if boxed { tall * 0.4 } else { tall * 0.08 } + stroke + 2.0;
    let gap = tall * 0.25;
    let caption_block = caption.map_or(0.0, |c| c.height + gap);
    let width = numbers.width.max(caption.map_or(0.0, |c| c.width)) + 2.0 * pad;
    let height = caption_block + numbers.height + 2.0 * pad;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let (wide, high) = (width.ceil() as u32, height.ceil() as u32);
    if wide == 0 || high == 0 || wide > 8192 || high > 8192 {
        return None;
    }

    let mut svg = String::new();
    let _ = write!(
        svg,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{wide}" height="{high}" viewBox="0 0 {wide} {high}">"#
    );
    if boxed {
        let _ = write!(
            svg,
            r#"<rect width="{wide}" height="{high}" rx="{r}" fill="{fill}" fill-opacity="{a}"/>"#,
            r = tall * 0.22,
            fill = hex(look.box_color),
            a = look.box_opacity,
        );
    }
    let mut line = |text: &str, size: f32, ink: Ink, top: f32| {
        // Centred by its ink, not its advance: side bearings don't tilt it.
        let left = (width - ink.width) / 2.0 - ink.left;
        let baseline = top - ink.top;
        let _ = write!(
            svg,
            r#"<text x="{left}" y="{baseline}" {}>{}</text>"#,
            attributes(look, size, spec.color, stroke),
            escape(text)
        );
    };
    if let Some(ink) = caption {
        line(look.caption.trim(), caption_px, ink, pad);
    }
    line(&spec.text, px, numbers, pad + caption_block);
    svg.push_str("</svg>");

    let options = usvg::Options {
        fontdb: fonts.clone(),
        ..Default::default()
    };
    let tree = match usvg::Tree::from_str(&svg, &options) {
        Ok(tree) => tree,
        Err(e) => {
            log::warn!("Timer not drawn: {e}");
            return None;
        }
    };
    let mut pixmap = resvg::tiny_skia::Pixmap::new(wide, high)?;
    resvg::render(&tree, resvg::tiny_skia::Transform::identity(), &mut pixmap.as_mut());
    let mut rgba = image::RgbaImage::new(wide, high);
    for (dst, src) in rgba.pixels_mut().zip(pixmap.pixels()) {
        let c = src.demultiply();
        *dst = image::Rgba([c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Some(rgba)
}

/// The ink of `text` set at `size` in the look's font: `None` if no glyph
/// of it could be drawn.
fn measure(
    text: &str,
    size: f32,
    look: &TimerLook,
    fonts: &Arc<usvg::fontdb::Database>,
) -> Option<Ink> {
    let room = size * 4.0;
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}"><text x="0" y="{room}" {}>{}</text></svg>"#,
        attributes(look, size, [0, 0, 0], 0.0),
        escape(text),
        w = size * (text.chars().count() as f32 + 2.0) * 1.5,
        h = room * 2.0,
    );
    let options = usvg::Options {
        fontdb: fonts.clone(),
        ..Default::default()
    };
    let tree = usvg::Tree::from_str(&svg, &options).ok()?;
    let text_node = tree.root().children().iter().find_map(|node| match node {
        usvg::Node::Text(text) => Some(text),
        _ => None,
    })?;
    let glyphs = text_node.flattened();
    if !glyphs.has_children() {
        return None;
    }
    let bounds = glyphs.abs_bounding_box();
    (bounds.width() > 0.0 && bounds.height() > 0.0).then(|| Ink {
        left: bounds.left(),
        top: bounds.top() - room,
        width: bounds.width(),
        height: bounds.height(),
    })
}

/// A `<text>`'s font, fill and outline.
fn attributes(look: &TimerLook, size: f32, color: [u8; 3], stroke: f32) -> String {
    let mut out = format!(
        r#"font-family="'{}', sans-serif" font-size="{size}" font-weight="{}" font-style="{}" fill="{}""#,
        escape(&look.font.replace('\'', "")),
        if look.bold { "bold" } else { "normal" },
        if look.italic { "italic" } else { "normal" },
        hex(color),
    );
    if stroke > 0.0 {
        // Twice as wide, half under the fill: the outline is all outside.
        let _ = write!(
            out,
            r#" stroke="{}" stroke-width="{}" stroke-linejoin="round" paint-order="stroke""#,
            hex(look.outline_color),
            stroke * 2.0,
        );
    }
    out
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(text: &str) -> FaceSpec {
        FaceSpec {
            text: text.into(),
            color: [255, 255, 255],
            look: TimerLook::default(),
            frame_height: 1080,
        }
    }

    fn fonts() -> Arc<usvg::fontdb::Database> {
        super::super::svg::system_fonts()
    }

    fn opaque_pixels(image: &image::RgbaImage) -> usize {
        image.pixels().filter(|p| p[3] > 200).count()
    }

    #[test]
    fn the_numbers_are_drawn_over_clear() {
        let image = paint(&spec("5:00"), &fonts()).expect("drawn");
        let (w, h) = image.dimensions();
        // 15 % of 1080 is 162 px high numbers, and a little room.
        assert!((162..200).contains(&h), "{w}×{h}");
        assert!(w > h, "{w}×{h}");
        assert!(opaque_pixels(&image) > 1000, "glyphs drawn");
        assert_eq!(image.get_pixel(0, 0)[3], 0, "the corner is clear");
    }

    #[test]
    fn the_box_keeps_its_size_while_the_digits_change() {
        let a = paint(&spec("4:11"), &fonts()).unwrap();
        let b = paint(&spec("4:58"), &fonts()).unwrap();
        assert_eq!(a.dimensions(), b.dimensions());
    }

    #[test]
    fn low_hanging_digits_stay_inside() {
        // Georgia's 3, 5 and 9 hang below the line; nothing is cut off.
        let mut georgia = spec("3:59");
        georgia.look.font = "Georgia".into();
        let image = paint(&georgia, &fonts()).unwrap();
        let last_row = image.height() - 1;
        let touched = (0..image.width()).any(|x| image.get_pixel(x, last_row)[3] > 0);
        assert!(!touched, "the bottom row is clear");
        let mut level = georgia.clone();
        level.text = "1:00".into();
        assert_eq!(
            paint(&level, &fonts()).unwrap().dimensions(),
            image.dimensions(),
            "the box is as tall before a 9 comes"
        );
    }

    #[test]
    fn a_box_fills_behind_the_numbers() {
        let mut boxed = spec("5:00");
        boxed.look.box_opacity = 1.0;
        boxed.look.box_color = [0, 0, 255];
        let image = paint(&boxed, &fonts()).unwrap();
        let corner = image.get_pixel(image.width() / 2, 2);
        assert_eq!(corner.0, [0, 0, 255, 255], "the box's top edge");
    }

    #[test]
    fn a_caption_goes_above_and_makes_it_taller() {
        let plain = paint(&spec("5:00"), &fonts()).unwrap();
        let mut captioned = spec("5:00");
        captioned.look.caption = "Back in".into();
        let tall = paint(&captioned, &fonts()).unwrap();
        assert!(tall.height() > plain.height() + 40, "{} vs {}", tall.height(), plain.height());
    }

    #[test]
    fn text_is_escaped_into_the_drawing() {
        assert_eq!(escape("a<b & 'c'"), "a&lt;b &amp; &apos;c&apos;");
        let mut odd = spec("5:00");
        odd.look.caption = "Q&A <live>".into();
        odd.look.font = "No Such Font's".into();
        assert!(paint(&odd, &fonts()).is_some(), "falls back to sans serif");
    }

    #[test]
    fn a_stopwatch_banks_time_across_pauses() {
        let t0 = Instant::now();
        let mut watch = Stopwatch::default();
        watch.start(t0);
        watch.pause(t0 + std::time::Duration::from_secs(3));
        assert!((watch.elapsed_at(t0 + std::time::Duration::from_secs(9)) - 3.0).abs() < 1e-9);
        watch.start(t0 + std::time::Duration::from_secs(10));
        let at = t0 + std::time::Duration::from_secs(12);
        assert!((watch.elapsed_at(at) - 5.0).abs() < 1e-9);
        watch.set(1.0, at);
        assert!((watch.elapsed_at(at + std::time::Duration::from_secs(1)) - 2.0).abs() < 1e-9);
    }
}
