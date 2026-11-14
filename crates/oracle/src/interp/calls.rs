//! Calls: working out what's called and with what, then calling it. The two are apart so an
//! `except` handles only the failure of the call it follows, never one passed up by a `?` in its
//! arguments.

use fennec_check::Type;
use fennec_syntax::{Arg, CompKind, Expr, ExprKind, Field, FnDef, Span};

use super::{Binding, Flow, Interp, Place, Run, crash, unsupported};
use crate::load::Kind;
use crate::value::{Func, Value};

/// An argument as it's passed: a value, the place a `ref` argument names, or a generator written
/// straight into a builtin's call, which reads it there.
#[derive(Debug)]
pub(crate) enum ArgVal {
    Value(Option<String>, Value),
    Ref(Option<String>, Place),
    Lazy(Option<String>, &'static Expr),
}

impl ArgVal {
    pub(crate) fn name(&self) -> Option<&str> {
        match self {
            ArgVal::Value(name, _) | ArgVal::Ref(name, _) | ArgVal::Lazy(name, _) => name.as_deref(),
        }
    }
}

/// What a method is called on: a place it may change, or a value.
#[derive(Debug)]
pub(crate) enum Receiver {
    Place(Place),
    Value(Value),
}

/// What a call calls.
pub(crate) enum Target {
    Def {
        def: &'static FnDef,
        parent: Option<u64>,
        receiver: Option<Receiver>,
    },
    Lambda {
        params: Vec<String>,
        body: &'static Expr,
        captured: Vec<(String, Value)>,
    },
    Construct {
        ty: String,
        case: Option<String>,
    },
    Builtin(String),
    /// A built-in method, of a `Buffer` when `buffer` says, which is held as a list.
    Method {
        receiver: Receiver,
        name: String,
        buffer: bool,
    },
    /// A function of a built-in type, as `int.parse`.
    Static {
        ty: String,
        name: String,
    },
    /// A value already worked out.
    Given(Value),
}

/// The built-in types' names, as `int.parse` names one.
const BUILT_IN_TYPES: &[&str] = &[
    "int", "float", "str", "bool", "list", "dict", "set", "i8", "i16", "i32", "i64", "u8", "u16", "u32",
    "u64", "f32", "f64",
];

/// The builtin functions the oracle models, and the ones it knows it doesn't.
const BUILTINS: &[&str] = &[
    "len",
    "abs",
    "str",
    "int",
    "float",
    "range",
    "enumerate",
    "zip",
    "sorted",
    "reversed",
    "min",
    "max",
    "sum",
    "any",
    "all",
    "list",
    "set",
    "copy",
    "round",
    "i8",
    "i16",
    "i32",
    "i64",
    "u8",
    "u16",
    "u32",
    "u64",
    "f32",
    "f64",
    "parallel",
    "bytes",
    "Ok",
    "Err",
    "replace",
    "embed_file",
    "embed_folder",
    "failure_of",
];

impl Interp {
    pub(crate) fn call(
        &mut self,
        expr: &'static Expr,
        callee: &'static Expr,
        args: &'static [Arg],
    ) -> Run<Value> {
        if let Some(counted) = self.length_in_place(callee, args, expr.span)? {
            return Ok(counted);
        }
        let (target, given) = self.prepare(expr, callee, args)?;
        self.invoke(target, given, expr)
    }

