//! Tokens to a syntax tree, by recursive descent: declarations here, statements in `stmt`,
//! expressions in `expr`, and `match` patterns in `pattern`.

mod expr;
mod pattern;
mod stmt;

use crate::ast::{
    Case, ExprKind, Extension, Field, FnDef, Generic, Import, Interface, Item, Module, Param, Stmt, StmtKind,
    TestDef, TypeDef, TypeExpr,
};
use crate::diagnostic::Diagnostic;
use crate::lexer::lex;
use crate::span::Span;
use crate::token::{Kw, Punct, Tok, Token};

/// Parses a whole `.fen` file, where only declarations may sit at the top level.
pub fn parse(source: &str) -> Result<Module, Diagnostic> {
    Parser::new(lex(source, 0)?).module(false)
}

/// Parses a standard library source. Its spans start at `Span::PRELUDE`, so a mistake found
/// in it is never shown against the program's own lines.
pub fn parse_prelude(source: &str) -> Result<Module, Diagnostic> {
    Parser::new(lex(source, Span::PRELUDE)?).module(false)
}

/// Parses one file of a program with several, its spans starting at byte `base` so they
/// don't overlap another file's. `Sources` turns them back into a file and line.
pub fn parse_at(source: &str, base: u32) -> Result<Module, Diagnostic> {
    Parser::new(lex(source, base)?).module(false)
}

/// Parses a chunk typed into the REPL, where statements may sit at the top level too.
pub fn parse_repl(source: &str) -> Result<Module, Diagnostic> {
    Parser::new(lex(source, 0)?).module(true)
}

