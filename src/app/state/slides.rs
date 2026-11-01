//! Slides inputs on the engine's side: adding one and its talks, starting a
//! talk, stepping the talk on, and what the panel and the API see of them.
//! The layer itself is `crate::deck::slides`; PowerPoint talks are kept open
//! and captured in `presentations`.

use anyhow::{Context, Result};

use super::super::VardaApp;
use crate::deck::Deck;
use crate::engine::value::presentation::{Go, PresentationSnapshot, PresentationStatus};
use crate::engine::value::slides::{
    SlidesSettings, SlidesSnapshot, Talk, TalkKind, TalkRef, TalkSnapshot, TalkState,
};
use crate::engine::{CommandResult, InputSource};

impl VardaApp {
    /// Add a Slides layer to a channel: `paths`' files as talks, nothing on.
    pub(crate) fn add_slides_deck(
        &mut self,
        channel_uuid: &str,
        paths: &[String],
        settings: Option<SlidesSettings>,
    ) -> Result<String> {
        let channel_idx = self.resolve_channel(channel_uuid)?;
        let talks = talks_of(paths)?;
        let count = talks.len();
        let mut deck = Deck::new_slides(
            &self.context,
            talks,
            settings.unwrap_or_default(),
            None,
            None,
            self.render_width,
            self.render_height,
        );
        let uuid = deck.uuid().to_string();
        self.add_default_effects(&mut deck);
        self.mixer
            .channel_mut(channel_idx)
            .context("Invalid channel")?
            .add_deck(deck);
        log::info!("Added Slides layer {uuid} to channel {channel_idx}: {count} talk(s)");
        Ok(uuid)
    }

    /// Do `edit` to the Slides layer `deck_uuid`.
    pub(crate) fn edit_slides<T>(
        &mut self,
        deck_uuid: &str,
        edit: impl FnOnce(&mut crate::deck::slides::Slides) -> T,
    ) -> Result<T> {
        let (ch, dk) = self.resolve_deck(deck_uuid)?;
        let slides = self.mixer.channels_mut()[ch].decks[dk]
            .deck
            .slides_mut()
            .context("That layer is not a Slides input's")?;
        Ok(edit(slides))
    }

