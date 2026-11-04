//! Dicts for native code. A dict is a block in the heap (see `heap`) holding a hash table:
//! its owner count, its shape, how many keys it has, how many entries it has used, and one
//! less than its number of slots. Then the slots, each 0 for empty or an entry's number
//! plus one, then the entries in the order their keys were added: each a key's hash, the
//! key and its value. Native code looks keys up, changes values and adds keys itself while
//! the dict has one owner and room; everything else happens here.
//!
//! Keys are ints, strings or plain structs, a plain struct key being the address of a block of
//! its own. Values are ints, floats, bools or strings, each one word. Which
//! ones a dict holds is in its shape: the shapes after the string shape, one for each pair.
//! A set is a dict whose values are nothing.

use fennec_syntax::Diagnostic;

use super::heap::shown::Written;
use super::heap::{
    HEADER_WORDS, TEXT_HEADER_WORDS, release, same, shape_word, text_address, text_from_bytes,
};
use super::{State, attempt, span, with};
use crate::packed::Element;
use crate::value::Value;

/// Words a dict's block starts with: owner count, shape, keys, entries used, slots less one.
pub const DICT_HEADER_WORDS: usize = 5;

/// Words an entry takes: hash, key, value.
pub const ENTRY_WORDS: usize = 3;

/// The fewest slots a table has.
const MIN_SLOTS: usize = 8;

/// The hash a removed entry is given. Real hashes never have their top bit set.
const REMOVED: u64 = u64::MAX;

const MIX: u64 = 0x9E37_79B9_7F4A_7C15;

/// What a dict's keys or values are.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Word {
    Int,
    Float,
    Bool,
    Str,
    /// What a set's keys have for values.
    Nothing,
    /// A plain struct or enum as a key, by its layout's number, which native code knows. The
    /// runtime goes by the shape of the key's block.
    Plain(u32),
    /// A string key held in the entry as its 24-byte header (see `super::text`): up to 23
    /// bytes in the header itself, or a block's address and length. Strings of 23 bytes or
    /// fewer are always inline, so a key's header is the same however it was made.
    Text,
    /// As a value, the address of a block of anything held by one, like a list, by that kind's
    /// number in `Tables::blocks`. Shapes of its own say which, so the runtime can make one.
    Block(u32),
}

impl Word {
    /// Whether a word of this kind is the address of a block the dict owns.
    fn owned(self) -> bool {
        matches!(self, Word::Str | Word::Plain(_) | Word::Block(_))
    }
}

const KEYS: [Word; 5] = [Word::Int, Word::Str, Word::Plain(0), Word::Text, Word::Float];

/// Words an entry with a string header for its key takes: hash, the header's three, value.
const HEADER_ENTRY_WORDS: usize = 5;
const VALUES: [Word; 6] = [Word::Int, Word::Float, Word::Bool, Word::Str, Word::Nothing, Word::Plain(0)];

/// The highest shape of a dict with `Word::Block` values, which count down from here, below the
/// shapes that aren't numbered from the string shape.
const TOP_BLOCK_DICT: u64 = super::heap::SHAPE_BITS - 16;
/// How many kinds of block a dict's values can be.
const BLOCK_KINDS: u64 = 1 << 20;

/// How many shapes dicts take, after the string shape.
pub const SHAPES: u64 = (KEYS.len() * VALUES.len()) as u64;

/// The shape of dicts with these keys and values, counted from the string shape, or `None`
/// for a pair native dicts don't hold.
pub fn shape(str_shape: u64, key: Word, value: Word) -> Option<u64> {
    let plain = |word| if let Word::Plain(_) = word { Word::Plain(0) } else { word };
    let key = KEYS.iter().position(|word| *word == plain(key))?;
    if let Word::Block(number) = value {
        let count = u64::try_from(KEYS.len()).ok()?;
        return Some(TOP_BLOCK_DICT - u64::from(number) * count - u64::try_from(key).ok()?);
    }
    let value = VALUES.iter().position(|word| *word == plain(value))?;
    u64::try_from(key * VALUES.len() + value).ok().map(|at| str_shape + 1 + at)
}

/// What a dict's block holds, when it is a dict's.
fn kind(state: &State, block: &[u64]) -> Option<(Word, Word)> {
    layout(state, block).map(|layout| (layout.key, layout.value))
}

/// How a dict's entries are laid out: what its keys and values are, and how many words an
/// entry takes, a hash, then the key's words, then the value's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layout {
    key: Word,
    value: Word,
    width: usize,
}

impl Layout {
    /// Where an entry's value is, from the entry's start.
    fn value_at(self) -> usize {
        self.width - 1
    }
}

/// How a dict's block lays out its entries, when it is a dict's.
fn layout(state: &State, block: &[u64]) -> Option<Layout> {
    shape_layout(state, shape_word(block)?)
}

/// How a dict of this shape lays out its entries, when the shape is a dict's.
fn shape_layout(state: &State, shape: u64) -> Option<Layout> {
    let count = u64::try_from(KEYS.len()).ok()?;
    let (key, value) = if shape <= TOP_BLOCK_DICT && shape > TOP_BLOCK_DICT - BLOCK_KINDS * count {
        let at = TOP_BLOCK_DICT - shape;
        let key = *KEYS.get(usize::try_from(at % count).ok()?)?;
        (key, Word::Block(u32::try_from(at / count).ok()?))
    } else {
        let at = usize::try_from(shape.checked_sub(state.tables.str_shape + 1)?).ok()?;
        (*KEYS.get(at / VALUES.len())?, *VALUES.get(at % VALUES.len())?)
    };
    let width = if key == Word::Text { HEADER_ENTRY_WORDS } else { ENTRY_WORDS };
    Some(Layout { key, value, width })
}

/// An entry's key: its words between the hash and the value.
fn key_of(entry: &[u64]) -> &[u64] {
    entry.get(1..entry.len().saturating_sub(1)).unwrap_or_default()
}

/// An entry's value: its last word.
fn value_of(entry: &[u64]) -> u64 {
    entry.last().copied().unwrap_or(0)
}

