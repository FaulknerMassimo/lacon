use crate::span::{Source, Span};

/// One diagnostic, rendered on one line:
/// `<code> <file>:<line>:<col> <message> [| fix: <replacement>]`
#[derive(Clone, Debug)]
pub struct Diag {
    pub code: &'static str,
    pub span: Span,
    pub msg: String,
    pub fix: Option<String>,
}

impl Diag {
    pub fn new(code: &'static str, span: Span, msg: impl Into<String>) -> Diag {
        Diag { code, span, msg: msg.into(), fix: None }
    }

    pub fn fix(mut self, fix: impl Into<String>) -> Diag {
        self.fix = Some(fix.into());
        self
    }

    pub fn render(&self, src: &Source) -> String {
        let (line, col) = src.line_col(self.span.start);
        let mut s = format!("{} {}:{}:{} {}", self.code, src.name, line, col, self.msg);
        if let Some(fix) = &self.fix {
            s.push_str(" | fix: ");
            s.push_str(fix);
        }
        s
    }
}

/// Default cap on how many diagnostics are printed.
pub const MAX_DIAGS: usize = 20;

/// Sorts, drops follow-on errors (at most one per line), caps the output and
/// appends a count of the rest.
pub fn render_all(diags: &[Diag], src: &Source, cap: usize) -> Vec<String> {
    let mut ds: Vec<&Diag> = diags.iter().collect();
    ds.sort_by_key(|d| d.span.start);
    let mut seen_lines = std::collections::HashSet::new();
    ds.retain(|d| seen_lines.insert(src.line_col(d.span.start).0));
    let mut out: Vec<String> = ds.iter().take(cap).map(|d| d.render(src)).collect();
    if ds.len() > cap {
        out.push(format!("... {} more", ds.len() - cap));
    }
    out
}
