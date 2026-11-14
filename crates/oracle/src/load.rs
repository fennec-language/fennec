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

/// Fields by name, each with the type the checker gave it, in the order they're written.
pub(crate) type TypedFields = Vec<(String, Type)>;

/// An enum's cases by name, each with its fields.
pub(crate) type TypedCases = Vec<(String, TypedFields)>;

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
    /// What each name of a standard library module run from its own source is called in these
    /// tables, by its whole name: `std.math.Vec3` is `Vec3`, and `std.math.dot` is `math.dot`.
    pub(crate) from_std: HashMap<String, String>,
    /// The type the checker gave each expression of the program, and of the standard library code
    /// it reaches, by its span.
    pub(crate) types_at: HashMap<(u32, u32), Type>,
    pub(crate) main: &'static FnDef,
    /// The type each method is of, by where its definition is.
    pub(crate) owners: HashMap<usize, String>,
    /// Whether any type has a `where` rule, which every write then checks.
    pub(crate) has_rules: bool,
    /// The type of each field of each struct the program declares, as the checker worked it out,
    /// in the order they're written, for saving and loading by type.
    pub(crate) field_types: HashMap<String, TypedFields>,
    /// The same for each case of each enum the program declares.
    pub(crate) case_types: HashMap<String, TypedCases>,
}

impl Program {
    /// The type a method is of, which `Self` names in it.
    pub(crate) fn owner_of(&self, def: &FnDef) -> Option<&str> {
        self.owners.get(&(std::ptr::from_ref(def) as usize)).map(String::as_str)
    }

    /// The types of a struct's fields, or of an enum's case's, as the checker gave them.
    pub(crate) fn fields_typed(&self, ty: &str, case: Option<&str>) -> Option<&[(String, Type)]> {
        match case {
            None => self.field_types.get(ty).map(Vec::as_slice),
            Some(case) => self
                .case_types
                .get(ty)?
                .iter()
                .find(|(name, _)| name == case)
                .map(|(_, fields)| fields.as_slice()),
        }
    }

    pub(crate) fn type_at(&self, span: Span) -> Option<&Type> {
        self.types_at.get(&(span.start, span.end))
    }

