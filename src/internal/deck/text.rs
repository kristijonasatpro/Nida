//! A text input: lines typed in, drawn in a font with an optional box,
//! outline and shadow, placed by one of nine spots, faded in and out. Drawn
//! as one picture on a thread of its own whenever its text or look changes —
//! the picture before stays on air until the new one is ready, so an edit
//! live shows at once, without a flash — and placed each frame for next to
//! nothing. It can be sent to buses too, drawn over what each shows in the
//! part its screens show, laid out for that part (see
//! [`Text::set_target_frames`]).

use std::fmt::Write as _;
use std::sync::{mpsc, Arc};
use std::time::Instant;

use crate::engine::value::text::{TextAlign, TextLook, TextSettings, TextSnapshot};
use crate::renderer::blit::BlitPipeline;
use crate::renderer::GpuContext;

use super::lower_third::{Params, Placer};

/// What a drawing is of: whatever changes it.
#[derive(Debug, Clone, PartialEq)]
struct Spec {
    text: String,
    look: TextLook,
    frame: (u32, u32),
}

/// A drawing: its picture (none when there is nothing to draw), or that
/// it could not be drawn — the picture on air then stays.
type Drawing = Result<Option<image::RgbaImage>, ()>;

/// A batch drawn: each frame's drawing.
type Painted = Vec<(Spec, Drawing)>;

/// The thread the drawings are made on, a batch — every frame it is shown
/// at — at a time.
struct Painter {
    jobs: Option<mpsc::Sender<Vec<Spec>>>,
    done: mpsc::Receiver<Painted>,
    done_here: Option<Painted>,
}

impl Painter {
    fn start() -> Self {
        let (jobs_tx, jobs_rx) = mpsc::channel::<Vec<Spec>>();
        let (done_tx, done) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("text-paint".into())
            .spawn(move || {
                let fonts = super::svg::system_fonts();
                while let Ok(mut batch) = jobs_rx.recv() {
                    // Only the latest is worth drawing: typing sends many.
                    while let Ok(newer) = jobs_rx.try_recv() {
                        batch = newer;
                    }
                    let drawn = batch
                        .into_iter()
                        .map(|spec| {
                            let drawing = draw(&spec, &fonts);
                            (spec, drawing)
                        })
                        .collect();
                    if done_tx.send(drawn).is_err() {
                        break;
                    }
                }
            });
        let jobs = match spawned {
            Ok(_) => Some(jobs_tx),
            Err(e) => {
                log::warn!("Text drawing thread not started, drawing in the frame: {e}");
                None
            }
        };
        Self {
            jobs,
            done,
            done_here: None,
        }
    }

    fn ask(&mut self, batch: Vec<Spec>) {
        let unsent = match &self.jobs {
            Some(jobs) => jobs.send(batch).err().map(|mpsc::SendError(batch)| batch),
            None => Some(batch),
        };
        if let Some(batch) = unsent {
            let fonts = super::svg::system_fonts();
            self.done_here = Some(
                batch
                    .into_iter()
                    .map(|spec| {
                        let drawing = draw(&spec, &fonts);
                        (spec, drawing)
                    })
                    .collect(),
            );
        }
    }

    fn take(&mut self) -> Option<Painted> {
        let mut latest = self.done_here.take();
        while let Ok(painted) = self.done.try_recv() {
            latest = Some(painted);
        }
        latest
    }
}

/// A drawing on the GPU, and the frame it was laid out for: bound for its
/// own deck's picture, and for drawing over a bus's with placement
/// settings of its own.
struct Face {
    size: (u32, u32),
    frame: (u32, u32),
    bind_group: wgpu::BindGroup,
    over: wgpu::BindGroup,
    over_params: wgpu::Buffer,
}

pub struct Text {
    context: GpuContext,
    settings: TextSettings,
    frame: (u32, u32),
    blit: BlitPipeline,
    placer: Placer,
    /// A drawing for each frame it is shown at.
    faces: Vec<Face>,
    /// The frame of the drawing it shows itself: its own, or — just after
    /// a resize, until the new one is in — the one before, scaled.
    main_frame: Option<(u32, u32)>,
    /// The other frames it is drawn for: the parts of the buses it is sent
    /// to that their screens show.
    target_frames: Vec<(u32, u32)>,
    /// What its own drawing on the GPU is of, and what was last asked for.
    drawn: Option<Spec>,
    asked: Option<Vec<Spec>>,
    painter: Painter,
    /// How far shown (0 hidden, 1 shown) when `since`, and toward which.
    fade_from: f32,
    since: Instant,
}

