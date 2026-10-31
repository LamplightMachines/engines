//! Comment-preserving edits: each operation returns the minimal [`TextEdit`]s that make the change,
//! touching nothing else — every comment, blank line and formatting choice around it survives. The
//! code actions in `engines-lsp` and the parts library (`engines-part`, P8) build on these.

use super::value::{is_identifier, key_text};
use super::{Cst, Node, Span, TokenKind};

/// Replace `span` of the source with `text`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub span: Span,
    pub text: String,
}

impl TextEdit {
    pub fn new(span: Span, text: impl Into<String>) -> TextEdit {
        TextEdit { span, text: text.into() }
    }
    pub fn insert(at: usize, text: impl Into<String>) -> TextEdit {
        TextEdit { span: Span::new(at, at), text: text.into() }
    }
}

/// Apply non-overlapping edits (in any order) to `src`.
pub fn apply(src: &str, edits: &[TextEdit]) -> String {
    let mut sorted: Vec<&TextEdit> = edits.iter().collect();
    sorted.sort_by_key(|e| (e.span.start, e.span.end));
    let mut out = String::with_capacity(src.len());
    let mut at = 0;
    for e in sorted {
        out.push_str(&src[at..e.span.start.max(at)]);
        out.push_str(&e.text);
        at = e.span.end.max(at);
    }
    out.push_str(&src[at..]);
    out
}

/// A key as written for `name`, keeping the original's quoting style where it can.
pub fn key_as(name: &str, quote: Option<char>) -> String {
    match quote {
        Some('\'') => format!("'{}'", name.replace('\\', "\\\\").replace('\'', "\\'")),
        Some(_) => serde_json::Value::String(name.to_string()).to_string(),
        None => key_text(name),
    }
}

/// A string value as written, in the given quote style.
pub fn string_as(value: &str, quote: char) -> String {
    if quote == '\'' { key_as(value, Some('\'')) } else { serde_json::Value::String(value.to_string()).to_string() }
}

impl Cst {
    /// Replace the value at `path` with `text` (already JSON5).
    pub fn replace_value(&self, path: &str, text: &str) -> Option<Vec<TextEdit>> {
        let f = self.find(path);
        let node = f.node.filter(|_| f.complete)?;
        Some(vec![TextEdit::new(node.span, text)])
    }

    /// Rename the key at `path` (quoting kept where possible).
    pub fn rename_key(&self, path: &str, new_name: &str) -> Option<Vec<TextEdit>> {
        let f = self.find(path);
        let m = f.member.filter(|_| f.complete)?;
        let quote = if m.key.quote.is_none() && !is_identifier(new_name) { Some('"') } else { m.key.quote };
        Some(vec![TextEdit::new(m.key.span, key_as(new_name, quote))])
    }

    /// Set `key: value_text` in the object at `object_path`: replace the value when the key exists,
    /// else insert a member.
    pub fn set_member(&self, src: &str, object_path: &str, key: &str, value_text: &str) -> Option<Vec<TextEdit>> {
        let f = self.find(object_path);
        let obj = f.node.filter(|n| f.complete && n.is_object())?;
        match obj.member(key) {
            Some(m) => match &m.value {
                Some(v) => Some(vec![TextEdit::new(v.span, value_text)]),
                None => Some(vec![TextEdit::new(Span::new(m.key.span.start, m.span().end), format!("{}: {value_text}", key_as(key, m.key.quote)))]),
            },
            None => self.insert_member(src, object_path, key, value_text),
        }
    }

    /// Append `key: value_text` to the object at `object_path`, in its layout: a multi-line object
    /// gets a new line at the members' indentation (after any trailing comment on the last member's
    /// line, with a trailing comma); a one-line object gets `, key: value`.
    pub fn insert_member(&self, src: &str, object_path: &str, key: &str, value_text: &str) -> Option<Vec<TextEdit>> {
        let f = self.find(object_path);
        let obj = f.node.filter(|n| f.complete && n.is_object())?;
        self.insert_into(src, obj, key, value_text)
    }

