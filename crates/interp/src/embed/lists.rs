//! Lists for native code. A native list is a block in the heap (see `heap`): its owner
//! count, its shape, how many items it has, how many it has room for, then its items packed
//! one after another, a word per number. A string or boxed item is the address of its own
//! block, which the list owns. Native code reads, changes and appends items itself while the
//! list has one owner and room; copying a shared list and growing a full one happen here.
//!
//! A list's shape is counted from the string shape, after the dicts' shapes: one for each
//! kind of item, by the kind's number in the table of elements.

use std::rc::Rc;

use fennec_syntax::Diagnostic;

use super::heap::{TEXT_HEADER_WORDS, read_block, release, text_address, write};
use super::{State, attempt, span, with};
use crate::packed::{Element, Packed};
use crate::value::Value;

/// Words a list's block starts with: owner count, shape, items, room.
pub const LIST_HEADER_WORDS: usize = 4;

/// How `sorted` orders words: as ints or bools, as floats, or as strings.
pub const SORT_INTS: i64 = 0;
pub const SORT_FLOATS: i64 = 1;
pub const SORT_STRINGS: i64 = 2;

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

/// `element_of`, borrowed.
pub(super) fn element_in<'a>(state: &'a State, block: &[u64]) -> Option<&'a Element> {
    let at = block.get(1)?.checked_sub(state.tables.str_shape + 1 + super::dicts::SHAPES)?;
    state.tables.elements.get(usize::try_from(at).ok()?).map(AsRef::as_ref)
}

fn element(state: &State, element: i64) -> Option<(usize, Rc<Element>)> {
    let at = usize::try_from(element).ok()?;
    Some((at, Rc::clone(state.tables.elements.get(at)?)))
}

/// Whether a list of this kind of item owns a block for each.
pub(super) fn owns_items(element: &Element) -> bool {
    matches!(element, Element::Str | Element::Boxed(_) | Element::Maybe(_) | Element::List(_))
}

