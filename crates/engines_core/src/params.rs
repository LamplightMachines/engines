//! **The param registry** — single source of truth for per-param metadata that was
//! previously hand-synced across four files (the "add-a-param scavenger hunt"):
//!
//! - the **curve** between the engine's normalized `0..1` and the param's physical value (Hz, s,
//!   cents, ×…: ADR 0005, 0054) → [`Curve`]. The engine's unit maps (`engine::cutoff_to_hz`,
//!   `synth::t60_fundamental`, …), the display units and the capability catalog all read these.
//! - what a doc writes (schema v0.9, ADR 0054: the native value under a unit-suffixed key —
//!   `cutoff_hz`, `level_db` — except the unitless fractions) and so its range (the ADR 0005
//!   param-range seam) → [`ParamSpec::doc_range`], [`crate::loop_doc::param_range`]
//! - Tier-2 safe bounds (ADR 0011), native → [`crate::loop_doc::safe_range_default`]
//!   (mirrored in `contracts/loop_contract_v0.9.schema.json` `x-safe-bounds-defaults`,
//!   parity-tested from loop_doc)
//! - the drift scale (ADR 0054 P2) and a route depth's unit and span (P4) → [`Curve::drift_scale`],
//!   [`DepthUnit`]
//! - categorical option sets (categorical moves) → [`crate::categorical`]
//!
//! **Adding a param means: one row here**, and the synth code that reads it. An unlisted param
//! defaults to `(0,1)` Continuous, and the parity tests pin every cross-file relationship.
//! Display units (Hz, s, ms, st, ×) are a registry column; [`units`] formats with them.

pub mod units;

use crate::engine::{ATTACK_MAX_S, DECAY_MAX_S, RELEASE_MAX_S, VOICES_DETUNE_CENTS_MAX};
use crate::synth::{DETUNE_CENTS_MAX, FM_INDEX_MAX, T60_MAX, T60_MIN};
use units::Unit;

/// Octave-pedal `shift_st` in semitones (ADR 0005 — the param-range seam's founding case).
pub const SHIFT_RANGE: (f32, f32) = (-24.0, 24.0);
/// Reverb pre-delay in milliseconds (the only non-0..1 bus param).
pub const PREDELAY_RANGE: (f32, f32) = (0.0, 100.0);
/// Dual-osc: osc-B frequency ratio to the note (ADR 0009).
pub const OSC_B_RATIO_RANGE: (f32, f32) = (0.5, 12.0);
/// Binaural: the pair's root frequency in Hz (ADR 0047) — authored and read as real Hz.
pub const ROOT_HZ_RANGE: (f32, f32) = (20.0, 1000.0);
/// Binaural: the L/R spread in Hz — the beat rate (ADR 0047). L = root − s/2, R = root + s/2.
pub const SPREAD_HZ_RANGE: (f32, f32) = (0.0, 40.0);

/// Pitched-oscillator waveshapes (must map 1:1 onto [`crate::dsp::Wave`]'s pitched variants —
/// parity-tested in `categorical.rs`; `filtered_noise` is the noise-`source` domain, not a
/// pitched `wave`). Shared by the subtractive `wave` and the dual-osc `osc_a_wave`/`osc_b_wave`.
pub const WAVE_OPTIONS: [&str; 6] = ["sine", "triangle", "saw_soft", "square", "pulse", "sine_bell"];
/// Modal-synth material presets (must match `synth::material_partials` — parity-tested).
pub const MATERIAL_OPTIONS: [&str; 4] = ["marimba", "vibraphone", "tuning_fork", "resonators"];

/// How a param's engine-normalized `0..1` maps to its native value (what a v0.9 doc writes). The
/// frequency and time forms are written exactly as the engine's maps always computed them, so
/// their normalized space — where drift steps and routes add — didn't move with schema v0.9.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Curve {
    /// `lo + n·(hi − lo)`.
    Linear { lo: f32, hi: f32 },
    /// `lo · base^(n − shift)`: log frequency and log time.
    Exp { lo: f32, base: f32, shift: f32 },
    /// A level in dB, linear over `lo..hi` (ADR 0054 P2): a gain over −60..+6 dB, `drive_db` over
    /// 0..+20. `-Infinity` (silence) maps to `-inf` both ways and stays there under any sum.
    Db { lo: f32, hi: f32 },
}

impl Curve {
    /// Normalized → native. Not clamped: callers clamp where the engine always did.
    #[inline]
    pub fn eval(self, n: f32) -> f32 {
        match self {
            Curve::Linear { lo, hi } | Curve::Db { lo, hi } => lo + n * (hi - lo),
            Curve::Exp { lo, base, shift } => lo * base.powf(n - shift),
        }
    }

    /// Native → normalized, clamped to `0..1` (a degenerate linear range passes the value through);
    /// a dB curve keeps `-inf` (silence) rather than clamping it up to the floor.
    #[inline]
    pub fn inverse(self, v: f32) -> f32 {
        match self {
            Curve::Linear { lo, hi } if (hi - lo).abs() < 1e-9 => v,
            Curve::Linear { lo, hi } => ((v - lo) / (hi - lo)).clamp(0.0, 1.0),
            Curve::Db { .. } if v == f32::NEG_INFINITY => v,
            Curve::Db { lo, hi } => ((v - lo) / (hi - lo)).clamp(0.0, 1.0),
            Curve::Exp { lo, base, shift } => ((v.max(1e-6) / lo).ln() / base.ln() + shift).clamp(0.0, 1.0),
        }
    }

