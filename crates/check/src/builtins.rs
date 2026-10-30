//! The builtins' signatures: Python's builtins that Fennec keeps, `Ok`, `Err`, `replace` and
//! `ignore`, and the methods of built-in values. Methods that change a value need it held in
//! a `mut` place.

use fennec_syntax::{Arg, Diagnostic, Expr, Span};

use crate::function::FnCx;
use crate::messages::{mismatch, needless_mut, no_method, wrong_args};
use crate::types::{Type, accepts, article, join, join_all};

/// Each builtin, and how many plain arguments it takes (at least, at most).
/// Names with a dot belong to a module, like `json.loads`, and exist only once imported.
const BUILTINS: [(&str, usize, usize); 32] = [
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
    ("range", 1, 3),
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
const NEEDS_VALUES: [&str; 11] =
    ["abs", "ceil", "cos", "float", "floor", "int", "len", "range", "round", "sin", "sqrt"];

pub(crate) fn is_builtin(name: &str) -> bool {
    BUILTINS.iter().any(|(builtin, ..)| *builtin == name)
}

/// The modules with a function of this name, like `json` and `toml` for `loads`.
pub(crate) fn modules_with(name: &str) -> Vec<&'static str> {
    BUILTINS
        .iter()
        .filter_map(|(builtin, ..)| builtin.split_once('.').filter(|(_, function)| *function == name))
        .map(|(module, _)| module)
        .collect()
}

/// A method of a built-in value: whether it changes the value, what it gives back, and how
/// many arguments it takes.
struct Method {
    changes: bool,
    returns: Type,
    arity: (usize, usize),
    /// The types an arrow function passed to it receives, like the item for `remove_where`.
    receives: Vec<Type>,
}

fn reads(returns: Type, low: usize, high: usize) -> Method {
    Method { changes: false, returns, arity: (low, high), receives: Vec::new() }
}

fn changes(returns: Type, low: usize, high: usize) -> Method {
    Method { changes: true, returns, arity: (low, high), receives: Vec::new() }
}

impl Method {
    /// An arrow function passed to this method receives an item.
    fn receiving(self, item: &Type) -> Method {
        Method { receives: vec![item.clone()], ..self }
    }
}

fn method(ty: &Type, name: &str) -> Option<Method> {
    Some(match (ty, name) {
        (Type::List(_), "append" | "extend" | "remove") | (Type::Set(_), "add" | "discard" | "remove") => {
            changes(Type::Unit, 1, 1)
        }
        (Type::List(_), "insert") => changes(Type::Unit, 2, 2),
        (Type::List(_), "clear" | "reverse") | (Type::Dict(..) | Type::Set(_) | Type::IdMap(_), "clear") => {
            changes(Type::Unit, 0, 0)
        }
        (Type::List(item) | Type::IdMap(item), "remove_where") => changes(Type::Unit, 1, 1).receiving(item),
        (Type::List(item), "sort") => changes(Type::Unit, 0, 0).receiving(item),
        (Type::List(item), "pop") => changes(item.as_ref().clone(), 0, 1),
        (Type::List(_), "index") => reads(Type::optional(Type::Int), 1, 1),
        (Type::List(_), "count") => reads(Type::Int, 1, 1),
        (Type::Dict(_, value), "pop") => changes(value.as_ref().clone(), 1, 1),
        (Type::Dict(_, item) | Type::IdMap(item), "get") => {
            reads(Type::optional(item.as_ref().clone()), 1, 2)
        }
        (Type::Dict(key, _), "keys") => reads(Type::list(key.as_ref().clone()), 0, 0),
        (Type::Dict(_, item) | Type::IdMap(item), "values") => reads(Type::list(item.as_ref().clone()), 0, 0),
        (Type::Dict(key, value), "items") => {
            reads(Type::list(Type::Tuple(vec![*key.clone(), *value.clone()])), 0, 0)
        }
        (Type::IdMap(item), "insert") => changes(Type::Id(item.clone()), 1, 1),
        (Type::IdMap(item), "remove") => changes(Type::optional(item.as_ref().clone()), 1, 1),
        (Type::IdMap(item), "modify") => changes(Type::Bool, 2, 2).receiving(item),
        (Type::IdMap(item), "items") => {
            reads(Type::list(Type::Tuple(vec![Type::Id(item.clone()), *item.clone()])), 0, 0)
        }
        (Type::IdMap(item), "ids") => reads(Type::list(Type::Id(item.clone())), 0, 0),
        (Type::Str, _) => return str_method(name),
        (Type::Named(capability), _) => return capability_method(capability, name),
        _ => return None,
    })
}

