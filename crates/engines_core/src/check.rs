//! Doc checking (ADR 0033): every problem in a loop doc, each located by a JSONPath.
//!
//! [`check_str`] is the authoring boundary. It layers four passes:
//! 1. **syntax** — JSON5;
//! 2. **schema** ([`crate::schema::walk`]) on the raw value — unknown keys (did-you-mean), enum
//!    values, types, ranges: what serde would otherwise drop or default silently;
//! 3. **structure** ([`LoopDoc::validate_all`]) — the engine's own hard rules (midi routing, phrase
//!    arms, buses, songs…);
//! 4. **semantics** (here) — what the engine would silently ignore: an unparseable root or mode,
//!    routes and sends to things that don't exist, params a synth type never reads, a silent
//!    instrument or bus.
//!
//! Errors make [`LoopDoc::from_json_str`] refuse the doc (a live edit keeps the previous one playing);
//! warnings ride along (`engines-check`, the TUI status line, the LSP).

use crate::loop_doc::{resolve_dest, Dest, LoopDoc};
use serde::Serialize;
use crate::doc_value::{from_doc_value, to_doc_value, DocValue};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

/// One located problem.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DocDiag {
    pub severity: Severity,
    /// JSONPath of the offending node (`instruments.pad.params.cutof`, `modulators.arp.steps[2]`);
    /// empty for doc-wide problems.
    pub path: String,
    /// Stable machine code (`syntax`, `unknown-key`, `enum`, `range`, `type`, `missing`, `dangling`,
    /// `no-effect`, `silent`, `invalid`, `lint`…).
    pub code: String,
    pub message: String,
    /// The message already names its subject (the engine's legacy messages) — render it bare.
    #[serde(skip)]
    pub(crate) standalone: bool,
}

impl DocDiag {
    pub fn error(path: &str, code: &str, message: impl Into<String>) -> Self {
        Self { severity: Severity::Error, path: path.to_string(), code: code.to_string(), message: message.into(), standalone: false }
    }
    pub fn warning(path: &str, code: &str, message: impl Into<String>) -> Self {
        Self { severity: Severity::Warning, path: path.to_string(), code: code.to_string(), message: message.into(), standalone: false }
    }
    /// A legacy engine message that already names its subject (`instrument 'x': …`).
    pub(crate) fn standalone(mut self) -> Self {
        self.standalone = true;
        self
    }
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

impl fmt::Display for DocDiag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.standalone || self.path.is_empty() {
            write!(f, "{}", self.message)
        } else {
            write!(f, "{}: {}", self.path, self.message)
        }
    }
}

/// The result of checking a doc: the parsed doc when it deserialized (even with errors), and every
/// diagnostic, errors first.
#[derive(Debug)]
pub struct Checked {
    pub doc: Option<LoopDoc>,
    pub diags: Vec<DocDiag>,
}

impl Checked {
    pub fn errors(&self) -> impl Iterator<Item = &DocDiag> {
        self.diags.iter().filter(|d| d.is_error())
    }
    pub fn warnings(&self) -> impl Iterator<Item = &DocDiag> {
        self.diags.iter().filter(|d| !d.is_error())
    }
    pub fn has_errors(&self) -> bool {
        self.errors().next().is_some()
    }
}

/// The closest option to `word` (edit distance ≤ ⅓ of the word, at least 1), if any.
pub fn did_you_mean<'a>(word: &str, options: &[&'a str]) -> Option<&'a str> {
    let limit = (word.chars().count() / 3).max(1);
    options
        .iter()
        .map(|o| (levenshtein(&word.to_ascii_lowercase(), &o.to_ascii_lowercase()), *o))
        .filter(|(d, _)| *d <= limit)
        .min_by_key(|(d, o)| (*d, o.len()))
        .map(|(_, o)| o)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != *cb)).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Check a loop doc's source text (JSON5).
pub fn check_str(src: &str) -> Checked {
    match dups::parse(src) {
        Ok((raw, dups)) => {
            let mut checked = check_value(raw);
            checked.diags.extend(dups.iter().map(|p| duplicate_key(p)));
            checked.diags = sorted(checked.diags);
            checked
        }
        Err(e) => Checked { doc: None, diags: vec![DocDiag::error("", "syntax", e.to_string())] },
    }
}

