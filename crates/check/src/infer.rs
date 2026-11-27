//! Types still being worked out, and the two ways types combine. `fits` is for a value going
//! where a type is expected, and `join` for a value that can come from several places. Both
//! settle type variables as they go: `[]` is a list of a variable until something is added.
//!
//! Variables live only while a function is checked. One that's still open when the
//! function's check ends means the program doesn't say enough, and is reported where it was
//! made, with the annotation that would settle it.

use fennec_syntax::{Diagnostic, Span};

use crate::checker::Checker;
use crate::types::Type;

/// Why a type variable was made, for explaining what to write when nothing settles it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Hole {
    /// The items of `[]`.
    EmptyList,
    /// The items of `set()`.
    EmptySet,
    /// The keys and values of `{}`.
    EmptyDict,
    /// What a name first given `None` holds otherwise.
    None,
    /// The error `Ok(...)` could have held.
    OkError,
    /// The value `Err(...)` could have held.
    ErrValue,
    /// A type parameter of a generic function, in one call to it.
    TypeParam(String),
    /// A part of something unpacked before its type is known.
    Unpacked,
}

pub(crate) struct Slot {
    pub(crate) value: Option<Type>,
    pub(crate) span: Span,
    pub(crate) hole: Hole,
    /// The name it was first given to, for suggesting where to write its type.
    pub(crate) name: Option<String>,
}

impl Checker {
    /// A new type variable.
    pub(crate) fn fresh(&mut self, span: Span, hole: Hole) -> Type {
        let id = u32::try_from(self.vars.len()).unwrap_or(u32::MAX);
        self.vars.push(Slot { value: None, span, hole, name: None });
        Type::Var(id)
    }

    /// A type with every settled variable replaced by what it was settled to.
    pub(crate) fn settled(&self, ty: &Type) -> Type {
        ty.map(&mut |part| match part {
            Type::Var(id) => {
                self.slot(*id).and_then(|slot| slot.value.as_ref()).map(|value| self.settled(value))
            }
            _ => None,
        })
    }

    fn slot(&self, id: u32) -> Option<&Slot> {
        self.vars.get(usize::try_from(id).ok()?)
    }

    fn bind(&mut self, id: u32, ty: &Type) -> bool {
        if *ty == Type::Var(id) {
            return true;
        }
        if ty.mentions(id) {
            return false;
        }
        if let Some(slot) = usize::try_from(id).ok().and_then(|at| self.vars.get_mut(at)) {
            slot.value = Some(ty.clone());
        }
        true
    }

    /// Whether two generators fail the same way: neither can fail, or both fail with the same
    /// error. An error already reported as unknown is the same as any.
    pub(crate) fn same_failure(&self, failure: Option<&Type>, other: Option<&Type>) -> bool {
        match (failure, other) {
            (None, None) => true,
            (Some(failure), Some(other)) => {
                let (failure, other) = (self.settled(failure), self.settled(other));
                failure == other || matches!(failure, Type::Unknown) || matches!(other, Type::Unknown)
            }
            _ => false,
        }
    }