    /// The native values at normalized 0 and 1.
    pub fn ends(self) -> (f32, f32) {
        (self.eval(0.0), self.eval(1.0))
    }

    /// What the controller's caps (`max_change`, `max_drift`) are multiplied by on this curve (ADR
    /// 0054 P2): 0.3 for dB, so a gain drifts about 1 dB a loop at `max_change` 0.05, as a v0.8
    /// amplitude did near `level 0.5`; 1 elsewhere.
    pub fn drift_scale(self) -> f32 {
        match self {
            Curve::Db { .. } => 0.3,
            _ => 1.0,
        }
    }

    /// A route depth's normalized distance per native unit of depth (ADR 0054 P4): octaves across a
    /// log curve, the native span across a linear or dB one.
    pub fn depth_span(self) -> f32 {
        match self {
            Curve::Linear { lo, hi } | Curve::Db { lo, hi } => hi - lo,
            Curve::Exp { base, .. } => base.log2(),
        }
    }
}

/// dB → the amplitude multiplier the DSP applies; `-Infinity` (silence) → 0.
#[inline]
pub fn db_to_amp(db: f32) -> f32 {
    if db == f32::NEG_INFINITY {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

/// A gain's normalized value (through [`GAIN`]) → the amplitude the DSP multiplies by. Every gain
/// the engine applies — levels, sends, returns, the master — goes through here.
#[inline]
pub fn gain_amp(n: f32) -> f32 {
    db_to_amp(GAIN.eval(n))
}

/// The amplitude → dB (`0` → `-Infinity`).
pub fn amp_to_db(amp: f32) -> f32 {
    if amp <= 0.0 {
        f32::NEG_INFINITY
    } else {
        20.0 * amp.log10()
    }
}

/// Low-pass cutoff: 80 Hz × 100ⁿ (80 Hz–8 kHz; the engine caps it at Nyquist).
pub const CUTOFF: Curve = Curve::Exp { lo: 80.0, base: 100.0, shift: 0.0 };
/// A `filtered_noise` band edge: 20 Hz × 200ⁿ (20 Hz–4 kHz; capped at Nyquist). Re-anchored from
/// v0.9's 80 Hz × 50ⁿ (ADR 0046 amendment) so a pitched noise note can sit down to B0 with
/// noise under it: an authored Hz keeps its sound, its normalized position (drift's space) moved.
pub const BAND: Curve = Curve::Exp { lo: 20.0, base: 200.0, shift: 0.0 };
/// ADSR attack in seconds.
pub const ATTACK: Curve = Curve::Linear { lo: 0.0, hi: ATTACK_MAX_S };
/// Decay in seconds: the ADSR's, and the legacy `event` bell's ring (both 0–4 s, ADR 0054 P7).
pub const DECAY: Curve = Curve::Linear { lo: 0.0, hi: DECAY_MAX_S };
/// ADSR release in seconds.
pub const RELEASE: Curve = Curve::Linear { lo: 0.0, hi: RELEASE_MAX_S };
/// `voices_detune_cents`: the outermost voice's offset in cents (the voices spread across ±that).
pub const VOICES_DETUNE: Curve = Curve::Linear { lo: 0.0, hi: VOICES_DETUNE_CENTS_MAX };
/// Dual osc-B detune in cents (upward).
pub const OSC_B_DETUNE: Curve = Curve::Linear { lo: 0.0, hi: DETUNE_CENTS_MAX };
/// Dual osc-B → osc-A phase-modulation depth in radians.
pub const FM_INDEX: Curve = Curve::Linear { lo: 0.0, hi: FM_INDEX_MAX };
/// Modal `t60_mode: "fixed"`: the fundamental's ring time in seconds, log.
pub const T60: Curve = Curve::Exp { lo: T60_MIN, base: T60_MAX / T60_MIN, shift: 0.0 };
/// Modal `t60_mode: "note"`: the ring as a ratio of the note's length, 4^(n − ½) (¼× to 4×).
pub const RING_RATIO: Curve = Curve::Exp { lo: 1.0, base: 4.0, shift: 0.5 };
/// Filter `resonance` as the SVF's Q (0.707 at 0, no peak). A `pitched` noise voice reads it as a
/// peak in dB instead (ADR 0046).
pub const RESONANCE_Q: Curve = Curve::Linear { lo: 0.707, hi: 6.0 };
/// `drive_db`: the soft clipper's pre-gain in dB, 0 to +20 (×1 to ×10; [`db_to_amp`]).
pub const DRIVE: Curve = Curve::Db { lo: 0.0, hi: 20.0 };
/// Every gain in dB: `level_db`, the dual osc levels, `master_gain_db`, `return_gain_db`, `sends_db`.
/// Normalized over −60..+6 dB; the DSP applies [`gain_amp`].
pub const GAIN: Curve = Curve::Db { lo: GAIN_FLOOR_DB, hi: GAIN_CEILING_DB };
/// A gain's finite floor (`0` normalized) and its hard ceiling (ADR 0054 P1).
pub const GAIN_FLOOR_DB: f32 = -60.0;
pub const GAIN_CEILING_DB: f32 = 6.0;
/// Octave-pedal `shift_st` in semitones.
pub const SHIFT: Curve = Curve::Linear { lo: SHIFT_RANGE.0, hi: SHIFT_RANGE.1 };
/// Reverb pre-delay in ms.
pub const PREDELAY: Curve = Curve::Linear { lo: PREDELAY_RANGE.0, hi: PREDELAY_RANGE.1 };
/// Dual osc-B frequency ratio.
pub const OSC_B_RATIO: Curve = Curve::Linear { lo: OSC_B_RATIO_RANGE.0, hi: OSC_B_RATIO_RANGE.1 };
/// Binaural root in Hz.
pub const ROOT_HZ: Curve = Curve::Linear { lo: ROOT_HZ_RANGE.0, hi: ROOT_HZ_RANGE.1 };
/// Binaural spread (the beat) in Hz.
pub const SPREAD_HZ: Curve = Curve::Linear { lo: SPREAD_HZ_RANGE.0, hi: SPREAD_HZ_RANGE.1 };

/// What a loop doc writes for a param (schema v0.9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Authored {
    /// A unitless `0..1` (`resonance`, `fm_index`, a mix): the engine's normalized value itself.
    Normalized,
    /// The native value its unit-suffixed key names (`cutoff_hz`, `attack_s`, `level_db`, …).
    Native,
}

/// A route depth's unit — the dest's, under its own key (ADR 0054 P4): `depth_oct` into a log
/// curve, `depth_db` into a gain, plain `depth` into a `0..1` dest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DepthUnit {
    Plain,
    Oct,
    St,
    Cents,
    S,
    Ms,
    Db,
    Hz,
    X,
}

