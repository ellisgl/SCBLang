//! Body checking: type-checks every function, method, closure and initializer and
//! produces its TIR (`crate::tir`).
//!
//! # Frames, scopes and locals
//!
//! Each body being checked has a [`Frame`]: its locals (numbered slots), a stack of
//! block scopes (name → slot), the expected return type and loop bookkeeping. Frames
//! form a stack: a closure pushes a frame on top of its enclosing function's. Looking
//! up `$x` searches the current frame's scopes innermost first, then the enclosing
//! frames; a hit in an enclosing frame becomes a *capture*: the closure gets its own
//! slot, filled with a copy when the closure is created (SPEC "Closures": capture by
//! value), and every closure in between captures it too.
//!
//! Declaring a name that is visible anywhere in the frame stack is
//! `redeclared-variable` (SPEC "Type system": no shadowing, closure parameters
//! included).
//!
//! # Flow typing
//!
//! `facts` maps a local to a narrower type than it was declared with (`?int` → `int`
//! after `$x != null`). Conditions produce facts for their true and false outcomes;
//! branches apply them and the state after a branch is the intersection of what
//! holds at the end of each branch that can complete. Assigning to a local drops its
//! fact. Before a loop, facts about locals the loop assigns are dropped, because the
//! body may run after such an assignment.
//!
//! The submodules hold the rules: `expr` (expressions), `ops` (operators), `call`
//! (calls and arguments), `place` (assignment targets and nested mutation), `stmt`
//! (statements), `pattern` (`match`, patterns, exhaustiveness), `rel` (type relations).

mod call;
mod expr;
mod ops;
mod pattern;
mod place;
pub(crate) mod rel;
mod stmt;

use std::collections::HashMap;

use scb_syntax::ast;
use scb_syntax::{Diagnostic, Span};

use crate::Ctx;
use crate::decls::{ConstId, DeclId, FnId, MethodId, StaticId, TypeKind};
use crate::resolve::TyScope;
use crate::tir::{self, Body, Builtin, FuncId, LocalId};
use crate::ty::{Ty, TyKind};

/// What kind of body a frame checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameKind {
    Function,
    Method,
    Closure,
    /// A constant, static property or default value initializer.
    Initializer,
}

#[derive(Debug, Clone)]
pub(crate) struct LocalMeta {
    pub declared: Ty,
    pub readonly: bool,
    pub constant: bool,
    /// A closure's copy of an outer local: never assignable.
    pub captured: bool,
    pub is_this: bool,
}

#[derive(Debug, Default)]
pub(crate) struct Scope {
    pub names: HashMap<String, LocalId>,
    pub declared: Vec<LocalId>,
}

pub(crate) struct Frame {
    pub file: usize,
    pub kind: FrameKind,
    pub name: String,
    pub locals: Vec<tir::Local>,
    pub meta: Vec<LocalMeta>,
    pub scopes: Vec<Scope>,
    pub facts: HashMap<LocalId, Ty>,
    pub ret: Ty,
    /// The type whose method this is (also inside its closures).
    pub self_decl: Option<DeclId>,
    pub is_static: bool,
    /// A `mutating` struct method, or any class method: `$this` fields are writable.
    pub this_writable: bool,
    pub in_ctor: bool,
    /// Closure captures: (inner slot, outer slot in the enclosing frame).
    pub captures: Vec<(LocalId, LocalId)>,
    /// Enclosing loops, innermost last; `true` once a `break` targets the loop.
    pub loops: Vec<bool>,
    /// Loop depth at the start of each enclosing `finally` body.
    pub finally_at: Vec<usize>,
    /// Stdlib type parameters in scope (stub initializers only).
    pub ty_scope: TyScope,
    /// Open `?->` sections of the postfix chain being checked: the temporary holding
    /// the non-null value, and the expression producing it (see `expr::chain_object`).
    pub nullsafe: Vec<(LocalId, tir::Expr)>,
}

/// Facts for the two outcomes of a condition.
#[derive(Debug, Default, Clone)]
pub(crate) struct Facts {
    pub when_true: Vec<(LocalId, Ty)>,
    pub when_false: Vec<(LocalId, Ty)>,
}

impl Facts {
    pub fn swap(self) -> Facts {
        Facts {
            when_true: self.when_false,
            when_false: self.when_true,
        }
    }
}

