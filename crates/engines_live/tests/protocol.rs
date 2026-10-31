//! `engines-live --stdio` (ADR 0036): the JSON-lines protocol over pipes, headless. Walks the contract in
//! `contracts/engines_live_protocol_v1.md`: ready → ticks → doc edits (accepted / applied / rejected /
//! unchanged) → mute → hold → song audition → preview → errors → quit.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A 1 s loop (240 bpm × 4 beats), a held pad at amplitude `level` (written as its `level_db`), and a
/// two-block song (block 1 drops the pad).
fn doc(level: f32) -> String {
    let level = format!("{:.2}", 20.0 * level.log10());
    format!(
        r#"{{
  schema_version: "0.9",
  transport: {{ bpm: 240, master_steps: 4 }},
  harmony: {{ root: "D", mode: "dorian" }},
  global: {{ master_gain_db: -6.02 }},
  instruments: {{ pad: {{ note_engine: {{ polyphony: 2 }}, params: {{ wave: "saw_soft", level_db: {level}, cutoff_hz: 800, sustain: 1.0 }} }} }},
  modulators: {{ seq: {{ type: "step_seq", output: "note", length: 4, steps: [ {{ step: 0, voicing: {{ v1: {{ offset: 0 }} }} }} ] }} }},
  matrix: {{ m: {{ source: "seq", dest: "instruments.pad.notes", kind: "note", voices: 1 }} }},
  song: {{ matrices: {{ thin: {{ m: null }} }}, sequence: [ {{ loops: 2, label: "full" }}, {{ matrix: "thin", loops: 2, label: "thin" }} ], transition_s: 0.5, tail_loops: 1 }},
}}"#
    )
}

struct Live {
    child: Child,
    stdin: ChildStdin,
    events: Arc<Mutex<Vec<Value>>>,
    seen: usize,
    base: PathBuf,
}

static RUNS: AtomicUsize = AtomicUsize::new(0);

/// A fresh `pieces/demo` holding `doc(0.4)`, in a temp dir of its own (tests run in parallel).
fn piece_dir(extra: &[&str]) -> PathBuf {
    let run = RUNS.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!("engines-live-proto-{}-{run}-{}", std::process::id(), extra.join("_")));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("pieces/demo")).unwrap();
    std::fs::write(base.join("pieces/demo/demo.json5"), doc(0.4)).unwrap();
    base
}

impl Live {
    fn start(extra: &[&str]) -> Live {
        Self::start_at(piece_dir(extra), extra)
    }

    /// [`start`](Self::start) on a piece holding `text` instead of `doc(0.4)`.
    fn start_with(text: &str, extra: &[&str]) -> Live {
        let base = piece_dir(extra);
        std::fs::write(base.join("pieces/demo/demo.json5"), text).unwrap();
        Self::start_at(base, extra)
    }

