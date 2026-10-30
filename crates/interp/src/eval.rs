//! Evaluating expressions.

use std::rc::Rc;

use fennec_syntax::{BinOp, CompKind, Diagnostic, Expr, ExprKind, FStrPart, MatchArm, Span};

use crate::exec::Flow;
use crate::interp::{Interp, PASSING_UP};
use crate::value::{Lambda, Value};
use crate::{builtins, format, ops};

impl Interp<'_> {
    pub(crate) fn eval(&mut self, expr: &Expr) -> Result<Value, Diagnostic> {
        let span = expr.span;
        self.step(span)?;
        match &expr.kind {
            ExprKind::Int(n) => Ok(Value::Int(*n)),
            ExprKind::Float(x) => Ok(Value::Float(*x)),
            ExprKind::Str(text) => Ok(Value::str(text)),
            ExprKind::Bool(b) => Ok(Value::Bool(*b)),
            ExprKind::None => Ok(Value::None),
            ExprKind::FStr(parts) => self.fstring(parts),
            ExprKind::Name(name) => self.lookup(name, span),
            ExprKind::Variant(name) => self.construct_variant(None, name, Vec::new(), span),
            ExprKind::List(items) => Ok(Value::list(self.eval_all(items)?)),
            ExprKind::Tuple(items) => Ok(Value::tuple(self.eval_all(items)?)),
            ExprKind::Set(items) => Ok(Value::Set(Rc::new(dedupe(self.eval_all(items)?)))),
            ExprKind::Dict(pairs) => {
                let mut built = Vec::with_capacity(pairs.len());
                for (key, value) in pairs {
                    let key = self.eval(key)?;
                    let value = self.eval(value)?;
                    insert_pair(&mut built, key, value);
                }
                Ok(Value::Dict(Rc::new(built)))
            }
            ExprKind::Comprehension { kind, elem, value, names, iter, cond } => {
                let parts = Parts { elem, value: value.as_deref(), cond: cond.as_deref() };
                self.comprehension(*kind, &parts, names, iter)
            }
            ExprKind::Unary { op, expr: inner } => {
                let value = self.eval(inner)?;
                ops::unary(*op, value, span)
            }
            ExprKind::Binary { op: op @ (BinOp::And | BinOp::Or), lhs, rhs } => {
                let left = self.truth(lhs, op.symbol())?;
                if left == (*op == BinOp::Or) {
                    return Ok(Value::Bool(left));
                }
                self.truth(rhs, op.symbol()).map(Value::Bool)
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let left = self.eval(lhs)?;
                let right = self.eval(rhs)?;
                self.binary(*op, left, right, span)
            }
            ExprKind::Cond { then, cond, otherwise } => {
                if self.truth(cond, "if")? {
                    self.eval(then)
                } else {
                    self.eval(otherwise)
                }
            }
            ExprKind::Else { value, default } => match self.eval(value)? {
                Value::Err(_) | Value::None => self.eval(default),
                Value::Ok(inner) => Ok(inner.as_ref().clone()),
                other => Ok(other),
            },
            ExprKind::Try { value, context } => self.try_value(value, context.as_deref(), span),
            ExprKind::Call { callee, args } => self.call(callee, args, span),
            ExprKind::Attr { object, name, optional } => self.eval_attr(object, name, *optional, span),
            ExprKind::Index { object, index } => {
                let value = self.eval(object)?;
                let at = self.eval(index)?;
                self.index(value, at, span)
            }
            ExprKind::Slice { object, start, end } => {
                let value = self.eval(object)?;
                let start = start.as_deref().map(|start| self.eval(start)).transpose()?;
                let end = end.as_deref().map(|end| self.eval(end)).transpose()?;
                ops::slice(&value, start.as_ref(), end.as_ref(), span)
            }
            ExprKind::Lambda { params, body } => Ok(Value::Lambda(Rc::new(Lambda {
                params: params.clone(),
                body: body.as_ref().clone(),
                captured: self.frame.visible(),
            }))),
            ExprKind::Match { subject, arms } => self.match_value(subject, arms),
        }
    }

    fn eval_all(&mut self, items: &[Expr]) -> Result<Vec<Value>, Diagnostic> {
        items.iter().map(|item| self.eval(item)).collect()
    }

    /// `object.name`: a field, or a name from an imported module.
    fn eval_attr(
        &mut self,
        object: &Expr,
        name: &str,
        optional: bool,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        if let Some(member) = self.module_member(object, name, span) {
            return self.lookup(&member?, span);
        }
        let value = self.eval(object)?;
        self.attr(value, name, optional, span)
    }

    /// For `json.loads` where `json` is an imported module, the name it means: a module
    /// function like `json.loads`, or a type like `Path`.
    pub(crate) fn module_member(
        &self,
        object: &Expr,
        name: &str,
        span: Span,
    ) -> Option<Result<String, Diagnostic>> {
        let alias = fennec_syntax::dotted(object)?;
        let first = alias.split('.').next().unwrap_or_default();
        if self.frame.get(first).is_some() {
            return None;
        }
        let Some(module) = self.modules.get(&alias) else {
            // `std.toml` after `import std.json`, which binds `std` but not `std.toml`.
            let path = format!("{alias}.{name}");
            let is_package = self.modules.keys().any(|bound| package_of(bound) == Some(alias.as_str()));
            let module = self.modules.get(&path).copied();
            return is_package.then(|| Err(fennec_syntax::package_member(&path, module, span)));
        };
        Some(fennec_syntax::std_member(module, name, span).map(|()| {
            let qualified = format!("{module}.{name}");
            if builtins::named(&qualified).is_some() { qualified } else { name.to_owned() }
        }))
    }

    pub(crate) fn lookup(&self, name: &str, span: Span) -> Result<Value, Diagnostic> {
        if let Some(binding) = self.frame.get(name) {
            return Ok(binding.value.clone().unpacked());
        }
        if let Some(function) = self.functions.get(name) {
            return Ok(Value::Function(Rc::clone(function)));
        }
        if self.types.contains_key(name) || builtins::is_type(name) {
            return Ok(Value::Type(Rc::from(name)));
        }
        let prelude = span.in_prelude().then(|| self.prelude_imports.get(name)).flatten();
        if let Some(builtin) = self.imports.get(name).or(prelude) {
            return Ok(Value::Builtin(builtin));
        }
        if let Some(module) = self.modules.get(name) {
            return Err(fennec_syntax::module_as_value(name, module, span));
        }
        if let Some((path, module)) = self.modules.iter().find(|(path, _)| package_of(path) == Some(name)) {
            return Err(fennec_syntax::module_as_value(path, module, span));
        }
        if self.modules.contains_key(&format!("std.{name}")) {
            return Err(fennec_syntax::short_module(name, span));
        }
        if let Some(constant) = builtins::constant(name) {
            return Ok(constant);
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
        kind: CompKind,
        parts: &Parts<'_>,
        names: &[String],
        iter: &Expr,
    ) -> Result<Value, Diagnostic> {
        let source = self.eval(iter)?;
        let mut items = Vec::new();
        let mut pairs = Vec::new();
        for item in self.iterate(source, iter.span)? {
            self.frame.push();
            let result = self.comprehension_step(names, item, iter.span, parts);
            self.frame.pop();
            match result? {
                Some((key, Some(value))) => insert_pair(&mut pairs, key, value),
                Some((item, None)) => items.push(item),
                None => {}
            }
        }
        Ok(match kind {
            CompKind::List | CompKind::Generator => Value::list(items),
            CompKind::Set => Value::Set(Rc::new(dedupe(items))),
            CompKind::Dict => Value::Dict(Rc::new(pairs)),
        })
    }

    /// One item of a comprehension: the element (and value, for a dict), or nothing if filtered out.
    fn comprehension_step(
        &mut self,
        names: &[String],
        item: Value,
        span: Span,
        parts: &Parts<'_>,
    ) -> Result<Option<(Value, Option<Value>)>, Diagnostic> {
        self.bind_names(names, item, false, span)?;
        if let Some(cond) = parts.cond
            && !self.truth(cond, "if")?
        {
            return Ok(None);
        }
        let elem = self.eval(parts.elem)?;
        let value = parts.value.map(|value| self.eval(value)).transpose()?;
        Ok(Some((elem, value)))
    }

    /// `value?`: the value inside an `Ok`, or else leave the function with the error or `None`.
    fn try_value(&mut self, value: &Expr, context: Option<&Expr>, span: Span) -> Result<Value, Diagnostic> {
        let passed = match self.eval(value)? {
            Value::Ok(inner) => return Ok(inner.as_ref().clone()),
            Value::Err(error) => match context {
                Some(context) => {
                    let mut wrapper = self.eval(context)?;
                    if let Value::Instance(instance) = &mut wrapper {
                        Rc::make_mut(instance).cause = Some(error.as_ref().clone());
                    }
                    Value::Err(Rc::new(wrapper))
                }
                None => Value::Err(error),
            },
            Value::None => Value::None,
            other => return Ok(other),
        };
        self.passing_up = Some(Flow::PassUp(passed));
        Err(Diagnostic::new(PASSING_UP, span))
    }

    fn match_value(&mut self, subject: &Expr, arms: &[MatchArm]) -> Result<Value, Diagnostic> {
        let value = self.eval(subject)?;
        for arm in arms {
            let Some(bindings) = self.matches(&arm.pattern, &value, arm.span)? else { continue };
            self.frame.push();
            for (name, bound) in bindings {
                self.frame.declare(&name, bound, false);
            }
            let flow = self.exec_stmts(&arm.body);
            self.frame.pop();
            return match flow? {
                Flow::Normal(value) => Ok(value),
                other => {
                    self.passing_up = Some(other);
                    Err(Diagnostic::new(PASSING_UP, arm.span))
                }
            };
        }
        Err(Diagnostic::new(format!("no case matches {}", format::repr(&value)), subject.span)
            .with_help("add a case for it, or `case _:` for everything else"))
    }

    /// An operator, using the type's own definition (like `def +`) when it has one.
    pub(crate) fn binary(
        &mut self,
        op: BinOp,
        left: Value,
        right: Value,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        if let (Value::Instance(instance), Some(method)) = (&left, op.method_name())
            && let Some(function) =
                self.type_info(&instance.type_name).and_then(|info| info.methods.get(method).cloned())
        {
            return self.call_with_values(&function, vec![left, right], span);
        }
        if matches!(op, BinOp::Gt | BinOp::LtEq | BinOp::GtEq)
            && let Value::Instance(instance) = &right
            && let Some(less) =
                self.type_info(&instance.type_name).and_then(|info| info.methods.get("<").cloned())
        {
            let (a, b) = if matches!(op, BinOp::Gt | BinOp::LtEq) { (right, left) } else { (left, right) };
            let less_than = self.call_with_values(&less, vec![a, b], span)? == Value::Bool(true);
            return Ok(Value::Bool(if op == BinOp::Gt { less_than } else { !less_than }));
        }
        if matches!(left, Value::Ok(_) | Value::Err(_)) && !matches!(op, BinOp::Eq | BinOp::NotEq) {
            return Err(unhandled_result(span));
        }
        ops::binary(op, left, right, span)
    }
}

