//! Names: written the way each kind of name is written, long enough to say what they are, and
//! not the name of a builtin. None of these has a fix, since a good name needs a person, but each
//! is an exact test, so none can be wrong.

use std::collections::HashSet;

use fennec_syntax::{
    Diagnostic, ExprKind, FnDef, Import, Item, Module, Pattern, Sources, Span, Stmt, StmtKind, parse_at,
};

use crate::Lint;
use crate::sizes::{blocks, functions};

/// Short names that say enough, by long use.
const SHORT_ENOUGH: [&str; 9] = ["i", "j", "k", "n", "x", "y", "z", "dt", "id"];

/// Two-letter names that are words, not abbreviations.
const WORDS: [&str; 18] = [
    "at", "by", "to", "of", "on", "in", "is", "it", "up", "go", "do", "no", "ok", "or", "as", "if", "be",
    "we",
];

/// The builtins a local could hide.
const BUILTINS: [&str; 21] = [
    "all",
    "any",
    "bytes",
    "copy",
    "enumerate",
    "float",
    "ignore",
    "int",
    "list",
    "max",
    "min",
    "parallel",
    "range",
    "replace",
    "reversed",
    "round",
    "set",
    "sorted",
    "str",
    "sum",
    "zip",
];

/// What a name names, which decides how it's written.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Functions, methods, parameters, locals and fields: `snake_case`.
    Value,
    /// Structs, enums, errors, interfaces, aliases and enum cases: `CapWords`.
    Type,
    /// A module's constants: `UPPER_CASE`.
    Constant,
}

/// One name a program binds.
struct Named<'m> {
    name: &'m str,
    kind: Kind,
    span: Span,
    /// Whether it's bound inside a function, where it could hide a builtin.
    local: bool,
    /// The name of the type it's written with, for a parameter: `fs: Fs` names what it holds.
    written_type: Option<&'m str>,
}

fn is_snake(name: &str) -> bool {
    let name = name.strip_prefix('_').unwrap_or(name);
    name.starts_with(|ch: char| ch.is_ascii_lowercase())
        && name.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}

fn is_cap_words(name: &str) -> bool {
    name.starts_with(|ch: char| ch.is_ascii_uppercase()) && name.chars().all(|ch| ch.is_ascii_alphanumeric())
}

fn is_upper(name: &str) -> bool {
    let name = name.strip_prefix('_').unwrap_or(name);
    name.starts_with(|ch: char| ch.is_ascii_uppercase())
        && name.chars().all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
}

/// The names a pattern binds.
fn pattern_names<'m>(pattern: &'m Pattern, found: &mut Vec<&'m str>) {
    match pattern {
        Pattern::Bind(name) => found.push(name),
        Pattern::Variant { args: items, .. } | Pattern::Tuple(items) | Pattern::Or(items) => {
            for item in items {
                pattern_names(item, found);
            }
        }
        Pattern::Wildcard | Pattern::Literal(_) => {}
    }
}

/// The names a block binds: locals, loop names, `with` names and what a `match` takes apart.
fn block_names<'m>(stmts: &'m [Stmt], found: &mut Vec<Named<'m>>) {
    let local =
        |name: &'m str, span: Span| Named { name, kind: Kind::Value, span, local: true, written_type: None };
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Assign { target, op: None, .. } => {
                if let ExprKind::Name(name) = &target.kind {
                    found.push(local(name, target.span));
                } else if let ExprKind::Tuple(items) = &target.kind {
                    for item in items {
                        if let ExprKind::Name(name) = &item.kind {
                            found.push(local(name, item.span));
                        }
                    }
                }
            }
            StmtKind::For { names, .. } => found.extend(names.iter().map(|name| local(name, stmt.span))),
            StmtKind::With { name: Some(name), .. } => found.push(local(name, stmt.span)),
            _ => {}
        }
        for block in blocks(stmt) {
            block_names(block, found);
        }
    }
}

/// The names expressions bind: comprehensions', arrow functions' and `match` arms'.
fn expression_names(def: &FnDef, found: &mut Vec<(String, Span)>) {
    let mut walked = def.clone();
    fennec_syntax::visit::in_function(&mut walked, &mut |expr| match &expr.kind {
        ExprKind::Comprehension { names, .. } | ExprKind::Lambda { params: names, .. } => {
            found.extend(names.iter().map(|name| (name.clone(), expr.span)));
        }
        ExprKind::Match { arms, .. } => {
            for arm in arms {
                let mut names = Vec::new();
                pattern_names(&arm.pattern, &mut names);
                found.extend(names.into_iter().map(|name| (name.to_owned(), arm.span)));
            }
        }
        _ => {}
    });
}

