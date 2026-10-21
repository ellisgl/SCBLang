//! Compile errors.
//!
//! A [`Diagnostic`] is built with the builder methods (`with_span`, `with_note`,
//! `with_fix`) and rendered once at the end. The `kind` is a stable identifier that
//! tests and tools match on; the message is for humans and may change.

use std::fmt::Write as _;

use crate::{SourceMap, Span};

/// A compile error: a stable kind (listed in `docs/DIAGNOSTICS.md`), a message,
/// an optional source span, notes and an optional fix-it hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub kind: &'static str,
    pub message: String,
    pub span: Option<Span>,
    pub notes: Vec<String>,
    pub fix: Option<String>,
}

impl Diagnostic {
    pub fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Diagnostic {
            kind,
            message: message.into(),
            span: None,
            notes: Vec::new(),
            fix: None,
        }
    }

    pub fn with_span(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn with_fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }

    /// Renders as `path:line:col: error[kind]: message`, followed by indented
    /// `note:` and `fix:` lines. The format is documented in `docs/DIAGNOSTICS.md`.
    pub fn render(&self, sources: &SourceMap) -> String {
        let mut out = String::new();
        if let Some(span) = self.span {
            let file = sources.file(span.file);
            let (line, col) = file.line_col(span.start);
            let _ = write!(out, "{}:{}:{}: ", file.path.display(), line, col);
        }
        let _ = write!(out, "error[{}]: {}", self.kind, self.message);
        for note in &self.notes {
            let _ = write!(out, "\n  note: {note}");
        }
        if let Some(fix) = &self.fix {
            let _ = write!(out, "\n  fix: {fix}");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn renders_with_span_note_and_fix() {
        let mut map = SourceMap::new();
        let id = map.add(Path::new("t.scb"), b"x\n  y".to_vec()).unwrap();
        let d = Diagnostic::new("short-ternary", "`?:` is removed")
            .with_span(Span::new(id, 4, 5))
            .with_note("it tests truthiness")
            .with_fix("use `??`");
        assert_eq!(
            d.render(&map),
            "t.scb:2:3: error[short-ternary]: `?:` is removed\n  note: it tests truthiness\n  fix: use `??`"
        );
    }

    #[test]
    fn renders_without_span() {
        let map = SourceMap::new();
        assert_eq!(
            Diagnostic::new("missing-main", "no main.scb").render(&map),
            "error[missing-main]: no main.scb"
        );
    }
}
