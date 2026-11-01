//! Fennec's type checker. It finds mistakes before a program runs (unknown names, changes to
//! names that aren't `mut`, wrong arguments, ignored errors, values that might be None,
//! `match`es that miss a case) and works out every function's signature. Every `def` writes
//! the type it gives back, and one with no arrow gives back nothing; the errors a function
//! can fail with are inferred.
//!
//! Each function is checked on its own, against the signatures of the functions it calls.
//! A function is checked the first time something needs its signature, which is when its
//! errors become known.
//!
//! Every type in a program that checks is known. Where the checker can't tell a type from
//! the code, like the items of an empty list nothing is added to, it says what to write. So
//! besides the signatures, a check gives the type of every expression and every name: the
//! typed tree a compiler builds on. See [`Types`].

mod alias;
mod builtins;
mod call;
mod checker;
mod constants;
mod effects;
mod expect;
mod expr;
mod extend;
mod function;
mod generic;
mod imports;
mod index;
mod infer;
mod keys;
mod messages;
mod pattern;
mod place;
mod program;
mod rules;
mod stdlib;
mod stmt;
mod sum;
mod types;

use std::collections::{BTreeMap, BTreeSet};

use fennec_syntax::{Diagnostic, Expr, Module, Span};

pub use types::Type;

/// Checks a parsed module. Returns the signature of every function and method, or every
/// problem found, in source order.
pub fn check(module: &Module) -> Result<Signatures, Vec<Diagnostic>> {
    check_typed(module).map(Checked::into_signatures)
}

/// Checks a parsed module. Returns the signatures and the type of every expression and
/// name, or every problem found, in source order.
pub fn check_typed(module: &Module) -> Result<Checked, Vec<Diagnostic>> {
    library_parses()?;
    checker::Checker::new(module).run(false)
}

/// A mistake in the standard library's own code comes first, since without it a program's
/// names would seem not to exist.
fn library_parses() -> Result<(), Vec<Diagnostic>> {
    fennec_syntax::prelude_modules().map(|_| ()).map_err(|problem| vec![problem])
}

/// Whether the language provides `name` everywhere, with no import: a builtin like `len`,
/// `Ok` or `Err`, a built-in type like `str` or `Stdout`, or a built-in interface like `Eq`.
/// An import can't take one of these names with `as`.
pub fn provided_everywhere(name: &str) -> bool {
    (builtins::is_builtin(name) && !name.contains('.'))
        || program::BUILT_IN_TYPES.contains(&name)
        || program::BUILT_IN_INTERFACES.contains(&name)
}

/// Whether the engines provide a function of this name, like `sum` or `math.sqrt`.
pub fn is_builtin(name: &str) -> bool {
    builtins::is_builtin(name)
}

/// Whether a method the engines provide on a type changes its value, like `list.append`.
/// `None` when the type has no such method.
pub fn builtin_method_changes(ty: &Type, name: &str) -> Option<bool> {
    builtins::method(ty, name).map(|method| method.changes)
}

/// Checks a parsed module, then writes into it the enum of each leading-dot case whose name
/// more than one enum has, as the type expected where it's written decided: `.Int(value)`
/// given back as an `ExprKind` becomes `ExprKind.Int(value)`. A `sum` of floats gets its
/// start of `0.0` written in. Running a module needs this, since what runs it doesn't know
/// types.
pub fn check_and_resolve(module: &mut Module) -> Result<Signatures, Vec<Diagnostic>> {
    let checked = check_typed(module)?;
    Ok(resolved(module, checked))
}

/// Like `check_and_resolve`, for a module about to run: the standard library's functions are
/// checked only when the module reaches them, so its signatures hold just those. The
/// library's own tests check all of it.
pub fn check_to_run(module: &mut Module) -> Result<Signatures, Vec<Diagnostic>> {
    library_parses()?;
    let checked = checker::Checker::new(module).run(true)?;
    Ok(resolved(module, checked))
}

fn resolved(module: &mut Module, checked: Checked) -> Signatures {
    if !checked.places.is_empty() {
        fennec_syntax::visit::functions_mut(module, &mut |def| {
            alias::write_in(&mut def.body, &checked.places);
        });
    }
    if !checked.cases.is_empty()
        || !checked.float_sums.is_empty()
        || !checked.generator_types.is_empty()
        || !checked.defaults.is_empty()
        || !checked.not_equals.is_empty()
        || !checked.optional_steps.is_empty()
        || !checked.std_names.is_empty()
    {
        fennec_syntax::visit::exprs_mut(module, &mut |expr| {
            let span = (expr.span.start, expr.span.end);
            if let Some(owner) = checked.cases.get(&span) {
                fennec_syntax::visit::name_enum(expr, owner);
            }
            if checked.float_sums.contains(&span) {
                fennec_syntax::visit::add_float_start(expr);
            }
            if let Some(defaults) = checked.defaults.get(&span) {
                fennec_syntax::visit::add_defaults(expr, defaults);
            }
            if let Some(function) = checked.std_names.get(&span) {
                expr.kind = fennec_syntax::ExprKind::Name(function.clone());
            }
            if checked.optional_steps.contains(&span)
                && let fennec_syntax::ExprKind::Attr { optional, .. } = &mut expr.kind
            {
                *optional = true;
            }
            if checked.not_equals.contains(&span) {
                fennec_syntax::visit::not_of_equals(expr);
            }
            if let Some(shown) = checked.generator_types.get(&span) {
                fennec_syntax::visit::name_generator_type(expr, shown);
            }
        });
    }
    fennec_syntax::fuse::stored_stages(module);
    checked.into_signatures()
}

