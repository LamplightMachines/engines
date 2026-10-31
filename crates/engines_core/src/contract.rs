//! The runtime control plane (objectives_v0.1): *how* the controller drives the music — the
//! `optimize` × `movement` axes, Tier-3 constraint bands, a live overlay, and guardrails — plus
//! the committed JSON5 templates. Mirrors `contracts/objectives_v0.1.schema.json`. `BTreeMap` is
//! deliberate — deterministic key ordering ⇒ reproducible serialization (CE-R7.3).

use serde::{Deserialize, Serialize};
use crate::doc_value::{from_doc_value, to_doc_value, DocValue, Map};
use std::collections::BTreeMap;

pub type CanonicalPath = String; // e.g. "instruments.poly.params.cutoff_hz"

// ── Session control input (CE-R8.2; objectives_v0.1) ────────────────
/// The runtime control plane — *how* the controller drives the music. Two orthogonal axes
/// (`optimize` × `movement`) plus guardrails, Tier-3 constraint bands, and a live loop-doc overlay.
/// Standing & complete; editing replaces it and bumps `revision` (re-base on change). Mirrors
/// `contracts/objectives_v0.1.schema.json` (ADR 0010). Unknown fields (e.g. a pre-ADR-0031
/// `objective`) are ignored, so older docs still load.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionControl {
    #[serde(default = "default_objectives_schema")]
    pub schema_version: String,
    /// Bumped on every applied edit; recorded in each loop's telemetry / `LoopRecord`.
    #[serde(default)]
    pub revision: u64,
    /// Timbre axis: freeze, drift, or hand-authoring (CE-R8.1).
    #[serde(default)]
    pub optimize: OptimizeMode,
    /// Musical-movement axis: static, or generative Orrery harmony.
    #[serde(default)]
    pub movement: Movement,
    /// Tier-3 runtime constraint bands (native units), JSONPath-keyed. Parsed in Phase 0; the
    /// "drive into range" attractor behavior is wired in Phase 1.
    #[serde(default)]
    pub constraints: BTreeMap<CanonicalPath, crate::loop_doc::ConstraintBand>,
    /// Live loop-doc overlay (JSONPath → value). Parsed in Phase 0; applied in Phase 1.
    #[serde(default)]
    pub overrides: BTreeMap<CanonicalPath, DocValue>,
    #[serde(default)]
    pub guardrails: Guardrails,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta_notes: Option<String>,
}
fn default_objectives_schema() -> String {
    "objectives_v0.1".to_string()
}
impl Default for SessionControl {
    fn default() -> Self {
        Self {
            schema_version: default_objectives_schema(),
            revision: 0,
            optimize: OptimizeMode::Drift,
            movement: Movement::default(),
            constraints: BTreeMap::new(),
            overrides: BTreeMap::new(),
            guardrails: Guardrails::default(),
            meta_notes: None,
        }
    }
}

/// Timbre axis (CE-R8.1): how the controller moves the operating point's params loop to loop.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptimizeMode {
    /// Freeze at the authored values (the engine's free modulators still breathe).
    Hold,
    /// Bounded random walk with directional momentum — organic variation over hours (the default).
    #[default]
    Drift,
    /// Author mode (live-edit): no drift; the loop doc is swapped wholesale from a file watcher /
    /// editor. Free modulators and the Orrery still move. Switchable back to drift in-session.
    LiveEdit,
}

