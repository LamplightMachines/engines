//! # engines_core — the Engines core
//!
//! Portable, deterministic, **I/O-free** core of a composing instrument for long-form generative
//! tracks (ADR 0031): synthesis + loop sequencer + modulators + matrix + songs + the controller's
//! drift / Orrery movement. No audio device, no files, no wall-clock — the shell owns all of that
//! (CE-R1.2). Same doc + seed ⇒ bit-identical render (CE-R7.3), pinned by `tests/golden.rs`.

pub mod analysis;
pub mod catalog;
pub mod check;
pub mod contract;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod build_stamp;
pub mod doc_value;
pub mod dsp;
pub mod effects;
pub mod ffi;
pub mod engine;
pub mod euclid;
pub mod hash;
pub mod loop_doc;
pub mod loudness;
pub mod params;

// Synthesis → sequencer/matrix → controller (drift + Orrery) → session:
pub mod categorical;
pub mod controller;
pub mod matrix;
pub mod midi;
pub mod midi_log;
pub mod modulator;
pub mod orrery;
pub mod phrase;
pub mod schema;
pub mod session;
pub mod song;
pub mod synth;
pub mod telemetry;
pub mod theory;
pub mod voice;
pub mod wav;

// Desktop MIDI-out delivery (ADR 0026) — optional, off by default so the core stays
// I/O-free (CE-R1.2); bin/shell-side like cpal. Enable with `--features midi`.
#[cfg(feature = "midi")]
pub mod midi_out;

// Streaming FLAC output for `engines-song` (pure-Rust `flacenc`) — bin-side like `wav`, on in the
// default build; the lean core (`--no-default-features`) doesn't carry it.
#[cfg(feature = "flac")]
pub mod flac;

// The `pieces/<slug>/` project layout (ADR 0034): paths, history snapshots, render naming —
// desktop file I/O, feature-gated like `flac`.
#[cfg(feature = "pieces")]
pub mod piece;

// The piece's routing map (ADR 0044): which device pair each audio source plays on — shared by
// `engines-live --route-sources` and the Live set builder, which keeps its own fields in it.
#[cfg(feature = "pieces")]
pub mod routing;

// Loudness-normalized stem export (ADR 0030), shared by `engines-song --stems` and `engines-live`'s stem
// previews (ADR 0040) — desktop file I/O like `pieces`.
#[cfg(feature = "stems")]
pub mod stems;

/// Shared desktop playback shell (cpal): device resolution, output/return streams, MIDI-pump
/// shutdown — used by `engines-play` and the TUI's `EngineLink` (feature `playback`).
#[cfg(feature = "playback")]
pub mod playback;

/// Core version string.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The engine build (ADR 0054 P10): `<version>+g<12-hex commit>`, `.dirty` when the engine's sources
/// differed from that commit, or `<version>+unknown` built without git (set by `build.rs`). Render
/// sidecars record it as `engine_build`: a render is re-creatable with the build that rendered it.
pub fn build_id() -> &'static str {
    env!("ENGINES_BUILD_ID")
}

/// The host build: built as a submodule of another repo, that repo's `g<12-hex commit>`,
/// `.dirty` when its tracked files differed (as `build.rs` last saw them); `None` built on its own.
/// Sidecars record it as `host_build` next to `engine_build`, so a render names both revisions.
pub fn host_build() -> Option<&'static str> {
    Some(env!("ENGINES_HOST_BUILD")).filter(|h| !h.is_empty())
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_build_id_is_the_version_plus_the_commit() {
        let id = super::build_id();
        let rest = id.strip_prefix(super::version()).and_then(|r| r.strip_prefix('+')).unwrap_or_else(|| panic!("{id}"));
        let commit = rest.strip_suffix(".dirty").unwrap_or(rest);
        let is_sha = commit.strip_prefix('g').is_some_and(|h| h.len() == 12 && h.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(is_sha || rest == "unknown", "{id}");
    }
}
