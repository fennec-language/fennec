//! The builtins' signatures: Python's builtins that Fennec keeps, `Ok`, `Err`, `replace` and
//! `ignore`, and the methods of built-in values. Methods that change a value need it held in
//! a `mut` place.

use fennec_syntax::{Arg, Diagnostic, Expr, Span};

use crate::function::FnCx;
use crate::infer::Hole;
use crate::messages::{mismatch, needless_mut, no_method, wrong_args};
use crate::stdlib;
use crate::types::{Type, a, article};
use crate::{ParamType, Signature};

/// Each builtin, and how many plain arguments it takes (at least, at most). Standard library
/// functions, like `json.loads`, are in `stdlib`, and exist only once imported.
const BUILTINS: [(&str, usize, usize); 23] = [
    ("Err", 1, 1),
    ("Ok", 0, 1),
    ("all", 1, 1),
    ("any", 1, 1),
    ("bytes", 0, 1),
    ("enumerate", 1, 1),
    ("float", 1, 1),
    ("ignore", 2, 2),
    ("copy", 1, 1),
    ("int", 1, 1),
    ("list", 1, 1),
    ("max", 1, usize::MAX),
    ("min", 1, usize::MAX),
    ("parallel", 1, 1),
    ("range", 1, 3),
    ("replace", 1, usize::MAX),
    ("reversed", 1, 1),
    ("round", 1, 2),
    ("set", 0, 1),
    ("sorted", 1, 1),
    ("str", 1, 1),
    ("sum", 1, 2),
    ("zip", 2, 2),
];

/// Builtins whose arguments have to be plain values, not results or optionals.
const NEEDS_VALUES: [&str; 4] = ["float", "int", "range", "round"];

pub(crate) fn is_builtin(name: &str) -> bool {
    BUILTINS.iter().any(|(builtin, ..)| *builtin == name) || stdlib::function(name).is_some()
}

/// The modules with a function of this name, like `json` and `toml` for `loads`.
pub(crate) fn modules_with(name: &str) -> Vec<&'static str> {
    stdlib::functions()
        .filter_map(|builtin| builtin.split_once('.').filter(|(_, function)| *function == name))
        .map(|(module, _)| module)
        .chain(fennec_syntax::importable_type(name))
        .collect()
}

/// A method of a built-in value: whether it changes the value, what it takes and gives
/// back, and how many arguments it takes.
pub(crate) struct Method {
    pub(crate) changes: bool,
    /// The types of its arguments, in order. Arguments past these can be anything.
    params: Vec<Type>,
    returns: Type,
    arity: (usize, usize),
    /// The types an arrow function passed to it receives, like the item for `remove_where`.
    receives: Vec<Type>,
}

