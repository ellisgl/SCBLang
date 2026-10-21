//! Class-level rules, checked once all signatures are known (SPEC "Structs, classes
//! and interfaces"):
//!
//! - `final` only where a method could otherwise be overridden (`redundant-final`);
//!   overriding a `final` method (`override-final`); override signatures.
//! - Concrete classes implement every abstract and interface method
//!   (`missing-method`); conflicting interface defaults (`default-method-conflict`).
//! - Property initialization (`uninitialized-property`,
//!   `missing-parent-constructor-call`): a definite-assignment analysis over each
//!   constructor body, done on the AST because it needs no types.

use std::collections::BTreeSet;

use scb_syntax::ast::{self, ExprKind, StmtKind};
use scb_syntax::{Diagnostic, Span};

use crate::Ctx;
use crate::decls::{DeclId, MethodId, MethodKind, TypeKind};

impl Ctx<'_> {
    pub(crate) fn validate(&mut self) {
        for idx in 0..self.decls.types.len() {
            let id = DeclId(idx as u32);
            if self.decls.ty(id).stdlib {
                continue;
            }
            self.check_callable(id);
            self.check_methods(id);
            self.check_implementations(id);
            self.check_initialization(id);
        }
    }

    /// The `Callable` interface (`__invoke` replacement) is out of POC scope.
    fn check_callable(&mut self, id: DeclId) {
        let callable = self.decls.type_named("Callable");
        if callable.is_some_and(|c| self.decls.ty(id).interface_ids.contains(&c)) {
            let span = self.decls.ty(id).span;
            self.not_in_poc(span, "the `Callable` interface");
        }
    }

    pub(crate) fn not_in_poc(&mut self, span: Span, feature: &str) {
        self.report(
            Diagnostic::new(
                "not-supported-in-poc",
                format!("not supported in the POC: {feature}"),
            )
            .with_span(span)
            .with_note("SPEC \"Implementation plan → POC scope\""),
        );
    }

    fn own_methods(&self, id: DeclId) -> Vec<MethodId> {
        let mut ms: Vec<MethodId> = self.decls.ty(id).methods.values().copied().collect();
        ms.sort_by_key(|m| m.0);
        ms
    }

    fn check_methods(&mut self, id: DeclId) {
        let decl = self.decls.ty(id);
        let (kind, extendable, is_abstract, parent) =
            (decl.kind, decl.extendable, decl.is_abstract, decl.parent);
        for mid in self.own_methods(id) {
            let m = self.decls.method(mid);
            let (name, mkind, span, has_body) = (m.name.clone(), m.kind, m.span, m.has_body);

            // `final` (SPEC: only meaningful on an overridable method).
            if mkind == MethodKind::Final {
                let why = match kind {
                    TypeKind::Struct => Some("struct methods cannot be overridden"),
                    TypeKind::Enum => Some("enum methods cannot be overridden"),
                    TypeKind::Interface => Some("interface methods are meant to be implemented"),
                    TypeKind::Class if !extendable => Some(
                        "the class is final (not `extendable`), so no method can be overridden",
                    ),
                    TypeKind::Class => None,
                };
                if let Some(why) = why {
                    self.report(
                        Diagnostic::new("redundant-final", format!("`final` on `{name}`: {why}"))
                            .with_span(span)
                            .with_fix("remove `final`"),
                    );
                }
            }
            if mkind == MethodKind::Abstract {
                if kind != TypeKind::Class || !is_abstract {
                    self.report(
                        Diagnostic::new(
                            "abstract-method",
                            format!("abstract method `{name}` outside an abstract class"),
                        )
                        .with_span(span),
                    );
                }
            } else if !has_body && kind != TypeKind::Interface {
                self.report(
                    Diagnostic::new("abstract-method", format!("method `{name}` needs a body"))
                        .with_span(span)
                        .with_fix("add a body, or declare it `abstract` in an abstract class"),
                );
            }

            // Overrides.
            let Some(parent) = parent else { continue };
            let Some(base) = self.decls.find_method(parent, &name) else {
                continue;
            };
            if name == "__construct" {
                continue;
            }
            let b = self.decls.method(base);
            let (bkind, bstatic, bowner) = (b.kind, b.is_static, b.owner);
            if bkind == MethodKind::Final {
                let owner = self.decls.ty(bowner).name.clone();
                self.report(
                    Diagnostic::new(
                        "override-final",
                        format!("`{name}` is `final` in `{owner}` and cannot be overridden"),
                    )
                    .with_span(span),
                );
                continue;
            }
            if bstatic != self.decls.method(mid).is_static || !self.same_signature(mid, base) {
                self.report(
                    Diagnostic::new(
                        "incompatible-override",
                        format!("`{name}` does not match the signature it overrides"),
                    )
                    .with_span(span)
                    .with_note(
                        "parameters must have the same types; the return type may be narrower",
                    ),
                );
            }
        }
    }

    /// Same parameter types and modes; the overriding return type is assignable to
    /// the overridden one.
    fn same_signature(&mut self, m: MethodId, base: MethodId) -> bool {
        let (a, b) = (&self.decls.method(m).sig, &self.decls.method(base).sig);
        if a.params.len() != b.params.len() {
            return false;
        }
        let params_ok = a
            .params
            .iter()
            .zip(&b.params)
            .all(|(x, y)| x.ty == y.ty && x.inout == y.inout && x.variadic == y.variadic);
        let (ra, rb) = (a.ret, b.ret);
        params_ok && self.assignable(ra, rb)
    }

    /// Concrete classes, structs and enums provide every abstract method; interface
    /// defaults must not conflict.
    fn check_implementations(&mut self, id: DeclId) {
        let decl = self.decls.ty(id);
        if decl.kind == TypeKind::Interface || decl.is_abstract {
            return;
        }
        let span = decl.span;
        // Abstract methods of superclasses.
        let mut required: Vec<String> = Vec::new();
        for t in self.decls.ancestors(id) {
            for m in self.own_methods(t) {
                let md = self.decls.method(m);
                if md.kind == MethodKind::Abstract {
                    required.push(md.name.clone());
                }
            }
        }
        for i in self.decls.all_interfaces(id) {
            for m in self.own_methods(i) {
                let md = self.decls.method(m);
                if !md.has_body && !self.builtin_conformance(id, i) {
                    required.push(md.name.clone());
                }
            }
        }
        required.sort();
        required.dedup();
        for name in required {
            let implemented = self.decls.ancestors(id).iter().any(|&t| {
                self.decls
                    .ty(t)
                    .methods
                    .get(&name)
                    .is_some_and(|&m| self.decls.method(m).has_body)
            }) || self.default_providers(id, &name).len() == 1;
            if !implemented {
                self.report(
                    Diagnostic::new(
                        "missing-method",
                        format!("`{}` does not implement `{name}`", self.short_name(id)),
                    )
                    .with_span(span),
                );
            }
        }
        // Two interface defaults for the same name, not overridden by the class.
        let mut names: Vec<String> = self
            .decls
            .all_interfaces(id)
            .iter()
            .flat_map(|&i| self.decls.ty(i).methods.keys().cloned().collect::<Vec<_>>())
            .collect();
        names.sort();
        names.dedup();
        for name in names {
            let in_class = self
                .decls
                .ancestors(id)
                .iter()
                .any(|&t| self.decls.ty(t).methods.contains_key(&name));
            let providers = self.default_providers(id, &name);
            if !in_class && providers.len() > 1 {
                let list: Vec<String> = providers.iter().map(|&p| self.short_name(p)).collect();
                self.report(
                    Diagnostic::new(
                        "default-method-conflict",
                        format!("`{name}` has conflicting defaults in {}", list.join(" and ")),
                    )
                    .with_span(span)
                    .with_fix(format!(
                        "override `{name}` in the class; it can call one with `parent({})::{name}()`",
                        list[0]
                    )),
                );
            }
        }
    }

    /// Interfaces the compiler implements itself for user types (none of the
    /// operator interfaces need explicit methods when derived; SPEC
    /// "Auto-derivation"). `Stringable` always needs `toString`.
    fn builtin_conformance(&self, _id: DeclId, iface: DeclId) -> bool {
        matches!(
            self.decls.ty(iface).name.as_str(),
            "Iterable" | "Sendable" | "Callable" | "Generator" | "__Numeric" | "__Integer"
        )
    }

    /// Interfaces of `id` that provide a default body for `name`, ignoring defaults
    /// re-declared by a more specific interface.
    fn default_providers(&self, id: DeclId, name: &str) -> Vec<DeclId> {
        let ifaces = self.decls.all_interfaces(id);
        let with_default: Vec<DeclId> = ifaces
            .iter()
            .copied()
            .filter(|&i| {
                self.decls
                    .ty(i)
                    .methods
                    .get(name)
                    .is_some_and(|&m| self.decls.method(m).has_body)
            })
            .collect();
        with_default
            .iter()
            .copied()
            .filter(|&i| {
                !with_default
                    .iter()
                    .any(|&j| j != i && self.decls.all_interfaces(j).contains(&i))
            })
            .collect()
    }

    // ---------- property initialization ----------

    fn check_initialization(&mut self, id: DeclId) {
        let decl = self.decls.ty(id);
        if !matches!(decl.kind, TypeKind::Class | TypeKind::Struct) {
            return;
        }
        let span = decl.span;
        let own_uninit: BTreeSet<String> = decl
            .fields
            .iter()
            .filter(|f| f.owner == id && !f.initialized)
            .map(|f| f.name.clone())
            .collect();
        let parent_ctor = decl.parent.and_then(|p| self.decls.constructor(p));
        let Some(&ctor) = decl.methods.get("__construct") else {
            if let Some(first) = own_uninit.iter().next() {
                let fspan = decl
                    .fields
                    .iter()
                    .find(|f| f.name == *first)
                    .map_or(span, |f| f.span);
                let names: Vec<String> = own_uninit.iter().map(|n| format!("`${n}`")).collect();
                self.report(
                    Diagnostic::new(
                        "uninitialized-property",
                        format!(
                            "{} has no default and `{}` has no constructor to assign it",
                            names.join(", "),
                            self.short_name(id)
                        ),
                    )
                    .with_span(fspan)
                    .with_fix(
                        "give the property a default (`= null` for `?T`) or add a constructor",
                    ),
                );
            }
            return;
        };
        let at = self.decls.method(ctor).ast;
        let member = match &self.files[at.file as usize].ast.decls[at.decl as usize] {
            ast::Decl::Class(c) => &c.members[at.member.unwrap_or(0) as usize],
            ast::Decl::Struct(s) => &s.members[at.member.unwrap_or(0) as usize],
            _ => return,
        };
        let ast::Member::Method(m) = member else {
            return;
        };
        let Some(body) = &m.body else { return };
        let mut a = InitAnalysis {
            required: own_uninit,
            needs_parent: parent_ctor.is_some(),
            diags: Vec::new(),
            reported_use: false,
        };
        let start = InitState {
            assigned: BTreeSet::new(),
            parent: ParentCall::No,
            diverged: false,
        };
        let end = a.block(&body.stmts, start);
        if !end.diverged {
            a.check_complete(&end, m.name.span, "the constructor ends");
        }
        self.diags.extend(a.diags);
    }
}

