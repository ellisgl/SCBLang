//! Lexer: SPEC "Lexical structure", numeric and string literal tables.
//!
//! Produces every token it can and keeps going after errors, so one run reports
//! all lexical diagnostics of a file.

use crate::token::{Keyword, Punct, StrLit, StrPart, Token, TokenKind};
use crate::{Diagnostic, FileId, Span};

/// Lexes one file. The token list always ends with `Eof`.
pub fn lex(file: FileId, src: &str) -> (Vec<Token>, Vec<Diagnostic>) {
    let mut lx = Lexer::new(file, src);
    if src.starts_with('\u{FEFF}') {
        lx.error(0, 3, "bom", "byte order mark at start of file")
            .with_note("the formatter strips it")
            .push(&mut lx);
        lx.pos = 3;
    }
    let mut tokens = Vec::new();
    loop {
        let token = lx.next_token();
        let eof = token.kind == TokenKind::Eof;
        tokens.push(token);
        if eof {
            break;
        }
    }
    (tokens, lx.diags)
}

struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    file: FileId,
    diags: Vec<Diagnostic>,
    puncts: Vec<Punct>,
}

/// Where a string body ends.
#[derive(Clone, Copy)]
enum Term<'a> {
    /// At an unescaped `"`.
    Quote,
    /// At byte `end`; every line starts with `indent`, which is removed.
    Heredoc { end: usize, indent: &'a str },
}

/// A diagnostic under construction (lets callers add notes before pushing).
struct Pending(Diagnostic);

