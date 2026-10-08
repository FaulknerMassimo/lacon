//! Turns source text into tokens, including the layout tokens (NEWLINE,
//! INDENT, DEDENT) that indentation-based blocks need.
//!
//! Newlines inside brackets are ignored. A line also continues the previous
//! one when the previous line ends in a binary operator, or when the line
//! starts with `.name`, `and`, `or` or `??`.

use crate::diag::{Diag, Fix};
use crate::span::Span;

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    Int(i64),
    Float(f64),
    Str(Vec<StrPart>),

    // Keywords.
    Fn,
    Type,
    Enum,
    Var,
    If,
    Elif,
    Else,
    For,
    In,
    While,
    Break,
    Continue,
    Return,
    Match,
    Fail,
    Assert,
    True,
    False,
    None,
    And,
    Or,
    Not,
    As,

    // Punctuation.
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Colon,
    Semi,
    Dot,
    DotDot,
    DotDotEq,
    Assign,
    EqEq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    StarStar,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    Question,
    QQ,
    Pipe,
    PipePipe,
    Amp,
    AmpAmp,
    Caret,
    Bang,
    Shl,
    Shr,
    Arrow,
    FatArrow,
    // Foreign spellings, lexed only so the parser can name the Lacon form.
    ColonColon,
    ColonEq,
    SlashSlash,
    PlusPlus,
    MinusMinus,

    Newline,
    Indent,
    Dedent,
    Eof,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StrPart {
    Lit(String),
    /// `{expr}` or `{expr:spec}`; the expression is source text at `span`.
    Expr { span: Span, spec: Option<String> },
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
    /// True when a newline separates this token from the previous one, even
    /// inside brackets.
    pub nl_before: bool,
    /// True inside `( )`, `[ ]` or `{ }`, where there are no layout tokens.
    pub in_brackets: bool,
}

pub fn describe(t: &Tok) -> String {
    match t {
        Tok::Ident(s) => format!("`{s}`"),
        Tok::Int(n) => format!("`{n}`"),
        Tok::Float(n) => format!("`{n}`"),
        Tok::Str(_) => "string".into(),
        Tok::Newline => "end of line".into(),
        Tok::Indent => "indent".into(),
        Tok::Dedent => "dedent".into(),
        Tok::Eof => "end of file".into(),
        other => format!("`{}`", symbol(other)),
    }
}

pub fn symbol(t: &Tok) -> &'static str {
    use Tok::*;
    match t {
        Fn => "fn",
        Type => "type",
        Enum => "enum",
        Var => "var",
        If => "if",
        Elif => "elif",
        Else => "else",
        For => "for",
        In => "in",
        While => "while",
        Break => "break",
        Continue => "continue",
        Return => "return",
        Match => "match",
        Fail => "fail",
        Assert => "assert",
        True => "true",
        False => "false",
        None => "none",
        And => "and",
        Or => "or",
        Not => "not",
        As => "as",
        LParen => "(",
        RParen => ")",
        LBracket => "[",
        RBracket => "]",
        LBrace => "{",
        RBrace => "}",
        Comma => ",",
        Colon => ":",
        Semi => ";",
        Dot => ".",
        DotDot => "..",
        DotDotEq => "..=",
        Assign => "=",
        EqEq => "==",
        Ne => "!=",
        Lt => "<",
        Le => "<=",
        Gt => ">",
        Ge => ">=",
        Plus => "+",
        Minus => "-",
        Star => "*",
        Slash => "/",
        Percent => "%",
        StarStar => "**",
        PlusEq => "+=",
        MinusEq => "-=",
        StarEq => "*=",
        SlashEq => "/=",
        PercentEq => "%=",
        Question => "?",
        QQ => "??",
        Pipe => "|",
        PipePipe => "||",
        Amp => "&",
        AmpAmp => "&&",
        Caret => "^",
        Bang => "!",
        Shl => "<<",
        Shr => ">>",
        Arrow => "->",
        FatArrow => "=>",
        ColonColon => "::",
        ColonEq => ":=",
        SlashSlash => "//",
        PlusPlus => "++",
        MinusMinus => "--",
        _ => "?",
    }
}

