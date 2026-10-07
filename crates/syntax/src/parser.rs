//! Recursive-descent parser. Errors are recorded and parsing resumes at the
//! next statement or declaration, so one run reports every independent error.
//!
//! Where an agent writes a construct from another language and the meaning is
//! unambiguous (`->`, `&&`, `else if`, `name: Type`, `f"..."`), the parser
//! accepts it. Where it isn't (`let`, `//`, braces for blocks), it reports the
//! Lacon form as a hint.

use crate::ast::*;
use crate::diag::Diag;
use crate::lexer::{describe, Lexer, StrPart, Tok, Token};
use crate::span::Span;

type P<T> = Result<T, ()>;

pub struct Parser<'a> {
    src: &'a str,
    toks: Vec<Token>,
    pos: usize,
    indent: i32,
    /// Inside brackets there are no layout tokens, so a `match` there finds
    /// its arms by column; an expression stops at a line starting at or left
    /// of this column (the next arm).
    col_stop: Option<usize>,
    pub diags: Vec<Diag>,
}

pub fn parse(src: &str) -> (Module, Vec<Diag>) {
    let (toks, mut diags) = Lexer::new(src).tokenize();
    let mut p = Parser { src, toks, pos: 0, indent: 0, col_stop: None, diags: Vec::new() };
    let m = p.module();
    diags.extend(p.diags);
    for d in diags.iter_mut().filter(|d| d.code == "E0110") {
        if brace_quote_before(src, d.span.start as usize) {
            d.msg = "a `{` right before `\"` is a literal brace, so the string ended there; inside `{...}`, quote strings with `'`: \"{'-' * n}\"".into();
        }
    }
    (m, diags)
}

/// Does the line before `pos` have `{"` with a `}` after it, the sign of
/// `"{"x" * n}"`, where the lexer takes the `{` as a literal brace?
fn brace_quote_before(src: &str, pos: usize) -> bool {
    let pos = pos.min(src.len());
    let line_start = src[..pos].rfind('\n').map_or(0, |i| i + 1);
    let line_end = src[pos..].find('\n').map_or(src.len(), |i| pos + i);
    let line = &src[line_start..line_end];
    let before = pos - line_start;
    line.match_indices("{\"").any(|(i, _)| i < before && line[i + 2..].contains('}'))
}

fn is_upper(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_uppercase())
}

/// `User` or `T`, but not a constant like `MAX`.
fn is_type_name(s: &str) -> bool {
    is_upper(s) && (s.len() == 1 || s.chars().any(|c| c.is_ascii_lowercase()))
}

impl<'a> Parser<'a> {
    // ----- token helpers -----

    fn peek(&self) -> &Tok {
        &self.toks[self.pos].tok
    }

    fn peek_at(&self, n: usize) -> &Tok {
        let i = (self.pos + n).min(self.toks.len() - 1);
        &self.toks[i].tok
    }

    fn tok(&self) -> &Token {
        &self.toks[self.pos]
    }

    fn span(&self) -> Span {
        self.toks[self.pos].span
    }

    fn prev_span(&self) -> Span {
        self.toks[self.pos.saturating_sub(1)].span
    }

    fn prev_is_dedent(&self) -> bool {
        self.pos > 0 && matches!(self.toks[self.pos - 1].tok, Tok::Dedent)
    }

    fn bump(&mut self) -> Token {
        let t = self.toks[self.pos].clone();
        match t.tok {
            Tok::Indent => self.indent += 1,
            Tok::Dedent => self.indent -= 1,
            _ => {}
        }
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }

    fn at(&self, t: &Tok) -> bool {
        std::mem::discriminant(self.peek()) == std::mem::discriminant(t)
    }

    fn at_ident(&self, name: &str) -> bool {
        matches!(self.peek(), Tok::Ident(s) if s == name)
    }

    fn ident_at(&self, n: usize) -> Option<&str> {
        match self.peek_at(n) {
            Tok::Ident(s) => Some(s),
            _ => None,
        }
    }

    fn eat(&mut self, t: &Tok) -> bool {
        if self.at(t) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn err(&mut self, span: Span, msg: impl Into<String>) {
        self.diags.push(Diag::new("E0110", span, msg));
    }

    fn hint(&mut self, code: &'static str, span: Span, msg: impl Into<String>, fix: Option<String>) {
        let mut d = Diag::new(code, span, msg);
        d.fix = fix;
        self.diags.push(d);
    }

    fn expected(&mut self, what: &str) {
        let found = describe(self.peek());
        let span = self.span();
        if matches!(self.peek(), Tok::FatArrow) {
            self.hint("E0129", span, "lambdas are `|x| expr`, or use `it`: `xs.map(it + 1)`", None);
        } else if self.at_ident("is") {
            self.hint("E0110", span, "no `is`; compare with `==` / `!=`, e.g. `x == none`", None);
        } else if self.at_ident("then") || self.at_ident("do") {
            self.hint("E0123", span, "blocks start with `:`, e.g. `if x > 0: ...`", None);
        } else if matches!(self.peek(), Tok::LBrace) && (what.contains("`:`") || what.contains("`=`")) {
            self.hint("E0123", span, "blocks use `:` and indentation, not braces", None);
        } else {
            self.err(span, format!("expected {what}, found {found}"));
        }
    }

    fn expect(&mut self, t: &Tok, what: &str) -> P<Span> {
        if self.at(t) {
            Ok(self.bump().span)
        } else {
            self.expected(what);
            Err(())
        }
    }

    fn ident(&mut self, what: &str) -> P<Ident> {
        match self.peek().clone() {
            Tok::Ident(name) => {
                let span = self.bump().span;
                Ok(Ident { name, span })
            }
            _ => {
                self.expected(what);
                Err(())
            }
        }
    }

    /// Identifier, also accepting keywords (for `x.type`, `x.match`).
    fn name_after_dot(&mut self) -> P<Ident> {
        let t = self.tok().clone();
        let name = match &t.tok {
            Tok::Ident(s) => s.clone(),
            Tok::Int(n) => n.to_string(),
            other => {
                let s = crate::lexer::symbol(other);
                if s.chars().all(|c| c.is_ascii_alphabetic()) {
                    s.to_string()
                } else {
                    self.expected("a field or method name after `.`");
                    return Err(());
                }
            }
        };
        self.bump();
        Ok(Ident { name, span: t.span })
    }

    fn line_of(&self, span: Span) -> &'a str {
        let start = self.src[..span.start as usize].rfind('\n').map_or(0, |i| i + 1);
        let end = self.src[span.start as usize..].find('\n').map_or(self.src.len(), |i| span.start as usize + i);
        self.src[start..end].trim()
    }

    /// The comment line directly above `span`, used as a one-line doc.
    fn doc_above(&self, span: Span) -> Option<String> {
        let before = &self.src[..span.start as usize];
        let line_start = before.rfind('\n')?;
        let prev = before[..line_start].rsplit('\n').next()?.trim();
        prev.strip_prefix('#').map(|d| d.trim().to_string())
    }

    // ----- recovery -----

    fn sync_item(&mut self) {
        loop {
            if self.at(&Tok::Eof) {
                return;
            }
            let t = self.bump();
            if matches!(t.tok, Tok::Newline | Tok::Dedent) && self.indent <= 0 && !matches!(self.peek(), Tok::Indent | Tok::Dedent) {
                self.indent = 0;
                return;
            }
        }
    }

    fn sync_stmt(&mut self, level: i32) {
        loop {
            if self.at(&Tok::Eof) || (self.at(&Tok::Dedent) && self.indent == level) {
                return;
            }
            let t = self.bump();
            if self.indent < level {
                return;
            }
            // A newline, or the end of a nested block, back at this level
            // starts the next statement.
            if matches!(t.tok, Tok::Newline | Tok::Dedent) && self.indent == level && !self.at(&Tok::Indent) && !self.at(&Tok::Dedent) {
                return;
            }
        }
    }

    /// Consumes the end of a statement: a newline, `;`, or nothing when the
    /// statement ended with a block.
    fn stmt_end(&mut self) -> bool {
        if self.prev_is_dedent() {
            return true;
        }
        match self.peek() {
            Tok::Newline | Tok::Semi => {
                self.bump();
                true
            }
            Tok::Dedent | Tok::Eof => true,
            _ => false,
        }
    }

    fn end_or_err(&mut self, level: i32) {
        if !self.stmt_end() {
            match self.peek().clone() {
                Tok::PlusPlus | Tok::MinusMinus => {
                    let op = if self.at(&Tok::PlusPlus) { "+=" } else { "-=" };
                    self.hint("E0127", self.span(), format!("no `++`/`--`; write `x {op} 1`"), None);
                }
                Tok::ColonColon => self.hint("E0125", self.span(), "no `::` paths; use `.`, e.g. `Shape.Circle`", None),
                Tok::LBrace => self.hint("E0123", self.span(), "blocks use `:` and indentation, not braces", None),
                Tok::SlashSlash => self.slash_slash(),
                Tok::Bang if matches!(self.peek_at(1), Tok::LParen) => {
                    self.hint("E0130", self.span(), "no macros; use `print(\"...{x}...\")`", None)
                }
                _ => self.expected("end of line"),
            }
            self.sync_stmt(level);
        }
    }

