//! Generic functions and types. Each use of something generic fills in its type parameters:
//! a value's own, like `int` in `Deque[int]`, the types written in brackets, as in
//! `largest[int](xs)`, or else types worked out from the arguments. Each type parameter's
//! bounds say what the types filled in have to be able to do, and inside the generic code,
//! all a value of a type parameter can do.
//!
//! Interfaces are fitted by having their methods. `Eq`, `Hashable`, `Ordered` and `Format`
//! are built in and fitted automatically, wherever `==`, hashing, `<` and printing work.

use fennec_syntax::{Diagnostic, FnDef, Span, TypeExpr};

use crate::function::FnCx;
use crate::infer::Hole;
use crate::types::{Type, a};
use crate::{ParamType, Signature};

/// A type filled in for a type parameter, and the bounds it has to fit.
pub(crate) struct Chosen {
    pub(crate) ty: Type,
    pub(crate) bounds: Vec<String>,
}

impl FnCx<'_> {
    /// A function's signature for one use. A method called on `receiver` takes its type's
    /// parameters from it; the others are `explicit` types, in order, or new variables.
    /// Returns the signature and each type filled in, to check against its bounds.
    pub(crate) fn instantiate(
        &mut self,
        key: &str,
        signature: &Signature,
        receiver: Option<&Type>,
        explicit: &[Type],
        span: Span,
    ) -> (Signature, Vec<Chosen>) {
        let Some(entry) = self.checker.program.functions.get(key) else {
            return (signature.clone(), Vec::new());
        };
        let owner = self.checker.program.owner_params(entry);
        let generics = self.checker.program.generics_of(entry);
        let inherited: Vec<String> = entry.inherited.iter().map(|generic| generic.name.clone()).collect();
        let from_receiver = match receiver {
            Some(receiver) if entry.def.takes_self() => receiver.type_args(),
            _ => Vec::new(),
        };
        let mut given = Vec::new();
        let mut chosen = Vec::new();
        let mut explicit = explicit.iter();
        for (at, generic) in generics.iter().enumerate() {
            if inherited.contains(&generic.name) {
                continue;
            }
            let ty = if at < owner.len()
                && let Some(ty) = from_receiver.get(at)
            {
                ty.clone()
            } else {
                let ty = match explicit.next() {
                    Some(ty) => ty.clone(),
                    None => self.checker.fresh(span, Hole::TypeParam(generic.name.clone())),
                };
                let bounds = generic.bounds.iter().map(|bound| bound.name.clone()).collect();
                chosen.push(Chosen { ty: ty.clone(), bounds });
                ty
            };
            given.push((generic.name.clone(), ty));
        }
        let params = signature
            .params
            .iter()
            .map(|param| ParamType { ty: param.ty.substitute(&given), ..param.clone() })
            .collect();
        let returns = signature.returns.substitute(&given);
        (Signature { params, returns, ..signature.clone() }, chosen)
    }

    /// Checks the types filled in fit their bounds. One not worked out yet is checked when
    /// the function's check ends.
    pub(crate) fn check_bounds(&mut self, chosen: Vec<Chosen>, span: Span) {
        for Chosen { ty, bounds } in chosen {
            let settled = self.checker.settled(&ty);
            if !settled.is_known() {
                self.pending_bounds.push((ty, bounds, span));
                continue;
            }
            for bound in bounds {
                if let Err((why, fix)) = self.fits_bound(&settled, &bound) {
                    let an = if bound.starts_with(['A', 'E', 'I', 'O', 'U']) { "an" } else { "a" };
                    let message = format!("{} isn't {an} `{bound}`: {why}", show(&settled));
                    self.error(Diagnostic::new(message, span).with_help(fix));
                }
            }
        }
    }

    /// Checks the bounds left until the end of the function's check.
    pub(crate) fn check_pending_bounds(&mut self) {
        let pending = std::mem::take(&mut self.pending_bounds);
        for (ty, bounds, span) in pending {
            if self.checker.settled(&ty).is_known() {
                self.check_bounds(vec![Chosen { ty, bounds }], span);
            }
        }
        self.pending_bounds.clear();
    }

    /// Whether a type fits a bound: a built-in interface, or one the program declares.
    pub(crate) fn fits_bound(&mut self, ty: &Type, bound: &str) -> Result<(), (String, String)> {
        if let Type::Param(name) = ty {
            let bounds = self.bounds_of(name);
            let implied =
                bound == "Eq" && bounds.iter().any(|given| given == "Hashable" || given == "Ordered");
            let has_order = bound == "Ordered" && self.bound_method(name, "<").is_some();
            return if bounds.iter().any(|given| given == bound) || implied || has_order || bound == "Format" {
                Ok(())
            } else {
                Err((
                    format!("`{name}` could be any type"),
                    format!("write `{name}: {bound}` where `{name}` is declared"),
                ))
            };
        }
        let fits = match bound {
            "Eq" | "Format" => !matches!(ty, Type::Function(_) | Type::Arrow(..)),
            "Hashable" => self.is_hashable(ty),
            "Ordered" => self.is_ordered(ty),
            _ => return self.fits_interface(ty, bound),
        };
        if fits {
            return Ok(());
        }
        let fix = match bound {
            "Ordered" => format!("define `def <(self, other: ...)` for {}", show(ty)),
            _ => "use a type made of numbers, text, bools, tuples and structs of those".to_owned(),
        };
        Err((format!("{} can't be {}", show(ty), if bound == "Ordered" { "ordered" } else { "hashed" }), fix))
    }

    /// Whether values of a type can be put in order with `<`.
    pub(crate) fn is_ordered(&self, ty: &Type) -> bool {
        match ty {
            Type::Int | Type::Float | Type::Str | Type::Bytes | Type::Bool | Type::Unknown | Type::Var(_) => {
                true
            }
            Type::Tuple(items) => items.iter().all(|item| self.is_ordered(item)),
            Type::List(item) => self.is_ordered(item),
            Type::Param(name) => {
                self.bounds_of(name).iter().any(|bound| bound == "Ordered")
                    || self.bound_method(name, "<").is_some()
            }
            other => other.type_name().is_some_and(|name| self.checker.program.method(name, "<").is_some()),
        }
    }

    /// Whether values of a type can be dict keys and set items: anything but the
    /// collections that change, and what holds them.
    fn is_hashable(&self, ty: &Type) -> bool {
        match ty {
            Type::List(_)
            | Type::Set(_)
            | Type::Dict(..)
            | Type::IdMap(_)
            | Type::Function(_)
            | Type::Arrow(..) => false,
            Type::Param(name) => self.bounds_of(name).iter().any(|bound| bound == "Hashable"),
            other => other.children().iter().all(|child| self.is_hashable(child)),
        }
    }

    /// Whether a type has every method an interface asks for, or why not and what to do.
    pub(crate) fn fits_interface(&mut self, ty: &Type, interface: &str) -> Result<(), (String, String)> {
        let methods = self.checker.program.interfaces.get(interface).map(|found| found.methods.clone());
        for wanted in methods.unwrap_or_default() {
            let expected = self.interface_method(&wanted, ty);
            let wanted_text = signature_text(&wanted);
            let fix = format!("give {} the method `def {wanted_text}`", show(ty));
            let missing = (format!("it has no `{wanted_text}`"), fix.clone());
            let key = ty
                .method_owner()
                .and_then(|owner| self.checker.program.method(owner, &wanted.name).map(|(key, _)| key));
            let actual = if let Some(key) = key {
                let Some(actual) = self.method_signature(ty, &key) else { continue };
                actual
            } else if let Some(actual) = crate::builtins::engine_signature(ty, &wanted.name) {
                actual
            } else {
                return Err(missing);
            };
            let takes = actual.params.len() == expected.params.len()
                && actual.params.iter().zip(&expected.params).skip(1).all(|(actual, expected)| {
                    actual.is_mut == expected.is_mut && self.checker.fits(&actual.ty, &expected.ty)
                });
            if !takes {
                return Err((
                    format!("it has no `{wanted_text}`. Its `{}` takes other arguments", wanted.name),
                    fix,
                ));
            }
            if !self.checker.fits(&expected.returns, &actual.returns) {
                let returns = a(&self.checker.settled(&actual.returns));
                return Err((
                    format!("it has no `{wanted_text}`. Its `{}` returns {returns}", wanted.name),
                    fix,
                ));
            }
        }
        Ok(())
    }

    /// A method's signature for a value of type `receiver`, with its type's parameters
    /// filled in from the value.
    pub(crate) fn method_signature(&mut self, receiver: &Type, key: &str) -> Option<Signature> {
        let signature = self.checker.signature(key)?;
        let (signature, chosen) = self.instantiate(key, &signature, Some(receiver), &[], Span::default());
        self.check_bounds(chosen, Span::default());
        Some(signature)
    }

    /// The interfaces a type parameter in scope has to fit.
    pub(crate) fn bounds_of(&self, name: &str) -> Vec<String> {
        self.generics
            .iter()
            .filter(|generic| generic.name == name)
            .flat_map(|generic| generic.bounds.iter().map(|bound| bound.name.clone()))
            .collect()
    }

    /// The signature an interface gives a method, for a value of type `owner`, which is what
    /// `Self` means in it. A method that doesn't write its return type gives back nothing.
    pub(crate) fn interface_method(&mut self, method: &FnDef, owner: &Type) -> Signature {
        let own = [("Self".to_owned(), owner.clone())];
        let resolve = |cx: &mut Self, ty: &TypeExpr| {
            let written = cx.checker.program.resolve_in(ty, &["Self".to_owned()]);
            written.unwrap_or(Type::Unknown).substitute(&own)
        };
        let params = method
            .params
            .iter()
            .map(|param| ParamType {
                name: param.name.clone(),
                ty: param.ty.as_ref().map_or_else(|| owner.clone(), |ty| resolve(self, ty)),
                is_mut: param.is_mut,
                default: param.default.clone(),
            })
            .collect();
        let returns = method.ret.as_ref().map_or(Type::Unit, |ret| resolve(self, ret));
        Signature { params, returns, can_fail: false, generator: false }
    }

    /// The method an interface among a type parameter's bounds gives it.
    pub(crate) fn bound_method(&self, name: &str, method: &str) -> Option<(String, FnDef)> {
        self.bounds_of(name).into_iter().find_map(|interface| {
            let found =
                self.checker.program.interfaces.get(&interface)?.methods.iter().find(|m| m.name == method)?;
            Some((interface.clone(), found.clone()))
        })
    }
}

/// A method's signature as an interface writes it: `area(self) -> float`.
fn signature_text(def: &FnDef) -> String {
    let params: Vec<String> = def
        .params
        .iter()
        .map(|param| {
            let prefix = if param.is_mut { "mut " } else { "" };
            match &param.ty {
                Some(ty) => format!("{prefix}{}: {}", param.name, crate::program::written(ty)),
                None => format!("{prefix}{}", param.name),
            }
        })
        .collect();
    let ret = def.ret.as_ref().map(|ret| format!(" -> {}", crate::program::written(ret))).unwrap_or_default();
    format!("{}({}){ret}", def.name, params.join(", "))
}

/// A type for a message: `Rock`, or "an int".
pub(crate) fn show(ty: &Type) -> String {
    match ty {
        Type::Named(name) | Type::Param(name) => format!("`{name}`"),
        Type::Generic(..) => format!("`{ty}`"),
        other => a(other),
    }
}