    /// Whether a value of type `actual` can go where `expected` is wanted, settling any
    /// variables that make it fit. Something already reported as unknown fits anything.
    pub(crate) fn fits(&mut self, expected: &Type, actual: &Type) -> bool {
        let (expected, actual) = (self.settled(expected), self.settled(actual));
        match (&expected, &actual) {
            (Type::Unknown, _) | (_, Type::Unknown) => true,
            (Type::Var(id), other) | (other, Type::Var(id)) => self.bind(*id, other),
            _ if expected == actual => true,
            // A `LiteralStr` goes anywhere a `str` does; a `str` never goes where one is asked for.
            (Type::Str, Type::LiteralStr) | (Type::Optional(_), Type::None) => true,
            (Type::Optional(inner), Type::Optional(other)) => self.fits(inner, other),
            (Type::Optional(inner), other) => self.fits(inner, other),
            (Type::Errors(names), Type::Named(name)) => names.contains(name),
            (Type::Errors(names), Type::Errors(others)) => others.iter().all(|name| names.contains(name)),
            (Type::List(a), Type::List(b))
            | (Type::Buffer(a), Type::Buffer(b))
            | (Type::Set(a), Type::Set(b))
            | (Type::Id(a), Type::Id(b))
            | (Type::Table(a), Type::Table(b)) => self.fits(a, b),
            // What a generator fails with is where its failure is read from, so it has to be the
            // same error, not one of several that holds it: the layouts differ.
            (Type::Iterator(a, failure), Type::Iterator(b, other)) => {
                self.same_failure(failure.as_deref(), other.as_deref()) && self.fits(a, b)
            }
            (Type::Dict(k1, v1), Type::Dict(k2, v2)) | (Type::Result(k1, v1), Type::Result(k2, v2)) => {
                self.fits(k1, k2) && self.fits(v1, v2)
            }
            (Type::Array(a, n), Type::Array(b, m)) if n == m => self.fits(a, b),
            (Type::Tuple(a), Type::Tuple(b)) if a.len() == b.len() => {
                a.iter().zip(b).all(|(a, b)| self.fits(a, b))
            }
            // A generic type's arguments are exactly what's wanted, not only something that fits:
            // a `shared` value or a `Sender` made with one is written and read through the other,
            // so a `Sender[int]` given where a `Sender[int?]` goes could be sent a `None`.
            (Type::Generic(n1, a), Type::Generic(n2, b)) if n1 == n2 && a.len() == b.len() => {
                a.iter().zip(b).all(|(a, b)| self.fits(a, b) && self.holds_the_same(a, b))
            }
            // A function named as a value, like `handle` in `server.serve(handle)`, fits a function
            // type that takes and gives back what its signature does.
            (Type::Arrow(params, returns), Type::Function(key)) => self.function_fits(params, returns, key),
            (Type::Arrow(p1, r1), Type::Arrow(p2, r2)) if p1.len() == p2.len() => {
                p1.iter().zip(p2).all(|(a, b)| self.fits(b, a)) && self.fits(r1, r2)
            }
            // A member goes where its union is wanted, and so does a union of some of its members.
            (Type::Union(members), Type::Union(others)) => others.iter().all(|other| members.contains(other)),
            // A function goes where a union's one function member is wanted, if it fits that, as a
            // named one does where `Router | (Request) -> Response` goes. With two, which one an
            // arrow is can't be told.
            (Type::Union(members), other @ (Type::Function(_) | Type::Arrow(..)))
                if !members.contains(other) =>
            {
                match Type::Union(members.clone()).arrow_member() {
                    Some(arrow) => self.fits(&arrow, other),
                    None => false,
                }
            }
            (Type::Union(members), other) => members.contains(other),
            _ => false,
        }
    }

    /// Whether `actual`, which fits `expected`, is also all that `expected` can hold, as what's
    /// written through has to be: a `list[int]` passed with `ref` where a `list[int?]` goes could
    /// be given a `None` it can't hold. Something already reported as unknown is the same as any,
    /// and a function named as a value is the same as the function type it fits.
    pub(crate) fn holds_the_same(&mut self, expected: &Type, actual: &Type) -> bool {
        let (expected, actual) = (self.settled(expected), self.settled(actual));
        match (&expected, &actual) {
            (Type::Unknown | Type::Var(_), _) | (_, Type::Unknown | Type::Var(_)) => true,
            _ if expected == actual => true,
            // A named function is one value, so it's the same as each function type it fits.
            (Type::Arrow(..), Type::Function(_)) => self.fits(&expected, &actual),
            (Type::Function(_), Type::Arrow(..)) => {
                self.fits(&expected, &actual) && self.fits(&actual, &expected)
            }
            (Type::List(a), Type::List(b))
            | (Type::Buffer(a), Type::Buffer(b))
            | (Type::Set(a), Type::Set(b))
            | (Type::Id(a), Type::Id(b))
            | (Type::Table(a), Type::Table(b))
            | (Type::Optional(a), Type::Optional(b)) => self.holds_the_same(a, b),
            (Type::Array(a, n), Type::Array(b, m)) => n == m && self.holds_the_same(a, b),
            (Type::Dict(k1, v1), Type::Dict(k2, v2)) | (Type::Result(k1, v1), Type::Result(k2, v2)) => {
                self.holds_the_same(k1, k2) && self.holds_the_same(v1, v2)
            }
            (Type::Tuple(a), Type::Tuple(b)) if a.len() == b.len() => {
                a.iter().zip(b).all(|(a, b)| self.holds_the_same(a, b))
            }
            (Type::Generic(n1, a), Type::Generic(n2, b)) if n1 == n2 && a.len() == b.len() => {
                a.iter().zip(b).all(|(a, b)| self.holds_the_same(a, b))
            }
            _ => false,
        }
    }

