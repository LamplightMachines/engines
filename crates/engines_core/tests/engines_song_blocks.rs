//! A song is written one file per block (ADR 0043), over the process boundary: the mix and every
//! stem, cut sample-exactly — the block files laid end to end (one after another on a DAW track)
//! must decode to exactly the samples a `--whole` render of the same song holds. Nothing may
//! click, gap, overlap or jump in level at a join.

use std::path::{Path, PathBuf};
use std::process::Command;

use engines_core::wav::WavReader;

/// A 2 s loop (120 bpm × 4 beats): a pad and a sub, the pad sent to a reverb bus. The song drops
/// the sub in its first and last blocks, so the sub's stem is silent in those block files.
const DOC: &str = r#"{
  schema_version: "0.9",
  transport: { bpm: 120, master_steps: 4 },
  harmony: { root: "D", mode: "dorian" },
  global: { master_gain_db: -6.02 },
  instruments: {
    pad: { note_engine: { polyphony: 2 }, params: { wave: "saw_soft", level_db: -7.96, cutoff_hz: 800, sustain: 1.0, release_s: 1.2 }, sends_db: { rv: -6.02 } },
    sub: { note_engine: { polyphony: 1 }, params: { wave: "sine", level_db: -10.46, sustain: 1.0, release_s: 0.8 } },
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
  song: {
    matrices: { nosub: { b: null } },
    sequence: [
      { matrix: "nosub", loops: 2, label: "open" },
      { loops: 2, label: "full" },
      { matrix: "nosub", loops: 1, label: "thin air" },
    ],
    transition_s: 0.5,
    tail_loops: 1,
  },
}"#;

const SR: u64 = 24_000;
/// Frames per 2 s loop at [`SR`].
const LOOP: u64 = 2 * SR;

