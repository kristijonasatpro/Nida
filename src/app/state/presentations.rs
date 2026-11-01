//! PowerPoint inputs on the engine's side: adding one, moving its show on,
//! and each frame keeping the presentation worker in step with the decks —
//! opening what is new, closing what is gone, capturing each show's window
//! once PowerPoint plays it. See `crate::presentation`.

use std::path::Path;

use anyhow::{Context, Result};

use super::super::VardaApp;
use crate::deck::{Deck, ScreenCaptureState};
use crate::engine::value::presentation::{
    Go, PlaysOn, PresentationSnapshot, PresentationStatus, Screen,
};
use crate::engine::value::slides::TalkKind;
use crate::presentation::manager::{PresentationManager, Status};
use crate::presentation::powerpoint::POWERPOINT_ID;
use crate::presentation::screens::{self, ScreenSpot, Want};
use crate::presentation::PresentationConfig;
use crate::screen_capture::backend::{CaptureConfig, TargetIdentity};

/// The engine's PowerPoint state: the worker, started with the first
/// PowerPoint input, and the show moved last.
pub(crate) struct Presentations {
    pub(super) manager: Option<PresentationManager>,
    /// Where a step with no deck named goes when no show is on air.
    pub(super) last: Option<String>,
    /// `--no-powerpoint`: no worker is started (a stand-in may be given).
    disabled: bool,
    /// What each presenter view was last told, by its layer: told again
    /// only when its show changes.
    fed: std::collections::HashMap<String, Fed>,
    /// The connected screens, read a second ago at most; given by a test.
    screens: Vec<ScreenSpot>,
    screens_at: Option<std::time::Instant>,
    screens_given: bool,
    /// The screen each deck's show was last sent to, and the choice it came
    /// from: a choice changed (or undone) sends it again — another input of
    /// the file may have moved the show from there meanwhile.
    spots: std::collections::HashMap<String, (ScreenSpot, PlaysOn)>,
    /// A window's title bar on this macOS, points.
    title_bar: u32,
    /// The decks PowerPoint is saving as slide pictures (see
    /// `make_slide_pictures`).
    making: std::collections::HashSet<std::path::PathBuf>,
    /// Each slide pictures' PDF's pages, once counted — and the file's size
    /// and time then: counted again once the file is another.
    pdf_pages: std::collections::HashMap<std::path::PathBuf, (Option<PdfStamp>, u32)>,
}

/// A PDF file's size and time: another once it is written again.
type PdfStamp = (u64, std::time::SystemTime);

/// How a presenter view's show stood when it was last told.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Fed {
    gone: bool,
    status: Option<Status>,
    show: Option<crate::presentation::powerpoint::ShowState>,
    notes: bool,
    /// Of a Slides input: the talk on, and what was said of it.
    talk: Option<String>,
    slides: Option<PresentationSnapshot>,
    /// The next slide's pictures it was given, and their page.
    next: (Option<crate::deck::presenter::NextSource>, Option<u32>),
}

impl Presentations {
    pub(crate) fn new(disabled: bool) -> Self {
        Self {
            manager: None,
            last: None,
            disabled,
            fed: std::collections::HashMap::new(),
            screens: Vec::new(),
            screens_at: None,
            screens_given: false,
            spots: std::collections::HashMap::new(),
            title_bar: crate::presentation::manager::title_bar_points(),
            making: std::collections::HashSet::new(),
            pdf_pages: std::collections::HashMap::new(),
        }
    }

    /// Whether PowerPoint is saving the deck at `path` as slide pictures.
    pub(crate) fn making(&self, path: &Path) -> bool {
        self.making.contains(path)
    }
}

/// What a step, a black screen or a video control goes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ShowTarget {
    /// A PowerPoint input's layer.
    Deck(String),
    /// A Slides input's layer: its talk on.
    Slides(String),
}

impl ShowTarget {
    pub(crate) fn uuid(&self) -> &str {
        match self {
            Self::Deck(uuid) | Self::Slides(uuid) => uuid,
        }
    }
}

/// The key PowerPoint's session for a Slides talk goes by: its input, the
/// talk and the file — so two copies of one input, or a talk given another
/// file while it is on, are sessions of their own, and a window found for
/// one is never taken for another.
pub(crate) fn talk_key(slides: &str, talk: &str, path: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    format!("{slides}/{talk}/{:016x}", h.finish())
}

/// A Slides talk PowerPoint plays: opened ahead, or its show running.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TalkShow {
    /// Its session's key (see [`talk_key`]).
    pub(crate) key: String,
    /// The Slides input's layer.
    pub(crate) slides: String,
    pub(crate) talk: String,
    pub(crate) path: String,
    pub(crate) slide: Option<u32>,
    pub(crate) plays_on: PlaysOn,
    /// On or coming: its show runs.
    pub(crate) show: bool,
    /// Its picture shows: the talk on, or the one coming being brought on.
    pub(crate) on: bool,
}

/// A presentation's capture rate: animations and video move smoothly.
const PRESENTATION_CAPTURE_RATE: f32 = 60.0;

impl VardaApp {
    /// Add a PowerPoint input's layer: a capture deck with no window yet,
    /// bound to the show's window once PowerPoint plays it.
    pub(crate) fn add_presentation_deck(
        &mut self,
        channel_uuid: &str,
        path: &str,
    ) -> Result<String> {
        let path = std::fs::canonicalize(path).with_context(|| format!("No file at {path}"))?;
        if !path.is_file() {
            anyhow::bail!("{} is not a file", path.display());
        }
        let channel_idx = self.resolve_channel(channel_uuid)?;
        let name = path.file_stem().map_or_else(
            || "PowerPoint".to_string(),
            |s| s.to_string_lossy().into_owned(),
        );
        let state = ScreenCaptureState {
            capture_id: crate::screen_capture::UNBOUND_CAPTURE_ID,
            identity: TargetIdentity::Window {
                app: POWERPOINT_ID.to_string(),
                title: name.clone(),
            },
            config: CaptureConfig {
                rate: PRESENTATION_CAPTURE_RATE,
                exclude_varda: false,
                scale_to: Some((self.render_width, self.render_height)),
                ..CaptureConfig::default()
            },
            config_dirty: false,
            presentation: Some(PresentationConfig {
                path: path.to_string_lossy().into_owned(),
                slide: None,
                plays_on: PlaysOn::Auto,
            }),
        };
        let mut deck = Deck::new_from_screen_capture(
            &self.context,
            state,
            &name,
            self.render_width,
            self.render_height,
            self.render_width,
            self.render_height,
        )?;
        deck.set_source_name(format!("📽 {name}"));
        let uuid = deck.uuid().to_string();
        self.add_default_effects(&mut deck);
        let ch = self
            .mixer
            .channel_mut(channel_idx)
            .context("Invalid channel")?;
        ch.add_deck(deck);
        log::info!("Added PowerPoint input {uuid}: {}", path.display());
        Ok(uuid)
    }

