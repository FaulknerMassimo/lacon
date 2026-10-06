//! Runtime values. Heap values are reference counted and copied on write
//! (`Rc::make_mut`), which gives value semantics: assigning a list shares it,
//! and the first mutation through a shared reference copies it. A value with
//! a single owner is mutated in place.

use std::cmp::Ordering;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use indexmap::{IndexMap, IndexSet};

use crate::ir::{FnId, LambdaDef, Program};
use crate::frame::Frame;

pub type Map = IndexMap<Value, Value>;
pub type Set = IndexSet<Value>;

#[derive(Clone)]
pub enum Value {
    Unit,
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Rc<String>),
    List(Rc<Vec<Value>>),
    Tuple(Rc<Vec<Value>>),
    Map(Rc<Map>),
    Set(Rc<Set>),
    /// A binary min-heap, ordered by `cmp`.
    Heap(Rc<Vec<Value>>),
    Struct(Rc<Obj>),
    Variant(Rc<Obj>),
    Range(Rc<RangeV>),
    Err(Rc<Value>),
    Func(Rc<Func>),
}

/// A struct (`ty` indexes `Program::structs`) or an enum variant (`ty`
/// indexes `Program::enums`, `tag` the variant).
#[derive(Clone)]
pub struct Obj {
    pub ty: u32,
    pub tag: u32,
    pub fields: Vec<Value>,
}

/// Half-open integer range `start..end` stepping by `step`.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct RangeV {
    pub start: i64,
    pub end: i64,
    pub step: i64,
}

impl RangeV {
    pub fn len(&self) -> usize {
        if self.step > 0 {
            if self.end <= self.start {
                0
            } else {
                ((self.end - self.start - 1) / self.step + 1) as usize
            }
        } else if self.end >= self.start {
            0
        } else {
            ((self.start - self.end - 1) / (-self.step) + 1) as usize
        }
    }

    pub fn get(&self, i: usize) -> i64 {
        self.start + self.step * i as i64
    }

    pub fn contains(&self, n: i64) -> bool {
        if self.step > 0 {
            n >= self.start && n < self.end && (n - self.start) % self.step == 0
        } else {
            n <= self.start && n > self.end && (self.start - n) % (-self.step) == 0
        }
    }
}

pub enum Func {
    /// A named function, possibly overloaded on its first parameter's type.
    User(Rc<[FnId]>),
    Closure(Rc<LambdaDef>, Rc<Frame>),
    /// An enum variant constructor used as a function: `xs.map(Circle)`.
    Ctor(u32, u32),
    /// A builtin method used as a function value: `xs.map(str)`.
    Method(Rc<str>),
}

impl Value {
    pub fn str(s: impl Into<String>) -> Value {
        Value::Str(Rc::new(s.into()))
    }

    pub fn list(v: Vec<Value>) -> Value {
        Value::List(Rc::new(v))
    }

    pub fn tuple(v: Vec<Value>) -> Value {
        Value::Tuple(Rc::new(v))
    }

    pub fn is_unit(&self) -> bool {
        matches!(self, Value::Unit)
    }

    /// Name of the value's kind, as shown in error messages.
    pub fn kind(&self, p: &Program) -> String {
        match self {
            Value::Unit => "()".into(),
            Value::None => "none".into(),
            Value::Bool(_) => "bool".into(),
            Value::Int(_) => "int".into(),
            Value::Float(_) => "f64".into(),
            Value::Str(_) => "str".into(),
            Value::List(_) => "list".into(),
            Value::Tuple(t) => format!("{}-tuple", t.len()),
            Value::Map(_) => "map".into(),
            Value::Set(_) => "set".into(),
            Value::Heap(_) => "heap".into(),
            Value::Struct(o) => p.structs[o.ty as usize].name.clone(),
            Value::Variant(o) => p.enums[o.ty as usize].name.clone(),
            Value::Range(_) => "range".into(),
            Value::Err(_) => "error".into(),
            Value::Func(_) => "fn".into(),
        }
    }
}

// ----- equality, ordering and hashing -----

