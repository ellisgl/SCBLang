//! S-expression dump of the AST, for golden tests. Spans are omitted.
//!
//! Each node becomes `(tag child child …)`. A node that fits in the line width is
//! printed on one line; otherwise its leading atoms stay on the first line and each
//! remaining child goes on its own indented line. The format is only for humans and
//! golden files; nothing parses it back.

use crate::ast::*;

enum S {
    Atom(String),
    List(Vec<S>),
}

fn atom(s: impl Into<String>) -> S {
    S::Atom(s.into())
}

macro_rules! list {
    ($($e:expr),* $(,)?) => { S::List(vec![$($e),*]) };
}

fn tagged(tag: &str, items: impl IntoIterator<Item = S>) -> S {
    let mut v = vec![atom(tag)];
    v.extend(items);
    S::List(v)
}

const WIDTH: usize = 88;

impl S {
    fn flat(&self) -> String {
        match self {
            S::Atom(a) => a.clone(),
            S::List(items) => {
                let inner: Vec<String> = items.iter().map(S::flat).collect();
                format!("({})", inner.join(" "))
            }
        }
    }

    fn render(&self, indent: usize, out: &mut String) {
        let flat = self.flat();
        let S::List(items) = self else {
            out.push_str(&flat);
            return;
        };
        if indent + flat.len() <= WIDTH || items.len() < 2 {
            out.push_str(&flat);
            return;
        }
        // Head (and any leading atoms) on the first line, the rest indented below.
        let split = items
            .iter()
            .take_while(|i| matches!(i, S::Atom(_)))
            .count()
            .max(1);
        let head: Vec<String> = items[..split].iter().map(S::flat).collect();
        out.push('(');
        out.push_str(&head.join(" "));
        for item in &items[split..] {
            out.push('\n');
            out.push_str(&" ".repeat(indent + 2));
            item.render(indent + 2, out);
        }
        out.push(')');
    }
}

/// Renders a file's AST as an indented S-expression.
pub fn dump_file(f: &File) -> String {
    let mut out = String::new();
    file(f).render(0, &mut out);
    out.push('\n');
    out
}

/// Renders one expression on its own (used by tests).
pub fn dump_expr(e: &Expr) -> String {
    let mut out = String::new();
    expr(e).render(0, &mut out);
    out
}

/// Renders a list of statements (used by tests and the SPEC example harness).
pub fn dump_stmts(stmts: &[Stmt]) -> String {
    let mut out = String::new();
    for s in stmts {
        stmt(s).render(0, &mut out);
        out.push('\n');
    }
    out
}

fn file(f: &File) -> S {
    let mut items = vec![atom("file"), list![atom("namespace"), qname(&f.namespace)]];
    for u in &f.uses {
        let kind = match u.kind {
            UseKind::Type => "use",
            UseKind::Function => "use-function",
            UseKind::Const => "use-const",
        };
        let mut v = vec![atom(kind), qname(&u.name)];
        if let Some(a) = &u.alias {
            v.push(list![atom("as"), atom(&*a.name)]);
        }
        items.push(S::List(v));
    }
    items.extend(f.decls.iter().map(decl));
    S::List(items)
}

fn qname(n: &QualifiedName) -> S {
    let parts: Vec<&str> = n.parts.iter().map(|p| &*p.name).collect();
    atom(format!(
        "{}{}",
        if n.absolute { "\\" } else { "" },
        parts.join("\\")
    ))
}

fn var(i: &Ident) -> S {
    atom(format!("${}", i.name))
}

