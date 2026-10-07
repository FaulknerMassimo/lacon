//! `lacon fix`: applies the fixes that diagnostics carry, checks again and
//! repeats, since fixing a syntax error can uncover type errors with fixes
//! of their own. Prints each fix it made as the line it left, then what
//! still fails.

use std::collections::HashSet;
use std::process::ExitCode;

use lacon_syntax::{Diag, Fix, Source, Span};

use crate::{analyze, read, report, Analysis};

/// Rounds of fixing and checking before giving up on more.
const ROUNDS: usize = 5;

/// A fix made: its code, and where its text starts in the current source.
struct Made {
    code: &'static str,
    at: usize,
}

pub fn fix(paths: &[String]) -> ExitCode {
    let mut ok = true;
    for path in paths {
        ok &= fix_file(path);
    }
    if ok {
        println!("ok");
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn fix_file(path: &str) -> bool {
    let Some(orig) = read(path) else {
        return false;
    };
    let mut text = orig.clone();
    let mut made: Vec<Made> = Vec::new();
    let mut a = analyze(path, text.clone());
    for _ in 0..ROUNDS {
        // A fix for the same error at the place an earlier fix wrote didn't
        // work, and another would only stack on it.
        let edits: Vec<(&'static str, Span, String)> =
            edits(&a).into_iter().filter(|(code, at, _)| !made.iter().any(|m| m.code == *code && m.at == at.start as usize)).collect();
        if edits.is_empty() {
            break;
        }
        // Apply from the end, so earlier offsets stay put, then move the
        // offsets of earlier rounds' fixes past this round's.
        for (_, at, new) in edits.iter().rev() {
            text.replace_range(at.start as usize..at.end as usize, new);
        }
        let mut shift: isize = 0;
        let mut moved = Vec::with_capacity(edits.len());
        for (code, at, new) in &edits {
            moved.push(Made { code, at: (at.start as isize + shift) as usize });
            shift += new.len() as isize - (at.end - at.start) as isize;
        }
        for m in &mut made {
            m.at = moved_offset(m.at, &edits);
        }
        made.extend(moved);
        a = analyze(path, text.clone());
    }
    if text != orig {
        if let Err(e) = std::fs::write(path, &text) {
            eprintln!("cannot write {path}: {e}");
            return false;
        }
    }
    made.sort_by_key(|m| m.at);
    let src = Source::new(path, text.clone());
    for m in &made {
        let (line, col) = src.line_col(m.at as u32);
        println!("fixed {} {path}:{line}:{col} {}", m.code, short(src.line_text(line).trim()));
    }
    report(&a)
}

/// The fixes to apply in one round, in order and not overlapping: one per
/// line, from the diagnostic `check` would print for it.
fn edits(a: &Analysis) -> Vec<(&'static str, Span, String)> {
    let mut ds: Vec<&Diag> = a.diags.iter().collect();
    ds.sort_by_key(|d| d.span.start);
    let mut lines = HashSet::new();
    ds.retain(|d| lines.insert(a.src.line_col(d.span.start).0));
    let mut fixes: Vec<(&'static str, Span, String)> = ds
        .into_iter()
        .filter_map(|d| match &d.fix {
            Some(Fix { text, at: Some(at) }) => Some((d.code, *at, text.clone())),
            _ => None,
        })
        .collect();
    fixes.sort_by_key(|f| (f.1.start, f.1.end));
    let mut out: Vec<(&'static str, Span, String)> = Vec::new();
    for f in fixes {
        if out.last().is_none_or(|l| f.1.start >= l.1.end && f.1.start > l.1.start) {
            out.push(f);
        }
    }
    out
}

/// Where an offset into the source before `edits` is after them.
fn moved_offset(at: usize, edits: &[(&'static str, Span, String)]) -> usize {
    let mut out = at as isize;
    for (_, sp, new) in edits {
        let (s, e) = (sp.start as usize, sp.end as usize);
        if at >= e {
            out += new.len() as isize - (e - s) as isize;
        } else if at > s {
            // Inside a replaced range: the start of its replacement.
            out -= (at - s) as isize;
        }
    }
    out as usize
}

fn short(line: &str) -> String {
    const MAX: usize = 100;
    if line.chars().count() <= MAX {
        return line.to_string();
    }
    let cut: String = line.chars().take(MAX).collect();
    format!("{cut}...")
}
