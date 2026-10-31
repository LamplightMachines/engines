//! # The parts library (ADR 0039)
//!
//! A **part** is one musical idea that spans several sections of a loop doc — a pulse is an
//! instrument, the step sequence that plays it, a phrase cell, the note route and a reverb send.
//! [`add`] merges one into a doc's text:
//!
//! - placeholders resolve against the doc — `$id` (made unique), `$bus` (its send bus), `$target`
//!   (an existing instrument) and `"$deg(N, oct)"` (a scale degree of its mode, in semitones);
//! - each section's members are appended in the doc's own layout (the members' indentation, a
//!   trailing comma, the part's comments with them); a missing section is created where it belongs;
//!   a `safe_bounds` band the doc already has stays as authored; `patch` values are set in existing
//!   objects;
//! - the doc's own text is only added to — every comment and blank line stays — and the result is
//!   checked like the engine loads it: a part that would leave errors isn't added.
//!
//! The parts live in `assets/parts/` (see its README for the format); `engines-part`, the language
//! server's **Add part** source actions and the editor extension all come through here. A part can
//! also come from a file in the same format ([`from_file`], `engines-part add --file`): Claude's patches
//! (ADR 0050) merge through the same placeholders, check and atomic write.

use crate::cst::edit::{apply, eol, indent_unit, line_indent, line_start, only_space_before, spans_lines, TextEdit};
use crate::cst::path::{render, Seg};
use crate::cst::{Cst, Member, Node, TokenKind};
use engines_core::check::{check_str, param_applies, DocDiag};
use engines_core::theory::Key;
use std::borrow::Cow;
use std::fmt;
use std::path::Path;

/// The built-in parts (`assets/parts/<name>.json5`), by name.
const SOURCES: &[(&str, &str)] = &[
    ("bass", include_str!("../assets/parts/bass.json5")),
    ("binaural", include_str!("../assets/parts/binaural.json5")),
    ("breath", include_str!("../assets/parts/breath.json5")),
    ("drone", include_str!("../assets/parts/drone.json5")),
    ("halo", include_str!("../assets/parts/halo.json5")),
    ("lead", include_str!("../assets/parts/lead.json5")),
    ("partials", include_str!("../assets/parts/partials.json5")),
    ("pulse", include_str!("../assets/parts/pulse.json5")),
    ("shimmer", include_str!("../assets/parts/shimmer.json5")),
];

/// The sections a part adds members to, in merge order (`safe_bounds` only when the doc has one).
const SECTIONS: &[&str] = &["instruments", "buses", "sequence_pools", "modulators", "matrix", "safe_bounds"];

/// The sections whose keys are ids (unique per section).
const ID_SECTIONS: &[&str] = &["instruments", "buses", "sequence_pools", "modulators", "matrix"];

/// A doc's usual top-level order: a created section goes after the nearest one before it.
const ORDER: &[&str] = &[
    "schema_version", "loop_id", "meta", "transport", "harmony", "global", "safe_bounds", "max_drift", "instruments", "modulators",
    "sequence_pools", "matrix", "buses", "song", "optimize", "movement", "constraints",
];

/// One part: of the library, or from a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    /// Its name: the library's, or the file's stem — and the default id.
    pub name: Cow<'static, str>,
    /// The one line menus show.
    pub summary: String,
    /// What `$target` needs: a param leaf (`cutoff_hz`) or `any`; `None` when the part has no target.
    pub target: Option<String>,
    /// The key its offsets are written in (`"D mixolydian"`), for a part whose notes are raw
    /// semitones from that root rather than `$deg` (`engines-midi parts`, ADR 0053). It joins only a
    /// doc on the same root and notes; `None` for a key-agnostic part.
    pub key: Option<String>,
    pub source: Cow<'static, str>,
}

