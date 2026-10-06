//! Name resolution and lowering from the AST to the IR, plus the static checks
//! the Phase 0 interpreter can do without a type checker: undefined names,
//! assignment to immutable bindings, unknown methods and fields, arity,
//! discarded results of pure methods, and enum match exhaustiveness.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use lacon_syntax::ast::*;
use lacon_syntax::fmtspec::FmtSpec;
use lacon_syntax::{Diag, Span};

use crate::builtins::{hof_arg, is_method, is_mutator};
use crate::ir::{self, Arm, Builtin, ConvTo, Ex, FnId, LambdaDef, PatIr, Place, Program, Recv, Root, Seg, St, StrPiece, Target, Ty};
use crate::value::{Func, Value};

#[derive(Clone)]
struct Binding {
    slot: u32,
    mutable: bool,
}

#[derive(Default)]
struct Ctx {
    scopes: Vec<HashMap<String, Binding>>,
    nslots: u32,
    /// The enclosing named function returns `T?`.
    ret_optional: bool,
    loops: u32,
    /// Names bound in blocks that have ended, for the "bound inside a block" hint.
    closed: HashSet<String>,
}

struct FnInfo {
    params: Vec<(Mode, bool)>, // (mode, is a function type)
    required: usize,
}

pub struct Resolver<'a> {
    src: &'a str,
    pub diags: Vec<Diag>,
    prog: Program,
    struct_ids: HashMap<String, u32>,
    enum_ids: HashMap<String, u32>,
    aliases: HashMap<String, TypeExpr>,
    variants: HashMap<String, Vec<(u32, u32)>>,
    fn_ids: HashMap<String, Vec<FnId>>,
    fn_info: Vec<FnInfo>,
    const_ids: HashMap<String, u32>,
    field_names: HashSet<String>,
    ctxs: Vec<Ctx>,
    // Effects: direct I/O and callees per function, for `lacon sig`.
    cur_fn: Option<FnId>,
    io: Vec<bool>,
    calls: Vec<HashSet<FnId>>,
}

const INT_TYPES: &[(&str, i64, i64)] = &[
    ("int", i64::MIN, i64::MAX),
    ("i64", i64::MIN, i64::MAX),
    ("usize", 0, i64::MAX),
    ("isize", i64::MIN, i64::MAX),
    ("i32", i32::MIN as i64, i32::MAX as i64),
    ("i16", i16::MIN as i64, i16::MAX as i64),
    ("i8", i8::MIN as i64, i8::MAX as i64),
    ("u64", 0, i64::MAX),
    ("u32", 0, u32::MAX as i64),
    ("u16", 0, u16::MAX as i64),
    ("u8", 0, u8::MAX as i64),
];

fn int_type(name: &str) -> Option<(&'static str, i64, i64)> {
    INT_TYPES.iter().find(|t| t.0 == name).copied()
}

pub(crate) fn conv_for(name: &str) -> Option<ConvTo> {
    if let Some((n, lo, hi)) = int_type(name) {
        return Some(ConvTo::Int(n, lo, hi));
    }
    Some(match name {
        "f64" | "f32" => ConvTo::Float,
        "str" => ConvTo::Str,
        "bool" => ConvTo::Bool,
        _ => return None,
    })
}

const NAMESPACES: &[&str] = &["fs", "io", "os", "math", "time"];

fn type_hint(name: &str) -> Option<&'static str> {
    Some(match name {
        "String" | "string" | "&str" | "Str" | "text" => "the string type is `str`",
        "Int" | "Integer" | "long" | "Long" | "number" | "i128" | "u128" => "integers are `int` (64-bit) or sized `i32`, `u8`, ...",
        "float" | "double" | "Float" | "Double" => "floats are `f64` or `f32`",
        "boolean" | "Bool" | "Boolean" => "booleans are `bool`",
        "Vec" | "List" | "list" | "Array" | "array" | "ArrayList" | "slice" => "lists are written `[T]`",
        "HashMap" | "Map" | "dict" | "Dict" | "BTreeMap" | "map" | "Dictionary" => "maps are written `{K:V}`",
        "HashSet" | "Set" | "set" | "BTreeSet" => "sets are written `{T}`",
        "BinaryHeap" | "PriorityQueue" | "Heap" | "priority_queue" => "the min-heap type is `heap[T]`; make one with `heap()`",
        "Option" | "Optional" => "optionals are written `T?`",
        "Result" => "results are written `T!` (or `T!E`)",
        "void" | "None" | "Unit" | "unit" | "nil" => "omit the return type when a function returns nothing",
        "Self" => "name the type explicitly; there is no `Self`",
        "Box" | "Rc" | "Arc" | "RefCell" | "Cell" => "no smart pointers: use the type directly",
        "char" | "rune" | "Char" => "there is no char type; single characters are `str`",
        _ => return None,
    })
}

fn name_hint(name: &str) -> Option<&'static str> {
    Some(match name {
        "None" | "nil" | "null" | "undefined" | "NULL" => "write `none`",
        "True" => "write `true`",
        "False" => "write `false`",
        "self" | "this" => "no `self`: methods are plain functions whose first parameter is the receiver, called as `x.method()`",
        "it" => "`it` only works inside an argument to a function-taking call such as `map`, `filter` or `sort_by`",
        "input" | "readline" | "read_line" | "stdin" => "read input with `io.read_line()` (str?), `io.lines()` or `io.read()`",
        "open" => "read files with `fs.read(path)?`",
        "sorted" => "sort with `xs.sort()` (returns a new list)",
        "heapq" | "BinaryHeap" | "PriorityQueue" | "heapify" | "heappush" | "heappop" => {
            "use the min-heap `h = heap()` (or `heap(xs)`), `h.push(x)`, `h.pop()` (smallest, T?), `h.first`"
        }
        "reversed" | "reverse" => "reverse with `xs.rev()`",
        "println" | "printf" | "console" | "puts" | "echo" | "fmt" => "print with `print(\"...{x}...\")`",
        "Some" => "no `Some`: an optional is the value itself, or `none`",
        "Ok" => "no `Ok`: return the value itself",
        "Err" => "no `Err`: fail with `fail \"msg\"`",
        "exit" | "quit" => "exit with `os.exit(code)`",
        "format" | "sprintf" => "every string interpolates: \"{x} and {y:.2}\"",
        "String" | "Vec" | "HashMap" | "HashSet" | "Vec_new" => "use literals: \"\", [], {}",
        "dict" => "maps are `{}` or `{k: v}`",
        "isinstance" | "type" | "typeof" => "there is no runtime type inspection; use an enum and `match`",
        "assert_eq" | "assertEqual" => "write `assert a == b`",
        _ => return None,
    })
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let c = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + c);
        }
        prev = cur;
    }
    prev[b.len()]
}

fn closest<'b>(name: &str, cands: impl Iterator<Item = &'b str>) -> Option<&'b str> {
    let max = if name.len() <= 3 { 1 } else { 2 };
    cands
        .filter(|c| *c != name)
        .map(|c| (edit_distance(name, c), c))
        .filter(|(d, _)| *d <= max)
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

/// Visits direct sub-expressions; stops early when `f` returns true.
fn any_child(e: &Expr, f: &mut dyn FnMut(&Expr) -> bool) -> bool {
    fn block(b: &Block, f: &mut dyn FnMut(&Expr) -> bool) -> bool {
        b.stmts.iter().any(|s| stmt(s, f))
    }
    fn stmt(s: &Stmt, f: &mut dyn FnMut(&Expr) -> bool) -> bool {
        match s {
            Stmt::Expr(e) | Stmt::Var { value: e, .. } | Stmt::Fail(e, _) => f(e),
            Stmt::Assign { targets, value, .. } => targets.iter().any(|t| f(t)) || f(value),
            Stmt::For { iter, body, .. } => f(iter) || block(body, f),
            Stmt::While { cond, body, .. } => f(cond) || block(body, f),
            Stmt::Return(Some(e), _) => f(e),
            Stmt::Assert { cond, msg, .. } => f(cond) || msg.as_ref().is_some_and(|m| f(m)),
            _ => false,
        }
    }
    match &e.kind {
        ExprKind::Str(segs) => segs.iter().any(|s| matches!(s, StrSeg::Expr(e, _, args) if f(e) || args.iter().any(&mut *f))),
        ExprKind::List(xs) | ExprKind::Set(xs) | ExprKind::Tuple(xs) => xs.iter().any(|x| f(x)),
        ExprKind::Map(ps) => ps.iter().any(|(k, v)| f(k) || f(v)),
        ExprKind::StructLit { fields, .. } => fields.iter().any(|fi| f(&fi.value)),
        ExprKind::Field { obj, .. } => f(obj),
        ExprKind::Index { obj, index } => f(obj) || f(index),
        ExprKind::Slice { obj, start, end, .. } => f(obj) || start.as_ref().is_some_and(|x| f(x)) || end.as_ref().is_some_and(|x| f(x)),
        ExprKind::Call { callee, args } => f(callee) || args.iter().any(|a| f(a)),
        ExprKind::Method { obj, args, .. } => f(obj) || args.iter().any(|a| f(a)),
        ExprKind::Unary { expr, .. } | ExprKind::Try(expr) | ExprKind::Cast { expr, .. } => f(expr),
        ExprKind::Binary { lhs, rhs, .. } => f(lhs) || f(rhs),
        ExprKind::And(a, b) | ExprKind::Or(a, b) | ExprKind::Coalesce(a, b) => f(a) || f(b),
        ExprKind::Compare { first, rest } => f(first) || rest.iter().any(|(_, x)| f(x)),
        ExprKind::Range { start, end, .. } => start.as_ref().is_some_and(|x| f(x)) || end.as_ref().is_some_and(|x| f(x)),
        ExprKind::Lambda { body, .. } => f(body),
        ExprKind::If { cond, then, els } => f(cond) || block(then, f) || els.as_ref().is_some_and(|b| block(b, f)),
        ExprKind::Match { scrutinee, arms } => {
            f(scrutinee) || arms.iter().any(|a| a.guard.as_ref().is_some_and(|g| f(g)) || block(&a.body, f))
        }
        ExprKind::Block(b) => block(b, f),
        _ => false,
    }
}

