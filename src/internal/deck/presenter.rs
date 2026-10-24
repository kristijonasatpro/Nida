//! A presenter view: a PowerPoint input's show for the speaker's own screen
//! (a confidence monitor) — the slide on screen, live, with its number among
//! those the show plays, the speaker notes beside it, what comes next, how
//! long the talk has run and the time of day.
//!
//! Drawn as a timer is: the words on a thread of their own each time what
//! they say changes (each second, for the clocks); the slide placed over
//! them every frame, straight from the PowerPoint input's capture.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{mpsc, Arc};
use std::time::Instant;

use crate::engine::value::presentation::{
    ClocksAt, PresentationSnapshot, PresentationStatus, PresenterLayout, Screen,
};
use crate::renderer::blit::BlitPipeline;
use crate::renderer::region::Pixels;
use crate::renderer::GpuContext;

const BACKGROUND: &str = "#101010";
const DIM: &str = "#8a8a8a";
const LIGHT: &str = "#f2f2f2";
const ACCENT: &str = "#7fc4ff";
const WARN: &str = "#e6aa3c";
const FONT: &str = "'Helvetica Neue', Helvetica, Arial, sans-serif";

/// What the words say, taken from the PowerPoint input's show each frame.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Words {
    /// "3 / 22", or what the show waits on.
    pub position: String,
    pub title: String,
    pub notes: String,
    /// "Next: 4 · Results", "Last slide".
    pub next: String,
    /// Over the slide: "BLACK", "WHITE", "END", or why there is no show.
    pub banner: String,
}

impl Words {
    /// The words for a PowerPoint input's show; `None` (no such input) says so.
    #[must_use]
    pub fn of(show: Option<&PresentationSnapshot>) -> Self {
        let Some(p) = show else {
            return Self {
                banner: "The PowerPoint input is gone".into(),
                ..Self::default()
            };
        };
        let position = match (p.position, p.shown) {
            (Some(at), Some(of)) => format!("{at} / {of}"),
            (Some(at), None) => at.to_string(),
            _ => "—".into(),
        };
        let next = match (p.next_slide, &p.next_title) {
            (Some(n), Some(title)) => format!("Next: {n} · {title}"),
            (Some(n), None) => format!("Next: slide {n}"),
            (None, _) if p.slide.is_some() => "Last slide".into(),
            (None, _) => String::new(),
        };
        let banner = match (p.status, p.screen) {
            (PresentationStatus::Opening, _) => "Opening in PowerPoint…".into(),
            (PresentationStatus::Failed | PresentationStatus::NoWindow, _) => {
                p.message.clone().unwrap_or_else(|| "PowerPoint".into())
            }
            (PresentationStatus::Showing, Screen::Black) => "BLACK".into(),
            (PresentationStatus::Showing, Screen::White) => "WHITE".into(),
            (PresentationStatus::Showing, Screen::Ended) => "END".into(),
            (PresentationStatus::Showing, Screen::Slide) => String::new(),
        };
        Self {
            position,
            title: p.title.clone().unwrap_or_default(),
            notes: p.notes.clone(),
            next,
            banner,
        }
    }
}

/// A rectangle, frame pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Where everything goes on a `frame`: the bar — where the show is, the
/// clocks — across the top or the foot, the slide down the left in its own
/// shape, what comes next under it, the notes down the right.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    /// The bar's top, and its height.
    pub bar_y: f32,
    pub bar: f32,
    pub slide: Rect,
    pub next_baseline: f32,
    pub notes: Rect,
}

/// The layout for a `frame`, the slide `aspect` (width over height) wide,
/// as `settings` has it.
#[must_use]
pub fn layout(frame: (u32, u32), aspect: f32, settings: &PresenterLayout) -> Layout {
    #[allow(clippy::cast_precision_loss)]
    let (w, h) = (frame.0.max(1) as f32, frame.1.max(1) as f32);
    let aspect = if aspect.is_finite() && aspect > 0.1 {
        aspect
    } else {
        16.0 / 9.0
    };
    let settings = settings.sanitized();
    let margin = 0.025 * h;
    let bar = 0.1 * h;
    // What the slide and the notes share: all but the bar.
    let (bar_y, top, bottom) = match settings.clocks_at {
        ClocksAt::Top => (0.0, bar, h),
        ClocksAt::Bottom => (h - bar, 0.0, h - bar),
    };
    let column = settings.slide_share * w;
    // Room under the slide for what comes next, when it is said.
    let below = if settings.next {
        0.09 * h * settings.next_size
    } else {
        0.0
    };
    let room = (
        column - 1.5 * margin,
        (bottom - top - below - 2.0 * margin).max(1.0),
    );
    let (sw, sh) = if room.0 / room.1 > aspect {
        (room.1 * aspect, room.1)
    } else {
        (room.0, room.0 / aspect)
    };
    let slide = Rect {
        x: margin,
        y: top + margin,
        w: sw,
        h: sh,
    };
    let notes_x = column + 0.5 * margin;
    Layout {
        bar_y,
        bar,
        slide,
        next_baseline: slide.y + sh + 0.065 * h * settings.next_size,
        notes: Rect {
            x: notes_x,
            y: top + margin,
            w: w - notes_x - margin,
            h: bottom - top - 2.0 * margin,
        },
    }
}

/// What the painter is asked to draw.
#[derive(Debug, Clone, PartialEq)]
struct PanelSpec {
    words: Words,
    elapsed: String,
    clock: String,
    frame: (u32, u32),
    /// The slide's shape, in thousandths.
    aspect_milli: u32,
    layout: PresenterLayout,
}

