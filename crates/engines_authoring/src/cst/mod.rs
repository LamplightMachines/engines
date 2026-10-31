//! # A lossless JSON5 concrete syntax tree (ADR 0037)
//!
//! The editor-side view of a loop doc. The engine parses JSON5 with the `json5` crate straight into
//! values; tools need more: **where** every key and value sits (to put a diagnostic on `cutof`, to
//! complete at the cursor), the **comments** (to edit a doc without destroying its annotations), and
//! a tree even while the doc is half-typed (`params: { cut }` mid-keystroke).
//!
//! - [`Cst::parse`] lexes the whole text into [`Token`]s — trivia (whitespace, comments) included, so
//!   the tokens concatenate back to the source byte for byte — and builds a tree of [`Node`]s with
//!   byte [`Span`]s. It **recovers** from errors (missing commas/colons/values, unclosed brackets,
//!   stray tokens), collecting [`SyntaxError`]s instead of stopping at the first.
//! - [`Cst::to_value`] gives the `serde_json::Value` of an error-free doc — tested identical to the
//!   `json5` crate's (the engine's parser) on every tracked doc.
//! - [`path`] maps between the checker's JSONPaths (`instruments.pad.params.cutoff_hz`,
//!   `modulators.arp.steps[2]`) and nodes, and says what the cursor is on.
//! - [`edit`] produces comment-preserving text edits (replace a value, rename a key, insert or remove
//!   a member).
//! - [`lines`] converts byte offsets to LSP positions (line + UTF-16 column) and back.
//!
//! Positions are **byte offsets** into the UTF-8 source everywhere in here; UTF-16 appears only at the
//! LSP edge.

pub mod edit;
mod lex;
pub mod lines;
mod parse;
pub mod path;
mod value;

pub use value::to_json5;
pub(crate) use value::is_identifier;

/// A byte range `start..end` in the source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Span {
        Span { start, end }
    }
    pub fn len(&self) -> usize {
        self.end - self.start
    }
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
    /// `offset` lies inside (`start ≤ offset < end`).
    pub fn contains(&self, offset: usize) -> bool {
        self.start <= offset && offset < self.end
    }
    /// `offset` lies inside or right at either edge — a cursor just after a token still "touches" it.
    pub fn touches(&self, offset: usize) -> bool {
        self.start <= offset && offset <= self.end
    }
    pub fn text<'a>(&self, src: &'a str) -> &'a str {
        &src[self.start..self.end]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Colon,
    Comma,
    /// A single- or double-quoted string (possibly unterminated — then a [`SyntaxError`] says so).
    String,
    /// A number, signed `Infinity`/`NaN` included.
    Number,
    /// An identifier: an unquoted key, `true`/`false`/`null`, `Infinity`/`NaN`.
    Word,
    LineComment,
    BlockComment,
    Whitespace,
    /// Anything else (one character).
    Unknown,
}

