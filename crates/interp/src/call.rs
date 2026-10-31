//! Calling functions, arrow functions, methods and constructors. A `mut` argument is taken
//! out of its place, handed to the callee, and put back when the call returns.

use std::rc::Rc;

use fennec_syntax::{Arg, CompKind, Diagnostic, Expr, ExprKind, Param, Span};

use crate::eval::Parts;
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

    pub(crate) fn named(name: &str, value: Value, span: Span) -> Self {
        CallArg { name: Some(name.to_owned()), ..CallArg::plain(value, span) }
    }
}

impl Interp<'_> {
    pub(crate) fn call(&mut self, callee: &Expr, args: &[Arg], span: Span) -> Result<Value, Diagnostic> {
        match &callee.kind {
            ExprKind::Attr { object, name, optional } => {
                if let Some(member) = self.module_member(object, name, callee.span) {
                    let function = self.lookup(&member?, callee.span)?;
                    let args = self.eval_args(args)?;
                    return self.call_value(&function, args, span);
                }
                return self.method_call(object, name, *optional, args, span);
            }
            ExprKind::Variant(name) => {
                let args = self.eval_args(args)?;
                return self.construct_variant(None, name, args, span);
            }
            ExprKind::Index { object, index }
                if matches!(object.kind, ExprKind::Name(_) | ExprKind::Attr { .. }) =>
            {
                let function = self.eval(object)?;
                match (&function, index.as_type()) {
                    (Value::Builtin(name), Some(ty)) if matches!(*name, "json.loads" | "toml.loads") => {
                        let args = self.eval_args(args)?;
                        return self.typed_loads(name, &ty, args, span);
                    }
                    // Types in brackets after a generic function or type, as in `largest[int](xs)`
                    // or `Deque[int]()`, matter only to the type checker.
                    (Value::Function(_) | Value::Type(_), _) => {
                        let args = self.eval_args(args)?;
                        return self.call_value(&function, args, span);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        let function = self.eval(callee)?;
        let args = match (&function, args) {
            (Value::Builtin(name), [first, rest @ ..]) if pulls_it_all(name, first) => {
                let items = self.items_in_place(&first.value)?;
                let mut args = vec![CallArg {
                    name: None,
                    value: items,
                    mut_place: None,
                    literal: false,
                    span: first.value.span,
                }];
                args.extend(self.eval_args(rest)?);
                args
            }
            (Value::Builtin(name @ ("any" | "all")), [only]) if is_generator(&only.value) => {
                return self.any_or_all_in_place(name, &only.value, span);
            }
            _ => self.eval_args(args)?,
        };
        self.call_value(&function, args, span)
    }

    /// A generator written right inside what uses all of it, whose items are worked out here,
    /// able to change the names around it.
    fn items_in_place(&mut self, generator: &Expr) -> Result<Value, Diagnostic> {
        let ExprKind::Comprehension { elem, names, iter, cond, .. } = &generator.kind else {
            return self.eval(generator);
        };
        let parts = Parts { elem, value: None, cond: cond.as_deref() };
        self.comprehension(CompKind::List, &parts, names, iter)
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

    pub(crate) fn enter(&mut self, span: Span) -> Result<Frame, Diagnostic> {
        if self.depth >= self.max_depth {
            return Err(Diagnostic::new("calls nested too deeply", span)
                .with_help("check for recursion that never stops"));
        }
        self.depth += 1;
        Ok(std::mem::replace(&mut self.frame, Frame::new()))
    }

    pub(crate) fn leave(&mut self, caller: Frame) {
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
        self.ran_program_code |= !def.span.in_prelude();
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
        // Arguments move into the frame, so a `mut` argument the callee changes has one owner
        // and changes in place, rather than being copied on its first write.
        let mut mut_places = Vec::new();
        for (param, arg) in def.params.iter().zip(bound) {
            if let Some(place) = arg.mut_place {
                mut_places.push((place, param.name.as_str()));
            }
            self.frame.declare(&param.name, arg.value, param.is_mut);
        }
        if function.generator {
            // Nothing runs until an item is asked for.
            let frame = std::mem::replace(&mut self.frame, Frame::new());
            self.leave(caller);
            let generator = crate::generators::of_function(Rc::clone(def), frame);
            return Ok(if function.fallible { Value::Ok(Rc::new(generator)) } else { generator });
        }
        let flow = self.exec_stmts(&def.body).map_err(|diagnostic| at_call(diagnostic, span));
        let changed: Vec<(Place, Value)> = mut_places
            .into_iter()
            .filter_map(|(place, name)| {
                let binding = self.frame.get_mut(name)?;
                Some((place, std::mem::replace(&mut binding.value, Value::Unit)))
            })
            .collect();
        self.leave(caller);
        self.clean_up_abandoned()?;
        for (place, value) in changed {
            self.put(&place, value)?;
        }
        let value = match flow? {
            Flow::PassUp(value) if function.fallible => return Ok(value),
            Flow::PassUp(value) => {
                // Only a generator it was handed can get a failure into a function like this.
                return Err(crate::generators::nowhere_to_pass(&value, span));
            }
            Flow::Normal(value) | Flow::Return(value) => value,
            Flow::Yield(_) | Flow::Break | Flow::Continue => {
                return Err(Diagnostic::new("`break` or `continue` outside a loop", def.span));
            }
        };
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
    // A standard library function's calls have no defaults written into them.
    for (param, slot) in params.iter().zip(&mut slots) {
        if let (None, Some(value)) = (slot.as_ref(), param.default.as_ref().and_then(plain_value)) {
            *slot = Some(CallArg { name: None, value, mut_place: None, literal: true, span });
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

/// The value of a parameter's default.
fn plain_value(default: &Expr) -> Option<Value> {
    use fennec_syntax::UnaryOp;
    Some(match &default.kind {
        ExprKind::Int(value) => Value::Int(*value),
        ExprKind::Float(value) => Value::Float(*value),
        ExprKind::Str(value) => Value::str(value),
        ExprKind::Bool(value) => Value::Bool(*value),
        ExprKind::None => Value::None,
        ExprKind::Unary { op: UnaryOp::Neg, expr } => match expr.kind {
            ExprKind::Int(value) => Value::Int(value.wrapping_neg()),
            ExprKind::Float(value) => Value::Float(-value),
            _ => return None,
        },
        _ => return None,
    })
}

/// A failure inside the standard library is reported where the program called it.
pub(crate) fn at_call(mut diagnostic: Diagnostic, call: Span) -> Diagnostic {
    if diagnostic.span.in_prelude() && !call.in_prelude() {
        diagnostic.span = call;
    }
    diagnostic
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

fn is_generator(expr: &Expr) -> bool {
    matches!(&expr.kind, ExprKind::Comprehension { kind: CompKind::Generator, .. })
}

/// Whether this builtin uses every item of a generator written right inside the call.
fn pulls_it_all(name: &str, first: &Arg) -> bool {
    is_generator(&first.value) && matches!(name, "sum" | "min" | "max" | "sorted" | "list" | "set" | "dict")
}
