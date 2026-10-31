//! Candidate enumeration for the **timbre axis**: which param paths drift may move, midi-path
//! gating (ADR 0026), and the structural-swap test. Split from
//! `controller.rs` (pure code motion); the `pub` fns are re-exported by the parent so
//! `controller::dither_candidates` paths are unchanged.

use super::*;

/// Snapshot each candidate param's authored value (normalized `0..1`) — the center of its cumulative
/// `max_drift` band (ADR 0025). Captured from the post-safe-clamp base so an anchor is always in-range.
pub(super) fn capture_anchors(base: &LoopDoc, candidates: &[String]) -> BTreeMap<String, f32> {
    candidates
        .iter()
        .filter_map(|p| base.get_param_norm(p).map(|n| (p.clone(), n)))
        .collect()
}

/// True when a candidate path's audible effect exists **only in the external chain** (ADR 0026):
/// a `type:"midi"` instrument's own param, or a matrix depth whose dest resolves into one.
pub fn path_targets_midi(doc: &LoopDoc, path: &str) -> bool {
    let inst_is_midi = |inst: &str| doc.instruments.get(inst).map(|i| i.is_midi()).unwrap_or(false);
    if let Some(rest) = path.strip_prefix("instruments.") {
        if let Some((inst, _)) = rest.split_once(".params.") {
            return inst_is_midi(inst);
        }
        return false;
    }
    if let Some(e) = doc.route_of_depth(path) {
        return match crate::loop_doc::resolve_dest(&e.dest) {
            crate::loop_doc::Dest::InstParam { inst, .. } => inst_is_midi(&inst),
            crate::loop_doc::Dest::Notes { inst } => inst_is_midi(&inst),
            _ => false,
        };
    }
    false
}

/// Enumerate controller-changeable base params (Ctrl ✓): instrument params in range, the two
/// globals, `mod`-kind matrix depths within their dest's span, and the buses' returns and effect
/// params. A silent (`-Infinity`) gain never moves (ADR 0054 P2). Returns canonical JSONPaths, sorted.
/// Midi-targeted paths are excluded (the default); [`dither_candidates_with`] opts them in.
pub fn dither_candidates(doc: &LoopDoc) -> Vec<String> {
    dither_candidates_with(doc, false)
}

