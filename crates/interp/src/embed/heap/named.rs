//! Values a backend lays out as ordinary structs and enums, but the interpreter holds as
//! values of their own: an optional held as an enum, an `Id` and an `IdMap`. Reading a block
//! turns these into what the interpreter holds, and writing one turns them back.

use std::rc::Rc;

use super::{State, write};
use crate::packed::Element;
use crate::value::{Id, IdMap, Instance, Slot, Value};

/// An optional held as an enum, with cases `None` and `Some(value)`.
fn is_option(name: &str) -> bool {
    name.starts_with("Option[")
}

/// An `IdMap` laid out as its items, each slot's generation, and how many are live. A
/// removed slot's generation is held as `-1 - generation`, and its item is zero.
fn is_id_map(name: &str) -> bool {
    name.starts_with("IdMap[")
}

/// Whether items of a kind read as the interpreter's packed items: numbers, and structs and
/// enums of them that aren't one of these or a tuple. Anything else is read item by item.
pub(in crate::embed) fn packable(element: &Element) -> bool {
    let all = |fields: &[(Rc<str>, Element)]| fields.iter().all(|(_, field)| packable(field));
    match element {
        Element::Int | Element::Float | Element::Bool => true,
        // A tuple a backend lays out as a struct, like `(int, int)`, is a tuple to the interpreter.
        Element::Struct { name, fields } => {
            !name.starts_with('(') && name.as_ref() != "Id" && !is_id_map(name) && all(fields)
        }
        Element::Enum { name, cases, .. } => !is_option(name) && cases.iter().all(|(_, fields)| all(fields)),
        _ => false,
    }
}

/// What the interpreter holds for a struct or enum read from a block named `name`.
pub(super) fn interpreted(name: &str, value: Value) -> Value {
    let Value::Instance(instance) = &value else { return value };
    if is_option(name) {
        return match instance.case.as_deref() {
            Some("Some") => instance.field("value").cloned().unwrap_or(Value::None),
            _ => Value::None,
        };
    }
    if name == "Id" {
        return id(instance).map_or(value, Value::Id);
    }
    if is_id_map(name) {
        return id_map(instance).map_or(value, |map| Value::IdMap(Rc::new(map)));
    }
    value
}

fn id(instance: &Instance) -> Option<Id> {
    let number = |field: &str| match instance.field(field)? {
        Value::Int(n) => Some(*n),
        _ => None,
    };
    let slot = usize::try_from(number("slot")?).ok()?;
    let generation = u32::try_from(number("generation")?).ok()?;
    Some(Id { slot, generation })
}

fn id_map(instance: &Instance) -> Option<IdMap> {
    let list = |field: &str| match instance.field(field)?.clone().unpacked() {
        Value::List(items) => Some(items),
        _ => None,
    };
    let (items, generations) = (list("items")?, list("generations")?);
    let slots = items.iter().zip(generations.iter()).map(|(item, generation)| {
        let Value::Int(generation) = generation else { return None };
        Some(if *generation < 0 {
            Slot { generation: u32::try_from(-1 - generation).ok()?, value: None }
        } else {
            Slot { generation: u32::try_from(*generation).ok()?, value: Some(item.clone()) }
        })
    });
    Some(IdMap { slots: slots.collect::<Option<_>>()? })
}

/// What `write_named` did.
pub(super) enum Written {
    Done,
    /// The value isn't of the type laid out.
    Failed,
    /// It isn't one of these, so `write` goes on as usual.
    NotNamed,
}

impl From<Option<()>> for Written {
    fn from(written: Option<()>) -> Self {
        if written.is_some() { Written::Done } else { Written::Failed }
    }
}

/// Writes a value the interpreter holds as one of these, when `element` is laid out for it.
pub(super) fn write_named(
    state: &mut State,
    element: &Element,
    value: &Value,
    words: &mut Vec<u64>,
) -> Written {
    match (element, value) {
        (Element::Enum { name, cases, words: size }, value) if is_option(name) => {
            let start = words.len();
            let written = match (value, cases.get(1).and_then(|(_, fields)| fields.first())) {
                (Value::None, _) => {
                    words.push(0);
                    Some(())
                }
                (value, Some((_, inner))) => {
                    words.push(1);
                    write(state, inner, value, words)
                }
                (_, None) => None,
            };
            words.resize(start + size, 0);
            written.into()
        }
        (Element::Struct { name, .. }, Value::Id(id)) if name.as_ref() == "Id" => {
            let Ok(slot) = u64::try_from(id.slot) else { return Written::Failed };
            words.extend([slot, u64::from(id.generation)]);
            Written::Done
        }
        (Element::Struct { name, fields }, Value::IdMap(map)) if is_id_map(name) => {
            write_id_map(state, fields, map, words).into()
        }
        _ => Written::NotNamed,
    }
}

fn write_id_map(
    state: &mut State,
    fields: &[(Rc<str>, Element)],
    map: &IdMap,
    words: &mut Vec<u64>,
) -> Option<()> {
    let Some((_, Element::List(item))) = fields.first() else { return None };
    let mut items = Vec::new();
    let mut generations = Vec::new();
    for slot in &map.slots {
        let generation = i64::from(slot.generation);
        if let Some(value) = &slot.value {
            write(state, item, value, &mut items)?;
            generations.push(generation.cast_unsigned());
        } else {
            items.resize(items.len() + item.words(), 0);
            generations.push((-1 - generation).cast_unsigned());
        }
    }
    let live = map.slots.iter().filter(|slot| slot.value.is_some()).count();
    words.push(u64::try_from(super::super::lists::list_of_items(state, item, &items)).ok()?);
    let ints = Rc::new(Element::Int);
    words.push(u64::try_from(super::super::lists::list_of_items(state, &ints, &generations)).ok()?);
    words.push(u64::try_from(live).ok()?);
    Some(())
}