/// Every built-in part, by name.
pub fn library() -> Vec<Part> {
    SOURCES.iter().map(|&(name, source)| parse_part(Cow::Borrowed(name), Cow::Borrowed(source)).unwrap_or_else(|e| panic!("assets/parts/{name}.json5: {e}"))).collect()
}

/// The part called `name`.
pub fn get(name: &str) -> Option<Part> {
    library().into_iter().find(|p| p.name == name)
}

/// Why a part file can't be added: it can't be read, or it isn't a part.
#[derive(Debug)]
pub enum FileError {
    Read(std::io::Error),
    Invalid(String),
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FileError::Read(e) => write!(f, "{e}"),
            FileError::Invalid(e) => write!(f, "not a part: {e}"),
        }
    }
}

/// A part from a file in the library's format (a `part` header, then sections; see
/// `assets/parts/README.md`), named for the file's stem.
pub fn from_file(path: &Path) -> Result<Part, FileError> {
    let source = std::fs::read_to_string(path).map_err(FileError::Read)?;
    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("part").to_string();
    parse_part(Cow::Owned(name), Cow::Owned(source)).map_err(FileError::Invalid)
}

/// A part from its text (a generated part, before or instead of a file), named `name`.
pub fn from_source(name: &str, source: String) -> Result<Part, String> {
    parse_part(Cow::Owned(name.to_string()), Cow::Owned(source))
}

fn parse_part(name: Cow<'static, str>, source: Cow<'static, str>) -> Result<Part, String> {
    let cst = Cst::parse(&source);
    if let Some(e) = cst.errors.first() {
        return Err(format!("syntax: {}", e.message));
    }
    let header = cst.root.as_ref().and_then(|r| r.get("part")).ok_or("no `part` header")?;
    let summary = header.get("summary").and_then(Node::as_str).ok_or("no `part.summary`")?.to_string();
    let target = header.get("target").and_then(Node::as_str).map(str::to_string);
    let key = header.get("key").and_then(Node::as_str).map(str::to_string);
    if let Some(k) = &key {
        parse_key(k).ok_or_else(|| format!("`part.key`: `{k}` isn't a key (a root and a mode, e.g. \"D mixolydian\")"))?;
    }
    Ok(Part { name, summary, target, key, source })
}

/// `"D mixolydian"` → the key.
fn parse_key(s: &str) -> Option<Key> {
    let (root, mode) = s.trim().split_once(char::is_whitespace)?;
    Key::new(root, mode.trim())
}

/// The pitch classes a key's scale holds, as a 12-bit set.
fn pitch_set(k: &Key) -> u16 {
    (0..12u8).filter(|&pc| k.scale.contains(engines_core::theory::Pc(pc))).fold(0, |m, pc| m | 1 << pc)
}

/// A keyed part joins only a doc on its root with its notes: its offsets are semitones from that
/// root, so another root would transpose it, and another mode would put it out of key.
fn key_fits(part_key: &str, doc: Option<&Key>) -> Result<(), String> {
    let want = parse_key(part_key).ok_or_else(|| format!("`part.key`: `{part_key}` isn't a key"))?;
    let Some(have) = doc else {
        return Err(format!("this part is written in {} — the doc needs `harmony.root` and `harmony.mode`", want.name()));
    };
    if have.scale.root == want.scale.root && pitch_set(have) == pitch_set(&want) {
        return Ok(());
    }
    let same_notes = pitch_set(have) == pitch_set(&want);
    Err(format!(
        "this part is written in {}, and the doc is in {}{} — write the part for {} (`engines-midi parts --key \"{}\"`), or change the doc's harmony",
        want.name(),
        have.name(),
        if same_notes { " (the same notes on another root: its offsets would land a transposition away)" } else { "" },
        have.name(),
        have.name()
    ))
}

/// Choices that override the defaults.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// The part's id (default: its name, `_2`, `_3`… when taken). An id given here must be free.
    pub id: Option<String>,
    /// The bus `$bus` sends go to (default: the doc's first bus that takes sends — not an
    /// `external_return`; none ⇒ the sends are dropped).
    pub bus: Option<String>,
    /// The instrument `$target` names (default: the first one that fits).
    pub target: Option<String>,
}

