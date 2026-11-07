//! Where what a name refers to is declared, and what to show about it: the signature as it's
//! written and its `##` doc comment.

use fennec_check::{Checked, Type};
use fennec_syntax::{FnDef, Item, Module, Sources, Span, TypeDef};

use crate::definition::{Target, name_in};

/// A declaration a name can refer to.
pub(crate) struct Declaration {
    /// Where its name is written, which go-to-definition goes to.
    pub(crate) name_at: Span,
    /// What it starts with as written: a function's signature, or a type's, field's or
    /// constant's first line.
    pub(crate) header: Span,
    /// What a local name holds, which only the typed tree says.
    pub(crate) holds: Option<Type>,
    /// Whether it's a local name, which shows only what it holds.
    pub(crate) local: bool,
    /// Whether it's the standard library's, whose spans are `library::library()`'s rather than
    /// the program's.
    pub(crate) library: bool,
}

/// Every declaration `target` may refer to. One, unless it's a member of an object whose
/// type isn't known. What the program doesn't declare may be the standard library's.
pub(crate) fn declarations(
    target: &Target,
    module: &Module,
    sources: &Sources,
    checked: Option<&Checked>,
) -> Vec<Declaration> {
    let own = Found { module, sources, library: None }.find(target, checked);
    if !own.is_empty() || matches!(target, Target::Local { .. } | Target::Module(_)) {
        return own;
    }
    // `json.loads`, with `json` a module of the library, is that module's function.
    let module_named = match target {
        Target::Member { object, .. } => covered(sources, *object),
        _ => None,
    };
    let library = crate::library::library();
    library
        .modules
        .iter()
        .flat_map(|(name, module)| {
            let found = Found { module, sources: &library.sources, library: Some(name) };
            match (target, module_named) {
                (Target::Member { name: member, .. }, Some(named)) if named == *name => {
                    found.functions(member).collect()
                }
                _ => found.find(target, checked),
            }
        })
        .collect()
}

/// The text `span` covers.
fn covered(sources: &Sources, span: Span) -> Option<&str> {
    let (_, text, base) = sources.file_at(span)?;
    let local = |at: u32| usize::try_from(at.saturating_sub(base)).unwrap_or(0);
    text.get(local(span.start)..local(span.end))
}

impl Found<'_> {
    /// What `target` refers to among what this declares.
    fn find(&self, target: &Target, checked: Option<&Checked>) -> Vec<Declaration> {
        let found = self;
        match target {
            Target::Local { name, name_at, declared, function } => {
                let holds =
                    checked.and_then(|checked| checked.types.body(function)?.name(*declared, name).cloned());
                vec![Declaration { name_at: *name_at, header: *name_at, holds, local: true, library: false }]
            }
            Target::Global(name) => found.global(name),
            Target::Case(name) => {
                found.types().flat_map(|def| found.members(def, name, MemberKind::Cases)).collect()
            }
            Target::TypeMember { owner, name } => found.of_type(owner, name, MemberKind::All),
            Target::Module(module) => found.module_file(module).into_iter().collect(),
            Target::Param { function, name } => found.param(function, name).into_iter().collect(),
            Target::MethodParam { object, method, name, function } => {
                let owner = checked
                    .and_then(|checked| checked.types.body(function)?.expr(*object))
                    .and_then(type_name);
                owner.and_then(|owner| found.param(&format!("{owner}.{method}"), name)).into_iter().collect()
            }
            Target::CaseField { case, name } => found
                .types()
                .flat_map(|def| &def.cases)
                .filter(|declared| declared.name == *case)
                .flat_map(|declared| &declared.fields)
                .filter(|field| field.name == *name)
                .map(|field| found.declared(field.span, name))
                .collect(),
            Target::Member { object, name, function } => {
                let owner = checked
                    .and_then(|checked| checked.types.body(function)?.expr(*object))
                    .and_then(type_name);
                if let Some(owner) = owner {
                    return found.of_type(&owner, name, MemberKind::Values);
                }
                let mut all: Vec<Declaration> =
                    found.types().flat_map(|def| found.members(def, name, MemberKind::Values)).collect();
                all.extend(found.extensions().filter_map(|(_, methods)| found.method(methods, name)));
                all
            }
        }
    }
}

