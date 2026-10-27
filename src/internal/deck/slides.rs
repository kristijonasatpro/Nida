//! A Slides input's layer: a conference's presentations in running order,
//! one on at a time (see [`crate::engine::value::slides`]).
//!
//! Each talk on is a deck of its own drawn at the layer's size, as a
//! playlist's items are: a PowerPoint show's window or a web presentation's,
//! captured (the app finds and binds the capture, see
//! `app::state::slides`); a PDF's page, drawn on a thread; a clip; a picture.
//! A talk started is the *coming* one until its first picture is there —
//! what is on stays on meanwhile, so the room never sees black or a desktop —
//! then it takes over by the input's cut, fade or slide.
//!
//! Only the talk on and the one coming are open here. The talk marked next
//! and the one before the talk on are opened ahead by what plays them — the
//! deck open in PowerPoint, the page loaded — so starting one is quick.

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Instant;

use super::{Deck, ScalingMode};
use crate::engine::value::playlist::PlaylistTransition;
use crate::engine::value::presentation::{Go, Screen};
use crate::engine::value::slides::{ClipPosition, SlidesSettings, Talk, TalkKind, TalkRef};
use crate::presentation::pdf::{PdfDrawer, PdfEvent};
use crate::renderer::playlist_mix::{Mix, PlaylistMixPipeline};
use crate::renderer::GpuContext;

/// The longest step time takes between two looks: a stall does not jump a
/// fade.
const MAX_STEP: f64 = 0.25;

/// A talk open as a deck.
struct OpenTalk {
    id: String,
    /// The file it was opened with: another put in the list while it is on
    /// is played the next time it is started.
    path: String,
    kind: TalkKind,
    /// The name it had, kept with the show should it leave the list.
    name: String,
    /// Where it is: the slide (a PDF's page) it would start at again.
    slide: Option<u32>,
    deck: Box<Deck>,
    pdf: Option<PdfTalk>,
}

/// A clip or picture talk being opened on a thread.
struct Opening {
    id: String,
    path: String,
    name: String,
    slide: Option<u32>,
    done: mpsc::Receiver<anyhow::Result<Deck>>,
}

/// A PDF talk's pages: drawn on a thread, the page shown and those either
/// side kept.
struct PdfTalk {
    drawer: PdfDrawer,
    pages: Option<u32>,
    /// The page wanted on screen, from 1.
    page: u32,
    /// The page whose picture the deck has.
    shown: Option<u32>,
    drawn: HashMap<u32, image::RgbaImage>,
    /// The size pages were asked at.
    asked_size: (u32, u32),
}

impl PdfTalk {
    fn open(path: &str, page: u32, size: (u32, u32)) -> Self {
        let drawer = PdfDrawer::open(path.into());
        let page = page.max(1);
        drawer.want(around(page, None), size);
        Self {
            drawer,
            pages: None,
            page,
            shown: None,
            drawn: HashMap::new(),
            asked_size: size,
        }
    }

    /// Ask for the page on screen and those either side of it.
    fn ask(&mut self, size: (u32, u32)) {
        if size != self.asked_size {
            self.drawn.clear();
            self.shown = None;
            self.asked_size = size;
        }
        let wanted: Vec<u32> = around(self.page, self.pages)
            .into_iter()
            .filter(|p| !self.drawn.contains_key(p))
            .collect();
        if !wanted.is_empty() {
            self.drawer.want(wanted, size);
        }
    }

    /// Take what the thread drew; give back why it failed, if it did.
    fn take(&mut self) -> Option<String> {
        let mut failed = None;
        let mut clamped = false;
        for event in self.drawer.poll() {
            match event {
                PdfEvent::Opened { pages, .. } => {
                    self.pages = Some(pages);
                    if self.page > pages {
                        // Saved further on than the file now goes: its last
                        // page, asked for afresh.
                        self.page = pages;
                        clamped = true;
                    }
                }
                PdfEvent::Failed(why) => failed = Some(why),
                PdfEvent::Drawn { page, size, image } if size == self.asked_size => {
                    self.drawn.insert(page, image);
                }
                PdfEvent::Drawn { .. } => {}
                PdfEvent::DrawFailed { page, message } => {
                    if page == self.page {
                        failed = Some(format!("page {page}: {message}"));
                    }
                }
            }
        }
        // Only the page shown and its neighbours are kept.
        let keep = around(self.page, self.pages);
        self.drawn.retain(|p, _| keep.contains(p));
        if clamped {
            self.ask(self.asked_size);
        }
        failed
    }
}

/// The page `page` and those either side, within `pages`, that first.
fn around(page: u32, pages: Option<u32>) -> Vec<u32> {
    let last = pages.unwrap_or(u32::MAX).max(1);
    let page = page.clamp(1, last);
    let mut out = vec![page];
    if page < last {
        out.push(page + 1);
    }
    if page > 1 {
        out.push(page - 1);
    }
    out
}

/// What the app keeps open for a talk: a PowerPoint deck opened ahead or
/// its show running, a web page loaded or captured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WantedTalk {
    pub id: String,
    pub path: String,
    pub kind: TalkKind,
    /// On or coming: its show runs and its window is captured. Else opened
    /// ahead only.
    pub show: bool,
    /// Its picture shows: the talk on, or the one coming once it is being
    /// brought on — a web talk's video and sound play from then.
    pub on: bool,
    /// The slide it starts at.
    pub slide: Option<u32>,
}

