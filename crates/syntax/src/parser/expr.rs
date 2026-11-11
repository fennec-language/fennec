//! Expressions, from lowest precedence to highest: arrow functions and `match`; conditional
//! expressions and `else` defaults; `or`; `and`; `not`; comparisons; `|`; `^`; `&`; `<< >>`;
//! `+ -`; `* / // %`; unary `-` and `~`; `**`; then calls, attributes, indexing, `?` and `?.`.

use super::Parser;
use crate::ast::{
    Arg, BinOp, CompKind, Expr, ExprKind, FStrPart, MatchArm, Pattern, Stmt, StmtKind, UnaryOp,
};
use crate::diagnostic::Diagnostic;
use crate::lexer::lex;
use crate::span::Span;
use crate::token::{FPart, Kw, Punct, Tok};

/// What an expression made at its top, outside any parentheses.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Formed {
    /// `a if condition else b`.
    Conditional,
    /// `value else default`.
    Fallback,
    Plain,
}

/// A one-line `if` and a fallback `else` in one expression, without parentheses saying which
/// holds which: `int(text) else 80 if given else 8080` reads two ways. The fix writes the
/// parentheses around `inner`, the way it's read.
fn mixed_else(inner: &Expr) -> Diagnostic {
    let at_end = Span { start: inner.span.end, end: inner.span.end };
    let at_start = Span { start: inner.span.start, end: inner.span.start };
    Diagnostic::new(
        "a one-line `if` and a fallback `else` need parentheses to share an expression",
        inner.span,
    )
    .with_help("put this part in parentheses, or give the `if` or the fallback a line of its own")
    .with_fix(at_start, "(")
    .with_fix(at_end, ")")
}

impl Parser {
    pub(super) fn expr(&mut self) -> Result<Expr, Diagnostic> {
        self.expr_made().map(|(made, _)| made)
    }

    /// An expression, and whether this call made a one-line `if` or a fallback `else` of its own,
    /// rather than reading one inside parentheses.
    fn expr_made(&mut self) -> Result<(Expr, Formed), Diagnostic> {
        if self.at_match() {
            return Ok((self.match_expr()?, Formed::Plain));
        }
        if let Some(lambda) = self.lambda()? {
            return Ok((lambda, Formed::Plain));
        }
        let value = self.or_expr()?;
        if self.eat_kw(Kw::If) {
            let cond = self.or_expr()?;
            if !self.eat_kw(Kw::Else) {
                return Err(Diagnostic::new("a conditional expression needs `else`", self.span())
                    .with_help("write `a if condition else b`"));
            }
            let (otherwise, made) = self.expr_made()?;
            if made == Formed::Fallback {
                return Err(mixed_else(&otherwise));
            }
            let span = value.span.to(otherwise.span);
            let kind = ExprKind::Cond {
                then: Box::new(value),
                cond: Box::new(cond),
                otherwise: Box::new(otherwise),
            };
            return Ok((Expr { kind, span }, Formed::Conditional));
        }
        if self.eat_kw(Kw::Else) {
            let default = match self.peek() {
                Tok::Kw(Kw::Continue | Kw::Break | Kw::Return | Kw::Fail) => self.leaving()?,
                Tok::P(Punct::Colon) => self.leaving_block(None)?,
                Tok::Kw(Kw::As) => {
                    self.advance();
                    let failure = self.name("a name for the failure after `else as`")?;
                    if !self.at(&Tok::P(Punct::Colon)) {
                        return Err(Diagnostic::new("`else as` needs a block", self.span())
                            .with_help("write `else as failure:` and the block under it, which leaves"));
                    }
                    self.leaving_block(Some(failure))?
                }
                _ => {
                    let (default, made) = self.expr_made()?;
                    if made == Formed::Conditional {
                        return Err(mixed_else(&default));
                    }
                    default
                }
            };
            let span = value.span.to(default.span);
            let kind = ExprKind::Else { value: Box::new(value), default: Box::new(default), except: false };
            return Ok((Expr { kind, span }, Formed::Fallback));
        }
        if self.at(&Tok::Kw(Kw::Except)) {
            let handler = self.except_handler()?;
            let span = value.span.to(handler.span);
            let kind = ExprKind::Else { value: Box::new(value), default: Box::new(handler), except: true };
            return Ok((Expr { kind, span }, Formed::Fallback));
        }
        Ok((value, Formed::Plain))
    }

