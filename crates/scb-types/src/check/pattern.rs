//! Conditions, `instanceof`, `match`, patterns and exhaustiveness (SPEC "Enums and
//! match", "Type system").
//!
//! # Conditions and facts
//!
//! `condition(e)` checks a `bool` expression and returns the facts its outcomes
//! establish about locals: `$x != null` makes `$x` non-null when true and `null` when
//! false; `$x instanceof T` makes it `T`; `!` swaps; `&&` combines the true facts and
//! `||` the false ones. An assignment in the condition counts as its variable
//! (`($line = readLine()) != null`).
//!
//! # Exhaustiveness
//!
//! Each unguarded arm *covers* part of the subject's type: everything (`default`,
//! `_`, a binding), a whole type (`$v: int`), an enum case whose payload patterns
//! cover everything, `true` / `false`, or `null`. A `match` is exhaustive when the
//! covers account for every member of the subject's type (every case of an enum,
//! both `bool` values, every union member). Guarded arms never count. Anything else
//! needs `default`.

use scb_syntax::ast::{self, ExprKind as A, PatternKind as P};
use scb_syntax::{Diagnostic, Span};

use crate::Ctx;
use crate::check::Facts;
use crate::check::rel::Iface;
use crate::decls::{DeclId, TypeKind};
use crate::tir::{self, Expr, ExprKind as T, Lit, LocalId, Pattern, PatternKind as TP};
use crate::ty::{Ty, TyKind};

/// What an unguarded pattern covers, for exhaustiveness.
#[derive(Debug, Clone, PartialEq)]
enum Cover {
    All,
    Type(Ty),
    Case(DeclId, u32),
    Bool(bool),
    Null,
    Partial,
}

/// Where a pattern appears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// A `match` arm or nested inside one: anything goes.
    Match,
    /// After `instanceof`: a type test or refutable pattern.
    InstanceOf,
    /// Destructuring and `foreach`: must always match.
    Irrefutable,
}

