//! The recovering JSON5 parser: tokens → [`Node`] tree + [`SyntaxError`]s.
//!
//! Recovery rules (so a half-typed doc still has a usable tree): a key without `:` becomes a member
//! with no value; a missing value leaves `value: None`; a missing comma between two members/items is
//! reported and parsing carries on; an unclosed bracket closes at the end of input (or at a closer
//! that belongs to an outer bracket); stray tokens are reported and skipped.

use super::{Item, Key, Member, Node, NodeKind, Num, Span, SyntaxError, Token, TokenKind};
use json5::char::{is_json5_identifier, is_json5_identifier_start, is_json5_line_terminator};

pub(super) fn parse(src: &str, tokens: &[Token]) -> (Option<Node>, Vec<SyntaxError>) {
    let significant: Vec<Token> = tokens.iter().copied().filter(|t| !t.kind.is_trivia()).collect();
    let mut p = Parser { src, toks: significant, i: 0, errors: Vec::new() };
    let root = p.value();
    if root.is_none() {
        let at = p.peek().map(|t| t.span).unwrap_or(Span::new(src.len(), src.len()));
        p.error(at, "expected a value (the doc is an object: `{ … }`)");
    }
    if let Some(t) = p.peek() {
        let end = p.toks.last().map(|l| l.span.end).unwrap_or(t.span.end);
        p.error(Span::new(t.span.start, end), "unexpected content after the end of the doc");
    }
    (root, p.errors)
}

