//! Places: the storage behind variables, properties, elements and static properties,
//! for reads and for writes (SPEC "Nested mutation").
//!
//! A place is a root (a local, a static property, or the value of an expression) plus
//! a path of projections, with the type at every level. Reading `$o->lines[1]->qty`
//! and writing it share the same structure: a place is always resolved the same
//! way, and a write then checks the result (`check_writable`):
//!
//! - the path is split at the last class reference: everything behind a reference
//!   is writable (even `getUser()->name = 'x'`), except `readonly` properties
//!   outside their constructor;
//! - without a reference on the path, the root itself must be writable: a local
//!   that is declared, not `const`, not a closure capture (`assign-to-capture`), not
//!   `readonly` (`readonly-write`); not a call result (`mutating-temporary`); `$this`
//!   in a struct only in a `mutating` method (`missing-mutating`);
//! - `?->` cannot be written through (`write-through-nullsafe`).

use scb_syntax::ast::{self, ExprKind as A};
use scb_syntax::{Diagnostic, Span};

use crate::Ctx;
use crate::check::ops::strip_parens;
use crate::decls::{DeclId, TypeKind, Visibility};
use crate::tir::{BinaryOp, Expr, ExprKind as T, IndexKind, LocalId, Place, PlaceRoot, Proj};
use crate::ty::{Ty, TyKind};

/// Why a place is being resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    Read,
    /// `=`.
    Write,
    /// Compound assignment, `++`/`--`, `inout`, `mutating` receivers: read and write.
    Modify,
}

