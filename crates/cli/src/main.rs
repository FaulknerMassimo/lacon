//! `lacon`: run, test and inspect Lacon programs. Output is short and
//! line-oriented because its main reader is an agent.

mod explain;
mod native;

use std::process::ExitCode;

use lacon_interp::{eval::ty_name, run_main, run_tests, Interp, Out, Outcome, Panic, Program};
use lacon_syntax::diag::{render_all, MAX_DIAGS};
use lacon_syntax::Source;

const USAGE: &str = "\
usage: lacon <command> [args]
  run <file.lc> [args...]   run main
  test <file.lc>... [-k s]  run inline tests; prints only failures
  check <file.lc>...        report errors without running
  build <file.lc> [-o exe]  compile to a native executable
  sig <file.lc>             signatures, docs and effects, no bodies
  explain <code>            long form of a diagnostic code, e.g. E0202";

fn main() -> ExitCode {
    // Deep recursion in user programs needs a big interpreter stack.
    let child = std::thread::Builder::new().stack_size(2 << 30).spawn(real_main).expect("spawn interpreter thread");
    child.join().unwrap_or(ExitCode::from(101))
}

fn real_main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let rest = &args[1..];
    match cmd.as_str() {
        "run" => match rest.split_first() {
            Some((file, prog_args)) => run(file, prog_args.to_vec()),
            None => usage_err("run needs a file"),
        },
        "test" => test(rest),
        "check" => {
            if rest.is_empty() {
                return usage_err("check needs a file");
            }
            let mut ok = true;
            for f in rest {
                ok &= load(f).is_some();
            }
            if ok {
                println!("ok");
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        "build" => build(rest),
        "sig" => match rest.first() {
            Some(f) => sig(f),
            None => usage_err("sig needs a file"),
        },
        "explain" => match rest.first() {
            Some(code) => match explain::explain(code) {
                Some(text) => {
                    println!("{text}");
                    ExitCode::SUCCESS
                }
                None => {
                    eprintln!("unknown code {code}");
                    ExitCode::from(2)
                }
            },
            None => usage_err("explain needs a code"),
        },
        "-V" | "--version" | "version" => {
            println!("lacon {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        other if other.ends_with(".lc") => run(other, rest.to_vec()),
        other => usage_err(&format!("unknown command `{other}`")),
    }
}

fn usage_err(msg: &str) -> ExitCode {
    eprintln!("{msg}\n{USAGE}");
    ExitCode::from(2)
}

/// Reads, parses and resolves a file, printing diagnostics. `None` on errors.
fn load(path: &str) -> Option<(Source, Program)> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            return None;
        }
    };
    let src = Source::new(path, text);
    let (mut prog, mut diags) = lacon_interp::load(&src);
    // Type errors after a syntax error are mostly noise; after a name error,
    // only the lines that already have one are.
    if !diags.iter().any(|d| d.code.starts_with("E01")) {
        let lines: std::collections::HashSet<usize> = diags.iter().map(|d| src.line_col(d.span.start).0).collect();
        diags.extend(lacon_check::check(&mut prog, &src.text).into_iter().filter(|d| !lines.contains(&src.line_col(d.span.start).0)));
    }
    if !diags.is_empty() {
        for line in render_all(&diags, &src, MAX_DIAGS) {
            eprintln!("{line}");
        }
        return None;
    }
    Some((src, prog))
}

fn render_panic(p: &Panic, src: &Source) -> Vec<String> {
    let (line, col) = src.line_col(p.span.start);
    let mut out = vec![format!("{} {}:{}:{} {}", p.code, src.name, line, col, p.msg)];
    for d in &p.detail {
        out.push(format!("  {d}"));
    }
    // Collapse runs of the same frame (deep recursion) into one line.
    let mut frames: Vec<(&str, lacon_syntax::Span, usize)> = Vec::new();
    for (name, call) in &p.trace {
        if call.start == 0 && call.end == 0 {
            continue;
        }
        match frames.last_mut() {
            Some((n, s, count)) if *n == name.as_str() && s == call => *count += 1,
            _ => frames.push((name.as_str(), *call, 1)),
        }
    }
    for (name, call, count) in frames.iter().take(4) {
        let (l, c) = src.line_col(call.start);
        let times = if *count > 1 { format!(" ({count} times)") } else { String::new() };
        out.push(format!("  in `{name}`, called at {}:{l}:{c}{times}", src.name));
    }
    if frames.len() > 4 {
        out.push(format!("  ... {} more frames", frames.len() - 4));
    }
    out
}