impl Text {
    /// A text input, shown as its settings say.
    ///
    /// # Errors
    ///
    /// When the pipeline that places it cannot be made.
    pub fn new(
        context: &GpuContext,
        settings: TextSettings,
        frame: (u32, u32),
    ) -> anyhow::Result<Self> {
        let settings = settings.normalized();
        let shown = if settings.visible { 1.0 } else { 0.0 };
        Ok(Self {
            context: context.clone(),
            settings,
            frame: (frame.0.max(1), frame.1.max(1)),
            blit: BlitPipeline::new(&context.device, context.compositing_format)?,
            placer: Placer::new(&context.device, context.compositing_format),
            faces: Vec::new(),
            main_frame: None,
            target_frames: Vec::new(),
            drawn: None,
            asked: None,
            painter: Painter::start(),
            fade_from: shown,
            since: Instant::now(),
        })
    }

    pub fn settings(&self) -> &TextSettings {
        &self.settings
    }

    /// New text, look or visibility: drawn again (the picture before stays
    /// until the new one is ready); shown or hidden, faded from where it is.
    pub fn set_settings(&mut self, settings: TextSettings) {
        let settings = settings.normalized();
        if settings.visible != self.settings.visible
            || settings.fade_secs.to_bits() != self.settings.fade_secs.to_bits()
        {
            // From where it is now, by the time it had: a fade time changed
            // part way (or after) never jumps it.
            self.fade_from = self.shown(Instant::now());
            self.since = Instant::now();
        }
        self.settings = settings;
    }

    pub fn snapshot(&self) -> TextSnapshot {
        TextSnapshot {
            settings: self.settings.clone(),
        }
    }

    /// How far shown `now`: 0 hidden, 1 shown.
    fn shown(&self, now: Instant) -> f32 {
        let to = if self.settings.visible { 1.0 } else { 0.0 };
        let secs = self.settings.fade_secs;
        if secs <= 0.0 {
            return to;
        }
        let p = ((now - self.since).as_secs_f32() / secs).clamp(0.0, 1.0);
        // Smooth at both ends.
        let p = p * p * (3.0 - 2.0 * p);
        self.fade_from + (to - self.fade_from) * p
    }

    /// Every frame it is drawn for: its own, and those of the buses' parts.
    fn frames(&self) -> Vec<(u32, u32)> {
        let mut frames = vec![self.frame];
        for frame in &self.target_frames {
            if !frames.contains(frame) {
                frames.push(*frame);
            }
        }
        frames
    }

    /// The drawings wanted now: its text and look, at every frame.
    fn wanted(&self) -> Vec<Spec> {
        self.frames()
            .into_iter()
            .map(|frame| Spec {
                text: self.settings.text.clone(),
                look: self.settings.look.clone(),
                frame,
            })
            .collect()
    }

    /// The sizes of the parts of the buses it is sent to (their screens'
    /// parts, in master pixels): drawn for each too, laid out for it (see
    /// [`Self::render_over`]).
    pub fn set_target_frames(&mut self, frames: &[(u32, u32)]) {
        self.target_frames = frames
            .iter()
            .map(|(w, h)| ((*w).max(1), (*h).max(1)))
            .collect();
    }

    /// Whether it is drawn for buses too (see [`Self::set_target_frames`]).
    pub fn has_target_frames(&self) -> bool {
        !self.target_frames.is_empty()
    }

    /// Whether anything of it shows now: shown or fading, a drawing ready.
    pub fn shows(&self) -> bool {
        self.shown(Instant::now()) > 0.0 && !self.faces.is_empty()
    }

    /// Each frame: the drawings asked for if they changed, and taken when
    /// back.
    pub fn tick(&mut self) {
        let wanted = self.wanted();
        if self.asked.as_ref() != Some(&wanted) {
            self.painter.ask(wanted.clone());
            self.asked = Some(wanted);
        }
        if let Some(painted) = self.painter.take() {
            // Shown as they come: the painter draws in the order asked,
            // skipping to the latest, so each drawing back is newer than
            // the one on air. Edits made faster than a drawing takes show
            // one after another, not only once they stop.
            for (spec, drawing) in painted {
                self.put(spec, drawing);
            }
        }
    }