fn keyword(s: &str) -> Option<Tok> {
    use Tok::*;
    Some(match s {
        "fn" => Fn,
        "type" => Type,
        "enum" => Enum,
        "var" => Var,
        "if" => If,
        "elif" => Elif,
        "else" => Else,
        "for" => For,
        "in" => In,
        "while" => While,
        "break" => Break,
        "continue" => Continue,
        "return" => Return,
        "match" => Match,
        "fail" => Fail,
        "assert" => Assert,
        "true" => True,
        "false" => False,
        "none" => None,
        "and" => And,
        "or" => Or,
        "not" => Not,
        "as" => As,
        _ => return Option::None,
    })
}

/// A line ending in one of these continues on the next line.
fn continues_line(t: &Tok) -> bool {
    use Tok::*;
    matches!(
        t,
        Plus | Minus
            | Star
            | Slash
            | Percent
            | StarStar
            | And
            | Or
            | AmpAmp
            | PipePipe
            | QQ
            | EqEq
            | Ne
            | Lt
            | Le
            | Gt
            | Ge
            | Comma
            | Dot
            | Pipe
            | Amp
            | Caret
            | Shl
            | Shr
            | Arrow
            | In
            | Not
            | As
    )
}

pub struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    /// Offset added to every span (for lexing interpolated expressions).
    base: usize,
    out: Vec<Token>,
    indents: Vec<usize>,
    depth: usize,
    /// Interpolated expressions are lexed with layout off.
    layout: bool,
    pub diags: Vec<Diag>,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Lexer<'a> {
        Lexer {
            src,
            bytes: src.as_bytes(),
            pos: 0,
            base: 0,
            out: Vec::new(),
            indents: vec![0],
            depth: 0,
            layout: true,
            diags: Vec::new(),
        }
    }

    /// Lexes `src[start..end]` as a single expression (no layout tokens).
    pub fn expression(src: &'a str, start: usize, end: usize) -> Lexer<'a> {
        let mut lx = Lexer::new(&src[..end]);
        lx.pos = start;
        lx.layout = false;
        lx.base = 0;
        lx
    }

    fn span(&self, start: usize, end: usize) -> Span {
        Span::new(start + self.base, end + self.base)
    }

    fn err(&mut self, start: usize, end: usize, msg: impl Into<String>) {
        let sp = self.span(start, end);
        self.diags.push(Diag::new("E0101", sp, msg));
    }

    fn peek(&self, off: usize) -> u8 {
        *self.bytes.get(self.pos + off).unwrap_or(&0)
    }

    fn push(&mut self, tok: Tok, start: usize, end: usize, nl_before: bool) {
        let span = self.span(start, end);
        let in_brackets = self.depth > 0;
        self.out.push(Token { tok, span, nl_before, in_brackets });
    }

    pub fn tokenize(mut self) -> (Vec<Token>, Vec<Diag>) {
        let mut at_line_start = self.layout;
        let mut nl_before = false;
        let mut first_line = true;
        loop {
            if at_line_start && self.depth == 0 {
                // Measure indentation, skipping blank and comment-only lines.
                let line_start = self.pos;
                let mut col = 0;
                while self.pos < self.bytes.len() {
                    match self.bytes[self.pos] {
                        b' ' => col += 1,
                        b'\t' => col += 4,
                        b'\r' => {}
                        _ => break,
                    }
                    self.pos += 1;
                }
                let c = self.peek(0);
                if c == b'\n' {
                    self.pos += 1;
                    continue;
                }
                if c == b'#' {
                    self.skip_comment();
                    continue;
                }
                if self.pos >= self.bytes.len() {
                    break;
                }
                at_line_start = false;
                let last = self.out.last().map(|t| t.tok.clone());
                let leading_cont = (c == b'.' && self.peek(1) != b'.')
                    || self.src[self.pos..].starts_with("??")
                    || self.src[self.pos..].starts_with("&&")
                    || self.src[self.pos..].starts_with("||")
                    || starts_word(&self.src[self.pos..], "and")
                    || starts_word(&self.src[self.pos..], "or");
                // An `elif` or `else` indented past the current block can only
                // continue a one-line `if` above it: `r = if a: x` then
                // `    else: y`.
                let deeper_branch = (starts_word(&self.src[self.pos..], "elif") || starts_word(&self.src[self.pos..], "else"))
                    && col > *self.indents.last().unwrap();
                let after_opener = matches!(last, Some(Tok::Colon) | Some(Tok::Assign));
                let cont = !first_line
                    && (last.as_ref().is_some_and(continues_line) || ((leading_cont || deeper_branch) && !after_opener));
                first_line = false;
                if !cont {
                    if !self.out.is_empty() && !matches!(last, Some(Tok::Newline)) {
                        self.push(Tok::Newline, line_start.saturating_sub(1), line_start, false);
                    }
                    let top = *self.indents.last().unwrap();
                    if col > top {
                        self.indents.push(col);
                        self.push(Tok::Indent, line_start, self.pos, true);
                    } else if col < top {
                        while col < *self.indents.last().unwrap() {
                            self.indents.pop();
                            self.push(Tok::Dedent, self.pos, self.pos, true);
                        }
                        if col != *self.indents.last().unwrap() {
                            self.err(line_start, self.pos, "indentation does not match any enclosing block");
                            self.indents.push(col);
                        }
                    }
                }
                nl_before = true;
            }

            if self.pos >= self.bytes.len() {
                break;
            }
            let c = self.bytes[self.pos];
            match c {
                b'\n' => {
                    self.pos += 1;
                    nl_before = true;
                    if self.depth == 0 && self.layout {
                        at_line_start = true;
                    }
                    continue;
                }
                b' ' | b'\t' | b'\r' => {
                    self.pos += 1;
                    continue;
                }
                b'#' => {
                    self.skip_comment();
                    continue;
                }
                _ => {}
            }
            let start = self.pos;
            let tok = self.lex_token();
            let nl = std::mem::replace(&mut nl_before, false);
            if let Some(tok) = tok {
                match tok {
                    Tok::LParen | Tok::LBracket | Tok::LBrace => self.depth += 1,
                    Tok::RParen | Tok::RBracket | Tok::RBrace => self.depth = self.depth.saturating_sub(1),
                    _ => {}
                }
                self.push(tok, start, self.pos, nl);
            }
        }
        let end = self.bytes.len();
        if self.layout {
            if !self.out.is_empty() && !matches!(self.out.last().unwrap().tok, Tok::Newline) {
                self.push(Tok::Newline, end, end, false);
            }
            while self.indents.len() > 1 {
                self.indents.pop();
                self.push(Tok::Dedent, end, end, true);
            }
        }
        self.push(Tok::Eof, end, end, true);
        (self.out, self.diags)
    }

    fn skip_comment(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos] != b'\n' {
            self.pos += 1;
        }
    }

    fn lex_token(&mut self) -> Option<Tok> {
        let c = self.bytes[self.pos];
        if c.is_ascii_digit() {
            return Some(self.lex_number());
        }
        if c == b'"' || c == b'\'' {
            return Some(self.lex_string());
        }
        if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 {
            let start = self.pos;
            while self.pos < self.bytes.len() {
                let b = self.bytes[self.pos];
                if b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80 {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            let word = &self.src[start..self.pos];
            // Python f-strings: every Lacon string already interpolates.
            if word == "f" && matches!(self.peek(0), b'"' | b'\'') {
                return Some(self.lex_string());
            }
            return Some(keyword(word).unwrap_or_else(|| Tok::Ident(word.to_string())));
        }
        let two = |a: u8, b: u8| c == a && self.peek(1) == b;
        use Tok::*;
        let (tok, len) = if c == b'.' && self.peek(1) == b'.' && self.peek(2) == b'=' {
            (DotDotEq, 3)
        } else if two(b'.', b'.') {
            (DotDot, 2)
        } else if two(b'=', b'=') {
            (EqEq, 2)
        } else if two(b'!', b'=') {
            (Ne, 2)
        } else if two(b'<', b'=') {
            (Le, 2)
        } else if two(b'>', b'=') {
            (Ge, 2)
        } else if two(b'<', b'<') {
            (Shl, 2)
        } else if two(b'>', b'>') {
            (Shr, 2)
        } else if two(b'*', b'*') {
            (StarStar, 2)
        } else if two(b'+', b'=') {
            (PlusEq, 2)
        } else if two(b'-', b'=') {
            (MinusEq, 2)
        } else if two(b'*', b'=') {
            (StarEq, 2)
        } else if two(b'/', b'=') {
            (SlashEq, 2)
        } else if two(b'%', b'=') {
            (PercentEq, 2)
        } else if two(b'?', b'?') {
            (QQ, 2)
        } else if two(b'|', b'|') {
            (PipePipe, 2)
        } else if two(b'&', b'&') {
            (AmpAmp, 2)
        } else if two(b'-', b'>') {
            (Arrow, 2)
        } else if two(b'=', b'>') {
            (FatArrow, 2)
        } else if two(b':', b':') {
            (ColonColon, 2)
        } else if two(b':', b'=') {
            (ColonEq, 2)
        } else if two(b'/', b'/') {
            (SlashSlash, 2)
        } else if two(b'+', b'+') {
            (PlusPlus, 2)
        } else if two(b'-', b'-') {
            (MinusMinus, 2)
        } else {
            let t = match c {
                b'(' => LParen,
                b')' => RParen,
                b'[' => LBracket,
                b']' => RBracket,
                b'{' => LBrace,
                b'}' => RBrace,
                b',' => Comma,
                b':' => Colon,
                b';' => Semi,
                b'.' => Dot,
                b'=' => Assign,
                b'<' => Lt,
                b'>' => Gt,
                b'+' => Plus,
                b'-' => Minus,
                b'*' => Star,
                b'/' => Slash,
                b'%' => Percent,
                b'?' => Question,
                b'|' => Pipe,
                b'&' => Amp,
                b'^' => Caret,
                b'!' => Bang,
                _ => {
                    let ch = self.src[self.pos..].chars().next().unwrap();
                    let start = self.pos;
                    self.pos += ch.len_utf8();
                    self.err(start, self.pos, format!("unexpected character `{ch}`"));
                    return Option::None;
                }
            };
            (t, 1)
        };
        self.pos += len;
        Some(tok)
    }

    fn lex_number(&mut self) -> Tok {
        let start = self.pos;
        let radix = if self.peek(0) == b'0' && matches!(self.peek(1), b'x' | b'b' | b'o') {
            let r = match self.peek(1) {
                b'x' => 16,
                b'b' => 2,
                _ => 8,
            };
            self.pos += 2;
            r
        } else {
            10
        };
        let digits_start = self.pos;
        while self.pos < self.bytes.len() && (self.bytes[self.pos].is_ascii_alphanumeric() || self.bytes[self.pos] == b'_') {
            // Stop at an exponent sign or at `e` that isn't followed by digits.
            if radix == 10 && !self.bytes[self.pos].is_ascii_digit() && self.bytes[self.pos] != b'_' {
                break;
            }
            self.pos += 1;
        }
        let mut is_float = false;
        if radix == 10 {
            // A fraction needs a digit after the dot, so `1..5` and `x.0.1` lex correctly.
            let prev_is_dot = start > 0 && self.bytes[start - 1] == b'.';
            if !prev_is_dot && self.peek(0) == b'.' && self.peek(1).is_ascii_digit() {
                is_float = true;
                self.pos += 1;
                while self.pos < self.bytes.len() && (self.bytes[self.pos].is_ascii_digit() || self.bytes[self.pos] == b'_') {
                    self.pos += 1;
                }
            }
            if matches!(self.peek(0), b'e' | b'E')
                && (self.peek(1).is_ascii_digit() || (matches!(self.peek(1), b'+' | b'-') && self.peek(2).is_ascii_digit()))
            {
                is_float = true;
                self.pos += 2;
                while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
                    self.pos += 1;
                }
            }
        }
        let text: String = self.src[digits_start..self.pos].chars().filter(|&c| c != '_').collect();
        if is_float {
            match text.parse::<f64>() {
                Ok(f) => Tok::Float(f),
                Err(_) => {
                    self.err(start, self.pos, "bad number");
                    Tok::Float(0.0)
                }
            }
        } else {
            match i64::from_str_radix(&text, radix) {
                Ok(n) => Tok::Int(n),
                Err(_) => {
                    self.err(start, self.pos, "integer literal too large or malformed");
                    Tok::Int(0)
                }
            }
        }
    }

    fn lex_string(&mut self) -> Tok {
        let q = self.bytes[self.pos];
        let triple = self.peek(1) == q && self.peek(2) == q;
        let open_start = self.pos;
        self.pos += if triple { 3 } else { 1 };
        let mut parts = Vec::new();
        let mut lit = String::new();
        loop {
            if self.pos >= self.bytes.len() {
                self.err(open_start, self.pos, "unterminated string");
                break;
            }
            let c = self.bytes[self.pos];
            if c == q && (!triple || (self.peek(1) == q && self.peek(2) == q)) {
                self.pos += if triple { 3 } else { 1 };
                break;
            }
            if c == b'\n' && !triple {
                self.err(open_start, self.pos, "unterminated string");
                break;
            }
            match c {
                b'\\' => {
                    let esc_start = self.pos;
                    self.pos += 1;
                    let e = self.peek(0);
                    self.pos += 1;
                    match e {
                        b'n' => lit.push('\n'),
                        b't' => lit.push('\t'),
                        b'r' => lit.push('\r'),
                        b'0' => lit.push('\0'),
                        b'\\' => lit.push('\\'),
                        b'"' => lit.push('"'),
                        b'\'' => lit.push('\''),
                        b'{' => lit.push('{'),
                        b'}' => lit.push('}'),
                        b'\n' => {}
                        b'u' if self.peek(0) == b'{' => {
                            let hex_start = self.pos + 1;
                            while self.pos < self.bytes.len() && self.bytes[self.pos] != b'}' {
                                self.pos += 1;
                            }
                            let hex = &self.src[hex_start..self.pos];
                            self.pos += 1;
                            match u32::from_str_radix(hex, 16).ok().and_then(char::from_u32) {
                                Some(ch) => lit.push(ch),
                                Option::None => self.err(esc_start, self.pos, "bad unicode escape"),
                            }
                        }
                        // Python's, JavaScript's and Java's `\u0001`.
                        b'u' if self.bytes.get(self.pos..self.pos + 4).is_some_and(|h| h.iter().all(u8::is_ascii_hexdigit)) => {
                            let hex = &self.src[self.pos..self.pos + 4];
                            self.pos += 4;
                            match u32::from_str_radix(hex, 16).ok().and_then(char::from_u32) {
                                Some(ch) => lit.push(ch),
                                Option::None => self.err(esc_start, self.pos, "bad unicode escape"),
                            }
                        }
                        _ => self.err(esc_start, self.pos, "unknown escape"),
                    }
                }
                b'{' if self.peek(1) == b'{' => {
                    lit.push('{');
                    self.pos += 2;
                }
                b'}' if self.peek(1) == b'}' => {
                    lit.push('}');
                    self.pos += 2;
                }
                // A whole string of `{}` is an empty JSON object, not a
                // placeholder; `print("{}", x)` is caught when resolving.
                b'{' if !triple && parts.is_empty() && lit.is_empty() && self.peek(1) == b'}' && self.peek(2) == q => {
                    lit.push_str("{}");
                    self.pos += 2;
                }
                b'{' => {
                    let brace = self.pos;
                    self.pos += 1;
                    let expr_start = self.pos;
                    // A `{` that cannot start an interpolation (right before the
                    // closing quote, at a line end, never closed, or before a
                    // `\`) is literal, so "{", "([{" and "{\n" need no escaping.
                    let next = self.peek(0);
                    let end = if next == q || next == b'\n' || next == 0 || next == b'\\' { None } else { self.scan_interp(q) };
                    let Some((end, escaped)) = end else {
                        self.pos = expr_start;
                        lit.push('{');
                        continue;
                    };
                    if escaped {
                        // `{s.pad_left(2, \"0\")}` would otherwise be literal
                        // text, printed as written with no error.
                        let text = &self.src[brace..end + 1];
                        let esc = format!("\\{}", q as char);
                        let fix = (!text.contains('\'')).then(|| Fix::at(self.span(brace, end + 1), text.replace(&esc, "'")));
                        let msg = format!("inside `{{...}}`, `{esc}` doesn't quote a string; use `'`");
                        self.diags.push(Diag::new("E0101", self.span(brace, end + 1), msg).with_fix(fix));
                        self.pos = expr_start;
                        lit.push('{');
                        continue;
                    }
                    if !lit.is_empty() {
                        parts.push(StrPart::Lit(std::mem::take(&mut lit)));
                    }
                    let (expr_end, spec) = split_spec(&self.src[expr_start..end]);
                    let expr_end = expr_start + expr_end;
                    if self.src[expr_start..expr_end].trim().is_empty() {
                        self.err(brace, end + 1, "empty `{}` in string: put the value inside, \"{x}\" (no format placeholders); `{{}}` is a literal `{}`");
                    }
                    parts.push(StrPart::Expr { span: self.span(expr_start, expr_end), spec });
                    self.pos = end + 1;
                }
                _ => {
                    let ch = self.src[self.pos..].chars().next().unwrap();
                    lit.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
        if !lit.is_empty() {
            parts.push(StrPart::Lit(lit));
        }
        if triple {
            dedent_parts(&mut parts);
        }
        Tok::Str(parts)
    }

    /// Finds the `}` closing an interpolation, skipping nested brackets and
    /// strings. Returns its byte offset, and whether a nested string was
    /// quoted with the outer quote escaped (`\"0\"`), which is an error.
    fn scan_interp(&mut self, q: u8) -> Option<(usize, bool)> {
        let mut escaped = false;
        let mut depth = 0;
        while self.pos < self.bytes.len() {
            let c = self.bytes[self.pos];
            match c {
                b'{' | b'(' | b'[' => depth += 1,
                b')' | b']' => depth -= 1,
                b'}' => {
                    if depth == 0 {
                        return Some((self.pos, escaped));
                    }
                    depth -= 1;
                }
                b'\\' if self.bytes.get(self.pos + 1) == Some(&q) => {
                    // A string quoted `\"...\"`, ending on this line.
                    self.pos += 2;
                    while !(self.bytes.get(self.pos) == Some(&b'\\') && self.bytes.get(self.pos + 1) == Some(&q)) {
                        if self.pos >= self.bytes.len() || self.bytes[self.pos] == b'\n' {
                            return Option::None;
                        }
                        self.pos += 1;
                    }
                    self.pos += 1;
                    escaped = true;
                }
                b'"' | b'\'' => {
                    let q = c;
                    self.pos += 1;
                    while self.pos < self.bytes.len() && self.bytes[self.pos] != q {
                        if self.bytes[self.pos] == b'\n' {
                            return Option::None;
                        }
                        if self.bytes[self.pos] == b'\\' {
                            self.pos += 1;
                        }
                        self.pos += 1;
                    }
                }
                // No other expression holds a `\` outside a string, so the
                // `{` is literal: "{ \n" + body + "\n}" isn't one interpolation.
                b'\n' | b'\\' => return Option::None,
                _ => {}
            }
            self.pos += 1;
        }
        Option::None
    }
}

fn starts_word(s: &str, w: &str) -> bool {
    s.starts_with(w) && !s[w.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
}

/// Splits `expr:spec` at the last top-level colon when what follows is a valid
/// format spec. Returns the expression length and the spec.
fn split_spec(s: &str) -> (usize, Option<String>) {
    let mut depth = 0i32;
    let mut last = Option::None;
    let mut in_str: Option<char> = Option::None;
    for (i, c) in s.char_indices() {
        if let Some(q) = in_str {
            if c == q {
                in_str = Option::None;
            }
            continue;
        }
        match c {
            '"' | '\'' => in_str = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ':' if depth == 0 => last = Some(i),
            _ => {}
        }
    }
    if let Some(i) = last {
        let spec = &s[i + 1..];
        if crate::fmtspec::FmtSpec::parse(spec).is_some() {
            return (i, Some(spec.to_string()));
        }
    }
    (s.len(), Option::None)
}

/// Triple-quoted strings drop the first newline and the indentation of the
/// closing delimiter's line, like Swift.
fn dedent_parts(parts: &mut Vec<StrPart>) {
    // Indentation of the closing line: the spaces after the last newline.
    let indent = match parts.last() {
        Some(StrPart::Lit(s)) => match s.rfind('\n') {
            Some(i) if s[i + 1..].chars().all(|c| c == ' ') => s.len() - i - 1,
            _ => 0,
        },
        _ => 0,
    };
    if let Some(StrPart::Lit(s)) = parts.last_mut() {
        if let Some(i) = s.rfind('\n') {
            if s[i + 1..].chars().all(|c| c == ' ') {
                s.truncate(i);
            }
        }
    }
    for (k, p) in parts.iter_mut().enumerate() {
        if let StrPart::Lit(s) = p {
            let mut out = String::with_capacity(s.len());
            let mut first = true;
            for line in s.split('\n') {
                if !first {
                    out.push('\n');
                    let strip = line.len() - line.trim_start_matches(' ').len();
                    out.push_str(&line[strip.min(indent)..]);
                } else {
                    out.push_str(line);
                }
                first = false;
            }
            if k == 0 && out.starts_with('\n') {
                out.remove(0);
            }
            *s = out;
        }
    }
    parts.retain(|p| !matches!(p, StrPart::Lit(s) if s.is_empty()));
}