/// The talk a saved show had on, to start again as it opens: by its id,
/// with its own record when it had been taken out of the list (and where
/// in the list it was).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resume {
    pub id: String,
    pub unlisted: Option<Talk>,
    pub at: Option<usize>,
}

/// A talk open — on or coming — as the app reads it to bind its capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenTalkInfo {
    pub id: String,
    /// The file it was opened with.
    pub path: String,
    pub kind: TalkKind,
    pub capture: Option<crate::screen_capture::CaptureId>,
}

/// Where the layer has got, as the app and the panel read it.
#[derive(Debug, Clone, PartialEq)]
pub struct SlidesState {
    pub live: Option<String>,
    pub coming: Option<String>,
    pub next: Option<String>,
    pub next_picked: bool,
    pub previous: Option<String>,
    pub failed: HashMap<String, String>,
    /// A talk open, by talk and file (the talk on and a new file of it
    /// starting are two): a PDF's page and pages, a web page's slide.
    pub pages: HashMap<(String, String), (u32, Option<u32>)>,
    pub clip: HashMap<(String, String), ClipPosition>,
    pub talk_secs: Option<f64>,
    pub screen: Screen,
}

pub struct Slides {
    context: GpuContext,
    /// The size talks are drawn at: the master's.
    size: (u32, u32),
    talks: Vec<Talk>,
    settings: SlidesSettings,
    scaling_mode: ScalingMode,
    live: Option<OpenTalk>,
    coming: Option<OpenTalk>,
    opening: Option<Opening>,
    /// Seconds into the change from the talk on to the coming one.
    transition: Option<f64>,
    /// The talk picked to come next, by hand.
    next_pick: Option<String>,
    /// The talk on before the one on now.
    previous: Option<String>,
    /// Talks that could not be played, and why: until started again or the
    /// list changes.
    failed: HashMap<String, String>,
    /// Where the talk on was, by place, when it was taken out of the list:
    /// what comes next is counted from there.
    gone_at: Option<usize>,
    /// The talk a saved show had on, until it is on again or another is
    /// started: kept with the show though its file was not there to open —
    /// with its own record, when it had been taken out of the list.
    resume: Option<String>,
    resume_unlisted: Option<(Talk, Option<usize>)>,
    /// Where each web talk's page says it is: its slide and slides.
    positions: HashMap<(String, String), (u32, u32)>,
    started_at: Option<Instant>,
    last_step: Option<Instant>,
    screen: Screen,
    mix: PlaylistMixPipeline,
    black: (wgpu::Texture, wgpu::TextureView),
    white: (wgpu::Texture, wgpu::TextureView),
    output_fps: std::sync::atomic::AtomicU32,
}

impl Slides {
    /// A Slides layer of `talks`, nothing on — or, opened from a saved
    /// show, `resume` started at once where it was.
    pub fn new(
        context: &GpuContext,
        talks: Vec<Talk>,
        settings: SlidesSettings,
        (width, height): (u32, u32),
        resume: Option<Resume>,
        next_pick: Option<String>,
    ) -> Self {
        let mut slides = Self {
            context: context.clone(),
            size: (width.max(1), height.max(1)),
            talks,
            settings: settings.clamped(),
            scaling_mode: ScalingMode::Fit,
            live: None,
            coming: None,
            opening: None,
            transition: None,
            next_pick: None,
            previous: None,
            failed: HashMap::new(),
            gone_at: None,
            resume: None,
            resume_unlisted: None,
            positions: HashMap::new(),
            started_at: None,
            last_step: None,
            screen: Screen::Slide,
            mix: PlaylistMixPipeline::new(&context.device, context.compositing_format),
            black: solid(context, [0, 0, 0, 255]),
            white: solid(context, [255, 255, 255, 255]),
            output_fps: std::sync::atomic::AtomicU32::new(0),
        };
        slides.next_pick = next_pick.filter(|id| slides.index_of(id).is_some());
        if let Some(resume) = resume {
            slides.resume = Some(resume.id.clone());
            match resume.unlisted {
                // On when saved though taken out of the list: on again.
                Some(talk) if slides.index_of(&talk.id).is_none() => {
                    slides.gone_at = resume.at;
                    slides.resume_unlisted = Some((talk.clone(), resume.at));
                    let _ = slides.open_talk(&talk);
                }
                _ => {
                    let _ = slides.start(&TalkRef::Id(resume.id));
                }
            }
        }
        slides
    }

    pub fn talks(&self) -> &[Talk] {
        &self.talks
    }

    pub fn settings(&self) -> &SlidesSettings {
        &self.settings
    }

    pub fn scaling_mode(&self) -> ScalingMode {
        self.scaling_mode
    }

    /// Every talk's deck is scaled this way.
    pub fn set_scaling_mode(&mut self, mode: ScalingMode) {
        self.scaling_mode = mode;
        for open in self.live.iter_mut().chain(self.coming.iter_mut()) {
            open.deck.set_scaling_mode(mode);
        }
    }

    pub fn set_settings(&mut self, settings: SlidesSettings) {
        self.settings = settings.clamped();
    }