/// One more owner for each block a list of this kind of item holds, for a new list that
/// holds them too.
fn retain_items(state: &mut State, element: &Element, words: &[u64]) {
    if !owns_items(element) {
        return;
    }
    for word in words {
        if let Some(count) = state.heap.block_mut(*word).and_then(|block| block.first_mut()) {
            *count += 1;
        }
    }
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

/// A new empty list owned once, with room for `room` items, for a loop that knows about how
/// many it will add. A long list's memory isn't touched until it's used.
pub extern "C" fn with_room(element: i64, room: i64) -> i64 {
    with(|state| {
        let Some((at, element)) = self::element(state, element) else { return 0 };
        let room = usize::try_from(room).unwrap_or(0);
        let block = build(state, at, element.words(), &[], room);
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
        if shared {
            state.heap.counts.copied(kept.len());
            retain_items(state, &element, &kept);
        }
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

/// A number word as an int that orders the same way: floats in the order `total_cmp` gives
/// them, which is how the interpreter sorts them, and ints and bools as they are.
fn sort_word(word: u64, kind: i64) -> i64 {
    let int = word.cast_signed();
    if kind == SORT_FLOATS { (word ^ ((int >> 63).cast_unsigned() >> 1)).cast_signed() } else { int }
}

/// The places of `keys` in sorted order. Each key's place breaks ties, which keeps the sort
/// stable.
fn sorted_places(state: &State, keys: &[u64], kind: i64) -> Vec<usize> {
    if kind == SORT_STRINGS {
        // Words read big end first order the same as the bytes in them, and the shorter of
        // two that match so far has zeros left, so its length settles it.
        let text = |key: &u64| {
            let block = state.heap.block(*key).unwrap_or_default();
            let length = block.get(2).copied().unwrap_or(0);
            let words = usize::try_from(length).unwrap_or(0).div_ceil(8);
            (block.get(TEXT_HEADER_WORDS..TEXT_HEADER_WORDS + words).unwrap_or_default(), length)
        };
        let mut order: Vec<(&[u64], u64, usize)> = keys
            .iter()
            .enumerate()
            .map(|(place, key)| {
                let (words, length) = text(key);
                (words, length, place)
            })
            .collect();
        order.sort_unstable_by(|a, b| {
            let bytes =
                a.0.iter().map(|word| word.swap_bytes()).cmp(b.0.iter().map(|word| word.swap_bytes()));
            bytes.then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2))
        });
        return order.into_iter().map(|(_, _, place)| place).collect();
    }
    let mut order: Vec<(i64, usize)> =
        keys.iter().enumerate().map(|(place, key)| (sort_word(*key, kind), place)).collect();
    order.sort_unstable();
    order.into_iter().map(|(_, place)| place).collect()
}

/// A new list owned once of a list's items in the order of `keys`, one for each.
fn in_order(state: &mut State, list: u64, keys: &[u64], kind: i64) -> Option<i64> {
    let block = state.heap.block(list)?;
    let element = element_of(state, block)?;
    let at = usize::try_from(block.get(1)?.checked_sub(state.tables.str_shape + 1 + super::dicts::SHAPES)?)
        .ok()?;
    let stride = element.words();
    let old = items(block, stride).to_vec();
    let mut words = Vec::with_capacity(old.len());
    for place in sorted_places(state, keys, kind) {
        words.extend_from_slice(old.get(place * stride..(place + 1) * stride)?);
    }
    retain_items(state, &element, &words);
    let block = build(state, at, stride, &words, 0);
    Some(state.heap.insert(block))
}

/// A new list owned once of a list of numbers' or strings' items in order.
pub extern "C" fn sorted(list: i64, kind: i64) -> i64 {
    with(|state| {
        let list = u64::try_from(list).unwrap_or(0);
        let keys = state.heap.block(list).map(|block| items(block, 1).to_vec()).unwrap_or_default();
        in_order(state, list, &keys, kind).unwrap_or(0)
    })
}

/// A new list owned once holding these words, one an item, of a kind of item by its number.
pub(super) fn list_of_words(state: &mut State, element: i64, words: &[u64]) -> i64 {
    let Some((at, _)) = self::element(state, element) else { return 0 };
    let block = build(state, at, 1, words, 0);
    state.heap.insert(block)
}

/// A new list owned once of new strings, with the number of the kind of item it has.
pub(super) fn list_of_strings(state: &mut State, element: i64, strings: &[&str]) -> i64 {
    let words: Vec<u64> =
        strings.iter().filter_map(|text| u64::try_from(text_address(state, text)).ok()).collect();
    list_of_words(state, element, &words)
}

/// A new list owned once of a list's items in the order `compare` puts them, which stays stable.
/// `compare` takes the addresses of two items and gives a negative, zero or positive number. It
/// runs with nothing here borrowed, so it can call back in. Once it has failed it's not asked
/// again, and the list comes back in whatever order it had reached.
pub extern "C" fn sorted_with(list: i64, compare: extern "C" fn(i64, i64) -> i64) -> i64 {
    let Some((copy, base, stride, count)) = with(|state| copied_for_sorting(state, list)) else {
        return 0;
    };
    let address = |place: usize| i64::try_from(base + place * stride * 8).unwrap_or(0);
    let mut order: Vec<usize> = (0..count).collect();
    order.sort_by(|a, b| {
        if super::FAILED.get() != 0 {
            return std::cmp::Ordering::Equal;
        }
        compare(address(*a), address(*b)).cmp(&0)
    });
    with(|state| {
        let Some(block) = state.heap.block_mut(copy) else { return };
        let Some(words) = block.get_mut(LIST_HEADER_WORDS..) else { return };
        let old: Vec<u64> = words.to_vec();
        for (to, from) in order.iter().enumerate() {
            let source = old.get(from * stride..(from + 1) * stride).unwrap_or_default();
            if let Some(target) = words.get_mut(to * stride..(to + 1) * stride) {
                target.copy_from_slice(source);
            }
        }
    });
    i64::try_from(copy).unwrap_or(0)
}

/// A new list owned once holding a list's items, with where its items start, how many words
/// each takes, and how many there are.
fn copied_for_sorting(state: &mut State, list: i64) -> Option<(u64, usize, usize, usize)> {
    let list = u64::try_from(list).ok()?;
    let block = state.heap.block(list)?;
    let element = element_of(state, block)?;
    let first = state.tables.str_shape + 1 + super::dicts::SHAPES;
    let at = usize::try_from(block.get(1)?.checked_sub(first)?).ok()?;
    let stride = element.words().max(1);
    let words = items(block, stride).to_vec();
    retain_items(state, &element, &words);
    let block = build(state, at, stride, &words, 0);
    let copy = state.heap.insert(block);
    let copy = u64::try_from(copy).ok()?;
    let start = state.heap.block(copy)?.get(LIST_HEADER_WORDS..)?.as_ptr().addr();
    Some((copy, start, stride, words.len() / stride))
}

/// A new list owned once of a list's items in the order of their keys, a list as long as it.
/// Items with the same key stay in the order they were in.
pub extern "C" fn sorted_by(list: i64, keys: i64, kind: i64) -> i64 {
    with(|state| {
        let keys = u64::try_from(keys).ok().and_then(|keys| state.heap.block(keys));
        let keys = keys.map(|block| items(block, 1).to_vec()).unwrap_or_default();
        in_order(state, u64::try_from(list).unwrap_or(0), &keys, kind).unwrap_or(0)
    })
}

/// A list's block as the interpreter holds it: a packed list.
pub(super) fn read(state: &State, block: &[u64]) -> Value {
    let Some(element) = element_of(state, block) else { return Value::None };
    let words = items(block, element.words()).to_vec();
    if owns_items(&element) {
        return Value::list(words.iter().map(|word| read_block(state, *word)).collect());
    }
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
        let wanted = self::element(state, element).map(|(_, element)| element);
        let fits = |state: &State, block: &[u64]| element_of(state, block) == wanted;
        if let Some(address) = super::heap::lent_block(state, value, fits) {
            return Ok(address);
        }
        let value = state.expanded(state.raw(value));
        let (at, element) = self::element(state, element).ok_or_else(|| wrong(&value))?;
        if owns_items(&element) {
            let Value::List(items) = &value else { return Err(wrong(&value)) };
            let mut words = Vec::with_capacity(items.len());
            for item in items.iter() {
                write(state, &element, item, &mut words).ok_or_else(|| wrong(&value))?;
            }
            let block = build(state, at, 1, &words, 0);
            return Ok(state.heap.insert(block));
        }
        let packed = match &value {
            Value::Packed(packed) if packed.element == element => Rc::clone(packed),
            Value::List(items) => Rc::new(Packed::pack(&element, items).ok_or_else(|| wrong(&value))?),
            _ => return Err(wrong(&value)),
        };
        let block = build(state, at, packed.stride, &packed.words, 0);
        Ok(state.heap.insert(block))
    })
}