/// Language-level `==`. Ints and floats compare numerically; comparing values
/// of unrelated kinds is an error rather than silently false.
pub fn eq(a: &Value, b: &Value) -> Result<bool, ()> {
    use Value::*;
    Ok(match (a, b) {
        (None, None) | (Unit, Unit) => true,
        (None, _) | (_, None) => false,
        (Err(x), Err(y)) => eq(x, y)?,
        (Err(_), _) | (_, Err(_)) => false,
        (Bool(x), Bool(y)) => x == y,
        (Int(x), Int(y)) => x == y,
        (Float(x), Float(y)) => x == y,
        (Int(x), Float(y)) | (Float(y), Int(x)) => (*x as f64) == *y,
        (Str(x), Str(y)) => x == y,
        (List(x), List(y)) | (Tuple(x), Tuple(y)) => {
            if Rc::ptr_eq(x, y) {
                return Ok(true);
            }
            if x.len() != y.len() {
                return Ok(false);
            }
            for (p, q) in x.iter().zip(y.iter()) {
                if !eq(p, q)? {
                    return Ok(false);
                }
            }
            true
        }
        (Map(x), Map(y)) => {
            if x.len() != y.len() {
                return Ok(false);
            }
            for (k, v) in x.iter() {
                match y.get(k) {
                    Some(w) => {
                        if !eq(v, w)? {
                            return Ok(false);
                        }
                    }
                    Option::None => return Ok(false),
                }
            }
            true
        }
        (Set(x), Set(y)) => x.len() == y.len() && x.iter().all(|k| y.contains(k)),
        (Heap(x), Heap(y)) => {
            let (mut x, mut y) = (x.to_vec(), y.to_vec());
            x.sort_by(|a, b| cmp(a, b).unwrap_or(Ordering::Equal));
            y.sort_by(|a, b| cmp(a, b).unwrap_or(Ordering::Equal));
            return eq(&Value::list(x), &Value::list(y));
        }
        (Struct(x), Struct(y)) | (Variant(x), Variant(y)) => {
            if x.ty != y.ty || x.tag != y.tag {
                return Ok(false);
            }
            for (p, q) in x.fields.iter().zip(y.fields.iter()) {
                if !eq(p, q)? {
                    return Ok(false);
                }
            }
            true
        }
        (Range(x), Range(y)) => x == y,
        (Func(x), Func(y)) => Rc::ptr_eq(x, y),
        (List(_), Range(_)) | (Range(_), List(_)) => {
            let xs = crate::builtins::iter_values(a).map_err(|_| ())?;
            let ys = crate::builtins::iter_values(b).map_err(|_| ())?;
            return eq(&Value::list(xs), &Value::list(ys));
        }
        _ => return Result::Err(()),
    })
}

/// Ordering for `<`, sorting, `min` and `max`.
pub fn cmp(a: &Value, b: &Value) -> Result<Ordering, ()> {
    use Value::*;
    Ok(match (a, b) {
        (Int(x), Int(y)) => x.cmp(y),
        (Float(x), Float(y)) => x.partial_cmp(y).ok_or(())?,
        (Int(x), Float(y)) => (*x as f64).partial_cmp(y).ok_or(())?,
        (Float(x), Int(y)) => x.partial_cmp(&(*y as f64)).ok_or(())?,
        (Str(x), Str(y)) => x.cmp(y),
        (Bool(x), Bool(y)) => x.cmp(y),
        (Unit, Unit) => Ordering::Equal,
        (List(x), List(y)) | (Tuple(x), Tuple(y)) => {
            for (p, q) in x.iter().zip(y.iter()) {
                let o = cmp(p, q)?;
                if o != Ordering::Equal {
                    return Ok(o);
                }
            }
            x.len().cmp(&y.len())
        }
        (Struct(x), Struct(y)) | (Variant(x), Variant(y)) if x.ty == y.ty => {
            let o = x.tag.cmp(&y.tag);
            if o != Ordering::Equal {
                return Ok(o);
            }
            for (p, q) in x.fields.iter().zip(y.fields.iter()) {
                let o = cmp(p, q)?;
                if o != Ordering::Equal {
                    return Ok(o);
                }
            }
            Ordering::Equal
        }
        (None, None) => Ordering::Equal,
        (None, _) => Ordering::Less,
        (_, None) => Ordering::Greater,
        _ => return Result::Err(()),
    })
}

// Map keys use structural equality with floats compared by bit pattern, so
// that `Eq` and `Hash` agree.
impl PartialEq for Value {
    fn eq(&self, other: &Value) -> bool {
        use Value::*;
        match (self, other) {
            (Float(x), Float(y)) => norm(*x).to_bits() == norm(*y).to_bits(),
            (Func(x), Func(y)) => Rc::ptr_eq(x, y),
            (Map(x), Map(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k) == Some(v)),
            (Set(x), Set(y)) => x.len() == y.len() && x.iter().all(|k| y.contains(k)),
            (List(x), List(y)) | (Tuple(x), Tuple(y)) => x == y,
            (Heap(x), Heap(y)) => sorted(x) == sorted(y),
            (Struct(x), Struct(y)) | (Variant(x), Variant(y)) => x.ty == y.ty && x.tag == y.tag && x.fields == y.fields,
            (Err(x), Err(y)) => x == y,
            (Range(x), Range(y)) => x == y,
            (Int(x), Int(y)) => x == y,
            (Str(x), Str(y)) => x == y,
            (Bool(x), Bool(y)) => x == y,
            (Unit, Unit) | (None, None) => true,
            _ => false,
        }
    }
}

