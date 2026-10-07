//! Where each top-level item is in a source file, from its tokens alone, so
//! that `lacon put` and `lacon q def` work on a file that doesn't parse.

use crate::lexer::{Lexer, StrPart, Tok};

/// A top-level item: its kind (`fn`, `type` for types and enums, `test`,
/// `const`, or `?` when it isn't one of those) and name, and byte ranges.
#[derive(Clone, Debug)]
pub struct ItemSpan {
    pub kind: &'static str,
    pub name: String,
    /// The start of the comment lines directly above the item, or `start`.
    pub doc: usize,
    /// The start of the item's first line.
    pub start: usize,
    /// The end of its last line, before any blank lines or unindented
    /// comments that follow it.
    pub end: usize,
}

impl ItemSpan {
    pub fn label(&self) -> String {
        match self.kind {
            "test" => format!("test \"{}\"", self.name),
            "const" => self.name.clone(),
            k => format!("{k} {}", self.name),
        }
    }
}

/// The items of `src` in order. An item starts at an unindented token that
/// begins a line (not one that continues the line before it).
pub fn scan(src: &str) -> Vec<ItemSpan> {
    let (toks, _) = Lexer::new(src).tokenize();
    let mut starts = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        if matches!(t.tok, Tok::Newline | Tok::Indent | Tok::Dedent | Tok::Eof) {
            continue;
        }
        let begins = i == 0 || matches!(toks[i - 1].tok, Tok::Newline | Tok::Indent | Tok::Dedent);
        let at = t.span.start as usize;
        if !begins || line_start(src, at) != at {
            continue;
        }
        let next = toks.get(i + 1).map(|t| &t.tok);
        let (kind, name) = match (&t.tok, next) {
            (Tok::Fn, Some(Tok::Ident(n))) => ("fn", n.clone()),
            (Tok::Type | Tok::Enum, Some(Tok::Ident(n))) => ("type", n.clone()),
            (Tok::Ident(t), Some(Tok::Str(parts))) if t == "test" => ("test", str_text(src, parts)),
            (Tok::Ident(n), Some(Tok::Assign)) => ("const", n.clone()),
            _ => ("?", src[at..t.span.end as usize].to_string()),
        };
        starts.push((kind, name, at));
    }
    let mut out = Vec::with_capacity(starts.len());
    for (i, (kind, name, start)) in starts.iter().enumerate() {
        let doc = doc_start(src, *start);
        let limit = starts.get(i + 1).map_or(src.len(), |n| doc_start(src, n.2));
        out.push(ItemSpan { kind, name: name.clone(), doc, start: *start, end: content_end(src, *start, limit) });
    }
    out
}

/// A test's name as `TestDecl` holds it: the literal text, with
/// interpolations kept as written.
fn str_text(src: &str, parts: &[StrPart]) -> String {
    parts
        .iter()
        .map(|p| match p {
            StrPart::Lit(s) => s.clone(),
            StrPart::Expr { span, .. } => format!("{{{}}}", &src[span.start as usize..span.end as usize]),
        })
        .collect()
}

fn line_start(src: &str, at: usize) -> usize {
    src[..at].rfind('\n').map_or(0, |i| i + 1)
}

/// The start of the unindented comment lines directly above the line at
/// `start`.
fn doc_start(src: &str, start: usize) -> usize {
    let mut doc = start;
    while doc > 0 {
        let prev = line_start(src, doc - 1);
        if !src[prev..doc].starts_with('#') {
            break;
        }
        doc = prev;
    }
    doc
}

/// The end of the last line in `start..limit` that isn't blank or an
/// unindented comment, without trailing space. The item's first line always
/// counts.
fn content_end(src: &str, start: usize, limit: usize) -> usize {
    let mut end = limit;
    loop {
        let le = src[..end].trim_end_matches(['\n', '\r']).len();
        if le <= start {
            return start;
        }
        let ls = line_start(src, le).max(start);
        let line = &src[ls..le];
        if ls == start || !(line.trim().is_empty() || line.starts_with('#')) {
            return ls + line.trim_end().len();
        }
        end = ls;
    }
}
