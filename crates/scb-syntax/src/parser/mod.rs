//! Hand-written recursive-descent parser with Pratt operator parsing (SPEC "Grammar
//! sketch", "Implementation plan").
//!
//! Errors are reported as diagnostics and the parser recovers at statement, member and
//! declaration boundaries, so one run reports every syntax error it can.

mod decl;
mod expr;
mod pattern;
mod stmt;
mod types;

use crate::ast::{File, Ident, QualifiedName};
use crate::{Diagnostic, FileId, Keyword, Punct, Span, Token, TokenKind};

/// Parses one file's tokens (as produced by [`crate::lex`]).
pub fn parse(file: FileId, tokens: Vec<Token>) -> (File, Vec<Diagnostic>) {
    let mut p = Parser::new(file, tokens);
    let ast = p.file();
    (ast, p.diags)
}

/// Lexes and parses one file. Lexical errors are returned without parsing, so a bad
/// token does not cascade into syntax errors.
pub fn parse_source(file: FileId, src: &str) -> (Option<File>, Vec<Diagnostic>) {
    let (tokens, diags) = crate::lex(file, src);
    if !diags.is_empty() {
        return (None, diags);
    }
    let (ast, diags) = parse(file, tokens);
    (Some(ast), diags)
}

/// A syntax error has been reported; the caller recovers.
#[derive(Debug)]
pub(crate) struct Fail;

pub(crate) type PResult<T> = Result<T, Fail>;

pub(crate) struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    file: FileId,
    pub(crate) diags: Vec<Diagnostic>,
}

impl Parser {
    pub(crate) fn new(file: FileId, tokens: Vec<Token>) -> Self {
        // Doc comments are not attached to anything in the POC.
        let mut tokens: Vec<Token> = tokens
            .into_iter()
            .filter(|t| !matches!(t.kind, TokenKind::DocComment(_)))
            .collect();
        if !matches!(
            tokens.last(),
            Some(Token {
                kind: TokenKind::Eof,
                ..
            })
        ) {
            let end = tokens.last().map_or(0, |t| t.span.end);
            tokens.push(Token {
                kind: TokenKind::Eof,
                span: Span::new(file, end, end),
            });
        }
        Parser {
            tokens,
            pos: 0,
            file,
            diags: Vec::new(),
        }
    }

    // ---------- cursor ----------

    pub(crate) fn peek(&self) -> &TokenKind {
        &self.tokens[self.pos].kind
    }

    pub(crate) fn peek_at(&self, n: usize) -> &TokenKind {
        let i = (self.pos + n).min(self.tokens.len() - 1);
        &self.tokens[i].kind
    }

    pub(crate) fn span(&self) -> Span {
        self.tokens[self.pos].span
    }

    /// Span of the last consumed token.
    pub(crate) fn prev_span(&self) -> Span {
        self.tokens[self.pos.saturating_sub(1)].span
    }

    pub(crate) fn span_from(&self, start: Span) -> Span {
        let end = self.prev_span().end.max(start.start);
        Span::new(self.file, start.start, end)
    }

    pub(crate) fn bump(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
        if !matches!(t.kind, TokenKind::Eof) {
            self.pos += 1;
        }
        t
    }

    pub(crate) fn at_eof(&self) -> bool {
        matches!(self.peek(), TokenKind::Eof)
    }

    pub(crate) fn at(&self, p: Punct) -> bool {
        matches!(self.peek(), TokenKind::Punct(q) if *q == p)
    }

    pub(crate) fn at_n(&self, n: usize, p: Punct) -> bool {
        matches!(self.peek_at(n), TokenKind::Punct(q) if *q == p)
    }

    pub(crate) fn eat(&mut self, p: Punct) -> bool {
        if self.at(p) {
            self.bump();
            true
        } else {
            false
        }
    }

    pub(crate) fn expect(&mut self, p: Punct) -> PResult<Span> {
        if self.at(p) {
            Ok(self.bump().span)
        } else {
            Err(self.expected(&format!("`{}`", p.as_str())))
        }
    }

    pub(crate) fn at_kw(&self, k: Keyword) -> bool {
        matches!(self.peek(), TokenKind::Keyword(q) if *q == k)
    }

    pub(crate) fn at_kw_n(&self, n: usize, k: Keyword) -> bool {
        matches!(self.peek_at(n), TokenKind::Keyword(q) if *q == k)
    }

