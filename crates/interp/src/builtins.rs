//! The standard library: operators, methods on builtin types, in-place
//! mutators, conversions and the `fs`/`io`/`os`/`time` functions.

use std::cmp::Ordering;
use std::io::{BufRead, Read};
use std::rc::Rc;

use lacon_syntax::ast::BinOp;
use lacon_syntax::Span;

use crate::eval::{norm_index, panic, Ctrl, Interp, Panic, R};
use crate::ir::{Builtin, ConvTo, Program};
use crate::value::{self, RangeV, Value};

/// Every builtin method name, used for unknown-method errors and for calling
/// methods as functions (`len(xs)`).
pub const METHODS: &[&str] = &[
    // any value
    "str", "to_string", "unwrap", "expect", "is_none", "is_some", "is_err", "is_ok", "clone", "iter", "into_iter", "collect", "to_owned",
    "copied", "cloned", "as_str",
    // numbers
    "abs", "pow", "min", "max", "clamp", "sign", "chr", "sqrt", "cbrt", "floor", "ceil", "round", "trunc", "fract", "exp", "ln", "log",
    "log2", "log10", "sin", "cos", "tan", "asin", "acos", "atan", "atan2", "hypot", "is_nan", "is_finite",
    // strings
    "len", "is_empty", "chars", "bytes", "lines", "split", "split_once", "words", "trim", "trim_start", "trim_end", "starts_with",
    "ends_with", "contains", "contains_key", "find", "rfind", "replace", "upper", "lower", "repeat", "parse", "rev", "count", "is_digit",
    "is_alpha", "is_alnum", "is_space", "is_upper", "is_lower", "ord", "join", "strip_prefix", "strip_suffix", "pad_left", "pad_right",
    "capitalize",
    // collections
    "first", "last", "get", "index", "map", "filter", "position", "any", "all", "sum", "product", "min_by", "max_by", "sort",
    "sort_by", "unique", "enumerate", "zip", "flat_map", "flatten", "take", "skip", "take_while", "skip_while", "chunks", "windows",
    "group_by", "partition", "fold", "reduce", "each", "to_list", "to_set", "to_map", "step_by", "keys", "values", "items", "union",
    "intersection", "difference", "is_subset",
    // mutators
    "push", "pop", "insert", "remove", "clear", "extend", "add", "swap", "truncate", "retain",
];

pub const MUTATORS: &[&str] = &["push", "pop", "insert", "remove", "clear", "extend", "add", "swap", "truncate", "retain"];

pub fn is_method(n: &str) -> bool {
    METHODS.contains(&n)
}

pub fn is_mutator(n: &str) -> bool {
    MUTATORS.contains(&n)
}

/// Does argument `i` of builtin method `name` take a function? Those
/// arguments turn into lambdas when they mention `it`.
pub fn hof_arg(name: &str, i: usize) -> bool {
    match name {
        "map" | "filter" | "find" | "position" | "any" | "all" | "count" | "sort_by" | "min_by" | "max_by" | "flat_map" | "take_while"
        | "skip_while" | "group_by" | "partition" | "reduce" | "each" | "retain" => i == 0,
        "fold" => i == 1,
        _ => false,
    }
}

/// Lacon names for methods from other languages.
pub fn method_hint(n: &str) -> Option<&'static str> {
    Some(match n {
        "append" | "push_back" | "add_last" => "lists use `push`",
        "length" | "size" => "use `len`",
        "strip" => "use `trim`",
        "lstrip" | "trim_left" | "trimStart" => "use `trim_start`",
        "rstrip" | "trim_right" | "trimEnd" => "use `trim_end`",
        "to_uppercase" | "toUpperCase" | "uppercase" | "to_upper" => "use `upper`",
        "to_lowercase" | "toLowerCase" | "lowercase" | "to_lower" => "use `lower`",
        "startswith" | "startsWith" => "use `starts_with`",
        "endswith" | "endsWith" => "use `ends_with`",
        "includes" | "has" | "has_key" => "use `contains`",
        "indexOf" | "index_of" | "find_index" | "findIndex" => "use `index(x)` or `position(pred)`",
        "sorted" => "use `sort()` (it returns a new list)",
        "reverse" | "reversed" => "use `rev()` (it returns a new value): `xs = xs.rev()`",
        "push_str" => "strings are values: `s += \"...\"`",
        "to_vec" | "to_array" | "toList" | "tolist" => "use `to_list`",
        "for_each" | "forEach" => "use `each`, or a `for` loop",
        "substring" | "substr" | "slice" => "slice with `s[a..b]` or `s[a:b]`",
        "char_at" | "charAt" | "nth" | "at" => "index with `s[i]`",
        "distinct" | "dedup" | "uniq" => "use `unique`",
        "sum_by" | "sumOf" => "use `.map(...).sum()`",
        "entry" | "setdefault" | "get_or_insert" | "or_insert" => "`m[k] += v` and `m[k].push(x)` create missing entries",
        "unwrap_or" | "or_else" | "unwrap_or_else" | "getOrElse" | "orElse" => "use `x ?? default`",
        "unwrap_or_default" => "use `x ?? 0` (or the right zero value)",
        "format" | "toFixed" | "to_fixed" => "interpolate with a spec: \"{x:.2}\"",
        "splitlines" => "use `lines`",
        "split_whitespace" => "use `split()` with no argument, or `words`",
        "isdigit" | "is_numeric" | "isnumeric" | "is_ascii_digit" => "use `is_digit`",
        "isalpha" | "is_alphabetic" | "is_ascii_alphabetic" => "use `is_alpha`",
        "isalnum" | "is_alphanumeric" | "is_ascii_alphanumeric" => "use `is_alnum`",
        "isspace" | "is_whitespace" | "is_ascii_whitespace" => "use `is_space`",
        "isupper" | "is_uppercase" => "use `is_upper`",
        "islower" | "is_lowercase" => "use `is_lower`",
        "concat" => "concatenate with `+`",
        "flat" => "use `flatten`",
        "sortBy" | "sorted_by" | "sort_by_key" | "sortedBy" => "use `sort_by(key)` (returns a new list)",
        "maxBy" | "max_by_key" | "maxOf" => "use `max_by(key)`",
        "minBy" | "min_by_key" | "minOf" => "use `min_by(key)`",
        "parse_int" | "parseInt" | "to_int" | "toInt" | "as_int" | "atoi" => "use `int(x)` or `s.parse()`",
        "parse_float" | "parseFloat" | "to_float" | "toFloat" | "to_f64" | "as_f64" => "use `f64(x)` or `s.parse()`",
        "toString" | "to_str" => "use `str(x)` or interpolation",
        "removeAt" | "remove_at" | "del" => "use `remove(i)`",
        "keySet" => "use `keys`",
        "entries" | "iteritems" => "use `items`, or `for k, v in m`",
        "discard" => "use `remove`",
        "update" => "use `extend`",
        "abs_diff" => "use `(a - b).abs()`",
        "powi" | "powf" => "use `pow`",
        "to_ascii_uppercase" => "use `upper`",
        "to_ascii_lowercase" => "use `lower`",
        "code" | "char_code" | "charCodeAt" | "codePointAt" => "use `ord`",
        "fromCharCode" | "from_u32" | "from_char_code" => "use `n.chr()`",
        _ => return None,
    })
}

// ----- operators -----

