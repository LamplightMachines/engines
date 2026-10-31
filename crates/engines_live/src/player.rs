//! The live player's brain, device-independent (ADR 0034–0036): owns the [`Session`], watches the
//! piece's doc, applies every accepted revision (snapshotted into `.live/history`), keeps
//! `.live/status.json` current, and speaks the protocol (`contracts/engines_live_protocol_v1.md`):
//! [`LivePlayer::handle`] takes commands, and every state change lands in [`LivePlayer::events`]
//! (the binary prints them in `--stdio` mode). The binary adds the audio device or a headless drain.
//!
//! The session runs in **live mode** (ADR 0035): an accepted revision is applied mid-loop (fork +
//! retarget + crossfade), heard ~0.2 s later. A revision with errors is **rejected**: the last good
//! one keeps playing. `.live/pause` holds saves on disk back (multi-step edits land as one). Playing
//! modes: the doc's top-level matrix (`loop`), one song block as a plain loop (`block` — edits
//! re-materialize it), or the song from a block (`song` — edits are kept for later, not applied).
//! A saved `optimize` change or a `control` command switches hold ⇄ drift; `hold` re-applies the
//! doc so it plays exactly as written.

use crate::hooks::{NoteHook, NotePlan};
use crate::watch::DocWatcher;
use serde::Serialize;
use serde_json::{json, Value};
use engines_core::check::{check_str, DocDiag};
use engines_core::contract::{LiveToggle, OptimizeMode, SessionControl};
use engines_core::controller::DitherConfig;
use engines_core::engine::EditKind;
use engines_core::loop_doc::LoopDoc;
use engines_core::piece::{unix_now, write_atomic, Piece};
use engines_core::routing;
use engines_core::engine::SourceKey;
use engines_core::session::{Session, SessionConfig, SessionOptions, SourceRouting};
use std::path::{Path, PathBuf};
use engines_core::telemetry::SongStamp;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::AtomicU64;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// How the player runs (from the CLI).
#[derive(Clone, Debug)]
pub struct LiveOptions {
    pub sample_rate: f64,
    pub out_channels: usize,
    /// Ring length in milliseconds — the lookahead, and most of the edit latency.
    pub ring_ms: f64,
    /// Render block (frames): how often a pending edit is checked.
    pub block: usize,
    pub seed: u64,
    /// CLI control flags layered over the doc's own settings (ADR 0033). An unknown `optimize`
    /// refuses to start.
    pub optimize: String,
    pub orrery: bool,
    pub max_loops: Option<usize>,
    /// Debounce: a save is applied once the file has been quiet this long.
    pub quiet: Duration,
    /// `tick` event rate cap.
    pub tick_every: Duration,
    /// What to play first (`--loop-block K` / `--play-from K`; default the doc's matrix).
    pub start_mode: Mode,
    /// The live protection limiter's starting state (`--no-limiter` ⇒ off; the device shell applies it).
    pub limiter: bool,
    /// The output device's name for `ready` (`None` headless).
    pub device: Option<String>,
    /// Route every audio source to its own device pair (ADR 0044) per this routing map (`engines-als`
    /// writes it; without the file, the doc's sources play in order from 1/2). `None`: plain output.
    pub route_sources: Option<PathBuf>,
}

impl Default for LiveOptions {
    fn default() -> Self {
        Self {
            sample_rate: 48_000.0,
            out_channels: 2,
            ring_ms: 170.0,
            block: 1024,
            seed: 0x5EED,
            optimize: String::new(),
            orrery: false,
            max_loops: None,
            quiet: Duration::from_millis(300),
            tick_every: Duration::from_millis(50),
            start_mode: Mode::Loop,
            limiter: true,
            device: None,
            route_sources: None,
        }
    }
}

/// `status.routing` / `ready.routing` (ADR 0044): which device pair each audio source plays on.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RoutingStatus {
    /// The session's frame width (the device's channels).
    pub channels: usize,
    /// The routing map in use; absent ⇒ there is none yet (the doc's sources in order from 1/2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub map: Option<String>,
    pub pairs: Vec<RoutedPair>,
    /// The pair carrying what no slot does (`"15/16"`).
    pub overflow: String,
    /// The source the overflow shares its pair with (a full device).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared_with: Option<String>,
    /// The accepted doc's sources with no pair of their own — they play on the overflow until
    /// `make als` gives them a track (and a restart routes them).
    pub unrouted: Vec<String>,
    /// The routing map changed on disk since this session started: reopen the Live set, restart.
    pub stale: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// What a note hook adds (Engines Studio's MIDI mirror: `midi`, ADR 0048); none from the free player.
    #[serde(flatten)]
    pub notes: serde_json::Map<String, Value>,
}

/// One routed source.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RoutedPair {
    /// `"3/4"`.
    pub pair: String,
    /// `inst:<id>` / `bus:<id>`.
    pub source: String,
    /// Its Live track / stem name.
    pub name: String,
    /// What a note hook adds (the MIDI mirror's `midi` / `no_midi`, ADR 0048); none from the free player.
    #[serde(flatten)]
    pub notes: serde_json::Map<String, Value>,
}

/// The last revision that was refused (cleared by the next accepted one).
#[derive(Clone, Debug, Serialize)]
pub struct Rejected {
    pub at: u64,
    pub errors: Vec<DocDiag>,
}

/// What the attached editor (`--stdio` host) said about its copy of the doc.
#[derive(Clone, Debug, Serialize)]
pub struct EditorState {
    /// Its buffer holds edits that aren't saved: an edit written to the file now (Claude's) would
    /// collide with them — save or discard first.
    pub unsaved: bool,
    pub at: u64,
}

/// `.live/status.json` — what the editor panel and Claude read.
#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub piece: String,
    pub pid: u32,
    /// `playing` | `paused` (a `.live/pause` file holds saves back) | `stopped` | `finished`.
    pub state: String,
    /// `loop` (the doc's matrix) | `block` (one song block, looped) | `song` (the song from a block).
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block: Option<usize>,
    /// The newest accepted revision (applied or queued).
    pub rev: u64,
    /// The timbre axis the audio is playing under (`hold` / `drift` / `live_edit`).
    pub optimize: String,
    pub orrery: bool,
    /// The revision the audio is playing now (rendered — up to a ring ahead of the speakers).
    pub applied_rev: u64,
    /// The loop at which `applied_rev` started, and where in it (seconds).
    pub applied_at_loop: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied_at_s: Option<f64>,
    /// `value` | `structural` — what the last applied edit was (its crossfade length follows).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rejected: Option<Rejected>,
    /// Warnings of the accepted revision (advice — it still plays).
    pub warnings: Vec<DocDiag>,
    pub muted: Vec<String>,
    pub soloed: Vec<String>,
    pub loop_id: u64,
    pub loop_s: f64,
    /// Where in the loop the renderer is (seconds; ~a ring ahead of the speakers).
    pub pos_s: f64,
    /// Per-instrument peak (dBFS, post mute/solo) since the last status write.
    pub meters: BTreeMap<String, f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chord: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tension: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peak_dbfs: Option<f32>,
    /// Live limiter gain reduction over the last status interval (its deepest, dB ≤ 0); absent
    /// headless or with the limiter off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limiter_gr_db: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nan_muted: Option<u64>,
    pub underruns: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The editor's last `editor` command (absent without one: a terminal `make live`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editor: Option<EditorState>,
    /// Per-source device routing (`--route-sources`, ADR 0044).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routing: Option<RoutingStatus>,
    pub updated_at: u64,
}

/// What's playing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Loop,
    Block(usize),
    Song(usize),
}

/// What [`LivePlayer::apply_text`] did with a revision.
#[derive(Debug, PartialEq, Eq)]
pub enum EditOutcome {
    /// Accepted as this revision and handed to the session (applied mid-loop: an `applied` follows).
    Queued(u64),
    /// Accepted as this revision but not applied now — a song is playing, or nothing is (stopped,
    /// finished): the next `play` / `song stop` starts on it (no `applied` of its own).
    Kept(u64),
    /// Identical to the playing revision — nothing to do.
    Unchanged,
    /// Refused (errors listed in the status).
    Rejected,
}

/// Where a revision came from (`doc` command vs the file on disk).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Command,
    Disk,
}

/// Running tick accumulators (between `tick` events).
#[derive(Default)]
struct TickAcc {
    loop_id: u64,
    pos: usize,
    total: usize,
    master: f32,
    /// The master peak per channel (linear).
    master_lr: [f32; 2],
    meters: BTreeMap<String, f32>,
    /// Per-instrument `[left, right]` peaks (dBFS).
    meters_lr: BTreeMap<String, [f32; 2]>,
    /// Where in the song (or the looped block) the latest block is.
    song: Option<SongStamp>,
    /// The limiter's deepest gain reduction since the last tick (`None`: headless / limiter off).
    gr_db: Option<f32>,
}

pub struct LivePlayer {
    pub piece: Piece,
    pub status: Status,
    /// Protocol events not yet sent (the binary drains + prints them in `--stdio` mode).
    pub events: Vec<Value>,
    /// Bumped every time a new session starts — the device shell re-attaches its output stream.
    pub epoch: u64,
    /// Requested limiter state (the device shell applies it).
    pub limiter_on: bool,
    /// The device name for `ready` (`None` headless).
    pub device: Option<String>,
    session: Option<Session>,
    opts: LiveOptions,
    watcher: DocWatcher,
    last_status_write: Instant,
    last_tick: Instant,
    /// The accepted doc (with its song) and the doc actually playing (song stripped / a block).
    doc: LoopDoc,
    live_doc: LoopDoc,
    mode: Mode,
    mute: BTreeSet<String>,
    solo: BTreeSet<String>,
    /// The `optimize` of the last accepted doc — a revision that changes it switches the mode.
    doc_optimize: Option<OptimizeMode>,
    /// `control` command overrides (cleared when the doc's own `optimize` changes).
    panel_optimize: Option<OptimizeMode>,
    panel_orrery: Option<bool>,
    max_change: f32,
    control_rev: u64,
    acc: TickAcc,
    /// A rendering preview's result channel and the `id` of the command that asked for it.
    preview: Option<(mpsc::Receiver<Value>, Option<Value>)>,
    /// `doc` command ids by the revision they queued — echoed on that revision's `applied`.
    pending_ids: BTreeMap<u64, Value>,
    /// The `quit` command's id — echoed on the final `stopped`.
    quit_id: Option<Value>,
    /// The accepted doc's instrument ids in document order (`LoopDoc` keys are sorted).
    inst_order: Vec<String>,
    /// The accepted doc's text (the authored order of its sources, for routing).
    doc_text: String,
    /// The routing map's modification time when the session started (a change ⇒ `stale`).
    route_mtime: Option<std::time::SystemTime>,
    /// Block mode: the loop the looped block started at (its loop 0) — set by the first loop seen
    /// after entering the block.
    block_origin: Option<u64>,
    /// Song mode: the latest song position the session reported.
    song_pos: Option<SongStamp>,
    /// What a companion player does with the session's notes (Studio's MIDI mirror); none here.
    notes: Option<Box<dyn NoteHook>>,
    /// The instruments the next session taps (the hook's plan).
    taps: BTreeSet<String>,
}

