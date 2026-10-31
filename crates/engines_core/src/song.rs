//! Songs — a "loop of loops" (ADR 0029).
//!
//! A loop doc may carry an optional top-level `song`: an ordered `sequence` of **blocks**, each a
//! **matrix configuration** played for a set number of inner loops. When the sequence (plus an
//! optional release `tail`) completes, the song ends. Each block's matrix is the doc's top-level
//! `matrix` with a named **layer** merged over it by route id (RFC 7386 JSON merge patch: a field
//! overrides, `null` removes a route, a full entry adds one). Blocks change the matrix ONLY —
//! instruments, modulators, harmony, transport, and globals are fixed for the whole song, so the
//! engine runs continuously across blocks (synth voices, reverb tails, free-running phases carry).
//!
//! This module is pure (no I/O): spec types, materialization, validation, the block clock, and the
//! dry-run plan. The session drives it at the loop boundary (`SongDriver`, ADR 0029 §4); the
//! controller resets at each block start (`Controller::begin_block`).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use crate::doc_value::{from_doc_value, to_doc_value, DocValue, Map};

use crate::check::DocDiag;
use crate::contract::{patch_movement, ControlPatch, OptimizeMode};
use crate::controller::{tier2_norm, Controller, DEFAULT_MAX_CHANGE};
use crate::engine::{is_glideable_dest, FadingRoute, PhaseSnapshot, Seam};
use crate::telemetry::SongTelemetry;
use crate::hash::hash64;
use crate::loop_doc::{param_denorm, param_norm, param_range, resolve_dest, ConstraintBand, Dest, LoopDoc, MatrixEntry};

/// The `song` section of a loop doc. `deny_unknown_fields` is the **matrix-only** enforcement:
/// there is nowhere to put a per-block instrument/harmony/global change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SongSpec {
    /// Named matrix layers: `layer → { route id → partial MatrixEntry | null }`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub matrices: BTreeMap<String, Layer>,
    /// The blocks, in play order.
    pub sequence: Vec<SongBlock>,
    /// Seam glide length in seconds (params ease back to the new block's authored values, routes
    /// fade in/out). `0` = hard cut. Must not exceed the shortest block.
    #[serde(default = "default_transition_s")]
    pub transition_s: f32,
    /// Loops rendered after the last block with every note route dropped (voices release, reverb
    /// rings out) before the song ends.
    #[serde(default = "default_tail_loops")]
    pub tail_loops: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SongBlock {
    /// A `matrices` layer name; absent ⇒ the doc's top-level matrix as-is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matrix: Option<String>,
    /// How many inner loops this block lasts (≥ 1).
    pub loops: u32,
    /// Explicit block seed: blocks sharing one replay the same Orrery voyage (a recap). Mixed with
    /// the session seed, so `--seed` still varies the whole song.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// Display label (telemetry / dry-run); defaults to the layer name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// This block's session control (ADR 0055): a patch over the session's, for this block only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<BlockControl>,
}

/// A block's `control` (ADR 0055), in the doc's own control vocabulary (ADR 0033).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockControl {
    /// `hold` or `drift` for this block (`live_edit` is refused).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optimize: Option<OptimizeMode>,
    /// A partial `movement`, merged over the session's field by field (`orrery: false` turns it off).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub movement: Option<Map>,
    /// The block's cumulative drift cap (normalized), in place of the doc's `max_drift`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_drift: Option<f32>,
    /// Caps the block's per-loop step (normalized); the session's step stays the ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_change: Option<f32>,
    /// Per-param drift overrides, keyed by canonical JSONPath.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub drift: BTreeMap<String, DriftOverride>,
}

impl BlockControl {
    /// What the block says about the Orrery: `Some(false)` turns it off, `Some(true)` on, `None`
    /// leaves the session's.
    pub fn orrery(&self) -> Option<bool> {
        self.movement.as_ref()?.get("orrery")?.as_bool()
    }

    /// One line for the dry-run plan: `orrery off · hold · max_drift 0.05 · cutoff_hz hold, drive_db 6..12`.
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(m) = &self.movement {
            if let Some(on) = m.get("orrery").and_then(DocValue::as_bool) {
                parts.push(format!("orrery {}", if on { "on" } else { "off" }));
            }
            parts.extend(m.iter().filter(|(k, _)| k.as_str() != "orrery").map(|(k, v)| format!("{k} {v}")));
        }
        if let Some(o) = self.optimize {
            parts.push(match o {
                OptimizeMode::Hold => "hold".into(),
                OptimizeMode::Drift => "drift".into(),
                OptimizeMode::LiveEdit => "live_edit".into(),
            });
        }
        parts.extend(self.max_drift.map(|d| format!("max_drift {d}")));
        parts.extend(self.max_change.map(|d| format!("max_change {d}")));
        let leaf = |p: &str| p.strip_prefix("instruments.").map(|r| r.replacen(".params.", ".", 1)).unwrap_or_else(|| p.to_string());
        let drift: Vec<String> = self
            .drift
            .iter()
            .map(|(p, o)| match o {
                DriftOverride::Word(w) => format!("{} {w}", leaf(p)),
                DriftOverride::Spec(DriftSpec { max_drift: Some(d), .. }) => format!("{} ±{d}", leaf(p)),
                DriftOverride::Spec(s) => {
                    let end = |x: Option<f32>| x.map(|x| x.to_string()).unwrap_or_default();
                    format!("{} {}..{}", leaf(p), end(s.min), end(s.max))
                }
            })
            .collect();
        if !drift.is_empty() {
            parts.push(format!("drift: {}", drift.join(", ")));
        }
        parts.join(" · ")
    }
}

/// One `drift` override: `"hold"` (pinned at the authored value), or a spec.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DriftOverride {
    Word(String),
    Spec(DriftSpec),
}

/// A drift override's spec: a native `{min, max}` band, or a normalized `max_drift` around the
/// authored value — one or the other.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriftSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_drift: Option<f32>,
}

/// The movement fields a block may patch (every field of [`crate::contract::Movement`]).
pub const MOVEMENT_FIELDS: &[&str] = &["orrery", "bind", "phrase_loops", "tension_target", "tension_max", "voice_changes_per_loop", "home", "arrange"];
/// Movement fields that only matter while the Orrery runs.
const ORRERY_ONLY_FIELDS: &[&str] = &["bind", "phrase_loops", "tension_target", "tension_max", "voice_changes_per_loop", "home", "arrange"];

/// The authored value of drift candidate `path` as the block's controller starts it (normalized,
/// after the Tier-2 safe clamp, as `Controller::new` does) and the native value it stores.
fn start_value(doc: &LoopDoc, path: &str) -> Option<(f32, f32)> {
    let v = doc.get_param(path)?;
    let n = param_norm(path, v);
    let (lo, hi) = tier2_norm(doc, path, Some(n));
    let clamped = n.max(lo).min(hi);
    Some(if (clamped - n).abs() > 1e-9 { (clamped, param_denorm(path, clamped)) } else { (n, v) })
}

/// Block `control` → the controller's patch (ADR 0055 D1). `doc` is the block's doc. A `drift`
/// entry that doesn't resolve (`validate` refuses those) is skipped.
pub fn resolve_control(doc: &LoopDoc, ctl: &BlockControl) -> ControlPatch {
    let constraints = ctl
        .drift
        .iter()
        .filter_map(|(path, o)| {
            let (n, native) = start_value(doc, path)?;
            let band = match o {
                DriftOverride::Word(w) if w == "hold" => ConstraintBand { min: Some(native), max: Some(native) },
                DriftOverride::Word(_) => return None,
                DriftOverride::Spec(DriftSpec { max_drift: Some(d), .. }) => {
                    ConstraintBand { min: Some(param_denorm(path, (n - d).max(0.0))), max: Some(param_denorm(path, (n + d).min(1.0))) }
                }
                DriftOverride::Spec(s) => ConstraintBand { min: s.min, max: s.max },
            };
            Some((path.clone(), band))
        })
        .collect();
    ControlPatch { optimize: ctl.optimize, movement: ctl.movement.clone(), max_change: ctl.max_change, constraints }
}

/// Write a block's control into its doc's own control settings (ADR 0055 D5), so playing the block
/// on its own (`from_doc`) hears it; `max_drift` goes where the controller reads it.
fn fold_control(doc: &mut LoopDoc, ctl: &BlockControl) {
    let patch = resolve_control(doc, ctl);
    if let Some(o) = patch.optimize {
        doc.optimize = Some(o);
    }
    if let Some(m) = &patch.movement {
        doc.movement = Some(patch_movement(&doc.movement.clone().unwrap_or_default(), m));
    }
    doc.constraints.extend(patch.constraints);
    if let Some(d) = ctl.max_drift {
        doc.max_drift = Some(d);
    }
    if let Some(m) = ctl.max_change {
        doc.max_change = Some(doc.max_change.unwrap_or(DEFAULT_MAX_CHANGE).min(m));
    }
}

fn default_transition_s() -> f32 {
    8.0
}
fn default_tail_loops() -> u32 {
    1
}

/// A matrix layer: `route id → partial MatrixEntry | null`.
pub type Layer = BTreeMap<String, DocValue>;