impl Ctx<'_> {
    /// A `bool` condition and the facts it establishes.
    pub(crate) fn condition(&mut self, e: &ast::Expr) -> (Expr, Facts) {
        match &e.kind {
            A::Paren(inner) => {
                let (x, f) = self.condition(inner);
                (Expr { span: e.span, ..x }, f)
            }
            A::Unary {
                op: ast::UnaryOp::Not,
                expr,
            } => {
                let (x, f) = self.condition(expr);
                let x = self.require_bool(x);
                (
                    Expr {
                        kind: T::Unary {
                            op: tir::UnaryOp::Not,
                            expr: Box::new(x),
                        },
                        ty: Ty::BOOL,
                        span: e.span,
                    },
                    f.swap(),
                )
            }
            A::Binary {
                op: op @ (ast::BinaryOp::And | ast::BinaryOp::Or),
                lhs,
                rhs,
            } => self.logic(*op == ast::BinaryOp::And, lhs, rhs, e.span),
            A::Binary {
                op: op @ (ast::BinaryOp::Eq | ast::BinaryOp::NotEq),
                lhs,
                rhs,
            } => {
                let x = self.binary(*op, lhs, rhs, None, e.span);
                let facts = self.null_check_facts(*op == ast::BinaryOp::NotEq, lhs, rhs);
                (x, facts)
            }
            A::InstanceOf { expr, pattern } => self.instanceof(expr, pattern, e.span),
            _ => (self.condition_value(e), Facts::default()),
        }
    }

    fn require_bool(&mut self, x: Expr) -> Expr {
        if x.ty != Ty::BOOL && x.ty != Ty::ERROR && x.ty != Ty::NEVER {
            let shown = self.show(x.ty);
            self.report(
                Diagnostic::new(
                    "non-bool-condition",
                    format!("`!` needs a `bool`, found `{shown}`"),
                )
                .with_span(x.span),
            );
        }
        x
    }

    /// Facts from `$x == null` / `$x != null` (either side, or an assignment).
    fn null_check_facts(&mut self, not_eq: bool, lhs: &ast::Expr, rhs: &ast::Expr) -> Facts {
        let other = if is_null(rhs) {
            lhs
        } else if is_null(lhs) {
            rhs
        } else {
            return Facts::default();
        };
        let Some(id) = self.fact_local(other) else {
            return Facts::default();
        };
        let declared = self.frame().meta[id.0 as usize].declared;
        let current = self.local_ty(id);
        let base = if self.types.is_nullable(current) {
            current
        } else {
            declared
        };
        let non_null = self.types.strip_null(base);
        let facts = Facts {
            when_true: vec![(id, non_null)],
            when_false: vec![(id, Ty::NULL)],
        };
        if not_eq { facts } else { facts.swap() }
    }

    /// The local a condition operand talks about: `$x`, `($x = …)`.
    fn fact_local(&mut self, e: &ast::Expr) -> Option<LocalId> {
        match &e.kind {
            A::Var(name) => {
                let id = self.lookup_local(name)?;
                // Captured copies and `$this` can be narrowed too: they never change.
                Some(id)
            }
            A::Paren(x) => self.fact_local(x),
            A::Assign {
                op: None, target, ..
            } => self.fact_local(target),
            _ => None,
        }
    }

    /// `expr instanceof Pattern`: a type test (`Foo`, `string`, `list<int>`) or a
    /// refutable pattern (`Shape::CIRCLE($r)`); bindings are declared in the current
    /// scope, which the caller limits to the true branch.
    pub(crate) fn instanceof(
        &mut self,
        subject: &ast::Expr,
        pattern: &ast::Pattern,
        span: Span,
    ) -> (Expr, Facts) {
        let x = self.expr(subject, None);
        let (p, cover) = self.pattern(pattern, x.ty, Mode::InstanceOf, true);
        let mut facts = Facts::default();
        if let Some(id) = self.fact_local(subject) {
            let current = self.local_ty(id);
            match cover {
                Cover::Type(t) => {
                    facts.when_true.push((id, t));
                    if self.types.members(current).contains(&t) {
                        let rest = self.types.without(current, &[t]);
                        facts.when_false.push((id, rest));
                    }
                }
                Cover::Null => facts.when_false.push((id, self.types.strip_null(current))),
                _ => {}
            }
        }
        (
            Expr {
                kind: T::InstanceOf {
                    expr: Box::new(x),
                    pattern: Box::new(p),
                },
                ty: Ty::BOOL,
                span,
            },
            facts,
        )
    }

    /// A destructuring or `foreach` pattern: bindings are declared in the current
    /// scope and the pattern must always match (`refutable-pattern` otherwise).
    pub(crate) fn irrefutable_pattern(&mut self, p: &ast::Pattern, subject: Ty) -> Pattern {
        self.pattern(p, subject, Mode::Irrefutable, true).0
    }

    // ---------- match ----------

    pub(crate) fn match_expr(&mut self, m: &ast::Match, expect: Option<Ty>, is_stmt: bool) -> Expr {
        let subject = m.subject.as_ref().map(|s| self.expr(s, None));
        let subject_ty = subject.as_ref().map_or(Ty::BOOL, |s| s.ty);
        let subject_local = m.subject.as_ref().and_then(|s| self.fact_local(s));
        let mut arms = Vec::new();
        let mut covers = Vec::new();
        let mut has_default = false;
        let mut bodies_div = true;
        let before = self.facts_snapshot();
        for arm in &m.arms {
            self.restore_facts(before.clone());
            self.push_scope();
            let mut patterns = Vec::new();
            let mut condition = None;
            let mut arm_covers = Vec::new();
            match &arm.test {
                ast::ArmTest::Default => {
                    has_default = true;
                    arm_covers.push(Cover::All);
                }
                ast::ArmTest::Patterns(ps) => {
                    let binds = ps.len() == 1;
                    for p in ps {
                        let (tp, c) = self.pattern(p, subject_ty, Mode::Match, binds);
                        patterns.push(tp);
                        arm_covers.push(c);
                    }
                    // A whole-type arm narrows the subject variable inside the arm.
                    if let (Some(id), [Cover::Type(t)]) = (subject_local, arm_covers.as_slice()) {
                        self.frame().facts.insert(id, *t);
                    }
                }
                ast::ArmTest::Conditions(cs) => {
                    // `a, b => …` in a subjectless match: either condition.
                    let mut cond: Option<Expr> = None;
                    for c in cs {
                        let x = self.condition_value(c);
                        cond = Some(match cond {
                            None => x,
                            Some(prev) => {
                                let span = prev.span;
                                Expr {
                                    kind: T::Logic {
                                        and: false,
                                        lhs: Box::new(prev),
                                        rhs: Box::new(x),
                                    },
                                    ty: Ty::BOOL,
                                    span,
                                }
                            }
                        });
                    }
                    condition = cond;
                }
            }
            let guard = arm.guard.as_ref().map(|g| {
                let (x, f) = self.condition(g);
                self.apply_facts(&f.when_true);
                x
            });
            if guard.is_none() {
                covers.extend(arm_covers);
            }
            let body = match &arm.body {
                ast::ArmBody::Expr(e) => {
                    let x = if is_stmt {
                        self.expr(e, None)
                    } else {
                        self.expr(e, expect)
                    };
                    bodies_div &= x.ty == Ty::NEVER;
                    tir::ArmBody::Expr(x)
                }
                ast::ArmBody::Block(b) => {
                    let (b, d) = self.block(b);
                    bodies_div &= d;
                    tir::ArmBody::Block(b)
                }
            };
            let locals = self.pop_scope();
            arms.push(tir::Arm {
                patterns,
                condition,
                guard,
                body,
                locals,
            });
        }
        self.restore_facts(before);

        // Exhaustiveness.
        let exhaustive =
            has_default || (m.subject.is_some() && self.covers_type(subject_ty, &covers));
        if !exhaustive {
            let missing = if m.subject.is_none() {
                "a subjectless `match` needs `default`".to_string()
            } else {
                self.missing_cases(subject_ty, &covers)
            };
            self.report(
                Diagnostic::new(
                    "non-exhaustive-match",
                    format!("`match` is not exhaustive: {missing}"),
                )
                .with_span(m.span)
                .with_note("arms with an `if` guard do not count toward exhaustiveness")
                .with_fix("add the missing arms, or `default => …`"),
            );
        }

        let ty = if is_stmt {
            if bodies_div && exhaustive {
                Ty::NEVER
            } else {
                Ty::VOID
            }
        } else {
            let exprs: Vec<&Expr> = arms
                .iter()
                .filter_map(|a| match &a.body {
                    tir::ArmBody::Expr(e) => Some(e),
                    tir::ArmBody::Block(_) => None,
                })
                .collect();
            let ty = self.branch_type(&exprs, expect, m.span);
            for a in &mut arms {
                if let tir::ArmBody::Expr(e) = &mut a.body {
                    let x = std::mem::replace(e, Self::error_expr(m.span));
                    *e = self.coerce(x, ty);
                }
            }
            ty
        };
        Expr {
            kind: T::Match(Box::new(tir::Match { subject, arms })),
            ty,
            span: m.span,
        }
    }

    /// Whether the covers account for every value of `ty`.
    fn covers_type(&mut self, ty: Ty, covers: &[Cover]) -> bool {
        if covers.contains(&Cover::All) || covers.contains(&Cover::Type(ty)) || ty == Ty::ERROR {
            return true;
        }
        self.types
            .members(ty)
            .into_iter()
            .all(|m| self.member_covered(m, covers))
    }

    fn member_covered(&mut self, m: Ty, covers: &[Cover]) -> bool {
        if covers.contains(&Cover::Type(m)) {
            return true;
        }
        match self.types.kind(m).clone() {
            TyKind::Null => covers.contains(&Cover::Null),
            TyKind::Bool => {
                covers.contains(&Cover::Bool(true)) && covers.contains(&Cover::Bool(false))
            }
            TyKind::Named(d, _) if self.decls.ty(d).kind == TypeKind::Enum => {
                let n = self.decls.ty(d).cases.len() as u32;
                (0..n).all(|c| covers.contains(&Cover::Case(d, c)))
            }
            _ => false,
        }
    }

    fn missing_cases(&mut self, ty: Ty, covers: &[Cover]) -> String {
        let mut missing = Vec::new();
        for m in self.types.members(ty) {
            if self.member_covered(m, covers) {
                continue;
            }
            match self.types.kind(m).clone() {
                TyKind::Bool => {
                    for b in [true, false] {
                        if !covers.contains(&Cover::Bool(b)) {
                            missing.push(format!("`{b}`"));
                        }
                    }
                }
                TyKind::Named(d, _) if self.decls.ty(d).kind == TypeKind::Enum => {
                    let short = self.short_name(d);
                    for (i, c) in self.decls.ty(d).cases.iter().enumerate() {
                        if !covers.contains(&Cover::Case(d, i as u32)) {
                            missing.push(format!("`{short}::{}`", c.name));
                        }
                    }
                }
                _ => missing.push(format!("`{}`", self.show(m))),
            }
        }
        if missing.is_empty() {
            format!("values of `{}` are not all covered", self.show(ty))
        } else {
            format!("{} not covered", missing.join(", "))
        }
    }

    // ---------- patterns ----------

    /// Checks a pattern against a subject type. `binds` is false where bindings are
    /// not allowed (alternatives `a, b =>`).
    fn pattern(
        &mut self,
        p: &ast::Pattern,
        subject: Ty,
        mode: Mode,
        binds: bool,
    ) -> (Pattern, Cover) {
        let span = p.span;
        let (kind, cover) = match &p.kind {
            P::Wildcard => (TP::Wildcard, Cover::All),
            P::Binding(name) => {
                if mode == Mode::InstanceOf {
                    self.report(
                        Diagnostic::new(
                            "syntax-error",
                            "`instanceof` needs a type or pattern, not a bare variable",
                        )
                        .with_span(span),
                    );
                }
                let id = self.bind(name, subject, binds);
                (TP::Bind(id), Cover::All)
            }
            P::Typed { name, ty } => {
                let file = self.file();
                let scope = self.frame().ty_scope.clone();
                let t = self.resolve_type(file, ty, &scope);
                self.check_type_test(t, subject, ty.span);
                let bind = name.as_ref().map(|n| self.bind(n, t, binds));
                (TP::TypeTest { ty: t, bind }, Cover::Type(t))
            }
            P::Literal(e) => self.literal_pattern(e, subject),
            P::Equals(e) => {
                let target = self.types_strip(subject);
                let x = self.expr(e, Some(target));
                if x.ty != Ty::ERROR
                    && !self.assignable(x.ty, subject)
                    && !self.assignable(x.ty, target)
                {
                    self.type_mismatch(x.span, subject, x.ty);
                }
                (TP::Equals(x), Cover::Partial)
            }
            P::Range {
                start,
                end,
                inclusive,
            } => {
                let int = self.types_strip(subject);
                let s = self.expr(start, Some(int));
                let e = self.expr(end, Some(int));
                let (Some(_), T::Lit(Lit::Int(a)), T::Lit(Lit::Int(b))) =
                    (self.types.as_int(int), &s.kind, &e.kind)
                else {
                    if subject != Ty::ERROR && s.ty != Ty::ERROR {
                        let shown = self.show(subject);
                        self.report(
                            Diagnostic::new(
                                "type-mismatch",
                                format!("range patterns match integers, not `{shown}`"),
                            )
                            .with_span(span),
                        );
                    }
                    return (
                        Pattern {
                            kind: TP::Wildcard,
                            span,
                        },
                        Cover::Partial,
                    );
                };
                (
                    TP::Range {
                        start: *a,
                        end: *b,
                        inclusive: *inclusive,
                    },
                    Cover::Partial,
                )
            }
            P::Path {
                name,
                member,
                payload,
            } => self.path_pattern(
                name,
                member.as_ref(),
                payload.as_deref(),
                subject,
                mode,
                binds,
                span,
            ),
            P::Type(t) => {
                let file = self.file();
                let scope = self.frame().ty_scope.clone();
                let ty = self.resolve_type(file, t, &scope);
                self.check_type_test(ty, subject, span);
                (TP::TypeTest { ty, bind: None }, Cover::Type(ty))
            }
            P::Struct { ty, fields } => self.struct_pattern(ty, fields, subject, mode, binds, span),
            P::List(items) => self.tuple_pattern(items, subject, mode, binds, span),
        };
        if mode == Mode::Irrefutable
            && !matches!(cover, Cover::All)
            && !self.full_cover(&cover, subject)
        {
            self.report(
                Diagnostic::new("refutable-pattern", "this pattern may not match")
                    .with_span(span)
                    .with_note("destructuring and `foreach` need patterns that always match")
                    .with_fix("use `match`, or `if ($x instanceof …)`"),
            );
        }
        (Pattern { kind, span }, cover)
    }

    fn full_cover(&mut self, cover: &Cover, subject: Ty) -> bool {
        match cover {
            Cover::All => true,
            Cover::Type(t) => *t == subject,
            _ => false,
        }
    }

    fn types_strip(&mut self, t: Ty) -> Ty {
        self.types.strip_null(t)
    }

    /// Declares a pattern binding (SPEC: bindings always declare new variables).
    fn bind(&mut self, name: &ast::Ident, ty: Ty, allowed: bool) -> LocalId {
        if !allowed {
            self.report(
                Diagnostic::new(
                    "syntax-error",
                    "an arm with several patterns cannot bind variables",
                )
                .with_span(name.span)
                .with_fix("split the arm, one pattern per arm"),
            );
        }
        self.declare(&name.name, ty, name.span, Self::plain_meta(ty))
    }

    /// A type test `$v: T` / `instanceof T`: `T` must be able to match the subject.
    fn check_type_test(&mut self, t: Ty, subject: Ty, span: Span) {
        if t == Ty::ERROR || subject == Ty::ERROR {
            return;
        }
        let possible = self.types.members(subject).into_iter().any(|m| {
            m == t
                || self.assignable(t, m)
                || matches!((self.class_of(t), self.class_of(m)), (Some(a), Some(b)) if self.decls.is_subtype(b, a))
        });
        if !possible {
            let (ts, ss) = (self.show(t), self.show(subject));
            self.report(
                Diagnostic::new("type-mismatch", format!("a `{ss}` is never a `{ts}`"))
                    .with_span(span),
            );
        }
    }

    fn literal_pattern(&mut self, e: &ast::Expr, subject: Ty) -> (TP, Cover) {
        let inner = self.types.strip_null(subject);
        let x = self.expr(e, Some(inner));
        let cover = match &x.kind {
            T::Lit(Lit::Null) => {
                if !self.types.is_nullable(subject) && subject != Ty::ERROR {
                    let shown = self.show(subject);
                    self.report(
                        Diagnostic::new("type-mismatch", format!("`{shown}` is never `null`"))
                            .with_span(x.span),
                    );
                }
                Cover::Null
            }
            T::Lit(Lit::Bool(b)) => Cover::Bool(*b),
            _ => Cover::Partial,
        };
        if x.ty != Ty::NULL && x.ty != Ty::ERROR && !self.assignable(x.ty, subject) {
            self.type_mismatch(x.span, subject, x.ty);
        }
        (TP::Equals(x), cover)
    }

    #[allow(clippy::too_many_arguments)]
    fn path_pattern(
        &mut self,
        name: &ast::QualifiedName,
        member: Option<&ast::Ident>,
        payload: Option<&[ast::Pattern]>,
        subject: Ty,
        mode: Mode,
        binds: bool,
        span: Span,
    ) -> (TP, Cover) {
        let file = self.file();
        let Some(member) = member else {
            // `MAX` (a constant) or `Foo` / `string` (a type test).
            if self.lookup_const(file, name).is_some() {
                let e = ast::Expr {
                    kind: A::Name(name.clone()),
                    span,
                };
                let x = self.expr(&e, Some(subject));
                return (TP::Equals(x), Cover::Partial);
            }
            let t = ast::Type {
                kind: ast::TypeKind::Named(ast::NamedType {
                    name: name.clone(),
                    args: Vec::new(),
                    span,
                }),
                span,
            };
            let scope = self.frame().ty_scope.clone();
            let ty = self.resolve_type(file, &t, &scope);
            self.check_type_test(ty, subject, span);
            return (TP::TypeTest { ty, bind: None }, Cover::Type(ty));
        };
        // `X::NAME`, `X::CASE(…)`.
        let class_expr =
            if name.parts.len() == 1 && &*name.parts[0].name == "self" && !name.absolute {
                ast::Expr {
                    kind: A::SelfRef,
                    span: name.span,
                }
            } else {
                ast::Expr {
                    kind: A::Name(name.clone()),
                    span: name.span,
                }
            };
        let Some(decl) = self.static_class(&class_expr) else {
            return (TP::Wildcard, Cover::Partial);
        };
        if let Some(ci) = self
            .decls
            .ty(decl)
            .cases
            .iter()
            .position(|c| *c.name == *member.name)
        {
            let case = self.decls.ty(decl).cases[ci].clone();
            let enum_ty = self.types.named(decl, Vec::new());
            if subject != Ty::ERROR && !self.types.members(subject).contains(&enum_ty) {
                let shown = self.show(subject);
                self.report(
                    Diagnostic::new(
                        "type-mismatch",
                        format!("a `{shown}` is never a `{}`", self.short_name(decl)),
                    )
                    .with_span(span),
                );
            }
            let mut sub = Vec::new();
            let mut full = true;
            match (&case.payload, payload) {
                (Some(params), Some(ps)) => {
                    if params.len() != ps.len() {
                        self.report(
                            Diagnostic::new(
                                "wrong-arguments",
                                format!(
                                    "`{}` has {} payload field(s), the pattern has {}",
                                    case.name,
                                    params.len(),
                                    ps.len()
                                ),
                            )
                            .with_span(span),
                        );
                    }
                    for (pp, param) in ps.iter().zip(params) {
                        let sub_mode = if mode == Mode::Irrefutable {
                            Mode::Match
                        } else {
                            mode
                        };
                        let (tp, c) = self.pattern(pp, param.ty, sub_mode, binds);
                        full &= c == Cover::All || c == Cover::Type(param.ty);
                        sub.push(tp);
                    }
                }
                (Some(_), None) => {
                    self.report(
                        Diagnostic::new(
                            "missing-case-arguments",
                            format!(
                                "`{}` carries data: match it with `{}(…)`",
                                case.name, case.name
                            ),
                        )
                        .with_span(span)
                        .with_fix(format!(
                            "write `{}::{}(_)` with one pattern per payload field",
                            self.short_name(decl),
                            case.name
                        )),
                    );
                    full = false;
                }
                (None, Some(_)) => {
                    self.report(
                        Diagnostic::new("type-mismatch", format!("`{}` has no payload", case.name))
                            .with_span(span),
                    );
                    full = false;
                }
                (None, None) => {}
            }
            let cover = if full {
                Cover::Case(decl, ci as u32)
            } else {
                Cover::Partial
            };
            let cover = if full && self.decls.ty(decl).cases.len() == 1 {
                Cover::Type(enum_ty)
            } else {
                cover
            };
            return (
                TP::Case {
                    decl,
                    case: ci as u32,
                    payload: sub,
                },
                cover,
            );
        }
        if payload.is_some() {
            self.report(
                Diagnostic::new(
                    "unknown-member",
                    format!("`{}` has no case `{}`", self.short_name(decl), member.name),
                )
                .with_span(member.span),
            );
            return (TP::Wildcard, Cover::Partial);
        }
        // A class constant compared with `==`.
        let e = ast::Expr {
            kind: A::StaticMember {
                class: Box::new(class_expr),
                name: member.clone(),
            },
            span,
        };
        let x = self.expr(&e, Some(subject));
        (TP::Equals(x), Cover::Partial)
    }

    fn struct_pattern(
        &mut self,
        ty: &ast::NamedType,
        fields: &[(ast::Ident, ast::Pattern)],
        subject: Ty,
        mode: Mode,
        binds: bool,
        span: Span,
    ) -> (TP, Cover) {
        let file = self.file();
        let scope = self.frame().ty_scope.clone();
        let t = self.resolve_type(
            file,
            &ast::Type {
                kind: ast::TypeKind::Named(ty.clone()),
                span: ty.span,
            },
            &scope,
        );
        let Some(decl) = self.class_of(t) else {
            return (TP::Wildcard, Cover::Partial);
        };
        self.check_type_test(t, subject, span);
        let mut out = Vec::new();
        let mut full = true;
        for (fname, fp) in fields {
            let Some(idx) = self.decls.field_index(decl, &fname.name) else {
                self.report(
                    Diagnostic::new(
                        "unknown-member",
                        format!(
                            "`{}` has no property `{}`",
                            self.short_name(decl),
                            fname.name
                        ),
                    )
                    .with_span(fname.span),
                );
                full = false;
                continue;
            };
            let f = self.decls.ty(decl).fields[idx as usize].clone();
            self.check_visibility(f.owner, f.visibility, &format!("${}", f.name), fname.span);
            let sub_mode = if mode == Mode::Irrefutable {
                Mode::Match
            } else {
                mode
            };
            let (tp, c) = self.pattern(fp, f.ty, sub_mode, binds);
            full &= c == Cover::All || c == Cover::Type(f.ty);
            out.push((idx, tp));
        }
        let cover = if full { Cover::Type(t) } else { Cover::Partial };
        (TP::Struct { decl, fields: out }, cover)
    }

    fn tuple_pattern(
        &mut self,
        items: &[ast::Pattern],
        subject: Ty,
        mode: Mode,
        binds: bool,
        span: Span,
    ) -> (TP, Cover) {
        let elems: Vec<Ty> = match self.types.kind(subject).clone() {
            TyKind::Tuple(es) => {
                if es.len() != items.len() {
                    self.report(
                        Diagnostic::new(
                            "type-mismatch",
                            format!(
                                "`{}` has {} elements, the pattern has {}",
                                self.show(subject),
                                es.len(),
                                items.len()
                            ),
                        )
                        .with_span(span),
                    );
                }
                es
            }
            TyKind::List(e) if mode != Mode::Irrefutable => vec![e; items.len()],
            TyKind::Error => vec![Ty::ERROR; items.len()],
            _ => {
                let shown = self.show(subject);
                let mut d = Diagnostic::new(
                    "type-mismatch",
                    format!("`[…]` destructures a tuple, not `{shown}`"),
                )
                .with_span(span);
                if matches!(self.types.kind(subject), TyKind::List(_)) {
                    d = d
                        .with_note("a list may be shorter than the pattern")
                        .with_fix("index it: `$a = $list[0];`, or use `match`");
                }
                self.report(d);
                vec![Ty::ERROR; items.len()]
            }
        };
        let mut out = Vec::new();
        // After a shape error (`elems` all errors), claim coverage: no `refutable-pattern`.
        let mut full = matches!(self.types.kind(subject), TyKind::Tuple(_))
            || elems.iter().all(|&e| e == Ty::ERROR);
        for (p, &t) in items.iter().zip(&elems) {
            let sub_mode = if mode == Mode::Irrefutable {
                Mode::Match
            } else {
                mode
            };
            let (tp, c) = self.pattern(p, t, sub_mode, binds);
            full &= c == Cover::All || c == Cover::Type(t);
            out.push(tp);
        }
        let cover = if full { Cover::All } else { Cover::Partial };
        (TP::Tuple(out), cover)
    }

    /// Whether `ty` is `Comparable` (used by range-like checks elsewhere).
    #[allow(dead_code)]
    pub(crate) fn comparable(&mut self, ty: Ty) -> bool {
        self.conforms(ty, Iface::Comparable)
    }
}

