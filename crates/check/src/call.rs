//! Calls: functions, methods, named constructors and operators, and building structs,
//! errors and enum cases. Arguments are matched to parameters or fields, positionally and by
//! name, and `mut` has to be written exactly where a parameter changes.

use fennec_syntax::{Arg, Diagnostic, Expr, ExprKind, Field, Span, TypeExpr};

use crate::ParamType;
use crate::builtins;
use crate::function::FnCx;
use crate::messages::{might_be_none, mismatch, no_method, unhandled_result};
use crate::program::TypeKind;
use crate::types::{Type, accepts};

/// A method call: `object.method(args)`.
#[derive(Clone, Copy)]
struct MethodCall<'a> {
    object: &'a Expr,
    method: &'a str,
    args: &'a [Arg],
    span: Span,
}

const MUT_ARG: &str = "only a name, or a field or item of one, can be passed with `mut`";

impl FnCx<'_> {
    pub(crate) fn call(&mut self, callee: &Expr, args: &[Arg], span: Span) -> Type {
        match &callee.kind {
            ExprKind::Attr { object, name, optional } => {
                return self.method_call(object, name, *optional, args, span);
            }
            ExprKind::Variant(name) => return self.construct_variant(None, name, args, span),
            ExprKind::Index { object, index } => {
                if let Some(type_name) = self.as_type_name(object) {
                    return self.construct_builtin(type_name, Some(index), args, span);
                }
            }
            ExprKind::Name(name) if self.lookup(name).is_none() => {
                if self.checker.program.functions.contains_key(name) {
                    return self.call_function(name, name, args, span);
                }
                if self.checker.program.is_type_name(name) {
                    return self.construct(name, args, span);
                }
                if let Some(builtin) = self.checker.program.imports.get(name).cloned() {
                    return self.call_builtin(&builtin, args, span);
                }
                if builtins::is_builtin(name) {
                    return self.call_builtin(name, args, span);
                }
            }
            _ => {}
        }
        if let Type::Function(key) = self.expr(callee) {
            let name = key.rsplit('.').next().unwrap_or_default().to_owned();
            return self.call_function(&key, &name, args, span);
        }
        self.ignore_args(args);
        Type::Unknown
    }

    /// An argument's type. An arrow function gets its parameters' types from `params`.
    pub(crate) fn arg_value(&mut self, arg: &Arg, params: &[Type]) -> Type {
        if arg.is_mut {
            self.mut_place(&arg.value, MUT_ARG);
        }
        match &arg.value.kind {
            ExprKind::Lambda { params: names, body } => self.lambda(names, body, params),
            _ => self.expr(&arg.value),
        }
    }

    /// Checks arguments to something that takes anything, like an arrow function.
    pub(crate) fn ignore_args(&mut self, args: &[Arg]) {
        for arg in args {
            self.arg_value(arg, &[]);
        }
    }

    pub(crate) fn call_function(&mut self, key: &str, name: &str, args: &[Arg], span: Span) -> Type {
        let Some(signature) = self.checker.signature(key) else {
            self.ignore_args(args);
            return Type::Unknown;
        };
        self.bind_args(name, &signature.params, args, span);
        signature.returns
    }

    /// Matches arguments to parameters, positionally and by name.
    fn bind_args(&mut self, function: &str, params: &[ParamType], args: &[Arg], span: Span) {
        let mut given = vec![false; params.len()];
        let mut unmatched_args = false;
        let mut next = 0;
        for arg in args {
            let ty = self.arg_value(arg, &[]);
            let at = if let Some(name) = &arg.name {
                params.iter().position(|param| &param.name == name)
            } else {
                next += 1;
                Some(next - 1)
            };
            let Some((at, param)) = at.and_then(|at| params.get(at).map(|param| (at, param))) else {
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
            self.check_mut_arg(function, param, arg);
            self.check_arg(param, &ty, arg.value.span);
        }
        // An argument with a misspelled name is most likely the missing one.
        for (param, given) in params.iter().zip(given) {
            if !given && !unmatched_args {
                let message = format!("`{function}` is missing its `{}` argument", param.name);
                self.error(Diagnostic::new(message, span));
            }
        }
    }

    fn check_mut_arg(&mut self, function: &str, param: &ParamType, arg: &Arg) {
        let span = arg.value.span;
        match (param.is_mut, arg.is_mut) {
            (true, false) => self.error(
                Diagnostic::new(
                    format!("`{function}` changes `{}`, so it has to be passed with `mut`", param.name),
                    span,
                )
                .with_help("write `mut` before the argument, so readers can see it changes"),
            ),
            (false, true) => self.error(Diagnostic::new(
                format!("`{function}` doesn't change `{}`, so it shouldn't be passed with `mut`", param.name),
                span,
            )),
            _ => {}
        }
    }

    /// Checks an argument's type fits its parameter.
    pub(crate) fn check_arg(&mut self, param: &ParamType, actual: &Type, span: Span) {
        if !accepts(&param.ty, actual) {
            self.error(mismatch(&format!("`{}`", param.name), &param.ty, actual, span));
        }
    }

    /// An operator defined by a type, like `def +` or `def []`, called with one argument.
    pub(crate) fn call_operator(&mut self, key: &str, arg: &Type, span: Span) -> Type {
        let Some(signature) = self.checker.signature(key) else { return Type::Unknown };
        if let Some(param) = signature.params.get(1) {
            self.check_arg(param, arg, span);
        }
        signature.returns
    }

    fn method_call(&mut self, object: &Expr, method: &str, optional: bool, args: &[Arg], span: Span) -> Type {
        if let Some(type_name) = self.as_type_name(object) {
            return self.static_call(type_name, method, args, span);
        }
        let ty = self.expr(object);
        self.method_on(&MethodCall { object, method, args, span }, &ty, optional)
    }

    fn method_on(&mut self, call: &MethodCall<'_>, ty: &Type, optional: bool) -> Type {
        let MethodCall { object, method, args, span } = *call;
        match ty {
            Type::None if optional => {
                self.ignore_args(args);
                Type::None
            }
            Type::Optional(inner) if optional => Type::optional(self.method_on(call, inner, false)),
            ty if ty.maybe_none() => self.unusable(might_be_none(object.span), args),
            Type::Result(value, _) if method == "expect" => {
                self.expect_reason(args, span);
                value.as_ref().clone()
            }
            Type::Result(..) => self.unusable(unhandled_result(object.span), args),
            Type::Named(name) if self.checker.program.type_info(name).is_some() => {
                match self.checker.program.method(name, method) {
                    Some((key, entry)) => {
                        let takes_self = entry.def.takes_self();
                        self.call_method(call, &key, takes_self)
                    }
                    None => self.unusable(no_method(ty, method, span), args),
                }
            }
            Type::Unknown => {
                self.ignore_args(args);
                Type::Unknown
            }
            _ => self.builtin_method(object, ty, method, args, span),
        }
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
        }
    }

    /// Calls a method on a value. A `mut self` method needs its value in a mut place.
    fn call_method(&mut self, call: &MethodCall<'_>, key: &str, takes_self: bool) -> Type {
        let MethodCall { object, method, args, span } = *call;
        let Some(signature) = self.checker.signature(key) else {
            self.ignore_args(args);
            return Type::Unknown;
        };
        let params =
            if takes_self { signature.params.get(1..).unwrap_or_default() } else { &signature.params };
        if takes_self && signature.params.first().is_some_and(|param| param.is_mut) {
            let what = format!("`{method}` changes its value, so call it on something held in a mut name");
            self.mut_place(object, &what);
        }
        self.bind_args(method, params, args, span);
        signature.returns
    }

    /// `Type.name(...)`: an enum case, or a named constructor.
    fn static_call(&mut self, type_name: &str, method: &str, args: &[Arg], span: Span) -> Type {
        let is_case = self.checker.program.type_info(type_name).is_some_and(|info| {
            info.kind == TypeKind::Enum && info.def.cases.iter().any(|case| case.name == method)
        });
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

    /// Calls a type to build a value: `Enemy(position=p)`, `NotFound(path)`.
    pub(crate) fn construct(&mut self, type_name: &str, args: &[Arg], span: Span) -> Type {
        let Some(info) = self.checker.program.type_info(type_name) else {
            return self.construct_builtin(type_name, None, args, span);
        };
        if info.kind == TypeKind::Enum {
            let cases: Vec<String> =
                info.def.cases.iter().map(|case| format!("{type_name}.{}", case.name)).collect();
            let diagnostic =
                Diagnostic::new(format!("`{type_name}` is an enum, so pick one of its cases"), span)
                    .with_help(format!("write one of {}", cases.join(", ")));
            return self.unusable(diagnostic, args);
        }
        let fields = info.def.fields.clone();
        let literal = self.build(type_name, None, &fields, args, span);
        let built = Type::named(type_name);
        if fields.iter().any(|field| field.rule.is_some()) && !literal {
            return Type::result(built, Type::named("InvalidValue"));
        }
        built
    }

    /// `IdMap[Enemy]()` and `SoaList[Particle]()`.
    fn construct_builtin(&mut self, type_name: &str, item: Option<&Expr>, args: &[Arg], span: Span) -> Type {
        let item = match item.map(|item| &item.kind) {
            Some(ExprKind::Name(name)) => self.checker.program.resolve(&TypeExpr {
                name: name.clone(),
                args: Vec::new(),
                optional: false,
                span,
            }),
            _ => Type::Unknown,
        };
        let types: Vec<Type> = args.iter().map(|arg| self.arg_value(arg, &[])).collect();
        match (type_name, types.as_slice()) {
            ("IdMap", []) => Type::IdMap(Box::new(item)),
            ("SoaList", []) => Type::list(item),
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
        self.build(&owner, Some(case), &fields, args, span);
        Type::Named(owner)
    }

    /// Matches arguments to fields, positionally and by name. Returns whether every argument
    /// is written as a literal, so `where` rules can be checked when the value is made.
    fn build(
        &mut self,
        type_name: &str,
        case: Option<&str>,
        fields: &[Field],
        args: &[Arg],
        span: Span,
    ) -> bool {
        let display = case.map_or_else(|| type_name.to_owned(), |case| format!("{type_name}.{case}"));
        let mut given = vec![false; fields.len()];
        let mut unmatched_args = false;
        for (position, arg) in args.iter().enumerate() {
            let ty = self.arg_value(arg, &[]);
            if arg.is_mut {
                let message = "building a value doesn't change its arguments, so don't pass `mut`";
                self.error(Diagnostic::new(message, arg.value.span));
            }
            let at = match &arg.name {
                Some(name) => fields.iter().position(|field| &field.name == name),
                None => Some(position),
            };
            let Some((field, slot)) = at.and_then(|at| fields.get(at).zip(given.get_mut(at))) else {
                let names: Vec<&str> = fields.iter().map(|field| field.name.as_str()).collect();
                self.error(unmatched(&display, ("field", "value"), &names, arg));
                unmatched_args = true;
                continue;
            };
            if *slot {
                self.error(Diagnostic::new("this field is given twice", arg.value.span));
            }
            *slot = true;
            let expected = self.checker.field_type(type_name, case, &field.name);
            if !accepts(&expected, &ty) {
                self.error(mismatch(&format!("`{}`", field.name), &expected, &ty, arg.value.span));
            }
        }
        for (field, given) in fields.iter().zip(given) {
            if !given && field.default.is_none() && !unmatched_args {
                self.error(
                    Diagnostic::new(format!("`{display}` needs a value for `{}`", field.name), span)
                        .with_help(format!("pass it, like `{display}({}=...)`", field.name)),
                );
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
