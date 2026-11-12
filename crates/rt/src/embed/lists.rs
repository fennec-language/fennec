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

use crate::buffer::{MIN_ROOM, grown_room};

use super::heap::shown::Written;
use super::heap::{TEXT_HEADER_WORDS, read_block, release, shape_word, text_address, write};
use super::{State, attempt, span, with};
use crate::packed::{Element, Packed};
use crate::value::Value;

/// Words a list's block starts with: owner count, shape, items, room.
pub const LIST_HEADER_WORDS: usize = 4;

/// How `sorted` orders words: as ints or bools, as floats, as strings, or as `u64`s.
pub const SORT_INTS: i64 = 0;
pub const SORT_FLOATS: i64 = 1;
pub const SORT_STRINGS: i64 = 2;
pub const SORT_U64S: i64 = 3;

/// The bit of a list's or a dict's owner count a lock's rollback log sets when it marks it, in place
/// of holding a count of it (see `guarded::rollback`). The count then never reads as 1, so native
/// code asks the runtime before every change, and the block lives while the log has it. Well below
/// `heap::IMMORTAL`, which nothing marked comes near.
pub const MARKED: u64 = 1 << 40;

/// Whether the block at `address` is a list or a dict a log can mark: not one already, nor one that
/// lives for the whole run.
pub(super) fn markable_block(state: &State, block: &[u64]) -> bool {
    block.first().is_some_and(|count| count & MARKED == 0 && *count < super::heap::IMMORTAL)
        && (element_number(state, block).is_some_and(|at| at < state.tables.elements.len())
            || super::dicts::is_dict(state, block))
}

/// Marks a list or a dict for a lock's log.
pub(super) fn mark(state: &mut State, address: u64) {
    if let Some(count) = state.heap.block_mut(address).and_then(|block| block.first_mut()) {
        *count |= MARKED;
    }
}

/// Takes a log's mark off a list or a dict. With `owner`, the mark becomes an owner, for the word the
/// log puts it back in; otherwise it goes if nothing else holds it.
pub(super) fn unmark(state: &mut State, address: u64, owner: bool) {
    let Some(count) = state.heap.block_mut(address).and_then(|block| block.first_mut()) else { return };
    // An owner either way, which letting go of again, without `owner`, frees one nothing else holds.
    // Mostly something else does, so that's let go of here, with no second look for the block.
    let left = *count & !MARKED;
    if owner || left == 0 {
        *count = left + 1;
        if !owner {
            release(state, address);
        }
    } else {
        *count = left;
    }
}

/// Whether a list or a dict has the log's mark and, apart from it, one owner: changed in place, a
/// log has it.
pub(super) fn marked_alone(block: &[u64]) -> bool {
    block.first().is_some_and(|count| count & MARKED != 0 && count & !MARKED == 1)
}

/// `more` items' room on the end of a marked list with one owner and room enough, zeroed, the
/// append noted for a crash to undo. `None` for any other list, which is grown as one shared is.
fn marked_append(state: &mut State, list: i64, more: usize) -> Option<i64> {
    let address = u64::try_from(list).ok()?;
    let block = state.heap.block(address)?;
    if !marked_alone(block) {
        return None;
    }
    let stride = element_of(state, block)?.words();
    let count = usize::try_from(*block.get(2)?).ok()?;
    let room = usize::try_from(*block.get(3)?).ok()?;
    let marked = *block.first()?;
    let address = if count + more > room {
        // Moved to a block with room, as a list with one owner is, mark and all, and the log told
        // where it is now.
        let at = element_number(state, block)?;
        let old = state.heap.take_out(address)?;
        let mut grown = build(state, at, stride, items(&old, stride), grown_room(count, more));
        state.heap.recycle(old);
        *grown.first_mut()? = marked;
        let moved = u64::try_from(state.heap.insert(grown)).ok()?;
        super::guarded::rollback::moved(address, moved);
        moved
    } else {
        address
    };
    if !super::guarded::rollback::grew(address, count) {
        return None;
    }
    let block = state.heap.block_mut(address)?;
    let start = LIST_HEADER_WORDS + count * stride;
    block.get_mut(start..start + more * stride)?.fill(0);
    *block.get_mut(2)? = u64::try_from(count + more).ok()?;
    i64::try_from(address).ok()
}

/// `fennec_list_pop_unshare(list)`: the list a `pop` takes its last item off, the same when it has
/// one owner. A marked one stays as it is, and its log holds the item, for a crash to put back; any
/// other shared list is copied.
pub extern "C" fn pop_unshare(list: i64) -> i64 {
    super::made_with("fennec_list_pop_unshare", |state| {
        marked_pop(state, list).or_else(|| grow_in(state, list, 0)).unwrap_or(list)
    })
}

fn marked_pop(state: &mut State, list: i64) -> Option<i64> {
    let address = u64::try_from(list).ok()?;
    let block = state.heap.block(address)?;
    if !marked_alone(block) {
        return None;
    }
    let element = element_of(state, block)?;
    let stride = element.words();
    let count = usize::try_from(*block.get(2)?).ok()?;
    // An empty one has nothing to take, and the pop fails.
    let Some(at) = count.checked_sub(1) else { return Some(list) };
    let start = LIST_HEADER_WORDS + at * stride;
    let words = block.get(start..start + stride)?.to_vec();
    retain_items(state, &element, &words);
    if !super::guarded::rollback::popped(address, at, words.clone()) {
        release_items(state, &element, &words);
        return None;
    }
    Some(list)
}

