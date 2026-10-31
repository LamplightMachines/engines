//! Param **units / display registry** (ADR 0024; moved into the core for the language server,
//! ADR 0037) — the TUI's instrument screen + constraint editor and `engines-lsp`'s hovers share it.
//!
//! A schema v0.9 doc writes each param in its native unit (ADR 0054); internally the engine moves
//! it in a normalized 0..1 (the ADR 0005 seam). This formats either in the param's real-world unit.
//! The unit is the param registry's `unit` column and a normalized value goes through the
//! registry's [`Curve`](super::Curve) — the same curve the engine renders with — so the displayed
//! value can't drift from what the engine plays.

use super::{spec_for_path, Authored, Curve};

/// Real-world unit of a param.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    /// Plain 0..1 (a mix, a position, resonance, the envelope's sustain level, …).
    Normalized,
    /// Filter / bandpass frequency in Hz (log-mapped by the engine).
    Hz,
    /// Envelope or ring time in seconds.
    Seconds,
    /// Pre-delay in milliseconds.
    Millis,
    /// Octave-pedal interval in semitones.
    Semitones,
    /// A detune in cents.
    Cents,
    /// A frequency ratio (osc B's, a modal ring's of its note).
    Ratio,
    /// A gain or the drive's pre-gain, in dB (`-Infinity` = silence for a gain).
    Db,
    /// A real frequency **authored in Hz** (the binaural `root_hz`/`spread_hz`, ADR 0047) — linear,
    /// not log-mapped like [`Unit::Hz`].
    HzNative,
}

/// The JSONPath leaf (last `.`-segment), e.g. `instruments.pad.params.cutoff_hz` → `"cutoff_hz"`.
pub fn leaf(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or("")
}

/// The display unit for a param path (the registry's `unit`; [`Unit::Normalized`] when unregistered).
pub fn unit_of(path: &str) -> Unit {
    spec_for_path(path).map_or(Unit::Normalized, |s| s.unit)
}

fn curve_of(path: &str) -> Option<Curve> {
    spec_for_path(path).and_then(|s| s.curve)
}

/// Short unit label for an axis/column header.
pub fn unit_label(path: &str) -> &'static str {
    match unit_of(path) {
        Unit::Normalized => "0..1",
        Unit::Hz | Unit::HzNative => "Hz",
        Unit::Seconds => "s",
        Unit::Millis => "ms",
        Unit::Semitones => "st",
        Unit::Cents => "¢",
        Unit::Ratio => "×",
        Unit::Db => "dB",
    }
}

/// Format a **normalized** 0..1 value (the controller's and the TUI's view) as a native-units
/// string (`nyq` = Nyquist for the Hz maps). A gain's `-inf` reads as silence.
pub fn fmt_native(path: &str, norm: f32, nyq: f32) -> String {
    let n = if norm == f32::NEG_INFINITY { norm } else { norm.clamp(0.0, 1.0) };
    match spec_for_path(path) {
        Some(s) if s.authored == Authored::Native => fmt_value(path, s.curve.map_or(n, |c| c.eval(n)), nyq),
        _ => format!("{n:.3}"),
    }
}

/// Format a value **as a v0.9 doc writes it** in its unit: `cutoff_hz: 318.49` → `"318Hz"`,
/// `attack_s: 1.05` → `"1.05s"`, `level_db: -6` → `"-6.0dB"`, `resonance: 0.3` → `"0.300"`.
pub fn fmt_authored(path: &str, value: f32) -> String {
    fmt_value(path, value, f32::INFINITY)
}

fn fmt_value(path: &str, v: f32, nyq: f32) -> String {
    match unit_of(path) {
        Unit::Hz => {
            let hz = v.min(nyq);
            if hz >= 1000.0 {
                format!("{:.2}kHz", hz / 1000.0)
            } else {
                format!("{hz:.0}Hz")
            }
        }
        Unit::Seconds => format!("{v:.2}s"),
        Unit::Millis => format!("{v:.0}ms"),
        Unit::Semitones => format!("{v:+.0}st"),
        Unit::Cents => format!("{v:.1}¢"),
        Unit::Ratio => format!("{v:.2}×"),
        Unit::Db if v == f32::NEG_INFINITY => "-∞dB".to_string(),
        Unit::Db => format!("{v:.1}dB"),
        Unit::HzNative => fmt_hz(v),
        Unit::Normalized => format!("{v:.3}"),
    }
}

/// A real frequency to at most two decimals, trailing zeros trimmed: `110` → `"110Hz"`,
/// `109.5` → `"109.5Hz"`, `2.25` → `"2.25Hz"`.
pub fn fmt_hz(hz: f32) -> String {
    let s = format!("{hz:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    format!("{s}Hz")
}

/// The brainwave band a binaural beat rate sits in (ADR 0047) — the label sleep/focus tracks are
/// published under: delta < 4 Hz, theta 4–8, alpha 8–13, beta 13–30, gamma ≥ 30.
pub fn beat_band(hz: f32) -> &'static str {
    match hz {
        h if h < 4.0 => "delta",
        h if h < 8.0 => "theta",
        h if h < 13.0 => "alpha",
        h if h < 30.0 => "beta",
        _ => "gamma",
    }
}

