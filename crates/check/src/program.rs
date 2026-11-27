//! A program's declarations: its functions, methods, types and aliases, together with the
//! standard library's. Function bodies are looked at later, one function at a time.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;

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

/// A declaration's definition: the standard library's, held where its parse is kept for the
/// whole process, or the program's own. The library's are the same for every program, so they're
/// never copied.
#[derive(Debug)]
pub(crate) enum Held<T: 'static> {
    Library(&'static T),
    Own(std::sync::Arc<T>),
}

impl<T> Clone for Held<T> {
    fn clone(&self) -> Self {
        match self {
            Held::Library(def) => Held::Library(def),
            Held::Own(def) => Held::Own(std::sync::Arc::clone(def)),
        }
    }
}

/// Two are the same when what they hold is: at once when they hold the same one, as the library's
/// always do.
impl<T: PartialEq> PartialEq for Held<T> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq::<T>(&raw const **self, &raw const **other) || **self == **other
    }
}

impl<T> std::ops::Deref for Held<T> {
    type Target = T;

    fn deref(&self) -> &T {
        match self {
            Held::Library(def) => def,
            Held::Own(def) => def,
        }
    }
}

impl<T: Clone> Held<T> {
    /// The definition as a value of its own, shared: the program's is, and the library's is
    /// copied, which only the few written in where they're used need.
    pub(crate) fn shared(&self) -> std::sync::Arc<T> {
        match self {
            Held::Library(def) => std::sync::Arc::new((*def).clone()),
            Held::Own(def) => std::sync::Arc::clone(def),
        }
    }
}

/// How declarations added to a program hold their definitions.
pub(crate) trait Holds<'m> {
    fn function(&self, def: &'m FnDef) -> Held<FnDef>;
    fn type_def(&self, def: &'m TypeDef) -> Held<TypeDef>;
}

/// A program's own declarations, copied, since its module can change after it's checked.
pub(crate) struct Copies;

impl<'m> Holds<'m> for Copies {
    fn function(&self, def: &'m FnDef) -> Held<FnDef> {
        Held::Own(std::sync::Arc::new(def.clone()))
    }

    fn type_def(&self, def: &'m TypeDef) -> Held<TypeDef> {
        Held::Own(std::sync::Arc::new(def.clone()))
    }
}

/// The standard library's, held where its parse is kept for the whole process.
pub(crate) struct Library;

impl Holds<'static> for Library {
    fn function(&self, def: &'static FnDef) -> Held<FnDef> {
        Held::Library(def)
    }

    fn type_def(&self, def: &'static TypeDef) -> Held<TypeDef> {
        Held::Library(def)
    }
}

#[derive(Clone)]
pub(crate) struct TypeInfo {
    pub(crate) kind: TypeKind,
    pub(crate) def: Held<TypeDef>,
    /// Whether it comes from the standard library rather than the program.
    pub(crate) prelude: bool,
}