fn decl(d: &Decl) -> S {
    match d {
        Decl::Function(f) => {
            let mut v = vec![atom("function"), atom(&*f.name.name)];
            type_params(&f.type_params, &mut v);
            v.push(params(&f.params));
            v.push(list![atom("returns"), ty(&f.ret)]);
            v.push(f.body.as_ref().map_or_else(|| atom("no-body"), block));
            S::List(v)
        }
        Decl::Const(c) => const_decl(c),
        Decl::Class(c) => {
            let mut v = vec![atom("class"), atom(&*c.name.name)];
            type_params(&c.type_params, &mut v);
            match c.kind {
                ClassKind::Plain => {}
                ClassKind::Extendable => v.push(atom("extendable")),
                ClassKind::Abstract => v.push(atom("abstract")),
            }
            if c.readonly {
                v.push(atom("readonly"));
            }
            if let Some(e) = &c.extends {
                v.push(list![atom("extends"), named(e)]);
            }
            implements(&c.implements, &mut v);
            v.extend(c.members.iter().map(member));
            S::List(v)
        }
        Decl::Struct(s) => {
            let mut v = vec![atom("struct"), atom(&*s.name.name)];
            type_params(&s.type_params, &mut v);
            if s.readonly {
                v.push(atom("readonly"));
            }
            implements(&s.implements, &mut v);
            v.extend(s.members.iter().map(member));
            S::List(v)
        }
        Decl::Interface(i) => {
            let mut v = vec![atom("interface"), atom(&*i.name.name)];
            type_params(&i.type_params, &mut v);
            if !i.extends.is_empty() {
                v.push(tagged("extends", i.extends.iter().map(named)));
            }
            v.extend(i.members.iter().map(member));
            S::List(v)
        }
        Decl::Enum(e) => {
            let mut v = vec![atom("enum"), atom(&*e.name.name)];
            type_params(&e.type_params, &mut v);
            if let Some(b) = &e.backing {
                v.push(list![atom("backed"), ty(b)]);
            }
            implements(&e.implements, &mut v);
            for c in &e.cases {
                let mut cv = vec![atom("case"), atom(&*c.name.name)];
                match &c.body {
                    CaseBody::Unit => {}
                    CaseBody::Value(x) => cv.push(expr(x)),
                    CaseBody::Data(ps) => cv.push(params(ps)),
                }
                v.push(S::List(cv));
            }
            v.extend(e.members.iter().map(member));
            S::List(v)
        }
    }
}

fn type_params(tps: &[TypeParam], v: &mut Vec<S>) {
    if tps.is_empty() {
        return;
    }
    v.push(tagged(
        "type-params",
        tps.iter().map(|tp| {
            let mut t = vec![atom(&*tp.name.name)];
            match tp.variance {
                Variance::Invariant => {}
                Variance::In => t.push(atom("in")),
                Variance::Out => t.push(atom("out")),
            }
            if let Some(b) = &tp.bound {
                t.push(list![atom("of"), ty(b)]);
            }
            if let Some(d) = &tp.default {
                t.push(list![atom("="), ty(d)]);
            }
            S::List(t)
        }),
    ));
}

fn implements(list: &[NamedType], v: &mut Vec<S>) {
    if !list.is_empty() {
        v.push(tagged("implements", list.iter().map(named)));
    }
}

fn const_decl(c: &ConstDecl) -> S {
    list![
        atom("const"),
        atom(&*c.name.name),
        ty(&c.ty),
        expr(&c.value)
    ]
}

fn visibility(v: Visibility) -> S {
    atom(match v {
        Visibility::Public => "public",
        Visibility::Protected => "protected",
        Visibility::Private => "private",
        Visibility::Internal => "internal",
    })
}

fn member(m: &Member) -> S {
    match m {
        Member::Property(p) => {
            let mut v = vec![atom("property"), var(&p.name), visibility(p.visibility)];
            if p.is_static {
                v.push(atom("static"));
            }
            if p.readonly {
                v.push(atom("readonly"));
            }
            v.push(ty(&p.ty));
            if let Some(d) = &p.default {
                v.push(list![atom("="), expr(d)]);
            }
            S::List(v)
        }
        Member::Method(m) => {
            let mut v = vec![atom("method"), atom(&*m.name.name)];
            match m.kind {
                MethodKind::Plain => {}
                MethodKind::Abstract => v.push(atom("abstract")),
                MethodKind::Final => v.push(atom("final")),
            }
            v.push(visibility(m.visibility));
            if m.is_static {
                v.push(atom("static"));
            }
            if m.mutating {
                v.push(atom("mutating"));
            }
            type_params(&m.type_params, &mut v);
            v.push(params(&m.params));
            if let Some(r) = &m.ret {
                v.push(list![atom("returns"), ty(r)]);
            }
            if !m.where_bounds.is_empty() {
                v.push(tagged(
                    "where",
                    m.where_bounds
                        .iter()
                        .map(|w| list![atom(&*w.param.name), ty(&w.bound)]),
                ));
            }
            match &m.body {
                Some(b) => v.push(block(b)),
                None => v.push(atom("no-body")),
            }
            S::List(v)
        }
        Member::Const(c) => list![
            atom("const"),
            atom(&*c.name.name),
            visibility(c.visibility),
            ty(&c.ty),
            expr(&c.value)
        ],
    }
}

