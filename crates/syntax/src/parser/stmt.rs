//! Statements and blocks.

use super::Parser;
use crate::ast::{BinOp, Expr, ExprKind, Stmt, StmtKind, TypeExpr, WithName};
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use crate::token::{Kw, Punct, Tok};

impl Parser {
    /// The body after a `:`: an indented block, or one simple statement on the same line.
    pub(super) fn block(&mut self) -> Result<Vec<Stmt>, Diagnostic> {
        if !self.at(&Tok::Newline) {
            return Ok(vec![self.simple_stmt()?]);
        }
        self.expect_block_start()?;
        let mut stmts = Vec::new();
        while !self.eat(&Tok::Dedent) && !self.at(&Tok::Eof) {
            stmts.push(self.stmt()?);
        }
        Ok(stmts)
    }

    pub(super) fn stmt(&mut self) -> Result<Stmt, Diagnostic> {
        match self.peek() {
            Tok::Kw(Kw::If) => self.if_stmt(),
            Tok::Kw(Kw::While) => self.while_stmt(),
            Tok::Kw(Kw::For) => self.for_stmt(),
            Tok::Kw(Kw::With) => self.with_stmt(false),
            // `lock scores as ref current:`, where a name follows `lock`.
            Tok::Name(word) if word == "lock" && matches!(self.peek_at(1), Tok::Name(_)) => {
                self.with_stmt(true)
            }
            Tok::Kw(Kw::Def) => {
                let function = self.def()?;
                let span = function.span;
                Ok(Stmt { kind: StmtKind::Def(function), span })
            }
            _ => self.simple_stmt(),
        }
    }

    fn simple_stmt(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.span();
        let kind = match self.peek().clone() {
            Tok::Kw(Kw::Return) => {
                self.advance();
                let at_end = matches!(self.peek(), Tok::Newline | Tok::Eof | Tok::Dedent);
                StmtKind::Return(if at_end { None } else { Some(self.expr()?) })
            }
            Tok::Kw(Kw::Fail) => self.keyword_with_expr(StmtKind::Fail)?,
            Tok::Kw(Kw::Yield) => self.keyword_with_expr(StmtKind::Yield)?,
            Tok::Kw(Kw::Assert) => self.keyword_with_expr(StmtKind::Assert)?,
            Tok::Kw(Kw::Break) => self.keyword_stmt(StmtKind::Break),
            Tok::Kw(Kw::Continue) => self.keyword_stmt(StmtKind::Continue),
            Tok::Kw(Kw::Pass) => self.keyword_stmt(StmtKind::Pass),
            Tok::Kw(Kw::Mut) => self.mut_declaration()?,
            // `shared scores = Scores()`: a plain value made `shared`, written out.
            Tok::Name(word)
                if word == "shared"
                    && matches!(self.peek_at(1), Tok::Name(_))
                    && matches!(self.peek_at(2), Tok::P(Punct::Assign)) =>
            {
                self.shared_declaration()?
            }
            Tok::Kw(Kw::Ref) => return Err(no_inline_ref(start)),
            Tok::Name(word) if self.is_python_only(&word) => return Err(python_only(&word, start)),
            _ => self.expr_or_assign()?,
        };
        let span = start.to(self.prev_span());
        self.end_line()?;
        Ok(Stmt { kind, span })
    }

    /// `raise x`, `try:` and `except x:` at the start of a statement: Python habits Fennec
    /// catches with a pointer to what to write instead.
    fn is_python_only(&self, word: &str) -> bool {
        matches!(word, "raise" | "try" | "except" | "finally")
            && !matches!(
                self.peek_at(1),
                Tok::P(Punct::Assign | Punct::Dot | Punct::LParen | Punct::LBracket)
            )
    }

    fn keyword_stmt(&mut self, kind: StmtKind) -> StmtKind {
        self.advance();
        kind
    }

    fn keyword_with_expr(&mut self, kind: fn(Expr) -> StmtKind) -> Result<StmtKind, Diagnostic> {
        self.advance();
        Ok(kind(self.expr()?))
    }

