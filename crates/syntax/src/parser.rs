//! Tokens to a syntax tree, by recursive descent: declarations here, statements in `stmt`,
//! expressions in `expr`, and `match` patterns in `pattern`.

mod expr;
mod pattern;
mod stmt;
mod template;

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
    Parser::new(lex(source, 0)?).module()
}

/// Parses a standard library source. Its spans start at `Span::PRELUDE`, so a mistake found
/// in it is never shown against the program's own lines.
pub fn parse_prelude(source: &str) -> Result<Module, Diagnostic> {
    parse_prelude_at(source, Span::PRELUDE)
}

/// Parses a standard library source with its spans starting at `base`, past `Span::PRELUDE`:
/// each file has its own, so a span names the file it's in.
pub(crate) fn parse_prelude_at(source: &str, base: u32) -> Result<Module, Diagnostic> {
    Parser::new(lex(source, base)?).module()
}

/// Parses one file of a program with several, its spans starting at byte `base` so they
/// don't overlap another file's. `Sources` turns them back into a file and line.
pub fn parse_at(source: &str, base: u32) -> Result<Module, Diagnostic> {
    Parser::new(lex(source, base)?).module()
}

/// Parses statements standing alone, like the code in a doc example. They're indented as they
/// are in `source`, and their spans start at byte `base`.
pub(crate) fn parse_statements_at(source: &str, base: u32) -> Result<Vec<Stmt>, Diagnostic> {
    let mut parser = Parser::new(lex(source, base)?);
    parser.eat(&Tok::Newline);
    parser.eat(&Tok::Indent);
    let mut stmts = Vec::new();
    while !parser.eat(&Tok::Dedent) && !parser.at(&Tok::Eof) {
        stmts.push(parser.stmt()?);
    }
    parser.old_spelling_error().map_or(Ok(stmts), Err)
}

