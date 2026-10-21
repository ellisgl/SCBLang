//! Name resolution (SPEC "Modules and packages") and resolving AST types to [`Ty`].
//!
//! Rules, PHP-style:
//! - `\A\B` is fully qualified.
//! - A single name `X` is looked up as a `use` alias, then in the file's namespace
//!   (`App\X`), then in the prelude (the root namespace's stdlib names). There is no
//!   other global fallback.
//! - A qualified name `A\B` whose first part is a `use` alias expands the alias;
//!   otherwise it is relative to the file's namespace (`App\A\B`). `Env\get()` in
//!   `namespace App;` therefore means `App\Env\get`, which usually does not exist.
//! - Types, functions and constants are looked up in separate tables.

use std::collections::HashMap;

use scb_syntax::ast;
use scb_syntax::{Diagnostic, Span};

use crate::Ctx;
use crate::decls::{ConstId, DeclId, FnId, ParamId};
use crate::ty::{FnParam, Ty, TyKind};

/// A parsed file with its resolution scope.
#[derive(Debug)]
pub struct ParsedFile {
    pub ast: ast::File,
    /// `App\Models`, or `""` for the prelude stubs (root namespace).
    pub namespace: String,
    pub stdlib: bool,
    pub uses: Uses,
    /// `main.scb` in directory mode, or the only file in single-file mode.
    pub main_file: bool,
    /// For `__FILE__` / `__DIR__`.
    pub path: String,
}

/// A file's `use` imports: alias → fully qualified name, one table per kind.
#[derive(Debug, Default)]
pub struct Uses {
    pub types: HashMap<String, String>,
    pub functions: HashMap<String, String>,
    pub consts: HashMap<String, String>,
}

/// Which kind of name is being looked up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameKind {
    Type,
    Function,
    Const,
}

/// Type parameters in scope while resolving a stdlib signature, and the enclosing
/// type (for `self`).
#[derive(Debug, Clone, Default)]
pub struct TyScope {
    pub params: Vec<(String, ParamId)>,
    pub self_decl: Option<DeclId>,
}

/// Names of the built-in types that are not declarations (SPEC "Lexical structure"
/// prelude list); `int` and `float` are aliases.
pub fn builtin_scalar(name: &str) -> Option<Ty> {
    Some(match name {
        "bool" => Ty::BOOL,
        "int" | "i64" => Ty::I64,
        "i8" => Ty::I8,
        "i16" => Ty::I16,
        "i32" => Ty::I32,
        "u8" => Ty::U8,
        "u16" => Ty::U16,
        "u32" => Ty::U32,
        "u64" => Ty::U64,
        "float" | "f64" => Ty::F64,
        "f32" => Ty::F32,
        "string" => Ty::STRING,
        "bytes" => Ty::BYTES,
        _ => return None,
    })
}

pub fn qname_text(n: &ast::QualifiedName) -> String {
    n.parts
        .iter()
        .map(|p| &*p.name)
        .collect::<Vec<_>>()
        .join("\\")
}

