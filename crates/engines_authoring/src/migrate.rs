//! `engines-migrate` (ADR 0054 P9): a schema v0.8 loop doc → schema v0.9 text, through the [`cst`] so every
//! comment, blank line and layout choice survives. Time, frequency and gain become real units with
//! unit-suffixed keys (`cutoff` 0.3 → `cutoff_hz` 318.39, `attack` 0.35 → `attack_s` 1.05, `level`
//! 0.5 → `level_db` −6.02, `level` 0 → `-Infinity`); a route's `depth` becomes a distance in its dest's
//! unit (`depth_oct`, `depth_s`, `depth_db`, …); `dest` paths, `safe_bounds` and `constraints` keys and
//! song overrides follow. Unitless fractions (`sustain`, `resonance`, mixes, positions) are untouched.
//!
//! The v0.8 maps are frozen in [`v08`], so this keeps reading v0.8 after the engine reads only v0.9.
//! Three kinds of file migrate: a loop doc (`schema_version: "0.8"`), a part of the library (a `part`
//! header), and an objectives file (`objectives_v0.1`, migrated against the doc it steers).
//!
//! **Gain routes can't migrate exactly**: v0.9 sums a gain route in dB, v0.8 in amplitude. A gain
//! depth is converted so the source at +1 lands on the amplitude it did (owner's call, 2026-10-27):
//! exact at both ends for a unipolar source; a bipolar source's −1 side becomes the mirror image in
//! dB. A route that ducked to silence now ducks to −60 dB, and a gain authored at 0 with a route
//! lifting it starts from a −60 dB floor (a `-Infinity` base is silent under any route).

pub mod v08;

use crate::cst::edit::{apply, key_as, string_as, TextEdit};
use crate::cst::{Cst, Member, Node, Span, TokenKind};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// The schema this writes.
pub const TO_VERSION: &str = "0.9";
/// The schema this reads.
pub const FROM_VERSION: &str = "0.8";
/// The finite floor of a v0.9 gain (dB): quieter amplitudes are raised to it.
pub const FLOOR_DB: f64 = -60.0;
/// The widest gain depth (dB): the −60 to +6 dB span.
const GAIN_SPAN_DB: f64 = 66.0;
/// Appended to a comment that quotes a number the migration changed.
pub const TODO: &str = "TODO(v0.9)";

/// What a file is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DocKind {
    Loop,
    Part,
    Objectives,
}

/// One change, in the doc's own paths: `instruments.pad.params.cutoff: 0.3 → cutoff_hz: 318.39`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Change {
    pub path: String,
    pub from: String,
    pub to: String,
    /// What kind of change, for a summary (`cutoff → cutoff_hz`, `depth → depth_oct`).
    #[serde(skip)]
    pub label: String,
}

/// A migrated file.
#[derive(Clone, Debug, Serialize)]
pub struct Migration {
    pub kind: DocKind,
    /// The v0.9 text (the input itself when `unchanged`).
    #[serde(skip)]
    pub text: String,
    /// Nothing to migrate: already v0.9, or nothing in it has a unit.
    pub unchanged: bool,
    pub changes: Vec<Change>,
    /// What a person should look at: a raised floor, a removed route, an assumed base, a gain route
    /// that changes sound.
    pub notes: Vec<String>,
}

/// Migrate a loop doc or a part (`src`). An objectives file needs the doc it steers: [`migrate_with`].
pub fn migrate(src: &str) -> Result<Migration, String> {
    migrate_with(src, None)
}

/// Migrate `src`; `doc` is the v0.8 loop doc an objectives file steers (its instruments decide what
/// `decay` and `t60_scale` mean, its routes' bases what a depth band means). Without it, an objectives
/// file assumes the defaults (an ADSR, `t60_mode: "fixed"`, a default base) and says so in `notes`.
pub fn migrate_with(src: &str, doc: Option<&str>) -> Result<Migration, String> {
    let cst = Cst::parse(src);
    let Some(value) = cst.to_value() else {
        let e = cst.errors.first().map(|e| e.message.clone()).unwrap_or_else(|| "empty".into());
        return Err(format!("not valid JSON5: {e}"));
    };
    let root = cst.root.as_ref().expect("a value has a root");
    let version = value.get("schema_version").and_then(Value::as_str);
    let kind = match version {
        Some(v) if v.starts_with("objectives") => DocKind::Objectives,
        Some(TO_VERSION) => return Ok(unchanged(src, DocKind::Loop)),
        Some(FROM_VERSION) => DocKind::Loop,
        Some(v) => return Err(format!("schema_version {v:?}: engines-migrate reads {FROM_VERSION:?} and writes {TO_VERSION:?}")),
        None if value.get("part").is_some() => DocKind::Part,
        None => return Err("no schema_version, and not a part: not a loop doc".into()),
    };
    let ctx_value = match (kind, doc) {
        (DocKind::Objectives, Some(d)) => {
            let v = Cst::parse(d).to_value().ok_or("the loop doc given for the objectives file isn't valid JSON5")?;
            if v.get("schema_version").and_then(Value::as_str) == Some(TO_VERSION) {
                return Err("the loop doc given for the objectives file is already v0.9: give the v0.8 doc (its backup)".into());
            }
            v
        }
        (DocKind::Objectives, None) => Value::Null,
        _ => value.clone(),
    };
    let json = root.members().first().is_some_and(|m| m.key.quote == Some('"'));
    let mut m = Migrator { src, cst: &cst, ctx: Ctx::new(&ctx_value, kind), edits: Vec::new(), changes: Vec::new(), notes: Vec::new(), numeric_lines: BTreeSet::new(), json };
    match kind {
        DocKind::Loop | DocKind::Part => m.loop_doc(root),
        DocKind::Objectives => {
            if doc.is_none() {
                m.notes.push("migrated without its loop doc: `decay` read as an ADSR's, `t60_scale` as `t60_mode: \"fixed\"`, depth bands at default bases — give the doc (--doc) to be exact".into());
            }
            m.objectives(root);
        }
    }
    if m.edits.is_empty() {
        return Ok(Migration { notes: m.notes, ..unchanged(src, kind) });
    }
    m.tag_comments();
    let text = apply(src, &m.edits);
    Ok(Migration { kind, text, unchanged: false, changes: m.changes, notes: m.notes })
}