impl Ctx<'_> {
    /// Reads a property, element or static property.
    pub(crate) fn read_place(&mut self, e: &ast::Expr) -> Expr {
        let span = e.span;
        // Backed enum `->value`.
        if let A::Prop {
            object,
            name,
            nullsafe,
        } = &e.kind
            && &*name.name == "value"
        {
            let obj = self.chain_object(object, *nullsafe);
            if let TyKind::Named(d, _) = self.types.kind(obj.ty)
                && let Some(backing) = self.decls.ty(*d).backing
            {
                return Expr {
                    kind: T::EnumValue(Box::new(obj)),
                    ty: backing,
                    span,
                };
            }
            let base = object_place(obj);
            return match self.project(base, e, Access::Read) {
                Some(p) => read(p, span),
                None => Self::error_expr(span),
            };
        }
        match self.place(e, Access::Read) {
            Some(p) => read(p, span),
            None => Self::error_expr(span),
        }
    }

    /// Resolves a place expression and, for writes, checks that it is writable.
    /// `None` after an error.
    pub(crate) fn place(&mut self, e: &ast::Expr, access: Access) -> Option<Place> {
        if access != Access::Read && has_nullsafe_link(e) {
            self.report(
                Diagnostic::new("write-through-nullsafe", "cannot write through `?->`")
                    .with_span(e.span)
                    .with_fix("check for `null` first, then use `->`"),
            );
            return None;
        }
        let p = self.resolve_place(e, access)?;
        if access != Access::Read {
            self.check_writable(&p, e.span);
        }
        Some(p)
    }

    fn resolve_place(&mut self, e: &ast::Expr, access: Access) -> Option<Place> {
        let span = e.span;
        match &e.kind {
            A::Paren(inner) => self.resolve_place(inner, access),
            A::Var(name) => {
                let Some(id) = self.lookup_local(name) else {
                    let msg = if access == Access::Read {
                        format!("`${name}` is not declared")
                    } else {
                        format!("`${name}` is assigned but never declared")
                    };
                    self.report(
                        Diagnostic::new("undeclared-variable", msg)
                            .with_span(span)
                            .with_fix(format!("declare it with a type: `${name}: T = …;`")),
                    );
                    return None;
                };
                let ty = self.local_ty(id);
                Some(Place {
                    root: PlaceRoot::Local(id),
                    root_ty: ty,
                    path: Vec::new(),
                    tys: Vec::new(),
                    ty,
                    span,
                })
            }
            A::StaticProp { class, name } => {
                let decl = self.static_class(class)?;
                let Some(sid) = self.decls.find_static(decl, &name.name) else {
                    self.report(
                        Diagnostic::new(
                            "unknown-member",
                            format!(
                                "`{}` has no static property `${}`",
                                self.short_name(decl),
                                name.name
                            ),
                        )
                        .with_span(name.span),
                    );
                    return None;
                };
                let s = self.decls.static_(sid);
                let (ty, vis, owner) = (s.ty, s.visibility, s.owner);
                self.check_visibility(owner, vis, &format!("${}", name.name), name.span);
                Some(Place {
                    root: PlaceRoot::Static(sid),
                    root_ty: ty,
                    path: Vec::new(),
                    tys: Vec::new(),
                    ty,
                    span,
                })
            }
            A::Prop {
                object, nullsafe, ..
            } => {
                let base = self.resolve_base(object, *nullsafe, access)?;
                self.project(base, e, access)
            }
            A::Index { object, .. } => {
                let base = self.resolve_base(object, false, access)?;
                self.project(base, e, access)
            }
            _ => {
                let x = self.expr(e, None);
                Some(object_place(x))
            }
        }
    }

    /// The place a projection applies to: a nested place, or the value of an
    /// arbitrary expression (call results, `?->` targets).
    fn resolve_base(
        &mut self,
        object: &ast::Expr,
        nullsafe: bool,
        access: Access,
    ) -> Option<Place> {
        if is_place_expr(object) && !nullsafe {
            return self.resolve_place(object, access);
        }
        let x = if nullsafe {
            self.chain_object(object, true)
        } else {
            self.chain_base(object)
        };
        Some(object_place(x))
    }

    /// Applies one `->prop` or `[index]` link to `base`.
    fn project(&mut self, mut base: Place, link: &ast::Expr, access: Access) -> Option<Place> {
        let span = link.span;
        if base.ty == Ty::ERROR {
            return None;
        }
        let (proj, ty) = match &link.kind {
            A::Prop { name, nullsafe, .. } => {
                let mut ty = base.ty;
                if *nullsafe {
                    ty = self.types.strip_null(ty);
                }
                if self.types.is_nullable(ty) {
                    self.report(
                        Diagnostic::new(
                            "type-mismatch",
                            format!("`{}` may be `null`", self.show(ty)),
                        )
                        .with_span(span)
                        .with_fix("use `?->`, or check `!= null` first"),
                    );
                    return None;
                }
                let Some(d) = self.class_of(ty) else {
                    self.report(
                        Diagnostic::new(
                            "unknown-member",
                            format!("`{}` has no properties", self.show(ty)),
                        )
                        .with_span(name.span),
                    );
                    return None;
                };
                let Some(idx) = self.decls.field_index(d, &name.name) else {
                    let hint = if self.decls.find_method(d, &name.name).is_some() {
                        format!(" (`{}` is a method: call it with `()`)", name.name)
                    } else {
                        String::new()
                    };
                    self.report(
                        Diagnostic::new(
                            "unknown-member",
                            format!("`{}` has no property `{}`{hint}", self.show(ty), name.name),
                        )
                        .with_span(name.span),
                    );
                    return None;
                };
                let f = &self.decls.ty(d).fields[idx as usize];
                let (fty, owner, vis, fname) = (f.ty, f.owner, f.visibility, f.name.clone());
                self.check_visibility(owner, vis, &format!("${fname}"), name.span);
                // Fields of a generic stdlib class (`Ref<T>::$value`) through `Ref<int>`.
                let fty = self.instantiate_member(d, ty, fty);
                // A readonly property reached from a local through readonly properties
                // may have been narrowed by a condition.
                let fty = match self.readonly_path(&base, idx, d) {
                    Some(key) => self.frame().facts.get(&key).copied().unwrap_or(fty),
                    None => fty,
                };
                (Proj::Field(idx), fty)
            }
            A::Index { index, .. } => {
                let ty = base.ty;
                match (self.types.kind(ty).clone(), index) {
                    (TyKind::List(e), Some(i)) => {
                        let i = self.expr_as(i, Ty::I64);
                        (Proj::Index(Box::new(i), IndexKind::List), e)
                    }
                    (TyKind::List(e), None) => {
                        if access == Access::Read {
                            self.report(
                                Diagnostic::new(
                                    "invalid-assignment-target",
                                    "`[]` appends; it cannot be read",
                                )
                                .with_span(span),
                            );
                            return None;
                        }
                        (Proj::Push, e)
                    }
                    (TyKind::Map(k, v), Some(i)) => {
                        let i = self.expr_as(i, k);
                        (Proj::Index(Box::new(i), IndexKind::Map), v)
                    }
                    (TyKind::Bytes, Some(i)) => {
                        let i = self.expr_as(i, Ty::I64);
                        (Proj::Index(Box::new(i), IndexKind::Bytes), Ty::U8)
                    }
                    (TyKind::Tuple(es), Some(i)) => {
                        let n = match &strip_parens(i).kind {
                            A::Int(n) => usize::try_from(*n).ok().filter(|&n| n < es.len()),
                            _ => None,
                        };
                        let Some(n) = n else {
                            self.report(
                                Diagnostic::new(
                                    "tuple-index",
                                    format!("a tuple index must be an integer literal from 0 to {}", es.len() - 1),
                                )
                                .with_span(i.span)
                                .with_note("each element has its own type, so the index is fixed at compile time"),
                            );
                            return None;
                        };
                        (Proj::TupleField(n as u32), es[n])
                    }
                    (TyKind::String, _) => {
                        self.report(
                            Diagnostic::new("string-subscript", "strings cannot be indexed")
                                .with_span(span)
                                .with_note("pick a view: bytes, codepoints or graphemes")
                                .with_fix(
                                    "use `$s->bytes()[$i]`, `$s->slice(…)` or `$s->graphemes()`",
                                ),
                        );
                        return None;
                    }
                    (TyKind::Map(..), None) => {
                        self.report(
                            Diagnostic::new(
                                "invalid-assignment-target",
                                "`[]` appends to a list; a map needs a key",
                            )
                            .with_span(span),
                        );
                        return None;
                    }
                    _ => {
                        let mut d = Diagnostic::new(
                            "type-mismatch",
                            format!("`{}` cannot be indexed", self.show(ty)),
                        )
                        .with_span(span);
                        if self.types.is_nullable(ty) {
                            d = d.with_fix("check `!= null` first");
                        }
                        self.report(d);
                        return None;
                    }
                }
            }
            _ => unreachable!("only property and index links"),
        };
        base.path.push(proj);
        base.tys.push(ty);
        base.ty = ty;
        base.span = span;
        Some(base)
    }

    /// The fact key of `base->field` when it is a chain of readonly properties from a
    /// local; `None` otherwise.
    pub(crate) fn readonly_path(
        &self,
        base: &Place,
        idx: crate::decls::FieldIdx,
        decl: DeclId,
    ) -> Option<super::FactKey> {
        let PlaceRoot::Local(root) = base.root else {
            return None;
        };
        if !self.decls.ty(decl).fields[idx as usize].readonly {
            return None;
        }
        let mut path = Vec::new();
        for (i, proj) in base.path.iter().enumerate() {
            let Proj::Field(f) = proj else { return None };
            let level = if i == 0 {
                base.root_ty
            } else {
                base.tys[i - 1]
            };
            let d = self.class_of(level)?;
            if !self.decls.ty(d).fields[*f as usize].readonly {
                return None;
            }
            path.push(*f);
        }
        path.push(idx);
        Some((root, path))
    }

    /// A member type of `decl` seen through a receiver of type `recv`: the type's
    /// parameters are replaced by the receiver's type arguments.
    pub(crate) fn instantiate_member(&mut self, decl: DeclId, recv: Ty, member: Ty) -> Ty {
        let TyKind::Named(_, args) = self.types.kind(recv).clone() else {
            return member;
        };
        if args.is_empty() {
            return member;
        }
        let subst = self
            .decls
            .ty(decl)
            .type_params
            .iter()
            .copied()
            .zip(args)
            .collect();
        self.types.substitute(member, &subst, Some(recv))
    }

    /// Checks that a resolved place may be written (see the module docs).
    pub(crate) fn check_writable(&mut self, p: &Place, span: Span) {
        // Whatever was known about the root local (or its readonly properties) may
        // no longer hold.
        if let PlaceRoot::Local(id) = p.root {
            self.forget_root(id);
        }
        let level_ty = |i: usize| if i == 0 { p.root_ty } else { p.tys[i - 1] };
        // Projections from the last class reference on are writes through it.
        let boundary = (0..p.path.len())
            .rev()
            .find(|&i| self.is_class(level_ty(i)));
        let start = match boundary {
            Some(i) => i,
            None => {
                self.check_root_writable(p, span);
                0
            }
        };
        let root_is_this =
            matches!(p.root, PlaceRoot::Local(id) if self.frame().meta[id.0 as usize].is_this);
        for i in start..p.path.len() {
            let Proj::Field(idx) = p.path[i] else {
                continue;
            };
            let Some(d) = self.class_of(level_ty(i)) else {
                continue;
            };
            let f = self.decls.ty(d).fields[idx as usize].clone();
            if f.readonly {
                // Assign-once: only in the declaring type's constructor, and only when
                // neither a default nor promotion already assigned it.
                let in_own_ctor = i == 0
                    && root_is_this
                    && !f.initialized
                    && self.frame().in_ctor
                    && self.frame().self_decl == Some(f.owner);
                if !in_own_ctor {
                    self.report(
                        Diagnostic::new("readonly-write", format!("`${}` is readonly", f.name))
                            .with_span(span)
                            .with_note("readonly properties are assigned once, in their class's constructor"),
                    );
                }
            }
        }
    }

    fn check_root_writable(&mut self, p: &Place, span: Span) {
        match &p.root {
            PlaceRoot::Local(id) => {
                let id = *id;
                let meta = self.frame().meta[id.0 as usize].clone();
                let name = self.frame().locals[id.0 as usize].name.clone();
                if meta.is_this {
                    if p.path.is_empty() {
                        self.report(
                            Diagnostic::new("readonly-write", "`$this` cannot be reassigned")
                                .with_span(span),
                        );
                    } else if meta.captured {
                        self.report(
                            Diagnostic::new(
                                "assign-to-capture",
                                "a closure in a struct method cannot change `$this`",
                            )
                            .with_span(span)
                            .with_note("the closure captured a copy of the struct"),
                        );
                    } else if !self.frame().this_writable
                        && self
                            .class_of(p.root_ty)
                            .is_some_and(|d| self.decls.ty(d).kind != TypeKind::Class)
                    {
                        self.report(
                            Diagnostic::new(
                                "missing-mutating",
                                "this method changes `$this` but is not `mutating`",
                            )
                            .with_span(span)
                            .with_fix("declare the method `public mutating function …`"),
                        );
                    }
                    return;
                }
                if meta.captured {
                    self.report(
                        Diagnostic::new(
                            "assign-to-capture",
                            format!("`${name}` is captured by value and cannot be changed in the closure"),
                        )
                        .with_span(span)
                        .with_note("closures copy the variables they use when they are created")
                        .with_fix("share mutable state through a `Ref<T>`"),
                    );
                } else if meta.constant || meta.readonly {
                    let what = if meta.constant {
                        "a constant"
                    } else {
                        "readonly"
                    };
                    self.report(
                        Diagnostic::new("readonly-write", format!("`${name}` is {what}"))
                            .with_span(span),
                    );
                }
                self.forget_root(id);
            }
            PlaceRoot::Static(sid) => {
                let s = self.decls.static_(*sid);
                if s.readonly && p.path.is_empty() {
                    let name = s.name.clone();
                    self.report(
                        Diagnostic::new("readonly-write", format!("`${name}` is readonly"))
                            .with_span(span),
                    );
                }
            }
            PlaceRoot::Object(x) => {
                if p.path.is_empty() {
                    self.report(
                        Diagnostic::new(
                            "invalid-assignment-target",
                            "this expression cannot be written to",
                        )
                        .with_span(span),
                    );
                } else if x.ty != Ty::ERROR {
                    let shown = self.show(x.ty);
                    self.report(
                        Diagnostic::new(
                            "mutating-temporary",
                            format!("cannot modify part of a temporary `{shown}` value"),
                        )
                        .with_span(x.span)
                        .with_note("value types are copied: the change would be lost")
                        .with_fix("store the value in a variable, modify it, and use the variable"),
                    );
                }
            }
        }
    }

    /// Private members are visible in their type; protected ones in its subclasses
    /// and superclasses too.
    pub(crate) fn check_visibility(
        &mut self,
        owner: DeclId,
        vis: Visibility,
        what: &str,
        span: Span,
    ) {
        let here = self.frames.last().and_then(|f| f.self_decl);
        let ok = match vis {
            Visibility::Public | Visibility::Internal => true,
            Visibility::Private => here == Some(owner),
            Visibility::Protected => here.is_some_and(|h| {
                self.decls.is_subtype(h, owner) || self.decls.is_subtype(owner, h)
            }),
        };
        if !ok {
            let level = if vis == Visibility::Private {
                "private"
            } else {
                "protected"
            };
            self.report(
                Diagnostic::new(
                    "visibility",
                    format!("`{what}` is {level} in `{}`", self.short_name(owner)),
                )
                .with_span(span),
            );
        }
    }

    // ---------- assignment ----------

    pub(crate) fn assign(
        &mut self,
        op: Option<ast::BinaryOp>,
        target: &ast::Expr,
        value: &ast::Expr,
        span: Span,
    ) -> Expr {
        let Some(op) = op else {
            // Path subexpressions are evaluated before the value (SPEC "Evaluation order").
            let Some(place) = self.place(target, Access::Write) else {
                let _ = self.expr(value, None);
                return Self::error_expr(span);
            };
            let declared = self.place_declared_ty(&place);
            let v = self.expr_as(value, declared);
            // Flow typing narrows only through conditions (SPEC "Type system"); the
            // write itself dropped any fact about the variable (`check_root_writable`).
            let ty = v.ty;
            return Expr {
                kind: T::Assign {
                    place,
                    value: Box::new(v),
                },
                ty,
                span,
            };
        };
        let Some(place) = self.place(target, Access::Modify) else {
            let _ = self.expr(value, None);
            return Self::error_expr(span);
        };
        let ty = place.ty;
        let bop = crate::check::ops::arith(op);
        let v = match bop {
            BinaryOp::Coalesce => {
                // `$m[$k] ??= []` assigns when the key is missing (SPEC "Nested
                // mutation": the idiom that replaces autovivification).
                let map_element = matches!(place.path.last(), Some(Proj::Index(_, IndexKind::Map)));
                if !map_element && !self.types.is_nullable(ty) && ty != Ty::ERROR {
                    let shown = self.show(ty);
                    self.report(
                        Diagnostic::new(
                            "type-mismatch",
                            format!("`??=` needs a nullable place, found `{shown}`"),
                        )
                        .with_span(target.span),
                    );
                }
                let inner = self.types.strip_null(ty);
                self.expr_as(value, inner)
            }
            BinaryOp::Shl | BinaryOp::Shr => self.expr_as(value, Ty::U32),
            BinaryOp::Pow if self.types.as_int(ty).is_some() => self.expr_as(value, Ty::U32),
            BinaryOp::Concat => {
                if ty != Ty::STRING && ty != Ty::ERROR {
                    let shown = self.show(ty);
                    self.report(
                        Diagnostic::new(
                            "type-mismatch",
                            format!("`.=` appends to a string, found `{shown}`"),
                        )
                        .with_span(target.span),
                    );
                }
                self.expr_as(value, Ty::STRING)
            }
            BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor if ty == Ty::BOOL => {
                self.report(
                    Diagnostic::new("bitwise-on-bool", "bitwise assignment on `bool`")
                        .with_span(span)
                        .with_fix("use `$b = $b && …` / `||`"),
                );
                self.expr_as(value, ty)
            }
            _ => {
                let _ = self.check_arith(bop, ty, span);
                self.expr_as(value, ty)
            }
        };
        self.check_compound_exclusivity(&place, &v);
        let result = if bop == BinaryOp::Coalesce {
            self.types.strip_null(ty)
        } else {
            ty
        };
        Expr {
            kind: T::CompoundAssign {
                place,
                op: bop,
                value: Box::new(v),
            },
            ty: result,
            span,
        }
    }

    /// The type a place was declared with: writes are checked against the declared
    /// type, not a narrowed one.
    fn place_declared_ty(&mut self, place: &Place) -> Ty {
        match place.root {
            PlaceRoot::Local(id) if place.path.is_empty() => {
                self.frame().meta[id.0 as usize].declared
            }
            _ => place.ty,
        }
    }

    pub(crate) fn inc_dec(&mut self, op: ast::UnaryOp, operand: &ast::Expr, span: Span) -> Expr {
        let Some(place) = self.place(operand, Access::Modify) else {
            return Self::error_expr(span);
        };
        let ty = place.ty;
        if self.types.as_int(ty).is_none() && ty != Ty::ERROR {
            let shown = self.show(ty);
            self.report(
                Diagnostic::new(
                    "type-mismatch",
                    format!("`++` / `--` need an integer, found `{shown}`"),
                )
                .with_span(span),
            );
        }
        Expr {
            kind: T::IncDec {
                place,
                increment: matches!(op, ast::UnaryOp::PreInc | ast::UnaryOp::PostInc),
                prefix: matches!(op, ast::UnaryOp::PreInc | ast::UnaryOp::PreDec),
            },
            ty,
            span,
        }
    }

    // ---------- exclusivity (SPEC "Exclusivity") ----------

    /// Two modifying accesses overlap when they share a root local and one path is a
    /// prefix of the other; distinct fields or tuple elements are disjoint, any two
    /// indexes may alias. Only locals are checked here (and only up to the first
    /// class reference): class properties and statics are checked at runtime.
    pub(crate) fn overlaps(&self, a: &Place, b: &Place) -> bool {
        let (PlaceRoot::Local(x), PlaceRoot::Local(y)) = (&a.root, &b.root) else {
            return false;
        };
        if x != y || self.is_class(a.root_ty) {
            return false;
        }
        for (i, (p, q)) in a.path.iter().zip(&b.path).enumerate() {
            match (p, q) {
                (Proj::Field(i), Proj::Field(j)) if i != j => return false,
                (Proj::TupleField(i), Proj::TupleField(j)) if i != j => return false,
                _ => {}
            }
            if self.is_class(a.tys[i]) {
                return false;
            }
        }
        true
    }

    pub(crate) fn exclusivity_error(&mut self, span: Span, what: &str) {
        self.report(
            Diagnostic::new(
                "exclusivity-violation",
                format!("overlapping access: {what}"),
            )
            .with_span(span)
            .with_note("a modifying access must not overlap any other access to the same variable")
            .with_fix("use a temporary, or `$list->swap($i, $j)` for list elements"),
        );
    }

    /// `$a += g(inout $a)`: the compound assignment's access spans its right side.
    fn check_compound_exclusivity(&mut self, place: &Place, value: &Expr) {
        let mut inouts = Vec::new();
        collect_inout_places(value, &mut inouts);
        if inouts.iter().any(|p| self.overlaps(place, p)) {
            self.exclusivity_error(
                value.span,
                "the assignment's target is also passed `inout` on its right side",
            );
        }
    }

    /// The target of `local` as a bare place (for `inout $x` and receivers).
    pub(crate) fn local_place(&mut self, id: LocalId, span: Span) -> Place {
        let ty = self.local_ty(id);
        Place {
            root: PlaceRoot::Local(id),
            root_ty: ty,
            path: Vec::new(),
            tys: Vec::new(),
            ty,
            span,
        }
    }
}

