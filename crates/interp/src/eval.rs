//! Tree-walking evaluator over the IR.

use std::cell::{Cell, RefCell};
use std::io::Write;
use std::rc::Rc;

use lacon_syntax::ast::{BinOp, CmpOp, Mode, UnOp};
use lacon_syntax::fmtspec::FmtSpec;
use lacon_syntax::Span;

use crate::builtins::{self, arith, contains, is_mutator};
use crate::frame::Frame;
use crate::ir::*;
use crate::value::{self, Func, Obj, RangeV, Value};

/// Non-local control flow. `?` and `fail` return an error value from the
/// enclosing named function, also from inside a lambda.
pub enum Ctrl {
    Return(Value),
    Break,
    Continue,
    Panic(Box<Panic>),
}

pub struct Panic {
    pub code: &'static str,
    pub msg: String,
    pub span: Span,
    pub detail: Vec<String>,
    /// Set by `os.exit`.
    pub exit: Option<i32>,
    /// Enclosing calls, innermost first: (function name, call site).
    pub trace: Vec<(String, Span)>,
}

pub type R<T> = Result<T, Ctrl>;

pub fn panic(code: &'static str, span: Span, msg: impl Into<String>) -> Ctrl {
    Ctrl::Panic(Box::new(Panic { code, msg: msg.into(), span, detail: vec![], exit: None, trace: vec![] }))
}

/// Deep enough for recursive solutions over inputs of ~10^5, with the
/// interpreter thread's large stack.
const MAX_DEPTH: u32 = 20_000;

pub enum Out {
    Stdout(std::io::BufWriter<std::io::Stdout>),
    Capture(Vec<u8>),
}

impl Out {
    pub fn write_str(&mut self, s: &str) {
        match self {
            Out::Stdout(w) => {
                let _ = w.write_all(s.as_bytes());
            }
            Out::Capture(v) => v.extend_from_slice(s.as_bytes()),
        }
    }

    pub fn flush(&mut self) {
        if let Out::Stdout(w) = self {
            let _ = w.flush();
        }
    }
}

/// What to create when a mutation reaches a missing map key.
#[derive(Clone, Copy)]
enum Viv<'v> {
    No,
    /// `m[k] = v`
    Insert,
    /// `m[k] += v`: the zero value of `v`'s type.
    ZeroOf(&'v Value),
    /// `m[k].push(x)`: an empty collection for the method.
    Method(&'v str),
}

pub struct Interp<'p> {
    pub prog: &'p Program,
    pub globals: RefCell<Vec<Value>>,
    pub out: RefCell<Out>,
    depth: Cell<u32>,
    pub stack: RefCell<Vec<(FnId, Span)>>,
    pub args: Vec<String>,
    pub stdin: RefCell<Option<std::io::StdinLock<'static>>>,
}

