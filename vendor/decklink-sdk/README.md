# Blackmagic DeckLink SDK headers

The headers and the dispatch source of Blackmagic Design's DeckLink SDK 12.0,
for Nida's Blackmagic outputs (`src/internal/decklink`, built by `build.rs` on
a Mac). Taken as OBS Studio ships them
(`obs-studio/plugins/decklink/mac/decklink-sdk`). They are under Blackmagic's
DeckLink SDK licence, at the top of each file, which allows them to be
redistributed.

Nothing is linked against Blackmagic's code: `DeckLinkAPIDispatch.cpp` loads
the API at run time from the `DeckLinkAPI.framework` that Blackmagic's Desktop
Video installs. Without Desktop Video there are no Blackmagic devices, and
nothing else changes. Desktop Video 16.4 serves these 12.0 interfaces.