impl Checked {
    /// The signatures, holding the type of every expression too, for compilers to use.
    fn into_signatures(self) -> Signatures {
        Signatures { types: self.types, ..self.signatures }
    }
}

/// Everything a check works out about a program that has no mistakes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Checked {
    pub signatures: Signatures,
    pub types: Types,
    /// The enum the expected type chose for each leading-dot case more than one enum has.
    cases: BTreeMap<(u32, u32), String>,
    /// The calls to `sum` of floats with no start, which start at `0.0`.
    float_sums: BTreeSet<(u32, u32)>,
    /// The statements that name a place, by `mut x = place`.
    places: BTreeSet<(u32, u32)>,
    /// The defaults each call leaves out, to write into it.
    defaults: BTreeMap<(u32, u32), Vec<(String, Expr)>>,
    /// The type of each generator expression, which printing one shows.
    generator_types: BTreeMap<(u32, u32), String>,
    /// What each name for a standard library function written in Fennec means.
    std_names: BTreeMap<(u32, u32), String>,
    /// The `!=`s on a type with its own `def ==`, which become `not (a == b)`.
    not_equals: BTreeSet<(u32, u32)>,
    /// The fields and method calls on a value that might be missing, in a chain an `else`
    /// ends, which are written in as optional.
    optional_steps: BTreeSet<(u32, u32)>,
}

/// The typed tree: the type of every expression and every declared name in a program,
/// the standard library's included, grouped by the code they're in.
///
/// Each group is keyed like [`Signatures`]: `name` for a function, `Type.name` for a method,
/// `outer.inner` for a nested function. Field defaults and `where` rules are under
/// `fields of Type`, and each `test` block under `test "its name"`.
///
/// Within a group, an expression is found by its span, which is unique: no two
/// expressions in one file cover the same bytes. A name is found by the span of what
/// declares it and its name:
/// - an assignment that makes it: the name's own span (each name's, in `a, b = pair`);
/// - a parameter: the parameter's span;
/// - a `for` loop or comprehension: the span of the whole `for` statement or comprehension;
/// - a `with ... as name`: the `with` statement's span;
/// - a `match` case: the case's span;
/// - an arrow function's parameter: the arrow function's span;
/// - a nested function's name: the nested `def`'s span.
///
/// Every value has a type here, nothing in it is `Type::Unknown` or `Type::Var`, and a
/// function called with a type parameter has its call typed with the parameter filled in.
/// Some expressions aren't values and have no type: a type's name used to reach a
/// constructor or enum case (`Vec3` in `Vec3.zero()`), a module's name, and the
/// `object.method` part of a method call (the call itself has a type).
///
/// Inside a generic function, types mention its parameters as `Type::Param`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Types {
    functions: BTreeMap<String, Body>,
}

impl Types {
    /// The types in one function, method, test or type's fields.
    pub fn body(&self, key: &str) -> Option<&Body> {
        self.functions.get(key)
    }

    /// Every group of types, by key in alphabetical order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Body)> {
        self.functions.iter().map(|(key, body)| (key.as_str(), body))
    }
}

/// The types of the expressions and names in one function, method, test or type's fields.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Body {
    pub(crate) exprs: BTreeMap<(u32, u32), Type>,
    pub(crate) names: BTreeMap<((u32, u32), String), Type>,
}

impl Body {
    /// The type of the expression at `span`.
    pub fn expr(&self, span: Span) -> Option<&Type> {
        self.exprs.get(&(span.start, span.end))
    }

    /// The type a name is declared with, by the span of what declares it. This is the type
    /// every value given to it has to fit, like `Enemy?`; what it holds at one point can be
    /// narrower, like `Enemy` after a check for None, and that's the type of the expression
    /// reading it there.
    pub fn name(&self, declared_at: Span, name: &str) -> Option<&Type> {
        self.names.get(&((declared_at.start, declared_at.end), name.to_owned()))
    }

    /// Every expression's span and type, in source order.
    pub fn exprs(&self) -> impl Iterator<Item = (Span, &Type)> {
        self.exprs.iter().map(|((start, end), ty)| (Span { start: *start, end: *end }, ty))
    }

