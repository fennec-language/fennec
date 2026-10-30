//! Calling functions: user functions, arrow functions and builtins. A `mut` argument is
//! handed to the callee and its final value copied back when the call returns.

use fennec_syntax::{Arg, Diagnostic, Expr, ExprKind, FnDef, Param, Span};

use crate::eval::unknown_name;
use crate::interp::{Flow, Interp, MAX_DEPTH};
use crate::scope::Frame;
use crate::value::{Lambda, Value};

/// An evaluated argument. `mut_place` is the caller's name a `mut` argument came from.
pub(crate) struct CallArg {
    pub(crate) name: Option<String>,
    pub(crate) value: Value,
    pub(crate) mut_place: Option<String>,
    pub(crate) span: Span,
}

impl CallArg {
    pub(crate) fn plain(value: Value, span: Span) -> Self {
        CallArg {
            name: None,
            value,
            mut_place: None,
            span,
        }
    }
}

impl Interp<'_> {
    pub(crate) fn call(&mut self, callee: &Expr, args: &[Arg], span: Span) -> Result<Value, Diagnostic> {
        if let ExprKind::Attr { object, name } = &callee.kind {
            return self.method_call(object, name, args, span);
        }
        let function = self.eval(callee)?;
        let args = self.eval_args(args)?;
        self.call_value(&function, args, span)
    }

    pub(crate) fn eval_args(&mut self, args: &[Arg]) -> Result<Vec<CallArg>, Diagnostic> {
        let mut evaluated = Vec::with_capacity(args.len());
        for arg in args {
            let mut_place = match (&arg.value.kind, arg.is_mut) {
                (ExprKind::Name(name), true) => {
                    self.check_mut_place(&arg.value, "")?;
                    Some(name.clone())
                }
                (_, true) => {
                    return Err(Diagnostic::new(
                        "only a name can be passed with `mut`",
                        arg.value.span,
                    ));
                }
                (_, false) => None,
            };
            let value = self.eval(&arg.value)?;
            evaluated.push(CallArg {
                name: arg.name.clone(),
                value,
                mut_place,
                span: arg.value.span,
            });
        }
        Ok(evaluated)
    }

    pub(crate) fn call_value(
        &mut self,
        function: &Value,
        args: Vec<CallArg>,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        match function {
            Value::Function(name) => {
                let def = self
                    .functions
                    .get(name.as_ref())
                    .cloned()
                    .ok_or_else(|| unknown_name(name, span))?;
                self.call_user(&def, args, span)
            }
            Value::Builtin(name) => self.builtin(name, args, span),
            Value::Lambda(lambda) => self.call_lambda(lambda, args, span),
            other => Err(Diagnostic::new(
                format!("a {} can't be called", other.type_name()),
                span,
            )),
        }
    }

    /// Calls a function with plain values, as `run` does for `main`.
    pub(crate) fn call_with_values(
        &mut self,
        def: &FnDef,
        values: Vec<Value>,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let args = values
            .into_iter()
            .map(|value| CallArg::plain(value, span))
            .collect();
        self.call_user(def, args, span)
    }

    fn enter(&mut self, span: Span) -> Result<Frame, Diagnostic> {
        if self.depth >= MAX_DEPTH {
            return Err(
                Diagnostic::new(format!("calls nested more than {MAX_DEPTH} deep"), span)
                    .with_help("check for recursion that never stops"),
            );
        }
        self.depth += 1;
        Ok(std::mem::replace(&mut self.frame, Frame::new()))
    }

    fn leave(&mut self, caller: Frame) {
        self.depth -= 1;
        self.frame = caller;
    }

    fn call_user(&mut self, def: &FnDef, args: Vec<CallArg>, span: Span) -> Result<Value, Diagnostic> {
        let bound = bind(def, args, span)?;
        let caller = self.enter(span)?;
        for (param, arg) in def.params.iter().zip(&bound) {
            self.frame.declare(&param.name, arg.value.clone(), param.is_mut);
        }
        let result = self.exec_stmts(&def.body);
        let changed: Vec<(String, Value)> = def
            .params
            .iter()
            .zip(&bound)
            .filter_map(|(param, arg)| {
                Some((arg.mut_place.clone()?, self.frame.get(&param.name)?.value.clone()))
            })
            .collect();
        self.leave(caller);
        for (place, value) in changed {
            if let Some(binding) = self.frame.get_mut(&place) {
                binding.value = value;
            }
        }
        match result? {
            Flow::Normal(value) | Flow::Return(value) => Ok(value),
            Flow::Break | Flow::Continue => {
                Err(Diagnostic::new("`break` or `continue` outside a loop", def.span))
            }
        }
    }

    fn call_lambda(&mut self, lambda: &Lambda, args: Vec<CallArg>, span: Span) -> Result<Value, Diagnostic> {
        if args.len() != lambda.params.len() {
            return Err(Diagnostic::new(
                format!(
                    "this function takes {} argument(s), not {}",
                    lambda.params.len(),
                    args.len()
                ),
                span,
            ));
        }
        if let Some(arg) = args
            .iter()
            .find(|arg| arg.name.is_some() || arg.mut_place.is_some())
        {
            return Err(Diagnostic::new("arrow functions take plain arguments", arg.span));
        }
        let caller = self.enter(span)?;
        for (name, value) in &lambda.captured {
            self.frame.declare(name, value.clone(), false);
        }
        self.frame.push();
        for (param, arg) in lambda.params.iter().zip(args) {
            self.frame.declare(param, arg.value, false);
        }
        let result = self.eval(&lambda.body);
        self.leave(caller);
        result
    }
}