/// `fennec_list_item_unshare(list, at)`: the list an item set at `at` changes, the same when it has
/// one owner. A marked one stays as it is, and its log holds the item as it was, for a crash to put
/// back; any other shared list is copied. `at` counts from the end when it's below 0, as an index
/// does.
pub extern "C" fn item_unshare(list: i64, at: i64) -> i64 {
    with(|state| marked_set(state, list, at).or_else(|| grow_in(state, list, 0)).unwrap_or(list))
}

fn marked_set(state: &mut State, list: i64, at: i64) -> Option<i64> {
    let address = u64::try_from(list).ok()?;
    let block = state.heap.block(address)?;
    if !marked_alone(block) {
        return None;
    }
    let element = element_of(state, block)?;
    let stride = element.words();
    let count = i64::try_from(*block.get(2)?).ok()?;
    // An index outside the list changes nothing: the set fails, as it would anyway.
    let Ok(at) = usize::try_from(if at < 0 { at + count } else { at }) else { return Some(list) };
    if at >= usize::try_from(count).ok()? {
        return Some(list);
    }
    let start = LIST_HEADER_WORDS + at * stride;
    let words = block.get(start..start + stride)?.to_vec();
    retain_items(state, &element, &words);
    if !super::guarded::rollback::set(address, at, words.clone()) {
        release_items(state, &element, &words);
        return None;
    }
    Some(list)
}

/// After a crash, a marked list's item at `at` as it was before it was set, with the owners the log
/// held: what's there now is let go of.
pub(super) fn set_back(state: &mut State, address: u64, at: usize, words: &[u64]) {
    let Some(block) = state.heap.block(address) else { return };
    let Some(element) = element_of(state, block) else { return };
    let start = LIST_HEADER_WORDS + at * words.len();
    let Some(now) = block.get(start..start + words.len()).map(<[u64]>::to_vec) else { return };
    if let Some(slots) =
        state.heap.block_mut(address).and_then(|block| block.get_mut(start..start + words.len()))
    {
        slots.copy_from_slice(words);
    }
    release_items(state, &element, &now);
}

/// After a crash, a marked list as long as it was before an append: what went on since is let go of.
pub(super) fn shorten_to(state: &mut State, address: u64, length: usize) {
    let Some(block) = state.heap.block(address) else { return };
    let Some(element) = element_of(state, block) else { return };
    let stride = element.words();
    let dropped = items(block, stride).get(length * stride..).unwrap_or_default().to_vec();
    if dropped.is_empty() && items(block, stride).len() == length * stride {
        return;
    }
    if let Some(block) = state.heap.block_mut(address) {
        let start = LIST_HEADER_WORDS + length * stride;
        block.get_mut(start..start + dropped.len()).unwrap_or_default().fill(0);
        if let Some(count) = block.get_mut(2) {
            *count = u64::try_from(length).unwrap_or(0);
        }
    }
    release_items(state, &element, &dropped);
}

/// After a crash, a marked list's popped item back at `at`, its last, with the owners the log held:
/// what went on after it since is let go of.
pub(super) fn put_back(state: &mut State, address: u64, at: usize, words: &[u64]) {
    shorten_to(state, address, at);
    let Some(block) = state.heap.block_mut(address) else { return };
    let start = LIST_HEADER_WORDS + at * words.len();
    if let Some(slots) = block.get_mut(start..start + words.len()) {
        slots.copy_from_slice(words);
        if let Some(count) = block.get_mut(2) {
            *count = u64::try_from(at + 1).unwrap_or(0);
        }
    }
}

/// Lets go of the owners a log held of what a popped item's `words` hold, as items of `list`'s kind.
pub(super) fn let_go_of_words(state: &mut State, list: u64, words: &[u64]) {
    let Some(element) = state.heap.block(list).and_then(|block| element_of(state, block)) else { return };
    release_items(state, &element, words);
}

/// The shape of lists of the element numbered `element`.
fn shape(state: &State, element: usize) -> u64 {
    state.tables.str_shape + 1 + super::dicts::SHAPES + u64::try_from(element).unwrap_or(0)
}

/// The kind of item a block holds, when it's a list's.
pub(super) fn element_of(state: &State, block: &[u64]) -> Option<Rc<Element>> {
    let at = shape_word(block)?.checked_sub(state.tables.str_shape + 1 + super::dicts::SHAPES)?;
    state.tables.elements.get(usize::try_from(at).ok()?).cloned()
}

/// `element_of`, borrowed.
pub(super) fn element_in<'a>(state: &'a State, block: &[u64]) -> Option<&'a Element> {
    state.tables.elements.get(element_number(state, block)?).map(AsRef::as_ref)
}

/// The number of the kind of item a list's block holds.
pub(super) fn element_number(state: &State, block: &[u64]) -> Option<usize> {
    let at = shape_word(block)?.checked_sub(first_list_shape(state))?;
    usize::try_from(at).ok()
}

/// The shape of a list of the first kind of item: lists' shapes follow strings' and dicts'.
pub(super) fn first_list_shape(state: &State) -> u64 {
    state.tables.str_shape + 1 + super::dicts::SHAPES
}

/// The number of a kind of item and how many words one takes, worked out once by the heap where
/// it can.
fn item_words(state: &State, element: i64) -> Option<(usize, usize)> {
    let at = usize::try_from(element).ok()?;
    let words = match state.heap.item_words(at) {
        Some(words) => words,
        None => state.tables.elements.get(at)?.words(),
    };
    Some((at, words))
}

