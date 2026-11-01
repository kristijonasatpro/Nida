//! Text inputs: added and changed.

use anyhow::Context as _;

use crate::deck::text::Text;
use crate::deck::Deck;
use crate::engine::value::text::TextSettings;

impl super::super::VardaApp {
    pub(crate) fn add_text_deck(
        &mut self,
        channel_uuid: &str,
        settings: Option<TextSettings>,
    ) -> anyhow::Result<String> {
        let channel_idx = self.resolve_channel(channel_uuid)?;
        let mut deck = Deck::new_text(
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
        log::info!("Added text deck {idx} to channel {channel_idx}");
        Ok(uuid)
    }

    /// Do `edit` to the text of deck `deck_uuid`.
    pub(crate) fn edit_text(
        &mut self,
        deck_uuid: &str,
        edit: impl FnOnce(&mut Text),
    ) -> anyhow::Result<()> {
        let (ch, dk) = self.resolve_deck(deck_uuid)?;
        let text = self.mixer.channels_mut()[ch].decks[dk]
            .deck
            .text_mut()
            .context("That layer is not a text input")?;
        edit(text);
        Ok(())
    }
}