/// Whether `parent::__construct()` has run on the paths reaching a point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParentCall {
    No,
    Yes,
    /// On some paths only, or possibly more than once.
    Maybe,
}

#[derive(Debug, Clone)]
struct InitState {
    /// Properties assigned on every path to this point.
    assigned: BTreeSet<String>,
    parent: ParentCall,
    /// Every path to this point has returned or thrown.
    diverged: bool,
}

impl InitState {
    /// The state after two alternative paths.
    fn merge(a: InitState, b: InitState) -> InitState {
        match (a.diverged, b.diverged) {
            (true, _) => b,
            (_, true) => a,
            _ => InitState {
                assigned: a.assigned.intersection(&b.assigned).cloned().collect(),
                parent: if a.parent == b.parent {
                    a.parent
                } else {
                    ParentCall::Maybe
                },
                diverged: false,
            },
        }
    }
}

struct InitAnalysis {
    required: BTreeSet<String>,
    needs_parent: bool,
    diags: Vec<Diagnostic>,
    /// Report one premature `$this` use per constructor.
    reported_use: bool,
}

impl InitAnalysis {
    fn complete(&self, s: &InitState) -> bool {
        self.required.is_subset(&s.assigned) && (!self.needs_parent || s.parent == ParentCall::Yes)
    }