struct Face {
    size: (u32, u32),
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    /// The banner's strip over the slide (panel pixels): drawn again over
    /// the live slide, which would cover it.
    banner: Option<Rect>,
}

/// A panel drawn, and its banner's strip.
type Drawing = (image::RgbaImage, Option<Rect>);
/// The notes as last fitted: their text, room and size (as bits), and the
/// size and lines they were fitted to.
type NotesFit = (String, Rect, u32, f32, Vec<String>);
type Painted = (PanelSpec, Option<Drawing>);

/// The panel is drawn at most this many pixels on its long side, and scaled
/// up past it: a 16K master keeps its words.
const PANEL_MOST: u32 = 4096;

/// The size the panel is drawn at for a `frame`.
fn panel_size(frame: (u32, u32)) -> (u32, u32) {
    let long = frame.0.max(frame.1).max(1);
    if long <= PANEL_MOST {
        return (frame.0.max(1), frame.1.max(1));
    }
    let k = f64::from(PANEL_MOST) / f64::from(long);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let scaled = |v: u32| ((f64::from(v) * k).round() as u32).max(1);
    (scaled(frame.0), scaled(frame.1))
}

/// Words' widths, measured once each, by word and size.
#[derive(Default)]
struct Widths(HashMap<(String, u32, bool), f32>);

impl Widths {
    fn of(
        &mut self,
        text: &str,
        size: f32,
        bold: bool,
        fonts: &Arc<usvg::fontdb::Database>,
    ) -> f32 {
        let key = (text.to_string(), size.to_bits(), bold);
        if let Some(w) = self.0.get(&key) {
            return *w;
        }
        if self.0.len() > 4096 {
            self.0.clear();
        }
        let w = measure(text, size, bold, fonts);
        self.0.insert(key, w);
        w
    }
}

/// How wide `text` sets at `size`: its advance as laid out.
fn measure(text: &str, size: f32, bold: bool, fonts: &Arc<usvg::fontdb::Database>) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let guess = 0.55 * size * text.chars().count() as f32;
    // A dot either side: a word's own spaces are measured too, and the dots'
    // width is taken off.
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}"><text x="0" y="{y}" font-family="{FONT}" font-size="{size}" font-weight="{weight}" xml:space="preserve">.{t}.</text><text x="0" y="{y2}" font-family="{FONT}" font-size="{size}" font-weight="{weight}">..</text></svg>"#,
        w = guess * 3.0 + size * 4.0,
        h = size * 6.0,
        y = size * 2.0,
        y2 = size * 4.5,
        weight = if bold { "bold" } else { "normal" },
        t = escape(text),
    );
    let options = usvg::Options {
        fontdb: fonts.clone(),
        ..Default::default()
    };
    let Ok(tree) = usvg::Tree::from_str(&svg, &options) else {
        return guess;
    };
    let widths: Vec<f32> = tree
        .root()
        .children()
        .iter()
        .filter_map(|node| match node {
            usvg::Node::Text(t) => Some(t.flattened().abs_bounding_box().width()),
            _ => None,
        })
        .collect();
    match widths.as_slice() {
        [with, dots] if *with > 0.0 => (with - dots).max(0.0),
        _ => guess,
    }
}

/// A word wider than `width` cut into pieces that fit, a character at a
/// time: a long address in the notes.
fn break_word(
    word: &str,
    size: f32,
    width: f32,
    widths: &mut Widths,
    fonts: &Arc<usvg::fontdb::Database>,
) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut piece = String::new();
    for c in word.chars() {
        let mut longer = piece.clone();
        longer.push(c);
        if !piece.is_empty() && widths.of(&longer, size, false, fonts) > width {
            pieces.push(std::mem::take(&mut piece));
            piece.push(c);
        } else {
            piece = longer;
        }
    }
    if !piece.is_empty() {
        pieces.push(piece);
    }
    pieces
}