    pub(crate) fn eat_kw(&mut self, k: Keyword) -> bool {
        if self.at_kw(k) {
            self.bump();
            true
        } else {
            false
        }
    }

    pub(crate) fn expect_kw(&mut self, k: Keyword) -> PResult<Span> {
        if self.at_kw(k) {
            Ok(self.bump().span)
        } else {
            Err(self.expected(&format!("`{}`", k.as_str())))
        }
    }

    /// An identifier token with this exact text (contextual keywords, removed words).
    pub(crate) fn at_word(&self, word: &str) -> bool {
        matches!(self.peek(), TokenKind::Ident(s) if &**s == word)
    }

    pub(crate) fn at_word_n(&self, n: usize, word: &str) -> bool {
        matches!(self.peek_at(n), TokenKind::Ident(s) if &**s == word)
    }

    pub(crate) fn ident(&mut self, what: &str) -> PResult<Ident> {
        match self.peek().clone() {
            TokenKind::Ident(name) => {
                let span = self.bump().span;
                Ok(Ident { name, span })
            }
            TokenKind::Keyword(k) => {
                let span = self.span();
                self.report(
                    Diagnostic::new(
                        "syntax-error",
                        format!(
                            "`{}` is a reserved keyword and cannot be used as {what}",
                            k.as_str()
                        ),
                    )
                    .with_span(span),
                );
                Err(Fail)
            }
            _ => Err(self.expected(what)),
        }
    }

    /// A member name or argument label: identifiers and keywords (SPEC "Lexical structure").
    pub(crate) fn name(&mut self, what: &str) -> PResult<Ident> {
        match self.peek().clone() {
            TokenKind::Ident(name) => {
                let span = self.bump().span;
                Ok(Ident { name, span })
            }
            TokenKind::Keyword(k) => {
                let span = self.bump().span;
                Ok(Ident {
                    name: k.as_str().into(),
                    span,
                })
            }
            _ => Err(self.expected(what)),
        }
    }

    pub(crate) fn variable(&mut self, what: &str) -> PResult<Ident> {
        match self.peek().clone() {
            TokenKind::Variable(name) => {
                let span = self.bump().span;
                Ok(Ident { name, span })
            }
            _ => Err(self.expected(what)),
        }
    }

    /// `[\] Ident { \ Ident }`. The leading `\` is allowed only when `absolute_ok`.
    pub(crate) fn qualified_name(&mut self, absolute_ok: bool) -> PResult<QualifiedName> {
        let start = self.span();
        let absolute = self.at(Punct::Backslash);
        if absolute {
            if !absolute_ok {
                self.report(
                    Diagnostic::new("syntax-error", "names here are always fully qualified")
                        .with_span(start)
                        .with_fix("remove the leading `\\`"),
                );
            }
            self.bump();
        }
        // SPEC "Lexical structure" (Q28): after a `\` the position is unambiguous,
        // so keywords are allowed (`\Io\print`).
        let mut parts = vec![if absolute {
            self.name("a name")?
        } else {
            self.ident("a name")?
        }];
        while self.at(Punct::Backslash)
            && matches!(self.peek_at(1), TokenKind::Ident(_) | TokenKind::Keyword(_))
        {
            self.bump();
            parts.push(self.name("a name")?);
        }
        Ok(QualifiedName {
            absolute,
            parts,
            span: self.span_from(start),
        })
    }

    /// Consumes one `>` closing a generic argument list, splitting `>>`, `>=` and `>>=`.
    pub(crate) fn eat_close_angle(&mut self) -> bool {
        let rest = match self.peek() {
            TokenKind::Punct(Punct::Gt) => {
                self.bump();
                return true;
            }
            TokenKind::Punct(Punct::Shr) => Punct::Gt,
            TokenKind::Punct(Punct::Ge) => Punct::Eq,
            TokenKind::Punct(Punct::ShrEq) => Punct::Ge,
            _ => return false,
        };
        let tok = &mut self.tokens[self.pos];
        tok.kind = TokenKind::Punct(rest);
        tok.span.start += 1;
        true
    }

    // ---------- diagnostics ----------

    pub(crate) fn report(&mut self, d: Diagnostic) {
        self.diags.push(d);
    }