/// The JSONPath of every repeated key in `src` (empty if it doesn't parse) — for a caller that
/// parsed the text itself and checks via [`check_value`] (the language server's CST).
pub fn duplicate_keys(src: &str) -> Vec<String> {
    dups::parse(src).map(|(_, d)| d).unwrap_or_default()
}

/// A key repeated in its object (at `path`, the repeat's JSONPath): JSON5 keeps only the last one.
pub fn duplicate_key(path: &str) -> DocDiag {
    let key = path.rsplit('.').next().unwrap_or(path);
    DocDiag::error(path, "duplicate", format!("`{key}` appears twice in this object — only the last one counts, the earlier one is silently dropped"))
}

/// Check an already-parsed doc (passes 2–4 of [`check_str`]) — for a caller that parsed the JSON5
/// itself (the language server's CST).
/// The schema version this engine reads (ADR 0054: a clean break — only `engines-migrate` knows 0.8).
pub const SCHEMA_VERSION: &str = "0.9";

/// The refusal of a doc in another schema: `schema 0.8: run engines-migrate <doc>`.
pub fn version_error(found: &str) -> DocDiag {
    let message = match found {
        "0.8" => "schema 0.8: run `engines-migrate <doc>` (this engine reads schema 0.9: seconds, Hz and dB — ADR 0054)".to_string(),
        other => format!("schema {other}: this engine reads schema {SCHEMA_VERSION} (`engines-migrate` converts a 0.8 doc)"),
    };
    DocDiag::error("schema_version", "version", message)
}

pub fn check_value(raw: DocValue) -> Checked {
    // Another schema's doc reads wrongly everywhere (every renamed key an error): refuse it whole.
    if let Some(v) = raw.get("schema_version").and_then(DocValue::as_str).filter(|v| *v != SCHEMA_VERSION) {
        return Checked { doc: None, diags: vec![version_error(v)] };
    }
    let mut schema_diags = Vec::new();
    crate::schema::walk(&raw, &mut schema_diags);
    let doc = match LoopDoc::from_value(raw.clone()) {
        Ok(d) => d,
        Err(e) => {
            if schema_diags.iter().all(|d| !d.is_error()) {
                schema_diags.push(DocDiag::error("", "type", e.to_string()));
            }
            return Checked { doc: None, diags: sorted(schema_diags) };
        }
    };
    let mut diags = Vec::new();
    doc.validate_all(&mut diags);
    semantic(&doc, &raw, &mut diags);
    // The engine's own (structural/semantic) finding wins over the schema's for the same node.
    for d in schema_diags {
        if !(d.is_error() && diags.iter().any(|x| x.is_error() && x.path == d.path)) {
            diags.push(d);
        }
    }
    for w in doc.lint() {
        diags.push(DocDiag::warning("", "lint", w));
    }
    Checked { doc: Some(doc), diags: sorted(diags) }
}

/// Errors first, otherwise in discovery order.
fn sorted(mut diags: Vec<DocDiag>) -> Vec<DocDiag> {
    diags.sort_by_key(|d| d.severity);
    diags
}

