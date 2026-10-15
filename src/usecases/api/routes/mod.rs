//! Route handlers for the HTTP API.
//!
//! Each sub-module groups routes by domain (mixer, channels, decks, etc.).
//! Route handlers are thin: validate input, read state or send commands,
//! map results to HTTP responses.

pub mod arrangement;
pub mod audio;
pub mod audio_mix;
pub mod buses;
pub mod channels;
pub mod clipboard;
pub mod decks;
pub mod effects;
pub mod library;
pub mod macros;
pub mod main_program;
pub mod mixer;
pub mod modulation;
pub mod outputs;
pub mod record;
pub mod rundown;
pub mod scene;
pub mod sequences;
pub mod stage;
pub mod state;
pub mod stream;
pub mod surfaces;
pub mod system;
#[cfg(test)]
mod tests;
pub mod timecode;
pub mod transport;