/// The ids under `instruments` in the order the doc's text writes them.
fn instrument_order(text: &str) -> Vec<String> {
    engines_authoring::cst::member_order(text, "instruments")
}

/// The doc's audio sources in the order their tracks appear (the doc's text order, instruments then
/// buses), each with its stem name.
fn doc_sources(text: &str, doc: &LoopDoc) -> Vec<(SourceKey, String)> {
    routing::doc_sources(doc, &instrument_order(text), &engines_authoring::cst::member_order(text, "buses"))
}

/// A history `source` label from a `doc` command: a short lowercase token (`editor`, `revert`, …),
/// `editor` when absent or malformed.
fn history_source(cmd: &Value) -> String {
    let ok = |s: &&str| !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    cmd.get("source").and_then(Value::as_str).filter(ok).unwrap_or("editor").to_string()
}

/// Check a doc's text strictly; `Ok((doc, warnings))` or the errors.
fn accept(text: &str) -> Result<(LoopDoc, Vec<DocDiag>), Vec<DocDiag>> {
    let checked = check_str(text);
    if checked.has_errors() {
        return Err(checked.errors().cloned().collect());
    }
    let warnings = checked.warnings().cloned().collect();
    Ok((checked.doc.expect("a doc without errors parsed"), warnings))
}

fn label(m: OptimizeMode) -> &'static str {
    match m {
        OptimizeMode::Hold => "hold",
        OptimizeMode::Drift => "drift",
        OptimizeMode::LiveEdit => "live_edit",
    }
}

fn source_label(source: Source) -> &'static str {
    match source {
        Source::Command => "command",
        Source::Disk => "disk",
    }
}

fn parse_optimize(s: &str) -> Option<OptimizeMode> {
    match s {
        "hold" => Some(OptimizeMode::Hold),
        "drift" => Some(OptimizeMode::Drift),
        "live_edit" => Some(OptimizeMode::LiveEdit),
        _ => None,
    }
}

/// The doc's key for a host's keyboard: its name, root pitch class, the scale (semitones above the
/// root) with degree labels, and every pitch class spelled in the key (`names[pc]`). `null` when
/// `harmony` doesn't parse (the checker refuses such a doc, so only a hand-built one).
fn key_info(doc: &LoopDoc) -> Value {
    use engines_core::theory::{Key, Pc};
    let Some(key) = Key::new(&doc.harmony.root, &doc.harmony.mode) else { return Value::Null };
    json!({
        "name": key.name(),
        "root": key.scale.root.as_i32(),
        "scale": key.scale.degrees(),
        "degrees": key.scale.mode.degree_labels(),
        "names": (0..12).map(|pc| key.spell(Pc::new(pc))).collect::<Vec<_>>(),
    })
}

fn db(x: f32) -> f32 {
    if x > 0.0 { (20.0 * x.log10()).max(-120.0) } else { -120.0 }
}

/// A `preview` command's length and kind: `seconds` (default 10) clamped to 1..600 for the mix, to
/// 1..300 with `stems` (every source is a file of its own).
fn preview_request(cmd: &Value) -> (f64, bool) {
    let stems = cmd.get("stems").and_then(Value::as_bool).unwrap_or(false);
    let secs = cmd.get("seconds").and_then(Value::as_f64).unwrap_or(10.0).clamp(1.0, if stems { 300.0 } else { 600.0 });
    (secs, stems)
}

/// The disk a stems preview needs at its peak: per source a float temp (4 bytes) and its 24-bit
/// stem (3 bytes), plus the float mix (`preview.wav.tmp`, 4 bytes), written while the temps still
/// exist. All stereo.
fn stems_preview_bytes(sources: usize, secs: f64, sr: f64) -> u64 {
    (secs * sr * 2.0 * (sources as f64 * 7.0 + 4.0)) as u64
}

/// What a preview thread renders.
struct PreviewJob {
    secs: f64,
    sr: f64,
    seed: u64,
    max_change: f32,
    path: std::path::PathBuf,
    live_dir: std::path::PathBuf,
    /// The stems' sources and file names, when stems were asked for.
    stems: Option<(Vec<engines_core::engine::SourceKey>, Vec<String>)>,
}