    /// A Slides layer's talks, all of them.
    pub(crate) fn set_slides_talks(&mut self, deck_uuid: &str, talks: Vec<Talk>) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        for talk in &talks {
            if talk.id.trim().is_empty() || !seen.insert(talk.id.clone()) {
                anyhow::bail!("Talk id '{}' is empty or used twice", talk.id);
            }
            if talk.path.trim().is_empty() {
                anyhow::bail!("Talk {} has no file", talk.id);
            }
        }
        self.edit_slides(deck_uuid, |s| s.set_talks(talks))
    }

    /// One talk of `deck_uuid` renamed, or given another file, against the
    /// list as it is now.
    pub(crate) fn edit_talk(
        &mut self,
        deck_uuid: &str,
        talk_id: &str,
        name: Option<String>,
        path: Option<&str>,
    ) -> Result<()> {
        let path = match path {
            Some(path) => Some(
                talks_of(&[path.to_string()])?
                    .pop()
                    .map(|t| t.path)
                    .context("no file")?,
            ),
            None => None,
        };
        self.edit_slides(deck_uuid, |s| {
            let mut talks = s.talks().to_vec();
            let talk = talks
                .iter_mut()
                .find(|t| t.id == talk_id)
                .with_context(|| format!("No talk {talk_id} in the list"))?;
            if let Some(name) = name {
                talk.name = name.trim().to_string();
            }
            if let Some(path) = path {
                talk.path = path;
                talk.slide = None;
            }
            s.set_talks(talks);
            Ok(())
        })?
    }

    /// One talk of `deck_uuid` moved to place `to`.
    pub(crate) fn move_talk(&mut self, deck_uuid: &str, talk_id: &str, to: usize) -> Result<()> {
        self.edit_slides(deck_uuid, |s| {
            let mut talks = s.talks().to_vec();
            let from = talks
                .iter()
                .position(|t| t.id == talk_id)
                .with_context(|| format!("No talk {talk_id} in the list"))?;
            let talk = talks.remove(from);
            talks.insert(to.min(talks.len()), talk);
            s.set_talks(talks);
            Ok(())
        })?
    }

    /// One talk of `deck_uuid` taken out of the list.
    pub(crate) fn remove_talk(&mut self, deck_uuid: &str, talk_id: &str) -> Result<()> {
        self.edit_slides(deck_uuid, |s| {
            let mut talks = s.talks().to_vec();
            let before = talks.len();
            talks.retain(|t| t.id != talk_id);
            if talks.len() == before {
                anyhow::bail!("No talk {talk_id} in the list");
            }
            s.set_talks(talks);
            Ok(())
        })?
    }

    /// Add `paths`' files as talks to `deck_uuid` — or the show's first
    /// Slides input, made on a new input when there is none — at place `at`,
    /// else at the end. Replies with the Slides layer's UUID.
    pub(crate) fn add_talks(
        &mut self,
        deck_uuid: Option<&str>,
        paths: &[String],
        at: Option<usize>,
    ) -> CommandResult {
        let added = match talks_of(paths) {
            Ok(talks) => talks,
            Err(e) => {
                return CommandResult::Err {
                    code: crate::engine::ErrorCode::InvalidInput,
                    message: e.to_string(),
                }
            }
        };
        let named = deck_uuid
            .map(str::to_string)
            .or_else(|| self.slides_decks().into_iter().next());
        let deck = if let Some(uuid) = named {
            uuid
        } else {
            // A Slides input of its own, the talks in it.
            let made = self.cmd_add_input(
                &InputSource::Slides { paths: Vec::new() },
                Some("Slides"),
                None,
            );
            if let CommandResult::Err { .. } = made {
                return made;
            }
            let Some(uuid) = self.slides_decks().pop() else {
                return CommandResult::Err {
                    code: crate::engine::ErrorCode::InternalError,
                    message: "The Slides input was not made".into(),
                };
            };
            uuid
        };
        let result = self.edit_slides(&deck, |s| {
            let mut talks = s.talks().to_vec();
            let at = at.unwrap_or(talks.len()).min(talks.len());
            for (i, talk) in added.into_iter().enumerate() {
                talks.insert(at + i, talk);
            }
            s.set_talks(talks);
        });
        match result {
            Ok(()) => CommandResult::OkWithId { uuid: deck },
            Err(e) => CommandResult::Err {
                code: crate::engine::ErrorCode::NotFound,
                message: e.to_string(),
            },
        }
    }

    /// The Slides layers, by deck UUID, in the order of the inputs.
    pub(crate) fn slides_decks(&self) -> Vec<String> {
        self.mixer
            .channels()
            .iter()
            .flat_map(|ch| &ch.decks)
            .filter(|slot| slot.deck.slides().is_some())
            .map(|slot| slot.deck.uuid().to_string())
            .collect()
    }

    /// The Slides input a start goes to: the one named, else the one on air,
    /// else the one moved last, else the first.
    fn slides_target(&self, deck_uuid: Option<&str>) -> Result<String> {
        match deck_uuid {
            Some(uuid) => self
                .slides_decks()
                .into_iter()
                .find(|d| d == uuid)
                .with_context(|| format!("{uuid} is not a Slides input")),
            None => match self.show_target(None) {
                Ok(super::presentations::ShowTarget::Slides(deck)) => Ok(deck),
                _ => self
                    .slides_decks()
                    .into_iter()
                    .next()
                    .context("There is no Slides input"),
            },
        }
    }

    /// Start a talk of a Slides input (see `EngineCommand::StartTalk`).
    pub(crate) fn start_talk(&mut self, deck_uuid: Option<&str>, to: &TalkRef) -> Result<()> {
        let deck = self.slides_target(deck_uuid)?;
        let started = self.edit_slides(&deck, |s| s.start(to))?;
        let talk = started.map_err(anyhow::Error::msg)?;
        log::info!("Slides input {deck}: starting talk {talk}");
        self.presentations.last = Some(deck);
        Ok(())
    }

    /// The talk Start next takes, picked by hand — or `None`, the talk after
    /// the one on.
    /// Look a Slides input's talks' files over again — or every Slides
    /// input's: what changes no talk's file (a video copied beside its deck,
    /// a font installed) is seen.
    pub(crate) fn check_talks(&mut self, deck_uuid: Option<&str>) -> Result<()> {
        let Some(uuid) = deck_uuid else {
            self.talk_checks.check_again(None);
            return Ok(());
        };
        let (ch, dk) = self.resolve_deck(uuid)?;
        let slides = self.mixer.channels()[ch].decks[dk]
            .deck
            .slides()
            .with_context(|| format!("{uuid} is not a Slides input"))?;
        let paths: Vec<String> = slides.talks().iter().map(|t| t.path.clone()).collect();
        self.talk_checks.check_again(Some(&paths));
        Ok(())
    }

    pub(crate) fn set_next_talk(&mut self, deck_uuid: &str, talk: Option<String>) -> Result<()> {
        self.edit_slides(deck_uuid, |s| s.pick_next(talk))?
            .map_err(anyhow::Error::msg)
    }

    /// Step a Slides input's talk on: PowerPoint's show, a PDF's page.
    pub(crate) fn slides_go(&mut self, deck: &str, to: Go) -> Result<()> {
        let live = {
            let (ch, dk) = self.resolve_deck(deck)?;
            self.mixer.channels()[ch].decks[dk]
                .deck
                .slides()
                .and_then(|s| s.live_talk().map(|(id, kind)| (id.to_string(), kind)))
        };
        let Some((_, kind)) = live else {
            return Ok(());
        };
        match kind {
            TalkKind::PowerPoint => {
                let target = super::presentations::ShowTarget::Slides(deck.to_string());
                if let (Some(manager), Some(key)) =
                    (&self.presentations.manager, self.show_key(&target))
                {
                    manager.go(&key, to);
                }
            }
            TalkKind::Pdf => {
                self.edit_slides(deck, |s| s.go(to))?;
            }
            // The page given the key a clicker sends.
            TalkKind::Web => {
                let key = {
                    let (ch, dk) = self.resolve_deck(deck)?;
                    let s = self.mixer.channels()[ch].decks[dk].deck.slides();
                    s.and_then(|s| {
                        let (id, _) = s.live_talk()?;
                        Some(super::presentations::talk_key(deck, id, s.open_path(id)?))
                    })
                };
                if let (Some(engine), Some(key)) = (self.web_talks.engine.as_mut(), key) {
                    engine.go(&key, to);
                }
            }
            TalkKind::Clip | TalkKind::Picture => {}
        }
        Ok(())
    }

    /// A Slides layer for the API and the panels; `None` for any other.
    pub(crate) fn slides_snapshot(&self, deck: &Deck) -> Option<SlidesSnapshot> {
        let slides = deck.slides()?;
        let state = slides.state();
        let index = |id: &Option<String>| {
            id.as_ref()
                .and_then(|id| slides.talks().iter().position(|t| t.id == *id))
        };
        let plays_on = &slides.settings().plays_on;
        let (width, height) = slides.size();
        let aspect = f64::from(width) / f64::from(height.max(1));
        // A talk as the panel shows it: `shown_path` its file in the list,
        // `session_path` the file what plays it has (the one it was started
        // with, while it is on).
        let snap = |talk: &Talk, shown_path: &str, session_path: &str, kind: Option<TalkKind>| {
            let file = std::fs::metadata(shown_path).ok().filter(std::fs::Metadata::is_file);
            let missing = file.is_none();
            let warnings = file
                .map(|f| {
                    self.talk_checks
                        .warnings(shown_path, (f.len(), f.modified().ok()), aspect)
                })
                .unwrap_or_default();
            let key = super::presentations::talk_key(deck.uuid(), &talk.id, session_path);
            let presentation = (kind == Some(TalkKind::PowerPoint))
                .then(|| {
                    self.presentations
                        .manager
                        .as_ref()
                        .and_then(|m| m.view(&key))
                        .map(|_| self.show_snapshot(&key, session_path, plays_on))
                })
                .flatten();
            let live = state.live.as_deref() == Some(talk.id.as_str());
            let coming = state.coming.as_deref() == Some(talk.id.as_str());
            let mut message = state.failed.get(&talk.id).cloned();
            if message.is_none() && kind.is_none() {
                message = Some("Nida does not play this kind of file".into());
            } else if message.is_none() && missing {
                message = Some("Its file is not there".into());
            }
            let shown_failed = presentation
                .as_ref()
                .is_some_and(|p| p.status == PresentationStatus::Failed);
            if shown_failed && message.is_none() {
                message = presentation.as_ref().and_then(|p| p.message.clone());
            }
            let state_of = if live {
                TalkState::Live
            } else if coming {
                TalkState::Starting
            } else if message.is_some() {
                TalkState::Failed
            } else if presentation
                .as_ref()
                .is_some_and(|p| p.status == PresentationStatus::Ready)
                || (kind == Some(TalkKind::Web) && self.web_talk_ready(&key))
            {
                TalkState::Ready
            } else {
                TalkState::Waiting
            };
            // Where this file of the talk is: the talk on and a new file of
            // it starting have each their own.
            let at = (talk.id.clone(), session_path.to_string());
            let (position, shown) = match (&presentation, state.pages.get(&at)) {
                (Some(p), _) if p.position.is_some() => (p.position, p.shown),
                (_, Some((page, pages))) => (Some(*page), *pages),
                _ => (None, None),
            };
            let pictures = kind == Some(TalkKind::PowerPoint)
                && !missing
                && crate::presentation::pictures::saved_for(std::path::Path::new(shown_path))
                    .is_some();
            TalkSnapshot {
                id: talk.id.clone(),
                name: talk.label(),
                given_name: talk.name.clone(),
                path: shown_path.to_string(),
                kind,
                state: state_of,
                message,
                missing,
                position,
                shown,
                presentation,
                clip: state.clip.get(&at).copied(),
                warnings,
                pictures,
            }
        };
        let talks = slides
            .talks()
            .iter()
            .map(|talk| {
                let session = slides.open_path(&talk.id).unwrap_or(&talk.path);
                snap(talk, &talk.path, session, talk.kind())
            })
            .collect();
        // The talk on, as it plays: out of the list, or given another file
        // there, it is still the one moved and blacked out.
        let on = slides.live_record().map(|(id, path, kind, name)| {
            let talk = Talk {
                id: id.to_string(),
                path: path.to_string(),
                name: name.to_string(),
                slide: None,
            };
            snap(&talk, path, path, Some(kind))
        });
        Some(SlidesSnapshot {
            talks,
            settings: slides.settings().clone(),
            live: index(&state.live),
            on,
            screen: state.screen,
            starting: index(&state.coming),
            next: index(&state.next),
            next_picked: state.next_picked,
            previous: index(&state.previous),
            talk_secs: state.talk_secs,
            making_pictures: slides
                .talks()
                .iter()
                .filter(|t| self.presentations.making(std::path::Path::new(&t.path)))
                .count(),
        })
    }

    /// What a presenter view of the Slides layer `deck` shows: the talk on,
    /// as a PowerPoint input's show would say it — its slide and notes for a
    /// PowerPoint talk, its page for a PDF — and black or white over it.
    pub(crate) fn slides_show(&self, deck: &Deck) -> Option<(String, PresentationSnapshot)> {
        let slides = deck.slides()?;
        let (talk_id, kind) = slides.live_talk()?;
        let talk = slides.talks().iter().find(|t| t.id == talk_id);
        let path = talk.map_or_else(String::new, |t| t.path.clone());
        let plays_on = &slides.settings().plays_on;
        let path = slides.open_path(talk_id).map_or(path, str::to_string);
        let mut show = if kind == TalkKind::PowerPoint {
            let key = super::presentations::talk_key(deck.uuid(), talk_id, &path);
            self.show_snapshot(&key, &path, plays_on)
        } else {
            let state = slides.state();
            // The file the talk on plays: a new file of it starting has its
            // own place.
            let (position, shown) = state
                .pages
                .get(&(talk_id.to_string(), path.clone()))
                .map_or((None, None), |(page, pages)| (Some(*page), *pages));
            PresentationSnapshot {
                path,
                status: PresentationStatus::Showing,
                message: None,
                slide: position,
                slides: shown,
                position,
                shown,
                screen: crate::engine::value::presentation::Screen::Slide,
                title: talk.map(Talk::label),
                notes: String::new(),
                next_slide: position
                    .zip(shown)
                    .and_then(|(p, n)| (p < n).then_some(p + 1)),
                next_title: None,
                plays_on: plays_on.clone(),
                playing_on: None,
                slide_pixels: None,
                video: None,
            }
        };
        if slides.screen() != crate::engine::value::presentation::Screen::Slide {
            show.screen = slides.screen();
        }
        Some((talk_id.to_string(), show))
    }
}