/// `text` broken into lines no wider than `width` at `size`: its own lines
/// kept, words moved down whole — a word wider than a line cut to fit.
fn wrap(
    text: &str,
    size: f32,
    width: f32,
    widths: &mut Widths,
    fonts: &Arc<usvg::fontdb::Database>,
) -> Vec<String> {
    let space = widths.of(" ", size, false, fonts);
    let mut lines = Vec::new();
    for paragraph in text.lines() {
        let mut line = String::new();
        let mut used = 0.0;
        for word in paragraph.split_whitespace() {
            let pieces = if widths.of(word, size, false, fonts) > width {
                break_word(word, size, width, widths, fonts)
            } else {
                vec![word.to_string()]
            };
            for piece in pieces {
                let w = widths.of(&piece, size, false, fonts);
                if !line.is_empty() && used + space + w > width {
                    lines.push(std::mem::take(&mut line));
                    used = 0.0;
                }
                if !line.is_empty() {
                    line.push(' ');
                    used += space;
                }
                line.push_str(&piece);
                used += w;
            }
        }
        lines.push(line);
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

/// The notes set to fill their column: the largest size, from a comfortable
/// one down to a smallest, at which they fit; at the smallest, cut with "…".
/// One line of `text` within `room`: at `size`, smaller down to half of it,
/// then cut with "…".
fn fit_line(
    text: &str,
    size: f32,
    room: f32,
    widths: &mut Widths,
    fonts: &Arc<usvg::fontdb::Database>,
) -> (f32, String) {
    let wide = widths.of(text, size, false, fonts);
    if wide <= room {
        return (size, text.to_string());
    }
    // A little under the room: widths do not scale quite evenly with size.
    let size = (size * room / wide * 0.98).max(size * 0.5);
    if widths.of(text, size, false, fonts) <= room {
        return (size, text.to_string());
    }
    let mut line = text.to_string();
    while line.pop().is_some() {
        let cut = format!("{} …", line.trim_end());
        if widths.of(&cut, size, false, fonts) <= room {
            return (size, cut);
        }
    }
    (size, String::new())
}

fn fit_notes(
    notes: &str,
    rect: Rect,
    frame_h: f32,
    widths: &mut Widths,
    fonts: &Arc<usvg::fontdb::Database>,
) -> (f32, Vec<String>) {
    let mut size = 0.048 * frame_h;
    let smallest = 0.026 * frame_h;
    loop {
        let lines = wrap(notes, size, rect.w, widths, fonts);
        #[allow(clippy::cast_precision_loss)]
        let fits = (lines.len() as f32) * size * 1.3 <= rect.h;
        if fits {
            return (size, lines);
        }
        if size * 0.9 < smallest {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let room = (rect.h / (size * 1.3)).floor().max(1.0) as usize;
            let mut kept: Vec<String> = lines.into_iter().take(room).collect();
            if let Some(last) = kept.last_mut() {
                // The "…" fits too: words come off the end until it does —
                // then characters, from a piece of a long address.
                loop {
                    let cut = format!("{last} …");
                    if last.is_empty() || widths.of(&cut, size, false, fonts) <= rect.w {
                        *last = cut.trim_start().to_string();
                        break;
                    }
                    match last.rfind(' ') {
                        Some(at) => last.truncate(at),
                        None => {
                            last.pop();
                        }
                    }
                }
            }
            return (size, kept);
        }
        size *= 0.9;
    }
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

/// The panel for `spec`: the whole frame, opaque, the slide's place left
/// dark for the live slide drawn over it; and the banner's strip over the
/// slide, when there is a banner.
fn paint(
    spec: &PanelSpec,
    fonts: &Arc<usvg::fontdb::Database>,
    widths: &mut Widths,
    notes_cache: &mut Option<NotesFit>,
) -> Option<Drawing> {
    let (fw, fh) = spec.frame;
    let (svg, banner) = panel_svg(spec, fonts, widths, notes_cache)?;
    let options = usvg::Options {
        fontdb: fonts.clone(),
        ..Default::default()
    };
    let tree = match usvg::Tree::from_str(&svg, &options) {
        Ok(tree) => tree,
        Err(e) => {
            log::warn!("Presenter view not drawn: {e}");
            return None;
        }
    };
    let mut pixmap = resvg::tiny_skia::Pixmap::new(fw, fh)?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    let mut rgba = image::RgbaImage::new(fw, fh);
    for (dst, src) in rgba.pixels_mut().zip(pixmap.pixels()) {
        let c = src.demultiply();
        *dst = image::Rgba([c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Some((rgba, banner))
}

/// The panel as SVG, and its banner's strip (see [`paint`]).
fn panel_svg(
    spec: &PanelSpec,
    fonts: &Arc<usvg::fontdb::Database>,
    widths: &mut Widths,
    notes_cache: &mut Option<NotesFit>,
) -> Option<(String, Option<Rect>)> {
    let (fw, fh) = spec.frame;
    if fw == 0 || fh == 0 || fw > 8192 || fh > 8192 {
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    let (wide, high) = (fw as f32, fh as f32);
    #[allow(clippy::cast_precision_loss)]
    let at = layout(spec.frame, spec.aspect_milli as f32 / 1000.0, &spec.layout);
    let settings = spec.layout.sanitized();
    let margin = 0.025 * high;
    let mut svg = String::new();
    let _ = write!(
        svg,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{fw}" height="{fh}" viewBox="0 0 {fw} {fh}"><rect width="{fw}" height="{fh}" fill="{BACKGROUND}"/>"#
    );
    let text = |svg: &mut String,
                x: f32,
                y: f32,
                size: f32,
                fill: &str,
                anchor: &str,
                bold: bool,
                s: &str| {
        let _ = write!(
            svg,
            r#"<text x="{x}" y="{y}" font-family="{FONT}" font-size="{size}" font-weight="{}" fill="{fill}" text-anchor="{anchor}">{}</text>"#,
            if bold { "bold" } else { "normal" },
            escape(s)
        );
    };
    // The bar: where the show is, how long it has run, the time of day —
    // each clock when it is wanted.
    let bar_size = at.bar * 0.62;
    let baseline = at.bar_y + at.bar * 0.78;
    text(
        &mut svg,
        margin,
        baseline,
        bar_size,
        LIGHT,
        "start",
        true,
        &spec.words.position,
    );
    if settings.timer {
        text(
            &mut svg,
            wide / 2.0,
            baseline,
            bar_size,
            ACCENT,
            "middle",
            true,
            &spec.elapsed,
        );
    }
    if settings.clock {
        text(
            &mut svg,
            wide - margin,
            baseline,
            bar_size,
            LIGHT,
            "end",
            false,
            &spec.clock,
        );
    }
    // The slide's place.
    let slide = at.slide;
    let _ = write!(
        svg,
        r##"<rect x="{}" y="{}" width="{}" height="{}" fill="#000000" stroke="#3a3a3a" stroke-width="{}"/>"##,
        slide.x,
        slide.y,
        slide.w,
        slide.h,
        (0.002 * high).max(1.0)
    );
    // The banner across the slide's foot, two lines at most.
    let mut banner = None;
    if !spec.words.banner.is_empty() {
        // Two lines and their strip within the slide, however narrow.
        let size = (0.045 * high).min(slide.h / 3.2);
        let lines: Vec<String> = wrap(&spec.words.banner, size, slide.w * 0.9, widths, fonts)
            .into_iter()
            .take(2)
            .collect();
        #[allow(clippy::cast_precision_loss)]
        let strip_h = size * (1.3 * lines.len().max(1) as f32 + 0.5);
        let strip = Rect {
            x: slide.x,
            y: slide.y + slide.h - strip_h,
            w: slide.w,
            h: strip_h,
        };
        let _ = write!(
            svg,
            r##"<rect x="{}" y="{}" width="{}" height="{}" fill="#000000" fill-opacity="0.8"/>"##,
            strip.x, strip.y, strip.w, strip.h
        );
        for (i, line) in lines.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let baseline = strip.y + size * (0.25 + 1.3 * (i + 1) as f32) - size * 0.3;
            text(
                &mut svg,
                slide.x + slide.w / 2.0,
                baseline,
                size,
                WARN,
                "middle",
                true,
                line,
            );
        }
        banner = Some(strip);
    }
    // What comes next, under the slide, when it is wanted: no wider than
    // the room up to the notes.
    if settings.next {
        let room = (at.notes.x - slide.x - margin).max(1.0);
        let (size, line) = fit_line(
            &spec.words.next,
            0.045 * high * settings.next_size,
            room,
            widths,
            fonts,
        );
        text(
            &mut svg,
            slide.x,
            at.next_baseline,
            size,
            DIM,
            "start",
            false,
            &line,
        );
    }
    // The notes, with the slide's title over them.
    let column = at.notes;
    let label = 0.028 * high;
    text(
        &mut svg,
        column.x,
        column.y + label,
        label,
        DIM,
        "start",
        true,
        "NOTES",
    );
    let mut top = column.y + label * 1.8;
    if !spec.words.title.is_empty() {
        let size = 0.04 * high;
        let title = wrap(&spec.words.title, size, column.w, widths, fonts);
        for line in title.iter().take(2) {
            top += size * 1.15;
            text(&mut svg, column.x, top, size, ACCENT, "start", true, line);
        }
        top += size * 0.5;
    }
    let notes_rect = Rect {
        x: column.x,
        y: top,
        w: column.w,
        h: (column.y + column.h - top).max(0.0),
    };
    let (size, lines) = match notes_cache {
        Some((notes, rect, scale, size, lines))
            if *notes == spec.words.notes
                && *rect == notes_rect
                && *scale == settings.notes_size.to_bits() =>
        {
            (*size, lines.clone())
        }
        _ => {
            let (size, lines) = fit_notes(
                &spec.words.notes,
                notes_rect,
                high * settings.notes_size,
                widths,
                fonts,
            );
            *notes_cache = Some((
                spec.words.notes.clone(),
                notes_rect,
                settings.notes_size.to_bits(),
                size,
                lines.clone(),
            ));
            (size, lines)
        }
    };
    if lines.is_empty() {
        text(
            &mut svg,
            column.x,
            top + 0.04 * high,
            0.035 * high,
            DIM,
            "start",
            false,
            "No notes",
        );
    }
    for line in &lines {
        top += size * 1.3;
        text(
            &mut svg,
            column.x,
            top - size * 0.3,
            size,
            LIGHT,
            "start",
            false,
            line,
        );
    }
    svg.push_str("</svg>");
    Some((svg, banner))
}

/// The thread the panel is drawn on: only the latest asked is drawn.
struct Painter {
    jobs: Option<mpsc::Sender<PanelSpec>>,
    done: mpsc::Receiver<Painted>,
    done_here: Option<Painted>,
}

impl Painter {
    fn start() -> Self {
        let (jobs_tx, jobs_rx) = mpsc::channel::<PanelSpec>();
        let (done_tx, done) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("presenter-paint".into())
            .spawn(move || {
                let fonts = super::svg::system_fonts();
                let mut widths = Widths::default();
                let mut notes = None;
                while let Ok(mut spec) = jobs_rx.recv() {
                    while let Ok(newer) = jobs_rx.try_recv() {
                        spec = newer;
                    }
                    let panel = paint(&spec, &fonts, &mut widths, &mut notes);
                    if done_tx.send((spec, panel)).is_err() {
                        break;
                    }
                }
            });
        let jobs = match spawned {
            Ok(_) => Some(jobs_tx),
            Err(e) => {
                log::warn!(
                    "Presenter view's drawing thread not started, drawing in the frame: {e}"
                );
                None
            }
        };
        Self {
            jobs,
            done,
            done_here: None,
        }
    }

    fn ask(&mut self, spec: PanelSpec) {
        let unsent = match &self.jobs {
            Some(jobs) => jobs.send(spec).err().map(|mpsc::SendError(spec)| spec),
            None => Some(spec),
        };
        if let Some(spec) = unsent {
            let panel = paint(
                &spec,
                &super::svg::system_fonts(),
                &mut Widths::default(),
                &mut None,
            );
            self.done_here = Some((spec, panel));
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

/// A presenter view's deck source.
pub struct Presenter {
    context: GpuContext,
    /// The PowerPoint input's layer (deck UUID) it shows.
    follows: String,
    frame: (u32, u32),
    words: Words,
    /// When the show it follows was first seen running: how long the talk
    /// has run.
    started: Option<Instant>,
    /// Its clocks, what comes next, and the sizes of slide and notes.
    layout: PresenterLayout,
    /// The layout, slide shape and size of the panel shown: the live slide
    /// goes where its placeholder is — scaled with the panel when the
    /// master's size has changed — until a panel drawn anew comes.
    shown: Option<(PresenterLayout, f32, (u32, u32))>,
    panel_blit: BlitPipeline,
    slide_blit: BlitPipeline,
    panel: Option<Face>,
    asked: Option<PanelSpec>,
    painter: Painter,
    /// The PowerPoint input's live picture and its size, handed over each
    /// frame by the render loop.
    pub slide: Option<(wgpu::TextureView, (u32, u32))>,
}

impl Presenter {
    /// A presenter view of the PowerPoint input whose layer is `follows`.
    ///
    /// # Errors
    ///
    /// Returns an error if its drawing pipelines cannot be made.
    pub fn new(context: &GpuContext, follows: String, frame: (u32, u32)) -> anyhow::Result<Self> {
        Ok(Self {
            context: context.clone(),
            follows,
            frame,
            words: Words::default(),
            started: None,
            layout: PresenterLayout::default(),
            shown: None,
            panel_blit: BlitPipeline::new(&context.device, context.compositing_format)?,
            slide_blit: BlitPipeline::new(&context.device, context.compositing_format)?,
            panel: None,
            asked: None,
            painter: Painter::start(),
            slide: None,
        })
    }

    #[must_use]
    pub fn follows(&self) -> &str {
        &self.follows
    }

    /// What its words say now.
    #[must_use]
    pub fn words(&self) -> &Words {
        &self.words
    }

    /// What the show it follows says now.
    pub fn set_show(&mut self, show: Option<&PresentationSnapshot>) {
        if self.started.is_none() && show.is_some_and(|p| p.status == PresentationStatus::Showing) {
            self.started = Some(Instant::now());
        }
        self.words = Words::of(show);
    }

    /// The talk's clock back to 0:00, from now.
    pub fn restart_clock(&mut self) {
        self.started = Some(Instant::now());
    }

    /// Where the live slide goes: where the panel shown left room for it —
    /// as now laid out until that is drawn.
    fn slide_rect(&self) -> Rect {
        match self.shown {
            Some((settings, aspect, painted)) => {
                // The panel is drawn at `painted` and stretched over the
                // frame: its placeholder, stretched with it.
                let slide = layout(painted, aspect, &settings).slide;
                #[allow(clippy::cast_precision_loss)]
                let (kx, ky) = (
                    self.frame.0.max(1) as f32 / painted.0.max(1) as f32,
                    self.frame.1.max(1) as f32 / painted.1.max(1) as f32,
                );
                Rect {
                    x: slide.x * kx,
                    y: slide.y * ky,
                    w: slide.w * kx,
                    h: slide.h * ky,
                }
            }
            None => layout(self.frame, self.aspect(), &self.layout).slide,
        }
    }

    /// How it is laid out (see [`PresenterLayout`]).
    #[must_use]
    pub fn layout(&self) -> PresenterLayout {
        self.layout
    }

    /// Laid out as `layout` has it: drawn again at the next frame.
    pub fn set_layout(&mut self, layout: PresenterLayout) {
        self.layout = layout.sanitized();
    }

    fn aspect(&self) -> f32 {
        #[allow(clippy::cast_precision_loss)]
        self.slide
            .as_ref()
            .map_or(16.0 / 9.0, |(_, (w, h))| *w as f32 / (*h).max(1) as f32)
    }

    /// Each frame: ask for the panel to be drawn again if what it says has
    /// changed, and take a drawing that is ready.
    pub fn tick(&mut self) {
        let elapsed = self.started.map_or_else(
            || "0:00".to_string(),
            |at| {
                let s = at.elapsed().as_secs();
                if s >= 3600 {
                    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
                } else {
                    format!("{}:{:02}", s / 60, s % 60)
                }
            },
        );
        let clock = chrono::Local::now().format("%H:%M").to_string();
        let frame = panel_size(self.frame);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let aspect_milli = (self.aspect() * 1000.0).round() as u32;
        // Compared field by field: nothing is copied while nothing changes.
        let same = self.asked.as_ref().is_some_and(|a| {
            a.words == self.words
                && a.elapsed == elapsed
                && a.clock == clock
                && a.frame == frame
                && a.aspect_milli == aspect_milli
                && a.layout == self.layout
        });
        if !same {
            let spec = PanelSpec {
                words: self.words.clone(),
                elapsed,
                clock,
                frame,
                aspect_milli,
                layout: self.layout,
            };
            self.painter.ask(spec.clone());
            self.asked = Some(spec);
        }
        if let Some((spec, drawing)) = self.painter.take() {
            #[allow(clippy::cast_precision_loss)]
            let aspect = spec.aspect_milli as f32 / 1000.0;
            self.shown = drawing.as_ref().map(|_| (spec.layout, aspect, spec.frame));
            self.show(drawing);
        }
    }

    fn show(&mut self, drawing: Option<Drawing>) {
        let Some((image, banner)) = drawing else {
            self.panel = None;
            return;
        };
        let image = &image;
        let size = image.dimensions();
        match &mut self.panel {
            Some(face) if face.size == size => {
                face.banner = banner;
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
                let bind_group = self
                    .panel_blit
                    .create_bind_group(&self.context.device, &view);
                self.panel = Some(Face {
                    size,
                    texture,
                    bind_group,
                    banner,
                });
            }
        }
    }

    /// Draw the frame — `target` pixels — : the panel, and the live slide in
    /// its place.
    pub fn render(
        &self,
        cmd_buffers: &mut Vec<wgpu::CommandBuffer>,
        output: &wgpu::TextureView,
        target: (u32, u32),
    ) {
        let slide = self.slide.as_ref().map(|(view, _)| {
            (
                self.slide_blit
                    .create_bind_group(&self.context.device, view),
                self.slide_rect(),
            )
        });
        let mut encoder =
            self.context
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Presenter Encoder"),
                });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Presenter Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: output,
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
            if let Some(face) = &self.panel {
                self.panel_blit
                    .set_uv_transform(&self.context.queue, 1.0, [1.0, 1.0], [0.0, 0.0]);
                self.panel_blit.render(&mut pass, &face.bind_group);
            }
            if let Some((bind_group, rect)) = &slide {
                #[allow(clippy::cast_precision_loss)]
                let frame = (self.frame.0.max(1) as f32, self.frame.1.max(1) as f32);
                self.slide_blit.set_uv_transform(
                    &self.context.queue,
                    1.0,
                    [frame.0 / rect.w, frame.1 / rect.h],
                    [-rect.x / rect.w, -rect.y / rect.h],
                );
                if let Some(within) = scissor(*rect, self.frame, target) {
                    self.slide_blit
                        .render_within(&mut pass, bind_group, Some(&[within]));
                }
            }
            // The banner over the slide's foot: the panel's again, there.
            if let Some((face, strip)) = self
                .panel
                .as_ref()
                .and_then(|face| face.banner.map(|strip| (face, strip)))
            {
                #[allow(clippy::cast_precision_loss)]
                let (kx, ky) = (
                    self.frame.0 as f32 / face.size.0.max(1) as f32,
                    self.frame.1 as f32 / face.size.1.max(1) as f32,
                );
                let on_frame = Rect {
                    x: strip.x * kx,
                    y: strip.y * ky,
                    w: strip.w * kx,
                    h: strip.h * ky,
                };
                if let Some(within) = scissor(on_frame, self.frame, target) {
                    self.panel_blit
                        .render_within(&mut pass, &face.bind_group, Some(&[within]));
                }
            }
        }
        cmd_buffers.push(encoder.finish());
    }

    /// The master's size changed: the panel is drawn again to suit.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.frame = (width.max(1), height.max(1));
    }
}

/// `rect` (frame pixels) as target pixels, for a target `target` big drawn
/// from a `frame`; `None` when nothing of it is left.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn scissor(rect: Rect, frame: (u32, u32), target: (u32, u32)) -> Option<Pixels> {
    let kx = target.0 as f32 / frame.0.max(1) as f32;
    let ky = target.1 as f32 / frame.1.max(1) as f32;
    let x0 = ((rect.x * kx).floor().max(0.0) as u32).min(target.0);
    let y0 = ((rect.y * ky).floor().max(0.0) as u32).min(target.1);
    let x1 = (((rect.x + rect.w) * kx).ceil().max(0.0) as u32).min(target.0);
    let y1 = (((rect.y + rect.h) * ky).ceil().max(0.0) as u32).min(target.1);
    (x1 > x0 && y1 > y0).then_some(Pixels { x0, y0, x1, y1 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show() -> PresentationSnapshot {
        PresentationSnapshot {
            path: "/Talks/talk.pptx".into(),
            status: PresentationStatus::Showing,
            message: None,
            slide: Some(3),
            slides: Some(24),
            position: Some(3),
            shown: Some(22),
            screen: Screen::Slide,
            title: Some("Results".into()),
            notes: "Thank the organisers.".into(),
            next_slide: Some(4),
            next_title: Some("Questions".into()),
            plays_on: crate::engine::value::presentation::PlaysOn::Auto,
            playing_on: None,
            slide_pixels: None,
            video: None,
        }
    }

    #[test]
    fn the_words_say_where_the_show_is_and_what_comes_next() {
        let words = Words::of(Some(&show()));
        assert_eq!(words.position, "3 / 22");
        assert_eq!(words.next, "Next: 4 · Questions");
        assert_eq!(words.banner, "");
        let mut last = show();
        last.next_slide = None;
        last.screen = Screen::Black;
        let words = Words::of(Some(&last));
        assert_eq!(
            (words.next.as_str(), words.banner.as_str()),
            ("Last slide", "BLACK")
        );
        let mut failed = show();
        failed.status = PresentationStatus::Failed;
        failed.message = Some("macOS does not allow it".into());
        assert_eq!(Words::of(Some(&failed)).banner, "macOS does not allow it");
        assert_eq!(Words::of(None).banner, "The PowerPoint input is gone");
    }

    #[test]
    fn the_slide_keeps_its_shape_and_leaves_room_for_the_notes() {
        for (frame, aspect) in [
            ((1920, 1080), 16.0 / 9.0),
            ((1920, 1080), 4.0 / 3.0),
            ((1280, 1024), 16.0 / 9.0),
            ((3840, 2160), 16.0 / 9.0),
        ] {
            let at = layout(frame, aspect, &PresenterLayout::default());
            #[allow(clippy::cast_precision_loss)]
            let (w, h) = (frame.0 as f32, frame.1 as f32);
            assert!(
                (at.slide.w / at.slide.h - aspect).abs() < 0.01,
                "{frame:?} {aspect}"
            );
            assert!(
                at.slide.x + at.slide.w < at.notes.x,
                "{frame:?}: slide into the notes"
            );
            assert!(
                at.notes.x + at.notes.w <= w && at.next_baseline <= h,
                "{frame:?}"
            );
            assert!(at.slide.y >= at.bar && at.slide.y + at.slide.h < at.next_baseline);
            assert!(at.notes.w > 0.3 * w, "{frame:?}: notes too narrow");
        }
    }

    #[test]
    fn notes_wrap_to_their_column_and_shrink_to_fit() {
        let fonts = super::super::svg::system_fonts();
        let mut widths = Widths::default();
        let notes = "Thank the organisers and the sponsors. Then the agenda, in three parts: \
                     what we found, what it means, and what we ask of you.\nPause for questions.";
        let lines = wrap(notes, 40.0, 600.0, &mut widths, &fonts);
        assert!(lines.len() >= 3, "{lines:?}");
        assert_eq!(
            lines.last().map(String::as_str),
            Some("Pause for questions.")
        );
        for line in &lines {
            assert!(
                widths.of(line, 40.0, false, &fonts) <= 600.0 + 1.0,
                "{line}"
            );
        }
        let words: Vec<&str> = notes.split_whitespace().collect();
        let rejoined: Vec<String> = lines
            .iter()
            .flat_map(|l| l.split_whitespace())
            .map(str::to_string)
            .collect();
        assert_eq!(rejoined, words, "no word lost or split");

        // Far too long for the column: smaller, then cut with "…".
        let long = "word ".repeat(2000);
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            w: 700.0,
            h: 800.0,
        };
        let (size, lines) = fit_notes(&long, rect, 1080.0, &mut widths, &fonts);
        assert!(size < 0.048 * 1080.0);
        #[allow(clippy::cast_precision_loss)]
        let height = lines.len() as f32 * size * 1.3;
        assert!(height <= rect.h + size * 1.3, "{height}");
        assert!(lines.last().is_some_and(|l| l.ends_with('…')));
    }

    /// The bar at the foot, what comes next left out, a wider slide: the
    /// slide and the notes take the room as asked; a share beyond bounds is
    /// brought into them.
    #[test]
    fn the_layout_follows_its_settings() {
        let frame = (1920, 1080);
        let aspect = 16.0 / 9.0;
        let with = |settings: PresenterLayout| layout(frame, aspect, &settings);
        let default = with(PresenterLayout::default());
        assert!(default.bar_y.abs() < f32::EPSILON, "the bar at the top");

        let foot = with(PresenterLayout {
            clocks_at: ClocksAt::Bottom,
            ..PresenterLayout::default()
        });
        assert!((foot.bar_y - 972.0).abs() < 0.5, "{}", foot.bar_y);
        assert!(foot.slide.y < default.slide.y, "the slide up top");
        assert!(foot.notes.y + foot.notes.h <= foot.bar_y);

        let wide = with(PresenterLayout {
            slide_share: 0.8,
            ..PresenterLayout::default()
        });
        assert!(wide.slide.w > default.slide.w);
        assert!(wide.notes.w < default.notes.w);
        let wide_no_next = with(PresenterLayout {
            slide_share: 0.8,
            next: false,
            ..PresenterLayout::default()
        });
        assert!(
            wide_no_next.slide.h > wide.slide.h,
            "the next line's room the slide's"
        );

        assert_eq!(
            with(PresenterLayout {
                slide_share: 5.0,
                ..PresenterLayout::default()
            }),
            with(PresenterLayout {
                slide_share: 0.8,
                ..PresenterLayout::default()
            })
        );
    }

    /// A line too wide for its room: smaller, down to half, then cut with
    /// "…" — never past the room.
    #[test]
    fn a_line_too_wide_is_made_smaller_then_cut() {
        let fonts = super::super::svg::system_fonts();
        let mut widths = Widths::default();
        let text = "Next: 4 · Questions and answers";
        let full = widths.of(text, 40.0, false, &fonts);
        let (size, line) = fit_line(text, 40.0, full * 0.8, &mut widths, &fonts);
        assert!((20.0..40.0).contains(&size));
        assert_eq!(line, text, "smaller, whole");
        let (size, line) = fit_line(text, 40.0, full * 0.3, &mut widths, &fonts);
        assert!((size - 20.0).abs() < 0.01, "{size}");
        assert!(line.ends_with('…'), "{line}");
        assert!(widths.of(&line, size, false, &fonts) <= full * 0.3);
    }

    /// On a tall screen with a narrow slide, the banner — two lines —
    /// stays within the slide.
    #[test]
    fn a_banner_stays_within_a_narrow_slide() {
        let fonts = super::super::svg::system_fonts();
        let settings = PresenterLayout {
            slide_share: 0.35,
            clocks_at: ClocksAt::Bottom,
            ..PresenterLayout::default()
        };
        let mut words = Words::of(Some(&show()));
        words.banner = "The slide show is not running: PowerPoint is opening the deck".into();
        let spec = PanelSpec {
            words,
            elapsed: "0:00".into(),
            clock: "09:00".into(),
            frame: (1080, 1920),
            aspect_milli: 1778,
            layout: settings,
        };
        let (_, banner) = paint(&spec, &fonts, &mut Widths::default(), &mut None).expect("drawn");
        let slide = layout((1080, 1920), 1.778, &settings).slide;
        let strip = banner.expect("a banner");
        assert!(strip.y >= slide.y - 0.5, "{strip:?} over {slide:?}");
    }

    /// The talk timer, the time of day and what comes next are drawn when
    /// wanted, and not when switched off.
    #[test]
    fn the_clocks_and_the_next_line_are_drawn_only_when_wanted() {
        let fonts = super::super::svg::system_fonts();
        let svg = |layout: PresenterLayout| {
            let spec = PanelSpec {
                words: Words::of(Some(&show())),
                elapsed: "12:34".into(),
                clock: "14:05".into(),
                frame: (960, 540),
                aspect_milli: 1778,
                layout,
            };
            panel_svg(&spec, &fonts, &mut Widths::default(), &mut None)
                .expect("drawn")
                .0
        };
        let next = Words::of(Some(&show())).next;
        assert!(!next.is_empty());
        let all = svg(PresenterLayout::default());
        assert!(all.contains(">12:34<") && all.contains(">14:05<"));
        assert!(all.contains(&escape(&next)));
        let none = svg(PresenterLayout {
            timer: false,
            clock: false,
            next: false,
            ..PresenterLayout::default()
        });
        assert!(!none.contains("12:34") && !none.contains("14:05"));
        assert!(!none.contains(&escape(&next)));
    }

    #[test]
    fn the_panel_fills_the_frame_and_leaves_the_slide_dark() {
        let fonts = super::super::svg::system_fonts();
        let spec = PanelSpec {
            words: Words::of(Some(&show())),
            elapsed: "12:34".into(),
            clock: "14:05".into(),
            frame: (960, 540),
            aspect_milli: 1778,
            layout: PresenterLayout::default(),
        };
        let (image, banner) =
            paint(&spec, &fonts, &mut Widths::default(), &mut None).expect("drawn");
        assert!(banner.is_none(), "no banner while the slide shows");
        assert_eq!(image.dimensions(), (960, 540));
        let at = layout((960, 540), 1.778, &PresenterLayout::default());
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let inside = image.get_pixel(
            (at.slide.x + at.slide.w / 2.0) as u32,
            (at.slide.y + at.slide.h / 2.0) as u32,
        );
        assert_eq!(inside.0, [0, 0, 0, 255], "the slide's place is black");
        assert!(image.pixels().all(|p| p.0[3] == 255), "opaque");
        // Some of the notes' column is lit by text.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let lit = (at.notes.x as u32..(at.notes.x + at.notes.w) as u32)
            .flat_map(|x| {
                (at.notes.y as u32..(at.notes.y + at.notes.h) as u32).map(move |y| (x, y))
            })
            .filter(|&(x, y)| image.get_pixel(x, y).0[0] > 200)
            .count();
        assert!(lit > 50, "{lit} bright pixels of notes");
    }

    #[test]
    fn a_long_address_in_the_notes_is_cut_to_the_column() {
        let fonts = super::super::svg::system_fonts();
        let mut widths = Widths::default();
        let url = "https://example.com/a/very/long/path/that/goes/on/and/on/without/a/single/space";
        let lines = wrap(
            &format!("See {url} after."),
            40.0,
            400.0,
            &mut widths,
            &fonts,
        );
        for line in &lines {
            assert!(widths.of(line, 40.0, false, &fonts) <= 401.0, "{line}");
        }
        assert_eq!(lines.concat().replace(' ', ""), format!("See{url}after."));
    }

    #[test]
    fn notes_cut_inside_a_long_address_still_end_within_the_column() {
        let fonts = super::super::svg::system_fonts();
        let mut widths = Widths::default();
        let url = format!("https://example.com/{}", "segment".repeat(60));
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            w: 500.0,
            h: 200.0,
        };
        let (size, lines) = fit_notes(&url, rect, 1080.0, &mut widths, &fonts);
        let last = lines.last().expect("some of it shows");
        assert!(last.ends_with('…'), "{last}");
        for line in &lines {
            assert!(
                widths.of(line, size, false, &fonts) <= rect.w + 1.0,
                "{line}"
            );
        }
    }

    #[test]
    fn a_banner_over_the_slide_is_drawn_again_over_it() {
        let fonts = super::super::svg::system_fonts();
        let mut black = show();
        black.screen = Screen::Black;
        let spec = PanelSpec {
            words: Words::of(Some(&black)),
            elapsed: "1:00".into(),
            clock: "09:00".into(),
            frame: (960, 540),
            aspect_milli: 1778,
            layout: PresenterLayout::default(),
        };
        let (_, banner) = paint(&spec, &fonts, &mut Widths::default(), &mut None).expect("drawn");
        let strip = banner.expect("a banner strip");
        let at = layout((960, 540), 1.778, &PresenterLayout::default());
        assert!(strip.y >= at.slide.y && strip.y + strip.h <= at.slide.y + at.slide.h + 0.5);
        // A long reason takes two lines, not more.
        let mut failed = show();
        failed.status = PresentationStatus::Failed;
        failed.message = Some("macOS does not allow Nida to control PowerPoint: System Settings → Privacy & Security → Automation → Nida → Microsoft PowerPoint".into());
        let spec = PanelSpec {
            words: Words::of(Some(&failed)),
            ..spec
        };
        let (_, banner) = paint(&spec, &fonts, &mut Widths::default(), &mut None).expect("drawn");
        let tall = banner.expect("a banner strip").h;
        assert!(
            tall > strip.h && tall < strip.h * 2.0,
            "{tall} against {}",
            strip.h
        );
    }

    #[test]
    fn a_huge_master_draws_its_panel_smaller_and_keeps_it() {
        assert_eq!(panel_size((1920, 1080)), (1920, 1080));
        assert_eq!(panel_size((16384, 9216)), (4096, 2304));
        assert_eq!(panel_size((2160, 7680)), (1152, 4096));
    }

    #[test]
    fn the_slide_is_cut_to_its_place_in_the_targets_pixels() {
        let rect = Rect {
            x: 27.0,
            y: 135.0,
            w: 1100.0,
            h: 618.75,
        };
        assert_eq!(
            scissor(rect, (1920, 1080), (1920, 1080)),
            Some(Pixels {
                x0: 27,
                y0: 135,
                x1: 1127,
                y1: 754
            })
        );
        // Drawn at half the master's size.
        assert_eq!(
            scissor(rect, (1920, 1080), (960, 540)),
            Some(Pixels {
                x0: 13,
                y0: 67,
                x1: 564,
                y1: 377
            })
        );
        assert_eq!(
            scissor(
                Rect {
                    x: 2000.0,
                    y: 0.0,
                    w: 10.0,
                    h: 10.0
                },
                (1920, 1080),
                (1920, 1080)
            ),
            None
        );
    }
}
