//! # engines_authoring — the desktop authoring toolchain for loop docs
//!
//! Wraps `engines_core`'s strict checker ([`engines_core::check`]) and structure analysis
//! ([`engines_core::analysis`]) into reports for people (`engines-check`) and tools (`engines-check --json`,
//! the PostToolUse hook), and hosts the editor side (ADR 0037): the lossless JSON5 [`cst`] and the
//! language server ([`lsp`], bin `engines-lsp`). Diagnostics are located by JSONPath and, through the
//! CST, by line:column. Desktop-only — the portable core never depends on this crate.

pub mod clean;
pub mod cst;
pub mod lsp;
pub mod migrate;
pub mod parts;

use serde::Serialize;
use engines_core::analysis::{polymeter, Clock, PartKind, Polymeter};
use engines_core::catalog::{Catalog, Dflt, ParamInfo};
use std::collections::BTreeMap;
use engines_core::check::{check_str, DocDiag, Severity};

/// One checked doc: its diagnostics plus, when it parsed, its polymeter.
#[derive(Debug, Serialize)]
pub struct Report {
    /// The file as given on the command line.
    pub path: String,
    /// No errors (warnings allowed) — the engine will load it.
    pub ok: bool,
    pub errors: usize,
    pub warnings: usize,
    pub diagnostics: Vec<Located>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structure: Option<Polymeter>,
}

/// A diagnostic and where it sits in the source (1-based line and column, columns in characters) —
/// the token its JSONPath names, found through the CST (ADR 0037).
#[derive(Debug, Serialize)]
pub struct Located {
    #[serde(flatten)]
    pub diag: DocDiag,
    pub line: usize,
    pub column: usize,
}

/// Check one doc's source text.
pub fn check(path: &str, src: &str) -> Report {
    let checked = check_str(src);
    let structure = checked.doc.as_ref().map(polymeter);
    let errors = checked.errors().count();
    let warnings = checked.warnings().count();
    let tree = cst::Cst::parse(src);
    let lines = cst::lines::LineIndex::new(src);
    let diagnostics = checked
        .diags
        .into_iter()
        .map(|d| {
            // A syntax error carries its own position in the message; the CST knows where it is.
            let at = match d.code.as_str() {
                "syntax" => tree.errors.first().map(|e| e.span.start).unwrap_or(0),
                _ => lsp::diagnostics::locate_in(&tree, &d).start,
            };
            let line = lines.line_of(at);
            let column = src[lines.line_start(line)..at].chars().count() + 1;
            Located { diag: d, line: line + 1, column }
        })
        .collect();
    Report { path: path.to_string(), ok: errors == 0, errors, warnings, diagnostics, structure }
}

/// The human-readable report: a verdict line, one line per diagnostic, and a structure summary
/// (with `parts`, a line per modulator).
pub fn render_human(r: &Report, parts: bool) -> String {
    let mut out = String::new();
    let verdict = if r.ok { "ok" } else { "REFUSED" };
    out.push_str(&format!("{}: {verdict} — {} error{}, {} warning{}\n", r.path, r.errors, plural(r.errors), r.warnings, plural(r.warnings)));
    for l in &r.diagnostics {
        let d = &l.diag;
        let sev = match d.severity {
            Severity::Error => "error  ",
            Severity::Warning => "warning",
        };
        out.push_str(&format!("  {sev} {}:{} {d}  [{}]\n", l.line, l.column, d.code));
    }
    if let Some(p) = &r.structure {
        out.push_str(&format!("  structure: {}\n", structure_summary(p)));
        if parts {
            for part in &p.parts {
                out.push_str(&format!("    {}\n", part_line(part)));
            }
        }
    }
    out
}

/// `14.5 s loop · realigns every ≈ 275 loops (≈ 1:06:40)`.
pub fn structure_summary(p: &Polymeter) -> String {
    let approx = if p.approximate { "≈ " } else { "" };
    let realign = match (p.realign_loops, p.realign_seconds()) {
        (Some(1), _) => "repeats every loop".to_string(),
        (Some(n), Some(s)) => format!("realigns every {approx}{n} loops ({approx}{})", clock(s)),
        _ => format!("never realigns within {} loops", engines_core::analysis::MAX_REALIGN_LOOPS),
    };
    format!("{:.1} s loop · {realign}", p.loop_seconds)
}