/// Where a presenter view of the Slides input `deck` draws the next slide's
/// picture from: the talk on's own PDF, the file it plays — with its size
/// and time, so one put in its place is opened afresh; none for a talk of
/// another kind.
pub(crate) fn next_pictures_of(deck: &Deck) -> Option<crate::deck::presenter::NextSource> {
    let slides = deck.slides()?;
    let (talk_id, kind) = slides.live_talk()?;
    if kind != TalkKind::Pdf {
        return None;
    }
    let path = slides.open_path(talk_id).map(str::to_string).or_else(|| {
        slides
            .talks()
            .iter()
            .find(|t| t.id == talk_id)
            .map(|t| t.path.clone())
    })?;
    let path = std::path::PathBuf::from(path);
    let stamp = std::fs::metadata(&path)
        .ok()
        .and_then(|m| Some((m.len(), m.modified().ok()?)));
    Some(crate::deck::presenter::NextSource {
        talk: talk_id.to_string(),
        path,
        stamp,
    })
}

/// The talk on of a Slides layer when PowerPoint plays it: its id, and the
/// file its show has.
pub(crate) fn powerpoint_talk_on(deck: &Deck) -> Option<(String, std::path::PathBuf)> {
    let slides = deck.slides()?;
    let (talk_id, kind) = slides.live_talk()?;
    if kind != TalkKind::PowerPoint {
        return None;
    }
    let path = slides.open_path(talk_id).map(str::to_string).or_else(|| {
        slides
            .talks()
            .iter()
            .find(|t| t.id == talk_id)
            .map(|t| t.path.clone())
    })?;
    Some((talk_id.to_string(), std::path::PathBuf::from(path)))
}