pub(crate) struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    eof: Token,
    /// Set when an expression ended with an indented block (a `match`), so the statement
    /// holding it has no line ending of its own to consume.
    block_ended: bool,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        let end = tokens.last().map_or_else(Span::default, |token| token.span);
        Parser { tokens, pos: 0, eof: Token { tok: Tok::Eof, span: end }, block_ended: false }
    }

    fn current(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or(&self.eof)
    }

    fn peek(&self) -> &Tok {
        &self.current().tok
    }

    fn peek_at(&self, ahead: usize) -> &Tok {
        self.tokens.get(self.pos + ahead).map_or(&self.eof.tok, |token| &token.tok)
    }

    fn span(&self) -> Span {
        self.current().span
    }

    fn prev_span(&self) -> Span {
        self.pos
            .checked_sub(1)
            .and_then(|at| self.tokens.get(at))
            .map_or_else(|| self.span(), |token| token.span)
    }

    fn advance(&mut self) -> Token {
        let token = self.current().clone();
        self.pos = (self.pos + 1).min(self.tokens.len());
        token
    }

    fn at(&self, tok: &Tok) -> bool {
        self.peek() == tok
    }

    fn at_p(&self, punct: Punct) -> bool {
        self.at(&Tok::P(punct))
    }

    fn at_kw(&self, kw: Kw) -> bool {
        self.at(&Tok::Kw(kw))
    }

    /// Whether the next token is the soft keyword `word`.
    fn at_soft(&self, word: &str) -> bool {
        matches!(self.peek(), Tok::Name(name) if name == word)
    }

    fn eat(&mut self, tok: &Tok) -> bool {
        let found = self.at(tok);
        if found {
            self.pos += 1;
        }
        found
    }

    fn eat_p(&mut self, punct: Punct) -> bool {
        self.eat(&Tok::P(punct))
    }

    /// Skips `pub` before a declaration or field. It matters only between kits.
    fn eat_pub(&mut self) {
        if self.at_soft("pub") && matches!(self.peek_at(1), Tok::Name(_) | Tok::Kw(_)) {
            self.advance();
        }
    }

    fn eat_kw(&mut self, kw: Kw) -> bool {
        self.eat(&Tok::Kw(kw))
    }

    fn expect_p(&mut self, punct: Punct) -> Result<Span, Diagnostic> {
        if self.at_p(punct) {
            Ok(self.advance().span)
        } else {
            Err(self.unexpected(&format!("`{}`", punct.text())))
        }
    }

    fn unexpected(&self, expected: &str) -> Diagnostic {
        Diagnostic::new(format!("expected {expected}, found {}", describe(self.peek())), self.span())
    }

    fn name(&mut self, what: &str) -> Result<String, Diagnostic> {
        match self.peek().clone() {
            Tok::Name(name) => {
                self.advance();
                Ok(name)
            }
            Tok::Kw(Kw::Reserved(word)) => Err(self.reserved(word)),
            Tok::Kw(_) => Err(self
                .unexpected(what)
                .with_help(format!("{} is a keyword, so it can't be a name", describe(self.peek())))),
            _ => Err(self.unexpected(what)),
        }
    }

    fn reserved(&self, word: &str) -> Diagnostic {
        Diagnostic::new(format!("`{word}` is part of Fennec's design but isn't supported yet"), self.span())
            .with_help("see what's coming at https://fennec-lang.dev/roadmap/")
    }

    /// The start of an indented block: a line break, then a deeper indent.
    fn expect_block_start(&mut self) -> Result<(), Diagnostic> {
        if self.eat(&Tok::Newline) && self.eat(&Tok::Indent) {
            Ok(())
        } else {
            Err(Diagnostic::new("expected an indented block", self.span())
                .with_help("indent the body 4 spaces more than the line ending in `:`"))
        }
    }

    fn end_line(&mut self) -> Result<(), Diagnostic> {
        if std::mem::take(&mut self.block_ended)
            || self.eat(&Tok::Newline)
            || matches!(self.peek(), Tok::Eof | Tok::Dedent)
        {
            Ok(())
        } else {
            Err(self.unexpected("the end of the line"))
        }
    }

    fn module(&mut self, allow_statements: bool) -> Result<Module, Diagnostic> {
        let mut items = Vec::new();
        loop {
            while self.eat(&Tok::Newline) {}
            if self.at(&Tok::Eof) {
                break;
            }
            let item = self.item()?;
            let item = match item {
                Item::Stmt(stmt) if !allow_statements => constant(stmt)?,
                item => item,
            };
            items.push(item);
        }
        Ok(Module { items })
    }

    fn item(&mut self) -> Result<Item, Diagnostic> {
        // `pub`, `export` and `noalloc` are accepted. Inside a kit everything is visible, and
        // allocation checks come later.
        self.eat_pub();
        self.eat_kw(Kw::Export);
        if self.eat_kw(Kw::Noalloc) && !self.at_kw(Kw::Def) {
            return Err(Diagnostic::new("`noalloc` goes right before `def`", self.span()));
        }
        match self.peek().clone() {
            Tok::Kw(Kw::Def) => Ok(Item::Function(self.def()?)),
            Tok::Kw(Kw::From | Kw::Import) => Ok(Item::Import(self.import()?)),
            Tok::Name(word) if self.is_declaration(&word) => self.declaration(&word),
            _ => Ok(Item::Stmt(self.stmt()?)),
        }
    }

    fn is_declaration(&self, word: &str) -> bool {
        match word {
            "struct" | "enum" | "error" | "interface" | "extend" => matches!(self.peek_at(1), Tok::Name(_)),
            "type" => matches!((self.peek_at(1), self.peek_at(2)), (Tok::Name(_), Tok::P(Punct::Assign))),
            "test" | "bench" => matches!(self.peek_at(1), Tok::Str(_)),
            _ => false,
        }
    }

    fn declaration(&mut self, word: &str) -> Result<Item, Diagnostic> {
        let start = self.advance().span;
        match word {
            "struct" => Ok(Item::Struct(self.type_def(start, false)?)),
            "error" => Ok(Item::Error(self.type_def(start, false)?)),
            "enum" => Ok(Item::Enum(self.type_def(start, true)?)),
            "interface" => self.interface(start),
            "extend" => self.extension(start),
            "type" => {
                let name = self.name("a type name")?;
                self.expect_p(Punct::Assign)?;
                let ty = self.type_expr()?;
                let span = start.to(self.prev_span());
                self.end_line()?;
                Ok(Item::TypeAlias { name, ty, span })
            }
            "bench" => self.test(start, true),
            _ => self.test(start, false),
        }
    }

    fn type_def(&mut self, start: Span, is_enum: bool) -> Result<TypeDef, Diagnostic> {
        let name = self.name("a type name")?;
        let generics = self.generic_params()?;
        let span = start.to(self.prev_span());
        self.expect_p(Punct::Colon)?;
        self.expect_block_start()?;
        let mut def =
            TypeDef { name, generics, fields: Vec::new(), cases: Vec::new(), methods: Vec::new(), span };
        while !self.eat(&Tok::Dedent) && !self.at(&Tok::Eof) {
            // Enum cases come with their enum and have no `pub` of their own.
            if !is_enum || matches!(self.peek_at(1), Tok::Kw(Kw::Def)) {
                self.eat_pub();
            }
            if self.at_kw(Kw::Def) {
                def.methods.push(self.def()?);
            } else if self.eat_kw(Kw::Pass) {
                self.end_line()?;
            } else if is_enum {
                def.cases.push(self.case()?);
            } else {
                def.fields.push(self.field(true)?);
                self.end_line()?;
            }
        }
        Ok(def)
    }

    /// `name: type`, `name = default`, or both, with a `where` rule when `rules` allows.
    fn field(&mut self, rules: bool) -> Result<Field, Diagnostic> {
        let start = self.span();
        let name = self.name("a field name")?;
        let ty = if self.eat_p(Punct::Colon) { Some(self.type_expr()?) } else { None };
        let default = if self.eat_p(Punct::Assign) { Some(self.expr()?) } else { None };
        let rule = if rules && self.at_soft("where") {
            self.advance();
            Some(self.expr()?)
        } else {
            None
        };
        if ty.is_none() && default.is_none() {
            return Err(Diagnostic::new(format!("field `{name}` needs a type or a default"), start)
                .with_help(format!("write `{name}: int`, or a default like `{name} = 0`")));
        }
        Ok(Field { name, ty, default, rule, span: start.to(self.prev_span()) })
    }

    fn case(&mut self) -> Result<Case, Diagnostic> {
        let start = self.span();
        let name = self.name("an enum case")?;
        let mut fields = Vec::new();
        if self.eat_p(Punct::LParen) {
            while !self.at_p(Punct::RParen) {
                fields.push(self.field(false)?);
                if !self.eat_p(Punct::Comma) {
                    break;
                }
            }
            self.expect_p(Punct::RParen)?;
        }
        let span = start.to(self.prev_span());
        self.end_line()?;
        Ok(Case { name, fields, span })
    }

    fn interface(&mut self, start: Span) -> Result<Item, Diagnostic> {
        let name = self.name("an interface name")?;
        self.expect_p(Punct::Colon)?;
        self.expect_block_start()?;
        let mut methods = Vec::new();
        while !self.eat(&Tok::Dedent) && !self.at(&Tok::Eof) {
            if !self.eat_kw(Kw::Pass) {
                methods.push(self.signature()?);
            }
            self.end_line()?;
        }
        Ok(Item::Interface(Interface { name, methods, span: start.to(self.prev_span()) }))
    }

    /// `extend Name:` and its methods. A field is an error: an extension adds methods only.
    fn extension(&mut self, start: Span) -> Result<Item, Diagnostic> {
        let name = self.name("a type name")?;
        let generics = self.generic_params()?;
        let span = start.to(self.prev_span());
        self.expect_p(Punct::Colon)?;
        self.expect_block_start()?;
        let mut methods = Vec::new();
        while !self.eat(&Tok::Dedent) && !self.at(&Tok::Eof) {
            self.eat_pub();
            if self.at_kw(Kw::Def) {
                methods.push(self.def()?);
            } else if self.eat_kw(Kw::Pass) {
                self.end_line()?;
            } else {
                return Err(Diagnostic::new("an extension adds methods only", self.span())
                    .with_help(format!("declare fields where `{name}` is declared")));
            }
        }
        Ok(Item::Extend(Extension { name, generics, methods, span }))
    }

    fn test(&mut self, start: Span, bench: bool) -> Result<Item, Diagnostic> {
        let Tok::Str(name) = self.advance().tok else { return Err(self.unexpected("a test name")) };
        let mut capabilities = Vec::new();
        if !bench && self.eat_p(Punct::LParen) {
            while !self.at_p(Punct::RParen) {
                capabilities.push(self.param()?);
                if !self.eat_p(Punct::Comma) {
                    break;
                }
            }
            self.expect_p(Punct::RParen)?;
        }
        let mut inputs = Vec::new();
        if !bench && self.eat_kw(Kw::For) {
            loop {
                inputs.push(self.param()?);
                if !self.eat_p(Punct::Comma) {
                    break;
                }
            }
        }
        let span = start.to(self.prev_span());
        self.expect_p(Punct::Colon)?;
        let body = self.block()?;
        Ok(Item::Test(TestDef { name, capabilities, inputs, body, bench, span }))
    }

    fn import(&mut self) -> Result<Import, Diagnostic> {
        let start = self.span();
        let from = self.eat_kw(Kw::From);
        if !from {
            self.advance();
        }
        let mut module = vec![self.name("a module name")?];
        while self.eat_p(Punct::Dot) {
            module.push(self.name("a module name")?);
        }
        let mut names = Vec::new();
        let mut aliases = Vec::new();
        let mut alias = None;
        if from {
            if !self.eat_kw(Kw::Import) {
                return Err(self.unexpected("`import`"));
            }
            loop {
                names.push(self.name("a name to import")?);
                aliases.push(self.alias()?);
                if !self.eat_p(Punct::Comma) {
                    break;
                }
            }
        } else {
            alias = self.alias()?;
        }
        let span = start.to(self.prev_span());
        self.end_line()?;
        Ok(Import { module, names, aliases, alias, span })
    }

    /// `as name` after something imported, if it's there.
    fn alias(&mut self) -> Result<Option<String>, Diagnostic> {
        if self.eat_kw(Kw::As) { Ok(Some(self.name("a name to import it as")?)) } else { Ok(None) }
    }

    /// `[T, U: Shape]` after a function or type name. Generic parameters are checked by the
    /// type checker, so the interpreter only needs to skip them.
    fn generic_params(&mut self) -> Result<Vec<Generic>, Diagnostic> {
        let mut generics = Vec::new();
        if !self.eat_p(Punct::LBracket) {
            return Ok(generics);
        }
        loop {
            let start = self.span();
            let name = self.name("a type parameter")?;
            let mut bounds = Vec::new();
            if self.eat_p(Punct::Colon) {
                bounds.push(self.type_expr()?);
                while self.eat_p(Punct::Amp) {
                    bounds.push(self.type_expr()?);
                }
            }
            generics.push(Generic { name, bounds, span: start.to(self.prev_span()) });
            if !self.eat_p(Punct::Comma) {
                break;
            }
        }
        self.expect_p(Punct::RBracket)?;
        Ok(generics)
    }

    pub(crate) fn def(&mut self) -> Result<FnDef, Diagnostic> {
        let mut function = self.signature()?;
        self.expect_p(Punct::Colon)?;
        function.body = self.block()?;
        Ok(function)
    }

    /// `def name(params) -> ret`, without a body. Operators and indexing are defined as
    /// `def +`, `def []` and `def []=`.
    fn signature(&mut self) -> Result<FnDef, Diagnostic> {
        let start = self.advance().span;
        let name = match self.peek().clone() {
            Tok::Name(name) => {
                self.advance();
                name
            }
            Tok::P(
                punct @ (Punct::Plus
                | Punct::Minus
                | Punct::Star
                | Punct::Slash
                | Punct::SlashSlash
                | Punct::Percent
                | Punct::EqEq
                | Punct::Lt),
            ) => {
                self.advance();
                punct.text().to_owned()
            }
            Tok::P(Punct::LBracket) if self.peek_at(1) == &Tok::P(Punct::RBracket) => {
                self.pos += 2;
                if self.eat_p(Punct::Assign) { "[]=".to_owned() } else { "[]".to_owned() }
            }
            _ => return Err(self.unexpected("a function name")),
        };
        let generics = self.generic_params()?;
        self.expect_p(Punct::LParen)?;
        let mut params = Vec::new();
        while !self.at_p(Punct::RParen) {
            let param = self.param()?;
            if param.default.is_none() && params.iter().any(|earlier: &Param| earlier.default.is_some()) {
                let message =
                    format!("`{}` has no default, so it has to come before the ones that do", param.name);
                return Err(Diagnostic::new(message, param.span));
            }
            params.push(param);
            if !self.eat_p(Punct::Comma) {
                break;
            }
        }
        self.expect_p(Punct::RParen)?;
        let arrow = self.eat_p(Punct::Arrow).then(|| self.prev_span());
        let ret = if arrow.is_some() { Some(self.type_expr()?) } else { None };
        Ok(FnDef { name, generics, params, ret, arrow, body: Vec::new(), span: start.to(self.prev_span()) })
    }

    fn param(&mut self) -> Result<Param, Diagnostic> {
        let start = self.span();
        let is_mut = self.eat_kw(Kw::Mut);
        let name = self.name("a parameter name")?;
        if name == "self" {
            return Ok(Param {
                name,
                ty: None,
                is_mut,
                is_self: true,
                default: None,
                span: start.to(self.prev_span()),
            });
        }
        // `limit=8` takes its type from the literal.
        if self.eat_p(Punct::Assign) {
            let default = Some(self.param_default(&name, is_mut)?);
            return Ok(Param {
                name,
                ty: None,
                is_mut,
                is_self: false,
                default,
                span: start.to(self.prev_span()),
            });
        }
        if !self.eat_p(Punct::Colon) {
            return Err(Diagnostic::new(format!("parameter `{name}` needs a type"), self.prev_span())
                .with_help(format!(
                    "write it with its type, like `{name}: int`, or give it a default, like `{name}=8`"
                )));
        }
        let ty = self.type_expr()?;
        let default = if self.eat_p(Punct::Assign) { Some(self.param_default(&name, is_mut)?) } else { None };
        Ok(Param { name, ty: Some(ty), is_mut, is_self: false, default, span: start.to(self.prev_span()) })
    }

    /// A parameter's default: a plain value, known without running anything.
    fn param_default(&mut self, name: &str, is_mut: bool) -> Result<crate::ast::Expr, Diagnostic> {
        let value = self.expr()?;
        if is_mut {
            let message = format!("`mut` parameter `{name}` can't have a default");
            return Err(Diagnostic::new(message, value.span).with_help(
                "a `mut` parameter changes what the caller passes, so a call has to pass something",
            ));
        }
        if !value.is_plain_value() {
            return Err(Diagnostic::new("a parameter's default has to be a plain value", value.span)
                .with_help("write a number, string, `True`, `False` or `None`, and work anything else out in the function"));
        }
        Ok(value)
    }

    pub(crate) fn type_expr(&mut self) -> Result<TypeExpr, Diagnostic> {
        let start = self.span();
        let mut args = Vec::new();
        let name = if self.eat_kw(Kw::None) {
            "None".to_owned()
        } else if self.eat_p(Punct::LParen) {
            while !self.at_p(Punct::RParen) {
                args.push(self.type_expr()?);
                if !self.eat_p(Punct::Comma) {
                    break;
                }
            }
            self.expect_p(Punct::RParen)?;
            // `(int, str) -> bool`: a function type, its return type last.
            if self.eat_p(Punct::Arrow) {
                args.push(self.type_expr()?);
                return Ok(TypeExpr {
                    name: "->".to_owned(),
                    args,
                    optional: false,
                    span: start.to(self.prev_span()),
                });
            }
            "tuple".to_owned()
        } else {
            // `physics.Body`, a type from a module imported whole.
            let mut name = self.name("a type")?;
            while self.eat_p(Punct::Dot) {
                name = format!("{name}.{}", self.name("a type")?);
            }
            name
        };
        if self.eat_p(Punct::LBracket) {
            loop {
                args.push(self.type_expr()?);
                if !self.eat_p(Punct::Comma) {
                    break;
                }
            }
            self.expect_p(Punct::RBracket)?;
        }
        let optional = self.eat_p(Punct::Question);
        Ok(TypeExpr { name, args, optional, span: start.to(self.prev_span()) })
    }
}

