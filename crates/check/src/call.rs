//! Calls: functions, methods, named constructors and operators, and building structs,
//! errors and enum cases. Arguments are matched to parameters or fields, positionally and by
//! name, and `mut` has to be written exactly where a parameter changes.

use fennec_syntax::{Arg, CompKind, Diagnostic, Expr, ExprKind, Field, Generic, Span, UnaryOp};

use crate::ParamType;
use crate::builtins;
use crate::expr::Member;
use crate::function::{FnCx, Origin};
use crate::generic::Chosen;
use crate::infer::Hole;
use crate::messages::{mismatch, no_method, unhandled_result};
use crate::program::TypeKind;
use crate::types::{Type, a};

/// A method call: `object.method(args)`.
#[derive(Clone, Copy)]
struct MethodCall<'a> {
    object: &'a Expr,
    method: &'a str,
    /// The types written in brackets for the method's own type parameters.
    types: &'a [Type],
    args: &'a [Arg],
    span: Span,
}

/// What's being built: a type, or one case of an enum, with the types filled in for the
/// type's parameters.
struct Target<'a> {
    name: &'a str,
    case: Option<&'a str>,
    given: Vec<(String, Type)>,
}

const MUT_ARG: &str = "only a name, or a field or item of one, can be passed with `ref`";

