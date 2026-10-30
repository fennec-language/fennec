//! The standard library so far. Most of it is written in Fennec itself (in `fennec_syntax::PRELUDE`,
//! which the type checker reads too); the rest is builtins.

use fennec_syntax::{Diagnostic, Import};

/// Fennec source loaded before every program.
pub(crate) const SOURCES: [&str; fennec_syntax::PRELUDE.len()] = fennec_syntax::PRELUDE;

/// The modules that exist so far, and the names each one provides.
const MODULES: [(&str, &[&str]); 6] = [
    ("std.math", &["Vec3", "PI", "sqrt", "floor", "ceil", "sin", "cos"]),
    ("std.game", &["Id", "IdMap"]),
    ("std.layout", &["SoaList"]),
    ("std.json", &["loads", "dumps", "ParseError"]),
    ("std.toml", &["loads", "dumps", "ParseError"]),
    ("std.path", &["Path"]),
];

/// Checks an import names a module and names that exist. Most standard library names are
/// available everywhere for now, so importing them only has to be correct, not complete.
/// Functions of modules like `std.json` exist only once imported; this returns them, as the
/// name they're imported under and the builtin it means, like `loads` and `json.loads`.
pub(crate) fn check_import(import: &Import) -> Result<Vec<(String, &'static str)>, Diagnostic> {
    let module = import.module.join(".");
    let Some((_, names)) = MODULES.iter().find(|(name, _)| *name == module) else {
        return Err(Diagnostic::new(format!("there's no module called `{module}`"), import.span)
            .with_help(format!("the standard library so far has {}", module_names().join(", "))));
    };
    match import.names.iter().find(|name| !names.contains(&name.as_str())) {
        Some(missing) => {
            Err(Diagnostic::new(format!("`{module}` has nothing called `{missing}`"), import.span)
                .with_help(format!("`{module}` provides {}", names.join(", "))))
        }
        None => Ok(import
            .names
            .iter()
            .filter_map(|name| {
                let last = import.module.last()?;
                crate::builtins::named(&format!("{last}.{name}")).map(|builtin| (name.clone(), builtin))
            })
            .collect()),
    }
}

fn module_names() -> Vec<&'static str> {
    MODULES.iter().map(|(name, _)| *name).collect()
}
