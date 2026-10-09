//! The checker: walks the resolved IR of each function, infers the type of
//! every expression and reports type errors.
//!
//! Signatures are declared; everything inside a body is inferred.
//! Inference is bidirectional where an expected type is known (lambda
//! parameters, empty literals, `none`), with unification for the rest.
//! Operations on a value whose type isn't known yet (`stack[-1].push(x)`
//! before anything says what `stack` holds) are deferred until it is.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use lacon_interp::builtins::is_method;
use lacon_interp::ir::*;
use lacon_interp::value::{Func, Value};
use lacon_syntax::ast::{BinOp, CmpOp, Mode, UnOp};
use lacon_syntax::{Diag, Fix, Span};

use crate::methods::NoSig;
use crate::types::{Subst, T, INT};

#[derive(Clone)]
struct Frame {
    /// Each slot's type as bound or declared.
    decl: Vec<T>,
    /// The slot is an optional known not to be `none` here.
    narrowed: Vec<bool>,
    /// A `var` bound to an int without a declared type, which becomes a float
    /// if a float is assigned to it later.
    widenable: Vec<bool>,
    /// The slot was bound to a `T!` by this expression, where a `?` can go.
    from: Vec<Option<Span>>,
}

impl Frame {
    fn new(n: u32, s: &mut Subst) -> Frame {
        let decl = (0..n).map(|_| s.fresh()).collect();
        Frame { decl, narrowed: vec![false; n as usize], widenable: vec![false; n as usize], from: vec![None; n as usize] }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Fn,
    Main,
    Test,
    Const,
}

/// The named function (or test, or constant) being checked.
#[derive(Clone)]
pub(crate) struct FnCtx {
    name: String,
    sig: String,
    /// The span of `sig` in the source, which signature fixes replace.
    head: Span,
    ret: Option<T>,
    kind: Kind,
}

impl FnCtx {
    fn returns_result(&self, s: &Subst) -> bool {
        self.ret.as_ref().is_some_and(|r| matches!(s.resolve(r), T::Res(_)))
    }

    fn returns_optional(&self, s: &Subst) -> bool {
        self.ret.as_ref().is_some_and(|r| matches!(s.resolve(r), T::Opt(_)))
    }

    /// `x?` on an optional can leave this function.
    fn passes_none(&self, s: &Subst) -> bool {
        matches!(self.kind, Kind::Main | Kind::Test) || self.returns_result(s) || self.returns_optional(s)
    }

    /// `x?` on a result can leave this function (in a `T?` function an
    /// error becomes `none`).
    fn passes_err(&self, s: &Subst) -> bool {
        self.passes_none(s)
    }

    /// `fail` can leave this function.
    fn can_fail(&self, s: &Subst) -> bool {
        matches!(self.kind, Kind::Main | Kind::Test) || self.returns_result(s)
    }
}

/// An argument, either still an expression (checked against the parameter
/// type) or already inferred (deferred calls).
pub(crate) enum Arg<'a> {
    Ex(&'a Ex),
    Ty(T, Span, bool),
}

impl Arg<'_> {
    fn span(&self) -> Span {
        match self {
            Arg::Ex(e) => e.span(),
            Arg::Ty(_, s, _) => *s,
        }
    }

    fn is_fn(&self) -> bool {
        match self {
            Arg::Ex(e) => is_fn_ex(e),
            Arg::Ty(_, _, f) => *f,
        }
    }
}

fn is_fn_ex(e: &Ex) -> bool {
    matches!(e, Ex::Lambda(_) | Ex::Lit(Value::Func(_), _))
}

