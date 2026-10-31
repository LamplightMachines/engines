//! `engines-part` (ADR 0039) as Claude's skills use it: list, dry run, add to a piece by slug (one
//! atomic write), a part from a file (ADR 0050), refusals and exit codes.

use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

const DOC: &str = r#"{
  schema_version: "0.9",
  transport: { bpm: 60, master_steps: 8 },
  harmony: { root: "D", mode: "dorian" },
  instruments: {
    pad: { note_engine: { polyphony: 1 }, params: { wave: "saw_soft", level_db: -10.46, cutoff_hz: 800 }, sends_db: { rv: -7.96 } },
  },
  modulators: {
    seq: { type: "step_seq", output: "note", length: 4, steps: [ { step: 0, voicing: { v1: { offset: 0 } } } ] },
  },
  matrix: {
    m_pad: { source: "seq", dest: "instruments.pad.notes", kind: "note", voices: 1 },
  },
  buses: {
    rv: { return_gain_db: -7.96, effects: { verb: { type: "reverb", params: { room: 0.7 } } } },
  },
}
"#;

/// A scratch dir holding `pieces/demo/demo.json5` (tests run in parallel: each its own `name`).
fn piece(name: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("engines-part-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("pieces/demo")).unwrap();
    let doc = base.join("pieces/demo/demo.json5");
    std::fs::write(&doc, DOC).unwrap();
    (base, doc)
}

fn run(base: &PathBuf, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_engines-part")).current_dir(base).args(args).output().unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into())
}

#[test]
fn engines_part_lists_dry_runs_and_adds() {
    let (base, doc) = piece("library");
    let (code, list, _) = run(&base, &["list"]);
    assert_eq!(code, 0);
    assert!(list.lines().count() == 9 && list.contains("pulse") && list.contains("[acts on an instrument's cutoff_hz]"), "{list}");

    let (code, out, _) = run(&base, &["add", "demo", "pulse", "--dry-run"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("would add `pulse` as `pulse`") && out.contains("+ instruments.pulse") && out.contains("+     pulse: {"), "{out}");
    assert_eq!(std::fs::read_to_string(&doc).unwrap(), DOC, "a dry run writes nothing");

    let (code, out, err) = run(&base, &["add", "demo", "pulse", "--json"]);
    assert_eq!(code, 0, "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!((v["ok"].clone(), v["id"].clone(), v["bus"].clone(), v["written"].clone()), (Value::Bool(true), "pulse".into(), "rv".into(), Value::Bool(true)));
    let text = std::fs::read_to_string(&doc).unwrap();
    assert!(text.contains("m_pulse:") && text.contains("pulse_cell:"));
    assert!(engines_core::check::check_str(&text).errors().next().is_none());

    // Again, then refusals: a taken id (1), an unknown part (2), a missing doc (2).
    let (code, out, _) = run(&base, &["add", "pieces/demo/demo.json5", "pulse"]);
    assert_eq!(code, 0);
    assert!(out.contains("as `pulse_2`"), "{out}");
    let (code, _, err) = run(&base, &["add", "demo", "breath", "--id", "seq"]);
    assert_eq!(code, 1);
    assert!(err.contains("taken"), "{err}");
    assert_eq!(run(&base, &["add", "demo", "nope"]).0, 2);
    assert_eq!(run(&base, &["add", "missing.json5", "pulse"]).0, 2);
    let (code, src, _) = run(&base, &["show", "breath"]);
    assert!(code == 0 && src.contains("$target"));

    // A doc that already has errors (the player refuses it) is fixed first: nothing is written.
    let broken = DOC.replace("cutoff_hz: 800", "cutof_hz: 800");
    std::fs::write(&doc, &broken).unwrap();
    let (code, _, err) = run(&base, &["add", "demo", "drone"]);
    assert_eq!(code, 1);
    assert!(err.contains("already has 1 error") && err.contains("cutof"), "{err}");
    assert_eq!(std::fs::read_to_string(&doc).unwrap(), broken, "the file is untouched");
    let (code, out, _) = run(&base, &["add", "demo", "drone", "--json"]);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!((code, v["ok"].clone(), v["existing_errors"].as_array().map(Vec::len)), (1, Value::Bool(false), Some(1)));
    let _ = std::fs::remove_dir_all(&base);
}

/// A part as Claude writes one for a piece (ADR 0050): the library's format, placeholders and all.
const PAD_PART: &str = r#"// Claude's patch: a soft triangle pad on the root.
{
  part: { summary: "a soft triangle pad on the root (from a snippet)" },
  instruments: {
    $id: {
      note_engine: { polyphony: 2 },
      params: { wave: "triangle", level_db: -13.98, cutoff_hz: 504.8, attack_s: 0.9, release_s: 1.6 },
      sends_db: { $bus: -10.46 },
    },
  },
  modulators: {
    $id_notes: { type: "step_seq", output: "note", length: 1, steps: [ { step: 0, voicing: { a: { offset: "$deg(1, -1)" }, b: { offset: "$deg(3, -1)" } } } ] },
  },
  matrix: {
    m_$id: { source: "$id_notes", dest: "instruments.$id.notes", kind: "note", voices: 1 },
  },
}
"#;

#[test]
fn engines_part_adds_a_part_file() {
    let (base, doc) = piece("file");
    std::fs::write(base.join("soft_pad.json5"), PAD_PART).unwrap();

    let (code, out, err) = run(&base, &["add", "demo", "--file", "soft_pad.json5", "--json"]);
    assert_eq!(code, 0, "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!((v["ok"].clone(), v["part"].clone(), v["id"].clone(), v["bus"].clone()), (Value::Bool(true), "soft_pad".into(), "soft_pad".into(), "rv".into()), "{v}");
    assert_eq!(v["file"], "soft_pad.json5");
    let text = std::fs::read_to_string(&doc).unwrap();
    assert!(text.contains("soft_pad: {") && text.contains("m_soft_pad:") && text.contains("sends_db: { rv: -10.46 }"), "{text}");
    assert!(text.contains("a: { offset: -12 }, b: { offset: -9 }"), "$deg in the doc's D dorian, an octave down: the root and the minor third — {text}");
    assert!(engines_core::check::check_str(&text).errors().next().is_none());

    // Again: a fresh id, like a library part; --id names it.
    let (code, out, _) = run(&base, &["add", "demo", "--file", "soft_pad.json5", "--dry-run"]);
    assert_eq!(code, 0);
    assert!(out.contains("would add `soft_pad.json5` as `soft_pad_2`") && out.contains("+ instruments.soft_pad_2"), "{out}");
    let (code, out, _) = run(&base, &["add", "demo", "--file", "soft_pad.json5", "--id", "glow"]);
    assert_eq!(code, 0);
    assert!(out.contains("as `glow`"), "{out}");

    // Refused (1), the doc untouched: a file that isn't a part, a part that would leave errors.
    let before = std::fs::read_to_string(&doc).unwrap();
    std::fs::write(base.join("headless.json5"), PAD_PART.replace("part: { summary: \"a soft triangle pad on the root (from a snippet)\" },", "")).unwrap();
    let (code, _, err) = run(&base, &["add", "demo", "--file", "headless.json5"]);
    assert_eq!(code, 1);
    assert!(err.contains("not a part: no `part` header"), "{err}");
    let (code, out, _) = run(&base, &["add", "demo", "--file", "headless.json5", "--json"]);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!((code, v["ok"].clone()), (1, Value::Bool(false)));
    std::fs::write(base.join("typo.json5"), PAD_PART.replace("cutoff_hz: 504.8", "cutof_hz: 504.8")).unwrap();
    let (code, _, err) = run(&base, &["add", "demo", "--file", "typo.json5"]);
    assert_eq!(code, 1);
    assert!(err.contains("would leave the doc with errors") && err.contains("cutof"), "{err}");
    assert_eq!(std::fs::read_to_string(&doc).unwrap(), before, "refusals write nothing");

    // A missing file (2); a part name and a file at once, or neither, is a usage error (2).
    assert_eq!(run(&base, &["add", "demo", "--file", "nope.json5"]).0, 2);
    assert_eq!(run(&base, &["add", "demo", "pulse", "--file", "soft_pad.json5"]).0, 2);
    assert_eq!(run(&base, &["add", "demo"]).0, 2);
    // The library is unchanged.
    let (code, list, _) = run(&base, &["list"]);
    assert_eq!((code, list.lines().count()), (0, 9), "{list}");
    let _ = std::fs::remove_dir_all(&base);
}
