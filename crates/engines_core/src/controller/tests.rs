use super::*;
use crate::contract::{Guardrails, HomeScope, LiveToggle, Movement, OptimizeMode, OrreryBind, Range01, SessionControl};

use crate::test_support::fixture;

#[test]
fn swap_doc_replaces_base_and_keeps_candidates() {
    // LiveEdit same-id swap (ADR 0024): the operating point becomes the new doc (clamped to safe
    // bounds) and the candidate set is recomputed; a same-shape edit keeps the candidate count.
    let cfg = DitherConfig { max_change_per_loop: 0.05, seed: 1, cat_change_prob: 0.0, ..Default::default() };
    let mut c = Controller::new(fixture(), cfg);
    let n_before = c.candidates().len();
    let d2 = fixture().with_param("global.master_gain_db", -4.44);
    c.swap_doc(d2);
    assert!((c.operating_point().get_param("global.master_gain_db").unwrap() - -4.44).abs() < 1e-6);
    assert_eq!(c.candidates().len(), n_before, "same-shape doc keeps the candidate set");
}

#[test]
fn swap_doc_restarts_the_drift_streak_only_when_the_candidates_change() {
    // The streak holds an index into the sorted candidates: a value edit keeps it, an edit that adds
    // a numeric param (shifting every index after it) re-picks instead of drifting the wrong param.
    let cfg = DitherConfig { max_change_per_loop: 0.05, seed: 3, cat_change_prob: 0.0, ..Default::default() };
    let mut c = Controller::new(fixture(), cfg);
    let p = c.next(0);
    c.rebase(p.doc);
    assert!(c.drift_streak.is_some());
    c.swap_doc(fixture().with_param("global.master_gain_db", -4.44));
    assert!(c.drift_streak.is_some(), "a value edit keeps the streak");
    let added = fixture().with_param("instruments.poly.params.drive_db", 8.94);
    assert_ne!(dither_candidates(&added), dither_candidates(&fixture()));
    c.swap_doc(added);
    assert!(c.drift_streak.is_none(), "a new candidate list restarts it");
}

#[test]
fn live_edit_mode_produces_no_perturbation() {
    // LiveEdit freezes the operating point like Hold — `next` returns no move (empty path).
    let cfg = DitherConfig { max_change_per_loop: 0.1, seed: 9, cat_change_prob: 0.5, ..Default::default() };
    let mut c = Controller::new(fixture(), cfg);
    c.set_control(SessionControl::from_flags("live_edit", false, &[], &[], 0.05).unwrap());
    let p = c.next(7);
    assert!(p.path.is_empty(), "LiveEdit must not pick a param to perturb");
    assert_eq!(p.delta, 0.0);
    assert!(p.categorical_move.is_none(), "LiveEdit must not emit categorical moves");
}

#[test]
fn candidates_are_continuous_params_not_strings() {
    let c = dither_candidates(&fixture());
    assert!(c.contains(&"instruments.poly.params.cutoff_hz".to_string()));
    assert!(c.contains(&"instruments.poly.params.voices_detune_cents".to_string()));
    assert!(c.contains(&"global.master_gain_db".to_string()));
    assert!(c.contains(&"matrix.m_breath_c.depth_oct".to_string()), "a depth under its own key: {c:?}");
    // no string params, no note-routing depths
    assert!(!c.iter().any(|p| p.ends_with(".wave") || p.ends_with(".source")));
    assert!(!c.iter().any(|p| p.starts_with("matrix.m_arp.")));
}

#[test]
fn numeric_x_annotations_are_never_drift_candidates() {
    // `x_*` keys are free annotations (ADR 0033): a number under `params` must not move, or it
    // would reshuffle every other param's drift (the pick is an index into the sorted candidates).
    let mut d = fixture();
    d.instruments.get_mut("poly").unwrap().params.insert("x_note".into(), serde_json::json!(0.5).into());
    let c = dither_candidates(&d);
    assert!(!c.iter().any(|p| p.contains("x_note")), "{c:?}");
    assert_eq!(c, dither_candidates(&fixture()), "an annotation leaves the candidate set as it was");
}

#[test]
fn binaural_hz_params_and_their_route_depths_are_never_dither_candidates() {
    // ADR 0047: `root_hz`/`spread_hz` are author-only (`ParamKind::Fixed`) — drift must not wander
    // the beat, neither directly nor through the depth of a route into them. The pair's other
    // params drift like any subtractive voice's.
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../lab/tests/fixtures/loop_binaural_basic.json");
    let mut d = LoopDoc::from_json_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    d.instruments.get_mut("bin").unwrap().params.insert("root_hz".into(), serde_json::json!(110.0).into());
    let c = dither_candidates(&d);
    assert!(!c.iter().any(|p| p.ends_with(".root_hz") || p.ends_with(".spread_hz")), "{c:?}");
    assert!(!c.contains(&"matrix.m_beat.depth".to_string()), "the depth into spread_hz moves the beat too: {c:?}");
    assert!(c.contains(&"instruments.bin.params.cutoff_hz".to_string()) && c.contains(&"instruments.bin.params.level_db".to_string()), "{c:?}");
}

#[test]
fn bus_params_are_dither_candidates_except_width() {
    // The fixture has no `buses`, so the default reverb bus `rv` is synthesized.
    let c = dither_candidates(&fixture());
    assert!(c.contains(&"buses.rv.effects.verb.params.room".to_string()), "{c:?}");
    assert!(c.contains(&"buses.rv.effects.verb.params.shimmer".to_string()));
    assert!(c.contains(&"buses.rv.effects.verb.params.predelay_ms".to_string()));
    assert!(c.contains(&"buses.rv.return_gain_db".to_string()));
    // `width` is build-time-fixed → never dithered.
    assert!(!c.contains(&"buses.rv.effects.verb.params.width".to_string()), "width must not be dithered: {c:?}");
}

fn load_fixture(name: &str) -> LoopDoc {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../lab/tests/fixtures/");
    LoopDoc::from_json_str(&std::fs::read_to_string(format!("{dir}{name}")).unwrap()).unwrap()
}

// Dual-osc numeric params (incl. native-ranged `osc_b_ratio`) are dither candidates; strings and
// `type` are not (ADR 0009).
#[test]
fn dual_synth_numeric_params_are_dither_candidates() {
    let c = dither_candidates(&load_fixture("loop_dual_basic.json"));
    for p in ["fm_index", "fm_feedback", "osc_a_level_db", "osc_b_level_db", "osc_b_ratio", "osc_b_detune_cents"] {
        assert!(c.contains(&format!("instruments.dual.params.{p}")), "{p} should be a candidate: {c:?}");
    }
    assert!(!c.iter().any(|x| x.ends_with(".osc_a_wave") || x.ends_with(".osc_b_wave") || x.ends_with(".type")));
}

// Modal continuous params are candidates; `num_modes` (integer, outside 0..1) is NOT a numeric
// candidate — it is a categorical lever instead.
#[test]
fn modal_continuous_params_are_candidates_but_num_modes_is_not() {
    let c = dither_candidates(&load_fixture("loop_modal_marimba.json"));
    for p in ["t60_s", "brightness", "strike_pos", "stretch"] {
        assert!(c.contains(&format!("instruments.mallet.params.{p}")), "{p} should be a candidate: {c:?}");
    }
    assert!(!c.contains(&"instruments.mallet.params.num_modes".to_string()), "num_modes is categorical: {c:?}");
    assert!(!c.iter().any(|x| x.ends_with(".material") || x.ends_with(".type")));
}

/// The v0.7 fixture with `shift`/`shift_mix` added to poly's params.
fn fixture_with_shift() -> LoopDoc {
    let mut d = fixture();
    d = d.with_param("instruments.poly.params.shift_st", 0.0);
    d = d.with_param("instruments.poly.params.shift_mix", 0.5);
    d
}

#[test]
fn shift_is_a_dither_candidate() {
    let c = dither_candidates(&fixture_with_shift());
    assert!(c.contains(&"instruments.poly.params.shift_st".to_string()), "wide-range shift should be a candidate: {c:?}");
    assert!(c.contains(&"instruments.poly.params.shift_mix".to_string()));
}

#[test]
fn drift_on_shift_steps_in_semitones() {
    // force the pick onto shift by making it the only candidate
    let mut ctl = Controller::new(fixture_with_shift(), DitherConfig { max_change_per_loop: 0.05, seed: 7, cat_change_prob: 0.0, ..Default::default() });
    ctl.candidates = vec!["instruments.poly.params.shift_st".into()];
    let p = ctl.next(1);
    assert_eq!(p.path, "instruments.poly.params.shift_st");
    let base = ctl.operating_point().get_param(&p.path).unwrap(); // 0.0 semitones
    let played = p.doc.get_param(&p.path).unwrap();
    // δ is reported in native (semitone) units.
    assert!((played - (base + p.delta)).abs() < 1e-4, "played = base + δ (semitones)");
    // a 0.05 normalized step on a ±24 range is ~2.4 semitones — far bigger than a 0..1 δ.
    assert!(p.delta.abs() > 0.5, "normalized δ maps to a real semitone step: {}", p.delta);
    assert!(p.delta.abs() <= 0.05 * 48.0 + 1e-4, "δ bounded by max_change in normalized space");
}

#[test]
fn drift_pick_plays_base_plus_delta_stamped_with_the_loop_id() {
    let mut ctl = Controller::new(fixture(), DitherConfig { max_change_per_loop: 0.05, seed: 7, cat_change_prob: 0.0, ..Default::default() });
    let p = ctl.next(413);
    assert!(!p.path.is_empty());
    let base = ctl.operating_point().get_param(&p.path).unwrap();
    assert!((p.doc.get_param(&p.path).unwrap() - (base + p.delta)).abs() < 1e-6);
    assert_eq!(p.doc.loop_id, 413);
}

#[test]
fn dither_is_deterministic_and_bounded() {
    let cfg = DitherConfig { max_change_per_loop: 0.05, seed: 7, cat_change_prob: 0.0, ..Default::default() };
    let a = Controller::new(fixture(), cfg).next(100);
    let b = Controller::new(fixture(), cfg).next(100);
    assert_eq!(a.path, b.path);
    assert_eq!(a.delta, b.delta, "same seed+loop_id → same dither");
    // δ is native (Hz, dB, …); the cap holds in the controller's normalized space.
    let base = fixture();
    let step = (a.doc.get_param_norm(&a.path).unwrap() - base.get_param_norm(&a.path).unwrap()).abs();
    assert!(step <= 0.05 + 1e-6, "δ bounded by max_change_per_loop: {} moved {step}", a.path);
    let c = Controller::new(fixture(), cfg).next(101);
    assert!(c.path != a.path || (c.delta - a.delta).abs() > 1e-9);
}

#[test]
fn rebase_moves_operating_point() {
    let mut ctl = Controller::new(fixture(), DitherConfig::default());
    let p = ctl.next(1);
    ctl.rebase(p.doc.clone());
    let moved = ctl.operating_point().get_param(&p.path).unwrap();
    assert!((moved - p.doc.get_param(&p.path).unwrap()).abs() < 1e-6);
}

#[test]
fn guardrail_ceiling_bounds_the_drift_walk() {
    let cut = "instruments.poly.params.cutoff_hz";
    let mut ctl = Controller::new(fixture(), DitherConfig { max_change_per_loop: 0.2, seed: 3, cat_change_prob: 0.0, ..Default::default() });
    ctl.candidates = vec![cut.into()];
    let base = ctl.operating_point().get_param(cut).unwrap();
    let mut g = Guardrails::default();
    // a ceiling just above base, far closer than one max_change step (0.2)
    g.ceilings.insert(cut.into(), base + 0.03);
    ctl.set_control(Some(SessionControl { guardrails: g, ..Default::default() }));
    let mut seen = Vec::new();
    for i in 0..60u64 {
        let p = ctl.next(i);
        assert_eq!(p.path, cut);
        ctl.rebase(p.doc);
        seen.push(ctl.operating_point().get_param(cut).unwrap());
    }
    assert!(seen.iter().all(|&v| v <= base + 0.03 + 1e-6), "never above the ceiling: {seen:?}");
    assert!(seen.iter().any(|&v| v < base - 0.05), "the walk bounces off the ceiling and travels down: {seen:?}");
}

