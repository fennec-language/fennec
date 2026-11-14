//! A program of several files: a kit's `src/main.fen` and every module it imports, or a single
//! file and the files beside it that it imports. The compiler's kit loader is in a crate that also
//! fetches and builds other kits, so the oracle finds and merges the files on its own, by the same
//! rules: `from game import physics` is the file `game/physics.fen` when there is one, and
//! otherwise the name `physics` from `game.fen`.
//!
//! Every file of a kit shares one set of top-level names, so the merged module reads like one
//! file: a module's name in front of one of its names is dropped, a name imported with `as` is
//! written as the name it stands for, and a leading-dot case that only one enum the file can see
//! has is given that enum's name. Each file is parsed at its own offset, so a span says which file
//! it's in.
//!
//! The compiler refuses a file that uses another's name without importing it, and the oracle
//! doesn't look for that: it's held only to programs the compiler builds. It refuses a file whose
//! own names hide a name an import binds, rather than work out which one each use means.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use fennec_syntax::{Expr, ExprKind, FnDef, Import, Item, Module, Pattern, Sources, TypeExpr, visit};

use crate::load::Refused;

/// The files of a program merged into one module, and the files, for messages.
pub(crate) struct Merged {
    pub(crate) module: Module,
    pub(crate) sources: Sources,
}

/// Loads the kit in `dir`, shown as `shown`: its `src/main.fen` and the modules it imports. A kit
/// that depends on other kits is unsupported.
pub(crate) fn kit(dir: &Path, shown: &str) -> Result<Merged, Refused> {
    let manifest = std::fs::read_to_string(dir.join("kit.toml"))
        .map_err(|_| Refused::Invalid(format!("{} has no kit.toml", dir.display())))?;
    if depends_on_kits(&manifest) {
        return Err(Refused::Unsupported("a kit with dependencies".to_owned()));
    }
    let shown_src = format!("{}/src/", shown.trim_end_matches('/'));
    let mut loader = Loader::new(dir.join("src"), shown_src);
    let (real, shown_main) = loader.path(&["main".to_owned()]);
    loader.visit("main".to_owned(), &real, shown_main)?;
    loader.merge()
}

/// Loads a single file, shown as `shown`, and the modules beside it that it imports.
pub(crate) fn file(path: &Path, shown: &str) -> Result<Merged, Refused> {
    let src = path.parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let shown_src = match Path::new(shown).parent().map(|folder| folder.to_string_lossy().replace('\\', "/"))
    {
        Some(folder) if !folder.is_empty() && folder != "." => format!("{}/", folder.trim_end_matches('/')),
        _ => String::new(),
    };
    let name = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    let mut loader = Loader::new(src, shown_src);
    loader.visit(name, path, shown.to_owned())?;
    loader.merge()
}

/// Whether a `kit.toml` names other kits: a line in its `[dependencies]` table, a table of its
/// own under it, or `gets`.
fn depends_on_kits(manifest: &str) -> bool {
    let mut table = String::new();
    for line in manifest.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(header) = line.strip_prefix('[') {
            header.trim_matches(|c| c == '[' || c == ']').trim().clone_into(&mut table);
            if table.starts_with("dependencies.") || table.starts_with("gets") {
                return true;
            }
            continue;
        }
        let key = line.split('=').next().unwrap_or_default().trim();
        if table == "dependencies" || (table.is_empty() && key == "gets") {
            return true;
        }
    }
    false
}

/// Whether an import is of the standard library rather than of the program's own files.
fn is_std(import: &Import) -> bool {
    import.module.first().is_some_and(|first| first == "std")
}

/// A parsed file of a program.
struct File {
    /// Its module name, like `world.physics`.
    name: String,
    module: Module,
}

/// Finds the files of a program, following imports from file to file.
struct Loader {
    /// The folder module paths resolve against.
    src: PathBuf,
    /// How `src` is shown before a module's path.
    shown_src: String,
    sources: Sources,
    files: Vec<File>,
    seen: HashSet<String>,
    /// The modules being loaded, outermost first, to find imports that go in a circle.
    stack: Vec<String>,
}

impl Loader {
    fn new(src: PathBuf, shown_src: String) -> Loader {
        Loader {
            src,
            shown_src,
            sources: Sources::new(),
            files: Vec::new(),
            seen: HashSet::new(),
            stack: Vec::new(),
        }
    }

    /// Where a module's file is, and how it's shown.
    fn path(&self, module: &[String]) -> (PathBuf, String) {
        let mut real = self.src.clone();
        real.extend(module);
        real.set_extension("fen");
        (real, format!("{}{}.fen", self.shown_src, module.join("/")))
    }

