//! Self-contained synth types (ADR 0009).
//!
//! Every instrument selects a [`SynthKind`] via its `type` field (absent ⇒ [`SynthKind::Subtractive`],
//! the legacy single-oscillator path). A synth type owns only its **source stage** — the raw
//! pre-drive signal — while the engine keeps the shared `drive → filter → amp → pan → submix → sends`
//! chain. `Subtractive` and `Dual` ride the shared filter + ADSR; `Modal` opts out (intrinsic decay,
//! no resonant filter), mirroring how the engine already special-cases noise/`event`.
//!
//! Per-voice DSP state that must survive the loop seam lives in [`SynthVoiceState`], carried inside
//! `ActiveVoice` and serialized in the snapshot (CE-R7.3 determinism): same `loop_id` + snapshot ⇒
//! bit-identical output. State is zero-initialized deterministically at articulation; nothing here
//! reads a clock or RNG.

use crate::dsp::{osc, soft_clip, Wave, TWO_PI};
use crate::loop_doc::Instrument;
use serde::{Deserialize, Serialize};
use std::f32::consts::PI;

// ── Per-instrument synth selection + prepared config ────────────────────────

/// Per-instrument synth type, prepared once per render from the doc.
#[derive(Clone, Debug)]
pub enum SynthKind {
    /// Today's path: one oscillator (+ optional octave-pedal), shared filter + ADSR.
    Subtractive,
    /// Two oscillators (Hydrasynth-style): each goes to the mixer at its own level; osc B can
    /// additionally phase-modulate osc A (DX-style PM) with operator self-feedback.
    Dual(DualConfig),
    /// Modal/physical model: a parallel bank of decaying two-pole resonators excited at strike.
    Modal(ModalConfig),
    /// External-MIDI translator (ADR 0026): produces **no audio** — the engine skips its
    /// synthesis/filter/envelope entirely and instead diffs its voice set into MIDI
    /// Note-On/Off + samples its mapped params as CC (see `engine.rs` / `midi.rs`). Voice
    /// allocation (polyphony, stealing, sustain semantics) is shared with audio synths.
    Midi,
    /// Binaural beat pair (ADR 0047): two subtractive voices — the left hard LEFT at
    /// root − spread/2, the right hard RIGHT at root + spread/2 — sharing every subtractive param.
    /// The only type whose source is stereo ([`SynthKind::hard_lr`]).
    Binaural(BinauralConfig),
}

/// Amplitude shaping the engine applies after the source stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AmpPolicy {
    /// Full ADSR with release tail (CE-R4.3).
    Adsr,
    /// Exponential bell decay (the legacy `event` instrument).
    Bell,
    /// The synth supplies its own decay (modal); the engine applies only `level_db`.
    Intrinsic,
}

impl SynthKind {
    /// Build from the instrument's `type` field; absent ⇒ subtractive. The `_` fallback is
    /// unreachable from authored docs — `LoopDoc::validate()` rejects unknown type strings at
    /// the authoring boundary — and kept only for internally-constructed docs' robustness.
    pub fn from_doc(inst: &Instrument) -> SynthKind {
        match inst.synth_type() {
            "dual" => SynthKind::Dual(DualConfig::from_doc(inst)),
            "modal" => SynthKind::Modal(ModalConfig::from_doc(inst)),
            "midi" => SynthKind::Midi,
            "binaural" => SynthKind::Binaural(BinauralConfig::from_doc(inst)),
            _ => SynthKind::Subtractive,
        }
    }

    /// Does the engine apply its shared per-voice filter (lowpass / noise bandpass)?
    pub fn uses_engine_filter(&self) -> bool {
        !matches!(self, SynthKind::Modal(_) | SynthKind::Midi)
    }

    /// Does the synth apply `drive_db` (soft-clip) inside its own source stage? `Dual` does, so its
    /// drive harmonics are oversampled/band-limited (ADR 0009); the engine must then skip the
    /// shared soft-clip for it.
    pub fn drive_in_source(&self) -> bool {
        matches!(self, SynthKind::Dual(_))
    }

    /// Is the dry output a hard-L/R stereo pair that bypasses the master `stereo_width`
    /// (ADR 0047)? Only `Binaural`: its left and right must never bleed into each other.
    pub fn hard_lr(&self) -> bool {
        matches!(self, SynthKind::Binaural(_))
    }

    /// Amp policy. `is_event` only matters for the legacy subtractive path; a typed synth ignores
    /// the name-based `event` special case. `Midi` is silent, so its policy is moot (`Intrinsic`
    /// keeps it out of the ADSR path if anything ever samples it).
    pub fn amp_policy(&self, is_event: bool) -> AmpPolicy {
        match self {
            SynthKind::Modal(_) | SynthKind::Midi => AmpPolicy::Intrinsic,
            SynthKind::Subtractive if is_event => AmpPolicy::Bell,
            _ => AmpPolicy::Adsr,
        }
    }
}

// ── Per-voice DSP state (serializable; crosses the loop seam) ────────────────

/// Extra per-voice DSP state for non-subtractive synths. `None` (subtractive) is skipped when
/// serializing, so existing snapshots stay byte-identical.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SynthVoiceState {
    /// Subtractive — no extra state.
    None,
    /// Dual-osc: osc B (modulator) phase + DX-style averaged self-feedback memory. Osc A reuses the
    /// voice's `phase_rad`.
    Dual { phase_b: f32, fb1: f32, fb2: f32 },
    /// Modal: per-mode two-pole resonator memories (`y[n-1]`, `y[n-2]`); length == number of modes.
    Modal { y1: Vec<f32>, y2: Vec<f32> },
    /// Binaural (ADR 0047): the right side's oscillator + octave-pedal phases. The left side reuses
    /// the voice's `phase_rad` / `phase_rad_shift`, so both start at 0 on articulation.
    Binaural { phase_r: f32, phase_shift_r: f32 },
}