fn unchanged(src: &str, kind: DocKind) -> Migration {
    Migration { kind, text: src.to_string(), unchanged: true, changes: Vec::new(), notes: Vec::new() }
}

// ── what a param becomes ─────────────────────────────────────────────────────

/// How a value converts.
#[derive(Clone, Copy, Debug)]
enum Val {
    /// A log-frequency map → Hz.
    Hz(fn(f64) -> f64),
    /// `x × max`, written to `dp` decimals.
    Times(f64, usize),
    /// A log time or ratio map, to 3 decimals.
    Log(fn(f64) -> f64),
    /// An amplitude → dB (`-Infinity` for silence).
    Gain,
    /// The drive pre-gain → dB.
    Drive,
    /// Already native: the key is renamed, the value kept as written.
    Keep,
}

/// How a route's depth into it converts.
#[derive(Clone, Copy, Debug)]
enum Dep {
    /// `depth × factor` under the key, to `dp` decimals: a rescale (the normalized distance is the same).
    Scale(&'static str, f64, usize),
    /// A gain route: the +1 swing at the base, in dB.
    Gain,
    /// A drive route: the +1 swing at the base, in dB of pre-gain.
    Drive,
}

#[derive(Clone, Copy, Debug)]
struct Rule {
    leaf: &'static str,
    val: Val,
    dep: Dep,
}

const fn rule(leaf: &'static str, val: Val, dep: Dep) -> Rule {
    Rule { leaf, val, dep }
}

/// An instrument param's rule; `None` for a unitless one (it stays as it is). `bell`: the legacy
/// `event` bell, whose `decay` is its 0–4 s ring; `note`: `t60_mode: "note"`.
fn param_rule(leaf: &str, bell: bool, note: bool) -> Option<Rule> {
    let oct = |base: f64| base.log2();
    Some(match leaf {
        "cutoff" => rule("cutoff_hz", Val::Hz(v08::cutoff_hz), Dep::Scale("depth_oct", oct(100.0), 3)),
        "band_lo" => rule("band_lo_hz", Val::Hz(v08::band_hz), Dep::Scale("depth_oct", oct(50.0), 3)),
        "band_hi" => rule("band_hi_hz", Val::Hz(v08::band_hz), Dep::Scale("depth_oct", oct(50.0), 3)),
        "voices_detune" => rule("voices_detune_cents", Val::Times(v08::VOICES_DETUNE_CENTS, 1), Dep::Scale("depth_cents", v08::VOICES_DETUNE_CENTS, 1)),
        "osc_b_detune" => rule("osc_b_detune_cents", Val::Times(v08::OSC_B_DETUNE_CENTS, 1), Dep::Scale("depth_cents", v08::OSC_B_DETUNE_CENTS, 1)),
        "attack" => rule("attack_s", Val::Times(v08::ATTACK_MAX_S, 3), Dep::Scale("depth_s", v08::ATTACK_MAX_S, 3)),
        "decay" if bell => rule("decay_s", Val::Times(v08::BELL_DECAY_MAX_S, 3), Dep::Scale("depth_s", v08::BELL_DECAY_MAX_S, 3)),
        "decay" => rule("decay_s", Val::Times(v08::DECAY_MAX_S, 3), Dep::Scale("depth_s", v08::DECAY_MAX_S, 3)),
        "release" => rule("release_s", Val::Times(v08::RELEASE_MAX_S, 3), Dep::Scale("depth_s", v08::RELEASE_MAX_S, 3)),
        "t60_scale" if note => rule("ring_ratio", Val::Log(v08::ring_ratio), Dep::Scale("depth_oct", oct(16.0), 3)),
        "t60_scale" => rule("t60_s", Val::Log(v08::t60_s), Dep::Scale("depth_oct", oct(v08::T60_MAX_S / v08::T60_MIN_S), 3)),
        "level" => rule("level_db", Val::Gain, Dep::Gain),
        "osc_a_level" => rule("osc_a_level_db", Val::Gain, Dep::Gain),
        "osc_b_level" => rule("osc_b_level_db", Val::Gain, Dep::Gain),
        "drive" => rule("drive_db", Val::Drive, Dep::Drive),
        "shift" => rule("shift_st", Val::Keep, Dep::Scale("depth_st", v08::SHIFT_SPAN_ST, 2)),
        "osc_b_ratio" => rule("osc_b_ratio", Val::Keep, Dep::Scale("depth_x", v08::OSC_B_RATIO_SPAN, 3)),
        "root_hz" => rule("root_hz", Val::Keep, Dep::Scale("depth_hz", v08::ROOT_HZ_SPAN, 2)),
        "spread_hz" => rule("spread_hz", Val::Keep, Dep::Scale("depth_hz", v08::SPREAD_HZ_SPAN, 2)),
        _ => return None,
    })
}

/// Where a path points.
#[derive(Clone, Debug, PartialEq)]
enum Target {
    Inst { id: String, leaf: String },
    Global { leaf: String },
    BusReturn { bus: String },
    BusFx { bus: String, fx: String, leaf: String },
}

impl Target {
    fn parse(path: &str) -> Option<Target> {
        let s: Vec<&str> = path.split('.').collect();
        Some(match s.as_slice() {
            ["instruments", id, "params", leaf] => Target::Inst { id: id.to_string(), leaf: leaf.to_string() },
            ["global", leaf] => Target::Global { leaf: leaf.to_string() },
            ["buses", bus, "return_gain"] => Target::BusReturn { bus: bus.to_string() },
            ["buses", bus, "effects", fx, "params", leaf] => Target::BusFx { bus: bus.to_string(), fx: fx.to_string(), leaf: leaf.to_string() },
            _ => return None,
        })
    }
}

/// The v0.8 doc's facts the rules need: each instrument's type, whether it's the bell, its
/// `t60_mode`, and the bases routes start from.
struct Ctx<'a> {
    doc: &'a Value,
    /// A part: its instruments are placeholders and a `$target` is an instrument we can't see.
    part: bool,
}