    /// Add a presenter view layer of the PowerPoint input whose layer is
    /// `follows`.
    pub(crate) fn add_presenter_deck(
        &mut self,
        channel_uuid: &str,
        follows: &str,
    ) -> Result<String> {
        let (fch, fdk) = self
            .mixer
            .find_deck_by_uuid(follows)
            .with_context(|| format!("No layer {follows}"))?;
        let followed = &self.mixer.channels()[fch].decks[fdk].deck;
        let stem = if followed.slides().is_some() {
            "Slides".to_string()
        } else {
            let config = followed
                .screen_capture
                .as_ref()
                .and_then(|s| s.presentation.as_ref())
                .context("That layer is not a PowerPoint or Slides input's")?;
            Path::new(&config.path).file_stem().map_or_else(
                || "PowerPoint".to_string(),
                |s| s.to_string_lossy().into_owned(),
            )
        };
        let channel_idx = self.resolve_channel(channel_uuid)?;
        let mut deck = Deck::new_presenter(
            &self.context,
            follows.to_string(),
            &format!("Presenter — {stem}"),
            self.render_width,
            self.render_height,
        )?;
        let uuid = deck.uuid().to_string();
        self.add_default_effects(&mut deck);
        self.mixer
            .channel_mut(channel_idx)
            .context("Invalid channel")?
            .add_deck(deck);
        log::info!("Added presenter view {uuid} of PowerPoint input {follows}");
        Ok(uuid)
    }

    /// A presenter view's talk clock back to 0:00.
    pub(crate) fn restart_presenter_clock(&mut self, deck_uuid: &str) -> Result<()> {
        let (ch, dk) = self.resolve_deck(deck_uuid)?;
        self.mixer.channels_mut()[ch].decks[dk]
            .deck
            .presenter_mut()
            .context("That layer is not a presenter view")?
            .restart_clock();
        Ok(())
    }

    /// Lay the presenter view `deck_uuid` out as `layout` has it.
    pub(crate) fn set_presenter_layout(
        &mut self,
        deck_uuid: &str,
        layout: crate::engine::value::presentation::PresenterLayout,
    ) -> Result<()> {
        let (ch, dk) = self.resolve_deck(deck_uuid)?;
        self.mixer.channels_mut()[ch].decks[dk]
            .deck
            .presenter_mut()
            .context("That layer is not a presenter view")?
            .set_layout(layout);
        Ok(())
    }

    /// Each frame: every presenter view says what its show says — or that
    /// its PowerPoint input is gone.
    pub(crate) fn feed_presenters(&mut self) {
        let presenters: Vec<(String, String)> = self
            .mixer
            .channels()
            .iter()
            .flat_map(|ch| &ch.decks)
            .filter_map(|slot| {
                let p = slot.deck.presenter()?;
                Some((slot.deck.uuid().to_string(), p.follows().to_string()))
            })
            .collect();
        let alive: std::collections::HashSet<String> =
            presenters.iter().map(|(uuid, _)| uuid.clone()).collect();
        self.presentations
            .fed
            .retain(|uuid, _| alive.contains(uuid));
        for (uuid, follows) in presenters {
            let followed = self.mixer.find_deck_by_uuid(&follows);
            // A Slides input: its talk on, whatever plays it.
            let talk_show = followed
                .and_then(|(ch, dk)| self.slides_show(&self.mixer.channels()[ch].decks[dk].deck));
            let is_slides = followed.is_some_and(|(ch, dk)| {
                self.mixer.channels()[ch].decks[dk].deck.slides().is_some()
            });
            if is_slides {
                // A PDF talk's next page is the next slide's picture; a
                // PowerPoint talk's, once its slides are saved as pictures.
                let followed_deck =
                    followed.map(|(ch, dk)| &self.mixer.channels()[ch].decks[dk].deck);
                let pdf = followed_deck.and_then(super::slides::next_pictures_of);
                let powerpoint = followed_deck.and_then(super::slides::powerpoint_talk_on);
                let (talk, show) = talk_show.map_or((None, None), |(t, s)| (Some(t), Some(s)));
                let next = match (pdf, powerpoint) {
                    (Some(source), _) => (Some(source), show.as_ref().and_then(|s| s.next_slide)),
                    (None, Some((talk_id, path))) => {
                        self.slide_pictures_next(&talk_id, &path, show.as_ref())
                    }
                    (None, None) => (None, None),
                };
                let last = self.presentations.fed.get(&uuid);
                let new_talk =
                    talk.is_some() && last.and_then(|f| f.talk.as_ref()) != talk.as_ref();
                let fresh = self.mixer.find_deck_by_uuid(&uuid).is_some_and(|(ch, dk)| {
                    self.mixer.channels()[ch].decks[dk]
                        .deck
                        .presenter()
                        .is_some_and(|p| *p.words() == crate::deck::presenter::Words::default())
                });
                if !fresh
                    && !new_talk
                    && last.and_then(|f| f.slides.as_ref()) == show.as_ref()
                    && last.map(|f| &f.next) == Some(&next)
                {
                    continue;
                }
                if let Some((ch, dk)) = self.mixer.find_deck_by_uuid(&uuid) {
                    if let Some(p) = self.mixer.channels_mut()[ch].decks[dk].deck.presenter_mut() {
                        // Each talk's clock starts as it comes on.
                        if new_talk {
                            p.restart_clock();
                        }
                        p.set_show(show.as_ref());
                        p.set_next_picture(next.0.clone(), next.1);
                    }
                }
                self.presentations.fed.insert(
                    uuid,
                    Fed {
                        gone: false,
                        status: None,
                        show: None,
                        notes: false,
                        talk,
                        slides: show,
                        next,
                    },
                );
                continue;
            }
            let view = self
                .presentations
                .manager
                .as_ref()
                .and_then(|m| m.view(&follows));
            let now = Fed {
                gone: followed.is_none(),
                status: view.map(|v| v.status.clone()),
                show: view.and_then(|v| v.show),
                notes: view.is_some_and(|v| v.doc.is_some()),
                talk: None,
                slides: None,
                next: (None, None),
            };
            // A presenter view made again (an undo, a show opened) is told
            // at once, though its show is as it was.
            let fresh = self.mixer.find_deck_by_uuid(&uuid).is_some_and(|(ch, dk)| {
                self.mixer.channels()[ch].decks[dk]
                    .deck
                    .presenter()
                    .is_some_and(|p| *p.words() == crate::deck::presenter::Words::default())
            });
            if !fresh && self.presentations.fed.get(&uuid) == Some(&now) {
                continue;
            }
            let show = followed.and_then(|(ch, dk)| {
                self.presentation_snapshot(&self.mixer.channels()[ch].decks[dk].deck)
            });
            if let Some((ch, dk)) = self.mixer.find_deck_by_uuid(&uuid) {
                if let Some(p) = self.mixer.channels_mut()[ch].decks[dk].deck.presenter_mut() {
                    p.set_show(show.as_ref());
                    // A PowerPoint input's — or one gone, a Slides input
                    // among them: no next slide's picture.
                    p.set_next_picture(None, None);
                }
            }
            self.presentations.fed.insert(uuid, now);
        }
    }

