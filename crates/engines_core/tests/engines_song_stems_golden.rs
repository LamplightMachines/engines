//! `engines-song --stems` pinned byte for byte (ADR 0030; ADR 0040 moved the stem code into
//! `engines_core::stems` for the live player's previews). A short render of a two-instrument doc with a
//! reverb bus: every stem file's hash and every gain in `stems.json` must stay exactly as they were.
//! An intended change re-blesses: `ENGINES_BLESS=1 cargo test -p engines_core --test engines_song_stems_golden`
//! prints the new table to paste below (and the commit says why). Like the render goldens, the
//! table is blessed on macOS and pinned there (CONTRIBUTING.md); off macOS the test shows as ignored.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A 2 s loop (120 bpm × 4 beats): a pad and a sub, the pad sent to a reverb bus.
const DOC: &str = r#"{
  schema_version: "0.9",
  transport: { bpm: 120, master_steps: 4 },
  harmony: { root: "D", mode: "dorian" },
  global: { master_gain_db: -6.02 },
  instruments: {
    pad: { note_engine: { polyphony: 2 }, params: { wave: "saw_soft", level_db: -7.96, cutoff_hz: 800, sustain: 1.0 }, sends_db: { rv: -6.02 } },
    sub: { note_engine: { polyphony: 1 }, params: { wave: "sine", level_db: -10.46, sustain: 1.0 } },
  },
  modulators: {
    seq: { type: "step_seq", output: "note", length: 4, steps: [ { step: 0, voicing: { v1: { offset: 0 }, v2: { offset: 7 } } } ] },
    low: { type: "step_seq", output: "note", length: 4, steps: [ { step: 0, voicing: { b: { offset: -24 } } } ] },
  },
  matrix: {
    m: { source: "seq", dest: "instruments.pad.notes", kind: "note", voices: 2 },
    b: { source: "low", dest: "instruments.sub.notes", kind: "note", voices: 1 },
  },
  buses: { rv: { return_gain_db: -7.96, effects: { verb: { type: "reverb", params: { room: 0.6, damping: 0.4, width: 0.8 } } } } },
}"#;

/// What the render must reproduce: (file, FNV-1a of its bytes, gain_db's bits).
const GOLDEN: &[(&str, &str, &str)] = &[
    ("bus_rv.wav", "4f2a4cfd0f00f02a:1728080", "402a52cb2f171508"),
    ("pad.wav", "55ea4583b9524e82:1728080", "c0162d3294a5dabc"),
    ("sub.wav", "24abc662fcdfdfb3:1728080", "3ffaae62fb650180"),
];

fn fnv(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x00000100000001B3);
    }
    format!("{h:016x}:{}", bytes.len())
}

fn scratch() -> PathBuf {
    let b = std::env::temp_dir().join(format!("engines-song-stems-golden-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&b);
    std::fs::create_dir_all(&b).unwrap();
    std::fs::write(b.join("doc.json5"), DOC).unwrap();
    b
}

fn render(base: &Path) -> Vec<(String, String, String)> {
    let out = Command::new(env!("CARGO_BIN_EXE_engines-song"))
        .current_dir(base)
        .args(["--doc", "doc.json5", "--stems", "stems", "--container", "wav", "--format", "s24", "--max-loops", "3", "--optimize", "drift", "--seed", "7"])
        .output()
        .unwrap();
    assert!(out.status.success(), "engines-song failed:\n{}", String::from_utf8_lossy(&out.stderr));
    let manifest: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(base.join("stems/stems.json")).unwrap()).unwrap();
    let mut rows: Vec<(String, String, String)> = manifest["stems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            let file = s["file"].as_str().unwrap().to_string();
            let hash = fnv(&std::fs::read(base.join("stems").join(&file)).unwrap());
            let gain = format!("{:016x}", s["gain_db"].as_f64().unwrap().to_bits());
            (file, hash, gain)
        })
        .collect();
    rows.sort();
    rows
}

#[test]
#[cfg_attr(not(target_os = "macos"), ignore = "stem hashes are blessed on macOS; run `make test-mac` (CONTRIBUTING.md)")]
fn stems_render_byte_for_byte_as_blessed() {
    let base = scratch();
    let rows = render(&base);
    let _ = std::fs::remove_dir_all(&base);
    assert_eq!(rows.len(), 3, "pad, sub and the reverb bus: {rows:?}");
    if std::env::var("ENGINES_BLESS").is_ok_and(|v| v == "1") {
        for (f, h, g) in &rows {
            println!("    (\"{f}\", \"{h}\", \"{g}\"),");
        }
        return;
    }
    let want: Vec<(String, String, String)> = GOLDEN.iter().map(|(f, h, g)| (f.to_string(), h.to_string(), g.to_string())).collect();
    assert_eq!(rows, want, "engines-song --stems output changed (intended? re-bless with ENGINES_BLESS=1 and say why in the commit)");
}