/// Whether the engine reads param `leaf` on an instrument of synth type `kind` (`subtractive` /
/// `dual` / `modal` / `binaural` / `midi`; `noise` = its source is `filtered_noise`; `pitched` = it says
/// `pitched: true` — a pitched subtractive noise instrument also reads `resonance`, ADR 0046). `midi`
/// instruments hold free-form values, so everything applies.
pub fn param_applies(kind: &str, noise: bool, pitched: bool, leaf: &str) -> bool {
    const SHARED: &[&str] = &["level_db", "drive_db"];
    const ENVELOPE: &[&str] = &["attack_s", "decay_s", "sustain", "release_s"];
    const FILTER: &[&str] = &["cutoff_hz", "resonance"];
    const BAND: &[&str] = &["band_lo_hz", "band_hi_hz"];
    if SHARED.contains(&leaf) {
        return true;
    }
    match kind {
        "subtractive" => {
            matches!(leaf, "wave" | "source")
                || ENVELOPE.contains(&leaf)
                || if noise { BAND.contains(&leaf) || (pitched && leaf == "resonance") } else { FILTER.contains(&leaf) || matches!(leaf, "voices_detune_cents" | "shift_st" | "shift_mix") }
        }
        "dual" => {
            ENVELOPE.contains(&leaf)
                || matches!(leaf, "voices_detune_cents" | "osc_a_wave" | "osc_b_wave" | "osc_a_level_db" | "osc_b_level_db" | "osc_b_ratio" | "osc_b_detune_cents" | "fm_index" | "fm_feedback" | "wave" | "source")
                || if noise { BAND.contains(&leaf) } else { FILTER.contains(&leaf) }
        }
        "modal" => matches!(leaf, "material" | "num_modes" | "brightness" | "strike_pos" | "t60_s" | "ring_ratio" | "stretch" | "t60_mode"),
        // Two subtractive voices hard L/R (ADR 0047) — never noise (a validate() error), so `noise`
        // doesn't narrow it.
        "binaural" => {
            ENVELOPE.contains(&leaf)
                || FILTER.contains(&leaf)
                || matches!(leaf, "wave" | "source" | "voices_detune_cents" | "shift_st" | "shift_mix" | "root_hz" | "spread_hz")
        }
        _ => true, // midi: free-form value holders
    }
}

/// Every `harmony.mode` spelling the engine accepts (aliases included).
pub const MODES: &[&str] = &["ionian", "major", "dorian", "phrygian", "lydian", "mixolydian", "aeolian", "minor", "locrian", "harmonic_minor", "melodic_minor"];
/// The LFO waveshapes (anything else plays as a sine).
pub const LFO_WAVES: &[&str] = &["sine", "triangle", "saw_soft", "square", "pulse", "sine_bell"];