    /// The renderer shows `fps` frames a second (0: as fast as it can).
    pub fn set_output_fps(&self, fps: u32) {
        self.output_fps
            .store(fps, std::sync::atomic::Ordering::Relaxed);
    }

    fn index_of(&self, id: &str) -> Option<usize> {
        self.talks.iter().position(|t| t.id == id)
    }

    fn talk(&self, id: &str) -> Option<&Talk> {
        self.talks.iter().find(|t| t.id == id)
    }

    /// The talk on: its id and what plays it.
    pub fn live_talk(&self) -> Option<(&str, TalkKind)> {
        self.live.as_ref().map(|o| (o.id.as_str(), o.kind))
    }

    /// The talk being started.
    pub fn coming_talk(&self) -> Option<(&str, TalkKind)> {
        self.coming
            .as_ref()
            .map(|o| (o.id.as_str(), o.kind))
            .or_else(|| {
                self.opening.as_ref().and_then(|o| {
                    let kind = TalkKind::of(&o.path)?;
                    Some((o.id.as_str(), kind))
                })
            })
    }

    /// What Start next takes: the talk picked to come next, else the one
    /// after the talk on (or, nothing on, the first).
    pub fn next_talk(&self) -> Option<String> {
        let busy = |id: &str| {
            self.live.as_ref().is_some_and(|o| o.id == id)
                || self.coming_talk().is_some_and(|(c, _)| c == id)
        };
        if let Some(id) = &self.next_pick {
            if self.index_of(id).is_some() && !busy(id) {
                return Some(id.clone());
            }
        }
        // Counted from the talk being started, else the one on.
        let from = self
            .coming_talk()
            .map(|(id, _)| id.to_string())
            .or_else(|| self.live.as_ref().map(|o| o.id.clone()));
        let after = match from.as_deref().and_then(|id| self.index_of(id)) {
            Some(i) => i + 1,
            None if from.is_some() => self.gone_at.unwrap_or(0),
            None => 0,
        };
        self.talks
            .get(after..)
            .unwrap_or_default()
            .iter()
            .find(|t| !busy(&t.id))
            .map(|t| t.id.clone())
    }

    /// The talk on before the one on now, while it is in the list.
    pub fn previous_talk(&self) -> Option<&str> {
        self.previous
            .as_deref()
            .filter(|id| self.index_of(id).is_some())
    }

    /// A new list: talks added, taken out, moved, renamed, given another
    /// file. The talk on stays on — taken out of the list, until another is
    /// started.
    pub fn set_talks(&mut self, talks: Vec<Talk>) {
        let old_live_at = self.live.as_ref().and_then(|o| self.index_of(&o.id));
        // Where each talk was left is the show's, not the list's: kept — by
        // talk and file, so a talk given another file starts it at its own
        // beginning, not where the old file was left.
        let slides: HashMap<(String, String), Option<u32>> = self
            .talks
            .iter()
            .map(|t| ((t.id.clone(), t.path.clone()), t.slide))
            .collect();
        self.talks = talks;
        for talk in &mut self.talks {
            if let Some(slide) = slides.get(&(talk.id.clone(), talk.path.clone())) {
                talk.slide = *slide;
            }
            // A talk on keeps where it has got to — moved on while out of
            // the list, and put back by an undo, too — its own file's.
            if let Some(open) = self
                .live
                .iter()
                .chain(self.coming.iter())
                .find(|o| o.id == talk.id && o.path == talk.path && o.slide.is_some())
            {
                talk.slide = open.slide;
            }
        }
        self.failed.clear();
        if let Some(open) = &self.live {
            match self.talks.iter().position(|t| t.id == open.id) {
                Some(_) => self.gone_at = None,
                None => self.gone_at = self.gone_at.or(old_live_at),
            }
        }
        // A talk being started that is gone, or given another file, is
        // started no more.
        let stale = |id: &str, path: &str, talks: &[Talk]| {
            !talks.iter().any(|t| t.id == id && t.path == path)
        };
        if self
            .coming
            .as_ref()
            .is_some_and(|o| stale(&o.id, &o.path, &self.talks))
        {
            self.transition = None;
            if let Some(open) = self.coming.take() {
                let_go(open.deck);
            }
        }
        if self
            .opening
            .as_ref()
            .is_some_and(|o| stale(&o.id, &o.path, &self.talks))
        {
            self.opening = None;
        }
        if self
            .next_pick
            .as_deref()
            .is_some_and(|id| self.index_of(id).is_none())
        {
            self.next_pick = None;
        }
    }

    /// The talk picked to come next, or none: NEXT goes back to the talk
    /// after the one on.
    ///
    /// # Errors
    ///
    /// No such talk in the list.
    pub fn pick_next(&mut self, id: Option<String>) -> Result<(), String> {
        if let Some(id) = &id {
            if self.index_of(id).is_none() {
                return Err(format!("No talk {id} in the list"));
            }
        }
        self.next_pick = id;
        Ok(())
    }

