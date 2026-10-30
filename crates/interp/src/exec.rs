//! Running statements: assignment, `if`, loops, `with`, `return`, `fail` and `assert`.

use std::rc::Rc;

use fennec_syntax::{BinOp, Diagnostic, Expr, ExprKind, Span, Stmt, StmtKind};

use crate::format;
use crate::interp::{Interp, PASSING_UP, callable};
use crate::place::Step;
use crate::value::Value;

/// How a statement finished.
pub(crate) enum Flow {
    Normal(Value),
    Return(Value),
    /// Leave the function with exactly this value: an error or `None` passed up by `?`.
    PassUp(Value),
    Break,
    Continue,
}

pub(crate) fn not_mut(name: &str, span: Span) -> Diagnostic {
    Diagnostic::new(format!("`{name}` isn't mut"), span)
        .with_help(format!("declare it with `mut {name} = ...` if it needs to change"))
}

fn unhandled(span: Span) -> Diagnostic {
    Diagnostic::new("this can fail, but nothing handles it", span)
        .with_help("pass the error up with `?`, give a default with `else`, or handle it with `match`")
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
        for (at, stmt) in stmts.iter().enumerate() {
            let flow = match self.exec(stmt) {
                Ok(flow) => flow,
                Err(diagnostic) if diagnostic.message == PASSING_UP => {
                    self.passing_up.take().unwrap_or(Flow::Normal(Value::Unit))
                }
                Err(diagnostic) => return Err(diagnostic),
            };
            match flow {
                Flow::Normal(value) => {
                    let is_last = at + 1 == stmts.len();
                    if !is_last
                        && matches!(stmt.kind, StmtKind::Expr(_))
                        && matches!(value, Value::Ok(_) | Value::Err(_))
                    {
                        return Err(unhandled(stmt.span));
                    }
                    last = value;
                }
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal(last))
    }

    /// Runs one REPL statement in the session's scope, returning how to echo it.
    pub(crate) fn exec_top(&mut self, stmt: &Stmt) -> Result<Option<String>, Diagnostic> {
        let flow = self.exec_stmts(std::slice::from_ref(stmt))?;
        match flow {
            Flow::Normal(Value::Unit) => Ok(None),
            Flow::Normal(value) if matches!(stmt.kind, StmtKind::Expr(_)) => Ok(Some(format::repr(&value))),
            Flow::Normal(_) => Ok(None),
            _ => Err(Diagnostic::new("there's no function or loop here to leave", stmt.span)),
        }
    }

    fn exec(&mut self, stmt: &Stmt) -> Result<Flow, Diagnostic> {
        match &stmt.kind {
            StmtKind::Expr(expr) => Ok(Flow::Normal(self.eval(expr)?)),
            StmtKind::Assign { target, op, value, declare_mut } => {
                self.assign(target, *op, value, *declare_mut)?;
                Ok(Flow::Normal(Value::Unit))
            }
            StmtKind::If { branches, otherwise } => {
                for (cond, body) in branches {
                    if self.truth(cond, "if")? {
                        return self.exec_block(body);
                    }
                }
                otherwise.as_ref().map_or(Ok(Flow::Normal(Value::Unit)), |body| self.exec_block(body))
            }
            StmtKind::While { cond, body } => {
                while self.truth(cond, "while")? {
                    match self.exec_block(body)? {
                        Flow::Break => break,
                        Flow::Normal(_) | Flow::Continue => {}
                        other => return Ok(other),
                    }
                }
                Ok(Flow::Normal(Value::Unit))
            }
            StmtKind::For { names, is_mut, iter, body } => self.for_loop(names, *is_mut, iter, body),
            StmtKind::Return(value) => {
                Ok(Flow::Return(value.as_ref().map_or(Ok(Value::Unit), |value| self.eval(value))?))
            }
            StmtKind::Fail(expr) => {
                let error = self.eval(expr)?;
                check_error(&error, expr.span)?;
                Ok(Flow::PassUp(Value::Err(Rc::new(error))))
            }
            StmtKind::Yield(expr) => {
                let value = self.eval(expr)?;
                let collected = self
                    .yields
                    .last_mut()
                    .ok_or_else(|| Diagnostic::new("`yield` outside a function", stmt.span))?;
                collected.push(value);
                Ok(Flow::Normal(Value::Unit))
            }
            StmtKind::Assert(expr) => {
                self.assert(expr)?;
                Ok(Flow::Normal(Value::Unit))
            }
            StmtKind::With { value, name, is_mut, body } => {
                self.with_block(value, name.as_deref(), *is_mut, body)
            }
            StmtKind::Def(def) => {
                let function = Value::Function(Rc::new(callable(def, self.frame.visible())));
                self.frame.declare(&def.name, function, false);
                Ok(Flow::Normal(Value::Unit))
            }
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
                    Value::List(_) | Value::Str(_) | Value::Dict(_) | Value::Set(_) => {
                        "Fennec has no truthiness: write `len(x) > 0`"
                    }
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

    fn assert(&mut self, expr: &Expr) -> Result<(), Diagnostic> {
        if let ExprKind::Binary { op, lhs, rhs } = &expr.kind
            && matches!(
                op,
                BinOp::Eq
                    | BinOp::NotEq
                    | BinOp::Lt
                    | BinOp::LtEq
                    | BinOp::Gt
                    | BinOp::GtEq
                    | BinOp::In
                    | BinOp::NotIn
                    | BinOp::Is
                    | BinOp::IsNot
            )
        {
            let left = self.eval(lhs)?;
            let right = self.eval(rhs)?;
            if self.binary(*op, left.clone(), right.clone(), expr.span)? == Value::Bool(true) {
                return Ok(());
            }
            return Err(Diagnostic::new("this assertion failed", expr.span).with_help(format!(
                "the left side is {} and the right side is {}",
                format::repr(&left),
                format::repr(&right)
            )));
        }
        if self.truth(expr, "assert")? {
            Ok(())
        } else {
            Err(Diagnostic::new("this assertion failed", expr.span))
        }
    }

    fn with_block(
        &mut self,
        value: &Expr,
        name: Option<&str>,
        is_mut: bool,
        body: &[Stmt],
    ) -> Result<Flow, Diagnostic> {
        let resource = self.eval(value)?;
        self.frame.push();
        self.frame.declare(name.unwrap_or("_with"), resource, is_mut);
        let flow = self.exec_stmts(body);
        let resource = self.frame.get(name.unwrap_or("_with")).map(|binding| binding.value.clone());
        self.frame.pop();
        if let Some(resource) = resource {
            self.clean_up(resource, value.span)?;
        }
        flow
    }

    fn assign(
        &mut self,
        target: &Expr,
        op: Option<BinOp>,
        value: &Expr,
        declare_mut: bool,
    ) -> Result<(), Diagnostic> {
        match &target.kind {
            ExprKind::Name(name) => {
                let new_value = self.eval(value)?;
                self.assign_name(name, target.span, op, new_value, declare_mut)
            }
            ExprKind::Tuple(names) => {
                let new_value = self.eval(value)?;
                let Value::Tuple(items) = new_value else {
                    return Err(Diagnostic::new(
                        format!("can't unpack a {}", new_value.type_name()),
                        value.span,
                    ));
                };
                if items.len() != names.len() {
                    return Err(Diagnostic::new(
                        format!("can't unpack {} values into {} names", items.len(), names.len()),
                        value.span,
                    ));
                }
                for (name, item) in names.iter().zip(items.iter()) {
                    if let ExprKind::Name(name) = &name.kind {
                        self.assign_name(name, target.span, None, item.clone(), false)?;
                    }
                }
                Ok(())
            }
            ExprKind::Index { object, index } if self.has_method(object, "[]=")? => {
                let args = vec![index.as_ref().clone(), value.clone()];
                self.call_mut_method(object, "[]=", &args, target.span).map(|_| ())
            }
            _ => {
                let place = self.mut_place(target, "this can't be assigned to")?;
                let new_value = self.eval(value)?;
                let new_value = match op {
                    Some(op) => {
                        let old = self.take(&place)?;
                        self.binary(op, old, new_value, target.span)?
                    }
                    None => new_value,
                };
                self.put(&place, new_value)
            }
        }
    }

    fn assign_name(
        &mut self,
        name: &str,
        span: Span,
        op: Option<BinOp>,
        new_value: Value,
        declare_mut: bool,
    ) -> Result<(), Diagnostic> {
        let exists = self.frame.get(name).is_some();
        if declare_mut && exists {
            return Err(Diagnostic::new(format!("`{name}` already exists"), span)
                .with_help(format!("to change it, write `{name} = ...` without `mut`")));
        }
        if !exists {
            if op.is_some() {
                return Err(Diagnostic::new(format!("`{name}` doesn't exist yet"), span));
            }
            self.frame.declare(name, new_value, declare_mut);
            return Ok(());
        }
        if self.frame.get(name).is_some_and(|binding| !binding.is_mut) {
            return Err(not_mut(name, span));
        }
        let place = crate::place::Place { root: name.to_owned(), steps: Vec::new(), span };
        let new_value = match op {
            Some(op) => {
                let old = self.take(&place)?;
                self.binary(op, old, new_value, span)?
            }
            None => new_value,
        };
        self.put(&place, new_value)
    }

    fn for_loop(
        &mut self,
        names: &[String],
        is_mut: bool,
        iter: &Expr,
        body: &[Stmt],
    ) -> Result<Flow, Diagnostic> {
        let place =
            if is_mut {
                Some(self.mut_place(
                    iter,
                    "`for mut` changes each item, so loop over something held in a mut name",
                )?)
            } else {
                None
            };
        let source = self.eval(iter)?;
        for (key, item) in self.entries(source, iter.span)? {
            self.frame.push();
            let bound = self.bind_names(names, item, is_mut, iter.span);
            let flow = bound.and_then(|()| self.exec_stmts(body));
            let changed = match (is_mut, names) {
                (true, [name]) => self.frame.get(name).map(|binding| binding.value.clone()),
                _ => None,
            };
            self.frame.pop();
            if let (Some(value), Some(place), Some(key)) = (changed, &place, key) {
                self.put(&place.then(Step::Index(key)), value)?;
            }
            match flow? {
                Flow::Break => break,
                Flow::Normal(_) | Flow::Continue => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal(Value::Unit))
    }

    /// Binds a loop's names to an item: one name, or several unpacking a tuple.
    pub(crate) fn bind_names(
        &mut self,
        names: &[String],
        item: Value,
        is_mut: bool,
        span: Span,
    ) -> Result<(), Diagnostic> {
        if let [name] = names {
            self.frame.declare(name, item, is_mut);
            return Ok(());
        }
        let Value::Tuple(items) = item else {
            return Err(Diagnostic::new(
                format!("can't unpack a {} into {} names", item.type_name(), names.len()),
                span,
            ));
        };
        if items.len() != names.len() {
            return Err(Diagnostic::new(
                format!("can't unpack {} values into {} names", items.len(), names.len()),
                span,
            ));
        }
        for (name, item) in names.iter().zip(items.iter()) {
            self.frame.declare(name, item.clone(), is_mut);
        }
        Ok(())
    }
}

/// Checks a value can be failed with: it must be declared with `error`.
fn check_error(value: &Value, span: Span) -> Result<(), Diagnostic> {
    match value {
        Value::Instance(instance) if instance.is_error => Ok(()),
        other => {
            Err(Diagnostic::new(format!("`fail` needs an error, but this is a {}", other.type_name()), span)
                .with_help("declare error types with `error`, like `error NotFound:`"))
        }
    }
}