/// The block a key is held in, when its kind is held in one: a one-word key's address.
fn key_block(key_word: Word, key: &[u64]) -> Option<u64> {
    match key {
        [address] if key_word.owned() => Some(*address),
        [a, b, c] if key_word == Word::Text => super::text::block_of(&[*a, *b, *c]),
        _ => None,
    }
}

pub(super) fn is_dict(state: &State, block: &[u64]) -> bool {
    kind(state, block).is_some()
}

/// How many keys a table with `slots` slots holds before it grows.
pub fn capacity(slots: usize) -> usize {
    slots - slots / 4
}

/// An int key's hash. Native code works it out the same way.
pub fn hash_int(key: u64) -> u64 {
    finish(key.wrapping_mul(MIX))
}

/// A string key's hash, from its length in bytes and its words of text: its bytes packed
/// little-endian eight to a word, from its first byte, the last word padded with zero bytes,
/// as its block holds them. Native code works it out the same way, for a slice of a string too,
/// so this mustn't change without changing them.
pub fn hash_text(length: u64, words: &[u64]) -> u64 {
    finish(
        words
            .iter()
            .fold(length.wrapping_mul(MIX), |hash, word| (hash.rotate_left(23) ^ word).wrapping_mul(MIX)),
    )
}

/// A plain struct key's hash, from its words. Native code works it out the same way.
pub fn hash_words(words: &[u64]) -> u64 {
    let count = u64::try_from(words.len()).unwrap_or(0);
    finish(
        words
            .iter()
            .fold(count.wrapping_mul(MIX), |hash, word| (hash.rotate_left(23) ^ word).wrapping_mul(MIX)),
    )
}

fn finish(hash: u64) -> u64 {
    (hash ^ (hash >> 29)) >> 1
}

/// Whether a plain key is hashed by what it holds rather than by its words.
fn by_what_it_holds(element: &crate::packed::Element) -> bool {
    use crate::packed::Element;
    match element {
        Element::Float => true,
        Element::Struct { fields, .. } => fields.iter().any(|(_, field)| by_what_it_holds(field)),
        Element::Enum { cases, .. } => {
            cases.iter().any(|(_, fields)| fields.iter().any(|(_, field)| by_what_it_holds(field)))
        }
        other => super::heap::holds_blocks(other),
    }
}

/// `dict[key]`'s entry for a plain key that's hashed by what it holds, or 0. The key's block stays
/// the caller's.
pub extern "C" fn find_by_value(dict: i64, key: i64) -> i64 {
    with(|state| {
        let Some(block) = u64::try_from(dict).ok().and_then(|at| state.heap.block(at)) else { return 0 };
        let Some(layout) = layout(state, block) else { return 0 };
        let word = [key.cast_unsigned()];
        let hash = hash_key(state, layout.key, &word);
        match find(state, block, layout, &word, hash) {
            Probe::Found(entry) => {
                let mask = block.get(4).and_then(|mask| usize::try_from(*mask).ok()).unwrap_or(0);
                let at = DICT_HEADER_WORDS + mask + 1 + entry * layout.width;
                block.get(at).map_or(0, |place| i64::try_from(std::ptr::from_ref(place).addr()).unwrap_or(0))
            }
            Probe::Empty(_) => 0,
        }
    })
}

/// A float key's bits as they compare and hash: floats have one equality, with `-0.0` the same as
/// `0.0` and every NaN the same.
fn float_key(bits: u64) -> u64 {
    let value = f64::from_bits(bits);
    if value == 0.0 {
        0
    } else if value.is_nan() {
        f64::NAN.to_bits()
    } else {
        bits
    }
}

fn hash_key(state: &State, key_word: Word, key: &[u64]) -> u64 {
    let first = key.first().copied().unwrap_or(0);
    if key_word == Word::Float {
        return hash_int(float_key(first));
    }
    if let Word::Plain(_) = key_word {
        let block = state.heap.block(first).unwrap_or_default();
        // Blocks or floats in a key go by what they hold, not by their words.
        if super::heap::element_of_block(state, block).is_some_and(|element| by_what_it_holds(&element)) {
            return finish(crate::pairs::hash_of(&super::heap::read_block(state, first)));
        }
        return hash_words(block.get(HEADER_WORDS..).unwrap_or_default());
    }
    match key_word {
        Word::Str => hash_block_text(state, first),
        Word::Text => hash_header(state, key),
        _ => hash_int(first),
    }
}

/// The hash of the string in a block.
fn hash_block_text(state: &State, address: u64) -> u64 {
    let block = state.heap.block(address).unwrap_or_default();
    let length = block.get(2).copied().unwrap_or(0);
    let words = usize::try_from(length).unwrap_or(0).div_ceil(8);
    hash_text(length, block.get(TEXT_HEADER_WORDS..TEXT_HEADER_WORDS + words).unwrap_or_default())
}

/// The hash of a string held as its header: an inline one's words are its bytes packed as a
/// block packs them, once byte 23 is cleared, so both forms hash alike.
fn hash_header(state: &State, key: &[u64]) -> u64 {
    let Ok(&[a, b, c]) = <&[u64; 3]>::try_from(key) else { return 0 };
    if let Some(address) = super::text::block_of(&[a, b, c]) {
        return hash_block_text(state, address);
    }
    let length = (c >> 56) & 0x7f;
    let words = [a, b, c & (u64::MAX >> 8)];
    debug_assert!(
        words
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .skip(usize::try_from(length).unwrap_or(0))
            .all(|byte| byte == 0),
        "an inline header is zero after its bytes"
    );
    hash_text(length, words.get(..usize::try_from(length).unwrap_or(0).div_ceil(8)).unwrap_or_default())
}

fn keys_equal(state: &State, key_word: Word, a: &[u64], b: &[u64]) -> bool {
    match (a, b) {
        ([a], [b]) if key_word == Word::Float => float_key(*a) == float_key(*b),
        ([a], [b]) => a == b || (key_word.owned() && same(state, *a, *b)),
        // Equal strings have the same form, so inline headers are equal exactly when their
        // words are, and only two blocks need their bytes compared.
        _ => {
            a == b
                || matches!((key_block(key_word, a), key_block(key_word, b)), (Some(a), Some(b)) if same(state, a, b))
        }
    }
}