impl TokenKind {
    pub fn is_trivia(self) -> bool {
        matches!(self, TokenKind::Whitespace | TokenKind::LineComment | TokenKind::BlockComment)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxError {
    pub span: Span,
    pub message: String,
}

/// A parsed number literal, kept the way `json5` reads it (so [`Cst::to_value`] matches).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Num {
    /// Unsigned (or `+`-signed) integer — decimal or hex.
    UInt(u128),
    /// `-`-signed integer.
    Int(i128),
    /// Anything with `.`/`e`, and `Infinity`/`NaN`.
    Float(f64),
}

impl Num {
    pub fn as_f64(&self) -> f64 {
        match *self {
            Num::UInt(u) => u as f64,
            Num::Int(i) => i as f64,
            Num::Float(f) => f,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum NodeKind {
    Object(Vec<Member>),
    Array(Vec<Item>),
    String(String),
    Number(Num),
    Bool(bool),
    Null,
    /// A token where a value should be that isn't one (an unquoted word, a bad number). Its span is
    /// kept so completion can still replace it.
    Invalid,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub span: Span,
    pub kind: NodeKind,
}

impl Node {
    pub fn members(&self) -> &[Member] {
        match &self.kind {
            NodeKind::Object(m) => m,
            _ => &[],
        }
    }
    pub fn items(&self) -> &[Item] {
        match &self.kind {
            NodeKind::Array(i) => i,
            _ => &[],
        }
    }
    pub fn is_object(&self) -> bool {
        matches!(self.kind, NodeKind::Object(_))
    }
    pub fn is_container(&self) -> bool {
        matches!(self.kind, NodeKind::Object(_) | NodeKind::Array(_))
    }
    /// The member with this key (the **last** one when a key repeats — what `json5` keeps).
    pub fn member(&self, key: &str) -> Option<&Member> {
        self.members().iter().rev().find(|m| m.key.name == key)
    }
    /// The value under `key`, when it has one.
    pub fn get(&self, key: &str) -> Option<&Node> {
        self.member(key).and_then(|m| m.value.as_ref())
    }
    pub fn as_str(&self) -> Option<&str> {
        match &self.kind {
            NodeKind::String(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match &self.kind {
            NodeKind::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match &self.kind {
            NodeKind::Number(n) => Some(n.as_f64()),
            _ => None,
        }
    }
    /// This subtree as a value (members still being typed are skipped; invalid tokens are `null`).
    pub fn value(&self) -> serde_json::Value {
        value::node_value(self)
    }
    /// One level of this node as a value: an object's scalar members as values, its containers as
    /// empty ones (enough for a schema's `type` / `const` / `required` conditions, at the cost of
    /// the node's own members rather than its whole subtree).
    pub fn shallow_value(&self) -> serde_json::Value {
        use serde_json::Value;
        let flat = |n: &Node| match &n.kind {
            NodeKind::Object(_) => Value::Object(Default::default()),
            NodeKind::Array(_) => Value::Array(Vec::new()),
            _ => value::node_value(n),
        };
        match &self.kind {
            NodeKind::Object(members) => Value::Object(members.iter().filter_map(|m| Some((m.key.name.clone(), flat(m.value.as_ref()?)))).collect()),
            _ => flat(self),
        }
    }
}

/// A key as written: its decoded name, its span (quotes included), and its quote character.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    pub name: String,
    pub span: Span,
    /// `Some('"')` / `Some('\'')` for a quoted key, `None` for a bare identifier.
    pub quote: Option<char>,
}

/// `key: value,` — the value is `None` while it's being typed (`{ cut }`, `{ cutoff: }`).
#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    pub key: Key,
    pub colon: Option<Span>,
    pub value: Option<Node>,
    pub comma: Option<Span>,
}

impl Member {
    /// From the key through the value (or as far as it got).
    pub fn span(&self) -> Span {
        let end = self.value.as_ref().map(|v| v.span.end).or(self.colon.map(|c| c.end)).unwrap_or(self.key.span.end);
        Span::new(self.key.span.start, end)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub node: Node,
    pub comma: Option<Span>,
}

/// A parsed doc: every token, the tree (absent only for a doc with no value at all), and the syntax
/// errors met on the way.
#[derive(Clone, Debug)]
pub struct Cst {
    pub tokens: Vec<Token>,
    pub root: Option<Node>,
    pub errors: Vec<SyntaxError>,
}

impl Cst {
    /// Lex and parse `src`, recovering from errors.
    pub fn parse(src: &str) -> Cst {
        let (tokens, mut errors) = lex::lex(src);
        let (root, parse_errors) = parse::parse(src, &tokens);
        errors.extend(parse_errors);
        errors.sort_by_key(|e| (e.span.start, e.span.end));
        Cst { tokens, root, errors }
    }

    pub fn is_ok(&self) -> bool {
        self.errors.is_empty() && self.root.is_some()
    }

    /// The value of an error-free doc (`None` with syntax errors) — what `json5::from_str` returns.
    pub fn to_value(&self) -> Option<serde_json::Value> {
        if !self.errors.is_empty() {
            return None;
        }
        self.root.as_ref().map(value::node_value)
    }

    /// The same, as the engine's doc tree: a `-Infinity` stays a number (ADR 0054 P3). What the
    /// language server checks.
    pub fn to_doc_value(&self) -> Option<engines_core::doc_value::DocValue> {
        if !self.errors.is_empty() {
            return None;
        }
        self.root.as_ref().map(value::node_doc_value)
    }

    /// The token whose span contains `offset` (or ends at it, preferring a non-trivia token there).
    pub fn token_at(&self, offset: usize) -> Option<(usize, &Token)> {
        let i = self.tokens.partition_point(|t| t.span.end < offset);
        let here = self.tokens.get(i).filter(|t| t.span.touches(offset));
        let next = self.tokens.get(i + 1).filter(|t| t.span.start == offset);
        match (here, next) {
            (Some(h), Some(n)) if h.kind.is_trivia() && !n.kind.is_trivia() => Some((i + 1, n)),
            (Some(h), _) => Some((i, h)),
            (None, _) => None,
        }
    }
}

/// The keys of the top-level `section` object (`instruments`, `buses`, …) in the order the doc's text
/// writes them — the authored order a `BTreeMap`-backed `LoopDoc` loses (a repeated key keeps its
/// first place). Empty when the text has no such object; recovers from syntax errors like the CST.
pub fn member_order(text: &str, section: &str) -> Vec<String> {
    let cst = Cst::parse(text);
    let mut out: Vec<String> = Vec::new();
    for m in cst.root.as_ref().and_then(|r| r.get(section)).map(|n| n.members()).unwrap_or_default() {
        if !out.contains(&m.key.name) {
            out.push(m.key.name.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests;
