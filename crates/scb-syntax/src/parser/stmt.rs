//! Statements and blocks (SPEC "Grammar sketch → Statements").

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::{Diagnostic, Keyword, Punct, Span, TokenKind};

impl Parser {
    /// `{ { Statement } }`
    pub(crate) fn block(&mut self) -> PResult<Block> {
        let start = self.expect(Punct::LBrace)?;
        let mut stmts = Vec::new();
        while !self.at(Punct::RBrace) && !self.at_eof() {
            let start = self.pos;
            match self.stmt() {
                Ok(s) => stmts.push(s),
                Err(Fail) => self.sync_stmt(start),
            }
        }
        self.expect(Punct::RBrace)?;
        Ok(Block {
            stmts,
            span: self.span_from(start),
        })
    }

    /// Tokens that begin a statement rather than a declaration (for `top-level-statement`).
    pub(crate) fn at_stmt_start(&self) -> bool {
        match self.peek() {
            TokenKind::Variable(_) => true,
            TokenKind::Keyword(k) => matches!(
                k,
                Keyword::Echo
                    | Keyword::Print
                    | Keyword::If
                    | Keyword::While
                    | Keyword::Do
                    | Keyword::For
                    | Keyword::Foreach
                    | Keyword::Try
                    | Keyword::Match
                    | Keyword::Return
                    | Keyword::Exit
                    | Keyword::Die
                    | Keyword::Throw
            ),
            TokenKind::Ident(_) | TokenKind::Punct(Punct::Backslash) => {
                matches!(self.peek_at(1), TokenKind::Punct(Punct::LParen))
                    || self.at_n(1, Punct::Backslash)
            }
            _ => false,
        }
    }

    pub(crate) fn stmt(&mut self) -> PResult<Stmt> {
        let start = self.span();
        let kind = match self.peek().clone() {
            TokenKind::Punct(Punct::LBrace) => StmtKind::Block(self.block()?),
            TokenKind::Variable(_) if self.at_n(1, Punct::Colon) => self.local(false)?,
            TokenKind::Keyword(Keyword::Readonly) => {
                self.bump();
                if !matches!(self.peek(), TokenKind::Variable(_)) {
                    return Err(self.expected("a variable declaration after `readonly`"));
                }
                self.local(true)?
            }
            TokenKind::Keyword(Keyword::Const) => {
                self.bump();
                StmtKind::Const(super::decl::const_tail(self, start)?)
            }
            TokenKind::Punct(Punct::LBracket) if self.bracket_then_assign() => {
                self.destructure()?
            }
            TokenKind::Ident(_) | TokenKind::Punct(Punct::Backslash)
                if self.struct_pattern_ahead() =>
            {
                self.destructure()?
            }
            TokenKind::Keyword(Keyword::If) => self.if_stmt()?,
            TokenKind::Keyword(Keyword::While) => {
                self.bump();
                let cond = self.paren_expr()?;
                let body = self.body("while")?;
                StmtKind::While { cond, body }
            }
            TokenKind::Keyword(Keyword::Do) => {
                self.bump();
                let body = self.body("do")?;
                self.expect_kw(Keyword::While)?;
                let cond = self.paren_expr()?;
                self.expect(Punct::Semi)?;
                StmtKind::DoWhile { body, cond }
            }
            TokenKind::Keyword(Keyword::For) => self.for_stmt()?,
            TokenKind::Keyword(Keyword::Foreach) => self.foreach_stmt()?,
            TokenKind::Keyword(Keyword::Try) => self.try_stmt()?,
            TokenKind::Keyword(Keyword::Match) => {
                let m = self.match_expr(true)?;
                self.eat(Punct::Semi);
                StmtKind::Match(m)
            }
            TokenKind::Keyword(Keyword::Echo) => {
                self.bump();
                let mut exprs = vec![self.expr()?];
                while self.eat(Punct::Comma) {
                    exprs.push(self.expr()?);
                }
                self.expect(Punct::Semi)?;
                StmtKind::Echo(exprs)
            }
            TokenKind::Keyword(Keyword::Print) => {
                self.bump();
                let e = self.expr()?;
                self.expect(Punct::Semi)?;
                StmtKind::Print(e)
            }
            TokenKind::Keyword(Keyword::Exit | Keyword::Die) if self.at_n(1, Punct::Semi) => {
                self.bump();
                self.bump();
                StmtKind::Exit
            }
            TokenKind::Keyword(Keyword::Return) => {
                self.bump();
                let value = if self.at(Punct::Semi) {
                    None
                } else {
                    Some(self.expr()?)
                };
                self.expect(Punct::Semi)?;
                StmtKind::Return(value)
            }
            TokenKind::Keyword(k @ (Keyword::Break | Keyword::Continue)) => {
                self.bump();
                let depth = match *self.peek() {
                    TokenKind::Int(n) => {
                        self.bump();
                        Some(n)
                    }
                    _ => None,
                };
                self.expect(Punct::Semi)?;
                if k == Keyword::Break {
                    StmtKind::Break(depth)
                } else {
                    StmtKind::Continue(depth)
                }
            }
            TokenKind::Keyword(Keyword::Function)
                if matches!(self.peek_at(1), TokenKind::Ident(_)) =>
            {
                let span = self.span();
                self.report(
                    Diagnostic::new("syntax-error", "functions are declared at the top level")
                        .with_span(span)
                        .with_fix("move the function out, or use a closure `fn(…): T { … }`"),
                );
                return Err(Fail);
            }
            TokenKind::Keyword(Keyword::Static)
                if matches!(self.peek_at(1), TokenKind::Variable(_)) =>
            {
                let span = self.span();
                self.removed(
                    span,
                    "`static` local variables are removed",
                    "use a static property",
                );
                return Err(Fail);
            }
            TokenKind::Ident(w) if self.removed_statement(&w) => return Err(Fail),
            _ => {
                let e = self.expr()?;
                self.expect_stmt_end()?;
                StmtKind::Expr(e)
            }
        };
        Ok(Stmt {
            kind,
            span: self.span_from(start),
        })
    }

