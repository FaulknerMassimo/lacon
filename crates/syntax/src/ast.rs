use crate::span::Span;

#[derive(Clone, Debug)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

#[derive(Debug, Default)]
pub struct Module {
    pub items: Vec<Item>,
    /// Names of declarations that failed to parse, so errors about them
    /// elsewhere can be recognized as follow-on noise.
    pub broken: Vec<String>,
    /// A `type` or `enum` declaration failed to parse, so field names are
    /// unknown.
    pub broken_type: bool,
}

#[derive(Debug)]
pub enum Item {
    Fn(FnDecl),
    Struct(StructDecl),
    Enum(EnumDecl),
    Alias(AliasDecl),
    Test(TestDecl),
    Const(ConstDecl),
}

#[derive(Debug)]
pub struct FnDecl {
    pub name: Ident,
    pub generics: Vec<Generic>,
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    pub body: Block,
    pub doc: Option<String>,
    pub span: Span,
    /// From `fn` to the `=` before the body.
    pub head: Span,
}

#[derive(Clone, Debug)]
pub struct Generic {
    pub name: Ident,
    pub bounds: Vec<Ident>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Borrow,
    Mut,
    Own,
}

#[derive(Debug)]
pub struct Param {
    pub mode: Mode,
    pub name: Ident,
    pub ty: TypeExpr,
    pub default: Option<Expr>,
}

#[derive(Debug)]
pub struct StructDecl {
    pub name: Ident,
    pub generics: Vec<Generic>,
    pub fields: Vec<FieldDecl>,
    pub doc: Option<String>,
    pub span: Span,
}

#[derive(Debug)]
pub struct FieldDecl {
    pub name: Ident,
    pub ty: TypeExpr,
    pub default: Option<Expr>,
}

#[derive(Debug)]
pub struct EnumDecl {
    pub name: Ident,
    pub generics: Vec<Generic>,
    pub variants: Vec<Variant>,
    pub doc: Option<String>,
    pub span: Span,
}

#[derive(Debug)]
pub struct Variant {
    pub name: Ident,
    pub fields: Vec<TypeExpr>,
}

#[derive(Debug)]
pub struct AliasDecl {
    pub name: Ident,
    pub ty: TypeExpr,
    pub span: Span,
}

#[derive(Debug)]
pub struct TestDecl {
    pub name: String,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug)]
pub struct ConstDecl {
    pub name: Ident,
    pub value: Expr,
}

#[derive(Clone, Debug)]
pub enum TypeExpr {
    /// `int`, `User`, `Pair[A, B]`
    Name(Ident, Vec<TypeExpr>),
    List(Box<TypeExpr>, Span),
    Map(Box<TypeExpr>, Box<TypeExpr>, Span),
    Set(Box<TypeExpr>, Span),
    Tuple(Vec<TypeExpr>, Span),
    Optional(Box<TypeExpr>),
    /// `T!` or `T!E`
    Result(Box<TypeExpr>, Option<Box<TypeExpr>>),
    Fn(Vec<TypeExpr>, Option<Box<TypeExpr>>, Span),
}

impl TypeExpr {
    pub fn span(&self) -> Span {
        match self {
            TypeExpr::Name(id, _) => id.span,
            TypeExpr::List(_, s) | TypeExpr::Map(_, _, s) | TypeExpr::Set(_, s) | TypeExpr::Tuple(_, s) | TypeExpr::Fn(_, _, s) => *s,
            TypeExpr::Optional(t) | TypeExpr::Result(t, _) => t.span(),
        }
    }

    pub fn unit() -> TypeExpr {
        TypeExpr::Tuple(vec![], Span::default())
    }
}