impl Default for SynthVoiceState {
    fn default() -> Self {
        SynthVoiceState::None
    }
}

impl SynthVoiceState {
    pub fn is_none(&self) -> bool {
        matches!(self, SynthVoiceState::None)
    }

    /// Deterministic zeroed initial state for `kind` (called at articulation, like the filter reset).
    pub fn init_for(kind: &SynthKind) -> SynthVoiceState {
        match kind {
            SynthKind::Subtractive | SynthKind::Midi => SynthVoiceState::None,
            SynthKind::Dual(_) => SynthVoiceState::Dual { phase_b: 0.0, fb1: 0.0, fb2: 0.0 },
            SynthKind::Modal(cfg) => {
                let n = cfg.partials.len();
                SynthVoiceState::Modal { y1: vec![0.0; n], y2: vec![0.0; n] }
            }
            SynthKind::Binaural(_) => SynthVoiceState::Binaural { phase_r: 0.0, phase_shift_r: 0.0 },
        }
    }

    /// Resize a carried `Modal` state to `n` modes (deterministic truncate / zero-extend). No-op for
    /// other variants. Used at seed time when `num_modes` changed between loops.
    pub fn reshape_modal(&mut self, n: usize) {
        if let SynthVoiceState::Modal { y1, y2 } = self {
            y1.resize(n, 0.0);
            y2.resize(n, 0.0);
        }
    }
}

// ── Binaural pair (ADR 0047) ────────────────────────────────────────────────

/// A binaural instrument's own params, prepared once per render from the doc — in **real Hz**,
/// exactly as authored (never through the 0..1 seam).
#[derive(Clone, Debug)]
pub struct BinauralConfig {
    /// `root_hz`: the pair's root. `None` ⇒ the root is each voice's note pitch.
    pub root_hz: Option<f32>,
    /// `spread_hz`: the L/R spread — the beat rate. Absent ⇒ 0 (no beat; engines-check warns).
    pub spread_hz: f32,
}

impl BinauralConfig {
    pub fn from_doc(inst: &Instrument) -> Self {
        BinauralConfig { root_hz: inst.native("root_hz"), spread_hz: inst.native_or_default("spread_hz") }
    }
}

/// The pair around `center` Hz: left = center − spread/2 (floored at 0 Hz), right = center + spread/2.
/// 110 / 2 ⇒ (109, 111).
pub fn binaural_pair(center: f32, spread: f32) -> (f32, f32) {
    ((center - spread * 0.5).max(0.0), center + spread * 0.5)
}

/// A pinned `root_hz` with a unison voice's `voices_detune_cents` offset applied. Zero detune
/// returns `root_hz` untouched, so an authored 109.5 sounds at exactly 109.5.
pub fn binaural_root(root_hz: f32, detune_cents: f32) -> f32 {
    if detune_cents == 0.0 {
        root_hz
    } else {
        root_hz * cents_ratio(detune_cents)
    }
}

/// One binaural sample pair `(left, right)`: the subtractive oscillator — and its octave pedal
/// (`hz_shift`, blended at `shift_mix`; `None` ⇒ pure dry) — run once per side at
/// [`binaural_pair`] of the centre frequency. The pedal oscillator gets the same ±spread/2 Hz, so
/// the whole instrument beats at one rate. `phase_l`/`phase_shift_l` are the voice's own phases;
/// the right side's live in `st`.
#[allow(clippy::too_many_arguments)]
pub fn binaural_sample(
    wave: Wave,
    st: &mut SynthVoiceState,
    phase_l: &mut f32,
    phase_shift_l: &mut f32,
    hz: f32,
    hz_shift: Option<f32>,
    spread: f32,
    shift_mix: f32,
    sr: f32,
) -> (f32, f32) {
    let (phase_r, phase_shift_r) = match st {
        SynthVoiceState::Binaural { phase_r, phase_shift_r } => (phase_r, phase_shift_r),
        _ => {
            *st = SynthVoiceState::Binaural { phase_r: 0.0, phase_shift_r: 0.0 };
            return binaural_sample(wave, st, phase_l, phase_shift_l, hz, hz_shift, spread, shift_mix, sr);
        }
    };
    let (hz_l, hz_r) = binaural_pair(hz, spread);
    *phase_l = (*phase_l + TWO_PI * hz_l / sr).rem_euclid(TWO_PI);
    *phase_r = (*phase_r + TWO_PI * hz_r / sr).rem_euclid(TWO_PI);
    let (dry_l, dry_r) = (osc(wave, *phase_l), osc(wave, *phase_r));
    match hz_shift {
        Some(hs) => {
            let (hs_l, hs_r) = binaural_pair(hs, spread);
            *phase_shift_l = (*phase_shift_l + TWO_PI * hs_l / sr).rem_euclid(TWO_PI);
            *phase_shift_r = (*phase_shift_r + TWO_PI * hs_r / sr).rem_euclid(TWO_PI);
            (
                (1.0 - shift_mix) * dry_l + shift_mix * osc(wave, *phase_shift_l),
                (1.0 - shift_mix) * dry_r + shift_mix * osc(wave, *phase_shift_r),
            )
        }
        None => (dry_l, dry_r),
    }
}

// ── Dual-oscillator synth ───────────────────────────────────────────────────

