//! `engines-song verify <doc>`: render the doc in memory and measure it. It replaces
//! `verify_render.py` (basics, bands, centroid, rhythm, per-beat), `analyze_center.py`
//! (harmonicity + pulse, when the doc has a centre), `ab_modulators.py` (`--ab`) and
//! `song_blocks.py` (a song doc's per-block table), with their numbers, verdicts and thresholds.
//!
//! The measured render is `engines-render`'s: the loops of the doc's top-level matrix through one
//! engine, no controller (`--block K`: a song block's matrix). A song's block table adds each
//! block as the controller starts it, through the Session (`engines-song --optimize hold`).

use std::collections::HashMap;
use std::path::Path;

use engines_core::loop_doc::LoopDoc;
use engines_core::song;
use engines_core::voice::root_semitone;
use serde::Serialize;

use super::dsp::{self, Band, Conv, Peaks, Rfft, Sos};
use super::{control, load, loop_seconds, mono, peak, raw_render, reparse, session_render, Collect, Common, Findings};
use crate::Fail;

#[derive(clap::Args, Debug, PartialEq)]
pub struct VerifyArgs {
    #[command(flatten)]
    pub common: Common,
    /// Loops to render (the first fades in from silence)
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u64).range(1..))]
    pub loops: u64,
    /// The centre for the harmonicity and pulse checks, in Hz [default: the doc's `meta.center_hz`,
    /// else its lowest `context` pitch]
    #[arg(long, value_name = "HZ")]
    pub f0: Option<f64>,
    /// Also measure each matrix route's audibility: the full render minus the render without that
    /// route, as a share of the full RMS (one render per route; 0 % = wired but silent)
    #[arg(long)]
    pub ab: bool,
    /// A song: verify block K's matrix on its own (K as `--dry-run` numbers it) instead of the
    /// top-level matrix and the block table
    #[arg(long, value_name = "K")]
    pub block: Option<usize>,
    /// A song's block starts: WARN above this peak (FS)
    #[arg(long, default_value_t = 0.6)]
    pub target: f64,
    /// A song's block starts: FAIL above this peak (FS), the clip guard's ceiling
    #[arg(long, default_value_t = 0.7)]
    pub ceiling: f64,
    /// Also write the measured render to this file (32-bit float WAV)
    #[arg(long, value_name = "FILE")]
    pub out: Option<String>,
}

/// The whole report (`--json` prints it as is).
#[derive(Debug, Serialize)]
pub struct Report {
    pub doc: String,
    /// `--block K`: the song block that was measured.
    pub block: Option<BlockRef>,
    pub loops: usize,
    pub basics: Basics,
    pub bands: Vec<BandShare>,
    pub centroid: Centroid,
    pub rhythm: Rhythm,
    /// Low-mid (150–800 Hz) energy per beat of loop 2, normalised to the loudest beat.
    pub per_beat: Vec<f64>,
    /// Harmonicity + pulse; `None` when the doc has no centre (and no `--f0`).
    pub centre: Option<Centre>,
    pub ab: Option<Ab>,
    pub blocks: Option<Blocks>,
    pub lints: Vec<String>,
    #[serde(flatten)]
    pub findings: Findings,
    pub verdict: String,
}

#[derive(Debug, Serialize)]
pub struct BlockRef {
    pub idx: usize,
    pub label: String,
}

pub fn run(a: VerifyArgs) -> Result<bool, Fail> {
    let (doc, lints) = load(&a.common.doc)?;
    let sr = a.common.sample_rate;
    let loops = a.loops as usize;
    let (measured, block) = match a.block {
        Some(k) => {
            let spec = doc.song.as_ref().ok_or_else(|| Fail::Usage(format!("--block needs a song doc; {} has no `song`", a.common.doc)))?;
            if k >= spec.sequence.len() {
                return Err(Fail::Usage(format!("--block {k}: the song has {} blocks (0 … {})", spec.sequence.len(), spec.sequence.len() - 1)));
            }
            let b = song::materialize(&doc, spec, k).map_err(Fail::Usage)?;
            (reparse(&b)?, Some(BlockRef { idx: k, label: spec.label(k) }))
        }
        None => (doc.clone(), None),
    };
    eprintln!("verify: rendering {loops} loop(s) of {} …", block.as_ref().map_or("the top-level matrix".to_string(), |b| format!("block {} ({})", b.idx, b.label)));
    let audio = raw_render(&measured, loops, sr);
    if let Some(out) = &a.out {
        engines_core::wav::write_wav_f32(Path::new(out), &audio, 2, sr as u32).map_err(|e| Fail::Runtime(format!("{out}: {e}")))?;
        eprintln!("wrote {out}");
    }
    let m = mono(&audio);
    let mut findings = Findings::default();

    let centre = match centre_of(&measured, a.f0) {
        Some((f0, source)) => Some(harmonicity(&m, sr, &measured, f0, source)),
        None => None,
    };
    let ab = if a.ab { Some(ab(&measured, &audio, loops, sr, &mut findings)?) } else { None };
    let blocks = match (&doc.song, a.block) {
        (Some(_), None) => Some(blocks(&doc, &a, &mut findings)?),
        _ => None,
    };
    if let Some(c) = centre.as_ref().filter(|c| c.verdict != "PASS") {
        findings.warns.push(format!(
            "the centre check reads CHECK ({:.0} % on the series + {:.0} % family, {:.1} % off; pulse {}) — a centre drone wants ≥ 35 % on the series, ≥ 90 % with the family, ≤ 2 % off and the pulse on the 8th or the beat",
            c.on_series_pct,
            c.family_pct,
            c.other_pct,
            c.pulse.dominant.as_ref().map_or("none", |d| d.grid.as_str())
        ));
    }
    findings.warns.extend(lints.iter().map(|l| format!("engine lint — {l}")));
    let report = Report {
        doc: a.common.doc.clone(),
        block,
        loops,
        basics: basics(&audio, &m, sr, &measured),
        bands: bands(&m, sr),
        centroid: centroid(&m, sr),
        rhythm: rhythm(&m, sr, &measured),
        per_beat: per_beat(&m, sr, &measured),
        centre,
        ab,
        blocks,
        lints,
        verdict: findings.verdict("PASS"),
        findings,
    };
    super::emit(a.common.json, &report, print)?;
    Ok(report.findings.fails.is_empty())
}

// ── verify_render.py ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct Basics {
    pub sample_rate: f64,
    pub frames: usize,
    pub duration_s: f64,
    /// `duration_s` in loops.
    pub loops: f64,
    pub loop_s: f64,
    pub bpm: f64,
    pub all_finite: bool,
    pub nonzero_frac: f64,
    pub peak: f64,
    pub rms: f64,
}