    /// The layers PowerPoint's one sound belongs to, by deck UUID: the
    /// PowerPoint inputs, and the Slides inputs with a PowerPoint talk.
    pub(crate) fn powerpoint_decks(&self) -> Vec<String> {
        let mut decks = self.powerpoint_inputs();
        decks.extend(
            self.mixer
                .channels()
                .iter()
                .flat_map(|ch| &ch.decks)
                .filter(|slot| {
                    slot.deck.slides().is_some_and(|s| {
                        s.talks()
                            .iter()
                            .any(|t| t.kind() == Some(TalkKind::PowerPoint))
                    })
                })
                .map(|slot| slot.deck.uuid().to_string()),
        );
        decks
    }

    /// The PowerPoint inputs, by deck UUID.
    pub(crate) fn powerpoint_inputs(&self) -> Vec<String> {
        self.presentation_decks()
            .into_iter()
            .map(|(deck, ..)| deck)
            .collect()
    }

    /// Whether the sound input `input`, tied to the deck `deck`, is
    /// PowerPoint's one sound, shared by every PowerPoint input: PowerPoint's,
    /// tied to one of `powerpoint` — or to an input gone since, as an undo can
    /// leave it. Tied to another kind of input, it follows that one alone.
    pub(crate) fn shares_powerpoint_sound(
        &self,
        input: &str,
        deck: &str,
        powerpoint: &[String],
    ) -> bool {
        self.audio_mix.app_of(input) == Some(crate::presentation::powerpoint::POWERPOINT_ID)
            && (powerpoint.iter().any(|d| d == deck)
                || self.mixer.find_deck_by_uuid(deck).is_none())
    }

    /// Whether the sound input `input`, tied to the deck `deck`, is web
    /// talks' one sound, shared by every Slides input: their pages' sound
    /// all comes from the one `WebKit` (its GPU process plays every page's),
    /// so it is one strip, as PowerPoint's is — tied to one of `slides`, or
    /// to an input gone since.
    pub(crate) fn shares_web_sound(&self, input: &str, deck: &str, slides: &[String]) -> bool {
        self.audio_mix.app_of(input) == Some(crate::audio::app_sound::WEB_TALKS)
            && (slides.iter().any(|d| d == deck) || self.mixer.find_deck_by_uuid(deck).is_none())
    }

    /// The strip that is web talks' shared sound: the first.
    pub(crate) fn web_sound_strip(&self, slides: &[String]) -> Option<String> {
        self.audio_mix
            .config()
            .inputs
            .iter()
            .find(|i| {
                i.deck
                    .as_deref()
                    .is_some_and(|d| self.shares_web_sound(&i.uuid, d, slides))
            })
            .map(|i| i.uuid.clone())
    }

    /// The strip that is PowerPoint's shared sound: the first, should a show
    /// saved by an older Nida hold two.
    pub(crate) fn powerpoint_sound_strip(&self, powerpoint: &[String]) -> Option<String> {
        self.audio_mix
            .config()
            .inputs
            .iter()
            .find(|i| {
                i.deck
                    .as_deref()
                    .is_some_and(|d| self.shares_powerpoint_sound(&i.uuid, d, powerpoint))
            })
            .map(|i| i.uuid.clone())
    }

    /// The decks that are PowerPoint inputs: UUID, file, slide saved, and
    /// the screen they are to play on.
    pub(crate) fn presentation_decks(&self) -> Vec<(String, String, Option<u32>, PlaysOn)> {
        self.mixer
            .channels()
            .iter()
            .flat_map(|ch| &ch.decks)
            .filter_map(|slot| {
                let p = slot.deck.screen_capture.as_ref()?.presentation.as_ref()?;
                Some((
                    slot.deck.uuid().to_string(),
                    p.path.clone(),
                    p.slide,
                    p.plays_on.clone(),
                ))
            })
            .collect()
    }

    /// The connected screens, read again a second on — or as a test gave.
    fn presentation_screens(&mut self) -> &[ScreenSpot] {
        let p = &mut self.presentations;
        let due = p
            .screens_at
            .is_none_or(|at| at.elapsed() >= std::time::Duration::from_secs(1));
        if due && !p.screens_given {
            p.screens = screens::connected();
            p.screens_at = Some(std::time::Instant::now());
        }
        &p.screens
    }