impl std::fmt::Display for TypeExpr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TypeExpr::Name(id, args) => {
                write!(f, "{}", id.name)?;
                if !args.is_empty() {
                    write!(f, "[")?;
                    for (i, a) in args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{a}")?;
                    }
                    write!(f, "]")?;
                }
                Ok(())
            }
            TypeExpr::List(t, _) => write!(f, "[{t}]"),
            TypeExpr::Map(k, v, _) => write!(f, "{{{k}:{v}}}"),
            TypeExpr::Set(t, _) => write!(f, "{{{t}}}"),
            TypeExpr::Tuple(ts, _) => {
                write!(f, "(")?;
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{t}")?;
                }
                write!(f, ")")
            }
            TypeExpr::Optional(t) => write!(f, "{t}?"),
            TypeExpr::Result(t, e) => match e {
                Some(e) => write!(f, "{t}!{e}"),
                None => write!(f, "{t}!"),
            },
            TypeExpr::Fn(ps, r, _) => {
                write!(f, "fn(")?;
                for (i, t) in ps.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{t}")?;
                }
                write!(f, ")")?;
                if let Some(r) = r {
                    write!(f, " {r}")?;
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug)]
pub enum Stmt {
    Expr(Expr),
    /// `var x [T] = e`, or `x T = e` (also `x: T = e`): an immutable binding
    /// with a declared type.
    Var { name: Ident, ty: Option<TypeExpr>, value: Expr, mutable: bool },
    /// `x = e`, `a.b[i] += e`, `a, b = e`. Whether `x = e` binds or assigns
    /// is decided during name resolution.
    Assign { targets: Vec<Expr>, op: Option<BinOp>, value: Expr, span: Span },
    For { pat: Pat, iter: Expr, body: Block, span: Span },
    While { cond: Expr, body: Block, span: Span },
    Break(Span),
    Continue(Span),
    Return(Option<Expr>, Span),
    Fail(Expr, Span),
    Assert { cond: Expr, msg: Option<Expr>, span: Span },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

impl BinOp {
    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
            BinOp::Pow => "**",
            BinOp::BitAnd => "&",
            BinOp::BitOr => "|",
            BinOp::BitXor => "^",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    In,
    NotIn,
}

impl CmpOp {
    pub fn symbol(self) -> &'static str {
        match self {
            CmpOp::Eq => "==",
            CmpOp::Ne => "!=",
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
            CmpOp::In => "in",
            CmpOp::NotIn => "not in",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    /// `!x`: logical not on bool, bitwise not on ints.
    Bang,
}

#[derive(Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug)]
pub enum StrSeg {
    Lit(String),
    /// `{e:spec}`, with the computed width and precision of the spec, in
    /// that order, when it has them (`{e:>{w}}`).
    Expr(Box<Expr>, Option<String>, Vec<Expr>),
}

#[derive(Debug)]
pub struct FieldInit {
    pub name: Option<Ident>,
    pub value: Expr,
}

#[derive(Debug)]
pub struct MatchArm {
    pub pat: Pat,
    pub guard: Option<Expr>,
    pub body: Block,
}

#[derive(Debug)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Str(Vec<StrSeg>),
    Bool(bool),
    None,
    Name(String),
    List(Vec<Expr>),
    Map(Vec<(Expr, Expr)>),
    Set(Vec<Expr>),
    Tuple(Vec<Expr>),
    StructLit { name: Ident, fields: Vec<FieldInit> },
    /// `a.b`, including tuple fields `t.0`.
    Field { obj: Box<Expr>, name: Ident },
    Index { obj: Box<Expr>, index: Box<Expr> },
    Slice { obj: Box<Expr>, start: Option<Box<Expr>>, end: Option<Box<Expr>>, inclusive: bool },
    Call { callee: Box<Expr>, args: Vec<Expr> },
    Method { obj: Box<Expr>, name: Ident, args: Vec<Expr> },
    Unary { op: UnOp, expr: Box<Expr> },
    Binary { op: BinOp, lhs: Box<Expr>, rhs: Box<Expr> },
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Coalesce(Box<Expr>, Box<Expr>),
    /// `a < b`, or a chain `a < b <= c`.
    Compare { first: Box<Expr>, rest: Vec<(CmpOp, Expr)> },
    Range { start: Option<Box<Expr>>, end: Option<Box<Expr>>, inclusive: bool },
    Try(Box<Expr>),
    Lambda { params: Vec<Ident>, body: Box<Expr> },
    If { cond: Box<Expr>, then: Block, els: Option<Block> },
    Match { scrutinee: Box<Expr>, arms: Vec<MatchArm> },
    Block(Block),
    Cast { expr: Box<Expr>, ty: TypeExpr },
}

#[derive(Debug)]
pub enum Pat {
    Wild(Span),
    /// A name. Resolution decides whether it binds or names a unit variant.
    Name(Ident),
    Lit(Expr),
    Range { lo: Option<Expr>, hi: Option<Expr>, inclusive: bool, span: Span },
    Tuple(Vec<Pat>, Span),
    List { items: Vec<Pat>, rest: Option<Option<Ident>>, span: Span },
    Variant { name: Ident, qualifier: Option<Ident>, args: Vec<Pat> },
    Struct { name: Ident, fields: Vec<(Ident, Pat)>, span: Span },
    Or(Vec<Pat>, Span),
    None(Span),
    Err(Box<Pat>, Span),
    Ok(Box<Pat>, Span),
}

impl Pat {
    pub fn span(&self) -> Span {
        match self {
            Pat::Wild(s) | Pat::None(s) => *s,
            Pat::Name(id) => id.span,
            Pat::Lit(e) => e.span,
            Pat::Range { span, .. } | Pat::Tuple(_, span) | Pat::List { span, .. } | Pat::Struct { span, .. } | Pat::Or(_, span) => *span,
            Pat::Variant { name, .. } => name.span,
            Pat::Err(_, s) | Pat::Ok(_, s) => *s,
        }
    }
}