/// One modulator's line: `seq  notes  free cycle 5/8 loop — back in phase every 5 loops`.
pub fn part_line(p: &engines_core::analysis::PartCycle) -> String {
    let kind = match p.kind {
        PartKind::Lfo => "lfo",
        PartKind::ControlSteps => "steps",
        PartKind::NoteSteps => "notes",
        PartKind::Euclid => "euclid",
        PartKind::Phrase => "phrase",
    };
    let clock_s = match p.clock {
        Clock::Loop => "loop",
        Clock::Free => "free",
    };
    let cycle = match (p.exact, p.rate_hz) {
        (Some((a, 1)), _) => format!("cycle {a} loop{}", plural(a as usize)),
        (Some((a, b)), _) => format!("cycle {a}/{b} loop"),
        (None, Some(hz)) if p.structural => format!("{hz} Hz · cycle {:.2} loops", p.cycle_loops),
        (None, Some(hz)) => format!("{hz} Hz"),
        (None, None) => format!("cycle {:.2} loops", p.cycle_loops),
    };
    let mut notes = Vec::new();
    match (p.clock, p.realign_loops) {
        (Clock::Free, _) if !p.structural => {}
        (Clock::Free, Some(n)) => {
            let approx = if p.kind == PartKind::Lfo { "≈ " } else { "" };
            notes.push(format!("back in phase every {approx}{n} loop{}", plural(n as usize)));
        }
        (Clock::Free, None) => notes.push("never back in phase".into()),
        _ => {}
    }
    if p.clock == Clock::Loop && !p.fits_loop && p.exact.is_some() {
        notes.push("cut short / wraps at the loop top".into());
    }
    if !p.routed {
        notes.push("unrouted".into());
    } else if !p.structural {
        notes.push("texture rate — not counted".into());
    }
    if p.stochastic {
        notes.push("varies (seeded)".into());
    }
    let notes = if notes.is_empty() { String::new() } else { format!(" — {}", notes.join(", ")) };
    format!("{:<18} {kind:<6} {clock_s:<4} {cycle}{notes}", p.id)
}