/// Where a value meets an expected type, for the wording of mismatches.
#[derive(Clone, Copy)]
pub(crate) enum Site<'a> {
    Arg { func: &'a str, param: &'a str },
    MethodArg { method: &'a str, n: usize },
    Field { ty: &'a str, field: &'a str },
    Variant { name: &'a str },
    Return,
    Bind,
    Assign { target: &'a str },
    Lambda { method: &'a str },
    Key,
    Plain,
}

enum DKind {
    Method { name: Rc<str>, user: Option<Rc<[FnId]>>, args: Vec<(T, Span, bool)> },
    Field { name: Rc<str>, user: Option<Rc<[FnId]>> },
    Index { idx: T, write: bool },
    Iter { keys_only: bool },
    Arith { op: BinOp, l: T, r: T, lspan: Span, rspan: Span },
    Try,
}

/// An operation waiting for the type of `recv` to be known.
struct Deferred {
    recv: T,
    kind: DKind,
    ret: T,
    span: Span,
    ctx: FnCtx,
}

type Key = (usize, u32);

/// A struct field reached from a slot through fields: `r.ms` or `r.a.ms`.
type FieldKey = (Key, Vec<Rc<str>>);

/// Something known not to be `none`: a slot, or a field reached from one.
#[derive(Clone, PartialEq)]
enum Fact {
    Slot(Key),
    Field(FieldKey),
}

/// What narrowing knows at a point: per frame, the slots known not to be
/// `none`, and the fields.
#[derive(Clone)]
struct Narrow {
    slots: Vec<Vec<bool>>,
    fields: Vec<FieldKey>,
}

pub struct Checker<'p> {
    pub prog: &'p Program,
    src: &'p str,
    pub s: Subst,
    pub diags: Vec<Diag>,
    frames: Vec<Frame>,
    ctx: FnCtx,
    deferred: Vec<Deferred>,
    consts: Vec<T>,
    /// The type a function without a declared return type ends with, for
    /// the hint when a caller uses its value.
    tails: Vec<Option<T>>,
    widen: HashSet<u32>,
    widen_hit: bool,
    /// Per enclosing loop: whether it has a `break`.
    loops: Vec<bool>,
    /// Optional fields known not to be `none` here, as `Frame::narrowed`
    /// knows slots: `if r.ms == none: continue` makes `r.ms` an int after.
    fields: Vec<FieldKey>,
    /// `s.parse()` calls with a declared type to parse to, for
    /// `Program::parse_to`.
    pub parse_to: Vec<(Span, ConvTo)>,
    /// `x.map(f)` calls on an optional, for `Program::opt_map`.
    pub opt_map: Vec<Span>,
    /// `x?` on a `T?!`, for `Program::res_try`.
    pub res_try: Vec<Span>,
    /// `r.name` nodes, by address, that read a struct's field although a
    /// function `name` exists too: they narrow as any field does.
    real_fields: HashSet<usize>,
    /// The type of every expression checked, by node address, in order;
    /// a later entry for the same node wins.
    pub ex_log: Vec<(usize, T)>,
    /// The slot types of every function, lambda and default body, by the
    /// address of the body.
    pub frame_log: Vec<(usize, Vec<T>)>,
    /// What `s.parse()` without a declared type gives: whichever number the
    /// text is, whatever type use gives it.
    loose_parses: Vec<T>,
    /// Reads of a local bound to a `T!`, by the read's span: the expression
    /// it was bound to.
    res_reads: HashMap<Span, Span>,
}

impl<'p> Checker<'p> {
    pub fn new(prog: &'p Program, src: &'p str) -> Checker<'p> {
        Checker {
            prog,
            src,
            s: Subst::default(),
            diags: Vec::new(),
            frames: Vec::new(),
            ctx: FnCtx { name: String::new(), sig: String::new(), head: Span::default(), ret: None, kind: Kind::Const },
            deferred: Vec::new(),
            consts: vec![T::Unknown; prog.consts.len()],
            tails: vec![None; prog.fns.len()],
            widen: HashSet::new(),
            widen_hit: false,
            loops: Vec::new(),
            fields: Vec::new(),
            parse_to: Vec::new(),
            opt_map: Vec::new(),
            res_try: Vec::new(),
            real_fields: HashSet::new(),
            ex_log: Vec::new(),
            frame_log: Vec::new(),
            loose_parses: Vec::new(),
            res_reads: HashMap::new(),
        }
    }

    // ----- entry points -----

    pub fn run(&mut self) {
        let prog = self.prog;
        for (i, c) in prog.consts.iter().enumerate() {
            self.ctx = FnCtx { name: c.name.clone(), sig: String::new(), head: Span::default(), ret: None, kind: Kind::Const };
            self.frames = vec![Frame::new(c.nslots, &mut self.s)];
            self.fields.clear();
            let t = self.expr(&c.value, None);
            self.solve(true);
            self.consts[i] = self.s.zonk(&t);
            self.log_frame(&c.value);
        }
        for sd in &prog.structs {
            let subst: Vec<(Rc<str>, T)> = sd.generics.iter().map(|g| (g.as_str().into(), T::Unknown)).collect();
            for f in &sd.fields {
                if let Some(d) = &f.default {
                    let want = self.s.from_ir(&f.ty, &subst);
                    self.thunk(d, &want, Site::Field { ty: &sd.name, field: &f.name });
                }
            }
        }
        for id in 0..prog.fns.len() {
            self.check_fn(id);
        }
        // `"2.5".parse() ?? 0` is typed int by its use but reads a float.
        for t in std::mem::take(&mut self.loose_parses) {
            let t = match self.s.resolve(&t) {
                T::Res(x) => self.s.resolve(&x),
                t => t,
            };
            if matches!(t, T::Int(_)) {
                self.s.leaks += 1;
            }
        }
        // Tests run only in the interpreter, so what they let through
        // unchecked can't reach compiled code.
        let leaks = self.s.leaks;
        for t in &prog.tests {
            self.ctx = FnCtx { name: t.name.clone(), sig: String::new(), head: Span::default(), ret: None, kind: Kind::Test };
            self.frames = vec![Frame::new(t.nslots, &mut self.s)];
            self.fields.clear();
            self.deferred.clear();
            self.expr(&t.body, None);
            self.solve(true);
            self.log_frame(&t.body);
        }
        self.s.leaks = leaks;
    }

    fn log_frame(&mut self, body: &Ex) {
        if let Some(f) = self.frames.last() {
            self.frame_log.push((body as *const Ex as usize, f.decl.clone()));
        }
    }

    /// Checks a default value, which sees no locals.
    fn thunk(&mut self, d: &LambdaDef, want: &T, site: Site) {
        let saved = std::mem::replace(&mut self.frames, vec![Frame::new(d.nslots, &mut self.s)]);
        let fields = std::mem::take(&mut self.fields);
        self.check(&d.body, want, site);
        self.log_frame(&d.body);
        self.frames = saved;
        self.fields = fields;
    }

    fn check_fn(&mut self, id: usize) {
        self.widen.clear();
        for _ in 0..4 {
            let snap = self.s.snapshot();
            let nd = self.diags.len();
            let (np, no, nr) = (self.parse_to.len(), self.opt_map.len(), self.res_try.len());
            let (ne, nf, nl) = (self.ex_log.len(), self.frame_log.len(), self.loose_parses.len());
            self.widen_hit = false;
            self.fn_once(id);
            if !self.widen_hit {
                return;
            }
            self.s = snap;
            self.diags.truncate(nd);
            self.parse_to.truncate(np);
            self.opt_map.truncate(no);
            self.res_try.truncate(nr);
            self.ex_log.truncate(ne);
            self.frame_log.truncate(nf);
            self.loose_parses.truncate(nl);
        }
    }

    fn fn_once(&mut self, id: usize) {
        let fd = &self.prog.fns[id];
        let ret = fd.ret.as_ref().map(|t| self.s.from_ir(t, &[]));
        let kind = if self.prog.main == Some(id as FnId) { Kind::Main } else { Kind::Fn };
        self.ctx = FnCtx { name: fd.name.clone(), sig: fd.sig.clone(), head: fd.head, ret: ret.clone(), kind };
        let mut frame = Frame::new(fd.nslots, &mut self.s);
        let mut params = Vec::new();
        for (i, p) in fd.params.iter().enumerate() {
            let t = self.s.from_ir(&p.ty, &[]);
            frame.decl[i] = t.clone();
            params.push((t, p));
        }
        for (t, p) in &params {
            if let Some(d) = &p.default {
                self.thunk(d, t, Site::Arg { func: &fd.name, param: &p.name });
            }
        }
        self.frames = vec![frame];
        self.fields.clear();
        self.deferred.clear();
        self.loops.clear();
        let body = &fd.body;
        let tail_span = body.span();
        match ret.as_ref().map(|r| self.s.resolve(r)) {
            None | Some(T::Unit) => {
                let t = self.expr(body, None);
                match self.s.resolve(&t) {
                    T::Res(_) if kind != Kind::Main => {
                        let snip = self.snippet(tail_span).to_string();
                        self.err_res_ignored(tail_span, &snip);
                    }
                    T::Unit | T::Never | T::Unknown | T::Var(_) => {}
                    t => self.tails[id] = Some(t),
                }
            }
            Some(T::Res(inner)) if matches!(self.s.resolve(&inner), T::Unit) => {
                self.expr(body, None);
            }
            Some(r) => {
                let t = self.expr(body, Some(&r));
                match self.s.resolve(&t) {
                    T::Unit => {
                        let what = self.show(&r);
                        self.err(
                            "E0304",
                            fd.span,
                            format!("`{}` must return {what}, but its body can end without a value; end it with an expression or `return`", fd.name),
                        );
                    }
                    _ => {
                        if !self.s.coerce(&t, &r) {
                            let span = last_value_span(body);
                            self.mismatch(Site::Return, &t, &r, span, Some(body));
                        }
                    }
                }
            }
        }
        self.solve(true);
        self.log_frame(body);
    }

    // ----- diagnostics -----

    fn err(&mut self, code: &'static str, span: Span, msg: impl Into<String>) {
        self.diags.push(Diag::new(code, span, msg));
    }

    /// An error whose fix, if any, replaces `span`.
    fn err_fix(&mut self, code: &'static str, span: Span, msg: impl Into<String>, fix: Option<String>) {
        self.diags.push(Diag::new(code, span, msg).with_fix(fix.map(|f| Fix::at(span, f))));
    }

    /// An error whose fix rewrites the signature at `head`.
    fn err_sig_fix(&mut self, code: &'static str, span: Span, msg: impl Into<String>, head: Span, sig: String) {
        self.diags.push(Diag::new(code, span, msg).with_fix(Some(Fix::at(head, sig))));
    }

    pub fn snippet(&self, span: Span) -> &str {
        self.src.get(span.start as usize..span.end as usize).unwrap_or("")
    }

    pub fn show(&self, t: &T) -> String {
        self.s.show(t, self.prog)
    }

    fn zero_of(&self, t: &T) -> Option<&'static str> {
        Some(match self.s.resolve(t) {
            T::Int(_) => "0",
            T::Float => "0.0",
            T::Str => "\"\"",
            T::Bool => "false",
            T::List(_) => "[]",
            T::Map(..) => "{}",
            T::Set(_) => "set()",
            _ => return None,
        })
    }

    fn wrap(snip: &str, paren: bool) -> String {
        if paren {
            format!("({snip})")
        } else {
            snip.to_string()
        }
    }

    /// `x` may be `none` where a value is needed.
    fn err_opt(&mut self, span: Span, inner: &T, paren: bool) {
        let snip = self.snippet(span).to_string();
        let fix = match self.zero_of(inner) {
            Some(z) => Some(Self::wrap(&format!("{snip} ?? {z}"), paren)),
            None if self.ctx.passes_none(&self.s) => Some(format!("{snip}?")),
            None => None,
        };
        self.err_fix("E0407", span, format!("`{snip}` may be none; give a default with `?? value`, check `!= none` first, or pass it up with `?`"), fix);
    }

    /// `x` may be an error where a value is needed.
    fn err_res(&mut self, span: Span, inner: &T, paren: bool) {
        if let Some(&at) = self.res_reads.get(&span) {
            return self.err_res_bound(span, at, inner);
        }
        let snip = self.snippet(span).to_string();
        let fix = if self.ctx.passes_err(&self.s) {
            Some(format!("{snip}?"))
        } else {
            self.zero_of(inner).map(|z| Self::wrap(&format!("{snip} ?? {z}"), paren))
        };
        let how = if self.ctx.passes_err(&self.s) { "add `?` to pass the error up" } else { "handle it with `?? default` or `match`" };
        self.err_fix("E0406", span, format!("`{snip}` may be an error; {how}"), fix);
    }

    /// A local bound to a `T!` is used as a `T`: one error, where it's bound.
    fn err_res_bound(&mut self, read: Span, at: Span, inner: &T) {
        if self.diags.iter().any(|d| d.code == "E0406" && d.span == at) {
            return;
        }
        let (name, snip) = (self.snippet(read).to_string(), self.snippet(at).to_string());
        let (fix, how) = if self.ctx.passes_err(&self.s) {
            (Some(format!("{snip}?")), "add `?` where it's bound to pass the error up")
        } else {
            (self.zero_of(inner).map(|z| format!("{snip} ?? {z}")), "handle it where it's bound with `?? default`, or `match` it")
        };
        self.err_fix("E0406", at, format!("`{name}` may be an error; {how}"), fix);
    }

    fn err_res_ignored(&mut self, span: Span, snip: &str) {
        let fix = if self.ctx.passes_err(&self.s) { Some(format!("{snip}?")) } else { None };
        let how = if self.ctx.passes_err(&self.s) {
            "add `?` to pass it up, or handle it with `??` or `match`".to_string()
        } else {
            format!("handle it with `??` or `match`, or declare `{}` with `!` and add `?`", self.ctx.name)
        };
        self.err_fix("E0406", span, format!("error ignored: `{snip}` may fail; {how}"), fix);
    }

    fn err_mixed(&mut self, span: Span, t: &T) {
        let snip = self.snippet(span).to_string();
        let parts = match self.s.resolve(t) {
            T::Mixed(ts) => ts.iter().map(|t| self.show(t)).collect::<Vec<_>>().join(" and "),
            t => self.show(&t),
        };
        self.err("E0303", span, format!("`{snip}` mixes {parts}, so it can only be printed or interpolated; make the branches one type, e.g. with `str(...)`"));
    }

    /// The user function an expression calls when it has no declared
    /// return type.
    fn unit_fn_of(&self, e: &Ex) -> Option<FnId> {
        let ids: &[FnId] = match e {
            Ex::CallFn { fns, .. } => fns,
            Ex::Method { user: Some(ids), name, .. } | Ex::Field { user: Some(ids), name, .. } if !is_method(name) => ids,
            _ => return None,
        };
        if ids.iter().all(|&id| self.prog.fns[id as usize].ret.is_none()) {
            ids.first().copied()
        } else {
            None
        }
    }

    fn err_unit(&mut self, fid: FnId, span: Span) {
        let fd = &self.prog.fns[fid as usize];
        let snip = self.snippet(span).to_string();
        let fix = self.tails[fid as usize].clone().map(|t| Fix::at(fd.head, format!("{} {} =", fd.sig, self.show(&t))));
        let msg = format!("`{}` declares no return type, so `{snip}` has no value; declare one: `{} T =`", fd.name, fd.sig);
        self.diags.push(Diag::new("E0305", span, msg).with_fix(fix));
    }

    /// Reports that `got` does not fit `want`.
    pub(crate) fn mismatch(&mut self, site: Site, got: &T, want: &T, span: Span, e: Option<&Ex>) {
        let (g, w) = (self.s.resolve(got), self.s.resolve(want));
        // `(name, int(s), n)` where `(str, int, int)` is wanted: report the
        // unhandled element, with its fix, rather than the whole tuple.
        if let (T::Tuple(gs), Some(Ex::Tuple(xs, _))) = (&g, e.map(last_value)) {
            let inner = match &w {
                T::Opt(x) | T::Res(x) => self.s.resolve(x),
                w => w.clone(),
            };
            if let T::Tuple(ws) = inner {
                if ws.len() == gs.len() && xs.len() == gs.len() {
                    let mut reported = false;
                    for ((x, gt), wt) in xs.iter().zip(gs).zip(&ws) {
                        let gt = self.s.resolve(gt);
                        if matches!(gt, T::Res(_) | T::Opt(_)) && !matches!(self.s.resolve(wt), T::Res(_) | T::Opt(_)) {
                            self.mismatch(Site::Plain, &gt, wt, x.span(), Some(x));
                            reported = true;
                        }
                    }
                    if reported {
                        return;
                    }
                }
            }
        }
        match (&g, &w) {
            (T::Res(inner), w) if !matches!(w, T::Res(_)) => return self.err_res(span, inner, false),
            (T::Opt(inner), w) if !matches!(w, T::Opt(_) | T::Res(_)) => return self.err_opt(span, inner, false),
            (T::Mixed(_), _) => return self.err_mixed(span, &g),
            (T::Unit, _) => {
                if let Some(fid) = e.and_then(|e| self.unit_fn_of(e)) {
                    return self.err_unit(fid, span);
                }
            }
            _ => {}
        }
        let (gs, ws) = (self.show(&g), self.show(&w));
        let snip = self.snippet(span).to_string();
        let literal = snip.starts_with(['"', '\'']) || snip.parse::<f64>().is_ok();
        let fix = match (&g, &w) {
            _ if literal => None,
            (T::Float, T::Int(n)) => Some(format!("{n}({snip})")),
            (T::Int(_) | T::Float | T::Bool, T::Str) => Some(format!("str({snip})")),
            (T::Str, T::Int(n)) if self.ctx.passes_err(&self.s) => Some(format!("{n}({snip})?")),
            (T::Str, T::Float) if self.ctx.passes_err(&self.s) => Some(format!("f64({snip})?")),
            _ => None,
        };
        let msg = match site {
            Site::Arg { func, param } => format!("argument `{param}` of `{func}`: want {ws} got {gs}"),
            Site::MethodArg { method, n } => format!("argument {} of `{method}`: want {ws} got {gs}", n + 1),
            Site::Field { ty, field } => format!("field `{field}` of {ty}: want {ws} got {gs}"),
            Site::Variant { name } => format!("`{name}` field: want {ws} got {gs}"),
            Site::Return => format!("`{}` returns {ws}, got {gs}", self.ctx.name),
            Site::Bind | Site::Plain => format!("want {ws} got {gs}"),
            Site::Assign { target } => format!("`{target}` holds {ws}, got {gs}"),
            Site::Lambda { method } => format!("the function given to `{method}` must return {ws}, got {gs}"),
            Site::Key => format!("map key: want {ws} got {gs}"),
        };
        self.err_fix("E0301", span, msg, fix);
    }

    // ----- frames and narrowing -----

    fn frame_index(&self, depth: u32) -> usize {
        self.frames.len() - 1 - depth as usize
    }

    fn slot_type(&self, depth: u32, slot: u32) -> T {
        let f = &self.frames[self.frame_index(depth)];
        let t = f.decl[slot as usize].clone();
        if f.narrowed[slot as usize] {
            if let T::Opt(inner) = self.s.resolve(&t) {
                return *inner;
            }
        }
        t
    }

    fn decl_type(&self, depth: u32, slot: u32) -> T {
        self.frames[self.frame_index(depth)].decl[slot as usize].clone()
    }

    fn bind_slot(&mut self, slot: u32, t: T) {
        let f = self.frames.last_mut().unwrap();
        f.decl[slot as usize] = t;
        f.narrowed[slot as usize] = false;
        f.from[slot as usize] = None;
        let key = (self.frames.len() - 1, slot);
        self.forget_fields(key, &[]);
    }

    fn key_of(&self, e: &Ex) -> Option<Key> {
        match e {
            Ex::Local(s, _) => Some((self.frames.len() - 1, *s)),
            Ex::Up(d, s, _) => Some((self.frame_index(*d), *s)),
            _ => None,
        }
    }

    fn root_key(&self, r: &Root) -> Key {
        match *r {
            Root::Local(s) => (self.frames.len() - 1, s),
            Root::Up(d, s) => (self.frame_index(d), s),
        }
    }

    /// `e` as a slot, or a field reached from one through fields.
    fn fact_of(&self, e: &Ex) -> Option<Fact> {
        match e {
            Ex::Field { .. } => self.field_key(e).map(Fact::Field),
            _ => self.key_of(e).map(Fact::Slot),
        }
    }

    /// `r.a.ms` as the slot `r` and the fields `a`, `ms`.
    fn field_key(&self, e: &Ex) -> Option<FieldKey> {
        match e {
            Ex::Field { obj, name, user, .. } if user.is_none() || self.real_fields.contains(&(e as *const Ex as usize)) => {
                let (key, mut path) = match &**obj {
                    Ex::Field { .. } => self.field_key(obj)?,
                    o => (self.key_of(o)?, Vec::new()),
                };
                path.push(name.clone());
                Some((key, path))
            }
            _ => None,
        }
    }

    /// A place whose steps are all fields, as a `FieldKey`.
    fn place_field_key(&self, p: &Place) -> Option<FieldKey> {
        let mut path = Vec::new();
        for seg in &p.path {
            match seg {
                Seg::Field(name, _) => path.push(name.clone()),
                Seg::Index(..) => return None,
            }
        }
        (!path.is_empty()).then(|| (self.root_key(&p.root), path))
    }

    /// The fields of a place before its first index: what a store to it,
    /// or a call that changes it, may change under.
    fn place_prefix(p: &Place) -> Vec<Rc<str>> {
        p.path
            .iter()
            .map_while(|seg| match seg {
                Seg::Field(name, _) => Some(name.clone()),
                Seg::Index(..) => None,
            })
            .collect()
    }

    /// Forgets what is known about the fields of slot `key` under the
    /// fields `prefix`, or above them.
    fn forget_fields(&mut self, key: Key, prefix: &[Rc<str>]) {
        self.fields.retain(|(k, path)| *k != key || !(path.starts_with(prefix) || prefix.starts_with(path)));
    }

    /// Forgets what is known about place `p` and what's inside it, after
    /// a store to it or a call that changes it.
    fn forget_place(&mut self, p: &Place) {
        let key = self.root_key(&p.root);
        self.forget_fields(key, &Self::place_prefix(p));
    }

    /// A field's type `t`, as narrowing knows it: an optional known not to
    /// be `none` is its inner type.
    fn narrowed_field(&self, key: Option<FieldKey>, t: T) -> T {
        if let Some(k) = key {
            if self.fields.contains(&k) {
                if let T::Opt(inner) = self.s.resolve(&t) {
                    return *inner;
                }
            }
        }
        t
    }

    /// Slots and fields known not to be `none` when `e` is true, and when
    /// it is false.
    fn facts(&self, e: &Ex) -> (Vec<Fact>, Vec<Fact>) {
        let is_none = |x: &Ex| matches!(x, Ex::Lit(Value::None, _));
        match e {
            Ex::Compare { first, rest, .. } if rest.len() == 1 && matches!(rest[0].0, CmpOp::Eq | CmpOp::Ne) => {
                let (a, b) = (&**first, &rest[0].1);
                let key = if is_none(b) { self.fact_of(a) } else if is_none(a) { self.fact_of(b) } else { None };
                match (key, rest[0].0) {
                    (Some(k), CmpOp::Ne) => (vec![k], vec![]),
                    (Some(k), _) => (vec![], vec![k]),
                    (None, _) => (vec![], vec![]),
                }
            }
            Ex::Unary { op: UnOp::Not | UnOp::Bang, e, .. } => {
                let (t, f) = self.facts(e);
                (f, t)
            }
            Ex::And(a, b, _) => {
                let (at, af) = self.facts(a);
                let (bt, bf) = self.facts(b);
                (at.into_iter().chain(bt).collect(), af.into_iter().filter(|k| bf.contains(k)).collect())
            }
            Ex::Or(a, b, _) => {
                let (at, af) = self.facts(a);
                let (bt, bf) = self.facts(b);
                (at.into_iter().filter(|k| bt.contains(k)).collect(), af.into_iter().chain(bf).collect())
            }
            Ex::Method { recv: Recv::Place(p), name, args, .. } if args.is_empty() => {
                let key = match self.place_field_key(p) {
                    Some(k) => Fact::Field(k),
                    None if p.path.is_empty() => Fact::Slot(self.root_key(&p.root)),
                    None => return (vec![], vec![]),
                };
                match &**name {
                    "is_some" => (vec![key], vec![]),
                    "is_none" => (vec![], vec![key]),
                    _ => (vec![], vec![]),
                }
            }
            _ => (vec![], vec![]),
        }
    }

    fn apply(&mut self, facts: &[Fact]) {
        for fact in facts {
            match fact {
                Fact::Slot((f, s)) => {
                    if let Some(fr) = self.frames.get_mut(*f) {
                        fr.narrowed[*s as usize] = true;
                    }
                }
                Fact::Field(k) => {
                    if !self.fields.contains(k) {
                        self.fields.push(k.clone());
                    }
                }
            }
        }
    }

    fn narrow_state(&self) -> Narrow {
        Narrow { slots: self.frames.iter().map(|f| f.narrowed.clone()).collect(), fields: self.fields.clone() }
    }

    fn set_narrow_state(&mut self, st: &Narrow) {
        for (f, n) in self.frames.iter_mut().zip(&st.slots) {
            f.narrowed.clone_from(n);
        }
        self.fields.clone_from(&st.fields);
    }

    fn merge_states(states: &[Narrow]) -> Option<Narrow> {
        let mut it = states.iter();
        let mut acc = it.next()?.clone();
        for st in it {
            for (a, b) in acc.slots.iter_mut().zip(&st.slots) {
                for (x, y) in a.iter_mut().zip(b) {
                    *x = *x && *y;
                }
            }
            acc.fields.retain(|k| st.fields.contains(k));
        }
        Some(acc)
    }

    /// Forgets what is known about slots a loop body assigns, and their
    /// fields, since the body may run again after the assignment.
    fn forget_assigned(&mut self, body: &[St]) {
        let mut out = HashSet::new();
        assigned_stmts(body, &mut out);
        let fi = self.frames.len() - 1;
        let f = self.frames.last_mut().unwrap();
        for &s in &out {
            if let Some(n) = f.narrowed.get_mut(s as usize) {
                *n = false;
            }
        }
        self.fields.retain(|((f, s), _)| *f != fi || !out.contains(s));
    }

    // ----- deferred operations -----

    fn defer(&mut self, recv: T, kind: DKind, span: Span) -> T {
        let ret = self.s.fresh();
        self.deferred.push(Deferred { recv, kind, ret: ret.clone(), span, ctx: self.ctx.clone() });
        ret
    }

    fn ready(&self, d: &Deferred) -> bool {
        match &d.kind {
            DKind::Arith { l, r, .. } => !self.s.is_var(l) && !self.s.is_var(r),
            _ => !self.s.is_var(&d.recv),
        }
    }

    fn run_deferred(&mut self, d: Deferred) {
        let saved = std::mem::replace(&mut self.ctx, d.ctx.clone());
        let t = match d.kind {
            DKind::Method { name, user, args } => {
                let args: Vec<Arg> = args.into_iter().map(|(t, s, f)| Arg::Ty(t, s, f)).collect();
                self.method_on(d.recv, d.span, &name, user.as_ref(), &args, d.span)
            }
            DKind::Field { name, user } => self.field_type(&d.recv, &name, user.as_ref(), d.span, d.span),
            DKind::Index { idx, write } => self.index_type(&d.recv, &idx, None, d.span, d.span, write),
            DKind::Iter { keys_only } => self.for_elem(&d.recv, keys_only, d.span),
            DKind::Arith { op, l, r, lspan, rspan } => self.binop_types(op, &l, &r, lspan, rspan, d.span),
            DKind::Try => self.try_type(&d.recv, d.span),
        };
        self.s.unify(&d.ret, &t);
        self.ctx = saved;
    }

    /// Runs deferred operations whose receivers are now known. At the end of
    /// a function, also guesses a receiver from the method called on it
    /// (`m[k].push(x)` makes the map's values lists).
    fn solve(&mut self, last: bool) {
        loop {
            let mut progress = false;
            let pending = std::mem::take(&mut self.deferred);
            for d in pending {
                if self.ready(&d) {
                    self.run_deferred(d);
                    progress = true;
                } else {
                    self.deferred.push(d);
                }
            }
            if progress {
                continue;
            }
            if !last || !self.default_one() {
                break;
            }
        }
        if last {
            // Operations that never learned their receiver's type went
            // unchecked, and their results may have been typed by use.
            if !self.deferred.is_empty() {
                self.s.leaks += 1;
            }
            self.deferred.clear();
        }
    }

    fn default_one(&mut self) -> bool {
        for i in 0..self.deferred.len() {
            let d = &self.deferred[i];
            let guess = match &d.kind {
                DKind::Method { name, .. } => match &**name {
                    "push" | "extend" | "insert" | "truncate" | "swap" | "retain" => Some(T::list(self.s.fresh())),
                    "add" => Some(T::set(self.s.fresh())),
                    _ => None,
                },
                DKind::Index { idx, write: true } => Some(T::map(idx.clone(), self.s.fresh())),
                DKind::Arith { l, r, .. } => {
                    // A guess, not a proof: `l` is whatever `r` is.
                    let (l, r) = (l.clone(), r.clone());
                    if self.s.is_var(&l) && !self.s.is_var(&r) {
                        self.s.unify(&l, &r);
                        self.s.leaks += 1;
                        return true;
                    }
                    if self.s.is_var(&r) && !self.s.is_var(&l) {
                        self.s.unify(&r, &l);
                        self.s.leaks += 1;
                        return true;
                    }
                    None
                }
                _ => None,
            };
            if let Some(g) = guess {
                let recv = self.deferred[i].recv.clone();
                self.s.unify(&recv, &g);
                return true;
            }
        }
        false
    }

    // ----- expressions -----

    pub(crate) fn infer(&mut self, e: &Ex) -> T {
        self.expr(e, None)
    }

    /// Checks `e` against an expected type, reporting a mismatch.
    pub(crate) fn check(&mut self, e: &Ex, want: &T, site: Site) -> T {
        let t = self.expr(e, Some(want));
        if !self.s.coerce(&t, want) {
            self.mismatch(site, &t, want, e.span(), Some(e));
        }
        t
    }

    pub(crate) fn check_arg(&mut self, a: &Arg, want: &T, site: Site) -> T {
        match a {
            Arg::Ex(e) => self.check(e, want, site),
            Arg::Ty(t, span, _) => {
                if !self.s.coerce(t, want) {
                    self.mismatch(site, t, want, *span, None);
                }
                t.clone()
            }
        }
    }

    fn arg_type(&mut self, a: &Arg) -> T {
        match a {
            Arg::Ex(e) => self.infer(e),
            Arg::Ty(t, _, _) => t.clone(),
        }
    }

    /// A condition: must be `bool`.
    fn cond(&mut self, e: &Ex) {
        let t = self.infer(e);
        let r = self.s.resolve(&t);
        if !self.s.coerce(&r, &T::Bool) {
            let hint = match &r {
                T::List(_) | T::Map(..) | T::Set(_) | T::Heap(_) | T::Str => "; test emptiness with `x.is_empty()` or `x.len > 0`",
                T::Int(_) | T::Float => "; compare explicitly, e.g. `n != 0`",
                T::Opt(_) => "; compare explicitly, e.g. `x != none`",
                _ => "",
            };
            if let T::Res(inner) = &r {
                return self.err_res(e.span(), inner, false);
            }
            let got = self.show(&r);
            self.err("E0301", e.span(), format!("condition must be bool, got {got}{hint}"));
        }
    }

    /// A value about to be printed or interpolated.
    fn display(&mut self, t: &T, e: &Ex) {
        match self.s.resolve(t) {
            T::Res(inner) => self.err_res(e.span(), &inner, false),
            T::Unit => {
                if let Some(fid) = self.unit_fn_of(e) {
                    self.err_unit(fid, e.span());
                }
            }
            _ => {}
        }
    }

    /// Infers the type of `e`, guided by the type expected of it, and
    /// records it for the backend.
    pub(crate) fn expr(&mut self, e: &Ex, exp: Option<&T>) -> T {
        let t = self.expr_inner(e, exp);
        self.ex_log.push((e as *const Ex as usize, t.clone()));
        t
    }

    fn expr_inner(&mut self, e: &Ex, exp: Option<&T>) -> T {
        match e {
            Ex::Lit(v, span) => self.lit(v, exp, *span),
            Ex::Str(pieces, _) => {
                for p in pieces {
                    if let StrPiece::Expr(x, _, args) = p {
                        let t = self.infer(x);
                        self.display(&t, x);
                        for a in args {
                            self.check(a, &INT, Site::Plain);
                        }
                    }
                }
                T::Str
            }
            Ex::Local(slot, span) => {
                if let Some(at) = self.frames.last().and_then(|f| f.from[*slot as usize]) {
                    self.res_reads.insert(*span, at);
                }
                self.slot_type(0, *slot)
            }
            Ex::Up(d, slot, _) => self.slot_type(*d, *slot),
            Ex::Global(g, _) => self.consts.get(*g as usize).cloned().unwrap_or(T::Unknown),
            Ex::List(xs, _) => {
                let want = match exp.map(|t| self.s.resolve(t)) {
                    Some(T::List(e)) => Some(*e),
                    _ => None,
                };
                let elem = self.elems(xs, want.as_ref());
                T::list(elem)
            }
            Ex::Set(xs, _) => {
                let want = match exp.map(|t| self.s.resolve(t)) {
                    Some(T::Set(e)) => Some(*e),
                    _ => None,
                };
                let elem = self.elems(xs, want.as_ref());
                T::set(elem)
            }
            Ex::Map(ps, _) => {
                let (wk, wv) = match exp.map(|t| self.s.resolve(t)) {
                    Some(T::Map(k, v)) => (Some(*k), Some(*v)),
                    _ => (None, None),
                };
                let ks: Vec<&Ex> = ps.iter().map(|(k, _)| k).collect();
                let vs: Vec<&Ex> = ps.iter().map(|(_, v)| v).collect();
                let k = self.elem_refs(&ks, wk.as_ref());
                let v = self.elem_refs(&vs, wv.as_ref());
                T::map(k, v)
            }
            Ex::Tuple(xs, _) => {
                let want: Vec<Option<T>> = match exp.map(|t| self.s.resolve(t)) {
                    Some(T::Tuple(ts)) if ts.len() == xs.len() => ts.into_iter().map(Some).collect(),
                    _ => vec![None; xs.len()],
                };
                T::Tuple(xs.iter().zip(want).map(|(x, w)| self.expr(x, w.as_ref())).collect())
            }
            Ex::Struct { id, fields, span } => self.struct_lit(*id, fields, *span),
            Ex::Variant { id, tag, args, span } => self.variant(*id, *tag, args, *span, exp),
            Ex::Field { obj, name, user, span } => {
                let t = self.infer(obj);
                if user.is_some() {
                    if let T::Struct(id, _) = self.s.resolve(&t) {
                        if self.prog.structs[id as usize].fields.iter().any(|f| f.name == **name) {
                            self.real_fields.insert(e as *const Ex as usize);
                        }
                    }
                }
                let ft = self.field_type(&t, name, user.as_ref(), obj.span(), *span);
                self.narrowed_field(self.field_key(e), ft)
            }
            Ex::Index { obj, index, span } => {
                let t = self.infer(obj);
                let it = self.infer(index);
                self.index_type(&t, &it, Some(index), obj.span(), *span, false)
            }
            Ex::Slice { obj, start, end, span, .. } => {
                let t = self.infer(obj);
                for b in [start, end].into_iter().flatten() {
                    self.check(b, &INT, Site::Plain);
                }
                self.slice_type(&t, obj.span(), *span)
            }
            Ex::CallFn { fns, args, places, span } => {
                let t = self.call_fn(fns, args, *span);
                // What a `mut` argument's place was known to hold, it may not now.
                for (i, p) in places.iter().enumerate() {
                    if let Some(p) = p {
                        if fns.iter().any(|&id| self.prog.fns[id as usize].params.get(i).is_some_and(|p| p.mode == Mode::Mut)) {
                            self.forget_place(p);
                        }
                    }
                }
                t
            }
            Ex::CallValue { f, args, span } => self.call_value(f, args, *span),
            Ex::CallBuiltin { f, args, span } => self.builtin(*f, args, *span, exp),
            Ex::Method { recv, name, user, args, span } => {
                let (t, rspan) = match recv {
                    Recv::Place(p) => (self.place_type(p, false), p.span),
                    Recv::Value(e) => (self.infer(e), e.span()),
                };
                let mut loose = false;
                if &**name == "parse" && args.is_empty() {
                    match exp.and_then(|t| self.parse_target(t)) {
                        Some(to) => self.parse_to.push((*span, to)),
                        None => loose = true,
                    }
                }
                let n = args.len();
                let args: Vec<Arg> = args.iter().map(Arg::Ex).collect();
                let r = self.method_on(t.clone(), rspan, name, user.as_ref(), &args, *span);
                if loose && n == 0 && matches!(self.s.resolve(&t), T::Str | T::Var(_)) {
                    self.loose_parses.push(r.clone());
                }
                if let (Recv::Place(p), Some(ids)) = (recv, user) {
                    if ids.iter().any(|&id| self.prog.fns[id as usize].params.first().is_some_and(|p| p.mode == Mode::Mut)) {
                        self.forget_place(p);
                    }
                }
                r
            }
            Ex::Unary { op, e: x, span } => {
                let t = self.infer(x);
                self.unary(*op, &t, x.span(), *span)
            }
            Ex::Binary { op, l, r, span } => {
                let lt = self.infer(l);
                let rt = self.infer(r);
                for (t, x) in [(&lt, l), (&rt, r)] {
                    if matches!(self.s.resolve(t), T::Unit) {
                        if let Some(fid) = self.unit_fn_of(x) {
                            self.err_unit(fid, x.span());
                            return T::Unknown;
                        }
                    }
                }
                let t = self.binop_types(*op, &lt, &rt, l.span(), r.span(), *span);
                if *op == BinOp::Pow {
                    self.int_pow(&t, Some(r));
                }
                t
            }
            Ex::And(a, b, _) | Ex::Or(a, b, _) => {
                self.cond(a);
                let pre = self.narrow_state();
                let (at, af) = self.facts(a);
                self.apply(if matches!(e, Ex::And(..)) { &at } else { &af });
                self.cond(b);
                self.set_narrow_state(&pre);
                T::Bool
            }
            Ex::Coalesce(a, b, _) => {
                let want = exp.map(|t| T::opt(t.clone()));
                let at = self.expr(a, want.as_ref());
                let inner = match self.s.resolve(&at) {
                    T::Opt(x) | T::Res(x) => *x,
                    t => t,
                };
                let bt = self.expr(b, Some(&inner));
                self.s.join(&inner, &bt)
            }
            Ex::Compare { first, rest, .. } => {
                let mut prev = self.infer(first);
                let mut prev_span = first.span();
                for (op, x, sp) in rest {
                    let t = self.infer(x);
                    match op {
                        CmpOp::Eq | CmpOp::Ne => self.eq_check(&prev, &t, prev_span, x.span(), *sp),
                        CmpOp::In | CmpOp::NotIn => self.in_check(&prev, &t, prev_span, x.span(), *sp),
                        _ => self.order_check(&prev, &t, prev_span, x.span(), *sp),
                    }
                    prev = t;
                    prev_span = x.span();
                }
                T::Bool
            }
            Ex::Range { start, end, .. } => {
                for b in [start, end].into_iter().flatten() {
                    self.check(b, &INT, Site::Plain);
                }
                T::Range
            }
            Ex::Try { e: x, span, .. } => {
                let want = exp.map(|t| T::res(t.clone()));
                let t = self.expr(x, want.as_ref());
                self.try_type(&t, *span)
            }
            Ex::Lambda(def) => self.lambda(def, exp, None),
            Ex::If { cond, then, els, .. } => self.if_expr(cond, then, els.as_deref(), exp),
            Ex::Match { scrut, arms, span } => self.match_expr(scrut, arms, *span, exp),
            Ex::Block(stmts, tail) => {
                let mut diverges = false;
                for s in stmts {
                    diverges |= self.stmt(s);
                    self.solve(false);
                }
                let t = match tail {
                    Some(t) => self.expr(t, exp),
                    None => T::Unit,
                };
                if diverges {
                    T::Never
                } else {
                    t
                }
            }
            Ex::Cast { e: x, to, span } => {
                let t = self.infer(x);
                self.conv(*to, &t, Some(x), *span)
            }
            Ex::AssertEq { l, r, msg, span } => {
                let lt = self.infer(l);
                let rt = self.infer(r);
                self.eq_check(&lt, &rt, l.span(), r.span(), *span);
                if let Some(m) = msg {
                    let t = self.infer(m);
                    self.display(&t, m);
                }
                T::Bool
            }
            Ex::Poison(_) => T::Unknown,
        }
    }

    /// An int raised to a power is typed int, but a negative exponent
    /// gives a float at run time, so unless the exponent is a literal the
    /// type is a guess.
    fn int_pow(&mut self, t: &T, exp: Option<&Ex>) {
        let lit = matches!(exp, Some(Ex::Lit(Value::Int(n), _)) if *n >= 0);
        if matches!(self.s.resolve(t), T::Int(_)) && !lit {
            self.s.leaks += 1;
        }
    }

    fn elems(&mut self, xs: &[Ex], want: Option<&T>) -> T {
        let refs: Vec<&Ex> = xs.iter().collect();
        self.elem_refs(&refs, want)
    }

    /// The common type of list, set or map literal items.
    fn elem_refs(&mut self, xs: &[&Ex], want: Option<&T>) -> T {
        // An expected item type only guides inference (empty literals,
        // lambdas, `none`); a mismatch is reported for the whole literal.
        let mut acc = T::Never;
        for x in xs {
            let t = self.expr(x, want);
            acc = self.s.join(&acc, &t);
        }
        match (self.s.resolve(&acc), want) {
            (T::Never, Some(w)) => w.clone(),
            (T::Never, None) => self.s.fresh(),
            _ => {
                if let Some(w) = want {
                    if self.s.is_var(w) {
                        self.s.unify(w, &acc);
                    } else if self.s.could_coerce(&acc, w) {
                        self.s.coerce(&acc, w);
                        return w.clone();
                    }
                }
                acc
            }
        }
    }

    /// The type `s.parse()` reads when `exp` is expected of it: a declared
    /// int, float or bool, under at most one `?` or `T!`.
    fn parse_target(&self, exp: &T) -> Option<ConvTo> {
        match self.s.resolve(&strip(&self.s.resolve(exp))) {
            T::Int(name) => lacon_interp::resolve::conv_for(name),
            T::Float => Some(ConvTo::Float),
            T::Bool => Some(ConvTo::Bool),
            _ => None,
        }
    }

    fn lit(&mut self, v: &Value, exp: Option<&T>, span: Span) -> T {
        match v {
            Value::Int(_) => INT,
            Value::Float(_) => T::Float,
            Value::Bool(_) => T::Bool,
            Value::Str(_) => T::Str,
            Value::Unit => T::Unit,
            Value::None => match exp.map(|t| self.s.resolve(t)) {
                Some(T::Opt(x)) => T::Opt(x),
                _ => T::opt(self.s.fresh()),
            },
            Value::Func(f) => match &**f {
                Func::User(ids) => {
                    if ids.len() == 1 {
                        return self.fn_value(ids[0]);
                    }
                    if let Some(T::Fn(ps, _)) = exp.map(|t| self.s.resolve(t)) {
                        if let Some(p0) = ps.first() {
                            for &id in ids.iter() {
                                let fd = &self.prog.fns[id as usize];
                                if fd.params.len() == ps.len() {
                                    let want = self.s.from_ir(&fd.params[0].ty, &[]);
                                    if self.s.could_coerce(p0, &want) {
                                        return self.fn_value(id);
                                    }
                                }
                            }
                        }
                    }
                    T::Unknown
                }
                Func::Ctor(eid, tag) => {
                    let ed = &self.prog.enums[*eid as usize];
                    let subst: Vec<(Rc<str>, T)> = ed.generics.iter().map(|g| (g.as_str().into(), self.s.fresh())).collect();
                    let fields = ed.variants[*tag as usize].fields.iter().map(|t| self.s.from_ir(t, &subst)).collect();
                    T::func(fields, T::Enum(*eid, subst.into_iter().map(|(_, t)| t).collect()))
                }
                Func::Method(name) => match exp.map(|t| self.s.resolve(t)) {
                    Some(T::Fn(ps, _)) if !ps.is_empty() => {
                        let recv = ps[0].clone();
                        let ret = match lacon_interp::resolve::conv_for(name) {
                            Some(to) => self.conv(to, &recv, None, span),
                            None => {
                                let rest: Vec<Arg> = ps[1..].iter().map(|t| Arg::Ty(t.clone(), span, false)).collect();
                                self.method_on(recv, span, name, None, &rest, span)
                            }
                        };
                        T::Fn(ps, Box::new(ret))
                    }
                    _ => T::Unknown,
                },
                Func::Closure(..) => T::Unknown,
            },
            _ => T::Unknown,
        }
    }

    /// The type of a user function used as a value.
    fn fn_value(&mut self, id: FnId) -> T {
        let fd = &self.prog.fns[id as usize];
        let subst: Vec<(Rc<str>, T)> = fd.generics.iter().map(|g| (g.as_str().into(), self.s.fresh())).collect();
        let ps = fd.params.iter().map(|p| self.s.from_ir(&p.ty, &subst)).collect();
        let r = fd.ret.as_ref().map_or(T::Unit, |t| self.s.from_ir(t, &subst));
        T::func(ps, r)
    }

    fn struct_lit(&mut self, id: u32, fields: &[Option<Ex>], _span: Span) -> T {
        let sd = &self.prog.structs[id as usize];
        let subst: Vec<(Rc<str>, T)> = sd.generics.iter().map(|g| (g.as_str().into(), self.s.fresh())).collect();
        for (i, f) in fields.iter().enumerate() {
            let Some(e) = f else { continue };
            match sd.fields.get(i) {
                Some(fd) => {
                    let want = self.s.from_ir(&fd.ty, &subst);
                    self.check(e, &want, Site::Field { ty: &sd.name, field: &fd.name });
                }
                None => {
                    self.infer(e);
                }
            }
        }
        T::Struct(id, subst.into_iter().map(|(_, t)| t).collect())
    }

    fn variant(&mut self, id: u32, tag: u32, args: &[Ex], _span: Span, exp: Option<&T>) -> T {
        let ed = &self.prog.enums[id as usize];
        let subst: Vec<(Rc<str>, T)> = match exp.map(|t| self.s.resolve(t)) {
            Some(T::Enum(eid, targs)) if eid == id && targs.len() == ed.generics.len() => {
                ed.generics.iter().map(|g| g.as_str().into()).zip(targs).collect()
            }
            _ => ed.generics.iter().map(|g| (g.as_str().into(), self.s.fresh())).collect(),
        };
        let vd = &ed.variants[tag as usize];
        for (i, a) in args.iter().enumerate() {
            match vd.fields.get(i) {
                Some(ft) => {
                    let want = self.s.from_ir(ft, &subst);
                    self.check(a, &want, Site::Variant { name: &vd.name });
                }
                None => {
                    self.infer(a);
                }
            }
        }
        T::Enum(id, subst.into_iter().map(|(_, t)| t).collect())
    }

    /// The type stored at a place (`x`, `x.f`, `x[i]`). `write` reads the
    /// root's declared type rather than what narrowing knows.
    fn place_type(&mut self, p: &Place, write: bool) -> T {
        let (depth, slot) = match p.root {
            Root::Local(s) => (0, s),
            Root::Up(d, s) => (d, s),
        };
        let mut t = if write && p.path.is_empty() { self.decl_type(depth, slot) } else { self.slot_type(depth, slot) };
        let mut prev_span = Span { start: p.span.start, end: p.span.start + p.name.len() as u32 };
        // The fields so far, while every step is one: a read sees them
        // narrowed, and a store the declared type of the last.
        let mut fields = Some((self.root_key(&p.root), Vec::new()));
        let n = p.path.len();
        for (i, seg) in p.path.iter().enumerate() {
            t = match seg {
                Seg::Field(name, sp) => {
                    let ft = self.field_type(&t, name, None, prev_span, Span { start: p.span.start, end: sp.end });
                    if let Some((_, path)) = fields.as_mut() {
                        path.push(name.clone());
                    }
                    if write && i + 1 == n {
                        ft
                    } else {
                        self.narrowed_field(fields.clone(), ft)
                    }
                }
                Seg::Index(e, sp) => {
                    fields = None;
                    let it = self.infer(e);
                    let whole = Span { start: p.span.start, end: sp.end.max(e.span().end + 1) };
                    self.index_type(&t, &it, Some(e), prev_span, whole, write)
                }
            };
            prev_span = match seg {
                Seg::Field(_, sp) => Span { start: p.span.start, end: sp.end },
                Seg::Index(e, _) => Span { start: p.span.start, end: e.span().end + 1 },
            };
        }
        t
    }

    /// `recv.name`: a struct field, a tuple element, or a method without
    /// arguments.
    fn field_type(&mut self, recv: &T, name: &str, user: Option<&Rc<[FnId]>>, recv_span: Span, span: Span) -> T {
        let r = self.s.resolve(recv);
        match &r {
            T::Struct(id, args) => {
                let sd = &self.prog.structs[*id as usize];
                if let Some(fd) = sd.fields.iter().find(|f| f.name == name) {
                    let subst: Vec<(Rc<str>, T)> = if args.len() == sd.generics.len() {
                        sd.generics.iter().map(|g| g.as_str().into()).zip(args.iter().cloned()).collect()
                    } else {
                        sd.generics.iter().map(|g| (g.as_str().into(), T::Unknown)).collect()
                    };
                    return self.s.from_ir(&fd.ty, &subst);
                }
            }
            T::Tuple(ts) => {
                if let Ok(i) = name.parse::<usize>() {
                    return match ts.get(i) {
                        Some(t) => t.clone(),
                        None => {
                            let ty = self.show(&r);
                            self.err("E0203", span, format!("{ty} has no element {i} (length {})", ts.len()));
                            T::Unknown
                        }
                    };
                }
            }
            T::Var(_) => return self.defer(r, DKind::Field { name: name.into(), user: user.cloned() }, span),
            T::Unknown | T::Never | T::Param(_) => return T::Unknown,
            _ => {}
        }
        if user.is_some() || is_method(name) {
            return self.method_on(r, recv_span, name, user, &[], span);
        }
        if name.parse::<usize>().is_ok() {
            let ty = self.show(&r);
            let fix = format!("{}[{name}]", self.snippet(recv_span));
            self.err_fix("E0203", span, format!("`.{name}` needs a tuple, got {ty}; index a list with `[{name}]`"), Some(fix));
            return T::Unknown;
        }
        match &r {
            T::Opt(inner) => self.err_opt(recv_span, inner, true),
            T::Res(inner) => self.err_res(recv_span, inner, true),
            T::Struct(id, _) => {
                let sd = &self.prog.structs[*id as usize];
                let names: Vec<&str> = sd.fields.iter().map(|f| f.name.as_str()).collect();
                let msg = format!("{} has no field `{name}`; fields: {}", sd.name, names.join(", "));
                self.err("E0208", span, msg);
            }
            _ => {
                let ty = self.show(&r);
                self.err("E0203", span, format!("{ty} has no field `{name}`"));
            }
        }
        T::Unknown
    }

    fn index_type(&mut self, recv: &T, idx: &T, idx_ex: Option<&Ex>, recv_span: Span, span: Span, write: bool) -> T {
        let r = self.s.resolve(recv);
        let idx_span = idx_ex.map_or(span, |e| e.span());
        let want_int = |c: &mut Checker, what: &str| {
            if !c.s.coerce(idx, &INT) {
                match c.s.resolve(idx) {
                    T::Opt(inner) => c.err_opt(idx_span, &inner, false),
                    T::Res(inner) => c.err_res(idx_span, &inner, false),
                    t => {
                        let got = c.show(&t);
                        c.err("E0301", idx_span, format!("{what} index must be int, got {got}"));
                    }
                }
            }
        };
        match r {
            T::List(e) => {
                want_int(self, "list");
                *e
            }
            T::Str => {
                if write {
                    self.err("E0410", span, "strings are immutable; build a new string (e.g. with slices and `+`)");
                }
                want_int(self, "string");
                T::Str
            }
            T::Map(k, v) => {
                if !self.s.coerce(idx, &k) {
                    self.mismatch(Site::Key, idx, &k, idx_span, idx_ex);
                }
                *v
            }
            T::Range => {
                want_int(self, "range");
                INT
            }
            T::Tuple(ts) => match idx_ex {
                Some(Ex::Lit(Value::Int(n), _)) => {
                    let i = if *n < 0 { ts.len() as i64 + n } else { *n };
                    ts.get(i as usize).cloned().unwrap_or(T::Unknown)
                }
                _ => T::Unknown,
            },
            T::Opt(inner) => {
                self.err_opt(recv_span, &inner, true);
                T::Unknown
            }
            T::Res(inner) => {
                self.err_res(recv_span, &inner, true);
                T::Unknown
            }
            T::Var(_) => self.defer(r, DKind::Index { idx: idx.clone(), write }, span),
            T::Unknown | T::Never | T::Param(_) => T::Unknown,
            T::Set(_) => {
                self.err("E0301", span, "a set has no order or index; test membership with `x in s`");
                T::Unknown
            }
            t => {
                let ty = self.show(&t);
                self.err("E0301", span, format!("cannot index {ty}"));
                T::Unknown
            }
        }
    }

    fn slice_type(&mut self, t: &T, recv_span: Span, span: Span) -> T {
        match self.s.resolve(t) {
            r @ (T::List(_) | T::Str | T::Range | T::Var(_)) => r,
            T::Opt(inner) => {
                self.err_opt(recv_span, &inner, true);
                T::Unknown
            }
            T::Res(inner) => {
                self.err_res(recv_span, &inner, true);
                T::Unknown
            }
            T::Unknown | T::Never | T::Param(_) | T::Tuple(_) => T::Unknown,
            r => {
                let ty = self.show(&r);
                self.err("E0301", span, format!("cannot slice {ty}"));
                T::Unknown
            }
        }
    }

    // ----- calls -----

    fn call_fn(&mut self, fns: &Rc<[FnId]>, args: &[Ex], span: Span) -> T {
        if fns.len() == 1 || args.is_empty() {
            let a: Vec<Arg> = args.iter().map(Arg::Ex).collect();
            return self.call_user(fns[0], None, &a, span);
        }
        let t0 = self.infer(&args[0]);
        let mut pick = None;
        for &id in fns.iter() {
            let fd = &self.prog.fns[id as usize];
            let req = fd.params.iter().filter(|p| p.default.is_none()).count();
            if args.len() < req || args.len() > fd.params.len() {
                continue;
            }
            let subst: Vec<(Rc<str>, T)> = fd.generics.iter().map(|g| (g.as_str().into(), T::Unknown)).collect();
            let want = self.s.from_ir(&fd.params[0].ty, &subst);
            if self.s.could_coerce(&t0, &want) {
                pick = Some(id);
                break;
            }
        }
        let Some(id) = pick else {
            let name = self.prog.fns[fns[0] as usize].name.clone();
            let got = self.show(&t0);
            self.err("E0301", span, format!("no `{name}` takes {got} first"));
            for a in &args[1..] {
                self.infer(a);
            }
            return T::Unknown;
        };
        let mut a: Vec<Arg> = vec![Arg::Ty(t0, args[0].span(), is_fn_ex(&args[0]))];
        a.extend(args[1..].iter().map(Arg::Ex));
        self.call_user(id, None, &a, span)
    }

    /// Calls user function `id`, with the receiver of a method call first.
    fn call_user(&mut self, id: FnId, recv: Option<(T, Span)>, args: &[Arg], _span: Span) -> T {
        let fd = &self.prog.fns[id as usize];
        let subst: Vec<(Rc<str>, T)> = fd.generics.iter().map(|g| (g.as_str().into(), self.s.fresh())).collect();
        let mut all: Vec<Arg> = Vec::new();
        if let Some((t, s)) = recv {
            all.push(Arg::Ty(t, s, false));
        }
        for a in args {
            all.push(match a {
                Arg::Ex(e) => Arg::Ex(e),
                Arg::Ty(t, s, f) => Arg::Ty(t.clone(), *s, *f),
            });
        }
        for (i, a) in all.iter().enumerate() {
            let Some(p) = fd.params.get(i) else {
                self.arg_type(a);
                continue;
            };
            let want = self.s.from_ir(&p.ty, &subst);
            let site = Site::Arg { func: &fd.name, param: &p.name };
            if p.mode == Mode::Mut {
                let t = match a {
                    Arg::Ex(e) => self.expr(e, Some(&want)),
                    Arg::Ty(t, _, _) => t.clone(),
                };
                if !self.s.unify(&t, &want) {
                    self.mismatch(site, &t, &want, a.span(), None);
                }
            } else {
                self.check_arg(a, &want, site);
            }
        }
        fd.ret.as_ref().map_or(T::Unit, |t| self.s.from_ir(t, &subst))
    }

    fn call_value(&mut self, f: &Ex, args: &[Ex], span: Span) -> T {
        let ft = self.infer(f);
        match self.s.resolve(&ft) {
            T::Fn(ps, r) => {
                if ps.len() != args.len() {
                    self.err("E0206", span, format!("`{}` takes {} argument(s), got {}", self.snippet(f.span()), ps.len(), args.len()));
                }
                for (a, p) in args.iter().zip(ps.iter()) {
                    self.check(a, p, Site::Plain);
                }
                for a in args.iter().skip(ps.len()) {
                    self.infer(a);
                }
                *r
            }
            T::Var(_) => {
                let ts: Vec<T> = args.iter().map(|a| self.infer(a)).collect();
                let r = self.s.fresh();
                self.s.unify(&ft, &T::func(ts, r.clone()));
                r
            }
            T::Unknown | T::Param(_) | T::Never => {
                for a in args {
                    self.infer(a);
                }
                T::Unknown
            }
            t => {
                let ty = self.show(&t);
                self.err("E0301", span, format!("cannot call {ty}"));
                for a in args {
                    self.infer(a);
                }
                T::Unknown
            }
        }
    }

    /// `recv.name(args)`: a user function whose first parameter takes the
    /// receiver, or a builtin method.
    pub(crate) fn method_on(&mut self, recv: T, recv_span: Span, name: &str, user: Option<&Rc<[FnId]>>, args: &[Arg], span: Span) -> T {
        let r = self.s.resolve(&recv);
        if let Some(ids) = user {
            let mut pick = None;
            for &id in ids.iter() {
                let fd = &self.prog.fns[id as usize];
                let req = fd.params.iter().filter(|p| p.default.is_none()).count();
                if fd.params.is_empty() || args.len() + 1 < req || args.len() + 1 > fd.params.len() {
                    continue;
                }
                let subst: Vec<(Rc<str>, T)> = fd.generics.iter().map(|g| (g.as_str().into(), T::Unknown)).collect();
                let want = self.s.from_ir(&fd.params[0].ty, &subst);
                if matches!(r, T::Var(_)) && is_method(name) {
                    break;
                }
                if self.s.could_coerce(&r, &want) {
                    pick = Some(id);
                    break;
                }
            }
            if let Some(id) = pick {
                return self.call_user(id, Some((recv, recv_span)), args, span);
            }
            if !is_method(name) {
                let fd = &self.prog.fns[ids[0] as usize];
                if let Some(p0) = fd.params.first() {
                    let want = self.s.from_ir(&p0.ty, &[]);
                    for a in args {
                        self.arg_type(a);
                    }
                    if matches!(r, T::Opt(_) | T::Res(_)) && self.s.could_coerce(&strip(&r), &want) {
                        match &r {
                            T::Opt(inner) => self.err_opt(recv_span, inner, true),
                            T::Res(inner) => self.err_res(recv_span, inner, true),
                            _ => {}
                        }
                        return fd.ret.as_ref().map_or(T::Unit, |t| self.s.from_ir(t, &[]));
                    }
                    if !matches!(r, T::Unknown | T::Never) {
                        let (ws, gs) = (self.show(&want), self.show(&r));
                        self.err("E0301", span, format!("`{name}` takes {ws} first, got {gs}"));
                    }
                }
                return T::Unknown;
            }
        }
        match &r {
            T::Var(_) => {
                let targs: Vec<(T, Span, bool)> = args
                    .iter()
                    .map(|a| {
                        let t = self.arg_type(a);
                        (t, a.span(), a.is_fn())
                    })
                    .collect();
                return self.defer(r, DKind::Method { name: name.into(), user: user.cloned(), args: targs }, span);
            }
            T::Unknown | T::Never => {
                for a in args {
                    self.arg_type(a);
                }
                return T::Unknown;
            }
            T::Opt(inner) | T::Res(inner) => {
                let is_opt = matches!(r, T::Opt(_));
                match name {
                    "unwrap" | "expect" => {
                        for a in args {
                            self.arg_type(a);
                        }
                        return (**inner).clone();
                    }
                    "is_none" | "is_some" | "is_err" | "is_ok" => return T::Bool,
                    // `m.get(k).map(it.len) ?? 0`: `none` stays `none`, and
                    // a value goes to the lambda.
                    "map" if is_opt && args.len() == 1 && args[0].is_fn() => {
                        let u = self.s.fresh();
                        let want = T::func(vec![(**inner).clone()], u.clone());
                        match &args[0] {
                            Arg::Ex(Ex::Lambda(def)) => self.lambda(def, Some(&want), Some(name)),
                            a => self.check_arg(a, &want, Site::Lambda { method: name }),
                        };
                        self.opt_map.push(span);
                        // An optional from the lambda isn't wrapped again: at
                        // run time both are a value or `none`.
                        return match self.s.resolve(&u) {
                            T::Opt(_) => u,
                            _ => T::opt(u),
                        };
                    }
                    "str" | "to_string" => {
                        if !is_opt {
                            self.err_res(recv_span, inner, true);
                        }
                        return T::Str;
                    }
                    _ => {
                        for a in args {
                            self.arg_type(a);
                        }
                        if is_opt {
                            self.err_opt(recv_span, inner, true);
                        } else {
                            self.err_res(recv_span, inner, true);
                        }
                        return T::Unknown;
                    }
                }
            }
            T::Mixed(_) => {
                for a in args {
                    self.arg_type(a);
                }
                if matches!(name, "str" | "to_string") {
                    return T::Str;
                }
                self.err_mixed(recv_span, &r);
                return T::Unknown;
            }
            _ => {}
        }
        self.builtin_method(r, recv_span, name, args, span)
    }

    fn builtin_method(&mut self, recv: T, recv_span: Span, name: &str, args: &[Arg], span: Span) -> T {
        let fn_args: Vec<bool> = args.iter().map(|a| a.is_fn()).collect();
        let sig = match self.method_sig(&recv, name, args.len(), &fn_args) {
            Ok(s) => s,
            Err(e) => {
                for a in args {
                    self.arg_type(a);
                }
                match e {
                    NoSig::Missing(msg) => {
                        let fix = if msg.contains("`.map(it.") { Some(format!("{}.map(it.{name}())", self.snippet(recv_span))) } else { None };
                        self.err_fix("E0203", span, msg, fix);
                    }
                    NoSig::Immutable => {
                        self.err("E0410", span, format!("strings are immutable, so there is no `{name}`; build a new one: `s += \"x\"`"));
                    }
                }
                return T::Unknown;
            }
        };
        if args.len() < sig.required || (args.len() > sig.params.len() && !sig.variadic) {
            let want = if sig.required == sig.params.len() { format!("{}", sig.params.len()) } else { format!("{} to {}", sig.required, sig.params.len()) };
            self.err("E0206", span, format!("`{name}` takes {want} argument(s), got {}", args.len()));
        }
        // `xs.fold(0, ...)` over floats accumulates a float.
        if name == "fold" && args.len() == 2 {
            if let (Some(T::Fn(ps, _)), Some(e)) = (sig.params.get(1).map(|t| self.s.resolve(t)), self.elem_of(&recv)) {
                if matches!(self.s.resolve(&e), T::Float) {
                    let init = self.arg_type(&args[0]);
                    if matches!(self.s.resolve(&init), T::Int(_)) {
                        self.s.unify(&ps[0], &T::Float);
                    }
                    self.s.coerce(&init, &sig.params[0]);
                    let a1 = &args[1];
                    self.check_arg(a1, &sig.params[1], Site::Lambda { method: name });
                    return sig.ret;
                }
            }
        }
        let mut arg_types = Vec::new();
        for (i, a) in args.iter().enumerate() {
            if let Some((_, elem)) = sig.iter_args.iter().find(|(j, _)| *j == i) {
                let t = self.arg_type(a);
                let el = match self.s.resolve(&t) {
                    T::Var(_) => self.defer(t.clone(), DKind::Iter { keys_only: false }, a.span()),
                    rt => match self.elem_of(&rt) {
                        Some(el) => el,
                        None => {
                            let got = self.show(&rt);
                            self.err("E0301", a.span(), format!("`{name}` needs a list or other collection, got {got}"));
                            T::Unknown
                        }
                    },
                };
                if !self.s.coerce(&el, elem) {
                    let (ws, gs) = (self.show(elem), self.show(&el));
                    self.err("E0301", a.span(), format!("argument {} of `{name}`: want items of {ws}, got {gs}", i + 1));
                }
                arg_types.push(t);
                continue;
            }
            let want = match sig.params.get(i).or_else(|| if sig.variadic { sig.params.last() } else { None }) {
                Some(w) => w.clone(),
                None => {
                    arg_types.push(self.arg_type(a));
                    continue;
                }
            };
            let site = if matches!(self.s.resolve(&want), T::Fn(..)) { Site::Lambda { method: name } } else { Site::MethodArg { method: name, n: i } };
            let t = match (a, self.s.resolve(&want)) {
                // A lambda's body is checked against the return type the
                // method needs, which gives better messages than comparing
                // whole function types.
                (Arg::Ex(Ex::Lambda(def)), T::Fn(..)) => self.lambda(def, Some(&want), Some(name)),
                _ => self.check_arg(a, &want, site),
            };
            arg_types.push(t);
        }
        let mut ret = sig.ret;
        if sig.num_mix {
            let float = matches!(self.s.resolve(&recv), T::Float) || arg_types.iter().any(|t| matches!(self.s.resolve(t), T::Float));
            if float {
                ret = T::Float;
            }
        }
        if name == "pow" {
            self.int_pow(&ret, args.first().and_then(|a| if let Arg::Ex(e) = a { Some(*e) } else { None }));
        }
        if let Some((c, x)) = sig.elems_of {
            let el = match self.s.resolve(&c) {
                T::Var(_) => self.defer(c, DKind::Iter { keys_only: false }, span),
                T::Opt(inner) => *inner,
                rc => self.elem_of(&rc).unwrap_or(T::Unknown),
            };
            self.s.unify(&x, &el);
        }
        if matches!(name, "sum" | "product") && args.is_empty() {
            match self.s.resolve(&ret) {
                T::Int(_) | T::Float | T::Var(_) | T::Unknown | T::Param(_) | T::Never => {}
                T::Res(inner) => {
                    let snip = self.snippet(recv_span).to_string();
                    self.err(
                        "E0406",
                        span,
                        format!("`{snip}` holds {}, values that may be errors; convert with `?` first, e.g. `.map(int(it)?)`", self.show(&T::res(*inner))),
                    );
                    return T::Unknown;
                }
                t => {
                    let ty = self.show(&t);
                    self.err("E0301", span, format!("`{name}` needs numbers, got items of {ty}"));
                    return T::Unknown;
                }
            }
        }
        ret
    }

    fn lambda(&mut self, def: &LambdaDef, exp: Option<&T>, method: Option<&str>) -> T {
        let n = def.params.len();
        let mut frame = Frame::new(def.nslots, &mut self.s);
        let exp = exp.map(|t| self.s.resolve(t));
        let (ptys, outer, ret_want): (Vec<T>, Option<Vec<T>>, Option<T>) = match &exp {
            Some(T::Fn(ps, r)) if ps.len() == n => (ps.clone(), None, Some((**r).clone())),
            Some(T::Fn(ps, r)) if ps.len() == 1 && n > 1 => {
                // `pairs.map(|k, v| ...)` spreads a tuple over the parameters.
                let fresh: Vec<T> = (0..n).map(|_| self.s.fresh()).collect();
                if !self.s.unify(&ps[0], &T::Tuple(fresh.clone())) {
                    if let Some(msg) = comparator_hint(method, n) {
                        self.err("E0206", def.span, msg);
                    } else {
                        let got = self.show(&ps[0]);
                        self.err("E0206", def.span, format!("this lambda takes {n} arguments, but gets one {got}"));
                    }
                }
                (fresh, Some(ps.clone()), Some((**r).clone()))
            }
            Some(T::Fn(ps, r)) => {
                let what = method.map_or("it".to_string(), |m| format!("`{m}`"));
                let msg = comparator_hint(method, n)
                    .unwrap_or_else(|| format!("this lambda takes {n} argument(s), but {what} passes {}", ps.len()));
                self.err("E0206", def.span, msg);
                ((0..n).map(|_| self.s.fresh()).collect(), Some(ps.clone()), Some((**r).clone()))
            }
            _ => ((0..n).map(|_| self.s.fresh()).collect(), None, None),
        };
        for (slot, t) in def.params.iter().zip(&ptys) {
            frame.decl[*slot as usize] = t.clone();
        }
        self.frames.push(frame);
        let saved_loops = std::mem::take(&mut self.loops);
        let body = match &ret_want {
            Some(r) if !self.s.is_var(r) => {
                let t = self.expr(&def.body, Some(r));
                if !self.s.coerce(&t, r) {
                    let site = match method {
                        Some(m) => Site::Lambda { method: m },
                        None => Site::Plain,
                    };
                    self.mismatch(site, &t, r, def.body.span(), Some(&def.body));
                }
                r.clone()
            }
            Some(r) => {
                let t = self.expr(&def.body, None);
                self.s.unify(r, &t);
                t
            }
            None => self.expr(&def.body, None),
        };
        self.loops = saved_loops;
        self.log_frame(&def.body);
        self.frames.pop();
        let n = self.frames.len();
        self.fields.retain(|((f, _), _)| *f < n);
        T::func(outer.unwrap_or(ptys), body)
    }

    fn builtin(&mut self, b: Builtin, args: &[Ex], span: Span, exp: Option<&T>) -> T {
        let str_arg = |c: &mut Checker, i: usize| {
            if let Some(a) = args.get(i) {
                c.check(a, &T::Str, Site::Plain);
            }
        };
        match b {
            Builtin::Print | Builtin::Eprint | Builtin::IoWrite => {
                for a in args {
                    let t = self.infer(a);
                    self.display(&t, a);
                }
                T::Unit
            }
            Builtin::Range => {
                for a in args {
                    self.check(a, &INT, Site::Plain);
                }
                T::Range
            }
            Builtin::Min | Builtin::Max => {
                let mut acc = T::Never;
                let mut prev: Option<(T, Span)> = None;
                for a in args {
                    let t = self.infer(a);
                    if let Some((p, ps)) = &prev {
                        self.order_check(p, &t, *ps, a.span(), span);
                    }
                    acc = self.s.join(&acc, &t);
                    prev = Some((t, a.span()));
                }
                match self.s.resolve(&acc) {
                    T::Mixed(_) => T::Unknown,
                    t => t,
                }
            }
            Builtin::SetNew | Builtin::HeapNew | Builtin::ListNew => {
                let want = exp.map(|t| self.s.resolve(t)).and_then(|t| match t {
                    T::Set(e) | T::Heap(e) | T::List(e) => Some(*e),
                    _ => None,
                });
                let elem = match args.first() {
                    None => want.unwrap_or_else(|| self.s.fresh()),
                    Some(a) => {
                        let t = self.infer(a);
                        match self.s.resolve(&t) {
                            T::Var(_) => self.defer(t, DKind::Iter { keys_only: false }, a.span()),
                            rt => match self.elem_of(&rt) {
                                Some(e) => e,
                                None => {
                                    let got = self.show(&rt);
                                    self.err("E0301", a.span(), format!("cannot make a collection from {got}"));
                                    T::Unknown
                                }
                            },
                        }
                    }
                };
                match b {
                    Builtin::SetNew => T::set(elem),
                    Builtin::HeapNew => T::Heap(Box::new(elem)),
                    _ => T::list(elem),
                }
            }
            Builtin::Conv(to) => match args.first() {
                Some(a) => {
                    let t = self.infer(a);
                    self.conv(to, &t, Some(a), span)
                }
                None => T::Unknown,
            },
            Builtin::Panic => {
                for a in args {
                    let t = self.infer(a);
                    self.display(&t, a);
                }
                T::Never
            }
            Builtin::FsRead => {
                str_arg(self, 0);
                T::res(T::Str)
            }
            Builtin::FsLines => {
                str_arg(self, 0);
                T::res(T::list(T::Str))
            }
            Builtin::FsWrite | Builtin::FsAppend => {
                str_arg(self, 0);
                if let Some(a) = args.get(1) {
                    let t = self.infer(a);
                    self.display(&t, a);
                }
                T::res(T::Unit)
            }
            Builtin::FsExists => {
                str_arg(self, 0);
                T::Bool
            }
            Builtin::FsRemove => {
                str_arg(self, 0);
                T::res(T::Unit)
            }
            Builtin::IoRead => T::Str,
            Builtin::IoLines | Builtin::OsArgs => T::list(T::Str),
            Builtin::IoReadLine => T::opt(T::Str),
            Builtin::OsEnv => {
                str_arg(self, 0);
                T::opt(T::Str)
            }
            Builtin::OsExit => {
                if let Some(a) = args.first() {
                    self.check(a, &INT, Site::Plain);
                }
                T::Never
            }
            Builtin::TimeNow => T::Float,
        }
    }

    /// `int(x)`, `f64(x)`, `str(x)`, `bool(x)` and `x as T`. Parsing a string
    /// can fail, so it gives a result, unless the string is a literal that
    /// parses.
    fn conv(&mut self, to: ConvTo, t: &T, e: Option<&Ex>, span: Span) -> T {
        let r = self.s.resolve(t);
        let lit = match e {
            Some(Ex::Lit(Value::Str(s), _)) => Some(s.trim().to_string()),
            _ => None,
        };
        let espan = e.map_or(span, |e| e.span());
        let (target, name): (T, &str) = match to {
            ConvTo::Int(n, _, _) => (T::Int(n), n),
            ConvTo::Float => (T::Float, "f64"),
            ConvTo::Str => (T::Str, "str"),
            ConvTo::Bool => (T::Bool, "bool"),
        };
        match &r {
            T::Opt(inner) => {
                if !matches!(to, ConvTo::Str) {
                    self.err_opt(espan, inner, false);
                    return target;
                }
            }
            T::Res(inner) => {
                self.err_res(espan, inner, false);
                return target;
            }
            T::Mixed(_) if !matches!(to, ConvTo::Str) => {
                self.err_mixed(espan, &r);
                return target;
            }
            _ => {}
        }
        match to {
            ConvTo::Str => {
                if let Some(e) = e {
                    self.display(&r, e);
                }
                T::Str
            }
            ConvTo::Int(..) | ConvTo::Float | ConvTo::Bool => match &r {
                T::Str => {
                    let parses = lit.is_some_and(|s| match to {
                        ConvTo::Int(..) => s.parse::<i64>().is_ok(),
                        ConvTo::Float => s.parse::<f64>().is_ok(),
                        _ => s == "true" || s == "false",
                    });
                    if parses {
                        target
                    } else {
                        T::res(target)
                    }
                }
                T::Int(_) | T::Float | T::Bool => {
                    if matches!(to, ConvTo::Bool) && matches!(r, T::Float) {
                        self.err("E0301", espan, "cannot convert f64 to bool; compare explicitly, e.g. `x != 0.0`");
                    }
                    target
                }
                T::Unknown | T::Var(_) | T::Param(_) | T::Never | T::Mixed(_) => target,
                _ => {
                    let got = self.show(&r);
                    self.err("E0301", espan, format!("cannot convert {got} to {name}"));
                    target
                }
            },
        }
    }

    // ----- operators -----

    fn unary(&mut self, op: UnOp, t: &T, xspan: Span, span: Span) -> T {
        let r = self.s.resolve(t);
        match (&r, op) {
            (T::Unknown | T::Never | T::Param(_), _) => T::Unknown,
            (T::Var(_), UnOp::Neg) => r,
            (T::Var(_), _) => {
                self.s.unify(&r, &T::Bool);
                T::Bool
            }
            (T::Opt(inner), _) => {
                self.err_opt(xspan, inner, true);
                T::Unknown
            }
            (T::Res(inner), _) => {
                self.err_res(xspan, inner, true);
                T::Unknown
            }
            (T::Int(_) | T::Float, UnOp::Neg) => r,
            (T::Tuple(ts), UnOp::Neg) if ts.iter().all(|t| matches!(self.s.resolve(t), T::Int(_) | T::Float | T::Var(_) | T::Unknown)) => r,
            (T::Bool, UnOp::Not | UnOp::Bang) => T::Bool,
            (T::Int(_), UnOp::Bang) => r,
            _ => {
                let ty = self.show(&r);
                let what = if op == UnOp::Neg { "negate" } else { "apply `not` to" };
                let hint = match (&r, op) {
                    (T::Str, UnOp::Neg) => "; to sort descending, sort then `.rev()`",
                    (T::Str | T::List(_) | T::Map(..) | T::Set(_), UnOp::Not | UnOp::Bang) => "; no truthiness: test `x.is_empty()`",
                    (T::Int(_) | T::Float, UnOp::Not) => "; no truthiness: test `n == 0`",
                    _ => "",
                };
                self.err("E0301", span, format!("cannot {what} {ty}{hint}"));
                T::Unknown
            }
        }
    }

    pub(crate) fn binop_types(&mut self, op: BinOp, l: &T, r: &T, lspan: Span, rspan: Span, span: Span) -> T {
        let (a, b) = (self.s.resolve(l), self.s.resolve(r));
        for (t, sp) in [(&a, lspan), (&b, rspan)] {
            match t {
                T::Opt(inner) => {
                    self.err_opt(sp, inner, true);
                    return T::Unknown;
                }
                T::Res(inner) => {
                    self.err_res(sp, inner, true);
                    return T::Unknown;
                }
                T::Mixed(_) => {
                    self.err_mixed(sp, t);
                    return T::Unknown;
                }
                _ => {}
            }
        }
        if matches!(a, T::Unknown | T::Param(_)) || matches!(b, T::Unknown | T::Param(_)) {
            return T::Unknown;
        }
        if matches!(a, T::Never) {
            return b;
        }
        if matches!(b, T::Never) {
            return a;
        }
        if matches!(a, T::Var(_)) || matches!(b, T::Var(_)) {
            if let (T::Var(x), T::Var(y)) = (&a, &b) {
                if x == y {
                    // `a + b` on two elements of a list not yet known to
                    // hold `int!`: the result is their type, checked once
                    // it is known.
                    let kind = DKind::Arith { op, l: a.clone(), r: b, lspan, rspan };
                    self.deferred.push(Deferred { recv: a.clone(), kind, ret: a.clone(), span, ctx: self.ctx.clone() });
                    return a;
                }
            }
            return self.defer(a.clone(), DKind::Arith { op, l: a, r: b, lspan, rspan }, span);
        }
        use BinOp::*;
        let arith = matches!(op, Add | Sub | Mul | Div | Rem | Pow);
        match (op, &a, &b) {
            (_, T::Int(_), T::Int(_)) => a,
            (_, T::Int(_) | T::Float, T::Int(_) | T::Float) if arith => T::Float,
            (Add, T::Str, T::Str) => T::Str,
            (Mul, T::Str, T::Int(_)) | (Mul, T::Int(_), T::Str) => T::Str,
            (Add, T::List(x), T::List(y)) => T::list(self.s.join(x, y)),
            (Add, T::List(x), T::Range) => T::list(self.s.join(x, &INT)),
            (Mul, T::List(_), T::Int(_)) => a,
            (Mul, T::Int(_), T::List(_)) => b,
            (BitOr | BitAnd | Sub | BitXor, T::Set(x), T::Set(y)) => {
                if !self.s.unify(x, y) {
                    let (ls, rs) = (self.show(&a), self.show(&b));
                    self.err("E0301", span, format!("cannot combine {ls} and {rs}"));
                }
                a
            }
            (BitOr, T::Map(..), T::Map(..)) => {
                if !self.s.unify(&a, &b) {
                    let (ls, rs) = (self.show(&a), self.show(&b));
                    self.err("E0301", span, format!("cannot combine {ls} and {rs}"));
                }
                a
            }
            (BitAnd | BitOr | BitXor, T::Bool, T::Bool) => T::Bool,
            (Rem, T::Str, _) => {
                self.err("E0301", span, "no `%` formatting; every string interpolates: \"{n} items, {x:.2}\"");
                T::Str
            }
            (Add, T::Str, _) | (Add, _, T::Str) => {
                let (ls, rs) = (self.show(&a), self.show(&b));
                let fix = format!("\"{{{}}}{{{}}}\"", self.snippet(lspan), self.snippet(rspan));
                let fix = if fix.contains('"') && fix[1..fix.len() - 1].contains('"') { None } else { Some(fix) };
                self.err_fix("E0301", span, format!("cannot add {ls} and {rs}; interpolate instead: \"{{a}}{{b}}\""), fix);
                T::Str
            }
            _ => {
                let (ls, rs) = (self.show(&a), self.show(&b));
                self.err("E0301", span, format!("cannot apply `{}` to {ls} and {rs}", op.symbol()));
                T::Unknown
            }
        }
    }

    fn eq_check(&mut self, a: &T, b: &T, aspan: Span, bspan: Span, span: Span) {
        let (ra, rb) = (self.s.resolve(a), self.s.resolve(b));
        match (&ra, &rb) {
            (T::Res(_), T::Res(_)) => return,
            (T::Res(inner), _) => return self.err_res(aspan, inner, true),
            (_, T::Res(inner)) => return self.err_res(bspan, inner, true),
            _ => {}
        }
        // `x == none` says `x` is an optional.
        for (x, y) in [(&ra, &rb), (&rb, &ra)] {
            if matches!(x, T::Var(_)) && matches!(y, T::Opt(_)) {
                let inner = self.s.fresh();
                self.s.unify(x, &T::opt(inner));
            }
        }
        let (ra, rb) = (self.s.resolve(&ra), self.s.resolve(&rb));
        let (sa, sb) = (strip_opt(&self.s, &ra), strip_opt(&self.s, &rb));
        if matches!(sa, T::Mixed(_)) || matches!(sb, T::Mixed(_)) {
            return;
        }
        if numeric(&sa) && numeric(&sb) {
            return;
        }
        if self.s.could_unify(&sa, &sb) {
            self.s.unify(&sa, &sb);
            return;
        }
        if self.s.could_coerce(&sa, &sb) || self.s.could_coerce(&sb, &sa) {
            return;
        }
        let (x, y) = (self.show(&ra), self.show(&rb));
        self.err("E0301", span, format!("cannot compare {x} with {y}"));
    }

    fn order_check(&mut self, a: &T, b: &T, aspan: Span, bspan: Span, span: Span) {
        let (ra, rb) = (self.s.resolve(a), self.s.resolve(b));
        for (t, sp) in [(&ra, aspan), (&rb, bspan)] {
            match t {
                T::Opt(inner) => return self.err_opt(sp, inner, true),
                T::Res(inner) => return self.err_res(sp, inner, true),
                T::Mixed(_) => return self.err_mixed(sp, t),
                _ => {}
            }
        }
        if numeric(&ra) && numeric(&rb) {
            return;
        }
        if self.s.could_unify(&ra, &rb) {
            self.s.unify(&ra, &rb);
            return;
        }
        if self.s.could_coerce(&ra, &rb) || self.s.could_coerce(&rb, &ra) {
            return;
        }
        let (x, y) = (self.show(&ra), self.show(&rb));
        self.err("E0301", span, format!("cannot compare {x} with {y}"));
    }

    fn in_check(&mut self, x: &T, c: &T, xspan: Span, cspan: Span, span: Span) {
        let rc = self.s.resolve(c);
        let elem = match &rc {
            T::List(e) | T::Set(e) | T::Heap(e) => (**e).clone(),
            T::Map(k, _) => (**k).clone(),
            T::Range => INT,
            T::Str => T::Str,
            T::Opt(inner) => return self.err_opt(cspan, inner, true),
            T::Res(inner) => return self.err_res(cspan, inner, true),
            T::Tuple(_) | T::Unknown | T::Var(_) | T::Param(_) | T::Never => return,
            t => {
                let ty = self.show(t);
                return self.err("E0301", cspan, format!("cannot use `in` with {ty}"));
            }
        };
        let rx = self.s.resolve(x);
        if let T::Res(inner) = &rx {
            return self.err_res(xspan, inner, true);
        }
        let sx = strip_opt(&self.s, &rx);
        if self.s.is_var(&elem) {
            self.s.unify(&elem, &sx);
            return;
        }
        if numeric(&sx) && numeric(&self.s.resolve(&elem)) {
            return;
        }
        if self.s.could_unify(&sx, &elem) {
            self.s.unify(&sx, &elem);
            return;
        }
        let (xs, cs) = (self.show(&rx), self.show(&rc));
        let what = if matches!(rc, T::Map(..)) { "keys of " } else { "" };
        self.err("E0301", span, format!("`in` looks for {xs} among the {what}{cs}"));
    }

    fn try_type(&mut self, t: &T, span: Span) -> T {
        match self.s.resolve(t) {
            T::Opt(inner) => {
                if !self.ctx.passes_none(&self.s) {
                    self.err_cant_pass(span, true);
                }
                *inner
            }
            T::Res(inner) => {
                if !self.ctx.passes_err(&self.s) {
                    self.err_cant_pass(span, false);
                }
                if matches!(self.s.resolve(&inner), T::Opt(_)) {
                    self.res_try.push(span);
                }
                *inner
            }
            r @ T::Var(_) => self.defer(r, DKind::Try, span),
            r => r,
        }
    }

    fn err_cant_pass(&mut self, span: Span, opt: bool) {
        let name = self.ctx.name.clone();
        if self.ctx.kind == Kind::Const {
            return self.err("E0302", span, "`?` needs a function to pass the failure to; constants can't fail");
        }
        let sig = self.ctx.sig.clone();
        let msg = match self.ctx.ret.as_ref().map(|r| self.s.resolve(r)) {
            None | Some(T::Unit) => format!("`?` passes {} up, but `{name}` can't fail; declare it `{sig}!`", if opt { "`none`" } else { "the error" }),
            Some(r) => {
                let rs = self.show(&r);
                format!("`?` passes {} up, but `{name}` returns {rs}; make it `{rs}!` (or `{rs}?`)", if opt { "`none`" } else { "the error" })
            }
        };
        self.err_sig_fix("E0302", span, msg, self.ctx.head, format!("{sig}! ="));
    }

    // ----- control flow -----

    fn if_expr(&mut self, cond: &Ex, then: &Ex, els: Option<&Ex>, exp: Option<&T>) -> T {
        self.cond(cond);
        self.solve(false);
        let pre = self.narrow_state();
        let (ft, ff) = self.facts(cond);
        self.apply(&ft);
        let tt = self.expr(then, exp);
        self.solve(false);
        let then_state = self.narrow_state();
        let then_div = matches!(self.s.resolve(&tt), T::Never);
        self.set_narrow_state(&pre);
        self.apply(&ff);
        let et = match els {
            Some(e) => self.expr(e, exp),
            None => T::Unit,
        };
        let else_state = self.narrow_state();
        let else_div = els.is_some() && matches!(self.s.resolve(&et), T::Never);
        let after = match (then_div, else_div) {
            (true, false) => else_state,
            (false, true) => then_state,
            _ => Self::merge_states(&[then_state, else_state]).unwrap(),
        };
        self.set_narrow_state(&after);
        match els {
            Some(_) => self.s.join(&tt, &et),
            None => match self.s.resolve(&tt) {
                // A result in an `if` without `else` is still ignored if it fails.
                T::Res(inner) => T::res(self.s.join(&inner, &T::Unit)),
                _ => T::Unit,
            },
        }
    }

    fn match_expr(&mut self, scrut: &Ex, arms: &[Arm], span: Span, exp: Option<&T>) -> T {
        let st = self.infer(scrut);
        let pre = self.narrow_state();
        let (mut none_seen, mut err_seen) = (false, false);
        let mut result = T::Never;
        let mut states = Vec::new();
        for arm in arms {
            self.solve(false);
            self.set_narrow_state(&pre);
            let mut t = self.s.resolve(&st);
            if none_seen {
                if let T::Opt(inner) = t {
                    t = self.s.resolve(&inner);
                }
            }
            if err_seen {
                if let T::Res(inner) = t {
                    t = self.s.resolve(&inner);
                }
            }
            self.pattern(&arm.pat, &t, scrut.span(), span);
            if let Some(g) = &arm.guard {
                self.cond(g);
            }
            let bt = self.expr(&arm.body, exp);
            if !matches!(self.s.resolve(&bt), T::Never) {
                states.push(self.narrow_state());
            }
            result = self.s.join(&result, &bt);
            if arm.guard.is_none() {
                none_seen |= covers_none(&arm.pat);
                err_seen |= covers_err(&arm.pat);
            }
        }
        match Self::merge_states(&states) {
            Some(st) => self.set_narrow_state(&st),
            None => self.set_narrow_state(&pre),
        }
        result
    }

    /// The type of the elements a `for` loop binds.
    fn for_elem(&mut self, t: &T, keys_only: bool, span: Span) -> T {
        match self.s.resolve(t) {
            T::Map(k, v) => {
                if keys_only {
                    *k
                } else {
                    T::Tuple(vec![*k, *v])
                }
            }
            T::Opt(inner) => {
                self.err_opt(span, &inner, false);
                T::Unknown
            }
            T::Res(inner) => {
                self.err_res(span, &inner, false);
                T::Unknown
            }
            r @ T::Var(_) => self.defer(r, DKind::Iter { keys_only }, span),
            r => match self.elem_of(&r) {
                Some(e) => e,
                None => {
                    let ty = self.show(&r);
                    self.err("E0301", span, format!("cannot loop over {ty}"));
                    T::Unknown
                }
            },
        }
    }

    // ----- statements -----

    /// Checks a statement; true when it never finishes (`return`, `fail`,
    /// `break`, `continue`, an endless loop).
    fn stmt(&mut self, s: &St) -> bool {
        match s {
            St::Expr(e, span) => {
                let t = self.infer(e);
                match self.s.resolve(&t) {
                    T::Res(_) => {
                        let snip = self.snippet(e.span()).to_string();
                        self.err_res_ignored(*span, &snip);
                        false
                    }
                    T::Never => true,
                    _ => false,
                }
            }
            St::Bind(pat, e, ty, _) => {
                let t = match ty {
                    Some(d) => {
                        let d = self.s.from_ir(d, &[]);
                        self.check(e, &d, Site::Bind);
                        d
                    }
                    None => {
                        let t = self.infer(e);
                        if matches!(self.s.resolve(&t), T::Unit) {
                            if let Some(fid) = self.unit_fn_of(e) {
                                self.err_unit(fid, e.span());
                            }
                        }
                        t
                    }
                };
                if let PatIr::Bind(slot) = pat {
                    let top = self.frames.len() == 1;
                    let mut t = t;
                    if ty.is_none() && top && matches!(self.s.resolve(&t), T::Int(_)) {
                        if self.widen.contains(slot) {
                            t = T::Float;
                        }
                        self.frames[0].widenable[*slot as usize] = true;
                    }
                    let res = ty.is_none() && matches!(self.s.resolve(&t), T::Res(_));
                    self.bind_slot(*slot, t);
                    // `n = int(s)` then `n + 1`: the `?` goes where `n` is
                    // bound, not on every use.
                    let postfix = matches!(
                        e,
                        Ex::CallFn { .. } | Ex::CallValue { .. } | Ex::CallBuiltin { .. } | Ex::Method { .. } | Ex::Field { .. } | Ex::Index { .. } | Ex::Cast { .. }
                    );
                    if res && postfix {
                        self.frames.last_mut().unwrap().from[*slot as usize] = Some(e.span());
                    }
                    return false;
                }
                self.pattern(pat, &t, e.span(), e.span());
                false
            }
            St::Assign { place, op, value, span } => {
                self.assign(place, *op, value, *span);
                false
            }
            St::AssignMulti { targets, value, span } => {
                let t = self.infer(value);
                let parts: Vec<T> = match self.s.resolve(&t) {
                    T::Tuple(ts) if ts.len() == targets.len() => ts,
                    T::List(e) => vec![*e; targets.len()],
                    T::Var(_) => {
                        let ts: Vec<T> = (0..targets.len()).map(|_| self.s.fresh()).collect();
                        self.s.unify(&t, &T::Tuple(ts.clone()));
                        ts
                    }
                    T::Opt(inner) => {
                        self.err_opt(value.span(), &inner, false);
                        vec![T::Unknown; targets.len()]
                    }
                    T::Res(inner) => {
                        self.err_res(value.span(), &inner, false);
                        vec![T::Unknown; targets.len()]
                    }
                    T::Unknown => vec![T::Unknown; targets.len()],
                    r => {
                        let ty = self.show(&r);
                        self.err("E0301", *span, format!("cannot unpack {ty} into {} names", targets.len()));
                        vec![T::Unknown; targets.len()]
                    }
                };
                for (tg, pt) in targets.iter().zip(parts) {
                    match tg {
                        Target::Bind(slot) => self.bind_slot(*slot, pt),
                        Target::Place(p) => {
                            let want = self.place_type(p, true);
                            if !self.s.coerce(&pt, &want) {
                                let target = self.snippet(p.span).to_string();
                                self.mismatch(Site::Assign { target: &target }, &pt, &want, p.span, None);
                            }
                            if p.path.is_empty() {
                                self.note_assigned(p, &pt);
                            } else {
                                self.note_field_assigned(p, &want, &pt);
                            }
                        }
                    }
                }
                false
            }
            St::For { pat, iter, body, span } => {
                let it = self.infer(iter);
                let keys_only = matches!(pat, PatIr::Bind(_) | PatIr::Wild);
                let elem = self.for_elem(&it, keys_only, iter.span());
                let pre = self.narrow_state();
                self.forget_assigned(body);
                self.pattern(pat, &elem, iter.span(), *span);
                self.loops.push(false);
                for s in body {
                    self.stmt(s);
                    self.solve(false);
                }
                self.loops.pop();
                self.set_narrow_state(&pre);
                self.forget_assigned(body);
                false
            }
            St::While { cond, body, .. } => {
                let pre = self.narrow_state();
                self.forget_assigned(body);
                self.cond(cond);
                let (ft, _) = self.facts(cond);
                self.apply(&ft);
                self.loops.push(false);
                for s in body {
                    self.stmt(s);
                    self.solve(false);
                }
                let broke = self.loops.pop().unwrap_or(true);
                self.set_narrow_state(&pre);
                self.forget_assigned(body);
                matches!(cond, Ex::Lit(Value::Bool(true), _)) && !broke
            }
            St::Break(_) => {
                if let Some(b) = self.loops.last_mut() {
                    *b = true;
                }
                true
            }
            St::Continue(_) => true,
            St::Return(e, span) => {
                self.ret(e.as_ref(), *span);
                true
            }
            St::Fail(e, span) => {
                let t = self.infer(e);
                self.display(&t, e);
                if !self.ctx.can_fail(&self.s) {
                    let name = self.ctx.name.clone();
                    let sig = self.ctx.sig.clone();
                    if self.ctx.kind == Kind::Const {
                        self.err("E0302", *span, "`fail` needs a function that returns `T!`");
                    } else if self.ctx.returns_optional(&self.s) {
                        self.err("E0302", *span, format!("`fail` needs a `T!` function, but `{name}` returns an optional; `return none` instead, or make it `T!`"));
                    } else {
                        self.err_sig_fix("E0302", *span, format!("`fail` needs a `T!` function; declare `{name}` with `!`"), self.ctx.head, format!("{sig}! ="));
                    }
                }
                true
            }
            St::Assert { cond, msg, .. } => {
                self.cond(cond);
                if let Some(m) = msg {
                    let t = self.infer(m);
                    self.display(&t, m);
                }
                false
            }
        }
    }

    /// Records what an assignment to a whole variable says about `none`.
    fn note_assigned(&mut self, p: &Place, value: &T) {
        let (fi, slot) = match p.root {
            Root::Local(s) => (self.frames.len() - 1, s),
            Root::Up(d, s) => (self.frame_index(d), s),
        };
        let decl_opt = matches!(self.s.resolve(&self.frames[fi].decl[slot as usize]), T::Opt(_));
        let val_opt = matches!(self.s.resolve(value), T::Opt(_) | T::Var(_) | T::Unknown);
        self.frames[fi].narrowed[slot as usize] = decl_opt && !val_opt;
        self.forget_fields((fi, slot), &[]);
    }

    fn assign(&mut self, place: &Place, op: Option<BinOp>, value: &Ex, span: Span) {
        let want = self.place_type(place, true);
        let root_slot = match place.root {
            Root::Local(s) if place.path.is_empty() && self.frames.len() == 1 => Some(s),
            _ => None,
        };
        let widenable = root_slot.is_some_and(|s| self.frames[0].widenable[s as usize]);
        let got = match op {
            None => self.expr(value, Some(&want)),
            Some(op) => {
                let vt = self.infer(value);
                if self.s.is_var(&want) {
                    // `m[k] += 1` on a map of unknown values: they are ints.
                    self.s.unify(&want, &vt);
                }
                let cur = if place.path.is_empty() { self.place_type(place, false) } else { want.clone() };
                let t = self.binop_types(op, &cur, &vt, place.span, value.span(), span);
                if op == BinOp::Pow {
                    self.int_pow(&t, Some(value));
                }
                t
            }
        };
        if !self.s.coerce(&got, &want) {
            let float_into_int = matches!(self.s.resolve(&got), T::Float) && matches!(self.s.resolve(&want), T::Int(_));
            if float_into_int && widenable {
                let slot = root_slot.unwrap();
                if self.widen.insert(slot) {
                    self.widen_hit = true;
                    return;
                }
            }
            if float_into_int && op.is_some() {
                let (name, ws) = (self.snippet(place.span).to_string(), self.show(&want));
                self.err("E0301", span, format!("`{name}` holds {ws}, and this makes it f64; start it as a float, e.g. `var {name} = 0.0`"));
            } else {
                let sp = if op.is_some() { span } else { value.span() };
                let target = self.snippet(place.span).to_string();
                self.mismatch(Site::Assign { target: &target }, &got, &want, sp, Some(value));
            }
        }
        if place.path.is_empty() {
            self.note_assigned(place, &got);
        } else {
            self.note_field_assigned(place, &want, &got);
        }
    }

    /// Records what a store to a field says about `none`: an optional field
    /// given a value that isn't one is known not to be `none`, and what was
    /// known about the place, or inside it, is forgotten.
    fn note_field_assigned(&mut self, p: &Place, want: &T, got: &T) {
        self.forget_place(p);
        if let Some(k) = self.place_field_key(p) {
            let decl_opt = matches!(self.s.resolve(want), T::Opt(_));
            let val_opt = matches!(self.s.resolve(got), T::Opt(_) | T::Var(_) | T::Unknown);
            if decl_opt && !val_opt {
                self.fields.push(k);
            }
        }
    }

    fn ret(&mut self, e: Option<&Ex>, span: Span) {
        let ret = self.ctx.ret.clone().map(|r| self.s.resolve(&r));
        let name = self.ctx.name.clone();
        match (e, ret) {
            (Some(e), None | Some(T::Unit)) => {
                let t = self.infer(e);
                if self.ctx.kind == Kind::Main && matches!(self.s.resolve(&t), T::Int(_)) {
                    // The fix replaces the whole `return x`; `span` is the keyword.
                    let fix = Fix::at(span.to(e.span()), format!("os.exit({})", self.snippet(e.span())));
                    self.diags.push(Diag::new("E0305", span, "`main` returns no value; to set the exit code, call `os.exit(code)`").with_fix(Some(fix)));
                } else if !matches!(self.s.resolve(&t), T::Unit | T::Never | T::Unknown) && self.ctx.kind != Kind::Test {
                    let sig = self.ctx.sig.clone();
                    let ty = self.show(&t);
                    self.err_sig_fix(
                        "E0305",
                        e.span(),
                        format!("`{name}` declares no return type, so `return` takes no value; declare it: `{sig} {ty} =`"),
                        self.ctx.head,
                        format!("{sig} {ty} ="),
                    );
                }
            }
            (Some(e), Some(r)) => {
                if let T::Res(inner) = &r {
                    if matches!(self.s.resolve(inner), T::Unit) {
                        self.infer(e);
                        return;
                    }
                }
                self.check(e, &r, Site::Return);
            }
            (None, None | Some(T::Unit)) => {}
            (None, Some(T::Res(inner))) if matches!(self.s.resolve(&inner), T::Unit) => {}
            (None, Some(T::Opt(_))) => {
                self.err_fix("E0301", span, format!("`{name}` returns an optional, so `return` needs a value; use `return none`"), Some("return none".into()));
            }
            (None, Some(r)) => {
                let rs = self.show(&r);
                self.err("E0301", span, format!("`{name}` returns {rs}, so `return` needs a value"));
            }
        }
    }

    // ----- patterns -----

    fn pattern(&mut self, p: &PatIr, t: &T, scrut_span: Span, span: Span) {
        let r = self.s.resolve(t);
        match p {
            PatIr::Wild => {}
            PatIr::Bind(slot) => self.bind_slot(*slot, r),
            PatIr::Lit(v) => {
                let lt = self.lit(v, None, span);
                let target = strip_opt(&self.s, &r);
                if matches!(target, T::Var(_)) {
                    self.s.unify(&target, &lt);
                } else if !(numeric(&target) && numeric(&lt)) && !self.s.could_unify(&target, &lt) && !matches!(target, T::Mixed(_)) {
                    let (ps, ts) = (self.show(&lt), self.show(&r));
                    self.err("E0301", span, format!("a {ps} pattern can't match {ts}"));
                }
            }
            PatIr::Range { lo, hi, .. } => {
                let target = strip_opt(&self.s, &r);
                for v in [lo, hi].into_iter().flatten() {
                    let lt = self.lit(v, None, span);
                    if matches!(target, T::Var(_)) {
                        self.s.unify(&target, &lt);
                    }
                }
            }
            PatIr::Tuple(ps) => {
                let parts: Vec<T> = match &r {
                    T::Tuple(ts) if ts.len() == ps.len() => ts.clone(),
                    T::List(e) => vec![(**e).clone(); ps.len()],
                    T::Var(_) => {
                        let ts: Vec<T> = (0..ps.len()).map(|_| self.s.fresh()).collect();
                        self.s.unify(&r, &T::Tuple(ts.clone()));
                        ts
                    }
                    T::Opt(inner) => {
                        self.err_opt(scrut_span, inner, false);
                        vec![T::Unknown; ps.len()]
                    }
                    T::Res(inner) => {
                        self.err_res(scrut_span, inner, false);
                        vec![T::Unknown; ps.len()]
                    }
                    T::Unknown | T::Never | T::Param(_) => vec![T::Unknown; ps.len()],
                    _ => {
                        let ty = self.show(&r);
                        self.err("E0301", span, format!("cannot unpack {ty} into {} names", ps.len()));
                        vec![T::Unknown; ps.len()]
                    }
                };
                for (p, t) in ps.iter().zip(parts) {
                    self.pattern(p, &t, scrut_span, span);
                }
            }
            PatIr::List { items, rest } => {
                let e = match &r {
                    T::List(e) => (**e).clone(),
                    T::Var(_) => {
                        let e = self.s.fresh();
                        self.s.unify(&r, &T::list(e.clone()));
                        e
                    }
                    T::Opt(inner) => {
                        self.err_opt(scrut_span, inner, false);
                        T::Unknown
                    }
                    _ => T::Unknown,
                };
                for p in items {
                    self.pattern(p, &e, scrut_span, span);
                }
                if let Some(Some(slot)) = rest {
                    self.bind_slot(*slot, T::list(e));
                }
            }
            PatIr::Variant { id, tag, args } => {
                let target = strip_opt(&self.s, &r);
                let ed = &self.prog.enums[*id as usize];
                let targs: Vec<T> = match &target {
                    T::Enum(eid, targs) if eid == id => targs.clone(),
                    T::Var(_) => {
                        let targs: Vec<T> = ed.generics.iter().map(|_| self.s.fresh()).collect();
                        self.s.unify(&target, &T::Enum(*id, targs.clone()));
                        targs
                    }
                    T::Unknown | T::Never | T::Param(_) => vec![],
                    _ => {
                        let ty = self.show(&r);
                        let vname = &ed.variants[*tag as usize].name;
                        self.err("E0301", span, format!("variant `{vname}` of {} can't match {ty}", ed.name));
                        vec![]
                    }
                };
                let subst: Vec<(Rc<str>, T)> = if targs.len() == ed.generics.len() {
                    ed.generics.iter().map(|g| g.as_str().into()).zip(targs).collect()
                } else {
                    ed.generics.iter().map(|g| (g.as_str().into(), T::Unknown)).collect()
                };
                let fields: Vec<T> = ed.variants[*tag as usize].fields.iter().map(|f| self.s.from_ir(f, &subst)).collect();
                for (p, ft) in args.iter().zip(fields) {
                    self.pattern(p, &ft, scrut_span, span);
                }
            }
            PatIr::Struct { id, fields } => {
                let sd = &self.prog.structs[*id as usize];
                let target = strip_opt(&self.s, &r);
                let targs = match &target {
                    T::Struct(sid, targs) if sid == id => targs.clone(),
                    T::Var(_) => {
                        let targs: Vec<T> = sd.generics.iter().map(|_| self.s.fresh()).collect();
                        self.s.unify(&target, &T::Struct(*id, targs.clone()));
                        targs
                    }
                    _ => vec![],
                };
                let subst: Vec<(Rc<str>, T)> = if targs.len() == sd.generics.len() {
                    sd.generics.iter().map(|g| g.as_str().into()).zip(targs).collect()
                } else {
                    sd.generics.iter().map(|g| (g.as_str().into(), T::Unknown)).collect()
                };
                for (i, p) in fields {
                    let ft = self.s.from_ir(&sd.fields[*i].ty, &subst);
                    self.pattern(p, &ft, scrut_span, span);
                }
            }
            PatIr::Or(ps) => {
                for p in ps {
                    self.pattern(p, &r, scrut_span, span);
                }
            }
            PatIr::None => match &r {
                T::Opt(_) | T::Unknown | T::Never | T::Param(_) => {}
                T::Var(_) => {
                    let inner = self.s.fresh();
                    self.s.unify(&r, &T::opt(inner));
                }
                _ => {
                    let ty = self.show(&r);
                    self.err("E0301", span, format!("`none` can't match {ty}, which is never none"));
                }
            },
            PatIr::Err(p) => {
                match &r {
                    T::Res(_) | T::Unknown | T::Never | T::Param(_) => {}
                    T::Var(_) => {
                        let inner = self.s.fresh();
                        self.s.unify(&r, &T::res(inner));
                    }
                    _ => {
                        let ty = self.show(&r);
                        self.err("E0301", span, format!("`err(...)` can't match {ty}, which is never an error"));
                    }
                }
                self.pattern(p, &T::Str, scrut_span, span);
            }
            PatIr::Ok(p) => {
                let inner = match &r {
                    T::Res(x) => (**x).clone(),
                    _ => r.clone(),
                };
                self.pattern(p, &inner, scrut_span, span);
            }
            PatIr::Some(p) => {
                let inner = match &r {
                    T::Opt(x) => (**x).clone(),
                    T::Var(_) => {
                        let inner = self.s.fresh();
                        self.s.unify(&r, &T::opt(inner.clone()));
                        inner
                    }
                    // After a `none:` arm the value is already narrowed.
                    _ => r.clone(),
                };
                self.pattern(p, &inner, scrut_span, span);
            }
        }
    }
}