fn basics(stereo: &[f32], m: &[f64], sr: f64, doc: &LoopDoc) -> Basics {
    let (_, loop_s) = loop_seconds(doc);
    let n = m.len();
    let dur = n as f64 / sr;
    Basics {
        sample_rate: sr,
        frames: n,
        duration_s: dur,
        loops: dur / loop_s,
        loop_s,
        bpm: doc.transport.bpm as f64,
        all_finite: stereo.iter().all(|s| s.is_finite()),
        nonzero_frac: m.iter().filter(|v| v.abs() > 1e-6).count() as f64 / n.max(1) as f64,
        peak: peak(stereo),
        rms: dsp::mean(&m.iter().map(|v| v * v).collect::<Vec<_>>()).sqrt(),
    }
}

#[derive(Debug, Serialize)]
pub struct BandShare {
    pub name: &'static str,
    pub lo_hz: f64,
    pub hi_hz: f64,
    pub pct: f64,
}

/// The averaged 16384-point spectrum split into six bands (fraction of the total).
fn bands(m: &[f64], sr: f64) -> Vec<BandShare> {
    let (win, hop) = (16_384usize, 8_192usize);
    let w = dsp::hanning(win);
    let mut fft = Rfft::new(win);
    let mut mag = vec![0.0; win / 2 + 1];
    let mut acc = vec![0.0; win / 2 + 1];
    let mut cnt = 0usize;
    let mut i = 0;
    while i < m.len().saturating_sub(win).max(1) && i + win <= m.len() {
        fft.magnitudes(&m[i..i + win], &w, &mut mag);
        for (a, v) in acc.iter_mut().zip(&mag) {
            *a += v * v;
        }
        cnt += 1;
        i += hop;
    }
    let psd: Vec<f64> = acc.iter().map(|a| a / cnt.max(1) as f64).collect();
    let freqs = dsp::rfftfreq(win, sr);
    let tot = psd.iter().sum::<f64>() + 1e-12;
    [("sub 20-60", 20.0, 60.0), ("BASS 60-150", 60.0, 150.0), ("lowmid 150-500", 150.0, 500.0), ("mid 0.5-2k", 500.0, 2000.0), ("high 2-8k", 2000.0, 8000.0), ("air 8k+", 8000.0, sr / 2.0)]
        .into_iter()
        .map(|(name, lo, hi)| {
            let e: f64 = psd.iter().zip(&freqs).filter(|(_, &f)| f >= lo && f < hi).map(|(p, _)| p).sum();
            BandShare { name, lo_hz: lo, hi_hz: hi, pct: e / tot * 100.0 }
        })
        .collect()
}

#[derive(Debug, Serialize)]
pub struct Centroid {
    pub mean_hz: f64,
    pub min_hz: f64,
    pub max_hz: f64,
    /// The 3-s moving mean's swing: free LFOs on cutoff / resonance / shift_mix.
    pub slow_swing_hz: f64,
    /// The residual's standard deviation: bar-synced step sequences.
    pub rhythmic_hz: f64,
}

fn centroid(m: &[f64], sr: f64) -> Centroid {
    let (fw, fh) = (4096usize, 1024usize);
    let w = dsp::hanning(fw);
    let f = dsp::rfftfreq(fw, sr);
    let mut fft = Rfft::new(fw);
    let mut mag = vec![0.0; fw / 2 + 1];
    let mut cen = Vec::new();
    let mut i = 0;
    while i + fw < m.len() {
        fft.magnitudes(&m[i..i + fw], &w, &mut mag);
        let num: f64 = mag.iter().zip(&f).map(|(a, f)| a * f).sum();
        cen.push(num / (mag.iter().sum::<f64>() + 1e-12));
        i += fh;
    }
    if cen.is_empty() {
        return Centroid { mean_hz: f64::NAN, min_hz: f64::NAN, max_hz: f64::NAN, slow_swing_hz: f64::NAN, rhythmic_hz: f64::NAN };
    }
    let k = ((3.0 / (fh as f64 / sr)) as usize).max(1);
    let trend = dsp::box_conv(&cen, k, 1.0 / k as f64, Conv::Same);
    let resid: Vec<f64> = cen.iter().zip(&trend).map(|(c, t)| c - t).collect();
    let (lo, hi) = minmax(&cen);
    let (tlo, thi) = minmax(&trend);
    Centroid { mean_hz: dsp::mean(&cen), min_hz: lo, max_hz: hi, slow_swing_hz: thi - tlo, rhythmic_hz: dsp::std(&resid) }
}

fn minmax(v: &[f64]) -> (f64, f64) {
    v.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &x| (lo.min(x), hi.max(x)))
}

#[derive(Debug, Serialize)]
pub struct Rhythm {
    pub onsets: usize,
    pub per_loop: f64,
    /// Median inter-onset interval; `None` under two onsets.
    pub median_ioi_s: Option<f64>,
    pub eighth_s: f64,
    pub quarter_s: f64,
    /// The > 1 kHz envelope's peak / RMS (> ~4: clear plucky transients).
    pub hf_crest: f64,
}

/// > 1 kHz spectral-flux onsets and the > 1 kHz envelope's crest.
fn rhythm(m: &[f64], sr: f64, doc: &LoopDoc) -> Rhythm {
    let (beat, loop_s) = loop_seconds(doc);
    let (ow, oh) = (1024usize, 256usize);
    let freqs = dsp::rfftfreq(ow, sr);
    let first_hf = freqs.iter().position(|&f| f >= 1000.0).unwrap_or(freqs.len());
    let w = dsp::hanning(ow);
    let mut fft = Rfft::new(ow);
    let mut mag = vec![0.0; ow / 2 + 1];
    let mut prev: Option<Vec<f64>> = None;
    let (mut flux, mut ft) = (Vec::new(), Vec::new());
    let mut i = 0;
    while i + ow < m.len() {
        fft.magnitudes(&m[i..i + ow], &w, &mut mag);
        let cur = mag[first_hf..].to_vec();
        if let Some(p) = &prev {
            flux.push(cur.iter().zip(p).map(|(c, p)| (c - p).max(0.0)).sum::<f64>());
            ft.push(i as f64 / sr);
        }
        prev = Some(cur);
        i += oh;
    }
    let top = flux.iter().copied().fold(0.0f64, f64::max) + 1e-12;
    for f in &mut flux {
        *f /= top;
    }
    let thr = dsp::mean(&flux) + 0.8 * dsp::std(&flux);
    let gap = (0.18 / (oh as f64 / sr)) as usize;
    let mut pk: Vec<usize> = Vec::new();
    for i in 1..flux.len().saturating_sub(1) {
        if flux[i] > thr && flux[i] >= flux[i - 1] && flux[i] > flux[i + 1] && pk.last().is_none_or(|&l| i - l >= gap) {
            pk.push(i);
        }
    }
    let on: Vec<f64> = pk.iter().map(|&i| ft[i]).collect();
    let dur = m.len() as f64 / sr;
    let iois: Vec<f64> = on.windows(2).map(|w| w[1] - w[0]).collect();
    let hp = Sos::butter(4, Band::High(1000.0), sr).filter(m);
    let sq: Vec<f64> = hp.iter().map(|v| v * v).collect();
    let k = (0.02 * sr) as usize;
    let he = dsp::box_conv(&sq, k, 1.0 / k as f64, Conv::Same);
    let he_max = he.iter().fold(0.0f64, |a, &v| a.max(v.sqrt()));
    Rhythm {
        onsets: on.len(),
        per_loop: on.len() as f64 / (dur / loop_s),
        median_ioi_s: (on.len() > 1).then(|| dsp::median(&iois)),
        eighth_s: 0.5 * beat,
        quarter_s: beat,
        hf_crest: he_max / (dsp::mean(&sq).sqrt() + 1e-12),
    }
}