/// Render a preview: the mix into `preview.wav` (written beside it and renamed over it once whole,
/// so the last one stays intact until then), and the stems — split from the same render, each
/// through a float temp, normalized and written as 24-bit WAV, then renamed over its old file — into
/// `preview_stems/` with a `stems.json`. Only stems the previous manifest listed are ever removed.
fn render_preview(doc: LoopDoc, control: Option<SessionControl>, job: PreviewJob) -> Result<Value, String> {
    use engines_core::stems::{self as st, StemSplitter, TempGuard};
    let PreviewJob { secs, sr, seed, max_change, path, live_dir, stems } = job;
    let t0 = Instant::now();
    // Progress on stderr: the VS Code extension shows engines-live's stderr in its Output channel.
    let what = stems.as_ref().map_or("the mix".to_string(), |(l, _)| format!("the mix + {} stems", l.len()));
    eprintln!("preview: rendering {secs:.0} s of {what} offline (seed {seed:#x})…");
    let loop_frames = doc.total_frames(sr).max(1);
    let want = (secs * sr) as usize;
    let loops = want.div_ceil(loop_frames);
    let mut s = Session::start(
        doc,
        SessionConfig { sample_rate: sr, max_loops: Some(loops), source_stems: stems.is_some(), ..Default::default() },
        DitherConfig { max_change_per_loop: max_change, seed, ..Default::default() },
        control,
    );
    let width = s.channels() as usize;
    // The stems' temps: a run dir of our own under `.live/.stems-tmp` (a killed run's are reclaimed).
    let tmp_root = live_dir.join(".stems-tmp");
    let mut split = None;
    let mut guard = None;
    if let Some((layout, names)) = &stems {
        std::fs::create_dir_all(&tmp_root).map_err(|e| e.to_string())?;
        st::reclaim_stale_runs(&tmp_root);
        let run = st::create_run_dir(&tmp_root).map_err(|e| e.to_string())?;
        let temps: Vec<std::path::PathBuf> = names.iter().map(|n| run.join(format!("{n}{}", st::TEMP_SUFFIX))).collect();
        if s.source_layout() != Some(layout.as_slice()) {
            return Err(format!("stem layout mismatch: planned {layout:?}, session {:?}", s.source_layout()));
        }
        guard = Some(TempGuard { files: temps.iter().cloned().chain([run.join(st::OWNER_MARKER)]).collect(), dirs: vec![run, tmp_root.clone()] });
        split = Some(StemSplitter::new(&temps, width, sr));
    }
    let mut audio = Vec::with_capacity(want * 2);
    let mut buf = vec![0.0f32; 8192 / width * width + width];
    let (mut carry, mut frames) = (0usize, 0usize);
    let mut next_report = 1usize;
    while !s.finished() && frames < want {
        let n = s.pull_audio(&mut buf[carry..]);
        let total = carry + n;
        let whole = (total / width).min(want - frames) * width;
        if whole > 0 {
            audio.extend(buf[..whole].chunks_exact(width).flat_map(|f| [f[0], f[1]]));
            if let Some(sp) = split.as_mut() {
                sp.write_frames(&buf[..whole]).map_err(|e| e.to_string())?;
            }
            frames += whole / width;
            while next_report < 20 && frames * 20 >= want * next_report {
                eprintln!("preview: {}% ({:.1} of {secs:.0} s)", next_report * 5, frames as f64 / sr);
                next_report += 1;
            }
        }
        buf.copy_within(whole..total, 0);
        carry = total - whole;
        if n == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    s.stop();
    let peak = audio.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    let mut meter = engines_core::loudness::LoudnessMeter::new(sr, 2);
    meter.push_interleaved(&audio);
    let tmp = path.with_extension("wav.tmp");
    let written = std::fs::create_dir_all(&live_dir).map_err(|e| e.to_string()).and_then(|()| {
        let mut w = engines_core::wav::WavWriter::create(&tmp, 2, sr as u32, engines_core::wav::SampleFormat::F32).map_err(|e| e.to_string())?;
        w.write_interleaved(&audio).map_err(|e| e.to_string())?;
        w.finalize().map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    });
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let lufs_text = meter.integrated().map_or("silent".to_string(), |l| format!("{l:.1} LUFS"));
    eprintln!("preview: mix → {} · peak {:.1} dBFS · {lufs_text}", path.display(), db(peak));
    let mut ev = json!({ "type": "preview", "path": path, "seconds": audio.len() as f64 / 2.0 / sr, "peak_db": db(peak), "lufs": meter.integrated(), "stems": Value::Null });
    let (Some(sp), Some((layout, names))) = (split, stems) else {
        eprintln!("preview: done in {:.1} s", t0.elapsed().as_secs_f64());
        return Ok(ev);
    };
    eprintln!("preview: normalizing the stems to {} LUFS…", st::DEFAULT_LUFS);
    let measured = sp.finish().map_err(|e| e.to_string())?;
    let dir = live_dir.join("preview_stems");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let jobs: Vec<(usize, std::path::PathBuf, st::Decision)> = measured.iter().enumerate().filter_map(|(i, m)| m.temp.clone().map(|t| (i, t, st::decide(m, st::DEFAULT_LUFS)))).collect();
    let results = st::parallel_map(&jobs, |(i, temp, d)| {
        let out = dir.join(format!("{}.wav", names[*i]));
        let part = dir.join(format!("{}.wav.tmp", names[*i]));
        let r = st::rescale_to_wav(temp, &part, d.gain_db, engines_core::wav::SampleFormat::S24).and_then(|res| std::fs::rename(&part, &out).map(|()| res));
        if r.is_err() {
            let _ = std::fs::remove_file(&part);
        }
        r
    });
    drop(guard);
    if let Some(e) = results.iter().find_map(|r| r.as_ref().err()) {
        return Err(format!("writing a stem into {}: {e}", dir.display()));
    }
    let r2 = |x: f64| (x * 100.0).round() / 100.0;
    let files: Vec<Value> = jobs
        .iter()
        .zip(&results)
        .map(|((i, _, d), res)| {
            let (out_lufs, out_peak) = *res.as_ref().expect("checked above");
            json!({
                "id": layout[*i].id(), "kind": layout[*i].kind(), "file": format!("{}.wav", names[*i]),
                "gain_db": d.gain_db, "normalized": d.normalized, "limited_by": d.limited_by,
                "raw_lufs": d.raw_lufs.map(r2), "out_lufs": out_lufs.map(r2), "out_peak_dbfs": r2(st::db(out_peak)),
            })
        })
        .collect();
    for ((i, _, d), res) in jobs.iter().zip(&results) {
        let (out_lufs, out_peak) = *res.as_ref().expect("checked above");
        let lufs = out_lufs.map_or("silent".to_string(), |l| format!("{l:.1} LUFS"));
        let limit = if d.limited_by == Some("peak") { " (held by the peak ceiling)" } else { "" };
        eprintln!("preview:   {:<24} gain {:+.1} dB · {lufs}{limit} · peak {:.1} dBFS", format!("{}.wav", names[*i]), d.gain_db, st::db(out_peak));
    }
    let skipped: Vec<Value> = measured.iter().zip(&layout).filter(|(m, _)| m.temp.is_none()).map(|(_, k)| json!({ "id": k.id(), "kind": k.kind(), "reason": "digital silence" })).collect();
    for k in measured.iter().zip(&layout).filter(|(m, _)| m.temp.is_none()).map(|(_, k)| k) {
        eprintln!("preview:   {k} skipped (digital silence)");
    }
    let fresh: Vec<String> = jobs.iter().map(|(i, ..)| format!("{}.wav", names[*i])).collect();
    st::remove_stale_stems(&dir, &fresh);
    let mut manifest = json!({
        "tool": "engines-live preview (ADR 0040)", "engine_version": engines_core::version(),
        "engine_build": engines_core::build_id(), "sample_rate": sr as u32,
        "seconds": audio.len() as f64 / 2.0 / sr, "container": "wav", "format": "s24",
        "normalization": { "target_lufs": st::DEFAULT_LUFS, "peak_ceiling_dbfs": st::PEAK_CEILING_DBFS },
        "stems": files, "skipped": skipped,
    });
    if let Some(host) = engines_core::host_build() {
        manifest["host_build"] = host.into();
    }
    std::fs::write(dir.join(st::MANIFEST), serde_json::to_string_pretty(&manifest).unwrap_or_default() + "\n").map_err(|e| e.to_string())?;
    ev["stems"] = json!({ "dir": dir, "target_lufs": st::DEFAULT_LUFS, "files": manifest["stems"], "skipped": manifest["skipped"] });
    eprintln!("preview: stems → {} · done in {:.1} s", dir.display(), t0.elapsed().as_secs_f64());
    Ok(ev)
}

/// A song position as the protocol writes it (`loop.song`, `tick.song`).
fn song_json(s: SongStamp) -> Value {
    json!({ "block": s.block, "seam": s.seam, "loop_in_block": s.loop_in_block, "block_loops": s.block_loops, "tail": s.tail })
}

/// The refusal of a song block out of range — one message for the `song` command and the CLI's
/// `--loop-block` / `--play-from` (`what` names the one asking).
pub fn song_block_error(what: &str, blocks: usize) -> String {
    format!("`{what}` needs a \"block\" in 0..{blocks} (the doc has {blocks} blocks)")
}

/// How many blocks the `song` of the doc at `path` has (0 without one); `None` when the doc can't
/// be read or has errors (starting reports those).
pub fn song_blocks(path: &std::path::Path) -> Option<usize> {
    let (doc, _) = accept(&std::fs::read_to_string(path).ok()?).ok()?;
    Some(doc.song.as_ref().map_or(0, |s| s.sequence.len()))
}

impl LivePlayer {
    /// Load + check the piece's doc (errors refuse to start), snapshot it, start playing.
    pub fn start(piece: Piece, opts: &LiveOptions) -> Result<Self, String> {
        Self::start_with(piece, opts, None)
    }

    /// [`start`](Self::start) with a [`NoteHook`]: what a companion player (Engines Studio's) does
    /// with the notes.
    pub fn start_with(piece: Piece, opts: &LiveOptions, notes: Option<Box<dyn NoteHook>>) -> Result<Self, String> {
        let path = piece.doc_path();
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let (doc, warnings) = accept(&text).map_err(|errs| {
            let lines: Vec<String> = errs.iter().map(|e| format!("  {e}")).collect();
            format!("{} has errors — fix them first:\n{}", path.display(), lines.join("\n"))
        })?;
        let max_change = doc.max_change.unwrap_or(engines_core::controller::DEFAULT_MAX_CHANGE);
        // The CLI flags layer over the doc's own control (ADR 0033): one that doesn't parse (an
        // unknown `optimize`) refuses to start rather than dropping the doc's movement/constraints.
        SessionControl::with_flags(SessionControl::from_doc(&doc), &opts.optimize, opts.orrery, &[], &[], max_change)?;
        let rev = piece.snapshot(&text, "disk", unix_now()).map_err(|e| format!("{}: {e}", piece.history_dir().display()))?;
        let loop_s = doc.total_frames(opts.sample_rate) as f64 / opts.sample_rate;
        let status = Status {
            piece: piece.slug.clone(),
            pid: std::process::id(),
            state: "stopped".into(),
            mode: "loop".into(),
            block: None,
            rev: rev.rev as u64,
            optimize: "drift".into(),
            orrery: false,
            applied_rev: 0,
            applied_at_loop: None,
            applied_at_s: None,
            applied_kind: None,
            rejected: None,
            warnings,
            muted: Vec::new(),
            soloed: Vec::new(),
            loop_id: 0,
            loop_s,
            pos_s: 0.0,
            meters: BTreeMap::new(),
            chord: None,
            tension: None,
            peak_dbfs: None,
            limiter_gr_db: None,
            nan_muted: None,
            underruns: 0,
            note: None,
            editor: None,
            routing: None,
            updated_at: unix_now(),
        };
        let watcher = DocWatcher::new(path, opts.quiet, &text);
        let mut live_doc = doc.clone();
        live_doc.song = None;
        let mut p = Self {
            piece,
            status,
            events: Vec::new(),
            epoch: 0,
            limiter_on: opts.limiter,
            device: opts.device.clone(),
            session: None,
            opts: opts.clone(),
            watcher,
            last_status_write: Instant::now() - Duration::from_secs(10),
            last_tick: Instant::now(),
            doc_optimize: doc.optimize,
            doc,
            live_doc,
            mode: opts.start_mode,
            mute: BTreeSet::new(),
            solo: BTreeSet::new(),
            panel_optimize: None,
            panel_orrery: None,
            max_change,
            control_rev: 0,
            acc: TickAcc::default(),
            preview: None,
            pending_ids: BTreeMap::new(),
            quit_id: None,
            inst_order: instrument_order(&text),
            doc_text: text.clone(),
            route_mtime: None,
            block_origin: None,
            song_pos: None,
            notes,
            taps: BTreeSet::new(),
        };
        p.play();
        p.events.push(p.ready_event());
        Ok(p)
    }

    /// The running session (absent while stopped).
    pub fn session_mut(&mut self) -> Option<&mut Session> {
        self.session.as_mut()
    }

    /// The doc for the current mode (song stripped, or one block materialized).
    fn doc_for_mode(&self, mode: Mode) -> (LoopDoc, Mode) {
        match (mode, self.doc.song.as_ref()) {
            (Mode::Block(k), Some(spec)) if k < spec.sequence.len() => match engines_core::song::materialize(&self.doc, spec, k) {
                Ok(d) => (d, mode),
                Err(_) => (self.stripped(), Mode::Loop),
            },
            (Mode::Song(k), Some(spec)) if k < spec.sequence.len() => (self.doc.clone(), mode),
            _ => (self.stripped(), Mode::Loop),
        }
    }

    fn stripped(&self) -> LoopDoc {
        let mut d = self.doc.clone();
        d.song = None;
        d
    }

    /// The control for `doc` with the CLI flags, then any `control`-command overrides.
    fn control_for(&self, doc: &LoopDoc) -> Option<SessionControl> {
        // The flags were checked at start, so this can't fail — but never drop the doc's own control.
        let flagged = SessionControl::with_flags(SessionControl::from_doc(doc), &self.opts.optimize, self.opts.orrery, &[], &[], self.max_change);
        let mut c = flagged.unwrap_or_else(|_| SessionControl::from_doc(doc));
        if self.panel_optimize.is_some() || self.panel_orrery.is_some() {
            let mut cc = c.unwrap_or_default();
            if let Some(o) = self.panel_optimize {
                cc.optimize = o;
            }
            if let Some(on) = self.panel_orrery {
                cc.movement.orrery = on;
            }
            cc.guardrails.max_change_per_loop = self.max_change;
            c = Some(cc);
        }
        c
    }

    /// Start a fresh session for the current mode (stopping any running one).
    pub fn play(&mut self) {
        self.stop_session();
        // A fresh session: nothing of the old one's position, meters or song place carries over.
        self.acc = TickAcc::default();
        self.last_tick = Instant::now();
        self.block_origin = None;
        self.song_pos = None;
        let (doc, mode) = self.doc_for_mode(self.mode);
        self.mode = mode;
        let control = self.control_for(&doc);
        self.status.optimize = label(control.as_ref().map(|c| c.optimize).unwrap_or_default()).into();
        self.status.orrery = control.as_ref().is_some_and(|c| c.movement.orrery);
        // A song started at a block with its own control plays that from its first loop (ADR 0055).
        if let (Mode::Song(k), Some(spec)) = (mode, self.doc.song.as_ref()) {
            if let Some(bc) = spec.sequence.get(k).and_then(|b| b.control.as_ref()) {
                if let Some(on) = bc.orrery() {
                    self.status.orrery = on;
                }
                if let Some(o) = bc.optimize {
                    self.status.optimize = label(o).into();
                }
            }
        }
        let song_start_block = if let Mode::Song(k) = mode { k } else { 0 };
        let mut live_doc = doc.clone();
        if !matches!(mode, Mode::Song(_)) {
            live_doc.song = None;
        }
        self.status.loop_s = self.loop_s(&live_doc);
        // A `doc` id waiting for its `applied` belongs to the old session (and an edit kept for this
        // play has none): a re-apply in the new one mustn't echo it.
        self.pending_ids.clear();
        let cfg = SessionConfig {
            sample_rate: self.opts.sample_rate,
            max_loops: self.opts.max_loops,
            ring_capacity: ((self.opts.ring_ms / 1000.0 * self.opts.sample_rate) as usize * 2).max(2 * self.opts.block),
            live_block: Some(self.opts.block),
            out_channels: self.opts.out_channels,
            doc_rev: self.status.rev,
            song_start_block,
            ..Default::default()
        };
        let dither = DitherConfig { max_change_per_loop: self.max_change, seed: self.opts.seed, ..Default::default() };
        // What the client last heard of the routing (`ready`, or an earlier `routing` event).
        let announced = self.status.routing.clone();
        self.taps.clear();
        let session = match self.opts.route_sources.clone() {
            None => Session::start(live_doc.clone(), cfg, dither, control),
            Some(path) => match self.plan_routing(&path, &live_doc).and_then(|r| {
                let opts = SessionOptions { routing: Some(r), note_tap: self.taps.clone(), note_log: false };
                Session::start_with(live_doc.clone(), cfg, dither, control, opts)
            }) {
                Ok(s) => s,
                Err(e) => {
                    self.events.push(json!({ "type": "error", "message": format!("routing: {e}") }));
                    self.status.state = "stopped".into();
                    return;
                }
            },
        };
        self.live_doc = live_doc;
        self.refresh_unrouted();
        // A restart re-reads the routing map (a rebuilt Live set, a MIDI switch): tell the client what
        // now plays — `ready` carries it only at start and on an output switch.
        if announced.is_some() && self.status.routing != announced {
            self.events.push(json!({ "type": "routing", "stale": false, "routing": self.status.routing }));
        }
        self.session = Some(session);
        self.apply_gains();
        self.epoch += 1;
        if let Some(e) = self.notes.as_mut().and_then(|h| h.begin()) {
            self.events.push(json!({ "type": "error", "message": e }));
        }
        self.status.applied_rev = 0;
        self.status.applied_at_loop = None;
        self.status.applied_at_s = None;
        self.status.applied_kind = None;
        self.status.state = "playing".into();
        self.set_mode_status();
        self.events.push(json!({ "type": "playing", "mode": self.status.mode, "block": self.status.block }));
    }

    /// Whether a routing map can drive an output `out_channels` wide (ADR 0044 D7): checked before
    /// switching, so a refused switch leaves the old output playing. `None`: plain output, always fine.
    pub fn check_output(&self, out_channels: usize, route: Option<&Path>) -> Result<(), String> {
        match route.map(routing::read).transpose()?.flatten() {
            Some(map) => routing::session_routing(&map, out_channels).map(|_| ()),
            None => Ok(()), // no map yet: the sources fill the device's pairs in order
        }
    }

    /// Play on another output (ADR 0044 D7): the device shell has opened it — `sample_rate` and
    /// `out_channels` are its own — and `route` is the routing map to play by (`None`: plain). A
    /// session that was running starts over (like `restart`); mutes, solos, hold/drift and the song
    /// mode carry over. Announced with a fresh `ready`.
    pub fn set_output(&mut self, sample_rate: f64, out_channels: usize, device: Option<String>, route: Option<PathBuf>) {
        self.opts.sample_rate = sample_rate;
        self.opts.out_channels = out_channels;
        self.opts.route_sources = route;
        self.device = device;
        self.status.routing = None;
        self.route_mtime = None;
        if self.session.is_some() {
            self.play();
        } else {
            self.status.loop_s = self.loop_s(&self.live_doc.clone());
        }
        self.events.push(self.ready_event());
    }

    /// A session is rendering — an edit or a block swap can land in it (not stopped, not finished).
    fn live(&self) -> bool {
        self.session.is_some() && self.status.state != "finished"
    }

    fn loop_s(&self, doc: &LoopDoc) -> f64 {
        doc.total_frames(self.opts.sample_rate) as f64 / self.opts.sample_rate
    }

    /// Swap what the session plays to `live` mid-loop (an edit, a block swap), as the current revision.
    fn apply_live(&mut self, live: LoopDoc) {
        self.status.loop_s = self.loop_s(&live);
        self.live_doc = live.clone();
        if let Some(s) = self.session.as_ref() {
            s.apply_doc_rev(live, self.status.rev);
        }
        self.apply_gains();
        self.refresh_unrouted();
    }

    /// The routing for a new session (ADR 0044), with its status: the piece's routing map as `engines-als`
    /// built it — frozen, since the open Live set's tracks listen on those pairs — or, without one,
    /// the doc's sources in order from 1/2. `Err` when the map can't drive this device.
    fn plan_routing(&mut self, path: &Path, live: &LoopDoc) -> Result<SourceRouting, String> {
        let map = routing::read(path)?;
        self.route_mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        let sources = doc_sources(&self.doc_text, live);
        let status = match &map {
            Some(m) => {
                let r = routing::session_routing(m, self.opts.out_channels)?;
                let plan: NotePlan = self.notes.as_mut().map(|h| h.plan(m, live)).unwrap_or_default();
                let pairs = m
                    .slots
                    .iter()
                    .map(|s| RoutedPair { pair: routing::pair_label(s.channel), source: s.source.clone(), name: s.name.clone(), notes: plan.pairs.get(&s.source).cloned().unwrap_or_default() })
                    .collect();
                self.taps = plan.taps;
                let st = RoutingStatus { channels: r.width, map: Some(path.display().to_string()), pairs, overflow: routing::pair_label(m.overflow.channel), shared_with: m.overflow.shared_with.clone(), unrouted: Vec::new(), stale: false, note: None, notes: plan.status };
                (r, st)
            }
            None => {
                let keys: Vec<SourceKey> = sources.iter().map(|(k, _)| k.clone()).collect();
                let r = SourceRouting::sequential(&keys, self.opts.out_channels);
                let pairs = sources.iter().take(r.slots.len()).enumerate().map(|(p, (k, n))| RoutedPair { pair: routing::pair_label(routing::channel_of(p)), source: k.to_string(), name: n.clone(), notes: Default::default() }).collect();
                let shared_with = r.slots[r.residual].as_ref().map(|k| k.to_string());
                let note = Some("no routing map yet: the sources play in doc order from 1/2 — `make als` builds the Live set and its map".to_string());
                let st = RoutingStatus { channels: r.width, map: None, pairs, overflow: routing::pair_label(routing::channel_of(r.residual)), shared_with, unrouted: Vec::new(), stale: false, note, notes: Default::default() };
                (r, st)
            }
        };
        self.status.routing = Some(status.1);
        Ok(status.0)
    }

    /// Which of the playing doc's sources have no pair of their own (they ride the overflow).
    fn refresh_unrouted(&mut self) {
        let sources = doc_sources(&self.doc_text, &self.live_doc);
        if let Some(rs) = self.status.routing.as_mut() {
            rs.unrouted = sources.iter().map(|(k, _)| k.to_string()).filter(|s| !rs.pairs.iter().any(|p| &p.source == s)).collect();
        }
    }

    /// Stop the session; its MIDI mirror ends what it left sounding (and drops what it rendered
    /// ahead) — the ports stay open for the next one.
    fn stop_session(&mut self) {
        if let Some(mut s) = self.session.take() {
            s.stop();
            // The worker's exit flush (Note-Offs for every mirrored note) comes as it exits.
            let deadline = Instant::now() + Duration::from_secs(2);
            while !s.worker_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(2));
            }
            let taps = s.poll_note_tap();
            s.join();
            if let Some(h) = self.notes.as_mut() {
                h.push(self.epoch, taps);
                h.flush();
            }
        }
    }

    /// The device shell started session `epoch`'s output: `played` counts the ring samples it has
    /// played, `width` to a frame — the clock a note hook's notes go out against.
    pub fn attach_note_clock(&mut self, played: Arc<AtomicU64>, width: u64) {
        if let Some(h) = self.notes.as_mut() {
            h.attach(self.epoch, played, width);
        }
    }

    fn set_mode_status(&mut self) {
        let (m, b) = match self.mode {
            Mode::Loop => ("loop", None),
            Mode::Block(k) => ("block", Some(k)),
            Mode::Song(k) => ("song", Some(k)),
        };
        self.status.mode = m.into();
        self.status.block = b;
        self.status.note = match self.mode {
            Mode::Song(_) => Some("a song is playing: edits are kept (and snapshotted) but not applied until `song stop` (ADR 0029)".into()),
            _ if self.doc.song.is_some() && self.mode == Mode::Loop => Some("the doc's `song` isn't played in loop mode — `song loop_block` / `play_from` audition it".into()),
            _ => None,
        };
    }

    /// Mute/solo → live gains for every instrument of the playing doc.
    fn apply_gains(&mut self) {
        let gains: BTreeMap<String, f32> = self
            .live_doc
            .instruments
            .keys()
            .map(|id| {
                let off = self.mute.contains(id) || (!self.solo.is_empty() && !self.solo.contains(id));
                (id.clone(), if off { 0.0 } else { 1.0 })
            })
            .collect();
        if let Some(s) = self.session.as_ref() {
            s.set_live_gains(gains);
        }
        self.status.muted = self.mute.iter().cloned().collect();
        self.status.soloed = self.solo.iter().cloned().collect();
    }

    /// Check a revision's text and queue it (snapshotting it, its history `origin` — `editor`,
    /// `revert`, `disk` … — recorded with it) or reject it.
    pub fn apply_text(&mut self, text: &str, source: Source, origin: &str, version: Option<Value>) -> EditOutcome {
        if source == Source::Command {
            self.watcher.seen_text(text); // its save (the editor sends on save) isn't news
        }
        match accept(text) {
            Err(errors) => {
                self.events.push(json!({ "type": "rejected", "version": version, "errors": errors, "source": source_label(source) }));
                self.status.rejected = Some(Rejected { at: unix_now(), errors });
                EditOutcome::Rejected
            }
            Ok((doc, warnings)) => {
                // Every revision that plays is in the history (renders and sidecars name it): one
                // that can't be snapshotted (a full disk) is refused, not applied unrecorded.
                let rev = match self.piece.snapshot(text, origin, unix_now()) {
                    Ok(r) => r.rev as u64,
                    Err(e) => {
                        let errors = vec![DocDiag::error("", "history", format!("couldn't snapshot this revision into {}: {e} — not applied", self.piece.history_dir().display()))];
                        self.events.push(json!({ "type": "rejected", "version": version, "errors": errors, "source": source_label(source) }));
                        self.status.rejected = Some(Rejected { at: unix_now(), errors });
                        return EditOutcome::Rejected;
                    }
                };
                self.status.rejected = None;
                self.status.warnings = warnings.clone();
                if rev == self.status.rev {
                    // From disk too: a refused save put back (Claude restoring the good text) clears
                    // the refusal, and the host only hears that from this event.
                    self.events.push(json!({ "type": "unchanged", "version": version, "rev": rev, "source": source_label(source) }));
                    return EditOutcome::Unchanged;
                }
                self.status.rev = rev;
                self.inst_order = instrument_order(text);
                self.doc_text = text.to_string();
                let optimize_changed = doc.optimize != self.doc_optimize;
                self.doc_optimize = doc.optimize;
                self.doc = doc;
                if optimize_changed {
                    self.panel_optimize = None; // the doc's own switch wins from here
                }
                self.events.push(json!({ "type": "accepted", "version": version, "rev": rev, "warnings": warnings }));
                if source == Source::Disk {
                    self.events.push(json!({ "type": "external_change", "rev": rev }));
                }
                if matches!(self.mode, Mode::Song(_)) || !self.live() {
                    return EditOutcome::Kept(rev); // played from the next play / song stop
                }
                let (live, mode) = self.doc_for_mode(self.mode);
                if mode != self.mode {
                    self.mode = mode; // a block that no longer exists → back to the loop
                    self.set_mode_status();
                    self.events.push(json!({ "type": "song", "mode": self.status.mode, "block": self.status.block }));
                }
                if optimize_changed {
                    self.push_control(&live, LiveToggle::default());
                }
                self.apply_live(live);
                EditOutcome::Queued(rev)
            }
        }
    }

    /// Send the current control (doc + flags + overrides) to the session (next loop top). The status
    /// says the mode now requested even while stopped (the next `play` starts under it).
    /// `toggle` names the panel switches this command flipped: they beat a song block's own control
    /// until the next block starts (ADR 0055 D3).
    fn push_control(&mut self, doc: &LoopDoc, toggle: LiveToggle) {
        let mut c = self.control_for(doc).unwrap_or_default();
        // A playing song's block may hold its own control: the status keeps what plays (from the
        // loop records) except for what this command switched.
        let song = matches!(self.mode, Mode::Song(_)) && self.session.is_some();
        if !song || toggle.optimize.is_some() {
            self.status.optimize = label(c.optimize).into();
        }
        if !song || toggle.orrery.is_some() {
            self.status.orrery = c.movement.orrery;
        }
        if let Some(s) = self.session.as_ref() {
            self.control_rev += 1;
            c.revision = self.control_rev;
            s.apply_live_control(c, toggle);
        }
    }

    /// Handle one protocol command (`contracts/engines_live_protocol_v1.md`). Returns `false` on `quit`.
    /// Every event answering the command echoes its `id` — the ones it emits now, and the later
    /// `applied` (of the revision a `doc` queued), `preview` and the `stopped` after `quit`.
    pub fn handle(&mut self, cmd: &Value) -> bool {
        let id = cmd.get("id").cloned().filter(|v| !v.is_null());
        let mark = self.events.len();
        let go_on = self.handle_cmd(cmd, id.clone());
        if let Some(id) = id {
            for e in &mut self.events[mark..] {
                if e.get("id").is_none_or(Value::is_null) {
                    e["id"] = id.clone();
                }
            }
            if !go_on {
                self.quit_id = Some(id);
            }
        }
        go_on
    }

    fn handle_cmd(&mut self, cmd: &Value, id: Option<Value>) -> bool {
        let err = |events: &mut Vec<Value>, m: String| events.push(json!({ "type": "error", "message": m, "id": id }));
        let Some(ty) = cmd.get("type").and_then(Value::as_str) else {
            err(&mut self.events, "a command needs a \"type\"".into());
            return true;
        };
        match ty {
            "doc" => match cmd.get("text").and_then(Value::as_str) {
                Some(text) => {
                    // Only an edit handed to the session gets an `applied` to echo the id on (a kept one doesn't).
                    let origin = history_source(cmd);
                    if let (EditOutcome::Queued(rev), Some(id)) = (self.apply_text(text, Source::Command, &origin, cmd.get("version").cloned()), id) {
                        self.pending_ids.insert(rev, id);
                    }
                }
                None => err(&mut self.events, "`doc` needs a \"text\"".into()),
            },
            // Already playing: a no-op (no event) — `restart` starts over.
            "play" => {
                if !self.live() {
                    self.play();
                }
            }
            "stop" => {
                self.stop_session();
                self.status.state = "stopped".into();
                self.events.push(json!({ "type": "stopped" }));
            }
            "restart" => self.play(),
            "mute" | "solo" => {
                let (Some(inst), Some(on)) = (cmd.get("instrument").and_then(Value::as_str), cmd.get("on").and_then(Value::as_bool)) else {
                    err(&mut self.events, format!("`{ty}` needs \"instrument\" and \"on\""));
                    return true;
                };
                // The instruments that are playing: while a song plays, a newer accepted doc waits
                // (ADR 0029) and may have removed one that's still sounding.
                if !self.live_doc.instruments.contains_key(inst) {
                    err(&mut self.events, format!("no instrument `{inst}` is playing"));
                    return true;
                }
                let set = if ty == "mute" { &mut self.mute } else { &mut self.solo };
                if on {
                    set.insert(inst.to_string());
                } else {
                    set.remove(inst);
                }
                self.apply_gains();
                self.events.push(json!({ "type": "gains", "muted": self.status.muted, "soloed": self.status.soloed }));
            }
            "clear_solo" => {
                self.solo.clear();
                self.apply_gains();
                self.events.push(json!({ "type": "gains", "muted": self.status.muted, "soloed": self.status.soloed }));
            }
            "control" => {
                let mut toggle = LiveToggle::default();
                if let Some(o) = cmd.get("optimize").and_then(Value::as_str) {
                    match parse_optimize(o) {
                        Some(m) => {
                            self.panel_optimize = Some(m);
                            toggle.optimize = Some(m);
                        }
                        None => {
                            err(&mut self.events, format!("unknown optimize `{o}` (hold | drift | live_edit)"));
                            return true;
                        }
                    }
                }
                if let Some(on) = cmd.get("orrery").and_then(Value::as_bool) {
                    self.panel_orrery = Some(on);
                    toggle.orrery = Some(on);
                }
                let live = self.live_doc.clone();
                self.push_control(&live, toggle);
                // Hold = play exactly what's written: re-apply the doc, dropping drift's wandering.
                if self.panel_optimize == Some(OptimizeMode::Hold) && !matches!(self.mode, Mode::Song(_)) {
                    if let Some(s) = self.session.as_ref() {
                        s.apply_doc_rev(live, self.status.rev);
                    }
                }
                self.events.push(json!({ "type": "control", "optimize": self.status.optimize, "orrery": self.status.orrery }));
            }
            "song" => self.handle_song(cmd, id),
            "preview" => {
                if self.preview.is_some() {
                    err(&mut self.events, "a preview is already rendering — wait for its `preview` event".into());
                    return true;
                }
                let (secs, stems) = preview_request(cmd);
                if let Err(why) = self.start_preview(secs, stems, id.clone()) {
                    err(&mut self.events, why);
                }
            }
            "limiter" => {
                if let Some(on) = cmd.get("on").and_then(Value::as_bool) {
                    self.limiter_on = on;
                }
                self.events.push(json!({ "type": "limiter", "on": self.limiter_on }));
            }
            "status" => self.events.push(self.ready_event()),
            "editor" => {
                let unsaved = cmd.get("unsaved").and_then(Value::as_bool).unwrap_or(false);
                self.status.editor = Some(EditorState { unsaved, at: unix_now() });
                self.last_status_write = Instant::now() - Duration::from_secs(10); // Claude reads it: write now
            }
            "quit" => return false,
            other => err(&mut self.events, format!("unknown command `{other}`")),
        }
        true
    }

    fn handle_song(&mut self, cmd: &Value, id: Option<Value>) {
        let action = cmd.get("action").and_then(Value::as_str).unwrap_or("");
        let blocks = self.doc.song.as_ref().map_or(0, |s| s.sequence.len());
        let block = cmd.get("block").and_then(Value::as_u64).map(|b| b as usize);
        let bad = |events: &mut Vec<Value>, m: String| events.push(json!({ "type": "error", "message": m, "id": id }));
        match action {
            "loop_block" | "play_from" => {
                let Some(k) = block.filter(|&k| k < blocks) else {
                    bad(&mut self.events, song_block_error(&format!("song {action}"), blocks));
                    return;
                };
                let was_song = matches!(self.mode, Mode::Song(_));
                self.mode = if action == "loop_block" { Mode::Block(k) } else { Mode::Song(k) };
                if action == "loop_block" && !was_song && self.live() {
                    // Stay in the same session: swap to the block's doc mid-loop, like an edit. The
                    // loop it lands in counts as the block's first.
                    let (live, _) = self.doc_for_mode(self.mode);
                    self.apply_live(live);
                    self.set_mode_status();
                    self.block_origin = Some(self.status.loop_id);
                } else {
                    self.play();
                }
            }
            "stop" => {
                let was_song = matches!(self.mode, Mode::Song(_));
                self.mode = Mode::Loop;
                if was_song || !self.live() {
                    self.play();
                } else {
                    let live = self.stripped();
                    self.apply_live(live);
                    self.set_mode_status();
                }
            }
            other => {
                bad(&mut self.events, format!("unknown song action `{other}` (loop_block | play_from | stop)"));
                return;
            }
        }
        self.events.push(json!({ "type": "song", "mode": self.status.mode, "block": self.status.block }));
    }

    /// Render `secs` of the playing doc offline (a background thread) into `.live/preview.wav` — and,
    /// with `stems`, one −18 LUFS stem per instrument + bus into `.live/preview_stems/` (ADR 0030 /
    /// 0040). `Err` (refused before rendering): a stems preview the disk can't hold.
    fn start_preview(&mut self, secs: f64, stems: bool, id: Option<Value>) -> Result<(), String> {
        let mut doc = self.live_doc.clone();
        doc.song = None;
        let control = self.control_for(&doc);
        let sr = self.opts.sample_rate;
        // Absolute: the host doesn't know engines-live's working directory.
        let live_dir = std::env::current_dir().map(|d| d.join(self.piece.live_dir())).unwrap_or_else(|_| self.piece.live_dir());
        let path = live_dir.join("preview.wav");
        let plan = if stems {
            let layout = engines_core::engine::source_layout(&doc);
            if layout.is_empty() {
                return Err("preview: the doc has no audio instruments or buses to render as stems".into());
            }
            let need = stems_preview_bytes(layout.len(), secs, sr);
            let _ = std::fs::create_dir_all(&live_dir);
            if let Some(free) = engines_core::stems::free_bytes(&live_dir) {
                if need > free {
                    return Err(format!("preview: {} stems × {secs:.0} s need ≈ {:.0} MB but only {:.0} MB is free — try fewer seconds", layout.len(), need as f64 / 1e6, free as f64 / 1e6));
                }
            }
            let names = engines_core::stems::assign_names(&layout);
            Some((layout, names))
        } else {
            None
        };
        let seed = self.opts.seed;
        let max_change = self.max_change;
        let (tx, rx) = mpsc::channel();
        self.preview = Some((rx, id));
        std::thread::spawn(move || {
            let ev = render_preview(doc, control, PreviewJob { secs, sr, seed, max_change, path, live_dir, stems: plan });
            if let Err(e) = &ev {
                eprintln!("preview: failed — {e}");
            }
            let _ = tx.send(ev.unwrap_or_else(|e| json!({ "type": "error", "message": format!("preview: {e}") })));
        });
        Ok(())
    }

    /// The `ready` event (also the answer to `status`).
    pub fn ready_event(&self) -> Value {
        let song = self.doc.song.as_ref().map(|s| {
            let blocks: Vec<Value> = (0..s.sequence.len())
                .map(|i| {
                    let mut b = json!({ "label": s.label(i), "loops": s.sequence[i].loops });
                    // What the block says about the Orrery (ADR 0055); absent ⇒ the session's.
                    if let Some(on) = s.sequence[i].control.as_ref().and_then(|c| c.orrery()) {
                        b["orrery"] = json!(on);
                    }
                    b
                })
                .collect();
            let from = if let Mode::Song(k) = self.mode { Some(k) } else { None };
            json!({ "blocks": blocks, "mode": self.status.mode, "block": self.status.block, "from": from, "current": self.current_block(), "tail_loops": s.tail_loops })
        });
        let note = self.live_doc.meta.as_ref().and_then(|m| m.get("note")).and_then(engines_core::doc_value::DocValue::as_str);
        json!({
            "type": "ready",
            "protocol": 1,
            // What a companion player adds: `["midi_mirror"]` from Studio's, `[]` here.
            "capabilities": self.notes.as_ref().map(|h| vec![h.capability()]).unwrap_or_default(),
            "piece": self.piece.slug,
            "rev": self.status.rev,
            "sample_rate": self.opts.sample_rate,
            "loop_s": self.status.loop_s,
            "master_steps": self.live_doc.transport.master_steps,
            "device": self.device,
            "limiter": self.limiter_on,
            "playing": self.live(),
            "optimize": self.status.optimize,
            "orrery": self.status.orrery,
            "instruments": self.instruments(),
            "muted": self.status.muted,
            "soloed": self.status.soloed,
            "song": song,
            "key": key_info(&self.live_doc),
            "note": note,
            "routing": self.status.routing,
        })
    }

    /// The block playing now: the looped one, or where the song is (`None` in loop mode).
    fn current_block(&self) -> Option<usize> {
        match self.mode {
            Mode::Loop => None,
            Mode::Block(k) => Some(k),
            Mode::Song(k) => Some(self.song_pos.map_or(k, |p| p.block as usize)),
        }
    }

    /// Where in its block loop `loop_id` is: the session's own song position (song mode), or — in
    /// block mode, where the session plays the block as a plain loop — counted from the loop the
    /// block started at, wrapping every `loops` (the block repeats forever).
    fn song_stamp(&mut self, loop_id: u64, session: Option<SongStamp>) -> Option<SongStamp> {
        match self.mode {
            Mode::Loop => None,
            Mode::Song(_) => session,
            Mode::Block(k) => {
                let loops = self.doc.song.as_ref().and_then(|s| s.sequence.get(k)).map_or(1, |b| b.loops.max(1) as u64);
                let origin = *self.block_origin.get_or_insert(loop_id);
                Some(SongStamp { block: k as u32, loop_in_block: (loop_id.saturating_sub(origin) % loops) as u32, block_loops: loops as u32, ..Default::default() })
            }
        }
    }

    /// Current step of every loop-synced step sequence at loop frame `pos` of `total`.
    fn steps(&self, pos: usize, total: usize) -> BTreeMap<String, u32> {
        let frac = if total > 0 { pos as f32 / total as f32 } else { 0.0 };
        self.live_doc
            .modulators
            .iter()
            .filter(|(_, m)| m.kind == "step_seq" && m.sync != "free")
            .map(|(id, m)| {
                let sd = engines_core::modulator::step_duration_frac(m).max(1e-6);
                (id.clone(), ((frac / sd) as u32) % m.length.max(1))
            })
            .collect()
    }

    /// Note device underruns (the shell's running total for the process) — an `underrun` event
    /// when it grows.
    pub fn note_underruns(&mut self, total: u64) {
        if total > self.status.underruns {
            self.events.push(json!({ "type": "underrun", "count": total }));
        }
        self.status.underruns = total;
    }

    /// Fold the device limiter's gain reduction since the shell's last read (dB ≤ 0; `None` with the
    /// limiter off) into the deepest of this tick and of this status interval — the shell reads its
    /// meter every pass, more often than either, so a short burst isn't missed.
    pub fn note_limiter(&mut self, gr_db: Option<f32>) {
        let deepest = |acc: Option<f32>| gr_db.map(|g| acc.map_or(g, |a| a.min(g)));
        self.acc.gr_db = deepest(self.acc.gr_db);
        self.status.limiter_gr_db = deepest(self.status.limiter_gr_db);
    }

    /// One housekeeping step: pick up a settled save on disk (unless paused), fold in the session's
    /// records / applied edits / blocks / telemetry, emit `tick` / `loop` events, and rewrite the
    /// status at most every `status_every`.
    pub fn tick(&mut self, now: Instant, status_every: Duration) {
        // ADR 0044: a rebuilt routing map only reaches the audio at the next play — say so once.
        if let (Some(path), Some(rs)) = (self.opts.route_sources.as_ref(), self.status.routing.as_mut()) {
            if !rs.stale && std::fs::metadata(path).and_then(|m| m.modified()).ok() != self.route_mtime {
                rs.stale = true;
                self.events.push(json!({ "type": "routing", "stale": true, "message": "the routing map changed — reopen the Live set, then restart to play the new routing" }));
            }
        }
        let paused = self.piece.live_dir().join("pause").exists();
        if !paused {
            if let Some(text) = self.watcher.poll(now) {
                self.apply_text(&text, Source::Disk, "disk", None);
            }
        }
        if let Some((rx, id)) = self.preview.as_ref() {
            if let Ok(mut ev) = rx.try_recv() {
                if let Some(id) = id {
                    ev["id"] = id.clone();
                }
                self.events.push(ev);
                self.preview = None;
            }
        }
        let was = self.status.state.clone();
        if self.status.state == "playing" || self.status.state == "paused" {
            self.status.state = if paused { "paused" } else { "playing" }.into();
        }
        let sr = self.opts.sample_rate;
        let mut died = None;
        // Drain the session first: folding it in needs `&mut self` (block-mode song positions).
        let polled = self.session.as_mut().map(|session| {
            let out = (session.poll_telemetry(), session.poll_records(), session.poll_applied(), session.poll_blocks());
            let _ = session.poll_midi(); // `type:"midi"` instruments aren't delivered by engines-live — keep the channel drained
            let taps = session.poll_note_tap();
            if let Some(h) = self.notes.as_mut() {
                h.push(self.epoch, taps);
            }
            out
        });
        if let Some((tele, records, applied, blocks)) = polled {
            for r in records {
                self.status.optimize = r.optimize.clone();
                // What played: a song block's control can switch the Orrery (ADR 0055).
                self.status.orrery = r.orrery.is_some();
                self.status.peak_dbfs = (r.peak > 0.0).then(|| db(r.peak));
                if r.doc_rev != self.status.applied_rev && self.status.applied_at_loop.is_none_or(|l| r.loop_id > l) {
                    self.status.applied_rev = r.doc_rev;
                    self.status.applied_at_loop = Some(r.loop_id);
                }
                let orr = r.orrery.as_ref(); // this loop's own chord, even when several loops drained at once
                let song = self.song_stamp(r.loop_id, r.song);
                self.events.push(json!({
                    "type": "loop", "loop": r.loop_id, "rev": r.doc_rev, "optimize": r.optimize, "orrery": orr.is_some(),
                    "chord": orr.map(|o| o.chord.clone()), "tension": orr.map(|o| o.tension), "peak_db": db(r.peak),
                    "voicing": orr.map(|o| json!({ "pad": o.pad, "bass": o.bass, "tones": o.tones })),
                    "song": song.map(song_json),
                }));
            }
            for a in applied {
                self.status.applied_rev = a.doc_rev;
                self.status.applied_at_loop = Some(a.loop_id);
                self.status.applied_at_s = Some(a.pos as f64 / sr);
                let kind = match a.kind {
                    EditKind::Value => "value",
                    EditKind::Structural => "structural",
                };
                self.status.applied_kind = Some(kind.into());
                let mut ev = json!({ "type": "applied", "rev": a.doc_rev, "loop": a.loop_id, "at_s": a.pos as f64 / sr, "abs_frame": a.abs_frame, "kind": kind, "xfade_s": a.xfade_frames as f64 / sr });
                // The `doc` that queued this revision (and any superseded before they landed).
                if let Some(id) = self.pending_ids.remove(&a.doc_rev) {
                    ev["id"] = id;
                }
                self.pending_ids.retain(|&rev, _| rev > a.doc_rev);
                self.events.push(ev);
            }
            for b in blocks {
                self.status.loop_id = b.loop_id;
                self.status.pos_s = b.pos as f64 / sr;
                if self.status.applied_rev == 0 {
                    self.status.applied_rev = self.status.rev; // the starting doc is playing
                    self.status.applied_at_loop = Some(b.loop_id);
                }
                if let Mode::Song(_) = self.mode {
                    self.song_pos = b.song.or(self.song_pos);
                }
                self.acc.song = self.song_stamp(b.loop_id, b.song);
                self.acc.loop_id = b.loop_id;
                self.acc.pos = b.pos + b.frames;
                self.acc.total = b.total;
                self.acc.master = self.acc.master.max(b.peak);
                self.acc.master_lr = [self.acc.master_lr[0].max(b.peak_lr[0]), self.acc.master_lr[1].max(b.peak_lr[1])];
                for (name, [l, r]) in b.meters {
                    let (dl, dr) = (db(l), db(r));
                    let d = dl.max(dr);
                    let m = self.status.meters.entry(name.clone()).or_insert(-120.0);
                    *m = m.max(d);
                    let a = self.acc.meters.entry(name.clone()).or_insert(-120.0);
                    *a = a.max(d);
                    let lr = self.acc.meters_lr.entry(name).or_insert([-120.0; 2]);
                    *lr = [lr[0].max(dl), lr[1].max(dr)];
                }
            }
            if let Some(t) = tele {
                self.status.chord = t.orrery.as_ref().map(|o| o.chord.clone());
                self.status.tension = t.orrery.as_ref().map(|o| o.tension);
            }
        }
        if let Some(session) = self.session.as_ref() {
            if session.worker_finished() && self.status.state != "stopped" && self.status.state != "finished" {
                match session.worker_failure() {
                    Some(why) => died = Some(why),
                    None => {
                        self.status.state = "finished".into();
                        self.events.push(json!({ "type": "finished", "mode": self.status.mode }));
                    }
                }
            }
        }
        // The render worker panicked: say so and stop — not `finished`, and not silence that claims
        // to be playing. `play` / `restart` start a fresh session.
        if let Some(why) = died {
            self.events.push(json!({ "type": "error", "message": format!("the render worker died: {why} — stopped (play or restart to start again)") }));
            self.stop_session();
            self.status.state = "stopped".into();
            self.events.push(json!({ "type": "stopped" }));
        }
        if self.session.is_some() && self.status.state != "finished" && now.duration_since(self.last_tick) >= self.opts.tick_every && self.acc.total > 0 {
            self.last_tick = now;
            let steps = self.steps(self.acc.pos, self.acc.total);
            let acc = std::mem::take(&mut self.acc);
            self.events.push(json!({
                "type": "tick", "loop": acc.loop_id, "pos_s": acc.pos as f64 / sr, "loop_s": acc.total as f64 / sr,
                "steps": steps, "meters": acc.meters, "master_db": db(acc.master), "gr_db": acc.gr_db,
                "meters_lr": acc.meters_lr, "master_lr": [db(acc.master_lr[0]), db(acc.master_lr[1])],
                "song": acc.song.map(song_json),
            }));
            self.acc.loop_id = acc.loop_id;
            self.acc.pos = acc.pos;
            self.acc.total = acc.total;
            self.acc.song = acc.song;
        }
        if was != self.status.state || now.duration_since(self.last_status_write) >= status_every {
            self.last_status_write = now;
            self.write_status();
            self.status.meters.clear();
            self.status.limiter_gr_db = None; // the next interval's deepest (the shell folds it in)
        }
    }

    pub fn write_status(&mut self) {
        self.status.updated_at = unix_now();
        let dir = self.piece.live_dir();
        if std::fs::create_dir_all(&dir).is_ok() {
            if let Ok(text) = serde_json::to_string_pretty(&self.status) {
                let _ = write_atomic(&dir.join("status.json"), (text + "\n").as_bytes());
            }
        }
    }

    /// Stop the session and record `stopped` (the shell drains the final events after).
    pub fn stop(&mut self) {
        self.stop_session();
        self.status.state = "stopped".into();
        let mut ev = json!({ "type": "stopped" });
        if let Some(id) = self.quit_id.take() {
            ev["id"] = id;
        }
        self.events.push(ev);
        self.write_status();
    }

    /// The playing doc's instruments in document order (`ready`, the shell's console lines).
    pub fn instruments(&self) -> Vec<String> {
        let mut out: Vec<String> = self.inst_order.iter().filter(|id| self.live_doc.instruments.contains_key(*id)).cloned().collect();
        // Any the text order missed (a doc built by hand) follow in id order.
        out.extend(self.live_doc.instruments.keys().filter(|id| !out.contains(id)).cloned().collect::<Vec<_>>());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"{
  schema_version: "0.9",
  transport: { bpm: 240, master_steps: 4 },
  harmony: { root: "D", mode: "dorian" },
  movement: { orrery: true },
  instruments: { pad: { params: { wave: "saw_soft", level_db: -10.46, sustain: 1.0 }, sends_db: { rv: -6.02 } } },
  modulators: { seq: { type: "step_seq", output: "note", length: 4, steps: [ { step: 0, voicing: { v1: { offset: 0 } } } ] } },
  matrix: { m: { source: "seq", dest: "instruments.pad.notes", kind: "note", voices: 1 } },
  buses: { rv: { return_gain_db: -12.04, effects: { verb: { type: "reverb", params: {} } } } },
}"#;

    /// A fresh piece `pieces/demo` in its own temp dir (removed by the caller).
    fn piece(name: &str) -> (std::path::PathBuf, Piece) {
        let base = std::env::temp_dir().join(format!("engines-live-player-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("pieces/demo")).unwrap();
        std::fs::write(base.join("pieces/demo/demo.json5"), DOC).unwrap();
        let piece = Piece::open(&base, "demo").unwrap();
        (base, piece)
    }

    #[test]
    fn an_unknown_optimize_flag_refuses_to_start() {
        let (base, piece) = piece("optimize");
        let history = piece.history_dir();
        let opts = LiveOptions { optimize: "bogus".into(), ..Default::default() };
        let err = LivePlayer::start(piece.clone(), &opts).err().expect("refused, not started without the doc's control");
        assert!(err.contains("unknown optimize mode 'bogus'"), "{err}");
        assert!(!history.exists(), "refused before anything is recorded");
        // A good flag layers over the doc's own control: its Orrery stays on.
        let mut p = LivePlayer::start(piece, &LiveOptions { optimize: "hold".into(), ..Default::default() }).unwrap();
        assert_eq!((p.status.optimize.as_str(), p.status.orrery), ("hold", true));
        p.stop();
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_fresh_session_carries_nothing_of_the_last_ones_tick() {
        let (base, piece) = piece("fresh");
        let mut p = LivePlayer::start(piece, &LiveOptions::default()).unwrap();
        p.acc = TickAcc { loop_id: 99, pos: 10, total: 100, master: 1.0, song: Some(SongStamp::default()), ..Default::default() };
        p.block_origin = Some(97);
        p.play();
        assert_eq!((p.acc.loop_id, p.acc.total, p.acc.master, p.acc.song, p.block_origin), (0, 0, 0.0, None, None), "a restart's first tick is the new session's");
        p.stop();
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_stems_preview_is_capped_shorter_than_a_mix() {
        assert_eq!(preview_request(&json!({})), (10.0, false));
        assert_eq!(preview_request(&json!({ "seconds": 1000 })), (600.0, false));
        assert_eq!(preview_request(&json!({ "seconds": 1000, "stems": true })), (300.0, true));
        assert_eq!(preview_request(&json!({ "seconds": 0.1, "stems": true })), (1.0, true));
    }

    #[test]
    fn a_stems_preview_budgets_for_the_mix_too() {
        // One second at 1 kHz, stereo: 3 sources × (4 + 3) bytes + the 4-byte float mix.
        assert_eq!(stems_preview_bytes(3, 1.0, 1000.0), 2000 * (3 * 7 + 4));
        assert_eq!(stems_preview_bytes(0, 1.0, 1000.0), 2000 * 4);
    }

    // ── ADR 0044: per-source routing ──────────────────────────────────────────────────────────

    fn routed(path: &Path, channels: usize) -> LiveOptions {
        LiveOptions { route_sources: Some(path.to_path_buf()), out_channels: channels, ..Default::default() }
    }

    fn doc_sources_of(text: &str) -> Vec<(SourceKey, String)> {
        let (doc, _) = accept(text).unwrap();
        doc_sources(text, &doc)
    }

    #[test]
    fn without_a_routing_map_the_docs_sources_play_in_order_from_1_2() {
        let (base, piece) = piece("route-fresh");
        let map = routing::path(&piece);
        let mut p = LivePlayer::start(piece, &routed(&map, 8)).unwrap();
        assert_eq!(p.session_mut().unwrap().channels(), 8);
        let rs = p.status.routing.clone().expect("routing reported");
        let pairs: Vec<(&str, &str)> = rs.pairs.iter().map(|q| (q.pair.as_str(), q.source.as_str())).collect();
        assert_eq!(pairs, [("1/2", "inst:pad"), ("3/4", "bus:rv")], "the reverb bus is a source too");
        assert_eq!((rs.overflow.as_str(), rs.map.as_deref(), rs.unrouted.len()), ("5/6", None, 0));
        assert!(rs.note.unwrap().contains("make als"));
        assert_eq!(p.ready_event()["routing"]["overflow"], "5/6");
        p.stop();
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn the_routing_map_drives_the_session_and_reports_a_new_source_as_unrouted() {
        let (base, piece) = piece("route-map");
        let map_path = routing::path(&piece);
        let (map, _) = routing::assign("demo", None, &doc_sources_of(DOC), 8, routing::Policy::Grow).unwrap();
        routing::write(&map_path, &map).unwrap();
        let mut p = LivePlayer::start(piece, &routed(&map_path, 16)).unwrap();
        assert_eq!(p.session_mut().unwrap().channels(), 16, "a wider device: the extra pairs stay silent");
        assert_eq!(p.status.routing.as_ref().unwrap().map.as_deref(), Some(map_path.display().to_string().as_str()));

        let text = DOC.replace("instruments: { pad:", "instruments: { lead: { params: { wave: \"sine\", level_db: -13.98 } }, pad:");
        assert!(matches!(p.apply_text(&text, Source::Command, "editor", None), EditOutcome::Queued(_)));
        assert_eq!(p.status.routing.as_ref().unwrap().unrouted, ["inst:lead"], "it plays on the overflow until `make als`");
        p.stop();
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn switching_the_output_restarts_on_the_new_width_and_keeps_the_mix_settings() {
        let (base, piece) = piece("switch");
        let map_path = routing::path(&piece);
        let (map, _) = routing::assign("demo", None, &doc_sources_of(DOC), 8, routing::Policy::Grow).unwrap();
        routing::write(&map_path, &map).unwrap();
        let mut p = LivePlayer::start(piece, &LiveOptions::default()).unwrap();
        assert!(p.handle(&json!({ "type": "mute", "instrument": "pad", "on": true })));
        let epoch = p.epoch;
        p.events.clear();

        // Speakers → the loopback, each source on its pair.
        assert_eq!(p.check_output(16, Some(&map_path)), Ok(()));
        p.set_output(44_100.0, 16, Some("BlackHole 16ch".into()), Some(map_path.clone()));
        assert_eq!(p.epoch, epoch + 1, "a new session for the device shell to attach");
        assert_eq!(p.session_mut().unwrap().channels(), 16);
        assert!(p.status.routing.is_some());
        assert_eq!(p.status.muted, ["pad"], "mutes carry over");
        let ready = p.events.iter().find(|e| e["type"] == "ready").expect("a fresh ready");
        assert_eq!((ready["device"].as_str(), ready["sample_rate"].as_f64(), ready["routing"].is_object()), (Some("BlackHole 16ch"), Some(44_100.0), true));

        // → headphones: plain stereo, no routing.
        p.set_output(48_000.0, 2, Some("Headphones".into()), None);
        assert_eq!(p.session_mut().unwrap().channels(), 2);
        assert!(p.status.routing.is_none());

        // A routing the device can't carry is refused up front.
        assert!(p.check_output(4, Some(&map_path)).unwrap_err().contains("needs 8 channels"));

        // Stopped: the switch waits for the next play.
        assert!(p.handle(&json!({ "type": "stop" })));
        p.set_output(48_000.0, 16, None, Some(map_path));
        assert!(p.session_mut().is_none(), "a stopped player stays stopped");
        assert!(p.handle(&json!({ "type": "play" })));
        assert_eq!(p.session_mut().unwrap().channels(), 16);
        p.stop();
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_routing_map_wider_than_the_device_refuses_to_play() {
        let (base, piece) = piece("route-wide");
        let map_path = routing::path(&piece);
        let (map, _) = routing::assign("demo", None, &doc_sources_of(DOC), 16, routing::Policy::Grow).unwrap();
        routing::write(&map_path, &map).unwrap();
        let mut p = LivePlayer::start(piece, &routed(&map_path, 8)).unwrap();
        assert!(p.session_mut().is_none());
        assert_eq!(p.status.state, "stopped");
        let err = p.events.iter().find(|e| e["type"] == "error").expect("an error event");
        assert!(err["message"].as_str().unwrap().contains("needs 16 channels"), "{err}");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_restart_on_a_changed_map_tells_the_client_the_new_routing() {
        // The Live set was rebuilt (here: the pad's track renamed), then restarted: the panel only
        // learns routing from events, so the restart must send it. (With Studio's player a MIDI switch
        // does the same; `engines_mirror`'s tests.)
        let (base, piece) = piece("route-update");
        let map_path = routing::path(&piece);
        let (map, _) = routing::assign("demo", None, &doc_sources_of(DOC), 8, routing::Policy::Grow).unwrap();
        routing::write(&map_path, &map).unwrap();
        let mut p = LivePlayer::start(piece, &routed(&map_path, 8)).unwrap();
        assert_eq!(p.ready_event()["routing"]["pairs"][0]["name"], "pad");
        std::thread::sleep(Duration::from_millis(20));
        let mut renamed = map.clone();
        renamed.slots[0].name = "pad dry".into();
        routing::write(&map_path, &renamed).unwrap();
        p.tick(Instant::now(), Duration::from_secs(60)); // stale, said once
        p.events.clear();
        assert!(p.handle(&json!({ "type": "restart" })));
        let ev: Vec<&Value> = p.events.iter().filter(|e| e["type"] == "routing").collect();
        assert_eq!(ev.len(), 1, "{:?}", p.events);
        assert_eq!((ev[0]["stale"].as_bool(), ev[0]["routing"]["pairs"][0]["name"].as_str()), (Some(false), Some("pad dry")));
        // The same map again: nothing new to say.
        p.events.clear();
        assert!(p.handle(&json!({ "type": "restart" })));
        assert!(p.events.iter().all(|e| e["type"] != "routing"));
        p.stop();
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_rebuilt_routing_map_marks_the_routing_stale_once() {
        let (base, piece) = piece("route-stale");
        let map_path = routing::path(&piece);
        let (map, _) = routing::assign("demo", None, &doc_sources_of(DOC), 8, routing::Policy::Grow).unwrap();
        routing::write(&map_path, &map).unwrap();
        let mut p = LivePlayer::start(piece, &routed(&map_path, 8)).unwrap();
        p.tick(Instant::now(), Duration::from_secs(60));
        assert!(!p.status.routing.as_ref().unwrap().stale);
        std::thread::sleep(Duration::from_millis(20));
        let mut rebuilt = map.clone();
        rebuilt.extra.insert("template".into(), "t.als".into());
        routing::write(&map_path, &rebuilt).unwrap();
        p.events.clear();
        p.tick(Instant::now(), Duration::from_secs(60));
        p.tick(Instant::now(), Duration::from_secs(60));
        assert!(p.status.routing.as_ref().unwrap().stale);
        assert_eq!(p.events.iter().filter(|e| e["type"] == "routing").count(), 1, "said once");
        // A restart picks the new map up.
        p.play();
        assert!(!p.status.routing.as_ref().unwrap().stale);
        p.stop();
        let _ = std::fs::remove_dir_all(base);
    }

    // ── The free player taps no notes ────────────────────────────────────────────────────────
    // (The MIDI mirror's own tests, with its hook, are in the closed `engines_mirror`.)

    #[test]
    fn the_free_player_reports_no_capabilities_and_no_midi_even_when_the_map_switches_it_on() {
        let (base, piece) = piece("free");
        let map_path = routing::path(&piece);
        let (mut map, _) = routing::assign("demo", None, &doc_sources_of(DOC), 8, routing::Policy::Grow).unwrap();
        // As Studio's Live set builder records a MIDI switch.
        map.slots[0].extra.insert("midi".into(), json!({ "enabled": true, "port": "Engines Out", "channel": 1 }));
        routing::write(&map_path, &map).unwrap();
        let mut p = LivePlayer::start(piece, &routed(&map_path, 8)).unwrap();
        let ready = p.ready_event();
        assert_eq!(ready["capabilities"], json!([]));
        let routing = &ready["routing"];
        assert!(routing.get("midi").is_none(), "{routing}");
        for pair in routing["pairs"].as_array().unwrap() {
            assert!(pair.get("midi").is_none() && pair.get("no_midi").is_none(), "{pair}");
        }
        assert!(p.taps.is_empty(), "nothing is tapped");
        p.stop();
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn the_instrument_order_is_the_docs() {
        assert_eq!(instrument_order("{ instruments: { sub: {}, crank: {}, 'partials': {}, sub: {} } }"), ["sub", "crank", "partials"]);
        assert!(instrument_order("{ global: {} }").is_empty());
        assert!(instrument_order("not json5").is_empty());
    }

    #[test]
    fn a_history_source_is_a_short_label_or_editor() {
        for (cmd, want) in [
            (json!({}), "editor"),
            (json!({ "source": "revert" }), "revert"),
            (json!({ "source": "claude-code_2" }), "claude-code_2"),
            (json!({ "source": "Upper" }), "editor"),
            (json!({ "source": "" }), "editor"),
            (json!({ "source": "x".repeat(33) }), "editor"),
            (json!({ "source": 7 }), "editor"),
        ] {
            assert_eq!(history_source(&cmd), want, "{cmd}");
        }
    }

    #[test]
    fn the_limiter_meter_keeps_the_deepest_reduction_of_each_tick_and_status_interval() {
        let (base, piece) = piece("limiter");
        let mut p = LivePlayer::start(piece, &LiveOptions::default()).unwrap();
        // The shell reads its meter every pass (more often than a tick): a short burst between
        // ticks isn't lost.
        for gr in [-0.5, -6.0, -1.0] {
            p.note_limiter(Some(gr));
        }
        assert_eq!((p.acc.gr_db, p.status.limiter_gr_db), (Some(-6.0), Some(-6.0)));
        let t0 = Instant::now() + Duration::from_secs(1);
        p.acc.total = p.acc.total.max(1); // as if a block had played
        p.events.clear();
        p.tick(t0, Duration::from_secs(3600));
        let tick = p.events.iter().find(|e| e["type"] == "tick").expect("a tick").clone();
        assert_eq!(tick["gr_db"], json!(-6.0));
        // The tick consumed its own; the status interval keeps the deepest until it's written.
        p.note_limiter(Some(-2.0));
        assert_eq!((p.acc.gr_db, p.status.limiter_gr_db), (Some(-2.0), Some(-6.0)));
        p.tick(t0 + Duration::from_secs(1), Duration::ZERO);
        let written: Value = serde_json::from_str(&std::fs::read_to_string(p.piece.live_dir().join("status.json")).unwrap()).unwrap();
        assert_eq!(written["limiter_gr_db"], json!(-6.0));
        assert_eq!(p.status.limiter_gr_db, None, "the next interval starts afresh");
        // The limiter off: nothing to report.
        p.note_limiter(None);
        assert_eq!((p.acc.gr_db, p.status.limiter_gr_db), (None, None));
        p.stop();
        let _ = std::fs::remove_dir_all(base);
    }
}