    /// What follows `except`, written in as the same one-case `match` an `else` block is: any
    /// failure, `except why:` with a block of statements or of `case` arms, which becomes a
    /// `match why:` in the block, or one case, as `except InvalidValue(): 8080`, whose pattern is
    /// the case's.
    fn except_handler(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.advance().span;
        if let (Tok::Name(name), Tok::P(Punct::Colon)) = (self.peek().clone(), self.peek_at(1))
            && name.starts_with(char::is_uppercase)
        {
            let help = format!(
                "write `except {name}():` to handle only `{name}`, or a lowercase name to handle any failure"
            );
            return Err(Diagnostic::new(
                format!("`{name}` after `except` would name any failure"),
                self.span(),
            )
            .with_help(help));
        }
        if let (Tok::Name(name), Tok::P(Punct::Colon)) = (self.peek().clone(), self.peek_at(1))
            && name != "_"
        {
            let at = self.advance().span;
            return self.named_handler(start, (name, at));
        }
        if self.at_p(Punct::Colon) {
            return Err(Diagnostic::new("`except` names the failure it handles", self.span())
                .with_help("write `except why:` and use `why` in the block, or name one case, as `except InvalidValue(): 8080`"));
        }
        let pattern = self.pattern()?;
        if !self.at_p(Punct::Colon) {
            return Err(Diagnostic::new("`except` and its case need a `:`", self.span())
                .with_help("write the case, a `:` and what it gives, as `except InvalidValue(): 8080`"));
        }
        let arm_start = self.advance().span;
        let indented = self.at(&Tok::Newline);
        let body = self.block()?;
        // On the line, the case's statement has ended the line, and the statement it's in with it.
        self.block_ended = !indented || self.block_ended;
        let span = arm_start.to(self.prev_span());
        // The failure has a name nobody can write, as the one case's `match` is over it.
        let failure = format!("except{}failure", crate::visit::HIDDEN);
        let arm = MatchArm { pattern, guard: None, body, span };
        Ok(self.handler_over(start, (failure, start), vec![arm]))
    }

    /// `except why:` and its block, of statements, or of `case` arms, which are a `match why:`
    /// in the block.
    fn named_handler(&mut self, start: Span, (failure, at): (String, Span)) -> Result<Expr, Diagnostic> {
        let arms_follow = matches!(self.peek_at(1), Tok::Newline)
            && matches!(self.peek_at(2), Tok::Indent)
            && matches!(self.peek_at(3), Tok::Name(word) if word == "case");
        if !arms_follow {
            let mut handler = self.leaving_block(Some(failure))?;
            handler.span = start.to(handler.span);
            return Ok(handler);
        }
        self.advance();
        self.expect_block_start()?;
        let arms = self.arms()?;
        Ok(self.handler_over(start, (failure, at), arms))
    }

    /// A handler whose block is a `match` over the failure, named `failure`, with these arms.
    fn handler_over(&self, start: Span, (failure, at): (String, Span), arms: Vec<MatchArm>) -> Expr {
        let span = start.to(self.prev_span());
        let subject = Box::new(Expr { kind: ExprKind::Name(failure.clone()), span: at });
        let matched = Expr { kind: ExprKind::Match { subject, arms }, span };
        let mut handler = leaving_match(start, vec![Stmt { kind: StmtKind::Expr(matched), span }], span);
        if let ExprKind::Match { arms, .. } = &mut handler.kind
            && let Some(arm) = arms.first_mut()
        {
            arm.pattern = Pattern::Bind(failure);
        }
        handler
    }