    /// Whether the function `key` can go where a `(params) -> returns` is wanted. A generic
    /// one, or one with a `mut` parameter, can't: a call through the type can't say which types
    /// it stands for or pass anything `mut`.
    fn function_fits(&mut self, params: &[Type], returns: &Type, key: &str) -> bool {
        let Some(signature) = self.signature(key) else { return false };
        signature.generics.is_empty()
            && signature.params.len() == params.len()
            && signature
                .params
                .iter()
                .zip(params)
                .all(|(param, ty)| !param.is_mut && self.fits(&param.ty, ty))
            && self.fits(returns, &signature.returns)
    }

    /// The type of a value that comes from either `a` or `b`, or `None` when they don't fit
    /// together. `None` and a `T` make a `T?`, and different errors make one of them.
    pub(crate) fn join(&mut self, a: &Type, b: &Type) -> Option<Type> {
        let (a, b) = (self.settled(a), self.settled(b));
        Some(match (&a, &b) {
            _ if a == b => a,
            (Type::Unknown, other) | (other, Type::Unknown) => other.clone(),
            (Type::Var(_), Type::None) | (Type::None, Type::Var(_)) => {
                Type::Optional(Box::new(if a == Type::None { b } else { a }))
            }
            (Type::Var(id), other) | (other, Type::Var(id)) => {
                let other = other.clone();
                return self.bind(*id, &other).then_some(other);
            }
            (Type::None, other) | (other, Type::None) => Type::or_none(other.clone()),
            (Type::Optional(inner), other) | (other, Type::Optional(inner)) => {
                let other = other.without_none();
                Type::optional(self.join(inner, &other)?)
            }
            (Type::List(a), Type::List(b)) => Type::list(self.join(a, b)?),
            (Type::Array(a, n), Type::Array(b, m)) if n == m => Type::Array(Box::new(self.join(a, b)?), *n),
            (Type::Set(a), Type::Set(b)) => Type::Set(Box::new(self.join(a, b)?)),
            (Type::Iterator(a, failure), Type::Iterator(b, other))
                if self.same_failure(failure.as_deref(), other.as_deref()) =>
            {
                Type::Iterator(Box::new(self.join(a, b)?), failure.clone().or_else(|| other.clone()))
            }
            (Type::Id(a), Type::Id(b)) => Type::Id(Box::new(self.join(a, b)?)),
            (Type::Table(a), Type::Table(b)) => Type::Table(Box::new(self.join(a, b)?)),
            (Type::Dict(k1, v1), Type::Dict(k2, v2)) => {
                Type::Dict(Box::new(self.join(k1, k2)?), Box::new(self.join(v1, v2)?))
            }
            (Type::Result(v1, e1), Type::Result(v2, e2)) => {
                Type::result(self.join(v1, v2)?, self.join(e1, e2)?)
            }
            (Type::Tuple(a), Type::Tuple(b)) if a.len() == b.len() => Type::Tuple(self.join_each(a, b)?),
            (Type::Generic(n1, a), Type::Generic(n2, b)) if n1 == n2 && a.len() == b.len() => {
                Type::Generic(n1.clone(), self.join_each(a, b)?)
            }
            _ => return self.error_union(&a, &b),
        })
    }

