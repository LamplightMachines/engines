//! What the player does with a session's notes, if anything: the free `engines-live` has no
//! [`NoteHook`]; Engines Studio's player runs the same player with the MIDI mirror's (ADR 0048,
//! the closed `engines_mirror`), so no MIDI code ships in the open one.
//!
//! The hook sees the session's **note tap** (the engine's per-instrument Note-On/Off stream,
//! `Session::poll_note_tap`): it says which instruments to tap for a routed session, adds what it
//! reports to the routing status, gets the session's audio clock, and ends what's sounding when a
//! session stops.

use engines_core::loop_doc::LoopDoc;
use engines_core::midi::TimedTap;
use engines_core::routing::RoutingMap;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

/// A routed session's plan from a [`NoteHook`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NotePlan {
    /// The instruments whose notes the session taps.
    pub taps: BTreeSet<String>,
    /// Fields added to `routing` (the mirror's `midi`).
    pub status: Map<String, Value>,
    /// Fields added to each routed pair, by its source (`inst:pad`): the mirror's `midi` / `no_midi`.
    pub pairs: BTreeMap<String, Map<String, Value>>,
}

/// What a companion player does with the notes a session plays.
pub trait NoteHook: Send {
    /// The capability `ready` reports for it (`"midi_mirror"`).
    fn capability(&self) -> &'static str;
    /// Plan a session routed by `map`, playing `doc`.
    fn plan(&mut self, map: &RoutingMap, doc: &LoopDoc) -> NotePlan;
    /// The planned session starts; an error to report, if any.
    fn begin(&mut self) -> Option<String>;
    /// Session `epoch`'s output started: `played` counts the ring samples the device has played,
    /// `width` to a frame — the clock its notes go out against.
    fn attach(&mut self, epoch: u64, played: Arc<AtomicU64>, width: u64);
    /// Notes session `epoch` tapped.
    fn push(&mut self, epoch: u64, taps: Vec<TimedTap>);
    /// The session stopped: end what's sounding.
    fn flush(&mut self);
}