fn semantic(doc: &LoopDoc, raw: &DocValue, out: &mut Vec<DocDiag>) {
    // ── harmony: the engine silently falls back to C / ionian ──
    if crate::voice::try_root_semitone(&doc.harmony.root).is_none() {
        let fix = capitalized_root(&doc.harmony.root).map(|r| format!(" — did you mean \"{r}\"?")).unwrap_or_default();
        out.push(DocDiag::error("harmony.root", "invalid", format!("unknown root \"{}\" (a capital letter A–G, optionally # or b){fix}", doc.harmony.root)));
    }
    if crate::theory::scale::Mode::try_from_name(&doc.harmony.mode).is_none() {
        let hint = did_you_mean(&doc.harmony.mode, MODES).map(|m| format!(" — did you mean \"{m}\"?")).unwrap_or_default();
        out.push(DocDiag::error("harmony.mode", "invalid", format!("unknown mode \"{}\" (one of: {}){hint}", doc.harmony.mode, MODES.join(", "))));
    }

    // ── instruments ──
    for (name, inst) in &doc.instruments {
        let base = format!("instruments.{name}");
        let kind = inst.synth_type();
        // A `midi` instrument sends MIDI, not audio — and only `engines-play --midi` sends it.
        if inst.is_midi() {
            out.push(DocDiag::warning(&format!("{base}.type"), "silent", "a `midi` instrument makes no sound in `engines-live`: it plays only through `engines-play --midi`"));
        }
        if inst.note_engine.polyphony == 0 {
            let how = if raw.pointer(&format!("/instruments/{name}/note_engine")).is_some() { "polyphony 0" } else { "no `note_engine` (polyphony defaults to 0)" };
            out.push(DocDiag::warning(&format!("{base}.note_engine"), "silent", format!("{how}: this instrument never sounds — give it `note_engine: {{ polyphony: N }}`")));
        }
        let noise = inst.wave() == Some("filtered_noise");
        if kind != "midi" {
            for leaf in inst.params.keys() {
                if crate::params::spec_for(leaf).is_some() || matches!(leaf.as_str(), "source" | "t60_mode") {
                    if !param_applies(kind, noise, inst.pitched == Some(true), leaf) {
                        let what = if kind == "subtractive" && noise { "a filtered_noise instrument".to_string() } else { format!("a {kind} instrument") };
                        // An unpitched noise's `resonance` is one `pitched: true` away from doing something.
                        let hint = if kind == "subtractive" && noise && leaf == "resonance" { " unless it's `pitched: true` (then it raises the note in the noise's band)" } else { "" };
                        out.push(DocDiag::warning(&format!("{base}.params.{leaf}"), "no-effect", format!("`{leaf}` has no effect on {what}{hint}")));
                    }
                }
            }
        }
        // A modal ring is `t60_s` (`t60_mode: "fixed"`) or `ring_ratio` (`"note"`), never the other
        // (ADR 0054 P7: v0.8's one `t60_scale` meant either).
        if kind == "modal" {
            let note = inst.string("t60_mode") == Some("note");
            let (wrong, right, mode) = if note { ("t60_s", "ring_ratio", "note") } else { ("ring_ratio", "t60_s", "fixed") };
            if inst.params.contains_key(wrong) {
                out.push(DocDiag::error(&format!("{base}.params.{wrong}"), "invalid", format!("`t60_mode: \"{mode}\"` rings by `{right}` — `{wrong}` is the other mode's")));
            }
        }
        if kind == "binaural" {
            binaural_checks(doc, raw, name, inst, out);
        }
        for bus in inst.sends_db.keys() {
            let path = format!("{base}.sends_db.{bus}");
            match doc.buses.get(bus) {
                None => {
                    let ids: Vec<&str> = doc.buses.keys().map(String::as_str).collect();
                    let hint = did_you_mean(bus, &ids).map(|m| format!(" — did you mean `{m}`?")).unwrap_or_default();
                    out.push(DocDiag::error(&path, "dangling", format!("send to unknown bus `{bus}`{hint}")));
                }
                Some(b) if b.is_external_return() => out.push(DocDiag::error(&path, "dangling", format!("`{bus}` is an external_return bus — it takes no sends"))),
                _ => {}
            }
        }
    }

    // ── ids: a JSONPath segment each (`matrix.<id>.depth`, `instruments.<id>.params.…`) ──
    let mut ids: Vec<(String, &str)> = Vec::new();
    ids.extend(doc.instruments.keys().map(|k| ("instruments".to_string(), k.as_str())));
    ids.extend(doc.modulators.keys().map(|k| ("modulators".to_string(), k.as_str())));
    ids.extend(doc.matrix.keys().map(|k| ("matrix".to_string(), k.as_str())));
    ids.extend(doc.sequence_pools.keys().map(|k| ("sequence_pools".to_string(), k.as_str())));
    for (bus, b) in &doc.buses {
        ids.push(("buses".into(), bus.as_str()));
        ids.extend(b.effects.keys().map(|k| (format!("buses.{bus}.effects"), k.as_str())));
    }
    if let Some(spec) = &doc.song {
        for (layer, routes) in &spec.matrices {
            ids.push(("song.matrices".into(), layer.as_str()));
            ids.extend(routes.keys().map(|k| (format!("song.matrices.{layer}"), k.as_str())));
        }
    }
    for (map, id) in ids.into_iter().filter(|(_, id)| id.contains('.')) {
        out.push(DocDiag::error(&format!("{map}.{id}"), "invalid", format!("id `{id}` contains `.` — ids are path segments (`{map}.<id>…`), so routes, bounds and overrides can't name it")));
    }

    // ── modulators ──
    for (id, m) in &doc.modulators {
        // A note step at or past `length` is never reached (the grid wraps at `length`).
        if m.kind == "step_seq" && m.output != "control" && m.phrase.is_none() && m.generator.is_none() {
            let length = m.length.max(1);
            for (i, s) in m.steps.iter().enumerate().filter(|(_, s)| s.step >= length) {
                out.push(DocDiag::warning(&format!("modulators.{id}.steps[{i}].step"), "no-effect", format!("step {} is past the end of this {length}-step sequence (0–{}) — it never plays", s.step, length - 1)));
            }
        }
        if m.kind == "lfo" {
            if let Some(w) = &m.wave {
                if !LFO_WAVES.contains(&w.as_str()) {
                    let hint = did_you_mean(w, LFO_WAVES).map(|x| format!(" — did you mean \"{x}\"?")).unwrap_or_default();
                    out.push(DocDiag::error(&format!("modulators.{id}.wave"), "enum", format!("unknown LFO wave \"{w}\" (it would play as a sine){hint}")));
                }
            }
        }
    }

    // ── matrix: every route must reach something that exists and listens ──
    let mods: Vec<&str> = doc.modulators.keys().map(String::as_str).collect();
    for (id, e) in &doc.matrix {
        let base = format!("matrix.{id}");
        if !doc.modulators.contains_key(&e.source) {
            let hint = did_you_mean(&e.source, &mods).map(|m| format!(" — did you mean `{m}`?")).unwrap_or_default();
            out.push(DocDiag::error(&format!("{base}.source"), "dangling", format!("unknown modulator `{}`{hint}", e.source)));
        }
        check_route_dest(doc, &base, e, out);
        check_depth_keys(&base, &raw["matrix"][id.as_str()], Some(&e.dest), out);
    }
    // A song layer's route that names its own `dest` gets the same check (the layer is merged over
    // the base route of that id, as a block plays it); a depth-only patch keeps the base's dest,
    // checked above. The structural pass (`song.rs`) already refused a dangling layer route — then
    // only the warnings are new.
    if let Some(spec) = &doc.song {
        let structural = out.iter().any(|d| d.is_error() && d.path == "song");
        let mut layer_diags = Vec::new();
        for (layer, routes) in &spec.matrices {
            for (id, patch) in routes {
                let dest = patch.get("dest").and_then(DocValue::as_str).or_else(|| doc.matrix.get(id).map(|e| e.dest.as_str()));
                check_depth_keys(&format!("song.matrices.{layer}.{id}"), patch, dest, &mut layer_diags);
                if patch.get("dest").is_none() {
                    continue;
                }
                let mut v = doc.matrix.get(id).and_then(|e| to_doc_value(e).ok()).unwrap_or(DocValue::Object(Default::default()));
                crate::song::merge_patch(&mut v, patch);
                if let Ok(e) = from_doc_value::<crate::loop_doc::MatrixEntry>(v) {
                    check_route_dest(doc, &format!("song.matrices.{layer}.{id}"), &e, &mut layer_diags);
                }
            }
        }
        out.extend(layer_diags.into_iter().filter(|d| !(structural && d.is_error())));
    }

    // ── buses ──
    for (id, b) in &doc.buses {
        if b.is_external_return() {
            continue;
        }
        let authored = raw.pointer(&format!("/buses/{id}"));
        if authored.is_some_and(|bus| bus.get("return_gain_db").is_none()) {
            out.push(DocDiag::warning(&format!("buses.{id}"), "silent", "no `return_gain_db` (defaults to -Infinity): this bus's wet signal is silent"));
        }
        for (fx, e) in &b.effects {
            if e.kind != "reverb" {
                out.push(DocDiag::error(&format!("buses.{id}.effects.{fx}.type"), "enum", format!("unknown effect type \"{}\" (only \"reverb\" exists; it would be skipped)", e.kind)));
            }
        }
    }

    // ── bound keys must name a param that exists; a band can't be inverted ──
    for (field, bands) in [("safe_bounds", &doc.safe_bounds), ("constraints", &doc.constraints)] {
        for (path, band) in bands {
            if doc.get_param(path).is_none() {
                out.push(DocDiag::warning(&format!("{field}.{path}"), "dangling", format!("`{path}` names no numeric param in this doc — the band is unused")));
            }
            if let (Some(lo), Some(hi)) = (band.min, band.max) {
                if lo > hi {
                    out.push(DocDiag::error(&format!("{field}.{path}"), "range", format!("min {lo} is above max {hi} (use min == max to freeze a param)")));
                }
            }
            // A band's min may be silence only on a gain (ADR 0054 P3).
            if band.min == Some(f32::NEG_INFINITY) && !crate::params::spec_for_path(path).is_some_and(|s| s.is_gain()) {
                out.push(DocDiag::error(&format!("{field}.{path}.min"), "type", format!("-Infinity (silence) is only for a gain — `{path}` isn't one")));
            }
            if field != "safe_bounds" {
                continue;
            }
            // One explicit side merges with the Tier-2 default on the other (ADR 0010); a side beyond
            // the default's other side collapses the band onto it — say what it becomes.
            let (dlo, dhi) = crate::loop_doc::safe_range_default(path);
            let (lo, hi) = doc.safe_range(path);
            let at = format!("{field}.{path}");
            match (band.min, band.max) {
                (None, Some(m)) if m < dlo => out.push(DocDiag::warning(&at, "range", format!("max {m} is below the default safe floor {dlo}: the effective band is [{lo}, {hi}]"))),
                (Some(m), None) if m > dhi => out.push(DocDiag::warning(&at, "range", format!("min {m} is above the default safe ceiling {dhi}: the effective band is [{lo}, {hi}]"))),
                _ => {}
            }
            // An explicit side is a hard clamp: an authored value beyond it never plays as written.
            if let Some(v) = doc.get_param(path) {
                let clamped = if band.min.is_some() && v < lo { Some(lo) } else if band.max.is_some() && v > hi { Some(hi) } else { None };
                if let Some(c) = clamped {
                    out.push(DocDiag::warning(&at, "range", format!("`{path}` is authored {v}, outside this band [{lo}, {hi}] — it plays clamped to {c}")));
                }
            }
        }
    }

    // ── movement: an explicitly bound id must be a modulator (unbound defaults may be absent) ──
    if let Some(m) = &doc.movement {
        let b = &m.bind;
        let slots = [("chord", &b.chord), ("bass", &b.bass), ("arp", &b.arp)]
            .into_iter()
            .map(|(key, id)| (key.to_string(), key, id))
            .chain(b.oneshot.iter().enumerate().map(|(i, id)| (format!("oneshot[{i}]"), "oneshot", id)));
        for (slot, key, id) in slots {
            if raw.pointer(&format!("/movement/bind/{key}")).is_some() && !doc.modulators.contains_key(id) {
                let hint = did_you_mean(id, &mods).map(|x| format!(" — did you mean `{x}`?")).unwrap_or_default();
                out.push(DocDiag::warning(&format!("movement.bind.{slot}"), "dangling", format!("unknown modulator `{id}`: the Orrery won't drive it{hint}")));
            }
        }
    }
}

