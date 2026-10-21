//! SCB type checking and the compiled program.
//!
//! [`check`] is the front end's entry point: source files in, a [`Program`] or
//! diagnostics out. [`Program`] is the parsed, type-checked program. It is immutable
//! once built and shared between isolates as `Arc<Program>` (CLAUDE.md architecture
//! rule 3).

mod check;
mod collect;
pub mod decls;
mod resolve;
mod stdlib;
pub mod tir;
pub mod ty;
mod validate;

use std::path::Path;

use scb_syntax::{Diagnostic, SourceMap, ast};

use crate::decls::Decls;
use crate::resolve::{ParsedFile, Uses, qname_text};
use crate::tir::FuncId;
use crate::ty::Types;

/// A compiled program. Immutable; `Send + Sync` so one copy can serve many isolates.
#[derive(Debug)]
pub struct Program {
    /// User files first, then the stdlib stubs (as `<stdlib>/name.scb`).
    pub sources: SourceMap,
    pub types: Types,
    pub decls: Decls,
    /// Every function body, indexed by [`FuncId`]: functions, methods (including
    /// stdlib builtins), closures.
    pub funcs: Vec<tir::Func>,
    /// Initializer of each constant (namespace and class), indexed by `ConstId`.
    pub consts: Vec<tir::Init>,
    /// Initializer of each static property (evaluated lazily), indexed by `StaticId`.
    pub statics: Vec<tir::Init>,
    /// Default values of each type's own instance properties, indexed by `DeclId`:
    /// evaluated for every new instance, superclass defaults first, before the
    /// constructor runs.
    pub field_defaults: Vec<Vec<(decls::FieldIdx, tir::Init)>>,
    /// The value of each case of a backed enum, indexed by `DeclId` then case
    /// (`None` for other types and cases).
    pub case_values: Vec<Vec<Option<tir::Init>>>,
    /// The program's `main`.
    pub main: FuncId,
}

impl Program {
    pub fn sources(&self) -> &SourceMap {
        &self.sources
    }
}

/// Checker state shared by all passes. `files` are the parsed user files and stubs;
/// everything else is built up by the passes and moved into the [`Program`].
pub(crate) struct Ctx<'a> {
    pub files: &'a [ParsedFile],
    pub sources: &'a SourceMap,
    pub types: Types,
    pub decls: Decls,
    /// Bodies, filled as they are checked (`None` until then).
    pub funcs: Vec<Option<tir::Func>>,
    pub consts: Vec<Option<tir::Init>>,
    pub statics: Vec<Option<tir::Init>>,
    pub field_defaults: Vec<Vec<(decls::FieldIdx, tir::Init)>>,
    pub case_values: Vec<Vec<Option<tir::Init>>>,
    /// Declared `map` / `set` types, checked for `Hashable` keys once every type's
    /// fields are known (`check_bodies`).
    pub pending_hashable: Vec<(ty::Ty, scb_syntax::Span)>,
    pub diags: Vec<Diagnostic>,
    /// The bodies being checked: a function and the closures nested in it.
    pub frames: Vec<check::Frame>,
}

impl<'a> Ctx<'a> {
    fn new(files: &'a [ParsedFile], sources: &'a SourceMap) -> Self {
        Ctx {
            files,
            sources,
            types: Types::new(),
            decls: Decls::default(),
            funcs: Vec::new(),
            consts: Vec::new(),
            statics: Vec::new(),
            field_defaults: Vec::new(),
            case_values: Vec::new(),
            pending_hashable: Vec::new(),
            diags: Vec::new(),
            frames: Vec::new(),
        }
    }

    pub fn report(&mut self, d: Diagnostic) {
        self.diags.push(d);
    }
}

/// Stack size of the compiler thread. The parser and checker are recursive; the
/// parser caps tree depth (`too-deeply-nested`), and this stack covers that depth
/// with room to spare even in unoptimized builds, whose frames are several times
/// larger. Only touched pages are committed, so the size costs nothing up front.
const COMPILER_STACK: usize = 64 * 1024 * 1024;

/// Result of a compilation: the program, or the sources back with the diagnostics.
pub type CheckResult = Result<Program, (SourceMap, Vec<Diagnostic>)>;

/// Parses and type-checks a compilation.
///
/// Runs on a dedicated thread with a [`COMPILER_STACK`]-sized stack, so the caller's
/// stack size (the main thread's 8 MiB, a test thread's 2 MiB) never limits how
/// deeply nested a program may be.
pub fn check(sources: SourceMap) -> CheckResult {
    let compiler = std::thread::Builder::new()
        .name("scb-compiler".into())
        .stack_size(COMPILER_STACK)
        .spawn(move || compile(sources));
    match compiler {
        // A panic in the compiler is a bug; re-raise it on the caller's thread.
        Ok(handle) => handle
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
        // Only under OS resource exhaustion. The sources moved into the failed
        // closure, so the diagnostic carries no position.
        Err(e) => Err((
            SourceMap::new(),
            vec![Diagnostic::new(
                "internal-error",
                format!("cannot start the compiler thread: {e}"),
            )],
        )),
    }
}