/// Salt for per-block seeds (`hash64(session, block_idx, SALT, 0)`).
pub(crate) const SONG_SEED_SALT: u64 = 0x534E_4721;
/// Salt for explicit per-block seeds (`hash64(session, seed, SALT, 0)`).
pub(crate) const SONG_EXPLICIT_SEED_SALT: u64 = 0x534E_4722;

/// Every field a matrix entry may carry (a layer patch naming anything else is a typo).
const MATRIX_ENTRY_FIELDS: &[&str] = &[
    "source", "dest", "kind", "depth", "depth_oct", "depth_st", "depth_cents", "depth_s", "depth_ms", "depth_db", "depth_hz", "depth_x", "mode", "depth_slew_ms", "voices",
    "priority",
];

/// RFC 7386 JSON merge patch: object members merge recursively, `null` deletes, anything else
/// replaces.
pub fn merge_patch(target: &mut DocValue, patch: &DocValue) {
    let DocValue::Object(p) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = DocValue::Object(Map::new());
    }
    let DocValue::Object(t) = target else { unreachable!() };
    for (k, v) in p {
        if v.is_null() {
            t.remove(k);
        } else {
            merge_patch(t.entry(k.clone()).or_insert(DocValue::Null), v);
        }
    }
}

/// The seed block `idx` runs its controller (drift / Orrery) with. Block 0 keeps the
/// session seed **unchanged**, so a one-block song is bit-identical to a plain session; later
/// blocks get a fresh hashed seed (a fresh Orrery voyage per block); an explicit `seed` is mixed
/// with the session seed so equal explicit seeds replay the same voyage within a song.
pub fn block_seed(session_seed: u64, idx: usize, explicit: Option<u64>) -> u64 {
    match explicit {
        Some(s) => hash64(session_seed, s, SONG_EXPLICIT_SEED_SALT, 0),
        None if idx == 0 => session_seed,
        None => hash64(session_seed, idx as u64, SONG_SEED_SALT, 0),
    }
}

impl SongSpec {
    /// The layer a block plays (`None` ⇒ base matrix).
    fn layer(&self, idx: usize) -> Result<Option<(&str, &Layer)>, String> {
        let block = self.sequence.get(idx).ok_or_else(|| format!("song: no block {idx}"))?;
        match &block.matrix {
            None => Ok(None),
            Some(name) => self
                .matrices
                .get(name)
                .map(|l| Some((name.as_str(), l)))
                .ok_or_else(|| format!("song block {idx}: unknown matrix layer {name:?} (have {:?})", self.matrices.keys().collect::<Vec<_>>())),
        }
    }

    /// Display label for block `idx`: explicit label, else layer name, else `"base"`.
    pub fn label(&self, idx: usize) -> String {
        let Some(b) = self.sequence.get(idx) else { return String::new() };
        b.label.clone().or_else(|| b.matrix.clone()).unwrap_or_else(|| "base".into())
    }

    /// Σ block loops (excluding the tail).
    pub fn body_loops(&self) -> usize {
        self.sequence.iter().map(|b| b.loops as usize).sum()
    }
}

/// Block `idx`'s playable doc: `base` with the block's layer merged over its matrix, its `control`
/// written into the doc's own control settings (ADR 0055 D5), and `song` stripped. Goes through the
/// same serde round-trip the controller's perturbations use.
pub fn materialize(base: &LoopDoc, spec: &SongSpec, idx: usize) -> Result<LoopDoc, String> {
    let mut doc = materialize_matrix(base, spec, idx)?;
    if let Some(ctl) = &spec.sequence[idx].control {
        fold_control(&mut doc, ctl);
    }
    Ok(doc)
}