    /// What a name the program imports from a standard library module run from its own source
    /// is called in the tables.
    pub(crate) fn std_name(&self, imported_as: &str) -> Option<&str> {
        self.from_std.get(self.imported.get(imported_as)?).map(String::as_str)
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
    // Every expression the checker typed: the program's own, and those of the standard library
    // code it reaches, which a module run from its own source needs.
    let mut types_at = HashMap::new();
    let typed = typed_fields(&module, &checked.signatures);
    for (_, body) in checked.types.iter() {
        for (span, ty) in body.exprs() {
            types_at.insert((span.start, span.end), ty.clone());
        }
    }
    // The tree is kept for the rest of the process, since values and generators running on
    // other threads point into it.
    let module: &'static Module = Box::leak(Box::new(module));
    let mut program = tables(module, sources, types_at)?;
    (program.field_types, program.case_types) = typed;
    Ok(Box::leak(Box::new(program)))
}

/// The standard library's modules the oracle runs from their own Fennec source, as the compiler
/// builds them. Only the functions the runtime writes in Rust beneath them are modeled here, in
/// `interp/library.rs`, so a bug in one of these modules' own Fennec would show on both sides.
const RUN_FROM_SOURCE: &[&str] = &["math", "collections", "env", "time", "string"];

/// The capabilities `main` may take whose type a module run from its own source declares, which
/// that module is loaded for even when the program imports nothing from it: `Env` is
/// `std.env`'s, with its `secret` and its `Missing` written in Fennec, and a `Clock` gives
/// `std.time`'s `Instant` and `Time`.
const CAPABILITY_MODULES: &[(&str, &str)] = &[("Env", "env"), ("Clock", "time")];

/// The types the oracle models on its own, whose methods a module run from its source adds in an
/// `extend` are left out: `std.collections` extends `Table` and `Id`, which `interp/tables.rs`
/// models.
const MODELED_HERE: &[&str] = &["Table", "Id"];

/// What the tables are made from: the program's own declarations, and those of each standard
/// library module it imports that's run from its own source.
#[derive(Default)]
struct Gathered {
    functions: HashMap<String, &'static FnDef>,
    types: HashMap<String, Declared>,
    extensions: HashMap<String, Vec<&'static FnDef>>,
    constants: HashMap<String, &'static Expr>,
    aliases: HashMap<String, Vec<String>>,
    imported: HashMap<String, String>,
    from_std: HashMap<String, String>,
}

impl Gathered {
    /// Takes in one of a module's items: the program's own, or, with `std` naming it, one of a
    /// standard library module's, which mustn't share a name with anything already taken in.
    fn take(&mut self, item: &'static Item, std: Option<&str>) -> Result<(), Refused> {
        let declared = match item {
            Item::Function(def) => {
                self.unclaimed(&def.name, std)?;
                self.functions.insert(def.name.clone(), def);
                // A module's functions are named `module.function` when it's run.
                std.zip(def.name.split_once('.')).map(|(module, (_, short))| (module, short, &def.name))
            }
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                let kind = match item {
                    Item::Struct(_) => Kind::Struct,
                    Item::Enum(_) => Kind::Enum,
                    _ => Kind::Error,
                };
                self.unclaimed(&def.name, std)?;
                self.types.insert(def.name.clone(), Declared { def, kind, ruled: ruled(def) });
                std.map(|module| (module, def.name.as_str(), &def.name))
            }
            Item::Extend(extension) if std.is_some() && MODELED_HERE.contains(&extension.name.as_str()) => {
                None
            }
            Item::Extend(extension) => {
                self.extensions.entry(extension.name.clone()).or_default().extend(extension.methods.iter());
                None
            }
            Item::Const { name, value, .. } => {
                self.unclaimed(name, std)?;
                self.constants.insert(name.clone(), value);
                std.map(|module| (module, name.as_str(), name))
            }
            Item::TypeAlias { name, ty, .. } => {
                let members = if ty.name == "|" {
                    ty.args.iter().map(|arg| arg.name.clone()).collect()
                } else {
                    vec![ty.name.clone()]
                };
                self.aliases.insert(name.clone(), members);
                None
            }
            Item::Import(import) => {
                bring_in(import, &mut self.imported)?;
                None
            }
            Item::Interface(_) | Item::Test(_) => None,
            Item::Stmt(_) => return Err(Refused::Unsupported("a statement at the top level".to_owned())),
        };
        if let Some((module, short, name)) = declared {
            self.from_std.insert(format!("std.{module}.{short}"), name.clone());
        }
        Ok(())
    }

    /// Refuses a standard library module's declaration that has a name the program, or another
    /// module, already gave something.
    fn unclaimed(&self, name: &str, std: Option<&str>) -> Result<(), Refused> {
        let taken = self.functions.contains_key(name)
            || self.types.contains_key(name)
            || self.constants.contains_key(name);
        match std {
            Some(module) if taken => {
                Err(Refused::Unsupported(format!("`{name}`, declared by the program and by std.{module}")))
            }
            _ => Ok(()),
        }
    }

    /// Whether `main` takes a capability whose type `module` declares.
    fn main_takes(&self, module: &str) -> bool {
        let Some(main) = self.functions.get("main") else { return false };
        CAPABILITY_MODULES.iter().any(|(cap, from)| {
            *from == module
                && main.params.iter().any(|param| param.ty.as_ref().is_some_and(|ty| ty.name == *cap))
        })
    }

