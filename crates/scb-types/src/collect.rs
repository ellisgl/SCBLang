//! Declaration collection: the passes that run before any function body is checked.
//!
//! 1. `register`: every type, function and constant name of every file (stubs
//!    first) goes into the name tables, with checks on namespaces and duplicate
//!    names. Nothing is resolved yet, so declarations may refer to each other freely.
//! 2. `headers`: type parameters, parents and interfaces of every type.
//! 3. `members`: fields, methods, constants, static properties and enum cases, with
//!    resolved signatures; field layouts include inherited fields.
//! 4. `validate`: class-level rules (`final`, overrides, abstract methods,
//!    interface conformance, implicit constructors).

use std::collections::HashMap;

use scb_syntax::ast::{self, Decl, Member};
use scb_syntax::{Diagnostic, Span};

use crate::Ctx;
use crate::decls::*;
use crate::resolve::{TyScope, qname_text, qualify};
use crate::stdlib::RESERVED_ROOTS;
use crate::tir::FuncId;
use crate::ty::Ty;

/// Names of the prelude (SPEC "Lexical structure"): user code may not declare a type,
/// function or constant with one of these names (`prelude-name-redeclared`).
pub const PRELUDE_NAMES: &[&str] = &[
    "bool",
    "int",
    "i8",
    "i16",
    "i32",
    "i64",
    "u8",
    "u16",
    "u32",
    "u64",
    "float",
    "f32",
    "f64",
    "string",
    "bytes",
    "list",
    "map",
    "set",
    "tuple",
    "Range",
    "Generator",
    "Iterable",
    "Stringable",
    "Equatable",
    "Comparable",
    "Hashable",
    "Callable",
    "Sendable",
    "Throwable",
    "Error",
    "Exception",
    "OverflowError",
    "CastError",
    "DivisionByZeroError",
    "IndexError",
    "KeyError",
    "BoundaryError",
    "AssertionError",
    "ExclusivityError",
    "StaticInitError",
    "ValueError",
    "IoException",
    "ParseException",
    "EncodingException",
    "Ref",
];

fn vis(v: ast::Visibility) -> Visibility {
    match v {
        ast::Visibility::Public => Visibility::Public,
        ast::Visibility::Protected => Visibility::Protected,
        ast::Visibility::Private => Visibility::Private,
        ast::Visibility::Internal => Visibility::Internal,
    }
}