    /// Start a talk: what is on stays until its picture is there. Starting
    /// the talk on does nothing; starting another while one is being
    /// started starts that one instead.
    ///
    /// # Errors
    ///
    /// Which talk was meant cannot be told, or it cannot be played: no file
    /// there, or a kind nothing plays.
    pub fn start(&mut self, to: &TalkRef) -> Result<String, String> {
        let id = match to {
            TalkRef::Next => self.next_talk().ok_or("No talk comes next")?,
            TalkRef::Previous => self
                .previous_talk()
                .map(str::to_string)
                .ok_or("No talk was on before this one")?,
            TalkRef::Id(id) => id.clone(),
        };
        let talk = self
            .talk(&id)
            .cloned()
            .ok_or_else(|| format!("No talk {id} in the list"))?;
        // On already, with the file in the list: nothing to start. Given
        // another file since, it starts again with that one — what is on
        // stays on until the new file's picture is there.
        if self
            .live
            .as_ref()
            .is_some_and(|o| o.id == id && o.path == talk.path)
        {
            self.drop_coming();
            return Ok(id);
        }
        let opening_it = self.opening.as_ref().is_some_and(|o| o.id == id && o.path == talk.path);
        let coming_it = self.coming.as_ref().is_some_and(|o| o.id == id && o.path == talk.path);
        if opening_it || coming_it {
            return Ok(id);
        }
        self.open_talk(&talk)?;
        Ok(id)
    }

    /// Open `talk` as the coming one, whether in the list or not (the talk a
    /// saved show had on, taken out of the list).
    fn open_talk(&mut self, talk: &Talk) -> Result<(), String> {
        let id = talk.id.clone();
        self.drop_coming();
        self.failed.remove(&id);
        if self.next_pick.as_deref() == Some(id.as_str()) {
            self.next_pick = None;
        }
        let Some(kind) = talk.kind() else {
            return Err(self.fail(&id, "Nida does not play this kind of file".into()));
        };
        if !std::path::Path::new(&talk.path).is_file() {
            return Err(self.fail(&id, format!("its file is not there: {}", talk.path)));
        }
        let open = |deck: Deck, pdf: Option<PdfTalk>| OpenTalk {
            id: id.clone(),
            path: talk.path.clone(),
            kind,
            name: talk.name.clone(),
            slide: talk.slide,
            deck: Box::new(deck),
            pdf,
        };
        match kind {
            TalkKind::PowerPoint | TalkKind::Web => {
                let deck = capture_deck(&self.context, self.size, self.scaling_mode, talk)
                    .map_err(|e| self.fail(&id, format!("{e:#}")))?;
                self.coming = Some(open(deck, None));
            }
            TalkKind::Pdf => {
                let deck = picture_deck(&self.context, self.size, self.scaling_mode, talk)
                    .map_err(|e| self.fail(&id, format!("{e:#}")))?;
                let pdf = PdfTalk::open(&talk.path, talk.slide.unwrap_or(1), self.size);
                self.coming = Some(open(deck, Some(pdf)));
            }
            TalkKind::Clip | TalkKind::Picture => self.open_file(talk),
        }
        Ok(())
    }

    /// Note why talk `id` cannot play, and give it back.
    fn fail(&mut self, id: &str, why: String) -> String {
        log::warn!("Slides: talk {id} cannot play: {why}");
        self.failed.insert(id.to_string(), why.clone());
        why
    }

    fn drop_coming(&mut self) {
        self.transition = None;
        if let Some(open) = self.coming.take() {
            let_go(open.deck);
        }
        // Its thread finishes on its own; what it opens is let go then.
        self.opening = None;
    }