fn element(state: &State, element: i64) -> Option<(usize, Rc<Element>)> {
    let at = usize::try_from(element).ok()?;
    Some((at, Rc::clone(state.tables.elements.get(at)?)))
}

/// Whether a list of this kind of item owns a block for each.
pub(super) fn owns_items(element: &Element) -> bool {
    matches!(
        element,
        Element::Str | Element::Boxed(_) | Element::Maybe(_) | Element::List(_) | Element::ListOf(_)
    ) && !element.is_optional_text()
}

/// The blocks items of a kind hold, laid out as `words`: an item of a word that's a block's
/// address, or the blocks an item laid out in place holds, like a struct's string field.
fn held_blocks(element: &Element, words: &[u64]) -> Vec<u64> {
    if owns_items(element) {
        return words.iter().copied().filter(|address| *address != 0).collect();
    }
    let mut found = Vec::new();
    if super::heap::holds_blocks(element) {
        for item in words.chunks(element.words().max(1)) {
            super::heap::held_addresses(element, item, &mut found);
        }
    }
    found
}

/// One more owner for each block a list of this kind of item holds, for a new list that
/// holds them too.
fn retain_items(state: &mut State, element: &Element, words: &[u64]) {
    for address in held_blocks(element, words) {
        super::heap::retain(state, address);
    }
}

/// Gives up the owner a list held of each block its items, laid out as `words`, hold.
fn release_items(state: &mut State, element: &Element, words: &[u64]) {
    for address in held_blocks(element, words) {
        release(state, address);
    }
}

