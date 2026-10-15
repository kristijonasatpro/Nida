//! Playlists: clips and pictures shown one after another in one layer, as
//! vMix's List input (see [`crate::deck::playlist`]).

use anyhow::Context as _;

use super::super::VardaApp;
use crate::deck::playlist::Playlist;
use crate::deck::Deck;
use crate::engine::value::playlist::{PlaylistItem, PlaylistSettings};

impl VardaApp {
    /// Add a playlist layer to a channel, stopped on its first item.
    pub(crate) fn add_playlist_deck(
        &mut self,
        channel_uuid: &str,
        items: Vec<PlaylistItem>,
        settings: Option<PlaylistSettings>,
    ) -> anyhow::Result<String> {
        let channel_idx = self.resolve_channel(channel_uuid)?;
        let count = items.len();
        let mut deck = Deck::new_playlist(
            &self.context,
            items,
            settings.unwrap_or_default(),
            self.render_width,
            self.render_height,
        );
        let uuid = deck.uuid().to_string();
        self.add_default_effects(&mut deck);
        let ch = self
            .mixer
            .channel_mut(channel_idx)
            .context("Invalid channel")?;
        let idx = ch.add_deck(deck);
        log::info!("Added playlist deck {idx} to channel {channel_idx}: {count} item(s)");
        Ok(uuid)
    }

    /// Do `edit` to the playlist of deck `deck_uuid`.
    pub(crate) fn edit_playlist(
        &mut self,
        deck_uuid: &str,
        edit: impl FnOnce(&mut Playlist),
    ) -> anyhow::Result<()> {
        let (ch, dk) = self.resolve_deck(deck_uuid)?;
        let list = self.mixer.channels_mut()[ch].decks[dk]
            .deck
            .playlist_mut()
            .context("That layer is not a playlist")?;
        edit(list);
        Ok(())
    }
}