/// The struct, enum or error a type is, through `?`.
fn type_name(ty: &Type) -> Option<String> {
    match ty {
        Type::Named(name) | Type::Generic(name, _) => Some(name.clone()),
        Type::Optional(inner) => type_name(inner),
        // The built-in types' methods written in Fennec are in an `extend` of each.
        Type::List(_) => Some("list".to_owned()),
        Type::Dict(..) => Some("dict".to_owned()),
        Type::Set(_) => Some("set".to_owned()),
        Type::Str => Some("str".to_owned()),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MemberKind {
    /// What a value has: fields and methods.
    Values,
    /// An enum's cases.
    Cases,
    /// Anything a type has, as `Type.name` reaches it.
    All,
}

struct Found<'a> {
    module: &'a Module,
    sources: &'a Sources,
    /// The library module this is, by name, or `None` for the program.
    library: Option<&'a str>,
}

impl<'a> Found<'a> {
    fn types(&self) -> impl Iterator<Item = &'a TypeDef> {
        self.module.items.iter().filter_map(|item| match item {
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => Some(def),
            _ => None,
        })
    }

    /// Every `extend`, as the type it extends and its methods.
    fn extensions(&self) -> impl Iterator<Item = (&'a str, &'a [FnDef])> {
        self.module.items.iter().filter_map(|item| match item {
            Item::Extend(extension) => Some((extension.name.as_str(), extension.methods.as_slice())),
            _ => None,
        })
    }

    fn declared(&self, span: Span, name: &str) -> Declaration {
        let library = self.library.is_some();
        Declaration {
            name_at: name_in(self.sources, span, name),
            header: span,
            holds: None,
            local: false,
            library,
        }
    }

    /// The top-level functions named `name`.
    fn functions(&self, name: &str) -> impl Iterator<Item = Declaration> {
        self.module.items.iter().filter_map(move |item| match item {
            Item::Function(def) if def.name == name => Some(self.function(def)),
            _ => None,
        })
    }

    fn function(&self, def: &FnDef) -> Declaration {
        let end = def.ret.as_ref().map_or(def.span.end, |ret| ret.span.end.max(def.span.end));
        Declaration { header: Span { start: def.span.start, end }, ..self.declared(def.span, &def.name) }
    }

    fn method(&self, methods: &[FnDef], name: &str) -> Option<Declaration> {
        methods.iter().find(|method| method.name == name).map(|method| self.function(method))
    }

    /// A parameter of the function with key `function`: a top-level function's name, or a
    /// type's method as `Type.method`.
    fn param(&self, function: &str, name: &str) -> Option<Declaration> {
        let def = match function.split_once('.') {
            Some((owner, method)) => {
                let own = self.types().filter(|def| def.name == owner).flat_map(|def| &def.methods);
                let extended = self
                    .extensions()
                    .filter(|(extended, _)| *extended == owner)
                    .flat_map(|(_, methods)| methods);
                own.chain(extended).find(|declared| declared.name == method)
            }
            None => self.module.items.iter().find_map(|item| match item {
                Item::Function(def) if def.name == function => Some(def),
                _ => None,
            }),
        }?;
        let param = def.params.iter().find(|param| param.name == name)?;
        Some(self.declared(param.span, name))
    }

    /// A kit's module, by its name, like `combat.damage`: the start of its file, with its first
    /// line, which is its `##` comment when it has one.
    fn module_file(&self, module: &str) -> Option<Declaration> {
        let file = format!("{}.fen", module.replace('.', "/"));
        let (path, _) = self.sources.files().find(|(path, _)| {
            path.strip_suffix(file.as_str()).is_some_and(|before| before.is_empty() || before.ends_with('/'))
        })?;
        let (text, base) = self.sources.by_path(path)?;
        let first = u32::try_from(text.lines().next().unwrap_or_default().len()).unwrap_or(0);
        let start = Span { start: base, end: base };
        Some(Declaration {
            name_at: start,
            header: Span { start: base, end: base + first },
            holds: None,
            local: false,
            library: false,
        })
    }

    fn global(&self, name: &str) -> Vec<Declaration> {
        let found = self.module.items.iter().find_map(|item| match item {
            Item::Function(def) if def.name == name => Some(self.function(def)),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) if def.name == name => {
                Some(self.declared(def.span, name))
            }
            Item::Interface(interface) if interface.name == name => Some(self.declared(interface.span, name)),
            Item::TypeAlias { name: alias, span, .. } | Item::Const { name: alias, span, .. }
                if alias == name =>
            {
                Some(self.declared(*span, name))
            }
            _ => None,
        });
        found.into_iter().collect()
    }

    fn members(&self, def: &TypeDef, name: &str, kind: MemberKind) -> Vec<Declaration> {
        let mut found = Vec::new();
        if kind != MemberKind::Cases {
            let field = def.fields.iter().find(|field| field.name == name);
            found.extend(field.map(|field| self.declared(field.span, name)));
            found.extend(self.method(&def.methods, name));
        }
        if kind != MemberKind::Values {
            let case = def.cases.iter().find(|case| case.name == name);
            found.extend(case.map(|case| self.declared(case.span, name)));
        }
        found
    }

    fn of_type(&self, owner: &str, name: &str, kind: MemberKind) -> Vec<Declaration> {
        let mut found: Vec<Declaration> = self
            .types()
            .filter(|def| def.name == owner)
            .flat_map(|def| self.members(def, name, kind))
            .collect();
        let extended = self.extensions().filter(|(extended, _)| *extended == owner);
        found.extend(extended.filter_map(|(_, methods)| self.method(methods, name)));
        found
    }
}

