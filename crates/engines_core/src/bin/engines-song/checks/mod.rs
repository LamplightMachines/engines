//! The render checks: `engines-song verify | seams | voyage`. Each renders the doc
//! in memory — no WAV unless `--out`, no scratch files, no `engines-render`, no Python — and prints
//! the report the ambient-loop-doc Python scripts it replaces printed (`--json`: the same as one
//! object). The verdicts and thresholds are the scripts' own.
//!
//! - `verify` ([`verify`]): `verify_render.py` + `analyze_center.py` + `ab_modulators.py`
//!   (`--ab`) + `song_blocks.py` (a song doc's per-block table).
//! - `seams` ([`seams`]): `scale_song.py` + `song_seams.py`.
//! - `voyage` ([`voyage`]): `orrery_voyage.py`, on the controller run headless.
//!
//! Exit codes: 0 a pass (with or without warnings), 1 a FAIL verdict, 2 a usage error, an
//! unreadable doc or a render that couldn't run. Desktop-only measurement (D1): it lives in the bin, never in the core library.

mod dsp;
mod seams;
mod verify;
mod voyage;

use std::io;
use std::process::ExitCode;

use engines_core::contract::{Movement, SessionControl};
use engines_core::controller::{DitherConfig, DEFAULT_MAX_CHANGE};
use engines_core::engine::{Engine, PhaseSnapshot};
use engines_core::loop_doc::LoopDoc;
use engines_core::session::{Session, SessionConfig, SessionOptions};
use engines_core::song::fmt_hms;
use serde::Serialize;

use crate::{Fail, FrameSink};

/// `engines-song`'s checking subcommands.
#[derive(clap::Subcommand, Debug, PartialEq)]
pub enum Check {
    /// Render a loop doc (2 loops, in memory) and measure it: level and peak, the band split, the
    /// centroid's slow trend and rhythmic residual, onsets and transient crest, per-beat energy; the
    /// centre's harmonicity and pulse when the doc has one; each route's audibility (--ab); and on
    /// a song doc a row per block as the controller starts it
    Verify(verify::VerifyArgs),
    /// Render a song scaled down (every block kept, fewer loops) with drift and the Orrery, and
    /// judge it: duration, peak and clips, each block's level and creep, each seam's loudness,
    /// brightness and transient steps against the blocks' own movement, the tail's ring-out
    Seams(seams::SeamsArgs),
    /// Run the Orrery headless (20 loops, or a song scaled down) and tabulate its voyage: chord and
    /// tension per loop, the distinct chords, the longest excursion, tamed or roaming — per block
    /// on a song
    Voyage(voyage::VoyageArgs),
}

/// What every check takes.
#[derive(clap::Args, Debug, PartialEq)]
pub struct Common {
    /// Loop document (JSON5, schema v0.9), with or without a `song`
    pub doc: String,
    /// Print the report as one JSON object instead of text
    #[arg(long)]
    pub json: bool,
    /// Session seed for the controller's renders (engines-song's default)
    #[arg(long, default_value_t = 0x5EED)]
    pub seed: u64,
    /// Render sample rate in Hz
    #[arg(long, default_value_t = 48_000.0)]
    pub sample_rate: f64,
}

/// A song shrunk for a check (`scale_song.py`): every block kept, `loops` scaled so the longest is
/// `--longest`, none under `--min`.
#[derive(clap::Args, Debug, PartialEq)]
pub struct Scale {
    /// A song: the longest block's loops in the scaled-down copy (the rest in proportion)
    #[arg(long, default_value_t = 12, value_parser = clap::value_parser!(u32).range(1..))]
    pub longest: u32,
    /// A song: no block of the scaled-down copy is shorter than this
    #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u32).range(1..))]
    pub min: u32,
    /// A song: play it at full length instead of scaled down
    #[arg(long, conflicts_with_all = ["longest", "min"])]
    pub full: bool,
}

/// The FAIL and WARN lines a check collected, in order.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Findings {
    pub fails: Vec<String>,
    pub warns: Vec<String>,
}

impl Findings {
    /// `FAIL`, or `pass` (`… with warnings` when there are any).
    pub fn verdict(&self, pass: &str) -> String {
        if !self.fails.is_empty() {
            "FAIL".into()
        } else if self.warns.is_empty() {
            pass.into()
        } else {
            format!("{pass} with warnings")
        }
    }