    /// Open a clip or picture talk on a thread.
    fn open_file(&mut self, talk: &Talk) {
        let (id, path) = (talk.id.as_str(), talk.path.as_str());
        let (tx, done) = mpsc::channel();
        let context = self.context.clone();
        let size = self.size;
        let scaling_mode = self.scaling_mode;
        let file = path.to_string();
        let spawned = std::thread::Builder::new()
            .name("slides-open".into())
            .spawn(move || {
                let opened = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    open_file_deck(&context, &file, size, scaling_mode)
                }))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("opening it crashed")));
                let _ = tx.send(opened);
            });
        if let Err(e) = spawned {
            self.fail(id, format!("could not start opening it: {e}"));
            return;
        }
        self.opening = Some(Opening {
            id: id.to_string(),
            path: path.to_string(),
            name: talk.name.clone(),
            slide: talk.slide,
            done,
        });
    }

    /// Take the clip or picture opened, if it has.
    fn take_opened(&mut self) {
        let Some(opening) = &self.opening else {
            return;
        };
        let result = match opening.done.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err(anyhow::anyhow!("opening it stopped")),
        };
        let Some(opening) = self.opening.take() else {
            return;
        };
        match result {
            Ok(deck) => {
                let kind = TalkKind::of(&opening.path).unwrap_or(TalkKind::Picture);
                self.coming = Some(OpenTalk {
                    id: opening.id,
                    path: opening.path,
                    kind,
                    name: opening.name,
                    slide: opening.slide,
                    deck: Box::new(deck),
                    pdf: None,
                });
            }
            Err(e) => {
                self.fail(&opening.id, format!("{e:#}"));
            }
        }
    }

    /// Whether talk `open` has its picture: a capture's first, a PDF's
    /// page, a file opened.
    fn ready(open: &OpenTalk) -> bool {
        match open.kind {
            TalkKind::PowerPoint | TalkKind::Web => open.deck.external_source_view.is_some(),
            TalkKind::Pdf => open.pdf.as_ref().is_some_and(|p| p.shown.is_some()),
            // A clip's first frame decoded and put in its texture.
            TalkKind::Clip => open.deck.has_video_frame(),
            TalkKind::Picture => true,
        }
    }

    /// Step the talk on, when it is one stepped here — a PDF. PowerPoint
    /// and web talks are stepped by what plays them (the app); a clip or a
    /// picture has no steps. Gives back whether it was stepped here.
    pub fn go(&mut self, to: Go) -> bool {
        let Some(open) = &mut self.live else {
            return false;
        };
        let Some(pdf) = &mut open.pdf else {
            return false;
        };
        let last = pdf.pages.unwrap_or(pdf.page);
        pdf.page = match to {
            Go::Next => (pdf.page + 1).min(last),
            Go::Previous => pdf.page.saturating_sub(1).max(1),
            Go::First => 1,
            Go::Last => last,
            Go::Slide(n) => n.clamp(1, last.max(1)),
        };
        let page = pdf.page;
        let size = self.size;
        pdf.ask(size);
        let (id, path) = (open.id.clone(), open.path.clone());
        self.set_talk_slide(&id, &path, page);
        true
    }

    /// What the input shows over its talk: the slide, or black or white.
    pub fn screen(&self) -> Screen {
        self.screen
    }

    /// Black or white over the talk on, or the slide again.
    pub fn set_screen(&mut self, screen: Screen) {
        self.screen = match screen {
            Screen::Black | Screen::White => screen,
            Screen::Slide | Screen::Ended => Screen::Slide,
        };
    }

    /// Where talk `id` was left: the slide it starts at next time.
    ///
    /// By the file too: a talk given another file while on has its old
    /// file's place and its new one's apart.
    pub fn set_talk_slide(&mut self, id: &str, path: &str, slide: u32) {
        let slide = Some(slide.max(1));
        if let Some(talk) = self.talks.iter_mut().find(|t| t.id == id && t.path == path) {
            talk.slide = slide;
        }
        for open in self.live.iter_mut().chain(self.coming.iter_mut()) {
            if open.id == id && open.path == path {
                open.slide = slide;
            }
        }
    }

    /// Where a web talk's page says it is (reveal.js): its slide and
    /// how many it has; kept as where it was left.
    pub fn set_position(&mut self, id: &str, path: &str, at: u32, of: u32) {
        self.positions.insert((id.to_string(), path.to_string()), (at, of));
        self.set_talk_slide(id, path, at);
    }

    /// What plays talk `id`'s file `path` could not play it, and why: said,
    /// and the talk being started with that file is started no more — what
    /// is on stays on. Another file of the talk is not what failed: the one
    /// on, given a new file since, failing leaves the new one starting.
    pub fn talk_failed(&mut self, id: &str, path: &str, why: String) {
        let starting = self
            .coming
            .as_ref()
            .map(|o| (o.id.as_str(), o.path.as_str()))
            .or_else(|| self.opening.as_ref().map(|o| (o.id.as_str(), o.path.as_str())));
        let coming = starting == Some((id, path));
        // Said on the talk when it is the file the list has for it, or the
        // one starting; else only logged.
        if coming || self.talks.iter().any(|t| t.id == id && t.path == path) {
            self.fail(id, why);
        } else {
            log::warn!("Slides: talk {id}'s earlier file {path} cannot play: {why}");
        }
        if coming {
            self.drop_coming();
        }
    }

    /// What the app keeps open: the talk on and the one coming, their shows
    /// running; the next and the previous, opened ahead.
    pub fn wanted(&self) -> Vec<WantedTalk> {
        let mut out: Vec<WantedTalk> = Vec::new();
        let add = |out: &mut Vec<WantedTalk>, id: &str, show: bool| {
            if out.iter().any(|w| w.id == id) {
                return;
            }
            let Some(talk) = self.talk(id) else {
                // Taken out of the list while on: still played.
                if let Some(open) = self
                    .live
                    .iter()
                    .chain(self.coming.iter())
                    .find(|o| o.id == id)
                {
                    out.push(WantedTalk {
                        id: id.to_string(),
                        path: open.path.clone(),
                        kind: open.kind,
                        show,
                        on: false,
                        slide: open.slide,
                    });
                }
                return;
            };
            let Some(kind) = talk.kind() else {
                return;
            };
            if self.failed.contains_key(id) && !show {
                return;
            }
            out.push(WantedTalk {
                id: id.to_string(),
                path: talk.path.clone(),
                kind,
                show,
                on: false,
                slide: talk.slide,
            });
        };
        // The talks on and coming as they were opened: a file put in the
        // list for them since plays the next time they are started.
        for open in self.live.iter().chain(self.coming.iter()) {
            if out.iter().any(|w| w.id == open.id && w.path == open.path) {
                continue;
            }
            let live = self.live.as_ref().is_some_and(|l| std::ptr::eq(l, open));
            out.push(WantedTalk {
                id: open.id.clone(),
                path: open.path.clone(),
                kind: open.kind,
                show: true,
                on: live || self.transition.is_some(),
                slide: open.slide,
            });
        }
        if let Some(opening) = &self.opening {
            if let Some(kind) = TalkKind::of(&opening.path) {
                out.push(WantedTalk {
                    id: opening.id.clone(),
                    path: opening.path.clone(),
                    kind,
                    show: true,
                    on: false,
                    slide: opening.slide,
                });
            }
        }
        if let Some(next) = self.next_talk() {
            add(&mut out, &next, false);
        }
        if let Some(previous) = self.previous_talk() {
            add(&mut out, previous, false);
        }
        out
    }

    /// The deck of talk `id` while it is on or coming, for its capture to be
    /// bound.
    pub fn talk_deck_mut(&mut self, id: &str, path: &str) -> Option<&mut Deck> {
        self.live
            .iter_mut()
            .chain(self.coming.iter_mut())
            .find(|o| o.id == id && o.path == path)
            .map(|o| o.deck.as_mut())
    }

    /// The talk on as it was opened — whether still in the list, or given
    /// another file there since: its id, file, what plays it and its name.
    pub fn live_record(&self) -> Option<(&str, &str, TalkKind, &str)> {
        self.live
            .as_ref()
            .map(|o| (o.id.as_str(), o.path.as_str(), o.kind, o.name.as_str()))
    }

    /// The file talk `id` plays while it is on (else coming): the one it
    /// was opened with.
    pub fn open_path(&self, id: &str) -> Option<&str> {
        self.live
            .iter()
            .chain(self.coming.iter())
            .find(|o| o.id == id)
            .map(|o| o.path.as_str())
    }

    /// The decks open: the talk on and the one coming.
    pub fn for_each_deck(&self, mut f: impl FnMut(&Deck)) {
        for open in self.live.iter().chain(self.coming.iter()) {
            f(&open.deck);
        }
    }

    pub fn for_each_deck_mut(&mut self, mut f: impl FnMut(&mut Deck)) {
        for open in self.live.iter_mut().chain(self.coming.iter_mut()) {
            f(&mut open.deck);
        }
    }

    /// The talks open — on and coming — what plays each, and its capture
    /// when it has one bound.
    pub fn open_talks(&self) -> Vec<OpenTalkInfo> {
        self.live
            .iter()
            .chain(self.coming.iter())
            .map(|o| OpenTalkInfo {
                id: o.id.clone(),
                path: o.path.clone(),
                kind: o.kind,
                capture: o
                    .deck
                    .screen_capture_id()
                    .filter(|id| *id != crate::screen_capture::UNBOUND_CAPTURE_ID),
            })
            .collect()
    }

    /// The talk on, as kept with the show when it is no longer in the list:
    /// its record, and where it was in the list.
    pub fn unlisted_live(&self) -> Option<(Talk, Option<usize>)> {
        let Some(open) = self.live.as_ref() else {
            // Not open yet — its file not there as the show opened: as
            // saved, until another talk is started.
            return self
                .resume_unlisted
                .clone()
                .filter(|(talk, _)| self.index_of(&talk.id).is_none());
        };
        if self.index_of(&open.id).is_some() {
            return None;
        }
        Some((
            Talk {
                id: open.id.clone(),
                path: open.path.clone(),
                name: open.name.clone(),
                slide: open.slide,
            },
            self.gone_at,
        ))
    }

    /// The talk on — or one a saved show had on that could not be opened
    /// yet — as kept with the show.
    pub fn saved_live(&self) -> Option<&str> {
        self.live
            .as_ref()
            .map(|o| o.id.as_str())
            .or(self.resume.as_deref())
    }

    /// The talks' captures held: the talk on's and the coming one's.
    pub fn capture_ids(&self) -> Vec<crate::screen_capture::CaptureId> {
        self.live
            .iter()
            .chain(self.coming.iter())
            .filter_map(|o| o.deck.screen_capture_id())
            .filter(|id| *id != crate::screen_capture::UNBOUND_CAPTURE_ID)
            .collect()
    }

    /// Once a frame: take what has opened and been drawn, move a change of
    /// talk on, and bring each clip's newest frame to its texture.
    pub fn tick(&mut self, encoder: &mut wgpu::CommandEncoder) {
        self.step(Instant::now());
        for open in self.live.iter_mut().chain(self.coming.iter_mut()) {
            if let Err(e) = open.deck.update_video_frame(encoder) {
                log::warn!("Slides talk '{}': {e}", open.path);
            }
        }
    }

    /// Move on to `now`.
    pub fn step(&mut self, now: Instant) {
        let dt = self.last_step.map_or(0.0, |last| {
            now.saturating_duration_since(last)
                .as_secs_f64()
                .min(MAX_STEP)
        });
        self.last_step = Some(now);
        self.take_opened();
        self.take_pages();
        if let Some(elapsed) = &mut self.transition {
            *elapsed += dt;
            if *elapsed >= self.settings.transition_time() {
                self.finish_transition(now);
            }
            return;
        }
        if self.coming.as_ref().is_some_and(Self::ready) {
            self.start_transition(now);
        }
    }

    /// Bring each PDF talk's new pages to its deck.
    fn take_pages(&mut self) {
        let size = self.size;
        let context = self.context.clone();
        let mut failures = Vec::new();
        let mut shown = Vec::new();
        for open in self.live.iter_mut().chain(self.coming.iter_mut()) {
            let Some(pdf) = &mut open.pdf else {
                continue;
            };
            if let Some(why) = pdf.take() {
                failures.push((open.id.clone(), why));
            }
            if pdf.shown != Some(pdf.page) {
                if let Some(image) = pdf.drawn.get(&pdf.page) {
                    open.deck.replace_picture(&context, image);
                    pdf.shown = Some(pdf.page);
                    pdf.ask(size);
                    shown.push((open.id.clone(), open.path.clone(), pdf.page));
                }
            }
        }
        for (id, path, page) in shown {
            self.set_talk_slide(&id, &path, page);
        }
        for (id, why) in failures {
            let coming = self.coming.as_ref().is_some_and(|o| o.id == id);
            self.fail(&id, why);
            if coming {
                self.drop_coming();
            }
        }
    }

    fn start_transition(&mut self, now: Instant) {
        let Some(coming) = &self.coming else {
            return;
        };
        if matches!(coming.kind, TalkKind::Clip) {
            coming.deck.video_set_playing(true);
        }
        self.started_at = Some(now);
        self.transition = Some(0.0);
        if self.settings.transition_time() <= 0.0 || self.live.is_none() {
            self.finish_transition(now);
        }
    }

    fn finish_transition(&mut self, now: Instant) {
        self.transition = None;
        let Some(coming) = self.coming.take() else {
            return;
        };
        if self.started_at.is_none() {
            self.started_at = Some(now);
        }
        let new_id = coming.id.clone();
        if let Some(old) = self.live.replace(coming) {
            // Started again with another file: still the talk on, not the
            // one before it.
            if old.id != new_id && self.index_of(&old.id).is_some() {
                self.previous = Some(old.id.clone());
            }
            let_go(old.deck);
        }
        // A talk on now — the one a saved show had on, or another — so
        // that one is remembered no more.
        self.resume = None;
        self.resume_unlisted = None;
        if self.index_of(&new_id).is_some() {
            self.gone_at = None;
        }
    }

    /// Draw the layer's picture into `output`: the talk on, or it giving way
    /// to the coming one — or black or white over it.
    pub fn render(
        &mut self,
        audio_data: &crate::audio::AudioData,
        modulation: &crate::modulation::ModulationEngine,
        param_prefix: &str,
        cmd_buffers: &mut Vec<wgpu::CommandBuffer>,
        output: &wgpu::TextureView,
    ) {
        let context = self.context.clone();
        let mut draw = |open: &mut OpenTalk| {
            if let Err(e) = open.deck.render_with_prefix(
                &context,
                audio_data,
                modulation,
                param_prefix,
                cmd_buffers,
                None,
            ) {
                log::warn!("Slides talk '{}': {e}", open.path);
            }
        };
        if let Some(open) = &mut self.live {
            draw(open);
        }
        let progress = self.transition.map(|t| {
            let time = self.settings.transition_time();
            let x = if time <= 0.0 {
                1.0
            } else {
                (t / time).clamp(0.0, 1.0)
            };
            // Starts slowly, slows into place.
            (0.5 - 0.5 * (std::f64::consts::PI * x).cos()) as f32
        });
        let mix = match (progress, &mut self.coming) {
            (Some(t), Some(coming)) => {
                draw(coming);
                match self.settings.transition {
                    PlaylistTransition::Cut => Mix::Shown,
                    PlaylistTransition::Fade => Mix::Fade(t),
                    PlaylistTransition::Slide => Mix::Slide(t, [1.0, 0.0], 0.0),
                }
            }
            _ => Mix::Shown,
        };
        let (shown, next) = match self.screen {
            Screen::Black => (Some(&self.black.1), None),
            Screen::White => (Some(&self.white.1), None),
            Screen::Slide | Screen::Ended => (
                self.live.as_ref().map(|o| o.deck.output_view()),
                match mix {
                    Mix::Shown => None,
                    _ => self.coming.as_ref().map(|o| o.deck.output_view()),
                },
            ),
        };
        let mix = if self.screen == Screen::Slide {
            mix
        } else {
            Mix::Shown
        };
        self.mix.draw(
            &context.device,
            &context.queue,
            cmd_buffers,
            mix,
            shown,
            next,
            output,
            self.size,
        );
    }

    /// The master's size changed: talks are drawn at the new one.
    /// The picture the talks are shown in, pixels.
    #[must_use]
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.size = (width.max(1), height.max(1));
        let context = self.context.clone();
        let size = self.size;
        for open in self.live.iter_mut().chain(self.coming.iter_mut()) {
            open.deck.resize(&context, size.0, size.1);
            if let Some(pdf) = &mut open.pdf {
                pdf.ask(size);
            }
        }
    }

    /// The sound heard: the coming talk's once it takes over, else the one
    /// on — a clip's.
    pub fn clip_audio(&self) -> Option<std::sync::Arc<crate::audio_mix::ClipAudioShared>> {
        let coming = self
            .coming
            .as_ref()
            .filter(|_| self.transition.is_some())
            .and_then(|o| o.deck.clip_audio());
        coming.or_else(|| self.live.as_ref().and_then(|o| o.deck.clip_audio()))
    }

    /// Where the layer has got.
    pub fn state(&self) -> SlidesState {
        let mut pages = HashMap::new();
        let mut clip = HashMap::new();
        for open in self.live.iter().chain(self.coming.iter()) {
            let key = (open.id.clone(), open.path.clone());
            if let Some(pdf) = &open.pdf {
                pages.insert(key.clone(), (pdf.page, pdf.pages));
            }
            if let Some(&(at, of)) = self.positions.get(&key) {
                pages.insert(key.clone(), (at, Some(of)));
            }
            if let Some(p) = open.deck.playback_snapshot() {
                clip.insert(
                    key,
                    ClipPosition {
                        at: p.position_now(),
                        length: p.effective_out(),
                        playing: p.playing,
                    },
                );
            }
        }
        SlidesState {
            live: self.live.as_ref().map(|o| o.id.clone()),
            coming: self.coming_talk().map(|(id, _)| id.to_string()),
            next: self.next_talk(),
            next_picked: self
                .next_pick
                .as_ref()
                .is_some_and(|id| self.next_talk().as_ref() == Some(id)),
            previous: self.previous_talk().map(str::to_string),
            failed: self.failed.clone(),
            pages,
            clip,
            talk_secs: self
                .live
                .as_ref()
                .and(self.started_at)
                .map(|at| at.elapsed().as_secs_f64()),
            screen: self.screen,
        }
    }

    /// The talk picked to come next, as saved.
    pub fn next_pick(&self) -> Option<&str> {
        self.next_pick.as_deref()
    }
}

