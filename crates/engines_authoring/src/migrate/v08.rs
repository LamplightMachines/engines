//! The **frozen** schema v0.8 maps (ADR 0054 P9): what a v0.8 doc's numbers meant — copied from the
//! engine as it read v0.8 (`engine::cutoff_to_hz`, `synth::t60_fundamental`, …), never imported from
//! it, so the migrator keeps reading v0.8 once the engine reads only v0.9. A test pins them to the
//! engine's own maps while the engine still reads v0.8.

/// `cutoff` (0..1) → Hz: 80 Hz × 100ˣ.
pub fn cutoff_hz(x: f64) -> f64 {
    80.0 * 100f64.powf(x)
}

/// `band_lo`/`band_hi` (0..1) → Hz: 80 Hz × 50ˣ.
pub fn band_hz(x: f64) -> f64 {
    80.0 * 50f64.powf(x)
}

/// Modal `t60_scale` under `t60_mode: "fixed"` → the fundamental's ring time (s): 0.05 s × 160ˣ.
pub fn t60_s(x: f64) -> f64 {
    T60_MIN_S * (T60_MAX_S / T60_MIN_S).powf(x)
}

/// Modal `t60_scale` under `t60_mode: "note"` → the ring as a ratio of the note: 4^(x − ½).
pub fn ring_ratio(x: f64) -> f64 {
    4f64.powf(x - 0.5)
}

/// `drive` (0..1) → the soft clipper's pre-gain in dB: 20·log10(1 + 9x).
pub fn drive_db(x: f64) -> f64 {
    20.0 * (1.0 + 9.0 * x).log10()
}

pub const ATTACK_MAX_S: f64 = 3.0;
/// The ADSR's decay at 1.
pub const DECAY_MAX_S: f64 = 3.0;
/// The legacy `event` bell's ring at `decay` 1 (an instrument named `event`, subtractive).
pub const BELL_DECAY_MAX_S: f64 = 4.0;
pub const RELEASE_MAX_S: f64 = 4.0;
/// `voices_detune` 1 spreads the outermost voices ± this many cents.
pub const VOICES_DETUNE_CENTS: f64 = 100.0;
/// `osc_b_detune` 1 is this many cents up.
pub const OSC_B_DETUNE_CENTS: f64 = 50.0;
pub const T60_MIN_S: f64 = 0.05;
pub const T60_MAX_S: f64 = 8.0;

/// The ADR 0005 seam params' native spans: a v0.8 route's depth was a fraction of these.
pub const SHIFT_SPAN_ST: f64 = 48.0;
pub const PREDELAY_SPAN_MS: f64 = 100.0;
pub const OSC_B_RATIO_SPAN: f64 = 11.5;
pub const ROOT_HZ_SPAN: f64 = 980.0;
pub const SPREAD_HZ_SPAN: f64 = 40.0;

/// What the engine read for an omitted gain — the base a route into it starts from.
pub const LEVEL_DEFAULT: f64 = 0.5;
pub const OSC_A_LEVEL_DEFAULT: f64 = 0.8;
pub const OSC_B_LEVEL_DEFAULT: f64 = 0.0;
pub const DRIVE_DEFAULT: f64 = 0.0;
pub const MASTER_GAIN_DEFAULT: f64 = 1.0;
pub const RETURN_GAIN_DEFAULT: f64 = 0.0;

/// The bus a v0.8 doc without buses got from `global.reverb_mix` (`LoopDoc::backfill_buses`), its
/// reverb and that reverb's params; every non-midi instrument without sends sent 1.0 into it.
pub const DEFAULT_BUS: &str = "rv";
pub const DEFAULT_REVERB_FX: &str = "verb";
pub const DEFAULT_REVERB: &str = r#"{ type: "reverb", params: { room: 0.5, damping: 0.5, shimmer: 0, width: 0.5, predelay_ms: 0 } }"#;