/// A block owned once holding `items` words of items, with room for at least `room`.
fn build(state: &mut State, at: usize, stride: usize, items: &[u64], room: usize) -> Box<[u64]> {
    let count = items.len() / stride.max(1);
    let room = room.max(count).max(MIN_ROOM);
    // The header and the items are written here, and only the room after them is zeroed.
    let mut block = state.heap.fresh_written(
        super::heap::words_for(LIST_HEADER_WORDS, room, stride),
        LIST_HEADER_WORDS + items.len(),
    );
    let number = |n: usize| u64::try_from(n).unwrap_or(0);
    // Written a word at a time: a few words copied as a slice went through a call to copy them.
    if let Some([owners, kind, length, room_word]) = block.get_mut(..LIST_HEADER_WORDS) {
        (*owners, *kind, *length, *room_word) = (1, shape(state, at), number(count), number(room));
    }
    if !items.is_empty()
        && let Some(words) = block.get_mut(LIST_HEADER_WORDS..LIST_HEADER_WORDS + items.len())
    {
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
    super::made_with("fennec_list_new", |state| {
        let Some((at, stride)) = item_words(state, element) else { return 0 };
        let count = usize::try_from(count).unwrap_or(0);
        // A fresh block is already zero, so only the header is written.
        let mut block = build(state, at, stride, &[], count);
        if let Some(length) = block.get_mut(2) {
            *length = u64::try_from(count).unwrap_or(0);
        }
        state.heap.insert(block)
    })
}

/// A new empty list owned once, with room for `room` items, for a loop that knows about how
/// many it will add. A long list's memory isn't touched until it's used.
pub extern "C" fn with_room(element: i64, room: i64) -> i64 {
    super::made_with("fennec_list_with_room", |state| {
        let Some((at, stride)) = item_words(state, element) else { return 0 };
        let room = usize::try_from(room).unwrap_or(0);
        let block = build(state, at, stride, &[], room);
        state.heap.insert(block)
    })
}

/// Takes over a list's owner and gives back one with a zeroed item more on the end, for
/// native code to fill: the same block when it had one owner and room, or else a copy
/// with room to double.
pub extern "C" fn grow(list: i64) -> i64 {
    super::made_with("fennec_list_grow", |state| {
        marked_append(state, list, 1).or_else(|| grow_in(state, list, 1)).unwrap_or(list)
    })
}

/// Takes over a list's owner and gives back a block only that owner holds, copying the
/// list if anything else shares it.
pub extern "C" fn unshare(list: i64) -> i64 {
    super::made_with("fennec_list_unshare", |state| grow_in(state, list, 0).unwrap_or(list))
}

fn grow_in(state: &mut State, list: i64, more: usize) -> Option<i64> {
    grow_to(state, list, more, 0)
}

/// `grow_in`, with room for at least `least` items afterwards.
fn grow_to(state: &mut State, list: i64, more: usize, least: usize) -> Option<i64> {
    let address = u64::try_from(list).ok()?;
    let block = state.heap.block(address)?;
    let at =
        usize::try_from(shape_word(block)?.checked_sub(state.tables.str_shape + 1 + super::dicts::SHAPES)?)
            .ok()?;
    let (_, stride) = item_words(state, i64::try_from(at).ok()?)?;
    let count = usize::try_from(*block.get(2)?).ok()?;
    let room = usize::try_from(*block.get(3)?).ok()?;
    let shared = !state.heap.is_alone(address, block);
    let wanted = if count + more > room { grown_room(count, more) } else { room }.max(least);
    let address = if shared {
        let element = element_of(state, block)?;
        let kept = items(block, stride).to_vec();
        state.heap.copied(kept.len());
        retain_items(state, &element, &kept);
        let grown = build(state, at, stride, &kept, wanted);
        release(state, address);
        u64::try_from(state.heap.insert(grown)).ok()?
    } else if wanted > room {
        // Its own block: taken out first, its items move straight across with no copy on the way.
        let old = state.heap.take_out(address)?;
        let grown = build(state, at, stride, items(&old, stride), wanted);
        state.heap.recycle(old);
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

/// A number word as an int that orders the same way: floats in the order floats have, with those
/// that are equal as one, a `u64` with its top bit turned over, and ints and bools as they are.
fn sort_word(word: u64, kind: i64) -> i64 {
    if kind == SORT_U64S {
        return (word ^ (1 << 63)).cast_signed();
    }
    if kind != SORT_FLOATS {
        return word.cast_signed();
    }
    let key = crate::value::float_key(f64::from_bits(word));
    (key ^ ((key.cast_signed() >> 63).cast_unsigned() >> 1)).cast_signed()
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
        let order: Vec<(&[u64], u64, usize)> = keys
            .iter()
            .enumerate()
            .map(|(place, key)| {
                let (words, length) = text(key);
                (words, length, place)
            })
            .collect();
        return text_places(order);
    }
    // Each key and its place packed in one number that orders the same way, so sorting
    // compares one number instead of a pair.
    let packed = |place: usize, key: &u64| {
        let key = sort_word(*key, kind).cast_unsigned() ^ (1 << 63);
        u128::from(key) << 64 | u128::from(u64::try_from(place).unwrap_or(u64::MAX))
    };
    let mut order: Vec<u128> = keys.iter().enumerate().map(|(place, key)| packed(place, key)).collect();
    order.sort_unstable();
    order.into_iter().map(|both| usize::try_from(both & u128::from(u64::MAX)).unwrap_or(0)).collect()
}

/// A new list owned once of a list's items in the order of `keys`, one for each.
fn in_order(state: &mut State, list: u64, keys: &[u64], kind: i64) -> Option<i64> {
    let places = sorted_places(state, keys, kind);
    in_places(state, list, &places)
}

/// A new list owned once of a list's items, taken from these places in turn.
fn in_places(state: &mut State, list: u64, places: &[usize]) -> Option<i64> {
    let block = state.heap.block(list)?;
    let element = element_of(state, block)?;
    let at =
        usize::try_from(shape_word(block)?.checked_sub(state.tables.str_shape + 1 + super::dicts::SHAPES)?)
            .ok()?;
    let stride = element.words();
    let old = items(block, stride).to_vec();
    let mut words = Vec::with_capacity(old.len());
    for place in places {
        words.extend_from_slice(old.get(place * stride..(place + 1) * stride)?);
    }
    retain_items(state, &element, &words);
    let block = build(state, at, stride, &words, 0);
    Some(state.heap.insert(block))
}

/// A new list owned once of a list of ints, bools or `u64`s in order, sorted as words. Ints that sort
/// the same are the same int, so this needn't be stable, and nothing is gathered. Floats
/// aren't sorted this way: floats that are equal can differ, like `0.0` and `-0.0`, so only
/// a stable sort keeps their order from depending on how they're sorted.
fn sorted_ints(state: &mut State, list: u64, kind: i64) -> Option<i64> {
    let block = state.heap.block(list)?;
    let element = element_in(state, block)?;
    if element.words() != 1 || owns_items(element) {
        return None;
    }
    let at =
        usize::try_from(shape_word(block)?.checked_sub(state.tables.str_shape + 1 + super::dicts::SHAPES)?)
            .ok()?;
    let numbers = items(block, 1).to_vec();
    let mut block = build(state, at, 1, &numbers, 0);
    block
        .get_mut(LIST_HEADER_WORDS..LIST_HEADER_WORDS + numbers.len())?
        .sort_unstable_by_key(|word| sort_word(*word, kind));
    Some(state.heap.insert(block))
}

/// The places of texts, each its words, which are zero after its end, its length and its
/// place, in sorted order. Words read big end first order the same as the bytes in them, and
/// the shorter of two that match so far has zeros left, so its length settles it; the place
/// breaks ties, which keeps the sort stable.
fn text_places(mut order: Vec<(&[u64], u64, usize)>) -> Vec<usize> {
    order.sort_unstable_by(|a, b| {
        let bytes = a.0.iter().map(|word| word.swap_bytes()).cmp(b.0.iter().map(|word| word.swap_bytes()));
        bytes.then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2))
    });
    order.into_iter().map(|(_, _, place)| place).collect()
}

/// A new list owned once of a list of numbers' or strings' items in order.
pub extern "C" fn sorted(list: i64, kind: i64) -> i64 {
    super::made_with("fennec_list_sorted", |state| {
        let list = u64::try_from(list).unwrap_or(0);
        if (kind == SORT_INTS || kind == SORT_U64S)
            && let Some(sorted) = sorted_ints(state, list, kind)
        {
            return sorted;
        }
        if let Some(sorted) = sorted_texts(state, list) {
            return sorted;
        }
        let keys = state.heap.block(list).map(|block| items(block, 1).to_vec()).unwrap_or_default();
        in_order(state, list, &keys, kind).unwrap_or(0)
    })
}

