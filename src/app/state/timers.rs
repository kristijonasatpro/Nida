//! Timers: a count down, a count to a time of day, a count up or the time
//! of day, drawn as text in one layer (see [`crate::deck::timer`]).

use anyhow::Context as _;

use super::super::VardaApp;
use crate::deck::timer::Timer;
use crate::deck::Deck;
use crate::engine::value::timer::TimerSettings;

impl VardaApp {
    /// Add a timer layer to a channel; a count starts paused at its top.
    pub(crate) fn add_timer_deck(
        &mut self,
        channel_uuid: &str,
        settings: Option<TimerSettings>,
    ) -> anyhow::Result<String> {
        let channel_idx = self.resolve_channel(channel_uuid)?;
        let mut deck = Deck::new_timer(
            &self.context,
            settings.unwrap_or_default(),
            0.0,
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
        log::info!("Added timer deck {idx} to channel {channel_idx}");
        Ok(uuid)
    }

    /// Do `edit` to the timer of deck `deck_uuid`.
    pub(crate) fn edit_timer(
        &mut self,
        deck_uuid: &str,
        edit: impl FnOnce(&mut Timer),
    ) -> anyhow::Result<()> {
        let (ch, dk) = self.resolve_deck(deck_uuid)?;
        let timer = self.mixer.channels_mut()[ch].decks[dk]
            .deck
            .timer_mut()
            .context("That layer is not a timer")?;
        edit(timer);
        Ok(())
    }
}
