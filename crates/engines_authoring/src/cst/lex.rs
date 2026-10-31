//! The JSON5 lexer: every byte of the source lands in exactly one [`Token`] (trivia included).
//! Character classes come from `json5::char` — the engine's own parser — so the two agree on what
//! counts as whitespace, a line break or an identifier character.

use super::{Span, SyntaxError, Token, TokenKind};
use json5::char::{is_json5_identifier, is_json5_identifier_start, is_json5_line_terminator, is_json5_whitespace};

pub(super) fn lex(src: &str) -> (Vec<Token>, Vec<SyntaxError>) {
    let mut lx = Lexer { src, pos: 0, tokens: Vec::new(), errors: Vec::new() };
    while lx.pos < src.len() {
        lx.token();
    }
    (lx.tokens, lx.errors)
}

struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    tokens: Vec<Token>,
    errors: Vec<SyntaxError>,
}

impl Lexer<'_> {
    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }
    fn peek_at(&self, ahead: usize) -> Option<char> {
        self.src[self.pos..].chars().nth(ahead)
    }
    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }
    fn push(&mut self, kind: TokenKind, start: usize) {
        self.tokens.push(Token { kind, span: Span::new(start, self.pos) });
    }
    fn error(&mut self, start: usize, end: usize, message: impl Into<String>) {
        self.errors.push(SyntaxError { span: Span::new(start, end), message: message.into() });
    }

    fn token(&mut self) {
        let start = self.pos;
        let c = self.peek().expect("not at end");
        let kind = match c {
            '{' => self.single(TokenKind::LBrace),
            '}' => self.single(TokenKind::RBrace),
            '[' => self.single(TokenKind::LBracket),
            ']' => self.single(TokenKind::RBracket),
            ':' => self.single(TokenKind::Colon),
            ',' => self.single(TokenKind::Comma),
            '"' | '\'' => self.string(c),
            '/' => self.comment(),
            '+' | '-' | '.' | '0'..='9' => self.number(),
            _ if is_json5_whitespace(c) => {
                while self.peek().is_some_and(is_json5_whitespace) {
                    self.bump();
                }
                TokenKind::Whitespace
            }
            _ if is_json5_identifier_start(c) || c == '\\' => self.word(),
            _ => {
                self.bump();
                self.error(start, self.pos, format!("unexpected character `{c}`"));
                TokenKind::Unknown
            }
        };
        self.push(kind, start);
    }

    fn single(&mut self, kind: TokenKind) -> TokenKind {
        self.bump();
        kind
    }

    /// A quoted string. Escapes are skipped here (decoded by the parser); an unescaped line break or
    /// the end of input closes it with an error.
    fn string(&mut self, quote: char) -> TokenKind {
        let start = self.pos;
        self.bump();
        loop {
            match self.peek() {
                None => {
                    self.error(start, self.pos, "unterminated string");
                    break;
                }
                Some('\n' | '\r') => {
                    self.error(start, self.pos, "unterminated string (a line break inside a string needs `\\n`)");
                    break;
                }
                Some('\\') => {
                    self.bump();
                    // A backslash-CRLF line continuation is one escape.
                    if self.bump() == Some('\r') && self.peek() == Some('\n') {
                        self.bump();
                    }
                }
                Some(c) => {
                    self.bump();
                    if c == quote {
                        break;
                    }
                }
            }
        }
        TokenKind::String
    }

    fn comment(&mut self) -> TokenKind {
        let start = self.pos;
        self.bump();
        match self.peek() {
            Some('/') => {
                while self.peek().is_some_and(|c| !is_json5_line_terminator(c)) {
                    self.bump();
                }
                TokenKind::LineComment
            }
            Some('*') => {
                self.bump();
                // Like `json5`, an unterminated block comment simply runs to the end of the input.
                while let Some(c) = self.bump() {
                    if c == '*' && self.peek() == Some('/') {
                        self.bump();
                        break;
                    }
                }
                TokenKind::BlockComment
            }
            _ => {
                self.error(start, self.pos, "unexpected `/` (comments are `// …` or `/* … */`)");
                TokenKind::Unknown
            }
        }
    }

    /// A number as `json5` reads one: a sign, then `Infinity` / `NaN` / `0x` hex / a decimal run of
    /// `[0-9.eE+-]`. Validity (leading zeros, malformed exponents) is the parser's business.
    fn number(&mut self) -> TokenKind {
        if matches!(self.peek(), Some('+' | '-')) {
            self.bump();
        }
        let rest = &self.src[self.pos..];
        if let Some(word) = ["Infinity", "NaN"].into_iter().find(|w| rest.starts_with(w)) {
            self.pos += word.len();
            return TokenKind::Number;
        }
        if self.peek() == Some('0') && matches!(self.peek_at(1), Some('x' | 'X')) {
            self.bump();
            self.bump();
            while self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                self.bump();
            }
            return TokenKind::Number;
        }
        while self.peek().is_some_and(|c| matches!(c, '+' | '-' | '.' | 'e' | 'E' | '0'..='9')) {
            self.bump();
        }
        TokenKind::Number
    }

    /// An identifier, `\uXXXX` escapes included (validated by the parser when it decodes the name).
    fn word(&mut self) -> TokenKind {
        while let Some(c) = self.peek() {
            if c == '\\' {
                self.bump();
                if self.peek() == Some('u') {
                    self.bump();
                    for _ in 0..4 {
                        if self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                            self.bump();
                        }
                    }
                }
            } else if is_json5_identifier(c) {
                self.bump();
            } else {
                break;
            }
        }
        TokenKind::Word
    }
}
