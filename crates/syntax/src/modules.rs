//! Imports of the standard library's modules, which are listed in `stdlib`. The checker and
//! the compiler both read imports through here, so they agree on what an import brings in.

use crate::ast::{Expr, ExprKind, Import};
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use crate::stdlib::MODULES;

/// Names that a module provides but that are available everywhere too. `Clock`, `Env`, `Fs`,
/// `Net`, `Process` and `Signals` are capabilities, which `main` names like `Stdout`. `ParseError` and
/// `Missing` are errors, like `IoError`, that code matches on without importing where they came
/// from.
const EVERYWHERE: [&str; 8] = ["Clock", "Env", "Fs", "Net", "Process", "Signals", "ParseError", "Missing"];

/// The module a type or constant that has to be imported comes from, like `collections`
/// for `Deque`. Functions are named `module.function` and are imported apart.
pub fn importable_type(name: &str) -> Option<&'static str> {
    if EVERYWHERE.contains(&name) || !name.starts_with(|c: char| c.is_ascii_uppercase()) {
        return None;
    }
    MODULES
        .iter()
        .find(|module| module.names.contains(&name) && !kept_inside(module.name, name))
        .map(|module| module.name)
}

/// Whether `name` is one of a module's own top-level names, written in Fennec without `export`.
/// The standard library is one kit, so its other modules may import it, and a program may not.
fn kept_inside(module: &str, name: &str) -> bool {
    // A `_` name is the module's own, as a `_` field or method is.
    if name.starts_with('_') {
        return true;
    }
    let Some(source) = MODULES.iter().find(|known| known.name == module).map(|known| known.source) else {
        return false;
    };
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    source.lines().any(|line| {
        let declared = ["def ", "struct ", "enum ", "error ", "shared ", "interface ", "type "]
            .iter()
            .find_map(|keyword| line.strip_prefix(keyword))
            .unwrap_or(line);
        declared.strip_prefix(name).is_some_and(|after| after.starts_with(|c: char| !word(c)))
    })
}

/// What an import brings in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Imported {
    /// A module, used under `alias`: `std.json` for `import std.json`, which binds `std` as
    /// in Python, `json` for `from std import json`, and `j` for `import std.json as j`.
    Module { alias: String, module: &'static str },
    /// One name from a module, bound under `alias`: `loads` from `from std.json import
    /// loads`, or `json_loads` with `as json_loads`.
    Name { name: String, alias: String, module: &'static str },
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
            .bound()
            .map(|(name, alias)| {
                let module = module(name).ok_or_else(|| no_module(&format!("std.{name}"), span))?;
                Ok(Imported::Module { alias: alias.to_owned(), module })
            })
            .collect(),
        [std, name] if std == "std" => {
            let module = module(name).ok_or_else(|| no_module(&format!("std.{name}"), span))?;
            if import.names.is_empty() {
                let alias = import.alias.clone().unwrap_or_else(|| format!("std.{name}"));
                return Ok(vec![Imported::Module { alias, module }]);
            }
            import
                .bound()
                .map(|(name, alias)| {
                    member(module, name, span)?;
                    if alias != name && name.starts_with(|c: char| c.is_ascii_uppercase()) {
                        return Err(Diagnostic::new(
                            format!("`as` can't rename a type from the standard library yet, like `{name}`"),
                            span,
                        )
                        .with_help(format!("import it as `{name}`")));
                    }
                    Ok(Imported::Name { name: name.to_owned(), alias: alias.to_owned(), module })
                })
                .collect()
        }
        _ => Err(no_module(&import.module.join("."), span)),
    }
}

/// Checks `module.name` is something the module provides: to a program, what it exports, and to
/// the standard library's own code, any of its names.
pub fn member(module: &str, name: &str, span: Span) -> Result<(), Diagnostic> {
    let names = names(module);
    if names.contains(&name) && (span.in_prelude() || !kept_inside(module, name)) {
        return Ok(());
    }
    Err(Diagnostic::new(format!("`std.{module}` has nothing called `{name}`"), span)
        .with_help(format!("`std.{module}` provides {}", shown(module, names))))
}

