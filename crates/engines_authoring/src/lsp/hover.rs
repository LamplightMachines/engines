//! Hover: what the thing under the cursor means — in the schema's words, in native units, in the
//! doc's key, and in time.

use super::refs::{IdKind, IdRef};
use super::schema_nav::{description, schema_at, type_summary};
use super::{music, readings, Analysis};
use crate::cst::path::{render, Seg, Target};
use crate::cst::{Node, Span};
use engines_core::params::{spec_for, units};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hover {
    pub span: Span,
    pub markdown: String,
}

pub fn hover(a: &Analysis, offset: usize) -> Option<Hover> {
    if let Some(r) = a.refs.at(offset) {
        if let Some(md) = id_hover(a, r) {
            return Some(Hover { span: r.span, markdown: md });
        }
    }
    let cursor = a.cst.locate(&a.text, offset);
    let segs = cursor.segs;
    let keys: Vec<&str> = segs.iter().map(|s| if let Seg::Key(k) = s { k.as_str() } else { "" }).collect();
    match cursor.target {
        Target::Key { member, .. } => {
            let mut md = key_doc(a, &segs);
            if let Some(v) = &member.value {
                if let Some(extra) = value_note(a, &segs, &keys, v) {
                    md.push_str("\n\n");
                    md.push_str(&extra);
                }
            }
            (!md.is_empty()).then_some(Hover { span: member.key.span, markdown: md })
        }
        Target::Value { node: Some(v), .. } => {
            let note = value_note(a, &segs, &keys, v);
            let doc = key_doc(a, &segs);
            let md = match (note, doc.is_empty()) {
                (Some(n), true) => n,
                (Some(n), false) => format!("{n}\n\n---\n{doc}"),
                (None, false) => doc,
                (None, true) => return None,
            };
            Some(Hover { span: v.span, markdown: md })
        }
        _ => None,
    }
}

/// The schema's word on a key: `**cutoff** · number 0..1` + its description (+ the drift range for a
/// param with a Tier-2 safe default).
fn key_doc(a: &Analysis, segs: &[Seg]) -> String {
    let Some(Seg::Key(name)) = segs.last() else { return String::new() };
    let Some(s) = schema_at(a.root(), segs) else { return String::new() };
    let s = engines_core::schema::resolve(s);
    let summary = type_summary(s);
    let mut md = if summary.is_empty() { format!("**`{name}`**") } else { format!("**`{name}`** · {summary}") };
    if let Some(d) = description(s) {
        md.push_str("\n\n");
        md.push_str(d);
    }
    if let Some((lo, hi)) = spec_for(name).and_then(|p| p.safe_default) {
        md.push_str(&format!("\n\nDrift stays in {lo}..{hi} unless `safe_bounds` says otherwise."));
    }
    if engines_core::params::is_fixed(name) {
        md.push_str("\n\nNever drifted — only you, and routes into it, move it (ADR 0047).");
    }
    md
}

