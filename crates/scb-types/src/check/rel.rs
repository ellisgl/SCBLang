//! Relations between types: assignability, conformance to the operator interfaces,
//! and display for diagnostics.

use std::collections::HashMap;

use crate::Ctx;
use crate::decls::{DeclId, ParamId, TypeKind};
use crate::ty::{Ty, TyKind};

/// An operator interface (SPEC "Equality and comparison", "Strings → Interpolation").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Iface {
    Stringable,
    Equatable,
    Hashable,
    Comparable,
    /// Internal bound: integer and float types.
    Numeric,
    /// Internal bound: integer types.
    Integer,
}

impl Iface {
    pub fn from_name(name: &str) -> Option<Iface> {
        Some(match name {
            "Stringable" => Iface::Stringable,
            "Equatable" => Iface::Equatable,
            "Hashable" => Iface::Hashable,
            "Comparable" => Iface::Comparable,
            "__Numeric" => Iface::Numeric,
            "__Integer" => Iface::Integer,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Iface::Stringable => "Stringable",
            Iface::Equatable => "Equatable",
            Iface::Hashable => "Hashable",
            Iface::Comparable => "Comparable",
            Iface::Numeric => "numeric",
            Iface::Integer => "an integer type",
        }
    }
}

impl Ctx<'_> {
    /// Whether a value of type `src` may be stored where `dst` is expected.
    ///
    /// Exact match; `never` into anything; a member into a union; every member of a
    /// union into the target; a class into its superclasses and interfaces; a value
    /// into an interface it conforms to. Collections and function types are invariant.
    pub fn assignable(&mut self, src: Ty, dst: Ty) -> bool {
        if src == dst || src == Ty::ERROR || dst == Ty::ERROR || src == Ty::NEVER {
            return true;
        }
        if let TyKind::Union(ss) = self.types.kind(src).clone() {
            return ss.iter().all(|&s| self.assignable(s, dst));
        }
        if let TyKind::Union(ds) = self.types.kind(dst).clone() {
            return ds.iter().any(|&d| self.assignable(src, d));
        }
        match (self.types.kind(src).clone(), self.types.kind(dst).clone()) {
            (TyKind::Named(a, aargs), TyKind::Named(b, bargs)) => {
                if a == b {
                    return aargs == bargs;
                }
                if !self.decls.is_subtype(a, b) {
                    return false;
                }
                bargs.is_empty() || self.declared_supertypes(src).contains(&dst)
            }
            (_, TyKind::Named(b, args)) if self.decls.ty(b).kind == TypeKind::Interface => {
                let name = self.decls.ty(b).name.clone();
                match Iface::from_name(&name) {
                    Some(i) => self.conforms(src, i),
                    None if name == "Iterable" => self
                        .iter_elem(src)
                        .is_some_and(|e| args.first() == Some(&e)),
                    None => false,
                }
            }
            _ => false,
        }
    }

    /// The interface types (with arguments) a named type declares, transitively.
    fn declared_supertypes(&mut self, ty: Ty) -> Vec<Ty> {
        let TyKind::Named(d, _) = self.types.kind(ty).clone() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for t in self.decls.ancestors(d) {
            out.extend(self.decls.ty(t).interfaces.clone());
            out.push(self.types.named(t, Vec::new()));
        }
        out
    }

    /// The element type `foreach` yields (the value, for maps).
    pub fn iter_elem(&self, ty: Ty) -> Option<Ty> {
        match self.types.kind(ty) {
            TyKind::List(e) | TyKind::Set(e) | TyKind::Range(e) => Some(*e),
            TyKind::Map(_, v) => Some(*v),
            TyKind::Bytes => Some(Ty::U8),
            _ => None,
        }
    }

    /// Whether `ty` satisfies an operator interface, built in or declared.
    pub fn conforms(&mut self, ty: Ty, iface: Iface) -> bool {
        self.conforms_inner(ty, iface, &mut Vec::new())
    }

