//! ITU-R BS.1770-4 integrated loudness (LUFS) — the meter behind `engines-song --stems`' per-stem
//! normalization (ADR 0030).
//!
//! Pure and I/O-free like the rest of the core, but **observation-only**: nothing here feeds back
//! into rendering or scoring. A stem's normalization gain is a static, export-side gain applied to
//! the written file — the scored master stays the unlimited, unnormalized `mix · master_gain` of
//! ADR 0021.
//!
//! The algorithm, exactly as the recommendation specifies it:
//! - **K-weighting** per channel: a high-shelf "pre-filter" then the RLB high-pass, as two biquads
//!   in f64. Coefficients come from libebur128's analog prototypes, so any sample rate works (at
//!   48 kHz they reproduce the BS.1770 table to ~1e-15).
//! - **Blocks**: 400 ms gating blocks on a 100 ms hop (75 % overlap); block power = Σ over channels
//!   (all weights 1.0 — stereo) of the K-weighted mean square; loudness = −0.691 + 10·log10(power).
//! - **Gating**: an absolute gate at −70 LUFS, then a relative gate 10 LU under the mean power of
//!   the blocks that passed it (both strict `>`); integrated loudness = the gated mean.
//!
//! The meter keeps every block's power (≈ 10 f64 per second — 1.7 MB for 6 h) so a gain can be
//! re-gated exactly ([`LoudnessMeter::integrated_with_gain`]): raising a very quiet stem can lift
//! blocks over the −70 LUFS gate, which a plain `target − measured` would ignore.

/// Absolute gate (LUFS).
const ABS_GATE: f64 = -70.0;
/// Relative gate offset (LU) under the absolutely-gated mean.
const REL_GATE: f64 = -10.0;
/// The K-weighting's 997 Hz gain compensation in the loudness formula.
const OFFSET: f64 = -0.691;
/// Hops per gating block (400 ms / 100 ms).
const HOPS_PER_BLOCK: usize = 4;

/// One biquad, transposed direct form II, in f64. `b`/`a` are `[b0, b1, b2]` / `[1, a1, a2]`.
#[derive(Clone, Copy, Debug)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 3],
    s1: f64,
    s2: f64,
}

impl Biquad {
    fn new((b, a): ([f64; 3], [f64; 3])) -> Self {
        Self { b, a, s1: 0.0, s2: 0.0 }
    }
    #[inline]
    fn tick(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.s1;
        self.s1 = self.b[1] * x - self.a[1] * y + self.s2;
        self.s2 = self.b[2] * x - self.a[2] * y;
        y
    }
}

/// The two K-weighting biquads `[(b, a); 2]` (pre-filter, then RLB high-pass) for sample rate `fs`.
pub fn k_weighting(fs: f64) -> [([f64; 3], [f64; 3]); 2] {
    // Stage 1: the high-shelf pre-filter (head acoustics), ≈ +4 dB above ~1.7 kHz.
    let (f0, g, q) = (1681.974450955533, 3.999843853973347, 0.7071752369554196);
    let k = (std::f64::consts::PI * f0 / fs).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.4996667741545416);
    let a0 = 1.0 + k / q + k * k;
    let pre = (
        [(vh + vb * k / q + k * k) / a0, 2.0 * (k * k - vh) / a0, (vh - vb * k / q + k * k) / a0],
        [1.0, 2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
    );
    // Stage 2: the RLB high-pass (~38 Hz). Its numerator stays unnormalized [1, −2, 1], as in the
    // BS.1770 table — the 0.691 dB offset compensates the overall gain.
    let (f0, q) = (38.13547087602444, 0.5003270373238773);
    let k = (std::f64::consts::PI * f0 / fs).tan();
    let d = 1.0 + k / q + k * k;
    let rlb = ([1.0, -2.0, 1.0], [1.0, 2.0 * (k * k - 1.0) / d, (1.0 - k / q + k * k) / d]);
    [pre, rlb]
}

/// Streaming BS.1770 integrated-loudness meter over interleaved audio (see module docs).
#[derive(Clone, Debug)]
pub struct LoudnessMeter {
    channels: usize,
    /// Per channel: [pre-filter, RLB].
    filters: Vec<[Biquad; 2]>,
    /// Next channel index within the current frame (chunks may split frames).
    ch: usize,
    /// Frames per 100 ms hop.
    hop: usize,
    frames_in_hop: usize,
    /// Σ over channels of K-weighted x² in the current hop.
    hop_acc: f64,
    /// The last `HOPS_PER_BLOCK` completed hops' energies (ring), and how many hops completed.
    recent: [f64; HOPS_PER_BLOCK],
    hops: usize,
    /// Every complete gating block's power (Σ over channels of the K-weighted mean square).
    blocks: Vec<f64>,
}

