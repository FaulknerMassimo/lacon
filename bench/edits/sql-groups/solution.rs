//! A small SQL database. Reads statements separated by semicolons from
//! standard input and prints each one's result.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::io::{self, Read};

const KEYWORDS: &[&str] = &[
    "add", "alter", "and", "as", "asc", "begin", "between", "by", "column", "commit", "create", "delete", "desc",
    "describe", "drop", "from", "group", "having", "in", "insert", "int", "into", "is", "key", "like", "limit",
    "not", "null", "or", "order", "primary", "rollback", "select", "set", "show", "table", "tables", "text", "update",
    "values", "where",
];
const AGGREGATES: &[&str] = &["count", "sum", "min", "max", "avg"];
/// Scalar functions and how many arguments each takes (None: one or more).
const FUNCTIONS: &[(&str, Option<usize>)] =
    &[("upper", Some(1)), ("lower", Some(1)), ("length", Some(1)), ("abs", Some(1)), ("coalesce", None)];
const COMPARISONS: &[&str] = &["=", "!=", "<", "<=", ">", ">="];

type Res<T> = Result<T, String>;

// ----- reading statements -----

/// The statements in src: split at semicolons outside strings, with --
/// comments removed. Text after the last semicolon is a statement too.
fn split_statements(src: &str) -> Vec<String> {
    let cs: Vec<char> = src.chars().collect();
    let mut stmts = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if quoted {
            cur.push(c);
            if c == '\'' {
                quoted = false;
            }
        } else if c == '\'' {
            quoted = true;
            cur.push(c);
        } else if c == '-' && cs.get(i + 1) == Some(&'-') {
            while i < cs.len() && cs[i] != '\n' {
                i += 1;
            }
            continue;
        } else if c == ';' {
            stmts.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
        i += 1;
    }
    stmts.push(cur);
    stmts.into_iter().filter(|s| !s.trim().is_empty()).collect()
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Num,
    Str,
    Name,
    Kw,
    Op,
    End,
}

#[derive(Clone, Debug)]
struct Tok {
    kind: Kind,
    text: String,
}

/// A statement's tokens. Names and keywords are lowercased.
fn tokenize(src: &str) -> Res<Vec<Tok>> {
    let cs: Vec<char> = src.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() {
            let start = i;
            while i < cs.len() && cs[i].is_ascii_digit() {
                i += 1;
            }
            toks.push(Tok { kind: Kind::Num, text: cs[start..i].iter().collect() });
        } else if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < cs.len() && (cs[i].is_alphanumeric() || cs[i] == '_') {
                i += 1;
            }
            let word = cs[start..i].iter().collect::<String>().to_lowercase();
            let kind = if KEYWORDS.contains(&word.as_str()) { Kind::Kw } else { Kind::Name };
            toks.push(Tok { kind, text: word });
        } else if c == '\'' {
            let mut text = String::new();
            let mut j = i + 1;
            loop {
                if j >= cs.len() {
                    return Err("unterminated string".to_string());
                }
                if cs[j] == '\'' {
                    if cs.get(j + 1) == Some(&'\'') {
                        text.push('\'');
                        j += 2;
                        continue;
                    }
                    break;
                }
                text.push(cs[j]);
                j += 1;
            }
            toks.push(Tok { kind: Kind::Str, text });
            i = j + 1;
        } else if let Some(op) = ["<=", ">=", "!=", "<>", "||"]
            .iter()
            .find(|op| cs[i..].starts_with(&op.chars().collect::<Vec<_>>()))
        {
            toks.push(Tok { kind: Kind::Op, text: op.to_string() });
            i += 2;
        } else if "(),*+-/%=<>".contains(c) {
            toks.push(Tok { kind: Kind::Op, text: c.to_string() });
            i += 1;
        } else {
            return Err(format!("unexpected character '{}'", c));
        }
    }
    Ok(toks)
}

// ----- syntax -----

#[derive(Clone, Debug)]
enum Expr {
    /// A number, a string or NULL.
    Lit(Value),
    Col(String),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    /// Arithmetic, comparison, ||, and, or.
    Bin(String, Box<Expr>, Box<Expr>),
    /// e IS NULL, or e IS NOT NULL when the flag is set.
    IsNull(Box<Expr>, bool),
    Like(Box<Expr>, Box<Expr>),
    In(Box<Expr>, Vec<Expr>),
    Between(Box<Expr>, Box<Expr>, Box<Expr>),
    /// A scalar function.
    Call(String, Vec<Expr>),
    /// An aggregate; the argument is None for count(*).
    Agg(String, Option<Box<Expr>>),
}

#[derive(Clone, Debug)]
struct Column {
    name: String,
    ty: &'static str, // "INT" or "TEXT"
    not_null: bool,
    primary: bool,
}

struct Select {
    /// (expression, alias); the expression is None for *.
    items: Vec<(Option<Expr>, Option<String>)>,
    table: Option<String>,
    where_: Option<Expr>,
    /// The GROUP BY column names.
    group: Vec<String>,
    having: Option<Expr>,
    /// (expression, descending)
    order: Vec<(Expr, bool)>,
    limit: Option<usize>,
}

enum Stmt {
    Select(Select),
    /// columns is None for every column, in order.
    Insert { table: String, columns: Option<Vec<String>>, rows: Vec<Vec<Expr>> },
    Update { table: String, assignments: Vec<(String, Expr)>, where_: Option<Expr> },
    Delete { table: String, where_: Option<Expr> },
    CreateTable { name: String, columns: Vec<Column> },
    DropTable(String),
    AlterAdd { table: String, column: Column },
    Describe(String),
    /// A statement that is only its keywords: SHOW TABLES ("show"), BEGIN,
    /// COMMIT or ROLLBACK.
    Simple(String),
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn new(toks: Vec<Tok>) -> Self {
        Parser { toks, pos: 0 }
    }

