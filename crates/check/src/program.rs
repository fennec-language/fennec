//! A program's declarations: its functions, methods, types and aliases, together with the
//! standard library's. Function bodies are looked at later, one function at a time.

use std::collections::BTreeMap;

use fennec_syntax::{
    Diagnostic, FnDef, Generic, Import, Imported, Interface, Item, Module, Span, TestDef, TypeDef, TypeExpr,
};

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

/// A constant at the top level: `KEYWORDS = {"def", "if"}`.
pub(crate) struct ConstEntry {
    pub(crate) ty: Option<TypeExpr>,
    pub(crate) value: fennec_syntax::Expr,
    pub(crate) span: Span,
    pub(crate) prelude: bool,
}

pub(crate) struct FnEntry {
    pub(crate) def: FnDef,
    /// The type a method belongs to.
    pub(crate) owner: Option<String>,
    pub(crate) prelude: bool,
    /// The type parameters of the functions around a nested function, which it can use.
    pub(crate) inherited: Vec<Generic>,
    /// For a method from an extension of a generic type, the type's parameters as the
    /// extension names them, with any bounds that make it conditional.
    pub(crate) extension: Vec<Generic>,
}

/// Every declaration in a program, keyed the way the signature table is: `name` for a
/// function, `Type.name` for a method, `outer.inner` for a nested function.
#[derive(Default)]
pub(crate) struct Program {
    pub(crate) types: BTreeMap<String, TypeInfo>,
    pub(crate) functions: BTreeMap<String, FnEntry>,
    pub(crate) tests: Vec<TestDef>,
    pub(crate) interfaces: BTreeMap<String, Interface>,
    pub(crate) constants: BTreeMap<String, ConstEntry>,
    /// Builtins that exist only once imported, like `loads` from `std.json`, by the name
    /// they're imported under.
    pub(crate) imports: BTreeMap<String, String>,
    /// What the standard library's own code imports, which only that code sees.
    pub(crate) prelude_imports: BTreeMap<String, String>,
    /// Standard library types imported by name, like `Deque` from `std.collections`.
    pub(crate) imported_types: std::collections::BTreeSet<String>,
    /// Imported modules, by the name they're used under, like `json` in `json.loads(...)`.
    pub(crate) modules: BTreeMap<String, &'static str>,
    /// Problems found while adding declarations: imports of modules or names that don't
    /// exist, and methods that can't be added.
    pub(crate) problems: Vec<Diagnostic>,
    aliases: BTreeMap<String, TypeExpr>,
}

/// How deeply type aliases may refer to each other, which also stops an alias naming itself.
const ALIAS_DEPTH: usize = 16;