fn str_method(name: &str) -> Option<Method> {
    match name {
        "upper" | "lower" | "strip" | "lstrip" | "rstrip" => Some(reads(Type::Str, 0, 0)),
        "split" => Some(reads(Type::list(Type::Str), 0, 1)),
        "lines" => Some(reads(Type::list(Type::Str), 0, 0)),
        "startswith" | "endswith" => Some(reads(Type::Bool, 1, 1)),
        "isdigit" => Some(reads(Type::Bool, 0, 0)),
        "replace" => Some(reads(Type::Str, 2, 2)),
        "find" => Some(reads(Type::optional(Type::Int), 1, 1)),
        "join" => Some(reads(Type::Str, 1, 1)),
        _ => None,
    }
}

fn capability_method(capability: &str, name: &str) -> Option<Method> {
    match (capability, name) {
        ("Stdout", "print" | "write") => Some(reads(Type::Unit, 0, usize::MAX)),
        ("Fs", "exists") => Some(reads(Type::Bool, 1, 1)),
        ("Fs", "read_text") => Some(reads(Type::result(Type::Str, Type::named("IoError")), 1, 1)),
        ("Fs", "write_text") => Some(reads(Type::result(Type::Unit, Type::named("IoError")), 2, 2)),
        _ => None,
    }
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
        // `table.get(key, default)` gives the default instead of None.
        if let (Type::Dict(_, value), [_, (default, _)]) = (ty, plain.as_slice()) {
            return join(value, default);
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
                    if !accepts(&Type::Int, &ty) {
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
        let (first, at) = plain.first().cloned().unwrap_or((Type::Unknown, span));
        if NEEDS_VALUES.contains(&name) && !plain.iter().all(|(ty, at)| self.usable(ty, *at)) {
            return Type::Unknown;
        }
        let items = |cx: &mut Self| cx.item_type(&first, at);
        match name {
            "Ok" => Type::result(first, Type::Unknown),
            "Err" => Type::result(Type::Unknown, first),
            "len" => {
                if !first.is_collection() && !matches!(first, Type::Tuple(_) | Type::IdMap(_) | Type::Unknown)
                {
                    self.error(Diagnostic::new(format!("{} has no length", article(&first.kind())), span));
                }
                Type::Int
            }
            "range" => {
                if types.iter().any(|ty| !matches!(ty, Type::Int | Type::Unknown)) {
                    self.error(Diagnostic::new("`range` takes one to three ints", span));
                }
                Type::list(Type::Int)
            }
            "json.loads" | "toml.loads" | "json.dumps" | "toml.dumps" => self.format_result(name, &first, at),
            "str" => Type::Str,
            "round" if types.len() == 2 => Type::Float,
            "int" | "floor" | "ceil" | "round" => Type::Int,
            "float" | "sqrt" | "sin" | "cos" => Type::Float,
            "abs" => first,
            "ignore" => Type::Unit,
            "min" | "max" if types.len() > 1 => join_all(&types),
            "min" | "max" | "sum" => items(self),
            "any" | "all" => {
                let item = items(self);
                if !matches!(item, Type::Bool | Type::Unknown) {
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
                let a = items(self);
                let b = plain.get(1).map_or(Type::Unknown, |(ty, at)| self.item_type(ty, *at));
                Type::list(Type::Tuple(vec![a, b]))
            }
            _ => Type::Unknown,
        }
    }

    /// `std.json` and `std.toml`. Read text gives values of any type, so its type is unknown,
    /// except that a TOML document is always a table.
    fn format_result(&mut self, name: &str, first: &Type, at: Span) -> Type {
        let (format, function) = name.split_once('.').unwrap_or_default();
        if function == "loads" {
            if !accepts(&Type::Str, first) {
                self.error(mismatch("the text to read", &Type::Str, first, at));
            }
            let read = if format == "toml" {
                Type::Dict(Box::new(Type::Str), Box::new(Type::Unknown))
            } else {
                Type::Unknown
            };
            return Type::result(read, Type::named("ParseError"));
        }
        let format = format.to_uppercase();
        let is_table = matches!(first, Type::Dict(..) | Type::Unknown)
            || self.checker.program.kind_of(first) == Some(crate::program::TypeKind::Struct);
        if format == "TOML" && !is_table {
            self.error(Diagnostic::new(
                format!(
                    "a TOML document is a table, so `toml.dumps` needs a dict or a struct, not {}",
                    article(&first.kind())
                ),
                at,
            ));
        } else if matches!(
            first,
            Type::Set(_) | Type::Function(_) | Type::Id(_) | Type::IdMap(_) | Type::Result(..)
        ) {
            self.error(mismatch_holds(&format, first, at));
        }
        Type::Str
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
        let Type::Named(type_name) = ty else { return ty.clone() };
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
            let expected = self.checker.field_type(type_name, None, name);
            if !accepts(&expected, actual) {
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
        Type::Tuple(items) => join_all(items),
        Type::Str => Type::Str,
        _ => Type::Unknown,
    }
}

fn mismatch_holds(format: &str, ty: &Type, span: Span) -> Diagnostic {
    if matches!(ty, Type::Result(..)) {
        return crate::messages::unhandled_result(span);
    }
    Diagnostic::new(format!("{format} can't hold {}", article(&ty.kind())), span)
}