fn params(ps: &[Param]) -> S {
    tagged("params", ps.iter().map(param))
}

fn param(p: &Param) -> S {
    let mut v = vec![atom("param"), var(&p.name)];
    if let Some(pr) = &p.promote {
        v.push(visibility(pr.visibility));
        if pr.readonly {
            v.push(atom("readonly"));
        }
    }
    if p.inout {
        v.push(atom("inout"));
    }
    if p.variadic {
        v.push(atom("variadic"));
    }
    v.push(ty(&p.ty));
    if let Some(d) = &p.default {
        v.push(list![atom("="), expr(d)]);
    }
    S::List(v)
}

fn named(n: &NamedType) -> S {
    if n.args.is_empty() {
        return qname(&n.name);
    }
    let mut v = vec![qname(&n.name)];
    v.extend(n.args.iter().map(ty));
    tagged("generic", v)
}

fn ty(t: &Type) -> S {
    match &t.kind {
        TypeKind::Named(n) => named(n),
        TypeKind::Nullable(inner) => list![atom("?"), ty(inner)],
        TypeKind::Union(ms) => tagged("union", ms.iter().map(ty)),
        TypeKind::Fn { params, ret } => {
            let ps = params.iter().map(|p| {
                if p.inout {
                    list![atom("inout"), ty(&p.ty)]
                } else {
                    ty(&p.ty)
                }
            });
            list![atom("fn"), tagged("params", ps), ty(ret)]
        }
        TypeKind::SelfType => atom("self"),
        TypeKind::Void => atom("void"),
        TypeKind::Never => atom("never"),
        TypeKind::Null => atom("null"),
        TypeKind::Error => atom("<error>"),
    }
}

fn block(b: &Block) -> S {
    tagged("block", b.stmts.iter().map(stmt))
}

fn stmt(s: &Stmt) -> S {
    match &s.kind {
        StmtKind::Block(b) => block(b),
        StmtKind::Local {
            readonly,
            name,
            ty: t,
            value,
        } => {
            let tag = if *readonly { "readonly-local" } else { "local" };
            list![atom(tag), var(name), ty(t), expr(value)]
        }
        StmtKind::Const(c) => const_decl(c),
        StmtKind::Destructure { pattern: p, value } => {
            list![atom("destructure"), pattern(p), expr(value)]
        }
        StmtKind::If {
            cond,
            then,
            elseifs,
            otherwise,
        } => {
            let mut v = vec![atom("if"), expr(cond), block(then)];
            for (c, b) in elseifs {
                v.push(list![atom("elseif"), expr(c), block(b)]);
            }
            if let Some(b) = otherwise {
                v.push(list![atom("else"), block(b)]);
            }
            S::List(v)
        }
        StmtKind::While { cond, body } => list![atom("while"), expr(cond), block(body)],
        StmtKind::DoWhile { body, cond } => list![atom("do-while"), block(body), expr(cond)],
        StmtKind::For {
            init,
            cond,
            step,
            body,
        } => {
            let init = match init {
                ForInit::None => atom("_"),
                ForInit::Decl(d) => list![atom("local"), var(&d.name), ty(&d.ty), expr(&d.value)],
                ForInit::Exprs(es) => tagged("exprs", es.iter().map(expr)),
            };
            list![
                atom("for"),
                init,
                cond.as_ref().map_or_else(|| atom("_"), expr),
                tagged("exprs", step.iter().map(expr)),
                block(body)
            ]
        }
        StmtKind::Foreach {
            subject,
            key,
            value,
            body,
        } => {
            let mut v = vec![atom("foreach"), expr(subject)];
            if let Some(k) = key {
                v.push(list![atom("key"), var(k)]);
            }
            v.push(pattern(value));
            v.push(block(body));
            S::List(v)
        }
        StmtKind::Try {
            body,
            catches,
            finally,
        } => {
            let mut v = vec![atom("try"), block(body)];
            for c in catches {
                let mut cv = vec![atom("catch")];
                if let Some(var_) = &c.var {
                    cv.push(var(var_));
                }
                cv.push(tagged("types", c.types.iter().map(named)));
                cv.push(block(&c.body));
                v.push(S::List(cv));
            }
            if let Some(f) = finally {
                v.push(list![atom("finally"), block(f)]);
            }
            S::List(v)
        }
        StmtKind::Match(m) => match_(m, "match-stmt"),
        StmtKind::Echo(es) => tagged("echo", es.iter().map(expr)),
        StmtKind::Print(e) => list![atom("print"), expr(e)],
        StmtKind::Exit => list![atom("exit-stmt")],
        StmtKind::Return(e) => tagged("return", e.iter().map(expr)),
        StmtKind::Break(n) => tagged("break", n.iter().map(|n| atom(n.to_string()))),
        StmtKind::Continue(n) => tagged("continue", n.iter().map(|n| atom(n.to_string()))),
        StmtKind::Expr(e) => expr(e),
    }
}

