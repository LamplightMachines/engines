//! Hand-rolled DSP primitives (ADR 0003 decision; CE-R4.4/R4.5).
//!
//! Minimal set: zipper-free [`Smoothed`] params, oscillators, a TPT state-variable
//! [`Svf`] filter, [`soft_clip`] drive, and a Freeverb-style [`Reverb`]. Deterministic;
//! all state owned explicitly so it can persist across loop boundaries (CE-R4.4) and be
//! re-rendered reproducibly (CE-R7.3).

use std::f32::consts::PI;

pub const TWO_PI: f32 = 2.0 * PI;

// ──────────────────────────────────────────────────────────────────────────
// Zipper-free parameter smoothing (CE-R4.5)
// ──────────────────────────────────────────────────────────────────────────

/// Linear-ramp smoothed value. Set a target; `next()` ramps toward it over `ramp_len`
/// samples, eliminating discrete jumps at control-event boundaries.
#[derive(Clone, Debug)]
pub struct Smoothed {
    current: f32,
    target: f32,
    inc: f32,
    steps_left: u32,
    ramp_len: u32,
}

impl Smoothed {
    pub fn new(initial: f32, ramp_len: u32) -> Self {
        Self { current: initial, target: initial, inc: 0.0, steps_left: 0, ramp_len }
    }

    /// Jump immediately (no ramp) — e.g. on (re)initialization.
    pub fn set_now(&mut self, v: f32) {
        self.current = v;
        self.target = v;
        self.steps_left = 0;
        self.inc = 0.0;
    }

    /// Ramp toward `v` over `ramp_len` samples.
    pub fn set_target(&mut self, v: f32) {
        if v == self.target {
            return;
        }
        self.target = v;
        if self.ramp_len == 0 {
            self.current = v;
            self.steps_left = 0;
        } else {
            self.inc = (v - self.current) / self.ramp_len as f32;
            self.steps_left = self.ramp_len;
        }
    }

    #[inline]
    pub fn next(&mut self) -> f32 {
        if self.steps_left > 0 {
            self.current += self.inc;
            self.steps_left -= 1;
            if self.steps_left == 0 {
                self.current = self.target;
            }
        }
        self.current
    }