pub fn arith(p: &Program, op: BinOp, a: Value, b: Value) -> Result<Value, (&'static str, String)> {
    use Value::*;
    let overflow = || ("E0405", format!("integer overflow in `{}`", op.symbol()));
    Ok(match (op, a, b) {
        (_, Err(e), _) | (_, _, Err(e)) => return Result::Err(("E0406", format!("operating on an error ({}); add `?`", value::display(&e, p)))),
        (BinOp::Add, Int(x), Int(y)) => Int(x.checked_add(y).ok_or_else(overflow)?),
        (BinOp::Sub, Int(x), Int(y)) => Int(x.checked_sub(y).ok_or_else(overflow)?),
        (BinOp::Mul, Int(x), Int(y)) => Int(x.checked_mul(y).ok_or_else(overflow)?),
        (BinOp::Div | BinOp::Rem, Int(_), Int(0)) => return Result::Err(("E0404", "division by zero".into())),
        (BinOp::Div, Int(x), Int(y)) => Int(x.checked_div(y).ok_or_else(overflow)?),
        (BinOp::Rem, Int(x), Int(y)) => Int(x.checked_rem(y).ok_or_else(overflow)?),
        (BinOp::Pow, Int(x), Int(y)) => {
            if y < 0 {
                Float((x as f64).powf(y as f64))
            } else {
                Int(u32::try_from(y).ok().and_then(|y| x.checked_pow(y)).ok_or_else(overflow)?)
            }
        }
        (BinOp::BitAnd, Int(x), Int(y)) => Int(x & y),
        (BinOp::BitOr, Int(x), Int(y)) => Int(x | y),
        (BinOp::BitXor, Int(x), Int(y)) => Int(x ^ y),
        (BinOp::Shl, Int(x), Int(y)) => Int(u32::try_from(y).ok().filter(|&s| s < 64).map(|s| x << s).ok_or_else(overflow)?),
        (BinOp::Shr, Int(x), Int(y)) => Int(u32::try_from(y).ok().filter(|&s| s < 64).map(|s| x >> s).ok_or_else(overflow)?),
        (BinOp::BitAnd, Bool(x), Bool(y)) => Bool(x & y),
        (BinOp::BitOr, Bool(x), Bool(y)) => Bool(x | y),
        (BinOp::BitXor, Bool(x), Bool(y)) => Bool(x ^ y),
        (op, x @ (Int(_) | Float(_)), y @ (Int(_) | Float(_))) => {
            let (x, y) = (as_f64(&x), as_f64(&y));
            Float(match op {
                BinOp::Add => x + y,
                BinOp::Sub => x - y,
                BinOp::Mul => x * y,
                BinOp::Div => x / y,
                BinOp::Rem => x % y,
                BinOp::Pow => x.powf(y),
                _ => return Result::Err(("E0301", format!("`{}` needs ints, got f64", op.symbol()))),
            })
        }
        (BinOp::Add, Str(mut x), Str(y)) => {
            Rc::make_mut(&mut x).push_str(&y);
            Str(x)
        }
        (BinOp::Mul, Str(x), Int(n)) | (BinOp::Mul, Int(n), Str(x)) => Value::str(x.repeat(n.max(0) as usize)),
        (BinOp::Add, List(mut x), List(y)) => {
            Rc::make_mut(&mut x).extend(y.iter().cloned());
            List(x)
        }
        (BinOp::Add, List(mut x), Range(r)) => {
            Rc::make_mut(&mut x).extend((0..r.len()).map(|i| Int(r.get(i))));
            List(x)
        }
        (BinOp::Mul, List(x), Int(n)) | (BinOp::Mul, Int(n), List(x)) => {
            let mut out = Vec::with_capacity(x.len() * n.max(0) as usize);
            for _ in 0..n.max(0) {
                out.extend(x.iter().cloned());
            }
            Value::list(out)
        }
        (BinOp::BitOr, Set(mut x), Set(y)) => {
            Rc::make_mut(&mut x).extend(y.iter().cloned());
            Set(x)
        }
        (BinOp::BitAnd, Set(x), Set(y)) => Set(Rc::new(x.iter().filter(|v| y.contains(*v)).cloned().collect())),
        (BinOp::Sub, Set(x), Set(y)) => Set(Rc::new(x.iter().filter(|v| !y.contains(*v)).cloned().collect())),
        (BinOp::BitXor, Set(x), Set(y)) => {
            Set(Rc::new(x.iter().filter(|v| !y.contains(*v)).chain(y.iter().filter(|v| !x.contains(*v))).cloned().collect()))
        }
        (BinOp::BitOr, Map(mut x), Map(y)) => {
            let m = Rc::make_mut(&mut x);
            for (k, v) in y.iter() {
                m.insert(k.clone(), v.clone());
            }
            Map(x)
        }
        (_, None, _) | (_, _, None) => return Result::Err(("E0407", format!("`{}` on none; check for `none` first or use `x ?? default`", op.symbol()))),
        (BinOp::Add, Str(_), y) | (BinOp::Add, y, Str(_)) => {
            return Result::Err(("E0301", format!("cannot add str and {}; interpolate instead: \"{{a}}{{b}}\"", y.kind(p))))
        }
        (op, x, y) => return Result::Err(("E0301", format!("cannot apply `{}` to {} and {}", op.symbol(), x.kind(p), y.kind(p)))),
    })
}

fn as_f64(v: &Value) -> f64 {
    match v {
        Value::Int(n) => *n as f64,
        Value::Float(f) => *f,
        _ => f64::NAN,
    }
}

pub fn contains(it: &Interp, container: &Value, x: &Value, span: Span) -> R<bool> {
    Ok(match container {
        Value::List(xs) | Value::Tuple(xs) => xs.iter().any(|y| value::eq(y, x).unwrap_or(false)),
        Value::Set(s) => s.contains(x) || (matches!(x, Value::Float(_)) && s.iter().any(|y| value::eq(y, x).unwrap_or(false))),
        Value::Map(m) => m.contains_key(x),
        Value::Range(r) => match x {
            Value::Int(n) => r.contains(*n),
            _ => false,
        },
        Value::Str(s) => match x {
            Value::Str(sub) => s.contains(sub.as_str()),
            other => return Err(panic("E0301", span, format!("`in` on a string needs a str, got {}", it.kind(other)))),
        },
        Value::Err(e) => return Err(panic("E0406", span, format!("`in` on an error ({}); add `?`", it.display(e)))),
        other => return Err(panic("E0301", span, format!("cannot use `in` with {}", it.kind(other)))),
    })
}

/// The elements a value iterates over: list items, set members, map
/// `(k, v)` pairs, range numbers, string characters.
pub fn iter_values(v: &Value) -> Result<Vec<Value>, ()> {
    Ok(match v {
        Value::List(xs) | Value::Tuple(xs) => xs.to_vec(),
        Value::Set(s) => s.iter().cloned().collect(),
        Value::Map(m) => m.iter().map(|(k, v)| Value::tuple(vec![k.clone(), v.clone()])).collect(),
        Value::Range(r) => (0..r.len()).map(|i| Value::Int(r.get(i))).collect(),
        Value::Str(s) => s.chars().map(|c| Value::str(c.to_string())).collect(),
        _ => return Err(()),
    })
}

fn items(it: &Interp, v: &Value, span: Span) -> R<Vec<Value>> {
    iter_values(v).map_err(|_| panic("E0301", span, format!("cannot iterate over {}", it.kind(v))))
}