    /// [`Cst::insert_member`] on an object node of this tree.
    pub fn insert_into(&self, src: &str, obj: &Node, key: &str, value_text: &str) -> Option<Vec<TextEdit>> {
        self.insert_text(src, obj, &format!("{}: {value_text}", key_text(key)), ",")
    }

    /// [`Cst::insert_into`] for a strict-JSON doc: the key quoted and no trailing comma, so the file
    /// stays JSON (`value_text` must be JSON too).
    pub fn insert_into_json(&self, src: &str, obj: &Node, key: &str, value_text: &str) -> Option<Vec<TextEdit>> {
        self.insert_text(src, obj, &format!("{}: {value_text}", key_as(key, Some('"'))), "")
    }

    /// Insert the member text `member` into `obj`; `trailing` follows a new multi-line member.
    fn insert_text(&self, src: &str, obj: &Node, member: &str, trailing: &str) -> Option<Vec<TextEdit>> {
        if !obj.is_object() {
            return None;
        }
        let closed = src.as_bytes().get(obj.span.end.wrapping_sub(1)) == Some(&b'}');
        let inner = Span::new(obj.span.start + 1, if closed { obj.span.end - 1 } else { obj.span.end });
        let nl = eol(src);
        let Some(last) = obj.members().last() else {
            let inner_text = inner.text(src);
            return Some(vec![if spans_lines(inner_text) {
                let indent = format!("{}{}", line_indent(src, obj.span.end.saturating_sub(1)), indent_unit(src));
                TextEdit::insert(inner.start, format!("{nl}{indent}{member}{trailing}"))
            } else if inner_text.trim().is_empty() {
                TextEdit::new(inner, format!(" {member} "))
            } else {
                TextEdit::insert(inner.start, format!(" {member},"))
            }]);
        };
        let first = &obj.members()[0];
        let multiline = spans_lines(&src[inner.start..first.key.span.start]);
        let anchor = last.comma.map(|c| c.end).unwrap_or(last.span().end);
        let mut edits = Vec::new();
        if multiline {
            let indent = [last.key.span.start, first.key.span.start]
                .into_iter()
                .find(|&k| only_space_before(src, k))
                .map(|k| line_indent(src, k).to_string())
                .unwrap_or_else(|| format!("{}{}", line_indent(src, obj.span.end.saturating_sub(1)), indent_unit(src)));
            if last.comma.is_none() {
                edits.push(TextEdit::insert(last.span().end, ","));
            }
            edits.push(TextEdit::insert(self.line_end_after(src, anchor), format!("{nl}{indent}{member}{trailing}")));
        } else if last.comma.is_some() {
            edits.push(TextEdit::insert(anchor, format!(" {member},")));
        } else {
            edits.push(TextEdit::insert(anchor, format!(", {member}")));
        }
        Some(edits)
    }

