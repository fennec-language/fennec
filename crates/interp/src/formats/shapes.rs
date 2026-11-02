//! The standard library's collections in their natural shape: a `SortedMap` is an object, a
//! `SortedSet` or a `Deque` is an array, and a `Grid` is an array of rows. Each collection
//! says what it holds with its own method and is built again with its own `of`, so the shapes
//! live in the standard library, and this only knows which types have one. A program's own
//! type of the same name is written field by field, like any other.

use std::rc::Rc;

use fennec_syntax::{Diagnostic, Span, TypeExpr};

use crate::interp::Interp;
use crate::value::{Instance, Value};

/// Each collection, the method that says what it holds, and the type that's written as.
const SHAPES: [(&str, &str); 4] =
    [("SortedMap", "items"), ("SortedSet", "items"), ("Deque", "items"), ("Grid", "rows")];

impl Interp<'_> {
    /// Whether `name` is a standard library collection with a natural shape.
    pub(super) fn has_shape(&self, name: &str) -> bool {
        SHAPES.iter().any(|(shaped, _)| *shaped == name)
            && self.type_info(name).is_some_and(|info| info.def.span.in_prelude())
    }

    /// The type a collection is written as: `dict[K, V]` for a `SortedMap[K, V]`,
    /// `list[list[T]]` for a `Grid[T]`, and `list[T]` for the others.
    pub(super) fn shape_of(ty: &TypeExpr) -> TypeExpr {
        let like = |name: &str, args: Vec<TypeExpr>| TypeExpr {
            name: name.to_owned(),
            args,
            optional: false,
            span: ty.span,
        };
        match ty.name.as_str() {
            "SortedMap" => like("dict", ty.args.clone()),
            "Grid" => like("list", vec![like("list", ty.args.clone())]),
            _ => like("list", ty.args.clone()),
        }
    }

    /// A collection built again from what `shape_of` read, with its own `of`. `at` is where
    /// it is in the document, for saying which row of a `Grid` is the wrong length.
    pub(super) fn build_shaped(
        &mut self,
        name: &str,
        read: Value,
        at: &str,
        span: Span,
    ) -> Result<Value, String> {
        if let ("Grid", Value::List(rows)) = (name, &read) {
            let width = |row: &Value| if let Value::List(cells) = row { cells.len() } else { 0 };
            let first = rows.first().map_or(0, width);
            if let Some(n) = rows.iter().position(|row| width(row) != first) {
                let row = rows.get(n).map_or(0, width);
                let cells = if row == 1 { "cell" } else { "cells" };
                return Err(format!("`{at}[{n}]` has {row} {cells}, but the first row has {first}"));
            }
        }
        let read = match (name, read) {
            ("SortedMap", Value::Dict(pairs)) => Value::list(
                pairs.iter().map(|(key, value)| Value::tuple(vec![key.clone(), value.clone()])).collect(),
            ),
            (_, read) => read,
        };
        let of = self
            .type_info(name)
            .and_then(|info| info.methods.get("of").cloned())
            .ok_or_else(|| format!("`{name}` has no `of`"))?;
        self.call_with_values(&of, vec![read], span).map_err(|problem| problem.message)
    }

    /// `value` with every standard library collection in it replaced by its natural shape,
    /// ready to be written.
    pub(super) fn natural(&mut self, value: &Value, span: Span) -> Result<Value, Diagnostic> {
        Ok(match value {
            Value::Instance(instance) if self.has_shape(&instance.type_name) => {
                let name = instance.type_name.to_string();
                let method =
                    SHAPES.iter().find(|(shaped, _)| *shaped == name).map_or("items", |(_, method)| method);
                let held = self
                    .type_info(&name)
                    .and_then(|info| info.methods.get(method).cloned())
                    .ok_or_else(|| Diagnostic::new(format!("`{name}` has no `{method}`"), span))?;
                let items = self.call_with_values(&held, vec![value.clone()], span)?;
                let items = self.natural(&items, span)?;
                match (name.as_str(), items) {
                    ("SortedMap", Value::List(pairs)) => {
                        let pairs: Vec<(Value, Value)> = pairs
                            .iter()
                            .filter_map(|pair| match pair {
                                Value::Tuple(parts) => Some((parts.first()?.clone(), parts.get(1)?.clone())),
                                _ => None,
                            })
                            .collect();
                        Value::Dict(Rc::new(pairs.into()))
                    }
                    (_, items) => items,
                }
            }
            Value::Instance(instance) => {
                let mut fields = Vec::new();
                for (name, held) in &instance.fields {
                    fields.push((Rc::clone(name), self.natural(held, span)?));
                }
                Value::Instance(Rc::new(Instance { fields, ..instance.as_ref().clone() }))
            }
            Value::List(items) => Value::list(self.all_natural(items, span)?),
            Value::Tuple(items) => Value::tuple(self.all_natural(items, span)?),
            Value::Dict(pairs) => {
                let mut read = Vec::new();
                for (key, held) in pairs.iter() {
                    read.push((key.clone(), self.natural(held, span)?));
                }
                Value::Dict(Rc::new(read.into()))
            }
            Value::IdMap(map) => {
                let mut map = map.as_ref().clone();
                for slot in &mut map.slots {
                    if let Some(held) = &slot.value {
                        slot.value = Some(self.natural(held, span)?);
                    }
                }
                Value::IdMap(Rc::new(map))
            }
            other => other.clone(),
        })
    }

    fn all_natural(&mut self, items: &[Value], span: Span) -> Result<Vec<Value>, Diagnostic> {
        items.iter().map(|item| self.natural(item, span)).collect()
    }
}
