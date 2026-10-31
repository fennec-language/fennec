//! Lists for native code. A list of ints, floats, bools or plain structs is a block in the
//! heap (see `heap`): its owner count, its shape, how many items it has, how many it has
//! room for, then its items packed one after another, a word per number. Native code
//! reads, changes and appends items itself while the list has one owner and room; copying
//! a shared list and growing a full one happen here.
//!
//! A list's shape is counted from the string shape, after the dicts' shapes: one for each
//! kind of item, by the kind's number in the table of elements.

use std::rc::Rc;

use fennec_syntax::Diagnostic;

use super::heap::release;
use super::{State, attempt, span, with};
use crate::packed::{Element, Packed};
use crate::value::Value;

/// Words a list's block starts with: owner count, shape, items, room.
pub const LIST_HEADER_WORDS: usize = 4;

/// The fewest items a new list has room for.
const MIN_ROOM: usize = 4;

/// The shape of lists of the element numbered `element`.
fn shape(state: &State, element: usize) -> u64 {
    state.tables.str_shape + 1 + super::dicts::SHAPES + u64::try_from(element).unwrap_or(0)
}

/// The kind of item a block holds, when it's a list's.
pub(super) fn element_of(state: &State, block: &[u64]) -> Option<Rc<Element>> {
    let at = block.get(1)?.checked_sub(state.tables.str_shape + 1 + super::dicts::SHAPES)?;
    state.tables.elements.get(usize::try_from(at).ok()?).cloned()
}

fn element(state: &State, element: i64) -> Option<(usize, Rc<Element>)> {
    let at = usize::try_from(element).ok()?;
    Some((at, Rc::clone(state.tables.elements.get(at)?)))
}

/// A block owned once holding `items` words of items, with room for at least `room`.
fn build(state: &mut State, at: usize, stride: usize, items: &[u64], room: usize) -> Box<[u64]> {
    let count = items.len() / stride.max(1);
    let room = room.max(count).max(MIN_ROOM);
    let mut block = state.heap.fresh(LIST_HEADER_WORDS + room * stride);
    let number = |n: usize| u64::try_from(n).unwrap_or(0);
    if let Some(header) = block.get_mut(..LIST_HEADER_WORDS) {
        header.copy_from_slice(&[1, shape(state, at), number(count), number(room)]);
    }
    if let Some(words) = block.get_mut(LIST_HEADER_WORDS..LIST_HEADER_WORDS + items.len()) {
        words.copy_from_slice(items);
    }
    block
}

/// The words of a list block's items in use.
pub(super) fn items(block: &[u64], stride: usize) -> &[u64] {
    let count = block.get(2).and_then(|n| usize::try_from(*n).ok()).unwrap_or(0);
    block.get(LIST_HEADER_WORDS..LIST_HEADER_WORDS + count * stride).unwrap_or_default()
}

/// A new list owned once of `count` items of a kind, all zero, for native code to fill.
pub extern "C" fn new(element: i64, count: i64) -> i64 {
    with(|state| {
        let Some((at, element)) = self::element(state, element) else { return 0 };
        let count = usize::try_from(count).unwrap_or(0);
        // A fresh block is already zero, so only the header is written.
        let mut block = build(state, at, element.words(), &[], count);
        if let Some(length) = block.get_mut(2) {
            *length = u64::try_from(count).unwrap_or(0);
        }
        state.heap.insert(block)
    })
}

/// Takes over a list's owner and gives back one with a zeroed item more on the end, for
/// native code to fill: the same block when it had one owner and room, or else a copy
/// with room to double.
pub extern "C" fn grow(list: i64) -> i64 {
    with(|state| grow_in(state, list, 1).unwrap_or(list))
}

/// Takes over a list's owner and gives back a block only that owner holds, copying the
/// list if anything else shares it.
pub extern "C" fn unshare(list: i64) -> i64 {
    with(|state| grow_in(state, list, 0).unwrap_or(list))
}

fn grow_in(state: &mut State, list: i64, more: usize) -> Option<i64> {
    let address = u64::try_from(list).ok()?;
    let block = state.heap.block(address)?;
    let element = element_of(state, block)?;
    let at = usize::try_from(block.get(1)?.checked_sub(state.tables.str_shape + 1 + super::dicts::SHAPES)?)
        .ok()?;
    let stride = element.words();
    let count = usize::try_from(*block.get(2)?).ok()?;
    let room = usize::try_from(*block.get(3)?).ok()?;
    let shared = block.first() != Some(&1);
    let address = if shared || count + more > room {
        let kept = items(block, stride).to_vec();
        let grown =
            build(state, at, stride, &kept, if count + more > room { 2 * (count + more) } else { room });
        if shared {
            release(state, address);
        } else {
            state.heap.discard(address);
        }
        u64::try_from(state.heap.insert(grown)).ok()?
    } else {
        address
    };
    if more > 0 {
        let block = state.heap.block_mut(address)?;
        let start = LIST_HEADER_WORDS + count * stride;
        block.get_mut(start..start + more * stride)?.fill(0);
        *block.get_mut(2)? = u64::try_from(count + more).ok()?;
    }
    i64::try_from(address).ok()
}

/// A list's block as the interpreter holds it: a packed list.
pub(super) fn read(state: &State, block: &[u64]) -> Value {
    let Some(element) = element_of(state, block) else { return Value::None };
    let words = items(block, element.words()).to_vec();
    Value::Packed(Rc::new(Packed::new(element, words)))
}

/// A new list owned once with a temporary's items, failing like the interpreter when the
/// temporary isn't a list of the kind of item expected.
pub extern "C" fn from_value(value: i64, element: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let wrong = |value: &Value| {
            Diagnostic::new(
                format!("expected a list here, but this is a {}", value.type_name()),
                span(start, end),
            )
        };
        let value =
            usize::try_from(value).ok().and_then(|at| state.temps.get(at)).cloned().unwrap_or(Value::Unit);
        let (at, element) = self::element(state, element).ok_or_else(|| wrong(&value))?;
        let packed = match &value {
            Value::Packed(packed) if packed.element == element => Rc::clone(packed),
            Value::List(items) => Rc::new(Packed::pack(&element, items).ok_or_else(|| wrong(&value))?),
            _ => return Err(wrong(&value)),
        };
        let block = build(state, at, packed.stride, &packed.words, 0);
        Ok(state.heap.insert(block))
    })
}

/// `a ** b` on ints, failing like the interpreter.
pub extern "C" fn int_power(a: i64, b: i64, start: i64, end: i64) -> i64 {
    attempt(|_: &mut State| {
        match crate::ops::binary(fennec_syntax::BinOp::Pow, Value::Int(a), Value::Int(b), span(start, end))? {
            Value::Int(power) => Ok(power),
            _ => Ok(0),
        }
    })
}

/// `a ** b` on floats.
pub extern "C" fn float_power(a: f64, b: f64) -> f64 {
    a.powf(b)
}

/// Fails with the interpreter's message for an index past either end of a list.
pub extern "C" fn out_of_range(at: i64, len: i64, start: i64, end: i64) {
    attempt(|_: &mut State| -> Result<(), Diagnostic> {
        Err(Diagnostic::new(format!("index {at} is out of range for length {len}"), span(start, end)))
    });
}