/// Musical-movement axis: whether the generative **Orrery** harmonic layer is active, plus its
/// config (de-hardcodes the former `OrreryConfig::default()`): binding, phrase length, the tension
/// band, the voice-change governor, return-home scope, and optional Arranger tuning.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Movement {
    #[serde(default)]
    pub orrery: bool,
    #[serde(default)]
    pub bind: OrreryBind,
    #[serde(default = "default_phrase_loops")]
    pub phrase_loops: u64,
    #[serde(default = "default_tension_target")]
    pub tension_target: f32,
    #[serde(default = "default_tension_max")]
    pub tension_max: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_changes_per_loop: Option<Range01>,
    #[serde(default)]
    pub home: HomeScope,
    /// Optional Arranger tuning (Phase 6d). Absent ⇒ today's built-in constants ⇒ byte-identical render.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrange: Option<ArrangeCfg>,
}
impl Default for Movement {
    fn default() -> Self {
        Self {
            orrery: false,
            bind: OrreryBind::default(),
            phrase_loops: default_phrase_loops(),
            tension_target: default_tension_target(),
            tension_max: default_tension_max(),
            voice_changes_per_loop: None,
            home: HomeScope::default(),
            arrange: None,
        }
    }
}

/// Optional exposure of the Arranger's internals on the movement axis (Phase 6d / ADR 0021). Every
/// field defaults to the engine's built-in constant, so an absent or partial `arrange` block renders
/// byte-identically to before. The full per-track hazard table stays internal (tracked in the backlog).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArrangeCfg {
    /// Arp euclid-density band as a **fraction of the arp's authored length** (`min`/`max` in 0..1;
    /// default 0.12..0.70). The Arranger interpolates pulses across `[min·len, max·len]` by its
    /// energy/tension drive — widen for more dynamic range, raise `min` for a busier floor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arp_density: Option<Range01>,
    /// Multiplier on every return-home hazard (chord + per-track), default 1.0; `>1` pulls home sooner,
    /// `<1` lets the harmony wander longer. Applied on top of the Arranger's build/release `return_scale`.
    #[serde(default = "default_return_hazard_scale")]
    pub return_hazard_scale: f32,
}
impl Default for ArrangeCfg {
    fn default() -> Self {
        Self { arp_density: None, return_hazard_scale: 1.0 }
    }
}
fn default_return_hazard_scale() -> f32 {
    1.0
}
impl Movement {
    pub fn is_orrery(&self) -> bool {
        self.orrery
    }
}
fn default_phrase_loops() -> u64 {
    1
}
fn default_tension_target() -> f32 {
    0.32
}
fn default_tension_max() -> f32 {
    0.7
}

/// Which note modulators Orrery drives (by id). Defaults match the demo fixtures; Phase 4 adds
/// role-based binding via `modulators.<id>.orrery_role`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OrreryBind {
    #[serde(default = "default_chord_id")]
    pub chord: String,
    #[serde(default = "default_bass_id")]
    pub bass: String,
    #[serde(default = "default_arp_id")]
    pub arp: String,
    #[serde(default = "default_oneshot_ids")]
    pub oneshot: Vec<String>,
}
impl Default for OrreryBind {
    fn default() -> Self {
        Self { chord: default_chord_id(), bass: default_bass_id(), arp: default_arp_id(), oneshot: default_oneshot_ids() }
    }
}
fn default_chord_id() -> String {
    "chords".to_string()
}
fn default_bass_id() -> String {
    "bass_line".to_string()
}
fn default_arp_id() -> String {
    "arp_shimmer".to_string()
}
fn default_oneshot_ids() -> Vec<String> {
    vec!["event".to_string()]
}

/// Return-home scope (Phase 5): the whole doc, or per-modulator. **Defaults to `per_modulator`**
/// (ADR 0017) — the richer path: per-track organic returns + leader-led arrangement, with the chord
/// resolving home through a cadence. `global` keeps one shared traversal (now with an organic,
/// tension-rising whole-voyage return on top of the hard `tension_max` breach).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HomeScope {
    Global,
    #[default]
    PerModulator,
}