/// A normalized value → the native value a doc writes (the curve's `eval`; a unitless param's own).
pub fn native_of_norm(path: &str, n: f32) -> f32 {
    match spec_for_path(path) {
        Some(s) if s.authored == Authored::Native => curve_of(path).map_or(n, |c| c.eval(n)),
        _ => n,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_match_param_leaves() {
        assert_eq!(unit_of("instruments.pad.params.cutoff_hz"), Unit::Hz);
        assert_eq!(unit_of("instruments.pad.params.attack_s"), Unit::Seconds);
        assert_eq!(unit_of("instruments.pad.params.shift_st"), Unit::Semitones);
        assert_eq!(unit_of("buses.rv.effects.verb.params.predelay_ms"), Unit::Millis);
        assert_eq!(unit_of("instruments.pad.params.osc_b_ratio"), Unit::Ratio);
        assert_eq!(unit_of("instruments.bin.params.root_hz"), Unit::HzNative);
        assert_eq!(unit_of("instruments.bin.params.spread_hz"), Unit::HzNative);
        assert_eq!(unit_of("global.master_gain_db"), Unit::Db);
        assert_eq!(unit_of("instruments.pad.sends_db.rv"), Unit::Db, "a send is a gain");
        assert_eq!(unit_of("instruments.pad.params.voices_detune_cents"), Unit::Cents);
        assert_eq!(unit_of("instruments.pad.params.resonance"), Unit::Normalized);
    }

    #[test]
    fn binaural_hz_params_display_as_authored() {
        // ADR 0047: real Hz, fractions kept exactly.
        assert_eq!(fmt_authored("instruments.bin.params.root_hz", 109.5), "109.5Hz");
        assert_eq!(fmt_authored("instruments.bin.params.root_hz", 110.0), "110Hz");
        assert_eq!(fmt_authored("instruments.bin.params.spread_hz", 2.25), "2.25Hz");
        assert_eq!(unit_label("instruments.bin.params.spread_hz"), "Hz");
        // a normalized value (the TUI's view) denormalizes over the native range
        assert_eq!(fmt_native("instruments.bin.params.spread_hz", 0.5, 24_000.0), "20Hz");
        assert_eq!(beat_band(2.0), "delta");
        assert_eq!(beat_band(4.0), "theta");
        assert_eq!(beat_band(10.0), "alpha");
        assert_eq!(beat_band(20.0), "beta");
        assert_eq!(beat_band(40.0), "gamma");
    }

    #[test]
    fn cutoff_formats_in_hz_matching_engine_map() {
        // 0 → ~80 Hz, 1 → ~8 kHz (the engine's cutoff_to_hz, one source of truth).
        let nyq = 24_000.0;
        assert_eq!(fmt_native("instruments.pad.params.cutoff_hz", 0.0, nyq), "80Hz");
        assert_eq!(fmt_native("instruments.pad.params.cutoff_hz", 1.0, nyq), "8.00kHz");
    }

    #[test]
    fn native_value_formats_use_real_units() {
        let nyq = 24_000.0;
        assert_eq!(fmt_native("instruments.pad.params.attack_s", 1.0, nyq), "3.00s");
        assert_eq!(fmt_native("instruments.pad.params.decay_s", 1.0, nyq), "4.00s", "both envelopes decay over 0–4 s");
        assert_eq!(fmt_native("instruments.pad.params.shift_st", 1.0, nyq), "+24st");
        assert_eq!(fmt_native("instruments.pad.params.shift_st", 0.5, nyq), "+0st");
        assert_eq!(fmt_native("buses.rv.effects.verb.params.predelay_ms", 1.0, nyq), "100ms");
        assert_eq!(fmt_native("global.master_gain_db", 1.0, nyq), "6.0dB");
        assert_eq!(fmt_native("global.master_gain_db", 0.0, nyq), "-60.0dB");
        assert_eq!(fmt_native("instruments.pad.params.level_db", f32::NEG_INFINITY, nyq), "-∞dB");
        assert_eq!(fmt_native("instruments.pad.params.resonance", 0.25, nyq), "0.250");
    }

    #[test]
    fn authored_values_format_in_their_units() {
        assert_eq!(fmt_authored("instruments.sub.params.cutoff_hz", 318.49), "318Hz");
        assert_eq!(fmt_authored("instruments.sub.params.attack_s", 1.05), "1.05s");
        assert_eq!(fmt_authored("instruments.halo.params.shift_st", 12.0), "+12st");
        assert_eq!(fmt_authored("buses.rv.effects.verb.params.predelay_ms", 20.0), "20ms");
        assert_eq!(fmt_authored("instruments.crank.params.osc_b_ratio", 1.0), "1.00×");
        assert_eq!(fmt_authored("instruments.crank.params.level_db", -5.19), "-5.2dB");
        assert_eq!(fmt_authored("instruments.crank.params.level_db", f32::NEG_INFINITY), "-∞dB");
        assert_eq!(fmt_authored("instruments.crank.params.resonance", 0.3), "0.300");
        assert_eq!(fmt_authored("instruments.event.params.decay_s", 2.0), "2.00s", "a bell's ring is seconds like any decay");
    }

    #[test]
    fn native_of_norm_inverts_the_curve() {
        assert!((native_of_norm("instruments.pad.params.cutoff_hz", 0.5) - 800.0).abs() < 1e-3);
        assert_eq!(native_of_norm("instruments.pad.params.attack_s", 0.5), 1.5);
        assert_eq!(native_of_norm("global.master_gain_db", 1.0), 6.0);
        assert_eq!(native_of_norm("instruments.pad.params.resonance", 0.4), 0.4, "unitless: its own value");
    }
}