/// The live entries of a dict's block, in order, one after another, each `width` words: its
/// hash, its key and its value.
fn entries(block: &[u64], width: usize) -> Vec<u64> {
    let slots = block.get(4).map_or(0, |mask| usize::try_from(*mask).unwrap_or(0) + 1);
    let used = block.get(3).and_then(|used| usize::try_from(*used).ok()).unwrap_or(0);
    let start = DICT_HEADER_WORDS + slots;
    let words = block.get(start..start + used * width).unwrap_or_default();
    let mut live = Vec::with_capacity(words.len());
    match width {
        3 => live_entries::<3>(words, &mut live),
        4 => live_entries::<4>(words, &mut live),
        5 => live_entries::<5>(words, &mut live),
        6 => live_entries::<6>(words, &mut live),
        _ => {
            for entry in words.chunks_exact(width).filter(|entry| entry.first() != Some(&REMOVED)) {
                live.extend_from_slice(entry);
            }
        }
    }
    live
}

/// The live entries of `N` words each, added to `live`. A width known here copies an entry in
/// a few moves, where a slice's copy is a call.
fn live_entries<const N: usize>(words: &[u64], live: &mut Vec<u64>) {
    for entry in words.as_chunks::<N>().0 {
        if entry.first() != Some(&REMOVED) {
            live.extend(entry.iter().copied());
        }
    }
}

/// Copies an entry's words, a few moves for the widths dicts have. Gives back whether the two
/// were the same width, as every caller's are; nothing is copied when they aren't.
fn copy_entry(to: &mut [u64], from: &[u64]) -> bool {
    fn fixed<const N: usize>(to: &mut [u64], from: &[u64]) -> bool {
        match (<&mut [u64; N]>::try_from(to), <&[u64; N]>::try_from(from)) {
            (Ok(to), Ok(from)) => {
                *to = *from;
                true
            }
            _ => false,
        }
    }
    let copied = fixed::<3>(to, from) || fixed::<4>(to, from) || fixed::<5>(to, from) || fixed::<6>(to, from);
    if !copied && to.len() == from.len() {
        to.copy_from_slice(from);
    }
    to.len() == from.len()
}

/// A block holding `entries`, each `width` words, owned once, with room for at least `room`
/// keys.
fn build(state: &mut State, shape: u64, width: usize, entries: &[u64], room: usize) -> Box<[u64]> {
    let live = entries.len() / width;
    let mut slots = MIN_SLOTS;
    while capacity(slots) < room.max(live) {
        slots *= 2;
    }
    let mut block = state.heap.fresh(DICT_HEADER_WORDS + slots + capacity(slots) * width);
    let count = |n: usize| u64::try_from(n).unwrap_or(0);
    if let Some(header) = block.get_mut(..DICT_HEADER_WORDS) {
        header.copy_from_slice(&[1, shape, count(live), count(live), count(slots - 1)]);
    }
    for (at, entry) in entries.chunks_exact(width).enumerate() {
        let hash = entry.first().copied().unwrap_or(0);
        let mut slot = usize::try_from(hash).unwrap_or(0) & (slots - 1);
        while block.get(DICT_HEADER_WORDS + slot).is_some_and(|taken| *taken != 0) {
            slot = (slot + 1) & (slots - 1);
        }
        if let Some(taken) = block.get_mut(DICT_HEADER_WORDS + slot) {
            *taken = count(at + 1);
        }
        let start = DICT_HEADER_WORDS + slots + at * width;
        if let Some(words) = block.get_mut(start..start + width) {
            let copied = copy_entry(words, entry);
            debug_assert!(copied, "an entry as wide as its dict's");
        }
    }
    block
}

/// Where a key is: the number of its entry, or of the empty slot where it would go.
enum Probe {
    Found(usize),
    Empty(usize),
}

fn find(state: &State, block: &[u64], layout: Layout, key: &[u64], hash: u64) -> Probe {
    let mask = block.get(4).and_then(|mask| usize::try_from(*mask).ok()).unwrap_or(0);
    let start = DICT_HEADER_WORDS + mask + 1;
    let mut slot = usize::try_from(hash).unwrap_or(0) & mask;
    loop {
        let taken = block.get(DICT_HEADER_WORDS + slot).and_then(|taken| usize::try_from(*taken).ok());
        let Some(entry) = taken.and_then(|taken| taken.checked_sub(1)) else { return Probe::Empty(slot) };
        let at = start + entry * layout.width;
        if block.get(at) == Some(&hash)
            && block
                .get(at + 1..at + layout.value_at())
                .is_some_and(|other| keys_equal(state, layout.key, other, key))
        {
            return Probe::Found(entry);
        }
        slot = (slot + 1) & mask;
    }
}

/// Where entry number `entry`'s value is in a dict's block.
fn value_place(block: &[u64], layout: Layout, entry: usize) -> Option<usize> {
    let mask = usize::try_from(*block.get(4)?).ok()?;
    Some(DICT_HEADER_WORDS + mask + 1 + entry * layout.width + layout.value_at())
}

/// `text[from:to]`, with positions in characters clamped the way slicing clamps them, as a
/// string key's block would hold it: its length in bytes and in characters, and its bytes.
fn slice_of(state: &State, text: i64, from: i64, to: i64) -> Option<(Vec<u8>, usize)> {
    let block = state.heap.block(u64::try_from(text).ok()?)?;
    if !super::heap::is_text(state, block) {
        return None;
    }
    let length = usize::try_from(*block.get(2)?).ok()?;
    let chars = usize::try_from(*block.get(3)?).ok()?;
    let count = i64::try_from(chars).ok()?;
    let bound = |at: i64, missing: i64| {
        let at = if at == i64::MIN { missing } else { at };
        let at = if at < 0 { at + count } else { at };
        usize::try_from(at.clamp(0, count)).unwrap_or(0)
    };
    let (from, to) = (bound(from, 0), bound(to, count));
    let to = to.max(from);
    let words = block.get(TEXT_HEADER_WORDS..TEXT_HEADER_WORDS + length.div_ceil(8))?;
    let byte = |at: usize| words.get(at / 8).and_then(|word| word.to_le_bytes().get(at % 8).copied());
    if length == chars {
        // ASCII: a character is a byte, so only the bytes in the slice are read.
        let mut bytes = Vec::with_capacity(to - from);
        for at in from..to {
            bytes.push(byte(at)?);
        }
        return Some((bytes, to - from));
    }
    let all: Option<Vec<u8>> = (0..length).map(byte).collect();
    let all = String::from_utf8(all?).ok()?;
    let part: String = all.chars().skip(from).take(to - from).collect();
    Some((part.into_bytes(), to - from))
}

