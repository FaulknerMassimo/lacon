//! Overflow checks a loop's ranges make unnecessary. In
//!
//! ```text
//! while c >= wi:
//!   cand = best[c - wi] + vi
//!   ...
//!   c -= 1
//! ```
//!
//! `c` stays between `wi` and its value when the loop starts, so neither
//! `c - wi` nor `c -= 1` can overflow unless `c0 - wi` or `wi - 1` does.
//! One test of those before the loop stands for a check at every
//! iteration: the generated code computes each such operation as
//! `lc_isub_p(ok, ...)`, wrapping when the test passed and checked as
//! usual when it didn't, and the C compiler makes two loops of the one
//! that tests `ok`. The checks are only what stands between the operands
//! and the result (`jo` doesn't fuse with the `sub` before it), and a
//! checked `c - wi` keeps GCC from folding it into an address.
//!
//! The analysis is interval arithmetic over a loop's `+`, `-` and `*` by
//! a literal whose leaves are literals, ints the loop never writes, and
//! its induction variable:
//!
//! - the variable of a `for` over a range, between the range's bounds;
//! - a `while` loop's variable `c` whose only write is one `c -= k` or
//!   `c += k` among the body's own statements, with `k` a literal or an
//!   int the loop never writes, when the condition (or one side of an
//!   `and` in it) bounds `c` the other way: `c >= lo`, `c > lo` going
//!   down, `c < hi`, `c <= hi` going up. Before the step `c` is between
//!   the bound and its start; the step and what follows it are shifted
//!   by `k`.
//!
//! An operation's interval is computed from its operands' with the
//! endpoints checked, and the test is that no endpoint overflowed: the
//! exact results lie between the endpoints, so if those fit, every result
//! does. Operations inside a nested loop are left to that loop, which
//! treats this loop's variable as fixed.

use std::collections::HashSet;

use lacon_interp::ir::*;
use lacon_interp::value::Value;
use lacon_syntax::ast::{BinOp, CmpOp};

use super::pat_slots;

/// The loop being generated.
pub enum Loop<'a> {
    /// `while cond`.
    While(&'a Ex),
    /// `for` over the ints from C variable `a` up to, not including, `b`,
    /// with the loop variable in `slot` if it's an unboxed int.
    Range { slot: Option<u32>, a: &'a str, b: &'a str },
}

/// What a loop's test proves: `pre` is C to run as the loop starts, which
/// sets bool `flag`, and `ops` the operations (`Ex::Binary` and compound
/// `St::Assign` nodes, by address) that can't overflow when it's set.
pub struct Proof {
    pub pre: String,
    pub ops: Vec<usize>,
}

pub fn st_key(s: &St) -> usize {
    s as *const St as usize
}

/// A C expression's bounds.
type Iv = (String, String);

struct An<'a> {
    /// The guard flag's C name.
    flag: &'a str,
    /// Slots the loop writes.
    written: HashSet<u32>,
    is_int: &'a dyn Fn(u32) -> bool,
    /// The induction variable and its bounds where the walk is.
    iv: Option<(u32, Iv)>,
    /// Endpoint expressions to test, and the operations proven.
    terms: Vec<String>,
    ops: Vec<usize>,
}

/// The slots the statements write: bindings, assignments of any kind and
/// pattern variables, in nested loops and blocks too. Lambdas can't write
/// an unboxed slot (one they use is boxed), so they are left out.
fn writes(cond: Option<&Ex>, body: &[St]) -> (HashSet<u32>, Vec<(u32, usize)>) {
    let mut out = HashSet::new();
    let mut counts: Vec<(u32, usize)> = Vec::new();
    let mut f = |n: Node, d: u32| {
        if d != 0 {
            return;
        }
        let mut bound = Vec::new();
        match n {
            Node::St(St::Bind(p, ..)) | Node::St(St::For { pat: p, .. }) => pat_slots(p, &mut bound),
            Node::St(St::Assign { place: Place { root: Root::Local(s), .. }, .. }) => bound.push(*s),
            Node::St(St::AssignMulti { targets, .. }) => {
                for t in targets {
                    match t {
                        Target::Bind(s) | Target::Place(Place { root: Root::Local(s), .. }) => bound.push(*s),
                        _ => {}
                    }
                }
            }
            Node::Ex(Ex::Match { arms, .. }) => arms.iter().for_each(|a| pat_slots(&a.pat, &mut bound)),
            _ => {}
        }
        for s in bound {
            out.insert(s);
            match counts.iter_mut().find(|(x, _)| *x == s) {
                Some(c) => c.1 += 1,
                None => counts.push((s, 1)),
            }
        }
    };
    if let Some(c) = cond {
        walk_ex(c, 0, &mut f);
    }
    walk_stmts(body, 0, &mut f);
    (out, counts)
}