impl LoudnessMeter {
    pub fn new(sample_rate: f64, channels: usize) -> Self {
        assert!(channels >= 1, "a loudness meter needs at least one channel");
        let coeffs = k_weighting(sample_rate);
        Self {
            channels,
            filters: vec![[Biquad::new(coeffs[0]), Biquad::new(coeffs[1])]; channels],
            ch: 0,
            hop: ((0.1 * sample_rate).round() as usize).max(1),
            frames_in_hop: 0,
            hop_acc: 0.0,
            recent: [0.0; HOPS_PER_BLOCK],
            hops: 0,
            blocks: Vec::new(),
        }
    }

    /// Feed interleaved samples (any length — frames may split across calls).
    pub fn push_interleaved(&mut self, s: &[f32]) {
        for &x in s {
            let [pre, rlb] = &mut self.filters[self.ch];
            let y = rlb.tick(pre.tick(x as f64));
            self.hop_acc += y * y;
            self.ch += 1;
            if self.ch == self.channels {
                self.ch = 0;
                self.frames_in_hop += 1;
                if self.frames_in_hop == self.hop {
                    self.end_hop();
                }
            }
        }
    }

    fn end_hop(&mut self) {
        self.recent[self.hops % HOPS_PER_BLOCK] = self.hop_acc;
        self.hops += 1;
        self.hop_acc = 0.0;
        self.frames_in_hop = 0;
        if self.hops >= HOPS_PER_BLOCK {
            let energy: f64 = self.recent.iter().sum();
            self.blocks.push(energy / (HOPS_PER_BLOCK * self.hop) as f64);
        }
    }

    /// Complete 400 ms gating blocks measured so far.
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Integrated loudness (LUFS), or `None` when no block clears the −70 LUFS absolute gate
    /// (silence, a signal that quiet, or under 400 ms of audio).
    pub fn integrated(&self) -> Option<f64> {
        self.integrated_with_gain(0.0)
    }

    /// Integrated loudness the same audio would measure after a static `gain_db` — re-gated, so
    /// blocks the gain lifts over (or drops under) a gate count exactly as a re-measurement would.
    pub fn integrated_with_gain(&self, gain_db: f64) -> Option<f64> {
        let scale = 10f64.powf(gain_db / 10.0);
        let loud = |p: f64| OFFSET + 10.0 * (p * scale).log10();
        let gated_mean = |thresh: f64| {
            let (sum, n) = self.blocks.iter().filter(|&&p| loud(p) > thresh).fold((0.0, 0usize), |(s, n), &p| (s + p, n + 1));
            (n > 0).then(|| sum / n as f64)
        };
        let abs_mean = gated_mean(ABS_GATE)?;
        let rel_gate = loud(abs_mean) + REL_GATE;
        gated_mean(ABS_GATE.max(rel_gate)).map(loud)
    }
}

/// What capped a stem's normalization gain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limit {
    /// The gain reaches the loudness target.
    Loudness,
    /// The sample-peak ceiling stopped it short of the target (a high-crest stem).
    Peak,
}

impl Limit {
    pub fn as_str(self) -> &'static str {
        match self {
            Limit::Loudness => "loudness",
            Limit::Peak => "peak",
        }
    }
}

/// A stem's static normalization gain.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NormGain {
    pub gain_db: f64,
    pub limited_by: Limit,
    /// Predicted integrated loudness after the gain (re-gated).
    pub out_lufs: Option<f64>,
}