/// Bytes as a string's block packs them, eight to a word, the last one padded with zeros.
fn packed_words(bytes: &[u8]) -> Vec<u64> {
    bytes
        .chunks(8)
        .map(|chunk| {
            let mut word = [0_u8; 8];
            word.get_mut(..chunk.len()).unwrap_or_default().copy_from_slice(chunk);
            u64::from_le_bytes(word)
        })
        .collect()
}

/// Where a string key with these bytes is in a dict of string keys, comparing hashes first
/// and then the key's length and words, with no key of its own made.
fn find_text(state: &State, block: &[u64], (bytes, chars): (&[u8], usize)) -> Option<(Probe, u64)> {
    let layout = layout(state, block)?;
    let width = layout.width;
    let short = (bytes.len() <= super::text::INLINE_BYTES).then(|| super::text::inline(bytes));
    let words = packed_words(bytes);
    let length = u64::try_from(bytes.len()).ok()?;
    let hash = hash_text(length, &words);
    let mask = usize::try_from(*block.get(4)?).ok()?;
    let start = DICT_HEADER_WORDS + mask + 1;
    let header = [length, u64::try_from(chars).ok()?];
    let mut slot = usize::try_from(hash).unwrap_or(0) & mask;
    loop {
        let taken = block.get(DICT_HEADER_WORDS + slot).and_then(|taken| usize::try_from(*taken).ok());
        let Some(entry) = taken.and_then(|taken| taken.checked_sub(1)) else {
            return Some((Probe::Empty(slot), hash));
        };
        let at = start + entry * width;
        if block.get(at) == Some(&hash) {
            let same_block = |address: u64| {
                state.heap.block(address).is_some_and(|key| {
                    key.get(2..4) == Some(&header[..])
                        && key.get(TEXT_HEADER_WORDS..TEXT_HEADER_WORDS + words.len()) == Some(&words[..])
                })
            };
            let same_text = match block.get(at + 1..at + layout.value_at()) {
                // A key held as its header: inline ones are equal exactly when their words are.
                Some(&[a, b, c]) if layout.key == Word::Text => match super::text::block_of(&[a, b, c]) {
                    Some(address) => same_block(address),
                    None => short == Some([a, b, c]),
                },
                Some(&[address]) => same_block(address),
                _ => false,
            };
            if same_text {
                return Some((Probe::Found(entry), hash));
            }
        }
        slot = (slot + 1) & mask;
    }
}

/// `dict.get(text[from:to])` for a dict of string keys, with no string made for the key: the
/// address of the key's value word, to read straight away, or 0 if the key isn't there.
pub extern "C" fn find_slice(dict: i64, text: i64, from: i64, to: i64) -> i64 {
    with(|state| {
        let slice = slice_of(state, text, from, to)?;
        let block = state.heap.block(u64::try_from(dict).ok()?)?;
        if !matches!(kind(state, block)?.0, Word::Str | Word::Text) {
            return None;
        }
        let (Probe::Found(entry), _) = find_text(state, block, (&slice.0, slice.1))? else { return Some(0) };
        let word = value_place(block, layout(state, block)?, entry)?;
        i64::try_from(block.as_ptr().addr() + word * 8).ok()
    })
    .unwrap_or(0)
}

/// `dict[text[from:to]] = value` for a dict of string keys: takes over the owners `dict` and
/// `value` had, and gives back the dict's, like `set`. A string for the key is only made when
/// the key is new.
pub extern "C" fn add_slice(dict: i64, text: i64, from: i64, to: i64, value: i64) -> i64 {
    with(|state| {
        let (bytes, chars) = slice_of(state, text, from, to)?;
        let at = u64::try_from(dict).ok()?;
        let block = state.heap.block(at)?;
        let (key_word, value_word) = kind(state, block)?;
        if !matches!(key_word, Word::Str | Word::Text) {
            return None;
        }
        let unique = state.heap.is_alone(at, block);
        if let (true, (Probe::Found(entry), _)) = (unique, find_text(state, block, (&bytes, chars))?) {
            let word = value_place(block, layout(state, block)?, entry)?;
            let value = u64::from_ne_bytes(value.to_ne_bytes());
            let old = std::mem::replace(state.heap.block_mut(at)?.get_mut(word)?, value);
            if value_word.owned() {
                release(state, old);
            }
            return Some(dict);
        }
        let key = new_key(state, key_word, (&bytes, chars));
        set_words(state, dict, &key, value.cast_unsigned())
    })
    .unwrap_or(dict)
}

/// A new string key's words for a dict holding this kind of key, made from its bytes: its
/// header, inline when it fits, or else the address of a new block, owned once.
fn new_key(state: &mut State, key_word: Word, (bytes, chars): (&[u8], usize)) -> Vec<u64> {
    if key_word == Word::Text && bytes.len() <= super::text::INLINE_BYTES {
        return super::text::inline(bytes).to_vec();
    }
    let address = text_from_bytes(state, bytes, chars).cast_unsigned();
    if key_word == Word::Text {
        return vec![address, u64::try_from(bytes.len()).unwrap_or(0), 0];
    }
    vec![address]
}

/// `add_slice`, for a caller that has just looked for the key and not found it: the key's
/// block is made straight from the text's, with no looking first. A key that's there after
/// all is still found, by `set_in`.
pub extern "C" fn add_new_slice(dict: i64, text: i64, from: i64, to: i64, value: i64) -> i64 {
    with(|state| {
        let held_as_headers = state
            .heap
            .block(u64::try_from(dict).ok()?)
            .and_then(|block| kind(state, block))
            .is_some_and(|(key, _)| key == Word::Text);
        if held_as_headers {
            if let Some(key) = ascii_slice_header(state, text, from, to) {
                return set_words(state, dict, &key, value.cast_unsigned());
            }
            let (bytes, chars) = slice_of(state, text, from, to)?;
            let key = new_key(state, Word::Text, (&bytes, chars));
            return set_words(state, dict, &key, value.cast_unsigned());
        }
        let key = slice_key(state, text, from, to)?;
        set_in(state, dict, key, value)
    })
    .unwrap_or(dict)
}