    fn start_at(base: PathBuf, extra: &[&str]) -> Live {
        let mut child = Command::new(env!("CARGO_BIN_EXE_engines-live"))
            .current_dir(&base)
            .args(["demo", "--stdio", "--headless", "--fast"])
            .args(extra)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let ev = events.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let v: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON ({e}): {line}"));
                ev.lock().unwrap().push(v);
            }
        });
        Live { child, stdin, events, seen: 0, base }
    }

    fn send(&mut self, cmd: Value) {
        writeln!(self.stdin, "{cmd}").unwrap();
        self.stdin.flush().unwrap();
    }

    /// Wait for the next event (after everything already consumed) matching `pred`.
    fn wait(&mut self, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        let t0 = Instant::now();
        loop {
            {
                let evs = self.events.lock().unwrap();
                if let Some(i) = evs[self.seen..].iter().position(&pred) {
                    self.seen += i + 1;
                    return evs[self.seen - 1].clone();
                }
            }
            if t0.elapsed() > Duration::from_secs(30) {
                let evs = self.events.lock().unwrap();
                let tail: Vec<String> = evs[self.seen..].iter().rev().take(12).map(|e| e.to_string()).collect();
                panic!("timed out waiting for {what}; recent events: {tail:#?}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn ty(t: &'static str) -> impl Fn(&Value) -> bool {
    move |e| e["type"] == t
}

#[test]
fn the_protocol_round_trip() {
    let mut live = Live::start(&[]);
    let ready = live.wait("ready", ty("ready"));
    assert_eq!(ready["capabilities"], json!([]), "the free player adds nothing: no MIDI mirror");
    assert_eq!(ready["protocol"], 1);
    assert_eq!(ready["rev"], 1);
    assert_eq!(ready["instruments"], json!(["pad"]));
    assert_eq!(ready["song"]["blocks"][1]["label"], "thin");
    let key = &ready["key"];
    assert_eq!((key["name"].clone(), key["root"].clone()), (json!("D dorian"), json!(2)));
    assert_eq!(key["scale"], json!([0, 2, 3, 5, 7, 9, 10]));
    assert_eq!(key["degrees"][2], "♭3");
    assert_eq!((key["names"][5].clone(), key["names"][6].clone(), key["names"][10].clone()), (json!("F"), json!("F#"), json!("Bb")));
    // ADR 0040: the limiter's state, the loop's steps, the song's tail; nothing playing from a block.
    assert_eq!((ready["limiter"].clone(), ready["master_steps"].clone(), ready["note"].clone()), (json!(true), json!(4), Value::Null));
    let song = &ready["song"];
    assert_eq!((song["from"].clone(), song["current"].clone(), song["tail_loops"].clone()), (Value::Null, Value::Null, json!(1)));

    let tick = live.wait("a sounding tick", |e| e["type"] == "tick" && e["meters"]["pad"].as_f64().is_some_and(|d| d > -60.0));
    assert!(tick["steps"]["seq"].is_u64(), "{tick}");
    // Left and right peaks beside the max of both; no song position in loop mode.
    let (l, r) = (tick["meters_lr"]["pad"][0].as_f64().unwrap(), tick["meters_lr"]["pad"][1].as_f64().unwrap());
    assert!((l.max(r) - tick["meters"]["pad"].as_f64().unwrap()).abs() < 1e-3, "{tick}");
    let (ml, mr) = (tick["master_lr"][0].as_f64().unwrap(), tick["master_lr"][1].as_f64().unwrap());
    assert!((ml.max(mr) - tick["master_db"].as_f64().unwrap()).abs() < 1e-3, "{tick}");
    assert!(tick["song"].is_null(), "{tick}");

    // A buffer edit: accepted (a history revision), then applied mid-loop.
    live.send(json!({ "type": "doc", "text": doc(0.3), "version": 7, "id": "d7" }));
    let acc = live.wait("accepted", ty("accepted"));
    assert_eq!((acc["version"].clone(), acc["rev"].clone(), acc["id"].clone()), (json!(7), json!(2), json!("d7")));
    let applied = live.wait("applied r2", |e| e["type"] == "applied" && e["rev"] == 2);
    assert_eq!(applied["kind"], "value");
    assert_eq!(applied["id"], "d7", "the asynchronous `applied` echoes the doc command's id");

    // A typo is refused with located errors; identical text is `unchanged`.
    live.send(json!({ "type": "doc", "text": doc(0.3).replace("cutoff", "cutof"), "version": 8 }));
    let rej = live.wait("rejected", ty("rejected"));
    assert_eq!(rej["version"], 8);
    assert!(rej["errors"][0]["path"].as_str().unwrap().ends_with("cutof_hz"), "{rej}");
    live.send(json!({ "type": "doc", "text": doc(0.3), "version": 9 }));
    let same = live.wait("unchanged", ty("unchanged"));
    assert_eq!((same["rev"].clone(), same["source"].clone()), (json!(2), json!("command")));

    // Mute: the pad's meter falls silent.
    live.send(json!({ "type": "mute", "instrument": "pad", "on": true, "id": 31 }));
    let gains = live.wait("gains", ty("gains"));
    assert_eq!((gains["muted"].clone(), gains["id"].clone()), (json!(["pad"]), json!(31)));
    live.wait("a silent pad", |e| e["type"] == "tick" && e["meters"]["pad"].as_f64().is_some_and(|d| d <= -119.0));
    live.send(json!({ "type": "mute", "instrument": "pad", "on": false }));
    live.wait("gains", ty("gains"));

    // Hold: the control switches at the next loop.
    live.send(json!({ "type": "control", "optimize": "hold", "id": "c1" }));
    let control = live.wait("control", ty("control"));
    assert_eq!((control["optimize"].clone(), control["id"].clone()), (json!("hold"), json!("c1")));
    live.wait("a hold loop", |e| e["type"] == "loop" && e["optimize"] == "hold");

    // Song audition: loop one block, play the song from block 1 to its end, back to the loop.
    live.send(json!({ "type": "song", "action": "loop_block", "block": 1 }));
    let s = live.wait("song block", ty("song"));
    assert_eq!((s["mode"].clone(), s["block"].clone()), (json!("block"), json!(1)));
    // The looped block counts its loops (wrapping at its length: it repeats).
    let lb = live.wait("a block loop", |e| e["type"] == "loop" && e["song"]["block"] == 1);
    assert_eq!(lb["song"]["block_loops"], 2, "{lb}");
    let bt = live.wait("a block tick", |e| e["type"] == "tick" && e["song"]["block"] == 1);
    assert!(bt["song"]["loop_in_block"].as_u64().unwrap() < 2, "{bt}");
    live.send(json!({ "type": "status" }));
    let rd = live.wait("ready", ty("ready"));
    assert_eq!((rd["song"]["current"].clone(), rd["song"]["from"].clone()), (json!(1), Value::Null));
    live.send(json!({ "type": "song", "action": "play_from", "block": 1 }));
    assert_eq!(live.wait("song mode", ty("song"))["mode"], "song");
    let lp = live.wait("a song loop", |e| e["type"] == "loop" && !e["song"].is_null());
    assert_eq!(lp["song"]["block"], 1, "starts at block 1");
    assert_eq!((lp["song"]["loop_in_block"].clone(), lp["song"]["block_loops"].clone()), (json!(0), json!(2)), "{lp}");
    live.send(json!({ "type": "status" }));
    assert_eq!(live.wait("ready", ty("ready"))["song"]["from"], 1);
    // Into the tail: one loop, after block 1's two.
    let tail = live.wait("the tail", |e| e["type"] == "loop" && e["song"]["tail"] == true);
    assert_eq!((tail["song"]["loop_in_block"].clone(), tail["song"]["block_loops"].clone()), (json!(0), json!(1)), "{tail}");
    live.wait("the song ends", ty("finished"));
    live.send(json!({ "type": "song", "action": "stop" }));
    assert_eq!(live.wait("loop mode", ty("song"))["mode"], "loop");
    live.wait("ticking again", ty("tick"));

    // Preview: two seconds rendered offline into .live/preview.wav.
    // One at a time: a second request while the first renders is refused (both would write the file).
    live.send(json!({ "type": "preview", "seconds": 2, "id": "p1" }));
    live.send(json!({ "type": "preview", "seconds": 2, "id": "p2" }));
    let busy = live.wait("busy", ty("error"));
    assert_eq!(busy["id"], "p2");
    let pv = live.wait("preview", ty("preview"));
    assert_eq!(pv["id"], "p1");
    assert!((pv["seconds"].as_f64().unwrap() - 2.0).abs() < 0.01, "{pv}");
    assert!(PathBuf::from(pv["path"].as_str().unwrap()).is_file());

    // Errors are events, not crashes.
    live.send(json!({ "type": "explode", "id": 42 }));
    let err = live.wait("error", ty("error"));
    assert_eq!(err["id"], 42);
    live.send(json!({ "type": "solo", "instrument": "nope", "on": true }));
    live.wait("error for an unknown instrument", ty("error"));

    // Quit: a clean stop.
    live.send(json!({ "type": "quit", "id": "q" }));
    assert_eq!(live.wait("stopped", ty("stopped"))["id"], "q");
    let status = live.child.wait().unwrap();
    assert!(status.success());
    let st: Value = serde_json::from_str(&std::fs::read_to_string(live.base.join("pieces/demo/.live/status.json")).unwrap()).unwrap();
    assert_eq!(st["state"], "stopped");
}

#[test]
fn ready_reports_the_limiter_and_the_instruments_in_document_order() {
    let mut live = Live::start(&["--no-limiter"]);
    let ready = live.wait("ready", ty("ready"));
    assert_eq!(ready["limiter"], false, "--no-limiter is in the first ready, headless too");
    // `zed` is written before `alpha`: the doc's order, not the id order.
    let two = doc(0.4).replace("instruments: { pad:", "instruments: { zed: { params: { level_db: -20 } }, alpha: { params: { level_db: -20 } }, pad:");
    live.send(json!({ "type": "doc", "text": two }));
    live.wait("applied", ty("applied"));
    live.send(json!({ "type": "status" }));
    assert_eq!(live.wait("ready", ty("ready"))["instruments"], json!(["zed", "alpha", "pad"]));
    live.send(json!({ "type": "quit" }));
    live.wait("stopped", ty("stopped"));
}

#[test]
fn the_history_records_where_each_revision_came_from() {
    let mut live = Live::start(&["--max-loops", "996"]);
    live.wait("ready", ty("ready"));
    let index = live.base.join("pieces/demo/.live/history/index.jsonl");
    let last_source = |index: &PathBuf| -> String {
        let text = std::fs::read_to_string(index).unwrap();
        let line: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        line["source"].as_str().unwrap().to_string()
    };
    assert_eq!(last_source(&index), "disk", "the starting doc came from the file");
    live.send(json!({ "type": "doc", "text": doc(0.3) }));
    live.wait("accepted", ty("accepted"));
    assert_eq!(last_source(&index), "editor", "a doc command without a source is the editor's");
    live.send(json!({ "type": "doc", "text": doc(0.2), "source": "revert" }));
    live.wait("accepted", ty("accepted"));
    assert_eq!(last_source(&index), "revert");
    live.send(json!({ "type": "doc", "text": doc(0.25), "source": "Not a label!" }));
    live.wait("accepted", ty("accepted"));
    assert_eq!(last_source(&index), "editor", "a malformed source falls back");
    std::fs::write(live.base.join("pieces/demo/demo.json5"), doc(0.35)).unwrap();
    live.wait("the disk save", ty("external_change"));
    assert_eq!(last_source(&index), "disk");
    live.send(json!({ "type": "quit" }));
    live.wait("stopped", ty("stopped"));
}

#[test]
fn a_preview_can_render_the_stems_beside_the_mix() {
    let mut live = Live::start(&["--max-loops", "995"]);
    live.wait("ready", ty("ready"));
    live.send(json!({ "type": "preview", "seconds": 2, "stems": true, "id": "s1" }));
    let pv = live.wait("preview", |e| e["type"] == "preview" || e["type"] == "error");
    assert_eq!((pv["type"].clone(), pv["id"].clone()), (json!("preview"), json!("s1")), "{pv}");
    assert!(PathBuf::from(pv["path"].as_str().unwrap()).is_file(), "the mix as well");
    let stems = &pv["stems"];
    let dir = PathBuf::from(stems["dir"].as_str().unwrap());
    assert_eq!((stems["files"][0]["id"].clone(), stems["files"][0]["kind"].clone(), stems["files"][0]["file"].clone()), (json!("pad"), json!("instrument"), json!("pad.wav")), "{pv}");
    let out = stems["files"][0]["out_lufs"].as_f64().unwrap();
    assert!((out + 18.0).abs() < 0.5 || stems["files"][0]["limited_by"] == "peak", "normalized to −18 LUFS (or peak-limited): {out}");
    assert!(dir.join("pad.wav").is_file() && dir.join("stems.json").is_file());
    let manifest: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("stems.json")).unwrap()).unwrap();
    assert_eq!(manifest["engine_build"], engines_core::build_id(), "the build that rendered it (ADR 0054 P10)");
    // A second preview rewrites the stems and leaves a file that isn't one of them alone.
    std::fs::write(dir.join("mine.txt"), "keep").unwrap();
    live.send(json!({ "type": "preview", "seconds": 1, "stems": true }));
    let again = live.wait("preview", ty("preview"));
    assert!((again["seconds"].as_f64().unwrap() - 1.0).abs() < 0.01);
    assert!(dir.join("mine.txt").is_file() && dir.join("pad.wav").is_file());
    assert!(!live.base.join("pieces/demo/.live/.stems-tmp").exists(), "the float temps are gone");
    // A mix-only preview has no stems.
    live.send(json!({ "type": "preview", "seconds": 1 }));
    assert!(live.wait("preview", ty("preview"))["stems"].is_null());
    live.send(json!({ "type": "quit" }));
    live.wait("stopped", ty("stopped"));
}

#[test]
fn a_block_can_switch_the_orrery_off_and_the_panel_toggle_beats_it() {
    // ADR 0055: the Orrery is on for the piece and off in block 1; the song structure says so, the
    // loops report what plays, and the panel's hold leaves the block's Orrery alone while its Orrery
    // switch wins for the rest of the block.
    let text = doc(0.4)
        .replace("song: {", "movement: { orrery: true },\n  song: {")
        .replace("{ matrix: \"thin\", loops: 2, label: \"thin\" }", "{ loops: 40, label: \"calm\", control: { movement: { orrery: false } } }");
    let mut live = Live::start_with(&text, &[]);
    let ready = live.wait("ready", ty("ready"));
    let blocks = &ready["song"]["blocks"];
    assert_eq!((blocks[0].get("orrery"), blocks[1]["orrery"].clone()), (None, json!(false)), "{blocks}");
    live.send(json!({ "type": "song", "action": "play_from", "block": 0 }));
    let first = live.wait("a loop of block 0", |e| e["type"] == "loop" && e["song"]["block"] == 0);
    assert_eq!(first["orrery"], true, "{first}");
    let calm = live.wait("a loop of block 1", |e| e["type"] == "loop" && e["song"]["block"] == 1);
    assert_eq!(calm["orrery"], false, "{calm}");
    live.send(json!({ "type": "control", "optimize": "hold" }));
    let ev = live.wait("control", ty("control"));
    assert_eq!((ev["optimize"].clone(), ev["orrery"].clone()), (json!("hold"), json!(false)), "the status keeps the block's Orrery: {ev}");
    let held = live.wait("a held loop", |e| e["type"] == "loop" && e["optimize"] == "hold");
    assert_eq!((held["song"]["block"].clone(), held["orrery"].clone()), (json!(1), json!(false)), "{held}");
    live.send(json!({ "type": "control", "orrery": true }));
    assert_eq!(live.wait("control", ty("control"))["orrery"], true);
    let on = live.wait("the Orrery back on", |e| e["type"] == "loop" && e["orrery"] == true);
    assert_eq!(on["song"]["block"], 1, "the toggle wins inside the block: {on}");
    live.send(json!({ "type": "quit" }));
    live.wait("stopped", ty("stopped"));
}

#[test]
fn song_audition_can_start_from_the_command_line() {
    let mut live = Live::start(&["--loop-block", "1"]);
    let ready = live.wait("ready", ty("ready"));
    assert_eq!((ready["song"]["mode"].clone(), ready["song"]["block"].clone()), (json!("block"), json!(1)));
    live.send(json!({ "type": "quit" }));
    live.wait("stopped", ty("stopped"));
}

#[test]
fn a_revision_that_cannot_be_recorded_is_refused() {
    let mut live = Live::start(&["--max-loops", "1000"]);
    live.wait("ready", ty("ready"));
    // The history can't be written any more (a full disk, say): `history` is now a file.
    let history = live.base.join("pieces/demo/.live/history");
    std::fs::remove_dir_all(&history).unwrap();
    std::fs::write(&history, "not a directory").unwrap();
    live.send(json!({ "type": "doc", "text": doc(0.2), "version": 3, "id": "h" }));
    let rej = live.wait("rejected", |e| e["type"] == "rejected" || e["type"] == "accepted");
    assert_eq!(rej["type"], "rejected", "an unrecorded revision never plays: {rej}");
    assert_eq!((rej["errors"][0]["code"].clone(), rej["id"].clone(), rej["version"].clone()), (json!("history"), json!("h"), json!(3)));
    live.send(json!({ "type": "status" }));
    assert_eq!(live.wait("ready", ty("ready"))["rev"], 1, "still on the recorded revision");
    live.send(json!({ "type": "quit" }));
    live.wait("stopped", ty("stopped"));
}

#[test]
fn a_save_the_editor_already_sent_is_heard_once() {
    let mut live = Live::start(&["--max-loops", "999"]); // (the args name its temp dir: unique per test)
    live.wait("ready", ty("ready"));
    let path = live.base.join("pieces/demo/demo.json5");
    // The editor sends the doc as it saves it: the command is what's heard, and the save on disk
    // isn't news — a refused text isn't refused a second time from the file.
    let broken = doc(0.3).replace("cutoff", "cutof");
    live.send(json!({ "type": "doc", "text": broken, "version": 2 }));
    assert_eq!(live.wait("rejected", ty("rejected"))["source"], "command");
    std::fs::write(&path, &broken).unwrap();
    std::thread::sleep(Duration::from_millis(900)); // > the 300 ms quiet (polled every 5 ms with --fast)
    // A save the editor didn't send (Claude's) is heard from the file.
    std::fs::write(&path, doc(0.25)).unwrap();
    let next = live.wait("the disk save", |e| e["type"] == "external_change" || e["type"] == "rejected");
    assert_eq!(next["type"], "external_change", "the broken text was refused once, as a command: {next}");

    // The editor says when its buffer holds unsaved edits; status.json tells Claude (whose edit
    // to the file would collide with them).
    let status = live.base.join("pieces/demo/.live/status.json");
    let editor = |status: &PathBuf| -> Value {
        let st: Value = serde_json::from_str(&std::fs::read_to_string(status).unwrap_or_default()).unwrap_or(Value::Null);
        st["editor"].clone()
    };
    assert!(editor(&status).is_null(), "no editor has spoken yet");
    for unsaved in [true, false] {
        live.send(json!({ "type": "editor", "unsaved": unsaved }));
        let t0 = Instant::now();
        while editor(&status)["unsaved"] != unsaved {
            assert!(t0.elapsed() < Duration::from_secs(5), "status.json never said unsaved: {unsaved}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    live.send(json!({ "type": "quit" }));
    live.wait("stopped", ty("stopped"));
}

#[test]
fn mute_and_solo_follow_what_is_playing_not_a_waiting_edit() {
    let mut live = Live::start(&["--max-loops", "997"]);
    live.wait("ready", ty("ready"));
    live.send(json!({ "type": "song", "action": "play_from", "block": 0 }));
    assert_eq!(live.wait("song mode", ty("song"))["mode"], "song");
    // While the song plays, an edit that renames `pad` is kept but waits (ADR 0029): `pad` is still
    // the instrument sounding, so it can be muted; `pad2` isn't playing yet.
    live.send(json!({ "type": "doc", "text": doc(0.4).replace("pad", "pad2") }));
    live.wait("accepted", ty("accepted"));
    live.send(json!({ "type": "mute", "instrument": "pad", "on": true, "id": "m1" }));
    let ev = live.wait("an answer", |e| e["id"] == "m1");
    assert_eq!((ev["type"].clone(), ev["muted"].clone()), (json!("gains"), json!(["pad"])), "{ev}");
    live.send(json!({ "type": "solo", "instrument": "pad2", "on": true, "id": "s1" }));
    assert_eq!(live.wait("an answer", |e| e["id"] == "s1")["type"], "error");
    live.send(json!({ "type": "quit" }));
    live.wait("stopped", ty("stopped"));
}

#[test]
fn an_edit_in_block_mode_re_materializes_the_block() {
    let mut live = Live::start(&["--loop-block", "1"]);
    live.wait("ready", ty("ready"));
    // Block 1 ("thin") drops the pad's notes, so it's silent: an edit re-materializes the block
    // rather than falling back to the doc's own matrix (where the pad plays).
    live.send(json!({ "type": "doc", "text": doc(0.3), "id": "d" }));
    assert_eq!(live.wait("applied r2", ty("applied"))["rev"], 2);
    live.send(json!({ "type": "preview", "seconds": 2 }));
    let pv = live.wait("preview", ty("preview"));
    assert!(pv["peak_db"].as_f64().unwrap() <= -119.0, "still block 1: {pv}");

    // An edit that removes the song leaves block mode — and says so.
    let no_song = doc(0.3).lines().filter(|l| !l.trim_start().starts_with("song:")).collect::<Vec<_>>().join("\n");
    live.send(json!({ "type": "doc", "text": no_song, "id": "n" }));
    let s = live.wait("song", ty("song"));
    assert_eq!((s["mode"].clone(), s["block"].clone(), s["id"].clone()), (json!("loop"), Value::Null, json!("n")));
    live.wait("applied r3", |e| e["type"] == "applied" && e["rev"] == 3);
    live.send(json!({ "type": "preview", "seconds": 2 }));
    let pv = live.wait("preview", ty("preview"));
    assert!(pv["peak_db"].as_f64().unwrap() > -60.0, "the loop plays the pad: {pv}");
    live.send(json!({ "type": "status" }));
    assert!(live.wait("ready", ty("ready"))["song"].is_null());
    live.send(json!({ "type": "quit" }));
    live.wait("stopped", ty("stopped"));
}

#[test]
fn play_stop_restart_and_the_panel_toggles() {
    let mut live = Live::start(&[]);
    live.wait("ready", ty("ready"));
    live.send(json!({ "type": "stop", "id": "s" }));
    assert_eq!(live.wait("stopped", ty("stopped"))["id"], "s");
    // While stopped, `control` answers with the mode now requested — the next `play` starts under it.
    live.send(json!({ "type": "control", "optimize": "hold", "id": "c" }));
    let c = live.wait("control", ty("control"));
    assert_eq!((c["optimize"].clone(), c["id"].clone()), (json!("hold"), json!("c")));
    live.send(json!({ "type": "status" }));
    assert_eq!(live.wait("ready", ty("ready"))["playing"], false);
    live.send(json!({ "type": "play", "id": "p" }));
    let p = live.wait("playing", ty("playing"));
    assert_eq!((p["mode"].clone(), p["id"].clone()), (json!("loop"), json!("p")));
    live.wait("a hold loop", |e| e["type"] == "loop" && e["optimize"] == "hold");
    // `play` while playing is a no-op: nothing answers it before the `status` sent after it.
    live.send(json!({ "type": "play", "id": "p2" }));
    live.send(json!({ "type": "status", "id": "st" }));
    let next = live.wait("an answer", |e| e["id"] == "p2" || e["id"] == "st");
    assert_eq!((next["type"].clone(), next["playing"].clone(), next["optimize"].clone()), (json!("ready"), json!(true), json!("hold")));
    live.send(json!({ "type": "restart", "id": "r" }));
    assert_eq!(live.wait("playing", ty("playing"))["id"], "r");
    live.wait("ticking", ty("tick"));

    live.send(json!({ "type": "solo", "instrument": "pad", "on": true }));
    assert_eq!(live.wait("gains", ty("gains"))["soloed"], json!(["pad"]));
    live.send(json!({ "type": "clear_solo", "id": "cs" }));
    let g = live.wait("gains", ty("gains"));
    assert_eq!((g["soloed"].clone(), g["id"].clone()), (json!([]), json!("cs")));
    live.send(json!({ "type": "limiter", "on": false, "id": "l" }));
    let l = live.wait("limiter", ty("limiter"));
    assert_eq!((l["on"].clone(), l["id"].clone()), (json!(false), json!("l")));
    assert!(live.wait("a tick", ty("tick"))["gr_db"].is_null(), "no limiter headless");
    live.send(json!({ "type": "quit" }));
    live.wait("stopped", ty("stopped"));
}

#[test]
fn a_doc_kept_for_the_next_play_never_claims_a_later_applied() {
    let mut live = Live::start(&[]);
    live.wait("ready", ty("ready"));
    live.send(json!({ "type": "song", "action": "play_from", "block": 0 }));
    assert_eq!(live.wait("song mode", ty("song"))["mode"], "song");
    // Kept while the song plays (ADR 0029); `song stop` starts a session on it — no `applied` of its own.
    live.send(json!({ "type": "doc", "text": doc(0.3), "id": "dX" }));
    assert_eq!(live.wait("accepted", ty("accepted"))["rev"], 2);
    live.send(json!({ "type": "song", "action": "stop" }));
    assert_eq!(live.wait("loop mode", ty("song"))["mode"], "loop");
    // Hold re-applies r2: that `applied` answers nothing the host asked with "dX".
    live.send(json!({ "type": "control", "optimize": "hold", "id": "cH" }));
    let a = live.wait("the re-apply", |e| e["type"] == "applied" && e["rev"] == 2);
    assert!(a["id"].is_null(), "a stale id: {a}");
    live.send(json!({ "type": "quit" }));
    live.wait("stopped", ty("stopped"));
}

#[test]
fn disk_saves_and_tempo_edits_keep_the_host_current() {
    let mut live = Live::start(&[]);
    live.wait("ready", ty("ready"));
    let path = live.base.join("pieces/demo/demo.json5");
    // A refused save put back (Claude restoring the good text) clears the refusal — the host hears it.
    std::fs::write(&path, doc(0.4).replace("cutoff", "cutof")).unwrap();
    assert_eq!(live.wait("rejected", ty("rejected"))["source"], "disk");
    std::fs::write(&path, doc(0.4)).unwrap();
    let back = live.wait("an answer", |e| e["type"] == "unchanged" || e["type"] == "accepted");
    assert_eq!((back["type"].clone(), back["rev"].clone(), back["source"].clone()), (json!("unchanged"), json!(1), json!("disk")));

    // Half the tempo: `ready.loop_s` follows (as `tick.loop_s` does).
    live.send(json!({ "type": "doc", "text": doc(0.4).replace("bpm: 240", "bpm: 120") }));
    live.wait("applied r2", |e| e["type"] == "applied" && e["rev"] == 2);
    live.send(json!({ "type": "status" }));
    assert_eq!(live.wait("ready", ty("ready"))["loop_s"], 2.0);

    // A line that isn't UTF-8 is an error, not the end of the session.
    live.stdin.write_all(b"{\"type\": \"\xff\xfe\"}\n").unwrap();
    live.stdin.flush().unwrap();
    assert!(live.wait("error", ty("error"))["message"].as_str().unwrap().contains("UTF-8"));
    live.send(json!({ "type": "status", "id": "still" }));
    assert_eq!(live.wait("ready", ty("ready"))["id"], "still");
    live.send(json!({ "type": "quit" }));
    live.wait("stopped", ty("stopped"));
}

/// Run `engines-live demo --headless --fast --max-loops 1 <args>` to its end.
fn cli(args: &[&str]) -> Output {
    let base = piece_dir(&["cli"]);
    let out = Command::new(env!("CARGO_BIN_EXE_engines-live"))
        .current_dir(&base)
        .args(["demo", "--headless", "--fast", "--max-loops", "1"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&base);
    out
}

#[test]
fn the_command_line_refuses_a_block_or_a_mode_that_does_not_exist() {
    for (args, says) in [
        (&["--loop-block", "7"], "`--loop-block` needs a \"block\" in 0..2 (the doc has 2 blocks)"),
        (&["--play-from", "2"], "`--play-from` needs a \"block\" in 0..2"),
        // Not quietly dropped (with the doc's own control): refused.
        (&["--optimize", "bogus"], "bogus"),
    ] {
        let out = cli(args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains(says), "{args:?}: {stderr}");
    }
    let ok = cli(&["--play-from", "1"]);
    assert!(ok.status.success(), "{}", String::from_utf8_lossy(&ok.stderr));
}

#[test]
fn route_sources_needs_a_device_and_reports_the_routing_in_ready() {
    // No multichannel device and not headless: refused before any audio device is opened.
    let base = piece_dir(&["route-cli"]);
    let out = Command::new(env!("CARGO_BIN_EXE_engines-live")).current_dir(&base).args(["demo", "--route-sources"]).stdin(Stdio::null()).output().unwrap();
    let _ = std::fs::remove_dir_all(&base);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("--route-sources needs a multichannel --output-device"), "{stderr}");

    // Headless, no routing map yet: 16 channels, the doc's sources from 1/2 (its one source, the
    // pad), the overflow on the next pair.
    let mut live = Live::start(&["--route-sources"]);
    let ready = live.wait("ready", ty("ready"));
    let r = &ready["routing"];
    assert_eq!((r["channels"].clone(), r["overflow"].clone(), r["map"].clone()), (json!(16), json!("3/4"), Value::Null), "{r}");
    assert_eq!(r["pairs"], json!([{ "pair": "1/2", "source": "inst:pad", "name": "pad" }]));
    live.wait("a tick", ty("tick"));
}

#[test]
fn output_switches_need_a_device_and_the_devices_can_be_listed() {
    // Headless has no device to switch: an error carrying the command's id, and it keeps playing.
    let mut live = Live::start(&[]);
    live.wait("ready", ty("ready"));
    live.send(json!({ "type": "output", "device": "Headphones", "route_sources": false, "id": 7 }));
    let err = live.wait("the refusal", ty("error"));
    assert_eq!(err["id"], 7);
    assert!(err["message"].as_str().unwrap().contains("headless player has no device"), "{err}");
    live.wait("a tick", ty("tick"));

    // The picker's list: a JSON array of {name, channels, sample_rate, default}.
    let out = Command::new(env!("CARGO_BIN_EXE_engines-live")).args(["--list-devices", "--json"]).output().unwrap();
    assert!(out.status.success());
    let list: Value = serde_json::from_slice(&out.stdout).unwrap();
    for d in list.as_array().unwrap() {
        assert!(d["name"].is_string() && d["channels"].is_u64() && d["sample_rate"].is_f64() && d["default"].is_boolean(), "{d}");
    }
}