/// A `[min,max]` band (used for the Orrery voice-change governor, Phase 5).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Range01 {
    pub min: f32,
    pub max: f32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Guardrails {
    pub max_change_per_loop: f32,
    /// Per-path ceilings and floors in the param's **native** units, as an objectives file, a doc's
    /// control or a `--ceiling`/`--floor` flag writes them (e.g. `shift_st` in semitones). The
    /// controller normalizes them through the seam when it bounds a path.
    #[serde(default)]
    pub ceilings: BTreeMap<CanonicalPath, f32>,
    #[serde(default)]
    pub floors: BTreeMap<CanonicalPath, f32>,
}

fn parse_pair(s: &str) -> Result<(String, f32), String> {
    let (k, v) = s.split_once('=').ok_or_else(|| format!("expected key=value, got '{s}'"))?;
    Ok((k.trim().to_string(), v.trim().parse().map_err(|e| format!("'{s}': {e}"))?))
}

/// Parse a TUI **constraint** line (Phase 6d) — whitespace-separated, native units:
///   `"path min max"` → set a `[min, max]` band (use `-` for an open end, e.g. `path 0.4 -`)
///   `"path"` or `"path -"` → **clear** the band (`Ok((path, None))`).
/// Reusable + testable, the parse-pattern sibling of [`parse_override`].
pub fn parse_constraint(s: &str) -> Result<(CanonicalPath, Option<crate::loop_doc::ConstraintBand>), String> {
    let mut it = s.split_whitespace();
    let path = it.next().ok_or_else(|| "expected: path [min max | -]".to_string())?.to_string();
    let bound = |t: &str| -> Result<Option<f32>, String> {
        if t == "-" { Ok(None) } else { t.parse().map(Some).map_err(|e| format!("'{t}': {e}")) }
    };
    let rest: Vec<&str> = it.collect();
    let band = match rest.as_slice() {
        [] | ["-"] => None,
        [min, max] => Some(crate::loop_doc::ConstraintBand { min: bound(min)?, max: bound(max)? }),
        _ => return Err(format!("expected: '{path} min max' or '{path} -' (got '{s}')")),
    };
    Ok((path, band))
}

/// Parse a TUI **override** line (Phase 6d): `"path value"` → set `path` to `value`, or `"path -"` →
/// **clear** the override (`Ok((path, None))`). `value` is read as JSON when it parses as such (number/
/// bool/null/array/object/quoted string), else taken as a bare string — most overrides are 0..1 numbers.
pub fn parse_override(s: &str) -> Result<(CanonicalPath, Option<DocValue>), String> {
    let (path, val) = s.split_once(char::is_whitespace).ok_or_else(|| "expected: path value".to_string())?;
    let val = val.trim();
    if val.is_empty() {
        return Err("expected: path value".to_string());
    }
    if val == "-" {
        return Ok((path.to_string(), None)); // clear
    }
    let value = serde_json::from_str::<serde_json::Value>(val).map_or_else(|_| DocValue::String(val.to_string()), DocValue::from);
    Ok((path.to_string(), Some(value)))
}

impl SessionControl {
    /// Build from CLI-style flags (CE-R12.2). `optimize` ∈ hold|drift|live_edit; `orrery` toggles
    /// the movement axis; `ceilings`/`floors` are `["path=val", …]` (native units). Returns `None`
    /// for plain `drift` with no guardrails/orrery (the default — no control needed).
    pub fn from_flags(optimize: &str, orrery: bool, ceilings: &[String], floors: &[String], max_change: f32) -> Result<Option<Self>, String> {
        let optimize = match optimize {
            "" | "drift" => OptimizeMode::Drift,
            "hold" => OptimizeMode::Hold,
            "live_edit" | "liveedit" => OptimizeMode::LiveEdit,
            "orrery" => return Err(
                "`orrery` is no longer an optimize mode — use --optimize <hold|drift> together with --orrery (the movement axis)".into(),
            ),
            "pursue" => return Err("`pursue` was removed with the CLAP learner (ADR 0031) — use hold or drift".into()),
            other => return Err(format!("unknown optimize mode '{other}' (hold|drift|live_edit)")),
        };
        if matches!(optimize, OptimizeMode::Drift) && !orrery && ceilings.is_empty() && floors.is_empty() {
            return Ok(None);
        }
        // Guardrail bounds stay in native units (e.g. `shift=12` semitones), like an objectives
        // file's; the controller normalizes them (`Controller::bounds`).
        let to_bounds = |xs: &[String]| xs.iter().map(|s| parse_pair(s)).collect::<Result<BTreeMap<_, _>, _>>();
        Ok(Some(SessionControl {
            schema_version: default_objectives_schema(),
            revision: 0,
            optimize,
            movement: Movement { orrery, ..Default::default() },
            constraints: BTreeMap::new(),
            overrides: BTreeMap::new(),
            guardrails: Guardrails {
                max_change_per_loop: max_change,
                ceilings: to_bounds(ceilings)?,
                floors: to_bounds(floors)?,
            },
            meta_notes: None,
        }))
    }

