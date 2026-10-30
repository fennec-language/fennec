//! Calling functions, arrow functions, methods and constructors. A `mut` argument is taken
//! out of its place, handed to the callee, and put back when the call returns.

use std::rc::Rc;

use fennec_syntax::{Arg, Diagnostic, Expr, ExprKind, Param, Span};

use crate::exec::Flow;
use crate::interp::Interp;
use crate::place::Place;
use crate::scope::Frame;
use crate::value::{Callable, Lambda, Value};

/// An evaluated argument. `mut_place` is where a `mut` argument came from.
pub(crate) struct CallArg {
    pub(crate) name: Option<String>,
    pub(crate) value: Value,
    pub(crate) mut_place: Option<Place>,
    /// Whether the argument was written as a literal, so rules on it can be checked now.
    pub(crate) literal: bool,
    pub(crate) span: Span,
}

impl CallArg {
    pub(crate) fn plain(value: Value, span: Span) -> Self {
        CallArg { name: None, value, mut_place: None, literal: false, span }
    }
}

impl Interp<'_> {
    pub(crate) fn call(&mut self, callee: &Expr, args: &[Arg], span: Span) -> Result<Value, Diagnostic> {
        match &callee.kind {
            ExprKind::Attr { object, name, optional } => {
                return self.method_call(object, name, *optional, args, span);
            }
            ExprKind::Variant(name) => {
                let args = self.eval_args(args)?;
                return self.construct_variant(None, name, args, span);
            }
            _ => {}
        }
        let function = self.eval(callee)?;
        let args = self.eval_args(args)?;
        self.call_value(&function, args, span)
    }

    pub(crate) fn eval_args(&mut self, args: &[Arg]) -> Result<Vec<CallArg>, Diagnostic> {
        let mut evaluated = Vec::with_capacity(args.len());
        for arg in args {
            let mut_place = if arg.is_mut {
                Some(self.mut_place(
                    &arg.value,
                    "only a name, or a field or item of one, can be passed with `mut`",
                )?)
            } else {
                None
            };
            let value = self.eval(&arg.value)?;
            let literal = matches!(
                arg.value.kind,
                ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Bool(_) | ExprKind::None
            );
            evaluated.push(CallArg {
                name: arg.name.clone(),
                value,
                mut_place,
                literal,
                span: arg.value.span,
            });
        }
        // Take `mut` arguments out of their places, so the callee changes them without copying.
        for arg in &evaluated {
            if let Some(place) = &arg.mut_place {
                self.take(place)?;
            }
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
            Value::Function(callable) => self.call_user(callable, args, span),
            Value::Builtin(name) => self.builtin(name, args, span),
            Value::Lambda(lambda) => self.call_lambda(lambda, args, span),
            Value::Type(name) => self.construct(name, args, span),
            other => Err(Diagnostic::new(format!("a {} can't be called", other.type_name()), span)),
        }
    }

    /// Calls a function with plain values, as `main`, operators and keys do.
    pub(crate) fn call_with_values(
        &mut self,
        function: &Callable,
        values: Vec<Value>,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let args = values.into_iter().map(|value| CallArg::plain(value, span)).collect();
        self.call_user(function, args, span)
    }

    fn enter(&mut self, span: Span) -> Result<Frame, Diagnostic> {
        if self.depth >= self.max_depth {
            return Err(Diagnostic::new("calls nested too deeply", span)
                .with_help("check for recursion that never stops"));
        }
        self.depth += 1;
        Ok(std::mem::replace(&mut self.frame, Frame::new()))
    }

    fn leave(&mut self, caller: Frame) {
        self.depth -= 1;
        self.frame = caller;
    }

    pub(crate) fn call_user(
        &mut self,
        function: &Callable,
        args: Vec<CallArg>,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let def = &function.def;
        let bound = match bind(&def.name, &def.params, args, span) {
            Ok(bound) => bound,
            Err((diagnostic, args)) => {
                self.restore(args)?;
                return Err(diagnostic);
            }
        };
        let caller = self.enter(span)?;
        for (name, value) in &function.captured {
            self.frame.declare(name, value.clone(), false);
        }
        self.frame.push();
        for (param, arg) in def.params.iter().zip(&bound) {
            self.frame.declare(&param.name, arg.value.clone(), param.is_mut);
        }
        if function.generator {
            self.yields.push(Vec::new());
        }
        let flow = self.exec_stmts(&def.body);
        let yielded = if function.generator { self.yields.pop() } else { None };
        let changed: Vec<(Place, Value)> = def
            .params
            .iter()
            .zip(&bound)
            .filter_map(|(param, arg)| {
                Some((arg.mut_place.clone()?, self.frame.get(&param.name)?.value.clone()))
            })
            .collect();
        self.leave(caller);
        for (place, value) in changed {
            self.put(&place, value)?;
        }
        let value = match flow? {
            Flow::PassUp(value) => return Ok(value),
            Flow::Normal(value) | Flow::Return(value) => value,
            Flow::Break | Flow::Continue => {
                return Err(Diagnostic::new("`break` or `continue` outside a loop", def.span));
            }
        };
        if let Some(yielded) = yielded {
            return Ok(Value::list(yielded));
        }
        Ok(if function.fallible && !matches!(value, Value::Ok(_) | Value::Err(_)) {
            Value::Ok(Rc::new(value))
        } else {
            value
        })
    }

    /// Puts `mut` arguments back after a call that never started.
    fn restore(&mut self, args: Vec<CallArg>) -> Result<(), Diagnostic> {
        for arg in args {
            if let Some(place) = &arg.mut_place {
                self.put(place, arg.value)?;
            }
        }
        Ok(())
    }

    fn call_lambda(&mut self, lambda: &Lambda, args: Vec<CallArg>, span: Span) -> Result<Value, Diagnostic> {
        if args.len() != lambda.params.len() {
            let message =
                format!("this function takes {} argument(s), not {}", lambda.params.len(), args.len());
            self.restore(args)?;
            return Err(Diagnostic::new(message, span));
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

    /// Calls a method on whatever `object` names, taking it out of its place and putting it back.
    pub(crate) fn call_mut_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[Expr],
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let receiver = self.eval(object)?;
        let Value::Instance(instance) = &receiver else {
            return Err(Diagnostic::new(
                format!("a {} has no method `{method}`", receiver.type_name()),
                span,
            ));
        };
        let function = self
            .type_info(&instance.type_name)
            .and_then(|info| info.methods.get(method).cloned())
            .ok_or_else(|| {
                Diagnostic::new(format!("`{}` has no method `{method}`", instance.type_name), span)
            })?;
        let args: Vec<Arg> =
            args.iter().map(|value| Arg { name: None, is_mut: false, value: value.clone() }).collect();
        drop(receiver);
        self.call_method(object, &function, &args, span)
    }

    /// Calls a user-defined method. A `mut self` method needs its receiver in a mut place.
    pub(crate) fn call_method(
        &mut self,
        object: &Expr,
        function: &Callable,
        args: &[Arg],
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let takes_mut_self = function.def.params.first().is_some_and(|param| param.is_self && param.is_mut);
        let mut evaluated = self.eval_args(args)?;
        let receiver = if takes_mut_self {
            let what = format!(
                "`{}` changes its value, so call it on something held in a mut name",
                function.def.name
            );
            let place = match self.mut_place(object, &what) {
                Ok(place) => place,
                Err(diagnostic) => {
                    self.restore(evaluated)?;
                    return Err(diagnostic);
                }
            };
            let value = self.take(&place)?;
            CallArg { name: None, value, mut_place: Some(place), literal: false, span }
        } else {
            CallArg::plain(self.eval(object)?, span)
        };
        evaluated.insert(0, receiver);
        self.call_user(function, evaluated, span)
    }

    pub(crate) fn has_method(&mut self, object: &Expr, method: &str) -> Result<bool, Diagnostic> {
        Ok(match self.eval(object)? {
            Value::Instance(instance) => {
                self.type_info(&instance.type_name).is_some_and(|info| info.methods.contains_key(method))
            }
            _ => false,
        })
    }

    /// Runs a value's `def del` when a `with` block ends.
    pub(crate) fn clean_up(&mut self, value: Value, span: Span) -> Result<(), Diagnostic> {
        if let Value::Instance(instance) = &value
            && let Some(cleanup) =
                self.type_info(&instance.type_name).and_then(|info| info.methods.get("del").cloned())
        {
            self.call_with_values(&cleanup, vec![value], span)?;
        }
        Ok(())
    }
}

