//! Format specs in interpolations: `{x:.2}`, `{name:>10}`, `{n:05}`, `{n:,}`.
//! Grammar (Python's): `[[fill]align][sign][#][0][width][,|_][.precision][type]`
//! with align `<` `>` `^` `=`, sign `+` `-` or a space, and type one of
//! `b c d e E f F g G n o s x X %`. Width and precision may be computed, as in
//! Python: `{name:<{w}}`, `{x:.{digits}}`.
//!
//! One difference from Python: with no type, a precision counts decimals as
//! in Rust (`{3.14159:.2}` is `3.14`), not significant digits.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FmtSpec {
    /// `None` when not written, so a `0` flag can supply `'0'`.
    pub fill: Option<char>,
    pub align: Option<char>,
    /// `'+'`, `' '`, or `'-'` (the default).
    pub sign: char,
    /// `#`: `0x`, `0o` and `0b` prefixes, and trailing zeros kept by `g`.
    pub alt: bool,
    pub zero: bool,
    pub width: usize,
    /// `,` or `_` between thousands.
    pub grouping: Option<char>,
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
        let mut spec = FmtSpec { sign: '-', ..Default::default() };
        let is_align = |c: char| matches!(c, '<' | '>' | '^' | '=');
        if cs.len() >= 2 && is_align(cs[1]) {
            spec.fill = Some(cs[0]);
            spec.align = Some(cs[1]);
            i = 2;
        } else if !cs.is_empty() && is_align(cs[0]) {
            spec.align = Some(cs[0]);
            i = 1;
        }
        if i < cs.len() && matches!(cs[i], '+' | '-' | ' ') {
            spec.sign = cs[i];
            i += 1;
        }
        if i < cs.len() && cs[i] == '#' {
            spec.alt = true;
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
        if i < cs.len() && matches!(cs[i], ',' | '_') {
            spec.grouping = Some(cs[i]);
            i += 1;
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
        if i < cs.len() && "bcdeEfFgGnosxX%".contains(cs[i]) {
            spec.kind = Some(cs[i]);
            i += 1;
        }
        if i != cs.len() || s.is_empty() {
            return None;
        }
        Some(spec)
    }

    /// Does the type need a number?
    pub fn numeric_kind(&self) -> bool {
        self.kind.is_some_and(|k| k != 's')
    }

    pub fn int(&self, n: i64) -> String {
        let float_kind = matches!(self.kind, Some('e' | 'E' | 'f' | 'F' | 'g' | 'G' | '%'));
        if float_kind || (self.precision.is_some() && !matches!(self.kind, Some('b' | 'o' | 'x' | 'X' | 'c' | 'd' | 'n'))) {
            return self.float(n as f64, &n.to_string());
        }
        let m = n.unsigned_abs();
        let (prefix, digits, group) = match self.kind {
            Some('x') => ("0x", format!("{m:x}"), 4),
            Some('X') => ("0X", format!("{m:X}"), 4),
            Some('o') => ("0o", format!("{m:o}"), 4),
            Some('b') => ("0b", format!("{m:b}"), 4),
            Some('c') => return self.text(&u32::try_from(n).ok().and_then(char::from_u32).map_or_else(String::new, String::from)),
            _ => ("", m.to_string(), 3),
        };
        let digits = match self.grouping {
            Some(sep) => group_digits(&digits, sep, group),
            None => digits,
        };
        self.number(n < 0, if self.alt { prefix } else { "" }, digits)
    }

    /// `plain` is how the value prints with no spec.
    pub fn float(&self, x: f64, plain: &str) -> String {
        let neg = x.is_sign_negative() && !x.is_nan();
        let a = x.abs();
        let upper = matches!(self.kind, Some('E' | 'F' | 'G'));
        let body = if !a.is_finite() {
            if a.is_nan() { "nan" } else { "inf" }.to_string()
        } else {
            let p = self.precision;
            let body = match self.kind {
                Some('f' | 'F') => format!("{a:.*}", p.unwrap_or(6)),
                Some('e' | 'E') => exp_form(a, p.unwrap_or(6)),
                Some('g' | 'G' | 'n') => general(a, p.unwrap_or(6), self.alt),
                Some('%') => format!("{:.*}%", p.unwrap_or(6), a * 100.0),
                Some('d') => format!("{}", a.round()),
                _ => match p {
                    Some(p) => format!("{a:.p$}"),
                    None => plain.trim_start_matches('-').to_string(),
                },
            };
            match self.grouping {
                Some(sep) => {
                    let end = body.find(|c: char| !c.is_ascii_digit()).unwrap_or(body.len());
                    format!("{}{}", group_digits(&body[..end], sep, 3), &body[end..])
                }
                None => body,
            }
        };
        self.number(neg, "", if upper { body.to_uppercase() } else { body })
    }

    /// Strings and everything else: a precision truncates.
    pub fn text(&self, s: &str) -> String {
        let s: String = match self.precision {
            Some(p) => s.chars().take(p).collect(),
            None => s.to_string(),
        };
        self.pad(&s, false)
    }

    fn number(&self, neg: bool, prefix: &str, digits: String) -> String {
        let sign = match (neg, self.sign) {
            (true, _) => "-",
            (false, '+') => "+",
            (false, ' ') => " ",
            _ => "",
        };
        let len = sign.len() + prefix.len() + digits.chars().count();
        // `=` and the `0` flag pad between the sign and the digits.
        if len < self.width && (self.align == Some('=') || (self.zero && self.align.is_none())) {
            let fill = self.fill.unwrap_or(if self.zero { '0' } else { ' ' });
            return format!("{sign}{prefix}{}{digits}", fill.to_string().repeat(self.width - len));
        }
        self.pad(&format!("{sign}{prefix}{digits}"), true)
    }

    fn pad(&self, body: &str, numeric: bool) -> String {
        let len = body.chars().count();
        if len >= self.width {
            return body.to_string();
        }
        let n = self.width - len;
        let fill = |k: usize| self.fill.unwrap_or(if self.zero { '0' } else { ' ' }).to_string().repeat(k);
        match self.align.unwrap_or(if numeric { '>' } else { '<' }) {
            '>' | '=' => format!("{}{body}", fill(n)),
            '^' => format!("{}{body}{}", fill(n / 2), fill(n - n / 2)),
            _ => format!("{body}{}", fill(n)),
        }
    }
}

