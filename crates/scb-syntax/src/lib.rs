//! SCB syntax: source files, spans, diagnostics, tokens and the lexer.
//!
//! The parser (M2) lands here too.

mod diagnostic;
mod lexer;
mod source;
mod token;

pub use diagnostic::Diagnostic;
pub use lexer::lex;
pub use source::{FileId, SourceFile, SourceMap, Span};
pub use token::{Keyword, Punct, StrLit, StrPart, Token, TokenKind};