/// The inline header of `text[from:to]`, for an ASCII string whose slice fits in one, made
/// from the few words of the string that hold it.
fn ascii_slice_header(state: &State, text: i64, from: i64, to: i64) -> Option<[u64; 3]> {
    let block = state.heap.block(u64::try_from(text).ok()?)?;
    let (length, chars) = (*block.get(2)?, *block.get(3)?);
    if !super::heap::is_text(state, block) || length != chars {
        return None;
    }
    let count = i64::try_from(chars).ok()?;
    let bound = |at: i64, missing: i64| {
        let at = if at == i64::MIN { missing } else { at };
        let at = if at < 0 { at + count } else { at };
        usize::try_from(at.clamp(0, count)).unwrap_or(0)
    };
    let (from, to) = (bound(from, 0), bound(to, count));
    let size = to.max(from) - from;
    if size > super::text::INLINE_BYTES {
        return None;
    }
    let mut bytes = [0_u8; 40];
    let words = block.get(TEXT_HEADER_WORDS + from / 8..TEXT_HEADER_WORDS + (from + size).div_ceil(8))?;
    for (at, word) in words.iter().enumerate() {
        bytes.get_mut(at * 8..at * 8 + 8)?.copy_from_slice(&word.to_le_bytes());
    }
    Some(super::text::inline(bytes.get(from % 8..from % 8 + size)?))
}

/// A new string owned once of the characters `from..to` of a string, as `text[from:to]`
/// gives them. An ASCII string's bytes are copied a word at a time.
fn slice_key(state: &mut State, text: i64, from: i64, to: i64) -> Option<i64> {
    let address = u64::try_from(text).ok()?;
    let block = state.heap.block(address)?;
    let (length, chars) = (*block.get(2)?, *block.get(3)?);
    if super::heap::is_text(state, block) && length == chars {
        let count = i64::try_from(chars).ok()?;
        let bound = |at: i64, missing: i64| {
            let at = if at == i64::MIN { missing } else { at };
            let at = if at < 0 { at + count } else { at };
            usize::try_from(at.clamp(0, count)).unwrap_or(0)
        };
        let (from, to) = (bound(from, 0), bound(to, count));
        let to = to.max(from);
        let kept = u64::try_from(to - from).ok()?;
        return super::strings::ascii_part_of(state, address, (from, to), kept);
    }
    let (bytes, chars) = slice_of(state, text, from, to)?;
    Some(text_from_bytes(state, &bytes, chars))
}

/// A string key given as its header (see `super::text`): its bytes and how many characters
/// they are, read from the header itself or from its block, with no block made.
fn header_text(state: &State, header: &[u64; 3]) -> Option<(Vec<u8>, usize)> {
    if let Some(bytes) = super::text::inline_bytes(header) {
        let chars = String::from_utf8_lossy(&bytes).chars().count();
        return Some((bytes, chars));
    }
    let block = state.heap.block(super::text::block_of(header)?)?;
    let length = usize::try_from(*block.get(2)?).ok()?;
    let chars = usize::try_from(*block.get(3)?).ok()?;
    let words = block.get(TEXT_HEADER_WORDS..)?;
    Some((words.iter().flat_map(|word| word.to_le_bytes()).take(length).collect(), chars))
}

/// `dict.get(key)` for a dict of string keys, the key given as its header's words: the
/// address of the key's value word, to read straight away, or 0 if the key isn't there. No
/// block is made for a key held in its header.
pub extern "C" fn find_header(dict: i64, a: u64, b: u64, c: u64) -> i64 {
    with(|state| {
        let block = state.heap.block(u64::try_from(dict).ok()?)?;
        let layout = layout(state, block)?;
        if layout.key == Word::Text {
            let key = [a, b, c];
            let Probe::Found(entry) = find(state, block, layout, &key, hash_key(state, Word::Text, &key))
            else {
                return Some(0);
            };
            let word = value_place(block, layout, entry)?;
            return i64::try_from(block.as_ptr().addr() + word * 8).ok();
        }
        let (bytes, chars) = header_text(state, &[a, b, c])?;
        if layout.key != Word::Str {
            return None;
        }
        let (Probe::Found(entry), _) = find_text(state, block, (&bytes, chars))? else { return Some(0) };
        let word = value_place(block, layout, entry)?;
        i64::try_from(block.as_ptr().addr() + word * 8).ok()
    })
    .unwrap_or(0)
}

/// `dict[key] = value` for a dict of string keys, the key given as its header's words: takes
/// over the owners `dict`, `value` and a key in a block had, like `set`. A block is made for a
/// key held in its header only when the key is new.
pub extern "C" fn set_header(dict: i64, a: u64, b: u64, c: u64, value: i64) -> i64 {
    with(|state| {
        let header = [a, b, c];
        let at = u64::try_from(dict).ok()?;
        let block = state.heap.block(at)?;
        let (key_word, value_word) = kind(state, block)?;
        if key_word == Word::Text {
            return set_words(state, dict, &header, value.cast_unsigned());
        }
        let (bytes, chars) = header_text(state, &header)?;
        if key_word != Word::Str {
            return None;
        }
        let owned = super::text::block_of(&header);
        if let (Some(&1), (Probe::Found(entry), _)) =
            (block.first(), find_text(state, block, (&bytes, chars))?)
        {
            let word = value_place(block, layout(state, block)?, entry)?;
            let old = std::mem::replace(state.heap.block_mut(at)?.get_mut(word)?, value.cast_unsigned());
            if value_word.owned() {
                release(state, old);
            }
            if let Some(key) = owned {
                release(state, key);
            }
            return Some(dict);
        }
        let key = match owned {
            Some(key) => key.cast_signed(),
            None => text_from_bytes(state, &bytes, chars),
        };
        set_in(state, dict, key, value)
    })
    .unwrap_or(dict)
}