/// Every name a module defines at the top level, with fields, cases and methods.
fn top_level<'m>(module: &'m Module, found: &mut Vec<Named<'m>>) {
    let top =
        |name: &'m str, kind: Kind, span: Span| Named { name, kind, span, local: false, written_type: None };
    for item in &module.items {
        match item {
            Item::Function(def) => found.push(top(&def.name, Kind::Value, def.span)),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                found.push(top(&def.name, Kind::Type, def.span));
                found.extend(def.fields.iter().map(|field| top(&field.name, Kind::Value, field.span)));
                for case in &def.cases {
                    found.push(top(&case.name, Kind::Type, case.span));
                    found.extend(case.fields.iter().map(|field| top(&field.name, Kind::Value, field.span)));
                }
                found.extend(def.methods.iter().map(|method| top(&method.name, Kind::Value, method.span)));
            }
            Item::Interface(interface) => found.push(top(&interface.name, Kind::Type, interface.span)),
            Item::Extend(extension) => {
                found.extend(
                    extension.methods.iter().map(|method| top(&method.name, Kind::Value, method.span)),
                );
            }
            Item::TypeAlias { name, span, .. } => found.push(top(name, Kind::Type, *span)),
            Item::Const { name, span, .. } => found.push(top(name, Kind::Constant, *span)),
            Item::Import(_) | Item::Test(_) | Item::Stmt(_) => {}
        }
    }
}

/// What's wrong with a name, if anything.
fn judge(named: &Named<'_>) -> Option<Lint> {
    let Named { name, kind, span, local, written_type } = *named;
    // An operator method's name, like `+` or `[]`, isn't a name to judge.
    if name == "_" || !name.chars().all(|ch| ch.is_alphanumeric() || ch == '_') {
        return None;
    }
    let short = name.trim_start_matches('_');
    let named_for_its_type = written_type.is_some_and(|ty| ty.to_lowercase() == name);
    let too_short = short.chars().count() == 1 || (short.chars().count() == 2 && !WORDS.contains(&short));
    let (rule, message) = if local && BUILTINS.contains(&name) {
        ("shadowed-builtin", format!("`{name}` hides the builtin of that name: call it something else"))
    } else if too_short && !SHORT_ENOUGH.contains(&name) && !named_for_its_type {
        ("short-name", format!("`{name}` is too short to say what it is: write a word"))
    } else {
        let (written, how) = match kind {
            Kind::Value => (is_snake(name), "`snake_case`"),
            Kind::Type => (is_cap_words(name), "`CapWords`"),
            Kind::Constant => (is_upper(name), "`UPPER_CASE`"),
        };
        if written {
            return None;
        }
        ("naming-case", format!("`{name}` should be written in {how}"))
    };
    Some(Lint::new(rule, Diagnostic::new(message, span)))
}

/// `from std import json as json`, `import std.json as json`, or one thing imported twice.
fn aliases(text: &str, base: u32, found: &mut Vec<Lint>) {
    let Ok(module) = parse_at(text, base) else { return };
    let imports = module
        .items
        .iter()
        .filter_map(|item| if let Item::Import(import) = item { Some(import) } else { None });
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for import in imports {
        if import.names.is_empty() {
            found.extend(whole_module_alias(import));
            continue;
        }
        for (at, (name, bound)) in import.bound().enumerate() {
            let module = import.module.join(".");
            if !seen.insert((module.clone(), name.to_owned())) {
                let message = format!("`{name}` from `{module}` is imported already: use one name for it");
                found.push(Lint::new("needless-alias", Diagnostic::new(message, import.span)));
            } else if import.aliases.get(at).is_some_and(Option::is_some) && name == bound {
                let names: Vec<String> = import
                    .bound()
                    .enumerate()
                    .map(|(other, (name, bound))| {
                        if other == at || name == bound {
                            name.to_owned()
                        } else {
                            format!("{name} as {bound}")
                        }
                    })
                    .collect();
                let fixed = format!("from {module} import {}", names.join(", "));
                let diagnostic = Diagnostic::new(format!("`{name}` is already called that"), import.span)
                    .with_fix(import.span, fixed);
                found.push(Lint::new("needless-alias", diagnostic));
            }
        }
    }
}

/// `import std.json as json` says `from std import json`.
fn whole_module_alias(import: &Import) -> Option<Lint> {
    let alias = import.alias.as_deref()?;
    let (last, parents) = import.module.split_last()?;
    if alias != last || parents.is_empty() {
        return None;
    }
    let fixed = format!("from {} import {last}", parents.join("."));
    let diagnostic =
        Diagnostic::new(format!("write this as `{fixed}`"), import.span).with_fix(import.span, fixed);
    Some(Lint::new("needless-alias", diagnostic))
}

pub(crate) fn of(module: &Module, sources: &Sources) -> Vec<Lint> {
    let mut named = Vec::new();
    top_level(module, &mut named);
    let mut found: Vec<Lint> = named.iter().filter_map(judge).collect();
    for def in functions(module) {
        let mut bound: Vec<Named<'_>> = def
            .params
            .iter()
            .filter(|param| !param.is_self)
            .map(|param| Named {
                name: &param.name,
                kind: Kind::Value,
                span: param.span,
                local: true,
                written_type: param.ty.as_ref().map(|ty| ty.name.as_str()),
            })
            .collect();
        block_names(&def.body, &mut bound);
        let mut from_expressions = Vec::new();
        expression_names(def, &mut from_expressions);
        bound.extend(from_expressions.iter().map(|(name, span)| Named {
            name,
            kind: Kind::Value,
            span: *span,
            local: true,
            written_type: None,
        }));
        // A name given a value again is still one name, so it's judged once.
        let mut judged: HashSet<&str> = HashSet::new();
        found.extend(bound.iter().filter(|named| judged.insert(named.name)).filter_map(judge));
    }
    for (path, text) in sources.files() {
        if let Some((_, base)) = sources.by_path(path) {
            aliases(text, base, &mut found);
        }
    }
    found
}