/// The parts of a comprehension evaluated for each item.
struct Parts<'a> {
    elem: &'a Expr,
    value: Option<&'a Expr>,
    cond: Option<&'a Expr>,
}

pub(crate) fn unhandled_result(span: Span) -> Diagnostic {
    Diagnostic::new("this is the result of something that can fail", span)
        .with_help("get the value out with `?`, `else`, or `match` first")
}

fn dedupe(items: Vec<Value>) -> Vec<Value> {
    let mut unique: Vec<Value> = Vec::with_capacity(items.len());
    for item in items {
        if !unique.contains(&item) {
            unique.push(item);
        }
    }
    unique
}

fn insert_pair(pairs: &mut Vec<(Value, Value)>, key: Value, value: Value) {
    match pairs.iter_mut().find(|(existing, _)| *existing == key) {
        Some((_, slot)) => *slot = value,
        None => pairs.push((key, value)),
    }
}

/// An unknown name, with a pointer to the Fennec spelling when it's a habit from elsewhere.
/// The package a module path is in: `std` for `std.json`.
pub(crate) fn package_of(path: &str) -> Option<&str> {
    path.split_once('.').map(|(package, _)| package)
}

pub(crate) fn unknown_name(name: &str, span: Span) -> Diagnostic {
    let diagnostic = Diagnostic::new(format!("`{name}` doesn't exist"), span);
    match name {
        "print" | "console" => diagnostic.with_help(
            "there's no global output: write with the `stdout` that `main` receives, like `stdout.print(x)`",
        ),
        "true" | "false" => diagnostic.with_help("write `True` or `False`"),
        "null" | "nil" | "none" | "undefined" => diagnostic.with_help("write `None`"),
        "lambda" => diagnostic.with_help("write an arrow function: `x => x * 2`"),
        "raise" => diagnostic.with_help("Fennec has no exceptions: write `fail SomeError(...)`"),
        "ord" => diagnostic.with_help("a character's code is a method: write `c.code()`"),
        "chr" => diagnostic.with_help("make a character from its code with `str.from_code(n)`"),
        _ => match builtins::modules_with(name).as_slice() {
            [] => diagnostic,
            modules @ [module, ..] => diagnostic.with_help(format!(
                "import it from std.{}, like `from std.{module} import {name}`",
                modules.join(" or std.")
            )),
        },
    }
}