    /// Works out what a call calls and its arguments, left to right, without calling it.
    pub(crate) fn prepare(
        &mut self,
        expr: &'static Expr,
        callee: &'static Expr,
        args: &'static [Arg],
    ) -> Run<(Target, Vec<ArgVal>)> {
        let target = match &callee.kind {
            ExprKind::Name(name) => self.named_target(name, callee.span)?,
            ExprKind::Attr { object, name, optional: false } => {
                match self.attr_target(object, name, args, expr)? {
                    Some(target) => target,
                    None => return Ok((Target::Given(Value::None), Vec::new())),
                }
            }
            ExprKind::Variant(case) => {
                let ty = self.enum_named_at(case, &[expr.span, callee.span])?;
                Target::Construct { ty, case: Some(case.clone()) }
            }
            ExprKind::Index { object, .. } => match &object.kind {
                ExprKind::Attr { object: owner, name, optional: false } => {
                    match self.attr_target(owner, name, args, expr)? {
                        Some(target) => target,
                        None => return Ok((Target::Given(Value::None), Vec::new())),
                    }
                }
                ExprKind::Name(name)
                    if self.program.functions.contains_key(name.as_str()) && !self.is_bound(name) =>
                {
                    self.named_target(name, callee.span)?
                }
                // A `Buffer` is held as the list it prints as. What it has room for isn't modeled.
                ExprKind::Name(name)
                    if name == "Buffer"
                        && args.iter().all(|arg| arg.name.as_deref() == Some("capacity"))
                        && !self.program.types.contains_key("Buffer") =>
                {
                    Target::Given(Value::List(Vec::new()))
                }
                ExprKind::Name(name)
                    if matches!(name.as_str(), "list" | "dict" | "set") && args.is_empty() =>
                {
                    Target::Given(match name.as_str() {
                        "list" => Value::List(Vec::new()),
                        "dict" => Value::Dict(Vec::new()),
                        _ => Value::Set(Vec::new()),
                    })
                }
                ExprKind::Name(name) if self.program.types.contains_key(&*self.type_named(name)) => {
                    Target::Construct { ty: self.type_named(name).into_owned(), case: None }
                }
                ExprKind::Name(name) => match self.program.imported.get(name.as_str()) {
                    Some(from) => return unsupported(from.clone()),
                    None => return unsupported(format!("`{name}[...]()`")),
                },
                _ => return unsupported("calling something in brackets"),
            },
            _ => {
                let value = self.eval(callee)?;
                self.func_target(value)?
            }
        };
        let lazy = matches!(target, Target::Builtin(_));
        let mut given = Vec::with_capacity(args.len());
        for arg in args {
            given.push(self.arg_value(arg, lazy)?);
        }
        Ok((target, given))
    }

    /// `len(place)`, counted where the value is rather than on a copy of it.
    fn length_in_place(
        &mut self,
        callee: &'static Expr,
        args: &'static [Arg],
        span: Span,
    ) -> Run<Option<Value>> {
        let ExprKind::Name(name) = &callee.kind else { return Ok(None) };
        let [arg] = args else { return Ok(None) };
        if name != "len" || arg.is_mut || arg.name.is_some() || self.is_bound(name) {
            return Ok(None);
        }
        let Some(place) = self.place_of(&arg.value)? else { return Ok(None) };
        let counted = self.peek_place(&place, span, |value| match value {
            Value::Str(text) => Some(text.chars().count()),
            Value::List(items) | Value::Tuple(items) | Value::Set(items) => Some(items.len()),
            Value::Dict(pairs) => Some(pairs.len()),
            _ => None,
        })?;
        if let Some(count) = counted {
            return Ok(Some(Value::Int(super::as_int(count))));
        }
        let value = self.read_place(&place, span)?;
        self.length(value, span).map(|count| Some(Value::Int(count)))
    }

    pub(crate) fn arg_value(&mut self, arg: &'static Arg, lazy: bool) -> Run<ArgVal> {
        if arg.is_mut {
            let Some(place) = self.place_of(&arg.value)? else {
                return unsupported("a `ref` argument that isn't a place");
            };
            return Ok(ArgVal::Ref(arg.name.clone(), place));
        }
        if lazy && matches!(arg.value.kind, ExprKind::Comprehension { kind: CompKind::Generator, .. }) {
            return Ok(ArgVal::Lazy(arg.name.clone(), &arg.value));
        }
        Ok(ArgVal::Value(arg.name.clone(), self.eval(&arg.value)?))
    }

    fn named_target(&mut self, name: &str, span: Span) -> Run<Target> {
        if let Some(value) = self.lookup(name, span)? {
            return self.func_target(value);
        }
        let name = &*self.type_named(name);
        if let Some(def) = self.program.functions.get(name) {
            return Ok(Target::Def { def, parent: None, receiver: None });
        }
        if let Some(declared) = self.program.types.get(name) {
            if declared.kind == Kind::Enum || !declared.def.cases.is_empty() {
                return unsupported(format!("calling the enum `{name}`"));
            }
            return Ok(Target::Construct { ty: name.to_owned(), case: None });
        }
        if BUILTINS.contains(&name) {
            return Ok(Target::Builtin(name.to_owned()));
        }
        if let Some(from) = self.program.imported.get(name) {
            if super::library::has_function(from) {
                return Ok(Target::Builtin(from.clone()));
            }
            return unsupported(from.clone());
        }
        unsupported(format!("calling `{name}`"))
    }