/// Talks of `paths`' files, as found: each absolute, a kind Nida plays.
///
/// # Errors
///
/// A file not there, or one Nida does not play: none is added then.
fn talks_of(paths: &[String]) -> Result<Vec<Talk>> {
    let mut out = Vec::new();
    for path in paths {
        let found = std::fs::canonicalize(path).with_context(|| format!("No file at {path}"))?;
        if !found.is_file() {
            anyhow::bail!("{} is not a file", found.display());
        }
        let found = found.to_string_lossy().into_owned();
        if TalkKind::of(&found).is_none() {
            anyhow::bail!(
                "{found}: Nida plays PowerPoint decks, PDFs, web pages (.html), clips and pictures"
            );
        }
        out.push(Talk::new(found));
    }
    Ok(out)
}

/// The engine's web talks: what plays them (the Mac's `WebKit`, made with the
/// first web talk — or a stand-in in tests) and the pages open.
pub(crate) struct WebTalks {
    engine: Option<Box<dyn crate::presentation::web::WebEngine>>,
    /// Pages open, by key.
    pages: std::collections::HashMap<String, WebPage>,
    /// Pages that did not open, by key: not opened again before then.
    retry: std::collections::HashMap<String, std::time::Instant>,
    /// The number the next opening, or capture, is given.
    counter: u64,
    /// Headless: no `WebKit` (it lives on the main thread, which a headless
    /// run does not turn) unless a stand-in is given.
    headless: bool,
}

