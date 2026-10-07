//! `lacon q`: short answers about a program, so an agent reads less of it.
//!
//! - `q def <file> <name>...`: the source of the named items.
//! - `q callers <file> <fn>`: each call of a function, and each use of it
//!   as a value, with the line it's on.
//! - `q type <file>:<line>:<col>`: the type of the innermost expression
//!   there, or of the variable a binding there names.
//! - `q type <file> <name>`: a function's signature, a type's declaration,
//!   or a constant's type.

use std::process::ExitCode;

use lacon_check::T;
use lacon_interp::ir::{walk_ex, Ex, FnId, Node, PatIr, Program, St};
use lacon_interp::value::{Func, Value};
use lacon_syntax::{Source, Span};

use crate::{analyze, edit, enum_line, fn_line, read, report, struct_line, usage_err};

const USAGE: &str = "q def <file.lc> <name>... | q callers <file.lc> <fn> | q type <file.lc>:<line>:<col> | q type <file.lc> <name>";

pub fn query(args: &[String]) -> ExitCode {
    match args {
        [what, file, names @ ..] if what == "def" && !names.is_empty() => def(file, names),
        [what, file, name] if what == "callers" => callers(file, name),
        [what, at] if what == "type" => type_at(at),
        [what, file, name] if what == "type" => type_of(file, name),
        _ => usage_err(USAGE),
    }
}