    pub(crate) fn func_target(&self, value: Value) -> Run<Target> {
        match value {
            Value::Func(func) => match *func {
                Func::Named(name) => match self.program.functions.get(name.as_str()) {
                    Some(def) => Ok(Target::Def { def, parent: None, receiver: None }),
                    None => unsupported(format!("calling `{name}`")),
                },
                Func::Builtin(name) => Ok(Target::Builtin(name)),
                Func::Nested { def, frame } => Ok(Target::Def { def, parent: Some(frame), receiver: None }),
                Func::Lambda { params, body, captured } => Ok(Target::Lambda { params, body, captured }),
            },
            other => unsupported(format!("calling a {}", other.kind())),
        }
    }

    /// `object.name(...)`: a method, a type's own function or case, or `None` when `object` is a
    /// missing value in a chain an `else` ends, so nothing's called.
    fn attr_target(
        &mut self,
        object: &'static Expr,
        name: &str,
        args: &'static [Arg],
        expr: &'static Expr,
    ) -> Run<Option<Target>> {
        if let ExprKind::Name(owner) = &object.kind
            && !self.is_bound(owner)
            && !self.program.constants.contains_key(owner.as_str())
        {
            let owner = &self.type_named(owner).into_owned();
            if let Some(declared) = self.program.types.get(owner.as_str()) {
                if declared.def.cases.iter().any(|case| case.name == name) {
                    return Ok(Some(Target::Construct { ty: owner.clone(), case: Some(name.to_owned()) }));
                }
                return match self.program.method(owner, name) {
                    Some(def) if !def.takes_self() => {
                        Ok(Some(Target::Def { def, parent: None, receiver: None }))
                    }
                    _ => unsupported(format!("`{owner}.{name}`")),
                };
            }
            if BUILT_IN_TYPES.contains(&owner.as_str()) {
                return Ok(Some(Target::Static { ty: owner.clone(), name: name.to_owned() }));
            }
            if let Some(from) = self.program.imported.get(owner.as_str()) {
                let path = format!("{from}.{name}");
                if super::library::has_function(&path) {
                    return Ok(Some(Target::Builtin(path)));
                }
                return unsupported(path);
            }
            return unsupported(format!("`{owner}.{name}`"));
        }
        if name == "expect" {
            return self.expected(object, args, expr).map(Some);
        }
        let receiver = match self.place_of(object)? {
            Some(place) => Receiver::Place(place),
            None => Receiver::Value(self.eval(object)?),
        };
        self.method_target(receiver, name, object.span)
    }

    /// The method `name` of what `receiver` holds: one the program wrote, or a built-in one, or
    /// `None` when it holds a missing value in a chain an `else` ends.
    pub(crate) fn method_target(
        &mut self,
        receiver: Receiver,
        name: &str,
        span: Span,
    ) -> Run<Option<Target>> {
        let object_span = span;
        let owner = match &receiver {
            Receiver::Place(place) => self.type_name_at(place, object_span)?,
            Receiver::Value(value) => type_name_of(value),
        };
        if owner.as_deref() == Some("None") {
            return Ok(None);
        }
        if let Some(owner) = owner
            && let Some(def) = self.program.method(&owner, name)
        {
            if !def.takes_self() {
                return unsupported("calling a type's function on a value");
            }
            return Ok(Some(Target::Def { def, parent: None, receiver: Some(receiver) }));
        }
        let buffer = matches!(self.program.type_at(object_span), Some(Type::Buffer(_)));
        Ok(Some(Target::Method { receiver, name: name.to_owned(), buffer }))
    }