/// A constant at the top level: `KEYWORDS = {"def", "if"}`.
#[derive(Clone)]
pub(crate) struct ConstEntry {
    pub(crate) ty: Option<TypeExpr>,
    pub(crate) value: fennec_syntax::Expr,
    pub(crate) span: Span,
    pub(crate) prelude: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FnEntry {
    pub(crate) def: Held<FnDef>,
    /// The type a method belongs to.
    pub(crate) owner: Option<String>,
    pub(crate) prelude: bool,
    /// The type parameters of the functions around a nested function, which it can use.
    pub(crate) inherited: Vec<Generic>,
    /// For a method from an extension of a generic type, the type's parameters as the
    /// extension names them, with any bounds that make it conditional.
    pub(crate) extension: Vec<Generic>,
    /// Whether it's written inside another function, whose key its own starts with. A library
    /// module's function, `json.dumps`, is keyed under the module's name and isn't.
    pub(crate) nested: bool,
}

/// Every declaration in a program, keyed the way the signature table is: `name` for a
/// function, `Type.name` for a method, `outer.inner` for a nested function.
#[derive(Clone, Default)]
pub(crate) struct Program {
    pub(crate) types: BTreeMap<String, TypeInfo>,
    pub(crate) functions: BTreeMap<String, FnEntry>,
    pub(crate) tests: Vec<TestDef>,
    pub(crate) interfaces: BTreeMap<String, Interface>,
    pub(crate) constants: BTreeMap<String, ConstEntry>,
    /// Each file of the program, the program's own and its kits', by the span its text covers.
    /// None for a program parsed from one text, which is one file.
    pub(crate) files: Vec<Span>,
    /// Where each copy of the program's text written in went (`Module::copied`): code in one is in
    /// the file its text is in, and sees what that file imports.
    pub(crate) copied: Vec<fennec_syntax::Copied>,
    /// What each of those files imports from the standard library, in the same order: one file's
    /// imports are its own, never another's.
    pub(crate) file_imports: Vec<Imports>,
    /// What the standard library's own code imports, which only that code sees.
    pub(crate) std_imports: Imports,
    /// What lowering imports for the code it writes in, under names no file can write.
    pub(crate) written_imports: Imports,
    /// Problems found while adding declarations: imports of modules or names that don't
    /// exist, and methods that can't be added.
    pub(crate) problems: Vec<Diagnostic>,
    aliases: BTreeMap<String, TypeExpr>,
    /// Each test's place among `tests`, by the key its body is checked under, so finding one by
    /// its key doesn't look through every test.
    test_keys: BTreeMap<String, usize>,
}

/// The key a test's body is checked under, beside the functions' keys: `test "<name>"`.
pub(crate) fn test_key(name: &str) -> String {
    format!("test \"{name}\"")
}

/// What one file imports from the standard library, by the names it binds.
#[derive(Clone, Default)]
pub(crate) struct Imports {
    /// Builtins that exist only once imported, like `loads` from `std.json`, by the name they're
    /// imported under.
    pub(crate) builtins: BTreeMap<String, String>,
    /// Standard library functions written in Fennec, by the name they're imported under.
    pub(crate) functions: BTreeMap<String, String>,
    /// Standard library constants, by the name they're imported under, as `INFINITY` for
    /// `math.INFINITY`.
    pub(crate) constants: BTreeMap<String, String>,
    /// Standard library types imported by name, like `Deque` from `std.collections`.
    pub(crate) types: BTreeSet<String>,
    /// Imported modules, by the name they're used under, like `json` in `json.loads(...)`.
    pub(crate) modules: BTreeMap<String, &'static str>,
    /// The names an import that failed would have bound, which aren't reported again where
    /// they're used.
    pub(crate) failed: BTreeSet<String>,
}

/// How deeply type aliases may refer to each other, which also stops an alias naming itself.
const ALIAS_DEPTH: usize = 16;

impl Program {
    /// Adds a program's own declarations.
    pub(crate) fn add(&mut self, module: &Module, prelude: bool) {
        self.add_with(module, prelude, &Copies);
    }

