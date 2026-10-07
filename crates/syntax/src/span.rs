/// Byte range into a source file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Span {
        Span { start: start as u32, end: end as u32 }
    }

    pub fn to(self, other: Span) -> Span {
        Span { start: self.start.min(other.start), end: self.end.max(other.end) }
    }
}

/// A source file with a line index for turning byte offsets into line:col.
pub struct Source {
    pub name: String,
    pub text: String,
    line_starts: Vec<usize>,
}

impl Source {
    pub fn new(name: impl Into<String>, text: impl Into<String>) -> Source {
        let text = text.into();
        let mut line_starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        Source { name: name.into(), text, line_starts }
    }

    /// 1-based line and column (column counted in chars).
    pub fn line_col(&self, offset: u32) -> (usize, usize) {
        let offset = (offset as usize).min(self.text.len());
        let line = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let start = self.line_starts[line];
        let col = self.text[start..offset].chars().count();
        (line + 1, col + 1)
    }

    /// The byte offset of a 1-based line and column (in chars), if the
    /// line exists; a column past the end gives the end of the line.
    pub fn offset(&self, line: usize, col: usize) -> Option<u32> {
        let start = *self.line_starts.get(line.checked_sub(1)?)?;
        let text = self.line_text(line);
        let at = text.char_indices().nth(col.saturating_sub(1)).map_or(text.len(), |(i, _)| i);
        Some((start + at) as u32)
    }

    pub fn line_text(&self, line: usize) -> &str {
        let start = self.line_starts[line - 1];
        let end = self.line_starts.get(line).copied().unwrap_or(self.text.len());
        self.text[start..end].trim_end_matches(['\n', '\r'])
    }

    pub fn snippet(&self, span: Span) -> &str {
        &self.text[span.start as usize..(span.end as usize).min(self.text.len())]
    }
}