fn strip(t: &T) -> T {
    match t {
        T::Opt(x) | T::Res(x) => (**x).clone(),
        t => t.clone(),
    }
}

fn strip_opt(s: &Subst, t: &T) -> T {
    match s.resolve(t) {
        T::Opt(x) => s.resolve(&x),
        t => t,
    }
}

fn numeric(t: &T) -> bool {
    matches!(t, T::Int(_) | T::Float)
}

/// `xs.sort_by(|a, b| ...)`, Rust's comparator, where Lacon takes a key.
fn comparator_hint(method: Option<&str>, n: usize) -> Option<String> {
    match method {
        Some(m @ ("sort_by" | "min_by" | "max_by")) if n == 2 => Some(format!(
            "`{m}` takes a key, not a comparator: `xs.{m}(it.age)`, or `xs.{m}((it.city, -it.age))` for two keys"
        )),
        _ => None,
    }
}

fn covers_none(p: &PatIr) -> bool {
    match p {
        PatIr::None => true,
        PatIr::Or(ps) => ps.iter().any(covers_none),
        _ => false,
    }
}

fn covers_err(p: &PatIr) -> bool {
    match p {
        PatIr::Err(inner) => matches!(**inner, PatIr::Wild | PatIr::Bind(_)),
        PatIr::Or(ps) => ps.iter().any(covers_err),
        _ => false,
    }
}

