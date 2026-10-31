//! Types as the checker sees them. How they combine (a value that can come from several
//! places, or a value going where a type is expected) is in `infer`, since that can settle
//! types still being worked out.

use std::fmt;

/// The type of a value.
///
/// A program that checks without errors has no `Unknown` or `Var` anywhere in its
/// signatures or typed tree: every type is fully known.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Type {
    /// A type the checker couldn't work out because of a mistake it has already reported.
    Unknown,
    /// What a function returns when it has nothing to return.
    Unit,
    /// The type of `None` on its own.
    None,
    Bool,
    Int,
    Float,
    Str,
    /// Binary data: a run of bytes, each an int from 0 to 255.
    Bytes,
    List(Box<Type>),
    Set(Box<Type>),
    Dict(Box<Type>, Box<Type>),
    /// A generator: items one at a time, as they're asked for, once.
    Iterator(Box<Type>),
    Tuple(Vec<Type>),
    /// `T?`: a `T`, or `None`.
    Optional(Box<Type>),
    /// What something that can fail gives back: a value, or an error of the second type.
    Result(Box<Type>, Box<Type>),
    /// A struct, enum or error, or a capability like `Stdout`.
    Named(String),
    /// A struct with type parameters, and the type given for each: `Deque[int]`.
    Generic(String, Vec<Type>),
    /// An `Id` from an `IdMap` of the given type.
    Id(Box<Type>),
    IdMap(Box<Type>),
    /// A function, by its name in the signature table.
    Function(String),
    /// An arrow function: the types of its parameters, and what it gives back.
    Arrow(Vec<Type>, Box<Type>),
    /// A type parameter of a generic function or type: `T` in `def first[T](items: list[T]) -> T`
    /// or `struct Deque[T]:`. Each use fills in one type for it.
    Param(String),
    /// One of several error types, in name order: what something gives back when it can
    /// fail in more than one way.
    Errors(Vec<String>),
    /// A type still being worked out inside one function, like the items of `[]` before
    /// anything is added. It's settled, or reported, before the function's check ends.
    Var(u32),
}

impl Type {
    pub(crate) fn list(item: Type) -> Type {
        Type::List(Box::new(item))
    }

    pub(crate) fn optional(inner: Type) -> Type {
        match inner {
            Type::Optional(_) | Type::None | Type::Unknown => inner,
            other => Type::Optional(Box::new(other)),
        }
    }

    pub(crate) fn result(value: Type, error: Type) -> Type {
        Type::Result(Box::new(value), Box::new(error))
    }

    pub(crate) fn named(name: &str) -> Type {
        Type::Named(name.to_owned())
    }

    /// Whether this might be `None`, so it can't be used as a value yet.
    pub(crate) fn maybe_none(&self) -> bool {
        matches!(self, Type::None | Type::Optional(_))
    }

    /// The value inside an optional, once it's known not to be `None`.
    pub(crate) fn without_none(&self) -> Type {
        match self {
            Type::Optional(inner) => inner.as_ref().clone(),
            Type::None => Type::Unknown,
            other => other.clone(),
        }
    }

    /// The short name the interpreter uses for a value of this type, as in "this is a list".
    pub(crate) fn kind(&self) -> String {
        match self {
            Type::Unknown | Type::Var(_) => "value".to_owned(),
            Type::Unit => "nothing".to_owned(),
            Type::None => "None".to_owned(),
            Type::Bool => "bool".to_owned(),
            Type::Int => "int".to_owned(),
            Type::Float => "float".to_owned(),
            Type::Str => "str".to_owned(),
            Type::Bytes => "bytes".to_owned(),
            Type::List(_) => "list".to_owned(),
            Type::Set(_) => "set".to_owned(),
            Type::Iterator(_) => "Iterator".to_owned(),
            Type::Dict(..) => "dict".to_owned(),
            Type::Tuple(_) => "tuple".to_owned(),
            Type::Optional(inner) => format!("{}?", inner.kind()),
            Type::Result(..) => "result".to_owned(),
            Type::Named(name) | Type::Param(name) | Type::Generic(name, _) => name.clone(),
            Type::Id(_) => "Id".to_owned(),
            Type::IdMap(_) => "IdMap".to_owned(),
            Type::Function(_) | Type::Arrow(..) => "function".to_owned(),
            Type::Errors(_) => "error".to_owned(),
        }
    }