/// A `binaural` instrument's advisories (ADR 0047): a pair with no spread doesn't beat, a send blends
/// the isolated L/R in the bus, and a pinned `root_hz` with undetuned unison stacks identical pairs.
fn binaural_checks(doc: &LoopDoc, raw: &DocValue, name: &str, inst: &crate::loop_doc::Instrument, out: &mut Vec<DocDiag>) {
    let base = format!("instruments.{name}");
    let spread_dest = format!("{base}.params.spread_hz");
    if inst.authored_num("spread_hz").unwrap_or(0.0) == 0.0 && !routed(doc, &spread_dest) {
        let at = if inst.params.contains_key("spread_hz") { spread_dest } else { format!("{base}.params") };
        out.push(DocDiag::warning(
            &at,
            "no-effect",
            "no `spread_hz`: both sides play the root, so nothing beats — set the beat rate in Hz (e.g. `spread_hz: 4`, theta)",
        ));
    }
    if let Some(sends) = raw.pointer(&format!("/instruments/{name}/sends_db")).and_then(DocValue::as_object) {
        for (bus, amt) in sends {
            if amt.as_f64().is_some_and(|a| a > f64::NEG_INFINITY) {
                out.push(DocDiag::warning(
                    &format!("{base}.sends_db.{bus}"),
                    "no-effect",
                    format!("a send blends this binaural pair's L and R in `{bus}` — the wet part beats in the room, not between the ears"),
                ));
            }
        }
    }
    if inst.params.contains_key("root_hz") && inst.note_engine.polyphony > 1 && inst.authored_num("voices_detune_cents").unwrap_or(0.0) == 0.0 {
        out.push(DocDiag::warning(
            &format!("{base}.note_engine.polyphony"),
            "no-effect",
            format!(
                "with `root_hz` set every voice sits on the same root — {} identical pairs; add `voices_detune_cents` to spread them, or use polyphony 1",
                inst.note_engine.polyphony
            ),
        ));
    }
}

