//! Expressions.
//!
//! `expr(e, expect)` checks an expression and returns its TIR. `expect` is the type
//! the context wants, if any; it is a *hint*, used where SCB takes a type from
//! context (SPEC "Numerics", "Collections and values"):
//!
//! - an integer literal takes the width of the expected integer type and is
//!   range-checked (`$a: u8 = 300` is `literal-out-of-range`); a float literal takes
//!   the expected float width; an integer literal never becomes a float;
//! - `[]` needs an expected type (`cannot-infer` otherwise); `[1, 2]` becomes a
//!   `set` or `tuple` when one is expected;
//! - `match` arms and ternary branches are each checked against it.
//!
//! The hint does not enforce anything: callers that require a type use `expr_as`,
//! which also checks assignability and reports `type-mismatch`.

use scb_syntax::ast::{self, ExprKind as A};
use scb_syntax::{Diagnostic, Span};

use crate::Ctx;
use crate::check::FrameKind;
use crate::check::rel::Iface;
use crate::decls::TypeKind;
use crate::resolve::qname_text;
use crate::tir::{self, Expr, ExprKind as T, Lit};
use crate::ty::{FloatTy, Ty, TyKind};

impl Ctx<'_> {
    /// Checks `e` and requires its type to be assignable to `ty`.
    pub(crate) fn expr_as(&mut self, e: &ast::Expr, ty: Ty) -> Expr {
        let x = self.expr(e, Some(ty));
        self.coerce(x, ty)
    }

    /// Converts a checked expression to `ty`, or reports `type-mismatch`.
    pub(crate) fn coerce(&mut self, x: Expr, ty: Ty) -> Expr {
        if x.ty == ty || x.ty == Ty::ERROR || ty == Ty::ERROR {
            return x;
        }
        if x.ty == Ty::NEVER {
            return x;
        }
        if self.assignable(x.ty, ty) {
            let span = x.span;
            return Expr {
                kind: T::Upcast(Box::new(x)),
                ty,
                span,
            };
        }
        let mut d = Diagnostic::new(
            "type-mismatch",
            format!("expected `{}`, found `{}`", self.show(ty), self.show(x.ty)),
        )
        .with_span(x.span);
        if self.types.is_nullable(x.ty) && !self.types.is_nullable(ty) {
            let inner = self.types.strip_null(x.ty);
            if self.assignable(inner, ty) {
                d = d
                    .with_note("the value may be `null`")
                    .with_fix("check `!= null` first, or give a default with `??`");
            }
        }
        if matches!(self.types.kind(x.ty), TyKind::Int(_))
            && matches!(self.types.kind(ty), TyKind::Int(_))
        {
            d = d.with_fix(format!(
                "cast explicitly: `({}) …` (no implicit widening)",
                self.show(ty)
            ));
        }
        self.report(d);
        Expr { ty: Ty::ERROR, ..x }
    }

    /// Checks an expression whose result must be `bool` (SPEC "Type system":
    /// conditions are never truthy).
    pub(crate) fn condition_value(&mut self, e: &ast::Expr) -> Expr {
        let x = self.expr(e, Some(Ty::BOOL));
        if x.ty != Ty::BOOL && x.ty != Ty::ERROR && x.ty != Ty::NEVER {
            let found = self.show(x.ty);
            let fix = if self.types.is_nullable(x.ty) {
                "compare explicitly: `!= null`"
            } else if self.types.is_numeric(x.ty) {
                "compare explicitly: `!= 0`"
            } else if x.ty == Ty::STRING {
                "compare explicitly: `!= ''`"
            } else {
                "use a `bool` expression"
            };
            self.report(
                Diagnostic::new(
                    "non-bool-condition",
                    format!("a condition must be `bool`, found `{found}`"),
                )
                .with_span(x.span)
                .with_fix(fix),
            );
            return Expr { ty: Ty::ERROR, ..x };
        }
        x
    }

    /// Converts to `string` for `echo`, interpolation and `(string)`: the value must
    /// be `Stringable` (SPEC "Strings → Interpolation").
    pub(crate) fn stringify(&mut self, x: Expr) -> Expr {
        if x.ty == Ty::STRING || x.ty == Ty::ERROR {
            return x;
        }
        if self.conforms(x.ty, Iface::Stringable) {
            let span = x.span;
            return Expr {
                kind: T::ToString(Box::new(x)),
                ty: Ty::STRING,
                span,
            };
        }
        let found = self.show(x.ty);
        let mut d = Diagnostic::new(
            "type-mismatch",
            format!("`{found}` is not `Stringable` and cannot be printed or interpolated"),
        )
        .with_span(x.span);
        if x.ty == Ty::BOOL {
            d = d
                .with_note("`bool` has no string conversion")
                .with_fix("write `$b ? 'true' : 'false'`");
        } else if self.types.is_nullable(x.ty) {
            d = d
                .with_note("`null` is not `Stringable`")
                .with_fix("give a default: `$x ?? ''`");
        }
        self.report(d);
        Expr { ty: Ty::ERROR, ..x }
    }

    pub(crate) fn expr(&mut self, e: &ast::Expr, expect: Option<Ty>) -> Expr {
        // Postfix chains are checked as a unit so `?->` can short-circuit the rest of
        // the chain (see `nullsafe` below).
        if is_postfix(e) {
            let mark = self.frame().nullsafe.len();
            let x = self.postfix(e, expect);
            return self.close_nullsafe(mark, x);
        }
        let span = e.span;
        let (kind, ty) = match &e.kind {
            A::Int(v) => return self.int_lit(i128::from(*v), span, expect),
            A::Float(text) => return self.float_lit(text, span, expect),
            A::Str(s) => (T::Lit(Lit::Str(s.clone())), Ty::STRING),
            A::Bytes(b) => (T::Lit(Lit::Bytes(b.clone())), Ty::BYTES),
            A::Bool(b) => (T::Lit(Lit::Bool(*b)), Ty::BOOL),
            A::Null => (T::Lit(Lit::Null), Ty::NULL),
            A::Interpolated(parts) => {
                let parts = parts
                    .iter()
                    .map(|p| match p {
                        ast::InterpPart::Text(t) => Expr {
                            kind: T::Lit(Lit::Str(t.clone())),
                            ty: Ty::STRING,
                            span,
                        },
                        ast::InterpPart::Expr(x) => {
                            let x = self.expr(x, None);
                            self.stringify(x)
                        }
                    })
                    .collect();
                (T::Interpolate(parts), Ty::STRING)
            }
            A::Var(name) => return self.var(name, span),
            A::Name(q) => return self.name_value(q, span),
            A::SelfRef | A::Parent(_) => {
                self.report(
                    Diagnostic::new(
                        "syntax-error",
                        "`self` / `parent` is only valid before `::`",
                    )
                    .with_span(span),
                );
                return Self::error_expr(span);
            }
            A::Paren(inner) => {
                let x = self.expr(inner, expect);
                return Expr { span, ..x };
            }
            A::List(items) => return self.list_lit(items, expect, span),
            A::Map(entries) => return self.map_lit(entries, expect, span),
            A::Unary { op, expr } => return self.unary(*op, expr, expect, span),
            A::Binary { op, lhs, rhs } => return self.binary(*op, lhs, rhs, expect, span),
            A::Assign { op, target, value } => return self.assign(*op, target, value, span),
            A::Cast { ty, expr } => return self.cast(ty, expr, span),
            A::Ternary {
                cond,
                then,
                otherwise,
            } => return self.ternary(cond, then, otherwise, expect, span),
            A::Range {
                start,
                end,
                inclusive,
            } => return self.range(start, end, *inclusive, expect, span),
            A::InstanceOf { expr, pattern } => return self.instanceof(expr, pattern, span).0,
            A::New { class, args } => return self.new_expr(class, args, expect, span),
            A::Clone { expr, with } => return self.clone_expr(expr, with, span),
            A::Closure(c) => return self.closure(c, span),
            A::Match(m) => return self.match_expr(m, expect, false),
            A::Exit(code) => {
                let code = code.as_ref().map(|c| {
                    let x = self.expr(c, Some(Ty::I64));
                    if x.ty == Ty::STRING {
                        self.report(
                            Diagnostic::new(
                                "type-mismatch",
                                "`exit` takes an `int` exit code, not a message",
                            )
                            .with_span(c.span)
                            .with_fix("print the message first: `\\Io\\eprint($msg); exit(1);`"),
                        );
                        return Box::new(Expr { ty: Ty::ERROR, ..x });
                    }
                    Box::new(self.coerce(x, Ty::I64))
                });
                (T::Exit(code), Ty::NEVER)
            }
            A::Throw(x) => {
                let x = self.expr(x, None);
                let throwable = self.decls.type_named("Throwable");
                let ok = match self.types.kind(x.ty) {
                    TyKind::Named(d, _) => throwable.is_some_and(|t| self.decls.is_subtype(*d, t)),
                    TyKind::Error | TyKind::Never => true,
                    _ => false,
                };
                if !ok {
                    let found = self.show(x.ty);
                    self.report(
                        Diagnostic::new(
                            "type-mismatch",
                            format!("only `Throwable`s can be thrown, found `{found}`"),
                        )
                        .with_span(x.span),
                    );
                }
                (T::Throw(Box::new(x)), Ty::NEVER)
            }
            A::Error => return Self::error_expr(span),
            A::Prop { .. }
            | A::StaticMember { .. }
            | A::StaticProp { .. }
            | A::Index { .. }
            | A::Call { .. }
            | A::CallableRef(_) => {
                unreachable!("postfix expressions are handled above")
            }
        };
        Expr { kind, ty, span }
    }

    /// Checks one link of a postfix chain without closing pending `?->`s, so the
    /// whole chain short-circuits together.
    pub(crate) fn chain_base(&mut self, e: &ast::Expr) -> Expr {
        if is_postfix(e) {
            self.postfix(e, None)
        } else {
            self.expr(e, None)
        }
    }

    fn postfix(&mut self, e: &ast::Expr, expect: Option<Ty>) -> Expr {
        let span = e.span;
        match &e.kind {
            A::Call { callee, args } => self.call(callee, args, expect, span),
            A::CallableRef(target) => self.callable_ref(target, span),
            A::StaticMember { class, name } => self.static_member(class, name, span),
            A::Prop { .. } | A::StaticProp { .. } | A::Index { .. } => self.read_place(e),
            _ => unreachable!("is_postfix"),
        }
    }

    /// The object of `->`/`?->`: for `?->` on a nullable value, starts a nullsafe
    /// section: the value goes into a temporary and the rest of the chain works on
    /// the non-null temporary. `close_nullsafe` wraps the finished chain.
    pub(crate) fn chain_object(&mut self, object: &ast::Expr, nullsafe: bool) -> Expr {
        let base = self.chain_base(object);
        if !nullsafe || !self.types.is_nullable(base.ty) {
            return base;
        }
        let inner = self.types.strip_null(base.ty);
        let span = base.span;
        let local = self.new_local("?->", inner, Self::plain_meta(inner));
        self.frame().nullsafe.push((local, base));
        Expr {
            kind: T::Local(local),
            ty: inner,
            span,
        }
    }

    fn close_nullsafe(&mut self, mark: usize, mut x: Expr) -> Expr {
        while self.frame().nullsafe.len() > mark {
            let (local, object) = self.frame().nullsafe.pop().expect("length checked");
            let ty = if x.ty == Ty::VOID {
                Ty::VOID
            } else {
                self.types.nullable(x.ty)
            };
            let span = x.span;
            x = Expr {
                kind: T::Nullsafe {
                    object: Box::new(object),
                    local,
                    then: Box::new(x),
                },
                ty,
                span,
            };
        }
        x
    }

    // ---------- literals ----------

    /// An integer literal (already negated for `-5`), typed from `expect`.
    pub(crate) fn int_lit(&mut self, v: i128, span: Span, expect: Option<Ty>) -> Expr {
        let target = expect.and_then(|t| self.literal_target(t, true));
        let ty = match target {
            Some(t) if self.types.as_float(t).is_some() => {
                self.report(
                    Diagnostic::new(
                        "type-mismatch",
                        format!("an integer literal is not a `{}`", self.show(t)),
                    )
                    .with_span(span)
                    .with_note("a literal's kind is fixed by its spelling")
                    .with_fix(format!("write `{v}.0`")),
                );
                return Self::error_expr(span);
            }
            Some(t) => t,
            None => Ty::I64,
        };
        let i = self.types.as_int(ty).expect("an integer type");
        if v < i.min() || v > i.max() {
            self.report(
                Diagnostic::new(
                    "literal-out-of-range",
                    format!(
                        "`{v}` does not fit in `{}` ({}..={})",
                        i.name(),
                        i.min(),
                        i.max()
                    ),
                )
                .with_span(span),
            );
            return Self::error_expr(span);
        }
        Expr {
            kind: T::Lit(Lit::Int(v)),
            ty,
            span,
        }
    }

    /// A float literal (text possibly starting with `-`), typed from `expect`.
    pub(crate) fn float_lit(&mut self, text: &str, span: Span, expect: Option<Ty>) -> Expr {
        let target = expect.and_then(|t| self.literal_target(t, false));
        let ty = match target {
            Some(t) if self.types.as_int(t).is_some() => {
                self.report(
                    Diagnostic::new(
                        "type-mismatch",
                        format!("a float literal is not a `{}`", self.show(t)),
                    )
                    .with_span(span)
                    .with_note("a literal's kind is fixed by its spelling"),
                );
                return Self::error_expr(span);
            }
            Some(t) => t,
            None => Ty::F64,
        };
        let value = match self.types.as_float(ty) {
            Some(FloatTy::F32) => text.parse::<f32>().map(f64::from),
            _ => text.parse::<f64>(),
        };
        let Ok(value) = value else {
            self.report(
                Diagnostic::new("invalid-number", "malformed float literal").with_span(span),
            );
            return Self::error_expr(span);
        };
        if value.is_infinite() {
            self.report(
                Diagnostic::new(
                    "literal-out-of-range",
                    format!("`{text}` does not fit in `{}`", self.show(ty)),
                )
                .with_span(span),
            );
            return Self::error_expr(span);
        }
        Expr {
            kind: T::Lit(Lit::Float(value)),
            ty,
            span,
        }
    }

    /// The numeric type a literal should take where `t` is expected: `t` itself, or
    /// the single matching numeric member of a union (`?u8` → `u8`).
    fn literal_target(&self, t: Ty, int: bool) -> Option<Ty> {
        let numeric: Vec<Ty> = self
            .types
            .members(t)
            .into_iter()
            .filter(|&m| self.types.is_numeric(m))
            .collect();
        let same_kind: Vec<Ty> = numeric
            .iter()
            .copied()
            .filter(|&m| self.types.as_int(m).is_some() == int)
            .collect();
        match (same_kind.as_slice(), numeric.as_slice()) {
            ([one], _) => Some(*one),
            ([], [other, ..]) => Some(*other),
            _ => None,
        }
    }

    /// Whether an expression is a numeric literal (possibly negated or parenthesized):
    /// such an operand takes its type from the other operand.
    pub(crate) fn is_numeric_literal(e: &ast::Expr) -> bool {
        match &e.kind {
            A::Int(_) | A::Float(_) => true,
            A::Unary {
                op: ast::UnaryOp::Neg | ast::UnaryOp::Plus,
                expr,
            } => Self::is_numeric_literal(expr),
            A::Paren(x) => Self::is_numeric_literal(x),
            _ => false,
        }
    }

    // ---------- names ----------

    fn var(&mut self, name: &str, span: Span) -> Expr {
        match self.lookup_local(name) {
            Some(id) => {
                let ty = self.local_ty(id);
                Expr {
                    kind: T::Local(id),
                    ty,
                    span,
                }
            }
            None => {
                let d = if name == "this" {
                    Diagnostic::new(
                        "undeclared-variable",
                        "`$this` is only available in instance methods",
                    )
                    .with_span(span)
                } else {
                    Diagnostic::new("undeclared-variable", format!("`${name}` is not declared"))
                        .with_span(span)
                        .with_fix(format!("declare it with a type: `${name}: T = …;`"))
                };
                self.report(d);
                Self::error_expr(span)
            }
        }
    }

    /// A bare name in value position: a constant or a magic constant.
    fn name_value(&mut self, q: &ast::QualifiedName, span: Span) -> Expr {
        let text = qname_text(q);
        if !q.absolute
            && q.parts.len() == 1
            && let Some(lit) = self.magic_constant(&text, span)
        {
            let ty = if matches!(lit, Lit::Int(_)) {
                Ty::I64
            } else {
                Ty::STRING
            };
            return Expr {
                kind: T::Lit(lit),
                ty,
                span,
            };
        }
        let file = self.file();
        match self.lookup_const(file, q) {
            Some(c) => {
                let ty = self.decls.konst(c).ty;
                Expr {
                    kind: T::Const(c),
                    ty,
                    span,
                }
            }
            None => {
                if self.lookup_function(file, q).is_some() {
                    self.report(
                        Diagnostic::new("type-mismatch", format!("`{text}` is a function"))
                            .with_span(span)
                            .with_fix(format!(
                                "call it, or write `{text}(...)` for a function value"
                            )),
                    );
                } else {
                    self.unresolved(span, "constant", q, file);
                }
                Self::error_expr(span)
            }
        }
    }

    /// `__LINE__`, `__FILE__`, … (SPEC "Built-in statements": resolved at compile time).
    fn magic_constant(&mut self, name: &str, span: Span) -> Option<Lit> {
        let frame = self.frames.last()?;
        let file = &self.files[frame.file];
        Some(match name {
            "__FILE__" => Lit::Str(file.path.clone()),
            "__DIR__" => Lit::Str(
                std::path::Path::new(&file.path)
                    .parent()
                    .map_or(String::new(), |p| p.display().to_string()),
            ),
            "__NAMESPACE__" => Lit::Str(file.namespace.clone()),
            "__CLASS__" => Lit::Str(
                frame
                    .self_decl
                    .map_or(String::new(), |d| self.decls.ty(d).name.clone()),
            ),
            "__FUNCTION__" | "__METHOD__" => Lit::Str(frame.name.clone()),
            "__LINE__" => Lit::Int(i128::from(
                self.sources.file(span.file).line_col(span.start).0,
            )),
            _ => return None,
        })
    }

    // ---------- collections ----------

    fn list_lit(&mut self, items: &[ast::Expr], expect: Option<Ty>, span: Span) -> Expr {
        let want = expect.map(|t| self.collection_target(t));
        match want.map(|t| (t, self.types.kind(t).clone())) {
            Some((ty, TyKind::List(e))) => {
                let xs = items.iter().map(|i| self.expr_as(i, e)).collect();
                Expr {
                    kind: T::List(xs),
                    ty,
                    span,
                }
            }
            Some((ty, TyKind::Set(e))) => {
                let xs = items.iter().map(|i| self.expr_as(i, e)).collect();
                Expr {
                    kind: T::Set(xs),
                    ty,
                    span,
                }
            }
            Some((ty, TyKind::Tuple(es))) => {
                if es.len() != items.len() {
                    self.report(
                        Diagnostic::new(
                            "type-mismatch",
                            format!(
                                "expected {} elements for `{}`, found {}",
                                es.len(),
                                self.show(ty),
                                items.len()
                            ),
                        )
                        .with_span(span),
                    );
                    return Self::error_expr(span);
                }
                let xs = items
                    .iter()
                    .zip(es)
                    .map(|(i, e)| self.expr_as(i, e))
                    .collect();
                Expr {
                    kind: T::Tuple(xs),
                    ty,
                    span,
                }
            }
            Some((ty, TyKind::Map(..))) if items.is_empty() => Expr {
                kind: T::Map(Vec::new()),
                ty,
                span,
            },
            _ => {
                if items.is_empty() {
                    self.report(
                        Diagnostic::new("cannot-infer", "the type of `[]` cannot be inferred")
                            .with_span(span)
                            .with_fix("declare the type: `$x: list<int> = [];`"),
                    );
                    return Self::error_expr(span);
                }
                // No usable context: all elements must have one type.
                let first = self.expr(&items[0], None);
                let elem = first.ty;
                let mut xs = vec![first];
                for i in &items[1..] {
                    let x = self.expr(i, Some(elem));
                    if x.ty != elem && x.ty != Ty::ERROR && elem != Ty::ERROR {
                        self.report(
                            Diagnostic::new(
                                "type-mismatch",
                                format!(
                                    "list elements have different types (`{}` and `{}`)",
                                    self.show(elem),
                                    self.show(x.ty)
                                ),
                            )
                            .with_span(x.span)
                            .with_fix("declare the list's type, e.g. `list<int|string>`"),
                        );
                    }
                    xs.push(x);
                }
                let ty = self.types.list(elem);
                Expr {
                    kind: T::List(xs),
                    ty,
                    span,
                }
            }
        }
    }

    /// The collection type a literal should build where `t` is expected: `t`, or the
    /// single collection member of a union (`?list<int>` → `list<int>`).
    fn collection_target(&self, t: Ty) -> Ty {
        let cs: Vec<Ty> = self
            .types
            .members(t)
            .into_iter()
            .filter(|&m| {
                matches!(
                    self.types.kind(m),
                    TyKind::List(_) | TyKind::Set(_) | TyKind::Map(..) | TyKind::Tuple(_)
                )
            })
            .collect();
        if cs.len() == 1 { cs[0] } else { t }
    }

    fn map_lit(
        &mut self,
        entries: &[(ast::Expr, ast::Expr)],
        expect: Option<Ty>,
        span: Span,
    ) -> Expr {
        let want = expect.map(|t| self.collection_target(t));
        if let Some((ty, TyKind::Map(k, v))) = want.map(|t| (t, self.types.kind(t).clone())) {
            let xs = entries
                .iter()
                .map(|(ke, ve)| (self.expr_as(ke, k), self.expr_as(ve, v)))
                .collect();
            // The declared type's key was checked where the type was written.
            return Expr {
                kind: T::Map(xs),
                ty,
                span,
            };
        }
        let (k0, v0) = &entries[0];
        let k = self.expr(k0, None);
        let v = self.expr(v0, None);
        let (kt, vt) = (k.ty, v.ty);
        let mut xs = vec![(k, v)];
        for (ke, ve) in &entries[1..] {
            let k = self.expr_as(ke, kt);
            let v = self.expr_as(ve, vt);
            xs.push((k, v));
        }
        self.require_hashable(kt, span);
        let ty = self.types.map(kt, vt);
        Expr {
            kind: T::Map(xs),
            ty,
            span,
        }
    }

    /// Map keys and set elements must be `Hashable` (floats are not).
    pub(crate) fn require_hashable(&mut self, ty: Ty, span: Span) {
        if !self.conforms(ty, Iface::Hashable) {
            let mut d = Diagnostic::new(
                "unsatisfied-bound",
                format!(
                    "`{}` is not `Hashable` and cannot be a map key or set element",
                    self.show(ty)
                ),
            )
            .with_span(span);
            if self.types.as_float(ty).is_some() {
                d = d.with_fix("wrap the float in `\\Float\\Key`");
            }
            self.report(d);
        }
    }

    // ---------- clone and closures ----------

    fn clone_expr(&mut self, object: &ast::Expr, with: &[ast::Arg], span: Span) -> Expr {
        let x = self.expr(object, None);
        let ty = x.ty;
        let Some(d) = self.class_of(ty) else {
            if ty != Ty::ERROR {
                self.report(
                    Diagnostic::new(
                        "type-mismatch",
                        format!("`clone` needs a class instance, found `{}`", self.show(ty)),
                    )
                    .with_span(span),
                );
            }
            return Self::error_expr(span);
        };
        match self.decls.ty(d).kind {
            TypeKind::Class | TypeKind::Interface => {}
            _ => {
                self.report(
                    Diagnostic::new(
                        "clone-value-type",
                        format!("`clone` on `{}`, a value type", self.show(ty)),
                    )
                    .with_span(span)
                    .with_note("assignment already copies structs and enums")
                    .with_fix("assign it: `$copy: T = $value;`"),
                );
                return Self::error_expr(span);
            }
        }
        let mut fields = Vec::new();
        for a in with {
            let (Some(name), ast::ArgKind::Value(v)) = (&a.name, &a.kind) else {
                continue;
            };
            let Some(idx) = self.decls.field_index(d, &name.name) else {
                self.report(
                    Diagnostic::new(
                        "unknown-member",
                        format!("`{}` has no property `${}`", self.show(ty), name.name),
                    )
                    .with_span(name.span),
                );
                continue;
            };
            let fty = self.decls.ty(d).fields[idx as usize].ty;
            fields.push((idx, self.expr_as(v, fty)));
        }
        Expr {
            kind: T::Clone {
                object: Box::new(x),
                with: fields,
            },
            ty,
            span,
        }
    }

    fn closure(&mut self, c: &ast::Closure, span: Span) -> Expr {
        let file = self.file();
        let scope = self.frame().ty_scope.clone();
        let sig = self.signature(file, &c.params, Some(&c.ret), &scope);
        let fn_ty = {
            let params = sig
                .params
                .iter()
                .map(|p| crate::ty::FnParam {
                    inout: p.inout,
                    ty: if p.variadic {
                        self.types.list(p.ty)
                    } else {
                        p.ty
                    },
                })
                .collect();
            self.types.func(params, sig.ret)
        };
        let func = tir::FuncId(self.funcs.len() as u32);
        self.funcs.push(None);
        let outer = self.frames.last().expect("closures appear inside bodies");
        let mut frame = self.new_frame(file, FrameKind::Closure, "{closure}".into(), sig.ret);
        frame.self_decl = outer.self_decl;
        frame.is_static = outer.is_static;
        frame.ty_scope = scope;
        self.frames.push(frame);
        let params = self.declare_params(&c.params, &sig);
        let body = match &c.body {
            ast::ClosureBody::Expr(e) => tir::Body::Expr(self.expr_as(e, sig.ret)),
            ast::ClosureBody::Block(b) => {
                let (block, diverges) = self.block(b);
                if !diverges && sig.ret != Ty::VOID && sig.ret != Ty::ERROR {
                    self.report(
                        Diagnostic::new(
                            "missing-return",
                            format!(
                                "missing `return`: the closure can end without returning a `{}`",
                                self.show(sig.ret)
                            ),
                        )
                        .with_span(span),
                    );
                }
                tir::Body::Block(block)
            }
        };
        let frame = self.frames.pop().expect("pushed above");
        let captures = frame.captures.clone();
        let owner = frame.self_decl;
        self.funcs[func.0 as usize] = Some(crate::check::finish(frame, owner, params, body, span));
        Expr {
            kind: T::Closure { func, captures },
            ty: fn_ty,
            span,
        }
    }

    // ---------- constant expressions ----------

    /// Constant initializers and defaults are compile-time constants: literals,
    /// constants, enum cases, operators and collection literals of those (`new` is
    /// also allowed in defaults; SPEC "Functions").
    pub(crate) fn require_constant(&mut self, e: &ast::Expr, allow_new: bool) {
        if let Some(bad) = non_constant(e, allow_new) {
            self.report(
                Diagnostic::new(
                    "non-constant-expression",
                    "this must be a compile-time constant",
                )
                .with_span(bad.span)
                .with_note("allowed: literals, constants, enum cases, operators on them, and collection literals"),
            );
        }
    }
}

