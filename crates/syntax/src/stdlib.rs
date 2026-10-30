//! The standard library's modules. Each one has its own file here, with the names it
//! provides and its code written in Fennec, and its own files in the interpreter and the
//! checker for anything written in Rust. Adding a module adds one line to each list.

/// One module under `std`.
pub(crate) struct Module {
    pub(crate) name: &'static str,
    /// Everything it provides: types, constants and functions.
    pub(crate) names: &'static [&'static str],
    /// Its code written in Fennec, or nothing.
    pub(crate) source: &'static str,
    /// Its types that methods give back to code that never names them, by the method, like
    /// `Instant` from `clock.now()`.
    pub(crate) gives: &'static [(&'static str, &'static str)],
}

/// Declares each module's file and lists them, one line per module.
macro_rules! modules {
    ($($module:ident,)*) => {
        $(mod $module;)*

        /// The modules under `std` so far.
        pub(crate) const MODULES: &[Module] = &[$($module::MODULE),*];
    };
}

modules! {
    collections,
    fs,
    game,
    hash,
    json,
    layout,
    math,
    path,
    random,
    re,
    time,
    toml,
}

/// What's available everywhere without an import: the errors standard operations fail
/// with, and the built-in types' methods written in Fennec.
const CORE: [&str; 2] = [include_str!("stdlib/errors.fen"), include_str!("stdlib/str.fen")];

/// The standard library written in Fennec, loaded before every program by both the
/// interpreter and the type checker: the core, then each module's code.
pub fn prelude() -> impl Iterator<Item = &'static str> {
    let modules = MODULES.iter().map(|module| module.source).filter(|source| !source.is_empty());
    CORE.into_iter().chain(modules)
}

/// Standard library types a program gets values of without naming them, by the method
/// that gives one back, like `Instant` from `now`.
pub fn given() -> impl Iterator<Item = (&'static str, &'static str)> {
    MODULES.iter().flat_map(|module| module.gives.iter().copied())
}

#[cfg(test)]
mod tests {
    /// The JIT compiles a type's methods only once something gives it one, so every method
    /// that gives back a standard library type has to be listed.
    #[test]
    fn methods_say_which_types_they_give() {
        let given: Vec<_> = super::given().collect();
        for expected in
            [("now", "Instant"), ("search", "Match"), ("metadata", "Metadata"), ("list_dir", "Path")]
        {
            assert!(given.contains(&expected), "{expected:?} is missing from {given:?}");
        }
    }
}