    /// The `FAIL:` / `WARN:` lines, as the scripts print them.
    pub fn print(&self) {
        for f in &self.fails {
            println!("FAIL: {f}");
        }
        for w in &self.warns {
            println!("WARN: {w}");
        }
    }
}

/// Run a check; the exit code follows its verdict.
pub fn run(check: Check) -> ExitCode {
    let out = match check {
        Check::Verify(a) => verify::run(a),
        Check::Seams(a) => seams::run(a),
        Check::Voyage(a) => voyage::run(a),
    };
    // 1 is a FAIL verdict and nothing else: a check that couldn't run (a bad flag, an unreadable
    // or refused doc, a render or write that failed) exits 2.
    match out {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(Fail::Usage(m) | Fail::Runtime(m)) => {
            eprintln!("error: {m}");
            ExitCode::from(2)
        }
    }
}

/// Print a report: JSON, or the text the closure writes.
pub fn emit<T: Serialize>(json: bool, report: &T, text: impl FnOnce(&T)) -> Result<(), Fail> {
    if json {
        let s = serde_json::to_string_pretty(report).map_err(|e| Fail::Runtime(e.to_string()))?;
        println!("{s}");
    } else {
        text(report);
    }
    Ok(())
}

/// Read and check a doc; an unreadable or refused doc is a usage error (exit 2). Returns the doc
/// and the engine's lints (`warning:` lines on a render).
pub fn load(path: &str) -> Result<(LoopDoc, Vec<String>), Fail> {
    let text = std::fs::read_to_string(path).map_err(|e| Fail::Usage(format!("{path}: {e}")))?;
    let doc = LoopDoc::from_json_str(&text).map_err(|e| Fail::Usage(format!("{path}: {e}")))?;
    let lints = doc.lint();
    Ok((doc, lints))
}

/// A doc changed in memory, written out and read back exactly as a file of it would be (what
/// the scripts did by writing a JSON copy for `engines-render`).
pub fn reparse(doc: &LoopDoc) -> Result<LoopDoc, Fail> {
    let text = engines_core::doc_value::to_string(doc).map_err(|e| Fail::Runtime(e.to_string()))?;
    LoopDoc::from_json_str(&text).map_err(|e| Fail::Runtime(format!("the changed doc: {e}")))
}

/// Seconds per loop as the scripts compute it (`master_steps · 60 / bpm`, in that order).
pub fn loop_seconds(doc: &LoopDoc) -> (f64, f64) {
    let beat = 60.0 / doc.transport.bpm as f64;
    (beat, doc.transport.master_steps as f64 * beat)
}

/// `engines-render`: `loops` loops of the doc's top-level matrix through one engine (no
/// controller), interleaved stereo.
pub fn raw_render(doc: &LoopDoc, loops: usize, sr: f64) -> Vec<f32> {
    let mut doc = doc.clone();
    doc.take_song();
    let mut eng = Engine::new(sr);
    let mut snap = PhaseSnapshot::empty();
    let mut audio = Vec::with_capacity(doc.total_frames(sr) * 2 * loops);
    for _ in 0..loops {
        let r = eng.render(&doc, &snap);
        audio.extend_from_slice(&r.audio);
        snap = r.next_snapshot;
    }
    audio
}

/// The doc's own control (ADR 0033) with engines-song's `--optimize` / `--orrery` flags over it,
/// and the doc's max per-loop change.
pub fn control(doc: &LoopDoc, optimize: &str, orrery: bool) -> Result<(f32, Option<SessionControl>), Fail> {
    let max_change = doc.max_change.unwrap_or(DEFAULT_MAX_CHANGE);
    let c = SessionControl::with_flags(SessionControl::from_doc(doc), optimize, orrery, &[], &[], max_change).map_err(|e| Fail::Usage(format!("control: {e}")))?;
    Ok((max_change, c))
}

/// [`control`] with the Orrery at its default movement (a chord change every loop): the worst
/// case a `context` floor must hold.
pub fn default_movement(c: Option<SessionControl>) -> Option<SessionControl> {
    c.map(|mut c| {
        c.movement = Movement { orrery: true, ..Movement::default() };
        c
    })
}

