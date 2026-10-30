//! Expressions, from lowest precedence to highest: arrow functions, conditional expressions,
//! `or`, `and`, `not`, comparisons, `+ -`, `* / // %`, unary minus, `**`, then calls,
//! attributes and indexing.

use super::Parser;
use crate::ast::{Arg, BinOp, Expr, ExprKind, FStrPart, UnaryOp};
use crate::diagnostic::Diagnostic;
use crate::lexer::lex;
use crate::span::Span;
use crate::token::{FPart, Kw, Punct, Tok};

impl Parser {
    pub(super) fn expr(&mut self) -> Result<Expr, Diagnostic> {
        if let Some(lambda) = self.lambda()? {
            return Ok(lambda);
        }
        let then = self.or_expr()?;
        if !self.eat_kw(Kw::If) {
            return Ok(then);
        }
        let cond = self.or_expr()?;
        if !self.eat_kw(Kw::Else) {
            return Err(
                Diagnostic::new("a conditional expression needs `else`", self.span())
                    .with_help("write `a if condition else b`"),
            );
        }
        let otherwise = self.expr()?;
        let span = then.span.to(otherwise.span);
        Ok(Expr {
            kind: ExprKind::Cond {
                then: Box::new(then),
                cond: Box::new(cond),
                otherwise: Box::new(otherwise),
            },
            span,
        })
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
        Ok(Some(Expr {
            kind: ExprKind::Lambda {
                params,
                body: Box::new(body),
            },
            span,
        }))
    }