fn match_(m: &Match, tag: &str) -> S {
    let mut v = vec![atom(tag)];
    if let Some(s) = &m.subject {
        v.push(list![atom("subject"), expr(s)]);
    }
    for arm in &m.arms {
        let mut av = vec![atom("arm")];
        match &arm.test {
            ArmTest::Default => av.push(atom("default")),
            ArmTest::Patterns(ps) => av.extend(ps.iter().map(pattern)),
            ArmTest::Conditions(cs) => av.push(tagged("when", cs.iter().map(expr))),
        }
        if let Some(g) = &arm.guard {
            av.push(list![atom("if"), expr(g)]);
        }
        av.push(match &arm.body {
            ArmBody::Expr(e) => list![atom("=>"), expr(e)],
            ArmBody::Block(b) => list![atom("=>"), block(b)],
        });
        v.push(S::List(av));
    }
    S::List(v)
}

fn pattern(p: &Pattern) -> S {
    match &p.kind {
        PatternKind::Wildcard => atom("_"),
        PatternKind::Binding(i) => list![atom("bind"), var(i)],
        PatternKind::Typed { name, ty: t } => list![
            atom("typed"),
            name.as_ref().map_or_else(|| atom("_"), var),
            ty(t)
        ],
        PatternKind::Literal(e) => list![atom("lit"), expr(e)],
        PatternKind::Equals(e) => list![atom("=="), expr(e)],
        PatternKind::Range {
            start,
            end,
            inclusive,
        } => list![
            atom(if *inclusive { "..=" } else { "..<" }),
            expr(start),
            expr(end)
        ],
        PatternKind::Path {
            name,
            member,
            payload,
        } => {
            let mut v = vec![atom("path"), qname(name)];
            if let Some(m) = member {
                v.push(atom(format!("::{}", m.name)));
            }
            if let Some(ps) = payload {
                v.push(tagged("payload", ps.iter().map(pattern)));
            }
            S::List(v)
        }
        PatternKind::Type(t) => list![atom("type"), ty(t)],
        PatternKind::Struct { ty: t, fields } => {
            let mut v = vec![atom("struct-pattern"), named(t)];
            v.extend(
                fields
                    .iter()
                    .map(|(f, p)| list![atom(format!("{}:", f.name)), pattern(p)]),
            );
            S::List(v)
        }
        PatternKind::List(ps) => tagged("list-pattern", ps.iter().map(pattern)),
    }
}

fn args(a: &[Arg]) -> Vec<S> {
    a.iter()
        .map(|a| {
            let value = match &a.kind {
                ArgKind::Value(e) => expr(e),
                ArgKind::Spread(e) => list![atom("..."), expr(e)],
                ArgKind::Inout(e) => list![atom("inout"), expr(e)],
            };
            match &a.name {
                Some(n) => list![atom(format!("{}:", n.name)), value],
                None => value,
            }
        })
        .collect()
}

fn quote(s: &str) -> String {
    format!("{s:?}")
}

