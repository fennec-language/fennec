//! A file's outline, as the protocol's document symbols: its functions, types with their fields,
//! cases and methods, interfaces, extensions, constants, aliases and tests.

use fennec_syntax::{Field, FnDef, Item, Module, Span};

use crate::analysis::Analysis;
use crate::definition::name_in;
use crate::json::Json;
use crate::text;

/// The protocol's numbers for the kinds of symbol shown.
const FUNCTION: usize = 12;
const METHOD: usize = 6;
const FIELD: usize = 8;
const STRUCT: usize = 23;
const ENUM: usize = 10;
const ENUM_MEMBER: usize = 22;
const INTERFACE: usize = 11;
const CLASS: usize = 5;
const CONSTANT: usize = 14;
const EVENT: usize = 24;

/// One symbol in `path`, with those inside it, or None if it's in another file.
fn symbol(
    analysis: &Analysis,
    path: &str,
    name: &str,
    kind: usize,
    span: Span,
    children: Vec<Json>,
) -> Option<Json> {
    let (file, text, start, end) = analysis.place(span)?;
    if file != path {
        return None;
    }
    let (_, _, name_start, name_end) = analysis.place(name_in(&analysis.sources, span, name))?;
    let mut fields = vec![
        ("name".to_owned(), Json::string(name)),
        ("kind".to_owned(), Json::number(kind)),
        ("range".to_owned(), text::range(text, start, end)),
        ("selectionRange".to_owned(), text::range(text, name_start, name_end)),
    ];
    if !children.is_empty() {
        fields.push(("children".to_owned(), Json::Array(children)));
    }
    Some(Json::Object(fields))
}

fn methods(analysis: &Analysis, path: &str, defs: &[FnDef]) -> Vec<Json> {
    defs.iter().filter_map(|def| symbol(analysis, path, &def.name, METHOD, def.span, Vec::new())).collect()
}

fn fields(analysis: &Analysis, path: &str, fields: &[Field]) -> Vec<Json> {
    fields
        .iter()
        .filter_map(|field| symbol(analysis, path, &field.name, FIELD, field.span, Vec::new()))
        .collect()
}

fn item(analysis: &Analysis, path: &str, item: &Item) -> Option<Json> {
    match item {
        Item::Function(def) => symbol(analysis, path, &def.name, FUNCTION, def.span, Vec::new()),
        Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
            let mut children = fields(analysis, path, &def.fields);
            for case in &def.cases {
                let inside = fields(analysis, path, &case.fields);
                children.extend(symbol(analysis, path, &case.name, ENUM_MEMBER, case.span, inside));
            }
            children.extend(methods(analysis, path, &def.methods));
            let kind = if matches!(item, Item::Enum(_)) { ENUM } else { STRUCT };
            symbol(analysis, path, &def.name, kind, def.span, children)
        }
        Item::Interface(interface) => {
            let children = methods(analysis, path, &interface.methods);
            symbol(analysis, path, &interface.name, INTERFACE, interface.span, children)
        }
        Item::Extend(extension) => {
            let children = methods(analysis, path, &extension.methods);
            symbol(analysis, path, &extension.name, CLASS, extension.span, children)
        }
        Item::TypeAlias { name, span, .. } => symbol(analysis, path, name, CLASS, *span, Vec::new()),
        Item::Const { name, span, .. } => symbol(analysis, path, name, CONSTANT, *span, Vec::new()),
        Item::Test(test) => symbol(analysis, path, &test.name, EVENT, test.span, Vec::new()),
        Item::Import(_) | Item::Stmt(_) => None,
    }
}

/// The outline of the file at `path`, in the order it's written.
pub(crate) fn outline(analysis: &Analysis, module: &Module, path: &str) -> Vec<Json> {
    module.items.iter().filter_map(|found| item(analysis, path, found)).collect()
}
