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
use crate::program::TypeKind;
use crate::types::Type;

impl FnCx<'_> {
    /// An expression's type, where a value of type `expected` goes.
    pub(crate) fn expr_as(&mut self, expr: &Expr, expected: &Type) -> Type {
        self.expecting = Some(expected.clone());
        self.expr(expr)
    }

    /// An expression's type, given what's expected of it. Only leading-dot cases, `match`es and
    /// lists use what's expected. A list written where a list of optionals goes is one of those,
    /// since it can come to hold `None`.
    pub(crate) fn expected_kind(&mut self, expr: &Expr, expected: &Type) -> Type {
        self.note_named_case(expr, expected);
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
            ExprKind::List(_) | ExprKind::Dict(_) => {
                let ty = self.expr_kind(expr);
                let wanted = self.checker.settled(expected);
                let of_optionals = match &wanted {
                    Type::List(item) | Type::Dict(_, item) => matches!(&**item, Type::Optional(held)
                        if matches!(**held, Type::Int | Type::Float | Type::Bool | Type::Str | Type::Named(_))),
                    _ => false,
                };
                if of_optionals && self.checker.fits(&wanted, &ty) { wanted } else { ty }
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

    /// Notes an enum's case written with the enum's name where `expected` says it already, which
    /// a leading dot would mean the same as: `expected_kind` gives it this same expected type.
    fn note_named_case(&mut self, expr: &Expr, expected: &Type) {
        let Some((object, case)) = named_case(expr) else { return };
        // An enum a kit has written in for a leading dot covers no source, so there it is one.
        if object.span.start == object.span.end {
            return;
        }
        let Some(written) = self.type_named_by(object) else { return };
        let wanted = match self.checker.settled(expected) {
            Type::Optional(inner) => *inner,
            wanted => wanted,
        };
        if matches!(&wanted, Type::Named(name) if name == written) && self.has_case(written, case) {
            self.checker.named_cases.insert((object.span.start, object.span.end));
        }
    }

    fn has_case(&self, owner: &str, case: &str) -> bool {
        let info = self.checker.program.type_info(owner);
        info.is_some_and(|info| info.def.cases.iter().any(|written| written.name == case))
    }

    /// The type a bare name means, not one reached through a module, which a dot leaves out too.
    fn type_named_by<'e>(&self, object: &'e Expr) -> Option<&'e str> {
        self.as_type_name(object).filter(|_| matches!(object.kind, ExprKind::Name(_)))
    }

    /// The enum or error a leading-dot case belongs to, when the expected type is the error, or
    /// is one of the enums when more than one has a case by that name. Without one, the case is
    /// looked up by its name alone, among the enums.
    fn case_owner(&mut self, case: &str, expected: &Type, span: Span) -> Option<String> {
        let owner = match self.checker.settled(expected) {
            Type::Named(name) => name,
            Type::Optional(inner) => match *inner {
                Type::Named(name) => name,
                _ => return None,
            },
            _ => return None,
        };
        let is_error =
            self.checker.program.type_info(&owner).is_some_and(|info| info.kind == TypeKind::Error);
        let owners = self.checker.program.enums_with_case(case);
        let chosen =
            if is_error { self.has_case(&owner, case) } else { owners.len() > 1 && owners.contains(&owner) };
        chosen.then(|| {
            self.checker.cases.insert((span.start, span.end), owner.clone());
            owner
        })
    }
}

/// What's before the dot and after it, in `Shape.Circle` or `Shape.Circle(radius=1.0)`.
fn named_case(expr: &Expr) -> Option<(&Expr, &str)> {
    let at = match &expr.kind {
        ExprKind::Call { callee, .. } => callee,
        _ => expr,
    };
    match &at.kind {
        ExprKind::Attr { object, name, optional: false } => Some((object, name)),
        _ => None,
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