    /// Loads a module and what it imports, unless it's loaded already.
    fn visit(&mut self, name: String, real: &Path, shown: String) -> Result<(), Refused> {
        if self.stack.contains(&name) {
            return Err(Refused::Invalid(format!("the imports of `{name}` go in a circle")));
        }
        if !self.seen.insert(name.clone()) {
            return Ok(());
        }
        let text = std::fs::read_to_string(real)
            .map_err(|err| Refused::Invalid(format!("couldn't read {shown}: {err}")))?;
        let base = self.sources.add(shown, text.clone());
        let mut module = fennec_syntax::parse_at(&text, base)
            .map_err(|problem| Refused::Invalid(self.sources.render(&problem)))?;
        if let Some(embed) = embeds(&mut module) {
            return Err(Refused::Unsupported(format!("`{embed}`")));
        }
        let mut wanted = Vec::new();
        for item in &module.items {
            if let Item::Import(import) = item
                && !is_std(import)
            {
                wanted.extend(self.imported_modules(import));
            }
        }
        self.files.push(File { name: name.clone(), module });
        self.stack.push(name);
        for module in wanted {
            let (real, shown) = self.path(&module);
            self.visit(module.join("."), &real, shown)?;
        }
        self.stack.pop();
        Ok(())
    }

    /// The modules an import loads. `from game import physics` loads `game/physics.fen` when
    /// there is one, as in Python, and otherwise takes `physics` from `game.fen`.
    fn imported_modules(&self, import: &Import) -> Vec<Vec<String>> {
        let mut modules = Vec::new();
        let mut from_parent = import.names.is_empty();
        for name in &import.names {
            let submodule: Vec<String> = import.module.iter().chain([name]).cloned().collect();
            if self.path(&submodule).0.is_file() {
                modules.push(submodule);
            } else {
                from_parent = true;
            }
        }
        if from_parent {
            modules.push(import.module.clone());
        }
        modules
    }

    /// Writes each file's uses of other files' names as the merged module has them, and merges
    /// the files, leaving out the imports of the program's own modules.
    fn merge(mut self) -> Result<Merged, Refused> {
        let index: HashMap<String, usize> =
            self.files.iter().enumerate().map(|(at, file)| (file.name.clone(), at)).collect();
        let defines: Vec<HashSet<String>> = self.files.iter().map(|file| defined(&file.module)).collect();
        let mut cases: HashMap<String, Vec<(String, usize)>> = HashMap::new();
        for (at, file) in self.files.iter().enumerate() {
            for item in &file.module.items {
                if let Item::Enum(def) = item {
                    for case in &def.cases {
                        cases.entry(case.name.clone()).or_default().push((def.name.clone(), at));
                    }
                }
            }
        }
        let kit = Kit { index, defines, cases };
        for (at, file) in self.files.iter_mut().enumerate() {
            let scope = Scope::of(&kit, at, &file.module)?;
            scope.rewrite(&mut file.module)?;
        }
        let items = self
            .files
            .into_iter()
            .flat_map(|file| file.module.items)
            .filter(|item| !matches!(item, Item::Import(import) if !is_std(import)))
            .collect();
        Ok(Merged { module: Module { items, ..Default::default() }, sources: self.sources })
    }
}

/// The first of `embed_file` and `embed_folder` a file calls, which the compiler reads the files of
/// as it loads the program, and the oracle doesn't.
fn embeds(module: &mut Module) -> Option<String> {
    let mut found = None;
    visit::exprs_mut(module, &mut |expr| {
        if let ExprKind::Call { callee, .. } = &expr.kind
            && let ExprKind::Name(name) = &callee.kind
            && matches!(name.as_str(), "embed_file" | "embed_folder")
        {
            found.get_or_insert_with(|| name.clone());
        }
    });
    found
}

/// The names a file defines at the top level.
fn defined(module: &Module) -> HashSet<String> {
    let names = module.items.iter().filter_map(|item| match item {
        Item::Function(def) => Some(&def.name),
        Item::Struct(def) | Item::Enum(def) | Item::Error(def) => Some(&def.name),
        Item::Interface(interface) => Some(&interface.name),
        Item::TypeAlias { name, .. } | Item::Const { name, .. } => Some(name),
        Item::Extend(_) | Item::Import(_) | Item::Test(_) | Item::Stmt(_) => None,
    });
    names.cloned().collect()
}