/// Play the doc through the live Session path (`engines-song --doc … --out …`) into `sink`:
/// a song plays its blocks and tail; a song-less doc plays `max_loops`.
pub fn session_render(doc: LoopDoc, control: Option<SessionControl>, max_change: f32, seed: u64, max_loops: Option<usize>, sr: f64, sink: &mut dyn FrameSink, label: &str) -> Result<(), Fail> {
    let frames_planned = match &doc.song {
        Some(s) => (s.body_loops() + s.tail_loops as usize) as u64 * doc.total_frames(sr) as u64,
        None => max_loops.unwrap_or(0) as u64 * doc.total_frames(sr) as u64,
    };
    let session = Session::start_with(
        doc,
        SessionConfig { sample_rate: sr, max_loops, ..Default::default() },
        DitherConfig { max_change_per_loop: max_change, seed, ..Default::default() },
        control,
        SessionOptions::default(),
    )
    .map_err(Fail::Runtime)?;
    crate::drain(session, sink, &mut None, None, label, sr, frames_planned, None)?;
    Ok(())
}

/// A sink that keeps every frame (short renders: a block start, 2 loops).
#[derive(Default)]
pub struct Collect(pub Vec<f32>);

impl FrameSink for Collect {
    fn write_frames(&mut self, frames: &[f32]) -> io::Result<()> {
        self.0.extend_from_slice(frames);
        Ok(())
    }
    fn checkpoint(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Interleaved stereo → mono (the channel mean), in f64.
pub fn mono(stereo: &[f32]) -> Vec<f64> {
    stereo.chunks_exact(2).map(|f| (f[0] as f64 + f[1] as f64) / 2.0).collect()
}

/// The largest `|sample|` over both channels.
pub fn peak(stereo: &[f32]) -> f64 {
    stereo.iter().fold(0.0f64, |m, &s| m.max((s as f64).abs()))
}

/// A song scaled down for a check (`scale_song.py`): each block's loops shrunk in proportion so
/// the longest is `longest`, none under `min` (only ever shrunk); `transition_s` clamped to the
/// shortest scaled block. `None` for a song-less doc.
pub fn scale_song(doc: &LoopDoc, s: &Scale) -> Option<(LoopDoc, Scaled)> {
    let spec = doc.song.as_ref()?;
    let mut out = doc.clone();
    let song = out.song.as_mut()?;
    let (_, loop_s) = loop_seconds(doc);
    let authored: Vec<u32> = spec.sequence.iter().map(|b| b.loops).collect();
    let k = s.longest as f64 / authored.iter().copied().max().unwrap_or(1).max(1) as f64;
    if !s.full && k < 1.0 {
        for (b, &a) in song.sequence.iter_mut().zip(&authored) {
            // Python's round(): ties to even.
            b.loops = s.min.max((a as f64 * k).round_ties_even() as u32);
        }
    }
    let shortest_s = song.sequence.iter().map(|b| b.loops).min().unwrap_or(1) as f64 * loop_s;
    let authored_transition = song.transition_s;
    let clamped = song.transition_s as f64 > shortest_s;
    if clamped {
        song.transition_s = ((shortest_s * 1000.0).round_ties_even() / 1000.0) as f32;
    }
    let mut start = 0.0;
    let blocks = spec
        .sequence
        .iter()
        .zip(&song.sequence)
        .enumerate()
        .map(|(i, (a, b))| {
            let row = ScaledBlock { idx: i, label: spec.label(i), layer: a.matrix.clone(), loops_authored: a.loops, loops: b.loops, start_s: start };
            start += b.loops as f64 * loop_s;
            row
        })
        .collect();
    let tail = song.tail_loops;
    let scaled = Scaled {
        full: s.full || k >= 1.0,
        longest: s.longest,
        min: s.min,
        loop_s,
        transition_s: song.transition_s,
        transition_authored_s: authored_transition,
        transition_clamped: clamped,
        tail_loops: tail,
        blocks,
        total_s: start + tail as f64 * loop_s,
        authored_s: (authored.iter().map(|&l| l as u64).sum::<u64>() + tail as u64) as f64 * loop_s,
    };
    Some((out, scaled))
}

/// What [`scale_song`] did, for the report.
#[derive(Clone, Debug, Serialize)]
pub struct Scaled {
    /// Played at its authored length (`--full`, or already within `--longest`).
    pub full: bool,
    pub longest: u32,
    pub min: u32,
    pub loop_s: f64,
    pub transition_s: f32,
    pub transition_authored_s: f32,
    pub transition_clamped: bool,
    pub tail_loops: u32,
    pub blocks: Vec<ScaledBlock>,
    pub total_s: f64,
    pub authored_s: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ScaledBlock {
    pub idx: usize,
    pub label: String,
    pub layer: Option<String>,
    pub loops_authored: u32,
    pub loops: u32,
    pub start_s: f64,
}

impl Scaled {
    /// `scale_song.py`'s table.
    pub fn print(&self, doc: &str) {
        if self.full {
            println!("{doc}: the song at its authored length ({} blocks + {} tail loop(s), {})", self.blocks.len(), self.tail_loops, fmt_hms(self.total_s));
            return;
        }
        if self.transition_clamped {
            println!(
                "note: transition_s {} > the shortest scaled block ({:.1} s) — clamped (raise --min to keep it)",
                self.transition_authored_s,
                self.blocks.iter().map(|b| b.loops).min().unwrap_or(0) as f64 * self.loop_s
            );
        }
        println!("{doc} scaled down   (loop = {:.3} s; seam {} s; tail {} loops; longest → {}, none under {})", self.loop_s, self.transition_s, self.tail_loops, self.longest, self.min);
        println!("  {:>2}  {:<14} {:<10} {:>13}  {:>9}", "#", "label", "layer", "loops", "starts at");
        for b in &self.blocks {
            let layer = b.layer.as_deref().unwrap_or("—");
            println!("  {:>2}  {:<14} {:<10} {:>5} → {:<5}  {:>9}", b.idx, b.label, layer, b.loops_authored, b.loops, fmt_hms(b.start_s));
        }
        let tail_start = self.total_s - self.tail_loops as f64 * self.loop_s;
        println!("      {:<14} {:<10} {:>13}  {:>9}", "tail", "", self.tail_loops, fmt_hms(tail_start));
        println!("scaled total {} (authored {})", fmt_hms(self.total_s), fmt_hms(self.authored_s));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Args;
    use clap::Parser;

    #[test]
    fn subcommands_parse_and_the_render_flags_still_do() {
        let a = Args::parse_from(["engines-song", "verify", "d.json5", "--ab", "--json"]);
        match a.check {
            Some(Check::Verify(v)) => {
                assert_eq!((v.common.doc.as_str(), v.loops, v.ab, v.common.json, v.common.seed), ("d.json5", 2, true, true, 0x5EED));
            }
            other => panic!("{other:?}"),
        }
        let a = Args::parse_from(["engines-song", "seams", "s.json5", "--longest", "8"]);
        assert!(matches!(a.check, Some(Check::Seams(ref s)) if s.scale.longest == 8 && s.scale.min == 4));
        let a = Args::parse_from(["engines-song", "voyage", "d.json5", "--movement", "default"]);
        assert!(matches!(a.check, Some(Check::Voyage(ref v)) if v.loops == 20 && v.movement == voyage::MovementArg::Default));
        // The render command is unchanged, and the two don't mix.
        let a = Args::parse_from(["engines-song", "--doc", "d.json5", "--out", "o.wav"]);
        assert!(a.check.is_none() && a.doc == "d.json5");
        assert!(Args::try_parse_from(["engines-song", "--doc", "d.json5", "verify", "d.json5"]).is_err());
        assert!(Args::try_parse_from(["engines-song", "seams", "s.json5", "--full", "--longest", "3"]).is_err());
        assert!(Args::try_parse_from(["engines-song", "verify"]).is_err(), "the doc is required");
    }

    #[test]
    fn findings_name_the_verdict() {
        let mut f = Findings::default();
        assert_eq!(f.verdict("CLEAN"), "CLEAN");
        f.warns.push("w".into());
        assert_eq!(f.verdict("CLEAN"), "CLEAN with warnings");
        f.fails.push("x".into());
        assert_eq!(f.verdict("CLEAN"), "FAIL");
    }
}