    /// Remove the member at `path`. Alone on its line(s) in a multi-line object, its whole lines go
    /// (a trailing comment on its last line with it; comment lines above it stay); inline, the member
    /// and its comma go.
    pub fn remove_member(&self, src: &str, path: &str) -> Option<Vec<TextEdit>> {
        let f = self.find(path);
        let m = f.member.filter(|_| f.complete)?;
        let parent_path = &f.segs[..f.segs.len() - 1];
        let pf = self.find(&super::path::render(parent_path));
        let obj: &Node = pf.node?;
        let members = obj.members();
        let idx = members.iter().position(|x| std::ptr::eq(x, m))?;
        let anchor = m.comma.map(|c| c.end).unwrap_or(m.span().end);
        let end = self.line_end_after(src, anchor);
        let alone_after = end < src.len() && matches!(src.as_bytes()[end], b'\n' | b'\r') || end == src.len();
        if only_space_before(src, m.key.span.start) && alone_after {
            let start = line_start(src, m.key.span.start);
            let mut stop = end;
            if src[stop..].starts_with("\r\n") {
                stop += 2;
            } else if stop < src.len() {
                stop += 1;
            }
            return Some(vec![TextEdit::new(Span::new(start, stop), "")]);
        }
        if members.len() == 1 {
            let closed = src.as_bytes().get(obj.span.end.wrapping_sub(1)) == Some(&b'}');
            let inner = Span::new(obj.span.start + 1, if closed { obj.span.end - 1 } else { obj.span.end });
            let rest_blank = src[inner.start..m.key.span.start].trim().is_empty() && src[anchor..inner.end].trim().is_empty();
            return Some(vec![if rest_blank { TextEdit::new(inner, "") } else { TextEdit::new(Span::new(m.key.span.start, anchor), "") }]);
        }
        Some(vec![match (m.comma, idx.checked_sub(1).map(|i| &members[i])) {
            // `a: 1, b: 2 }` → drop `b: 2` with the comma before it
            (None, Some(prev)) if prev.comma.is_some() => TextEdit::new(Span::new(prev.comma.expect("checked").start, m.span().end), ""),
            // `a: 1, b: 2, …` → drop `a: 1, ` up to the next member
            _ => {
                let next_start = members.get(idx + 1).map(|n| n.key.span.start).unwrap_or(anchor);
                let stop = if src[anchor..next_start].trim().is_empty() { next_start } else { anchor };
                TextEdit::new(Span::new(m.key.span.start, stop), "")
            }
        }])
    }

    /// The offset of the end of the line `anchor` is on, past same-line trivia (spaces, a trailing
    /// comment) — or `anchor` itself when real content follows on that line.
    pub(crate) fn line_end_after(&self, src: &str, anchor: usize) -> usize {
        let mut at = anchor;
        let i = self.tokens.partition_point(|t| t.span.start < anchor);
        for t in &self.tokens[i..] {
            match t.kind {
                TokenKind::Whitespace => match t.span.text(src).find(['\n', '\r']) {
                    Some(nl) => return t.span.start + nl,
                    None => at = t.span.end,
                },
                TokenKind::LineComment => at = t.span.end,
                TokenKind::BlockComment if !spans_lines(t.span.text(src)) => at = t.span.end,
                _ => return anchor,
            }
        }
        at
    }
}

// Layout helpers, shared with the parts library. Lines end at `\n`, `\r\n` or `\r` (as LSP counts
// them), and inserted text uses the doc's own line ending.

/// The start of `offset`'s line.
pub(crate) fn line_start(src: &str, offset: usize) -> usize {
    src[..offset.min(src.len())].rfind(['\n', '\r']).map(|i| i + 1).unwrap_or(0)
}

/// Only spaces/tabs between the start of `offset`'s line and `offset`.
pub(crate) fn only_space_before(src: &str, offset: usize) -> bool {
    src[line_start(src, offset)..offset].chars().all(|c| c == ' ' || c == '\t')
}

/// The leading spaces/tabs of `offset`'s line.
pub fn line_indent(src: &str, offset: usize) -> &str {
    let rest = &src[line_start(src, offset)..];
    &rest[..rest.len() - rest.trim_start_matches([' ', '\t']).len()]
}

/// The doc's indentation step: a tab if lines indent with tabs, else two spaces.
pub(crate) fn indent_unit(src: &str) -> &'static str {
    if src.split(['\n', '\r']).any(|l| l.starts_with('\t')) { "\t" } else { "  " }
}

/// The doc's line ending, for the lines an edit inserts: `\r\n` when it has any, else `\r` for a
/// CR-only doc, else `\n`.
pub(crate) fn eol(src: &str) -> &'static str {
    if src.contains("\r\n") {
        "\r\n"
    } else if src.contains('\r') {
        "\r"
    } else {
        "\n"
    }
}

/// Whether `text` holds a line break (of any kind).
pub(crate) fn spans_lines(text: &str) -> bool {
    text.contains(['\n', '\r'])
}
