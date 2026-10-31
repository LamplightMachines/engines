//! Categorical dither candidates (CE-R8, categorical A/B).
//!
//! The controller's drift walks numeric params in small steps. Some musically interesting
//! **timbre** levers are *not* numeric — an oscillator `wave`, a modal `material`, a modal
//! `num_modes`. Drift can't step those, so the controller occasionally flips one to another of its
//! values instead (a categorical move). This module only *enumerates* the discrete option set per leaf; the controller
//! does the seeded pick and the guardrails, and records the move (see [`CategoricalMove`]).
//!
//! **The axis seam (ADR 0017).** Categorical dither is part of the *optimize* (sound) axis, so it
//! only enumerates leaves under `instruments.*.params.*`. It deliberately does **not** touch
//! `instruments.*.enabled` (authored), nor any `modulators.*` leaf — note content, the LFO
//! `wave`, a euclid `generator`'s `pulses`/`rotation`, and a `pitch_walk`'s `order` all belong to
//! the *movement* (Orrery) axis, which writes them as deliberate arrangement moves.
//!
//! Pure and deterministic (no I/O, no RNG) — the candidate list is a function of the doc.

use crate::loop_doc::LoopDoc;
use crate::params::{ParamKind, PARAMS};
use serde::{Deserialize, Serialize};
use crate::doc_value::DocValue;

/// A categorical move the controller applied this loop: `path` flipped from `from` (the operating
/// point's value) to `to` (played). Recorded in the per-loop
/// [`LoopRecord`](crate::telemetry::LoopRecord) stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CategoricalMove {
    pub path: String,
    pub from: String,
    pub to: String,
}
/// A categorical dither candidate: a JSONPath leaf and its full discrete option set
/// (always ≥2 distinct values, current value included).
#[derive(Clone, Debug, PartialEq)]
pub struct CatCandidate {
    pub path: String,
    pub options: Vec<DocValue>,
}

