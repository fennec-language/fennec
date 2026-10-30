//! The standard library so far. Most of it is written in Fennec itself (in `fennec_syntax::PRELUDE`,
//! which the type checker reads too); the rest is builtins.

use fennec_syntax::{Diagnostic, Import, Imported};

/// Fennec source loaded before every program.
pub(crate) const SOURCES: [&str; fennec_syntax::PRELUDE.len()] = fennec_syntax::PRELUDE;

/// What an import brings in: modules by the name they're used under, like `json`, and
/// module functions by the name they're imported under, like `loads` for `json.loads`. Other
/// standard library names are available everywhere for now, so importing them only has to
/// be correct.
pub(crate) type Brought = (Vec<(String, &'static str)>, Vec<(String, &'static str)>);

pub(crate) fn check_import(import: &Import) -> Result<Brought, Diagnostic> {
    let mut modules = Vec::new();
    let mut functions = Vec::new();
    for imported in fennec_syntax::std_import(import)? {
        match imported {
            Imported::Module { alias, module } => modules.push((alias, module)),
            Imported::Name { name, module } => {
                if let Some(builtin) = crate::builtins::named(&format!("{module}.{name}")) {
                    functions.push((name, builtin));
                }
            }
        }
    }
    Ok((modules, functions))
}
