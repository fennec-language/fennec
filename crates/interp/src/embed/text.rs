//! A string held in place as its 24-byte header, as LLVM's code holds one: up to 23 bytes
//! of text in the header itself, or the address of a string's block. The last byte says
//! which, so the runtime never has to guess:
//!
//! - **Inline:** the bytes, zero after the end, and `0x80 | length` in byte 23.
//! - **In a block:** word 0 is the block's address and word 1 the length in bytes, and byte 23
//!   is 0.
//!
//! A string of 23 bytes or fewer is always inline, so two equal strings always have the same
//! form, and two inline ones are equal exactly when their words are. Native code hands a
//! header to the runtime as its three words, never as an address to read.

use std::rc::Rc;

use super::heap::{TEXT_HEADER_WORDS, block_text, release, retain, text_from_bytes};
use super::{State, with};
use crate::packed::Element;

/// The most bytes a header holds itself.
pub const INLINE_BYTES: usize = 23;

/// What byte 23 holds, above the length, for an inline string.
pub const INLINE: u8 = 0x80;

/// A string's header, as the runtime gives one back to native code.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Text {
    pub words: [u64; 3],
}

/// What byte 23 of a header holds.
fn tag(words: &[u64; 3]) -> u8 {
    let [.., last] = *words;
    let [.., byte] = last.to_le_bytes();
    byte
}

/// The block a header points at, when it's in one.
pub(super) fn block_of(words: &[u64; 3]) -> Option<u64> {
    let [first, ..] = *words;
    (tag(words) & INLINE == 0).then_some(first)
}

/// The bytes an inline header holds, when it's inline.
pub(super) fn inline_bytes(words: &[u64; 3]) -> Option<Vec<u8>> {
    let tag = tag(words);
    if tag & INLINE == 0 {
        return None;
    }
    let length = usize::from(tag & !INLINE).min(INLINE_BYTES);
    let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).take(length).collect();
    Some(bytes)
}

/// An inline header holding `length` bytes, 8 or fewer, packed in a word with zeros after them.
pub(super) fn inline_word(word: u64, length: u8) -> [u64; 3] {
    [word, 0, u64::from(INLINE | length) << 56]
}

/// An inline header holding `bytes`, which are 23 or fewer.
pub(super) fn inline(bytes: &[u8]) -> [u64; 3] {
    let mut packed = [0_u8; 24];
    let length = bytes.len().min(INLINE_BYTES);
    if let (Some(to), Some(from)) = (packed.get_mut(..length), bytes.get(..length)) {
        to.copy_from_slice(from);
    }
    if let Some(last) = packed.last_mut() {
        *last = INLINE | u8::try_from(length).unwrap_or(0);
    }
    let word = |at: usize| {
        let mut eight = [0_u8; 8];
        if let Some(from) = packed.get(at * 8..at * 8 + 8) {
            eight.copy_from_slice(from);
        }
        u64::from_le_bytes(eight)
    };
    [word(0), word(1), word(2)]
}

/// The header of a string's text, inline when it fits, making a block when it doesn't.
pub(super) fn header(state: &mut State, text: &str) -> [u64; 3] {
    if text.len() <= INLINE_BYTES {
        return inline(text.as_bytes());
    }
    let address = super::heap::text_address(state, text);
    [address.cast_unsigned(), u64::try_from(text.len()).unwrap_or(0), 0]
}

/// The text a header holds, from itself or from its block.
pub(super) fn text_of(state: &State, words: &[u64; 3]) -> String {
    if let Some(bytes) = inline_bytes(words) {
        return String::from_utf8_lossy(&bytes).into_owned();
    }
    let block = block_of(words).and_then(|address| state.heap.block(address));
    block.filter(|block| super::heap::is_text(state, block)).map_or_else(String::new, block_text)
}

/// The kind of item a list of strings the runtime makes holds: headers, when native code holds
/// strings as headers, and blocks' addresses otherwise.
pub(super) fn string_element(state: &State) -> Rc<Element> {
    Rc::new(if state.tables.text_headers { Element::Text } else { Element::Str })
}

/// The texts of a list of headers' items, laid out three words each.
pub(super) fn texts(state: &State, words: &[u64]) -> Vec<String> {
    words.as_chunks::<3>().0.iter().map(|header| text_of(state, header)).collect()
}

/// `fennec_text_block(a, b, c)`: a string's block, owned once, for the runtime's string calls,
/// which take blocks. An inline string is copied into a new block; a string in a block gives
/// that block, with one more owner.
pub extern "C" fn block(a: u64, b: u64, c: u64) -> i64 {
    let words = [a, b, c];
    with(|state| {
        if let Some(bytes) = inline_bytes(&words) {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            return text_from_bytes(state, text.as_bytes(), text.chars().count());
        }
        let address = block_of(&words).unwrap_or(0);
        retain(state, address);
        address.cast_signed()
    })
}