/// Max phase-modulation depth (radians) at `fm_index = 1`.
pub const FM_INDEX_MAX: f32 = 8.0;
/// Max osc-B detune (cents) at `osc_b_detune = 1`.
pub const DETUNE_CENTS_MAX: f32 = 50.0;
/// Dual-synth oversampling factor: osc + PM + drive run at `OVERSAMPLE×` the rate, then decimate
/// through [`Aa2x`] (ADR 0009). 2× tames PM/drive sidebands that would otherwise fold.
pub const OVERSAMPLE: usize = 2;

// ── 2× anti-alias decimation (a 4th-order Butterworth, biquad cascade) ───────

#[derive(Clone, Copy, Debug)]
struct BiquadCoeffs {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

/// RBJ low-pass biquad at normalized cutoff `fc` (cycles/sample) and quality `q`.
fn butter_lp(fc: f32, q: f32) -> BiquadCoeffs {
    let w0 = TWO_PI * fc;
    let (sn, cs) = w0.sin_cos();
    let alpha = sn / (2.0 * q);
    let a0 = 1.0 + alpha;
    BiquadCoeffs {
        b0: (1.0 - cs) * 0.5 / a0,
        b1: (1.0 - cs) / a0,
        b2: (1.0 - cs) * 0.5 / a0,
        a1: -2.0 * cs / a0,
        a2: (1.0 - alpha) / a0,
    }
}

/// A 2× decimation low-pass (4th-order Butterworth ≈ 0.45·base-rate) with carried state. Lives in
/// the engine per-voice (like the per-voice `Svf`), reset on articulation — so it crosses the loop
/// seam deterministically without bloating the serialized snapshot.
#[derive(Clone, Debug)]
pub struct Aa2x {
    c: [BiquadCoeffs; 2],
    s: [[f32; 2]; 2], // Direct-Form-II Transposed state per section
}

impl Default for Aa2x {
    fn default() -> Self {
        // Cutoff 0.225 cycles/sample in the 2× domain = 0.45·base-rate (just below base Nyquist).
        // 4th-order Butterworth pole Qs: 0.5412 and 1.3066.
        let fc = 0.225;
        Self { c: [butter_lp(fc, 0.541_196_1), butter_lp(fc, 1.306_563)], s: [[0.0; 2]; 2] }
    }
}

impl Aa2x {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    fn step(&mut self, x: f32) -> f32 {
        let mut y = x;
        for k in 0..2 {
            let c = self.c[k];
            let s = &mut self.s[k];
            let out = c.b0 * y + s[0];
            s[0] = c.b1 * y - c.a1 * out + s[1];
            s[1] = c.b2 * y - c.a2 * out;
            y = out;
        }
        y
    }

