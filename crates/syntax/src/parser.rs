//! Tokens to a syntax tree, by recursive descent. Statements live here; expressions in `expr`.

mod expr;

use crate::ast::{BinOp, ExprKind, FnDef, Module, Param, Stmt, StmtKind, TypeExpr};
use crate::diagnostic::Diagnostic;
use crate::lexer::lex;
use crate::span::Span;
use crate::token::{Kw, Punct, Tok, Token};

/// Parses a whole `.fen` file.
pub fn parse(source: &str) -> Result<Module, Diagnostic> {
    Parser::new(lex(source, 0)?).module()
}

pub(crate) struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    eof: Token,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        let end = tokens.last().map_or_else(Span::default, |token| token.span);
        Parser {
            tokens,
            pos: 0,
            eof: Token {
                tok: Tok::Eof,
                span: end,
            },
        }
    }

    fn current(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or(&self.eof)
    }

    fn peek(&self) -> &Tok {
        &self.current().tok
    }

    fn peek_at(&self, ahead: usize) -> &Tok {
        self.tokens
            .get(self.pos + ahead)
            .map_or(&self.eof.tok, |token| &token.tok)
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
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
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
        Diagnostic::new(
            format!("expected {expected}, found {}", describe(self.peek())),
            self.span(),
        )
    }

    fn name(&mut self, what: &str) -> Result<String, Diagnostic> {
        match self.peek().clone() {
            Tok::Name(name) => {
                self.advance();
                Ok(name)
            }
            Tok::Kw(Kw::Reserved(word)) => Err(self.reserved(word)),
            _ => Err(self.unexpected(what)),
        }
    }

    fn reserved(&self, word: &str) -> Diagnostic {
        Diagnostic::new(
            format!("`{word}` is part of Fennec's design but isn't supported yet"),
            self.span(),
        )
        .with_help("see what's coming at https://seashell86.github.io/fennec/roadmap/")
    }

    fn module(&mut self) -> Result<Module, Diagnostic> {
        let mut functions = Vec::new();
        loop {
            while self.eat(&Tok::Newline) {}
            match self.peek() {
                Tok::Eof => break,
                Tok::Kw(Kw::Def) => functions.push(self.def()?),
                Tok::Kw(Kw::Reserved(word)) => return Err(self.reserved(word)),
                _ => {
                    return Err(Diagnostic::new(
                        "only `def` is supported at the top level so far",
                        self.span(),
                    )
                    .with_help("put this code inside `def main(stdout: Stdout):`"));
                }
            }
        }
        Ok(Module { functions })
    }

    fn def(&mut self) -> Result<FnDef, Diagnostic> {
        let start = self.advance().span;
        let name = self.name("a function name")?;
        self.expect_p(Punct::LParen)?;
        let mut params = Vec::new();
        while !self.at_p(Punct::RParen) {
            params.push(self.param()?);
            if !self.eat_p(Punct::Comma) {
                break;
            }
        }
        self.expect_p(Punct::RParen)?;
        let ret = if self.eat_p(Punct::Arrow) {
            Some(self.type_expr()?)
        } else {
            None
        };
        let span = start.to(self.prev_span());
        self.expect_p(Punct::Colon)?;
        let body = self.block()?;
        Ok(FnDef {
            name,
            params,
            ret,
            body,
            span,
        })
    }

    fn param(&mut self) -> Result<Param, Diagnostic> {
        let start = self.span();
        let is_mut = self.eat_kw(Kw::Mut);
        let name = self.name("a parameter name")?;
        if !self.eat_p(Punct::Colon) {
            return Err(
                Diagnostic::new(format!("parameter `{name}` needs a type"), self.prev_span())
                    .with_help(format!("write it with its type, like `{name}: int`")),
            );
        }
        let ty = self.type_expr()?;
        Ok(Param {
            name,
            ty,
            is_mut,
            span: start.to(self.prev_span()),
        })
    }

    fn type_expr(&mut self) -> Result<TypeExpr, Diagnostic> {
        let start = self.span();
        let name = if self.eat_kw(Kw::None) {
            "None".to_owned()
        } else {
            self.name("a type")?
        };
        let mut args = Vec::new();
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
        Ok(TypeExpr {
            name,
            args,
            optional,
            span: start.to(self.prev_span()),
        })
    }

    /// The body after a `:`: an indented block, or one simple statement on the same line.
    fn block(&mut self) -> Result<Vec<Stmt>, Diagnostic> {
        if !self.eat(&Tok::Newline) {
            return Ok(vec![self.simple_stmt()?]);
        }
        if !self.eat(&Tok::Indent) {
            return Err(Diagnostic::new("expected an indented block", self.span())
                .with_help("indent the body 4 spaces more than the line ending in `:`"));
        }
        let mut stmts = Vec::new();
        while !self.eat(&Tok::Dedent) && !self.at(&Tok::Eof) {
            stmts.push(self.stmt()?);
        }
        Ok(stmts)
    }

    fn stmt(&mut self) -> Result<Stmt, Diagnostic> {
        match self.peek() {
            Tok::Kw(Kw::If) => self.if_stmt(),
            Tok::Kw(Kw::While) => self.while_stmt(),
            Tok::Kw(Kw::For) => self.for_stmt(),
            Tok::Kw(Kw::Def) => Err(Diagnostic::new(
                "nested functions aren't supported yet",
                self.span(),
            )),
            _ => self.simple_stmt(),
        }
    }

    fn simple_stmt(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.span();
        let kind = match self.peek().clone() {
            Tok::Kw(Kw::Return) => {
                self.advance();
                StmtKind::Return(if self.at_line_end() {
                    None
                } else {
                    Some(self.expr()?)
                })
            }
            Tok::Kw(Kw::Break) => self.keyword_stmt(StmtKind::Break),
            Tok::Kw(Kw::Continue) => self.keyword_stmt(StmtKind::Continue),
            Tok::Kw(Kw::Pass) => self.keyword_stmt(StmtKind::Pass),
            Tok::Kw(Kw::Mut) => self.mut_declaration()?,
            Tok::Kw(Kw::Reserved(word)) => return Err(self.reserved(word)),
            _ => self.expr_or_assign()?,
        };
        let span = start.to(self.prev_span());
        if !self.eat(&Tok::Newline) && !matches!(self.peek(), Tok::Eof | Tok::Dedent) {
            return Err(self.unexpected("the end of the line"));
        }
        Ok(Stmt { kind, span })
    }

    fn keyword_stmt(&mut self, kind: StmtKind) -> StmtKind {
        self.advance();
        kind
    }

    fn at_line_end(&self) -> bool {
        matches!(self.peek(), Tok::Newline | Tok::Eof | Tok::Dedent)
    }

    fn mut_declaration(&mut self) -> Result<StmtKind, Diagnostic> {
        self.advance();
        let target = self.expr()?;
        if !matches!(target.kind, ExprKind::Name(_)) {
            return Err(Diagnostic::new(
                "`mut` declares a new name, like `mut count = 0`",
                target.span,
            ));
        }
        self.expect_p(Punct::Assign)?;
        let value = self.expr()?;
        Ok(StmtKind::Assign {
            target,
            op: None,
            value,
            declare_mut: true,
        })
    }

    fn expr_or_assign(&mut self) -> Result<StmtKind, Diagnostic> {
        let target = self.expr()?;
        let op = match self.peek() {
            Tok::P(Punct::Assign) => None,
            Tok::P(Punct::PlusEq) => Some(BinOp::Add),
            Tok::P(Punct::MinusEq) => Some(BinOp::Sub),
            Tok::P(Punct::StarEq) => Some(BinOp::Mul),
            Tok::P(Punct::SlashEq) => Some(BinOp::Div),
            _ => return Ok(StmtKind::Expr(target)),
        };
        self.advance();
        if !matches!(
            target.kind,
            ExprKind::Name(_) | ExprKind::Index { .. } | ExprKind::Attr { .. }
        ) {
            return Err(Diagnostic::new("this can't be assigned to", target.span));
        }
        let value = self.expr()?;
        Ok(StmtKind::Assign {
            target,
            op,
            value,
            declare_mut: false,
        })
    }

    fn if_stmt(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.advance().span;
        let mut branches = vec![self.condition_and_block()?];
        let mut otherwise = None;
        loop {
            if self.eat_kw(Kw::Elif) {
                branches.push(self.condition_and_block()?);
            } else if self.eat_kw(Kw::Else) {
                self.expect_p(Punct::Colon)?;
                otherwise = Some(self.block()?);
                break;
            } else {
                break;
            }
        }
        Ok(Stmt {
            kind: StmtKind::If { branches, otherwise },
            span: start.to(self.prev_span()),
        })
    }

    fn condition_and_block(&mut self) -> Result<(crate::ast::Expr, Vec<Stmt>), Diagnostic> {
        let cond = self.expr()?;
        self.expect_p(Punct::Colon)?;
        Ok((cond, self.block()?))
    }

    fn while_stmt(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.advance().span;
        let (cond, body) = self.condition_and_block()?;
        Ok(Stmt {
            kind: StmtKind::While { cond, body },
            span: start.to(self.prev_span()),
        })
    }

    fn for_stmt(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.advance().span;
        let is_mut = self.eat_kw(Kw::Mut);
        let name = self.name("a loop variable")?;
        if !self.eat_kw(Kw::In) {
            return Err(self.unexpected("`in`"));
        }
        let (iter, body) = self.condition_and_block()?;
        Ok(Stmt {
            kind: StmtKind::For {
                name,
                is_mut,
                iter,
                body,
            },
            span: start.to(self.prev_span()),
        })
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
