//! `Table`s and the `Id`s into them, in JSON and TOML. A `Table` is a list of its items in order,
//! and an `Id` is where its item is in that list, so the text says nothing of slots. Loading makes
//! a new `Table`, whose items are where the list has them, so each `Id` read finds its item. One
//! `Table` of each type of item can be in a document, so an `Id` can only be in that one.

use std::collections::HashMap;
use std::rc::Rc;

use fennec_syntax::TypeExpr;

use super::decode::{given_types, substituted, written};
use crate::interp::Interp;
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

impl Interp<'_> {
    /// `value` as JSON and TOML save it: each `Table` as a list of its items, and each `Id` as
    /// where its item is in its `Table`, or missing when it's optional and its item has gone.
    pub(crate) fn saved_ids(&self, value: &Value) -> Result<Value, String> {
        let ty = match value {
            Value::Instance(instance) => Some(named(&instance.type_name)),
            _ => None,
        };
        let mut found = Found::default();
        self.find(value, ty.as_ref(), "", &mut found, &mut Vec::new());
        if found.tables.is_empty() && found.ids.is_empty() {
            return Ok(value.clone());
        }
        if let Some(twice) = found.twice("the value") {
            return Err(twice);
        }
        let mut positions: HashMap<(usize, u32), Option<i64>> = HashMap::new();
        for (kind, at, optional, id) in &found.ids {
            let shown = kind.as_deref().unwrap_or("?");
            let Some((_, table_at, table)) = found.table_for(kind.as_ref()) else {
                return Err(format!(
                    "{} is an Id, but nothing saved with it is a `Table[{shown}]`",
                    place(at, "the value")
                ));
            };
            let position = table.entries().position(|(held, _)| held == *id);
            if position.is_none() && !optional {
                return Err(format!(
                    "{} is an Id of something that's been taken out of {}",
                    place(at, "the value"),
                    place(table_at, "the value")
                ));
            }
            positions.insert((id.slot, id.generation), position.and_then(|at| i64::try_from(at).ok()));
        }
        Ok(as_saved(value, &positions))
    }

    /// Checks every `Id` read in `value`, a `ty`, is in the one `Table` of its type, and that
    /// that `Table` holds an item where it says.
    pub(crate) fn check_ids(&self, value: &Value, ty: &TypeExpr) -> Result<(), String> {
        let mut found = Found::default();
        self.find(value, Some(ty), "", &mut found, &mut Vec::new());
        if let Some(twice) = found.twice("the text") {
            return Err(twice);
        }
        for (kind, at, _, id) in &found.ids {
            let shown = kind.as_deref().unwrap_or("?");
            let Some((_, table_at, table)) = found.table_for(kind.as_ref()) else {
                return Err(format!(
                    "{} is an Id, but the text has no `Table[{shown}]` for it to be in",
                    place(at, "the text")
                ));
            };
            let count = table.entries().count();
            if id.slot >= count {
                let items = match count {
                    0 => "nothing".to_owned(),
                    1 => "only 1".to_owned(),
                    n => format!("only {n}"),
                };
                return Err(format!(
                    "{} is item {} of {}, which has {items}",
                    place(at, "the text"),
                    id.slot,
                    place(table_at, "the text")
                ));
            }
        }
        Ok(())
    }

    /// Collects the `Id`s and `Table`s inside `value`, a `ty` when that's known. `open` holds the
    /// structs being looked inside, so one that holds itself is looked at once at a time.
    fn find(
        &self,
        value: &Value,
        ty: Option<&TypeExpr>,
        at: &str,
        found: &mut Found,
        open: &mut Vec<String>,
    ) {
        let arg = |n: usize| ty.and_then(|ty| ty.args.get(n));
        let name = ty.map_or("", |ty| ty.name.as_str());
        match value {
            Value::Id(id) => {
                let optional = ty.is_some_and(|ty| ty.optional);
                found.ids.push((arg(0).map(written), at.to_owned(), optional, *id));
            }
            Value::Table(map) => {
                let item = if name == "Table" { arg(0) } else { None };
                for (n, (_, held)) in map.entries().enumerate() {
                    self.find(held, item, &inside(at, &format!("[{n}]")), found, open);
                }
                found.tables.push((item.map(written), at.to_owned(), map.as_ref().clone()));
            }
            Value::List(items) => {
                let item = if name == "list" { arg(0) } else { None };
                for (n, held) in items.iter().enumerate() {
                    self.find(held, item, &inside(at, &format!("[{n}]")), found, open);
                }
            }
            Value::Dict(pairs) => {
                let item = if name == "dict" { arg(1) } else { None };
                for (key, held) in pairs.iter() {
                    let at = inside(at, &format!("[{}]", crate::format::repr(key)));
                    self.find(held, item, &at, found, open);
                }
            }
            Value::Tuple(items) => {
                for (n, held) in items.iter().enumerate() {
                    let item = if name == "tuple" { arg(n) } else { None };
                    self.find(held, item, &inside(at, &format!("[{n}]")), found, open);
                }
            }
            Value::Instance(instance) => self.find_fields(instance, ty, at, found, open),
            _ => {}
        }
    }

    fn find_fields(
        &self,
        instance: &Instance,
        ty: Option<&TypeExpr>,
        at: &str,
        found: &mut Found,
        open: &mut Vec<String>,
    ) {
        let own = ty.cloned().unwrap_or_else(|| named(&instance.type_name));
        if open.iter().any(|seen| *seen == written(&own)) {
            return;
        }
        let Some(info) = self.type_info(&instance.type_name) else { return };
        let fields = match &instance.case {
            Some(case) => info.def.cases.iter().find(|each| *each.name == **case).map(|each| &each.fields),
            None => Some(&info.def.fields),
        };
        let given = given_types(&info.def.generics, &own);
        open.push(written(&own));
        for field in fields.into_iter().flatten() {
            let Some(held) = instance.field(&field.name) else { continue };
            let declared = field.ty.as_ref().map(|field_ty| substituted(field_ty, &given));
            self.find(held, declared.as_ref(), &inside(at, &field.name), found, open);
        }
        open.pop();
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
fn as_saved(value: &Value, positions: &HashMap<(usize, u32), Option<i64>>) -> Value {
    match value {
        Value::Id(id) => match positions.get(&(id.slot, id.generation)) {
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