struct Parser<'a> {
    src: &'a str,
    toks: Vec<Token>,
    i: usize,
    errors: Vec<SyntaxError>,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<Token> {
        self.toks.get(self.i).copied()
    }
    fn bump(&mut self) -> Token {
        let t = self.toks[self.i];
        self.i += 1;
        t
    }
    fn error(&mut self, span: Span, message: impl Into<String>) {
        self.errors.push(SyntaxError { span, message: message.into() });
    }
    fn text(&self, t: Token) -> &'a str {
        t.span.text(self.src)
    }

    /// A value, or `None` (consuming nothing) when the next token can't start one.
    fn value(&mut self) -> Option<Node> {
        let t = self.peek()?;
        Some(match t.kind {
            TokenKind::LBrace => self.object(),
            TokenKind::LBracket => self.array(),
            TokenKind::String => {
                self.bump();
                match self.decode_string(t) {
                    Some(s) => Node { span: t.span, kind: NodeKind::String(s) },
                    None => Node { span: t.span, kind: NodeKind::Invalid },
                }
            }
            TokenKind::Number => {
                self.bump();
                match self.number(t) {
                    Some(n) => Node { span: t.span, kind: NodeKind::Number(n) },
                    None => Node { span: t.span, kind: NodeKind::Invalid },
                }
            }
            TokenKind::Word => {
                self.bump();
                let kind = match self.text(t) {
                    "true" => NodeKind::Bool(true),
                    "false" => NodeKind::Bool(false),
                    "null" => NodeKind::Null,
                    "Infinity" => NodeKind::Number(Num::Float(f64::INFINITY)),
                    "NaN" => NodeKind::Number(Num::Float(f64::NAN)),
                    w => {
                        let msg = format!("unexpected `{w}` — a string value needs quotes: \"{w}\"");
                        self.error(t.span, msg);
                        NodeKind::Invalid
                    }
                };
                Node { span: t.span, kind }
            }
            TokenKind::Unknown => {
                // The lexer already reported it; take it as an invalid value so parsing moves on.
                self.bump();
                Node { span: t.span, kind: NodeKind::Invalid }
            }
            _ => return None,
        })
    }

    fn object(&mut self) -> Node {
        let open = self.bump();
        let mut members: Vec<Member> = Vec::new();
        let end = loop {
            let Some(t) = self.peek() else {
                self.error(open.span, "unclosed `{`");
                break self.src.len();
            };
            match t.kind {
                TokenKind::RBrace => {
                    self.bump();
                    break t.span.end;
                }
                TokenKind::RBracket => {
                    self.error(t.span, "expected `}` to close the object, found `]`");
                    break members.last().map(|m| m.comma.map(|c| c.end).unwrap_or(m.span().end)).unwrap_or(open.span.end);
                }
                TokenKind::Comma => {
                    self.bump();
                    self.error(t.span, "unexpected `,`");
                }
                TokenKind::String | TokenKind::Word | TokenKind::Number => {
                    let member = self.member(t);
                    members.push(member);
                    let Some(next) = self.peek() else { continue };
                    match next.kind {
                        TokenKind::Comma => {
                            self.bump();
                            members.last_mut().expect("just pushed").comma = Some(next.span);
                        }
                        TokenKind::RBrace | TokenKind::RBracket => {}
                        TokenKind::String | TokenKind::Word => {
                            // (A key with no `:` already has its error.)
                            let last = members.last().expect("just pushed");
                            if last.colon.is_some() {
                                let at = last.span().end;
                                self.error(Span::new(at, at), "expected `,` between members");
                            }
                        }
                        _ => {
                            self.bump();
                            self.error(next.span, format!("expected `,` or `}}`, found `{}`", self.text(next)));
                        }
                    }
                }
                _ => {
                    // `{ [1] }`, `{ : 1 }` … skip whatever it is.
                    if self.value().is_none() {
                        self.bump();
                    }
                    self.error(t.span, "expected a key");
                }
            }
        };
        Node { span: Span::new(open.span.start, end), kind: NodeKind::Object(members) }
    }

    fn member(&mut self, key_tok: Token) -> Member {
        self.bump();
        let name = match key_tok.kind {
            TokenKind::String => self.decode_string(key_tok).unwrap_or_default(),
            TokenKind::Word => self.decode_identifier(key_tok).unwrap_or_default(),
            _ => {
                self.error(key_tok.span, "a key can't be a number — quote it: \"…\"");
                self.text(key_tok).to_string()
            }
        };
        let quote = match key_tok.kind {
            TokenKind::String => self.text(key_tok).chars().next(),
            _ => None,
        };
        let key = Key { name, span: key_tok.span, quote };
        let colon = match self.peek() {
            Some(t) if t.kind == TokenKind::Colon => Some(self.bump().span),
            _ => {
                self.error(key_tok.span, format!("expected `:` after the key `{}`", key.name));
                return Member { key, colon: None, value: None, comma: None };
            }
        };
        // `{ cutoff: ⏎ resonance: 0.1 }` — what follows is the next member's key, not this value.
        let next_is_key = matches!(self.peek().map(|t| t.kind), Some(TokenKind::Word | TokenKind::String))
            && self.toks.get(self.i + 1).is_some_and(|t| t.kind == TokenKind::Colon);
        let value = if next_is_key { None } else { self.value() };
        if value.is_none() {
            let at = colon.expect("set above");
            self.error(Span::new(at.end, at.end), format!("expected a value for `{}`", key.name));
        }
        Member { key, colon, value, comma: None }
    }

    fn array(&mut self) -> Node {
        let open = self.bump();
        let mut items: Vec<Item> = Vec::new();
        let end = loop {
            let Some(t) = self.peek() else {
                self.error(open.span, "unclosed `[`");
                break self.src.len();
            };
            match t.kind {
                TokenKind::RBracket => {
                    self.bump();
                    break t.span.end;
                }
                TokenKind::RBrace => {
                    self.error(t.span, "expected `]` to close the array, found `}`");
                    break items.last().map(|it| it.comma.map(|c| c.end).unwrap_or(it.node.span.end)).unwrap_or(open.span.end);
                }
                TokenKind::Comma => {
                    self.bump();
                    self.error(t.span, "unexpected `,`");
                }
                _ => match self.value() {
                    Some(node) => {
                        items.push(Item { node, comma: None });
                        let Some(next) = self.peek() else { continue };
                        match next.kind {
                            TokenKind::Comma => {
                                self.bump();
                                items.last_mut().expect("just pushed").comma = Some(next.span);
                            }
                            TokenKind::RBracket | TokenKind::RBrace => {}
                            TokenKind::Colon => {
                                self.bump();
                                self.error(next.span, "unexpected `:` in an array");
                            }
                            _ => {
                                let at = items.last().expect("just pushed").node.span.end;
                                self.error(Span::new(at, at), "expected `,` between items");
                            }
                        }
                    }
                    None => {
                        self.bump();
                        self.error(t.span, format!("unexpected `{}`", self.text(t)));
                    }
                },
            }
        };
        Node { span: Span::new(open.span.start, end), kind: NodeKind::Array(items) }
    }

    /// A number token the way `json5` reads it (`parse_number`): `Infinity`/`NaN`, hex, or a decimal
    /// through Rust's `FromStr` — `f64` when it has `.`/`e`, else `i128` (negative) / `u128`.
    fn number(&mut self, t: Token) -> Option<Num> {
        let text = self.text(t);
        let (neg, body) = match text.as_bytes().first() {
            Some(b'-') => (true, &text[1..]),
            Some(b'+') => (false, &text[1..]),
            _ => (false, text),
        };
        let bad = |p: &mut Self, why: &str| {
            p.error(t.span, format!("invalid number `{text}`{why}"));
            None
        };
        match body {
            "Infinity" => return Some(Num::Float(if neg { f64::NEG_INFINITY } else { f64::INFINITY })),
            "NaN" => return Some(Num::Float(if neg { -f64::NAN } else { f64::NAN })),
            "" => return bad(self, ""),
            _ => {}
        }
        if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
            let Ok(n) = u128::from_str_radix(hex, 16) else { return bad(self, "") };
            if !neg {
                return self.in_range(t, Num::UInt(n));
            }
            return match i128::try_from(n) {
                Ok(i) => self.in_range(t, Num::Int(-i)),
                Err(_) => {
                    let n = if n == 1 << 127 { Num::Int(i128::MIN) } else { return bad(self, " (out of range)") };
                    self.in_range(t, n)
                }
            };
        }
        let digits = body.as_bytes();
        if digits[0] == b'0' && digits.get(1).is_some_and(u8::is_ascii_digit) {
            return bad(self, " (no leading zeros)");
        }
        if !matches!(digits[0], b'.' | b'0'..=b'9') {
            return bad(self, "");
        }
        let n = if body.contains(['.', 'e', 'E']) {
            text.parse::<f64>().ok().map(Num::Float)
        } else if neg {
            text.parse::<i128>().ok().map(Num::Int)
        } else {
            text.parse::<u128>().ok().map(Num::UInt)
        };
        match n {
            None => bad(self, ""),
            Some(n) => self.in_range(t, n),
        }
    }

    /// serde_json (without `arbitrary_precision`) holds integers as `u64`/`i64`; a bigger one fails the
    /// engine's parse too — write it with a decimal point.
    fn in_range(&mut self, t: Token, n: Num) -> Option<Num> {
        let ok = match n {
            Num::UInt(u) => u64::try_from(u).is_ok(),
            Num::Int(i) => i64::try_from(i).is_ok(),
            Num::Float(_) => true,
        };
        if ok {
            Some(n)
        } else {
            self.error(t.span, format!("integer `{}` is out of range (write it as a float: `{}.0`)", self.text(t), self.text(t)));
            None
        }
    }

    /// Decode a string token's escapes (ECMAScript 5.1 §7.8.4, as `json5` does).
    fn decode_string(&mut self, t: Token) -> Option<String> {
        let raw = self.text(t);
        let quote = raw.chars().next()?;
        let closed = raw.len() >= 2 && raw.ends_with(quote) && !ends_with_escape(&raw[1..raw.len() - 1]);
        if !closed {
            return None; // the lexer reported it
        }
        let body = &raw[1..raw.len() - 1];
        let mut out = String::with_capacity(body.len());
        let mut chars = body.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            let at = t.span.start + 1 + i;
            let Some((_, e)) = chars.next() else { break };
            match e {
                'b' => out.push('\u{0008}'),
                't' => out.push('\t'),
                'n' => out.push('\n'),
                'v' => out.push('\u{000B}'),
                'f' => out.push('\u{000C}'),
                'r' => {
                    out.push('\r');
                }
                '0' => {
                    if chars.peek().is_some_and(|(_, c)| c.is_ascii_digit()) {
                        self.error(Span::new(at, at + 2), "invalid escape `\\0` followed by a digit");
                        return None;
                    }
                    out.push('\0');
                }
                '1'..='9' => {
                    self.error(Span::new(at, at + 2), format!("invalid escape `\\{e}`"));
                    return None;
                }
                'x' => {
                    let hex: String = (0..2).filter_map(|_| chars.next_if(|(_, c)| c.is_ascii_hexdigit()).map(|(_, c)| c)).collect();
                    match (hex.len() == 2).then(|| u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)).flatten() {
                        Some(c) => out.push(c),
                        None => {
                            self.error(Span::new(at, at + 2 + hex.len()), "invalid `\\x` escape (two hex digits)");
                            return None;
                        }
                    }
                }
                'u' => match unicode_escape(&mut chars) {
                    Some(c) => out.push(c),
                    None => {
                        self.error(Span::new(at, at + 2), "invalid `\\u` escape (four hex digits; astral characters as a surrogate pair)");
                        return None;
                    }
                },
                // A backslash before a line break continues the string onto the next line.
                c if is_json5_line_terminator(c) => {
                    if c == '\r' {
                        chars.next_if(|(_, c)| *c == '\n');
                    }
                }
                c => out.push(c),
            }
        }
        Some(out)
    }

    /// An identifier key, `\uXXXX` escapes decoded and checked against the identifier classes.
    fn decode_identifier(&mut self, t: Token) -> Option<String> {
        let raw = self.text(t);
        if !raw.contains('\\') {
            return Some(raw.to_string());
        }
        let mut out = String::new();
        let mut chars = raw.char_indices().peekable();
        while let Some((_, c)) = chars.next() {
            let c = if c == '\\' {
                if chars.next().map(|(_, c)| c) != Some('u') {
                    self.error(t.span, "invalid escape in a key (only `\\uXXXX`)");
                    return None;
                }
                match unicode_escape(&mut chars) {
                    Some(c) => c,
                    None => {
                        self.error(t.span, "invalid `\\u` escape in a key");
                        return None;
                    }
                }
            } else {
                c
            };
            let ok = if out.is_empty() { is_json5_identifier_start(c) } else { is_json5_identifier(c) };
            if !ok {
                self.error(t.span, "not a valid key — quote it: \"…\"");
                return None;
            }
            out.push(c);
        }
        Some(out)
    }
}

/// Whether `body` ends in an odd run of backslashes (its last quote is escaped).
fn ends_with_escape(body: &str) -> bool {
    body.bytes().rev().take_while(|&b| b == b'\\').count() % 2 == 1
}

/// The four hex digits after `\u` (plus a trailing `\uXXXX` low surrogate when the first is high).
fn unicode_escape(chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>) -> Option<char> {
    let four = |chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>| -> Option<u32> {
        let mut v = 0;
        for _ in 0..4 {
            let (_, c) = chars.next_if(|(_, c)| c.is_ascii_hexdigit())?;
            v = v * 16 + c.to_digit(16)?;
        }
        Some(v)
    };
    let a = four(chars)?;
    if let Some(c) = char::from_u32(a) {
        return Some(c);
    }
    if chars.next().map(|(_, c)| c) != Some('\\') || chars.next().map(|(_, c)| c) != Some('u') {
        return None;
    }
    let b = four(chars)?;
    let mut it = char::decode_utf16([u16::try_from(a).ok()?, u16::try_from(b).ok()?]);
    let c = it.next()?.ok()?;
    it.next().is_none().then_some(c)
}
