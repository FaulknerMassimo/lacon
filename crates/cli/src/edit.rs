//! `lacon put`: replaces top-level items by name with the ones on stdin, or
//! adds them, so an agent never quotes the old code to change it. Then
//! checks the file.

use std::io::Read;
use std::process::ExitCode;

use lacon_syntax::items::{scan, ItemSpan};

use crate::{analyze, report};

pub fn put(path: &str) -> ExitCode {
    let mut input = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut input) {
        eprintln!("cannot read stdin: {e}");
        return ExitCode::from(2);
    }
    let input = dedent(&input);
    // Items that don't parse are put all the same, since `lacon fix` or
    // another put can mend them in place. An item whose start isn't one is
    // refused, and a body line that lost its indentation shows up as an
    // added item.
    let items = scan(&input);
    if items.is_empty() {
        eprintln!("nothing to put: stdin has no items");
        return ExitCode::from(2);
    }
    if let Some(it) = items.iter().find(|it| it.kind == "?") {
        eprintln!("put takes top-level items (fn, type, enum, test or a constant), not `{}`", it.name);
        return ExitCode::from(2);
    }
    let mut text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            return ExitCode::from(1);
        }
    };
    for it in &items {
        // A new doc comment replaces the old one; without one, the old stays.
        let new = &input[it.doc..it.end];
        match find(&text, it, &input[it.start..it.end]) {
            Some(old) => {
                let from = if it.doc < it.start { old.doc } else { old.start };
                text.replace_range(from..old.end, new);
                println!("replaced {}", it.label());
            }
            None => {
                let kept = text.trim_end().len();
                text.truncate(kept);
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                text.push_str(new);
                println!("added {}", it.label());
            }
        }
    }
    if !text.ends_with('\n') {
        text.push('\n');
    }
    if let Err(e) = std::fs::write(path, &text) {
        eprintln!("cannot write {path}: {e}");
        return ExitCode::from(1);
    }
    if report(&analyze(path, text)) {
        println!("ok");
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// The item in `text` that `it` (whose source is `new`) replaces: the one of
/// the same kind and name, or for a function defined more than once, the
/// one with as many parameters.
fn find(text: &str, it: &ItemSpan, new: &str) -> Option<ItemSpan> {
    let same: Vec<ItemSpan> = scan(text).into_iter().filter(|o| o.kind == it.kind && o.name == it.name).collect();
    if same.len() <= 1 || it.kind != "fn" {
        return same.into_iter().next();
    }
    let want = arity(new);
    same.iter().find(|o| arity(&text[o.start..o.end]) == want).or(same.first()).cloned()
}

/// How many parameters the function in `src` declares, if it parses.
fn arity(src: &str) -> Option<usize> {
    let (m, _) = lacon_syntax::parse(src);
    m.items.iter().find_map(|i| match i {
        lacon_syntax::ast::Item::Fn(f) => Some(f.params.len()),
        _ => None,
    })
}

/// `s` without its leading blank lines, trailing space, and the
/// indentation all its lines share.
fn dedent(s: &str) -> String {
    let lines: Vec<&str> = s.lines().skip_while(|l| l.trim().is_empty()).collect();
    let indent = lines.iter().filter(|l| !l.trim().is_empty()).map(|l| l.len() - l.trim_start().len()).min().unwrap_or(0);
    let out: Vec<&str> = lines.iter().map(|l| if l.trim().is_empty() { "" } else { &l[indent..] }).collect();
    out.join("\n").trim_end().to_string() + "\n"
}

/// The source of the named items, for `lacon q def`: each item of that
/// name, with its doc comment.
pub fn defs(text: &str, name: &str) -> Vec<String> {
    scan(text).into_iter().filter(|it| it.name == name && it.kind != "?").map(|it| text[it.doc..it.end].to_string()).collect()
}
