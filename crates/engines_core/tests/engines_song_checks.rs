//! `engines-song verify | seams | voyage` over the process boundary: the reports, `--json`,
//! and the exit codes the skills read (0 a pass, 1 a FAIL verdict, 2 a check that couldn't run).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A 2-s loop (120 bpm × 4 beats): a saw pad on D whose cutoff an LFO moves at `depth_oct`, sent to
/// a reverb; as a song, three short blocks (the middle one with the LFO a little shallower) and a
/// tail.
fn doc(depth_oct: f64, song: bool) -> String {
    let song = if song {
        r#"song: {
    matrices: { still: { w: { depth_oct: 0.9 } } },
    sequence: [ { loops: 3, label: "open" }, { matrix: "still", loops: 3, label: "still" }, { loops: 3, label: "close" } ],
    transition_s: 1,
    tail_loops: 2,
  },"#
    } else {
        ""
    };
    format!(
        r#"{{
  schema_version: "0.9",
  transport: {{ bpm: 120, master_steps: 4 }},
  harmony: {{ root: "D", mode: "dorian" }},
  global: {{ master_gain_db: -6 }},
  instruments: {{ pad: {{ note_engine: {{ polyphony: 2 }}, params: {{ wave: "saw_soft", level_db: -8, cutoff_hz: 900, sustain: 1.0, release_s: 0.6 }}, sends_db: {{ rv: -8 }} }} }},
  modulators: {{
    seq: {{ type: "step_seq", output: "note", length: 4, steps: [ {{ step: 0, voicing: {{ v1: {{ offset: 0 }}, v2: {{ offset: 7 }} }} }} ] }},
    wob: {{ type: "lfo", output: "control", wave: "sine", sync: "free", rate_hz: 1.5, polarity: "bipolar" }},
  }},
  matrix: {{
    m: {{ source: "seq", dest: "instruments.pad.notes", kind: "note", voices: 2 }},
    w: {{ source: "wob", dest: "instruments.pad.params.cutoff_hz", kind: "mod", depth_oct: {depth_oct} }},
  }},
  buses: {{ rv: {{ return_gain_db: -8, effects: {{ verb: {{ type: "reverb", params: {{ room: 0.5, damping: 0.4, width: 0.8 }} }} }} }} }},
  {song}
}}"#
    )
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("engines-song-checks-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn engines_song(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_engines-song")).current_dir(cwd).args(args).args(["--sample-rate", "24000"]).output().unwrap()
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("not one JSON object ({e}):\n{}", String::from_utf8_lossy(&out.stdout)))
}

#[test]
fn verify_reports_a_song_and_its_blocks_and_passes() {
    let d = scratch("verify");
    std::fs::write(d.join("song.json5"), doc(1.0, true)).unwrap();
    let out = engines_song(&d, &["verify", "song.json5", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let r = json(&out);
    assert_eq!(r["verdict"], "PASS");
    assert!(r["basics"]["peak"].as_f64().unwrap() > 0.05);
    assert_eq!(r["blocks"]["rows"].as_array().unwrap().len(), 3);
    assert!(r["centre"].is_null() && r["ab"].is_null(), "no centre, no --ab");
    // The text report carries the same verdict.
    let text = engines_song(&d, &["verify", "song.json5"]);
    assert!(String::from_utf8_lossy(&text.stdout).ends_with("VERDICT: PASS\n"));
    // One block on its own, with every route's share.
    let out = engines_song(&d, &["verify", "song.json5", "--block", "1", "--ab", "--f0", "146.83", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let r = json(&out);
    assert_eq!((r["block"]["label"].as_str(), r["blocks"].is_null()), (Some("still"), true));
    let ids: Vec<&str> = r["ab"]["routes"].as_array().unwrap().iter().map(|x| x["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["m", "w"]);
    assert!(r["ab"]["routes"].as_array().unwrap().iter().all(|x| x["audible"] == true));
    assert!(r["centre"]["on_series_pct"].as_f64().is_some());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_silent_route_fails_verify_with_exit_1() {
    let d = scratch("silent");
    std::fs::write(d.join("loop.json5"), doc(0.0, false)).unwrap();
    let out = engines_song(&d, &["verify", "loop.json5", "--ab"]);
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("** SILENT/UNWIRED -- fix **") && text.contains("FAIL: route w") && text.ends_with("VERDICT: FAIL\n"), "{text}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn seams_judges_the_whole_song_and_voyage_tabulates_it() {
    let d = scratch("seams");
    std::fs::write(d.join("song.json5"), doc(1.0, true)).unwrap();
    let out = engines_song(&d, &["seams", "song.json5", "--json", "--out", "small.wav"]);
    let r = json(&out);
    // The exit code is the verdict's (the seams' own verdicts are pinned by the unit tests).
    let fail = r["verdict"] == "FAIL";
    assert_eq!(out.status.code(), Some(if fail { 1 } else { 0 }), "{r}\n{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(fail, !r["fails"].as_array().unwrap().is_empty());
    assert_eq!(r["seams"].as_array().unwrap().len(), 2);
    assert_eq!(r["seams_checked"], 2);
    assert!(r["basics"]["clipped"] == 0 && r["tail"]["judged"] == true && r["tail"]["ring_s"].as_f64().is_some(), "{r}");
    // --out writes the render it measured: 9 loops + a 2-loop tail of 2 s at 24 kHz.
    let wav = engines_core::wav::WavReader::open(&d.join("small.wav")).unwrap();
    assert_eq!(wav.frames(), 11 * 2 * 24_000);
    // The voyage plays the same song, block by block.
    let out = engines_song(&d, &["voyage", "song.json5", "--movement", "default", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let r = json(&out);
    assert_eq!(r["rows"].as_array().unwrap().len(), 11);
    assert_eq!(r["movement"], "default");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_check_that_cannot_run_exits_2() {
    let d = scratch("usage");
    std::fs::write(d.join("loop.json5"), doc(1.0, false)).unwrap();
    std::fs::write(d.join("broken.json5"), "{ schema_version: \"0.9\", nope: 1 }").unwrap();
    for args in [
        vec!["verify", "missing.json5"],
        vec!["verify", "broken.json5"],
        vec!["seams", "loop.json5"],           // not a song
        vec!["verify", "loop.json5", "--block", "0"], // not a song either
        vec!["voyage", "loop.json5", "--loops", "0"],
    ] {
        let out = engines_song(&d, &args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn help_lists_the_checks_and_the_render_command_still_works() {
    let help = Command::new(env!("CARGO_BIN_EXE_engines-song")).arg("--help").output().unwrap();
    let text = String::from_utf8_lossy(&help.stdout);
    for c in ["verify", "seams", "voyage"] {
        assert!(text.lines().any(|l| l.trim_start().starts_with(c)), "--help lists {c}:\n{text}");
    }
    let d = scratch("render");
    std::fs::write(d.join("loop.json5"), doc(1.0, false)).unwrap();
    let out = engines_song(&d, &["--doc", "loop.json5", "--max-loops", "2", "--dry-run"]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let _ = std::fs::remove_dir_all(&d);
}