/// What a hover shows for a declaration, as Markdown: what it is as written, then its doc.
pub(crate) fn hover_text(declaration: &Declaration, name: &str, sources: &Sources) -> Option<String> {
    let (_, text, base) = sources.file_at(declaration.header)?;
    let local = |at: u32| usize::try_from(at.saturating_sub(base)).unwrap_or(0);
    let (start, end) = (local(declaration.header.start), local(declaration.header.end));
    let line_start = text.get(..start)?.rfind('\n').map_or(0, |newline| newline + 1);
    let indent = start - line_start;
    let shown = match &declaration.holds {
        Some(ty) => format!("{name}: {ty}"),
        None if declaration.local => return None,
        None => written(text.get(start..end.min(text.len()))?, indent),
    };
    let doc = doc_comment(text, line_start);
    let code = format!("```fennec\n{shown}\n```");
    Some(if doc.is_empty() { code } else { format!("{code}\n\n{doc}") })
}

/// A declaration's text as written: its first line, or all of a signature broken over
/// several, with the indentation they share taken off.
fn written(text: &str, indent: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let single = lines.len() == 1 || !text.trim_start().starts_with("def ");
    let lines = if single { lines.into_iter().take(1).collect() } else { lines };
    let dedented: Vec<&str> = lines
        .iter()
        .enumerate()
        .map(|(at, line)| {
            if at == 0 {
                return *line;
            }
            let spaces = line.len() - line.trim_start_matches(' ').len();
            line.get(spaces.min(indent)..).unwrap_or(line)
        })
        .collect();
    dedented.join("\n").trim_end().trim_end_matches(':').to_owned()
}

/// The `##` doc comment on the lines just above the line starting at `line_start`, without
/// its markers.
pub(crate) fn doc_comment(text: &str, line_start: usize) -> String {
    let above = text.get(..line_start).unwrap_or("");
    let mut lines: Vec<&str> = above
        .lines()
        .rev()
        .map(str::trim_start)
        .take_while(|line| line.starts_with("##"))
        .map(|line| {
            line.trim_start_matches("##").strip_prefix(' ').unwrap_or_else(|| line.trim_start_matches("##"))
        })
        .collect();
    lines.reverse();
    lines.join("\n")
}