    /// `continue`, `break`, `return` or `fail` after `else`, as a `match` with one case that leaves, which
    /// every engine and analysis already follows out of the expression.
    fn leaving(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.span();
        let word = self.peek().clone();
        self.advance();
        let kind = match word {
            Tok::Kw(Kw::Continue) => StmtKind::Continue,
            Tok::Kw(Kw::Break) => StmtKind::Break,
            Tok::Kw(Kw::Fail) => StmtKind::Fail(self.expr()?),
            _ if matches!(self.peek(), Tok::Newline | Tok::Eof | Tok::Dedent) => StmtKind::Return(None),
            _ => StmtKind::Return(Some(self.expr()?)),
        };
        let span = start.to(self.prev_span());
        Ok(leaving_match(start, vec![Stmt { kind, span }], span))
    }

    /// An `else:` block, which leaves or gives the value, as in `text = fs.read(path) else:` with
    /// `stdout.print(...)` and `return` under it, or `""` as its last line. It's the same `match`
    /// as `else return`, with more statements in its case, and the checker sees that it leaves or
    /// gives a value of the right type. `else as failure:` names what failed in the block.
    fn leaving_block(&mut self, failure: Option<String>) -> Result<Expr, Diagnostic> {
        let start = self.advance().span;
        let indented = self.at(&Tok::Newline);
        let body = self.block()?;
        self.block_ended = indented;
        let span = start.to(self.prev_span());
        let mut block = leaving_match(start, body, span);
        if let (ExprKind::Match { arms, .. }, Some(failure)) = (&mut block.kind, failure)
            && let Some(arm) = arms.first_mut()
        {
            arm.pattern = Pattern::Bind(failure);
        }
        Ok(block)
    }

    /// `x => body` or `(a, b) => body`.
    fn lambda(&mut self) -> Result<Option<Expr>, Diagnostic> {
        let start = self.span();
        let params = match (self.peek().clone(), self.peek_at(1)) {
            (Tok::Name(name), Tok::P(Punct::FatArrow)) => {
                self.pos += 2;
                vec![name]
            }
            (Tok::P(Punct::LParen), _) => match self.paren_params() {
                Some((params, used)) => {
                    self.pos += used;
                    params
                }
                None => return Ok(None),
            },
            _ => return Ok(None),
        };
        let body = self.expr()?;
        let span = start.to(body.span);
        Ok(Some(Expr { kind: ExprKind::Lambda { params, body: Box::new(body) }, span }))
    }

    /// Looks ahead for `(a, b) =>`, returning the names and how many tokens they take. A
    /// trailing comma is fine, as it is in `def` parameters.
    fn paren_params(&self) -> Option<(Vec<String>, usize)> {
        let mut params = Vec::new();
        let mut at = 1;
        loop {
            match self.peek_at(at) {
                Tok::Name(name) => params.push(name.clone()),
                Tok::P(Punct::RParen) => break,
                _ => return None,
            }
            at += 1;
            match self.peek_at(at) {
                Tok::P(Punct::Comma) => at += 1,
                Tok::P(Punct::RParen) => break,
                _ => return None,
            }
        }
        (self.peek_at(at + 1) == &Tok::P(Punct::FatArrow)).then_some((params, at + 2))
    }

    pub(super) fn or_expr(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.and_expr()?;
        while self.eat_kw(Kw::Or) {
            let rhs = self.and_expr()?;
            lhs = binary(BinOp::Or, lhs, rhs);
        }
        Ok(lhs)
    }

    fn and_expr(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.not_expr()?;
        while self.eat_kw(Kw::And) {
            let rhs = self.not_expr()?;
            lhs = binary(BinOp::And, lhs, rhs);
        }
        Ok(lhs)
    }

    fn not_expr(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.span();
        if self.eat_kw(Kw::Not) {
            let expr = self.not_expr()?;
            let span = start.to(expr.span);
            return Ok(Expr { kind: ExprKind::Unary { op: UnaryOp::Not, expr: Box::new(expr) }, span });
        }
        self.comparison()
    }

