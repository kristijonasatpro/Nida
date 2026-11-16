//! Lower-third inputs: added, changed, brought on air and taken off, and
//! their rows read from an organiser's CSV or TSV list — read again as it
//! is saved, on a thread of its own, so a list on a network drive that
//! stops answering never holds up a frame.

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Context as _;

use crate::deck::lower_third::LowerThird;
use crate::deck::Deck;
use crate::engine::value::lower_third::{rows_from_bytes, LowerThirdRow, LowerThirdSettings};

/// How often the lists are looked at for changes.
const LIST_LOOK_EVERY: Duration = Duration::from_secs(2);

/// Reads under way at most, those let go but never answering among them:
/// past this no more are started (files on a drive gone), so they cannot
/// pile up.
const MOST_READS: usize = 16;

/// A list looked at: its file, the file's time, and its rows or why it
/// could not be read.
struct ListRead {
    path: String,
    modified: SystemTime,
    rows: Result<Vec<LowerThirdRow>, String>,
}

/// The lower thirds' list files being looked at.
#[derive(Default)]
pub(crate) struct ListWatch {
    next_at: Option<Instant>,
    /// Each lower third's look under way, by deck UUID: the file, and where
    /// its answer comes — none when the file has not changed. One that never
    /// answers (a drive gone) holds up only its own lower third, and only
    /// while it follows that file.
    looking: HashMap<String, (String, mpsc::Receiver<Option<ListRead>>)>,
    /// Reads whose threads have not ended, let go or not.
    reading: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Where the next round of looks starts among the lists: turned each
    /// round, so with reads at their most none is left out for ever.
    first: usize,
}

/// Look at the list at `path`: read it unless its time is `read_at`'s (it
/// has not changed since its rows were read).
fn look_at(path: &str, read_at: Option<SystemTime>) -> Option<ListRead> {
    let (modified, rows) = match std::fs::metadata(path).and_then(|m| m.modified()) {
        Err(e) => (
            SystemTime::UNIX_EPOCH,
            Err(format!("{path} is not there: {e}")),
        ),
        Ok(time) if read_at == Some(time) => return None,
        Ok(time) => (
            time,
            std::fs::read(path)
                .map_err(|e| format!("{path} could not be read: {e}"))
                .and_then(|bytes| rows_from_bytes(&bytes).map_err(|e| format!("{path}: {e}"))),
        ),
    };
    Some(ListRead {
        path: path.to_string(),
        modified,
        rows,
    })
}

impl super::super::VardaApp {
    pub(crate) fn add_lower_third_deck(
        &mut self,
        channel_uuid: &str,
        settings: Option<LowerThirdSettings>,
    ) -> anyhow::Result<String> {
        let channel_idx = self.resolve_channel(channel_uuid)?;
        let mut deck = Deck::new_lower_third(
            &self.context,
            settings.unwrap_or_default(),
            self.render_width,
            self.render_height,
        )?;
        let uuid = deck.uuid().to_string();
        self.add_default_effects(&mut deck);
        let ch = self
            .mixer
            .channel_mut(channel_idx)
            .context("Invalid channel")?;
        let idx = ch.add_deck(deck);
        log::info!("Added lower third deck {idx} to channel {channel_idx}");
        Ok(uuid)
    }

    /// Do `edit` to the lower third of deck `deck_uuid`.
    pub(crate) fn edit_lower_third(
        &mut self,
        deck_uuid: &str,
        edit: impl FnOnce(&mut LowerThird),
    ) -> anyhow::Result<()> {
        let (ch, dk) = self.resolve_deck(deck_uuid)?;
        let lower = self.mixer.channels_mut()[ch].decks[dk]
            .deck
            .lower_third_mut()
            .context("That layer is not a lower third")?;
        edit(lower);
        Ok(())
    }

    /// Follow the CSV or TSV at `path` as deck `deck_uuid`'s list: read on
    /// a thread of its own within a frame or two — never here, so a file on
    /// a drive that stops answering holds up nothing — its names cued from
    /// the first; why it could not be read is said in its `problem`.
    pub(crate) fn import_lower_third_rows(
        &mut self,
        deck_uuid: &str,
        path: &str,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(!path.trim().is_empty(), "No list file given");
        self.edit_lower_third(deck_uuid, |lower| lower.choose_list(path.to_string()))?;
        // Looked at on the next frame.
        self.lower_third_lists.next_at = None;
        Ok(())
    }

