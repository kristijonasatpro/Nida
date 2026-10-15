//! What is on air, frame by frame — and what clips do about it, as vMix
//! inputs do: a clip plays as it goes on air and pauses and goes back to its
//! start as it leaves (Auto Play, Auto Pause, Auto Restart), and its sound
//! switches on and off with it (Audio Auto).
//!
//! On air is on Main, or on any bus's Program — on Main only for a clip set
//! so (its Auto Play's "Main only"); a clip is on air as a layer of a channel
//! that is there, as well as on its own — and as a layer of a channel shown
//! as a layer of one that is there (another input as a layer), however deep.

use std::collections::HashMap;

use super::super::VardaApp;
use crate::audio_mix::FOLLOW_MAIN;
use crate::bus::BusInputRef;
use crate::mixer::{MainSource, MixMode};

/// How much of each deck is on air this frame, 0 to 1, keyed by deck UUID;
/// missing is off air.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct OnAir {
    /// On Main.
    main: HashMap<String, f32>,
    /// On each bus's Program, by bus UUID.
    buses: HashMap<String, HashMap<String, f32>>,
}

impl OnAir {
    /// On Main or on any bus.
    pub(crate) fn anywhere(&self, deck_uuid: &str) -> f32 {
        self.buses
            .values()
            .chain(std::iter::once(&self.main))
            .filter_map(|levels| levels.get(deck_uuid))
            .fold(0.0, |a, &b| a.max(b))
    }

    /// Where Audio Auto looks: `None` anywhere, [`FOLLOW_MAIN`] on Main, else
    /// on that bus.
    pub(crate) fn at(&self, follow: Option<&str>, deck_uuid: &str) -> f32 {
        match follow {
            None => self.anywhere(deck_uuid),
            Some(FOLLOW_MAIN) => self.main.get(deck_uuid).copied().unwrap_or(0.0),
            Some(bus) => self
                .buses
                .get(bus)
                .and_then(|levels| levels.get(deck_uuid))
                .copied()
                .unwrap_or(0.0),
        }
    }
}

impl VardaApp {
    /// How much of every deck is on air, on Main and on each bus.
    pub(crate) fn on_air(&self) -> OnAir {
        let decks: Vec<&str> = self
            .mixer
            .channels()
            .iter()
            .flat_map(|ch| &ch.decks)
            .map(|slot| slot.deck.uuid())
            .collect();
        let keep = |level: f32| level > 0.0;
        let main = decks
            .iter()
            .map(|d| (d.to_string(), self.deck_on_main(d, &[])))
            .filter(|(_, l)| keep(*l))
            .collect();
        let buses = self
            .production_buses
            .iter()
            .map(|bus| {
                let levels = decks
                    .iter()
                    .map(|d| (d.to_string(), self.deck_on_bus(bus, d)))
                    .filter(|(_, l)| keep(*l))
                    .collect();
                (bus.uuid().to_string(), levels)
            })
            .collect();
        OnAir { main, buses }
    }

    /// Act on this frame's on-air changes: play, pause and rewind clips, and
    /// switch Audio Auto sources on and off. Called once per rendered frame,
    /// after the mixer has seen which clips there are.
    pub(crate) fn update_on_air(&mut self) {
        let on_air = self.on_air();
        let mut now_on = HashMap::new();
        let mut arrived_held = Vec::new();
        for slot in self.mixer.channels().iter().flat_map(|ch| &ch.decks) {
            let deck = &slot.deck;
            let Some(playback) = deck.playback_snapshot() else {
                continue;
            };
            let uuid = deck.uuid();
            let identity = deck.playback_identity().unwrap_or_default();
            let auto = deck.auto_playback;
            let on = if auto.main_only {
                on_air.at(Some(FOLLOW_MAIN), uuid) > 0.0
            } else {
                on_air.anywhere(uuid) > 0.0
            };
            // A clip rebuilt under the same UUID starts paused like a new
            // one, so it counts as arriving: on air, it plays.
            let was = self
                .clips_on_air
                .get(uuid)
                .is_some_and(|&(was, id)| was && id == identity);
            // A rundown step that brought it here paused has the say, once
            // — for the clip it paused, not one rebuilt under its UUID since.
            let held = self
                .clips_held
                .iter()
                .any(|(held, of, _)| held == uuid && *of == identity);
            if on && !was {
                if held {
                    arrived_held.push(uuid.to_string());
                } else if auto.play {
                    deck.video_set_playing(true);
                }
            }
            if !on && was {
                if auto.pause {
                    deck.video_set_playing(false);
                }
                if auto.restart {
                    deck.video_seek(playback.in_point);
                }
            }
            now_on.insert(uuid.to_string(), (on, identity));
        }
        self.clips_on_air = now_on;
        // A held clip is held for the arrival its step brings about: until
        // it has arrived — a fade brings it on a frame or two after the take
        // — or the few updates in which that can happen are over. The same
        // clip put on air by hand later starts as its Auto Play says.
        self.clips_held.retain_mut(|(held, _, updates)| {
            *updates = updates.saturating_sub(1);
            *updates > 0 && !arrived_held.contains(held)
        });

        let levels = self
            .audio_mix
            .auto_sources()
            .into_iter()
            .map(|(strip, deck, follow)| {
                let level = on_air.at(follow.as_deref(), &deck);
                (strip, level)
            })
            .collect();
        self.audio_mix.set_on_air(levels);
    }