    fn check_complete(&mut self, s: &InitState, span: Span, when: &str) {
        if self.needs_parent && s.parent != ParentCall::Yes {
            self.diags.push(
                Diagnostic::new(
                    "missing-parent-constructor-call",
                    format!(
                        "{when} without calling `parent::__construct(…)` exactly once on every path"
                    ),
                )
                .with_span(span)
                .with_fix("call `parent::__construct(…)` first in the constructor"),
            );
        }
        let missing: Vec<String> = self
            .required
            .difference(&s.assigned)
            .map(|n| format!("`${n}`"))
            .collect();
        if !missing.is_empty() {
            self.diags.push(
                Diagnostic::new(
                    "uninitialized-property",
                    format!(
                        "{when} before {} is assigned on every path",
                        missing.join(", ")
                    ),
                )
                .with_span(span)
                .with_fix("assign it on every path, or give it a default"),
            );
        }
    }

    fn block(&mut self, stmts: &[ast::Stmt], mut s: InitState) -> InitState {
        for st in stmts {
            if s.diverged {
                break;
            }
            s = self.stmt(st, s);
        }
        s
    }

    fn stmt(&mut self, st: &ast::Stmt, s: InitState) -> InitState {
        match &st.kind {
            StmtKind::Block(b) => self.block(&b.stmts, s),
            StmtKind::Local { value, .. } | StmtKind::Destructure { value, .. } => {
                self.expr(value, s)
            }
            StmtKind::Const(c) => self.expr(&c.value, s),
            StmtKind::Expr(e) => {
                let mut s = self.expr(e, s);
                if matches!(e.kind, ExprKind::Throw(_) | ExprKind::Exit(_)) {
                    s.diverged = true;
                }
                s
            }
            StmtKind::Echo(es) => es.iter().fold(s, |s, e| self.expr(e, s)),
            StmtKind::Print(e) => self.expr(e, s),
            StmtKind::Exit => InitState {
                diverged: true,
                ..s
            },
            StmtKind::Return(e) => {
                let s = match e {
                    Some(e) => self.expr(e, s),
                    None => s,
                };
                self.check_complete(&s, st.span, "the constructor returns");
                InitState {
                    diverged: true,
                    ..s
                }
            }
            StmtKind::If {
                cond,
                then,
                elseifs,
                otherwise,
            } => {
                let s = self.expr(cond, s);
                let mut out = self.block(&then.stmts, s.clone());
                let mut rest = s;
                for (c, b) in elseifs {
                    rest = self.expr(c, rest);
                    let branch = self.block(&b.stmts, rest.clone());
                    out = InitState::merge(out, branch);
                }
                let last = match otherwise {
                    Some(b) => self.block(&b.stmts, rest),
                    None => rest,
                };
                InitState::merge(out, last)
            }
            StmtKind::While { cond, body } => {
                let s = self.expr(cond, s);
                self.loop_body(&body.stmts, s)
            }
            StmtKind::DoWhile { body, cond } => {
                let once = self.block(&body.stmts, s.clone());
                let after = self.expr(cond, once);
                // A parent call in the body could run more than once.
                let looped = self.loop_body(&body.stmts, after.clone());
                InitState {
                    parent: if looped.parent != after.parent {
                        ParentCall::Maybe
                    } else {
                        after.parent
                    },
                    ..after
                }
            }
            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                let mut s = match init {
                    ast::ForInit::None => s,
                    ast::ForInit::Decl(d) => self.expr(&d.value, s),
                    ast::ForInit::Exprs(es) => es.iter().fold(s, |s, e| self.expr(e, s)),
                };
                if let Some(c) = cond {
                    s = self.expr(c, s);
                }
                let mut stmts: Vec<ast::Stmt> = body.stmts.clone();
                stmts.extend(step.iter().map(|e| ast::Stmt {
                    kind: StmtKind::Expr(e.clone()),
                    span: e.span,
                }));
                self.loop_body(&stmts, s)
            }
            StmtKind::Foreach { subject, body, .. } => {
                let s = self.expr(subject, s);
                self.loop_body(&body.stmts, s)
            }
            StmtKind::Try {
                body,
                catches,
                finally,
            } => {
                let start = s.clone();
                let mut out = self.block(&body.stmts, s);
                for c in catches {
                    // A catch may run after any prefix of the try body.
                    let branch = self.block(&c.body.stmts, start.clone());
                    out = InitState::merge(out, branch);
                }
                match finally {
                    Some(f) => self.block(&f.stmts, out),
                    None => out,
                }
            }
            StmtKind::Match(m) => self.match_(m, s),
            StmtKind::Break(_) | StmtKind::Continue(_) => InitState {
                diverged: true,
                ..s
            },
        }
    }

    /// A loop body may run zero or more times: assignments in it do not count after
    /// the loop, and a parent constructor call in it is never "exactly once".
    fn loop_body(&mut self, stmts: &[ast::Stmt], s: InitState) -> InitState {
        let after = self.block(stmts, s.clone());
        if after.parent != s.parent {
            InitState {
                parent: ParentCall::Maybe,
                ..s
            }
        } else {
            s
        }
    }

    fn match_(&mut self, m: &ast::Match, s: InitState) -> InitState {
        let s = match &m.subject {
            Some(e) => self.expr(e, s),
            None => s,
        };
        let has_default = m
            .arms
            .iter()
            .any(|a| matches!(a.test, ast::ArmTest::Default));
        let mut out: Option<InitState> = if has_default { None } else { Some(s.clone()) };
        for arm in &m.arms {
            let branch = match &arm.body {
                ast::ArmBody::Expr(e) => self.expr(e, s.clone()),
                ast::ArmBody::Block(b) => self.block(&b.stmts, s.clone()),
            };
            out = Some(match out {
                Some(o) => InitState::merge(o, branch),
                None => branch,
            });
        }
        out.unwrap_or(s)
    }

    /// Walks an expression in evaluation order: `$this->prop = …` assigns, a
    /// `parent::__construct(…)` call counts, any other `$this` use requires complete
    /// initialization.
    fn expr(&mut self, e: &ast::Expr, mut s: InitState) -> InitState {
        match &e.kind {
            ExprKind::Assign {
                op: None,
                target,
                value,
            } => {
                if let Some(prop) = this_prop(target) {
                    s = self.expr(value, s);
                    s.assigned.insert(prop.to_string());
                    return s;
                }
                s = self.expr(target, s);
                self.expr(value, s)
            }
            ExprKind::Call { callee, args } if is_parent_ctor(callee) => {
                for a in args {
                    s = self.expr(arg_expr(a), s);
                }
                s.parent = match s.parent {
                    ParentCall::No => ParentCall::Yes,
                    _ => {
                        self.diags.push(
                            Diagnostic::new(
                                "missing-parent-constructor-call",
                                "`parent::__construct(…)` may run more than once",
                            )
                            .with_span(e.span),
                        );
                        ParentCall::Maybe
                    }
                };
                s
            }
            ExprKind::Var(v) if &**v == "this" => {
                if !self.complete(&s) && !self.reported_use {
                    self.reported_use = true;
                    let (kind, msg) = if self.needs_parent && s.parent != ParentCall::Yes {
                        (
                            "missing-parent-constructor-call",
                            "`$this` is used before `parent::__construct(…)` is called".to_string(),
                        )
                    } else {
                        let missing: Vec<String> = self
                            .required
                            .difference(&s.assigned)
                            .map(|n| format!("`${n}`"))
                            .collect();
                        (
                            "uninitialized-property",
                            format!("`$this` is used before {} is assigned", missing.join(", ")),
                        )
                    };
                    self.diags.push(
                        Diagnostic::new(kind, msg)
                            .with_span(e.span)
                            .with_note("until every property is assigned, `$this` may only be used to assign properties"),
                    );
                }
                s
            }
            _ => {
                let mut out = s;
                for child in children(e) {
                    out = self.expr(child, out);
                }
                out
            }
        }
    }
}