/// A page open, as the engine was asked to have it: each flag what it was
/// told apart from the others.
#[allow(clippy::struct_excessive_bools)]
struct WebPage {
    /// Which opening of it this is: what an earlier one finds is let go.
    generation: u64,
    /// On or coming: its window captured.
    shown: bool,
    /// Its video and sound playing: its picture shows (the talk on, or the
    /// one coming being brought on) — never heard before it is seen.
    playing: bool,
    /// Set up — loaded, at its slide — so its window can be captured.
    ready: bool,
    muted: bool,
    pixels: (u32, u32),
    /// The capture asked and not answered yet, and when: no other is asked
    /// meanwhile — an opening can take seconds — unless it is overdue. What
    /// another, earlier, capture finds is let go.
    pending: Option<(u64, std::time::Instant)>,
    /// When the capture adopted last was: its first picture is waited for a
    /// while before another is asked.
    adopted: Option<std::time::Instant>,
    /// A capture failed: the next is not asked before then.
    retry_at: Option<std::time::Instant>,
    /// Laid out at a new size since its window was captured: captured again.
    resized: bool,
}

impl WebTalks {
    pub(crate) fn new(headless: bool) -> Self {
        Self {
            engine: None,
            pages: std::collections::HashMap::new(),
            retry: std::collections::HashMap::new(),
            counter: 0,
            headless,
        }
    }