    /// Feed the two 2×-rate sub-samples; return the decimated base-rate output.
    #[inline]
    pub fn decimate(&mut self, x0: f32, x1: f32) -> f32 {
        self.step(x0);
        self.step(x1)
    }
}

#[derive(Clone, Debug)]
pub struct DualConfig {
    pub wave_a: Wave,
    pub wave_b: Wave,
    /// Authored base values (the engine overrides these per-sample when matrix-modulated).
    pub osc_a_level: f32,
    pub osc_b_level: f32,
    /// Osc B frequency = note × `ratio_b` (native; integer ⇒ harmonic, fractional ⇒ inharmonic).
    pub ratio_b: f32,
    /// 0..1 → 0..[`DETUNE_CENTS_MAX`] cents (osc B only).
    pub detune_b: f32,
    /// 0..1 → 0..[`FM_INDEX_MAX`] rad of osc-B→osc-A phase modulation. 0 ⇒ pure dual-osc.
    pub fm_index: f32,
    /// 0..1 operator self-feedback on osc B.
    pub fm_feedback: f32,
}

impl DualConfig {
    pub fn from_doc(inst: &Instrument) -> DualConfig {
        DualConfig {
            wave_a: Wave::from_name(inst.string("osc_a_wave").unwrap_or("sine")),
            wave_b: Wave::from_name(inst.string("osc_b_wave").unwrap_or("sine")),
            osc_a_level: inst.norm_or_default("osc_a_level_db"),
            osc_b_level: inst.norm_or_default("osc_b_level_db"),
            ratio_b: inst.native_or_default("osc_b_ratio"),
            detune_b: inst.norm_or_default("osc_b_detune_cents"),
            fm_index: inst.norm_or_default("fm_index"),
            fm_feedback: inst.norm_or_default("fm_feedback"),
        }
    }
}

/// Per-sample effective dual params (after matrix modulation).
#[derive(Clone, Copy, Debug)]
pub struct DualParams {
    pub wave_a: Wave,
    pub wave_b: Wave,
    pub osc_a_level: f32,
    pub osc_b_level: f32,
    pub ratio_b: f32,
    pub detune_b: f32,
    pub fm_index: f32,
    pub fm_feedback: f32,
}

#[inline]
fn cents_ratio(cents: f32) -> f32 {
    2.0_f32.powf(cents / 1200.0)
}

/// One dual-osc sample. `phase_a` is the voice's carrier phase (`phase_rad`); `st` carries the
/// modulator phase + feedback. Osc B goes to the mix at `osc_b_level_db` whether or not it modulates A.
/// Osc + PM + `drive_db` (soft-clip) run at [`OVERSAMPLE`]× and are decimated through `aa` to tame the
/// PM/drive sidebands that would otherwise alias (ADR 0009). `drive_db` is applied here, so the engine
/// must NOT re-apply the shared soft-clip to a dual voice.
pub fn dual_sample(
    p: &DualParams,
    st: &mut SynthVoiceState,
    phase_a: &mut f32,
    aa: &mut Aa2x,
    drive: f32,
    hz: f32,
    sr: f32,
) -> f32 {
    let (phase_b, fb1, fb2) = match st {
        SynthVoiceState::Dual { phase_b, fb1, fb2 } => (phase_b, fb1, fb2),
        _ => {
            *st = SynthVoiceState::Dual { phase_b: 0.0, fb1: 0.0, fb2: 0.0 };
            return dual_sample(p, st, phase_a, aa, drive, hz, sr);
        }
    };

    let sr_os = sr * OVERSAMPLE as f32;
    let hz_b = hz * p.ratio_b * cents_ratio(p.detune_b * DETUNE_CENTS_MAX);
    let idx = p.fm_index * FM_INDEX_MAX;
    let mut sub = [0.0f32; OVERSAMPLE];
    for s in sub.iter_mut() {
        // Modulator (osc B): own frequency, DX-style averaged self-feedback.
        *phase_b = (*phase_b + TWO_PI * hz_b / sr_os).rem_euclid(TWO_PI);
        let fb = p.fm_feedback * 0.5 * (*fb1 + *fb2);
        let m = osc(p.wave_b, (*phase_b + fb).rem_euclid(TWO_PI));
        *fb2 = *fb1;
        *fb1 = m;

        // Carrier (osc A): phase-modulated by osc B.
        *phase_a = (*phase_a + TWO_PI * hz / sr_os).rem_euclid(TWO_PI);
        let a = osc(p.wave_a, (*phase_a + idx * m).rem_euclid(TWO_PI));

        // Osc B also reaches the mix at its own level (whether or not it modulates A); drive runs
        // inside the oversampled block so its harmonics are band-limited before decimation.
        *s = soft_clip(p.osc_a_level * a + p.osc_b_level * m, drive);
    }
    aa.decimate(sub[0], sub[1])
}

// ── Modal synth ─────────────────────────────────────────────────────────────

/// One resonant mode of a struck object.
#[derive(Clone, Copy, Debug)]
pub struct Partial {
    /// Frequency ratio to the fundamental.
    pub ratio: f32,
    /// Relative amplitude at strike.
    pub amp: f32,
    /// Decay length relative to the fundamental's T60 (≤1 ⇒ rings down faster).
    pub decay_rel: f32,
}

const fn p(ratio: f32, amp: f32, decay_rel: f32) -> Partial {
    Partial { ratio, amp, decay_rel }
}

// Near-pure fundamental + a weak, fast-decaying high "clang" partial.
static TUNING_FORK: [Partial; 3] = [p(1.0, 1.0, 1.0), p(6.27, 0.06, 0.12), p(17.55, 0.012, 0.05)];
// Long, shimmering metallic decay; strong fundamental, stretched upper partials.
static VIBRAPHONE: [Partial; 5] = [
    p(1.0, 1.0, 1.0), p(3.97, 0.5, 0.85), p(9.5, 0.28, 0.55), p(16.0, 0.12, 0.35), p(24.6, 0.06, 0.22),
];
// Generic harmonic resonator bank — defaults harmonic so `stretch` sculpts it; good for drones /
// tuned bodies. Eight partials so `num_modes` can select up to 8.
static RESONATORS: [Partial; 8] = [
    p(1.0, 1.0, 1.0), p(2.0, 0.6, 0.9), p(3.0, 0.42, 0.82), p(4.0, 0.3, 0.74),
    p(5.0, 0.22, 0.66), p(6.0, 0.16, 0.58), p(7.0, 0.12, 0.5), p(8.0, 0.09, 0.44),
];
// Marimba (default): tuned undercut bar ~1:4:10, woody/short decay.
static MARIMBA: [Partial; 3] = [p(1.0, 1.0, 0.9), p(3.93, 0.4, 0.45), p(9.2, 0.14, 0.22)];

/// Up to 8 partials per material; the config truncates to `num_modes`. Unknown strings fall
/// back to marimba — unreachable from authored docs; `categorical` parity-tests the option set.
pub(crate) fn material_partials(material: &str) -> &'static [Partial] {
    match material {
        "tuning_fork" => &TUNING_FORK,
        "vibraphone" => &VIBRAPHONE,
        "resonators" => &RESONATORS,
        _ => &MARIMBA,
    }
}

/// The modal ring-time bounds (s): `t60_s` spans them (and they clamp the ring in `t60_mode: "note"`).
pub(crate) const T60_MIN: f32 = 0.05;
pub(crate) const T60_MAX: f32 = 8.0;
const STRETCH_K: f32 = 0.015;
const RELEASE_DAMP: f32 = 0.18;
/// Target strike peak (linear FS) at `level_db` 0 (unity), per-material-normalized so every preset lands
/// in the same loudness ballpark as the sustained synths (`dual`/`subtractive` peak ~ the level's amplitude).
/// Replaces the old flat `MODAL_OUT_GAIN` (0.5), which left modal ~3× quieter than the others.
const MODAL_TARGET_PEAK: f32 = 0.9;

/// Fundamental T60 (seconds) from the normalized ring lever (`t60_s`'s log map).
#[inline]
pub fn t60_fundamental(t60_scale: f32) -> f32 {
    crate::params::T60.eval(t60_scale.clamp(0.0, 1.0))
}

/// Note-coupled ring ratio: how long the ring lasts relative to the note's own duration
/// (`t60_mode: "note"`, the doc's `ring_ratio`). Its normalized lever trims around unity — 0.5 ⇒
/// T60 ≈ the note (a clean strike that fills the step), <0.5 ⇒ staccato, >0.5 ⇒ blooms past the
/// note. Log-symmetric in 4× either way.
#[inline]
pub fn ring_ratio(t60_scale: f32) -> f32 {
    crate::params::RING_RATIO.eval(t60_scale.clamp(0.0, 1.0))
}