    /// Whether this is a list, str, bytes, dict or set: something with a length.
    pub(crate) fn is_collection(&self) -> bool {
        matches!(self, Type::List(_) | Type::Str | Type::Bytes | Type::Dict(..) | Type::Set(_))
    }

    /// Whether every part of this type is known: no `Unknown` and no `Var` inside.
    pub fn is_known(&self) -> bool {
        match self {
            Type::Unknown | Type::Var(_) => false,
            other => other.children().iter().all(|child| child.is_known()),
        }
    }

    /// The types directly inside this one.
    pub(crate) fn children(&self) -> Vec<&Type> {
        match self {
            Type::List(inner)
            | Type::Set(inner)
            | Type::Iterator(inner)
            | Type::Optional(inner)
            | Type::Id(inner)
            | Type::IdMap(inner) => {
                vec![inner]
            }
            Type::Dict(a, b) | Type::Result(a, b) => vec![a, b],
            Type::Tuple(items) | Type::Generic(_, items) => items.iter().collect(),
            Type::Arrow(params, ret) => params.iter().chain(std::iter::once(ret.as_ref())).collect(),
            _ => Vec::new(),
        }
    }

    /// The same type with every part rewritten by `change`, innermost parts first. Parts
    /// `change` gives `None` for stay as they are.
    pub(crate) fn map(&self, change: &mut dyn FnMut(&Type) -> Option<Type>) -> Type {
        let mut inner = |ty: &Type| Box::new(ty.map(change));
        let mapped = match self {
            Type::List(item) => Type::List(inner(item)),
            Type::Set(item) => Type::Set(inner(item)),
            Type::Iterator(item) => Type::Iterator(inner(item)),
            Type::Optional(item) => Type::optional(*inner(item)),
            Type::Id(item) => Type::Id(inner(item)),
            Type::IdMap(item) => Type::IdMap(inner(item)),
            Type::Dict(a, b) => Type::Dict(inner(a), inner(b)),
            Type::Result(a, b) => Type::Result(inner(a), inner(b)),
            Type::Tuple(items) => Type::Tuple(items.iter().map(|item| *inner(item)).collect()),
            Type::Generic(name, items) => {
                Type::Generic(name.clone(), items.iter().map(|item| *inner(item)).collect())
            }
            Type::Arrow(params, ret) => {
                Type::Arrow(params.iter().map(|param| *inner(param)).collect(), inner(ret))
            }
            other => other.clone(),
        };
        change(&mapped).unwrap_or(mapped)
    }

    /// The name of the struct, enum, error or capability this is a value of.
    /// The name a type's methods are kept under: a struct's or enum's own name, or `str`'s,
    /// for the methods the standard library gives it. More built-in types join as the
    /// standard library gives them methods.
    pub(crate) fn method_owner(&self) -> Option<&str> {
        match self {
            Type::Str => Some("str"),
            Type::List(_) => Some("list"),
            Type::Dict(..) => Some("dict"),
            Type::Set(_) => Some("set"),
            other => other.type_name(),
        }
    }

    /// A generic built-in type with its arguments, like `list[T]`, for the methods the
    /// standard library gives it.
    pub(crate) fn built_in_generic(name: &str, args: &[Type]) -> Option<Type> {
        match (name, args) {
            ("list", [item]) => Some(Type::list(item.clone())),
            ("dict", [key, value]) => Some(Type::Dict(Box::new(key.clone()), Box::new(value.clone()))),
            ("set", [item]) => Some(Type::Set(Box::new(item.clone()))),
            _ => None,
        }
    }

    /// The arguments of a generic type: `int` in `list[int]` or in `Deque[int]`.
    pub(crate) fn type_args(&self) -> Vec<Type> {
        match self {
            Type::Generic(_, args) => args.clone(),
            Type::List(item) | Type::Set(item) => vec![item.as_ref().clone()],
            Type::Dict(key, value) => vec![key.as_ref().clone(), value.as_ref().clone()],
            _ => Vec::new(),
        }
    }

    /// The built-in type `self` is in a method the standard library gives it, like `str`.
    pub(crate) fn built_in(name: &str) -> Option<Type> {
        (name == "str").then_some(Type::Str)
    }