impl Eq for Value {}

fn norm(f: f64) -> f64 {
    if f == 0.0 {
        0.0
    } else {
        f
    }
}

impl Hash for Value {
    fn hash<H: Hasher>(&self, h: &mut H) {
        use Value::*;
        std::mem::discriminant(self).hash(h);
        match self {
            Unit | None => {}
            Bool(b) => b.hash(h),
            Int(n) => n.hash(h),
            Float(f) => norm(*f).to_bits().hash(h),
            Str(s) => s.hash(h),
            List(v) | Tuple(v) => {
                v.len().hash(h);
                for x in v.iter() {
                    x.hash(h);
                }
            }
            Map(m) => m.len().hash(h),
            Set(s) => s.len().hash(h),
            Heap(v) => v.len().hash(h),
            Struct(o) | Variant(o) => {
                o.ty.hash(h);
                o.tag.hash(h);
                for x in &o.fields {
                    x.hash(h);
                }
            }
            Range(r) => r.hash(h),
            Err(e) => e.hash(h),
            Func(f) => (Rc::as_ptr(f) as *const u8 as usize).hash(h),
        }
    }
}

/// A heap's items in ascending order.
pub fn sorted(heap: &[Value]) -> Vec<Value> {
    let mut v = heap.to_vec();
    v.sort_by(|a, b| cmp(a, b).unwrap_or(Ordering::Equal));
    v
}

/// Adds `x` to a binary min-heap. Fails if `x` can't be compared with the
/// items it meets.
pub fn heap_push(h: &mut Vec<Value>, x: Value) -> Result<(), (String, String)> {
    h.push(x);
    let mut i = h.len() - 1;
    while i > 0 {
        let parent = (i - 1) / 2;
        match cmp(&h[i], &h[parent]) {
            Ok(Ordering::Less) => h.swap(i, parent),
            Ok(_) => break,
            Result::Err(()) => {
                let x = h.remove(i);
                return Result::Err((kind_name(&x), kind_name(&h[parent])));
            }
        }
        i = parent;
    }
    Ok(())
}

/// Removes the smallest item of a binary min-heap.
pub fn heap_pop(h: &mut Vec<Value>) -> Option<Value> {
    if h.is_empty() {
        return Option::None;
    }
    let top = h.swap_remove(0);
    let mut i = 0;
    loop {
        let (l, r) = (2 * i + 1, 2 * i + 2);
        let mut m = i;
        if l < h.len() && cmp(&h[l], &h[m]) == Ok(Ordering::Less) {
            m = l;
        }
        if r < h.len() && cmp(&h[r], &h[m]) == Ok(Ordering::Less) {
            m = r;
        }
        if m == i {
            break;
        }
        h.swap(i, m);
        i = m;
    }
    Some(top)
}

fn kind_name(v: &Value) -> String {
    match v {
        Value::Int(_) => "int".into(),
        Value::Float(_) => "f64".into(),
        Value::Str(_) => "str".into(),
        Value::Tuple(t) => format!("{}-tuple", t.len()),
        Value::None => "none".into(),
        _ => "value".into(),
    }
}

// ----- display -----

pub fn fmt_float(f: f64) -> String {
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if f.is_nan() {
        return "nan".into();
    }
    format!("{f:?}")
}

/// `print` form: strings unquoted at the top level, quoted inside containers.
pub fn display(v: &Value, p: &Program) -> String {
    match v {
        Value::Str(s) => s.to_string(),
        Value::Err(e) => display(e, p),
        _ => {
            let mut out = String::new();
            repr_into(v, p, &mut out);
            out
        }
    }
}

/// Debug form: strings quoted everywhere.
pub fn repr(v: &Value, p: &Program) -> String {
    let mut out = String::new();
    repr_into(v, p, &mut out);
    out
}