fn run(path: &str, args: Vec<String>) -> ExitCode {
    let Some((src, prog)) = load(path) else {
        return ExitCode::from(1);
    };
    if prog.main.is_none() {
        eprintln!("E0215 {path}:1:1 no `fn main()` to run");
        return ExitCode::from(1);
    }
    let it = Interp::new(&prog, Out::Stdout(std::io::BufWriter::new(std::io::stdout())), args);
    match run_main(&it) {
        Outcome::Ok => ExitCode::SUCCESS,
        Outcome::Exit(code) => ExitCode::from(code.clamp(0, 255) as u8),
        Outcome::Error(msg) => {
            eprintln!("error: {msg}");
            ExitCode::from(1)
        }
        Outcome::Panic(p) => {
            for line in render_panic(&p, &src) {
                eprintln!("{line}");
            }
            ExitCode::from(1)
        }
    }
}

fn test(args: &[String]) -> ExitCode {
    let mut files = Vec::new();
    let mut filter = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "-k" {
            filter = args.get(i + 1).cloned();
            i += 2;
            continue;
        }
        files.push(args[i].clone());
        i += 1;
    }
    if files.is_empty() {
        return usage_err("test needs a file");
    }
    let (mut passed, mut failed) = (0, 0);
    let mut load_failed = false;
    for path in &files {
        let Some((src, prog)) = load(path) else {
            load_failed = true;
            continue;
        };
        for r in run_tests(&prog, filter.as_deref()) {
            let (line, col) = src.line_col(r.span.start);
            let head = format!("FAIL \"{}\" {}:{line}:{col}", r.name, src.name);
            match r.outcome {
                Outcome::Ok => {
                    passed += 1;
                    continue;
                }
                Outcome::Error(msg) => println!("{head} {msg}"),
                Outcome::Exit(code) => println!("{head} os.exit({code}) called"),
                Outcome::Panic(p) => {
                    println!("{head}");
                    for line in render_panic(&p, &src) {
                        println!("  {line}");
                    }
                }
            }
            failed += 1;
            let out: Vec<&str> = r.output.lines().collect();
            if !out.is_empty() {
                println!("  output ({} lines, last 5):", out.len());
                for l in out.iter().rev().take(5).rev() {
                    println!("    {l}");
                }
            }
        }
    }
    if load_failed {
        return ExitCode::from(1);
    }
    if failed == 0 {
        if passed == 0 {
            println!("no tests");
        } else {
            println!("ok {passed} tests");
        }
        ExitCode::SUCCESS
    } else {
        println!("{failed} failed, {passed} passed");
        ExitCode::from(1)
    }
}

/// Compiles a program to a native executable through C.
fn build(args: &[String]) -> ExitCode {
    let mut file = None;
    let mut out = None;
    let mut keep_c = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => {
                out = args.get(i + 1).cloned();
                i += 2;
                continue;
            }
            "--emit-c" => keep_c = true,
            a => file = Some(a.to_string()),
        }
        i += 1;
    }
    let Some(file) = file else {
        return usage_err("build needs a file");
    };
    let Some((src, prog)) = load(&file) else {
        return ExitCode::from(1);
    };
    if prog.main.is_none() {
        eprintln!("E0215 {file}:1:1 no `fn main()` to build");
        return ExitCode::from(1);
    }
    let out = out.unwrap_or_else(|| std::path::Path::new(&file).file_stem().map_or("a.out".into(), |s| s.to_string_lossy().into_owned()));
    let c = lacon_cgen::generate(&prog, &src);
    match native::compile(&c, &out, keep_c) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("build failed: {e}");
            ExitCode::from(1)
        }
    }
}

fn sig(path: &str) -> ExitCode {
    let Some((_, prog)) = load(path) else {
        return ExitCode::from(1);
    };
    for s in &prog.structs {
        let fields: Vec<String> = s.fields.iter().map(|f| format!("{} {}", f.name, ty_name(&f.ty, &prog))).collect();
        println!("type {} {{{}}}", s.name, fields.join(", "));
    }
    for e in &prog.enums {
        let vs: Vec<String> = e
            .variants
            .iter()
            .map(|v| {
                if v.fields.is_empty() {
                    v.name.clone()
                } else {
                    format!("{}({})", v.name, v.fields.iter().map(|t| ty_name(t, &prog)).collect::<Vec<_>>().join(", "))
                }
            })
            .collect();
        println!("enum {} = {}", e.name, vs.join(" | "));
    }
    for c in &prog.consts {
        println!("{} = ...", c.name);
    }
    let width = prog.fns.iter().map(|f| f.sig.len()).max().unwrap_or(0).min(48);
    for f in &prog.fns {
        let effect = if f.io { "io" } else { "pure" };
        let doc = f.doc.as_ref().map(|d| format!(": {d}")).unwrap_or_default();
        println!("{:width$}  # {effect}{doc}", f.sig);
    }
    if !prog.tests.is_empty() {
        println!("# {} tests", prog.tests.len());
    }
    ExitCode::SUCCESS
}