/// The span of the value a block ends with, for return mismatches.
fn last_value_span(e: &Ex) -> Span {
    last_value(e).span()
}

/// The expression a block ends with.
fn last_value(e: &Ex) -> &Ex {
    match e {
        Ex::Block(_, Some(t)) => last_value(t),
        e => e,
    }
}

/// Slots of the current frame that statements assign to.
fn assigned_stmts(stmts: &[St], out: &mut HashSet<u32>) {
    for s in stmts {
        match s {
            St::Expr(e, _) | St::Fail(e, _) => assigned_ex(e, out),
            St::Bind(_, e, _, _) => assigned_ex(e, out),
            St::Assign { place, value, .. } => {
                if let Root::Local(s) = place.root {
                    out.insert(s);
                }
                assigned_ex(value, out);
            }
            St::AssignMulti { targets, value, .. } => {
                for t in targets {
                    if let Target::Place(p) = t {
                        if let Root::Local(s) = p.root {
                            out.insert(s);
                        }
                    }
                }
                assigned_ex(value, out);
            }
            St::For { iter, body, .. } => {
                assigned_ex(iter, out);
                assigned_stmts(body, out);
            }
            St::While { cond, body, .. } => {
                assigned_ex(cond, out);
                assigned_stmts(body, out);
            }
            St::Return(Some(e), _) => assigned_ex(e, out),
            St::Assert { cond, .. } => assigned_ex(cond, out),
            _ => {}
        }
    }
}

