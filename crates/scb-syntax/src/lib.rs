//! SCB syntax: source files, spans, diagnostics, tokens, the lexer, the AST and the
//! parser.

pub mod ast;
mod diagnostic;
mod dump;
mod lexer;
mod parser;
mod source;
mod token;

pub use diagnostic::Diagnostic;
pub use dump::{dump_expr, dump_file, dump_stmts};
pub use lexer::lex;
pub use parser::{parse, parse_source};
pub use source::{FileId, SourceFile, SourceMap, Span};
pub use token::{Keyword, Punct, StrLit, StrPart, Token, TokenKind};
