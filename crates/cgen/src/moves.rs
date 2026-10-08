//! Last uses. A read of a variable that nothing reads afterwards can move
//! the value out of its slot instead of retaining it, so whatever takes the
//! value (`+`, `sort`, a parameter, a binding) may hold the only reference
//! and change it in place, as Perceus reuse does.
//!
//! This is liveness over a frame's statements, backwards. Its unit is an
//! expression without statements of its own to step through: C leaves the
//! order of some evaluations in the generated code open, so a read moves
//! only if it is its unit's one mention of the slot, and only if the unit
//! has no loop that could read it again.

use std::collections::{HashMap, HashSet};

use lacon_interp::builtins::is_mutator;
use lacon_interp::ir::*;

/// A set of slots.
#[derive(Clone, PartialEq)]
struct Live(Vec<u64>);

impl Live {
    fn new(n: u32) -> Live {
        Live(vec![0; (n as usize).div_ceil(64).max(1)])
    }
    fn has(&self, s: u32) -> bool {
        self.0.get(s as usize / 64).is_some_and(|w| w >> (s % 64) & 1 == 1)
    }
    fn add(&mut self, s: u32) {
        let i = s as usize / 64;
        if i >= self.0.len() {
            self.0.resize(i + 1, 0);
        }
        self.0[i] |= 1 << (s % 64);
    }
    fn del(&mut self, s: u32) {
        if let Some(w) = self.0.get_mut(s as usize / 64) {
            *w &= !(1 << (s % 64));
        }
    }
    fn union(&mut self, o: &Live) {
        if o.0.len() > self.0.len() {
            self.0.resize(o.0.len(), 0);
        }
        for (a, b) in self.0.iter_mut().zip(&o.0) {
            *a |= b;
        }
    }
}

/// The reads in the body of a frame with `nslots` slots that move their
/// slot's value: `Ex::Local` nodes and the places of builtin methods'
/// receivers, by address. Only slots `movable` allows are moved.
pub fn last_uses(body: &Ex, nslots: u32, movable: &dyn Fn(u32) -> bool) -> HashSet<usize> {
    let mut an = An { movable, n: nslots, dry: 0, loops: Vec::new(), moves: HashSet::new() };
    an.ex(body, Live::new(nslots));
    an.moves
}

pub fn ex_key(e: &Ex) -> usize {
    e as *const Ex as usize
}

pub fn place_key(p: &Place) -> usize {
    p as *const Place as usize
}

struct An<'a> {
    movable: &'a dyn Fn(u32) -> bool,
    n: u32,
    /// Above zero while a loop's live sets are still being found: reads
    /// aren't marked until they are.
    dry: u32,
    /// The enclosing loops' live sets: at the top of an iteration, and
    /// after the loop.
    loops: Vec<(Live, Live)>,
    moves: HashSet<usize>,
}

