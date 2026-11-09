//! Clips and pictures asked for over the bus — the HTTP API, a WebSocket,
//! `nida` from a terminal — made on a thread of their own and put in their
//! channel once made, a frame or more later: the reply waits for them, the
//! show does not.
//!
//! Made on the show's thread, a 4K clip held it up 224 ms (measured
//! 2026-11-04: the file opened three or four times over, frames decoded
//! each time to learn its format), a large picture 50–100 ms, and a clip on
//! a disk gone to sleep for as long as the disk took to wake. The control
//! window's own adds were already made on a thread (see the UI runner's
//! `spawn_deck_loads`); these now are too. Nida's own calls
//! (`execute_command`: the rundown, tests) still make them in place.
//!
//! Commands sent after an add, not waiting for its reply, can be carried out
//! before it is put in — another layer added, a save, an undo: a client
//! wanting the clip there first waits for the reply (it names the new
//! layer) before going on.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc;

use crate::deck::Deck;
use crate::engine::{CommandResult, EngineCommand, ErrorCode};

use super::Reply;

/// What is being made.
#[derive(Debug, Clone, Copy)]
enum Media {
    Video,
    Image,
}

/// Made at once, at most: the rest wait their turn, rather than every clip
/// asked for decoding and taking GPU memory at the same time as the show.
const MADE_AT_ONCE: usize = 2;

/// A clip or picture asked for: where it goes, who waits for it, the show
/// it was asked of, what it is, whether it is being made, and, once made,
/// what was made and at what size.
struct Asked {
    token: u64,
    channel: String,
    reply: Option<Reply>,
    show: u64,
    media: Media,
    path: PathBuf,
    started: bool,
    made: Option<(anyhow::Result<Deck>, (u32, u32))>,
}

/// The clips and pictures being made, in the order asked.
pub(crate) struct MediaAdds {
    asked: VecDeque<Asked>,
    done_tx: mpsc::Sender<(u64, anyhow::Result<Deck>, (u32, u32))>,
    done_rx: mpsc::Receiver<(u64, anyhow::Result<Deck>, (u32, u32))>,
    next: u64,
    /// Threads making one now, counted until their result comes — that of
    /// one asked of a show since replaced too.
    running: usize,
}

impl Default for MediaAdds {
    fn default() -> Self {
        let (done_tx, done_rx) = mpsc::channel();
        Self {
            asked: VecDeque::new(),
            done_tx,
            done_rx,
            next: 0,
            running: 0,
        }
    }
}

impl MediaAdds {
    /// In hand: being made, or made and waiting to be put in behind one
    /// still being made. No more than [`MADE_AT_ONCE`] are.
    fn in_hand(&self) -> usize {
        self.running
            + self
                .asked
                .iter()
                .filter(|asked| asked.made.is_some())
                .count()
    }
}

fn answer(reply: Option<Reply>, result: CommandResult) {
    if let Some(reply) = reply {
        let _ = reply.send(result);
    }
}

impl super::VardaApp {
    /// A clip or picture asked for over the bus (`AddVideoDeck`,
    /// `AddImageDeck`): made on a thread of its own, `reply` answered once
    /// it is in its channel (see [`Self::place_made_media`]). Any other
    /// command is handed back.
    pub(crate) fn add_media_for_command(
        &mut self,
        cmd: EngineCommand,
        reply: Option<Reply>,
    ) -> Option<(EngineCommand, Option<Reply>)> {
        let (channel, path, media) = match cmd {
            EngineCommand::AddVideoDeck { channel_uuid, path } => {
                (channel_uuid, path, Media::Video)
            }
            EngineCommand::AddImageDeck { channel_uuid, path } => {
                (channel_uuid, path, Media::Image)
            }
            other => return Some((other, reply)),
        };
        // A channel not there is said at once, as before.
        if let Err(missing) = self.resolve_channel(&channel) {
            answer(reply, super::commands::wire_id(Err(missing.into())));
            return None;
        }
        let token = self.media_adds.next;
        self.media_adds.next += 1;
        self.media_adds.asked.push_back(Asked {
            token,
            channel,
            reply,
            show: self.show_generation,
            media,
            path,
            started: false,
            made: None,
        });
        self.start_making_media();
        None
    }