    /// Comparisons chain as in Python: `a < b <= c` means `a < b and b <= c`.
    fn comparison(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.bit_or()?;
        let mut chain: Option<Expr> = None;
        while let Some(op) = self.comparison_op() {
            let rhs = self.bit_or()?;
            let pair = binary(op, lhs, rhs.clone());
            chain = Some(match chain {
                Some(previous) => binary(BinOp::And, previous, pair),
                None => pair,
            });
            lhs = rhs;
        }
        Ok(chain.unwrap_or(lhs))
    }

    fn comparison_op(&mut self) -> Option<BinOp> {
        let (op, used) = match (self.peek(), self.peek_at(1)) {
            (Tok::P(Punct::EqEq), _) => (BinOp::Eq, 1),
            (Tok::P(Punct::NotEq), _) => (BinOp::NotEq, 1),
            (Tok::P(Punct::Lt), _) => (BinOp::Lt, 1),
            (Tok::P(Punct::LtEq), _) => (BinOp::LtEq, 1),
            (Tok::P(Punct::Gt), _) => (BinOp::Gt, 1),
            (Tok::P(Punct::GtEq), _) => (BinOp::GtEq, 1),
            (Tok::Kw(Kw::In), _) => (BinOp::In, 1),
            (Tok::Kw(Kw::Not), Tok::Kw(Kw::In)) => (BinOp::NotIn, 2),
            (Tok::Kw(Kw::Is), Tok::Kw(Kw::Not)) => (BinOp::IsNot, 2),
            (Tok::Kw(Kw::Is), _) => (BinOp::Is, 1),
            _ => return None,
        };
        self.pos += used;
        Some(op)
    }

    fn bit_or(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.bit_xor()?;
        while self.eat_p(Punct::Pipe) {
            let rhs = self.bit_xor()?;
            lhs = binary(BinOp::BitOr, lhs, rhs);
        }
        Ok(lhs)
    }

    fn bit_xor(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.bit_and()?;
        while self.eat_p(Punct::Caret) {
            let rhs = self.bit_and()?;
            lhs = binary(BinOp::BitXor, lhs, rhs);
        }
        Ok(lhs)
    }

    fn bit_and(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.shift()?;
        while self.eat_p(Punct::Amp) {
            let rhs = self.shift()?;
            lhs = binary(BinOp::BitAnd, lhs, rhs);
        }
        Ok(lhs)
    }

    fn shift(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.sum()?;
        loop {
            let op = match self.peek() {
                Tok::P(Punct::ShiftLeft) => BinOp::ShiftLeft,
                Tok::P(Punct::ShiftRight) => BinOp::ShiftRight,
                _ => return Ok(lhs),
            };
            self.advance();
            let rhs = self.sum()?;
            lhs = binary(op, lhs, rhs);
        }
    }

    fn sum(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.term()?;
        loop {
            let op = match self.peek() {
                Tok::P(Punct::Plus) => BinOp::Add,
                Tok::P(Punct::Minus) => BinOp::Sub,
                _ => return Ok(lhs),
            };
            self.advance();
            let rhs = self.term()?;
            lhs = binary(op, lhs, rhs);
        }
    }

    fn term(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.unary()?;
        loop {
            let op = match self.peek() {
                Tok::P(Punct::Star) => BinOp::Mul,
                Tok::P(Punct::Slash) => BinOp::Div,
                Tok::P(Punct::SlashSlash) => BinOp::FloorDiv,
                Tok::P(Punct::Percent) => BinOp::Mod,
                _ => return Ok(lhs),
            };
            self.advance();
            let rhs = self.unary()?;
            lhs = binary(op, lhs, rhs);
        }
    }