/// Analytic strike-peak estimate for a config — the summed per-mode strike amplitudes
/// `Σ amp·|sin(π(k+1)·strike_pos)|·bright^k`. Used only to compute a per-material normalization so
/// louder/denser presets don't overshoot; the true summed peak differs by mode-phase alignment but
/// tracks this within ~15%, which is enough for loudness matching.
fn strike_peak_estimate(partials: &[Partial], strike_pos: f32, brightness: f32) -> f32 {
    let bright_factor = 0.25 + 0.75 * brightness.clamp(0.0, 1.0);
    let sp = strike_pos.clamp(0.0, 1.0);
    let est: f32 = partials
        .iter()
        .enumerate()
        .map(|(k, pp)| pp.amp * (PI * (k as f32 + 1.0) * sp).sin().abs() * bright_factor.powi(k as i32))
        .sum();
    est.max(1e-3)
}

#[derive(Clone, Debug)]
pub struct ModalConfig {
    pub partials: Vec<Partial>,
    /// Authored base values, normalized (matrix can override per-sample). `t60_scale` is the ring
    /// lever: `t60_s` through its curve, or `ring_ratio` under `t60_mode: "note"`.
    pub t60_scale: f32,
    pub brightness: f32,
    pub strike_pos: f32,
    pub stretch: f32,
    /// Per-material output normalization: `MODAL_TARGET_PEAK / strike_peak_estimate(...)`, precomputed
    /// from the authored strike_pos/brightness so every preset strikes at ~the same loudness.
    pub strike_norm: f32,
    /// `t60_mode: "note"` — couple the ring length to the driving note's duration (its source's
    /// step, the doc's `ring_ratio`) instead of the absolute `t60_s`. Default false (intrinsic
    /// absolute decay).
    pub t60_from_note: bool,
}

impl ModalConfig {
    pub fn from_doc(inst: &Instrument) -> ModalConfig {
        let material = inst.string("material").unwrap_or("marimba");
        let all = material_partials(material);
        let n = (inst.native_or_default("num_modes").round() as usize).clamp(1, all.len());
        let partials = all[..n].to_vec();
        let brightness = inst.norm_or_default("brightness");
        let strike_pos = inst.norm_or_default("strike_pos");
        let strike_norm = MODAL_TARGET_PEAK / strike_peak_estimate(&partials, strike_pos, brightness);
        let t60_from_note = inst.string("t60_mode") == Some("note");
        ModalConfig {
            partials,
            t60_scale: inst.norm_or_default(if t60_from_note { "ring_ratio" } else { "t60_s" }),
            brightness,
            strike_pos,
            stretch: inst.norm_or_default("stretch"),
            strike_norm,
            t60_from_note,
        }
    }

    /// The doc param the ring lever is: `ring_ratio` under `t60_mode: "note"`, else `t60_s`.
    pub fn t60_leaf(&self) -> &'static str {
        if self.t60_from_note {
            "ring_ratio"
        } else {
            "t60_s"
        }
    }

    /// Worst-case post-release ring length (seconds) at the absolute `t60_scale` — the engine keeps a
    /// released modal voice this long before pruning so its intrinsic tail isn't truncated. In
    /// `t60_from_note` mode the ring is per-voice (depends on the driving step), so the engine prunes
    /// those voices with a per-voice bound instead (see `engine.rs`); this stays the intrinsic floor.
    pub fn release_tail_s(&self) -> f32 {
        let t60 = t60_fundamental(self.t60_scale);
        let max_rel = self.partials.iter().map(|p| p.decay_rel).fold(0.0f32, f32::max);
        (t60 * max_rel * RELEASE_DAMP).max(0.05)
    }

    /// Post-release prune tail (seconds) for a voice ringing `note_dur_s` (its source's step) in
    /// `t60_from_note` mode. Mirrors `release_tail_s` but with the note-coupled T60.
    pub fn release_tail_for_note(&self, note_dur_s: f32) -> f32 {
        let t60 = note_dur_s * ring_ratio(self.t60_scale);
        let max_rel = self.partials.iter().map(|p| p.decay_rel).fold(0.0f32, f32::max);
        (t60 * max_rel * RELEASE_DAMP).max(0.05)
    }
}

/// Per-sample effective modal params (after matrix modulation).
#[derive(Clone, Copy, Debug)]
pub struct ModalParams {
    pub t60_scale: f32,
    pub brightness: f32,
    pub strike_pos: f32,
    pub stretch: f32,
    /// Per-material loudness normalization (from [`ModalConfig::strike_norm`]).
    pub strike_norm: f32,
}

