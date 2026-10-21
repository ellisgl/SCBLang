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
/// M0: the front end does not exist yet, so every compilation fails with
/// `not-implemented`. The lexer arrives in M1, the parser in M2, the checker in M3.
pub fn check(sources: SourceMap) -> Result<Program, (SourceMap, Vec<Diagnostic>)> {
    let diagnostic = Diagnostic::new(
        "not-implemented",
        "the SCB front end is not implemented yet",
    )
    .with_note(format!("{} source file(s) loaded", sources.len()));
    Err((sources, vec![diagnostic]))
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