impl<'p> Interp<'p> {
    pub fn new(prog: &'p Program, out: Out, args: Vec<String>) -> Interp<'p> {
        Interp {
            prog,
            globals: RefCell::new(vec![Value::Unit; prog.consts.len()]),
            out: RefCell::new(out),
            depth: Cell::new(0),
            stack: RefCell::new(Vec::new()),
            args,
            stdin: RefCell::new(None),
        }
    }

    pub fn init_globals(&self) -> R<()> {
        for (i, c) in self.prog.consts.iter().enumerate() {
            let f = Frame::new(c.nslots, None);
            let v = self.eval(&c.value, &f)?;
            self.globals.borrow_mut()[i] = v;
        }
        Ok(())
    }

    pub fn reset(&self) {
        self.depth.set(0);
        self.stack.borrow_mut().clear();
    }

    pub fn trace(&self) -> Vec<(String, Span)> {
        self.stack.borrow().iter().rev().map(|(f, s)| (self.prog.fns[*f as usize].name.clone(), *s)).collect()
    }

    pub fn kind(&self, v: &Value) -> String {
        v.kind(self.prog)
    }

    pub fn repr(&self, v: &Value) -> String {
        value::repr(v, self.prog)
    }

    pub fn display(&self, v: &Value) -> String {
        value::display(v, self.prog)
    }

    /// A short form of a value for error messages.
    pub fn short(&self, v: &Value) -> String {
        let s = self.repr(v);
        if s.chars().count() > 60 {
            let t: String = s.chars().take(57).collect();
            format!("{t}...")
        } else {
            s
        }
    }

    // ----- blocks and statements -----

    fn exec_all(&self, stmts: &[St], f: &Rc<Frame>) -> R<()> {
        for s in stmts {
            self.exec(s, f)?;
        }
        Ok(())
    }

    fn exec(&self, s: &St, f: &Rc<Frame>) -> R<()> {
        match s {
            St::Expr(e, span) => {
                let v = self.eval(e, f)?;
                if let Value::Err(err) = v {
                    return Err(panic(
                        "E0406",
                        *span,
                        format!("error ignored: {}; add `?` to pass it up, or handle it with `??` or `match`", self.display(&err)),
                    ));
                }
                Ok(())
            }
            St::Bind(pat, e, span) => {
                let v = self.eval(e, f)?;
                if !self.match_pat(pat, &v, f) {
                    return Err(panic("E0414", *span, format!("cannot destructure {}", self.short(&v))));
                }
                Ok(())
            }
            St::Assign { place, op, value, span } => {
                let rhs = self.eval(value, f)?;
                match op {
                    None => self.with_place(place, f, Viv::Insert, |slot| {
                        *slot = rhs;
                        Ok(())
                    }),
                    Some(op) => {
                        let zero = rhs.clone();
                        self.with_place(place, f, Viv::ZeroOf(&zero), |slot| {
                            let old = std::mem::replace(slot, Value::Unit);
                            match arith(self.prog, *op, old, rhs) {
                                Ok(v) => {
                                    *slot = v;
                                    Ok(())
                                }
                                Err((code, msg)) => Err(panic(code, *span, msg)),
                            }
                        })
                    }
                }
            }
            St::AssignMulti { targets, value, span } => {
                let v = self.eval(value, f)?;
                let items = match &v {
                    Value::Tuple(xs) | Value::List(xs) if xs.len() == targets.len() => xs.clone(),
                    _ => return Err(panic("E0414", *span, format!("cannot unpack {} into {} names", self.short(&v), targets.len()))),
                };
                for (t, x) in targets.iter().zip(items.iter()) {
                    match t {
                        Target::Bind(slot) => f.set(*slot, x.clone()),
                        Target::Place(p) => self.with_place(p, f, Viv::Insert, |slot| {
                            *slot = x.clone();
                            Ok(())
                        })?,
                    }
                }
                Ok(())
            }
            St::For { pat, iter, body, span } => self.exec_for(pat, iter, body, *span, f),
            St::While { cond, body } => {
                loop {
                    let c = self.eval(cond, f)?;
                    if !self.truth(&c, cond_span(cond))? {
                        break;
                    }
                    match self.exec_all(body, f) {
                        Ok(()) | Err(Ctrl::Continue) => {}
                        Err(Ctrl::Break) => break,
                        Err(e) => return Err(e),
                    }
                }
                Ok(())
            }
            St::Break => Err(Ctrl::Break),
            St::Continue => Err(Ctrl::Continue),
            St::Return(e) => {
                let v = match e {
                    Some(e) => self.eval(e, f)?,
                    None => Value::Unit,
                };
                Err(Ctrl::Return(v))
            }
            St::Fail(e, _) => {
                let v = self.eval(e, f)?;
                let err = match v {
                    Value::Err(_) => v,
                    v => Value::Err(Rc::new(v)),
                };
                Err(Ctrl::Return(err))
            }
            St::Assert { cond, msg, span } => {
                let c = self.eval(cond, f)?;
                if !self.truth(&c, *span)? {
                    let m = match msg {
                        Some(m) => {
                            let v = self.eval(m, f)?;
                            format!("assertion failed: {}", self.display(&v))
                        }
                        None => "assertion failed".to_string(),
                    };
                    return Err(panic("E0421", *span, m));
                }
                Ok(())
            }
        }
    }

    fn exec_for(&self, pat: &PatIr, iter: &Ex, body: &[St], span: Span, f: &Rc<Frame>) -> R<()> {
        macro_rules! run_body {
            () => {
                match self.exec_all(body, f) {
                    Ok(()) | Err(Ctrl::Continue) => {}
                    Err(Ctrl::Break) => break,
                    Err(e) => return Err(e),
                }
            };
        }
        let it = self.eval(iter, f)?;
        let bind = |v: &Value| -> R<()> {
            if self.match_pat(pat, v, f) {
                Ok(())
            } else {
                Err(panic("E0414", span, format!("cannot destructure {} in `for`", self.short(v))))
            }
        };
        match &it {
            Value::Range(r) => {
                let r = (**r).clone();
                let n = r.len();
                if let PatIr::Bind(slot) = pat {
                    for i in 0..n {
                        f.set(*slot, Value::Int(r.get(i)));
                        run_body!();
                    }
                } else {
                    for i in 0..n {
                        bind(&Value::Int(r.get(i)))?;
                        run_body!();
                    }
                }
            }
            Value::List(xs) | Value::Tuple(xs) => {
                for x in xs.iter() {
                    bind(x)?;
                    run_body!();
                }
            }
            Value::Set(xs) => {
                for x in xs.iter() {
                    bind(x)?;
                    run_body!();
                }
            }
            Value::Heap(xs) => {
                for x in value::sorted(xs).iter() {
                    bind(x)?;
                    run_body!();
                }
            }
            Value::Map(m) => {
                // `for k, v in m` gives pairs; `for k in m` gives keys.
                let keys_only = matches!(pat, PatIr::Bind(_) | PatIr::Wild);
                for (k, v) in m.iter() {
                    if keys_only {
                        bind(k)?;
                    } else {
                        bind(&Value::tuple(vec![k.clone(), v.clone()]))?;
                    }
                    run_body!();
                }
            }
            Value::Str(s) => {
                for c in s.chars() {
                    bind(&Value::str(c.to_string()))?;
                    run_body!();
                }
            }
            Value::Err(e) => {
                return Err(panic("E0406", span, format!("cannot loop over an error ({}); add `?`", self.display(e))));
            }
            v => return Err(panic("E0301", span, format!("cannot loop over {}", self.kind(v)))),
        }
        Ok(())
    }

    pub fn truth(&self, v: &Value, span: Span) -> R<bool> {
        match v {
            Value::Bool(b) => Ok(*b),
            Value::Err(e) => Err(panic("E0406", span, format!("condition is an error ({}); add `?`", self.display(e)))),
            other => {
                let hint = match other {
                    Value::List(_) | Value::Map(_) | Value::Set(_) | Value::Heap(_) | Value::Str(_) => "; test emptiness with `x.is_empty()` or `x.len > 0`",
                    Value::Int(_) | Value::Float(_) => "; compare explicitly, e.g. `n != 0`",
                    _ => "; compare explicitly, e.g. `x != none`",
                };
                Err(panic("E0301", span, format!("condition must be bool, got {}{hint}", self.kind(other))))
            }
        }
    }

    // ----- expressions -----

    pub fn eval(&self, e: &Ex, f: &Rc<Frame>) -> R<Value> {
        match e {
            Ex::Lit(v) => Ok(v.clone()),
            Ex::Local(s) => Ok(f.get(*s)),
            Ex::Up(d, s) => Ok(f.up(*d).get(*s)),
            Ex::Global(g) => Ok(self.globals.borrow()[*g as usize].clone()),
            Ex::Str(pieces) => {
                let mut out = String::new();
                for p in pieces {
                    match p {
                        StrPiece::Lit(s) => out.push_str(s),
                        StrPiece::Expr(e, spec, args) => {
                            let v = self.eval(e, f)?;
                            if let Value::Err(err) = &v {
                                return Err(panic("E0406", cond_span(e), format!("interpolating an error ({}); add `?`", self.display(err))));
                            }
                            match spec {
                                Some(spec) if !args.is_empty() => {
                                    let mut spec = (**spec).clone();
                                    let mut args = args.iter();
                                    for (arg, slot) in [(spec.width_arg, 0), (spec.precision_arg, 1)] {
                                        if arg.is_none() {
                                            continue;
                                        }
                                        let a = args.next().unwrap();
                                        let n = self.eval(a, f)?;
                                        let n = usize::try_from(self.int_of(&n, cond_span(a))?)
                                            .map_err(|_| panic("E0301", cond_span(a), "format width and precision must not be negative"))?;
                                        if slot == 0 {
                                            spec.width = n;
                                        } else {
                                            spec.precision = Some(n);
                                        }
                                    }
                                    out.push_str(&self.format_spec(&v, &spec, cond_span(e))?)
                                }
                                Some(spec) => out.push_str(&self.format_spec(&v, spec, cond_span(e))?),
                                None => out.push_str(&self.display(&v)),
                            }
                        }
                    }
                }
                Ok(Value::str(out))
            }
            Ex::List(xs) => {
                let mut v = Vec::with_capacity(xs.len());
                for x in xs {
                    v.push(self.eval(x, f)?);
                }
                Ok(Value::list(v))
            }
            Ex::Tuple(xs) => {
                let mut v = Vec::with_capacity(xs.len());
                for x in xs {
                    v.push(self.eval(x, f)?);
                }
                Ok(Value::tuple(v))
            }
            Ex::Set(xs) => {
                let mut s = value::Set::default();
                for x in xs {
                    s.insert(self.eval(x, f)?);
                }
                Ok(Value::Set(Rc::new(s)))
            }
            Ex::Map(ps) => {
                let mut m = value::Map::default();
                for (k, v) in ps {
                    let k = self.eval(k, f)?;
                    let v = self.eval(v, f)?;
                    m.insert(k, v);
                }
                Ok(Value::Map(Rc::new(m)))
            }
            Ex::Struct { id, fields, span } => {
                let def = &self.prog.structs[*id as usize];
                let mut vals = Vec::with_capacity(fields.len());
                for (i, fe) in fields.iter().enumerate() {
                    let fd = &def.fields[i];
                    let v = match fe {
                        Some(e) => self.eval(e, f)?,
                        None => match &fd.default {
                            Some(d) => self.eval_thunk(d)?,
                            None => Value::Unit,
                        },
                    };
                    let v = self.coerce(v, &fd.ty).map_err(|got| {
                        panic("E0301", fe.as_ref().map_or(*span, |e| cond_span_or(e, *span)), format!("field `{}` of {}: want {} got {got}", fd.name, def.name, ty_name(&fd.ty, self.prog)))
                    })?;
                    vals.push(v);
                }
                Ok(Value::Struct(Rc::new(Obj { ty: *id, tag: 0, fields: vals })))
            }
            Ex::Variant { id, tag, args, span } => {
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.eval(a, f)?);
                }
                self.make_variant(*id, *tag, vals, *span)
            }
            Ex::Field { obj, name, user, span } => {
                let v = self.eval(obj, f)?;
                self.get_field(v, name, user.as_deref(), *span)
            }
            Ex::Index { obj, index, span } => {
                let o = self.eval(obj, f)?;
                let i = self.eval(index, f)?;
                self.index(&o, &i, *span)
            }
            Ex::Slice { obj, start, end, inclusive, span } => {
                let o = self.eval(obj, f)?;
                let s = match start {
                    Some(e) => Some(self.int_of(&self.eval(e, f)?, cond_span(e))?),
                    None => None,
                };
                let e2 = match end {
                    Some(e) => Some(self.int_of(&self.eval(e, f)?, cond_span(e))?),
                    None => None,
                };
                builtins::slice(self, &o, s, e2, *inclusive, *span)
            }
            Ex::CallFn { fns, args, places, span } => {
                let mut argv = Vec::with_capacity(args.len());
                for a in args {
                    argv.push(self.eval(a, f)?);
                }
                let fid = self.pick(fns, argv.first(), argv.len(), *span)?;
                let fd = &self.prog.fns[fid as usize];
                let muts: Vec<usize> = (0..argv.len())
                    .filter(|&i| fd.params.get(i).is_some_and(|p| p.mode == Mode::Mut) && places.get(i).is_some_and(|p| p.is_some()))
                    .collect();
                if muts.is_empty() {
                    return self.call_fid(fid, argv, *span);
                }
                // `mut` parameters: move the value in, run, write the result back.
                for &i in &muts {
                    argv[i] = Value::Unit;
                    argv[i] = self.with_place(places[i].as_ref().unwrap(), f, Viv::No, |s| Ok(std::mem::replace(s, Value::Unit)))?;
                }
                let (v, frame) = self.call_frame(fid, argv, *span)?;
                for &i in &muts {
                    let nv = frame.get(i as u32);
                    self.with_place(places[i].as_ref().unwrap(), f, Viv::No, |s| {
                        *s = nv;
                        Ok(())
                    })?;
                }
                Ok(v)
            }
            Ex::CallValue { f: fe, args, span } => {
                let fv = self.eval(fe, f)?;
                let mut argv = Vec::with_capacity(args.len());
                for a in args {
                    argv.push(self.eval(a, f)?);
                }
                self.call_value(&fv, argv, *span)
            }
            Ex::CallBuiltin { f: b, args, span } => {
                let mut argv = Vec::with_capacity(args.len());
                for a in args {
                    argv.push(self.eval(a, f)?);
                }
                builtins::call_builtin(self, *b, argv, *span)
            }
            Ex::Method { recv, name, user, args, span } => self.eval_method(recv, name, user.as_deref(), args, *span, f),
            Ex::Unary { op, e, span } => {
                let v = self.eval(e, f)?;
                match (op, &v) {
                    (UnOp::Neg, Value::Int(n)) => n.checked_neg().map(Value::Int).ok_or_else(|| panic("E0405", *span, "integer overflow")),
                    (UnOp::Neg, Value::Float(x)) => Ok(Value::Float(-x)),
                    (UnOp::Not | UnOp::Bang, Value::Bool(b)) => Ok(Value::Bool(!b)),
                    (UnOp::Bang, Value::Int(n)) => Ok(Value::Int(!n)),
                    (UnOp::Neg, Value::Tuple(xs)) => {
                        // `sort_by(-it.score, it.name)` style keys: negate numbers inside.
                        let mut out = Vec::new();
                        for x in xs.iter() {
                            out.push(match x {
                                Value::Int(n) => Value::Int(-n),
                                Value::Float(f) => Value::Float(-f),
                                _ => return Err(panic("E0301", *span, format!("cannot negate {}", self.kind(&v)))),
                            });
                        }
                        Ok(Value::tuple(out))
                    }
                    (_, Value::Err(err)) => Err(panic("E0406", *span, format!("operating on an error ({}); add `?`", self.display(err)))),
                    _ => {
                        let what = match op {
                            UnOp::Neg => "negate",
                            _ => "apply `not` to",
                        };
                        Err(panic("E0301", *span, format!("cannot {what} {}", self.kind(&v))))
                    }
                }
            }
            Ex::Binary { op, l, r, span } => {
                let a = self.eval(l, f)?;
                let b = self.eval(r, f)?;
                self.binop(*op, a, b, *span)
            }
            Ex::And(a, b, span) => {
                let x = self.eval(a, f)?;
                if !self.truth(&x, *span)? {
                    return Ok(Value::Bool(false));
                }
                let y = self.eval(b, f)?;
                Ok(Value::Bool(self.truth(&y, *span)?))
            }
            Ex::Or(a, b, span) => {
                let x = self.eval(a, f)?;
                if self.truth(&x, *span)? {
                    return Ok(Value::Bool(true));
                }
                let y = self.eval(b, f)?;
                Ok(Value::Bool(self.truth(&y, *span)?))
            }
            Ex::Coalesce(a, b) => {
                let x = self.eval(a, f)?;
                match x {
                    Value::None | Value::Err(_) => self.eval(b, f),
                    x => Ok(x),
                }
            }
            Ex::Compare { first, rest } => {
                let mut prev = self.eval(first, f)?;
                for (op, e, span) in rest {
                    let next = self.eval(e, f)?;
                    if !self.compare(*op, &prev, &next, *span)? {
                        return Ok(Value::Bool(false));
                    }
                    prev = next;
                }
                Ok(Value::Bool(true))
            }
            Ex::Range { start, end, inclusive, span } => {
                let s = match start {
                    Some(e) => self.int_of(&self.eval(e, f)?, *span)?,
                    None => 0,
                };
                let Some(end) = end else {
                    return Err(panic("E0301", *span, "a range needs an end (open ranges only work in slices like `xs[2..]`)"));
                };
                let mut e2 = self.int_of(&self.eval(end, f)?, *span)?;
                if *inclusive {
                    e2 += 1;
                }
                Ok(Value::Range(Rc::new(RangeV { start: s, end: e2, step: 1 })))
            }
            Ex::Try { e, fn_optional, span: _ } => {
                let v = self.eval(e, f)?;
                match v {
                    Value::Err(_) => Err(Ctrl::Return(v)),
                    Value::None => {
                        if *fn_optional {
                            Err(Ctrl::Return(Value::None))
                        } else {
                            Err(Ctrl::Return(Value::Err(Rc::new(Value::str("unexpected none")))))
                        }
                    }
                    v => Ok(v),
                }
            }
            Ex::Lambda(def) => Ok(Value::Func(Rc::new(Func::Closure(def.clone(), f.clone())))),
            Ex::If { cond, then, els, span } => {
                let c = self.eval(cond, f)?;
                if self.truth(&c, cond_span_or(cond, *span))? {
                    self.eval(then, f)
                } else if let Some(e) = els {
                    self.eval(e, f)
                } else {
                    Ok(Value::Unit)
                }
            }
            Ex::Match { scrut, arms, span } => {
                let v = self.eval(scrut, f)?;
                for arm in arms {
                    if !self.match_pat(&arm.pat, &v, f) {
                        continue;
                    }
                    if let Some(g) = &arm.guard {
                        let gv = self.eval(g, f)?;
                        if !self.truth(&gv, *span)? {
                            continue;
                        }
                    }
                    return self.eval(&arm.body, f);
                }
                Err(panic("E0411", *span, format!("no match arm for {}", self.short(&v))))
            }
            Ex::Block(stmts, tail) => {
                self.exec_all(stmts, f)?;
                match tail {
                    Some(t) => self.eval(t, f),
                    None => Ok(Value::Unit),
                }
            }
            Ex::Cast { e, to, span } => {
                let v = self.eval(e, f)?;
                builtins::convert(self, v, *to, *span)
            }
            Ex::AssertEq { l, r, msg, span } => {
                let a = self.eval(l, f)?;
                let b = self.eval(r, f)?;
                let same = value::eq(&a, &b).map_err(|_| panic("E0301", *span, format!("cannot compare {} with {}", self.kind(&a), self.kind(&b))))?;
                if same {
                    return Ok(Value::Bool(true));
                }
                let mut detail = vec![format!("left:  {}", self.repr(&a)), format!("right: {}", self.repr(&b))];
                if let Some((path, x, y)) = value::first_diff(&a, &b, self.prog) {
                    if !path.is_empty() {
                        detail.push(format!("first difference at {path}: {x} vs {y}"));
                    }
                }
                let m = match msg {
                    Some(m) => {
                        let v = self.eval(m, f)?;
                        format!("{} ", self.display(&v))
                    }
                    None => String::new(),
                };
                Err(Ctrl::Panic(Box::new(Panic { code: "E0420", msg: format!("{m}left != right"), span: *span, detail, exit: None, trace: vec![] })))
            }
        }
    }

    pub fn int_of(&self, v: &Value, span: Span) -> R<i64> {
        match v {
            Value::Int(n) => Ok(*n),
            other => Err(panic("E0301", span, format!("want int got {}", self.kind(other)))),
        }
    }

    pub fn binop(&self, op: BinOp, a: Value, b: Value, span: Span) -> R<Value> {
        arith(self.prog, op, a, b).map_err(|(code, msg)| panic(code, span, msg))
    }

    pub fn compare(&self, op: CmpOp, a: &Value, b: &Value, span: Span) -> R<bool> {
        let incomparable = || panic("E0301", span, format!("cannot compare {} with {}", self.kind(a), self.kind(b)));
        for v in [a, b] {
            if let Value::Err(e) = v {
                if !matches!(op, CmpOp::Eq | CmpOp::Ne) || !matches!((a, b), (Value::Err(_), Value::Err(_))) {
                    return Err(panic("E0406", span, format!("comparing an error ({}); add `?`", self.display(e))));
                }
            }
        }
        Ok(match op {
            CmpOp::Eq => value::eq(a, b).map_err(|_| incomparable())?,
            CmpOp::Ne => !value::eq(a, b).map_err(|_| incomparable())?,
            CmpOp::In => contains(self, b, a, span)?,
            CmpOp::NotIn => !contains(self, b, a, span)?,
            _ => {
                if matches!(a, Value::None) || matches!(b, Value::None) {
                    return Err(panic("E0301", span, format!("cannot order none ({} {} {})", self.short(a), op.symbol(), self.short(b))));
                }
                let o = value::cmp(a, b).map_err(|_| incomparable())?;
                match op {
                    CmpOp::Lt => o.is_lt(),
                    CmpOp::Le => o.is_le(),
                    CmpOp::Gt => o.is_gt(),
                    _ => o.is_ge(),
                }
            }
        })
    }

    pub fn index(&self, o: &Value, i: &Value, span: Span) -> R<Value> {
        match (o, i) {
            (Value::List(xs), Value::Int(n)) | (Value::Tuple(xs), Value::Int(n)) => {
                let idx = norm_index(*n, xs.len()).ok_or_else(|| panic("E0402", span, format!("index {n} out of range for length {}", xs.len())))?;
                Ok(xs[idx].clone())
            }
            (Value::Str(s), Value::Int(n)) => {
                let len = if s.is_ascii() { s.len() } else { s.chars().count() };
                let idx = norm_index(*n, len).ok_or_else(|| panic("E0402", span, format!("index {n} out of range for string of length {len}")))?;
                let c = if s.is_ascii() { s[idx..idx + 1].to_string() } else { s.chars().nth(idx).unwrap().to_string() };
                Ok(Value::str(c))
            }
            (Value::Range(r), Value::Int(n)) => {
                let idx = norm_index(*n, r.len()).ok_or_else(|| panic("E0402", span, format!("index {n} out of range for length {}", r.len())))?;
                Ok(Value::Int(r.get(idx)))
            }
            (Value::Map(m), k) => match m.get(k) {
                Some(v) => Ok(v.clone()),
                None => Err(panic("E0403", span, format!("key {} not found; use `m.get(k)` (gives `none`) or `m.get(k) ?? default`", self.short(k)))),
            },
            (Value::Err(e), _) => Err(panic("E0406", span, format!("indexing an error ({}); add `?`", self.display(e)))),
            (Value::None, _) => Err(panic("E0407", span, "indexing none")),
            _ => Err(panic("E0301", span, format!("cannot index {} with {}", self.kind(o), self.kind(i)))),
        }
    }

    /// `v.name`: a struct field, a tuple element, or a zero-argument method.
    pub fn get_field(&self, v: Value, name: &str, user: Option<&[FnId]>, span: Span) -> R<Value> {
        match &v {
            Value::Struct(o) => {
                let def = &self.prog.structs[o.ty as usize];
                if let Some(i) = def.fields.iter().position(|fd| fd.name == name) {
                    return Ok(o.fields[i].clone());
                }
            }
            Value::Tuple(xs) => {
                if let Ok(i) = name.parse::<usize>() {
                    return xs.get(i).cloned().ok_or_else(|| panic("E0402", span, format!("tuple has no element {i} (length {})", xs.len())));
                }
            }
            _ => {}
        }
        if let Some(ids) = user {
            if let Some(fid) = self.pick_opt(ids, Some(&v), 1) {
                return self.call_fid(fid, vec![v], span);
            }
        }
        if name.parse::<usize>().is_ok() {
            return Err(panic("E0301", span, format!("`.{name}` needs a tuple, got {}", self.kind(&v))));
        }
        if builtins::is_method(name) {
            return builtins::method(self, v, name, vec![], span);
        }
        Err(panic("E0203", span, format!("{} has no field `{name}`", self.kind(&v))))
    }

    fn eval_method(&self, recv: &Recv, name: &str, user: Option<&[FnId]>, args: &[Ex], span: Span, f: &Rc<Frame>) -> R<Value> {
        // A user function whose first parameter accepts the receiver wins.
        let mut rv_opt = None;
        if let Some(ids) = user {
            let rv = match recv {
                Recv::Value(e) => self.eval(e, f)?,
                Recv::Place(p) => match self.read_place(p, f) {
                    Ok(v) => v,
                    // `m[k].push(x)` on a missing key: only a builtin mutator applies.
                    Err(_) if is_mutator(name) => return self.mutate_place(recv, name, args, span, f),
                    Err(e) => return Err(e),
                },
            };
            if let Some(fid) = self.pick_opt(ids, Some(&rv), args.len() + 1) {
                return self.call_user_method(fid, rv, recv, args, span, f);
            }
            if !builtins::is_method(name) {
                let fd = &self.prog.fns[ids[0] as usize];
                let want = fd.params.first().map_or(Ty::Unit, |p| p.ty.clone());
                return Err(panic("E0301", span, format!("`{name}` takes {} first, got {}", ty_name(&want, self.prog), self.kind(&rv))));
            }
            rv_opt = Some(rv);
        }
        // Builtin mutators go straight to the place, so a uniquely owned list
        // is changed in place and a missing map entry can be created.
        if is_mutator(name) {
            drop(rv_opt);
            return self.mutate_place(recv, name, args, span, f);
        }
        let rv = match (rv_opt, recv) {
            (Some(v), _) => v,
            (None, Recv::Value(e)) => self.eval(e, f)?,
            (None, Recv::Place(p)) => self.read_place(p, f)?,
        };
        let mut argv = Vec::with_capacity(args.len());
        for a in args {
            argv.push(self.eval(a, f)?);
        }
        builtins::method(self, rv, name, argv, span)
    }

    fn mutate_place(&self, recv: &Recv, name: &str, args: &[Ex], span: Span, f: &Rc<Frame>) -> R<Value> {
        let mut argv = Vec::with_capacity(args.len());
        for a in args {
            argv.push(self.eval(a, f)?);
        }
        let Recv::Place(p) = recv else {
            return Err(panic("E0410", span, format!("`{name}` changes its receiver, so call it on a variable, not a temporary value")));
        };
        if !p.mutable {
            return Err(panic("E0202", span, format!("`{}` is immutable; declare it with `var`", p.name)));
        }
        if name == "retain" {
            let cur = self.read_place(p, f)?;
            let kept = builtins::method(self, cur, "filter", argv, span)?;
            return self.with_place(p, f, Viv::No, |s| {
                *s = kept;
                Ok(Value::Unit)
            });
        }
        self.with_place(p, f, Viv::Method(name), |slot| builtins::mutate(self, slot, name, argv, span))
    }

    /// Calls a user function as a method. A `mut` receiver is moved out of
    /// its place for the call and written back afterwards.
    fn call_user_method(&self, fid: FnId, rv: Value, recv: &Recv, args: &[Ex], span: Span, f: &Rc<Frame>) -> R<Value> {
        let mut argv = Vec::with_capacity(args.len() + 1);
        argv.push(rv);
        for a in args {
            argv.push(self.eval(a, f)?);
        }
        let fd = &self.prog.fns[fid as usize];
        if let (Mode::Mut, Recv::Place(p)) = (fd.params[0].mode, recv) {
            if !p.mutable {
                return Err(panic("E0202", span, format!("`{}` is immutable; declare it with `var`", p.name)));
            }
            argv[0] = Value::Unit;
            argv[0] = self.with_place(p, f, Viv::No, |s| Ok(std::mem::replace(s, Value::Unit)))?;
            let (v, frame) = self.call_frame(fid, argv, span)?;
            let nv = frame.get(0);
            self.with_place(p, f, Viv::No, |s| {
                *s = nv;
                Ok(())
            })?;
            return Ok(v);
        }
        self.call_fid(fid, argv, span)
    }

    fn read_place(&self, p: &Place, f: &Rc<Frame>) -> R<Value> {
        let frame = match p.root {
            Root::Local(_) => f,
            Root::Up(d, _) => f.up(d),
        };
        let (Root::Local(slot) | Root::Up(_, slot)) = p.root;
        let mut v = frame.get(slot);
        for seg in &p.path {
            v = match seg {
                Seg::Field(name, sp) => self.get_field(v, name, None, *sp)?,
                Seg::Index(e, sp) => {
                    let k = self.eval(e, f)?;
                    self.index(&v, &k, *sp)?
                }
            };
        }
        Ok(v)
    }

    /// Runs `op` on the value stored at `p`, creating missing map entries as
    /// `viv` says. No user code runs while the frame is borrowed.
    fn with_place<T>(&self, p: &Place, f: &Rc<Frame>, viv: Viv, op: impl FnOnce(&mut Value) -> R<T>) -> R<T> {
        let mut keys: Vec<Option<Value>> = Vec::with_capacity(p.path.len());
        for seg in &p.path {
            keys.push(match seg {
                Seg::Index(e, _) => Some(self.eval(e, f)?),
                Seg::Field(..) => None,
            });
        }
        let frame = match p.root {
            Root::Local(_) => f,
            Root::Up(d, _) => f.up(d),
        };
        let (Root::Local(slot) | Root::Up(_, slot)) = p.root;
        let mut slots = frame.slots.borrow_mut();
        let mut cur: &mut Value = &mut slots[slot as usize];
        let n = p.path.len();
        for (i, seg) in p.path.iter().enumerate() {
            let last = i + 1 == n;
            cur = match seg {
                Seg::Field(name, sp) => self.field_mut(cur, name, *sp)?,
                Seg::Index(_, sp) => {
                    let key = keys[i].take().unwrap();
                    let create = if last {
                        match viv {
                            Viv::No => None,
                            Viv::Insert => Some(Value::Unit),
                            Viv::ZeroOf(v) => Some(zero_of(v).ok_or_else(|| panic("E0403", *sp, format!("key {} not found", self.short(&key))))?),
                            Viv::Method(m) => Some(if matches!(m, "add") {
                                Value::Set(Rc::new(value::Set::default()))
                            } else {
                                Value::list(vec![])
                            }),
                        }
                    } else if matches!(viv, Viv::No) {
                        None
                    } else {
                        match &p.path[i + 1] {
                            Seg::Index(..) => Some(Value::Map(Rc::new(value::Map::default()))),
                            Seg::Field(..) => None,
                        }
                    };
                    self.index_mut(cur, key, create, *sp)?
                }
            };
        }
        op(cur)
    }

    fn field_mut<'v>(&self, cur: &'v mut Value, name: &str, span: Span) -> R<&'v mut Value> {
        let kind = self.kind(cur);
        match cur {
            Value::Struct(o) => {
                let def = &self.prog.structs[o.ty as usize];
                let Some(i) = def.fields.iter().position(|fd| fd.name == name) else {
                    return Err(panic("E0208", span, format!("{} has no field `{name}`", def.name)));
                };
                Ok(&mut Rc::make_mut(o).fields[i])
            }
            Value::Tuple(xs) => match name.parse::<usize>() {
                Ok(i) if i < xs.len() => Ok(&mut Rc::make_mut(xs)[i]),
                _ => Err(panic("E0402", span, format!("tuple has no element {name}"))),
            },
            _ => Err(panic("E0301", span, format!("cannot set field `{name}` on {kind}"))),
        }
    }

    fn index_mut<'v>(&self, cur: &'v mut Value, key: Value, create: Option<Value>, span: Span) -> R<&'v mut Value> {
        let kind = self.kind(cur);
        match cur {
            Value::List(xs) => {
                let Value::Int(n) = key else {
                    return Err(panic("E0301", span, format!("list index must be int, got {}", self.kind(&key))));
                };
                let len = xs.len();
                let idx = norm_index(n, len).ok_or_else(|| panic("E0402", span, format!("index {n} out of range for length {len}")))?;
                Ok(&mut Rc::make_mut(xs)[idx])
            }
            Value::Map(m) => {
                let m = Rc::make_mut(m);
                if !m.contains_key(&key) {
                    match create {
                        Some(z) => {
                            m.insert(key.clone(), z);
                        }
                        None => return Err(panic("E0403", span, format!("key {} not found", self.short(&key)))),
                    }
                }
                Ok(m.get_mut(&key).unwrap())
            }
            Value::Str(_) => Err(panic("E0410", span, "strings are immutable; build a new string (e.g. with slices and `+`)")),
            _ => Err(panic("E0301", span, format!("cannot index into {kind}"))),
        }
    }

    // ----- calls -----

    pub fn pick_opt(&self, fns: &[FnId], first: Option<&Value>, n: usize) -> Option<FnId> {
        if fns.len() == 1 {
            let fd = &self.prog.fns[fns[0] as usize];
            if fd.params.is_empty() && n > 0 {
                return None;
            }
            if let (Some(v), Some(p)) = (first, fd.params.first()) {
                if !self.ty_matches(v, &p.ty) {
                    return None;
                }
            }
            return Some(fns[0]);
        }
        fns.iter().copied().find(|&id| {
            let fd = &self.prog.fns[id as usize];
            let required = fd.params.iter().filter(|p| p.default.is_none()).count();
            n >= required && n <= fd.params.len() && match (first, fd.params.first()) {
                (Some(v), Some(p)) => self.ty_matches(v, &p.ty),
                (None, None) => true,
                _ => false,
            }
        })
    }

    fn pick(&self, fns: &[FnId], first: Option<&Value>, n: usize, span: Span) -> R<FnId> {
        if fns.len() == 1 {
            return Ok(fns[0]);
        }
        self.pick_opt(fns, first, n).ok_or_else(|| {
            let name = &self.prog.fns[fns[0] as usize].name;
            panic("E0301", span, format!("no `{name}` takes {}", first.map_or("no arguments".to_string(), |v| self.kind(v))))
        })
    }

    pub fn call_fid(&self, fid: FnId, args: Vec<Value>, span: Span) -> R<Value> {
        Ok(self.call_frame(fid, args, span)?.0)
    }

    fn call_frame(&self, fid: FnId, mut args: Vec<Value>, span: Span) -> R<(Value, Rc<Frame>)> {
        let fd = &self.prog.fns[fid as usize];
        let required = fd.params.iter().filter(|p| p.default.is_none()).count();
        if args.len() < required || args.len() > fd.params.len() {
            return Err(panic("E0206", span, format!("`{}` takes {} argument(s), got {}: {}", fd.name, fd.params.len(), args.len(), fd.sig)));
        }
        for p in &fd.params[args.len()..] {
            args.push(self.eval_thunk(p.default.as_ref().unwrap())?);
        }
        let frame = Frame::new(fd.nslots, None);
        {
            let mut slots = frame.slots.borrow_mut();
            for (i, (a, p)) in args.into_iter().zip(fd.params.iter()).enumerate() {
                let a = self.coerce(a, &p.ty).map_err(|got| {
                    panic("E0301", span, format!("argument `{}` of `{}`: want {} got {got}", p.name, fd.name, ty_name(&p.ty, self.prog)))
                })?;
                slots[i] = a;
            }
        }
        let d = self.depth.get();
        if d >= MAX_DEPTH {
            return Err(panic("E0408", span, format!("stack overflow: recursion deeper than {MAX_DEPTH} calls (in `{}`)", fd.name)));
        }
        self.depth.set(d + 1);
        self.stack.borrow_mut().push((fid, span));
        let r = self.eval(&fd.body, &frame);
        let v = match r {
            Ok(v) | Err(Ctrl::Return(v)) => v,
            Err(Ctrl::Break) | Err(Ctrl::Continue) => return Err(panic("E0211", fd.span, "`break`/`continue` outside a loop")),
            Err(p) => return Err(p),
        };
        self.depth.set(d);
        self.stack.borrow_mut().pop();
        let v = match &fd.ret {
            None => v,
            Some(t) => self.coerce_ret(v, t).map_err(|got| {
                if got == "()" {
                    panic(
                        "E0301",
                        fd.span,
                        format!("`{}` must return {} but its body ended without a value; end with an expression or use `return`", fd.name, ty_name(t, self.prog)),
                    )
                } else {
                    panic("E0301", fd.span, format!("`{}` returned {got}, want {}", fd.name, ty_name(t, self.prog)))
                }
            })?,
        };
        Ok((v, frame))
    }

    fn coerce_ret(&self, v: Value, t: &Ty) -> Result<Value, String> {
        match t {
            Ty::Unit => Ok(match v {
                Value::Err(_) => v,
                _ => Value::Unit,
            }),
            Ty::Result(inner) if matches!(**inner, Ty::Unit) => Ok(match v {
                Value::Err(_) => v,
                _ => Value::Unit,
            }),
            _ => self.coerce(v, t),
        }
    }

    pub fn eval_thunk(&self, d: &LambdaDef) -> R<Value> {
        let fr = Frame::new(d.nslots, None);
        self.eval(&d.body, &fr)
    }

    pub fn call_value(&self, fv: &Value, mut args: Vec<Value>, span: Span) -> R<Value> {
        let Value::Func(func) = fv else {
            return Err(panic("E0301", span, format!("cannot call {}", self.kind(fv))));
        };
        match &**func {
            Func::User(ids) => {
                let fid = self.pick(ids, args.first(), args.len(), span)?;
                self.call_fid(fid, args, span)
            }
            Func::Closure(def, env) => {
                let frame = Frame::new(def.nslots, Some(env.clone()));
                let np = def.params.len();
                if np > 1 && args.len() == 1 {
                    // `pairs.map(|k, v| ...)`: spread a tuple over the parameters.
                    if let Value::Tuple(xs) = &args[0] {
                        if xs.len() == np {
                            args = xs.to_vec();
                        }
                    }
                }
                if args.len() != np {
                    return Err(panic("E0206", span, format!("lambda takes {np} argument(s), got {}", args.len())));
                }
                {
                    let mut slots = frame.slots.borrow_mut();
                    for (i, a) in args.into_iter().enumerate() {
                        slots[def.params[i] as usize] = a;
                    }
                }
                let d = self.depth.get();
                if d >= MAX_DEPTH {
                    return Err(panic("E0408", span, "stack overflow"));
                }
                self.depth.set(d + 1);
                let r = self.eval(&def.body, &frame);
                self.depth.set(d);
                r
            }
            Func::Ctor(e, t) => self.make_variant(*e, *t, args, span),
            Func::Method(name) => {
                if args.is_empty() {
                    return Err(panic("E0206", span, format!("`{name}` needs a receiver")));
                }
                let recv = args.remove(0);
                if let Some(to) = crate::resolve_conv(name) {
                    return builtins::convert(self, recv, to, span);
                }
                builtins::method(self, recv, name, args, span)
            }
        }
    }

    fn make_variant(&self, id: u32, tag: u32, vals: Vec<Value>, span: Span) -> R<Value> {
        let vd = &self.prog.enums[id as usize].variants[tag as usize];
        if vd.fields.len() != vals.len() {
            return Err(panic("E0206", span, format!("`{}` takes {} field(s), got {}", vd.name, vd.fields.len(), vals.len())));
        }
        let mut out = Vec::with_capacity(vals.len());
        for (v, t) in vals.into_iter().zip(vd.fields.iter()) {
            out.push(self.coerce(v, t).map_err(|got| panic("E0301", span, format!("`{}` field: want {} got {got}", vd.name, ty_name(t, self.prog))))?);
        }
        Ok(Value::Variant(Rc::new(Obj { ty: id, tag, fields: out })))
    }

    // ----- declared types -----

    pub fn ty_matches(&self, v: &Value, t: &Ty) -> bool {
        match (t, v) {
            (Ty::Any, _) => true,
            (Ty::Int { .. }, Value::Int(_)) => true,
            (Ty::Float, Value::Float(_) | Value::Int(_)) => true,
            (Ty::Bool, Value::Bool(_)) | (Ty::Str, Value::Str(_)) | (Ty::Unit, Value::Unit) => true,
            (Ty::List(_), Value::List(_) | Value::Range(_)) => true,
            (Ty::Map(..), Value::Map(_)) | (Ty::Set(_), Value::Set(_)) | (Ty::Heap(_), Value::Heap(_)) => true,
            (Ty::Tuple(ts), Value::Tuple(xs)) => ts.len() == xs.len(),
            (Ty::Optional(_), Value::None) => true,
            (Ty::Optional(t), v) => self.ty_matches(v, t),
            (Ty::Result(_), Value::Err(_)) => true,
            (Ty::Result(t), v) => self.ty_matches(v, t),
            (Ty::Fn, Value::Func(_)) => true,
            (Ty::Struct(id), Value::Struct(o)) => o.ty == *id,
            (Ty::Enum(id), Value::Variant(o)) => o.ty == *id,
            _ => false,
        }
    }

    /// Checks a value against a declared type, converting where the static
    /// language would infer the literal's type (`3` where `f64` is wanted).
    /// On mismatch returns a description of what was found.
    pub fn coerce(&self, v: Value, t: &Ty) -> Result<Value, String> {
        match (t, &v) {
            (Ty::Any, _) => Ok(v),
            (Ty::Int { min, max, name }, Value::Int(n)) => {
                if n < min || n > max {
                    Err(format!("{n} (out of range for {name})"))
                } else {
                    Ok(v)
                }
            }
            (Ty::Float, Value::Int(n)) => Ok(Value::Float(*n as f64)),
            (Ty::List(inner), Value::List(xs)) if matches!(**inner, Ty::Float) && matches!(xs.first(), Some(Value::Int(_))) => {
                Ok(Value::list(xs.iter().map(|x| if let Value::Int(n) = x { Value::Float(*n as f64) } else { x.clone() }).collect()))
            }
            (Ty::List(_), Value::Range(_)) => Ok(Value::list(builtins::iter_values(&v).unwrap_or_default())),
            (Ty::Optional(_), Value::None) => Ok(v),
            (Ty::Optional(inner), _) => self.coerce(v, inner),
            (Ty::Result(_), Value::Err(_)) => Ok(v),
            (Ty::Result(inner), _) => self.coerce(v, inner),
            (_, Value::Err(e)) => Err(format!("an error ({}); add `?`", self.display(e))),
            _ => {
                if self.ty_matches(&v, t) {
                    Ok(v)
                } else {
                    Err(self.kind(&v))
                }
            }
        }
    }

    // ----- patterns -----

    pub fn match_pat(&self, p: &PatIr, v: &Value, f: &Rc<Frame>) -> bool {
        match p {
            PatIr::Wild => true,
            PatIr::Bind(slot) => {
                f.set(*slot, v.clone());
                true
            }
            PatIr::Lit(l) => value::eq(l, v).unwrap_or(false),
            PatIr::Range { lo, hi, inclusive } => {
                if matches!(v, Value::None | Value::Err(_)) {
                    return false;
                }
                let above = lo.as_ref().map_or(true, |l| value::cmp(v, l).is_ok_and(|o| o.is_ge()));
                let below = hi.as_ref().map_or(true, |h| value::cmp(v, h).is_ok_and(|o| if *inclusive { o.is_le() } else { o.is_lt() }));
                above && below
            }
            PatIr::Tuple(ps) => match v {
                Value::Tuple(xs) | Value::List(xs) => xs.len() == ps.len() && ps.iter().zip(xs.iter()).all(|(p, x)| self.match_pat(p, x, f)),
                _ => false,
            },
            PatIr::List { items, rest } => {
                let xs = match v {
                    Value::List(xs) | Value::Tuple(xs) => xs.clone(),
                    _ => return false,
                };
                let ok_len = if rest.is_some() { xs.len() >= items.len() } else { xs.len() == items.len() };
                if !ok_len || !items.iter().zip(xs.iter()).all(|(p, x)| self.match_pat(p, x, f)) {
                    return false;
                }
                if let Some(Some(slot)) = rest {
                    f.set(*slot, Value::list(xs[items.len()..].to_vec()));
                }
                true
            }
            PatIr::Variant { id, tag, args } => match v {
                Value::Variant(o) => o.ty == *id && o.tag == *tag && args.iter().zip(o.fields.iter()).all(|(p, x)| self.match_pat(p, x, f)),
                _ => false,
            },
            PatIr::Struct { id, fields } => match v {
                Value::Struct(o) => o.ty == *id && fields.iter().all(|(i, p)| self.match_pat(p, &o.fields[*i], f)),
                _ => false,
            },
            PatIr::Or(ps) => ps.iter().any(|p| self.match_pat(p, v, f)),
            PatIr::None => matches!(v, Value::None),
            PatIr::Err(p) => match v {
                Value::Err(e) => self.match_pat(p, e, f),
                _ => false,
            },
            PatIr::Ok(p) => !matches!(v, Value::Err(_)) && self.match_pat(p, v, f),
        }
    }

    // ----- formatting -----

    fn format_spec(&self, v: &Value, spec: &FmtSpec, span: Span) -> R<String> {
        let numeric = matches!(v, Value::Int(_) | Value::Float(_));
        let body = match (v, spec.kind) {
            (Value::Int(n), Some('x')) => format!("{n:x}"),
            (Value::Int(n), Some('X')) => format!("{n:X}"),
            (Value::Int(n), Some('b')) => format!("{n:b}"),
            (Value::Int(n), Some('o')) => format!("{n:o}"),
            (Value::Int(n), Some('e')) => format!("{:.*e}", spec.precision.unwrap_or(6), *n as f64),
            (Value::Float(x), Some('e')) => format!("{:.*e}", spec.precision.unwrap_or(6), x),
            (Value::Int(n), Some('f')) => format!("{:.*}", spec.precision.unwrap_or(6), *n as f64),
            (Value::Int(n), _) if spec.precision.is_some() => format!("{:.*}", spec.precision.unwrap(), *n as f64),
            (Value::Float(x), Some('f')) => format!("{:.*}", spec.precision.unwrap_or(6), x),
            (Value::Float(x), _) if spec.precision.is_some() => format!("{:.*}", spec.precision.unwrap(), x),
            (Value::Float(x), Some('d')) => format!("{}", x.round() as i64),
            (_, Some(k)) if matches!(k, 'x' | 'X' | 'b' | 'o' | 'e' | 'f' | 'd') && !numeric => {
                return Err(panic("E0301", span, format!("format `{k}` needs a number, got {}", self.kind(v))));
            }
            (Value::Str(s), _) if spec.precision.is_some() => s.chars().take(spec.precision.unwrap()).collect(),
            _ => self.display(v),
        };
        let body = if spec.plus && numeric && !body.starts_with('-') { format!("+{body}") } else { body };
        Ok(spec.pad(body, numeric))
    }

    pub fn print(&self, s: &str) {
        self.out.borrow_mut().write_str(s);
    }
}

