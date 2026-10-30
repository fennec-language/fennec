//! The standard library's functions and methods written in Rust, one file per module. The
//! rest of each module is Fennec code in `fennec_syntax`. Adding a module adds one line to
//! the list here.

use fennec_syntax::{Diagnostic, Span};

use crate::call::CallArg;
use crate::interp::Interp;
use crate::value::{Instance, Value};

type Call = fn(&mut Interp<'_>, &str, Vec<CallArg>, Span) -> Result<Value, Diagnostic>;
type Method = fn(&mut Interp<'_>, &Instance, &str, Vec<CallArg>, Span) -> Result<Value, Diagnostic>;

/// What one module has written in Rust.
pub(crate) struct Module {
    /// Its functions, by their full names, like `json.loads`.
    pub(crate) functions: &'static [&'static str],
    /// Calls one of its functions, by its full name.
    pub(crate) call: Call,
    /// The value of one of its constants, like `PI`.
    pub(crate) constant: fn(&str) -> Option<Value>,
    /// Its types whose methods are written in Rust, like `Regex`, and how to call them.
    pub(crate) methods: &'static [(&'static str, Method)],
}

/// Declares each module's file and lists them, one line per module.
macro_rules! modules {
    ($($module:ident,)*) => {
        $(pub(crate) mod $module;)*

        const MODULES: &[Module] = &[$($module::MODULE),*];
    };
}

modules! {
    json,
    math,
    random,
    re,
    toml,
}

/// The module with a function of this full name, like `json.loads`.
pub(crate) fn module_of(function: &str) -> Option<&'static Module> {
    MODULES.iter().find(|module| module.functions.contains(&function))
}

/// Every module function, by its full name.
pub(crate) fn functions() -> impl Iterator<Item = &'static str> {
    MODULES.iter().flat_map(|module| module.functions.iter().copied())
}

pub(crate) fn constant(name: &str) -> Option<Value> {
    MODULES.iter().find_map(|module| (module.constant)(name))
}

/// How to call the Rust methods of a standard library type, if it has them.
pub(crate) fn methods_of(type_name: &str) -> Option<Method> {
    MODULES
        .iter()
        .flat_map(|module| module.methods)
        .find(|(name, _)| *name == type_name)
        .map(|(_, call)| *call)
}

/// For a module with no constants.
fn no_constants(_: &str) -> Option<Value> {
    None
}