fn lit_c(n: i64) -> String {
    if n == i64::MIN {
        "INT64_MIN".into()
    } else {
        format!("{n}LL")
    }
}

impl An<'_> {
    /// An int the loop doesn't change, as C.
    fn fixed(&self, e: &Ex) -> Option<String> {
        match e {
            Ex::Lit(Value::Int(n), _) => Some(lit_c(*n)),
            Ex::Local(s, _) if (self.is_int)(*s) && !self.written.contains(s) => Some(format!("N{s}")),
            _ => None,
        }
    }

    fn add(&self, a: &str, b: &str) -> String {
        format!("lc_radd(&{}, {a}, {b})", self.flag)
    }

    fn sub(&self, a: &str, b: &str) -> String {
        format!("lc_rsub(&{}, {a}, {b})", self.flag)
    }

    /// `e`'s bounds where the walk is, if it is an int computed from
    /// literals, fixed ints and the induction variable.
    fn bounds(&self, e: &Ex) -> Option<Iv> {
        if let Some(c) = self.fixed(e) {
            return Some((c.clone(), c));
        }
        match e {
            Ex::Local(s, _) => self.iv.as_ref().filter(|(v, _)| v == s).map(|(_, b)| b.clone()),
            Ex::Binary { op: BinOp::Add, l, r, .. } => {
                let ((al, ah), (bl, bh)) = (self.bounds(l)?, self.bounds(r)?);
                Some((self.add(&al, &bl), self.add(&ah, &bh)))
            }
            Ex::Binary { op: BinOp::Sub, l, r, .. } => {
                let ((al, ah), (bl, bh)) = (self.bounds(l)?, self.bounds(r)?);
                Some((self.sub(&al, &bh), self.sub(&ah, &bl)))
            }
            Ex::Binary { op: BinOp::Mul, l, r, .. } => {
                let (k, x) = match (&**l, &**r) {
                    (Ex::Lit(Value::Int(k), _), x) | (x, Ex::Lit(Value::Int(k), _)) => (*k, x),
                    _ => return None,
                };
                let (xl, xh) = self.bounds(x)?;
                let mul = |v: &str| format!("lc_rmul(&{}, {v}, {})", self.flag, lit_c(k));
                Some(if k >= 0 { (mul(&xl), mul(&xh)) } else { (mul(&xh), mul(&xl)) })
            }
            _ => None,
        }
    }

    /// Proves what it can of the operations in `body`, outside nested loops.
    fn stmts(&mut self, body: &[St]) {
        let mut found = Vec::new();
        let mut nested = HashSet::new();
        walk_stmts(body, 0, &mut |n, d| match n {
            Node::Ex(b @ Ex::Binary { op: BinOp::Add | BinOp::Sub | BinOp::Mul, .. }) if d == 0 => found.push(b),
            Node::St(s @ (St::For { .. } | St::While { .. })) => nested.extend(loop_ops(s)),
            _ => {}
        });
        self.prove(found, &nested);
    }

    fn prove(&mut self, found: Vec<&Ex>, nested: &HashSet<usize>) {
        for b in found {
            let key = b as *const Ex as usize;
            if nested.contains(&key) {
                continue;
            }
            if let Some((lo, hi)) = self.bounds(b) {
                self.terms.push(lo);
                self.terms.push(hi);
                self.ops.push(key);
            }
        }
    }
}

/// The operations inside a nested loop, which that loop proves or not.
fn loop_ops(s: &St) -> Vec<usize> {
    let mut out = Vec::new();
    walk_stmts(std::slice::from_ref(s), 0, &mut |n, _| {
        if let Node::Ex(b @ Ex::Binary { .. }) = n {
            out.push(b as *const Ex as usize);
        }
    });
    out
}