fn group_digits(digits: &str, sep: char, size: usize) -> String {
    let n = digits.len();
    let mut out = String::with_capacity(n + n / size);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (n - i).is_multiple_of(size) {
            out.push(sep);
        }
        out.push(c);
    }
    out
}

/// `1.234568e+03`, as Python writes it.
fn exp_form(a: f64, p: usize) -> String {
    let s = format!("{a:.p$e}");
    let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    format!("{mant}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
}

/// Python's `g`: `p` significant digits, in fixed or exponent form by size,
/// without trailing zeros unless `alt`.
fn general(a: f64, p: usize, alt: bool) -> String {
    let p = p.max(1);
    let e = format!("{a:.*e}", p - 1);
    let exp: i32 = e.split_once('e').map_or(0, |(_, x)| x.parse().unwrap_or(0));
    let trim = |s: String| -> String {
        if alt || !s.contains('.') {
            s
        } else {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        }
    };
    if a == 0.0 || (-4..p as i32).contains(&exp) {
        trim(format!("{a:.*}", (p as i32 - 1 - exp).max(0) as usize))
    } else {
        let s = exp_form(a, p - 1);
        let (mant, rest) = s.split_once('e').unwrap();
        format!("{}e{rest}", trim(mant.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::FmtSpec;

    fn f(spec: &str, x: f64) -> String {
        FmtSpec::parse(spec).unwrap().float(x, &x.to_string())
    }
    fn i(spec: &str, n: i64) -> String {
        FmtSpec::parse(spec).unwrap().int(n)
    }

    #[test]
    fn matches_python() {
        // Expected values from Python's format().
        assert_eq!(i(",", 1234567), "1,234,567");
        assert_eq!(i("_", -1234567), "-1_234_567");
        assert_eq!(i("_x", 0xdeadbeef), "dead_beef");
        assert_eq!(i("#x", 255), "0xff");
        assert_eq!(i("#010b", 5), "0b00000101");
        assert_eq!(i("+", 7), "+7");
        assert_eq!(i(" ", 7), " 7");
        assert_eq!(i("=+8", 42), "+     42");
        assert_eq!(i("*^7", 42), "**42***");
        assert_eq!(i("05", -42), "-0042");
        assert_eq!(i("c", 97), "a");
        assert_eq!(i(".2f", 3), "3.00");
        assert_eq!(i(".1%", 1), "100.0%");
        assert_eq!(f(",.2f", 1234567.891), "1,234,567.89");
        assert_eq!(f(".1%", 0.256), "25.6%");
        assert_eq!(f("e", 1234.5678), "1.234568e+03");
        assert_eq!(f(".2E", 0.000123), "1.23E-04");
        assert_eq!(f("g", 1234.5678), "1234.57");
        assert_eq!(f("g", 1234567.0), "1.23457e+06");
        assert_eq!(f(".3g", 0.0001234), "0.000123");
        assert_eq!(f("g", 0.00001234), "1.234e-05");
        assert_eq!(f("g", 100.0), "100");
        assert_eq!(f("08.3f", -3.14159), "-003.142");
        assert_eq!(f(">10.2f", 3.14159), "      3.14");
        assert_eq!(f("+.1f", 2.0), "+2.0");
        assert_eq!(f("f", f64::INFINITY), "inf");
        assert_eq!(f(",", 1234.5), "1,234.5");
    }

    #[test]
    fn precision_counts_decimals() {
        assert_eq!(f(".2", 3.14159), "3.14");
        assert_eq!(f("8.3", 2.5), "   2.500");
    }
}
