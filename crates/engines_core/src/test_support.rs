//! Shared test fixtures (`#[cfg(test)]` only — never compiled into the lib).
//!
//! One home for the canonical-fixture loading that was previously copy-pasted into ~10
//! modules' test mods. The fixture lives with the lab suite so Rust and Python exercise the
//! SAME document (`lab/tests/fixtures/loop_contract_v0.9.json`, a smaller strict-JSON fixture of
//! the same shape as the annotated template `doc/loop_contract_v0.9.json5`).

#![allow(dead_code)] // consumers vary by feature set

use crate::loop_doc::LoopDoc;
use std::path::PathBuf;

/// Directory holding the shared Rust+Python test fixtures.
pub(crate) fn fixtures_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../lab/tests/fixtures"))
}

/// Path of the canonical v0.8 contract fixture.
pub(crate) fn fixture_path() -> PathBuf {
    fixtures_dir().join("loop_contract_v0.9.json")
}

/// The canonical fixture's raw JSON.
pub(crate) fn fixture_json() -> String {
    std::fs::read_to_string(fixture_path()).unwrap()
}

/// The canonical fixture, parsed. The workhorse: most engine/controller/session tests start here.
pub(crate) fn fixture() -> LoopDoc {
    LoopDoc::from_json_str(&fixture_json()).unwrap()
}

/// A small doc: a pad + a drone, an LFO on the pad cutoff, two note sources.
pub(crate) fn song_doc(song: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "schema_version": "0.9",
        "transport": { "bpm": 60, "master_steps": 4, "step_unit": "beat" },
        "harmony": { "root": "D", "mode": "dorian" },
        "instruments": {
            "pad":   { "params": { "wave": "sine", "level_db": -6.02, "cutoff_hz": 800, "sustain": 1.0, "release_s": 1.2 } },
            "drone": { "params": { "wave": "sine", "level_db": -7.96, "sustain": 1.0, "release_s": 1.2 } }
        },
        "modulators": {
            "chords": { "type": "step_seq", "output": "note", "sync": "loop", "length": 1,
                        "steps": [ { "step": 0, "voicing": { "a": { "offset": 0 }, "b": { "offset": 7 } } } ] },
            "root":   { "type": "step_seq", "output": "note", "sync": "loop", "length": 1,
                        "steps": [ { "step": 0, "voicing": { "r": { "offset": -12 } } } ] },
            "wob":    { "type": "lfo", "output": "control", "rate_hz": 0.5, "wave": "sine" }
        },
        "matrix": {
            "m_pad":   { "source": "chords", "dest": "instruments.pad.notes",   "kind": "note" },
            "m_drone": { "source": "root",   "dest": "instruments.drone.notes", "kind": "note" },
            "m_wob":   { "source": "wob",    "dest": "instruments.pad.params.cutoff_hz", "kind": "mod", "depth_oct": 1.329 }
        },
        "song": song
    })
}