    fn slash_slash(&mut self) {
        let sp = self.span();
        self.hint("E0124", sp, "`//` is not an operator: comments start with `#`; integer `/` already truncates", None);
    }

    // ----- module and items -----

    fn module(&mut self) -> Module {
        let mut m = Module::default();
        loop {
            while self.eat(&Tok::Newline) {}
            if self.at(&Tok::Eof) {
                break;
            }
            if self.at(&Tok::Indent) || self.at(&Tok::Dedent) {
                if self.at(&Tok::Indent) {
                    self.err(self.span(), "unexpected indentation");
                }
                self.sync_item();
                continue;
            }
            // `fn name`, `type Name`, `enum Name`: the name, if the item fails.
            let decl = match (self.peek(), self.peek_at(1)) {
                (Tok::Fn | Tok::Type | Tok::Enum, Tok::Ident(n)) => Some((n.clone(), !matches!(self.peek(), Tok::Fn))),
                _ => None,
            };
            match self.item() {
                Ok(item) => {
                    m.items.push(item);
                    if !self.stmt_end() {
                        self.expected("end of line");
                        self.sync_item();
                    }
                }
                Err(()) => {
                    if let Some((name, is_type)) = decl {
                        m.broken.push(name);
                        m.broken_type |= is_type;
                    }
                    self.sync_item()
                }
            }
        }
        m
    }

    fn item(&mut self) -> P<Item> {
        let start = self.span();
        match self.peek().clone() {
            Tok::Fn => self.fn_decl().map(Item::Fn),
            Tok::Type => self.type_decl(),
            Tok::Enum => {
                self.bump();
                self.enum_decl(start).map(Item::Enum)
            }
            Tok::Ident(name) => {
                let next = self.peek_at(1).clone();
                match (name.as_str(), &next) {
                    ("test", Tok::Str(_)) => {
                        self.bump();
                        let Tok::Str(parts) = self.bump().tok else { unreachable!() };
                        let name = parts
                            .iter()
                            .map(|p| match p {
                                StrPart::Lit(s) => s.clone(),
                                StrPart::Expr { span, .. } => format!("{{{}}}", &self.src[span.start as usize..span.end as usize]),
                            })
                            .collect();
                        self.expect(&Tok::Colon, "`:` after the test name")?;
                        let body = self.block()?;
                        Ok(Item::Test(TestDecl { name, body, span: start }))
                    }
                    ("def" | "func" | "fun" | "function", Tok::Ident(_)) => {
                        let line = self.line_of(start).replacen(&name, "fn", 1);
                        let fix = match line.strip_suffix(':').or_else(|| line.strip_suffix('{')) {
                            Some(head) => format!("{} =", head.trim_end()),
                            None => line,
                        };
                        self.hint("E0121", start, "functions are declared with `fn name(param Type) Ret =`", Some(fix));
                        Err(())
                    }
                    ("class" | "struct" | "interface" | "record" | "data", Tok::Ident(_)) => {
                        self.hint("E0122", start, "types are declared with `type Name {field Type, ...}`", None);
                        Err(())
                    }
                    ("impl" | "trait", _) => {
                        self.hint(
                            "E0136",
                            start,
                            "no impl or trait blocks; a method is a plain fn whose first parameter is the receiver: `fn area(s Shape) f64`, called as `s.area()`",
                            None,
                        );
                        Err(())
                    }
                    ("let" | "const" | "static", _) => {
                        self.let_hint(start);
                        Err(())
                    }
                    ("use" | "import" | "from" | "package" | "mod" | "include", _) => {
                        self.hint("E0134", start, "no imports: the standard library is always in scope", None);
                        Err(())
                    }
                    (_, Tok::Assign) => {
                        let name = self.ident("a name")?;
                        self.bump();
                        let value = self.expr()?;
                        Ok(Item::Const(ConstDecl { name, value }))
                    }
                    _ => {
                        self.hint(
                            "E0134",
                            start,
                            "statements are not allowed at top level; put them in `fn main() =` (it runs automatically)",
                            None,
                        );
                        Err(())
                    }
                }
            }
            Tok::Str(_) => {
                self.err(start, "a test needs the `test` keyword: test \"name\": expr");
                Err(())
            }
            _ => {
                self.expected("a declaration (`fn`, `type`, `enum` or `test`)");
                Err(())
            }
        }
    }

    fn let_hint(&mut self, span: Span) {
        let line = self.line_of(span);
        let fix = line
            .replacen("let mut ", "var ", 1)
            .replacen("let ", "", 1)
            .replacen("const ", "", 1)
            .replacen("static ", "", 1);
        self.hint("E0120", span, "no `let`: write `x = 1` (immutable) or `var x = 0` (mutable)", Some(fix));
    }

    fn generics(&mut self) -> P<Vec<Generic>> {
        let mut gs = Vec::new();
        if self.at(&Tok::Lt) {
            self.hint("E0131", self.span(), "generic parameters use brackets: `fn max[T](a T, b T) T`", None);
            return Err(());
        }
        if !self.eat(&Tok::LBracket) {
            return Ok(gs);
        }
        while !self.at(&Tok::RBracket) {
            let name = self.ident("a type parameter")?;
            let mut bounds = Vec::new();
            if self.eat(&Tok::Colon) {
                bounds.push(self.ident("a trait name")?);
                while self.eat(&Tok::Plus) {
                    bounds.push(self.ident("a trait name")?);
                }
            }
            gs.push(Generic { name, bounds });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RBracket, "`]`")?;
        Ok(gs)
    }

    fn fn_decl(&mut self) -> P<FnDecl> {
        let start = self.bump().span;
        let doc = self.doc_above(start);
        let name = self.ident("a function name")?;
        let generics = self.generics()?;
        self.expect(&Tok::LParen, "`(`")?;
        let mut params = Vec::new();
        while !self.at(&Tok::RParen) {
            params.push(self.param()?);
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RParen, "`)` after parameters")?;
        let ret = if self.eat(&Tok::Arrow) {
            Some(self.ty()?)
        } else if self.at(&Tok::Assign) || self.at(&Tok::Colon) || (self.at(&Tok::LBrace) && !self.brace_is_type()) || self.at(&Tok::Newline) {
            None
        } else {
            Some(self.ty()?)
        };
        if self.at(&Tok::LBrace) {
            self.hint("E0123", self.span(), "function bodies follow `=` and are indented, not braced", None);
            return Err(());
        }
        if !self.eat(&Tok::Assign) && !self.eat(&Tok::Colon) {
            self.expected("`=` before the function body");
            return Err(());
        }
        let body = self.block()?;
        Ok(FnDecl { name, generics, params, ret, body, doc, span: start })
    }

    /// After a parameter list, is `{` a map or set return type (`{str: int} =`)
    /// rather than a braced body? The type is followed by `=`, `!` or `?`.
    fn brace_is_type(&self) -> bool {
        let mut depth = 0;
        let mut i = 0;
        loop {
            match self.peek_at(i) {
                Tok::LBrace | Tok::LBracket | Tok::LParen => depth += 1,
                Tok::RBrace | Tok::RBracket | Tok::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                Tok::Newline | Tok::Eof => return false,
                _ => {}
            }
            i += 1;
        }
        matches!(self.peek_at(i + 1), Tok::Assign | Tok::Bang | Tok::Question)
    }

    fn param(&mut self) -> P<Param> {
        let mut mode = Mode::Borrow;
        if self.at(&Tok::Amp) {
            self.hint("E0132", self.span(), "no references: parameters are borrowed by default; write `mut x T` to modify in place", None);
            return Err(());
        }
        if let (Some(m @ ("mut" | "own")), Some(_)) = (self.ident_at(0), self.ident_at(1)) {
            mode = if m == "mut" { Mode::Mut } else { Mode::Own };
            self.bump();
        }
        if self.at_ident("self") {
            self.hint("E0136", self.span(), "no `self`: name the receiver like any parameter, e.g. `fn area(s Shape) f64`", None);
            return Err(());
        }
        let name = self.ident("a parameter name")?;
        self.eat(&Tok::Colon);
        if let Some(m @ ("mut" | "own")) = self.ident_at(0) {
            if mode == Mode::Borrow && matches!(self.peek_at(1), Tok::Ident(_) | Tok::LBracket | Tok::LBrace | Tok::LParen | Tok::Fn) {
                mode = if m == "mut" { Mode::Mut } else { Mode::Own };
                self.bump();
            }
        }
        if self.at(&Tok::Comma) || self.at(&Tok::RParen) {
            self.hint("E0140", name.span, format!("parameter `{}` needs a type: `{} int`", name.name, name.name), None);
            return Err(());
        }
        let ty = self.ty()?;
        let default = if self.eat(&Tok::Assign) { Some(self.expr()?) } else { None };
        Ok(Param { mode, name, ty, default })
    }

