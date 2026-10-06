//! Format specs in interpolations: `{x:.2}`, `{name:>10}`, `{n:05}`, `{n:x}`.
//! Grammar (Python's, minus the rarely used parts):
//! `[[fill]align][+][0][width][.precision][type]` with align `<` `>` `^`
//! and type one of `f e x X b o d s`. Width and precision may be computed,
//! as in Python: `{name:<{w}}`, `{x:.{digits}}`.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FmtSpec {
    pub fill: char,
    pub align: Option<char>,
    pub plus: bool,
    pub zero: bool,
    pub width: usize,
    pub precision: Option<usize>,
    pub kind: Option<char>,
    /// Byte ranges, within the spec text, of a computed `{width}` and
    /// `{precision}` (the expression without its braces).
    pub width_arg: Option<(usize, usize)>,
    pub precision_arg: Option<(usize, usize)>,
}

impl FmtSpec {
    pub fn parse(s: &str) -> Option<FmtSpec> {
        let cs: Vec<char> = s.chars().collect();
        let mut i = 0;
        let mut spec = FmtSpec { fill: ' ', ..Default::default() };
        let is_align = |c: char| matches!(c, '<' | '>' | '^');
        if cs.len() >= 2 && is_align(cs[1]) {
            spec.fill = cs[0];
            spec.align = Some(cs[1]);
            i = 2;
        } else if !cs.is_empty() && is_align(cs[0]) {
            spec.align = Some(cs[0]);
            i = 1;
        }
        if i < cs.len() && cs[i] == '+' {
            spec.plus = true;
            i += 1;
        }
        if i < cs.len() && cs[i] == '0' {
            spec.zero = true;
            i += 1;
        }
        let byte = |i: usize| cs[..i].iter().map(|c| c.len_utf8()).sum::<usize>();
        // `{expr}` in the width or precision slot: the index after its `}`.
        let computed = |i: usize| -> Option<usize> {
            if cs.get(i) != Some(&'{') {
                return None;
            }
            let mut depth = 0;
            for (j, &c) in cs.iter().enumerate().skip(i) {
                match c {
                    '{' | '(' | '[' => depth += 1,
                    '}' | ')' | ']' => {
                        depth -= 1;
                        if depth == 0 {
                            return if j > i + 1 { Some(j + 1) } else { None };
                        }
                    }
                    _ => {}
                }
            }
            None
        };
        if let Some(end) = computed(i) {
            spec.width_arg = Some((byte(i + 1), byte(end - 1)));
            i = end;
        } else {
            let w0 = i;
            while i < cs.len() && cs[i].is_ascii_digit() {
                i += 1;
            }
            if i > w0 {
                spec.width = cs[w0..i].iter().collect::<String>().parse().ok()?;
            }
        }
        if i < cs.len() && cs[i] == '.' {
            i += 1;
            if let Some(end) = computed(i) {
                spec.precision_arg = Some((byte(i + 1), byte(end - 1)));
                spec.precision = Some(0);
                i = end;
            } else {
                let p0 = i;
                while i < cs.len() && cs[i].is_ascii_digit() {
                    i += 1;
                }
                if i == p0 {
                    return None;
                }
                spec.precision = Some(cs[p0..i].iter().collect::<String>().parse().ok()?);
            }
        }
        if i < cs.len() && matches!(cs[i], 'f' | 'e' | 'x' | 'X' | 'b' | 'o' | 'd' | 's') {
            spec.kind = Some(cs[i]);
            i += 1;
        }
        if i != cs.len() || s.is_empty() {
            return None;
        }
        Some(spec)
    }

    /// Pads an already formatted value to the spec's width.
    pub fn pad(&self, body: String, numeric: bool) -> String {
        let len = body.chars().count();
        if len >= self.width {
            return body;
        }
        let n = self.width - len;
        if self.zero && self.align.is_none() && numeric {
            let (sign, digits) = match body.strip_prefix('-') {
                Some(rest) => ("-", rest.to_string()),
                None => match body.strip_prefix('+') {
                    Some(rest) => ("+", rest.to_string()),
                    None => ("", body.clone()),
                },
            };
            return format!("{sign}{}{digits}", "0".repeat(n));
        }
        let fill = |k: usize| self.fill.to_string().repeat(k);
        match self.align.unwrap_or(if numeric { '>' } else { '<' }) {
            '>' => format!("{}{body}", fill(n)),
            '^' => format!("{}{body}{}", fill(n / 2), fill(n - n / 2)),
            _ => format!("{body}{}", fill(n)),
        }
    }
}