    /// The screens Nida's outputs are on, or are waiting for: a show's
    /// window does not go there — the room would see it.
    fn output_displays(&self) -> std::collections::HashSet<u32> {
        let waiting: Vec<&str> = self
            .output
            .outputs
            .iter()
            .filter_map(|output| match output {
                crate::renderer::context::UnifiedOutput::Window(w) => {
                    w.awaiting_display.as_ref().map(|a| a.monitor.as_str())
                }
                crate::renderer::context::UnifiedOutput::Headless(_) => None,
            })
            .collect();
        let on: Vec<u32> = self
            .output
            .outputs
            .iter()
            .filter_map(|output| match output {
                crate::renderer::context::UnifiedOutput::Window(w) => w.display_id(),
                crate::renderer::context::UnifiedOutput::Headless(_) => None,
            })
            .collect();
        // A screen an output waits for, by its name as outputs address it.
        let waited = self
            .output
            .cached_monitors
            .iter()
            .zip(&self.output.screen_facts)
            .filter(|((name, _), _)| waiting.contains(&name.as_str()))
            .filter_map(|(_, fact)| fact.display_id);
        on.into_iter().chain(waited).collect()
    }

    /// The screen a deck's show plays on, as its input asks.
    fn presentation_spot(
        &self,
        deck: &str,
        plays_on: &PlaysOn,
        taken: &std::collections::HashSet<u32>,
    ) -> Option<ScreenSpot> {
        let want = match plays_on {
            PlaysOn::Auto => Want::Auto,
            PlaysOn::Main => Want::Main,
            PlaysOn::Screen(identity) => self
                .output
                .screen_facts
                .iter()
                .find(|f| f.identity == *identity)
                .and_then(|f| f.display_id)
                .map_or(Want::Auto, Want::Display),
        };
        let aspect = self
            .presentations
            .manager
            .as_ref()
            .and_then(|m| m.view(deck))
            .and_then(|v| v.doc.as_ref())
            .map_or(16.0 / 9.0, crate::presentation::pptx::Document::aspect);
        screens::choose(
            &self.presentations.screens,
            want,
            taken,
            (self.render_width, self.render_height),
            aspect,
            self.presentations.title_bar,
        )
    }

    /// How the screen `spot` is named: the name given it, else what macOS
    /// calls it — this Mac's own said so.
    fn presentation_screen_name(&self, spot: &ScreenSpot) -> String {
        let name = self
            .output
            .screen_facts
            .iter()
            .find(|f| f.display_id == Some(spot.display))
            .and_then(|f| {
                self.output
                    .screen_names
                    .get(&f.identity)
                    .map(str::to_string)
                    .or_else(|| f.model.clone())
            });
        match (name, spot.main) {
            (Some(name), true) => format!("{name} (this Mac's screen)"),
            (Some(name), false) => name,
            (None, true) => "this Mac's screen".to_string(),
            (None, false) => format!("screen {}", spot.display),
        }
    }

    /// Each frame: open what is new, close what is gone, capture each show's
    /// window when found, and keep each deck's slide for saving — for
    /// PowerPoint inputs and for the Slides talks PowerPoint plays: the talk
    /// on and the one coming, their shows running; the next and the one
    /// before, opened ahead.
    pub(crate) fn reconcile_presentations(&mut self) {
        let wanted = self.presentation_decks();
        let talks = self.powerpoint_talks();
        if self.presentations.manager.is_none()
            && ((wanted.is_empty() && talks.is_empty()) || self.presentations.disabled)
        {
            return;
        }
        let pixels = (self.render_width, self.render_height);
        self.presentations
            .manager
            .get_or_insert_with(PresentationManager::new);
        // Each show on the screen its input asks for — moved when that
        // changes: a spare screen plugged in or taken by an output.
        self.presentation_screens();
        let taken = self.output_displays();
        let spots: Vec<Option<ScreenSpot>> = wanted
            .iter()
            .map(|(deck, _, _, plays_on)| self.presentation_spot(deck, plays_on, &taken))
            .collect();
        let talk_spots: Vec<Option<ScreenSpot>> = talks
            .iter()
            .map(|t| self.presentation_spot(&t.key, &t.plays_on, &taken))
            .collect();
        let Some(manager) = self.presentations.manager.as_mut() else {
            return;
        };
        let sent = &mut self.presentations.spots;
        for ((deck, path, slide, plays_on), spot) in wanted.iter().zip(spots) {
            let asked = spot.map(|spot| (spot, plays_on.clone()));
            if manager.view(deck).map(|v| v.path.as_path()) != Some(Path::new(path)) {
                manager.open(deck, Path::new(path), *slide, pixels, spot);
                if let Some(asked) = asked {
                    sent.insert(deck.clone(), asked);
                }
            } else if let Some(asked) = asked.filter(|a| sent.get(deck) != Some(a)) {
                manager.place(deck, asked.0);
                sent.insert(deck.clone(), asked);
            }
        }
        for (t, spot) in talks.iter().zip(talk_spots) {
            let asked = spot.map(|spot| (spot, t.plays_on.clone()));
            if let Some(wants_show) = manager.view(&t.key).map(|v| v.wants_show) {
                if wants_show != t.show {
                    manager.set_show(&t.key, t.show, t.slide);
                }
                if let Some(asked) = asked.filter(|a| sent.get(&t.key) != Some(a)) {
                    manager.place(&t.key, asked.0);
                    sent.insert(t.key.clone(), asked);
                }
            } else {
                if t.show {
                    manager.open(&t.key, Path::new(&t.path), t.slide, pixels, spot);
                } else {
                    manager.prepare(&t.key, Path::new(&t.path), pixels, spot);
                }
                if let Some(asked) = asked {
                    sent.insert(t.key.clone(), asked);
                }
            }
        }
        let gone: Vec<(String, std::path::PathBuf)> = manager
            .decks()
            .filter(|(deck, _)| {
                !wanted.iter().any(|(w, ..)| w == deck) && !talks.iter().any(|t| t.key == *deck)
            })
            .map(|(deck, path)| (deck.to_string(), path.to_path_buf()))
            .collect();
        for (deck, path) in gone {
            manager.close(&deck);
            sent.remove(&deck);
            // The show was the file's, and this input may have moved it: the
            // file's other inputs put it where they ask again.
            for (other, other_path, ..) in &wanted {
                if Path::new(other_path) == path {
                    sent.remove(other);
                }
            }
            for t in &talks {
                if Path::new(&t.path) == path {
                    sent.remove(&t.key);
                }
            }
        }
        let windows = manager.poll();
        let slides: Vec<(String, u32)> = wanted
            .iter()
            .filter_map(|(deck, _, saved, _)| {
                let now = manager.view(deck)?.show?.slide;
                (Some(now) != *saved).then(|| (deck.clone(), now))
            })
            .collect();
        let talk_slides: Vec<(String, String, String, u32)> = talks
            .iter()
            .filter(|t| t.show)
            .filter_map(|t| {
                let now = manager.view(&t.key)?.show?.slide;
                (Some(now) != t.slide)
                    .then(|| (t.slides.clone(), t.talk.clone(), t.path.clone(), now))
            })
            .collect();
        for found in windows {
            self.bind_presentation_window(found);
        }
        self.take_slide_pictures();
        // A deck with no picture of a running show — its capture failed or
        // is gone, or an undo made it again — asks for its window again.
        let mut unbound: Vec<String> = self
            .mixer
            .channels()
            .iter()
            .flat_map(|ch| &ch.decks)
            .filter_map(|slot| {
                let state = slot.deck.screen_capture.as_ref()?;
                (state.presentation.is_some()
                    && (state.capture_id == crate::screen_capture::UNBOUND_CAPTURE_ID
                        || !self.screen_capture_manager.is_active(state.capture_id)))
                .then(|| slot.deck.uuid().to_string())
            })
            .collect();
        // So does a talk on or coming whose show's window is not captured.
        for slot in self.mixer.channels().iter().flat_map(|ch| &ch.decks) {
            if let Some(s) = slot.deck.slides() {
                for open in s.open_talks() {
                    if open.kind == TalkKind::PowerPoint
                        && open
                            .capture
                            .is_none_or(|id| !self.screen_capture_manager.is_active(id))
                    {
                        unbound.push(talk_key(slot.deck.uuid(), &open.id, &open.path));
                    }
                }
            }
        }
        if let Some(manager) = self.presentations.manager.as_mut() {
            let now = std::time::Instant::now();
            for deck in unbound {
                manager.want_window(&deck, now);
            }
        }
        for (deck, slide) in slides {
            if let Some((ch, dk)) = self.mixer.find_deck_by_uuid(&deck) {
                let state = self.mixer.channels_mut()[ch].decks[dk]
                    .deck
                    .screen_capture
                    .as_mut();
                if let Some(p) = state.and_then(|s| s.presentation.as_mut()) {
                    p.slide = Some(slide);
                }
            }
        }
        for (deck, talk, path, slide) in talk_slides {
            if let Some((ch, dk)) = self.mixer.find_deck_by_uuid(&deck) {
                if let Some(s) = self.mixer.channels_mut()[ch].decks[dk].deck.slides_mut() {
                    s.set_talk_slide(&talk, &path, slide);
                }
            }
        }
    }

