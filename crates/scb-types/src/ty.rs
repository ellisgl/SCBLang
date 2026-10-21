//! Types.
//!
//! Every type is interned in a [`Types`] table and referred to by a small copyable
//! [`Ty`] handle, so comparing two types is comparing two integers and types can be
//! stored everywhere without cloning trees. Interning also normalizes unions: `int|string`
//! and `string|int` are the same `Ty`.
//!
//! The common types (`int`, `bool`, `string`, …) are pre-interned at fixed indices and
//! available as constants (`Ty::BOOL`, `Ty::I64`, …).

use std::collections::HashMap;

use crate::decls::{DeclId, ParamId};

/// A handle to an interned type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ty(u32);

/// Integer types (SPEC "Numerics").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IntTy {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
}

impl IntTy {
    pub const ALL: [IntTy; 8] = [
        IntTy::I8,
        IntTy::I16,
        IntTy::I32,
        IntTy::I64,
        IntTy::U8,
        IntTy::U16,
        IntTy::U32,
        IntTy::U64,
    ];

    pub fn signed(self) -> bool {
        matches!(self, IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64)
    }

    pub fn bits(self) -> u32 {
        match self {
            IntTy::I8 | IntTy::U8 => 8,
            IntTy::I16 | IntTy::U16 => 16,
            IntTy::I32 | IntTy::U32 => 32,
            IntTy::I64 | IntTy::U64 => 64,
        }
    }

    pub fn min(self) -> i128 {
        if self.signed() {
            -(1i128 << (self.bits() - 1))
        } else {
            0
        }
    }

    pub fn max(self) -> i128 {
        if self.signed() {
            (1i128 << (self.bits() - 1)) - 1
        } else {
            (1i128 << self.bits()) - 1
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            IntTy::I8 => "i8",
            IntTy::I16 => "i16",
            IntTy::I32 => "i32",
            IntTy::I64 => "i64",
            IntTy::U8 => "u8",
            IntTy::U16 => "u16",
            IntTy::U32 => "u32",
            IntTy::U64 => "u64",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FloatTy {
    F32,
    F64,
}

impl FloatTy {
    pub fn name(self) -> &'static str {
        match self {
            FloatTy::F32 => "f32",
            FloatTy::F64 => "f64",
        }
    }
}

/// A function type `fn(inout int, string): bool`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FnTy {
    pub params: Vec<FnParam>,
    pub ret: Ty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FnParam {
    pub inout: bool,
    pub ty: Ty,
}

/// The structure of a type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TyKind {
    /// The type of an expression that already produced a diagnostic. Compatible with
    /// everything, so one mistake does not cascade into many.
    Error,
    Never,
    Void,
    Null,
    Bool,
    Int(IntTy),
    Float(FloatTy),
    String,
    Bytes,
    List(Ty),
    Map(Ty, Ty),
    Set(Ty),
    Tuple(Vec<Ty>),
    Range(Ty),
    Fn(FnTy),
    /// Two or more members, sorted and flattened, without `never` or duplicates.
    /// `?T` is the union `T|null`.
    Union(Vec<Ty>),
    /// A class, struct, interface or enum, with type arguments (only stdlib types such
    /// as `Ref<T>` and `Iterable<T>` have any in the POC).
    Named(DeclId, Vec<Ty>),
    /// A type parameter of a stdlib generic (`T` in `list<T>::push`), replaced by a
    /// concrete type at each use.
    Param(ParamId),
    /// `self` in a stdlib interface signature (`Equatable::equals($other: self)`): the
    /// implementing type.
    SelfType,
}

/// The type interner. Lives in the [`crate::Program`]; immutable after checking.
#[derive(Debug)]
pub struct Types {
    kinds: Vec<TyKind>,
    ids: HashMap<TyKind, Ty>,
}

impl Ty {
    pub const ERROR: Ty = Ty(0);
    pub const NEVER: Ty = Ty(1);
    pub const VOID: Ty = Ty(2);
    pub const NULL: Ty = Ty(3);
    pub const BOOL: Ty = Ty(4);
    pub const I8: Ty = Ty(5);
    pub const I16: Ty = Ty(6);
    pub const I32: Ty = Ty(7);
    pub const I64: Ty = Ty(8);
    pub const U8: Ty = Ty(9);
    pub const U16: Ty = Ty(10);
    pub const U32: Ty = Ty(11);
    pub const U64: Ty = Ty(12);
    pub const F32: Ty = Ty(13);
    pub const F64: Ty = Ty(14);
    pub const STRING: Ty = Ty(15);
    pub const BYTES: Ty = Ty(16);
    pub const SELF: Ty = Ty(17);

