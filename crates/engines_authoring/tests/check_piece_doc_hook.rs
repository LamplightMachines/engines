//! The PostToolUse hook (`.claude/hooks/check-piece-doc.sh`) run on a piece doc with the built
//! `engines-check`: a doc with warnings goes back to Claude as context — the checker's warning that a
//! `midi` instrument makes no sound in `engines-live` among them. Needs `jq` (skipped without it).

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

#[test]
fn the_hook_reports_a_midi_instrument_as_silent() {
    if Command::new("jq").arg("--version").output().is_err() {
        eprintln!("skipped: no jq");
        return;
    }
    let base = std::env::temp_dir().join(format!("engines-hook-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("pieces/demo")).unwrap();
    let doc = base.join("pieces/demo/demo.json5");
    std::fs::write(&doc, r#"{
  schema_version: "0.9",
  transport: { bpm: 60, master_steps: 4 },
  harmony: { root: "D", mode: "dorian" },
  instruments: {
    pad: { note_engine: { polyphony: 1 }, params: { wave: "sine", level_db: -10.46 } },
    ext: { type: "midi", note_engine: { polyphony: 1 }, routing: { channel: 2 } },
  },
}"#).unwrap();
    let hook = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.claude/hooks/check-piece-doc.sh");
    let mut child = Command::new("bash")
        .arg(&hook)
        .env("ENGINES_CHECK", env!("CARGO_BIN_EXE_engines-check"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let input = serde_json::json!({ "tool_input": { "file_path": doc.to_str().unwrap() } }).to_string();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)));
    let ctx = v["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert!(ctx.contains("makes no sound in `engines-live`"), "{ctx}");
    let _ = std::fs::remove_dir_all(base);
}