    fn unary(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.span();
        if self.eat_p(Punct::Minus) {
            let expr = self.unary()?;
            let span = start.to(expr.span);
            return Ok(Expr { kind: ExprKind::Unary { op: UnaryOp::Neg, expr: Box::new(expr) }, span });
        }
        if self.eat_p(Punct::Tilde) {
            let expr = self.unary()?;
            let span = start.to(expr.span);
            return Ok(Expr { kind: ExprKind::Unary { op: UnaryOp::BitNot, expr: Box::new(expr) }, span });
        }
        let base = self.postfix()?;
        if self.eat_p(Punct::StarStar) {
            let exponent = self.unary()?;
            return Ok(binary(BinOp::Pow, base, exponent));
        }
        Ok(base)
    }

    fn postfix(&mut self) -> Result<Expr, Diagnostic> {
        let mut expr = self.atom()?;
        loop {
            expr = if self.eat_p(Punct::LParen) {
                let args = self.call_args()?;
                let span = expr.span.to(self.expect_p(Punct::RParen)?);
                Expr { kind: ExprKind::Call { callee: Box::new(expr), args }, span }
            } else if self.eat_p(Punct::Dot) {
                let name = self.field_name("a name after `.`")?;
                let span = expr.span.to(self.prev_span());
                Expr { kind: ExprKind::Attr { object: Box::new(expr), name, optional: false }, span }
            } else if self.eat_p(Punct::LBracket) {
                self.index(expr)?
            } else if self.eat_p(Punct::Question) {
                let context = if self.eat_kw(Kw::From) { Some(Box::new(self.postfix()?)) } else { None };
                let span = expr.span.to(self.prev_span());
                Expr { kind: ExprKind::Try { value: Box::new(expr), context }, span }
            } else {
                return Ok(expr);
            };
        }
    }

    /// `object[index]` or a slice `object[start:end]`, after the `[`. `object[a, b]` indexes
    /// with the tuple `(a, b)`, which is also how types with several parts are written, as in
    /// `json.loads[dict[str, int]]`.
    fn index(&mut self, object: Expr) -> Result<Expr, Diagnostic> {
        let mut start = if self.at_p(Punct::Colon) { None } else { Some(Box::new(self.expr()?)) };
        if let Some(first) = start.take_if(|_| self.at_p(Punct::Comma)) {
            let mut items = vec![*first];
            while self.eat_p(Punct::Comma) && !self.at_p(Punct::RBracket) {
                items.push(self.expr()?);
            }
            let span = items.iter().fold(self.prev_span(), |span, item| span.to(item.span));
            start = Some(Box::new(Expr { kind: ExprKind::Tuple(items), span }));
        }
        let slice_end = if self.eat_p(Punct::Colon) {
            Some(if self.at_p(Punct::RBracket) { None } else { Some(Box::new(self.expr()?)) })
        } else {
            None
        };
        let span = object.span.to(self.expect_p(Punct::RBracket)?);
        let object = Box::new(object);
        let kind = match (slice_end, start) {
            (Some(end), start) => ExprKind::Slice { object, start, end },
            (None, Some(index)) => ExprKind::Index { object, index },
            (None, None) => return Err(self.unexpected("an index")),
        };
        Ok(Expr { kind, span })
    }

    /// Arguments up to (not including) `)`. A single argument followed by `for` is a generator
    /// expression, as in `sum(x * x for x in xs)`.
    pub(crate) fn call_args(&mut self) -> Result<Vec<Arg>, Diagnostic> {
        let mut args = Vec::new();
        while !self.at_p(Punct::RParen) {
            let is_mut = self.eat_ref("an argument");
            let name = match (self.peek().clone(), self.peek_at(1)) {
                (Tok::Name(name), Tok::P(Punct::Assign)) if !is_mut => {
                    self.pos += 2;
                    Some(name)
                }
                // A field named by a keyword, like `User(type="admin")`.
                (Tok::Kw(kw), Tok::P(Punct::Assign)) if !is_mut && !matches!(kw, Kw::Reserved(_)) => {
                    self.pos += 2;
                    Some(kw.text().to_owned())
                }
                _ => None,
            };
            let mut value = self.expr()?;
            if args.is_empty() && name.is_none() && !is_mut && self.at_kw(Kw::For) {
                value = self.comprehension(CompKind::Generator, value, None)?;
            }
            args.push(Arg { name, is_mut, value });
            if !self.eat_p(Punct::Comma) {
                break;
            }
        }
        Ok(args)
    }