    fn peek(&self, ahead: usize) -> (Kind, &str) {
        match self.toks.get(self.pos + ahead) {
            Some(t) => (t.kind, t.text.as_str()),
            None => (Kind::End, ""),
        }
    }

    fn next(&mut self) -> Res<Tok> {
        if self.pos >= self.toks.len() {
            return Err("unexpected end of statement".to_string());
        }
        self.pos += 1;
        Ok(self.toks[self.pos - 1].clone())
    }

    /// Whether a token is the keyword or operator text.
    fn at(&self, text: &str, ahead: usize) -> bool {
        let (kind, t) = self.peek(ahead);
        matches!(kind, Kind::Kw | Kind::Op) && t == text
    }

    fn accept(&mut self, text: &str) -> bool {
        if self.at(text, 0) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, text: &str) -> Res<()> {
        if self.accept(text) {
            Ok(())
        } else {
            Err(self.unexpected())
        }
    }

    /// The error for the token at the current position.
    fn unexpected(&self) -> String {
        match self.peek(0) {
            (Kind::End, _) => "unexpected end of statement".to_string(),
            (_, t) => format!("syntax error at '{}'", t),
        }
    }

    fn name(&mut self) -> Res<String> {
        if self.peek(0).0 != Kind::Name {
            return Err(self.unexpected());
        }
        Ok(self.next()?.text)
    }

    /// Names separated by commas, through a closing parenthesis.
    fn names(&mut self) -> Res<Vec<String>> {
        let mut names = vec![self.name()?];
        while self.accept(",") {
            names.push(self.name()?);
        }
        self.expect(")")?;
        Ok(names)
    }

    fn done(&self) -> Res<()> {
        if self.pos < self.toks.len() {
            return Err(self.unexpected());
        }
        Ok(())
    }

    // Expressions, loosest-binding first.

    fn expr(&mut self) -> Res<Expr> {
        let mut e = self.conjunction()?;
        while self.accept("or") {
            e = Expr::Bin("or".to_string(), Box::new(e), Box::new(self.conjunction()?));
        }
        Ok(e)
    }

    fn conjunction(&mut self) -> Res<Expr> {
        let mut e = self.negation()?;
        while self.accept("and") {
            e = Expr::Bin("and".to_string(), Box::new(e), Box::new(self.negation()?));
        }
        Ok(e)
    }

    fn negation(&mut self) -> Res<Expr> {
        if self.accept("not") {
            return Ok(Expr::Not(Box::new(self.negation()?)));
        }
        self.comparison()
    }

    fn comparison(&mut self) -> Res<Expr> {
        let e = self.additive()?;
        if self.accept("is") {
            let negated = self.accept("not");
            self.expect("null")?;
            return Ok(Expr::IsNull(Box::new(e), negated));
        }
        let negated = self.at("not", 0) && ["like", "in", "between"].iter().any(|k| self.at(k, 1));
        if negated {
            self.next()?;
        }
        let r = if self.accept("like") {
            Expr::Like(Box::new(e), Box::new(self.additive()?))
        } else if self.accept("in") {
            self.expect("(")?;
            let mut items = vec![self.expr()?];
            while self.accept(",") {
                items.push(self.expr()?);
            }
            self.expect(")")?;
            Expr::In(Box::new(e), items)
        } else if self.accept("between") {
            let low = self.additive()?;
            self.expect("and")?;
            Expr::Between(Box::new(e), Box::new(low), Box::new(self.additive()?))
        } else {
            let (kind, t) = self.peek(0);
            if kind == Kind::Op && (COMPARISONS.contains(&t) || t == "<>") {
                let op = if t == "<>" { "!=".to_string() } else { t.to_string() };
                self.next()?;
                return Ok(Expr::Bin(op, Box::new(e), Box::new(self.additive()?)));
            }
            return Ok(e);
        };
        Ok(if negated { Expr::Not(Box::new(r)) } else { r })
    }

    fn additive(&mut self) -> Res<Expr> {
        let mut e = self.multiplicative()?;
        while matches!(self.peek(0), (Kind::Op, "+" | "-" | "||")) {
            let op = self.next()?.text;
            e = Expr::Bin(op, Box::new(e), Box::new(self.multiplicative()?));
        }
        Ok(e)
    }

    fn multiplicative(&mut self) -> Res<Expr> {
        let mut e = self.unary()?;
        while matches!(self.peek(0), (Kind::Op, "*" | "/" | "%")) {
            let op = self.next()?.text;
            e = Expr::Bin(op, Box::new(e), Box::new(self.unary()?));
        }
        Ok(e)
    }