/// Low-mid (150–800 Hz) energy per beat of loop 2 (loop 1 when there's only one).
fn per_beat(m: &[f64], sr: f64, doc: &LoopDoc) -> Vec<f64> {
    let (beat, loop_s) = loop_seconds(doc);
    let lo2 = (loop_s * sr) as usize;
    let seg = if 2 * lo2 <= m.len() { &m[lo2..2 * lo2] } else { &m[..lo2.min(m.len())] };
    let mut ffts: HashMap<usize, (Rfft, Vec<f64>, Vec<f64>)> = HashMap::new();
    let mut pb = Vec::new();
    for b in 0..doc.transport.master_steps as usize {
        let (a, z) = ((b as f64 * beat * sr) as usize, ((b + 1) as f64 * beat * sr) as usize);
        if z > seg.len() || z <= a {
            break;
        }
        let n = z - a;
        let (fft, w, ff) = ffts.entry(n).or_insert_with(|| (Rfft::new(n), dsp::hanning(n), dsp::rfftfreq(n, sr)));
        let mut mag = vec![0.0; n / 2 + 1];
        fft.magnitudes(&seg[a..z], w, &mut mag);
        pb.push(mag.iter().zip(ff.iter()).filter(|(_, &f)| (150.0..800.0).contains(&f)).map(|(v, _)| v).sum::<f64>());
    }
    let top = pb.iter().copied().fold(0.0f64, f64::max) + 1e-12;
    pb.iter().map(|v| v / top).collect()
}

// ── analyze_center.py ────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct Centre {
    pub f0_hz: f64,
    /// Where f0 came from: `--f0`, `meta.center_hz` or `context` (the lowest context pitch).
    pub f0_source: &'static str,
    /// Share of 40–1500 Hz energy within ±1.5 % of k·f0 (k = 1 … 14). Target ≥ 35 %.
    pub on_series_pct: f64,
    /// The rest of the f0/4 family (the sub-octave floor, firing sidebands). Series + family ≥ 90 %.
    pub family_pct: f64,
    /// Everything else. Target ≤ 2 %.
    pub other_pct: f64,
    /// The 16 strongest spectral peaks, low to high.
    pub peaks: Vec<SpectralPeak>,
    pub pulse: Pulse,
    /// `PASS`, or `CHECK` when a target is missed.
    pub verdict: String,
}