/// [`dither_candidates`] with the midi switch ([`DitherConfig::include_midi`], ADR 0026):
/// `include_midi` adds `type:"midi"` instruments' numeric params and matrix depths routed into
/// them, so drift also *moves* the external synth's CCs.
pub fn dither_candidates_with(doc: &LoopDoc, include_midi: bool) -> Vec<String> {
    let mut out = Vec::new();
    for (name, inst) in &doc.instruments {
        if inst.dither_disable {
            continue; // pinned instrument (e.g. a constant bass drone) — never dithered
        }
        if inst.is_midi() && !include_midi {
            // midi translator (ADR 0026): its params only reach the *external* synth, whose patch
            // is the author's. Excluded by default; `include_midi` opts into drifting them too.
            continue;
        }
        for (param, val) in &inst.params {
            if param.starts_with("x_") {
                continue; // an annotation (ADR 0033): it must never move — or change the render
            }
            if crate::params::is_fixed(param) {
                continue; // author-only (ADR 0047): drift must not wander a binaural beat
            }
            if let Some(v) = val.as_f64() {
                // A gain authored silent (`-Infinity`) is off — drift must not fade it in; the range
                // gate below leaves it out.
                let path = format!("instruments.{name}.params.{param}");
                // Each param is gated against its own native range (default 0..1), so wide-range
                // params like `shift_st` (−24..24) are candidates while strings/out-of-range aren't.
                let (lo, hi) = param_range(&path);
                if (lo as f64..=hi as f64).contains(&v) {
                    out.push(path);
                }
            }
        }
    }
    if doc.global.master_gain_db.is_finite() {
        out.push("global.master_gain_db".to_string());
    }
    out.push("global.stereo_width".to_string());
    // A connection *into* a midi instrument (param mod or notes) is part of the external patch
    // like the instrument's own params (ADR 0026) — skip its depth unless `include_midi`.
    for (id, e) in &doc.matrix {
        // …nor through the depth of a route into an author-only param (ADR 0047).
        // A depth is controllable within its dest's span (|depth_oct| ≤ 6.64 into a cutoff; ADR 0054 P4).
        if e.kind == "mod" && e.depth_norm().abs() <= 1.0 && !crate::params::is_fixed(crate::params::units::leaf(&e.dest)) {
            let path = format!("matrix.{id}.{}", e.depth_key());
            if include_midi || !path_targets_midi(doc, &path) {
                out.push(path);
            }
        }
    }
    // Send-bus params: return gain + each effect's numeric params (range-gated). `width` is
    // build-time-fixed, so it's never dithered. An external_return bus (ADR 0026 phase D) is
    // shell config, not rendered audio — there's nothing to drift.
    for (bus_id, bus) in &doc.buses {
        if bus.is_external_return() {
            continue;
        }
        if bus.return_gain_db.is_finite() {
            out.push(format!("buses.{bus_id}.return_gain_db"));
        }
        for (fx_id, fx) in &bus.effects {
            for (param, val) in &fx.params {
                if param == "width" || param.starts_with("x_") {
                    continue;
                }
                if let Some(v) = val.as_f64() {
                    let path = format!("buses.{bus_id}.effects.{fx_id}.params.{param}");
                    let (lo, hi) = param_range(&path);
                    if (lo as f64..=hi as f64).contains(&v) {
                        out.push(path);
                    }
                }
            }
        }
    }
    out.sort();
    out
}

/// Whether an `old → new` loop-doc edit changes **structure** (not just leaf/array-content values):
/// the instrument/modulator/matrix/bus id sets, any instrument synth `type` or modulator `type`, a
/// bus's `type` or effect chain ([`Bus::same_shape`](crate::loop_doc::Bus::same_shape) — the engine
/// rebuilds such a bus, which would hard-cut its wet), or the loop length (transport
/// `bpm`/`master_steps`/`step_unit`). Same-id, same-shape edits return
/// `false` ⇒ the session applies them seamlessly via [`Controller::swap_doc`] (the carried
/// `PhaseSnapshot` reconciles by id). A `true` ⇒ the session crossfades a fresh standby engine so the
/// transition is glitch-free (ADR 0024).
pub fn doc_swap_is_structural(old: &LoopDoc, new: &LoopDoc) -> bool {
    // Which of the three exclusive note sources a modulator uses (steps ⊕ generator ⊕
    // phrase, ADR 0028). A same-id switch between them must NOT reuse the old
    // `ModulatorPhase`: its `pending_notes` / tile clock belong to the old source.
    fn note_source_shape(m: &crate::loop_doc::Modulator) -> u8 {
        if m.phrase.is_some() {
            2
        } else if m.generator.is_some() {
            1
        } else {
            0
        }
    }
    old.instruments.keys().ne(new.instruments.keys())
        || old.modulators.keys().ne(new.modulators.keys())
        || old.matrix.keys().ne(new.matrix.keys())
        || old.buses.keys().ne(new.buses.keys())
        || old.buses.iter().any(|(k, b)| new.buses.get(k).is_some_and(|n| !n.same_shape(b)))
        || old.instruments.iter().any(|(k, i)| new.instruments.get(k).is_some_and(|n| n.kind != i.kind))
        || old.modulators.iter().any(|(k, m)| {
            new.modulators.get(k).is_some_and(|n| n.kind != m.kind || note_source_shape(n) != note_source_shape(m))
        })
        || old.transport.bpm != new.transport.bpm
        || old.transport.master_steps != new.transport.master_steps
        || old.transport.step_unit != new.transport.step_unit
}
