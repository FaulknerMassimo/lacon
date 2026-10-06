//! Inference types, the substitution that binds type variables, and the
//! three ways two types meet: `unify` (must be equal), `coerce` (a value of
//! one type flows into a place of the other) and `join` (two branches of an
//! `if`, a `match` or `??` become one value).

use std::rc::Rc;

use lacon_interp::ir::{Program, Ty};

#[derive(Clone, Debug)]
pub enum T {
    /// All integer types share one representation (64-bit); the name is the
    /// declared one, for messages.
    Int(&'static str),
    Float,
    Bool,
    Str,
    Unit,
    /// The type of an expression that never produces a value (`panic`, a
    /// block that always returns).
    Never,
    /// Not checked: error recovery, `any`, and what the checker does not model.
    Unknown,
    Range,
    List(Box<T>),
    Map(Box<T>, Box<T>),
    Set(Box<T>),
    Heap(Box<T>),
    Tuple(Vec<T>),
    Opt(Box<T>),
    Res(Box<T>),
    Fn(Vec<T>, Box<T>),
    Struct(u32, Vec<T>),
    Enum(u32, Vec<T>),
    /// A generic parameter inside the generic function's own body.
    Param(Rc<str>),
    /// Branches of different types (`f() ?? "error"`, `[name] + counts`):
    /// fine to print or interpolate, nothing else.
    Mixed(Vec<T>),
    Var(u32),
}

pub const INT: T = T::Int("int");

impl T {
    pub fn list(t: T) -> T {
        T::List(Box::new(t))
    }
    pub fn opt(t: T) -> T {
        T::Opt(Box::new(t))
    }
    pub fn res(t: T) -> T {
        T::Res(Box::new(t))
    }
    pub fn set(t: T) -> T {
        T::Set(Box::new(t))
    }
    pub fn map(k: T, v: T) -> T {
        T::Map(Box::new(k), Box::new(v))
    }
    pub fn func(ps: Vec<T>, r: T) -> T {
        T::Fn(ps, Box::new(r))
    }
}

#[derive(Clone, Default)]
pub struct Subst {
    vars: Vec<Option<T>>,
}

impl Subst {
    pub fn fresh(&mut self) -> T {
        self.vars.push(None);
        T::Var(self.vars.len() as u32 - 1)
    }

    pub fn snapshot(&self) -> Subst {
        self.clone()
    }

    /// Follows variable bindings at the top level only.
    pub fn resolve(&self, t: &T) -> T {
        let mut t = t.clone();
        while let T::Var(v) = t {
            match &self.vars[v as usize] {
                Some(b) => t = b.clone(),
                None => return T::Var(v),
            }
        }
        t
    }

    pub fn is_var(&self, t: &T) -> bool {
        matches!(self.resolve(t), T::Var(_))
    }

    /// Resolves variables everywhere inside a type.
    pub fn zonk(&self, t: &T) -> T {
        match self.resolve(t) {
            T::List(x) => T::list(self.zonk(&x)),
            T::Set(x) => T::set(self.zonk(&x)),
            T::Heap(x) => T::Heap(Box::new(self.zonk(&x))),
            T::Opt(x) => T::opt(self.zonk(&x)),
            T::Res(x) => T::res(self.zonk(&x)),
            T::Map(k, v) => T::map(self.zonk(&k), self.zonk(&v)),
            T::Tuple(ts) => T::Tuple(ts.iter().map(|t| self.zonk(t)).collect()),
            T::Fn(ps, r) => T::func(ps.iter().map(|t| self.zonk(t)).collect(), self.zonk(&r)),
            T::Struct(i, ts) => T::Struct(i, ts.iter().map(|t| self.zonk(t)).collect()),
            T::Enum(i, ts) => T::Enum(i, ts.iter().map(|t| self.zonk(t)).collect()),
            T::Mixed(ts) => T::Mixed(ts.iter().map(|t| self.zonk(t)).collect()),
            t => t,
        }
    }