fn read(p: Place, span: Span) -> Expr {
    let ty = p.ty;
    Expr {
        kind: T::Read(p),
        ty,
        span,
    }
}

/// A place rooted at the value of an expression.
fn object_place(x: Expr) -> Place {
    let (ty, span) = (x.ty, x.span);
    Place {
        root: PlaceRoot::Object(Box::new(x)),
        root_ty: ty,
        path: Vec::new(),
        tys: Vec::new(),
        ty,
        span,
    }
}

/// Whether an expression denotes storage (a variable, property, element or static
/// property) rather than a computed value.
pub(crate) fn is_place_expr(e: &ast::Expr) -> bool {
    matches!(
        strip_parens(e).kind,
        A::Var(_) | A::Prop { .. } | A::Index { .. } | A::StaticProp { .. }
    )
}

/// Whether a write target goes through `?->` anywhere.
fn has_nullsafe_link(e: &ast::Expr) -> bool {
    match &e.kind {
        A::Prop {
            object, nullsafe, ..
        } => *nullsafe || has_nullsafe_link(object),
        A::Index { object, .. } => has_nullsafe_link(object),
        A::Paren(x) => has_nullsafe_link(x),
        _ => false,
    }
}

/// `inout` arguments in an expression's calls (not inside closures).
fn collect_inout_places<'e>(e: &'e Expr, out: &mut Vec<&'e Place>) {
    match &e.kind {
        T::Call { args, .. } | T::New { args, .. } => {
            for a in args {
                match &a.value {
                    crate::tir::ArgValue::Inout(p) => out.push(p),
                    crate::tir::ArgValue::Value(x) | crate::tir::ArgValue::Spread(x) => {
                        collect_inout_places(x, out)
                    }
                }
            }
        }
        T::Binary { lhs, rhs, .. } | T::Logic { lhs, rhs, .. } | T::Coalesce { lhs, rhs } => {
            collect_inout_places(lhs, out);
            collect_inout_places(rhs, out);
        }
        T::Unary { expr, .. } | T::Upcast(expr) | T::Cast(expr) | T::ToString(expr) => {
            collect_inout_places(expr, out)
        }
        _ => {}
    }
}