/// The extra reading of a value: native units, a note, a harmonic, a time.
fn value_note(a: &Analysis, segs: &[Seg], keys: &[&str], v: &Node) -> Option<String> {
    let last = *keys.last()?;
    // A note's pitch.
    if matches!(keys, ["modulators", _, "orrery_note_min" | "orrery_note_max"]) {
        let key = a.key.as_ref()?;
        let o = v.as_f64()?;
        let edge = if last == "orrery_note_min" { "lowest" } else { "highest" };
        return Some(format!(
            "**{}** · {:.2} Hz — the {edge} note the Orrery may write into this modulator (ADR 0042)",
            music::note_of(key, o, 0.0),
            music::hz_of(key, o, 0.0)
        ));
    }
    if last == "offset" || matches!(keys, ["modulators", _, "pitch_walk", "pool", _]) {
        let key = a.key.as_ref()?;
        let o = v.as_f64()?;
        let voice = if last == "offset" { a.node(&render(&segs[..segs.len() - 1])) } else { None };
        let cents = voice.and_then(|n| n.get("cents_offset")).and_then(Node::as_f64).unwrap_or(0.0);
        let mut md = format!("**{}** · {} of {} · {:.2} Hz", music::note_of(key, o, cents), music::degree_of(key, o), key.name(), music::hz_of(key, o, cents));
        if !music::in_scale(key, o) {
            md.push_str(" — outside the scale");
        }
        if let Some(chord) = step_chord(a, keys, segs) {
            md.push_str(&format!("\n\nStep chord: **{chord}**"));
        }
        return Some(md);
    }
    if last == "cents_offset" {
        let key = a.key.as_ref()?;
        let c = v.as_f64()?;
        let voice = a.node(&render(&segs[..segs.len() - 1]));
        let o = voice.and_then(|n| n.get("offset")).and_then(Node::as_f64).unwrap_or(0.0);
        let note = music::note_of(key, o, c);
        return Some(match music::harmonic_of(o, c, 3.0) {
            Some(h) if h.n > 1 => format!(
                "**{note}** · {:.2} Hz — the just **{}/{}** above {} (**harmonic {}**), {:+.1}¢ from tempered",
                music::hz_of(key, o, c),
                h.num,
                h.den,
                key.spell(key.scale.root),
                h.n,
                h.vs_tempered
            ),
            _ => format!("**{note}** · {:.2} Hz — no low harmonic of {} (within 3¢)", music::hz_of(key, o, c), key.spell(key.scale.root)),
        });
    }
    match keys {
        ["harmony", "root" | "mode"] => {
            let key = a.key.as_ref()?;
            let notes: Vec<String> = (0..7).map(|d| key.spell(key.scale.pitch_class(d))).collect();
            return Some(format!("**{}**: {} · {}", key.name(), notes.join(" "), key.scale.mode.degree_labels().join(" ")));
        }
        ["transport", "bpm" | "master_steps"] => {
            let secs = a.loop_seconds()?;
            let mut md = format!("Loop: **{secs:.2} s**");
            if let Some(p) = &a.poly {
                md.push_str(&format!(" · {}", crate::structure_summary(p)));
            }
            return Some(md);
        }
        ["modulators", id, "rate_hz" | "length" | "step_duration" | "sync"] => {
            let part = a.part(id)?;
            let mut md = format!("`{}`", crate::part_line(part).trim_end());
            if let Some(hz) = part.rate_hz.filter(|&hz| hz > 0.0) {
                md.push_str(&format!("\n\nPeriod {:.1} s = {:.2} loops", 1.0 / hz as f64, part.cycle_loops));
            }
            return Some(md);
        }
        // A binaural pair in real Hz (ADR 0047): the root as a note, the spread as a beat band —
        // and where each side lands.
        ["instruments", inst, "params", leaf @ ("root_hz" | "spread_hz")] => {
            let x = v.as_f64()?;
            let params = a.node(&format!("instruments.{inst}.params"));
            let num = |k: &str| params.and_then(|p| p.get(k)).and_then(Node::as_f64);
            let sides = |root: f64, spread: f64| {
                let (l, r) = engines_core::synth::binaural_pair(root as f32, spread as f32);
                format!("L {} | R {}", units::fmt_hz(l), units::fmt_hz(r))
            };
            return Some(if *leaf == "root_hz" {
                let note = a.key.as_ref().map(|k| format!("**{}** · ", music::note_of_hz(k, x))).unwrap_or_default();
                format!("{note}{} — {}", units::fmt_hz(x as f32), sides(x, num("spread_hz").unwrap_or(0.0)))
            } else {
                let band = units::beat_band(x as f32);
                let at = match num("root_hz") {
                    Some(root) => sides(root, x),
                    None => format!("±{} around the note", units::fmt_hz(x as f32 / 2.0)),
                };
                format!("**{} beat · {band}** ({}) — {at}", units::fmt_hz(x as f32), band_range(band))
            });
        }
        _ => {}
    }
    // A route's depth: the range it swings its dest through (ADR 0054 U6).
    if let Some((dest, key, source)) = readings::route_of(a, keys) {
        let r = readings::of_depth(a, &dest, &key, v.as_f64()?, source.as_deref())?;
        return Some(format!("`{key}` {} → {}", v.span.text(&a.text), r.hover));
    }
    // A param, written in its unit (schema v0.9): the value as written, and what the unit doesn't say.
    let is_param = matches!(keys, ["instruments", _, "params", _] | ["instruments", _, "sends_db", _] | ["global", _] | ["buses", _, "return_gain_db"] | ["buses", _, "effects", _, "params", _]);
    if is_param {
        let r = readings::of_param(a, &render(segs), v.as_f64()?)?;
        let label = units::unit_label(&render(segs));
        let unit = if label == "0..1" { String::new() } else { format!(" {label}") };
        return Some(format!("`{last}` {}{unit} — {}", v.span.text(&a.text), r.hover));
    }
    None
}