    fn type_decl(&mut self) -> P<Item> {
        let start = self.bump().span;
        let doc = self.doc_above(start);
        let name = self.ident("a type name")?;
        let generics = self.generics()?;
        if self.eat(&Tok::Assign) {
            // `type Shape = Circle(f64) | Rect(f64, f64)` is an enum.
            let is_enum = self.at(&Tok::Pipe)
                || self.at(&Tok::Newline)
                || (matches!(self.peek(), Tok::Ident(_)) && matches!(self.peek_at(1), Tok::LParen | Tok::Pipe));
            if is_enum {
                let variants = self.variants()?;
                return Ok(Item::Enum(EnumDecl { name, generics, variants, doc, span: start }));
            }
            let ty = self.ty()?;
            return Ok(Item::Alias(AliasDecl { name, ty, span: start }));
        }
        let mut fields = Vec::new();
        if self.eat(&Tok::Colon) {
            // Indented form, one field per line.
            self.expect(&Tok::Newline, "a newline")?;
            self.expect(&Tok::Indent, "indented fields")?;
            while !self.at(&Tok::Dedent) && !self.at(&Tok::Eof) {
                if self.eat(&Tok::Newline) {
                    continue;
                }
                fields.push(self.field()?);
                self.eat(&Tok::Comma);
                self.eat(&Tok::Newline);
            }
            self.bump();
        } else {
            self.expect(&Tok::LBrace, "`{` and the fields")?;
            while !self.at(&Tok::RBrace) {
                fields.push(self.field()?);
                if !self.eat(&Tok::Comma) && !self.tok().nl_before {
                    break;
                }
            }
            self.expect(&Tok::RBrace, "`}` or `,` between fields")?;
        }
        Ok(Item::Struct(StructDecl { name, generics, fields, doc, span: start }))
    }

    fn field(&mut self) -> P<FieldDecl> {
        // `var x int` / `mut x int` / `pub x int`: fields are mutable through
        // a `var` binding and always visible, so the marker means nothing.
        let marker = self.at(&Tok::Var) || matches!(self.ident_at(0), Some("mut" | "pub"));
        if marker && matches!(self.peek_at(1), Tok::Ident(_)) && matches!(self.peek_at(2), Tok::Ident(_) | Tok::LBracket | Tok::LBrace | Tok::LParen | Tok::Colon | Tok::Fn) {
            self.bump();
        }
        let name = self.ident("a field name")?;
        self.eat(&Tok::Colon);
        let ty = self.ty()?;
        let default = if self.eat(&Tok::Assign) { Some(self.expr()?) } else { None };
        Ok(FieldDecl { name, ty, default })
    }

    fn enum_decl(&mut self, start: Span) -> P<EnumDecl> {
        let doc = self.doc_above(start);
        let name = self.ident("an enum name")?;
        let generics = self.generics()?;
        let variants = if self.eat(&Tok::LBrace) {
            let mut vs = Vec::new();
            while !self.at(&Tok::RBrace) {
                vs.push(self.variant()?);
                if !self.eat(&Tok::Comma) && !self.eat(&Tok::Pipe) && !self.tok().nl_before {
                    break;
                }
            }
            self.expect(&Tok::RBrace, "`}`")?;
            vs
        } else {
            if !self.eat(&Tok::Assign) && !self.eat(&Tok::Colon) {
                self.expected("`=` and the variants");
                return Err(());
            }
            self.variants()?
        };
        Ok(EnumDecl { name, generics, variants, doc, span: start })
    }

    fn variants(&mut self) -> P<Vec<Variant>> {
        let mut vs = Vec::new();
        if self.eat(&Tok::Newline) {
            self.expect(&Tok::Indent, "indented variants")?;
            while !self.at(&Tok::Dedent) && !self.at(&Tok::Eof) {
                if self.eat(&Tok::Newline) || self.eat(&Tok::Comma) {
                    continue;
                }
                self.eat(&Tok::Pipe);
                vs.push(self.variant()?);
                while self.eat(&Tok::Pipe) || self.eat(&Tok::Comma) {
                    if self.at(&Tok::Newline) {
                        break;
                    }
                    vs.push(self.variant()?);
                }
            }
            self.bump();
        } else {
            self.eat(&Tok::Pipe);
            vs.push(self.variant()?);
            while self.eat(&Tok::Pipe) {
                vs.push(self.variant()?);
            }
        }
        Ok(vs)
    }