/// `fennec_text_of(address)`: the header of a string's block, taking over the owner given.
/// Text of 23 bytes or fewer comes back inline, letting go of the block.
pub extern "C" fn of(address: i64) -> Text {
    with(|state| {
        let at = address.cast_unsigned();
        let Some(block) = state.heap.block(at).filter(|block| super::heap::is_text(state, block)) else {
            return Text { words: inline(&[]) };
        };
        let length = block.get(2).and_then(|length| usize::try_from(*length).ok()).unwrap_or(0);
        if length > INLINE_BYTES {
            return Text { words: [at, u64::try_from(length).unwrap_or(0), 0] };
        }
        let bytes: Vec<u8> = block
            .get(TEXT_HEADER_WORDS..)
            .unwrap_or_default()
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .take(length)
            .collect();
        let words = inline(&bytes);
        release(state, at);
        Text { words }
    })
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::super::{Tables, finish, heap, lists, start, strings, with};
    use super::{INLINE, block_of, inline_bytes};
    use crate::packed::Element;
    use crate::value::Value;

    fn begin() {
        let text = Rc::new(Element::Text);
        let optional = Rc::new(Element::Maybe(Rc::clone(&text)));
        let tables = Tables {
            shapes: vec![Rc::new(Element::Str)],
            elements: vec![text, optional],
            text_headers: true,
            ..Tables::default()
        };
        start(&fennec_syntax::Module { items: Vec::new() }, tables, crate::Inputs::default()).unwrap();
    }

    fn header(text: &str) -> [u64; 3] {
        with(|state| Some(super::header(state, text))).unwrap()
    }

    fn text_of(words: [u64; 3]) -> String {
        with(|state| Some(super::text_of(state, &words))).unwrap()
    }

    fn live() -> usize {
        with(|state| state.heap.live())
    }

    #[test]
    fn a_string_of_23_bytes_or_fewer_is_held_in_its_header() {
        begin();
        let before = live();
        let short = header("twenty-three bytes long");
        assert_eq!(super::tag(&short), INLINE + 23);
        assert_eq!(inline_bytes(&short).as_deref(), Some(&b"twenty-three bytes long"[..]));
        assert_eq!(header(""), super::inline(&[]));
        assert_eq!(live(), before);
        let long = header("twenty-four bytes long!!");
        assert!(block_of(&long).is_some());
        assert_eq!(long[1], 24);
        assert_eq!(text_of(long), "twenty-four bytes long!!");
        assert_eq!(text_of(header("é and €")), "é and €");
        // Through the calls native code makes: a block of a header, and a header of a block.
        let block = super::block(short[0], short[1], short[2]);
        assert_eq!(super::of(block).words, short);
        let again = super::of(super::block(long[0], long[1], long[2]));
        assert_eq!(again.words, long);
        heap::free(i64::try_from(long[0]).unwrap());
        assert_eq!(live(), before);
        finish();
    }

    #[test]
    fn a_list_of_headers_is_joined_sorted_searched_compared_and_freed() {
        begin();
        let before = live();
        let names = ["medic", "a name too long for any header", "archer"];
        let items: Vec<Value> = names.iter().map(|name| Value::str(name)).collect();
        let temp = with(|state| Some(state.push(Value::list(items.clone())))).unwrap();
        let list = lists::from_value(temp, 0, 0, 0);
        let read =
            with(|state| Some(heap::read_block(state, u64::try_from(list).unwrap()).unpacked())).unwrap();
        assert_eq!(read, Value::list(items.clone()));
        let comma = with(|state| heap::text_address(state, ", "));
        let joined = strings::join(comma, list);
        let text = with(|state| Some(super::text_of(state, &[joined.cast_unsigned(), 0, 0]))).unwrap();
        assert_eq!(text, names.join(", "));
        let sorted = lists::sorted(list, lists::SORT_STRINGS);
        let read =
            with(|state| Some(heap::read_block(state, u64::try_from(sorted).unwrap()).unpacked())).unwrap();
        let mut ordered = names.to_vec();
        ordered.sort_unstable();
        assert_eq!(read, Value::list(ordered.iter().map(|name| Value::str(name)).collect()));
        // Sorted by keys that are strings too, here the list itself backwards.
        let keys: Vec<Value> = ["c", "b", "a"].iter().map(|key| Value::str(key)).collect();
        let keys = with(|state| Some(state.push(Value::list(keys)))).unwrap();
        let keys = lists::from_value(keys, 0, 0, 0);
        let by_keys = lists::sorted_by(list, keys, lists::SORT_STRINGS);
        let read =
            with(|state| Some(heap::read_block(state, u64::try_from(by_keys).unwrap()).unpacked())).unwrap();
        assert_eq!(read, Value::list(items.iter().rev().cloned().collect()));
        heap::free(keys);
        heap::free(by_keys);
        let wanted = with(|state| heap::text_address(state, "a name too long for any header"));
        assert_eq!(lists::contains(list, wanted), 1);
        let copy = lists::from_value(temp, 0, 0, 0);
        assert_eq!(heap::equal(list, copy), 1);
        let spaced = with(|state| heap::text_address(state, "a b"));
        let split = strings::split(spaced, 0, 0, 0);
        let element =
            with(|state| lists::element_of(state, state.heap.block(split.cast_unsigned())?)).unwrap();
        assert_eq!(*element, Element::Text);
        for done in [list, sorted, copy, split, spaced, joined, wanted, comma] {
            heap::free(done);
        }
        assert_eq!(live(), before);
        finish();
    }

    #[test]
    fn an_optional_string_is_a_header_with_all_zero_for_none() {
        begin();
        let before = live();
        let items = vec![Value::None, Value::str("short"), Value::str("a string too long for any header")];
        let temp = with(|state| Some(state.push(Value::list(items.clone())))).unwrap();
        let list = lists::from_value(temp, 1, 0, 0);
        let words =
            with(|state| Some(lists::items(state.heap.block(list.cast_unsigned())?, 3).to_vec())).unwrap();
        assert_eq!(words.len(), 9);
        assert_eq!(words.get(..3), Some(&[0, 0, 0][..]));
        let read =
            with(|state| Some(heap::read_block(state, u64::try_from(list).unwrap()).unpacked())).unwrap();
        assert_eq!(read, Value::list(items.clone()));
        // Equal to a copy, and not to one with `None` where a string was.
        let copy = lists::from_value(temp, 1, 0, 0);
        assert_eq!(heap::equal(list, copy), 1);
        let other = vec![Value::None, Value::None, Value::str("a string too long for any header")];
        let other = with(|state| Some(state.push(Value::list(other)))).unwrap();
        let other = lists::from_value(other, 1, 0, 0);
        assert_eq!(heap::equal(list, other), 0);
        assert_eq!(heap::equal(other, list), 0);
        // `in` finds `None` and strings by their text.
        assert_eq!(lists::contains(list, 0), 1);
        let wanted = with(|state| heap::text_address(state, "a string too long for any header"));
        assert_eq!(lists::contains(list, wanted), 1);
        let missing = with(|state| heap::text_address(state, "not there"));
        assert_eq!(lists::contains(list, missing), 0);
        for done in [list, copy, other, wanted, missing] {
            heap::free(done);
        }
        assert_eq!(live(), before);
        finish();
    }

    #[test]
    fn a_dict_of_strings_finds_and_sets_a_key_given_as_its_header_without_making_a_block() {
        use super::super::dicts;
        begin();
        let before = live();
        let shape = dicts::shape(0, dicts::Word::Str, dicts::Word::Int).unwrap();
        let mut dict = dicts::new(i64::try_from(shape).unwrap(), 4);
        let read = |dict: i64, key: [u64; 3]| {
            let found = dicts::find_header(dict, key[0], key[1], key[2]);
            (found != 0).then(|| {
                with(|state| {
                    let block = state.heap.block(dict.cast_unsigned())?;
                    let at = (usize::try_from(found).ok()? - block.as_ptr().addr()) / 8;
                    block.get(at).copied()
                })
            })
        };
        let short = header("fox");
        dict = dicts::set_header(dict, short[0], short[1], short[2], 1);
        let allocations = with(|state| state.heap.counts.allocations);
        assert_eq!(read(dict, header("fox")), Some(Some(1)));
        assert_eq!(read(dict, header("cat")), None);
        assert_eq!(with(|state| state.heap.counts.allocations), allocations);
        dict = dicts::set_header(dict, short[0], short[1], short[2], 2);
        assert_eq!(read(dict, short), Some(Some(2)));
        let long = header("a key much longer than any header");
        dict = dicts::set_header(dict, long[0], long[1], long[2], 3);
        // The dict holds the key's block now, so the header still reads it.
        assert_eq!(read(dict, long), Some(Some(3)));
        dict = dicts::pop_header(dict, short[0], short[1], short[2], 0, 0);
        assert_eq!(dicts::popped(), 2);
        assert_eq!(read(dict, short), None);
        heap::free(dict);
        assert_eq!(live(), before);
        finish();
    }
}