    /// Start making those asked for that wait their turn, up to
    /// [`MADE_AT_ONCE`] at a time.
    fn start_making_media(&mut self) {
        let mut room = MADE_AT_ONCE.saturating_sub(self.media_adds.in_hand());
        let (width, height) = (self.render_width, self.render_height);
        for asked in &mut self.media_adds.asked {
            if room == 0 {
                break;
            }
            if asked.started {
                continue;
            }
            asked.started = true;
            room -= 1;
            let (token, media, path) = (asked.token, asked.media, asked.path.clone());
            let done = self.media_adds.done_tx.clone();
            let context = self.context.clone();
            let spawned = std::thread::Builder::new()
                .name("media-add".into())
                .spawn(move || {
                    let made =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match media {
                            Media::Video => Deck::new_from_video(&context, &path, width, height),
                            Media::Image => Deck::new_from_image(&context, &path, width, height),
                        }))
                        .unwrap_or_else(|_| Err(anyhow::anyhow!("making it crashed")));
                    let _ = done.send((token, made, (width, height)));
                });
            match spawned {
                Ok(_) => self.media_adds.running += 1,
                Err(e) => {
                    asked.made = Some((
                        Err(anyhow::anyhow!("No thread to open it on: {e}")),
                        (width, height),
                    ));
                }
            }
        }
    }

    /// Answer every clip and picture asked for and not yet put in: Nida is
    /// quitting, and an answer waited for would hold its quitting up.
    pub(crate) fn refuse_media(&mut self) {
        for asked in self.media_adds.asked.drain(..) {
            answer(
                asked.reply,
                CommandResult::Err {
                    code: ErrorCode::Unavailable,
                    message: "Nida is quitting".into(),
                },
            );
        }
    }

    /// Put the clips and pictures made since in their channels, in the
    /// order they were asked for, answering each — once a frame, as
    /// commands are taken.
    pub(crate) fn place_made_media(&mut self) {
        while let Ok((token, made, size)) = self.media_adds.done_rx.try_recv() {
            self.media_adds.running = self.media_adds.running.saturating_sub(1);
            // One asked of a show since replaced is answered already (below):
            // what it made is let go.
            if let Some(asked) = self.media_adds.asked.iter_mut().find(|a| a.token == token) {
                asked.made = Some((made, size));
            }
        }
        // Asked of a show since replaced: answered now, made or not — still
        // being made, it would hold up those asked of this show behind it.
        let show = self.show_generation;
        let mut kept = VecDeque::with_capacity(self.media_adds.asked.len());
        for asked in self.media_adds.asked.drain(..) {
            if asked.show == show {
                kept.push_back(asked);
            } else {
                answer(asked.reply, replaced_show());
            }
        }
        self.media_adds.asked = kept;
        while self
            .media_adds
            .asked
            .front()
            .is_some_and(|asked| asked.made.is_some())
        {
            let Some(asked) = self.media_adds.asked.pop_front() else {
                break;
            };
            let Some((made, size)) = asked.made else {
                continue;
            };
            let placed = self.place_media(&asked.channel, made, size);
            answer(asked.reply, placed);
        }
        self.start_making_media();
    }

    /// Put `made`, made at `size`, in `channel`, as `add_video_deck` /
    /// `add_image_deck` do in place — an undo step first — unless the
    /// channel has gone; resized first should the show's size have changed
    /// while it was made.
    fn place_media(
        &mut self,
        channel: &str,
        made: anyhow::Result<Deck>,
        size: (u32, u32),
    ) -> CommandResult {
        let mut deck = match made {
            Ok(deck) => deck,
            Err(e) => return super::commands::wire_id(Err(e)),
        };
        if size != (self.render_width, self.render_height) {
            deck.resize(&self.context, self.render_width, self.render_height);
        }
        let Ok(channel_idx) = self.resolve_channel(channel) else {
            return CommandResult::Err {
                code: ErrorCode::NotFound,
                message: format!("Channel {channel} went while it was being opened"),
            };
        };
        if !self.is_recording() {
            let snapshot = self.history_snapshot_default();
            self.push_history(snapshot);
        }
        let uuid = deck.uuid().to_string();
        self.add_default_effects(&mut deck);
        let name = deck.source_name().to_string();
        let Some(ch) = self.mixer.channel_mut(channel_idx) else {
            return CommandResult::Err {
                code: ErrorCode::NotFound,
                message: format!("Channel {channel} went while it was being opened"),
            };
        };
        let idx = ch.add_deck(deck);
        log::info!("Added deck {idx} to channel {channel_idx}: {name} (opened on a thread)");
        CommandResult::OkWithId { uuid }
    }
}

/// The answer to one asked of a show since replaced.
fn replaced_show() -> CommandResult {
    CommandResult::Err {
        code: ErrorCode::Unavailable,
        message: "Another show was opened while it was being opened: not added".into(),
    }
}