/// `c` and the bound the condition `e` sets on it, going `down` (a lower
/// bound) or up: the bound as C, and whether it's strict.
fn cond_bound(e: &Ex, c: u32, down: bool, fixed: &dyn Fn(&Ex) -> Option<String>) -> Option<(String, bool)> {
    match e {
        Ex::And(a, b, _) => cond_bound(a, c, down, fixed).or_else(|| cond_bound(b, c, down, fixed)),
        Ex::Compare { first, rest, .. } if rest.len() == 1 => {
            let (op, other) = (rest[0].0, &rest[0].1);
            // As `c op bound`.
            let (op, bound) = match (&**first, other) {
                (Ex::Local(s, _), b) if *s == c => (op, b),
                (b, Ex::Local(s, _)) if *s == c => (
                    match op {
                        CmpOp::Lt => CmpOp::Gt,
                        CmpOp::Le => CmpOp::Ge,
                        CmpOp::Gt => CmpOp::Lt,
                        CmpOp::Ge => CmpOp::Le,
                        _ => return None,
                    },
                    b,
                ),
                _ => return None,
            };
            let bound = fixed(bound)?;
            match (op, down) {
                (CmpOp::Ge, true) | (CmpOp::Le, false) => Some((bound, false)),
                (CmpOp::Gt, true) | (CmpOp::Lt, false) => Some((bound, true)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The C names `flag`, and `start` for a `while` loop's variable's first
/// value, are the caller's, unique to this loop.
pub fn prove(lp: Loop, body: &[St], flag: &str, start: &str, is_int: &dyn Fn(u32) -> bool) -> Option<Proof> {
    let cond = match lp {
        Loop::While(c) => Some(c),
        Loop::Range { .. } => None,
    };
    let (written, counts) = writes(cond, body);
    let mut an = An { flag, written, is_int, iv: None, terms: Vec::new(), ops: Vec::new() };
    let mut pre = String::new();
    match lp {
        Loop::Range { slot, a, b } => {
            // The loop writes its variable, though not in the body.
            if let Some(s) = slot {
                an.written.insert(s);
            }
            if let Some(s) = slot.filter(|s| counts.iter().all(|(x, _)| x != s)) {
                an.iv = Some((s, (a.to_string(), an.sub(b, "1LL"))));
            }
            an.stmts(body);
        }
        Loop::While(cond) => {
            // The induction variable: written once, by a step among the
            // body's own statements.
            let step = body.iter().enumerate().find_map(|(i, s)| match s {
                St::Assign { place: Place { root: Root::Local(c), path, .. }, op: Some(op @ (BinOp::Add | BinOp::Sub)), value, .. }
                    if path.is_empty() && is_int(*c) && counts.iter().any(|&(x, n)| x == *c && n == 1) =>
                {
                    let fixed = |e: &Ex| an.fixed(e);
                    let k = fixed(value)?;
                    let down = *op == BinOp::Sub;
                    let (bound, strict) = cond_bound(cond, *c, down, &fixed)?;
                    Some((i, *c, k, down, bound, strict))
                }
                _ => None,
            });
            match step {
                Some((i, c, k, down, bound, strict)) => {
                    let _ = std::fmt::Write::write_fmt(&mut pre, format_args!("int64_t {start} = N{c}; "));
                    // A step that isn't a literal must not turn the loop around.
                    if !matches!(&body[i], St::Assign { value: Ex::Lit(Value::Int(n), _), .. } if *n >= 0) {
                        an.terms.push(format!("({flag} &= {k} >= 0, 0)"));
                    }
                    let before = if down {
                        (if strict { an.add(&bound, "1LL") } else { bound }, start.to_string())
                    } else {
                        (start.to_string(), if strict { an.sub(&bound, "1LL") } else { bound })
                    };
                    let after = if down {
                        (an.sub(&before.0, &k), an.sub(&before.1, &k))
                    } else {
                        (an.add(&before.0, &k), an.add(&before.1, &k))
                    };
                    an.iv = Some((c, before));
                    an.stmts(&body[..i]);
                    // The step itself.
                    an.terms.push(after.0.clone());
                    an.terms.push(after.1.clone());
                    an.ops.push(st_key(&body[i]));
                    an.iv = Some((c, after));
                    an.stmts(&body[i + 1..]);
                }
                None => an.stmts(body),
            }
        }
    }
    if an.ops.is_empty() {
        return None;
    }
    let mut seen = HashSet::new();
    let _ = std::fmt::Write::write_fmt(&mut pre, format_args!("bool {flag} = true; "));
    // A bound that is a literal or a variable tests nothing.
    for t in &an.terms {
        if t.contains(flag) && seen.insert(t.clone()) {
            let _ = std::fmt::Write::write_fmt(&mut pre, format_args!("(void)({t}); "));
        }
    }
    Some(Proof { pre, ops: an.ops })
}
