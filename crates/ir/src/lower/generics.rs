//! Generics. A generic function or type is copied for each set of types a program uses it
//! with, so the IR only ever holds concrete types. A copy of `Deque[T]` for ints is the
//! struct `Deque[int]`, and its `append` is the function `Deque.append[int]`. The typed tree
//! keeps saying `T`, so types read from it go through what `T` stands for in this copy.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};

use fennec_check::copies::{Copies, Refusal};
use fennec_check::{Signature, Type};

use super::{Context, Lowered, Lowering};
use fennec_syntax::{Expr, ExprKind, Stmt};

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
    /// Why the last copy asked for was refused, for the call that asked to say.
    refused: RefCell<Option<String>>,
    /// The copies made, as the rules every walk over them shares count them.
    copies: RefCell<Copies>,
}

impl Instances {
    /// Why the last copy asked for was refused, if it was, and forgets it.
    pub(super) fn take_refusal(&self) -> Option<String> {
        self.refused.borrow_mut().take()
    }
}

/// Whether a type still has a type parameter in it.
pub(super) fn mentions_param(ty: &Type) -> bool {
    match ty {
        Type::Param(_) | Type::Generic(..) => true,
        Type::List(item)
        | Type::Buffer(item)
        | Type::Array(item, _)
        | Type::Iterator(item, _)
        | Type::Set(item)
        | Type::Optional(item)
        | Type::Table(item)
        | Type::Id(item) => mentions_param(item),
        Type::Dict(first, second) | Type::Result(first, second) => {
            mentions_param(first) || mentions_param(second)
        }
        Type::Tuple(items) | Type::Union(items) => items.iter().any(mentions_param),
        Type::Arrow(params, returns) => params.iter().any(mentions_param) || mentions_param(returns),
        _ => false,
    }
}

/// Whether a signature only makes sense once its type parameters are known.
pub(super) fn is_generic(signature: &Signature) -> bool {
    !signature.generics.is_empty() || leaves_open(signature)
}

/// Whether its parameters or what it gives back still mention a type parameter.
fn leaves_open(signature: &Signature) -> bool {
    signature.params.iter().any(|param| mentions_param(&param.ty)) || mentions_param(&signature.returns)
}

