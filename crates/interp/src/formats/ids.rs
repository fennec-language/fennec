//! `Table`s and the `Id`s into them, in JSON and TOML. A `Table` is a list of its items in order,
//! and an `Id` is where its item is in that list, so the text says nothing of slots. Loading makes
//! a new `Table`, whose items are where the list has them, so each `Id` read finds its item. One
//! `Table` of each type of item can be in a document, so an `Id` can only be in that one.

use std::collections::HashMap;
use std::rc::Rc;

use fennec_syntax::TypeExpr;

use super::decode::{given_types, substituted, written};
use crate::value::{Id, Instance, Table, Value};

/// Where something is as a message says it: "`units[0].target`", or `whole` when it's all of it.
fn place(at: &str, whole: &str) -> String {
    let at = at.strip_prefix('.').unwrap_or(at);
    if at.is_empty() { whole.to_owned() } else { format!("`{at}`") }
}

fn inside(at: &str, part: &str) -> String {
    if part.starts_with('[') { format!("{at}{part}") } else { format!("{at}.{part}") }
}

/// A `Table` found in a walk: the type of its items when that's known, where it is, and it.
type FoundTable = (Option<String>, String, Table);

/// What a walk through a value found: each `Table`, and each `Id`, with the type it's for when
/// that's known, where it is, and whether it's held as an optional.
#[derive(Default)]
struct Found {
    tables: Vec<FoundTable>,
    ids: Vec<(Option<String>, String, bool, Id)>,
}

impl Found {
    /// The one `Table` an `Id` of `kind` can be in, if there's one.
    fn table_for(&self, kind: Option<&String>) -> Option<&FoundTable> {
        let mut fits = self
            .tables
            .iter()
            .filter(|(table, ..)| kind.is_none() || table.is_none() || table.as_ref() == kind);
        let first = fits.next()?;
        fits.next().is_none().then_some(first)
    }

    /// Two `Table`s of the same type, which leave an `Id` unable to say which it's in.
    fn twice(&self, whole: &str) -> Option<String> {
        let mut seen: Vec<(&String, &String)> = Vec::new();
        for (kind, at, _) in &self.tables {
            let Some(kind) = kind else { continue };
            if let Some((_, first)) = seen.iter().find(|(seen, _)| *seen == kind) {
                return Some(format!(
                    "{} and {} are both a `Table[{kind}]`, so an Id can't say which it's in",
                    place(first, whole),
                    place(at, whole)
                ));
            }
            seen.push((kind, at));
        }
        None
    }
}

/// A type named and nothing more, for a value whose type is only known by its name.
fn named(name: &str) -> TypeExpr {
    TypeExpr {
        name: name.to_owned(),
        args: Vec::new(),
        optional: false,
        span: fennec_syntax::Span::default(),
    }
}

/// `value` with each `Table` as the list of its items and each `Id` as where its item is.
fn as_saved(value: &Value, positions: &HashMap<Id, Option<i64>>) -> Value {
    match value {
        Value::Id(id) => match positions.get(id) {
            Some(Some(position)) => Value::Int(*position),
            _ => Value::None,
        },
        Value::Table(map) => Value::list(map.entries().map(|(_, held)| as_saved(held, positions)).collect()),
        Value::List(items) => Value::list(items.iter().map(|held| as_saved(held, positions)).collect()),
        Value::Tuple(items) => Value::tuple(items.iter().map(|held| as_saved(held, positions)).collect()),
        Value::Dict(pairs) => {
            let pairs: Vec<(Value, Value)> = pairs
                .iter()
                .map(|(key, held)| (as_saved(key, positions), as_saved(held, positions)))
                .collect();
            Value::Dict(Rc::new(pairs.into()))
        }
        Value::Instance(instance) => {
            let fields = instance
                .fields
                .iter()
                .map(|(name, held)| (name.clone(), as_saved(held, positions)))
                .collect();
            Value::Instance(Rc::new(Instance { fields, ..instance.as_ref().clone() }))
        }
        other => other.clone(),
    }
}