pub fn slice(it: &Interp, o: &Value, start: Option<i64>, end: Option<i64>, inclusive: bool, span: Span) -> R<Value> {
    let len = match o {
        Value::List(xs) | Value::Tuple(xs) => xs.len(),
        Value::Str(s) => {
            if s.is_ascii() {
                s.len()
            } else {
                s.chars().count()
            }
        }
        Value::Range(r) => r.len(),
        Value::Err(e) => return Err(panic("E0406", span, format!("slicing an error ({}); add `?`", it.display(e)))),
        other => return Err(panic("E0301", span, format!("cannot slice {}", it.kind(other)))),
    } as i64;
    let fix = |i: i64| if i < 0 { (len + i).max(0) } else { i.min(len) };
    let a = start.map_or(0, fix);
    let mut b = end.map_or(len, |e| if inclusive { fix(e).saturating_add(1).min(len) } else { fix(e) });
    if b < a {
        b = a;
    }
    let (a, b) = (a as usize, b as usize);
    Ok(match o {
        Value::List(xs) => Value::list(xs[a..b].to_vec()),
        Value::Tuple(xs) => Value::tuple(xs[a..b].to_vec()),
        Value::Str(s) => {
            if s.is_ascii() {
                Value::str(&s[a..b])
            } else {
                Value::str(s.chars().skip(a).take(b - a).collect::<String>())
            }
        }
        Value::Range(r) => Value::Range(Rc::new(RangeV { start: r.get(a), end: r.get(a) + (b - a) as i64 * r.step, step: r.step })),
        _ => unreachable!(),
    })
}

// ----- conversions -----

pub fn parse_number(s: &str) -> Option<Value> {
    let t = s.trim();
    if let Ok(n) = t.parse::<i64>() {
        return Some(Value::Int(n));
    }
    if let Ok(n) = t.replace('_', "").parse::<i64>() {
        if !t.starts_with('_') && !t.ends_with('_') {
            return Some(Value::Int(n));
        }
    }
    if let Ok(f) = t.parse::<f64>() {
        if !t.is_empty() && !t.eq_ignore_ascii_case("nan") && !t.to_lowercase().contains("inf") {
            return Some(Value::Float(f));
        }
    }
    None
}

fn parse_err(s: &str, what: &str) -> Value {
    Value::Err(Rc::new(Value::str(format!("cannot parse {s:?} as {what}"))))
}

/// `int(x)`, `f64(x)`, `str(x)`, `bool(x)` and `x as T`. Parsing a string
/// that isn't a number gives an error value, so `int(s)?` and
/// `int(s) ?? 0` work.
pub fn convert(it: &Interp, v: Value, to: ConvTo, span: Span) -> R<Value> {
    if let Value::Err(e) = &v {
        return Err(panic("E0406", span, format!("converting an error ({}); add `?`", it.display(e))));
    }
    Ok(match to {
        ConvTo::Int(name, lo, hi) => {
            let n = match &v {
                Value::Int(n) => *n,
                Value::Float(f) => {
                    if !f.is_finite() || *f >= 9.3e18 || *f <= -9.3e18 {
                        return Err(panic("E0405", span, format!("{} does not fit in {name}", value::fmt_float(*f))));
                    }
                    f.trunc() as i64
                }
                Value::Bool(b) => *b as i64,
                Value::Str(s) => match s.trim().parse::<i64>() {
                    Ok(n) => n,
                    Err(_) => match s.trim().parse::<f64>() {
                        Ok(f) if f.fract() == 0.0 && f.abs() < 9e18 && !s.trim().is_empty() => f as i64,
                        _ => return Ok(parse_err(s, name)),
                    },
                },
                other => return Err(panic("E0301", span, format!("cannot convert {} to {name}", it.kind(other)))),
            };
            if n < lo || n > hi {
                return Err(panic("E0405", span, format!("{n} does not fit in {name}")));
            }
            Value::Int(n)
        }
        ConvTo::Float => match &v {
            Value::Int(n) => Value::Float(*n as f64),
            Value::Float(_) => v,
            Value::Bool(b) => Value::Float(*b as i64 as f64),
            Value::Str(s) => match s.trim().parse::<f64>() {
                Ok(f) if !s.trim().is_empty() => Value::Float(f),
                _ => parse_err(s, "f64"),
            },
            other => return Err(panic("E0301", span, format!("cannot convert {} to f64", it.kind(other)))),
        },
        ConvTo::Str => Value::str(it.display(&v)),
        ConvTo::Bool => match &v {
            Value::Bool(_) => v,
            Value::Int(n) => Value::Bool(*n != 0),
            Value::Str(s) => match s.trim() {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                _ => parse_err(s, "bool"),
            },
            other => return Err(panic("E0301", span, format!("cannot convert {} to bool", it.kind(other)))),
        },
    })
}

// ----- namespaced functions -----

fn str_arg<'a>(it: &Interp, args: &'a [Value], i: usize, what: &str, span: Span) -> R<&'a str> {
    match args.get(i) {
        Some(Value::Str(s)) => Ok(s.as_str()),
        Some(other) => Err(panic("E0301", span, format!("{what}: want str got {}", it.kind(other)))),
        None => Err(panic("E0206", span, format!("{what}: missing argument"))),
    }
}

fn io_err(what: &str, path: &str, e: std::io::Error) -> Value {
    let reason = match e.kind() {
        std::io::ErrorKind::NotFound => "no such file".to_string(),
        std::io::ErrorKind::PermissionDenied => "permission denied".to_string(),
        _ => e.to_string(),
    };
    Value::Err(Rc::new(Value::str(format!("cannot {what} {path}: {reason}"))))
}

