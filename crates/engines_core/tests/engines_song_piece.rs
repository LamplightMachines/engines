//! `engines-song --piece` over the process boundary (ADR 0034): renders land in the piece's
//! `renders/` as `<date>_r<rev>` with a `.json` sidecar, and every render records the doc revision
//! it came from in `.live/history`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A 4 s loop (60 bpm × 4 beats) with one pad voice.
const DOC: &str = r#"{
  schema_version: "0.9",
  transport: { bpm: 60, master_steps: 4 },
  harmony: { root: "D", mode: "dorian" },
  global: { master_gain_db: -6.02 },
  instruments: { pad: { note_engine: { polyphony: 2 }, params: { wave: "saw_soft", level_db: -7.96, cutoff_hz: 800, sustain: 1.0 } } },
  modulators: { seq: { type: "step_seq", output: "note", length: 4, steps: [ { step: 0, voicing: { v1: { offset: 0 } } } ] } },
  matrix: { m: { source: "seq", dest: "instruments.pad.notes", kind: "note", voices: 1 } },
}"#;

fn base(name: &str) -> PathBuf {
    let b = std::env::temp_dir().join(format!("engines-song-piece-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&b);
    std::fs::create_dir_all(b.join("pieces/demo")).unwrap();
    std::fs::write(b.join("pieces/demo/demo.json5"), DOC).unwrap();
    b
}

fn engines_song(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_engines-song")).current_dir(cwd).args(args).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "engines-song {args:?} failed:\n{err}");
    err
}

fn renders(b: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(b.join("pieces/demo/renders")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

fn sidecar(b: &Path, name: &str) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(b.join("pieces/demo/renders").join(name)).unwrap()).unwrap()
}

#[test]
fn a_piece_render_lands_in_renders_with_a_sidecar_and_a_revision() {
    let b = base("mix");
    let err = engines_song(&b, &["--piece", "demo", "--max-loops", "1"]);
    assert!(err.contains("piece: demo · r1"), "{err}");
    assert!(err.contains("LUFS"), "the summary reports loudness: {err}");
    let files = renders(&b);
    assert_eq!(files.len(), 2, "{files:?}");
    let flac = files.iter().find(|f| f.ends_with("_r1.flac")).expect("a <date>_r1.flac");
    let side = sidecar(&b, &flac.replace(".flac", ".json"));
    assert_eq!(side["piece"], "demo");
    assert_eq!(side["rev"], 1);
    assert_eq!(side["kind"], "mix");
    assert_eq!(side["frames"], 4 * 48_000);
    assert!(side["integrated_lufs"].as_f64().is_some_and(|l| l < 0.0), "{side}");
    assert_eq!(side["published"], false);
    // The build that rendered it (ADR 0054 P10): re-creatable with that engine.
    assert_eq!(side["engine_build"], engines_core::build_id());
    // And, built as a submodule, the embedding repo's revision; absent on its own.
    assert_eq!(side["host_build"].as_str(), engines_core::host_build(), "{side}");
    // The snapshot the sidecar names holds the exact doc text.
    let snap = b.join("pieces/demo").join(side["snapshot"].as_str().unwrap());
    assert_eq!(std::fs::read_to_string(snap).unwrap(), DOC);

    // Same doc again ⇒ same revision, a fresh file name; an edited doc ⇒ the next revision.
    engines_song(&b, &["--piece", "demo", "--max-loops", "1"]);
    assert!(renders(&b).iter().any(|f| f.ends_with("_r1_2.flac")), "{:?}", renders(&b));
    std::fs::write(b.join("pieces/demo/demo.json5"), DOC.replace("level_db: -7.96", "level_db: -10.46")).unwrap();
    let err = engines_song(&b, &["--doc", "pieces/demo/demo.json5", "--max-loops", "1"]);
    assert!(err.contains("piece: demo · r2"), "a doc path inside pieces/ is piece mode too: {err}");
    assert_eq!(std::fs::read_to_string(b.join("pieces/demo/.live/history/index.jsonl")).unwrap().lines().count(), 2);
    let _ = std::fs::remove_dir_all(b);
}

#[test]
fn a_dry_run_predicts_without_writing_and_bare_stems_pick_a_dir() {
    let b = base("stems");
    let err = engines_song(&b, &["--piece", "demo", "--max-loops", "1", "--dry-run"]);
    assert!(err.contains("r1 (not recorded — dry run)"), "{err}");
    assert!(!b.join("pieces/demo/renders").exists() && !b.join("pieces/demo/.live").exists(), "a dry run writes nothing");
    engines_song(&b, &["--piece", "demo", "--max-loops", "1", "--stems"]);
    let files = renders(&b);
    let dir = files.iter().find(|f| f.ends_with("_r1_stems")).expect("a stems dir");
    assert!(b.join("pieces/demo/renders").join(dir).join("stems.json").is_file());
    let side = sidecar(&b, &format!("{dir}.json"));
    assert_eq!(side["kind"], "stems");
    // A stems sidecar measures the mix like a mix render's does (ADR 0034 D3).
    assert_eq!(side["frames"], 4 * 48_000);
    assert!(side["integrated_lufs"].as_f64().is_some_and(|l| l < 0.0), "{side}");
    assert!(side["peak_dbfs"].as_f64().is_some() && side["clipped"] == 0 && side["duration_s"] == 4.0, "{side}");
    let _ = std::fs::remove_dir_all(b);
}

#[test]
fn a_piece_song_renders_a_directory_of_block_files() {
    // A song (ADR 0043): the mix lands in `renders/<date>_r<rev>/`, one FLAC per block named after
    // the piece, with the sidecar beside the directory (as a stems render's is).
    let b = base("song");
    let song = DOC.trim_end().trim_end_matches('}').to_string()
        + "  song: { sequence: [ { loops: 1, label: \"intro\" }, { loops: 1 } ], transition_s: 1, tail_loops: 1 },\n}";
    std::fs::write(b.join("pieces/demo/demo.json5"), &song).unwrap();
    engines_song(&b, &["--piece", "demo"]);
    let files = renders(&b);
    let dir = files.iter().find(|f| f.ends_with("_r1")).expect("a <date>_r1 directory");
    assert_eq!(files, [dir.clone(), format!("{dir}.json")]);
    let mut inside: Vec<String> = std::fs::read_dir(b.join("pieces/demo/renders").join(dir)).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    inside.sort();
    assert_eq!(inside, ["blocks.json", "demo-mix-00-intro.flac", "demo-mix-01-base.flac", "demo-mix-02-tail.flac"]);
    let side = sidecar(&b, &format!("{dir}.json"));
    assert_eq!((side["kind"].as_str(), side["manifest"].as_str(), side["files"].as_u64()), (Some("mix"), Some("blocks.json"), Some(3)));
    assert_eq!(side["frames"], 3 * 4 * 48_000);
    // `--whole`: the one file, as before.
    engines_song(&b, &["--piece", "demo", "--whole"]);
    assert!(renders(&b).iter().any(|f| f.ends_with("_r1_2.flac")), "{:?}", renders(&b));
    let _ = std::fs::remove_dir_all(b);
}

#[test]
fn a_refused_run_records_no_revision() {
    // History holds what renders: a doc the checker refuses, a usage error and a bad flag for the
    // doc each fail before anything lands in `.live/history`.
    let b = base("refused");
    let doc = b.join("pieces/demo/demo.json5");
    let index = b.join("pieces/demo/.live/history/index.jsonl");
    let run = |args: &[&str]| Command::new(env!("CARGO_BIN_EXE_engines-song")).current_dir(&b).args(args).output().unwrap();
    std::fs::write(&doc, DOC.replace("cutoff_hz:", "cutof_hz:")).unwrap();
    let out = run(&["--piece", "demo", "--max-loops", "1"]);
    assert_eq!(out.status.code(), Some(1), "{}", String::from_utf8_lossy(&out.stderr));
    std::fs::write(&doc, DOC).unwrap();
    let out = run(&["--piece", "demo"]); // a song-less doc needs --max-loops / --duration
    assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
    let out = run(&["--piece", "demo", "--max-loops", "1", "--emit-blocks", "blocks"]);
    assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!index.exists(), "no revision was recorded");
    assert!(!b.join("pieces/demo/renders").exists(), "and no renders dir was made");
    let _ = std::fs::remove_dir_all(b);
}

#[test]
fn a_json_file_is_not_a_piece_doc() {
    // Pieces are `<slug>.json5` (ADR 0034 D1): a `.json` beside it renders as a plain doc, from
    // the path it was given.
    let b = base("json");
    std::fs::write(b.join("pieces/demo/demo.json"), DOC).unwrap();
    let err = engines_song(&b, &["--doc", "pieces/demo/demo.json", "--max-loops", "1", "--out", "x.wav"]);
    assert!(!err.contains("piece:"), "{err}");
    assert!(b.join("x.wav").is_file());
    let _ = std::fs::remove_dir_all(b);
}

#[test]
fn a_sidecar_keeps_a_minus_infinity_guardrail() {
    // ADR 0054 P3: the sidecar's control goes through the doc writer, so a `-inf` floor is written
    // `-Infinity` (serde_json wrote `null`, which read back as no floor at all).
    let b = base("inf");
    engines_song(&b, &["--piece", "demo", "--max-loops", "1", "--floor", "instruments.pad.params.level_db=-inf"]);
    let name = renders(&b).into_iter().find(|f| f.ends_with(".json")).expect("a sidecar");
    let text = std::fs::read_to_string(b.join("pieces/demo/renders").join(&name)).unwrap();
    assert!(text.contains("\"instruments.pad.params.level_db\": -Infinity"), "{text}");
    let side: engines_core::doc_value::DocValue = json5::from_str(&text).unwrap();
    assert_eq!(side["control"]["guardrails"]["floors"]["instruments.pad.params.level_db"].as_f64(), Some(f64::NEG_INFINITY));
    assert_eq!(side["piece"], "demo", "the rest reads as before");
}