    /// Reports "expected X, found Y" at the current token.
    pub(crate) fn expected(&mut self, what: &str) -> Fail {
        let found = describe(self.peek());
        let span = self.span();
        self.report(
            Diagnostic::new("syntax-error", format!("expected {what}, found {found}"))
                .with_span(span),
        );
        Fail
    }

    pub(crate) fn removed(&mut self, span: Span, message: &str, fix: &str) {
        self.report(
            Diagnostic::new("removed-syntax", message)
                .with_span(span)
                .with_fix(fix),
        );
    }

    pub(crate) fn not_in_poc(&mut self, span: Span, feature: &str) {
        self.report(
            Diagnostic::new(
                "not-supported-in-poc",
                format!("not supported in the POC: {feature}"),
            )
            .with_span(span)
            .with_note("SPEC \"Implementation plan → POC scope\""),
        );
    }

    // ---------- recovery ----------

    /// Brackets opened and not yet closed between token `start` and the cursor.
    fn open_brackets(&self, start: usize) -> usize {
        let mut depth = 0usize;
        for t in &self.tokens[start..self.pos] {
            match t.kind {
                TokenKind::Punct(Punct::LBrace | Punct::LParen | Punct::LBracket) => depth += 1,
                TokenKind::Punct(Punct::RBrace | Punct::RParen | Punct::RBracket) => {
                    depth = depth.saturating_sub(1)
                }
                _ => {}
            }
        }
        depth
    }

    /// Skips to the end of a broken statement that began at token `start`: closes the
    /// brackets it opened, then stops past a `;`, after a closing `}` that ends it, or
    /// before the `}` of the enclosing block.
    pub(crate) fn sync_stmt(&mut self, start: usize) {
        let mut depth = self.open_brackets(start);
        let from = self.pos;
        loop {
            match self.peek() {
                TokenKind::Eof => return,
                TokenKind::Punct(Punct::LBrace | Punct::LParen | Punct::LBracket) => depth += 1,
                TokenKind::Punct(p @ (Punct::RBrace | Punct::RParen | Punct::RBracket)) => {
                    let brace = *p == Punct::RBrace;
                    if depth == 0 {
                        // A stray `)` or `]` is skipped; a `}` closes the enclosing block.
                        if !brace {
                            self.bump();
                            continue;
                        }
                        if self.pos == from && self.pos == start {
                            self.bump();
                        }
                        return;
                    }
                    depth -= 1;
                    if depth == 0 && brace {
                        self.bump();
                        // A block just closed: the statement ends here unless `;` follows.
                        self.eat(Punct::Semi);
                        return;
                    }
                }
                TokenKind::Punct(Punct::Semi) if depth == 0 => {
                    self.bump();
                    return;
                }
                _ => {}
            }
            self.bump();
        }
    }

    /// Skips to the next token that can start a top-level declaration.
    pub(crate) fn sync_decl(&mut self) {
        let start = self.pos;
        let mut depth = 0usize;
        loop {
            match self.peek() {
                TokenKind::Eof => return,
                TokenKind::Punct(Punct::LBrace) => depth += 1,
                TokenKind::Punct(Punct::RBrace) => {
                    depth = depth.saturating_sub(1);
                    self.bump();
                    if depth == 0 {
                        return;
                    }
                    continue;
                }
                _ if depth == 0 && self.pos != start && self.at_decl_start() => return,
                _ => {}
            }
            self.bump();
        }
    }

    pub(crate) fn file(&mut self) -> File {
        decl::file(self)
    }
}

/// Human-readable description of a token for "expected X, found Y".
pub(crate) fn describe(kind: &TokenKind) -> String {
    match kind {
        TokenKind::Ident(s) => format!("`{s}`"),
        TokenKind::Keyword(k) => format!("keyword `{}`", k.as_str()),
        TokenKind::Variable(v) => format!("`${v}`"),
        TokenKind::Underscore => "`_`".into(),
        TokenKind::Int(_) | TokenKind::Float(_) => "a number".into(),
        TokenKind::Str(_) => "a string".into(),
        TokenKind::Bytes(_) => "a bytes literal".into(),
        TokenKind::DocComment(_) => "a doc comment".into(),
        TokenKind::AttrOpen => "`#[`".into(),
        TokenKind::Punct(p) => format!("`{}`", p.as_str()),
        TokenKind::Eof => "end of file".into(),
    }
}

#[cfg(test)]
#[path = "../parser_tests.rs"]
mod tests;
