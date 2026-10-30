//! The standard library so far. Most of it is written in Fennec itself (in `fennec_syntax::PRELUDE`,
//! which the type checker reads too); the rest is builtins.

use fennec_syntax::{Diagnostic, Import};

/// Fennec source loaded before every program.
pub(crate) const SOURCES: &[&str] = &fennec_syntax::PRELUDE;

/// The modules that exist so far, and the names each one provides.
const MODULES: [(&str, &[&str]); 6] = [
    ("std.math", &["Vec3", "PI", "sqrt", "floor", "ceil", "sin", "cos"]),
    ("std.game", &["Id", "IdMap"]),
    ("std.layout", &["SoaList"]),
    ("std.collections", &["Deque", "Heap", "PriorityQueue", "Counter"]),
    ("std.random", &["Random", "random_int", "random_float", "random_choice"]),
    ("std.time", &["Clock", "Duration", "Instant"]),
];

/// Checks an import names a module and names that exist. Standard library names are
/// available everywhere for now, so importing only has to be correct, not complete.
pub(crate) fn check_import(import: &Import) -> Result<(), Diagnostic> {
    let module = import.module.join(".");
    let Some((_, names)) = MODULES.iter().find(|(name, _)| *name == module) else {
        return Err(Diagnostic::new(format!("there's no module called `{module}`"), import.span)
            .with_help(format!(
                "the standard library so far has {}",
                MODULES.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(", ")
            )));
    };
    match import.names.iter().find(|name| !names.contains(&name.as_str())) {
        Some(missing) => {
            Err(Diagnostic::new(format!("`{module}` has nothing called `{missing}`"), import.span)
                .with_help(format!("`{module}` provides {}", names.join(", "))))
        }
        None => Ok(()),
    }
}