    fn next_number(&mut self) -> u64 {
        self.counter += 1;
        self.counter
    }

    /// Whether `generation` is the opening of page `key` open now.
    fn is_current(&self, key: &str, generation: u64) -> bool {
        self.pages.get(key).is_some_and(|p| p.generation == generation)
    }
}

/// A capture not answered in this long is given up on: an opening takes at
/// most about ten seconds (five to find the window, five to start it).
const CAPTURE_OPENS: std::time::Duration = std::time::Duration::from_secs(12);

/// A capture adopted with no picture in this long is asked for again.
const FIRST_PICTURE: std::time::Duration = std::time::Duration::from_secs(5);

/// After a capture failed, the next is asked this long after.
const CAPTURE_AGAIN: std::time::Duration = std::time::Duration::from_secs(3);

/// A page that did not open is opened again this long after, if still
/// wanted.
const OPEN_AGAIN: std::time::Duration = std::time::Duration::from_secs(5);

impl VardaApp {
    /// Each frame: open the web talks wanted — playing the talk on and the
    /// one coming, the next and the one before ready — close the rest, and
    /// take what the pages say: set up, their windows captured, where they
    /// are, why one cannot play. A page's window is captured only once it
    /// is set up: a talk started never shows a page half drawn.
    pub(crate) fn reconcile_web_talks(&mut self) {
        use crate::presentation::web::WebEvent;
        let wanted = self.talk_shows(TalkKind::Web);
        if self.web_talks.engine.is_none() {
            if wanted.is_empty() || self.web_talks.headless {
                return;
            }
            #[cfg(target_os = "macos")]
            {
                if !crate::presentation::web::WebKit::can_draw_off_screen() {
                    log::warn!(
                        "Web talks: this macOS's WebKit cannot keep a page drawing off screen"
                    );
                }
                self.web_talks.engine = Some(Box::new(crate::presentation::web::WebKit::default()));
            }
            #[cfg(not(target_os = "macos"))]
            return;
        }
        let pixels = (self.render_width, self.render_height);
        let heard = self.web_talks_heard(&wanted);
        let now = std::time::Instant::now();
        let web = &mut self.web_talks;
        let Some(mut engine) = web.engine.take() else {
            return;
        };
        for w in &wanted {
            let muted = heard.as_ref().is_some_and(|h| !h.contains(&w.slides));
            if !web.pages.contains_key(&w.key) {
                if web.retry.get(&w.key).is_some_and(|at| now < *at) {
                    continue;
                }
                let generation = web.next_number();
                engine.load(&w.key, generation, std::path::Path::new(&w.path), pixels, w.slide);
                if muted {
                    engine.set_muted(&w.key, true);
                }
                web.pages.insert(
                    w.key.clone(),
                    WebPage {
                        generation,
                        shown: false,
                        playing: false,
                        ready: false,
                        muted,
                        pixels,
                        pending: None,
                        adopted: None,
                        retry_at: None,
                        resized: false,
                    },
                );
            }
            let Some(page) = web.pages.get_mut(&w.key) else {
                continue;
            };
            if page.pixels != pixels {
                engine.resize(&w.key, pixels);
                page.pixels = pixels;
                page.resized = true;
                page.pending = None;
                page.adopted = None;
            }
            if page.muted != muted {
                engine.set_muted(&w.key, muted);
                page.muted = muted;
            }
            if page.shown != w.show {
                page.shown = w.show;
                page.pending = None;
                page.adopted = None;
                page.retry_at = None;
            }
            if page.playing != w.on {
                engine.set_playing(&w.key, w.on);
                page.playing = w.on;
            }
        }
        let gone: Vec<String> = web
            .pages
            .keys()
            .filter(|k| !wanted.iter().any(|w| &w.key == *k))
            .cloned()
            .collect();
        for key in gone {
            engine.unload(&key);
            web.pages.remove(&key);
        }
        web.retry
            .retain(|k, at| now < *at && wanted.iter().any(|w| &w.key == k));
        let events = engine.poll();
        self.web_talks.engine = Some(engine);
        for event in events {
            match event {
                WebEvent::Ready { key, generation } => {
                    if let Some(page) = self
                        .web_talks
                        .pages
                        .get_mut(&key)
                        .filter(|p| p.generation == generation)
                    {
                        page.ready = true;
                    }
                }
                WebEvent::Captured {
                    key,
                    generation,
                    attempt,
                    target,
                    capture,
                    config,
                } => {
                    // An earlier opening's, or capture's, is let go: dropped,
                    // its capture stops.
                    let asked = self.web_talks.pages.get_mut(&key).filter(|p| {
                        p.generation == generation && p.pending.is_some_and(|(a, _)| a == attempt)
                    });
                    if let Some(page) = asked {
                        page.pending = None;
                        page.adopted = Some(now);
                        page.resized = false;
                        // Each capture of a page asked for is to replace any
                        // there is: one that sent no picture is not kept.
                        self.bind_talk_window(&key, &target, capture, config, true);
                    }
                }
                WebEvent::Failed {
                    key,
                    generation,
                    attempt,
                    why,
                } => {
                    let current = self.web_talks.pages.get(&key).is_some_and(|p| {
                        p.generation == generation
                            && attempt.is_none_or(|a| p.pending.is_some_and(|(at, _)| at == a))
                    });
                    if !current {
                        continue;
                    }
                    if let Some(page) = self.web_talks.pages.get_mut(&key).filter(|_| attempt.is_some()) {
                        page.pending = None;
                        page.retry_at = Some(now + CAPTURE_AGAIN);
                    }
                    if attempt.is_none() {
                        // It did not open: closed, and opened again in a
                        // while if still wanted.
                        if let Some(engine) = self.web_talks.engine.as_mut() {
                            engine.unload(&key);
                        }
                        self.web_talks.pages.remove(&key);
                        self.web_talks.retry.insert(key.clone(), now + OPEN_AGAIN);
                    }
                    if let Some(w) = wanted.iter().find(|w| w.key == key) {
                        log::warn!("Web talk {}: {why}", w.talk);
                        let (talk, path) = (w.talk.clone(), w.path.clone());
                        let _ = self.edit_slides(&w.slides, |s| s.talk_failed(&talk, &path, why));
                    }
                }
                WebEvent::Position {
                    key,
                    generation,
                    at,
                    of,
                } => {
                    if !self.web_talks.is_current(&key, generation) {
                        continue;
                    }
                    if let Some(w) = wanted.iter().find(|w| w.key == key) {
                        let (talk, path) = (w.talk.clone(), w.path.clone());
                        let _ = self
                            .edit_slides(&w.slides, |s| s.set_position(&talk, &path, at, of));
                    }
                }
            }
        }
        // A talk on or coming whose page is set up, with no picture of its
        // window — or one at a size since changed: captured. One capture at
        // a time, waited for; one adopted with no picture a while, or one
        // that failed, asked again.
        let mut asks = Vec::new();
        for slot in self.mixer.channels().iter().flat_map(|ch| &ch.decks) {
            let Some(s) = slot.deck.slides() else {
                continue;
            };
            for open in s.open_talks() {
                if open.kind != TalkKind::Web {
                    continue;
                }
                let key = super::presentations::talk_key(slot.deck.uuid(), &open.id, &open.path);
                let Some(page) = self.web_talks.pages.get(&key) else {
                    continue;
                };
                if !page.shown || !page.ready {
                    continue;
                }
                // A picture there from the capture running now — not one
                // merely open, which may never send one, nor one it replaced.
                let pictured = open
                    .capture
                    .is_some_and(|id| self.screen_capture_manager.has_fresh_picture(id));
                if pictured && !page.resized {
                    continue;
                }
                let since = |at: std::time::Instant| now.saturating_duration_since(at);
                let under_way = page.pending.is_some_and(|(_, at)| since(at) < CAPTURE_OPENS);
                let resting = page.retry_at.is_some_and(|at| now < at);
                let waiting = !page.resized && page.adopted.is_some_and(|at| since(at) < FIRST_PICTURE);
                if !under_way && !resting && !waiting {
                    asks.push(key);
                }
            }
        }
        for key in asks {
            let attempt = self.web_talks.next_number();
            if let Some(engine) = self.web_talks.engine.as_mut() {
                engine.capture(&key, attempt);
            }
            if let Some(page) = self.web_talks.pages.get_mut(&key) {
                page.pending = Some((attempt, now));
                page.retry_at = None;
            }
        }
    }

