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
/// snippets in messages and fixes.
pub fn check(prog: &Program, src: &str) -> Vec<Diag> {
    let mut c = Checker::new(prog, src);
    c.run();
    c.diags
}