impl FnCx<'_> {
    /// Whether this is one of the functions on built-in types, `str.from_code(n)` and
    /// `bytes.fromhex(text)`.
    fn is_built_in_function(&self, object: &Expr, name: &str) -> bool {
        matches!(&object.kind, ExprKind::Name(owner) if self.lookup(owner).is_none()
            && matches!((owner.as_str(), name), ("str", "from_code") | ("bytes", "fromhex")))
    }

    fn built_in_function(&mut self, name: &str, args: &[Arg], span: Span) -> Type {
        if name == "from_code" { self.char_from_code(args, span) } else { self.bytes_from_hex(args, span) }
    }

    /// What's called with types given and isn't a type or a function of this module's: `json.loads[T]`,
    /// a module's generic function like `pack.loads[T]`, checked as the function's own name with
    /// them, or a method with types for its own type parameters, like `src.field[int]()`.
    fn called_with_types(
        &mut self,
        (object, index): (&Expr, &Expr),
        callee_span: Span,
        args: &[Arg],
        span: Span,
    ) -> Option<Type> {
        if let Some(builtin) = self.loads(object) {
            return Some(self.typed_loads(&builtin, index, args, span));
        }
        let ExprKind::Attr { object: owner, name, .. } = &object.kind else {
            return self.typed_method_call(object, index, args, span);
        };
        let Some(Member::Name(member)) = self.module_member(owner, name, object.span) else {
            return self.typed_method_call(object, index, args, span);
        };
        let named = Expr { kind: ExprKind::Name(member), span: object.span };
        let given = Expr {
            kind: ExprKind::Index { object: Box::new(named), index: Box::new(index.clone()) },
            span: callee_span,
        };
        Some(self.call(&given, args, span))
    }

    /// A generator right inside what uses it is worked out here, in order, as a loop would be.
    fn note_generator_worked_out_here(&mut self, callee: &Expr, args: &[Arg]) {
        if let (ExprKind::Name(name), Some(first)) = (&callee.kind, args.first())
            && matches!(name.as_str(), "sum" | "min" | "max" | "sorted" | "list" | "set" | "any" | "all")
            && matches!(first.value.kind, ExprKind::Comprehension { kind: CompKind::Generator, .. })
        {
            self.worked_out_here.insert((first.value.span.start, first.value.span.end));
        }
    }

    pub(crate) fn call(&mut self, callee: &Expr, args: &[Arg], span: Span) -> Type {
        self.closure_locks_again(callee);
        self.note_generator_worked_out_here(callee, args);
        self.no_password_in_imported_url(callee, args);
        match &callee.kind {
            ExprKind::Attr { object, name, .. } if self.is_built_in_function(object, name) => {
                return self.built_in_function(name, args, span);
            }
            ExprKind::Attr { object, name, optional } => {
                return match self.module_member(object, name, callee.span) {
                    Some(Member::Name(member)) => {
                        self.no_password_in_written_url(&member, args);
                        self.call(&Expr { kind: ExprKind::Name(member), span: callee.span }, args, span)
                    }
                    Some(Member::Missing) => {
                        self.ignore_args(args);
                        Type::Unknown
                    }
                    None => self.method_call(object, name, (*optional, callee.span), (&[], args), span),
                };
            }
            ExprKind::Variant(name) => return self.construct_variant(None, name, args, span),
            ExprKind::Index { object, index } => {
                if let Some(made) = self.made_collection(callee, (object, index), args, span) {
                    return made;
                }
                if let Some(type_name) = self.as_type_name(object) {
                    if self.checker.program.type_info(type_name).is_some() {
                        let types = self.types_in(index);
                        return self.construct_with(type_name, (args, &types), span);
                    }
                    return self.construct_builtin(type_name, Some(index), args, span);
                }
                if let ExprKind::Name(name) = &object.kind
                    && self.lookup(name).is_none()
                    && self.checker.program.functions.contains_key(name)
                {
                    let types = self.types_in(index);
                    self.record(object.span, &Type::Function(name.clone()));
                    return self.call_with_types(name, name, (args, &types), span);
                }
                if let ExprKind::Name(name) = &object.kind
                    && self.lookup(name).is_none()
                    && let Some(function) = self.std_function(name, object.span)
                {
                    let types = self.types_in(index);
                    self.record(object.span, &Type::Function(function.clone()));
                    return self.call_with_types(&function, name, (args, &types), span);
                }
                if let Some(returns) = self.called_with_types((object, index), callee.span, args, span) {
                    return returns;
                }
            }
            ExprKind::Name(name) if self.lookup(name).is_none() => {
                if let Some(returns) = self.call_by_name(name, callee, args, span) {
                    return returns;
                }
            }
            _ => {}
        }
        match self.expr(callee) {
            Type::Function(key) => {
                let name = key.rsplit('.').next().unwrap_or_default().to_owned();
                self.call_function(&key, &name, args, span)
            }
            Type::Arrow(params, returns) => self.call_value(callee, (params, *returns), args, span),
            Type::Unknown => {
                self.ignore_args(args);
                Type::Unknown
            }
            other => {
                let message = format!("{} can't be called", a(&other));
                self.unusable(Diagnostic::new(message, callee.span), args)
            }
        }
    }

    /// A call through a function value, as `work()` with `work: () -> nothing`.
    fn call_value(
        &mut self,
        callee: &Expr,
        (params, returns): (Vec<Type>, Type),
        args: &[Arg],
        span: Span,
    ) -> Type {
        self.note_value_call(callee, span);
        let params: Vec<ParamType> =
            params.into_iter().enumerate().map(|(at, ty)| argument_param(at, ty)).collect();
        // Through a function value, what's called can't be followed by its parameters.
        self.calling.push(String::new());
        self.bind_args("this function", &params, args, span);
        self.calling.pop();
        returns
    }

    /// A call to a name that isn't a local: a function, a type or a builtin.
    fn call_by_name(&mut self, name: &str, callee: &Expr, args: &[Arg], span: Span) -> Option<Type> {
        if let Some(refused) = self.test_only_function(name, span) {
            self.error(refused);
        }
        if self.checker.program.functions.contains_key(name) {
            self.record(callee.span, &Type::Function(name.to_owned()));
            return Some(self.call_function(name, name, args, span));
        }
        if let Some(function) = self.std_function(name, callee.span) {
            self.record(callee.span, &Type::Function(function.clone()));
            return Some(self.call_function(&function, name, args, span));
        }
        if self.checker.program.is_type_name(name) {
            return Some(self.construct(name, args, span));
        }
        if let Some(builtin) = self.checker.program.imported(name, callee.span).cloned() {
            return Some(self.call_builtin(&builtin, args, span));
        }
        if matches!(name, "len" | "abs") || builtins::is_builtin(name) {
            return Some(self.call_global(name, callee, args, span));
        }
        // `with launch(...)`, the one place `launch` is written.
        if name == "launch" {
            return Some(self.launch(args, span));
        }
        None
    }

    /// `failure_of` is only for tests: anywhere else a failure is named where it happens, with
    /// `else as why:`. The standard library's own code may still call it.
    fn test_only_function(&self, name: &str, span: Span) -> Option<Diagnostic> {
        let entry = self.checker.program.functions.get(name)?;
        if name != "failure_of" || !entry.prelude || self.in_a_test() || span.in_prelude() {
            return None;
        }
        let help = "outside a test, name a failure where it happens with `x = f() else as why:`; a test checks one with `assert f() fails Oops()`";
        Some(Diagnostic::new("`failure_of` is only for tests", span).with_help(help))
    }

    /// `json.loads` or `toml.loads`, imported by module or by name, without being called.
    fn loads(&mut self, callee: &Expr) -> Option<String> {
        let builtin = match &callee.kind {
            ExprKind::Attr { object, name, .. } => match self.module_member(object, name, callee.span)? {
                Member::Name(member) => member,
                Member::Missing => return None,
            },
            ExprKind::Name(name) if self.lookup(name).is_none() => {
                self.checker.program.imported(name, callee.span)?.clone()
            }
            _ => return None,
        };
        matches!(builtin.as_str(), "json.loads" | "toml.loads").then_some(builtin)
    }

    /// An argument's type. An arrow function gets its parameters' types from `params`.
    pub(crate) fn arg_value(&mut self, arg: &Arg, params: &[Type]) -> Type {
        self.arg_as(arg, params, None)
    }

    /// An argument's type, where a value of type `expected` goes, if that's known.
    fn arg_as(&mut self, arg: &Arg, params: &[Type], expected: Option<&Type>) -> Type {
        if arg.is_mut {
            self.mut_place(&arg.value, MUT_ARG);
        }
        match (&arg.value.kind, expected) {
            (ExprKind::Lambda { params: names, body }, _) => {
                let ty = self.lambda(names, body, params, arg.value.span);
                self.failing_arrow(body, &ty, expected);
                match self.widened_arrow(&ty, expected) {
                    Some(widened) => {
                        self.record(arg.value.span, &widened);
                        widened
                    }
                    None => ty,
                }
            }
            // A function named as a value fails as its own type says, so one failing fewer ways
            // than the function type it's given for is written in as an arrow calling it.
            (ExprKind::Name(_), Some(expected)) => {
                let ty = self.expr_as(&arg.value, expected);
                let arrow = match &ty {
                    Type::Function(key) => self.checker.signature(key).map(|signature| {
                        let params = signature.params.iter().map(|param| param.ty.clone()).collect();
                        Type::Arrow(params, Box::new(signature.returns))
                    }),
                    other => Some(other.clone()),
                };
                if let Some(Type::Arrow(params, _)) =
                    arrow.and_then(|arrow| self.widened_arrow(&arrow, Some(expected)))
                {
                    let at = (arg.value.span.start, arg.value.span.end);
                    self.checker.widened_values.insert(at, params.len());
                }
                ty
            }
            (_, Some(expected)) => self.expr_as(&arg.value, expected),
            (_, None) => self.expr(&arg.value),
        }
    }

    /// An arrow function that fails fewer ways than the function type it's given for, as one
    /// failing with `Refused` where `() -> str fails Refused | Shut` goes, typed as failing all of
    /// its ways: it's made as that function, whose `?`s pass each failure up as one of them, as a
    /// `def`'s do.
    pub(crate) fn widened_arrow(&mut self, ty: &Type, takes: Option<&Type>) -> Option<Type> {
        let Type::Arrow(params, returns) = ty else { return None };
        let Type::Result(value, error) = self.checker.settled(returns) else { return None };
        let Type::Arrow(_, wanted) = self.checker.settled(takes?) else { return None };
        let Type::Result(_, wider) = self.checker.settled(&wanted) else { return None };
        let (error, wider) = (self.checker.settled(&error), self.checker.settled(&wider));
        (wider.is_known() && error.is_known() && error != wider && self.checker.fits(&wider, &error))
            .then(|| Type::Arrow(params.clone(), Box::new(Type::Result(value, Box::new(wider)))))
    }

    /// Refuses an arrow function whose `?` passes a failure up, given to a builtin, as `sorted`'s
    /// `key=`: none takes one that fails, so nothing there is told to expect a failure. One whose
    /// body gives back what can fail as it is was refused for that already.
    fn failing_arrow(&mut self, body: &Expr, ty: &Type, takes: Option<&Type>) {
        let Type::Arrow(_, returns) = ty else { return };
        if !matches!(self.checker.settled(returns), Type::Result(..))
            || self
                .recorded(body.span)
                .is_some_and(|ty| matches!(self.checker.settled(&ty), Type::Result(..)))
        {
            return;
        }
        // Where a parameter's type says what it takes, its check says so.
        if takes.is_some() {
            return;
        }
        self.error(
            Diagnostic::new(
                "this arrow function can fail, but where it's passed takes one that can't",
                body.span,
            )
            .with_help(
                "give a value for a failure with `else` inside it, or write a loop, which can use `?`",
            ),
        );
    }

    /// Checks arguments to something that takes anything, like an arrow function.
    pub(crate) fn ignore_args(&mut self, args: &[Arg]) {
        for arg in args {
            self.arg_value(arg, &[]);
        }
    }

    pub(crate) fn call_function(&mut self, key: &str, name: &str, args: &[Arg], span: Span) -> Type {
        self.call_with_types(key, name, (args, &[]), span)
    }

    /// Calls a function, with the types for its type parameters that are written in
    /// brackets, as in `largest[int](xs)`.
    pub(crate) fn call_with_types(
        &mut self,
        key: &str,
        name: &str,
        given: (&[Arg], &[Type]),
        span: Span,
    ) -> Type {
        self.call_through(key, name, given, 0, span)
    }

    /// Calls a function, as `call_with_types` does, where the first `through` types written are
    /// those of the type a static method is called through: `int` in `Deque[int].of(items)`.
    fn call_through(
        &mut self,
        key: &str,
        name: &str,
        given: (&[Arg], &[Type]),
        through: usize,
        span: Span,
    ) -> Type {
        let (args, types) = given;
        let Some(signature) = self.checker.signature(key) else {
            self.ignore_args(args);
            return Type::Unknown;
        };
        let entry = self.checker.program.functions.get(key);
        let own = entry.map_or(types.len(), |entry| written_generics(&entry.def.generics) + through);
        if !self.takes_types(name, own, types, span) {
            self.ignore_args(args);
            return Type::Unknown;
        }
        let (signature, chosen) = self.instantiate(key, &signature, None, types, span);
        if !chosen.is_empty() {
            let types = chosen.iter().map(|chosen| chosen.ty.clone()).collect();
            let call =
                crate::generic_copies::Call { caller: self.key.clone(), callee: key.to_owned(), types };
            self.checker.generic_calls.insert((span.start, span.end), call);
        }
        if let Some(what) = crate::waits::intrinsic_waits(key) {
            self.note_wait(what, span);
        }
        self.note_lock_call(key, name, span);
        self.calling.push(key.to_owned());
        self.bind_args(name, &signature.params, args, span);
        self.calling.pop();
        self.check_bounds(chosen, span);
        self.note_source(key, &signature.returns, false, span);
        signature.returns
    }

    /// Matches arguments to parameters, positionally and by name.
    fn bind_args(&mut self, function: &str, params: &[ParamType], args: &[Arg], span: Span) {
        let mut given = vec![false; params.len()];
        let mut unmatched_args = false;
        let mut next = 0;
        for arg in args {
            let at = if let Some(name) = &arg.name {
                params.iter().position(|param| &param.name == name)
            } else {
                next += 1;
                Some(next - 1)
            };
            let found = at.and_then(|at| params.get(at).map(|param| (at, param)));
            if let (None, Some((_, param))) = (&arg.name, found)
                && param.default.is_some()
            {
                let message = format!("`{}` has a default, so it's passed by name", param.name);
                self.error(
                    Diagnostic::new(message, arg.value.span).with_help(format!("write `{}=...`", param.name)),
                );
            }
            // An arrow function passed for a function parameter receives its parameters' types.
            let receives = match found.map(|(_, param)| self.checker.settled(&param.ty)) {
                Some(Type::Arrow(receives, _)) => receives,
                _ => Vec::new(),
            };
            let ty = self.arg_as(arg, &receives, found.map(|(_, param)| &param.ty));
            self.handle_passed(&arg.value, &ty);
            if let Some((_, param)) = found
                && matches!(self.checker.settled(&param.ty), Type::Arrow(..))
            {
                self.note_passed_function(&param.name, &arg.value);
            }
            let Some((at, param)) = found else {
                let names: Vec<&str> = params.iter().map(|param| param.name.as_str()).collect();
                self.error(unmatched(function, ("parameter", "argument"), &names, arg));
                unmatched_args = true;
                continue;
            };
            if let Some(slot) = given.get_mut(at) {
                if *slot {
                    self.error(Diagnostic::new(format!("`{}` is given twice", param.name), arg.value.span));
                }
                *slot = true;
            }
            self.check_mut_arg(function, param, arg, params);
            self.check_arg(param, &ty, arg.value.span);
        }
        // An argument with a misspelled name is most likely the missing one.
        let mut left_out = Vec::new();
        for (param, given) in params.iter().zip(given) {
            if let (false, Some(default)) = (given, &param.default) {
                // What's written in for it is typed here, as lowering reads its type.
                self.record(default.span, &param.ty);
                left_out.push((param.name.clone(), plain_default(default)));
            } else if !given && !unmatched_args {
                let message = format!("`{function}` is missing its `{}` argument", param.name);
                self.error(Diagnostic::new(message, span));
            }
        }
        if !left_out.is_empty() {
            self.checker.defaults.insert((span.start, span.end), left_out);
        }
    }

    fn check_mut_arg(&mut self, function: &str, param: &ParamType, arg: &Arg, params: &[ParamType]) {
        let span = arg.value.span;
        match (param.is_mut, arg.is_mut) {
            (true, false) => self.error(
                Diagnostic::new(
                    format!("`{function}` changes `{}`, so it has to be passed with `ref`", param.name),
                    span,
                )
                .with_help("write `ref` before the argument, so readers can see it changes"),
            ),
            (false, true) => {
                let diagnostic = Diagnostic::new(
                    format!(
                        "`{function}` doesn't change `{}`, so it shouldn't be passed with `ref`",
                        param.name
                    ),
                    span,
                );
                // It's most likely meant for what the function does change.
                self.error(match params.iter().find(|other| other.is_mut) {
                    Some(changed) => diagnostic.with_help(format!(
                        "did you mean to put `ref` on `{}`, which `{function}` does change?",
                        changed.name
                    )),
                    None => diagnostic,
                });
            }
            // What it's given to can make it `None`, whatever a check for `None` said before.
            (true, true) => {
                if let ExprKind::Name(name) = &arg.value.kind
                    && self.checker.settled(&param.ty).maybe_none()
                    && let Some(declared) = self.declared_type(name)
                {
                    self.set_type(name, declared);
                }
            }
            (false, false) => {}
        }
    }

    /// Checks an argument's type fits its parameter.
    pub(crate) fn check_arg(&mut self, param: &ParamType, actual: &Type, span: Span) {
        if !self.checker.fits(&param.ty, actual) {
            let expected = self.checker.settled(&param.ty);
            self.error(mismatch(&format!("`{}`", param.name), &expected, actual, span));
        }
    }

    /// An operator defined by a value's type, like `def +` or `def []`, called with one
    /// argument.
    pub(crate) fn call_operator(&mut self, receiver: &Type, key: &str, arg: &Type, span: Span) -> Type {
        let Some(signature) = self.method_signature(receiver, key) else { return Type::Unknown };
        if let Some(param) = signature.params.get(1) {
            self.check_arg(param, arg, span);
        }
        signature.returns
    }

    /// Which of a type's definitions of an operator a use of it calls: the one defined for exactly
    /// the type on the right, when the type has one again for another, or else its first, `key`.
    pub(crate) fn operator_for(&mut self, receiver: &Type, key: String, right: &Type) -> String {
        let again = format!("{key}{}", crate::extend::OTHER_RIGHT);
        let others: Vec<String> = self
            .checker
            .program
            .functions
            .range(again.clone()..)
            .map(|(found, _)| found)
            .take_while(|found| found.starts_with(&again))
            .cloned()
            .collect();
        if others.is_empty() {
            return key;
        }
        let right = self.checker.settled(right);
        for each in std::iter::once(key.clone()).chain(others) {
            let takes = self
                .method_signature(receiver, &each)
                .and_then(|signature| signature.params.get(1).map(|param| param.ty.clone()));
            if takes.is_some_and(|takes| self.checker.settled(&takes) == right) {
                return each;
            }
        }
        key
    }

    /// An operator on a value of a type parameter, from one of its interfaces, like `def +`
    /// in `T: Addable`. `None` when none of them has it.
    pub(crate) fn interface_operator(
        &mut self,
        left: &Type,
        method: &str,
        right: &Type,
        span: Span,
    ) -> Option<Type> {
        let Type::Param(name) = left else { return None };
        let (_, def) = self.bound_method(name, method)?;
        let signature = self.interface_method(&def, left);
        if let Some(param) = signature.params.get(1) {
            self.check_arg(param, right, span);
        }
        Some(signature.returns)
    }

    /// `object.method(...)`. `step` is where `object.method` is written, which an `else` may
    /// make optional.
    fn method_call(
        &mut self,
        object: &Expr,
        method: &str,
        (optional, step): (bool, Span),
        (types, args): (&[Type], &[Arg]),
        span: Span,
    ) -> Type {
        if let Some(type_name) = self.as_type_name(object) {
            return self.static_call(type_name, method, args, span);
        }
        if let ExprKind::Name(held) = &object.kind
            && let Some((ty, _)) = self.lookup(held)
            && let Some(field) = self.field_get(&ty, method, args, span)
        {
            self.expr(object);
            return field;
        }
        // `u8.from(x)`: a function on a sized number type, from the standard library.
        if let ExprKind::Name(size) = &object.kind
            && self.lookup(size).is_none()
            && crate::types::Num::named(size).is_some()
        {
            let size = size.clone();
            return self.static_call(&size, method, args, span);
        }
        // `Deque[int].of(items)`: a static method, with the type's parameters written.
        if let ExprKind::Index { object: written, index } = &object.kind
            && let Some(type_name) = self.as_type_name(written)
            && self.checker.program.type_info(type_name).is_some()
        {
            // A static method's type parameters start with its type's, so they're given first.
            let types = self.types_in(index);
            let through = self.checker.program.type_info(type_name).map_or(0, |info| info.def.generics.len());
            if types.len() != through {
                let takes = if through == 1 { "1 type".to_owned() } else { format!("{through} types") };
                let message =
                    format!("`{type_name}` takes {takes} in brackets, but this gives {}", types.len());
                self.error(Diagnostic::new(message, index.span));
                self.ignore_args(args);
                return Type::Unknown;
            }
            return match self.checker.program.method(type_name, method) {
                Some((key, entry)) if !entry.def.takes_self() => {
                    self.call_through(&key, method, (args, &types), through, span)
                }
                _ => self.static_call(type_name, method, args, span),
            };
        }
        if let ExprKind::Name(param) = &object.kind
            && self.lookup(param).is_none()
            && self.generics.iter().any(|generic| generic.name == *param)
        {
            return self.static_on_param(param, method, (types, args), span);
        }
        let ty = self.expr(object);
        if self.outside_lock(object, &ty, span) {
            self.ignore_args(args);
            return Type::Unknown;
        }
        let ty = if method == "expect" { ty } else { self.failing_step(ty, object.span, step) };
        let optional = optional || self.chained_step(&ty, step);
        self.method_on(&MethodCall { object, method, types, args, span }, &ty, optional)
    }

    fn method_on(&mut self, call: &MethodCall<'_>, ty: &Type, optional: bool) -> Type {
        let MethodCall { object, method, args, span, .. } = *call;
        match ty {
            Type::None if optional => {
                self.ignore_args(args);
                Type::None
            }
            Type::Optional(inner) if optional => Type::optional(self.method_on(call, inner, false)),
            ty if ty.maybe_none() => self.unusable(crate::messages::missing_step(object.span), args),
            Type::Result(value, _) if method == "expect" => {
                self.expect_reason(args, span);
                value.as_ref().clone()
            }
            Type::Result(..) => self.unusable(unhandled_result(object.span), args),
            Type::Named(name) | Type::Generic(name, _) if self.checker.program.type_info(name).is_some() => {
                match self.checker.program.method(name, method) {
                    Some((key, entry)) => {
                        let takes_self = entry.def.takes_self();
                        self.call_method(call, ty, (&key, takes_self))
                    }
                    // Every error's notes, which the language gives each `error` type.
                    None if matches!(method, "notes" | "add_note") && self.checker.is_error_type(ty) => {
                        self.note_method(object, method, args, span)
                    }
                    // Methods written in Rust, like `Regex`'s.
                    None => self.builtin_method(object, ty, method, args, span),
                }
            }
            Type::Param(name) => self.interface_call(call, name),
            // A failure that's one of several errors has notes too: each error does.
            Type::Errors(_) if matches!(method, "notes" | "add_note") => {
                self.note_method(object, method, args, span)
            }
            Type::Union(members) => self.union_method(call, members),
            Type::Unknown | Type::Var(_) => {
                self.ignore_args(args);
                Type::Unknown
            }
            // Methods the standard library writes in Fennec for a built-in type, like
            // `str.startswith`; the rest are the engines' own.
            ty if let Some(found) = self.std_method(ty, method) => {
                self.call_method(call, ty, (&found.0, found.1))
            }
            _ => self.builtin_method(object, ty, method, args, span),
        }
    }

    /// A method of a union, which it has when every type in it does, giving back the same type
    /// on each. Which one runs depends on the member it holds.
    fn union_method(&mut self, call: &MethodCall<'_>, members: &[Type]) -> Type {
        let union = Type::Union(members.to_vec());
        if let Some(missing) = members.iter().find(|member| !self.has_method(member, call.method)) {
            let message =
                format!("{} has no method `{}`, so {} doesn't either", a(missing), call.method, a(&union));
            let help = "a union has a method when every type in it does";
            return self.unusable(
                Diagnostic::new(message, call.object.span.to(call.span)).with_help(help),
                call.args,
            );
        }
        let Some((first, rest)) = members.split_first() else { return Type::Unknown };
        let dispatch = match self.union_source(call, &union, first) {
            UnionSource::Dispatched(dispatch) => Some(dispatch),
            UnionSource::Refused => {
                self.ignore_args(call.args);
                return Type::Unknown;
            }
            UnionSource::NotOne => None,
        };
        let returns = self.method_on(call, first, false);
        let report = std::mem::replace(&mut self.report, false);
        let others: Vec<Type> = rest.iter().map(|member| self.method_on(call, member, false)).collect();
        self.report = report;
        let settled = self.checker.settled(&returns);
        if others.iter().any(|other| self.checker.settled(other) != settled) {
            let message =
                format!("`{}` gives back different types on the types in {}", call.method, a(&union));
            self.error(
                Diagnostic::new(message, call.span)
                    .with_help("give each the same return type, or use `is` or `match` to tell them apart"),
            );
            return Type::Unknown;
        }
        // Each member's source is written in under an `is` for it, so none is kept for the call.
        if let Some(dispatch) = dispatch {
            let at = (call.span.start, call.span.end);
            self.checker.sources.remove(&at);
            self.checker.union_sources.insert(at, dispatch);
        }
        returns
    }

    /// A `with` source every member of a union has. Called on a name whose members are named
    /// types, the `with` is written in once for each member, under an `is` for it, where that
    /// member's own source runs. Called on anything else, as a field, it's refused: `is` doesn't
    /// narrow it.
    fn union_source(&mut self, call: &MethodCall<'_>, union: &Type, first: &Type) -> UnionSource {
        let Some(owner) = first.type_name() else { return UnionSource::NotOne };
        let Some((_, entry)) = self.checker.program.method(owner, call.method) else {
            return UnionSource::NotOne;
        };
        if entry.def.ret.as_ref().is_none_or(|ret| ret.name != crate::lend::WITH) {
            return UnionSource::NotOne;
        }
        let Type::Union(members) = union else { return UnionSource::NotOne };
        let types: Option<Vec<String>> = members
            .iter()
            .map(|member| match member {
                Type::Named(name) => Some(name.clone()),
                _ => None,
            })
            .collect();
        if let (ExprKind::Name(path), Some(members)) = (&call.object.kind, types) {
            return UnionSource::Dispatched(crate::union_sources::Dispatch { path: path.clone(), members });
        }
        // A member that's a generic type's, as `Box[int]`, isn't one `is` names.
        let (message, help) = if let ExprKind::Name(held) = &call.object.kind {
            (
                format!(
                    "`{}` is a `with` source each type in {} has, and which one runs can't be chosen as the program runs yet",
                    call.method,
                    a(union)
                ),
                format!(
                    "narrow it first, as `if {held} is {owner}:` or a `match` case for `{owner}`, where it runs that type's source"
                ),
            )
        } else {
            let written = fennec_syntax::dotted(call.object);
            let last = written.as_deref().and_then(|path| path.rsplit('.').next()).unwrap_or("held");
            let whole = written.as_deref().unwrap_or("...");
            (
                format!(
                    "`{}` is a `with` source each type in {} has, called on what `is` can't narrow",
                    call.method,
                    a(union)
                ),
                format!(
                    "give it a name first, as `{last} = {whole}`, and call it on that: the source of the type it holds runs"
                ),
            )
        };
        self.error(Diagnostic::new(message, call.object.span.to(call.span)).with_help(help));
        UnionSource::Refused
    }

    /// Whether a type has a method by this name, its own or one std or the engines give it.
    pub(crate) fn has_method(&self, ty: &Type, method: &str) -> bool {
        let own = ty.type_name().is_some_and(|name| self.checker.program.method(name, method).is_some());
        own || self.std_method(ty, method).is_some() || builtins::method(ty, method).is_some()
    }

    /// The method the standard library gives a built-in type, if it has one by that name,
    /// and whether it takes `self`.
    fn std_method(&self, ty: &Type, method: &str) -> Option<(String, bool)> {
        let owner = ty.method_owner()?;
        self.checker.program.method(owner, method).map(|(key, entry)| (key, entry.def.takes_self()))
    }

    /// `T.make()`: a function with no `self` that an interface among a type parameter's bounds
    /// gives it. Each copy of the generic calls the one its type has.
    fn static_on_param(
        &mut self,
        param: &str,
        method: &str,
        (types, args): (&[Type], &[Arg]),
        span: Span,
    ) -> Type {
        let owner = Type::Param(param.to_owned());
        let Some((_, def)) = self.bound_method(param, method) else {
            let message = format!("`{param}` has nothing called `{method}`");
            let help = format!("bind `{param}` to an interface that has it, like `{param}: Made`");
            return self.unusable(Diagnostic::new(message, span).with_help(help), args);
        };
        if def.takes_self() {
            let message = format!("`{method}` needs a `{param}` to call it on");
            return self.unusable(Diagnostic::new(message, span), args);
        }
        if !self.takes_types(method, written_generics(&def.generics), types, span) {
            self.ignore_args(args);
            return Type::Unknown;
        }
        let signature = self.interface_method_with(&def, &owner, types, span);
        self.bind_args(method, &signature.params, args, span);
        signature.returns
    }

    /// A method called on a value of a type parameter, which has the methods of its
    /// interfaces and no others.
    fn interface_call(&mut self, call: &MethodCall<'_>, name: &str) -> Type {
        let MethodCall { object, method, types, args, span } = *call;
        // Any type's method of this name could be the one called.
        self.note_lock_call(&format!("*.{method}"), method, span);
        let owner = Type::Param(name.to_owned());
        let Some((_, def)) = self.bound_method(name, method) else {
            let help = match self.bounds_of(name).as_slice() {
                [] => format!("bind `{name}` to an interface that has it, like `{name}: HasLength`"),
                bounds => {
                    format!("`{}` has no `{method}` method; add it to the interface", bounds.join("` or `"))
                }
            };
            let message = no_method(&owner, method, span).message;
            return self.unusable(Diagnostic::new(message, span).with_help(help), args);
        };
        if !self.takes_types(method, written_generics(&def.generics), types, span) {
            self.ignore_args(args);
            return Type::Unknown;
        }
        let signature = self.interface_method_with(&def, &owner, types, span);
        let gives = match &signature.returns {
            Type::Result(value, _) => value.as_ref(),
            other => other,
        };
        if crate::lend::lent(gives).is_some() {
            self.checker.generic_sources.insert(self.key.clone());
        }
        if signature.params.first().is_some_and(|param| param.is_mut) {
            let what = format!("`{method}` changes its value, so call it on something held in a mut name");
            self.mut_place(object, &what);
        }
        self.bind_args(method, signature.params.get(1..).unwrap_or_default(), args, span);
        signature.returns
    }

    /// `src.field[int](0)`: a method, with types for its own type parameters.
    fn typed_method_call(&mut self, method: &Expr, index: &Expr, args: &[Arg], span: Span) -> Option<Type> {
        let ExprKind::Attr { object: receiver, name, optional } = &method.kind else { return None };
        if self.module_member(receiver, name, method.span).is_some() || self.as_type_name(receiver).is_some()
        {
            return None;
        }
        let types = self.types_in(index);
        Some(self.method_call(receiver, name, (*optional, method.span), (&types, args), span))
    }

    /// Whether a method call writes no more types in brackets than the method has type
    /// parameters of its own, reporting it when it writes more.
    fn takes_types(&mut self, method: &str, own: usize, types: &[Type], span: Span) -> bool {
        if types.len() <= own {
            return true;
        }
        let takes = match own {
            0 => "no types in brackets".to_owned(),
            1 => "1 type in brackets".to_owned(),
            several => format!("{several} types in brackets"),
        };
        let help = if own == 0 {
            format!("remove the brackets: `{method}` has no type parameters of its own")
        } else {
            "write its type parameters' types in order, or leave them out to work them out".to_owned()
        };
        self.error(
            Diagnostic::new(format!("`{method}` takes {takes}, but this gives {}", types.len()), span)
                .with_help(help),
        );
        false
    }

    /// Reports why a value can't be used, still checking the arguments.
    pub(crate) fn unusable(&mut self, diagnostic: Diagnostic, args: &[Arg]) -> Type {
        self.error(diagnostic);
        self.ignore_args(args);
        Type::Unknown
    }

    /// `result.expect("reason")`: the value, or stop the program with the reason.
    fn expect_reason(&mut self, args: &[Arg], span: Span) {
        let types: Vec<Type> = args.iter().map(|arg| self.arg_value(arg, &[])).collect();
        if !matches!(types.as_slice(), [Type::Str | Type::Unknown]) {
            self.error(
                Diagnostic::new("`expect` needs a reason this can't fail", span)
                    .with_help("write it as a string, like `.expect(\"the file was just written\")`"),
            );
            return;
        }
        // A reason with nothing in it says nothing to whoever reads the program, or the crash.
        if let [Arg { value, .. }] = args
            && let ExprKind::Str(reason) = &value.kind
            && reason.trim().is_empty()
        {
            self.error(
                Diagnostic::new("say why this can't fail", value.span)
                    .with_help("the reason is what a reader trusts, and what the crash says if it's wrong, like `.expect(\"the file was just written\")`"),
            );
        }
    }

    /// `failure.notes()`, the notes an error carries, as Python's exceptions do, or
    /// `failure.add_note(text)`, which adds one and so needs the error in a mut name.
    fn note_method(&mut self, object: &Expr, method: &str, args: &[Arg], span: Span) -> Type {
        if method == "notes" {
            if !args.is_empty() {
                self.error(Diagnostic::new("`notes()` takes nothing", span));
                self.ignore_args(args);
            }
            return Type::list(Type::Str);
        }
        if let [Arg { name: None, value, .. }] = args {
            let given = self.expr_as(value, &Type::Str);
            if !self.checker.fits(&Type::Str, &given) {
                let message = format!("`add_note` takes the note's text, a str, but this is {}", a(&given));
                self.error(
                    Diagnostic::new(message, value.span)
                        .with_help("write it as text, like `f\"while {what}\"`"),
                );
            }
        } else {
            self.error(Diagnostic::new("`add_note` takes the note's text", span));
            self.ignore_args(args);
        }
        self.mut_place(object, "`add_note` changes its error, so call it on something held in a mut name");
        Type::Unit
    }

    /// Calls a method on a value of type `receiver`. A `mut self` method needs its value in
    /// a mut place.
    fn call_method(&mut self, call: &MethodCall<'_>, receiver: &Type, method_key: (&str, bool)) -> Type {
        let MethodCall { object, method, types, args, span } = *call;
        let (key, takes_self) = method_key;
        self.note_lock_call(key, method, span);
        self.locks_again(object, receiver, &format!("calling `{method}`"));
        self.method_on_holder(object, receiver, method);
        if let Some(owner) = receiver.type_name() {
            self.private_member(owner, method, object.span.to(span));
        }
        if !self.extension_fits(receiver, key, method, span) {
            self.ignore_args(args);
            return Type::Unknown;
        }
        let own =
            self.checker.program.functions.get(key).map_or(0, |entry| written_generics(&entry.def.generics));
        if !self.takes_types(method, own, types, span) {
            self.ignore_args(args);
            return Type::Unknown;
        }
        let Some(signature) = self.method_signature_with(receiver, key, types, span) else {
            self.ignore_args(args);
            return Type::Unknown;
        };
        let params =
            if takes_self { signature.params.get(1..).unwrap_or_default() } else { &signature.params };
        // A `shared` value's method takes its lock, so any name for the handle can call it.
        if takes_self
            && signature.params.first().is_some_and(|param| param.is_mut)
            && !self.checker.program.is_shared(receiver)
        {
            let what = format!("`{method}` changes its value, so call it on something held in a mut name");
            self.mut_place(object, &what);
        } else if takes_self
            && signature.params.first().is_some_and(|param| param.is_mut)
            && let ExprKind::Name(name) = &object.kind
            && self.origin(name) == Some(Origin::Locked)
            && self.lookup(name).is_some_and(|(_, is_mut)| !is_mut)
        {
            self.error(
                Diagnostic::new(
                    format!("`{name}` is locked to read, and `{method}` changes it"),
                    object.span,
                )
                .with_help(format!("lock it to write with `as ref {name}`")),
            );
        }
        self.calling.push(key.to_owned());
        self.bind_args(method, params, args, span);
        self.calling.pop();
        self.note_source(key, &signature.returns, takes_self, span);
        signature.returns
    }

    /// A global function, or `len(x)` or `abs(x)`, which aren't ones.
    fn call_global(&mut self, name: &str, callee: &Expr, args: &[Arg], span: Span) -> Type {
        if matches!(name, "len" | "abs") {
            self.method_as_function(name, callee, args, span)
        } else {
            self.call_builtin(name, args, span)
        }
    }

    /// `len(x)` or `abs(x)`, which Fennec doesn't have: these ask a value about itself, so
    /// they're its own methods, `x.len()` and `x.abs()`. The fix moves them there.
    fn method_as_function(&mut self, name: &str, callee: &Expr, args: &[Arg], span: Span) -> Type {
        let mut diagnostic = Diagnostic::new(format!("`{name}` is a method: write `x.{name}()`"), span).with_help(
            "the global functions are `sum`, `min`, `max`, `any`, `all`, `sorted`, `zip`, `enumerate` and `range`",
        );
        let mut value_type = Type::Unknown;
        if let [arg] = args
            && arg.name.is_none()
            && !arg.is_mut
        {
            value_type = self.expr(&arg.value);
            let value = arg.value.span;
            // A dict or set gets parentheses too, since `{{` in an f-string is a brace.
            let grouped = matches!(
                arg.value.kind,
                ExprKind::Dict(_)
                    | ExprKind::Set(_)
                    | ExprKind::Binary { .. }
                    | ExprKind::Unary { .. }
                    | ExprKind::Cond { .. }
                    | ExprKind::Else { .. }
                    | ExprKind::Lambda { .. }
            );
            let (open, close) = if grouped {
                ("(".to_owned(), format!(").{name}()"))
            } else {
                (String::new(), format!(".{name}()"))
            };
            diagnostic = diagnostic
                .with_fix(Span { start: callee.span.start, end: value.start }, open)
                .with_fix(Span { start: value.end, end: span.end }, close);
        } else {
            self.ignore_args(args);
        }
        self.error(diagnostic);
        if name == "len" { Type::Int } else { value_type }
    }

    /// Whether a method from a conditional extension, like `count` from
    /// `extend list[T: Eq]:`, exists for this receiver: its type's arguments have to fit the
    /// extension's bounds. Reports it when they don't.
    fn extension_fits(&mut self, receiver: &Type, key: &str, method: &str, span: Span) -> bool {
        let Some(entry) = self.checker.program.functions.get(key) else { return true };
        let wanted: Vec<(String, Vec<String>)> = entry
            .extension
            .iter()
            .map(|generic| {
                (generic.name.clone(), generic.bounds.iter().map(|bound| bound.name.clone()).collect())
            })
            .collect();
        for ((param, bounds), arg) in wanted.into_iter().zip(receiver.type_args()) {
            let arg = self.checker.settled(&arg);
            if !arg.is_known() {
                continue;
            }
            for bound in bounds {
                if let Err((_, fix)) = self.fits_bound(&arg, &bound) {
                    let message = format!(
                        "`{method}` needs `{param}` to fit `{bound}`, but here `{param}` is {}",
                        crate::generic::show(&arg)
                    );
                    self.error(Diagnostic::new(message, span).with_help(fix));
                    return false;
                }
            }
        }
        true
    }

    /// `Type.name(...)`: an enum case, or a named constructor.
    fn static_call(&mut self, type_name: &str, method: &str, args: &[Arg], span: Span) -> Type {
        let is_case = self
            .checker
            .program
            .type_info(type_name)
            .is_some_and(|info| info.def.cases.iter().any(|case| case.name == method));
        if is_case {
            return self.construct_variant(Some(type_name), method, args, span);
        }
        match self.checker.program.method(type_name, method) {
            Some((key, entry)) if !entry.def.takes_self() => self.call_function(&key, method, args, span),
            Some(_) => {
                let message = format!("`{method}` needs a `{type_name}` to call it on");
                self.unusable(Diagnostic::new(message, span), args)
            }
            None => {
                let message = format!("`{type_name}` has nothing called `{method}`");
                self.unusable(Diagnostic::new(message, span), args)
            }
        }
    }

    /// The types written in brackets after a generic function or type: `int` in
    /// `largest[int]`, `str, int` in `Pair[str, int]`.
    fn types_in(&mut self, index: &Expr) -> Vec<Type> {
        let written = match &index.kind {
            ExprKind::Tuple(items) => items.iter().map(Expr::as_type).collect::<Option<Vec<_>>>(),
            _ => index.as_type().map(|ty| vec![ty]),
        };
        let Some(written) = written else {
            self.error(Diagnostic::new("write types in the brackets, like `[int]`", index.span));
            return Vec::new();
        };
        let types: Vec<Type> = written.iter().map(|ty| self.resolve(ty)).collect();
        // For compilers, since a function's body may be the only thing that uses them.
        self.record(index.span, &Type::Tuple(types.clone()));
        types
    }

    /// Calls a type to build a value: `Enemy(position=p)`, `NotFound(path)`.
    pub(crate) fn construct(&mut self, type_name: &str, args: &[Arg], span: Span) -> Type {
        self.construct_with(type_name, (args, &[]), span)
    }

    /// Builds a value, with the types for the type's parameters that are written in
    /// brackets, as in `Deque[int]()`. The others are worked out from the fields given.
    pub(crate) fn construct_with(&mut self, type_name: &str, given: (&[Arg], &[Type]), span: Span) -> Type {
        let (args, types) = given;
        let Some(info) = self.checker.program.type_info(type_name) else {
            return self.construct_builtin(type_name, None, args, span);
        };
        if !info.def.cases.is_empty() {
            let cases: Vec<String> =
                info.def.cases.iter().map(|case| format!("{type_name}.{}", case.name)).collect();
            let what = if info.kind == TypeKind::Enum { "is an enum" } else { "has cases" };
            let diagnostic = Diagnostic::new(format!("`{type_name}` {what}, so pick one of its cases"), span)
                .with_help(format!("write one of {}", cases.join(", ")));
            return self.unusable(diagnostic, args);
        }
        let (fields, generics) = (info.def.fields.clone(), info.def.generics.clone());
        if types.len() > generics.len() {
            self.error(crate::messages::wrong_args(type_name, span));
        }
        let mut given = Vec::new();
        let mut chosen = Vec::new();
        for (at, generic) in generics.iter().enumerate() {
            let ty = match types.get(at) {
                Some(ty) => ty.clone(),
                None => self.checker.fresh(span, Hole::TypeParam(generic.name.clone())),
            };
            let bounds = generic.bounds.iter().map(|bound| bound.name.clone()).collect();
            chosen.push(Chosen { ty: ty.clone(), bounds });
            given.push((generic.name.clone(), ty));
        }
        let built = match given.as_slice() {
            [] => Type::named(type_name),
            given => Type::Generic(type_name.to_owned(), given.iter().map(|(_, ty)| ty.clone()).collect()),
        };
        self.private_fields_given(type_name, &fields, args);
        let literal = self.build(&Target { name: type_name, case: None, given }, &fields, args, span);
        self.check_bounds(chosen, span);
        crate::stdlib::construct(self, type_name, args);
        if fields.iter().any(|field| field.rule.is_some()) && !literal {
            return Type::result(built, Type::named("InvalidValue"));
        }
        built
    }

    /// `Table[Enemy]()` and `SoaList[Particle]()`.
    /// Whether an expression is `list`, `dict` or `set`, the built-in collections.
    /// `list[str]()` and the like, or `array[float, 16]()`, when the name is the builtin's.
    fn made_collection(
        &mut self,
        callee: &Expr,
        (object, index): (&Expr, &Expr),
        args: &[Arg],
        span: Span,
    ) -> Option<Type> {
        let ExprKind::Name(name) = &object.kind else { return None };
        if self.lookup(name).is_some() {
            return None;
        }
        match name.as_str() {
            "list" | "dict" | "set" => Some(self.empty_collection(callee, args, span)),
            "array" | "array.new" => Some(self.filled_array(callee, index, args, span)),
            _ => None,
        }
    }

    /// `list[str]()`, `dict[str, int]()` or `set[int]()`: an empty collection that says its type.
    /// It's written in as `[]`, `{}` or `set()`, with the type known.
    fn empty_collection(&mut self, callee: &Expr, args: &[Arg], span: Span) -> Type {
        let Some(written) = callee.as_type() else {
            self.error(Diagnostic::new("write the types it holds as names, like `list[str]()`", callee.span));
            return Type::Unknown;
        };
        if !args.is_empty() {
            let message =
                format!("`{}()` makes an empty one, so it takes nothing", crate::program::written(&written));
            self.error(
                Diagnostic::new(message, span)
                    .with_help("fill it in afterwards, or write the items in `[...]`"),
            );
            self.ignore_args(args);
            return Type::Unknown;
        }
        let ty = self.resolve(&written);
        if !ty.is_known() {
            return ty;
        }
        ty
    }

    /// `array[float, 16]()`: an array filled with its element's zero. It's written in as
    /// `array.new[float, 16](0.0)`, with the zero checked here like any other expression, and
    /// checked again in that form.
    fn filled_array(&mut self, callee: &Expr, index: &Expr, args: &[Arg], span: Span) -> Type {
        if let ExprKind::Index { object, .. } = &callee.kind
            && let ExprKind::Name(name) = &object.kind
            && name == "array.new"
        {
            let renamed = Expr { kind: ExprKind::Name("array".to_owned()), span: object.span };
            let written = Expr {
                kind: ExprKind::Index { object: Box::new(renamed), index: Box::new(index.clone()) },
                span: callee.span,
            };
            let ty = written.as_type().map_or(Type::Unknown, |written| self.resolve(&written));
            if let (Type::Array(item, _), [zero]) = (&ty, args) {
                let got = self.expr_as(&zero.value, item);
                if !self.checker.fits(item, &got) {
                    self.error(mismatch("this array's zero", item, &got, zero.value.span));
                }
            }
            return ty;
        }
        let Some(written) = callee.as_type() else {
            self.error(Diagnostic::new("write an array's type like `array[float, 16]()`", callee.span));
            self.ignore_args(args);
            return Type::Unknown;
        };
        if !args.is_empty() {
            let message =
                format!("`{}()` fills it with zeros, so it takes nothing", crate::program::written(&written));
            self.error(
                Diagnostic::new(message, span)
                    .with_help("set the items afterwards, or write them in `[...]` where an array goes"),
            );
            self.ignore_args(args);
            return Type::Unknown;
        }
        let ty = self.resolve(&written);
        let Type::Array(item, _) = &ty else { return ty };
        let item_written = match &index.kind {
            ExprKind::Tuple(parts) => parts.first().unwrap_or(index),
            _ => index,
        };
        let Some(zero) = self.zero_of(item, item_written, callee.span) else {
            let message = format!("{} has no zero to fill an array with", a(item));
            let help = match item.as_ref() {
                Type::Named(name)
                    if self
                        .checker
                        .program
                        .type_info(name)
                        .is_some_and(|info| info.kind == TypeKind::Struct && !info.def.shared) =>
                {
                    format!(
                        "give every field of `{name}` a default, or write the items out: `[first, second, ...]`"
                    )
                }
                _ => "write the items out where the array goes: `[first, second, ...]`".to_owned(),
            };
            self.error(Diagnostic::new(message, span).with_help(help));
            return Type::Unknown;
        };
        self.expr(&zero);
        self.checker.array_zeros.insert((span.start, span.end), zero);
        ty
    }

    /// What an array of `item` starts out holding: 0, 0.0, False, "", None, an array of zeros,
    /// or a struct made with all its defaults. It takes the span of the array's type, which is
    /// never checked as an expression, so its type has a place of its own.
    fn zero_of(&mut self, item: &Type, written: &Expr, at: Span) -> Option<Expr> {
        let kind = match item {
            Type::Int => ExprKind::Int(0),
            Type::Float => ExprKind::Float(0.0),
            Type::Bool => ExprKind::Bool(false),
            Type::Str => ExprKind::Str(String::new()),
            Type::Optional(_) => ExprKind::None,
            Type::Array(..) => ExprKind::Call { callee: Box::new(written.clone()), args: Vec::new() },
            Type::Named(name) => {
                let info = self.checker.program.type_info(name)?;
                let all_defaults = info.kind == TypeKind::Struct
                    && !info.def.shared
                    && info.def.fields.iter().all(|field| field.default.is_some());
                if !all_defaults {
                    return None;
                }
                ExprKind::Call { callee: Box::new(written.clone()), args: Vec::new() }
            }
            _ => return None,
        };
        Some(Expr { kind, span: at })
    }

    fn construct_builtin(&mut self, type_name: &str, item: Option<&Expr>, args: &[Arg], span: Span) -> Type {
        let has_item = item.is_some();
        let item = match item.map(|item| (item.as_type(), item.span)) {
            Some((Some(written), _)) => self.resolve(&written),
            Some((None, at)) => {
                let message =
                    format!("write the type `{type_name}` holds as a name, like `{type_name}[Enemy]`");
                self.error(Diagnostic::new(message, at));
                Type::Unknown
            }
            None => Type::Unknown,
        };
        let types: Vec<Type> = args.iter().map(|arg| self.arg_value(arg, &[])).collect();
        match (type_name, types.as_slice()) {
            ("Table", []) if has_item => Type::Table(Box::new(item)),
            ("SoaList", []) if has_item => Type::list(item),
            ("Buffer", []) if has_item => Type::Buffer(Box::new(item)),
            ("Buffer", [Type::Int])
                if has_item && args.first().is_some_and(|arg| arg.name.as_deref() == Some("capacity")) =>
            {
                Type::Buffer(Box::new(item))
            }
            _ => {
                self.error(crate::messages::wrong_args(type_name, span));
                Type::Unknown
            }
        }
    }

    /// Builds an enum case, like `.Circle(radius=1.0)` or `Shape.Circle(1.0)`. Without a
    /// type, the case's name has to belong to exactly one enum.
    pub(crate) fn construct_variant(
        &mut self,
        enum_name: Option<&str>,
        case: &str,
        args: &[Arg],
        span: Span,
    ) -> Type {
        let owners = match enum_name {
            Some(name) => vec![name.to_owned()],
            None => self.checker.program.enums_with_case(case),
        };
        let owner = match owners.as_slice() {
            [owner] => owner.clone(),
            [] => {
                return self
                    .unusable(Diagnostic::new(format!("no enum has a case called `{case}`"), span), args);
            }
            several => {
                let choices: Vec<String> = several.iter().map(|owner| format!("`{owner}.{case}`")).collect();
                let diagnostic =
                    Diagnostic::new(format!("more than one enum has a case called `{case}`"), span)
                        .with_help(format!("say which one: {}", choices.join(" or ")));
                return self.unusable(diagnostic, args);
            }
        };
        let fields = self
            .checker
            .program
            .type_info(&owner)
            .and_then(|info| info.def.cases.iter().find(|c| c.name == case))
            .map(|c| c.fields.clone())
            .unwrap_or_default();
        self.build(&Target { name: &owner, case: Some(case), given: Vec::new() }, &fields, args, span);
        Type::Named(owner)
    }

    /// Building a type from outside it when one of its `_` fields has no default, which only its
    /// own methods can set: it's built through one of its named constructors.
    fn built_elsewhere(&self, type_name: &str, field: &str, span: Span) -> Diagnostic {
        let message = format!(
            "`{type_name}` is built through its named constructors: only its own methods can set `{field}`"
        );
        let named: Vec<String> = self
            .checker
            .program
            .type_info(type_name)
            .map(|info| {
                crate::privacy::named_constructors(&info.def)
                    .iter()
                    .map(|name| format!("`{type_name}.{name}(...)`"))
                    .collect()
            })
            .unwrap_or_default();
        let help = if named.is_empty() {
            format!("`{field}` has no default, and `{type_name}` has no named constructor yet")
        } else {
            format!("use {}", named.join(" or "))
        };
        Diagnostic::new(message, span).with_help(help)
    }

    /// Matches arguments to fields, positionally and by name. Returns whether every argument
    /// is written as a literal, so `where` rules can be checked when the value is made.
    fn build(&mut self, target: &Target<'_>, fields: &[Field], args: &[Arg], span: Span) -> bool {
        let Target { name: type_name, case, .. } = *target;
        let display = case.map_or_else(|| type_name.to_owned(), |case| format!("{type_name}.{case}"));
        let mut given = vec![false; fields.len()];
        let mut unmatched_args = false;
        // From outside the type, its automatic constructor sets only its public fields, so
        // values given by position go to those.
        let outside = !self.inside(type_name);
        let settable: Vec<usize> = (0..fields.len())
            .filter(|&at| !(outside && fields.get(at).is_some_and(|field| field.name.starts_with('_'))))
            .collect();
        for (position, arg) in args.iter().enumerate() {
            let at = match &arg.name {
                Some(name) => fields.iter().position(|field| &field.name == name),
                None => settable.get(position).copied(),
            };
            // A value given by position that skips a `_` field is written in by its field's name,
            // so what runs the program puts it where the check did.
            if arg.name.is_none()
                && at != Some(position)
                && let Some(field) = at.and_then(|at| fields.get(at))
            {
                self.checker
                    .field_args
                    .insert((arg.value.span.start, arg.value.span.end), field.name.clone());
            }
            let expected = at
                .and_then(|at| fields.get(at))
                .map(|field| self.checker.field_type(type_name, case, &field.name).substitute(&target.given));
            let ty = self.arg_as(arg, &[], expected.as_ref());
            if arg.is_mut {
                let message = "building a value doesn't change its arguments, so don't pass `ref`";
                self.error(Diagnostic::new(message, arg.value.span));
            }
            let Some((field, slot)) = at.and_then(|at| fields.get(at).zip(given.get_mut(at))) else {
                let names: Vec<&str> = settable
                    .iter()
                    .filter_map(|&at| fields.get(at))
                    .map(|field| field.name.as_str())
                    .collect();
                self.error(unmatched(&display, ("field", "value"), &names, arg));
                unmatched_args = true;
                continue;
            };
            if *slot {
                self.error(Diagnostic::new("this field is given twice", arg.value.span));
            }
            *slot = true;
            let expected = expected.unwrap_or(Type::Unknown);
            if !self.checker.fits(&expected, &ty) {
                let expected = self.checker.settled(&expected);
                self.error(mismatch(&format!("`{}`", field.name), &expected, &ty, arg.value.span));
            }
        }
        for (field, given) in fields.iter().zip(given) {
            if !given && field.default.is_none() && !unmatched_args {
                let problem = if outside && field.name.starts_with('_') {
                    self.built_elsewhere(type_name, &field.name, span)
                } else {
                    Diagnostic::new(format!("`{display}` needs a value for `{}`", field.name), span)
                        .with_help(format!("pass it, like `{display}({}=...)`", field.name))
                };
                self.error(problem);
            }
        }
        args.iter().all(|arg| {
            matches!(
                arg.value.kind,
                ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Bool(_) | ExprKind::None
            )
        })
    }
}

