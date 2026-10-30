//! The builtins' signatures: Python's builtins that Fennec keeps, `Ok`, `Err`, `replace` and
//! `ignore`, and the methods of built-in values. Methods that change a value need it held in
//! a `mut` place.

use fennec_syntax::{Arg, Diagnostic, Expr, Span};

use crate::function::FnCx;
use crate::infer::Hole;
use crate::messages::{mismatch, needless_mut, no_method, wrong_args};
use crate::types::{Type, a, article};

/// Each builtin, and how many plain arguments it takes (at least, at most).
/// Names with a dot belong to a module, like `json.loads`, and exist only once imported.
const BUILTINS: [(&str, usize, usize); 36] = [
    ("Err", 1, 1),
    ("Ok", 0, 1),
    ("abs", 1, 1),
    ("all", 1, 1),
    ("any", 1, 1),
    ("ceil", 1, 1),
    ("cos", 1, 1),
    ("enumerate", 1, 1),
    ("float", 1, 1),
    ("floor", 1, 1),
    ("ignore", 2, 2),
    ("int", 1, 1),
    ("json.dumps", 1, 1),
    ("json.loads", 1, 1),
    ("len", 1, 1),
    ("list", 1, 1),
    ("max", 1, usize::MAX),
    ("min", 1, usize::MAX),
    ("parallel", 1, 1),
    ("random.random_choice", 3, 3),
    ("random.random_float", 2, 2),
    ("random.random_int", 4, 4),
    ("range", 1, 3),
    ("re.is_regex", 1, 1),
    ("replace", 1, usize::MAX),
    ("reversed", 1, 1),
    ("round", 1, 2),
    ("set", 0, 1),
    ("sin", 1, 1),
    ("sorted", 1, 1),
    ("sqrt", 1, 1),
    ("str", 1, 1),
    ("sum", 1, 1),
    ("toml.dumps", 1, 1),
    ("toml.loads", 1, 1),
    ("zip", 2, 2),
];

/// Builtins whose arguments have to be plain values, not results or optionals.
const NEEDS_VALUES: [&str; 14] = [
    "abs",
    "ceil",
    "cos",
    "float",
    "floor",
    "int",
    "len",
    "random.random_choice",
    "random.random_float",
    "random.random_int",
    "range",
    "round",
    "sin",
    "sqrt",
];

pub(crate) fn is_builtin(name: &str) -> bool {
    BUILTINS.iter().any(|(builtin, ..)| *builtin == name)
}

/// The modules with a function of this name, like `json` and `toml` for `loads`.
pub(crate) fn modules_with(name: &str) -> Vec<&'static str> {
    BUILTINS
        .iter()
        .filter_map(|(builtin, ..)| builtin.split_once('.').filter(|(_, function)| *function == name))
        .map(|(module, _)| module)
        .chain(fennec_syntax::importable_type(name))
        .collect()
}

/// A method of a built-in value: whether it changes the value, what it takes and gives
/// back, and how many arguments it takes.
struct Method {
    changes: bool,
    /// The types of its arguments, in order. Arguments past these can be anything.
    params: Vec<Type>,
    returns: Type,
    arity: (usize, usize),
    /// The types an arrow function passed to it receives, like the item for `remove_where`.
    receives: Vec<Type>,
}

fn reads(params: &[Type], returns: Type) -> Method {
    let count = params.len();
    Method { changes: false, params: params.to_vec(), returns, arity: (count, count), receives: Vec::new() }
}

fn changes(params: &[Type], returns: Type) -> Method {
    Method { changes: true, ..reads(params, returns) }
}

impl Method {
    /// An arrow function passed to this method receives an item.
    fn receiving(self, item: &Type) -> Method {
        Method { receives: vec![item.clone()], ..self }
    }

    /// The last `optional` arguments can be left out.
    fn optional(self, optional: usize) -> Method {
        Method { arity: (self.arity.0.saturating_sub(optional), self.arity.1), ..self }
    }

    /// Takes any number of arguments of any type.
    fn any_args(self) -> Method {
        Method { arity: (0, usize::MAX), ..self }
    }
}