/// Does any route — the doc's matrix or a song layer — target `dest`?
fn routed(doc: &LoopDoc, dest: &str) -> bool {
    doc.matrix.values().any(|e| e.dest == dest)
        || doc.song.as_ref().is_some_and(|s| {
            s.matrices.values().flat_map(|l| l.values()).any(|e| e.get("dest").and_then(DocValue::as_str) == Some(dest))
        })
}

/// [`check_dest`] for route `e` at `base`, plus the silent-base warning (ADR 0054 P5).
fn check_route_dest(doc: &LoopDoc, base: &str, e: &crate::loop_doc::MatrixEntry, out: &mut Vec<DocDiag>) {
    check_dest(doc, base, e.kind.as_str(), &e.dest, out);
    // A route adds to its param in dB: onto a `-Infinity` base it stays silent.
    if e.kind == "mod" && e.depth != 0.0 && silent_base(doc, &e.dest) {
        out.push(DocDiag::warning(
            &format!("{base}.dest"),
            "no-effect",
            format!("the base of `{}` is -Infinity, so this route is inaudible — give it a finite floor (say -60 dB) and a `depth_db`", e.dest),
        ));
    }
}

/// Is `dest` a gain whose base is `-Infinity` (authored, or the default of `osc_b_level_db` and
/// `return_gain_db`)?
fn silent_base(doc: &LoopDoc, dest: &str) -> bool {
    match resolve_dest(dest) {
        Dest::InstParam { inst, param } if crate::params::spec_for(&param).is_some_and(|s| s.is_gain()) => doc
            .instruments
            .get(&inst)
            .map(|i| i.native(&param).or_else(|| crate::params::default_num(&param)))
            .is_some_and(|v| v == Some(f32::NEG_INFINITY)),
        Dest::Global { param } if param == "master_gain_db" => doc.global.master_gain_db == f32::NEG_INFINITY,
        Dest::BusReturn { bus } => doc.buses.get(&bus).is_some_and(|b| b.return_gain_db == f32::NEG_INFINITY),
        _ => false,
    }
}