    pub fn int(i: IntTy) -> Ty {
        match i {
            IntTy::I8 => Ty::I8,
            IntTy::I16 => Ty::I16,
            IntTy::I32 => Ty::I32,
            IntTy::I64 => Ty::I64,
            IntTy::U8 => Ty::U8,
            IntTy::U16 => Ty::U16,
            IntTy::U32 => Ty::U32,
            IntTy::U64 => Ty::U64,
        }
    }

    pub fn float(f: FloatTy) -> Ty {
        match f {
            FloatTy::F32 => Ty::F32,
            FloatTy::F64 => Ty::F64,
        }
    }

    pub fn index(self) -> usize {
        self.0 as usize
    }
}

impl Default for Types {
    fn default() -> Self {
        Self::new()
    }
}

impl Types {
    pub fn new() -> Self {
        let mut t = Types {
            kinds: Vec::new(),
            ids: HashMap::new(),
        };
        // Order must match the `Ty` constants above.
        let fixed = [
            TyKind::Error,
            TyKind::Never,
            TyKind::Void,
            TyKind::Null,
            TyKind::Bool,
            TyKind::Int(IntTy::I8),
            TyKind::Int(IntTy::I16),
            TyKind::Int(IntTy::I32),
            TyKind::Int(IntTy::I64),
            TyKind::Int(IntTy::U8),
            TyKind::Int(IntTy::U16),
            TyKind::Int(IntTy::U32),
            TyKind::Int(IntTy::U64),
            TyKind::Float(FloatTy::F32),
            TyKind::Float(FloatTy::F64),
            TyKind::String,
            TyKind::Bytes,
            TyKind::SelfType,
        ];
        for k in fixed {
            t.intern_raw(k);
        }
        t
    }

    fn intern_raw(&mut self, kind: TyKind) -> Ty {
        if let Some(&ty) = self.ids.get(&kind) {
            return ty;
        }
        let ty = Ty(self.kinds.len() as u32);
        self.kinds.push(kind.clone());
        self.ids.insert(kind, ty);
        ty
    }

    pub fn kind(&self, ty: Ty) -> &TyKind {
        &self.kinds[ty.index()]
    }

    pub fn list(&mut self, elem: Ty) -> Ty {
        self.intern_raw(TyKind::List(elem))
    }

    pub fn map(&mut self, k: Ty, v: Ty) -> Ty {
        self.intern_raw(TyKind::Map(k, v))
    }

    pub fn set(&mut self, elem: Ty) -> Ty {
        self.intern_raw(TyKind::Set(elem))
    }

    pub fn tuple(&mut self, elems: Vec<Ty>) -> Ty {
        self.intern_raw(TyKind::Tuple(elems))
    }

    pub fn range(&mut self, elem: Ty) -> Ty {
        self.intern_raw(TyKind::Range(elem))
    }

    pub fn func(&mut self, params: Vec<FnParam>, ret: Ty) -> Ty {
        self.intern_raw(TyKind::Fn(FnTy { params, ret }))
    }

    pub fn named(&mut self, decl: DeclId, args: Vec<Ty>) -> Ty {
        self.intern_raw(TyKind::Named(decl, args))
    }

    pub fn param(&mut self, p: ParamId) -> Ty {
        self.intern_raw(TyKind::Param(p))
    }

    /// `T|null`.
    pub fn nullable(&mut self, ty: Ty) -> Ty {
        self.union(&[ty, Ty::NULL])
    }

    /// The union of `members`: nested unions are flattened, `never` and duplicates
    /// dropped, members sorted. A single remaining member is returned as itself; an
    /// empty union is `never`. `Error` absorbs everything.
    pub fn union(&mut self, members: &[Ty]) -> Ty {
        let mut flat = Vec::new();
        for &m in members {
            match self.kind(m) {
                TyKind::Error => return Ty::ERROR,
                TyKind::Never => {}
                TyKind::Union(inner) => flat.extend(inner.iter().copied()),
                _ => flat.push(m),
            }
        }
        flat.sort();
        flat.dedup();
        match flat.len() {
            0 => Ty::NEVER,
            1 => flat[0],
            _ => self.intern_raw(TyKind::Union(flat)),
        }
    }

