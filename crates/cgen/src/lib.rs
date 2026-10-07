//! The C backend. Compiles the resolved IR (`lacon_interp::ir`) to C that
//! links against the runtime in `runtime/`, which mirrors the interpreter's
//! semantics: values are tagged, reference counted and copied on write.
//!
//! Every Lacon expression becomes a C expression (often a GCC statement
//! expression) that yields an owned `lc_v`. Slots are `S[i]`, on the C stack,
//! or in a heap frame when lambdas capture them, as the interpreter's frames
//! are. `?` and `return` inside a lambda leave the enclosing named function,
//! which the generated code does by setting `lc_unwinding` and returning
//! until a named function takes the value.

use std::collections::HashMap;
use std::fmt::Write;
use std::rc::Rc;

use lacon_check::{Types, T};
use lacon_interp::builtins::{is_method, is_mutator, METHODS};
use lacon_interp::ir::*;
use lacon_interp::value::{Func, Value};
use lacon_syntax::ast::{BinOp, CmpOp, Mode, UnOp};
use lacon_syntax::fmtspec::FmtSpec;
use lacon_syntax::{Source, Span};

pub const HEADER: &str = include_str!("../runtime/lacon.h");
pub const RUNTIME: &[(&str, &str)] = &[
    ("rt_core.c", include_str!("../runtime/rt_core.c")),
    ("rt_ops.c", include_str!("../runtime/rt_ops.c")),
    ("rt_methods.c", include_str!("../runtime/rt_methods.c")),
];

/// The C source of a program, to compile with the runtime. `types` is what
/// the checker inferred; when they are sound, ints and bools are unboxed.
pub fn generate(prog: &Program, src: &Source, types: &Types) -> String {
    Gen::new(prog, src, types).run()
}

/// The conversion names, in the order of the runtime's `lc_convs`.
const CONVS: &[&str] = &["int", "i64", "usize", "isize", "i32", "i16", "i8", "u64", "u32", "u16", "u8", "f64", "f32", "str", "bool"];

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// A named function: `return` and `?` leave it.
    Named,
    /// A lambda, default value or constant: they unwind to the named function.
    Nested,
}

/// How a value is held in C: a tagged `lc_v`, or unboxed where the checker
/// proved its type.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Rep {
    Boxed,
    /// An `int64_t`.
    Int,
    /// A C `bool`.
    Bool,
    /// A `double`. The checker's `f64` isn't enough, since an `f64` can hold
    /// an int at run time (`if c: 1 else: 2.5`); see `Gen::certain_float`.
    Float,
}

fn rep_of_type(t: &T) -> Rep {
    match t {
        T::Int(_) => Rep::Int,
        T::Bool => Rep::Bool,
        _ => Rep::Boxed,
    }
}

/// A declared type as the checker's, as far as `place_type` needs it: the
/// structs, lists and maps a place walks through. `None` for the rest.
fn shape_of(t: &Ty) -> Option<T> {
    Some(match t {
        Ty::Struct(id, args) if args.is_empty() => T::Struct(*id, vec![]),
        Ty::List(x) => T::list(shape_of(x)?),
        Ty::Map(k, v) => T::map(shape_of(k).unwrap_or(T::Unknown), shape_of(v)?),
        _ => return None,
    })
}

/// Integer operators done on unboxed ints, as the runtime does them.
fn int_op(op: BinOp) -> Option<&'static str> {
    Some(match op {
        BinOp::Add => "lc_iadd",
        BinOp::Sub => "lc_isub",
        BinOp::Mul => "lc_imul",
        BinOp::Div => "lc_idiv",
        BinOp::Rem => "lc_irem",
        BinOp::Shl => "lc_ishl",
        BinOp::Shr => "lc_ishr",
        BinOp::BitAnd => "&",
        BinOp::BitOr => "|",
        BinOp::BitXor => "^",
        BinOp::Pow => return None,
    })
}

fn cmp_op(op: CmpOp) -> Option<&'static str> {
    Some(match op {
        CmpOp::Eq => "==",
        CmpOp::Ne => "!=",
        CmpOp::Lt => "<",
        CmpOp::Le => "<=",
        CmpOp::Gt => ">",
        CmpOp::Ge => ">=",
        CmpOp::In | CmpOp::NotIn => return None,
    })
}

#[derive(Clone)]
struct Cx {
    /// Deepest `Up` reference, for the frame pointers a lambda sets up.
    max_up: u32,
    /// How each slot of the current frame is held. An unboxed slot `s` is
    /// the C local `N{s}`; a boxed one is `S[s]`.
    slots: Vec<Rep>,
    /// How the current function returns its value: `RETURN` takes this.
    ret: Rep,
    /// The slots of lists of ints, bools or floats that no lambda of the
    /// current frame uses (a call could change those), with that kind:
    /// what a loop may take a view of (see `view_slots`).
    viewable: HashMap<u32, Rep>,
    /// The enclosing loops' views (`lc_view`) of list slots: slot, C name,
    /// kind.
    views: Vec<(u32, String, Rep)>,
    /// The checker's type of each slot, in a sound program.
    tys: Vec<T>,
}

impl Cx {
    fn new(slots: Vec<Rep>, tys: Vec<T>) -> Cx {
        Cx { max_up: 0, slots, ret: Rep::Boxed, viewable: HashMap::new(), views: Vec::new(), tys }
    }
}

/// A named function's typed entry `FT{id}`: parameters and result held as
/// the checker proved them, so direct calls skip boxing and argument arrays.
#[derive(Clone)]
struct Entry {
    params: Vec<Rep>,
    ret: Rep,
}

fn c_type(r: Rep) -> &'static str {
    match r {
        Rep::Boxed => "lc_v",
        Rep::Int => "int64_t",
        Rep::Bool => "bool",
        Rep::Float => "double",
    }
}

fn box_c(r: Rep, v: &str) -> String {
    match r {
        Rep::Boxed => v.to_string(),
        Rep::Int => format!("lc_int({v})"),
        Rep::Bool => format!("lc_bool({v})"),
        Rep::Float => format!("lc_float({v})"),
    }
}

/// A float as a C literal.
fn float_c(f: f64) -> String {
    if f.is_nan() {
        "NAN".into()
    } else if f.is_infinite() {
        if f > 0.0 { "INFINITY" } else { "(-INFINITY)" }.into()
    } else {
        format!("({f:?})")
    }
}

/// Float methods that give a float whatever number they are called on.
const FLOAT_FNS: &[&str] = &["sqrt", "cbrt", "exp", "ln", "log2", "log10", "sin", "cos", "tan", "asin", "acos", "atan", "fract", "atan2", "hypot", "log"];
/// Number methods that give a float when called on one.
const FLOAT_KEEPS: &[&str] = &["abs", "floor", "ceil", "round", "trunc", "sign", "min", "max", "clamp", "pow"];



struct Gen<'p> {
    prog: &'p Program,
    src: &'p Source,
    types: &'p Types,
    /// The checker's types hold at run time, so values can be unboxed.
    typed: bool,
    statics: String,
    funcs: String,
    init: String,
    sites: HashMap<String, String>,
    strs: HashMap<String, String>,
    fields: Vec<String>,
    field_ids: HashMap<String, usize>,
    tys: HashMap<String, String>,
    groups: Vec<Rc<[FnId]>>,
    ctors: Vec<(u32, u32)>,
    entries: Vec<Option<Entry>>,
    lambda_n: usize,
    tmp: usize,
    cx: Cx,
}

fn cstr(s: &str) -> String {
    let mut out = String::from("\"");
    for b in s.bytes() {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            b'?' => out.push_str("\\?"),
            0x20..=0x7e => out.push(b as char),
            _ => {
                let _ = write!(out, "\\{:03o}", b);
            }
        }
    }
    out.push('"');
    out
}

/// The `to, name, lo, hi` arguments of `lc_convert` and `lc_parse_as`.
fn conv_args(to: ConvTo) -> String {
    let (code, name, lo, hi) = match to {
        ConvTo::Int(n, lo, hi) => ("CONV_INT", n, lo, hi),
        ConvTo::Float => ("CONV_FLOAT", "f64", 0, 0),
        ConvTo::Str => ("CONV_STR", "str", 0, 0),
        ConvTo::Bool => ("CONV_BOOL", "bool", 0, 0),
    };
    let lo = if lo == i64::MIN { "INT64_MIN".to_string() } else { format!("{lo}LL") };
    let hi = if hi == i64::MAX { "INT64_MAX".to_string() } else { format!("{hi}LL") };
    format!("{code}, {}, {lo}, {hi}", cstr(name))
}

fn binop_c(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "OP_ADD",
        BinOp::Sub => "OP_SUB",
        BinOp::Mul => "OP_MUL",
        BinOp::Div => "OP_DIV",
        BinOp::Rem => "OP_REM",
        BinOp::Pow => "OP_POW",
        BinOp::BitAnd => "OP_BITAND",
        BinOp::BitOr => "OP_BITOR",
        BinOp::BitXor => "OP_BITXOR",
        BinOp::Shl => "OP_SHL",
        BinOp::Shr => "OP_SHR",
    }
}

fn cmp_c(op: CmpOp) -> &'static str {
    match op {
        CmpOp::Eq => "CMP_EQ",
        CmpOp::Ne => "CMP_NE",
        CmpOp::Lt => "CMP_LT",
        CmpOp::Le => "CMP_LE",
        CmpOp::Gt => "CMP_GT",
        CmpOp::Ge => "CMP_GE",
        CmpOp::In => "CMP_IN",
        CmpOp::NotIn => "CMP_NOTIN",
    }
}

fn method_id(name: &str) -> Option<usize> {
    METHODS.iter().position(|m| *m == name)
}

fn conv_index(to: ConvTo) -> usize {
    match to {
        ConvTo::Int(name, _, _) => CONVS.iter().position(|c| *c == name).unwrap_or(0),
        ConvTo::Float => 11,
        ConvTo::Str => 13,
        ConvTo::Bool => 14,
    }
}

/// Does an expression contain a lambda anywhere inside it?
fn has_lambda(e: &Ex) -> bool {
    let mut found = false;
    walk_ex(e, 0, &mut |n, _| {
        if let Node::Ex(Ex::Lambda(_)) = n {
            found = true;
        }
    });
    found
}

/// A function's parameter slots, and those of them declared `f64`, which
/// the runtime converts to floats on entry.
fn param_slots(fd: &FnDef) -> (Vec<u32>, Vec<u32>) {
    let all = (0..fd.params.len() as u32).collect();
    let floats = fd.params.iter().enumerate().filter(|(_, p)| matches!(p.ty, Ty::Float)).map(|(i, _)| i as u32).collect();
    (all, floats)
}

/// The slots a pattern binds.
fn pat_slots(p: &PatIr, out: &mut Vec<u32>) {
    match p {
        PatIr::Bind(s) => out.push(*s),
        PatIr::Tuple(ps) | PatIr::Or(ps) => ps.iter().for_each(|p| pat_slots(p, out)),
        PatIr::List { items, rest } => {
            items.iter().for_each(|p| pat_slots(p, out));
            if let Some(Some(s)) = rest {
                out.push(*s);
            }
        }
        PatIr::Variant { args, .. } => args.iter().for_each(|p| pat_slots(p, out)),
        PatIr::Struct { fields, .. } => fields.iter().for_each(|(_, p)| pat_slots(p, out)),
        PatIr::Err(p) | PatIr::Ok(p) | PatIr::Some(p) => pat_slots(p, out),
        PatIr::Wild | PatIr::Lit(_) | PatIr::Range { .. } | PatIr::None => {}
    }
}