impl Program {
    pub(crate) fn add(&mut self, module: &Module, prelude: bool) {
        for item in &module.items {
            match item {
                Item::Function(def) => {
                    let entry = FnEntry {
                        def: def.clone(),
                        owner: None,
                        prelude,
                        inherited: Vec::new(),
                        extension: Vec::new(),
                    };
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
                Item::Import(import) => self.add_import(import),
                Item::Const { name, ty, value, span } => {
                    let entry = ConstEntry { ty: ty.clone(), value: value.clone(), span: *span, prelude };
                    self.constants.insert(name.clone(), entry);
                }
                Item::Extend(_) | Item::Stmt(_) => {}
            }
        }
        // Extensions come last, so the type they extend is known wherever it's declared.
        for item in &module.items {
            if let Item::Extend(extension) = item {
                self.add_extension(extension, prelude);
            }
        }
    }

    fn add_import(&mut self, import: &Import) {
        let imported = match fennec_syntax::std_import(import) {
            Ok(imported) => imported,
            Err(problem) => {
                self.problems.push(problem);
                return;
            }
        };
        for item in imported {
            match item {
                Imported::Module { alias, module } => {
                    self.modules.insert(alias, module);
                }
                Imported::Name { name, alias, module } => {
                    let builtin = format!("{module}.{name}");
                    if crate::builtins::is_builtin(&builtin) {
                        self.imports.insert(alias, builtin);
                    } else {
                        self.imported_types.insert(name);
                    }
                }
            }
        }
    }

    fn add_type(&mut self, def: &TypeDef, kind: TypeKind, prelude: bool) {
        // A program's own type replaces a standard library type of the same name, methods too.
        let methods = format!("{}.", def.name);
        self.functions.retain(|key, entry| !(!prelude && entry.prelude && key.starts_with(&methods)));
        for method in &def.methods {
            self.add_method(&def.name, method, prelude, &[]);
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

    /// The type a written type means, outside any generic function.
    pub(crate) fn resolve(&self, written: &TypeExpr) -> Result<Type, Diagnostic> {
        self.resolve_in(written, &[])
    }

    /// The type a written type means, with these type parameters in scope.
    pub(crate) fn resolve_in(&self, written: &TypeExpr, generics: &[String]) -> Result<Type, Diagnostic> {
        self.resolve_within(written, generics, ALIAS_DEPTH)
    }

    fn resolve_within(
        &self,
        written: &TypeExpr,
        generics: &[String],
        depth: usize,
    ) -> Result<Type, Diagnostic> {
        // `std.collections.Deque` or `collections.Deque`, through an imported module.
        let name = match written.name.rsplit_once('.') {
            Some((path, member)) if self.modules.contains_key(path) && self.types.contains_key(member) => {
                member
            }
            _ => written.name.as_str(),
        };
        if self.is_interface(name) {
            return Err(interface_as_type(name, written.span));
        }
        let builtin = BUILT_IN_TYPES.contains(&name);
        let declared = generics.iter().any(|generic| generic == name)
            || self.types.contains_key(name)
            || self.aliases.contains_key(name);
        if !builtin && !declared {
            return Err(unknown_type(name, written.span));
        }
        let wanted = match name {
            "->" => written.args.len().max(1),
            "list" | "set" | "SoaList" | "Id" | "IdMap" | "Iterator" => 1,
            "dict" | "Result" => 2,
            "tuple" => written.args.len(),
            name => self.type_params(name).len(),
        };
        if written.args.len() != wanted {
            return Err(wrong_type_args(written, wanted));
        }
        let mut args = Vec::new();
        for arg in &written.args {
            args.push(self.resolve_within(arg, generics, depth)?);
        }
        let mut items = args.clone().into_iter();
        let mut arg = || Box::new(items.next().unwrap_or(Type::Unknown));
        let ty = match name {
            "int" => Type::Int,
            "float" => Type::Float,
            "bool" => Type::Bool,
            "str" => Type::Str,
            "bytes" => Type::Bytes,
            "None" => Type::None,
            // What a function gives back when it gives back nothing, for writing the return
            // type of one that calls itself.
            "nothing" => Type::Unit,
            "list" | "SoaList" => Type::List(arg()),
            "Iterator" => Type::Iterator(arg()),
            "set" => Type::Set(arg()),
            "dict" => Type::Dict(arg(), arg()),
            "Result" => Type::Result(arg(), arg()),
            "tuple" => Type::Tuple(args),
            "->" => {
                let mut params = args;
                let ret = params.pop().unwrap_or(Type::Unit);
                Type::Arrow(params, Box::new(ret))
            }
            "Id" => Type::Id(arg()),
            "IdMap" => Type::IdMap(arg()),
            "Stdout" | "Fs" | "Clock" | "Args" | "Stdin" => Type::named(name),
            name if generics.iter().any(|generic| generic == name) => Type::Param(name.to_owned()),
            name if self.types.contains_key(name) && !args.is_empty() => Type::Generic(name.to_owned(), args),
            name if self.types.contains_key(name) => Type::named(name),
            name => match (self.aliases.get(name), depth.checked_sub(1)) {
                (Some(alias), Some(depth)) => self.resolve_within(alias, generics, depth)?,
                _ => {
                    return Err(Diagnostic::new(
                        format!("the type alias `{name}` refers to itself"),
                        written.span,
                    ));
                }
            },
        };
        Ok(if written.optional { Type::optional(ty) } else { ty })
    }

    /// Whether a program can write a type's name where it is: it's built in, declared in the
    /// program, or imported from the standard library.
    pub(crate) fn can_name(&self, name: &str) -> bool {
        fennec_syntax::importable_type(name).is_none()
            || self.types.get(name).is_some_and(|info| !info.prelude)
            || self.imported_types.contains(name)
    }

    /// Whether a name is an interface: one the program declares, or a built-in one.
    pub(crate) fn is_interface(&self, name: &str) -> bool {
        self.interfaces.contains_key(name) || BUILT_IN_INTERFACES.contains(&name)
    }

    /// A type's type parameters, in order: `T` for `Deque[T]`.
    pub(crate) fn type_params(&self, name: &str) -> Vec<String> {
        self.types
            .get(name)
            .map(|info| info.def.generics.iter().map(|generic| generic.name.clone()).collect())
            .unwrap_or_default()
    }

    /// What each of a generic type's parameters stands for in a value's type: `T` is `int`
    /// in `Deque[int]`.
    pub(crate) fn type_args(&self, ty: &Type) -> Vec<(String, Type)> {
        match ty {
            Type::Generic(name, args) => {
                self.type_params(name).into_iter().zip(args.iter().cloned()).collect()
            }
            _ => Vec::new(),
        }
    }

    /// The names a method's owner gives its type parameters, in order.
    pub(crate) fn owner_params(&self, entry: &FnEntry) -> Vec<String> {
        if entry.extension.is_empty() {
            return entry.owner.as_ref().map(|owner| self.type_params(owner)).unwrap_or_default();
        }
        entry.extension.iter().map(|generic| generic.name.clone()).collect()
    }

    /// The type of `self` in a method: a built-in generic type with its extension's
    /// parameters, like `list[T]`, or the owner's own type.
    pub(crate) fn method_self_type(&self, entry: &FnEntry, owner: &str) -> Type {
        let params: Vec<Type> =
            entry.extension.iter().map(|generic| Type::Param(generic.name.clone())).collect();
        Type::built_in_generic(owner, &params).unwrap_or_else(|| self.self_type(owner))
    }

    /// The type of `self` in a type's methods: `Deque[T]` for a `Deque`.
    pub(crate) fn self_type(&self, name: &str) -> Type {
        if let Some(built_in) = Type::built_in(name) {
            return built_in;
        }
        let params = self.type_params(name);
        if params.is_empty() {
            return Type::named(name);
        }
        Type::Generic(name.to_owned(), params.into_iter().map(Type::Param).collect())
    }

    /// The type parameters in scope in a function: its type's, then its own.
    pub(crate) fn generics_of(&self, entry: &FnEntry) -> Vec<Generic> {
        let owner = entry.owner.as_ref().and_then(|owner| self.types.get(owner));
        let declared = owner.map(|info| info.def.generics.clone()).unwrap_or_default();
        // An extension's parameters keep the bounds the declaration gives them, and add its own.
        let owner = if entry.extension.is_empty() {
            declared
        } else {
            let mut generics = entry.extension.clone();
            for (generic, declared) in generics.iter_mut().zip(declared) {
                generic.bounds.extend(declared.bounds);
            }
            generics
        };
        owner
            .into_iter()
            .chain(entry.inherited.iter().cloned())
            .chain(entry.def.generics.iter().cloned())
            .collect()
    }
}

/// The interfaces every type that can fits automatically.
pub(crate) const BUILT_IN_INTERFACES: [&str; 4] = ["Eq", "Hashable", "Ordered", "Format"];

pub(crate) const BUILT_IN_TYPES: [&str; 22] = [
    "int", "float", "bool", "str", "bytes", "None", "nothing", "list", "SoaList", "set", "dict", "tuple",
    "Id", "IdMap", "Stdout", "Fs", "Clock", "Args", "Stdin", "Result", "Iterator", "->",
];

/// An interface written where a type goes, as in `list[Shape]`.
fn interface_as_type(name: &str, span: Span) -> Diagnostic {
    Diagnostic::new(format!("`{name}` is an interface, not a type"), span).with_help(format!(
        "take a type parameter that fits it, like `def f[T: {name}](items: list[T])`, or use an enum for a mix of types"
    ))
}

fn wrong_type_args(written: &TypeExpr, wanted: usize) -> Diagnostic {
    let name = &written.name;
    let example = match name.as_str() {
        "dict" => "dict[str, int]".to_owned(),
        "Result" => "Result[int, IoError]".to_owned(),
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

pub(crate) fn unknown_type(name: &str, span: Span) -> Diagnostic {
    let diagnostic = Diagnostic::new(format!("`{name}` isn't a type"), span);
    match name {
        "f32" | "f64" | "double" => diagnostic.with_help("write `float`"),
        "i32" | "i64" | "usize" | "long" => diagnostic.with_help("write `int`"),
        "string" | "String" => diagnostic.with_help("write `str`"),
        "boolean" | "Bool" => diagnostic.with_help("write `bool`"),
        "List" | "Dict" | "Set" => diagnostic.with_help(format!("write `{}`", name.to_lowercase())),
        "Optional" => diagnostic.with_help("write `?` after the type, like `Enemy?`"),
        "Self" => diagnostic
            .with_help("`Self` means the type that fits an interface, so it goes in an interface's methods"),
        _ => diagnostic.with_help(format!("declare it, like `struct {name}:`, or check the spelling")),
    }
}

/// The return type a `def` writes. `-> None` counts as none, since no arrow already means it
/// gives back nothing; it's reported, with a fix that removes it.
pub(crate) fn written_ret(def: &FnDef) -> Option<&TypeExpr> {
    def.ret.as_ref().filter(|ret| {
        !(matches!(ret.name.as_str(), "None" | "nothing") && ret.args.is_empty() && !ret.optional)
    })
}

/// A written type as source text, for suggesting a fix.
pub(crate) fn written(ty: &TypeExpr) -> String {
    let args: Vec<String> = ty.args.iter().map(written).collect();
    if ty.name == "->" {
        let (ret, params) = args.split_last().map_or(("", &[][..]), |(ret, params)| (ret.as_str(), params));
        return format!("({}) -> {ret}", params.join(", "));
    }
    let base = if ty.name == "tuple" {
        format!("({})", args.join(", "))
    } else if args.is_empty() {
        ty.name.clone()
    } else {
        format!("{}[{}]", ty.name, args.join(", "))
    };
    if ty.optional { format!("{base}?") } else { base }
}