/// Empties a list only one owner holds, letting go of what it held. The room left is zero,
/// as native code expects past a list's end.
pub extern "C" fn clear(list: i64) {
    with(|state| {
        let address = u64::try_from(list).unwrap_or(0);
        let Some(block) = state.heap.block(address) else { return };
        let Some(element) = element_of(state, block) else { return };
        let stride = element.words();
        let old = items(block, stride).to_vec();
        if owns_items(&element) {
            for word in &old {
                release(state, *word);
            }
        }
        if let Some(block) = state.heap.block_mut(address) {
            block.get_mut(LIST_HEADER_WORDS..LIST_HEADER_WORDS + old.len()).unwrap_or_default().fill(0);
            if let Some(length) = block.get_mut(2) {
                *length = 0;
            }
        }
    });
}

/// Moves the items after `at`, a place in words past the header, one place down, and gives up
/// the list's owner of the item that was there, if it has one. The list has one owner.
fn close_up(state: &mut State, address: u64, at: usize) -> Option<()> {
    let block = state.heap.block(address)?;
    let element = element_of(state, block)?;
    let stride = element.words();
    let end = LIST_HEADER_WORDS + items(block, stride).len();
    let removed = owns_items(&element).then(|| block.get(at).copied()).flatten();
    let block = state.heap.block_mut(address)?;
    block.get_mut(at..end)?.copy_within(stride.., 0);
    block.get_mut(end - stride..end)?.fill(0);
    *block.get_mut(2)? -= 1;
    if let Some(removed) = removed {
        release(state, removed);
    }
    Some(())
}

/// Takes out the item at an address native code worked out in a list only one owner holds.
pub extern "C" fn remove_at(list: i64, item: i64) {
    with(|state| {
        let at = item.saturating_sub(list) / 8;
        close_up(state, u64::try_from(list).unwrap_or(0), usize::try_from(at).unwrap_or(0));
    });
}

/// Takes over a list's owner and gives back a list only that owner holds with a zeroed item
/// at `at`, for native code to fill, failing like the interpreter for a place that isn't in
/// the list or just past its end.
pub extern "C" fn insert(list: i64, at: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let block = u64::try_from(list).ok().and_then(|list| state.heap.block(list));
        let length = block.and_then(|block| block.get(2)).copied().unwrap_or(0);
        let place = u64::try_from(at).ok().filter(|place| *place <= length);
        let Some(place) = place.and_then(|place| usize::try_from(place).ok()) else {
            let message = format!("can't insert at {at} in a list of length {length}");
            return Err(Diagnostic::new(message, span(start, end)));
        };
        let grown = grow_in(state, list, 1).unwrap_or(list);
        let address = u64::try_from(grown).unwrap_or(0);
        let stride =
            state.heap.block(address).and_then(|block| element_of(state, block)).map_or(1, |e| e.words());
        if let Some(block) = state.heap.block_mut(address) {
            let from = LIST_HEADER_WORDS + place * stride;
            let to = LIST_HEADER_WORDS + (usize::try_from(length).unwrap_or(0) + 1) * stride;
            // What native code passes always has this much room; anything else is left alone.
            let moved = block.get_mut(from..to).filter(|moved| moved.len() >= stride).unwrap_or_default();
            if !moved.is_empty() {
                moved.copy_within(..moved.len() - stride, stride);
                moved.get_mut(..stride).unwrap_or_default().fill(0);
            }
        }
        Ok(grown)
    })
}

