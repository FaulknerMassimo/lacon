//! The Phase 0 Lacon interpreter: a resolver that lowers the AST to an IR
//! with static name checks, and a tree-walking evaluator. No type checker
//! yet; declared types are checked at function, struct and return
//! boundaries at run time.

pub mod builtins;
pub mod eval;
pub mod frame;
pub mod ir;
pub mod resolve;
pub mod value;

use lacon_syntax::{Diag, Source};

pub use eval::{Ctrl, Interp, Out, Panic};
pub use ir::Program;

pub(crate) fn resolve_conv(name: &str) -> Option<ir::ConvTo> {
    resolve::conv_for(name)
}

/// Parses and resolves a source file.
pub fn load(src: &Source) -> (Program, Vec<Diag>) {
    let (module, mut diags) = lacon_syntax::parse(&src.text);
    let (prog, rdiags) = resolve::Resolver::new(&src.text).module(&module);
    // Resolution errors after a parse error are usually follow-on noise.
    if diags.is_empty() {
        diags = rdiags;
    } else {
        let parse_lines: std::collections::HashSet<usize> = diags.iter().map(|d| src.line_col(d.span.start).0).collect();
        diags.extend(rdiags.into_iter().filter(|d| !parse_lines.contains(&src.line_col(d.span.start).0) && d.code != "E0201"));
    }
    (prog, diags)
}

/// Result of running a program or a test.
pub enum Outcome {
    Ok,
    /// `main` returned an error value.
    Error(String),
    Panic(Box<Panic>),
    Exit(i32),
}

pub fn run_main(it: &Interp) -> Outcome {
    let Some(main) = it.prog.main else {
        return Outcome::Error("no `fn main()` to run".into());
    };
    let r = it.init_globals().and_then(|_| it.call_fid(main, vec![], lacon_syntax::Span::default()));
    it.out.borrow_mut().flush();
    match r {
        Ok(value::Value::Err(e)) | Err(Ctrl::Return(value::Value::Err(e))) => Outcome::Error(it.display(&e)),
        Ok(_) | Err(Ctrl::Return(_)) => Outcome::Ok,
        Err(Ctrl::Panic(p)) => match p.exit {
            Some(code) => Outcome::Exit(code),
            None => {
                let mut p = p;
                p.trace = it.trace();
                Outcome::Panic(p)
            }
        },
        Err(_) => Outcome::Ok,
    }
}

pub struct TestResult {
    pub name: String,
    pub span: lacon_syntax::Span,
    pub outcome: Outcome,
    pub output: String,
}

pub fn run_tests(prog: &Program, filter: Option<&str>) -> Vec<TestResult> {
    let mut results = Vec::new();
    let it = Interp::new(prog, Out::Capture(Vec::new()), vec![]);
    if let Err(Ctrl::Panic(p)) = it.init_globals() {
        results.push(TestResult { name: "(constants)".into(), span: p.span, outcome: Outcome::Panic(p), output: String::new() });
        return results;
    }
    for t in &prog.tests {
        if filter.is_some_and(|f| !t.name.contains(f)) {
            continue;
        }
        it.reset();
        *it.out.borrow_mut() = Out::Capture(Vec::new());
        let frame = frame::Frame::new(t.nslots, None);
        let r = it.eval(&t.body, &frame);
        let outcome = match r {
            Ok(value::Value::Bool(false)) | Err(Ctrl::Return(value::Value::Bool(false))) => Outcome::Error("test expression is false".into()),
            Ok(value::Value::Err(e)) | Err(Ctrl::Return(value::Value::Err(e))) => Outcome::Error(format!("error: {}", it.display(&e))),
            Ok(_) | Err(Ctrl::Return(_)) => Outcome::Ok,
            Err(Ctrl::Panic(mut p)) => {
                if let Some(code) = p.exit {
                    Outcome::Exit(code)
                } else {
                    p.trace = it.trace();
                    Outcome::Panic(p)
                }
            }
            Err(_) => Outcome::Error("`break`/`continue` outside a loop".into()),
        };
        let output = match &*it.out.borrow() {
            Out::Capture(v) => String::from_utf8_lossy(v).into_owned(),
            _ => String::new(),
        };
        results.push(TestResult { name: t.name.clone(), span: t.span, outcome, output });
    }
    results
}