/// A part merged into a doc.
#[derive(Clone, Debug)]
pub struct Added {
    /// The doc's new text.
    pub text: String,
    pub id: String,
    pub bus: Option<String>,
    pub target: Option<String>,
    /// What was added (`instruments.pulse`, …) and patched (`instruments.pad.sends_db.shimmer`).
    pub added: Vec<String>,
    /// Warnings the part brings (advice — the doc still loads).
    pub warnings: Vec<DocDiag>,
    /// Errors the doc already had (the part didn't cause them, and they're still there). The
    /// editor's source actions add a part anyway (the buffer isn't the file; the squiggles show
    /// what to fix before saving); `engines-part`, which writes the file, refuses.
    pub existing_errors: Vec<DocDiag>,
}

/// Merge `part` into the doc `doc` (its text). Errors say why it can't be added; the doc is never
/// left with new errors.
pub fn add(doc: &str, part: &Part, opts: &Options) -> Result<Added, String> {
    let cst = Cst::parse(doc);
    if let Some(e) = cst.errors.first() {
        return Err(format!("the doc has a syntax error ({}) — fix it first", e.message));
    }
    let root = cst.root.as_ref().filter(|r| r.is_object()).ok_or("the doc isn't a JSON5 object")?;
    let ids = |section: &str| -> Vec<String> { root.get(section).map(Node::members).unwrap_or_default().iter().map(|m| m.key.name.clone()).collect() };

    let target = match &part.target {
        None => None,
        Some(need) => Some(pick_target(root, need, opts.target.as_deref())?),
    };
    let buses = ids("buses");
    // An `external_return` bus takes no sends (the checker refuses them): `$bus` never defaults to one.
    let external = |b: &str| root.get("buses").and_then(|bs| bs.get(b)).and_then(|v| v.get("type")).and_then(Node::as_str) == Some("external_return");
    let bus = match &opts.bus {
        Some(b) if !buses.contains(b) => return Err(format!("no bus `{b}` in the doc (it has {})", list(&buses))),
        Some(b) if external(b) => return Err(format!("bus `{b}` is an external_return bus — it takes no sends; choose another")),
        Some(b) => Some(b.clone()),
        None => buses.iter().find(|b| !external(b)).cloned(),
    };
    let key = root.get("harmony").and_then(|h| Key::new(h.get("root")?.as_str()?, h.get("mode")?.as_str()?));
    if let Some(k) = &part.key {
        key_fits(k, key.as_ref())?;
    }

    let base = opts.id.clone().unwrap_or_else(|| part.name.to_string());
    if !is_plain_id(&base) {
        return Err(format!("`{base}` can't be an id — letters, digits and _, not starting with a digit"));
    }
    let (id, text) = (1..)
        .map(|n| if n == 1 { base.clone() } else { format!("{base}_{n}") })
        .map(|id| instantiate(&part.source, &id, bus.as_deref(), target.as_deref(), key.as_ref()).map(|text| (id, text)))
        .find(|r| match r {
            Ok((_, text)) => !clashes(text, &ids),
            Err(_) => true,
        })
        .expect("an unbounded search ends")?;
    if opts.id.is_some() && id != base {
        return Err(format!("`{base}` is taken in the doc — choose another id"));
    }

    let pcst = Cst::parse(&text);
    if let Some(e) = pcst.errors.first() {
        return Err(format!("part `{}` doesn't parse once filled in: {}", part.name, e.message));
    }
    let proot = pcst.root.as_ref().expect("parsed");
    let mut out = doc.to_string();
    let mut added = Vec::new();
    for &section in SECTIONS {
        let Some(obj) = proot.get(section) else { continue };
        if section == "safe_bounds" && root.get("safe_bounds").is_none() {
            continue; // the doc doesn't freeze levers: neither does the part
        }
        // A band the doc already has (left behind when the part was removed by hand) is the author's:
        // it stays as written, and the part's default isn't added as a second, shadowing key.
        let kept = if section == "safe_bounds" { ids(section) } else { Vec::new() };
        let members: Vec<&Member> = obj.members().iter().filter(|m| !kept.contains(&m.key.name)).collect();
        let blocks: Vec<Block> = members.iter().map(|m| Block::of(&pcst, &text, m)).collect();
        if blocks.is_empty() {
            continue;
        }
        out = insert_members(&out, section, &blocks)?;
        added.extend(members.iter().map(|m| format!("{section}.{}", m.key.name)));
    }
    for m in proot.get("patch").map(Node::members).unwrap_or_default() {
        let value = m.value.as_ref().map(|v| v.span.text(&text)).unwrap_or("null");
        out = set_path(&out, &m.key.name, value)?;
        added.push(m.key.name.clone());
    }

    // What the doc already had vs what the part brings — each finding before pairs with at most one
    // after, so a second copy of one is still the part's.
    let mut before = check_str(doc).diags;
    let (mut existing_errors, mut fresh) = (Vec::new(), Vec::new());
    for d in check_str(&out).diags {
        match before.iter().position(|b| same_finding(b, &d)) {
            Some(i) => {
                before.swap_remove(i);
                if d.is_error() {
                    existing_errors.push(d);
                }
            }
            None => fresh.push(d),
        }
    }
    let errors: Vec<String> = fresh.iter().filter(|d| d.is_error()).map(|d| format!("  {d}")).collect();
    if !errors.is_empty() {
        return Err(format!("adding `{}` would leave the doc with errors:\n{}", part.name, errors.join("\n")));
    }
    let bus = bus.filter(|_| part.source.contains("$bus")); // reported only when the part sends
    Ok(Added { text: out, id, bus, target, added, warnings: fresh, existing_errors })
}