fn with_stdin<T>(it: &Interp, f: impl FnOnce(&mut std::io::StdinLock<'static>) -> T) -> T {
    it.out.borrow_mut().flush();
    let mut slot = it.stdin.borrow_mut();
    let lock = slot.get_or_insert_with(|| std::io::stdin().lock());
    f(lock)
}

pub fn call_builtin(it: &Interp, b: Builtin, args: Vec<Value>, span: Span) -> R<Value> {
    Ok(match b {
        Builtin::Print | Builtin::Eprint => {
            let mut s = String::new();
            for (i, a) in args.iter().enumerate() {
                if let Value::Err(e) = a {
                    return Err(panic("E0406", span, format!("printing an error ({}); add `?`", it.display(e))));
                }
                if i > 0 {
                    s.push(' ');
                }
                s.push_str(&it.display(a));
            }
            s.push('\n');
            if b == Builtin::Print {
                it.print(&s);
            } else {
                it.out.borrow_mut().flush();
                eprint!("{s}");
            }
            Value::Unit
        }
        Builtin::Range => {
            let mut ns = Vec::new();
            for a in &args {
                ns.push(it.int_of(a, span)?);
            }
            let (start, end, step) = match ns.as_slice() {
                [e] => (0, *e, 1),
                [s, e] => (*s, *e, 1),
                [s, e, st] => (*s, *e, *st),
                _ => return Err(panic("E0206", span, "range takes 1 to 3 arguments")),
            };
            if step == 0 {
                return Err(panic("E0301", span, "range step must not be 0"));
            }
            Value::Range(Rc::new(RangeV { start, end, step }))
        }
        Builtin::Min | Builtin::Max => {
            let mut best = args[0].clone();
            for a in &args[1..] {
                let o = value::cmp(a, &best).map_err(|_| panic("E0301", span, format!("cannot compare {} with {}", it.kind(a), it.kind(&best))))?;
                if (b == Builtin::Min && o.is_lt()) || (b == Builtin::Max && o.is_gt()) {
                    best = a.clone();
                }
            }
            best
        }
        Builtin::SetNew => match args.first() {
            None => Value::Set(Rc::new(value::Set::default())),
            Some(v) => Value::Set(Rc::new(items(it, v, span)?.into_iter().collect())),
        },
        Builtin::ListNew => match args.first() {
            None => Value::list(vec![]),
            Some(v) => Value::list(items(it, v, span)?),
        },
        Builtin::Conv(to) => return convert(it, args.into_iter().next().unwrap_or(Value::Unit), to, span),
        Builtin::Panic => return Err(panic("E0400", span, format!("panic: {}", it.display(&args[0])))),
        Builtin::FsRead => {
            let p = str_arg(it, &args, 0, "fs.read", span)?;
            match std::fs::read_to_string(p) {
                Ok(s) => Value::str(s),
                Err(e) => io_err("read", p, e),
            }
        }
        Builtin::FsLines => {
            let p = str_arg(it, &args, 0, "fs.lines", span)?;
            match std::fs::read_to_string(p) {
                Ok(s) => Value::list(s.lines().map(Value::str).collect()),
                Err(e) => io_err("read", p, e),
            }
        }
        Builtin::FsWrite | Builtin::FsAppend => {
            let p = str_arg(it, &args, 0, "fs.write", span)?;
            let content = it.display(&args[1]);
            let r = if b == Builtin::FsWrite {
                std::fs::write(p, content)
            } else {
                use std::io::Write;
                std::fs::OpenOptions::new().append(true).create(true).open(p).and_then(|mut f| f.write_all(content.as_bytes()))
            };
            match r {
                Ok(()) => Value::Unit,
                Err(e) => io_err("write", p, e),
            }
        }
        Builtin::FsExists => Value::Bool(std::path::Path::new(str_arg(it, &args, 0, "fs.exists", span)?).exists()),
        Builtin::FsRemove => {
            let p = str_arg(it, &args, 0, "fs.remove", span)?;
            match std::fs::remove_file(p) {
                Ok(()) => Value::Unit,
                Err(e) => io_err("remove", p, e),
            }
        }
        Builtin::IoRead => {
            let mut s = String::new();
            let _ = with_stdin(it, |l| l.read_to_string(&mut s));
            Value::str(s)
        }
        Builtin::IoLines => {
            let mut s = String::new();
            let _ = with_stdin(it, |l| l.read_to_string(&mut s));
            Value::list(s.lines().map(Value::str).collect())
        }
        Builtin::IoReadLine => {
            let mut s = String::new();
            let n = with_stdin(it, |l| l.read_line(&mut s)).unwrap_or(0);
            if n == 0 {
                Value::None
            } else {
                if s.ends_with('\n') {
                    s.pop();
                    if s.ends_with('\r') {
                        s.pop();
                    }
                }
                Value::str(s)
            }
        }
        Builtin::IoWrite => {
            let s = it.display(&args[0]);
            it.print(&s);
            Value::Unit
        }
        Builtin::OsArgs => Value::list(it.args.iter().map(|a| Value::str(a.as_str())).collect()),
        Builtin::OsEnv => match std::env::var(str_arg(it, &args, 0, "os.env", span)?) {
            Ok(v) => Value::str(v),
            Err(_) => Value::None,
        },
        Builtin::OsExit => {
            let code = it.int_of(&args[0], span)?;
            return Err(Ctrl::Panic(Box::new(Panic { code: "", msg: String::new(), span, detail: vec![], exit: Some(code as i32), trace: vec![] })));
        }
        Builtin::TimeNow => Value::Float(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)),
    })
}

// ----- methods -----

fn arg<'a>(args: &'a [Value], i: usize, name: &str, span: Span) -> R<&'a Value> {
    args.get(i).ok_or_else(|| panic("E0206", span, format!("`{name}` needs {} argument(s)", i + 1)))
}

fn int_arg(it: &Interp, args: &[Value], i: usize, name: &str, span: Span) -> R<i64> {
    let v = arg(args, i, name, span)?;
    it.int_of(v, span).map_err(|_| panic("E0301", span, format!("`{name}` wants an int argument, got {}", it.kind(v))))
}

fn sarg<'a>(it: &Interp, args: &'a [Value], i: usize, name: &str, span: Span) -> R<&'a str> {
    match arg(args, i, name, span)? {
        Value::Str(s) => Ok(s.as_str()),
        other => Err(panic("E0301", span, format!("`{name}` wants a str argument, got {}", it.kind(other)))),
    }
}

fn pred(it: &Interp, f: &Value, x: Value, span: Span) -> R<bool> {
    let r = it.call_value(f, vec![x], span)?;
    match r {
        Value::Bool(b) => Ok(b),
        other => Err(panic("E0301", span, format!("predicate must return bool, got {}", it.kind(&other)))),
    }
}

fn opt(v: Option<Value>) -> Value {
    v.unwrap_or(Value::None)
}

fn sort_values(it: &Interp, v: &mut [Value], span: Span) -> R<()> {
    let mut bad = None;
    v.sort_by(|a, b| match value::cmp(a, b) {
        Ok(o) => o,
        Err(()) => {
            if bad.is_none() {
                bad = Some((it.kind(a), it.kind(b)));
            }
            Ordering::Equal
        }
    });
    match bad {
        Some((a, b)) => Err(panic("E0301", span, format!("cannot sort: {a} and {b} are not comparable"))),
        None => Ok(()),
    }
}

fn sort_by_key(it: &Interp, xs: Vec<Value>, f: &Value, span: Span) -> R<Vec<Value>> {
    let mut keyed = Vec::with_capacity(xs.len());
    for x in xs {
        keyed.push((it.call_value(f, vec![x.clone()], span)?, x));
    }
    let mut bad = None;
    keyed.sort_by(|a, b| match value::cmp(&a.0, &b.0) {
        Ok(o) => o,
        Err(()) => {
            if bad.is_none() {
                bad = Some((it.kind(&a.0), it.kind(&b.0)));
            }
            Ordering::Equal
        }
    });
    if let Some((a, b)) = bad {
        return Err(panic("E0301", span, format!("cannot sort: keys {a} and {b} are not comparable")));
    }
    Ok(keyed.into_iter().map(|(_, x)| x).collect())
}

fn ext_by(it: &Interp, xs: Vec<Value>, f: Option<&Value>, want: Ordering, span: Span) -> R<Value> {
    let mut best: Option<(Value, Value)> = None;
    for x in xs {
        let k = match f {
            Some(f) => it.call_value(f, vec![x.clone()], span)?,
            None => x.clone(),
        };
        let better = match &best {
            None => true,
            Some((bk, _)) => value::cmp(&k, bk).map_err(|_| panic("E0301", span, format!("cannot compare {} with {}", it.kind(&k), it.kind(bk))))? == want,
        };
        if better {
            best = Some((k, x));
        }
    }
    Ok(opt(best.map(|b| b.1)))
}

fn flatten_into(out: &mut Vec<Value>, v: Value) {
    match v {
        Value::List(xs) | Value::Tuple(xs) => out.extend(xs.iter().cloned()),
        Value::Set(s) => out.extend(s.iter().cloned()),
        Value::Range(r) => out.extend((0..r.len()).map(|i| Value::Int(r.get(i)))),
        Value::None => {}
        other => out.push(other),
    }
}