/// A route's depth keys (ADR 0054 P4): exactly one, in its dest's unit — `depth_oct` into a
/// cutoff, `depth_db` into a gain, plain `depth` into a `0..1` param. `route` is the raw entry (a
/// song layer's patch takes the base route's dest when it doesn't name one).
fn check_depth_keys(path: &str, route: &DocValue, dest: Option<&str>, out: &mut Vec<DocDiag>) {
    let Some(m) = route.as_object() else { return };
    let keys: Vec<&str> = m.keys().map(String::as_str).filter(|k| crate::params::DepthUnit::from_key(k).is_some()).collect();
    if keys.len() > 1 {
        out.push(DocDiag::error(path, "invalid", format!("a route has one depth key — found {}", keys.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(" and "))));
        return;
    }
    let (Some(key), Some(dest)) = (keys.first(), dest) else { return };
    let want = crate::loop_doc::depth_unit_for(dest);
    if want.key() != *key {
        let unit = match want {
            crate::params::DepthUnit::Plain => "a fraction of its 0..1".to_string(),
            u => format!("{} (its unit)", u.key().trim_start_matches("depth_")),
        };
        out.push(DocDiag::error(&format!("{path}.{key}"), "invalid", format!("a route into `{}` takes `{}` — {unit}", crate::params::units::leaf(dest), want.key())));
    }
}