    /// `;` after an expression statement, with a hint for PHP's `and` / `or` idioms.
    fn expect_stmt_end(&mut self) -> PResult<()> {
        if self.eat(Punct::Semi) {
            return Ok(());
        }
        Err(self.expected("`;`"))
    }

    /// PHP statements that SCB removed: `switch`, `global`, `goto`.
    fn removed_statement(&mut self, word: &str) -> bool {
        let span = self.span();
        match word {
            "switch" if self.at_n(1, Punct::LParen) => {
                self.removed(span, "`switch` is removed", "use `match`");
            }
            "global" if matches!(self.peek_at(1), TokenKind::Variable(_)) => {
                self.removed(span, "`global` is removed", "use a static property");
            }
            "goto" if matches!(self.peek_at(1), TokenKind::Ident(_)) => {
                self.removed(span, "`goto` is removed", "use structured control flow");
            }
            _ => return false,
        }
        true
    }

    /// `$x: T = expr;`
    fn local(&mut self, readonly: bool) -> PResult<StmtKind> {
        let name = self.variable("a variable")?;
        self.expect(Punct::Colon)?;
        let ty = self.ty()?;
        if !self.at(Punct::Eq) {
            let span = self.span();
            self.report(
                Diagnostic::new("syntax-error", "a local declaration needs an initializer")
                    .with_span(span)
                    .with_fix(format!("write `${}: T = value;`", name.name)),
            );
            return Err(Fail);
        }
        self.bump();
        let value = self.expr()?;
        self.expect(Punct::Semi)?;
        Ok(StmtKind::Local {
            readonly,
            name,
            ty,
            value,
        })
    }