#[derive(Debug, Serialize)]
pub struct SpectralPeak {
    pub hz: f64,
    pub harmonic: i64,
    pub cents: f64,
    pub rel_db: f64,
    /// `f0/2` when an off-series peak is the sub-octave.
    pub tag: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct Pulse {
    /// The crank band whose envelope is measured (0.8·f0 … 3.6·f0).
    pub band_hz: (f64, f64),
    /// The envelope spectrum's strongest lines, strongest first.
    pub lines: Vec<PulseLine>,
    pub dominant: Option<PulseLine>,
    pub eighth_hz: f64,
    pub beat_hz: f64,
    /// > 600 Hz transient onsets per loop.
    pub onsets_per_loop: f64,
    pub eighths_per_loop: u32,
    pub sixteenths_per_loop: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct PulseLine {
    pub hz: f64,
    /// Relative to the strongest line.
    pub rel: f64,
    /// `beat`, `8th`, `16th`, `triplet-8th`, or `?` when no grid line is within 4 %.
    pub grid: String,
}

/// The doc's centre: `--f0`, else `meta.center_hz`, else the lowest pitch of a `context` /
/// `orrery_disable` note sequence.
fn centre_of(doc: &LoopDoc, flag: Option<f64>) -> Option<(f64, &'static str)> {
    if let Some(f) = flag {
        return Some((f, "--f0"));
    }
    if let Some(f) = doc.meta.as_ref().and_then(|m| m.get("center_hz")).and_then(|v| v.as_f64()) {
        return Some((f, "meta.center_hz"));
    }
    let semi = root_semitone(&doc.harmony.root) as f64;
    doc.modulators
        .values()
        .filter(|m| m.output == "note" && (m.orrery_disable || m.orrery_role.as_deref() == Some("context")))
        .flat_map(|m| m.steps.iter().flat_map(|s| s.voicing.values()))
        .map(|v| 440.0 * 2f64.powf((60.0 + semi + v.offset as f64 - 69.0) / 12.0))
        .min_by(f64::total_cmp)
        .map(|f| (f, "context"))
}

fn harmonicity(mono: &[f64], sr: f64, doc: &LoopDoc, f0: f64, f0_source: &'static str) -> Centre {
    let (beat, loop_s) = loop_seconds(doc);
    // Skip the bloom.
    let seg = if mono.len() as f64 > 8.0 * sr { &mono[(4.0 * sr) as usize..] } else { mono };
    let win = 1usize << 17;
    let hop = win / 2;
    let w = dsp::hanning(win);
    let mut fft = Rfft::new(win);
    let mut mag = vec![0.0; win / 2 + 1];
    let mut acc = vec![0.0; win / 2 + 1];
    let mut n = 0usize;
    let mut i = 0;
    while i + win < seg.len() {
        fft.magnitudes(&seg[i..i + win], &w, &mut mag);
        for (a, v) in acc.iter_mut().zip(&mag) {
            *a += v * v;
        }
        n += 1;
        i += hop;
    }
    for a in &mut acc {
        *a /= n.max(1) as f64;
    }
    let f = dsp::rfftfreq(win, sr);
    let tol = 0.015;
    let on_series = |hz: f64| (1..15).any(|k| (hz - k as f64 * f0).abs() <= tol * k as f64 * f0);
    let q = f0 / 4.0;
    let in_family = |hz: f64| (1..60).filter(|k| k % 4 != 0).any(|k| (hz - k as f64 * q).abs() <= tol * k as f64 * q);
    let band: Vec<usize> = (0..f.len()).filter(|&i| f[i] >= 40.0 && f[i] <= 1500.0).collect();
    let tot = band.iter().map(|&i| acc[i]).sum::<f64>() + 1e-20;
    let (mut s, mut fam, mut other) = (0.0, 0.0, 0.0);
    for &i in &band {
        if on_series(f[i]) {
            s += acc[i];
        } else if in_family(f[i]) {
            fam += acc[i];
        } else {
            other += acc[i];
        }
    }
    let (on_series_pct, family_pct, other_pct) = (s / tot * 100.0, fam / tot * 100.0, other / tot * 100.0);

    // The strongest peaks (prominence ≥ 6 dB), labelled by harmonic and cents.
    let band_db: Vec<f64> = band.iter().map(|&i| 10.0 * (acc[i] + 1e-20).log10()).collect();
    let pk = dsp::find_peaks(&band_db, Peaks { prominence: Some(6.0), ..Default::default() });
    let (fb, ab): (Vec<f64>, Vec<f64>) = pk.iter().map(|&j| (f[band[j]], acc[band[j]])).unzip();
    let mut by_power: Vec<usize> = (0..ab.len()).collect();
    by_power.sort_by(|&x, &y| ab[y].total_cmp(&ab[x]));
    by_power.truncate(16);
    by_power.sort_by(|&x, &y| fb[x].total_cmp(&fb[y]));
    let reference = if ab.is_empty() { 1.0 } else { ab.iter().copied().fold(f64::NEG_INFINITY, f64::max) };
    let peaks = by_power
        .iter()
        .map(|&j| {
            let k = ((fb[j] / f0).round_ties_even() as i64).max(1);
            let cents = 1200.0 * (fb[j] / (k as f64 * f0)).log2();
            let tag = (cents.abs() > 3.0 && (1200.0 * (fb[j] / (f0 / 2.0)).log2()).abs() <= 3.0).then_some("f0/2");
            SpectralPeak { hz: fb[j], harmonic: k, cents, rel_db: 10.0 * (ab[j] / reference).log10(), tag }
        })
        .collect();

    let pulse = pulse(mono, seg, sr, f0, beat, loop_s, doc.transport.master_steps);
    let pass = on_series_pct >= 35.0
        && on_series_pct + family_pct >= 90.0
        && other_pct <= 2.0
        && pulse.dominant.as_ref().is_some_and(|d| d.grid == "8th" || d.grid == "beat");
    Centre { f0_hz: f0, f0_source, on_series_pct, family_pct, other_pct, peaks, pulse, verdict: if pass { "PASS" } else { "CHECK" }.into() }
}

/// The envelope spectrum of the crank band (its dominant line should be the 8th or the beat) and
/// the > 600 Hz transient onsets per loop.
fn pulse(mono: &[f64], seg: &[f64], sr: f64, f0: f64, beat: f64, loop_s: f64, steps: u32) -> Pulse {
    let band_hz = (0.8 * f0, 3.6 * f0);
    let e: Vec<f64> = Sos::butter(4, Band::Pass(band_hz.0, band_hz.1), sr).filter(seg).iter().map(|v| v * v).collect();
    let dec = (sr as usize / 200).max(1);
    let mut env: Vec<f64> = dsp::box_conv(&e, dec, 1.0 / dec as f64, Conv::Valid).into_iter().step_by(dec).map(f64::sqrt).collect();
    let m = dsp::mean(&env);
    for v in &mut env {
        *v -= m;
    }
    let spec = dsp::power_spectrum(&env);
    let fr = dsp::rfftfreq(env.len(), 200.0);
    let idx: Vec<usize> = (0..fr.len()).filter(|&i| fr[i] > 0.3 && fr[i] < 12.0).collect();
    let sm: Vec<f64> = idx.iter().map(|&i| spec[i]).collect();
    let smax = sm.iter().copied().fold(0.0f64, f64::max);
    let mut lines_ix = dsp::find_peaks(&sm, Peaks { prominence: Some(smax * 0.02), ..Default::default() });
    lines_ix.sort_by(|&x, &y| sm[y].total_cmp(&sm[x]));
    lines_ix.truncate(6);
    let grid = [("beat", 1.0 / beat), ("8th", 2.0 / beat), ("16th", 4.0 / beat), ("triplet-8th", 3.0 / beat)];
    let label = |hz: f64| {
        let mut best = grid[0];
        for g in &grid[1..] {
            if (g.1 - hz).abs() / g.1 < (best.1 - hz).abs() / best.1 {
                best = *g;
            }
        }
        if (best.1 - hz).abs() / best.1 < 0.04 {
            best.0.to_string()
        } else {
            "?".to_string()
        }
    };
    let lines: Vec<PulseLine> = lines_ix.iter().map(|&i| PulseLine { hz: fr[idx[i]], rel: sm[i] / smax, grid: label(fr[idx[i]]) }).collect();

    let hp: Vec<f64> = Sos::butter(4, Band::High(600.0), sr).filter(mono).iter().map(|v| v.abs()).collect();
    let frm = (sr as usize / 100).max(1);
    let sq: Vec<f64> = hp.iter().map(|v| v * v).collect();
    let env2: Vec<f64> = dsp::box_conv(&sq, frm, 1.0 / frm as f64, Conv::Valid).into_iter().step_by(frm).map(f64::sqrt).collect();
    let flux: Vec<f64> = env2.windows(2).map(|w| (w[1] - w[0]).max(0.0)).collect();
    let on = dsp::find_peaks(&flux, Peaks { height: Some(dsp::percentile(&flux, 97.0)), distance: Some(8.0), ..Default::default() });
    Pulse {
        band_hz,
        dominant: lines.first().cloned(),
        lines,
        eighth_hz: 2.0 / beat,
        beat_hz: 1.0 / beat,
        onsets_per_loop: on.len() as f64 / (mono.len() as f64 / sr / loop_s),
        eighths_per_loop: steps * 2,
        sixteenths_per_loop: steps * 4,
    }
}

// ── ab_modulators.py ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct Ab {
    pub full_rms: f64,
    pub routes: Vec<AbRoute>,
}

#[derive(Debug, Serialize)]
pub struct AbRoute {
    pub id: String,
    pub kind: String,
    pub source: String,
    pub dest: String,
    /// RMS of (full − without this route), as a percent of the full RMS.
    pub diff_pct: f64,
    pub audible: bool,
}

/// Each route's contribution: the engine is deterministic, so the difference between the full
/// render and the render without one route is exactly that route's part.
fn ab(doc: &LoopDoc, full: &[f32], loops: usize, sr: f64, findings: &mut Findings) -> Result<Ab, Fail> {
    let full_m = mono(full);
    let fr = dsp::mean(&full_m.iter().map(|v| v * v).collect::<Vec<_>>()).sqrt() + 1e-12;
    let mut routes = Vec::new();
    for (id, e) in &doc.matrix {
        eprintln!("verify: A/B without {id} …");
        let mut d = doc.clone();
        d.matrix.remove(id);
        let ablated = mono(&raw_render(&reparse(&d)?, loops, sr));
        let n = ablated.len().min(full_m.len());
        let diff = (ablated[..n].iter().zip(&full_m[..n]).map(|(a, b)| (a - b) * (a - b)).sum::<f64>() / n.max(1) as f64).sqrt();
        let pct = diff / fr * 100.0;
        let audible = pct > 1e-3;
        if !audible {
            findings.fails.push(format!("route {id} ({} → {}) reads 0 % — wired but silent (a masked register, or a voice that never sounds): fix or remove it", e.source, e.dest));
        }
        routes.push(AbRoute { id: id.clone(), kind: e.kind.clone(), source: e.source.clone(), dest: e.dest.clone(), diff_pct: pct, audible });
    }
    Ok(Ab { full_rms: fr, routes })
}

// ── song_blocks.py ───────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct Blocks {
    pub loops: usize,
    pub target: f64,
    pub ceiling: f64,
    pub rows: Vec<BlockRow>,
    pub rms_spread_db: f64,
    pub hottest_peak: f64,
}