/// One modal sample. `t_note` is seconds since articulation (note-relative; the same clock the ADSR
/// uses), so the strike impulse fires reproducibly across the loop seam. `released` (Some) shortens
/// the ring (note-off damping). `gate_s` (Some) couples the ring length to the note's own duration
/// (`t60_mode: "note"`) — the fundamental T60 becomes `gate_s · ring_ratio(t60_scale)` instead of the
/// absolute `t60_scale` log map; `None` keeps the intrinsic absolute decay.
#[allow(clippy::too_many_arguments)]
pub fn modal_sample(
    partials: &[Partial],
    mp: &ModalParams,
    st: &mut SynthVoiceState,
    t_note: f32,
    released: Option<f32>,
    gate_s: Option<f32>,
    hz: f32,
    sr: f32,
) -> f32 {
    let (y1, y2) = match st {
        SynthVoiceState::Modal { y1, y2 } => (y1, y2),
        _ => {
            let n = partials.len();
            *st = SynthVoiceState::Modal { y1: vec![0.0; n], y2: vec![0.0; n] };
            return modal_sample(partials, mp, st, t_note, released, gate_s, hz, sr);
        }
    };
    if y1.len() != partials.len() {
        y1.resize(partials.len(), 0.0);
        y2.resize(partials.len(), 0.0);
    }

    // Unit impulse on the articulation sample (broadband strike) — and only there. The engine's
    // note clock reads exactly 0.0 on that sample (`-(f/sr) + f/sr`), but only ≈ 1/sr on the next,
    // where f32 rounding lands under 1/sr about a third of the time: a `t < 1/sr` window struck
    // those notes twice, a sample apart (+6 dB).
    let x = if t_note == 0.0 { 1.0 } else { 0.0 };
    // Fundamental T60: note-coupled (fills the note's duration) or the absolute `t60_scale` map.
    let t60_base = match gate_s {
        Some(g) => (g * ring_ratio(mp.t60_scale)).clamp(T60_MIN, T60_MAX),
        None => t60_fundamental(mp.t60_scale),
    };
    let t60_fund = t60_base * if released.is_some() { RELEASE_DAMP } else { 1.0 };
    let bright_factor = 0.25 + 0.75 * mp.brightness.clamp(0.0, 1.0);

    let mut out = 0.0f32;
    for (k, pp) in partials.iter().enumerate() {
        // inharmonic push: higher modes go progressively sharper with `stretch`.
        let f_k = hz * pp.ratio * (1.0 + mp.stretch * STRETCH_K * k as f32);
        let w = (TWO_PI * f_k / sr).clamp(0.001, PI * 0.99);
        let t60 = (t60_fund * pp.decay_rel).max(1e-4);
        let r = (-6.9078 / (t60 * sr)).exp().min(0.9999);
        // strike-position weighting (bar mode shape) × brightness tilt; sin(w) normalizes the
        // resonator so the struck amplitude ≈ amp regardless of decay/frequency.
        let strike_w = (PI * (k as f32 + 1.0) * mp.strike_pos.clamp(0.0, 1.0)).sin().abs();
        let b0 = pp.amp * strike_w * bright_factor.powi(k as i32) * w.sin();

        let yk = b0 * x + 2.0 * r * w.cos() * y1[k] - r * r * y2[k];
        out += yk;
        y2[k] = y1[k];
        y1[k] = yk;
    }
    out * mp.strike_norm
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binaural_pair_splits_the_spread_and_floors_at_zero() {
        assert_eq!(binaural_pair(110.0, 2.0), (109.0, 111.0)); // the ADR 0047 example
        assert_eq!(binaural_pair(109.5, 0.0), (109.5, 109.5));
        assert_eq!(binaural_pair(20.0, 40.0), (0.0, 40.0)); // never a negative frequency
        assert_eq!(binaural_root(109.5, 0.0), 109.5, "no detune ⇒ the authored Hz, exactly");
        assert!((binaural_root(110.0, 1200.0) - 220.0).abs() < 1e-3);
    }

    /// Count upward zero crossings of `n` samples of one side ⇒ its frequency.
    fn crossings(samples: &[f32]) -> usize {
        samples.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count()
    }

    #[test]
    fn binaural_sample_puts_each_side_on_its_own_frequency() {
        let sr = 48_000.0;
        let mut st = SynthVoiceState::None; // re-initialized to Binaural on the first sample
        let (mut pl, mut psl) = (0.0f32, 0.0f32);
        let (mut l, mut r) = (Vec::new(), Vec::new());
        for _ in 0..(sr as usize * 2) {
            let (a, b) = binaural_sample(Wave::Sine, &mut st, &mut pl, &mut psl, 110.0, None, 2.0, 0.0, sr);
            l.push(a);
            r.push(b);
        }
        assert!(matches!(st, SynthVoiceState::Binaural { .. }));
        // 2 s ⇒ 218 crossings at 109 Hz, 222 at 111 Hz (±1 for the window edge).
        assert!((crossings(&l) as i64 - 218).abs() <= 1, "left {}", crossings(&l));
        assert!((crossings(&r) as i64 - 222).abs() <= 1, "right {}", crossings(&r));
    }

    #[test]
    fn binaural_pedal_gets_the_same_spread() {
        // shift_mix 1 ⇒ only the pedal sounds: an octave up (220 Hz) ± the same 1 Hz ⇒ 219 / 221.
        let sr = 48_000.0;
        let mut st = SynthVoiceState::init_for(&SynthKind::Binaural(BinauralConfig { root_hz: None, spread_hz: 2.0 }));
        let (mut pl, mut psl) = (0.0f32, 0.0f32);
        let (mut l, mut r) = (Vec::new(), Vec::new());
        for _ in 0..(sr as usize * 2) {
            let (a, b) = binaural_sample(Wave::Sine, &mut st, &mut pl, &mut psl, 110.0, Some(220.0), 2.0, 1.0, sr);
            l.push(a);
            r.push(b);
        }
        assert!((crossings(&l) as i64 - 438).abs() <= 1, "left {}", crossings(&l));
        assert!((crossings(&r) as i64 - 442).abs() <= 1, "right {}", crossings(&r));
    }

    // The 2× decimator passes the base band but rejects the alias band (content between base
    // Nyquist and the 2× Nyquist), which is what makes oversampling worthwhile (ADR 0009).
    #[test]
    fn aa2x_rejects_the_alias_band() {
        let sr2 = 96_000.0; // the 2× (oversampled) rate; base rate 48 kHz, base Nyquist 24 kHz
        let energy = |f: f32| {
            let mut aa = Aa2x::new();
            let (mut acc, mut n) = (0.0f32, 0usize);
            for k in 0..4000usize {
                let x0 = (TWO_PI * f * (2 * k) as f32 / sr2).sin();
                let x1 = (TWO_PI * f * (2 * k + 1) as f32 / sr2).sin();
                let y = aa.decimate(x0, x1);
                if k > 200 {
                    acc += y * y;
                    n += 1;
                }
            }
            (acc / n as f32).sqrt()
        };
        let pass = energy(2_000.0); // well inside the base band → passes
        let stop = energy(40_000.0); // above base Nyquist → alias band → must be rejected
        assert!(stop < pass * 0.2, "decimator must reject the alias band: pass={pass} stop={stop}");
    }

    #[test]
    fn dual_index_zero_is_a_clean_tone() {
        // fm_index=0, osc_b_level=0 ⇒ a clean low sine: bounded, finite, very little HF content.
        let p = DualParams {
            wave_a: Wave::Sine, wave_b: Wave::Sine, osc_a_level: 1.0, osc_b_level: 0.0,
            ratio_b: 2.0, detune_b: 0.0, fm_index: 0.0, fm_feedback: 0.0,
        };
        let mut st = SynthVoiceState::Dual { phase_b: 0.0, fb1: 0.0, fb2: 0.0 };
        let mut aa = Aa2x::new();
        let mut phase_a = 0.0f32;
        let (mut num, mut den, mut prev) = (0.0f32, 0.0f32, 0.0f32);
        for i in 0..2048 {
            let s = dual_sample(&p, &mut st, &mut phase_a, &mut aa, 0.0, 220.0, 48_000.0);
            assert!(s.is_finite() && s.abs() < 1.2, "bounded: {s}");
            if i > 64 {
                num += (s - prev).abs();
                den += s.abs();
            }
            prev = s;
        }
        assert!(num / den.max(1e-9) < 0.2, "fm_index 0 ⇒ clean tone (low HF)");
    }

    #[test]
    fn dual_fm_index_adds_sidebands() {
        // higher fm_index ⇒ more high-frequency content (rougher waveform).
        let base = DualParams {
            wave_a: Wave::Sine, wave_b: Wave::Sine, osc_a_level: 1.0, osc_b_level: 0.0,
            ratio_b: 2.0, detune_b: 0.0, fm_index: 0.0, fm_feedback: 0.0,
        };
        let hi = DualParams { fm_index: 0.8, ..base };
        let rough = |p: DualParams| {
            let mut st = SynthVoiceState::Dual { phase_b: 0.0, fb1: 0.0, fb2: 0.0 };
            let mut aa = Aa2x::new();
            let mut ph = 0.0f32;
            let mut prev = 0.0f32;
            let (mut num, mut den) = (0.0f32, 0.0f32);
            for i in 0..2048 {
                let s = dual_sample(&p, &mut st, &mut ph, &mut aa, 0.0, 220.0, 48_000.0);
                if i > 0 {
                    num += (s - prev).abs();
                    den += s.abs();
                }
                prev = s;
            }
            num / den.max(1e-9)
        };
        assert!(rough(hi) > rough(base) * 1.3, "fm_index should add HF content");
    }

    #[test]
    fn modal_strike_rings_and_decays() {
        let cfg = ModalConfig {
            partials: material_partials("marimba").to_vec(),
            t60_scale: 0.5, brightness: 0.7, strike_pos: 0.3, stretch: 0.0,
            strike_norm: 1.0, t60_from_note: false,
        };
        let mp = ModalParams { t60_scale: 0.5, brightness: 0.7, strike_pos: 0.3, stretch: 0.0, strike_norm: 1.0 };
        let mut st = SynthVoiceState::init_for(&SynthKind::Modal(cfg.clone()));
        let sr = 48_000.0;
        let rms = |st: &mut SynthVoiceState, start: usize, n: usize| {
            let mut acc = 0.0f32;
            for i in 0..(start + n) {
                let t = i as f32 / sr;
                let s = modal_sample(&cfg.partials, &mp, st, t, None, None, 220.0, sr);
                if i >= start {
                    acc += s * s;
                }
            }
            (acc / n as f32).sqrt()
        };
        // ring early vs late: energy must decay.
        let mut st2 = st.clone();
        let early = rms(&mut st, 1000, 4000);
        let late = rms(&mut st2, 20_000, 4000);
        assert!(early > 0.0 && early.is_finite(), "rings after strike: {early}");
        assert!(late < early, "decays over time: early {early} late {late}");
    }

    #[test]
    fn modal_longer_t60_decays_slower() {
        let partials = material_partials("vibraphone").to_vec();
        let decay_ratio = |scale: f32| {
            let mp = ModalParams { t60_scale: scale, brightness: 0.7, strike_pos: 0.3, stretch: 0.0, strike_norm: 1.0 };
            let mut st = SynthVoiceState::Modal { y1: vec![0.0; partials.len()], y2: vec![0.0; partials.len()] };
            let sr = 48_000.0;
            let energy = |st: &mut SynthVoiceState, start: usize, n: usize| {
                let mut acc = 0.0f32;
                let mut fed = false;
                for i in 0..(start + n) {
                    let t = i as f32 / sr;
                    let s = modal_sample(&partials, &mp, st, t, None, None, 220.0, sr);
                    let _ = fed; fed = true;
                    if i >= start { acc += s * s; }
                }
                acc
            };
            let mut s2 = st.clone();
            let e0 = energy(&mut st, 2000, 2000);
            let e1 = energy(&mut s2, 30_000, 2000);
            e1 / e0.max(1e-12)
        };
        // a longer ring retains more of its energy later in the tail.
        assert!(decay_ratio(0.9) > decay_ratio(0.3), "longer t60 ⇒ slower decay");
    }

    #[test]
    fn modal_release_damps_faster() {
        let partials = material_partials("vibraphone").to_vec();
        let mp = ModalParams { t60_scale: 0.9, brightness: 0.7, strike_pos: 0.3, stretch: 0.0, strike_norm: 1.0 };
        let sr = 48_000.0;
        let tail = |released: bool| {
            let mut st = SynthVoiceState::Modal { y1: vec![0.0; partials.len()], y2: vec![0.0; partials.len()] };
            let rel = if released { Some(0.05f32) } else { None };
            let mut acc = 0.0f32;
            for i in 0..12_000 {
                let t = i as f32 / sr;
                let s = modal_sample(&partials, &mp, &mut st, t, if t > 0.05 { rel } else { None }, None, 220.0, sr);
                if i >= 8000 { acc += s * s; }
            }
            acc
        };
        assert!(tail(true) < tail(false), "released ring decays faster");
    }

    #[test]
    fn modal_loudness_is_normalized_across_materials() {
        // Every preset strikes at ~the same peak (MODAL_TARGET_PEAK ballpark) so no material is the
        // ~3× quieter outlier it was before per-material normalization (the reported "barely audible").
        let sr = 48_000.0;
        let peak = |material: &str, nm: usize, sp: f32, br: f32| -> f32 {
            let partials = material_partials(material)[..nm].to_vec();
            let strike_norm = MODAL_TARGET_PEAK / strike_peak_estimate(&partials, sp, br);
            let mp = ModalParams { t60_scale: 0.5, brightness: br, strike_pos: sp, stretch: 0.0, strike_norm };
            let mut st = SynthVoiceState::Modal { y1: vec![0.0; nm], y2: vec![0.0; nm] };
            let mut pk = 0.0f32;
            for i in 0..(sr as usize / 10) {
                pk = pk.max(modal_sample(&partials, &mp, &mut st, i as f32 / sr, None, None, 220.0, sr).abs());
            }
            pk
        };
        let peaks = [
            peak("tuning_fork", 3, 0.24, 0.86),
            peak("marimba", 3, 0.3, 0.7),
            peak("vibraphone", 5, 0.3, 0.7),
            peak("resonators", 8, 0.3, 0.6),
        ];
        for p in peaks {
            assert!((0.35..=1.1).contains(&p), "normalized strike peak in band: {p} of {peaks:?}");
        }
        let mn = peaks.iter().cloned().fold(f32::MAX, f32::min);
        let mx = peaks.iter().cloned().fold(0.0f32, f32::max);
        assert!(mx / mn < 2.0, "materials within 2× of each other: {mn}..{mx}");
    }

    #[test]
    fn modal_note_mode_longer_gate_rings_longer() {
        // `t60_mode: "note"`: a longer note (bigger gate) leaves more late-window ring energy —
        // proving the ring couples to note duration instead of a fixed intrinsic T60.
        let sr = 48_000.0;
        let partials = material_partials("tuning_fork")[..3].to_vec();
        let late_energy = |gate: f32| -> f32 {
            let mp = ModalParams { t60_scale: 0.5, brightness: 0.86, strike_pos: 0.24, stretch: 0.0, strike_norm: 1.0 };
            let mut st = SynthVoiceState::Modal { y1: vec![0.0; 3], y2: vec![0.0; 3] };
            let mut acc = 0.0f32;
            for i in 0..(sr as usize / 2) {
                let t = i as f32 / sr;
                let s = modal_sample(&partials, &mp, &mut st, t, None, Some(gate), 220.0, sr);
                if t > 0.4 {
                    acc += s * s;
                }
            }
            acc
        };
        assert!(late_energy(1.0) > late_energy(0.3) * 2.0, "longer gate rings longer past 0.4 s");
    }

    #[test]
    fn modal_strike_is_one_impulse_wherever_it_is_articulated() {
        // The engine's note clock: a voice articulated at loop frame `f` stores `-(f/sr)` and reads
        // `t = elapsed + frame/sr` — exactly 0.0 on the articulation sample, but only ≈ 1/sr on the
        // next, where f32 rounding can land a hair under 1/sr. Wherever the strike lands it must be
        // ONE impulse: the engines-listen benchmark (2026-10-24) heard ~30 % of strikes as two impulses
        // a sample apart (+6 dB, identical partials) at loop positions that repeated every loop.
        let partials = material_partials("resonators")[..5].to_vec();
        let mp = ModalParams { t60_scale: 0.5, brightness: 0.45, strike_pos: 0.22, stretch: 0.0, strike_norm: 1.0 };
        let sr = 48_000.0f32;
        let strike_at = |f: usize| -> Vec<f32> {
            let mut st = SynthVoiceState::Modal { y1: vec![0.0; 5], y2: vec![0.0; 5] };
            let elapsed = -(f as f32 / sr);
            (f..f + 64).map(|frame| modal_sample(&partials, &mp, &mut st, elapsed + frame as f32 / sr, None, None, 110.0, sr)).collect()
        };
        let reference = strike_at(0);
        let frames: Vec<usize> = (0..2_000).map(|i| 1 + i * 347).collect(); // across a ~14 s loop
        let doubled: Vec<usize> = frames.iter().copied().filter(|&f| strike_at(f) != reference).collect();
        assert!(doubled.is_empty(), "{} of {} strikes differ from the frame-0 strike (first at {:?})", doubled.len(), frames.len(), doubled.first());
    }
}
