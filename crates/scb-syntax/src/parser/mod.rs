//! Hand-written recursive-descent parser with Pratt operator parsing (SPEC "Grammar
//! sketch", "Implementation plan").
//!
//! # How it works
//!
//! The lexer hands over a flat `Vec<Token>` ending in `Eof`. The [`Parser`] walks it
//! with a cursor (`pos`); every grammar rule is a method that looks at the current
//! token (`peek`), consumes what it recognizes (`bump`, `eat`, `expect`) and returns
//! an AST node. The rules are split by topic:
//!
//! | File | Grammar |
//! | --- | --- |
//! | `decl.rs` | the file, `use`, declarations, class bodies, parameters |
//! | `stmt.rs` | blocks and statements |
//! | `expr.rs` | expressions, one function per precedence level (Pratt for binary operators) |
//! | `pattern.rs` | `match` and patterns |
//! | `types.rs` | type expressions |
//!
//! # Errors and recovery
//!
//! A rule that cannot continue reports a [`Diagnostic`] and returns `Err(Fail)`.
//! `Fail` carries no data: the diagnostic is already recorded. The error travels up
//! with `?` to the nearest loop over statements, members or declarations, which
//! skips ahead to a likely boundary (`sync_stmt`, `sync_member`, `sync_decl`) and
//! continues, so one run reports every independent syntax error.
//!
//! Some problems do not stop parsing at all: removed PHP syntax, out-of-scope
//! features and missing annotations are reported and parsing carries on as if the
//! code were valid (e.g. `===` is parsed as `==`), which avoids follow-on errors.
//!
//! # Limits
//!
//! Recursive descent uses the Rust stack, so pathological nesting (`((((…))))` or a
//! 100 000-term `1 + 1 + …` chain, whose tree is as deep as it is long) would
//! overflow it. [`Parser::nested`] caps the depth of the tree being built and reports
//! `too-deeply-nested` instead.

mod decl;
mod expr;
mod pattern;
mod stmt;
mod types;

use crate::ast::{File, Ident, QualifiedName};
use crate::{Diagnostic, FileId, Keyword, Punct, Span, Token, TokenKind};

/// Maximum depth of the syntax tree: nested expressions, statements, patterns and
/// types, plus operator and postfix chains (each link is one level). Far above what
/// real code needs, and low enough that every later recursive pass (checker,
/// interpreter, the AST's own `Drop`) stays well within a thread's stack.
const MAX_DEPTH: usize = 512;

/// Parses one file's tokens (as produced by [`crate::lex`]).
pub fn parse(file: FileId, tokens: Vec<Token>) -> (File, Vec<Diagnostic>) {
    let mut p = Parser::new(file, tokens);
    let ast = p.file();
    (ast, p.diags)
}

/// Parses a stdlib declaration stub: like [`parse`], but generics, `where` bounds and
/// bodiless functions are accepted (they are how the core library's API is written
/// down; user code gets `not-supported-in-poc` for them).
pub fn parse_stdlib(file: FileId, tokens: Vec<Token>) -> (File, Vec<Diagnostic>) {
    let mut p = Parser::new(file, tokens);
    p.stdlib = true;
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
    /// Index of the current token. Never moves past the final `Eof`.
    pos: usize,
    file: FileId,
    pub(crate) diags: Vec<Diagnostic>,
    /// Current tree depth (see [`Parser::nested`]).
    pub(crate) depth: usize,
    /// Whether `too-deeply-nested` was reported (once per file is enough).
    pub(crate) too_deep: bool,
    /// Stdlib stub mode (see [`parse_stdlib`]).
    pub(crate) stdlib: bool,
    /// Tokens rewritten by [`Parser::eat_close_angle`], with their original value,
    /// so a backtracking caller can restore them (see [`Parser::restore`]).
    splits: Vec<(usize, Token)>,
}