    /// `call().expect("reason")`: the call's value, or a stop with the reason if it fails.
    fn expected(&mut self, object: &'static Expr, args: &'static [Arg], expr: &'static Expr) -> Run<Target> {
        let ran = match &object.kind {
            ExprKind::Call { callee, args } => {
                let (target, given) = self.prepare(object, callee, args)?;
                self.invoke(target, given, object)
            }
            _ => self.eval(object),
        };
        match ran {
            Ok(Value::Instance(instance)) if self.program.method(&instance.ty, "expect").is_some() => {
                unsupported("a type's own `expect`")
            }
            Ok(value) => Ok(Target::Given(value)),
            Err(Flow::Fail(error, _)) => {
                let reason = match args.first().map(|arg| &arg.value.kind) {
                    Some(ExprKind::Str(reason)) => reason.clone(),
                    _ => return unsupported("`expect` with a reason that isn't written out"),
                };
                let shown = self.repr(&error)?;
                crash(format!("{reason}: {shown}"), expr.span)
            }
            Err(other) => Err(other),
        }
    }

    /// The name of the type of what's at a place, for finding its methods.
    fn type_name_at(&self, place: &Place, span: Span) -> Run<Option<String>> {
        self.peek_place(place, span, type_name_of)
    }

    /// The enum or error a leading-dot case like `.Circle` is of, from the type the checker gave
    /// where it's written.
    pub(crate) fn enum_named_at(&self, case: &str, spans: &[Span]) -> Run<String> {
        for span in spans {
            if let Some(ty) = self.program.type_at(*span) {
                let mut names = Vec::new();
                type_names(ty, &mut names);
                for name in names {
                    if let Some(declared) = self.program.types.get(name.as_str())
                        && declared.def.cases.iter().any(|known| known.name == case)
                    {
                        return Ok(name);
                    }
                }
            }
        }
        unsupported(format!("the case `.{case}` of a type the oracle doesn't know"))
    }

    /// Calls what `prepare` worked out.
    pub(crate) fn invoke(&mut self, target: Target, given: Vec<ArgVal>, expr: &'static Expr) -> Run<Value> {
        match target {
            Target::Def { def, parent, receiver } => self.call_def(def, parent, receiver, given, expr.span),
            Target::Lambda { params, body, captured } => self.call_lambda(&params, body, captured, given),
            Target::Construct { ty, case } => self.construct(&ty, case.as_deref(), given, expr.span),
            Target::Builtin(name) => self.builtin(&name, given, expr),
            Target::Method { receiver, name, buffer: false } => {
                self.method(receiver, &name, given, expr.span)
            }
            Target::Method { receiver, name, buffer: true } => {
                self.buffer_method(receiver, &name, given, expr.span)
            }
            Target::Static { ty, name } => self.static_call(&ty, &name, given, expr.span),
            Target::Given(value) => Ok(value),
        }
    }

    /// Calls a value that's a function, with plain values.
    pub(crate) fn call_value(&mut self, func: &Value, values: Vec<Value>, expr: &'static Expr) -> Run<Value> {
        let target = self.func_target(func.clone())?;
        let given = values.into_iter().map(|value| ArgVal::Value(None, value)).collect();
        self.invoke(target, given, expr)
    }

    fn call_lambda(
        &mut self,
        params: &[String],
        body: &'static Expr,
        captured: Vec<(String, Value)>,
        given: Vec<ArgVal>,
    ) -> Run<Value> {
        if given.len() != params.len() {
            return unsupported("an arrow function called with another number of arguments");
        }
        let mut vars: Vec<(String, Binding)> =
            captured.into_iter().map(|(name, value)| (name, Binding::Value(value))).collect();
        for (param, arg) in params.iter().zip(given) {
            let value = self.arg_as_value(arg)?;
            vars.push((param.clone(), Binding::Value(value)));
        }
        let previous = self.push_frame(vars, None)?;
        let value = self.eval(body);
        self.pop_frame(previous);
        value
    }

    /// An argument's value, reading the place a `ref` one names.
    pub(crate) fn arg_as_value(&mut self, arg: ArgVal) -> Run<Value> {
        match arg {
            ArgVal::Value(_, value) => Ok(value),
            ArgVal::Ref(_, place) => self.read_place(&place, Span::default()),
            ArgVal::Lazy(_, expr) => self.eval(expr),
        }
    }

