use std::collections::BTreeMap;
use std::io::{self, Read};

const OPS: &str = "+-*/%(),=";

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Num,
    Name,
    Op,
}

type Token = (Kind, String);
type Res<T> = Result<T, String>;

/// Numbers, names and one-character operators.
fn tokenize(line: &str) -> Res<Vec<Token>> {
    let cs: Vec<char> = line.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c == ' ' {
            i += 1;
        } else if c.is_ascii_digit() {
            let mut j = i;
            while j < cs.len() && cs[j].is_ascii_digit() {
                j += 1;
            }
            toks.push((Kind::Num, cs[i..j].iter().collect()));
            i = j;
        } else if c.is_alphabetic() || c == '_' {
            let mut j = i;
            while j < cs.len() && (cs[j].is_alphanumeric() || cs[j] == '_') {
                j += 1;
            }
            toks.push((Kind::Name, cs[i..j].iter().collect()));
            i = j;
        } else if OPS.contains(c) {
            toks.push((Kind::Op, c.to_string()));
            i += 1;
        } else {
            return Err(format!("unexpected character '{}'", c));
        }
    }
    Ok(toks)
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn new(toks: Vec<Token>) -> Self {
        Parser { toks, pos: 0 }
    }

    /// The next token's text, or "" at the end.
    fn peek(&self) -> &str {
        self.toks.get(self.pos).map_or("", |t| t.1.as_str())
    }

    fn take(&mut self) -> Res<Token> {
        if self.pos >= self.toks.len() {
            return Err("unexpected end".to_string());
        }
        self.pos += 1;
        Ok(self.toks[self.pos - 1].clone())
    }

    fn expect(&mut self, text: &str) -> Res<()> {
        let (_, t) = self.take()?;
        if t != text {
            return Err(format!("expected '{}' but found '{}'", text, t));
        }
        Ok(())
    }
}

