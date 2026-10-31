//! `engines-song seams <song doc>`: the whole form, scaled down, judged. It replaces
//! `scale_song.py` (every block kept, `loops` shrunk in proportion — unlike `--max-loops`, which
//! cuts the song short) and `song_seams.py` (peak and clips, each block's level and creep, each
//! seam's loudness / brightness / transient steps against the blocks' own movement, the tail's
//! ring-out), with their thresholds.
//!
//! The copy plays as it will be heard — drift and the Orrery over the doc's own control, through
//! the Session — and streams through [`SeamMeter`], so even `--full` holds only per-hop measures.

use std::fs::File;
use std::io;
use std::path::Path;

use engines_core::loop_doc::LoopDoc;
use engines_core::song::fmt_hms;
use engines_core::wav::{SampleFormat, WavWriter};
use serde::Serialize;

use super::dsp::{self, db, Band, Conv, Rfft, Sos};
use super::{control, load, scale_song, session_render, Common, Findings, Scale, Scaled};
use crate::{Fail, FrameSink};

#[derive(clap::Args, Debug, PartialEq)]
pub struct SeamsArgs {
    #[command(flatten)]
    pub common: Common,
    #[command(flatten)]
    pub scale: Scale,
    /// Also write the scaled-down render to this file (one 32-bit float WAV)
    #[arg(long, value_name = "FILE")]
    pub out: Option<String>,
}

pub fn run(a: SeamsArgs) -> Result<bool, Fail> {
    let (doc, _) = load(&a.common.doc)?;
    let Some((scaled_doc, scaled)) = scale_song(&doc, &a.scale) else {
        return Err(Fail::Usage(format!("{} has no `song` — this check is for songs (ADR 0029)", a.common.doc)));
    };
    let sr = a.common.sample_rate;
    let plan = Plan::of(&scaled_doc, sr);
    let out = match &a.out {
        Some(p) => Some(WavWriter::create(Path::new(p), 2, sr as u32, SampleFormat::F32).map_err(|e| Fail::Runtime(format!("{p}: {e}")))?),
        None => None,
    };
    let mut meter = SeamMeter::new(sr, out);
    let (max_change, ctl) = control(&scaled_doc, "drift", true)?;
    eprintln!("seams: rendering {} with drift and the Orrery …", if scaled.full { "the song" } else { "the song scaled down" });
    session_render(scaled_doc, ctl, max_change, a.common.seed, None, sr, &mut meter, "the scaled song")?;
    let measured = meter.finish().map_err(|e| Fail::Runtime(format!("{}: {e}", a.out.as_deref().unwrap_or("the render"))))?;
    if let Some(p) = &a.out {
        eprintln!("wrote {p}");
    }
    let report = judge(&a.common.doc, Some(scaled), &plan, &measured);
    super::emit(a.common.json, &report, print)?;
    Ok(report.findings.fails.is_empty())
}

/// The song's shape in frames (from the doc that was rendered).
#[derive(Clone, Debug)]
pub struct Plan {
    pub sr: f64,
    /// Frames per loop (`LoopDoc::total_frames`).
    pub loop_frames: usize,
    pub transition_s: f64,
    pub tail_loops: usize,
    /// `(label, layer, loops)` per block.
    pub blocks: Vec<(String, Option<String>, usize)>,
}

impl Plan {
    pub fn of(doc: &LoopDoc, sr: f64) -> Self {
        let spec = doc.song.as_ref().expect("a song doc");
        Plan {
            sr,
            loop_frames: doc.total_frames(sr),
            transition_s: spec.transition_s as f64,
            tail_loops: spec.tail_loops as usize,
            blocks: spec.sequence.iter().enumerate().map(|(i, b)| (spec.label(i), b.matrix.clone(), b.loops as usize)).collect(),
        }
    }