    /// The control a loop doc carries itself (ADR 0033): its `optimize` / `movement` / `constraints`
    /// fields, or `None` when it authors none of them (⇒ plain drift, the default).
    pub fn from_doc(doc: &crate::loop_doc::LoopDoc) -> Option<Self> {
        if doc.optimize.is_none() && doc.movement.is_none() && doc.constraints.is_empty() {
            return None;
        }
        Some(SessionControl {
            optimize: doc.optimize.unwrap_or_default(),
            movement: doc.movement.clone().unwrap_or_default(),
            constraints: doc.constraints.clone(),
            ..Default::default()
        })
    }

    /// Fold a live-edited doc's own `movement` / `constraints` (ADR 0033) into a running control,
    /// keeping its mode, guardrails and overlay (the player owns those) — returns the updated control
    /// with `revision` bumped, or `None` when the doc's settings didn't change.
    pub fn with_doc_settings(current: Option<&Self>, old_doc: &crate::loop_doc::LoopDoc, new_doc: &crate::loop_doc::LoopDoc) -> Option<Self> {
        if old_doc.movement == new_doc.movement && old_doc.constraints == new_doc.constraints {
            return None;
        }
        let mut c = current.cloned().unwrap_or_default();
        c.movement = new_doc.movement.clone().unwrap_or_default();
        c.constraints = new_doc.constraints.clone();
        c.revision += 1;
        Some(c)
    }

    /// Layer CLI flags over a base control (the doc's, or none): a non-empty `optimize` replaces the
    /// timbre axis, `orrery` switches the Orrery on (keeping the doc's movement config), and
    /// ceilings/floors (native units) add guardrails. With no flags the base passes through unchanged.
    pub fn with_flags(
        base: Option<Self>,
        optimize: &str,
        orrery: bool,
        ceilings: &[String],
        floors: &[String],
        max_change: f32,
    ) -> Result<Option<Self>, String> {
        let Some(mut c) = base else {
            return Self::from_flags(optimize, orrery, ceilings, floors, max_change);
        };
        if let Some(flagged) = Self::from_flags(optimize, orrery, ceilings, floors, max_change)? {
            if !optimize.is_empty() {
                c.optimize = flagged.optimize;
            }
            if orrery {
                c.movement.orrery = true;
            }
            c.guardrails.max_change_per_loop = max_change;
            c.guardrails.ceilings.extend(flagged.guardrails.ceilings);
            c.guardrails.floors.extend(flagged.guardrails.floors);
        }
        Ok(Some(c))
    }

    /// Parse an objectives/control doc from **JSON5** (comments + trailing commas, so the file can
    /// carry inline help/examples). JSON5 is a superset of JSON, so plain `.json` parses unchanged
    /// (ADR 0023). The error is a message string (callers format it via Display/Debug).
    pub fn from_json_str(s: &str) -> Result<Self, String> {
        json5::from_str(s).map_err(|e| e.to_string())
    }
}