pub(crate) fn reads(params: &[Type], returns: Type) -> Method {
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

/// The signature of a method or operator the engines give a built-in type, for fitting it
/// to an interface: `abs` on an int, or `+` on text.
pub(crate) fn engine_signature(ty: &Type, name: &str) -> Option<Signature> {
    let param = |name: &str, ty: &Type, is_mut| ParamType {
        name: name.to_owned(),
        ty: ty.clone(),
        is_mut,
        default: None,
    };
    let (others, returns, is_mut) = if let Some(found) = method(ty, name) {
        (found.params, found.returns, found.changes)
    } else {
        let returns = match (ty, name) {
            (Type::Int | Type::Float, "+" | "-" | "*") | (Type::Str, "+") => ty.clone(),
            (Type::Int | Type::Float | Type::Str, "<") => Type::Bool,
            _ => return None,
        };
        (vec![ty.clone()], returns, false)
    };
    let mut params = vec![param("self", ty, is_mut)];
    params.extend(others.iter().map(|other| param("other", other, false)));
    Some(Signature { params, returns, can_fail: false, generator: false })
}

pub(crate) fn method(ty: &Type, name: &str) -> Option<Method> {
    // Every collection knows its length, and so does a tuple.
    let sized = matches!(
        ty,
        Type::List(_)
            | Type::Set(_)
            | Type::Dict(..)
            | Type::IdMap(_)
            | Type::Str
            | Type::Bytes
            | Type::Tuple(_)
    );
    if sized && name == "len" {
        return Some(reads(&[], Type::Int));
    }
    if matches!(ty, Type::Int | Type::Float) && name == "abs" {
        return Some(reads(&[], ty.clone()));
    }
    match ty {
        Type::List(item) => list_method(item, name),
        Type::Set(item) => set_method(item, name),
        Type::Dict(key, value) => dict_method(key, value, name),
        Type::IdMap(item) => id_map_method(item, name),
        Type::Str => str_method(name),
        Type::Bytes => bytes_method(name),
        Type::Named(type_name) => named_method(type_name, name),
        _ => None,
    }
}

fn list_method(item: &Type, name: &str) -> Option<Method> {
    let item = item.clone();
    Some(match name {
        "append" | "remove" => changes(&[item], Type::Unit),
        "insert" => changes(&[Type::Int, item], Type::Unit),
        "clear" => changes(&[], Type::Unit),
        "remove_where" => changes(&[], Type::Unit).receiving(&item).any_args(),
        "sort" => changes(&[], Type::Unit).receiving(&item),
        "pop" => changes(&[Type::Int], item).optional(1),
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
        "replace" => reads(&[text.clone(), text], Type::Str),
        "find" => reads(&[text], Type::optional(Type::Int)),
        "join" => reads(&[Type::list(Type::Str)], Type::Str),
        "encode" => reads(&[], Type::Bytes),
        "code" => reads(&[], Type::Int),
        "codes" => reads(&[], Type::list(Type::Int)),
        _ => return None,
    })
}

/// `data.decode()` reads UTF-8 text, and fails on bytes that aren't.
fn bytes_method(name: &str) -> Option<Method> {
    (name == "decode").then(|| reads(&[], Type::result(Type::Str, Type::named("InvalidValue"))))
}

/// Methods of capabilities, and of standard library types that are written in Rust.
fn named_method(type_name: &str, name: &str) -> Option<Method> {
    Some(match (type_name, name) {
        ("Stdout", "print" | "write") => reads(&[], Type::Unit).any_args(),
        ("Stdout", "captured") => reads(&[], Type::Str),
        ("Clock", "now") => reads(&[], Type::named("Instant")),
        ("Args", "len") => reads(&[], Type::Int),
        ("Stdin", "lines") => reads(&[], Type::list(Type::Str)),
        ("Clock", "sleep") => reads(&[Type::named("Duration")], Type::Unit),
        _ => return stdlib::method(type_name, name),
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
        if name == "captured" && *ty == Type::named("Stdout") && !self.in_a_test() {
            let help = "only a test's own body can read what it printed, from the `Stdout` it received";
            return self.unusable(no_method(ty, name, span).with_help(help), args);
        }
        // Only a test's fake `Process` is scripted, and says what ran.
        if matches!(name, "script" | "ran") && *ty == Type::named("Process") && !self.in_a_test() {
            let help = "only a test's own body can script or read the fake `Process` it received";
            return self.unusable(no_method(ty, name, span).with_help(help), args);
        }
        if method.changes {
            let what = format!(
                "`{name}` changes it, so call it on something held in a mut name, like `items.{name}(...)`"
            );
            self.mut_place(object, &what);
        }
        let named: &[&str] = if name == "sort" { &["key"] } else { &[] };
        let plain = self.builtin_args(name, args, named, &method.receives, span);
        if name == "run" && *ty == Type::named("Process") && plain.len() != 2 {
            let message =
                "`run` takes the program, then its arguments as a list, never one string a shell would split";
            let help = "like `process.run(\"git\", [\"log\", \"--oneline\"])`, or `process.run(\"ls\", [])` for none";
            self.error(Diagnostic::new(message, span).with_help(help));
        } else if plain.len() < method.arity.0 || plain.len() > method.arity.1 {
            self.error(wrong_args(name, span));
        }
        // `Fs` takes a file's path as a `Path` or as text.
        let path = *ty == Type::named("Fs");
        for ((actual, at), expected) in plain.iter().zip(&method.params) {
            let as_path = path && *expected == Type::Str && *actual == Type::named("Path");
            let generated = name == "join" && *actual == Type::Iterator(Box::new(Type::Str));
            if as_path || generated || self.checker.fits(expected, actual) {
                continue;
            }
            if path && *expected == Type::Str {
                let message =
                    format!("`{name}` takes a path, as a str or a `Path`, but this is {}", a(actual));
                self.error(Diagnostic::new(message, *at));
                continue;
            }
            let expected = self.checker.settled(expected);
            self.error(mismatch(&format!("`{name}`'s argument"), &expected, actual, *at));
        }
        // `get` has no default of its own, since an optional's `else` says it.
        if let (Type::Dict(_, value), "get", [(_, key), (_, default)]) = (ty, name, plain.as_slice()) {
            self.error(
                Diagnostic::new("`get` takes only a key", *default)
                    .with_help("write `table.get(key) else default`")
                    .with_fix(Span { start: span.start, end: span.start }, "(")
                    .with_fix(Span { start: key.end, end: default.start }, ") else "),
            );
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
        let mut start = None;
        for arg in args {
            let ty = self.arg_value(arg, receives);
            match arg.name.as_deref() {
                Some("key") if !named.contains(&"key") => {
                    self.error(Diagnostic::new(format!("`{name}` doesn't take `key`"), span));
                }
                Some("key") => {}
                Some("start") if named.contains(&"start") => start = Some((ty, arg.value.span)),
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
        plain.extend(start);
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
        let named: &[&str] = match (name, stdlib::function(name)) {
            ("min" | "max" | "sorted", _) => &["key"],
            ("sum", _) => &["start"],
            (_, Some((_, function))) => function.named,
            _ => &[],
        };
        let plain = self.builtin_args(name, args, named, std::slice::from_ref(&item), span);
        let (low, high) = match stdlib::function(name) {
            Some((_, function)) => function.arity,
            None => BUILTINS
                .iter()
                .find(|(builtin, ..)| *builtin == name)
                .map_or((0, 0), |(_, low, high)| (*low, *high)),
        };
        if plain.len() < low || plain.len() > high {
            self.error(wrong_args(name, span));
            return Type::Unknown;
        }
        if name == "sum" {
            return self.sum(args, &plain, span);
        }
        let result = self.builtin_result(name, &plain, span);
        let keyed = args.iter().any(|arg| arg.name.as_deref() == Some("key"));
        if matches!(name, "min" | "max") && !keyed {
            self.needs_order(name, &result, span);
        }
        result
    }

    /// `min` and `max` compare the items with `<`, so the items need an order.
    fn needs_order(&mut self, name: &str, item: &Type, span: Span) {
        let item = self.checker.settled(item);
        if item.is_known()
            && let Err((why, fix)) = self.fits_bound(&item, "Ordered")
        {
            let message = format!("`{name}` compares its items with `<`, but {why}");
            self.error(Diagnostic::new(message, span).with_help(fix));
        }
    }

    fn builtin_result(&mut self, name: &str, plain: &[(Type, Span)], span: Span) -> Type {
        let needs_values =
            stdlib::function(name).map_or(NEEDS_VALUES.contains(&name), |(_, f)| f.needs_values);
        if needs_values && !plain.iter().all(|(ty, at)| self.usable(ty, *at)) {
            return Type::Unknown;
        }
        if let Some((module, _)) = stdlib::function(name) {
            return (module.result)(self, name, plain, span);
        }
        let types: Vec<Type> = plain.iter().map(|(ty, _)| ty.clone()).collect();
        let (first, at) = plain.first().cloned().unwrap_or((Type::Unit, span));
        match name {
            "copy" => first,
            "Ok" => Type::result(first, self.checker.fresh(span, Hole::OkError)),
            "Err" => Type::result(self.checker.fresh(span, Hole::ErrValue), first),
            "range" => {
                if !types.iter().all(|ty| self.checker.fits(&Type::Int, ty)) {
                    self.error(Diagnostic::new("`range` takes one to three ints", span));
                }
                Type::list(Type::Int)
            }
            "str" => Type::Str,
            "bytes" => {
                if let Some((items, at)) = plain.first()
                    && !self.checker.fits(&Type::list(Type::Int), items)
                {
                    self.error(mismatch("`bytes`'s argument", &Type::list(Type::Int), items, *at));
                }
                Type::Bytes
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
                let number = if name == "int" { Type::Int } else { Type::Float };
                // Text might not be a number, so reading one can fail.
                if self.checker.settled(&first) == Type::Str {
                    return Type::result(number, Type::named("InvalidValue"));
                }
                number
            }
            "round" => self.math(name, &types, span),
            _ => self.collection_builtin(name, plain, span),
        }
    }

    /// `round` and `std.math`'s functions, which take numbers.
    pub(crate) fn math(&mut self, name: &str, types: &[Type], span: Span) -> Type {
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
            "round" | "math.floor" | "math.ceil" => Type::Int,
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
            "enumerate" => {
                let pairs = Type::Tuple(vec![Type::Int, items(self)]);
                if plain.iter().any(|(ty, _)| matches!(ty, Type::Iterator(_))) {
                    Type::Iterator(Box::new(pairs))
                } else {
                    Type::list(pairs)
                }
            }
            "zip" => {
                let first = items(self);
                let second = plain.get(1).map_or(Type::Unknown, |(ty, at)| self.item_type(ty, *at));
                let pairs = Type::Tuple(vec![first, second]);
                if plain.iter().any(|(ty, _)| matches!(ty, Type::Iterator(_))) {
                    Type::Iterator(Box::new(pairs))
                } else {
                    Type::list(pairs)
                }
            }
            _ => Type::Unknown,
        }
    }

    /// `str.from_code(n)`: the character with a code, which fails for a number that isn't one.
    pub(crate) fn char_from_code(&mut self, args: &[Arg], span: Span) -> Type {
        let plain = self.builtin_args("from_code", args, &[], &[], span);
        match plain.as_slice() {
            [(code, at)] => {
                if !self.checker.fits(&Type::Int, code) {
                    self.error(mismatch("the code", &Type::Int, code, *at));
                }
            }
            _ => self.error(wrong_args("from_code", span)),
        }
        Type::result(Type::Str, Type::named("InvalidValue"))
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
        Type::List(item)
        | Type::Set(item)
        | Type::Iterator(item)
        | Type::IdMap(item)
        | Type::Dict(item, _) => item.as_ref().clone(),
        Type::Tuple(items) if items.windows(2).all(|pair| pair.first() == pair.last()) => {
            items.first().cloned().unwrap_or(Type::Unknown)
        }
        Type::Str => Type::Str,
        Type::Bytes => Type::Int,
        _ => Type::Unknown,
    }
}