    /// A drawing on the GPU, for its frame; drawings for frames no longer
    /// shown let go.
    fn put(&mut self, spec: Spec, drawing: Drawing) {
        let frame = spec.frame;
        let frames = self.frames();
        if !frames.contains(&frame) {
            return;
        }
        let Ok(image) = drawing else {
            // Not drawn: the picture before stays on air — after a resize
            // too, scaled, until one is drawn.
            if frame == self.frame {
                self.drawn = Some(spec);
            }
            return;
        };
        self.faces.retain(|f| f.frame != frame);
        if let Some(image) = image {
            let (_, view) = super::source::upload_image_texture(&self.context, &image);
            let over_params = Placer::params(&self.context.device);
            self.faces.push(Face {
                size: image.dimensions(),
                frame,
                bind_group: self.blit.create_bind_group(&self.context.device, &view),
                over: self
                    .placer
                    .bind(&self.context.device, &view, &view, &over_params),
                over_params,
            });
        }
        if frame == self.frame {
            self.main_frame = Some(frame);
            self.drawn = Some(spec);
        }
        let main = self.main_frame;
        self.faces
            .retain(|f| frames.contains(&f.frame) || Some(f.frame) == main);
    }

    /// The drawing it shows itself, if any.
    fn main_face(&self) -> Option<&Face> {
        let frame = self.main_frame?;
        self.faces.iter().find(|f| f.frame == frame)
    }

    /// Draw it in `pass` — over a bus's picture — in the area `area` (x, y,
    /// width, height in the target's pixels), laid out for that area's
    /// frame, as far shown as it is; blended over what is there. True when
    /// anything of it was drawn.
    pub fn render_over(&self, pass: &mut wgpu::RenderPass<'_>, area: (u32, u32, u32, u32)) -> bool {
        let frame = (area.2.max(1), area.3.max(1));
        let shown = self.shown(Instant::now());
        let Some(face) = self.faces.iter().find(|f| f.frame == frame) else {
            return false;
        };
        if shown <= 0.0 {
            return false;
        }
        let (uv_scale, uv_offset) = self.placement(face);
        #[allow(clippy::cast_precision_loss)]
        let params = Params {
            uv_scale,
            uv_offset,
            plate_alpha: 0.0,
            text_alpha: shown,
            reveal: 1.0,
            from_right: 0,
            width: face.size.0 as f32,
            text_reveal: 1.0,
            text_uv_offset: uv_offset,
        };
        self.context
            .queue
            .write_buffer(&face.over_params, 0, bytemuck::bytes_of(&params));
        #[allow(clippy::cast_precision_loss)]
        pass.set_viewport(
            area.0 as f32,
            area.1 as f32,
            frame.0 as f32,
            frame.1 as f32,
            0.0,
            1.0,
        );
        pass.set_pipeline(&self.placer.over);
        pass.set_bind_group(0, &face.over, &[]);
        pass.draw(0..3, 0..1);
        true
    }