fn non_constant(e: &ast::Expr, allow_new: bool) -> Option<&ast::Expr> {
    match &e.kind {
        A::Int(_)
        | A::Float(_)
        | A::Str(_)
        | A::Bytes(_)
        | A::Bool(_)
        | A::Null
        | A::Name(_)
        | A::Error => None,
        A::StaticMember { class, .. } => non_constant(class, allow_new),
        A::SelfRef | A::Parent(_) => None,
        A::Paren(x) | A::Unary { expr: x, .. } | A::Cast { expr: x, .. } => {
            non_constant(x, allow_new)
        }
        A::Binary { lhs, rhs, .. } => {
            non_constant(lhs, allow_new).or_else(|| non_constant(rhs, allow_new))
        }
        A::Ternary {
            cond,
            then,
            otherwise,
        } => non_constant(cond, allow_new)
            .or_else(|| non_constant(then, allow_new))
            .or_else(|| non_constant(otherwise, allow_new)),
        A::List(xs) => xs.iter().find_map(|x| non_constant(x, allow_new)),
        A::Map(kvs) => kvs
            .iter()
            .find_map(|(k, v)| non_constant(k, allow_new).or_else(|| non_constant(v, allow_new))),
        A::Range { start, end, .. } => {
            non_constant(start, allow_new).or_else(|| non_constant(end, allow_new))
        }
        A::New { args, .. } if allow_new => args.iter().find_map(|a| match &a.kind {
            ast::ArgKind::Value(x) | ast::ArgKind::Spread(x) | ast::ArgKind::Inout(x) => {
                non_constant(x, allow_new)
            }
        }),
        // Enum case construction `Shape::CIRCLE(1.0)`.
        A::Call { callee, args } if matches!(callee.kind, A::StaticMember { .. }) => {
            args.iter().find_map(|a| match &a.kind {
                ast::ArgKind::Value(x) => non_constant(x, allow_new),
                _ => Some(e),
            })
        }
        _ => Some(e),
    }
}

/// Member access, indexing, calls and `::`: the links of a postfix chain.
pub(crate) fn is_postfix(e: &ast::Expr) -> bool {
    matches!(
        e.kind,
        A::Prop { .. }
            | A::StaticMember { .. }
            | A::StaticProp { .. }
            | A::Index { .. }
            | A::Call { .. }
            | A::CallableRef(_)
    )
}