/// A cursor position to backtrack to: see [`Parser::checkpoint`].
pub(crate) struct Checkpoint {
    pos: usize,
    diags: usize,
    splits: usize,
    too_deep: bool,
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
            depth: 0,
            too_deep: false,
            stdlib: false,
            splits: Vec::new(),
        }
    }

    /// A parser for the tokens of a `{$expr}` interpolation, starting at this
    /// parser's depth so nesting inside strings counts toward the same limit.
    pub(crate) fn sub_parser(&self, tokens: Vec<Token>) -> Parser {
        let mut sub = Parser::new(self.file, tokens);
        sub.depth = self.depth;
        sub.too_deep = self.too_deep;
        sub.stdlib = self.stdlib;
        sub
    }

    /// Runs `rule` one level deeper in the tree, or reports `too-deeply-nested`.
    pub(crate) fn nested<T>(&mut self, rule: impl FnOnce(&mut Self) -> PResult<T>) -> PResult<T> {
        if !self.descend() {
            return Err(Fail);
        }
        let result = rule(self);
        self.depth -= 1;
        result
    }

    /// Enters one tree level; `false` (after reporting) when the limit is reached.
    /// Every successful call must be paired with `self.depth -= 1`.
    pub(crate) fn descend(&mut self) -> bool {
        if self.depth >= MAX_DEPTH {
            if !self.too_deep {
                self.too_deep = true;
                let span = self.span();
                self.report(
                    Diagnostic::new(
                        "too-deeply-nested",
                        format!("code is nested more than {MAX_DEPTH} levels deep"),
                    )
                    .with_span(span)
                    .with_note("long operator chains count one level per operator")
                    .with_fix("split the expression or block into smaller pieces"),
                );
            }
            return false;
        }
        self.depth += 1;
        true
    }

    // ---------- backtracking ----------

    /// Records the cursor so a speculative parse can be undone with [`Parser::restore`].
    pub(crate) fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            pos: self.pos,
            diags: self.diags.len(),
            splits: self.splits.len(),
            too_deep: self.too_deep,
        }
    }

    /// Rewinds to a checkpoint: cursor, diagnostics and any split `>>` tokens.
    pub(crate) fn restore(&mut self, cp: Checkpoint) {
        while self.splits.len() > cp.splits {
            let (i, original) = self.splits.pop().expect("length checked");
            self.tokens[i] = original;
        }
        self.pos = cp.pos;
        self.diags.truncate(cp.diags);
        self.too_deep = cp.too_deep;
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

    /// Consumes the current token and returns its span. At `Eof` the cursor stays put,
    /// so a rule that keeps bumping cannot run off the end.
    pub(crate) fn bump(&mut self) -> Span {
        let span = self.tokens[self.pos].span;
        if !matches!(self.tokens[self.pos].kind, TokenKind::Eof) {
            self.pos += 1;
        }
        span
    }

    /// The current token (cloned, for the few rules that need its value after bumping).
    pub(crate) fn current(&self) -> Token {
        self.tokens[self.pos].clone()
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
            Ok(self.bump())
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
            Ok(self.bump())
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

    /// An identifier; keywords are rejected (declaration names, `use` aliases, …).
    pub(crate) fn ident(&mut self, what: &str) -> PResult<Ident> {
        match self.peek() {
            TokenKind::Ident(name) => {
                let name = name.clone();
                let span = self.bump();
                Ok(Ident { name, span })
            }
            &TokenKind::Keyword(k) => {
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
        match self.peek() {
            TokenKind::Ident(name) => {
                let name = name.clone();
                let span = self.bump();
                Ok(Ident { name, span })
            }
            &TokenKind::Keyword(k) => {
                let span = self.bump();
                Ok(Ident {
                    name: k.as_str().into(),
                    span,
                })
            }
            _ => Err(self.expected(what)),
        }
    }

    /// A `$variable`; the name is stored without the `$`.
    pub(crate) fn variable(&mut self, what: &str) -> PResult<Ident> {
        match self.peek() {
            TokenKind::Variable(name) => {
                let name = name.clone();
                let span = self.bump();
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
    ///
    /// The lexer is context-free, so `list<list<int>>` arrives with a single `>>`
    /// token. In a type, that token is rewritten in place to the remainder (`>`), as
    /// if the first `>` had been consumed. The rewrite is logged in `splits` so that
    /// [`Parser::restore`] can undo it.
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
        self.splits.push((self.pos, self.tokens[self.pos].clone()));
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

    /// Reports PHP syntax that SCB removed (`removed-syntax`), with a fix-it.
    pub(crate) fn removed(&mut self, span: Span, message: &str, fix: &str) {
        self.report(
            Diagnostic::new("removed-syntax", message)
                .with_span(span)
                .with_fix(fix),
        );
    }

    /// Reports syntax of a feature outside POC scope (`not-supported-in-poc`).
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

    /// Skips to the next token that can start a top-level declaration. Brace groups
    /// are skipped whole, so a broken function body is passed over in one go.
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