fn assigned_ex(e: &Ex, out: &mut HashSet<u32>) {
    match e {
        Ex::Block(stmts, tail) => {
            assigned_stmts(stmts, out);
            if let Some(t) = tail {
                assigned_ex(t, out);
            }
        }
        Ex::If { cond, then, els, .. } => {
            assigned_ex(cond, out);
            assigned_ex(then, out);
            if let Some(e) = els {
                assigned_ex(e, out);
            }
        }
        Ex::Match { scrut, arms, .. } => {
            assigned_ex(scrut, out);
            for a in arms {
                assigned_ex(&a.body, out);
            }
        }
        Ex::CallFn { args, places, .. } => {
            for p in places.iter().flatten() {
                if let Root::Local(s) = p.root {
                    out.insert(s);
                }
            }
            for a in args {
                assigned_ex(a, out);
            }
        }
        Ex::Method { recv, args, user, .. } => {
            if let (Recv::Place(p), Some(_)) = (recv, user) {
                if let Root::Local(s) = p.root {
                    out.insert(s);
                }
            }
            for a in args {
                assigned_ex(a, out);
            }
        }
        Ex::And(a, b, _) | Ex::Or(a, b, _) | Ex::Coalesce(a, b, _) | Ex::Binary { l: a, r: b, .. } => {
            assigned_ex(a, out);
            assigned_ex(b, out);
        }
        Ex::Unary { e, .. } | Ex::Try { e, .. } | Ex::Cast { e, .. } => assigned_ex(e, out),
        Ex::CallValue { args, .. } | Ex::CallBuiltin { args, .. } | Ex::List(args, _) | Ex::Tuple(args, _) | Ex::Set(args, _) => {
            for a in args {
                assigned_ex(a, out);
            }
        }
        _ => {}
    }
}