    /// The members of a union, or the type itself.
    pub fn members(&self, ty: Ty) -> Vec<Ty> {
        match self.kind(ty) {
            TyKind::Union(ms) => ms.clone(),
            _ => vec![ty],
        }
    }

    pub fn is_nullable(&self, ty: Ty) -> bool {
        ty == Ty::NULL || self.members(ty).contains(&Ty::NULL)
    }

    /// `T` for `?T` (removes `null` from a union); other types unchanged.
    pub fn strip_null(&mut self, ty: Ty) -> Ty {
        let rest: Vec<Ty> = self
            .members(ty)
            .into_iter()
            .filter(|&m| m != Ty::NULL)
            .collect();
        self.union(&rest)
    }

    /// `ty` without the members in `remove` (flow typing after a failed type test).
    pub fn without(&mut self, ty: Ty, remove: &[Ty]) -> Ty {
        let rest: Vec<Ty> = self
            .members(ty)
            .into_iter()
            .filter(|m| !remove.contains(m))
            .collect();
        self.union(&rest)
    }

    pub fn as_int(&self, ty: Ty) -> Option<IntTy> {
        match self.kind(ty) {
            TyKind::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub fn as_float(&self, ty: Ty) -> Option<FloatTy> {
        match self.kind(ty) {
            TyKind::Float(f) => Some(*f),
            _ => None,
        }
    }

    pub fn is_numeric(&self, ty: Ty) -> bool {
        matches!(self.kind(ty), TyKind::Int(_) | TyKind::Float(_))
    }

    /// Whether `ty` mentions a type parameter or `self` (a signature not yet
    /// instantiated).
    pub fn has_params(&self, ty: Ty) -> bool {
        match self.kind(ty) {
            TyKind::Param(_) | TyKind::SelfType => true,
            TyKind::List(e) | TyKind::Set(e) | TyKind::Range(e) => self.has_params(*e),
            TyKind::Map(k, v) => self.has_params(*k) || self.has_params(*v),
            TyKind::Tuple(es) | TyKind::Union(es) | TyKind::Named(_, es) => {
                es.iter().any(|&e| self.has_params(e))
            }
            TyKind::Fn(f) => {
                f.params.iter().any(|p| self.has_params(p.ty)) || self.has_params(f.ret)
            }
            _ => false,
        }
    }

    /// Replaces type parameters by `subst` (unknown ones stay) and `self` by `self_ty`.
    pub fn substitute(&mut self, ty: Ty, subst: &HashMap<ParamId, Ty>, self_ty: Option<Ty>) -> Ty {
        if !self.has_params(ty) {
            return ty;
        }
        match self.kind(ty).clone() {
            TyKind::Param(p) => subst.get(&p).copied().unwrap_or(ty),
            TyKind::SelfType => self_ty.unwrap_or(ty),
            TyKind::List(e) => {
                let e = self.substitute(e, subst, self_ty);
                self.list(e)
            }
            TyKind::Set(e) => {
                let e = self.substitute(e, subst, self_ty);
                self.set(e)
            }
            TyKind::Range(e) => {
                let e = self.substitute(e, subst, self_ty);
                self.range(e)
            }
            TyKind::Map(k, v) => {
                let k = self.substitute(k, subst, self_ty);
                let v = self.substitute(v, subst, self_ty);
                self.map(k, v)
            }
            TyKind::Tuple(es) => {
                let es = es
                    .iter()
                    .map(|&e| self.substitute(e, subst, self_ty))
                    .collect();
                self.tuple(es)
            }
            TyKind::Union(es) => {
                let es: Vec<Ty> = es
                    .iter()
                    .map(|&e| self.substitute(e, subst, self_ty))
                    .collect();
                self.union(&es)
            }
            TyKind::Named(d, es) => {
                let es = es
                    .iter()
                    .map(|&e| self.substitute(e, subst, self_ty))
                    .collect();
                self.named(d, es)
            }
            TyKind::Fn(f) => {
                let params = f
                    .params
                    .iter()
                    .map(|p| FnParam {
                        inout: p.inout,
                        ty: self.substitute(p.ty, subst, self_ty),
                    })
                    .collect();
                let ret = self.substitute(f.ret, subst, self_ty);
                self.func(params, ret)
            }
            _ => ty,
        }
    }
}
