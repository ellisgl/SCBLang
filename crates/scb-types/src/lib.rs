//! SCB type checking and the compiled program.
//!
//! [`Program`] is the parsed, type-checked program. It is immutable once built and
//! shared between isolates as `Arc<Program>` (CLAUDE.md architecture rule 3).

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

/// Parses and type-checks a compilation.
///
/// M1: every file is lexed and lexical errors are reported. The parser (M2) and the
/// checker (M3) do not exist yet, so a file that lexes cleanly still fails with
/// `not-implemented`.
pub fn check(sources: SourceMap) -> Result<Program, (SourceMap, Vec<Diagnostic>)> {
    let mut diagnostics = Vec::new();
    let mut tokens = 0;
    for (id, file) in sources.files() {
        let (toks, diags) = scb_syntax::lex(id, &file.text);
        tokens += toks.len();
        diagnostics.extend(diags);
    }
    if diagnostics.is_empty() {
        diagnostics.push(
            Diagnostic::new("not-implemented", "the SCB parser is not implemented yet").with_note(
                format!("{} file(s) lexed, {tokens} token(s)", sources.len()),
            ),
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
}
