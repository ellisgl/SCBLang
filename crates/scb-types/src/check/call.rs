//! Calls: functions, methods, static methods, enum cases, `new`, closures, and
//! argument matching (SPEC "Functions").
//!
//! Arguments are matched to parameters by position, then by name (named arguments
//! after positional ones); omitted parameters use their default; a variadic parameter
//! takes the remaining positional arguments (or one `...$list` spread). The TIR keeps
//! the arguments in written order, each tagged with its parameter, because arguments
//! are evaluated left to right as written.
//!
//! Stdlib signatures may be generic (`map<U>($f: fn(T): U): list<U>`). The receiver
//! fixes the type's own parameters (`T = int` for a `list<int>`); a method's own
//! parameters are inferred from the arguments by `unify`. Literal arguments are
//! checked last, once their expected type is known.

use std::collections::HashMap;

use scb_syntax::ast::{self, ExprKind as A};
use scb_syntax::{Diagnostic, Span};

use crate::Ctx;
use crate::check::place::Access;
use crate::check::rel::Iface;
use crate::decls::{DeclId, MethodId, ParamId, ParamSig, TypeKind};
use crate::resolve::qname_text;
use crate::tir::{Arg, ArgValue, Builtin, CallTarget, Expr, ExprKind as T, Place, Receiver};
use crate::ty::{FnParam, Ty, TyKind};

/// A resolved callee signature: parameters and return type, with type parameters
/// partially or fully substituted.
struct Callee {
    params: Vec<ParamSig>,
    ret: Ty,
    subst: HashMap<ParamId, Ty>,
    self_ty: Option<Ty>,
    /// Type parameters to infer from the arguments.
    infer: Vec<ParamId>,
    what: String,
}