/// A new list owned once of a list of strings' headers in order, kept stable, when it is one.
fn sorted_texts(state: &mut State, list: u64) -> Option<i64> {
    let block = state.heap.block(list)?;
    if element_in(state, block) != Some(&Element::Text) {
        return None;
    }
    let element = element_of(state, block)?;
    let at =
        usize::try_from(shape_word(block)?.checked_sub(state.tables.str_shape + 1 + super::dicts::SHAPES)?)
            .ok()?;
    let headers = items(block, 3).to_vec();
    // An inline string's words with byte 23 cleared are zero after its end, as a block's are.
    let cleared: Vec<[u64; 3]> =
        headers.as_chunks::<3>().0.iter().map(|&[a, b, c]| [a, b, c & (u64::MAX >> 8)]).collect();
    let texts =
        headers.as_chunks::<3>().0.iter().zip(&cleared).enumerate().map(|(place, (header, inline))| {
            let [.., tag_word] = *header;
            let (words, length): (&[u64], u64) = if let Some(address) = super::text::block_of(header) {
                let block = state.heap.block(address).unwrap_or_default();
                (block.get(TEXT_HEADER_WORDS..).unwrap_or_default(), block.get(2).copied().unwrap_or(0))
            } else {
                (inline, (tag_word >> 56) & 0x7f)
            };
            let used = usize::try_from(length).unwrap_or(0).div_ceil(8);
            (words.get(..used).unwrap_or_default(), length, place)
        });
    let order = text_places(texts.collect());
    let words: Vec<u64> = order
        .iter()
        .flat_map(|place| headers.get(place * 3..place * 3 + 3).unwrap_or_default())
        .copied()
        .collect();
    retain_items(state, &element, &words);
    let block = build(state, at, 3, &words, 0);
    Some(state.heap.insert(block))
}

/// A new list owned once holding these words, one an item, of a kind of item by its number.
pub(super) fn list_of_words(state: &mut State, element: i64, words: &[u64]) -> i64 {
    let Some((at, _)) = self::element(state, element) else { return 0 };
    let block = build(state, at, 1, words, 0);
    state.heap.insert(block)
}

/// A new list owned once of new strings, with the number of the kind of item it has.
pub(super) fn list_of_strings(state: &mut State, element: i64, strings: &[&str]) -> i64 {
    if self::element(state, element).is_some_and(|(_, element)| *element == Element::Text) {
        let words: Vec<u64> = strings.iter().flat_map(|text| super::text::header(state, text)).collect();
        let Some((at, _)) = self::element(state, element) else { return 0 };
        let block = build(state, at, 3, &words, 0);
        return state.heap.insert(block);
    }
    let words: Vec<u64> =
        strings.iter().filter_map(|text| u64::try_from(text_address(state, text)).ok()).collect();
    list_of_words(state, element, &words)
}