impl Ctx<'_> {
    fn next_func(&mut self) -> FuncId {
        let id = FuncId(self.funcs.len() as u32);
        self.funcs.push(None);
        id
    }

    /// Pass 1: names.
    pub(crate) fn register(&mut self) {
        let files = self.files;
        let user_namespaces: Vec<&str> = files
            .iter()
            .filter(|f| !f.stdlib)
            .map(|f| f.namespace.as_str())
            .collect();
        if let Some(first) = user_namespaces.first()
            && let Some((i, other)) = files
                .iter()
                .enumerate()
                .filter(|(_, f)| !f.stdlib)
                .find(|(_, f)| f.namespace != *first)
        {
            let _ = i;
            self.report(
                Diagnostic::new(
                    "namespace-mismatch",
                    format!(
                        "every file in a directory declares the same namespace: `{}` here, `{first}` elsewhere",
                        other.namespace
                    ),
                )
                .with_span(other.ast.namespace.span),
            );
        }

        for (fi, f) in files.iter().enumerate() {
            // Each alias names one import per kind (`use A\X; use B\X;` is ambiguous).
            let mut aliases: Vec<(ast::UseKind, String)> = Vec::new();
            for u in &f.ast.uses {
                let alias = u.alias.as_ref().map_or_else(
                    || {
                        u.name
                            .parts
                            .last()
                            .map_or(String::new(), |p| p.name.to_string())
                    },
                    |a| a.name.to_string(),
                );
                if aliases.contains(&(u.kind, alias.clone())) {
                    self.report(
                        Diagnostic::new(
                            "duplicate-declaration",
                            format!("`{alias}` is imported twice"),
                        )
                        .with_span(u.span)
                        .with_fix("import one of them under another name with `as`"),
                    );
                }
                aliases.push((u.kind, alias));
            }
            if !f.stdlib {
                let root = f.namespace.split('\\').next().unwrap_or("");
                if RESERVED_ROOTS.contains(&root) {
                    self.report(
                        Diagnostic::new(
                            "reserved-namespace",
                            format!("`{root}` is a stdlib namespace"),
                        )
                        .with_span(f.ast.namespace.span)
                        .with_note("user code may not declare a stdlib namespace or one under it"),
                    );
                }
            }
            for (di, d) in f.ast.decls.iter().enumerate() {
                let at = AstRef {
                    file: fi as u32,
                    decl: di as u32,
                    member: None,
                };
                match d {
                    Decl::Function(fun) => {
                        let fqn = qualify(&f.namespace, &fun.name.name);
                        if self.check_name(f.stdlib, &fun.name, "function") {
                            let func = self.next_func();
                            let id = FnId(self.decls.functions.len() as u32);
                            self.decls.functions.push(FnDecl {
                                name: fqn.clone(),
                                stdlib: f.stdlib,
                                type_params: Vec::new(),
                                sig: Sig {
                                    params: Vec::new(),
                                    ret: Ty::ERROR,
                                },
                                ast: at,
                                func,
                                span: fun.name.span,
                            });
                            self.declare_unique(NameSpace::Fn(id), fqn, fun.name.span);
                        }
                    }
                    Decl::Const(c) => {
                        let fqn = qualify(&f.namespace, &c.name.name);
                        if self.check_name(f.stdlib, &c.name, "constant") {
                            let id = ConstId(self.decls.consts.len() as u32);
                            self.decls.consts.push(ConstDecl {
                                name: fqn.clone(),
                                ty: Ty::ERROR,
                                owner: None,
                                visibility: Visibility::Public,
                                ast: at,
                                span: c.name.span,
                            });
                            self.declare_unique(NameSpace::Const(id), fqn, c.name.span);
                        }
                    }
                    _ => {
                        let (name, kind) = type_name(d);
                        let fqn = qualify(&f.namespace, &name.name);
                        if !self.check_name(f.stdlib, name, "type") {
                            continue;
                        }
                        let id = DeclId(self.decls.types.len() as u32);
                        self.decls.types.push(TypeDecl {
                            name: fqn.clone(),
                            kind,
                            stdlib: f.stdlib,
                            ast: at,
                            span: name.span,
                            type_params: Vec::new(),
                            extendable: false,
                            is_abstract: false,
                            readonly: false,
                            parent: None,
                            interfaces: Vec::new(),
                            interface_ids: Vec::new(),
                            fields: Vec::new(),
                            methods: HashMap::new(),
                            consts: HashMap::new(),
                            statics: HashMap::new(),
                            cases: Vec::new(),
                            backing: None,
                            builtin: None,
                        });
                        self.declare_unique(NameSpace::Type(id), fqn, name.span);
                    }
                }
            }
        }
    }

    /// `prelude-name-redeclared` for user declarations named like a prelude name.
    fn check_name(&mut self, stdlib: bool, name: &ast::Ident, what: &str) -> bool {
        if !stdlib && PRELUDE_NAMES.contains(&&*name.name) {
            self.report(
                Diagnostic::new(
                    "prelude-name-redeclared",
                    format!(
                        "`{}` is a prelude name and cannot be declared as a {what}",
                        name.name
                    ),
                )
                .with_span(name.span)
                .with_note("prelude names are visible everywhere; pick another name"),
            );
            return false;
        }
        true
    }

    fn declare_unique(&mut self, item: NameSpace, fqn: String, span: Span) {
        let taken = match item {
            NameSpace::Type(id) => self.decls.type_names.insert(fqn.clone(), id).is_some(),
            NameSpace::Fn(id) => self.decls.fn_names.insert(fqn.clone(), id).is_some(),
            NameSpace::Const(id) => self.decls.const_names.insert(fqn.clone(), id).is_some(),
        };
        if taken {
            self.report(
                Diagnostic::new(
                    "duplicate-declaration",
                    format!("`{fqn}` is declared twice"),
                )
                .with_span(span),
            );
        }
    }

    /// A stdlib-generic's type parameters, registered as `ParamId`s.
    fn type_params(
        &mut self,
        file: usize,
        tps: &[ast::TypeParam],
        outer: &TyScope,
    ) -> (Vec<ParamId>, TyScope) {
        let mut scope = outer.clone();
        let mut ids = Vec::new();
        for tp in tps {
            let id = ParamId(self.decls.params.len() as u32);
            self.decls.params.push(TypeParamDecl {
                name: tp.name.name.to_string(),
                bound: None,
            });
            scope.params.push((tp.name.name.to_string(), id));
            ids.push(id);
        }
        // Bounds may mention the parameters themselves.
        for (tp, &id) in tps.iter().zip(&ids) {
            if let Some(b) = &tp.bound {
                let bound = self.resolve_type(file, b, &scope);
                self.decls.params[id.0 as usize].bound = Some(bound);
            }
        }
        (ids, scope)
    }

    /// Pass 2: type parameters, parents and interfaces.
    pub(crate) fn headers(&mut self) {
        let files = self.files;
        for idx in 0..self.decls.types.len() {
            let id = DeclId(idx as u32);
            let at = self.decls.ty(id).ast;
            let file = at.file as usize;
            let d = &files[file].ast.decls[at.decl as usize];
            let tps: &[ast::TypeParam] = match d {
                Decl::Class(c) => &c.type_params,
                Decl::Struct(s) => &s.type_params,
                Decl::Interface(i) => &i.type_params,
                Decl::Enum(e) => &e.type_params,
                _ => unreachable!("only types are in the type table"),
            };
            let base = TyScope {
                params: Vec::new(),
                self_decl: Some(id),
            };
            let (params, _) = self.type_params(file, tps, &base);
            let decl = self.decls.ty_mut(id);
            decl.type_params = params;
            if let Decl::Class(c) = d {
                decl.extendable = c.kind != ast::ClassKind::Plain;
                decl.is_abstract = c.kind == ast::ClassKind::Abstract;
                decl.readonly = c.readonly;
            }
            if let Decl::Struct(s) = d {
                decl.readonly = s.readonly;
            }
        }
        // Built-in names must resolve to their types (not their stub classes) before
        // any parent or interface type is resolved.
        self.mark_builtins();
        for idx in 0..self.decls.types.len() {
            let id = DeclId(idx as u32);
            let at = self.decls.ty(id).ast;
            let file = at.file as usize;
            let (extends, implements): (Vec<&ast::NamedType>, &[ast::NamedType]) =
                match &files[file].ast.decls[at.decl as usize] {
                    Decl::Class(c) => (c.extends.iter().collect(), &c.implements),
                    Decl::Struct(s) => (Vec::new(), &s.implements),
                    Decl::Interface(i) => (Vec::new(), &i.extends),
                    Decl::Enum(e) => (Vec::new(), &e.implements),
                    _ => unreachable!("only types are in the type table"),
                };
            let scope = TyScope {
                params: self
                    .decls
                    .ty(id)
                    .type_params
                    .iter()
                    .map(|&p| (self.decls.param(p).name.clone(), p))
                    .collect(),
                self_decl: Some(id),
            };
            for e in extends {
                let ty = self.resolve_named_type(file, e, &scope);
                match self.class_of(ty) {
                    Some(p) if self.decls.ty(p).kind == TypeKind::Class => {
                        self.decls.ty_mut(id).parent = Some(p);
                    }
                    Some(_) => self.report(
                        Diagnostic::new("type-mismatch", "a class can only extend a class")
                            .with_span(e.span)
                            .with_fix("use `implements` for interfaces"),
                    ),
                    None => {}
                }
            }
            for i in implements {
                let ty = self.resolve_named_type(file, i, &scope);
                match self.class_of(ty) {
                    Some(iface) if self.decls.ty(iface).kind == TypeKind::Interface => {
                        let decl = self.decls.ty_mut(id);
                        decl.interfaces.push(ty);
                        decl.interface_ids.push(iface);
                    }
                    Some(_) => self.report(
                        Diagnostic::new("type-mismatch", "only interfaces can be implemented")
                            .with_span(i.span),
                    ),
                    None => {}
                }
            }
        }
        self.check_hierarchy();
    }

    fn resolve_named_type(&mut self, file: usize, n: &ast::NamedType, scope: &TyScope) -> Ty {
        let t = ast::Type {
            kind: ast::TypeKind::Named(n.clone()),
            span: n.span,
        };
        self.resolve_type(file, &t, scope)
    }

    /// The declaration of a named type, if `ty` is one.
    pub fn class_of(&self, ty: Ty) -> Option<DeclId> {
        match self.types.kind(ty) {
            crate::ty::TyKind::Named(d, _) => Some(*d),
            _ => None,
        }
    }

    /// Links the stub classes of built-in types to the types they describe.
    fn mark_builtins(&mut self) {
        for idx in 0..self.decls.types.len() {
            let id = DeclId(idx as u32);
            if !self.decls.ty(id).stdlib || self.decls.ty(id).name.contains('\\') {
                continue;
            }
            let params: Vec<Ty> = self
                .decls
                .ty(id)
                .type_params
                .clone()
                .into_iter()
                .map(|p| self.types.param(p))
                .collect();
            let name = self.decls.ty(id).name.clone();
            let ty = match name.as_str() {
                "list" => self.types.list(params[0]),
                "set" => self.types.set(params[0]),
                "Range" => self.types.range(params[0]),
                "map" => self.types.map(params[0], params[1]),
                other => match crate::resolve::builtin_scalar(other) {
                    Some(t) => t,
                    None => continue,
                },
            };
            self.decls.ty_mut(id).builtin = Some(ty);
        }
    }

    /// Class rules that need only the headers: cycles, extending a final class,
    /// extending `Throwable` directly.
    fn check_hierarchy(&mut self) {
        for idx in 0..self.decls.types.len() {
            let id = DeclId(idx as u32);
            let decl = self.decls.ty(id);
            if decl.stdlib {
                continue;
            }
            let span = decl.span;
            let Some(parent) = decl.parent else { continue };
            let mut seen = vec![id];
            let mut cur = Some(parent);
            while let Some(c) = cur {
                if seen.contains(&c) {
                    self.report(
                        Diagnostic::new("inheritance-cycle", "a class cannot extend itself")
                            .with_span(span),
                    );
                    self.decls.ty_mut(id).parent = None;
                    break;
                }
                seen.push(c);
                cur = self.decls.ty(c).parent;
            }
            let p = self.decls.ty(parent);
            if p.name == "Throwable" {
                self.report(
                    Diagnostic::new(
                        "type-mismatch",
                        "user classes cannot extend `Throwable` directly",
                    )
                    .with_span(span)
                    .with_fix(
                        "extend `Error` (programming faults) or `Exception` (expected failures)",
                    ),
                );
            } else if !p.extendable {
                let pname = p.name.clone();
                self.report(
                    Diagnostic::new(
                        "extend-final",
                        format!("`{pname}` is final and cannot be extended"),
                    )
                    .with_span(span)
                    .with_note("classes are final unless declared `extendable` or `abstract`"),
                );
            }
        }
    }

    /// Pass 3: members.
    pub(crate) fn members(&mut self) {
        // Parents before children, so inherited fields are known.
        let mut order: Vec<DeclId> = (0..self.decls.types.len() as u32).map(DeclId).collect();
        order.sort_by_key(|&d| self.decls.ancestors(d).len());
        for id in order {
            self.collect_members(id);
        }
        for idx in 0..self.decls.functions.len() {
            self.function_signature(FnId(idx as u32));
        }
        for idx in 0..self.decls.consts.len() {
            let c = ConstId(idx as u32);
            if self.decls.konst(c).owner.is_none() {
                let at = self.decls.konst(c).ast;
                let Decl::Const(ast_c) = &self.files[at.file as usize].ast.decls[at.decl as usize]
                else {
                    unreachable!("namespace constants are Decl::Const")
                };
                let ty = self.resolve_type(at.file as usize, &ast_c.ty, &TyScope::default());
                self.decls.consts[idx].ty = ty;
            }
        }
    }

    fn function_signature(&mut self, id: FnId) {
        let at = self.decls.function(id).ast;
        let file = at.file as usize;
        let Decl::Function(f) = &self.files[file].ast.decls[at.decl as usize] else {
            unreachable!("functions are Decl::Function")
        };
        let (params, scope) = self.type_params(file, &f.type_params, &TyScope::default());
        let sig = self.signature(file, &f.params, Some(&f.ret), &scope);
        let decl = &mut self.decls.functions[id.0 as usize];
        decl.type_params = params;
        decl.sig = sig;
    }

    /// Resolves a parameter list and return type.
    pub(crate) fn signature(
        &mut self,
        file: usize,
        params: &[ast::Param],
        ret: Option<&ast::Type>,
        scope: &TyScope,
    ) -> Sig {
        let params = params
            .iter()
            .map(|p| ParamSig {
                name: p.name.name.to_string(),
                ty: self.resolve_type(file, &p.ty, scope),
                inout: p.inout,
                variadic: p.variadic,
                has_default: p.default.is_some(),
            })
            .collect();
        let ret = ret.map_or(Ty::VOID, |r| self.resolve_type(file, r, scope));
        Sig { params, ret }
    }

    fn collect_members(&mut self, id: DeclId) {
        let files = self.files;
        let at = self.decls.ty(id).ast;
        let file = at.file as usize;
        let stdlib = files[file].stdlib;
        let d = &files[file].ast.decls[at.decl as usize];
        let (members, cases): (&[Member], &[ast::EnumCase]) = match d {
            Decl::Class(c) => (&c.members, &[]),
            Decl::Struct(s) => (&s.members, &[]),
            Decl::Interface(i) => (&i.members, &[]),
            Decl::Enum(e) => (&e.members, &e.cases),
            _ => unreachable!(),
        };
        let scope = TyScope {
            params: self
                .decls
                .ty(id)
                .type_params
                .iter()
                .map(|&p| (self.decls.param(p).name.clone(), p))
                .collect(),
            self_decl: Some(id),
        };

        // Inherited layout first.
        let mut fields = match self.decls.ty(id).parent {
            Some(p) => self.decls.ty(p).fields.clone(),
            None => Vec::new(),
        };

        if let Decl::Enum(e) = d {
            if let Some(b) = &e.backing {
                let ty = self.resolve_type(file, b, &scope);
                if ty != Ty::I64 && ty != Ty::STRING && ty != Ty::ERROR {
                    self.report(
                        Diagnostic::new(
                            "type-mismatch",
                            "a backed enum is backed by `int` or `string`",
                        )
                        .with_span(b.span),
                    );
                }
                self.decls.ty_mut(id).backing = Some(ty);
            }
            let mut out = Vec::new();
            for c in cases {
                if out.iter().any(|x: &Case| x.name == *c.name.name) {
                    self.report(
                        Diagnostic::new(
                            "duplicate-declaration",
                            format!("case `{}` is declared twice", c.name.name),
                        )
                        .with_span(c.name.span),
                    );
                    continue;
                }
                let payload = match &c.body {
                    ast::CaseBody::Data(ps) => {
                        if e.backing.is_some() {
                            self.report(
                                Diagnostic::new(
                                    "type-mismatch",
                                    "an enum is backed or carries data, never both",
                                )
                                .with_span(c.span),
                            );
                        }
                        Some(self.signature(file, ps, None, &scope).params)
                    }
                    ast::CaseBody::Value(_) if e.backing.is_none() => {
                        self.report(
                            Diagnostic::new(
                                "type-mismatch",
                                "only a backed enum gives cases values",
                            )
                            .with_span(c.span)
                            .with_fix("declare the backing type: `enum Name: string`"),
                        );
                        None
                    }
                    ast::CaseBody::Unit if e.backing.is_some() => {
                        self.report(
                            Diagnostic::new(
                                "type-mismatch",
                                "every case of a backed enum needs a value",
                            )
                            .with_span(c.span),
                        );
                        None
                    }
                    _ => None,
                };
                out.push(Case {
                    name: c.name.name.to_string(),
                    payload,
                    span: c.name.span,
                });
            }
            self.decls.ty_mut(id).cases = out;
        }

        for (mi, m) in members.iter().enumerate() {
            let mat = AstRef {
                member: Some(mi as u32),
                ..at
            };
            match m {
                Member::Property(p) => {
                    let kind = self.decls.ty(id).kind;
                    if matches!(kind, TypeKind::Interface | TypeKind::Enum) {
                        let what = if kind == TypeKind::Interface {
                            "interfaces"
                        } else {
                            "enums"
                        };
                        self.report(
                            Diagnostic::new(
                                "invalid-member",
                                format!("{what} cannot declare properties"),
                            )
                            .with_span(p.name.span)
                            .with_note(
                                if kind == TypeKind::Interface {
                                    "interfaces describe behavior; default methods have no state"
                                } else {
                                    "enum values are their cases; use a constant or a method"
                                },
                            ),
                        );
                        continue;
                    }
                    let ty = self.resolve_type(file, &p.ty, &scope);
                    if p.is_static {
                        let sid = StaticId(self.decls.statics.len() as u32);
                        if p.default.is_none() {
                            self.report(
                                Diagnostic::new(
                                    "uninitialized-property",
                                    format!(
                                        "static property `${}` needs an initializer",
                                        p.name.name
                                    ),
                                )
                                .with_span(p.name.span),
                            );
                        }
                        self.decls.statics.push(StaticDecl {
                            name: p.name.name.to_string(),
                            ty,
                            owner: id,
                            visibility: vis(p.visibility),
                            readonly: p.readonly,
                            ast: mat,
                            span: p.name.span,
                        });
                        if self
                            .decls
                            .ty_mut(id)
                            .statics
                            .insert(p.name.name.to_string(), sid)
                            .is_some()
                        {
                            self.duplicate_member(&p.name);
                        }
                        continue;
                    }
                    if fields.iter().any(|f| f.name == *p.name.name) {
                        self.duplicate_member(&p.name);
                        continue;
                    }
                    let readonly = p.readonly || self.decls.ty(id).readonly;
                    fields.push(Field {
                        name: p.name.name.to_string(),
                        ty,
                        visibility: vis(p.visibility),
                        readonly,
                        owner: id,
                        initialized: p.default.is_some(),
                        span: p.name.span,
                    });
                }
                Member::Const(c) => {
                    let ty = self.resolve_type(file, &c.ty, &scope);
                    let cid = ConstId(self.decls.consts.len() as u32);
                    let fqn = format!("{}::{}", self.decls.ty(id).name, c.name.name);
                    self.decls.consts.push(ConstDecl {
                        name: fqn,
                        ty,
                        owner: Some(id),
                        visibility: vis(c.visibility),
                        ast: mat,
                        span: c.name.span,
                    });
                    if self
                        .decls
                        .ty_mut(id)
                        .consts
                        .insert(c.name.name.to_string(), cid)
                        .is_some()
                    {
                        self.duplicate_member(&c.name);
                    }
                }
                Member::Method(m) => {
                    let (tparams, mscope) = self.type_params(file, &m.type_params, &scope);
                    if m.name.name.as_ref() == "__construct" {
                        for p in &m.params {
                            if let Some(pr) = p.promote {
                                if fields.iter().any(|f| f.name == *p.name.name) {
                                    self.duplicate_member(&p.name);
                                    continue;
                                }
                                let ty = self.resolve_type(file, &p.ty, &mscope);
                                let readonly = pr.readonly || self.decls.ty(id).readonly;
                                fields.push(Field {
                                    name: p.name.name.to_string(),
                                    ty,
                                    visibility: vis(pr.visibility),
                                    readonly,
                                    owner: id,
                                    initialized: true,
                                    span: p.name.span,
                                });
                            }
                        }
                    }
                    let sig = self.signature(file, &m.params, m.ret.as_ref(), &mscope);
                    let mut where_bounds = Vec::new();
                    for w in &m.where_bounds {
                        let Some((_, p)) = mscope.params.iter().find(|(n, _)| *n == *w.param.name)
                        else {
                            continue;
                        };
                        let b = self.resolve_type(file, &w.bound, &mscope);
                        where_bounds.push((*p, b));
                    }
                    let func = self.next_func();
                    let mid = MethodId(self.decls.methods.len() as u32);
                    let kind = match m.kind {
                        ast::MethodKind::Plain => MethodKind::Plain,
                        ast::MethodKind::Abstract => MethodKind::Abstract,
                        ast::MethodKind::Final => MethodKind::Final,
                    };
                    self.decls.methods.push(MethodDecl {
                        name: m.name.name.to_string(),
                        owner: id,
                        kind,
                        visibility: vis(m.visibility),
                        is_static: m.is_static,
                        mutating: m.mutating,
                        type_params: tparams,
                        where_bounds,
                        sig,
                        // A bodiless stub method is native, except in an interface,
                        // where it is the method to implement (`Stringable::toString`).
                        has_body: m.body.is_some()
                            || (stdlib
                                && kind != MethodKind::Abstract
                                && self.decls.ty(id).kind != TypeKind::Interface),
                        ast: mat,
                        func,
                        span: m.name.span,
                    });
                    if self
                        .decls
                        .ty_mut(id)
                        .methods
                        .insert(m.name.name.to_string(), mid)
                        .is_some()
                    {
                        self.duplicate_member(&m.name);
                    }
                }
            }
        }
        self.decls.ty_mut(id).fields = fields;
    }

    fn duplicate_member(&mut self, name: &ast::Ident) {
        self.report(
            Diagnostic::new(
                "duplicate-declaration",
                format!("`{}` is declared twice", name.name),
            )
            .with_span(name.span),
        );
    }

    /// Short name of a declaration for messages.
    pub fn short_name(&self, id: DeclId) -> String {
        let n = &self.decls.ty(id).name;
        n.rsplit('\\').next().unwrap_or(n).to_string()
    }

    #[allow(dead_code)]
    fn qname(n: &ast::QualifiedName) -> String {
        qname_text(n)
    }
}

enum NameSpace {
    Type(DeclId),
    Fn(FnId),
    Const(ConstId),
}

fn type_name(d: &Decl) -> (&ast::Ident, TypeKind) {
    match d {
        Decl::Class(c) => (&c.name, TypeKind::Class),
        Decl::Struct(s) => (&s.name, TypeKind::Struct),
        Decl::Interface(i) => (&i.name, TypeKind::Interface),
        Decl::Enum(e) => (&e.name, TypeKind::Enum),
        Decl::Function(_) | Decl::Const(_) => unreachable!("handled by the caller"),
    }
}
