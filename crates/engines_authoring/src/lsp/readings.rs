//! What a written value means beyond its number (ADR 0054 U6). Schema v0.9 writes a param in its unit,
//! so the editor reads back what the unit doesn't say: the note nearest a cutoff, a gain's amplitude,
//! drive's pre-gain, both readings of `resonance`, `fm_index` in radians, and a route depth's swing at
//! its dest's base. Hover shows the sentence, an inlay hint the few characters after it.

use super::{music, Analysis};
use crate::cst::Node;
use engines_core::loop_doc::{depth_unit_for, param_range};
use engines_core::params::{default_num, spec_for_path, units, DepthUnit, FM_INDEX, RESONANCE_Q};

/// One value read back: a sentence for the hover, a label for the inlay hint.
#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    pub hover: String,
    pub inlay: String,
}

fn reading(hover: impl Into<String>, inlay: impl Into<String>) -> Option<Reading> {
    Some(Reading { hover: hover.into(), inlay: inlay.into() })
}

/// A param's value `x` at `path` (`instruments.pad.params.cutoff_hz`, `global.master_gain_db`, a send).
pub fn of_param(a: &Analysis, path: &str, x: f64) -> Option<Reading> {
    let leaf = units::leaf(path);
    if spec_for_path(path).is_some_and(|s| s.is_gain()) {
        return gain(x);
    }
    match leaf {
        "cutoff_hz" | "band_lo_hz" | "band_hi_hz" => {
            let note = music::note_of_hz(a.key.as_ref()?, x);
            reading(format!("≈ **{note}**, the nearest note"), format!("≈{note}"))
        }
        "drive_db" => {
            let pre = 10f64.powf(x / 20.0);
            if pre <= 1.009 {
                reading("0 dB: the soft clipper is bypassed", "bypass")
            } else {
                reading(format!("pre-gain **×{pre:.2}** into the soft clipper"), format!("×{pre:.2}"))
            }
        }
        "resonance" => {
            let q = RESONANCE_Q.eval(x as f32);
            let inst = path.strip_prefix("instruments.").and_then(|r| r.split('.').next()).and_then(|id| a.node(&format!("instruments.{id}")));
            let params = inst.and_then(|i| i.get("params"));
            let noise = params.and_then(|p| p.get("source")).and_then(Node::as_str) == Some("filtered_noise");
            let pitched = inst.and_then(|i| i.get("pitched")).and_then(Node::as_bool) == Some(true);
            if noise && pitched {
                reading(format!("the note band's peak on this pitched noise voice (on a filter: **Q {q:.2}**)"), format!("Q{q:.2} band"))
            } else {
                reading(format!("**Q {q:.2}** at the filter's corner (on a `pitched: true` filtered_noise voice it sets the note band's peak)"), format!("Q{q:.2}"))
            }
        }
        "fm_index" => {
            let rad = FM_INDEX.eval(x as f32);
            if rad == 0.0 {
                reading("0 rad: plain dual oscillators, no phase modulation", "0rad")
            } else {
                reading(format!("**{rad:.2} rad** of osc-B → osc-A phase modulation"), format!("{rad:.2}rad"))
            }
        }
        _ => None,
    }
}

/// A gain in dB as an amplitude, with words for the round ones.
fn gain(db: f64) -> Option<Reading> {
    if db == f64::NEG_INFINITY {
        return reading("**silent** (a route can't lift a `-Infinity` gain: give it a finite floor)", "silent");
    }
    let amp = 10f64.powf(db / 20.0);
    let words = [(1.0, "unity"), (2.0, "double"), (0.5, "half"), (0.25, "a quarter"), (0.1, "a tenth")]
        .iter()
        .find(|(v, _)| (amp / v - 1.0).abs() < 0.01)
        .map(|(_, w)| format!(" ({w})"))
        .unwrap_or_default();
    reading(format!("≈ **×{}** amplitude{words}", amp_text(amp)), format!("×{}", amp_text(amp)))
}

fn amp_text(amp: f64) -> String {
    if amp >= 0.01 { format!("{amp:.2}") } else { format!("{amp:.4}") }
}