/// Whether an item of a word equals a word, as the interpreter compares them.
fn same_item(state: &State, element: &Element, item: u64, word: u64) -> bool {
    match element {
        Element::Float => f64::from_bits(item).total_cmp(&f64::from_bits(word)).is_eq(),
        Element::Str | Element::Boxed(_) | Element::Maybe(_) | Element::List(_) => {
            super::heap::same(state, item, word)
        }
        _ => item == word,
    }
}

/// The place of the first item of a list that equals a word, in words past the header.
fn place_of(state: &State, list: u64, word: u64) -> Option<usize> {
    let block = state.heap.block(list)?;
    let element = element_of(state, block)?;
    let at = items(block, 1).iter().position(|item| same_item(state, &element, *item, word))?;
    Some(LIST_HEADER_WORDS + at)
}

/// `list.remove(word)` on a list of items of a word each, only one owner holds.
pub extern "C" fn remove(list: i64, word: i64, start: i64, end: i64) {
    attempt(|state: &mut State| {
        let (address, word) = (u64::try_from(list).unwrap_or(0), word.cast_unsigned());
        if let Some(at) = place_of(state, address, word) {
            close_up(state, address, at);
            return Ok(());
        }
        let element = state.heap.block(address).and_then(|block| element_of(state, block));
        let value = element.map_or(Value::None, |element| super::heap::read_word(state, &element, word));
        let message = format!("{} isn't in the list", crate::format::repr(&value));
        Err(Diagnostic::new(message, span(start, end)))
    });
}

/// `word in list`, on a list of items of a word each.
pub extern "C" fn contains(list: i64, word: i64) -> i8 {
    with(|state| i8::from(place_of(state, u64::try_from(list).unwrap_or(0), word.cast_unsigned()).is_some()))
}

/// Fails with the interpreter's message for `pop` on an empty list.
pub extern "C" fn pop_empty(start: i64, end: i64) {
    attempt(|_: &mut State| -> Result<(), Diagnostic> {
        Err(Diagnostic::new("can't pop from an empty list", span(start, end)))
    });
}

/// A new list owned once of a list's items from one place to another, where a missing end is
/// `i64::MIN`. Places from the end count back, and places past either end stop there.
pub extern "C" fn slice(list: i64, from: i64, to: i64) -> i64 {
    with(|state| {
        let address = u64::try_from(list).unwrap_or(0);
        let Some(block) = state.heap.block(address) else { return 0 };
        let Some(element) = element_of(state, block) else { return 0 };
        let Some(number) = block
            .get(1)
            .and_then(|shape| shape.checked_sub(state.tables.str_shape + 1 + super::dicts::SHAPES))
        else {
            return 0;
        };
        let stride = element.words();
        let words = items(block, stride);
        let length = i64::try_from(words.len() / stride.max(1)).unwrap_or(0);
        let place = |at: i64, missing: i64| {
            if at == i64::MIN {
                return missing;
            }
            (if at < 0 { at + length } else { at }).clamp(0, length)
        };
        let (begin, stop) = (place(from, 0), place(to, length));
        let offset = |at: i64| usize::try_from(at).unwrap_or(0) * stride;
        let kept = if begin < stop {
            words.get(offset(begin)..offset(stop)).unwrap_or_default().to_vec()
        } else {
            Vec::new()
        };
        retain_items(state, &element, &kept);
        let block = build(state, usize::try_from(number).unwrap_or(0), stride, &kept, 0);
        state.heap.insert(block)
    })
}

/// A new block owned once with a value's items, if it's a list of the kind of item given.
pub(super) fn write_list(state: &mut State, element: &Rc<Element>, value: &Value) -> Option<i64> {
    let at = state.tables.elements.iter().position(|known| known == element).unwrap_or_else(|| {
        state.tables.elements.push(Rc::clone(element));
        state.tables.elements.len() - 1
    });
    let mut words = Vec::new();
    match value {
        Value::Packed(packed) if packed.element == *element => words.extend_from_slice(&packed.words),
        Value::List(items) => {
            for item in items.iter() {
                write(state, element, item, &mut words)?;
            }
        }
        _ => return None,
    }
    let block = build(state, at, element.words(), &words, 0);
    Some(state.heap.insert(block))
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