    /// Every declared name, with the span of what declares it and its type, in source order.
    pub fn names(&self) -> impl Iterator<Item = (Span, &str, &Type)> {
        self.names
            .iter()
            .map(|(((start, end), name), ty)| (Span { start: *start, end: *end }, name.as_str(), ty))
    }
}

/// The signature of every function, method and nested function in a program, including the
/// standard library's: `name` for a function, `Type.name` for a method, and `outer.inner`
/// for a function defined inside another.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Signatures {
    functions: BTreeMap<String, Signature>,
    structs: BTreeMap<String, Struct>,
    enums: BTreeMap<String, Enum>,
    errors: BTreeMap<String, Struct>,
    locals: BTreeMap<String, Type>,
    types: Types,
}

/// A struct's fields, with their types, in the order they're written.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Struct {
    pub fields: Vec<(String, Type)>,
    /// Whether any field has a `where` rule, checked whenever a value is made.
    pub rules: bool,
}

/// An enum's cases, in the order they're written, each with its fields and their types.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Enum {
    pub cases: Vec<(String, Vec<(String, Type)>)>,
    /// Whether any field has a `where` rule, checked whenever a value is made.
    pub rules: bool,
}

impl Signatures {
    /// Whether `==` on values of this type calls a type's own `def ==`: its own, or one of a
    /// type it holds, in a field, an enum case or a collection.
    pub fn own_equality(&self, ty: &Type) -> bool {
        self.holds_own_equality(ty, &mut Vec::new())
    }

    fn holds_own_equality(&self, ty: &Type, open: &mut Vec<String>) -> bool {
        let name = match ty {
            Type::Named(name) => name,
            Type::Generic(name, args) => {
                if args.iter().any(|arg| self.holds_own_equality(arg, open)) {
                    return true;
                }
                name
            }
            other => return other.children().into_iter().any(|child| self.holds_own_equality(child, open)),
        };
        if open.contains(name) {
            return false;
        }
        if self.functions.contains_key(&format!("{name}.==")) {
            return true;
        }
        open.push(name.clone());
        let fields = self.structs.get(name).or_else(|| self.errors.get(name)).map(|found| &found.fields);
        let in_fields =
            fields.is_some_and(|fields| fields.iter().any(|(_, ty)| self.holds_own_equality(ty, open)));
        let cases = self.enums.get(name).map(|found| &found.cases);
        let in_cases = cases.is_some_and(|cases| {
            cases.iter().flat_map(|(_, fields)| fields).any(|(_, ty)| self.holds_own_equality(ty, open))
        });
        in_fields || in_cases
    }

    /// The typed tree of the check that made these signatures.
    pub fn types(&self) -> &Types {
        &self.types
    }

    pub fn get(&self, name: &str) -> Option<&Signature> {
        self.functions.get(name)
    }

    /// A struct's fields, the standard library's included.
    pub fn structure(&self, name: &str) -> Option<&Struct> {
        self.structs.get(name)
    }

    /// An enum's cases, the standard library's included.
    pub fn enumeration(&self, name: &str) -> Option<&Enum> {
        self.enums.get(name)
    }

    /// An error type's fields, the standard library's included.
    pub fn error_type(&self, name: &str) -> Option<&Struct> {
        self.errors.get(name)
    }

    /// The name of every error type, the standard library's included.
    pub fn error_types(&self) -> impl Iterator<Item = &str> {
        self.errors.keys().map(String::as_str)
    }

    /// The name of every struct, the standard library's included.
    pub fn structures(&self) -> impl Iterator<Item = &str> {
        self.structs.keys().map(String::as_str)
    }

    /// The type of the expression at `span` in a function.
    pub fn expr_type(&self, function: &str, span: Span) -> Option<&Type> {
        self.types.body(function)?.expr(span).filter(|ty| ty.is_known())
    }

    /// The type a name holds in a function (`name`, `Type.name` or `outer.inner`), when it's
    /// declared with the same type everywhere in it. [`Types`] has each declaration's type.
    pub fn local(&self, function: &str, name: &str) -> Option<&Type> {
        self.locals.get(&format!("{function}/{name}")).filter(|ty| ty.is_known())
    }

    /// Every signature, by name in alphabetical order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Signature)> {
        self.functions.iter().map(|(name, signature)| (name.as_str(), signature))
    }
}

/// What a function takes and gives back.
#[derive(Clone, Debug, PartialEq)]
pub struct Signature {
    /// Every parameter, including `self` for a method.
    pub params: Vec<ParamType>,
    /// The type of what a call gives back: a `Type::Result` when it can fail, and a list of
    /// what it yields when it's a generator.
    pub returns: Type,
    /// Whether it can fail: it contains `fail` or `?`.
    pub can_fail: bool,
    /// Whether it's a generator: it contains `yield`.
    pub generator: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParamType {
    pub name: String,
    pub ty: Type,
    pub is_mut: bool,
    /// What it holds when a call leaves it out.
    pub default: Option<Expr>,
}