pub fn norm_index(n: i64, len: usize) -> Option<usize> {
    let i = if n < 0 { len as i64 + n } else { n };
    if i < 0 || i >= len as i64 {
        None
    } else {
        Some(i as usize)
    }
}

fn zero_of(v: &Value) -> Option<Value> {
    Some(match v {
        Value::Int(_) => Value::Int(0),
        Value::Float(_) => Value::Float(0.0),
        Value::Str(_) => Value::str(""),
        Value::List(_) => Value::list(vec![]),
        Value::Set(_) => Value::Set(Rc::new(value::Set::default())),
        Value::Map(_) => Value::Map(Rc::new(value::Map::default())),
        _ => return None,
    })
}

/// Best-effort span of an expression, for errors about a sub-expression.
pub fn cond_span(e: &Ex) -> Span {
    match e {
        Ex::Struct { span, .. }
        | Ex::Variant { span, .. }
        | Ex::Field { span, .. }
        | Ex::Index { span, .. }
        | Ex::Slice { span, .. }
        | Ex::CallFn { span, .. }
        | Ex::CallValue { span, .. }
        | Ex::CallBuiltin { span, .. }
        | Ex::Method { span, .. }
        | Ex::Unary { span, .. }
        | Ex::Binary { span, .. }
        | Ex::And(_, _, span)
        | Ex::Or(_, _, span)
        | Ex::Range { span, .. }
        | Ex::Try { span, .. }
        | Ex::If { span, .. }
        | Ex::Match { span, .. }
        | Ex::Cast { span, .. }
        | Ex::AssertEq { span, .. } => *span,
        Ex::Compare { rest, .. } => rest.first().map_or(Span::default(), |r| r.2),
        Ex::Lambda(d) => d.span,
        _ => Span::default(),
    }
}