/// What every file of a program defines.
struct Kit {
    /// Each file, by its module name.
    index: HashMap<String, usize>,
    /// The names each file defines.
    defines: Vec<HashSet<String>>,
    /// The enums with each case name, and the file each is in.
    cases: HashMap<String, Vec<(String, usize)>>,
}

impl Kit {
    fn defines(&self, file: usize, name: &str) -> bool {
        self.defines.get(file).is_some_and(|names| names.contains(name))
    }
}

/// What one file's imports bind.
struct Scope<'k> {
    kit: &'k Kit,
    file: usize,
    /// The modules imported whole, by the name they're used under, like `physics` or
    /// `world.physics`, with the file each is.
    modules: HashMap<String, usize>,
    /// What each name imported with `as` stands for: `spawn` for `make` after
    /// `from enemies import spawn as make`.
    renamed: HashMap<String, String>,
    /// The names imported by name, with the file each is from.
    imported: HashMap<String, usize>,
}

impl<'k> Scope<'k> {
    fn of(kit: &'k Kit, file: usize, module: &Module) -> Result<Scope<'k>, Refused> {
        let mut scope =
            Scope { kit, file, modules: HashMap::new(), renamed: HashMap::new(), imported: HashMap::new() };
        for item in &module.items {
            let Item::Import(import) = item else { continue };
            if is_std(import) {
                continue;
            }
            let module = import.module.join(".");
            // `from game import physics` binds the module `game.physics`, when there is one.
            let mut names = Vec::new();
            for (name, bound) in import.bound() {
                match kit.index.get(&format!("{module}.{name}")) {
                    Some(&from) => {
                        scope.modules.insert(bound.to_owned(), from);
                    }
                    None => names.push((name, bound)),
                }
            }
            if import.names.is_empty() || !names.is_empty() {
                let Some(&from) = kit.index.get(&module) else {
                    return Err(Refused::Invalid(format!("there's no module called `{module}`")));
                };
                scope.imported_from(import, &module, &names, from)?;
            }
        }
        Ok(scope)
    }

    /// Notes what an import of the module `module`, the file `from`, binds.
    fn imported_from(
        &mut self,
        import: &Import,
        module: &str,
        names: &[(&str, &str)],
        from: usize,
    ) -> Result<(), Refused> {
        if import.names.is_empty() {
            self.modules.insert(import.alias.clone().unwrap_or_else(|| module.to_owned()), from);
        }
        for (name, bound) in names {
            if !self.kit.defines(from, name) {
                return Err(Refused::Invalid(format!("`{module}` has nothing called `{name}`")));
            }
            self.imported.insert((*bound).to_owned(), from);
            if bound != name {
                self.renamed.insert((*bound).to_owned(), (*name).to_owned());
            }
        }
        Ok(())
    }

    /// Whether a name the file binds itself is one an import binds too, or the first part of one.
    fn hides_an_import(&self, bound: &HashSet<String>) -> bool {
        let modules = self.modules.keys().map(|path| path.split('.').next().unwrap_or(path));
        modules.chain(self.renamed.keys().map(String::as_str)).any(|name| bound.contains(name))
    }

    /// Writes the file's uses of other files' names as the merged module has them.
    fn rewrite(&self, module: &mut Module) -> Result<(), Refused> {
        let mut hidden = false;
        visit::exprs_in_scope_mut(module, &mut |_, bound: &HashSet<String>| {
            hidden = hidden || self.hides_an_import(bound);
        });
        if hidden {
            return Err(Refused::Unsupported(
                "a local name that hides an import of the program's own".to_owned(),
            ));
        }
        let mut missing = Vec::new();
        visit::exprs_mut(module, &mut |expr| self.expr(expr, &mut missing));
        if let Some(problem) = missing.into_iter().next() {
            return Err(Refused::Invalid(problem));
        }
        for item in &mut module.items {
            if let Item::Extend(extension) = item {
                self.qualified(&mut extension.name);
            }
            types_in(item, &mut |ty| self.ty(ty));
        }
        Ok(())
    }

    /// One expression: `module.name` becomes `name`, a name imported with `as` becomes the name it
    /// stands for, and a leading-dot case only one visible enum has names that enum.
    fn expr(&self, expr: &mut Expr, missing: &mut Vec<String>) {
        let member = match &expr.kind {
            ExprKind::Attr { object, name, optional: false } => fennec_syntax::dotted(object)
                .and_then(|path| Some((path.clone(), *self.modules.get(&path)?, name.clone()))),
            _ => None,
        };
        if let Some((path, from, name)) = member {
            if !self.kit.defines(from, &name) {
                missing.push(format!("`{path}` has nothing called `{name}`"));
            }
            expr.kind = ExprKind::Name(name);
            return;
        }
        match &mut expr.kind {
            ExprKind::Name(name) => {
                if let Some(original) = self.renamed.get(name.as_str()) {
                    original.clone_into(name);
                }
            }
            ExprKind::Variant(case) => {
                if let Some(owner) = self.only_visible(case) {
                    visit::name_enum(expr, &owner);
                }
            }
            ExprKind::Match { arms, .. } => {
                for arm in arms {
                    self.pattern(&mut arm.pattern);
                }
            }
            _ => {}
        }
    }

    /// The one enum this file can see with a case called `case`, when enums in other files have a
    /// case by that name too: one it declares, imports by name, or is in a module it imports.
    fn only_visible(&self, case: &str) -> Option<String> {
        let owners = self.kit.cases.get(case)?;
        if owners.len() < 2 {
            return None;
        }
        let mut visible = owners.iter().filter(|(owner, file)| {
            *file == self.file
                || self.imported.get(owner) == Some(file)
                || self.modules.values().any(|module| module == file)
        });
        match (visible.next(), visible.next()) {
            (Some((owner, _)), None) => Some(owner.clone()),
            _ => None,
        }
    }

    fn pattern(&self, pattern: &mut Pattern) {
        match pattern {
            Pattern::Variant { name, dotted, args, .. } => {
                if !*dotted {
                    self.qualified(name);
                }
                for arg in args {
                    self.pattern(arg);
                }
            }
            Pattern::Tuple(items) | Pattern::Or(items) => {
                for item in items {
                    self.pattern(item);
                }
            }
            Pattern::Wildcard | Pattern::Bind(_) | Pattern::RefBind(_) | Pattern::Literal(_) => {}
        }
    }

    fn ty(&self, ty: &mut TypeExpr) {
        self.qualified(&mut ty.name);
        for arg in &mut ty.args {
            self.ty(arg);
        }
    }

    /// A type or case name: `physics.Body` becomes `Body` for a module imported whole, and a name
    /// imported with `as` becomes the name it stands for.
    fn qualified(&self, name: &mut String) {
        let replaced = match name.rsplit_once('.') {
            Some((module, member)) => self.modules.contains_key(module).then(|| member.to_owned()),
            None => self.renamed.get(name.as_str()).cloned(),
        };
        if let Some(replaced) = replaced {
            *name = replaced;
        }
    }
}

/// Calls `change` on every type an item writes, at its top: each parameter's, return's and
/// failure's, each field's, each bound's, each local's, and those of the functions inside.
fn types_in(item: &mut Item, change: &mut dyn FnMut(&mut TypeExpr)) {
    match item {
        Item::Function(def) => types_in_function(def, change),
        Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
            def.generics.iter_mut().flat_map(|generic| generic.bounds.iter_mut()).for_each(&mut *change);
            let fields =
                def.fields.iter_mut().chain(def.cases.iter_mut().flat_map(|case| case.fields.iter_mut()));
            fields.filter_map(|field| field.ty.as_mut()).for_each(&mut *change);
            for method in &mut def.methods {
                types_in_function(method, change);
            }
        }
        Item::Interface(interface) => {
            for method in &mut interface.methods {
                types_in_function(method, change);
            }
        }
        Item::Extend(extension) => {
            extension
                .generics
                .iter_mut()
                .flat_map(|generic| generic.bounds.iter_mut())
                .for_each(&mut *change);
            for method in &mut extension.methods {
                types_in_function(method, change);
            }
        }
        Item::TypeAlias { ty, .. } => change(ty),
        Item::Const { ty, .. } => ty.iter_mut().for_each(&mut *change),
        Item::Test(test) => {
            let params = test.capabilities.iter_mut().chain(test.inputs.iter_mut());
            params.filter_map(|param| param.ty.as_mut()).for_each(&mut *change);
            visit::written_types_in(&mut test.body, change);
        }
        Item::Import(_) | Item::Stmt(_) => {}
    }
}

fn types_in_function(def: &mut FnDef, change: &mut dyn FnMut(&mut TypeExpr)) {
    def.generics.iter_mut().flat_map(|generic| generic.bounds.iter_mut()).for_each(&mut *change);
    def.params.iter_mut().filter_map(|param| param.ty.as_mut()).for_each(&mut *change);
    def.ret.iter_mut().chain(def.fails.iter_mut()).for_each(&mut *change);
    visit::written_types_in(&mut def.body, change);
}
