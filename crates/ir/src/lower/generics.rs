//! Generics. A generic function or type is copied for each set of types a program uses it
//! with, so the IR only ever holds concrete types. A copy of `Deque[T]` for ints is the
//! struct `Deque[int]`, and its `append` is the function `Deque.append[int]`. The typed tree
//! keeps saying `T`, so types read from it go through what `T` stands for in this copy.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};

use fennec_check::{Signature, Type};

use super::{Context, Lowering};

/// How many copies of generic functions a program can ask for.
const MOST_COPIES: usize = 2000;

/// How deeply a type a generic function is copied for can nest.
const DEEPEST_TYPE: usize = 32;

/// How deeply a type nests.
fn depth(ty: &Type) -> usize {
    let deepest = |types: &[Type]| types.iter().map(depth).max().unwrap_or(0);
    1 + match ty {
        Type::List(item) | Type::Set(item) | Type::Optional(item) | Type::IdMap(item) | Type::Id(item) => {
            depth(item)
        }
        Type::Dict(first, second) | Type::Result(first, second) => depth(first).max(depth(second)),
        Type::Tuple(items) | Type::Generic(_, items) => deepest(items),
        Type::Arrow(params, returns) => deepest(params).max(depth(returns)),
        _ => 0,
    }
}

/// What each type parameter stands for.
pub(super) type Subst = BTreeMap<String, Type>;

/// A copy of a generic type.
#[derive(Clone)]
pub(super) struct TypeInstance {
    pub(super) base: String,
    pub(super) subst: Subst,
}

/// A function to lower: its key in the typed tree, what it's called in the IR, and what its
/// type parameters stand for. The two names only differ for a copy of something generic.
#[derive(Clone)]
pub(super) struct FunctionInstance {
    pub(super) base: String,
    pub(super) name: String,
    pub(super) subst: Subst,
}

impl FunctionInstance {
    pub(super) fn plain(key: &str) -> Self {
        FunctionInstance { base: key.to_owned(), name: key.to_owned(), subst: Subst::new() }
    }
}

/// The copies wanted so far.
#[derive(Default)]
pub(super) struct Instances {
    pub(super) types: RefCell<BTreeMap<String, TypeInstance>>,
    pub(super) waiting: RefCell<Vec<FunctionInstance>>,
    seen: RefCell<HashSet<String>>,
}

/// Whether a type still has a type parameter in it.
pub(super) fn mentions_param(ty: &Type) -> bool {
    match ty {
        Type::Param(_) | Type::Generic(..) => true,
        Type::List(item) | Type::Set(item) | Type::Optional(item) | Type::IdMap(item) | Type::Id(item) => {
            mentions_param(item)
        }
        Type::Dict(first, second) | Type::Result(first, second) => {
            mentions_param(first) || mentions_param(second)
        }
        Type::Tuple(items) => items.iter().any(mentions_param),
        Type::Arrow(params, returns) => params.iter().any(mentions_param) || mentions_param(returns),
        _ => false,
    }
}

/// Whether a signature only makes sense once its type parameters are known.
pub(super) fn is_generic(signature: &Signature) -> bool {
    signature.params.iter().any(|param| mentions_param(&param.ty)) || mentions_param(&signature.returns)
}