impl Pending {
    fn with_note(self, note: impl Into<String>) -> Self {
        Pending(self.0.with_note(note))
    }
    fn with_fix(self, fix: impl Into<String>) -> Self {
        Pending(self.0.with_fix(fix))
    }
    fn push(self, lx: &mut Lexer) {
        lx.diags.push(self.0);
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

impl<'a> Lexer<'a> {
    fn new(file: FileId, src: &'a str) -> Self {
        Lexer {
            src,
            pos: 0,
            file,
            diags: Vec::new(),
            puncts: Punct::longest_first(),
        }
    }

    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.rest().chars().nth(n)
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn span(&self, start: usize, end: usize) -> Span {
        Span::new(self.file, start as u32, end as u32)
    }

    fn error(
        &self,
        start: usize,
        end: usize,
        kind: &'static str,
        msg: impl Into<String>,
    ) -> Pending {
        Pending(Diagnostic::new(kind, msg).with_span(self.span(start, end)))
    }

    fn token(&self, kind: TokenKind, start: usize) -> Token {
        Token {
            kind,
            span: self.span(start, self.pos),
        }
    }

    fn next_token(&mut self) -> Token {
        loop {
            self.skip_trivia();
            let start = self.pos;
            let Some(c) = self.peek() else {
                return self.token(TokenKind::Eof, start);
            };
            if let Some(token) = self.lex_one(c, start) {
                return token;
            }
        }
    }

    /// Whitespace and non-doc comments.
    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(' ' | '\t' | '\n' | '\r') => {
                    self.bump();
                }
                Some('/') if self.peek_at(1) == Some('/') => self.skip_line(),
                Some('/') if self.peek_at(1) == Some('*') && !self.at_doc_comment() => {
                    let start = self.pos;
                    match self.rest()[2..].find("*/") {
                        Some(i) => self.pos += 2 + i + 2,
                        None => {
                            self.pos = self.src.len();
                            self.error(
                                start,
                                start + 2,
                                "unterminated-comment",
                                "`/*` comment is never closed",
                            )
                            .push(self);
                        }
                    }
                }
                Some('#') if self.peek_at(1) != Some('[') => {
                    let start = self.pos;
                    self.skip_line();
                    self.error(start, start + 1, "hash-comment", "`#` comments are removed")
                        .with_fix("use `//`")
                        .push(self);
                }
                _ => return,
            }
        }
    }

    fn skip_line(&mut self) {
        match self.rest().find('\n') {
            Some(i) => self.pos += i,
            None => self.pos = self.src.len(),
        }
    }

    /// `/**` starts a doc comment unless it is the empty block comment `/**/`.
    fn at_doc_comment(&self) -> bool {
        self.rest().starts_with("/**") && !self.rest().starts_with("/**/")
    }

    /// Lexes one token starting at `c`; `None` after an error with nothing to return.
    fn lex_one(&mut self, c: char, start: usize) -> Option<Token> {
        if c == '/' && self.at_doc_comment() {
            return Some(self.doc_comment(start));
        }
        if c == 'b' && self.peek_at(1) == Some('"') {
            self.pos += 2;
            let bytes = self.string_body(Term::Quote, true, start);
            let bytes = text_of(bytes);
            return Some(self.token(TokenKind::Bytes(bytes), start));
        }
        if is_ident_start(c) {
            let name = self.ident();
            let kind = if name == "_" {
                TokenKind::Underscore
            } else if let Some(kw) = Keyword::from_ident(name) {
                TokenKind::Keyword(kw)
            } else {
                TokenKind::Ident(name.into())
            };
            return Some(self.token(kind, start));
        }
        if c.is_ascii_digit() {
            return Some(self.number(start));
        }
        match c {
            '$' => return self.variable(start),
            '\'' => return Some(self.single_quoted(start)),
            '"' => {
                self.bump();
                let parts = self.string_body(Term::Quote, false, start);
                return Some(self.token(
                    TokenKind::Str(StrLit {
                        parts: string_parts(parts),
                    }),
                    start,
                ));
            }
            '#' => {
                self.pos += 2;
                return Some(self.token(TokenKind::AttrOpen, start));
            }
            '<' if self.rest().starts_with("<<<") => {
                if let Some(token) = self.heredoc(start) {
                    return Some(token);
                }
            }
            '.' => {
                let rest = self.rest();
                if rest.starts_with("..")
                    && !rest.starts_with("...")
                    && !rest.starts_with("..=")
                    && !rest.starts_with("..<")
                {
                    self.pos += 2;
                    self.error(
                        start,
                        self.pos,
                        "bare-range-operator",
                        "there is no bare `..`",
                    )
                    .with_fix("use `..=` (inclusive) or `..<` (half-open)")
                    .push(self);
                    return None;
                }
                if self.peek_at(1).is_some_and(|d| d.is_ascii_digit()) {
                    self.bump();
                    let digits_start = self.pos;
                    while self.peek().is_some_and(|d| d.is_ascii_digit() || d == '_') {
                        self.bump();
                    }
                    let digits = &self.src[digits_start..self.pos];
                    self.error(
                        start,
                        self.pos,
                        "invalid-number",
                        "a float needs a leading digit",
                    )
                    .with_fix(format!("write `0.{digits}`"))
                    .push(self);
                    return None;
                }
            }
            _ => {}
        }
        if let Some(p) = self
            .puncts
            .iter()
            .copied()
            .find(|p| self.rest().starts_with(p.as_str()))
        {
            self.pos += p.as_str().len();
            return Some(self.token(TokenKind::Punct(p), start));
        }
        self.bump();
        let msg = if c.is_ascii() {
            format!("unexpected character `{}`", c.escape_default())
        } else {
            format!(
                "unexpected character `{c}` (U+{:04X}); identifiers are ASCII only",
                c as u32
            )
        };
        self.error(start, self.pos, "invalid-character", msg)
            .push(self);
        None
    }

    fn ident(&mut self) -> &'a str {
        let start = self.pos;
        while self.peek().is_some_and(is_ident_char) {
            self.bump();
        }
        &self.src[start..self.pos]
    }

    fn doc_comment(&mut self, start: usize) -> Token {
        match self.rest()[3..].find("*/") {
            Some(i) => {
                let text = &self.src[start + 3..start + 3 + i];
                self.pos = start + 3 + i + 2;
                self.token(TokenKind::DocComment(text.into()), start)
            }
            None => {
                self.pos = self.src.len();
                self.error(
                    start,
                    start + 3,
                    "unterminated-comment",
                    "`/**` comment is never closed",
                )
                .push(self);
                self.token(TokenKind::DocComment(self.src[start + 3..].into()), start)
            }
        }
    }

    fn variable(&mut self, start: usize) -> Option<Token> {
        self.bump();
        if self.peek().is_some_and(is_ident_start) {
            let name = self.ident();
            return Some(self.token(TokenKind::Variable(name.into()), start));
        }
        let msg = match self.peek() {
            Some('$' | '{') => "variable variables are banned",
            _ => "`$` must be followed by a variable name",
        };
        self.error(start, self.pos, "invalid-variable", msg)
            .push(self);
        None
    }

    // ---------- numbers ----------

    fn number(&mut self, start: usize) -> Token {
        let rest = self.rest();
        let radix = match rest.as_bytes().get(..2) {
            Some(b"0x") => Some(16),
            Some(b"0o") => Some(8),
            Some(b"0b") => Some(2),
            _ => None,
        };
        let reported = self.diags.len();
        let kind = match radix {
            Some(radix) => self.radix_int(start, radix),
            None => self.decimal(start),
        };
        if self.peek().is_some_and(is_ident_char) {
            let suffix_start = self.pos;
            self.ident();
            // One diagnostic per literal: `1.e5` is reported as a trailing dot only.
            if self.diags.len() == reported {
                self.error(
                    suffix_start,
                    self.pos,
                    "invalid-number",
                    "numeric literals take no suffix",
                )
                .with_note("the declared type sets the width: `$x: u8 = 200;`")
                .push(self);
            }
        }
        self.token(kind, start)
    }

    fn radix_int(&mut self, start: usize, radix: u32) -> TokenKind {
        self.pos += 2;
        let digits_start = self.pos;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            self.bump();
        }
        let raw = &self.src[digits_start..self.pos];
        if raw.is_empty() {
            self.error(
                start,
                self.pos,
                "invalid-number",
                "missing digits after the radix prefix",
            )
            .push(self);
            return TokenKind::Int(0);
        }
        if let Some(bad) = raw.chars().find(|&c| c != '_' && !c.is_digit(radix)) {
            self.error(
                start,
                self.pos,
                "invalid-number",
                format!("`{bad}` is not a base-{radix} digit"),
            )
            .push(self);
            return TokenKind::Int(0);
        }
        self.check_separators(start, raw, |c| c.is_digit(radix));
        self.int_value(start, raw, radix)
    }

    fn decimal(&mut self, start: usize) -> TokenKind {
        self.digits();
        let int_end = self.pos;
        let mut is_float = false;
        if self.peek() == Some('.') {
            match self.peek_at(1) {
                Some(d) if d.is_ascii_digit() => {
                    is_float = true;
                    self.bump();
                    self.digits();
                }
                // `1..<3`, `1..=3`, `1...`: an int followed by a range or spread operator.
                Some('.') => {}
                _ => {
                    self.bump();
                    let text = &self.src[start..self.pos - 1];
                    self.error(
                        start,
                        self.pos,
                        "invalid-number",
                        "a float needs a digit after the `.`",
                    )
                    .with_fix(format!("write `{text}.0`"))
                    .push(self);
                    return TokenKind::Float(format!("{}.0", text.replace('_', "")).into());
                }
            }
        }
        // SPEC-OPEN: Q26 — only lowercase `e` (like the lowercase `0x`/`0o`/`0b` prefixes).
        if self.peek() == Some('e') {
            let sign = usize::from(matches!(self.peek_at(1), Some('+' | '-')));
            if self.peek_at(1 + sign).is_some_and(|d| d.is_ascii_digit()) {
                is_float = true;
                self.pos += 1 + sign;
                self.digits();
            }
        }
        let raw = &self.src[start..self.pos];
        self.check_separators(start, raw, |c| c.is_ascii_digit());
        let int_part = &self.src[start..int_end];
        let leading_zero = int_part.len() > 1 && int_part.starts_with('0');
        if is_float {
            // SPEC-OPEN: Q26 — leading zeros in float literals.
            if leading_zero {
                self.error(
                    start,
                    self.pos,
                    "invalid-number",
                    "leading zeros are not allowed",
                )
                .push(self);
            }
            return TokenKind::Float(raw.replace('_', "").into());
        }
        if leading_zero {
            let octal = raw[1..].trim_start_matches(['0', '_']);
            self.error(
                start,
                self.pos,
                "legacy-octal",
                "legacy octal literals are removed",
            )
            .with_fix(format!(
                "write `0o{}` for octal",
                if octal.is_empty() { "0" } else { octal }
            ))
            .push(self);
            return TokenKind::Int(0);
        }
        self.int_value(start, raw, 10)
    }

    fn digits(&mut self) {
        while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '_') {
            self.bump();
        }
    }

    /// `_` is allowed only between two digits.
    fn check_separators(&mut self, start: usize, raw: &str, is_digit: impl Fn(char) -> bool) {
        let chars: Vec<char> = raw.chars().collect();
        for (i, &c) in chars.iter().enumerate() {
            if c != '_' {
                continue;
            }
            let before = i.checked_sub(1).map(|j| chars[j]);
            let after = chars.get(i + 1).copied();
            if !(before.is_some_and(&is_digit) && after.is_some_and(&is_digit)) {
                self.error(
                    start,
                    self.pos,
                    "invalid-separator",
                    "`_` is allowed only between digits",
                )
                .push(self);
                return;
            }
        }
    }

    fn int_value(&mut self, start: usize, raw: &str, radix: u32) -> TokenKind {
        let digits = raw.replace('_', "");
        match u64::from_str_radix(&digits, radix) {
            Ok(v) => TokenKind::Int(v),
            Err(_) => {
                self.error(
                    start,
                    self.pos,
                    "literal-out-of-range",
                    "integer literal does not fit in 64 bits",
                )
                .push(self);
                TokenKind::Int(0)
            }
        }
    }

    // ---------- strings ----------

    fn single_quoted(&mut self, start: usize) -> Token {
        self.bump();
        let mut text = String::new();
        loop {
            match self.bump() {
                None => {
                    self.error(
                        start,
                        start + 1,
                        "unterminated-string",
                        "string is never closed",
                    )
                    .push(self);
                    break;
                }
                Some('\'') => break,
                Some('\\') if matches!(self.peek(), Some('\\' | '\'')) => {
                    text.push(self.bump().unwrap_or('\\'))
                }
                Some('\r') if self.peek() == Some('\n') => {}
                Some(c) => text.push(c),
            }
        }
        self.token(
            TokenKind::Str(StrLit {
                parts: vec![StrPart::Text(text)],
            }),
            start,
        )
    }

    /// Scans a double-quoted or heredoc body: escapes, `$name` and `{$expr}`.
    /// In bytes mode (`b"…"`) `\x80`–`\xFF` are allowed and interpolation is an error.
    fn string_body(&mut self, term: Term, bytes: bool, start: usize) -> Vec<RawPart> {
        let mut parts = Vec::new();
        let mut text: Vec<u8> = Vec::new();
        if let Term::Heredoc { indent, .. } = term {
            self.skip_indent(indent);
        }
        loop {
            if let Term::Heredoc { end, .. } = term
                && self.pos >= end
            {
                break;
            }
            let Some(c) = self.peek() else {
                if !self.unterminated_since(start) {
                    self.error(
                        start,
                        start + 1,
                        "unterminated-string",
                        "string is never closed",
                    )
                    .push(self);
                }
                break;
            };
            let at = self.pos;
            match c {
                '"' if matches!(term, Term::Quote) => {
                    self.bump();
                    break;
                }
                '\\' => {
                    self.bump();
                    self.escape(at, bytes, &mut text);
                }
                '$' if self.peek_at(1).is_some_and(is_ident_start) => {
                    self.bump();
                    let name = self.ident();
                    let span = self.span(at, self.pos);
                    if bytes {
                        self.bytes_interpolation(at);
                        text.extend_from_slice(&self.src.as_bytes()[at..self.pos]);
                        continue;
                    }
                    let rest = self.rest();
                    let follower = ["?->", "->", "["].into_iter().find(|f| rest.starts_with(f));
                    if let Some(follower) = follower {
                        self.error(at, self.pos, "ambiguous-interpolation", format!("`${name}` is followed by `{follower}`"))
                            .with_note("only `$name` interpolates without braces")
                            .with_fix(format!("write `{{${name}{follower}…}}` to interpolate the expression, or `{{${name}}}{follower}` for literal text"))
                            .push(self);
                    }
                    flush(&mut parts, &mut text);
                    parts.push(RawPart::Var {
                        name: name.into(),
                        span,
                    });
                }
                '$' if self.peek_at(1) == Some('{') => {
                    // SPEC-OPEN: Q25 — PHP's `${name}` form is rejected rather than printed literally.
                    self.pos += 2;
                    self.error(
                        at,
                        self.pos,
                        "invalid-interpolation",
                        "`${…}` interpolation is not supported",
                    )
                    .with_fix("write `{$name}`")
                    .push(self);
                    text.extend_from_slice(b"${");
                }
                '{' if self.peek_at(1) == Some('$') => {
                    self.bump();
                    if bytes {
                        // The `$name` that follows reports the error.
                        text.push(b'{');
                        continue;
                    }
                    let tokens = self.interpolated_expr(at);
                    flush(&mut parts, &mut text);
                    parts.push(RawPart::Expr {
                        tokens,
                        span: self.span(at, self.pos),
                    });
                }
                '\r' if self.peek_at(1) == Some('\n') => {
                    // SPEC-OPEN: Q23 — source CRLF inside a literal becomes LF.
                    self.bump();
                }
                '\n' => {
                    self.bump();
                    text.push(b'\n');
                    if let Term::Heredoc { end, indent } = term
                        && self.pos < end
                    {
                        self.skip_indent(indent);
                    }
                }
                _ => {
                    self.bump();
                    let mut buf = [0; 4];
                    text.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
            }
        }
        flush(&mut parts, &mut text);
        parts
    }

    /// Whether an unterminated string at or after `start` was already reported: a
    /// string left open swallows the rest of the file, so report it once.
    fn unterminated_since(&self, start: usize) -> bool {
        self.diags.iter().any(|d| {
            d.kind == "unterminated-string" && d.span.is_some_and(|s| s.start as usize >= start)
        })
    }

    fn bytes_interpolation(&mut self, at: usize) {
        // SPEC-OPEN: Q22 — interpolation in `b"…"` literals.
        self.error(
            at,
            self.pos,
            "bytes-interpolation",
            "`b\"…\"` literals do not interpolate",
        )
        .with_fix("escape the dollar sign: `\\$`")
        .push(self);
    }

    /// Lexes the tokens of `{$expr}` up to the matching `}` (consumed, not included).
    fn interpolated_expr(&mut self, open: usize) -> Vec<Token> {
        let mut tokens = Vec::new();
        let mut depth = 0usize;
        loop {
            let token = self.next_token();
            match token.kind {
                TokenKind::Eof => {
                    if !self.unterminated_since(open) {
                        self.error(
                            open,
                            open + 1,
                            "unterminated-string",
                            "`{$` interpolation is never closed",
                        )
                        .push(self);
                    }
                    tokens.push(token);
                    return tokens;
                }
                TokenKind::Punct(Punct::LBrace) => depth += 1,
                TokenKind::Punct(Punct::RBrace) if depth == 0 => {
                    tokens.push(Token {
                        kind: TokenKind::Eof,
                        span: token.span,
                    });
                    return tokens;
                }
                TokenKind::Punct(Punct::RBrace) => depth -= 1,
                _ => {}
            }
            tokens.push(token);
        }
    }

    /// After `\`: SPEC "String literals".
    fn escape(&mut self, at: usize, bytes: bool, out: &mut Vec<u8>) {
        let Some(c) = self.bump() else { return };
        let simple = match c {
            'n' => Some(b'\n'),
            't' => Some(b'\t'),
            'r' => Some(b'\r'),
            'v' => Some(0x0B),
            'e' => Some(0x1B),
            'f' => Some(0x0C),
            '\\' => Some(b'\\'),
            '$' => Some(b'$'),
            '"' => Some(b'"'),
            '0' if self.peek().is_some_and(|d| d.is_ascii_digit()) => {
                // SPEC-OPEN: Q24 — `\0` followed by a digit looks like a PHP octal escape.
                self.error(
                    at,
                    self.pos + 1,
                    "unknown-escape",
                    "octal escapes are removed",
                )
                .with_note("`\\0` is NUL; a following digit would silently change meaning from PHP")
                .with_fix("use `\\x..` or `\\u{..}`")
                .push(self);
                None
            }
            '0' => Some(0),
            'x' => {
                let hex: String = self.rest().chars().take(2).collect();
                match u8::from_str_radix(&hex, 16)
                    .ok()
                    .filter(|_| hex.len() == 2 && hex.chars().all(|h| h.is_ascii_hexdigit()))
                {
                    Some(v) => {
                        self.pos += 2;
                        if v > 0x7F && !bytes {
                            self.error(
                                at,
                                self.pos,
                                "invalid-escape",
                                format!("`\\x{hex}` is not valid in a string"),
                            )
                            .with_note(
                                "strings are UTF-8; `\\x80`–`\\xFF` are only allowed in `b\"…\"`",
                            )
                            .with_fix("use `\\u{..}` for a codepoint")
                            .push(self);
                            return;
                        }
                        Some(v)
                    }
                    None => {
                        self.error(at, self.pos, "invalid-escape", "`\\x` needs two hex digits")
                            .push(self);
                        None
                    }
                }
            }
            'u' => {
                self.unicode_escape(at, out);
                return;
            }
            other => {
                self.error(
                    at,
                    self.pos,
                    "unknown-escape",
                    format!("unknown escape `\\{other}`"),
                )
                .with_fix(format!("write `\\\\{other}` for a backslash"))
                .push(self);
                None
            }
        };
        if let Some(b) = simple {
            out.push(b);
        }
    }

    fn unicode_escape(&mut self, at: usize, out: &mut Vec<u8>) {
        let rest = self.rest();
        let close = rest.strip_prefix('{').and_then(|r| r.find('}'));
        let Some(close) = close else {
            self.error(at, self.pos, "invalid-escape", "`\\u` needs `{hex}`")
                .push(self);
            return;
        };
        let hex = &rest[1..1 + close];
        self.pos += close + 2;
        let scalar = ((1..=6).contains(&hex.len()) && hex.chars().all(|h| h.is_ascii_hexdigit()))
            .then(|| u32::from_str_radix(hex, 16).ok())
            .flatten()
            .and_then(char::from_u32);
        match scalar {
            Some(c) => {
                let mut buf = [0; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
            None => self
                .error(
                    at,
                    self.pos,
                    "invalid-escape",
                    format!("`\\u{{{hex}}}` is not a Unicode scalar value"),
                )
                .with_note("surrogates and values above 10FFFF are not allowed")
                .push(self),
        }
    }

    // ---------- heredoc / nowdoc ----------

    /// `<<<ID`, `<<<"ID"` (heredoc) or `<<<'ID'` (nowdoc), then a newline. Returns
    /// `None` (nothing consumed) when the text after `<<<` is not a heredoc opener.
    fn heredoc(&mut self, start: usize) -> Option<Token> {
        let after = &self.src[start + 3..];
        let trimmed = after.trim_start_matches([' ', '\t']);
        let quote = trimmed.chars().next().filter(|c| *c == '"' || *c == '\'');
        let name_start = &trimmed[quote.map_or(0, |_| 1)..];
        let name_len = name_start
            .find(|c: char| !is_ident_char(c))
            .unwrap_or(name_start.len());
        let name = &name_start[..name_len];
        if name.is_empty() || !name.starts_with(is_ident_start) {
            return None;
        }
        let mut header_rest = &name_start[name_len..];
        if let Some(q) = quote {
            header_rest = header_rest.strip_prefix(q)?;
        }
        let newline_len = if header_rest.starts_with("\r\n") {
            2
        } else if header_rest.starts_with('\n') {
            1
        } else {
            return None;
        };
        let body_start = self.src.len() - header_rest.len() + newline_len;
        let nowdoc = quote == Some('\'');

        // Find the closing marker: a line whose content after indentation is the
        // name, not followed by an identifier character.
        let mut line_start = body_start;
        let close = loop {
            if line_start > self.src.len() {
                break None;
            }
            let line = &self.src[line_start..];
            let content = line.trim_start_matches([' ', '\t']);
            if let Some(after_name) = content.strip_prefix(name)
                && !after_name.starts_with(is_ident_char)
            {
                break Some((line_start, line.len() - content.len()));
            }
            match line.find('\n') {
                Some(i) => line_start += i + 1,
                None => break None,
            }
        };
        let Some((close_line, indent_len)) = close else {
            self.pos = self.src.len();
            self.error(
                start,
                body_start,
                "invalid-heredoc",
                format!("closing marker `{name}` not found"),
            )
            .push(self);
            return Some(self.token(TokenKind::Str(StrLit::default()), start));
        };
        let indent = &self.src[close_line..close_line + indent_len];
        if indent.contains(' ') && indent.contains('\t') {
            self.error(
                close_line,
                close_line + indent_len,
                "invalid-heredoc",
                "closing marker indentation mixes tabs and spaces",
            )
            .push(self);
        }
        // The newline before the closing line is not part of the value.
        let body_end = if close_line == body_start {
            body_start
        } else if self.src[..close_line].ends_with("\r\n") {
            close_line - 2
        } else {
            close_line - 1
        };
        self.pos = body_start;
        let parts = if body_end == body_start {
            Vec::new()
        } else if nowdoc {
            self.nowdoc_body(body_end, indent)
        } else {
            string_parts(self.string_body(
                Term::Heredoc {
                    end: body_end,
                    indent,
                },
                false,
                start,
            ))
        };
        self.pos = close_line + indent_len + name.len();
        Some(self.token(TokenKind::Str(StrLit { parts }), start))
    }

    fn nowdoc_body(&mut self, end: usize, indent: &str) -> Vec<StrPart> {
        let mut text = String::new();
        self.skip_indent(indent);
        while self.pos < end {
            let Some(c) = self.bump() else { break };
            match c {
                '\r' if self.peek() == Some('\n') => {}
                '\n' => {
                    text.push('\n');
                    if self.pos < end {
                        self.skip_indent(indent);
                    }
                }
                c => text.push(c),
            }
        }
        vec![StrPart::Text(text)]
    }

    /// Removes the closing marker's indentation from the start of a body line.
    /// Blank lines may be shorter; other lines must carry the full indentation.
    fn skip_indent(&mut self, indent: &str) {
        if self.rest().starts_with(indent) {
            self.pos += indent.len();
            return;
        }
        let line = self.rest();
        let line_end = line.find('\n').unwrap_or(line.len());
        if line[..line_end]
            .trim_end_matches('\r')
            .trim_matches([' ', '\t'])
            .is_empty()
        {
            self.pos += line[..line_end].trim_end_matches('\r').len();
            return;
        }
        let ws = line.len() - line.trim_start_matches([' ', '\t']).len();
        self.error(
            self.pos,
            self.pos + ws,
            "invalid-heredoc",
            "line is indented less than the closing marker",
        )
        .push(self);
        self.pos += ws;
    }
}

/// String body parts before UTF-8 validation of text runs.
enum RawPart {
    Text(Vec<u8>),
    Var { name: Box<str>, span: Span },
    Expr { tokens: Vec<Token>, span: Span },
}

fn flush(parts: &mut Vec<RawPart>, text: &mut Vec<u8>) {
    if !text.is_empty() {
        parts.push(RawPart::Text(std::mem::take(text)));
    }
}

/// Text runs of a string literal are UTF-8 by construction: source text is UTF-8 and
/// escapes that could break it (`\x80`–`\xFF`) are rejected outside `b"…"`.
fn string_parts(raw: Vec<RawPart>) -> Vec<StrPart> {
    raw.into_iter()
        .map(|p| match p {
            RawPart::Text(t) => StrPart::Text(String::from_utf8_lossy(&t).into_owned()),
            RawPart::Var { name, span } => StrPart::Var { name, span },
            RawPart::Expr { tokens, span } => StrPart::Expr { tokens, span },
        })
        .collect()
}

/// The bytes of a `b"…"` literal (interpolation was rejected and kept as text).
fn text_of(raw: Vec<RawPart>) -> Vec<u8> {
    raw.into_iter()
        .flat_map(|p| match p {
            RawPart::Text(t) => t,
            RawPart::Var { .. } | RawPart::Expr { .. } => Vec::new(),
        })
        .collect()
}

#[cfg(test)]
#[path = "lexer_tests.rs"]
mod tests;
