//! Declaration tables: every named thing in the program and the stdlib.
//!
//! Declarations are collected in two passes (`collect.rs`): first every name is
//! registered (so declarations can refer to each other in any order, across files),
//! then signatures are resolved to [`Ty`]s. Function bodies are checked afterwards
//! (`check/`), with all signatures known.
//!
//! Types (classes, structs, interfaces, enums, and the stub classes of built-in
//! types), functions and constants live in separate name spaces, as in PHP: `use`,
//! `use function` and `use const` import from different tables.

use std::collections::HashMap;

use scb_syntax::Span;

use crate::tir::FuncId;
use crate::ty::Ty;

/// A class, struct, interface or enum (or the stub class of a built-in type).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeclId(pub u32);

/// A namespace-level function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FnId(pub u32);

/// A method of a type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MethodId(pub u32);

/// A namespace or class constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConstId(pub u32);

/// A static property.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StaticId(pub u32);

/// A type parameter of a stdlib generic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ParamId(pub u32);

/// Index into a type's full field layout (inherited fields first).
pub type FieldIdx = u32;

/// Index into an enum's cases.
pub type CaseIdx = u32;

/// Where a declaration's AST is: file index and declaration index in that file
/// (`member` for class members).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AstRef {
    pub file: u32,
    pub decl: u32,
    pub member: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeKind {
    Class,
    Struct,
    Interface,
    Enum,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Protected,
    Private,
    /// Package-only; the same as `public` within one module in the POC.
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodKind {
    Plain,
    Abstract,
    Final,
}

/// One parameter of a signature.
#[derive(Debug, Clone)]
pub struct ParamSig {
    pub name: String,
    pub ty: Ty,
    pub inout: bool,
    /// `...$xs: T`: the parameter's type is `list<T>`; `ty` holds `T`.
    pub variadic: bool,
    pub has_default: bool,
}

#[derive(Debug, Clone)]
pub struct Sig {
    pub params: Vec<ParamSig>,
    pub ret: Ty,
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub ty: Ty,
    pub visibility: Visibility,
    pub readonly: bool,
    /// Declared in this type (not inherited).
    pub owner: DeclId,
    /// Has a default value or is a promoted constructor parameter: initialized
    /// before the constructor body runs.
    pub initialized: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Case {
    pub name: String,
    /// Payload parameters of a data-carrying case.
    pub payload: Option<Vec<ParamSig>>,
    pub span: Span,
}

#[derive(Debug)]
pub struct TypeDecl {
    /// Fully qualified, without a leading `\`: `Conformance\User`, `list`.
    pub name: String,
    pub kind: TypeKind,
    pub stdlib: bool,
    pub ast: AstRef,
    pub span: Span,
    pub type_params: Vec<ParamId>,
    /// Classes: may be extended (`extendable` or `abstract`).
    pub extendable: bool,
    pub is_abstract: bool,
    pub readonly: bool,
    pub parent: Option<DeclId>,
    /// Implemented (classes, structs, enums) or extended (interfaces) interfaces, as
    /// types (with arguments, e.g. `Iterable<T>`) and as declarations.
    pub interfaces: Vec<Ty>,
    pub interface_ids: Vec<DeclId>,
    /// Full instance layout: inherited fields first, then own fields.
    pub fields: Vec<Field>,
    /// Own methods by name.
    pub methods: HashMap<String, MethodId>,
    pub consts: HashMap<String, ConstId>,
    pub statics: HashMap<String, StaticId>,
    pub cases: Vec<Case>,
    /// Backed enums: `int` or `string`.
    pub backing: Option<Ty>,
    /// For the stub class of a built-in type: the type it describes, with the stub's
    /// type parameters (`list<T>` → `List(Param T)`).
    pub builtin: Option<Ty>,
}

#[derive(Debug)]
pub struct MethodDecl {
    pub name: String,
    pub owner: DeclId,
    pub kind: MethodKind,
    pub visibility: Visibility,
    pub is_static: bool,
    pub mutating: bool,
    pub type_params: Vec<ParamId>,
    /// `where T of Bound`: the method exists only when these hold.
    pub where_bounds: Vec<(ParamId, Ty)>,
    pub sig: Sig,
    /// `false` for abstract and interface methods without a default, and stubs.
    pub has_body: bool,
    pub ast: AstRef,
    pub func: FuncId,
    pub span: Span,
}

#[derive(Debug)]
pub struct FnDecl {
    pub name: String,
    pub stdlib: bool,
    pub type_params: Vec<ParamId>,
    pub sig: Sig,
    pub ast: AstRef,
    pub func: FuncId,
    pub span: Span,
}

#[derive(Debug)]
pub struct ConstDecl {
    pub name: String,
    pub ty: Ty,
    /// Class constants.
    pub owner: Option<DeclId>,
    pub visibility: Visibility,
    pub ast: AstRef,
    pub span: Span,
}

#[derive(Debug)]
pub struct StaticDecl {
    pub name: String,
    pub ty: Ty,
    pub owner: DeclId,
    pub visibility: Visibility,
    pub readonly: bool,
    pub ast: AstRef,
    pub span: Span,
}

#[derive(Debug)]
pub struct TypeParamDecl {
    pub name: String,
    pub bound: Option<Ty>,
}

/// All declarations, with name lookup tables.
#[derive(Debug, Default)]
pub struct Decls {
    pub types: Vec<TypeDecl>,
    pub functions: Vec<FnDecl>,
    pub methods: Vec<MethodDecl>,
    pub consts: Vec<ConstDecl>,
    pub statics: Vec<StaticDecl>,
    pub params: Vec<TypeParamDecl>,
    pub type_names: HashMap<String, DeclId>,
    pub fn_names: HashMap<String, FnId>,
    pub const_names: HashMap<String, ConstId>,
}

impl Decls {
    pub fn ty(&self, id: DeclId) -> &TypeDecl {
        &self.types[id.0 as usize]
    }

    pub fn ty_mut(&mut self, id: DeclId) -> &mut TypeDecl {
        &mut self.types[id.0 as usize]
    }

    pub fn method(&self, id: MethodId) -> &MethodDecl {
        &self.methods[id.0 as usize]
    }

    pub fn function(&self, id: FnId) -> &FnDecl {
        &self.functions[id.0 as usize]
    }

    pub fn konst(&self, id: ConstId) -> &ConstDecl {
        &self.consts[id.0 as usize]
    }

    pub fn static_(&self, id: StaticId) -> &StaticDecl {
        &self.statics[id.0 as usize]
    }

    pub fn param(&self, id: ParamId) -> &TypeParamDecl {
        &self.params[id.0 as usize]
    }

    pub fn type_named(&self, fqn: &str) -> Option<DeclId> {
        self.type_names.get(fqn).copied()
    }

    /// The superclass chain, starting with `id` itself.
    pub fn ancestors(&self, id: DeclId) -> Vec<DeclId> {
        let mut chain = vec![id];
        let mut cur = id;
        while let Some(p) = self.ty(cur).parent {
            if chain.contains(&p) {
                break;
            }
            chain.push(p);
            cur = p;
        }
        chain
    }

    /// Finds a method by name on `id`, its superclasses, then (for default methods)
    /// its interfaces, breadth first. Returns the declaring method.
    pub fn find_method(&self, id: DeclId, name: &str) -> Option<MethodId> {
        for t in self.ancestors(id) {
            if let Some(&m) = self.ty(t).methods.get(name) {
                return Some(m);
            }
        }
        let mut seen = Vec::new();
        let mut queue: Vec<DeclId> = self
            .ancestors(id)
            .iter()
            .flat_map(|&t| self.interface_decls(t))
            .collect();
        while let Some(i) = queue.first().copied() {
            queue.remove(0);
            if seen.contains(&i) {
                continue;
            }
            seen.push(i);
            if let Some(&m) = self.ty(i).methods.get(name) {
                return Some(m);
            }
            queue.extend(self.interface_decls(i));
        }
        None
    }

    /// Declarations of the interfaces `id` lists directly.
    pub fn interface_decls(&self, id: DeclId) -> Vec<DeclId> {
        self.ty(id).interface_ids.clone()
    }

    /// Every interface `id` implements: directly, through superclasses, and through
    /// interfaces extending other interfaces.
    pub fn all_interfaces(&self, id: DeclId) -> Vec<DeclId> {
        let mut out: Vec<DeclId> = Vec::new();
        let mut queue: Vec<DeclId> = self
            .ancestors(id)
            .iter()
            .flat_map(|&t| self.interface_decls(t))
            .collect();
        while let Some(i) = queue.pop() {
            if !out.contains(&i) {
                out.push(i);
                queue.extend(self.interface_decls(i));
            }
        }
        out
    }

    /// Whether `sub` is `sup`, a subclass of it, or implements it.
    pub fn is_subtype(&self, sub: DeclId, sup: DeclId) -> bool {
        sub == sup || self.ancestors(sub).contains(&sup) || self.all_interfaces(sub).contains(&sup)
    }

    /// A constant of `id`, its superclasses, or the interfaces it implements.
    pub fn find_const(&self, id: DeclId, name: &str) -> Option<ConstId> {
        self.ancestors(id)
            .into_iter()
            .chain(self.all_interfaces(id))
            .find_map(|t| self.ty(t).consts.get(name).copied())
    }

    pub fn find_static(&self, id: DeclId, name: &str) -> Option<StaticId> {
        self.ancestors(id)
            .into_iter()
            .find_map(|t| self.ty(t).statics.get(name).copied())
    }

    pub fn field_index(&self, id: DeclId, name: &str) -> Option<FieldIdx> {
        self.ty(id)
            .fields
            .iter()
            .position(|f| f.name == name)
            .map(|i| i as FieldIdx)
    }

    /// The constructor that `new id(…)` runs: own, else inherited.
    pub fn constructor(&self, id: DeclId) -> Option<MethodId> {
        self.ancestors(id)
            .into_iter()
            .find_map(|t| self.ty(t).methods.get("__construct").copied())
    }
}
