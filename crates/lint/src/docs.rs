//! `missing-doc`: every `pub` item has a `##` comment of at least a line, saying what it's for:
//! a function, a type and what's `pub` in it, an interface, an alias or a constant. There's no
//! fix, since only someone who knows what it's for can write that. The parser drops `pub`, so
//! this reads it from the item's line, as `unused-item` does. An annotation can sit between the
//! comment and what it documents.

use fennec_syntax::{Diagnostic, FnDef, Item, Module, Sources, Span, TypeDef};

use crate::Lint;

/// What's declared and could be `pub`, as its kind, name and where it is.
fn declared(module: &Module) -> Vec<(&'static str, &str, Span)> {
    let mut found = Vec::new();
    fn add_methods<'m>(defs: &'m [FnDef], found: &mut Vec<(&'static str, &'m str, Span)>) {
        found.extend(defs.iter().map(|def| ("method", def.name.as_str(), def.span)));
    }
    fn add_type<'m>(kind: &'static str, def: &'m TypeDef, found: &mut Vec<(&'static str, &'m str, Span)>) {
        found.push((kind, def.name.as_str(), def.span));
        found.extend(def.fields.iter().map(|field| ("field", field.name.as_str(), field.span)));
        add_methods(&def.methods, found);
    }
    for item in &module.items {
        match item {
            Item::Function(def) => found.push(("function", def.name.as_str(), def.span)),
            Item::Struct(def) => add_type("struct", def, &mut found),
            Item::Enum(def) => add_type("enum", def, &mut found),
            Item::Error(def) => add_type("error", def, &mut found),
            Item::Interface(interface) => {
                found.push(("interface", interface.name.as_str(), interface.span));
                add_methods(&interface.methods, &mut found);
            }
            Item::Extend(extension) => add_methods(&extension.methods, &mut found),
            Item::TypeAlias { name, span, .. } => found.push(("alias", name.as_str(), *span)),
            Item::Const { name, span, .. } => found.push(("constant", name.as_str(), *span)),
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

pub(crate) fn of(module: &Module, sources: &Sources) -> Vec<Lint> {
    let mut found = Vec::new();
    for (kind, name, span) in declared(module) {
        let Some((_, text, base)) = sources.file_at(span) else { continue };
        let Ok(start) = usize::try_from(span.start - base) else { continue };
        let line_start = text.get(..start).and_then(|before| before.rfind('\n')).map_or(0, |at| at + 1);
        let public = text.get(line_start..).is_some_and(|line| line.trim_start().starts_with("pub "));
        if public && !documented(text, line_start) {
            let diagnostic = Diagnostic::new(format!("the `pub` {kind} `{name}` has no doc comment"), span)
                .with_help("write a `##` line above it saying what it's for");
            found.push(Lint::new("missing-doc", diagnostic));
        }
    }
    found
}
