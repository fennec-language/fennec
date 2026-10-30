//! Types as the checker sees them, and the two ways they combine: `join`, for a value that
//! can come from several places, and `accepts`, for a value going where a type is expected.

use std::fmt;

/// The type of a value. `Unknown` stands for anything the checker can't work out yet; every
/// check involving it stays silent, so the checker never reports a mistake it isn't sure of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Type {
    Unknown,
    /// What a function returns when it has nothing to return.
    Unit,
    /// The type of `None` on its own.
    None,
    Bool,
    Int,
    Float,
    Str,
    List(Box<Type>),
    Set(Box<Type>),
    Dict(Box<Type>, Box<Type>),
    Tuple(Vec<Type>),
    /// `T?`: a `T`, or `None`.
    Optional(Box<Type>),
    /// What something that can fail gives back: a value, or an error of the second type.
    Result(Box<Type>, Box<Type>),
    /// A struct, enum or error, or a capability like `Stdout`.
    Named(String),
    /// An `Id` from an `IdMap` of the given type.
    Id(Box<Type>),
    IdMap(Box<Type>),
    /// A function, by its name in the signature table.
    Function(String),
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
            Type::Unknown => "value".to_owned(),
            Type::Unit => "nothing".to_owned(),
            Type::None => "None".to_owned(),
            Type::Bool => "bool".to_owned(),
            Type::Int => "int".to_owned(),
            Type::Float => "float".to_owned(),
            Type::Str => "str".to_owned(),
            Type::List(_) => "list".to_owned(),
            Type::Set(_) => "set".to_owned(),
            Type::Dict(..) => "dict".to_owned(),
            Type::Tuple(_) => "tuple".to_owned(),
            Type::Optional(inner) => format!("{}?", inner.kind()),
            Type::Result(..) => "result".to_owned(),
            Type::Named(name) => name.clone(),
            Type::Id(_) => "Id".to_owned(),
            Type::IdMap(_) => "IdMap".to_owned(),
            Type::Function(_) => "function".to_owned(),
        }
    }

    /// Whether this is a list, str, dict or set: something with a length.
    pub(crate) fn is_collection(&self) -> bool {
        matches!(self, Type::List(_) | Type::Str | Type::Dict(..) | Type::Set(_))
    }
}

/// The type of a value that comes from either `a` or `b`. Anything that doesn't fit together
/// is `Unknown`, except that `None` and a `T` make a `T?`.
pub(crate) fn join(a: &Type, b: &Type) -> Type {
    match (a, b) {
        _ if a == b => a.clone(),
        (Type::None, other) | (other, Type::None) => Type::optional(other.clone()),
        (Type::Optional(a), other) | (other, Type::Optional(a)) => Type::optional(join(a, other)),
        (Type::List(a), Type::List(b)) => Type::list(join(a, b)),
        (Type::Set(a), Type::Set(b)) => Type::Set(Box::new(join(a, b))),
        (Type::Dict(k1, v1), Type::Dict(k2, v2)) => {
            Type::Dict(Box::new(join(k1, k2)), Box::new(join(v1, v2)))
        }
        (Type::Tuple(a), Type::Tuple(b)) if a.len() == b.len() => {
            Type::Tuple(a.iter().zip(b).map(|(a, b)| join(a, b)).collect())
        }
        (Type::Result(v1, e1), Type::Result(v2, e2)) => Type::result(join(v1, v2), join(e1, e2)),
        _ => Type::Unknown,
    }
}

/// Joins every type in a list; an empty list is `Unknown`.
pub(crate) fn join_all<'t>(types: impl IntoIterator<Item = &'t Type>) -> Type {
    let mut types = types.into_iter();
    let Some(first) = types.next() else { return Type::Unknown };
    types.fold(first.clone(), |joined, ty| join(&joined, ty))
}

/// Whether a value of type `actual` can go where `expected` is wanted. Anything unknown fits.
pub(crate) fn accepts(expected: &Type, actual: &Type) -> bool {
    match (expected, actual) {
        (Type::Unknown, _) | (_, Type::Unknown) => true,
        _ if expected == actual => true,
        (Type::Optional(inner), actual) => accepts(inner, &actual.without_none()),
        (Type::List(a), Type::List(b))
        | (Type::Set(a), Type::Set(b))
        | (Type::Id(a), Type::Id(b))
        | (Type::IdMap(a), Type::IdMap(b))
        | (Type::Result(a, _), Type::Result(b, _)) => accepts(a, b),
        (Type::Dict(k1, v1), Type::Dict(k2, v2)) => accepts(k1, k2) && accepts(v1, v2),
        (Type::Tuple(a), Type::Tuple(b)) => a.len() == b.len() && a.iter().zip(b).all(|(a, b)| accepts(a, b)),
        _ => false,
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::List(item) => write!(f, "list[{item}]"),
            Type::Set(item) => write!(f, "set[{item}]"),
            Type::Dict(key, value) => write!(f, "dict[{key}, {value}]"),
            Type::Tuple(items) => {
                let items: Vec<String> = items.iter().map(ToString::to_string).collect();
                write!(f, "({})", items.join(", "))
            }
            Type::Optional(inner) => write!(f, "{inner}?"),
            Type::Result(value, _) => write!(f, "result of {value}"),
            Type::Id(item) => write!(f, "Id[{item}]"),
            Type::IdMap(item) => write!(f, "IdMap[{item}]"),
            other => f.write_str(&other.kind()),
        }
    }
}

/// "a list" or "an int": the type's short name with the right article.
pub(crate) fn a(ty: &Type) -> String {
    article(&ty.to_string())
}

pub(crate) fn article(name: &str) -> String {
    if name.starts_with(['a', 'e', 'i', 'o', 'u', 'A', 'E', 'I', 'O', 'U']) {
        format!("an {name}")
    } else {
        format!("a {name}")
    }
}
