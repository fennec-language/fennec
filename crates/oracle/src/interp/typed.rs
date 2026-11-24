//! The types a generic function's type parameters stand for where it's called. The compiler builds
//! a generic function once for each set of types it's called with, so code that depends on a type
//! rather than a value, like `x.serialize(ref out)` of an `int?` that holds an int, runs as built
//! for that type. The oracle runs one copy, so each call notes what its type parameters, and those
//! of the type whose method it is, are there: from the type of what it's called on, of each
//! argument, and of what it gives back, as the checker typed them where the call is written.

use fennec_check::Type;
use fennec_syntax::{Arg, Expr, FnDef, Span, TypeExpr};

use super::Interp;
use super::json::filled;
use crate::value::Value;

/// Type parameters by name, each with the type it stands for in one call.
pub(crate) type Bound = Vec<(String, Type)>;

impl Interp {
    /// The type the checker gave what's written at `span`, with the type parameters of the calls
    /// it's in filled in.
    pub(crate) fn static_type(&self, span: Span) -> Option<Type> {
        self.program.type_at(span).map(|ty| self.resolved(ty))
    }

    /// `ty` with the type parameters of the running call filled in, and of the calls a nested
    /// function was written in.
    pub(crate) fn resolved(&self, ty: &Type) -> Type {
        let mut ty = ty.clone();
        let mut at = self.frames.get(self.cur);
        while let Some(frame) = at {
            if !frame.types.is_empty() {
                ty = filled(&ty, &frame.types);
            }
            at = frame.parent.and_then(|parent| self.frames.iter().rfind(|frame| frame.id == parent));
        }
        ty
    }

    /// How many levels of optional a type written as a function's return is, in the running
    /// call: 1 for `int?`, 2 for `int??`, and 2 for `T?` where `T` stands for an `int?`.
    pub(crate) fn levels_written(&self, written: &TypeExpr) -> usize {
        let within = match (written.name.as_str(), written.args.as_slice()) {
            (fennec_syntax::OPTIONAL, [held]) => 1 + self.levels_written(held),
            ("fails", [returns, _]) => self.levels_written(returns),
            (name, []) => levels(&self.resolved(&Type::Param(name.to_owned()))),
            _ => 0,
        };
        within + usize::from(written.optional)
    }

    /// `value`, returned as what's written at `span`, as the running call gives it back: a
    /// `None` returned as a `T?` where the call gives a `T??` is held in a `Some`.
    pub(crate) fn returned(&self, value: Value, span: Span) -> Value {
        let Some(returns) = self.frames.get(self.cur).map(|frame| frame.returns) else { return value };
        if returns < 2 || value.levels().is_none() {
            return value;
        }
        let given = match self.static_type(span) {
            Some(ty @ Type::Optional(_)) => levels(&ty),
            _ => return value,
        };
        (given..returns).fold(value, |value, _| Value::Some(Box::new(value)))
    }

    /// `value`, read by name at `span`, as the type it has there: a `T??` that's been narrowed to
    /// a `T?`, by `is not None`, is what it holds.
    pub(crate) fn as_read(&self, value: Value, span: Span) -> Value {
        if !matches!(value, Value::Some(_)) {
            return value;
        }
        let Some(ty @ Type::Optional(_)) = self.static_type(span) else { return value };
        let wanted = levels(&ty);
        let mut value = value;
        while value.levels().is_some_and(|levels| levels >= wanted && levels > 0) {
            value = value.unwrapped();
        }
        value
    }

    /// What the type parameters of `def`, and of the type or `extend` it's in, stand for at a call
    /// written as `call`: on `object`, when it's a method, with `args`. A type parameter none of
    /// them settles is left out, so code that needs it is refused.
    pub(crate) fn bound_at_call(
        &self,
        def: &FnDef,
        object: Option<&Expr>,
        args: &[Arg],
        call: &Expr,
    ) -> Bound {
        let owner = self.program.owner_generics(def);
        if def.generics.is_empty() && owner.is_empty() {
            return Bound::new();
        }
        let mut bound = Bound::new();
        if let Some(object) = object
            && let Some(ty) = self.static_type(object.span)
        {
            bound_by_owner(owner, &ty, &mut bound);
        }
        let generics: Vec<&str> = def
            .generics
            .iter()
            .map(|generic| generic.name.as_str())
            .chain(owner.iter().map(String::as_str))
            .collect();
        let params: Vec<&fennec_syntax::Param> = def.params.iter().filter(|param| !param.is_self).collect();
        let mut next = 0;
        for arg in args {
            let param = if let Some(name) = &arg.name {
                params.iter().find(|param| param.name == *name)
            } else {
                next += 1;
                params.get(next - 1)
            };
            if let Some(written) = param.and_then(|param| param.ty.as_ref())
                && let Some(ty) = self.static_type(arg.value.span)
            {
                unify(written, &ty, &generics, &mut bound);
            }
        }
        if let Some(written) = &def.ret
            && let Some(ty) = self.static_type(call.span)
        {
            // A call that can fail is typed as what it gives back or the error.
            let given = match ty {
                Type::Result(value, _) => *value,
                other => other,
            };
            unify(written, &given, &generics, &mut bound);
        }
        bound
    }
}