impl<'a> Ctx<'a> {
    fn new(doc: &'a Value, kind: DocKind) -> Ctx<'a> {
        Ctx { doc, part: kind == DocKind::Part }
    }
    fn inst(&self, id: &str) -> Option<&Value> {
        self.doc.get("instruments").and_then(|i| i.get(id))
    }
    fn inst_type(&self, id: &str) -> &str {
        self.inst(id).and_then(|i| i.get("type")).and_then(Value::as_str).unwrap_or("subtractive")
    }
    fn is_midi(&self, id: &str) -> bool {
        self.inst_type(id) == "midi"
    }
    fn is_bell(&self, id: &str) -> bool {
        id == "event" && self.inst_type(id) == "subtractive"
    }
    fn t60_note(&self, id: &str) -> bool {
        self.inst(id).and_then(|i| i.get("params")).and_then(|p| p.get("t60_mode")).and_then(Value::as_str) == Some("note")
    }
    fn no_buses(&self) -> bool {
        self.doc.get("buses").and_then(Value::as_object).is_none_or(|b| b.is_empty())
    }

    /// The rule for a path, and its new path; `None` when nothing about it changes.
    fn resolve(&self, path: &str) -> Option<(String, Rule, Target)> {
        let t = Target::parse(path)?;
        let (new, r) = match &t {
            Target::Inst { id, leaf } => {
                if self.is_midi(id) {
                    return None; // free-form 0..1, the external synth's own
                }
                let r = param_rule(leaf, self.is_bell(id), self.t60_note(id))?;
                (format!("instruments.{id}.params.{}", r.leaf), r)
            }
            Target::Global { leaf } if leaf == "master_gain" => ("global.master_gain_db".to_string(), rule("master_gain_db", Val::Gain, Dep::Gain)),
            Target::Global { .. } => return None,
            Target::BusReturn { bus } => (format!("buses.{bus}.return_gain_db"), rule("return_gain_db", Val::Gain, Dep::Gain)),
            Target::BusFx { bus, fx, leaf } if leaf == "predelay" => {
                (format!("buses.{bus}.effects.{fx}.params.predelay_ms"), rule("predelay_ms", Val::Keep, Dep::Scale("depth_ms", v08::PREDELAY_SPAN_MS, 1)))
            }
            Target::BusFx { .. } => return None,
        };
        Some((new, r, t))
    }

    /// A gain's (or drive's) v0.8 value: as authored, else the engine's default for it.
    fn base(&self, t: &Target) -> (f64, bool) {
        let num = |v: Option<&Value>| v.and_then(Value::as_f64);
        match t {
            Target::Inst { id, leaf } => {
                let authored = num(self.inst(id).and_then(|i| i.get("params")).and_then(|p| p.get(leaf.as_str())));
                let default = match leaf.as_str() {
                    "level" => v08::LEVEL_DEFAULT,
                    "osc_a_level" => v08::OSC_A_LEVEL_DEFAULT,
                    "osc_b_level" => v08::OSC_B_LEVEL_DEFAULT,
                    _ => v08::DRIVE_DEFAULT,
                };
                authored.map_or((default, false), |a| (a, true))
            }
            Target::Global { .. } => num(self.doc.get("global").and_then(|g| g.get("master_gain"))).map_or((v08::MASTER_GAIN_DEFAULT, false), |a| (a, true)),
            Target::BusReturn { bus } => {
                if self.no_buses() && bus == v08::DEFAULT_BUS {
                    return (num(self.doc.get("global").and_then(|g| g.get("reverb_mix"))).unwrap_or(0.0), true);
                }
                num(self.doc.get("buses").and_then(|b| b.get(bus.as_str())).and_then(|b| b.get("return_gain"))).map_or((v08::RETURN_GAIN_DEFAULT, false), |a| (a, true))
            }
            Target::BusFx { .. } => (0.0, false),
        }
    }

    /// Every mod route in the doc and its song layers: `(route id, dest, depth)`, a layer's partial
    /// entry taking the top-level route's dest.
    fn routes(&self) -> Vec<(String, String, f64)> {
        let mut out = Vec::new();
        let top = self.doc.get("matrix").and_then(Value::as_object);
        let mut add = |id: &str, e: &Value, fallback: Option<&Value>| {
            let dest = e.get("dest").or_else(|| fallback.and_then(|f| f.get("dest"))).and_then(Value::as_str);
            let kind = e.get("kind").or_else(|| fallback.and_then(|f| f.get("kind"))).and_then(Value::as_str).unwrap_or("mod");
            if let (Some(dest), Some(d), "mod") = (dest, e.get("depth").and_then(Value::as_f64), kind) {
                out.push((id.to_string(), dest.to_string(), d));
            }
        };
        for (id, e) in top.into_iter().flatten() {
            add(id, e, None);
        }
        for layer in self.doc.get("song").and_then(|s| s.get("matrices")).and_then(Value::as_object).into_iter().flat_map(|m| m.values()) {
            for (id, e) in layer.as_object().into_iter().flatten() {
                add(id, e, top.and_then(|t| t.get(id)));
            }
        }
        out
    }

    /// Gains authored (or defaulting) to silence that a route lifts: they start from the −60 dB floor.
    fn lifted(&self) -> BTreeSet<String> {
        self.routes()
            .into_iter()
            .filter(|(_, _, d)| *d > 0.0)
            .filter_map(|(_, dest, _)| {
                let (_, r, t) = self.resolve(&dest)?;
                (matches!(r.val, Val::Gain) && self.base(&t).0 <= 0.0).then_some(dest)
            })
            .collect()
    }
}

// ── numbers ──────────────────────────────────────────────────────────────────

thread_local! {
    /// [`full_precision`]'s switch: every number at the shortest text that reads back exactly.
    static FULL_PRECISION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` with every converted number written at full precision instead of the ADR's per-unit
/// decimals — for the one-time null test (ADR 0054 P11), which proves the curves and the parse path
/// exact on docs whose rounding didn't move them. Shipped docs use the decimals.
pub fn full_precision<T>(f: impl FnOnce() -> T) -> T {
    FULL_PRECISION.with(|c| c.set(true));
    let out = f();
    FULL_PRECISION.with(|c| c.set(false));
    out
}

/// `v` to `dp` decimals, trailing zeros trimmed (`318.39`, `1.05`, `0`); every digit in a
/// [`full_precision`] run.
fn fmt(v: f64, dp: usize) -> String {
    if FULL_PRECISION.with(|c| c.get()) {
        let s = format!("{v}");
        return if s == "-0" { "0".into() } else { s };
    }
    let s = format!("{v:.dp$}");
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    if s == "-0" { "0".into() } else { s }
}

/// Hz: 2 decimals under 1 kHz, 1 above (≈ 0.1 cent either way).
fn fmt_hz(hz: f64) -> String {
    fmt(hz, if hz < 1000.0 { 2 } else { 1 })
}

/// An amplitude in dB: `None` for silence, the floor for anything quieter.
fn amp_db(a: f64) -> Option<f64> {
    if a <= 0.0 {
        None
    } else {
        Some((20.0 * a.log10()).max(FLOOR_DB))
    }
}

/// A value under `val` in its new unit, and a note when it isn't a plain conversion.
fn convert(val: Val, x: f64, lifted: bool) -> (String, Option<String>) {
    match val {
        Val::Hz(f) => (fmt_hz(f(x)), None),
        Val::Times(max, dp) => (fmt(x * max, dp), None),
        Val::Log(f) => (fmt(f(x), 3), None),
        Val::Drive => (fmt(v08::drive_db(x.clamp(0.0, 1.0)), 2), None),
        Val::Gain if lifted && x <= 0.0 => (fmt(FLOOR_DB, 2), Some("silent, but a route lifts it: starts from the −60 dB floor (a -Infinity base stays silent under any route)".into())),
        Val::Gain => match amp_db(x) {
            None => ("-Infinity".into(), None),
            Some(db) if 20.0 * x.log10() < FLOOR_DB => (fmt(db, 2), Some(format!("{x} is under −60 dB: raised to the −60 dB floor"))),
            Some(db) => (fmt(db, 2), None),
        },
        Val::Keep => unreachable!("a kept value isn't converted"),
    }
}

/// A depth under `dep` at the route's base `a` (v0.8), as `(key, text, note)`.
fn convert_depth(dep: Dep, d: f64, a: f64) -> (&'static str, String, Option<String>) {
    match dep {
        Dep::Scale(key, k, dp) => (key, fmt(d * k, dp), None),
        Dep::Drive => {
            let x = a.clamp(0.0, 1.0);
            ("depth_db", fmt(v08::drive_db((x + d).clamp(0.0, 1.0)) - v08::drive_db(x), 2), None)
        }
        Dep::Gain => {
            let at = |amp: f64| amp_db(amp).unwrap_or(FLOOR_DB);
            let (db, note) = if a > 0.0 {
                (at((a + d).clamp(0.0, 1.0)) - at(a), None)
            } else if d > 0.0 {
                (at(d.min(1.0)) - FLOOR_DB, Some("lifts a silent gain: from the −60 dB floor".to_string()))
            } else {
                (0.0, Some("a route down from silence is inaudible: depth 0".to_string()))
            };
            ("depth_db", fmt(db.clamp(-GAIN_SPAN_DB, GAIN_SPAN_DB), 2), note)
        }
    }
}

// ── the walk ─────────────────────────────────────────────────────────────────

struct Migrator<'s> {
    src: &'s str,
    cst: &'s Cst,
    ctx: Ctx<'s>,
    edits: Vec<TextEdit>,
    changes: Vec<Change>,
    notes: Vec<String>,
    /// Byte offsets of the numbers that changed — their comments get a [`TODO`].
    numeric_lines: BTreeSet<usize>,
    /// A strict-JSON doc (its keys quoted): what's inserted stays JSON.
    json: bool,
}

/// The value of `key` in object `n`.
fn get<'n>(n: &'n Node, key: &str) -> Option<&'n Node> {
    n.get(key)
}

impl<'s> Migrator<'s> {
    fn text(&self, span: Span) -> &'s str {
        span.text(self.src)
    }

    fn rename(&mut self, m: &Member, new: &str) {
        if m.key.name != new {
            self.edits.push(TextEdit::new(m.key.span, key_as(new, m.key.quote)));
        }
    }

    fn replace(&mut self, n: &Node, text: String, numeric: bool) {
        if self.text(n.span) != text {
            if numeric {
                self.numeric_lines.insert(n.span.start);
            }
            self.edits.push(TextEdit::new(n.span, text));
        }
    }

    fn change(&mut self, label: impl Into<String>, path: String, from: String, to: String) {
        self.changes.push(Change { path, from, to, label: label.into() });
    }

    /// A key as this doc writes keys: quoted in a strict-JSON doc.
    fn k(&self, name: &str) -> String {
        key_as(name, self.json.then_some('"'))
    }

    /// Insert `key: value_text` into `obj` in this doc's style.
    fn insert(&mut self, obj: &Node, key: &str, value_text: &str) -> bool {
        let e = if self.json { self.cst.insert_into_json(self.src, obj, key, value_text) } else { self.cst.insert_into(self.src, obj, key, value_text) };
        let ok = e.is_some();
        self.edits.extend(e.unwrap_or_default());
        ok
    }

    fn note(&mut self, path: &str, note: Option<String>) {
        if let Some(n) = note {
            self.notes.push(format!("{path}: {n}"));
        }
    }

    /// A string value rewritten, its quote style kept.
    fn replace_str(&mut self, n: &Node, s: &str) {
        let quote = self.src[n.span.start..].chars().next().filter(|c| *c == '\'').unwrap_or('"');
        self.replace(n, string_as(s, quote), false);
    }

    /// Remove the member at `path` (a removed route, `global.reverb_mix`). In a strict-JSON doc, the
    /// last member's removal takes the comma before it too, so the object stays JSON.
    fn remove(&mut self, path: &str) {
        let Some(e) = self.cst.remove_member(self.src, path) else { return };
        self.edits.extend(e);
        if !self.json {
            return;
        }
        let f = self.cst.find(path);
        let parent = self.cst.find(&crate::cst::path::render(&f.segs[..f.segs.len().saturating_sub(1)]));
        let (Some(m), Some(obj)) = (f.member, parent.node) else { return };
        let members = obj.members();
        if members.last().is_some_and(|l| std::ptr::eq(l, m)) && members.len() > 1 {
            if let Some(c) = members[members.len() - 2].comma {
                self.edits.push(TextEdit::new(c, ""));
            }
        }
    }

    fn loop_doc(&mut self, root: &Node) {
        if let Some(v) = get(root, "schema_version") {
            self.replace_str(v, TO_VERSION);
            self.change("schema_version", "schema_version".into(), format!("{FROM_VERSION:?}"), format!("{TO_VERSION:?}"));
        }
        let lifted = self.ctx.lifted();
        let removed = self.removed_routes();
        if let Some(part) = get(root, "part") {
            self.part_header(part);
        }
        if let Some(patch) = get(root, "patch") {
            self.patch(patch);
        }
        if let Some(g) = get(root, "global") {
            self.global(g, &lifted);
        }
        let synth_bus = self.ctx.doc.get("global").and_then(|g| g.get("reverb_mix")).is_some() && self.ctx.no_buses() && !self.ctx.part;
        if let Some(insts) = get(root, "instruments") {
            for m in insts.members() {
                if let Some(inst) = &m.value {
                    self.instrument(&m.key.name, inst, &lifted, synth_bus);
                }
            }
        }
        if let Some(buses) = get(root, "buses") {
            for m in buses.members() {
                if let Some(bus) = &m.value {
                    self.bus(&m.key.name, bus, &lifted);
                }
            }
        }
        if synth_bus {
            self.synthesize_bus(root);
        }
        if let Some(matrix) = get(root, "matrix") {
            for m in matrix.members() {
                if removed.contains(&m.key.name) {
                    self.remove(&format!("matrix.{}", m.key.name));
                } else if let Some(e) = &m.value {
                    self.route(&format!("matrix.{}", m.key.name), e, None);
                }
            }
        }
        for section in ["safe_bounds", "constraints"] {
            if let Some(bands) = get(root, section) {
                self.bands(section, bands, &removed);
            }
        }
        // A song block's drift overrides (ADR 0055) are bands keyed by path, like `constraints`
        // ("hold" and `{ max_drift }` are unit-free: only their key follows).
        let blocks = get(root, "song").and_then(|s| get(s, "sequence")).map(Node::items).unwrap_or_default();
        for (i, block) in blocks.iter().enumerate() {
            if let Some(drift) = get(&block.node, "control").and_then(|c| get(c, "drift")) {
                self.bands(&format!("song.sequence[{i}].control.drift"), drift, &removed);
            }
        }
        if let Some(layers) = get(root, "song").and_then(|s| get(s, "matrices")) {
            for layer in layers.members() {
                let Some(l) = &layer.value else { continue };
                for m in l.members() {
                    let path = format!("song.matrices.{}.{}", layer.key.name, m.key.name);
                    if removed.contains(&m.key.name) {
                        self.remove(&path);
                    } else if let Some(e) = m.value.as_ref().filter(|e| e.is_object()) {
                        let top = self.ctx.doc.get("matrix").and_then(|t| t.get(&m.key.name)).and_then(|t| t.get("dest")).and_then(Value::as_str).map(str::to_string);
                        self.route(&path, e, top);
                    }
                }
            }
        }
    }

    /// Routes into `global.reverb_mix`: it never moved anything (only the back-compat bus's seed), and
    /// v0.9 has no `reverb_mix`.
    fn removed_routes(&mut self) -> BTreeSet<String> {
        let ids: BTreeSet<String> = self
            .ctx
            .doc
            .get("matrix")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter(|(_, e)| e.get("dest").and_then(Value::as_str) == Some("global.reverb_mix"))
            .map(|(id, _)| id.clone())
            .collect();
        for id in &ids {
            self.notes.push(format!("matrix.{id}: removed — a route into global.reverb_mix never moved anything, and v0.9 has no reverb_mix"));
        }
        ids
    }

    fn part_header(&mut self, part: &Node) {
        // `target: "cutoff"` names the param a part acts on.
        if let Some(t) = get(part, "target") {
            if let Some(r) = t.as_str().and_then(|leaf| param_rule(leaf, false, false)).filter(|r| Some(r.leaf) != t.as_str()) {
                let from = self.text(t.span).to_string();
                self.replace_str(t, r.leaf);
                self.change("part.target", "part.target".into(), from, format!("{:?}", r.leaf));
            }
        }
    }

    fn global(&mut self, g: &Node, lifted: &BTreeSet<String>) {
        for m in g.members() {
            match m.key.name.as_str() {
                "master_gain" => self.param("global.master_gain", m, lifted),
                "reverb_mix" => {
                    self.remove("global.reverb_mix");
                    let from = m.value.as_ref().map(|v| self.text(v.span).to_string()).unwrap_or_default();
                    self.change("global.reverb_mix removed", "global.reverb_mix".into(), from, "(removed)".into());
                }
                _ => {}
            }
        }
    }

    /// A param member at `path` (its v0.8 path): renamed, and its number converted.
    fn param(&mut self, path: &str, m: &Member, lifted: &BTreeSet<String>) {
        let Some((new_path, r, _)) = self.ctx.resolve(path) else { return };
        let Some(v) = &m.value else { return };
        self.rename(m, r.leaf);
        let from = format!("{}: {}", m.key.name, self.text(v.span));
        if matches!(r.val, Val::Keep) {
            if m.key.name != r.leaf {
                self.change(format!("{} → {}", m.key.name, r.leaf), path.into(), from, format!("{}: {}", r.leaf, self.text(v.span)));
            }
            return;
        }
        let Some(x) = v.as_f64() else {
            self.notes.push(format!("{path}: not a number, renamed only"));
            return;
        };
        let (text, note) = convert(r.val, x, lifted.contains(path));
        self.change(format!("{} → {}", m.key.name, r.leaf), path.into(), from, format!("{}: {text}", r.leaf));
        self.note(&new_path, note);
        self.replace(v, text, true);
    }

    fn instrument(&mut self, id: &str, inst: &Node, lifted: &BTreeSet<String>, synth_bus: bool) {
        let midi = self.ctx.is_midi(id);
        if let Some(params) = get(inst, "params").filter(|_| !midi) {
            for m in params.members() {
                self.param(&format!("instruments.{id}.params.{}", m.key.name), m, lifted);
            }
            // A gain that defaults to silence (`osc_b_level`) and that a route lifts needs its floor written.
            for leaf in ["osc_b_level"] {
                let path = format!("instruments.{id}.params.{leaf}");
                if lifted.contains(&path) && params.get(leaf).is_none() {
                    if self.insert(params, &format!("{leaf}_db"), &fmt(FLOOR_DB, 2)) {
                        self.change(format!("{leaf}_db written at the floor"), path.clone(), "(default 0)".into(), format!("{leaf}_db: -60"));
                        self.notes.push(format!("{path}: defaults to silence, but a route lifts it: written at the −60 dB floor"));
                    }
                }
            }
        }
        let back_compat = format!("{{ {}: 0 }}", self.k(v08::DEFAULT_BUS));
        let sends = inst.member("sends");
        if let Some(m) = sends {
            self.rename(m, "sends_db");
            if let Some(map) = &m.value {
                if map.members().is_empty() && synth_bus && !midi {
                    self.replace(map, back_compat.clone(), false);
                    self.change("sends to the back-compat bus", format!("instruments.{id}.sends"), "{}".into(), format!("sends_db: {back_compat}"));
                }
                for s in map.members() {
                    let Some(v) = &s.value else { continue };
                    let Some(x) = v.as_f64() else { continue };
                    let (text, note) = convert(Val::Gain, x, false);
                    self.change("sends → sends_db", format!("instruments.{id}.sends.{}", s.key.name), self.text(v.span).to_string(), text.clone());
                    self.note(&format!("instruments.{id}.sends_db.{}", s.key.name), note);
                    self.replace(v, text, true);
                }
            }
        } else if synth_bus && !midi && self.insert(inst, "sends_db", &back_compat) {
            self.change("sends to the back-compat bus", format!("instruments.{id}.sends"), "(none)".into(), format!("sends_db: {back_compat}"));
        }
    }

    fn bus(&mut self, id: &str, bus: &Node, lifted: &BTreeSet<String>) {
        let path = format!("buses.{id}.return_gain");
        match bus.member("return_gain") {
            Some(m) => self.param(&path, m, lifted),
            None if lifted.contains(&path) => {
                if self.insert(bus, "return_gain_db", &fmt(FLOOR_DB, 2)) {
                    self.change("return_gain_db written at the floor", path.clone(), "(default 0)".into(), "return_gain_db: -60".into());
                    self.notes.push(format!("{path}: defaults to silence, but a route lifts it: written at the −60 dB floor"));
                }
            }
            None => {}
        }
        for fx in get(bus, "effects").map(Node::members).unwrap_or_default() {
            for m in fx.value.as_ref().and_then(|f| get(f, "params")).map(Node::members).unwrap_or_default() {
                self.param(&format!("buses.{id}.effects.{}.params.{}", fx.key.name, m.key.name), m, lifted);
            }
        }
    }

    /// A v0.8 doc without buses got an `rv` reverb from `global.reverb_mix`; v0.9 has no reverb_mix, so
    /// the bus is written out (its return at reverb_mix, every non-midi instrument without sends at 0 dB).
    fn synthesize_bus(&mut self, root: &Node) {
        let mix = self.ctx.doc.get("global").and_then(|g| g.get("reverb_mix")).and_then(Value::as_f64).unwrap_or(0.0);
        let (ret, _) = convert(Val::Gain, mix, false);
        let k = |n: &str| self.k(n);
        let reverb = format!(
            "{{ {}: \"reverb\", {}: {{ {}: 0.5, {}: 0.5, {}: 0, {}: 0.5, {}: 0 }} }}",
            k("type"), k("params"), k("room"), k("damping"), k("shimmer"), k("width"), k("predelay_ms")
        );
        let bus = format!("{{ {}: {ret}, {}: {{ {}: {reverb} }} }}", k("return_gain_db"), k("effects"), k(v08::DEFAULT_REVERB_FX));
        let ok = match get(root, "buses") {
            Some(b) => self.insert(b, v08::DEFAULT_BUS, &bus),
            None => self.insert(root, "buses", &format!("{{ {}: {bus} }}", k(v08::DEFAULT_BUS))),
        };
        if !ok {
            return;
        }
        self.change(format!("buses.{} written out", v08::DEFAULT_BUS), "buses".into(), format!("(none; global.reverb_mix {mix})"), format!("buses.{}.return_gain_db {ret}", v08::DEFAULT_BUS));
        self.notes.push(format!("buses.{}: written out — the reverb a doc without buses got from global.reverb_mix", v08::DEFAULT_BUS));
    }

    /// A route (top-level or a song layer's) at `path`: its dest renamed, its depth converted. A layer's
    /// partial entry takes the top-level route's dest (`fallback`).
    fn route(&mut self, path: &str, e: &Node, fallback: Option<String>) {
        let dest_node = get(e, "dest");
        let Some(dest) = dest_node.and_then(Node::as_str).map(str::to_string).or(fallback) else { return };
        let Some((new_dest, r, target)) = self.ctx.resolve(&dest) else { return };
        if let Some(dn) = dest_node.filter(|_| new_dest != dest) {
            self.replace_str(dn, &new_dest);
            self.change("dest", format!("{path}.dest"), format!("{dest:?}"), format!("{new_dest:?}"));
        }
        let Some(m) = e.member("depth") else { return };
        let Some(v) = &m.value else { return };
        let Some(d) = v.as_f64() else { return };
        let (base, authored) = self.ctx.base(&target);
        let (key, text, note) = convert_depth(r.dep, d, base);
        self.rename(m, key);
        self.change(format!("depth → {key}"), format!("{path}.depth"), self.text(v.span).to_string(), format!("{key}: {text}"));
        if matches!(r.dep, Dep::Gain | Dep::Drive) {
            let at = if authored { format!("at its base {base}") } else if self.ctx.part { format!("at an assumed base {base} (a part can't see the instrument)") } else { format!("at the default base {base}") };
            self.notes.push(format!("{path}: gain route {d} → {text} dB {at}: the source's +1 lands where it did; it sums in dB now, so it sounds different in between"));
        }
        self.note(path, note);
        self.replace(v, text, true);
    }

    /// `safe_bounds` / `constraints`: each key a path, each value `{ min, max }`.
    fn bands(&mut self, section: &str, bands: &Node, removed: &BTreeSet<String>) {
        for m in bands.members() {
            let key = m.key.name.as_str();
            let Some(band) = &m.value else { continue };
            if let Some(id) = key.strip_prefix("matrix.").and_then(|r| r.strip_suffix(".depth")) {
                if removed.contains(id) {
                    self.remove(&format!("{section}.{key}"));
                    continue;
                }
                // The route's dest: the top-level matrix's, or a song layer's that defines it.
                let layer_dest = || self.ctx.routes().into_iter().find(|(rid, _, _)| rid == id).map(|(_, dest, _)| dest);
                let Some(dest) = self.ctx.doc.get("matrix").and_then(|t| t.get(id)).and_then(|t| t.get("dest")).and_then(Value::as_str).map(str::to_string).or_else(layer_dest) else { continue };
                let dest = dest.as_str();
                let Some((_, r, target)) = self.ctx.resolve(dest) else { continue };
                let (base, _) = self.ctx.base(&target);
                let new_key = format!("matrix.{id}.{}", convert_depth(r.dep, 0.0, base).0);
                self.rename(m, &new_key);
                for side in ["min", "max"] {
                    if let Some(v) = get(band, side) {
                        if let Some(d) = v.as_f64() {
                            let (_, text, _) = convert_depth(r.dep, d, base);
                            self.change(format!("{section} depth band"), format!("{section}.{key}.{side}"), self.text(v.span).to_string(), format!("{new_key}.{side}: {text}"));
                            self.replace(v, text, true);
                        }
                    }
                }
                continue;
            }
            let Some((new_key, r, _)) = self.ctx.resolve(key) else { continue };
            self.rename(m, &new_key);
            for side in ["min", "max"] {
                let Some(v) = get(band, side) else { continue };
                let Some(x) = v.as_f64().filter(|_| !matches!(r.val, Val::Keep)) else { continue };
                let (text, _) = convert(r.val, x, false);
                self.change(format!("{section} band"), format!("{section}.{key}.{side}"), self.text(v.span).to_string(), format!("{new_key}.{side}: {text}"));
                self.replace(v, text, true);
            }
            if matches!(r.val, Val::Keep) {
                self.change(format!("{section} band"), format!("{section}.{key}"), key.to_string(), new_key.clone());
            }
        }
    }

    /// A part's `patch`: path → value set in the doc it merges into (`instruments.$target.sends.$id`).
    fn patch(&mut self, patch: &Node) {
        for m in patch.members() {
            let path = m.key.name.as_str();
            let Some(v) = &m.value else { continue };
            let send = path.strip_prefix("instruments.").and_then(|r| r.split_once(".sends.")).filter(|(_, bus)| !bus.contains('.'));
            let (new_key, val) = match (send, self.ctx.resolve(path)) {
                (Some((inst, bus)), _) => (format!("instruments.{inst}.sends_db.{bus}"), Val::Gain),
                (None, Some((new_key, r, _))) => (new_key, r.val),
                _ => continue,
            };
            self.rename(m, &new_key);
            match v.as_f64().filter(|_| !matches!(val, Val::Keep)) {
                Some(x) => {
                    let (text, _) = convert(val, x, false);
                    self.change("patch", format!("patch.{path}"), self.text(v.span).to_string(), format!("{new_key}: {text}"));
                    self.replace(v, text, true);
                }
                None => self.change("patch", format!("patch.{path}"), path.to_string(), new_key.clone()),
            }
        }
    }

    /// An objectives file: `constraints` bands, `overrides` values, `guardrails` ceilings and floors.
    fn objectives(&mut self, root: &Node) {
        if let Some(c) = get(root, "constraints") {
            self.bands("constraints", c, &BTreeSet::new());
        }
        let mut values = |section: &str, node: Option<&Node>| {
            for m in node.map(Node::members).unwrap_or_default() {
                let path = m.key.name.as_str();
                let Some((new_key, r, _)) = self.ctx.resolve(path) else { continue };
                self.rename(m, &new_key);
                let Some(v) = &m.value else { continue };
                match v.as_f64().filter(|_| !matches!(r.val, Val::Keep)) {
                    Some(x) => {
                        let (text, _) = convert(r.val, x, false);
                        self.change(section.to_string(), format!("{section}.{path}"), self.text(v.span).to_string(), format!("{new_key}: {text}"));
                        self.replace(v, text, true);
                    }
                    None => self.change(section.to_string(), format!("{section}.{path}"), path.to_string(), new_key.clone()),
                }
            }
        };
        values("overrides", get(root, "overrides"));
        let g = get(root, "guardrails");
        values("guardrails.ceilings", g.and_then(|g| get(g, "ceilings")));
        values("guardrails.floors", g.and_then(|g| get(g, "floors")));
    }

    /// Mark each comment that quotes a number near a number that changed: a trailing `//` on its line, or
    /// a whole-line `//` block right above a line whose member holds one (a route, a song layer…).
    fn tag_comments(&mut self) {
        let mut lines: BTreeSet<usize> = self.numeric_lines.iter().map(|&at| line_of(self.src, at)).collect();
        if let Some(root) = &self.cst.root {
            let mut holders = Vec::new();
            members_holding(root, &self.numeric_lines, &mut holders);
            lines.extend(holders.into_iter().map(|at| line_of(self.src, at)));
        }
        let mut tagged = BTreeSet::new();
        for (i, t) in self.cst.tokens.iter().enumerate() {
            if t.kind != TokenKind::LineComment {
                continue;
            }
            let text = t.span.text(self.src);
            if text.contains(TODO) || !quotes_a_number(text) {
                continue;
            }
            let whole_line = only_space_before(self.src, t.span.start);
            let trailing = !whole_line && lines.contains(&line_of(self.src, t.span.start));
            if (trailing || whole_line && self.block_quotes_above(i, &lines)) && tagged.insert(t.span.end) {
                self.edits.push(TextEdit::insert(t.span.end, format!(" {TODO}")));
            }
        }
    }

    /// Is token `i` (a whole-line comment) in a comment block directly above a changed line? (Each line
    /// of the block that quotes a number is tagged.)
    fn block_quotes_above(&self, i: usize, lines: &BTreeSet<usize>) -> bool {
        let mut line = line_of(self.src, self.cst.tokens[i].span.start);
        for t in self.cst.tokens[i + 1..].iter().filter(|t| t.kind != TokenKind::Whitespace) {
            let tl = line_of(self.src, t.span.start);
            if tl != line + 1 {
                return false; // a blank line (or more) ends the block
            }
            if t.kind != TokenKind::LineComment {
                return lines.contains(&tl);
            }
            line = tl;
        }
        false
    }
}

/// The key offsets of every member whose value holds one of `offsets`.
fn members_holding(n: &Node, offsets: &BTreeSet<usize>, out: &mut Vec<usize>) {
    for m in n.members() {
        if let Some(v) = &m.value {
            if offsets.range(v.span.start..v.span.end.max(v.span.start + 1)).next().is_some() {
                out.push(m.key.span.start);
                members_holding(v, offsets, out);
            }
        }
    }
    for it in n.items() {
        members_holding(&it.node, offsets, out);
    }
}

/// 0-based line of byte offset `at`.
fn line_of(src: &str, at: usize) -> usize {
    src[..at].bytes().filter(|&b| b == b'\n').count()
}

fn only_space_before(src: &str, at: usize) -> bool {
    src[..at].rsplit('\n').next().is_some_and(|l| l.trim().is_empty())
}

/// Does a comment quote a value-like number (`0.36`, `.5`)?
fn quotes_a_number(text: &str) -> bool {
    text.as_bytes().windows(2).any(|w| w[0] == b'.' && w[1].is_ascii_digit())
}

// ── files and pieces ─────────────────────────────────────────────────────────

/// Where a piece keeps the v0.8 originals of the files it migrated (paths mirror the piece's).
pub const BACKUP_DIR: &str = ".v0.8-backup";

/// One file's migration on disk.
#[derive(Clone, Debug, Serialize)]
pub struct FileResult {
    pub path: std::path::PathBuf,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub migration: Option<Migration>,
    /// The v0.9 text went to disk (never on a dry run, an unchanged file or a refusal).
    pub written: bool,
    /// Where the v0.8 original was kept (piece mode).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup: Option<std::path::PathBuf>,
    /// Why the file wasn't migrated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Refused (its content), not an I/O failure.
    #[serde(skip)]
    pub refused: bool,
    /// The input text (for `--dry-run`'s diff).
    #[serde(skip)]
    pub before: String,
}

/// Migrate the file at `path`, writing it atomically unless `dry_run`. `doc` is the v0.8 loop doc an
/// objectives file steers; `backup` keeps the original there first (never over an older backup).
pub fn migrate_file(path: &std::path::Path, doc: Option<&str>, dry_run: bool, backup: Option<std::path::PathBuf>) -> FileResult {
    let mut r = FileResult { path: path.to_path_buf(), migration: None, written: false, backup: None, error: None, refused: false, before: String::new() };
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            r.error = Some(e.to_string());
            return r;
        }
    };
    let m = match migrate_with(&src, doc) {
        Ok(m) => m,
        Err(e) => {
            r.error = Some(e);
            r.refused = true;
            return r;
        }
    };
    r.before = src;
    if !m.unchanged && !dry_run {
        if let Some(b) = backup {
            if !b.exists() {
                let kept = b.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&b, &r.before));
                if let Err(e) = kept {
                    r.error = Some(format!("backup {}: {e}", b.display()));
                    r.migration = Some(m);
                    return r;
                }
            }
            r.backup = Some(b);
        }
        match engines_core::piece::write_atomic(path, m.text.as_bytes()) {
            Ok(()) => r.written = true,
            Err(e) => r.error = Some(e.to_string()),
        }
    }
    r.migration = Some(m);
    r
}