    /// Calls a function the program wrote: its parameters bound to the arguments, by position
    /// and then by name, and the defaults of those left out.
    pub(crate) fn call_def(
        &mut self,
        def: &'static FnDef,
        parent: Option<u64>,
        receiver: Option<Receiver>,
        given: Vec<ArgVal>,
        span: Span,
    ) -> Run<Value> {
        if def.body.is_empty() {
            return unsupported(format!("the intrinsic `{}`", def.name));
        }
        let mut vars = Vec::new();
        let mut params = def.params.iter().peekable();
        if let Some(param) = params.next_if(|param| param.is_self) {
            let binding = match receiver {
                Some(Receiver::Place(place)) if param.is_mut => Binding::Ref(place),
                Some(Receiver::Place(place)) => Binding::Value(self.read_place(&place, span)?),
                Some(Receiver::Value(value)) => Binding::Value(value),
                None => return unsupported("a method called without its value"),
            };
            vars.push(("self".to_owned(), binding));
        }
        let params: Vec<&fennec_syntax::Param> = params.collect();
        let mut slots: Vec<Option<Binding>> = params.iter().map(|_| None).collect();
        let mut next = 0;
        for arg in given {
            let at = if let Some(name) = arg.name() {
                params.iter().position(|param| param.name == name)
            } else {
                next += 1;
                Some(next - 1)
            };
            let Some(slot) = at.and_then(|at| slots.get_mut(at)) else {
                return unsupported("an argument no parameter takes");
            };
            *slot = Some(match arg {
                ArgVal::Ref(_, place) => Binding::Ref(place),
                other => Binding::Value(self.arg_as_value(other)?),
            });
        }
        let mut defaults = Vec::new();
        for (param, slot) in params.iter().zip(slots) {
            match slot {
                Some(binding) => vars.push((param.name.clone(), binding)),
                None => match &param.default {
                    Some(default) => defaults.push((param.name.clone(), default)),
                    None => return unsupported(format!("a call leaving out `{}`", param.name)),
                },
            }
        }
        if self.is_generator(def) {
            for (name, default) in defaults {
                let value = self.plain_value(default)?;
                vars.push((name, Binding::Value(value)));
            }
            let shown = self.program.type_at(span).map(shown_type);
            return self.generator_from_def(def, vars, shown);
        }
        let previous = self.push_frame(vars, parent)?;
        if let (Some(owner), Some(frame)) = (self.program.owner_of(def), self.frames.get_mut(self.cur)) {
            frame.owner = Some(owner.to_owned());
        }
        let ran = (|| {
            for (name, default) in defaults {
                let value = self.eval(default)?;
                self.bind(&name, value)?;
            }
            self.block(&def.body)
        })();
        self.pop_frame(previous);
        match ran {
            Ok(()) => Ok(Value::Unit),
            Err(Flow::Return(value)) => Ok(value),
            Err(Flow::Break | Flow::Continue) => unsupported("a `break` or `continue` outside a loop"),
            Err(other) => Err(other),
        }
    }

    /// A default written as a plain value, worked out on its own.
    pub(crate) fn plain_value(&mut self, expr: &'static Expr) -> Run<Value> {
        let previous = self.push_frame(Vec::new(), None)?;
        let value = self.eval(expr);
        self.pop_frame(previous);
        value
    }

    /// Makes a struct, an error, or a case: values given by position go to the public fields in
    /// order, then the named ones, then each field left out takes its default. A struct with
    /// `where` rules is checked once it's made.
    pub(crate) fn construct(
        &mut self,
        ty: &str,
        case: Option<&str>,
        given: Vec<ArgVal>,
        span: Span,
    ) -> Run<Value> {
        let Some(declared) = self.program.types.get(ty) else {
            return unsupported(format!("making a `{ty}`"));
        };
        let fields: &'static [Field] = match case {
            Some(case) => match declared.def.cases.iter().find(|known| known.name == case) {
                Some(found) => &found.fields,
                None => return unsupported(format!("the case `{ty}.{case}`")),
            },
            None => &declared.def.fields,
        };
        let positional: Vec<usize> = (0..fields.len())
            .filter(|at| case.is_some() || fields.get(*at).is_some_and(|field| !field.name.starts_with('_')))
            .collect();
        let mut values: Vec<Option<Value>> = fields.iter().map(|_| None).collect();
        let mut next = 0;
        for arg in given {
            let at = if let Some(name) = arg.name() {
                fields.iter().position(|field| field.name == name)
            } else {
                next += 1;
                positional.get(next - 1).copied()
            };
            let value = self.arg_as_value(arg)?;
            match at.and_then(|at| values.get_mut(at)) {
                Some(slot) => *slot = Some(value),
                None => return unsupported("a value no field takes"),
            }
        }
        let mut made = Vec::with_capacity(fields.len());
        for (field, value) in fields.iter().zip(values) {
            let value = match (value, &field.default) {
                (Some(value), _) => value,
                (None, Some(default)) => self.plain_value(default)?,
                (None, None) => return unsupported(format!("making a `{ty}` without `{}`", field.name)),
            };
            made.push((field.name.clone(), value));
        }
        let instance =
            crate::value::Instance { ty: ty.to_owned(), case: case.map(str::to_owned), fields: made };
        self.ruled(instance, span)
    }