impl Drop for Slides {
    fn drop(&mut self) {
        for open in self.live.take().into_iter().chain(self.coming.take()) {
            let_go(open.deck);
        }
    }
}

/// A 1 × 1 picture of one colour, for black and white over the talk.
fn solid(context: &GpuContext, rgba: [u8; 4]) -> (wgpu::Texture, wgpu::TextureView) {
    let image = image::RgbaImage::from_pixel(1, 1, image::Rgba(rgba));
    super::source::upload_image_texture(context, &image)
}

/// A PowerPoint or web talk's deck: a capture with no window yet, bound
/// to its window once it plays.
fn capture_deck(
    context: &GpuContext,
    size: (u32, u32),
    scaling_mode: ScalingMode,
    talk: &Talk,
) -> anyhow::Result<Deck> {
    use crate::screen_capture::backend::{CaptureConfig, TargetIdentity};
    let app = match talk.kind() {
        Some(TalkKind::PowerPoint) => crate::presentation::powerpoint::POWERPOINT_ID,
        _ => "lt.proconf.nida",
    };
    let state = super::ScreenCaptureState {
        capture_id: crate::screen_capture::UNBOUND_CAPTURE_ID,
        identity: TargetIdentity::Window {
            app: app.to_string(),
            title: talk.label(),
        },
        config: CaptureConfig {
            rate: crate::presentation::manager::SHOW_CAPTURE_RATE,
            exclude_varda: false,
            scale_to: Some(size),
            ..CaptureConfig::default()
        },
        config_dirty: false,
        presentation: None,
    };
    let mut deck = Deck::new_from_screen_capture(
        context,
        state,
        &talk.label(),
        size.0,
        size.1,
        size.0,
        size.1,
    )?;
    deck.set_scaling_mode(scaling_mode);
    Ok(deck)
}