    /// Takes in each standard library module the program imports that's run from its own
    /// source, and each such module those import in turn, as `std.time` imports `std.string`.
    /// The checker typed their code as it checked the program.
    fn std_modules(&mut self) -> Result<(), Refused> {
        let mut wanted: Vec<&'static str> = RUN_FROM_SOURCE
            .iter()
            .copied()
            .filter(|module| {
                let whole = format!("std.{module}");
                self.imported.values().any(|from| *from == whole || from.starts_with(&format!("{whole}.")))
                    || self.main_takes(module)
            })
            .collect();
        let mut loaded = Vec::new();
        while let Some(name) = wanted.pop() {
            if loaded.contains(&name) {
                continue;
            }
            loaded.push(name);
            let module = fennec_syntax::module_to_run(name)
                .map_err(|problem| Refused::Invalid(problem.message.clone()))?
                .ok_or_else(|| Refused::Unsupported(format!("std.{name}")))?;
            for item in &module.items {
                if let Item::Import(import) = item {
                    wanted.extend(
                        modules_imported(import)
                            .filter_map(|used| RUN_FROM_SOURCE.iter().copied().find(|run| *run == used)),
                    );
                }
                self.take(item, Some(name))?;
            }
        }
        Ok(())
    }
}

/// The standard library modules an import names: `from std.string import ...` names `string`,
/// and `from std import math, json` names both.
fn modules_imported(import: &fennec_syntax::Import) -> impl Iterator<Item = String> + '_ {
    let named = match import.module.as_slice() {
        [std, module, ..] if std == "std" => vec![module.clone()],
        [std] if std == "std" => import.bound().map(|(name, _)| name.to_owned()).collect(),
        _ => Vec::new(),
    };
    named.into_iter()
}

fn tables(
    module: &'static Module,
    sources: Sources,
    types_at: HashMap<(u32, u32), Type>,
) -> Result<Program, Refused> {
    let mut gathered = Gathered::default();
    for item in &module.items {
        gathered.take(item, None)?;
    }
    gathered.std_modules()?;
    let Gathered { functions, types, extensions, constants, aliases, imported, from_std } = gathered;
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
        from_std,
        types_at,
        main,
        owners,
        has_rules,
        field_types: HashMap::new(),
        case_types: HashMap::new(),
    })
}

/// What the checker gave each field of the program's own structs and enums. A type the program
/// names as the standard library names one of its own is checked under another name, which is
/// looked for first.
fn typed_fields(
    module: &Module,
    signatures: &fennec_check::Signatures,
) -> (HashMap<String, TypedFields>, HashMap<String, TypedCases>) {
    let mut fields = HashMap::new();
    let mut cases = HashMap::new();
    for item in &module.items {
        match item {
            Item::Struct(def) => {
                let renamed = format!("{}{}", def.name, fennec_syntax::rename::RENAMED);
                if let Some(found) =
                    signatures.structure(&renamed).or_else(|| signatures.structure(&def.name))
                {
                    fields.insert(def.name.clone(), found.fields.clone());
                }
            }
            Item::Enum(def) => {
                let renamed = format!("{}{}", def.name, fennec_syntax::rename::RENAMED);
                if let Some(found) =
                    signatures.enumeration(&renamed).or_else(|| signatures.enumeration(&def.name))
                {
                    cases.insert(def.name.clone(), found.cases.clone());
                }
            }
            _ => {}
        }
    }
    (fields, cases)
}

/// The capabilities `main` may take here: standard output and standard error, which are real,
/// the arguments and standard input, which are empty, the file system, which is modeled, the
/// environment, which is read, `Signals`, which never says to stop, and a `Clock`, read from the
/// system's, whose `sleep` moves it on without waiting.
fn given_to_main(main: &FnDef) -> Result<(), Refused> {
    for param in &main.params {
        let ty = param.ty.as_ref().map_or("", |ty| ty.name.as_str());
        if !matches!(ty, "Stdout" | "Args" | "Stdin" | "Fs" | "Stderr" | "Signals" | "Env" | "Clock") {
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
