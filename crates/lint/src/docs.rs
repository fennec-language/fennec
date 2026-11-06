//! `missing-doc`: every `pub` item has a `##` comment of at least a line, saying what it's for:
//! a function, a type, its `pub` methods and the fields it shows, an interface, an alias or a
//! constant. A `pub` type shows every field but its `_name` ones, since a field has no `pub` of
//! its own. There's no fix, since only someone who knows what it's for can write that. The parser
//! drops `pub`, so this reads it from the item's line, as `unused-item` does. An annotation can
//! sit between the comment and what it documents.

use fennec_syntax::{Diagnostic, FnDef, Item, Module, Sources, Span, TypeDef};

use crate::Lint;

/// What's declared and could be `pub`, as its kind, name and where it is. A field is `pub`
/// when its type is, so it comes with where its type is written.
type Declared<'m> = (&'static str, &'m str, Span, Option<Span>);

fn declared(module: &Module) -> Vec<Declared<'_>> {
    fn add_methods<'m>(defs: &'m [FnDef], found: &mut Vec<Declared<'m>>) {
        found.extend(defs.iter().map(|def| ("method", def.name.as_str(), def.span, None)));
    }
    fn add_type<'m>(kind: &'static str, def: &'m TypeDef, found: &mut Vec<Declared<'m>>) {
        found.push((kind, def.name.as_str(), def.span, None));
        let shown = def.fields.iter().filter(|field| !field.name.starts_with('_'));
        found.extend(shown.map(|field| ("field", field.name.as_str(), field.span, Some(def.span))));
        add_methods(&def.methods, found);
    }
    let mut found = Vec::new();
    for item in &module.items {
        match item {
            Item::Function(def) => found.push(("function", def.name.as_str(), def.span, None)),
            Item::Struct(def) => add_type("struct", def, &mut found),
            Item::Enum(def) => add_type("enum", def, &mut found),
            Item::Error(def) => add_type("error", def, &mut found),
            Item::Interface(interface) => {
                found.push(("interface", interface.name.as_str(), interface.span, None));
                add_methods(&interface.methods, &mut found);
            }
            Item::Extend(extension) => add_methods(&extension.methods, &mut found),
            Item::TypeAlias { name, span, .. } => found.push(("alias", name.as_str(), *span, None)),
            Item::Const { name, span, .. } => found.push(("constant", name.as_str(), *span, None)),
            _ => {}
        }
    }
    found
}

/// Whether the line starting at `line_start` has a `##` comment just above it, past any
/// annotations.
fn documented(text: &str, line_start: usize) -> bool {
    let above = text.get(..line_start).unwrap_or("");
    above
        .lines()
        .rev()
        .map(str::trim_start)
        .find(|line| !line.starts_with('@'))
        .is_some_and(|line| line.starts_with("##"))
}

/// Where the line holding `span` starts in its file, with the file's text.
fn line_of(sources: &Sources, span: Span) -> Option<(&str, usize)> {
    let (_, text, base) = sources.file_at(span)?;
    let start = usize::try_from(span.start - base).ok()?;
    Some((text, text.get(..start).and_then(|before| before.rfind('\n')).map_or(0, |at| at + 1)))
}

pub(crate) fn of(module: &Module, sources: &Sources) -> Vec<Lint> {
    let starts_pub = |(text, line_start): (&str, usize)| {
        text.get(line_start..).is_some_and(|line| line.trim_start().starts_with("pub "))
    };
    let mut found = Vec::new();
    for (kind, name, span, owner) in declared(module) {
        let Some((text, line_start)) = line_of(sources, span) else { continue };
        let public = match owner {
            Some(owner) => line_of(sources, owner).is_some_and(starts_pub),
            None => starts_pub((text, line_start)),
        };
        if public && !documented(text, line_start) {
            let diagnostic = Diagnostic::new(format!("the `pub` {kind} `{name}` has no doc comment"), span)
                .with_help("write a `##` line above it saying what it's for");
            found.push(Lint::new("missing-doc", diagnostic));
        }
    }
    found
}
