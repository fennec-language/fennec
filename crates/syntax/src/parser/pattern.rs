//! `match` expressions and their `case` patterns.

use super::Parser;
use crate::ast::{Expr, ExprKind, MatchArm, Pattern};
use crate::diagnostic::Diagnostic;
use crate::token::{Kw, Punct, Tok};

impl Parser {
    /// Whether `match` here starts a match rather than naming something called `match`.
    pub(super) fn at_match(&self) -> bool {
        self.at_soft("match")
            && !matches!(
                self.peek_at(1),
                Tok::P(
                    Punct::Assign | Punct::LParen | Punct::Dot | Punct::Comma | Punct::RParen | Punct::Colon
                ) | Tok::Newline
                    | Tok::Eof
            )
    }

    pub(super) fn match_expr(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.advance().span;
        let subject = self.or_expr()?;
        self.expect_p(Punct::Colon)?;
        self.expect_block_start()?;
        let mut arms = Vec::new();
        while !self.eat(&Tok::Dedent) && !self.at(&Tok::Eof) {
            if !self.at_soft("case") {
                return Err(self.unexpected("`case`"));
            }
            let arm_start = self.advance().span;
            let pattern = self.pattern()?;
            let guard = if self.eat_kw(Kw::If) { Some(self.expr()?) } else { None };
            self.expect_p(Punct::Colon)?;
            let body = self.block()?;
            arms.push(MatchArm { pattern, guard, body, span: arm_start.to(self.prev_span()) });
        }
        self.block_ended = true;
        let span = start.to(self.prev_span());
        Ok(Expr { kind: ExprKind::Match { subject: Box::new(subject), arms }, span })
    }

    /// A pattern, or several separated by `|`, any of which matches.
    fn pattern(&mut self) -> Result<Pattern, Diagnostic> {
        let first = self.one_pattern()?;
        if !self.at_p(Punct::Pipe) {
            return Ok(first);
        }
        let mut alternatives = vec![first];
        while self.eat_p(Punct::Pipe) {
            alternatives.push(self.one_pattern()?);
        }
        Ok(Pattern::Or(alternatives))
    }

    fn one_pattern(&mut self) -> Result<Pattern, Diagnostic> {
        let token = self.advance();
        let literal = |kind| Ok(Pattern::Literal(Expr { kind, span: token.span }));
        match token.tok {
            Tok::Name(name) if name == "_" => Ok(Pattern::Wildcard),
            Tok::P(Punct::Dot) => {
                let name = self.name("an enum case")?;
                Ok(Pattern::Variant { name, dotted: true, args: self.pattern_args()? })
            }
            // `physics.Hit(amount)`: a case from a module imported whole.
            Tok::Name(mut name) if self.at_p(Punct::Dot) => {
                while self.eat_p(Punct::Dot) {
                    name = format!("{name}.{}", self.name("a name")?);
                }
                Ok(Pattern::Variant { name, dotted: false, args: self.pattern_args()? })
            }
            Tok::Name(name) if name.starts_with(|c: char| c.is_ascii_uppercase()) => {
                Ok(Pattern::Variant { name, dotted: false, args: self.pattern_args()? })
            }
            Tok::Name(name) => Ok(Pattern::Bind(name)),
            Tok::Int(n) => literal(ExprKind::Int(n)),
            Tok::Float(x) => literal(ExprKind::Float(x)),
            Tok::Str(text) => literal(ExprKind::Str(text)),
            Tok::Kw(Kw::True) => literal(ExprKind::Bool(true)),
            Tok::Kw(Kw::False) => literal(ExprKind::Bool(false)),
            Tok::Kw(Kw::None) => literal(ExprKind::None),
            Tok::P(Punct::Minus) => match self.advance().tok {
                Tok::Int(n) => literal(ExprKind::Int(-n)),
                Tok::Float(x) => literal(ExprKind::Float(-x)),
                _ => Err(Diagnostic::new("expected a number after `-`", token.span)),
            },
            Tok::P(Punct::LParen) => {
                let mut items = Vec::new();
                while !self.at_p(Punct::RParen) {
                    items.push(self.pattern()?);
                    if !self.eat_p(Punct::Comma) {
                        break;
                    }
                }
                self.expect_p(Punct::RParen)?;
                Ok(Pattern::Tuple(items))
            }
            _ => {
                self.pos -= 1;
                Err(self.unexpected("a pattern"))
            }
        }
    }

    fn pattern_args(&mut self) -> Result<Vec<Pattern>, Diagnostic> {
        let mut args = Vec::new();
        if self.eat_p(Punct::LParen) {
            while !self.at_p(Punct::RParen) {
                args.push(self.pattern()?);
                if !self.eat_p(Punct::Comma) {
                    break;
                }
            }
            self.expect_p(Punct::RParen)?;
        }
        Ok(args)
    }
}