    /// Lower thirds whose list file changed since its rows were read — the
    /// organiser fixing a name in Excel — or that were never read from it
    /// (the show just opened, an undo, a read that failed): read again, each
    /// on a thread of its own, every [`LIST_LOOK_EVERY`]. A list that will
    /// not read keeps the rows before, and says why.
    pub(crate) fn follow_lower_third_lists(&mut self) {
        // Answers back.
        let mut answers = Vec::new();
        self.lower_third_lists
            .looking
            .retain(|uuid, (_, answer)| match answer.try_recv() {
                Ok(read) => {
                    answers.extend(read.map(|read| (uuid.clone(), read)));
                    false
                }
                Err(mpsc::TryRecvError::Empty) => true,
                Err(mpsc::TryRecvError::Disconnected) => false,
            });
        for (uuid, read) in answers {
            let _ = self.edit_lower_third(&uuid, |lower| {
                if read.rows.is_ok() {
                    log::info!("Lower third list {} read", read.path);
                }
                lower.take_list(&read.path, read.modified, read.rows);
            });
        }

        let now = Instant::now();
        if self.lower_third_lists.next_at.is_some_and(|at| now < at) {
            return;
        }
        self.lower_third_lists.next_at = Some(now + LIST_LOOK_EVERY);
        let lists: Vec<(String, String, Option<SystemTime>)> = self
            .mixer
            .channels()
            .iter()
            .flat_map(|ch| &ch.decks)
            .filter_map(|slot| {
                let lower = slot.deck.lower_third()?;
                let path = lower.settings().source.clone()?;
                let read_at = lower
                    .list_read()
                    .filter(|(read, _)| *read == path)
                    .map(|(_, time)| *time);
                Some((slot.deck.uuid().to_string(), path, read_at))
            })
            .collect();
        let looking = &mut self.lower_third_lists.looking;
        // A look for a deck gone, or for a file it no longer follows, is let
        // go (its thread ends when its read does).
        looking.retain(|uuid, (path, _)| lists.iter().any(|(u, p, _)| u == uuid && p == path));
        let mut lists = lists;
        if !lists.is_empty() {
            let first = self.lower_third_lists.first % lists.len();
            lists.rotate_left(first);
            self.lower_third_lists.first = first + 1;
        }
        let looking = &mut self.lower_third_lists.looking;
        let reading = &self.lower_third_lists.reading;
        for (uuid, path, read_at) in lists {
            if looking.contains_key(&uuid) {
                continue;
            }
            if reading.load(std::sync::atomic::Ordering::Relaxed) >= MOST_READS {
                log::warn!(
                    "Lower third list {path} not looked at: {MOST_READS} reads not answering"
                );
                break;
            }
            let (tx, rx) = mpsc::channel();
            let file = path.clone();
            let count = std::sync::Arc::clone(reading);
            count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let spawned = std::thread::Builder::new()
                .name("lower-third-list".into())
                .spawn(move || {
                    let read = look_at(&file, read_at);
                    count.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                    let _ = tx.send(read);
                });
            match spawned {
                Ok(_) => {
                    looking.insert(uuid, (path, rx));
                }
                Err(e) => {
                    reading.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                    log::warn!("Lower third list {path} not looked at: {e}");
                }
            }
        }
    }
}