    /// The Slides talks PowerPoint plays, each input's: its talk on and the
    /// one coming (shows running), the next and the one before (opened
    /// ahead).
    pub(crate) fn powerpoint_talks(&self) -> Vec<TalkShow> {
        self.talk_shows(TalkKind::PowerPoint)
    }

    /// The Slides talks of `kind` kept open, each input's: its talk on and
    /// the one coming (playing), the next and the one before (opened ahead).
    pub(crate) fn talk_shows(&self, kind: TalkKind) -> Vec<TalkShow> {
        let mut out = Vec::new();
        for slot in self.mixer.channels().iter().flat_map(|ch| &ch.decks) {
            let Some(slides) = slot.deck.slides() else {
                continue;
            };
            for w in slides.wanted() {
                let key = talk_key(slot.deck.uuid(), &w.id, &w.path);
                if w.kind != kind || out.iter().any(|t: &TalkShow| t.key == key) {
                    continue;
                }
                out.push(TalkShow {
                    key,
                    slides: slot.deck.uuid().to_string(),
                    talk: w.id,
                    path: w.path,
                    slide: w.slide,
                    plays_on: slides.settings().plays_on.clone(),
                    show: w.show,
                    on: w.on,
                });
            }
        }
        out
    }

    /// Capture the window a deck's show plays in: the capture was opened on
    /// the worker's thread, so taking it holds no frame.
    fn bind_presentation_window(&mut self, found: crate::presentation::manager::Found) {
        let crate::presentation::manager::Found {
            deck,
            target,
            capture,
            config,
        } = found;
        let Some((ch, dk)) = self.mixer.find_deck_by_uuid(&deck) else {
            // A Slides talk's show: its deck, while the talk is on or coming.
            self.bind_talk_window(&deck, &target, capture, config, false);
            return;
        };
        if self.mixer.channels()[ch].decks[dk]
            .deck
            .screen_capture
            .is_none()
        {
            return;
        }
        let adopted = self.screen_capture_manager.adopt(
            &target,
            capture,
            config.clone(),
            &self.context.device,
        );
        let (id, width, height) = match adopted {
            Ok(bound) => bound,
            Err(e) => {
                log::warn!(
                    "PowerPoint show's window '{}' could not be captured: {e}",
                    target.label
                );
                return;
            }
        };
        let old = self.mixer.channels_mut()[ch].decks[dk]
            .deck
            .rebind_screen_capture(id, target.identity(), config, width, height);
        if let Some(old) =
            old.filter(|old| *old != id && *old != crate::screen_capture::UNBOUND_CAPTURE_ID)
        {
            self.screen_capture_manager.release(old);
        }
        log::info!("PowerPoint input {deck} captures '{}'", target.label);
    }