fn repr_into(v: &Value, p: &Program, out: &mut String) {
    match v {
        Value::Unit => out.push_str("()"),
        Value::None => out.push_str("none"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(n) => {
            let _ = write!(out, "{n}");
        }
        Value::Float(f) => out.push_str(&fmt_float(*f)),
        Value::Str(s) => {
            out.push('"');
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\t' => out.push_str("\\t"),
                    '\r' => out.push_str("\\r"),
                    c => out.push(c),
                }
            }
            out.push('"');
        }
        Value::List(xs) => seq(out, "[", "]", xs.iter(), p),
        Value::Tuple(xs) => {
            seq(out, "(", "", xs.iter(), p);
            if xs.len() == 1 {
                out.push(',');
            }
            out.push(')');
        }
        Value::Set(xs) => seq(out, "{", "}", xs.iter(), p),
        Value::Heap(xs) => {
            out.push_str("heap(");
            seq(out, "[", "]", sorted(xs).iter(), p);
            out.push(')');
        }
        Value::Map(m) => {
            out.push('{');
            for (i, (k, v)) in m.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                repr_into(k, p, out);
                out.push_str(": ");
                repr_into(v, p, out);
            }
            out.push('}');
        }
        Value::Struct(o) => {
            let def = &p.structs[o.ty as usize];
            out.push_str(&def.name);
            out.push('{');
            for (i, (f, v)) in def.fields.iter().zip(o.fields.iter()).enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&f.name);
                out.push_str(": ");
                repr_into(v, p, out);
            }
            out.push('}');
        }
        Value::Variant(o) => {
            let def = &p.enums[o.ty as usize];
            out.push_str(&def.variants[o.tag as usize].name);
            if !o.fields.is_empty() {
                seq(out, "(", ")", o.fields.iter(), p);
            }
        }
        Value::Range(r) => {
            let _ = if r.step == 1 { write!(out, "{}..{}", r.start, r.end) } else { write!(out, "({}..{}).step_by({})", r.start, r.end, r.step) };
        }
        Value::Err(e) => {
            out.push_str("err(");
            repr_into(e, p, out);
            out.push(')');
        }
        Value::Func(f) => match &**f {
            Func::User(ids) => {
                let _ = write!(out, "<fn {}>", p.fns[ids[0] as usize].name);
            }
            Func::Closure(..) => out.push_str("<lambda>"),
            Func::Ctor(e, t) => {
                let _ = write!(out, "<fn {}>", p.enums[*e as usize].variants[*t as usize].name);
            }
            Func::Method(m) => {
                let _ = write!(out, "<fn {m}>");
            }
        },
    }
}

fn seq<'a>(out: &mut String, open: &str, close: &str, items: impl Iterator<Item = &'a Value>, p: &Program) {
    out.push_str(open);
    for (i, x) in items.enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        repr_into(x, p, out);
    }
    out.push_str(close);
}

/// The first place two values differ, as a path like `[2].age`, with both
/// sides at that point. Used for test failure output.
pub fn first_diff(a: &Value, b: &Value, p: &Program) -> Option<(String, String, String)> {
    fn go(a: &Value, b: &Value, p: &Program, path: &mut String) -> Option<(String, String, String)> {
        if eq(a, b) == Ok(true) {
            return Option::None;
        }
        match (a, b) {
            (Value::List(x), Value::List(y)) | (Value::Tuple(x), Value::Tuple(y)) => {
                for (i, (u, v)) in x.iter().zip(y.iter()).enumerate() {
                    let n = path.len();
                    let _ = if matches!(a, Value::List(_)) { write!(path, "[{i}]") } else { write!(path, ".{i}") };
                    if let Some(d) = go(u, v, p, path) {
                        return Some(d);
                    }
                    path.truncate(n);
                }
                if x.len() != y.len() {
                    return Some((format!("{path}.len"), x.len().to_string(), y.len().to_string()));
                }
            }
            (Value::Struct(x), Value::Struct(y)) if x.ty == y.ty => {
                let def = &p.structs[x.ty as usize];
                for (i, (u, v)) in x.fields.iter().zip(y.fields.iter()).enumerate() {
                    let n = path.len();
                    let _ = write!(path, ".{}", def.fields[i].name);
                    if let Some(d) = go(u, v, p, path) {
                        return Some(d);
                    }
                    path.truncate(n);
                }
            }
            (Value::Map(x), Value::Map(y)) => {
                for (k, u) in x.iter() {
                    let n = path.len();
                    let _ = write!(path, "[{}]", repr(k, p));
                    match y.get(k) {
                        Some(v) => {
                            if let Some(d) = go(u, v, p, path) {
                                return Some(d);
                            }
                        }
                        Option::None => return Some((path.clone(), repr(u, p), "(missing)".into())),
                    }
                    path.truncate(n);
                }
                for (k, v) in y.iter() {
                    if !x.contains_key(k) {
                        return Some((format!("{path}[{}]", repr(k, p)), "(missing)".into(), repr(v, p)));
                    }
                }
            }
            _ => {}
        }
        Some((path.clone(), repr(a, p), repr(b, p)))
    }
    let mut path = String::new();
    go(a, b, p, &mut path)
}