/// A brainwave band's span, for a `spread_hz` hover.
fn band_range(band: &str) -> &'static str {
    match band {
        "delta" => "< 4 Hz",
        "theta" => "4–8 Hz",
        "alpha" => "8–13 Hz",
        "beta" => "13–30 Hz",
        _ => "≥ 30 Hz",
    }
}

/// The chord a step's voicing spells (when the cursor is inside a step voice).
fn step_chord(a: &Analysis, keys: &[&str], segs: &[Seg]) -> Option<String> {
    if !matches!(keys, ["modulators", _, "steps", _, "voicing", _, "offset"]) {
        return None;
    }
    let step = a.node(&render(&segs[..4]))?;
    let offsets: Vec<f64> = step.get("voicing")?.members().iter().filter_map(|m| m.value.as_ref()).filter(|v| v.get("release").is_none()).map(|v| v.get("offset").and_then(Node::as_f64).unwrap_or(0.0)).collect();
    music::chord_of(a.key.as_ref()?, &offsets)
}

/// What an id names, at its definition or any use.
fn id_hover(a: &Analysis, r: &IdRef) -> Option<String> {
    let uses = a.refs.sites(r).filter(|x| !x.def).count();
    let use_note = match uses {
        0 => " · not referenced".to_string(),
        1 => " · 1 reference".to_string(),
        n => format!(" · {n} references"),
    };
    let head = format!("**{} `{}`**", r.kind.noun(), r.id);
    let body = match r.kind {
        IdKind::Modulator => {
            let m = a.node(&format!("modulators.{}", r.id))?;
            let kind = m.get("type").and_then(Node::as_str).unwrap_or("?");
            let output = m.get("output").and_then(Node::as_str).unwrap_or("control");
            let mut s = format!("{kind} · {output}");
            if let Some(p) = a.part(&r.id) {
                s.push_str(&format!("\n\n`{}`", crate::part_line(p).trim_end()));
            }
            s
        }
        IdKind::Instrument => {
            let i = a.node(&format!("instruments.{}", r.id))?;
            let kind = i.get("type").and_then(Node::as_str).unwrap_or("subtractive");
            let poly = i.get("note_engine").and_then(|n| n.get("polyphony")).and_then(Node::as_f64).unwrap_or(0.0);
            let prefix = format!("instruments.{}.", r.id);
            let routes: Vec<String> = a
                .root()
                .and_then(|root| root.get("matrix"))
                .map(Node::members)
                .unwrap_or_default()
                .iter()
                .filter_map(|m| {
                    let dest = m.value.as_ref()?.get("dest")?.as_str()?;
                    dest.starts_with(&prefix).then(|| format!("`{}` → {}", m.key.name, dest.rsplit('.').next().unwrap_or("")))
                })
                .collect();
            let mut s = format!("{kind} · {poly} voice{}", if poly == 1.0 { "" } else { "s" });
            if !routes.is_empty() {
                s.push_str(&format!("\n\nRoutes: {}", routes.join(", ")));
            }
            s
        }
        IdKind::Bus => {
            let b = a.node(&format!("buses.{}", r.id))?;
            let ret = b.get("return_gain_db").and_then(Node::as_f64).map(|g| format!("return {}", units::fmt_authored("buses._.return_gain_db", g as f32))).unwrap_or_else(|| "no return_gain_db (silent)".into());
            let fx: Vec<&str> = b.get("effects").map(Node::members).unwrap_or_default().iter().map(|m| m.key.name.as_str()).collect();
            format!("{ret} · effects: {}", if fx.is_empty() { "none".to_string() } else { fx.join(", ") })
        }
        IdKind::Pool => {
            let p = a.node(&format!("sequence_pools.{}", r.id))?;
            let len = p.get("length").and_then(Node::as_f64).unwrap_or(0.0);
            let notes = p.get("notes").map(Node::items).map(<[_]>::len).unwrap_or(0);
            let fit = p.get("fit").and_then(Node::as_str).unwrap_or("chord");
            format!("{notes} notes over {len} steps · fit `{fit}`")
        }
        IdKind::Route => {
            let e = a.node(&format!("matrix.{}", r.id))?;
            let src = e.get("source").and_then(Node::as_str).unwrap_or("?");
            let dest = e.get("dest").and_then(Node::as_str).unwrap_or("?");
            let depth = engines_core::params::DepthUnit::ALL.iter().find_map(|u| e.get(u.key()).and_then(Node::as_f64).map(|d| format!(" · {} {d}", u.key()))).unwrap_or_default();
            format!("`{src}` → `{dest}`{depth}")
        }
        IdKind::Effect => {
            let bus = r.scope.as_deref()?;
            let e = a.node(&format!("buses.{bus}.effects.{}", r.id))?;
            format!("{} on bus `{bus}`", e.get("type").and_then(Node::as_str).unwrap_or("?"))
        }
        IdKind::Layer => "song matrix layer".to_string(),
    };
    Some(format!("{head}{use_note}\n\n{body}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"{
  schema_version: "0.9",
  transport: { bpm: 60, master_steps: 8 },
  harmony: { root: "D", mode: "dorian" },
  instruments: {
    pad: { note_engine: { polyphony: 3 }, params: { wave: "saw_soft", level_db: -7.96, cutoff_hz: 318.49, attack_s: 1.5 } },
  },
  modulators: {
    lfo: { type: "lfo", output: "control", rate_hz: 0.05 },
    seq: { type: "step_seq", output: "note", sync: "free", length: 5, step_duration: "1/8",
           steps: [ { step: 0, voicing: { v1: { offset: 0 }, v2: { offset: 3 }, v3: { offset: 7 }, v4: { offset: 10, cents_offset: -31 } } } ] },
  },
  matrix: {
    m_notes: { source: "seq", dest: "instruments.pad.notes", kind: "note", voices: 4 },
    m_cut: { source: "lfo", dest: "instruments.pad.params.cutoff_hz", kind: "mod", depth_oct: 0.664 },
  },
}"#;

    fn hover_at(needle: &str, delta: usize) -> String {
        let a = Analysis::new(DOC);
        hover(&a, DOC.find(needle).unwrap_or_else(|| panic!("{needle}")) + delta).map(|h| h.markdown).unwrap_or_default()
    }

    #[test]
    fn values_read_in_the_key_and_beyond_their_unit() {
        // Schema v0.9 writes a param in its unit: the hover says what the unit doesn't (ADR 0054 U6).
        let cut = hover_at("318.49", 1);
        assert!(cut.starts_with("`cutoff_hz` 318.49 Hz — ≈ **Eb4 +40¢**, the nearest note"), "{cut}");
        assert!(cut.contains("**`cutoff_hz`** · number 80..8000"), "the key's doc follows: {cut}");
        assert!(hover_at("-7.96", 1).starts_with("`level_db` -7.96 dB — ≈ **×0.40** amplitude"), "{}", hover_at("-7.96", 1));
        let depth = hover_at("0.664", 1);
        assert!(depth.starts_with("`depth_oct` 0.664 → **±0.66 oct** on `cutoff_hz`: 201Hz to 505Hz around 318Hz at full swing"), "{depth}");
        assert!(!hover_at("1.5 } }", 1).contains("—"), "a time is just its seconds");
        let off = hover_at("offset: 7", 8);
        assert!(off.starts_with("**A4** · 5 of D dorian · 440.00 Hz"), "{off}");
        assert!(off.contains("Step chord: **Dm7 (i7)**"), "{off}");
        let cents = hover_at("-31", 1);
        assert!(cents.contains("just **7/4** above D (**harmonic 7**), -31.2¢ from tempered"), "{cents}");
        assert!(hover_at("\"dorian\"", 2).starts_with("**D dorian**: D E F G A B C · 1 2 ♭3 4 5 6 ♭7"));
        assert!(hover_at("master_steps: 8", 15).starts_with("Loop: **8.00 s**"));
        let rate = hover_at("rate_hz: 0.05", 2);
        assert!(rate.contains("Period 20.0 s = 2.50 loops"), "{rate}");
    }

    #[test]
    fn keys_show_the_schema_and_ids_show_their_uses() {
        let cut = hover_at("cutoff_hz: 318.49", 2);
        assert!(cut.starts_with("**`cutoff_hz`** · number 80..8000"), "{cut}");
        assert!(cut.contains("low-pass corner in Hz") && cut.contains("Drift stays in 101..8000"), "{cut}");
        let lfo = hover_at("source: \"lfo\"", 10);
        assert!(lfo.starts_with("**modulator `lfo`** · 1 reference"), "{lfo}");
        let pad = hover_at("pad: {", 1);
        assert!(pad.contains("subtractive · 3 voices") && pad.contains("`m_cut` → cutoff_hz"), "{pad}");
    }

    #[test]
    fn binaural_hz_read_as_a_note_a_beat_band_and_two_sides() {
        const BIN: &str = r#"{
  schema_version: "0.9",
  transport: { bpm: 60, master_steps: 8 },
  harmony: { root: "A", mode: "aeolian" },
  instruments: {
    bin: { type: "binaural", note_engine: { polyphony: 1 }, params: { wave: "sine", root_hz: 109.5, spread_hz: 2, level_db: -10.46 } },
    near: { type: "binaural", note_engine: { polyphony: 1 }, params: { wave: "sine", spread_hz: 6.5, level_db: -10.46 } },
  },
  modulators: {
    lfo: { type: "lfo", output: "control", rate_hz: 0.05 },
    seq: { type: "step_seq", output: "note", length: 1, steps: [ { step: 0, voicing: { v: { offset: -24 } } } ] },
  },
  matrix: {
    m_bin: { source: "seq", dest: "instruments.bin.notes", kind: "note", voices: 1 },
    m_near: { source: "seq", dest: "instruments.near.notes", kind: "note", voices: 1 },
    m_beat: { source: "lfo", dest: "instruments.near.params.spread_hz", kind: "mod", depth_hz: 2 },
  },
}"#;
        let a = Analysis::new(BIN);
        let at = |needle: &str, delta: usize| hover(&a, BIN.find(needle).unwrap() + delta).map(|h| h.markdown).unwrap_or_default();
        let root = at("109.5", 1);
        assert!(root.starts_with("**A2 -8¢** · 109.5Hz — L 108.5Hz | R 110.5Hz"), "{root}");
        let spread = at("spread_hz: 2,", 12);
        assert!(spread.starts_with("**2Hz beat · delta** (< 4 Hz) — L 108.5Hz | R 110.5Hz"), "{spread}");
        let near = at("6.5", 1);
        assert!(near.starts_with("**6.5Hz beat · theta** (4–8 Hz) — ±3.25Hz around the note"), "{near}");
        let key = at("root_hz", 2);
        assert!(key.contains("REAL Hz") && key.contains("Never drifted"), "{key}");
        // The route names its depth in the dest's unit (schema v0.9).
        let route = at("m_beat", 1);
        assert!(route.contains("`lfo` → `instruments.near.params.spread_hz` · depth_hz 2"), "{route}");
    }
}