    /// The Slides inputs whose web talks are heard now. Their sound all
    /// comes from the one `WebKit`, so it is one strip; while that strip
    /// follows the inputs on air (Audio Auto), only the pages of the Slides
    /// inputs it hears now are heard — another's would sound with theirs.
    /// `None` when every page is heard: no such strip, or one heard whatever
    /// is on air.
    fn web_talks_heard(
        &self,
        wanted: &[super::presentations::TalkShow],
    ) -> Option<std::collections::HashSet<String>> {
        if wanted.is_empty() {
            return None;
        }
        let slides = self.slides_decks();
        let strip = self.web_sound_strip(&slides)?;
        let (_, _, follow) = self
            .audio_mix
            .auto_sources()
            .into_iter()
            .find(|(s, _, _)| *s == strip)?;
        let on_air = self.on_air();
        Some(
            slides
                .into_iter()
                .filter(|d| on_air.at(follow.as_deref(), d) > 0.0)
                .collect(),
        )
    }

    /// Whether the web talk `key`'s page is set up, ready to start.
    pub(crate) fn web_talk_ready(&self, key: &str) -> bool {
        self.web_talks.pages.get(key).is_some_and(|p| p.ready)
    }

    /// Play web talks with `engine` — a stand-in, in tests.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn set_web_engine(&mut self, engine: Box<dyn crate::presentation::web::WebEngine>) {
        self.web_talks.engine = Some(engine);
    }
}