    fn unary(&mut self) -> Res<Expr> {
        if self.accept("-") {
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Res<Expr> {
        let (kind, t) = self.peek(0);
        let t = t.to_string();
        match kind {
            Kind::Num => {
                self.next()?;
                let n = t.parse().map_err(|_| format!("number too large: {}", t))?;
                Ok(Expr::Lit(Value::Int(n)))
            }
            Kind::Str => {
                self.next()?;
                Ok(Expr::Lit(Value::Text(t)))
            }
            Kind::Name => {
                self.next()?;
                if self.accept("(") {
                    return self.call(t);
                }
                Ok(Expr::Col(t))
            }
            _ if self.accept("null") => Ok(Expr::Lit(Value::Null)),
            _ if self.accept("(") => {
                let e = self.expr()?;
                self.expect(")")?;
                Ok(e)
            }
            _ => Err(self.unexpected()),
        }
    }

    /// A function call, after its opening parenthesis.
    fn call(&mut self, name: String) -> Res<Expr> {
        if AGGREGATES.contains(&name.as_str()) {
            if name == "count" && self.accept("*") {
                self.expect(")")?;
                return Ok(Expr::Agg(name, None));
            }
            let arg = self.expr()?;
            self.expect(")")?;
            return Ok(Expr::Agg(name, Some(Box::new(arg))));
        }
        let Some(&(_, want)) = FUNCTIONS.iter().find(|(f, _)| *f == name) else {
            return Err(format!("no such function {}", name));
        };
        let mut args = Vec::new();
        if !self.accept(")") {
            args.push(self.expr()?);
            while self.accept(",") {
                args.push(self.expr()?);
            }
            self.expect(")")?;
        }
        match want {
            None if args.is_empty() => return Err(format!("{}() needs at least one argument", name)),
            Some(n) if args.len() != n => {
                return Err(format!("{}() takes {} argument{}", name, n, if n == 1 { "" } else { "s" }))
            }
            _ => {}
        }
        Ok(Expr::Call(name, args))
    }
}

fn parse_statement(toks: Vec<Tok>) -> Res<Stmt> {
    let mut p = Parser::new(toks);
    let stmt = if p.accept("select") {
        Stmt::Select(parse_select(&mut p)?)
    } else if p.accept("insert") {
        parse_insert(&mut p)?
    } else if p.accept("update") {
        parse_update(&mut p)?
    } else if p.accept("delete") {
        p.expect("from")?;
        let table = p.name()?;
        Stmt::Delete { table, where_: parse_where(&mut p)? }
    } else if p.accept("create") {
        p.expect("table")?;
        parse_create(&mut p)?
    } else if p.accept("drop") {
        p.expect("table")?;
        Stmt::DropTable(p.name()?)
    } else if p.accept("alter") {
        p.expect("table")?;
        let table = p.name()?;
        p.expect("add")?;
        p.accept("column");
        Stmt::AlterAdd { table, column: parse_column(&mut p)? }
    } else if p.accept("show") {
        p.expect("tables")?;
        Stmt::Simple("show".to_string())
    } else if p.accept("describe") {
        Stmt::Describe(p.name()?)
    } else if p.at("begin", 0) || p.at("commit", 0) || p.at("rollback", 0) {
        Stmt::Simple(p.next()?.text)
    } else {
        return Err(p.unexpected());
    };
    p.done()?;
    Ok(stmt)
}

fn parse_where(p: &mut Parser) -> Res<Option<Expr>> {
    Ok(if p.accept("where") { Some(p.expr()?) } else { None })
}

fn parse_select(p: &mut Parser) -> Res<Select> {
    let mut items = Vec::new();
    loop {
        if p.accept("*") {
            items.push((None, None));
        } else {
            let e = p.expr()?;
            let alias = if p.accept("as") { Some(p.name()?) } else { None };
            items.push((Some(e), alias));
        }
        if !p.accept(",") {
            break;
        }
    }
    let table = if p.accept("from") { Some(p.name()?) } else { None };
    let where_ = parse_where(p)?;
    let mut group = Vec::new();
    if p.accept("group") {
        p.expect("by")?;
        group.push(p.name()?);
        while p.accept(",") {
            group.push(p.name()?);
        }
    }
    let having = if p.accept("having") { Some(p.expr()?) } else { None };
    let mut order = Vec::new();
    if p.accept("order") {
        p.expect("by")?;
        loop {
            let e = p.expr()?;
            let desc = p.accept("desc");
            if !desc {
                p.accept("asc");
            }
            order.push((e, desc));
            if !p.accept(",") {
                break;
            }
        }
    }
    let mut limit = None;
    if p.accept("limit") {
        if p.peek(0).0 != Kind::Num {
            return Err(p.unexpected());
        }
        let t = p.next()?.text;
        limit = Some(t.parse().map_err(|_| format!("number too large: {}", t))?);
    }
    Ok(Select { items, table, where_, group, having, order, limit })
}

fn parse_insert(p: &mut Parser) -> Res<Stmt> {
    p.expect("into")?;
    let table = p.name()?;
    let columns = if p.accept("(") { Some(p.names()?) } else { None };
    p.expect("values")?;
    let mut rows = Vec::new();
    loop {
        p.expect("(")?;
        let mut values = vec![p.expr()?];
        while p.accept(",") {
            values.push(p.expr()?);
        }
        p.expect(")")?;
        rows.push(values);
        if !p.accept(",") {
            break;
        }
    }
    Ok(Stmt::Insert { table, columns, rows })
}

fn parse_update(p: &mut Parser) -> Res<Stmt> {
    let table = p.name()?;
    p.expect("set")?;
    let mut assignments = Vec::new();
    loop {
        let name = p.name()?;
        p.expect("=")?;
        assignments.push((name, p.expr()?));
        if !p.accept(",") {
            break;
        }
    }
    Ok(Stmt::Update { table, assignments, where_: parse_where(p)? })
}

/// A column definition: a name, a type and constraints.
fn parse_column(p: &mut Parser) -> Res<Column> {
    let name = p.name()?;
    let ty = if p.accept("int") {
        "INT"
    } else if p.accept("text") {
        "TEXT"
    } else {
        return Err(p.unexpected());
    };
    let mut column = Column { name, ty, not_null: false, primary: false };
    loop {
        if p.accept("not") {
            p.expect("null")?;
            column.not_null = true;
        } else if p.accept("primary") {
            p.expect("key")?;
            column.primary = true;
            column.not_null = true;
        } else {
            return Ok(column);
        }
    }
}

fn parse_create(p: &mut Parser) -> Res<Stmt> {
    let name = p.name()?;
    p.expect("(")?;
    let mut columns = vec![parse_column(p)?];
    while p.accept(",") {
        columns.push(parse_column(p)?);
    }
    p.expect(")")?;
    Ok(Stmt::CreateTable { name, columns })
}

/// The subexpressions of e.
fn children(e: &Expr) -> Vec<&Expr> {
    match e {
        Expr::Lit(_) | Expr::Col(_) => vec![],
        Expr::Neg(x) | Expr::Not(x) | Expr::IsNull(x, _) => vec![x],
        Expr::Bin(_, l, r) => vec![l, r],
        Expr::Like(x, pattern) => vec![x, pattern],
        Expr::In(x, items) => std::iter::once(&**x).chain(items).collect(),
        Expr::Between(x, low, high) => vec![x, low, high],
        Expr::Call(_, args) => args.iter().collect(),
        Expr::Agg(_, arg) => arg.iter().map(|a| &**a).collect(),
    }
}

/// The columns e refers to, in order.
fn columns_in(e: &Expr) -> Vec<String> {
    match e {
        Expr::Col(name) => vec![name.clone()],
        _ => children(e).into_iter().flat_map(columns_in).collect(),
    }
}

/// The aggregates in e, outermost first.
fn aggregates_in(e: &Expr) -> Vec<&Expr> {
    match e {
        Expr::Agg(..) => vec![e],
        _ => children(e).into_iter().flat_map(aggregates_in).collect(),
    }
}

/// The columns e refers to outside any aggregate.
fn bare_columns(e: &Expr) -> Vec<String> {
    match e {
        Expr::Agg(..) => vec![],
        Expr::Col(name) => vec![name.clone()],
        _ => children(e).into_iter().flat_map(bare_columns).collect(),
    }
}

/// e as SQL text, to head a result column that has no alias.
fn render(e: &Expr) -> String {
    let list = |xs: &[Expr]| xs.iter().map(render).collect::<Vec<_>>().join(", ");
    match e {
        Expr::Lit(Value::Null) => "null".to_string(),
        Expr::Lit(Value::Int(n)) => n.to_string(),
        Expr::Lit(Value::Text(s)) => format!("'{}'", s.replace('\'', "''")),
        Expr::Col(name) => name.clone(),
        Expr::Neg(x) => format!("-{}", operand(x)),
        Expr::Not(x) => format!("not {}", operand(x)),
        Expr::IsNull(x, negated) => format!("{} is {}null", operand(x), if *negated { "not " } else { "" }),
        Expr::Bin(op, l, r) => format!("{} {} {}", operand(l), op, operand(r)),
        Expr::Like(x, pattern) => format!("{} like {}", operand(x), operand(pattern)),
        Expr::In(x, items) => format!("{} in ({})", operand(x), list(items)),
        Expr::Between(x, low, high) => format!("{} between {} and {}", operand(x), operand(low), operand(high)),
        Expr::Call(name, args) => format!("{}({})", name, list(args)),
        Expr::Agg(name, arg) => format!("{}({})", name, arg.as_ref().map_or("*".to_string(), |a| render(a))),
    }
}

/// e rendered as part of a larger expression: in parentheses if it has
/// operators of its own.
fn operand(e: &Expr) -> String {
    match e {
        Expr::Lit(_) | Expr::Col(_) | Expr::Call(..) | Expr::Agg(..) => render(e),
        _ => format!("({})", render(e)),
    }
}

// ----- values -----

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Value {
    Null,
    Int(i64),
    Text(String),
}

impl Value {
    fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "NULL",
            Value::Int(_) => "INT",
            Value::Text(_) => "TEXT",
        }
    }

