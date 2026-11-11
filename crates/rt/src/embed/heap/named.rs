//! Values a backend lays out as ordinary structs and enums, but a `Value` holds as values of
//! their own: an optional held as an enum, a result, an `Id` and a `Table`. Reading a block turns
//! these into `Value`s, and writing one turns them back.

use std::rc::Rc;

use super::{State, write};
use crate::packed::Element;
use crate::value::{Id, Instance, Slot, Table, Value};

/// An optional held as an enum, with cases `None` and `Some(value)`.
pub(super) fn is_option(name: &str) -> bool {
    name.starts_with("Option[")
}

/// A result held as an enum, with cases `Ok(value)` and `Err(error)`.
pub(super) fn is_result(name: &str) -> bool {
    name.starts_with("Result[")
}

/// One of several types held as an enum, with a case for each holding the value: several errors,
/// `A | B`, or a union's members, `list[int] | str`. The ` | ` is outside any brackets, so a generic
/// enum given a union, `Pair[A | B]`, is the enum it is.
pub(super) fn is_one_of_several(name: &str) -> bool {
    let mut depth = 0_usize;
    for (at, character) in name.char_indices() {
        match character {
            '[' | '(' => depth += 1,
            ']' | ')' => depth = depth.saturating_sub(1),
            '|' if depth == 0 && name.get(..at).is_some_and(|before| before.ends_with(' ')) => return true,
            _ => {}
        }
    }
    false
}

/// A fixed-size array a backend keeps in place, laid out as a struct with a field for each item,
/// named after its type, like `array[int, 4]`. A `Value` holds it as a list.
pub(in crate::embed) fn is_array(name: &str) -> bool {
    name.starts_with("array[")
}

/// A `Table` laid out as its items, each slot's generation, and how many are live. A
/// removed slot's generation is held as `-1 - generation`, and its item is zero.
pub(super) fn is_id_map(name: &str) -> bool {
    name.starts_with("Table[")
}

/// Whether items of a kind read as a `Value`'s packed items: numbers, and structs and
/// enums of them that aren't one of these or a tuple. Anything else is read item by item.
pub(in crate::embed) fn packable(element: &Element) -> bool {
    let all = |fields: &[(Rc<str>, Element)]| fields.iter().all(|(_, field)| packable(field));
    match element {
        Element::Int | Element::Float | Element::Bool | Element::U64 | Element::F32 => true,
        // A tuple a backend lays out as a struct, like `(int, int)`, is a tuple as a `Value`.
        Element::Struct { name, fields } => {
            !name.starts_with('(')
                && !is_array(name)
                && name.as_ref() != "Id"
                && !is_id_map(name)
                && all(fields)
        }
        Element::Enum { name, cases, .. } => {
            !is_option(name)
                && !is_result(name)
                && !is_one_of_several(name)
                && cases.iter().all(|(_, fields)| all(fields))
        }
        _ => false,
    }
}

/// What a `Value` holds for a struct or enum read from a block named `name`.
pub(super) fn pinned_outcome(name: &str, value: Value) -> Value {
    let Value::Instance(instance) = &value else { return value };
    if is_option(name) {
        return match instance.case.as_deref() {
            Some("Some") => instance.field("value").cloned().unwrap_or(Value::None),
            _ => Value::None,
        };
    }
    if is_result(name) {
        let held = Rc::new(instance.fields.first().map_or(Value::Unit, |(_, held)| held.clone()));
        return match instance.case.as_deref() {
            Some("Err") => Value::Err(held),
            _ => Value::Ok(held),
        };
    }
    if is_one_of_several(name) {
        return instance.fields.first().map_or(Value::Unit, |(_, held)| held.clone());
    }
    if name == "Id" {
        return id(instance).map_or(value, Value::Id);
    }
    if is_id_map(name) {
        return id_map(instance).map_or(value, |map| Value::Table(Rc::new(map)));
    }
    value
}

fn id(instance: &Instance) -> Option<Id> {
    let number = |field: &str| match instance.field(field)? {
        Value::Int(n) => Some(*n),
        _ => None,
    };
    let key = u64::try_from(number("key")?).ok()?;
    let tag = u64::try_from(number("tag")?).ok()?;
    Some(Id::from_key(key, tag))
}

fn id_map(instance: &Instance) -> Option<Table> {
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
    let tag = match instance.field("tag")? {
        Value::Int(tag) => u64::try_from(*tag).ok()?,
        _ => return None,
    };
    Some(Table::tagged(slots.collect::<Option<_>>()?, tag))
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

/// Writes a `Value` held as one of these, when `element` is laid out for it.
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
        (Element::Enum { name, cases, words: size }, Value::Ok(held) | Value::Err(held))
            if is_result(name) =>
        {
            let wanted = if matches!(value, Value::Err(_)) { "Err" } else { "Ok" };
            let Some((case, (_, fields))) =
                cases.iter().enumerate().find(|(_, (case, _))| case.as_ref() == wanted)
            else {
                return Written::Failed;
            };
            let start = words.len();
            words.push(u64::try_from(case).unwrap_or(0));
            let written = match fields.first() {
                Some((_, inner)) => write(state, inner, held, words),
                None => Some(()),
            };
            words.resize(start + size, 0);
            written.into()
        }
        (Element::Struct { name, .. }, Value::Id(id)) if name.as_ref() == "Id" => {
            words.extend([id.key(), id.tag]);
            Written::Done
        }
        (Element::Struct { name, fields }, Value::Table(map)) if is_id_map(name) => {
            write_id_map(state, fields, map, words).into()
        }
        _ => Written::NotNamed,
    }
}

fn write_id_map(
    state: &mut State,
    fields: &[(Rc<str>, Element)],
    map: &Table,
    words: &mut Vec<u64>,
) -> Option<()> {
    let item = match fields.first() {
        Some((_, Element::List(item))) => Rc::clone(item),
        Some((_, Element::ListOf(number))) => Rc::clone(state.tables.elements.get(*number)?),
        _ => return None,
    };
    let mut items = Vec::new();
    let mut generations = Vec::new();
    for slot in &map.slots {
        let generation = i64::from(slot.generation);
        if let Some(value) = &slot.value {
            write(state, &item, value, &mut items)?;
            generations.push(generation.cast_unsigned());
        } else {
            items.resize(items.len() + item.words(), 0);
            generations.push((-1 - generation).cast_unsigned());
        }
    }
    let live = map.slots.iter().filter(|slot| slot.value.is_some()).count();
    words.push(u64::try_from(super::super::lists::list_of_items(state, &item, &items)).ok()?);
    let ints = Rc::new(Element::Int);
    words.push(u64::try_from(super::super::lists::list_of_items(state, &ints, &generations)).ok()?);
    words.push(u64::try_from(live).ok()?);
    // Where to start looking for a free slot: from the first is always right.
    words.push(0);
    words.push(map.tag);
    Some(())
}

#[cfg(test)]
mod tests {
    use super::is_one_of_several;

    #[test]
    fn several_types_are_told_by_a_bar_outside_brackets() {
        for several in ["IoError | NetError", "list[int] | str", "(int, str) | bool", "dict[str, int] | None"]
        {
            assert!(is_one_of_several(several), "{several}");
        }
        for one in [
            "Pair[int | str]",
            "Option[int | str]",
            "Result[int, IoError | NetError]",
            "(int | str, bool)",
            "Shape",
        ] {
            assert!(!is_one_of_several(one), "{one}");
        }
    }
}