    /// A value just made, if its `where` rules hold. Where the checker typed it as something that
    /// can fail, a broken rule is an `InvalidValue`, and otherwise it stops the program.
    pub(crate) fn ruled(&mut self, instance: crate::value::Instance, span: Span) -> Run<Value> {
        if let Some(broken) = self.broken_rule(&instance)? {
            if matches!(self.program.type_at(span), Some(Type::Result(..))) {
                let message = Value::Str(format!("{}.{broken} breaks its `where` rule", instance.ty));
                let error = Value::instance("InvalidValue", None, vec![("message".to_owned(), message)]);
                return Err(Flow::Fail(error, span));
            }
            return crash(format!("this breaks the `where` rule on `{}.{broken}`", instance.ty), span);
        }
        Ok(Value::Instance(Box::new(instance)))
    }

    /// The first field whose `where` rule a struct breaks, if any.
    pub(crate) fn broken_rule(&mut self, instance: &crate::value::Instance) -> Run<Option<String>> {
        if instance.case.is_some() {
            return Ok(None);
        }
        let Some(declared) = self.program.types.get(instance.ty.as_str()) else { return Ok(None) };
        if declared.def.fields.iter().all(|field| field.rule.is_none()) {
            return Ok(None);
        }
        let vars = instance
            .fields
            .iter()
            .map(|(name, value)| (name.clone(), Binding::Value(value.clone())))
            .collect();
        let previous = self.push_frame(vars, None)?;
        let found = (|| {
            for field in &declared.def.fields {
                if let Some(rule) = &field.rule
                    && !self.truth(rule)?
                {
                    return Ok(Some(field.name.clone()));
                }
            }
            Ok(None)
        })();
        self.pop_frame(previous);
        found
    }

    /// The name a case `.Case` is made of, where it's written, then made with these values.
    pub(crate) fn construct_case_at(
        &mut self,
        case: &str,
        expr: &'static Expr,
        given: Vec<ArgVal>,
    ) -> Run<Value> {
        let ty = self.enum_named_at(case, &[expr.span])?;
        self.construct(&ty, Some(case), given, expr.span)
    }
}

/// The type's name a value's methods are found under.
pub(crate) fn type_name_of(value: &Value) -> Option<String> {
    Some(match value {
        Value::Instance(instance) => instance.ty.clone(),
        Value::Str(_) => "str".to_owned(),
        Value::Int(_) => "int".to_owned(),
        Value::Float(_) => "float".to_owned(),
        Value::Bool(_) => "bool".to_owned(),
        Value::List(_) => "list".to_owned(),
        Value::Dict(_) => "dict".to_owned(),
        Value::Set(_) => "set".to_owned(),
        Value::Sized(num, _) => num.name().to_owned(),
        Value::None => "None".to_owned(),
        _ => return None,
    })
}

/// Every type name a type mentions at its top: itself, what an optional holds, a union's members.
fn type_names(ty: &Type, names: &mut Vec<String>) {
    match ty {
        Type::Named(name) | Type::Generic(name, _) => {
            names.push(fennec_syntax::rename::shown_type_name(name).to_owned());
        }
        Type::Optional(inner) => type_names(inner, names),
        Type::Result(value, error) => {
            type_names(value, names);
            type_names(error, names);
        }
        Type::Union(members) => {
            for member in members {
                type_names(member, names);
            }
        }
        Type::Errors(errors) => {
            names.extend(errors.iter().map(|name| fennec_syntax::rename::shown_type_name(name).to_owned()));
        }
        _ => {}
    }
}

/// A type as a generator prints it, with the program's own type names as they're written.
pub(crate) fn shown_type(ty: &Type) -> String {
    ty.to_string().replace(fennec_syntax::rename::RENAMED, "")
}