/// A PDF talk's deck: a picture, its page put in once drawn.
fn picture_deck(
    context: &GpuContext,
    size: (u32, u32),
    scaling_mode: ScalingMode,
    talk: &Talk,
) -> anyhow::Result<Deck> {
    let clear = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 0]));
    let mut deck =
        Deck::new_from_pixels(context, &clear, talk.label(), &talk.path, size.0, size.1)?;
    deck.set_scaling_mode(scaling_mode);
    Ok(deck)
}

/// A clip that holds its last frame (the talk stays there), or a picture.
fn open_file_deck(
    context: &GpuContext,
    path: &str,
    size: (u32, u32),
    scaling_mode: ScalingMode,
) -> anyhow::Result<Deck> {
    let mut deck = match TalkKind::of(path) {
        Some(TalkKind::Picture) => Deck::new_from_image(context, path, size.0, size.1)?,
        Some(TalkKind::Clip) => {
            let deck = Deck::new_from_video(context, path, size.0, size.1)?;
            deck.video_set_loop_mode(crate::video::LoopMode::HoldLast);
            deck
        }
        _ => anyhow::bail!("not a clip or a picture"),
    };
    deck.set_scaling_mode(scaling_mode);
    Ok(deck)
}

/// Close a deck on a thread: a clip's decoder is waited for as it stops.
fn let_go(deck: Box<Deck>) {
    let _ = std::thread::Builder::new()
        .name("slides-close".into())
        .spawn(move || drop(deck));
}

#[cfg(test)]
mod tests {
    use super::around;

    #[test]
    fn the_pages_kept_are_the_one_shown_and_those_either_side() {
        assert_eq!(around(1, Some(10)), vec![1, 2]);
        assert_eq!(around(5, Some(10)), vec![5, 6, 4]);
        assert_eq!(around(10, Some(10)), vec![10, 9]);
        assert_eq!(around(1, Some(1)), vec![1]);
        assert_eq!(around(3, None), vec![3, 4, 2]);
        assert_eq!(around(12, Some(10)), vec![10, 9]);
        assert_eq!(around(0, Some(3)), vec![1, 2]);
    }
}
