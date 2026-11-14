//! Getting a program to run: its source parsed and checked by the compiler's own parser and
//! checker, and what the oracle needs to find quickly gathered into tables.

use std::collections::HashMap;
use std::path::Path;

use fennec_check::Type;
use fennec_syntax::{Expr, FnDef, Item, Module, Sources, Span, TypeDef};

/// Why a program can't be run here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    /// Something it uses that the oracle doesn't model, named.
    Unsupported(String),
    /// It couldn't be read, or the checker refuses it.
    Invalid(String),
}

/// What kind of type a declaration makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Struct,
    Enum,
    Error,
}

/// A type the program declares.
pub(crate) struct Declared {
    pub(crate) def: &'static TypeDef,
    pub(crate) kind: Kind,
    /// Whether any of its fields has a `where` rule.
    pub(crate) ruled: bool,
}

/// A program ready to run: its syntax tree, kept for the whole run, and tables over it.
pub struct Program {
    /// The program's files, as they're shown, for what it says as it stops.
    pub(crate) sources: Sources,
    pub(crate) functions: HashMap<String, &'static FnDef>,
    pub(crate) types: HashMap<String, Declared>,
    /// The methods `extend` adds, by the type they're for, the built-in types' included.
    pub(crate) extensions: HashMap<String, Vec<&'static FnDef>>,
    pub(crate) constants: HashMap<String, &'static Expr>,
    /// Each `type Name = A | B`, by name, as the names of its members.
    pub(crate) aliases: HashMap<String, Vec<String>>,
    /// The names an import brings in, each with the standard library module it's from.
    pub(crate) imported: HashMap<String, String>,
    /// The type the checker gave each expression of the program's own code, by its span.
    pub(crate) types_at: HashMap<(u32, u32), Type>,
    pub(crate) main: &'static FnDef,
    /// The type each method is of, by where its definition is.
    pub(crate) owners: HashMap<usize, String>,
    /// Whether any type has a `where` rule, which every write then checks.
    pub(crate) has_rules: bool,
}

impl Program {
    /// The type a method is of, which `Self` names in it.
    pub(crate) fn owner_of(&self, def: &FnDef) -> Option<&str> {
        self.owners.get(&(std::ptr::from_ref(def) as usize)).map(String::as_str)
    }

    pub(crate) fn type_at(&self, span: Span) -> Option<&Type> {
        self.types_at.get(&(span.start, span.end))
    }

    /// The method `name` of the type called `ty`, written in the type or in an `extend` of it.
    pub(crate) fn method(&self, ty: &str, name: &str) -> Option<&'static FnDef> {
        let own =
            self.types.get(ty).and_then(|declared| declared.def.methods.iter().find(|def| def.name == name));
        own.or_else(|| self.extensions.get(ty)?.iter().copied().find(|def| def.name == name))
    }
}

/// Reads, parses and checks the program at `path`: a `.fen` file, or a kit's folder, with the
/// modules of its own that it imports.
pub fn load(path: &Path, shown: &str) -> Result<&'static Program, Refused> {
    let merged = if path.is_dir() { crate::kit::kit(path, shown)? } else { crate::kit::file(path, shown)? };
    load_module(merged.module, merged.sources)
}

/// Parses and checks a program's source, shown as `shown`.
pub fn load_source(source: &str, shown: &str) -> Result<&'static Program, Refused> {
    let mut sources = Sources::new();
    let base = sources.add(shown, source);
    let module = fennec_syntax::parse_at(source, base)
        .map_err(|problem| Refused::Invalid(sources.render(&problem)))?;
    for item in &module.items {
        if let Item::Import(import) = item
            && import.module.first().is_none_or(|first| first != "std")
        {
            return Err(Refused::Unsupported("importing the program's own modules".to_owned()));
        }
    }
    load_module(module, sources)
}

/// Checks a program's module, with every file it's from.
fn load_module(module: Module, sources: Sources) -> Result<&'static Program, Refused> {
    if let Err(problems) = fennec_check::check(&module) {
        return Err(Refused::Invalid(sources.render_all(&problems)));
    }
    let checked =
        fennec_check::check_typed(&module).map_err(|_| Refused::Invalid("it doesn't check".to_owned()))?;
    let mut types_at = HashMap::new();
    for (_, body) in checked.types.iter() {
        for (span, ty) in body.exprs() {
            if span.start < Span::PRELUDE {
                types_at.insert((span.start, span.end), ty.clone());
            }
        }
    }
    // The tree is kept for the rest of the process, since values and generators running on
    // other threads point into it.
    let module: &'static Module = Box::leak(Box::new(module));
    let program = tables(module, sources, types_at)?;
    Ok(Box::leak(Box::new(program)))
}

