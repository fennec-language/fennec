//! The standard library's modules. Each one has its own file here, with the names it
//! provides and its code written in Fennec, and its own files in the interpreter and the
//! checker for anything written in Rust. Adding a module adds one line to each list.

use std::collections::{HashMap, HashSet};

use crate::ast::{ExprKind, Item};
use crate::modules::Imported;

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
const CORE: [&str; 5] = [
    include_str!("stdlib/errors.fen"),
    include_str!("stdlib/str.fen"),
    include_str!("stdlib/list.fen"),
    include_str!("stdlib/dict.fen"),
    include_str!("stdlib/set.fen"),
];

/// The standard library written in Fennec, loaded before every program by both the
/// interpreter and the type checker: the core, then each module's code.
pub fn prelude() -> impl Iterator<Item = &'static str> {
    let modules = MODULES.iter().map(|module| module.source).filter(|source| !source.is_empty());
    CORE.into_iter().chain(modules)
}

/// The prelude's sources parsed, once for the whole process: the checker, the compiler and
/// the interpreter all read it, and a run uses them all.
pub fn prelude_modules() -> Result<&'static [crate::Module], crate::Diagnostic> {
    static PARSED: std::sync::OnceLock<Result<Vec<crate::Module>, crate::Diagnostic>> =
        std::sync::OnceLock::new();
    match PARSED.get_or_init(parse_prelude) {
        Ok(modules) => Ok(modules),
        Err(diagnostic) => Err(diagnostic.clone()),
    }
}

fn parse_prelude() -> Result<Vec<crate::Module>, crate::Diagnostic> {
    let mut parsed: Vec<crate::Module> =
        CORE.into_iter().map(crate::parser::parse_prelude).collect::<Result<_, _>>()?;
    let mut modules = Vec::new();
    for module in MODULES.iter().filter(|module| !module.source.is_empty()) {
        modules.push((module.name, crate::parser::parse_prelude(module.source)?));
    }
    qualify_functions(&mut modules);
    parsed.extend(modules.into_iter().map(|(_, module)| module));
    Ok(parsed)
}

/// Gives each module's top-level functions the name `module.function`, which no one can write
/// by accident, so two modules or a program can use the same name. A program reaches one by
/// importing it, and a module's own calls are written in here. A local in a module can't share
/// a name with one of the module's functions.
fn qualify_functions(modules: &mut [(&'static str, crate::Module)]) {
    let mut all = HashSet::new();
    for (name, module) in modules.iter_mut() {
        for item in &mut module.items {
            if let Item::Function(def) = item {
                def.name = format!("{name}.{}", def.name);
                all.insert(def.name.clone());
            }
        }
    }
    for (name, module) in modules.iter_mut() {
        let prefix = format!("{name}.");
        let mut names: HashMap<String, String> = all
            .iter()
            .filter_map(|function| Some((function.strip_prefix(&prefix)?.to_owned(), function.clone())))
            .collect();
        let mut aliases = HashMap::new();
        let imports = module.items.iter().filter_map(|item| match item {
            Item::Import(import) => crate::std_import(import).ok(),
            _ => None,
        });
        for imported in imports.flatten() {
            match imported {
                Imported::Module { alias, module } => {
                    aliases.insert(alias, module);
                }
                Imported::Name { name, alias, module } => {
                    let function = format!("{module}.{name}");
                    if all.contains(&function) {
                        names.insert(alias, function);
                    }
                }
            }
        }
        crate::visit::exprs_mut(module, &mut |expr| {
            let function = match &expr.kind {
                ExprKind::Name(found) => names.get(found).cloned(),
                ExprKind::Attr { object, name, optional: false } => crate::dotted(object)
                    .and_then(|alias| aliases.get(&alias))
                    .map(|module| format!("{module}.{name}"))
                    .filter(|function| all.contains(function)),
                _ => None,
            };
            if let Some(function) = function {
                expr.kind = ExprKind::Name(function);
            }
        });
    }
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
