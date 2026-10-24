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
use crate::presentation::manager::{PresentationManager, Status};
use crate::presentation::powerpoint::POWERPOINT_ID;
use crate::presentation::screens::{self, ScreenSpot, Want};
use crate::presentation::PresentationConfig;
use crate::screen_capture::backend::{CaptureConfig, TargetIdentity};

/// The engine's PowerPoint state: the worker, started with the first
/// PowerPoint input, and the show moved last.
pub(crate) struct Presentations {
    manager: Option<PresentationManager>,
    /// Where a step with no deck named goes when no show is on air.
    last: Option<String>,
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
}

/// How a presenter view's show stood when it was last told.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Fed {
    gone: bool,
    status: Option<Status>,
    show: Option<crate::presentation::powerpoint::ShowState>,
    notes: bool,
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
        }
    }
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
        let config = self.mixer.channels()[fch].decks[fdk]
            .deck
            .screen_capture
            .as_ref()
            .and_then(|s| s.presentation.as_ref())
            .context("That layer is not a PowerPoint input's")?;
        let stem = Path::new(&config.path).file_stem().map_or_else(
            || "PowerPoint".to_string(),
            |s| s.to_string_lossy().into_owned(),
        );
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
                }
            }
            self.presentations.fed.insert(uuid, now);
        }
    }

    /// The PowerPoint inputs, by deck UUID.
    pub(crate) fn powerpoint_decks(&self) -> Vec<String> {
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
    /// window when found, and keep each deck's slide for saving.
    pub(crate) fn reconcile_presentations(&mut self) {
        let wanted = self.presentation_decks();
        if self.presentations.manager.is_none()
            && (wanted.is_empty() || self.presentations.disabled)
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
        let gone: Vec<(String, std::path::PathBuf)> = manager
            .decks()
            .filter(|(deck, _)| !wanted.iter().any(|(w, ..)| w == deck))
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
        }
        let windows = manager.poll();
        let slides: Vec<(String, u32)> = wanted
            .iter()
            .filter_map(|(deck, _, saved, _)| {
                let now = manager.view(deck)?.show?.slide;
                (Some(now) != *saved).then(|| (deck.clone(), now))
            })
            .collect();
        for found in windows {
            self.bind_presentation_window(found);
        }
        // A deck with no picture of a running show — its capture failed or
        // is gone, or an undo made it again — asks for its window again.
        let unbound: Vec<String> = self
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

    /// The PowerPoint input a step goes to: the one named, else the one on
    /// air (on Main or a bus), else the one moved last, else the first.
    fn presentation_target(&self, deck_uuid: Option<&str>) -> Result<String> {
        let decks = self.presentation_decks();
        if let Some(uuid) = deck_uuid {
            return decks
                .iter()
                .find(|(d, ..)| d == uuid)
                .map(|(d, ..)| d.clone())
                .with_context(|| format!("{uuid} is not a PowerPoint input"));
        }
        if decks.is_empty() {
            anyhow::bail!("There is no PowerPoint input");
        }
        let on_air = self.on_air();
        let most = decks
            .iter()
            .map(|(d, ..)| (d, on_air.anywhere(d)))
            .filter(|(_, level)| *level > 0.0)
            .max_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((deck, _)) = most {
            return Ok(deck.clone());
        }
        if let Some(last) = &self.presentations.last {
            if decks.iter().any(|(d, ..)| d == last) {
                return Ok(last.clone());
            }
        }
        Ok(decks[0].0.clone())
    }

    /// Play, pause or move the video on the slide on screen of `deck_uuid`'s
    /// show — or, without one, the show on air (see `presentation_target`).
    pub(crate) fn presentation_video(
        &mut self,
        deck_uuid: Option<&str>,
        control: crate::engine::value::presentation::VideoControl,
    ) -> Result<()> {
        let deck = self.presentation_target(deck_uuid)?;
        if let Some(manager) = &self.presentations.manager {
            manager.video(&deck, control);
        }
        self.presentations.last = Some(deck);
        Ok(())
    }

    pub(crate) fn presentation_go(&mut self, deck_uuid: Option<&str>, to: Go) -> Result<()> {
        let deck = self.presentation_target(deck_uuid)?;
        if let Some(manager) = &self.presentations.manager {
            manager.go(&deck, to);
        }
        self.presentations.last = Some(deck);
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
        let deck = self.presentation_target(deck_uuid)?;
        if let Some(manager) = &self.presentations.manager {
            let now = manager.view(&deck).and_then(|v| v.show).map(|s| s.screen);
            let screen = if toggle && now == Some(screen) {
                Screen::Slide
            } else {
                screen
            };
            manager.screen(&deck, screen);
        }
        self.presentations.last = Some(deck);
        Ok(())
    }

    /// Which screen a PowerPoint input's show plays on: moved there at the
    /// next frame.
    pub(crate) fn set_presentation_plays_on(
        &mut self,
        deck_uuid: &str,
        plays_on: PlaysOn,
    ) -> Result<()> {
        let (ch, dk) = self
            .mixer
            .find_deck_by_uuid(deck_uuid)
            .with_context(|| format!("no layer {deck_uuid}"))?;
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
        let deck = self.presentation_target(Some(deck_uuid))?;
        if let Some(manager) = &self.presentations.manager {
            manager.restart(&deck);
        }
        Ok(())
    }

    /// A deck's show for the API and the panels; `None` for any other deck.
    pub(crate) fn presentation_snapshot(&self, deck: &Deck) -> Option<PresentationSnapshot> {
        let config = deck.screen_capture.as_ref()?.presentation.as_ref()?;
        let view = self
            .presentations
            .manager
            .as_ref()
            .and_then(|m| m.view(deck.uuid()));
        let (status, message) = match view.map(|v| &v.status) {
            None if self.presentations.disabled && self.presentations.manager.is_none() => (
                PresentationStatus::Failed,
                Some("PowerPoint inputs are off (--no-powerpoint)".to_string()),
            ),
            None | Some(Status::Opening) => (PresentationStatus::Opening, None),
            Some(Status::Showing) => (PresentationStatus::Showing, None),
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
        Some(PresentationSnapshot {
            path: config.path.clone(),
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
            plays_on: config.plays_on.clone(),
            playing_on: playing_on.map(|spot| self.presentation_screen_name(spot)),
            slide_pixels,
            video: view.and_then(|v| v.video).map(|v| {
                crate::engine::value::presentation::VideoSnapshot {
                    state: v.state,
                    position_ms: v.position_ms,
                }
            }),
        })
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
                            | crate::engine::InputSource::PresenterView { .. })
                    ),
                    "{line}: {source:?}"
                );
            }
        }
        assert_eq!(commands, 10);
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