/// A statement at the top level of a file, which has to be a constant: `NAME = value`.
fn constant(stmt: Stmt) -> Result<Item, Diagnostic> {
    let span = stmt.span;
    match stmt.kind {
        StmtKind::Assign { target, op: None, value, declare_mut: false, ty } => match target.kind {
            ExprKind::Name(name) => Ok(Item::Const { name, ty, value, span }),
            _ => Err(Diagnostic::new("a constant is one name, like `LIMIT = 10`", target.span)),
        },
        StmtKind::Assign { declare_mut: true, .. } => Err(Diagnostic::new(
            "there's no `mut` at the top level of a file, so there's no state shared between functions",
            span,
        )
        .with_help("make it a constant by leaving out `mut`, or keep the state in `main` and pass it on")),
        _ => Err(Diagnostic::new("only declarations and constants can go at the top level of a file", span)
            .with_help("put this code inside `def main(stdout: Stdout):`")),
    }
}

fn describe(tok: &Tok) -> String {
    match tok {
        Tok::Name(name) => format!("`{name}`"),
        Tok::Int(_) | Tok::Float(_) => "a number".to_owned(),
        Tok::Str(_) | Tok::FStr(_) => "a string".to_owned(),
        Tok::Kw(kw) => format!("`{}`", kw.text()),
        Tok::P(punct) => format!("`{}`", punct.text()),
        Tok::Newline => "the end of the line".to_owned(),
        Tok::Indent => "an indented line".to_owned(),
        Tok::Dedent => "the end of the block".to_owned(),
        Tok::Eof => "the end of the file".to_owned(),
    }
}