/// The type parameters of the type or `extend` a method is in, from the type of what it's called
/// on: `T` of `Deque[T]` from a `Deque[str]`, or of `extend list[T]` from a `list[int]`.
fn bound_by_owner(owner: &[String], ty: &Type, bound: &mut Bound) {
    let given: Vec<Type> = match ty {
        Type::Generic(_, args) => args.clone(),
        Type::List(item) | Type::Set(item) | Type::Optional(item) | Type::Buffer(item) => {
            vec![(**item).clone()]
        }
        Type::Dict(key, value) => vec![(**key).clone(), (**value).clone()],
        _ => return,
    };
    if given.len() != owner.len() {
        return;
    }
    for (name, ty) in owner.iter().zip(given) {
        if settled(&ty) {
            bound.push((name.clone(), ty));
        }
    }
}

/// Matches a type as written, which may name type parameters among `generics`, with the type
/// the checker gave what's there, noting what each type parameter it reaches stands for.
fn unify(written: &TypeExpr, ty: &Type, generics: &[&str], bound: &mut Bound) {
    let ty = if written.optional {
        match ty {
            Type::Optional(inner) => inner.as_ref(),
            Type::None => return,
            other => other,
        }
    } else {
        ty
    };
    if written.args.is_empty() && generics.contains(&written.name.as_str()) {
        if settled(ty) && !bound.iter().any(|(name, _)| *name == written.name) {
            bound.push((written.name.clone(), ty.clone()));
        }
        return;
    }
    match (written.name.as_str(), written.args.as_slice(), ty) {
        ("list", [item], Type::List(inner))
        | ("set", [item], Type::Set(inner))
        | ("Buffer", [item], Type::Buffer(inner))
        | ("Table", [item], Type::Table(inner))
        | ("Id", [item], Type::Id(inner))
        | ("Optional", [item], Type::Optional(inner)) => unify(item, inner, generics, bound),
        ("dict", [key, value], Type::Dict(key_ty, value_ty)) => {
            unify(key, key_ty, generics, bound);
            unify(value, value_ty, generics, bound);
        }
        ("tuple", items, Type::Tuple(types)) if items.len() == types.len() => {
            for (item, ty) in items.iter().zip(types) {
                unify(item, ty, generics, bound);
            }
        }
        (name, args, Type::Generic(own, given)) if name == own && args.len() == given.len() => {
            for (arg, ty) in args.iter().zip(given) {
                unify(arg, ty, generics, bound);
            }
        }
        _ => {}
    }
}

/// How many levels of optional a type is: 0 for `int`, 1 for `int?`, 2 for `int??`.
pub(crate) fn levels(ty: &Type) -> usize {
    match ty {
        Type::Optional(inner) => 1 + levels(inner),
        _ => 0,
    }
}

/// Whether a type is known all the way down: no type parameter left in it, and nothing the
/// checker couldn't work out.
pub(crate) fn settled(ty: &Type) -> bool {
    match ty {
        Type::Param(_) | Type::Unknown | Type::Var(_) => false,
        Type::List(inner)
        | Type::Set(inner)
        | Type::Optional(inner)
        | Type::Buffer(inner)
        | Type::Table(inner)
        | Type::Id(inner)
        | Type::Iterator(inner, _)
        | Type::Array(inner, _) => settled(inner),
        Type::Dict(key, value) | Type::Result(key, value) => settled(key) && settled(value),
        Type::Tuple(items) | Type::Union(items) | Type::Generic(_, items) => items.iter().all(settled),
        Type::Arrow(params, returned) => params.iter().all(settled) && settled(returned),
        _ => true,
    }
}