    fn show(&self) -> String {
        match self {
            Value::Null => "NULL".to_string(),
            Value::Int(n) => n.to_string(),
            Value::Text(s) => s.clone(),
        }
    }
}

/// A condition's value: None for NULL, else whether it is nonzero.
fn truth(v: &Value) -> Res<Option<bool>> {
    match v {
        Value::Null => Ok(None),
        Value::Int(n) => Ok(Some(*n != 0)),
        Value::Text(_) => Err("a condition must be INT, not TEXT".to_string()),
    }
}

fn compare(op: &str, a: &Value, b: &Value) -> Res<bool> {
    let ord = match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Text(x), Value::Text(y)) => x.cmp(y),
        _ => return Err(format!("cannot compare {} with {}", a.type_name(), b.type_name())),
    };
    Ok(match op {
        "=" => ord == Ordering::Equal,
        "!=" => ord != Ordering::Equal,
        "<" => ord == Ordering::Less,
        "<=" => ord != Ordering::Greater,
        ">" => ord == Ordering::Greater,
        _ => ord != Ordering::Less,
    })
}

fn binary(op: &str, a: Value, b: Value) -> Res<Value> {
    if op == "and" || op == "or" {
        let (x, y) = (truth(&a)?, truth(&b)?);
        let (decisive, other) = if op == "and" { (false, 1) } else { (true, 0) };
        if x == Some(decisive) || y == Some(decisive) {
            return Ok(Value::Int(1 - other));
        }
        return Ok(if x.is_none() || y.is_none() { Value::Null } else { Value::Int(other) });
    }
    if a == Value::Null || b == Value::Null {
        return Ok(Value::Null);
    }
    if op == "||" {
        return Ok(Value::Text(a.show() + &b.show()));
    }
    if COMPARISONS.contains(&op) {
        return Ok(Value::Int(compare(op, &a, &b)? as i64));
    }
    let (Value::Int(a), Value::Int(b)) = (a, b) else {
        return Err(format!("cannot apply {} to TEXT", op));
    };
    match op {
        "+" => Ok(Value::Int(a + b)),
        "-" => Ok(Value::Int(a - b)),
        "*" => Ok(Value::Int(a * b)),
        _ if b == 0 => Err("division by zero".to_string()),
        // `/` and `%` truncate toward zero.
        "/" => Ok(Value::Int(a / b)),
        _ => Ok(Value::Int(a % b)),
    }
}

/// Whether s matches pattern, in which % is any run of characters and _ any
/// one character.
fn like(s: &str, pattern: &str) -> bool {
    let s: Vec<char> = s.chars().collect();
    // matched[i]: whether the pattern so far matches s[..i]
    let mut matched = vec![false; s.len() + 1];
    matched[0] = true;
    for c in pattern.chars() {
        if c == '%' {
            for i in 1..=s.len() {
                matched[i] = matched[i] || matched[i - 1];
            }
        } else {
            for i in (1..=s.len()).rev() {
                matched[i] = matched[i - 1] && (c == '_' || c == s[i - 1]);
            }
            matched[0] = false;
        }
    }
    matched[s.len()]
}

