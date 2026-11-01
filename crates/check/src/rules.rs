//! What a `where` rule may say, and what's known about it before the program runs. A rule looks
//! only at its own field, so checking one on a write costs what it looks like. A default is a
//! value known now, so a default that breaks its rule is reported now.

use fennec_syntax::{BinOp, Diagnostic, Expr, ExprKind, Field, UnaryOp, visit};

use crate::function::FnCx;

/// Checks a field's rule looks only at that field, and that its default keeps to it.
pub(crate) fn check_field(cx: &mut FnCx<'_>, type_name: &str, field: &Field, fields: &[Field]) {
    let Some(rule) = &field.rule else { return };
    let mut written = rule.clone();
    let mut names = Vec::new();
    visit::in_expr(&mut written, &mut |seen| {
        if let ExprKind::Name(name) = &seen.kind {
            names.push((name.clone(), seen.span));
        }
    });
    for (name, span) in names {
        if name != field.name && fields.iter().any(|other| other.name == name) {
            let message = format!("the `where` rule on `{}` can't look at `{name}`", field.name);
            cx.error(
                Diagnostic::new(message, span)
                    .with_help("a rule looks only at its own field: check several fields in a method or a function that builds the value"),
            );
        }
    }
    let Some(default) = &field.default else { return };
    if let Some(value) = constant(default, &field.name, None)
        && constant(rule, &field.name, Some(&value)) == Some(Constant::Bool(false))
    {
        let message = format!("this default breaks the `where` rule on `{type_name}.{}`", field.name);
        cx.error(Diagnostic::new(message, default.span));
    }
}

/// A value worked out before the program runs.
#[derive(Clone, Debug, PartialEq)]
enum Constant {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
}

/// What an expression comes to when it only uses literals and `field`, which holds `value`.
/// `None` when that takes running the program.
fn constant(expr: &Expr, field: &str, value: Option<&Constant>) -> Option<Constant> {
    Some(match &expr.kind {
        ExprKind::Int(number) => Constant::Int(*number),
        ExprKind::Float(number) => Constant::Float(*number),
        ExprKind::Bool(truth) => Constant::Bool(*truth),
        ExprKind::Str(text) => Constant::Str(text.clone()),
        ExprKind::Name(name) if name == field => value?.clone(),
        ExprKind::Unary { op, expr } => match (op, constant(expr, field, value)?) {
            (UnaryOp::Neg, Constant::Int(number)) => Constant::Int(number.checked_neg()?),
            (UnaryOp::Neg, Constant::Float(number)) => Constant::Float(-number),
            (UnaryOp::Not, Constant::Bool(truth)) => Constant::Bool(!truth),
            _ => return None,
        },
        ExprKind::Binary { op, lhs, rhs } => {
            binary(*op, constant(lhs, field, value)?, constant(rhs, field, value)?)?
        }
        _ => return None,
    })
}

fn binary(op: BinOp, lhs: Constant, rhs: Constant) -> Option<Constant> {
    let ordering = |order: Option<std::cmp::Ordering>| {
        let order = order?;
        Some(Constant::Bool(match op {
            BinOp::Eq => order.is_eq(),
            BinOp::NotEq => order.is_ne(),
            BinOp::Lt => order.is_lt(),
            BinOp::LtEq => order.is_le(),
            BinOp::Gt => order.is_gt(),
            BinOp::GtEq => order.is_ge(),
            _ => return None,
        }))
    };
    match (lhs, rhs) {
        (Constant::Bool(left), Constant::Bool(right)) => match op {
            BinOp::And => Some(Constant::Bool(left && right)),
            BinOp::Or => Some(Constant::Bool(left || right)),
            _ => ordering(Some(left.cmp(&right))),
        },
        (Constant::Int(left), Constant::Int(right)) => match op {
            BinOp::Add => left.checked_add(right).map(Constant::Int),
            BinOp::Sub => left.checked_sub(right).map(Constant::Int),
            BinOp::Mul => left.checked_mul(right).map(Constant::Int),
            _ => ordering(Some(left.cmp(&right))),
        },
        (Constant::Float(left), Constant::Float(right)) => ordering(left.partial_cmp(&right)),
        (Constant::Str(left), Constant::Str(right)) => ordering(Some(left.cmp(&right))),
        _ => None,
    }
}