impl Ctx<'_> {
    pub(crate) fn call(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::Arg],
        expect: Option<Ty>,
        span: Span,
    ) -> Expr {
        match &callee.kind {
            A::Name(q) => self.function_call(q, args, expect, span),
            A::Prop {
                object,
                name,
                nullsafe,
            } => self.method_call(object, name, *nullsafe, args, expect, span),
            A::StaticMember { class, name } => self.static_call(class, name, args, expect, span),
            _ => {
                let f = self.chain_base(callee);
                self.value_call(f, args, span)
            }
        }
    }

    fn function_call(
        &mut self,
        q: &ast::QualifiedName,
        args: &[ast::Arg],
        expect: Option<Ty>,
        span: Span,
    ) -> Expr {
        let file = self.file();
        let Some(fid) = self.lookup_function(file, q) else {
            self.unresolved(q.span, "function", q, file);
            self.discard_args(args);
            return Self::error_expr(span);
        };
        let f = self.decls.function(fid);
        let callee = Callee {
            params: f.sig.params.clone(),
            ret: f.sig.ret,
            subst: HashMap::new(),
            self_ty: None,
            infer: f.type_params.clone(),
            what: format!("`{}`", qname_text(q)),
        };
        let func = f.func;
        let (targs, ret) = self.match_args(callee, args, expect, span);
        Expr {
            kind: T::Call {
                target: CallTarget::Func(func),
                args: targs,
            },
            ty: ret,
            span,
        }
    }

    /// `$x->m(…)` / `$x?->m(…)`.
    ///
    /// A receiver written as a place (`$list`, `$o->lines`) is resolved as one, so a
    /// `mutating` method can write back through it; other receivers are values.
    fn method_call(
        &mut self,
        object: &ast::Expr,
        name: &ast::Ident,
        nullsafe: bool,
        args: &[ast::Arg],
        expect: Option<Ty>,
        span: Span,
    ) -> Expr {
        let is_place_expr = !nullsafe
            && matches!(
                crate::check::ops::strip_parens(object).kind,
                A::Var(_) | A::Prop { .. } | A::Index { .. } | A::StaticProp { .. }
            );
        let recv = if is_place_expr {
            match self.place(object, Access::Read) {
                Some(p) => Recv::Place(p),
                None => {
                    self.discard_args(args);
                    return Self::error_expr(span);
                }
            }
        } else {
            Recv::Value(self.chain_object(object, nullsafe))
        };
        let recv_ty = match &recv {
            Recv::Place(p) => p.ty,
            Recv::Value(x) => x.ty,
        };
        if recv_ty == Ty::ERROR {
            self.discard_args(args);
            return Self::error_expr(span);
        }
        if self.types.is_nullable(recv_ty) {
            let shown = self.show(recv_ty);
            self.report(
                Diagnostic::new("type-mismatch", format!("`{shown}` may be `null`"))
                    .with_span(object.span)
                    .with_fix("use `?->`, or check `!= null` first"),
            );
            self.discard_args(args);
            return Self::error_expr(span);
        }
        let Some((mid, decl)) = self.find_method_on(recv_ty, &name.name, name.span) else {
            self.discard_args(args);
            return Self::error_expr(span);
        };
        let m = self.decls.method(mid);
        let (mutating, owner, vis, func, is_static) =
            (m.mutating, m.owner, m.visibility, m.func, m.is_static);
        if is_static {
            let owner = self.short_name(owner);
            self.report(
                Diagnostic::new("static-call", format!("`{}` is static", name.name))
                    .with_span(name.span)
                    .with_fix(format!("call it as `{owner}::{}(…)`", name.name)),
            );
        }
        self.check_visibility(owner, vis, &format!("{}()", name.name), name.span);
        let callee = self.method_callee(mid, decl, recv_ty, name.span);
        // A `mutating` method on a value type writes back through the receiver.
        let receiver = match recv {
            Recv::Place(p) if mutating && !self.is_class(recv_ty) => {
                if has_nullsafe(object) {
                    self.report(
                        Diagnostic::new(
                            "write-through-nullsafe",
                            "cannot call a `mutating` method through `?->`",
                        )
                        .with_span(object.span),
                    );
                }
                self.check_writable(&p, object.span);
                Receiver::Place(p)
            }
            Recv::Value(x) if mutating && !self.is_class(recv_ty) => {
                let shown = self.show(recv_ty);
                self.report(
                    Diagnostic::new(
                        "mutating-temporary",
                        format!("calling a `mutating` method on a temporary `{shown}`"),
                    )
                    .with_span(x.span)
                    .with_note("the change would be lost")
                    .with_fix("store the value in a variable first"),
                );
                self.discard_args(args);
                return Self::error_expr(span);
            }
            Recv::Place(p) => {
                let (ty, sp) = (p.ty, p.span);
                Receiver::Value(Expr {
                    kind: T::Read(p),
                    ty,
                    span: sp,
                })
            }
            Recv::Value(x) => Receiver::Value(x),
        };
        let (targs, ret) = self.match_args(callee, args, expect, span);
        if let Receiver::Place(p) = &receiver {
            self.check_receiver_exclusivity(p, &targs, span);
        }
        let target = if self.is_builtin(mid) {
            CallTarget::Builtin {
                builtin: self.builtin_name(mid),
                receiver: Some(Box::new(receiver)),
            }
        } else {
            CallTarget::Method {
                receiver: Box::new(receiver),
                name: name.name.to_string(),
                func,
            }
        };
        Expr {
            kind: T::Call {
                target,
                args: targs,
            },
            ty: ret,
            span,
        }
    }

    /// Finds an instance or static method on a type (with stdlib built-in types
    /// looked up through their stub class).
    fn find_method_on(&mut self, ty: Ty, name: &str, span: Span) -> Option<(MethodId, DeclId)> {
        // `catch ($e: IndexError | KeyError)`: methods of the closest common supertype.
        let decl = self.methods_decl(ty).or_else(|| self.common_supertype(ty));
        let Some(decl) = decl else {
            let shown = self.show(ty);
            self.report(
                Diagnostic::new("unknown-member", format!("`{shown}` has no methods"))
                    .with_span(span),
            );
            return None;
        };
        match self.decls.find_method(decl, name) {
            Some(m) => Some((m, decl)),
            None => {
                let shown = self.show(ty);
                let hint = if self.decls.field_index(decl, name).is_some() {
                    format!(
                        " (`${name}` is a property; a closure stored in it is called as `($obj->{name})(…)`)"
                    )
                } else {
                    String::new()
                };
                self.report(
                    Diagnostic::new(
                        "unknown-member",
                        format!("`{shown}` has no method `{name}`{hint}"),
                    )
                    .with_span(span),
                );
                None
            }
        }
    }

    /// The closest class or interface every member of a union of named types extends
    /// or implements.
    pub(crate) fn common_supertype(&self, ty: Ty) -> Option<DeclId> {
        let TyKind::Union(ms) = self.types.kind(ty) else {
            return None;
        };
        let decls: Vec<DeclId> = ms
            .iter()
            .map(|&m| self.class_of(m))
            .collect::<Option<_>>()?;
        let first = *decls.first()?;
        let mut candidates = self.decls.ancestors(first);
        candidates.extend(self.decls.all_interfaces(first));
        candidates
            .into_iter()
            .find(|&c| decls.iter().all(|&d| self.decls.is_subtype(d, c)))
    }

    /// A method's signature as seen through a receiver of type `recv`: the type's own
    /// parameters are fixed by the receiver; `where` bounds must hold.
    fn method_callee(&mut self, mid: MethodId, decl: DeclId, recv: Ty, span: Span) -> Callee {
        let m = self.decls.method(mid);
        let params = m.sig.params.clone();
        let ret = m.sig.ret;
        let infer = m.type_params.clone();
        let bounds = m.where_bounds.clone();
        let name = m.name.clone();
        let mut subst = HashMap::new();
        let type_params = self.decls.ty(decl).type_params.clone();
        let args: Vec<Ty> = match self.types.kind(recv).clone() {
            TyKind::List(e) | TyKind::Set(e) | TyKind::Range(e) => vec![e],
            TyKind::Map(k, v) => vec![k, v],
            TyKind::Named(_, a) => a,
            _ => Vec::new(),
        };
        for (p, a) in type_params.iter().zip(args) {
            subst.insert(*p, a);
        }
        for (p, bound) in bounds {
            let actual = subst.get(&p).copied().unwrap_or(Ty::ERROR);
            if let Some(iface) = self.iface_of(bound)
                && !self.conforms(actual, iface)
            {
                let shown = self.show(actual);
                self.report(
                    Diagnostic::new(
                        "unsatisfied-bound",
                        format!(
                            "`{name}()` needs elements that are {}, and `{shown}` is not",
                            iface.name()
                        ),
                    )
                    .with_span(span),
                );
            }
        }
        Callee {
            params,
            ret,
            subst,
            self_ty: Some(recv),
            infer,
            what: format!("`{name}()`"),
        }
    }

    fn iface_of(&self, bound: Ty) -> Option<Iface> {
        match self.types.kind(bound) {
            TyKind::Named(d, _) => Iface::from_name(&self.decls.ty(*d).name),
            _ => None,
        }
    }

    fn is_builtin(&self, mid: MethodId) -> bool {
        let m = self.decls.method(mid);
        self.decls.ty(m.owner).stdlib && m.ast.member.is_some() && {
            let ast::Member::Method(am) = crate::check::member_ast(self.files, m.ast) else {
                return false;
            };
            am.body.is_none()
        }
    }

    fn builtin_name(&self, mid: MethodId) -> Builtin {
        let m = self.decls.method(mid);
        let sep = if m.is_static { "::" } else { "." };
        Builtin(format!("{}{sep}{}", self.decls.ty(m.owner).name, m.name))
    }

    /// `X::m(…)`, `parent::__construct(…)`, `parent(I)::m(…)`, `Enum::CASE(…)`.
    fn static_call(
        &mut self,
        class: &ast::Expr,
        name: &ast::Ident,
        args: &[ast::Arg],
        expect: Option<Ty>,
        span: Span,
    ) -> Expr {
        if let A::Parent(iface) = &class.kind {
            return self.parent_call(iface.as_ref(), name, args, span);
        }
        let Some(decl) = self.static_class(class) else {
            self.discard_args(args);
            return Self::error_expr(span);
        };
        // Enum case construction.
        if self.decls.ty(decl).kind == TypeKind::Enum
            && let Some(ci) = self
                .decls
                .ty(decl)
                .cases
                .iter()
                .position(|c| *c.name == *name.name)
        {
            let case = self.decls.ty(decl).cases[ci].clone();
            let Some(payload) = case.payload else {
                self.report(
                    Diagnostic::new(
                        "type-mismatch",
                        format!("`{}` has no payload and takes no arguments", case.name),
                    )
                    .with_span(span)
                    .with_fix(format!(
                        "write `{}::{}`",
                        self.short_name(decl),
                        case.name
                    )),
                );
                self.discard_args(args);
                return Self::error_expr(span);
            };
            let callee = Callee {
                params: payload,
                ret: Ty::VOID,
                subst: HashMap::new(),
                self_ty: None,
                infer: Vec::new(),
                what: format!("`{}`", case.name),
            };
            let (targs, _) = self.match_args(callee, args, None, span);
            let ty = self.types.named(decl, Vec::new());
            return Expr {
                kind: T::EnumCase {
                    decl,
                    case: ci as u32,
                    args: targs,
                },
                ty,
                span,
            };
        }
        if self.decls.ty(decl).kind == TypeKind::Enum
            && self.decls.find_method(decl, &name.name).is_none()
            && let Some(x) = self.enum_builtin(decl, name, args, span)
        {
            return x;
        }
        let Some(mid) = self.decls.find_method(decl, &name.name) else {
            self.report(
                Diagnostic::new(
                    "unknown-member",
                    format!(
                        "`{}` has no method or case `{}`",
                        self.short_name(decl),
                        name.name
                    ),
                )
                .with_span(name.span),
            );
            self.discard_args(args);
            return Self::error_expr(span);
        };
        let m = self.decls.method(mid);
        let (is_static, owner, vis, func) = (m.is_static, m.owner, m.visibility, m.func);
        self.check_visibility(owner, vis, &format!("{}()", name.name), name.span);
        if !is_static {
            self.report(
                Diagnostic::new(
                    "static-call",
                    format!("`{}` is an instance method", name.name),
                )
                .with_span(name.span)
                .with_fix(format!("call it on an instance: `$x->{}(…)`", name.name)),
            );
        }
        let recv_ty = self.self_type(decl);
        let mut callee = self.method_callee(mid, decl, recv_ty, name.span);
        callee.what = format!("`{}::{}()`", self.short_name(decl), name.name);
        let (targs, ret) = self.match_args(callee, args, expect, span);
        let target = if self.is_builtin(mid) {
            CallTarget::Builtin {
                builtin: self.builtin_name(mid),
                receiver: None,
            }
        } else {
            CallTarget::Func(func)
        };
        Expr {
            kind: T::Call {
                target,
                args: targs,
            },
            ty: ret,
            span,
        }
    }

    /// The built-in enum API (SPEC "Enums and match"): `from()` and `tryFrom()` on
    /// backed enums, `cases()` on pure and backed enums. `None` if `name` is not one.
    fn enum_builtin(
        &mut self,
        decl: DeclId,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> Option<Expr> {
        let enum_ty = self.types.named(decl, Vec::new());
        let backing = self.decls.ty(decl).backing;
        let has_data = self
            .decls
            .ty(decl)
            .cases
            .iter()
            .any(|c| c.payload.is_some());
        match &*name.name {
            "from" | "tryFrom" => {
                let Some(backing) = backing else {
                    self.report(
                        Diagnostic::new(
                            "unknown-member",
                            format!(
                                "`{}` is not a backed enum, so it has no `{}()`",
                                self.short_name(decl),
                                name.name
                            ),
                        )
                        .with_span(name.span),
                    );
                    return Some(Self::error_expr(span));
                };
                let try_ = &*name.name == "tryFrom";
                let callee = Callee {
                    params: vec![ParamSig {
                        name: "value".into(),
                        ty: backing,
                        inout: false,
                        variadic: false,
                        has_default: false,
                    }],
                    ret: Ty::VOID,
                    subst: HashMap::new(),
                    self_ty: None,
                    infer: Vec::new(),
                    what: format!("`{}()`", name.name),
                };
                let (mut targs, _) = self.match_args(callee, args, None, span);
                let value = match targs.pop().map(|a| a.value) {
                    Some(ArgValue::Value(v)) => v,
                    _ => Self::error_expr(span),
                };
                let ty = if try_ {
                    self.types.nullable(enum_ty)
                } else {
                    enum_ty
                };
                Some(Expr {
                    kind: T::EnumFrom {
                        decl,
                        value: Box::new(value),
                        try_,
                    },
                    ty,
                    span,
                })
            }
            "cases" => {
                // SPEC "Enums and match" (Q35): pure and backed enums; data-carrying
                // cases need payloads, so those enums have no `cases()`.
                if has_data {
                    self.report(
                        Diagnostic::new(
                            "unknown-member",
                            "`cases()` lists enums without payloads only",
                        )
                        .with_span(name.span),
                    );
                    return Some(Self::error_expr(span));
                }
                if !args.is_empty() {
                    self.report(
                        Diagnostic::new("wrong-arguments", "`cases()` takes no arguments")
                            .with_span(span),
                    );
                }
                let ty = self.types.list(enum_ty);
                Some(Expr {
                    kind: T::EnumCases(decl),
                    ty,
                    span,
                })
            }
            _ => None,
        }
    }

    /// `parent::m(…)` (no dynamic dispatch) and `parent(Iface)::m(…)` (an explicit
    /// interface default; SPEC "Default-method conflicts").
    fn parent_call(
        &mut self,
        iface: Option<&ast::NamedType>,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> Expr {
        let here = self.frames.last().and_then(|f| f.self_decl);
        let target_decl = match (here, iface) {
            (Some(h), None) => self.decls.ty(h).parent,
            (Some(h), Some(i)) => {
                let file = self.file();
                let scope = self.frame().ty_scope.clone();
                let t = ast::Type {
                    kind: ast::TypeKind::Named(i.clone()),
                    span: i.span,
                };
                let ty = self.resolve_type(file, &t, &scope);
                let d = self.class_of(ty);
                if d.is_some_and(|d| !self.decls.ty(h).interface_ids.contains(&d)) {
                    self.report(
                        Diagnostic::new(
                            "type-mismatch",
                            "`parent(I)` must name an interface this type implements",
                        )
                        .with_span(i.span),
                    );
                    self.discard_args(args);
                    return Self::error_expr(span);
                }
                d
            }
            (None, _) => None,
        };
        let Some(target_decl) = target_decl else {
            self.report(
                Diagnostic::new("unresolved-name", "`parent` used where there is no parent")
                    .with_span(span),
            );
            self.discard_args(args);
            return Self::error_expr(span);
        };
        let Some(mid) = self.decls.find_method(target_decl, &name.name) else {
            self.report(
                Diagnostic::new(
                    "unknown-member",
                    format!(
                        "`{}` has no method `{}`",
                        self.short_name(target_decl),
                        name.name
                    ),
                )
                .with_span(name.span),
            );
            self.discard_args(args);
            return Self::error_expr(span);
        };
        let m = self.decls.method(mid);
        let (is_static, func) = (m.is_static, m.func);
        if name.name.as_ref() == "__construct" && !self.frame().in_ctor {
            self.report(
                Diagnostic::new(
                    "type-mismatch",
                    "`parent::__construct()` is only called from a constructor",
                )
                .with_span(span),
            );
        }
        let recv_ty = self.self_type(target_decl);
        let callee = self.method_callee(mid, target_decl, recv_ty, name.span);
        let (targs, ret) = self.match_args(callee, args, None, span);
        let receiver = if is_static {
            None
        } else {
            let this = self.lookup_local("this");
            this.map(|id| {
                let p = self.local_place(id, span);
                let ty = p.ty;
                Box::new(Receiver::Value(Expr {
                    kind: T::Read(p),
                    ty,
                    span,
                }))
            })
        };
        let target = if self.is_builtin(mid) {
            CallTarget::Builtin {
                builtin: self.builtin_name(mid),
                receiver,
            }
        } else {
            CallTarget::Parent { func, receiver }
        };
        Expr {
            kind: T::Call {
                target,
                args: targs,
            },
            ty: ret,
            span,
        }
    }

    /// The declaration before `::`: a class name, `self`, a built-in type (`i8`,
    /// `int`, `f64`).
    pub(crate) fn static_class(&mut self, class: &ast::Expr) -> Option<DeclId> {
        match &class.kind {
            A::SelfRef => {
                let d = self.frames.last().and_then(|f| f.self_decl);
                if d.is_none() {
                    self.report(
                        Diagnostic::new("unresolved-name", "`self` outside a class")
                            .with_span(class.span),
                    );
                }
                d
            }
            A::Parent(_) => {
                let d = self
                    .frames
                    .last()
                    .and_then(|f| f.self_decl)
                    .and_then(|d| self.decls.ty(d).parent);
                if d.is_none() {
                    self.report(
                        Diagnostic::new(
                            "unresolved-name",
                            "`parent` used where there is no parent",
                        )
                        .with_span(class.span),
                    );
                }
                d
            }
            A::Name(q) => {
                let file = self.file();
                if !q.absolute
                    && q.parts.len() == 1
                    && let Some(ty) = crate::resolve::builtin_scalar(&q.parts[0].name)
                {
                    return self.methods_decl(ty);
                }
                let d = self.lookup_type(file, q);
                if d.is_none() {
                    self.unresolved(q.span, "type", q, file);
                }
                d
            }
            _ => {
                self.report(
                    Diagnostic::new("syntax-error", "`::` needs a class name")
                        .with_span(class.span),
                );
                None
            }
        }
    }

    /// `X::NAME` in value position: a class constant, an enum case, or `X::class`.
    pub(crate) fn static_member(
        &mut self,
        class: &ast::Expr,
        name: &ast::Ident,
        span: Span,
    ) -> Expr {
        let Some(decl) = self.static_class(class) else {
            return Self::error_expr(span);
        };
        if &*name.name == "class" {
            return Expr {
                kind: T::ClassName(decl),
                ty: Ty::STRING,
                span,
            };
        }
        if let Some(ci) = self
            .decls
            .ty(decl)
            .cases
            .iter()
            .position(|c| *c.name == *name.name)
        {
            let case = &self.decls.ty(decl).cases[ci];
            if case.payload.is_some() {
                let case_name = case.name.clone();
                self.report(
                    Diagnostic::new(
                        "missing-case-arguments",
                        format!(
                            "`{}::{case_name}` carries data and needs its arguments",
                            self.short_name(decl)
                        ),
                    )
                    .with_span(span)
                    .with_note("a data case is not a function value")
                    .with_fix(format!("write `{}::{case_name}(…)`", self.short_name(decl))),
                );
                return Self::error_expr(span);
            }
            let ty = self.types.named(decl, Vec::new());
            return Expr {
                kind: T::EnumCase {
                    decl,
                    case: ci as u32,
                    args: Vec::new(),
                },
                ty,
                span,
            };
        }
        if let Some(c) = self.decls.find_const(decl, &name.name) {
            let k = self.decls.konst(c);
            let (ty, vis, owner) = (k.ty, k.visibility, k.owner);
            if let Some(o) = owner {
                self.check_visibility(o, vis, &name.name, name.span);
            }
            return Expr {
                kind: T::Const(c),
                ty,
                span,
            };
        }
        let hint = if self.decls.find_method(decl, &name.name).is_some() {
            format!(
                " (`{}` is a method: call it, or write `{}::{}(...)`)",
                name.name,
                self.short_name(decl),
                name.name
            )
        } else {
            String::new()
        };
        self.report(
            Diagnostic::new(
                "unknown-member",
                format!(
                    "`{}` has no constant or case `{}`{hint}",
                    self.short_name(decl),
                    name.name
                ),
            )
            .with_span(name.span),
        );
        Self::error_expr(span)
    }

    /// Calling a value of function type: `$f(…)`. Function types have no parameter
    /// names, so arguments are positional.
    fn value_call(&mut self, f: Expr, args: &[ast::Arg], span: Span) -> Expr {
        let TyKind::Fn(fty) = self.types.kind(f.ty).clone() else {
            if f.ty != Ty::ERROR {
                let shown = self.show(f.ty);
                self.report(
                    Diagnostic::new("type-mismatch", format!("`{shown}` is not callable"))
                        .with_span(f.span),
                );
            }
            self.discard_args(args);
            return Self::error_expr(span);
        };
        let params = fty
            .params
            .iter()
            .enumerate()
            .map(|(i, p)| ParamSig {
                name: format!("#{i}"),
                ty: p.ty,
                inout: p.inout,
                variadic: false,
                has_default: false,
            })
            .collect();
        if let Some(named) = args.iter().find_map(|a| a.name.as_ref()) {
            self.report(
                Diagnostic::new(
                    "wrong-arguments",
                    "a function value takes positional arguments only",
                )
                .with_span(named.span),
            );
        }
        let callee = Callee {
            params,
            ret: fty.ret,
            subst: HashMap::new(),
            self_ty: None,
            infer: Vec::new(),
            what: "the function value".into(),
        };
        let (targs, ret) = self.match_args(callee, args, None, span);
        Expr {
            kind: T::Call {
                target: CallTarget::Value(Box::new(f)),
                args: targs,
            },
            ty: ret,
            span,
        }
    }

    /// `f(...)`, `$obj->m(...)`, `C::m(...)`: a function value.
    pub(crate) fn callable_ref(&mut self, target: &ast::Expr, span: Span) -> Expr {
        let (target, sig) = match &target.kind {
            A::Name(q) => {
                let file = self.file();
                let Some(fid) = self.lookup_function(file, q) else {
                    self.unresolved(q.span, "function", q, file);
                    return Self::error_expr(span);
                };
                let f = self.decls.function(fid);
                if !f.type_params.is_empty() {
                    self.report(
                        Diagnostic::new(
                            "cannot-infer",
                            "a generic function cannot be used as a value",
                        )
                        .with_span(span)
                        .with_fix("wrap it in a closure with concrete types"),
                    );
                    return Self::error_expr(span);
                }
                (CallTarget::Func(f.func), f.sig.clone())
            }
            A::Prop {
                object,
                name,
                nullsafe: false,
            } => {
                let recv = self.chain_base(object);
                let Some((mid, decl)) = self.find_method_on(recv.ty, &name.name, name.span) else {
                    return Self::error_expr(span);
                };
                let callee = self.method_callee(mid, decl, recv.ty, name.span);
                let mut sig = self.decls.method(mid).sig.clone();
                for p in &mut sig.params {
                    p.ty = self.types.substitute(p.ty, &callee.subst, callee.self_ty);
                }
                sig.ret = self
                    .types
                    .substitute(sig.ret, &callee.subst, callee.self_ty);
                let func = self.decls.method(mid).func;
                let target = if self.is_builtin(mid) {
                    CallTarget::Builtin {
                        builtin: self.builtin_name(mid),
                        receiver: Some(Box::new(Receiver::Value(recv))),
                    }
                } else {
                    CallTarget::Method {
                        receiver: Box::new(Receiver::Value(recv)),
                        name: name.name.to_string(),
                        func,
                    }
                };
                (target, sig)
            }
            A::StaticMember { class, name } => {
                let Some(decl) = self.static_class(class) else {
                    return Self::error_expr(span);
                };
                let Some(mid) = self.decls.find_method(decl, &name.name) else {
                    self.report(
                        Diagnostic::new(
                            "unknown-member",
                            format!("`{}` has no method `{}`", self.short_name(decl), name.name),
                        )
                        .with_span(name.span),
                    );
                    return Self::error_expr(span);
                };
                let m = self.decls.method(mid);
                (CallTarget::Func(m.func), m.sig.clone())
            }
            _ => {
                self.report(
                    Diagnostic::new(
                        "syntax-error",
                        "`(...)` makes a function value from a function or method name",
                    )
                    .with_span(span),
                );
                return Self::error_expr(span);
            }
        };
        let params = sig
            .params
            .iter()
            .map(|p| FnParam {
                inout: p.inout,
                ty: if p.variadic {
                    self.types.list(p.ty)
                } else {
                    p.ty
                },
            })
            .collect();
        let ty = self.types.func(params, sig.ret);
        if self.types.has_params(ty) {
            self.report(
                Diagnostic::new(
                    "cannot-infer",
                    "this method is generic and cannot be used as a value",
                )
                .with_span(span),
            );
            return Self::error_expr(span);
        }
        Expr {
            kind: T::CallableRef(target),
            ty,
            span,
        }
    }

    /// `new C(…)`.
    pub(crate) fn new_expr(
        &mut self,
        class: &ast::NamedType,
        args: &[ast::Arg],
        expect: Option<Ty>,
        span: Span,
    ) -> Expr {
        let file = self.file();
        let scope = self.frame().ty_scope.clone();
        let t = ast::Type {
            kind: ast::TypeKind::Named(class.clone()),
            span: class.span,
        };
        // `new Ref(0)`: type arguments may be omitted and inferred.
        let explicit_args = !class.args.is_empty();
        let ty = if explicit_args {
            self.resolve_type(file, &t, &scope)
        } else {
            match self.lookup_type(file, &class.name) {
                Some(d) => {
                    let params = self.decls.ty(d).type_params.clone();
                    let args = params.into_iter().map(|p| self.types.param(p)).collect();
                    self.types.named(d, args)
                }
                None => self.resolve_type(file, &t, &scope),
            }
        };
        let Some(decl) = self.class_of(ty) else {
            if ty != Ty::ERROR {
                let shown = self.show(ty);
                self.report(
                    Diagnostic::new(
                        "type-mismatch",
                        format!("`new` needs a class or struct, found `{shown}`"),
                    )
                    .with_span(class.span),
                );
            }
            self.discard_args(args);
            return Self::error_expr(span);
        };
        let d = self.decls.ty(decl);
        let problem = match d.kind {
            TypeKind::Interface => Some("an interface cannot be instantiated"),
            TypeKind::Enum => Some("enum values are built from their cases: `Enum::CASE`"),
            TypeKind::Class if d.is_abstract => Some("an abstract class cannot be instantiated"),
            _ if d.builtin.is_some() => Some("built-in types have literals, not constructors"),
            _ => None,
        };
        if let Some(problem) = problem {
            self.report(Diagnostic::new("type-mismatch", problem).with_span(class.span));
            self.discard_args(args);
            return Self::error_expr(span);
        }
        let ctor = self.decls.constructor(decl);
        let (targs, ctor_func, final_ty) = match ctor {
            Some(mid) => {
                let m = self.decls.method(mid);
                let (vis, owner, func) = (m.visibility, m.owner, m.func);
                self.check_visibility(owner, vis, "__construct()", class.span);
                let mut callee = self.method_callee(mid, decl, ty, class.span);
                callee.what = format!("`{}` constructor", self.short_name(decl));
                // Unknown type arguments of the class are inferred like method ones.
                let params = self.decls.ty(decl).type_params.clone();
                for p in &params {
                    if !explicit_args {
                        callee.subst.remove(p);
                        callee.infer.push(*p);
                    }
                }
                if let Some(TyKind::Named(ed, eargs)) = expect.map(|e| self.types.kind(e).clone())
                    && ed == decl
                {
                    for (p, a) in params.iter().zip(eargs) {
                        callee.subst.insert(*p, a);
                    }
                }
                let (targs, _, subst) = self.match_args_subst(callee, args, None, span);
                let final_args: Vec<Ty> = params
                    .iter()
                    .map(|p| subst.get(p).copied().unwrap_or(Ty::ERROR))
                    .collect();
                let final_ty = if params.is_empty() {
                    ty
                } else {
                    self.types.named(decl, final_args)
                };
                (targs, Some(func), final_ty)
            }
            None => {
                if !args.is_empty() {
                    self.report(
                        Diagnostic::new(
                            "wrong-arguments",
                            format!("`{}` has no constructor and takes no arguments", self.short_name(decl)),
                        )
                        .with_span(span)
                        .with_note("there is no implicit memberwise constructor; use constructor promotion"),
                    );
                    self.discard_args(args);
                }
                (Vec::new(), None, ty)
            }
        };
        Expr {
            kind: T::New {
                class: decl,
                ctor: ctor_func,
                args: targs,
            },
            ty: final_ty,
            span,
        }
    }

    /// `$map[$k] ?? …` / `$list[$i] ?? …`: a non-trapping `get`.
    pub(crate) fn get_call(&mut self, obj: Expr, index: &ast::Expr, span: Span) -> Expr {
        let (key_ty, value_ty) = match self.types.kind(obj.ty).clone() {
            TyKind::Map(k, v) => (k, v),
            TyKind::List(e) => (Ty::I64, e),
            _ => unreachable!("callers check for map or list"),
        };
        let builtin = if matches!(self.types.kind(obj.ty), TyKind::Map(..)) {
            "map.get"
        } else {
            "list.get"
        };
        let key = self.expr_as(index, key_ty);
        let ty = self.types.nullable(value_ty);
        Expr {
            kind: T::Call {
                target: CallTarget::Builtin {
                    builtin: Builtin(builtin.into()),
                    receiver: Some(Box::new(Receiver::Value(obj))),
                },
                args: vec![Arg {
                    param: 0,
                    value: ArgValue::Value(key),
                }],
            },
            ty,
            span,
        }
    }

    /// Checks arguments that cannot be used (after an error) so their own mistakes
    /// are still reported.
    pub(crate) fn discard_args(&mut self, args: &[ast::Arg]) {
        for a in args {
            let (ast::ArgKind::Value(e) | ast::ArgKind::Spread(e) | ast::ArgKind::Inout(e)) =
                &a.kind;
            let _ = self.expr(e, None);
        }
    }

    fn match_args(
        &mut self,
        callee: Callee,
        args: &[ast::Arg],
        expect: Option<Ty>,
        span: Span,
    ) -> (Vec<Arg>, Ty) {
        let (a, r, _) = self.match_args_subst(callee, args, expect, span);
        (a, r)
    }

    /// Matches arguments to parameters, checks them, infers type parameters, and
    /// returns the TIR arguments (in written order), the return type and the final
    /// substitution.
    fn match_args_subst(
        &mut self,
        mut callee: Callee,
        args: &[ast::Arg],
        expect: Option<Ty>,
        span: Span,
    ) -> (Vec<Arg>, Ty, HashMap<ParamId, Ty>) {
        // 1. Which parameter each argument goes to.
        let n = callee.params.len();
        let variadic = callee
            .params
            .last()
            .is_some_and(|p| p.variadic)
            .then(|| n - 1);
        let mut slots: Vec<Option<usize>> = Vec::new();
        let mut filled = vec![false; n];
        let mut next_positional = 0usize;
        let mut seen_named = false;
        for a in args {
            let slot = match &a.name {
                Some(label) => {
                    seen_named = true;
                    match callee.params.iter().position(|p| *p.name == *label.name) {
                        Some(i) if filled[i] => {
                            self.report(
                                Diagnostic::new(
                                    "wrong-arguments",
                                    format!("`{}` is given twice", label.name),
                                )
                                .with_span(label.span),
                            );
                            None
                        }
                        Some(i) => Some(i),
                        None => {
                            self.report(
                                Diagnostic::new(
                                    "wrong-arguments",
                                    format!("{} has no parameter `${}`", callee.what, label.name),
                                )
                                .with_span(label.span),
                            );
                            None
                        }
                    }
                }
                None => {
                    if seen_named {
                        self.report(
                            Diagnostic::new(
                                "wrong-arguments",
                                "positional arguments must come before named ones",
                            )
                            .with_span(a.span),
                        );
                        None
                    } else if let ast::ArgKind::Spread(_) = a.kind
                        && variadic.is_none()
                    {
                        // SPEC "Functions" (Q33): a list spreads only into a variadic
                        // parameter; fixed parameters and `...$map` are not supported.
                        self.report(
                            Diagnostic::new(
                                "wrong-arguments",
                                "`...` spreads only into a variadic parameter",
                            )
                            .with_span(a.span),
                        );
                        None
                    } else if next_positional < n && Some(next_positional) != variadic {
                        next_positional += 1;
                        Some(next_positional - 1)
                    } else if let Some(v) = variadic {
                        Some(v)
                    } else {
                        self.report(
                            Diagnostic::new(
                                "wrong-arguments",
                                format!("too many arguments for {}: it takes {n}", callee.what),
                            )
                            .with_span(a.span),
                        );
                        None
                    }
                }
            };
            if let Some(i) = slot {
                filled[i] = true;
            }
            slots.push(slot);
        }
        for (i, p) in callee.params.iter().enumerate() {
            if !filled[i] && !p.has_default && !p.variadic {
                self.report(
                    Diagnostic::new(
                        "wrong-arguments",
                        format!("{} needs an argument for `${}`", callee.what, p.name),
                    )
                    .with_span(span),
                );
            }
        }

        // 2. Seed inference from the expected result type: `$x: list<string> = $l->map(…)`.
        if let Some(e) = expect
            && !callee.infer.is_empty()
        {
            let mut trial = callee.subst.clone();
            if self.unify(callee.ret, e, &mut trial) {
                for p in &callee.infer {
                    if let Some(&t) = trial.get(p)
                        && !self.types.has_params(t)
                    {
                        callee.subst.entry(*p).or_insert(t);
                    }
                }
            }
        }

        // 3. Check arguments: those whose expected type is still open (and that are not
        //    literals) first, to infer type parameters; then the rest.
        let mut out: Vec<Option<Arg>> = (0..args.len()).map(|_| None).collect();
        let mut order: Vec<usize> = (0..args.len()).collect();
        order.sort_by_key(|&i| {
            let deferred = match &args[i].kind {
                ast::ArgKind::Value(e) => is_literal_like(e),
                _ => false,
            };
            usize::from(deferred)
        });
        let mut inout_places: Vec<(usize, Span)> = Vec::new();
        for i in order {
            let a = &args[i];
            let Some(slot) = slots[i] else {
                self.discard_args(std::slice::from_ref(a));
                continue;
            };
            let p = callee.params[slot].clone();
            let pty = self.types.substitute(p.ty, &callee.subst, callee.self_ty);
            let value = match &a.kind {
                ast::ArgKind::Inout(e) => {
                    if !p.inout {
                        self.report(
                            Diagnostic::new(
                                "inout-mismatch",
                                format!("`${}` is not an `inout` parameter", p.name),
                            )
                            .with_span(a.span)
                            .with_fix("remove `inout`"),
                        );
                    }
                    match self.place(e, Access::Modify) {
                        Some(place) => {
                            if self.types.has_params(pty) {
                                self.unify(pty, place.ty, &mut callee.subst);
                            } else if place.ty != pty && place.ty != Ty::ERROR && pty != Ty::ERROR {
                                // `inout` is invariant: the callee may store any `pty`.
                                self.type_mismatch(e.span, pty, place.ty);
                            }
                            inout_places.push((i, e.span));
                            ArgValue::Inout(place)
                        }
                        None => ArgValue::Value(Self::error_expr(e.span)),
                    }
                }
                ast::ArgKind::Spread(e) => {
                    let list = self.types.list(pty);
                    ArgValue::Spread(self.typed_arg(e, list, &mut callee.subst, callee.self_ty))
                }
                ast::ArgKind::Value(e) => {
                    if p.inout {
                        self.report(
                            Diagnostic::new(
                                "inout-mismatch",
                                format!(
                                    "`${}` is an `inout` parameter: the call site says so too",
                                    p.name
                                ),
                            )
                            .with_span(a.span)
                            .with_fix("write `inout $var`"),
                        );
                    }
                    ArgValue::Value(self.typed_arg(e, pty, &mut callee.subst, callee.self_ty))
                }
            };
            out[i] = Some(Arg {
                param: slot as u32,
                value,
            });
        }
        let out: Vec<Arg> = out.into_iter().flatten().collect();

        // 4. Exclusivity between `inout` arguments.
        let places: Vec<&Place> = out
            .iter()
            .filter_map(|a| match &a.value {
                ArgValue::Inout(p) => Some(p),
                _ => None,
            })
            .collect();
        'outer: for (x, a) in places.iter().enumerate() {
            for b in &places[x + 1..] {
                if self.overlaps(a, b) {
                    let sp = b.span;
                    self.exclusivity_error(sp, "the same variable is passed `inout` twice");
                    break 'outer;
                }
            }
        }

        for p in &callee.infer {
            if !callee.subst.contains_key(p) {
                // Unconstrained (e.g. an empty argument list): leave it to the caller's
                // context; an unresolved parameter in the result is an error below.
                let _ = p;
            }
        }
        let ret = self
            .types
            .substitute(callee.ret, &callee.subst, callee.self_ty);
        let ret = if self.types.has_params(ret) {
            self.report(
                Diagnostic::new(
                    "cannot-infer",
                    format!("cannot infer the result type of {}", callee.what),
                )
                .with_span(span)
                .with_fix("declare the expected type, e.g. `$x: list<int> = …`"),
            );
            Ty::ERROR
        } else {
            ret
        };
        (out, ret, callee.subst)
    }

    /// Checks one argument against a parameter type that may mention type
    /// parameters, binding them from the argument when it is not a literal.
    fn typed_arg(
        &mut self,
        e: &ast::Expr,
        pty: Ty,
        subst: &mut HashMap<ParamId, Ty>,
        self_ty: Option<Ty>,
    ) -> Expr {
        if !self.types.has_params(pty) {
            return self.expr_as(e, pty);
        }
        let x = self.expr(e, None);
        if !self.unify(pty, x.ty, subst) {
            self.type_mismatch(x.span, pty, x.ty);
            return x;
        }
        let resolved = self.types.substitute(pty, subst, self_ty);
        if self.types.has_params(resolved) {
            return x;
        }
        self.coerce(x, resolved)
    }

    /// A `mutating` call's receiver must not overlap its `inout` arguments.
    fn check_receiver_exclusivity(&mut self, recv: &Place, args: &[Arg], span: Span) {
        let overlap = args.iter().any(|a| match &a.value {
            ArgValue::Inout(p) => self.overlaps(recv, p),
            _ => false,
        });
        if overlap {
            self.exclusivity_error(
                span,
                "the receiver of a `mutating` call is also passed `inout`",
            );
        }
    }
}

/// Literals, collection literals and `null`: arguments whose type depends on the
/// parameter type, so they are checked after inference.
fn is_literal_like(e: &ast::Expr) -> bool {
    match &e.kind {
        A::Int(_) | A::Float(_) | A::Null | A::List(_) | A::Map(_) => true,
        A::Unary { expr, .. } | A::Paren(expr) => is_literal_like(expr),
        _ => false,
    }
}

/// A method receiver: a place (for `mutating` calls) or a plain value.
enum Recv {
    Place(Place),
    Value(Expr),
}

fn has_nullsafe(e: &ast::Expr) -> bool {
    match &e.kind {
        A::Prop {
            object, nullsafe, ..
        } => *nullsafe || has_nullsafe(object),
        A::Index { object, .. } | A::Paren(object) => has_nullsafe(object),
        _ => false,
    }
}