    /// How much of a deck Main shows. In the vMix way (Program), the channel
    /// or input on Main — both, blended, during a fade — and overlays at
    /// their level; in the layered mix, each channel at its fader and the
    /// crossfader. Channels in `hidden` count for nothing, as a bus showing
    /// Main without them.
    fn deck_on_main(&self, deck_uuid: &str, hidden: &[String]) -> f32 {
        let channels = self.mixer.channels();
        let shown = |ch: &crate::channel::Channel| !hidden.iter().any(|h| h == ch.uuid());
        match self.mixer.mix_mode() {
            MixMode::Program => {
                let main = self.mixer.main();
                let level = |source: &MainSource| -> f32 {
                    match source {
                        MainSource::Black => 0.0,
                        MainSource::Channel(uuid) => channels
                            .iter()
                            .find(|ch| ch.uuid() == uuid && shown(ch))
                            .map_or(0.0, |ch| deck_share(channels, ch, deck_uuid)),
                        MainSource::Deck(uuid) => channels
                            .iter()
                            .filter(|ch| shown(ch))
                            .flat_map(|ch| &ch.decks)
                            .find(|s| s.deck.uuid() == uuid)
                            .map_or(0.0, |slot| deck_in(channels, &slot.deck, deck_uuid)),
                    }
                };
                let program = match &main.fade {
                    Some(fade) => {
                        let p = fade.progress.clamp(0.0, 1.0);
                        level(&fade.from) * (1.0 - p) + level(&main.program) * p
                    }
                    None => level(&main.program),
                };
                let overlays = channels
                    .iter()
                    .enumerate()
                    .filter(|(_, ch)| ch.overlay && shown(ch))
                    .map(|(i, ch)| {
                        main.overlay_level(ch.uuid())
                            * self.mixer.channel_opacity(i)
                            * deck_share(channels, ch, deck_uuid)
                    })
                    .fold(0.0, f32::max);
                program.max(overlays).clamp(0.0, 1.0)
            }
            MixMode::Layered => {
                let xf = self.mixer.crossfader();
                channels
                    .iter()
                    .enumerate()
                    .filter(|(_, ch)| shown(ch))
                    .map(|(i, ch)| {
                        let opacity = self.mixer.channel_opacity(i);
                        let in_master = if channels.len() == 2 {
                            if i == 0 {
                                (1.0 - xf) * opacity
                            } else {
                                xf * opacity
                            }
                        } else {
                            opacity
                        };
                        in_master * deck_share(channels, ch, deck_uuid)
                    })
                    .fold(0.0, f32::max)
                    .clamp(0.0, 1.0)
            }
        }
    }

    /// How much of a deck a bus has on air: all of it when the bus shows the
    /// deck, the deck's share of a channel it shows, its share of Main for
    /// Master; blended across a fade so the sound fades with the picture.
    pub(crate) fn deck_on_bus(&self, bus: &crate::bus::Bus, deck_uuid: &str) -> f32 {
        let channels = self.mixer.channels();
        let level = |input: &BusInputRef| -> f32 {
            match input {
                BusInputRef::Black => 0.0,
                BusInputRef::Deck(uuid) => channels
                    .iter()
                    .flat_map(|ch| &ch.decks)
                    .find(|s| s.deck.uuid() == uuid)
                    .map_or(0.0, |slot| deck_in(channels, &slot.deck, deck_uuid)),
                BusInputRef::Channel(uuid) => channels
                    .iter()
                    .find(|ch| ch.uuid() == uuid)
                    .map_or(0.0, |ch| deck_share(channels, ch, deck_uuid)),
                BusInputRef::Master => self.deck_on_main(deck_uuid, bus.hidden_layers()),
            }
        };
        match bus.transition() {
            Some(t) => {
                let mix = t.mix();
                let from = if t.from_resolved { level(&t.from) } else { 0.0 };
                let to = if t.to_resolved { level(&t.to) } else { 0.0 };
                (from * (1.0 - mix) + to * mix).clamp(0.0, 1.0)
            }
            None => level(bus.effective_program()).clamp(0.0, 1.0),
        }
    }
}

