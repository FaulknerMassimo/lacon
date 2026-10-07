//! The Lacon type checker. It runs on the resolved IR (names are already
//! slots and `it` arguments are lambdas), infers the type of every
//! expression, and reports errors in the same one-line format as the
//! resolver. Signatures are declared; bodies are inferred.

mod infer;
mod methods;
pub mod types;

use lacon_interp::ir::Program;
use lacon_syntax::Diag;

pub use infer::Checker;

/// Type-checks a resolved program. `src` is its source text, used for the
/// snippets in messages and fixes. Records in `prog.parse_to` the type each
/// `s.parse()` with a declared type reads.
pub fn check(prog: &mut Program, src: &str) -> Vec<Diag> {
    let mut c = Checker::new(prog, src);
    c.run();
    let (diags, parse_to) = (c.diags, c.parse_to);
    prog.parse_to = parse_to.into_iter().collect();
    diags
}