pub fn method(it: &Interp, recv: Value, name: &str, args: Vec<Value>, span: Span) -> R<Value> {
    if let Value::Err(e) = &recv {
        return match name {
            "is_err" => Ok(Value::Bool(true)),
            "is_ok" | "is_none" => Ok(Value::Bool(false)),
            "is_some" => Ok(Value::Bool(true)),
            "expect" => Err(panic("E0407", span, format!("{}: {}", it.display(args.first().unwrap_or(&Value::str(""))), it.display(e)))),
            "unwrap" => Err(panic("E0407", span, format!("unwrap on an error: {}", it.display(e)))),
            _ => Err(panic("E0406", span, format!("calling `{name}` on an error ({}); add `?`", it.display(e)))),
        };
    }
    match name {
        "is_none" => return Ok(Value::Bool(matches!(recv, Value::None))),
        "is_some" => return Ok(Value::Bool(!matches!(recv, Value::None))),
        "is_err" => return Ok(Value::Bool(false)),
        "is_ok" => return Ok(Value::Bool(true)),
        "unwrap" | "expect" => {
            if matches!(recv, Value::None) {
                let msg = if name == "expect" { it.display(args.first().unwrap_or(&Value::str("expect"))) } else { "unwrap on none".into() };
                return Err(panic("E0407", span, msg));
            }
            return Ok(recv);
        }
        "str" | "to_string" => return Ok(Value::str(it.display(&recv))),
        "clone" | "iter" | "into_iter" | "to_owned" | "copied" | "cloned" | "as_str" => return Ok(recv),
        "collect" => {
            return Ok(match recv {
                Value::Range(_) | Value::Set(_) => Value::list(items(it, &recv, span)?),
                v => v,
            })
        }
        _ => {}
    }
    if matches!(recv, Value::None) {
        return Err(panic("E0407", span, format!("calling `{name}` on none; check `x != none` first or use `x ?? default`")));
    }
    if let Value::Int(_) | Value::Float(_) = recv {
        return num_method(it, recv, name, &args, span);
    }
    if let Value::Str(s) = &recv {
        if let Some(v) = str_method(it, s, name, &args, span)? {
            return Ok(v);
        }
    }
    match &recv {
        Value::List(xs) => {
            if let Some(v) = list_method(it, xs, name, &args, span)? {
                return Ok(v);
            }
        }
        Value::Map(m) => match name {
            "len" => return Ok(Value::Int(m.len() as i64)),
            "is_empty" => return Ok(Value::Bool(m.is_empty())),
            "keys" => return Ok(Value::list(m.keys().cloned().collect())),
            "values" => return Ok(Value::list(m.values().cloned().collect())),
            "items" | "to_list" => return Ok(Value::list(iter_values(&recv).unwrap())),
            "get" => {
                let k = arg(&args, 0, name, span)?;
                return Ok(m.get(k).cloned().unwrap_or_else(|| args.get(1).cloned().unwrap_or(Value::None)));
            }
            "contains" | "contains_key" => return Ok(Value::Bool(m.contains_key(arg(&args, 0, name, span)?))),
            "to_map" => return Ok(recv.clone()),
            _ => {}
        },
        Value::Set(s) => match name {
            "len" => return Ok(Value::Int(s.len() as i64)),
            "is_empty" => return Ok(Value::Bool(s.is_empty())),
            "contains" => return contains(it, &recv, arg(&args, 0, name, span)?, span).map(Value::Bool),
            "to_set" => return Ok(recv.clone()),
            "union" | "intersection" | "difference" => {
                let other = Value::Set(Rc::new(items(it, arg(&args, 0, name, span)?, span)?.into_iter().collect()));
                let op = match name {
                    "union" => BinOp::BitOr,
                    "intersection" => BinOp::BitAnd,
                    _ => BinOp::Sub,
                };
                return arith(it.prog, op, recv.clone(), other).map_err(|(c, m)| panic(c, span, m));
            }
            "is_subset" => {
                let other = arg(&args, 0, name, span)?;
                let mut ok = true;
                for x in s.iter() {
                    if !contains(it, other, x, span)? {
                        ok = false;
                        break;
                    }
                }
                return Ok(Value::Bool(ok));
            }
            _ => {}
        },
        Value::Range(r) => match name {
            "len" => return Ok(Value::Int(r.len() as i64)),
            "is_empty" => return Ok(Value::Bool(r.len() == 0)),
            "contains" => return contains(it, &recv, arg(&args, 0, name, span)?, span).map(Value::Bool),
            "rev" => {
                let n = r.len();
                if n == 0 {
                    return Ok(Value::Range(Rc::new(RangeV { start: 0, end: 0, step: 1 })));
                }
                let last = r.get(n - 1);
                return Ok(Value::Range(Rc::new(RangeV { start: last, end: r.start - r.step.signum(), step: -r.step })));
            }
            "step_by" => {
                let k = int_arg(it, &args, 0, name, span)?;
                if k <= 0 {
                    return Err(panic("E0301", span, "step_by needs a positive step"));
                }
                return Ok(Value::Range(Rc::new(RangeV { start: r.start, end: r.end, step: r.step * k })));
            }
            "sum" if r.step == 1 => {
                let n = r.len() as i128;
                let total = n * (r.start as i128 * 2 + n - 1) / 2;
                return i64::try_from(total).map(Value::Int).map_err(|_| panic("E0405", span, "integer overflow in `sum`"));
            }
            _ => {}
        },
        Value::Tuple(xs) => {
            if name == "len" {
                return Ok(Value::Int(xs.len() as i64));
            }
        }
        Value::Struct(_) | Value::Variant(_) | Value::Bool(_) | Value::Func(_) | Value::Unit => {
            return Err(panic("E0203", span, format!("{} has no method `{name}`", it.kind(&recv))));
        }
        _ => {}
    }
    iter_method(it, recv, name, args, span)
}