/// What the frame's own code writes to its slots: the value of a plain
/// binding or assignment, or `None` where a pattern or an unpacking binds
/// a slot from inside another value. Compound assignments are left out.
/// The flag marks a write the slot's declared `f64` type makes a float.
fn frame_writes(body: &Ex) -> Vec<(u32, Option<&Ex>, bool)> {
    let mut out = Vec::new();
    walk_ex(body, 0, &mut |n, d| {
        if d != 0 {
            return;
        }
        let mut bound = Vec::new();
        match n {
            Node::St(St::Bind(PatIr::Bind(s), e, ty, _)) => out.push((*s, Some(e), matches!(ty, Some(Ty::Float)))),
            Node::St(St::Bind(p, ..)) | Node::St(St::For { pat: p, .. }) => pat_slots(p, &mut bound),
            Node::St(St::Assign { place: Place { root: Root::Local(s), path, decl, .. }, op: None, value, .. }) if path.is_empty() => {
                out.push((*s, Some(value), matches!(decl, Some(Ty::Float))));
            }
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
        out.extend(bound.into_iter().map(|s| (s, None, false)));
    });
    out
}

/// Slots of the frame whose body is `body` that must stay boxed whatever
/// their type: those a nested lambda uses, which live in the heap frame,
/// and those used as the root of a longer place or changed in place by a
/// method or a `mut` argument, which the runtime reaches by pointer.
fn pinned_slots(prog: &Program, body: &Ex) -> std::collections::HashSet<u32> {
    let mut out = std::collections::HashSet::new();
    let mut_param = |id: FnId, i: usize| prog.fns[id as usize].params.get(i).is_some_and(|p| p.mode == Mode::Mut);
    walk_ex(body, 0, &mut |n, d| match n {
        Node::Ex(Ex::Up(k, s, _)) if *k == d => {
            out.insert(*s);
        }
        Node::Ex(Ex::CallFn { fns, places, .. }) if d == 0 => {
            for (i, p) in places.iter().enumerate() {
                if let Some(Place { root: Root::Local(s), .. }) = p {
                    if fns.iter().any(|&id| mut_param(id, i)) {
                        out.insert(*s);
                    }
                }
            }
        }
        Node::Ex(Ex::Method { recv: Recv::Place(Place { root: Root::Local(s), .. }), user: Some(ids), .. }) if d == 0 => {
            if ids.iter().any(|&id| mut_param(id, 0)) {
                out.insert(*s);
            }
        }
        Node::Place(p, mutated) => match p.root {
            Root::Up(k, s) if k == d => {
                out.insert(s);
            }
            Root::Local(s) if d == 0 && (!p.path.is_empty() || mutated) => {
                out.insert(s);
            }
            _ => {}
        },
        _ => {}
    });
    out
}

/// Whether the frame whose body is `body` only reads its slot `s`: never
/// binds it, assigns it (compound assignments too), changes it in place or
/// reaches it by pointer. Such a parameter can borrow its argument.
fn only_read(prog: &Program, body: &Ex, s: u32) -> bool {
    if pinned_slots(prog, body).contains(&s) || frame_writes(body).iter().any(|(w, _, _)| *w == s) {
        return false;
    }
    let mut assigned = false;
    walk_ex(body, 0, &mut |n, d| {
        if let (Node::St(St::Assign { place: Place { root: Root::Local(r), .. }, .. }), 0) = (n, d) {
            assigned |= *r == s;
        }
    });
    !assigned
}

/// The slots of the frame whose body is `body` that a lambda in it uses.
fn captured_slots(body: &Ex) -> std::collections::HashSet<u32> {
    let mut out = std::collections::HashSet::new();
    walk_ex(body, 0, &mut |n, d| match n {
        Node::Ex(Ex::Up(k, s, _)) if *k == d => {
            out.insert(*s);
        }
        Node::Place(Place { root: Root::Up(k, s), .. }, _) if *k == d => {
            out.insert(*s);
        }
        _ => {}
    });
    out
}

/// The slots a loop (its condition and body) uses only to read elements,
/// assign elements or their fields (`xs[i] = v`, `xs[i].f op= v`) and read
/// `len`: it never binds or assigns them, passes them anywhere, calls a
/// method on them or reaches into an element otherwise. Nothing in such a
/// loop changes their lists but those assignments, unless a lambda that
/// captured one is called, so the caller leaves out captured slots.
fn view_slots(cond: Option<&Ex>, body: &[St]) -> Vec<u32> {
    // Per slot: uses as a value, the allowed ones, uses as a place, the
    // allowed ones.
    let mut uses: HashMap<u32, [usize; 4]> = HashMap::new();
    let mut bad = std::collections::HashSet::new();
    let local = |e: &Ex| match e {
        Ex::Local(s, _) => Some(*s),
        _ => None,
    };
    let mut f = |n: Node, d: u32| {
        if d != 0 {
            return;
        }
        let mut bound = Vec::new();
        match n {
            Node::Ex(Ex::Local(s, _)) => uses.entry(*s).or_default()[0] += 1,
            Node::Ex(Ex::Index { obj, .. }) => {
                if let Some(s) = local(obj) {
                    uses.entry(s).or_default()[1] += 1;
                }
            }
            Node::Ex(Ex::Field { obj, name, user: None, .. }) if &**name == "len" => {
                if let Some(s) = local(obj) {
                    uses.entry(s).or_default()[1] += 1;
                }
            }
            Node::Ex(Ex::Method { recv, name, user: None, args, .. }) if &**name == "len" && args.is_empty() => match recv {
                Recv::Value(obj) => {
                    if let Some(s) = local(obj) {
                        uses.entry(s).or_default()[1] += 1;
                    }
                }
                Recv::Place(Place { root: Root::Local(s), path, .. }) if path.is_empty() => uses.entry(*s).or_default()[3] += 1,
                _ => {}
            },
            Node::Place(Place { root: Root::Local(s), .. }, _) => uses.entry(*s).or_default()[2] += 1,
            Node::St(St::Assign { place: Place { root: Root::Local(s), path, .. }, .. }) if matches!(&path[..], [Seg::Index(..)] | [Seg::Index(..), Seg::Field(..)]) => {
                uses.entry(*s).or_default()[3] += 1;
            }
            Node::St(St::Bind(p, ..)) | Node::St(St::For { pat: p, .. }) => pat_slots(p, &mut bound),
            Node::St(St::AssignMulti { targets, .. }) => {
                for t in targets {
                    if let Target::Bind(s) = t {
                        bound.push(*s);
                    }
                }
            }
            Node::Ex(Ex::Match { arms, .. }) => arms.iter().for_each(|a| pat_slots(&a.pat, &mut bound)),
            _ => {}
        }
        bad.extend(bound);
    };
    if let Some(c) = cond {
        walk_ex(c, 0, &mut f);
    }
    walk_stmts(body, 0, &mut f);
    let mut out: Vec<u32> = uses.into_iter().filter(|(s, [v, va, p, pa])| v == va && p == pa && !bad.contains(s)).map(|(s, _)| s).collect();
    out.sort_unstable();
    out
}

impl<'p> Gen<'p> {
    fn new(prog: &'p Program, src: &'p Source, types: &'p Types) -> Gen<'p> {
        Gen {
            prog,
            src,
            types,
            // `LACON_BOXED=1` keeps every value boxed, to measure the difference.
            typed: types.sound && std::env::var_os("LACON_BOXED").is_none(),
            statics: String::new(),
            funcs: String::new(),
            init: String::new(),
            sites: HashMap::new(),
            strs: HashMap::new(),
            fields: Vec::new(),
            field_ids: HashMap::new(),
            tys: HashMap::new(),
            groups: Vec::new(),
            ctors: Vec::new(),
            entries: Vec::new(),
            lambda_n: 0,
            tmp: 0,
            cx: Cx::new(vec![], vec![]),
        }
    }

    // ----- representations -----

    fn rep(&self, e: &Ex) -> Rep {
        if !self.typed {
            return Rep::Boxed;
        }
        match self.types.of(e) {
            Some(T::Float) if self.certain_float(e) => Rep::Float,
            Some(t) => rep_of_type(t),
            None => Rep::Boxed,
        }
    }

    /// Typed int or `f64` by the checker: an int or a float at run time.
    fn numeric(&self, e: &Ex) -> bool {
        self.typed && matches!(self.types.of(e), Some(T::Int(_) | T::Float))
    }

    /// An `f64` expression that is a float at run time, not an int: it is
    /// made by something that always makes floats (a float literal, `f64(n)`,
    /// `sqrt`, a declared `f64` parameter or result, which the runtime
    /// converts) or computed from one with numbers.
    fn certain_float(&self, e: &Ex) -> bool {
        let float_or_never = |x: &Ex| self.rep(x) == Rep::Float || matches!(self.types.of(x), Some(T::Never));
        match e {
            Ex::Lit(Value::Float(_), _) => true,
            Ex::Local(s, _) => self.slot_rep(*s) == Rep::Float,
            Ex::Binary { op, l, r, .. } => {
                matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem | BinOp::Pow)
                    && self.numeric(l)
                    && self.numeric(r)
                    && (self.rep(l) == Rep::Float || self.rep(r) == Rep::Float)
            }
            Ex::Unary { op: UnOp::Neg, e: x, .. } => self.rep(x) == Rep::Float,
            Ex::If { then, els: Some(els), .. } => float_or_never(then) && float_or_never(els),
            Ex::Block(_, Some(t)) => self.rep(t) == Rep::Float,
            Ex::Match { arms, .. } => arms.iter().all(|a| float_or_never(&a.body)),
            Ex::CallFn { fns, args, .. } => self.direct_entry(fns, args).is_some_and(|en| en.ret == Rep::Float),
            Ex::CallBuiltin { f: Builtin::Conv(ConvTo::Float), args, .. } => args.len() == 1 && self.numeric(&args[0]),
            Ex::Cast { to: ConvTo::Float, e: x, .. } => self.numeric(x),
            Ex::Field { obj, name, user: None, .. } => self.float_field(obj, name).is_some(),
            Ex::Method { recv, name, user: None, .. } => {
                let recv_float = match recv {
                    Recv::Value(x) => self.rep(x) == Rep::Float,
                    Recv::Place(Place { root: Root::Local(s), path, .. }) => path.is_empty() && self.slot_rep(*s) == Rep::Float,
                    Recv::Place(_) => false,
                };
                FLOAT_FNS.contains(&&**name) || (recv_float && FLOAT_KEEPS.contains(&&**name))
            }
            _ => false,
        }
    }

    /// The checker's slot types for the frame whose body is `body`.
    fn slot_tys(&self, body: &Ex) -> Vec<T> {
        if !self.typed {
            return Vec::new();
        }
        self.types.frame(body).map(|t| t.to_vec()).unwrap_or_default()
    }

    /// The checker's type of what the first `upto` steps of place `p`
    /// reach, for a place rooted in this frame's own slots.
    fn place_type(&self, p: &Place, upto: usize) -> Option<T> {
        let Root::Local(s) = p.root else { return None };
        let mut t = self.cx.tys.get(s as usize)?.clone();
        for seg in p.path.iter().take(upto) {
            t = match (seg, t) {
                (Seg::Field(name, _), T::Struct(id, _)) => {
                    let fd = self.prog.structs[id as usize].fields.iter().find(|f| *f.name == **name)?;
                    shape_of(&fd.ty)?
                }
                (Seg::Index(..), T::List(x)) => *x,
                (Seg::Index(..), T::Map(_, v)) => *v,
                _ => return None,
            };
        }
        Some(t)
    }

    fn slot_rep(&self, s: u32) -> Rep {
        self.cx.slots.get(s as usize).copied().unwrap_or(Rep::Boxed)
    }

    /// How each slot of a frame is held: unboxed where the checker proved it
    /// an int or a bool, unless something reaches it by pointer. `boxed`
    /// slots stay boxed regardless.
    ///
    /// An `f64` slot is unboxed when everything written to it is a float
    /// for certain (`certain_float`). `params` are written by callers, and
    /// only the `float_params` among them for certain, being converted on
    /// entry; patterns never are. Starting from every `f64` slot, those with
    /// a write that may be an int are dropped until none is left.
    fn frame_reps(&mut self, body: &Ex, nslots: u32, boxed: &[u32], params: &[u32], float_params: &[u32]) -> Vec<Rep> {
        let mut reps = vec![Rep::Boxed; nslots as usize];
        if !self.typed {
            return reps;
        }
        let Some(ts) = self.types.frame(body) else { return reps };
        let pinned = pinned_slots(self.prog, body);
        for (i, t) in ts.iter().enumerate().take(nslots as usize) {
            let s = i as u32;
            if !pinned.contains(&s) && !boxed.contains(&s) {
                reps[i] = match t {
                    T::Float => Rep::Float,
                    t => rep_of_type(t),
                };
            }
        }
        let writes = frame_writes(body);
        let saved = std::mem::replace(&mut self.cx, Cx::new(reps, vec![]));
        for &s in params {
            if self.cx.slots.get(s as usize) == Some(&Rep::Float) && !float_params.contains(&s) {
                self.cx.slots[s as usize] = Rep::Boxed;
            }
        }
        loop {
            let mut changed = false;
            for (s, w, conv) in &writes {
                let i = *s as usize;
                let float = *conv && w.is_some_and(|e| self.numeric(e)) || w.is_some_and(|e| self.rep(e) == Rep::Float);
                if self.cx.slots.get(i) == Some(&Rep::Float) && !float {
                    self.cx.slots[i] = Rep::Boxed;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        std::mem::replace(&mut self.cx, saved).slots
    }

    /// Declarations of the current frame's unboxed slots.
    fn native_decls(&self) -> String {
        let mut out = String::new();
        for (i, r) in self.cx.slots.iter().enumerate() {
            match r {
                Rep::Int => {
                    let _ = writeln!(out, "    int64_t N{i} = 0;");
                }
                Rep::Bool => {
                    let _ = writeln!(out, "    bool N{i} = 0;");
                }
                Rep::Float => {
                    let _ = writeln!(out, "    double N{i} = 0;");
                }
                Rep::Boxed => {}
            }
        }
        out
    }

    /// Stores the C value `v` (an owned `lc_v`) into slot `s`, unboxing it
    /// when the slot is unboxed.
    fn store(&mut self, s: u32, v: &str, site: &str) -> String {
        match self.slot_rep(s) {
            Rep::Int => format!("N{s} = lc_unbox_int({v}, {site}); "),
            Rep::Bool => format!("N{s} = lc_unbox_bool({v}, {site}); "),
            Rep::Float => format!("N{s} = lc_unbox_float({v}, {site}); "),
            Rep::Boxed => format!("lc_set(&S[{s}], {v}); "),
        }
    }

    fn t(&mut self) -> String {
        self.tmp += 1;
        format!("_t{}", self.tmp)
    }

    // ----- statics -----

    fn site(&mut self, span: Span) -> String {
        let (l, c) = self.src.line_col(span.start);
        let text = format!("{}:{l}:{c}", self.src.name);
        if let Some(id) = self.sites.get(&text) {
            return id.clone();
        }
        let id = format!("SITE{}", self.sites.len());
        let _ = writeln!(self.statics, "static const char {id}[] = {};", cstr(&text));
        self.sites.insert(text, id.clone());
        id
    }

    fn str_const(&mut self, s: &str) -> String {
        if let Some(id) = self.strs.get(s) {
            return id.clone();
        }
        let id = format!("STR{}", self.strs.len());
        let _ = writeln!(self.statics, "static lc_v {id};");
        let _ = writeln!(self.init, "    {id} = lc_str_lit({}, {});", cstr(s), s.len());
        self.strs.insert(s.to_string(), id.clone());
        id
    }

    fn field_id(&mut self, name: &str) -> usize {
        if let Some(&i) = self.field_ids.get(name) {
            return i;
        }
        let i = self.fields.len();
        self.fields.push(name.to_string());
        self.field_ids.insert(name.to_string(), i);
        i
    }

    /// A static `lc_ty` for a declared type; returns `&TYn`.
    fn ty(&mut self, t: &Ty) -> String {
        let key = format!("{t:?}");
        if let Some(id) = self.tys.get(&key) {
            return format!("&{id}");
        }
        let null = "NULL".to_string();
        let (kind, name, lo, hi, a, b, elems, id): (&str, String, i64, i64, String, String, Vec<String>, u32) = match t {
            Ty::Any => ("TY_ANY", null.clone(), 0, 0, null.clone(), null.clone(), vec![], 0),
            Ty::Param(n) => ("TY_ANY", cstr(n), 0, 0, null.clone(), null.clone(), vec![], 0),
            Ty::Int { name, min, max } => ("TY_INT", cstr(name), *min, *max, null.clone(), null.clone(), vec![], 0),
            Ty::Float => ("TY_FLOAT", null.clone(), 0, 0, null.clone(), null.clone(), vec![], 0),
            Ty::Bool => ("TY_BOOL", null.clone(), 0, 0, null.clone(), null.clone(), vec![], 0),
            Ty::Str => ("TY_STR", null.clone(), 0, 0, null.clone(), null.clone(), vec![], 0),
            Ty::Unit => ("TY_UNIT", null.clone(), 0, 0, null.clone(), null.clone(), vec![], 0),
            Ty::List(x) => ("TY_LIST", null.clone(), 0, 0, self.ty(x), null.clone(), vec![], 0),
            Ty::Set(x) => ("TY_SET", null.clone(), 0, 0, self.ty(x), null.clone(), vec![], 0),
            Ty::Heap(x) => ("TY_HEAP", null.clone(), 0, 0, self.ty(x), null.clone(), vec![], 0),
            Ty::Optional(x) => ("TY_OPT", null.clone(), 0, 0, self.ty(x), null.clone(), vec![], 0),
            Ty::Result(x) => ("TY_RES", null.clone(), 0, 0, self.ty(x), null.clone(), vec![], 0),
            Ty::Map(k, v) => {
                let (k, v) = (self.ty(k), self.ty(v));
                ("TY_MAP", null.clone(), 0, 0, k, v, vec![], 0)
            }
            Ty::Tuple(ts) => {
                let es = ts.iter().map(|t| self.ty(t)).collect();
                ("TY_TUPLE", null.clone(), 0, 0, null.clone(), null.clone(), es, 0)
            }
            Ty::Fn(ps, r) => {
                let es = ps.iter().map(|t| self.ty(t)).collect();
                let r = self.ty(r);
                ("TY_FN", null.clone(), 0, 0, r, null.clone(), es, 0)
            }
            Ty::Struct(sid, args) => {
                let es = args.iter().map(|t| self.ty(t)).collect();
                ("TY_STRUCT", null.clone(), 0, 0, null.clone(), null.clone(), es, *sid)
            }
            Ty::Enum(eid, args) => {
                let es = args.iter().map(|t| self.ty(t)).collect();
                ("TY_ENUM", null.clone(), 0, 0, null.clone(), null.clone(), es, *eid)
            }
        };
        let n = self.tys.len();
        let name_id = format!("TY{n}");
        let elems_c = if elems.is_empty() {
            "NULL".to_string()
        } else {
            let _ = writeln!(self.statics, "static const lc_ty *const {name_id}_e[] = {{{}}};", elems.join(", "));
            format!("{name_id}_e")
        };
        let lo_c = if lo == i64::MIN { "INT64_MIN".to_string() } else { format!("{lo}LL") };
        let hi_c = if hi == i64::MAX { "INT64_MAX".to_string() } else { format!("{hi}LL") };
        let _ = writeln!(self.statics, "static const lc_ty {name_id} = {{{kind}, {name}, {lo_c}, {hi_c}, {a}, {b}, {}, {elems_c}, {id}}};", elems.len());
        self.tys.insert(key, name_id.clone());
        format!("&{name_id}")
    }

    fn group(&mut self, ids: &Rc<[FnId]>) -> String {
        if !self.groups.iter().any(|g| g[0] == ids[0]) {
            self.groups.push(ids.clone());
            let _ = writeln!(self.statics, "static lc_v G{}(lc_fn *self, int argc, lc_v *args, const char *site);", ids[0]);
        }
        format!("G{}", ids[0])
    }

    fn ctor(&mut self, e: u32, t: u32) -> String {
        if !self.ctors.contains(&(e, t)) {
            self.ctors.push((e, t));
            let _ = writeln!(self.statics, "static lc_v C{e}_{t}(lc_fn *self, int argc, lc_v *args, const char *site);");
        }
        format!("C{e}_{t}")
    }

    // ----- program -----

    fn run(mut self) -> String {
        let prog = self.prog;
        for s in &prog.structs {
            for f in &s.fields {
                self.field_id(&f.name);
            }
        }
        // Constants become functions run at start.
        for (i, c) in prog.consts.iter().enumerate() {
            let _ = writeln!(self.statics, "static lc_v K{i};");
            let body = self.thunk_fn(&format!("KI{i}"), &c.value, c.nslots);
            self.funcs.push_str(&body);
            let _ = writeln!(self.init, "    K{i} = KI{i}();");
        }
        // Entries first, so bodies can call them; a parameter's rep never
        // depends on the calls in its function.
        for id in 0..prog.fns.len() {
            let en = self.entry_of(id as FnId);
            self.entries.push(en);
        }
        for (id, fd) in prog.fns.iter().enumerate() {
            let _ = writeln!(self.statics, "static lc_v F{id}(lc_v *a);");
            if let Some(en) = &self.entries[id] {
                let ps: Vec<String> = en.params.iter().map(|r| c_type(*r).to_string()).collect();
                let ps = if ps.is_empty() { "void".to_string() } else { ps.join(", ") };
                let _ = writeln!(self.statics, "static {} FT{id}({ps});", c_type(en.ret));
            }
            for (i, p) in fd.params.iter().enumerate() {
                if let Some(d) = &p.default {
                    let _ = writeln!(self.statics, "static lc_v D{id}_{i}(void);");
                    let body = self.thunk_fn(&format!("D{id}_{i}"), &d.body, d.nslots);
                    self.funcs.push_str(&body);
                }
            }
        }
        for (sid, sd) in prog.structs.iter().enumerate() {
            for (i, f) in sd.fields.iter().enumerate() {
                if let Some(d) = &f.default {
                    let _ = writeln!(self.statics, "static lc_v SD{sid}_{i}(void);");
                    let body = self.thunk_fn(&format!("SD{sid}_{i}"), &d.body, d.nslots);
                    self.funcs.push_str(&body);
                }
            }
        }
        for id in 0..prog.fns.len() {
            let body = self.named_fn(id as FnId);
            self.funcs.push_str(&body);
        }
        // Dispatchers for functions used as values, and variant constructors.
        let mut i = 0;
        while i < self.groups.len() {
            let g = self.groups[i].clone();
            let body = self.dispatcher(&g);
            self.funcs.push_str(&body);
            i += 1;
        }
        for (e, t) in self.ctors.clone() {
            let body = self.ctor_fn(e, t);
            self.funcs.push_str(&body);
        }

        let mut out = String::new();
        out.push_str("#include \"lacon.h\"\n#include <math.h>\n\n");
        // Program tables.
        let mut tables = String::new();
        for (sid, sd) in prog.structs.iter().enumerate() {
            let ids: Vec<String> = sd.fields.iter().map(|f| self.field_ids[&f.name].to_string()).collect();
            let names: Vec<String> = sd.fields.iter().map(|f| cstr(&f.name)).collect();
            let _ = writeln!(tables, "static const int SF{sid}[] = {{{}}};", if ids.is_empty() { "0".into() } else { ids.join(", ") });
            let _ = writeln!(tables, "static const char *const SN{sid}[] = {{{}}};", if names.is_empty() { "\"\"".into() } else { names.join(", ") });
        }
        // Each field's declared type, which a value stored in it takes;
        // after the statics, which define the types.
        let mut field_tys = String::new();
        for (sid, sd) in prog.structs.iter().enumerate() {
            let tys: Vec<String> = sd.fields.iter().map(|f| self.ty(&f.ty)).collect();
            let _ = writeln!(field_tys, "static const lc_ty *const ST{sid}[] = {{{}}};", if tys.is_empty() { "NULL".into() } else { tys.join(", ") });
        }
        let structs: Vec<String> =
            prog.structs.iter().enumerate().map(|(i, s)| format!("{{{}, {}, SF{i}, SN{i}, ST{i}}}", cstr(&s.name), s.fields.len())).collect();
        let _ = writeln!(field_tys, "static const lc_struct_info STRUCTS[] = {{{}}};", if structs.is_empty() { "{\"\", 0, NULL, NULL, NULL}".into() } else { structs.join(", ") });
        for (eid, ed) in prog.enums.iter().enumerate() {
            let vs: Vec<String> = ed.variants.iter().map(|v| format!("{{{}, {}}}", cstr(&v.name), v.fields.len())).collect();
            let _ = writeln!(tables, "static const lc_variant_info EV{eid}[] = {{{}}};", if vs.is_empty() { "{\"\", 0}".into() } else { vs.join(", ") });
        }
        let enums: Vec<String> = prog.enums.iter().enumerate().map(|(i, e)| format!("{{{}, {}, EV{i}}}", cstr(&e.name), e.variants.len())).collect();
        let _ = writeln!(tables, "static const lc_enum_info ENUMS[] = {{{}}};", if enums.is_empty() { "{\"\", 0, NULL}".into() } else { enums.join(", ") });
        let fns: Vec<String> = prog.fns.iter().map(|f| cstr(&f.name)).collect();
        let _ = writeln!(tables, "static const char *const FN_NAMES[] = {{{}}};", if fns.is_empty() { "\"\"".into() } else { fns.join(", ") });
        let fields: Vec<String> = self.fields.iter().map(|f| cstr(f)).collect();
        let _ = writeln!(tables, "static const char *const FIELD_NAMES[] = {{{}}};", if fields.is_empty() { "\"\"".into() } else { fields.join(", ") });
        let _ = writeln!(field_tys, "static const lc_program PROG = {{STRUCTS, ENUMS, FN_NAMES, FIELD_NAMES}};");
        out.push_str(&tables);
        out.push_str(&self.statics);
        out.push_str(&field_tys);
        out.push('\n');
        out.push_str(&self.funcs);
        let _ = writeln!(out, "\nstatic void INIT(void) {{\n{}}}\n", self.init);
        match prog.main {
            Some(m) => {
                let _ = writeln!(out, "int main(int argc, char **argv) {{ return lc_main(argc, argv, &PROG, INIT, F{m}); }}");
            }
            None => {
                let _ = writeln!(out, "int main(void) {{ return 0; }}");
            }
        }
        out
    }

    /// The prologue that gives a function its slots: `S`, and `FR` when
    /// they live in a heap frame.
    fn frame_setup(nslots: u32, heap: bool, parent: &str) -> (String, String) {
        let n = nslots.max(1);
        if heap {
            (
                format!("    lc_frame *FR = lc_frame_new({n}, {parent});\n    lc_v *S = FR->slots;\n"),
                "    lc_release(lc_obj_v(T_FRAME, FR));\n".to_string(),
            )
        } else {
            let mut setup = format!("    lc_v S[{n}];\n");
            let _ = writeln!(setup, "    for (int _i = 0; _i < {n}; _i++) S[_i] = LC_UNIT;");
            (setup, format!("    for (int _i = 0; _i < {n}; _i++) lc_release(S[_i]);\n"))
        }
    }

    fn macros(kind: Kind) -> &'static str {
        match kind {
            Kind::Named => "#undef RETURN\n#undef UNWIND\n#define RETURN(v) { ret = (v); goto out; }\n#define UNWIND { ret = lc_unwind_val; lc_unwinding = 0; goto out; }\n",
            Kind::Nested => "#undef RETURN\n#undef UNWIND\n#define RETURN(v) { lc_unwind_val = (v); lc_unwinding = 1; goto out; }\n#define UNWIND { goto out; }\n",
        }
    }

    /// A function of no arguments that computes a value: a constant or a
    /// default.
    fn thunk_fn(&mut self, name: &str, body: &Ex, nslots: u32) -> String {
        let reps = self.frame_reps(body, nslots, &[], &[], &[]);
        let tys = self.slot_tys(body);
        let saved = std::mem::replace(&mut self.cx, Cx::new(reps, tys));
        let heap = has_lambda(body);
        let e = self.expr(body);
        let natives = self.native_decls();
        self.cx = saved;
        let (setup, teardown) = Self::frame_setup(nslots, heap, "NULL");
        let mut f = String::new();
        f.push_str(Self::macros(Kind::Nested));
        let _ = writeln!(f, "static lc_v {name}(void) {{\n    lc_v ret = LC_UNIT;\n{setup}{natives}    ret = {e};\n    goto out;\nout:\n{teardown}    return ret;\n}}\n");
        f
    }

    /// The typed entry of function `id`, if it gets one: any function but
    /// `main` without generics, `mut` parameters, defaults or lambdas.
    fn entry_of(&mut self, id: FnId) -> Option<Entry> {
        let fd = &self.prog.fns[id as usize];
        if !self.typed || self.prog.main == Some(id) || !fd.generics.is_empty() || has_lambda(&fd.body) {
            return None;
        }
        if fd.params.iter().any(|p| p.mode == Mode::Mut || p.default.is_some()) {
            return None;
        }
        let (params, floats) = param_slots(fd);
        let reps = self.frame_reps(&fd.body, fd.nslots, &[], &params, &floats);
        let ret = match &fd.ret {
            Some(Ty::Int { .. }) => Rep::Int,
            Some(Ty::Bool) => Rep::Bool,
            Some(Ty::Float) => Rep::Float,
            _ => Rep::Boxed,
        };
        Some(Entry { params: reps[..fd.params.len()].to_vec(), ret })
    }

    /// Checks parameter `i` of `fd`, given the C value `v` (an `lc_v` when
    /// `boxed`, else of the parameter's rep), as the runtime checks
    /// arguments; gives the value to pass, of the parameter's rep.
    fn check_param(&mut self, fd: &FnDef, i: usize, r: Rep, v: &str, boxed: bool, site: &str) -> String {
        let p = &fd.params[i];
        let fname = cstr(&fd.name);
        if !boxed {
            // Unboxed into an unboxed parameter: only a sized int's range.
            if let Ty::Int { min, max, .. } = p.ty {
                if min != i64::MIN || max != i64::MAX {
                    let ty = self.ty(&p.ty);
                    return format!(
                        "({{ if ({v} < {min}LL || {v} > {max}LL) lc_coerce_arg(lc_int({v}), {ty}, {}, {fname}, {site}); {v}; }})",
                        cstr(&p.name)
                    );
                }
            }
            return v.to_string();
        }
        let checked = if matches!(p.ty, Ty::Any | Ty::Param(_)) {
            v.to_string()
        } else {
            let ty = self.ty(&p.ty);
            let slow = format!("lc_coerce_arg({v}, {ty}, {}, {fname}, {site})", cstr(&p.name));
            match exact_tag(&p.ty, v) {
                Some(check) => format!("({check} ? {v} : {slow})"),
                None => slow,
            }
        };
        match r {
            Rep::Int => format!("({checked}).u.i"),
            Rep::Bool => format!("(({checked}).u.i != 0)"),
            Rep::Float => format!("({checked}).u.f"),
            Rep::Boxed => checked,
        }
    }

    /// `FT{id}`, and `F{id}` as a wrapper that checks boxed arguments and
    /// calls it.
    fn typed_fn(&mut self, id: FnId, en: &Entry) -> String {
        let fd = &self.prog.fns[id as usize];
        let (params, floats) = param_slots(fd);
        let mut cx = Cx::new(self.frame_reps(&fd.body, fd.nslots, &[], &params, &floats), self.slot_tys(&fd.body));
        cx.ret = en.ret;
        cx.viewable = self.viewable(&fd.body);
        let saved = std::mem::replace(&mut self.cx, cx);
        let body = match en.ret {
            Rep::Int => self.int(&fd.body),
            Rep::Bool => self.boolean(&fd.body),
            // The runtime makes an int returned for an `f64` a float.
            Rep::Float => self.num(&fd.body),
            Rep::Boxed => self.expr(&fd.body),
        };
        let natives = self.native_decls();
        self.cx = saved;
        let (setup, teardown) = Self::frame_setup(fd.nslots, false, "NULL");
        let fname = cstr(&fd.name);
        let fsite = self.site(fd.span);
        let rt = c_type(en.ret);
        let mut f = String::new();
        // `return` and `?` inside a lambda called from here end this
        // function with a boxed value, checked like any return value.
        let _ = writeln!(f, "#undef RETURN\n#undef UNWIND\n#define RETURN(v) {{ ret = (v); goto out; }}");
        let _ = writeln!(f, "#define UNWIND {{ unw = lc_unwind_val; unwound = true; lc_unwinding = 0; goto out; }}");
        let ps: Vec<String> = en.params.iter().enumerate().map(|(i, r)| format!("{} P{i}", c_type(*r))).collect();
        let ps = if ps.is_empty() { "void".to_string() } else { ps.join(", ") };
        let zero = if en.ret == Rep::Boxed { "LC_UNIT" } else { "0" };
        let _ = writeln!(f, "static {rt} FT{id}({ps}) {{\n    {rt} ret = {zero};\n    lc_v unw = LC_UNIT;\n    bool unwound = false;\n    (void)unw; (void)unwound;");
        f.push_str(&setup);
        f.push_str(&natives);
        for (i, r) in en.params.iter().enumerate() {
            match r {
                Rep::Boxed => {
                    let _ = writeln!(f, "    S[{i}] = P{i};");
                }
                _ => {
                    let _ = writeln!(f, "    N{i} = P{i};");
                }
            }
        }
        // The caller pushed this call's record (`lc_push`) and pops it.
        let _ = writeln!(f, "    ret = {body};\n    goto out;\nout:");
        match (&fd.ret, en.ret) {
            (Some(t), Rep::Boxed) => {
                let ty = self.ty(t);
                let _ = writeln!(f, "    if (unwound) ret = unw;");
                match exact_tag(t, "ret") {
                    Some(check) => {
                        let _ = writeln!(f, "    if (!{check}) ret = lc_coerce_ret(ret, {ty}, {fname}, {fsite});");
                    }
                    None => {
                        let _ = writeln!(f, "    ret = lc_coerce_ret(ret, {ty}, {fname}, {fsite});");
                    }
                }
            }
            (Some(t), r) => {
                let ty = self.ty(t);
                let unbox = match r {
                    Rep::Int => "u.i",
                    Rep::Float => "u.f",
                    _ => "u.i != 0",
                };
                let _ = writeln!(f, "    if (unwound) ret = lc_coerce_ret(unw, {ty}, {fname}, {fsite}).{unbox};");
                if let Ty::Int { min, max, .. } = t {
                    if *min != i64::MIN || *max != i64::MAX {
                        let _ = writeln!(f, "    if (ret < {min}LL || ret > {max}LL) lc_coerce_ret(lc_int(ret), {ty}, {fname}, {fsite});");
                    }
                }
            }
            (None, _) => {
                let _ = writeln!(f, "    if (unwound) ret = unw;");
            }
        }
        f.push_str(&teardown);
        f.push_str("    return ret;\n}\n\n");
        // The boxed entry, for calls through values and overloads.
        let _ = writeln!(f, "static lc_v F{id}(lc_v *a) {{\n    const char *cs = lc_callsite ? lc_callsite : \"\";\n    (void)cs;");
        let mut args = Vec::new();
        for (i, r) in en.params.iter().enumerate() {
            let v = self.check_param(fd, i, *r, &format!("a[{i}]"), true, "cs");
            let _ = writeln!(f, "    {} Q{i} = {v};", c_type(*r));
            args.push(format!("Q{i}"));
        }
        let _ = writeln!(f, "    int64_t d = lc_push({id}, lc_callsite);\n    {} r = FT{id}({});\n    lc_depth = d;", c_type(en.ret), args.join(", "));
        let _ = writeln!(f, "    return {};\n}}\n", box_c(en.ret, "r"));
        f
    }

    /// A direct call to `fns` that can go to a typed entry.
    fn direct_entry(&self, fns: &Rc<[FnId]>, args: &[Ex]) -> Option<Entry> {
        if fns.len() != 1 {
            return None;
        }
        let en = self.entries.get(fns[0] as usize)?.clone()?;
        (en.params.len() == args.len()).then_some(en)
    }

    /// A call to typed entry `FT{id}`, giving a value of `en.ret`'s rep.
    fn typed_call(&mut self, id: FnId, en: &Entry, args: &[Ex], span: Span) -> String {
        let site = self.site(span);
        let fd = &self.prog.fns[id as usize];
        let mut code = String::from("({ ");
        // Arguments in order, then their checks in order, as `F` does.
        let mut vals = Vec::new();
        for (a, r) in args.iter().zip(&en.params) {
            let t = self.t();
            // An int passed for an `f64` is converted, as the runtime does.
            let int_to_float = *r == Rep::Float && self.rep(a) == Rep::Int;
            let native = *r != Rep::Boxed && (self.rep(a) == *r || int_to_float);
            let v = match (native, r) {
                (true, Rep::Int) => self.int(a),
                (true, Rep::Float) => self.num(a),
                (true, _) => self.boolean(a),
                (false, _) => self.expr(a),
            };
            let ty = if native { c_type(*r) } else { "lc_v" };
            let _ = write!(code, "{ty} {t} = {v}; ");
            vals.push((t, native));
        }
        let mut pass = Vec::new();
        for (i, ((t, native), r)) in vals.iter().zip(&en.params).enumerate() {
            let v = self.check_param(fd, i, *r, t, !native, &site);
            if v == *t {
                pass.push(t.clone());
            } else {
                let q = self.t();
                let _ = write!(code, "{} {q} = {v}; ", c_type(*r));
                pass.push(q);
            }
        }
        let (d, r) = (self.t(), self.t());
        let _ = write!(code, "int64_t {d} = lc_push({id}, {site}); {} {r} = FT{id}({}); lc_depth = {d}; {r}; }})", c_type(en.ret), pass.join(", "));
        code
    }

    fn named_fn(&mut self, id: FnId) -> String {
        if let Some(en) = self.entries[id as usize].clone() {
            return self.typed_fn(id, &en);
        }
        let fd = &self.prog.fns[id as usize];
        // `mut` parameters are copied back out of their slots.
        let muts: Vec<u32> = fd.params.iter().enumerate().filter(|(_, p)| p.mode == Mode::Mut).map(|(i, _)| i as u32).collect();
        let (params, floats) = param_slots(fd);
        let reps = self.frame_reps(&fd.body, fd.nslots, &muts, &params, &floats);
        let tys = self.slot_tys(&fd.body);
        let saved = std::mem::replace(&mut self.cx, Cx::new(reps, tys));
        self.cx.viewable = self.viewable(&fd.body);
        let heap = has_lambda(&fd.body);
        let body = self.expr(&fd.body);
        let natives = self.native_decls();
        let param_reps: Vec<Rep> = (0..fd.params.len()).map(|i| self.slot_rep(i as u32)).collect();
        self.cx = saved;
        let (setup, teardown) = Self::frame_setup(fd.nslots, heap, "NULL");
        let fname = cstr(&fd.name);
        let fsite = self.site(fd.span);
        let mut f = String::new();
        f.push_str(Self::macros(Kind::Named));
        let _ = writeln!(f, "static lc_v F{id}(lc_v *a) {{");
        let _ = writeln!(f, "    const char *cs = lc_callsite;\n    (void)cs;\n    lc_v ret = LC_UNIT;");
        f.push_str(&setup);
        f.push_str(&natives);
        for (i, p) in fd.params.iter().enumerate() {
            let checked = if matches!(p.ty, Ty::Any | Ty::Param(_)) {
                format!("a[{i}]")
            } else {
                let ty = self.ty(&p.ty);
                let slow = format!("lc_coerce_arg(a[{i}], {ty}, {}, {fname}, cs ? cs : \"\")", cstr(&p.name));
                match exact_tag(&p.ty, &format!("a[{i}]")) {
                    Some(check) => format!("{check} ? a[{i}] : {slow}"),
                    None => slow,
                }
            };
            // A checked int or bool parameter is unboxed as it is.
            match param_reps[i] {
                Rep::Int => {
                    let _ = writeln!(f, "    N{i} = ({checked}).u.i;");
                }
                Rep::Bool => {
                    let _ = writeln!(f, "    N{i} = ({checked}).u.i != 0;");
                }
                Rep::Float => {
                    let _ = writeln!(f, "    N{i} = ({checked}).u.f;");
                }
                Rep::Boxed => {
                    let _ = writeln!(f, "    S[{i}] = {checked};");
                }
            }
        }
        let _ = writeln!(f, "    lc_enter({id});");
        let _ = writeln!(f, "    ret = {body};\n    goto out;\nout:\n    lc_leave();");
        if let Some(t) = &fd.ret {
            let ty = self.ty(t);
            match exact_tag(t, "ret") {
                Some(check) => {
                    let _ = writeln!(f, "    if (!{check}) ret = lc_coerce_ret(ret, {ty}, {fname}, {fsite});");
                }
                None => {
                    let _ = writeln!(f, "    ret = lc_coerce_ret(ret, {ty}, {fname}, {fsite});");
                }
            }
        }
        for (i, p) in fd.params.iter().enumerate() {
            if p.mode == Mode::Mut {
                let _ = writeln!(f, "    a[{i}] = lc_take(&S[{i}]);");
            }
        }
        f.push_str(&teardown);
        f.push_str("    return ret;\n}\n\n");
        f
    }

    /// Calls a user function through a value: picks the overload, fills in
    /// defaults.
    fn dispatcher(&mut self, ids: &Rc<[FnId]>) -> String {
        let mut f = format!("static lc_v G{}(lc_fn *self, int argc, lc_v *args, const char *site) {{\n    (void)self;\n    int k = -1;\n", ids[0]);
        if ids.len() == 1 {
            let _ = writeln!(f, "    k = {};", ids[0]);
        } else {
            for &id in ids.iter() {
                let fd = &self.prog.fns[id as usize];
                let req = fd.params.iter().filter(|p| p.default.is_none()).count();
                let first = match fd.params.first() {
                    Some(p) => format!("argc > 0 && lc_ty_matches(args[0], {})", self.ty(&p.ty)),
                    None => "argc == 0".to_string(),
                };
                let _ = writeln!(f, "    if (k < 0 && argc >= {req} && argc <= {} && {first}) k = {id};", fd.params.len());
            }
            let name = cstr(&self.prog.fns[ids[0] as usize].name);
            let _ = writeln!(
                f,
                "    if (k < 0) {{ char kb[64]; lc_panic(\"E0301\", site, \"no `%s` takes %s\", {name}, argc ? lc_kind(args[0], kb) : \"no arguments\"); }}"
            );
        }
        f.push_str("    switch (k) {\n");
        for &id in ids.iter() {
            let fd = &self.prog.fns[id as usize];
            let np = fd.params.len();
            let req = fd.params.iter().filter(|p| p.default.is_none()).count();
            let _ = writeln!(f, "    case {id}: {{");
            let _ = writeln!(
                f,
                "        if (argc < {req} || argc > {np}) lc_panic(\"E0206\", site, \"`%s` takes {np} argument(s), got %d: %s\", {}, argc, {});",
                cstr(&fd.name),
                cstr(&fd.sig)
            );
            let _ = writeln!(f, "        lc_v A[{}];", np.max(1));
            let _ = writeln!(f, "        for (int i = 0; i < argc; i++) A[i] = lc_retain(args[i]);");
            for (i, p) in fd.params.iter().enumerate() {
                if p.default.is_some() {
                    let _ = writeln!(f, "        if (argc <= {i}) A[{i}] = D{id}_{i}();");
                }
            }
            let _ = writeln!(f, "        lc_callsite = site;\n        lc_v r = F{id}(A);");
            for (i, p) in fd.params.iter().enumerate() {
                if p.mode == Mode::Mut {
                    let _ = writeln!(f, "        lc_release(A[{i}]);");
                }
            }
            f.push_str("        return r;\n    }\n");
        }
        f.push_str("    }\n    return LC_UNIT;\n}\n\n");
        f
    }

    fn ctor_fn(&mut self, e: u32, t: u32) -> String {
        let ed = &self.prog.enums[e as usize];
        let vd = &ed.variants[t as usize];
        let n = vd.fields.len();
        let mut f = format!("static lc_v C{e}_{t}(lc_fn *self, int argc, lc_v *args, const char *site) {{\n    (void)self;\n");
        let _ = writeln!(
            f,
            "    if (argc != {n}) lc_panic(\"E0206\", site, \"`%s` takes {n} field(s), got %d\", {}, argc);",
            cstr(&vd.name)
        );
        let _ = writeln!(f, "    lc_v A[{}];", n.max(1));
        for (i, ft) in vd.fields.iter().enumerate() {
            let ty = self.ty(ft);
            let _ = writeln!(f, "    A[{i}] = lc_coerce_field(lc_retain(args[{i}]), {ty}, \"\", {}, true, site);", cstr(&vd.name));
        }
        let _ = writeln!(f, "    return lc_rec_new(T_VARIANT, {e}, {t}, {n}, A);\n}}\n");
        f
    }

    // ----- slots and places -----

    fn slot(&mut self, depth: u32, s: u32) -> String {
        if depth == 0 {
            format!("S[{s}]")
        } else {
            self.cx.max_up = self.cx.max_up.max(depth);
            format!("P{depth}->slots[{s}]")
        }
    }

    /// The boxed storage of a place's root. Never an unboxed slot: those
    /// are never the root of a longer place or changed by pointer.
    fn root(&mut self, r: &Root) -> String {
        match r {
            Root::Local(s) => {
                debug_assert_eq!(self.slot_rep(*s), Rep::Boxed);
                self.slot(0, *s)
            }
            Root::Up(d, s) => self.slot(*d, *s),
        }
    }

    /// Evaluates the index keys of a place, in order. Returns the statements
    /// and the key temporaries.
    fn place_keys(&mut self, p: &Place) -> (String, Vec<String>) {
        self.place_keys_to(p, p.path.len())
    }

    /// `place_keys` for the steps before `upto`.
    fn place_keys_to(&mut self, p: &Place, upto: usize) -> (String, Vec<String>) {
        let mut code = String::new();
        let mut keys = Vec::new();
        for seg in p.path.iter().take(upto) {
            if let Seg::Index(e, _) = seg {
                let k = self.t();
                let v = self.expr(e);
                let _ = write!(code, "lc_v {k} = {v}; ");
                keys.push(k);
            }
        }
        (code, keys)
    }

    /// Statements that point `ptr` at a place, creating missing map entries
    /// as `viv` says for the last step (`zero` and `method` go with it).
    fn place_walk(&mut self, p: &Place, keys: &[String], ptr: &str, viv: &str, zero: &str, method: usize) -> String {
        self.place_walk_to(p, keys, ptr, viv, zero, method, p.path.len())
    }

    /// `place_walk` that stops before step `upto`, pointing `ptr` at what
    /// that step goes into.
    #[allow(clippy::too_many_arguments)]
    fn place_walk_to(&mut self, p: &Place, keys: &[String], ptr: &str, viv: &str, zero: &str, method: usize, upto: usize) -> String {
        let root = self.root(&p.root);
        let mut code = format!("lc_v *{ptr} = &{root}; ");
        let mut ki = 0;
        let n = p.path.len();
        for (i, seg) in p.path.iter().enumerate().take(upto) {
            match seg {
                Seg::Field(name, sp) => {
                    let fid = self.field_id(name);
                    let site = self.site(*sp);
                    let _ = write!(code, "{ptr} = lc_place_field({ptr}, {fid}, {}, {site}); ", cstr(name));
                }
                Seg::Index(_, sp) => {
                    let site = self.site(*sp);
                    let this_viv = if i + 1 == n {
                        viv.to_string()
                    } else if viv == "VIV_NO" {
                        "VIV_NO".to_string()
                    } else {
                        match &p.path[i + 1] {
                            Seg::Index(..) => "VIV_MAP".to_string(),
                            Seg::Field(..) => "VIV_NO".to_string(),
                        }
                    };
                    let (z, m) = if i + 1 == n { (zero.to_string(), method) } else { ("LC_UNIT".to_string(), 0) };
                    let _ = write!(code, "{ptr} = lc_place_index_fast({ptr}, {}, {this_viv}, {z}, {m}, {site}); ", keys[ki]);
                    ki += 1;
                }
            }
        }
        code
    }

    /// Statements that store the owned `lc_v` named `v` into place `p`,
    /// evaluating its keys. A last index step stores into the container,
    /// so a packed list stays packed. A value stored in a struct field, or
    /// in a variable declared with a type, takes that type, or is reported
    /// at `vsite`; `exact` says the checker proved it already has it.
    fn store_place(&mut self, p: &Place, v: &str, vsite: &str, exact: bool) -> String {
        let (kc, keys) = self.place_keys(p);
        let ptr = self.t();
        let rk = Self::release_all(&keys);
        match p.path.last() {
            Some(Seg::Index(_, sp)) => {
                let lsite = self.site(*sp);
                let walk = self.place_walk_to(p, &keys, &ptr, "VIV_INSERT", "LC_UNIT", 0, p.path.len() - 1);
                format!("{{ {kc}{walk}lc_store_index({ptr}, {}, {v}, {lsite}); {rk}}} ", keys[keys.len() - 1])
            }
            Some(Seg::Field(name, sp)) => {
                let (fsite, fid) = (self.site(*sp), self.field_id(name));
                let walk = self.place_walk_to(p, &keys, &ptr, "VIV_INSERT", "LC_UNIT", 0, p.path.len() - 1);
                format!("{{ {kc}{walk}lc_store_field({ptr}, {fid}, {}, {v}, {fsite}, {vsite}); {rk}}} ", cstr(name))
            }
            None => {
                let walk = self.place_walk(p, &keys, &ptr, "VIV_INSERT", "LC_UNIT", 0);
                let v = if exact { v.to_string() } else { self.conform_var(p, v, vsite) };
                format!("{{ {kc}{walk}lc_set({ptr}, {v}); {rk}}} ")
            }
        }
    }

    /// Statements for `p op= v`, consuming the owned `lc_v` named `v`, with
    /// arithmetic errors reported at `site`; as `store_place` for the steps
    /// and declared types.
    fn update_place(&mut self, p: &Place, op: BinOp, v: &str, site: &str) -> String {
        let (kc, keys) = self.place_keys(p);
        let ptr = self.t();
        let rk = Self::release_all(&keys);
        let opc = binop_c(op);
        match p.path.last() {
            Some(Seg::Index(_, sp)) => {
                let lsite = self.site(*sp);
                let walk = self.place_walk_to(p, &keys, &ptr, "VIV_ZERO_OF", "LC_UNIT", 0, p.path.len() - 1);
                format!("{{ {kc}{walk}lc_update_index({ptr}, {}, {opc}, {v}, {lsite}, {site}); {rk}}} ", keys[keys.len() - 1])
            }
            Some(Seg::Field(name, sp)) => {
                let (fsite, fid) = (self.site(*sp), self.field_id(name));
                let walk = self.place_walk_to(p, &keys, &ptr, "VIV_ZERO_OF", "LC_UNIT", 0, p.path.len() - 1);
                format!("{{ {kc}{walk}lc_update_field({ptr}, {fid}, {}, {opc}, {v}, {fsite}, {site}); {rk}}} ", cstr(name))
            }
            None => {
                let walk = self.place_walk(p, &keys, &ptr, "VIV_ZERO_OF", v, 0);
                let old = self.t();
                let new = self.conform_var(p, &format!("lc_arith_own({opc}, {old}, {v}, {site})"), site);
                format!("{{ {kc}{walk}lc_v {old} = lc_take({ptr}); *{ptr} = {new}; {rk}}} ")
            }
        }
    }

    /// The owned `lc_v` `v`, stored in variable `p` itself, converted to
    /// the variable's declared type or reported at `site`.
    fn conform_var(&mut self, p: &Place, v: &str, site: &str) -> String {
        match &p.decl {
            Some(t) if !matches!(t, Ty::Any | Ty::Param(_)) => {
                let ty = self.ty(t);
                format!("lc_coerce_var({v}, {ty}, {}, {site})", cstr(&p.name))
            }
            _ => v.to_string(),
        }
    }

    /// For an unboxed int `x` stored where a sized int type `t` is
    /// declared: a statement reporting it at `site` when out of range.
    /// `name` is the variable's, or `None` for the binding declaring it.
    fn sized_check(&mut self, t: &Ty, x: &str, name: Option<&str>, site: &str) -> String {
        match t {
            Ty::Int { min, max, .. } if *min != i64::MIN || *max != i64::MAX => {
                let ty = self.ty(t);
                let name = name.map_or("NULL".to_string(), cstr);
                format!("if ({x} < {min}LL || {x} > {max}LL) lc_coerce_var(lc_int({x}), {ty}, {name}, {site}); ")
            }
            _ => String::new(),
        }
    }

    /// `xs[i] = v` and `xs[i] op= v` with an unboxed index and value: done
    /// in place when the list is packed as the element's kind, else as
    /// `store_place` and `update_place` do. `None` when the place or the
    /// value doesn't suit.
    fn assign_elem(&mut self, place: &Place, op: Option<BinOp>, value: &Ex, site: &str) -> Option<String> {
        let Some(Seg::Index(last, lsp)) = place.path.last() else { return None };
        if !self.typed || self.rep(last) != Rep::Int {
            return None;
        }
        let vrep = self.rep(value);
        let fop = |op: BinOp, x: &str, y: &str| -> Option<String> {
            Some(match op {
                BinOp::Add => format!("{x} + {y}"),
                BinOp::Sub => format!("{x} - {y}"),
                BinOp::Mul => format!("{x} * {y}"),
                BinOp::Div => format!("{x} / {y}"),
                BinOp::Rem => format!("fmod({x}, {y})"),
                BinOp::Pow => format!("pow({x}, {y})"),
                _ => return None,
            })
        };
        let (v, k, ptr, e, f) = (self.t(), self.t(), self.t(), self.t(), self.t());
        // The element's new value from its old one `*e` and the value `v`,
        // then a second try for an int into a list of floats.
        let (at, new, also) = match (op, vrep) {
            (None, Rep::Int) => ("lc_int_at", v.clone(), None),
            (None, Rep::Bool) => ("lc_bool_at", v.clone(), None),
            (None, Rep::Float) => ("lc_float_at", v.clone(), None),
            (Some(op), Rep::Int) => {
                let iop = int_op(op)?;
                let new = if iop.len() == 1 { format!("*{e} {iop} {v}") } else { format!("{iop}(*{e}, {v}, {site})") };
                ("lc_int_at", new, fop(op, &format!("*{f}"), &format!("(double){v}")))
            }
            (Some(op), Rep::Float) => ("lc_float_at", fop(op, &format!("*{e}"), &v)?, None),
            _ => return None,
        };
        let n = place.path.len();
        let lsite = self.site(*lsp);
        let vv = match vrep {
            Rep::Int => self.int(value),
            Rep::Bool => self.boolean(value),
            _ => self.float(value),
        };
        let (kc, keys) = self.place_keys_to(place, n - 1);
        let kv = self.int(last);
        let viv = if op.is_none() { "VIV_INSERT" } else { "VIV_ZERO_OF" };
        let walk = self.place_walk_to(place, &keys, &ptr, viv, "LC_UNIT", 0, n - 1);
        let rk = Self::release_all(&keys);
        let ct = c_type(vrep);
        let boxed = box_c(vrep, &v);
        let slow = match op {
            None => format!("lc_store_index({ptr}, lc_int({k}), {boxed}, {lsite})"),
            Some(op) => format!("lc_update_index({ptr}, lc_int({k}), {}, {boxed}, {lsite}, {site})", binop_c(op)),
        };
        let (fdecl, also) = match also {
            Some(x) => (format!("double *{f}; "), format!("else if (({f} = lc_float_at({ptr}, {k})) != NULL) *{f} = {x}; ")),
            None => (String::new(), String::new()),
        };
        let view = match (&place.root, n) {
            (Root::Local(s), 1) => self.view_of(*s),
            _ => None,
        };
        let reread = self.view_reread(place);
        match view {
            // The view's bounds test, then its element: a pointer the
            // C compiler can't prove non-null would cost a second test.
            Some((_, w, kind)) if kind == vrep => {
                let j = self.t();
                Some(format!(
                    "{{ {ct} {v} = {vv}; {kc}int64_t {k} = {kv}; {walk}uint64_t {j} = lc_pos({k}, {w}.wlen); {ct} *{e}; if ({j} < (uint64_t){w}.wlen) {{ {e} = ({ct} *){w}.data + {j}; *{e} = {new}; }} else {{ {e} = {at}({ptr}, {k}); {fdecl}if ({e}) *{e} = {new}; {also}else {slow}; {reread}}} {rk}}} "
                ))
            }
            _ => Some(format!(
                "{{ {ct} {v} = {vv}; {kc}int64_t {k} = {kv}; {walk}{ct} *{e} = {at}({ptr}, {k}); {fdecl}if ({e}) *{e} = {new}; {also}else {slow}; {reread}{rk}}} "
            )),
        }
    }

    fn release_all(names: &[String]) -> String {
        names.iter().map(|k| format!("lc_release({k}); ")).collect()
    }

    /// Reads a place: the root, then each field or element.
    fn read_place(&mut self, p: &Place) -> String {
        if let (Root::Local(s), true) = (&p.root, p.path.is_empty()) {
            match self.slot_rep(*s) {
                Rep::Boxed => {}
                r => return box_c(r, &format!("N{s}")),
            }
        }
        let root = self.root(&p.root);
        let cur = self.t();
        let mut code = format!("({{ lc_v {cur} = lc_retain({root}); ");
        for seg in &p.path {
            let next = self.t();
            match seg {
                Seg::Field(name, sp) => {
                    let fid = self.field_id(name);
                    let site = self.site(*sp);
                    let mid = method_id(name).map_or(-1, |m| m as i64);
                    let found = self.t();
                    let _ = write!(
                        code,
                        "bool {found}; lc_v {next} = lc_field({cur}, {fid}, {n}, &{found}, {site}); if (!{found}) {next} = lc_field_fallback({cur}, {n}, {mid}, {site}); if (lc_unwinding) UNWIND; lc_release({cur}); {cur} = {next}; ",
                        n = cstr(name)
                    );
                }
                Seg::Index(e, sp) => {
                    let site = self.site(*sp);
                    let k = self.t();
                    let v = self.expr(e);
                    let _ = write!(code, "lc_v {k} = {v}; lc_v {next} = lc_index_fast({cur}, {k}, {site}); lc_release({k}); lc_release({cur}); {cur} = {next}; ");
                }
            }
        }
        let _ = write!(code, "{cur}; }})");
        code
    }

    // ----- expressions -----

    fn args_into(&mut self, arr: &str, args: &[Ex]) -> String {
        let mut code = format!("lc_v {arr}[{}]; ", args.len().max(1));
        for (i, a) in args.iter().enumerate() {
            let v = self.expr(a);
            let _ = write!(code, "{arr}[{i}] = {v}; ");
        }
        code
    }

    fn release_arr(arr: &str, n: usize) -> String {
        if n == 0 {
            String::new()
        } else {
            format!("for (int _i = 0; _i < {n}; _i++) lc_release({arr}[_i]); ")
        }
    }

    fn lit(&mut self, v: &Value) -> String {
        match v {
            Value::Unit => "LC_UNIT".into(),
            Value::None => "LC_NONE".into(),
            Value::Bool(b) => if *b { "LC_TRUE" } else { "LC_FALSE" }.into(),
            Value::Int(n) => {
                if *n == i64::MIN {
                    "lc_int(INT64_MIN)".into()
                } else {
                    format!("lc_int({n}LL)")
                }
            }
            Value::Float(f) => {
                if f.is_nan() {
                    "lc_float(NAN)".into()
                } else if f.is_infinite() {
                    if *f > 0.0 { "lc_float(INFINITY)" } else { "lc_float(-INFINITY)" }.into()
                } else {
                    format!("lc_float({f:?})")
                }
            }
            Value::Str(s) => self.str_const(s),
            Value::Func(f) => match &**f {
                Func::User(ids) => {
                    let g = self.group(ids);
                    format!("lc_fnval_new(FN_USER, {}, 0, {g})", ids[0])
                }
                Func::Ctor(e, t) => {
                    let c = self.ctor(*e, *t);
                    format!("lc_fnval_new(FN_CTOR, {e}, {t}, {c})")
                }
                Func::Method(name) => match lacon_interp::resolve::conv_for(name) {
                    Some(to) => format!("lc_fnval_new(FN_CONV, {}, 0, NULL)", conv_index(to)),
                    None => format!("lc_fnval_new(FN_METHOD, {}, 0, NULL)", method_id(name).unwrap_or(0)),
                },
                Func::Closure(..) => "LC_UNIT".into(),
            },
            _ => "LC_UNIT".into(),
        }
    }

    /// `e` as an owned `lc_v`.
    fn expr(&mut self, e: &Ex) -> String {
        match self.rep(e) {
            Rep::Int if self.int_native(e) => {
                let c = self.int(e);
                format!("lc_int({c})")
            }
            Rep::Bool if self.bool_native(e) => {
                let c = self.boolean(e);
                format!("lc_bool({c})")
            }
            Rep::Float if self.float_native(e) => {
                let c = self.float(e);
                format!("lc_float({c})")
            }
            _ => self.expr_boxed(e),
        }
    }

    /// Can `float` compute `e` (proven a float) without a boxed value?
    fn float_native(&self, e: &Ex) -> bool {
        match e {
            Ex::Lit(Value::Float(_), _) | Ex::Binary { .. } | Ex::Unary { op: UnOp::Neg, .. } => true,
            Ex::Local(s, _) => self.slot_rep(*s) == Rep::Float,
            Ex::If { els: Some(_), .. } | Ex::Block(_, Some(_)) | Ex::Match { .. } | Ex::CallFn { .. } => true,
            Ex::CallBuiltin { f: Builtin::Conv(ConvTo::Float), args, .. } => matches!(self.rep(&args[0]), Rep::Int | Rep::Float),
            Ex::Field { obj, name, user: None, .. } => self.float_field(obj, name).is_some(),
            Ex::Method { recv: Recv::Value(x), name, args, .. } => args.is_empty() && matches!(&**name, "sqrt" | "abs") && self.rep(x) == Rep::Float,
            Ex::Method { recv: Recv::Place(Place { root: Root::Local(s), path, .. }), name, args, .. } => {
                args.is_empty() && matches!(&**name, "sqrt" | "abs") && path.is_empty() && self.slot_rep(*s) == Rep::Float
            }
            _ => false,
        }
    }

    /// `e`, proven a float, as a C `double`.
    fn float(&mut self, e: &Ex) -> String {
        if self.rep(e) != Rep::Float || !self.float_native(e) {
            let site = self.site(e.span());
            let v = self.expr(e);
            return format!("lc_unbox_float({v}, {site})");
        }
        match e {
            Ex::Lit(Value::Float(f), _) => float_c(*f),
            Ex::Local(s, _) => format!("N{s}"),
            Ex::Binary { op, l, r, .. } => {
                let (a, b) = (self.num(l), self.num(r));
                let apply = |x: &str, y: &str| match op {
                    BinOp::Add => format!("({x} + {y})"),
                    BinOp::Sub => format!("({x} - {y})"),
                    BinOp::Mul => format!("({x} * {y})"),
                    BinOp::Div => format!("({x} / {y})"),
                    BinOp::Rem => format!("fmod({x}, {y})"),
                    _ => format!("pow({x}, {y})"),
                };
                if self.simple(l) || self.simple(r) {
                    apply(&a, &b)
                } else {
                    let (x, y) = (self.t(), self.t());
                    format!("({{ double {x} = {a}; double {y} = {b}; {}; }})", apply(&x, &y))
                }
            }
            Ex::Unary { e: x, .. } => format!("(-{})", self.float(x)),
            Ex::If { cond, then, els: Some(els), span } => {
                let site = self.site(if cond.span() == Span::default() { *span } else { cond.span() });
                let c = self.cond_at(cond, &site);
                let (t, f) = (self.float(then), self.float(els));
                format!("({c} ? {t} : {f})")
            }
            Ex::Block(stmts, Some(tail)) => {
                let mut code = String::from("({ ");
                for s in stmts {
                    code.push_str(&self.stmt(s));
                }
                let r = self.t();
                let tv = self.float(tail);
                let _ = write!(code, "double {r} = {tv}; {r}; }})");
                code
            }
            Ex::Match { scrut, arms, span } => self.match_native(scrut, arms, *span, Rep::Float),
            Ex::CallFn { fns, args, span, .. } => {
                let en = self.direct_entry(fns, args).unwrap();
                self.typed_call(fns[0], &en, args, *span)
            }
            Ex::CallBuiltin { args, .. } => self.num(&args[0]),
            Ex::Field { obj, name, span, .. } => self.field_scalar(obj, name, *span, Rep::Float),
            Ex::Method { recv, name, .. } => {
                let x = match recv {
                    Recv::Value(x) => self.float(x),
                    Recv::Place(Place { root: Root::Local(s), .. }) => format!("N{s}"),
                    Recv::Place(_) => unreachable!(),
                };
                if &**name == "sqrt" {
                    format!("sqrt({x})")
                } else {
                    format!("fabs({x})")
                }
            }
            _ => unreachable!(),
        }
    }

    /// `e`, typed int or `f64`, as a C `double`, as the runtime's float
    /// operators convert their operands.
    fn num(&mut self, e: &Ex) -> String {
        match (self.rep(e), e) {
            (Rep::Float, _) => self.float(e),
            (Rep::Int, _) => format!("((double){})", self.int(e)),
            (_, Ex::Index { obj, index, span }) if self.numeric(e) && self.rep(index) == Rep::Int => self.elem(obj, index, *span, Rep::Float),
            _ => {
                let site = self.site(e.span());
                let v = self.expr(e);
                format!("lc_num({v}, {site})")
            }
        }
    }

    /// Can `int` compute `e` without going through a boxed value?
    fn int_native(&self, e: &Ex) -> bool {
        match e {
            Ex::Lit(Value::Int(_), _) => true,
            Ex::Local(s, _) => self.slot_rep(*s) == Rep::Int,
            Ex::Binary { op, l, r, .. } => int_op(*op).is_some() && self.rep(l) == Rep::Int && self.rep(r) == Rep::Int,
            Ex::Unary { op: UnOp::Neg | UnOp::Bang, e: x, .. } => self.rep(x) == Rep::Int,
            Ex::If { els: Some(_), .. } | Ex::Block(_, Some(_)) | Ex::Match { .. } => true,
            Ex::CallFn { fns, args, .. } => self.direct_entry(fns, args).is_some_and(|en| en.ret == Rep::Int),
            Ex::Field { obj, name, user: None, .. } => self.is_len(e) || self.scalar_field(obj, name).is_some_and(|f| f.2 == Rep::Int),
            Ex::Field { .. } | Ex::Method { .. } => self.is_len(e),
            Ex::Index { index, .. } => self.rep(index) == Rep::Int,
            _ => false,
        }
    }

    /// Can `boolean` compute `e` without going through a boxed value?
    fn bool_native(&self, e: &Ex) -> bool {
        match e {
            Ex::Lit(Value::Bool(_), _) => true,
            Ex::Local(s, _) => self.slot_rep(*s) == Rep::Bool,
            Ex::Compare { .. } | Ex::And(..) | Ex::Or(..) => true,
            Ex::Field { obj, name, user: None, .. } => self.scalar_field(obj, name).is_some_and(|f| f.2 == Rep::Bool),
            Ex::Unary { op: UnOp::Not | UnOp::Bang, e: x, .. } => self.rep(x) == Rep::Bool,
            Ex::If { els: Some(_), .. } | Ex::Block(_, Some(_)) | Ex::Match { .. } => true,
            Ex::CallFn { fns, args, .. } => self.direct_entry(fns, args).is_some_and(|en| en.ret == Rep::Bool),
            Ex::Index { index, .. } => self.rep(index) == Rep::Int,
            _ => false,
        }
    }

    /// An expression with no side effects that can't fail, so the order
    /// it is evaluated in doesn't matter.
    fn simple(&self, e: &Ex) -> bool {
        match e {
            Ex::Lit(Value::Int(_) | Value::Bool(_) | Value::Float(_), _) => true,
            Ex::Local(s, _) => self.slot_rep(*s) != Rep::Boxed,
            _ => false,
        }
    }

    /// Does `e` already have declared type `ty` exactly, so a boundary check
    /// would neither convert nor reject it?
    fn exact(&self, ty: &Ty, e: &Ex) -> bool {
        if !self.typed {
            return false;
        }
        match (ty, self.types.of(e)) {
            (Ty::Int { min, max, .. }, Some(T::Int(_))) => *min == i64::MIN && *max == i64::MAX,
            (Ty::Bool, Some(T::Bool)) | (Ty::Str, Some(T::Str)) => true,
            (Ty::Float, _) => self.rep(e) == Rep::Float,
            (Ty::Struct(a, _), Some(T::Struct(b, _))) | (Ty::Enum(a, _), Some(T::Enum(b, _))) => a == b,
            _ => false,
        }
    }

    /// The struct and position of field `name` of `obj`, when the checker
    /// knows `obj` is a struct with that field.
    fn struct_field(&self, obj: &Ex, name: &str) -> Option<(u32, usize)> {
        if !self.typed {
            return None;
        }
        let Some(T::Struct(id, _)) = self.types.of(obj) else { return None };
        let k = self.prog.structs[*id as usize].fields.iter().position(|f| f.name == name)?;
        Some((*id, k))
    }

    /// `obj.name`, a field declared `f64`, an int type or `bool`, as a C
    /// scalar of `rep`. `xs[i].f` reads the element in place, through the
    /// view an enclosing loop took of `xs` or from the borrowed list.
    fn field_scalar(&mut self, obj: &Ex, name: &str, span: Span, rep: Rep) -> String {
        let (id, k, _) = self.scalar_field(obj, name).unwrap_or((0, 0, rep));
        let site = self.site(span);
        let (ct, unbox) = match rep {
            Rep::Float => ("double", "lc_unbox_float"),
            Rep::Int => ("int64_t", "lc_unbox_int"),
            _ => ("bool", "lc_unbox_bool"),
        };
        // On a list of these structs, the general read can only end in an
        // error (an index out of range, or a checker bug): written so, the
        // C compiler knows nothing else happens there, and can share the
        // checks between reads.
        let list = matches!(obj, Ex::Index { obj: lst, .. } if matches!(self.types.of(lst), Some(T::List(_))));
        if let (Ex::Index { obj: lst, index, .. }, true) = (obj, list) {
            if self.rep(index) == Rep::Int && self.pure(index) {
                if let Some((_, w, Rep::Boxed)) = self.view(lst) {
                    let i = self.int(index);
                    let (j, el, r) = (self.t(), self.t(), self.t());
                    let slow = self.field_scalar_read(obj, name, id, k, ct, unbox, &site);
                    return format!(
                        "({{ uint64_t {j} = lc_pos({i}, {w}.len); lc_v *{el}; {ct} {r}; if ({j} < (uint64_t){w}.len && ({el} = (lc_v *){w}.data + {j})->tag == T_STRUCT && REC(*{el})->ty == {id}) {r} = {unbox}(REC(*{el})->f[{k}], {site}); else {{ (void){slow}; lc_unbox_fail(LC_UNIT, {}, {site}); }} {r}; }})",
                        cstr(&format!("a struct with field `{name}`"))
                    );
                }
                if let Some(l) = self.borrowed(lst) {
                    let i = self.int(index);
                    let (o, v, j, r) = (self.t(), self.t(), self.t(), self.t());
                    let slow = self.field_scalar_read(obj, name, id, k, ct, unbox, &site);
                    return format!(
                        "({{ lc_v {o} = {l}; lc_vec *{v}; uint64_t {j}; {ct} {r}; if ({o}.tag == T_LIST && ({v} = VEC({o}))->kind == K_BOXED && ({j} = lc_pos({i}, {v}->len)) < (uint64_t){v}->len && {v}->boxed[{j}].tag == T_STRUCT && REC({v}->boxed[{j}])->ty == {id}) {r} = {unbox}(REC({v}->boxed[{j}])->f[{k}], {site}); else {{ (void){slow}; lc_unbox_fail(LC_UNIT, {}, {site}); }} {r}; }})",
                        cstr(&format!("a struct with field `{name}`"))
                    );
                }
            }
        }
        self.field_scalar_read(obj, name, id, k, ct, unbox, &site)
    }

    /// `field_scalar` for any `obj`: in place when it is struct `id`, else
    /// as the boxed read does it.
    #[allow(clippy::too_many_arguments)]
    fn field_scalar_read(&mut self, obj: &Ex, name: &str, id: u32, k: usize, ct: &str, unbox: &str, site: &str) -> String {
        let (o, r, found, b) = (self.t(), self.t(), self.t(), self.t());
        let fid = self.field_id(name);
        let mid = method_id(name).map_or(-1, |m| m as i64);
        let (init, release) = match self.borrowed(obj) {
            Some(b) => (b, String::new()),
            None => (self.expr(obj), format!("lc_release({o}); ")),
        };
        format!(
            "({{ lc_v {o} = {init}; {ct} {r}; if ({o}.tag == T_STRUCT && REC({o})->ty == {id}) {r} = {unbox}(REC({o})->f[{k}], {site}); else {{ bool {found}; lc_v {b} = lc_field({o}, {fid}, {n}, &{found}, {site}); if (!{found}) {{ {b} = lc_field_fallback({o}, {n}, {mid}, {site}); if (lc_unwinding) UNWIND; }} {r} = {unbox}({b}, {site}); }} {release}{r}; }})",
            n = cstr(name)
        )
    }

    /// `p.f = v` and `p.f op= v` where the checker knows the struct, `f` is
    /// declared `f64` or `int`, and the value is unboxed: done in place when
    /// nothing else holds the struct, else as `store_place` and
    /// `update_place` do. `None` when the place or the value doesn't suit.
    fn assign_field(&mut self, place: &Place, op: Option<BinOp>, value: &Ex, site: &str) -> Option<String> {
        let Some(Seg::Field(name, fsp)) = place.path.last() else { return None };
        if !self.typed {
            return None;
        }
        let n = place.path.len();
        let Some(T::Struct(id, _)) = self.place_type(place, n - 1) else { return None };
        let k = self.prog.structs[id as usize].fields.iter().position(|f| *f.name == **name)?;
        let float = match (&self.prog.structs[id as usize].fields[k].ty, self.rep(value)) {
            (Ty::Float, Rep::Float | Rep::Int) => true,
            (Ty::Int { min, max, .. }, Rep::Int) if *min == i64::MIN && *max == i64::MAX => false,
            _ => return None,
        };
        let (v, ptr, f) = (self.t(), self.t(), self.t());
        let new = match (op, float) {
            (None, _) => v.clone(),
            (Some(op), true) => match op {
                BinOp::Add => format!("{f}->u.f + {v}"),
                BinOp::Sub => format!("{f}->u.f - {v}"),
                BinOp::Mul => format!("{f}->u.f * {v}"),
                BinOp::Div => format!("{f}->u.f / {v}"),
                BinOp::Rem => format!("fmod({f}->u.f, {v})"),
                BinOp::Pow => format!("pow({f}->u.f, {v})"),
                _ => return None,
            },
            (Some(op), false) => {
                let iop = int_op(op)?;
                if iop.len() == 1 {
                    format!("{f}->u.i {iop} {v}")
                } else {
                    format!("{iop}({f}->u.i, {v}, {site})")
                }
            }
        };
        let (ct, member, tag, rep) = if float { ("double", "f", "T_FLOAT", Rep::Float) } else { ("int64_t", "i", "T_INT", Rep::Int) };
        let vv = if float { self.num(value) } else { self.int(value) };
        let viv = if op.is_none() { "VIV_INSERT" } else { "VIV_ZERO_OF" };
        let (fsite, fid) = (self.site(*fsp), self.field_id(name));
        let boxed = box_c(rep, &v);
        let slow = match op {
            None => {
                let vsite = self.site(if value.span() == Span::default() { place.span } else { value.span() });
                format!("lc_store_field({ptr}, {fid}, {}, {boxed}, {fsite}, {vsite})", cstr(name))
            }
            Some(op) => format!("lc_update_field({ptr}, {fid}, {}, {}, {boxed}, {fsite}, {site})", cstr(name), binop_c(op)),
        };
        // `xs[i].f` through the view an enclosing loop took of `xs`: the
        // element in place when nothing else holds the list or it.
        if let (Root::Local(s), [Seg::Index(ix, _), Seg::Field(..)]) = (&place.root, &place.path[..]) {
            if let Some((s, w, Rep::Boxed)) = self.view_of(*s).filter(|_| self.rep(ix) == Rep::Int) {
                let (kk, j, el) = (self.t(), self.t(), self.t());
                let iv = self.int(ix);
                let walk = self.place_walk_to(place, &[format!("lc_int({kk})")], &ptr, viv, "LC_UNIT", 0, n - 1);
                let reread = self.view_read(s, Rep::Boxed);
                return Some(format!(
                    "{{ {ct} {v} = {vv}; int64_t {kk} = {iv}; uint64_t {j} = lc_pos({kk}, {w}.wlen); lc_v *{el}; if ({j} < (uint64_t){w}.wlen && ({el} = (lc_v *){w}.data + {j})->tag == T_STRUCT && {el}->u.o->rc == 1 && REC(*{el})->ty == {id} && REC(*{el})->f[{k}].tag == {tag}) {{ lc_v *{f} = &REC(*{el})->f[{k}]; {f}->u.{member} = {new}; }} else {{ {walk}{slow}; {w} = {reread}; }} }} "
                ));
            }
        }
        let (kc, keys) = self.place_keys_to(place, n - 1);
        let walk = self.place_walk_to(place, &keys, &ptr, viv, "LC_UNIT", 0, n - 1);
        let rk = Self::release_all(&keys);
        let reread = self.view_reread(place);
        Some(format!(
            "{{ {ct} {v} = {vv}; {kc}{walk}if ({ptr}->tag == T_STRUCT && {ptr}->u.o->rc == 1 && REC(*{ptr})->ty == {id} && REC(*{ptr})->f[{k}].tag == {tag}) {{ lc_v *{f} = &REC(*{ptr})->f[{k}]; {f}->u.{member} = {new}; }} else {{ {slow}; {reread}}} {rk}}} "
        ))
    }

    /// The struct and position of field `name` of `obj` when it is
    /// declared `f64`. It holds a float for certain, since every value
    /// stored in it is converted.
    fn float_field(&self, obj: &Ex, name: &str) -> Option<(u32, usize)> {
        self.scalar_field(obj, name).filter(|f| f.2 == Rep::Float).map(|(id, k, _)| (id, k))
    }

    /// The struct, position and representation of field `name` of `obj`
    /// when it is declared `f64`, an int type or `bool`: what it holds at
    /// run time, since every value stored in a field takes its type.
    fn scalar_field(&self, obj: &Ex, name: &str) -> Option<(u32, usize, Rep)> {
        let (id, k) = self.struct_field(obj, name)?;
        let rep = match self.prog.structs[id as usize].fields[k].ty {
            Ty::Float => Rep::Float,
            Ty::Int { .. } => Rep::Int,
            Ty::Bool => Rep::Bool,
            _ => return None,
        };
        Some((id, k, rep))
    }

    /// An expression statement whose value is dropped, in a sound program:
    /// an `if` or a block becomes C control flow without building a value.
    /// Only a value typed other than `()` can be an ignored error.
    fn effect(&mut self, e: &Ex, site: &str) -> String {
        match e {
            Ex::If { cond, then, els, span } => {
                let csite = self.site(if cond.span() == Span::default() { *span } else { cond.span() });
                let c = self.cond_at(cond, &csite);
                let t = self.effect(then, site);
                let f = els.as_ref().map_or(String::new(), |x| self.effect(x, site));
                format!("if ({c}) {{ {t}}} else {{ {f}}} ")
            }
            Ex::Block(stmts, tail) => {
                let mut code = String::from("{ ");
                for s in stmts {
                    code.push_str(&self.stmt(s));
                }
                if let Some(t) = tail {
                    code.push_str(&self.effect(t, site));
                }
                code.push_str("} ");
                code
            }
            _ => match self.rep(e) {
                Rep::Int if self.int_native(e) => format!("(void)({}); ", self.int(e)),
                Rep::Bool if self.bool_native(e) => format!("(void)({}); ", self.boolean(e)),
                Rep::Float if self.float_native(e) => format!("(void)({}); ", self.float(e)),
                _ => {
                    let v = self.expr(e);
                    if matches!(self.types.of(e), Some(T::Unit | T::Never)) {
                        format!("lc_release({v}); ")
                    } else {
                        let t = self.t();
                        format!("{{ lc_v {t} = {v}; lc_check_ignored({t}, {site}); lc_release({t}); }} ")
                    }
                }
            },
        }
    }

    /// An expression that reads but changes nothing (no statements, calls
    /// or methods), so a value it reads can be borrowed across it.
    fn pure(&self, e: &Ex) -> bool {
        match e {
            Ex::Lit(..) | Ex::Local(..) | Ex::Up(..) => true,
            Ex::Binary { l, r, .. } => self.pure(l) && self.pure(r),
            Ex::Unary { e, .. } => self.pure(e),
            _ => false,
        }
    }

    /// A boxed variable read without taking a reference: the C lvalue.
    fn borrowed(&mut self, e: &Ex) -> Option<String> {
        match e {
            Ex::Local(s, _) if self.slot_rep(*s) == Rep::Boxed => Some(self.slot(0, *s)),
            Ex::Up(d, s, _) => Some(self.slot(*d, *s)),
            _ => None,
        }
    }

    /// Is `e` a `len` that `len_fast` computes?
    fn is_len(&self, e: &Ex) -> bool {
        let ok = match e {
            Ex::Field { name, user: None, .. } => &**name == "len",
            Ex::Method { recv: Recv::Value(_), name, user: None, args, .. } => &**name == "len" && args.is_empty(),
            Ex::Method { recv: Recv::Place(p), name, user: None, args, .. } => {
                &**name == "len" && args.is_empty() && p.path.is_empty() && matches!(p.root, Root::Local(s) if self.slot_rep(s) == Rep::Boxed)
            }
            _ => false,
        };
        ok && self.rep(e) == Rep::Int
    }

    /// `o.len` for a list or a string, without the method call. `None` when
    /// `e` is something else.
    fn len_fast(&mut self, e: &Ex) -> Option<String> {
        if !self.is_len(e) {
            return None;
        }
        let (obj, span) = match e {
            Ex::Field { obj, name, user: None, span } if &**name == "len" => (&**obj, *span),
            Ex::Method { recv: Recv::Value(obj), name, user: None, args, span } if &**name == "len" && args.is_empty() => (&**obj, *span),
            Ex::Method { recv: Recv::Place(p), name, user: None, args, span } if &**name == "len" && args.is_empty() && p.path.is_empty() => {
                let Root::Local(s) = p.root else { return None };
                let site = self.site(*span);
                let fid = self.field_id("len");
                let o = self.slot(0, s);
                if let Some((_, w, _)) = self.view_of(s) {
                    return Some(format!("({w}.data ? {w}.len : lc_len({o}, {fid}, {site}))"));
                }
                return Some(format!("lc_len({o}, {fid}, {site})"));
            }
            _ => return None,
        };
        let site = self.site(span);
        let fid = self.field_id("len");
        if let Some(o) = self.borrowed(obj) {
            if let Some((_, w, _)) = self.view(obj) {
                return Some(format!("({w}.data ? {w}.len : lc_len({o}, {fid}, {site}))"));
            }
            return Some(format!("lc_len({o}, {fid}, {site})"));
        }
        let (o, n) = (self.t(), self.t());
        let ov = self.expr(obj);
        Some(format!("({{ lc_v {o} = {ov}; int64_t {n} = lc_len({o}, {fid}, {site}); lc_release({o}); {n}; }})"))
    }

    /// The slots of a frame a loop in it may take a view of, by kind: lists
    /// of ints, bools or floats that no lambda in it uses.
    fn viewable(&self, body: &Ex) -> HashMap<u32, Rep> {
        let mut out = HashMap::new();
        let Some(ts) = self.types.frame(body).filter(|_| self.typed) else { return out };
        let captured = captured_slots(body);
        for (s, t) in ts.iter().enumerate() {
            let kind = match t {
                T::List(e) => match **e {
                    T::Int(_) => Rep::Int,
                    T::Bool => Rep::Bool,
                    T::Float => Rep::Float,
                    // A list of structs, whose fields a loop reads and
                    // assigns: `Boxed`.
                    T::Struct(..) => Rep::Boxed,
                    _ => continue,
                },
                _ => continue,
            };
            if !captured.contains(&(s as u32)) {
                out.insert(s as u32, kind);
            }
        }
        out
    }

    /// Views (`lc_view`) of the list slots a loop only reads and assigns
    /// elements of, for `elem`, `assign_elem` and `len_fast` to use: the C
    /// that reads them, to run before the loop, and how many views to drop
    /// after it (`close_views`).
    fn open_views(&mut self, cond: Option<&Ex>, body: &[St]) -> (String, usize) {
        let mut code = String::new();
        let mut n = 0;
        for s in view_slots(cond, body) {
            let Some(&kind) = self.cx.viewable.get(&s) else { continue };
            if self.slot_rep(s) != Rep::Boxed || self.view_of(s).is_some() {
                continue;
            }
            let w = self.t();
            let _ = write!(code, "lc_view {w} = {}; ", self.view_read(s, kind));
            self.cx.views.push((s, w, kind));
            n += 1;
        }
        (code, n)
    }

    fn close_views(&mut self, n: usize) {
        let keep = self.cx.views.len() - n;
        self.cx.views.truncate(keep);
    }

    /// Reads the view of slot `s`, packed as `kind`.
    fn view_read(&mut self, s: u32, kind: Rep) -> String {
        let k = match kind {
            Rep::Int => "K_INT",
            Rep::Bool => "K_BOOL",
            Rep::Float => "K_FLOAT",
            Rep::Boxed => "K_BOXED",
        };
        format!("lc_view_of({}, {k})", self.slot(0, s))
    }

    /// The view an enclosing loop took of `e`'s list, if `e` is a slot.
    fn view(&self, e: &Ex) -> Option<(u32, String, Rep)> {
        let Ex::Local(s, _) = e else { return None };
        self.view_of(*s)
    }

    fn view_of(&self, s: u32) -> Option<(u32, String, Rep)> {
        self.cx.views.iter().rev().find(|(v, _, _)| *v == s).cloned()
    }

    /// After an assignment to an element of the list in `place`, or to a
    /// field of one, that may have copied it or changed its kind: reads its
    /// view again, if it has one.
    fn view_reread(&mut self, place: &Place) -> String {
        match (&place.root, place.path.first()) {
            (Root::Local(s), Some(Seg::Index(..))) => match self.view_of(*s) {
                Some((s, w, kind)) => format!("{w} = {}; ", self.view_read(s, kind)),
                None => String::new(),
            },
            _ => String::new(),
        }
    }

    /// `obj[index]` with an unboxed index, its element typed int, bool or
    /// `f64` (`rep` Float: a number read as a double): read from a packed
    /// list without boxing.
    fn elem(&mut self, obj: &Ex, index: &Ex, span: Span, rep: Rep) -> String {
        let site = self.site(span);
        if let Some((s, w, kind)) = self.view(obj).filter(|v| v.2 == rep) {
            let get = match kind {
                Rep::Int => "lc_view_int",
                Rep::Bool => "lc_view_bool",
                _ => "lc_view_num",
            };
            let i = self.int(index);
            return format!("{get}({w}, {i}, &{}, {site})", self.slot(0, s));
        }
        let get = match rep {
            Rep::Int => "lc_get_int",
            Rep::Bool => "lc_get_bool",
            _ => "lc_get_num",
        };
        if self.pure(index) {
            if let Some(o) = self.borrowed(obj) {
                let i = self.int(index);
                return format!("{get}({o}, {i}, {site})");
            }
        }
        let (o, r) = (self.t(), self.t());
        let ov = self.expr(obj);
        let i = self.int(index);
        format!("({{ lc_v {o} = {ov}; {} {r} = {get}({o}, {i}, {site}); lc_release({o}); {r}; }})", c_type(rep))
    }

    /// `e`, typed int by the checker, as a C `int64_t`.
    fn int(&mut self, e: &Ex) -> String {
        if let Some(c) = self.len_fast(e) {
            return c;
        }
        match e {
            Ex::Lit(Value::Int(n), _) => {
                if *n == i64::MIN {
                    "INT64_MIN".into()
                } else {
                    format!("{n}LL")
                }
            }
            Ex::Local(s, _) if self.slot_rep(*s) == Rep::Int => format!("N{s}"),
            Ex::Binary { op, l, r, span } if self.int_native(e) => {
                let site = self.site(*span);
                let f = int_op(*op).unwrap_or("+");
                let (a, b) = (self.int(l), self.int(r));
                let apply = |x: &str, y: &str| if f.len() == 1 { format!("({x} {f} {y})") } else { format!("{f}({x}, {y}, {site})") };
                if self.simple(l) || self.simple(r) {
                    apply(&a, &b)
                } else {
                    let (x, y) = (self.t(), self.t());
                    format!("({{ int64_t {x} = {a}; int64_t {y} = {b}; {}; }})", apply(&x, &y))
                }
            }
            Ex::Unary { op: UnOp::Neg, e: x, span } if self.int_native(e) => {
                let site = self.site(*span);
                let v = self.int(x);
                format!("lc_ineg({v}, {site})")
            }
            Ex::Unary { op: UnOp::Bang, e: x, .. } if self.int_native(e) => {
                let v = self.int(x);
                format!("(~{v})")
            }
            Ex::If { cond, then, els: Some(els), span } => {
                let site = self.site(if cond.span() == Span::default() { *span } else { cond.span() });
                let c = self.cond_at(cond, &site);
                let (t, f) = (self.int(then), self.int(els));
                format!("({c} ? {t} : {f})")
            }
            Ex::Block(stmts, Some(tail)) => {
                let mut code = String::from("({ ");
                for s in stmts {
                    code.push_str(&self.stmt(s));
                }
                let r = self.t();
                let tv = self.int(tail);
                let _ = write!(code, "int64_t {r} = {tv}; {r}; }})");
                code
            }
            Ex::Match { scrut, arms, span } => self.match_native(scrut, arms, *span, Rep::Int),
            Ex::CallFn { fns, args, span, .. } if self.int_native(e) => {
                let en = self.direct_entry(fns, args).unwrap();
                self.typed_call(fns[0], &en, args, *span)
            }
            Ex::Index { obj, index, span } if self.int_native(e) => self.elem(obj, index, *span, Rep::Int),
            Ex::Field { obj, name, user: None, span } if self.int_native(e) => self.field_scalar(obj, name, *span, Rep::Int),
            _ => {
                let site = self.site(e.span());
                let v = self.expr(e);
                format!("lc_unbox_int({v}, {site})")
            }
        }
    }

    /// `e`, typed bool by the checker, as a C `bool`.
    fn boolean(&mut self, e: &Ex) -> String {
        match e {
            Ex::Lit(Value::Bool(b), _) => if *b { "true" } else { "false" }.into(),
            Ex::Local(s, _) if self.slot_rep(*s) == Rep::Bool => format!("N{s}"),
            Ex::Compare { first, rest, .. } => self.compare(first, rest),
            Ex::Field { obj, name, user: None, span } if self.bool_native(e) => self.field_scalar(obj, name, *span, Rep::Bool),
            Ex::And(a, b, span) | Ex::Or(a, b, span) => {
                let site = self.site(*span);
                let (x, y) = (self.cond_at(a, &site), self.cond_at(b, &site));
                let op = if matches!(e, Ex::And(..)) { "&&" } else { "||" };
                format!("({x} {op} {y})")
            }
            Ex::Unary { op: UnOp::Not | UnOp::Bang, e: x, .. } if self.bool_native(e) => {
                let v = self.boolean(x);
                format!("(!{v})")
            }
            Ex::If { cond, then, els: Some(els), span } => {
                let site = self.site(if cond.span() == Span::default() { *span } else { cond.span() });
                let c = self.cond_at(cond, &site);
                let (t, f) = (self.boolean(then), self.boolean(els));
                format!("({c} ? {t} : {f})")
            }
            Ex::Block(stmts, Some(tail)) => {
                let mut code = String::from("({ ");
                for s in stmts {
                    code.push_str(&self.stmt(s));
                }
                let r = self.t();
                let tv = self.boolean(tail);
                let _ = write!(code, "bool {r} = {tv}; {r}; }})");
                code
            }
            Ex::Match { scrut, arms, span } => self.match_native(scrut, arms, *span, Rep::Bool),
            Ex::CallFn { fns, args, span, .. } if self.bool_native(e) => {
                let en = self.direct_entry(fns, args).unwrap();
                self.typed_call(fns[0], &en, args, *span)
            }
            Ex::Index { obj, index, span } if self.bool_native(e) => self.elem(obj, index, *span, Rep::Bool),
            _ => {
                let site = self.site(e.span());
                let v = self.expr(e);
                format!("lc_unbox_bool({v}, {site})")
            }
        }
    }

    /// A condition as a C `bool`; a boxed one must be a bool at run time,
    /// with errors reported at `site`.
    fn cond_at(&mut self, e: &Ex, site: &str) -> String {
        if self.rep(e) == Rep::Bool {
            return self.boolean(e);
        }
        let (c, t) = (self.t(), self.t());
        let v = self.expr(e);
        format!("({{ lc_v {c} = {v}; bool {t} = lc_test({c}, {site}); lc_release({c}); {t}; }})")
    }

    /// A comparison chain `a < b <= c` as a C `bool`.
    fn compare(&mut self, first: &Ex, rest: &[(CmpOp, Ex, Span)]) -> String {
        // How each operand is compared: unboxed, or a boxed number read as
        // a double (against a float only, as the runtime converts it).
        #[derive(Clone, Copy, PartialEq)]
        enum K {
            Int,
            Bool,
            Float,
            Num,
        }
        let operands: Vec<&Ex> = std::iter::once(first).chain(rest.iter().map(|(_, x, _)| x)).collect();
        let kinds: Option<Vec<K>> = operands
            .iter()
            .map(|x| match self.rep(x) {
                Rep::Int => Some(K::Int),
                Rep::Bool => Some(K::Bool),
                Rep::Float => Some(K::Float),
                Rep::Boxed if self.numeric(x) => Some(K::Num),
                Rep::Boxed => None,
            })
            .collect();
        let native = kinds.as_ref().is_some_and(|ks| {
            rest.iter().enumerate().all(|(i, (op, _, _))| {
                cmp_op(*op).is_some()
                    && match (ks[i], ks[i + 1]) {
                        (K::Int, K::Int) => true,
                        (K::Bool, K::Bool) => matches!(op, CmpOp::Eq | CmpOp::Ne),
                        (K::Float, K::Float | K::Int | K::Num) | (K::Int | K::Num, K::Float) => true,
                        _ => false,
                    }
            })
        });
        if native {
            let ks = kinds.unwrap_or_default();
            let mut vals = Vec::new();
            for (x, k) in operands.iter().zip(&ks) {
                vals.push(match k {
                    K::Int => self.int(x),
                    K::Bool => self.boolean(x),
                    K::Float => self.float(x),
                    K::Num => self.num(x),
                });
            }
            let mut cmps = Vec::new();
            for (i, (op, _, sp)) in rest.iter().enumerate() {
                let site = self.site(*sp);
                cmps.push((i, *op, site));
            }
            let pair = |a: &str, b: &str, i: usize, op: CmpOp, site: &str| -> String {
                let c = cmp_op(op).unwrap_or("==");
                match (ks[i], ks[i + 1]) {
                    (K::Int, K::Int) | (K::Bool, K::Bool) => format!("({a} {c} {b})"),
                    (K::Int, _) => format!("lc_fcmp({}, (double){a}, {b}, {site})", cmp_c(op)),
                    (_, K::Int) => format!("lc_fcmp({}, {a}, (double){b}, {site})", cmp_c(op)),
                    _ => format!("lc_fcmp({}, {a}, {b}, {site})", cmp_c(op)),
                }
            };
            if rest.len() == 1 && (self.simple(first) || self.simple(&rest[0].1)) {
                let (i, op, site) = &cmps[0];
                return pair(&vals[0], &vals[1], *i, *op, site);
            }
            let ok = self.t();
            let ts: Vec<String> = (0..vals.len()).map(|_| self.t()).collect();
            let ctype = |k: K| match k {
                K::Int => "int64_t",
                K::Bool => "bool",
                K::Float | K::Num => "double",
            };
            let mut code = format!("({{ {} {} = {}; bool {ok} = true; ", ctype(ks[0]), ts[0], vals[0]);
            for (i, op, site) in &cmps {
                let (a, b) = (&ts[*i], &ts[i + 1]);
                let _ = write!(code, "{} {b}; if ({ok}) {{ {b} = {}; {ok} = {}; }} ", ctype(ks[i + 1]), vals[i + 1], pair(a, b, *i, *op, site));
            }
            let _ = write!(code, "{ok}; }})");
            return code;
        }
        let ok = self.t();
        let p = self.t();
        let fv = self.expr(first);
        let mut code = format!("({{ lc_v {p} = {fv}; bool {ok} = true; ");
        for (op, x, sp) in rest {
            let site = self.site(*sp);
            let n = self.t();
            let xv = self.expr(x);
            let _ = write!(code, "if ({ok}) {{ lc_v {n} = {xv}; {ok} = lc_cmp_fast({}, {p}, {n}, {site}); lc_release({p}); {p} = {n}; }} ", cmp_c(*op));
        }
        let _ = write!(code, "lc_release({p}); {ok}; }})");
        code
    }

    /// A `match` whose arms give an unboxed value.
    fn match_native(&mut self, scrut: &Ex, arms: &[Arm], span: Span, rep: Rep) -> String {
        let site = self.site(span);
        let (sv, r) = (self.t(), self.t());
        let scv = self.expr(scrut);
        let ty = c_type(rep);
        let mut code = format!("({{ lc_v {sv} = {scv}; {ty} {r}; ");
        for arm in arms {
            let cond = self.pat(&arm.pat, &sv, &site);
            let guard = match &arm.guard {
                Some(g) => format!(" && {}", self.cond_at(g, &site)),
                None => String::new(),
            };
            let body = match rep {
                Rep::Int => self.int(&arm.body),
                Rep::Float => self.float(&arm.body),
                _ => self.boolean(&arm.body),
            };
            let _ = write!(code, "if (({cond}){guard}) {r} = {body}; else ");
        }
        let _ = write!(code, "lc_no_arm({sv}, {site}); lc_release({sv}); {r}; }})");
        code
    }

    /// `e` as an owned `lc_v`, computed boxed.
    fn expr_boxed(&mut self, e: &Ex) -> String {
        match e {
            Ex::Lit(v, _) => self.lit(v),
            Ex::Str(pieces, _) => self.interp(pieces),
            Ex::Local(s, _) => match self.slot_rep(*s) {
                Rep::Boxed => format!("lc_retain({})", self.slot(0, *s)),
                r => box_c(r, &format!("N{s}")),
            },
            Ex::Up(d, s, _) => format!("lc_retain({})", self.slot(*d, *s)),
            Ex::Global(g, _) => format!("lc_retain(K{g})"),
            Ex::List(xs, _) | Ex::Tuple(xs, _) => {
                let l = self.t();
                let ctor = if matches!(e, Ex::List(..)) { "lc_list_new" } else { "lc_tuple_new" };
                let mut code = format!("({{ lc_v {l} = {ctor}({}); ", xs.len());
                for x in xs {
                    let v = self.expr(x);
                    let _ = write!(code, "lc_vec_push({l}, {v}); ");
                }
                let _ = write!(code, "{l}; }})");
                code
            }
            Ex::Set(xs, _) => {
                let l = self.t();
                let mut code = format!("({{ lc_v {l} = lc_set_new(); ");
                for x in xs {
                    let v = self.expr(x);
                    let _ = write!(code, "lc_set_add({l}, {v}); ");
                }
                let _ = write!(code, "{l}; }})");
                code
            }
            Ex::Map(ps, _) => {
                let l = self.t();
                let mut code = format!("({{ lc_v {l} = lc_map_new(); ");
                for (k, v) in ps {
                    let (tk, kv) = (self.t(), self.expr(k));
                    let vv = self.expr(v);
                    let _ = write!(code, "lc_v {tk} = {kv}; lc_map_put({l}, {tk}, {vv}); ");
                }
                let _ = write!(code, "{l}; }})");
                code
            }
            Ex::Struct { id, fields, span } => {
                let sd = &self.prog.structs[*id as usize];
                let n = fields.len();
                let arr = self.t();
                let mut code = format!("({{ lc_v {arr}[{}]; ", n.max(1));
                for (i, fe) in fields.iter().enumerate() {
                    let Some(fd) = sd.fields.get(i) else { continue };
                    let v = match fe {
                        Some(x) => self.expr(x),
                        None => {
                            if fd.default.is_some() {
                                format!("SD{id}_{i}()")
                            } else {
                                "LC_UNIT".to_string()
                            }
                        }
                    };
                    if fe.as_ref().is_some_and(|x| self.exact(&fd.ty, x)) {
                        let _ = write!(code, "{arr}[{i}] = {v}; ");
                        continue;
                    }
                    let site = self.site(fe.as_ref().map_or(*span, |x| if x.span() == Span::default() { *span } else { x.span() }));
                    let ty = self.ty(&fd.ty);
                    let _ = write!(code, "{arr}[{i}] = lc_coerce_field({v}, {ty}, {}, {}, false, {site}); ", cstr(&fd.name), cstr(&sd.name));
                }
                let _ = write!(code, "lc_rec_new(T_STRUCT, {id}, 0, {n}, {arr}); }})");
                code
            }
            Ex::Variant { id, tag, args, span } => {
                let vd = &self.prog.enums[*id as usize].variants[*tag as usize];
                let site = self.site(*span);
                let arr = self.t();
                let mut code = self.args_into(&arr, args);
                code = format!("({{ {code}");
                for (i, ft) in vd.fields.iter().enumerate().take(args.len()) {
                    let ty = self.ty(ft);
                    let _ = write!(code, "{arr}[{i}] = lc_coerce_field({arr}[{i}], {ty}, \"\", {}, true, {site}); ", cstr(&vd.name));
                }
                let _ = write!(code, "lc_rec_new(T_VARIANT, {id}, {tag}, {}, {arr}); }})", args.len());
                code
            }
            Ex::Field { obj, name, user: None, span } if self.struct_field(obj, name).is_some() => {
                // A field of a struct the checker knows: by position.
                let (id, k) = self.struct_field(obj, name).unwrap_or_default();
                let site = self.site(*span);
                let (o, r, found) = (self.t(), self.t(), self.t());
                let fid = self.field_id(name);
                let mid = method_id(name).map_or(-1, |m| m as i64);
                let (init, release) = match self.borrowed(obj) {
                    Some(b) => (b, String::new()),
                    None => (self.expr(obj), format!("lc_release({o}); ")),
                };
                format!(
                    "({{ lc_v {o} = {init}; lc_v {r}; if ({o}.tag == T_STRUCT && REC({o})->ty == {id}) {r} = lc_retain(REC({o})->f[{k}]); else {{ bool {found}; {r} = lc_field({o}, {fid}, {n}, &{found}, {site}); if (!{found}) {{ {r} = lc_field_fallback({o}, {n}, {mid}, {site}); if (lc_unwinding) UNWIND; }} }} {release}{r}; }})",
                    n = cstr(name)
                )
            }
            Ex::Field { obj, name, user, span } => {
                let site = self.site(*span);
                let o = self.t();
                let r = self.t();
                let found = self.t();
                let fid = self.field_id(name);
                let ov = self.expr(obj);
                let mut code = format!("({{ lc_v {o} = {ov}; bool {found}; lc_v {r} = lc_field({o}, {fid}, {n}, &{found}, {site}); if (!{found}) {{ ", n = cstr(name));
                let mut close = String::new();
                if let Some(ids) = user {
                    let k = self.t();
                    let _ = write!(code, "int {k} = {}; ", self.pick(ids, &o, 1));
                    for &fid in ids.iter() {
                        let np = self.prog.fns[fid as usize].params.len().max(1);
                        let fill = self.fill_defaults(fid, "A", 1);
                        let _ = write!(code, "if ({k} == {fid}) {{ lc_v A[{np}]; A[0] = lc_retain({o}); {fill}lc_callsite = {site}; {r} = F{fid}(A); }} else ");
                    }
                    code.push_str("{ ");
                    close.push_str("} ");
                }
                let mid = method_id(name).map_or(-1, |m| m as i64);
                let _ = write!(code, "{r} = lc_field_fallback({o}, {}, {mid}, {site}); if (lc_unwinding) UNWIND; {close}}} lc_release({o}); {r}; }})", cstr(name));
                code
            }
            Ex::Index { obj, index, span } if self.rep(index) == Rep::Int => {
                let site = self.site(*span);
                if self.pure(index) {
                    if let Some(o) = self.borrowed(obj) {
                        let i = self.int(index);
                        return format!("lc_index_int({o}, {i}, {site})");
                    }
                }
                let (o, r) = (self.t(), self.t());
                let ov = self.expr(obj);
                let i = self.int(index);
                format!("({{ lc_v {o} = {ov}; lc_v {r} = lc_index_int({o}, {i}, {site}); lc_release({o}); {r}; }})")
            }
            Ex::Index { obj, index, span } => {
                let site = self.site(*span);
                let (o, i, r) = (self.t(), self.t(), self.t());
                let (ov, iv) = (self.expr(obj), self.expr(index));
                format!("({{ lc_v {o} = {ov}; lc_v {i} = {iv}; lc_v {r} = lc_index_fast({o}, {i}, {site}); lc_release({o}); lc_release({i}); {r}; }})")
            }
            Ex::Slice { obj, start, end, inclusive, span } => {
                let site = self.site(*span);
                let o = self.t();
                let ov = self.expr(obj);
                let mut code = format!("({{ lc_v {o} = {ov}; ");
                let mut bound = |g: &mut Gen, x: &Option<Box<Ex>>| -> (String, String) {
                    match x {
                        Some(x) => {
                            let (t, n) = (g.t(), g.t());
                            let s = g.site(x.span());
                            let v = g.expr(x);
                            let _ = write!(code, "lc_v {t} = {v}; int64_t {n} = lc_int_of({t}, {s}); lc_release({t}); ");
                            ("true".into(), n)
                        }
                        None => ("false".into(), "0".into()),
                    }
                };
                let (hs, s) = bound(self, start);
                let (he, en) = bound(self, end);
                let r = self.t();
                let _ = write!(code, "lc_v {r} = lc_slice({o}, {hs}, {s}, {he}, {en}, {inclusive}, {site}); lc_release({o}); {r}; }})");
                code
            }
            Ex::CallFn { fns, args, places, span } => self.call_fn(fns, args, places, *span),
            Ex::CallValue { f, args, span } => {
                let site = self.site(*span);
                let (fv, arr, r) = (self.t(), self.t(), self.t());
                let fe = self.expr(f);
                let a = self.args_into(&arr, args);
                let rel = Self::release_arr(&arr, args.len());
                format!("({{ lc_v {fv} = {fe}; {a}lc_v {r} = lc_call({fv}, {}, {arr}, {site}); lc_release({fv}); {rel}if (lc_unwinding) UNWIND; {r}; }})", args.len())
            }
            Ex::CallBuiltin { f, args, span } => self.builtin(*f, args, *span),
            Ex::Method { recv, name, user, args, span } => self.method(recv, name, user.as_ref(), args, *span),
            Ex::Unary { op, e: x, span } => {
                let site = self.site(*span);
                let (t, r) = (self.t(), self.t());
                let v = self.expr(x);
                let op = match op {
                    UnOp::Neg => "UN_NEG",
                    UnOp::Not => "UN_NOT",
                    UnOp::Bang => "UN_BANG",
                };
                format!("({{ lc_v {t} = {v}; lc_v {r} = lc_unop({op}, {t}, {site}); lc_release({t}); {r}; }})")
            }
            Ex::Binary { op, l, r, span } => {
                let site = self.site(*span);
                let (a, b, res) = (self.t(), self.t(), self.t());
                let (lv, rv) = (self.expr(l), self.expr(r));
                let opc = binop_c(*op);
                format!("({{ lc_v {a} = {lv}; lc_v {b} = {rv}; lc_v {res} = lc_arith_own({opc}, {a}, {b}, {site}); {res}; }})")
            }
            Ex::And(..) | Ex::Or(..) | Ex::Compare { .. } => {
                let c = self.boolean(e);
                format!("lc_bool({c})")
            }
            Ex::Coalesce(a, b, _) => {
                let (x, r) = (self.t(), self.t());
                let (av, bv) = (self.expr(a), self.expr(b));
                format!("({{ lc_v {x} = {av}; lc_v {r}; if ({x}.tag == T_NONE || {x}.tag == T_ERR) {{ lc_release({x}); {r} = {bv}; }} else {r} = {x}; {r}; }})")
            }
            Ex::Range { start, end, inclusive, span } => {
                let site = self.site(*span);
                let (s, en, r) = (self.t(), self.t(), self.t());
                let sv = start.as_ref().map_or("LC_UNIT".to_string(), |x| self.expr(x));
                let ev = end.as_ref().map_or("LC_UNIT".to_string(), |x| self.expr(x));
                format!(
                    "({{ lc_v {s} = {sv}; lc_v {en} = {ev}; lc_v {r} = lc_make_range({}, {s}, {}, {en}, {inclusive}, {site}); lc_release({s}); lc_release({en}); {r}; }})",
                    start.is_some(),
                    end.is_some()
                )
            }
            Ex::Try { e: x, fn_optional, .. } => {
                let t = self.t();
                let v = self.expr(x);
                let none_ret = if *fn_optional { "{ lc_release(" .to_string() + &t + "); RETURN(LC_NONE) }" } else { format!("RETURN({t})") };
                format!(
                    "({{ lc_v {t} = {v}; if ({t}.tag == T_ERR) {none_ret} if ({t}.tag == T_NONE) RETURN(lc_try_none({fn_optional})) {t}; }})"
                )
            }
            Ex::Lambda(def) => self.lambda(def),
            Ex::If { cond, then, els, span } => {
                let site = self.site(if cond.span() == Span::default() { *span } else { cond.span() });
                let r = self.t();
                let c = self.cond_at(cond, &site);
                let tv = self.expr(then);
                let ev = els.as_ref().map_or("LC_UNIT".to_string(), |x| self.expr(x));
                format!("({{ lc_v {r}; if ({c}) {r} = {tv}; else {r} = {ev}; {r}; }})")
            }
            Ex::Match { scrut, arms, span } => {
                let site = self.site(*span);
                let (sv, r) = (self.t(), self.t());
                let scv = self.expr(scrut);
                let mut code = format!("({{ lc_v {sv} = {scv}; lc_v {r}; ");
                for arm in arms {
                    let cond = self.pat(&arm.pat, &sv, &site);
                    let guard = match &arm.guard {
                        Some(g) => format!(" && {}", self.cond_at(g, &site)),
                        None => String::new(),
                    };
                    let body = self.expr(&arm.body);
                    let _ = write!(code, "if (({cond}){guard}) {r} = {body}; else ");
                }
                let _ = write!(code, "lc_no_arm({sv}, {site}); lc_release({sv}); {r}; }})");
                code
            }
            Ex::Block(stmts, tail) => {
                let mut code = String::from("({ ");
                for s in stmts {
                    code.push_str(&self.stmt(s));
                }
                let tv = tail.as_ref().map_or("LC_UNIT".to_string(), |t| self.expr(t));
                let r = self.t();
                let _ = write!(code, "lc_v {r} = {tv}; {r}; }})");
                code
            }
            Ex::Cast { e: x, to, span } => {
                let site = self.site(*span);
                let (t, r) = (self.t(), self.t());
                let v = self.expr(x);
                let conv = self.conv(*to, &t, &site);
                format!("({{ lc_v {t} = {v}; lc_v {r} = {conv}; lc_release({t}); {r}; }})")
            }
            Ex::AssertEq { l, r, msg, span } => {
                let site = self.site(*span);
                let (a, b, eq) = (self.t(), self.t(), self.t());
                let (lv, rv) = (self.expr(l), self.expr(r));
                let fail = match msg {
                    Some(m) => {
                        let mt = self.t();
                        let mv = self.expr(m);
                        format!("{{ lc_v {mt} = {mv}; lc_assert_eq({a}, {b}, {mt}, true, {site}); }}")
                    }
                    None => format!("lc_assert_eq({a}, {b}, LC_UNIT, false, {site});"),
                };
                format!(
                    "({{ lc_v {a} = {lv}; lc_v {b} = {rv}; int {eq} = lc_eq({a}, {b}); if ({eq} != 1) {fail} lc_release({a}); lc_release({b}); LC_TRUE; }})"
                )
            }
            Ex::Poison(span) => {
                let site = self.site(*span);
                format!("(lc_panic(\"E0000\", {site}, \"internal error: unresolved expression\"), LC_UNIT)")
            }
        }
    }

    fn conv(&mut self, to: ConvTo, v: &str, site: &str) -> String {
        format!("lc_convert({v}, {}, {site})", conv_args(to))
    }

    fn interp(&mut self, pieces: &[StrPiece]) -> String {
        let b = self.t();
        let mut code = format!("({{ lc_buf {b} = {{0}}; ");
        for p in pieces {
            match p {
                StrPiece::Lit(s) => {
                    let _ = write!(code, "lc_buf_put(&{b}, {}, {}); ", cstr(s), s.len());
                }
                StrPiece::Expr(x, spec, args) => {
                    let site = self.site(x.span());
                    let t = self.t();
                    let v = self.expr(x);
                    let _ = write!(
                        code,
                        "lc_v {t} = {v}; if ({t}.tag == T_ERR) {{ lc_buf _e = {{0}}; lc_buf_display(&_e, {t}); lc_panic(\"E0406\", {site}, \"interpolating an error (%s); add `?`\", _e.p); }} "
                    );
                    match spec {
                        None => {
                            let _ = write!(code, "lc_buf_display(&{b}, {t}); ");
                        }
                        Some(sp) => {
                            let f = self.t();
                            let _ = write!(code, "lc_fmt {f} = {}; ", fmt_init(sp));
                            let mut ai = args.iter();
                            for (has, field) in [(sp.width_arg.is_some(), "width"), (sp.precision_arg.is_some(), "precision")] {
                                if !has {
                                    continue;
                                }
                                let Some(a) = ai.next() else { continue };
                                let asite = self.site(a.span());
                                let (at, an) = (self.t(), self.t());
                                let av = self.expr(a);
                                let _ = write!(
                                    code,
                                    "lc_v {at} = {av}; int64_t {an} = lc_int_of({at}, {asite}); lc_release({at}); if ({an} < 0) lc_panic(\"E0301\", {asite}, \"format width and precision must not be negative\"); {f}.{field} = {an}; "
                                );
                            }
                            let _ = write!(code, "lc_buf_format(&{b}, {t}, &{f}, {site}); ");
                        }
                    }
                    let _ = write!(code, "lc_release({t}); ");
                }
            }
        }
        let _ = write!(code, "lc_buf_finish(&{b}); }})");
        code
    }

    /// The first-argument check the interpreter's `pick_opt` makes:
    /// an expression giving the chosen function id, or -1.
    fn pick(&mut self, ids: &Rc<[FnId]>, first: &str, n: usize) -> String {
        if ids.len() == 1 {
            let fd = &self.prog.fns[ids[0] as usize];
            if fd.params.is_empty() && n > 0 {
                return "-1".into();
            }
            return match fd.params.first() {
                Some(p) => format!("(lc_ty_matches({first}, {}) ? {} : -1)", self.ty(&p.ty), ids[0]),
                None => ids[0].to_string(),
            };
        }
        let mut code = String::new();
        for &id in ids.iter() {
            let fd = &self.prog.fns[id as usize];
            let req = fd.params.iter().filter(|p| p.default.is_none()).count();
            if n < req || n > fd.params.len() {
                continue;
            }
            let check = match fd.params.first() {
                Some(p) => format!("lc_ty_matches({first}, {})", self.ty(&p.ty)),
                None => "1".to_string(),
            };
            let _ = write!(code, "{check} ? {id} : ");
        }
        format!("({code}-1)")
    }

    /// Calls user function `fid` with argument values in `arr` (owned, one
    /// per parameter given), filling defaults.
    fn fill_defaults(&self, fid: FnId, arr: &str, given: usize) -> String {
        let fd = &self.prog.fns[fid as usize];
        let mut code = String::new();
        for (i, p) in fd.params.iter().enumerate().skip(given) {
            if p.default.is_some() {
                let _ = write!(code, "{arr}[{i}] = D{fid}_{i}(); ");
            } else {
                let _ = write!(code, "{arr}[{i}] = LC_UNIT; ");
            }
        }
        code
    }

    fn call_fn(&mut self, fns: &Rc<[FnId]>, args: &[Ex], places: &[Option<Place>], span: Span) -> String {
        if let Some(en) = self.direct_entry(fns, args) {
            let c = self.typed_call(fns[0], &en, args, span);
            return box_c(en.ret, &c);
        }
        let site = self.site(span);
        let arr = self.t();
        let r = self.t();
        let np_max = fns.iter().map(|&f| self.prog.fns[f as usize].params.len()).max().unwrap_or(0).max(args.len());
        let mut code = format!("({{ lc_v {arr}[{}]; ", np_max.max(1));
        for (i, a) in args.iter().enumerate() {
            let v = self.expr(a);
            let _ = write!(code, "{arr}[{i}] = {v}; ");
        }
        // `mut` arguments: the keys of their places, evaluated once.
        let mut writebacks = Vec::new();
        for (i, p) in places.iter().enumerate() {
            if let Some(p) = p {
                let (kc, keys) = self.place_keys(p);
                code.push_str(&kc);
                writebacks.push((i, p, keys));
            }
        }
        let call_one = |g: &mut Gen, fid: FnId| -> String {
            let fill = g.fill_defaults(fid, &arr, args.len());
            let fd = &g.prog.fns[fid as usize];
            let mut before = String::new();
            let mut after = String::new();
            for (i, p, keys) in &writebacks {
                if fd.params.get(*i).is_some_and(|p| p.mode == Mode::Mut) {
                    // Move the value out of its place for the call, and back after.
                    let (p1, p2) = (g.t(), g.t());
                    let w1 = g.place_walk(p, keys, &p1, "VIV_NO", "LC_UNIT", 0);
                    let w2 = g.place_walk(p, keys, &p2, "VIV_NO", "LC_UNIT", 0);
                    let _ = write!(before, "lc_release({arr}[{i}]); {{ {w1}{arr}[{i}] = lc_take({p1}); }} ");
                    let _ = write!(after, "{{ {w2}lc_set({p2}, {arr}[{i}]); }} ");
                }
            }
            format!("{fill}{before}lc_callsite = {site}; {r} = F{fid}({arr}); {after}")
        };
        let _ = write!(code, "lc_v {r}; ");
        if fns.len() == 1 {
            code.push_str(&call_one(self, fns[0]));
        } else {
            let k = self.t();
            let first = if args.is_empty() { "LC_UNIT".to_string() } else { format!("{arr}[0]") };
            let pick = self.pick_dispatch(fns, &first, args.len());
            let _ = write!(code, "int {k} = {pick}; ");
            for &fid in fns.iter() {
                let c = call_one(self, fid);
                let _ = write!(code, "if ({k} == {fid}) {{ {c}}} else ");
            }
            let name = cstr(&self.prog.fns[fns[0] as usize].name);
            let _ = write!(
                code,
                "{{ char kb[64]; lc_panic(\"E0301\", {site}, \"no `%s` takes %s\", {name}, {} ); }} ",
                if args.is_empty() { "\"no arguments\"".to_string() } else { format!("lc_kind({arr}[0], kb)") }
            );
        }
        for (_, _, keys) in &writebacks {
            code.push_str(&Self::release_all(keys));
        }
        let _ = write!(code, "{r}; }})");
        code
    }

    /// The interpreter's `pick` for a direct call to an overloaded name.
    fn pick_dispatch(&mut self, ids: &Rc<[FnId]>, first: &str, n: usize) -> String {
        let mut code = String::new();
        for &id in ids.iter() {
            let fd = &self.prog.fns[id as usize];
            let req = fd.params.iter().filter(|p| p.default.is_none()).count();
            if n < req || n > fd.params.len() {
                continue;
            }
            let check = match fd.params.first() {
                Some(p) if n > 0 => format!("lc_ty_matches({first}, {})", self.ty(&p.ty)),
                Some(_) => "0".to_string(),
                None => (n == 0).to_string(),
            };
            let _ = write!(code, "{check} ? {id} : ");
        }
        format!("({code}-1)")
    }

    fn builtin(&mut self, b: Builtin, args: &[Ex], span: Span) -> String {
        let site = self.site(span);
        let arr = self.t();
        let r = self.t();
        let a = self.args_into(&arr, args);
        let n = args.len();
        let call = match b {
            Builtin::Print => format!("(lc_print({n}, {arr}, false, {site}), LC_UNIT)"),
            Builtin::Eprint => format!("(lc_print({n}, {arr}, true, {site}), LC_UNIT)"),
            Builtin::Range => format!("lc_range_fn({n}, {arr}, {site})"),
            Builtin::Min => format!("lc_minmax(false, {n}, {arr}, {site})"),
            Builtin::Max => format!("lc_minmax(true, {n}, {arr}, {site})"),
            Builtin::SetNew => format!("lc_collect(0, {n}, {arr}, {site})"),
            Builtin::HeapNew => format!("lc_collect(1, {n}, {arr}, {site})"),
            Builtin::ListNew => format!("lc_collect(2, {n}, {arr}, {site})"),
            Builtin::Conv(to) => self.conv(to, &format!("{arr}[0]"), &site),
            Builtin::Panic => format!("(lc_panic_fn({arr}[0], {site}), LC_UNIT)"),
            Builtin::FsRead => format!("lc_fs(FS_READ, {n}, {arr}, {site})"),
            Builtin::FsWrite => format!("lc_fs(FS_WRITE, {n}, {arr}, {site})"),
            Builtin::FsAppend => format!("lc_fs(FS_APPEND, {n}, {arr}, {site})"),
            Builtin::FsExists => format!("lc_fs(FS_EXISTS, {n}, {arr}, {site})"),
            Builtin::FsLines => format!("lc_fs(FS_LINES, {n}, {arr}, {site})"),
            Builtin::FsRemove => format!("lc_fs(FS_REMOVE, {n}, {arr}, {site})"),
            Builtin::IoRead => format!("lc_io(IO_READ, {n}, {arr}, {site})"),
            Builtin::IoLines => format!("lc_io(IO_LINES, {n}, {arr}, {site})"),
            Builtin::IoReadLine => format!("lc_io(IO_READ_LINE, {n}, {arr}, {site})"),
            Builtin::IoWrite => format!("lc_io(IO_WRITE, {n}, {arr}, {site})"),
            Builtin::OsArgs => format!("lc_os(OS_ARGS, {n}, {arr}, {site})"),
            Builtin::OsEnv => format!("lc_os(OS_ENV, {n}, {arr}, {site})"),
            Builtin::OsExit => format!("lc_os(OS_EXIT, {n}, {arr}, {site})"),
            Builtin::TimeNow => format!("lc_os(TIME_NOW, {n}, {arr}, {site})"),
        };
        let rel = Self::release_arr(&arr, n);
        format!("({{ {a}lc_v {r} = {call}; {rel}{r}; }})")
    }

    fn method(&mut self, recv: &Recv, name: &str, user: Option<&Rc<[FnId]>>, args: &[Ex], span: Span) -> String {
        let site = self.site(span);
        let m = method_id(name);
        let builtin = m.is_some() && is_method(name);
        // A builtin mutator with no user function of the name: straight to
        // the place.
        if user.is_none() && is_mutator(name) {
            let m = m.unwrap_or(0);
            if let (Recv::Place(p), "push", [x]) = (recv, name, args) {
                if let Some(push) = match self.rep(x) {
                    Rep::Int => Some("lc_push_int"),
                    Rep::Bool => Some("lc_push_bool"),
                    Rep::Float => Some("lc_push_float"),
                    Rep::Boxed => None,
                } {
                    // An unboxed value: straight into a list packed as its kind.
                    let t = self.t();
                    let v = match self.rep(x) {
                        Rep::Int => self.int(x),
                        Rep::Bool => self.boolean(x),
                        _ => self.float(x),
                    };
                    let (kc, keys) = self.place_keys(p);
                    let ptr = self.t();
                    let walk = self.place_walk(p, &keys, &ptr, "VIV_METHOD", "LC_UNIT", m);
                    let rk = Self::release_all(&keys);
                    return format!("({{ {} {t} = {v}; {kc}{walk}{push}({ptr}, {t}, {site}); {rk}LC_UNIT; }})", c_type(self.rep(x)));
                }
            }
            let arr = self.t();
            let a = self.args_into(&arr, args);
            let rel = Self::release_arr(&arr, args.len());
            let r = self.t();
            return match recv {
                Recv::Place(p) => {
                    let (kc, keys) = self.place_keys(p);
                    let ptr = self.t();
                    let walk = self.place_walk(p, &keys, &ptr, "VIV_METHOD", "LC_UNIT", m);
                    let rk = Self::release_all(&keys);
                    format!("({{ {a}{kc}{walk}lc_v {r} = lc_mutate({m}, {ptr}, {}, {arr}, {site}); {rk}{rel}if (lc_unwinding) UNWIND; {r}; }})", args.len())
                }
                Recv::Value(_) => format!(
                    "({{ {a}lc_panic(\"E0410\", {site}, \"`%s` changes its receiver, so call it on a variable, not a temporary value\", {}); LC_UNIT; }})",
                    cstr(name)
                ),
            };
        }
        let rv = self.t();
        let recv_code = match recv {
            Recv::Place(p) => self.read_place(p),
            Recv::Value(e) => self.expr(e),
        };
        let r = self.t();
        let mut code = format!("({{ lc_v {rv} = {recv_code}; lc_v {r}; ");
        let builtin_call = |g: &mut Gen| -> String {
            let Some(m) = m else {
                return format!("{r} = LC_UNIT; ");
            };
            if is_mutator(name) {
                // A user function shares the name; the builtin mutates the place.
                let arr = g.t();
                let a = g.args_into(&arr, args);
                let rel = Self::release_arr(&arr, args.len());
                return match recv {
                    Recv::Place(p) => {
                        let (kc, keys) = g.place_keys(p);
                        let ptr = g.t();
                        let walk = g.place_walk(p, &keys, &ptr, "VIV_METHOD", "LC_UNIT", m);
                        let rk = Self::release_all(&keys);
                        format!("{a}{kc}{walk}{r} = lc_mutate({m}, {ptr}, {}, {arr}, {site}); {rk}{rel}if (lc_unwinding) UNWIND; ", args.len())
                    }
                    Recv::Value(_) => format!(
                        "{a}lc_panic(\"E0410\", {site}, \"`%s` changes its receiver, so call it on a variable, not a temporary value\", {}); ",
                        cstr(name)
                    ),
                };
            }
            if let (true, Some(&to)) = (name == "parse", g.prog.parse_to.get(&span)) {
                return format!("{r} = lc_parse_as({rv}, {}, {site}); if (lc_unwinding) UNWIND; ", conv_args(to));
            }
            let arr = g.t();
            let a = g.args_into(&arr, args);
            let rel = Self::release_arr(&arr, args.len());
            format!("{a}{r} = lc_method({m}, {rv}, {}, {arr}, {site}); {rel}if (lc_unwinding) UNWIND; ", args.len())
        };
        match user {
            None => match self.inline_loop(name, args, &rv, &r, &site) {
                Some(l) => {
                    let b = builtin_call(self);
                    let _ = write!(code, "if ({rv}.tag == T_LIST) {{ {l}}} else {{ {b}}} ");
                }
                None => code.push_str(&builtin_call(self)),
            },
            Some(ids) => {
                let k = self.t();
                let pick = self.pick(ids, &rv, args.len() + 1);
                let _ = write!(code, "int {k} = {pick}; ");
                for &fid in ids.iter() {
                    let fd = &self.prog.fns[fid as usize];
                    let np = fd.params.len().max(args.len() + 1);
                    let arr = self.t();
                    let mut c = format!("lc_v {arr}[{np}]; {arr}[0] = lc_retain({rv}); ");
                    for (i, a) in args.iter().enumerate() {
                        let v = self.expr(a);
                        let _ = write!(c, "{arr}[{}] = {v}; ", i + 1);
                    }
                    c.push_str(&self.fill_defaults(fid, &arr, args.len() + 1));
                    let mut_recv = fd.params.first().is_some_and(|p| p.mode == Mode::Mut);
                    match (mut_recv, recv) {
                        (true, Recv::Place(p)) => {
                            let (kc, keys) = self.place_keys(p);
                            let (p1, p2) = (self.t(), self.t());
                            let w1 = self.place_walk(p, &keys, &p1, "VIV_NO", "LC_UNIT", 0);
                            let w2 = self.place_walk(p, &keys, &p2, "VIV_NO", "LC_UNIT", 0);
                            let rk = Self::release_all(&keys);
                            let _ = write!(
                                c,
                                "lc_release({arr}[0]); {kc}{{ {w1}{arr}[0] = lc_take({p1}); }} lc_callsite = {site}; {r} = F{fid}({arr}); {{ {w2}lc_set({p2}, {arr}[0]); }} {rk}"
                            );
                        }
                        _ => {
                            let _ = write!(c, "lc_callsite = {site}; {r} = F{fid}({arr}); ");
                            for (i, p) in fd.params.iter().enumerate() {
                                if p.mode == Mode::Mut {
                                    let _ = write!(c, "lc_release({arr}[{i}]); ");
                                }
                            }
                        }
                    }
                    let _ = write!(code, "if ({k} == {fid}) {{ {c}}} else ");
                }
                if builtin {
                    let b = builtin_call(self);
                    let _ = write!(code, "{{ {b}}} ");
                } else {
                    let fd = &self.prog.fns[ids[0] as usize];
                    let want = fd.params.first().map_or("()".to_string(), |p| lacon_interp::eval::ty_name(&p.ty, self.prog));
                    let _ = write!(
                        code,
                        "{{ char kb[64]; lc_panic(\"E0301\", {site}, \"`%s` takes %s first, got %s\", {}, {}, lc_kind({rv}, kb)); }} ",
                        cstr(name),
                        cstr(&want)
                    );
                }
            }
        }
        let _ = write!(code, "lc_release({rv}); {r}; }})");
        code
    }

    fn lambda(&mut self, def: &LambdaDef) -> String {
        self.lambda_n += 1;
        let n = self.lambda_n;
        let reps = self.frame_reps(&def.body, def.nslots, &[], &def.params, &[]);
        let tys = self.slot_tys(&def.body);
        let saved = std::mem::replace(&mut self.cx, Cx::new(reps, tys));
        let heap = has_lambda(&def.body);
        let body = self.expr(&def.body);
        let max_up = self.cx.max_up.max(1);
        let natives = self.native_decls();
        let lsite = self.site(def.span);
        let params: Vec<String> = def.params.iter().enumerate().map(|(i, &s)| self.store(s, &format!("lc_retain(args[{i}])"), &lsite)).collect();
        self.cx = saved;
        let mut f = String::new();
        f.push_str(Self::macros(Kind::Nested));
        let _ = writeln!(f, "static lc_v L{n}(lc_fn *self, int argc, lc_v *args, const char *site) {{");
        let _ = writeln!(f, "    (void)argc; (void)site;\n    lc_frame *P1 = FRAME(self->env[0]);");
        for d in 2..=max_up {
            let _ = writeln!(f, "    lc_frame *P{d} = P{}->parent;", d - 1);
        }
        let (setup, teardown) = Self::frame_setup(def.nslots, heap, "P1");
        let _ = writeln!(f, "    lc_v ret = LC_UNIT;");
        f.push_str(&setup);
        f.push_str(&natives);
        for p in params {
            let _ = writeln!(f, "    {p}");
        }
        let _ = writeln!(f, "    ret = {body};\n    goto out;\nout:");
        f.push_str(&teardown);
        f.push_str("    return ret;\n}\n\n");
        // Emitted before the function that creates it.
        let _ = writeln!(self.statics, "static lc_v L{n}(lc_fn *self, int argc, lc_v *args, const char *site);");
        self.funcs.push_str(&f);
        format!("lc_closure_new(L{n}, {}, 1, (lc_v[]){{lc_retain(lc_obj_v(T_FRAME, FR))}})", def.params.len())
    }

    /// A lambda of one parameter, and no lambda of its own, as a C function
    /// `I{n}(P1, x)` that the loop calling it inlines. It is the closure's
    /// code without the closure: `P1` is the frame that made it, and a
    /// parameter the body only reads borrows `x`, which the list holds.
    fn inline_lambda(&mut self, def: &LambdaDef) -> Option<String> {
        if def.params.len() != 1 || has_lambda(&def.body) {
            return None;
        }
        self.lambda_n += 1;
        let n = self.lambda_n;
        let reps = self.frame_reps(&def.body, def.nslots, &[], &def.params, &[]);
        let tys = self.slot_tys(&def.body);
        let saved = std::mem::replace(&mut self.cx, Cx::new(reps, tys));
        let body = self.expr(&def.body);
        let max_up = self.cx.max_up.max(1);
        let natives = self.native_decls();
        let lsite = self.site(def.span);
        let p = def.params[0];
        let borrow = self.slot_rep(p) == Rep::Boxed && only_read(self.prog, &def.body, p);
        let param = if borrow { format!("S[{p}] = x; ") } else { self.store(p, "lc_retain(x)", &lsite) };
        self.cx = saved;
        let mut f = String::new();
        f.push_str(Self::macros(Kind::Nested));
        let _ = writeln!(f, "static inline __attribute__((always_inline)) lc_v I{n}(lc_frame *P1, lc_v x) {{");
        for d in 2..=max_up {
            let _ = writeln!(f, "    lc_frame *P{d} = P{}->parent;", d - 1);
        }
        let (setup, teardown) = Self::frame_setup(def.nslots, false, "P1");
        let _ = writeln!(f, "    lc_v ret = LC_UNIT;");
        f.push_str(&setup);
        f.push_str(&natives);
        let _ = writeln!(f, "    {param}");
        let _ = writeln!(f, "    ret = {body};\n    goto out;\nout:");
        if borrow {
            let _ = writeln!(f, "    S[{p}] = LC_UNIT;");
        }
        f.push_str(&teardown);
        f.push_str("    return ret;\n}\n\n");
        // Emitted before the function that calls it.
        let _ = writeln!(self.statics, "static inline __attribute__((always_inline)) lc_v I{n}(lc_frame *P1, lc_v x);");
        self.funcs.push_str(&f);
        Some(format!("I{n}"))
    }

    /// `map`, `filter` or `sort_by` on the list `rv`, given a lambda: a loop
    /// that runs the lambda inlined on each element, into `r`, where the
    /// runtime would call a closure. The lambda's entry counts toward the
    /// depth limit, as `lc_call` counts it. None for other methods and
    /// arguments.
    fn inline_loop(&mut self, name: &str, args: &[Ex], rv: &str, r: &str, site: &str) -> Option<String> {
        let ([Ex::Lambda(def)], "map" | "filter" | "sort_by") = (args, name) else {
            return None;
        };
        let f = self.inline_lambda(def)?;
        let (v, n, i, x, y) = (self.t(), self.t(), self.t(), self.t(), self.t());
        let unwind = format!("if (lc_unwinding) {{ lc_release({r}); lc_release({rv}); UNWIND; }} ");
        let (init, step, done) = match name {
            "map" => (format!("{r} = lc_list_new({n}); "), format!("lc_vec_push({r}, {y}); "), unwind),
            "filter" => (format!("{r} = lc_list_new(0); "), format!("if (lc_pred({y}, {site})) lc_vec_push({r}, lc_retain({x})); "), unwind),
            _ => {
                let kv = self.t();
                (
                    format!("lc_kv *{kv} = lc_pairs_new({n}); "),
                    format!("{kv}[{i}] = (lc_kv){{{y}, lc_retain({x})}}; "),
                    format!("if (lc_unwinding) {{ lc_pairs_free({kv}, {i}); lc_release({rv}); UNWIND; }} {r} = lc_sorted_pairs({kv}, {n}, true, {site}); "),
                )
            }
        };
        Some(format!(
            "lc_vec *{v} = VEC({rv}); int64_t {n} = {v}->len, {i} = 0; {init}if ({n} > 0) lc_enter_lambda({site}); for (; {i} < {n}; {i}++) {{ lc_v {x} = lc_vget({v}, {i}); lc_v {y} = {f}(FR, {x}); if (lc_unwinding) break; {step}}} if ({n} > 0) lc_depth--; {done}"
        ))
    }

    // ----- patterns -----

    /// A C condition that matches pattern `p` against the value `v` (a C
    /// expression without side effects) and binds slots as it goes. `site`
    /// is where an unboxed binding reports a value of the wrong type.
    fn pat(&mut self, p: &PatIr, v: &str, site: &str) -> String {
        match p {
            PatIr::Wild => "1".into(),
            PatIr::Bind(s) => {
                let st = self.store(*s, &format!("lc_retain({v})"), site);
                format!("({{ {st}1; }})")
            }
            PatIr::Lit(l) => {
                let lv = self.lit(l);
                format!("(lc_eq({v}, {lv}) == 1)")
            }
            PatIr::Range { lo, hi, inclusive } => {
                let lo_v = lo.as_ref().map_or("LC_UNIT".to_string(), |x| self.lit(x));
                let hi_v = hi.as_ref().map_or("LC_UNIT".to_string(), |x| self.lit(x));
                format!("lc_pat_range({v}, {}, {lo_v}, {}, {hi_v}, {inclusive})", lo.is_some(), hi.is_some())
            }
            PatIr::Tuple(ps) => {
                let mut c = format!("(({v}.tag == T_TUPLE || {v}.tag == T_LIST) && VEC({v})->len == {}", ps.len());
                for (i, sp) in ps.iter().enumerate() {
                    let sub = self.pat(sp, &format!("lc_vget(VEC({v}), {i})"), site);
                    let _ = write!(c, " && {sub}");
                }
                c.push(')');
                c
            }
            PatIr::List { items, rest } => {
                let len = if rest.is_some() { format!("VEC({v})->len >= {}", items.len()) } else { format!("VEC({v})->len == {}", items.len()) };
                let mut c = format!("(({v}.tag == T_LIST || {v}.tag == T_TUPLE) && {len}");
                for (i, sp) in items.iter().enumerate() {
                    let sub = self.pat(sp, &format!("lc_vget(VEC({v}), {i})"), site);
                    let _ = write!(c, " && {sub}");
                }
                if let Some(Some(slot)) = rest {
                    let s = self.slot(0, *slot);
                    let _ = write!(c, " && (lc_set(&{s}, lc_list_from({v}, {})), 1)", items.len());
                }
                c.push(')');
                c
            }
            PatIr::Variant { id, tag, args } => {
                let mut c = format!("({v}.tag == T_VARIANT && REC({v})->ty == {id} && REC({v})->tag == {tag}");
                for (i, sp) in args.iter().enumerate() {
                    let sub = self.pat(sp, &format!("REC({v})->f[{i}]"), site);
                    let _ = write!(c, " && {sub}");
                }
                c.push(')');
                c
            }
            PatIr::Struct { id, fields } => {
                let mut c = format!("({v}.tag == T_STRUCT && REC({v})->ty == {id}");
                for (i, sp) in fields {
                    let sub = self.pat(sp, &format!("REC({v})->f[{i}]"), site);
                    let _ = write!(c, " && {sub}");
                }
                c.push(')');
                c
            }
            PatIr::Or(ps) => {
                let subs: Vec<String> = ps.iter().map(|sp| self.pat(sp, v, site)).collect();
                format!("({})", subs.join(" || "))
            }
            PatIr::None => format!("({v}.tag == T_NONE)"),
            PatIr::Err(sp) => {
                let sub = self.pat(sp, &format!("ERRV({v})->payload"), site);
                format!("({v}.tag == T_ERR && {sub})")
            }
            PatIr::Ok(sp) => {
                let sub = self.pat(sp, v, site);
                format!("({v}.tag != T_ERR && {sub})")
            }
            PatIr::Some(sp) => {
                let sub = self.pat(sp, v, site);
                format!("({v}.tag != T_NONE && {sub})")
            }
        }
    }

    // ----- statements -----

    fn stmt(&mut self, s: &St) -> String {
        match s {
            St::Expr(e, span) if self.typed => {
                let site = self.site(*span);
                self.effect(e, &site)
            }
            St::Expr(e, span) => {
                match self.rep(e) {
                    Rep::Int if self.int_native(e) => return format!("(void)({}); ", self.int(e)),
                    Rep::Bool if self.bool_native(e) => return format!("(void)({}); ", self.boolean(e)),
                    Rep::Float if self.float_native(e) => return format!("(void)({}); ", self.float(e)),
                    _ => {}
                }
                let site = self.site(*span);
                let t = self.t();
                let v = self.expr(e);
                format!("{{ lc_v {t} = {v}; lc_check_ignored({t}, {site}); lc_release({t}); }} ")
            }
            St::Bind(PatIr::Bind(slot), e, ty, span) => {
                // `x f64 = 1` makes `x` a float, and `b u8 = n` checks `n`.
                let decl = ty.as_ref().filter(|t| !matches!(t, Ty::Any | Ty::Param(_)));
                match self.slot_rep(*slot) {
                    Rep::Int => {
                        let v = self.int(e);
                        let site = self.site(*span);
                        let check = decl.map_or(String::new(), |t| self.sized_check(t, &format!("N{slot}"), None, &site));
                        format!("N{slot} = {v}; {check}")
                    }
                    Rep::Bool => format!("N{slot} = {}; ", self.boolean(e)),
                    Rep::Float if decl.is_some() => format!("N{slot} = {}; ", self.num(e)),
                    Rep::Float => format!("N{slot} = {}; ", self.float(e)),
                    Rep::Boxed => {
                        let v = self.expr(e);
                        let v = match decl {
                            Some(t) if !self.exact(t, e) => {
                                let (ty, site) = (self.ty(t), self.site(*span));
                                format!("lc_coerce_var({v}, {ty}, NULL, {site})")
                            }
                            _ => v,
                        };
                        let s = self.slot(0, *slot);
                        format!("lc_set(&{s}, {v}); ")
                    }
                }
            }
            St::Bind(pat, e, _, span) => {
                let site = self.site(*span);
                let t = self.t();
                let v = self.expr(e);
                let cond = self.pat(pat, &t, &site);
                format!("{{ lc_v {t} = {v}; if (!({cond})) lc_bad_unpack({t}, -1, false, {site}); lc_release({t}); }} ")
            }
            St::Assign { place: Place { root: Root::Local(s), path, decl, name, .. }, op, value, span }
                if path.is_empty() && self.slot_rep(*s) != Rep::Boxed =>
            {
                let s = *s;
                let site = self.site(*span);
                let r = self.slot_rep(s);
                // A sized int's range, checked after the store, at the value
                // for `=` and at the statement for `op=`.
                let check = match decl {
                    Some(t) if r == Rep::Int => {
                        let csite = if op.is_none() { self.site(if value.span() == Span::default() { *span } else { value.span() }) } else { site.clone() };
                        self.sized_check(t, &format!("N{s}"), Some(name), &csite)
                    }
                    _ => String::new(),
                };
                let code = match op {
                    None if r == Rep::Int => format!("N{s} = {}; ", self.int(value)),
                    None if r == Rep::Float && decl.is_some() => format!("N{s} = {}; ", self.num(value)),
                    None if r == Rep::Float => format!("N{s} = {}; ", self.float(value)),
                    None => format!("N{s} = {}; ", self.boolean(value)),
                    Some(op) if r == Rep::Float => {
                        // A float stays one whatever number is added to it.
                        let (t, v) = (self.t(), self.num(value));
                        let new = match op {
                            BinOp::Rem => format!("fmod(N{s}, {t})"),
                            BinOp::Pow => format!("pow(N{s}, {t})"),
                            BinOp::Add => format!("N{s} + {t}"),
                            BinOp::Sub => format!("N{s} - {t}"),
                            BinOp::Mul => format!("N{s} * {t}"),
                            _ => format!("N{s} / {t}"),
                        };
                        format!("{{ double {t} = {v}; N{s} = {new}; }} ")
                    }
                    Some(op) if r == Rep::Int && self.rep(value) == Rep::Int && int_op(*op).is_some() => {
                        let f = int_op(*op).unwrap_or("+");
                        let v = self.int(value);
                        if f.len() == 1 {
                            format!("N{s} = N{s} {f} ({v}); ")
                        } else {
                            let t = self.t();
                            format!("{{ int64_t {t} = {v}; N{s} = {f}(N{s}, {t}, {site}); }} ")
                        }
                    }
                    Some(op) => {
                        // Bools under `&=` and the like: as the runtime does it.
                        let (t, old) = (self.t(), box_c(r, &format!("N{s}")));
                        let v = self.expr(value);
                        let st = self.store(s, &format!("lc_arith_own({}, {old}, {t}, {site})", binop_c(*op)), &site);
                        format!("{{ lc_v {t} = {v}; {st}}} ")
                    }
                };
                format!("{code}{check}")
            }
            St::Assign { place, op, value, span } => {
                let site = self.site(*span);
                if let Some(c) = self.assign_elem(place, *op, value, &site) {
                    return c;
                }
                if let Some(c) = self.assign_field(place, *op, value, &site) {
                    return c;
                }
                let v = self.t();
                let vsite = self.site(if value.span() == Span::default() { *span } else { value.span() });
                // A value the checker proves has the variable's declared
                // type needn't be checked again.
                let exact = place.path.is_empty() && place.decl.as_ref().is_some_and(|t| self.exact(t, value));
                let vv = self.expr(value);
                let st = match op {
                    None => self.store_place(place, &v, &vsite, exact),
                    Some(op) => self.update_place(place, *op, &v, &site),
                };
                let reread = self.view_reread(place);
                format!("{{ lc_v {v} = {vv}; {st}{reread}}} ")
            }
            St::AssignMulti { targets, value, span } => {
                let site = self.site(*span);
                let t = self.t();
                let vv = self.expr(value);
                let n = targets.len();
                let mut code = format!(
                    "{{ lc_v {t} = {vv}; if (!(({t}.tag == T_TUPLE || {t}.tag == T_LIST) && VEC({t})->len == {n})) lc_bad_unpack({t}, {n}, false, {site}); "
                );
                for (i, tg) in targets.iter().enumerate() {
                    match tg {
                        Target::Bind(slot) => {
                            let st = self.store(*slot, &format!("lc_retain(lc_vget(VEC({t}), {i}))"), &site);
                            code.push_str(&st);
                        }
                        Target::Place(p @ Place { root: Root::Local(slot), path, .. }) if path.is_empty() && self.slot_rep(*slot) != Rep::Boxed => {
                            let x = format!("lc_retain(lc_vget(VEC({t}), {i}))");
                            let x = self.conform_var(p, &x, &site);
                            let st = self.store(*slot, &x, &site);
                            code.push_str(&st);
                        }
                        Target::Place(p) => {
                            let x = self.t();
                            let st = self.store_place(p, &x, &site, false);
                            let _ = write!(code, "{{ lc_v {x} = lc_retain(lc_vget(VEC({t}), {i})); {st}}} ");
                        }
                    }
                }
                let _ = write!(code, "lc_release({t}); }} ");
                code
            }
            St::For { pat: pat @ (PatIr::Bind(_) | PatIr::Wild), iter: Ex::Range { start, end: Some(end), inclusive, span: rspan }, body, .. } => {
                // A range written in the loop: a C loop over its ints.
                let rsite = self.site(*rspan);
                let (a, b, i) = (self.t(), self.t(), self.t());
                let mut code = String::from("{ ");
                let start_int = start.as_ref().map_or(true, |x| self.rep(x) == Rep::Int);
                if start_int && self.rep(end) == Rep::Int {
                    let sv = start.as_ref().map_or("0".to_string(), |x| self.int(x));
                    let ev = self.int(end);
                    let _ = write!(code, "int64_t {a} = {sv}; int64_t {b} = {ev}; ");
                } else {
                    let (ta, tb) = (self.t(), self.t());
                    let sv = start.as_ref().map_or("lc_int(0)".to_string(), |x| self.expr(x));
                    let ev = self.expr(end);
                    let _ = write!(
                        code,
                        "lc_v {ta} = {sv}; lc_v {tb} = {ev}; int64_t {a} = lc_int_of({ta}, {rsite}); lc_release({ta}); int64_t {b} = lc_int_of({tb}, {rsite}); lc_release({tb}); "
                    );
                }
                if *inclusive {
                    let _ = write!(code, "{b} = (int64_t)((uint64_t){b} + 1); ");
                }
                let bind = match pat {
                    PatIr::Bind(slot) => match self.slot_rep(*slot) {
                        Rep::Int => format!("N{slot} = {i}; "),
                        _ => format!("lc_set(&{}, lc_int({i})); ", self.slot(0, *slot)),
                    },
                    _ => String::new(),
                };
                let (views, nv) = self.open_views(None, body);
                let mut bd = String::new();
                for s in body {
                    bd.push_str(&self.stmt(s));
                }
                self.close_views(nv);
                let _ = write!(code, "{views}for (int64_t {i} = {a}; {i} < {b}; {i}++) {{ {bind}{bd}}} }} ");
                code
            }
            St::For { pat, iter, body, span } => {
                let site = self.site(*span);
                let (iv, it, x) = (self.t(), self.t(), self.t());
                let ive = self.expr(iter);
                let keys_only = matches!(pat, PatIr::Bind(_) | PatIr::Wild);
                if let PatIr::Bind(slot) = pat {
                    if let Some(next) = match self.slot_rep(*slot) {
                        Rep::Int => Some("lc_iter_next_int"),
                        Rep::Bool => Some("lc_iter_next_bool"),
                        _ => None,
                    } {
                        // An unboxed loop variable: packed elements go straight into it.
                        let (views, nv) = self.open_views(None, body);
                        let mut b = String::new();
                        for s in body {
                            b.push_str(&self.stmt(s));
                        }
                        self.close_views(nv);
                        return format!(
                            "{{ lc_v {iv} = {ive}; lc_iter {it}; lc_iter_init(&{it}, {iv}, true, {site}); lc_release({iv}); {views}while ({next}(&{it}, &N{slot}, {site})) {{ {b}}} lc_iter_done(&{it}); }} "
                        );
                    }
                }
                let bind = match pat {
                    PatIr::Bind(slot) => self.store(*slot, &x, &site),
                    PatIr::Wild => format!("lc_release({x}); "),
                    _ => {
                        let cond = self.pat(pat, &x, &site);
                        format!("if (!({cond})) lc_bad_unpack({x}, -1, true, {site}); lc_release({x}); ")
                    }
                };
                let (views, nv) = self.open_views(None, body);
                let mut b = String::new();
                for s in body {
                    b.push_str(&self.stmt(s));
                }
                self.close_views(nv);
                format!(
                    "{{ lc_v {iv} = {ive}; lc_iter {it}; lc_iter_init(&{it}, {iv}, {keys_only}, {site}); lc_release({iv}); {views}lc_v {x}; while (lc_iter_next_fast(&{it}, &{x})) {{ {bind}{b}}} lc_iter_done(&{it}); }} "
                )
            }
            St::While { cond, body, .. } => {
                let site = self.site(cond.span());
                let (views, nv) = self.open_views(Some(cond), body);
                let c = self.cond_at(cond, &site);
                let mut b = String::new();
                for s in body {
                    b.push_str(&self.stmt(s));
                }
                self.close_views(nv);
                format!("{{ {views}while (1) {{ if (!{c}) break; {b}}} }} ")
            }
            St::Break(_) => "break; ".into(),
            St::Continue(_) => "continue; ".into(),
            St::Return(e, _) => {
                let v = match (e, self.cx.ret) {
                    (Some(x), Rep::Int) => self.int(x),
                    (Some(x), Rep::Bool) => self.boolean(x),
                    (Some(x), Rep::Float) => self.num(x),
                    (Some(x), Rep::Boxed) => self.expr(x),
                    (None, _) => "LC_UNIT".to_string(),
                };
                format!("RETURN({v}) ")
            }
            St::Fail(e, _) => {
                let t = self.t();
                let v = self.expr(e);
                format!("{{ lc_v {t} = {v}; if ({t}.tag != T_ERR) {t} = lc_err_new({t}); RETURN({t}) }} ")
            }
            St::Assert { cond, msg, span } => {
                let site = self.site(*span);
                let t = self.t();
                let cv = self.cond_at(cond, &site);
                let fail = match msg {
                    Some(m) => {
                        let mt = self.t();
                        let mv = self.expr(m);
                        format!("{{ lc_v {mt} = {mv}; lc_assert_fail({mt}, true, {site}); }}")
                    }
                    None => format!("lc_assert_fail(LC_UNIT, false, {site});"),
                };
                format!("{{ bool {t} = {cv}; if (!{t}) {fail} }} ")
            }
        }
    }
}

/// A C condition under which a value already has a declared type exactly,
/// so the boundary check would leave it alone.
fn exact_tag(t: &Ty, v: &str) -> Option<String> {
    Some(match t {
        Ty::Int { min, max, .. } if *min == i64::MIN && *max == i64::MAX => format!("({v}.tag == T_INT)"),
        Ty::Float => format!("({v}.tag == T_FLOAT)"),
        Ty::Bool => format!("({v}.tag == T_BOOL)"),
        Ty::Str => format!("({v}.tag == T_STR)"),
        Ty::Map(..) => format!("({v}.tag == T_MAP)"),
        Ty::Set(_) => format!("({v}.tag == T_SET)"),
        Ty::Heap(_) => format!("({v}.tag == T_HEAP)"),
        Ty::List(e) if !matches!(**e, Ty::Float) => format!("({v}.tag == T_LIST)"),
        Ty::Struct(id, _) => format!("({v}.tag == T_STRUCT && REC({v})->ty == {id})"),
        Ty::Enum(id, _) => format!("({v}.tag == T_VARIANT && REC({v})->ty == {id})"),
        _ => return None,
    })
}

/// A C initializer for a parsed format spec.
fn fmt_init(s: &FmtSpec) -> String {
    let ch = |c: Option<char>| c.map_or("0".to_string(), |c| format!("'{}'", c));
    format!(
        "(lc_fmt){{{}, {}, '{}', {}, {}, {}, {}, {}, {}}}",
        s.fill.map_or("-1".to_string(), |c| (c as u32).to_string()),
        ch(s.align),
        s.sign,
        s.alt,
        s.zero,
        s.width,
        ch(s.grouping),
        s.precision.map_or("-1".to_string(), |p| p.to_string()),
        ch(s.kind),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn method_enum_matches_methods() {
        let h = super::HEADER;
        let start = h.find("M_str, M_to_string").unwrap();
        let end = h.find("M_COUNT").unwrap();
        let names: Vec<&str> = h[start..end].split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).map(|s| s.trim_start_matches("M_")).collect();
        assert_eq!(names, lacon_interp::builtins::METHODS);
    }
}