/// The mistake of using a module itself as a value, as in `stdout.print(json)`.
pub fn module_as_value(alias: &str, module: &str, span: Span) -> Diagnostic {
    let names = names(module);
    let example = names.first().copied().unwrap_or("name");
    Diagnostic::new(format!("`{alias}` is a module, so use one of its names, like `{alias}.{example}`"), span)
        .with_help(format!("`std.{module}` provides {}", shown(module, names)))
}

/// `a.b.c` for a chain of names and attributes, like the module path in `std.json.loads`.
pub fn dotted(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Name(name) => Some(name.clone()),
        ExprKind::Attr { object, name, optional: false } => Some(format!("{}.{name}", dotted(object)?)),
        _ => None,
    }
}

/// `std.toml` written after `import std.json`, which binds `std` but not `std.toml`; or
/// `std.json` on its own, which is a module, not a value. `module` is the module `path`
/// names, if it's imported.
pub fn package_member(path: &str, module: Option<&str>, span: Span) -> Diagnostic {
    match module {
        Some(module) => module_as_value(path, module, span),
        None => Diagnostic::new(format!("`{path}` isn't imported here"), span)
            .with_help(format!("add `import {path}` at the top of this file")),
    }
}

/// The mistake of writing `json.loads` after `import std.json`, which binds `std`.
pub fn short_module(name: &str, span: Span) -> Diagnostic {
    let example = names(name).first().copied().unwrap_or("name");
    Diagnostic::new(format!("`{name}` doesn't exist"), span).with_help(format!(
        "`import std.{name}` binds `std`, so write `std.{name}.{example}`, or import it with `from std import {name}`"
    ))
}

/// A module's names as help lists them: not those only other std modules and native code use.
fn shown(module: &str, names: &[&str]) -> String {
    let public: Vec<&str> = names
        .iter()
        .copied()
        .filter(|name| !kept_inside(module, name) && !name.starts_with("native_"))
        .collect();
    public.join(", ")
}

fn names(module: &str) -> &'static [&'static str] {
    MODULES.iter().find(|known| known.name == module).map_or(&[], |known| known.names)
}

fn module(name: &str) -> Option<&'static str> {
    MODULES.iter().map(|module| module.name).find(|module| *module == name)
}

fn no_module(name: &str, span: Span) -> Diagnostic {
    if name == "std.game" {
        return Diagnostic::new("there's no module called `std.game` any more", span).with_help(
            "`Table` and `Id` are in `std.collections`, like `from std.collections import Id, Table`",
        );
    }
    let modules: Vec<String> = MODULES.iter().map(|module| format!("std.{}", module.name)).collect();
    let diagnostic = Diagnostic::new(format!("there's no module called `{name}`"), span);
    // A slip of a letter or two, like `std.compres`, is most likely the module it nearly spells.
    match modules
        .iter()
        .filter(|module| letters_apart(module, name) <= 2)
        .min_by_key(|module| letters_apart(module, name))
    {
        Some(meant) => diagnostic.with_help(format!("did you mean `{meant}`?")),
        None => diagnostic.with_help(format!("the standard library so far has {}", modules.join(", "))),
    }
}

/// How many letters it takes to change one name into the other: added, removed or changed.
fn letters_apart(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.chars().enumerate() {
        let mut diagonal = row.first().copied().unwrap_or(0);
        if let Some(first) = row.first_mut() {
            *first = i + 1;
        }
        for (j, y) in b.iter().enumerate() {
            let above = row.get(j + 1).copied().unwrap_or(0);
            let left = row.get(j).copied().unwrap_or(0);
            let best = (diagonal + usize::from(x != *y)).min(above + 1).min(left + 1);
            diagonal = above;
            if let Some(cell) = row.get_mut(j + 1) {
                *cell = best;
            }
        }
    }
    row.last().copied().unwrap_or(0)
}