/// ADR 0054 P12: a guardrail is in the param's native units wherever it comes from. An objectives
/// file's `shift_st` ceiling of 12 semitones once read as normalized 12 (no ceiling at all), while the
/// same `--ceiling` flag meant 12 st.
#[test]
fn guardrails_are_native_from_a_file_or_a_flag() {
    let path = "instruments.poly.params.shift_st";
    let flags = SessionControl::from_flags("drift", false, &[format!("{path}=12")], &[format!("{path}=-12")], 0.05).unwrap().unwrap();
    let file = SessionControl::from_json_str(&format!(r#"{{ guardrails: {{ max_change_per_loop: 0.05, ceilings: {{ "{path}": 12 }}, floors: {{ "{path}": -12 }} }} }}"#)).unwrap();
    for control in [flags, file] {
        let mut ctl = Controller::new(fixture_with_shift(), DitherConfig::default());
        ctl.set_control(Some(control));
        let (lo, hi) = ctl.bounds(path);
        assert!((lo - 0.25).abs() < 1e-6 && (hi - 0.75).abs() < 1e-6, "±12 st of the ±24 st range: {lo}..{hi}");
    }
}

// ── ADR 0032: drift reflects off band edges ──────────────────────────────

#[test]
fn reflect_step_bounces_off_both_edges() {
    let band = (0.2, 0.8);
    assert_eq!(reflect_step(0.5, 0.1, band), (0.6, None), "inside: a plain step");
    let (t, turn) = reflect_step(0.75, 0.1, band);
    assert!((t - 0.75).abs() < 1e-6 && turn == Some(-1.0), "overshoots 0.8 by 0.05 → bounces back to 0.75, heading down: {t}");
    let (t, turn) = reflect_step(0.25, -0.1, band);
    assert!((t - 0.25).abs() < 1e-6 && turn == Some(1.0), "undershoots 0.2 by 0.05 → 0.25, heading up: {t}");
    let (t, _) = reflect_step(0.79, 2.0, band);
    assert!((0.2..=0.8).contains(&t), "a step longer than the band still lands inside: {t}");
    assert_eq!(reflect_step(0.9, 0.05, band).1, Some(-1.0), "outside above → steps back in, heading down");
    assert!((reflect_step(0.9, 0.05, band).0 - 0.85).abs() < 1e-6);
    assert_eq!(reflect_step(0.5, 0.3, (0.5, 0.5)), (0.5, None), "a zero-width band pins the param");
}

#[test]
fn drift_never_sticks_at_a_band_edge() {
    // The ±δ-era symmetric clamp pinned a param forever once it reached a band edge (room = 0 both
    // ways). Authored AT its cap (safe_bounds max == value), a level must now wander below it and
    // return — never above the cap, never stuck.
    let lvl = "instruments.poly.params.level_db";
    let mut doc = fixture().with_param(lvl, 0.6);
    doc.safe_bounds.insert(lvl.into(), crate::loop_doc::ConstraintBand { min: None, max: Some(0.6) });
    let mut ctl = Controller::new(doc, DitherConfig { max_change_per_loop: 0.05, seed: 5, cat_change_prob: 0.0, ..Default::default() });
    ctl.candidates = vec![lvl.into()];
    let mut seen = Vec::new();
    for i in 0..80u64 {
        let p = ctl.next(i);
        ctl.rebase(p.doc);
        seen.push(ctl.operating_point().get_param(lvl).unwrap());
    }
    assert!(seen.iter().all(|&v| v <= 0.6 + 1e-6), "never louder than the cap");
    assert!(seen.iter().any(|&v| v < 0.55), "moves off the edge: {seen:?}");
    let distinct: std::collections::BTreeSet<i64> = seen.iter().map(|v| (v * 1e4).round() as i64).collect();
    assert!(distinct.len() > 10, "keeps moving rather than settling on an edge: {} distinct values", distinct.len());
}

#[test]
fn a_bounce_turns_the_drift_streak_around() {
    let lvl = "instruments.poly.params.level_db";
    let mut doc = fixture().with_param(lvl, 0.6);
    doc.safe_bounds.insert(lvl.into(), crate::loop_doc::ConstraintBand { min: None, max: Some(0.6) });
    let mut ctl = Controller::new(doc, DitherConfig { max_change_per_loop: 0.05, seed: 5, cat_change_prob: 0.0, ..Default::default() });
    ctl.candidates = vec![lvl.into()];
    // A streak heading UP that continues this loop (not re-picking) but comes out heading DOWN can
    // only have been turned by a bounce off the cap; its step must land below the cap.
    let mut bounces = 0;
    for i in 0..200u64 {
        let continuing_up = ctl.drift_streak.is_some_and(|s| s.sign > 0.0 && s.remaining > 0);
        let p = ctl.next(i);
        if continuing_up && ctl.drift_streak.is_some_and(|s| s.sign < 0.0) {
            bounces += 1;
            assert!(p.doc.get_param(lvl).unwrap() < 0.6, "the bounced step lands under the cap");
        }
        ctl.rebase(p.doc);
    }
    assert!(bounces > 0, "an upward streak hit the cap and turned around");
}

/// Drive `path` alone with large drift steps for `loops`, returning (min, max) of its native value.
fn walk_extent(doc: LoopDoc, path: &str, loops: u64) -> (f32, f32) {
    let mut ctl = Controller::new(doc, DitherConfig { max_change_per_loop: 0.2, seed: 1, cat_change_prob: 0.0, ..Default::default() });
    ctl.candidates = vec![path.into()];
    let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
    for i in 0..loops {
        let p = ctl.next(i);
        ctl.rebase(p.doc);
        let v = ctl.operating_point().get_param(path).unwrap();
        lo = lo.min(v);
        hi = hi.max(v);
    }
    (lo, hi)
}

#[test]
fn safe_floor_keeps_master_gain_above_silence() {
    // However far drift wanders, master_gain_db must never walk below its Tier-2 safe floor −8 dB
    // (ADR 0010 — the original bug drove master_gain to 0.0) nor above 0 dB (ADR 0054 P1).
    let mut doc = fixture().with_param("global.master_gain_db", -1.94);
    doc.max_drift = Some(1.0); // isolate the Tier-2 safe floor from the cumulative drift cap
    let (lo, hi) = walk_extent(doc, "global.master_gain_db", 200);
    assert!(lo >= -8.0 - 1e-4 && hi <= 1e-4, "master_gain_db must stay in its Tier-2 band −8..0, got {lo}..{hi}");
    assert!(hi - lo > 1.0, "the walk actually moved ({lo}..{hi})");
}

#[test]
fn safe_floor_keeps_instrument_level_above_silence() {
    // S2-h2: over a long run, drift must never walk an instrument's level to silence
    // (`s *= env * level`): it stays at or above the Tier-2 floor −26 dB, and at most unity.
    let mut doc = fixture().with_param("instruments.poly.params.level_db", -1.94);
    doc.max_drift = Some(1.0); // isolate the Tier-2 safe floor from the cumulative drift cap
    let (lo, hi) = walk_extent(doc, "instruments.poly.params.level_db", 200);
    assert!(lo >= -26.0 - 1e-4 && hi <= 1e-4, "level_db must stay in its Tier-2 band −26..0, got {lo}..{hi}");
    assert!(hi - lo > 1.0, "the walk actually moved ({lo}..{hi})");
}

#[test]
fn safe_bounds_override_tightens_a_param() {
    // A per-doc `safe_bounds` override narrows the controllable band; drift must stay inside it.
    let mut d = fixture().with_param("instruments.poly.params.cutoff_hz", 800.0);
    d.safe_bounds.insert(
        "instruments.poly.params.cutoff_hz".into(),
        crate::loop_doc::ConstraintBand { min: Some(650.0), max: Some(1000.0) },
    );
    let mut ctl = Controller::new(d, DitherConfig { max_change_per_loop: 0.5, seed: 2, cat_change_prob: 0.0, ..Default::default() });
    for i in 0..60u64 {
        let p = ctl.next(i);
        ctl.rebase(p.doc.clone());
        let c = ctl.operating_point().get_param("instruments.poly.params.cutoff_hz").unwrap();
        assert!((650.0 - 0.01..=1000.0 + 0.01).contains(&c), "cutoff stayed inside the overridden safe band: {c}");
    }
}

#[test]
fn an_authored_mute_stays_silent_in_hold_and_drift() {
    // The default level floor (−26 dB) only bounds what drift does — an authored `-Infinity` is a
    // mute (ADR 0054 P2): not raised at start, not a drift candidate, still silent after a long run.
    let lvl = "instruments.poly.params.level_db";
    let doc = fixture().with_param(lvl, f32::NEG_INFINITY);
    let mut ctl = Controller::new(doc, DitherConfig { max_change_per_loop: 0.5, seed: 4, cat_change_prob: 0.0, ..Default::default() });
    assert_eq!(ctl.operating_point().get_param(lvl), Some(f32::NEG_INFINITY), "not raised to the −26 dB default floor");
    assert!(!ctl.candidates().iter().any(|p| p == lvl), "a muted voice isn't drifted");
    for i in 0..100u64 {
        let p = ctl.next(i);
        ctl.rebase(p.doc);
    }
    assert_eq!(ctl.operating_point().get_param(lvl), Some(f32::NEG_INFINITY));
}

#[test]
fn an_authored_value_below_the_default_floor_is_kept_but_an_explicit_band_still_clamps() {
    // master_gain_db's default floor is −8 dB: an authored −14 plays at −14 and drift never goes below.
    let mg = "global.master_gain_db";
    let mut ctl = Controller::new(fixture().with_param(mg, -14.0), DitherConfig { max_change_per_loop: 0.5, seed: 6, cat_change_prob: 0.0, ..Default::default() });
    assert!((ctl.operating_point().get_param(mg).unwrap() - -14.0).abs() < 1e-4);
    ctl.candidates = vec![mg.into()];
    for i in 0..100u64 {
        let p = ctl.next(i);
        ctl.rebase(p.doc);
        assert!(ctl.operating_point().get_param(mg).unwrap() >= -14.0 - 1e-4, "never below the authored value");
    }
    // An explicit safe_bounds side is still a hard clamp (ADR 0010): min −30 lifts an authored −40…
    let lvl = "instruments.poly.params.level_db";
    let mut doc = fixture().with_param(lvl, -40.0);
    doc.safe_bounds.insert(lvl.into(), crate::loop_doc::ConstraintBand { min: Some(-30.0), max: None });
    let ctl = Controller::new(doc.clone(), DitherConfig::default());
    assert!((ctl.operating_point().get_param(lvl).unwrap() - -30.0).abs() < 1e-3);
    // …but never a silent gain: the controller doesn't move a `-Infinity` (ADR 0054 P2).
    let ctl = Controller::new(doc.with_param(lvl, f32::NEG_INFINITY), DitherConfig::default());
    assert_eq!(ctl.operating_point().get_param(lvl), Some(f32::NEG_INFINITY));
}

#[test]
fn a_one_sided_cap_below_the_default_floor_clamps_instead_of_panicking() {
    // `{max: -10}` on master_gain_db sits under its default floor −8 dB: the explicit side wins (the
    // band is [-10, -10]), so an authored −6 starts at the cap — on `new` and on `swap_doc` — instead
    // of `f32::clamp(-8, -10)` panicking.
    let mg = "global.master_gain_db";
    let capped = |v: f32| {
        let mut doc = fixture().with_param(mg, v);
        doc.safe_bounds.insert(mg.into(), crate::loop_doc::ConstraintBand { min: None, max: Some(-10.0) });
        doc
    };
    assert_eq!(capped(-6.0).safe_range(mg), (-10.0, -10.0));
    let mut ctl = Controller::new(capped(-6.0), DitherConfig { max_change_per_loop: 0.2, seed: 3, cat_change_prob: 0.0, ..Default::default() });
    assert!((ctl.operating_point().get_param(mg).unwrap() - -10.0).abs() < 1e-4);
    ctl.swap_doc(capped(-4.0));
    assert!((ctl.operating_point().get_param(mg).unwrap() - -10.0).abs() < 1e-4);
    for i in 0..20u64 {
        let p = ctl.next(i);
        ctl.rebase(p.doc);
        assert!(ctl.operating_point().get_param(mg).unwrap() <= -10.0 + 1e-4);
    }
    // The mirror image: a `min` above the default ceiling raises the ceiling to it.
    let lvl = "instruments.poly.params.level_db";
    let mut doc = fixture().with_param(lvl, -14.0);
    doc.safe_bounds.insert(lvl.into(), crate::loop_doc::ConstraintBand { min: Some(3.0), max: None });
    let (lo, hi) = doc.safe_range(lvl);
    assert!(lo <= hi, "[{lo}, {hi}]");
}

#[test]
fn a_guardrail_ceiling_under_the_safe_floor_does_not_panic_the_attractor() {
    // A CLI ceiling (−10 dB) under master_gain_db's safe floor (−8) with a violated constraint band:
    // the attractor's bounds invert, and the clamp must not panic inside `next`.
    let mg = "global.master_gain_db";
    let mut ctl = Controller::new(fixture().with_param(mg, -6.0), DitherConfig { max_change_per_loop: 0.1, seed: 5, cat_change_prob: 0.0, ..Default::default() });
    let mut control = SessionControl::from_flags("drift", false, &[format!("{mg}=-10")], &[], 0.1).unwrap().unwrap();
    control.constraints.insert(mg.into(), crate::loop_doc::ConstraintBand { min: Some(-2.0), max: Some(-1.0) });
    ctl.set_control(Some(control));
    for i in 0..10u64 {
        let p = ctl.next(i);
        ctl.rebase(p.doc);
    }
}

#[test]
fn max_drift_caps_cumulative_drift_travel() {
    // Default max_drift (0.15): over a long drift run with a large per-loop step, no candidate's
    // operating point may travel more than 0.15 (normalized) from its authored anchor (ADR 0025).
    let cfg = DitherConfig { max_change_per_loop: 0.5, seed: 7, cat_change_prob: 0.0, ..Default::default() };
    let mut ctl = Controller::new(fixture(), cfg);
    let anchors: Vec<(String, f32)> = ctl
        .candidates()
        .iter()
        .map(|p| (p.clone(), ctl.operating_point().get_param_norm(p).unwrap()))
        .collect();
    for i in 0..300u64 {
        let p = ctl.next(i);
        ctl.rebase(p.doc);
    }
    for (p, a) in &anchors {
        let v = ctl.operating_point().get_param_norm(p).unwrap();
        assert!((v - a).abs() <= DEFAULT_MAX_DRIFT + 1e-4, "{p}: travelled {} > max_drift from anchor {a}", (v - a).abs());
    }
}

#[test]
fn explicit_max_drift_in_doc_is_tighter() {
    // An authored `max_drift` of 0.05 caps travel more tightly than the 0.15 default.
    let mut doc = fixture();
    doc.max_drift = Some(0.05);
    let cfg = DitherConfig { max_change_per_loop: 0.5, seed: 3, cat_change_prob: 0.0, ..Default::default() };
    let mut ctl = Controller::new(doc, cfg);
    let anchors: Vec<(String, f32)> = ctl
        .candidates()
        .iter()
        .map(|p| (p.clone(), ctl.operating_point().get_param_norm(p).unwrap()))
        .collect();
    for i in 0..300u64 {
        let p = ctl.next(i);
        ctl.rebase(p.doc);
    }
    for (p, a) in &anchors {
        let v = ctl.operating_point().get_param_norm(p).unwrap();
        assert!((v - a).abs() <= 0.05 + 1e-4, "{p}: travelled {} > authored max_drift 0.05", (v - a).abs());
    }
}

#[test]
fn max_drift_band_clamps_at_a_nearby_safe_bound() {
    // Anchor near the top of the safe range (cutoff 0.97, safe hi 1.0): the drift band's upper side
    // is clipped to the ceiling while the lower side stays anchor − max_drift.
    let doc = fixture().with_param("instruments.poly.params.cutoff_hz", 6967.7);
    let cfg = DitherConfig { max_change_per_loop: 0.5, seed: 11, cat_change_prob: 0.0, ..Default::default() };
    let mut ctl = Controller::new(doc, cfg);
    let path = "instruments.poly.params.cutoff_hz";
    let anchor = ctl.operating_point().get_param_norm(path).unwrap();
    for i in 0..300u64 {
        let p = ctl.next(i);
        ctl.rebase(p.doc);
        let v = ctl.operating_point().get_param_norm(path).unwrap();
        assert!(v <= 1.0 + 1e-6, "never exceeds the safe ceiling: {v}");
        assert!(v >= anchor - DEFAULT_MAX_DRIFT - 1e-4, "never below anchor − max_drift: {v}");
    }
}

#[test]
fn objectives_constraint_replaces_max_drift_even_when_wider() {
    // A param-specific constraint band [−7, −1] dB is WIDER than ±max_drift around the anchor −4
    // dB (±0.15 × the gain's drift scale 0.3 of the 66 dB span ≈ ±3 dB): the constraint replaces the
    // max_drift band (ADR 0025).
    let path = "global.master_gain_db";
    let doc = fixture().with_param(path, -4.0);
    let mut ctl = Controller::new(doc, DitherConfig { max_change_per_loop: 0.2, seed: 1, cat_change_prob: 0.0, ..Default::default() });
    let (lo, hi) = ctl.drift_bounds(path);
    let (anchor, d) = (crate::params::GAIN.inverse(-4.0), DEFAULT_MAX_DRIFT * 0.3);
    assert!((lo - (anchor - d)).abs() < 1e-5 && (hi - (anchor + d)).abs() < 1e-5, "default band = anchor ± max_drift × 0.3: [{lo}, {hi}]");
    assert!(((hi - lo) * 66.0 / 2.0 - 2.97).abs() < 0.01, "≈ ±3 dB");
    let mut control = SessionControl::default();
    control.constraints.insert(path.into(), crate::loop_doc::ConstraintBand { min: Some(-7.0), max: Some(-1.0) });
    ctl.set_control(Some(control));
    let (lo, hi) = ctl.drift_bounds(path);
    let (want_lo, want_hi) = (crate::loop_doc::param_norm(path, -7.0), crate::loop_doc::param_norm(path, -1.0));
    assert!((lo - want_lo).abs() < 1e-6 && (hi - want_hi).abs() < 1e-6, "the constraint band replaces it: [{lo}, {hi}]");
}

#[test]
fn override_reanchors_the_drift_band() {
    // An override sets cutoff far (0.9) from its authored anchor (0.5) — beyond ±max_drift. The
    // override wins: the drift band re-centers on 0.9, so drift explores [0.75, 1.0] around it
    // rather than freezing it (ADR 0025). Without re-anchoring the param would be pinned at 0.9.
    let doc = fixture().with_param("instruments.poly.params.cutoff_hz", 800.0);
    let cfg = DitherConfig { max_change_per_loop: 0.5, seed: 5, cat_change_prob: 0.0, ..Default::default() };
    let mut ctl = Controller::new(doc, cfg);
    let mut control = SessionControl::default();
    control.overrides.insert("instruments.poly.params.cutoff_hz".into(), serde_json::json!(5047.7).into());
    ctl.set_control(Some(control));
    let path = "instruments.poly.params.cutoff_hz";
    assert!((ctl.operating_point().get_param_norm(path).unwrap() - 0.9).abs() < 1e-5, "override applied");
    let mut min_c = f32::INFINITY;
    for i in 0..300u64 {
        let p = ctl.next(i);
        ctl.rebase(p.doc);
        let c = ctl.operating_point().get_param_norm(path).unwrap();
        min_c = min_c.min(c);
        assert!(c >= 0.9 - DEFAULT_MAX_DRIFT - 1e-4 && c <= 1.0 + 1e-6, "stays within ±max_drift of the override: {c}");
    }
    assert!(min_c < 0.9 - 1e-3, "drift explored below the override (band re-centered, not pinned): {min_c}");
}

#[test]
fn swap_doc_reanchors_the_drift_band() {
    // A live doc-swap re-centers the drift band on the newly authored value (cutoff 0.5 → 0.9).
    let cfg = DitherConfig { max_change_per_loop: 0.5, seed: 6, cat_change_prob: 0.0, ..Default::default() };
    let mut ctl = Controller::new(fixture().with_param("instruments.poly.params.cutoff_hz", 800.0), cfg);
    ctl.swap_doc(fixture().with_param("instruments.poly.params.cutoff_hz", 5047.7));
    let path = "instruments.poly.params.cutoff_hz";
    let mut min_c = f32::INFINITY;
    for i in 0..300u64 {
        let p = ctl.next(i);
        ctl.rebase(p.doc);
        let c = ctl.operating_point().get_param_norm(path).unwrap();
        min_c = min_c.min(c);
        assert!(c >= 0.9 - DEFAULT_MAX_DRIFT - 1e-4, "drift band re-centered on the swapped value: {c}");
    }
    assert!(min_c < 0.9 - 1e-3, "drift explored around the swapped anchor (not the old one): {min_c}");
}

#[test]
fn constraint_attractor_drives_param_into_band() {
    // master_gain_db starts at −0.92; a constraint band [−6, −4.4] is below it ⇒ the controller must
    // drive it DOWN toward the band (priority over drift) each loop, then release once inside.
    let doc = fixture().with_param("global.master_gain_db", -0.92);
    let mut ctl = Controller::new(doc, DitherConfig { max_change_per_loop: 0.1, seed: 4, cat_change_prob: 0.0, ..Default::default() });
    let mut control = SessionControl::default();
    control.constraints.insert("global.master_gain_db".into(), crate::loop_doc::ConstraintBand { min: Some(-6.0), max: Some(-4.4) });
    ctl.set_control(Some(control));
    let mut prev = ctl.operating_point().get_param("global.master_gain_db").unwrap();
    let mut reached = false;
    for i in 0..20u64 {
        let g = ctl.operating_point().get_param("global.master_gain_db").unwrap();
        if g > -4.4 + 1e-4 {
            let p = ctl.next(i);
            assert_eq!(p.path, "global.master_gain_db", "constraint preempts drift while out of band (loop {i})");
            assert!(p.delta < 0.0, "drives master_gain_db down (loop {i}): δ={}", p.delta);
            ctl.rebase(p.doc);
            let now = ctl.operating_point().get_param("global.master_gain_db").unwrap();
            assert!(now < prev, "monotonic approach toward the band");
            prev = now;
        } else {
            reached = true;
            break;
        }
    }
    assert!(reached, "master_gain_db reached its constraint band");
    assert!(ctl.operating_point().get_param("global.master_gain_db").unwrap() <= -4.4 + 1e-4);
}

#[test]
fn constraint_attractor_drives_instrument_param_into_band() {
    // The Tier-3 attractor must drive INSTRUMENT params (not just globals) into their band in
    // drift — `numeric_pair`/`get_param_norm` navigate the doc by JSONPath, so the namespace is
    // immaterial. Regression guard for "a constraint on an instrument param does nothing".
    let doc = fixture().with_param("instruments.poly.params.cutoff_hz", 5047.7);
    let mut ctl = Controller::new(doc, DitherConfig { max_change_per_loop: 0.1, seed: 4, cat_change_prob: 0.0, ..Default::default() });
    let mut control = SessionControl::default();
    control.constraints.insert("instruments.poly.params.cutoff_hz".into(), crate::loop_doc::ConstraintBand { min: Some(318.5), max: Some(800.0) });
    ctl.set_control(Some(control));
    let mut reached = false;
    for i in 0..30u64 {
        let c = ctl.operating_point().get_param("instruments.poly.params.cutoff_hz").unwrap();
        if c > 800.0 + 0.01 {
            let p = ctl.next(i);
            assert_eq!(p.path, "instruments.poly.params.cutoff_hz", "constraint preempts drift while out of band (loop {i})");
            assert!(p.delta < 0.0, "drives the instrument cutoff down (loop {i})");
            ctl.rebase(p.doc);
        } else {
            reached = true;
            break;
        }
    }
    assert!(reached, "instrument cutoff reached its constraint band");
    assert!(ctl.operating_point().get_param("instruments.poly.params.cutoff_hz").unwrap() <= 800.0 + 0.01);
}

#[test]
fn constraint_attractor_enforces_in_live_edit() {
    // ADR 0024 (amended): a Tier-3 constraint is an explicit guardrail, so it's enforced even in
    // LiveEdit — the engine drives the param into the band while the user hand-authors.
    let doc = fixture().with_param("instruments.poly.params.cutoff_hz", 5047.7);
    let mut ctl = Controller::new(doc, DitherConfig { max_change_per_loop: 0.1, seed: 4, cat_change_prob: 0.0, ..Default::default() });
    let mut control = SessionControl::from_flags("live_edit", false, &[], &[], 0.1).unwrap().expect("live_edit control");
    control.constraints.insert("instruments.poly.params.cutoff_hz".into(), crate::loop_doc::ConstraintBand { min: Some(318.5), max: Some(800.0) });
    ctl.set_control(Some(control));
    let mut reached = false;
    for i in 0..30u64 {
        let c = ctl.operating_point().get_param("instruments.poly.params.cutoff_hz").unwrap();
        if c > 800.0 + 0.01 {
            let p = ctl.next(i);
            assert_eq!(p.path, "instruments.poly.params.cutoff_hz", "constraint moves the param even in LiveEdit (loop {i})");
            ctl.rebase(p.doc);
        } else {
            reached = true;
            break;
        }
    }
    assert!(reached, "cutoff reached its band in LiveEdit");
}

#[test]
fn hold_stays_frozen_even_with_a_constraint() {
    // Pure Hold remains a full freeze — a constraint does NOT move the param (unlike LiveEdit).
    let doc = fixture().with_param("instruments.poly.params.cutoff_hz", 5047.7);
    let mut ctl = Controller::new(doc, DitherConfig { max_change_per_loop: 0.1, seed: 4, cat_change_prob: 0.0, ..Default::default() });
    let mut control = SessionControl::from_flags("hold", false, &[], &[], 0.1).unwrap().expect("hold control");
    control.constraints.insert("instruments.poly.params.cutoff_hz".into(), crate::loop_doc::ConstraintBand { min: Some(318.5), max: Some(800.0) });
    ctl.set_control(Some(control));
    let before = ctl.operating_point().get_param("instruments.poly.params.cutoff_hz").unwrap();
    let p = ctl.next(0);
    assert!(p.path.is_empty(), "Hold produces a no-op pair even with a constraint");
    ctl.rebase(p.doc);
    let after = ctl.operating_point().get_param("instruments.poly.params.cutoff_hz").unwrap();
    assert_eq!(before, after, "Hold leaves the param frozen");
}

#[test]
fn override_injects_once_then_controller_may_move_it() {
    // `overrides` are apply-once-on-change set-points: numeric + bool leaves injected into the
    // operating point; re-applying the same value does NOT re-inject (so the controller can move it).
    let mut ctl = Controller::new(fixture(), DitherConfig { max_change_per_loop: 0.05, seed: 1, cat_change_prob: 0.0, ..Default::default() });
    let mut control = SessionControl::default();
    control.overrides.insert("instruments.poly.params.shift_st".into(), DocValue::from(-12.0));
    control.overrides.insert("instruments.mono.enabled".into(), DocValue::Bool(false));
    ctl.set_control(Some(control));
    assert_eq!(ctl.operating_point().get_param("instruments.poly.params.shift_st").unwrap(), -12.0, "numeric override injected (native units)");
    assert!(!ctl.operating_point().instruments["mono"].enabled, "bool override injected");
    // simulate the controller having moved shift, then re-apply the same control:
    let moved = ctl.operating_point().with_param("instruments.poly.params.shift_st", -10.0);
    ctl.rebase(moved);
    let mut control2 = SessionControl::default();
    control2.overrides.insert("instruments.poly.params.shift_st".into(), DocValue::from(-12.0));
    ctl.set_control(Some(control2));
    assert_eq!(ctl.operating_point().get_param("instruments.poly.params.shift_st").unwrap(), -10.0, "unchanged override does not re-inject");
}

// ── WS4.1: role-based Orrery binding ─────────────────────────────────────

#[test]
fn orrery_config_resolves_roles_over_bind_ids() {
    // A doc that tags modulators by `orrery_role` binds by role even when `bind` names something
    // else — Orrery is no longer welded to the demo ids.
    let mut doc = load_fixture("loop_orrery_dorian.json");
    doc.modulators.get_mut("chords").unwrap().orrery_role = Some("chord".into());
    doc.modulators.get_mut("bass_line").unwrap().orrery_role = Some("bass".into());
    doc.modulators.get_mut("arp_shimmer").unwrap().orrery_role = Some("arp".into());
    doc.modulators.get_mut("spark_events").unwrap().orrery_role = Some("oneshot".into());
    doc.modulators.get_mut("rhy_noise").unwrap().orrery_disable = true; // a context drone

    let mut ctl = Controller::new(doc, DitherConfig::default());
    // `bind` deliberately points the chord role at a WRONG id; the role must win.
    let control = SessionControl {
        movement: Movement { orrery: true, bind: OrreryBind { chord: "WRONG".into(), ..Default::default() }, ..Default::default() },
        ..Default::default()
    };
    ctl.set_control(Some(control));
    let cfg = ctl.orrery_config();
    assert_eq!(cfg.chord_mod, "chords", "orrery_role beats the bind id");
    assert_eq!(cfg.bass_mod, "bass_line");
    assert_eq!(cfg.arp_mod, "arp_shimmer");
    assert!(cfg.oneshot_mods.contains(&"spark_events".to_string()), "oneshot role resolved: {:?}", cfg.oneshot_mods);
    assert!(cfg.context_mods.contains(&"rhy_noise".to_string()), "context drone harvested: {:?}", cfg.context_mods);
}

#[test]
fn orrery_config_falls_back_to_bind_ids_without_roles() {
    // No `orrery_role` anywhere → the bind ids (here the demo defaults) drive, preserving the
    // back-compatible behavior on the existing seed docs.
    let mut ctl = Controller::new(load_fixture("loop_orrery_dorian.json"), DitherConfig::default());
    ctl.set_control(Some(SessionControl { movement: Movement { orrery: true, ..Default::default() }, ..Default::default() }));
    let cfg = ctl.orrery_config();
    assert_eq!(cfg.chord_mod, "chords");
    assert_eq!(cfg.bass_mod, "bass_line");
    assert_eq!(cfg.arp_mod, "arp_shimmer");
    // `bind.oneshot` default `["event"]` names an *instrument*, not a modulator ⇒ filtered out.
    assert!(cfg.oneshot_mods.is_empty(), "legacy instrument name drops out: {:?}", cfg.oneshot_mods);
}

#[test]
fn enriched_orrery_is_deterministic_across_runs() {
    // All four surfaces together (chord/bass/arp + one-shot accents + variable pools + the
    // fixed-pitch floor) must stay bit-reproducible loop-to-loop (CE-R7.3).
    let run = || {
        let mut doc = load_fixture("loop_orrery_dorian.json");
        doc.modulators.get_mut("spark_events").unwrap().orrery_role = Some("oneshot".into());
        doc.modulators.get_mut("bass_line").unwrap().orrery_disable = true; // a held drone (context)
        let mut ctl = Controller::new(doc, DitherConfig { max_change_per_loop: 0.05, seed: 11, cat_change_prob: 0.1, ..Default::default() });
        ctl.set_control(Some(SessionControl {
            movement: Movement { orrery: true, phrase_loops: 1, ..Default::default() },
            ..Default::default()
        }));
        let mut ops = Vec::new();
        for i in 0..8u64 {
            let p = ctl.next(i);
            ctl.rebase(p.doc);
            ops.push(serde_json::to_value(ctl.operating_point()).unwrap());
        }
        ops
    };
    assert_eq!(run(), run(), "enriched orrery trajectory is bit-reproducible");
}

#[test]
fn orrery_drives_all_surfaces_on_a_non_demo_role_bound_doc() {
    // A fresh doc that binds Orrery purely by `orrery_role` (non-demo ids) drives chord, bass,
    // arp, and one-shot surfaces — proving independence from the demo ids.
    let mut ctl = Controller::new(load_fixture("loop_orrery_roles.json"), DitherConfig { max_change_per_loop: 0.05, seed: 3, cat_change_prob: 0.0, ..Default::default() });
    ctl.set_control(Some(SessionControl { movement: Movement { orrery: true, phrase_loops: 1, ..Default::default() }, ..Default::default() }));
    let cfg = ctl.orrery_config();
    assert_eq!(cfg.chord_mod, "pad_voicing");
    assert_eq!(cfg.bass_mod, "sub_bass");
    assert_eq!(cfg.arp_mod, "glints");
    assert!(cfg.oneshot_mods.contains(&"chimes".to_string()), "{:?}", cfg.oneshot_mods);
    assert!(cfg.context_mods.contains(&"held_fifth".to_string()), "{:?}", cfg.context_mods);
    // advance a few phrases; the one-shot modulator's authored step is replaced by accents.
    let authored = serde_json::to_value(&ctl.operating_point().modulators["chimes"].steps).unwrap();
    for i in 0..4u64 {
        let p = ctl.next(i);
        ctl.rebase(p.doc);
    }
    let after = serde_json::to_value(&ctl.operating_point().modulators["chimes"].steps).unwrap();
    assert!(!ctl.operating_point().modulators["chimes"].steps.is_empty(), "one-shot accents written on the non-demo doc");
    assert_ne!(after, authored, "Orrery rewrote the one-shot accents (chord-driven) on the non-demo doc");
}

#[test]
fn fixed_pitch_context_harvests_drone_pitches() {
    // An `orrery_disable` note modulator is read as context; its sounding pitch-classes feed the
    // voyage's harmonic floor (WS4.2). The bass holds offset −17 from D (pc 2) → pc 9 (A).
    let mut doc = load_fixture("loop_orrery_dorian.json");
    doc.modulators.get_mut("bass_line").unwrap().orrery_disable = true;
    let mut ctl = Controller::new(doc, DitherConfig::default());
    ctl.set_control(Some(SessionControl { movement: Movement { orrery: true, ..Default::default() }, ..Default::default() }));
    let cfg = ctl.orrery_config();
    let fixed = ctl.fixed_pitch_context(&cfg);
    assert!(fixed.contains(&Pc(9)), "drone pitch-class harvested into the floor: {fixed:?}");
}

// ── WS5.2: Orrery-off reverts to the authored home + resets ──────────────

fn orrery_on() -> SessionControl {
    SessionControl { movement: Movement { orrery: true, phrase_loops: 1, ..Default::default() }, ..Default::default() }
}
fn orrery_off() -> SessionControl {
    SessionControl { movement: Movement { orrery: false, ..Default::default() }, ..Default::default() }
}

#[test]
fn orrery_drives_phrase_selection_and_off_restores_the_authored_arm() {
    // ADR 0028: over a phrase-armed arp, the voyage materializes `phrase.active` +
    // `phrase.realized` each phrase window (visible in telemetry), deterministically per
    // seed — and Orrery-off restores the authored arm (no active pin, no realized copy).
    let run = |n: u64| -> Vec<Option<String>> {
        let mut ctl = Controller::new(load_fixture("loop_orrery_phrase.json"), DitherConfig { seed: 11, ..DitherConfig::default() });
        ctl.set_control(Some(orrery_on()));
        (0..n)
            .map(|i| {
                ctl.next(100 + i);
                ctl.orrery_telemetry().unwrap().phrase_active.clone()
            })
            .collect()
    };
    let a = run(6);
    let b = run(6);
    assert_eq!(a, b, "phrase selection is deterministic per seed");
    assert!(a.iter().flatten().count() > 0, "selection lands after the first phrase boundary: {a:?}");
    let picked: std::collections::BTreeSet<&String> = a.iter().flatten().collect();
    assert!(picked.iter().all(|p| ["lib:glass_up", "lib:pedal_pulse", "turn"].contains(&p.as_str())), "{picked:?}");

    // …and the realized copy actually plays under the arp mod while the voyage runs
    let mut ctl = Controller::new(load_fixture("loop_orrery_phrase.json"), DitherConfig { seed: 11, ..DitherConfig::default() });
    ctl.set_control(Some(orrery_on()));
    for i in 0..4 {
        ctl.next(200 + i);
    }
    let ph = ctl.operating_point().modulators["arp_shimmer"].phrase.as_ref().unwrap();
    assert!(ph.active.is_some() && ph.realized.is_some());

    // Orrery-off: the authored phrase arm comes back byte-for-byte (HomeSnapshot).
    ctl.set_control(Some(orrery_off()));
    let authored = load_fixture("loop_orrery_phrase.json");
    assert_eq!(
        serde_json::to_value(&ctl.operating_point().modulators["arp_shimmer"].phrase).unwrap(),
        serde_json::to_value(&authored.modulators["arp_shimmer"].phrase).unwrap(),
        "authored arm restored: no active pin, no realized copy"
    );
}

#[test]
fn orrery_off_reverts_full_footprint_and_resets() {
    let mut doc = load_fixture("loop_orrery_dorian.json");
    doc.modulators.get_mut("spark_events").unwrap().orrery_role = Some("oneshot".into());
    // authored content == what new() snapshots as "home".
    let authored_chord = serde_json::to_value(&doc.modulators["chords"].steps).unwrap();
    let authored_bass = serde_json::to_value(&doc.modulators["bass_line"].steps).unwrap();
    let authored_arp = doc.modulators["arp_shimmer"].pitch_walk.as_ref().unwrap().pool.clone();
    let authored_spark = serde_json::to_value(&doc.modulators["spark_events"].steps).unwrap();
    let authored_event_on = doc.instruments["event"].enabled;

    let mut ctl = Controller::new(doc, DitherConfig { max_change_per_loop: 0.05, seed: 5, cat_change_prob: 0.0, ..Default::default() });
    ctl.set_control(Some(orrery_on()));
    for i in 0..6u64 {
        let p = ctl.next(i);
        ctl.rebase(p.doc);
    }
    // the voyage actually rewrote the chord (so the revert is meaningful).
    assert_ne!(serde_json::to_value(&ctl.operating_point().modulators["chords"].steps).unwrap(), authored_chord);

    ctl.set_control(Some(orrery_off()));

    let op = ctl.operating_point();
    assert_eq!(serde_json::to_value(&op.modulators["chords"].steps).unwrap(), authored_chord, "chord reverted to authored");
    assert_eq!(serde_json::to_value(&op.modulators["bass_line"].steps).unwrap(), authored_bass, "bass reverted");
    assert_eq!(op.modulators["arp_shimmer"].pitch_walk.as_ref().unwrap().pool, authored_arp, "arp pool reverted");
    assert_eq!(serde_json::to_value(&op.modulators["spark_events"].steps).unwrap(), authored_spark, "one-shot reverted");
    assert_eq!(op.instruments["event"].enabled, authored_event_on, "instrument `enabled` is never touched off-axis (ADR 0017 seam)");
    assert!(ctl.orrery_telemetry().is_none(), "Orrery state reset");
}

#[test]
fn orrery_off_when_never_on_is_a_noop() {
    // A static→static edit must not restore/clobber anything (e.g. drift progress is preserved).
    let mut ctl = Controller::new(load_fixture("loop_orrery_dorian.json"), DitherConfig { max_change_per_loop: 0.05, seed: 2, cat_change_prob: 0.0, ..Default::default() });
    ctl.set_control(Some(SessionControl::default())); // drift, static
    for i in 0..3u64 {
        let p = ctl.next(i);
        ctl.rebase(p.doc);
    }
    let before = serde_json::to_value(ctl.operating_point()).unwrap();
    ctl.set_control(Some(orrery_off()));
    assert_eq!(serde_json::to_value(ctl.operating_point()).unwrap(), before, "static→static is a no-op");
}

#[test]
fn orrery_on_off_on_is_deterministic() {
    // A scripted control timeline (on → off → on) replays bit-identically (CE-R7.3 / AC4).
    let run = || {
        let mut doc = load_fixture("loop_orrery_dorian.json");
        doc.modulators.get_mut("spark_events").unwrap().orrery_role = Some("oneshot".into());
        let mut ctl = Controller::new(doc, DitherConfig { max_change_per_loop: 0.05, seed: 9, cat_change_prob: 0.0, ..Default::default() });
        let mut ops = Vec::new();
        let step = |ctl: &mut Controller, i: u64, ops: &mut Vec<serde_json::Value>| {
            let p = ctl.next(i);
            ctl.rebase(p.doc);
            ops.push(serde_json::to_value(ctl.operating_point()).unwrap());
        };
        ctl.set_control(Some(orrery_on()));
        for i in 0..3 {
            step(&mut ctl, i, &mut ops);
        }
        ctl.set_control(Some(orrery_off()));
        for i in 3..5 {
            step(&mut ctl, i, &mut ops);
        }
        ctl.set_control(Some(orrery_on()));
        for i in 5..8 {
            step(&mut ctl, i, &mut ops);
        }
        ops
    };
    assert_eq!(run(), run(), "on→off→on trajectory is bit-reproducible");
}

// ── WS5.3: voice-change governor ─────────────────────────────────────────

#[test]
fn governor_holds_voice_changes_in_band() {
    // Per-phrase voice changes are measured from the realized chord voicing. A tight band drives
    // common-tone retention up (fewer changes); a wide band lets the voyage re-voice freely.
    let mean_changes = |band: Range01| -> f32 {
        let mut ctl = Controller::new(load_fixture("loop_orrery_dorian.json"), DitherConfig { max_change_per_loop: 0.05, seed: 4, cat_change_prob: 0.0, ..Default::default() });
        let mv = Movement { orrery: true, phrase_loops: 1, voice_changes_per_loop: Some(band), ..Default::default() };
        ctl.set_control(Some(SessionControl { movement: mv, ..Default::default() }));
        let voicing = |ctl: &Controller| -> std::collections::BTreeMap<String, i32> {
            ctl.operating_point().modulators["chords"].steps[0]
                .voicing
                .iter()
                .map(|(id, nv)| (id.clone(), nv.offset.round() as i32))
                .collect()
        };
        let mut prev = voicing(&ctl);
        let (mut total, mut phrases) = (0u32, 0u32);
        for i in 0..40u64 {
            let p = ctl.next(i);
            ctl.rebase(p.doc);
            let cur = voicing(&ctl);
            total += cur.iter().filter(|(id, off)| prev.get(*id).is_none_or(|p| p != *off)).count() as u32;
            phrases += 1;
            prev = cur;
        }
        total as f32 / phrases as f32
    };
    let tight = mean_changes(Range01 { min: 0.0, max: 1.0 });
    let wide = mean_changes(Range01 { min: 3.0, max: 4.0 });
    assert!(tight < wide, "tight band holds fewer voice changes than a wide one: tight={tight} wide={wide}");
}

// ── WS5.4: tension governor + home scope ─────────────────────────────────

#[test]
fn movement_tension_target_anchors_the_preference() {
    let target_of = |t: f32| {
        let mut ctl = Controller::new(load_fixture("loop_orrery_dorian.json"), DitherConfig::default());
        ctl.set_control(Some(SessionControl { movement: Movement { orrery: true, tension_target: t, ..Default::default() }, ..Default::default() }));
        ctl.harmonic_pref().tension_target
    };
    assert!(target_of(0.1) < target_of(0.8), "higher movement.tension_target → higher preference target");
}

#[test]
fn the_pad_register_follows_the_chord_tracks_authored_octave() {
    // ADR 0041: a chord track authored an octave low keeps its re-voicings an octave low (and the
    // Arranger never lifts it back into the default register); one authored in the default octave
    // keeps the built-in centre exactly.
    let ctl_for = |doc: LoopDoc| {
        let mut ctl = Controller::new(doc, DitherConfig { max_change_per_loop: 0.05, seed: 4, cat_change_prob: 0.0, ..Default::default() });
        let mv = Movement { orrery: true, phrase_loops: 1, ..Default::default() };
        ctl.set_control(Some(SessionControl { movement: mv, ..Default::default() }));
        ctl
    };
    let authored = load_fixture("loop_orrery_dorian.json");
    assert_eq!(ctl_for(authored.clone()).harmonic_pref().pad_center, crate::orrery::PAD_CENTER, "the fixture's home is in the default octave");

    let mut low = authored.clone();
    for nv in low.modulators.get_mut("chords").unwrap().steps[0].voicing.values_mut() {
        nv.offset -= 12.0;
    }
    let mut ctl = ctl_for(low);
    let center = ctl.harmonic_pref().pad_center;
    assert_eq!(center, crate::orrery::PAD_CENTER - 12);
    let mut moved = false;
    for i in 0..40u64 {
        let p = ctl.next(i);
        ctl.rebase(p.doc);
        let offs: Vec<f32> = ctl.operating_point().modulators["chords"].steps[0].voicing.values().map(|nv| nv.offset).collect();
        if offs.is_empty() {
            continue; // a narrative drop released the chord this phrase
        }
        let mean = offs.iter().sum::<f32>() / offs.len() as f32;
        assert!((center as f32 - 7.0..=center as f32 + 9.0).contains(&mean), "loop {i}: voicing {offs:?} left the authored octave (centre {center})");
        moved |= ctl.orrery_telemetry().is_some_and(|s| s.loops_since_home > 0);
    }
    assert!(moved, "the voyage left home, so the register was exercised on borrowed chords too");
}

#[test]
fn tension_max_forces_repeated_returns_home() {
    // With a low `tension_max`, any excursion is pulled home next phrase — the voyage keeps
    // resolving rather than parking in high tension.
    let mut ctl = Controller::new(load_fixture("loop_orrery_dorian.json"), DitherConfig { max_change_per_loop: 0.05, seed: 6, cat_change_prob: 0.0, ..Default::default() });
    ctl.set_control(Some(SessionControl {
        movement: Movement { orrery: true, phrase_loops: 1, tension_max: 0.2, ..Default::default() },
        ..Default::default()
    }));
    let (mut returns, mut prev_lsh, mut max_away) = (0u32, 0u32, 0u32);
    for i in 0..40u64 {
        let p = ctl.next(i);
        ctl.rebase(p.doc);
        let lsh = ctl.orrery_telemetry().unwrap().loops_since_home;
        if lsh == 0 && prev_lsh > 0 {
            returns += 1;
        }
        max_away = max_away.max(lsh);
        prev_lsh = lsh;
    }
    assert!(returns >= 3, "tension_max keeps forcing returns home, got {returns}");
    assert!(max_away >= 1, "the voyage still departs home between returns");
}

#[test]
fn returning_home_telemetry_marks_the_return_phrase() {
    // Phase 6d: `returning_home` flags the phrases where a homeward return actually lands. With a
    // low global `tension_max` (forced breaches), it is true exactly when `loops_since_home` resets.
    let mut ctl = Controller::new(load_fixture("loop_orrery_dorian.json"), DitherConfig { max_change_per_loop: 0.05, seed: 6, cat_change_prob: 0.0, ..Default::default() });
    ctl.set_control(Some(SessionControl {
        movement: Movement { orrery: true, phrase_loops: 1, tension_max: 0.2, ..Default::default() },
        ..Default::default()
    }));
    let mut flagged = 0u32;
    for i in 0..40u64 {
        let p = ctl.next(i);
        ctl.rebase(p.doc);
        let tv = ctl.orrery_telemetry().unwrap();
        if tv.returning_home {
            assert_eq!(tv.loops_since_home, 0, "global return ⇒ loops_since_home reset this phrase");
            flagged += 1;
        }
    }
    assert!(flagged >= 3, "returning_home marks the breach/return phrases, got {flagged}");
}

#[test]
fn arrange_arp_density_raises_the_pulse_floor() {
    // Phase 6d: `Movement.arrange.arp_density` widens/shifts the euclid-density band. A high floor
    // (min 0.6) produces strictly more arp pulses on average than the default band (min 0.12).
    let avg_pulses = |arrange: Option<crate::contract::ArrangeCfg>| {
        let mut ctl = Controller::new(load_fixture("loop_orrery_roles.json"), DitherConfig { max_change_per_loop: 0.05, seed: 9, cat_change_prob: 0.0, ..Default::default() });
        ctl.set_control(Some(SessionControl {
            movement: Movement { orrery: true, phrase_loops: 1, home: HomeScope::PerModulator, arrange, ..Default::default() },
            ..Default::default()
        }));
        let mut sum = 0u32;
        for i in 0..40u64 {
            let p = ctl.next(i);
            ctl.rebase(p.doc);
            sum += ctl.orrery_telemetry().unwrap().arp_pulses;
        }
        sum as f32 / 40.0
    };
    let base = avg_pulses(None);
    let busy = avg_pulses(Some(crate::contract::ArrangeCfg {
        arp_density: Some(Range01 { min: 0.6, max: 0.95 }),
        ..Default::default()
    }));
    assert!(busy > base + 1.0, "a higher arp_density floor busies the arp: base {base} → busy {busy}");
}

#[test]
fn arrange_absent_is_byte_identical_to_default_cfg() {
    // Determinism (CE-R7.3): an absent `arrange` block must render identically to an explicit
    // default `ArrangeCfg` — the config only *exposes* the built-in constants, never changes them.
    let run = |arrange: Option<crate::contract::ArrangeCfg>| {
        let mut ctl = Controller::new(load_fixture("loop_orrery_roles.json"), DitherConfig { max_change_per_loop: 0.05, seed: 3, cat_change_prob: 0.0, ..Default::default() });
        ctl.set_control(Some(SessionControl {
            movement: Movement { orrery: true, phrase_loops: 1, home: HomeScope::PerModulator, arrange, ..Default::default() },
            ..Default::default()
        }));
        let mut trace = Vec::new();
        for i in 0..30u64 {
            let p = ctl.next(i);
            ctl.rebase(p.doc.clone());
            let tv = ctl.orrery_telemetry().unwrap();
            trace.push((p.path, tv.arp_pulses, tv.energy.to_bits(), tv.returning_home));
        }
        trace
    };
    assert_eq!(run(None), run(Some(crate::contract::ArrangeCfg::default())), "default arrange ≡ absent");
}

// ── Phase 5b: per-track organic returns + leader-led arrangement ─────────

#[test]
fn pick_leader_precedence_and_one_hits_never_lead() {
    assert_eq!(Controller::pick_leader(&["oneshot".to_string()]), None, "one-hits never lead");
    assert_eq!(Controller::pick_leader(&["arp".to_string(), "oneshot".to_string()]), Some("arp".to_string()));
    assert_eq!(Controller::pick_leader(&["bass".to_string(), "arp".to_string()]), Some("bass".to_string()), "bass outranks arp");
    assert_eq!(Controller::pick_leader(&[]), None);
}

fn per_modulator_on() -> SessionControl {
    SessionControl {
        movement: Movement { orrery: true, phrase_loops: 1, tension_max: 1.0, home: HomeScope::PerModulator, ..Default::default() },
        ..Default::default()
    }
}

#[test]
fn per_modulator_rests_a_track_to_authored_home_while_chord_roams() {
    // The dorian bass sits on the 5th (A, a dominant pedal). Over a long per_modulator voyage it
    // should rest to its authored home (the pedal) on some phrase while the chord is *not* home.
    let doc = load_fixture("loop_orrery_dorian.json");
    let authored_bass = serde_json::to_value(&doc.modulators["bass_line"].steps).unwrap();
    let authored_chord = serde_json::to_value(&doc.modulators["chords"].steps).unwrap();
    let mut ctl = Controller::new(doc, DitherConfig { max_change_per_loop: 0.05, seed: 21, cat_change_prob: 0.0, ..Default::default() });
    ctl.set_control(Some(per_modulator_on()));
    let mut pedal_moments = 0;
    for i in 0..60u64 {
        let p = ctl.next(i);
        ctl.rebase(p.doc);
        let bass = serde_json::to_value(&ctl.operating_point().modulators["bass_line"].steps).unwrap();
        let chord = serde_json::to_value(&ctl.operating_point().modulators["chords"].steps).unwrap();
        if bass == authored_bass && chord != authored_chord {
            pedal_moments += 1;
        }
    }
    assert!(pedal_moments >= 1, "the bass rested to its authored home (a pedal) while the chord roamed");
}

#[test]
fn chord_returns_home_organically_before_tension_max() {
    // tension_max = 1.0 ⇒ no hard breach; the chord still returns home (loops_since_home → 0)
    // organically via the cadence-aware return.
    let mut ctl = Controller::new(load_fixture("loop_orrery_dorian.json"), DitherConfig { max_change_per_loop: 0.05, seed: 14, cat_change_prob: 0.0, ..Default::default() });
    ctl.set_control(Some(per_modulator_on()));
    let (mut returns, mut prev_lsh, mut max_away) = (0u32, 0u32, 0u32);
    for i in 0..60u64 {
        let p = ctl.next(i);
        ctl.rebase(p.doc);
        let lsh = ctl.orrery_telemetry().unwrap().loops_since_home;
        if lsh == 0 && prev_lsh > 0 {
            returns += 1;
        }
        max_away = max_away.max(lsh);
        prev_lsh = lsh;
    }
    assert!(max_away >= 1, "the chord departs home");
    assert!(returns >= 1, "the chord returns home organically without a tension_max breach, got {returns}");
}

#[test]
fn per_modulator_is_deterministic() {
    let run = || {
        let mut ctl = Controller::new(load_fixture("loop_orrery_dorian.json"), DitherConfig { max_change_per_loop: 0.05, seed: 33, cat_change_prob: 0.0, ..Default::default() });
        ctl.set_control(Some(per_modulator_on()));
        let mut ops = Vec::new();
        for i in 0..20u64 {
            let p = ctl.next(i);
            ctl.rebase(p.doc);
            ops.push(serde_json::to_value(ctl.operating_point()).unwrap());
        }
        ops
    };
    assert_eq!(run(), run(), "per_modulator trajectory is bit-reproducible");
}

// ── Phase 6a: the axis seam + total return-home ──────────────────────────

#[test]
fn orrery_never_mutes_an_instrument_on_a_role_bound_doc() {
    // The live-review regression: drift + Orrery on the role-bound fixture must never drop a voice
    // (the chord pad vanished and never returned). Voice presence is off both axes now — the
    // categorical layer can't touch `enabled` and Orrery silences via sequences — so every
    // instrument the doc enabled stays enabled across a long run with categorical dither on.
    let doc = load_fixture("loop_orrery_roles.json");
    let enabled0: BTreeMap<String, bool> = doc.instruments.iter().map(|(n, i)| (n.clone(), i.enabled)).collect();
    let mut ctl = Controller::new(doc, DitherConfig { max_change_per_loop: 0.05, seed: 5, cat_change_prob: 0.15, ..Default::default() });
    ctl.set_control(Some(SessionControl {
        movement: Movement { orrery: true, phrase_loops: 1, ..Default::default() },
        ..Default::default()
    }));
    for i in 0..400u64 {
        let p = ctl.next(i);
        for (n, inst) in &p.doc.instruments {
            assert_eq!(inst.enabled, enabled0[n], "loop {i}: {n}.enabled changed — a voice was muted off-axis");
        }
        ctl.rebase(p.doc);
    }
}

#[test]
fn orrery_off_is_a_total_return_home_on_a_role_bound_doc() {
    // The seam makes Orrery-off revert the *full* modulator footprint (not just steps/pool): the
    // euclid generator and pitch_walk order — which drift used to be able to walk — return home too.
    let doc = load_fixture("loop_orrery_roles.json");
    let arp_gen0 = serde_json::to_value(&doc.modulators["glints"].generator).unwrap();
    let arp_order0 = doc.modulators["glints"].pitch_walk.as_ref().unwrap().order.clone();
    let arp_pool0 = doc.modulators["glints"].pitch_walk.as_ref().unwrap().pool.clone();
    let chord0 = serde_json::to_value(&doc.modulators["pad_voicing"].steps).unwrap();
    let mut ctl = Controller::new(doc, DitherConfig { max_change_per_loop: 0.05, seed: 9, cat_change_prob: 0.1, ..Default::default() });
    ctl.set_control(Some(SessionControl {
        movement: Movement { orrery: true, phrase_loops: 1, ..Default::default() },
        ..Default::default()
    }));
    for i in 0..30u64 {
        let p = ctl.next(i);
        ctl.rebase(p.doc);
    }
    // the voyage actually rewrote the arp pool + chord (so the revert is meaningful)
    assert_ne!(ctl.operating_point().modulators["glints"].pitch_walk.as_ref().unwrap().pool, arp_pool0);
    ctl.set_control(Some(SessionControl { movement: Movement { orrery: false, ..Default::default() }, ..Default::default() }));
    let g = &ctl.operating_point().modulators["glints"];
    assert_eq!(serde_json::to_value(&g.generator).unwrap(), arp_gen0, "euclid generator reverted to authored");
    assert_eq!(g.pitch_walk.as_ref().unwrap().order, arp_order0, "pitch_walk order reverted to authored");
    assert_eq!(g.pitch_walk.as_ref().unwrap().pool, arp_pool0, "arp pool reverted to authored");
    assert_eq!(
        serde_json::to_value(&ctl.operating_point().modulators["pad_voicing"].steps).unwrap(),
        chord0,
        "chord steps reverted to authored",
    );
}

#[test]
fn global_scope_returns_home_without_a_breach() {
    // Global scope used to resolve only on a hard `tension_max` breach. With `tension_max` high
    // (no breach) the shared voyage still returns home repeatedly — stronger gravity plus the
    // organic, tension-driven chord-return (ADR 0017).
    let mut ctl = Controller::new(load_fixture("loop_orrery_dorian.json"), DitherConfig { max_change_per_loop: 0.05, seed: 12, cat_change_prob: 0.0, ..Default::default() });
    ctl.set_control(Some(SessionControl {
        movement: Movement { orrery: true, phrase_loops: 1, tension_max: 1.0, home: HomeScope::Global, ..Default::default() },
        ..Default::default()
    }));
    let (mut returns, mut prev_lsh, mut max_away) = (0u32, 0u32, 0u32);
    for i in 0..60u64 {
        let p = ctl.next(i);
        ctl.rebase(p.doc);
        let lsh = ctl.orrery_telemetry().unwrap().loops_since_home;
        if lsh == 0 && prev_lsh > 0 {
            returns += 1;
        }
        max_away = max_away.max(lsh);
        prev_lsh = lsh;
    }
    assert!(max_away >= 1, "the voyage departs home");
    assert!(returns >= 2, "global scope returns home repeatedly without a breach, got {returns}");
}

// ── Phase 6b: the Arranger (register / density / form) ───────────────────

#[test]
fn arranger_breathes_density_and_exposes_energy() {
    // On the D-Lydian sportscar fixture (demo-bound chords/arp), the Arranger drives the arp's
    // euclid density so it varies over the run, and exposes a bounded energy arc in telemetry.
    let mut ctl = Controller::new(
        load_fixture("loop_orrery_sportscar_dlydian.json"),
        DitherConfig { max_change_per_loop: 0.05, seed: 7, cat_change_prob: 0.0, ..Default::default() },
    );
    ctl.set_control(Some(SessionControl {
        movement: Movement { orrery: true, phrase_loops: 1, ..Default::default() },
        ..Default::default()
    }));
    let mut pulses = std::collections::HashSet::new();
    let (mut min_e, mut max_e) = (1.0f32, 0.0f32);
    for i in 0..80u64 {
        let p = ctl.next(i);
        ctl.rebase(p.doc);
        let v = ctl.orrery_telemetry().unwrap();
        assert!((0.0..=1.0).contains(&v.energy), "energy bounded: {}", v.energy);
        if i >= 1 {
            assert!(v.arp_pulses >= 1, "arp density set once the voyage advances");
            pulses.insert(ctl.operating_point().modulators["arp_shimmer"].generator.as_ref().unwrap().pulses);
            min_e = min_e.min(v.energy);
            max_e = max_e.max(v.energy);
        }
    }
    assert!(pulses.len() > 1, "arp density breathes over the run: {pulses:?}");
    assert!(max_e - min_e > 0.1, "the energy arc actually moves: [{min_e}, {max_e}]");
}

#[test]
fn arranger_keeps_determinism_and_reverts_on_orrery_off() {
    // The Arranger is seeded ⇒ a per_modulator voyage with it active replays bit-identically, and
    // Orrery-off still returns the arp register/density to authored (ADR 0017 + 0018).
    let run = || {
        let mut ctl = Controller::new(
            load_fixture("loop_orrery_sportscar_dlydian.json"),
            DitherConfig { max_change_per_loop: 0.05, seed: 19, cat_change_prob: 0.0, ..Default::default() },
        );
        ctl.set_control(Some(per_modulator_on()));
        let mut ops = Vec::new();
        for i in 0..24u64 {
            let p = ctl.next(i);
            ctl.rebase(p.doc);
            ops.push(serde_json::to_value(ctl.operating_point()).unwrap());
        }
        ops
    };
    assert_eq!(run(), run(), "Arranger-driven voyage is bit-reproducible");

    let doc = load_fixture("loop_orrery_sportscar_dlydian.json");
    let pulses0 = doc.modulators["arp_shimmer"].generator.as_ref().unwrap().pulses;
    let pool0 = doc.modulators["arp_shimmer"].pitch_walk.as_ref().unwrap().pool.clone();
    let mut ctl = Controller::new(doc, DitherConfig { max_change_per_loop: 0.05, seed: 19, cat_change_prob: 0.0, ..Default::default() });
    ctl.set_control(Some(per_modulator_on()));
    for i in 0..24u64 {
        let p = ctl.next(i);
        ctl.rebase(p.doc);
    }
    ctl.set_control(Some(SessionControl { movement: Movement { orrery: false, ..Default::default() }, ..Default::default() }));
    let arp = &ctl.operating_point().modulators["arp_shimmer"];
    assert_eq!(arp.generator.as_ref().unwrap().pulses, pulses0, "arp density reverted to authored");
    assert_eq!(arp.pitch_walk.as_ref().unwrap().pool, pool0, "arp register/pool reverted to authored");
}

#[test]
fn narrative_drop_fires_at_a_peak_and_is_deterministic() {
    // Over a long voyage the energy arc crosses the drop threshold and a prominent voice is
    // silenced for a phrase (telemetry.dropped Some); the run replays bit-identically (Phase 6c).
    let run = || {
        let mut ctl = Controller::new(
            load_fixture("loop_orrery_sportscar_dlydian.json"),
            DitherConfig { max_change_per_loop: 0.05, seed: 7, cat_change_prob: 0.0, ..Default::default() },
        );
        ctl.set_control(Some(per_modulator_on()));
        let mut drops = 0u32;
        let mut ops = Vec::new();
        for i in 0..400u64 {
            let p = ctl.next(i);
            ctl.rebase(p.doc);
            if ctl.orrery_telemetry().unwrap().dropped.is_some() {
                drops += 1;
            }
            if i < 40 {
                ops.push(serde_json::to_value(ctl.operating_point()).unwrap());
            }
        }
        (drops, ops)
    };
    let (drops, ops) = run();
    assert!(drops >= 1, "a narrative drop fires at a tension peak over a long run, got {drops}");
    assert_eq!(ops, run().1, "the drop-bearing voyage replays bit-identically");
}

#[test]
fn hold_mode_yields_no_perturbation() {
    let mut ctl = Controller::new(fixture(), DitherConfig::default());
    ctl.set_control(Some(SessionControl { optimize: OptimizeMode::Hold, ..Default::default() }));
    let p = ctl.next(1);
    assert_eq!(p.delta, 0.0);
    assert!(p.path.is_empty());
}

// ── drift directional momentum ───────────────────────────────────────────

#[test]
fn drift_has_directional_momentum() {
    // Over a multi-loop run, drift should hold the same param + direction for several
    // consecutive loops (momentum) and accumulate more than a single step — instead of
    // re-rolling a fresh param/sign every loop and mean-reverting.
    let cfg = DitherConfig { max_change_per_loop: 0.05, seed: 11, cat_change_prob: 0.0, ..Default::default() };
    let mut ctl = Controller::new(fixture(), cfg);
    let mut prev: Option<(String, f32)> = None;
    let (mut max_streak, mut cur_streak) = (1usize, 1usize);
    let mut net = std::collections::HashMap::<String, f32>::new();
    for i in 0..32u64 {
        let p = ctl.next(i);
        *net.entry(p.path.clone()).or_insert(0.0) += p.delta;
        if let Some((pp, ps)) = &prev {
            if *pp == p.path && p.delta != 0.0 && ps.signum() == p.delta.signum() {
                cur_streak += 1;
                max_streak = max_streak.max(cur_streak);
            } else {
                cur_streak = 1;
            }
        }
        prev = Some((p.path.clone(), p.delta));
        ctl.rebase(p.doc);
    }
    assert!(max_streak >= 2, "drift should hold a param+direction for ≥2 consecutive loops; got {max_streak}");
    assert!(
        net.values().any(|&m| m.abs() > cfg.max_change_per_loop),
        "a param should accumulate more than one step in one direction: {net:?}"
    );
}

#[test]
fn drift_momentum_is_deterministic() {
    let cfg = DitherConfig { max_change_per_loop: 0.05, seed: 11, cat_change_prob: 0.0, ..Default::default() };
    let run = || {
        let mut ctl = Controller::new(fixture(), cfg);
        (0..24u64)
            .map(|i| {
                let p = ctl.next(i);
                ctl.rebase(p.doc.clone());
                (p.path, p.delta)
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(run(), run(), "same seed ⇒ identical drift voyage incl. momentum");
}

// ── categorical A/B dither ───────────────────────────────────────────────

#[test]
fn categorical_move_flips_one_lever_with_zero_delta() {
    // cat_change_prob = 1.0 ⇒ every loop is a categorical move.
    let cfg = DitherConfig { max_change_per_loop: 0.05, seed: 19, cat_change_prob: 1.0, ..Default::default() };
    let mut ctl = Controller::new(fixture(), cfg);
    let p = ctl.next(1);
    let cm = p.categorical_move.expect("categorical move at prob 1.0");
    assert_eq!(cm.path, p.path);
    assert_eq!(p.delta, 0.0, "categorical loops carry no numeric δ");
    // the played doc differs from the operating point at the moved path: from → to.
    let from = ctl.operating_point().get_value(&p.path).unwrap();
    let to = p.doc.get_value(&p.path).unwrap();
    assert_ne!(to, from);
    assert_eq!(value_label(&to), cm.to);
    assert_eq!(value_label(&from), cm.from);
}

#[test]
fn categorical_is_deterministic_for_a_fixed_seed() {
    let cfg = DitherConfig { max_change_per_loop: 0.05, seed: 77, cat_change_prob: 0.5, ..Default::default() };
    let run = || {
        let mut ctl = Controller::new(fixture(), cfg);
        (0..16u64)
            .map(|i| {
                let p = ctl.next(i);
                ctl.rebase(p.doc.clone());
                p.categorical_move.map(|m| (m.path, m.from, m.to))
            })
            .collect::<Vec<_>>()
    };
    let a = run();
    assert_eq!(a, run(), "same seed ⇒ identical categorical voyage");
    assert!(a.iter().any(|m| m.is_some()), "some loops should be categorical at p=0.5");
    assert!(a.iter().any(|m| m.is_none()), "some loops should stay numeric at p=0.5");
}

#[test]
fn cat_change_prob_zero_is_pure_numeric() {
    let cfg = DitherConfig { max_change_per_loop: 0.05, seed: 7, cat_change_prob: 0.0, ..Default::default() };
    let mut ctl = Controller::new(fixture(), cfg);
    for i in 0..32 {
        assert!(ctl.next(i).categorical_move.is_none());
    }
}

#[test]
fn dither_disable_pins_an_instrument_against_numeric_and_categorical() {
    let mut d = fixture();
    d.instruments.get_mut("poly").unwrap().dither_disable = true;
    let num = dither_candidates(&d);
    assert!(!num.iter().any(|p| p.starts_with("instruments.poly.")), "poly numeric params excluded: {num:?}");
    assert!(num.iter().any(|p| p.starts_with("instruments.mono.")), "other instruments still dithered");
    let cat: Vec<_> = crate::categorical::categorical_candidates(&d).into_iter().map(|c| c.path).collect();
    assert!(!cat.iter().any(|p| p.starts_with("instruments.poly.")), "poly categorical excluded: {cat:?}");
}

#[test]
fn midi_instruments_are_excluded_from_all_dither_candidates() {
    // ADR 0026: a midi translator's params reach only the *external* synth (the author's
    // patch), so its params, `wave`-style categorical levers, and any matrix depth INTO it are
    // all off the drift axis by default.
    let mut d = fixture();
    {
        let ext = d.instruments.get_mut("poly").unwrap();
        ext.kind = Some("midi".into());
        ext.routing = Some(crate::loop_doc::MidiRouting {
            channel: 1,
            cc: std::collections::BTreeMap::from([("cutoff".to_string(), 74u8)]),
        });
    }
    let num = dither_candidates(&d);
    assert!(!num.iter().any(|p| p.starts_with("instruments.poly.params.")), "midi params excluded: {num:?}");
    assert!(num.iter().any(|p| p.starts_with("instruments.mono.")), "audio instruments untouched");
    // matrix depths whose dest is the midi instrument (param mod or notes) are excluded…
    for (id, e) in &d.matrix {
        if e.kind == "mod" && e.dest.starts_with("instruments.poly.") {
            assert!(!num.contains(&format!("matrix.{id}.{}", e.depth_key())), "depth into midi inst excluded: {id}");
        }
    }
    // …while depths into other dests survive.
    assert!(num.iter().any(|p| crate::loop_doc::depth_path_route(p).is_some()), "{num:?}");
    let cat: Vec<_> = crate::categorical::categorical_candidates(&d).into_iter().map(|c| c.path).collect();
    assert!(!cat.iter().any(|p| p.starts_with("instruments.poly.")), "midi categorical excluded: {cat:?}");
}

#[test]
fn include_midi_opts_midi_paths_into_dither_candidates() {
    // Perturb-only midi dither (ADR 0026 / DitherConfig::include_midi): the opt-in adds the
    // midi translator's numeric params AND matrix depths routed into it; the default keeps
    // excluding both; path_targets_midi classifies exactly the opted-in set.
    let mut d = fixture();
    {
        let ext = d.instruments.get_mut("poly").unwrap();
        ext.kind = Some("midi".into());
        ext.routing = Some(crate::loop_doc::MidiRouting {
            channel: 1,
            cc: std::collections::BTreeMap::from([("cutoff".to_string(), 74u8)]),
        });
    }
    let with = dither_candidates_with(&d, true);
    assert!(with.iter().any(|p| p.starts_with("instruments.poly.params.")), "midi params opted in: {with:?}");
    let mut depth_into_midi_seen = false;
    for (id, e) in &d.matrix {
        if e.kind == "mod" && e.dest.starts_with("instruments.poly.") {
            depth_into_midi_seen = true;
            assert!(with.contains(&format!("matrix.{id}.{}", e.depth_key())), "depth into midi inst opted in: {id}");
        }
    }
    assert!(depth_into_midi_seen, "fixture routes a mod into poly — required for this test");
    // The default stays exclusionary, and the controller wires the flag from its config.
    assert!(!dither_candidates(&d).iter().any(|p| p.starts_with("instruments.poly.params.")));
    let ctl = Controller::new(d.clone(), DitherConfig { include_midi: true, ..Default::default() });
    assert!(ctl.candidates().iter().any(|p| p.starts_with("instruments.poly.params.")), "Controller::new honors include_midi");

    // path_targets_midi: midi params + depths into midi ⇒ true; everything else ⇒ false.
    assert!(path_targets_midi(&d, "instruments.poly.params.cutoff_hz"));
    assert!(!path_targets_midi(&d, "instruments.mono.params.cutoff_hz"));
    assert!(!path_targets_midi(&d, "global.master_gain_db"));
    for (id, e) in &d.matrix {
        if e.kind == "mod" {
            let expect = e.dest.starts_with("instruments.poly.");
            assert_eq!(path_targets_midi(&d, &format!("matrix.{id}.depth")), expect, "matrix.{id} (dest {})", e.dest);
        }
    }
}

#[test]
fn categorical_dither_never_touches_enabled() {
    // The seam (ADR 0017): `enabled` is off the optimize axis. With categorical dither maxed,
    // no instrument's `enabled` ever changes — voice presence belongs to Orrery alone, which
    // silences a voice with a blank/release modulator sequence, never by muting the instrument.
    let d = fixture();
    let before: BTreeMap<String, bool> = d.instruments.iter().map(|(n, i)| (n.clone(), i.enabled)).collect();
    let cfg = DitherConfig { max_change_per_loop: 0.05, seed: 5, cat_change_prob: 1.0, ..Default::default() };
    let mut ctl = Controller::new(d, cfg);
    for i in 0..64u64 {
        let p = ctl.next(i);
        for (n, inst) in &p.doc.instruments {
            assert_eq!(inst.enabled, before[n], "loop {i}: categorical changed {n}.enabled");
        }
        assert!(
            p.categorical_move.as_ref().is_none_or(|m| !m.path.ends_with(".enabled")),
            "loop {i}: an `enabled` flip leaked onto the sound axis"
        );
        ctl.rebase(p.doc);
    }
}

// WS1.6: a fixed control-edit *timeline* (set_control at fixed loop indices) replays identically —
// the determinism basis for replaying a runtime-edited session (each loop record carries the
// revision, so the timeline is recoverable). Per-loop renders stay bit-reproducible (CE-R7.3).
#[test]
fn scripted_control_edits_are_deterministic() {
    let script = || {
        let mut ctl = Controller::new(fixture(), DitherConfig { max_change_per_loop: 0.05, seed: 7, cat_change_prob: 0.0, ..Default::default() });
        let mut traj = Vec::new();
        for i in 0..12u64 {
            if i == 3 {
                ctl.set_control(Some(SessionControl { movement: Movement { orrery: true, ..Default::default() }, ..Default::default() }));
            }
            if i == 7 {
                let mut c = SessionControl::default();
                c.constraints.insert("global.master_gain_db".into(), crate::loop_doc::ConstraintBand { min: Some(0.5), max: Some(0.6) });
                ctl.set_control(Some(c));
            }
            let p = ctl.next(i);
            traj.push((p.path.clone(), p.delta));
            ctl.rebase(p.doc);
        }
        traj
    };
    assert_eq!(script(), script(), "a fixed control-edit timeline replays identically");
}

#[test]
fn note_source_shape_change_is_a_structural_swap() {
    // Same modulator id, different note source (steps → phrase): the old ModulatorPhase's
    // pending_notes / tile clock belong to the old source, so the swap must NOT be seamless.
    let base = r#"{ "schema_version": "0.9", "transport": { "bpm": 60, "master_steps": 16 },
        "harmony": { "root": "C", "mode": "ionian" },
        "instruments": { "m": { "enabled": true, "pitched": true, "note_engine": { "polyphony": 2 }, "params": { "level_db": -6.02 } } },
        "modulators": { "seq": { "type": "step_seq", "output": "note", "length": 8, "step_duration": "1/8", SOURCE } },
        "sequence_pools": { "cell": { "length": 4, "notes": [{ "t": 0, "offset": 0, "dur": 0.5 }] } },
        "matrix": { "r": { "source": "seq", "dest": "instruments.m.notes", "kind": "note", "voices": 2, "priority": 1 } } }"#;
    let steps = LoopDoc::from_json_str(&base.replace("SOURCE", r#""steps": [{ "step": 0, "voicing": { "v": { "offset": 0 } } }]"#)).unwrap();
    let euclid = LoopDoc::from_json_str(&base.replace("SOURCE", r#""generator": { "kind": "euclid", "pulses": 3, "rotation": 0 }"#)).unwrap();
    let phrase = LoopDoc::from_json_str(&base.replace("SOURCE", r#""phrase": { "pool": ["cell"] }"#)).unwrap();
    assert!(!doc_swap_is_structural(&steps, &steps.clone()), "identical docs swap in place");
    assert!(doc_swap_is_structural(&steps, &phrase), "steps → phrase is structural");
    assert!(doc_swap_is_structural(&phrase, &euclid), "phrase → generator is structural");
    assert!(doc_swap_is_structural(&euclid, &steps), "generator → steps is structural");
}

#[test]
fn bus_retype_or_effect_chain_edit_is_a_structural_swap() {
    // The engine keeps a built bus across loops and only re-applies params, so a same-id bus
    // whose `type` or effect chain changed must crossfade, not swap in place (the engine would
    // hard-cut its wet). Param-only bus edits stay seamless.
    let old = fixture(); // synthesized default bus `rv` → effect `verb` (reverb)
    let edit = |f: &dyn Fn(&mut crate::loop_doc::Bus)| {
        let mut d = old.clone();
        f(d.buses.get_mut("rv").unwrap());
        d
    };
    let retyped = edit(&|b| {
        b.kind = Some("external_return".into());
        b.effects.clear();
    });
    assert!(doc_swap_is_structural(&old, &retyped), "internal → external_return is structural");
    assert!(doc_swap_is_structural(&retyped, &old), "external_return → internal is structural");
    assert!(doc_swap_is_structural(&old, &edit(&|b| b.effects.clear())), "removing an effect is structural");
    assert!(
        doc_swap_is_structural(&old, &edit(&|b| {
            let fx = b.effects.remove("verb").unwrap();
            b.effects.insert("hall".into(), fx);
        })),
        "renaming an effect is structural"
    );
    assert!(
        doc_swap_is_structural(&old, &edit(&|b| b.effects.get_mut("verb").unwrap().kind = "delay".into())),
        "retyping an effect is structural"
    );
    let param_edit = edit(&|b| {
        b.return_gain_db = -0.92;
        b.effects.get_mut("verb").unwrap().params.insert("room".into(), DocValue::from(0.95));
    });
    assert!(!doc_swap_is_structural(&old, &param_edit), "a bus param / return_gain_db edit swaps in place");
}

// ── ADR 0029: song block reset ───────────────────────────────────────────

fn drift_orrery_control() -> SessionControl {
    let mut c = SessionControl { revision: 4, movement: Movement { orrery: true, phrase_loops: 1, ..Default::default() }, ..Default::default() };
    c.overrides.insert("global.master_gain_db".into(), DocValue::from(0.8));
    c
}

/// Drive `ctl` for `n` loops (rebasing each), as the session does.
fn run_loops(ctl: &mut Controller, from: u64, n: u64) {
    for i in from..from + n {
        let p = ctl.next(i);
        ctl.rebase(p.doc);
    }
}

#[test]
fn begin_block_is_a_fresh_controller() {
    let cfg = DitherConfig { max_change_per_loop: 0.05, seed: 7, cat_change_prob: 0.0, ..Default::default() };
    let mut ctl = Controller::new(load_fixture("loop_orrery_dorian.json"), cfg);
    ctl.set_control(Some(drift_orrery_control()));
    run_loops(&mut ctl, 0, 12);
    // The block has travelled: drift moved some param and the voyage advanced.
    let authored = load_fixture("loop_orrery_dorian.json");
    let moved = ctl.candidates().iter().filter(|p| *p != "global.master_gain_db").any(|p| {
        (ctl.operating_point().get_param_norm(p).unwrap() - authored.get_param_norm(p).unwrap()).abs() > 1e-3
    });
    assert!(moved, "drift travel happened");
    assert!(ctl.orrery.as_ref().unwrap().loops_seen > 0);
    let (opt, rev) = (ctl.optimize_label(), ctl.revision());

    // Block 2: a different matrix, seed 99.
    let mut block = authored.clone();
    let route = block.matrix.keys().next().unwrap().clone();
    block.matrix.get_mut(&route).unwrap().priority = 2;
    ctl.begin_block(block.clone(), 99, None);

    // ≡ a controller that started the session on this block with this seed + control …
    let mut fresh = Controller::new(block, DitherConfig { seed: 99, ..cfg });
    fresh.set_control(Some(drift_orrery_control()));
    assert_eq!(
        serde_json::to_value(ctl.operating_point()).unwrap(),
        serde_json::to_value(fresh.operating_point()).unwrap(),
        "operating point = the authored block (+ Orrery home phrase + overrides)"
    );
    assert_eq!(ctl.operating_point().get_param("global.master_gain_db").unwrap(), 0.8, "overrides re-applied");
    assert!(ctl.drift_streak.is_none());
    assert_eq!(ctl.cfg.seed, 99);
    let orr = ctl.orrery.as_ref().unwrap();
    assert_eq!((orr.loops_seen, orr.voyager.loops_since_home()), (0, 0), "a fresh voyage from home");
    // … with the same standing control.
    assert_eq!((ctl.optimize_label(), ctl.revision()), (opt, rev));
    // From here on it moves exactly like the fresh one (same seed ⇒ same trajectory).
    for i in 100..106 {
        let (a, b) = (ctl.next(i), fresh.next(i));
        assert_eq!(serde_json::to_value(&a.doc).unwrap(), serde_json::to_value(&b.doc).unwrap(), "loop {i}");
        ctl.rebase(a.doc);
        fresh.rebase(b.doc);
    }
}

#[test]
fn block_seed_picks_the_voyage() {
    let voyage = |seed: u64| {
        let mut ctl = Controller::new(load_fixture("loop_orrery_dorian.json"), DitherConfig { max_change_per_loop: 0.05, seed: 1, cat_change_prob: 0.0, ..Default::default() });
        ctl.set_control(Some(orrery_on()));
        run_loops(&mut ctl, 0, 6);
        ctl.begin_block(load_fixture("loop_orrery_dorian.json"), seed, None);
        let mut chords = Vec::new();
        for i in 6..20u64 {
            let p = ctl.next(i);
            ctl.rebase(p.doc);
            chords.push(ctl.orrery_telemetry().unwrap().chord);
        }
        chords
    };
    assert_eq!(voyage(42), voyage(42), "same block seed ⇒ same voyage");
    assert_ne!(voyage(42), voyage(43), "a different block seed ⇒ a different voyage");
}

// ── ADR 0055: a block's control patch ───────────────────────────────────

fn orrery_off_patch() -> ControlPatch {
    let mut movement = crate::doc_value::Map::new();
    movement.insert("orrery".into(), DocValue::from(false));
    ControlPatch { movement: Some(movement), ..Default::default() }
}

/// The Orrery-writable footprint of every modulator (steps, pools, generators, phrase arms).
fn footprint(doc: &LoopDoc) -> serde_json::Value {
    serde_json::to_value(&doc.modulators).unwrap()
}

#[test]
fn a_block_can_turn_the_orrery_off_and_the_next_one_brings_it_back() {
    let doc = load_fixture("loop_orrery_dorian.json");
    let cfg = DitherConfig { max_change_per_loop: 0.05, seed: 7, cat_change_prob: 0.0, ..Default::default() };
    let mut ctl = Controller::new(doc.clone(), cfg);
    ctl.set_control(Some(orrery_on()));
    run_loops(&mut ctl, 0, 8);
    assert_ne!(footprint(ctl.operating_point()), footprint(&doc), "the voyage rewrote the bound voices");

    ctl.begin_block(doc.clone(), 31, Some(orrery_off_patch()));
    assert!(ctl.orrery.is_none() && ctl.orrery_telemetry().is_none(), "no voyage in the block");
    for i in 8..20u64 {
        let p = ctl.next(i);
        assert!(p.harmonic_move.is_none(), "loop {i}: no chord change");
        assert_eq!(footprint(&p.doc), footprint(&doc), "loop {i}: the bound voices play their authored material");
        ctl.rebase(p.doc);
    }
    assert!(ctl.control_ref().unwrap().movement.orrery == false && ctl.standing.as_ref().unwrap().movement.orrery, "the standing control is untouched");

    // A block without control gets the session control back: a fresh voyage from home.
    ctl.begin_block(doc.clone(), 32, None);
    let orr = ctl.orrery.as_ref().expect("the Orrery is back");
    assert_eq!((orr.loops_seen, orr.voyager.loops_since_home()), (0, 0));
    let mut fresh = Controller::new(doc, DitherConfig { seed: 32, ..cfg });
    fresh.set_control(Some(orrery_on()));
    for i in 20..26u64 {
        let (a, b) = (ctl.next(i), fresh.next(i));
        assert_eq!(serde_json::to_value(&a.doc).unwrap(), serde_json::to_value(&b.doc).unwrap(), "loop {i}");
        ctl.rebase(a.doc);
        fresh.rebase(b.doc);
    }
}

#[test]
fn a_block_patch_equals_a_session_started_with_that_control() {
    // The patch merges field by field over the standing control; the block then plays exactly as a
    // session that began with the merged control.
    let doc = load_fixture("loop_orrery_dorian.json");
    let cfg = DitherConfig { max_change_per_loop: 0.05, seed: 7, cat_change_prob: 0.0, ..Default::default() };
    let mut movement = crate::doc_value::Map::new();
    movement.insert("phrase_loops".into(), DocValue::from(3u64));
    movement.insert("tension_target".into(), DocValue::from(0.1));
    let patch = ControlPatch { movement: Some(movement), max_change: Some(0.02), ..Default::default() };
    let mut ctl = Controller::new(doc.clone(), cfg);
    ctl.set_control(Some(drift_orrery_control()));
    run_loops(&mut ctl, 0, 5);
    ctl.begin_block(doc.clone(), 44, Some(patch));

    let mut want = drift_orrery_control();
    want.movement.phrase_loops = 3;
    want.movement.tension_target = 0.1;
    want.guardrails.max_change_per_loop = 0.02;
    let mut fresh = Controller::new(doc, DitherConfig { seed: 44, ..cfg });
    fresh.set_control(Some(want));
    for i in 5..17u64 {
        let (a, b) = (ctl.next(i), fresh.next(i));
        assert_eq!(serde_json::to_value(&a.doc).unwrap(), serde_json::to_value(&b.doc).unwrap(), "loop {i}");
        let (lo, hi) = param_range(&a.path);
        assert!(a.delta.abs() <= (hi - lo) * 0.02 + 1e-5, "loop {i}: the block's max_change caps the step ({} {})", a.path, a.delta);
        ctl.rebase(a.doc);
        fresh.rebase(b.doc);
    }
}

#[test]
fn a_live_toggle_beats_the_block_until_the_next_block() {
    let doc = load_fixture("loop_orrery_dorian.json");
    let mut ctl = Controller::new(doc.clone(), DitherConfig { max_change_per_loop: 0.05, seed: 7, cat_change_prob: 0.0, ..Default::default() });
    ctl.set_control(Some(orrery_on()));
    ctl.begin_block(doc.clone(), 5, Some(orrery_off_patch()));

    // The panel's hold ⇄ drift toggle sends the whole session control (its Orrery on) and names
    // only the hold: the block keeps the Orrery off.
    let hold = SessionControl { optimize: OptimizeMode::Hold, ..orrery_on() };
    ctl.set_live_control(Some(hold.clone()), LiveToggle { optimize: Some(OptimizeMode::Hold), orrery: None });
    assert_eq!(ctl.optimize_label(), "hold");
    assert!(ctl.orrery.is_none(), "a hold toggle doesn't turn the block's Orrery back on");
    // A whole-control edit (no toggle) keeps the block's patch and the earlier toggle.
    ctl.set_control(Some(orrery_on()));
    assert!(ctl.orrery.is_none());
    assert_eq!(ctl.optimize_label(), "hold", "the toggle lasts the block");

    // The Orrery switch, flipped on, beats the block for the rest of it.
    ctl.set_live_control(Some(hold.clone()), LiveToggle { optimize: None, orrery: Some(true) });
    assert!(ctl.orrery.is_some());

    // The next block start applies its own control over the session control again.
    ctl.begin_block(doc, 6, Some(orrery_off_patch()));
    assert!(ctl.orrery.is_none());
    assert_eq!(ctl.optimize_label(), "hold", "the panel's hold is in the session control it sent, so it carries on");
    // A block that says drift beats it from its start.
    let drift = ControlPatch { optimize: Some(OptimizeMode::Drift), ..Default::default() };
    ctl.begin_block(load_fixture("loop_orrery_dorian.json"), 7, Some(drift));
    assert_eq!(ctl.optimize_label(), "drift");
}

#[test]
fn a_held_param_stays_put_while_the_others_drift() {
    let doc = fixture();
    let cfg = DitherConfig { max_change_per_loop: 0.05, seed: 3, cat_change_prob: 0.0, ..Default::default() };
    let probe = Controller::new(doc.clone(), cfg);
    let held = probe.candidates()[0].clone();
    let v = doc.get_param(&held).unwrap();
    let mut constraints = BTreeMap::new();
    constraints.insert(held.clone(), crate::loop_doc::ConstraintBand { min: Some(v), max: Some(v) });
    let mut ctl = Controller::new(doc.clone(), cfg);
    ctl.begin_block(doc.clone(), 3, Some(ControlPatch { constraints, ..Default::default() }));
    let mut moved_other = false;
    for i in 0..200u64 {
        let p = ctl.next(i);
        assert_eq!(p.doc.get_param(&held), Some(v), "loop {i}: {held} is pinned");
        moved_other |= p.delta != 0.0 && p.path != held;
        ctl.rebase(p.doc);
    }
    assert!(moved_other, "the other params still drift");
}

#[test]
fn begin_tail_freezes_and_drops_note_routes() {
    let mut ctl = Controller::new(fixture(), DitherConfig { max_change_per_loop: 0.05, seed: 3, cat_change_prob: 0.3, ..Default::default() });
    ctl.set_control(Some(orrery_on()));
    run_loops(&mut ctl, 0, 3);
    ctl.begin_tail();
    assert!(ctl.operating_point().matrix.values().all(|e| e.kind != "note"), "note routes dropped");
    assert!(ctl.operating_point().matrix.values().any(|e| e.kind == "mod"), "mod routes stay (the tail still breathes)");
    for i in 3..6u64 {
        let p = ctl.next(i);
        assert!(p.path.is_empty() && p.delta == 0.0 && p.harmonic_move.is_none() && p.categorical_move.is_none());
        let unmoved = ctl.operating_point().clone().with_loop_id(i);
        assert_eq!(serde_json::to_value(&p.doc).unwrap(), serde_json::to_value(&unmoved).unwrap());
        ctl.rebase(p.doc);
    }
}

#[test]
fn orrery_telemetry_carries_the_voicing() {
    // ADR 0038: the editor panel's keyboard draws the pad voicing, the bass and the chord's tones.
    let mut ctl = Controller::new(load_fixture("loop_orrery_dorian.json"), DitherConfig { max_change_per_loop: 0.05, seed: 6, cat_change_prob: 0.0, ..Default::default() });
    ctl.set_control(Some(SessionControl { movement: Movement { orrery: true, phrase_loops: 1, ..Default::default() }, ..Default::default() }));
    for i in 0..12u64 {
        let p = ctl.next(i);
        ctl.rebase(p.doc);
        let tv = ctl.orrery_telemetry().unwrap();
        assert!(tv.tones.len() >= 3, "a chord has tones: {:?}", tv.tones);
        assert_eq!(tv.bass.rem_euclid(12), tv.tones[0].rem_euclid(12), "the bass is the chord's root");
        assert!(!tv.pad.is_empty());
        let pcs: Vec<i32> = tv.tones.iter().map(|t| t.rem_euclid(12)).collect();
        let in_chord = tv.pad.iter().filter(|o| pcs.contains(&o.rem_euclid(12))).count();
        assert!(in_chord * 2 >= tv.pad.len(), "the pad voices the chord (a pedal may hold): {:?} vs {:?}", tv.pad, tv.tones);
    }
}

// ── Live edits keep the Orrery's voyage (the approved plan; ADR 0035 amendment) ──────────────

const VOYAGE_VOICES: [&str; 4] = ["pad_voicing", "sub_bass", "glints", "chimes"];

fn roles_ctl(doc: LoopDoc) -> Controller {
    let mut ctl = Controller::new(doc, DitherConfig { seed: 5, cat_change_prob: 0.0, ..Default::default() });
    ctl.set_control(Some(SessionControl {
        optimize: OptimizeMode::Hold,
        movement: Movement { orrery: true, phrase_loops: 2, ..Default::default() },
        ..Default::default()
    }));
    ctl
}

/// What the Orrery writes: the chord / bass / arp / one-shot modulators as they play.
fn voyage_voices(ctl: &Controller) -> serde_json::Value {
    let m = &ctl.operating_point().modulators;
    serde_json::to_value(VOYAGE_VOICES.map(|id| &m[id])).unwrap()
}

/// `n` loops from `from`, as a session plays them (`next` + `rebase`): the voices after each.
fn play_voyage(ctl: &mut Controller, from: u64, n: u64) -> Vec<serde_json::Value> {
    (from..from + n)
        .map(|i| {
            let p = ctl.next(i);
            ctl.rebase(p.doc);
            voyage_voices(ctl)
        })
        .collect()
}

#[test]
fn every_orrery_track_stays_inside_its_note_range() {
    // ADR 0042: whatever the voyage does (chord re-voicing, the bass, arp pools and Arranger lifts,
    // one-shot accents in their high register), each ranged modulator plays inside its range.
    let ranges = [("pad_voicing", -5.0, 12.0), ("sub_bass", -24.0, -12.0), ("glints", 0.0, 12.0), ("chimes", 0.0, 12.0)];
    let pitches = |m: &crate::loop_doc::Modulator| -> Vec<f32> {
        let mut v: Vec<f32> = m.steps.iter().flat_map(|s| s.voicing.values().filter(|nv| !nv.release).map(|nv| nv.offset)).collect();
        v.extend(m.pitch_walk.iter().flat_map(|pw| pw.pool.iter().copied()));
        v.extend(m.phrase.iter().flat_map(|ph| ph.realized.iter().flat_map(|r| r.notes.iter().map(|n| n.offset))));
        v
    };
    let run = |ranged: bool| -> Vec<(&str, f32, f32, f32)> {
        let mut doc = load_fixture("loop_orrery_roles.json");
        if ranged {
            for (id, lo, hi) in ranges {
                let m = doc.modulators.get_mut(id).unwrap();
                m.orrery_note_min = Some(lo);
                m.orrery_note_max = Some(hi);
            }
        }
        let mut ctl = roles_ctl(doc);
        let mut seen = Vec::new();
        for i in 0..80u64 {
            let p = ctl.next(i);
            ctl.rebase(p.doc);
            for (id, lo, hi) in ranges {
                for x in pitches(&ctl.operating_point().modulators[id]) {
                    if ranged {
                        assert!((lo..=hi).contains(&x), "loop {i}: {id} wrote {x}, outside {lo}..={hi}");
                    }
                    seen.push((id, x, lo, hi));
                }
            }
        }
        seen
    };
    let strays: Vec<&str> = run(false).into_iter().filter(|&(_, x, lo, hi)| !(lo..=hi).contains(&x)).map(|(id, ..)| id).collect();
    assert!(strays.contains(&"glints") && strays.contains(&"chimes"), "without ranges the arp and accents leave them: {strays:?}");
    run(true);
}

#[test]
fn a_live_edit_keeps_the_orrery_voyage() {
    let doc = load_fixture("loop_orrery_roles.json");
    let mut plain = roles_ctl(doc.clone());
    let mut edited = roles_ctl(doc.clone());
    play_voyage(&mut plain, 0, 5);
    play_voyage(&mut edited, 0, 5); // mid-phrase: loop 5 isn't a boundary (2-loop phrases)
    let before = voyage_voices(&edited);
    assert_ne!(before, voyage_voices(&roles_ctl(doc.clone())), "the voyage has left home");

    // A value edit (timbre) doesn't put the authored chord back mid-phrase …
    edited.swap_doc(doc.with_param("instruments.warm_pad.params.cutoff_hz", 318.49));
    assert_eq!(voyage_voices(&edited), before);
    // … and a structural one (a new LFO + route) doesn't restart the voyage from home.
    let mut grown = doc.clone().with_param("instruments.warm_pad.params.cutoff_hz", 318.49);
    let mut lfo = grown.modulators["lfo_breath"].clone();
    lfo.rate_hz = Some(0.013);
    grown.modulators.insert("lfo_slow".into(), lfo);
    let mut route = grown.matrix.values().find(|r| r.source == "lfo_breath").unwrap().clone();
    route.source = "lfo_slow".into();
    grown.matrix.insert("m_slow".into(), route);
    assert!(candidates::doc_swap_is_structural(edited.operating_point(), &grown));
    let mut edited = edited.rebuilt(grown);
    assert_eq!(voyage_voices(&edited), before);
    assert_eq!(play_voyage(&mut edited, 5, 6), play_voyage(&mut plain, 5, 6), "the voyage goes on as if nothing happened");
}

#[test]
fn an_edited_voice_plays_as_written_while_the_others_keep_the_voyage() {
    let doc = load_fixture("loop_orrery_roles.json");
    let mut ctl = roles_ctl(doc.clone());
    play_voyage(&mut ctl, 0, 5);
    let before = voyage_voices(&ctl);
    let mut revoiced = doc.clone();
    let v1 = revoiced.modulators.get_mut("pad_voicing").unwrap().steps[0].voicing.get_mut("v1").unwrap();
    v1.offset -= 12.0;
    ctl.swap_doc(revoiced.clone());
    let now = voyage_voices(&ctl);
    assert_eq!(now[0], serde_json::to_value(&revoiced.modulators["pad_voicing"]).unwrap(), "the edit is heard");
    assert_eq!(now.as_array().unwrap()[1..], before.as_array().unwrap()[1..], "bass, arp and accents keep the voyage");
}

#[test]
fn a_new_key_or_a_restructured_chord_starts_the_voyage_from_home() {
    let doc = load_fixture("loop_orrery_roles.json");
    // After the edit at loop 5 the voices play exactly as a fresh controller on the edited doc does.
    let check = |what: &str, new_doc: LoopDoc| {
        let mut ctl = roles_ctl(doc.clone());
        play_voyage(&mut ctl, 0, 5);
        let mut ctl = if candidates::doc_swap_is_structural(ctl.operating_point(), &new_doc) {
            ctl.rebuilt(new_doc.clone())
        } else {
            ctl.swap_doc(new_doc.clone());
            ctl
        };
        assert_eq!(play_voyage(&mut ctl, 5, 6), play_voyage(&mut roles_ctl(new_doc), 0, 6), "{what}");
    };
    let mut d = doc.clone();
    d.harmony.root = "E".into();
    check("a new root", d);
    let mut d = doc.clone();
    d.harmony.mode = "aeolian".into();
    check("a new mode", d);
    let mut d = doc.clone();
    let upper = d.modulators["pad_voicing"].clone();
    d.modulators.insert("pad_upper".into(), upper);
    check("a second chord track", d);
    let mut d = doc.clone();
    d.modulators.get_mut("sub_bass").unwrap().orrery_role = None;
    check("the bass unbound", d);
}
