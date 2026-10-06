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
}

pub struct FnDef {
    pub name: String,
    pub params: Vec<ParamDef>,
    pub ret: Option<Ty>,
    pub body: Ex,
    pub nslots: u32,
    pub span: Span,
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
    pub fields: Vec<FieldDef>,
}

pub struct FieldDef {
    pub name: String,
    pub ty: Ty,
    pub default: Option<Rc<LambdaDef>>,
}

pub struct EnumDef {
    pub name: String,
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

/// A declared type, checked shallowly at function, struct and return
/// boundaries.
#[derive(Clone, Debug)]
pub enum Ty {
    Any,
    Int { name: &'static str, min: i64, max: i64 },
    Float,
    Bool,
    Str,
    Unit,
    List(Box<Ty>),
    Map(Box<Ty>, Box<Ty>),
    Set(Box<Ty>),
    Tuple(Vec<Ty>),
    Optional(Box<Ty>),
    Result(Box<Ty>),
    Fn,
    Struct(u32),
    Enum(u32),
}

pub struct LambdaDef {
    pub params: Vec<u32>,
    pub nslots: u32,
    pub body: Ex,
    pub span: Span,
}

pub enum StrPiece {
    Lit(Rc<str>),
    Expr(Ex, Option<FmtSpec>),
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
    Lit(Value),
    Str(Vec<StrPiece>),
    Local(u32),
    Up(u32, u32),
    Global(u32),
    List(Vec<Ex>),
    Map(Vec<(Ex, Ex)>),
    Set(Vec<Ex>),
    Tuple(Vec<Ex>),
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
    Coalesce(Box<Ex>, Box<Ex>),
    Compare { first: Box<Ex>, rest: Vec<(CmpOp, Ex, Span)> },
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
}

pub struct Arm {
    pub pat: PatIr,
    pub guard: Option<Ex>,
    pub body: Ex,
}

pub enum St {
    Expr(Ex, Span),
    Bind(PatIr, Ex, Span),
    Assign { place: Place, op: Option<BinOp>, value: Ex, span: Span },
    /// `a, b = b, a` where some targets are existing variables.
    AssignMulti { targets: Vec<Target>, value: Ex, span: Span },
    For { pat: PatIr, iter: Ex, body: Vec<St>, span: Span },
    While { cond: Ex, body: Vec<St> },
    Break,
    Continue,
    Return(Option<Ex>),
    Fail(Ex, Span),
    Assert { cond: Ex, msg: Option<Ex>, span: Span },
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
}