    fn occurs(&self, v: u32, t: &T) -> bool {
        match self.resolve(t) {
            T::Var(w) => v == w,
            T::List(x) | T::Set(x) | T::Heap(x) | T::Opt(x) | T::Res(x) => self.occurs(v, &x),
            T::Map(k, x) => self.occurs(v, &k) || self.occurs(v, &x),
            T::Tuple(ts) | T::Struct(_, ts) | T::Enum(_, ts) | T::Mixed(ts) => ts.iter().any(|t| self.occurs(v, t)),
            T::Fn(ps, r) => ps.iter().any(|t| self.occurs(v, t)) || self.occurs(v, &r),
            _ => false,
        }
    }

    fn bind(&mut self, v: u32, t: &T) -> bool {
        if matches!(t, T::Unknown | T::Never) {
            return true;
        }
        if self.occurs(v, t) {
            return false;
        }
        self.vars[v as usize] = Some(t.clone());
        true
    }

    /// Makes two types equal, binding variables. On failure some variables
    /// may already be bound; the caller reports the error and moves on.
    pub fn unify(&mut self, a: &T, b: &T) -> bool {
        let (a, b) = (self.resolve(a), self.resolve(b));
        match (&a, &b) {
            (T::Var(x), T::Var(y)) if x == y => true,
            (T::Var(x), t) | (t, T::Var(x)) => self.bind(*x, t),
            (T::Unknown, _) | (_, T::Unknown) | (T::Never, _) | (_, T::Never) => true,
            (T::Int(_), T::Int(_)) | (T::Float, T::Float) | (T::Bool, T::Bool) | (T::Str, T::Str) | (T::Unit, T::Unit) | (T::Range, T::Range) => true,
            (T::List(x), T::List(y)) | (T::Set(x), T::Set(y)) | (T::Heap(x), T::Heap(y)) | (T::Opt(x), T::Opt(y)) | (T::Res(x), T::Res(y)) => {
                self.unify(x, y)
            }
            (T::Map(k1, v1), T::Map(k2, v2)) => self.unify(k1, k2) && self.unify(v1, v2),
            (T::Tuple(xs), T::Tuple(ys)) => xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| self.unify(x, y)),
            (T::Fn(ps, r), T::Fn(qs, s)) => ps.len() == qs.len() && ps.iter().zip(qs).all(|(p, q)| self.unify(p, q)) && self.unify(r, s),
            (T::Struct(i, xs), T::Struct(j, ys)) | (T::Enum(i, xs), T::Enum(j, ys)) => {
                i == j && (xs.len() != ys.len() || xs.iter().zip(ys).all(|(x, y)| self.unify(x, y)))
            }
            (T::Param(x), T::Param(y)) => x == y,
            (T::Mixed(_), T::Mixed(_)) => true,
            _ => false,
        }
    }