fn def(path: &str, names: &[String]) -> ExitCode {
    let Some(text) = read(path) else {
        return ExitCode::from(1);
    };
    let mut ok = true;
    let mut first = true;
    for name in names {
        let found = edit::defs(&text, name);
        if found.is_empty() {
            eprintln!("no item `{name}` in {path}");
            ok = false;
        }
        for d in found {
            if !first {
                println!();
            }
            first = false;
            println!("{d}");
        }
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// Each function, test and constant body with what to call it in output.
fn bodies(prog: &Program) -> Vec<(String, &Ex)> {
    let mut out = Vec::new();
    for f in &prog.fns {
        for p in &f.params {
            if let Some(d) = &p.default {
                out.push((f.name.clone(), &d.body));
            }
        }
        out.push((f.name.clone(), &f.body));
    }
    for t in &prog.tests {
        out.push((format!("test \"{}\"", t.name), &t.body));
    }
    for c in &prog.consts {
        out.push((c.name.clone(), &c.value));
    }
    out
}

fn callers(path: &str, name: &str) -> ExitCode {
    let Some(text) = read(path) else {
        return ExitCode::from(1);
    };
    let a = analyze(path, text);
    let Some(ids) = a.prog.fn_names.get(name).cloned() else {
        eprintln!("no function `{name}` in {path}");
        return ExitCode::from(1);
    };
    let hits = |fns: &[FnId]| fns.iter().any(|f| ids.contains(f));
    let mut sites: Vec<(Span, String)> = Vec::new();
    for (owner, body) in bodies(&a.prog) {
        walk_ex(body, 0, &mut |n, _| {
            let Node::Ex(e) = n else { return };
            let called = match e {
                Ex::CallFn { fns, .. } => hits(fns),
                Ex::Method { user: Some(fns), .. } | Ex::Field { user: Some(fns), .. } => hits(fns),
                Ex::Lit(Value::Func(f), _) => matches!(&**f, Func::User(fns) if hits(fns)),
                _ => false,
            };
            if called {
                sites.push((e.span(), owner.clone()));
            }
        });
    }
    if sites.is_empty() {
        println!("no callers of `{name}`");
        return ExitCode::SUCCESS;
    }
    sites.sort_by_key(|s| s.0.start);
    for (span, owner) in sites {
        let (line, col) = a.src.line_col(span.start);
        println!("{path}:{line}:{col} in {owner}: {}", a.src.line_text(line).trim());
    }
    ExitCode::SUCCESS
}

/// `file.lc:line:col`
fn parse_at(at: &str) -> Option<(&str, usize, usize)> {
    let mut parts = at.rsplitn(3, ':');
    let col = parts.next()?.parse().ok()?;
    let line = parts.next()?.parse().ok()?;
    Some((parts.next()?, line, col))
}

fn type_at(at: &str) -> ExitCode {
    let Some((path, line, col)) = parse_at(at) else {
        return usage_err(USAGE);
    };
    let Some(text) = read(path) else {
        return ExitCode::from(1);
    };
    let a = analyze(path, text);
    if a.types.exprs.is_empty() && !a.diags.is_empty() {
        eprintln!("no types until these are fixed:");
        report(&a);
        return ExitCode::from(1);
    }
    let Some(off) = a.src.offset(line, col) else {
        eprintln!("{path} has no line {line}");
        return ExitCode::from(1);
    };
    match find_type(&a.prog, &a.types, &a.src, off) {
        Some((what, t)) => {
            println!("`{what}` {}", lacon_check::show(&t, &a.prog));
            ExitCode::SUCCESS
        }
        None => {
            eprintln!("no expression at {path}:{line}:{col}");
            ExitCode::from(1)
        }
    }
}

/// The innermost typed expression around `off`, or the variable a binding
/// there names: its source and type.
fn find_type(prog: &Program, types: &lacon_check::Types, src: &Source, off: u32) -> Option<(String, T)> {
    let within = |s: Span| s.start <= off && off < s.end.max(s.start + 1);
    let mut best: Option<(Span, T)> = None;
    let mut bound: Option<(Span, T)> = None;
    for (_, body) in bodies(prog) {
        // The slot types of the frame at each lambda depth.
        let mut frames: Vec<Option<&[T]>> = vec![types.frame(body)];
        walk_ex(body, 0, &mut |n, d| {
            match n {
                Node::Ex(e) => {
                    if let Ex::Lambda(l) = e {
                        frames.truncate(d as usize + 1);
                        frames.push(types.frame(&l.body));
                    }
                    let span = e.span();
                    if let (true, Some(t)) = (within(span), types.of(e)) {
                        if best.as_ref().is_none_or(|(b, _)| span.end - span.start < b.end - b.start) {
                            best = Some((span, t.clone()));
                        }
                    }
                }
                Node::St(St::Bind(PatIr::Bind(slot), _, _, span)) if within(*span) => {
                    if let Some(t) = frames.get(d as usize).copied().flatten().and_then(|f| f.get(*slot as usize)) {
                        bound = Some((*span, t.clone()));
                    }
                }
                Node::St(St::For { pat: PatIr::Bind(slot), iter, span, .. }) if span.start <= off && off < iter.span().start => {
                    if let Some(t) = frames.get(d as usize).copied().flatten().and_then(|f| f.get(*slot as usize)) {
                        bound = Some((ident_at(src, off).unwrap_or(*span), t.clone()));
                    }
                }
                _ => {}
            }
        });
    }
    let (span, t) = bound.or(best)?;
    let what = src.snippet(span);
    let what = match what.lines().next() {
        Some(first) if first.len() < what.len() || first.chars().count() > 60 => format!("{}...", first.chars().take(60).collect::<String>()),
        _ => what.to_string(),
    };
    Some((what, t))
}

/// The identifier around `off`, if there is one.
fn ident_at(src: &Source, off: u32) -> Option<Span> {
    let text = &src.text;
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let off = off as usize;
    if !text[off..].starts_with(word) {
        return None;
    }
    let start = text[..off].rfind(|c: char| !word(c)).map_or(0, |i| i + 1);
    let end = text[off..].find(|c: char| !word(c)).map_or(text.len(), |i| off + i);
    Some(Span::new(start, end))
}

fn type_of(path: &str, name: &str) -> ExitCode {
    let Some(text) = read(path) else {
        return ExitCode::from(1);
    };
    let a = analyze(path, text);
    let p = &a.prog;
    let mut lines = Vec::new();
    lines.extend(p.structs.iter().filter(|s| s.name == name).map(|s| struct_line(s, p)));
    lines.extend(p.enums.iter().filter(|e| e.name == name).map(|e| enum_line(e, p)));
    let fns: Vec<_> = p.fns.iter().filter(|f| f.name == name).collect();
    let width = fns.iter().map(|f| f.sig.len()).max().unwrap_or(0);
    lines.extend(fns.iter().map(|f| fn_line(f, width)));
    for c in p.consts.iter().filter(|c| c.name == name) {
        let t = a.types.of(&c.value).map_or("_".to_string(), |t| lacon_check::show(t, p));
        lines.push(format!("{name} {t}"));
    }
    if lines.is_empty() {
        eprintln!("no top-level `{name}` in {path}; for a variable, give a position: {path}:<line>:<col>");
        return ExitCode::from(1);
    }
    for l in lines {
        println!("{l}");
    }
    ExitCode::SUCCESS
}