    fn join_each(&mut self, a: &[Type], b: &[Type]) -> Option<Vec<Type>> {
        a.iter().zip(b).map(|(a, b)| self.join(a, b)).collect()
    }

    /// Joins every type in a list. An empty list joins to `None`, like types that don't fit.
    pub(crate) fn join_all<'t>(&mut self, types: impl IntoIterator<Item = &'t Type>) -> Option<Type> {
        let mut types = types.into_iter();
        let first = types.next()?.clone();
        types.try_fold(first, |joined, ty| self.join(&joined, ty))
    }

    /// Two errors, or sets of them, as one set.
    fn error_union(&self, a: &Type, b: &Type) -> Option<Type> {
        let mut names = Vec::new();
        for ty in [a, b] {
            match ty {
                Type::Named(name) if self.is_error_type(ty) => names.push(name.clone()),
                Type::Errors(several) => names.extend(several.iter().cloned()),
                _ => return None,
            }
        }
        names.sort();
        names.dedup();
        Some(Type::Errors(names))
    }

    /// Notes that the variables in a type were given to a name, for messages about them.
    pub(crate) fn name_vars(&mut self, ty: &Type, name: &str) {
        let settled = self.settled(ty);
        settled.map(&mut |part| {
            if let Type::Var(id) = part
                && let Some(slot) = usize::try_from(*id).ok().and_then(|at| self.vars.get_mut(at))
                && slot.name.is_none()
            {
                slot.name = Some(name.to_owned());
            }
            None
        });
    }

    /// The variables made since `first` that nothing settled, with where each was made.
    pub(crate) fn open_since(&self, first: usize, types: &[Type]) -> Vec<(u32, Span, Hole, Option<String>)> {
        let mut open: Vec<(u32, Span, Hole, Option<String>)> = Vec::new();
        for ty in types {
            let settled = self.settled(ty);
            settled.map(&mut |part| {
                if let Type::Var(id) = part
                    && usize::try_from(*id).is_ok_and(|at| at >= first)
                    && !open.iter().any(|(seen, ..)| seen == id)
                    && let Some(slot) = self.slot(*id)
                {
                    open.push((*id, slot.span, slot.hole.clone(), slot.name.clone()));
                }
                None
            });
        }
        open.sort_by_key(|(_, span, ..)| (span.start, span.end));
        open
    }
}

/// What to say about a variable nothing settled. `name` is the name it was given to.
pub(crate) fn unsettled(hole: &Hole, span: Span, name: Option<&str>) -> Diagnostic {
    let named = |example: &str, written: &str| match name {
        Some(name) => format!("write the type with the name, like `{name}: {written}`"),
        None => format!("write the type with the name, like `{example}: {written}`"),
    };
    let (message, help) = match hole {
        Hole::EmptyList => ("can't tell what this empty list holds", named("items", "list[int] = []")),
        Hole::EmptySet => ("can't tell what this empty set holds", named("seen", "set[str] = set()")),
        Hole::EmptyDict => ("can't tell what this empty dict holds", named("scores", "dict[str, int] = {}")),
        Hole::None => ("can't tell what this holds when it isn't None", named("target", "Enemy? = None")),
        Hole::OkError => (
            "can't tell what error this result could hold",
            "use it where its error type is known, like the value of a function that fails".to_owned(),
        ),
        Hole::ErrValue => (
            "can't tell what value this result could hold",
            "use it where its value type is known, like the value of a function that returns one".to_owned(),
        ),
        Hole::Unpacked => ("can't tell what these names hold", named("pairs", "list[(str, int)] = []")),
        Hole::TypeParam(name) => {
            return Diagnostic::new(format!("can't tell what `{name}` is in this call"), span)
                .with_help("pass values that show it, or write the type where the result is named");
        }
    };
    Diagnostic::new(message, span).with_help(help)
}