impl Context<'_> {
    /// A type with its parameters filled in, and each generic type as its copy.
    pub(super) fn concrete(&self, ty: &Type, subst: &Subst) -> Type {
        let inner = |item: &Type| Box::new(self.concrete(item, subst));
        match ty {
            Type::Param(name) => subst.get(name).cloned().unwrap_or_else(|| ty.clone()),
            Type::List(item) => Type::List(inner(item)),
            Type::Buffer(item) => Type::Buffer(inner(item)),
            Type::Array(item, size) => Type::Array(inner(item), *size),
            Type::Iterator(item, failure) => {
                Type::Iterator(inner(item), failure.as_ref().map(|failure| inner(failure)))
            }
            Type::Set(item) => Type::Set(inner(item)),
            // As the checker has it, an optional of an optional stays one: `V?` with `V` a `str?`
            // is a `str??`, which tells "nothing there" from "a `None` there".
            Type::Optional(item) => match self.concrete(item, subst) {
                held @ (Type::None | Type::Unknown) => held,
                held => Type::Optional(Box::new(held)),
            },
            Type::Table(item) => Type::Table(inner(item)),
            Type::Id(item) => Type::Id(inner(item)),
            Type::Dict(key, value) => Type::Dict(inner(key), inner(value)),
            // What fails with nothing, as a `T fails E` whose `E` a function that can't fail filled
            // in, is the `T`, as the checker has it.
            Type::Result(ok, error) => match self.concrete(error, subst) {
                error if error.fails_with_nothing() => self.concrete(ok, subst),
                error => Type::Result(inner(ok), Box::new(error)),
            },
            Type::Tuple(items) => Type::Tuple(items.iter().map(|item| self.concrete(item, subst)).collect()),
            // A generic type among a union's members, like `Secret[str]` in `str | Secret[str]`, is
            // its copy, as anywhere else.
            Type::Union(members) => {
                Type::Union(members.iter().map(|member| self.concrete(member, subst)).collect())
            }
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

    /// A type with each copy of a generic type in it written as that type given its arguments, as
    /// the copy named `Pair[int]` is the generic `Pair` given `int`: what the checker's signatures
    /// know, so a copy's shape is its fields' with the arguments filled in.
    pub(super) fn as_written(&self, ty: &Type) -> Type {
        ty.replaced(&mut |part| {
            let Type::Named(name) = part else { return None };
            let instance = self.type_instance(name)?;
            let def = self.generic_types.get(instance.base.as_str())?;
            let args: Option<Vec<Type>> = def
                .generics
                .iter()
                .map(|generic| instance.subst.get(&generic.name).map(|arg| self.as_written(arg)))
                .collect();
            Some(Type::Generic(instance.base.clone(), args?))
        })
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

    /// Asks for a copy of a generic function, and gives its name. `None` for one that would
    /// never finish, a generic that calls itself with a bigger type each time, with why kept for
    /// the call to say.
    fn request(&self, base: &str, subst: Subst) -> Option<String> {
        let refused = |refusal: Refusal| {
            *self.instances.refused.borrow_mut() = Some(refusal.said(base));
            None
        };
        if let Some(refusal) = self.instances.copies.borrow_mut().too_deep(base, &subst) {
            return refused(refusal);
        }
        let args: Vec<String> = subst.values().map(ToString::to_string).collect();
        let name = format!("{base}[{}]", args.join(", "));
        let mut seen = self.instances.seen.borrow_mut();
        if !seen.contains(&name) {
            if let Err(refusal) = self.instances.copies.borrow_mut().make(base, self.functions.len()) {
                return refused(refusal);
            }
            seen.insert(name.clone());
            let instance = FunctionInstance { base: base.to_owned(), name: name.clone(), subst };
            self.instances.waiting.borrow_mut().push(instance);
        }
        Some(name)
    }

    /// What a function's type gives back says of its type parameters, given a function's: a
    /// function that can't fail, given where one that `fails E` goes, says `E` is nothing, as
    /// the checker has it.
    fn unify_returns(&self, pattern: &Type, actual: &Type, subst: &mut Subst) {
        match (pattern, actual) {
            (Type::Result(ok, error), plain)
                if !matches!(plain, Type::Result(..) | Type::Unknown | Type::Var(_)) =>
            {
                self.unify(ok, plain, subst);
                if let Type::Param(name) = &**error {
                    subst.entry(name.clone()).or_insert_with(Type::nothing_failed);
                }
            }
            _ => self.unify(pattern, actual, subst),
        }
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
            | (Type::Iterator(first, _), Type::Iterator(second, _))
            | (Type::Buffer(first), Type::Buffer(second))
            | (Type::Array(first, _), Type::Array(second, _))
            | (Type::Optional(first), Type::Optional(second))
            | (Type::Table(first), Type::Table(second))
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
                self.unify_returns(returns, other_returns, subst);
            }
            // A function passed by name, like `one` for `make: () -> T fails E`: its signature
            // says what `T` and `E` are.
            (Type::Arrow(params, returns), Type::Function(key)) => {
                let Some(signature) = self.signatures.get(key) else { return };
                for (param, other) in params.iter().zip(&signature.params) {
                    self.unify(param, &other.ty, subst);
                }
                self.unify_returns(returns, &signature.returns, subst);
            }
            // A union given where a union goes, as a handler's passed on to `serve`: its function
            // member says what the wanted one's types stand for, as the checker has it, and with
            // none, the wanted one's failure is nothing.
            (Type::Union(members), Type::Union(others)) => match (one_arrow(members), one_arrow(others)) {
                (Some(arrow), Some(other)) => self.unify(arrow, other, subst),
                (Some(_), None) => fails_with_nothing(members, subst),
                _ => {}
            },
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
        if let Some((_, ty)) = self.retyped.iter().rev().find(|(at, _)| *at == span) {
            return Some(ty.clone());
        }
        if let Some(ty) = self.body.expr(span) {
            return Some(self.concrete(ty));
        }
        self.copied.iter().rev().find_map(|(_, body, subst)| {
            let ty = body.expr(span)?;
            Some(match subst {
                Some(subst) => self.context.concrete(ty, subst),
                None => self.concrete(ty),
            })
        })
    }

    /// What a call to `key` is to, given the types of its arguments and of what's kept: the
    /// function's name in the IR, and its signature with everything filled in.
    pub(super) fn resolve(
        &self,
        key: &str,
        given: &[Option<Type>],
        returns: Option<&Type>,
    ) -> Option<(String, Signature)> {
        self.resolve_written(key, &[], given, returns)
    }

    /// What a call to `key` says when its copy can't be made: why it was refused, or that the
    /// call doesn't say what its types stand for.
    pub(super) fn unresolved(&self, key: &str) -> String {
        match self.context.instances.take_refusal() {
            Some(why) => format!("its call to `{key}` {why}"),
            None => format!("its call to `{key}` doesn't say what its types stand for"),
        }
    }

    /// The same, with the types written in brackets at the call, which come first.
    pub(super) fn resolve_written(
        &self,
        key: &str,
        written: &[Type],
        given: &[Option<Type>],
        returns: Option<&Type>,
    ) -> Option<(String, Signature)> {
        let context = self.context;
        context.instances.refused.borrow_mut().take();
        let base = context.base_key(key);
        let raw = context.signatures.get(&base)?;
        let mut subst = key
            .rsplit_once('.')
            .and_then(|(owner, _)| context.type_instance(owner))
            .map(|instance| instance.subst)
            .unwrap_or_default();
        subst.extend(raw.generics.iter().cloned().zip(written.iter().cloned()));
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
        if leaves_open(&filled) || raw.generics.iter().any(|generic| !subst.contains_key(generic)) {
            return None;
        }
        Some((context.request(&base, subst)?, filled))
    }
}

impl Lowering<'_> {
    /// `for f in fields(T):`, written out once for each of `T`'s fields in this copy: `f.name` as
    /// the field's name and `f.get(x)` as `x`'s field, of the field's own type. A type with no
    /// fields, like `int`, gives none.
    pub(super) fn each_field(&mut self, names: &[String], iter: &Expr, body: &[Stmt]) -> Lowered<()> {
        let (Some(of), [name]) = (fennec_check::fields_of(iter), names) else {
            return Err("it loops over `fields` in a way that isn't lowered yet".to_owned());
        };
        let ty = match self.subst.get(of) {
            Some(ty) => ty.clone(),
            None => Type::Named(of.to_owned()),
        };
        let fields = match &ty {
            Type::Named(owner) => {
                self.context.signatures.structure(owner).map(|found| found.fields.clone()).unwrap_or_default()
            }
            Type::Generic(..) => return Err("it loops over the fields of a generic struct".to_owned()),
            _ => Vec::new(),
        };
        let each = fennec_check::each_field_type(iter.span.start);
        for (field, field_ty) in fields {
            let mut written = body.to_vec();
            fennec_syntax::visit::in_stmts(&mut written, &mut |expr| field_written(expr, name, &field));
            let outer = self.subst.insert(each.clone(), field_ty);
            let lowered = self.block(&written, super::Tail::Drop);
            match outer {
                Some(outer) => self.subst.insert(each.clone(), outer),
                None => self.subst.remove(&each),
            };
            lowered?;
        }
        Ok(())
    }
}