    /// The `for names in iter if cond` part of a comprehension, after its element.
    fn comprehension(&mut self, kind: CompKind, elem: Expr, value: Option<Expr>) -> Result<Expr, Diagnostic> {
        self.advance();
        let names = self.loop_names()?;
        let iter = self.or_expr()?;
        let cond = if self.eat_kw(Kw::If) { Some(Box::new(self.or_expr()?)) } else { None };
        let span = elem.span.to(self.prev_span());
        let kind = ExprKind::Comprehension {
            kind,
            elem: Box::new(elem),
            value: value.map(Box::new),
            names,
            iter: Box::new(iter),
            cond,
        };
        Ok(Expr { kind, span })
    }

    fn atom(&mut self) -> Result<Expr, Diagnostic> {
        let token = self.advance();
        let span = token.span;
        let kind = match token.tok {
            Tok::Int(value) => ExprKind::Int(value),
            Tok::Float(value) => ExprKind::Float(value),
            Tok::Duration(nanos) => ExprKind::Duration(nanos),
            Tok::Str(text) => ExprKind::Str(text),
            Tok::FStr(parts) => ExprKind::FStr(fstring_parts(parts)?),
            Tok::TStr(parts) => return super::template::template(parts, span),
            Tok::Kw(Kw::True) => ExprKind::Bool(true),
            Tok::Kw(Kw::False) => ExprKind::Bool(false),
            Tok::Kw(Kw::None) => ExprKind::None,
            Tok::Name(name) => ExprKind::Name(name),
            Tok::P(Punct::Dot) => ExprKind::Variant(self.name("an enum case after `.`")?),
            Tok::P(Punct::LParen) => return self.parenthesized(span),
            Tok::P(Punct::LBracket) => return self.list(span),
            Tok::P(Punct::LBrace) => return self.braces(span),
            Tok::Kw(Kw::Reserved(word)) => {
                self.pos -= 1;
                return Err(self.reserved(word));
            }
            other => {
                return Err(Diagnostic::new(
                    format!("expected a value, found {}", super::describe(&other)),
                    span,
                ));
            }
        };
        Ok(Expr { kind, span: span.to(self.prev_span()) })
    }

    /// `(expr)`, or a tuple: `()`, `(a,)`, `(a, b)`.
    fn parenthesized(&mut self, start: Span) -> Result<Expr, Diagnostic> {
        let mut items = Vec::new();
        let mut trailing_comma = false;
        while !self.at_p(Punct::RParen) {
            let item = self.expr()?;
            // `(x for x in xs)`: a generator, to keep and use later.
            if items.is_empty() && self.at_kw(Kw::For) {
                let made = self.comprehension(CompKind::Generator, item, None)?;
                let span = start.to(self.expect_p(Punct::RParen)?);
                return Ok(Expr { span, ..made });
            }
            items.push(item);
            trailing_comma = self.eat_p(Punct::Comma);
            if !trailing_comma {
                break;
            }
        }
        let span = start.to(self.expect_p(Punct::RParen)?);
        if items.len() == 1 && !trailing_comma {
            return Ok(items.into_iter().next().unwrap_or_else(|| unreachable_tuple(span)));
        }
        Ok(Expr { kind: ExprKind::Tuple(items), span })
    }

    fn list(&mut self, start: Span) -> Result<Expr, Diagnostic> {
        let mut items = Vec::new();
        while !self.at_p(Punct::RBracket) {
            let item = self.expr()?;
            if items.is_empty() && self.at_kw(Kw::For) {
                let comprehension = self.comprehension(CompKind::List, item, None)?;
                let span = start.to(self.expect_p(Punct::RBracket)?);
                return Ok(Expr { span, ..comprehension });
            }
            items.push(item);
            if !self.eat_p(Punct::Comma) {
                break;
            }
        }
        let span = start.to(self.expect_p(Punct::RBracket)?);
        Ok(Expr { kind: ExprKind::List(items), span })
    }