/// The front end proper: lex and parse every file, then collect declarations, check
/// every body and assemble the [`Program`].
fn compile(mut sources: SourceMap) -> CheckResult {
    let mut diagnostics = Vec::new();
    let mut files = Vec::new();
    let user: Vec<_> = sources.files().map(|(id, _)| id).collect();
    for id in user {
        let (ast, diags) = scb_syntax::parse_source(id, &sources.file(id).text);
        diagnostics.extend(diags);
        if let Some(ast) = ast {
            let path = sources.file(id).path.clone();
            let main_file = path.file_name().is_some_and(|n| n == "main.scb") || sources.len() == 1;
            let mut f = parsed_file(ast, false);
            f.main_file = main_file;
            f.path = path.display().to_string();
            files.push(f);
        }
    }
    if !diagnostics.is_empty() {
        return Err((sources, diagnostics));
    }

    for (name, text) in stdlib::stubs() {
        let path = format!("<stdlib>/{name}");
        let id = match sources.add(Path::new(&path), text.into_bytes()) {
            Ok(id) => id,
            Err(d) => return Err((sources, vec![d])),
        };
        let (tokens, mut diags) = scb_syntax::lex(id, &sources.file(id).text);
        let (ast, parse_diags) = scb_syntax::parse_stdlib(id, tokens);
        diags.extend(parse_diags);
        if !diags.is_empty() {
            // A broken stub is a bug in this crate, not in the user's program.
            let mut out = vec![Diagnostic::new(
                "internal-error",
                format!("stdlib stub {path} does not parse"),
            )];
            out.extend(diags);
            return Err((sources, out));
        }
        files.push(parsed_file(ast, true));
    }

    let mut ctx = Ctx::new(&files, &sources);
    ctx.register();
    ctx.headers();
    ctx.members();
    ctx.validate();
    let main = ctx.check_bodies();
    let Ctx {
        types,
        decls,
        funcs,
        consts,
        statics,
        field_defaults,
        case_values,
        diags,
        ..
    } = ctx;
    let main = match main {
        Some(main) if diags.is_empty() => main,
        _ => return Err((sources, diags)),
    };
    Ok(Program {
        sources,
        types,
        decls,
        funcs: funcs
            .into_iter()
            .map(|f| f.expect("every body is checked"))
            .collect(),
        consts: consts
            .into_iter()
            .map(|c| c.expect("every constant is checked"))
            .collect(),
        statics: statics
            .into_iter()
            .map(|s| s.expect("every static is checked"))
            .collect(),
        field_defaults,
        case_values,
        main,
    })
}

/// A parsed file with its namespace and `use` imports. `namespace Prelude;` (stubs
/// only) is the root namespace.
fn parsed_file(ast: ast::File, stdlib: bool) -> ParsedFile {
    let mut namespace = qname_text(&ast.namespace);
    if stdlib && namespace == stdlib::PRELUDE_NAMESPACE {
        namespace.clear();
    }
    let mut uses = Uses::default();
    for u in &ast.uses {
        let fqn = qname_text(&u.name);
        let alias = u.alias.as_ref().map_or_else(
            || {
                u.name
                    .parts
                    .last()
                    .map_or(String::new(), |p| p.name.to_string())
            },
            |a| a.name.to_string(),
        );
        let table = match u.kind {
            ast::UseKind::Type => &mut uses.types,
            ast::UseKind::Function => &mut uses.functions,
            ast::UseKind::Const => &mut uses.consts,
        };
        table.insert(alias, fqn);
    }
    ParsedFile {
        ast,
        namespace,
        stdlib,
        uses,
        main_file: false,
        path: String::new(),
    }
}

#[cfg(test)]
#[path = "check_tests.rs"]
mod check_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn program_is_send_and_sync() {
        assert_send_sync::<Program>();
    }

    fn check_body(body: &str) -> Vec<&'static str> {
        let mut sources = SourceMap::new();
        let text = format!("namespace T;\nfunction main(): void\n{{\n{body}\n}}\n");
        sources
            .add(std::path::Path::new("t.scb"), text.into_bytes())
            .unwrap();
        match check(sources) {
            Ok(_) => Vec::new(),
            Err((_, diags)) => diags.iter().map(|d| d.kind).collect(),
        }
    }

    /// Regression: these inputs used to overflow the stack and abort the process.
    #[test]
    fn pathological_nesting_is_a_diagnostic_not_a_crash() {
        let n = 100_000;
        let parens = format!("$x: int = {}1{};", "(".repeat(n), ")".repeat(n));
        let blocks = format!("{}{}", "{".repeat(n), "}".repeat(n));
        let chain = format!("$x: int = 1{};", " + 1".repeat(n));
        let negs = format!("$x: int = {}1;", "-".repeat(n));
        let calls = format!("$x: int = f(){};", "->g()".repeat(n));
        let types = format!("$x: {}int{} = 1;", "(".repeat(n), ")".repeat(n));
        // `"{$a["{$a["…"]}"]}"`: each level is a string inside an interpolation.
        let strings = format!("$s: string = {}0{};", "\"{$a[".repeat(n), "]}\"".repeat(n));
        for (name, body) in [
            ("parens", parens),
            ("blocks", blocks),
            ("chain", chain),
            ("negs", negs),
            ("calls", calls),
            ("types", types),
            ("strings", strings),
        ] {
            assert_eq!(check_body(&body), ["too-deeply-nested"], "{name}");
        }
    }

    #[test]
    fn nesting_below_the_limit_is_accepted() {
        let parens = format!("$x: int = {}1{};", "(".repeat(400), ")".repeat(400));
        let chain = format!("$x: int = 1{};", " + 1".repeat(400));
        for body in [parens, chain] {
            assert_eq!(check_body(&body), Vec::<&str>::new());
        }
    }
}