fn tables(
    module: &'static Module,
    sources: Sources,
    types_at: HashMap<(u32, u32), Type>,
) -> Result<Program, Refused> {
    let mut functions = HashMap::new();
    let mut types = HashMap::new();
    let mut extensions: HashMap<String, Vec<&'static FnDef>> = HashMap::new();
    let mut constants = HashMap::new();
    let mut aliases = HashMap::new();
    let mut imported = HashMap::new();
    for item in &module.items {
        match item {
            Item::Function(def) => {
                functions.insert(def.name.clone(), def);
            }
            Item::Struct(def) => {
                types.insert(def.name.clone(), Declared { def, kind: Kind::Struct, ruled: ruled(def) });
            }
            Item::Enum(def) => {
                types.insert(def.name.clone(), Declared { def, kind: Kind::Enum, ruled: ruled(def) });
            }
            Item::Error(def) => {
                types.insert(def.name.clone(), Declared { def, kind: Kind::Error, ruled: ruled(def) });
            }
            Item::Extend(extension) => {
                extensions.entry(extension.name.clone()).or_default().extend(extension.methods.iter());
            }
            Item::Const { name, value, .. } => {
                constants.insert(name.clone(), value);
            }
            Item::TypeAlias { name, ty, .. } => {
                let members = if ty.name == "|" {
                    ty.args.iter().map(|arg| arg.name.clone()).collect()
                } else {
                    vec![ty.name.clone()]
                };
                aliases.insert(name.clone(), members);
            }
            Item::Import(import) => bring_in(import, &mut imported)?,
            Item::Interface(_) | Item::Test(_) => {}
            Item::Stmt(_) => return Err(Refused::Unsupported("a statement at the top level".to_owned())),
        }
    }
    let mut owners = HashMap::new();
    for (name, declared) in &types {
        for def in &declared.def.methods {
            owners.insert(std::ptr::from_ref(def) as usize, name.clone());
        }
    }
    for (name, methods) in &extensions {
        for def in methods {
            owners.insert(std::ptr::from_ref(*def) as usize, name.clone());
        }
    }
    let has_rules = types.values().any(|declared: &Declared| declared.ruled);
    let main =
        *functions.get("main").ok_or_else(|| Refused::Invalid("the program has no `main`".to_owned()))?;
    given_to_main(main)?;
    Ok(Program {
        sources,
        functions,
        types,
        extensions,
        constants,
        aliases,
        imported,
        types_at,
        main,
        owners,
        has_rules,
    })
}

/// The capabilities `main` may take here: standard output, which is real, and the arguments and
/// standard input, which are empty.
fn given_to_main(main: &FnDef) -> Result<(), Refused> {
    for param in &main.params {
        let ty = param.ty.as_ref().map_or("", |ty| ty.name.as_str());
        if !matches!(ty, "Stdout" | "Args" | "Stdin") {
            return Err(Refused::Unsupported(format!("the capability {ty}")));
        }
    }
    Ok(())
}

/// Notes the names a standard library import brings in, each with the module it's from. Two
/// files of a program that bring in one name from different places are unsupported.
fn bring_in(import: &fennec_syntax::Import, imported: &mut HashMap<String, String>) -> Result<(), Refused> {
    let module_name = import.module.iter().skip(1).cloned().collect::<Vec<_>>().join(".");
    let mut brought = Vec::new();
    for (name, bound) in import.bound() {
        let from =
            if module_name.is_empty() { format!("std.{name}") } else { format!("std.{module_name}.{name}") };
        brought.push((bound.to_owned(), from));
    }
    if import.names.is_empty() {
        let bound = import.alias.clone().unwrap_or_else(|| import.module.join("."));
        let last = import.module.last().cloned().unwrap_or_default();
        brought.push((bound, format!("std.{module_name}")));
        brought.push((last, format!("std.{module_name}")));
    }
    for (bound, from) in brought {
        if imported.get(&bound).is_some_and(|known| *known != from) {
            return Err(Refused::Unsupported(format!("`{bound}` imported from two places")));
        }
        imported.insert(bound, from);
    }
    Ok(())
}

/// Whether a type has a field with a `where` rule.
fn ruled(def: &TypeDef) -> bool {
    def.fields.iter().any(|field| field.rule.is_some())
}
