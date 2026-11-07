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
    base64,
    cli,
    collections,
    crypto,
    csv,
    fs,
    intrinsics,
    json,
    layout,
    log,
    math,
    net,
    pack,
    path,
    process,
    random,
    re,
    string,
    time,
    toml,
    url,
}

/// What's available everywhere without an import: the errors standard operations fail
/// with, and the built-in types' methods written in Fennec.
const CORE: [(&str, &str); 6] = [
    ("errors", include_str!("stdlib/errors.fen")),
    ("str", include_str!("stdlib/str.fen")),
    ("list", include_str!("stdlib/list.fen")),
    ("dict", include_str!("stdlib/dict.fen")),
    ("set", include_str!("stdlib/set.fen")),
    ("serialize", include_str!("stdlib/serialize.fen")),
];

/// The standard library written in Fennec, loaded before every program by both the
/// interpreter and the type checker: the core, then each module's code.
pub fn prelude() -> impl Iterator<Item = &'static str> {
    let modules = MODULES.iter().map(|module| module.source).filter(|source| !source.is_empty());
    CORE.into_iter().map(|(_, source)| source).chain(modules)
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

/// The standard library as the engines run it: like [`prelude_modules`], with what the checker
/// rewrites in a program's own code rewritten here too. `list[T]()`, `dict[K, V]()` and
/// `set[T]()` say what an empty collection holds, for the checker; to run, each is just an empty
/// one. Only the standard library is here, so `list`, `dict` and `set` are always the built-in
/// ones.
pub fn prelude_to_run() -> Result<&'static [crate::Module], crate::Diagnostic> {
    static RUN: std::sync::OnceLock<Result<Vec<crate::Module>, crate::Diagnostic>> =
        std::sync::OnceLock::new();
    let made = RUN.get_or_init(|| {
        let mut modules = prelude_modules()?.to_vec();
        for module in &mut modules {
            crate::visit::exprs_mut(module, &mut |expr| {
                if is_typed_empty(expr) {
                    crate::visit::empty_collection(expr);
                }
            });
        }
        Ok(modules)
    });
    match made {
        Ok(modules) => Ok(modules),
        Err(diagnostic) => Err(diagnostic.clone()),
    }
}

/// The standard library's core as the engines run it: what's there without an import, the
/// errors and the built-in types' methods. Parsed once for the whole process.
pub fn core_to_run() -> Result<&'static [crate::Module], crate::Diagnostic> {
    static CORE_RUN: std::sync::OnceLock<Result<Vec<crate::Module>, crate::Diagnostic>> =
        std::sync::OnceLock::new();
    let made = CORE_RUN.get_or_init(|| {
        let mut core = Vec::new();
        for (name, source) in CORE {
            core.push(parse_module(name, source)?);
        }
        let interfaces = crate::visit::interface_names(&core);
        for module in &mut core {
            crate::visit::interface_params(module, &interfaces);
        }
        Ok(core.into_iter().map(to_run).collect())
    });
    match made {
        Ok(modules) => Ok(modules),
        Err(diagnostic) => Err(diagnostic.clone()),
    }
}

/// One module of the standard library as the engines run it, like [`prelude_to_run`]'s, parsed
/// the first time it's asked for, so a program pays only for the modules it uses. `None` for a
/// module with no code written in Fennec, or no module of that name.
pub fn module_to_run(name: &str) -> Result<Option<&'static crate::Module>, crate::Diagnostic> {
    static RUN: std::sync::OnceLock<Vec<std::sync::OnceLock<Result<crate::Module, crate::Diagnostic>>>> =
        std::sync::OnceLock::new();
    let Some(at) = MODULES.iter().position(|module| module.name == name && !module.source.is_empty()) else {
        return Ok(None);
    };
    let all = RUN.get_or_init(|| MODULES.iter().map(|_| std::sync::OnceLock::new()).collect());
    let Some(slot) = all.get(at) else { return Ok(None) };
    match slot.get_or_init(|| parse_alone(at)) {
        Ok(module) => Ok(Some(module)),
        Err(diagnostic) => Err(diagnostic.clone()),
    }
}

/// The module at `at` in `MODULES`, parsed and its functions qualified as `parse_prelude` does
/// for all of them: which needs only its own functions' names and those of the modules it
/// imports, so only those are parsed with it.
fn parse_alone(at: usize) -> Result<crate::Module, crate::Diagnostic> {
    let Some(this) = MODULES.get(at) else {
        return Err(crate::Diagnostic::new("no such module", crate::Span::default()));
    };
    let module = parse_module(this.name, this.source)?;
    let imports = imported_modules(&module);
    let mut set = vec![(this.name, module)];
    for name in imports {
        if name != this.name
            && let Some(other) = MODULES.iter().find(|other| other.name == name && !other.source.is_empty())
        {
            set.push((other.name, parse_module(other.name, other.source)?));
        }
    }
    qualify_functions(&mut set);
    let (_, mut module) = set.swap_remove(0);
    let imported = set.iter().map(|(_, module)| module);
    let interfaces = crate::visit::interface_names(core_to_run()?.iter().chain(imported).chain([&module]));
    crate::visit::interface_params(&mut module, &interfaces);
    Ok(to_run(module))
}