impl DepthUnit {
    pub const ALL: [DepthUnit; 9] = [DepthUnit::Plain, DepthUnit::Oct, DepthUnit::St, DepthUnit::Cents, DepthUnit::S, DepthUnit::Ms, DepthUnit::Db, DepthUnit::Hz, DepthUnit::X];

    /// The route key it's written under.
    pub fn key(self) -> &'static str {
        match self {
            DepthUnit::Plain => "depth",
            DepthUnit::Oct => "depth_oct",
            DepthUnit::St => "depth_st",
            DepthUnit::Cents => "depth_cents",
            DepthUnit::S => "depth_s",
            DepthUnit::Ms => "depth_ms",
            DepthUnit::Db => "depth_db",
            DepthUnit::Hz => "depth_hz",
            DepthUnit::X => "depth_x",
        }
    }

    pub fn from_key(key: &str) -> Option<DepthUnit> {
        DepthUnit::ALL.into_iter().find(|u| u.key() == key)
    }

    /// The unit a route into `spec`'s param takes: octaves across a log curve, else the param's own.
    pub fn of(spec: &ParamSpec) -> DepthUnit {
        match (spec.authored, spec.curve) {
            (Authored::Native, Some(Curve::Exp { .. })) => DepthUnit::Oct,
            (Authored::Native, Some(_)) => match spec.unit {
                Unit::Semitones => DepthUnit::St,
                Unit::Cents => DepthUnit::Cents,
                Unit::Seconds => DepthUnit::S,
                Unit::Millis => DepthUnit::Ms,
                Unit::Db => DepthUnit::Db,
                Unit::Hz | Unit::HzNative => DepthUnit::Hz,
                Unit::Ratio => DepthUnit::X,
                Unit::Normalized => DepthUnit::Plain,
            },
            _ => DepthUnit::Plain,
        }
    }
}

/// How the controller may perturb a param.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ParamKind {
    /// Numeric drift across its normalized `0..1`.
    Continuous,
    /// A discrete flip over a fixed option set (a categorical move).
    Categorical(&'static [&'static str]),
    /// Small-integer lever dithered as ±1 neighbours within `[lo, hi]` (e.g. `num_modes`).
    IntSteps { lo: i64, hi: i64 },
    /// Never perturbed by the controller — only the author (and matrix routes into it) move it.
    /// The binaural pair's `root_hz`/`spread_hz` (ADR 0047): drift must not wander the beat.
    Fixed,
}

/// One param's registry row. Params are keyed by their **path leaf** (`cutoff_hz`,
/// `osc_b_ratio`, …) — the same convention `param_range` always matched on.
pub struct ParamSpec {
    pub leaf: &'static str,
    /// Normalized `0..1` ↔ the native value a doc writes (Hz, s, dB, …), or for a unitless param
    /// the physical value the DSP computes with (`resonance` → Q). `None` ⇒ a unitless `0..1` (a
    /// mix, a position, a level of the envelope) that is its own value.
    pub curve: Option<Curve>,
    /// What a doc writes: the native value, or a unitless `0..1`.
    pub authored: Authored,
    /// The display unit (hovers, the TUI, the catalog).
    pub unit: Unit,
    /// Tier-2 safety clamp (ADR 0011) in native units; `None` ⇒ the full range. Any `Some` here
    /// must have a matching `x-safe-bounds-defaults` entry in the JSON schema (parity-tested).
    pub safe_default: Option<(f32, f32)>,
    pub kind: ParamKind,
}

impl ParamSpec {
    /// The range a doc writes the param in (the ADR 0005 seam's native range): the curve's ends
    /// for a natively-authored param, else `(0, 1)`. A gain may also be `-Infinity` (silence).
    pub fn doc_range(&self) -> (f32, f32) {
        match (self.authored, self.curve) {
            (Authored::Native, Some(c)) => c.ends(),
            _ => (0.0, 1.0),
        }
    }

    /// The unit a route into this param takes its depth in.
    pub fn depth_unit(&self) -> DepthUnit {
        DepthUnit::of(self)
    }

    /// A gain (`*_db` on the [`GAIN`] curve): `-Infinity` is its silence.
    pub fn is_gain(&self) -> bool {
        self.curve == Some(GAIN)
    }
}

