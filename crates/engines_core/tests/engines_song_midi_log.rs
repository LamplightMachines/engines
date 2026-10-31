//! `engines-song --stems` writes a MIDI log beside the stems (ADR 0048): every mirrored instrument's
//! notes on the render's frame clock, the per-loop feeds and chords, each source's pass starts and
//! the song's blocks — the same render twice gives the same file.

use std::path::{Path, PathBuf};
use std::process::Command;

use engines_core::midi_log::{MidiLog, FILE, SCHEMA};

/// A 2 s loop (120 bpm × 4 beats): a held chord on `pad`, a free-run phrase on `pluck`; a song of two
/// 2-loop blocks and a 1-loop tail.
const DOC: &str = r#"{
  schema_version: "0.9",
  transport: { bpm: 120, master_steps: 4 },
  harmony: { root: "D", mode: "dorian" },
  global: { master_gain_db: -6.02 },
  instruments: {
    pad: { note_engine: { polyphony: 3 }, params: { wave: "sine", level_db: -13.98, sustain: 1.0, release_s: 0.4 } },
    pluck: { note_engine: { polyphony: 2 }, params: { wave: "sine", level_db: -13.98, sustain: 0.5, release_s: 0.4 } },
  },
  modulators: {
    chord: { type: "step_seq", output: "note", length: 4, steps: [ { step: 0, voicing: { v1: { offset: 0 }, v2: { offset: 3 }, v3: { offset: 7 } } } ] },
    ph: { type: "step_seq", output: "note", sync: "free", length: 4, step_duration: "1/4", phrase: { pool: ["a", "b"], select: "cycle" } },
  },
  sequence_pools: {
    a: { fit: "literal", length: 3, notes: [ { t: 0, offset: 12, dur: 1 }, { t: 1.5, offset: 14, dur: 0.5 } ] },
    b: { fit: "literal", length: 2, notes: [ { t: 0, offset: 10, dur: 1 } ] },
  },
  matrix: {
    m_chord: { source: "chord", dest: "instruments.pad.notes", kind: "note", voices: 3 },
    m_ph: { source: "ph", dest: "instruments.pluck.notes", kind: "note", voices: 2 },
  },
  song: { sequence: [ { loops: 2, label: "one" }, { loops: 2, label: "two" } ], tail_loops: 1, transition_s: 0 },
}"#;