    /// Looks ahead for `(a, b) =>`, returning the names and how many tokens they take.
    fn paren_params(&self) -> Option<(Vec<String>, usize)> {
        let mut params = Vec::new();
        let mut at = 1;
        loop {
            match self.peek_at(at) {
                Tok::Name(name) => params.push(name.clone()),
                Tok::P(Punct::RParen) if params.is_empty() => break,
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

    fn or_expr(&mut self) -> Result<Expr, Diagnostic> {
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
            return Ok(Expr {
                kind: ExprKind::Unary {
                    op: UnaryOp::Not,
                    expr: Box::new(expr),
                },
                span,
            });
        }
        self.comparison()
    }

    /// Comparisons chain as in Python: `a < b <= c` means `a < b and b <= c`.
    fn comparison(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.sum()?;
        let mut chain: Option<Expr> = None;
        while let Some(op) = self.comparison_op() {
            let rhs = self.sum()?;
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
        let op = match self.peek() {
            Tok::P(Punct::EqEq) => BinOp::Eq,
            Tok::P(Punct::NotEq) => BinOp::NotEq,
            Tok::P(Punct::Lt) => BinOp::Lt,
            Tok::P(Punct::LtEq) => BinOp::LtEq,
            Tok::P(Punct::Gt) => BinOp::Gt,
            Tok::P(Punct::GtEq) => BinOp::GtEq,
            Tok::Kw(Kw::In) => BinOp::In,
            Tok::Kw(Kw::Not) if self.peek_at(1) == &Tok::Kw(Kw::In) => {
                self.pos += 2;
                return Some(BinOp::NotIn);
            }
            Tok::Kw(Kw::Is) if self.peek_at(1) == &Tok::Kw(Kw::Not) => {
                self.pos += 2;
                return Some(BinOp::IsNot);
            }
            Tok::Kw(Kw::Is) => BinOp::Is,
            _ => return None,
        };
        self.pos += 1;
        Some(op)
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
            return Ok(Expr {
                kind: ExprKind::Unary {
                    op: UnaryOp::Neg,
                    expr: Box::new(expr),
                },
                span,
            });
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
            if self.eat_p(Punct::LParen) {
                let args = self.call_args()?;
                let span = expr.span.to(self.expect_p(Punct::RParen)?);
                expr = Expr {
                    kind: ExprKind::Call {
                        callee: Box::new(expr),
                        args,
                    },
                    span,
                };
            } else if self.eat_p(Punct::Dot) {
                let name = self.name("a name after `.`")?;
                let span = expr.span.to(self.prev_span());
                expr = Expr {
                    kind: ExprKind::Attr {
                        object: Box::new(expr),
                        name,
                    },
                    span,
                };
            } else if self.eat_p(Punct::LBracket) {
                let index = self.expr()?;
                let span = expr.span.to(self.expect_p(Punct::RBracket)?);
                expr = Expr {
                    kind: ExprKind::Index {
                        object: Box::new(expr),
                        index: Box::new(index),
                    },
                    span,
                };
            } else {
                return Ok(expr);
            }
        }
    }

    /// Arguments up to (not including) `)`. A single argument followed by `for` is a generator
    /// expression, as in `sum(x * x for x in xs)`.
    fn call_args(&mut self) -> Result<Vec<Arg>, Diagnostic> {
        let mut args = Vec::new();
        while !self.at_p(Punct::RParen) {
            let is_mut = self.eat_kw(Kw::Mut);
            let name = match (self.peek().clone(), self.peek_at(1)) {
                (Tok::Name(name), Tok::P(Punct::Assign)) if !is_mut => {
                    self.pos += 2;
                    Some(name)
                }
                _ => None,
            };
            let mut value = self.expr()?;
            if args.is_empty() && name.is_none() && !is_mut && self.at_kw(Kw::For) {
                value = self.comprehension(value)?;
            }
            args.push(Arg { name, is_mut, value });
            if !self.eat_p(Punct::Comma) {
                break;
            }
        }
        Ok(args)
    }

    /// The `for var in iter if cond` part of a comprehension, after its element.
    fn comprehension(&mut self, elem: Expr) -> Result<Expr, Diagnostic> {
        self.advance();
        let var = self.name("a loop variable")?;
        if !self.eat_kw(Kw::In) {
            return Err(self.unexpected("`in`"));
        }
        let iter = self.or_expr()?;
        let cond = if self.eat_kw(Kw::If) {
            Some(Box::new(self.or_expr()?))
        } else {
            None
        };
        let span = elem.span.to(self.prev_span());
        Ok(Expr {
            kind: ExprKind::ListComp {
                elem: Box::new(elem),
                var,
                iter: Box::new(iter),
                cond,
            },
            span,
        })
    }

    fn atom(&mut self) -> Result<Expr, Diagnostic> {
        let token = self.advance();
        let span = token.span;
        let kind = match token.tok {
            Tok::Int(value) => ExprKind::Int(value),
            Tok::Float(value) => ExprKind::Float(value),
            Tok::Str(text) => ExprKind::Str(text),
            Tok::FStr(parts) => ExprKind::FStr(fstring_parts(parts)?),
            Tok::Kw(Kw::True) => ExprKind::Bool(true),
            Tok::Kw(Kw::False) => ExprKind::Bool(false),
            Tok::Kw(Kw::None) => ExprKind::None,
            Tok::Name(name) => ExprKind::Name(name),
            Tok::P(Punct::LParen) => {
                let inner = self.expr()?;
                self.expect_p(Punct::RParen)?;
                return Ok(inner);
            }
            Tok::P(Punct::LBracket) => return self.list(span),
            Tok::Kw(Kw::Reserved(word)) => {
                self.pos -= 1;
                return Err(self.reserved(word));
            }
            other => {
                self.pos -= 1;
                return Err(Diagnostic::new(
                    format!("expected a value, found {}", super::describe(&other)),
                    span,
                ));
            }
        };
        Ok(Expr { kind, span })
    }

    fn list(&mut self, start: Span) -> Result<Expr, Diagnostic> {
        let mut items = Vec::new();
        while !self.at_p(Punct::RBracket) {
            let item = self.expr()?;
            if items.is_empty() && self.at_kw(Kw::For) {
                let comprehension = self.comprehension(item)?;
                let span = start.to(self.expect_p(Punct::RBracket)?);
                return Ok(Expr {
                    span,
                    ..comprehension
                });
            }
            items.push(item);
            if !self.eat_p(Punct::Comma) {
                break;
            }
        }
        let span = start.to(self.expect_p(Punct::RBracket)?);
        Ok(Expr {
            kind: ExprKind::List(items),
            span,
        })
    }
}

fn binary(op: BinOp, lhs: Expr, rhs: Expr) -> Expr {
    let span = lhs.span.to(rhs.span);
    Expr {
        kind: ExprKind::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        },
        span,
    }
}

fn fstring_parts(parts: Vec<FPart>) -> Result<Vec<FStrPart>, Diagnostic> {
    parts
        .into_iter()
        .map(|part| match part {
            FPart::Text(text) => Ok(FStrPart::Text(text)),
            FPart::Expr { source, offset, spec } => {
                if source.trim().is_empty() {
                    return Err(Diagnostic::new(
                        "this f-string has an empty `{}`",
                        Span {
                            start: offset,
                            end: offset + 1,
                        },
                    )
                    .with_help("put an expression inside, or write `{{` for a literal brace"));
                }
                let mut parser = Parser::new(lex(&source, offset)?);
                let expr = parser.expr()?;
                parser.eat(&Tok::Newline);
                if !parser.at(&Tok::Eof) {
                    return Err(parser.unexpected("`}`"));
                }
                Ok(FStrPart::Expr { expr, spec })
            }
        })
        .collect()
}