const CONT: ParamKind = ParamKind::Continuous;
const NORM: Authored = Authored::Normalized;
const NATIVE: Authored = Authored::Native;
const N: Unit = Unit::Normalized;
/// Tier-2 band of a gain with no floor of its own: the finite floor up to unity (ADR 0054 P1).
const GAIN_SAFE: Option<(f32, f32)> = Some((GAIN_FLOOR_DB, 0.0));

/// A row: `p(leaf, curve, authored, unit, safe_default, kind)`.
const fn p(leaf: &'static str, curve: Option<Curve>, authored: Authored, unit: Unit, safe_default: Option<(f32, f32)>, kind: ParamKind) -> ParamSpec {
    ParamSpec { leaf, curve, authored, unit, safe_default, kind }
}

/// Every registered param (schema v0.9).
pub const PARAMS: &[ParamSpec] = &[
    // ── engine/base params ──
    p("cutoff_hz", Some(CUTOFF), NATIVE, Unit::Hz, Some((101.0, 8000.0)), CONT), // never fully closed → silence
    p("resonance", Some(RESONANCE_Q), NORM, N, None, CONT),
    p("level_db", Some(GAIN), NATIVE, Unit::Db, Some((-26.0, 0.0)), CONT), // a floor: drift can't walk a voice to silence (S2-h2)
    p("drive_db", Some(DRIVE), NATIVE, Unit::Db, None, CONT),
    p("attack_s", Some(ATTACK), NATIVE, Unit::Seconds, None, CONT),
    p("decay_s", Some(DECAY), NATIVE, Unit::Seconds, None, CONT), // the ADSR's, and the `event` bell's ring
    p("sustain", None, NORM, N, None, CONT),
    p("release_s", Some(RELEASE), NATIVE, Unit::Seconds, None, CONT),
    p("band_lo_hz", Some(BAND), NATIVE, Unit::Hz, None, CONT),
    p("band_hi_hz", Some(BAND), NATIVE, Unit::Hz, None, CONT),
    p("voices_detune_cents", Some(VOICES_DETUNE), NATIVE, Unit::Cents, None, CONT),
    p("stereo_width", None, NORM, N, None, CONT),
    p("master_gain_db", Some(GAIN), NATIVE, Unit::Db, Some((-8.0, 0.0)), CONT), // never silence; output is already hot
    p("shift_st", Some(SHIFT), NATIVE, Unit::Semitones, None, CONT),
    p("shift_mix", None, NORM, N, None, CONT),
    // ── send-bus / reverb effect params ──
    p("room", None, NORM, N, None, CONT),
    p("damping", None, NORM, N, None, CONT),
    p("shimmer", None, NORM, N, None, CONT),
    p("width", None, NORM, N, None, CONT), // build-time-fixed (never drifted)
    p("predelay_ms", Some(PREDELAY), NATIVE, Unit::Millis, None, CONT),
    p("return_gain_db", Some(GAIN), NATIVE, Unit::Db, GAIN_SAFE, CONT),
    // ── synth-type params (ADR 0009): dual-osc + modal continuous levers ──
    p("osc_a_level_db", Some(GAIN), NATIVE, Unit::Db, GAIN_SAFE, CONT),
    p("osc_b_level_db", Some(GAIN), NATIVE, Unit::Db, GAIN_SAFE, CONT),
    p("osc_b_ratio", Some(OSC_B_RATIO), NATIVE, Unit::Ratio, None, CONT),
    p("osc_b_detune_cents", Some(OSC_B_DETUNE), NATIVE, Unit::Cents, None, CONT),
    p("fm_index", Some(FM_INDEX), NORM, N, None, CONT),
    p("fm_feedback", None, NORM, N, Some((0.0, 0.95)), CONT), // avoid runaway self-FM
    p("t60_s", Some(T60), NATIVE, Unit::Seconds, None, CONT), // `t60_mode: "fixed"`
    p("ring_ratio", Some(RING_RATIO), NATIVE, Unit::Ratio, None, CONT), // `t60_mode: "note"`
    p("brightness", None, NORM, N, None, CONT),
    p("strike_pos", None, NORM, N, None, CONT),
    p("stretch", None, NORM, N, None, CONT),
    // ── categorical timbre levers (flipped, never stepped) ──
    p("wave", None, NORM, N, None, ParamKind::Categorical(&WAVE_OPTIONS)),
    p("osc_a_wave", None, NORM, N, None, ParamKind::Categorical(&WAVE_OPTIONS)),
    p("osc_b_wave", None, NORM, N, None, ParamKind::Categorical(&WAVE_OPTIONS)),
    p("material", None, NORM, N, None, ParamKind::Categorical(&MATERIAL_OPTIONS)),
    p("num_modes", None, NORM, N, None, ParamKind::IntSteps { lo: 1, hi: 8 }),
    // ── binaural (ADR 0047): native Hz, matrix-modulatable, never drifted ──
    p("root_hz", Some(ROOT_HZ), NATIVE, Unit::HzNative, None, ParamKind::Fixed),
    p("spread_hz", Some(SPREAD_HZ), NATIVE, Unit::HzNative, None, ParamKind::Fixed),
];

/// The registry row of an instrument's send (`instruments.<i>.sends_db.<bus>`): a gain like the
/// others, keyed by bus rather than by leaf.
pub const SEND: ParamSpec = p("sends_db", Some(GAIN), NATIVE, Unit::Db, GAIN_SAFE, CONT);