impl An<'_> {
    /// What is live before `e`, given what is live after it. Blocks, `if`
    /// and `match` are followed through; anything else is one unit.
    fn ex(&mut self, e: &Ex, out: Live) -> Live {
        match e {
            Ex::Block(stmts, tail) => {
                let mut l = match tail {
                    Some(t) => self.ex(t, out),
                    None => out,
                };
                for s in stmts.iter().rev() {
                    l = self.st(s, l);
                }
                l
            }
            Ex::If { cond, then, els, .. } => {
                let mut l = self.ex(then, out.clone());
                let le = match els {
                    Some(x) => self.ex(x, out),
                    None => out,
                };
                l.union(&le);
                self.unit(&[cond], &[], l)
            }
            Ex::Match { scrut, arms, .. } => {
                // An arm whose pattern or guard fails goes on to the next.
                let mut next = out.clone();
                for a in arms.iter().rev() {
                    let mut l = self.ex(&a.body, out.clone());
                    if let Some(g) = &a.guard {
                        l.union(&next);
                        l = self.unit(&[g], &[], l);
                    }
                    l.union(&next);
                    next = l;
                }
                self.unit(&[scrut], &[], next)
            }
            _ => self.unit(&[e], &[], out),
        }
    }

    fn st(&mut self, s: &St, out: Live) -> Live {
        match s {
            St::Expr(e, _) => self.ex(e, out),
            St::Bind(pat, e, _, _) => {
                let mut o = out;
                let mut bound = Vec::new();
                crate::pat_slots(pat, &mut bound);
                bound.into_iter().for_each(|b| o.del(b));
                self.ex(e, o)
            }
            St::Assign { place: Place { root: Root::Local(r), path, .. }, op: None, value, .. } if path.is_empty() => {
                let mut o = out;
                o.del(*r);
                self.ex(value, o)
            }
            // The place is read after the value.
            St::Assign { place, value, .. } => self.unit(&[value], &[place], out),
            St::AssignMulti { targets, value, .. } => {
                let mut o = out;
                let mut places = Vec::new();
                for t in targets {
                    match t {
                        Target::Bind(b) => o.del(*b),
                        Target::Place(p) => places.push(p),
                    }
                }
                self.unit(&[value], &places, o)
            }
            St::For { pat, iter, body, .. } => {
                let mut bound = Vec::new();
                crate::pat_slots(pat, &mut bound);
                let head = self.fix(&out, &mut |an: &mut An, head: &Live| {
                    let mut l = an.body(body, head, &out);
                    bound.iter().for_each(|&b| l.del(b));
                    l.union(&out);
                    l
                });
                self.body(body, &head, &out);
                self.unit(&[iter], &[], head)
            }
            St::While { cond, body, .. } => {
                let head = self.fix(&out, &mut |an: &mut An, head: &Live| {
                    let mut l = an.body(body, head, &out);
                    l.union(&out);
                    an.unit(&[cond], &[], l)
                });
                let mut l = self.body(body, &head, &out);
                l.union(&out);
                self.unit(&[cond], &[], l)
            }
            St::Break(_) => self.loops.last().map_or(out, |(_, exit)| exit.clone()),
            St::Continue(_) => self.loops.last().map_or(out, |(head, _)| head.clone()),
            St::Return(e, _) => match e {
                Some(e) => self.ex(e, Live::new(self.n)),
                None => Live::new(self.n),
            },
            St::Fail(e, _) => self.unit(&[e], &[], Live::new(self.n)),
            St::Assert { cond, msg, .. } => match msg {
                Some(m) => self.unit(&[cond, m], &[], out),
                None => self.unit(&[cond], &[], out),
            },
        }
    }

    /// A loop's live set at the top of an iteration: `step` gives it from a
    /// guess, starting from what is live after the loop, until it holds.
    /// Nothing is marked on the way.
    fn fix(&mut self, exit: &Live, step: &mut dyn FnMut(&mut An, &Live) -> Live) -> Live {
        self.dry += 1;
        let mut head = exit.clone();
        loop {
            let next = step(self, &head);
            if next == head {
                break;
            }
            head = next;
        }
        self.dry -= 1;
        head
    }

    /// What is live before a loop's body, given its live set at the top of
    /// an iteration and after the loop.
    fn body(&mut self, body: &[St], head: &Live, exit: &Live) -> Live {
        self.loops.push((head.clone(), exit.clone()));
        let mut l = head.clone();
        for s in body.iter().rev() {
            l = self.st(s, l);
        }
        self.loops.pop();
        l
    }

    /// What is live before a unit of expressions and places, given what is
    /// live after it, marking the reads that move.
    fn unit(&mut self, exs: &[&Ex], places: &[&Place], mut out: Live) -> Live {
        // Per slot: mentions, and the read that would move it.
        let mut uses: HashMap<u32, (u32, Option<usize>)> = HashMap::new();
        let mut recvs = HashSet::new();
        let (mut looped, mut jumps) = (false, false);
        let mut f = |n: Node, d: u32| {
            let mention = |uses: &mut HashMap<u32, (u32, Option<usize>)>, s: u32, key: Option<usize>| {
                let u = uses.entry(s).or_default();
                u.0 += 1;
                u.1 = key;
            };
            match n {
                Node::Ex(e @ Ex::Local(s, _)) if d == 0 => mention(&mut uses, *s, Some(ex_key(e))),
                Node::Ex(Ex::Up(k, s, _)) if *k == d => mention(&mut uses, *s, None),
                Node::Ex(Ex::Method { recv: Recv::Place(p), name, user: None, .. }) if d == 0 && p.path.is_empty() && !is_mutator(name) => {
                    recvs.insert(place_key(p));
                }
                Node::Place(p, _) => match p.root {
                    Root::Local(s) if d == 0 => mention(&mut uses, s, recvs.contains(&place_key(p)).then(|| place_key(p))),
                    Root::Up(k, s) if k == d => mention(&mut uses, s, None),
                    _ => {}
                },
                Node::St(St::For { .. } | St::While { .. }) if d == 0 => looped = true,
                Node::St(St::Break(_) | St::Continue(_)) if d == 0 => jumps = true,
                _ => {}
            }
        };
        for e in exs {
            walk_ex(e, 0, &mut f);
        }
        for p in places {
            walk_place(p, false, 0, &mut f);
        }
        // A `break` or `continue` inside leaves for the loop's sets.
        if jumps {
            if let Some((head, exit)) = self.loops.last() {
                out.union(head);
                out.union(exit);
            }
        }
        for (&s, &(count, key)) in &uses {
            if let (0, 1, false, Some(k)) = (self.dry, count, looped, key) {
                if !out.has(s) && (self.movable)(s) {
                    self.moves.insert(k);
                }
            }
        }
        for &s in uses.keys() {
            out.add(s);
        }
        out
    }
}