fn expr(e: &Expr) -> S {
    match &e.kind {
        ExprKind::Int(n) => atom(n.to_string()),
        ExprKind::Float(f) => atom(format!("{f}f")),
        ExprKind::Str(s) => atom(quote(s)),
        ExprKind::Interpolated(parts) => tagged(
            "interp",
            parts.iter().map(|p| match p {
                InterpPart::Text(t) => atom(quote(t)),
                InterpPart::Expr(e) => expr(e),
            }),
        ),
        ExprKind::Bytes(b) => tagged("bytes", b.iter().map(|x| atom(x.to_string()))),
        ExprKind::Bool(b) => atom(b.to_string()),
        ExprKind::Null => atom("null"),
        ExprKind::Var(v) => atom(format!("${v}")),
        ExprKind::Name(n) => qname(n),
        ExprKind::SelfRef => atom("self"),
        ExprKind::Parent(None) => atom("parent"),
        ExprKind::Parent(Some(n)) => list![atom("parent"), named(n)],
        ExprKind::Paren(inner) => list![atom("paren"), expr(inner)],
        ExprKind::List(items) => tagged("list", items.iter().map(expr)),
        ExprKind::Map(entries) => tagged(
            "map",
            entries
                .iter()
                .map(|(k, v)| list![atom("=>"), expr(k), expr(v)]),
        ),
        ExprKind::Unary { op, expr: inner } => list![atom(op.as_str()), expr(inner)],
        ExprKind::Binary { op, lhs, rhs } => list![atom(op.as_str()), expr(lhs), expr(rhs)],
        ExprKind::Assign { op, target, value } => {
            let tag = match op {
                None => "=".to_string(),
                Some(op) => format!("{}=", op.as_str()),
            };
            list![atom(tag), expr(target), expr(value)]
        }
        ExprKind::Cast { ty: t, expr: inner } => {
            list![atom(format!("cast-{}", t.name)), expr(inner)]
        }
        ExprKind::Ternary {
            cond,
            then,
            otherwise,
        } => list![atom("?:"), expr(cond), expr(then), expr(otherwise)],
        ExprKind::Range {
            start,
            end,
            inclusive,
        } => list![
            atom(if *inclusive { "..=" } else { "..<" }),
            expr(start),
            expr(end)
        ],
        ExprKind::InstanceOf {
            expr: inner,
            pattern: p,
        } => {
            list![atom("instanceof"), expr(inner), pattern(p)]
        }
        ExprKind::Prop {
            object,
            name,
            nullsafe,
        } => list![
            atom(if *nullsafe { "?->" } else { "->" }),
            expr(object),
            atom(&*name.name)
        ],
        ExprKind::StaticMember { class, name } => list![atom("::"), expr(class), atom(&*name.name)],
        ExprKind::StaticProp { class, name } => list![atom("::"), expr(class), var(name)],
        ExprKind::Index { object, index } => match index {
            Some(i) => list![atom("index"), expr(object), expr(i)],
            None => list![atom("push-slot"), expr(object)],
        },
        ExprKind::Call { callee, args: a } => {
            let mut v = vec![atom("call"), expr(callee)];
            v.extend(args(a));
            S::List(v)
        }
        ExprKind::CallableRef(c) => list![atom("callable"), expr(c)],
        ExprKind::New { class, args: a } => {
            let mut v = vec![atom("new"), named(class)];
            v.extend(args(a));
            S::List(v)
        }
        ExprKind::Clone { expr: inner, with } => {
            let mut v = vec![atom("clone"), expr(inner)];
            v.extend(args(with));
            S::List(v)
        }
        ExprKind::Closure(c) => list![
            atom("fn"),
            params(&c.params),
            list![atom("returns"), ty(&c.ret)],
            match &c.body {
                ClosureBody::Expr(e) => list![atom("=>"), expr(e)],
                ClosureBody::Block(b) => block(b),
            }
        ],
        ExprKind::Match(m) => match_(m, "match"),
        ExprKind::Exit(code) => tagged("exit", code.iter().map(|c| expr(c))),
        ExprKind::Throw(inner) => list![atom("throw"), expr(inner)],
        ExprKind::Error => atom("<error>"),
    }
}