    fn variant(&mut self) -> P<Variant> {
        let name = self.ident("a variant name")?;
        let mut fields = Vec::new();
        if self.eat(&Tok::LParen) {
            while !self.at(&Tok::RParen) {
                // Allow `Rect(w f64, h f64)`: names are documentation only.
                if matches!(self.peek(), Tok::Ident(_)) && matches!(self.peek_at(1), Tok::Ident(_) | Tok::Colon | Tok::LBracket | Tok::LBrace | Tok::LParen | Tok::Fn)
                    && !matches!(self.peek_at(1), Tok::LBracket) {
                        self.bump();
                        self.eat(&Tok::Colon);
                    }
                fields.push(self.ty()?);
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(&Tok::RParen, "`)`")?;
        } else if self.at(&Tok::LBrace) {
            self.hint("E0141", self.span(), "variants carry positional fields: `Rect(f64, f64)`", None);
            return Err(());
        }
        Ok(Variant { name, fields })
    }

    // ----- types -----

    fn ty(&mut self) -> P<TypeExpr> {
        let start = self.span();
        let mut t = match self.peek().clone() {
            Tok::Bang => TypeExpr::unit(),
            Tok::Ident(_) => {
                let id = self.ident("a type")?;
                let mut args = Vec::new();
                if self.at(&Tok::Lt) {
                    self.hint(
                        "E0131",
                        self.span(),
                        "generic arguments use brackets, and most std types have short forms: [T] list, {K:V} map, T? optional".to_string(),
                        None,
                    );
                    return Err(());
                }
                if self.eat(&Tok::LBracket) {
                    while !self.at(&Tok::RBracket) {
                        args.push(self.ty()?);
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    self.expect(&Tok::RBracket, "`]`")?;
                }
                TypeExpr::Name(id, args)
            }
            Tok::LBracket => {
                self.bump();
                let inner = self.ty()?;
                if self.eat(&Tok::Semi) {
                    self.err(self.span(), "no fixed-size arrays; use a list `[T]`");
                    return Err(());
                }
                let end = self.expect(&Tok::RBracket, "`]`")?;
                TypeExpr::List(Box::new(inner), start.to(end))
            }
            Tok::LBrace => {
                self.bump();
                let k = self.ty()?;
                if self.eat(&Tok::Colon) {
                    let v = self.ty()?;
                    let end = self.expect(&Tok::RBrace, "`}`")?;
                    TypeExpr::Map(Box::new(k), Box::new(v), start.to(end))
                } else {
                    let end = self.expect(&Tok::RBrace, "`}`")?;
                    TypeExpr::Set(Box::new(k), start.to(end))
                }
            }
            Tok::LParen => {
                self.bump();
                let mut ts = Vec::new();
                while !self.at(&Tok::RParen) {
                    ts.push(self.ty()?);
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                let end = self.expect(&Tok::RParen, "`)`")?;
                if ts.len() == 1 {
                    ts.pop().unwrap()
                } else {
                    TypeExpr::Tuple(ts, start.to(end))
                }
            }
            Tok::Fn => {
                self.bump();
                self.expect(&Tok::LParen, "`(`")?;
                let mut ps = Vec::new();
                while !self.at(&Tok::RParen) {
                    ps.push(self.ty()?);
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                let end = self.expect(&Tok::RParen, "`)`")?;
                self.eat(&Tok::Arrow);
                let ret = if self.starts_type() && !self.tok().nl_before { Some(Box::new(self.ty()?)) } else { None };
                TypeExpr::Fn(ps, ret, start.to(end))
            }
            Tok::Amp => {
                self.hint("E0132", start, "no references: `str` is the only string type and values are passed directly", None);
                return Err(());
            }
            _ => {
                self.expected("a type");
                return Err(());
            }
        };
        loop {
            if self.eat(&Tok::Question) {
                t = TypeExpr::Optional(Box::new(t));
            } else if self.at(&Tok::Bang) {
                self.bump();
                let err = if matches!(self.peek(), Tok::Ident(_)) && !self.tok().nl_before && !matches!(self.peek_at(1), Tok::Colon) {
                    Some(Box::new(self.ty()?))
                } else {
                    None
                };
                t = TypeExpr::Result(Box::new(t), err);
            } else {
                break;
            }
        }
        Ok(t)
    }

    fn starts_type(&self) -> bool {
        matches!(self.peek(), Tok::Ident(_) | Tok::LBracket | Tok::LBrace | Tok::LParen | Tok::Fn | Tok::Bang)
    }

    // ----- blocks and statements -----

    /// A block after `:` or `=`: either the rest of the line or an indented
    /// block on the following lines.
    fn block(&mut self) -> P<Block> {
        let start = self.span();
        if self.at(&Tok::LBrace) && self.brace_block_ahead() {
            self.hint("E0123", start, "blocks use `:` and indentation, not braces", None);
            return Err(());
        }
        if self.eat(&Tok::Newline) {
            if !self.at(&Tok::Indent) {
                self.expected("an indented block");
                return Err(());
            }
            self.bump();
            let level = self.indent;
            let mut stmts = Vec::new();
            loop {
                if self.indent < level {
                    break;
                }
                match self.peek() {
                    Tok::Dedent => {
                        self.bump();
                        break;
                    }
                    Tok::Eof => break,
                    Tok::Newline => {
                        self.bump();
                        continue;
                    }
                    _ => {}
                }
                match self.stmt() {
                    Ok(s) => {
                        stmts.push(s);
                        self.end_or_err(level);
                    }
                    Err(()) => self.sync_stmt(level),
                }
            }
            Ok(Block { stmts, span: start.to(self.prev_span()) })
        } else {
            let mut stmts = vec![self.stmt()?];
            while self.at(&Tok::Semi) && !matches!(self.peek_at(1), Tok::Newline | Tok::Eof | Tok::Dedent) {
                self.bump();
                stmts.push(self.stmt()?);
            }
            Ok(Block { stmts, span: start.to(self.prev_span()) })
        }
    }

    /// `{` opening a brace block (`if x {` + newline) rather than a map, set
    /// or struct literal: the braces span lines and hold no top-level `,`/`:`.
    fn brace_block_ahead(&self) -> bool {
        let mut i = self.pos + 1;
        let mut depth = 1;
        let mut multiline = false;
        while i < self.toks.len() {
            if self.toks[i].nl_before && depth == 1 {
                multiline = true;
            }
            match self.toks[i].tok {
                Tok::LBrace | Tok::LParen | Tok::LBracket => depth += 1,
                Tok::RParen | Tok::RBracket => depth -= 1,
                Tok::RBrace => {
                    depth -= 1;
                    if depth == 0 {
                        return multiline;
                    }
                }
                Tok::Comma | Tok::Colon if depth == 1 => return false,
                Tok::Eof => return multiline,
                _ => {}
            }
            i += 1;
        }
        multiline
    }

    fn stmt(&mut self) -> P<Stmt> {
        let start = self.span();
        match self.peek().clone() {
            Tok::Var => {
                self.bump();
                if self.at_ident("mut") {
                    self.bump();
                }
                let name = self.ident("a variable name")?;
                self.eat(&Tok::Colon);
                let ty = if self.at(&Tok::Assign) { None } else { Some(self.ty()?) };
                self.expect(&Tok::Assign, "`=` and an initial value")?;
                let value = self.rhs()?;
                Ok(Stmt::Var { name, ty, value, mutable: true })
            }
            Tok::Fn if matches!(self.peek_at(1), Tok::Ident(_)) => {
                self.hint("E0121", start, "no nested functions; move it to the top level, or bind a lambda: `add = |a, b| a + b`", None);
                Err(())
            }
            Tok::For => self.for_stmt(),
            Tok::While => {
                self.bump();
                if self.at_ident("let") {
                    self.hint("E0147", start, "no `while let`; write `while !stack.is_empty():` and then `x = stack.pop().unwrap()`", None);
                    return Err(());
                }
                let cond = self.expr()?;
                self.block_colon()?;
                let body = self.block()?;
                Ok(Stmt::While { cond, body, span: start })
            }
            Tok::Return => {
                self.bump();
                let value = if self.starts_expr() { Some(self.rhs()?) } else { None };
                Ok(Stmt::Return(value, start))
            }
            Tok::Break => {
                self.bump();
                Ok(Stmt::Break(start))
            }
            Tok::Continue => {
                self.bump();
                Ok(Stmt::Continue(start))
            }
            Tok::Fail => {
                self.bump();
                let e = self.expr()?;
                Ok(Stmt::Fail(e, start))
            }
            Tok::Assert => {
                self.bump();
                let cond = self.expr()?;
                let msg = if self.eat(&Tok::Comma) { Some(self.expr()?) } else { None };
                Ok(Stmt::Assert { cond, msg, span: start })
            }
            Tok::Ident(name) => {
                let next = self.peek_at(1).clone();
                match (name.as_str(), &next) {
                    ("let" | "const", Tok::Ident(_)) => {
                        self.let_hint(start);
                        Err(())
                    }
                    ("loop", Tok::Colon) => {
                        self.bump();
                        self.bump();
                        let body = self.block()?;
                        Ok(Stmt::While { cond: Expr { kind: ExprKind::Bool(true), span: start }, body, span: start })
                    }
                    ("try", Tok::Colon | Tok::LBrace) | ("raise" | "throw", _) => {
                        self.hint(
                            "E0128",
                            start,
                            "no exceptions: return `T!`, raise with `fail \"msg\"`, propagate with `?`, handle with `x ?? default` or `match r` + `err(e):`",
                            None,
                        );
                        Err(())
                    }
                    ("def" | "func" | "fun" | "function", Tok::Ident(_)) => {
                        self.hint("E0121", start, "nested functions aren't supported; use a lambda `|x| expr` or a top-level fn", None);
                        Err(())
                    }
                    // Python's empty statement means the same here, so it is accepted.
                    ("pass", Tok::Newline | Tok::Dedent | Tok::Eof) => {
                        self.bump();
                        Ok(Stmt::Expr(Expr { kind: ExprKind::Tuple(vec![]), span: start }))
                    }
                    ("del", Tok::Ident(_)) => {
                        let fix = del_fix(self.line_of(start));
                        self.hint("E0148", start, "no `del`: remove a map entry with `m.remove(k)`, a list item with `xs.remove(i)`", fix);
                        Err(())
                    }
                    _ if self.typed_binding_ahead() => {
                        let name = self.ident("a variable name")?;
                        self.eat(&Tok::Colon);
                        let ty = self.ty()?;
                        self.expect(&Tok::Assign, "`=` and a value")?;
                        let value = self.rhs()?;
                        Ok(Stmt::Var { name, ty: Some(ty), value, mutable: false })
                    }
                    _ => self.simple_stmt(),
                }
            }
            _ => self.simple_stmt(),
        }
    }

    /// `n int = v` or `xs: [int] = []`: a name, a type and `=`. Without the
    /// colon the type can't start with `[` or `(`, since `x [i] = 1` and
    /// `x (y) = 1` mean something else.
    fn typed_binding_ahead(&self) -> bool {
        let colon = matches!(self.peek_at(1), Tok::Colon);
        if !colon && !matches!(self.peek_at(1), Tok::Ident(_) | Tok::LBrace) {
            return false;
        }
        let start = if colon { 2 } else { 1 };
        let mut depth = 0;
        let mut i = start;
        loop {
            match self.peek_at(i) {
                Tok::Assign => return depth == 0 && i > start,
                Tok::Ident(_) | Tok::Question | Tok::Bang | Tok::Fn | Tok::Arrow => {}
                Tok::LBracket | Tok::LBrace | Tok::LParen => depth += 1,
                Tok::RBracket | Tok::RBrace | Tok::RParen if depth > 0 => depth -= 1,
                Tok::Comma | Tok::Colon if depth > 0 => {}
                _ => return false,
            }
            i += 1;
        }
    }

    fn block_colon(&mut self) -> P<()> {
        if self.eat(&Tok::Colon) {
            return Ok(());
        }
        if self.at(&Tok::LBrace) {
            self.hint("E0123", self.span(), "blocks use `:` and indentation, not braces", None);
        } else {
            self.expected("`:`");
        }
        Err(())
    }

    fn simple_stmt(&mut self) -> P<Stmt> {
        let start = self.span();
        let first = self.expr()?;
        let mut targets = vec![first];
        // Inside brackets a one-line branch is one expression, and a comma
        // belongs to the enclosing tuple, call or list: `(if c: a else: b, i)`.
        while self.at(&Tok::Comma) && !self.tok().in_brackets {
            self.bump();
            targets.push(self.expr()?);
        }
        let op = match self.peek() {
            Tok::Assign => None,
            Tok::PlusEq => Some(BinOp::Add),
            Tok::MinusEq => Some(BinOp::Sub),
            Tok::StarEq => Some(BinOp::Mul),
            Tok::SlashEq => Some(BinOp::Div),
            Tok::PercentEq => Some(BinOp::Rem),
            Tok::ColonEq => {
                let fix = self.line_of(start).replacen(":=", "=", 1);
                self.hint("E0126", self.span(), "no `:=`; write `x = 1` (immutable) or `var x = 0` (mutable)", Some(fix));
                return Err(());
            }
            Tok::Colon if targets.len() == 1 && matches!(targets[0].kind, ExprKind::Name(_)) => {
                self.hint("E0135", self.span(), "a binding needs a value: `x = value`, or `x T = value` to declare its type", None);
                return Err(());
            }
            _ => {
                if targets.len() > 1 {
                    // `a, b` as an expression statement: a tuple.
                    let span = start.to(self.prev_span());
                    return Ok(Stmt::Expr(Expr { kind: ExprKind::Tuple(targets), span }));
                }
                return Ok(Stmt::Expr(targets.pop().unwrap()));
            }
        };
        self.bump();
        let value = self.rhs()?;
        Ok(Stmt::Assign { targets, op, value, span: start })
    }

    /// Right-hand side: an expression, or `a, b` as a tuple.
    fn rhs(&mut self) -> P<Expr> {
        let first = self.expr()?;
        if !self.at(&Tok::Comma) {
            return Ok(first);
        }
        let start = first.span;
        let mut items = vec![first];
        while self.eat(&Tok::Comma) {
            if !self.starts_expr() {
                break;
            }
            items.push(self.expr()?);
        }
        Ok(Expr { kind: ExprKind::Tuple(items), span: start.to(self.prev_span()) })
    }

    fn for_stmt(&mut self) -> P<Stmt> {
        let start = self.bump().span;
        if self.at(&Tok::LParen) && self.c_style_for_ahead() {
            self.hint("E0142", start, "no C-style for; write `for i in 0..n:`", None);
            return Err(());
        }
        let first = self.pattern_single()?;
        let pat = if self.at(&Tok::Comma) {
            let mut ps = vec![first];
            while self.eat(&Tok::Comma) {
                ps.push(self.pattern_single()?);
            }
            Pat::Tuple(ps, start)
        } else {
            first
        };
        self.expect(&Tok::In, "`in`")?;
        let iter = self.expr()?;
        self.block_colon()?;
        let body = self.block()?;
        Ok(Stmt::For { pat, iter, body, span: start })
    }

    fn c_style_for_ahead(&self) -> bool {
        let mut i = self.pos;
        while i < self.toks.len() && !matches!(self.toks[i].tok, Tok::Newline | Tok::Eof) {
            if matches!(self.toks[i].tok, Tok::Semi) {
                return true;
            }
            i += 1;
        }
        false
    }

    // ----- expressions -----

    fn starts_expr(&self) -> bool {
        matches!(
            self.peek(),
            Tok::Ident(_)
                | Tok::Int(_)
                | Tok::Float(_)
                | Tok::Str(_)
                | Tok::LParen
                | Tok::LBracket
                | Tok::LBrace
                | Tok::Minus
                | Tok::Bang
                | Tok::Not
                | Tok::True
                | Tok::False
                | Tok::None
                | Tok::If
                | Tok::Match
                | Tok::Pipe
                | Tok::PipePipe
                | Tok::DotDot
                | Tok::DotDotEq
        )
    }

    pub fn expr(&mut self) -> P<Expr> {
        let e = self.or_expr()?;
        // Python's `a if cond else b`.
        if self.at(&Tok::If) && !self.prev_is_dedent() {
            self.bump();
            let cond = self.or_expr()?;
            self.expect(&Tok::Else, "`else` in `a if cond else b`")?;
            let other = self.expr()?;
            let span = e.span.to(other.span);
            return Ok(Expr {
                kind: ExprKind::If {
                    cond: Box::new(cond),
                    then: Block { span: e.span, stmts: vec![Stmt::Expr(e)] },
                    els: Some(Block { span: other.span, stmts: vec![Stmt::Expr(other)] }),
                },
                span,
            });
        }
        Ok(e)
    }

    fn stop(&self) -> bool {
        self.prev_is_dedent() || self.col_stop.is_some_and(|c| self.tok().nl_before && self.col(self.tok().span) <= c)
    }

    /// 0-based column of a span's start.
    fn col(&self, span: Span) -> usize {
        let start = span.start as usize;
        let line_start = self.src[..start].rfind('\n').map_or(0, |i| i + 1);
        self.src[line_start..start].chars().count()
    }

    fn or_expr(&mut self) -> P<Expr> {
        let mut e = self.and_expr()?;
        while !self.stop() && (self.at(&Tok::Or) || self.at(&Tok::PipePipe)) {
            self.bump();
            let r = self.and_expr()?;
            let span = e.span.to(r.span);
            e = Expr { kind: ExprKind::Or(Box::new(e), Box::new(r)), span };
        }
        Ok(e)
    }

    fn and_expr(&mut self) -> P<Expr> {
        let mut e = self.not_expr()?;
        while !self.stop() && (self.at(&Tok::And) || self.at(&Tok::AmpAmp)) {
            self.bump();
            let r = self.not_expr()?;
            let span = e.span.to(r.span);
            e = Expr { kind: ExprKind::And(Box::new(e), Box::new(r)), span };
        }
        Ok(e)
    }

    fn not_expr(&mut self) -> P<Expr> {
        if self.at(&Tok::Not) {
            let start = self.bump().span;
            let e = self.not_expr()?;
            let span = start.to(e.span);
            return Ok(Expr { kind: ExprKind::Unary { op: UnOp::Not, expr: Box::new(e) }, span });
        }
        self.cmp_expr()
    }

    fn cmp_op(&self) -> Option<(CmpOp, usize)> {
        Some(match self.peek() {
            Tok::EqEq => (CmpOp::Eq, 1),
            Tok::Ne => (CmpOp::Ne, 1),
            Tok::Lt => (CmpOp::Lt, 1),
            Tok::Le => (CmpOp::Le, 1),
            Tok::Gt => (CmpOp::Gt, 1),
            Tok::Ge => (CmpOp::Ge, 1),
            Tok::In => (CmpOp::In, 1),
            Tok::Not if matches!(self.peek_at(1), Tok::In) => (CmpOp::NotIn, 2),
            _ => return None,
        })
    }

    fn cmp_expr(&mut self) -> P<Expr> {
        let first = self.coalesce()?;
        let mut rest = Vec::new();
        while !self.stop() {
            let Some((op, n)) = self.cmp_op() else { break };
            for _ in 0..n {
                self.bump();
            }
            rest.push((op, self.coalesce()?));
        }
        if rest.is_empty() {
            return Ok(first);
        }
        let span = first.span.to(rest.last().unwrap().1.span);
        Ok(Expr { kind: ExprKind::Compare { first: Box::new(first), rest }, span })
    }

    fn coalesce(&mut self) -> P<Expr> {
        let e = self.range()?;
        if !self.stop() && self.eat(&Tok::QQ) {
            let r = self.coalesce()?;
            let span = e.span.to(r.span);
            return Ok(Expr { kind: ExprKind::Coalesce(Box::new(e), Box::new(r)), span });
        }
        Ok(e)
    }

    fn range(&mut self) -> P<Expr> {
        if self.at(&Tok::DotDot) || self.at(&Tok::DotDotEq) {
            let inclusive = self.at(&Tok::DotDotEq);
            let start = self.bump().span;
            let end = if self.starts_expr() { Some(Box::new(self.bitor()?)) } else { None };
            let span = start.to(self.prev_span());
            return Ok(Expr { kind: ExprKind::Range { start: None, end, inclusive }, span });
        }
        let e = self.bitor()?;
        if !self.stop() && (self.at(&Tok::DotDot) || self.at(&Tok::DotDotEq)) {
            let inclusive = self.at(&Tok::DotDotEq);
            self.bump();
            let end = if self.starts_expr() && !self.at(&Tok::Pipe) { Some(Box::new(self.bitor()?)) } else { None };
            let span = e.span.to(self.prev_span());
            return Ok(Expr { kind: ExprKind::Range { start: Some(Box::new(e)), end, inclusive }, span });
        }
        Ok(e)
    }

    fn binary_level(&mut self, next: fn(&mut Self) -> P<Expr>, ops: &[(Tok, BinOp)]) -> P<Expr> {
        let mut e = next(self)?;
        'outer: while !self.stop() {
            for (t, op) in ops {
                if self.at(t) {
                    self.bump();
                    let r = next(self)?;
                    let span = e.span.to(r.span);
                    e = Expr { kind: ExprKind::Binary { op: *op, lhs: Box::new(e), rhs: Box::new(r) }, span };
                    continue 'outer;
                }
            }
            break;
        }
        Ok(e)
    }

    fn bitor(&mut self) -> P<Expr> {
        self.binary_level(Self::bitxor, &[(Tok::Pipe, BinOp::BitOr)])
    }

    fn bitxor(&mut self) -> P<Expr> {
        self.binary_level(Self::bitand, &[(Tok::Caret, BinOp::BitXor)])
    }

    fn bitand(&mut self) -> P<Expr> {
        self.binary_level(Self::shift, &[(Tok::Amp, BinOp::BitAnd)])
    }

    fn shift(&mut self) -> P<Expr> {
        self.binary_level(Self::additive, &[(Tok::Shl, BinOp::Shl), (Tok::Shr, BinOp::Shr)])
    }

    fn additive(&mut self) -> P<Expr> {
        self.binary_level(Self::multiplicative, &[(Tok::Plus, BinOp::Add), (Tok::Minus, BinOp::Sub)])
    }

    fn multiplicative(&mut self) -> P<Expr> {
        if self.at(&Tok::SlashSlash) {
            self.slash_slash();
            return Err(());
        }
        let e = self.binary_level(Self::unary, &[(Tok::Star, BinOp::Mul), (Tok::Slash, BinOp::Div), (Tok::Percent, BinOp::Rem)])?;
        if self.at(&Tok::SlashSlash) {
            self.slash_slash();
            return Err(());
        }
        Ok(e)
    }

    fn unary(&mut self) -> P<Expr> {
        let op = match self.peek() {
            Tok::Minus => UnOp::Neg,
            Tok::Bang => UnOp::Bang,
            Tok::Amp => {
                self.hint("E0132", self.span(), "no references: pass the value directly (parameters borrow by default)", None);
                return Err(());
            }
            Tok::Star => {
                self.hint("E0132", self.span(), "no pointers to dereference: use the value directly", None);
                return Err(());
            }
            _ => return self.power(),
        };
        let start = self.bump().span;
        let e = self.unary()?;
        let span = start.to(e.span);
        // Fold negative literals so patterns and constants stay simple.
        if op == UnOp::Neg {
            match e.kind {
                ExprKind::Int(n) => return Ok(Expr { kind: ExprKind::Int(n.wrapping_neg()), span }),
                ExprKind::Float(f) => return Ok(Expr { kind: ExprKind::Float(-f), span }),
                _ => {}
            }
        }
        Ok(Expr { kind: ExprKind::Unary { op, expr: Box::new(e) }, span })
    }

    fn power(&mut self) -> P<Expr> {
        let e = self.cast()?;
        if !self.stop() && self.eat(&Tok::StarStar) {
            let r = self.unary()?;
            let span = e.span.to(r.span);
            return Ok(Expr { kind: ExprKind::Binary { op: BinOp::Pow, lhs: Box::new(e), rhs: Box::new(r) }, span });
        }
        Ok(e)
    }

    fn cast(&mut self) -> P<Expr> {
        let mut e = self.postfix()?;
        while !self.stop() && self.eat(&Tok::As) {
            let ty = self.ty()?;
            let span = e.span.to(self.prev_span());
            e = Expr { kind: ExprKind::Cast { expr: Box::new(e), ty }, span };
        }
        Ok(e)
    }

    fn args(&mut self, close: Tok) -> P<Vec<Expr>> {
        let mut args = Vec::new();
        while !self.at(&close) {
            if let (Tok::Ident(name), Tok::Assign) = (self.peek(), self.peek_at(1)) {
                let msg = match name.as_str() {
                    "end" | "flush" => "no named arguments; to print without a newline use `io.write(s)`",
                    "sep" => "no named arguments; `print(a, b)` separates with a space, or interpolate: `print(\"{a},{b}\")`",
                    "reverse" => "no named arguments; sort descending with `xs.sort_by(-it)` or `xs.sort().rev()`",
                    "key" => "no named arguments; sort by a key with `xs.sort_by(it.age)`, `min_by(...)`, `max_by(...)`",
                    "file" => "no named arguments; print to stderr with `eprint(...)`",
                    _ => "no named arguments; pass arguments by position",
                };
                self.hint("E0138", self.span(), msg, None);
                return Err(());
            }
            // `f(mut x)`, like Swift's `f(&x)`: a `mut` parameter needs no
            // marker at the call site.
            if self.at_ident("mut") && matches!(self.peek_at(1), Tok::Ident(_)) && matches!(self.peek_at(2), Tok::Comma | Tok::RParen | Tok::Dot | Tok::LBracket) {
                self.bump();
            }
            args.push(self.expr()?);
            if self.at(&Tok::For) {
                self.hint("E0133", self.span(), "no generator expressions; use `xs.any(cond)`, `xs.map(expr).sum()` and so on, with `it`", None);
                return Err(());
            }
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&close, &format!("`{}` or `,`", crate::lexer::symbol(&close)))?;
        Ok(args)
    }

    fn postfix(&mut self) -> P<Expr> {
        let mut e = self.primary()?;
        loop {
            if self.stop() {
                break;
            }
            match self.peek() {
                Tok::LParen => {
                    self.bump();
                    let args = self.args(Tok::RParen)?;
                    let span = e.span.to(self.prev_span());
                    e = Expr { kind: ExprKind::Call { callee: Box::new(e), args }, span };
                }
                Tok::LBracket => {
                    self.bump();
                    e = self.index(e)?;
                }
                Tok::Dot => {
                    self.bump();
                    let name = self.name_after_dot()?;
                    if self.at(&Tok::LParen) && !self.tok().nl_before {
                        self.bump();
                        let args = self.args(Tok::RParen)?;
                        let span = e.span.to(self.prev_span());
                        e = Expr { kind: ExprKind::Method { obj: Box::new(e), name, args }, span };
                    } else {
                        let span = e.span.to(name.span);
                        e = Expr { kind: ExprKind::Field { obj: Box::new(e), name }, span };
                    }
                }
                Tok::Question => {
                    let end = self.bump().span;
                    let span = e.span.to(end);
                    e = Expr { kind: ExprKind::Try(Box::new(e)), span };
                }
                Tok::Bang if matches!(self.peek_at(1), Tok::LParen | Tok::LBracket) && matches!(e.kind, ExprKind::Name(_)) => {
                    let ExprKind::Name(name) = &e.kind else { unreachable!() };
                    let msg = match name.as_str() {
                        "vec" => "no macros; lists are literals: `[1, 2, 3]`",
                        "assert" | "assert_eq" | "assert_ne" | "debug_assert" => "no macros; write `assert a == b` (or `assert cond, \"msg\"`)",
                        "panic" | "unreachable" | "todo" | "unimplemented" => "no macros; call `panic(\"msg\")`, or `fail \"msg\"` in a `T!` function",
                        "format" => "no macros; every string interpolates: `\"{x} and {y:.2}\"`",
                        "write" | "writeln" => "no macros; use `print(...)` or `io.write(s)`",
                        _ => "no macros; use `print(\"...{x}...\")` (every string interpolates)",
                    };
                    self.hint("E0130", self.span(), msg, None);
                    return Err(());
                }
                Tok::ColonColon => {
                    let empty = match &e.kind {
                        ExprKind::Name(n) => match n.as_str() {
                            "HashMap" | "BTreeMap" => Some("an empty map is `{}`"),
                            "HashSet" | "BTreeSet" => Some("an empty set is `set()`"),
                            "Vec" | "VecDeque" => Some("an empty list is `[]`"),
                            "String" => Some("an empty string is `\"\"`"),
                            "BinaryHeap" => Some("a min-heap is `heap()`"),
                            _ => None,
                        },
                        _ => None,
                    };
                    if matches!(self.peek_at(1), Tok::Lt) {
                        self.hint("E0125", self.span(), "no turbofish: methods return lists, so `.collect()` and type arguments aren't needed", None);
                    } else if let Some(empty) = empty {
                        self.hint("E0125", self.span(), format!("no `::` paths; {empty}"), None);
                    } else {
                        self.hint("E0125", self.span(), "no `::` paths; use `.`, e.g. `Shape.Circle(1.0)`", None);
                    }
                    return Err(());
                }
                _ => break,
            }
        }
        Ok(e)
    }

    fn index(&mut self, obj: Expr) -> P<Expr> {
        let start = obj.span;
        if self.at(&Tok::ColonColon) {
            self.hint("E0143", self.span(), "no slice steps; use `.rev()` or `.step_by(n)`", None);
            return Err(());
        }
        let first = if self.at(&Tok::Colon) { None } else { Some(self.expr()?) };
        let e = if self.eat(&Tok::Colon) {
            let end = if self.at(&Tok::RBracket) { None } else { Some(Box::new(self.expr()?)) };
            if self.at(&Tok::Colon) {
                self.hint("E0143", self.span(), "no slice steps; use `.rev()` or `.step_by(n)`", None);
                return Err(());
            }
            ExprKind::Slice { obj: Box::new(obj), start: first.map(Box::new), end, inclusive: false }
        } else {
            let first = first.unwrap();
            match first.kind {
                ExprKind::Range { start, end, inclusive } => ExprKind::Slice { obj: Box::new(obj), start, end, inclusive },
                _ => ExprKind::Index { obj: Box::new(obj), index: Box::new(first) },
            }
        };
        let end = self.expect(&Tok::RBracket, "`]`")?;
        Ok(Expr { kind: e, span: start.to(end) })
    }

    fn primary(&mut self) -> P<Expr> {
        let t = self.tok().clone();
        let span = t.span;
        let kind = match t.tok {
            Tok::Int(n) => {
                self.bump();
                ExprKind::Int(n)
            }
            Tok::Float(f) => {
                self.bump();
                ExprKind::Float(f)
            }
            Tok::Str(parts) => {
                self.bump();
                ExprKind::Str(self.str_parts(parts)?)
            }
            Tok::True => {
                self.bump();
                ExprKind::Bool(true)
            }
            Tok::False => {
                self.bump();
                ExprKind::Bool(false)
            }
            Tok::None => {
                self.bump();
                ExprKind::None
            }
            Tok::Ident(name) => {
                if name == "lambda" {
                    self.hint("E0129", span, "lambdas are `|x| expr`, or use `it` directly: `xs.map(it * 2)`", None);
                    return Err(());
                }
                self.bump();
                if is_type_name(&name) && self.at(&Tok::LBrace) && !self.brace_block_ahead() {
                    return self.struct_lit(Ident { name, span });
                }
                ExprKind::Name(name)
            }
            Tok::LParen => {
                self.bump();
                if self.eat(&Tok::RParen) {
                    if self.at(&Tok::FatArrow) {
                        self.hint("E0129", self.span(), "lambdas are `|| expr`", None);
                        return Err(());
                    }
                    ExprKind::Tuple(vec![])
                } else {
                    let first = self.expr()?;
                    if self.at(&Tok::Comma) {
                        let mut items = vec![first];
                        while self.eat(&Tok::Comma) {
                            if self.at(&Tok::RParen) {
                                break;
                            }
                            items.push(self.expr()?);
                        }
                        self.expect(&Tok::RParen, "`)`")?;
                        if self.at(&Tok::FatArrow) {
                            self.hint("E0129", self.span(), "lambdas are `|a, b| expr`", None);
                            return Err(());
                        }
                        ExprKind::Tuple(items)
                    } else {
                        self.expect(&Tok::RParen, "`)`")?;
                        if self.at(&Tok::FatArrow) {
                            self.hint("E0129", self.span(), "lambdas are `|x| expr`, or use `it`: `xs.map(it * 2)`", None);
                            return Err(());
                        }
                        // Parentheses only group.
                        return Ok(Expr { kind: first.kind, span: span.to(self.prev_span()) });
                    }
                }
            }
            Tok::LBracket => {
                self.bump();
                let mut items = Vec::new();
                while !self.at(&Tok::RBracket) {
                    items.push(self.expr()?);
                    if self.at(&Tok::For) {
                        self.hint("E0133", self.span(), "no comprehensions; use `xs.filter(cond).map(expr)` with `it`", None);
                        return Err(());
                    }
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.expect(&Tok::RBracket, "`]` or `,`")?;
                ExprKind::List(items)
            }
            Tok::LBrace => {
                self.bump();
                if self.eat(&Tok::RBrace) {
                    ExprKind::Map(vec![])
                } else {
                    let first = self.expr()?;
                    if self.eat(&Tok::Colon) {
                        let v = self.expr()?;
                        if self.at(&Tok::For) {
                            self.hint("E0133", self.span(), "no comprehensions; use `.map(...)` then `.to_map()`", None);
                            return Err(());
                        }
                        let mut pairs = vec![(first, v)];
                        while self.eat(&Tok::Comma) {
                            if self.at(&Tok::RBrace) {
                                break;
                            }
                            let k = self.expr()?;
                            self.expect(&Tok::Colon, "`:` between key and value")?;
                            let v = self.expr()?;
                            pairs.push((k, v));
                        }
                        self.expect(&Tok::RBrace, "`}` or `,`")?;
                        ExprKind::Map(pairs)
                    } else {
                        let mut items = vec![first];
                        while self.eat(&Tok::Comma) {
                            if self.at(&Tok::RBrace) {
                                break;
                            }
                            items.push(self.expr()?);
                        }
                        self.expect(&Tok::RBrace, "`}` or `,`")?;
                        ExprKind::Set(items)
                    }
                }
            }
            Tok::If => return self.if_expr(),
            Tok::Match => return self.match_expr(),
            // `x ?? fail "msg"` and `m.get(k) ?? continue`, Kotlin's
            // `?: throw`: a statement that leaves is a value that never comes.
            Tok::Fail | Tok::Return | Tok::Break | Tok::Continue => {
                let kw = self.bump().tok;
                let stmt = match kw {
                    Tok::Fail => Stmt::Fail(self.expr()?, span),
                    Tok::Return => Stmt::Return(if self.starts_expr() { Some(self.expr()?) } else { None }, span),
                    Tok::Break => Stmt::Break(span),
                    _ => Stmt::Continue(span),
                };
                let span = span.to(self.prev_span());
                return Ok(Expr { kind: ExprKind::Block(Block { stmts: vec![stmt], span }), span });
            }
            Tok::Pipe | Tok::PipePipe => {
                let mut params = Vec::new();
                if self.bump().tok == Tok::Pipe {
                    while !self.at(&Tok::Pipe) {
                        params.push(self.ident("a lambda parameter")?);
                        if self.eat(&Tok::Colon) || matches!(self.peek(), Tok::Ident(_)) {
                            self.ty()?;
                        }
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    self.expect(&Tok::Pipe, "`|` after lambda parameters")?;
                }
                if self.at(&Tok::LBrace) {
                    self.hint("E0129", self.span(), "lambda bodies are a single expression without braces: `|x| x + 1`", None);
                    return Err(());
                }
                let body = self.expr()?;
                if matches!(self.peek(), Tok::Assign | Tok::PlusEq | Tok::MinusEq | Tok::StarEq | Tok::SlashEq | Tok::PercentEq) {
                    self.hint("E0129", self.span(), "a lambda body is one expression and cannot assign; update variables in a `for` loop", None);
                    return Err(());
                }
                let span = span.to(body.span);
                return Ok(Expr { kind: ExprKind::Lambda { params, body: Box::new(body) }, span });
            }
            Tok::Fn => {
                self.hint("E0129", span, "anonymous functions are `|x| expr`", None);
                return Err(());
            }
            Tok::SlashSlash => {
                self.slash_slash();
                return Err(());
            }
            _ => {
                self.expected("an expression");
                return Err(());
            }
        };
        Ok(Expr { kind, span: span.to(self.prev_span()) })
    }

    fn struct_lit(&mut self, name: Ident) -> P<Expr> {
        self.bump();
        let mut fields = Vec::new();
        while !self.at(&Tok::RBrace) {
            if self.at(&Tok::DotDot) {
                self.hint("E0144", self.span(), "no struct update syntax; copy the value and set fields: `var v = u` then `v.age = 1`", None);
                return Err(());
            }
            if matches!(self.peek(), Tok::Ident(_)) && matches!(self.peek_at(1), Tok::Colon) {
                let n = self.ident("a field name")?;
                self.bump();
                let value = self.expr()?;
                fields.push(FieldInit { name: Some(n), value });
            } else {
                let value = self.expr()?;
                fields.push(FieldInit { name: None, value });
            }
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        let end = self.expect(&Tok::RBrace, "`}` or `,`")?;
        let span = name.span.to(end);
        Ok(Expr { kind: ExprKind::StructLit { name, fields }, span })
    }

    fn str_parts(&mut self, parts: Vec<StrPart>) -> P<Vec<StrSeg>> {
        let mut segs = Vec::new();
        for p in parts {
            match p {
                StrPart::Lit(s) => segs.push(StrSeg::Lit(s)),
                StrPart::Expr { span, spec } => {
                    let e = self.sub_expr(span.start as usize, span.end as usize)?;
                    // The spec follows the expression and its `:`.
                    let spec_start = span.end as usize + 1;
                    let mut args = Vec::new();
                    if let Some(fs) = spec.as_deref().and_then(crate::fmtspec::FmtSpec::parse) {
                        for (a, b) in [fs.width_arg, fs.precision_arg].into_iter().flatten() {
                            args.push(self.sub_expr(spec_start + a, spec_start + b)?);
                        }
                    }
                    segs.push(StrSeg::Expr(Box::new(e), spec, args));
                }
            }
        }
        Ok(segs)
    }

    /// Parses the source text `start..end` (inside a string) as one expression.
    fn sub_expr(&mut self, start: usize, end: usize) -> P<Expr> {
        let (toks, diags) = Lexer::expression(self.src, start, end).tokenize();
        let failed = !diags.is_empty();
        self.diags.extend(diags);
        if failed {
            return Err(());
        }
        let mut sub = Parser { src: self.src, toks, pos: 0, indent: 0, col_stop: None, diags: Vec::new() };
        let e = sub.expr();
        if e.is_ok() && !sub.at(&Tok::Eof) {
            sub.expected("`}` to close the interpolation");
        }
        let failed = !sub.diags.is_empty();
        self.diags.extend(sub.diags);
        match e {
            Ok(e) if !failed => Ok(e),
            _ => Err(()),
        }
    }

    fn if_expr(&mut self) -> P<Expr> {
        let start = self.bump().span; // `if` or `elif`
        if self.at_ident("let") {
            self.hint(
                "E0147",
                start,
                "no `if let`; an optional is the value or `none`: `x = m.get(k)` then `if x != none:`, or use `match` with a `none:` arm",
                None,
            );
            return Err(());
        }
        let cond = self.expr()?;
        self.block_colon()?;
        let then = self.block()?;
        let mut els = None;
        let block_ended = self.prev_is_dedent();
        if !block_ended && self.at(&Tok::Newline) && matches!(self.peek_at(1), Tok::Elif | Tok::Else) {
            self.bump();
        }
        if self.at(&Tok::Elif) {
            let e = self.if_expr()?;
            els = Some(Block { span: e.span, stmts: vec![Stmt::Expr(e)] });
        } else if self.at(&Tok::Else) {
            self.bump();
            if self.at(&Tok::If) {
                let e = self.if_expr()?;
                els = Some(Block { span: e.span, stmts: vec![Stmt::Expr(e)] });
            } else {
                self.block_colon()?;
                els = Some(self.block()?);
            }
        }
        let span = start.to(self.prev_span());
        Ok(Expr { kind: ExprKind::If { cond: Box::new(cond), then, els }, span })
    }

    fn match_expr(&mut self) -> P<Expr> {
        let start = self.bump().span;
        // The scrutinee ends with its line, even inside brackets, where the
        // arms on the next lines would otherwise continue it (`x` + `-1: ...`).
        let outer = self.col_stop.replace(usize::MAX);
        let scrutinee = self.expr();
        self.col_stop = outer;
        let scrutinee = scrutinee?;
        if self.at(&Tok::LBrace) {
            self.hint("E0123", self.span(), "match arms go on indented lines after `match x`, not in braces", None);
            return Err(());
        }
        self.eat(&Tok::Colon);
        if !self.at(&Tok::Newline) && self.tok().nl_before {
            return self.match_in_brackets(start, scrutinee);
        }
        self.expect(&Tok::Newline, "a newline and indented arms after `match x`")?;
        self.expect(&Tok::Indent, "indented match arms")?;
        let level = self.indent;
        let mut arms = Vec::new();
        loop {
            if self.indent < level {
                break;
            }
            match self.peek() {
                Tok::Dedent => {
                    self.bump();
                    break;
                }
                Tok::Eof => break,
                Tok::Newline => {
                    self.bump();
                    continue;
                }
                _ => {}
            }
            match self.arm() {
                Ok(arm) => {
                    arms.push(arm);
                    self.eat(&Tok::Comma);
                    self.end_or_err(level);
                }
                Err(()) => self.sync_stmt(level),
            }
        }
        let span = start.to(self.prev_span());
        Ok(Expr { kind: ExprKind::Match { scrutinee: Box::new(scrutinee), arms }, span })
    }

    /// A `match` inside brackets, where lines don't make layout tokens: each
    /// arm starts a line at the first arm's column, and its body is one
    /// expression.
    fn match_in_brackets(&mut self, start: Span, scrutinee: Expr) -> P<Expr> {
        let col = self.col(self.tok().span);
        let outer = self.col_stop.replace(col);
        let mut arms = Vec::new();
        let mut result = Ok(());
        while self.tok().nl_before && self.col(self.tok().span) == col && !matches!(self.peek(), Tok::RParen | Tok::RBracket | Tok::RBrace | Tok::Eof) {
            let arm = self.arm_head().and_then(|(pat, guard)| {
                if matches!(self.peek(), Tok::Return | Tok::Break | Tok::Continue | Tok::Fail) {
                    self.hint(
                        "E0146",
                        start,
                        "inside brackets, each `match` arm is one expression; bind the match first (`v = match x` + arms), then use `v`",
                        None,
                    );
                    return Err(());
                }
                let e = self.expr()?;
                // Anything but the next arm or the end of the brackets means the
                // arm was a block of statements.
                if !self.tok().nl_before && !matches!(self.peek(), Tok::RParen | Tok::RBracket | Tok::RBrace | Tok::Comma | Tok::Eof) {
                    self.hint(
                        "E0146",
                        start,
                        "inside brackets, each `match` arm is one expression; bind the match first (`v = match x` + arms), then use `v`",
                        None,
                    );
                    return Err(());
                }
                let span = e.span;
                Ok(MatchArm { pat, guard, body: Block { stmts: vec![Stmt::Expr(e)], span } })
            });
            match arm {
                Ok(arm) => {
                    arms.push(arm);
                    // A comma ends an arm only when the next arm or a closing
                    // bracket follows; otherwise it belongs to the enclosing
                    // call or literal: `f(match x ..., 5)`.
                    if self.at(&Tok::Comma) {
                        let next = &self.toks[(self.pos + 1).min(self.toks.len() - 1)];
                        let closing = matches!(next.tok, Tok::RParen | Tok::RBracket | Tok::RBrace);
                        if closing || (next.nl_before && self.col(next.span) == col) {
                            self.bump();
                        }
                    }
                }
                Err(()) => {
                    result = Err(());
                    break;
                }
            }
        }
        self.col_stop = outer;
        result?;
        if arms.is_empty() {
            self.expected("match arms");
            return Err(());
        }
        let span = start.to(self.prev_span());
        Ok(Expr { kind: ExprKind::Match { scrutinee: Box::new(scrutinee), arms }, span })
    }

    fn arm(&mut self) -> P<MatchArm> {
        let (pat, guard) = self.arm_head()?;
        let body = self.block()?;
        Ok(MatchArm { pat, guard, body })
    }

    /// An arm's pattern, guard and `:`.
    fn arm_head(&mut self) -> P<(Pat, Option<Expr>)> {
        if self.at_ident("case") && !matches!(self.peek_at(1), Tok::Colon | Tok::FatArrow | Tok::LParen) {
            self.bump();
        }
        let pat = self.pattern()?;
        let guard = if self.eat(&Tok::If) { Some(self.expr()?) } else { None };
        if !self.eat(&Tok::Colon) && !self.eat(&Tok::FatArrow) {
            self.expected("`:` after the pattern");
            return Err(());
        }
        Ok((pat, guard))
    }

    // ----- patterns -----

    fn pattern(&mut self) -> P<Pat> {
        let first = self.pattern_single()?;
        if !self.at(&Tok::Pipe) {
            return Ok(first);
        }
        let span = first.span();
        let mut ps = vec![first];
        while self.eat(&Tok::Pipe) {
            ps.push(self.pattern_single()?);
        }
        Ok(Pat::Or(ps, span))
    }

    fn pattern_single(&mut self) -> P<Pat> {
        let t = self.tok().clone();
        let span = t.span;
        match t.tok {
            Tok::Int(_) | Tok::Float(_) | Tok::Str(_) | Tok::True | Tok::False | Tok::Minus => {
                let lit = self.unary()?;
                if self.at(&Tok::DotDot) || self.at(&Tok::DotDotEq) {
                    let inclusive = self.bump().tok == Tok::DotDotEq;
                    let hi = if matches!(self.peek(), Tok::Int(_) | Tok::Float(_) | Tok::Str(_) | Tok::Minus) { Some(self.unary()?) } else { None };
                    return Ok(Pat::Range { lo: Some(lit), hi, inclusive, span: span.to(self.prev_span()) });
                }
                Ok(Pat::Lit(lit))
            }
            Tok::None => {
                self.bump();
                Ok(Pat::None(span))
            }
            Tok::LParen => {
                self.bump();
                let mut ps = Vec::new();
                while !self.at(&Tok::RParen) {
                    ps.push(self.pattern()?);
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                let end = self.expect(&Tok::RParen, "`)`")?;
                if ps.len() == 1 {
                    return Ok(ps.pop().unwrap());
                }
                Ok(Pat::Tuple(ps, span.to(end)))
            }
            Tok::LBracket => {
                self.bump();
                let mut items = Vec::new();
                let mut rest = None;
                while !self.at(&Tok::RBracket) {
                    if self.eat(&Tok::DotDot) {
                        rest = Some(if matches!(self.peek(), Tok::Ident(_)) { Some(self.ident("a name")?) } else { None });
                    } else {
                        if rest.is_some() {
                            self.err(self.span(), "`..rest` must come last in a list pattern");
                            return Err(());
                        }
                        items.push(self.pattern()?);
                    }
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                let end = self.expect(&Tok::RBracket, "`]`")?;
                Ok(Pat::List { items, rest, span: span.to(end) })
            }
            Tok::Ident(name) => {
                self.bump();
                if name == "_" {
                    return Ok(Pat::Wild(span));
                }
                let id = Ident { name: name.clone(), span };
                // `Some(x)`, `Ok(x)`, `Err(e)` and `None` mean what they do in
                // Rust and are resolved as such; a bare `Some` has no meaning.
                if matches!(name.as_str(), "Some" | "Ok" | "Err") && !self.at(&Tok::LParen) {
                    let msg = match name.as_str() {
                        "Some" => "no `Some`: an optional is the value itself or `none`; match `none` first, then bind a name",
                        "Ok" => "no `Ok`: a result is the value itself or an error; match `err(e)` first, then bind a name",
                        _ => "write `err(e)` to match an error",
                    };
                    self.hint("E0145", span, msg, None);
                    return Err(());
                }
                if (name == "err" || name == "ok") && self.at(&Tok::LParen) {
                    self.bump();
                    let inner = if self.at(&Tok::RParen) { Pat::Wild(span) } else { self.pattern()? };
                    let end = self.expect(&Tok::RParen, "`)`")?;
                    let b = Box::new(inner);
                    return Ok(if name == "err" { Pat::Err(b, span.to(end)) } else { Pat::Ok(b, span.to(end)) });
                }
                if self.at(&Tok::Dot) && matches!(self.peek_at(1), Tok::Ident(_)) {
                    self.bump();
                    let vname = self.ident("a variant name")?;
                    let args = if self.at(&Tok::LParen) { self.pat_args()? } else { vec![] };
                    return Ok(Pat::Variant { name: vname, qualifier: Some(id), args });
                }
                if self.at(&Tok::ColonColon) {
                    self.hint("E0125", self.span(), "no `::` paths; use `.`, or just the variant name", None);
                    return Err(());
                }
                if self.at(&Tok::LParen) {
                    let args = self.pat_args()?;
                    return Ok(Pat::Variant { name: id, qualifier: None, args });
                }
                if self.at(&Tok::LBrace) && is_type_name(&name) {
                    self.bump();
                    let mut fields = Vec::new();
                    while !self.at(&Tok::RBrace) {
                        if self.eat(&Tok::DotDot) {
                            break;
                        }
                        let f = self.ident("a field name")?;
                        let p = if self.eat(&Tok::Colon) { self.pattern()? } else { Pat::Name(f.clone()) };
                        fields.push((f, p));
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    let end = self.expect(&Tok::RBrace, "`}`")?;
                    return Ok(Pat::Struct { name: id, fields, span: span.to(end) });
                }
                Ok(Pat::Name(id))
            }
            _ => {
                self.expected("a pattern");
                Err(())
            }
        }
    }

    fn pat_args(&mut self) -> P<Vec<Pat>> {
        self.bump();
        let mut args = Vec::new();
        while !self.at(&Tok::RParen) {
            args.push(self.pattern()?);
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RParen, "`)`")?;
        Ok(args)
    }
}

/// `del m[k]` -> `m.remove(k)`.
fn del_fix(line: &str) -> Option<String> {
    let target = line.strip_prefix("del ")?.trim().strip_suffix(']')?;
    let mut depth = 0;
    for (i, c) in target.char_indices().rev() {
        match c {
            ']' | ')' => depth += 1,
            '(' => depth -= 1,
            '[' if depth == 0 => return Some(format!("{}.remove({})", &target[..i], &target[i + 1..])),
            '[' => depth -= 1,
            _ => {}
        }
    }
    None
}