/// Human-readable label for a categorical value (for a move's `from`/`to`): bare string for
/// strings, else the compact JSON (`true`, `9`, …).
pub fn value_label(v: &DocValue) -> String {
    match v {
        DocValue::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn strings(opts: &[&str]) -> Vec<DocValue> {
    opts.iter().map(|s| DocValue::from(*s)).collect()
}

/// Distinct integer option set around `cur` (the ±1 neighbours and `cur` itself), clamped to
/// `[lo, hi]`. Returns `None` if fewer than 2 distinct values fit (nothing to flip to).
fn int_neighbors(cur: i64, lo: i64, hi: i64) -> Option<Vec<DocValue>> {
    let mut v: Vec<i64> = [cur - 1, cur, cur + 1].into_iter().filter(|&n| n >= lo && n <= hi).collect();
    v.dedup();
    if v.len() < 2 {
        None
    } else {
        Some(v.into_iter().map(DocValue::from).collect())
    }
}

/// All categorical dither candidates for `doc`, sorted by path (deterministic). Each entry has
/// ≥2 options so there is always an alternative to the current value.
pub fn categorical_candidates(doc: &LoopDoc) -> Vec<CatCandidate> {
    let mut out = Vec::new();

    // Instruments (the sound axis): pitched `wave` + dual/modal timbre levers. `enabled` is NOT a
    // categorical lever (ADR 0017 — Orrery owns voice presence via modulator sequences, not muting).
    for (name, inst) in &doc.instruments {
        if inst.dither_disable {
            continue; // pinned instrument — exclude its `wave`/timbre levers from categorical dither
        }
        if inst.is_midi() {
            continue; // midi translator (ADR 0026): the external patch is the author's, as for drift
        }
        // The discrete lever set comes from the param registry (crate::params) — one row per
        // lever, present-in-doc gated here. `type` itself is never a categorical lever — it
        // changes per-voice state shape (a topology change, not a perturbation).
        for spec in PARAMS {
            match spec.kind {
                ParamKind::Continuous | ParamKind::Fixed => {}
                ParamKind::Categorical(opts) => {
                    if inst.string(spec.leaf).is_some() {
                        out.push(CatCandidate {
                            path: format!("instruments.{name}.params.{}", spec.leaf),
                            options: strings(opts),
                        });
                    }
                }
                ParamKind::IntSteps { lo, hi } => {
                    if let Some(n) = inst.native(spec.leaf) {
                        if let Some(opts) = int_neighbors(n.round() as i64, lo, hi) {
                            out.push(CatCandidate { path: format!("instruments.{name}.params.{}", spec.leaf), options: opts });
                        }
                    }
                }
            }
        }
    }

    // Modulators are the Orrery axis (note content, LFO wave, euclid generator, pitch_walk order) —
    // never categorical-dithered here (ADR 0017). Orrery writes them as deliberate arrangement moves.

    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{MATERIAL_OPTIONS, WAVE_OPTIONS};

    #[test]
    fn wave_options_map_one_to_one_onto_dsp_wave() {
        // A typo'd option would fall through Wave::from_name's `_ => Sine` fallback and
        // collapse onto "sine"'s variant — distinctness catches it.
        use crate::dsp::Wave;
        let parsed: Vec<Wave> = WAVE_OPTIONS.iter().map(|s| Wave::from_name(s)).collect();
        for (i, a) in parsed.iter().enumerate() {
            assert!(!matches!(a, Wave::Noise), "{}: pitched waves only", WAVE_OPTIONS[i]);
            for (j, b) in parsed.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "{} and {} parse to the same Wave", WAVE_OPTIONS[i], WAVE_OPTIONS[j]);
                }
            }
        }
    }

    #[test]
    fn material_options_map_one_to_one_onto_presets() {
        // material_partials falls back to marimba on an unknown string — pairwise-distinct
        // preset tables prove every option is recognized (a typo would duplicate marimba's).
        let tables: Vec<*const _> = MATERIAL_OPTIONS.iter().map(|m| crate::synth::material_partials(m).as_ptr()).collect();
        for i in 0..tables.len() {
            for j in 0..tables.len() {
                if i != j {
                    assert_ne!(tables[i], tables[j], "{} and {} resolve to the same preset", MATERIAL_OPTIONS[i], MATERIAL_OPTIONS[j]);
                }
            }
        }
    }

    fn dorian() -> LoopDoc {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../lab/tests/fixtures/loop_orrery_dorian.json");
        LoopDoc::from_json_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn paths(doc: &LoopDoc) -> Vec<String> {
        categorical_candidates(doc).into_iter().map(|c| c.path).collect()
    }

    #[test]
    fn enumerates_pitched_wave_but_never_enabled() {
        let p = paths(&dorian());
        // pitched `wave` is a timbre lever; `enabled` is NOT a categorical lever — Orrery owns voice
        // presence via modulator sequences, not by muting instruments (ADR 0017).
        assert!(p.contains(&"instruments.poly.params.wave".to_string()));
        assert!(!p.iter().any(|x| x.ends_with(".enabled")), "enabled is off the sound axis: {p:?}");
        // noise instrument has `source` (single option) → no `wave` candidate.
        assert!(!p.contains(&"instruments.rhythmic.params.wave".to_string()));
        assert!(!p.iter().any(|x| x.ends_with(".params.source")));
    }

    #[test]
    fn modulators_are_never_categorical_levers() {
        // The whole `modulators.*` namespace is the Orrery (movement) axis — LFO wave, euclid
        // generator pulses/rotation, pitch_walk order, note steps — so none appear as categorical
        // (sound-axis) candidates (ADR 0017 seam).
        let p = paths(&dorian());
        assert!(!p.iter().any(|x| x.starts_with("modulators.")), "modulators are off the sound axis: {p:?}");
    }

    #[test]
    fn phrase_content_is_never_a_dither_target() {
        // ADR 0028 extends the seam: sequence pools + the phrase arm are note content
        // (Orrery-owned), so a phrase-bearing doc yields no sequence_pools.* / *.phrase.*
        // candidates on the sound axis — categorical or numeric.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../lab/tests/fixtures/loop_phrase_pools.json");
        let doc = LoopDoc::from_json_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let cat = paths(&doc);
        let num = crate::controller::dither_candidates(&doc);
        for p in cat.iter().chain(num.iter()) {
            assert!(!p.starts_with("sequence_pools."), "pool content dithered: {p}");
            assert!(!p.contains(".phrase"), "phrase arm dithered: {p}");
            assert!(!p.starts_with("modulators."), "modulators are off the sound axis: {p}");
        }
    }

    #[test]
    fn every_candidate_has_at_least_two_options_and_includes_current() {
        let doc = dorian();
        for c in categorical_candidates(&doc) {
            assert!(c.options.len() >= 2, "{} has <2 options", c.path);
            let cur = doc.get_value(&c.path).unwrap();
            assert!(c.options.contains(&cur), "{} options must include current {cur}", c.path);
        }
    }

    #[test]
    fn is_deterministic() {
        let doc = dorian();
        assert_eq!(categorical_candidates(&doc), categorical_candidates(&doc));
    }

    // Dual-osc waves + modal material/num_modes are categorical levers; `type` never is (it changes
    // per-voice state shape — a topology change, ADR 0009).
    #[test]
    fn enumerates_synth_type_levers_never_type() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../lab/tests/fixtures/");
        let load = |n: &str| LoopDoc::from_json_str(&std::fs::read_to_string(format!("{dir}{n}")).unwrap()).unwrap();

        let dual = load("loop_dual_basic.json");
        let p = paths(&dual);
        assert!(p.contains(&"instruments.dual.params.osc_a_wave".to_string()), "{p:?}");
        assert!(p.contains(&"instruments.dual.params.osc_b_wave".to_string()));

        let modal = load("loop_modal_marimba.json");
        let pm = paths(&modal);
        assert!(pm.contains(&"instruments.mallet.params.material".to_string()), "{pm:?}");
        assert!(pm.contains(&"instruments.mallet.params.num_modes".to_string()));
        // num_modes (3) → adjacent ints {2,3,4}
        let nm = categorical_candidates(&modal)
            .into_iter()
            .find(|c| c.path == "instruments.mallet.params.num_modes")
            .unwrap();
        let nums: Vec<i64> = nm.options.iter().map(|v| v.as_i64().unwrap()).collect();
        assert_eq!(nums, vec![2, 3, 4]);

        for x in p.iter().chain(pm.iter()) {
            assert!(!x.ends_with(".type"), "`type` must never be a categorical lever: {x}");
        }
    }

    #[test]
    fn dither_disable_excludes_an_instrument() {
        let mut doc = dorian();
        doc.instruments.get_mut("poly").unwrap().dither_disable = true;
        let p = paths(&doc);
        assert!(!p.iter().any(|x| x.starts_with("instruments.poly.")), "pinned instrument excluded: {p:?}");
        assert!(p.iter().any(|x| x.starts_with("instruments.mono.")), "other instruments still present");
    }
}