/// `f.name` as the field's name, and `f.get(x)` as `x.field`, where `f` is the loop's name for it.
fn field_written(expr: &mut Expr, held: &str, field: &str) {
    let named = |object: &Expr| matches!(&object.kind, ExprKind::Name(found) if found == held);
    match &mut expr.kind {
        ExprKind::Attr { object, name, .. } if named(object) && name == "name" => {
            expr.kind = ExprKind::Str(field.to_owned());
        }
        ExprKind::Call { callee, args } => {
            if let ExprKind::Attr { object, name, .. } = &callee.kind
                && named(object)
                && name == "get"
                && let [given] = args.as_mut_slice()
            {
                let at = given.value.span;
                let read = std::mem::replace(&mut given.value, Expr { kind: ExprKind::None, span: at });
                expr.kind =
                    ExprKind::Attr { object: Box::new(read), name: field.to_owned(), optional: false };
            }
        }
        _ => {}
    }
}

/// A union's function member, when it has just one.
fn one_arrow(members: &[Type]) -> Option<&Type> {
    let mut arrows = members.iter().filter(|member| matches!(member, Type::Arrow(..)));
    match (arrows.next(), arrows.next()) {
        (Some(arrow), None) => Some(arrow),
        _ => None,
    }
}

/// A union's function member's failure stands for nothing, as when a `Router` is given for
/// `Router | (Request) -> Response fails E`.
fn fails_with_nothing(members: &[Type], subst: &mut Subst) {
    for member in members {
        if let Type::Arrow(_, returns) = member
            && let Type::Result(_, error) = &**returns
            && let Type::Param(name) = &**error
        {
            subst.entry(name.clone()).or_insert_with(Type::nothing_failed);
        }
    }
}
