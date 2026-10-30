//! The standard library's modules and the names each one provides. The checker and the
//! interpreter both read imports through here, so they agree on what an import brings in.

use crate::ast::Import;
use crate::diagnostic::Diagnostic;
use crate::span::Span;

/// The modules under `std` so far, and the names each one provides.
const MODULES: [(&str, &[&str]); 10] = [
    ("math", &["Vec3", "PI", "sqrt", "floor", "ceil", "sin", "cos"]),
    ("game", &["Id", "IdMap"]),
    ("layout", &["SoaList"]),
    ("json", &["loads", "dumps", "ParseError"]),
    ("toml", &["loads", "dumps", "ParseError"]),
    ("path", &["Path"]),
    ("collections", &["Deque", "Heap", "PriorityQueue", "Counter"]),
    ("random", &["Random", "random_int", "random_float", "random_choice"]),
    ("time", &["Clock", "Duration", "Instant"]),
    ("re", &["Regex", "Match", "is_regex"]),
];

/// Modules whose types, like `Deque`, exist only once imported. The older modules' names
/// are still available everywhere.
const IMPORTED_ONLY: [&str; 4] = ["collections", "random", "time", "re"];

/// The module a type that has to be imported comes from, like `collections` for `Deque`.
/// `Clock` is left out: it's a capability, which `main` names like `Stdout` and `Fs`.
pub fn importable_type(name: &str) -> Option<&'static str> {
    if name == "Clock" || !name.starts_with(|c: char| c.is_ascii_uppercase()) {
        return None;
    }
    MODULES
        .iter()
        .find(|(module, names)| IMPORTED_ONLY.contains(module) && names.contains(&name))
        .map(|(module, _)| *module)
}

/// What an import brings in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Imported {
    /// A module, used as `json.loads(...)`: from `import std.json` or `from std import json`.
    Module { alias: String, module: &'static str },
    /// One name from a module: from `from std.json import loads`.
    Name { name: String, module: &'static str },
}

/// Checks an import names standard library modules and names that exist, and says what it
/// brings in.
pub fn std_import(import: &Import) -> Result<Vec<Imported>, Diagnostic> {
    let span = import.span;
    match import.module.as_slice() {
        [std] if std == "std" && import.names.is_empty() => {
            Err(Diagnostic::new("import a module from std, not std itself", span)
                .with_help("like `import std.json` or `from std import json`"))
        }
        [std] if std == "std" => import
            .names
            .iter()
            .map(|name| {
                let module = module(name).ok_or_else(|| no_module(&format!("std.{name}"), span))?;
                Ok(Imported::Module { alias: name.clone(), module })
            })
            .collect(),
        [std, name] if std == "std" => {
            let module = module(name).ok_or_else(|| no_module(&format!("std.{name}"), span))?;
            if import.names.is_empty() {
                return Ok(vec![Imported::Module { alias: name.clone(), module }]);
            }
            import
                .names
                .iter()
                .map(|name| {
                    member(module, name, span)?;
                    Ok(Imported::Name { name: name.clone(), module })
                })
                .collect()
        }
        _ => Err(no_module(&import.module.join("."), span)),
    }
}

/// Checks `module.name` is something the module provides.
pub fn member(module: &str, name: &str, span: Span) -> Result<(), Diagnostic> {
    let names = names(module);
    if names.contains(&name) {
        return Ok(());
    }
    Err(Diagnostic::new(format!("`std.{module}` has nothing called `{name}`"), span)
        .with_help(format!("`std.{module}` provides {}", names.join(", "))))
}

/// The mistake of using a module itself as a value, as in `stdout.print(json)`.
pub fn module_as_value(alias: &str, module: &str, span: Span) -> Diagnostic {
    let names = names(module);
    let example = names.first().copied().unwrap_or("name");
    Diagnostic::new(format!("`{alias}` is a module, so use one of its names, like `{alias}.{example}`"), span)
        .with_help(format!("`std.{module}` provides {}", names.join(", ")))
}

fn names(module: &str) -> &'static [&'static str] {
    MODULES.iter().find(|(known, _)| *known == module).map_or(&[], |(_, names)| *names)
}

fn module(name: &str) -> Option<&'static str> {
    MODULES.iter().map(|(module, _)| *module).find(|module| *module == name)
}

fn no_module(name: &str, span: Span) -> Diagnostic {
    let modules: Vec<String> = MODULES.iter().map(|(module, _)| format!("std.{module}")).collect();
    Diagnostic::new(format!("there's no module called `{name}`"), span)
        .with_help(format!("the standard library so far has {}", modules.join(", ")))
}