    fn mut_declaration(&mut self) -> Result<StmtKind, Diagnostic> {
        self.advance();
        let target = self.expr()?;
        if !matches!(target.kind, ExprKind::Name(_)) {
            return Err(Diagnostic::new("`mut` declares a new name, like `mut count = 0`", target.span));
        }
        let ty = if self.eat_p(Punct::Colon) { Some(self.type_expr()?) } else { None };
        self.expect_p(Punct::Assign)?;
        let value = self.expr()?;
        Ok(StmtKind::Assign { target, op: None, value, declare_mut: true, ty })
    }

    /// An expression on its own, or an assignment. `a, b = pair` unpacks a tuple.
    fn expr_or_assign(&mut self) -> Result<StmtKind, Diagnostic> {
        let mut target = self.expr()?;
        if matches!(target.kind, ExprKind::Name(_)) && self.eat_p(Punct::Colon) {
            let ty = Some(self.type_expr()?);
            self.expect_p(Punct::Assign)?;
            let value = self.expr()?;
            return Ok(StmtKind::Assign { target, op: None, value, declare_mut: false, ty });
        }
        if self.at_p(Punct::Comma) {
            let mut items = vec![target];
            while self.eat_p(Punct::Comma) {
                items.push(self.expr()?);
            }
            let span = items.iter().fold(self.prev_span(), |span, item| span.to(item.span));
            target = Expr { kind: ExprKind::Tuple(items), span };
        }
        let op = match self.peek() {
            Tok::P(Punct::Assign) => None,
            Tok::P(Punct::PlusEq) => Some(BinOp::Add),
            Tok::P(Punct::MinusEq) => Some(BinOp::Sub),
            Tok::P(Punct::StarEq) => Some(BinOp::Mul),
            Tok::P(Punct::SlashEq) => Some(BinOp::Div),
            Tok::P(Punct::AmpEq) => Some(BinOp::BitAnd),
            Tok::P(Punct::PipeEq) => Some(BinOp::BitOr),
            Tok::P(Punct::CaretEq) => Some(BinOp::BitXor),
            Tok::P(Punct::ShiftLeftEq) => Some(BinOp::ShiftLeft),
            Tok::P(Punct::ShiftRightEq) => Some(BinOp::ShiftRight),
            _ => return Ok(StmtKind::Expr(target)),
        };
        self.advance();
        let assignable = match &target.kind {
            ExprKind::Name(_) | ExprKind::Index { .. } | ExprKind::Attr { optional: false, .. } => true,
            ExprKind::Tuple(items) => {
                op.is_none() && items.iter().all(|item| matches!(item.kind, ExprKind::Name(_)))
            }
            _ => false,
        };
        if !assignable {
            return Err(Diagnostic::new("this can't be assigned to", target.span));
        }
        let value = self.expr()?;
        Ok(StmtKind::Assign { target, op, value, declare_mut: false, ty: None })
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
        Ok(Stmt { kind: StmtKind::If { branches, otherwise }, span: start.to(self.prev_span()) })
    }

    fn condition_and_block(&mut self) -> Result<(Expr, Vec<Stmt>), Diagnostic> {
        let cond = self.expr()?;
        self.expect_p(Punct::Colon)?;
        Ok((cond, self.block()?))
    }

    fn while_stmt(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.advance().span;
        let (cond, body) = self.condition_and_block()?;
        Ok(Stmt { kind: StmtKind::While { cond, body }, span: start.to(self.prev_span()) })
    }

    fn for_stmt(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.advance().span;
        let is_mut = self.eat_ref("a loop's name");
        let names = self.loop_names()?;
        let (iter, body) = self.condition_and_block()?;
        Ok(Stmt { kind: StmtKind::For { names, is_mut, iter, body }, span: start.to(self.prev_span()) })
    }

    /// `name` or `a, b`, then `in`.
    pub(super) fn loop_names(&mut self) -> Result<Vec<String>, Diagnostic> {
        let mut names = vec![self.name("a loop variable")?];
        while self.eat_p(Punct::Comma) {
            names.push(self.name("a loop variable")?);
        }
        if !self.eat_kw(Kw::In) {
            return Err(self.unexpected("`in`"));
        }
        Ok(names)
    }

