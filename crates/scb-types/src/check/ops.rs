//! Operators (SPEC "Numerics", "Strings", "Equality and comparison").
//!
//! Operand typing follows one rule: both operands of an arithmetic, bitwise or
//! comparison operator have the same type, with no implicit widening. A numeric
//! literal takes its type from the other operand, so the non-literal side is
//! checked first: in `$x + 1` with `$x: i8`, the `1` is an `i8`. Shift amounts and
//! integer exponents are `u32`.

use scb_syntax::ast::{self, BinaryOp as B, UnaryOp as U};
use scb_syntax::{Diagnostic, Span};

use crate::Ctx;
use crate::check::rel::Iface;
use crate::tir::{BinaryOp, Expr, ExprKind as T, Lit, UnaryOp};
use crate::ty::{Ty, TyKind};

impl Ctx<'_> {
    pub(crate) fn unary(
        &mut self,
        op: U,
        operand: &ast::Expr,
        expect: Option<Ty>,
        span: Span,
    ) -> Expr {
        match op {
            U::Not => {
                let x = self.condition_value(operand);
                Expr {
                    kind: T::Unary {
                        op: UnaryOp::Not,
                        expr: Box::new(x),
                    },
                    ty: Ty::BOOL,
                    span,
                }
            }
            U::Neg if Self::is_numeric_literal(operand) => {
                self.negative_literal(operand, expect, span)
            }
            U::Neg | U::Plus | U::BitNot => {
                let x = self.expr(operand, expect);
                let ty = x.ty;
                let ok = match op {
                    U::BitNot => self.types.as_int(ty).is_some(),
                    _ => self.types.is_numeric(ty),
                };
                if !ok && ty != Ty::ERROR {
                    let what = if op == U::BitNot {
                        "an integer"
                    } else {
                        "a number"
                    };
                    let mut d = Diagnostic::new(
                        "type-mismatch",
                        format!(
                            "`{}` needs {what}, found `{}`",
                            unary_str(op),
                            self.show(ty)
                        ),
                    )
                    .with_span(span);
                    if op == U::BitNot && ty == Ty::BOOL {
                        d = Diagnostic::new("bitwise-on-bool", "`~` on `bool`")
                            .with_span(span)
                            .with_fix("use `!`");
                    }
                    self.report(d);
                    return Self::error_expr(span);
                }
                if op == U::Neg && self.types.as_int(ty).is_some_and(|i| !i.signed()) {
                    self.report(
                        Diagnostic::new(
                            "unsigned-negation",
                            format!("unary `-` on `{}`, an unsigned type", self.show(ty)),
                        )
                        .with_span(span)
                        .with_fix(format!(
                            "write `0 - $x` (traps on underflow) or `{}::wrappingNeg($x)`",
                            self.show(ty)
                        )),
                    );
                    return Self::error_expr(span);
                }
                let op = match op {
                    U::Neg => UnaryOp::Neg,
                    U::Plus => UnaryOp::Plus,
                    _ => UnaryOp::BitNot,
                };
                Expr {
                    kind: T::Unary {
                        op,
                        expr: Box::new(x),
                    },
                    ty,
                    span,
                }
            }
            U::PreInc | U::PreDec | U::PostInc | U::PostDec => self.inc_dec(op, operand, span),
        }
    }

    /// `-5`, `-2.5`, `-(1)`: one negative literal, range-checked as such (SPEC
    /// "Numerics": `$x: i8 = -128` is valid).
    fn negative_literal(&mut self, operand: &ast::Expr, expect: Option<Ty>, span: Span) -> Expr {
        match &strip_parens(operand).kind {
            ast::ExprKind::Int(v) => self.int_lit(-i128::from(*v), span, expect),
            ast::ExprKind::Float(t) => self.float_lit(&format!("-{t}"), span, expect),
            _ => {
                // `-(-5)` and the like: negate the inner literal's value.
                let x = self.expr(operand, expect);
                let ty = x.ty;
                match x.kind {
                    T::Lit(Lit::Int(v)) => self.int_lit(-v, span, Some(ty)),
                    T::Lit(Lit::Float(f)) => Expr {
                        kind: T::Lit(Lit::Float(-f)),
                        ty,
                        span,
                    },
                    _ => Self::error_expr(span),
                }
            }
        }
    }

    /// Checks two operands that must share one type, letting a literal on either side
    /// adapt to the other. Returns the checked operands.
    pub(crate) fn same_type_operands(
        &mut self,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        expect: Option<Ty>,
    ) -> (Expr, Expr) {
        let (l_lit, r_lit) = (Self::is_numeric_literal(lhs), Self::is_numeric_literal(rhs));
        if l_lit && !r_lit {
            let r = self.expr(rhs, None);
            let l = self.expr(lhs, Some(r.ty));
            (l, r)
        } else {
            // Two literals take the context's type only if it is of their kind:
            // `$x: f64 = 7 / 2` is an integer division, reported as such.
            let hint = expect.filter(|&t| r_lit && self.literal_fits_kind(lhs, t));
            let l = self.expr(lhs, hint);
            let hint = if l.ty == Ty::ERROR {
                expect
            } else {
                Some(l.ty)
            };
            let r = self.expr(rhs, hint);
            (l, r)
        }
    }

    /// Whether a numeric literal's kind (integer or float) matches type `t`.
    fn literal_fits_kind(&self, e: &ast::Expr, t: Ty) -> bool {
        let is_float = matches!(strip_neg(e).kind, ast::ExprKind::Float(_));
        self.types.members(t).into_iter().any(|m| {
            if is_float {
                self.types.as_float(m).is_some()
            } else {
                self.types.as_int(m).is_some()
            }
        })
    }

    pub(crate) fn binary(
        &mut self,
        op: B,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        expect: Option<Ty>,
        span: Span,
    ) -> Expr {
        match op {
            B::And | B::Or => {
                let (x, _) = self.logic(op == B::And, lhs, rhs, span);
                x
            }
            B::Coalesce => self.coalesce(lhs, rhs, expect, span),
            B::Concat => {
                let l = self.expr(lhs, Some(Ty::STRING));
                let r = self.expr(rhs, Some(Ty::STRING));
                for x in [&l, &r] {
                    if x.ty != Ty::STRING && x.ty != Ty::ERROR {
                        let found = self.show(x.ty);
                        self.report(
                            Diagnostic::new(
                                "type-mismatch",
                                format!("`.` concatenates strings, found `{found}`"),
                            )
                            .with_span(x.span)
                            .with_fix("use interpolation (`\"{$x}\"`) or `(string) $x`"),
                        );
                    }
                }
                self.bin(BinaryOp::Concat, l, r, Ty::STRING, span)
            }
            B::Eq | B::NotEq => self.equality(op == B::Eq, lhs, rhs, span),
            B::Is => {
                let (l, r) = self.same_type_operands(lhs, rhs, None);
                for x in [&l, &r] {
                    if !self.is_class(x.ty) && x.ty != Ty::ERROR {
                        let found = self.show(x.ty);
                        self.report(
                            Diagnostic::new(
                                "type-mismatch",
                                format!("`is` compares class instances by identity; `{found}` is a value type"),
                            )
                            .with_span(x.span)
                            .with_fix("use `==` for structural equality"),
                        );
                        break;
                    }
                }
                self.bin(BinaryOp::Is, l, r, Ty::BOOL, span)
            }
            B::Lt | B::Le | B::Gt | B::Ge | B::Spaceship => {
                let (l, r) = self.same_type_operands(lhs, rhs, None);
                let ty = self.common(&l, &r, span);
                if ty != Ty::ERROR && !self.conforms(ty, Iface::Comparable) {
                    let found = self.show(ty);
                    self.report(
                        Diagnostic::new(
                            "unsatisfied-bound",
                            format!("`{found}` is not `Comparable`"),
                        )
                        .with_span(span),
                    );
                }
                let (bop, rty) = match op {
                    B::Lt => (BinaryOp::Lt, Ty::BOOL),
                    B::Le => (BinaryOp::Le, Ty::BOOL),
                    B::Gt => (BinaryOp::Gt, Ty::BOOL),
                    B::Ge => (BinaryOp::Ge, Ty::BOOL),
                    _ => (BinaryOp::Cmp, Ty::I64),
                };
                self.bin(bop, l, r, rty, span)
            }
            B::Shl | B::Shr => {
                let l = self.expr(lhs, expect);
                let r = self.expr_as(rhs, Ty::U32);
                if self.types.as_int(l.ty).is_none() && l.ty != Ty::ERROR {
                    let found = self.show(l.ty);
                    self.report(
                        Diagnostic::new(
                            "type-mismatch",
                            format!("shifts need an integer, found `{found}`"),
                        )
                        .with_span(l.span),
                    );
                }
                let ty = l.ty;
                let bop = if op == B::Shl {
                    BinaryOp::Shl
                } else {
                    BinaryOp::Shr
                };
                self.bin(bop, l, r, ty, span)
            }
            B::Pow => {
                let base = self.expr(lhs, expect);
                if self.types.as_int(base.ty).is_some() {
                    // Integer power: the exponent is `u32` (SPEC "Numerics").
                    let exp = self.expr_as(rhs, Ty::U32);
                    let ty = base.ty;
                    return self.bin(BinaryOp::Pow, base, exp, ty, span);
                }
                let exp = self.expr(rhs, Some(base.ty));
                let ty = self.common(&base, &exp, span);
                self.numeric_operand(ty, "**", span);
                self.bin(BinaryOp::Pow, base, exp, ty, span)
            }
            B::BitAnd | B::BitOr | B::BitXor => {
                let (l, r) = self.same_type_operands(lhs, rhs, expect);
                let ty = self.common(&l, &r, span);
                if ty == Ty::BOOL {
                    let fix = match op {
                        B::BitAnd => "use `&&`",
                        B::BitOr => "use `||`",
                        _ => "use `!=` for exclusive or",
                    };
                    self.report(
                        Diagnostic::new("bitwise-on-bool", format!("`{}` on `bool`", op.as_str()))
                            .with_span(span)
                            .with_fix(fix),
                    );
                    return Self::error_expr(span);
                }
                if ty != Ty::ERROR && self.types.as_int(ty).is_none() {
                    let found = self.show(ty);
                    self.report(
                        Diagnostic::new(
                            "type-mismatch",
                            format!("`{}` needs integers, found `{found}`", op.as_str()),
                        )
                        .with_span(span),
                    );
                }
                let bop = match op {
                    B::BitAnd => BinaryOp::BitAnd,
                    B::BitOr => BinaryOp::BitOr,
                    _ => BinaryOp::BitXor,
                };
                self.bin(bop, l, r, ty, span)
            }
            B::Add | B::Sub | B::Mul | B::Div | B::IntDiv | B::Rem => {
                let (l, r) = self.same_type_operands(lhs, rhs, expect);
                let ty = self.common(&l, &r, span);
                let bop = arith(op);
                let ty = if self.check_arith(bop, ty, span) {
                    ty
                } else {
                    Ty::ERROR
                };
                self.bin(bop, l, r, ty, span)
            }
        }
    }

    /// Operand rules shared by binary arithmetic and compound assignment. `false`
    /// after reporting an error.
    pub(crate) fn check_arith(&mut self, op: BinaryOp, ty: Ty, span: Span) -> bool {
        if ty == Ty::ERROR {
            return true;
        }
        let errors = self.diags.len();
        match op {
            BinaryOp::Div if self.types.as_int(ty).is_some() => {
                self.report(
                    Diagnostic::new(
                        "type-mismatch",
                        "`/` divides floats; integer division is `~/`",
                    )
                    .with_span(span)
                    .with_fix("use `~/` (truncates toward zero), or cast to a float type"),
                );
            }
            BinaryOp::IntDiv if self.types.as_float(ty).is_some() => {
                self.report(
                    Diagnostic::new(
                        "type-mismatch",
                        "`~/` divides integers; float division is `/`",
                    )
                    .with_span(span),
                );
            }
            _ => self.numeric_operand(ty, binop_str(op), span),
        }
        self.diags.len() == errors
    }

    fn numeric_operand(&mut self, ty: Ty, op: &str, span: Span) {
        if ty != Ty::ERROR && !self.types.is_numeric(ty) {
            let found = self.show(ty);
            let mut d = Diagnostic::new(
                "type-mismatch",
                format!("`{op}` needs numbers, found `{found}`"),
            )
            .with_span(span);
            if ty == Ty::STRING && op == "+" {
                d = d.with_fix("concatenate strings with `.`");
            }
            self.report(d);
        }
    }

    /// The single type two operands share, or `type-mismatch`.
    pub(crate) fn common(&mut self, l: &Expr, r: &Expr, span: Span) -> Ty {
        if l.ty == r.ty || r.ty == Ty::ERROR {
            return l.ty;
        }
        if l.ty == Ty::ERROR {
            return r.ty;
        }
        let msg = format!(
            "operands have different types: `{}` and `{}`",
            self.show(l.ty),
            self.show(r.ty)
        );
        let mut d = Diagnostic::new("type-mismatch", msg).with_span(span);
        if self.types.is_numeric(l.ty) && self.types.is_numeric(r.ty) {
            d = d
                .with_note("there is no implicit widening")
                .with_fix("cast one operand explicitly");
        }
        self.report(d);
        Ty::ERROR
    }

    pub(crate) fn bin(&mut self, op: BinaryOp, l: Expr, r: Expr, ty: Ty, span: Span) -> Expr {
        Expr {
            kind: T::Binary {
                op,
                lhs: Box::new(l),
                rhs: Box::new(r),
            },
            ty,
            span,
        }
    }

    /// `&&` / `||`. Returns the expression and the facts its outcomes establish: the
    /// right operand of `&&` is checked with the left one's true facts applied.
    pub(crate) fn logic(
        &mut self,
        and: bool,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        span: Span,
    ) -> (Expr, crate::check::Facts) {
        let (l, lf) = self.condition(lhs);
        let saved = self.facts_snapshot();
        self.apply_facts(if and { &lf.when_true } else { &lf.when_false });
        let (r, rf) = self.condition(rhs);
        // Afterwards the right operand may or may not have run: what holds is what
        // holds on both paths (an assignment in it drops a fact for good).
        let after_rhs = self.facts_snapshot();
        self.join_facts(vec![saved, after_rhs]);
        let facts = if and {
            crate::check::Facts {
                when_true: [lf.when_true, rf.when_true].concat(),
                when_false: Vec::new(),
            }
        } else {
            crate::check::Facts {
                when_true: Vec::new(),
                when_false: [lf.when_false, rf.when_false].concat(),
            }
        };
        let x = Expr {
            kind: T::Logic {
                and,
                lhs: Box::new(l),
                rhs: Box::new(r),
            },
            ty: Ty::BOOL,
            span,
        };
        (x, facts)
    }

    /// `$a ?? $b`: `$b` replaces `null`. With `$map[$k]` on the left the lookup does
    /// not trap (SPEC "Collections and values").
    fn coalesce(
        &mut self,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        expect: Option<Ty>,
        span: Span,
    ) -> Expr {
        let lhint = expect.map(|t| self.types.nullable(t));
        let l = self.coalesce_lhs(lhs, lhint);
        let non_null = self.types.strip_null(l.ty);
        let r = self.expr(rhs, expect.or(Some(non_null)));
        let ty = match expect {
            Some(t) if self.assignable(non_null, t) && self.assignable(r.ty, t) => t,
            _ => {
                if r.ty == non_null || r.ty == Ty::NEVER {
                    non_null
                } else {
                    self.types.union(&[non_null, r.ty])
                }
            }
        };
        Expr {
            kind: T::Coalesce {
                lhs: Box::new(l),
                rhs: Box::new(r),
            },
            ty,
            span,
        }
    }

    /// The left side of `??`: an index read becomes a non-trapping `get`.
    fn coalesce_lhs(&mut self, lhs: &ast::Expr, hint: Option<Ty>) -> Expr {
        if let ast::ExprKind::Index {
            object,
            index: Some(index),
        } = &strip_parens(lhs).kind
        {
            let obj = self.chain_base(object);
            if matches!(self.types.kind(obj.ty), TyKind::Map(..) | TyKind::List(_)) {
                return self.get_call(obj, index, lhs.span);
            }
            // Not a collection: fall through to the general rules (reports errors).
        }
        self.expr(lhs, hint)
    }

    /// `==` / `!=`: structural equality on `Equatable` types; `null` only against a
    /// nullable operand (SPEC "Equality and comparison").
    fn equality(&mut self, eq: bool, lhs: &ast::Expr, rhs: &ast::Expr, span: Span) -> Expr {
        let op = if eq { BinaryOp::Eq } else { BinaryOp::NotEq };
        let (l_null, r_null) = (is_null(lhs), is_null(rhs));
        if l_null || r_null {
            let other = if l_null { rhs } else { lhs };
            let x = self.expr(other, None);
            if !self.types.is_nullable(x.ty) && x.ty != Ty::ERROR {
                let found = self.show(x.ty);
                self.report(
                    Diagnostic::new(
                        "type-mismatch",
                        format!(
                            "comparing `{found}` with `null` is always {}",
                            if eq { "false" } else { "true" }
                        ),
                    )
                    .with_span(span)
                    .with_note("only a `?T` value can be `null`"),
                );
            }
            let null = Expr {
                kind: T::Lit(Lit::Null),
                ty: Ty::NULL,
                span,
            };
            let (l, r) = if l_null { (null, x) } else { (x, null) };
            return self.bin(op, l, r, Ty::BOOL, span);
        }
        let (l, r) = self.same_type_operands(lhs, rhs, None);
        let ty = if l.ty == r.ty || self.assignable(l.ty, r.ty) {
            r.ty
        } else if self.assignable(r.ty, l.ty) {
            l.ty
        } else {
            self.common(&l, &r, span)
        };
        let check = self.types.strip_null(ty);
        if ty != Ty::ERROR && !self.conforms(check, Iface::Equatable) {
            let found = self.show(check);
            self.report(
                Diagnostic::new("unsatisfied-bound", format!("`{found}` is not `Equatable`"))
                    .with_span(span)
                    .with_fix("implement `Equatable`, or compare identity with `is`"),
            );
        }
        self.bin(op, l, r, Ty::BOOL, span)
    }

    /// `(T) expr` for primitive `T` (SPEC "Casts and conversions").
    pub(crate) fn cast(&mut self, ty: &ast::Ident, operand: &ast::Expr, span: Span) -> Expr {
        let target = crate::resolve::builtin_scalar(&ty.name)
            .expect("the parser only accepts primitive casts");
        if target == Ty::STRING {
            let x = self.expr(operand, None);
            return Expr {
                span,
                ..self.stringify(x)
            };
        }
        // A literal operand takes the target type directly (`(u32) 5`); `(int) -1`.
        let x = if Self::is_numeric_literal(operand) {
            let x = self.expr(operand, Some(target));
            if x.ty == target {
                return Expr { span, ..x };
            }
            x
        } else {
            self.expr(operand, None)
        };
        if !self.types.is_numeric(x.ty) && x.ty != Ty::ERROR {
            let found = self.show(x.ty);
            self.report(
                Diagnostic::new(
                    "type-mismatch",
                    format!("cannot cast `{found}` to `{}`", ty.name),
                )
                .with_span(span)
                .with_note(
                    "casts convert between numeric types; strings parse with `int::parse()`",
                ),
            );
            return Self::error_expr(span);
        }
        Expr {
            kind: T::Cast(Box::new(x)),
            ty: target,
            span,
        }
    }

    pub(crate) fn ternary(
        &mut self,
        cond: &ast::Expr,
        then: &ast::Expr,
        otherwise: &ast::Expr,
        expect: Option<Ty>,
        span: Span,
    ) -> Expr {
        let (c, facts) = self.condition(cond);
        let saved = self.facts_snapshot();
        self.apply_facts(&facts.when_true);
        let t = self.expr(then, expect);
        let then_out = self.facts_snapshot();
        self.restore_facts(saved.clone());
        self.apply_facts(&facts.when_false);
        let o = self.expr(otherwise, expect.or(Some(t.ty)));
        let else_out = self.facts_snapshot();
        // Afterwards, what holds at the end of both branches (that can complete).
        let outs: Vec<_> = [(t.ty, then_out), (o.ty, else_out)]
            .into_iter()
            .filter(|(ty, _)| *ty != Ty::NEVER)
            .map(|(_, f)| f)
            .collect();
        if outs.is_empty() {
            self.restore_facts(saved);
        } else {
            self.join_facts(outs);
        }
        let ty = self.branch_type(&[&t, &o], expect, span);
        let (t, o) = (self.coerce(t, ty), self.coerce(o, ty));
        Expr {
            kind: T::Ternary {
                cond: Box::new(c),
                then: Box::new(t),
                otherwise: Box::new(o),
            },
            ty,
            span,
        }
    }

    /// The type of alternatives (ternary branches, `match` arms): the expected type
    /// when every branch fits it, else the one type all non-`never` branches share.
    pub(crate) fn branch_type(&mut self, branches: &[&Expr], expect: Option<Ty>, span: Span) -> Ty {
        let tys: Vec<Ty> = branches
            .iter()
            .map(|b| b.ty)
            .filter(|&t| t != Ty::NEVER && t != Ty::ERROR)
            .collect();
        // With an expected type, every branch must fit it; the caller converts each
        // branch, which reports the ones that do not.
        if let Some(t) = expect {
            return t;
        }
        let Some(&first) = tys.first() else {
            return if branches.iter().any(|b| b.ty == Ty::ERROR) {
                Ty::ERROR
            } else {
                Ty::NEVER
            };
        };
        if tys.iter().all(|&t| t == first) {
            return first;
        }
        if expect.is_none() {
            let shown: Vec<String> = tys.iter().map(|&t| format!("`{}`", self.show(t))).collect();
            self.report(
                Diagnostic::new(
                    "type-mismatch",
                    format!("branches have different types: {}", shown.join(", ")),
                )
                .with_span(span)
                .with_fix(
                    "give the result a declared type that accepts every branch (e.g. `int|string`)",
                ),
            );
        }
        Ty::ERROR
    }

    /// `a..=b` / `a..<b` over integers (SPEC "Ranges").
    pub(crate) fn range(
        &mut self,
        start: &ast::Expr,
        end: &ast::Expr,
        inclusive: bool,
        expect: Option<Ty>,
        span: Span,
    ) -> Expr {
        let elem_hint = expect.and_then(|t| match self.types.kind(t) {
            TyKind::Range(e) => Some(*e),
            _ => None,
        });
        let (s, e) = self.same_type_operands(start, end, elem_hint);
        let elem = self.common(&s, &e, span);
        if elem != Ty::ERROR && self.types.as_int(elem).is_none() {
            let found = self.show(elem);
            self.report(
                Diagnostic::new(
                    "type-mismatch",
                    format!("ranges are over integers, found `{found}`"),
                )
                .with_span(span),
            );
        }
        let ty = self.types.range(elem);
        Expr {
            kind: T::Range {
                start: Box::new(s),
                end: Box::new(e),
                inclusive,
            },
            ty,
            span,
        }
    }
}