    pub(crate) fn type_name(&self) -> Option<&str> {
        match self {
            Type::Named(name) | Type::Generic(name, _) => Some(name),
            _ => None,
        }
    }

    /// Replaces type parameters by name: `T` with `int` in `Deque[T]`.
    pub(crate) fn substitute(&self, given: &[(String, Type)]) -> Type {
        self.map(&mut |part| match part {
            Type::Param(name) => given.iter().find(|(param, _)| param == name).map(|(_, ty)| ty.clone()),
            _ => None,
        })
    }

    /// The type as it's written in source, like `list[int]` or `Enemy?`. `None` for a type
    /// that has no written form, like one of several errors, or nothing at all.
    pub(crate) fn source(&self) -> Option<String> {
        let all = |items: &[Type]| {
            items.iter().map(Type::source).collect::<Option<Vec<_>>>().map(|items| items.join(", "))
        };
        Some(match self {
            Type::Int | Type::Float | Type::Bool | Type::Str | Type::Bytes => self.kind(),
            Type::None => "None".to_owned(),
            Type::List(item) => format!("list[{}]", item.source()?),
            Type::Set(item) => format!("set[{}]", item.source()?),
            Type::Iterator(item) => format!("Iterator[{}]", item.source()?),
            Type::Dict(key, value) => format!("dict[{}, {}]", key.source()?, value.source()?),
            Type::Tuple(items) => format!("({})", all(items)?),
            Type::Optional(inner) => format!("{}?", inner.source()?),
            Type::Result(value, error) => format!("Result[{}, {}]", value.source()?, error.source()?),
            Type::Named(name) | Type::Param(name) => name.clone(),
            Type::Generic(name, args) => format!("{name}[{}]", all(args)?),
            Type::Id(item) => format!("Id[{}]", item.source()?),
            Type::IdMap(item) => format!("IdMap[{}]", item.source()?),
            Type::Arrow(params, ret) => format!("({}) -> {}", all(params)?, ret.source()?),
            Type::Unknown | Type::Unit | Type::Function(_) | Type::Errors(_) | Type::Var(_) => return None,
        })
    }

    /// Whether `var` appears anywhere in this type.
    pub(crate) fn mentions(&self, var: u32) -> bool {
        *self == Type::Var(var) || self.children().iter().any(|child| child.mentions(var))
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::List(item) => write!(f, "list[{item}]"),
            Type::Set(item) => write!(f, "set[{item}]"),
            Type::Iterator(item) => write!(f, "Iterator[{item}]"),
            Type::Dict(key, value) => write!(f, "dict[{key}, {value}]"),
            Type::Tuple(items) => {
                let items: Vec<String> = items.iter().map(ToString::to_string).collect();
                write!(f, "({})", items.join(", "))
            }
            Type::Optional(inner) => write!(f, "{inner}?"),
            Type::Result(value, _) => write!(f, "result of {value}"),
            Type::Errors(names) => f.write_str(&names.join(" | ")),
            Type::Arrow(params, ret) => {
                let params: Vec<String> = params.iter().map(ToString::to_string).collect();
                write!(f, "({}) -> {ret}", params.join(", "))
            }
            Type::Id(item) => write!(f, "Id[{item}]"),
            Type::Generic(name, items) => {
                let items: Vec<String> = items.iter().map(ToString::to_string).collect();
                write!(f, "{name}[{}]", items.join(", "))
            }
            Type::IdMap(item) => write!(f, "IdMap[{item}]"),
            other => f.write_str(&other.kind()),
        }
    }
}

/// "a list" or "an int": the type's short name with the right article.
pub(crate) fn a(ty: &Type) -> String {
    match ty {
        Type::Unit => "nothing".to_owned(),
        Type::None => "None".to_owned(),
        other => article(&other.to_string()),
    }
}

pub(crate) fn article(name: &str) -> String {
    // A plural type name reads best on its own: "this is bytes". What a function gives back
    // when it gives back nothing has no type name to show.
    if name == "bytes" || name == "nothing" {
        return name.to_owned();
    }
    if name.starts_with(['a', 'e', 'i', 'o', 'u', 'A', 'E', 'I', 'O', 'U']) {
        format!("an {name}")
    } else {
        format!("a {name}")
    }
}