    /// Block starts … the tail's start, in frames.
    fn starts(&self) -> Vec<usize> {
        let mut s = vec![0];
        for (_, _, loops) in &self.blocks {
            s.push(s.last().copied().unwrap_or(0) + loops * self.loop_frames);
        }
        s
    }
}

/// Hop length (0.1 s) and the transient chunk (3 ms) at `sr`.
fn hop_sizes(sr: f64) -> (usize, usize) {
    ((0.1 * sr) as usize, (0.003 * sr) as usize)
}

/// One streaming pass over the render: per 0.1-s hop its energy, peak and spectral centroid, per
/// 3-ms chunk the > 4 kHz energy, the clips, and the last 0.5 s. What `song_seams.py` read from the
/// file, measured as the frames arrive.
pub struct SeamMeter {
    sr: f64,
    h: usize,
    cw: usize,
    hop: Vec<f64>,
    hop_peak: f64,
    fft: Rfft,
    win: Vec<f64>,
    fq: Vec<f64>,
    mag: Vec<f64>,
    hf: Sos,
    chunk: (f64, usize),
    end: std::collections::VecDeque<[f32; 2]>,
    m: Measured,
    out: Option<WavWriter<File>>,
}

/// The per-hop and per-chunk measures of a render.
#[derive(Clone, Debug, Default)]
pub struct Measured {
    pub frames: usize,
    pub hop_ss: Vec<f64>,
    pub hop_pk: Vec<f64>,
    pub cen: Vec<f64>,
    pub hf_ss: Vec<f64>,
    pub clipped: u64,
    pub peak: f64,
    /// Mean square of the last 0.5 s, both channels.
    pub end_ms: f64,
}

impl SeamMeter {
    pub fn new(sr: f64, out: Option<WavWriter<File>>) -> Self {
        let (h, cw) = hop_sizes(sr);
        SeamMeter {
            sr,
            h,
            cw,
            hop: Vec::with_capacity(h),
            hop_peak: 0.0,
            fft: Rfft::new(h),
            win: dsp::hanning(h),
            fq: dsp::rfftfreq(h, sr),
            mag: vec![0.0; h / 2 + 1],
            hf: Sos::butter(4, Band::High(4000.0), sr),
            chunk: (0.0, 0),
            end: std::collections::VecDeque::with_capacity(sr as usize / 2 + 1),
            m: Measured::default(),
            out,
        }
    }

    /// Feed interleaved stereo frames.
    pub fn push(&mut self, frames: &[f32]) {
        let keep = self.sr as usize / 2;
        for f in frames.chunks_exact(2) {
            let (l, r) = (f[0] as f64, f[1] as f64);
            self.m.clipped += (l.abs() >= 1.0) as u64 + (r.abs() >= 1.0) as u64;
            self.hop_peak = self.hop_peak.max(l.abs()).max(r.abs());
            self.hop.push((l + r) / 2.0);
            if self.end.len() == keep {
                self.end.pop_front();
            }
            self.end.push_back([f[0], f[1]]);
            self.m.frames += 1;
            if self.hop.len() == self.h {
                self.close_hop();
            }
        }
    }

    fn close_hop(&mut self) {
        self.m.hop_ss.push(self.hop.iter().map(|v| v * v).sum());
        self.m.hop_pk.push(self.hop_peak);
        self.fft.magnitudes(&self.hop, &self.win, &mut self.mag);
        let num: f64 = self.mag.iter().zip(&self.fq).map(|(a, f)| a * f).sum();
        self.m.cen.push(num / (self.mag.iter().sum::<f64>() + 1e-12));
        // Only whole hops are filtered (as the script did).
        for &v in &self.hop {
            let y = self.hf.process(v);
            self.chunk.0 += y * y;
            self.chunk.1 += 1;
            if self.chunk.1 == self.cw {
                self.m.hf_ss.push(self.chunk.0);
                self.chunk = (0.0, 0);
            }
        }
        self.m.peak = self.m.peak.max(self.hop_peak);
        self.hop.clear();
        self.hop_peak = 0.0;
    }