    /// `{}` (an empty dict), `{k: v, ...}`, `{a, b}`, or a dict or set comprehension.
    fn braces(&mut self, start: Span) -> Result<Expr, Diagnostic> {
        if self.at_p(Punct::RBrace) {
            let span = start.to(self.advance().span);
            return Ok(Expr { kind: ExprKind::Dict(Vec::new()), span });
        }
        let first = self.expr()?;
        let is_dict = self.eat_p(Punct::Colon);
        let first_value = if is_dict { Some(self.expr()?) } else { None };
        if self.at_kw(Kw::For) {
            let kind = if is_dict { CompKind::Dict } else { CompKind::Set };
            let comprehension = self.comprehension(kind, first, first_value)?;
            let span = start.to(self.expect_p(Punct::RBrace)?);
            return Ok(Expr { span, ..comprehension });
        }
        let kind = if let Some(value) = first_value {
            let mut pairs = vec![(first, value)];
            while self.eat_p(Punct::Comma) && !self.at_p(Punct::RBrace) {
                let key = self.expr()?;
                self.expect_p(Punct::Colon)?;
                pairs.push((key, self.expr()?));
            }
            ExprKind::Dict(pairs)
        } else {
            let mut items = vec![first];
            while self.eat_p(Punct::Comma) && !self.at_p(Punct::RBrace) {
                items.push(self.expr()?);
            }
            ExprKind::Set(items)
        };
        let span = start.to(self.expect_p(Punct::RBrace)?);
        Ok(Expr { kind, span })
    }
}

fn binary(op: BinOp, lhs: Expr, rhs: Expr) -> Expr {
    let span = lhs.span.to(rhs.span);
    Expr { kind: ExprKind::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) }, span }
}

fn unreachable_tuple(span: Span) -> Expr {
    Expr { kind: ExprKind::Tuple(Vec::new()), span }
}

fn fstring_parts(parts: Vec<FPart>) -> Result<Vec<FStrPart>, Diagnostic> {
    parts
        .into_iter()
        .map(|part| match part {
            FPart::Text(text) => Ok(FStrPart::Text(text)),
            FPart::Expr { source, offset, spec } => {
                Ok(FStrPart::Expr { expr: interpolated(&source, offset, "f-string")?, spec })
            }
        })
        .collect()
}

/// The expression written in an f-string's or template's `{}`, from the source the lexer kept.
pub(super) fn interpolated(source: &str, offset: u32, what: &str) -> Result<Expr, Diagnostic> {
    if source.trim().is_empty() {
        return Err(Diagnostic::new(
            format!("this {what} has an empty `{{}}`"),
            Span { start: offset, end: offset + 1 },
        )
        .with_help("put an expression inside, or write `{{` for a literal brace"));
    }
    // Lexed on its own, a leading space would read as indentation.
    let trimmed = source.trim_start();
    let skipped = source.len() - trimmed.len();
    let offset = Span::new(skipped, skipped).shifted(offset).start;
    let mut parser = Parser::new(lex(trimmed, offset)?);
    let expr = parser.expr()?;
    parser.eat(&Tok::Newline);
    if !parser.at(&Tok::Eof) {
        return Err(parser.unexpected("`}`"));
    }
    if let Some(old_mut) = parser.old_mut_error() {
        return Err(old_mut);
    }
    Ok(expr)
}

/// What a default that leaves parses to: a `match` with one case, holding the statements that
/// leave, which every engine and analysis already follows out of the expression.
pub(super) fn leaving_match(start: Span, body: Vec<Stmt>, span: Span) -> Expr {
    let arm = MatchArm { pattern: Pattern::Wildcard, guard: None, body, span };
    // Expressions are told apart by span, and nothing's written here, so it's empty.
    let subject =
        Box::new(Expr { kind: ExprKind::Bool(false), span: Span { start: start.start, end: start.start } });
    Expr { kind: ExprKind::Match { subject, arms: vec![arm] }, span }
}