/// Lower thirds sent to buses: where each bus's names go, the sizes each
/// lower third is drawn at, and which are live over which bus.
impl super::super::VardaApp {
    /// Each bus's names area, in master pixels (x, y, width, height): where
    /// the LED screens showing it show part of it, that part — the band a
    /// wall shows of a 16:9 picture — else the bus's whole region.
    pub(crate) fn bus_names_areas(&self) -> HashMap<String, (u32, u32, u32, u32)> {
        use crate::engine::value::led::LedFit;
        use crate::engine::value::render::OutputSource;
        let mut areas = HashMap::new();
        let (mw, mh) = (self.render_width.max(1), self.render_height.max(1));
        for bus in &self.production_buses {
            // The bus's region as the outputs take it — clamped to the
            // master — and the parts its screens show, of that picture.
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss
            )]
            let region = {
                let (scale, offset) = bus.canvas().uv(mw, mh);
                let px = |v: f32, of: u32| (v * of as f32).round().max(0.0) as u32;
                (
                    px(offset[0], mw),
                    px(offset[1], mh),
                    px(scale[0], mw).max(1),
                    px(scale[1], mh).max(1),
                )
            };
            let (cw, ch) = (i64::from(region.2), i64::from(region.3));
            let mut shown: Option<(i64, i64, i64, i64)> = None;
            for layout in self.output.outputs.iter().filter_map(|o| o.led()) {
                for screen in &layout.screens {
                    let OutputSource::BusProgram(uuid) = &screen.source else {
                        continue;
                    };
                    let LedFit::Part {
                        x,
                        y,
                        width,
                        height,
                    } = screen.fit
                    else {
                        continue;
                    };
                    if uuid != bus.uuid() {
                        continue;
                    }
                    let (x0, y0) = (i64::from(x).max(0), i64::from(y).max(0));
                    let x1 = (i64::from(x) + i64::from(width)).min(cw);
                    let y1 = (i64::from(y) + i64::from(height)).min(ch);
                    if x1 <= x0 || y1 <= y0 {
                        continue;
                    }
                    shown = Some(shown.map_or((x0, y0, x1, y1), |(a, b, c, d)| {
                        (a.min(x0), b.min(y0), c.max(x1), d.max(y1))
                    }));
                }
            }
            let (x0, y0, x1, y1) = shown.unwrap_or((0, 0, cw, ch));
            let at = |origin: u32, v: i64| origin.saturating_add(u32::try_from(v).unwrap_or(0));
            let side = |v: i64| u32::try_from(v).unwrap_or(1).max(1);
            areas.insert(
                bus.uuid().to_string(),
                (
                    at(region.0, x0),
                    at(region.1, y0),
                    side(x1 - x0),
                    side(y1 - y0),
                ),
            );
        }
        areas
    }

    /// Before the frame renders: each lower third and text input told the
    /// sizes of the names areas of the buses it is sent to, so its drawings
    /// for them are made ahead, laid out for each.
    pub(crate) fn prepare_lower_third_targets(&mut self) {
        let areas = self.bus_names_areas();
        let frames_of = |targets: &[String]| -> Vec<(u32, u32)> {
            targets
                .iter()
                .filter_map(|uuid| areas.get(uuid))
                .map(|area| (area.2, area.3))
                .collect()
        };
        for channel in self.mixer.channels_mut() {
            for slot in &mut channel.decks {
                if let Some(lower) = slot.deck.lower_third_mut() {
                    let frames = frames_of(&lower.settings().targets);
                    lower.set_target_frames(&frames);
                } else if let Some(text) = slot.deck.text_mut() {
                    let frames = frames_of(&text.settings().targets);
                    text.set_target_frames(&frames);
                }
            }
        }
    }
}

/// Each bus with lower thirds or text inputs showing over it this frame,
/// in the order the inputs stand (see [`super::super::bus_overlay`]).
pub(crate) fn live_bus_overlays<'a>(
    mixer: &'a crate::mixer::Mixer,
    buses: &crate::bus::BusBank,
    areas: &HashMap<String, (u32, u32, u32, u32)>,
    master: (u32, u32),
) -> Vec<super::super::bus_overlay::BusOverlay<'a>> {
    use super::super::bus_overlay::{Area, BusOverlay, OverBus};
    // What shows, and the buses it is sent to.
    let mut showing: Vec<(&dyn OverBus, &[String])> = Vec::new();
    for slot in mixer.channels().iter().flat_map(|ch| &ch.decks) {
        if let Some(lower) = slot.deck.lower_third() {
            if !lower.settings().targets.is_empty() && lower.shows() {
                showing.push((lower as &LowerThird, &lower.settings().targets));
            }
        } else if let Some(text) = slot.deck.text() {
            if !text.settings().targets.is_empty() && text.shows() {
                showing.push((text, &text.settings().targets));
            }
        }
    }
    let mut overlays = Vec::new();
    for bus in buses {
        let Some(area) = areas.get(bus.uuid()) else {
            continue;
        };
        let over: Vec<(&dyn OverBus, Area)> = showing
            .iter()
            .filter(|(_, targets)| targets.iter().any(|t| t == bus.uuid()))
            .map(|(over, _)| (*over, *area))
            .collect();
        if !over.is_empty() {
            overlays.push(BusOverlay {
                bus: bus.uuid().to_string(),
                region: bus.canvas().uv(master.0, master.1),
                over,
            });
        }
    }
    overlays
}