impl Ctx<'_> {
    pub(crate) fn frame(&mut self) -> &mut Frame {
        self.frames
            .last_mut()
            .expect("a frame is active while checking a body")
    }

    pub(crate) fn file(&self) -> usize {
        self.frames.last().map_or(0, |f| f.file)
    }

    pub(crate) fn new_frame(&self, file: usize, kind: FrameKind, name: String, ret: Ty) -> Frame {
        Frame {
            file,
            kind,
            name,
            locals: Vec::new(),
            meta: Vec::new(),
            scopes: vec![Scope::default()],
            facts: HashMap::new(),
            ret,
            self_decl: None,
            is_static: true,
            this_writable: false,
            in_ctor: false,
            captures: Vec::new(),
            loops: Vec::new(),
            finally_at: Vec::new(),
            ty_scope: TyScope::default(),
            nullsafe: Vec::new(),
        }
    }

    /// Adds a slot to the current frame without naming it in a scope.
    pub(crate) fn new_local(&mut self, name: &str, ty: Ty, meta: LocalMeta) -> LocalId {
        let f = self.frame();
        let id = LocalId(f.locals.len() as u32);
        f.locals.push(tir::Local {
            name: name.to_string(),
            ty,
        });
        f.meta.push(meta);
        id
    }

    pub(crate) fn plain_meta(ty: Ty) -> LocalMeta {
        LocalMeta {
            declared: ty,
            readonly: false,
            constant: false,
            captured: false,
            is_this: false,
        }
    }

    /// Whether `name` is visible in the current frame or any enclosing one.
    fn visible(&self, name: &str) -> bool {
        self.frames
            .iter()
            .any(|f| f.scopes.iter().any(|s| s.names.contains_key(name)))
    }

    /// Declares a new local in the innermost scope; `redeclared-variable` if the name
    /// is already visible (SPEC "Type system": no shadowing).
    pub(crate) fn declare(&mut self, name: &str, ty: Ty, span: Span, meta: LocalMeta) -> LocalId {
        if name == "this" {
            self.report(
                Diagnostic::new("redeclared-variable", "`$this` is reserved").with_span(span),
            );
        } else if self.visible(name) {
            self.report(
                Diagnostic::new(
                    "redeclared-variable",
                    format!("`${name}` is already declared in an enclosing scope"),
                )
                .with_span(span)
                .with_note("SCB has no shadowing: pick a new name, or assign without a type to reuse the variable"),
            );
        }
        let id = self.new_local(name, ty, meta);
        let scope = self.frame().scopes.last_mut().expect("a scope is open");
        scope.names.insert(name.to_string(), id);
        scope.declared.push(id);
        id
    }

    pub(crate) fn push_scope(&mut self) {
        self.frame().scopes.push(Scope::default());
    }

    /// Closes the innermost scope, returning the locals declared in it.
    pub(crate) fn pop_scope(&mut self) -> Vec<LocalId> {
        let scope = self.frame().scopes.pop().expect("a scope is open");
        let f = self.frame();
        for id in &scope.declared {
            f.facts.remove(id);
        }
        scope.declared
    }

    /// Looks a variable up, capturing it from enclosing frames when needed.
    pub(crate) fn lookup_local(&mut self, name: &str) -> Option<LocalId> {
        let depth = self.frames.len() - 1;
        self.lookup_in(depth, name)
    }

    fn lookup_in(&mut self, depth: usize, name: &str) -> Option<LocalId> {
        let found = self.frames[depth]
            .scopes
            .iter()
            .rev()
            .find_map(|s| s.names.get(name).copied());
        if found.is_some() {
            return found;
        }
        if depth == 0 || self.frames[depth].kind != FrameKind::Closure {
            return None;
        }
        let outer = self.lookup_in(depth - 1, name)?;
        // Capture: the closure gets a slot holding a copy, typed as the outer local is
        // known at this point (narrowed facts included).
        let outer_frame = &self.frames[depth - 1];
        let ty = outer_frame
            .facts
            .get(&outer)
            .copied()
            .unwrap_or(outer_frame.locals[outer.0 as usize].ty);
        let mut meta = outer_frame.meta[outer.0 as usize].clone();
        meta.captured = true;
        meta.declared = ty;
        let frame = &mut self.frames[depth];
        let id = LocalId(frame.locals.len() as u32);
        frame.locals.push(tir::Local {
            name: name.to_string(),
            ty,
        });
        frame.meta.push(meta);
        frame.captures.push((id, outer));
        frame.scopes[0].names.insert(name.to_string(), id);
        Some(id)
    }

    /// The current (possibly narrowed) type of a local.
    pub(crate) fn local_ty(&mut self, id: LocalId) -> Ty {
        let f = self.frame();
        f.facts
            .get(&id)
            .copied()
            .unwrap_or(f.locals[id.0 as usize].ty)
    }

    pub(crate) fn apply_facts(&mut self, facts: &[(LocalId, Ty)]) {
        for &(id, ty) in facts {
            self.frame().facts.insert(id, ty);
        }
    }

    pub(crate) fn facts_snapshot(&mut self) -> HashMap<LocalId, Ty> {
        self.frame().facts.clone()
    }

    pub(crate) fn restore_facts(&mut self, facts: HashMap<LocalId, Ty>) {
        self.frame().facts = facts;
    }

    /// Facts that hold after alternative paths: those equal in every completing path.
    pub(crate) fn join_facts(&mut self, outs: Vec<HashMap<LocalId, Ty>>) {
        let mut iter = outs.into_iter();
        let Some(mut acc) = iter.next() else { return };
        for other in iter {
            acc.retain(|k, v| other.get(k) == Some(v));
        }
        self.frame().facts = acc;
    }

    /// Drops facts about locals that `names` assigns (before a loop).
    pub(crate) fn forget_assigned(&mut self, names: &[String]) {
        let ids: Vec<LocalId> = names.iter().filter_map(|n| self.lookup_local(n)).collect();
        for id in ids {
            self.frame().facts.remove(&id);
        }
    }

    // ---------- driver ----------

    /// Checks every body and returns `main`.
    pub(crate) fn check_bodies(&mut self) -> Option<FuncId> {
        self.consts = (0..self.decls.consts.len()).map(|_| None).collect();
        self.statics = (0..self.decls.statics.len()).map(|_| None).collect();
        for i in 0..self.decls.functions.len() {
            self.check_function(FnId(i as u32));
        }
        for i in 0..self.decls.methods.len() {
            self.check_method(MethodId(i as u32));
        }
        for i in 0..self.decls.consts.len() {
            self.check_const(ConstId(i as u32));
        }
        for i in 0..self.decls.statics.len() {
            self.check_static(StaticId(i as u32));
        }
        self.find_main()
    }

    fn check_function(&mut self, id: FnId) {
        let d = self.decls.function(id);
        let (func, at, name, sig, span) = (d.func, d.ast, d.name.clone(), d.sig.clone(), d.span);
        let file = at.file as usize;
        let ast::Decl::Function(f) = &self.files[file].ast.decls[at.decl as usize] else {
            unreachable!("functions are Decl::Function")
        };
        let mut frame = self.new_frame(file, FrameKind::Function, name.clone(), sig.ret);
        frame.ty_scope = self.fn_ty_scope(&d_type_params(self, id));
        self.frames.push(frame);
        let params = self.declare_params(&f.params, &sig);
        let body = match &f.body {
            Some(b) => Body::Block(self.function_body(b, sig.ret, f.name.span)),
            // A stub: implemented natively, named by its qualified name.
            None => Body::Builtin(Builtin(name)),
        };
        let frame = self.frames.pop().expect("pushed above");
        self.funcs[func.0 as usize] = Some(finish(frame, None, params, body, span));
    }

    fn fn_ty_scope(&self, params: &[(String, crate::decls::ParamId)]) -> TyScope {
        TyScope {
            params: params.to_vec(),
            self_decl: None,
        }
    }

    fn check_method(&mut self, id: MethodId) {
        let m = self.decls.method(id);
        let (func, at, owner, sig, span) = (m.func, m.ast, m.owner, m.sig.clone(), m.span);
        let (is_static, mutating, mname, has_body) =
            (m.is_static, m.mutating, m.name.clone(), m.has_body);
        let file = at.file as usize;
        let qualified = format!("{}::{mname}", self.decls.ty(owner).name);
        let member = member_ast(self.files, at);
        let ast::Member::Method(ast_m) = member else {
            unreachable!("methods are Member::Method")
        };
        let decl_kind = self.decls.ty(owner).kind;
        let mut frame = self.new_frame(file, FrameKind::Method, qualified, sig.ret);
        frame.self_decl = Some(owner);
        frame.is_static = is_static;
        frame.in_ctor = mname == "__construct";
        frame.this_writable = decl_kind == TypeKind::Class || mutating || frame.in_ctor;
        frame.ty_scope = TyScope {
            params: self
                .decls
                .ty(owner)
                .type_params
                .iter()
                .chain(&self.decls.method(id).type_params)
                .map(|&p| (self.decls.param(p).name.clone(), p))
                .collect(),
            self_decl: Some(owner),
        };
        self.frames.push(frame);
        if !is_static {
            let this_ty = self.self_type(owner);
            let meta = LocalMeta {
                declared: this_ty,
                readonly: true,
                constant: false,
                captured: false,
                is_this: true,
            };
            let this = self.new_local("this", this_ty, meta);
            self.frame().scopes[0].names.insert("this".into(), this);
        }
        let params = self.declare_params(&ast_m.params, &sig);
        let ret = if mname == "__construct" || mname == "__destruct" {
            Ty::VOID
        } else {
            sig.ret
        };
        let body = match &ast_m.body {
            Some(b) => Body::Block(self.function_body(b, ret, ast_m.name.span)),
            // Stubs are implemented natively: `list.push` for instance methods,
            // `i8::wrap` for static ones.
            None if has_body => {
                let sep = if is_static { "::" } else { "." };
                Body::Builtin(Builtin(format!(
                    "{}{sep}{mname}",
                    self.decls.ty(owner).name
                )))
            }
            None => Body::Abstract,
        };
        let frame = self.frames.pop().expect("pushed above");
        // Promoted constructor parameters are stored into fields before the body runs.
        let params = params
            .into_iter()
            .zip(&ast_m.params)
            .map(|(mut p, ap)| {
                if ap.promote.is_some() {
                    p.promote = self.decls.field_index(owner, &ap.name.name);
                }
                p
            })
            .collect();
        self.funcs[func.0 as usize] = Some(finish(frame, Some(owner), params, body, span));
    }

    /// Declares parameters as the first locals and checks their defaults.
    pub(crate) fn declare_params(
        &mut self,
        params: &[ast::Param],
        sig: &crate::decls::Sig,
    ) -> Vec<tir::Param> {
        let mut out = Vec::new();
        for (p, s) in params.iter().zip(&sig.params) {
            let ty = if s.variadic {
                self.types.list(s.ty)
            } else {
                s.ty
            };
            let default = p.default.as_ref().map(|d| {
                let e = self.expr_as(d, s.ty);
                self.require_constant(d, true);
                e
            });
            let local = self.declare(&p.name.name, ty, p.name.span, Self::plain_meta(ty));
            out.push(tir::Param {
                local,
                ty,
                inout: s.inout,
                variadic: s.variadic,
                default,
                promote: None,
            });
        }
        out
    }

    /// Checks a function body; a non-`void` function must not fall off its end.
    fn function_body(&mut self, body: &ast::Block, ret: Ty, name_span: Span) -> tir::Block {
        let (block, diverges) = self.block(body);
        if !diverges && ret != Ty::VOID && ret != Ty::ERROR {
            let msg = if ret == Ty::NEVER {
                "a `never` function must not return normally".to_string()
            } else {
                format!(
                    "missing `return`: the function can end without returning a `{}`",
                    self.show(ret)
                )
            };
            self.report(Diagnostic::new("missing-return", msg).with_span(name_span));
        }
        block
    }

    fn check_const(&mut self, id: ConstId) {
        let c = self.decls.konst(id);
        let (at, ty, owner) = (c.ast, c.ty, c.owner);
        let file = at.file as usize;
        let value: &ast::Expr = match at.member {
            None => match &self.files[file].ast.decls[at.decl as usize] {
                ast::Decl::Const(c) => &c.value,
                _ => unreachable!("namespace constants are Decl::Const"),
            },
            Some(_) => match member_ast(self.files, at) {
                ast::Member::Const(c) => &c.value,
                _ => unreachable!("class constants are Member::Const"),
            },
        };
        let mut frame = self.new_frame(file, FrameKind::Initializer, "const".into(), ty);
        frame.self_decl = owner;
        self.frames.push(frame);
        let e = self.expr_as(value, ty);
        self.require_constant(value, false);
        self.frames.pop();
        self.consts[id.0 as usize] = Some(e);
    }

    fn check_static(&mut self, id: StaticId) {
        let s = self.decls.static_(id);
        let (at, ty, owner) = (s.ast, s.ty, s.owner);
        let file = at.file as usize;
        let ast::Member::Property(p) = member_ast(self.files, at) else {
            unreachable!("statics are Member::Property")
        };
        let mut frame = self.new_frame(file, FrameKind::Initializer, "static".into(), ty);
        frame.self_decl = Some(owner);
        self.frames.push(frame);
        // Statics are initialized lazily and may use any expression.
        let e = match &p.default {
            Some(d) => self.expr_as(d, ty),
            None => tir::Expr {
                kind: tir::ExprKind::Lit(tir::Lit::Null),
                ty: Ty::ERROR,
                span: p.name.span,
            },
        };
        self.frames.pop();
        self.statics[id.0 as usize] = Some(e);
    }

    /// `main` must exist in the user's namespace with an allowed signature (SPEC
    /// "Source files and program entry").
    fn find_main(&mut self) -> Option<FuncId> {
        let user_ns = self
            .files
            .iter()
            .find(|f| !f.stdlib)
            .map(|f| f.namespace.clone())?;
        let fqn = crate::resolve::qualify(&user_ns, "main");
        let Some(&id) = self.decls.fn_names.get(&fqn) else {
            self.report(Diagnostic::new(
                "missing-main",
                format!(
                    "no `main` function: define `function main(): void` in namespace `{user_ns}`"
                ),
            ));
            return None;
        };
        let d = self.decls.function(id);
        let (sig, span, func, file) = (d.sig.clone(), d.span, d.func, d.ast.file as usize);
        if !self.files[file].main_file {
            self.report(
                Diagnostic::new("missing-main", "`main` must be defined in main.scb")
                    .with_span(span),
            );
        }
        let args_ok = match sig.params.as_slice() {
            [] => true,
            [p] => {
                let list = self.types.list(Ty::STRING);
                p.ty == list && !p.inout && !p.variadic && !p.has_default
            }
            _ => false,
        };
        let ret_ok = sig.ret == Ty::VOID || sig.ret == Ty::I64;
        if !args_ok || !ret_ok {
            self.report(
                Diagnostic::new("invalid-main", "`main` has an unsupported signature")
                    .with_span(span)
                    .with_note("allowed: `main(): void`, `main(): int`, `main($args: list<string>): void`, `main($args: list<string>): int`"),
            );
        }
        Some(func)
    }

    // ---------- shared helpers ----------

    pub(crate) fn type_mismatch(&mut self, span: Span, expected: Ty, found: Ty) {
        if expected == Ty::ERROR || found == Ty::ERROR {
            return;
        }
        let msg = format!(
            "expected `{}`, found `{}`",
            self.show(expected),
            self.show(found)
        );
        self.report(Diagnostic::new("type-mismatch", msg).with_span(span));
    }

    pub(crate) fn error_expr(span: Span) -> tir::Expr {
        tir::Expr {
            kind: tir::ExprKind::Lit(tir::Lit::Null),
            ty: Ty::ERROR,
            span,
        }
    }

    /// Whether `ty` is a class (reference) type, as opposed to a value type.
    pub(crate) fn is_class(&self, ty: Ty) -> bool {
        match self.types.kind(ty) {
            TyKind::Named(d, _) => matches!(
                self.decls.ty(*d).kind,
                TypeKind::Class | TypeKind::Interface
            ),
            _ => false,
        }
    }
}

/// The AST of a class member.
pub(crate) fn member_ast(
    files: &[crate::resolve::ParsedFile],
    at: crate::decls::AstRef,
) -> &ast::Member {
    let members = match &files[at.file as usize].ast.decls[at.decl as usize] {
        ast::Decl::Class(c) => &c.members,
        ast::Decl::Struct(s) => &s.members,
        ast::Decl::Interface(i) => &i.members,
        ast::Decl::Enum(e) => &e.members,
        _ => unreachable!("members belong to types"),
    };
    &members[at.member.expect("a member reference") as usize]
}

fn d_type_params(ctx: &Ctx, id: FnId) -> Vec<(String, crate::decls::ParamId)> {
    ctx.decls
        .function(id)
        .type_params
        .iter()
        .map(|&p| (ctx.decls.param(p).name.clone(), p))
        .collect()
}

pub(crate) fn finish(
    frame: Frame,
    owner: Option<DeclId>,
    params: Vec<tir::Param>,
    body: Body,
    span: Span,
) -> tir::Func {
    tir::Func {
        name: frame.name,
        owner,
        params,
        ret: frame.ret,
        locals: frame.locals,
        body,
        span,
    }
}
