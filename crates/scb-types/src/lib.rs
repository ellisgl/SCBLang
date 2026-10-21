//! SCB type checking and the compiled program.
//!
//! [`check`] is the front end's entry point: source files in, a [`Program`] or
//! diagnostics out. [`Program`] is the parsed, type-checked program. It is immutable
//! once built and shared between isolates as `Arc<Program>` (CLAUDE.md architecture
//! rule 3).

use scb_syntax::{Diagnostic, SourceMap};

/// A compiled program. Immutable; `Send + Sync` so one copy can serve many isolates.
#[derive(Debug)]
pub struct Program {
    sources: SourceMap,
}

impl Program {
    pub fn sources(&self) -> &SourceMap {
        &self.sources
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

/// The front end proper: lex, parse (and from M3, check) every file.
///
/// M2: every file is lexed and parsed, and lexical and syntax errors are reported. The
/// checker (M3) does not exist yet, so a file that parses cleanly still fails with
/// `not-implemented`.
fn compile(sources: SourceMap) -> CheckResult {
    let mut diagnostics = Vec::new();
    let mut decls = 0;
    for (id, file) in sources.files() {
        let (ast, diags) = scb_syntax::parse_source(id, &file.text);
        decls += ast.map_or(0, |a| a.decls.len());
        diagnostics.extend(diags);
    }
    if diagnostics.is_empty() {
        diagnostics.push(
            Diagnostic::new(
                "not-implemented",
                "the SCB type checker is not implemented yet",
            )
            .with_note(format!(
                "{} file(s) parsed, {decls} declaration(s)",
                sources.len()
            )),
        );
    }
    Err((sources, diagnostics))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn program_is_send_and_sync() {
        assert_send_sync::<Program>();
    }

    #[test]
    fn check_reports_not_implemented() {
        let (_, diags) = check(SourceMap::new()).unwrap_err();
        assert_eq!(diags[0].kind, "not-implemented");
    }

    fn check_body(body: &str) -> Vec<&'static str> {
        let mut sources = SourceMap::new();
        let text = format!("namespace T;\nfunction main(): void\n{{\n{body}\n}}\n");
        sources
            .add(std::path::Path::new("t.scb"), text.into_bytes())
            .unwrap();
        let (_, diags) = check(sources).unwrap_err();
        diags.iter().map(|d| d.kind).collect()
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
            assert_eq!(check_body(&body), ["not-implemented"]);
        }
    }
}
