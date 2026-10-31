//! Golden-hash render tests — the safety net for the composition re-scope (ADR 0031).
//!
//! The run-vs-run determinism tests catch *nondeterminism*, but not a refactor that changes the
//! output *consistently*. These pin an FNV-1a hash of the rendered audio for a handful of fixtures
//! and paths (engine-direct, Session hold / drift / drift+Orrery, stems, a shortened song), so the
//! learner removal (P1a) and the block-render refactor (P4) can prove they leave every render
//! bit-identical.
//!
//! An INTENDED output change (e.g. P1b's reflecting drift) re-blesses with
//!     ENGINES_BLESS=1 cargo test -p engines_core --test golden
//! and the commit says which cases moved and why.
//!
//! The hashes are blessed on macOS and pinned there (CONTRIBUTING.md): `sin`/`exp`/`tanh` come from each
//! OS's own math library, so every case hashes differently on Linux (same sample counts). Off macOS
//! the golden check shows as ignored; `make test-mac` runs it and reports the `mac-suite` status.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use engines_core::contract::SessionControl;
use engines_core::controller::DitherConfig;
use engines_core::engine::{Engine, PhaseSnapshot};
use engines_core::loop_doc::LoopDoc;
use engines_core::session::{Session, SessionConfig};

/// Low SR keeps the debug-build renders quick; the hashes pin behavior, not fidelity.
const SR: f64 = 8_000.0;
const GOLDEN_FILE: &str = "tests/golden_hashes.json";

fn repo_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel)
}

fn load(rel: &str) -> LoopDoc {
    let src = std::fs::read_to_string(repo_path(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"));
    LoopDoc::from_json_str(&src).unwrap_or_else(|e| panic!("parse {rel}: {e}"))
}

/// FNV-1a over the little-endian bit patterns of every sample, plus the sample count.
fn hash_audio(audio: &[f32]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for s in audio {
        for b in s.to_bits().to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x00000100000001B3);
        }
    }
    format!("{h:016x}:{}", audio.len())
}

fn engine_render(doc: &LoopDoc, loops: usize) -> Vec<f32> {
    engine_render_with(doc, loops, Engine::new(SR))
}

fn engine_render_with(doc: &LoopDoc, loops: usize, mut eng: Engine) -> Vec<f32> {
    let mut snap = PhaseSnapshot::empty();
    let mut audio = Vec::new();
    for _ in 0..loops {
        let r = eng.render(doc, &snap);
        audio.extend_from_slice(&r.audio);
        snap = r.next_snapshot;
    }
    audio
}

fn session_render(doc: LoopDoc, cfg: SessionConfig, control: Option<SessionControl>, seed: u64) -> Vec<f32> {
    let dither = DitherConfig { seed, ..Default::default() };
    let mut s = Session::start(doc, SessionConfig { sample_rate: SR, ..cfg }, dither, control);
    let mut audio = Vec::new();
    let mut buf = [0.0f32; 8192];
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        let n = s.pull_audio(&mut buf);
        if n > 0 {
            audio.extend_from_slice(&buf[..n]);
        } else if s.finished() {
            break;
        } else {
            assert!(Instant::now() < deadline, "timed out draining session");
            thread::sleep(Duration::from_millis(1));
        }
    }
    audio
}

fn control(optimize: &str, orrery: bool) -> Option<SessionControl> {
    SessionControl::from_flags(optimize, orrery, &[], &[], 0.05).expect("control flags")
}

const TEMPLATE: &str = "lab/tests/fixtures/loop_contract_v0.9.json";
// The neutral test fixtures: a house-style drone and a short song, not a published piece.
const DRONE: &str = "lab/tests/fixtures/loop_drone_d_lydian.json5";
const SONG: &str = "lab/tests/fixtures/song_lydian_tide.json5";
const BINAURAL: &str = "lab/tests/fixtures/loop_binaural_basic.json";

/// Every case → its audio. Names are stable keys in `golden_hashes.json`.
fn cases() -> Vec<(&'static str, Vec<f32>)> {
    let mut out = Vec::new();

    out.push(("engine/template x3", engine_render(&load(TEMPLATE), 3)));
    out.push(("engine/drone x2", engine_render(&load(DRONE), 2)));

    let bounded = |n| SessionConfig { max_loops: Some(n), ..Default::default() };
    out.push(("session/hold/drone x2", session_render(load(DRONE), bounded(2), control("hold", false), 11)));
    out.push(("session/drift/template x3", session_render(load(TEMPLATE), bounded(3), None, 7)));
    out.push(("session/drift+orrery/drone x3", session_render(load(DRONE), bounded(3), control("drift", true), 7)));
    out.push((
        "session/stems/drone x1",
        session_render(load(DRONE), SessionConfig { max_loops: Some(1), source_stems: true, ..Default::default() }, control("hold", false), 11),
    ));
    // ADR 0047: the hard-L/R pair outside the master width — on the master and as a gain-only stem.
    out.push(("engine/binaural x2", engine_render(&load(BINAURAL), 2)));
    out.push((
        "session/stems/binaural x1",
        session_render(load(BINAURAL), SessionConfig { max_loops: Some(1), source_stems: true, ..Default::default() }, control("hold", false), 11),
    ));

    // A whole song, shortened to one loop per block (+ a one-loop tail) so seams, block resets and
    // the tail all run in a few seconds of audio.
    let mut song = load(SONG);
    let spec = song.song.as_mut().expect("song fixture has a song");
    for b in &mut spec.sequence {
        b.loops = 1;
    }
    spec.tail_loops = 1;
    spec.transition_s = 2.0;
    out.push(("song/drift+orrery/lydian_tide 1-per-block", session_render(song, SessionConfig::default(), control("drift", true), 7)));

    out
}