/// The places `0..count` in the order `compare` puts them, keeping equal ones in order. A program's
/// comparison can disagree with itself, or stop answering when it fails, so this mustn't rely on it
/// being a total order.
fn sorted_order(count: usize, compare: impl FnMut(usize, usize) -> std::cmp::Ordering) -> Vec<usize> {
    let mut compare = compare;
    let mut order: Vec<usize> = (0..count).collect();
    let mut merged = Vec::with_capacity(count);
    // Bottom-up merges of runs twice as long each time: each step only asks which of two goes
    // first, so whatever the answers, every place comes out once.
    let mut width = 1;
    while width < count {
        merged.clear();
        for start in (0..count).step_by(2 * width) {
            let middle = (start + width).min(count);
            let end = (start + 2 * width).min(count);
            let (mut left, mut right) = (start, middle);
            while left < middle && right < end {
                let (Some(&a), Some(&b)) = (order.get(left), order.get(right)) else { break };
                if compare(b, a) == std::cmp::Ordering::Less {
                    merged.push(b);
                    right += 1;
                } else {
                    merged.push(a);
                    left += 1;
                }
            }
            merged.extend(order.get(left..middle).unwrap_or_default());
            merged.extend(order.get(right..end).unwrap_or_default());
        }
        std::mem::swap(&mut order, &mut merged);
        width *= 2;
    }
    order
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
    let order = sorted_order(count, |a, b| {
        if super::failed() {
            return std::cmp::Ordering::Equal;
        }
        compare(address(a), address(b)).cmp(&0)
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
    let at = usize::try_from(shape_word(block)?.checked_sub(first)?).ok()?;
    let stride = element.words().max(1);
    let words = items(block, stride).to_vec();
    retain_items(state, &element, &words);
    let block = build(state, at, stride, &words, 0);
    let copy = state.heap.insert(block);
    let copy = u64::try_from(copy).ok()?;
    let start = state.heap.block(copy)?.get(LIST_HEADER_WORDS..)?.as_ptr().addr();
    Some((copy, start, stride, words.len() / stride))
}

/// A new list owned once of a list's items in the order of their keys, a list as long as it, by
/// a function comparing two keys by their addresses, as when keys are tuples.
pub extern "C" fn sorted_by_keys_with(list: i64, keys: i64, compare: extern "C" fn(i64, i64) -> i64) -> i64 {
    let Some((copy, base, stride, count)) = with(|state| copied_for_sorting(state, keys)) else {
        return 0;
    };
    let address = |place: usize| i64::try_from(base + place * stride * 8).unwrap_or(0);
    let mut places: Vec<usize> = (0..count).collect();
    places.sort_by(|a, b| {
        if super::failed() {
            return std::cmp::Ordering::Equal;
        }
        compare(address(*a), address(*b)).cmp(&0)
    });
    with(|state| {
        release(state, copy);
        in_places(state, u64::try_from(list).unwrap_or(0), &places).unwrap_or(0)
    })
}

/// A new list owned once of a list's items in the order of their keys, a list as long as it.
/// Items with the same key stay in the order they were in.
pub extern "C" fn sorted_by(list: i64, keys: i64, kind: i64) -> i64 {
    super::made_with("fennec_list_sorted_by", |state| {
        let list = u64::try_from(list).unwrap_or(0);
        let keys = u64::try_from(keys).ok().and_then(|keys| state.heap.block(keys)).unwrap_or_default();
        // Strings' keys held as headers, three words each, ordered by their text.
        if element_in(state, keys) == Some(&Element::Text) {
            let texts = super::text::texts(state, items(keys, 3));
            let mut places: Vec<usize> = (0..texts.len()).collect();
            places.sort_by(|a, b| texts.get(*a).cmp(&texts.get(*b)));
            return in_places(state, list, &places).unwrap_or(0);
        }
        let keys = items(keys, 1).to_vec();
        in_order(state, list, &keys, kind).unwrap_or(0)
    })
}

/// A list's block as a `Value`: a packed list.
pub(super) fn read(state: &State, block: &[u64]) -> Value {
    let Some(element) = element_of(state, block) else { return Value::None };
    let words = items(block, element.words()).to_vec();
    if owns_items(&element) {
        return Value::list(words.iter().map(|word| read_block(state, *word)).collect());
    }
    // An item with no fields takes no words, so it's the list's length that counts them.
    if !super::heap::packable(&element) || element.words() == 0 {
        let count = block.get(2).and_then(|n| usize::try_from(*n).ok()).unwrap_or(0);
        return super::heap::read_items(state, &element, &words, count);
    }
    Value::Packed(Rc::new(Packed::new(element, words)))
}

/// Writes a list as it shows.
pub(super) fn shown(state: &State, block: &[u64], out: &mut Written) {
    out.push('[');
    if let Some(element) = element_of(state, block) {
        let words = items(block, element.words());
        // An item with no fields takes no words, so it's the list's length that counts them.
        let count = block.get(2).and_then(|n| usize::try_from(*n).ok()).unwrap_or(0);
        super::heap::shown::items(state, &element, &mut words.iter(), count, out);
    }
    out.push(']');
}

/// Writes the items of a `Table` whose slots are still in use: those whose generation in the
/// list `generations` isn't negative.
pub(super) fn shown_where_used(state: &State, held: u64, generations: u64, out: &mut Written) {
    let (Some(block), Some(numbers)) = (state.heap.block(held), state.heap.block(generations)) else {
        return;
    };
    let Some(element) = element_of(state, block) else { return };
    let mut words = items(block, element.words()).iter();
    let mut first = true;
    for generation in items(numbers, 1) {
        let in_use = i64::from_ne_bytes(generation.to_ne_bytes()) >= 0;
        if in_use && !first {
            out.push_str(", ");
        }
        if in_use {
            super::heap::shown::item(state, &element, &mut words, out);
            first = false;
        } else {
            (&mut words).take(element.words()).for_each(drop);
        }
    }
}

/// A new list owned once with a temporary's items. Fails when the temporary isn't a list of the
/// kind of item expected.
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
        // Items that are or hold blocks are written by the runtime, which makes those blocks.
        if owns_items(&element) || !super::heap::packable(&element) {
            let Value::List(items) = value.clone().unpacked() else { return Err(wrong(&value)) };
            let mut words = Vec::with_capacity(items.len() * element.words());
            for item in items.iter() {
                write(state, &element, item, &mut words).ok_or_else(|| wrong(&value))?;
            }
            let block = build(state, at, element.words(), &words, 0);
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

/// Empties a list only one owner holds, letting go of what it held, and zeroes the room it held
/// it in.
pub extern "C" fn clear(list: i64) {
    with(|state| {
        let address = u64::try_from(list).unwrap_or(0);
        let Some(block) = state.heap.block(address) else { return };
        let Some(element) = element_of(state, block) else { return };
        let stride = element.words();
        let old = items(block, stride).to_vec();
        release_items(state, &element, &old);
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
    let removed = block.get(at..at + stride)?.to_vec();
    let block = state.heap.block_mut(address)?;
    block.get_mut(at..end)?.copy_within(stride.., 0);
    block.get_mut(end - stride..end)?.fill(0);
    *block.get_mut(2)? -= 1;
    release_items(state, &element, &removed);
    Some(())
}

/// Takes out the item at an address native code worked out in a list only one owner holds.
pub extern "C" fn remove_at(list: i64, item: i64) {
    with(|state| {
        let at = item.saturating_sub(list) / 8;
        close_up(state, u64::try_from(list).unwrap_or(0), usize::try_from(at).unwrap_or(0));
    });
}

/// Takes over a list's owner and gives back a list only that owner holds with a zeroed item at
/// `at`, for native code to fill. Fails for a place that isn't in the list or just past its end.
pub extern "C" fn insert(list: i64, at: i64, start: i64, end: i64) -> i64 {
    insert_into("list", list, at, (start, end))
}

/// `insert`, for a `Buffer`.
pub extern "C" fn insert_buffer(list: i64, at: i64, start: i64, end: i64) -> i64 {
    insert_into("Buffer", list, at, (start, end))
}

/// `insert`, failing with a message that names what's inserted into.
fn insert_into(kind: &str, list: i64, at: i64, (start, end): (i64, i64)) -> i64 {
    attempt(|state: &mut State| {
        let block = u64::try_from(list).ok().and_then(|list| state.heap.block(list));
        let length = block.and_then(|block| block.get(2)).copied().unwrap_or(0);
        let place = u64::try_from(at).ok().filter(|place| *place <= length);
        let Some(place) = place.and_then(|place| usize::try_from(place).ok()) else {
            let message = format!("can't insert at {at} in a {kind} of length {length}");
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

/// Whether an item of a word equals a word.
fn same_item(state: &State, element: &Element, item: u64, word: u64) -> bool {
    match element {
        Element::Float | Element::F32 => {
            crate::value::floats_same(f64::from_bits(item), f64::from_bits(word))
        }
        Element::Str | Element::Boxed(_) | Element::Maybe(_) | Element::List(_) | Element::ListOf(_) => {
            super::heap::same(state, item, word)
        }
        _ => item == word,
    }
}

/// Whether `in` and `remove` are handed an item of this kind as the address of a block of its
/// shape holding it, rather than as the item's one word: a struct or enum laid out in place
/// that takes more than a word or holds blocks.
fn passed_as_block(element: &Element) -> bool {
    matches!(element, Element::Struct { .. } | Element::Enum { .. })
        && (element.words() > 1 || super::heap::holds_blocks(element))
}

/// The place of the first item of a list that equals a word, or the item in the block at
/// `word` for items passed as blocks, in words past the header.
fn place_of(state: &State, list: u64, word: u64) -> Option<usize> {
    let block = state.heap.block(list)?;
    let element = element_of(state, block)?;
    if *element == Element::Text {
        // A string to find comes as its block, and items are compared with it where they are.
        let at = items(block, 3)
            .as_chunks::<3>()
            .0
            .iter()
            .position(|header| super::text::header_is(state, header, word))?;
        return Some(LIST_HEADER_WORDS + at * 3);
    }
    if element.is_optional_text() {
        // As for strings, with 0 for `None`, which only an all-zero header equals.
        let wanted = (word != 0).then(|| super::text::text_of(state, &[word, 0, 0]));
        let at = items(block, 3).as_chunks::<3>().0.iter().position(|header| match (&wanted, header) {
            (None, [0, 0, 0]) => true,
            (Some(wanted), header) if *header != [0, 0, 0] => super::text::text_of(state, header) == *wanted,
            _ => false,
        })?;
        return Some(LIST_HEADER_WORDS + at * 3);
    }
    if passed_as_block(&element) {
        let stride = element.words().max(1);
        let wanted = state.heap.block(word)?.get(super::heap::HEADER_WORDS..)?;
        let at = items(block, stride)
            .chunks(stride)
            .position(|item| super::heap::same_words(state, &element, item, wanted))?;
        return Some(LIST_HEADER_WORDS + at * stride);
    }
    let at = items(block, 1).iter().position(|item| same_item(state, &element, *item, word))?;
    Some(LIST_HEADER_WORDS + at)
}

/// `list.remove(word)` on a list only one owner holds, of items of a word each, or of items
/// passed as blocks (see `passed_as_block`).
pub extern "C" fn remove(list: i64, word: i64, start: i64, end: i64) {
    attempt(|state: &mut State| {
        let (address, word) = (u64::try_from(list).unwrap_or(0), word.cast_unsigned());
        if let Some(at) = place_of(state, address, word) {
            close_up(state, address, at);
            return Ok(());
        }
        let element = state.heap.block(address).and_then(|block| element_of(state, block));
        let value = match element {
            Some(element) if passed_as_block(&element) => read_block(state, word),
            Some(element) => super::heap::read_word(state, &element, word),
            None => Value::None,
        };
        let message = format!("{} isn't in the list", crate::format::repr(&value));
        Err(Diagnostic::new(message, span(start, end)))
    });
}

/// `word in list`, on a list of items of a word each, or of items passed as blocks.
pub extern "C" fn contains(list: i64, word: i64) -> i8 {
    with(|state| i8::from(place_of(state, u64::try_from(list).unwrap_or(0), word.cast_unsigned()).is_some()))
}

/// `buffer.reserve(room)`: takes over a list's owner and gives back one only that owner holds,
/// with room for at least `room` items, the same block when it had one owner and the room.
pub extern "C" fn reserve(list: i64, room: i64) -> i64 {
    super::made_with("fennec_list_reserve", |state| {
        grow_to(state, list, 0, usize::try_from(room).unwrap_or(0)).unwrap_or(list)
    })
}

/// `buffer.truncate(length)`: takes over a list's owner and gives back one only that owner
/// holds with no more than `length` items, letting go of the rest. Its room stays.
pub extern "C" fn truncate(list: i64, length: i64) -> i64 {
    super::made_with("fennec_list_truncate", |state| {
        let address = grow_in(state, list, 0).unwrap_or(list);
        let at = u64::try_from(address).unwrap_or(0);
        let kept = usize::try_from(length).unwrap_or(0);
        let Some(block) = state.heap.block(at) else { return address };
        let Some(element) = element_of(state, block) else { return address };
        let stride = element.words().max(1);
        let count = items(block, stride).len() / stride;
        if kept >= count {
            return address;
        }
        let dropped = items(block, stride).get(kept * stride..).unwrap_or_default().to_vec();
        if let Some(block) = state.heap.block_mut(at) {
            let start = LIST_HEADER_WORDS + kept * stride;
            block.get_mut(start..start + dropped.len()).unwrap_or_default().fill(0);
            if let Some(length) = block.get_mut(2) {
                *length = u64::try_from(kept).unwrap_or(0);
            }
        }
        release_items(state, &element, &dropped);
        address
    })
}

/// `buffer.extend(other)`: takes over a list's owner and gives back one only that owner holds
/// with `other`'s items after its own, the same kind of item, each held once more.
pub extern "C" fn extend(list: i64, other: i64) -> i64 {
    super::made_with("fennec_list_extend", |state| {
        let Some(more) = u64::try_from(other).ok().and_then(|other| state.heap.block(other)) else {
            return list;
        };
        let Some(element) = element_of(state, more) else { return list };
        let stride = element.words().max(1);
        let added = items(more, stride).to_vec();
        let count =
            state.heap.block(u64::try_from(list).unwrap_or(0)).map_or(0, |block| items(block, stride).len());
        let more = added.len() / stride;
        let grown = marked_append(state, list, more).or_else(|| grow_in(state, list, more)).unwrap_or(list);
        if let Some(block) = u64::try_from(grown).ok().and_then(|at| state.heap.block_mut(at))
            && let Some(slots) =
                block.get_mut(LIST_HEADER_WORDS + count..LIST_HEADER_WORDS + count + added.len())
        {
            slots.copy_from_slice(&added);
        }
        retain_items(state, &element, &added);
        grown
    })
}

/// Fails with the language's message for `pop` on an empty `Buffer`.
pub extern "C" fn pop_empty_buffer(start: i64, end: i64) {
    attempt(|_: &mut State| -> Result<(), Diagnostic> {
        Err(Diagnostic::new("can't pop from an empty Buffer", span(start, end)))
    });
}

/// Fails with the language's message for `pop` on an empty list.
pub extern "C" fn pop_empty(start: i64, end: i64) {
    attempt(|_: &mut State| -> Result<(), Diagnostic> {
        Err(Diagnostic::new("can't pop from an empty list", span(start, end)))
    });
}

/// A new list owned once of a list's items from one place to another, where a missing end is
/// `i64::MIN`. Places from the end count back, and places past either end stop there.
pub extern "C" fn slice(list: i64, from: i64, to: i64) -> i64 {
    super::made_with("fennec_list_slice", |state| {
        let address = u64::try_from(list).unwrap_or(0);
        let Some(block) = state.heap.block(address) else { return 0 };
        let Some(element) = element_of(state, block) else { return 0 };
        let Some(number) = shape_word(block)
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

/// A new list block owned once of items already laid out as words, of a kind of item.
pub(super) fn list_of_items(state: &mut State, element: &Rc<Element>, words: &[u64]) -> i64 {
    let at = state.tables.elements.iter().position(|known| known == element).unwrap_or_else(|| {
        state.tables.elements.push(Rc::clone(element));
        state.tables.elements.len() - 1
    });
    let block = build(state, at, element.words(), words, 0);
    state.heap.insert(block)
}

/// `a ** b` on ints. Fails for a negative power or an overflow.
pub extern "C" fn int_power(a: i64, b: i64, start: i64, end: i64) -> i64 {
    attempt(|_: &mut State| {
        match crate::ops::binary(fennec_syntax::BinOp::Pow, Value::Int(a), Value::Int(b), span(start, end))? {
            Value::Int(power) => Ok(power),
            _ => Ok(0),
        }
    })
}

/// `a ** b` on floats, by `libm` as every float function is, so it's the same on every platform.
/// The powers a program takes most, squares, square roots and reciprocals, are worked out
/// directly: each is exact in IEEE 754, which `pow` only comes within a bit of, and several times
/// quicker. They're chosen here, as the program runs, so `x ** 2.0` and `x ** two` give the same
/// bits. Each keeps what `pow` gives at -0, at infinity and for NaN. The exponent is matched by its
/// bits, exactly.
pub extern "C" fn float_power(a: f64, b: f64) -> f64 {
    if b.to_bits() == (2.0_f64).to_bits() {
        a * a
    } else if b.to_bits() == (0.5_f64).to_bits() {
        // `sqrt(-0)` is -0 and `sqrt(-inf)` is NaN, where `pow` gives +0 and +inf.
        if a == 0.0 {
            0.0
        } else if a == f64::NEG_INFINITY {
            f64::INFINITY
        } else {
            a.sqrt()
        }
    } else if b.to_bits() == (1.0_f64).to_bits() {
        a
    } else if b == 0.0 {
        // Even of NaN, as `pow` has it.
        1.0
    } else if b.to_bits() == (-1.0_f64).to_bits() {
        1.0 / a
    } else {
        libm::pow(a, b)
    }
}

/// Fails with the language's message for an index past either end of a list.
pub extern "C" fn out_of_range(at: i64, len: i64, start: i64, end: i64) {
    attempt(|_: &mut State| -> Result<(), Diagnostic> {
        Err(Diagnostic::new(format!("index {at} is out of range for length {len}"), span(start, end)))
    });
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod sorting {
    use std::cmp::Ordering;

    /// A comparison that contradicts itself still gives back every place once.
    #[test]
    fn a_comparison_that_contradicts_itself_still_gives_every_place() {
        for count in [0, 1, 2, 7, 20, 33, 100, 1000] {
            let mut turn = 0_u64;
            let order = super::sorted_order(count, |a, b| {
                turn = turn.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
                match (turn >> 33) % 3 {
                    0 => Ordering::Less,
                    1 => Ordering::Greater,
                    _ => a.cmp(&b).reverse(),
                }
            });
            let mut seen = order.clone();
            seen.sort_unstable();
            assert_eq!(seen, (0..count).collect::<Vec<_>>());
        }
    }

    #[test]
    fn equal_places_stay_in_order() {
        let keys = [3, 1, 2, 1, 3, 2, 1];
        let order = super::sorted_order(keys.len(), |a, b| keys[a].cmp(&keys[b]));
        assert_eq!(order, vec![1, 3, 6, 2, 5, 0, 4]);
    }
}
