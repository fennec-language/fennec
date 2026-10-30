//! Running statements: assignment, `if`, loops and `return`.

use fennec_syntax::{BinOp, Diagnostic, Expr, ExprKind, Span, Stmt, StmtKind};

use crate::interp::{Flow, Interp};
use crate::ops;
use crate::value::Value;

pub(crate) fn not_mut(name: &str, span: Span) -> Diagnostic {
    Diagnostic::new(format!("`{name}` isn't mut"), span).with_help(format!(
        "declare it with `mut {name} = ...` if it needs to change"
    ))
}

impl Interp<'_> {
    /// Runs a block in its own scope, so names created inside it don't outlive it.
    pub(crate) fn exec_block(&mut self, stmts: &[Stmt]) -> Result<Flow, Diagnostic> {
        self.frame.push();
        let flow = self.exec_stmts(stmts);
        self.frame.pop();
        flow
    }

    /// Runs statements in the current scope. A block's value is its last expression's.
    pub(crate) fn exec_stmts(&mut self, stmts: &[Stmt]) -> Result<Flow, Diagnostic> {
        let mut last = Value::Unit;
        for stmt in stmts {
            match self.exec(stmt)? {
                Flow::Normal(value) => last = value,
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal(last))
    }

    fn exec(&mut self, stmt: &Stmt) -> Result<Flow, Diagnostic> {
        match &stmt.kind {
            StmtKind::Expr(expr) => Ok(Flow::Normal(self.eval(expr)?)),
            StmtKind::Assign {
                target,
                op,
                value,
                declare_mut,
            } => {
                self.assign(target, *op, value, *declare_mut)?;
                Ok(Flow::Normal(Value::Unit))
            }
            StmtKind::If { branches, otherwise } => {
                for (cond, body) in branches {
                    if self.truth(cond, "if")? {
                        return self.exec_block(body);
                    }
                }
                otherwise
                    .as_ref()
                    .map_or(Ok(Flow::Normal(Value::Unit)), |body| self.exec_block(body))
            }
            StmtKind::While { cond, body } => {
                while self.truth(cond, "while")? {
                    match self.exec_block(body)? {
                        Flow::Break => break,
                        Flow::Return(value) => return Ok(Flow::Return(value)),
                        Flow::Normal(_) | Flow::Continue => {}
                    }
                }
                Ok(Flow::Normal(Value::Unit))
            }
            StmtKind::For {
                name,
                is_mut,
                iter,
                body,
            } => self.for_loop(name, *is_mut, iter, body),
            StmtKind::Return(value) => Ok(Flow::Return(
                value.as_ref().map_or(Ok(Value::Unit), |value| self.eval(value))?,
            )),
            StmtKind::Break => Ok(Flow::Break),
            StmtKind::Continue => Ok(Flow::Continue),
            StmtKind::Pass => Ok(Flow::Normal(Value::Unit)),
        }
    }

    /// Evaluates a condition, which must be a bool: Fennec has no truthiness.
    pub(crate) fn truth(&mut self, cond: &Expr, keyword: &str) -> Result<bool, Diagnostic> {
        match self.eval(cond)? {
            Value::Bool(b) => Ok(b),
            other => {
                let help = match other {
                    Value::List(_) | Value::Str(_) => "Fennec has no truthiness: write `len(x) > 0`",
                    Value::None => "Fennec has no truthiness: write `x is None` or `x is not None`",
                    _ => "Fennec has no truthiness: compare explicitly, like `count > 0`",
                };
                Err(Diagnostic::new(
                    format!("`{keyword}` needs a bool, but this is a {}", other.type_name()),
                    cond.span,
                )
                .with_help(help))
            }
        }
    }

    fn assign(
        &mut self,
        target: &Expr,
        op: Option<BinOp>,
        value: &Expr,
        declare_mut: bool,
    ) -> Result<(), Diagnostic> {
        match &target.kind {
            ExprKind::Name(name) => self.assign_name(name, target.span, op, value, declare_mut),
            ExprKind::Index { object, index } => self.assign_index(object, index, op, value, target.span),
            _ => Err(Diagnostic::new(
                "only names and list items can be assigned to so far",
                target.span,
            )),
        }
    }

    fn assign_name(
        &mut self,
        name: &str,
        span: Span,
        op: Option<BinOp>,
        value: &Expr,
        declare_mut: bool,
    ) -> Result<(), Diagnostic> {
        let new_value = self.eval(value)?;
        if declare_mut || self.frame.get(name).is_none() {
            if self.frame.get(name).is_some() {
                return Err(Diagnostic::new(format!("`{name}` already exists"), span)
                    .with_help(format!("to change it, write `{name} = ...` without `mut`")));
            }
            if op.is_some() {
                return Err(Diagnostic::new(format!("`{name}` doesn't exist yet"), span));
            }
            self.frame.declare(name, new_value, declare_mut);
            return Ok(());
        }
        let Some(binding) = self.frame.get_mut(name) else {
            return Ok(());
        };
        if !binding.is_mut {
            return Err(not_mut(name, span));
        }
        binding.value = match op {
            Some(op) => ops::binary(
                op,
                std::mem::replace(&mut binding.value, Value::Unit),
                new_value,
                span,
            )?,
            None => new_value,
        };
        Ok(())
    }

    fn assign_index(
        &mut self,
        object: &Expr,
        index: &Expr,
        op: Option<BinOp>,
        value: &Expr,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let ExprKind::Name(name) = &object.kind else {
            return Err(Diagnostic::new(
                "only items of a list held in a name can be assigned so far",
                object.span,
            ));
        };
        let at = self.eval(index)?;
        let new_value = self.eval(value)?;
        let binding = self
            .frame
            .get_mut(name)
            .ok_or_else(|| Diagnostic::new(format!("`{name}` doesn't exist"), object.span))?;
        if !binding.is_mut {
            return Err(not_mut(name, object.span));
        }
        let Value::List(items) = &mut binding.value else {
            return Err(Diagnostic::new(
                format!("can't assign into a {}", binding.value.type_name()),
                object.span,
            ));
        };
        let Value::Int(at) = at else {
            return Err(Diagnostic::new("an index must be an int", index.span));
        };
        let items = std::rc::Rc::make_mut(items);
        let slot = ops::position(at, items.len(), index.span).and_then(|at| {
            items
                .get_mut(at)
                .ok_or_else(|| Diagnostic::new("index out of range", index.span))
        })?;
        *slot = match op {
            Some(op) => ops::binary(op, std::mem::replace(slot, Value::Unit), new_value, span)?,
            None => new_value,
        };
        Ok(())
    }

    fn for_loop(&mut self, name: &str, is_mut: bool, iter: &Expr, body: &[Stmt]) -> Result<Flow, Diagnostic> {
        if is_mut {
            self.check_mut_place(
                iter,
                "`for mut` changes each item, so it needs a mut name to loop over",
            )?;
        }
        let source = self.eval(iter)?;
        let items = items(source, iter.span)?;
        for (position, item) in items.into_iter().enumerate() {
            self.frame.push();
            self.frame.declare(name, item, is_mut);
            let flow = self.exec_stmts(body);
            let changed = if is_mut {
                self.frame.get(name).map(|binding| binding.value.clone())
            } else {
                None
            };
            self.frame.pop();
            if let (Some(value), ExprKind::Name(list)) = (changed, &iter.kind)
                && let Some(Value::List(items)) = self.frame.get_mut(list).map(|binding| &mut binding.value)
                && let Some(slot) = std::rc::Rc::make_mut(items).get_mut(position)
            {
                *slot = value;
            }
            match flow? {
                Flow::Break => break,
                Flow::Return(value) => return Ok(Flow::Return(value)),
                Flow::Normal(_) | Flow::Continue => {}
            }
        }
        Ok(Flow::Normal(Value::Unit))
    }

    /// Checks that `expr` is a name declared `mut`, for anything that changes it in place.
    pub(crate) fn check_mut_place(&self, expr: &Expr, what: &str) -> Result<(), Diagnostic> {
        let ExprKind::Name(name) = &expr.kind else {
            return Err(Diagnostic::new(what, expr.span));
        };
        match self.frame.get(name) {
            Some(binding) if binding.is_mut => Ok(()),
            Some(_) => Err(not_mut(name, expr.span)),
            None => Err(Diagnostic::new(format!("`{name}` doesn't exist"), expr.span)),
        }
    }
}

/// The items a `for` loop or comprehension walks through.
pub(crate) fn items(value: Value, span: Span) -> Result<Vec<Value>, Diagnostic> {
    match value {
        Value::List(items) => Ok(items.as_ref().clone()),
        Value::Str(text) => Ok(text
            .chars()
            .map(|c| Value::str(c.encode_utf8(&mut [0; 4])))
            .collect()),
        other => Err(Diagnostic::new(
            format!("can't loop over a {}", other.type_name()),
            span,
        )),
    }
}