fn call(name: &str, args: Vec<Value>) -> Res<Value> {
    if name == "coalesce" {
        return Ok(args.into_iter().find(|a| *a != Value::Null).unwrap_or(Value::Null));
    }
    match (name, &args[0]) {
        (_, Value::Null) => Ok(Value::Null),
        ("abs", Value::Int(n)) => Ok(Value::Int(n.abs())),
        ("abs", _) => Err("abs() needs INT, not TEXT".to_string()),
        ("upper", Value::Text(s)) => Ok(Value::Text(s.to_uppercase())),
        ("lower", Value::Text(s)) => Ok(Value::Text(s.to_lowercase())),
        ("length", Value::Text(s)) => Ok(Value::Int(s.chars().count() as i64)),
        _ => Err(format!("{}() needs TEXT, not INT", name)),
    }
}

/// A row as a map from column name to value.
type Row = HashMap<String, Value>;

/// The value of e for row. In a query with aggregates, group holds the rows
/// they range over.
fn evaluate(e: &Expr, row: &Row, group: Option<&[Row]>) -> Res<Value> {
    match e {
        Expr::Lit(v) => Ok(v.clone()),
        Expr::Col(name) => row.get(name).cloned().ok_or_else(|| format!("no such column {}", name)),
        Expr::Neg(x) => match evaluate(x, row, group)? {
            Value::Null => Ok(Value::Null),
            Value::Int(n) => Ok(Value::Int(-n)),
            Value::Text(_) => Err("cannot apply - to TEXT".to_string()),
        },
        Expr::Not(x) => Ok(match truth(&evaluate(x, row, group)?)? {
            None => Value::Null,
            Some(t) => Value::Int(!t as i64),
        }),
        Expr::IsNull(x, negated) => Ok(Value::Int(((evaluate(x, row, group)? == Value::Null) != *negated) as i64)),
        Expr::Bin(op, l, r) => binary(op, evaluate(l, row, group)?, evaluate(r, row, group)?),
        Expr::Like(x, pattern) => match (evaluate(x, row, group)?, evaluate(pattern, row, group)?) {
            (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
            (Value::Text(s), Value::Text(p)) => Ok(Value::Int(like(&s, &p) as i64)),
            _ => Err("LIKE needs TEXT, not INT".to_string()),
        },
        Expr::In(x, items) => {
            let v = evaluate(x, row, group)?;
            let mut saw_null = false;
            for item in items {
                match binary("=", v.clone(), evaluate(item, row, group)?)? {
                    Value::Int(1) => return Ok(Value::Int(1)),
                    Value::Null => saw_null = true,
                    _ => {}
                }
            }
            Ok(if saw_null { Value::Null } else { Value::Int(0) })
        }
        Expr::Between(x, low, high) => {
            let v = evaluate(x, row, group)?;
            let lo = binary(">=", v.clone(), evaluate(low, row, group)?)?;
            let hi = binary("<=", v, evaluate(high, row, group)?)?;
            binary("and", lo, hi)
        }
        Expr::Call(name, args) => {
            let values = args.iter().map(|a| evaluate(a, row, group)).collect::<Res<Vec<_>>>()?;
            call(name, values)
        }
        Expr::Agg(name, arg) => match group {
            None => Err(format!("{}() is not allowed here", name)),
            Some(rows) => aggregate(name, arg.as_deref(), rows),
        },
    }
}

/// An aggregate's value over the rows in group; arg is None for count(*).
fn aggregate(name: &str, arg: Option<&Expr>, group: &[Row]) -> Res<Value> {
    let Some(arg) = arg else {
        return Ok(Value::Int(group.len() as i64));
    };
    let mut values = Vec::new();
    for row in group {
        let v = evaluate(arg, row, None)?;
        if v != Value::Null {
            values.push(v);
        }
    }
    if name == "count" {
        return Ok(Value::Int(values.len() as i64));
    }
    if values.is_empty() {
        return Ok(Value::Null);
    }
    if name == "min" || name == "max" {
        let mut best = values[0].clone();
        for v in &values[1..] {
            if compare(if name == "min" { "<" } else { ">" }, v, &best)? {
                best = v.clone();
            }
        }
        return Ok(best);
    }
    let mut total = 0;
    for v in &values {
        match v {
            Value::Int(n) => total += n,
            _ => return Err(format!("{}() needs INT, not TEXT", name)),
        }
    }
    if name == "sum" {
        return Ok(Value::Int(total));
    }
    Ok(Value::Int(total / values.len() as i64)) // avg truncates toward zero
}

// ----- tables -----

#[derive(Clone)]
struct Table {
    columns: Vec<Column>,
    rows: Vec<Vec<Value>>,
}

impl Table {
    fn names(&self) -> Vec<String> {
        self.columns.iter().map(|c| c.name.clone()).collect()
    }

    fn index(&self, name: &str) -> Res<usize> {
        self.columns.iter().position(|c| c.name == name).ok_or_else(|| format!("no such column {}", name))
    }

    /// A stored row as a map from column name to value.
    fn row(&self, values: &[Value]) -> Row {
        self.columns.iter().map(|c| c.name.clone()).zip(values.iter().cloned()).collect()
    }
}

struct Database {
    tables: HashMap<String, Table>,
    /// The tables as BEGIN found them, in a transaction.
    saved: Option<HashMap<String, Table>>,
}

impl Database {
    fn table(&self, name: &str) -> Res<&Table> {
        self.tables.get(name).ok_or_else(|| format!("no such table {}", name))
    }
}

/// Fails on the first column e refers to that isn't in names.
fn check_columns(e: &Expr, names: &[String]) -> Res<()> {
    for name in columns_in(e) {
        if !names.contains(&name) {
            return Err(format!("no such column {}", name));
        }
    }
    Ok(())
}

fn forbid_aggregates(e: &Expr, clause: &str) -> Res<()> {
    if let Some(Expr::Agg(name, _)) = aggregates_in(e).first() {
        return Err(format!("{}() is not allowed in {}", name, clause));
    }
    Ok(())
}

fn check_nesting(e: &Expr) -> Res<()> {
    for agg in aggregates_in(e) {
        if children(agg).into_iter().any(|c| !aggregates_in(c).is_empty()) {
            return Err("aggregates cannot be nested".to_string());
        }
    }
    Ok(())
}

fn check_value(column: &Column, v: &Value) -> Res<()> {
    if *v == Value::Null {
        if column.not_null {
            return Err(format!("column {} cannot be NULL", column.name));
        }
    } else if v.type_name() != column.ty {
        return Err(format!("column {} is {}, not {}", column.name, column.ty, v.type_name()));
    }
    Ok(())
}

/// Fails if two rows share a primary key.
fn check_keys(table: &Table, rows: &[Vec<Value>]) -> Res<()> {
    for (i, c) in table.columns.iter().enumerate() {
        if !c.primary {
            continue;
        }
        let mut seen = HashSet::new();
        for row in rows {
            if !seen.insert(&row[i]) {
                return Err(format!("duplicate key {} in column {}", row[i].show(), c.name));
            }
        }
    }
    Ok(())
}

/// Whether a WHERE clause keeps a row: there is none, or it is true.
fn keeps(where_: Option<&Expr>, row: &Row) -> Res<bool> {
    match where_ {
        None => Ok(true),
        Some(e) => Ok(truth(&evaluate(e, row, None)?)? == Some(true)),
    }
}

/// A result as lines of aligned columns: numbers to the right, the rest to
/// the left.
fn format_table(headers: &[String], rows: &[Vec<Value>]) -> Vec<String> {
    let cells: Vec<Vec<String>> = rows.iter().map(|r| r.iter().map(Value::show).collect()).collect();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in &cells {
        for (w, t) in widths.iter_mut().zip(row) {
            *w = (*w).max(t.chars().count());
        }
    }
    let head: Vec<String> = headers.iter().zip(&widths).map(|(h, &w)| format!("{:<w$}", h)).collect();
    let mut lines = vec![head.join(" | ").trim_end().to_string()];
    lines.push(widths.iter().map(|&w| "-".repeat(w)).collect::<Vec<_>>().join("-+-"));
    for (row, texts) in rows.iter().zip(&cells) {
        let parts: Vec<String> = row
            .iter()
            .zip(texts)
            .zip(&widths)
            .map(|((v, t), &w)| if matches!(v, Value::Int(_)) { format!("{:>w$}", t) } else { format!("{:<w$}", t) })
            .collect();
        lines.push(parts.join(" | ").trim_end().to_string());
    }
    lines.push(format!("({} {})", rows.len(), if rows.len() == 1 { "row" } else { "rows" }));
    lines
}

// ----- statements -----

fn run_create(db: &mut Database, name: String, columns: Vec<Column>) -> Res<Vec<String>> {
    if db.tables.contains_key(&name) {
        return Err(format!("table {} already exists", name));
    }
    let mut seen = HashSet::new();
    for c in &columns {
        if !seen.insert(&c.name) {
            return Err(format!("duplicate column {}", c.name));
        }
    }
    if columns.iter().filter(|c| c.primary).count() > 1 {
        return Err("a table has at most one primary key".to_string());
    }
    db.tables.insert(name, Table { columns, rows: Vec::new() });
    Ok(vec!["CREATE TABLE".to_string()])
}

fn run_drop(db: &mut Database, name: &str) -> Res<Vec<String>> {
    db.table(name)?;
    db.tables.remove(name);
    Ok(vec!["DROP TABLE".to_string()])
}

fn run_alter(db: &mut Database, name: &str, column: Column) -> Res<Vec<String>> {
    let table = db.table(name)?;
    if table.names().contains(&column.name) {
        return Err(format!("duplicate column {}", column.name));
    }
    if column.primary {
        return Err("cannot add a primary key".to_string());
    }
    if column.not_null && !table.rows.is_empty() {
        return Err(format!("column {} cannot be NULL", column.name));
    }
    let table = db.tables.get_mut(name).unwrap();
    table.columns.push(column);
    for row in &mut table.rows {
        row.push(Value::Null);
    }
    Ok(vec!["ALTER TABLE".to_string()])
}

fn run_insert(db: &mut Database, name: &str, columns: Option<Vec<String>>, rows: Vec<Vec<Expr>>) -> Res<Vec<String>> {
    let table = db.table(name)?;
    let names = columns.unwrap_or_else(|| table.names());
    let mut indexes = Vec::new();
    for n in &names {
        let i = table.index(n)?;
        if indexes.contains(&i) {
            return Err(format!("duplicate column {}", n));
        }
        indexes.push(i);
    }
    let mut new_rows = Vec::new();
    for values in &rows {
        if values.len() != indexes.len() {
            return Err(format!("expected {} values, got {}", indexes.len(), values.len()));
        }
        let mut row = vec![Value::Null; table.columns.len()];
        for (&i, e) in indexes.iter().zip(values) {
            forbid_aggregates(e, "VALUES")?;
            row[i] = evaluate(e, &Row::new(), None)?;
        }
        for (c, v) in table.columns.iter().zip(&row) {
            check_value(c, v)?;
        }
        new_rows.push(row);
    }
    let all: Vec<Vec<Value>> = table.rows.iter().chain(&new_rows).cloned().collect();
    check_keys(table, &all)?;
    let count = new_rows.len();
    db.tables.get_mut(name).unwrap().rows.extend(new_rows);
    Ok(vec![format!("INSERT {}", count)])
}

fn run_update(db: &mut Database, name: &str, assignments: Vec<(String, Expr)>, where_: Option<Expr>) -> Res<Vec<String>> {
    let table = db.table(name)?;
    let names = table.names();
    let mut targets: Vec<(usize, Expr)> = Vec::new();
    for (col, e) in assignments {
        let i = table.index(&col)?;
        if targets.iter().any(|(t, _)| *t == i) {
            return Err(format!("duplicate column {}", col));
        }
        check_columns(&e, &names)?;
        forbid_aggregates(&e, "SET")?;
        targets.push((i, e));
    }
    if let Some(w) = &where_ {
        check_columns(w, &names)?;
        forbid_aggregates(w, "WHERE")?;
    }
    let mut new_rows = Vec::new();
    let mut count = 0;
    for row in &table.rows {
        let ctx = table.row(row);
        let mut row = row.clone();
        if keeps(where_.as_ref(), &ctx)? {
            for (i, e) in &targets {
                row[*i] = evaluate(e, &ctx, None)?;
                check_value(&table.columns[*i], &row[*i])?;
            }
            count += 1;
        }
        new_rows.push(row);
    }
    check_keys(table, &new_rows)?;
    db.tables.get_mut(name).unwrap().rows = new_rows;
    Ok(vec![format!("UPDATE {}", count)])
}

fn run_delete(db: &mut Database, name: &str, where_: Option<Expr>) -> Res<Vec<String>> {
    let table = db.table(name)?;
    let mut keep = Vec::new();
    if let Some(w) = &where_ {
        check_columns(w, &table.names())?;
        forbid_aggregates(w, "WHERE")?;
        for row in &table.rows {
            if !keeps(Some(w), &table.row(row))? {
                keep.push(row.clone());
            }
        }
    }
    let count = table.rows.len() - keep.len();
    db.tables.get_mut(name).unwrap().rows = keep;
    Ok(vec![format!("DELETE {}", count)])
}

/// NULL sorts first, then numbers, then text.
fn sort_key(v: &Value) -> (u8, i64, &str) {
    match v {
        Value::Null => (0, 0, ""),
        Value::Int(n) => (1, *n, ""),
        Value::Text(s) => (2, 0, s),
    }
}

/// rows ordered by their keys, keeping their order where the keys tie.
fn sort_rows(rows: Vec<Vec<Value>>, keys: &[Vec<Value>], descending: &[bool]) -> Vec<Vec<Value>> {
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by(|&a, &b| {
        for (k, &desc) in descending.iter().enumerate() {
            let ord = sort_key(&keys[a][k]).cmp(&sort_key(&keys[b][k]));
            let ord = if desc { ord.reverse() } else { ord };
            if ord != Ordering::Equal {
                return ord;
            }
        }
        Ordering::Equal
    });
    let mut rows: Vec<Option<Vec<Value>>> = rows.into_iter().map(Some).collect();
    order.into_iter().map(|i| rows[i].take().unwrap()).collect()
}

/// One result row per row. ORDER BY sees the row's columns and the result's
/// column names, which win.
fn plain_query(exprs: &[Expr], headers: &[String], order: &[(Expr, bool)], rows: &[Row]) -> Res<Vec<Vec<Value>>> {
    let mut out = Vec::new();
    let mut keys = Vec::new();
    for r in rows {
        let values = exprs.iter().map(|e| evaluate(e, r, None)).collect::<Res<Vec<_>>>()?;
        if !order.is_empty() {
            let mut ctx = r.clone();
            for (h, v) in headers.iter().zip(&values) {
                ctx.insert(h.clone(), v.clone());
            }
            keys.push(order.iter().map(|(e, _)| evaluate(e, &ctx, None)).collect::<Res<Vec<_>>>()?);
        }
        out.push(values);
    }
    let descending: Vec<bool> = order.iter().map(|(_, d)| *d).collect();
    Ok(sort_rows(out, &keys, &descending))
}

/// A single result row, the aggregates over every row.
fn aggregate_query(exprs: &[Expr], rows: &[Row]) -> Res<Vec<Vec<Value>>> {
    for e in exprs {
        if let Some(name) = bare_columns(e).first() {
            return Err(format!("column {} must be used in an aggregate", name));
        }
    }
    let row = exprs.iter().map(|e| evaluate(e, &Row::new(), Some(rows))).collect::<Res<Vec<_>>>()?;
    Ok(vec![row])
}

/// Fails if e uses a column outside aggregates that isn't in allowed.
fn check_grouped(e: &Expr, allowed: &[String]) -> Res<()> {
    for name in bare_columns(e) {
        if !allowed.contains(&name) {
            return Err(format!("column {} must be in GROUP BY or used in an aggregate", name));
        }
    }
    Ok(())
}

/// One result row per group of rows that agree on the GROUP BY columns, the
/// groups in the order of their first rows. Outside aggregates, the select
/// list and HAVING see only the GROUP BY columns, and ORDER BY also the
/// result's column names.
fn grouped_query(exprs: &[Expr], headers: &[String], s: &Select, rows: Vec<Row>) -> Res<Vec<Vec<Value>>> {
    for e in exprs {
        check_grouped(e, &s.group)?;
    }
    if let Some(h) = &s.having {
        check_grouped(h, &s.group)?;
    }
    let visible: Vec<String> = s.group.iter().chain(headers).cloned().collect();
    for (e, _) in &s.order {
        check_grouped(e, &visible)?;
    }
    let mut groups: Vec<(Vec<Value>, Vec<Row>)> = Vec::new();
    let mut index: HashMap<Vec<Value>, usize> = HashMap::new();
    for r in rows {
        let key: Vec<Value> = s.group.iter().map(|c| r[c].clone()).collect();
        let i = *index.entry(key.clone()).or_insert_with(|| {
            groups.push((key, Vec::new()));
            groups.len() - 1
        });
        groups[i].1.push(r);
    }
    let mut out = Vec::new();
    let mut keys = Vec::new();
    for (key, members) in &groups {
        let ctx: Row = s.group.iter().cloned().zip(key.iter().cloned()).collect();
        if let Some(h) = &s.having {
            if truth(&evaluate(h, &ctx, Some(members))?)? != Some(true) {
                continue;
            }
        }
        let values = exprs.iter().map(|e| evaluate(e, &ctx, Some(members))).collect::<Res<Vec<_>>>()?;
        if !s.order.is_empty() {
            let mut octx = ctx.clone();
            for (h, v) in headers.iter().zip(&values) {
                octx.insert(h.clone(), v.clone());
            }
            keys.push(s.order.iter().map(|(e, _)| evaluate(e, &octx, Some(members))).collect::<Res<Vec<_>>>()?);
        }
        out.push(values);
    }
    let descending: Vec<bool> = s.order.iter().map(|(_, d)| *d).collect();
    Ok(sort_rows(out, &keys, &descending))
}

fn run_select(db: &Database, s: Select) -> Res<Vec<String>> {
    let (names, mut rows) = match &s.table {
        None => (Vec::new(), vec![Row::new()]),
        Some(name) => {
            let table = db.table(name)?;
            (table.names(), table.rows.iter().map(|r| table.row(r)).collect())
        }
    };
    let mut exprs = Vec::new();
    let mut headers = Vec::new();
    for (e, alias) in &s.items {
        match e {
            None => {
                if s.table.is_none() {
                    return Err("SELECT * needs a table".to_string());
                }
                for n in &names {
                    exprs.push(Expr::Col(n.clone()));
                    headers.push(n.clone());
                }
            }
            Some(e) => {
                check_columns(e, &names)?;
                check_nesting(e)?;
                headers.push(alias.clone().unwrap_or_else(|| render(e)));
                exprs.push(e.clone());
            }
        }
    }
    for name in &s.group {
        if !names.contains(name) {
            return Err(format!("no such column {}", name));
        }
    }
    if let Some(h) = &s.having {
        if s.group.is_empty() {
            return Err("HAVING needs GROUP BY".to_string());
        }
        check_columns(h, &names)?;
        check_nesting(h)?;
    }
    if let Some(w) = &s.where_ {
        check_columns(w, &names)?;
        forbid_aggregates(w, "WHERE")?;
        let mut kept = Vec::new();
        for r in rows {
            if keeps(Some(w), &r)? {
                kept.push(r);
            }
        }
        rows = kept;
    }
    let visible: Vec<String> = names.iter().chain(&headers).cloned().collect();
    for (e, _) in &s.order {
        check_columns(e, &visible)?;
    }
    let mut out = if !s.group.is_empty() {
        grouped_query(&exprs, &headers, &s, rows)?
    } else if exprs.iter().any(|e| !aggregates_in(e).is_empty()) {
        aggregate_query(&exprs, &rows)?
    } else {
        plain_query(&exprs, &headers, &s.order, &rows)?
    };
    if let Some(n) = s.limit {
        out.truncate(n);
    }
    Ok(format_table(&headers, &out))
}

fn run_simple(db: &mut Database, what: &str) -> Res<Vec<String>> {
    match what {
        "show" => {
            let mut names: Vec<&String> = db.tables.keys().collect();
            names.sort();
            let rows: Vec<Vec<Value>> =
                names.iter().map(|n| vec![Value::Text(n.to_string()), Value::Int(db.tables[*n].rows.len() as i64)]).collect();
            return Ok(format_table(&["table".to_string(), "rows".to_string()], &rows));
        }
        "begin" => {
            if db.saved.is_some() {
                return Err("already in a transaction".to_string());
            }
            db.saved = Some(db.tables.clone());
        }
        _ => {
            let Some(saved) = db.saved.take() else {
                return Err("no transaction".to_string());
            };
            if what == "rollback" {
                db.tables = saved;
            }
        }
    }
    Ok(vec![what.to_uppercase()])
}

fn run_describe(db: &Database, name: &str) -> Res<Vec<String>> {
    let table = db.table(name)?;
    let rows: Vec<Vec<Value>> = table
        .columns
        .iter()
        .map(|c| {
            vec![
                Value::Text(c.name.clone()),
                Value::Text(c.ty.to_string()),
                Value::Text(if c.not_null { "NO" } else { "YES" }.to_string()),
                Value::Text(if c.primary { "PRI" } else { "" }.to_string()),
            ]
        })
        .collect();
    let headers: Vec<String> = ["column", "type", "null", "key"].iter().map(|h| h.to_string()).collect();
    Ok(format_table(&headers, &rows))
}

/// Carries out one statement; returns the lines to print.
fn execute(db: &mut Database, text: &str) -> Res<Vec<String>> {
    match parse_statement(tokenize(text)?)? {
        Stmt::Select(s) => run_select(db, s),
        Stmt::Insert { table, columns, rows } => run_insert(db, &table, columns, rows),
        Stmt::Update { table, assignments, where_ } => run_update(db, &table, assignments, where_),
        Stmt::Delete { table, where_ } => run_delete(db, &table, where_),
        Stmt::CreateTable { name, columns } => run_create(db, name, columns),
        Stmt::DropTable(name) => run_drop(db, &name),
        Stmt::AlterAdd { table, column } => run_alter(db, &table, column),
        Stmt::Describe(name) => run_describe(db, &name),
        Stmt::Simple(what) => run_simple(db, &what),
    }
}

fn main() {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input).unwrap();
    let mut db = Database { tables: HashMap::new(), saved: None };
    for text in split_statements(&input) {
        let lines = execute(&mut db, &text).unwrap_or_else(|e| vec![format!("error: {}", e)]);
        for line in lines {
            println!("{}", line);
        }
    }
}
