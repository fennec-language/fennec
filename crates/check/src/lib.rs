//! Fennec's type checker. It finds mistakes before a program runs (unknown names, changes to
//! names that aren't `mut`, wrong arguments, ignored errors, values that might be None,
//! `match`es that miss a case) and works out every function's signature, inferring the
//! return types that aren't written.
//!
//! Each function is checked on its own, against the signatures of the functions it calls.
//! A function whose return type isn't written is checked the first time something needs its
//! signature, so every body is checked exactly once, in whatever order calls require.
//!
//! When the checker can't tell a type, it treats it as unknown and says nothing, so anything
//! it reports is a real mistake. The interpreter still checks the rest as the program runs.

mod builtins;
mod call;
mod checker;
mod effects;
mod expr;
mod function;
mod messages;
mod pattern;
mod place;
mod program;
mod stmt;
mod types;

use std::collections::BTreeMap;

use fennec_syntax::{Diagnostic, Module};

pub use types::Type;

/// Checks a parsed module. Returns the signature of every function and method, or every
/// problem found, in source order.
pub fn check(module: &Module) -> Result<Signatures, Vec<Diagnostic>> {
    checker::Checker::new(module).run()
}

/// The signature of every function, method and nested function in a program, including the
/// standard library's: `name` for a function, `Type.name` for a method, and `outer.inner`
/// for a function defined inside another.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Signatures {
    functions: BTreeMap<String, Signature>,
}

impl Signatures {
    pub fn get(&self, name: &str) -> Option<&Signature> {
        self.functions.get(name)
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
}