/// `dict.pop(key)` for a dict of string keys, the key given as its header's words, as `pop`
/// does it. The key's owner stays with the caller.
pub extern "C" fn pop_header(dict: i64, a: u64, b: u64, c: u64, start: i64, end: i64) -> i64 {
    let held_as_headers = with(|state| {
        let block = state.heap.block(u64::try_from(dict).unwrap_or(0));
        block.and_then(|block| kind(state, block)).is_some_and(|(key, _)| key == Word::Text)
    });
    if held_as_headers {
        return pop_words(dict, &[a, b, c], (start, end));
    }
    let key = super::text::block(a, b, c);
    let popped = pop(dict, key, start, end);
    with(|state| release(state, key.cast_unsigned()));
    popped
}

/// A new empty dict of a shape, owned once, with room for `room` keys.
pub extern "C" fn new(shape: i64, room: i64) -> i64 {
    with(|state| {
        let shape = u64::try_from(shape).unwrap_or(0);
        let width = shape_layout(state, shape).map_or(ENTRY_WORDS, |layout| layout.width);
        let block = build(state, shape, width, &[], usize::try_from(room).unwrap_or(0));
        state.heap.insert(block)
    })
}

/// `dict[key] = value`: takes over the owners `dict`, `key` and `value` had, and gives back
/// the dict's. A dict with other owners is copied first, and a full one grows.
pub extern "C" fn set(dict: i64, key: i64, value: i64) -> i64 {
    with(|state| set_in(state, dict, key, value).unwrap_or(dict))
}

fn set_in(state: &mut State, dict: i64, key: i64, value: i64) -> Option<i64> {
    set_words(state, dict, &[key.cast_unsigned()], value.cast_unsigned())
}

/// `dict[key] = value` with the key's words: takes over the owners `dict`, a key held in a
/// block and `value` had, and gives back the dict's.
fn set_words(state: &mut State, dict: i64, key: &[u64], value: u64) -> Option<i64> {
    let mut at = u64::try_from(dict).ok()?;
    let block = state.heap.block(at)?;
    let layout = layout(state, block)?;
    // A key of the wrong width changes nothing: it's refused before the dict grows or a slot
    // is taken.
    if key.len() + 2 != layout.width {
        return None;
    }
    let hash = hash_key(state, layout.key, key);
    let slots = usize::try_from(*block.get(4)?).ok()? + 1;
    let used = usize::try_from(*block.get(3)?).ok()?;
    let shared = !state.heap.is_alone(at, block);
    if shared || used >= capacity(slots) {
        let (shape, live) = (shape_word(block)?, entries(block, layout.width));
        if shared {
            state.heap.copied(live.len());
            for entry in live.chunks_exact(layout.width) {
                retain_entry(state, layout, entry);
            }
            release(state, at);
        } else {
            state.heap.discard(at);
        }
        let grown = build(state, shape, layout.width, &live, 2 * (live.len() / layout.width + 1));
        at = u64::try_from(state.heap.insert(grown)).ok()?;
    }
    let block = state.heap.block(at)?;
    let mask = usize::try_from(*block.get(4)?).ok()?;
    let start = DICT_HEADER_WORDS + mask + 1;
    let probe = find(state, block, layout, key, hash);
    let block = state.heap.block_mut(at)?;
    match probe {
        Probe::Found(entry) => {
            let slot = block.get_mut(start + entry * layout.width + layout.value_at())?;
            let old = std::mem::replace(slot, value);
            if layout.value.owned() {
                release(state, old);
            }
            if let Some(address) = key_block(layout.key, key) {
                release(state, address);
            }
        }
        Probe::Empty(slot) => {
            let used = usize::try_from(*block.get(3)?).ok()?;
            *block.get_mut(DICT_HEADER_WORDS + slot)? = u64::try_from(used + 1).ok()?;
            let entry = start + used * layout.width;
            let words = block.get_mut(entry..entry + layout.width)?;
            let (hash_word, rest) = words.split_first_mut()?;
            let (value_word, key_words) = rest.split_last_mut()?;
            *hash_word = hash;
            *value_word = value;
            key_words.copy_from_slice(key);
            *block.get_mut(2)? += 1;
            *block.get_mut(3)? += 1;
        }
    }
    i64::try_from(at).ok()
}

fn retain_entry(state: &mut State, layout: Layout, entry: &[u64]) {
    let held = [key_block(layout.key, key_of(entry)), layout.value.owned().then(|| value_of(entry))];
    for address in held.into_iter().flatten() {
        super::heap::retain(state, address);
    }
}

/// A block only one owner holds, taking over the owner `at` had.
fn unshared(state: &mut State, at: u64) -> Option<u64> {
    let block = state.heap.block(at)?;
    if state.heap.is_alone(at, block) {
        return Some(at);
    }
    let layout = layout(state, block)?;
    let (shape, live) = (shape_word(block)?, entries(block, layout.width));
    state.heap.copied(live.len());
    for entry in live.chunks_exact(layout.width) {
        retain_entry(state, layout, entry);
    }
    release(state, at);
    let copy = build(state, shape, layout.width, &live, live.len() / layout.width);
    u64::try_from(state.heap.insert(copy)).ok()
}

/// What taking a key out of a dict or set found.
enum Taken {
    /// It was there, with this value, whose owner goes to the caller.
    Removed(u64),
    Missing,
    /// What it was given isn't a dict or set.
    NotOne,
}

/// Takes a key, given as its words, out of a dict or set already owned once.
fn take_key(state: &mut State, at: u64, key: &[u64]) -> Taken {
    take(state, at, key).unwrap_or(Taken::NotOne)
}

fn take(state: &mut State, at: u64, key: &[u64]) -> Option<Taken> {
    let block = state.heap.block(at)?;
    let layout = layout(state, block)?;
    let hash = hash_key(state, layout.key, key);
    let mask = usize::try_from(*block.get(4)?).ok()?;
    let Probe::Found(entry) = find(state, block, layout, key, hash) else { return Some(Taken::Missing) };
    let place = DICT_HEADER_WORDS + mask + 1 + entry * layout.width;
    let removed = block.get(place..place + layout.width)?;
    let (held, value) = (key_block(layout.key, key_of(removed)), value_of(removed));
    // The whole entry is cleared, so nothing reads a key it no longer owns.
    state.heap.block_mut(at)?.get_mut(place..place + layout.width)?.fill(0);
    let block = state.heap.block_mut(at)?;
    *block.get_mut(place)? = REMOVED;
    *block.get_mut(2)? -= 1;
    if let Some(address) = held {
        release(state, address);
    }
    Some(Taken::Removed(value))
}