/// The capability catalog for people (`engines-check --catalog`, ADR 0050): the synth types and the params
/// each reads, every param's kind, range, default and unit, the unit maps' formulas, the harmony modes
/// and the modulators' choices. `--json` prints the catalog itself, sample points and all.
pub fn render_catalog(c: &Catalog) -> String {
    let mut out = format!("capability catalog v{} — loop schema {}, engine {} (hash {})\n", c.catalog_version, c.schema_version, c.engine_build, c.hash);
    out.push_str("\nsynth types — the params each reads\n");
    for (name, t) in &c.synth_types {
        let tag = if t.default { " (default)" } else { "" };
        let what = if t.free_form { "no audio: notes + CCs to an external synth; any param name holds a CC value".to_string() } else { t.params.join(" ") };
        out.push_str(&format!("  {:<22} {what}\n", format!("{name}{tag}")));
        if let Some(note) = t.note {
            out.push_str(&format!("    {:<20} {note}\n", "note"));
        }
        for (variant, params) in &t.variants {
            out.push_str(&format!("    {variant:<20} {}\n", params.join(" ")));
        }
        for (material, partials) in t.materials.iter().flatten() {
            let ratios: Vec<String> = partials.iter().map(|p| num(p.ratio)).collect();
            out.push_str(&format!("    {material:<20} partials × {}\n", ratios.join(" ")));
        }
    }
    out.push_str("\ninstrument params — instruments.<id>.params.<leaf>\n");
    params_table(&mut out, &c.params);
    for (kind, fx) in &c.effects {
        out.push_str(&format!("\n{kind} — buses.<bus>.effects.<fx>.params.<leaf>\n"));
        params_table(&mut out, &fx.params);
    }
    out.push_str("\nglobal — global.<leaf>\n");
    params_table(&mut out, &c.global);
    out.push_str("\nbuses — buses.<id>.<leaf>\n");
    params_table(&mut out, &c.buses);
    out.push_str("\nunit maps — x → y (sample points in --json)\n");
    for (id, m) in &c.unit_maps {
        let (lo, hi) = (num(m.lo), num(m.hi));
        let formula = match m.kind {
            "exp" => format!("{lo} · ({hi}/{lo})^x"),
            "log" => format!("ln(x/{lo}) / ln({hi}/{lo}), 0..1"),
            "linear" => format!("{hi} · x"),
            _ => format!("{lo} + {} · x", num(m.hi - m.lo)),
        };
        let note = m.note.map(|n| format!(" — {n}")).unwrap_or_default();
        out.push_str(&format!("  {id:<20} {:<6} {:<22} {formula}  [{}]{note}\n", m.kind, format!("{} → {}", m.from, m.to), m.params.join(", ")));
    }
    out.push_str("\nharmony\n");
    let roots: Vec<&str> = c.harmony.roots.iter().map(|r| r.name.as_str()).collect();
    out.push_str(&format!("  roots  {}\n", roots.join(" ")));
    for m in &c.harmony.modes {
        let aliases = if m.aliases.is_empty() { String::new() } else { format!(" ({})", m.aliases.join(", ")) };
        let degrees: Vec<String> = m.degrees.iter().map(i32::to_string).collect();
        out.push_str(&format!("  {:<28} {}\n", format!("{}{aliases}", m.name), degrees.join(" ")));
    }
    out.push_str("\nmodulators (* = the default)\n");
    let mods = &c.modulators;
    for (name, ch) in [
        ("type", &mods.kind),
        ("output", &mods.output),
        ("sync", &mods.sync),
        ("lfo wave", &mods.lfo_wave),
        ("lfo polarity", &mods.lfo_polarity),
        ("interp", &mods.interp),
        ("generator", &mods.generator),
        ("pitch_walk order", &mods.pitch_walk_order),
        ("orrery_role", &mods.orrery_role),
        ("route kind", &mods.route_kind),
    ] {
        out.push_str(&format!("  {name:<17} {}\n", choices(&ch.options, ch.default)));
    }
    out
}

/// One line per param: kind, range (and safe band), default, unit and maps, what moves it.
fn params_table(out: &mut String, params: &BTreeMap<&'static str, ParamInfo>) {
    for (leaf, p) in params {
        let range = match (&p.range, &p.options) {
            (Some([lo, hi]), _) => {
                let safe = p.safe_default.map(|[a, b]| format!(" (safe {}..{})", num(a), num(b))).unwrap_or_default();
                format!("{}..{}{safe}", num(*lo), num(*hi))
            }
            (None, Some(opts)) => opts.join("|"),
            _ => String::new(),
        };
        let default = match p.default {
            Some(Dflt::Num(v)) => num(v),
            Some(Dflt::Int(v)) => v.to_string(),
            Some(Dflt::Str(s)) => s.to_string(),
            None => "—".into(),
        };
        let unit = match (p.unit, p.unit_maps.is_empty()) {
            (Some(u), true) => u.to_string(),
            (Some(u), false) => format!("{u} → {}", p.unit_maps.join(", ")),
            (None, _) => String::new(),
        };
        let mut moves = Vec::new();
        if p.modulatable {
            moves.push(match p.depth_key {
                Some(k) => format!("routable ({k})"),
                None => "routable".into(),
            });
        }
        if p.glideable {
            moves.push("glides".into());
        }
        out.push_str(format!("  {leaf:<14} {:<11} {range:<40} {default:<8} {unit:<36} {}", p.kind, moves.join(", ")).trim_end());
        out.push('\n');
    }
}