    /// Capture the window a Slides talk's show plays in, for the talk's deck
    /// — kept only while the talk is on or coming.
    /// `fresh`: the window captured again because what captured it sent
    /// no picture — put in its place, never shared with it.
    pub(crate) fn bind_talk_window(
        &mut self,
        key: &str,
        target: &crate::screen_capture::backend::CaptureTargetInfo,
        capture: Box<dyn crate::screen_capture::backend::ScreenCaptureBackend>,
        config: CaptureConfig,
        fresh: bool,
    ) {
        // Only for a talk still on or coming with that very file: a window
        // found for a session since gone is let go.
        let Some(show) = self
            .powerpoint_talks()
            .into_iter()
            .chain(self.talk_shows(TalkKind::Web))
            .find(|t| t.key == key && t.show)
        else {
            return;
        };
        let (talk, path) = (show.talk.as_str(), show.path.as_str());
        let Some((ch, dk)) = self.mixer.find_deck_by_uuid(&show.slides) else {
            return;
        };
        let open = self.mixer.channels_mut()[ch].decks[dk]
            .deck
            .slides_mut()
            .and_then(|s| s.talk_deck_mut(talk, path))
            .is_some_and(|d| d.screen_capture.is_some());
        if !open {
            return;
        }
        let adopted = if fresh {
            self.screen_capture_manager
                .adopt_afresh(target, capture, config.clone(), &self.context.device)
        } else {
            self.screen_capture_manager
                .adopt(target, capture, config.clone(), &self.context.device)
        };
        let (id, width, height) = match adopted {
            Ok(bound) => bound,
            Err(e) => {
                log::warn!(
                    "Slides talk's window '{}' could not be captured: {e}",
                    target.label
                );
                return;
            }
        };
        let old = self.mixer.channels_mut()[ch].decks[dk]
            .deck
            .slides_mut()
            .and_then(|s| s.talk_deck_mut(talk, path))
            .and_then(|d| d.rebind_screen_capture(id, target.identity(), config, width, height));
        if let Some(old) =
            old.filter(|old| *old != id && *old != crate::screen_capture::UNBOUND_CAPTURE_ID)
        {
            self.screen_capture_manager.release(old);
        }
        log::info!("Slides talk {talk} captures '{}'", target.label);
    }

    /// The PowerPoint or Slides input a step goes to: the one named, else
    /// the one on air (on Main or a bus), else the one moved last, else the
    /// first.
    pub(crate) fn show_target(&self, deck_uuid: Option<&str>) -> Result<ShowTarget> {
        let decks = self.powerpoint_inputs();
        let slides = self.slides_decks();
        let of = |uuid: &str| {
            if decks.iter().any(|d| d == uuid) {
                Some(ShowTarget::Deck(uuid.to_string()))
            } else if slides.iter().any(|d| d == uuid) {
                Some(ShowTarget::Slides(uuid.to_string()))
            } else {
                None
            }
        };
        if let Some(uuid) = deck_uuid {
            return of(uuid).with_context(|| format!("{uuid} is not a PowerPoint or Slides input"));
        }
        if decks.is_empty() && slides.is_empty() {
            anyhow::bail!("There is no PowerPoint or Slides input");
        }
        let on_air = self.on_air();
        let most = slides
            .iter()
            .chain(&decks)
            .map(|d| (d, on_air.anywhere(d)))
            .filter(|(_, level)| *level > 0.0)
            .max_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((deck, _)) = most {
            return of(deck).context("gone");
        }
        if let Some(last) = self.presentations.last.as_deref().and_then(of) {
            return Ok(last);
        }
        slides
            .first()
            .or_else(|| decks.first())
            .and_then(|d| of(d))
            .context("There is no PowerPoint or Slides input")
    }

    /// Play, pause or move the video on the slide on screen of `deck_uuid`'s
    /// show — or, without one, the show on air (see `show_target`).
    pub(crate) fn presentation_video(
        &mut self,
        deck_uuid: Option<&str>,
        control: crate::engine::value::presentation::VideoControl,
    ) -> Result<()> {
        let target = self.show_target(deck_uuid)?;
        if let Some(key) = self.show_key(&target) {
            if let Some(manager) = &self.presentations.manager {
                manager.video(&key, control);
            }
        }
        self.presentations.last = Some(target.uuid().to_string());
        Ok(())
    }

    pub(crate) fn presentation_go(&mut self, deck_uuid: Option<&str>, to: Go) -> Result<()> {
        let target = self.show_target(deck_uuid)?;
        match &target {
            ShowTarget::Deck(deck) => {
                if let Some(manager) = &self.presentations.manager {
                    manager.go(deck, to);
                }
            }
            ShowTarget::Slides(deck) => self.slides_go(deck, to)?,
        }
        self.presentations.last = Some(target.uuid().to_string());
        Ok(())
    }

    /// Show `screen` — or, `toggle`, the slide again when it already shows
    /// it.
    pub(crate) fn presentation_screen(
        &mut self,
        deck_uuid: Option<&str>,
        screen: Screen,
        toggle: bool,
    ) -> Result<()> {
        let target = self.show_target(deck_uuid)?;
        match &target {
            ShowTarget::Deck(deck) => {
                if let Some(manager) = &self.presentations.manager {
                    let now = manager.view(deck).and_then(|v| v.show).map(|s| s.screen);
                    let screen = if toggle && now == Some(screen) {
                        Screen::Slide
                    } else {
                        screen
                    };
                    manager.screen(deck, screen);
                }
            }
            // Black or white over whatever talk is on, from Nida itself.
            ShowTarget::Slides(deck) => {
                self.edit_slides(deck, |s| {
                    let screen = if toggle && s.screen() == screen {
                        Screen::Slide
                    } else {
                        screen
                    };
                    s.set_screen(screen);
                })?;
            }
        }
        self.presentations.last = Some(target.uuid().to_string());
        Ok(())
    }

    /// The key PowerPoint's show of `target` goes by: a PowerPoint input's
    /// layer, or a Slides input's talk on when PowerPoint plays it.
    pub(crate) fn show_key(&self, target: &ShowTarget) -> Option<String> {
        match target {
            ShowTarget::Deck(deck) => Some(deck.clone()),
            ShowTarget::Slides(deck) => {
                let (ch, dk) = self.mixer.find_deck_by_uuid(deck)?;
                let slides = self.mixer.channels()[ch].decks[dk].deck.slides()?;
                let (id, kind) = slides.live_talk()?;
                if kind != TalkKind::PowerPoint {
                    return None;
                }
                Some(talk_key(deck, id, slides.open_path(id)?))
            }
        }
    }

