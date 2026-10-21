//! SCB syntax: source files, spans and diagnostics.
//!
//! The lexer (M1) and parser (M2) land here; M0 provides the source map and
//! the diagnostic type every later stage reports through.

mod diagnostic;
mod source;

pub use diagnostic::Diagnostic;
pub use source::{FileId, SourceFile, SourceMap, Span};