/// Migrate a whole piece: its doc, every `.live/history` snapshot (so a revert brings back v0.9 text)
/// and any `*.objectives.json5` beside the doc (against the v0.8 doc), each v0.8 original kept under
/// [`BACKUP_DIR`]. `renders/`, `ableton/` and everything else are never touched. The history index
/// keeps the v0.8 hashes it recorded: they name the backups, the text that played.
pub fn migrate_piece(piece: &engines_core::piece::Piece, dry_run: bool) -> Vec<FileResult> {
    let backup = |p: &std::path::Path| p.strip_prefix(&piece.root).ok().map(|rel| piece.root.join(BACKUP_DIR).join(rel));
    let doc_path = piece.doc_path();
    let doc_v08 = {
        // The v0.8 doc the objectives steer: the doc, or its backup once it's migrated.
        let kept = backup(&doc_path).filter(|b| b.is_file()).unwrap_or_else(|| doc_path.clone());
        std::fs::read_to_string(kept).ok()
    };
    let mut out = vec![migrate_file(&doc_path, None, dry_run, backup(&doc_path))];
    let listed = |dir: &std::path::Path, keep: &dyn Fn(&str) -> bool| -> Vec<std::path::PathBuf> {
        let mut files: Vec<_> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_file() && p.file_name().and_then(|n| n.to_str()).is_some_and(keep)).collect();
        files.sort();
        files
    };
    for p in listed(&piece.root, &|n| n.ends_with(".objectives.json5")) {
        out.push(migrate_file(&p, doc_v08.as_deref(), dry_run, backup(&p)));
    }
    for p in listed(&piece.history_dir(), &|n| n.ends_with(".json5")) {
        out.push(migrate_file(&p, None, dry_run, backup(&p)));
    }
    out
}

// ── reports ──────────────────────────────────────────────────────────────────

/// A line diff of `old` → `new` (`-`/`+` lines with their line numbers), for `--dry-run`.
pub fn diff(old: &str, new: &str) -> String {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    // LCS table over lines (docs are a few thousand lines at most).
    let mut lcs = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, String::new());
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            i += 1;
            j += 1;
        } else if i < a.len() && (j == b.len() || lcs[i + 1][j] >= lcs[i][j + 1]) {
            out.push_str(&format!("-{:>5} {}\n", i + 1, a[i]));
            i += 1;
        } else {
            out.push_str(&format!("+{:>5} {}\n", j + 1, b[j]));
            j += 1;
        }
    }
    out
}

/// Counts of a migration's changes by kind (`cutoff → cutoff_hz` ×3), for a summary.
pub fn summary(m: &Migration) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for c in &m.changes {
        *out.entry(c.label.clone()).or_insert(0) += 1;
    }
    out
}

#[cfg(test)]
mod tests;
