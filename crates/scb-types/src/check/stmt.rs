//! Statements.
//!
//! Each statement is checked into TIR together with whether it *diverges*: control
//! never reaches the statement after it (`return`, `throw`, `exit`, `break`,
//! `continue`, an `if` whose branches all diverge, `while (true)` without `break`).
//! Divergence drives `missing-return`, flow typing after early exits
//! (`if ($x == null) { return; }` narrows `$x` afterwards), and the join of facts
//! after branches.

use scb_syntax::ast::{self, StmtKind as S};
use scb_syntax::{Diagnostic, Span};

use crate::Ctx;
use crate::check::{Facts, LocalMeta};
use crate::tir::{self, Expr, ExprKind as T, IterKind, Stmt, StmtKind};
use crate::ty::{Ty, TyKind};

impl Ctx<'_> {
    /// A `{ … }` block in its own scope. Returns the block and whether it diverges.
    pub(crate) fn block(&mut self, b: &ast::Block) -> (tir::Block, bool) {
        self.push_scope();
        let mut stmts = Vec::new();
        let mut diverges = false;
        for s in &b.stmts {
            let (st, div) = self.stmt(s);
            stmts.push(st);
            diverges |= div;
        }
        let locals = self.pop_scope();
        (tir::Block { stmts, locals }, diverges)
    }

    fn stmt(&mut self, s: &ast::Stmt) -> (Stmt, bool) {
        let span = s.span;
        let (kind, diverges) = match &s.kind {
            S::Block(b) => {
                let (b, d) = self.block(b);
                (StmtKind::Block(b), d)
            }
            S::Local {
                readonly,
                name,
                ty,
                value,
            } => {
                let file = self.file();
                let scope = self.frame().ty_scope.clone();
                let declared = self.resolve_type(file, ty, &scope);
                self.check_value_type(declared, ty.span);
                // The value is checked before the name exists: `$x: int = $x;` is an error.
                let v = self.expr_as(value, declared);
                let meta = LocalMeta {
                    readonly: *readonly,
                    ..Self::plain_meta(declared)
                };
                let local = self.declare(&name.name, declared, name.span, meta);
                (StmtKind::Let { local, value: v }, false)
            }
            S::Const(c) => {
                let file = self.file();
                let scope = self.frame().ty_scope.clone();
                let declared = self.resolve_type(file, &c.ty, &scope);
                let v = self.expr_as(&c.value, declared);
                self.require_constant(&c.value, false);
                let meta = LocalMeta {
                    constant: true,
                    ..Self::plain_meta(declared)
                };
                let local = self.declare(&c.name.name, declared, c.name.span, meta);
                (StmtKind::Let { local, value: v }, false)
            }
            S::Destructure { pattern, value } => {
                let v = self.expr(value, None);
                let p = self.irrefutable_pattern(pattern, v.ty);
                (
                    StmtKind::Destructure {
                        pattern: p,
                        value: v,
                    },
                    false,
                )
            }
            S::If {
                cond,
                then,
                elseifs,
                otherwise,
            } => self.if_stmt(cond, then, elseifs, otherwise.as_ref()),
            S::While { cond, body } => {
                self.forget_loop_assignments(std::iter::once(cond), &body.stmts);
                self.push_scope();
                let (c, facts) = self.condition(cond);
                let saved = self.facts_snapshot();
                self.apply_facts(&facts.when_true);
                self.frame().loops.push(false);
                let (body, _) = self.block(body);
                let broke = self.frame().loops.pop().unwrap_or(false);
                let mut cond_locals = self.pop_scope();
                self.restore_facts(saved);
                if !broke {
                    self.apply_facts(&facts.when_false);
                }
                let mut body = body;
                body.locals.append(&mut cond_locals);
                let forever = is_true(cond) && !broke;
                (StmtKind::While { cond: c, body }, forever)
            }
            S::DoWhile { body, cond } => {
                self.forget_loop_assignments(std::iter::once(cond), &body.stmts);
                self.frame().loops.push(false);
                let (body, body_div) = self.block(body);
                let broke = self.frame().loops.pop().unwrap_or(false);
                let c = self.condition_value(cond);
                let forever = (is_true(cond) || body_div) && !broke;
                (StmtKind::DoWhile { body, cond: c }, forever)
            }
            S::For {
                init,
                cond,
                step,
                body,
            } => self.for_stmt(init, cond.as_ref(), step, body),
            S::Foreach {
                subject,
                key,
                value,
                body,
            } => self.foreach_stmt(subject, key.as_ref(), value, body, span),
            S::Try {
                body,
                catches,
                finally,
            } => self.try_stmt(body, catches, finally.as_ref()),
            S::Match(m) => {
                let x = self.match_expr(m, None, true);
                let diverges = x.ty == Ty::NEVER;
                let T::Match(m) = x.kind else {
                    return (
                        Stmt {
                            kind: StmtKind::Expr(x),
                            span,
                        },
                        diverges,
                    );
                };
                (StmtKind::Match(*m), diverges)
            }
            S::Echo(es) => {
                let parts = es
                    .iter()
                    .map(|e| {
                        let x = self.expr(e, None);
                        self.stringify(x)
                    })
                    .collect();
                (StmtKind::Print(parts), false)
            }
            S::Print(e) => {
                let x = self.expr(e, None);
                (StmtKind::Print(vec![self.stringify(x)]), false)
            }
            S::Exit => (
                StmtKind::Expr(Expr {
                    kind: T::Exit(None),
                    ty: Ty::NEVER,
                    span,
                }),
                true,
            ),
            S::Return(e) => (self.return_stmt(e.as_ref(), span), true),
            S::Break(n) | S::Continue(n) => {
                let is_break = matches!(s.kind, S::Break(_));
                let n = n.unwrap_or(1);
                let loops = self.frame().loops.len();
                if n == 0 || n as usize > loops {
                    let word = if is_break { "break" } else { "continue" };
                    self.report(
                        Diagnostic::new(
                            "invalid-break",
                            if loops == 0 {
                                format!("`{word}` outside a loop")
                            } else {
                                format!("`{word} {n}` but only {loops} loop(s) enclose it")
                            },
                        )
                        .with_span(span),
                    );
                } else {
                    let target = loops - n as usize;
                    if let Some(&fin) = self.frame().finally_at.last()
                        && target < fin
                    {
                        self.leaves_finally(span, if is_break { "break" } else { "continue" });
                    }
                    if is_break {
                        self.frame().loops[target] = true;
                    }
                }
                let n = n as u32;
                (
                    if is_break {
                        StmtKind::Break(n)
                    } else {
                        StmtKind::Continue(n)
                    },
                    true,
                )
            }
            S::Expr(e) => {
                let x = self.expr(e, None);
                let div = x.ty == Ty::NEVER;
                (StmtKind::Expr(x), div)
            }
        };
        (Stmt { kind, span }, diverges)
    }

    /// `void` and `never` are only return types.
    fn check_value_type(&mut self, ty: Ty, span: Span) {
        if ty == Ty::VOID || ty == Ty::NEVER {
            let shown = self.show(ty);
            self.report(
                Diagnostic::new(
                    "type-mismatch",
                    format!("`{shown}` is only valid as a return type"),
                )
                .with_span(span),
            );
        }
    }

    fn return_stmt(&mut self, e: Option<&ast::Expr>, span: Span) -> StmtKind {
        if !self.frame().finally_at.is_empty() {
            self.leaves_finally(span, "return");
        }
        let ret = self.frame().ret;
        match e {
            Some(e) => {
                if ret == Ty::VOID {
                    let x = self.expr(e, None);
                    self.report(
                        Diagnostic::new("type-mismatch", "a `void` function returns no value")
                            .with_span(x.span)
                            .with_fix("write `return;`, or declare a return type"),
                    );
                    return StmtKind::Return(Some(x));
                }
                if ret == Ty::NEVER {
                    self.report(
                        Diagnostic::new("type-mismatch", "a `never` function cannot return")
                            .with_span(span),
                    );
                }
                StmtKind::Return(Some(self.expr_as(e, ret)))
            }
            None => {
                if ret != Ty::VOID && ret != Ty::ERROR {
                    let shown = self.show(ret);
                    self.report(
                        Diagnostic::new(
                            "type-mismatch",
                            format!("`return;` in a function returning `{shown}`"),
                        )
                        .with_span(span),
                    );
                }
                StmtKind::Return(None)
            }
        }
    }

    fn leaves_finally(&mut self, span: Span, what: &str) {
        self.report(
            Diagnostic::new(
                "control-leaves-finally",
                format!("`{what}` would leave a `finally` block"),
            )
            .with_span(span)
            .with_note(
                "a `finally` block runs to its end; `throw` and `exit(…)` are the only ways out",
            )
            .with_fix("move the statement after the `try`"),
        );
    }

    fn if_stmt(
        &mut self,
        cond: &ast::Expr,
        then: &ast::Block,
        elseifs: &[(ast::Expr, ast::Block)],
        otherwise: Option<&ast::Block>,
    ) -> (StmtKind, bool) {
        // Pattern bindings in the condition (`$v instanceof Foo($x)`) are visible in
        // the `then` block only.
        self.push_scope();
        let (c, facts) = self.condition(cond);
        let before = self.facts_snapshot();
        self.apply_facts(&facts.when_true);
        let (mut then_block, then_div) = self.block(then);
        let then_out = self.facts_snapshot();
        let mut cond_locals = self.pop_scope();
        then_block.locals.append(&mut cond_locals);
        self.restore_facts(before.clone());
        self.apply_facts(&facts.when_false);

        // `elseif` chains are nested `if`s in the `else` branch.
        let (else_block, else_div) = match (elseifs.split_first(), otherwise) {
            (Some(((c2, b2), rest)), _) => {
                let (k, d) = self.if_stmt(c2, b2, rest, otherwise);
                let span = c2.span;
                (
                    Some(tir::Block {
                        stmts: vec![Stmt { kind: k, span }],
                        locals: Vec::new(),
                    }),
                    d,
                )
            }
            (None, Some(b)) => {
                let (b, d) = self.block(b);
                (Some(b), d)
            }
            (None, None) => (None, false),
        };
        let else_out = self.facts_snapshot();
        let mut outs = Vec::new();
        if !then_div {
            outs.push(then_out);
        }
        if !else_div {
            outs.push(else_out);
        }
        if outs.is_empty() {
            self.restore_facts(before);
        } else {
            self.join_facts(outs);
        }
        (
            StmtKind::If {
                cond: c,
                then: then_block,
                otherwise: else_block,
            },
            then_div && else_div,
        )
    }

    fn for_stmt(
        &mut self,
        init: &ast::ForInit,
        cond: Option<&ast::Expr>,
        step: &[ast::Expr],
        body: &ast::Block,
    ) -> (StmtKind, bool) {
        self.push_scope();
        let mut init_stmts = Vec::new();
        match init {
            ast::ForInit::None => {}
            ast::ForInit::Decl(d) => {
                let file = self.file();
                let scope = self.frame().ty_scope.clone();
                let declared = self.resolve_type(file, &d.ty, &scope);
                let v = self.expr_as(&d.value, declared);
                let local = self.declare(
                    &d.name.name,
                    declared,
                    d.name.span,
                    Self::plain_meta(declared),
                );
                init_stmts.push(Stmt {
                    span: d.name.span,
                    kind: StmtKind::Let { local, value: v },
                });
            }
            ast::ForInit::Exprs(es) => {
                for e in es {
                    let x = self.expr(e, None);
                    init_stmts.push(Stmt {
                        span: x.span,
                        kind: StmtKind::Expr(x),
                    });
                }
            }
        }
        self.forget_loop_assignments(cond.into_iter().chain(step), &body.stmts);
        let saved = self.facts_snapshot();
        let (c, facts) = match cond {
            Some(c) => {
                let (x, f) = self.condition(c);
                (Some(x), f)
            }
            None => (None, Facts::default()),
        };
        self.apply_facts(&facts.when_true);
        self.frame().loops.push(false);
        let (b, _) = self.block(body);
        let broke = self.frame().loops.pop().unwrap_or(false);
        let steps = step.iter().map(|e| self.expr(e, None)).collect();
        let scope = self.pop_scope();
        self.restore_facts(saved);
        let forever = cond.is_none_or(is_true) && !broke;
        (
            StmtKind::For {
                init: init_stmts,
                cond: c,
                step: steps,
                body: b,
                scope,
            },
            forever,
        )
    }

    fn foreach_stmt(
        &mut self,
        subject: &ast::Expr,
        key: Option<&ast::Ident>,
        value: &ast::Pattern,
        body: &ast::Block,
        span: Span,
    ) -> (StmtKind, bool) {
        let x = self.expr(subject, None);
        let (iter, key_ty, elem) = match self.types.kind(x.ty).clone() {
            TyKind::List(e) => (IterKind::List, Some(Ty::I64), e),
            TyKind::Map(k, v) => (IterKind::Map, Some(k), v),
            TyKind::Set(e) => (IterKind::Set, None, e),
            TyKind::Range(e) => (IterKind::Range, None, e),
            TyKind::Bytes => (IterKind::Bytes, Some(Ty::I64), Ty::U8),
            TyKind::Error => (IterKind::List, Some(Ty::ERROR), Ty::ERROR),
            _ => {
                let shown = self.show(x.ty);
                let mut d =
                    Diagnostic::new("type-mismatch", format!("`{shown}` cannot be iterated"))
                        .with_span(x.span);
                if x.ty == Ty::STRING {
                    d = d.with_fix(
                        "iterate a view: `$s->bytes()`, `$s->codepoints()` or `$s->graphemes()`",
                    );
                }
                self.report(d);
                (IterKind::List, Some(Ty::ERROR), Ty::ERROR)
            }
        };
        let names = crate::check::pattern::assigned_names(&body.stmts);
        self.forget_assigned(&names);
        self.push_scope();
        let key_local = key.map(|k| {
            let kt = match key_ty {
                Some(t) => t,
                None => {
                    self.report(
                        Diagnostic::new(
                            "type-mismatch",
                            format!("`{}` has no keys", self.show(x.ty)),
                        )
                        .with_span(k.span)
                        .with_fix("iterate the values only: `foreach ($set as $v)`"),
                    );
                    Ty::ERROR
                }
            };
            self.declare(&k.name, kt, k.span, Self::plain_meta(kt))
        });
        let pattern = self.irrefutable_pattern(value, elem);
        self.frame().loops.push(false);
        let (mut b, _) = self.block(body);
        self.frame().loops.pop();
        let mut locals = self.pop_scope();
        b.locals.append(&mut locals);
        let _ = span;
        (
            StmtKind::Foreach {
                subject: x,
                iter,
                key: key_local,
                value: pattern,
                body: b,
            },
            false,
        )
    }

    fn try_stmt(
        &mut self,
        body: &ast::Block,
        catches: &[ast::Catch],
        finally: Option<&ast::Block>,
    ) -> (StmtKind, bool) {
        let before = self.facts_snapshot();
        let (b, body_div) = self.block(body);
        let mut all_div = body_div;
        let mut out = Vec::new();
        let mut outs = Vec::new();
        if !body_div {
            outs.push(self.facts_snapshot());
        }
        // A `catch` or `finally` can start anywhere in the body (after any of its
        // assignments), a `finally` also anywhere in a `catch`.
        let mut assigned = crate::check::pattern::assigned_names(&body.stmts);
        let throwable = self.decls.type_named("Throwable");
        for c in catches {
            self.restore_facts(before.clone());
            self.forget_assigned(&assigned);
            self.push_scope();
            let file = self.file();
            let mut classes = Vec::new();
            let mut tys = Vec::new();
            for t in &c.types {
                let ty = self.resolve_type(
                    file,
                    &ast::Type {
                        kind: ast::TypeKind::Named(t.clone()),
                        span: t.span,
                    },
                    &crate::resolve::TyScope::default(),
                );
                match self.class_of(ty) {
                    Some(d) if throwable.is_some_and(|th| self.decls.is_subtype(d, th)) => {
                        classes.push(d);
                        tys.push(ty);
                    }
                    _ if ty == Ty::ERROR => {}
                    _ => {
                        let shown = self.show(ty);
                        self.report(
                            Diagnostic::new(
                                "type-mismatch",
                                format!("`{shown}` is not a `Throwable`"),
                            )
                            .with_span(t.span),
                        );
                    }
                }
            }
            let local = c.var.as_ref().map(|v| {
                let ty = self.types.union(&tys);
                let ty = if tys.is_empty() { Ty::ERROR } else { ty };
                self.declare(&v.name, ty, v.span, Self::plain_meta(ty))
            });
            let (mut cb, cdiv) = self.block(&c.body);
            if !cdiv {
                outs.push(self.facts_snapshot());
            }
            let mut locals = self.pop_scope();
            cb.locals.append(&mut locals);
            all_div &= cdiv;
            out.push(tir::Catch {
                local,
                classes,
                body: cb,
            });
        }
        for c in catches {
            assigned.extend(crate::check::pattern::assigned_names(&c.body.stmts));
        }
        // After the statement: what holds at the end of the body and of every
        // `catch` that can complete; a `finally` starts from what holds everywhere.
        match finally {
            Some(_) => {
                self.restore_facts(before);
                self.forget_assigned(&assigned);
            }
            None if outs.is_empty() => self.restore_facts(before),
            None => self.join_facts(outs),
        }
        let (fin, fin_div) = match finally {
            Some(f) => {
                let depth = self.frame().loops.len();
                self.frame().finally_at.push(depth);
                let (fb, d) = self.block(f);
                self.frame().finally_at.pop();
                (Some(fb), d)
            }
            None => (None, false),
        };
        (
            StmtKind::Try {
                body: b,
                catches: out,
                finally: fin,
            },
            all_div || fin_div,
        )
    }

    /// Before a loop: facts about locals the loop assigns no longer hold inside it.
    fn forget_loop_assignments<'e>(
        &mut self,
        exprs: impl Iterator<Item = &'e ast::Expr>,
        body: &[ast::Stmt],
    ) {
        let mut names = crate::check::pattern::assigned_names(body);
        for e in exprs {
            crate::check::pattern::assigned_in_expr(e, &mut names);
        }
        self.forget_assigned(&names);
    }
}

/// `true` (possibly parenthesized): `while (true)`.
fn is_true(e: &ast::Expr) -> bool {
    matches!(
        crate::check::ops::strip_parens(e).kind,
        ast::ExprKind::Bool(true)
    )
}