pub(crate) struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    eof: Token,
    /// Set when an expression ended with an indented block (a `match`), so the statement
    /// holding it has no line ending of its own to consume.
    block_ended: bool,
    /// Each `mut` written where the language now says `ref`, and what it was on. Parsing goes on
    /// past them, so the one error they give can fix every one in the file.
    old_mut: Vec<(Span, &'static str)>,
    /// Each `pub` written, the old spelling, with what goes there instead: `export` on a top-level
    /// name, and nothing on a method, which comes with its type. Like `old_mut`, they give one
    /// error that fixes them all.
    old_pub: Vec<(Span, Span, &'static str)>,
    /// Set just before the value a statement is written with: the whole right side of an
    /// assignment, a bare call or what `return` gives. Only there may `except` follow it, so the
    /// next expression takes it and what it holds sees it unset.
    value_at_top: bool,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        let end = tokens.last().map_or_else(Span::default, |token| token.span);
        Parser {
            tokens,
            pos: 0,
            eof: Token { tok: Tok::Eof, span: end },
            block_ended: false,
            old_mut: Vec::new(),
            old_pub: Vec::new(),
            value_at_top: false,
        }
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

    /// Skips `pub`, the old spelling of `export`, before a declaration or field, giving back the
    /// span from it to what follows. On a top-level name it's `export`, and anywhere else it goes.
    fn eat_pub(&mut self, top_level: bool) -> Option<Span> {
        if self.at_soft("pub") && matches!(self.peek_at(1), Tok::Name(_) | Tok::Kw(_)) {
            let word = self.advance().span;
            let span = Span { start: word.start, end: self.span().start };
            self.old_pub.push((word, span, if top_level { "export " } else { "" }));
            return Some(span);
        }
        None
    }

    /// Skips `export` on a method, which comes with its type, for the error that removes it.
    fn eat_export(&mut self) -> Option<Span> {
        if !self.at_kw(Kw::Export) {
            return None;
        }
        let word = self.advance().span;
        let span = Span { start: word.start, end: self.span().start };
        self.old_pub.push((word, span, ""));
        Some(span)
    }

    /// The error for each `pub`, at the first one, with the fixes for all of them.
    fn old_pub_error(&self) -> Option<Diagnostic> {
        let &(at, _, _) = self.old_pub.first()?;
        let others = self.old_pub.len() - 1;
        let more = match others {
            0 => String::new(),
            1 => " and in 1 other place".to_owned(),
            _ => format!(" and in {others} other places"),
        };
        let message = if self.old_pub.iter().any(|(_, _, text)| !text.is_empty()) {
            "`pub` is written `export` now"
        } else {
            "a field or method has no `export` of its own"
        };
        let mut diagnostic = Diagnostic::new(message, at).with_help(format!(
            "`export` marks a top-level name other kits can use, and a type's fields and methods come with it. `fennec fix` mends it here{more}"
        ));
        for &(_, span, text) in &self.old_pub {
            diagnostic = diagnostic.with_fix(span, text);
        }
        Some(diagnostic)
    }

    /// The one error for an old spelling, `mut` for `ref` first, then `pub`.
    fn old_spelling_error(&self) -> Option<Diagnostic> {
        self.old_mut_error().or_else(|| self.old_pub_error())
    }

    fn eat_kw(&mut self, kw: Kw) -> bool {
        self.eat(&Tok::Kw(kw))
    }

    /// `ref` on `what`: a loop, a parameter, an argument or a `with` name. `mut` there was the old
    /// spelling. It's kept as `ref` so parsing can go on, and the file fails with one error at
    /// the end, which `fennec fix` mends everywhere it was written.
    fn eat_ref(&mut self, what: &'static str) -> bool {
        if self.at_kw(Kw::Mut) {
            let span = self.advance().span;
            self.old_mut.push((span, what));
            return true;
        }
        self.eat_kw(Kw::Ref)
    }

    /// The error for each `mut` that should be `ref`, at the first one, with the fixes for all of
    /// them. It comes before any later mistake, since it's earlier in the file.
    fn old_mut_error(&self) -> Option<Diagnostic> {
        let &(span, what) = self.old_mut.first()?;
        let others = self.old_mut.len() - 1;
        let more = match others {
            0 => String::new(),
            1 => " and in 1 other place".to_owned(),
            _ => format!(" and in {others} other places"),
        };
        let mut diagnostic = Diagnostic::new(format!("{what} is written `ref`, not `mut`"), span)
            .with_help(format!("`mut` is only for a new local name. `fennec fix` writes `ref` here{more}"));
        for &(span, _) in &self.old_mut {
            diagnostic = diagnostic.with_fix(span, "ref");
        }
        Some(diagnostic)
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

    fn module(&mut self) -> Result<Module, Diagnostic> {
        let module = self.items();
        match self.old_spelling_error() {
            Some(old) => Err(old),
            None => module,
        }
    }

    fn items(&mut self) -> Result<Module, Diagnostic> {
        let mut items = Vec::new();
        loop {
            while self.eat(&Tok::Newline) {}
            if self.at(&Tok::Eof) {
                break;
            }
            let item = self.item()?;
            let item = match item {
                Item::Stmt(stmt) => constant(stmt)?,
                item => item,
            };
            items.push(item);
        }
        Ok(Module { items, std_rewritten: Vec::new() })
    }

    fn item(&mut self) -> Result<Item, Diagnostic> {
        let annotations = self.annotations()?;
        if let Some(first) = annotations.first() {
            self.eat_pub(true);
            self.eat_kw(Kw::Export);
            if !self.at_kw(Kw::Def) {
                return Err(Diagnostic::new("an annotation goes above a field or a `def`", first.span));
            }
            return Ok(Item::Function(FnDef { annotations, ..self.def()? }));
        }
        // Inside a kit everything is visible: `export` matters only between kits.
        self.eat_pub(true);
        self.eat_kw(Kw::Export);
        match self.peek().clone() {
            Tok::Kw(Kw::Def) => Ok(Item::Function(self.def()?)),
            Tok::Kw(Kw::From | Kw::Import) => Ok(Item::Import(self.import()?)),
            Tok::Name(word) if self.is_declaration(&word) => self.declaration(&word),
            _ => Ok(Item::Stmt(self.stmt()?)),
        }
    }

    fn is_declaration(&self, word: &str) -> bool {
        match word {
            "struct" | "shared" | "enum" | "error" | "interface" | "extend" => {
                matches!(self.peek_at(1), Tok::Name(_))
            }
            "type" => matches!((self.peek_at(1), self.peek_at(2)), (Tok::Name(_), Tok::P(Punct::Assign))),
            "test" | "bench" => matches!(self.peek_at(1), Tok::Str(_)),
            _ => false,
        }
    }

    fn declaration(&mut self, word: &str) -> Result<Item, Diagnostic> {
        let start = self.advance().span;
        match word {
            "struct" => Ok(Item::Struct(self.type_def(start, false)?)),
            "shared" => Ok(Item::Struct(TypeDef { shared: true, ..self.type_def(start, false)? })),
            "error" => Ok(Item::Error(self.error_def(start)?)),
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

    /// An error: fields, like a struct's, or cases, like an enum's, each failing its own way:
    /// `NotFound(path: str)`. A line that's a name and then `(` or nothing is a case.
    fn error_def(&mut self, start: Span) -> Result<TypeDef, Diagnostic> {
        let def = self.type_body(start, false, true)?;
        if let (Some(field), false) = (def.fields.first(), def.cases.is_empty()) {
            return Err(Diagnostic::new(format!("`{}` has both fields and cases", def.name), field.span)
                .with_help(
                    "an error has fields, like `path: str`, or cases, like `NotFound(path: str)`, not both",
                ));
        }
        Ok(def)
    }

    fn type_def(&mut self, start: Span, is_enum: bool) -> Result<TypeDef, Diagnostic> {
        self.type_body(start, is_enum, false)
    }

    /// A struct's, enum's or error's name and body. An error's body may have cases.
    fn type_body(&mut self, start: Span, is_enum: bool, is_error: bool) -> Result<TypeDef, Diagnostic> {
        let name = self.name("a type name")?;
        let generics = self.generic_params()?;
        let span = start.to(self.prev_span());
        self.expect_p(Punct::Colon)?;
        self.expect_block_start()?;
        let mut def = TypeDef {
            name,
            generics,
            fields: Vec::new(),
            cases: Vec::new(),
            methods: Vec::new(),
            shared: false,
            span,
        };
        while !self.eat(&Tok::Dedent) && !self.at(&Tok::Eof) {
            // Enum cases, fields and methods come with their type and have no `export` of their own.
            if !is_enum || matches!(self.peek_at(1), Tok::Kw(Kw::Def)) {
                let _ = self.eat_pub(false).or_else(|| self.eat_export());
            }
            let annotations = if is_enum { Vec::new() } else { self.annotations()? };
            if !annotations.is_empty() {
                let _ = self.eat_pub(false).or_else(|| self.eat_export());
            }
            if self.at_kw(Kw::Def) {
                def.methods.push(FnDef { annotations, ..self.def()? });
            } else if annotations.is_empty() && self.eat_kw(Kw::Pass) {
                self.end_line()?;
            } else if is_enum
                || (is_error
                    && matches!(self.peek(), Tok::Name(_))
                    && matches!(
                        self.peek_at(1),
                        Tok::P(Punct::LParen) | Tok::Newline | Tok::Dedent | Tok::Eof
                    ))
            {
                def.cases.push(self.case()?);
            } else {
                let field = self.field(true)?;
                default_comes_last(&def.fields, &field)?;
                def.fields.push(Field { annotations, ..field });
                self.end_line()?;
            }
        }
        Ok(def)
    }

    /// The annotations above a field or a `def`, each on its own line: `@skip`,
    /// `@serialize(name="uid")`, `@intrinsic`.
    fn annotations(&mut self) -> Result<Vec<crate::ast::Annotation>, Diagnostic> {
        let mut found = Vec::new();
        while self.at_p(Punct::At) {
            let start = self.advance().span;
            let name = self.name("an annotation's name")?;
            let args = if self.eat_p(Punct::LParen) {
                let args = self.call_args()?;
                self.expect_p(Punct::RParen)?;
                args
            } else {
                Vec::new()
            };
            let span = start.to(self.prev_span());
            self.end_line()?;
            found.push(crate::ast::Annotation { name, args, span });
        }
        Ok(found)
    }

    /// A field's name, which may be any word, keywords included: `type: str`. Where a field is
    /// named, after `.` or before `=` in a call, nothing else could be meant.
    pub(crate) fn field_name(&mut self, what: &str) -> Result<String, Diagnostic> {
        match self.peek().clone() {
            Tok::Kw(kw) if !matches!(kw, Kw::Reserved(_)) => {
                self.advance();
                Ok(kw.text().to_owned())
            }
            _ => self.name(what),
        }
    }

    /// `name: type`, `name = default`, or both, with a `where` rule when `rules` allows.
    fn field(&mut self, rules: bool) -> Result<Field, Diagnostic> {
        let start = self.span();
        let name = self.field_name("a field name")?;
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
        Ok(Field { name, ty, default, rule, annotations: Vec::new(), span: start.to(self.prev_span()) })
    }

    fn case(&mut self) -> Result<Case, Diagnostic> {
        let start = self.span();
        let name = self.name("an enum case")?;
        let mut fields = Vec::new();
        if self.eat_p(Punct::LParen) {
            while !self.at_p(Punct::RParen) {
                let field = self.field(false)?;
                default_comes_last(&fields, &field)?;
                fields.push(field);
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
            let _ = self.eat_pub(false).or_else(|| self.eat_export());
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
    /// type checker.
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

    /// `def name(params) -> ret:` and its body, or the signature alone, which only std's
    /// `@intrinsic` functions may be.
    pub(crate) fn def(&mut self) -> Result<FnDef, Diagnostic> {
        let mut function = self.signature()?;
        let bodiless = match self.peek() {
            Tok::Newline => self.peek_at(1) != &Tok::Indent,
            Tok::Eof | Tok::Dedent => true,
            _ => false,
        };
        if bodiless {
            self.end_line()?;
            return Ok(function);
        }
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
            // `def in(self, item)`, which `item in value` calls.
            Tok::Kw(Kw::In) => {
                self.advance();
                "in".to_owned()
            }
            // `def from(x: int) -> u8`, which `u8.from(x)` calls: a name after `.` can be a keyword.
            Tok::Kw(Kw::From) => {
                self.advance();
                "from".to_owned()
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
                return Err(no_default_after_defaults(&param.name, param.span));
            }
            params.push(param);
            if !self.eat_p(Punct::Comma) {
                break;
            }
        }
        self.expect_p(Punct::RParen)?;
        // `def [](mut self, i)` is the `mut` form, which a type can have beside `def [](self, i)`.
        let name = if name == "[]" && params.first().is_some_and(|first| first.is_self && first.is_mut) {
            "[]mut".to_owned()
        } else {
            name
        };
        let arrow = self.eat_p(Punct::Arrow).then(|| self.prev_span());
        let ret = if arrow.is_none() {
            None
        } else if self.at_kw(Kw::With) {
            // `-> with Transaction`: a source, which lends one to its caller's `with` block.
            let start = self.advance().span;
            let lent = self.type_expr()?;
            let span = start.to(self.prev_span());
            Some(TypeExpr { name: "with".to_owned(), args: vec![lent], optional: false, span })
        } else {
            Some(self.type_expr()?)
        };
        // `fails` is a word only here, after the parameters and any return type.
        let fails = if self.at_fails() { Some(self.fails_type()?) } else { None };
        Ok(FnDef {
            name,
            generics,
            params,
            ret,
            arrow,
            fails,
            body: Vec::new(),
            annotations: Vec::new(),
            span: start.to(self.prev_span()),
        })
    }

    fn param(&mut self) -> Result<Param, Diagnostic> {
        let start = self.span();
        let is_mut = self.eat_ref("a parameter");
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
            let message = format!("`ref` parameter `{name}` can't have a default");
            return Err(Diagnostic::new(message, value.span).with_help(
                "a `ref` parameter changes what the caller passes, so a call has to pass something",
            ));
        }
        if !value.is_plain_value() {
            return Err(Diagnostic::new("a parameter's default has to be a plain value", value.span)
                .with_help("write a number, string, `True`, `False` or `None`, and work anything else out in the function"));
        }
        Ok(value)
    }

    /// Whether `fails` comes next. It's a word only after a function's parameters and any
    /// return type.
    pub(super) fn at_fails(&self) -> bool {
        matches!(self.peek(), Tok::Name(word) if word == "fails")
    }

    /// `fails LoadError`, or `fails ParseError | RangeError`: the errors after `fails`. Several
    /// are one written type named `|`.
    fn fails_type(&mut self) -> Result<TypeExpr, Diagnostic> {
        self.advance();
        self.type_expr()
    }

    /// A type, or several joined by `|`: a union, like `Circle | Square`, or the errors after
    /// `fails` or in `Result[int, A | B]`. Either is one written type named `|`.
    pub(crate) fn type_expr(&mut self) -> Result<TypeExpr, Diagnostic> {
        let first = self.member_type()?;
        if !self.at_p(Punct::Pipe) {
            return Ok(first);
        }
        let start = first.span;
        let mut members = vec![first];
        while self.eat_p(Punct::Pipe) {
            members.push(self.member_type()?);
        }
        Ok(TypeExpr {
            name: "|".to_owned(),
            args: members,
            optional: false,
            span: start.to(self.prev_span()),
        })
    }

    /// One type with no `|` around it, unless it's in parentheses, as in `(Circle | Square)?`.
    fn member_type(&mut self) -> Result<TypeExpr, Diagnostic> {
        let start = self.span();
        // `shared Scores`: a value many tasks can reach, each inside a `lock`. A `?` after it
        // makes the shared value optional: `shared Scores?` is `(shared Scores)?`.
        if matches!(self.peek(), Tok::Name(word) if word == "shared")
            && matches!(self.peek_at(1), Tok::Name(_) | Tok::P(Punct::LParen))
        {
            self.advance();
            let mut held = self.member_type()?;
            let optional = std::mem::take(&mut held.optional);
            let span = start.to(self.prev_span());
            return Ok(TypeExpr { name: "shared".to_owned(), args: vec![held], optional, span });
        }
        let mut args = Vec::new();
        let name = if self.eat_kw(Kw::None) {
            "None".to_owned()
        } else if let Tok::Int(size) = self.peek().clone() {
            // An array's size: `16` in `array[float, 16]`.
            self.advance();
            size.to_string()
        } else if self.eat_p(Punct::LParen) {
            while !self.at_p(Punct::RParen) {
                args.push(self.type_expr()?);
                if !self.eat_p(Punct::Comma) {
                    break;
                }
            }
            self.expect_p(Punct::RParen)?;
            // `(Circle | Square)?`: a union in parentheses is the union, so it can be optional, as
            // is `(shared Scores)?`.
            if let [only] = args.as_slice()
                && (only.name == "|" || only.name == "shared")
                && !self.at_p(Punct::Arrow)
            {
                let optional = self.eat_p(Punct::Question);
                return Ok(TypeExpr { optional, span: start.to(self.prev_span()), ..only.clone() });
            }
            // `(int, str) -> bool`: a function type, its return type last. One that can fail
            // says so, as in `(str) -> int fails ParseError`, which is written in as its own
            // `fails` type holding the return type and the errors.
            if self.eat_p(Punct::Arrow) {
                let returns = self.type_expr()?;
                let returns = if self.at_fails() {
                    let errors = self.fails_type()?;
                    let span = returns.span.to(errors.span);
                    TypeExpr { name: "fails".to_owned(), args: vec![returns, errors], optional: false, span }
                } else {
                    returns
                };
                args.push(returns);
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
        Tok::Duration(_) => "a duration".to_owned(),
        Tok::Str(_) | Tok::FStr(_) | Tok::TStr(_) => "a string".to_owned(),
        Tok::Kw(kw) => format!("`{}`", kw.text()),
        Tok::P(punct) => format!("`{}`", punct.text()),
        Tok::Newline => "the end of the line".to_owned(),
        Tok::Indent => "an indented line".to_owned(),
        Tok::Dedent => "the end of the block".to_owned(),
        Tok::Eof => "the end of the file".to_owned(),
    }
}

/// A field with no default after one with a default. Fields, like parameters, put the ones with
/// defaults last, so a value given by position never lands in a defaulted one, and a type can
/// gain a field with a default at its end without changing what any call means.
fn default_comes_last(earlier: &[Field], field: &Field) -> Result<(), Diagnostic> {
    if field.default.is_none() && earlier.iter().any(|earlier| earlier.default.is_some()) {
        return Err(no_default_after_defaults(&field.name, field.span));
    }
    Ok(())
}

fn no_default_after_defaults(name: &str, span: Span) -> Diagnostic {
    Diagnostic::new(format!("`{name}` has no default, so it has to come before the ones that do"), span)
        .with_help("put the fields and parameters with defaults after all the ones without")
}
