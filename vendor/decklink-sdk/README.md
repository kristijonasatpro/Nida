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

## Windows

`win/` contains Blackmagic's unmodified SDK 12.0 IDL files and version header,
as shipped in OBS Studio at commit
`f1c0a116441bdb7a1de7896f810009ea487b8508`, under
`plugins/decklink/win/decklink-sdk`. The Blackmagic licence is retained in
every file. The full include closure, including the deprecated interfaces,
is needed because `DeckLinkAPI.idl` includes them.

The Windows SDK's MIDL compiler generates the C++ interface header and GUID
definitions into Cargo's `OUT_DIR`. Generated RPC proxy/stub and type-library
files are not shipped or registered. Nida uses the in-process COM server
registered by Blackmagic Desktop Video, and links only Windows COM system
libraries. No Blackmagic runtime DLL is linked or bundled.