fn method(ty: &Type, name: &str) -> Option<Method> {
    match ty {
        Type::List(item) => list_method(item, name),
        Type::Set(item) => set_method(item, name),
        Type::Dict(key, value) => dict_method(key, value, name),
        Type::IdMap(item) => id_map_method(item, name),
        Type::Str => str_method(name),
        Type::Named(type_name) => named_method(type_name, name),
        _ => None,
    }
}

fn list_method(item: &Type, name: &str) -> Option<Method> {
    let item = item.clone();
    Some(match name {
        "append" | "remove" => changes(&[item], Type::Unit),
        "extend" => changes(&[Type::list(item)], Type::Unit),
        "insert" => changes(&[Type::Int, item], Type::Unit),
        "clear" | "reverse" => changes(&[], Type::Unit),
        "remove_where" => changes(&[], Type::Unit).receiving(&item).any_args(),
        "sort" => changes(&[], Type::Unit).receiving(&item),
        "pop" => changes(&[Type::Int], item).optional(1),
        "index" => reads(&[item], Type::optional(Type::Int)),
        "count" => reads(&[item], Type::Int),
        _ => return None,
    })
}

fn set_method(item: &Type, name: &str) -> Option<Method> {
    Some(match name {
        "add" | "discard" | "remove" => changes(std::slice::from_ref(item), Type::Unit),
        "clear" => changes(&[], Type::Unit),
        _ => return None,
    })
}

fn dict_method(key: &Type, value: &Type, name: &str) -> Option<Method> {
    let (key, value) = (key.clone(), value.clone());
    Some(match name {
        "pop" => changes(&[key], value),
        "get" => reads(&[key, value.clone()], Type::optional(value)).optional(1),
        "keys" => reads(&[], Type::list(key)),
        "values" => reads(&[], Type::list(value)),
        "items" => reads(&[], Type::list(Type::Tuple(vec![key, value]))),
        "clear" => changes(&[], Type::Unit),
        _ => return None,
    })
}

fn id_map_method(item: &Type, name: &str) -> Option<Method> {
    let id = Type::Id(Box::new(item.clone()));
    let item = item.clone();
    Some(match name {
        "insert" => changes(&[item], id),
        "remove" => changes(&[id], Type::optional(item)),
        "get" => reads(&[id], Type::optional(item)),
        "modify" => changes(&[id], Type::Bool).receiving(&item).any_args(),
        "remove_where" => changes(&[], Type::Unit).receiving(&item).any_args(),
        "values" => reads(&[], Type::list(item)),
        "ids" => reads(&[], Type::list(id)),
        "items" => reads(&[], Type::list(Type::Tuple(vec![id, item]))),
        "clear" => changes(&[], Type::Unit),
        _ => return None,
    })
}

fn str_method(name: &str) -> Option<Method> {
    let text = Type::Str;
    Some(match name {
        "upper" | "lower" | "strip" | "lstrip" | "rstrip" => reads(&[], Type::Str),
        "split" => reads(&[text], Type::list(Type::Str)).optional(1),
        "lines" => reads(&[], Type::list(Type::Str)),
        "startswith" | "endswith" => reads(&[text], Type::Bool),
        "isdigit" => reads(&[], Type::Bool),
        "replace" => reads(&[text.clone(), text], Type::Str),
        "find" => reads(&[text], Type::optional(Type::Int)),
        "join" => reads(&[Type::list(Type::Str)], Type::Str),
        _ => return None,
    })
}

/// Methods of capabilities, and of standard library types that are written in Rust.
fn named_method(type_name: &str, name: &str) -> Option<Method> {
    let io = |value: Type| Type::result(value, Type::named("IoError"));
    let found = || Type::named("Match");
    let text = || Type::Str;
    Some(match (type_name, name) {
        ("Stdout", "print" | "write") => reads(&[], Type::Unit).any_args(),
        ("Fs", "exists") => reads(&[text()], Type::Bool),
        ("Fs", "read_text") => reads(&[text()], io(text())),
        ("Fs", "write_text") => reads(&[text(), text()], io(Type::Unit)),
        ("Clock", "now") => reads(&[], Type::named("Instant")),
        ("Clock", "sleep") => reads(&[Type::named("Duration")], Type::Unit),
        ("Regex", "match" | "fullmatch" | "search") => reads(&[text()], Type::optional(found())),
        ("Regex", "find_all") => reads(&[text()], Type::list(found())),
        ("Regex", "replace") => reads(&[text(), text()], text()),
        ("Regex", "split") => reads(&[text()], Type::list(text())),
        _ => return None,
    })
}

