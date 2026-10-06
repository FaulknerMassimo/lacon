//! Lexer, parser and diagnostics for Lacon.

pub mod ast;
pub mod diag;
pub mod fmtspec;
pub mod lexer;
pub mod parser;
pub mod span;

pub use diag::Diag;
pub use parser::parse;
pub use span::{Source, Span};