/// An argument that matches no parameter or field. `nouns` names what's being matched and
/// what's being passed, like `("parameter", "argument")`.
/// How many type parameters can be written in brackets: the program's own, not the ones it has
/// for parameters typed by an interface.
fn written_generics(generics: &[Generic]) -> usize {
    generics.iter().filter(|generic| !generic.is_hidden()).count()
}

fn unmatched(owner: &str, nouns: (&str, &str), names: &[&str], arg: &Arg) -> Diagnostic {
    let (matched, passed) = nouns;
    let listed: Vec<String> = names.iter().map(|name| format!("`{name}`")).collect();
    let help = if listed.is_empty() {
        format!("`{owner}` takes no {passed}s")
    } else {
        format!("`{owner}` takes {}", listed.join(", "))
    };
    let message = match &arg.name {
        Some(name) => format!("`{owner}` has no {matched} called `{name}`"),
        None => format!("`{owner}` takes {} {passed}(s)", names.len()),
    };
    Diagnostic::new(message, arg.value.span).with_help(help)
}

/// A default as a literal, so a negative number needs no types to read.
fn plain_default(default: &Expr) -> Expr {
    let kind = match &default.kind {
        ExprKind::Unary { op: UnaryOp::Neg, expr } => match expr.kind {
            ExprKind::Int(value) => ExprKind::Int(value.wrapping_neg()),
            ExprKind::Float(value) => ExprKind::Float(-value),
            _ => default.kind.clone(),
        },
        other => other.clone(),
    };
    Expr { kind, span: default.span }
}

/// A parameter of something called by value, whose type doesn't name its parameters.
fn argument_param(at: usize, ty: Type) -> ParamType {
    ParamType { name: format!("argument {}", at + 1), ty, is_mut: false, default: None }
}

/// What a method called on a union is, as a `with` source.
enum UnionSource {
    /// Not a `with` source.
    NotOne,
    /// One written in once for each member.
    Dispatched(crate::union_sources::Dispatch),
    /// One that can't be, which is said.
    Refused,
}
