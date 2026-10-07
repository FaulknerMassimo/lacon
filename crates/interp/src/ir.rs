//! The resolved program the interpreter runs: names are frame slots, `it`
//! arguments are explicit lambdas, and assignment targets are places.

use std::collections::HashMap;
use std::rc::Rc;

use lacon_syntax::ast::{BinOp, CmpOp, Mode, UnOp};
use lacon_syntax::fmtspec::FmtSpec;
use lacon_syntax::Span;

use crate::value::Value;

pub type FnId = u32;

#[derive(Default)]
pub struct Program {
    pub fns: Vec<FnDef>,
    pub structs: Vec<StructDef>,
    pub enums: Vec<EnumDef>,
    pub tests: Vec<TestDef>,
    pub consts: Vec<ConstDef>,
    pub fn_names: HashMap<String, Rc<[FnId]>>,
    pub main: Option<FnId>,
    /// `s.parse()` calls whose type the checker knows from a declared type
    /// (`n int = s.parse()?`), by span: they parse to that type or fail.
    pub parse_to: HashMap<Span, ConvTo>,
}

pub struct FnDef {
    pub name: String,
    /// Generic parameter names, in declaration order.
    pub generics: Vec<String>,
    pub params: Vec<ParamDef>,
    pub ret: Option<Ty>,
    pub body: Ex,
    pub nslots: u32,
    pub span: Span,
    /// From `fn` to the `=` before the body, which a fix that changes the
    /// signature replaces.
    pub head: Span,
    /// Signature as written, for `lacon sig`.
    pub sig: String,
    pub doc: Option<String>,
    /// Does I/O, directly or through a call (for `lacon sig`).
    pub io: bool,
}

pub struct ParamDef {
    pub name: String,
    pub ty: Ty,
    pub mode: Mode,
    /// Defaults are zero-parameter lambdas evaluated at each call.
    pub default: Option<Rc<LambdaDef>>,
}

pub struct StructDef {
    pub name: String,
    pub generics: Vec<String>,
    pub fields: Vec<FieldDef>,
}

pub struct FieldDef {
    pub name: String,
    pub ty: Ty,
    pub default: Option<Rc<LambdaDef>>,
}

pub struct EnumDef {
    pub name: String,
    pub generics: Vec<String>,
    pub variants: Vec<VariantDef>,
}

pub struct VariantDef {
    pub name: String,
    pub fields: Vec<Ty>,
}

pub struct TestDef {
    pub name: String,
    pub body: Ex,
    pub nslots: u32,
    pub span: Span,
}

pub struct ConstDef {
    pub name: String,
    pub value: Ex,
    pub nslots: u32,
}