fn num_method(it: &Interp, recv: Value, name: &str, args: &[Value], span: Span) -> R<Value> {
    if let Value::Int(n) = recv {
        let r = match name {
            "abs" => Some(Value::Int(n.checked_abs().ok_or_else(|| panic("E0405", span, "integer overflow in `abs`"))?)),
            "sign" => Some(Value::Int(n.signum())),
            "pow" => {
                let e = arg(args, 0, name, span)?.clone();
                return it.binop(BinOp::Pow, Value::Int(n), e, span);
            }
            "chr" => {
                let c = u32::try_from(n).ok().and_then(char::from_u32).ok_or_else(|| panic("E0301", span, format!("{n} is not a character code")))?;
                Some(Value::str(c.to_string()))
            }
            "floor" | "ceil" | "round" | "trunc" => Some(Value::Int(n)),
            "min" | "max" | "clamp" => None,
            _ => {
                if matches!(name, "sqrt" | "cbrt" | "exp" | "ln" | "log" | "log2" | "log10" | "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "atan2" | "hypot" | "fract" | "is_nan" | "is_finite") {
                    return num_method(it, Value::Float(n as f64), name, args, span);
                }
                return Err(panic("E0203", span, format!("int has no method `{name}`")));
            }
        };
        if let Some(v) = r {
            return Ok(v);
        }
    }
    let x = as_f64(&recv);
    let num = |i: usize| -> R<f64> {
        match arg(args, i, name, span)? {
            Value::Int(n) => Ok(*n as f64),
            Value::Float(f) => Ok(*f),
            other => Err(panic("E0301", span, format!("`{name}` wants a number, got {}", it.kind(other)))),
        }
    };
    let both_int = |v: &Value| matches!(recv, Value::Int(_)) && matches!(v, Value::Int(_));
    Ok(Value::Float(match name {
        "min" | "max" => {
            let o = arg(args, 0, name, span)?;
            let c = value::cmp(&recv, o).map_err(|_| panic("E0301", span, format!("`{name}` wants a number, got {}", it.kind(o))))?;
            let pick_recv = if name == "min" { c.is_le() } else { c.is_ge() };
            let v = if pick_recv { recv.clone() } else { o.clone() };
            return Ok(if both_int(o) { v } else { Value::Float(as_f64(&v)) });
        }
        "clamp" => {
            let lo = arg(args, 0, name, span)?;
            let hi = arg(args, 1, name, span)?;
            let v = if value::cmp(&recv, lo) == Ok(Ordering::Less) {
                lo.clone()
            } else if value::cmp(&recv, hi) == Ok(Ordering::Greater) {
                hi.clone()
            } else {
                recv.clone()
            };
            return Ok(if both_int(lo) && both_int(hi) { v } else { Value::Float(as_f64(&v)) });
        }
        "abs" => x.abs(),
        "sign" => x.signum(),
        "sqrt" => x.sqrt(),
        "cbrt" => x.cbrt(),
        "floor" => x.floor(),
        "ceil" => x.ceil(),
        "round" => match args.first() {
            Some(d) => {
                let d = it.int_of(d, span)?;
                let m = 10f64.powi(d as i32);
                (x * m).round() / m
            }
            None => x.round(),
        },
        "trunc" => x.trunc(),
        "fract" => x.fract(),
        "pow" => x.powf(num(0)?),
        "exp" => x.exp(),
        "ln" => x.ln(),
        "log" => match args.first() {
            Some(_) => x.log(num(0)?),
            None => x.ln(),
        },
        "log2" => x.log2(),
        "log10" => x.log10(),
        "sin" => x.sin(),
        "cos" => x.cos(),
        "tan" => x.tan(),
        "asin" => x.asin(),
        "acos" => x.acos(),
        "atan" => x.atan(),
        "atan2" => x.atan2(num(0)?),
        "hypot" => x.hypot(num(0)?),
        "is_nan" => return Ok(Value::Bool(x.is_nan())),
        "is_finite" => return Ok(Value::Bool(x.is_finite())),
        _ => return Err(panic("E0203", span, format!("{} has no method `{name}`", it.kind(&recv)))),
    }))
}

fn char_class(s: &str, f: fn(char) -> bool) -> Value {
    Value::Bool(!s.is_empty() && s.chars().all(f))
}

fn char_index(s: &str, byte: usize) -> i64 {
    s[..byte].chars().count() as i64
}

fn str_method(it: &Interp, s: &Rc<String>, name: &str, args: &[Value], span: Span) -> R<Option<Value>> {
    let s: &str = s.as_str();
    Ok(Some(match name {
        "len" => Value::Int(if s.is_ascii() { s.len() } else { s.chars().count() } as i64),
        "is_empty" => Value::Bool(s.is_empty()),
        "chars" => Value::list(s.chars().map(|c| Value::str(c.to_string())).collect()),
        "bytes" => Value::list(s.bytes().map(|b| Value::Int(b as i64)).collect()),
        "lines" => Value::list(s.lines().map(Value::str).collect()),
        "words" => Value::list(s.split_whitespace().map(Value::str).collect()),
        "split" => match args.first() {
            None => Value::list(s.split_whitespace().map(Value::str).collect()),
            Some(_) => {
                let sep = sarg(it, args, 0, name, span)?;
                if sep.is_empty() {
                    Value::list(s.chars().map(|c| Value::str(c.to_string())).collect())
                } else {
                    Value::list(s.split(sep).map(Value::str).collect())
                }
            }
        },
        "split_once" => {
            let sep = sarg(it, args, 0, name, span)?;
            opt(s.split_once(sep).map(|(a, b)| Value::tuple(vec![Value::str(a), Value::str(b)])))
        }
        "trim" => Value::str(s.trim()),
        "trim_start" => Value::str(s.trim_start()),
        "trim_end" => Value::str(s.trim_end()),
        "starts_with" => Value::Bool(s.starts_with(sarg(it, args, 0, name, span)?)),
        "ends_with" => Value::Bool(s.ends_with(sarg(it, args, 0, name, span)?)),
        "contains" => Value::Bool(s.contains(sarg(it, args, 0, name, span)?)),
        "find" | "index" => {
            if let Some(Value::Func(_)) = args.first() {
                return Ok(None);
            }
            let sub = sarg(it, args, 0, name, span)?;
            opt(s.find(sub).map(|b| Value::Int(char_index(s, b))))
        }
        "rfind" => {
            let sub = sarg(it, args, 0, name, span)?;
            opt(s.rfind(sub).map(|b| Value::Int(char_index(s, b))))
        }
        "replace" => Value::str(s.replace(sarg(it, args, 0, name, span)?, sarg(it, args, 1, name, span)?)),
        "upper" => Value::str(s.to_uppercase()),
        "lower" => Value::str(s.to_lowercase()),
        "capitalize" => {
            let mut cs = s.chars();
            match cs.next() {
                Some(c) => Value::str(c.to_uppercase().collect::<String>() + cs.as_str()),
                None => Value::str(""),
            }
        }
        "repeat" => Value::str(s.repeat(int_arg(it, args, 0, name, span)?.max(0) as usize)),
        "parse" => match parse_number(s) {
            Some(v) => v,
            None => match s.trim() {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                _ => parse_err(s, "a number"),
            },
        },
        "rev" => Value::str(s.chars().rev().collect::<String>()),
        "sort" => {
            let mut cs: Vec<char> = s.chars().collect();
            cs.sort();
            Value::str(cs.into_iter().collect::<String>())
        }
        "count" => match args.first() {
            Some(Value::Str(sub)) => {
                if sub.is_empty() {
                    Value::Int(s.chars().count() as i64 + 1)
                } else {
                    Value::Int(s.matches(sub.as_str()).count() as i64)
                }
            }
            _ => return Ok(None),
        },
        "is_digit" => char_class(s, |c| c.is_ascii_digit()),
        "is_alpha" => char_class(s, char::is_alphabetic),
        "is_alnum" => char_class(s, char::is_alphanumeric),
        "is_space" => char_class(s, char::is_whitespace),
        "is_upper" => Value::Bool(s.chars().any(char::is_alphabetic) && !s.chars().any(char::is_lowercase)),
        "is_lower" => Value::Bool(s.chars().any(char::is_alphabetic) && !s.chars().any(char::is_uppercase)),
        "ord" => match s.chars().next() {
            Some(c) => Value::Int(c as i64),
            None => return Err(panic("E0301", span, "`ord` on an empty string")),
        },
        "join" => {
            // Python's `", ".join(xs)`.
            let xs = items(it, arg(args, 0, name, span)?, span)?;
            Value::str(xs.iter().map(|x| it.display(x)).collect::<Vec<_>>().join(s))
        }
        "strip_prefix" => opt(s.strip_prefix(sarg(it, args, 0, name, span)?).map(Value::str)),
        "strip_suffix" => opt(s.strip_suffix(sarg(it, args, 0, name, span)?).map(Value::str)),
        "pad_left" | "pad_right" => {
            let w = int_arg(it, args, 0, name, span)?.max(0) as usize;
            let fill = match args.get(1) {
                Some(Value::Str(f)) => f.chars().next().unwrap_or(' '),
                _ => ' ',
            };
            let n = s.chars().count();
            let pad: String = std::iter::repeat(fill).take(w.saturating_sub(n)).collect();
            Value::str(if name == "pad_left" { pad + s } else { s.to_string() + &pad })
        }
        "get" => {
            let i = int_arg(it, args, 0, name, span)?;
            let n = s.chars().count();
            opt(norm_index(i, n).map(|i| Value::str(s.chars().nth(i).unwrap().to_string())))
        }
        "first" => opt(s.chars().next().map(|c| Value::str(c.to_string()))),
        "last" => opt(s.chars().last().map(|c| Value::str(c.to_string()))),
        _ => return Ok(None),
    }))
}

