//! Engine state mutation methods on `VardaApp`.
//!
//! These methods encapsulate all mixer/modulation/sequence mutations.
//! They access self.mixer internally — callers never need &mut Mixer.
//!
//! Split into focused sub-modules:
//! - `arrangement` — lane and region CRUD, authority, and live override
//! - `audio_mix` — the audio mixer: per-frame sync with the scene, and its commands
//! - `buses` — production bus (PVW/PGM) command state mutations
//! - `on_air` — what is on air, and clips playing and sounding with it
//! - `clipboard` — copy, paste, and duplicate of decks, channels, and effects
//! - `recorder` — capturing live parameter writes as automation curves
//! - `rundown` — the rundown: taking a step whole, and its commands
//! - `playlists` — playlist layers: adding one, its list, settings and transport
//! - `timers` — timer layers: adding one, its settings, starting and stopping it
//! - `presets` — deck/channel preset load + save
//! - `sequences` — transition sequence CRUD and step mutations
//! - `surfaces` — surface command state mutations
//! - `io` — external I/O deck creation and stream library mutations

mod arrangement;
mod audio_mix;
mod buses;
mod channel_order;
pub(crate) mod clipboard;
pub(crate) mod inputs;
mod io;
mod led;
mod main_program;
pub(crate) mod on_air;
mod outputs;
mod playlists;
pub(crate) mod presets;
pub(crate) mod record;
pub(crate) mod recorder;
pub(crate) mod rundown;
mod sequences;
pub(crate) mod streaming;
mod surfaces;
mod timers;

pub(crate) use outputs::{encoder_fps, resolve_output_audio, OutputAudio};
pub use streaming::DestinationEdit;
