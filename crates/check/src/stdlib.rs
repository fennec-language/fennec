//! The signatures of the standard library's functions and methods written in Rust, one file
//! per module. Types written in Fennec are checked like any other code. Adding a module adds
//! one line to the list here.

mod formats;

use fennec_syntax::{Arg, Span};

use crate::builtins::Method;
use crate::function::FnCx;
use crate::types::Type;

/// What a call to a module function gives back, given its plain arguments' types.
type Returns = fn(&mut FnCx<'_>, &str, &[(Type, Span)], Span) -> Type;

/// One function of a module.
pub(crate) struct Function {
    /// Its full name, like `json.loads`.
    pub(crate) name: &'static str,
    /// How many plain arguments it takes: at least, at most.
    pub(crate) arity: (usize, usize),
    /// The named arguments it takes, like `indent`.
    pub(crate) named: &'static [&'static str],
    /// Whether its arguments have to be plain values, not results or optionals.
    pub(crate) needs_values: bool,
}

/// What one module has written in Rust.
pub(crate) struct Module {
    pub(crate) functions: &'static [Function],
    /// What a call to one of its functions gives back, given its plain arguments' types.
    /// Arguments of the wrong type are reported here.
    pub(crate) result: Returns,
    /// A method of one of its types that's written in Rust, like `Regex.search`.
    pub(crate) method: fn(&str, &str) -> Option<Method>,
    /// Checks a call that builds one of its types, like a `Regex` with a literal pattern.
    pub(crate) construct: fn(&mut FnCx<'_>, &str, &[Arg]),
}

/// Declares each module's file and lists them, one line per module.
macro_rules! modules {
    ($($module:ident,)*) => {
        $(mod $module;)*

        const MODULES: &[Module] = &[$($module::MODULE),*];
    };
}

modules! {
    fs,
    hash,
    json,
    math,
    random,
    re,
    toml,
}

/// The module function of this full name, like `json.loads`, and its module.
pub(crate) fn function(name: &str) -> Option<(&'static Module, &'static Function)> {
    MODULES.iter().find_map(|module| {
        module.functions.iter().find(|function| function.name == name).map(|function| (module, function))
    })
}

/// Every module function's full name.
pub(crate) fn functions() -> impl Iterator<Item = &'static str> {
    MODULES.iter().flat_map(|module| module.functions.iter().map(|function| function.name))
}

/// A method written in Rust of a standard library type.
pub(crate) fn method(type_name: &str, name: &str) -> Option<Method> {
    MODULES.iter().find_map(|module| (module.method)(type_name, name))
}

/// Checks a call that builds a standard library type.
pub(crate) fn construct(cx: &mut FnCx<'_>, type_name: &str, args: &[Arg]) {
    for module in MODULES {
        (module.construct)(cx, type_name, args);
    }
}

/// A function that takes `arity` plain values.
const fn takes(name: &'static str, arity: (usize, usize)) -> Function {
    Function { name, arity, named: &[], needs_values: true }
}

/// For a module with no functions, which is never called.
fn no_functions(_: &mut FnCx<'_>, _: &str, _: &[(Type, Span)], _: Span) -> Type {
    Type::Unknown
}

/// For a module whose types have no methods written in Rust.
fn no_methods(_: &str, _: &str) -> Option<Method> {
    None
}

/// For a module whose types need no checks when they're built.
fn no_checks(_: &mut FnCx<'_>, _: &str, _: &[Arg]) {}