pub(crate) fn strip_parens(e: &ast::Expr) -> &ast::Expr {
    match &e.kind {
        ast::ExprKind::Paren(x) => strip_parens(x),
        _ => e,
    }
}

/// A literal without its sign and parentheses.
fn strip_neg(e: &ast::Expr) -> &ast::Expr {
    match &e.kind {
        ast::ExprKind::Paren(x) | ast::ExprKind::Unary { expr: x, .. } => strip_neg(x),
        _ => e,
    }
}

fn is_null(e: &ast::Expr) -> bool {
    matches!(strip_parens(e).kind, ast::ExprKind::Null)
}

pub(crate) fn arith(op: B) -> BinaryOp {
    match op {
        B::Add => BinaryOp::Add,
        B::Sub => BinaryOp::Sub,
        B::Mul => BinaryOp::Mul,
        B::Div => BinaryOp::Div,
        B::IntDiv => BinaryOp::IntDiv,
        B::Rem => BinaryOp::Rem,
        B::Pow => BinaryOp::Pow,
        B::Shl => BinaryOp::Shl,
        B::Shr => BinaryOp::Shr,
        B::BitAnd => BinaryOp::BitAnd,
        B::BitOr => BinaryOp::BitOr,
        B::BitXor => BinaryOp::BitXor,
        B::Concat => BinaryOp::Concat,
        B::Coalesce => BinaryOp::Coalesce,
        _ => unreachable!("not an arithmetic operator"),
    }
}

pub(crate) fn binop_str(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::IntDiv => "~/",
        BinaryOp::Rem => "%",
        BinaryOp::Pow => "**",
        BinaryOp::Shl => "<<",
        BinaryOp::Shr => ">>",
        BinaryOp::BitAnd => "&",
        BinaryOp::BitOr => "|",
        BinaryOp::BitXor => "^",
        BinaryOp::Concat => ".",
        _ => "operator",
    }
}

fn unary_str(op: U) -> &'static str {
    match op {
        U::Neg => "-",
        U::Plus => "+",
        U::BitNot => "~",
        _ => "!",
    }
}