/// [`materialize`] without the block's `control`: the base doc with the block's matrix.
fn materialize_matrix(base: &LoopDoc, spec: &SongSpec, idx: usize) -> Result<LoopDoc, String> {
    let layer = spec.layer(idx)?;
    let mut v = to_doc_value(base).map_err(|e| format!("song: serialize base doc: {e}"))?;
    if let DocValue::Object(m) = &mut v {
        m.remove("song");
    }
    if let Some((_, layer)) = layer {
        let patch = DocValue::Object(layer.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
        let target = v.as_object_mut().expect("doc serializes to an object").entry("matrix".to_string()).or_insert(DocValue::Object(Map::new()));
        merge_patch(target, &patch);
    }
    LoopDoc::from_value(v).map_err(|e| format!("song block {idx} ({}): {e}", spec.label(idx)))
}

/// Seconds per loop (exact — `master_steps` beats at `bpm`).
pub fn loop_seconds(doc: &LoopDoc) -> f64 {
    doc.transport.master_steps as f64 * 60.0 / doc.transport.bpm as f64
}

/// Check one block's resolved routes point at things that exist (the base doc's own matrix is not
/// otherwise validated; song docs get the stricter check because blocks re-wire it).
fn check_routes(doc: &LoopDoc, where_: &str) -> Result<(), String> {
    for (id, e) in &doc.matrix {
        let bad = |msg: String| Err(format!("{where_}: route {id:?}: {msg}"));
        if !doc.modulators.contains_key(&e.source) {
            return bad(format!("source {:?} is not a modulator", e.source));
        }
        let dest = resolve_dest(&e.dest);
        match e.kind.as_str() {
            "note" if !matches!(dest, Dest::Notes { .. }) => return bad(format!("a note route's dest must be instruments.<inst>.notes (got {:?})", e.dest)),
            "mod" if matches!(dest, Dest::Notes { .. }) => return bad(format!("a mod route can't target notes ({:?}) — use kind \"note\"", e.dest)),
            "note" | "mod" => {}
            other => return bad(format!("kind {other:?} (expected \"mod\" | \"note\")")),
        }
        // The renderer only adds contributions; anything else would silently render as "add".
        if e.mode != "add" {
            return bad(format!("mode {:?} (only \"add\" is supported)", e.mode));
        }
        match &dest {
            Dest::Unknown => return bad(format!("dest {:?} is not a recognised JSONPath", e.dest)),
            Dest::InstParam { inst, .. } | Dest::Notes { inst } if !doc.instruments.contains_key(inst) => {
                return bad(format!("dest instrument {inst:?} does not exist"));
            }
            Dest::BusReturn { bus } if !doc.buses.contains_key(bus) => return bad(format!("dest bus {bus:?} does not exist")),
            Dest::BusParam { bus, fx, .. } if !doc.buses.get(bus).is_some_and(|b| b.effects.contains_key(fx)) => {
                return bad(format!("dest bus effect {bus}.{fx} does not exist"));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Hard song errors (called from `LoopDoc::validate` at the authoring boundary). Every message
/// names the block / layer / route at fault.
pub(crate) fn validate(doc: &LoopDoc, spec: &SongSpec) -> Result<(), String> {
    if spec.sequence.is_empty() {
        return Err("song: `sequence` is empty — a song needs at least one block".into());
    }
    for (i, b) in spec.sequence.iter().enumerate() {
        if b.loops == 0 {
            return Err(format!("song block {i} ({}): loops must be ≥ 1", spec.label(i)));
        }
        spec.layer(i)?;
    }
    if !spec.transition_s.is_finite() || spec.transition_s < 0.0 {
        return Err(format!("song: transition_s {} must be a finite number ≥ 0", spec.transition_s));
    }
    let loop_s = loop_seconds(doc);
    let (shortest_i, shortest) = spec.sequence.iter().enumerate().min_by_key(|(_, b)| b.loops).map(|(i, b)| (i, b.loops)).unwrap();
    let shortest_s = shortest as f64 * loop_s;
    if spec.transition_s as f64 > shortest_s + 1e-9 {
        return Err(format!(
            "song: transition_s {} exceeds the shortest block ({} × {loop_s:.3} s = {shortest_s:.3} s, block {shortest_i}) — seams must not overlap",
            spec.transition_s, shortest
        ));
    }
    for (name, layer) in &spec.matrices {
        for (rid, patch) in layer {
            let where_ = format!("song matrix layer {name:?}, route {rid:?}");
            match patch {
                DocValue::Null => {}
                DocValue::Object(fields) => {
                    if let Some(k) = fields.keys().find(|k| !MATRIX_ENTRY_FIELDS.contains(&k.as_str())) {
                        return Err(format!("{where_}: unknown field {k:?} (a layer may only patch matrix entries: {MATRIX_ENTRY_FIELDS:?})"));
                    }
                    if !doc.matrix.contains_key(rid) {
                        // A new route: must be a complete entry.
                        if let Some(k) = ["source", "dest", "kind"].iter().find(|k| fields.get(**k).is_none_or(|v| v.is_null())) {
                            return Err(format!("{where_}: a route not in the base matrix must be a full entry — missing {k:?}"));
                        }
                    }
                    // Type-check the merged entry on its own so the error names the route.
                    let mut merged = doc.matrix.get(rid).map(|e| to_doc_value(e).unwrap_or(DocValue::Null)).unwrap_or(DocValue::Object(Map::new()));
                    merge_patch(&mut merged, patch);
                    from_doc_value::<MatrixEntry>(merged).map_err(|e| format!("{where_}: {e}"))?;
                }
                other => return Err(format!("{where_}: must be an object (a matrix-entry patch) or null, got {other}")),
            }
        }
    }
    for i in 0..spec.sequence.len() {
        let block = materialize(doc, spec, i)?;
        let where_ = format!("song block {i} ({})", spec.label(i));
        check_routes(&block, &where_)?;
        block.validate().map_err(|e| format!("{where_}: {e}"))?;
    }
    Ok(())
}

/// Hard errors in the blocks' `control` (ADR 0055 D6), each located at the key at fault. Run once
/// [`validate`] passed (every block materializes).
pub(crate) fn check_controls(doc: &LoopDoc, spec: &SongSpec) -> Vec<DocDiag> {
    let mut out = Vec::new();
    for (i, b) in spec.sequence.iter().enumerate() {
        let Some(ctl) = &b.control else { continue };
        let at = format!("song.sequence[{i}].control");
        let block = format!("song block {i} ({})", spec.label(i));
        let mut err = |path: String, msg: String| out.push(DocDiag::error(&path, "invalid", format!("{block}: {msg}")));
        if ctl.optimize == Some(OptimizeMode::LiveEdit) {
            err(format!("{at}.optimize"), "a block's optimize is \"hold\" or \"drift\" (\"live_edit\" is for hand-editing a running doc)".into());
        }
        if let Some(m) = &ctl.movement {
            if let Some(k) = m.keys().find(|k| !MOVEMENT_FIELDS.contains(&k.as_str())) {
                err(format!("{at}.movement.{k}"), format!("unknown movement field `{k}` (a block may patch: {})", MOVEMENT_FIELDS.join(", ")));
            } else {
                let mut v = to_doc_value(&doc.movement.clone().unwrap_or_default()).unwrap_or_default();
                merge_patch(&mut v, &DocValue::Object(m.clone()));
                if let Err(e) = from_doc_value::<crate::contract::Movement>(v) {
                    err(format!("{at}.movement"), format!("movement: {e}"));
                }
            }
        }
        for (key, v) in [("max_drift", ctl.max_drift), ("max_change", ctl.max_change)] {
            if let Some(x) = v.filter(|x| !(x.is_finite() && *x > 0.0 && *x <= 1.0)) {
                err(format!("{at}.{key}"), format!("{key} {x} must be above 0 and at most 1 (normalized)"));
            }
        }
        let Ok(bdoc) = materialize_matrix(doc, spec, i) else { continue };
        let candidates = crate::controller::dither_candidates_with(&bdoc, true);
        for (path, o) in &ctl.drift {
            let here = format!("{at}.drift.{path}");
            if !candidates.contains(path) {
                let why = match resolve_dest(path) {
                    Dest::InstParam { inst, .. } if bdoc.instruments.get(&inst).is_some_and(|x| x.dither_disable) => {
                        format!("instrument {inst:?} has dither_disable, so drift never moves it")
                    }
                    _ if bdoc.get_param(path).is_none() => "it names no numeric param in this block".to_string(),
                    _ => "drift never moves it (a muted level, an author-only param, or a value outside its range)".to_string(),
                };
                let names: Vec<&str> = candidates.iter().map(String::as_str).collect();
                let hint = crate::check::did_you_mean(path, &names).map(|m| format!(" — did you mean `{m}`?")).unwrap_or_default();
                err(here, format!("`{path}` is not a drift candidate: {why}{hint}"));
                continue;
            }
            match o {
                DriftOverride::Word(w) if w == "hold" => {}
                DriftOverride::Word(w) => err(here, format!("unknown override {w:?} (expected \"hold\", {{min, max}} or {{max_drift}})")),
                DriftOverride::Spec(s) => {
                    let band = s.min.is_some() || s.max.is_some();
                    match (band, s.max_drift) {
                        (true, Some(_)) => err(here, "give a band ({min, max}) or a max_drift, not both".into()),
                        (false, None) => err(here, "empty override (expected \"hold\", {min, max} or {max_drift})".into()),
                        (false, Some(d)) if !(d.is_finite() && d > 0.0 && d <= 1.0) => {
                            err(format!("{here}.max_drift"), format!("max_drift {d} must be above 0 and at most 1 (normalized)"))
                        }
                        (false, Some(_)) => {}
                        (true, None) => {
                            let (lo, hi) = param_range(path);
                            let (start, _) = start_value(&bdoc, path).unwrap_or((0.5, 0.0));
                            let (slo, shi) = tier2_norm(&bdoc, path, Some(start));
                            let (safe_lo, safe_hi) = (param_denorm(path, slo), param_denorm(path, shi));
                            if let (Some(a), Some(z)) = (s.min, s.max) {
                                if a > z {
                                    err(here.clone(), format!("min {a} is above max {z} (use \"hold\" to pin it)"));
                                    continue;
                                }
                            }
                            for (side, x) in [("min", s.min), ("max", s.max)] {
                                if let Some(x) = x.filter(|x| !(lo..=hi).contains(x)) {
                                    err(format!("{here}.{side}"), format!("{side} {x} is outside `{path}`'s range [{lo}, {hi}]"));
                                }
                            }
                            if s.min.is_some_and(|a| a > safe_hi) || s.max.is_some_and(|z| z < safe_lo) {
                                err(here, format!("the band misses `{path}`'s safe range [{safe_lo}, {safe_hi}] (safe_bounds) — drift could never reach it"));
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

/// Soft warnings about the blocks' `control` (ADR 0055 D6).
fn lint_controls(doc: &LoopDoc, spec: &SongSpec, w: &mut Vec<String>) {
    let doc_orrery = doc.movement.as_ref().is_some_and(|m| m.orrery);
    let any_on = doc_orrery || spec.sequence.iter().any(|b| b.control.as_ref().and_then(BlockControl::orrery) == Some(true));
    for (i, b) in spec.sequence.iter().enumerate() {
        let Some(ctl) = &b.control else { continue };
        let block = format!("song block {i} ({})", spec.label(i));
        if let Some(m) = &ctl.movement {
            let tuned: Vec<&str> = m.keys().map(String::as_str).filter(|k| ORRERY_ONLY_FIELDS.contains(k)).collect();
            if ctl.orrery() == Some(false) && !tuned.is_empty() {
                w.push(format!("{block}: control.movement turns the Orrery off, so {} do nothing here", tuned.join(", ")));
            } else if !any_on && (ctl.orrery() == Some(false) || !tuned.is_empty()) {
                w.push(format!("{block}: control.movement changes the Orrery, but it is never on in this doc (only `--orrery` would turn it on)"));
            }
        }
        let optimize = ctl.optimize.or(doc.optimize).unwrap_or_default();
        if optimize == OptimizeMode::Hold && !ctl.drift.is_empty() {
            w.push(format!("{block}: control.drift overrides do nothing — the block holds (optimize \"hold\")"));
        }
        let Ok(bdoc) = materialize_matrix(doc, spec, i) else { continue };
        for (path, o) in &ctl.drift {
            let (DriftOverride::Spec(s), Some(v)) = (o, bdoc.get_param(path)) else { continue };
            if s.min.is_some_and(|a| v < a) || s.max.is_some_and(|z| v > z) {
                w.push(format!(
                    "{block}: control.drift `{path}` is authored {v}, outside its band — drift drives it in by max_change per loop, so it moves at the block start"
                ));
            }
        }
    }
}

/// Soft song warnings (surfaced via `LoopDoc::lint`).
pub(crate) fn lint(doc: &LoopDoc, spec: &SongSpec) -> Vec<String> {
    let mut w = Vec::new();
    lint_controls(doc, spec, &mut w);
    let used: BTreeSet<&str> = spec.sequence.iter().filter_map(|b| b.matrix.as_deref()).collect();
    for name in spec.matrices.keys() {
        if !used.contains(name.as_str()) {
            w.push(format!("song: matrix layer {name:?} is never used by the sequence"));
        }
    }
    for (name, layer) in &spec.matrices {
        for (rid, patch) in layer {
            if patch.is_null() && !doc.matrix.contains_key(rid) {
                w.push(format!("song: layer {name:?} removes route {rid:?}, which the base matrix doesn't have (no-op)"));
            }
        }
    }
    if spec.tail_loops == 0 {
        w.push("song: tail_loops 0 — the song stops mid-sound (no release / reverb ring-out)".into());
    }
    if spec.transition_s > 60.0 {
        w.push(format!("song: transition_s {} is over a minute — seams this long blur whole sections", spec.transition_s));
    }
    // Note routes `(instrument, source)` a seam removes: the engine releases exactly those voices
    // (`release_orphans` keys on the pair, so another source still feeding the instrument doesn't
    // save them), and with ~zero release they cut off hard (`adsr_env` ends at release ≤ 0). The
    // last block → tail transition drops every note route, so it counts too.
    let mut warned = BTreeSet::new();
    let note_routes = |d: &LoopDoc| -> BTreeSet<(String, String)> {
        d.matrix
            .values()
            .filter(|e| e.kind == "note")
            .filter_map(|e| match resolve_dest(&e.dest) {
                Dest::Notes { inst } => Some((inst, e.source.clone())),
                _ => None,
            })
            .collect()
    };
    let mut stages: Vec<(String, BTreeSet<(String, String)>)> =
        (0..spec.sequence.len()).filter_map(|i| materialize(doc, spec, i).ok().map(|b| (spec.label(i), note_routes(&b)))).collect();
    if spec.tail_loops > 0 {
        stages.push(("the tail".into(), BTreeSet::new()));
    }
    for pair in stages.windows(2) {
        let ((_, before), (into, after)) = (&pair[0], &pair[1]);
        for (inst, source) in before.difference(after) {
            let Some(i) = doc.instruments.get(inst) else { continue };
            // Skip what can't hang: the engine's one-shot `event` voice, and percussive envelopes
            // (sustain ≈ 0) whose notes die on their own before any seam matters.
            let held = i.norm("sustain").unwrap_or(1.0) > 0.05 && inst != "event";
            let subtractive_like = matches!(i.synth_type(), "subtractive" | "dual" | "binaural");
            if subtractive_like && held && i.native_or_default("release_s") < 0.05 && warned.insert(inst.clone()) {
                w.push(format!(
                    "song: instrument {inst:?} loses its note route from {source:?} entering {into} but has ~zero release — notes still sounding there stop dead (raise `release_s` for a fade)"
                ));
            }
        }
    }
    let total_s = (spec.body_loops() + spec.tail_loops as usize) as f64 * loop_seconds(doc);
    if total_s > 24.0 * 3600.0 {
        w.push(format!("song: total length {} is over 24 h", fmt_hms(total_s)));
    }
    w
}

/// Format seconds as `H:MM:SS`.
pub fn fmt_hms(s: f64) -> String {
    let t = s.max(0.0).round() as u64;
    format!("{}:{:02}:{:02}", t / 3600, (t / 60) % 60, t % 60)
}

/// Where loop `n` (worker-local index, from 0) falls in the song.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SongPos {
    Block { idx: usize, loop_in_block: usize },
    Tail { loop_in_tail: usize },
    Done,
}

/// Maps a worker-local loop index to the song position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SongClock {
    starts: Vec<usize>,
    lens: Vec<usize>,
    body: usize,
    tail: usize,
}

impl SongClock {
    pub fn new(spec: &SongSpec) -> Self {
        let lens: Vec<usize> = spec.sequence.iter().map(|b| b.loops as usize).collect();
        let mut starts = Vec::with_capacity(lens.len());
        let mut acc = 0;
        for &l in &lens {
            starts.push(acc);
            acc += l;
        }
        Self { starts, lens, body: acc, tail: spec.tail_loops as usize }
    }
    /// Σ block loops + tail — the song's length in loops.
    pub fn total_loops(&self) -> usize {
        self.body + self.tail
    }
    pub fn body_loops(&self) -> usize {
        self.body
    }
    pub fn tail_loops(&self) -> usize {
        self.tail
    }
    pub fn blocks(&self) -> usize {
        self.lens.len()
    }
    pub fn block_loops(&self, idx: usize) -> usize {
        self.lens.get(idx).copied().unwrap_or(0)
    }
    pub fn block_start(&self, idx: usize) -> usize {
        self.starts.get(idx).copied().unwrap_or(self.body)
    }
    pub fn pos(&self, n: usize) -> SongPos {
        if n < self.body {
            let idx = self.starts.partition_point(|&s| s <= n) - 1;
            SongPos::Block { idx, loop_in_block: n - self.starts[idx] }
        } else if n < self.body + self.tail {
            SongPos::Tail { loop_in_tail: n - self.body }
        } else {
            SongPos::Done
        }
    }
    /// `n` starts a block or the tail.
    pub fn is_segment_start(&self, n: usize) -> bool {
        match self.pos(n) {
            SongPos::Block { loop_in_block, .. } => loop_in_block == 0,
            SongPos::Tail { loop_in_tail } => loop_in_tail == 0,
            SongPos::Done => false,
        }
    }
}

/// The glide from the outgoing operating point `old` (drifted, Orrery-voyaged) into block doc `new`
/// (the controller's freshly reset operating point) over `transition_frames` (ADR 0029 §2).
/// `transition_frames == 0` ⇒ a hard cut that still releases notes whose route disappeared.
pub fn seam_between(old: &LoopDoc, new: &LoopDoc, transition_frames: u64) -> Seam {
    if transition_frames == 0 {
        return Seam::release_only();
    }
    let mut seam = Seam { total_frames: transition_frames, release_orphans: true, ..Default::default() };
    // `o`/`n`: the old and new values, normalized (the seam's clamp applied here: a silent gain's
    // `-inf` glides from or to the −60 dB floor).
    let mut offset = |dest: String, o: Option<f32>, n: Option<f32>| {
        if let (Some(o), Some(n)) = (o, n) {
            let d = o.clamp(0.0, 1.0) - n.clamp(0.0, 1.0);
            if d.abs() > 1e-6 && is_glideable_dest(&dest, new) {
                seam.param_offsets.insert(dest, d);
            }
        }
    };
    for (name, inst) in &new.instruments {
        let Some(prev) = old.instruments.get(name) else { continue };
        for param in inst.params.keys() {
            offset(format!("instruments.{name}.params.{param}"), prev.norm(param), inst.norm(param));
        }
    }
    offset("global.master_gain_db".into(), Some(crate::params::GAIN.inverse(old.global.master_gain_db)), Some(crate::params::GAIN.inverse(new.global.master_gain_db)));
    offset("global.stereo_width".into(), Some(old.global.stereo_width), Some(new.global.stereo_width));
    for (bus_id, bus) in &new.buses {
        let Some(prev) = old.buses.get(bus_id) else { continue };
        offset(format!("buses.{bus_id}.return_gain_db"), Some(prev.return_norm()), Some(bus.return_norm()));
        for (fx_id, fx) in &bus.effects {
            let Some(pfx) = prev.effects.get(fx_id) else { continue };
            for param in fx.params.keys() {
                offset(format!("buses.{bus_id}.effects.{fx_id}.params.{param}"), pfx.norm(param), fx.norm(param));
            }
        }
    }
    let same_route = |a: &MatrixEntry, b: &MatrixEntry| a.source == b.source && a.dest == b.dest && a.kind == b.kind;
    for (id, e) in new.matrix.iter().filter(|(_, e)| e.kind == "mod") {
        match old.matrix.get(id) {
            Some(o) if same_route(o, e) => {
                if (o.depth_norm() - e.depth_norm()).abs() > 1e-6 {
                    seam.depth_from.insert(id.clone(), o.depth_norm());
                }
            }
            _ => {
                seam.depth_from.insert(id.clone(), 0.0);
            }
        }
    }
    for (id, o) in old.matrix.iter().filter(|(_, o)| o.kind == "mod") {
        let kept = new.matrix.get(id).is_some_and(|e| same_route(o, e));
        if !kept && o.depth != 0.0 {
            seam.fading_routes.insert(id.clone(), FadingRoute { source: o.source.clone(), dest: o.dest.clone(), depth: o.depth_norm() });
        }
    }
    seam
}

/// Drives a song at the session's loop boundary (ADR 0029 §4): holds every block's materialized doc
/// and seed, and at each block start resets the controller and arms the seam glide on the snapshot.
/// Pure (no I/O): the same driver runs under every shell, so a song is bit-reproducible.
pub struct SongDriver {
    blocks: Vec<LoopDoc>,
    seeds: Vec<u64>,
    /// Each block's control patch (ADR 0055); `None` ⇒ the session control as it stands.
    patches: Vec<Option<ControlPatch>>,
    labels: Vec<String>,
    clock: SongClock,
    transition_frames: u64,
    /// Rendered seconds per loop (the frame-rounded length, so `elapsed_s` matches the audio).
    loop_s: f64,
    /// Block audition (ADR 0036): the session starts at this block; `offset` = its first song loop.
    first: usize,
    offset: usize,
}

impl SongDriver {
    /// Materialize every block up front (a bad block fails here, not hours into a render).
    pub fn new(base: &LoopDoc, spec: &SongSpec, session_seed: u64, sample_rate: f64) -> Result<Self, String> {
        if spec.sequence.is_empty() {
            return Err("song: `sequence` is empty".into());
        }
        let blocks = (0..spec.sequence.len()).map(|i| materialize(base, spec, i)).collect::<Result<Vec<_>, _>>()?;
        let seeds = spec.sequence.iter().enumerate().map(|(i, b)| block_seed(session_seed, i, b.seed)).collect();
        let patches = spec.sequence.iter().zip(&blocks).map(|(b, doc)| b.control.as_ref().map(|c| resolve_control(doc, c))).collect();
        let labels = (0..spec.sequence.len()).map(|i| spec.label(i)).collect();
        let loop_s = blocks[0].total_frames(sample_rate) as f64 / sample_rate;
        let transition_frames = (spec.transition_s.max(0.0) as f64 * sample_rate).round() as u64;
        Ok(Self { blocks, seeds, patches, labels, clock: SongClock::new(spec), transition_frames, loop_s, first: 0, offset: 0 })
    }

    /// Block `i`'s playable doc (song stripped).
    pub fn block_doc(&self, i: usize) -> &LoopDoc {
        &self.blocks[i]
    }
    /// Block `i`'s controller seed.
    pub fn block_seed(&self, i: usize) -> u64 {
        self.seeds[i]
    }
    /// Block `i`'s control patch (ADR 0055), if it carries one.
    pub fn block_control(&self, i: usize) -> Option<&ControlPatch> {
        self.patches[i].as_ref()
    }
    /// Loops this run plays (from the first block to the end of the tail).
    pub fn total_loops(&self) -> usize {
        self.clock.total_loops() - self.offset
    }

    /// Start the run at block `k` (block audition: "play the song from here"). The session begins
    /// on [`block_doc(k)`](Self::block_doc) with its seed and loop ids from [`offset`](Self::offset),
    /// as the full song reaches it (the block's controller reset is the same either way) — only the
    /// state a full run carries into the block (DSP tails, a voyage already under way) differs.
    pub fn start_at(mut self, k: usize) -> Self {
        self.first = k.min(self.blocks.len() - 1);
        self.offset = self.clock.block_start(self.first);
        self
    }
    /// The block the run starts on (0 unless [`start_at`](Self::start_at)).
    pub fn first_block(&self) -> usize {
        self.first
    }
    /// The song loop the run starts on (`block_start(first_block)`): a session started at block
    /// `k` numbers its loops from here, so every `loop_id`-keyed roll matches the full song's.
    pub fn offset(&self) -> usize {
        self.offset
    }
    pub fn clock(&self) -> &SongClock {
        &self.clock
    }

    /// The loop-top hook: call once per loop with the worker-local index `n` (0-based; NOT the
    /// `loop_id`), after any pending control edit and before the pick. The first block was begun by
    /// the session (the controller starts on [`block_doc(first)`](Self::block_doc)); when that block
    /// carries a control, the controller is begun again on it with the control applied, before
    /// anything plays. Every later block start resets the controller ([`Controller::begin_block`])
    /// and arms the seam glide from the outgoing operating point; the tail start freezes the
    /// controller and releases every note.
    pub fn on_loop_top(&mut self, n: usize, ctl: &mut Controller, snap: &mut PhaseSnapshot) -> SongTelemetry {
        match self.clock.pos(n + self.offset) {
            SongPos::Block { idx, loop_in_block: 0 } if idx == self.first && n == 0 => {
                if let Some(p) = &self.patches[idx] {
                    ctl.begin_block(self.blocks[idx].clone(), self.seeds[idx], Some(p.clone()));
                }
            }
            SongPos::Block { idx, loop_in_block: 0 } if idx > self.first => {
                let outgoing = ctl.operating_point().clone();
                ctl.begin_block(self.blocks[idx].clone(), self.seeds[idx], self.patches[idx].clone());
                snap.seam = Some(seam_between(&outgoing, ctl.operating_point(), self.transition_frames));
            }
            SongPos::Tail { loop_in_tail: 0 } => {
                ctl.begin_tail();
                snap.seam = Some(Seam::release_only());
            }
            _ => {}
        }
        self.telemetry(n, snap.seam.is_some())
    }

    /// The song position of loop `n` (for telemetry / progress lines).
    pub fn telemetry(&self, n: usize, seam: bool) -> SongTelemetry {
        let n = n + self.offset;
        let blocks = self.clock.blocks();
        let (block, loop_in_block, block_loops, tail) = match self.clock.pos(n) {
            SongPos::Block { idx, loop_in_block } => (idx, loop_in_block, self.clock.block_loops(idx), false),
            SongPos::Tail { loop_in_tail } => (blocks - 1, loop_in_tail, self.clock.tail_loops(), true),
            SongPos::Done => (blocks - 1, 0, 0, true),
        };
        SongTelemetry {
            block,
            blocks,
            label: if tail { "tail".into() } else { self.labels[block].clone() },
            loop_in_block,
            block_loops,
            loop_in_song: n,
            total_loops: self.clock.total_loops(),
            tail,
            seam,
            elapsed_s: n as f64 * self.loop_s,
            total_s: self.clock.total_loops() as f64 * self.loop_s,
        }
    }
}

/// One row of the dry-run plan.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BlockPlan {
    pub idx: usize,
    pub label: String,
    pub matrix: Option<String>,
    pub loops: usize,
    pub start_loop: usize,
    pub start_s: f64,
    pub dur_s: f64,
    pub seed: u64,
    /// Route ids added / removed / changed vs the previous block (block 0: vs the base matrix).
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
    /// The block's control (ADR 0055), as authored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control: Option<BlockControl>,
}

/// The whole song laid out in time (for `engines-song --dry-run` and progress displays).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SongPlan {
    pub loop_s: f64,
    pub blocks: Vec<BlockPlan>,
    pub tail_loops: usize,
    pub total_loops: usize,
    pub total_s: f64,
    pub transition_s: f32,
}

fn route_diff(a: &BTreeMap<String, MatrixEntry>, b: &BTreeMap<String, MatrixEntry>) -> (Vec<String>, Vec<String>, Vec<String>) {
    let added = b.keys().filter(|k| !a.contains_key(*k)).cloned().collect();
    let removed = a.keys().filter(|k| !b.contains_key(*k)).cloned().collect();
    let changed = b
        .iter()
        .filter(|(k, e)| a.get(*k).is_some_and(|o| to_doc_value(o).ok() != to_doc_value(e).ok()))
        .map(|(k, _)| k.clone())
        .collect();
    (added, removed, changed)
}

/// Lay the song out: per-block start/duration/seed and the route changes at each seam.
pub fn plan(doc: &LoopDoc, spec: &SongSpec, session_seed: u64) -> Result<SongPlan, String> {
    let loop_s = loop_seconds(doc);
    let clock = SongClock::new(spec);
    let mut prev = doc.matrix.clone();
    let mut blocks = Vec::with_capacity(spec.sequence.len());
    for (i, b) in spec.sequence.iter().enumerate() {
        let m = materialize(doc, spec, i)?;
        let (added, removed, changed) = route_diff(&prev, &m.matrix);
        let start_loop = clock.block_start(i);
        blocks.push(BlockPlan {
            idx: i,
            label: spec.label(i),
            matrix: b.matrix.clone(),
            loops: b.loops as usize,
            start_loop,
            start_s: start_loop as f64 * loop_s,
            dur_s: b.loops as f64 * loop_s,
            seed: block_seed(session_seed, i, b.seed),
            added,
            removed,
            changed,
            control: b.control.clone(),
        });
        prev = m.matrix;
    }
    Ok(SongPlan {
        loop_s,
        blocks,
        tail_loops: clock.tail_loops(),
        total_loops: clock.total_loops(),
        total_s: clock.total_loops() as f64 * loop_s,
        transition_s: spec.transition_s,
    })
}

/// One stretch of a rendered song, in loops: a block, or the ring-out tail. A song is written one
/// file per segment (ADR 0043), so these are the cut points.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Segment {
    /// The block index (as the plan, `--loop-block K` and `--emit-blocks` number it); the tail is
    /// numbered after the last block.
    pub idx: usize,
    /// The block's label; `"tail"` for the tail.
    pub label: String,
    pub tail: bool,
    pub start_loop: usize,
    pub loops: usize,
}

impl SongPlan {
    /// The song's segments in play order — every block that has loops, then the tail when it has
    /// any. They tile `0..total_loops` with no gap or overlap.
    pub fn segments(&self) -> Vec<Segment> {
        let blocks = self.blocks.iter().filter(|b| b.loops > 0).map(|b| Segment { idx: b.idx, label: b.label.clone(), tail: false, start_loop: b.start_loop, loops: b.loops });
        let tail = (self.tail_loops > 0).then(|| Segment {
            idx: self.blocks.len(),
            label: "tail".into(),
            tail: true,
            start_loop: self.total_loops - self.tail_loops,
            loops: self.tail_loops,
        });
        blocks.chain(tail).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    use crate::test_support::song_doc;

    fn parse(v: Value) -> Result<LoopDoc, String> {
        LoopDoc::from_json_str(&v.to_string())
    }

    fn spec_of(v: Value) -> SongSpec {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn merge_patch_follows_rfc_7386() {
        let mut t = DocValue::from(json!({ "a": 1, "b": { "c": 2, "d": 3 }, "e": [1, 2] }));
        merge_patch(&mut t, &json!({ "a": null, "b": { "c": 9, "x": { "y": 1 } }, "e": [3], "f": "new" }).into());
        assert_eq!(t, json!({ "b": { "c": 9, "d": 3, "x": { "y": 1 } }, "e": [3], "f": "new" }));
        // Non-object patch replaces; object patch onto a scalar starts from {}.
        let mut s = DocValue::from(json!(5));
        merge_patch(&mut s, &json!({ "k": 1, "gone": null }).into());
        assert_eq!(s, json!({ "k": 1 }));
        let mut r = DocValue::from(json!({ "k": 1 }));
        merge_patch(&mut r, &json!("x").into());
        assert_eq!(r, json!("x"));
    }

    #[test]
    fn materialize_layers_patch_remove_and_add_routes() {
        let doc = parse(song_doc(json!({
            "matrices": {
                "thin": { "m_pad": null, "m_wob": { "depth_oct": 3.322, "depth_slew_ms": 200 } },
                "more": { "m_wob2": { "source": "wob", "dest": "instruments.drone.params.level_db", "kind": "mod", "depth_db": -2.5 } }
            },
            "sequence": [ { "matrix": "thin", "loops": 4 }, { "loops": 4 }, { "matrix": "more", "loops": 4 } ],
            "transition_s": 2
        })))
        .unwrap();
        let spec = doc.song.clone().unwrap();
        let thin = materialize(&doc, &spec, 0).unwrap();
        assert!(thin.song.is_none(), "block docs carry no song");
        assert!(!thin.matrix.contains_key("m_pad"));
        let wob = &thin.matrix["m_wob"];
        assert_eq!((wob.depth, wob.depth_slew_ms, wob.id.as_str()), (3.322, 200.0, "m_wob"));
        assert_eq!((wob.depth_key(), wob.dest.as_str()), ("depth_oct", "instruments.pad.params.cutoff_hz"), "untouched fields survive a partial patch");
        let base = materialize(&doc, &spec, 1).unwrap();
        assert_eq!(base.matrix.keys().collect::<Vec<_>>(), doc.matrix.keys().collect::<Vec<_>>(), "no layer ⇒ the base matrix");
        assert_eq!(base.matrix["m_wob"].depth, 1.329);
        let more = materialize(&doc, &spec, 2).unwrap();
        assert_eq!(more.matrix.len(), 4);
        assert_eq!(more.matrix["m_wob2"].id, "m_wob2");
        // The doc everything else sees is untouched.
        assert_eq!(doc.matrix.len(), 3);
    }

    fn err_of(song: Value) -> String {
        parse(song_doc(song)).expect_err("expected a validation error")
    }

    #[test]
    fn validation_errors_name_the_culprit() {
        let e = err_of(json!({ "sequence": [] }));
        assert!(e.contains("empty"), "{e}");
        let e = err_of(json!({ "sequence": [ { "loops": 0 } ] }));
        assert!(e.contains("loops must be ≥ 1"), "{e}");
        let e = err_of(json!({ "sequence": [ { "matrix": "nope", "loops": 2 } ] }));
        assert!(e.contains("unknown matrix layer \"nope\""), "{e}");
        let e = err_of(json!({ "sequence": [ { "loops": 2 } ], "transition_s": -1 }));
        assert!(e.contains("transition_s"), "{e}");
        // 2 loops × 4 s = 8 s < 10 s
        let e = err_of(json!({ "sequence": [ { "loops": 2 }, { "loops": 9 } ], "transition_s": 10 }));
        assert!(e.contains("exceeds the shortest block") && e.contains("block 0"), "{e}");
        let e = err_of(json!({ "matrices": { "x": { "m_new": { "source": "wob", "kind": "mod" } } }, "sequence": [ { "matrix": "x", "loops": 2 } ] }));
        assert!(e.contains("\"x\"") && e.contains("m_new") && e.contains("missing \"dest\""), "{e}");
        let e = err_of(json!({ "matrices": { "x": { "m_wob": { "dept": 0.3 } } }, "sequence": [ { "matrix": "x", "loops": 2 } ] }));
        assert!(e.contains("unknown field \"dept\""), "{e}");
        let e = err_of(json!({ "matrices": { "x": { "m_wob": 0.3 } }, "sequence": [ { "matrix": "x", "loops": 2 } ] }));
        assert!(e.contains("must be an object"), "{e}");
        let e = err_of(json!({ "matrices": { "x": { "m_wob": { "depth": "deep" } } }, "sequence": [ { "matrix": "x", "loops": 2 } ] }));
        assert!(e.contains("m_wob"), "{e}");
        let e = err_of(json!({ "matrices": { "x": { "m_wob": { "source": "ghost" } } }, "sequence": [ { "matrix": "x", "loops": 2 } ] }));
        assert!(e.contains("block 0 (x)") && e.contains("m_wob") && e.contains("\"ghost\""), "{e}");
        let e = err_of(json!({ "matrices": { "x": { "m_wob": { "dest": "instruments.nope.params.cutoff_hz" } } }, "sequence": [ { "matrix": "x", "loops": 2 } ] }));
        assert!(e.contains("instrument \"nope\""), "{e}");
        let e = err_of(json!({ "matrices": { "x": { "m_wob": { "dest": "nowhere.at.all" } } }, "sequence": [ { "matrix": "x", "loops": 2 } ] }));
        assert!(e.contains("not a recognised JSONPath"), "{e}");
        let e = err_of(json!({ "matrices": { "x": { "m_pad": { "kind": "mod" } } }, "sequence": [ { "matrix": "x", "loops": 2 } ] }));
        assert!(e.contains("can't target notes"), "{e}");
    }

    #[test]
    fn blocks_are_matrix_only() {
        // A block (or the song) carrying anything but its schema is rejected at parse time.
        let e = err_of(json!({ "sequence": [ { "loops": 2, "instruments": { "pad": {} } } ] }));
        assert!(e.contains("unknown key `instruments`"), "{e}");
        let e = err_of(json!({ "harmony": { "root": "C" }, "sequence": [ { "loops": 2 } ] }));
        assert!(e.contains("unknown key `harmony`"), "{e}");
    }

    #[test]
    fn json5_song_parses_with_null_removals() {
        let mut s = song_doc(json!(null)).to_string();
        s = s.replace(
            "\"song\":null",
            "song: { matrices: { quiet: { m_drone: null, /* gone */ }, }, sequence: [ { matrix: 'quiet', loops: 3, }, { loops: 2, seed: 7 }, ], tail_loops: 2, transition_s: 1.5, }",
        );
        let doc = LoopDoc::from_json_str(&s).unwrap();
        let spec = doc.song.as_ref().unwrap();
        assert_eq!(spec.sequence.len(), 2);
        assert_eq!(spec.matrices["quiet"]["m_drone"], DocValue::Null);
        assert_eq!((spec.tail_loops, spec.transition_s), (2, 1.5));
    }

    #[test]
    fn defaults_and_lints() {
        let doc = parse(song_doc(json!({
            "matrices": { "unused": {}, "x": { "m_ghost": null } },
            "sequence": [ { "matrix": "x", "loops": 3 } ],
            "tail_loops": 0
        })))
        .unwrap();
        let spec = doc.song.as_ref().unwrap();
        assert_eq!(spec.transition_s, 8.0, "default seam");
        let lints = doc.lint().join("\n");
        assert!(lints.contains("\"unused\" is never used"), "{lints}");
        assert!(lints.contains("m_ghost") && lints.contains("no-op"), "{lints}");
        assert!(lints.contains("tail_loops 0"), "{lints}");
        let quiet = parse(song_doc(json!({ "sequence": [ { "loops": 3 } ] }))).unwrap();
        assert!(quiet.lint().is_empty(), "{:?}", quiet.lint());
        assert_eq!(quiet.song.as_ref().unwrap().tail_loops, 1, "default tail");
    }

    #[test]
    fn lint_flags_a_removed_route_pair_and_the_tail() {
        // Two sources feed `pad`; removing just one still releases that source's voices.
        let mut d = song_doc(json!({ "matrices": { "x": { "m_pad2": null } }, "sequence": [ { "loops": 3 }, { "matrix": "x", "loops": 3 } ], "transition_s": 1, "tail_loops": 0 }));
        d["matrix"]["m_pad2"] = json!({ "source": "root", "dest": "instruments.pad.notes", "kind": "note" });
        d["instruments"]["pad"]["params"]["release_s"] = json!(0.0);
        let lints = parse(d.clone()).unwrap().lint().join("\n");
        assert!(lints.contains("\"pad\"") && lints.contains("\"root\"") && lints.contains("entering x"), "{lints}");
        // The tail drops every note route: a held zero-release voice cuts off there too.
        let mut d = song_doc(json!({ "sequence": [ { "loops": 3 } ], "tail_loops": 1 }));
        d["instruments"]["drone"]["params"]["release_s"] = json!(0.0);
        let lints = parse(d.clone()).unwrap().lint().join("\n");
        assert!(lints.contains("\"drone\"") && lints.contains("entering the tail"), "{lints}");
        d["song"]["tail_loops"] = json!(0);
        assert!(!parse(d).unwrap().lint().join("\n").contains("zero release"), "no tail ⇒ no tail transition");
    }

    #[test]
    fn non_add_modes_are_rejected() {
        let e = err_of(json!({ "matrices": { "x": { "m_wob": { "mode": "multiply" } } }, "sequence": [ { "matrix": "x", "loops": 2 } ] }));
        assert!(e.contains("m_wob") && e.contains("mode \"multiply\""), "{e}");
    }

    #[test]
    fn lint_flags_a_hard_cut_at_a_seam() {
        let mut d = song_doc(json!({ "matrices": { "x": { "m_drone": null } }, "sequence": [ { "loops": 3 }, { "matrix": "x", "loops": 3 } ], "transition_s": 1 }));
        d["instruments"]["drone"]["params"]["release_s"] = json!(0.0);
        let lints = parse(d).unwrap().lint().join("\n");
        assert!(lints.contains("\"drone\"") && lints.contains("zero release"), "{lints}");
    }

    #[test]
    fn clock_maps_loops_to_blocks_and_tail() {
        let spec = spec_of(json!({ "sequence": [ { "loops": 3 }, { "loops": 1 }, { "loops": 2 } ], "tail_loops": 2 }));
        let c = SongClock::new(&spec);
        assert_eq!((c.total_loops(), c.body_loops(), c.blocks()), (8, 6, 3));
        let got: Vec<SongPos> = (0..9).map(|n| c.pos(n)).collect();
        use SongPos::*;
        assert_eq!(
            got,
            vec![
                Block { idx: 0, loop_in_block: 0 },
                Block { idx: 0, loop_in_block: 1 },
                Block { idx: 0, loop_in_block: 2 },
                Block { idx: 1, loop_in_block: 0 },
                Block { idx: 2, loop_in_block: 0 },
                Block { idx: 2, loop_in_block: 1 },
                Tail { loop_in_tail: 0 },
                Tail { loop_in_tail: 1 },
                Done,
            ]
        );
        let starts: Vec<usize> = (0..9).filter(|&n| c.is_segment_start(n)).collect();
        assert_eq!(starts, vec![0, 3, 4, 6]);
        assert_eq!((c.block_start(2), c.block_loops(1)), (4, 1));
    }

    #[test]
    fn block_seeds() {
        assert_eq!(block_seed(0x5EED, 0, None), 0x5EED, "block 0 keeps the session seed (plain-session parity)");
        let s1 = block_seed(0x5EED, 1, None);
        let s2 = block_seed(0x5EED, 2, None);
        assert!(s1 != 0x5EED && s1 != s2);
        assert_eq!(s1, block_seed(0x5EED, 1, None), "deterministic");
        assert_ne!(s1, block_seed(0x5EEE, 1, None), "the session seed varies every block");
        // Explicit seeds: equal ⇒ same voyage regardless of position; still session-dependent.
        assert_eq!(block_seed(0x5EED, 0, Some(7)), block_seed(0x5EED, 3, Some(7)));
        assert_ne!(block_seed(0x5EED, 3, Some(7)), block_seed(0x5EED, 3, Some(8)));
        assert_ne!(block_seed(0x5EED, 3, Some(7)), block_seed(1, 3, Some(7)));
    }

    #[test]
    fn plan_lays_out_time_and_route_changes() {
        let doc = parse(song_doc(json!({
            "matrices": { "thin": { "m_pad": null, "m_wob": { "depth_oct": 2.658 } } },
            "sequence": [ { "loops": 10, "label": "intro" }, { "matrix": "thin", "loops": 5 }, { "loops": 10, "seed": 3 } ],
            "tail_loops": 2, "transition_s": 4
        })))
        .unwrap();
        let p = plan(&doc, doc.song.as_ref().unwrap(), 0x5EED).unwrap();
        assert_eq!(p.loop_s, 4.0);
        assert_eq!((p.total_loops, p.total_s), (27, 108.0));
        assert_eq!(p.blocks[0].label, "intro");
        assert_eq!(p.blocks[1].label, "thin");
        assert_eq!(p.blocks[2].label, "base");
        assert_eq!((p.blocks[1].start_loop, p.blocks[1].start_s, p.blocks[1].dur_s), (10, 40.0, 20.0));
        assert!(p.blocks[0].added.is_empty() && p.blocks[0].removed.is_empty());
        assert_eq!((p.blocks[1].removed.clone(), p.blocks[1].changed.clone()), (vec!["m_pad".to_string()], vec!["m_wob".to_string()]));
        assert_eq!((p.blocks[2].added.clone(), p.blocks[2].changed.clone()), (vec!["m_pad".to_string()], vec!["m_wob".to_string()]));
        assert_eq!(p.blocks[0].seed, 0x5EED);
        assert_eq!(p.blocks[2].seed, block_seed(0x5EED, 2, Some(3)));
        assert_eq!(fmt_hms(p.total_s), "0:01:48");
        assert_eq!(fmt_hms(5.0 * 3600.0 + 29.0 * 60.0 + 41.4), "5:29:41");
    }

    #[test]
    fn segments_tile_the_song_blocks_then_tail() {
        let doc = parse(song_doc(json!({
            "matrices": { "thin": { "m_pad": null } },
            "sequence": [ { "loops": 3, "label": "intro" }, { "matrix": "thin", "loops": 2 }, { "loops": 4 } ],
            "tail_loops": 2
        })))
        .unwrap();
        let p = plan(&doc, doc.song.as_ref().unwrap(), 0).unwrap();
        let seg = p.segments();
        fn row(s: &Segment) -> (usize, &str, bool, usize, usize) {
            (s.idx, s.label.as_str(), s.tail, s.start_loop, s.loops)
        }
        assert_eq!(seg.iter().map(row).collect::<Vec<_>>(), [(0, "intro", false, 0, 3), (1, "thin", false, 3, 2), (2, "base", false, 5, 4), (3, "tail", true, 9, 2)]);
        assert!(seg.windows(2).all(|w| w[0].start_loop + w[0].loops == w[1].start_loop), "no gap, no overlap");
        assert_eq!(seg.last().map(|s| s.start_loop + s.loops), Some(p.total_loops));
        // No tail loops ⇒ no tail segment.
        let doc = parse(song_doc(json!({ "sequence": [ { "loops": 2 } ], "tail_loops": 0 }))).unwrap();
        let seg = plan(&doc, doc.song.as_ref().unwrap(), 0).unwrap().segments();
        assert_eq!(seg.iter().map(row).collect::<Vec<_>>(), [(0, "base", false, 0, 2)]);
    }

    #[test]
    fn the_templates_commented_song_example_is_valid() {
        // The annotated template documents a `song` in comments; un-comment it and parse, so the
        // example can never rot against the real schema/validation.
        let t = crate::contract::LOOP_TEMPLATE_JSON5;
        let lines: Vec<&str> = t.lines().collect();
        let start = lines.iter().position(|l| l.trim_start().starts_with("//   \"song\": {")).expect("template documents a song");
        let end = start + lines[start..].iter().position(|l| l.trim_end() == "    //   }").expect("song example closes");
        let body: String = lines[start..=end].iter().map(|l| l.trim_start().trim_start_matches("//")).collect::<Vec<_>>().join("\n");
        let close = t.rfind('}').unwrap();
        let doc_src = format!("{}{body}\n}}", &t[..close]);
        let doc = LoopDoc::from_json_str(&doc_src).unwrap_or_else(|e| panic!("template song example: {e}"));
        let spec = doc.song.as_ref().unwrap();
        assert_eq!(spec.sequence.len(), 4);
        // The generic template's voices author no `release`, so the zero-release advice (tail /
        // seam hard cuts) is expected; nothing else may fire.
        assert!(doc.lint().iter().all(|w| !w.starts_with("song:") || w.contains("zero release")), "{:?}", doc.lint());
        // ≈ 5 h, as the comment claims.
        let p = plan(&doc, spec, 0).unwrap();
        assert!((4.5 * 3600.0..5.5 * 3600.0).contains(&p.total_s), "{}", fmt_hms(p.total_s));
        // The template itself (the parsed, un-modified doc) carries no song.
        assert!(LoopDoc::from_json_str(t).unwrap().song.is_none());
    }

    #[test]
    fn controller_never_carries_the_song() {
        let doc = parse(song_doc(json!({ "sequence": [ { "loops": 2 } ] }))).unwrap();
        assert!(doc.song.is_some());
        let mut ctl = crate::controller::Controller::new(doc.clone(), Default::default());
        assert!(ctl.operating_point().song.is_none());
        ctl.swap_doc(doc);
        assert!(ctl.operating_point().song.is_none());
    }

    // ── ADR 0055: per-block control ──

    fn control_doc(control: Value) -> Value {
        song_doc(json!({ "sequence": [ { "loops": 2 }, { "loops": 2, "label": "deep", "control": control } ] }))
    }

    #[test]
    fn block_control_parses_and_resolves_to_bands() {
        let doc = parse(control_doc(json!({
            "movement": { "orrery": false, "phrase_loops": 4 },
            "optimize": "hold",
            "max_drift": 0.05,
            "max_change": 0.02,
            "drift": {
                "instruments.pad.params.cutoff_hz": "hold",
                "instruments.pad.params.level_db": { "min": -10.46, "max": -4.44 },
                "instruments.drone.params.level_db": { "max_drift": 0.1 }
            }
        })))
        .unwrap();
        let spec = doc.song.as_ref().unwrap();
        let ctl = spec.sequence[1].control.as_ref().unwrap();
        assert_eq!(ctl.orrery(), Some(false));
        assert!(spec.sequence[0].control.is_none());
        let p = resolve_control(&doc, ctl);
        assert_eq!(p.optimize, Some(OptimizeMode::Hold));
        assert_eq!(p.max_change, Some(0.02));
        let band = |k: &str| p.constraints[k];
        assert_eq!(band("instruments.pad.params.cutoff_hz"), ConstraintBand { min: Some(800.0), max: Some(800.0) }, "hold = the authored value");
        assert_eq!(band("instruments.pad.params.level_db"), ConstraintBand { min: Some(-10.46), max: Some(-4.44) });
        // A `max_drift` is normalized: ±0.1 of the dB curve around the authored −7.96 dB.
        let path = "instruments.drone.params.level_db";
        let (n, d) = (param_norm(path, -7.96), band(path));
        assert!((d.min.unwrap() - param_denorm(path, n - 0.1)).abs() < 1e-4 && (d.max.unwrap() - param_denorm(path, n + 0.1)).abs() < 1e-4, "authored -7.96 dB ± 0.1: {d:?}");
        let s = ctl.summary();
        assert!(s.contains("orrery off") && s.contains("phrase_loops 4") && s.contains("hold") && s.contains("pad.cutoff_hz hold") && s.contains("drone.level_db ±0.1"), "{s}");
    }

    #[test]
    fn a_hold_starts_at_the_safe_clamped_value() {
        // Authored above an explicit safe ceiling: the controller starts it clamped, and so does the pin.
        let mut v = control_doc(json!({ "drift": { "instruments.pad.params.cutoff_hz": "hold" } }));
        v["safe_bounds"] = json!({ "instruments.pad.params.cutoff_hz": { "max": 400 } });
        let doc = parse(v).unwrap();
        let p = resolve_control(&doc, doc.song.as_ref().unwrap().sequence[1].control.as_ref().unwrap());
        let b = p.constraints["instruments.pad.params.cutoff_hz"];
        assert_eq!((b.min, b.max), (Some(400.0), Some(400.0)));
    }

    #[test]
    fn materialize_writes_the_block_control_into_the_doc() {
        let mut v = control_doc(json!({ "movement": { "orrery": false }, "max_drift": 0.05, "max_change": 0.02, "drift": { "instruments.pad.params.cutoff_hz": "hold" } }));
        v["movement"] = json!({ "orrery": true, "phrase_loops": 2 });
        v["constraints"] = json!({ "instruments.drone.params.level_db": { "min": -20 } });
        let doc = parse(v).unwrap();
        let spec = doc.song.as_ref().unwrap();
        let plain = materialize(&doc, spec, 0).unwrap();
        assert_eq!(plain.movement.as_ref().map(|m| m.orrery), Some(true), "a block without control keeps the doc's");
        assert_eq!((plain.max_drift, plain.max_change), (None, None));
        let deep = materialize(&doc, spec, 1).unwrap();
        let m = deep.movement.as_ref().unwrap();
        assert_eq!((m.orrery, m.phrase_loops), (false, 2), "merged field by field");
        assert_eq!((deep.max_drift, deep.max_change), (Some(0.05), Some(0.02)));
        assert_eq!(deep.constraints.len(), 2, "the session band stays, the block's joins it");
        // Playing the block on its own (`--loop-block`) hears its control.
        let c = crate::contract::SessionControl::from_doc(&deep).unwrap();
        assert!(!c.movement.orrery);
        assert_eq!(c.constraints["instruments.pad.params.cutoff_hz"].min, Some(800.0));
    }

    fn diags_of(v: Value) -> Vec<crate::check::DocDiag> {
        crate::check::check_str(&v.to_string()).diags
    }

    fn error_at(control: Value, path: &str) -> String {
        let d = diags_of(control_doc(control));
        d.iter().find(|d| d.is_error() && d.path == path).map(|d| d.to_string()).unwrap_or_else(|| panic!("no error at {path}: {d:?}"))
    }

    #[test]
    fn block_control_errors_are_located() {
        let at = "song.sequence[1].control";
        let e = error_at(json!({ "optimize": "live_edit" }), &format!("{at}.optimize"));
        assert!(e.contains("\"hold\" or \"drift\""), "{e}");
        let e = error_at(json!({ "drift": { "instruments.pad.params.cutof_hz": "hold" } }), &format!("{at}.drift.instruments.pad.params.cutof_hz"));
        assert!(e.contains("song block 1 (deep)") && e.contains("not a drift candidate") && e.contains("did you mean `instruments.pad.params.cutoff_hz`"), "{e}");
        let e = error_at(json!({ "drift": { "instruments.pad.params.wave": "hold" } }), &format!("{at}.drift.instruments.pad.params.wave"));
        assert!(e.contains("no numeric param"), "{e}");
        let e = error_at(json!({ "drift": { "instruments.pad.params.level_db": { "min": -4.44, "max": -10.46 } } }), &format!("{at}.drift.instruments.pad.params.level_db"));
        assert!(e.contains("above max"), "{e}");
        let e = error_at(json!({ "drift": { "instruments.pad.params.level_db": { "max": 12 } } }), &format!("{at}.drift.instruments.pad.params.level_db.max"));
        assert!(e.contains("outside"), "{e}");
        let e = error_at(json!({ "drift": { "instruments.pad.params.level_db": { "max": -6, "max_drift": 0.1 } } }), &format!("{at}.drift.instruments.pad.params.level_db"));
        assert!(e.contains("not both"), "{e}");
        let e = error_at(json!({ "drift": { "instruments.pad.params.level_db": {} } }), &format!("{at}.drift.instruments.pad.params.level_db"));
        assert!(e.contains("empty"), "{e}");
        // The schema refuses what serde would: an unknown key, a word that isn't "hold", a bad range.
        let e = error_at(json!({ "movment": { "orrery": false } }), &format!("{at}.movment"));
        assert!(e.contains("did you mean `movement`"), "{e}");
        error_at(json!({ "movement": { "orery": false } }), &format!("{at}.movement.orery"));
        error_at(json!({ "drift": { "instruments.pad.params.level_db": "freeze" } }), &format!("{at}.drift.instruments.pad.params.level_db"));
        error_at(json!({ "max_drift": 0 }), &format!("{at}.max_drift"));
        // A dither_disable instrument never drifts, so a hold on it is a mistake.
        let mut v = control_doc(json!({ "drift": { "instruments.drone.params.level_db": "hold" } }));
        v["instruments"]["drone"]["dither_disable"] = json!(true);
        let d = diags_of(v);
        assert!(d.iter().any(|d| d.is_error() && d.message.contains("dither_disable")), "{d:?}");
        // A safe_bounds band the override can't reach.
        let mut v = control_doc(json!({ "drift": { "instruments.pad.params.cutoff_hz": { "min": 5000 } } }));
        v["safe_bounds"] = json!({ "instruments.pad.params.cutoff_hz": { "max": 2000 } });
        let d = diags_of(v);
        assert!(d.iter().any(|d| d.is_error() && d.message.contains("misses")), "{d:?}");
        // A good control is clean, and the doc loads.
        let d = diags_of(control_doc(json!({ "drift": { "instruments.pad.params.cutoff_hz": "hold", "matrix.m_wob.depth_oct": { "max_drift": 0.05 } } })));
        assert!(!d.iter().any(|d| d.is_error()), "{d:?}");
    }

    #[test]
    fn block_control_lints() {
        let lints = |control: Value, orrery: bool| {
            let mut v = control_doc(control);
            if orrery {
                v["movement"] = json!({ "orrery": true });
            }
            parse(v).unwrap().lint().join("\n")
        };
        let l = lints(json!({ "movement": { "orrery": false, "phrase_loops": 8 } }), true);
        assert!(l.contains("turns the Orrery off, so phrase_loops do nothing"), "{l}");
        let l = lints(json!({ "movement": { "orrery": false } }), false);
        assert!(l.contains("never on in this doc"), "{l}");
        assert!(!lints(json!({ "movement": { "orrery": false } }), true).contains("song block"), "Orrery off in an Orrery doc is the point");
        let l = lints(json!({ "optimize": "hold", "drift": { "instruments.pad.params.cutoff_hz": "hold" } }), false);
        assert!(l.contains("the block holds"), "{l}");
        let l = lints(json!({ "drift": { "instruments.pad.params.level_db": { "min": -3 } } }), false);
        assert!(l.contains("authored -6.02, outside its band"), "{l}");
    }

    #[test]
    fn the_plan_carries_each_blocks_control() {
        let doc = parse(control_doc(json!({ "movement": { "orrery": false } }))).unwrap();
        let p = plan(&doc, doc.song.as_ref().unwrap(), 1).unwrap();
        assert!(p.blocks[0].control.is_none());
        assert_eq!(p.blocks[1].control.as_ref().and_then(BlockControl::orrery), Some(false));
        let j = serde_json::to_value(&p).unwrap();
        assert!(j["blocks"][0].get("control").is_none());
        assert_eq!(j["blocks"][1]["control"]["movement"]["orrery"], json!(false));
    }

    #[test]
    fn json5_block_control_parses() {
        let mut s = song_doc(json!(null)).to_string();
        s = s.replace(
            "\"song\":null",
            "song: { sequence: [ { loops: 2 }, { loops: 2, control: { movement: { orrery: false, }, drift: { 'instruments.pad.params.cutoff_hz': 'hold', }, }, }, ], }",
        );
        let doc = LoopDoc::from_json_str(&s).unwrap();
        let ctl = doc.song.as_ref().unwrap().sequence[1].control.as_ref().unwrap();
        assert_eq!(ctl.drift["instruments.pad.params.cutoff_hz"], DriftOverride::Word("hold".into()));
    }

    #[test]
    fn a_doc_without_a_song_serializes_without_one() {
        let mut v = song_doc(json!(null));
        v.as_object_mut().unwrap().remove("song");
        let doc = parse(v).unwrap();
        assert!(doc.song.is_none());
        assert!(serde_json::to_value(&doc).unwrap().get("song").is_none());
    }
}