#[derive(Debug, Serialize)]
pub struct BlockRow {
    pub idx: usize,
    pub label: String,
    /// The raw render's peak (what the doc says).
    pub raw_peak: f64,
    /// The controller's start (Tier-2 safe-clamped): what every block start plays.
    pub ctl_peak: f64,
    /// With the Orrery on: the same start with it off, which the clamp check compares to the raw
    /// peak (a re-voiced chord isn't a clamped level).
    pub ctl_peak_orrery_off: Option<f64>,
    /// RMS over loops 2.. (past the fade-in from silence), dBFS.
    pub rms_dbfs: f64,
    /// RMS step vs the previous block.
    pub step_db: Option<f64>,
    /// Shares of the spectrum below 80 Hz, 80–500, 500–2k and above 2 kHz.
    pub bands_pct: [f64; 4],
    pub verdict: String,
}

/// Every block twice: raw (the materialized doc through one engine) and from the controller's
/// start (the Session with `--optimize hold`, which safe-clamps every lever into its Tier-2 range) —
/// and with the Orrery on, the controller's start once more with it off, for the clamp check.
fn blocks(doc: &LoopDoc, a: &VerifyArgs, findings: &mut Findings) -> Result<Blocks, Fail> {
    let spec = doc.song.as_ref().expect("a song doc");
    let sr = a.common.sample_rate;
    let loops = a.loops as usize;
    let mut rows: Vec<BlockRow> = Vec::new();
    let mut prev: Option<f64> = None;
    for i in 0..spec.sequence.len() {
        let label = spec.label(i);
        let name = format!("block_{i:02}_{}", engines_core::stems::sanitize(&label));
        eprintln!("verify: {name}: raw and from the controller's start …");
        let block = reparse(&song::materialize(doc, spec, i).map_err(Fail::Usage)?)?;
        let raw = raw_render(&block, loops, sr);
        let (max_change, ctl) = control(&block, "hold", false)?;
        let orrery = ctl.as_ref().is_some_and(|c| c.movement.orrery);
        let mut sink = Collect::default();
        session_render(block.clone(), ctl.clone(), max_change, a.common.seed, Some(loops), sr, &mut sink, &name)?;
        let (raw_pk, _, _) = block_stats(&raw, loops, sr);
        let (ctl_pk, rms, bands) = block_stats(&sink.0, loops, sr);
        // The clamp check compares levels alone. With the Orrery on, the controller's start can voice
        // a chord the raw render doesn't, and a louder voicing isn't a clamped level: play that start
        // again with the Orrery off for the comparison (the peak, RMS and ceiling stay what plays).
        let clamp_pk = if orrery {
            let mut held = Collect::default();
            let off = ctl.map(|mut c| {
                c.movement.orrery = false;
                c
            });
            session_render(block, off, max_change, a.common.seed, Some(loops), sr, &mut held, &name)?;
            Some(peak(&held.0))
        } else {
            None
        };
        let step = prev.map(|p| rms - p);
        prev = Some(rms);
        let mut verdict = Vec::new();
        let level_pk = clamp_pk.unwrap_or(ctl_pk);
        if level_pk > raw_pk * 1.001 {
            verdict.push("LOUDER UNDER CONTROLLER".to_string());
            findings.fails.push(format!(
                "{name}: the controller plays it louder than it renders ({raw_pk:.3} → {level_pk:.3} FS{}) — a level authored under its `safe_bounds` floor, which every block start clamps it up to; move the value into its band, or lower the band's min",
                if orrery { ", the Orrery off" } else { "" }
            ));
        }
        if ctl_pk > a.ceiling {
            verdict.push(format!("> {} FS", a.ceiling));
            findings.fails.push(format!("{name}: block-start peak {ctl_pk:.3} FS > {} (the clip guard's ceiling) — lower master_gain_db inside its band, or trim this block's hot layer", a.ceiling));
        } else if ctl_pk > a.target {
            verdict.push(format!("> {} FS", a.target));
            findings.warns.push(format!("{name}: block-start peak {ctl_pk:.3} FS > {} — full-length renders peaked +3.4–3.9 dB over their block starts; aim ≤ 0.6 so the whole song stays under full scale", a.target));
        }
        if let Some(s) = step.filter(|s| s.abs() > 3.0) {
            verdict.push("LEVEL STEP".to_string());
            findings.warns.push(format!("{name}: {s:+.1} dB RMS vs the previous block — trim it (constant-offset idiom) unless the arc wants that step"));
        }
        rows.push(BlockRow {
            idx: i,
            label,
            raw_peak: raw_pk,
            ctl_peak: ctl_pk,
            ctl_peak_orrery_off: clamp_pk,
            rms_dbfs: rms,
            step_db: step,
            bands_pct: bands,
            verdict: if verdict.is_empty() { "ok".into() } else { verdict.join(" + ") },
        });
    }
    let (lo, hi) = minmax(&rows.iter().map(|r| r.rms_dbfs).collect::<Vec<_>>());
    let hottest = rows.iter().map(|r| r.ctl_peak).fold(0.0f64, f64::max);
    Ok(Blocks { loops, target: a.target, ceiling: a.ceiling, rows, rms_spread_db: hi - lo, hottest_peak: hottest })
}

/// A block render's peak, its RMS over loops 2.. (dBFS) and a coarse band split of the same.
fn block_stats(stereo: &[f32], loops: usize, sr: f64) -> (f64, f64, [f64; 4]) {
    let m = mono(stereo);
    let body = if loops > 1 { &m[m.len() / loops..] } else { &m[..] };
    let spec = dsp::power_spectrum(body);
    let f = dsp::rfftfreq(body.len(), sr);
    let tot = spec.iter().sum::<f64>() + 1e-12;
    let mut bands = [0.0; 4];
    for (b, (lo, hi)) in bands.iter_mut().zip([(0.0, 80.0), (80.0, 500.0), (500.0, 2000.0), (2000.0, sr / 2.0)]) {
        *b = spec.iter().zip(&f).filter(|(_, &f)| f >= lo && f < hi).map(|(p, _)| p).sum::<f64>() / tot * 100.0;
    }
    let rms = (body.iter().map(|v| v * v).sum::<f64>() / body.len().max(1) as f64).sqrt();
    (peak(stereo), 20.0 * (rms + 1e-12).log10(), bands)
}

