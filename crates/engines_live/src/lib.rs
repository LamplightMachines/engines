//! # engines_live — the live player for a piece (ADR 0034; `engines-live` v0)
//!
//! A desktop shell over `engines_core`: [`player::LivePlayer`] owns the session, watches the piece's
//! doc ([`watch`]), applies accepted saves (history snapshots + `.live/status.json`), and the
//! binary puts the device output through the live-only protection [`limiter`]. [`app::run`] is the
//! whole player; a companion's build passes a [`hooks::NoteHook`] (Engines Studio's MIDI mirror).

pub mod app;
pub mod hooks;
pub mod limiter;
pub mod player;
pub mod watch;