/// A declared type. The interpreter checks it shallowly at function, struct
/// and return boundaries; the type checker uses all of it.
#[derive(Clone, Debug)]
pub enum Ty {
    Any,
    /// A generic parameter of the enclosing declaration.
    Param(Rc<str>),
    Int { name: &'static str, min: i64, max: i64 },
    Float,
    Bool,
    Str,
    Unit,
    List(Box<Ty>),
    Map(Box<Ty>, Box<Ty>),
    Set(Box<Ty>),
    Heap(Box<Ty>),
    Tuple(Vec<Ty>),
    Optional(Box<Ty>),
    Result(Box<Ty>),
    Fn(Vec<Ty>, Box<Ty>),
    Struct(u32, Vec<Ty>),
    Enum(u32, Vec<Ty>),
}

pub struct LambdaDef {
    pub params: Vec<u32>,
    pub nslots: u32,
    pub body: Ex,
    pub span: Span,
}

pub enum StrPiece {
    Lit(Rc<str>),
    /// An interpolation, its spec, and the spec's computed width and
    /// precision, in that order.
    Expr(Ex, Option<Box<FmtSpec>>, Vec<Ex>),
}

pub enum Root {
    Local(u32),
    Up(u32, u32),
}

pub enum Seg {
    Field(Rc<str>, Span),
    Index(Ex, Span),
}

/// An assignable location: a variable followed by fields and indexes.
pub struct Place {
    pub root: Root,
    pub name: Rc<str>,
    pub mutable: bool,
    pub path: Vec<Seg>,
    pub span: Span,
}

pub enum Recv {
    Place(Place),
    Value(Box<Ex>),
}

/// Namespaced standard library functions and values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Builtin {
    Print,
    Eprint,
    Range,
    Min,
    Max,
    SetNew,
    HeapNew,
    ListNew,
    Conv(ConvTo),
    Panic,
    FsRead,
    FsWrite,
    FsAppend,
    FsExists,
    FsLines,
    FsRemove,
    IoRead,
    IoLines,
    IoReadLine,
    IoWrite,
    OsArgs,
    OsEnv,
    OsExit,
    TimeNow,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ConvTo {
    Int(&'static str, i64, i64),
    Float,
    Str,
    Bool,
}

pub enum Ex {
    Lit(Value, Span),
    Str(Vec<StrPiece>, Span),
    Local(u32, Span),
    Up(u32, u32, Span),
    Global(u32, Span),
    List(Vec<Ex>, Span),
    Map(Vec<(Ex, Ex)>, Span),
    Set(Vec<Ex>, Span),
    Tuple(Vec<Ex>, Span),
    Struct { id: u32, fields: Vec<Option<Ex>>, span: Span },
    Variant { id: u32, tag: u32, args: Vec<Ex>, span: Span },
    /// `a.b`: a struct field, a tuple index, or a zero-argument method.
    Field { obj: Box<Ex>, name: Rc<str>, user: Option<Rc<[FnId]>>, span: Span },
    Index { obj: Box<Ex>, index: Box<Ex>, span: Span },
    Slice { obj: Box<Ex>, start: Option<Box<Ex>>, end: Option<Box<Ex>>, inclusive: bool, span: Span },
    CallFn { fns: Rc<[FnId]>, args: Vec<Ex>, places: Vec<Option<Place>>, span: Span },
    CallValue { f: Box<Ex>, args: Vec<Ex>, span: Span },
    CallBuiltin { f: Builtin, args: Vec<Ex>, span: Span },
    Method { recv: Recv, name: Rc<str>, user: Option<Rc<[FnId]>>, args: Vec<Ex>, span: Span },
    Unary { op: UnOp, e: Box<Ex>, span: Span },
    Binary { op: BinOp, l: Box<Ex>, r: Box<Ex>, span: Span },
    And(Box<Ex>, Box<Ex>, Span),
    Or(Box<Ex>, Box<Ex>, Span),
    Coalesce(Box<Ex>, Box<Ex>, Span),
    Compare { first: Box<Ex>, rest: Vec<(CmpOp, Ex, Span)>, span: Span },
    Range { start: Option<Box<Ex>>, end: Option<Box<Ex>>, inclusive: bool, span: Span },
    Try { e: Box<Ex>, fn_optional: bool, span: Span },
    Lambda(Rc<LambdaDef>),
    If { cond: Box<Ex>, then: Box<Ex>, els: Option<Box<Ex>>, span: Span },
    Match { scrut: Box<Ex>, arms: Vec<Arm>, span: Span },
    Block(Vec<St>, Option<Box<Ex>>),
    Cast { e: Box<Ex>, to: ConvTo, span: Span },
    /// `a == b` as a test's last expression or an `assert`: on failure,
    /// reports both sides.
    AssertEq { l: Box<Ex>, r: Box<Ex>, msg: Option<Box<Ex>>, span: Span },
    /// Stands in for an expression that failed to resolve; never evaluated,
    /// because a program with errors does not run.
    Poison(Span),
}

impl Ex {
    pub fn span(&self) -> Span {
        match self {
            Ex::Lit(_, s)
            | Ex::Str(_, s)
            | Ex::Local(_, s)
            | Ex::Up(_, _, s)
            | Ex::Global(_, s)
            | Ex::List(_, s)
            | Ex::Map(_, s)
            | Ex::Set(_, s)
            | Ex::Tuple(_, s)
            | Ex::Coalesce(_, _, s)
            | Ex::And(_, _, s)
            | Ex::Or(_, _, s)
            | Ex::Poison(s) => *s,
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
            | Ex::Compare { span, .. }
            | Ex::Range { span, .. }
            | Ex::Try { span, .. }
            | Ex::If { span, .. }
            | Ex::Match { span, .. }
            | Ex::Cast { span, .. }
            | Ex::AssertEq { span, .. } => *span,
            Ex::Lambda(d) => d.span,
            Ex::Block(stmts, tail) => match (stmts.first(), tail) {
                (_, Some(t)) => t.span(),
                (Some(s), None) => s.span(),
                (None, None) => Span::default(),
            },
        }
    }
}

pub struct Arm {
    pub pat: PatIr,
    pub guard: Option<Ex>,
    pub body: Ex,
}

pub enum St {
    Expr(Ex, Span),
    /// A binding, with the declared type of `var x T = e`.
    Bind(PatIr, Ex, Option<Ty>, Span),
    Assign { place: Place, op: Option<BinOp>, value: Ex, span: Span },
    /// `a, b = b, a` where some targets are existing variables.
    AssignMulti { targets: Vec<Target>, value: Ex, span: Span },
    For { pat: PatIr, iter: Ex, body: Vec<St>, span: Span },
    While { cond: Ex, body: Vec<St>, span: Span },
    Break(Span),
    Continue(Span),
    Return(Option<Ex>, Span),
    Fail(Ex, Span),
    Assert { cond: Ex, msg: Option<Ex>, span: Span },
}

impl St {
    pub fn span(&self) -> Span {
        match self {
            St::Expr(_, s) | St::Bind(_, _, _, s) | St::Break(s) | St::Continue(s) | St::Return(_, s) | St::Fail(_, s) => *s,
            St::Assign { span, .. } | St::AssignMulti { span, .. } | St::For { span, .. } | St::While { span, .. } | St::Assert { span, .. } => *span,
        }
    }
}

pub enum Target {
    Bind(u32),
    Place(Place),
}

pub enum PatIr {
    Wild,
    Bind(u32),
    Lit(Value),
    Range { lo: Option<Value>, hi: Option<Value>, inclusive: bool },
    Tuple(Vec<PatIr>),
    List { items: Vec<PatIr>, rest: Option<Option<u32>> },
    Variant { id: u32, tag: u32, args: Vec<PatIr> },
    Struct { id: u32, fields: Vec<(usize, PatIr)> },
    Or(Vec<PatIr>),
    None,
    Err(Box<PatIr>),
    Ok(Box<PatIr>),
    /// `some(p)`: anything but `none`, matched against `p`.
    Some(Box<PatIr>),
}

/// What the walk visits: expressions, statements, and places with how they
/// are used.
pub enum Node<'a> {
    Ex(&'a Ex),
    St(&'a St),
    /// A place, and whether a builtin method changes it in place.
    Place(&'a Place, bool),
}

/// Visits every expression and place under `e`. `d` counts the lambdas
/// entered on the way, so `Ex::Up(d, _)` at depth `d` names a slot of the
/// frame the walk started in.
pub fn walk_ex<'a>(e: &'a Ex, d: u32, f: &mut dyn FnMut(Node<'a>, u32)) {
    f(Node::Ex(e), d);
    match e {
        Ex::Str(ps, _) => {
            for p in ps {
                if let StrPiece::Expr(x, _, args) = p {
                    walk_ex(x, d, f);
                    for a in args {
                        walk_ex(a, d, f);
                    }
                }
            }
        }
        Ex::List(xs, _) | Ex::Set(xs, _) | Ex::Tuple(xs, _) => xs.iter().for_each(|x| walk_ex(x, d, f)),
        Ex::Map(ps, _) => ps.iter().for_each(|(k, v)| {
            walk_ex(k, d, f);
            walk_ex(v, d, f);
        }),
        Ex::Struct { fields, .. } => fields.iter().flatten().for_each(|x| walk_ex(x, d, f)),
        Ex::Variant { args, .. } | Ex::CallBuiltin { args, .. } => args.iter().for_each(|x| walk_ex(x, d, f)),
        Ex::CallFn { args, places, .. } => {
            args.iter().for_each(|x| walk_ex(x, d, f));
            for p in places.iter().flatten() {
                walk_place(p, false, d, f);
            }
        }
        Ex::Field { obj, .. } => walk_ex(obj, d, f),
        Ex::Index { obj, index, .. } => {
            walk_ex(obj, d, f);
            walk_ex(index, d, f);
        }
        Ex::Slice { obj, start, end, .. } => {
            walk_ex(obj, d, f);
            for x in [start, end].into_iter().flatten() {
                walk_ex(x, d, f);
            }
        }
        Ex::CallValue { f: callee, args, .. } => {
            walk_ex(callee, d, f);
            args.iter().for_each(|x| walk_ex(x, d, f));
        }
        Ex::Method { recv, args, name, .. } => {
            match recv {
                Recv::Place(p) => walk_place(p, crate::builtins::is_mutator(name), d, f),
                Recv::Value(x) => walk_ex(x, d, f),
            }
            args.iter().for_each(|x| walk_ex(x, d, f));
        }
        Ex::Unary { e, .. } | Ex::Try { e, .. } | Ex::Cast { e, .. } => walk_ex(e, d, f),
        Ex::Binary { l, r, .. } | Ex::AssertEq { l, r, .. } => {
            walk_ex(l, d, f);
            walk_ex(r, d, f);
            if let Ex::AssertEq { msg: Some(m), .. } = e {
                walk_ex(m, d, f);
            }
        }
        Ex::And(a, b, _) | Ex::Or(a, b, _) | Ex::Coalesce(a, b, _) => {
            walk_ex(a, d, f);
            walk_ex(b, d, f);
        }
        Ex::Compare { first, rest, .. } => {
            walk_ex(first, d, f);
            rest.iter().for_each(|(_, x, _)| walk_ex(x, d, f));
        }
        Ex::Range { start, end, .. } => {
            for x in [start, end].into_iter().flatten() {
                walk_ex(x, d, f);
            }
        }
        Ex::Lambda(def) => walk_ex(&def.body, d + 1, f),
        Ex::If { cond, then, els, .. } => {
            walk_ex(cond, d, f);
            walk_ex(then, d, f);
            if let Some(x) = els {
                walk_ex(x, d, f);
            }
        }
        Ex::Match { scrut, arms, .. } => {
            walk_ex(scrut, d, f);
            for a in arms {
                if let Some(g) = &a.guard {
                    walk_ex(g, d, f);
                }
                walk_ex(&a.body, d, f);
            }
        }
        Ex::Block(stmts, tail) => {
            walk_stmts(stmts, d, f);
            if let Some(t) = tail {
                walk_ex(t, d, f);
            }
        }
        _ => {}
    }
}

pub fn walk_stmts<'a>(ss: &'a [St], d: u32, f: &mut dyn FnMut(Node<'a>, u32)) {
    for s in ss {
        f(Node::St(s), d);
        match s {
            St::Expr(e, _) | St::Fail(e, _) | St::Bind(_, e, _, _) => walk_ex(e, d, f),
            St::Assign { place, value, .. } => {
                walk_place(place, false, d, f);
                walk_ex(value, d, f);
            }
            St::AssignMulti { targets, value, .. } => {
                for t in targets {
                    if let Target::Place(p) = t {
                        walk_place(p, false, d, f);
                    }
                }
                walk_ex(value, d, f);
            }
            St::For { iter, body, .. } => {
                walk_ex(iter, d, f);
                walk_stmts(body, d, f);
            }
            St::While { cond, body, .. } => {
                walk_ex(cond, d, f);
                walk_stmts(body, d, f);
            }
            St::Return(Some(e), _) => walk_ex(e, d, f),
            St::Assert { cond, msg, .. } => {
                walk_ex(cond, d, f);
                if let Some(m) = msg {
                    walk_ex(m, d, f);
                }
            }
            _ => {}
        }
    }
}

pub fn walk_place<'a>(p: &'a Place, mutated: bool, d: u32, f: &mut dyn FnMut(Node<'a>, u32)) {
    f(Node::Place(p, mutated), d);
    for s in &p.path {
        if let Seg::Index(e, _) = s {
            walk_ex(e, d, f);
        }
    }
}