/// A route's depth `d` (written under `key`) into `dest`, swung by `source`: the range it moves the
/// dest through around its base (the authored value, else the param's default), at full swing.
pub fn of_depth(a: &Analysis, dest: &str, key: &str, d: f64, source: Option<&str>) -> Option<Reading> {
    let unit = DepthUnit::from_key(key)?;
    if unit != depth_unit_for(dest) {
        return None; // the checker names the right key
    }
    let leaf = units::leaf(dest);
    let (base, authored) = match a.node(dest).and_then(Node::as_f64) {
        Some(x) => (x, true),
        None => (f64::from(default_num(leaf)?), false),
    };
    let around = if authored { "around" } else { "around its default" };
    if unit == DepthUnit::Db && base == f64::NEG_INFINITY {
        return reading(format!("`{key}` {d} on `{leaf}`: **inaudible**, the base is `-Infinity` (give it a finite floor, say −60 dB)"), "inaudible");
    }
    let (s_lo, s_hi) = source.map(|s| source_range(a, s)).unwrap_or((-1.0, 1.0));
    let (lo, hi) = param_range(dest);
    let at = |v: f64| -> f64 {
        let x = match unit {
            DepthUnit::Oct => base * 2f64.powf(d * v),
            _ => base + d * v,
        };
        x.clamp(lo as f64, hi as f64)
    };
    let (x0, x1) = (at(s_lo), at(s_hi));
    let (x0, x1) = (x0.min(x1), x0.max(x1));
    let fmt = |v: f64| units::fmt_authored(dest, v as f32);
    let swing = match unit {
        DepthUnit::Oct => format!("{:.2} oct", d.abs()),
        DepthUnit::Db => format!("{:.1} dB", d.abs()),
        DepthUnit::St => format!("{:.2} st", d.abs()),
        DepthUnit::Cents => format!("{:.1}¢", d.abs()),
        DepthUnit::S => format!("{:.2} s", d.abs()),
        DepthUnit::Ms => format!("{:.0} ms", d.abs()),
        DepthUnit::Hz => units::fmt_hz(d.abs() as f32),
        DepthUnit::X => format!("{:.2}×", d.abs()),
        DepthUnit::Plain => format!("{:.3}", d.abs()),
    };
    let sign = if s_lo < 0.0 { "±" } else if d < 0.0 { "−" } else { "+" };
    if s_lo == s_hi {
        // A constant source (a held step) doesn't swing: it offsets.
        return reading(format!("**{sign}{swing}** on `{leaf}`: holds it at {} (from {})", fmt(x0), fmt(base)), format!("→{}", fmt(x0)));
    }
    reading(
        format!("**{sign}{swing}** on `{leaf}`: {} to {} {around} {} at full swing", fmt(x0), fmt(x1), fmt(base)),
        format!("{}…{}", fmt(x0), fmt(x1)),
    )
}

/// The values a control source sweeps: an LFO −1..1 (0..1 when `unipolar`), a control `step_seq` the
/// span of its step values.
fn source_range(a: &Analysis, source: &str) -> (f64, f64) {
    let Some(m) = a.node(&format!("modulators.{source}")) else { return (-1.0, 1.0) };
    match m.get("type").and_then(Node::as_str) {
        Some("lfo") if m.get("polarity").and_then(Node::as_str) == Some("unipolar") => (0.0, 1.0),
        Some("step_seq") => {
            let mut vals: Vec<f64> = m.get("steps").map(Node::items).unwrap_or_default().iter().filter_map(|s| s.node.get("value").and_then(Node::as_f64)).collect();
            vals.extend(m.get("entry_value").and_then(Node::as_f64));
            let lo = vals.iter().copied().fold(f64::INFINITY, f64::min);
            let hi = vals.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            if lo.is_finite() { (lo, hi) } else { (-1.0, 1.0) }
        }
        _ => (-1.0, 1.0),
    }
}