fn cond_span_or(e: &Ex, fallback: Span) -> Span {
    let s = cond_span(e);
    if s == Span::default() {
        fallback
    } else {
        s
    }
}

pub fn ty_name(t: &Ty, p: &Program) -> String {
    match t {
        Ty::Any => "any".into(),
        Ty::Int { name, .. } => (*name).into(),
        Ty::Float => "f64".into(),
        Ty::Bool => "bool".into(),
        Ty::Str => "str".into(),
        Ty::Unit => "()".into(),
        Ty::List(t) => format!("[{}]", ty_name(t, p)),
        Ty::Map(k, v) => format!("{{{}:{}}}", ty_name(k, p), ty_name(v, p)),
        Ty::Set(t) => format!("{{{}}}", ty_name(t, p)),
        Ty::Heap(t) => format!("heap[{}]", ty_name(t, p)),
        Ty::Tuple(ts) => format!("({})", ts.iter().map(|t| ty_name(t, p)).collect::<Vec<_>>().join(", ")),
        Ty::Optional(t) => format!("{}?", ty_name(t, p)),
        Ty::Result(t) => format!("{}!", ty_name(t, p)),
        Ty::Fn => "fn".into(),
        Ty::Struct(id) => p.structs[*id as usize].name.clone(),
        Ty::Enum(id) => p.enums[*id as usize].name.clone(),
    }
}