/// A song block's `control` resolved for the controller (ADR 0055): a patch over the standing session
/// control. `song::resolve_control` builds it from the block's authored `control` (its `drift` entries
/// already turned into native `constraints` bands; its `max_drift` lives in the block doc instead).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ControlPatch {
    pub optimize: Option<OptimizeMode>,
    /// A partial `movement`, merged field by field (RFC 7386) over the standing movement.
    pub movement: Option<Map>,
    /// Caps the per-loop step: the effective step is the smaller of this and the session's.
    pub max_change: Option<f32>,
    /// Bands that replace the standing control's band on the same key.
    pub constraints: BTreeMap<CanonicalPath, crate::loop_doc::ConstraintBand>,
}

impl ControlPatch {
    /// `standing` with this patch applied — what a block plays from its start. The standing control's
    /// `revision` is kept.
    pub fn apply(&self, standing: Option<&SessionControl>) -> Option<SessionControl> {
        let mut c = standing.cloned().unwrap_or_default();
        if let Some(o) = self.optimize {
            c.optimize = o;
        }
        if let Some(p) = &self.movement {
            c.movement = patch_movement(&c.movement, p);
        }
        if let Some(m) = self.max_change {
            let g = c.guardrails.max_change_per_loop;
            c.guardrails.max_change_per_loop = if g > 0.0 { g.min(m) } else { m };
        }
        c.constraints.extend(self.constraints.iter().map(|(k, b)| (k.clone(), *b)));
        Some(c)
    }
}

/// The live panel's toggles (ADR 0055 D3): hold ⇄ drift and the Orrery switch, as the player's last
/// commands set them. They win over a song block's control until the next block starts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LiveToggle {
    pub optimize: Option<OptimizeMode>,
    pub orrery: Option<bool>,
}

impl LiveToggle {
    pub fn is_empty(&self) -> bool {
        self.optimize.is_none() && self.orrery.is_none()
    }
    /// `later` on top of `self`: what it sets wins.
    pub fn merged(self, later: LiveToggle) -> LiveToggle {
        LiveToggle { optimize: later.optimize.or(self.optimize), orrery: later.orrery.or(self.orrery) }
    }
    /// `c` with the toggles applied.
    pub fn apply(&self, c: Option<SessionControl>) -> Option<SessionControl> {
        if self.is_empty() {
            return c;
        }
        let mut c = c.unwrap_or_default();
        if let Some(o) = self.optimize {
            c.optimize = o;
        }
        if let Some(on) = self.orrery {
            c.movement.orrery = on;
        }
        Some(c)
    }
}

/// `base` with a partial movement merged over it (RFC 7386). A patch that no longer deserializes
/// (`song::validate` refuses those) leaves `base` as it is.
pub fn patch_movement(base: &Movement, patch: &Map) -> Movement {
    let mut v = to_doc_value(base).unwrap_or_default();
    crate::song::merge_patch(&mut v, &DocValue::Object(patch.clone()));
    from_doc_value(v).unwrap_or_else(|_| base.clone())
}

/// The committed inline-commented JSON5 **template** for objectives docs, with `__TOKEN__`
/// placeholders rendered by [`objectives_template_json5`]. Comments document every field + examples.
const OBJECTIVES_TEMPLATE_JSON5: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../contracts/objectives_v0.1.template.json5"));

/// The committed, fully-annotated JSON5 **template** for loop docs (the engine never auto-creates a
/// loop doc, so this is shipped for the user to copy / emit). Guarded by a parse test so it stays a
/// valid renderable doc (ADR 0023).
pub const LOOP_TEMPLATE_JSON5: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../doc/loop_contract_v0.9.json5"));