    /// The measures (the final partial hop counts toward the peak only), with the WAV finalized.
    pub fn finish(mut self) -> io::Result<Measured> {
        self.m.peak = self.m.peak.max(self.hop_peak);
        self.m.hf_ss.resize(self.m.frames / self.cw.max(1), 0.0);
        let n = (self.end.len() * 2).max(1) as f64;
        self.m.end_ms = self.end.iter().map(|[l, r]| (*l as f64).powi(2) + (*r as f64).powi(2)).sum::<f64>() / n;
        if let Some(w) = self.out.take() {
            w.finalize()?;
        }
        Ok(self.m)
    }
}

impl FrameSink for SeamMeter {
    fn write_frames(&mut self, frames: &[f32]) -> io::Result<()> {
        if let Some(w) = self.out.as_mut() {
            w.write_interleaved(frames)?;
        }
        self.push(frames);
        Ok(())
    }
    fn checkpoint(&mut self) -> io::Result<()> {
        self.out.as_mut().map_or(Ok(()), WavWriter::checkpoint)
    }
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub doc: String,
    /// How the song was shrunk; `None` when measured as given.
    pub scaled: Option<Scaled>,
    pub basics: Basics,
    pub blocks: Vec<BlockLevel>,
    pub inside: Inside,
    pub seams: Vec<SeamStep>,
    pub tail: Tail,
    pub seams_checked: usize,
    #[serde(flatten)]
    pub findings: Findings,
    pub verdict: String,
}

#[derive(Debug, Serialize)]
pub struct Basics {
    pub duration_s: f64,
    pub plan_s: f64,
    pub loop_s: f64,
    pub transition_s: f64,
    pub blocks: usize,
    pub tail_loops: usize,
    pub peak: f64,
    pub peak_dbfs: f64,
    pub clipped: u64,
    pub body_rms_dbfs: f64,
    /// The render is shorter than its plan (only what it holds was checked).
    pub cut: bool,
}

#[derive(Debug, Serialize)]
pub struct BlockLevel {
    pub idx: usize,
    pub label: String,
    pub layer: Option<String>,
    pub start_s: f64,
    pub loops: usize,
    /// Under one loop of it in the render.
    pub rendered: bool,
    pub peak: Option<f64>,
    /// RMS past its first `transition_s`, dBFS.
    pub rms_dbfs: Option<f64>,
    /// Settled level vs the previous block.
    pub step_db: Option<f64>,
    /// Its last loop vs its first full loop (drift / the Orrery walking it louder).
    pub creep_db: Option<f64>,
}

/// The music's own movement inside the blocks, which a seam is measured against.
#[derive(Debug, Serialize)]
pub struct Inside {
    /// p99.9 of the 1-s loudness step (0.4-s RMS).
    pub loud_step_db: f64,
    /// p99.9 of the 1-s spectral-centroid step.
    pub centroid_step_hz: f64,
    /// The largest > 4 kHz transient (3-ms RMS).
    pub hf_max: f64,
}

#[derive(Debug, Serialize)]
pub struct SeamStep {
    pub at_s: f64,
    pub from: String,
    pub to: String,
    pub rendered: bool,
    pub loud_step_db: Option<f64>,
    pub centroid_step_hz: Option<f64>,
    /// The > 4 kHz transient within ±50 ms of the seam / the largest inside a block.
    pub hf_ratio: Option<f64>,
    /// `smooth`, or the lurches / CLICK it shows.
    pub verdict: String,
}

#[derive(Debug, Serialize)]
pub struct Tail {
    /// Judged only when the render reaches the planned end.
    pub judged: bool,
    pub pre_dbfs: Option<f64>,
    /// Seconds from the tail's start to −60 dBFS (`None`: never).
    pub ring_s: Option<f64>,
    pub tail_s: f64,
    /// The last 0.5 s, dBFS.
    pub end_dbfs: f64,
    /// The `tail_loops` that would hold the ring-out plus 3 s.
    pub tail_loops_needed: Option<usize>,
}