fn is_null(e: &ast::Expr) -> bool {
    matches!(crate::check::ops::strip_parens(e).kind, A::Null)
}

/// Names of variables assigned (not declared) anywhere in `stmts`, for dropping flow
/// facts before loops.
pub(crate) fn assigned_names(stmts: &[ast::Stmt]) -> Vec<String> {
    let mut out = Vec::new();
    for s in stmts {
        for e in crate::validate::stmt_exprs(s) {
            assigned_in_expr(e, &mut out);
        }
    }
    out
}

pub(crate) fn assigned_in_expr(e: &ast::Expr, out: &mut Vec<String>) {
    match &e.kind {
        A::Assign { target, .. } => {
            if let Some(n) = root_var(target) {
                out.push(n);
            }
        }
        A::Unary {
            op:
                ast::UnaryOp::PreInc
                | ast::UnaryOp::PreDec
                | ast::UnaryOp::PostInc
                | ast::UnaryOp::PostDec,
            expr,
        } => {
            if let Some(n) = root_var(expr) {
                out.push(n);
            }
        }
        A::Call { args, .. } => {
            for a in args {
                if let ast::ArgKind::Inout(x) = &a.kind
                    && let Some(n) = root_var(x)
                {
                    out.push(n);
                }
            }
        }
        _ => {}
    }
    for c in crate::validate::children(e) {
        assigned_in_expr(c, out);
    }
}

fn root_var(e: &ast::Expr) -> Option<String> {
    match &e.kind {
        A::Var(n) => Some(n.to_string()),
        A::Prop { object, .. } | A::Index { object, .. } | A::Paren(object) => root_var(object),
        _ => None,
    }
}