#[test]
#[cfg_attr(not(target_os = "macos"), ignore = "golden hashes are blessed on macOS; run `make test-mac` (CONTRIBUTING.md)")]
fn renders_match_golden_hashes() {
    let got: BTreeMap<String, String> = cases().into_iter().map(|(k, a)| (k.to_string(), hash_audio(&a))).collect();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(GOLDEN_FILE);

    if std::env::var("ENGINES_BLESS").is_ok_and(|v| v == "1") {
        let json = serde_json::to_string_pretty(&got).unwrap();
        std::fs::write(&path, json + "\n").unwrap();
        eprintln!("blessed {} golden hashes → {}", got.len(), path.display());
        return;
    }

    let want: BTreeMap<String, String> = serde_json::from_str(
        &std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("{GOLDEN_FILE} missing — run with ENGINES_BLESS=1 to create it")),
    )
    .expect("golden file parses");
    let mut diffs = Vec::new();
    for (k, g) in &got {
        match want.get(k) {
            Some(w) if w == g => {}
            Some(w) => diffs.push(format!("  {k}: golden {w}, got {g}")),
            None => diffs.push(format!("  {k}: no golden (new case?) — got {g}")),
        }
    }
    for k in want.keys().filter(|k| !got.contains_key(*k)) {
        diffs.push(format!("  {k}: golden exists but the case is gone"));
    }
    assert!(diffs.is_empty(), "render output changed:\n{}\n(intended? re-bless with ENGINES_BLESS=1 and explain in the commit)", diffs.join("\n"));
}

/// What the goldens pin that holds on every OS: the same cases, the same sample counts,
/// and cases that hash alike in the goldens (engine-direct and Session hold) still hash alike. On
/// macOS the full hash check above covers all of it.
#[test]
#[cfg_attr(target_os = "macos", ignore = "renders_match_golden_hashes checks the full hashes on macOS")]
fn renders_keep_the_golden_cases_lengths_and_parity() {
    let got: BTreeMap<String, String> = cases().into_iter().map(|(k, a)| (k.to_string(), hash_audio(&a))).collect();
    let want: BTreeMap<String, String> =
        serde_json::from_str(&std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(GOLDEN_FILE)).unwrap()).unwrap();
    assert_eq!(got.keys().collect::<Vec<_>>(), want.keys().collect::<Vec<_>>(), "the golden cases changed");
    let len = |h: &str| h.rsplit(':').next().unwrap_or_default().to_string();
    for (k, w) in &want {
        assert_eq!(len(&got[k]), len(w), "{k}: the sample count moved");
    }
    for (a, wa) in &want {
        for (b, wb) in want.range::<String, _>((std::ops::Bound::Excluded(a.clone()), std::ops::Bound::Unbounded)) {
            if wa == wb {
                assert_eq!(got[a], got[b], "{a} and {b} hash alike in the goldens but not here");
            }
        }
    }
}

/// ADR 0048: mirroring every instrument as MIDI reads voice state only — the engine cases hash the
/// same with the mirror on as off. On macOS the reference is their golden; elsewhere (the goldens are
/// pinned to macOS) it's the mirror-off render on the same machine.
#[test]
fn the_note_tap_leaves_the_engine_goldens_unchanged() {
    let goldens: BTreeMap<String, String> =
        serde_json::from_str(&std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(GOLDEN_FILE)).unwrap()).unwrap();
    for (key, rel, loops) in [("engine/template x3", TEMPLATE, 3), ("engine/drone x2", DRONE, 2), ("engine/binaural x2", BINAURAL, 2)] {
        let doc = load(rel);
        let want = if cfg!(target_os = "macos") { goldens[key].clone() } else { hash_audio(&engine_render(&doc, loops)) };
        let mut eng = Engine::new(SR);
        eng.set_note_tap(doc.instruments.keys().cloned().collect());
        assert_eq!(hash_audio(&engine_render_with(&doc, loops, eng)), want, "{key} moved with the mirror on");
    }
}
