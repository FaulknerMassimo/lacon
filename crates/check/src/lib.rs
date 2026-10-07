//! The Lacon type checker. It runs on the resolved IR (names are already
//! slots and `it` arguments are lambdas), infers the type of every
//! expression, and reports errors in the same one-line format as the
//! resolver. Signatures are declared; bodies are inferred.

mod infer;
mod methods;
pub mod types;

use std::collections::HashMap;

use lacon_interp::ir::{Ex, Program};
use lacon_syntax::Diag;

pub use infer::Checker;
pub use types::T;

/// What the checker inferred, for the backend: the type of each expression
/// and of each slot, with variables resolved as far as they go.
#[derive(Default)]
pub struct Types {
    /// By the address of the expression node.
    pub exprs: HashMap<usize, T>,
    /// Slot types of each function, lambda, constant and default, by the
    /// address of its body.
    pub frames: HashMap<usize, Vec<T>>,
    /// No unknown type met a known one anywhere outside tests, and no
    /// operation went unchecked, so the types hold at run time: an `int`
    /// expression always evaluates to an int.
    pub sound: bool,
}

impl Types {
    pub fn of(&self, e: &Ex) -> Option<&T> {
        self.exprs.get(&(e as *const Ex as usize))
    }

    pub fn frame(&self, body: &Ex) -> Option<&[T]> {
        self.frames.get(&(body as *const Ex as usize)).map(|v| v.as_slice())
    }
}

/// Type-checks a resolved program. `src` is its source text, used for the
/// snippets in messages and fixes. Records in `prog.parse_to` the type each
/// `s.parse()` with a declared type reads.
pub fn check(prog: &mut Program, src: &str) -> (Vec<Diag>, Types) {
    let mut c = Checker::new(prog, src);
    c.run();
    let mut types = Types { sound: c.s.leaks == 0, ..Types::default() };
    for (k, t) in &c.ex_log {
        types.exprs.insert(*k, c.s.zonk(t));
    }
    for (k, ts) in &c.frame_log {
        types.frames.insert(*k, ts.iter().map(|t| c.s.zonk(t)).collect());
    }
    let (diags, parse_to) = (c.diags, c.parse_to);
    prog.parse_to = parse_to.into_iter().collect();
    (diags, types)
}