    pub fn value(&self) -> f32 {
        self.current
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Oscillators (CE-R4.3) — phase in radians [0, 2π)
// ──────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wave {
    Sine,
    Triangle,
    SawSoft,
    /// Soft (tanh-rounded) square — band-limited-ish by the rounding, like `SawSoft`.
    Square,
    /// Soft ~30% pulse (difference of two soft saws) — narrower/nasal tone.
    Pulse,
    SineBell,
    /// filtered_noise — generated from white noise + bandpass elsewhere.
    Noise,
}

impl Wave {
    pub fn from_name(name: &str) -> Wave {
        match name {
            "triangle" => Wave::Triangle,
            "saw_soft" => Wave::SawSoft,
            "square" => Wave::Square,
            "pulse" => Wave::Pulse,
            "sine_bell" => Wave::SineBell,
            "filtered_noise" => Wave::Noise,
            _ => Wave::Sine,
        }
    }
}

/// A normalized soft saw at fractional phase `t` in [0,1) — shared by `SawSoft` and `Pulse`.
#[inline]
fn soft_saw(t: f32) -> f32 {
    let raw = 2.0 * t - 1.0;
    let d = (3.0_f32).tanh();
    if d > 0.0 {
        (3.0 * raw).tanh() / d
    } else {
        raw
    }
}

/// Sample an oscillator at `phase` (radians). `Noise` returns 0 here (handled by the
/// engine's RNG + bandpass path).
pub fn osc(wave: Wave, phase: f32) -> f32 {
    match wave {
        Wave::Sine => phase.sin(),
        Wave::Triangle => {
            let t = phase / TWO_PI;
            1.0 - 4.0 * (t - (t + 0.5).floor()).abs()
        }
        Wave::SawSoft => {
            let t = phase / TWO_PI;
            soft_saw(t)
        }
        Wave::Square => {
            // tanh-rounded square: rounded edges tame aliasing like the soft saw.
            (4.0 * phase.sin()).tanh()
        }
        Wave::Pulse => {
            // ~30%-duty pulse as a difference of two soft saws (PWM-style); mean ≈ 0.
            let duty = 0.3;
            let t1 = phase / TWO_PI;
            let t2 = (t1 - duty).rem_euclid(1.0);
            0.5 * (soft_saw(t1) - soft_saw(t2))
        }
        Wave::SineBell => {
            let t = phase / TWO_PI;
            phase.sin() * (1.0 - t).max(0.0).powi(2)
        }
        Wave::Noise => 0.0,
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Soft-clip drive (CE-R4.4)
// ──────────────────────────────────────────────────────────────────────────

/// `drive01`, `drive_db`'s normalized 0..1 (0 to +20 dB) → a pre-gain of 1×..10× through a
/// normalized tanh soft-clip. Bypassed below ×1.009 (+0.078 dB, v0.8's `drive` 0.001).
pub fn soft_clip(x: f32, drive01: f32) -> f32 {
    let g = crate::params::db_to_amp(crate::params::DRIVE.eval(drive01));
    if g <= 1.009 {
        return x;
    }
    let d = g.tanh();
    if d > 0.0 {
        (g * x).tanh() / d
    } else {
        x
    }
}

// ──────────────────────────────────────────────────────────────────────────
// ADSR envelope (CE-R4.3) — linear segments, sustain is a 0..1 level
// ──────────────────────────────────────────────────────────────────────────

/// ADSR amplitude (0..1) at `t_s` seconds since articulation. `released_s = Some(t)` puts
/// the voice in the release tail (note-off at `t` seconds-since-articulation); `None` = held.
/// Linear attack (0→1 over `attack_s`), decay (1→`sustain` over `decay_s`), hold at
/// `sustain`, then linear release (level-at-release → 0 over `release_s`).
#[inline]
pub fn adsr_env(t_s: f32, released_s: Option<f32>, attack_s: f32, decay_s: f32, sustain: f32, release_s: f32) -> f32 {
    let held = |t: f32| -> f32 {
        if attack_s > 0.0 && t < attack_s {
            t / attack_s
        } else if decay_s > 0.0 && t < attack_s + decay_s {
            1.0 - (1.0 - sustain) * (t - attack_s) / decay_s
        } else {
            sustain
        }
    };
    match released_s {
        None => held(t_s),
        Some(rel) => {
            let since = t_s - rel;
            if release_s <= 0.0 || since >= release_s {
                0.0
            } else {
                held(rel) * (1.0 - since / release_s)
            }
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// TPT state-variable filter (Zavalishin) — LP / BP / HP (CE-R4.4)
// ──────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct Svf {
    ic1eq: f32,
    ic2eq: f32,
    g: f32, // tan(π fc / fs); kept so resonance can recompute coeffs independently
    a1: f32,
    a2: f32,
    a3: f32,
    k: f32, // damping = 1/Q
    sample_rate: f32,
}

/// Map a normalized resonance 0..1 → filter Q. 0 ⇒ Q≈0.707 (Butterworth, no resonant peak);
/// 1 ⇒ Q≈6 (~15 dB peak at cutoff — a vowel-like/whistling character). Capped for level sanity.
#[inline]
pub fn resonance_to_q(res01: f32) -> f32 {
    crate::params::RESONANCE_Q.eval(res01.clamp(0.0, 1.0))
}

#[derive(Clone, Copy, Debug)]
pub struct SvfOut {
    pub lp: f32,
    pub bp: f32,
    pub hp: f32,
}

impl Svf {
    pub fn new(sample_rate: f32) -> Self {
        let mut s = Self {
            ic1eq: 0.0,
            ic2eq: 0.0,
            g: 0.0,
            a1: 0.0,
            a2: 0.0,
            a3: 0.0,
            k: 1.0 / 0.707,
            sample_rate,
        };
        s.set_cutoff(1000.0);
        s
    }

    pub fn reset(&mut self) {
        self.ic1eq = 0.0;
        self.ic2eq = 0.0;
    }

    #[inline]
    fn update_coeffs(&mut self) {
        self.a1 = 1.0 / (1.0 + self.g * (self.g + self.k));
        self.a2 = self.g * self.a1;
        self.a3 = self.g * self.a2;
    }

    /// Set cutoff (Hz), keeping the current resonance.
    pub fn set_cutoff(&mut self, freq_hz: f32) {
        let fc = freq_hz.clamp(20.0, self.sample_rate * 0.45);
        self.g = (PI * fc / self.sample_rate).tan();
        self.update_coeffs();
    }

    /// Set resonance (CE-R4.4 lever) from a normalized 0..1 value, keeping the current cutoff.
    pub fn set_resonance(&mut self, res01: f32) {
        self.k = 1.0 / resonance_to_q(res01);
        self.update_coeffs();
    }

    /// Set cutoff (Hz) + resonance (0..1) together (one coeff update — the per-sample path).
    pub fn set(&mut self, freq_hz: f32, res01: f32) {
        let fc = freq_hz.clamp(20.0, self.sample_rate * 0.45);
        self.g = (PI * fc / self.sample_rate).tan();
        self.k = 1.0 / resonance_to_q(res01);
        self.update_coeffs();
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> SvfOut {
        let v3 = x - self.ic2eq;
        let v1 = self.a1 * self.ic1eq + self.a2 * v3;
        let v2 = self.ic2eq + self.a2 * self.ic1eq + self.a3 * v3;
        self.ic1eq = 2.0 * v1 - self.ic1eq;
        self.ic2eq = 2.0 * v2 - self.ic2eq;
        SvfOut { lp: v2, bp: v1, hp: x - self.k * v1 - v2 }
    }

    #[inline]
    pub fn lowpass(&mut self, x: f32) -> f32 {
        self.process(x).lp
    }

    /// Set cutoff (Hz) and Q directly — for the fixed Butterworth stages of [`PitchBand`].
    pub fn set_q(&mut self, freq_hz: f32, q: f32) {
        let fc = freq_hz.clamp(20.0, self.sample_rate * 0.45);
        self.g = (PI * fc / self.sample_rate).tan();
        self.k = 1.0 / q.max(1e-3);
        self.update_coeffs();
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Pitched noise (ADR 0046): the note's band-pass after a noise voice's source band
// ──────────────────────────────────────────────────────────────────────────

/// The two stages' Qs of a 4th-order Butterworth (poles at ±22.5° and ±67.5°).
const BUTTERWORTH4_Q: [f32; 2] = [0.541_196_1, 1.306_563];
/// The note a pitched noise voice is level-matched at: constant-Q bands of white noise lose 3 dB
/// per octave down, so a voice at `f` Hz is scaled by `sqrt(PITCH_REF_HZ / f)` — even across notes.
pub const PITCH_REF_HZ: f32 = 1000.0;
/// The resonant peak at `resonance` 1: +24 dB.
const PITCH_PEAK_DB: f32 = 24.0;
/// How far the note, resonance or a source-band edge may move (log-frequency / resonance units)
/// before the loudness match is integrated again — far below anything audible (≈ 0.01 dB).
const MATCH_TOL: f32 = 1e-3;

/// The peak's Q at `res` (0..1): broad at first, narrowing to Q 30 at 1 (a near-whistle).
#[inline]
fn peak_q(res: f32) -> f32 {
    1.0 + 29.0 * res * res
}

/// The peak's gain at the note at `res` (linear).
#[inline]
fn peak_gain(res: f32) -> f32 {
    10f32.powf(PITCH_PEAK_DB * res / 20.0)
}

/// The gain that holds a pitched noise voice at its resonance-0 loudness: `sqrt(P(0) / P(res))`,
/// where `P` is the power of the source band's noise (`lo`…`hi`, the 2-pole low/high-pass the voice
/// already went through) out of the note's band at `hz`. With the peak `|1 + (G − 1)·B|² = 1 +
/// (G² − 1)·|B|²` (`B` the peak's unit band-pass), so `P(res) = P(0) + (G² − 1)·P_B`; both are
/// integrated over the analog prototypes (ω = 1 at the note). A note far outside the source band
/// keeps its resonance-0 attenuation — the peak there finds little noise, and is matched to that.
fn loudness_match(hz: f32, res: f32, lo: f32, hi: f32) -> f32 {
    if res <= 0.0 {
        return 1.0;
    }
    use std::f64::consts::{LN_2, PI};
    let (q, g) = (peak_q(res) as f64, peak_gain(res) as f64);
    let (a, b) = (lo as f64 / hz as f64, hi as f64 / hz as f64);
    // The source band × the note's band (×2 so the note is at 0 dB), as power at ω.
    let shaped = |w: f64| {
        let (x, z) = ((w / a).powi(4), (w / b).powi(4));
        x / (1.0 + x) / (1.0 + z) * 4.0 * w.powi(8) / (1.0 + w.powi(8)).powi(2)
    };
    // P(0) = ∫ shaped dω: a log grid over ±7 octaves (the note's band is ≥ 160 dB down past them).
    let (n, span) = (72, 14.0 * LN_2);
    let p0: f64 = (0..n)
        .map(|i| {
            let w = (-span / 2.0 + span * (i as f64 + 0.5) / n as f64).exp();
            shaped(w) * w * span / n as f64
        })
        .sum();
    // P_B = ∫ shaped·|B|² dω, |B|² = 1/(1 + y²) with y = Q(ω − 1/ω): substituting y = tan θ leaves
    // ∫ shaped · ω² / (Q(ω² + 1)) dθ over (−π/2, π/2) — uniform in θ, accurate however narrow the peak.
    let m = 48;
    let pb: f64 = (0..m)
        .map(|i| {
            let u = (-PI / 2.0 + PI * (i as f64 + 0.5) / m as f64).tan() / q;
            let w = (u + (u * u + 4.0).sqrt()) / 2.0;
            shaped(w) * w * w / (q * (w * w + 1.0)) * PI / m as f64
        })
        .sum();
    if p0 <= 0.0 {
        return 1.0;
    }
    (p0 / (p0 + (g * g - 1.0) * pb)).sqrt() as f32
}

/// A pitched noise voice's band (ADR 0046), applied after its source band (`band_lo_hz`…`band_hi_hz`):
/// a 4-pole Butterworth high-pass and low-pass both at the note, so the response falls 24 dB/oct
/// either side of it (0 dB at the note); then a peak at the note that `resonance` raises (none at
/// 0, +24 dB and narrow at 1). The output is level-matched across notes (`sqrt(PITCH_REF_HZ / f)`)
/// and loudness-matched across resonance for the noise actually coming in ([`loudness_match`]):
/// the peak shapes the band, it doesn't turn it up. Filter coefficients follow the note (glide) and
/// the resonance exactly; the loudness match is integrated again when the note, the resonance or a
/// source-band edge has moved by more than [`MATCH_TOL`].
#[derive(Clone, Debug)]
pub struct PitchBand {
    hp: [Svf; 2],
    lp: [Svf; 2],
    peak: Svf,
    hz: f32,
    res: f32,
    /// `(G − 1) / Q`: the peak's band-pass (its gain at the note is `Q`) added on top of the band.
    peak_mix: f32,
    /// ×2 (the note at 0 dB) · the note's level match.
    gain: f32,
    /// The resonance's loudness match, and the (note, resonance, lo, hi) it was integrated for
    /// (`None` until the first sample).
    matched: f32,
    matched_for: Option<[f32; 4]>,
}

impl PitchBand {
    pub fn new(sample_rate: f32) -> Self {
        let svf = Svf::new(sample_rate);
        Self { hp: [svf.clone(), svf.clone()], lp: [svf.clone(), svf.clone()], peak: svf, hz: f32::NAN, res: f32::NAN, peak_mix: 0.0, gain: 1.0, matched: 1.0, matched_for: None }
    }

    /// A frequency as the filters see it (their clamp).
    #[inline]
    fn clamp_hz(&self, hz: f32) -> f32 {
        hz.clamp(20.0, self.peak.sample_rate * 0.45)
    }

    fn retune(&mut self, hz: f32, res: f32) {
        for (f, q) in self.hp.iter_mut().zip(BUTTERWORTH4_Q) {
            f.set_q(hz, q);
        }
        for (f, q) in self.lp.iter_mut().zip(BUTTERWORTH4_Q) {
            f.set_q(hz, q);
        }
        let q = peak_q(res);
        self.peak.set_q(hz, q);
        self.peak_mix = (peak_gain(res) - 1.0) / q;
        self.gain = 2.0 * (PITCH_REF_HZ / self.clamp_hz(hz)).sqrt();
        (self.hz, self.res) = (hz, res);
    }

    /// One sample of the source-band noise `x` (already through `band_lo_hz`…`band_hi_hz`) through
    /// the band of the note at `hz`.
    #[inline]
    pub fn process(&mut self, x: f32, hz: f32, resonance: f32, band_lo_hz: f32, band_hi_hz: f32) -> f32 {
        let res = resonance.clamp(0.0, 1.0);
        if hz != self.hz || res != self.res {
            self.retune(hz, res);
        }
        let now = [self.clamp_hz(hz), res, self.clamp_hz(band_lo_hz), self.clamp_hz(band_hi_hz)];
        let moved = |a: f32, b: f32| (a / b).ln().abs() > MATCH_TOL;
        // The band edges only matter with a peak; resonance reaching exactly 0 is exactly unmatched.
        let stale = match self.matched_for {
            None => true,
            Some([h0, r0, l0, u0]) => moved(now[0], h0) || (now[1] - r0).abs() > MATCH_TOL || (res == 0.0) != (r0 == 0.0) || (res > 0.0 && (moved(now[2], l0) || moved(now[3], u0))),
        };
        if stale {
            self.matched = loudness_match(now[0], now[1], now[2], now[3]);
            self.matched_for = Some(now);
        }
        let mut y = x;
        for f in self.hp.iter_mut() {
            y = f.process(y).hp;
        }
        for f in self.lp.iter_mut() {
            y = f.process(y).lp;
        }
        (y + self.peak_mix * self.peak.process(y).bp) * self.gain * self.matched
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Freeverb-style reverb (CE-R4.4) — persistent state crosses loop boundaries
// ──────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub(crate) struct Comb {
    buf: Vec<f32>,
    idx: usize,
    filterstore: f32,
    pub(crate) feedback: f32,
    pub(crate) damp1: f32,
    pub(crate) damp2: f32,
}
impl Comb {
    pub(crate) fn new(size: usize) -> Self {
        Self { buf: vec![0.0; size.max(1)], idx: 0, filterstore: 0.0, feedback: 0.5, damp1: 0.5, damp2: 0.5 }
    }
    #[inline]
    pub(crate) fn process(&mut self, input: f32) -> f32 {
        let out = self.buf[self.idx];
        self.filterstore = out * self.damp2 + self.filterstore * self.damp1;
        self.buf[self.idx] = input + self.filterstore * self.feedback;
        self.idx = (self.idx + 1) % self.buf.len();
        out
    }
}

#[derive(Clone, Debug)]
pub(crate) struct AllPass {
    buf: Vec<f32>,
    idx: usize,
    pub(crate) feedback: f32,
}
impl AllPass {
    pub(crate) fn new(size: usize) -> Self {
        Self { buf: vec![0.0; size.max(1)], idx: 0, feedback: 0.5 }
    }
    #[inline]
    pub(crate) fn process(&mut self, input: f32) -> f32 {
        let bufout = self.buf[self.idx];
        let out = -input + bufout;
        self.buf[self.idx] = input + bufout * self.feedback;
        self.idx = (self.idx + 1) % self.buf.len();
        out
    }
}

pub(crate) const COMB_TUNINGS: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
pub(crate) const ALLPASS_TUNINGS: [usize; 4] = [556, 441, 341, 225];
pub(crate) const STEREO_SPREAD: usize = 23;

/// Stereo Freeverb. State persists so reverb tails cross the loop seam (CE-R4.4).
#[derive(Clone, Debug)]
pub struct Reverb {
    combs_l: Vec<Comb>,
    combs_r: Vec<Comb>,
    aps_l: Vec<AllPass>,
    aps_r: Vec<AllPass>,
}

impl Reverb {
    pub fn new(sample_rate: f32) -> Self {
        let scale = sample_rate / 44100.0;
        let s = |n: usize| ((n as f32) * scale) as usize;
        let mut rv = Self {
            combs_l: COMB_TUNINGS.iter().map(|&t| Comb::new(s(t))).collect(),
            combs_r: COMB_TUNINGS.iter().map(|&t| Comb::new(s(t) + s(STEREO_SPREAD))).collect(),
            aps_l: ALLPASS_TUNINGS.iter().map(|&t| AllPass::new(s(t))).collect(),
            aps_r: ALLPASS_TUNINGS.iter().map(|&t| AllPass::new(s(t) + s(STEREO_SPREAD))).collect(),
        };
        rv.set_params(0.5, 0.5);
        for ap in rv.aps_l.iter_mut().chain(rv.aps_r.iter_mut()) {
            ap.feedback = 0.5;
        }
        rv
    }

    /// `room_size` and `damping` in 0..1.
    pub fn set_params(&mut self, room_size: f32, damping: f32) {
        let feedback = room_size * 0.28 + 0.7;
        let damp1 = damping * 0.4;
        let damp2 = 1.0 - damp1;
        for c in self.combs_l.iter_mut().chain(self.combs_r.iter_mut()) {
            c.feedback = feedback;
            c.damp1 = damp1;
            c.damp2 = damp2;
        }
    }

    /// Process one stereo sample → wet stereo (caller blends dry/wet, CE-R4.6).
    #[inline]
    pub fn process(&mut self, in_l: f32, in_r: f32) -> (f32, f32) {
        let input = (in_l + in_r) * 0.015; // fixed gain into the reverb network
        let mut out_l = 0.0;
        let mut out_r = 0.0;
        for c in self.combs_l.iter_mut() {
            out_l += c.process(input);
        }
        for c in self.combs_r.iter_mut() {
            out_r += c.process(input);
        }
        for ap in self.aps_l.iter_mut() {
            out_l = ap.process(out_l);
        }
        for ap in self.aps_r.iter_mut() {
            out_r = ap.process(out_r);
        }
        (out_l, out_r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoothed_ramps_without_overshoot() {
        let mut s = Smoothed::new(0.0, 4);
        s.set_target(1.0);
        let seq: Vec<f32> = (0..6).map(|_| s.next()).collect();
        // monotonic non-decreasing, reaches target, never overshoots
        for w in seq.windows(2) {
            assert!(w[1] >= w[0], "not monotonic: {seq:?}");
            assert!(w[1] <= 1.0 + 1e-6, "overshoot: {seq:?}");
        }
        assert!((seq[5] - 1.0).abs() < 1e-6, "did not reach target: {seq:?}");
    }

    /// A source band wider than anything these tests play (the filters' own clamp).
    const WIDE: (f32, f32) = (20.0, 21_600.0);

    /// Steady-state gain (dB) of a sine at `freq` through a fresh [`PitchBand`] at `hz` / `res`.
    fn pitch_band_db(sr: f32, hz: f32, res: f32, freq: f32) -> f32 {
        let mut pb = PitchBand::new(sr);
        let n = (sr * 2.0) as usize;
        let (mut acc, mut ref_acc) = (0.0f64, 0.0f64);
        for i in 0..n {
            // f64 phase: an f32 one at 16 kHz over 96k samples is phase noise that leaks through the band.
            let x = (std::f64::consts::TAU * freq as f64 * i as f64 / sr as f64).sin() as f32;
            let y = pb.process(x, hz, res, WIDE.0, WIDE.1);
            if i > n / 2 {
                acc += (y as f64).powi(2);
                ref_acc += (x as f64).powi(2);
            }
        }
        10.0 * (acc / ref_acc).log10() as f32
    }

    /// RMS (dB) of white noise through a [`PitchBand`] at `hz` / `res`.
    fn pitch_band_noise_db(sr: f32, hz: f32, res: f32) -> f32 {
        source_band_noise_db(sr, hz, res, WIDE)
    }

    /// RMS (dB) of white noise through a source band `lo`…`hi` (the engine's 2-pole low/high-pass)
    /// and then a [`PitchBand`] at `hz` / `res` — the engine's pitched-noise path.
    fn source_band_noise_db(sr: f32, hz: f32, res: f32, band: (f32, f32)) -> f32 {
        source_band_noise_db_over(sr, hz, res, band, 8.0)
    }

    /// [`source_band_noise_db`] over `secs` of noise: a low note's narrow peak needs longer for its
    /// power to settle (a 3.7 Hz-wide peak at D2 has few degrees of freedom in 8 s).
    fn source_band_noise_db_over(sr: f32, hz: f32, res: f32, (lo, hi): (f32, f32), secs: f32) -> f32 {
        let (mut lp, mut hp) = (Svf::new(sr), Svf::new(sr));
        lp.set_cutoff(hi);
        hp.set_cutoff(lo);
        let mut pb = PitchBand::new(sr);
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let n = (sr * secs) as usize;
        let mut acc = 0.0f64;
        for i in 0..n {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let x = (state >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0;
            let x = if (lo, hi) == WIDE { x } else { hp.process(lp.lowpass(x)).hp };
            let y = pb.process(x, hz, res, lo, hi);
            if i > n / 8 {
                acc += (y as f64).powi(2);
            }
        }
        10.0 * (acc / (n - n / 8) as f64).log10() as f32
    }

    // The note's band: 0 dB at the note (level-matched at PITCH_REF_HZ), falling 24 dB/oct either
    // side once clear of the note — the slope between 2 and 4 octaves away, both ways.
    #[test]
    fn pitch_band_is_centred_on_the_note_with_24_db_per_octave_skirts() {
        let sr = 48_000.0;
        let hz = PITCH_REF_HZ;
        assert!(pitch_band_db(sr, hz, 0.0, hz).abs() < 0.2, "0 dB at the note: {}", pitch_band_db(sr, hz, 0.0, hz));
        let below = pitch_band_db(sr, hz, 0.0, hz / 4.0) - pitch_band_db(sr, hz, 0.0, hz / 16.0);
        let above = pitch_band_db(sr, hz, 0.0, hz * 4.0) - pitch_band_db(sr, hz, 0.0, hz * 16.0);
        assert!((below / 2.0 - 24.0).abs() < 1.0, "24 dB/oct below: {:.1} dB/oct", below / 2.0);
        assert!(above / 2.0 > 23.0, "at least 24 dB/oct above (the bilinear map steepens it toward Nyquist): {:.1} dB/oct", above / 2.0);
        // The band follows the note: A440's band peaks at 440, not at 880.
        assert!(pitch_band_db(sr, 440.0, 0.0, 440.0) > pitch_band_db(sr, 440.0, 0.0, 880.0) + 15.0);
    }

    // Resonance raises the note above the band: at 1, the note stands ~+24 dB over its flat-band
    // level relative to the skirts (a near-whistle), while the band's loudness stays put.
    #[test]
    fn pitch_band_resonance_makes_the_note_pronounced_at_the_same_loudness() {
        let sr = 48_000.0;
        let hz = 500.0;
        let contrast = |res: f32| pitch_band_db(sr, hz, res, hz) - pitch_band_db(sr, hz, res, hz * 1.5);
        let lift = contrast(1.0) - contrast(0.0);
        assert!(lift > 20.0, "the note stands out at resonance 1: +{lift:.1} dB over the band");
        assert!(contrast(0.5) - contrast(0.0) > 6.0, "and rises with resonance");
        for res in [0.25, 0.5, 1.0] {
            let d = pitch_band_noise_db(sr, hz, res) - pitch_band_noise_db(sr, hz, 0.0);
            assert!(d.abs() < 1.0, "loudness at resonance {res}: {d:+.2} dB");
        }
    }

    // Resonance holds the loudness of the noise that actually comes in: the source band first (here
    // ninety-lights' 566 Hz – 3.2 kHz). A note below it (D4, 110 Hz) or at its edge keeps its
    // resonance-0 level — its peak finds little noise, and the match accounts for that.
    #[test]
    fn pitch_band_resonance_holds_loudness_through_a_source_band() {
        let sr = 48_000.0;
        let band = (566.0, 3200.0);
        for hz in [110.0, 293.66, 587.33, 1174.66] {
            let flat = source_band_noise_db(sr, hz, 0.0, band);
            for res in [0.5, 1.0] {
                let d = source_band_noise_db(sr, hz, res, band) - flat;
                assert!(d.abs() < 0.5, "{hz} Hz in 566–3200 Hz at resonance {res}: {d:+.2} dB");
            }
        }
    }

    // Even across notes: white noise through a 250 Hz and a 4 kHz band comes out equally loud.
    #[test]
    fn pitch_band_is_level_matched_across_notes() {
        let sr = 48_000.0;
        let (lo, mid, hi) = (pitch_band_noise_db(sr, 250.0, 0.0), pitch_band_noise_db(sr, 1000.0, 0.0), pitch_band_noise_db(sr, 4000.0, 0.0));
        assert!((lo - mid).abs() < 1.0 && (hi - mid).abs() < 1.0, "250 Hz {lo:.2} · 1 kHz {mid:.2} · 4 kHz {hi:.2} dB");
    }

    // Low notes get noise once the source band opens under them: through a 20 Hz – 4 kHz
    // band, B0, D2 and D3 come out about as loud as 1 kHz, at resonance 0 and with a peak.
    #[test]
    fn pitch_band_is_level_matched_down_to_b0_through_an_open_source_band() {
        let (sr, secs) = (8_000.0, 32.0);
        let band = (20.0, 4000.0);
        let mid = source_band_noise_db_over(sr, 1000.0, 0.0, band, secs);
        for hz in [30.87, 73.42, 146.83] {
            let flat = source_band_noise_db_over(sr, hz, 0.0, band, secs);
            let d = source_band_noise_db_over(sr, hz, 0.8, band, secs) - flat;
            assert!((flat - mid).abs() < 1.5, "{hz} Hz in 20–4000 Hz: {flat:.2} dB vs 1 kHz {mid:.2} dB");
            assert!(d.abs() < 0.5, "{hz} Hz at resonance 0.8: {d:+.2} dB");
        }
    }

    #[test]
    fn smoothed_zero_ramp_is_instant() {
        let mut s = Smoothed::new(0.0, 0);
        s.set_target(0.5);
        assert_eq!(s.next(), 0.5);
    }

    #[test]
    fn svf_lowpass_attenuates_highs_more_than_lows() {
        let sr = 48_000.0;
        let cutoff = 500.0;
        let rms = |freq: f32| {
            let mut f = Svf::new(sr);
            f.set_cutoff(cutoff);
            let mut acc = 0.0f64;
            let n = 4800;
            for i in 0..n {
                let x = (TWO_PI * freq * i as f32 / sr).sin();
                let y = f.lowpass(x);
                if i > n / 2 {
                    acc += (y as f64) * (y as f64);
                }
            }
            (acc / (n as f64 / 2.0)).sqrt()
        };
        let low = rms(100.0);
        let high = rms(8000.0);
        assert!(low > high * 3.0, "lowpass should pass lows, cut highs: low={low} high={high}");
    }

    #[test]
    fn svf_resonance_boosts_energy_at_cutoff() {
        let sr = 48_000.0;
        let fc = 1000.0;
        let rms_at_fc = |res: f32| {
            let mut f = Svf::new(sr);
            f.set(fc, res);
            let (mut acc, n) = (0.0f64, 9600);
            for i in 0..n {
                let x = (TWO_PI * fc * i as f32 / sr).sin();
                let y = f.lowpass(x);
                if i > n / 2 {
                    acc += (y as f64).powi(2);
                }
            }
            (acc / (n as f64 / 2.0)).sqrt()
        };
        let flat = rms_at_fc(0.0);
        let resonant = rms_at_fc(1.0);
        assert!(resonant > flat * 1.5, "resonance should boost energy at the cutoff: flat={flat} resonant={resonant}");
        assert!(resonant.is_finite());
    }

    #[test]
    fn adsr_segments_and_release_tail() {
        let (a, d, s, r) = (0.1, 0.2, 0.5, 0.4); // seconds; sustain level 0.5
        assert!((adsr_env(0.0, None, a, d, s, r) - 0.0).abs() < 1e-6, "starts at 0");
        assert!((adsr_env(0.05, None, a, d, s, r) - 0.5).abs() < 1e-6, "attack half-way ≈ 0.5");
        assert!((adsr_env(0.1, None, a, d, s, r) - 1.0).abs() < 1e-6, "peak at end of attack");
        assert!((adsr_env(0.3, None, a, d, s, r) - 0.5).abs() < 1e-6, "decayed to sustain");
        assert!((adsr_env(2.0, None, a, d, s, r) - 0.5).abs() < 1e-6, "holds at sustain");
        // released at t=2.0; tail from 0.5 → 0 over 0.4 s
        assert!((adsr_env(2.2, Some(2.0), a, d, s, r) - 0.25).abs() < 1e-6, "half through release");
        assert!((adsr_env(2.5, Some(2.0), a, d, s, r)).abs() < 1e-6, "tail done");
        // instant attack+decay (a=d=0), full sustain → flat 1.0 (back-compat default)
        assert!((adsr_env(0.0, None, 0.0, 0.0, 1.0, 0.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn soft_clip_raises_level_for_small_input() {
        let x = 0.2;
        assert!(soft_clip(x, 0.9) > x, "drive should raise a small signal");
        assert_eq!(soft_clip(x, 0.0), x, "no drive is identity");
    }

    #[test]
    fn oscillators_bounded_and_distinct() {
        for w in [Wave::Sine, Wave::Triangle, Wave::SawSoft, Wave::Square, Wave::Pulse, Wave::SineBell] {
            for i in 0..64 {
                let p = TWO_PI * i as f32 / 64.0;
                let s = osc(w, p);
                assert!(s.is_finite() && s.abs() <= 1.5, "{w:?} out of range at {p}: {s}");
            }
        }
        let p = TWO_PI * 0.3;
        assert!((osc(Wave::Sine, p) - osc(Wave::Triangle, p)).abs() > 1e-3);
        assert!((osc(Wave::Square, p) - osc(Wave::Pulse, p)).abs() > 1e-3, "square/pulse distinct");
    }

    #[test]
    fn reverb_produces_decaying_persistent_tail() {
        let mut rv = Reverb::new(48_000.0);
        // one impulse, then silence
        let (mut l, mut r) = rv.process(1.0, 1.0);
        let early = (l.abs() + r.abs()).max(1e-9);
        let mut tail_energy = 0.0f64;
        for _ in 0..24_000 {
            let (nl, nr) = rv.process(0.0, 0.0);
            l = nl;
            r = nr;
            tail_energy += (l as f64).powi(2) + (r as f64).powi(2);
        }
        assert!(tail_energy > 0.0, "reverb should ring out after the impulse");
        assert!(early.is_finite());
    }
}