/// Matches arguments to parameters, positionally and by name.
fn bind(def: &FnDef, args: Vec<CallArg>, span: Span) -> Result<Vec<CallArg>, Diagnostic> {
    let mut slots: Vec<Option<CallArg>> = def.params.iter().map(|_| None).collect();
    let mut next = 0;
    for arg in args {
        let at = if let Some(name) = &arg.name {
            def.params
                .iter()
                .position(|param| &param.name == name)
                .ok_or_else(|| {
                    Diagnostic::new(
                        format!("`{}` has no parameter called `{name}`", def.name),
                        arg.span,
                    )
                })?
        } else {
            next += 1;
            next - 1
        };
        let (Some(param), Some(slot)) = (def.params.get(at), slots.get_mut(at)) else {
            return Err(Diagnostic::new(
                format!("`{}` takes {} argument(s)", def.name, def.params.len()),
                arg.span,
            ));
        };
        check_mut(def, param, &arg)?;
        if slot.is_some() {
            return Err(Diagnostic::new(
                format!("`{}` is given twice", param.name),
                arg.span,
            ));
        }
        *slot = Some(arg);
    }
    slots
        .into_iter()
        .zip(&def.params)
        .map(|(slot, param)| {
            slot.ok_or_else(|| {
                Diagnostic::new(
                    format!("`{}` is missing its `{}` argument", def.name, param.name),
                    span,
                )
            })
        })
        .collect()
}

fn check_mut(def: &FnDef, param: &Param, arg: &CallArg) -> Result<(), Diagnostic> {
    match (param.is_mut, arg.mut_place.is_some()) {
        (true, false) => Err(Diagnostic::new(
            format!(
                "`{}` changes `{}`, so it has to be passed with `mut`",
                def.name, param.name
            ),
            arg.span,
        )
        .with_help("write `mut` before the argument, so readers can see it changes")),
        (false, true) => Err(Diagnostic::new(
            format!(
                "`{}` doesn't change `{}`, so it shouldn't be passed with `mut`",
                def.name, param.name
            ),
            arg.span,
        )),
        _ => Ok(()),
    }
}
