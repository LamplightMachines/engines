//! Inlay hints: the numbers read back in musical terms, right where they're written — note names
//! after offsets, the harmonic a `cents_offset` tunes to, a binaural pair's note and beat band, what a
//! value in its unit doesn't say ([`readings`]: the note nearest a cutoff, a gain's amplitude, a
//! depth's swing), the loop length, each modulator's cycle, and when the whole doc realigns.

use super::{music, note_sites, readings, Analysis};
use crate::cst::{Node, Span};
use engines_core::analysis::{Clock, PartKind};
use engines_core::params::units;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InlayKind {
    Note,
    Unit,
    Time,
    Cycle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inlay {
    /// Shown right after this byte offset.
    pub offset: usize,
    pub label: String,
    pub kind: InlayKind,
    pub tooltip: Option<String>,
}

/// Which hints to show (the editor's settings; all on by default).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(default)]
pub struct InlayConfig {
    pub notes: bool,
    pub units: bool,
    #[serde(alias = "loopLength")]
    pub loop_length: bool,
    pub cycles: bool,
}

impl Default for InlayConfig {
    fn default() -> Self {
        InlayConfig { notes: true, units: true, loop_length: true, cycles: true }
    }
}

pub fn inlays(a: &Analysis, range: Span, cfg: &InlayConfig) -> Vec<Inlay> {
    let mut out = Vec::new();
    let Some(root) = a.root() else { return out };
    let mut push = |offset: usize, label: String, kind, tooltip: Option<String>| {
        if range.touches(offset) {
            out.push(Inlay { offset, label, kind, tooltip });
        }
    };
    if cfg.notes {
        if let Some(key) = &a.key {
            for s in note_sites(root) {
                if let Some(o) = s.offset {
                    push(o.span.end, music::note_of(key, s.semitones(), 0.0), InlayKind::Note, Some(format!("{} of {}", music::degree_of(key, s.semitones()), key.name())));
                }
                if let Some(c) = s.voice.get("cents_offset").filter(|_| s.cents != 0.0) {
                    if let Some(h) = music::harmonic_of(s.semitones(), s.cents, 3.0).filter(|h| h.n > 1) {
                        push(c.span.end, format!("h{} {}/{}", h.n, h.num, h.den), InlayKind::Note, Some(format!("harmonic {} of {}", h.n, key.spell(key.scale.root))));
                    }
                }
            }
            // The Orrery's note range on a modulator (ADR 0042): its bounds as notes.
            for m in root.get("modulators").map(Node::members).unwrap_or_default() {
                for bound in ["orrery_note_min", "orrery_note_max"] {
                    if let Some(v) = m.value.as_ref().and_then(|mv| mv.get(bound)) {
                        if let Some(o) = v.as_f64() {
                            push(v.span.end, music::note_of(key, o, 0.0), InlayKind::Note, Some(format!("the Orrery's {} for this modulator", if bound.ends_with("min") { "lowest note" } else { "highest note" })));
                        }
                    }
                }
            }
        }
    }
    if cfg.units {
        for (path, node) in param_values(root) {
            // A binaural pair's Hz (ADR 0047) reads back as a note (root) and a beat band (spread);
            // the rest as what their unit doesn't say.
            match units::leaf(&path) {
                "root_hz" => {
                    if let (Some(key), Some(x)) = (&a.key, node.as_f64()) {
                        push(node.span.end, music::note_of_hz(key, x), InlayKind::Note, Some("the binaural pair's root".into()));
                    }
                }
                "spread_hz" => {
                    if let Some(x) = node.as_f64() {
                        push(node.span.end, units::beat_band(x as f32).to_string(), InlayKind::Unit, Some("the beat's brainwave band".into()));
                    }
                }
                _ => {
                    if let Some(r) = node.as_f64().and_then(|x| readings::of_param(a, &path, x)) {
                        push(node.span.end, r.inlay, InlayKind::Unit, Some(plain(&r.hover)));
                    }
                }
            }
        }
        for (keys, node) in depth_values(root) {
            let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
            let Some((dest, key, source)) = readings::route_of(a, &keys) else { continue };
            if let Some(r) = node.as_f64().and_then(|d| readings::of_depth(a, &dest, &key, d, source.as_deref())) {
                push(node.span.end, r.inlay, InlayKind::Unit, Some(plain(&r.hover)));
            }
        }
    }
    if cfg.loop_length {
        if let (Some(ms), Some(secs)) = (root.get("transport").and_then(|t| t.get("master_steps")), a.loop_seconds()) {
            push(ms.span.end, format!("{secs:.1} s loop"), InlayKind::Time, None);
        }
    }
    if cfg.cycles {
        if let (Some(poly), Some(mods)) = (&a.poly, root.get("modulators")) {
            for m in mods.members() {
                let (Some(v), Some(part)) = (&m.value, a.part(&m.key.name)) else { continue };
                if let Some(label) = cycle_label(part, poly.loop_seconds) {
                    push(v.span.end, label, InlayKind::Cycle, Some(crate::part_line(part).trim_end().to_string()));
                }
            }
            let realign = crate::structure_summary(poly);
            let summary = realign.split(" · ").nth(1).unwrap_or(&realign).to_string();
            push(mods.span.end, summary, InlayKind::Cycle, Some(realign));
        }
    }
    out.sort_by_key(|i| i.offset);
    out
}

/// `⟳ 20.0 s · 2.5 loops`, `⟳ 5/8 loop · in phase every 5`, `⟳ every loop`; `None` for an unrouted
/// part (the checker already says so).
fn cycle_label(p: &engines_core::analysis::PartCycle, loop_s: f64) -> Option<String> {
    if !p.routed {
        return None;
    }
    let cycle = match (p.kind, p.exact) {
        (PartKind::Lfo, _) => format!("{:.1} s · {:.2} loops", p.cycle_loops * loop_s, p.cycle_loops),
        (_, Some((n, 1))) => format!("{n} loop{}", if n == 1 { "" } else { "s" }),
        (_, Some((n, d))) => format!("{n}/{d} loop"),
        (_, None) => format!("{:.2} loops", p.cycle_loops),
    };
    let phase = match (p.clock, p.realign_loops) {
        (Clock::Loop, _) => " · restarts each loop".to_string(),
        (Clock::Free, _) if !p.structural => String::new(),
        (Clock::Free, Some(1)) => String::new(),
        (Clock::Free, Some(n)) => format!(" · in phase every {}{n}", if p.kind == PartKind::Lfo { "≈ " } else { "" }),
        (Clock::Free, None) => " · never in phase".to_string(),
    };
    Some(format!("⟳ {cycle}{phase}"))
}

/// A hover sentence as a tooltip: no markdown emphasis.
fn plain(md: &str) -> String {
    md.replace("**", "").replace('`', "")
}

/// Every route depth in `matrix` and the song layers, with its key path (`["matrix", id, "depth_oct"]`).
fn depth_values(root: &Node) -> Vec<(Vec<String>, &Node)> {
    fn routes<'a>(prefix: &[&str], obj: Option<&'a Node>, out: &mut Vec<(Vec<String>, &'a Node)>) {
        for r in obj.map(Node::members).unwrap_or_default() {
            for m in r.value.as_ref().map(Node::members).unwrap_or_default() {
                if let Some(v) = m.value.as_ref().filter(|_| engines_core::params::DepthUnit::from_key(&m.key.name).is_some()) {
                    let mut keys: Vec<String> = prefix.iter().map(|s| s.to_string()).collect();
                    keys.extend([r.key.name.clone(), m.key.name.clone()]);
                    out.push((keys, v));
                }
            }
        }
    }
    let mut out = Vec::new();
    routes(&["matrix"], root.get("matrix"), &mut out);
    for layer in root.get("song").and_then(|s| s.get("matrices")).map(Node::members).unwrap_or_default() {
        routes(&["song", "matrices", &layer.key.name], layer.value.as_ref(), &mut out);
    }
    out
}

/// Every numeric param value in the tree with its checker path.
fn param_values(root: &Node) -> Vec<(String, &Node)> {
    fn nums<'a>(prefix: String, obj: Option<&'a Node>, out: &mut Vec<(String, &'a Node)>) {
        for m in obj.map(Node::members).unwrap_or_default() {
            if let Some(v) = m.value.as_ref().filter(|v| v.as_f64().is_some()) {
                out.push((format!("{prefix}.{}", m.key.name), v));
            }
        }
    }
    let mut out = Vec::new();
    nums("global".into(), root.get("global"), &mut out);
    for i in root.get("instruments").map(Node::members).unwrap_or_default() {
        nums(format!("instruments.{}.params", i.key.name), i.value.as_ref().and_then(|v| v.get("params")), &mut out);
        nums(format!("instruments.{}.sends_db", i.key.name), i.value.as_ref().and_then(|v| v.get("sends_db")), &mut out);
    }
    for b in root.get("buses").map(Node::members).unwrap_or_default() {
        if let Some(g) = b.value.as_ref().and_then(|v| v.get("return_gain_db")).filter(|g| g.as_f64().is_some()) {
            out.push((format!("buses.{}.return_gain_db", b.key.name), g));
        }
        for fx in b.value.as_ref().and_then(|v| v.get("effects")).map(Node::members).unwrap_or_default() {
            nums(format!("buses.{}.effects.{}.params", b.key.name, fx.key.name), fx.value.as_ref().and_then(|v| v.get("params")), &mut out);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_read_the_doc_back_musically() {
        let src = r#"{
  schema_version: "0.9",
  transport: { bpm: 60, master_steps: 4 },
  harmony: { root: "A", mode: "mixolydian" },
  instruments: { pad: { note_engine: { polyphony: 2 }, params: { wave: "sine", level_db: -7.96, cutoff_hz: 318.49, attack_s: 1.5 } } },
  buses: { rv: { return_gain_db: -6.02, effects: { verb: { type: "reverb", params: { room: 0.8, predelay_ms: 20 } } } } },
  modulators: {
    lfo: { type: "lfo", output: "control", rate_hz: 0.05 },
    seq: { type: "step_seq", output: "note", sync: "free", length: 5, step_duration: "1/8",
           steps: [ { step: 0, voicing: { v1: { offset: -24 }, v2: { offset: 10, cents_offset: -31 } } } ] },
  },
  matrix: {
    m_notes: { source: "seq", dest: "instruments.pad.notes", kind: "note", voices: 2 },
    m_cut: { source: "lfo", dest: "instruments.pad.params.cutoff_hz", kind: "mod", depth_oct: 0.664 },
  },
}"#;
        let a = Analysis::new(src);
        assert!(a.checked.as_ref().is_some_and(|c| !c.has_errors()), "{:?}", a.checked.as_ref().map(|c| &c.diags));
        let hints = inlays(&a, Span::new(0, src.len()), &InlayConfig::default());
        let after = |needle: &str| hints.iter().find(|h| h.offset == src.find(needle).unwrap() + needle.len()).map(|h| h.label.clone());
        assert_eq!(after("master_steps: 4").as_deref(), Some("4.0 s loop"));
        // What the unit doesn't say (ADR 0054 U6): the note nearest a cutoff, a gain's amplitude, a
        // depth's swing; a time is just its seconds.
        assert_eq!(after("cutoff_hz: 318.49").as_deref(), Some("≈D#4 +40¢"));
        assert_eq!(after("level_db: -7.96").as_deref(), Some("×0.40"));
        assert_eq!(after("return_gain_db: -6.02").as_deref(), Some("×0.50"));
        assert_eq!(after("depth_oct: 0.664").as_deref(), Some("201Hz…505Hz"));
        assert_eq!(after("attack_s: 1.5"), None);
        assert_eq!(after("predelay_ms: 20"), None);
        assert_eq!(after("offset: -24").as_deref(), Some("A2"));
        assert_eq!(after("offset: 10").as_deref(), Some("G5"));
        assert_eq!(after("cents_offset: -31").as_deref(), Some("h7 7/4"));
        assert_eq!(after("rate_hz: 0.05 }").as_deref(), Some("⟳ 20.0 s · 5.00 loops · in phase every ≈ 5"));
        let seq_end = src.find("] },\n  },").unwrap() + 3;
        assert!(hints.iter().any(|h| h.offset == seq_end && h.label == "⟳ 5/8 loop · in phase every 5"), "{hints:#?}");
        assert!(hints.iter().any(|h| h.label.starts_with("realigns every ≈ 5 loops")), "{hints:#?}");
        assert!(after("room: 0.8").is_none(), "normalized params get no unit");
        // A range limits the hints; the config turns kinds off.
        let few = inlays(&a, Span::new(0, src.find("instruments").unwrap()), &InlayConfig::default());
        assert_eq!(few.len(), 1);
        let none = inlays(&a, Span::new(0, src.len()), &InlayConfig { notes: false, units: false, loop_length: false, cycles: false });
        assert!(none.is_empty());
    }

    #[test]
    fn a_binaural_pair_reads_back_as_a_note_and_a_beat_band() {
        // ADR 0047: root_hz / spread_hz are written in Hz already — the hint names the note and band.
        let src = r#"{
  schema_version: "0.9",
  transport: { bpm: 60, master_steps: 4 },
  harmony: { root: "A", mode: "aeolian" },
  instruments: { bin: { type: "binaural", note_engine: { polyphony: 1 }, params: { wave: "sine", root_hz: 109.5, spread_hz: 6, level_db: -10.46 } } },
  modulators: { seq: { type: "step_seq", output: "note", length: 1, steps: [ { step: 0, voicing: { v: { offset: -24 } } } ] } },
  matrix: { m: { source: "seq", dest: "instruments.bin.notes", kind: "note", voices: 1 } },
}"#;
        let a = Analysis::new(src);
        let hints = inlays(&a, Span::new(0, src.len()), &InlayConfig::default());
        let after = |needle: &str| hints.iter().find(|h| h.offset == src.find(needle).unwrap() + needle.len()).map(|h| h.label.clone());
        assert_eq!(after("root_hz: 109.5").as_deref(), Some("A2 -8¢"));
        assert_eq!(after("spread_hz: 6").as_deref(), Some("theta"));
    }

    #[test]
    fn the_orrery_note_range_reads_as_notes() {
        let src = r#"{
  schema_version: "0.9",
  transport: { bpm: 60, master_steps: 4 },
  harmony: { root: "F", mode: "ionian" },
  instruments: { pad: { note_engine: { polyphony: 2 }, params: { wave: "sine", level_db: -7.96 } } },
  modulators: {
    arp: { type: "step_seq", output: "note", length: 4, orrery_role: "arp", orrery_note_min: -12, orrery_note_max: 13,
           steps: [ { step: 0, voicing: { v1: { offset: 0 } } } ] },
  },
  matrix: { m_notes: { source: "arp", dest: "instruments.pad.notes", kind: "note", voices: 1 } },
}"#;
        let a = Analysis::new(src);
        assert!(a.checked.as_ref().is_some_and(|c| !c.has_errors()), "{:?}", a.checked.as_ref().map(|c| &c.diags));
        let hints = inlays(&a, Span::new(0, src.len()), &InlayConfig::default());
        let after = |needle: &str| hints.iter().find(|h| h.offset == src.find(needle).unwrap() + needle.len()).map(|h| h.label.clone());
        assert_eq!(after("orrery_note_min: -12").as_deref(), Some("F3"));
        assert_eq!(after("orrery_note_max: 13").as_deref(), Some("Gb5"));
        // A bound outside the scale is not an out-of-scale note (it is a limit, not a pitch played).
        assert!(a.checked.as_ref().unwrap().diags.iter().all(|d| !d.path.contains("orrery_note")), "{:?}", a.checked.as_ref().map(|c| &c.diags));
    }
}