    /// `with game as ref current:`, or several at once: `with a as ref x, b as ref y:`, or
    /// `with a, b as x, y:`.
    /// `with a as x:`, or, for `lock`, `lock a as ref x:`, which takes `shared` values.
    fn with_stmt(&mut self, lock: bool) -> Result<Stmt, Diagnostic> {
        let start = self.advance().span;
        let mut items = vec![self.expr()?];
        while self.eat_p(Punct::Comma) {
            items.push(self.expr()?);
        }
        let mut names = Vec::new();
        if self.eat_kw(Kw::As) {
            loop {
                let at = self.span();
                let is_mut = self.eat_ref("a `with` name");
                let name = self.name("a name after `as`")?;
                names.push(WithName { name, is_mut, span: at.to(self.prev_span()) });
                if !self.eat_p(Punct::Comma) {
                    break;
                }
                // `with a as ref x, b as ref y:`: the next value comes with its own name.
                if names.len() == items.len() && !self.at_with_name() {
                    items.push(self.expr()?);
                    if !self.eat_kw(Kw::As) {
                        return Err(self.unexpected("`as` and a name for it"));
                    }
                }
            }
        }
        let held = items.len();
        let value = if let [one] = items.as_slice() {
            one.clone()
        } else {
            let first = items.first().map_or(start, |item| item.span);
            let span = items.iter().fold(first, |span, item| span.to(item.span));
            Expr { kind: ExprKind::Tuple(items), span }
        };
        if !names.is_empty() && names.len() != held {
            let span = names.iter().fold(value.span, |span, name| span.to(name.span));
            return Err(Diagnostic::new(
                format!("this `with` holds {held} values but names {}", names.len()),
                span,
            )
            .with_help("give each value its own name: `with a as ref x, b as ref y:`"));
        }
        if lock && names.is_empty() {
            return Err(Diagnostic::new("a `lock` names what it holds, for the block", value.span)
                .with_help("write `lock scores as current:` to read it, or `lock scores as ref current:` to change it"));
        }
        self.expect_p(Punct::Colon)?;
        let body = self.block()?;
        Ok(Stmt { kind: StmtKind::With { value, names, body, lock }, span: start.to(self.prev_span()) })
    }

    /// `shared name = value`, written in as `name: shared = value`: a type of `shared` with
    /// nothing in its brackets, which takes the value's type.
    fn shared_declaration(&mut self) -> Result<StmtKind, Diagnostic> {
        let word = self.advance().span;
        let at = self.span();
        let name = self.name("a name after `shared`")?;
        let target = Expr { kind: ExprKind::Name(name), span: at };
        self.expect_p(Punct::Assign)?;
        let value = self.expr()?;
        let ty = TypeExpr { name: "shared".to_owned(), args: Vec::new(), optional: false, span: word };
        Ok(StmtKind::Assign { target, op: None, value, declare_mut: false, ty: Some(ty) })
    }

    /// Whether a name after `as` comes next, as in `with a, b as x, y:`, rather than another
    /// value: `ref` or `mut`, or a name and then `,` or `:`.
    fn at_with_name(&self) -> bool {
        matches!(self.peek(), Tok::Kw(Kw::Ref | Kw::Mut))
            || matches!(self.peek(), Tok::Name(_))
                && matches!(self.peek_at(1), Tok::P(Punct::Comma | Punct::Colon))
    }
}

fn no_inline_ref(span: Span) -> Diagnostic {
    Diagnostic::new("there's no inline `ref`", span).with_help(
        "`ref` is for loops and parameters. To change something in a list, write to it there, like `units[0].health -= 1`; to make a name that can change, write `mut`",
    )
}

fn python_only(word: &str, span: Span) -> Diagnostic {
    let help = match word {
        "raise" => "Fennec has no exceptions: write `fail SomeError(...)`, which returns the error",
        _ => {
            "Fennec has no exceptions: pass errors up with `?`, give a default with `else`, or handle them with `match`"
        }
    };
    Diagnostic::new(format!("Fennec doesn't have `{word}`"), span).with_help(help)
}