/// The registry row for a path leaf (`cutoff_hz`, `osc_b_ratio`, …), if registered.
/// Unregistered leaves behave as `(0,1)` Continuous — same defaults as ever.
pub fn spec_for(leaf: &str) -> Option<&'static ParamSpec> {
    PARAMS.iter().find(|s| s.leaf == leaf)
}

/// The registry row a JSONPath names: its leaf's, or a send's for `instruments.<i>.sends_db.<bus>`.
pub fn spec_for_path(path: &str) -> Option<&'static ParamSpec> {
    let mut parts = path.rsplit('.');
    let leaf = parts.next()?;
    if parts.next() == Some("sends_db") && path.starts_with("instruments.") {
        return Some(&SEND);
    }
    spec_for(leaf)
}

/// A param's curve by leaf; `None` for a unitless or unregistered one.
pub fn curve_for(leaf: &str) -> Option<Curve> {
    spec_for(leaf).and_then(|s| s.curve)
}

/// An **authored** value (as a v0.9 doc writes it) → the engine's normalized `0..1`: through the
/// curve for a native param (a gain's `-Infinity` stays `-inf`), the value itself for a unitless
/// or unregistered one (not clamped, as the render always read it).
pub fn norm_of_authored(leaf: &str, v: f32) -> f32 {
    match spec_for(leaf) {
        Some(ParamSpec { authored: Authored::Native, curve: Some(c), .. }) => c.inverse(v),
        _ => v,
    }
}

/// An **authored** value → its physical value: as written for a native param (Hz, s, st, dB, …;
/// no normalize round trip), through the curve for a unitless one with a DSP unit (`resonance` →
/// Q, `fm_index` → radians), itself otherwise.
pub fn native_of_authored(leaf: &str, v: f32) -> f32 {
    match spec_for(leaf) {
        Some(s) if s.authored == Authored::Native => v,
        Some(ParamSpec { curve: Some(c), .. }) => c.eval(v),
        _ => v,
    }
}

/// A param default: a number (native, as a doc writes it), a count or an option name.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
#[serde(untagged)]
pub enum Dflt {
    Num(f32),
    Int(i64),
    Str(&'static str),
}

/// What the engine reads for an **omitted** param, as a v0.9 doc would write it. The render, the
/// synth and effect configs, a route's base (a route into an omitted param starts from it) and the
/// capability catalog all read this table, so a param written at its default sounds exactly like
/// one left out (the catalog's render tests prove it). The frequencies and times are v0.8's
/// normalized defaults through their curves (`cutoff` 0.7 ⇒ 2009.51 Hz); the levels are its
/// amplitudes in dB (½ ⇒ −6.02 dB). A param missing here has no single default (`root_hz`: absent
/// ⇒ the note's pitch).
pub const DEFAULTS: &[(&str, Dflt)] = &[
    // engine/loop_render.rs — InstBase
    ("wave", Dflt::Str("sine")),
    ("source", Dflt::Str("sine")),
    ("level_db", Dflt::Num(-6.0206)),
    ("drive_db", Dflt::Num(0.0)),
    ("cutoff_hz", Dflt::Num(2009.51)),
    ("resonance", Dflt::Num(0.0)),
    ("attack_s", Dflt::Num(0.0)),
    ("decay_s", Dflt::Num(0.0)),
    ("sustain", Dflt::Num(1.0)),
    ("release_s", Dflt::Num(0.0)),
    ("band_lo_hz", Dflt::Num(174.94)),
    ("band_hi_hz", Dflt::Num(1829.22)),
    ("voices_detune_cents", Dflt::Num(0.0)),
    ("shift_st", Dflt::Num(0.0)),
    ("shift_mix", Dflt::Num(0.0)),
    // synth.rs — DualConfig
    ("osc_a_wave", Dflt::Str("sine")),
    ("osc_b_wave", Dflt::Str("sine")),
    ("osc_a_level_db", Dflt::Num(-1.9382)),
    ("osc_b_level_db", Dflt::Num(f32::NEG_INFINITY)),
    ("osc_b_ratio", Dflt::Num(1.0)),
    ("osc_b_detune_cents", Dflt::Num(0.0)),
    ("fm_index", Dflt::Num(0.0)),
    ("fm_feedback", Dflt::Num(0.0)),
    // synth.rs — ModalConfig
    ("material", Dflt::Str("marimba")),
    ("num_modes", Dflt::Int(4)),
    ("brightness", Dflt::Num(0.6)),
    ("strike_pos", Dflt::Num(0.3)),
    ("t60_s", Dflt::Num(0.632456)),
    ("ring_ratio", Dflt::Num(1.0)),
    ("t60_mode", Dflt::Str("fixed")),
    ("stretch", Dflt::Num(0.0)),
    // synth.rs — BinauralConfig
    ("spread_hz", Dflt::Num(0.0)),
    // effects.rs — Bus::from_doc reads an absent reverb param as 0
    ("room", Dflt::Num(0.0)),
    ("damping", Dflt::Num(0.0)),
    ("shimmer", Dflt::Num(0.0)),
    ("width", Dflt::Num(0.0)),
    ("predelay_ms", Dflt::Num(0.0)),
    // loop_doc.rs — GlobalParams / Bus serde defaults
    ("master_gain_db", Dflt::Num(0.0)),
    ("stereo_width", Dflt::Num(0.5)),
    ("return_gain_db", Dflt::Num(f32::NEG_INFINITY)),
];

/// The default for `leaf`, if it has one.
pub fn default_of(leaf: &str) -> Option<Dflt> {
    DEFAULTS.iter().find(|(l, _)| *l == leaf).map(|&(_, d)| d)
}

/// A numeric default (a count included), native.
pub fn default_num(leaf: &str) -> Option<f32> {
    match default_of(leaf)? {
        Dflt::Num(v) => Some(v),
        Dflt::Int(n) => Some(n as f32),
        Dflt::Str(_) => None,
    }
}

/// A numeric default in the engine's normalized space.
pub fn default_norm(leaf: &str) -> Option<f32> {
    default_num(leaf).map(|v| norm_of_authored(leaf, v))
}

/// The v0.9 name of a schema v0.8 key (`cutoff` → `cutoff_hz`), for the check's "did you mean".
pub fn v09_name(old: &str) -> Option<&'static str> {
    Some(match old {
        "cutoff" => "cutoff_hz",
        "band_lo" => "band_lo_hz",
        "band_hi" => "band_hi_hz",
        "voices_detune" => "voices_detune_cents",
        "osc_b_detune" => "osc_b_detune_cents",
        "attack" => "attack_s",
        "decay" => "decay_s",
        "release" => "release_s",
        "level" => "level_db",
        "osc_a_level" => "osc_a_level_db",
        "osc_b_level" => "osc_b_level_db",
        "drive" => "drive_db",
        "shift" => "shift_st",
        "predelay" => "predelay_ms",
        "master_gain" => "master_gain_db",
        "return_gain" => "return_gain_db",
        "sends" => "sends_db",
        "t60_scale" => "t60_s",
        _ => return None,
    })
}