fn list_method(it: &Interp, xs: &Rc<Vec<Value>>, name: &str, args: &[Value], span: Span) -> R<Option<Value>> {
    Ok(Some(match name {
        "len" => Value::Int(xs.len() as i64),
        "is_empty" => Value::Bool(xs.is_empty()),
        "first" => opt(xs.first().cloned()),
        "last" => opt(xs.last().cloned()),
        "get" => {
            let i = int_arg(it, args, 0, name, span)?;
            opt(norm_index(i, xs.len()).map(|i| xs[i].clone()))
        }
        "contains" => Value::Bool(xs.iter().any(|y| value::eq(y, &args[0]).unwrap_or(false))),
        "index" => {
            let x = arg(args, 0, name, span)?;
            opt(xs.iter().position(|y| value::eq(y, x).unwrap_or(false)).map(|i| Value::Int(i as i64)))
        }
        "to_list" => Value::List(xs.clone()),
        _ => return Ok(None),
    }))
}

/// Methods every iterable shares, working on its elements (map entries are
/// `(k, v)` pairs). Results are lists.
fn iter_method(it: &Interp, recv: Value, name: &str, args: Vec<Value>, span: Span) -> R<Value> {
    let xs = items(it, &recv, span).map_err(|_| panic("E0203", span, format!("{} has no method `{name}`", it.kind(&recv))))?;
    let f = || arg(&args, 0, name, span);
    Ok(match name {
        "len" => Value::Int(xs.len() as i64),
        "is_empty" => Value::Bool(xs.is_empty()),
        "first" => opt(xs.into_iter().next()),
        "last" => opt(xs.into_iter().last()),
        "get" => {
            let i = int_arg(it, &args, 0, name, span)?;
            opt(norm_index(i, xs.len()).map(|i| xs[i].clone()))
        }
        "contains" => Value::Bool(xs.iter().any(|y| value::eq(y, &args[0]).unwrap_or(false))),
        "index" => {
            let x = f()?;
            opt(xs.iter().position(|y| value::eq(y, x).unwrap_or(false)).map(|i| Value::Int(i as i64)))
        }
        "to_list" => Value::list(xs),
        "to_set" => Value::Set(Rc::new(xs.into_iter().collect())),
        "to_map" => {
            let mut m = value::Map::default();
            for x in xs {
                match &x {
                    Value::Tuple(p) | Value::List(p) if p.len() == 2 => {
                        m.insert(p[0].clone(), p[1].clone());
                    }
                    other => return Err(panic("E0301", span, format!("`to_map` needs (key, value) pairs, got {}", it.short(other)))),
                }
            }
            Value::Map(Rc::new(m))
        }
        "map" => {
            let f = f()?;
            let mut out = Vec::with_capacity(xs.len());
            for x in xs {
                out.push(it.call_value(f, vec![x], span)?);
            }
            Value::list(out)
        }
        "filter" => {
            let f = f()?;
            let mut out = Vec::new();
            for x in xs {
                if pred(it, f, x.clone(), span)? {
                    out.push(x);
                }
            }
            Value::list(out)
        }
        "find" => {
            let f = f()?;
            for x in xs {
                if pred(it, f, x.clone(), span)? {
                    return Ok(x);
                }
            }
            Value::None
        }
        "position" => {
            let f = f()?;
            for (i, x) in xs.into_iter().enumerate() {
                if pred(it, f, x, span)? {
                    return Ok(Value::Int(i as i64));
                }
            }
            Value::None
        }
        "any" | "all" => {
            let want = name == "any";
            for x in xs {
                let b = match args.first() {
                    Some(f) => pred(it, f, x, span)?,
                    None => match x {
                        Value::Bool(b) => b,
                        other => return Err(panic("E0301", span, format!("`{name}()` without a predicate needs bools, got {}", it.kind(&other)))),
                    },
                };
                if b == want {
                    return Ok(Value::Bool(want));
                }
            }
            Value::Bool(!want)
        }
        "count" => match args.first() {
            None => Value::Int(xs.len() as i64),
            Some(f @ Value::Func(_)) => {
                let mut n = 0;
                for x in xs {
                    if pred(it, f, x, span)? {
                        n += 1;
                    }
                }
                Value::Int(n)
            }
            Some(v) => Value::Int(xs.iter().filter(|y| value::eq(y, v).unwrap_or(false)).count() as i64),
        },
        "sum" | "product" => {
            let mut acc = Value::Int(if name == "sum" { 0 } else { 1 });
            let op = if name == "sum" { BinOp::Add } else { BinOp::Mul };
            for x in xs {
                acc = it.binop(op, acc, x, span)?;
            }
            acc
        }
        "min" | "max" if args.is_empty() => ext_by(it, xs, None, if name == "min" { Ordering::Less } else { Ordering::Greater }, span)?,
        "min_by" | "max_by" => ext_by(it, xs, Some(f()?), if name == "min_by" { Ordering::Less } else { Ordering::Greater }, span)?,
        "sort" => {
            let mut v = xs;
            sort_values(it, &mut v, span)?;
            Value::list(v)
        }
        "sort_by" => Value::list(sort_by_key(it, xs, f()?, span)?),
        "rev" => Value::list(xs.into_iter().rev().collect()),
        "unique" => Value::list(xs.into_iter().collect::<value::Set>().into_iter().collect()),
        "enumerate" => {
            let start = match args.first() {
                Some(v) => it.int_of(v, span)?,
                None => 0,
            };
            Value::list(xs.into_iter().enumerate().map(|(i, x)| Value::tuple(vec![Value::Int(start + i as i64), x])).collect())
        }
        "zip" => {
            let ys = items(it, f()?, span)?;
            Value::list(xs.into_iter().zip(ys).map(|(a, b)| Value::tuple(vec![a, b])).collect())
        }
        "flat_map" => {
            let f = f()?;
            let mut out = Vec::new();
            for x in xs {
                flatten_into(&mut out, it.call_value(f, vec![x], span)?);
            }
            Value::list(out)
        }
        "flatten" => {
            let mut out = Vec::new();
            for x in xs {
                flatten_into(&mut out, x);
            }
            Value::list(out)
        }
        "take" => {
            let n = int_arg(it, &args, 0, name, span)?.max(0) as usize;
            Value::list(xs.into_iter().take(n).collect())
        }
        "skip" => {
            let n = int_arg(it, &args, 0, name, span)?.max(0) as usize;
            Value::list(xs.into_iter().skip(n).collect())
        }
        "step_by" => {
            let n = int_arg(it, &args, 0, name, span)?;
            if n <= 0 {
                return Err(panic("E0301", span, "step_by needs a positive step"));
            }
            Value::list(xs.into_iter().step_by(n as usize).collect())
        }
        "take_while" | "skip_while" => {
            let f = f()?;
            let mut i = 0;
            while i < xs.len() && pred(it, f, xs[i].clone(), span)? {
                i += 1;
            }
            Value::list(if name == "take_while" { xs[..i].to_vec() } else { xs[i..].to_vec() })
        }
        "chunks" | "windows" => {
            let n = int_arg(it, &args, 0, name, span)?;
            if n <= 0 {
                return Err(panic("E0301", span, format!("`{name}` needs a positive size")));
            }
            let n = n as usize;
            let parts: Vec<Value> = if name == "chunks" {
                xs.chunks(n).map(|c| Value::list(c.to_vec())).collect()
            } else {
                xs.windows(n).map(|c| Value::list(c.to_vec())).collect()
            };
            Value::list(parts)
        }
        "join" => {
            let sep = match args.first() {
                Some(Value::Str(s)) => s.to_string(),
                Some(other) => return Err(panic("E0301", span, format!("`join` wants a str separator, got {}", it.kind(other)))),
                None => String::new(),
            };
            Value::str(xs.iter().map(|x| it.display(x)).collect::<Vec<_>>().join(&sep))
        }
        "group_by" => {
            let f = f()?;
            let mut m = value::Map::default();
            for x in xs {
                let k = it.call_value(f, vec![x.clone()], span)?;
                match m.get_mut(&k) {
                    Some(Value::List(v)) => Rc::make_mut(v).push(x),
                    _ => {
                        m.insert(k, Value::list(vec![x]));
                    }
                }
            }
            Value::Map(Rc::new(m))
        }
        "partition" => {
            let f = f()?;
            let (mut yes, mut no) = (Vec::new(), Vec::new());
            for x in xs {
                if pred(it, f, x.clone(), span)? {
                    yes.push(x);
                } else {
                    no.push(x);
                }
            }
            Value::tuple(vec![Value::list(yes), Value::list(no)])
        }
        "fold" => {
            let mut acc = arg(&args, 0, name, span)?.clone();
            let f = arg(&args, 1, name, span)?;
            for x in xs {
                acc = it.call_value(f, vec![acc, x], span)?;
            }
            acc
        }
        "reduce" => {
            let f = f()?;
            let mut iter = xs.into_iter();
            let Some(mut acc) = iter.next() else { return Ok(Value::None) };
            for x in iter {
                acc = it.call_value(f, vec![acc, x], span)?;
            }
            acc
        }
        "each" => {
            let f = f()?;
            for x in xs {
                it.call_value(f, vec![x], span)?;
            }
            Value::Unit
        }
        "keys" | "values" | "items" => return Err(panic("E0203", span, format!("`{name}` is for maps, got {}", it.kind(&recv)))),
        _ => return Err(panic("E0203", span, format!("{} has no method `{name}`", it.kind(&recv)))),
    })
}