/// Render the objectives JSON5 template with `c`'s current values substituted into the placeholder
/// slots — inline help comments preserved. Used for **every** objectives-doc write (auto-create seed,
/// TUI/`engines-play` write-back, `--emit-template`), so the built-in documentation survives edits. The
/// output round-trips: `SessionControl::from_json_str(&objectives_template_json5(&c)) == c`.
pub fn objectives_template_json5(c: &SessionControl) -> String {
    // Compact per-field serialization keeps each value on its commented line; whitespace is irrelevant
    // to JSON5, so the user can reformat freely.
    fn j<T: Serialize>(v: &T) -> String {
        crate::doc_value::to_string(v).unwrap_or_else(|_| "null".into())
    }
    OBJECTIVES_TEMPLATE_JSON5
        .replace("__REVISION__", &c.revision.to_string())
        .replace("__OPTIMIZE__", &j(&c.optimize))
        .replace("__MOVEMENT__", &j(&c.movement))
        .replace("__CONSTRAINTS__", &j(&c.constraints))
        .replace("__OVERRIDES__", &j(&c.overrides))
        .replace("__GUARDRAILS__", &j(&c.guardrails))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_control_from_json5_parses_comments_and_trailing_commas() {
        // JSON5: line/block comments, trailing commas, unquoted keys — must all parse (ADR 0023).
        let src = r#"{
            // the control plane
            optimize: "hold",
            movement: { orrery: true, }, /* inline */
        }"#;
        let c = SessionControl::from_json_str(src).expect("json5 objectives parse");
        assert_eq!(c.optimize, OptimizeMode::Hold);
        assert!(c.movement.orrery);
    }

    #[test]
    fn pre_rescope_objective_fields_are_ignored_but_pursue_is_an_error() {
        // A pre-ADR-0031 doc carrying the removed learner fields still loads (unknown fields are
        // ignored), but `optimize: "pursue"` names a mode that no longer exists.
        let old = r#"{ optimize: "drift", objective: [ { term: "mc:peaceful", weight: 0.4 } ],
                       guardrails: { max_change_per_loop: 0.05, clap_avoid: [ { term: "mc:harsh", threshold: 0.7 } ] } }"#;
        let c = SessionControl::from_json_str(old).expect("old doc still loads");
        assert_eq!(c.optimize, OptimizeMode::Drift);
        assert!(SessionControl::from_json_str(r#"{ optimize: "pursue" }"#).is_err());
    }

    #[test]
    fn objectives_template_renders_with_comments_and_round_trips() {
        let val = |c: &SessionControl| serde_json::to_value(c).unwrap();
        // default control
        let d = SessionControl::default();
        let rendered = objectives_template_json5(&d);
        assert!(rendered.contains("//"), "template carries inline help comments");
        let back = SessionControl::from_json_str(&rendered).expect("rendered default template parses");
        assert_eq!(val(&d), val(&back), "default control round-trips through the template");

        // a populated control (hold + orrery + constraint + override + guardrails)
        let populated = SessionControl::from_json_str(
            r#"{ optimize: "hold", revision: 5, movement: { orrery: true, phrase_loops: 4 },
                 constraints: { "global.master_gain_db": { min: -6.02, max: 0 } },
                 overrides: { "instruments.pad_dual.params.shift_st": -12 },
                 guardrails: { max_change_per_loop: 0.05, ceilings: { "instruments.poly.params.cutoff_hz": 3175.6 } } }"#,
        )
        .unwrap();
        let back2 = SessionControl::from_json_str(&objectives_template_json5(&populated)).expect("rendered populated parses");
        assert_eq!(val(&populated), val(&back2), "populated control round-trips through the template");
    }

    #[test]
    fn parse_constraint_sets_band_open_ends_and_clears() {
        let (p, b) = parse_constraint("global.master_gain_db -6.94 -6.02").unwrap();
        assert_eq!(p, "global.master_gain_db");
        assert_eq!(b, Some(crate::loop_doc::ConstraintBand { min: Some(-6.94), max: Some(-6.02) }));
        // open-ended: a `-` leaves that side unbounded
        let (_, b) = parse_constraint("instruments.poly.params.cutoff_hz 200 -").unwrap();
        assert_eq!(b, Some(crate::loop_doc::ConstraintBand { min: Some(200.0), max: None }));
        // clear: bare path or trailing dash ⇒ None (remove the band)
        assert_eq!(parse_constraint("global.master_gain_db").unwrap().1, None);
        assert_eq!(parse_constraint("global.master_gain_db -").unwrap().1, None);
        // bad numerics + too many tokens error
        assert!(parse_constraint("p 0.1 0.2 0.3").is_err());
        assert!(parse_constraint("p lo hi").is_err());
        assert!(parse_constraint("   ").is_err());
    }

    #[test]
    fn parse_override_reads_json_value_or_clears() {
        let (p, v) = parse_override("instruments.poly.params.cutoff_hz 1200").unwrap();
        assert_eq!(p, "instruments.poly.params.cutoff_hz");
        assert_eq!(v, Some(DocValue::from(1200)));
        assert_eq!(parse_override("x true").unwrap().1, Some(DocValue::Bool(true)));
        // a non-JSON token falls back to a bare string
        assert_eq!(parse_override("inst.params.wave saw").unwrap().1, Some(DocValue::String("saw".into())));
        // `-` clears the override
        assert_eq!(parse_override("instruments.poly.params.cutoff_hz -").unwrap().1, None);
        assert!(parse_override("novalue").is_err());
    }

    #[test]
    fn session_control_round_trips() {
        let ctrl = SessionControl {
            schema_version: default_objectives_schema(),
            revision: 3,
            optimize: OptimizeMode::LiveEdit,
            movement: Movement { orrery: true, ..Default::default() },
            constraints: BTreeMap::from([(
                "global.master_gain_db".to_string(),
                crate::loop_doc::ConstraintBand { min: Some(-6.02), max: None },
            )]),
            overrides: BTreeMap::from([("instruments.poly.params.cutoff_hz".to_string(), DocValue::from(1261.9))]),
            guardrails: Guardrails {
                max_change_per_loop: 0.05,
                ceilings: BTreeMap::from([("poly.params.cutoff_hz".to_string(), 3175.6)]),
                floors: BTreeMap::new(),
            },
            meta_notes: None,
        };
        let s = serde_json::to_string(&ctrl).unwrap();
        let back: SessionControl = serde_json::from_str(&s).unwrap();
        assert_eq!(back.optimize, OptimizeMode::LiveEdit);
        assert!(back.movement.is_orrery());
        assert_eq!(back.revision, 3);
        assert_eq!(back.constraints["global.master_gain_db"].min, Some(-6.02));
        assert_eq!(back.guardrails.max_change_per_loop, 0.05);
    }

    #[test]
    fn the_objectives_template_writes_minus_infinity_and_reads_it_back() {
        // ADR 0054 P3: a gain's floor or override may be `-Infinity` (silence); `serde_json` wrote `null`.
        let mut c = SessionControl::from_flags("drift", false, &[], &["instruments.pad.params.level_db=-inf".into()], 0.05).unwrap().unwrap();
        c.constraints.insert("global.master_gain_db".into(), crate::loop_doc::ConstraintBand { min: Some(f32::NEG_INFINITY), max: Some(0.0) });
        c.overrides.insert("instruments.pad.params.level_db".into(), DocValue::from(f64::NEG_INFINITY));
        let text = objectives_template_json5(&c);
        assert!(text.contains("-Infinity") && !text.contains("null"), "{text}");
        let back = SessionControl::from_json_str(&text).unwrap();
        assert_eq!(back.guardrails.floors["instruments.pad.params.level_db"], f32::NEG_INFINITY);
        assert_eq!(back.constraints["global.master_gain_db"].min, Some(f32::NEG_INFINITY));
        assert_eq!(back.overrides["instruments.pad.params.level_db"].as_f64(), Some(f64::NEG_INFINITY));
    }

    #[test]
    fn control_from_flags_parses_or_defaults_to_drift() {
        // plain drift, no flags → None (default behavior)
        assert!(SessionControl::from_flags("drift", false, &[], &[], 0.05).unwrap().is_none());
        // drift + orrery (movement axis only) → Some with the movement flag set
        let m = SessionControl::from_flags("drift", true, &[], &[], 0.05).unwrap().unwrap();
        assert_eq!(m.optimize, OptimizeMode::Drift);
        assert!(m.movement.is_orrery());
        // hold with a ceiling
        let o = SessionControl::from_flags("hold", false, &["instruments.poly.params.cutoff_hz=3175.6".to_string()], &[], 0.04).unwrap().unwrap();
        assert_eq!(o.optimize, OptimizeMode::Hold);
        assert!(!o.movement.is_orrery());
        assert_eq!(o.guardrails.max_change_per_loop, 0.04);
        assert_eq!(o.guardrails.ceilings["instruments.poly.params.cutoff_hz"], 3175.6);
        // legacy `orrery` / removed `pursue` / unknown modes are rejected
        assert!(SessionControl::from_flags("orrery", false, &[], &[], 0.05).is_err());
        assert!(SessionControl::from_flags("pursue", false, &[], &[], 0.05).is_err());
        assert!(SessionControl::from_flags("bogus", false, &[], &[], 0.05).is_err());
    }

    #[test]
    fn a_doc_supplies_control_and_flags_layer_over_it() {
        let mut doc: crate::loop_doc::LoopDoc = crate::test_support::fixture();
        assert!(SessionControl::from_doc(&doc).is_none(), "no control fields ⇒ plain drift");
        doc.optimize = Some(OptimizeMode::Hold);
        doc.movement = Some(Movement { orrery: true, tension_target: 0.15, ..Default::default() });
        doc.constraints.insert("global.master_gain_db".into(), crate::loop_doc::ConstraintBand { min: Some(-6.02), max: None });
        let c = SessionControl::from_doc(&doc).unwrap();
        assert_eq!((c.optimize, c.movement.orrery, c.movement.tension_target), (OptimizeMode::Hold, true, 0.15));
        assert_eq!(c.constraints.len(), 1);
        // no flags: the doc's control passes through
        let same = SessionControl::with_flags(Some(c.clone()), "", false, &[], &[], 0.05).unwrap().unwrap();
        assert_eq!(serde_json::to_value(&same).unwrap(), serde_json::to_value(&c).unwrap());
        // --optimize drift replaces the mode but keeps the doc's movement; a ceiling adds a guardrail
        let f = SessionControl::with_flags(Some(c), "drift", false, &["global.master_gain_db=-1.94".into()], &[], 0.05).unwrap().unwrap();
        assert_eq!(f.optimize, OptimizeMode::Drift);
        assert!(f.movement.orrery && (f.movement.tension_target - 0.15).abs() < 1e-6);
        assert_eq!(f.guardrails.ceilings.len(), 1);
        // without a doc control, flags behave as before
        assert!(SessionControl::with_flags(None, "drift", false, &[], &[], 0.05).unwrap().is_none());
    }

    #[test]
    fn doc_settings_fold_into_a_running_control_keeping_its_mode() {
        let old: crate::loop_doc::LoopDoc = crate::test_support::fixture();
        let mut new = old.clone();
        let live = SessionControl { optimize: OptimizeMode::LiveEdit, revision: 3, ..Default::default() };
        assert!(SessionControl::with_doc_settings(Some(&live), &old, &new).is_none(), "unchanged ⇒ no update");
        new.movement = Some(Movement { orrery: true, ..Default::default() });
        new.optimize = Some(OptimizeMode::Drift); // the doc's render mode — the player keeps its own
        let c = SessionControl::with_doc_settings(Some(&live), &old, &new).unwrap();
        assert_eq!((c.optimize, c.movement.orrery, c.revision), (OptimizeMode::LiveEdit, true, 4));
    }
}