// ── the text report ──────────────────────────────────────────────────────────────────────────────

fn print(r: &Report) {
    let b = &r.basics;
    let what = match &r.block {
        Some(k) => format!("block {} ({})", k.idx, k.label),
        None if r.blocks.is_some() => "the top-level matrix (the song's home block; --block K measures one block)".into(),
        None => "the top-level matrix".into(),
    };
    println!("{} · {} raw loop(s) of {what}", r.doc, r.loops);
    println!("\n== basics ==");
    println!("sr={}  frames={}  duration={:.2}s  ({:.2} loops of {:.2}s @ {}bpm)", b.sample_rate as u32, b.frames, b.duration_s, b.loops, b.loop_s, b.bpm);
    println!("all_finite={}  nonzero_frac={:.3}  peak={:.3}  rms={:.4}  (peak should be <~1.0)", b.all_finite, b.nonzero_frac, b.peak, b.rms);

    println!("\n== spectral band energy (fraction of total; expect a distinct BASS band) ==");
    for s in &r.bands {
        println!("  {:<16}: {:5.1}%", s.name, s.pct);
    }
    let c = &r.centroid;
    println!("\n== spectral-centroid modulation (cutoff_hz/shift_mix) ==");
    println!("  centroid mean={:.0}Hz  min={:.0}  max={:.0}", c.mean_hz, c.min_hz, c.max_hz);
    println!("  SLOW trend swing  = {:.0}Hz   <- free LFOs on cutoff_hz/resonance/shift_mix", c.slow_swing_hz);
    println!("  RHYTHMIC residual = {:.0}Hz   <- bar-synced step-seqs", c.rhythmic_hz);

    let h = &r.rhythm;
    println!("\n== rhythm (>1kHz spectral-flux onsets + transient crest) ==");
    match h.median_ioi_s {
        Some(ioi) => println!("  onsets = {}  ({:.1} per loop)  median IOI = {ioi:.3}s  (eighth={:.3}s, quarter={:.3}s)", h.onsets, h.per_loop, h.eighth_s, h.quarter_s),
        None => println!("  onsets: too few"),
    }
    println!("  >1kHz envelope crest (peak/rms) = {:.2}  (>~4 => clear plucky transients)", h.hf_crest);
    println!("\n== per-beat low-mid (150-800Hz) energy in loop 2 (chord articulations = steps) ==");
    println!("  beat: {}", r.per_beat.iter().map(|v| format!("{v:.2}")).collect::<Vec<_>>().join(" "));

    match &r.centre {
        Some(c) => print_centre(c, b),
        None => println!("\n== harmonicity: no centre (no meta.center_hz and no context pitches) — pass --f0 to check one =="),
    }
    if let Some(ab) = &r.ab {
        println!("\n== A/B: each matrix route's share of the mix ({} loops, full RMS={:.4}) ==", r.loops, ab.full_rms);
        println!("  {:<12} {:<5} {:<46} {:>9}", "matrix id", "kind", "source -> dest", "diff-RMS");
        for x in &ab.routes {
            let route = format!("{} -> {}", x.source, x.dest.replace("instruments.", "").replace(".params", ""));
            println!("  {:<12} {:<5} {:<46} {:7.2}%  {}", x.id, x.kind, route, x.diff_pct, if x.audible { "audible" } else { "** SILENT/UNWIRED -- fix **" });
        }
    }
    if let Some(bl) = &r.blocks {
        println!("\n== blocks as the controller starts them ({} loops each; RMS/bands over loops 2..) ==", bl.loops);
        println!("  {:<24} {:>7} {:>7} {:>6} {:>6} {:>6}   {:>4} {:>6} {:>5} {:>4}  verdict", "block", "raw pk", "ctl pk", "dBFS", "RMS", "Δ prev", "<80", "80-500", ".5-2k", ">2k");
        for x in &bl.rows {
            let name = format!("block_{:02}_{}", x.idx, engines_core::stems::sanitize(&x.label));
            let st = x.step_db.map_or(String::new(), |s| format!("{s:+6.1}"));
            let [b0, b1, b2, b3] = x.bands_pct;
            println!("  {name:<24} {:7.3} {:7.3} {:6.1} {:6.1} {st:>6}   {b0:4.0} {b1:6.0} {b2:5.0} {b3:4.0}  {}", x.raw_peak, x.ctl_peak, 20.0 * x.ctl_peak.log10(), x.rms_dbfs, x.verdict);
        }
        let off: Vec<String> = bl.rows.iter().filter_map(|x| x.ctl_peak_orrery_off.map(|p| format!("{} {p:.3}", x.label))).collect();
        if !off.is_empty() {
            println!("  (the Orrery is on: the louder-under-the-controller check plays each start with it off, so a re-voiced chord isn't read as a level — {})", off.join(", "));
        }
        println!("\n  RMS spread across blocks {:.1} dB · hottest block start {:.3} FS (aim ≤ {}, ceiling {})", bl.rms_spread_db, bl.hottest_peak, bl.target, bl.ceiling);
    }
    println!();
    r.findings.print();
    println!("VERDICT: {}", r.verdict);
}