fn check_dest(doc: &LoopDoc, base: &str, kind: &str, dest: &str, out: &mut Vec<DocDiag>) {
    let path = format!("{base}.dest");
    let inst_known = |inst: &str, out: &mut Vec<DocDiag>| -> bool {
        if doc.instruments.contains_key(inst) {
            return true;
        }
        let ids: Vec<&str> = doc.instruments.keys().map(String::as_str).collect();
        let hint = did_you_mean(inst, &ids).map(|m| format!(" — did you mean `{m}`?")).unwrap_or_default();
        out.push(DocDiag::error(&path, "dangling", format!("unknown instrument `{inst}`{hint}")));
        false
    };
    match resolve_dest(dest) {
        Dest::Notes { inst } => {
            if kind != "note" {
                out.push(DocDiag::error(&path, "invalid", "a `mod` route can't target `.notes` (use kind \"note\")"));
            }
            inst_known(&inst, out);
        }
        _ if kind == "note" => out.push(DocDiag::error(&path, "invalid", format!("a `note` route must target `instruments.<id>.notes`, not `{dest}`"))),
        Dest::InstParam { inst, param } => {
            if dest.split('.').count() != 4 {
                out.push(DocDiag::error(&path, "invalid", format!("`{dest}` — expected instruments.<id>.params.<param>")));
            } else if inst_known(&inst, out) {
                let i = &doc.instruments[&inst];
                let routed_cc = i.is_midi() && i.routing.as_ref().is_some_and(|r| r.cc.contains_key(&param));
                if !routed_cc && !crate::engine::MODULATABLE_INST_PARAMS.contains(&param.as_str()) {
                    out.push(DocDiag::warning(&path, "no-effect", format!("`{param}` is not matrix-modulatable — this route does nothing")));
                } else if !i.is_midi() && !param_applies(i.synth_type(), i.wave() == Some("filtered_noise"), i.pitched == Some(true), &param) {
                    out.push(DocDiag::warning(&path, "no-effect", format!("`{param}` has no effect on a {} instrument — this route does nothing", i.synth_type())));
                } else if param == "root_hz" && !i.params.contains_key("root_hz") {
                    // ADR 0047: without an authored root_hz the root follows the note; the engine
                    // ignores a route into the unset param rather than modulate around 20 Hz.
                    out.push(DocDiag::warning(
                        &path,
                        "no-effect",
                        format!("`{inst}` has no `root_hz` — its root follows the note, so this route does nothing; author a `root_hz` to modulate around"),
                    ));
                }
            }
        }
        Dest::Global { param } => match param.as_str() {
            "master_gain_db" | "stereo_width" => {}
            other => out.push(DocDiag::error(&path, "invalid", format!("unknown global `{other}` (master_gain_db or stereo_width)"))),
        },
        Dest::BusReturn { bus } => match doc.buses.get(&bus) {
            None => out.push(DocDiag::error(&path, "dangling", format!("unknown bus `{bus}`"))),
            Some(b) if b.is_external_return() => out.push(DocDiag::error(&path, "invalid", format!("`{bus}` is an external_return bus — not modulatable"))),
            _ => {}
        },
        Dest::BusParam { bus, fx, param } => match doc.buses.get(&bus) {
            None => out.push(DocDiag::error(&path, "dangling", format!("unknown bus `{bus}`"))),
            Some(b) => match b.effects.get(&fx) {
                None => out.push(DocDiag::error(&path, "dangling", format!("bus `{bus}` has no effect `{fx}`"))),
                Some(_) if !crate::effects::REVERB_PARAM_LEAVES.contains(&param.as_str()) => {
                    let hint = did_you_mean(&param, &crate::effects::REVERB_PARAM_LEAVES).map(|m| format!(" — did you mean `{m}`?")).unwrap_or_default();
                    out.push(DocDiag::error(&path, "invalid", format!("unknown reverb param `{param}`{hint}")));
                }
                _ => {}
            },
        },
        Dest::Unknown => out.push(DocDiag::error(&path, "invalid", format!("`{dest}` is not a destination (instruments.<id>.params.<p> | instruments.<id>.notes | global.<p> | buses.<id>.return_gain_db | buses.<id>.effects.<fx>.params.<p>)"))),
    }
}

/// `eb` → `Eb`, `f#` → `F#`: the engine only accepts a capital letter with an accidental.
fn capitalized_root(r: &str) -> Option<String> {
    let mut c = r.trim().chars();
    let first = c.next()?.to_ascii_uppercase();
    let fixed: String = std::iter::once(first).chain(c).collect();
    crate::voice::try_root_semitone(&fixed).map(|_| fixed)
}

mod dups;
#[cfg(test)]
mod tests;