    /// Draw the frame: clear, and the text as far as it is shown.
    pub fn render(&self, cmd_buffers: &mut Vec<wgpu::CommandBuffer>, output: &wgpu::TextureView) {
        let mut encoder =
            self.context
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Text Encoder"),
                });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Text Pass"),
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
            let shown = self.shown(Instant::now());
            if let (Some(face), true) = (self.main_face(), shown > 0.0) {
                let (scale, offset) = self.placement(face);
                self.blit
                    .set_uv_transform(&self.context.queue, shown, scale, offset);
                self.blit.render(&mut pass, &face.bind_group);
            }
        }
        cmd_buffers.push(encoder.finish());
    }

    /// The UV transform that puts the drawing at its spot, at its own size
    /// — in the frame it was laid out for, so one drawn before a resize is
    /// scaled to the new frame until it is drawn again.
    #[allow(clippy::cast_precision_loss)]
    fn placement(&self, face: &Face) -> ([f32; 2], [f32; 2]) {
        let frame = (face.frame.0.max(1) as f32, face.frame.1.max(1) as f32);
        let size = (face.size.0.max(1) as f32, face.size.1.max(1) as f32);
        let look = &self.settings.look;
        let (x, y) = look.anchor.place(size, frame, look.margin * frame.1);
        (
            [frame.0 / size.0, frame.1 / size.1],
            [-x / size.0, -y / size.1],
        )
    }

    /// The master's size changed: drawn again to suit.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.frame = (width.max(1), height.max(1));
    }

    /// Wait (a while) for the drawing wanted now, for tests: true once it is
    /// on the GPU.
    pub fn wait_drawn(&mut self, timeout: std::time::Duration) -> bool {
        let until = Instant::now() + timeout;
        while Instant::now() < until {
            self.tick();
            let own = self.wanted().into_iter().find(|s| s.frame == self.frame);
            if self.drawn.is_some() && self.drawn == own {
                return self.main_face().is_some();
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        false
    }
}

// ── Drawing ─────────────────────────────────────────────────────────

/// A line's ink, from its baseline.
#[derive(Clone, Copy)]
struct Ink {
    left: f32,
    top: f32,
    width: f32,
    height: f32,
}

/// The largest side a drawing may have.
const MOST_PIXELS: f32 = 8192.0;

/// Lay out and draw `spec`'s lines: none when there is nothing to draw;
/// too big for a picture, made smaller until it fits. The box, then the
/// shadow, then the lines, each over what is under it.
fn draw(spec: &Spec, fonts: &Arc<usvg::fontdb::Database>) -> Drawing {
    #[allow(clippy::cast_precision_loss)]
    let px = (spec.look.size * spec.frame.1 as f32).max(4.0);
    let Some(mut laid) = layout(spec, px, fonts) else {
        return Ok(None);
    };
    let largest = laid.width.max(laid.height);
    if largest > MOST_PIXELS {
        laid = layout(spec, px * MOST_PIXELS / largest * 0.97, fonts).ok_or(())?;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let (wide, high) = (laid.width.ceil() as u32, laid.height.ceil() as u32);
    if wide == 0 || high == 0 || wide > 8192 || high > 8192 {
        log::warn!("Text not drawn: {wide}×{high} is too big");
        return Err(());
    }
    let options = usvg::Options {
        fontdb: fonts.clone(),
        ..Default::default()
    };
    // Drawn over what is in `picture`.
    let paint = |body: &str, picture: &mut resvg::tiny_skia::Pixmap| -> Result<(), ()> {
        let svg = format!("{}{body}</svg>", laid.head);
        let tree = usvg::Tree::from_str(&svg, &options).map_err(|e| {
            log::warn!("Text not drawn: {e}");
        })?;
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::identity(),
            &mut picture.as_mut(),
        );
        Ok(())
    };
    let mut picture = resvg::tiny_skia::Pixmap::new(wide, high).ok_or(())?;
    if !laid.boxed.is_empty() {
        paint(&laid.boxed, &mut picture)?;
    }
    if let Some((letters, blur, opacity)) = laid.shadow.as_ref().filter(|(_, blur, _)| *blur < 2.0)
    {
        // A small blur, of small letters: SVG's, resvg drawing it its own
        // way below two pixels — and soon, the picture being small.
        paint(
            &format!(
                r#"<filter id="soft" filterUnits="userSpaceOnUse" x="0" y="0" width="{wide}" height="{high}"><feGaussianBlur stdDeviation="{blur}"/></filter><g filter="url(#soft)" opacity="{opacity}">{letters}</g>"#
            ),
            &mut picture,
        )?;
    } else if let Some((letters, blur, opacity)) = &laid.shadow {
        // The letters drawn alone, their coverage blurred here: an SVG blur
        // of a big picture took ten times what the rest of it does.
        let mut drawn = resvg::tiny_skia::Pixmap::new(wide, high).ok_or(())?;
        paint(letters, &mut drawn)?;
        let mut alpha: Vec<u8> = drawn.pixels().iter().map(|p| p.alpha()).collect();
        blur_alpha(&mut alpha, wide as usize, high as usize, *blur);
        for (px, a) in drawn.pixels_mut().iter_mut().zip(alpha) {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let a = (f32::from(a) * opacity).round().clamp(0.0, 255.0) as u8;
            *px = resvg::tiny_skia::PremultipliedColorU8::from_rgba(0, 0, 0, a).ok_or(())?;
        }
        picture.draw_pixmap(
            0,
            0,
            drawn.as_ref(),
            &resvg::tiny_skia::PixmapPaint::default(),
            resvg::tiny_skia::Transform::identity(),
            None,
        );
    }
    paint(&laid.texts, &mut picture)?;
    let mut rgba = image::RgbaImage::new(wide, high);
    for (dst, src) in rgba.pixels_mut().zip(picture.pixels()) {
        let c = src.demultiply();
        *dst = image::Rgba([c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Ok(Some(rgba))
}

/// How many box blurs make a Gaussian one (as resvg's does).
const BLUR_STEPS: usize = 5;

/// The box sizes whose blurs, one after another, come close to a Gaussian
/// blur of `sigma` (Kutskir's way, as resvg does).
fn box_sizes(sigma: f32) -> [usize; BLUR_STEPS] {
    if sigma <= 0.0 {
        return [1; BLUR_STEPS];
    }
    #[allow(clippy::cast_precision_loss)]
    let n = BLUR_STEPS as f32;
    let ideal = (12.0 * sigma * sigma / n).sqrt() + 1.0;
    #[allow(clippy::cast_possible_truncation)]
    let mut lower = ideal.floor() as i32;
    if lower % 2 == 0 {
        lower -= 1;
    }
    let upper = lower + 2;
    #[allow(clippy::cast_precision_loss)]
    let l = lower as f32;
    let m = ((12.0 * sigma * sigma - n * l * l - 4.0 * n * l - 3.0 * n) / (-4.0 * l - 4.0)).round();
    let mut sizes = [0; BLUR_STEPS];
    for (i, size) in sizes.iter_mut().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let chosen = if (i as f32) < m { lower } else { upper };
        *size = usize::try_from(chosen.max(1)).unwrap_or(1);
    }
    sizes
}

/// Rounded to the nearest whole number, halves to even (as resvg rounds).
fn round_even(x: f32) -> f32 {
    (x + 12_582_912.0) - 12_582_912.0
}

/// `alpha` (a `wide` × `high` coverage, a byte a pixel) blurred as SVG's
/// Gaussian blur of `sigma` is by resvg: box blurs down, then across, each
/// rounded to bytes, past the edges clear. Rows at a time, so it reads the
/// memory in order.
fn blur_alpha(alpha: &mut [u8], wide: usize, high: usize, sigma: f32) {
    if wide == 0 || high == 0 {
        return;
    }
    let mut other = vec![0u8; alpha.len()];
    for size in box_sizes(sigma) {
        let radius = (size - 1) / 2;
        if radius == 0 {
            continue;
        }
        #[allow(clippy::cast_precision_loss)]
        let scale = 1.0 / (2 * radius + 1) as f32;
        // Down: each column's sum over the rows within reach, kept for a
        // whole row at a time.
        let mut sums = vec![0i32; wide];
        for row in alpha.chunks_exact(wide).take(radius.min(high - 1) + 1) {
            for (sum, a) in sums.iter_mut().zip(row) {
                *sum += i32::from(*a);
            }
        }
        for y in 0..high {
            for (out, sum) in other[y * wide..(y + 1) * wide].iter_mut().zip(&sums) {
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    clippy::cast_precision_loss
                )]
                {
                    *out = round_even(*sum as f32 * scale) as u8;
                }
            }
            if y + radius + 1 < high {
                let add = &alpha[(y + radius + 1) * wide..(y + radius + 2) * wide];
                for (sum, a) in sums.iter_mut().zip(add) {
                    *sum += i32::from(*a);
                }
            }
            if y >= radius {
                let gone = &alpha[(y - radius) * wide..(y - radius + 1) * wide];
                for (sum, a) in sums.iter_mut().zip(gone) {
                    *sum -= i32::from(*a);
                }
            }
        }
        // Across.
        for (src, dst) in other.chunks_exact(wide).zip(alpha.chunks_exact_mut(wide)) {
            let mut sum: i32 = src
                .iter()
                .take(radius.min(wide - 1) + 1)
                .map(|a| i32::from(*a))
                .sum();
            for x in 0..wide {
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    clippy::cast_precision_loss
                )]
                {
                    dst[x] = round_even(sum as f32 * scale) as u8;
                }
                if x + radius + 1 < wide {
                    sum += i32::from(src[x + radius + 1]);
                }
                if x >= radius {
                    sum -= i32::from(src[x - radius]);
                }
            }
        }
    }
}