    /// Which screen a PowerPoint input's show plays on — or a Slides
    /// input's PowerPoint talks': moved there at the next frame.
    pub(crate) fn set_presentation_plays_on(
        &mut self,
        deck_uuid: &str,
        plays_on: PlaysOn,
    ) -> Result<()> {
        let (ch, dk) = self
            .mixer
            .find_deck_by_uuid(deck_uuid)
            .with_context(|| format!("no layer {deck_uuid}"))?;
        if let Some(slides) = self.mixer.channels_mut()[ch].decks[dk].deck.slides_mut() {
            let settings = crate::engine::value::slides::SlidesSettings {
                plays_on,
                ..slides.settings().clone()
            };
            slides.set_settings(settings);
            return Ok(());
        }
        let config = self.mixer.channels_mut()[ch].decks[dk]
            .deck
            .screen_capture
            .as_mut()
            .and_then(|s| s.presentation.as_mut())
            .with_context(|| format!("layer {deck_uuid} is not a PowerPoint input"))?;
        config.plays_on = plays_on;
        Ok(())
    }

    pub(crate) fn restart_presentation(&mut self, deck_uuid: &str) -> Result<()> {
        let target = self.show_target(Some(deck_uuid))?;
        if let Some(key) = self.show_key(&target) {
            if let Some(manager) = &self.presentations.manager {
                manager.restart(&key);
            }
        }
        Ok(())
    }

    /// A deck's show for the API and the panels; `None` for any other deck.
    pub(crate) fn presentation_snapshot(&self, deck: &Deck) -> Option<PresentationSnapshot> {
        let config = deck.screen_capture.as_ref()?.presentation.as_ref()?;
        Some(self.show_snapshot(deck.uuid(), &config.path, &config.plays_on))
    }

    /// The show PowerPoint plays for `key` — a PowerPoint input's layer, or
    /// a Slides talk — of the deck at `path`, to play on `plays_on`.
    pub(crate) fn show_snapshot(
        &self,
        key: &str,
        path: &str,
        plays_on: &PlaysOn,
    ) -> PresentationSnapshot {
        let view = self
            .presentations
            .manager
            .as_ref()
            .and_then(|m| m.view(key));
        let (status, message) = match view.map(|v| &v.status) {
            None if self.presentations.disabled && self.presentations.manager.is_none() => (
                PresentationStatus::Failed,
                Some("PowerPoint inputs are off (--no-powerpoint)".to_string()),
            ),
            None | Some(Status::Opening) => (PresentationStatus::Opening, None),
            Some(Status::Showing) => (PresentationStatus::Showing, None),
            Some(Status::Ready) => (PresentationStatus::Ready, None),
            Some(Status::NoWindow(why)) => (
                PresentationStatus::NoWindow,
                Some(format!("PowerPoint plays the show, but {why}")),
            ),
            Some(Status::Failed(e)) => (PresentationStatus::Failed, Some(e.to_string())),
        };
        let show = view.and_then(|v| v.show);
        let doc = view.and_then(|v| v.doc.as_ref());
        let slide = show.map(|s| s.slide);
        // Where its window is, and the slide's pixels there: under its
        // title bar, at that screen's pixels.
        let playing_on = show.and_then(|s| {
            self.presentations
                .screens
                .iter()
                .find(|screen| screen.holds(s.window))
        });
        let slide_pixels = show.zip(playing_on).map(|(s, screen)| {
            let bar = i32::try_from(self.presentations.title_bar).unwrap_or(0);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let px = |points: i32| (f64::from(points.max(0)) * screen.scale).round() as u32;
            [px(s.window.width), px(s.window.height - bar)]
        });
        let at = |n: u32| doc.and_then(|d| d.slides.get(n.checked_sub(1)? as usize));
        let shown = doc.map_or_else(
            || show.map(|s| s.slides),
            |d| u32::try_from(d.slides.iter().filter(|s| !s.hidden).count()).ok(),
        );
        // The next slide the show plays: hidden ones are skipped.
        let next_slide = slide.and_then(|n| match doc {
            Some(d) => (n + 1..=u32::try_from(d.slides.len()).unwrap_or(0))
                .find(|m| at(*m).is_some_and(|s| !s.hidden)),
            None => show.filter(|s| n < s.slides).map(|_| n + 1),
        });
        PresentationSnapshot {
            path: path.to_string(),
            status,
            message,
            slide,
            slides: show.map(|s| s.slides),
            position: show.map(|s| s.position),
            shown,
            screen: show.map(|s| s.screen).unwrap_or_default(),
            title: slide.and_then(at).and_then(|s| s.title.clone()),
            notes: slide
                .and_then(at)
                .map(|s| s.notes.clone())
                .unwrap_or_default(),
            next_slide,
            next_title: next_slide.and_then(at).and_then(|s| s.title.clone()),
            plays_on: plays_on.clone(),
            playing_on: playing_on.map(|spot| self.presentation_screen_name(spot)),
            slide_pixels,
            video: view.and_then(|v| v.video).map(|v| {
                crate::engine::value::presentation::VideoSnapshot {
                    state: v.state,
                    position_ms: v.position_ms,
                }
            }),
        }
    }

