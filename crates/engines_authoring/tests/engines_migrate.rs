//! `engines-migrate` over the process boundary: a doc rewritten in place, `--dry-run` writing nothing,
//! `--json`, the exit codes, and `--piece` — the doc, its history and objectives migrated with v0.8
//! backups, renders and the Live set never touched.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

const DOC: &str = r#"{
  schema_version: "0.8",
  // a dark pad: cutoff 0.3 ≈ 318 Hz
  instruments: { pad: { params: { cutoff: 0.3, level: 0.5 }, sends: { rv: 0.5 } } },
  buses: { rv: { return_gain: 0.25, effects: {} } },
}
"#;

fn scratch(name: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("engines-migrate-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    base
}

fn run(base: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_engines-migrate")).current_dir(base).args(args).output().unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into())
}

#[test]
fn a_doc_is_migrated_in_place_and_a_dry_run_writes_nothing() {
    let base = scratch("doc");
    std::fs::write(base.join("a.json5"), DOC).unwrap();
    let (code, out, err) = run(&base, &["a.json5", "--dry-run"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("would migrate a.json5: ") && out.contains("cutoff → cutoff_hz") && out.contains("+    4   instruments: { pad: { params: { cutoff_hz: 318.49, level_db: -6.02 }"), "{out}");
    assert_eq!(std::fs::read_to_string(base.join("a.json5")).unwrap(), DOC, "a dry run writes nothing");

    let (code, out, err) = run(&base, &["a.json5"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("migrated a.json5: "), "{out}");
    let text = std::fs::read_to_string(base.join("a.json5")).unwrap();
    assert!(text.contains(r#"schema_version: "0.9""#) && text.contains("// a dark pad: cutoff 0.3 ≈ 318 Hz TODO(v0.9)") && text.contains("sends_db: { rv: -6.02 }"), "{text}");

    let (code, out, _) = run(&base, &["a.json5"]);
    assert_eq!(code, 0);
    assert!(out.contains("1 file already v0.9"), "a second run is a no-op: {out}");
}

#[test]
fn json_reports_every_change_and_exit_codes_tell_refused_from_missing() {
    let base = scratch("json");
    std::fs::write(base.join("a.json5"), DOC).unwrap();
    std::fs::write(base.join("old.json5"), r#"{ schema_version: "0.7" }"#).unwrap();
    let (code, out, _) = run(&base, &["a.json5", "--json", "--dry-run"]);
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(&out).unwrap();
    let f = &v["files"][0];
    assert_eq!((v["ok"].clone(), f["kind"].clone(), f["written"].clone(), f["unchanged"].clone()), (true.into(), "loop".into(), false.into(), false.into()));
    assert!(f["changes"].as_array().unwrap().iter().any(|c| c["path"] == "instruments.pad.params.cutoff" && c["to"] == "cutoff_hz: 318.49"), "{f}");

    let (code, _, err) = run(&base, &["old.json5"]);
    assert_eq!(code, 1, "a doc that isn't v0.8 is refused");
    assert!(err.contains("reads \"0.8\""), "{err}");
    let (code, _, _) = run(&base, &["nope.json5"]);
    assert_eq!(code, 2, "a missing file");
}

#[test]
fn a_piece_migrates_its_doc_history_and_objectives_and_leaves_renders_alone() {
    let base = scratch("piece");
    let p = base.join("pieces/demo");
    for d in ["renders", "ableton", ".live/history"] {
        std::fs::create_dir_all(p.join(d)).unwrap();
    }
    std::fs::write(p.join("demo.json5"), DOC).unwrap();
    let old_rev = DOC.replace("cutoff: 0.3", "cutoff: 0.5");
    std::fs::write(p.join(".live/history/r00001-aaaa.json5"), &old_rev).unwrap();
    std::fs::write(p.join(".live/history/index.jsonl"), "{\"rev\":1}\n").unwrap();
    let obj = r#"{ schema_version: "objectives_v0.1", overrides: { "instruments.pad.params.level": 0.25 } }"#;
    std::fs::write(p.join("demo.objectives.json5"), obj).unwrap();
    std::fs::write(p.join("renders/night.flac"), b"audio").unwrap();
    std::fs::write(p.join("renders/night.flac.asd"), b"ableton").unwrap();
    std::fs::write(p.join("ableton/demo.als"), b"live set").unwrap();

    let (code, out, err) = run(&base, &["--piece", "demo"]);
    assert_eq!(code, 0, "{err}\n{out}");
    assert!(std::fs::read_to_string(p.join("demo.json5")).unwrap().contains("cutoff_hz: 318.49"));
    assert!(std::fs::read_to_string(p.join(".live/history/r00001-aaaa.json5")).unwrap().contains("cutoff_hz: 800"), "history migrates so a revert brings back v0.9");
    assert!(std::fs::read_to_string(p.join("demo.objectives.json5")).unwrap().contains(r#""instruments.pad.params.level_db": -12.04"#));
    // The v0.8 originals are kept; the index (it hashes what played) and everything else is untouched.
    assert_eq!(std::fs::read_to_string(p.join(".v0.8-backup/demo.json5")).unwrap(), DOC);
    assert_eq!(std::fs::read_to_string(p.join(".v0.8-backup/.live/history/r00001-aaaa.json5")).unwrap(), old_rev);
    assert_eq!(std::fs::read_to_string(p.join(".v0.8-backup/demo.objectives.json5")).unwrap(), obj);
    assert_eq!(std::fs::read_to_string(p.join(".live/history/index.jsonl")).unwrap(), "{\"rev\":1}\n");
    assert_eq!(std::fs::read(p.join("renders/night.flac")).unwrap(), b"audio");
    assert_eq!(std::fs::read(p.join("renders/night.flac.asd")).unwrap(), b"ableton");
    assert_eq!(std::fs::read(p.join("ableton/demo.als")).unwrap(), b"live set");
    assert!(!p.join(".v0.8-backup/renders").exists() && !p.join(".v0.8-backup/ableton").exists());

    // Again: nothing to do, and the backups still hold the v0.8 text.
    let (code, out, _) = run(&base, &["--piece", "demo"]);
    assert_eq!(code, 0);
    assert!(out.contains("3 files already v0.9"), "{out}");
    assert_eq!(std::fs::read_to_string(p.join(".v0.8-backup/demo.json5")).unwrap(), DOC);

    let (code, _, err) = run(&base, &["--piece", "nope"]);
    assert_eq!(code, 2, "{err}");
}
