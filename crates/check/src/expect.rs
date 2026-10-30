//! The type expected where an expression is written: a parameter's, a field's, the return
//! type, a name's declared type, or the other side of `==`. It decides which enum a
//! leading-dot case belongs to when more than one enum has a case by that name:
//!
//! ```fennec
//! def literal(tok: Tok) -> ExprKind:
//!     match tok:
//!         case .Int(value): .Int(value)   # the return type makes it ExprKind.Int
//! ```

use fennec_syntax::{Expr, ExprKind, Span};

use crate::function::FnCx;
use crate::types::Type;

impl FnCx<'_> {
    /// An expression's type, where a value of type `expected` goes.
    pub(crate) fn expr_as(&mut self, expr: &Expr, expected: &Type) -> Type {
        self.expecting = Some(expected.clone());
        self.expr(expr)
    }

    /// An expression's type, given what's expected of it. Only leading-dot cases and
    /// `match`es use what's expected.
    pub(crate) fn expected_kind(&mut self, expr: &Expr, expected: &Type) -> Type {
        match &expr.kind {
            ExprKind::Variant(case) => {
                let owner = self.case_owner(case, expected, expr.span);
                self.construct_variant(owner.as_deref(), case, &[], expr.span)
            }
            ExprKind::Call { callee, args } if let ExprKind::Variant(case) = &callee.kind => {
                let owner = self.case_owner(case, expected, callee.span);
                self.construct_variant(owner.as_deref(), case, args, expr.span)
            }
            ExprKind::Match { subject, arms } => {
                self.match_expr(subject, arms, expr.span, Some(expected.clone()))
            }
            _ => self.expr_kind(expr),
        }
    }

    /// The two sides of `==` or `!=`. A leading-dot case on one side takes its enum from
    /// the other side.
    pub(crate) fn compared(&mut self, lhs: &Expr, rhs: &Expr) -> (Type, Type) {
        if leading_dot(lhs) && !leading_dot(rhs) {
            let right = self.expr(rhs);
            (self.expr_as(lhs, &right), right)
        } else {
            let left = self.expr(lhs);
            let right = self.expr_as(rhs, &left);
            (left, right)
        }
    }

    /// The enum a leading-dot case belongs to, when more than one enum has a case by that
    /// name and the expected type is one of them. Without one, the case is looked up by its
    /// name alone.
    fn case_owner(&mut self, case: &str, expected: &Type, span: Span) -> Option<String> {
        let owners = self.checker.program.enums_with_case(case);
        if owners.len() < 2 {
            return None;
        }
        let owner = match self.checker.settled(expected) {
            Type::Named(name) => name,
            Type::Optional(inner) => match *inner {
                Type::Named(name) => name,
                _ => return None,
            },
            _ => return None,
        };
        owners.contains(&owner).then(|| {
            self.checker.cases.insert((span.start, span.end), owner.clone());
            owner
        })
    }
}

/// Whether an expression is a leading-dot case: `.Circle` or `.Circle(radius=1.0)`.
fn leading_dot(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Variant(_) => true,
        ExprKind::Call { callee, .. } => matches!(callee.kind, ExprKind::Variant(_)),
        _ => false,
    }
}