/// `song_seams.py`'s verdicts over the measures.
pub fn judge(doc: &str, scaled: Option<Scaled>, plan: &Plan, m: &Measured) -> Report {
    let sr = plan.sr;
    let (h, cw) = hop_sizes(sr);
    let l = plan.loop_frames;
    let t = plan.transition_s;
    let starts = plan.starts();
    let tail0 = *starts.last().unwrap_or(&0);
    let plan_end = tail0 + plan.tail_loops * l;
    let n = m.frames;
    let nh = m.hop_ss.len();
    let hop_of = |frame: usize| frame / h;
    let mut f = Findings::default();
    let is_scaled = scaled.as_ref().is_some_and(|s| !s.full);

    let hb = hop_of(tail0).min(nh);
    let body_rms = (m.hop_ss[..hb].iter().sum::<f64>() / (hb * h).max(1) as f64).sqrt();
    let have = nh * h;
    let cut = n + l / 2 < plan_end;
    if cut {
        f.warns.push(format!(
            "the render is {} but the plan is {} — cut short; only the blocks and seams it holds were checked, and not the tail",
            fmt_hms(n as f64 / sr),
            fmt_hms(plan_end as f64 / sr)
        ));
    }
    if m.clipped > 0 {
        f.fails.push(format!("{} samples at/over full scale — lower master_gain_db (inside its safe_bounds band) or the hot block's levels", m.clipped));
    } else if is_scaled && m.peak > 0.71 {
        f.warns.push(format!(
            "peak {:.1} dBFS in a scaled-down render — full-length blocks walk longer and peaked +2.3 dB (1-h nap) and +2.8 dB (5.5-h reference) over their scaled renders; keep a scaled peak ≲ −3 dBFS",
            db(m.peak)
        ));
    } else if m.peak > 0.89 {
        f.warns.push(format!("peak {:.1} dBFS — under 1 dB from clipping", db(m.peak)));
    }

    // Short-term loudness (0.4 s = 4 hops) and its 1-s steps.
    let ss4 = dsp::box_conv(&m.hop_ss, 4, 1.0, Conv::Valid);
    let loud: Vec<f64> = ss4.iter().map(|s| db((s / (4 * h) as f64).sqrt())).collect();
    let tt: Vec<f64> = (0..loud.len()).map(|i| (i + 2) as f64 * h as f64 / sr).collect();
    let cen4 = dsp::box_conv(&m.cen, 4, 0.25, Conv::Valid);
    let lag = 10;
    let steps = |v: &[f64]| -> Vec<f64> { if v.len() > lag { (lag..v.len()).map(|i| (v[i] - v[i - lag]).abs()).collect() } else { Vec::new() } };
    let (loud_step, cen_step) = (steps(&loud), steps(&cen4));
    let ts: Vec<f64> = tt.iter().skip(lag).copied().collect();
    let seam_s: Vec<f64> = starts[1..starts.len() - 1].iter().map(|&s| s as f64 / sr).collect();
    let in_seam = |x: f64| seam_s.iter().any(|&s| x > s - 2.0 && x < s + t + 2.0);
    let inside: Vec<usize> = (0..ts.len().min(loud_step.len()))
        .filter(|&i| !in_seam(ts[i]) && ts[i] > t.max(5.0) && ts[i] < tail0 as f64 / sr - 1.0)
        .collect();
    let pick = |v: &[f64]| inside.iter().filter_map(|&i| v.get(i).copied()).collect::<Vec<_>>();
    let lp = if inside.is_empty() { 1.0 } else { dsp::percentile(&pick(&loud_step), 99.9) };
    let cp = if inside.is_empty() { 100.0 } else { dsp::percentile(&pick(&cen_step), 99.9) };
    let hf: Vec<f64> = m.hf_ss.iter().map(|s| (s / cw as f64).sqrt()).collect();
    let mut hf_in = hf.clone();
    let zero = |v: &mut Vec<f64>, a: usize, z: usize| {
        let z = z.min(v.len());
        if a < z {
            v[a..z].iter_mut().for_each(|x| *x = 0.0);
        }
    };
    for &s in &starts[1..] {
        let a = ((s as f64 - 0.25 * sr) / cw as f64).floor().max(0.0) as usize;
        let z = ((s as f64 + (t + 0.25) * sr) / cw as f64).floor() as usize;
        zero(&mut hf_in, a, z);
    }
    let first = (5.0 * sr) as usize / cw;
    zero(&mut hf_in, 0, first);
    let len = hf_in.len();
    zero(&mut hf_in, tail0 / cw, len);
    let hfmax = hf_in.iter().copied().fold(0.0f64, f64::max) + 1e-12;

    // Blocks.
    let rms_of = |a: usize, z: usize| {
        let (ha, hz) = (hop_of(a), hop_of(z).max(hop_of(a)));
        db((m.hop_ss[ha.min(nh)..hz.min(nh)].iter().sum::<f64>() / ((hz - ha) * h).max(1) as f64).sqrt())
    };
    let mut blocks = Vec::new();
    let mut prev: Option<f64> = None;
    for (i, (label, layer, loops)) in plan.blocks.iter().enumerate() {
        let s0 = starts[i];
        let z = starts[i + 1].min(have);
        let mut row = BlockLevel { idx: i, label: label.clone(), layer: layer.clone(), start_s: s0 as f64 / sr, loops: *loops, rendered: false, peak: None, rms_dbfs: None, step_db: None, creep_db: None };
        if z < s0 + l {
            blocks.push(row);
            continue;
        }
        let a = s0 + (t * sr) as usize;
        let a = if z > a { a } else { s0 };
        let rms = rms_of(a, z);
        let pk = m.hop_pk[hop_of(s0)..hop_of(z).min(nh)].iter().copied().fold(0.0f64, f64::max);
        let first_full = if (z - s0) / l >= 3 { rms_of(s0 + l, s0 + 2 * l) } else { rms };
        let creep = rms_of(z - l, z) - first_full;
        let step = prev.map(|p| rms - p);
        if let Some(s) = step.filter(|s| s.abs() > 3.0) {
            f.warns.push(format!("block {i} ({label}) sits {s:+.1} dB vs the previous block — a level step a sleeper hears; trim it with the constant-offset idiom unless the arc wants it"));
        }
        if creep > 2.0 {
            f.warns.push(format!("block {i} ({label}) creeps {creep:+.1} dB within the block — cap its level-ish levers in safe_bounds"));
        }
        prev = Some(rms);
        row.rendered = true;
        (row.peak, row.rms_dbfs, row.step_db, row.creep_db) = (Some(pk), Some(rms), step, Some(creep));
        blocks.push(row);
    }

    // Seams.
    let mut seams = Vec::new();
    let mut checked = 0;
    for (k, &s) in seam_s.iter().enumerate() {
        let (from, to) = (plan.blocks[k].0.clone(), plan.blocks[k + 1].0.clone());
        let mut row = SeamStep { at_s: s, from, to, rendered: false, loud_step_db: None, centroid_step_hz: None, hf_ratio: None, verdict: "not rendered".into() };
        if (s + t + 2.0) * sr > have as f64 {
            seams.push(row);
            continue;
        }
        checked += 1;
        let win: Vec<usize> = (0..ts.len().min(loud_step.len())).filter(|&i| ts[i] > s - 2.0 && ts[i] < s + t + 2.0).collect();
        let ls = win.iter().map(|&i| loud_step[i]).fold(f64::NEG_INFINITY, f64::max);
        let cs = win.iter().filter_map(|&i| cen_step.get(i).copied()).fold(f64::NEG_INFINITY, f64::max);
        let a = (s * sr) as usize;
        let d = (0.05 * sr) as usize;
        let (ca, cz) = (a.saturating_sub(d) / cw, ((a + d) / cw).min(hf.len()));
        let ck = hf[ca.min(cz)..cz].iter().copied().fold(0.0f64, f64::max) / hfmax;
        let mut bad = Vec::new();
        if ls > lp.max(1.5) {
            bad.push("LOUDNESS LURCH");
        }
        if cs > (1.5 * cp).max(150.0) {
            bad.push("TIMBRE LURCH");
        }
        if ck > 1.5 {
            bad.push("CLICK");
        }
        row.rendered = true;
        row.verdict = if bad.is_empty() { "smooth".into() } else { bad.join(" + ") };
        if !bad.is_empty() {
            let mut fix = Vec::new();
            if bad.contains(&"CLICK") {
                fix.push("give the voices that lose their route some `release` (engines-song lints ~zero release)");
            }
            if row.verdict.contains("LURCH") {
                fix.push("lengthen transition_s and/or trim the level step with the constant-offset idiom (a TIMBRE lurch only under drift = a categorical wave flip stepping back at the block reset)");
            }
            f.fails.push(format!("seam {} → {} at {}: {} — {}", row.from, row.to, fmt_hms(s), row.verdict, fix.join("; ")));
        }
        (row.loud_step_db, row.centroid_step_hz, row.hf_ratio) = (Some(ls), Some(cs), Some(ck));
        seams.push(row);
    }

    // Tail.
    let end_dbfs = db(m.end_ms.sqrt());
    let tail_s = (plan.tail_loops * l) as f64 / sr;
    let mut tail = Tail { judged: false, pre_dbfs: None, ring_s: None, tail_s, end_dbfs, tail_loops_needed: None };
    if !cut && plan.tail_loops == 0 {
        f.warns.push("tail_loops 0 — the song stops mid-sound".into());
    } else if !cut {
        let t0 = tail0 as f64 / sr;
        let pre: Vec<f64> = tt.iter().zip(&loud).filter(|(x, _)| **x > t0 - 6.0 && **x < t0).map(|(_, v)| *v).collect();
        let ring = tt.iter().zip(&loud).find(|(x, v)| **x > t0 && **v <= -60.0).map(|(x, _)| x - t0);
        tail.judged = true;
        tail.pre_dbfs = Some(dsp::mean(&pre));
        tail.ring_s = ring;
        tail.tail_loops_needed = ring.map(|r| ((r + 3.0) / (l as f64 / sr)).ceil() as usize);
        if ring.is_none() || end_dbfs > -60.0 {
            f.fails.push(format!("the ending is cut off (last 0.5 s at {end_dbfs:.1} dBFS) — raise song.tail_loops"));
        }
    }

    let verdict = f.verdict("CLEAN");
    Report {
        doc: doc.to_string(),
        scaled,
        basics: Basics {
            duration_s: n as f64 / sr,
            plan_s: plan_end as f64 / sr,
            loop_s: l as f64 / sr,
            transition_s: t,
            blocks: plan.blocks.len(),
            tail_loops: plan.tail_loops,
            peak: m.peak,
            peak_dbfs: db(m.peak),
            clipped: m.clipped,
            body_rms_dbfs: db(body_rms),
            cut,
        },
        blocks,
        inside: Inside { loud_step_db: lp, centroid_step_hz: cp, hf_max: hfmax },
        seams,
        tail,
        seams_checked: checked,
        verdict,
        findings: f,
    }
}