/// `set.discard(key)`, or `set.remove(key)` when `must`, failing like the interpreter for a
/// key it doesn't have. Takes over the set's owner and gives back the set's.
pub extern "C" fn set_remove(set: i64, key: i64, must: i8, start: i64, end: i64) -> i64 {
    remove_words(set, &[key.cast_unsigned()], must, (start, end))
}

/// `set_remove` for a set of string keys held as headers, the key given as its header's words.
/// The key's owner stays with the caller.
pub extern "C" fn set_remove_header(set: i64, a: u64, b: u64, c: u64, must: i8, start: i64, end: i64) -> i64 {
    remove_words(set, &[a, b, c], must, (start, end))
}

fn remove_words(set: i64, key: &[u64], must: i8, (start, end): (i64, i64)) -> i64 {
    attempt(|state: &mut State| {
        let at = u64::try_from(set).ok().and_then(|at| unshared(state, at)).unwrap_or(0);
        match take_key(state, at, key) {
            Taken::Missing if must != 0 => {
                let key_word = state
                    .heap
                    .block(at)
                    .and_then(|block| kind(state, block))
                    .map_or(Word::Int, |kind| kind.0);
                let value = key_value(state, key_word, key);
                let message = format!("{} isn't in the set", crate::format::repr(&value));
                Err(Diagnostic::new(message, span(start, end)))
            }
            Taken::NotOne => Ok(set),
            Taken::Removed(_) | Taken::Missing => Ok(i64::try_from(at).unwrap_or(0)),
        }
    })
}

