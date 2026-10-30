//! Evaluating expressions.

use std::rc::Rc;

use fennec_syntax::{BinOp, Diagnostic, Expr, ExprKind, FStrPart, Span};

use crate::interp::Interp;
use crate::value::{Lambda, Value};
use crate::{builtins, format, ops};

impl Interp<'_> {
    pub(crate) fn eval(&mut self, expr: &Expr) -> Result<Value, Diagnostic> {
        match &expr.kind {
            ExprKind::Int(n) => Ok(Value::Int(*n)),
            ExprKind::Float(x) => Ok(Value::Float(*x)),
            ExprKind::Str(text) => Ok(Value::str(text)),
            ExprKind::Bool(b) => Ok(Value::Bool(*b)),
            ExprKind::None => Ok(Value::None),
            ExprKind::FStr(parts) => self.fstring(parts),
            ExprKind::Name(name) => self.lookup(name, expr.span),
            ExprKind::List(items) => Ok(Value::list(
                items
                    .iter()
                    .map(|item| self.eval(item))
                    .collect::<Result<_, _>>()?,
            )),
            ExprKind::ListComp {
                elem,
                var,
                iter,
                cond,
            } => self.comprehension(elem, var, iter, cond.as_deref()),
            ExprKind::Unary { op, expr: inner } => {
                let value = self.eval(inner)?;
                ops::unary(*op, value, expr.span)
            }
            ExprKind::Binary {
                op: op @ (BinOp::And | BinOp::Or),
                lhs,
                rhs,
            } => {
                let left = self.truth(lhs, op.symbol())?;
                if left == (*op == BinOp::Or) {
                    return Ok(Value::Bool(left));
                }
                self.truth(rhs, op.symbol()).map(Value::Bool)
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let left = self.eval(lhs)?;
                let right = self.eval(rhs)?;
                ops::binary(*op, left, right, expr.span)
            }
            ExprKind::Cond {
                then,
                cond,
                otherwise,
            } => {
                if self.truth(cond, "if")? {
                    self.eval(then)
                } else {
                    self.eval(otherwise)
                }
            }
            ExprKind::Call { callee, args } => self.call(callee, args, expr.span),
            ExprKind::Attr { object, name } => {
                let value = self.eval(object)?;
                Err(Diagnostic::new(
                    format!("a {} has no field `{name}`", value.type_name()),
                    expr.span,
                ))
            }
            ExprKind::Index { object, index } => {
                let value = self.eval(object)?;
                let at = self.eval(index)?;
                ops::index(&value, &at, expr.span)
            }
            ExprKind::Lambda { params, body } => Ok(Value::Lambda(Rc::new(Lambda {
                params: params.clone(),
                body: body.as_ref().clone(),
                captured: self.frame.visible(),
            }))),
        }
    }

    fn lookup(&self, name: &str, span: Span) -> Result<Value, Diagnostic> {
        if let Some(binding) = self.frame.get(name) {
            return Ok(binding.value.clone());
        }
        if self.functions.contains_key(name) {
            return Ok(Value::Function(Rc::from(name)));
        }
        if let Some(builtin) = builtins::named(name) {
            return Ok(Value::Builtin(builtin));
        }
        Err(unknown_name(name, span))
    }

    fn fstring(&mut self, parts: &[FStrPart]) -> Result<Value, Diagnostic> {
        let mut text = String::new();
        for part in parts {
            match part {
                FStrPart::Text(literal) => text.push_str(literal),
                FStrPart::Expr { expr, spec } => {
                    let value = self.eval(expr)?;
                    let piece = format::with_spec(&value, spec.as_deref().unwrap_or(""))
                        .map_err(|message| Diagnostic::new(message, expr.span))?;
                    text.push_str(&piece);
                }
            }
        }
        Ok(Value::str(&text))
    }

    fn comprehension(
        &mut self,
        elem: &Expr,
        var: &str,
        iter: &Expr,
        cond: Option<&Expr>,
    ) -> Result<Value, Diagnostic> {
        let source = self.eval(iter)?;
        let mut results = Vec::new();
        for item in crate::exec::items(source, iter.span)? {
            self.frame.push();
            self.frame.declare(var, item, false);
            let keep = cond.map_or(Ok(true), |cond| self.truth(cond, "if"));
            let value = match keep {
                Ok(true) => self.eval(elem).map(Some),
                Ok(false) => Ok(None),
                Err(err) => Err(err),
            };
            self.frame.pop();
            results.extend(value?);
        }
        Ok(Value::list(results))
    }
}

/// An unknown name, with a pointer to the Fennec spelling when it's a habit from elsewhere.
pub(crate) fn unknown_name(name: &str, span: Span) -> Diagnostic {
    let diagnostic = Diagnostic::new(format!("`{name}` doesn't exist"), span);
    match name {
        "print" | "console" => diagnostic.with_help(
            "there's no global output: write with the `stdout` that `main` receives, like `stdout.print(x)`",
        ),
        "true" | "false" => diagnostic.with_help("write `True` or `False`"),
        "null" | "nil" | "none" | "undefined" => diagnostic.with_help("write `None`"),
        "lambda" => diagnostic.with_help("write an arrow function: `x => x * 2`"),
        _ => diagnostic,
    }
}