fn print(r: &Report) {
    if let Some(s) = &r.scaled {
        s.print(&r.doc);
        println!();
    }
    let b = &r.basics;
    println!("== basics: {} (rendered in memory with drift and the Orrery) ==", r.doc);
    println!("duration {} (plan {}) · loop {:.3} s · seam {} s · {} blocks + {} tail loop(s)", fmt_hms(b.duration_s), fmt_hms(b.plan_s), b.loop_s, b.transition_s, b.blocks, b.tail_loops);
    println!("peak {:.3} ({:.1} dBFS) · {} samples ≥ 1.0 FS · RMS {:.1} dBFS (body)", b.peak, b.peak_dbfs, b.clipped, b.body_rms_dbfs);

    println!("\n== blocks (first {} s of each excluded; Δ = settled level vs the previous block) ==", b.transition_s);
    println!("  {:>2}  {:<12} {:<10} {:>8} {:>5}  {:>6} {:>6}  {:>6}  {:>6}  {:>6}", "#", "label", "layer", "start", "loops", "peak", "dBFS", "RMS", "Δ prev", "creep");
    for x in &r.blocks {
        let layer = x.layer.as_deref().unwrap_or("—");
        match (x.peak, x.rms_dbfs, x.creep_db) {
            (Some(pk), Some(rms), Some(creep)) => {
                let step = x.step_db.map_or(String::new(), |s| format!("{s:+6.1}"));
                println!("  {:>2}  {:<12} {:<10} {:>8} {:>5}  {pk:6.3} {:6.1}  {rms:6.1}  {step:>6}  {creep:+6.1}", x.idx, x.label, layer, fmt_hms(x.start_s), x.loops, db(pk));
            }
            _ => println!("  {:>2}  {:<12} {:<10} {:>8} {:>5}  not rendered", x.idx, x.label, layer, fmt_hms(x.start_s), x.loops),
        }
    }
    let i = &r.inside;
    println!("\n== seams (window seam −2 s … +{} s +2 s; 'inside' = the same measure's p99.9 within blocks) ==", b.transition_s);
    println!("  inside: 1-s loudness step p99.9 {:.2} dB · 1-s centroid step p99.9 {:.0} Hz · >4 kHz transient max {:.4}", i.loud_step_db, i.centroid_step_hz, i.hf_max);
    println!("  {:>8}  {:<24} {:>9} {:>13} {:>9}  verdict", "at", "seam", "loud step", "centroid step", "HF/inside");
    for s in &r.seams {
        let name = format!("{} → {}", s.from, s.to);
        match (s.loud_step_db, s.centroid_step_hz, s.hf_ratio) {
            (Some(ls), Some(cs), Some(ck)) => println!("  {:>8}  {name:<24} {ls:7.2}dB {cs:11.0}Hz {ck:8.2}×  {}", fmt_hms(s.at_s), s.verdict),
            _ => println!("  {:>8}  {name:<24} not rendered", fmt_hms(s.at_s)),
        }
    }
    let t = &r.tail;
    println!("\n== tail (every note route dropped: releases + reverb ring out) ==");
    if b.cut {
        println!("  not judged — the render ends at {}, before the planned end {}", fmt_hms(b.duration_s), fmt_hms(b.plan_s));
    } else if b.tail_loops == 0 {
        println!("  no tail · last 0.5 s {:.1} dBFS", t.end_dbfs);
    } else {
        let ring = t.ring_s.map_or("never".to_string(), |r| format!("{r:.1} s"));
        let need = t.tail_loops_needed.map_or(String::new(), |n| format!(" · tail_loops {n} would do ({} authored)", b.tail_loops));
        println!("  pre-tail level {:.1} dBFS · reaches −60 dBFS after {ring} of a {:.1} s tail · last 0.5 s {:.1} dBFS{need}", t.pre_dbfs.unwrap_or(f64::NAN), t.tail_s, t.end_dbfs);
    }
    println!();
    r.findings.print();
    println!("VERDICT: {} — peak {:.1} dBFS, {} clipped, {} of {} seam(s) checked", r.verdict, b.peak_dbfs, b.clipped, r.seams_checked, r.seams.len());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    const SR: f64 = 48_000.0;

    /// Three 4-loop blocks of 2-s loops and a 1-loop tail; seams glide over 1 s.
    fn plan() -> Plan {
        Plan { sr: SR, loop_frames: 2 * SR as usize, transition_s: 1.0, tail_loops: 1, blocks: (0..3).map(|i| (format!("b{i}"), None, 4)).collect() }
    }

    /// The plan rendered as a steady 220 Hz tone (with a quiet 5 kHz partial, so the blocks carry
    /// some > 4 kHz energy) and a slow wobble, the block's own `gain(t)` on it, and a ring-out in
    /// the tail.
    fn render(gain: impl Fn(f64) -> f64) -> Measured {
        let p = plan();
        let tail0 = 24.0;
        let n = ((tail0 + 2.0) * SR) as usize;
        let mut meter = SeamMeter::new(SR, None);
        let frames: Vec<f32> = (0..n)
            .flat_map(|i| {
                let t = i as f64 / SR;
                let env = if t < tail0 { 1.0 } else { (-(t - tail0) * 8.0).exp() };
                let fade_in = (t / 0.5).min(1.0);
                let tone = (2.0 * PI * 220.0 * t).sin() + 0.03 * (2.0 * PI * 5000.0 * t).sin();
                let v = (0.3 * gain(t) * env * fade_in * (1.0 + 0.02 * (2.0 * PI * 0.3 * t).sin()) * tone) as f32;
                [v, v]
            })
            .collect();
        meter.push(&frames);
        assert_eq!(p.starts(), vec![0, 8 * SR as usize, 16 * SR as usize, 24 * SR as usize]);
        meter.finish().unwrap()
    }

    #[test]
    fn a_glided_seam_is_smooth_and_a_level_jump_is_a_loudness_lurch() {
        // A glide: block 1 eases 1 dB down over the 1-s seam (under the 1.5 dB a second that
        // reads as a lurch).
        let glide = |t: f64| if t < 8.0 { 1.0 } else if t < 9.0 { 1.0 - 0.109 * (t - 8.0) } else { 0.891 };
        let r = judge("glide", None, &plan(), &render(glide));
        assert_eq!(r.seams.iter().map(|s| s.verdict.as_str()).collect::<Vec<_>>(), ["smooth", "smooth"], "{:?}", r.seams);
        assert!(r.findings.fails.is_empty(), "{:?}", r.findings);
        assert!(r.tail.judged && r.tail.ring_s.is_some_and(|s| s < 2.0), "{:?}", r.tail);
        // A jump: block 2 lands 6 dB down within 20 ms (no glide, and no click either).
        let jump = |t: f64| if t < 16.0 { 1.0 } else if t < 16.02 { 1.0 - 25.0 * (t - 16.0) } else { 0.5 };
        let r = judge("jump", None, &plan(), &render(jump));
        assert_eq!(r.seams[1].verdict, "LOUDNESS LURCH", "{:?}", r.seams);
        assert!(r.seams[1].loud_step_db.unwrap() > 5.0);
        assert_eq!(r.seams[0].verdict, "smooth");
        assert_eq!(r.verdict, "FAIL");
        assert!(r.findings.warns.iter().any(|w| w.contains("block 2") && w.contains("-6.0 dB")), "{:?}", r.findings.warns);
    }

    #[test]
    fn a_cut_off_ending_and_clips_fail() {
        // No ring-out: the tone runs to the last sample at full level, past full scale.
        let mut meter = SeamMeter::new(SR, None);
        let n = (26.0 * SR) as usize;
        let frames: Vec<f32> = (0..n).flat_map(|i| [(1.2 * (2.0 * PI * 220.0 * i as f64 / SR).sin()) as f32; 2]).collect();
        meter.push(&frames);
        let r = judge("loud", None, &plan(), &meter.finish().unwrap());
        assert!(r.basics.clipped > 0 && r.tail.ring_s.is_none(), "{:?}", r.basics);
        assert!(r.findings.fails.iter().any(|f| f.contains("full scale")) && r.findings.fails.iter().any(|f| f.contains("cut off")), "{:?}", r.findings.fails);
    }
}