    fn conforms_inner(&mut self, ty: Ty, iface: Iface, visiting: &mut Vec<Ty>) -> bool {
        if visiting.contains(&ty) {
            // Recursive structs: assume yes on the cycle; other fields decide.
            return true;
        }
        let kind = self.types.kind(ty).clone();
        let numeric = matches!(kind, TyKind::Int(_) | TyKind::Float(_));
        match iface {
            Iface::Numeric => return numeric,
            Iface::Integer => return matches!(kind, TyKind::Int(_)),
            _ => {}
        }
        visiting.push(ty);
        let result = match kind {
            TyKind::Error | TyKind::Never => true,
            TyKind::Int(_) => true,
            TyKind::Float(_) => iface != Iface::Hashable,
            TyKind::String => true,
            TyKind::Bytes => iface != Iface::Stringable,
            TyKind::Bool => matches!(iface, Iface::Equatable | Iface::Hashable),
            // `null == null`, and `?T` keys hash: `null` is Equatable and Hashable, so
            // `?int` is too (and a struct with a `?T` field can be).
            TyKind::Null => matches!(iface, Iface::Equatable | Iface::Hashable),
            TyKind::Void | TyKind::Fn(_) | TyKind::Param(_) | TyKind::SelfType => false,
            TyKind::Union(ms) => ms.iter().all(|&m| self.conforms_inner(m, iface, visiting)),
            TyKind::List(e) => {
                iface != Iface::Stringable && self.conforms_inner(e, iface, visiting)
            }
            TyKind::Set(e) => {
                matches!(iface, Iface::Equatable | Iface::Hashable)
                    && self.conforms_inner(e, iface, visiting)
            }
            TyKind::Map(_, v) => {
                matches!(iface, Iface::Equatable | Iface::Hashable)
                    && self.conforms_inner(v, iface, visiting)
            }
            TyKind::Tuple(es) => {
                iface != Iface::Stringable
                    && es.iter().all(|&e| self.conforms_inner(e, iface, visiting))
            }
            TyKind::Range(_) => iface == Iface::Equatable,
            TyKind::Named(d, _) => self.named_conforms(d, iface, visiting),
        };
        visiting.pop();
        result
    }

    fn named_conforms(&mut self, d: DeclId, iface: Iface, visiting: &mut Vec<Ty>) -> bool {
        let explicit = self
            .decls
            .all_interfaces(d)
            .iter()
            .any(|&i| self.decls.ty(i).name == iface.name());
        if explicit {
            return true;
        }
        let decl = self.decls.ty(d);
        match (decl.kind, iface) {
            // SPEC "Auto-derivation": structs are Equatable / Hashable when every field
            // is; never automatically Comparable or Stringable.
            (TypeKind::Struct, Iface::Equatable | Iface::Hashable) => {
                let fields: Vec<Ty> = decl.fields.iter().map(|f| f.ty).collect();
                fields
                    .iter()
                    .all(|&f| self.conforms_inner(f, iface, visiting))
            }
            // Enums: Equatable and Hashable when every payload is (always, for pure and
            // backed enums); Comparable only by opting in (handled above).
            (TypeKind::Enum, Iface::Equatable | Iface::Hashable) => {
                let payloads: Vec<Ty> = decl
                    .cases
                    .iter()
                    .flat_map(|c| c.payload.iter().flatten().map(|p| p.ty))
                    .collect();
                payloads
                    .iter()
                    .all(|&p| self.conforms_inner(p, iface, visiting))
            }
            _ => false,
        }
    }

