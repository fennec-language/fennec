//! A program's declarations: its functions, methods, types and aliases, together with the
//! standard library's. Function bodies are looked at later, one function at a time.

use std::collections::BTreeMap;

use fennec_syntax::{Diagnostic, FnDef, Interface, Item, Module, TestDef, TypeDef, TypeExpr};

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
    pub(crate) interfaces: BTreeMap<String, Interface>,
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
                Item::Interface(interface) => {
                    self.interfaces.insert(interface.name.clone(), interface.clone());
                }
                Item::Import(_) | Item::Stmt(_) => {}
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
        self.types.contains_key(name) || matches!(name, "Id" | "IdMap" | "Path" | "SoaList")
    }

    /// The enums that have a case with this name, in name order.
    pub(crate) fn enums_with_case(&self, case: &str) -> Vec<String> {
        self.types
            .values()
            .filter(|info| info.kind == TypeKind::Enum && info.def.cases.iter().any(|c| c.name == case))
            .map(|info| info.def.name.clone())
            .collect()
    }

    /// The type a written type means, outside any generic function.
    pub(crate) fn resolve(&self, written: &TypeExpr) -> Result<Type, Diagnostic> {
        self.resolve_in(written, &[])
    }

    /// The type a written type means, with these type parameters in scope. An interface
    /// stands for a type parameter of its own, so `list[Shape]` is a list of any one type
    /// that fits `Shape`.
    pub(crate) fn resolve_in(&self, written: &TypeExpr, generics: &[String]) -> Result<Type, Diagnostic> {
        self.resolve_within(written, generics, ALIAS_DEPTH)
    }

    fn resolve_within(&self, written: &TypeExpr, generics: &[String], depth: usize) -> Result<Type, Diagnostic> {
        let name = written.name.as_str();
        let builtin = matches!(
            name,
            "int" | "float" | "bool" | "str" | "Path" | "None" | "list" | "SoaList" | "set" | "dict" | "tuple"
                | "Id" | "IdMap" | "Stdout" | "Fs"
        );
        let declared = generics.iter().any(|generic| generic == name)
            || self.interfaces.contains_key(name)
            || self.types.contains_key(name)
            || self.aliases.contains_key(name);
        if !builtin && !declared {
            return Err(unknown_type(name, written.span));
        }
        let wanted = match name {
            "list" | "set" | "SoaList" | "Id" | "IdMap" => 1,
            "dict" => 2,
            "tuple" => written.args.len(),
            _ => 0,
        };
        if written.args.len() != wanted {
            return Err(wrong_type_args(written, wanted));
        }
        let mut args = Vec::new();
        for arg in &written.args {
            args.push(self.resolve_within(arg, generics, depth)?);
        }
        let mut args = args.into_iter();
        let mut arg = || Box::new(args.next().unwrap_or(Type::Unknown));
        let ty = match name {
            "int" => Type::Int,
            "float" => Type::Float,
            "bool" => Type::Bool,
            "str" | "Path" => Type::Str,
            "None" => Type::None,
            "list" | "SoaList" => Type::List(arg()),
            "set" => Type::Set(arg()),
            "dict" => Type::Dict(arg(), arg()),
            "tuple" => Type::Tuple(written.args.iter().map(|_| *arg()).collect()),
            "Id" => Type::Id(arg()),
            "IdMap" => Type::IdMap(arg()),
            "Stdout" | "Fs" => Type::named(name),
            name if generics.iter().any(|generic| generic == name) || self.interfaces.contains_key(name) => {
                Type::Param(name.to_owned())
            }
            name if self.types.contains_key(name) => Type::named(name),
            name => match (self.aliases.get(name), depth.checked_sub(1)) {
                (Some(alias), Some(depth)) => self.resolve_within(alias, generics, depth)?,
                _ => {
                    return Err(Diagnostic::new(format!("the type alias `{name}` refers to itself"), written.span));
                }
            },
        };
        Ok(if written.optional { Type::optional(ty) } else { ty })
    }
}

fn wrong_type_args(written: &TypeExpr, wanted: usize) -> Diagnostic {
    let name = &written.name;
    let example = match name.as_str() {
        "dict" => "dict[str, int]".to_owned(),
        "list" | "set" | "SoaList" => format!("{name}[int]"),
        "Id" | "IdMap" => format!("{name}[Enemy]"),
        _ => name.clone(),
    };
    let message = if wanted == 0 {
        format!("`{name}` doesn't take types in brackets")
    } else {
        format!("`{name}` needs {wanted} type{} in brackets", if wanted == 1 { "" } else { "s" })
    };
    Diagnostic::new(message, written.span).with_help(format!("write it like `{example}`"))
}

fn unknown_type(name: &str, span: fennec_syntax::Span) -> Diagnostic {
    let diagnostic = Diagnostic::new(format!("`{name}` isn't a type"), span);
    match name {
        "f32" | "f64" | "double" => diagnostic.with_help("write `float`"),
        "i32" | "i64" | "usize" | "long" => diagnostic.with_help("write `int`"),
        "string" | "String" => diagnostic.with_help("write `str`"),
        "boolean" | "Bool" => diagnostic.with_help("write `bool`"),
        "List" | "Dict" | "Set" => diagnostic.with_help(format!("write `{}`", name.to_lowercase())),
        "Optional" => diagnostic.with_help("write `?` after the type, like `Enemy?`"),
        _ => diagnostic.with_help(format!("declare it, like `struct {name}:`, or check the spelling")),
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