/// A [`ParamKind::Fixed`] leaf: the controller never dithers it, nor the depth of a route into it.
pub fn is_fixed(leaf: &str) -> bool {
    spec_for(leaf).is_some_and(|s| s.kind == ParamKind::Fixed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The doc ranges and Tier-2 bands are native (schema v0.9, ADR 0054): the curve's ends, the
    /// safety floors in the param's own unit (v0.8's `cutoff 0.05` ⇒ 101 Hz, `level 0.05` ⇒ −26 dB,
    /// `master_gain 0.4` ⇒ −8 dB) and every gain's safe ceiling at 0 dB (P1).
    #[test]
    fn doc_ranges_and_safe_bands_are_native() {
        for (leaf, range) in [
            ("cutoff_hz", (80.0, 8000.0)),
            ("band_lo_hz", (20.0, 4000.0)),
            ("band_hi_hz", (20.0, 4000.0)),
            ("attack_s", (0.0, 3.0)),
            ("decay_s", (0.0, 4.0)),
            ("release_s", (0.0, 4.0)),
            ("voices_detune_cents", (0.0, 100.0)),
            ("osc_b_detune_cents", (0.0, 50.0)),
            ("level_db", (-60.0, 6.0)),
            ("master_gain_db", (-60.0, 6.0)),
            ("drive_db", (0.0, 20.0)),
            ("shift_st", (-24.0, 24.0)),
            ("predelay_ms", (0.0, 100.0)),
            ("osc_b_ratio", (0.5, 12.0)),
            ("root_hz", (20.0, 1000.0)),
            ("spread_hz", (0.0, 40.0)),
            ("ring_ratio", (0.5, 2.0)),
            ("resonance", (0.0, 1.0)),
            ("fm_index", (0.0, 1.0)),
        ] {
            let (lo, hi) = spec_for(leaf).unwrap().doc_range();
            assert!((lo - range.0).abs() < 1e-4 && (hi - range.1).abs() < 1e-2, "{leaf}: {lo}..{hi}");
        }
        let (lo, hi) = spec_for("t60_s").unwrap().doc_range();
        assert!((lo - 0.05).abs() < 1e-6 && (hi - 8.0).abs() < 1e-4);
        assert_eq!(spec_for("master_gain_db").unwrap().safe_default, Some((-8.0, 0.0)));
        assert_eq!(spec_for("cutoff_hz").unwrap().safe_default, Some((101.0, 8000.0)));
        assert_eq!(spec_for("level_db").unwrap().safe_default, Some((-26.0, 0.0)));
        assert_eq!(spec_for("fm_feedback").unwrap().safe_default, Some((0.0, 0.95)));
        for s in PARAMS.iter().filter(|s| s.is_gain()).chain([&SEND]) {
            assert_eq!(s.safe_default.map(|(_, hi)| hi), Some(0.0), "{}: a gain's safe ceiling is unity", s.leaf);
        }
        let clamped: Vec<&str> = PARAMS.iter().filter(|s| s.safe_default.is_some()).map(|s| s.leaf).collect();
        assert_eq!(clamped, ["cutoff_hz", "level_db", "master_gain_db", "return_gain_db", "osc_a_level_db", "osc_b_level_db", "fm_feedback"]);
        assert_eq!(spec_for_path("instruments.pad.sends_db.rv").map(|s| s.leaf), Some("sends_db"));
        assert!(spec_for_path("instruments.pad.params.sends_db").is_none(), "only a send path is a send");
    }

    /// `Fixed` is the binaural pair and nothing else — a new fixed param is a deliberate choice.
    #[test]
    fn fixed_params_are_the_binaural_hz_pair() {
        let fixed: Vec<&str> = PARAMS.iter().filter(|s| s.kind == ParamKind::Fixed).map(|s| s.leaf).collect();
        assert_eq!(fixed, ["root_hz", "spread_hz"]);
        assert!(is_fixed("root_hz") && is_fixed("spread_hz"));
        assert!(!is_fixed("cutoff_hz") && !is_fixed("x_note"));
    }

    /// The frequency and time curves are v0.8's maps, bit for bit (these formulas are copied from the
    /// pre-curve code, not derived), so their normalized space — where drift and routes add —
    /// didn't move with schema v0.9; only the decay's span grew (3 → 4 s, P7). The gains moved to dB.
    /// The band edges' curve alone was re-anchored since (20 Hz × 200ⁿ).
    #[test]
    fn curves_reproduce_the_old_maps_bit_for_bit() {
        let bits = |a: f32, b: f32, what: &str, x: f32| assert_eq!(a.to_bits(), b.to_bits(), "{what} at {x}: {a} vs {b}");
        for i in 0..=1000 {
            let x = i as f32 / 1000.0;
            bits(CUTOFF.eval(x), 80.0 * 100.0_f32.powf(x), "cutoff", x);
            bits(BAND.eval(x), 20.0 * 200.0_f32.powf(x), "band", x);
            bits(T60.eval(x), T60_MIN * (T60_MAX / T60_MIN).powf(x), "t60", x);
            bits(RING_RATIO.eval(x), 4.0f32.powf(x - 0.5), "ring ratio", x);
            bits(RESONANCE_Q.eval(x), 0.707 + x * (6.0 - 0.707), "resonance Q", x);
            for (c, max, what) in [(ATTACK, ATTACK_MAX_S, "attack"), (DECAY, DECAY_MAX_S, "decay"), (RELEASE, RELEASE_MAX_S, "release"), (FM_INDEX, 8.0, "fm index"), (OSC_B_DETUNE, 50.0, "osc-B detune"), (VOICES_DETUNE, 100.0, "voices detune")] {
                bits(c.eval(x), x * max, what, x);
            }
            for (c, (lo, hi), what) in [(SHIFT, SHIFT_RANGE, "shift"), (PREDELAY, PREDELAY_RANGE, "predelay"), (OSC_B_RATIO, OSC_B_RATIO_RANGE, "osc-B ratio"), (ROOT_HZ, ROOT_HZ_RANGE, "root Hz"), (SPREAD_HZ, SPREAD_HZ_RANGE, "spread Hz")] {
                bits(c.eval(x), lo + x * (hi - lo), what, x);
                bits(c.inverse(c.eval(x)), ((c.eval(x) - lo) / (hi - lo)).clamp(0.0, 1.0), what, x);
            }
        }
        for hz in [1.0, 40.0, 80.0, 100.0, 318.0, 1000.0, 4000.0, 8000.0, 16_000.0, 48_000.0] {
            bits(CUTOFF.inverse(hz), ((hz.max(1e-6) / 80.0).ln() / 100.0_f32.ln()).clamp(0.0, 1.0), "Hz → cutoff", hz);
            bits(BAND.inverse(hz), ((hz.max(1e-6) / 20.0).ln() / 200.0_f32.ln()).clamp(0.0, 1.0), "Hz → band", hz);
        }
        assert_eq!(DECAY_MAX_S, 4.0, "both envelopes decay over 0–4 s");
    }

    /// Re-anchoring the band curve at 20 Hz keeps every band edge a doc could write before
    /// (80..4000 Hz): the Hz the engine plays for it moves by float rounding only, ≤ 1e-6 of itself.
    #[test]
    fn the_band_curve_keeps_the_old_ranges_hz() {
        let old = Curve::Exp { lo: 80.0, base: 50.0, shift: 0.0 };
        for hz in (80..=4000).map(|v| v as f32).chain([174.94, 687.9, 1829.22]) {
            let (was, now) = (old.eval(old.inverse(hz)), BAND.eval(BAND.inverse(hz)));
            assert!(((now - was) / was).abs() <= 1e-6, "{hz} Hz: {was} → {now}");
        }
        assert_eq!(BAND.ends(), (20.0, 4000.0));
    }

    /// Gains are dB over −60..+6 (P2), `-Infinity` silent everywhere; the drive's pre-gain is dB over
    /// 0..+20, and lands on v0.8's `1 + 9x` for the `x` it migrated from.
    #[test]
    fn gains_are_db_and_silence_stays_silent() {
        assert_eq!((GAIN.eval(0.0), GAIN.eval(1.0)), (-60.0, 6.0));
        assert_eq!(GAIN.inverse(-6.0), 54.0 / 66.0);
        assert_eq!(GAIN.inverse(f32::NEG_INFINITY), f32::NEG_INFINITY, "silence isn't clamped up to the floor");
        assert_eq!(GAIN.inverse(-90.0), 0.0, "below the floor clamps to it");
        assert_eq!(GAIN.eval(f32::NEG_INFINITY), f32::NEG_INFINITY, "and a sum onto it stays silent");
        assert_eq!(gain_amp(f32::NEG_INFINITY), 0.0);
        assert!((gain_amp(GAIN.inverse(amp_to_db(0.5))) - 0.5).abs() < 1e-6, "an amplitude round-trips through dB");
        assert_eq!((db_to_amp(0.0), db_to_amp(f32::NEG_INFINITY), amp_to_db(0.0)), (1.0, 0.0, f32::NEG_INFINITY));
        for x in [0.0f32, 0.1, 0.3, 0.55, 1.0] {
            let db = 20.0 * (1.0 + 9.0 * x).log10();
            let pre = db_to_amp(DRIVE.eval(DRIVE.inverse(db)));
            assert!((pre - (1.0 + 9.0 * x)).abs() < 1e-4 * (1.0 + 9.0 * x), "drive {x}: ×{pre}");
        }
        assert_eq!((GAIN.drift_scale(), DRIVE.drift_scale(), CUTOFF.drift_scale(), ATTACK.drift_scale()), (0.3, 0.3, 1.0, 1.0));
        assert!((CUTOFF.depth_span() - 100f32.log2()).abs() < 1e-6, "6.64 octaves span a cutoff");
        assert_eq!((GAIN.depth_span(), ATTACK.depth_span(), SHIFT.depth_span()), (66.0, 3.0, 48.0));
    }

    /// The display unit, the doc range and the curve agree: a native-authored param has a curve in its
    /// unit, a param shown in a unit is native-authored, and a route into it takes its depth key.
    #[test]
    fn rows_are_consistent() {
        for s in PARAMS {
            if s.authored == Authored::Native {
                assert!(s.curve.is_some(), "{}: native without a curve", s.leaf);
                assert_ne!(s.unit, Unit::Normalized, "{}: native but shown as 0..1", s.leaf);
            } else {
                assert_eq!(s.unit, Unit::Normalized, "{}: a unit but authored 0..1", s.leaf);
            }
            if s.curve.is_none() {
                assert_eq!(native_of_authored(s.leaf, 0.3), 0.3, "{}: unitless ⇒ its own value", s.leaf);
            }
        }
        assert_eq!(norm_of_authored("shift_st", 12.0), 0.75);
        assert_eq!(native_of_authored("shift_st", 12.0), 12.0, "native as written, no round trip");
        assert_eq!(norm_of_authored("release_s", 2.0), 0.5);
        assert_eq!(native_of_authored("release_s", 2.0), 2.0);
        assert_eq!(norm_of_authored("resonance", 0.4), 0.4, "unitless: as written");
        assert_eq!(native_of_authored("resonance", 0.0), 0.707, "…its DSP reading through the curve");
        assert_eq!(native_of_authored("x_unregistered", 0.4), 0.4);
        for (leaf, unit) in [("cutoff_hz", DepthUnit::Oct), ("t60_s", DepthUnit::Oct), ("ring_ratio", DepthUnit::Oct), ("attack_s", DepthUnit::S), ("level_db", DepthUnit::Db), ("drive_db", DepthUnit::Db), ("shift_st", DepthUnit::St), ("voices_detune_cents", DepthUnit::Cents), ("predelay_ms", DepthUnit::Ms), ("root_hz", DepthUnit::Hz), ("osc_b_ratio", DepthUnit::X), ("resonance", DepthUnit::Plain), ("stereo_width", DepthUnit::Plain)] {
            assert_eq!(spec_for(leaf).unwrap().depth_unit(), unit, "{leaf}");
        }
        for u in DepthUnit::ALL {
            assert_eq!(DepthUnit::from_key(u.key()), Some(u));
        }
    }

    /// Every numeric default the engine reads is in its param's doc range (or silence for a gain).
    #[test]
    fn defaults_are_in_range() {
        for (leaf, d) in DEFAULTS {
            let (Dflt::Num(v), Some(s)) = (*d, spec_for(leaf)) else { continue };
            let (lo, hi) = s.doc_range();
            assert!((lo..=hi).contains(&v) || (s.is_gain() && v == f32::NEG_INFINITY), "{leaf}: {v} outside {lo}..{hi}");
        }
        assert!((CUTOFF.inverse(default_num("cutoff_hz").unwrap()) - 0.7).abs() < 1e-6, "v0.8's cutoff 0.7");
        assert!((T60.inverse(default_num("t60_s").unwrap()) - 0.5).abs() < 1e-6, "v0.8's t60_scale 0.5");
        assert!((db_to_amp(default_num("level_db").unwrap()) - 0.5).abs() < 1e-6, "v0.8's level ½");
    }

    /// The render path reads a param only through the seam (`Instrument::norm`/`native`,
    /// `BusEffect::norm`, the typed gain fields through [`GAIN`]), never the authored number, so the
    /// schema v0.9 change of what a doc writes (ADR 0054) lands in one place. A raw read that
    /// slipped past would mis-render silently (`cutoff_hz: 318` read as 0..1 clamps the filter open).
    #[test]
    fn the_render_path_reads_params_only_through_the_seam() {
        const RENDER_PATH: &[&str] = &[
            "engine.rs", "engine/loop_render.rs", "synth.rs", "effects.rs", "dsp.rs", "song.rs", "categorical.rs", "session.rs",
            "session/live.rs", "session/worker.rs", "voice.rs", "matrix.rs", "modulator.rs", "stems.rs", "phrase.rs", "euclid.rs",
            "bin/engines-play.rs",
        ];
        const RAW: &[&str] = &[".authored_num(", "params.get(", ".as_f64()"];
        let src = concat!(env!("CARGO_MANIFEST_DIR"), "/src/");
        for file in RENDER_PATH {
            let text = std::fs::read_to_string(format!("{src}{file}")).unwrap_or_else(|e| panic!("{file}: {e}"));
            let code = text.split("#[cfg(test)]\nmod tests").next().unwrap();
            for (n, line) in code.lines().enumerate() {
                let line = line.split("//").next().unwrap();
                for raw in RAW {
                    assert!(!line.contains(raw), "{file}:{}: `{raw}` reads a param past the seam — use Instrument::norm/native or BusEffect::norm", n + 1);
                }
            }
        }
    }

    #[test]
    fn leaves_are_unique() {
        for s in PARAMS {
            assert_eq!(PARAMS.iter().filter(|o| o.leaf == s.leaf).count(), 1, "{}", s.leaf);
        }
    }
}