/// The static gain that brings a measured stem to `target_lufs`, capped so its sample peak (linear
/// `peak`) stays at or under `ceiling_dbfs`. `None` when there's nothing to normalize: digital
/// silence, or audio entirely under the absolute gate (the caller keeps it at unity).
///
/// The gain is solved iteratively against the re-gated loudness (a few steps; a gain that moves
/// blocks across a gate makes `target − measured` inexact), then the peak cap is applied.
pub fn normalization_gain(meter: &LoudnessMeter, peak: f32, target_lufs: f64, ceiling_dbfs: f64) -> Option<NormGain> {
    if peak.is_nan() || peak <= 0.0 {
        return None;
    }
    let mut gain = target_lufs - meter.integrated()?;
    for _ in 0..8 {
        let Some(now) = meter.integrated_with_gain(gain) else { break };
        let err = target_lufs - now;
        if err.abs() < 1e-6 {
            break;
        }
        gain += err;
    }
    let headroom = ceiling_dbfs - 20.0 * (peak as f64).log10();
    let (gain_db, limited_by) = if gain > headroom { (headroom, Limit::Peak) } else { (gain, Limit::Loudness) };
    Some(NormGain { gain_db, limited_by, out_lufs: meter.integrated_with_gain(gain_db) })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stereo sine at `dbfs` peak amplitude (both channels), `secs` long.
    fn sine(fs: f64, hz: f64, dbfs: f64, secs: f64) -> Vec<f32> {
        let a = 10f64.powf(dbfs / 20.0);
        let n = (fs * secs).round() as usize;
        (0..n).flat_map(|i| {
            let v = (a * (2.0 * std::f64::consts::PI * hz * i as f64 / fs).sin()) as f32;
            [v, v]
        })
        .collect()
    }

    fn lufs(fs: f64, s: &[f32]) -> Option<f64> {
        let mut m = LoudnessMeter::new(fs, 2);
        m.push_interleaved(s);
        m.integrated()
    }

    fn close(got: Option<f64>, want: f64, tol: f64) {
        let got = got.expect("measurable");
        assert!((got - want).abs() <= tol, "got {got:.4} LUFS, want {want} ± {tol}");
    }

    #[test]
    fn coefficients_match_the_bs1770_48k_table() {
        let [(pb, pa), (rb, ra)] = k_weighting(48_000.0);
        let table = [
            (pb[0], 1.53512485958697),
            (pb[1], -2.69169618940638),
            (pb[2], 1.19839281085285),
            (pa[1], -1.69065929318241),
            (pa[2], 0.73248077421585),
            (ra[1], -1.99004745483398),
            (ra[2], 0.99007225036621),
        ];
        for (got, want) in table {
            assert!((got - want).abs() < 1e-12, "{got} vs {want}");
        }
        assert_eq!((rb, pa[0], ra[0]), ([1.0, -2.0, 1.0], 1.0, 1.0));
    }

    /// EBU Tech 3341 minimum-requirements cases 1–5 (997 Hz stereo sines, levels in dBFS peak).
    #[test]
    fn ebu_tech_3341_cases() {
        let fs = 48_000.0;
        close(lufs(fs, &sine(fs, 997.0, -23.0, 8.0)), -23.0, 0.01);
        close(lufs(fs, &sine(fs, 997.0, -33.0, 8.0)), -33.0, 0.01);
        let seq = |parts: &[(f64, f64)]| parts.iter().flat_map(|&(db, s)| sine(fs, 997.0, db, s)).collect::<Vec<f32>>();
        close(lufs(fs, &seq(&[(-36.0, 10.0), (-23.0, 60.0), (-36.0, 10.0)])), -23.0, 0.1);
        close(lufs(fs, &seq(&[(-72.0, 10.0), (-36.0, 10.0), (-23.0, 60.0), (-36.0, 10.0), (-72.0, 10.0)])), -23.0, 0.1);
        close(lufs(fs, &seq(&[(-26.0, 20.0), (-20.0, 20.1), (-26.0, 20.0)])), -23.0, 0.1);
    }

    #[test]
    fn one_channel_at_full_scale_reads_minus_three() {
        let fs = 48_000.0;
        let s: Vec<f32> = sine(fs, 997.0, 0.0, 4.0).chunks(2).flat_map(|f| [f[0], 0.0]).collect();
        close(lufs(fs, &s), -3.01, 0.01);
    }

    #[test]
    fn k_weighting_shapes_the_spectrum() {
        let fs = 48_000.0;
        // −23 dBFS stereo: 20 Hz is ≈ 13.3 dB down, 8 kHz ≈ 4 dB up (the pre-filter shelf).
        close(lufs(fs, &sine(fs, 20.0, -23.0, 6.0)), -36.97, 0.05);
        close(lufs(fs, &sine(fs, 8_000.0, -23.0, 6.0)), -19.65, 0.05);
    }

    #[test]
    fn other_sample_rates_read_the_same_tone() {
        close(lufs(44_100.0, &sine(44_100.0, 997.0, -23.0, 6.0)), -23.0, 0.03);
        close(lufs(96_000.0, &sine(96_000.0, 997.0, -23.0, 6.0)), -23.0, 0.03);
    }

    #[test]
    fn unmeasurable_audio_is_none() {
        let fs = 48_000.0;
        assert_eq!(lufs(fs, &vec![0.0; 96_000 * 2]), None, "silence");
        assert_eq!(lufs(fs, &sine(fs, 997.0, -80.0, 4.0)), None, "under the absolute gate");
        assert_eq!(lufs(fs, &sine(fs, 997.0, -10.0, 0.3)), None, "shorter than one 400 ms block");
    }

    #[test]
    fn chunking_never_changes_the_result() {
        let fs = 48_000.0;
        let s = [sine(fs, 440.0, -20.0, 1.3), sine(fs, 3_000.0, -30.0, 1.1)].concat();
        let want = lufs(fs, &s).unwrap();
        for size in [1, 3, 7, 4801] {
            let mut m = LoudnessMeter::new(fs, 2);
            for c in s.chunks(size) {
                m.push_interleaved(c);
            }
            assert_eq!(m.integrated(), Some(want), "chunk size {size}");
        }
    }

    #[test]
    fn gain_regates_exactly_like_a_remeasurement() {
        let fs = 48_000.0;
        // Half the blocks sit under the −70 gate until a +12 dB gain lifts them over it (and they
        // are within 10 LU of the rest, so the relative gate keeps them too).
        let s = [sine(fs, 997.0, -66.0, 4.0), sine(fs, 997.0, -74.0, 4.0)].concat();
        let mut m = LoudnessMeter::new(fs, 2);
        m.push_interleaved(&s);
        let g = 12.0;
        let scaled: Vec<f32> = s.iter().map(|&x| (x as f64 * 10f64.powf(g / 20.0)) as f32).collect();
        let want = lufs(fs, &scaled).unwrap();
        let got = m.integrated_with_gain(g).unwrap();
        assert!((got - want).abs() < 1e-3, "{got} vs re-measured {want}");
        assert!((got - (m.integrated().unwrap() + g)).abs() > 0.5, "the gate moved, so it isn't a plain offset");
    }

    #[test]
    fn normalization_gain_hits_the_target_or_the_ceiling() {
        let fs = 48_000.0;
        // A steady tone: loudness-limited, both upward and downward.
        let quiet = sine(fs, 997.0, -30.0, 4.0);
        let mut m = LoudnessMeter::new(fs, 2);
        m.push_interleaved(&quiet);
        let g = normalization_gain(&m, 10f32.powf(-30.0 / 20.0), -18.0, -1.0).unwrap();
        assert_eq!(g.limited_by, Limit::Loudness);
        assert!((g.gain_db - 12.0).abs() < 0.01 && (g.out_lufs.unwrap() + 18.0).abs() < 1e-4, "{g:?}");
        let mut m = LoudnessMeter::new(fs, 2);
        m.push_interleaved(&sine(fs, 997.0, -6.0, 4.0));
        let g = normalization_gain(&m, 10f32.powf(-6.0 / 20.0), -18.0, -1.0).unwrap();
        assert!((g.gain_db + 12.0).abs() < 0.01 && g.limited_by == Limit::Loudness, "{g:?}");

        // High crest (a quiet bed + one near-full-scale click): the ceiling stops it short.
        let mut clicky = sine(fs, 997.0, -40.0, 4.0);
        clicky[96_000] = 0.9;
        let mut m = LoudnessMeter::new(fs, 2);
        m.push_interleaved(&clicky);
        let g = normalization_gain(&m, 0.9, -18.0, -1.0).unwrap();
        assert_eq!(g.limited_by, Limit::Peak);
        assert!((g.gain_db - (-1.0 - 20.0 * (0.9f32 as f64).log10())).abs() < 1e-9, "{g:?}");
        assert!(g.out_lufs.unwrap() < -18.0);

        // Nothing to normalize.
        let m = LoudnessMeter::new(fs, 2);
        assert_eq!(normalization_gain(&m, 0.0, -18.0, -1.0), None);
        let mut m = LoudnessMeter::new(fs, 2);
        m.push_interleaved(&sine(fs, 997.0, -80.0, 4.0));
        assert_eq!(normalization_gain(&m, 1e-4, -18.0, -1.0), None, "all under the gate");
    }
}
