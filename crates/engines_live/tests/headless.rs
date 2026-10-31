//! Headless `engines-live` against a temp piece (ADR 0034): saves are applied (status + history),
//! a broken save is refused while the last good revision keeps playing, `.live/pause` holds saves
//! back, and SIGTERM leaves the status `stopped`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const DOC: &str = r#"{
  schema_version: "0.9",
  transport: { bpm: 240, master_steps: 4 },
  harmony: { root: "D", mode: "dorian" },
  global: { master_gain_db: -6.02 },
  instruments: { pad: { note_engine: { polyphony: 2 }, params: { wave: "saw_soft", level_db: -7.96, cutoff_hz: 800, sustain: 1.0 } } },
  modulators: { seq: { type: "step_seq", output: "note", length: 4, steps: [ { step: 0, voicing: { v1: { offset: 0 } } } ] } },
  matrix: { m: { source: "seq", dest: "instruments.pad.notes", kind: "note", voices: 1 } },
}"#;

struct Live {
    child: Child,
    base: PathBuf,
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn start() -> Live {
    let base = std::env::temp_dir().join(format!("engines-live-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("pieces/demo")).unwrap();
    std::fs::write(base.join("pieces/demo/demo.json5"), DOC).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_engines-live"))
        .current_dir(&base)
        .args(["demo", "--headless", "--fast"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    Live { child, base }
}

fn status(base: &Path) -> serde_json::Value {
    std::fs::read_to_string(base.join("pieces/demo/.live/status.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(serde_json::Value::Null)
}

fn wait_for(base: &Path, what: &str, ok: impl Fn(&serde_json::Value) -> bool) -> serde_json::Value {
    let t0 = Instant::now();
    loop {
        let s = status(base);
        if ok(&s) {
            return s;
        }
        assert!(t0.elapsed() < Duration::from_secs(30), "timed out waiting for {what}; status: {s:#}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn save(base: &Path, text: &str) {
    std::fs::write(base.join("pieces/demo/demo.json5"), text).unwrap();
}

fn history_len(base: &Path) -> usize {
    std::fs::read_to_string(base.join("pieces/demo/.live/history/index.jsonl")).map_or(0, |s| s.lines().count())
}

#[test]
fn saves_apply_bad_saves_are_refused_and_pause_holds_them_back() {
    let live = start();
    let b = live.base.clone();
    wait_for(&b, "r1 playing", |s| s["applied_rev"] == 1 && s["state"] == "playing");
    assert_eq!(history_len(&b), 1);

    // An accepted save → the next revision plays.
    save(&b, &DOC.replace("level_db: -7.96", "level_db: -10.46"));
    let s = wait_for(&b, "r2 playing", |s| s["applied_rev"] == 2);
    assert!(s["applied_at_loop"].as_u64().unwrap() > 0);
    assert_eq!(s["applied_kind"], "value", "a level change is a value edit, crossfaded mid-loop");
    assert!(s["applied_at_s"].as_f64().is_some(), "live mode reports where in the loop it landed");
    assert_eq!(history_len(&b), 2);

    // A typo is refused; r2 keeps playing and isn't snapshotted.
    save(&b, &DOC.replace("cutoff_hz: 800", "cutof_hz: 800"));
    let s = wait_for(&b, "the refusal", |s| s["rejected"].is_object());
    assert_eq!(s["applied_rev"], 2);
    assert!(s["rejected"]["errors"][0]["path"].as_str().unwrap().ends_with("cutof_hz"), "{s:#}");
    assert_eq!(history_len(&b), 2);

    // Paused: a good save waits; unpausing applies it (and clears the refusal).
    std::fs::write(b.join("pieces/demo/.live/pause"), "").unwrap();
    wait_for(&b, "paused", |s| s["state"] == "paused");
    save(&b, &DOC.replace("level_db: -7.96", "level_db: -13.98"));
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(status(&b)["applied_rev"], 2, "held back while paused");
    std::fs::remove_file(b.join("pieces/demo/.live/pause")).unwrap();
    let s = wait_for(&b, "r3 playing", |s| s["applied_rev"] == 3);
    assert!(s["rejected"].is_null());
    assert_eq!(history_len(&b), 3);

    // Saving a different `optimize` switches the mode (the interim hold/drift toggle).
    assert_eq!(s["optimize"], "drift", "no optimize in the doc ⇒ the default");
    save(&b, &DOC.replace("level_db: -7.96", "level_db: -13.98").replacen("schema_version: \"0.9\",", "schema_version: \"0.9\",\n  optimize: \"hold\",", 1));
    wait_for(&b, "hold playing", |s| s["applied_rev"] == 4 && s["optimize"] == "hold");

    // SIGTERM ⇒ a clean stop.
    #[cfg(unix)]
    unsafe {
        libc::kill(live.child.id() as libc::pid_t, libc::SIGTERM);
    }
    #[cfg(unix)]
    wait_for(&b, "stopped", |s| s["state"] == "stopped");
}