    /// `[ … ] =` (but not `==` / `=>`) ahead: a destructuring declaration.
    fn bracket_then_assign(&self) -> bool {
        let mut depth = 0usize;
        let mut i = self.pos;
        loop {
            match &self.tokens[i].kind {
                TokenKind::Eof => return false,
                TokenKind::Punct(Punct::LBracket | Punct::LParen | Punct::LBrace) => depth += 1,
                TokenKind::Punct(Punct::RBracket | Punct::RParen | Punct::RBrace) => {
                    depth -= 1;
                    if depth == 0 {
                        return matches!(self.tokens[i + 1].kind, TokenKind::Punct(Punct::Eq));
                    }
                }
                TokenKind::Punct(Punct::Semi) => return false,
                _ => {}
            }
            i += 1;
        }
    }

    /// `Name {` or `\A\Name {` ahead: a struct pattern.
    fn struct_pattern_ahead(&self) -> bool {
        let mut i = self.pos;
        if matches!(self.tokens[i].kind, TokenKind::Punct(Punct::Backslash)) {
            i += 1;
        }
        loop {
            if !matches!(self.tokens[i].kind, TokenKind::Ident(_)) {
                return false;
            }
            i += 1;
            match self.tokens[i].kind {
                TokenKind::Punct(Punct::Backslash) => i += 1,
                TokenKind::Punct(Punct::LBrace) => return true,
                _ => return false,
            }
        }
    }

    fn destructure(&mut self) -> PResult<StmtKind> {
        let pattern = self.pattern()?;
        self.expect(Punct::Eq)?;
        let value = self.expr()?;
        self.expect(Punct::Semi)?;
        Ok(StmtKind::Destructure { pattern, value })
    }

    /// `( expr )` after `if`, `while`, ….
    fn paren_expr(&mut self) -> PResult<Expr> {
        self.expect(Punct::LParen)?;
        let e = self.expr()?;
        self.expect(Punct::RParen)?;
        Ok(e)
    }

    /// A control-flow body. Braces are mandatory (SPEC "Grammar sketch → Notes").
    fn body(&mut self, construct: &str) -> PResult<Block> {
        if self.at(Punct::LBrace) {
            return self.block();
        }
        let span = self.span();
        if self.at(Punct::Colon) {
            self.removed(
                span,
                &format!("the alternative syntax `{construct} (…): … end{construct};` is removed"),
                "use braces `{ … }`",
            );
            return Err(Fail);
        }
        self.removed(
            span,
            &format!("the body of `{construct}` needs braces"),
            "wrap the body in `{ … }`",
        );
        // Recover by taking one statement as the body.
        let stmt = self.stmt()?;
        Ok(Block {
            span: stmt.span,
            stmts: vec![stmt],
        })
    }

    fn if_stmt(&mut self) -> PResult<StmtKind> {
        self.expect_kw(Keyword::If)?;
        let cond = self.paren_expr()?;
        let then = self.body("if")?;
        let mut elseifs = Vec::new();
        let mut otherwise = None;
        loop {
            if self.eat_kw(Keyword::Elseif) {
                let c = self.paren_expr()?;
                elseifs.push((c, self.body("elseif")?));
            } else if self.at_kw(Keyword::Else) && self.at_kw_n(1, Keyword::If) {
                let span = Span::new(
                    self.file,
                    self.span().start,
                    self.tokens[self.pos + 1].span.end,
                );
                self.removed(
                    span,
                    "`else if` is not valid: braces are required after `else`",
                    "write `elseif`",
                );
                self.bump();
                self.bump();
                let c = self.paren_expr()?;
                elseifs.push((c, self.body("elseif")?));
            } else if self.eat_kw(Keyword::Else) {
                otherwise = Some(self.body("else")?);
                break;
            } else {
                break;
            }
        }
        Ok(StmtKind::If {
            cond,
            then,
            elseifs,
            otherwise,
        })
    }