fn scratch(tag: &str) -> PathBuf {
    let b = std::env::temp_dir().join(format!("engines-song-blocks-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&b);
    std::fs::create_dir_all(&b).unwrap();
    std::fs::write(b.join("night.json5"), DOC).unwrap();
    b
}

fn engines_song(cwd: &Path, args: &[&str]) -> String {
    let sr = SR.to_string();
    let out = Command::new(env!("CARGO_BIN_EXE_engines-song"))
        .current_dir(cwd)
        .args(["--doc", "night.json5", "--seed", "7", "--optimize", "drift", "--sample-rate", &sr])
        .args(args)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "engines-song {args:?} failed:\n{err}");
    err
}

/// Every sample of a WAV, as f32 (integer formats come back as their exact scaled values).
fn wav(path: &Path) -> Vec<f32> {
    let mut rd = WavReader::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut out = vec![0.0f32; rd.frames() as usize * rd.channels() as usize];
    let n = rd.read_interleaved(&mut out).unwrap();
    out.truncate(n);
    out
}

fn flac(path: &Path) -> Vec<i32> {
    let mut rd = claxon::FlacReader::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    rd.samples().collect::<Result<_, _>>().unwrap()
}

fn json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn listing(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

/// The file names a manifest's `blocks` list, in order.
fn block_files(manifest: &serde_json::Value) -> Vec<String> {
    manifest["blocks"].as_array().unwrap().iter().map(|b| b["file"].as_str().unwrap().to_string()).collect()
}

const MIX_FILES: [&str; 4] = ["night-mix-00-open", "night-mix-01-full", "night-mix-02-thin_air", "night-mix-03-tail"];

#[test]
fn the_mix_is_a_file_per_block_that_joins_back_bit_for_bit() {
    let b = scratch("mix");
    // The plan names every file before anything renders.
    let err = engines_song(&b, &["--out", "night.wav", "--dry-run"]);
    assert!(MIX_FILES.iter().all(|f| err.contains(&format!("{f}.wav"))), "{err}");
    assert!(!b.join("night").exists(), "a dry run writes nothing");

    // s16, so the dither has to carry across every join for the files to match the whole render.
    let err = engines_song(&b, &["--out", "night.wav", "--format", "s16"]);
    assert!(err.contains("(4 files)"), "{err}");
    let dir = b.join("night");
    let want: Vec<String> = MIX_FILES.iter().map(|f| format!("{f}.wav")).chain(["blocks.json".into()]).collect::<Vec<_>>();
    let mut want_sorted = want.clone();
    want_sorted.sort();
    assert_eq!(listing(&dir), want_sorted);
    let manifest = json(&dir.join("blocks.json"));
    assert_eq!(block_files(&manifest), &want[..4]);
    assert_eq!(manifest["engine_build"], engines_core::build_id(), "the build that rendered it (ADR 0054 P10)");
    let starts: Vec<u64> = manifest["blocks"].as_array().unwrap().iter().map(|b| b["start_frame"].as_u64().unwrap()).collect();
    assert_eq!(starts, [0, 2 * LOOP, 4 * LOOP, 5 * LOOP], "cuts on the block starts");
    assert_eq!(manifest["blocks"][3]["tail"], true);

    engines_song(&b, &["--out", "whole.wav", "--format", "s16", "--whole"]);
    let whole = wav(&b.join("whole.wav"));
    assert_eq!(whole.len() as u64, 6 * LOOP * 2);
    let parts: Vec<Vec<f32>> = want[..4].iter().map(|f| wav(&dir.join(f))).collect();
    assert_eq!(parts.iter().map(|p| p.len() as u64 / 2).collect::<Vec<_>>(), [2 * LOOP, 2 * LOOP, LOOP, LOOP]);
    assert!(parts.concat() == whole, "the block files, end to end, are the whole song sample for sample");

    // FLAC (24-bit) splits the same way.
    engines_song(&b, &["--out", "night.flac"]);
    engines_song(&b, &["--out", "whole.flac", "--whole"]);
    let joined: Vec<i32> = MIX_FILES.iter().flat_map(|f| flac(&dir.join(format!("{f}.flac")))).collect();
    assert_eq!(joined, flac(&b.join("whole.flac")));
    // That re-render's manifest replaced the WAV run's, and the WAV files it recorded went with it.
    assert!(listing(&dir).iter().all(|f| !f.ends_with(".wav")), "{:?}", listing(&dir));

    // A render cut short: only the blocks it reached, the last one short.
    engines_song(&b, &["--out", "night.flac", "--max-loops", "3"]);
    let manifest = json(&dir.join("blocks.json"));
    assert_eq!(block_files(&manifest), ["night-mix-00-open.flac", "night-mix-01-full.flac"]);
    assert_eq!(manifest["blocks"][1]["frames"], LOOP);
    assert!(!dir.join("night-mix-03-tail.flac").exists(), "the previous render's later blocks are gone");
    let _ = std::fs::remove_dir_all(b);
}

#[test]
fn every_stem_is_a_file_per_block_with_one_gain_and_joins_back() {
    let b = scratch("stems");
    engines_song(&b, &["--stems", "stems", "--container", "wav", "--format", "s24"]);
    engines_song(&b, &["--stems", "whole", "--container", "wav", "--format", "s24", "--whole"]);
    let split = json(&b.join("stems/stems.json"));
    let whole = json(&b.join("whole/stems.json"));
    assert_eq!(split["blocks"].as_array().unwrap().len(), 4);
    let stems = split["stems"].as_array().unwrap();
    assert_eq!(stems.len(), 3, "pad, sub and the reverb bus: {split}");
    for s in stems {
        let id = s["id"].as_str().unwrap();
        let w = whole["stems"].as_array().unwrap().iter().find(|w| w["id"] == s["id"]).unwrap();
        assert_eq!(s["gain_db"], w["gain_db"], "{id}: one gain, measured over the whole song");
        let files: Vec<&str> = s["files"].as_array().unwrap().iter().map(|f| f["file"].as_str().unwrap()).collect();
        let name = if s["kind"] == "bus" { format!("bus_{id}") } else { id.to_string() };
        let want: Vec<String> = ["00-open", "01-full", "02-thin_air", "03-tail"].iter().map(|b| format!("night-{name}-{b}.wav")).collect();
        assert_eq!(files, want, "every block gets its file — a silent one too");
        let joined: Vec<f32> = files.iter().flat_map(|f| wav(&b.join("stems").join(f))).collect();
        assert!(joined == wav(&b.join("whole").join(w["file"].as_str().unwrap())), "{id}: the block files, end to end, are the whole stem");
    }
    // The sub only plays in "full": its first block's file holds digital silence.
    assert!(wav(&b.join("stems/night-sub-00-open.wav")).iter().all(|&x| x == 0.0));
    assert!(wav(&b.join("stems/night-sub-01-full.wav")).iter().any(|&x| x != 0.0));
    let _ = std::fs::remove_dir_all(b);
}

#[test]
fn stems_whose_block_files_would_share_a_name_are_refused_before_rendering() {
    // `pad` in block 0 (`01-deep`) and `pad-00` in block 1 (`deep`) both spell
    // `night-pad-00-01-deep` — the post-pass would overwrite one stem's audio with the other's.
    let b = scratch("clash");
    let doc = DOC
        .replace("\n    sub: {", "\n    \"pad-00\": {") // the instrument, not the `nosub` layer
        .replace("instruments.sub.notes", "instruments.pad-00.notes")
        .replace("label: \"open\"", "label: \"01-deep\"")
        .replace("label: \"full\"", "label: \"deep\"");
    std::fs::write(b.join("night.json5"), doc).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_engines-song")).current_dir(&b).args(["--doc", "night.json5", "--stems", "stems", "--container", "wav"]).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{err}");
    assert!(err.contains("pad (block 00) and pad-00 (block 01) would both write night-pad-00-01-deep.wav"), "{err}");
    assert!(!b.join("stems").exists(), "refused before anything was written");
    // `--whole` names stems without blocks — no clash.
    engines_song(&b, &["--stems", "whole", "--whole", "--max-loops", "1"]);
    let _ = std::fs::remove_dir_all(b);
}
