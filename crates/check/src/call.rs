//! Calls: functions, methods, named constructors and operators, and building structs,
//! errors and enum cases. Arguments are matched to parameters or fields, positionally and by
//! name, and `mut` has to be written exactly where a parameter changes.

use fennec_syntax::{Arg, Diagnostic, Expr, ExprKind, Field, Span};

use crate::ParamType;
use crate::builtins;
use crate::expr::Member;
use crate::function::FnCx;
use crate::generic::Chosen;
use crate::infer::Hole;
use crate::messages::{might_be_none, mismatch, no_method, unhandled_result};
use crate::program::TypeKind;
use crate::types::{Type, a};

/// A method call: `object.method(args)`.
#[derive(Clone, Copy)]
struct MethodCall<'a> {
    object: &'a Expr,
    method: &'a str,
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

const MUT_ARG: &str = "only a name, or a field or item of one, can be passed with `mut`";

impl FnCx<'_> {
    pub(crate) fn call(&mut self, callee: &Expr, args: &[Arg], span: Span) -> Type {
        match &callee.kind {
            ExprKind::Attr { object, name, .. }
                if name == "from_code"
                    && matches!(&object.kind, ExprKind::Name(type_name) if type_name == "str" && self.lookup(type_name).is_none()) =>
            {
                return self.char_from_code(args, span);
            }
            ExprKind::Attr { object, name, optional } => {
                return match self.module_member(object, name, callee.span) {
                    Some(Member::Name(member)) => {
                        self.call(&Expr { kind: ExprKind::Name(member), span: callee.span }, args, span)
                    }
                    Some(Member::Missing) => {
                        self.ignore_args(args);
                        Type::Unknown
                    }
                    None => self.method_call(object, name, *optional, args, span),
                };
            }
            ExprKind::Variant(name) => return self.construct_variant(None, name, args, span),
            ExprKind::Index { object, index } => {
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
                if let Some(builtin) = self.loads(object) {
                    return self.typed_loads(&builtin, index, args, span);
                }
            }
            ExprKind::Name(name) if self.lookup(name).is_none() => {
                if self.checker.program.functions.contains_key(name) {
                    self.record(callee.span, &Type::Function(name.clone()));
                    return self.call_function(name, name, args, span);
                }
                if self.checker.program.is_type_name(name) {
                    return self.construct(name, args, span);
                }
                if let Some(builtin) = self.checker.program.imported(name, callee.span).cloned() {
                    return self.call_builtin(&builtin, args, span);
                }
                if matches!(name.as_str(), "len" | "abs") || builtins::is_builtin(name) {
                    return self.call_global(name, callee, args, span);
                }
            }
            _ => {}
        }
        match self.expr(callee) {
            Type::Function(key) => {
                let name = key.rsplit('.').next().unwrap_or_default().to_owned();
                self.call_function(&key, &name, args, span)
            }
            Type::Arrow(params, returns) => {
                let params: Vec<ParamType> = params
                    .into_iter()
                    .enumerate()
                    .map(|(at, ty)| ParamType { name: format!("argument {}", at + 1), ty, is_mut: false })
                    .collect();
                self.bind_args("this function", &params, args, span);
                *returns
            }
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
            (ExprKind::Lambda { params: names, body }, _) => self.lambda(names, body, params, arg.value.span),
            (_, Some(expected)) => self.expr_as(&arg.value, expected),
            (_, None) => self.expr(&arg.value),
        }
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
        let (args, types) = given;
        let Some(signature) = self.checker.signature(key) else {
            self.ignore_args(args);
            return Type::Unknown;
        };
        let (signature, chosen) = self.instantiate(key, &signature, None, types, span);
        self.bind_args(name, &signature.params, args, span);
        self.check_bounds(chosen, span);
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
            // An arrow function passed for a function parameter receives its parameters' types.
            let receives = match found.map(|(_, param)| self.checker.settled(&param.ty)) {
                Some(Type::Arrow(receives, _)) => receives,
                _ => Vec::new(),
            };
            let ty = self.arg_as(arg, &receives, found.map(|(_, param)| &param.ty));
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
            Type::Named(name) | Type::Generic(name, _) if self.checker.program.type_info(name).is_some() => {
                match self.checker.program.method(name, method) {
                    Some((key, entry)) => {
                        let takes_self = entry.def.takes_self();
                        self.call_method(call, ty, (&key, takes_self))
                    }
                    // Methods written in Rust, like `Regex`'s.
                    None => self.builtin_method(object, ty, method, args, span),
                }
            }
            Type::Param(name) => self.interface_call(call, name),
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

    /// The method the standard library gives a built-in type, if it has one by that name,
    /// and whether it takes `self`.
    fn std_method(&self, ty: &Type, method: &str) -> Option<(String, bool)> {
        let owner = ty.method_owner()?;
        self.checker.program.method(owner, method).map(|(key, entry)| (key, entry.def.takes_self()))
    }

    /// A method called on a value of a type parameter, which has the methods of its
    /// interfaces and no others.
    fn interface_call(&mut self, call: &MethodCall<'_>, name: &str) -> Type {
        let MethodCall { object, method, args, span } = *call;
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
        let signature = self.interface_method(&def, &owner);
        if signature.params.first().is_some_and(|param| param.is_mut) {
            let what = format!("`{method}` changes its value, so call it on something held in a mut name");
            self.mut_place(object, &what);
        }
        self.bind_args(method, signature.params.get(1..).unwrap_or_default(), args, span);
        signature.returns
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

    /// Calls a method on a value of type `receiver`. A `mut self` method needs its value in
    /// a mut place.
    fn call_method(&mut self, call: &MethodCall<'_>, receiver: &Type, method_key: (&str, bool)) -> Type {
        let MethodCall { object, method, args, span } = *call;
        let (key, takes_self) = method_key;
        if !self.extension_fits(receiver, key, method, span) {
            self.ignore_args(args);
            return Type::Unknown;
        }
        let Some(signature) = self.method_signature(receiver, key) else {
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
        written.iter().map(|ty| self.resolve(ty)).collect()
    }

    /// Calls a type to build a value: `Enemy(position=p)`, `NotFound(path)`.
    pub(crate) fn construct(&mut self, type_name: &str, args: &[Arg], span: Span) -> Type {
        self.construct_with(type_name, (args, &[]), span)
    }

    /// Builds a value, with the types for the type's parameters that are written in
    /// brackets, as in `Deque[int]()`. The others are worked out from the fields given.
    fn construct_with(&mut self, type_name: &str, given: (&[Arg], &[Type]), span: Span) -> Type {
        let (args, types) = given;
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
        let literal = self.build(&Target { name: type_name, case: None, given }, &fields, args, span);
        self.check_bounds(chosen, span);
        crate::stdlib::construct(self, type_name, args);
        if fields.iter().any(|field| field.rule.is_some()) && !literal {
            return Type::result(built, Type::named("InvalidValue"));
        }
        built
    }

    /// `IdMap[Enemy]()` and `SoaList[Particle]()`.
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
            ("IdMap", []) if has_item => Type::IdMap(Box::new(item)),
            ("SoaList", []) if has_item => Type::list(item),
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

    /// Matches arguments to fields, positionally and by name. Returns whether every argument
    /// is written as a literal, so `where` rules can be checked when the value is made.
    fn build(&mut self, target: &Target<'_>, fields: &[Field], args: &[Arg], span: Span) -> bool {
        let Target { name: type_name, case, .. } = *target;
        let display = case.map_or_else(|| type_name.to_owned(), |case| format!("{type_name}.{case}"));
        let mut given = vec![false; fields.len()];
        let mut unmatched_args = false;
        for (position, arg) in args.iter().enumerate() {
            let at = match &arg.name {
                Some(name) => fields.iter().position(|field| &field.name == name),
                None => Some(position),
            };
            let expected = at
                .and_then(|at| fields.get(at))
                .map(|field| self.checker.field_type(type_name, case, &field.name).substitute(&target.given));
            let ty = self.arg_as(arg, &[], expected.as_ref());
            if arg.is_mut {
                let message = "building a value doesn't change its arguments, so don't pass `mut`";
                self.error(Diagnostic::new(message, arg.value.span));
            }
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
            let expected = expected.unwrap_or(Type::Unknown);
            if !self.checker.fits(&expected, &ty) {
                let expected = self.checker.settled(&expected);
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