/// The route a depth node belongs to: `(dest, depth key, source)` for `matrix.<id>.<key>` or a song
/// layer's `song.matrices.<layer>.<id>.<key>` (a patch takes its dest and source from the base route).
pub fn route_of(a: &Analysis, keys: &[&str]) -> Option<(String, String, Option<String>)> {
    let (entry, key) = match keys {
        ["matrix", id, key] => (format!("matrix.{id}"), *key),
        ["song", "matrices", layer, id, key] => (format!("song.matrices.{layer}.{id}"), *key),
        _ => return None,
    };
    DepthUnit::from_key(key)?;
    let e = a.node(&entry)?;
    let id = entry.rsplit('.').next()?;
    let base = a.node(&format!("matrix.{id}"));
    let pick = |k: &str| e.get(k).or_else(|| base.and_then(|b| b.get(k))).and_then(Node::as_str).map(str::to_string);
    Some((pick("dest")?, key.to_string(), pick("source")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"{
  schema_version: "0.9",
  transport: { bpm: 60, master_steps: 4 },
  harmony: { root: "A", mode: "aeolian" },
  global: { master_gain_db: -6.02 },
  instruments: {
    pad: { type: "dual", note_engine: { polyphony: 2 }, params: { level_db: 0, cutoff_hz: 440, resonance: 0.5, drive_db: 6, fm_index: 0.3 }, sends_db: { rv: -Infinity } },
    hiss: { pitched: true, note_engine: { polyphony: 1 }, params: { source: "filtered_noise", level_db: -12.04, resonance: 0.5 } },
    quiet: { note_engine: { polyphony: 1 }, params: { wave: "sine", level_db: -Infinity } },
  },
  modulators: {
    lfo: { type: "lfo", output: "control", rate_hz: 0.05 },
    up: { type: "lfo", output: "control", rate_hz: 0.05, polarity: "unipolar" },
    unity: { type: "step_seq", output: "control", length: 1, steps: [ { step: 0, value: 1 } ] },
  },
  matrix: {
    m_cut: { source: "lfo", dest: "instruments.pad.params.cutoff_hz", kind: "mod", depth_oct: 1 },
    m_trim: { source: "unity", dest: "instruments.hiss.params.level_db", kind: "mod", depth_db: -3 },
    m_swell: { source: "up", dest: "instruments.hiss.params.cutoff_hz", kind: "mod", depth_oct: 1 },
    m_mute: { source: "lfo", dest: "instruments.quiet.params.level_db", kind: "mod", depth_db: 6 },
  },
  buses: { rv: { return_gain_db: -6.02, effects: { verb: { type: "reverb", params: {} } } } },
}"#;

    fn param(path: &str, x: f64) -> Option<Reading> {
        of_param(&Analysis::new(DOC), path, x)
    }

    #[test]
    fn a_param_reads_back_what_its_unit_doesnt_say() {
        let cut = param("instruments.pad.params.cutoff_hz", 440.0).unwrap();
        assert_eq!((cut.hover.as_str(), cut.inlay.as_str()), ("≈ **A4**, the nearest note", "≈A4"));
        assert_eq!(param("instruments.pad.params.cutoff_hz", 452.0).unwrap().inlay, "≈A4 +47¢");
        assert_eq!(param("instruments.pad.params.level_db", -6.0206).unwrap().hover, "≈ **×0.50** amplitude (half)");
        assert_eq!(param("instruments.pad.params.level_db", 0.0).unwrap().inlay, "×1.00");
        assert_eq!(param("global.master_gain_db", -7.96).unwrap().hover, "≈ **×0.40** amplitude");
        assert_eq!(param("instruments.pad.sends_db.rv", f64::NEG_INFINITY).unwrap().inlay, "silent");
        assert_eq!(param("buses.rv.return_gain_db", -20.0).unwrap().hover, "≈ **×0.10** amplitude (a tenth)");
        assert_eq!(param("instruments.pad.params.drive_db", 6.0206).unwrap().inlay, "×2.00");
        assert_eq!(param("instruments.pad.params.drive_db", 0.0).unwrap().inlay, "bypass");
        assert_eq!(param("instruments.pad.params.fm_index", 0.3).unwrap().inlay, "2.40rad");
        // resonance: the filter's Q, or the note band's peak on a pitched noise voice — both readings.
        let q = param("instruments.pad.params.resonance", 0.5).unwrap();
        assert!(q.hover.starts_with("**Q 3.35** at the filter's corner") && q.hover.contains("note band's peak") && q.inlay == "Q3.35", "{q:?}");
        let band = param("instruments.hiss.params.resonance", 0.5).unwrap();
        assert!(band.hover.starts_with("the note band's peak") && band.hover.contains("Q 3.35") && band.inlay == "Q3.35 band", "{band:?}");
        // Times, cents and the rest are written in their unit already.
        assert!(param("instruments.pad.params.attack_s", 1.5).is_none() && param("instruments.pad.params.osc_b_ratio", 2.0).is_none());
        // As the editor shows them: a written `-Infinity` reads through the CST too.
        let a = Analysis::new(DOC);
        let hints = crate::lsp::inlay::inlays(&a, crate::cst::Span::new(0, DOC.len()), &Default::default());
        let after = |needle: &str| hints.iter().find(|h| h.offset == DOC.find(needle).unwrap() + needle.len()).map(|h| h.label.clone());
        assert_eq!(after("rv: -Infinity").as_deref(), Some("silent"));
        assert_eq!(after("level_db: -Infinity").as_deref(), Some("silent"));
        assert_eq!(after("depth_db: 6").as_deref(), Some("inaudible"));
        assert_eq!(after("master_gain_db: -6.02").as_deref(), Some("×0.50"));
        // Without a key there's no note to name.
        assert!(of_param(&Analysis::new(&DOC.replace("harmony: { root: \"A\", mode: \"aeolian\" },", "")), "instruments.pad.params.cutoff_hz", 440.0).is_none());
    }

    fn depth(route: &str) -> Reading {
        let a = Analysis::new(DOC);
        let (dest, key, source) = route_of(&a, &["matrix", route, if route == "m_trim" || route == "m_mute" { "depth_db" } else { "depth_oct" }]).unwrap();
        let d = a.node(&format!("matrix.{route}.{key}")).and_then(Node::as_f64).unwrap();
        of_depth(&a, &dest, &key, d, source.as_deref()).unwrap()
    }

    #[test]
    fn a_depth_reads_as_its_swing_at_the_base() {
        // A bipolar LFO, an octave each way around the authored 440 Hz.
        let cut = depth("m_cut");
        assert_eq!(cut.hover, "**±1.00 oct** on `cutoff_hz`: 220Hz to 880Hz around 440Hz at full swing");
        assert_eq!(cut.inlay, "220Hz…880Hz");
        // A constant trim from a step_seq held at 1: an offset, not a swing.
        let trim = depth("m_trim");
        assert_eq!((trim.hover.as_str(), trim.inlay.as_str()), ("**−3.0 dB** on `level_db`: holds it at -15.0dB (from -12.0dB)", "→-15.0dB"));
        // A unipolar LFO from an unwritten cutoff: up from the param's default.
        let swell = depth("m_swell");
        assert!(swell.hover.starts_with("**+1.00 oct** on `cutoff_hz`: 2.01kHz to 4.02kHz around its default 2.01kHz"), "{}", swell.hover);
        // Into a -Infinity base: nothing to hear.
        assert_eq!(depth("m_mute").inlay, "inaudible");
        // A depth key that doesn't fit the dest reads nothing (the checker names the right one).
        let a = Analysis::new(DOC);
        assert!(of_depth(&a, "instruments.pad.params.cutoff_hz", "depth_db", 3.0, Some("lfo")).is_none());
        // A song layer's patch takes its dest and source from the base route.
        let song = DOC.replace("  buses:", "  song: { matrices: { deep: { m_cut: { depth_oct: 0.5 } } }, sequence: [ { matrix: \"deep\", loops: 1 } ] },\n  buses:");
        let a = Analysis::new(song.as_str());
        let (dest, key, source) = route_of(&a, &["song", "matrices", "deep", "m_cut", "depth_oct"]).unwrap();
        assert_eq!((dest.as_str(), key.as_str(), source.as_deref()), ("instruments.pad.params.cutoff_hz", "depth_oct", Some("lfo")));
        assert_eq!(of_depth(&a, &dest, &key, 0.5, source.as_deref()).unwrap().inlay, "311Hz…622Hz");
    }
}