impl Ctx<'_> {
    /// Candidate fully qualified names for `name` written in `file`, most specific
    /// first.
    fn candidates(&self, file: usize, name: &ast::QualifiedName, kind: NameKind) -> Vec<String> {
        let text = qname_text(name);
        if name.absolute {
            return vec![text];
        }
        let f = &self.files[file];
        let uses = match kind {
            NameKind::Type => &f.uses.types,
            NameKind::Function => &f.uses.functions,
            NameKind::Const => &f.uses.consts,
        };
        let first = &*name.parts[0].name;
        let mut out = Vec::new();
        if name.parts.len() == 1 {
            if let Some(full) = uses.get(first) {
                out.push(full.clone());
            }
        } else if let Some(full) = f.uses.types.get(first) {
            // `use App\Models;` then `Models\User`.
            out.push(format!("{full}\\{}", &text[first.len() + 1..]));
        }
        out.push(qualify(&f.namespace, &text));
        if name.parts.len() == 1 && (f.stdlib || !first.starts_with("__")) {
            out.push(text);
        }
        out
    }

    pub fn lookup_type(&self, file: usize, name: &ast::QualifiedName) -> Option<DeclId> {
        self.candidates(file, name, NameKind::Type)
            .iter()
            .find_map(|c| self.decls.type_names.get(c).copied())
    }

    pub fn lookup_function(&self, file: usize, name: &ast::QualifiedName) -> Option<FnId> {
        self.candidates(file, name, NameKind::Function)
            .iter()
            .find_map(|c| self.decls.fn_names.get(c).copied())
    }

    pub fn lookup_const(&self, file: usize, name: &ast::QualifiedName) -> Option<ConstId> {
        self.candidates(file, name, NameKind::Const)
            .iter()
            .find_map(|c| self.decls.const_names.get(c).copied())
    }

    pub fn unresolved(&mut self, span: Span, what: &str, name: &ast::QualifiedName, file: usize) {
        let text = qname_text(name);
        let mut d =
            Diagnostic::new("unresolved-name", format!("unknown {what} `{text}`")).with_span(span);
        if !name.absolute && name.parts.len() > 1 {
            let ns = &self.files[file].namespace;
            d = d
                .with_note(format!(
                    "a qualified name without a leading `\\` is relative to the current namespace: `{}`",
                    qualify(ns, &text)
                ))
                .with_fix(format!("write `\\{text}` for the fully qualified name"));
        }
        self.report(d);
    }

    /// Resolves an AST type. Unknown names are reported and become `Ty::ERROR`.
    pub fn resolve_type(&mut self, file: usize, t: &ast::Type, scope: &TyScope) -> Ty {
        match &t.kind {
            ast::TypeKind::Void => Ty::VOID,
            ast::TypeKind::Never => Ty::NEVER,
            ast::TypeKind::Null => Ty::NULL,
            ast::TypeKind::Error => Ty::ERROR,
            ast::TypeKind::SelfType => match scope.self_decl {
                Some(d)
                    if self.decls.ty(d).kind == crate::decls::TypeKind::Interface
                        && self.decls.ty(d).stdlib =>
                {
                    Ty::SELF
                }
                Some(d) => self.self_type(d),
                None => {
                    self.report(
                        Diagnostic::new("unresolved-name", "`self` outside a class")
                            .with_span(t.span),
                    );
                    Ty::ERROR
                }
            },
            ast::TypeKind::Nullable(inner) => {
                let inner = self.resolve_type(file, inner, scope);
                self.types.nullable(inner)
            }
            ast::TypeKind::Union(members) => {
                let ms: Vec<Ty> = members
                    .iter()
                    .map(|m| self.resolve_type(file, m, scope))
                    .collect();
                self.types.union(&ms)
            }
            ast::TypeKind::Fn { params, ret } => {
                let params = params
                    .iter()
                    .map(|p| FnParam {
                        inout: p.inout,
                        ty: self.resolve_type(file, &p.ty, scope),
                    })
                    .collect();
                let ret = self.resolve_type(file, ret, scope);
                self.types.func(params, ret)
            }
            ast::TypeKind::Named(n) => self.resolve_named(file, n, scope),
        }
    }

    /// The type of `self` / `$this` inside declaration `d` (with its own type
    /// parameters as arguments for stdlib generics).
    pub fn self_type(&mut self, d: DeclId) -> Ty {
        if let Some(b) = self.decls.ty(d).builtin {
            return b;
        }
        let params: Vec<ParamId> = self.decls.ty(d).type_params.clone();
        let args = params.into_iter().map(|p| self.types.param(p)).collect();
        self.types.named(d, args)
    }

    fn resolve_named(&mut self, file: usize, n: &ast::NamedType, scope: &TyScope) -> Ty {
        let args: Vec<Ty> = n
            .args
            .iter()
            .map(|a| self.resolve_type(file, a, scope))
            .collect();
        let name = &n.name;
        if !name.absolute && name.parts.len() == 1 {
            let short = &*name.parts[0].name;
            if let Some((_, p)) = scope.params.iter().find(|(pn, _)| pn == short) {
                let p = self.types_param(*p);
                return self.no_args(n, &args, p);
            }
        }
        // Built-in types are prelude names: `\int` and `int` both work.
        if name.parts.len() == 1 {
            let short = &*name.parts[0].name;
            if self
                .lookup_type(file, name)
                .is_none_or(|d| self.decls.ty(d).builtin.is_some())
            {
                if let Some(ty) = builtin_scalar(short) {
                    return self.no_args(n, &args, ty);
                }
                if let Some(ty) = self.builtin_generic(n, short, &args) {
                    return ty;
                }
            }
        }
        let Some(decl) = self.lookup_type(file, name) else {
            self.unresolved(name.span, "type", name, file);
            return Ty::ERROR;
        };
        let expected = self.decls.ty(decl).type_params.len();
        if args.len() != expected {
            self.report(
                Diagnostic::new(
                    "wrong-type-arguments",
                    format!(
                        "`{}` takes {expected} type argument(s), {} given",
                        qname_text(name),
                        args.len()
                    ),
                )
                .with_span(n.span),
            );
            return Ty::ERROR;
        }
        self.types.named(decl, args)
    }

    fn types_param(&mut self, p: ParamId) -> Ty {
        self.types.param(p)
    }

    fn no_args(&mut self, n: &ast::NamedType, args: &[Ty], ty: Ty) -> Ty {
        if args.is_empty() {
            return ty;
        }
        self.report(
            Diagnostic::new(
                "wrong-type-arguments",
                format!("`{}` takes no type arguments", qname_text(&n.name)),
            )
            .with_span(n.span),
        );
        Ty::ERROR
    }

    /// `list<T>`, `map<K, V>`, `set<T>`, `tuple<…>`, `Range<T>`.
    fn builtin_generic(&mut self, n: &ast::NamedType, short: &str, args: &[Ty]) -> Option<Ty> {
        let arity = match short {
            "list" | "set" | "Range" => 1,
            "map" => 2,
            "tuple" => usize::MAX,
            _ => return None,
        };
        let ok = if arity == usize::MAX {
            args.len() >= 2
        } else {
            args.len() == arity
        };
        if !ok {
            let want = if arity == usize::MAX {
                "two or more".to_string()
            } else {
                arity.to_string()
            };
            self.report(
                Diagnostic::new(
                    "wrong-type-arguments",
                    format!(
                        "`{short}` takes {want} type argument(s), {} given",
                        args.len()
                    ),
                )
                .with_span(n.span),
            );
            return Some(Ty::ERROR);
        }
        // Map keys and set elements must be `Hashable`; checked once every struct's
        // fields are known (`check_bodies`), since derivation depends on them.
        // Stub signatures (`map<K, V>` with `K` a parameter) are checked at use.
        if matches!(short, "map" | "set") && !self.types.has_params(args[0]) {
            self.pending_hashable.push((args[0], n.span));
        }
        Some(match short {
            "list" => self.types.list(args[0]),
            "set" => self.types.set(args[0]),
            "Range" => self.types.range(args[0]),
            "map" => self.types.map(args[0], args[1]),
            _ => self.types.tuple(args.to_vec()),
        })
    }

    /// The stub class that holds the methods and statics of a type: user and stdlib
    /// declarations themselves, `list` for `list<int>`, `i8` for `i8`, ….
    pub fn methods_decl(&self, ty: Ty) -> Option<DeclId> {
        let name = match self.types.kind(ty) {
            TyKind::Named(d, _) => return Some(*d),
            TyKind::Bool => "bool",
            TyKind::Int(i) => i.name(),
            TyKind::Float(f) => f.name(),
            TyKind::String => "string",
            TyKind::Bytes => "bytes",
            TyKind::List(_) => "list",
            TyKind::Map(..) => "map",
            TyKind::Set(_) => "set",
            TyKind::Range(_) => "Range",
            _ => return None,
        };
        self.decls.type_names.get(name).copied()
    }
}

/// `ns\name`, or `name` in the root namespace.
pub fn qualify(ns: &str, name: &str) -> String {
    if ns.is_empty() {
        name.to_string()
    } else {
        format!("{ns}\\{name}")
    }
}