/// The lines laid out, ready to draw: SVG pieces at the picture's size.
struct Laid {
    width: f32,
    height: f32,
    /// The SVG's opening tag.
    head: String,
    /// The box behind the lines; empty for none.
    boxed: String,
    /// The lines in their colours.
    texts: String,
    /// The shadow, if any: the lines in black, moved by its drop; its blur
    /// (a standard deviation, in pixels) and how solid it is.
    shadow: Option<(String, f32, f32)>,
}

/// The lines laid out at font size `px`; none when there is nothing to draw.
#[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
fn layout(spec: &Spec, px: f32, fonts: &Arc<usvg::fontdb::Database>) -> Option<Laid> {
    let look = &spec.look;
    let lines: Vec<&str> = spec.text.lines().map(str::trim_end).collect();
    if lines.iter().all(|l| l.trim().is_empty()) {
        return None;
    }
    // Lines a step apart, from baseline to baseline, whatever is typed;
    // the room above the first and below the last as tall as the tallest
    // letters and deepest tails can be — or a line's own ink, if it
    // reaches further (Ǻ).
    let probe = measure("ŽĄÁÉjgyąęų|", px, look, fonts).unwrap_or(Ink {
        left: 0.0,
        top: -0.95 * px,
        width: px,
        height: 1.2 * px,
    });
    let step = px * look.spacing;
    let inks: Vec<Option<Ink>> = lines
        .iter()
        .map(|l| {
            (!l.trim().is_empty())
                .then(|| measure(l, px, look, fonts))
                .flatten()
        })
        .collect();
    let last = (lines.len() - 1) as f32 * step;
    let top = inks
        .iter()
        .enumerate()
        .filter_map(|(i, ink)| ink.map(|ink| i as f32 * step + ink.top))
        .fold(probe.top, f32::min);
    let bottom = inks
        .iter()
        .enumerate()
        .filter_map(|(i, ink)| ink.map(|ink| i as f32 * step + ink.top + ink.height))
        .fold(last + probe.top + probe.height, f32::max);
    let widest = inks.iter().flatten().map(|i| i.width).fold(0.0, f32::max);
    let stroke = look.outline * px;
    let boxed_on = look.box_opacity > 0.0;
    let shadow_on = look.shadow_opacity > 0.0;
    let blur = 0.06 * px;
    let drop = 0.05 * px;
    // Room round the letters: the box's padding, and what the outline and
    // the shadow reach past them.
    let pad = if boxed_on { look.box_padding * px } else { 0.0 }
        + stroke
        + if shadow_on { drop + 3.0 * blur } else { 0.0 }
        + 2.0;
    let width = widest + 2.0 * pad;
    let height = bottom - top + 2.0 * pad;

    let mut head = String::new();
    let _ = write!(
        head,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{wide}" height="{high}" viewBox="0 0 {wide} {high}">"#,
        wide = width.ceil(),
        high = height.ceil(),
    );
    let mut boxed = String::new();
    if boxed_on {
        let _ = write!(
            boxed,
            r#"<rect width="{}" height="{}" rx="{r}" fill="{}" fill-opacity="{}"/>"#,
            width.ceil(),
            height.ceil(),
            hex(look.box_colour),
            look.box_opacity,
            r = look.box_radius * px,
        );
    }
    let mut texts = String::new();
    for (i, (line, ink)) in lines.iter().zip(&inks).enumerate() {
        let Some(ink) = ink else { continue };
        // Lined up by their ink, not their advance: side bearings don't
        // pull a line off its edge.
        let left = match look.align {
            TextAlign::Left => pad,
            TextAlign::Centre => (width - ink.width) / 2.0,
            TextAlign::Right => width - pad - ink.width,
        } - ink.left;
        let baseline = pad - top + i as f32 * step;
        let _ = write!(
            texts,
            r#"<text x="{left}" y="{baseline}" {}>{}</text>"#,
            attributes(look, px),
            escape(line)
        );
    }
    let shadow = shadow_on.then(|| {
        // The shadow's letters black, their own fill and stroke left out.
        let letters = format!(
            r##"<g transform="translate({drop} {drop})" fill="#000000" stroke="#000000">{}</g>"##,
            texts
                .replace(&format!(r#"fill="{}""#, hex(look.colour)), "")
                .replace(&format!(r#"stroke="{}""#, hex(look.outline_colour)), ""),
        );
        (letters, blur, look.shadow_opacity)
    });
    Some(Laid {
        width,
        height,
        head,
        boxed,
        texts,
        shadow,
    })
}

/// The ink of `text` set at `size`: none if no glyph of it could be drawn.
fn measure(
    text: &str,
    size: f32,
    look: &TextLook,
    fonts: &Arc<usvg::fontdb::Database>,
) -> Option<Ink> {
    let room = size * 4.0;
    #[allow(clippy::cast_precision_loss)]
    let wide = size * (text.chars().count() as f32 + 2.0) * 1.5;
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{wide}" height="{h}"><text x="0" y="{room}" {}>{}</text></svg>"#,
        attributes(look, size),
        escape(text),
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
fn attributes(look: &TextLook, size: f32) -> String {
    let mut out = format!(
        r#"font-family="'{}', sans-serif" font-size="{size}" font-weight="{}" font-style="{}" fill="{}""#,
        escape(&look.font.replace('\'', "")),
        if look.bold { "bold" } else { "normal" },
        if look.italic { "italic" } else { "normal" },
        hex(look.colour),
    );
    let stroke = look.outline * size;
    if stroke > 0.0 {
        // Twice as wide, half under the fill: the outline is all outside.
        let _ = write!(
            out,
            r#" stroke="{}" stroke-width="{}" stroke-linejoin="round" paint-order="stroke""#,
            hex(look.outline_colour),
            stroke * 2.0,
        );
    }
    out
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// `text` safe in SVG: its markup escaped, and control characters — which
/// XML cannot hold, and would leave the whole drawing undrawn — left out (a
/// tab a space).
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\t' => out.push(' '),
            c if c.is_control() => {}
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

    fn spec(text: &str) -> Spec {
        Spec {
            text: text.into(),
            look: TextLook::default(),
            frame: (1920, 1080),
        }
    }

    fn rendered(svg: &str, fonts: &Arc<usvg::fontdb::Database>, wide: u32, high: u32) -> Vec<u8> {
        let options = usvg::Options {
            fontdb: fonts.clone(),
            ..Default::default()
        };
        let tree = usvg::Tree::from_str(svg, &options).expect("svg");
        let mut pixmap = resvg::tiny_skia::Pixmap::new(wide, high).expect("pixmap");
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::identity(),
            &mut pixmap.as_mut(),
        );
        pixmap.data().to_vec()
    }

    /// The shadow blurred here draws as SVG's Gaussian blur of the same
    /// letters does (resvg's), a narrow line's too — box, shadow and lines
    /// together, within a step or two of a byte — and in a fraction of the
    /// time.
    #[test]
    fn the_shadow_blurred_here_matches_svgs_blur() {
        let fonts = super::super::svg::system_fonts();
        for (text, boxed, size) in [
            ("PERTRAUKA\nTęsiame 14:00", 0.7, 0.08),
            ("i", 0.0, 0.08),
            ("Ž\nj", 0.4, 0.08),
            // Small: a blur under two pixels, which resvg draws its own way.
            ("PERTRAUKA\nTęsiame 14:00", 0.0, 0.02),
        ] {
            let look = TextLook {
                shadow_opacity: 0.8,
                box_opacity: boxed,
                size,
                ..TextLook::default()
            };
            let s = Spec {
                text: text.into(),
                look,
                frame: (1920, 1080),
            };
            let px = s.look.size * 1080.0;
            let laid = layout(&s, px, &fonts).expect("laid out");
            let (wide, high) = (laid.width.ceil() as u32, laid.height.ceil() as u32);
            let (letters, blur, opacity) = laid.shadow.clone().expect("a shadow");
            // As it was drawn before: the blur an SVG filter, over the picture.
            let reference = format!(
                r#"{}{}<filter id="soft" filterUnits="userSpaceOnUse" x="-{w}" y="-{h}" width="{w3}" height="{h3}"><feGaussianBlur stdDeviation="{blur}"/></filter><g filter="url(#soft)" opacity="{opacity}">{letters}</g>{}</svg>"#,
                laid.head,
                laid.boxed,
                laid.texts,
                w = laid.width,
                h = laid.height,
                w3 = 3.0 * laid.width,
                h3 = 3.0 * laid.height,
            );
            let t = Instant::now();
            let before = rendered(&reference, &fonts, wide, high);
            let slow = t.elapsed();
            let t = Instant::now();
            let now = draw(&s, &fonts).expect("drawn").expect("a picture");
            let fast = t.elapsed();
            // Both premultiplied, so a near-clear pixel's colour counts little.
            let mut most = 0u8;
            for (p, q) in now.pixels().zip(before.as_chunks::<4>().0) {
                let a = u16::from(p[3]);
                for c in 0..3 {
                    let pre = u8::try_from(u16::from(p[c]) * a / 255).unwrap_or(255);
                    most = most.max(pre.abs_diff(q[c]));
                }
                most = most.max(p[3].abs_diff(q[3]));
            }
            println!("{text:?}: {wide}x{high}, differs by at most {most}; {fast:?} vs {slow:?}");
            assert!(most <= 3, "{text:?}: differs by {most}");
        }
    }

    /// Resized, and the new size's drawing failed: the picture before stays
    /// on air, scaled, not lost.
    #[test]
    fn a_failed_drawing_after_a_resize_keeps_the_picture() {
        let Ok(gpu) = GpuContext::new_headless() else {
            return;
        };
        let settings = TextSettings {
            text: "PERTRAUKA".into(),
            ..TextSettings::default()
        };
        let mut text = Text::new(&gpu, settings, (640, 360)).expect("text");
        assert!(text.wait_drawn(std::time::Duration::from_secs(10)), "drawn");
        text.resize(1280, 720);
        let spec = text
            .wanted()
            .into_iter()
            .find(|s| s.frame == (1280, 720))
            .expect("the new size wanted");
        text.put(spec, Err(()));
        assert_eq!(
            text.main_face().map(|f| f.frame),
            Some((640, 360)),
            "the picture before kept"
        );
    }

    /// Typed faster than a drawing takes: the picture keeps up as drawings
    /// come back, not held on the first until the typing stops.
    #[test]
    fn typing_faster_than_drawing_still_shows_as_it_goes() {
        let Ok(gpu) = GpuContext::new_headless() else {
            return;
        };
        let mut settings = TextSettings {
            text: "PERTRAUKA 0".into(),
            ..TextSettings::default()
        };
        let mut text = Text::new(&gpu, settings.clone(), (640, 360)).expect("text");
        assert!(text.wait_drawn(std::time::Duration::from_secs(10)), "drawn");
        let first = text.drawn.clone();
        // A new text every 2 ms for a second and a half; each drawing takes
        // longer.
        let mut shown = Vec::new();
        let until = Instant::now() + std::time::Duration::from_millis(1500);
        let mut n = 0;
        while Instant::now() < until {
            n += 1;
            settings.text = format!("PERTRAUKA {n}");
            text.set_settings(settings.clone());
            text.tick();
            if text.drawn != first && shown.last() != Some(&text.drawn) {
                shown.push(text.drawn.clone());
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(
            shown.len() >= 2,
            "drawings shown while typing: {} (of {n} edits)",
            shown.len()
        );
    }

    /// Two lines draw taller than one, as wide as the wider; the letters
    /// have ink; nothing typed draws nothing.
    #[test]
    fn lines_lay_out_and_draw() {
        let fonts = super::super::svg::system_fonts();
        let one = draw(&spec("PERTRAUKA"), &fonts)
            .expect("drawn")
            .expect("a picture");
        let two = draw(&spec("PERTRAUKA\nTęsiame 14:00"), &fonts)
            .expect("drawn")
            .expect("a picture");
        assert!(
            two.height() > one.height() + 50,
            "{} vs {}",
            two.height(),
            one.height()
        );
        assert!(two.width() >= one.width());
        assert!(one.pixels().any(|p| p[3] > 200));
        assert!(matches!(draw(&spec("  \n "), &fonts), Ok(None)));
    }

    /// A line far too long for one picture is drawn smaller, not dropped;
    /// a capital with a mark over it (Ǻ) is not cut at the top.
    #[test]
    fn a_long_line_is_drawn_smaller_and_marks_are_not_cut() {
        let fonts = super::super::svg::system_fonts();
        let long = draw(&spec(&"W".repeat(400)), &fonts)
            .expect("drawn")
            .expect("a picture");
        assert!(long.width() <= 8192);
        let mut s = spec("Ǻ");
        s.look.shadow_opacity = 0.0;
        let img = draw(&s, &fonts).expect("drawn").expect("a picture");
        let top_ink = (0..img.width())
            .filter(|x| img.get_pixel(*x, 0)[3] > 16)
            .count();
        assert_eq!(top_ink, 0, "the mark reaches the top edge: cut");
        assert!(img.pixels().any(|p| p[3] > 200));
    }

    /// A control character in the text (one pasted, or sent over the API)
    /// is left out, the line still drawn — not the whole drawing lost.
    #[test]
    fn a_control_character_is_left_out() {
        let fonts = super::super::svg::system_fonts();
        let img = draw(&spec("Session\u{c}14:00"), &fonts)
            .expect("drawn")
            .expect("a picture");
        assert!(img.pixels().filter(|p| p[3] > 200).count() > 200);
    }

    /// Left, centre and right put a short line under a long one at its left,
    /// middle and right.
    #[test]
    fn a_short_line_lines_up_as_asked() {
        let fonts = super::super::svg::system_fonts();
        let ink_middle = |align: TextAlign| {
            let mut s = spec("Ilgesnė pirma eilutė\nTrumpa");
            s.look.align = align;
            s.look.shadow_opacity = 0.0;
            let img = draw(&s, &fonts).expect("drawn").expect("a picture");
            let (w, h) = img.dimensions();
            // The second line's ink, across.
            let xs: Vec<u32> = (h * 6 / 10..h)
                .flat_map(|y| (0..w).map(move |x| (x, y)))
                .filter(|(x, y)| img.get_pixel(*x, *y)[3] > 128)
                .map(|(x, _)| x)
                .collect();
            let (min, max) = (
                *xs.iter().min().expect("ink"),
                *xs.iter().max().expect("ink"),
            );
            (min + max) as f32 / 2.0 / w as f32
        };
        let (l, c, r) = (
            ink_middle(TextAlign::Left),
            ink_middle(TextAlign::Centre),
            ink_middle(TextAlign::Right),
        );
        assert!(l < 0.4 && (c - 0.5).abs() < 0.05 && r > 0.6, "{l} {c} {r}");
    }
}