impl FnCx<'_> {
    pub(crate) fn builtin_method(
        &mut self,
        object: &Expr,
        ty: &Type,
        name: &str,
        args: &[Arg],
        span: Span,
    ) -> Type {
        let Some(method) = method(ty, name) else {
            return self.unusable(no_method(ty, name, span), args);
        };
        if method.changes {
            let what = format!(
                "`{name}` changes it, so call it on something held in a mut name, like `items.{name}(...)`"
            );
            self.mut_place(object, &what);
        }
        let named: &[&str] = if name == "sort" { &["key"] } else { &[] };
        let plain = self.builtin_args(name, args, named, &method.receives, span);
        if plain.len() < method.arity.0 || plain.len() > method.arity.1 {
            self.error(wrong_args(name, span));
        }
        // `Fs` takes a file's path as a `Path` or as text.
        let path = *ty == Type::named("Fs");
        for ((actual, at), expected) in plain.iter().zip(&method.params) {
            let as_path = path && *expected == Type::Str && *actual == Type::named("Path");
            if as_path || self.checker.fits(expected, actual) {
                continue;
            }
            if path {
                let message =
                    format!("`{name}` takes a path, as a str or a `Path`, but this is {}", a(actual));
                self.error(Diagnostic::new(message, *at));
                continue;
            }
            let expected = self.checker.settled(expected);
            self.error(mismatch(&format!("`{name}`'s argument"), &expected, actual, *at));
        }
        // `table.get(key, default)` gives the default instead of None.
        if let (Type::Dict(_, value), [_, _]) = (ty, plain.as_slice()) {
            return value.as_ref().clone();
        }
        method.returns
    }

    /// Checks a builtin's arguments: none is `mut`, and only those in `named` may be named.
    /// Returns the plain arguments' types and where they are.
    fn builtin_args(
        &mut self,
        name: &str,
        args: &[Arg],
        named: &[&str],
        receives: &[Type],
        span: Span,
    ) -> Vec<(Type, Span)> {
        let mut plain = Vec::new();
        for arg in args {
            let ty = self.arg_value(arg, receives);
            match arg.name.as_deref() {
                Some("key") if !named.contains(&"key") => {
                    self.error(Diagnostic::new(format!("`{name}` doesn't take `key`"), span));
                }
                Some("key") => {}
                Some("indent") if named.contains(&"indent") => {
                    if !self.checker.fits(&Type::Int, &ty) {
                        self.error(mismatch("`indent`", &Type::Int, &ty, arg.value.span));
                    }
                }
                Some(other) => self.error(Diagnostic::new(
                    format!("`{name}` has no parameter called `{other}`"),
                    arg.value.span,
                )),
                None => plain.push((ty, arg.value.span)),
            }
        }
        if args.iter().any(|arg| arg.is_mut) {
            self.error(needless_mut(name, span));
        }
        plain
    }

    pub(crate) fn call_builtin(&mut self, name: &str, args: &[Arg], span: Span) -> Type {
        if name == "replace" {
            return self.replace(args, span);
        }
        // An arrow function passed as `key` receives the items of the first argument.
        let first = args
            .iter()
            .find(|arg| arg.name.is_none())
            .map_or(Type::Unknown, |arg| self.peek_type(&arg.value));
        let item = peek_item(&first);
        let named: &[&str] = match name {
            "min" | "max" | "sorted" => &["key"],
            "json.dumps" => &["indent"],
            _ => &[],
        };
        let plain = self.builtin_args(name, args, named, std::slice::from_ref(&item), span);
        let (low, high) = BUILTINS
            .iter()
            .find(|(builtin, ..)| *builtin == name)
            .map_or((0, 0), |(_, low, high)| (*low, *high));
        if plain.len() < low || plain.len() > high {
            self.error(wrong_args(name, span));
            return Type::Unknown;
        }
        self.builtin_result(name, &plain, span)
    }

    fn builtin_result(&mut self, name: &str, plain: &[(Type, Span)], span: Span) -> Type {
        let types: Vec<Type> = plain.iter().map(|(ty, _)| ty.clone()).collect();
        let (first, at) = plain.first().cloned().unwrap_or((Type::Unit, span));
        if NEEDS_VALUES.contains(&name) && !plain.iter().all(|(ty, at)| self.usable(ty, *at)) {
            return Type::Unknown;
        }
        match name {
            "Ok" => Type::result(first, self.checker.fresh(span, Hole::OkError)),
            "Err" => Type::result(self.checker.fresh(span, Hole::ErrValue), first),
            "len" => {
                let has_len =
                    first.type_name().is_some_and(|name| self.checker.program.method(name, "len").is_some());
                let sized =
                    first.is_collection() || has_len || matches!(first, Type::Tuple(_) | Type::IdMap(_));
                if !sized && first.is_known() {
                    self.error(Diagnostic::new(format!("{} has no length", article(&first.kind())), span));
                }
                Type::Int
            }
            "range" => {
                if !types.iter().all(|ty| self.checker.fits(&Type::Int, ty)) {
                    self.error(Diagnostic::new("`range` takes one to three ints", span));
                }
                Type::list(Type::Int)
            }
            "json.loads" | "toml.loads" | "json.dumps" | "toml.dumps" => {
                self.format_result(name, &first, (at, span))
            }
            "str" => Type::Str,
            "re.is_regex" => Type::Bool,
            "random.random_int" | "random.random_float" | "random.random_choice" => {
                self.random(name, plain, span)
            }
            "ignore" => {
                if !plain.get(1).is_some_and(|(reason, _)| self.checker.fits(&Type::Str, reason)) {
                    self.error(wrong_args(name, span));
                }
                Type::Unit
            }
            "int" | "float" => {
                if !matches!(first, Type::Int | Type::Float | Type::Str | Type::Bool | Type::Unknown) {
                    let message = format!("`{name}` converts a number or text, not {}", a(&first));
                    self.error(Diagnostic::new(message, at));
                }
                if name == "int" { Type::Int } else { Type::Float }
            }
            "round" | "floor" | "ceil" | "sqrt" | "sin" | "cos" | "abs" => self.math(name, &types, span),
            _ => self.collection_builtin(name, plain, span),
        }
    }

    /// `std.random`'s builtins, which its `Random` is written with. Each takes the seed and
    /// how many numbers were drawn, and gives back a value and the new count.
    fn random(&mut self, name: &str, plain: &[(Type, Span)], span: Span) -> Type {
        // `random_int` also takes the range; the others take only the seed and count.
        let ints = if name == "random.random_int" { 4 } else { 2 };
        let all_ints = plain.iter().take(ints).all(|(ty, _)| self.checker.fits(&Type::Int, ty));
        if !all_ints {
            self.error(Diagnostic::new(format!("`{name}` takes ints"), span));
        }
        let value = match name {
            "random.random_int" => Type::Int,
            "random.random_float" => Type::Float,
            _ => match plain.get(2) {
                Some((Type::List(item), _)) => item.as_ref().clone(),
                Some((other, at)) => {
                    let message = format!("`{name}` picks from a list, but this is {}", a(other));
                    self.error(Diagnostic::new(message, *at));
                    Type::Unknown
                }
                None => Type::Unknown,
            },
        };
        Type::Tuple(vec![value, Type::Int])
    }

    /// The math builtins, which take numbers.
    fn math(&mut self, name: &str, types: &[Type], span: Span) -> Type {
        let first = types.first().cloned().unwrap_or(Type::Unknown);
        let number = matches!(first, Type::Int | Type::Float | Type::Unknown);
        let digits = types.get(1).is_none_or(|digits| self.checker.fits(&Type::Int, digits));
        if !number || !digits {
            let message = format!("`{name}` takes a number, but this is {}", a(&first));
            self.error(Diagnostic::new(message, span));
            return Type::Unknown;
        }
        match name {
            "round" if types.len() == 2 => Type::Float,
            "round" | "floor" | "ceil" => Type::Int,
            "abs" => first,
            _ => Type::Float,
        }
    }

    /// The builtins that work on the items of a collection.
    fn collection_builtin(&mut self, name: &str, plain: &[(Type, Span)], span: Span) -> Type {
        let types: Vec<Type> = plain.iter().map(|(ty, _)| ty.clone()).collect();
        let Some((first, at)) = plain.first().cloned() else {
            // Only `set()` takes nothing.
            return Type::Set(Box::new(self.checker.fresh(span, Hole::EmptySet)));
        };
        let items = |cx: &mut Self| cx.item_type(&first, at);
        match name {
            "min" | "max" if types.len() > 1 => {
                self.join_or_report(&types, span, &format!("`{name}` is given"))
            }
            "min" | "max" => items(self),
            "sum" => {
                let item = items(self);
                if !matches!(
                    self.checker.settled(&item),
                    Type::Int | Type::Float | Type::Unknown | Type::Var(_)
                ) {
                    let message = format!("`sum` adds numbers, but these are {}s", item.kind());
                    self.error(Diagnostic::new(message, span));
                }
                item
            }
            "any" | "all" => {
                let item = items(self);
                if !self.checker.fits(&Type::Bool, &item) {
                    self.error(
                        Diagnostic::new(
                            format!("`{name}` needs bools, but got {}", article(&item.kind())),
                            span,
                        )
                        .with_help(
                            "Fennec has no truthiness: compare explicitly, like `any(x > 0 for x in xs)`",
                        ),
                    );
                }
                Type::Bool
            }
            "reversed" if first == Type::Str => Type::Str,
            "sorted" | "list" | "parallel" | "reversed" => Type::list(items(self)),
            "set" => Type::Set(Box::new(items(self))),
            "enumerate" => Type::list(Type::Tuple(vec![Type::Int, items(self)])),
            "zip" => {
                let first = items(self);
                let second = plain.get(1).map_or(Type::Unknown, |(ty, at)| self.item_type(ty, *at));
                Type::list(Type::Tuple(vec![first, second]))
            }
            _ => Type::Unknown,
        }
    }

    /// `replace(value, field=new, ...)`: a copy with some fields changed.
    fn replace(&mut self, args: &[Arg], span: Span) -> Type {
        let types: Vec<Type> = args.iter().map(|arg| self.arg_value(arg, &[])).collect();
        let Some(ty) = types.first() else {
            self.error(
                Diagnostic::new("`replace` takes a struct value, then the fields to change", span)
                    .with_help("like `replace(enemy, health=50)`"),
            );
            return Type::Unknown;
        };
        if args.iter().any(|arg| arg.is_mut) {
            self.error(needless_mut("replace", span));
        }
        let Some(type_name) = ty.type_name() else {
            if ty.is_known() {
                let message = format!("`replace` copies a struct, but this is {}", a(ty));
                self.error(Diagnostic::new(message, span));
            }
            return ty.clone();
        };
        for (arg, actual) in args.iter().zip(&types).skip(1) {
            let Some(name) = &arg.name else {
                self.error(Diagnostic::new("name the field to change, like `health=50`", arg.value.span));
                continue;
            };
            let has_field = self
                .checker
                .program
                .type_info(type_name)
                .is_some_and(|info| info.def.fields.iter().any(|field| &field.name == name));
            if !has_field {
                self.error(Diagnostic::new(
                    format!("`{type_name}` has no field called `{name}`"),
                    arg.value.span,
                ));
                continue;
            }
            let expected = self
                .checker
                .field_type(type_name, None, name)
                .substitute(&self.checker.program.type_args(ty));
            if !self.checker.fits(&expected, actual) {
                self.error(mismatch(&format!("`{name}`"), &expected, actual, arg.value.span));
            }
        }
        ty.clone()
    }
}

/// The item type of a value looped over, without reporting anything.
fn peek_item(ty: &Type) -> Type {
    match ty {
        Type::List(item) | Type::Set(item) | Type::IdMap(item) | Type::Dict(item, _) => item.as_ref().clone(),
        Type::Tuple(items) if items.windows(2).all(|pair| pair.first() == pair.last()) => {
            items.first().cloned().unwrap_or(Type::Unknown)
        }
        Type::Str => Type::Str,
        _ => Type::Unknown,
    }
}