/// `prop` for `$this->prop` (a plain property access on `$this`).
fn this_prop(e: &ast::Expr) -> Option<&str> {
    match &e.kind {
        ExprKind::Prop {
            object,
            name,
            nullsafe: false,
        } if matches!(&object.kind, ExprKind::Var(v) if &**v == "this") => Some(&name.name),
        _ => None,
    }
}

fn is_parent_ctor(callee: &ast::Expr) -> bool {
    matches!(&callee.kind, ExprKind::StaticMember { class, name }
        if matches!(class.kind, ExprKind::Parent(None)) && &*name.name == "__construct")
}

fn arg_expr(a: &ast::Arg) -> &ast::Expr {
    match &a.kind {
        ast::ArgKind::Value(e) | ast::ArgKind::Spread(e) | ast::ArgKind::Inout(e) => e,
    }
}

/// Direct sub-expressions in evaluation order (closure bodies included: a closure
/// that mentions `$this` captures it).
pub(crate) fn children(e: &ast::Expr) -> Vec<&ast::Expr> {
    match &e.kind {
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Bytes(_)
        | ExprKind::Bool(_)
        | ExprKind::Null
        | ExprKind::Var(_)
        | ExprKind::Name(_)
        | ExprKind::SelfRef
        | ExprKind::Parent(_)
        | ExprKind::Error => Vec::new(),
        ExprKind::Interpolated(parts) => parts
            .iter()
            .filter_map(|p| match p {
                ast::InterpPart::Expr(e) => Some(e),
                ast::InterpPart::Text(_) => None,
            })
            .collect(),
        ExprKind::Paren(x)
        | ExprKind::Unary { expr: x, .. }
        | ExprKind::Cast { expr: x, .. }
        | ExprKind::Throw(x)
        | ExprKind::CallableRef(x) => vec![x],
        ExprKind::Exit(x) => x.iter().map(|b| &**b).collect(),
        ExprKind::List(xs) => xs.iter().collect(),
        ExprKind::Map(kvs) => kvs.iter().flat_map(|(k, v)| [k, v]).collect(),
        ExprKind::Binary { lhs, rhs, .. } => vec![lhs, rhs],
        ExprKind::Assign { target, value, .. } => vec![target, value],
        ExprKind::Ternary {
            cond,
            then,
            otherwise,
        } => vec![cond, then, otherwise],
        ExprKind::Range { start, end, .. } => vec![start, end],
        ExprKind::InstanceOf { expr, .. } => vec![expr],
        ExprKind::Prop { object, .. } => vec![object],
        ExprKind::StaticMember { class, .. } | ExprKind::StaticProp { class, .. } => vec![class],
        ExprKind::Index { object, index } => {
            let mut v: Vec<&ast::Expr> = vec![object];
            v.extend(index.iter().map(|b| &**b));
            v
        }
        ExprKind::Call { callee, args } => {
            let mut v: Vec<&ast::Expr> = vec![callee];
            v.extend(args.iter().map(arg_expr));
            v
        }
        ExprKind::New { args, .. } => args.iter().map(arg_expr).collect(),
        ExprKind::Clone { expr, with } => {
            let mut v: Vec<&ast::Expr> = vec![expr];
            v.extend(with.iter().map(arg_expr));
            v
        }
        ExprKind::Closure(c) => match &c.body {
            ast::ClosureBody::Expr(e) => vec![e],
            ast::ClosureBody::Block(b) => b.stmts.iter().flat_map(stmt_exprs).collect(),
        },
        ExprKind::Match(m) => {
            let mut v: Vec<&ast::Expr> = m.subject.iter().collect();
            for arm in &m.arms {
                if let ast::ArmTest::Conditions(cs) = &arm.test {
                    v.extend(cs);
                }
                v.extend(arm.guard.iter());
                match &arm.body {
                    ast::ArmBody::Expr(e) => v.push(e),
                    ast::ArmBody::Block(b) => v.extend(b.stmts.iter().flat_map(stmt_exprs)),
                }
            }
            v
        }
    }
}

