//! A program's declarations: its functions, methods, types and aliases, together with the
//! standard library's. Function bodies are looked at later, one function at a time.

use std::collections::BTreeMap;

use fennec_syntax::{Diagnostic, FnDef, Import, Imported, Item, Module, Span, TestDef, TypeDef, TypeExpr};

use crate::types::Type;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TypeKind {
    Struct,
    Enum,
    Error,
}

pub(crate) struct TypeInfo {
    pub(crate) kind: TypeKind,
    pub(crate) def: TypeDef,
    /// Whether it comes from the standard library rather than the program.
    pub(crate) prelude: bool,
}

pub(crate) struct FnEntry {
    pub(crate) def: FnDef,
    /// The type a method belongs to.
    pub(crate) owner: Option<String>,
    pub(crate) prelude: bool,
}

/// Every declaration in a program, keyed the way the signature table is: `name` for a
/// function, `Type.name` for a method, `outer.inner` for a nested function.
#[derive(Default)]
pub(crate) struct Program {
    pub(crate) types: BTreeMap<String, TypeInfo>,
    pub(crate) functions: BTreeMap<String, FnEntry>,
    pub(crate) tests: Vec<TestDef>,
    /// Builtins that exist only once imported, like `loads` from `std.json`, by the name
    /// they're imported under.
    pub(crate) imports: BTreeMap<String, String>,
    /// What the standard library's own code imports, which only that code sees.
    pub(crate) prelude_imports: BTreeMap<String, String>,
    /// Standard library types imported by name, like `Deque` from `std.collections`.
    pub(crate) imported_types: std::collections::BTreeSet<String>,
    /// Imported modules, by the name they're used under, like `json` in `json.loads(...)`.
    pub(crate) modules: BTreeMap<String, &'static str>,
    /// Imports of modules or names that don't exist.
    pub(crate) import_problems: Vec<Diagnostic>,
    aliases: BTreeMap<String, TypeExpr>,
}

/// How deeply type aliases may refer to each other, which also stops an alias naming itself.
const ALIAS_DEPTH: usize = 16;

impl Program {
    pub(crate) fn add(&mut self, module: &Module, prelude: bool) {
        for item in &module.items {
            match item {
                Item::Function(def) => {
                    let entry = FnEntry { def: def.clone(), owner: None, prelude };
                    self.functions.insert(def.name.clone(), entry);
                }
                Item::Struct(def) => self.add_type(def, TypeKind::Struct, prelude),
                Item::Enum(def) => self.add_type(def, TypeKind::Enum, prelude),
                Item::Error(def) => self.add_type(def, TypeKind::Error, prelude),
                Item::TypeAlias { name, ty, .. } => {
                    self.aliases.insert(name.clone(), ty.clone());
                }
                Item::Test(test) => self.tests.push(test.clone()),
                Item::Import(import) => self.add_import(import),
                Item::Interface(_) | Item::Stmt(_) => {}
            }
        }
    }

    fn add_import(&mut self, import: &Import) {
        let imported = match fennec_syntax::std_import(import) {
            Ok(imported) => imported,
            Err(problem) => {
                self.import_problems.push(problem);
                return;
            }
        };
        for item in imported {
            match item {
                Imported::Module { alias, module } => {
                    self.modules.insert(alias, module);
                }
                Imported::Name { name, module } => {
                    let builtin = format!("{module}.{name}");
                    if crate::builtins::is_builtin(&builtin) {
                        self.imports.insert(name, builtin);
                    } else {
                        self.imported_types.insert(name);
                    }
                }
            }
        }
    }

    fn add_type(&mut self, def: &TypeDef, kind: TypeKind, prelude: bool) {
        for method in &def.methods {
            let entry = FnEntry { def: method.clone(), owner: Some(def.name.clone()), prelude };
            self.functions.insert(format!("{}.{}", def.name, method.name), entry);
        }
        self.types.insert(def.name.clone(), TypeInfo { kind, def: def.clone(), prelude });
    }

    /// The builtin an imported name at `span` stands for.
    pub(crate) fn imported(&self, name: &str, span: Span) -> Option<&String> {
        let prelude = span.in_prelude().then(|| self.prelude_imports.get(name)).flatten();
        self.imports.get(name).or(prelude)
    }

    pub(crate) fn type_info(&self, name: &str) -> Option<&TypeInfo> {
        self.types.get(name)
    }

    pub(crate) fn kind_of(&self, ty: &Type) -> Option<TypeKind> {
        match ty {
            Type::Named(name) => self.type_info(name).map(|info| info.kind),
            _ => None,
        }
    }

    /// A method of a type, by its key in the signature table.
    pub(crate) fn method(&self, type_name: &str, method: &str) -> Option<(String, &FnEntry)> {
        let key = format!("{type_name}.{method}");
        self.functions.get(&key).map(|entry| (key, entry))
    }

    /// Whether a name, used as a value, means a type: `Enemy` in `Enemy.at(...)`.
    pub(crate) fn is_type_name(&self, name: &str) -> bool {
        self.types.contains_key(name) || matches!(name, "Id" | "IdMap" | "SoaList")
    }

    /// The enums that have a case with this name, in name order.
    pub(crate) fn enums_with_case(&self, case: &str) -> Vec<String> {
        self.types
            .values()
            .filter(|info| info.kind == TypeKind::Enum && info.def.cases.iter().any(|c| c.name == case))
            .map(|info| info.def.name.clone())
            .collect()
    }

    /// The type a written type means. Names the checker doesn't know, like a generic `T`,
    /// are `Unknown`.
    pub(crate) fn resolve(&self, written: &TypeExpr) -> Type {
        self.resolve_within(written, ALIAS_DEPTH)
    }

    fn resolve_within(&self, written: &TypeExpr, depth: usize) -> Type {
        let arg =
            |at: usize| written.args.get(at).map_or(Type::Unknown, |arg| self.resolve_within(arg, depth));
        let ty = match written.name.as_str() {
            "int" => Type::Int,
            "float" => Type::Float,
            "bool" => Type::Bool,
            "str" => Type::Str,
            "None" => Type::None,
            "list" | "SoaList" => Type::list(arg(0)),
            "set" => Type::Set(Box::new(arg(0))),
            "dict" => Type::Dict(Box::new(arg(0)), Box::new(arg(1))),
            "tuple" => Type::Tuple(written.args.iter().map(|arg| self.resolve_within(arg, depth)).collect()),
            "Id" => Type::Id(Box::new(arg(0))),
            "IdMap" => Type::IdMap(Box::new(arg(0))),
            "Stdout" | "Fs" | "Clock" => Type::named(&written.name),
            name if self.types.contains_key(name) => Type::named(name),
            name => match (self.aliases.get(name), depth.checked_sub(1)) {
                (Some(alias), Some(depth)) => self.resolve_within(alias, depth),
                _ => Type::Unknown,
            },
        };
        if written.optional { Type::optional(ty) } else { ty }
    }
}

/// A written type as source text, for suggesting a fix.
pub(crate) fn written(ty: &TypeExpr) -> String {
    let args: Vec<String> = ty.args.iter().map(written).collect();
    let base = if ty.name == "tuple" {
        format!("({})", args.join(", "))
    } else if args.is_empty() {
        ty.name.clone()
    } else {
        format!("{}[{}]", ty.name, args.join(", "))
    };
    if ty.optional { format!("{base}?") } else { base }
}