/// The same finding before and after a merge: its severity, node and code — not its message, whose
/// did-you-mean hint names the ids the doc has *now* (`unknown instrument `pulsee`` gains
/// `— did you mean `pulse`?` once a pulse is added). A doc-wide finding (no path: a lint, a type
/// error) has only its message to tell it apart, so that is compared too, hint stripped.
fn same_finding(a: &DocDiag, b: &DocDiag) -> bool {
    let stem = |m: &str| m.split(" — did you mean").next().unwrap_or_default().to_string();
    a.severity == b.severity && a.path == b.path && a.code == b.code && (!a.path.is_empty() || stem(&a.message) == stem(&b.message))
}

fn list(xs: &[String]) -> String {
    if xs.is_empty() { "none".into() } else { xs.iter().map(|x| format!("`{x}`")).collect::<Vec<_>>().join(", ") }
}

/// An id that is a bare JSON5 key and needs no quoting in a JSONPath.
fn is_plain_id(s: &str) -> bool {
    let mut cs = s.chars();
    cs.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The instrument `$target` names: `wanted`, or the first that has `need` (`any` fits all).
fn pick_target(root: &Node, need: &str, wanted: Option<&str>) -> Result<String, String> {
    let insts = root.get("instruments").map(Node::members).unwrap_or_default();
    let fits = |m: &Member| {
        need == "any"
            || m.value.as_ref().is_some_and(|v| {
                let kind = v.get("type").and_then(Node::as_str).unwrap_or("subtractive");
                let noise = v.get("params").and_then(|p| p.get("source")).and_then(Node::as_str) == Some("filtered_noise");
                let pitched = v.get("pitched").and_then(Node::as_bool) == Some(true);
                param_applies(kind, noise, pitched, need)
            })
    };
    let what = if need == "any" { "an instrument".to_string() } else { format!("an instrument with a `{need}`") };
    match wanted {
        Some(w) => match insts.iter().find(|m| m.key.name == w) {
            Some(m) if fits(m) => Ok(w.to_string()),
            Some(_) => Err(format!("this part needs {what}, and `{w}` has none")),
            None => Err(format!("no instrument `{w}` in the doc")),
        },
        None => insts.iter().find(|m| fits(m)).map(|m| m.key.name.clone()).ok_or(format!("this part needs {what}; the doc has none")),
    }
}

/// Whether the filled-in part `text` adds an id the doc already has (per id section).
fn clashes(text: &str, doc_ids: &dyn Fn(&str) -> Vec<String>) -> bool {
    let cst = Cst::parse(text);
    let Some(root) = cst.root.as_ref() else { return false };
    ID_SECTIONS.iter().any(|&s| {
        let taken = doc_ids(s);
        root.get(s).map(Node::members).unwrap_or_default().iter().any(|m| taken.contains(&m.key.name))
    })
}

/// Fill in a part's placeholders. `bus: None` drops its `$bus` sends; `"$deg(…)"` needs the key.
fn instantiate(src: &str, id: &str, bus: Option<&str>, target: Option<&str>, key: Option<&Key>) -> Result<String, String> {
    let mut text = src.to_string();
    if bus.is_none() {
        text = drop_members(&text, "$bus");
    }
    text = fill_degrees(&text, key)?;
    if let Some(t) = target {
        text = text.replace("$target", t);
    }
    if let Some(b) = bus {
        text = text.replace("$bus", b);
    }
    text = text.replace("$id", id);
    if let Some(i) = text.find('$') {
        let rest: String = text[i..].chars().take(16).collect();
        return Err(format!("an unresolved placeholder in the part: {rest}…"));
    }
    Ok(text)
}

/// Remove every member keyed `key` (a `$bus` send when the doc has no bus).
fn drop_members(src: &str, key: &str) -> String {
    let mut text = src.to_string();
    loop {
        let cst = Cst::parse(&text);
        let Some(path) = cst.root.as_ref().and_then(|r| find_key(r, key, &mut Vec::new())) else { return text };
        match cst.remove_member(&text, &path) {
            Some(edits) => text = apply(&text, &edits),
            None => return text,
        }
    }
}

fn find_key(node: &Node, key: &str, segs: &mut Vec<Seg>) -> Option<String> {
    for m in node.members() {
        segs.push(Seg::Key(m.key.name.clone()));
        if m.key.name == key {
            return Some(render(segs));
        }
        if let Some(found) = m.value.as_ref().and_then(|v| find_key(v, key, segs)) {
            return Some(found);
        }
        segs.pop();
    }
    for (i, it) in node.items().iter().enumerate() {
        segs.push(Seg::Index(i));
        if let Some(found) = find_key(&it.node, key, segs) {
            return Some(found);
        }
        segs.pop();
    }
    None
}

/// `"$deg(N)"` / `"$deg(N, oct)"` (either quote) → the semitones of scale degree N (1 = the root)
/// of the doc's mode, `oct` octaves up or down.
fn fill_degrees(src: &str, key: Option<&Key>) -> Result<String, String> {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(i) = rest.find("$deg(") {
        let quote = rest[..i].chars().last().filter(|c| *c == '"' || *c == '\'').ok_or("`$deg(…)` must be a quoted string")?;
        let close = rest[i..].find(')').map(|j| i + j).ok_or("an unclosed `$deg(`")?;
        if !rest[close + 1..].starts_with(quote) {
            return Err("`$deg(…)` must be the whole string".into());
        }
        let args: Vec<i32> = rest[i + 5..close].split(',').map(|a| a.trim().parse::<i32>().map_err(|_| format!("`$deg({})`: whole numbers", &rest[i + 5..close]))).collect::<Result<_, _>>()?;
        let (degree, oct) = match args[..] {
            [d] => (d, 0),
            [d, o] => (d, o),
            _ => return Err(format!("`$deg({})`: a degree and an optional octave", &rest[i + 5..close])),
        };
        if degree < 1 {
            return Err(format!("`$deg({degree})`: degrees count from 1 (the root)"));
        }
        let key = key.ok_or("this part pitches by scale degree — the doc needs `harmony.root` and `harmony.mode`")?;
        out.push_str(&rest[..i - 1]);
        out.push_str(&(key.scale.degree_semitone(degree - 1) + 12 * oct).to_string());
        rest = &rest[close + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// A member of the part as text: its own comment lines above it, the member (`key: value`, lines
/// after the first relative to the key's column) and a comment trailing its last line.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Block {
    comments: Vec<String>,
    lines: Vec<String>,
    trailing: Option<String>,
}

impl Block {
    fn of(cst: &Cst, src: &str, m: &Member) -> Block {
        let start = m.key.span.start;
        let col = if only_space_before(src, start) { start - line_start(src, start) } else { 0 };
        let lines: Vec<String> = m
            .span()
            .text(src)
            .split('\n')
            .enumerate()
            .map(|(i, l)| {
                let l = l.trim_end_matches('\r');
                if i == 0 {
                    return l.to_string();
                }
                let lead = l.len() - l.trim_start_matches([' ', '\t']).len();
                l[lead.min(col)..].to_string()
            })
            .collect();
        // Own-line comments directly above (no blank line between).
        let mut comments = Vec::new();
        let i = cst.tokens.partition_point(|t| t.span.start < start);
        let mut j = i;
        let mut first = true;
        while j > 0 {
            j -= 1;
            let t = &cst.tokens[j];
            let text = t.span.text(src);
            match t.kind {
                TokenKind::Whitespace => {
                    let newlines = text.matches('\n').count();
                    if (first && newlines == 0) || newlines >= 2 {
                        break;
                    }
                }
                TokenKind::LineComment | TokenKind::BlockComment if only_space_before(src, t.span.start) => {
                    for l in text.lines().rev() {
                        comments.insert(0, l.trim().to_string());
                    }
                }
                _ => break,
            }
            first = false;
        }
        // A comment on the member's last line, after its comma.
        let mut trailing = None;
        for t in cst.tokens.iter().skip_while(|t| t.span.start < m.span().end) {
            match t.kind {
                TokenKind::Whitespace if !spans_lines(t.span.text(src)) => {}
                TokenKind::Comma => {}
                TokenKind::LineComment => {
                    trailing = Some(t.span.text(src).to_string());
                    break;
                }
                _ => break,
            }
        }
        Block { comments, lines, trailing }
    }

    /// The block at indentation `indent`, one line per element (no newlines), its member ending `,`.
    fn at(&self, indent: &str) -> Vec<String> {
        let mut out: Vec<String> = self.comments.iter().map(|c| format!("{indent}{c}")).collect();
        let n = self.lines.len();
        for (i, l) in self.lines.iter().enumerate() {
            let mut line = if l.is_empty() { String::new() } else { format!("{indent}{l}") };
            if i + 1 == n {
                line.push(',');
                if let Some(t) = &self.trailing {
                    line.push(' ');
                    line.push_str(t);
                }
            }
            out.push(line);
        }
        out
    }

    /// The member on one line, comments dropped (for a one-line object).
    fn compact(&self) -> String {
        let src = format!("{{{}}}", self.lines.join("\n"));
        let cst = Cst::parse(&src);
        let mut out = String::new();
        for t in cst.tokens.iter().filter(|t| t.span.start >= 1 && t.span.end < src.len()) {
            if t.kind.is_trivia() {
                if !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
                continue;
            }
            let s = t.span.text(&src);
            if matches!(t.kind, TokenKind::RBrace | TokenKind::RBracket) {
                // A trailing comma inside goes on one line; keep `{ … }` spacing.
                let kept = out.trim_end().strip_suffix(',').unwrap_or(out.trim_end()).to_string();
                out = kept + " ";
            }
            out.push_str(s);
        }
        out.trim().to_string()
    }
}

/// Lines (each without its line ending) as text inserted after `at`: each line after a `nl` (the
/// doc's own line ending).
fn lines_text(lines: &[String], blank_first: bool, nl: &str) -> String {
    let mut s = String::new();
    if blank_first {
        s.push_str(nl);
    }
    for l in lines {
        s.push_str(nl);
        s.push_str(l);
    }
    s
}

/// Append `blocks` to the doc's `section` object (creating the section where it belongs).
fn insert_members(doc: &str, section: &str, blocks: &[Block]) -> Result<String, String> {
    let cst = Cst::parse(doc);
    let root = cst.root.as_ref().expect("checked");
    let (unit, nl) = (indent_unit(doc), eol(doc));
    let Some(member) = root.member(section) else {
        return create_section(&cst, doc, root, section, blocks);
    };
    let obj = member.value.as_ref().filter(|v| v.is_object()).ok_or(format!("the doc's `{section}` isn't an object"))?;
    let closed = doc.as_bytes().get(obj.span.end.wrapping_sub(1)) == Some(&b'}');
    let inner_end = if closed { obj.span.end - 1 } else { obj.span.end };
    let edits = match obj.members() {
        [] => {
            // `{}` or an empty multi-line object: open it up.
            let close_indent = if spans_lines(&doc[obj.span.start + 1..inner_end]) { line_indent(doc, inner_end).to_string() } else { line_indent(doc, member.key.span.start).to_string() };
            let indent = format!("{close_indent}{unit}");
            let lines: Vec<String> = blocks.iter().flat_map(|b| b.at(&indent)).collect();
            vec![TextEdit::new(crate::cst::Span::new(obj.span.start + 1, inner_end), format!("{}{nl}{close_indent}", lines_text(&lines, false, nl)))]
        }
        members => {
            let first = &members[0];
            let last = members.last().expect("non-empty");
            let anchor = last.comma.map(|c| c.end).unwrap_or(last.span().end);
            let multiline = spans_lines(&doc[obj.span.start + 1..first.key.span.start]);
            if multiline {
                let indent = if only_space_before(doc, first.key.span.start) { line_indent(doc, first.key.span.start).to_string() } else { format!("{}{unit}", line_indent(doc, member.key.span.start)) };
                let mut edits = Vec::new();
                if last.comma.is_none() {
                    edits.push(TextEdit::insert(last.span().end, ","));
                }
                let mut text = String::new();
                for b in blocks {
                    text.push_str(&lines_text(&b.at(&indent), !b.comments.is_empty(), nl));
                }
                edits.push(TextEdit::insert(cst.line_end_after(doc, anchor), text));
                edits
            } else {
                let compact: Vec<String> = blocks.iter().map(Block::compact).collect();
                vec![if last.comma.is_some() { TextEdit::insert(anchor, format!(" {},", compact.join(", "))) } else { TextEdit::insert(anchor, format!(", {}", compact.join(", "))) }]
            }
        }
    };
    Ok(apply(doc, &edits))
}

/// Add `section: { blocks }` to the root, after the nearest section that comes before it.
fn create_section(cst: &Cst, doc: &str, root: &Node, section: &str, blocks: &[Block]) -> Result<String, String> {
    let rank = |k: &str| ORDER.iter().position(|o| *o == k);
    let mine = rank(section).unwrap_or(ORDER.len());
    let members = root.members();
    let after = members.iter().rposition(|m| rank(&m.key.name).is_some_and(|r| r < mine)).or(members.len().checked_sub(1));
    let Some(after) = after else {
        // An empty root: `{ section: { … } }`.
        let compact: Vec<String> = blocks.iter().map(Block::compact).collect();
        return Ok(apply(doc, &[TextEdit::insert(root.span.start + 1, format!(" {section}: {{ {} }} ", compact.join(", ")))]));
    };
    let prev = &members[after];
    let multiline = spans_lines(&doc[root.span.start + 1..members[0].key.span.start]);
    let mut edits = Vec::new();
    if prev.comma.is_none() {
        edits.push(TextEdit::insert(prev.span().end, ","));
    }
    let anchor = prev.comma.map(|c| c.end).unwrap_or(prev.span().end);
    if multiline {
        let indent = line_indent(doc, members[0].key.span.start).to_string();
        let inner = format!("{indent}{}", indent_unit(doc));
        let mut lines = vec![format!("{indent}{section}: {{")];
        lines.extend(blocks.iter().flat_map(|b| b.at(&inner)));
        lines.push(format!("{indent}}},"));
        edits.push(TextEdit::insert(cst.line_end_after(doc, anchor), lines_text(&lines, true, eol(doc))));
    } else {
        let compact: Vec<String> = blocks.iter().map(Block::compact).collect();
        edits.push(TextEdit::insert(anchor, format!(" {section}: {{ {} }},", compact.join(", "))));
    }
    Ok(apply(doc, &edits))
}

/// Set the value at a dotted `path` (plain keys), creating missing objects on the way.
fn set_path(doc: &str, path: &str, value: &str) -> Result<String, String> {
    let cst = Cst::parse(doc);
    let segs: Vec<&str> = path.split('.').collect();
    let (key, parents) = segs.split_last().ok_or("an empty patch path")?;
    // The deepest existing object along the path.
    let mut depth = parents.len();
    while depth > 0 {
        let f = cst.find(&parents[..depth].join("."));
        if f.complete && f.node.is_some_and(Node::is_object) {
            break;
        }
        depth -= 1;
    }
    if depth < 2 && parents.len() >= 2 {
        return Err(format!("patch `{path}`: `{}` isn't in the doc", parents[..2].join(".")));
    }
    let (object, missing) = (parents[..depth].join("."), &parents[depth..]);
    let edits = if missing.is_empty() {
        cst.set_member(doc, &object, key, value)
    } else {
        let mut nested = format!("{{ {key}: {value} }}");
        for k in missing[1..].iter().rev() {
            nested = format!("{{ {k}: {nested} }}");
        }
        cst.insert_member(doc, &object, missing[0], &nested)
    };
    let edits = edits.ok_or(format!("patch `{path}`: can't set it"))?;
    Ok(apply(doc, &edits))
}

/// The edits that turn `old` into `new`, line by line (a minimal diff), for an editor to apply.
pub fn diff_edits(old: &str, new: &str) -> Vec<TextEdit> {
    let a: Vec<&str> = old.split_inclusive('\n').collect();
    let b: Vec<&str> = new.split_inclusive('\n').collect();
    // Common prefix / suffix first: an added part is a few insertions in a long doc.
    let pre = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suf = a[pre..].iter().rev().zip(b[pre..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (am, bm) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    // LCS over the middle.
    let (n, m) = (am.len(), bm.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if am[i] == bm[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
        }
    }
    let offset_of = |lines: &[&str], upto: usize| -> usize { lines[..upto].iter().map(|l| l.len()).sum() };
    let base = offset_of(&a, pre);
    let mut edits = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && am[i] == bm[j] {
            i += 1;
            j += 1;
            continue;
        }
        let (si, sj) = (i, j);
        while (i < n || j < m) && !(i < n && j < m && am[i] == bm[j]) {
            if j < m && (i == n || lcs[i][j + 1] >= lcs[i + 1][j]) {
                j += 1;
            } else {
                i += 1;
            }
        }
        let start = base + offset_of(am, si);
        let end = base + offset_of(am, i);
        edits.push(TextEdit::new(crate::cst::Span::new(start, end), bm[sj..j].concat()));
    }
    edits
}

#[cfg(test)]
mod tests;