/// A deck's share of a channel's picture: its opacity as a layer of it,
/// nothing if it is muted or another deck is soloed — or, for a deck of
/// another channel, its share of that channel times the share of the layer
/// showing that channel here (another input as a layer), the most of the
/// ways it shows.
fn deck_share(
    channels: &[crate::channel::Channel],
    channel: &crate::channel::Channel,
    deck_uuid: &str,
) -> f32 {
    share_through(channels, channel, deck_uuid, &mut Vec::new())
}

/// How much of `deck_uuid` the deck `shown` shows: all of it when it is
/// that deck, its share of a channel `shown` is a layer showing.
fn deck_in(
    channels: &[crate::channel::Channel],
    shown: &crate::deck::Deck,
    deck_uuid: &str,
) -> f32 {
    if shown.uuid() == deck_uuid {
        return 1.0;
    }
    shown_channel(channels, shown).map_or(0.0, |ch| {
        share_through(channels, ch, deck_uuid, &mut vec![ch.uuid().to_string()])
    })
}

/// The channel a deck shows as a layer (a channel tap), when it is one.
fn shown_channel<'a>(
    channels: &'a [crate::channel::Channel],
    deck: &crate::deck::Deck,
) -> Option<&'a crate::channel::Channel> {
    match deck.tap.as_ref().map(|t| &t.source) {
        Some(crate::deck::TapSource::Channel(uuid)) => channels.iter().find(|ch| ch.uuid() == uuid),
        _ => None,
    }
}

/// [`deck_share`], not going back into a channel already on the way
/// (`visiting`): channels showing each other round a loop.
fn share_through(
    channels: &[crate::channel::Channel],
    channel: &crate::channel::Channel,
    deck_uuid: &str,
    visiting: &mut Vec<String>,
) -> f32 {
    visiting.push(channel.uuid().to_string());
    let any_solo = channel.decks.iter().any(|s| s.solo);
    let mut share: f32 = 0.0;
    for slot in &channel.decks {
        if slot.mute || (any_solo && !slot.solo) {
            continue;
        }
        let own = slot.opacity.clamp(0.0, 1.0);
        if slot.deck.uuid() == deck_uuid {
            share = share.max(own);
        } else if let Some(inner) = shown_channel(channels, &slot.deck) {
            if own > 0.0 && !visiting.iter().any(|v| v == inner.uuid()) {
                share = share.max(own * share_through(channels, inner, deck_uuid, visiting));
            }
        }
    }
    visiting.pop();
    share
}

#[cfg(test)]
mod tests {
    use super::*;

    fn levels(pairs: &[(&str, f32)]) -> HashMap<String, f32> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), *v)).collect()
    }

    #[test]
    fn on_air_anywhere_is_the_most_of_main_and_every_bus() {
        let on_air = OnAir {
            main: levels(&[("a", 0.25)]),
            buses: [
                ("led".to_string(), levels(&[("a", 0.75), ("b", 1.0)])),
                ("tv".to_string(), levels(&[])),
            ]
            .into_iter()
            .collect(),
        };
        assert!((on_air.anywhere("a") - 0.75).abs() < 1e-6);
        assert!((on_air.anywhere("b") - 1.0).abs() < 1e-6);
        assert!(on_air.anywhere("c").abs() < 1e-6);
        assert!((on_air.at(Some(FOLLOW_MAIN), "a") - 0.25).abs() < 1e-6);
        assert!(on_air.at(Some(FOLLOW_MAIN), "b").abs() < 1e-6);
        assert!((on_air.at(Some("led"), "b") - 1.0).abs() < 1e-6);
        assert!(on_air.at(Some("tv"), "b").abs() < 1e-6);
        assert!(on_air.at(Some("gone"), "b").abs() < 1e-6);
    }
}