/// The std modules a module imports, by name.
fn imported_modules(module: &crate::Module) -> Vec<&'static str> {
    let imports = module.items.iter().filter_map(|item| match item {
        Item::Import(import) => crate::std_import(import).ok(),
        _ => None,
    });
    imports
        .flatten()
        .map(|imported| match imported {
            Imported::Module { module, .. } | Imported::Name { module, .. } => module,
        })
        .collect()
}

/// A parsed module of the standard library as the engines run it: its typed empty collections
/// made plain ones.
fn to_run(mut module: crate::Module) -> crate::Module {
    crate::visit::exprs_mut(&mut module, &mut |expr| {
        if is_typed_empty(expr) {
            crate::visit::empty_collection(expr);
        }
    });
    module
}

/// The std module that provides a type, constant or function by its name, like `time` for
/// `Duration`: for engines that load modules as they're used.
pub fn module_providing(name: &str) -> Option<&'static str> {
    MODULES.iter().find(|module| module.names.contains(&name)).map(|module| module.name)
}

/// Whether an expression makes an empty collection with its types written: `list[int]()`.
fn is_typed_empty(expr: &crate::Expr) -> bool {
    let ExprKind::Call { callee, args } = &expr.kind else { return false };
    let ExprKind::Index { object, .. } = &callee.kind else { return false };
    args.is_empty()
        && matches!(&object.kind, ExprKind::Name(name) if matches!(name.as_str(), "list" | "dict" | "set"))
}

fn parse_prelude() -> Result<Vec<crate::Module>, crate::Diagnostic> {
    let mut parsed = Vec::new();
    for (name, source) in CORE {
        parsed.push(parse_module(name, source)?);
    }
    let mut modules = Vec::new();
    for module in MODULES.iter().filter(|module| !module.source.is_empty()) {
        modules.push((module.name, parse_module(module.name, module.source)?));
    }
    qualify_functions(&mut modules);
    parsed.extend(modules.into_iter().map(|(_, module)| module));
    let interfaces = crate::visit::interface_names(&parsed);
    for module in &mut parsed {
        crate::rename::self_as_its_type(module);
        crate::visit::interface_params(module, &interfaces);
    }
    Ok(parsed)
}

/// Parses one file of the standard library. A mistake in one says which file and line, since it
/// can't be shown against a program's own lines.
fn parse_module(name: &str, source: &str) -> Result<crate::Module, crate::Diagnostic> {
    let mut module = crate::parser::parse_prelude(source).map_err(|problem| {
        let at = problem.span.start.saturating_sub(crate::Span::PRELUDE) as usize;
        let line = source.get(..at).map_or(1, |before| before.matches('\n').count() + 1);
        let message = format!("the standard library's `{name}.fen`, line {line}: {}", problem.message);
        crate::Diagnostic { message, ..problem }
    })?;
    crate::rename::discards_apart(&mut module);
    Ok(module)
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
    /// How many typed empty collections, like `list[int]()`, a set of modules makes.
    fn typed_empties(modules: &[crate::Module]) -> usize {
        let mut found = 0;
        for module in modules {
            crate::visit::exprs_mut(&mut module.clone(), &mut |expr| {
                found += usize::from(super::is_typed_empty(expr));
            });
        }
        found
    }

    #[test]
    fn the_library_runs_its_typed_empty_collections_as_empty_ones() {
        assert!(typed_empties(super::prelude_modules().unwrap()) > 0, "the library writes some, like csv's");
        assert_eq!(typed_empties(super::prelude_to_run().unwrap()), 0);
        let mut module = crate::parse("def f():\n    a = dict[str, int]()\n    b = set[int]()\n").unwrap();
        crate::visit::exprs_mut(&mut module, &mut |expr| {
            if super::is_typed_empty(expr) {
                crate::visit::empty_collection(expr);
            }
        });
        assert_eq!(typed_empties(std::slice::from_ref(&module)), 0);
    }

    #[test]
    fn a_module_parsed_alone_is_what_parsing_the_whole_library_makes_of_it() {
        let whole = super::prelude_to_run().unwrap();
        let core = super::core_to_run().unwrap();
        assert_eq!(whole.get(..core.len()), Some(core));
        let alone: Vec<&crate::Module> =
            super::MODULES.iter().filter_map(|module| super::module_to_run(module.name).unwrap()).collect();
        assert_eq!(alone.len() + core.len(), whole.len());
        for (alone, whole) in alone.iter().zip(&whole[core.len()..]) {
            assert_eq!(*alone, whole);
        }
        assert_eq!(super::module_to_run("no such module").unwrap(), None);
        assert_eq!(super::module_providing("Duration"), Some("time"));
    }

    #[test]
    fn a_mistake_in_a_module_says_where() {
        let problem = super::parse_module("csv", "struct A:\n    x = 1\n\ndef (\n").err().unwrap();
        assert!(
            problem.message.starts_with("the standard library's `csv.fen`, line 4: "),
            "{}",
            problem.message
        );
    }

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