impl Context<'_> {
    /// A type with its parameters filled in, and each generic type as its copy.
    pub(super) fn concrete(&self, ty: &Type, subst: &Subst) -> Type {
        let inner = |item: &Type| Box::new(self.concrete(item, subst));
        match ty {
            Type::Param(name) => subst.get(name).cloned().unwrap_or_else(|| ty.clone()),
            Type::List(item) => Type::List(inner(item)),
            Type::Iterator(item) => Type::Iterator(inner(item)),
            Type::Set(item) => Type::Set(inner(item)),
            Type::Optional(item) => Type::Optional(inner(item)),
            Type::IdMap(item) => Type::IdMap(inner(item)),
            Type::Id(item) => Type::Id(inner(item)),
            Type::Dict(key, value) => Type::Dict(inner(key), inner(value)),
            Type::Result(ok, error) => Type::Result(inner(ok), inner(error)),
            Type::Tuple(items) => Type::Tuple(items.iter().map(|item| self.concrete(item, subst)).collect()),
            Type::Arrow(params, returns) => {
                Type::Arrow(params.iter().map(|param| self.concrete(param, subst)).collect(), inner(returns))
            }
            Type::Generic(name, args) => {
                let args: Vec<Type> = args.iter().map(|arg| self.concrete(arg, subst)).collect();
                self.type_copy(name, args)
            }
            other => other.clone(),
        }
    }

    /// The copy of a generic type for these arguments, when they're all known.
    fn type_copy(&self, name: &str, args: Vec<Type>) -> Type {
        let def = self.generic_types.get(name);
        let known = !args.iter().any(mentions_param);
        let written = Type::Generic(name.to_owned(), args.clone());
        let (Some(def), true) = (def, known) else { return written };
        let copy = written.to_string();
        let params = def.generics.iter().map(|generic| generic.name.clone());
        let subst: Subst = params.zip(args).collect();
        let instance = TypeInstance { base: name.to_owned(), subst };
        self.instances.types.borrow_mut().entry(copy.clone()).or_insert(instance);
        Type::Named(copy)
    }

    pub(super) fn type_instance(&self, name: &str) -> Option<TypeInstance> {
        self.instances.types.borrow().get(name).cloned()
    }

    /// The key of a method in the typed tree: `Deque.len` for `Deque[int].len`.
    pub(super) fn base_key(&self, key: &str) -> String {
        match key.rsplit_once('.').and_then(|(owner, method)| Some((self.type_instance(owner)?, method))) {
            Some((instance, method)) => format!("{}.{method}", instance.base),
            None => key.to_owned(),
        }
    }

    /// A signature with its type parameters filled in.
    pub(super) fn instantiate(&self, signature: &Signature, subst: &Subst) -> Signature {
        let mut filled = signature.clone();
        for param in &mut filled.params {
            param.ty = self.concrete(&param.ty, subst);
        }
        filled.returns = self.concrete(&signature.returns, subst);
        filled
    }

    /// Asks for a copy of a generic function, and gives its name. `None` once a program has
    /// asked for too many, as one does when a generic calls itself with a bigger type each
    /// time, which would never finish.
    fn request(&self, base: &str, subst: Subst) -> Option<String> {
        if subst.values().any(|ty| depth(ty) > DEEPEST_TYPE) {
            return None;
        }
        let args: Vec<String> = subst.values().map(ToString::to_string).collect();
        let name = format!("{base}[{}]", args.join(", "));
        let mut seen = self.instances.seen.borrow_mut();
        if !seen.contains(&name) {
            if seen.len() >= MOST_COPIES {
                return None;
            }
            seen.insert(name.clone());
            let instance = FunctionInstance { base: base.to_owned(), name: name.clone(), subst };
            self.instances.waiting.borrow_mut().push(instance);
        }
        Some(name)
    }

    /// Works out what the type parameters in `pattern` stand for, given a value's type.
    fn unify(&self, pattern: &Type, actual: &Type, subst: &mut Subst) {
        match (pattern, actual) {
            (Type::Param(name), actual) => {
                if !matches!(actual, Type::None | Type::Unknown | Type::Var(_)) && !subst.contains_key(name) {
                    subst.insert(name.clone(), actual.clone());
                }
            }
            (Type::List(first), Type::List(second))
            | (Type::Set(first), Type::Set(second))
            | (Type::Optional(first), Type::Optional(second))
            | (Type::IdMap(first), Type::IdMap(second))
            | (Type::Id(first), Type::Id(second)) => self.unify(first, second, subst),
            (Type::Optional(first), other) => self.unify(first, other, subst),
            (Type::Dict(key, value), Type::Dict(other_key, other_value))
            | (Type::Result(key, value), Type::Result(other_key, other_value)) => {
                self.unify(key, other_key, subst);
                self.unify(value, other_value, subst);
            }
            (Type::Tuple(items), Type::Tuple(others)) => {
                for (item, other) in items.iter().zip(others) {
                    self.unify(item, other, subst);
                }
            }
            (Type::Arrow(params, returns), Type::Arrow(others, other_returns)) => {
                for (param, other) in params.iter().zip(others) {
                    self.unify(param, other, subst);
                }
                self.unify(returns, other_returns, subst);
            }
            (Type::Generic(name, args), Type::Named(copy)) => {
                let Some(instance) = self.type_instance(copy).filter(|instance| instance.base == *name)
                else {
                    return;
                };
                let Some(def) = self.generic_types.get(name.as_str()) else { return };
                for (arg, generic) in args.iter().zip(&def.generics) {
                    if let Some(other) = instance.subst.get(&generic.name) {
                        self.unify(arg, other, subst);
                    }
                }
            }
            _ => {}
        }
    }
}

impl Lowering<'_> {
    /// A type read from the typed tree, as it is in this copy.
    pub(super) fn concrete(&self, ty: &Type) -> Type {
        self.context.concrete(ty, &self.subst)
    }

    /// The type of the expression at a span, as it is in this copy.
    pub(super) fn type_at(&self, span: fennec_syntax::Span) -> Option<Type> {
        let copied = self.copied.iter().rev().map(|(_, body)| *body);
        std::iter::once(self.body).chain(copied).find_map(|body| body.expr(span)).map(|ty| self.concrete(ty))
    }

    /// What a call to `key` is to, given the types of its arguments and of what's kept: the
    /// function's name in the IR, and its signature with everything filled in.
    pub(super) fn resolve(
        &self,
        key: &str,
        given: &[Option<Type>],
        returns: Option<&Type>,
    ) -> Option<(String, Signature)> {
        let context = self.context;
        let base = context.base_key(key);
        let raw = context.signatures.get(&base)?;
        let mut subst = key
            .rsplit_once('.')
            .and_then(|(owner, _)| context.type_instance(owner))
            .map(|instance| instance.subst)
            .unwrap_or_default();
        for (param, given) in raw.params.iter().zip(given) {
            if let Some(given) = given {
                context.unify(&param.ty, given, &mut subst);
            }
        }
        if let Some(returns) = returns {
            context.unify(&raw.returns, returns, &mut subst);
        }
        let filled = context.instantiate(raw, &subst);
        if !is_generic(raw) {
            return Some((base, filled));
        }
        if is_generic(&filled) {
            return None;
        }
        Some((context.request(&base, subst)?, filled))
    }
}