fn print_centre(c: &Centre, b: &Basics) {
    println!("\n== harmonicity (centre f0 = {} Hz from {}; {:.2}s loops @ {} bpm) ==", c.f0_hz, c.f0_source, b.loop_s, b.bpm);
    println!("  on the centre's series k·f0 (k=1..14, ±1.5%): {:5.1}%   (target ≥ 35%)", c.on_series_pct);
    println!("  rest of the f0/4 family (sub-octave, firing):  {:5.1}%   (series + family ≥ 90%)", c.family_pct);
    println!("  everything else:                               {:5.1}%   (target ≤ 2%)", c.other_pct);
    println!("  top peaks (Hz | nearest harmonic | cents off | rel dB):");
    for p in &c.peaks {
        println!("    {:8.1}  h{:<3} {:+7.1}¢  {:6.1} dB{}", p.hz, p.harmonic, p.cents, p.rel_db, p.tag.map_or(String::new(), |t| format!("  ← {t}")));
    }
    let p = &c.pulse;
    println!("\n== pulse (envelope spectrum of the crank band {:.0}–{:.0} Hz) ==", p.band_hz.0, p.band_hz.1);
    println!("  strongest lines (Hz | rel | grid): {}", p.lines.iter().map(|l| format!("({:.2}, {:.2}, {})", l.hz, l.rel, l.grid)).collect::<Vec<_>>().join(" "));
    if let Some(d) = &p.dominant {
        println!("  dominant = {:.2} Hz → {}   (want the 8th = {:.2} Hz or the beat = {:.2} Hz)", d.hz, d.grid, p.eighth_hz, p.beat_hz);
    }
    println!("  transient onsets per loop (>600 Hz): {:.1}   (8ths = {}, 16ths = {})", p.onsets_per_loop, p.eighths_per_loop, p.sixteenths_per_loop);
    println!(
        "\n{}: harmonicity {:.0}% on-series + {:.0}% family = {:.0}% / {:.1}% off; pulse dominant = {}",
        c.verdict,
        c.on_series_pct,
        c.family_pct,
        c.on_series_pct + c.family_pct,
        c.other_pct,
        p.dominant.as_ref().map_or("none", |d| d.grid.as_str())
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    /// A plain doc for the measurement functions: 16 beats at `bpm`, root A.
    fn doc(bpm: f64, extra: &str) -> LoopDoc {
        LoopDoc::from_json_str(&format!(
            r#"{{ schema_version: "0.9", transport: {{ bpm: {bpm}, master_steps: 16 }}, harmony: {{ root: "A", mode: "ionian" }},
                 instruments: {{ pad: {{ params: {{ level_db: -12 }} }} }}, {extra} }}"#
        ))
        .unwrap()
    }

    /// `seconds` of mono at 48 kHz.
    fn synth(seconds: f64, f: impl Fn(f64) -> f64) -> Vec<f64> {
        (0..(seconds * 48_000.0) as usize).map(|i| f(i as f64 / 48_000.0)).collect()
    }

    /// A harmonic series on 220 Hz (h1–h6), pulsing on the 8th: its ±2 Hz sidebands stay inside
    /// each harmonic's ±1.5 % window.
    fn series(detune: f64, pulse_hz: f64) -> Vec<f64> {
        synth(30.0, |t| {
            let amp = 0.6 + 0.4 * (2.0 * PI * pulse_hz * t).cos();
            (1..=6).map(|k| (2.0 * PI * 220.0 * k as f64 * (1.0 + if k > 1 { detune } else { 0.0 }) * t).sin() / k as f64).sum::<f64>() * 0.1 * amp
        })
    }

    #[test]
    fn a_pure_series_passes_and_a_detuned_one_does_not() {
        let d = doc(60.0, "");
        // 60 bpm: the 8th is 2 Hz.
        let pure = harmonicity(&series(0.0, 2.0), 48_000.0, &d, 220.0, "--f0");
        assert!(pure.on_series_pct > 99.0 && pure.other_pct < 1.0, "{pure:?}");
        assert_eq!(pure.pulse.dominant.as_ref().map(|l| l.grid.as_str()), Some("8th"));
        assert_eq!(pure.verdict, "PASS");
        for k in 1..=6 {
            assert!(pure.peaks.iter().any(|p| p.harmonic == k && p.cents.abs() < 1.0), "h{k} lands on its harmonic: {:?}", pure.peaks);
        }
        // 4 % sharp upper partials: off the ±1.5 % series windows.
        let detuned = harmonicity(&series(0.04, 2.0), 48_000.0, &d, 220.0, "--f0");
        assert!(detuned.other_pct > 2.0 && detuned.on_series_pct < 90.0, "{detuned:?}");
        assert_eq!(detuned.verdict, "CHECK");
    }

    #[test]
    fn a_click_train_on_the_8th_is_the_pulse_and_off_the_grid_is_not() {
        let d = doc(60.0, "");
        // A 110 Hz series gated into short clicks every 8th (2 Hz at 60 bpm), then at 2.7 Hz.
        let clicks = |hz: f64| {
            synth(30.0, move |t| {
                let ph = (t * hz).fract();
                let env = if ph < 0.06 { (1.0 - ph / 0.06).powi(2) } else { 0.0 };
                env * (1..=3).map(|k| (2.0 * PI * 110.0 * k as f64 * t).sin()).sum::<f64>() * 0.2
            })
        };
        let on = harmonicity(&clicks(2.0), 48_000.0, &d, 110.0, "--f0");
        assert_eq!(on.pulse.dominant.as_ref().map(|l| l.grid.as_str()), Some("8th"), "{:?}", on.pulse);
        assert!((on.pulse.onsets_per_loop - 32.0).abs() <= 4.0, "≈ 32 eighths a 16-beat loop: {}", on.pulse.onsets_per_loop);
        let off = harmonicity(&clicks(2.7), 48_000.0, &d, 110.0, "--f0");
        assert_eq!(off.pulse.dominant.as_ref().map(|l| l.grid.as_str()), Some("?"), "{:?}", off.pulse);
        assert_eq!(off.verdict, "CHECK");
    }

    #[test]
    fn the_centre_comes_from_the_flag_then_meta_then_the_lowest_context_pitch() {
        let ctx = r#"modulators: {
            low: { type: "step_seq", output: "note", length: 16, orrery_role: "context", steps: [ { step: 0, voicing: { a: { offset: -12 }, b: { offset: 7 } } } ] },
            hi: { type: "step_seq", output: "note", length: 16, steps: [ { step: 0, voicing: { a: { offset: -24 } } } ] } }"#;
        let d = doc(60.0, ctx);
        let (f, src) = centre_of(&d, None).unwrap();
        assert!((f - 220.0).abs() < 1e-9 && src == "context", "A3 under the A4 root, not the non-context A2: {f}");
        assert_eq!(centre_of(&d, Some(99.0)), Some((99.0, "--f0")));
        let d = doc(60.0, &format!(r#"meta: {{ center_hz: 146.83 }}, {ctx}"#));
        assert_eq!(centre_of(&d, None), Some((146.83, "meta.center_hz")));
        assert_eq!(centre_of(&doc(60.0, ""), None), None);
    }

    /// A 2-s loop at 120 bpm: a saw pad holding D, an LFO wired to its cutoff at `depth_oct`, and
    /// a one-block song at `master_gain_db`, whose `safe_bounds` band is −8 … 0 dB.
    fn pad_doc(depth_oct: f64, master_gain_db: f64) -> LoopDoc {
        LoopDoc::from_json_str(&format!(
            r#"{{ schema_version: "0.9", transport: {{ bpm: 120, master_steps: 4 }}, harmony: {{ root: "D", mode: "dorian" }},
                 global: {{ master_gain_db: {master_gain_db} }},
                 safe_bounds: {{ "global.master_gain_db": {{ min: -8, max: 0 }} }},
                 instruments: {{ pad: {{ note_engine: {{ polyphony: 1 }}, params: {{ wave: "saw_soft", level_db: -6, cutoff_hz: 800, sustain: 1.0, release_s: 0.5 }} }} }},
                 modulators: {{
                   seq: {{ type: "step_seq", output: "note", length: 4, steps: [ {{ step: 0, voicing: {{ v1: {{ offset: 0 }} }} }} ] }},
                   wob: {{ type: "lfo", output: "control", wave: "sine", sync: "free", rate_hz: 2.0, polarity: "bipolar" }} }},
                 matrix: {{
                   m: {{ source: "seq", dest: "instruments.pad.notes", kind: "note", voices: 1 }},
                   w: {{ source: "wob", dest: "instruments.pad.params.cutoff_hz", kind: "mod", depth_oct: {depth_oct} }} }},
                 song: {{ sequence: [ {{ loops: 2, label: "only" }} ], transition_s: 0.5, tail_loops: 1 }} }}"#
        ))
        .unwrap()
    }

    #[test]
    fn a_route_at_zero_depth_reads_0_percent_and_fails() {
        let sr = 24_000.0;
        let (loud, silent) = (pad_doc(1.0, -6.0), pad_doc(0.0, -6.0));
        for (doc, wob_audible) in [(loud, true), (silent, false)] {
            let full = raw_render(&doc, 2, sr);
            let mut f = Findings::default();
            let ab = ab(&doc, &full, 2, sr, &mut f).unwrap();
            let pct = |id: &str| ab.routes.iter().find(|r| r.id == id).unwrap();
            assert!(pct("m").audible && pct("m").diff_pct > 50.0, "{ab:?}");
            assert_eq!(pct("w").audible, wob_audible, "{ab:?}");
            if wob_audible {
                assert!(f.fails.is_empty() && pct("w").diff_pct > 1.0, "{ab:?}");
            } else {
                assert_eq!(pct("w").diff_pct, 0.0, "the engine is deterministic: exactly 0");
                assert!(f.fails.len() == 1 && f.fails[0].contains("route w"), "{f:?}");
            }
        }
    }

    #[test]
    fn a_block_authored_under_its_tier2_floor_plays_louder_under_the_controller_and_fails() {
        let args = |doc: &str| VerifyArgs {
            common: Common { doc: doc.into(), json: false, seed: 7, sample_rate: 24_000.0 },
            loops: 2,
            f0: None,
            ab: false,
            block: None,
            target: 0.6,
            ceiling: 0.7,
            out: None,
        };
        // −6 dB sits inside the master gain's band: the controller starts it as written.
        let mut f = Findings::default();
        let ok = blocks(&pad_doc(1.0, -6.0), &args("ok"), &mut f).unwrap();
        assert!(f.fails.is_empty() && ok.rows[0].verdict == "ok", "{ok:?} {f:?}");
        assert!((ok.rows[0].ctl_peak - ok.rows[0].raw_peak).abs() <= ok.rows[0].raw_peak * 1e-3, "{ok:?}");
        // −20 dB is under the band's −8 dB floor: every block start clamps it up.
        let mut f = Findings::default();
        let low = blocks(&pad_doc(1.0, -20.0), &args("low"), &mut f).unwrap();
        assert!(low.rows[0].ctl_peak > low.rows[0].raw_peak * 2.0, "{low:?}");
        assert_eq!(low.rows[0].verdict, "LOUDER UNDER CONTROLLER");
        assert!(f.fails.iter().any(|x| x.contains("louder than it renders")), "{f:?}");
    }

    /// A block whose Orrery re-voices its chord pad from the first loop: the controller's start
    /// plays other notes than the raw render, at another peak, with no level clamped at all.
    #[test]
    fn a_re_voiced_chord_is_not_read_as_a_clamped_level() {
        let doc = LoopDoc::from_json_str(
            r#"{ schema_version: "0.9", transport: { bpm: 120, master_steps: 4 }, harmony: { root: "D", mode: "dorian" },
                 global: { master_gain_db: -6 },
                 movement: { orrery: true, phrase_loops: 1, tension_target: 0.9, tension_max: 1.0 },
                 instruments: { pad: { note_engine: { polyphony: 4 }, params: { wave: "saw_soft", level_db: -12, cutoff_hz: 2000, sustain: 1.0, release_s: 0.3 } } },
                 modulators: { chords: { type: "step_seq", output: "note", length: 4, orrery_role: "chord",
                   steps: [ { step: 0, voicing: { v1: { offset: 0 }, v2: { offset: 3 }, v3: { offset: 7 }, v4: { offset: 10 } } } ] } },
                 matrix: { m: { source: "chords", dest: "instruments.pad.notes", kind: "note", voices: 4 } },
                 song: { sequence: [ { loops: 3, label: "only" } ], transition_s: 0.5, tail_loops: 1 } }"#,
        )
        .unwrap();
        // Over a few seeds the voicing lands louder at least once — what the check used to fail.
        let mut louder = 0;
        for seed in 1..=12 {
            let args = VerifyArgs {
                common: Common { doc: "chords".into(), json: false, seed, sample_rate: 24_000.0 },
                loops: 3,
                f0: None,
                ab: false,
                block: None,
                target: 0.6,
                ceiling: 0.7,
                out: None,
            };
            let mut f = Findings::default();
            let b = blocks(&doc, &args, &mut f).unwrap();
            let r = &b.rows[0];
            louder += (r.ctl_peak > r.raw_peak * 1.001) as usize;
            let off = r.ctl_peak_orrery_off.expect("the Orrery is on");
            assert!((off - r.raw_peak).abs() <= r.raw_peak * 1e-3, "seed {seed}: with it off, the start is the raw render: {r:?}");
            assert!(f.fails.is_empty() && !r.verdict.contains("LOUDER"), "seed {seed}: {r:?} {f:?}");
        }
        assert!(louder > 0, "no seed voiced the start louder: the test no longer covers the case");
    }

    #[test]
    fn the_basic_measures_read_a_known_signal() {
        let d = doc(60.0, "");
        // A 100 Hz sine (the BASS band) at 0.5, both channels.
        let stereo: Vec<f32> = synth(32.0, |t| 0.5 * (2.0 * PI * 100.0 * t).sin()).iter().flat_map(|&v| [v as f32, v as f32]).collect();
        let m = mono(&stereo);
        let b = basics(&stereo, &m, 48_000.0, &d);
        assert!((b.peak - 0.5).abs() < 1e-3 && (b.rms - 0.5 / 2f64.sqrt()).abs() < 1e-3 && (b.loops - 2.0).abs() < 1e-9);
        let bands = bands(&m, 48_000.0);
        assert!(bands[1].pct > 99.0, "{bands:?}");
        // A steady tone's centroid sits on it (the trend's 'same'-mode edges dip, as numpy's do).
        let c = centroid(&m, 48_000.0);
        assert!((c.min_hz - 100.0).abs() < 1.0 && (c.max_hz - 100.0).abs() < 1.0, "{c:?}");
    }
}