    /// Adds a module's declarations, each held as `hold` says.
    pub(crate) fn add_with<'m>(&mut self, module: &'m Module, prelude: bool, hold: &impl Holds<'m>) {
        if !prelude {
            self.files.clone_from(&module.files);
            self.copied.clone_from(&module.copied);
            self.file_imports = vec![Imports::default(); module.files.len().max(1)];
        }
        for item in &module.items {
            match item {
                Item::Function(def) => {
                    let first = self
                        .functions
                        .get(&def.name)
                        .filter(|first| !first.prelude)
                        .map(|first| first.def.span);
                    if self.defined_again(prelude, "function", &def.name, def.span, first) {
                        continue;
                    }
                    let entry = FnEntry {
                        def: hold.function(def),
                        owner: None,
                        prelude,
                        inherited: Vec::new(),
                        extension: Vec::new(),
                        nested: false,
                    };
                    self.functions.insert(def.name.clone(), entry);
                }
                Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                    let first =
                        self.types.get(&def.name).filter(|first| !first.prelude).map(|first| first.def.span);
                    if self.defined_again(prelude, "type", &def.name, def.span, first) {
                        continue;
                    }
                    let kind = match item {
                        Item::Struct(_) => TypeKind::Struct,
                        Item::Enum(_) => TypeKind::Enum,
                        _ => TypeKind::Error,
                    };
                    self.add_type(def, kind, prelude, hold);
                }
                Item::TypeAlias { name, ty, .. } => {
                    self.aliases.insert(name.clone(), ty.clone());
                }
                Item::Test(test) => {
                    // The first test of a name is the one its key finds.
                    self.test_keys.entry(test_key(&test.name)).or_insert(self.tests.len());
                    self.tests.push(test.clone());
                }
                Item::Interface(interface) => {
                    self.interfaces.insert(interface.name.clone(), interface.clone());
                }
                Item::Import(import) => self.add_import(import),
                Item::Const { name, ty, value, span } => {
                    let first =
                        self.constants.get(name).filter(|first| !first.prelude).map(|first| first.span);
                    if self.defined_again(prelude, "constant", name, *span, first) {
                        continue;
                    }
                    let entry = ConstEntry { ty: ty.clone(), value: value.clone(), span: *span, prelude };
                    self.constants.insert(name.clone(), entry);
                }
                Item::Extend(_) | Item::Stmt(_) => {}
            }
        }
        // Extensions come last, so the type they extend is known wherever it's declared.
        for item in &module.items {
            if let Item::Extend(extension) = item {
                self.add_extension(extension, prelude, hold);
            }
        }
    }

    fn add_import(&mut self, import: &Import) {
        // Among other names, as in `from std import json, intrinsics`, the others are imported.
        if import.names.len() > 1 && reaches_intrinsics(import) {
            for (at, name) in import.names.iter().enumerate() {
                let one = Import {
                    names: vec![name.clone()],
                    aliases: vec![import.aliases.get(at).cloned().flatten()],
                    ..import.clone()
                };
                self.add_import(&one);
            }
            return;
        }
        if reaches_intrinsics(import) && !import.span.in_prelude() {
            self.problems.push(
                Diagnostic::new("`std.intrinsics` is for the standard library", import.span)
                    .with_help("use the std types built on it, like `int`, `float` and `Buffer`"),
            );
            return;
        }
        let imported = match fennec_syntax::std_import(import) {
            Ok(imported) => imported,
            // One unknown name among several, as in `from std import compres, json`, is reported
            // alone: the others are imported as if each had its own line, and a name that
            // failed isn't reported again wherever it's used.
            Err(_) if import.names.len() > 1 => {
                for (at, name) in import.names.iter().enumerate() {
                    let one = Import {
                        names: vec![name.clone()],
                        aliases: vec![import.aliases.get(at).cloned().flatten()],
                        ..import.clone()
                    };
                    match fennec_syntax::std_import(&one) {
                        Ok(_) => self.add_import(&one),
                        Err(problem) => {
                            let bound = one
                                .bound()
                                .next()
                                .map_or_else(|| name.clone(), |(_, alias)| alias.to_owned());
                            self.imports_mut(import.span).failed.insert(bound);
                            self.problems.push(problem);
                        }
                    }
                }
                return;
            }
            Err(problem) => {
                if let Some((_, alias)) = import.bound().next() {
                    self.imports_mut(import.span).failed.insert(alias.to_owned());
                }
                self.problems.push(problem);
                return;
            }
        };
        for item in imported {
            match item {
                Imported::Module { alias, module } => {
                    self.imports_mut(import.span).modules.insert(alias, module);
                }
                Imported::Name { name, alias, module } => {
                    let builtin = format!("{module}.{name}");
                    let (function, constant) =
                        (self.functions.contains_key(&builtin), self.constants.contains_key(&builtin));
                    let imports = self.imports_mut(import.span);
                    if crate::builtins::is_builtin(&builtin) {
                        imports.builtins.insert(alias, builtin);
                    } else if function {
                        imports.functions.insert(alias, builtin);
                    } else if constant {
                        imports.constants.insert(alias, builtin);
                    } else {
                        imports.types.insert(name);
                    }
                }
            }
        }
    }

    /// The imports an import at `span` adds to: std's own, those of the file it's written in, or,
    /// for one lowering writes in with no place in a file, the written-in code's.
    fn imports_mut(&mut self, span: Span) -> &mut Imports {
        match crate::import_place(&self.files, span) {
            crate::Place::Std => &mut self.std_imports,
            crate::Place::File(at) => {
                if self.file_imports.len() <= at {
                    self.file_imports.resize(at + 1, Imports::default());
                }
                match self.file_imports.get_mut(at) {
                    Some(imports) => imports,
                    None => &mut self.written_imports,
                }
            }
            crate::Place::Written => &mut self.written_imports,
        }
    }

    /// What code at `span` imports: std's own code its own, a file's code that file's, and what
    /// lowering writes in only what it imports itself. What lowering imports, under names no file
    /// can write, every file's code reaches too.
    pub(crate) fn imports_at(&self, span: Span) -> [Option<&Imports>; 2] {
        match crate::place(&self.files, span.copied_from(&self.copied)) {
            crate::Place::Std => [Some(&self.std_imports), None],
            crate::Place::File(at) => [self.file_imports.get(at), Some(&self.written_imports)],
            crate::Place::Written => [None, Some(&self.written_imports)],
        }
    }

    /// Something code at `span` imported, found by `found` in the imports it sees.
    pub(crate) fn imported_by<'a, T>(
        &'a self,
        span: Span,
        found: impl Fn(&'a Imports) -> Option<T>,
    ) -> Option<T> {
        self.imports_at(span).into_iter().flatten().find_map(found)
    }

    /// Whether a program's own `what` called `name`, at `span`, is a second one: the first,
    /// at `first`, is kept, and this one is refused, since only one of them could be what the
    /// name means.
    fn defined_again(
        &mut self,
        prelude: bool,
        what: &str,
        name: &str,
        span: Span,
        first: Option<Span>,
    ) -> bool {
        let Some(first) = first.filter(|_| !prelude) else { return false };
        self.problems.push(
            Diagnostic::new(format!("there's already a {what} called `{name}`"), span)
                .with_help("rename one of them, or remove one")
                .also_at(first),
        );
        true
    }

    fn add_type<'m>(&mut self, def: &'m TypeDef, kind: TypeKind, prelude: bool, hold: &impl Holds<'m>) {
        // A program's own type replaces a standard library type of the same name, methods too: only
        // that type's, never a std module's functions, as `path.is_device` beside a `struct path`.
        // Its methods' keys all start `Type.`, so they sit together among the keys, found without
        // looking through every function.
        if !prelude {
            let methods = format!("{}.", def.name);
            let replaced: Vec<String> = self
                .functions
                .range::<str, _>((Bound::Included(methods.as_str()), Bound::Unbounded))
                .take_while(|(key, _)| key.starts_with(&methods))
                .filter(|(_, entry)| entry.prelude && entry.owner.as_deref() == Some(def.name.as_str()))
                .map(|(key, _)| key.clone())
                .collect();
            for key in replaced {
                self.functions.remove(&key);
            }
        }
        for method in &def.methods {
            self.add_method(&def.name, method, prelude, &[], hold);
        }
        self.types.insert(def.name.clone(), TypeInfo { kind, def: hold.type_def(def), prelude });
    }

    /// The type a parameter with no written type takes from its default: an `int`, `float`,
    /// `str` or `bool` literal, or an enum case like `Mode.fast`.
    pub(crate) fn default_type(&self, param: &fennec_syntax::Param) -> Option<Type> {
        literal_type(param.default.as_ref()?, self)
    }

    /// The modules imported where `span` is, by the names they're bound under.
    pub(crate) fn modules_at(&self, span: Span) -> BTreeMap<String, &'static str> {
        // A file's own alias stands over one lowering imported, though their names never meet.
        let seen = self.imports_at(span).into_iter().rev().flatten();
        seen.flat_map(|imports| imports.modules.iter().map(|(alias, module)| (alias.clone(), *module)))
            .collect()
    }

    /// The builtin an imported name at `span` stands for, as the file it's written in imports it.
    pub(crate) fn imported(&self, name: &str, span: Span) -> Option<&String> {
        self.imported_by(span, |imports| imports.builtins.get(name))
    }

    /// The test whose body is checked under `key`.
    pub(crate) fn test(&self, key: &str) -> Option<&TestDef> {
        self.tests.get(*self.test_keys.get(key)?)
    }

    pub(crate) fn type_info(&self, name: &str) -> Option<&TypeInfo> {
        self.types.get(name)
    }

    /// `Result[T, E]`, whose `E` is an error, as `fail` takes.
    fn result_written(
        &self,
        written: &TypeExpr,
        value: Box<Type>,
        error: Box<Type>,
    ) -> Result<Type, Diagnostic> {
        if !self.can_fail_with(&error) {
            let span = written.args.get(1).map_or(written.span, |error| error.span);
            return Err(not_an_error("a `Result` fails with an error", &error, span));
        }
        Ok(Type::Result(value, error))
    }

    /// Whether something can fail with a value of type `ty`, as `fail` and `Err(...)` take:
    /// an `error` type, several of them, or a type not known yet.
    pub(crate) fn can_fail_with(&self, ty: &Type) -> bool {
        match ty {
            Type::Errors(_) | Type::Param(_) | Type::Unknown | Type::Var(_) => true,
            Type::Union(members) => members.iter().all(|member| self.can_fail_with(member)),
            other => self.kind_of(other) == Some(TypeKind::Error),
        }
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
        self.types.contains_key(name) || matches!(name, "Id" | "Table" | "SoaList" | "Buffer")
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

    /// `ParseError | RangeError` after `fails`: one of several errors. Each has to be one.
    fn errors_named(&self, union: &TypeExpr, generics: &[String], depth: usize) -> Result<Type, Diagnostic> {
        let members: Vec<Type> = union
            .args
            .iter()
            .map(|member| self.resolve_within(member, generics, depth))
            .collect::<Result<_, _>>()?;
        let is_error = |ty: &Type| {
            matches!(ty, Type::Named(name) if self.types.get(name).is_some_and(|info| info.kind == TypeKind::Error))
                || matches!(ty, Type::Errors(_))
        };
        if !members.iter().any(is_error) {
            let ty = union_of(union, members)?;
            return Ok(if union.optional { Type::optional(ty) } else { ty });
        }
        let mut names = Vec::new();
        for error in &union.args {
            match self.resolve_within(error, generics, depth)? {
                Type::Named(name)
                    if self.types.get(&name).is_some_and(|info| info.kind == TypeKind::Error) =>
                {
                    names.push(name);
                }
                Type::Unknown => {}
                other => {
                    return Err(Diagnostic::new(
                        format!("`|` joins errors, but `{}` is {}", written(error), crate::types::a(&other)),
                        error.span,
                    )
                    .with_help("declare the ways it can fail with `error`, like `error LoadError:`"));
                }
            }
        }
        names.sort();
        names.dedup();
        Ok(if union.optional { Type::optional(Type::Errors(names)) } else { Type::Errors(names) })
    }

    /// The name a written type gives, through an imported module: `Deque` for
    /// `std.collections.Deque` or `collections.Deque`.
    fn written_name<'w>(&self, written: &'w TypeExpr) -> &'w str {
        match written.name.rsplit_once('.') {
            Some((path, member))
                if self.modules_at(written.span).contains_key(path) && self.types.contains_key(member) =>
            {
                member
            }
            _ => written.name.as_str(),
        }
    }

    /// `(str) -> int fails ParseError`'s return type: a result of what it gives back and its errors.
    fn fallible_returns(
        &self,
        written: &TypeExpr,
        generics: &[String],
        depth: usize,
    ) -> Result<Type, Diagnostic> {
        let [returns, errors] = written.args.as_slice() else { return Ok(Type::Unknown) };
        let returns = self.resolve_within(returns, generics, depth)?;
        Ok(Type::result(returns, self.resolve_within(errors, generics, depth)?))
    }

    /// How many types in brackets a type takes, when `given` are written.
    fn wanted_args(&self, name: &str, given: usize) -> usize {
        match name {
            "->" => given.max(1),
            "list" | "set" | "SoaList" | "Buffer" | "Id" | "Table" | "Iterator" => 1,
            "dict" | "Result" | "array" => 2,
            "tuple" => given,
            name => self.type_params(name).len(),
        }
    }

    /// `int??`: an optional of the optional it's written around, kept apart from it.
    fn optional_of_optional(
        &self,
        written: &TypeExpr,
        generics: &[String],
        depth: usize,
    ) -> Result<Type, Diagnostic> {
        let [held] = written.args.as_slice() else { return Err(unknown_type(&written.name, written.span)) };
        Ok(Type::Optional(Box::new(self.resolve_within(held, generics, depth)?)))
    }

    fn resolve_within(
        &self,
        written: &TypeExpr,
        generics: &[String],
        depth: usize,
    ) -> Result<Type, Diagnostic> {
        let name = self.written_name(written);
        if self.is_interface(name) {
            return Err(interface_as_type(name, written.span));
        }
        match name {
            "|" => return self.errors_named(written, generics, depth),
            "fails" => return self.fallible_returns(written, generics, depth),
            fennec_syntax::OPTIONAL => return self.optional_of_optional(written, generics, depth),
            "Result" if crate::gone::written_by_hand(written.span) => {
                return Err(crate::gone::result_type(written.span));
            }
            _ => {}
        }
        // `with Transaction`, what a source lends, and `shared Scores`, which holds any type.
        if matches!(name, crate::lend::WITH | crate::lock::SHARED)
            && let [held] = written.args.as_slice()
        {
            // Once lowered, a `shared T` is the declaration made to hold a `T`.
            let made = crate::lock_lowering::made_name(&self::written(held));
            let ty = if name == crate::lock::SHARED && self.types.contains_key(&made) {
                Type::named(&made)
            } else {
                Type::Generic(name.to_owned(), vec![self.resolve_within(held, generics, depth)?])
            };
            return Ok(if written.optional { Type::optional(ty) } else { ty });
        }
        let builtin = BUILT_IN_TYPES.contains(&name);
        let declared = generics.iter().any(|generic| generic == name)
            || self.types.contains_key(name)
            || self.aliases.contains_key(name);
        if !builtin && !declared {
            return Err(unknown_type(name, written.span));
        }
        let wanted = self.wanted_args(name, written.args.len());
        if written.args.len() != wanted {
            return Err(wrong_type_args(written, wanted));
        }
        let mut args = Vec::new();
        // An array's size is a number, not a type.
        for arg in written.args.iter().take(if name == "array" { 1 } else { usize::MAX }) {
            args.push(self.resolve_within(arg, generics, depth)?);
        }
        let mut items = args.clone().into_iter();
        let mut arg = || Box::new(items.next().unwrap_or(Type::Unknown));
        let ty = match name {
            name if let Some(plain) = Type::plain(name) => plain,
            "None" => Type::None,
            // What a function gives back when it gives back nothing, for writing the return
            // type of one that calls itself.
            "nothing" => Type::Unit,
            "list" | "SoaList" => Type::List(arg()),
            "Buffer" => Type::Buffer(arg()),
            "array" => Type::Array(arg(), array_size(written)?),
            "Iterator" => Type::Iterator(arg(), None),
            "set" => Type::Set(arg()),
            "dict" => Type::Dict(arg(), arg()),
            "Result" => self.result_written(written, arg(), arg())?,
            "tuple" => Type::Tuple(args),
            "->" => {
                let mut params = args;
                let ret = params.pop().unwrap_or(Type::Unit);
                Type::Arrow(params, Box::new(ret))
            }
            "Id" => Type::Id(arg()),
            "Table" => Type::Table(arg()),
            "Stdout" | "Fs" | "Clock" | "Args" | "Stdin" | "Process" | "Entropy" | "Net" | "Env"
            | "Signals" | "Stderr" => Type::named(name),
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
        if let Some(problem) = optional_inside(&ty, written.span) {
            return Err(problem);
        }
        Ok(if written.optional { Type::optional(ty) } else { ty })
    }

    /// Whether a program can write a type's name at `at`: it's built in, declared in the program,
    /// or imported from the standard library by the file `at` is in.
    pub(crate) fn can_name(&self, name: &str, at: Span) -> bool {
        fennec_syntax::importable_type(name).is_none()
            || self.types.get(name).is_some_and(|info| !info.prelude)
            || self.imported_by(at, |imports| imports.types.get(name)).is_some()
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

pub(crate) use fennec_syntax::visit::BUILT_IN_INTERFACES;

pub(crate) const BUILT_IN_TYPES: [&str; 41] = [
    "int",
    "float",
    "i8",
    "i16",
    "i32",
    "i64",
    "u8",
    "u16",
    "u32",
    "u64",
    "f32",
    "f64",
    "bool",
    "str",
    "LiteralStr",
    "bytes",
    "None",
    "nothing",
    "list",
    "array",
    "SoaList",
    "Buffer",
    "set",
    "dict",
    "tuple",
    "Id",
    "Table",
    "Stdout",
    "Fs",
    "Clock",
    "Args",
    "Stdin",
    "Process",
    "Entropy",
    "Net",
    "Env",
    "Signals",
    "Stderr",
    "Result",
    "Iterator",
    "->",
];

/// An interface written where a type goes, as in `list[Shape]`.
fn interface_as_type(name: &str, span: Span) -> Diagnostic {
    Diagnostic::new(format!("`{name}` is an interface, not a type"), span).with_help(format!(
        "take a type parameter that fits it, like `def f[T: {name}](items: list[T])`, or use an enum for a mix of types"
    ))
}

/// The `16` in `array[float, 16]`: a whole number of at least 1, written out.
fn array_size(written: &TypeExpr) -> Result<u64, Diagnostic> {
    let size = written.args.get(1).and_then(|size| size.name.parse::<u64>().ok());
    size.filter(|size| *size > 0).ok_or_else(|| {
        Diagnostic::new("an array's size is a whole number of at least 1, written out", written.span)
            .with_help("write it like `array[float, 16]`")
    })
}

fn wrong_type_args(written: &TypeExpr, wanted: usize) -> Diagnostic {
    let name = &written.name;
    let example = match name.as_str() {
        "dict" => "dict[str, int]".to_owned(),
        "Result" => "Result[int, IoError]".to_owned(),
        "list" | "set" | "SoaList" | "Buffer" => format!("{name}[int]"),
        "Id" | "Table" => format!("{name}[Enemy]"),
        _ => name.clone(),
    };
    let message = if wanted == 0 {
        format!("`{name}` doesn't take types in brackets")
    } else {
        format!("`{name}` needs {wanted} type{} in brackets", if wanted == 1 { "" } else { "s" })
    };
    Diagnostic::new(message, written.span).with_help(format!("write it like `{example}`"))
}

/// Something other than an error where only an error goes: `Result[int, str]`, `Err("no")`.
pub(crate) fn not_an_error(what: &str, ty: &Type, span: Span) -> Diagnostic {
    Diagnostic::new(format!("{what}, but this is {}", crate::types::a(ty)), span)
        .with_help("declare one with `error`, like `error Failed:` with a `why: str` field, as `fail` takes")
}

pub(crate) fn unknown_type(name: &str, span: Span) -> Diagnostic {
    let diagnostic = Diagnostic::new(format!("`{name}` isn't a type"), span);
    match name {
        "double" | "f16" => diagnostic.with_help("write `float`, or `f32` for 32 bits"),
        "usize" | "isize" | "long" => diagnostic.with_help("write `int`"),
        "string" | "String" => diagnostic.with_help("write `str`"),
        "boolean" | "Bool" => diagnostic.with_help("write `bool`"),
        "List" | "Dict" | "Set" => diagnostic.with_help(format!("write `{}`", name.to_lowercase())),
        "Optional" => diagnostic.with_help("write `?` after the type, like `Enemy?`"),
        // The type's name comes first in what's written, before any brackets.
        "IdMap" => diagnostic
            .with_help(crate::messages::TABLE)
            .with_fix(Span { start: span.start, end: span.start + 5 }, "Table"),
        "Self" => diagnostic.with_help(
            "`Self` means the type a method belongs to, so it goes in a method of a type or an interface",
        ),
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
    fennec_syntax::written_type(ty)
}

/// The type a literal default gives its parameter, if it's one that gives a type.
pub(crate) fn literal_type(default: &fennec_syntax::Expr, program: &Program) -> Option<Type> {
    use fennec_syntax::{ExprKind, UnaryOp};
    match &default.kind {
        ExprKind::Int(_) => Some(Type::Int),
        ExprKind::Float(_) => Some(Type::Float),
        ExprKind::Duration(_) => Some(Type::named("Duration")),
        ExprKind::Str(_) => Some(Type::Str),
        ExprKind::Bool(_) => Some(Type::Bool),
        ExprKind::Unary { op: UnaryOp::Neg, expr }
            if matches!(expr.kind, ExprKind::Int(_) | ExprKind::Float(_)) =>
        {
            literal_type(expr, program)
        }
        // `Packed[int]()`: a type made with its type arguments is that type.
        ExprKind::Call { callee, .. } if matches!(callee.kind, ExprKind::Index { .. }) => {
            program.resolve_in(&callee.as_type()?, &[]).ok()
        }
        ExprKind::Attr { object, name, optional: false } => {
            let ExprKind::Name(owner) = &object.kind else { return None };
            let info = program.type_info(owner)?;
            let is_case = info.def.cases.iter().any(|case| case.name == *name);
            is_case.then(|| Type::Named(owner.clone()))
        }
        _ => None,
    }
}

/// What's wrong with how a field's or a parameter's written type and default go together:
/// writing the type its default already gives, or an empty `[]` or `{}` where the default has to
/// say its type, like `list[str]()`. `name_end` is where the name ends, for the fix.
pub(crate) fn default_problem(
    program: &Program,
    (name, name_end): (&str, u32),
    (ty, resolved): (&TypeExpr, &Type),
    default: &fennec_syntax::Expr,
) -> Option<Diagnostic> {
    use fennec_syntax::ExprKind;
    // A type written from its default when it was checked, for what reads types from fields,
    // sits inside the default.
    if ty.span.start >= default.span.start {
        return None;
    }
    let between = Span { start: name_end, end: default.span.start };
    let empty = match &default.kind {
        ExprKind::List(items) => items.is_empty(),
        ExprKind::Dict(pairs) => pairs.is_empty(),
        _ => false,
    };
    // A `list[shared T]()` can't be written, since `shared T` is a type and not a value, so
    // `= []` is how one starts empty.
    if empty && !holds_shared(ty) {
        let made = format!("{}()", written(ty));
        let message = format!("`{name}`'s default is written as `{made}`");
        let help = format!("write `{name} = {made}`, which says its type");
        let whole = Span { start: name_end, end: default.span.end };
        return Some(
            Diagnostic::new(message, default.span).with_help(help).with_fix(whole, format!(" = {made}")),
        );
    }
    let given = literal_type(default, program)?;
    if given != *resolved {
        return None;
    }
    let message = format!("`{name}` takes its type from its default, so `: {}` isn't needed", written(ty));
    Some(
        Diagnostic::new(message, ty.span)
            .with_help(format!("remove the type, so it reads `{name} = ` and its default"))
            .with_fix(between, " = "),
    )
}

/// What's wrong with a dict or set that holds optionals: not being in one already means missing,
/// so a `None` in it would be a second way to say so.
pub(crate) fn optional_inside(ty: &Type, span: Span) -> Option<Diagnostic> {
    let (message, help) = match ty {
        Type::Dict(_, value) if matches!(**value, Type::Optional(_) | Type::None) => (
            "a dict can't hold optional values",
            "leave the key out instead, since not being in the dict already means missing",
        ),
        Type::Dict(key, _) if matches!(**key, Type::Optional(_) | Type::None) => {
            ("a dict's keys can't be optional", "keep a key that's missing out of the dict")
        }
        Type::Set(item) if matches!(**item, Type::Optional(_) | Type::None) => (
            "a set can't hold optional items",
            "leave a missing item out, since not being in the set already says so",
        ),
        _ => return None,
    };
    Some(Diagnostic::new(message, span).with_help(help))
}

/// `Circle | Square`: the union of `members`, flattened and in a fixed order, so `A | B` is
/// `B | A`. A member that's there twice, `None`, or a type parameter is a mistake.
fn union_of(written: &TypeExpr, members: Vec<Type>) -> Result<Type, Diagnostic> {
    let mut flat = Vec::new();
    for (member, at) in members.into_iter().zip(&written.args) {
        match member {
            Type::Union(inner) => flat.extend(inner.into_iter().map(|inner| (inner, at.span))),
            Type::None | Type::Optional(_) => {
                let others: Vec<String> =
                    written.args.iter().filter(|arg| arg.name != "None").map(written_type).collect();
                let without = match others.as_slice() {
                    [one] => format!("{one}?"),
                    _ => format!("({})?", others.join(" | ")),
                };
                return Err(Diagnostic::new("`None` is never part of a union", at.span)
                    .with_help(format!("write `{without}`: an optional says it can be missing"))
                    .with_fix(written.span, without));
            }
            Type::Param(name) => {
                return Err(Diagnostic::new(
                    format!("`{name}` is a type parameter, which can't be part of a union"),
                    at.span,
                )
                .with_help(
                    "a union mixes types that are known; generic code uses an enum, like `enum Either[L, R]`",
                ));
            }
            member => flat.push((member, at.span)),
        }
    }
    flat.sort_by_key(|(member, _)| member.to_string());
    let twice = flat.windows(2).find_map(|pair| match pair {
        [(one, _), (other, at)] if one == other => Some((other, at)),
        _ => None,
    });
    if let Some((member, at)) = twice {
        return Err(
            Diagnostic::new(format!("`{member}` is in this union twice"), *at).with_help("write it once")
        );
    }
    Ok(Type::Union(flat.into_iter().map(|(member, _)| member).collect()))
}

fn written_type(ty: &TypeExpr) -> String {
    written(ty)
}

/// Whether `ty` has a `shared` type anywhere in it.
fn holds_shared(ty: &TypeExpr) -> bool {
    ty.name == crate::lock::SHARED || ty.args.iter().any(holds_shared)
}

/// Whether an import reaches `std.intrinsics`, however it's written: `import std.intrinsics`,
/// `from std.intrinsics import x`, `from std import intrinsics` with or without `as`, or among
/// other names. What's asked is the module the import brings in, not how it's spelled.
fn reaches_intrinsics(import: &Import) -> bool {
    let named = |module: &str| module == "intrinsics";
    let from_its_path = import.module.first().is_some_and(|first| first == "std")
        && import.module.get(1).is_some_and(|second| named(second));
    from_its_path
        || fennec_syntax::std_import(import).is_ok_and(|all| {
            all.iter().any(|imported| match imported {
                Imported::Module { module, .. } | Imported::Name { module, .. } => named(module),
            })
        })
}