    /// Have PowerPoint save the PowerPoint talks of a Slides input — or of
    /// every one — as slide pictures, for the presenter view's next slide:
    /// those without them as their files are now (see
    /// `crate::presentation::pictures`). Never with a PowerPoint show
    /// running: PowerPoint may ask for access to Nida's folder for them
    /// first (Grant Access), and holds its show till that is answered.
    pub(crate) fn make_slide_pictures(&mut self, deck_uuid: Option<&str>) -> Result<()> {
        if let Some(uuid) = deck_uuid {
            let (ch, dk) = self.resolve_deck(uuid)?;
            self.mixer.channels()[ch].decks[dk]
                .deck
                .slides()
                .with_context(|| format!("{uuid} is not a Slides input"))?;
        }
        let mut paths: Vec<std::path::PathBuf> = self
            .mixer
            .channels()
            .iter()
            .flat_map(|ch| &ch.decks)
            .filter(|slot| deck_uuid.is_none_or(|u| u == slot.deck.uuid()))
            .filter_map(|slot| slot.deck.slides())
            .flat_map(crate::deck::slides::Slides::talks)
            .filter(|talk| talk.kind() == Some(TalkKind::PowerPoint))
            .map(|talk| std::path::PathBuf::from(&talk.path))
            .collect();
        paths.sort();
        paths.dedup();
        let manager = self
            .presentations
            .manager
            .as_ref()
            .context("PowerPoint plays nothing of this show yet")?;
        anyhow::ensure!(
            !manager.any_showing(),
            "A PowerPoint talk is on: make the slide pictures with none on — PowerPoint may ask \
             for access to Nida's folder first (Grant Access), and holds its show till that is \
             answered"
        );
        for path in paths {
            if self.presentations.making.contains(&path)
                || crate::presentation::pictures::saved_for(&path).is_some()
            {
                continue;
            }
            let Some(out) = crate::presentation::pictures::pdf_for(&path) else {
                continue;
            };
            manager.save_pdf(&path, &out);
            self.presentations.making.insert(path);
        }
        Ok(())
    }

    /// Take what PowerPoint made of the slide pictures asked: said when it
    /// could not.
    fn take_slide_pictures(&mut self) {
        let saved = self
            .presentations
            .manager
            .as_mut()
            .map(PresentationManager::take_saved)
            .unwrap_or_default();
        // Said once for the decks that failed alike: a batch dropped is one
        // notice.
        let mut failed: Vec<(String, Vec<String>)> = Vec::new();
        for saved in saved {
            self.presentations.making.remove(&saved.path);
            if let Err(why) = saved.result {
                let file = saved
                    .path
                    .file_name()
                    .map_or_else(String::new, |f| f.to_string_lossy().into_owned());
                match failed.iter_mut().find(|(w, _)| *w == why) {
                    Some((_, files)) => files.push(file),
                    None => failed.push((why, vec![file])),
                }
            }
        }
        for (why, files) in failed {
            self.notify_error(format!("Slide pictures of {}: {why}", files.join(", ")));
        }
    }

    /// The next slide's pictures of the PowerPoint talk `talk` (its file
    /// `path`) and their page as `show` stands, once its slides are saved as
    /// pictures; none till then.
    fn slide_pictures_next(
        &mut self,
        talk: &str,
        path: &Path,
        show: Option<&PresentationSnapshot>,
    ) -> (Option<crate::deck::presenter::NextSource>, Option<u32>) {
        let Some(pdf) = crate::presentation::pictures::saved_for(path) else {
            return (None, None);
        };
        let stamp = std::fs::metadata(&pdf)
            .ok()
            .and_then(|m| Some((m.len(), m.modified().ok()?)));
        let pages = match self.presentations.pdf_pages.get(&pdf) {
            Some((counted, n)) if *counted == stamp => Some(*n),
            _ => crate::presentation::pdf::PdfPages::open(&pdf)
                .ok()
                .map(|doc| doc.pages())
                .inspect(|n| {
                    self.presentations.pdf_pages.insert(pdf.clone(), (stamp, *n));
                }),
        };
        let page = show.zip(pages).and_then(|(s, pages)| {
            crate::presentation::pictures::page_of_next(
                pages,
                s.next_slide,
                s.slides,
                s.position,
                s.shown,
            )
        });
        (
            Some(crate::deck::presenter::NextSource {
                talk: talk.to_string(),
                path: pdf,
                stamp,
            }),
            page,
        )
    }

    /// Drive PowerPoint inputs with `manager` — a stand-in PowerPoint, in
    /// tests.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn set_presentation_manager(&mut self, manager: PresentationManager) {
        self.presentations.manager = Some(manager);
    }

    /// The screens connected, as a test has them: read from macOS no more.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn set_presentation_screens(&mut self, screens: Vec<ScreenSpot>) {
        self.presentations.screens = screens;
        self.presentations.screens_given = true;
    }
}

#[cfg(test)]
mod tests {
    /// Every command and input the manual's page shows is one Nida takes.
    #[test]
    fn the_manuals_examples_are_commands_nida_takes() {
        let manual = include_str!("../../../docs/26-presentations.md");
        let mut commands = 0;
        for line in manual.lines() {
            if let Some(json) = line.strip_prefix("POST /api/command") {
                let parsed = serde_json::from_str::<crate::engine::EngineCommand>(json.trim());
                assert!(parsed.is_ok(), "{line}: {parsed:?}");
                commands += 1;
            } else if let Some(json) = line.strip_prefix("POST /api/inputs") {
                let body: serde_json::Value = serde_json::from_str(json.trim()).unwrap();
                let source =
                    serde_json::from_value::<crate::engine::InputSource>(body["source"].clone());
                assert!(
                    matches!(
                        source,
                        Ok(crate::engine::InputSource::PowerPoint { .. }
                            | crate::engine::InputSource::PresenterView { .. }
                            | crate::engine::InputSource::Slides { .. })
                    ),
                    "{line}: {source:?}"
                );
            }
        }
        assert_eq!(commands, 22);
    }

    /// Each screen a show can be told to play on, as the manual writes it.
    #[test]
    fn plays_on_reads_as_the_manual_writes_it() {
        use crate::engine::value::presentation::PlaysOn;
        for (json, want) in [
            (r#""auto""#, PlaysOn::Auto),
            (r#""main""#, PlaysOn::Main),
            (
                r#"{"screen": "37D8832A"}"#,
                PlaysOn::Screen("37D8832A".into()),
            ),
        ] {
            assert_eq!(serde_json::from_str::<PlaysOn>(json).unwrap(), want);
        }
        // Automatic is not written into a saved show.
        let config = crate::presentation::PresentationConfig {
            path: "/Talks/talk.pptx".into(),
            slide: None,
            plays_on: PlaysOn::Auto,
        };
        assert_eq!(
            serde_json::to_string(&config).unwrap(),
            r#"{"path":"/Talks/talk.pptx"}"#
        );
    }
}