// ----- mutators -----

/// Builtin methods that change their receiver in place.
pub fn mutate(it: &Interp, slot: &mut Value, name: &str, mut args: Vec<Value>, span: Span) -> R<Value> {
    let kind = it.kind(slot);
    let need = |n: usize| -> R<()> {
        if args.len() < n {
            Err(panic("E0206", span, format!("`{name}` needs {n} argument(s)")))
        } else {
            Ok(())
        }
    };
    match slot {
        Value::List(xs) => {
            let v = Rc::make_mut(xs);
            Ok(match name {
                "push" => {
                    need(1)?;
                    v.extend(args);
                    Value::Unit
                }
                "pop" => match args.first() {
                    None => opt(v.pop()),
                    Some(i) => {
                        let i = it.int_of(i, span)?;
                        let idx = norm_index(i, v.len()).ok_or_else(|| panic("E0402", span, format!("index {i} out of range for length {}", v.len())))?;
                        v.remove(idx)
                    }
                },
                "insert" => {
                    need(2)?;
                    let i = it.int_of(&args[0], span)?;
                    let len = v.len() as i64;
                    let idx = if i < 0 { len + i } else { i };
                    if idx < 0 || idx > len {
                        return Err(panic("E0402", span, format!("insert index {i} out of range for length {len}")));
                    }
                    v.insert(idx as usize, args.pop().unwrap());
                    Value::Unit
                }
                "remove" => {
                    need(1)?;
                    let i = match &args[0] {
                        Value::Int(i) => *i,
                        other => return Err(panic("E0301", span, format!("list `remove` takes an index, got {}; to remove a value use `xs.retain(it != x)`", it.kind(other)))),
                    };
                    let idx = norm_index(i, v.len()).ok_or_else(|| panic("E0402", span, format!("index {i} out of range for length {}", v.len())))?;
                    v.remove(idx)
                }
                "clear" => {
                    v.clear();
                    Value::Unit
                }
                "extend" => {
                    need(1)?;
                    let ys = items(it, &args[0], span)?;
                    v.extend(ys);
                    Value::Unit
                }
                "swap" => {
                    need(2)?;
                    let len = v.len();
                    let a = it.int_of(&args[0], span)?;
                    let b = it.int_of(&args[1], span)?;
                    let (Some(a), Some(b)) = (norm_index(a, len), norm_index(b, len)) else {
                        return Err(panic("E0402", span, format!("swap index out of range for length {len}")));
                    };
                    v.swap(a, b);
                    Value::Unit
                }
                "truncate" => {
                    need(1)?;
                    v.truncate(it.int_of(&args[0], span)?.max(0) as usize);
                    Value::Unit
                }
                "add" => return Err(panic("E0203", span, "lists use `push`; `add` is for sets")),
                _ => return Err(panic("E0203", span, format!("list has no method `{name}`"))),
            })
        }
        Value::Map(m) => {
            let m = Rc::make_mut(m);
            Ok(match name {
                "insert" => {
                    need(2)?;
                    let v = args.pop().unwrap();
                    let k = args.pop().unwrap();
                    opt(m.insert(k, v))
                }
                "remove" | "pop" => {
                    need(1)?;
                    opt(m.shift_remove(&args[0]))
                }
                "clear" => {
                    m.clear();
                    Value::Unit
                }
                "extend" => {
                    need(1)?;
                    match &args[0] {
                        Value::Map(o) => {
                            for (k, v) in o.iter() {
                                m.insert(k.clone(), v.clone());
                            }
                        }
                        other => {
                            for x in items(it, other, span)? {
                                match &x {
                                    Value::Tuple(p) if p.len() == 2 => {
                                        m.insert(p[0].clone(), p[1].clone());
                                    }
                                    _ => return Err(panic("E0301", span, "map `extend` needs a map or (key, value) pairs")),
                                }
                            }
                        }
                    }
                    Value::Unit
                }
                "push" | "add" => return Err(panic("E0203", span, format!("maps have no `{name}`; set entries with `m[k] = v`"))),
                _ => return Err(panic("E0203", span, format!("map has no method `{name}`"))),
            })
        }
        Value::Set(s) => {
            let s = Rc::make_mut(s);
            Ok(match name {
                "add" | "insert" => {
                    need(1)?;
                    Value::Bool(s.insert(args.pop().unwrap()))
                }
                "remove" => {
                    need(1)?;
                    Value::Bool(s.shift_remove(&args[0]))
                }
                "clear" => {
                    s.clear();
                    Value::Unit
                }
                "extend" => {
                    need(1)?;
                    s.extend(items(it, &args[0], span)?);
                    Value::Unit
                }
                "push" => return Err(panic("E0203", span, "sets use `add`")),
                _ => return Err(panic("E0203", span, format!("set has no method `{name}`"))),
            })
        }
        Value::Str(_) => Err(panic("E0410", span, format!("strings are immutable, so there is no `{name}`; build a new one: `s += \"x\"`"))),
        Value::None => Err(panic("E0407", span, format!("calling `{name}` on none"))),
        _ => Err(panic("E0203", span, format!("{kind} has no method `{name}`"))),
    }
}