impl<'a> Resolver<'a> {
    pub fn new(src: &'a str) -> Resolver<'a> {
        Resolver {
            src,
            diags: Vec::new(),
            prog: Program::default(),
            struct_ids: HashMap::new(),
            enum_ids: HashMap::new(),
            aliases: HashMap::new(),
            variants: HashMap::new(),
            fn_ids: HashMap::new(),
            fn_info: Vec::new(),
            const_ids: HashMap::new(),
            field_names: HashSet::new(),
            ctxs: Vec::new(),
            cur_fn: None,
            io: Vec::new(),
            calls: Vec::new(),
        }
    }

    fn err(&mut self, code: &'static str, span: Span, msg: impl Into<String>) {
        self.diags.push(Diag::new(code, span, msg));
    }

    fn err_fix(&mut self, code: &'static str, span: Span, msg: impl Into<String>, fix: impl Into<String>) {
        self.diags.push(Diag::new(code, span, msg).fix(fix));
    }

    fn snippet(&self, span: Span) -> &str {
        &self.src[span.start as usize..span.end as usize]
    }

    // ----- declarations -----

    pub fn module(mut self, m: &Module) -> (Program, Vec<Diag>) {
        // Pass 1: declare names.
        let mut seen_types: HashSet<String> = HashSet::new();
        for item in &m.items {
            let (name, span) = match item {
                Item::Struct(s) => (&s.name.name, s.name.span),
                Item::Enum(e) => (&e.name.name, e.name.span),
                Item::Alias(a) => (&a.name.name, a.name.span),
                _ => continue,
            };
            if !seen_types.insert(name.clone()) || conv_for(name).is_some() {
                self.err("E0205", span, format!("type `{name}` is already defined"));
            }
        }
        for item in &m.items {
            match item {
                Item::Struct(s) => {
                    let id = self.prog.structs.len() as u32;
                    self.struct_ids.insert(s.name.name.clone(), id);
                    for f in &s.fields {
                        self.field_names.insert(f.name.name.clone());
                    }
                    self.prog.structs.push(ir::StructDef { name: s.name.name.clone(), fields: Vec::new() });
                }
                Item::Enum(e) => {
                    let id = self.prog.enums.len() as u32;
                    self.enum_ids.insert(e.name.name.clone(), id);
                    for (tag, v) in e.variants.iter().enumerate() {
                        self.variants.entry(v.name.name.clone()).or_default().push((id, tag as u32));
                    }
                    self.prog.enums.push(ir::EnumDef { name: e.name.name.clone(), variants: Vec::new() });
                }
                Item::Alias(a) => {
                    self.aliases.insert(a.name.name.clone(), a.ty.clone());
                }
                Item::Fn(f) => {
                    let id = self.prog.fns.len() as FnId;
                    let ids = self.fn_ids.entry(f.name.name.clone()).or_default();
                    let dup = !ids.is_empty() && f.params.is_empty();
                    ids.push(id);
                    if dup {
                        self.err("E0205", f.name.span, format!("function `{}` is already defined", f.name.name));
                    }
                    let params = f.params.iter().map(|p| (p.mode, matches!(p.ty, TypeExpr::Fn(..)))).collect();
                    let required = f.params.iter().filter(|p| p.default.is_none()).count();
                    self.fn_info.push(FnInfo { params, required });
                    self.prog.fns.push(ir::FnDef {
                        name: f.name.name.clone(),
                        params: Vec::new(),
                        ret: None,
                        body: Ex::Lit(Value::Unit),
                        nslots: 0,
                        span: f.name.span,
                        sig: self.sig_text(f),
                        doc: f.doc.clone(),
                        io: false,
                    });
                    self.io.push(false);
                    self.calls.push(HashSet::new());
                }
                Item::Const(c) => {
                    if self.const_ids.contains_key(&c.name.name) {
                        self.err("E0205", c.name.span, format!("`{}` is already defined", c.name.name));
                    }
                    let id = self.prog.consts.len() as u32;
                    self.const_ids.insert(c.name.name.clone(), id);
                    self.prog.consts.push(ir::ConstDef { name: c.name.name.clone(), value: Ex::Lit(Value::Unit), nslots: 0 });
                }
                Item::Test(_) => {}
            }
        }
        for (name, ids) in self.fn_ids.clone() {
            self.prog.fn_names.insert(name, ids.into());
        }
        self.prog.main = self.fn_ids.get("main").map(|ids| ids[0]);

        // Pass 2: types of fields, variants and signatures.
        for item in &m.items {
            match item {
                Item::Struct(s) => {
                    let gens: Vec<String> = s.generics.iter().map(|g| g.name.name.clone()).collect();
                    let id = self.struct_ids[&s.name.name] as usize;
                    let mut fields = Vec::new();
                    for f in &s.fields {
                        let ty = self.ty(&f.ty, &gens);
                        let default = f.default.as_ref().map(|d| self.thunk(d));
                        fields.push(ir::FieldDef { name: f.name.name.clone(), ty, default });
                    }
                    self.prog.structs[id].fields = fields;
                }
                Item::Enum(e) => {
                    let gens: Vec<String> = e.generics.iter().map(|g| g.name.name.clone()).collect();
                    let id = self.enum_ids[&e.name.name] as usize;
                    let vs = e
                        .variants
                        .iter()
                        .map(|v| ir::VariantDef { name: v.name.name.clone(), fields: v.fields.iter().map(|t| self.ty(t, &gens)).collect() })
                        .collect();
                    self.prog.enums[id].variants = vs;
                }
                _ => {}
            }
        }
        let mut fn_index = 0;
        for item in &m.items {
            if let Item::Fn(f) = item {
                let gens: Vec<String> = f.generics.iter().map(|g| g.name.name.clone()).collect();
                let params = f
                    .params
                    .iter()
                    .map(|p| ir::ParamDef {
                        name: p.name.name.clone(),
                        ty: self.ty(&p.ty, &gens),
                        mode: p.mode,
                        default: p.default.as_ref().map(|d| self.thunk(d)),
                    })
                    .collect();
                let ret = f.ret.as_ref().map(|t| self.ty(t, &gens));
                let def = &mut self.prog.fns[fn_index];
                def.params = params;
                def.ret = ret;
                fn_index += 1;
            }
        }

        // Pass 3: bodies.
        let mut fn_index = 0u32;
        for item in &m.items {
            match item {
                Item::Fn(f) => {
                    self.fn_body(fn_index, f);
                    fn_index += 1;
                }
                Item::Const(c) => {
                    let id = self.const_ids[&c.name.name] as usize;
                    self.ctxs.push(Ctx { scopes: vec![HashMap::new()], ..Default::default() });
                    let value = self.expr(&c.value);
                    let ctx = self.ctxs.pop().unwrap();
                    self.prog.consts[id].value = value;
                    self.prog.consts[id].nslots = ctx.nslots;
                }
                Item::Test(t) => {
                    self.ctxs.push(Ctx { scopes: vec![HashMap::new()], ..Default::default() });
                    let body = self.block_with_tail(&t.body, true);
                    let ctx = self.ctxs.pop().unwrap();
                    self.prog.tests.push(ir::TestDef { name: t.name.clone(), body, nslots: ctx.nslots, span: t.span });
                }
                _ => {}
            }
        }

        // Effects: propagate I/O through calls until nothing changes.
        loop {
            let mut changed = false;
            for i in 0..self.io.len() {
                if !self.io[i] && self.calls[i].iter().any(|&c| self.io[c as usize]) {
                    self.io[i] = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        for (i, f) in self.prog.fns.iter_mut().enumerate() {
            f.io = self.io[i];
        }
        (self.prog, self.diags)
    }

    fn sig_text(&self, f: &FnDecl) -> String {
        let mut s = format!("fn {}", f.name.name);
        if !f.generics.is_empty() {
            let gs: Vec<String> = f
                .generics
                .iter()
                .map(|g| {
                    if g.bounds.is_empty() {
                        g.name.name.clone()
                    } else {
                        format!("{}: {}", g.name.name, g.bounds.iter().map(|b| b.name.as_str()).collect::<Vec<_>>().join(" + "))
                    }
                })
                .collect();
            s.push_str(&format!("[{}]", gs.join(", ")));
        }
        let ps: Vec<String> = f
            .params
            .iter()
            .map(|p| {
                let m = match p.mode {
                    Mode::Mut => "mut ",
                    Mode::Own => "own ",
                    Mode::Borrow => "",
                };
                let d = p.default.as_ref().map(|d| format!(" = {}", self.snippet(d.span))).unwrap_or_default();
                format!("{m}{} {}{d}", p.name.name, p.ty)
            })
            .collect();
        s.push_str(&format!("({})", ps.join(", ")));
        if let Some(r) = &f.ret {
            match r {
                TypeExpr::Result(t, e) if matches!(&**t, TypeExpr::Tuple(v, _) if v.is_empty()) => {
                    s.push('!');
                    if let Some(e) = e {
                        s.push_str(&e.to_string());
                    }
                }
                _ => s.push_str(&format!(" {r}")),
            }
        }
        s
    }

    /// A default value, as a zero-parameter lambda with no access to locals.
    fn thunk(&mut self, e: &Expr) -> Rc<LambdaDef> {
        let saved = std::mem::take(&mut self.ctxs);
        self.ctxs.push(Ctx { scopes: vec![HashMap::new()], ..Default::default() });
        let body = self.expr(e);
        let ctx = self.ctxs.pop().unwrap();
        self.ctxs = saved;
        Rc::new(LambdaDef { params: vec![], nslots: ctx.nslots, body, span: e.span })
    }

    fn ty(&mut self, t: &TypeExpr, gens: &[String]) -> Ty {
        self.ty_depth(t, gens, 0)
    }

    fn ty_depth(&mut self, t: &TypeExpr, gens: &[String], depth: u32) -> Ty {
        match t {
            TypeExpr::Name(id, args) => {
                let n = id.name.as_str();
                if gens.iter().any(|g| g == n) || n == "any" {
                    return Ty::Any;
                }
                if let Some((name, min, max)) = int_type(n) {
                    return Ty::Int { name, min, max };
                }
                match n {
                    "f64" | "f32" => return Ty::Float,
                    "bool" => return Ty::Bool,
                    "str" | "char" => return Ty::Str,
                    "heap" => {
                        let inner = args.first().map_or(Ty::Any, |a| self.ty_depth(a, gens, depth));
                        return Ty::Heap(Box::new(inner));
                    }
                    _ => {}
                }
                for a in args {
                    self.ty_depth(a, gens, depth);
                }
                if let Some(&s) = self.struct_ids.get(n) {
                    return Ty::Struct(s);
                }
                if let Some(&e) = self.enum_ids.get(n) {
                    return Ty::Enum(e);
                }
                if let Some(a) = self.aliases.get(n).cloned() {
                    if depth > 20 {
                        self.err("E0204", id.span, format!("type alias `{n}` refers to itself"));
                        return Ty::Any;
                    }
                    return self.ty_depth(&a, gens, depth + 1);
                }
                // Single uppercase letters are generic parameters someone forgot to declare.
                if n.len() == 1 && n.chars().all(|c| c.is_ascii_uppercase()) {
                    self.err_fix("E0204", id.span, format!("unknown type `{n}`; declare type parameters after the name"), format!("fn name[{n}](...)"));
                    return Ty::Any;
                }
                match type_hint(n) {
                    Some(h) => self.err("E0204", id.span, format!("unknown type `{n}`: {h}")),
                    None => {
                        let cands: Vec<String> = self
                            .struct_ids
                            .keys()
                            .chain(self.enum_ids.keys())
                            .cloned()
                            .chain(["int", "f64", "str", "bool"].iter().map(|s| s.to_string()))
                            .collect();
                        match closest(n, cands.iter().map(|s| s.as_str())) {
                            Some(c) => self.err_fix("E0204", id.span, format!("unknown type `{n}`"), c.to_string()),
                            None => self.err("E0204", id.span, format!("unknown type `{n}`")),
                        }
                    }
                }
                Ty::Any
            }
            TypeExpr::List(t, _) => Ty::List(Box::new(self.ty_depth(t, gens, depth))),
            TypeExpr::Map(k, v, _) => Ty::Map(Box::new(self.ty_depth(k, gens, depth)), Box::new(self.ty_depth(v, gens, depth))),
            TypeExpr::Set(t, _) => Ty::Set(Box::new(self.ty_depth(t, gens, depth))),
            TypeExpr::Tuple(ts, _) => {
                if ts.is_empty() {
                    Ty::Unit
                } else {
                    Ty::Tuple(ts.iter().map(|t| self.ty_depth(t, gens, depth)).collect())
                }
            }
            TypeExpr::Optional(t) => Ty::Optional(Box::new(self.ty_depth(t, gens, depth))),
            TypeExpr::Result(t, e) => {
                if let Some(e) = e {
                    self.ty_depth(e, gens, depth);
                }
                Ty::Result(Box::new(self.ty_depth(t, gens, depth)))
            }
            TypeExpr::Fn(ps, r, _) => {
                for p in ps {
                    self.ty_depth(p, gens, depth);
                }
                if let Some(r) = r {
                    self.ty_depth(r, gens, depth);
                }
                Ty::Fn
            }
        }
    }

    fn fn_body(&mut self, id: FnId, f: &FnDecl) {
        let ret_optional = matches!(f.ret, Some(TypeExpr::Optional(_)));
        let mut ctx = Ctx { scopes: vec![HashMap::new()], ret_optional, ..Default::default() };
        for p in &f.params {
            if ctx.scopes[0].contains_key(&p.name.name) {
                self.err("E0205", p.name.span, format!("duplicate parameter `{}`", p.name.name));
            }
            let slot = ctx.nslots;
            ctx.nslots += 1;
            ctx.scopes[0].insert(p.name.name.clone(), Binding { slot, mutable: p.mode != Mode::Borrow });
        }
        self.ctxs.push(ctx);
        self.cur_fn = Some(id);
        let body = self.block_with_tail(&f.body, false);
        self.cur_fn = None;
        let ctx = self.ctxs.pop().unwrap();
        let def = &mut self.prog.fns[id as usize];
        def.body = body;
        def.nslots = ctx.nslots;
    }

    // ----- scopes -----

    fn ctx(&mut self) -> &mut Ctx {
        self.ctxs.last_mut().unwrap()
    }

    fn push_scope(&mut self) {
        self.ctx().scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        let ctx = self.ctx();
        if let Some(s) = ctx.scopes.pop() {
            ctx.closed.extend(s.into_keys());
        }
    }

    fn declare(&mut self, name: &str, mutable: bool) -> u32 {
        let ctx = self.ctx();
        let slot = ctx.nslots;
        ctx.nslots += 1;
        ctx.scopes.last_mut().unwrap().insert(name.to_string(), Binding { slot, mutable });
        slot
    }

    /// Finds a local: (frames up, binding, declared in the innermost scope).
    fn lookup(&self, name: &str) -> Option<(u32, Binding, bool)> {
        for (depth, ctx) in self.ctxs.iter().rev().enumerate() {
            for (i, scope) in ctx.scopes.iter().enumerate().rev() {
                if let Some(b) = scope.get(name) {
                    let innermost = depth == 0 && i == ctx.scopes.len() - 1;
                    return Some((depth as u32, b.clone(), innermost));
                }
            }
        }
        None
    }

    fn is_local(&self, name: &str) -> bool {
        self.lookup(name).is_some()
    }

    /// Is this expression certainly a function: a lambda, or the name of a
    /// function, conversion or variant constructor?
    fn is_fn_expr(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Lambda { .. } => true,
            ExprKind::Name(n) => {
                !self.is_local(n) && (self.fn_ids.contains_key(n) || conv_for(n).is_some() || self.variants.contains_key(n))
            }
            _ => false,
        }
    }

    /// Does a bare name refer to anything?
    fn is_defined(&self, n: &str) -> bool {
        self.is_local(n)
            || self.const_ids.contains_key(n)
            || self.fn_ids.contains_key(n)
            || self.variants.contains_key(n)
            || conv_for(n).is_some()
            || is_method(n)
            || NAMESPACES.contains(&n)
    }

    fn var_ex(&self, depth: u32, slot: u32) -> Ex {
        if depth == 0 {
            Ex::Local(slot)
        } else {
            Ex::Up(depth, slot)
        }
    }

    fn note_call(&mut self, ids: &[FnId]) {
        if let Some(cur) = self.cur_fn {
            self.calls[cur as usize].extend(ids.iter().copied());
        }
    }

    fn note_io(&mut self) {
        if let Some(cur) = self.cur_fn {
            self.io[cur as usize] = true;
        }
    }

    // ----- blocks and statements -----

    /// Lowers a block whose last expression statement is its value. In a
    /// test, a trailing `a == b` reports both sides on failure.
    fn block_with_tail(&mut self, b: &Block, test: bool) -> Ex {
        let mut stmts = Vec::new();
        let mut tail = None;
        let n = b.stmts.len();
        for (i, s) in b.stmts.iter().enumerate() {
            if i == n - 1 {
                if let Stmt::Expr(e) = s {
                    if test {
                        if let Some(eqx) = self.assert_eq(e, None) {
                            tail = Some(Box::new(eqx));
                            continue;
                        }
                    }
                    tail = Some(Box::new(self.expr(e)));
                    continue;
                }
            }
            if let Some(st) = self.stmt(s) {
                stmts.push(st);
            }
        }
        Ex::Block(stmts, tail)
    }

    fn scoped_block(&mut self, b: &Block) -> Ex {
        self.push_scope();
        let e = self.block_with_tail(b, false);
        self.pop_scope();
        e
    }

    fn body_stmts(&mut self, b: &Block) -> Vec<St> {
        let mut out = Vec::new();
        for s in &b.stmts {
            if let Some(st) = self.stmt(s) {
                out.push(st);
            }
        }
        out
    }

    fn assert_eq(&mut self, e: &Expr, msg: Option<&Expr>) -> Option<Ex> {
        if let ExprKind::Compare { first, rest } = &e.kind {
            if rest.len() == 1 && rest[0].0 == CmpOp::Eq {
                let l = self.expr(first);
                let r = self.expr(&rest[0].1);
                let msg = msg.map(|m| Box::new(self.expr(m)));
                return Some(Ex::AssertEq { l: Box::new(l), r: Box::new(r), msg, span: e.span });
            }
        }
        None
    }

    fn stmt(&mut self, s: &Stmt) -> Option<St> {
        Some(match s {
            Stmt::Expr(e) => {
                self.check_unused(e);
                St::Expr(self.expr(e), e.span)
            }
            Stmt::Var { name, ty, value } => {
                let v = self.expr(value);
                if let Some(t) = ty {
                    self.ty(t, &[]);
                }
                let slot = self.declare(&name.name, true);
                St::Bind(PatIr::Bind(slot), v, name.span)
            }
            Stmt::Assign { targets, op, value, span } => return self.assign(targets, *op, value, *span),
            Stmt::For { pat, iter, body, span } => {
                let iter = self.expr(iter);
                self.push_scope();
                let pat = self.pattern(pat, &mut HashMap::new());
                self.ctx().loops += 1;
                let body = self.body_stmts(body);
                self.ctx().loops -= 1;
                self.pop_scope();
                St::For { pat, iter, body, span: *span }
            }
            Stmt::While { cond, body, .. } => {
                let cond = self.expr(cond);
                self.push_scope();
                self.ctx().loops += 1;
                let body = self.body_stmts(body);
                self.ctx().loops -= 1;
                self.pop_scope();
                St::While { cond, body }
            }
            Stmt::Break(sp) | Stmt::Continue(sp) => {
                if self.ctx().loops == 0 {
                    self.err("E0211", *sp, "`break`/`continue` outside a loop");
                }
                if matches!(s, Stmt::Break(_)) {
                    St::Break
                } else {
                    St::Continue
                }
            }
            Stmt::Return(e, _) => St::Return(e.as_ref().map(|e| self.expr(e))),
            Stmt::Fail(e, sp) => St::Fail(self.expr(e), *sp),
            Stmt::Assert { cond, msg, span } => {
                if let Some(eqx) = self.assert_eq(cond, msg.as_ref()) {
                    return Some(St::Expr(eqx, *span));
                }
                let c = self.expr(cond);
                let m = msg.as_ref().map(|m| self.expr(m));
                St::Assert { cond: c, msg: m, span: *span }
            }
        })
    }

    /// A pure builtin method whose result is dropped is almost always a bug:
    /// `xs.sort()` returns the sorted list rather than sorting in place.
    fn check_unused(&mut self, e: &Expr) {
        if let ExprKind::Method { obj, name, .. } = &e.kind {
            let n = name.name.as_str();
            if is_method(n) && !is_mutator(n) && !matches!(n, "each" | "unwrap" | "expect") && !self.fn_ids.contains_key(n) {
                if matches!(obj.kind, ExprKind::Name(ref ns) if NAMESPACES.contains(&ns.as_str()) && !self.is_local(ns)) {
                    return;
                }
                // `console.log(x)`: report the unknown name, not the method.
                if matches!(obj.kind, ExprKind::Name(ref ns) if !self.is_defined(ns)) {
                    return;
                }
                let recv = self.snippet(obj.span).to_string();
                let call = self.snippet(e.span).to_string();
                let simple_place = matches!(obj.kind, ExprKind::Name(_) | ExprKind::Field { .. } | ExprKind::Index { .. });
                if simple_place {
                    self.err_fix(
                        "E0409",
                        e.span,
                        format!("result of `{n}` is unused: it returns a new value and does not change `{recv}`"),
                        format!("{recv} = {call}"),
                    );
                } else {
                    self.err("E0409", e.span, format!("result of `{n}` is unused; use a `for` loop for side effects"));
                }
            }
        }
    }

    fn assign(&mut self, targets: &[Expr], op: Option<BinOp>, value: &Expr, span: Span) -> Option<St> {
        // `(a, b) = pair` is `a, b = pair`.
        if let [Expr { kind: ExprKind::Tuple(inner), .. }] = targets {
            if !inner.is_empty() && op.is_none() {
                return self.assign(inner, op, value, span);
            }
        }
        let v = self.expr(value);
        if targets.len() == 1 {
            let t = &targets[0];
            if let ExprKind::Name(n) = &t.kind {
                match self.lookup(n) {
                    None => {
                        if op.is_some() {
                            self.err_fix("E0201", t.span, format!("`{n}` is not defined; declare it first"), format!("var {n} = 0"));
                            return None;
                        }
                        let slot = self.declare(n, false);
                        return Some(St::Bind(PatIr::Bind(slot), v, t.span));
                    }
                    Some((depth, b, innermost)) => {
                        if b.mutable {
                            let place = Place { root: self.root(depth, b.slot), name: n.as_str().into(), mutable: true, path: vec![], span: t.span };
                            return Some(St::Assign { place, op, value: v, span });
                        }
                        if innermost && op.is_none() {
                            let slot = self.declare(n, false);
                            return Some(St::Bind(PatIr::Bind(slot), v, t.span));
                        }
                        self.immutable_err(n, t.span);
                        return None;
                    }
                }
            }
            let place = self.place(t)?;
            if !place.mutable {
                self.immutable_err(&place.name.clone(), t.span);
                return None;
            }
            return Some(St::Assign { place, op, value: v, span });
        }
        if op.is_some() {
            self.err("E0210", span, "compound assignment takes a single target");
            return None;
        }
        let mut ts = Vec::new();
        let mut all_new = true;
        for t in targets {
            if let ExprKind::Name(n) = &t.kind {
                match self.lookup(n) {
                    None => {
                        ts.push(Target::Bind(self.declare(n, false)));
                        continue;
                    }
                    Some((_, b, innermost)) if !b.mutable && innermost => {
                        ts.push(Target::Bind(self.declare(n, false)));
                        continue;
                    }
                    _ => {}
                }
            }
            all_new = false;
            let place = self.place(t)?;
            if !place.mutable {
                self.immutable_err(&place.name.clone(), t.span);
                return None;
            }
            ts.push(Target::Place(place));
        }
        if all_new {
            let pats = ts.into_iter().map(|t| if let Target::Bind(s) = t { PatIr::Bind(s) } else { unreachable!() }).collect();
            return Some(St::Bind(PatIr::Tuple(pats), v, span));
        }
        Some(St::AssignMulti { targets: ts, value: v, span })
    }

    fn immutable_err(&mut self, name: &str, span: Span) {
        self.err(
            "E0202",
            span,
            format!("`{name}` is immutable; declare it with `var {name} = ...` (or take a parameter as `mut {name} T`)"),
        );
    }

    fn root(&self, depth: u32, slot: u32) -> Root {
        if depth == 0 {
            Root::Local(slot)
        } else {
            Root::Up(depth, slot)
        }
    }

    /// `x`, `x.f`, `x[i]`, `x.f[i].g`, ... rooted at a local variable.
    fn place_opt(&mut self, e: &Expr) -> Option<Place> {
        match &e.kind {
            ExprKind::Name(n) => {
                let (depth, b, _) = self.lookup(n)?;
                Some(Place { root: self.root(depth, b.slot), name: n.as_str().into(), mutable: b.mutable, path: vec![], span: e.span })
            }
            ExprKind::Field { obj, name } => {
                if !self.field_names.contains(&name.name) && name.name.parse::<usize>().is_err() {
                    return None;
                }
                let mut p = self.place_opt(obj)?;
                p.path.push(Seg::Field(name.name.as_str().into(), name.span));
                p.span = e.span;
                Some(p)
            }
            ExprKind::Index { obj, index } => {
                let mut p = self.place_opt(obj)?;
                let i = self.expr(index);
                p.path.push(Seg::Index(i, index.span));
                p.span = e.span;
                Some(p)
            }
            _ => None,
        }
    }

    fn place(&mut self, e: &Expr) -> Option<Place> {
        let p = self.place_opt(e);
        if p.is_none() {
            match &e.kind {
                ExprKind::Name(n) => self.undefined(n, e.span),
                _ => self.err("E0210", e.span, "can only assign to a variable, a field or an index"),
            }
        }
        p
    }

    // ----- patterns -----

    fn pattern(&mut self, p: &Pat, binds: &mut HashMap<String, u32>) -> PatIr {
        match p {
            Pat::Wild(_) => PatIr::Wild,
            Pat::Name(id) => {
                if let Some(vs) = self.variants.get(&id.name).cloned() {
                    let (eid, tag) = vs[0];
                    if !self.prog.enums[eid as usize].variants.is_empty() && !self.prog.enums[eid as usize].variants[tag as usize].fields.is_empty() {
                        self.err_fix(
                            "E0206",
                            id.span,
                            format!("variant `{}` has fields; match them too", id.name),
                            format!("{}({})", id.name, vec!["_"; self.prog.enums[eid as usize].variants[tag as usize].fields.len()].join(", ")),
                        );
                    }
                    return PatIr::Variant { id: eid, tag, args: vec![] };
                }
                if id.name.starts_with(|c: char| c.is_ascii_uppercase()) && id.name.chars().any(|c| c.is_ascii_lowercase()) {
                    self.err("E0205", id.span, format!("unknown variant `{}` (bindings start lowercase)", id.name));
                    return PatIr::Wild;
                }
                if let Some(&slot) = binds.get(&id.name) {
                    return PatIr::Bind(slot);
                }
                let slot = self.declare(&id.name, false);
                binds.insert(id.name.clone(), slot);
                PatIr::Bind(slot)
            }
            Pat::Lit(e) => match self.const_value(e) {
                Some(v) => PatIr::Lit(v),
                None => PatIr::Wild,
            },
            Pat::Range { lo, hi, inclusive, .. } => PatIr::Range {
                lo: lo.as_ref().and_then(|e| self.const_value(e)),
                hi: hi.as_ref().and_then(|e| self.const_value(e)),
                inclusive: *inclusive,
            },
            Pat::Tuple(ps, _) => PatIr::Tuple(ps.iter().map(|p| self.pattern(p, binds)).collect()),
            Pat::List { items, rest, .. } => {
                let items = items.iter().map(|p| self.pattern(p, binds)).collect();
                let rest = rest.as_ref().map(|r| {
                    r.as_ref().map(|id| {
                        let slot = self.declare(&id.name, false);
                        binds.insert(id.name.clone(), slot);
                        slot
                    })
                });
                PatIr::List { items, rest }
            }
            Pat::Variant { name, qualifier, args } => {
                let Some((eid, tag)) = self.variant_named(name, qualifier.as_ref()) else {
                    return PatIr::Wild;
                };
                let arity = self.prog.enums[eid as usize].variants[tag as usize].fields.len();
                if arity != args.len() {
                    self.err("E0206", name.span, format!("variant `{}` has {arity} field(s), pattern has {}", name.name, args.len()));
                }
                let args = args.iter().map(|p| self.pattern(p, binds)).collect();
                PatIr::Variant { id: eid, tag, args }
            }
            Pat::Struct { name, fields, .. } => {
                let Some(&sid) = self.struct_ids.get(&name.name) else {
                    self.err("E0204", name.span, format!("unknown type `{}`", name.name));
                    return PatIr::Wild;
                };
                let mut fs = Vec::new();
                for (f, p) in fields {
                    match self.prog.structs[sid as usize].fields.iter().position(|d| d.name == f.name) {
                        Some(i) => {
                            let p = self.pattern(p, binds);
                            fs.push((i, p));
                        }
                        None => self.err("E0208", f.span, format!("`{}` has no field `{}`", name.name, f.name)),
                    }
                }
                PatIr::Struct { id: sid, fields: fs }
            }
            Pat::Or(ps, _) => PatIr::Or(ps.iter().map(|p| self.pattern(p, binds)).collect()),
            Pat::None(_) => PatIr::None,
            Pat::Err(p, _) => PatIr::Err(Box::new(self.pattern(p, binds))),
            Pat::Ok(p, _) => PatIr::Ok(Box::new(self.pattern(p, binds))),
        }
    }

    fn const_value(&mut self, e: &Expr) -> Option<Value> {
        Some(match &e.kind {
            ExprKind::Int(n) => Value::Int(*n),
            ExprKind::Float(f) => Value::Float(*f),
            ExprKind::Bool(b) => Value::Bool(*b),
            ExprKind::Str(segs) if segs.iter().all(|s| matches!(s, StrSeg::Lit(_))) => {
                Value::str(segs.iter().map(|s| if let StrSeg::Lit(l) = s { l.as_str() } else { "" }).collect::<String>())
            }
            _ => {
                self.err("E0110", e.span, "patterns take literal values");
                return None;
            }
        })
    }

    fn variant_named(&mut self, name: &Ident, qualifier: Option<&Ident>) -> Option<(u32, u32)> {
        let cands = self.variants.get(&name.name).cloned().unwrap_or_default();
        if let Some(q) = qualifier {
            let Some(&eid) = self.enum_ids.get(&q.name) else {
                self.err("E0204", q.span, format!("unknown enum `{}`", q.name));
                return None;
            };
            return match cands.iter().find(|(e, _)| *e == eid) {
                Some(&c) => Some(c),
                None => {
                    self.err("E0205", name.span, format!("`{}` has no variant `{}`", q.name, name.name));
                    None
                }
            };
        }
        match cands.len() {
            0 => {
                let all: Vec<String> = self.variants.keys().cloned().collect();
                match closest(&name.name, all.iter().map(|s| s.as_str())) {
                    Some(c) => self.err_fix("E0205", name.span, format!("unknown variant `{}`", name.name), c.to_string()),
                    None => self.err("E0205", name.span, format!("unknown variant `{}`", name.name)),
                }
                None
            }
            1 => Some(cands[0]),
            _ => {
                let names: Vec<String> = cands.iter().map(|(e, _)| format!("{}.{}", self.prog.enums[*e as usize].name, name.name)).collect();
                self.err("E0205", name.span, format!("`{}` is ambiguous; write {}", name.name, names.join(" or ")));
                None
            }
        }
    }

    // ----- expressions -----

    fn has_free_it(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Name(n) => n == "it",
            ExprKind::Lambda { params, body } => !params.iter().any(|p| p.name == "it") && self.has_free_it(body),
            ExprKind::Call { callee, args } => {
                self.has_free_it(callee) || args.iter().enumerate().any(|(i, a)| !self.call_param_is_fn(callee, i) && self.has_free_it(a))
            }
            ExprKind::Method { obj, name, args } => {
                self.has_free_it(obj) || args.iter().enumerate().any(|(i, a)| !self.method_param_is_fn(&name.name, i) && self.has_free_it(a))
            }
            _ => any_child(e, &mut |c| self.has_free_it(c)),
        }
    }

    fn call_param_is_fn(&self, callee: &Expr, i: usize) -> bool {
        match &callee.kind {
            ExprKind::Name(n) if !self.is_local(n) => match self.fn_ids.get(n) {
                Some(ids) => ids.iter().any(|&id| self.fn_info[id as usize].params.get(i).is_some_and(|p| p.1)),
                // `map(xs, it * 2)` calls the method with the receiver first.
                None => i > 0 && hof_arg(n, i - 1),
            },
            _ => false,
        }
    }

    fn method_param_is_fn(&self, name: &str, i: usize) -> bool {
        hof_arg(name, i) || self.fn_ids.get(name).is_some_and(|ids| ids.iter().any(|&id| self.fn_info[id as usize].params.get(i + 1).is_some_and(|p| p.1)))
    }

    /// Lowers an argument; when the parameter takes a function and the
    /// argument mentions `it`, the argument becomes a lambda of `it`.
    fn arg(&mut self, a: &Expr, takes_fn: bool) -> Ex {
        if takes_fn && self.has_free_it(a) {
            return self.lambda(&[Ident { name: "it".into(), span: a.span }], a);
        }
        self.expr(a)
    }

    fn lambda(&mut self, params: &[Ident], body: &Expr) -> Ex {
        let ret_optional = self.ctxs.last().map_or(false, |c| c.ret_optional);
        let mut ctx = Ctx { scopes: vec![HashMap::new()], ret_optional, ..Default::default() };
        let mut slots = Vec::new();
        for p in params {
            let slot = ctx.nslots;
            ctx.nslots += 1;
            ctx.scopes[0].insert(p.name.clone(), Binding { slot, mutable: false });
            slots.push(slot);
        }
        self.ctxs.push(ctx);
        let b = self.expr(body);
        let ctx = self.ctxs.pop().unwrap();
        Ex::Lambda(Rc::new(LambdaDef { params: slots, nslots: ctx.nslots, body: b, span: body.span }))
    }

    fn exprs(&mut self, es: &[Expr]) -> Vec<Ex> {
        es.iter().map(|e| self.expr(e)).collect()
    }

    fn undefined(&mut self, n: &str, span: Span) {
        if let Some(h) = name_hint(n) {
            self.err("E0201", span, format!("`{n}` is not defined: {h}"));
            return;
        }
        if self.ctxs.last().is_some_and(|c| c.closed.contains(n)) {
            self.err(
                "E0201",
                span,
                format!("`{n}` was bound inside a block that has ended; declare `var {n} = ...` before the block, or bind the block's value: `{n} = if c: a else: b`"),
            );
            return;
        }
        let mut cands: Vec<String> = Vec::new();
        for ctx in &self.ctxs {
            for s in &ctx.scopes {
                cands.extend(s.keys().cloned());
            }
        }
        cands.extend(self.fn_ids.keys().cloned());
        cands.extend(self.const_ids.keys().cloned());
        cands.extend(["print", "range", "min", "max", "len", "set", "heap", "list"].iter().map(|s| s.to_string()));
        match closest(n, cands.iter().map(|s| s.as_str())) {
            Some(c) => self.err_fix("E0201", span, format!("`{n}` is not defined"), c.to_string()),
            None => self.err("E0201", span, format!("`{n}` is not defined")),
        }
    }

    pub fn expr(&mut self, e: &Expr) -> Ex {
        let span = e.span;
        match &e.kind {
            ExprKind::Int(n) => Ex::Lit(Value::Int(*n)),
            ExprKind::Float(f) => Ex::Lit(Value::Float(*f)),
            ExprKind::Bool(b) => Ex::Lit(Value::Bool(*b)),
            ExprKind::None => Ex::Lit(Value::None),
            ExprKind::Str(segs) => {
                if segs.iter().all(|s| matches!(s, StrSeg::Lit(_))) {
                    let s: String = segs.iter().map(|s| if let StrSeg::Lit(l) = s { l.as_str() } else { "" }).collect();
                    return Ex::Lit(Value::str(s));
                }
                let pieces = segs
                    .iter()
                    .map(|s| match s {
                        StrSeg::Lit(l) => StrPiece::Lit(l.as_str().into()),
                        StrSeg::Expr(e, spec, args) => StrPiece::Expr(self.expr(e), spec.as_ref().and_then(|s| FmtSpec::parse(s)).map(Box::new), args.iter().map(|a| self.expr(a)).collect()),
                    })
                    .collect();
                Ex::Str(pieces)
            }
            ExprKind::Name(n) => self.name(n, span),
            ExprKind::List(xs) => Ex::List(self.exprs(xs)),
            ExprKind::Set(xs) => {
                if xs.len() == 1 && self.has_free_it(&xs[0]) {
                    self.err("E0129", span, "lambdas have no braces: write the expression directly, e.g. `xs.filter(it > 0)`");
                }
                Ex::Set(self.exprs(xs))
            }
            ExprKind::Tuple(xs) => {
                if xs.is_empty() {
                    Ex::Lit(Value::Unit)
                } else {
                    Ex::Tuple(self.exprs(xs))
                }
            }
            ExprKind::Map(ps) => Ex::Map(ps.iter().map(|(k, v)| (self.expr(k), self.expr(v))).collect()),
            ExprKind::StructLit { name, fields } => self.struct_lit(name, fields, span),
            ExprKind::Field { obj, name } => self.field(obj, name, span),
            ExprKind::Index { obj, index } => Ex::Index { obj: Box::new(self.expr(obj)), index: Box::new(self.expr(index)), span },
            ExprKind::Slice { obj, start, end, inclusive } => Ex::Slice {
                obj: Box::new(self.expr(obj)),
                start: start.as_ref().map(|x| Box::new(self.expr(x))),
                end: end.as_ref().map(|x| Box::new(self.expr(x))),
                inclusive: *inclusive,
                span,
            },
            ExprKind::Call { callee, args } => self.call(callee, args, span),
            ExprKind::Method { obj, name, args } => self.method(obj, name, args, span),
            ExprKind::Unary { op, expr } => Ex::Unary { op: *op, e: Box::new(self.expr(expr)), span },
            ExprKind::Binary { op, lhs, rhs } => Ex::Binary { op: *op, l: Box::new(self.expr(lhs)), r: Box::new(self.expr(rhs)), span },
            ExprKind::And(a, b) => Ex::And(Box::new(self.expr(a)), Box::new(self.expr(b)), span),
            ExprKind::Or(a, b) => Ex::Or(Box::new(self.expr(a)), Box::new(self.expr(b)), span),
            ExprKind::Coalesce(a, b) => Ex::Coalesce(Box::new(self.expr(a)), Box::new(self.expr(b))),
            ExprKind::Compare { first, rest } => {
                let first = Box::new(self.expr(first));
                let rest = rest.iter().map(|(op, x)| (*op, self.expr(x), x.span)).collect();
                Ex::Compare { first, rest }
            }
            ExprKind::Range { start, end, inclusive } => Ex::Range {
                start: start.as_ref().map(|x| Box::new(self.expr(x))),
                end: end.as_ref().map(|x| Box::new(self.expr(x))),
                inclusive: *inclusive,
                span,
            },
            ExprKind::Try(x) => {
                let fn_optional = self.ctxs.last().is_some_and(|c| c.ret_optional);
                Ex::Try { e: Box::new(self.expr(x)), fn_optional, span }
            }
            ExprKind::Lambda { params, body } => self.lambda(params, body),
            ExprKind::If { cond, then, els } => Ex::If {
                cond: Box::new(self.expr(cond)),
                then: Box::new(self.scoped_block(then)),
                els: els.as_ref().map(|b| Box::new(self.scoped_block(b))),
                span,
            },
            ExprKind::Match { scrutinee, arms } => self.match_expr(scrutinee, arms, span),
            ExprKind::Block(b) => self.scoped_block(b),
            ExprKind::Cast { expr, ty } => {
                let inner = self.expr(expr);
                let to = match ty {
                    TypeExpr::Name(id, _) => conv_for(&id.name),
                    _ => None,
                };
                match to {
                    Some(to) => Ex::Cast { e: Box::new(inner), to, span },
                    None => {
                        self.err("E0212", ty.span(), format!("`as` converts between number types, `str` and `bool`, not to `{ty}`"));
                        inner
                    }
                }
            }
        }
    }

    fn name(&mut self, n: &str, span: Span) -> Ex {
        if let Some((depth, b, _)) = self.lookup(n) {
            return self.var_ex(depth, b.slot);
        }
        if let Some(&g) = self.const_ids.get(n) {
            return Ex::Global(g);
        }
        if let Some(ids) = self.fn_ids.get(n).cloned() {
            self.note_call(&ids);
            return Ex::Lit(Value::Func(Rc::new(Func::User(ids.into()))));
        }
        if let Some(vs) = self.variants.get(n).cloned() {
            if vs.len() == 1 {
                let (eid, tag) = vs[0];
                if self.prog.enums[eid as usize].variants[tag as usize].fields.is_empty() {
                    return Ex::Variant { id: eid, tag, args: vec![], span };
                }
                return Ex::Lit(Value::Func(Rc::new(Func::Ctor(eid, tag))));
            }
            self.variant_named(&Ident { name: n.to_string(), span }, None);
            return Ex::Lit(Value::Unit);
        }
        if conv_for(n).is_some() || is_method(n) {
            return Ex::Lit(Value::Func(Rc::new(Func::Method(n.into()))));
        }
        if NAMESPACES.contains(&n) {
            self.err("E0201", span, format!("`{n}` is a module; call one of its functions, e.g. `{n}.read(...)`"));
            return Ex::Lit(Value::Unit);
        }
        self.undefined(n, span);
        Ex::Lit(Value::Unit)
    }

    fn struct_lit(&mut self, name: &Ident, fields: &[FieldInit], span: Span) -> Ex {
        let Some(&sid) = self.struct_ids.get(&name.name) else {
            if self.variants.contains_key(&name.name) {
                self.err_fix("E0141", name.span, "variants take positional fields in parentheses", format!("{}(...)", name.name));
            } else {
                self.err("E0204", name.span, format!("unknown type `{}`", name.name));
            }
            return Ex::Lit(Value::Unit);
        };
        let defs: Vec<String> = self.prog.structs[sid as usize].fields.iter().map(|f| f.name.clone()).collect();
        let has_default: Vec<bool> = self.prog.structs[sid as usize].fields.iter().map(|f| f.default.is_some()).collect();
        let mut slots: Vec<Option<Ex>> = (0..defs.len()).map(|_| None).collect();
        let mut positional = Vec::new();
        for fi in fields {
            let named = match (&fi.name, &fi.value.kind) {
                (Some(n), _) => Some((n.name.clone(), n.span)),
                // `User{name, age}` with matching variables is shorthand.
                (None, ExprKind::Name(n)) if defs.contains(n) => Some((n.clone(), fi.value.span)),
                _ => None,
            };
            match named {
                Some((n, nspan)) => match defs.iter().position(|d| *d == n) {
                    Some(i) => {
                        if slots[i].is_some() {
                            self.err("E0205", nspan, format!("field `{n}` is set twice"));
                        }
                        slots[i] = Some(self.expr(&fi.value));
                    }
                    None => {
                        let fix = closest(&n, defs.iter().map(|s| s.as_str())).map(|s| s.to_string());
                        let msg = format!("`{}` has no field `{n}`; fields: {}", name.name, defs.join(", "));
                        match fix {
                            Some(f) => self.err_fix("E0208", nspan, msg, f),
                            None => self.err("E0208", nspan, msg),
                        }
                    }
                },
                None => positional.push(fi),
            }
        }
        let mut next = 0;
        for fi in positional {
            while next < slots.len() && slots[next].is_some() {
                next += 1;
            }
            if next >= slots.len() {
                self.err("E0206", fi.value.span, format!("too many fields for `{}` ({} fields: {})", name.name, defs.len(), defs.join(", ")));
                break;
            }
            slots[next] = Some(self.expr(&fi.value));
        }
        let missing: Vec<&str> = slots.iter().zip(defs.iter()).zip(has_default.iter()).filter(|((s, _), d)| s.is_none() && !**d).map(|((_, n), _)| n.as_str()).collect();
        if !missing.is_empty() {
            self.err("E0207", span, format!("missing field(s) {} in `{}`", missing.join(", "), name.name));
        }
        Ex::Struct { id: sid, fields: slots, span }
    }

    fn field(&mut self, obj: &Expr, name: &Ident, span: Span) -> Ex {
        let n = name.name.as_str();
        if let ExprKind::Name(ns) = &obj.kind {
            if !self.is_local(ns) {
                if let Some(e) = self.static_field(ns, name, span) {
                    return e;
                }
            }
        }
        let user = self.fn_ids.get(n).cloned();
        if let Some(ids) = &user {
            self.note_call(ids);
        }
        if self.field_names.contains(n) || n.parse::<usize>().is_ok() {
            let o = self.expr(obj);
            return Ex::Field { obj: Box::new(o), name: n.into(), user: user.map(|u| u.into()), span };
        }
        if is_method(n) || user.is_some() {
            return self.method(obj, name, &[], span);
        }
        let o = self.expr(obj);
        let mut cands: Vec<&str> = self.field_names.iter().map(|s| s.as_str()).collect();
        cands.extend(crate::builtins::METHODS.iter().copied());
        match crate::builtins::method_hint(n) {
            Some(h) => self.err("E0203", name.span, format!("no field or method `{n}`: {h}")),
            None => match closest(n, cands.into_iter()) {
                Some(c) => self.err_fix("E0203", name.span, format!("no field or method `{n}`"), c.to_string()),
                None => self.err("E0203", name.span, format!("no field or method `{n}`")),
            },
        }
        o
    }

    /// `math.pi`, `os.args`, `int.max`, `Shape.Circle`.
    fn static_field(&mut self, ns: &str, name: &Ident, span: Span) -> Option<Ex> {
        let n = name.name.as_str();
        match ns {
            "math" => {
                let v = match n {
                    "pi" => std::f64::consts::PI,
                    "e" => std::f64::consts::E,
                    "tau" => std::f64::consts::TAU,
                    "inf" => f64::INFINITY,
                    "nan" => f64::NAN,
                    _ => {
                        self.err("E0203", name.span, format!("`math` has `pi`, `e`, `tau`, `inf`, `nan`; for `{n}` call the method on the number: `x.{n}()`"));
                        return Some(Ex::Lit(Value::Unit));
                    }
                };
                return Some(Ex::Lit(Value::Float(v)));
            }
            "os" | "io" | "fs" | "time" => {
                return Some(self.ns_call(ns, name, &[], span));
            }
            _ => {}
        }
        if let Some((_, min, max)) = int_type(ns) {
            return Some(match n {
                "max" => Ex::Lit(Value::Int(max)),
                "min" => Ex::Lit(Value::Int(min)),
                _ => {
                    self.err("E0203", name.span, format!("`{ns}` has `max` and `min`"));
                    Ex::Lit(Value::Unit)
                }
            });
        }
        if ns == "f64" || ns == "f32" {
            return Some(match n {
                "max" => Ex::Lit(Value::Float(f64::MAX)),
                "min" => Ex::Lit(Value::Float(f64::MIN)),
                "inf" => Ex::Lit(Value::Float(f64::INFINITY)),
                "nan" => Ex::Lit(Value::Float(f64::NAN)),
                "eps" | "epsilon" => Ex::Lit(Value::Float(f64::EPSILON)),
                _ => {
                    self.err("E0203", name.span, format!("`{ns}` has `max`, `min`, `inf`, `nan`, `eps`"));
                    Ex::Lit(Value::Unit)
                }
            });
        }
        if self.enum_ids.contains_key(ns) {
            let (eid, tag) = self.variant_named(name, Some(&Ident { name: ns.to_string(), span }))?;
            if self.prog.enums[eid as usize].variants[tag as usize].fields.is_empty() {
                return Some(Ex::Variant { id: eid, tag, args: vec![], span });
            }
            return Some(Ex::Lit(Value::Func(Rc::new(Func::Ctor(eid, tag)))));
        }
        None
    }

    fn ns_call(&mut self, ns: &str, name: &Ident, args: &[Expr], span: Span) -> Ex {
        let n = name.name.as_str();
        let (f, arity): (Builtin, &[usize]) = match (ns, n) {
            ("fs", "read") => (Builtin::FsRead, &[1]),
            ("fs", "write") => (Builtin::FsWrite, &[2]),
            ("fs", "append") => (Builtin::FsAppend, &[2]),
            ("fs", "exists") => (Builtin::FsExists, &[1]),
            ("fs", "lines") => (Builtin::FsLines, &[1]),
            ("fs", "remove") => (Builtin::FsRemove, &[1]),
            ("io", "read") => (Builtin::IoRead, &[0]),
            ("io", "lines") => (Builtin::IoLines, &[0]),
            ("io", "read_line") => (Builtin::IoReadLine, &[0]),
            ("io", "write") => (Builtin::IoWrite, &[1]),
            ("os", "args") => (Builtin::OsArgs, &[0]),
            ("os", "env") => (Builtin::OsEnv, &[1]),
            ("os", "exit") => (Builtin::OsExit, &[1]),
            ("time", "now") => (Builtin::TimeNow, &[0]),
            _ => {
                let have = match ns {
                    "fs" => "read, write, append, exists, lines, remove",
                    "io" => "read, lines, read_line, write",
                    "os" => "args, env, exit",
                    _ => "now",
                };
                let hint = match (ns, n) {
                    ("io", "input" | "readline" | "stdin" | "readLine") => " (one line: `io.read_line()`)",
                    ("fs", "read_to_string" | "open" | "readFile" | "read_file") => " (`fs.read(path)?`)",
                    ("os", "argv") => " (`os.args`)",
                    _ => "",
                };
                self.err("E0203", name.span, format!("`{ns}` has no `{n}`; it has {have}{hint}"));
                return Ex::Lit(Value::Unit);
            }
        };
        if !arity.contains(&args.len()) {
            self.err("E0206", span, format!("`{ns}.{n}` takes {} argument(s), got {}", arity[0], args.len()));
        }
        if ns != "time" {
            self.note_io();
        }
        Ex::CallBuiltin { f, args: self.exprs(args), span }
    }

    fn check_arity(&mut self, name: &str, ids: &[FnId], n: usize, span: Span, offset: usize) {
        let ok = ids.iter().any(|&id| {
            let info = &self.fn_info[id as usize];
            n + offset >= info.required && n + offset <= info.params.len()
        });
        if !ok {
            let info = &self.fn_info[ids[0] as usize];
            let want = if info.required == info.params.len() {
                format!("{}", info.params.len() - offset.min(info.params.len()))
            } else {
                format!("{} to {}", info.required.saturating_sub(offset), info.params.len() - offset.min(info.params.len()))
            };
            let sig = self.prog.fns[ids[0] as usize].sig.clone();
            self.err("E0206", span, format!("`{name}` takes {want} argument(s), got {n}: {sig}"));
        }
    }

    fn call(&mut self, callee: &Expr, args: &[Expr], span: Span) -> Ex {
        let ExprKind::Name(n) = &callee.kind else {
            let f = self.expr(callee);
            return Ex::CallValue { f: Box::new(f), args: self.exprs(args), span };
        };
        if self.is_local(n) {
            let f = self.expr(callee);
            return Ex::CallValue { f: Box::new(f), args: self.exprs(args), span };
        }
        if let Some(ids) = self.fn_ids.get(n).cloned() {
            self.note_call(&ids);
            self.check_arity(n, &ids, args.len(), span, 0);
            let mut lowered = Vec::new();
            let mut places = Vec::new();
            for (i, a) in args.iter().enumerate() {
                let takes_fn = ids.iter().any(|&id| self.fn_info[id as usize].params.get(i).is_some_and(|p| p.1));
                let is_mut = ids.iter().any(|&id| self.fn_info[id as usize].params.get(i).is_some_and(|p| p.0 == Mode::Mut));
                if is_mut {
                    match self.place_opt(a) {
                        Some(p) => {
                            if !p.mutable {
                                let name = p.name.clone();
                                self.immutable_err(&name, a.span);
                            }
                            places.push(Some(p));
                        }
                        None => {
                            self.err("E0209", a.span, format!("`{n}` changes this argument (`mut` parameter), so pass a variable"));
                            places.push(None);
                        }
                    }
                } else {
                    places.push(None);
                }
                lowered.push(self.arg(a, takes_fn));
            }
            if places.iter().all(|p| p.is_none()) {
                places.clear();
            }
            return Ex::CallFn { fns: ids.into(), args: lowered, places, span };
        }
        if let Some(&g) = self.const_ids.get(n) {
            return Ex::CallValue { f: Box::new(Ex::Global(g)), args: self.exprs(args), span };
        }
        if self.variants.contains_key(n) {
            let Some((eid, tag)) = self.variant_named(&Ident { name: n.clone(), span: callee.span }, None) else {
                return Ex::Lit(Value::Unit);
            };
            return self.variant_ctor(eid, tag, args, span);
        }
        if let Some(&sid) = self.struct_ids.get(n) {
            // `User("ann", 30)` means the same as `User{"ann", 30}`.
            let lowered: Vec<Option<Ex>> = args.iter().map(|a| Some(self.expr(a))).collect();
            let nf = self.prog.structs[sid as usize].fields.len();
            if lowered.len() != nf {
                let names: Vec<String> = self.prog.structs[sid as usize].fields.iter().map(|f| f.name.clone()).collect();
                self.err("E0206", span, format!("`{n}` has {nf} fields ({}), got {}", names.join(", "), lowered.len()));
            }
            return Ex::Struct { id: sid, fields: lowered, span };
        }
        if let Some(to) = conv_for(n) {
            if args.len() != 1 {
                self.err("E0206", span, format!("`{n}(x)` takes one argument"));
                return Ex::Lit(Value::Unit);
            }
            return Ex::CallBuiltin { f: Builtin::Conv(to), args: self.exprs(args), span };
        }
        let builtin = match n.as_str() {
            "print" => Some(Builtin::Print),
            "eprint" => Some(Builtin::Eprint),
            "range" => Some(Builtin::Range),
            "min" if args.len() != 1 => Some(Builtin::Min),
            "max" if args.len() != 1 => Some(Builtin::Max),
            "set" => Some(Builtin::SetNew),
            "heap" => Some(Builtin::HeapNew),
            "list" => Some(Builtin::ListNew),
            "panic" => Some(Builtin::Panic),
            _ => None,
        };
        if let Some(f) = builtin {
            if matches!(f, Builtin::Print | Builtin::Eprint) {
                self.note_io();
            }
            let arity_ok = match f {
                Builtin::Range => (1..=3).contains(&args.len()),
                Builtin::SetNew | Builtin::HeapNew | Builtin::ListNew => args.len() <= 1,
                Builtin::Panic => args.len() == 1,
                Builtin::Min | Builtin::Max => !args.is_empty(),
                _ => true,
            };
            if !arity_ok {
                self.err("E0206", span, format!("wrong number of arguments to `{n}`"));
            }
            return Ex::CallBuiltin { f, args: self.exprs(args), span };
        }
        // Any method can be called as a function: `len(xs)` is `xs.len()`.
        if is_method(n) && !args.is_empty() {
            let name = Ident { name: n.clone(), span: callee.span };
            // Python's `map(str, xs)` and `filter(f, xs)` are `xs.map(str)`.
            if args.len() == 2 && crate::builtins::hof_arg(n, 0) && self.is_fn_expr(&args[0]) {
                return self.method(&args[1], &name, &args[..1], span);
            }
            return self.method(&args[0], &name, &args[1..], span);
        }
        self.undefined(n, callee.span);
        Ex::Lit(Value::Unit)
    }

    fn variant_ctor(&mut self, eid: u32, tag: u32, args: &[Expr], span: Span) -> Ex {
        let v = &self.prog.enums[eid as usize].variants[tag as usize];
        if v.fields.len() != args.len() {
            let msg = format!("`{}` takes {} field(s), got {}", v.name, v.fields.len(), args.len());
            self.err("E0206", span, msg);
        }
        Ex::Variant { id: eid, tag, args: self.exprs(args), span }
    }

    fn method(&mut self, obj: &Expr, name: &Ident, args: &[Expr], span: Span) -> Ex {
        let n = name.name.as_str();
        if let ExprKind::Name(ns) = &obj.kind {
            if !self.is_local(ns) {
                if NAMESPACES.contains(&ns.as_str()) {
                    if ns == "math" {
                        // `math.sqrt(x)` is `x.sqrt()`.
                        if let Some((first, rest)) = args.split_first() {
                            if is_method(n) {
                                return self.method(first, name, rest, span);
                            }
                        }
                        self.err("E0203", name.span, format!("`math` has no `{n}`; call the method on the number: `x.{n}()`"));
                        return Ex::Lit(Value::Unit);
                    }
                    return self.ns_call(ns, name, args, span);
                }
                if self.enum_ids.contains_key(ns) {
                    let Some((eid, tag)) = self.variant_named(name, Some(&Ident { name: ns.clone(), span: obj.span })) else {
                        return Ex::Lit(Value::Unit);
                    };
                    return self.variant_ctor(eid, tag, args, span);
                }
                if self.struct_ids.contains_key(ns) && !self.fn_ids.contains_key(n) {
                    self.err_fix("E0203", name.span, format!("no static methods; construct with braces: `{ns}{{...}}`"), format!("{ns}{{...}}"));
                    return Ex::Lit(Value::Unit);
                }
                if conv_for(ns).is_some() && !self.fn_ids.contains_key(n) {
                    let hint = match n {
                        "parse" | "from_str" | "from" | "new" => format!("convert with `{ns}(x)` or `s.parse()?`"),
                        _ => format!("`{ns}` has no `{n}`"),
                    };
                    self.err("E0203", name.span, hint);
                    return Ex::Lit(Value::Unit);
                }
            }
        }
        let user = self.fn_ids.get(n).cloned();
        if let Some(ids) = &user {
            self.note_call(ids);
            self.check_arity(n, ids, args.len(), span, 1);
        }
        if user.is_none() && !is_method(n) {
            match crate::builtins::method_hint(n) {
                Some(h) => self.err("E0203", name.span, format!("no method `{n}`: {h}")),
                None => {
                    let mut cands: Vec<&str> = crate::builtins::METHODS.to_vec();
                    let user_names: Vec<String> = self.fn_ids.keys().cloned().collect();
                    cands.extend(user_names.iter().map(|s| s.as_str()));
                    match closest(n, cands.into_iter()) {
                        Some(c) => self.err_fix("E0203", name.span, format!("no method `{n}`"), c.to_string()),
                        None => self.err("E0203", name.span, format!("no method `{n}`")),
                    }
                }
            }
        }
        let recv = match self.place_opt(obj) {
            Some(p) => {
                if is_mutator(n) && user.is_none() && !p.mutable {
                    let pname = p.name.clone();
                    self.immutable_err(&pname, obj.span);
                }
                Recv::Place(p)
            }
            None => Recv::Value(Box::new(self.expr(obj))),
        };
        let args = args.iter().enumerate().map(|(i, a)| {
            let f = self.method_param_is_fn(n, i);
            self.arg(a, f)
        });
        let args = args.collect();
        Ex::Method { recv, name: n.into(), user: user.map(|u| u.into()), args, span }
    }

    fn match_expr(&mut self, scrutinee: &Expr, arms: &[MatchArm], span: Span) -> Ex {
        let scrut = Box::new(self.expr(scrutinee));
        let mut out = Vec::new();
        for a in arms {
            self.push_scope();
            let pat = self.pattern(&a.pat, &mut HashMap::new());
            let guard = a.guard.as_ref().map(|g| self.expr(g));
            let body = self.block_with_tail(&a.body, false);
            self.pop_scope();
            out.push(Arm { pat, guard, body });
        }
        self.check_exhaustive(&out, span);
        Ex::Match { scrut, arms: out, span }
    }

    fn check_exhaustive(&mut self, arms: &[Arm], span: Span) {
        fn irrefutable(p: &PatIr) -> bool {
            match p {
                PatIr::Wild | PatIr::Bind(_) => true,
                PatIr::Tuple(ps) => ps.iter().all(irrefutable),
                PatIr::Or(ps) => ps.iter().any(irrefutable),
                _ => false,
            }
        }
        let mut enum_id = None;
        let mut covered = HashSet::new();
        for a in arms {
            let pats: Vec<&PatIr> = match &a.pat {
                PatIr::Or(ps) => ps.iter().collect(),
                p => vec![p],
            };
            for p in pats {
                match p {
                    PatIr::Variant { id, tag, args } => {
                        if enum_id.is_some_and(|e| e != *id) {
                            return;
                        }
                        enum_id = Some(*id);
                        if a.guard.is_none() && args.iter().all(irrefutable) {
                            covered.insert(*tag);
                        }
                    }
                    p if irrefutable(p) && a.guard.is_none() => return,
                    _ => return,
                }
            }
        }
        if let Some(eid) = enum_id {
            let e = &self.prog.enums[eid as usize];
            let missing: Vec<&str> = e.variants.iter().enumerate().filter(|(i, _)| !covered.contains(&(*i as u32))).map(|(_, v)| v.name.as_str()).collect();
            if !missing.is_empty() {
                let msg = format!("match on `{}` is missing {}; add the arm(s) or `_:`", e.name, missing.join(", "));
                self.err("E0213", span, msg);
            }
        }
    }
}