    /// Infers type parameters by matching a signature type against an actual type:
    /// `fn(T): U` against `fn(int): string` binds `T = int`, `U = string`. Returns
    /// `false` on a structural mismatch (the caller reports it).
    pub fn unify(&mut self, pattern: Ty, actual: Ty, subst: &mut HashMap<ParamId, Ty>) -> bool {
        if pattern == actual || actual == Ty::ERROR {
            return true;
        }
        match (
            self.types.kind(pattern).clone(),
            self.types.kind(actual).clone(),
        ) {
            (TyKind::Param(p), _) => match subst.get(&p) {
                Some(&bound) => bound == actual || self.assignable(actual, bound),
                None => {
                    subst.insert(p, actual);
                    true
                }
            },
            (TyKind::List(a), TyKind::List(b))
            | (TyKind::Set(a), TyKind::Set(b))
            | (TyKind::Range(a), TyKind::Range(b)) => self.unify(a, b, subst),
            (TyKind::Map(ak, av), TyKind::Map(bk, bv)) => {
                self.unify(ak, bk, subst) && self.unify(av, bv, subst)
            }
            (TyKind::Tuple(a), TyKind::Tuple(b)) if a.len() == b.len() => {
                a.iter().zip(&b).all(|(&x, &y)| self.unify(x, y, subst))
            }
            (TyKind::Named(a, aa), TyKind::Named(b, ba)) if a == b => {
                aa.iter().zip(&ba).all(|(&x, &y)| self.unify(x, y, subst))
            }
            (TyKind::Fn(a), TyKind::Fn(b)) if a.params.len() == b.params.len() => {
                a.params
                    .iter()
                    .zip(&b.params)
                    .all(|(x, y)| x.inout == y.inout && self.unify(x.ty, y.ty, subst))
                    && self.unify(a.ret, b.ret, subst)
            }
            (TyKind::Union(ps), _) => {
                // `?T` against `int` or `?int`: bind `T` from the non-null part.
                let non_null: Vec<Ty> = ps.iter().copied().filter(|&p| p != Ty::NULL).collect();
                if non_null.len() == 1 {
                    let a = self.types.strip_null(actual);
                    return self.unify(non_null[0], a, subst);
                }
                true
            }
            _ => true,
        }
    }

    /// A type as written in SCB, for diagnostics: `?int`, `list<string>`, `User`.
    pub fn show(&self, ty: Ty) -> String {
        match self.types.kind(ty) {
            TyKind::Error => "<error>".into(),
            TyKind::Never => "never".into(),
            TyKind::Void => "void".into(),
            TyKind::Null => "null".into(),
            TyKind::Bool => "bool".into(),
            // Users write `int`; `i64` is the same type.
            TyKind::Int(crate::ty::IntTy::I64) => "int".into(),
            TyKind::Int(i) => i.name().into(),
            TyKind::Float(f) => f.name().into(),
            TyKind::String => "string".into(),
            TyKind::Bytes => "bytes".into(),
            TyKind::List(e) => format!("list<{}>", self.show(*e)),
            TyKind::Set(e) => format!("set<{}>", self.show(*e)),
            TyKind::Range(e) => format!("Range<{}>", self.show(*e)),
            TyKind::Map(k, v) => format!("map<{}, {}>", self.show(*k), self.show(*v)),
            TyKind::Tuple(es) => format!(
                "tuple<{}>",
                es.iter()
                    .map(|&e| self.show(e))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            TyKind::Fn(f) => format!(
                "fn({}): {}",
                f.params
                    .iter()
                    .map(|p| format!("{}{}", if p.inout { "inout " } else { "" }, self.show(p.ty)))
                    .collect::<Vec<_>>()
                    .join(", "),
                self.show(f.ret)
            ),
            TyKind::Union(ms) => {
                let non_null: Vec<Ty> = ms.iter().copied().filter(|&m| m != Ty::NULL).collect();
                if non_null.len() == 1 && ms.len() == 2 {
                    format!("?{}", self.show(non_null[0]))
                } else {
                    ms.iter()
                        .map(|&m| self.show(m))
                        .collect::<Vec<_>>()
                        .join("|")
                }
            }
            TyKind::Named(d, args) => {
                let name = self.short_name(*d);
                if args.is_empty() {
                    name
                } else {
                    format!(
                        "{name}<{}>",
                        args.iter()
                            .map(|&a| self.show(a))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            }
            TyKind::Param(p) => self.decls.param(*p).name.clone(),
            TyKind::SelfType => "self".into(),
        }
    }
}