    /// Would `unify` succeed? Binds nothing; variables match anything.
    pub fn could_unify(&self, a: &T, b: &T) -> bool {
        let (a, b) = (self.resolve(a), self.resolve(b));
        match (&a, &b) {
            (T::Var(_), _) | (_, T::Var(_)) | (T::Unknown, _) | (_, T::Unknown) | (T::Never, _) | (_, T::Never) => true,
            (T::Int(_), T::Int(_)) | (T::Float, T::Float) | (T::Bool, T::Bool) | (T::Str, T::Str) | (T::Unit, T::Unit) | (T::Range, T::Range) => true,
            (T::List(x), T::List(y)) | (T::Set(x), T::Set(y)) | (T::Heap(x), T::Heap(y)) | (T::Opt(x), T::Opt(y)) | (T::Res(x), T::Res(y)) => {
                self.could_unify(x, y)
            }
            (T::Map(k1, v1), T::Map(k2, v2)) => self.could_unify(k1, k2) && self.could_unify(v1, v2),
            (T::Tuple(xs), T::Tuple(ys)) => xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| self.could_unify(x, y)),
            (T::Fn(ps, r), T::Fn(qs, s)) => ps.len() == qs.len() && ps.iter().zip(qs).all(|(p, q)| self.could_unify(p, q)) && self.could_unify(r, s),
            (T::Struct(i, xs), T::Struct(j, ys)) | (T::Enum(i, xs), T::Enum(j, ys)) => {
                i == j && (xs.len() != ys.len() || xs.iter().zip(ys).all(|(x, y)| self.could_unify(x, y)))
            }
            (T::Param(x), T::Param(y)) => x == y,
            (T::Mixed(_), T::Mixed(_)) => true,
            _ => false,
        }
    }

    /// A value of type `from` flows into a place of type `to`. On top of
    /// `unify`: an int becomes a float, a value becomes an optional or a
    /// result, a range becomes a list, and lists and tuples of ints become
    /// lists and tuples of floats.
    pub fn coerce(&mut self, from: &T, to: &T) -> bool {
        let (f, t) = (self.resolve(from), self.resolve(to));
        match (&f, &t) {
            (_, T::Unknown) | (T::Unknown, _) | (T::Never, _) => true,
            (T::Var(_), T::Opt(inner) | T::Res(inner)) => self.unify(&f, inner),
            (T::Var(_), _) | (_, T::Var(_)) => self.unify(&f, &t),
            (T::Int(_), T::Float) => true,
            (T::Opt(a), T::Opt(b)) | (T::Res(a), T::Res(b)) => self.coerce(a, b),
            (T::Opt(_), T::Res(_)) => false,
            (_, T::Opt(b)) | (_, T::Res(b)) => self.coerce(&f, b),
            (T::Range, T::List(e)) => self.coerce(&INT, e),
            (T::List(a), T::List(b)) => {
                if matches!(self.resolve(a), T::Int(_)) && matches!(self.resolve(b), T::Float) {
                    true
                } else {
                    self.unify(a, b)
                }
            }
            (T::Tuple(xs), T::Tuple(ys)) => xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| self.coerce(x, y)),
            (T::Fn(ps, r), T::Fn(qs, s)) => ps.len() == qs.len() && ps.iter().zip(qs).all(|(p, q)| self.unify(p, q)) && self.coerce(r, s),
            (T::Mixed(_), _) => matches!(t, T::Mixed(_)),
            _ => self.unify(&f, &t),
        }
    }

    /// Would `coerce` succeed? Binds nothing.
    pub fn could_coerce(&self, from: &T, to: &T) -> bool {
        let (f, t) = (self.resolve(from), self.resolve(to));
        match (&f, &t) {
            (_, T::Unknown) | (T::Unknown, _) | (T::Never, _) | (T::Var(_), _) | (_, T::Var(_)) => true,
            (T::Int(_), T::Float) => true,
            (T::Opt(a), T::Opt(b)) | (T::Res(a), T::Res(b)) => self.could_coerce(a, b),
            (T::Opt(_), T::Res(_)) => false,
            (_, T::Opt(b)) | (_, T::Res(b)) => self.could_coerce(&f, b),
            (T::Range, T::List(e)) => self.could_coerce(&INT, e),
            (T::List(a), T::List(b)) => {
                (matches!(self.resolve(a), T::Int(_)) && matches!(self.resolve(b), T::Float)) || self.could_unify(a, b)
            }
            (T::Tuple(xs), T::Tuple(ys)) => xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| self.could_coerce(x, y)),
            (T::Mixed(_), _) => matches!(t, T::Mixed(_)),
            _ => self.could_unify(&f, &t),
        }
    }

    /// The type of a value that is one of two branches. Never fails: types
    /// that don't meet become `Mixed`, which only display accepts.
    pub fn join(&mut self, a: &T, b: &T) -> T {
        let (a, b) = (self.resolve(a), self.resolve(b));
        match (&a, &b) {
            (T::Never, _) => return b,
            (_, T::Never) => return a,
            (T::Unknown, _) | (_, T::Unknown) => return T::Unknown,
            _ => {}
        }
        if self.could_unify(&a, &b) {
            self.unify(&a, &b);
            return a;
        }
        match (&a, &b) {
            (T::Int(_), T::Float) | (T::Float, T::Int(_)) => T::Float,
            (T::Opt(x), T::Opt(y)) => T::opt(self.join(x, y)),
            (T::Opt(x), _) => T::opt(self.join(x, &b)),
            (_, T::Opt(y)) => T::opt(self.join(&a, y)),
            (T::Res(x), T::Res(y)) => T::res(self.join(x, y)),
            (T::Res(x), _) => T::res(self.join(x, &b)),
            (_, T::Res(y)) => T::res(self.join(&a, y)),
            (T::Range, T::List(e)) | (T::List(e), T::Range) => T::list(self.join(e, &INT)),
            (T::List(x), T::List(y)) => T::list(self.join(x, y)),
            (T::Tuple(xs), T::Tuple(ys)) if xs.len() == ys.len() => T::Tuple(xs.iter().zip(ys).map(|(x, y)| self.join(x, y)).collect()),
            _ => {
                let mut parts = Vec::new();
                for t in [a, b] {
                    match t {
                        T::Mixed(ts) => parts.extend(ts),
                        t => parts.push(t),
                    }
                }
                let mut out: Vec<T> = Vec::new();
                for p in parts {
                    if !out.iter().any(|o| self.could_unify(o, &p)) {
                        out.push(p);
                    }
                }
                T::Mixed(out)
            }
        }
    }

    /// Converts a declared type, replacing generic parameters per `subst`.
    pub fn from_ir(&mut self, t: &Ty, subst: &[(Rc<str>, T)]) -> T {
        match t {
            Ty::Any => T::Unknown,
            Ty::Param(n) => subst.iter().find(|(name, _)| name == n).map_or_else(|| T::Param(n.clone()), |(_, t)| t.clone()),
            Ty::Int { name, .. } => T::Int(name),
            Ty::Float => T::Float,
            Ty::Bool => T::Bool,
            Ty::Str => T::Str,
            Ty::Unit => T::Unit,
            Ty::List(x) => T::list(self.from_ir(x, subst)),
            Ty::Map(k, v) => T::map(self.from_ir(k, subst), self.from_ir(v, subst)),
            Ty::Set(x) => T::set(self.from_ir(x, subst)),
            Ty::Heap(x) => T::Heap(Box::new(self.from_ir(x, subst))),
            Ty::Tuple(ts) => T::Tuple(ts.iter().map(|t| self.from_ir(t, subst)).collect()),
            Ty::Optional(x) => T::opt(self.from_ir(x, subst)),
            Ty::Result(x) => T::res(self.from_ir(x, subst)),
            Ty::Fn(ps, r) => T::func(ps.iter().map(|t| self.from_ir(t, subst)).collect(), self.from_ir(r, subst)),
            Ty::Struct(id, args) => T::Struct(*id, args.iter().map(|t| self.from_ir(t, subst)).collect()),
            Ty::Enum(id, args) => T::Enum(*id, args.iter().map(|t| self.from_ir(t, subst)).collect()),
        }
    }

    /// The type as Lacon writes it, for messages. Unknown parts show as `_`.
    pub fn show(&self, t: &T, p: &Program) -> String {
        let list = |ts: &[T], sep: &str| ts.iter().map(|t| self.show(t, p)).collect::<Vec<_>>().join(sep);
        match self.resolve(t) {
            T::Int(n) => n.to_string(),
            T::Float => "f64".into(),
            T::Bool => "bool".into(),
            T::Str => "str".into(),
            T::Unit => "()".into(),
            T::Never => "never".into(),
            T::Unknown | T::Var(_) => "_".into(),
            T::Range => "range".into(),
            T::List(x) => format!("[{}]", self.show(&x, p)),
            T::Map(k, v) => format!("{{{}:{}}}", self.show(&k, p), self.show(&v, p)),
            T::Set(x) => format!("{{{}}}", self.show(&x, p)),
            T::Heap(x) => format!("heap[{}]", self.show(&x, p)),
            T::Tuple(ts) => format!("({})", list(&ts, ", ")),
            T::Opt(x) => format!("{}?", self.show(&x, p)),
            T::Res(x) => match self.resolve(&x) {
                T::Unit => "()!".into(),
                _ => format!("{}!", self.show(&x, p)),
            },
            T::Fn(ps, r) => match self.resolve(&r) {
                T::Unit => format!("fn({})", list(&ps, ", ")),
                _ => format!("fn({}) {}", list(&ps, ", "), self.show(&r, p)),
            },
            T::Struct(id, args) | T::Enum(id, args) => {
                let name = if matches!(self.resolve(t), T::Struct(..)) { &p.structs[id as usize].name } else { &p.enums[id as usize].name };
                if args.is_empty() {
                    name.clone()
                } else {
                    format!("{name}[{}]", list(&args, ", "))
                }
            }
            T::Param(n) => n.to_string(),
            T::Mixed(ts) => list(&ts, " or "),
        }
    }
}