fn scratch(tag: &str, doc: &str) -> PathBuf {
    let b = std::env::temp_dir().join(format!("engines-song-midi-log-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&b);
    std::fs::create_dir_all(&b).unwrap();
    std::fs::write(b.join("doc.json5"), doc).unwrap();
    b
}

fn render(base: &Path, extra: &[&str]) {
    let out = Command::new(env!("CARGO_BIN_EXE_engines-song"))
        .current_dir(base)
        .args(["--doc", "doc.json5", "--stems", "stems", "--container", "wav", "--sample-rate", "8000", "--seed", "7"])
        .args(extra)
        .output()
        .unwrap();
    assert!(out.status.success(), "engines-song failed:\n{}", String::from_utf8_lossy(&out.stderr));
}

fn read_log(base: &Path) -> (MidiLog, serde_json::Value) {
    let manifest: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(base.join("stems/stems.json")).unwrap()).unwrap();
    let log: MidiLog = serde_json::from_str(&std::fs::read_to_string(base.join("stems").join(FILE)).unwrap()).unwrap();
    (log, manifest)
}

#[test]
fn a_stems_render_logs_every_mirrored_instruments_notes_and_passes() {
    let base = scratch("log", DOC);
    render(&base, &["--orrery"]);
    let (log, manifest) = read_log(&base);
    assert_eq!(manifest["midi"], FILE, "the manifest names the log");
    assert_eq!(log.schema, SCHEMA);
    // Both name the build that rendered them (ADR 0054 P10).
    assert_eq!((log.engine_build.as_str(), &manifest["engine_build"]), (engines_core::build_id(), &serde_json::json!(engines_core::build_id())));
    assert_eq!((log.sample_rate, log.bpm, log.root.as_str()), (8000, 120.0, "D"));
    assert_eq!(log.frames, manifest["frames"].as_u64().unwrap());

    // Blocks agree with the stems' own block table.
    let blocks: Vec<(u64, u64, bool)> = log.blocks.iter().map(|b| (b.start_frame, b.frames, b.tail)).collect();
    let want: Vec<(u64, u64, bool)> = manifest["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| (b["start_frame"].as_u64().unwrap(), b["frames"].as_u64().unwrap(), b["tail"].as_bool().unwrap_or(false)))
        .collect();
    assert_eq!(blocks, want);
    assert_eq!(log.blocks.iter().map(|b| b.label.as_str()).collect::<Vec<_>>(), vec!["one", "two", "tail"]);

    // One record per loop, each with its feeds and (with --orrery) its chord.
    assert_eq!(log.loops.len(), 5);
    assert!(log.loops.iter().take(4).all(|l| l.feeds["pad"] == ["chord"] && l.feeds["pluck"] == ["ph"]));
    assert!(log.loops.iter().all(|l| l.chord.is_some()), "chords with the Orrery");

    // Notes: balanced, sorted, never overlapping on one key.
    for (inst, i) in &log.instruments {
        assert!(!i.notes.is_empty(), "{inst} played");
        assert!(i.notes.windows(2).all(|w| w[0] <= w[1]), "{inst}: sorted");
        for (k, n) in i.notes.iter().enumerate() {
            assert!(n.0 <= n.1 && n.1 <= log.frames, "{inst}: {n:?} inside the render");
            assert!(!i.notes[k + 1..].iter().any(|m| m.2 == n.2 && m.0 < n.1), "{inst}: {n:?} overlaps a later note on its key");
        }
    }
    // The held chord ends where the tail drops every note route.
    let tail_start = log.blocks.last().unwrap().start_frame;
    let pad_ends: Vec<u64> = log.instruments["pad"].notes.iter().map(|n| n.1).collect();
    assert!(pad_ends.iter().all(|&e| e <= tail_start), "the held chord ends by the tail: {pad_ends:?} vs {tail_start}");
    assert!(pad_ends.contains(&tail_start));

    // The phrase source's passes: absolute, increasing, named by entry (a 3-step then a 2-step pass).
    let ph = &log.sources["ph"];
    assert_eq!(ph.kind, "phrase");
    assert!(ph.tiles.windows(2).all(|w| w[0].start < w[1].start && w[1].ordinal == w[0].ordinal + 1), "{:?}", ph.tiles);
    let names: Vec<&str> = ph.tiles.iter().take(2).map(|t| log.entries[t.entry.unwrap() as usize].as_str()).collect();
    assert_eq!(names, ["a", "b"]);
    assert_eq!(log.sources["chord"].kind, "steps");

    // The same render twice gives the same log, byte for byte.
    let first = std::fs::read(base.join("stems").join(FILE)).unwrap();
    render(&base, &["--orrery"]);
    assert_eq!(std::fs::read(base.join("stems").join(FILE)).unwrap(), first);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_doc_with_nothing_to_mirror_writes_no_log_and_removes_the_old_one() {
    let base = scratch("stale", DOC);
    render(&base, &[]);
    assert!(base.join("stems").join(FILE).exists());
    // Only a binaural pair pinned to root_hz left: nothing can be mirrored.
    let pinned = r#"{
      schema_version: "0.9",
      transport: { bpm: 120, master_steps: 4 },
      harmony: { root: "D", mode: "dorian" },
      global: { master_gain_db: -6.02 },
      instruments: { bin: { type: "binaural", note_engine: { polyphony: 1 }, params: { wave: "sine", level_db: -13.98, sustain: 1.0, root_hz: 110, spread_hz: 4 } } },
      modulators: { seq: { type: "step_seq", output: "note", length: 4, steps: [ { step: 0, voicing: { v: { offset: 0 } } } ] } },
      matrix: { m: { source: "seq", dest: "instruments.bin.notes", kind: "note", voices: 1 } },
    }"#;
    std::fs::write(base.join("doc.json5"), pinned).unwrap();
    render(&base, &["--max-loops", "1"]);
    assert!(!base.join("stems").join(FILE).exists(), "the previous export's log is gone");
    let manifest: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(base.join("stems/stems.json")).unwrap()).unwrap();
    assert!(manifest.get("midi").is_none());
    let _ = std::fs::remove_dir_all(&base);
}