enum Expr {
    Num(i64),
    Var(String),
    Neg(Box<Expr>),
    Bin(char, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
}

fn parse_sum(p: &mut Parser) -> Res<Expr> {
    let mut e = parse_product(p)?;
    while matches!(p.peek(), "+" | "-") {
        let op = p.take()?.1.chars().next().unwrap();
        e = Expr::Bin(op, Box::new(e), Box::new(parse_product(p)?));
    }
    Ok(e)
}

fn parse_product(p: &mut Parser) -> Res<Expr> {
    let mut e = parse_unary(p)?;
    while matches!(p.peek(), "*" | "/" | "%") {
        let op = p.take()?.1.chars().next().unwrap();
        e = Expr::Bin(op, Box::new(e), Box::new(parse_unary(p)?));
    }
    Ok(e)
}

fn parse_unary(p: &mut Parser) -> Res<Expr> {
    if p.peek() == "-" {
        p.take()?;
        return Ok(Expr::Neg(Box::new(parse_unary(p)?)));
    }
    parse_primary(p)
}

fn parse_primary(p: &mut Parser) -> Res<Expr> {
    let (kind, t) = p.take()?;
    match kind {
        Kind::Num => Ok(Expr::Num(t.parse().map_err(|_| format!("number too big: {}", t))?)),
        Kind::Name => {
            if p.peek() == "(" {
                p.take()?;
                return Ok(Expr::Call(t, parse_args(p)?));
            }
            Ok(Expr::Var(t))
        }
        Kind::Op if t == "(" => {
            let e = parse_sum(p)?;
            p.expect(")")?;
            Ok(e)
        }
        Kind::Op => Err(format!("unexpected '{}'", t)),
    }
}

/// A call's arguments, after its '(' and through its ')'.
fn parse_args(p: &mut Parser) -> Res<Vec<Expr>> {
    let mut args = Vec::new();
    if p.peek() == ")" {
        p.take()?;
        return Ok(args);
    }
    loop {
        args.push(parse_sum(p)?);
        let (_, t) = p.take()?;
        if t == ")" {
            return Ok(args);
        }
        if t != "," {
            return Err(format!("expected ',' or ')' but found '{}'", t));
        }
    }
}

fn parse_whole(p: &mut Parser) -> Res<Expr> {
    let e = parse_sum(p)?;
    if p.pos < p.toks.len() {
        return Err(format!("unexpected '{}'", p.peek()));
    }
    Ok(e)
}

type Env = BTreeMap<String, i64>;

fn evaluate(e: &Expr, env: &Env) -> Res<i64> {
    match e {
        Expr::Num(n) => Ok(*n),
        Expr::Var(name) => env.get(name).copied().ok_or_else(|| format!("undefined variable {}", name)),
        Expr::Neg(x) => Ok(-evaluate(x, env)?),
        Expr::Call(name, args) => {
            let vals = args.iter().map(|a| evaluate(a, env)).collect::<Res<Vec<i64>>>()?;
            call(name, &vals)
        }
        Expr::Bin(op, l, r) => {
            let a = evaluate(l, env)?;
            let b = evaluate(r, env)?;
            binary(*op, a, b)
        }
    }
}

fn binary(op: char, a: i64, b: i64) -> Res<i64> {
    match op {
        '+' => Ok(a + b),
        '-' => Ok(a - b),
        '*' => Ok(a * b),
        _ => {
            if b == 0 {
                return Err("division by zero".to_string());
            }
            // `/` and `%` truncate toward zero.
            Ok(if op == '/' { a / b } else { a % b })
        }
    }
}

fn need(name: &str, args: &[i64], n: usize) -> Res<()> {
    if args.len() != n {
        let s = if n == 1 { "" } else { "s" };
        return Err(format!("{} takes {} argument{}", name, n, s));
    }
    Ok(())
}

fn call(name: &str, args: &[i64]) -> Res<i64> {
    match name {
        "abs" => {
            need(name, args, 1)?;
            Ok(args[0].abs())
        }
        "min" | "max" => {
            if args.is_empty() {
                return Err(format!("{} needs at least one argument", name));
            }
            let it = args.iter().copied();
            Ok(if name == "min" { it.min() } else { it.max() }.unwrap())
        }
        "gcd" => {
            need(name, args, 2)?;
            let (mut a, mut b) = (args[0].abs(), args[1].abs());
            while b != 0 {
                (a, b) = (b, a % b);
            }
            Ok(a)
        }
        _ => Err(format!("unknown function {}", name)),
    }
}

/// Runs one line; returns the lines to print.
fn run_line(line: &str, env: &mut Env) -> Res<Vec<String>> {
    let toks = tokenize(line)?;
    if toks.is_empty() {
        return Ok(vec![]);
    }
    if toks[0].0 == Kind::Name && toks[0].1 == "let" {
        if toks.len() < 3 || toks[1].0 != Kind::Name || toks[2].1 != "=" {
            return Err("bad let".to_string());
        }
        let name = toks[1].1.clone();
        let value = evaluate(&parse_whole(&mut Parser::new(toks[3..].to_vec()))?, env)?;
        env.insert(name, value);
        return Ok(vec![]);
    }
    if toks.len() == 1 && toks[0].0 == Kind::Name && toks[0].1 == "vars" {
        return Ok(env.iter().map(|(k, v)| format!("{} = {}", k, v)).collect());
    }
    let e = parse_whole(&mut Parser::new(toks))?;
    Ok(vec![evaluate(&e, env)?.to_string()])
}

fn main() {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input).unwrap();
    let mut env = Env::new();
    for (i, line) in input.lines().enumerate() {
        let code = line.split('#').next().unwrap();
        match run_line(code, &mut env) {
            Ok(out) => {
                for s in out {
                    println!("{}", s);
                }
            }
            Err(e) => println!("line {}: {}", i + 1, e),
        }
    }
}