/// Every expression directly or indirectly inside a statement.
pub(crate) fn stmt_exprs(s: &ast::Stmt) -> Vec<&ast::Expr> {
    fn block(b: &ast::Block) -> Vec<&ast::Expr> {
        b.stmts.iter().flat_map(stmt_exprs).collect()
    }
    match &s.kind {
        StmtKind::Block(b) => block(b),
        StmtKind::Local { value, .. } | StmtKind::Destructure { value, .. } => vec![value],
        StmtKind::Const(c) => vec![&c.value],
        StmtKind::If {
            cond,
            then,
            elseifs,
            otherwise,
        } => {
            let mut v = vec![cond];
            v.extend(block(then));
            for (c, b) in elseifs {
                v.push(c);
                v.extend(block(b));
            }
            if let Some(b) = otherwise {
                v.extend(block(b));
            }
            v
        }
        StmtKind::While { cond, body } | StmtKind::DoWhile { body, cond } => {
            let mut v = vec![cond];
            v.extend(block(body));
            v
        }
        StmtKind::For {
            init,
            cond,
            step,
            body,
        } => {
            let mut v: Vec<&ast::Expr> = match init {
                ast::ForInit::None => Vec::new(),
                ast::ForInit::Decl(d) => vec![&d.value],
                ast::ForInit::Exprs(es) => es.iter().collect(),
            };
            v.extend(cond.iter());
            v.extend(step.iter());
            v.extend(block(body));
            v
        }
        StmtKind::Foreach { subject, body, .. } => {
            let mut v = vec![subject];
            v.extend(block(body));
            v
        }
        StmtKind::Try {
            body,
            catches,
            finally,
        } => {
            let mut v = block(body);
            for c in catches {
                v.extend(block(&c.body));
            }
            if let Some(f) = finally {
                v.extend(block(f));
            }
            v
        }
        StmtKind::Match(m) => {
            let mut v: Vec<&ast::Expr> = m.subject.iter().collect();
            for arm in &m.arms {
                if let ast::ArmTest::Conditions(cs) = &arm.test {
                    v.extend(cs);
                }
                v.extend(arm.guard.iter());
                match &arm.body {
                    ast::ArmBody::Expr(e) => v.push(e),
                    ast::ArmBody::Block(b) => v.extend(block(b)),
                }
            }
            v
        }
        StmtKind::Echo(es) => es.iter().collect(),
        StmtKind::Print(e) | StmtKind::Expr(e) => vec![e],
        StmtKind::Return(e) => e.iter().collect(),
        StmtKind::Exit | StmtKind::Break(_) | StmtKind::Continue(_) => Vec::new(),
    }
}