thread_local! {
    /// The value the last `pop` took out of a dict, for native code to pick up.
    static POPPED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// `dict.pop(key)`: takes over the dict's owner and gives back the dict's, copying it first if
/// it's shared. The value taken out is picked up straight after with `popped`, whose owner goes
/// with it. A key the dict doesn't have fails as the interpreter does.
pub extern "C" fn pop(dict: i64, key: i64, start: i64, end: i64) -> i64 {
    pop_words(dict, &[key.cast_unsigned()], (start, end))
}

fn pop_words(dict: i64, key: &[u64], (start, end): (i64, i64)) -> i64 {
    attempt(|state: &mut State| {
        let at = u64::try_from(dict).ok().and_then(|at| unshared(state, at)).unwrap_or(0);
        match take_key(state, at, key) {
            Taken::Removed(value) => {
                POPPED.set(value);
                Ok(i64::try_from(at).unwrap_or(0))
            }
            Taken::Missing => {
                let key_word = state
                    .heap
                    .block(at)
                    .and_then(|block| kind(state, block))
                    .map_or(Word::Int, |kind| kind.0);
                let value = key_value(state, key_word, key);
                Err(Diagnostic::new(
                    format!("the dict has no key {}", crate::format::repr(&value)),
                    span(start, end),
                ))
            }
            Taken::NotOne => Ok(dict),
        }
    })
}

/// The value the last `pop` took out, as a word, with its owner.
pub extern "C" fn popped() -> i64 {
    POPPED.replace(0).cast_signed()
}

/// `set.clear()`: takes over the set's owner and gives back an empty set's.
pub extern "C" fn set_clear(set: i64) -> i64 {
    with(|state| {
        let at = u64::try_from(set).unwrap_or(0);
        let shape = state.heap.block(at).and_then(shape_word).unwrap_or(0);
        release(state, at);
        let width = shape_layout(state, shape).map_or(ENTRY_WORDS, |layout| layout.width);
        let empty = build(state, shape, width, &[], 0);
        state.heap.insert(empty)
    })
}

/// Fails with the interpreter's message for a key a dict doesn't have: reading it, or, for
/// `augmented`, changing it with `+=` and the like.
pub extern "C" fn missing(dict: i64, key: i64, augmented: i8, start: i64, end: i64) {
    missing_words(dict, &[key.cast_unsigned()], augmented, (start, end));
}

/// `missing` for a dict of string keys held as headers, the key given as its header's words.
pub extern "C" fn missing_header(dict: i64, a: u64, b: u64, c: u64, augmented: i8, start: i64, end: i64) {
    missing_words(dict, &[a, b, c], augmented, (start, end));
}

fn missing_words(dict: i64, key: &[u64], augmented: i8, (start, end): (i64, i64)) {
    attempt(|state: &mut State| -> Result<(), Diagnostic> {
        let key_word = state
            .heap
            .block(u64::try_from(dict).unwrap_or(0))
            .and_then(|block| kind(state, block))
            .map_or(Word::Int, |kind| kind.0);
        let key = key_value(state, key_word, key);
        let message = format!("the dict has no key {}", crate::format::repr(&key));
        let failure = Diagnostic::new(message, span(start, end));
        Err(if augmented == 0 {
            failure.with_help("use `.get(key)`, which gives None instead")
        } else {
            failure
        })
    });
}

/// The addresses of the strings a dict's block owns.
pub(super) fn children(state: &State, block: &[u64]) -> Vec<u64> {
    let Some(layout) = layout(state, block) else { return Vec::new() };
    let mut found = Vec::new();
    for entry in entries(block, layout.width).chunks_exact(layout.width) {
        found.extend(key_block(layout.key, key_of(entry)));
        found.extend(layout.value.owned().then(|| value_of(entry)));
    }
    found
}

/// A dict's block as the interpreter holds a dict.
pub(super) fn read(state: &State, block: &[u64]) -> Value {
    let Some(layout) = layout(state, block) else { return Value::None };
    if layout.value == Word::Nothing {
        return read_set(state, layout, block);
    }
    let pairs: Vec<_> = entries(block, layout.width)
        .chunks_exact(layout.width)
        .map(|entry| {
            (key_value(state, layout.key, key_of(entry)), word_value(state, layout.value, value_of(entry)))
        })
        .collect();
    Value::Dict(std::rc::Rc::new(pairs.into()))
}

/// Writes a dict or a set as it shows, in the order its entries were added.
pub(super) fn shown(state: &State, block: &[u64], out: &mut Written) {
    let Some(layout) = layout(state, block) else {
        out.push_str("None");
        return;
    };
    let is_set = layout.value == Word::Nothing;
    let held = entries(block, layout.width);
    if is_set && held.is_empty() {
        out.push_str("set()");
        return;
    }
    out.push('{');
    for (at, entry) in held.chunks_exact(layout.width).enumerate() {
        if at > 0 {
            out.push_str(", ");
        }
        word_shown(state, layout.key, key_of(entry), out);
        if !is_set {
            out.push_str(": ");
            word_shown(state, layout.value, &[value_of(entry)], out);
        }
    }
    out.push('}');
}

fn word_shown(state: &State, word: Word, words: &[u64], out: &mut Written) {
    let element = match word {
        Word::Int => Element::Int,
        Word::Float => Element::Float,
        Word::Bool => Element::Bool,
        Word::Text => Element::Text,
        Word::Str | Word::Plain(_) | Word::Block(_) | Word::Nothing => Element::Str,
    };
    super::heap::shown::item(state, &element, &mut words.iter(), out);
}

/// A set's block as the interpreter holds a set.
fn read_set(state: &State, layout: Layout, block: &[u64]) -> Value {
    let items = entries(block, layout.width)
        .chunks_exact(layout.width)
        .map(|entry| key_value(state, layout.key, key_of(entry)))
        .collect();
    Value::Set(std::rc::Rc::new(items))
}

/// A key's words as the interpreter holds the key.
fn key_value(state: &State, word: Word, key: &[u64]) -> Value {
    if let (Word::Text, Ok(&[a, b, c])) = (word, <&[u64; 3]>::try_from(key)) {
        return Value::str(&super::text::text_of(state, &[a, b, c]));
    }
    word_value(state, word, key.first().copied().unwrap_or(0))
}

fn word_value(state: &State, word: Word, bits: u64) -> Value {
    match word {
        Word::Int => Value::Int(i64::from_ne_bytes(bits.to_ne_bytes())),
        Word::Float => Value::Float(f64::from_bits(bits)),
        Word::Bool => Value::Bool(bits != 0),
        Word::Str | Word::Plain(_) | Word::Block(_) => super::heap::read_block(state, bits),
        Word::Nothing | Word::Text => Value::Unit,
    }
}

/// A new block owned once holding a dict the interpreter holds, if every key and value is of
/// the kind the shape says.
pub(super) fn write(state: &mut State, shape: u64, value: &Value) -> Option<i64> {
    let layout = shape_layout(state, shape)?;
    let (key_word, value_word) = (layout.key, layout.value);
    let set: Vec<(Value, Value)>;
    let pairs = match value {
        Value::Dict(pairs) if value_word != Word::Nothing => &pairs[..],
        Value::Set(items) if value_word == Word::Nothing => {
            set = items.iter().map(|item| (item.clone(), Value::Unit)).collect();
            &set
        }
        _ => return None,
    };
    let words = |state: &mut State, word: Word, value: &Value| -> Option<u64> {
        Some(match (word, value) {
            (Word::Int, Value::Int(n)) => u64::from_ne_bytes(n.to_ne_bytes()),
            (Word::Float, Value::Float(x)) => x.to_bits(),
            (Word::Bool, Value::Bool(b)) => u64::from(*b),
            (Word::Str, Value::Str(text)) => u64::try_from(text_address(state, text)).ok()?,
            (Word::Nothing, Value::Unit) => 0,
            (Word::Plain(_), Value::Instance(instance)) => {
                let shape = super::heap::shape_named(state, &instance.type_name)?;
                u64::try_from(super::heap::write_block(state, shape, value)?).ok()?
            }
            (Word::Block(number), value) => {
                let element = state.tables.blocks.get(usize::try_from(number).ok()?).cloned()?;
                let mut held = Vec::with_capacity(1);
                super::heap::write(state, &element, value, &mut held)?;
                *held.first()?
            }
            _ => return None,
        })
    };
    let mut entries = Vec::with_capacity(pairs.len() * layout.width);
    for (key, value) in pairs {
        let key = match (key_word, key) {
            (Word::Text, Value::Str(text)) => Some(super::text::header(state, text).to_vec()),
            _ => words(state, key_word, key).map(|key| vec![key]),
        };
        let (Some(key), Some(value)) = (key, words(state, value_word, value)) else {
            for entry in entries.chunks_exact(layout.width) {
                release_entry(state, layout, entry);
            }
            return None;
        };
        entries.push(hash_key(state, key_word, &key));
        entries.extend(key);
        entries.push(value);
    }
    let block = build(state, shape, layout.width, &entries, pairs.len());
    Some(state.heap.insert(block))
}

fn release_entry(state: &mut State, layout: Layout, entry: &[u64]) {
    let held = [key_block(layout.key, key_of(entry)), layout.value.owned().then(|| value_of(entry))];
    for address in held.into_iter().flatten() {
        release(state, address);
    }
}

/// Whether two dicts' blocks hold the same keys with equal values, in any order, as the
/// interpreter compares dicts.
pub(super) fn equal(state: &State, left: &[u64], right: &[u64]) -> bool {
    let (Some(layout), theirs) = (layout(state, left), layout(state, right)) else { return false };
    // Dicts of the same type always share a layout, so differing ones are never equal.
    if theirs != Some(layout) {
        return false;
    }
    let (ours, theirs) = (entries(left, layout.width), entries(right, layout.width));
    ours.len() == theirs.len()
        && ours.chunks_exact(layout.width).all(|entry| {
            let (hash, value) = (entry.first().copied().unwrap_or(0), value_of(entry));
            match find(state, right, layout, key_of(entry), hash) {
                Probe::Found(at) => value_place(right, layout, at)
                    .and_then(|place| right.get(place))
                    .is_some_and(|other| match layout.value {
                        Word::Float => {
                            crate::value::floats_same(f64::from_bits(value), f64::from_bits(*other))
                        }
                        Word::Str | Word::Plain(_) | Word::Block(_) => same(state, value, *other),
                        Word::Int | Word::Bool | Word::Nothing | Word::Text => value == *other,
                    }),
                Probe::Empty(_) => false,
            }
        })
}