/// `loop*|free` — the options, the default starred.
fn choices(options: &[&str], default: Option<&str>) -> String {
    options.iter().map(|o| if Some(*o) == default { format!("{o}*") } else { o.to_string() }).collect::<Vec<_>>().join("|")
}

/// A number as the shortest text that reads back the same (`0.7`, `8000`, `-24`).
fn num(v: f32) -> String {
    format!("{v}")
}

/// `h:mm:ss` (or `m:ss` under an hour).
pub fn clock(seconds: f64) -> String {
    let s = seconds.round() as u64;
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"{
      schema_version: "0.9",
      transport: { bpm: 60, master_steps: 4 },
      harmony: { root: "D", mode: "dorian" },
      instruments: { pad: { note_engine: { polyphony: 2 }, params: { wave: "saw_soft", level_db: -7.96, cutoff_hz: 800 } } },
      modulators: {
        lfo: { type: "lfo", output: "control", rate_hz: 0.05 },
        seq: { type: "step_seq", output: "note", sync: "free", length: 5, step_duration: "1/8",
               steps: [ { step: 0, voicing: { v1: { offset: 0 } } } ] },
      },
      matrix: {
        m_notes: { source: "seq", dest: "instruments.pad.notes", kind: "note", voices: 2 },
        m_cut: { source: "lfo", dest: "instruments.pad.params.cutoff_hz", kind: "mod", depth_oct: 0.664 },
      },
    }"#;

    #[test]
    fn a_clean_doc_reports_ok_with_its_structure() {
        let r = check("a.json5", DOC);
        assert!(r.ok && r.errors == 0 && r.warnings == 0, "{r:#?}");
        // free 5/8-loop sequence (5) × a 0.05 Hz LFO on a 4 s loop (5) → 5 loops = 20 s.
        let text = render_human(&r, true);
        assert!(text.contains("a.json5: ok — 0 errors, 0 warnings"), "{text}");
        assert!(text.contains("4.0 s loop · realigns every ≈ 5 loops (≈ 0:20)"), "{text}");
        assert!(text.contains("seq") && text.contains("cycle 5/8 loop"), "{text}");
    }

    #[test]
    fn a_typo_is_refused_and_located_in_both_forms() {
        let r = check("a.json5", &DOC.replacen("cutoff_hz: 800", "cutof_hz: 800", 1));
        assert!(!r.ok && r.errors == 1);
        let text = render_human(&r, false);
        assert!(text.contains("REFUSED") && text.contains("instruments.pad.params.cutof_hz: unknown key `cutof_hz` — did you mean `cutoff_hz`?  [unknown-key]"), "{text}");
        // …and by line:column, through the CST.
        let src = DOC.replacen("cutoff_hz: 800", "cutof_hz: 800", 1);
        let (line, text_line) = src.lines().enumerate().find(|(_, l)| l.contains("cutof_hz:")).unwrap();
        let column = text_line[..text_line.find("cutof_hz:").unwrap()].chars().count() + 1;
        assert!(text_contains_at(&render_human(&r, false), line + 1, column), "{}", render_human(&r, false));
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!((json["diagnostics"][0]["line"].as_u64(), json["diagnostics"][0]["column"].as_u64()), (Some(line as u64 + 1), Some(column as u64)));
        assert_eq!(json["ok"], false);
        assert_eq!(json["diagnostics"][0]["path"], "instruments.pad.params.cutof_hz");
        assert_eq!(json["diagnostics"][0]["severity"], "error");
        assert_eq!(json["structure"]["realign_loops"], 5);
    }

    fn text_contains_at(text: &str, line: usize, column: usize) -> bool {
        text.contains(&format!("error   {line}:{column} instruments.pad.params.cutof"))
    }

    #[test]
    fn a_syntax_error_has_no_structure() {
        let r = check("a.json5", "{ transport: ");
        assert!(!r.ok && r.structure.is_none());
        assert_eq!(r.diagnostics[0].diag.code, "syntax");
    }

    #[test]
    fn clock_formats() {
        assert_eq!(clock(20.0), "0:20");
        assert_eq!(clock(4000.0), "1:06:40");
    }
}