    fn for_stmt(&mut self) -> PResult<StmtKind> {
        self.expect_kw(Keyword::For)?;
        self.expect(Punct::LParen)?;
        let init = if self.at(Punct::Semi) {
            ForInit::None
        } else if matches!(self.peek(), TokenKind::Variable(_)) && self.at_n(1, Punct::Colon) {
            let name = self.variable("a variable")?;
            self.bump();
            let ty = self.ty()?;
            self.expect(Punct::Eq)?;
            let value = self.expr()?;
            ForInit::Decl(Box::new(ForDecl { name, ty, value }))
        } else {
            ForInit::Exprs(self.expr_list()?)
        };
        self.expect(Punct::Semi)?;
        let cond = if self.at(Punct::Semi) {
            None
        } else {
            Some(self.expr()?)
        };
        self.expect(Punct::Semi)?;
        let step = if self.at(Punct::RParen) {
            Vec::new()
        } else {
            self.expr_list()?
        };
        self.expect(Punct::RParen)?;
        let body = self.body("for")?;
        Ok(StmtKind::For {
            init,
            cond,
            step,
            body,
        })
    }

    fn expr_list(&mut self) -> PResult<Vec<Expr>> {
        let mut list = vec![self.expr()?];
        while self.eat(Punct::Comma) {
            list.push(self.expr()?);
        }
        Ok(list)
    }

    fn foreach_stmt(&mut self) -> PResult<StmtKind> {
        self.expect_kw(Keyword::Foreach)?;
        self.expect(Punct::LParen)?;
        let subject = self.expr()?;
        self.expect_kw(Keyword::As)?;
        self.no_reference();
        let mut key = None;
        if matches!(self.peek(), TokenKind::Variable(_)) && self.at_n(1, Punct::FatArrow) {
            key = Some(self.variable("a key variable")?);
            self.bump();
            self.no_reference();
        }
        let value = self.pattern()?;
        self.expect(Punct::RParen)?;
        let body = self.body("foreach")?;
        Ok(StmtKind::Foreach {
            subject,
            key,
            value,
            body,
        })
    }

    /// Reports and skips PHP's `&` before a variable.
    pub(crate) fn no_reference(&mut self) {
        if self.at(Punct::Amp) && matches!(self.peek_at(1), TokenKind::Variable(_)) {
            let span = self.bump().span;
            self.removed(
                span,
                "`&` references are removed",
                "use `inout` parameters, or write back by key",
            );
        }
    }

    fn try_stmt(&mut self) -> PResult<StmtKind> {
        let start = self.expect_kw(Keyword::Try)?;
        let body = self.block()?;
        let mut catches = Vec::new();
        while self.at_kw(Keyword::Catch) {
            let catch_start = self.bump().span;
            self.expect(Punct::LParen)?;
            let mut var = None;
            if matches!(self.peek(), TokenKind::Variable(_)) {
                var = Some(self.variable("a variable")?);
                self.expect(Punct::Colon)?;
            }
            let mut types = vec![self.named_type()?];
            while self.eat(Punct::Pipe) {
                types.push(self.named_type()?);
            }
            if var.is_none() && matches!(self.peek(), TokenKind::Variable(_)) {
                let span = self.span();
                self.report(
                    Diagnostic::new("syntax-error", "types are written after the name")
                        .with_span(span)
                        .with_fix("write `catch ($e: Type)`"),
                );
                self.bump();
            }
            self.expect(Punct::RParen)?;
            let body = self.block()?;
            catches.push(Catch {
                var,
                types,
                body,
                span: self.span_from(catch_start),
            });
        }
        let finally = if self.eat_kw(Keyword::Finally) {
            Some(self.block()?)
        } else {
            None
        };
        if catches.is_empty() && finally.is_none() {
            self.report(
                Diagnostic::new("syntax-error", "`try` needs a `catch` or `finally`")
                    .with_span(start),
            );
        }
        Ok(StmtKind::Try {
            body,
            catches,
            finally,
        })
    }
}