/// Matches arguments to parameters, positionally and by name. On failure, hands the
/// arguments back so `mut` ones can be put back.
fn bind(
    function: &str,
    params: &[Param],
    args: Vec<CallArg>,
    span: Span,
) -> Result<Vec<CallArg>, (Diagnostic, Vec<CallArg>)> {
    let mut slots: Vec<Option<CallArg>> = params.iter().map(|_| None).collect();
    let mut next = 0;
    let mut problem = None;
    let mut leftovers = Vec::new();
    for arg in args {
        if problem.is_some() {
            leftovers.push(arg);
            continue;
        }
        let at = if let Some(name) = &arg.name {
            params.iter().position(|param| &param.name == name)
        } else {
            next += 1;
            Some(next - 1)
        };
        let checked = match (at.and_then(|at| params.get(at)), at) {
            (Some(param), Some(at)) => check_mut(function, param, &arg).and_then(|()| {
                if slots.get(at).is_some_and(Option::is_some) {
                    Err(Diagnostic::new(format!("`{}` is given twice", param.name), arg.span))
                } else {
                    Ok(at)
                }
            }),
            _ => Err(match &arg.name {
                Some(name) => {
                    Diagnostic::new(format!("`{function}` has no parameter called `{name}`"), arg.span)
                }
                None => Diagnostic::new(format!("`{function}` takes {} argument(s)", params.len()), arg.span),
            }),
        };
        match checked {
            Ok(at) => {
                if let Some(slot) = slots.get_mut(at) {
                    *slot = Some(arg);
                }
            }
            Err(diagnostic) => {
                problem = Some(diagnostic);
                leftovers.push(arg);
            }
        }
    }
    let missing = params.iter().zip(&slots).find(|(_, slot)| slot.is_none()).map(|(param, _)| param);
    let problem = problem.or_else(|| {
        missing.map(|param| {
            Diagnostic::new(format!("`{function}` is missing its `{}` argument", param.name), span)
        })
    });
    match problem {
        Some(diagnostic) => {
            leftovers.extend(slots.into_iter().flatten());
            Err((diagnostic, leftovers))
        }
        None => Ok(slots.into_iter().flatten().collect()),
    }
}

fn check_mut(function: &str, param: &Param, arg: &CallArg) -> Result<(), Diagnostic> {
    if param.is_self {
        return Ok(());
    }
    match (param.is_mut, arg.mut_place.is_some()) {
        (true, false) => Err(Diagnostic::new(
            format!("`{function}` changes `{}`, so it has to be passed with `mut`", param.name),
            arg.span,
        )
        .with_help("write `mut` before the argument, so readers can see it changes")),
        (false, true) => Err(Diagnostic::new(
            format!("`{function}` doesn't change `{}`, so it shouldn't be passed with `mut`", param.name),
            arg.span,
        )),
        _ => Ok(()),
    }
}
