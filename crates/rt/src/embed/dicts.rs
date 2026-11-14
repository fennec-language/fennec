//! Dicts for native code. A dict is a block in the heap (see `heap`) holding a hash table:
//! its owner count, its shape, how many keys it has, how many entries it has used, one
//! less than its number of slots, and how its keys are hashed. Then the slots, each 0 for empty or an entry's number
//! plus one, then the entries in the order their keys were added: each a key's hash, the
//! key and its value. Native code looks keys up, changes values and adds keys itself while
//! the dict has one owner and room; everything else happens here.
//!
//! Keys are ints, strings or plain structs, a plain struct key being the address of a block of
//! its own. Values are ints, floats, bools or strings, each one word. Which
//! ones a dict holds is in its shape: the shapes after the string shape, one for each pair.
//! A set is a dict whose values are nothing.

use std::collections::hash_map::{DefaultHasher, RandomState};
use std::hash::{BuildHasher as _, Hasher as _};
use std::sync::OnceLock;

use fennec_syntax::Diagnostic;

use super::heap::shown::Written;
use super::heap::{
    HEADER_WORDS, TEXT_HEADER_WORDS, release, same, shape_word, text_address, text_from_bytes,
};
use super::{State, attempt, span, with};
use crate::packed::Element;
use crate::value::Value;

/// Words a dict's block starts with: owner count, shape, keys, entries used, slots less one, and
/// how its keys are hashed.
pub const DICT_HEADER_WORDS: usize = 6;

/// The word of a dict's header saying how its keys are hashed: 0 fast, 1 strong.
const HOW_HASHED: usize = 5;

/// How far a key may land from its first slot before a dict stops trusting its fast hash. Linear
/// probing at this load doesn't get near it by chance, and reaching it costs an attacker's keys
/// about half a million comparisons, once. Native code stops adding keys at the same distance.
pub const LONGEST_PROBE: usize = 1024;

/// Words an entry takes: hash, key, value.
pub const ENTRY_WORDS: usize = 3;

/// The fewest slots a table has.
const MIN_SLOTS: usize = 8;

/// The hash a removed entry is given. Real hashes never have their top bit set.
const REMOVED: u64 = u64::MAX;

const MIX: u64 = 0x9E37_79B9_7F4A_7C15;

/// How a dict's keys are hashed. Every dict starts fast, and one whose probes get long changes
/// to strong, which only that dict pays for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hashing {
    Fast,
    Strong,
}

impl Hashing {
    fn of(block: &[u64]) -> Hashing {
        if block.get(HOW_HASHED) == Some(&1) { Hashing::Strong } else { Hashing::Fast }
    }

    fn word(self) -> u64 {
        u64::from(self == Hashing::Strong)
    }
}

/// What this process hashes keys with.
struct HashKeys {
    seed: u64,
    /// The strong hash's key. `None` for a fixed seed, when the strong hash has a fixed key too.
    strong: Option<RandomState>,
}

static HASH_KEYS: OnceLock<HashKeys> = OnceLock::new();

fn hash_keys() -> &'static HashKeys {
    HASH_KEYS.get_or_init(|| {
        if let Some(seed) = fixed_seed() {
            return HashKeys { seed, strong: None };
        }
        let strong = RandomState::new();
        HashKeys { seed: strong.hash_one(0_u64), strong: Some(strong) }
    })
}

/// The seed for a run that wants the same one each time: a test, or a replay. Keys are iterated
/// in the order they were added, so no seed changes what a program does.
fn fixed_seed() -> Option<u64> {
    if cfg!(test) {
        return Some(0);
    }
    let given = std::env::var("FENNEC_HASH_SEED").ok().and_then(|seed| seed.parse().ok());
    given.or_else(|| crate::replay::active().then_some(0))
}

/// `fennec_dict_hash_seed()`: the seed native code hashes keys with, which it reads once.
pub extern "C" fn hash_seed() -> u64 {
    hash_keys().seed
}

/// `fennec_dict_strong_step(hash, word)`: one step of the strong hash, `SipHash` under this
/// process's key. Native code hashes a key for a strong dict by calling this for each word.
pub extern "C" fn strong_step(hash: u64, word: u64) -> u64 {
    let mut hasher = hash_keys().strong.as_ref().map_or_else(DefaultHasher::new, RandomState::build_hasher);
    hasher.write_u64(hash);
    hasher.write_u64(word);
    hasher.finish()
}

/// The strong hash of a key: its length or count, then its words. Native code works it out the
/// same way, so this mustn't change without changing it.
fn strong_hash(count: u64, words: &[u64]) -> u64 {
    words.iter().fold(strong_step(0, count), |hash, word| strong_step(hash, *word)) >> 1
}

/// A hash for text, from its length in bytes and its words.
fn text_hash(how: Hashing, length: u64, words: &[u64]) -> u64 {
    match how {
        Hashing::Fast => hash_text(length, words),
        Hashing::Strong => strong_hash(length, words),
    }
}

/// Whether a key that hashes to `hash` has landed too far from its first slot, `slot`.
fn too_far(hash: u64, slot: usize, mask: usize) -> bool {
    slot.wrapping_sub(usize::try_from(hash).unwrap_or(0)) & mask > LONGEST_PROBE
}

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
    /// A `u64`, its bits in the word: hashed and compared as an int is, and shown unsigned.
    U64,
    /// An `f32`, as the `f64` that's exactly its value: hashed and compared as a float is, and
    /// shown as the shortest text that reads back as that `f32`.
    F32,
    /// As a value, one held in the entry itself, as the words a list holds an item of its kind
    /// as: by the kind's number in `Tables::elements`, which says how many words it takes and
    /// where among them it holds blocks. All zero holds nothing.
    InPlace(u32),
}

impl Word {
    /// Whether a word of this kind is the address of a block the dict owns.
    fn owned(self) -> bool {
        matches!(self, Word::Str | Word::Plain(_) | Word::Block(_))
    }

    /// Whether a value of this kind is one word that holds no block, so writing over one lets go
    /// of nothing: not a block's address, and not a value held in place.
    fn plain_word(self) -> bool {
        !matches!(self, Word::Str | Word::Plain(_) | Word::Block(_) | Word::InPlace(_))
    }
}

const KEYS: [Word; 7] = [Word::Int, Word::Str, Word::Plain(0), Word::Text, Word::Float, Word::U64, Word::F32];

/// Words an entry with a string header for its key takes: hash, the header's three, value.
const HEADER_ENTRY_WORDS: usize = 5;
const VALUES: [Word; 8] =
    [Word::Int, Word::Float, Word::Bool, Word::Str, Word::Nothing, Word::Plain(0), Word::U64, Word::F32];

/// The highest shape of a dict with `Word::Block` values, which count down from here, below the
/// shapes that aren't numbered from the string shape.
const TOP_BLOCK_DICT: u64 = super::heap::SHAPE_BITS - 16;
/// How many kinds of block a dict's values can be.
const BLOCK_KINDS: u64 = 1 << 20;

/// The highest shape of a dict with `Word::InPlace` values, which count down from here as those
/// with `Word::Block` values do: just below theirs.
const TOP_IN_PLACE_DICT: u64 = TOP_BLOCK_DICT - BLOCK_KINDS * KEYS.len() as u64;
/// How many kinds of value a dict can hold in place.
const IN_PLACE_KINDS: u64 = 1 << 19;
/// The shape just below every dict's that's counted down from the top.
const BELOW_COUNTED_DOWN: u64 = TOP_IN_PLACE_DICT - IN_PLACE_KINDS * KEYS.len() as u64;

/// How many shapes dicts take, after the string shape.
pub const SHAPES: u64 = (KEYS.len() * VALUES.len()) as u64;

/// The shape of dicts with these keys and values, counted from the string shape, or `None`
/// for a pair native dicts don't hold.
pub fn shape(str_shape: u64, key: Word, value: Word) -> Option<u64> {
    let plain = |word| if let Word::Plain(_) = word { Word::Plain(0) } else { word };
    let key = KEYS.iter().position(|word| *word == plain(key))?;
    let count = u64::try_from(KEYS.len()).ok()?;
    if let Word::Block(number) = value {
        return Some(TOP_BLOCK_DICT - u64::from(number) * count - u64::try_from(key).ok()?);
    }
    if let Word::InPlace(number) = value {
        let number = u64::from(number);
        return (number < IN_PLACE_KINDS)
            .then_some(TOP_IN_PLACE_DICT - number * count - u64::try_from(key).ok()?);
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
    /// How many words an entry takes, and where its value is from the entry's start: after its
    /// hash and its key. The value is the one word left, but for one held in place, which is as
    /// many as its kind takes. Both are small, and kept in the room one word had, so a layout
    /// costs what it did when every value was a word.
    wide: u32,
    value_from: u32,
}

impl Layout {
    /// The layout of entries `width` words wide whose value is the last `value_words` of them.
    fn of(key: Word, value: Word, width: usize, value_words: usize) -> Option<Layout> {
        let wide = u32::try_from(width).ok()?;
        let value_from = u32::try_from(width.checked_sub(value_words)?).ok()?;
        Some(Layout { key, value, wide, value_from })
    }

    /// How many words an entry takes.
    fn width(self) -> usize {
        self.wide as usize
    }

    /// Where an entry's value is, from the entry's start.
    fn value_at(self) -> usize {
        self.value_from as usize
    }

    /// How many words the value takes.
    fn value_words(self) -> usize {
        (self.wide - self.value_from) as usize
    }

    /// Whether the value is held in the entry as its own words (`Word::InPlace`).
    fn in_place(self) -> bool {
        matches!(self.value, Word::InPlace(_))
    }
}

/// How a dict's block lays out its entries, when it is a dict's.
fn layout(state: &State, block: &[u64]) -> Option<Layout> {
    shape_layout(state, shape_word(block)?)
}

/// How a dict of this shape lays out its entries, when the shape is a dict's.
fn shape_layout(state: &State, shape: u64) -> Option<Layout> {
    // The shapes counted down from the top, of dicts of blocks and of values in place, are all
    // above this one: every other dict's is told from them in one test.
    if shape > BELOW_COUNTED_DOWN {
        return counted_down_layout(state, shape);
    }
    let at = usize::try_from(shape.checked_sub(state.tables.str_shape + 1)?).ok()?;
    let (key, value) = (*KEYS.get(at / VALUES.len())?, *VALUES.get(at % VALUES.len())?);
    // The two widths an entry of one-word values has, each with where its value is.
    let (wide, value_from) = if key == Word::Text { (5, 4) } else { (3, 2) };
    Some(Layout { key, value, wide, value_from })
}

/// `shape_layout` for a dict of blocks or of values in place.
fn counted_down_layout(state: &State, shape: u64) -> Option<Layout> {
    let (key, value) = shape_kinds(state.tables.str_shape, shape)?;
    let width = if key == Word::Text { HEADER_ENTRY_WORDS } else { ENTRY_WORDS };
    let Word::InPlace(_) = value else { return Layout::of(key, value, width, 1) };
    // A value in place is as wide as its kind of item is in a list. One of no words is no value
    // a dict can find again.
    let value_words =
        value_element(state, value).map(|element| element.words()).filter(|words| *words > 0)?;
    Layout::of(key, value, width - 1 + value_words, value_words)
}

/// What the keys and values of a dict of this shape are, when the shape is a dict's.
fn shape_kinds(str_shape: u64, shape: u64) -> Option<(Word, Word)> {
    let count = u64::try_from(KEYS.len()).ok()?;
    if shape > BELOW_COUNTED_DOWN {
        if shape > TOP_BLOCK_DICT {
            return None;
        }
        let (top, in_place) =
            if shape > TOP_IN_PLACE_DICT { (TOP_BLOCK_DICT, false) } else { (TOP_IN_PLACE_DICT, true) };
        let at = top - shape;
        let key = *KEYS.get(usize::try_from(at % count).ok()?)?;
        let number = u32::try_from(at / count).ok()?;
        return Some((key, if in_place { Word::InPlace(number) } else { Word::Block(number) }));
    }
    let at = usize::try_from(shape.checked_sub(str_shape + 1)?).ok()?;
    Some((*KEYS.get(at / VALUES.len())?, *VALUES.get(at % VALUES.len())?))
}

/// The kind of item a value held in place is laid out as.
fn value_element(state: &State, value: Word) -> Option<std::rc::Rc<Element>> {
    let Word::InPlace(number) = value else { return None };
    state.tables.elements.get(usize::try_from(number).ok()?).cloned()
}

/// An entry's key: its words between the hash and the value.
fn key_of(layout: Layout, entry: &[u64]) -> &[u64] {
    entry.get(1..layout.value_at()).unwrap_or_default()
}

/// An entry's value, when it's one word: the entry's last.
fn value_of(entry: &[u64]) -> u64 {
    entry.last().copied().unwrap_or(0)
}

/// An entry's value as its words: one, or as many as a value held in place takes.
fn value_words(layout: Layout, entry: &[u64]) -> &[u64] {
    entry.get(layout.value_at()..).unwrap_or_default()
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

/// Whether a block is a set's: a dict's whose keys have nothing for values.
#[cfg(any(test, feature = "audit"))]
pub(super) fn is_set(state: &State, block: &[u64]) -> bool {
    kind(state, block).is_some_and(|(_, value)| value == Word::Nothing)
}

/// How many keys a table with `slots` slots holds before it grows.
pub fn capacity(slots: usize) -> usize {
    slots - slots / 4
}

/// An int key's hash. Native code works it out the same way.
pub fn hash_int(key: u64) -> u64 {
    finish((key ^ hash_keys().seed).wrapping_mul(MIX))
}

/// A string key's hash, from its length in bytes and its words of text: its bytes packed
/// little-endian eight to a word, from its first byte, the last word padded with zero bytes,
/// as its block holds them. Native code works it out the same way, for a slice of a string too,
/// so this mustn't change without changing them.
pub fn hash_text(length: u64, words: &[u64]) -> u64 {
    finish(words.iter().fold((length ^ hash_keys().seed).wrapping_mul(MIX), |hash, word| {
        (hash.rotate_left(23) ^ word).wrapping_mul(MIX)
    }))
}

/// A plain struct key's hash, from its words. Native code works it out the same way.
pub fn hash_words(words: &[u64]) -> u64 {
    let count = u64::try_from(words.len()).unwrap_or(0);
    finish(words.iter().fold((count ^ hash_keys().seed).wrapping_mul(MIX), |hash, word| {
        (hash.rotate_left(23) ^ word).wrapping_mul(MIX)
    }))
}

fn finish(hash: u64) -> u64 {
    (hash ^ (hash >> 29)) >> 1
}

/// Whether a plain key is hashed by what it holds rather than by its words.
pub(super) fn by_what_it_holds(element: &Element) -> bool {
    match element {
        Element::Float | Element::F32 => true,
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
        let hash = hash_key(state, layout.key, &word, Hashing::of(block));
        match find(state, block, layout, &word, hash) {
            Probe::Found(entry) => {
                let mask = block.get(4).and_then(|mask| usize::try_from(*mask).ok()).unwrap_or(0);
                let at = DICT_HEADER_WORDS + mask + 1 + entry * layout.width();
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

fn hash_key(state: &State, key_word: Word, key: &[u64], how: Hashing) -> u64 {
    let first = key.first().copied().unwrap_or(0);
    let one_word = |word: u64| match how {
        Hashing::Fast => hash_int(word),
        Hashing::Strong => strong_hash(1, &[word]),
    };
    if matches!(key_word, Word::Float | Word::F32) {
        return one_word(float_key(first));
    }
    if let Word::Plain(_) = key_word {
        let block = state.heap.block(first).unwrap_or_default();
        // Blocks or floats in a key go by what they hold, not by their words: worked out for each
        // of the program's shapes as it starts.
        let known = shape_word(block)
            .and_then(|shape| state.keys_by_value.get(usize::try_from(shape).ok()?))
            .copied();
        let by_value = known.unwrap_or_else(|| {
            super::heap::element_of_block(state, block).is_some_and(|element| by_what_it_holds(&element))
        });
        if by_value {
            let value = super::heap::read_block(state, first);
            return match how {
                Hashing::Fast => {
                    finish(crate::pairs::hash_with(hash_keys().seed, crate::pairs::mixed, &value))
                }
                Hashing::Strong => {
                    crate::pairs::hash_with(0, |hash, word| strong_step(hash, word), &value) >> 1
                }
            };
        }
        // Bytes go by their length and the words it takes up, as they're compared, not by any
        // room after them.
        let bytes;
        let words = if shape_word(block) == Some(super::heap::BYTES_SHAPE) {
            bytes = super::bytes::words_of(block);
            &bytes[..]
        } else {
            block.get(HEADER_WORDS..).unwrap_or_default()
        };
        return match how {
            Hashing::Fast => hash_words(words),
            Hashing::Strong => strong_hash(u64::try_from(words.len()).unwrap_or(0), words),
        };
    }
    match key_word {
        Word::Str => hash_block_text(state, first, how),
        Word::Text => hash_header(state, key, how),
        _ => one_word(first),
    }
}

/// A key held in a block, hashed both ways, as a dict would, for tests.
#[cfg(test)]
pub(super) fn block_key_hashes(state: &State, address: u64) -> (u64, u64) {
    let hashed = |how| hash_key(state, Word::Plain(0), &[address], how);
    (hashed(Hashing::Fast), hashed(Hashing::Strong))
}

/// The hash of the string in a block.
fn hash_block_text(state: &State, address: u64, how: Hashing) -> u64 {
    let block = state.heap.block(address).unwrap_or_default();
    let length = block.get(2).copied().unwrap_or(0);
    let words = usize::try_from(length).unwrap_or(0).div_ceil(8);
    text_hash(how, length, block.get(TEXT_HEADER_WORDS..TEXT_HEADER_WORDS + words).unwrap_or_default())
}

/// The hash of a string held as its header: an inline one's words are its bytes packed as a
/// block packs them, once byte 23 is cleared, so both forms hash alike.
fn hash_header(state: &State, key: &[u64], how: Hashing) -> u64 {
    let Ok(&[a, b, c]) = <&[u64; 3]>::try_from(key) else { return 0 };
    if let Some(address) = super::text::block_of(&[a, b, c]) {
        return hash_block_text(state, address, how);
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
    text_hash(how, length, words.get(..usize::try_from(length).unwrap_or(0).div_ceil(8)).unwrap_or_default())
}

fn keys_equal(state: &State, key_word: Word, a: &[u64], b: &[u64]) -> bool {
    match (a, b) {
        ([a], [b]) if matches!(key_word, Word::Float | Word::F32) => float_key(*a) == float_key(*b),
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

/// A block holding `entries`, each `width` words, owned once, with room for at least `room`
/// keys.
fn build(
    state: &mut State,
    (shape, how): (u64, Hashing),
    width: usize,
    entries: &[u64],
    room: usize,
) -> Box<[u64]> {
    let live = entries.len() / width;
    let (mut block, slots) = with_room(state, width, live, room);
    fill(state, &mut block, (shape, slots, how), width, entries);
    block
}

/// Fills in a new block, as `place_entries` does. Should a probe get too long, it starts over with
/// the strong hash.
fn fill(
    state: &State,
    block: &mut [u64],
    (shape, slots, how): (u64, usize, Hashing),
    width: usize,
    entries: &[u64],
) {
    if place_entries(block, (shape, slots, how), width, entries) {
        return;
    }
    let Some(layout) = shape_layout(state, shape) else { return };
    let strong = strongly_hashed(state, layout, entries.to_vec());
    if let Some(taken) = block.get_mut(DICT_HEADER_WORDS..DICT_HEADER_WORDS + slots) {
        taken.fill(0);
    }
    let placed = place_entries(block, (shape, slots, Hashing::Strong), width, &strong);
    debug_assert!(placed, "the strong hash places everything");
}

/// Entries with each key's hash changed to its strong one.
fn strongly_hashed(state: &State, layout: Layout, mut entries: Vec<u64>) -> Vec<u64> {
    for entry in entries.chunks_exact_mut(layout.width()) {
        let hash = hash_key(state, layout.key, key_of(layout, entry), Hashing::Strong);
        if let Some(first) = entry.first_mut() {
            *first = hash;
        }
    }
    entries
}

/// Changes a dict nothing else holds to the strong hash, in its own block.
fn strengthen(state: &mut State, at: u64, layout: Layout) -> Option<()> {
    let block = state.heap.block(at)?;
    let shape = shape_word(block)?;
    let slots = usize::try_from(*block.get(4)?).ok()? + 1;
    let live = entries(block, layout.width());
    let strong = strongly_hashed(state, layout, live);
    let block = state.heap.block_mut(at)?;
    block.get_mut(DICT_HEADER_WORDS..DICT_HEADER_WORDS + slots)?.fill(0);
    place_entries(block, (shape, slots, Hashing::Strong), layout.width(), &strong);
    Some(())
}

/// A dict's new block, not filled in yet, with room for at least `room` keys and all `live`.
fn with_room(state: &mut State, width: usize, live: usize, room: usize) -> (Box<[u64]>, usize) {
    let mut slots = MIN_SLOTS;
    while capacity(slots) < room.max(live) {
        slots = slots.saturating_mul(2);
        if slots == usize::MAX {
            super::heap::too_big();
        }
    }
    (state.heap.fresh(super::heap::words_for(DICT_HEADER_WORDS + slots, capacity(slots), width)), slots)
}

/// Fills in a new block with its header and `entries`, each `width` words and none removed, in
/// one copy, then finds each one's slot by its hash. False if a dict hashed the fast way got a
/// probe too long, stopping there.
fn place_entries(
    block: &mut [u64],
    (shape, slots, how): (u64, usize, Hashing),
    width: usize,
    entries: &[u64],
) -> bool {
    let live = entries.len() / width.max(1);
    let count = |n: usize| u64::try_from(n).unwrap_or(0);
    if let Some(header) = block.get_mut(..DICT_HEADER_WORDS) {
        header.copy_from_slice(&[1, shape, count(live), count(live), count(slots - 1), how.word()]);
    }
    let start = DICT_HEADER_WORDS + slots;
    let Some(moved) = block.get_mut(start..start + entries.len()) else { return true };
    moved.copy_from_slice(entries);
    let (taken, moved) = block.split_at_mut(start);
    let taken = taken.get_mut(DICT_HEADER_WORDS..).unwrap_or_default();
    for (at, entry) in moved.chunks_exact(width.max(1)).take(live).enumerate() {
        let hash = entry.first().copied().unwrap_or(0);
        let mut slot = usize::try_from(hash).unwrap_or(0) & (slots - 1);
        while taken.get(slot).is_some_and(|taken| *taken != 0) {
            slot = (slot + 1) & (slots - 1);
            if how == Hashing::Fast && too_far(hash, slot, slots - 1) {
                return false;
            }
        }
        if let Some(taken) = taken.get_mut(slot) {
            *taken = count(at + 1);
        }
    }
    true
}

/// A bigger block for a dict nothing else holds, its entries copied once straight across, and the
/// old block gone, as they've all moved.
fn grown(state: &mut State, at: u64, shape: u64, width: usize) -> Option<u64> {
    let live = usize::try_from(*state.heap.block(at)?.get(2)?).ok()?;
    let (mut new, new_slots) = with_room(state, width, live, 2 * (live + 1));
    let block = state.heap.block(at)?;
    let slots = usize::try_from(*block.get(4)?).ok()? + 1;
    let used = usize::try_from(*block.get(3)?).ok()?;
    let start = DICT_HEADER_WORDS + slots;
    let how = Hashing::of(block);
    let words = block.get(start..start + used * width)?;
    // With nothing removed, the entries move across as they are; otherwise those left are
    // gathered first.
    if used == live {
        fill(state, &mut new, (shape, new_slots, how), width, words);
    } else {
        let kept: Vec<u64> = words
            .chunks_exact(width)
            .filter(|entry| entry.first() != Some(&REMOVED))
            .flatten()
            .copied()
            .collect();
        fill(state, &mut new, (shape, new_slots, how), width, &kept);
    }
    state.heap.discard(at);
    u64::try_from(state.heap.insert(new)).ok()
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
        let at = start + entry * layout.width();
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
    Some(DICT_HEADER_WORDS + mask + 1 + entry * layout.width() + layout.value_at())
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
    let width = layout.width();
    let short = (bytes.len() <= super::text::INLINE_BYTES).then(|| super::text::inline(bytes));
    let words = packed_words(bytes);
    let length = u64::try_from(bytes.len()).ok()?;
    let hash = text_hash(Hashing::of(block), length, &words);
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
    with(|state| add_by_slice(state, dict, (text, from, to), value)).unwrap_or(dict)
}

/// `add_slice`, `None` having changed nothing for anything but a dict of string keys.
#[inline]
fn add_by_slice(state: &mut State, dict: i64, (text, from, to): (i64, i64, i64), value: i64) -> Option<i64> {
    let (bytes, chars) = slice_of(state, text, from, to)?;
    let at = u64::try_from(dict).ok()?;
    let block = state.heap.block(at)?;
    let (key_word, value_word) = kind(state, block)?;
    if !matches!(key_word, Word::Str | Word::Text) || matches!(value_word, Word::InPlace(_)) {
        if matches!(value_word, Word::InPlace(_)) {
            doesnt_fit(SET_AS_A_WORD);
        }
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
    super::made(
        "fennec_dict_add_new_slice",
        with(|state| {
            let held_as_headers = state
                .heap
                .block(u64::try_from(dict).ok()?)
                .and_then(|block| kind(state, block))
                .is_some_and(|(key, _)| key == Word::Text);
            if held_as_headers {
                if let Some(key) = ascii_slice_header(state, text, from, to) {
                    let value = value.cast_unsigned();
                    return set_inline(state, dict, key, value)
                        .or_else(|| set_words(state, dict, &key, value));
                }
                let (bytes, chars) = slice_of(state, text, from, to)?;
                let key = new_key(state, Word::Text, (&bytes, chars));
                return set_words(state, dict, &key, value.cast_unsigned());
            }
            let key = slice_key(state, text, from, to)?;
            set_in(state, dict, key, value)
        })
        .unwrap_or(dict),
    )
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
            let hash = hash_key(state, Word::Text, &key, Hashing::of(block));
            let Probe::Found(entry) = find(state, block, layout, &key, hash) else {
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
    super::made(
        "fennec_dict_set_header",
        with(|state| set_by_header(state, dict, [a, b, c], value)).unwrap_or(dict),
    )
}

/// `set_header`, `None` having changed nothing for anything but a dict of string keys.
#[inline]
fn set_by_header(state: &mut State, dict: i64, header: [u64; 3], value: i64) -> Option<i64> {
    let at = u64::try_from(dict).ok()?;
    let block = state.heap.block(at)?;
    let (key_word, value_word) = kind(state, block)?;
    if key_word == Word::Text {
        // Both of these refuse a dict that holds its values in place.
        let value = value.cast_unsigned();
        return set_inline(state, dict, header, value).or_else(|| set_words(state, dict, &header, value));
    }
    let (bytes, chars) = header_text(state, &header)?;
    // One word can't be written over a value held in place.
    if key_word != Word::Str || matches!(value_word, Word::InPlace(_)) {
        if matches!(value_word, Word::InPlace(_)) {
            doesnt_fit(SET_AS_A_WORD);
        }
        return None;
    }
    let owned = super::text::block_of(&header);
    if let (Some(&1), (Probe::Found(entry), _)) = (block.first(), find_text(state, block, (&bytes, chars))?) {
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
}

/// `dict.pop(key)` for a dict of string keys, the key given as its header's words, as `pop`
/// does it. The key's owner stays with the caller.
pub extern "C" fn pop_header(dict: i64, a: u64, b: u64, c: u64, start: i64, end: i64) -> i64 {
    pop_by_header::<false>(dict, [a, b, c], (start, end))
}

/// `pop_words` by a string key given as its header's words, for either kind of call.
#[inline]
fn pop_by_header<const IN_PLACE: bool>(dict: i64, [a, b, c]: [u64; 3], span: (i64, i64)) -> i64 {
    let held_as_headers = with(|state| {
        let block = state.heap.block(u64::try_from(dict).unwrap_or(0));
        block.and_then(|block| kind(state, block)).is_some_and(|(key, _)| key == Word::Text)
    });
    if held_as_headers {
        return pop_words::<IN_PLACE>(dict, &[a, b, c], Key::Lent, span);
    }
    let key = super::text::block(a, b, c);
    let popped = pop_words::<IN_PLACE>(dict, &[key.cast_unsigned()], Key::HandedOver, span);
    with(|state| release(state, key.cast_unsigned()));
    popped
}

/// A new empty dict of a shape, owned once, with room for `room` keys.
pub extern "C" fn new(shape: i64, room: i64) -> i64 {
    super::made_with("fennec_dict_new", |state| {
        let shape = u64::try_from(shape).unwrap_or(0);
        let width = shape_layout(state, shape).map_or(ENTRY_WORDS, Layout::width);
        let block = build(state, (shape, Hashing::Fast), width, &[], usize::try_from(room).unwrap_or(0));
        state.heap.insert(block)
    })
}

/// `dict[key] = value`: takes over the owners `dict`, `key` and `value` had, and gives back
/// the dict's. A dict with other owners is copied first, and a full one grows.
pub extern "C" fn set(dict: i64, key: i64, value: i64) -> i64 {
    super::made_with("fennec_dict_set", |state| set_in(state, dict, key, value).unwrap_or(dict))
}

fn set_in(state: &mut State, dict: i64, key: i64, value: i64) -> Option<i64> {
    set_words(state, dict, &[key.cast_unsigned()], value.cast_unsigned())
}

/// `fennec_dict_set_found(dict, entry, key, value)`: `dict[key] = value`, as `set` does, where native
/// code's lookup just found `key` at `entry`, the address of its entry, or 0. In a dict a lock's log
/// marked, with no other owner, as under `lock … as ref`, the value is written in place, what was
/// there saved in the log first, with no second search for the key. Only while `entry` is still one
/// of this dict's live entries, holding these key words: anything between the lookup and this that
/// grew, copied or changed the dict leaves `set` to do it.
pub extern "C" fn set_found(dict: i64, entry: i64, key: i64, value: i64) -> i64 {
    with(|state| {
        set_at_entry(state, dict, entry, key.cast_unsigned(), value.cast_unsigned())
            .or_else(|| set_in(state, dict, key, value))
            .unwrap_or(dict)
    })
}

/// `set_found`'s write in place, `None` having changed nothing for anything but a marked dict with
/// one owner whose live entry at `entry` holds `key`.
fn set_at_entry(state: &mut State, dict: i64, entry: i64, key: u64, value: u64) -> Option<i64> {
    let at = u64::try_from(dict).ok()?;
    let block = state.heap.block(at)?;
    if entry == 0 || !super::lists::marked_alone(block) {
        return None;
    }
    let layout = layout(state, block)?;
    if layout.width() != 3 || layout.in_place() {
        return None;
    }
    let mask = usize::try_from(*block.get(4)?).ok()?;
    let used = usize::try_from(*block.get(3)?).ok()?;
    let start = DICT_HEADER_WORDS + mask + 1;
    let bytes = usize::try_from(entry.cast_unsigned().checked_sub(at)?).ok()?;
    let word = (bytes % 8 == 0).then_some(bytes / 8)?;
    let number = word.checked_sub(start)?;
    if number % layout.width() != 0 || number / layout.width() >= used {
        return None;
    }
    if block.get(word) == Some(&REMOVED) || block.get(word + 1) != Some(&key) {
        return None;
    }
    let owned = layout.value.owned();
    let old = noted_write(state, at, word + layout.value_at(), value, owned)?;
    if owned {
        release(state, old);
    }
    if let Some(address) = key_block(layout.key, &[key]) {
        release(state, address);
    }
    if state.heap.counting() {
        FOUND_PUTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    Some(dict)
}

/// `fennec_proof_put_in_place(dict, entry)`, in a build that checks proofs: 1 when native code may
/// write a lock's put at `entry` without the runtime, as it does when it judged the dict and the
/// blocks on the way to it alone, and 0 when that was wrong: the dict must be a block this thread
/// holds, with exactly one owner and no mark, and `entry` one of its live entries.
pub extern "C" fn put_in_place_holds(dict: i64, entry: i64) -> i64 {
    with(|state| i64::from(live_entry_of_alone(state, dict, entry).is_some()))
}

fn live_entry_of_alone(state: &State, dict: i64, entry: i64) -> Option<()> {
    let at = u64::try_from(dict).ok()?;
    let block = state.heap.block(at)?;
    if block.first() != Some(&1) {
        return None;
    }
    let layout = layout(state, block)?;
    let mask = usize::try_from(*block.get(4)?).ok()?;
    let used = usize::try_from(*block.get(3)?).ok()?;
    let start = DICT_HEADER_WORDS + mask + 1;
    let bytes = usize::try_from(entry.cast_unsigned().checked_sub(at)?).ok()?;
    let word = (bytes % 8 == 0).then_some(bytes / 8)?;
    let number = word.checked_sub(start)?;
    let live =
        number % layout.width() == 0 && number / layout.width() < used && block.get(word) != Some(&REMOVED);
    live.then_some(())
}

/// How many puts `set_found` made where the lookup found the key, in a run that counts what it does,
/// for `FENNEC_COUNTS` to say.
static FOUND_PUTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many puts `set_found` made in place, while counting.
pub(super) fn found_puts() -> u64 {
    FOUND_PUTS.load(std::sync::atomic::Ordering::Relaxed)
}

/// `dict[key] = value` with the key's words: takes over the owners `dict`, a key held in a
/// block and `value` had, and gives back the dict's.
fn set_words(state: &mut State, dict: i64, key: &[u64], value: u64) -> Option<i64> {
    let at = u64::try_from(dict).ok()?;
    let block = state.heap.block(at)?;
    let layout = layout(state, block)?;
    // A key of the wrong width changes nothing: it's refused before the dict grows or a slot
    // is taken.
    // A value of one word for a dict that holds its values in place is the wrong call, which
    // stops the program: these setters don't know the owners in a value's words.
    if key.len() + 2 != layout.width() || layout.in_place() {
        if layout.in_place() {
            doesnt_fit(SET_AS_A_WORD);
        }
        return None;
    }
    if super::lists::marked_alone(block)
        && let Some(changed) = set_marked(state, at, layout, key, value)
    {
        return Some(changed);
    }
    let (at, hash, probe, mask) = ready_for(state, at, layout, key)?;
    let start = DICT_HEADER_WORDS + mask + 1;
    let block = state.heap.block_mut(at)?;
    match probe {
        Probe::Found(entry) => {
            let slot = block.get_mut(start + entry * layout.width() + layout.value_at())?;
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
            let entry = start + used * layout.width();
            let words = block.get_mut(entry..entry + layout.width())?;
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

/// Makes the dict at `at` ready for `key` to be written: one that's shared is copied, one that's
/// full grown, and one whose probe for the key got too long hashed the strong way. Gives the dict's
/// address, which may be new, the key's hash there, where the key is or goes, and one less than
/// the dict's slots.
#[inline]
fn ready_for(
    state: &mut State,
    mut at: u64,
    layout: Layout,
    key: &[u64],
) -> Option<(u64, u64, Probe, usize)> {
    let block = state.heap.block(at)?;
    let how = Hashing::of(block);
    let mut hash = hash_key(state, layout.key, key, how);
    let slots = usize::try_from(*block.get(4)?).ok()? + 1;
    let used = usize::try_from(*block.get(3)?).ok()?;
    let shared = !state.heap.is_alone(at, block);
    if shared {
        let (shape, live) = (shape_word(block)?, entries(block, layout.width()));
        // An empty dict shared, as the empty constant a dict can start as is, is made anew: there's
        // nothing to copy, so no copy is counted.
        if !live.is_empty() {
            state.heap.copied(live.len());
        }
        for entry in live.chunks_exact(layout.width()) {
            retain_entry(state, layout, entry);
        }
        release(state, at);
        let copy = build(state, (shape, how), layout.width(), &live, 2 * (live.len() / layout.width() + 1));
        at = u64::try_from(state.heap.insert(copy)).ok()?;
    } else if used >= capacity(slots) {
        let shape = shape_word(block)?;
        at = grown(state, at, shape, layout.width())?;
    }
    // Growing or copying may have changed how this dict hashes.
    let block = state.heap.block(at)?;
    if Hashing::of(block) != how {
        hash = hash_key(state, layout.key, key, Hashing::of(block));
    }
    let mask = usize::try_from(*block.get(4)?).ok()?;
    let mut probe = find(state, block, layout, key, hash);
    if let Probe::Empty(slot) = probe
        && Hashing::of(block) == Hashing::Fast
        && too_far(hash, slot, mask)
    {
        strengthen(state, at, layout)?;
        let block = state.heap.block(at)?;
        hash = hash_key(state, layout.key, key, Hashing::Strong);
        probe = find(state, block, layout, key, hash);
    }
    Some((at, hash, probe, mask))
}

/// `set_words` on a dict a lock's log marked, with no other owner: changed in place, each word that
/// changes saved in the log first, for a crash to put back (see `guarded::rollback`). `None`, having
/// changed nothing, when it would have to grow or change how it hashes, which copies it as for one
/// shared: its old block is then what a crash puts back.
fn set_marked(state: &mut State, at: u64, layout: Layout, key: &[u64], value: u64) -> Option<i64> {
    let block = state.heap.block(at)?;
    let how = Hashing::of(block);
    let hash = hash_key(state, layout.key, key, how);
    let mask = usize::try_from(*block.get(4)?).ok()?;
    let (keys, used) = (*block.get(2)?, usize::try_from(*block.get(3)?).ok()?);
    let start = DICT_HEADER_WORDS + mask + 1;
    match find(state, block, layout, key, hash) {
        Probe::Found(entry) => {
            let owned = layout.value.owned();
            let old =
                noted_write(state, at, start + entry * layout.width() + layout.value_at(), value, owned)?;
            if owned {
                release(state, old);
            }
            if let Some(address) = key_block(layout.key, key) {
                release(state, address);
            }
        }
        Probe::Empty(slot) => {
            if used >= capacity(mask + 1) || (how == Hashing::Fast && too_far(hash, slot, mask)) {
                return None;
            }
            let count = |n: usize| u64::try_from(n).unwrap_or(0);
            let entry = start + used * layout.width();
            let words = std::iter::once(hash).chain(key.iter().copied()).chain(std::iter::once(value));
            for (offset, word) in words.enumerate() {
                noted_write(state, at, entry + offset, word, false)?;
            }
            noted_write(state, at, DICT_HEADER_WORDS + slot, count(used + 1), false)?;
            noted_write(state, at, 2, keys + 1, false)?;
            noted_write(state, at, 3, count(used + 1), false)?;
        }
    }
    i64::try_from(at).ok()
}

/// Writes `new` at `word` in the dict at `at`, a lock's log marked, saving what was there in the log
/// first, and gives back what was there. With `holds`, that's a block's address the dict owns, and
/// the log takes an owner of its own.
fn noted_write(state: &mut State, at: u64, word: usize, new: u64, holds: bool) -> Option<u64> {
    // Written first, with one look for the block: what was there is still alive, for the log to
    // take an owner of, since nothing lets go of it until this returns.
    let old = std::mem::replace(state.heap.block_mut(at)?.get_mut(word)?, new);
    super::guarded::rollback::changing(state, at, word, old, holds);
    Some(old)
}

/// `set_words` for a key held inline in its header, in the usual case: a dict of string headers
/// that nothing else holds, with room for another key, and values that hold no block, so
/// nothing is let go. The dict's block is found once and the entry written whole. `None`,
/// having done nothing, for anything else, which `set_words` does.
fn set_inline(state: &mut State, dict: i64, key: [u64; 3], value: u64) -> Option<i64> {
    if super::text::block_of(&key).is_some() {
        return None;
    }
    let hash = hash_header(state, &key, Hashing::Fast);
    let State { heap, tables, .. } = state;
    if !heap.holds_all() {
        return None;
    }
    let block = heap.block_mut(u64::try_from(dict).ok()?)?;
    let (key_word, value_word) = shape_kinds(tables.str_shape, shape_word(block)?)?;
    // One word of a value that owns nothing: a value in place has its own way in.
    if key_word != Word::Text
        || !value_word.plain_word()
        || block.first() != Some(&1)
        || Hashing::of(block) == Hashing::Strong
    {
        return None;
    }
    let mask = usize::try_from(*block.get(4)?).ok()?;
    let used = usize::try_from(*block.get(3)?).ok()?;
    if used >= capacity(mask + 1) {
        return None;
    }
    let start = DICT_HEADER_WORDS + mask + 1;
    let mut slot = usize::try_from(hash).unwrap_or(0) & mask;
    // Inline headers are equal exactly when their words are, and never equal a block's.
    while let Some(entry) = usize::try_from(*block.get(DICT_HEADER_WORDS + slot)?).ok()?.checked_sub(1) {
        let at = start + entry * HEADER_ENTRY_WORDS;
        let words =
            <&mut [u64; HEADER_ENTRY_WORDS]>::try_from(block.get_mut(at..at + HEADER_ENTRY_WORDS)?).ok()?;
        if let [found, a, b, c, old] = words
            && *found == hash
            && [*a, *b, *c] == key
        {
            *old = value;
            return Some(dict);
        }
        slot = (slot + 1) & mask;
    }
    if too_far(hash, slot, mask) {
        return None;
    }
    *block.get_mut(DICT_HEADER_WORDS + slot)? = u64::try_from(used + 1).ok()?;
    let at = start + used * HEADER_ENTRY_WORDS;
    let [a, b, c] = key;
    *<&mut [u64; HEADER_ENTRY_WORDS]>::try_from(block.get_mut(at..at + HEADER_ENTRY_WORDS)?).ok()? =
        [hash, a, b, c, value];
    *block.get_mut(2)? += 1;
    *block.get_mut(3)? += 1;
    Some(dict)
}

fn retain_entry(state: &mut State, layout: Layout, entry: &[u64]) {
    if layout.in_place() {
        for address in held_in_place(state, layout, entry) {
            super::heap::retain(state, address);
        }
        return;
    }
    let held = [key_block(layout.key, key_of(layout, entry)), layout.value.owned().then(|| value_of(entry))];
    for address in held.into_iter().flatten() {
        super::heap::retain(state, address);
    }
}

/// The blocks an entry holds when its value is held in place: its key's, if it's held in one, and
/// each one the value's words hold, as its kind of item says.
fn held_in_place(state: &State, layout: Layout, entry: &[u64]) -> Vec<u64> {
    let mut held: Vec<u64> = key_block(layout.key, key_of(layout, entry)).into_iter().collect();
    if let Some(element) = value_element(state, layout.value) {
        super::heap::held_addresses(&element, value_words(layout, entry), &mut held);
    }
    held
}

/// A block only one owner holds, taking over the owner `at` had. One a lock's log marked, with no
/// other owner, stays as it is, for `take` to change in place.
fn unshared(state: &mut State, at: u64) -> Option<u64> {
    let block = state.heap.block(at)?;
    if state.heap.is_alone(at, block) || super::lists::marked_alone(block) {
        return Some(at);
    }
    let layout = layout(state, block)?;
    let (shape, live, how) = (shape_word(block)?, entries(block, layout.width()), Hashing::of(block));
    // An empty dict shared is made anew, with no copy counted, as in `set_words`.
    if !live.is_empty() {
        state.heap.copied(live.len());
    }
    for entry in live.chunks_exact(layout.width()) {
        retain_entry(state, layout, entry);
    }
    release(state, at);
    let copy = build(state, (shape, how), layout.width(), &live, live.len() / layout.width());
    u64::try_from(state.heap.insert(copy)).ok()
}

/// The dict or set to take a key out of. A shared one is copied only when the key is there: one
/// without it is left as it is, with every owner where it was, as there's nothing to take and the
/// call may be about to fail.
fn to_take_from(state: &mut State, at: u64, key: &[u64]) -> u64 {
    let Some(block) = state.heap.block(at) else { return at };
    if state.heap.is_alone(at, block) || super::lists::marked_alone(block) {
        return at;
    }
    let found = layout(state, block).is_some_and(|layout| {
        let hash = hash_key(state, layout.key, key, Hashing::of(block));
        matches!(find(state, block, layout, key, hash), Probe::Found(_))
    });
    if found { unshared(state, at).unwrap_or(0) } else { at }
}

/// A call for one kind of dict was made on the other. Native code has one set of calls for a
/// value that's a word and another for a value held in place, and picks by the dict's type. It's a bug in Fennec, in what native code
/// chose, so the program stops: going on would lose the write and every owner it was handed, or
/// hand back a value where the caller doesn't look, with nothing said.
#[cold]
#[inline(never)]
fn doesnt_fit(what: &str) {
    // The fuzzer hands made-up dicts on purpose, and goes on.
    if !super::made_up_inputs() {
        super::heap::runtime_bug(what);
    }
}

const SET_AS_A_WORD: &str = "a dict that holds its values in place was set a value of one word";
const SET_IN_PLACE: &str = "a dict of one-word values was set a value held in place";
const TAKEN_AS_A_WORD: &str = "a value was taken as one word out of a dict that holds its values in place";
const TAKEN_IN_PLACE: &str = "a value was taken as one held in place out of a dict of one-word values";

/// What taking a key out of a dict or set found.
enum Taken {
    /// It was there, with this value, whose owner goes to the caller.
    Removed(u64),
    Missing,
    /// What it was given isn't a dict or set.
    NotOne,
    /// The call isn't one for a dict that holds its values this way, where that goes on at all.
    DoesntFit,
}

/// Takes a key, given as its words, out of a dict or set already owned once: for a call that
/// takes out a value held in place when `IN_PLACE`, and one that's a word otherwise. Which it is
/// is known where each export is written, so it's a constant of the code and no argument: the
/// calls for a word are then the code they were, with the other kind's as a way out of them
/// that a dict of words never takes.
fn take_key<const IN_PLACE: bool>(state: &mut State, at: u64, key: &[u64]) -> Taken {
    take::<IN_PLACE>(state, at, key).unwrap_or(Taken::NotOne)
}

fn take<const IN_PLACE: bool>(state: &mut State, at: u64, key: &[u64]) -> Option<Taken> {
    let block = state.heap.block(at)?;
    let layout = layout(state, block)?;
    let hash = hash_key(state, layout.key, key, Hashing::of(block));
    let mask = usize::try_from(*block.get(4)?).ok()?;
    let Probe::Found(entry) = find(state, block, layout, key, hash) else { return Some(Taken::Missing) };
    let place = DICT_HEADER_WORDS + mask + 1 + entry * layout.width();
    let removed = block.get(place..place + layout.width())?;
    let held = key_block(layout.key, key_of(layout, removed));
    // A value in place leaves as its words, in this thread's buffer for whoever took it out, with
    // the owners they hold: there's no one word to give back.
    let mut holding = Vec::new();
    let value = match value_element(state, layout.value) {
        // Before anything changes: the value would come out where this call's caller doesn't look.
        Some(_) if !IN_PLACE => {
            doesnt_fit(TAKEN_AS_A_WORD);
            return Some(Taken::DoesntFit);
        }
        None if IN_PLACE => {
            doesnt_fit(TAKEN_IN_PLACE);
            return Some(Taken::DoesntFit);
        }
        Some(element) => {
            let fits = value_buffer_of(state, |buffer| {
                for (kept, word) in buffer.iter().zip(value_words(layout, removed)) {
                    kept.set(*word);
                }
                buffer.len() >= layout.value_words()
            });
            if !fits {
                too_wide_for_the_buffer();
            }
            super::heap::boxed_words(&element, removed, &mut layout.value_at(), &mut holding);
            0
        }
        None => value_of(removed),
    };
    if super::lists::marked_alone(block) {
        // A lock's log saves each word first, with owners of what the key and the value hold.
        let holds = |offset: usize| {
            (offset == 1 && held.is_some())
                || (offset == layout.value_at() && layout.value.owned())
                || holding.contains(&offset)
        };
        for offset in 0..layout.width() {
            let new = if offset == 0 { REMOVED } else { 0 };
            noted_write(state, at, place + offset, new, holds(offset))?;
        }
        let keys = *state.heap.block(at)?.get(2)?;
        noted_write(state, at, 2, keys - 1, false)?;
        if let Some(address) = held {
            release(state, address);
        }
        return Some(Taken::Removed(value));
    }
    // The whole entry is cleared, so nothing reads a key it no longer owns.
    state.heap.block_mut(at)?.get_mut(place..place + layout.width())?.fill(0);
    let block = state.heap.block_mut(at)?;
    *block.get_mut(place)? = REMOVED;
    *block.get_mut(2)? -= 1;
    if let Some(address) = held {
        release(state, address);
    }
    Some(Taken::Removed(value))
}

/// `set.discard(key)`, or `set.remove(key)` when `must`, which fails for a key the set doesn't
/// have. Takes over the set's owner and gives back the set's.
pub extern "C" fn set_remove(set: i64, key: i64, must: i8, start: i64, end: i64) -> i64 {
    remove_words(set, &[key.cast_unsigned()], Key::HandedOver, must, (start, end))
}

/// `set_remove` for a set of string keys held as headers, the key given as its header's words.
/// The key's owner stays with the caller.
pub extern "C" fn set_remove_header(set: i64, a: u64, b: u64, c: u64, must: i8, start: i64, end: i64) -> i64 {
    remove_words(set, &[a, b, c], Key::Lent, must, (start, end))
}

/// Whose the key given to look something up by is, when it's held in a block.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Key {
    /// Native code handed its owner over with the call, as it does with every key given as one
    /// word that's a block's address: the runtime lets go of it once it's done.
    HandedOver,
    /// The caller keeps it, as it does a string's header.
    Lent,
}

/// Lets go of the key a call was given, if its owner was handed over and the dict or set at `at`
/// holds its keys in blocks: the key looked up by is never the one the entry held, which goes
/// with the entry.
fn let_go_of_key(state: &mut State, at: u64, key: &[u64], whose: Key) {
    if whose == Key::Lent {
        return;
    }
    let kind = state.heap.block(at).and_then(|block| layout(state, block)).map(|layout| layout.key);
    if let Some(address) = kind.and_then(|kind| key_block(kind, key)) {
        release(state, address);
    }
}

fn remove_words(set: i64, key: &[u64], whose: Key, must: i8, (start, end): (i64, i64)) -> i64 {
    attempt(|state: &mut State| {
        let at = u64::try_from(set).map_or(0, |at| to_take_from(state, at, key));
        let removed = match take_key::<false>(state, at, key) {
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
            Taken::Removed(_) | Taken::Missing | Taken::DoesntFit => Ok(i64::try_from(at).unwrap_or(0)),
        };
        let_go_of_key(state, at, key, whose);
        removed
    })
}

thread_local! {
    /// The value the last `pop` took out of a dict, for native code to pick up.
    static POPPED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Does `with` this run's buffer on this thread for a value held in place on its way into a dict
/// or out of one: native code puts the value's words here before `fennec_dict_set_words`, and
/// finds them here after `fennec_dict_pop_words`. The runtime reads only its own memory, so a
/// value of several words can't come as a pointer into native code's. Each word is a cell, as
/// native code writes them where Rust can't see.
///
/// It's the run's own, kept with the runtime's state, and made the first time it's wanted: as
/// long as the widest kind of item this run has, so any value one of its dicts holds in place
/// fits. Native code writes a value's words before the runtime can look at how wide it is, so a
/// buffer left by another run, of a narrower kind, would be written past its end.
fn value_buffer_of<T>(state: &State, with: impl FnOnce(&[std::cell::Cell<u64>]) -> T) -> T {
    let buffer = state.dict_value.get_or_init(|| {
        let widest = state.tables.elements.iter().map(|element| element.words()).max().unwrap_or(0);
        (0..widest.max(1)).map(|_| std::cell::Cell::new(0)).collect()
    });
    with(buffer)
}

/// A dict's value in place is wider than this run's buffer, which is as long as the widest kind
/// of item the run had when the buffer was first wanted. Every dict native code makes names one of those kinds, so this is
/// a bug in Fennec: stopping says so, where going on would lose a write or hand back half a value.
fn too_wide_for_the_buffer() {
    // The fuzzer hands made-up dicts on purpose, and goes on.
    if !super::made_up_inputs() {
        super::heap::runtime_bug("a dict's value held in place is wider than the buffer it's passed in");
    }
}

/// The first `count` words of this thread's buffer for a value held in place, if it's that long.
fn buffered(state: &State, count: usize) -> Option<Vec<u64>> {
    value_buffer_of(state, |buffer| {
        buffer.get(..count).map(|words| words.iter().map(std::cell::Cell::get).collect())
    })
}

/// `fennec_dict_value_words()`: where this thread's buffer for a value held in place is, in this
/// run. It never moves while the run goes on, so native code may ask once on each thread and keep
/// the answer. A thread's next run has a buffer of its own.
pub extern "C" fn value_buffer() -> i64 {
    with(|state| value_buffer_of(state, |buffer| i64::try_from(buffer.as_ptr().addr()).unwrap_or(0)))
}

/// `fennec_dict_set_words(dict, key)`: `dict[key] = value` for a dict that holds its values in
/// place, the value's words being in this thread's buffer. As `set`, it takes over the owners
/// `dict` and a key held in a block had, and those the value's words hold, and gives back the
/// dict's. On a dict of one-word values it's the wrong call, which stops the program.
pub extern "C" fn set_in_place(dict: i64, key: i64) -> i64 {
    super::made_with("fennec_dict_set_words", |state| {
        set_buffered(state, dict, &[key.cast_unsigned()]).unwrap_or(dict)
    })
}

/// `fennec_dict_set_words_header(dict, a, b, c)`: `set_in_place` for a string key given as its
/// header's words. A key in a block is taken over, as `set_header` takes it.
pub extern "C" fn set_in_place_header(dict: i64, a: u64, b: u64, c: u64) -> i64 {
    super::made_with("fennec_dict_set_words_header", |state| {
        let key_word = u64::try_from(dict)
            .ok()
            .and_then(|at| state.heap.block(at))
            .and_then(|block| kind(state, block))
            .map(|(key, _)| key);
        let set = match key_word {
            Some(Word::Text) => set_buffered(state, dict, &[a, b, c]),
            // Held by its block's address: the block the header names, or one made for it.
            Some(Word::Str) => {
                let key = super::text::block(a, b, c).cast_unsigned();
                set_buffered(state, dict, &[key])
            }
            _ => None,
        };
        set.unwrap_or(dict)
    })
}

/// The value's words are copied out of the buffer before anything else, and are in the entry
/// before anything is let go of: letting go of the value replaced may run a program's own code,
/// which may use the buffer again.
fn set_buffered(state: &mut State, dict: i64, key: &[u64]) -> Option<i64> {
    let block = state.heap.block(u64::try_from(dict).ok()?)?;
    let layout = layout(state, block)?;
    if !layout.in_place() {
        doesnt_fit(SET_IN_PLACE);
        return None;
    }
    let Some(value) = buffered(state, layout.value_words()) else {
        too_wide_for_the_buffer();
        return None;
    };
    set_wide(state, dict, key, &value)
}

/// `dict[key] = value` for a dict that holds its values in place, the value given as its words:
/// takes over the owners `dict`, a key held in a block and the value's words had, and gives back
/// the dict's. `None`, having changed nothing, for a key or a value of the wrong width.
fn set_wide(state: &mut State, dict: i64, key: &[u64], value: &[u64]) -> Option<i64> {
    let at = u64::try_from(dict).ok()?;
    let block = state.heap.block(at)?;
    let layout = layout(state, block)?;
    let element = value_element(state, layout.value)?;
    if value.len() != layout.value_words() || 1 + key.len() + value.len() != layout.width() {
        return None;
    }
    if super::lists::marked_alone(block)
        && let Some(changed) = set_marked_wide(state, at, (layout, &element), key, value)
    {
        return Some(changed);
    }
    let (at, hash, probe, mask) = ready_for(state, at, layout, key)?;
    let start = DICT_HEADER_WORDS + mask + 1;
    let block = state.heap.block_mut(at)?;
    match probe {
        Probe::Found(entry) => {
            let place = start + entry * layout.width() + layout.value_at();
            let held = block.get_mut(place..place + value.len())?;
            let old = held.to_vec();
            held.copy_from_slice(value);
            // What the value it replaces held goes, and the key given, as the entry keeps its own.
            let mut let_go = Vec::new();
            super::heap::held_addresses(&element, &old, &mut let_go);
            let_go.extend(key_block(layout.key, key));
            for address in let_go {
                release(state, address);
            }
        }
        Probe::Empty(slot) => {
            let used = usize::try_from(*block.get(3)?).ok()?;
            *block.get_mut(DICT_HEADER_WORDS + slot)? = u64::try_from(used + 1).ok()?;
            let entry = start + used * layout.width();
            let words = block.get_mut(entry..entry + layout.width())?;
            *words.first_mut()? = hash;
            words.get_mut(1..layout.value_at())?.copy_from_slice(key);
            words.get_mut(layout.value_at()..)?.copy_from_slice(value);
            *block.get_mut(2)? += 1;
            *block.get_mut(3)? += 1;
        }
    }
    i64::try_from(at).ok()
}

/// `set_wide` on a dict a lock's log marked, with no other owner, as `set_marked` is to
/// `set_words`: each word that changes is saved in the log first. `None`, having changed nothing,
/// when it would have to grow or change how it hashes.
fn set_marked_wide(
    state: &mut State,
    at: u64,
    (layout, element): (Layout, &Element),
    key: &[u64],
    value: &[u64],
) -> Option<i64> {
    let block = state.heap.block(at)?;
    let how = Hashing::of(block);
    let hash = hash_key(state, layout.key, key, how);
    let mask = usize::try_from(*block.get(4)?).ok()?;
    let (keys, used) = (*block.get(2)?, usize::try_from(*block.get(3)?).ok()?);
    let start = DICT_HEADER_WORDS + mask + 1;
    match find(state, block, layout, key, hash) {
        Probe::Found(entry) => {
            let place = start + entry * layout.width() + layout.value_at();
            let old = block.get(place..place + value.len())?.to_vec();
            // The log takes an owner of each block the old words hold, at the word that holds it.
            let mut holding = Vec::new();
            super::heap::boxed_words(element, &old, &mut 0, &mut holding);
            for (offset, word) in value.iter().enumerate() {
                noted_write(state, at, place + offset, *word, holding.contains(&offset))?;
            }
            let mut let_go = Vec::new();
            super::heap::held_addresses(element, &old, &mut let_go);
            let_go.extend(key_block(layout.key, key));
            for address in let_go {
                release(state, address);
            }
        }
        Probe::Empty(slot) => {
            if used >= capacity(mask + 1) || (how == Hashing::Fast && too_far(hash, slot, mask)) {
                return None;
            }
            let count = |n: usize| u64::try_from(n).unwrap_or(0);
            let entry = start + used * layout.width();
            let words = std::iter::once(hash).chain(key.iter().copied()).chain(value.iter().copied());
            for (offset, word) in words.enumerate() {
                noted_write(state, at, entry + offset, word, false)?;
            }
            noted_write(state, at, DICT_HEADER_WORDS + slot, count(used + 1), false)?;
            noted_write(state, at, 2, keys + 1, false)?;
            noted_write(state, at, 3, count(used + 1), false)?;
        }
    }
    i64::try_from(at).ok()
}

/// `fennec_dict_pop_words(dict, key, start, end)`: `pop` for a dict that holds its values in
/// place. The key and a failure go as `pop`'s do. A value taken out is left as its words in this
/// thread's buffer, with the owners they hold, which are the caller's now; a key that isn't there
/// leaves the buffer as it was.
pub extern "C" fn pop_in_place(dict: i64, key: i64, start: i64, end: i64) -> i64 {
    pop_words::<true>(dict, &[key.cast_unsigned()], Key::HandedOver, (start, end))
}

/// `fennec_dict_pop_words_header(dict, a, b, c, start, end)`: `pop_in_place` for a string key
/// given as its header's words, which stays the caller's, as `pop_header`'s does.
pub extern "C" fn pop_in_place_header(dict: i64, a: u64, b: u64, c: u64, start: i64, end: i64) -> i64 {
    pop_by_header::<true>(dict, [a, b, c], (start, end))
}

/// `dict.pop(key)`: takes over the dict's owner and gives back the dict's, copying it first if
/// it's shared. The value taken out is picked up straight after with `popped`, whose owner goes
/// with it. A key the dict doesn't have fails.
pub extern "C" fn pop(dict: i64, key: i64, start: i64, end: i64) -> i64 {
    pop_words::<false>(dict, &[key.cast_unsigned()], Key::HandedOver, (start, end))
}

fn pop_words<const IN_PLACE: bool>(dict: i64, key: &[u64], whose: Key, (start, end): (i64, i64)) -> i64 {
    // A key that isn't there fails, and gives back the dict it was given, untouched even when
    // it's shared. Native code writes what this gives back as the dict only once it knows the call
    // worked, and after a failure still holds the dict it gave: a copy made here would have
    // moved the caller's owner to it.
    let (popped, failure) = with(|state| {
        let at = u64::try_from(dict).map_or(0, |at| to_take_from(state, at, key));
        let held = i64::try_from(at).unwrap_or(0);
        let popped = match take_key::<IN_PLACE>(state, at, key) {
            Taken::Removed(value) => {
                POPPED.set(value);
                (held, None)
            }
            // Its dict is the one it has now, which may be a copy made to take the key from.
            Taken::DoesntFit => (held, None),
            Taken::Missing => {
                let key_word = state
                    .heap
                    .block(at)
                    .and_then(|block| kind(state, block))
                    .map_or(Word::Int, |kind| kind.0);
                let value = key_value(state, key_word, key);
                let message = format!("the dict has no key {}", crate::format::repr(&value));
                (held, Some(Diagnostic::new(message, span(start, end))))
            }
            Taken::NotOne => (dict, None),
        };
        let_go_of_key(state, at, key, whose);
        popped
    });
    if let Some(failure) = failure {
        super::fail_with(failure);
    }
    popped
}

/// The value the last `pop` took out, as a word, with its owner.
pub extern "C" fn popped() -> i64 {
    POPPED.replace(0).cast_signed()
}

/// `set.clear()`: takes over the set's owner and gives back an empty set's.
pub extern "C" fn set_clear(set: i64) -> i64 {
    super::made_with("fennec_set_clear", |state| {
        let at = u64::try_from(set).unwrap_or(0);
        let shape = state.heap.block(at).and_then(shape_word).unwrap_or(0);
        release(state, at);
        let width = shape_layout(state, shape).map_or(ENTRY_WORDS, Layout::width);
        let empty = build(state, (shape, Hashing::Fast), width, &[], 0);
        state.heap.insert(empty)
    })
}

/// Fails with the language's message for a key a dict doesn't have: reading it, or, for
/// `augmented`, changing it with `+=` and the like.
pub extern "C" fn missing(dict: i64, key: i64, augmented: i8, start: i64, end: i64) {
    missing_words(dict, &[key.cast_unsigned()], Key::HandedOver, augmented, (start, end));
}

/// `missing` for a dict of string keys held as headers, the key given as its header's words.
pub extern "C" fn missing_header(dict: i64, a: u64, b: u64, c: u64, augmented: i8, start: i64, end: i64) {
    missing_words(dict, &[a, b, c], Key::Lent, augmented, (start, end));
}

fn missing_words(dict: i64, key: &[u64], whose: Key, augmented: i8, (start, end): (i64, i64)) {
    attempt(|state: &mut State| -> Result<(), Diagnostic> {
        let at = u64::try_from(dict).unwrap_or(0);
        let key_word =
            state.heap.block(at).and_then(|block| kind(state, block)).map_or(Word::Int, |kind| kind.0);
        let said = key_value(state, key_word, key);
        // The key is read for the message, and then let go of: the failure holds only its text.
        let_go_of_key(state, at, key, whose);
        let key = said;
        let message = format!("the dict has no key {}", crate::format::repr(&key));
        let failure = Diagnostic::new(message, span(start, end));
        Err(if augmented == 0 {
            failure.with_help("use `.get(key)`, which gives None instead")
        } else {
            failure
        })
    });
}

/// The addresses of the blocks a dict's block owns, read where its entries are, with no copy of them.
pub(super) fn children(state: &State, block: &[u64]) -> Vec<u64> {
    let Some(layout) = layout(state, block) else { return Vec::new() };
    // Numbers as keys and values hold no blocks, so there's nothing to look through.
    if !layout.value.owned() && !layout.key.owned() && layout.key != Word::Text && !layout.in_place() {
        return Vec::new();
    }
    let in_place = value_element(state, layout.value);
    let slots = block.get(4).map_or(0, |mask| usize::try_from(*mask).unwrap_or(0) + 1);
    let used = block.get(3).and_then(|used| usize::try_from(*used).ok()).unwrap_or(0);
    let start = DICT_HEADER_WORDS + slots;
    let words = block.get(start..start + used * layout.width()).unwrap_or_default();
    let mut found = Vec::new();
    for entry in words.chunks_exact(layout.width()).filter(|entry| entry.first() != Some(&REMOVED)) {
        found.extend(key_block(layout.key, key_of(layout, entry)));
        match &in_place {
            Some(element) => super::heap::held_addresses(element, value_words(layout, entry), &mut found),
            None => found.extend(layout.value.owned().then(|| value_of(entry))),
        }
    }
    found
}

/// Where among a dict's words its live entries hold the addresses of other blocks.
pub(super) fn child_places(state: &State, block: &[u64], found: &mut Vec<usize>) {
    let Some(layout) = layout(state, block) else { return };
    let slots = block.get(4).map_or(0, |mask| usize::try_from(*mask).unwrap_or(0) + 1);
    let used = block.get(3).and_then(|used| usize::try_from(*used).ok()).unwrap_or(0);
    let start = DICT_HEADER_WORDS + slots;
    let in_place = value_element(state, layout.value);
    for at in 0..used {
        let entry = start + at * layout.width();
        let Some(words) = block.get(entry..entry + layout.width()) else { return };
        if words.first() == Some(&REMOVED) {
            continue;
        }
        if key_block(layout.key, key_of(layout, words)).is_some_and(|held| held != 0) {
            found.push(entry + 1);
        }
        if let Some(element) = &in_place {
            // Each place among the value's words that holds a block, as in a list's item.
            super::heap::boxed_words(element, block, &mut (entry + layout.value_at()), found);
        } else if layout.value.owned() && value_of(words) != 0 {
            found.push(entry + layout.value_at());
        }
    }
}

/// A dict's block as a `Value`.
pub(super) fn read(state: &State, block: &[u64]) -> Value {
    let Some(layout) = layout(state, block) else { return Value::None };
    if layout.value == Word::Nothing {
        return read_set(state, layout, block);
    }
    let pairs: Vec<_> = entries(block, layout.width())
        .chunks_exact(layout.width())
        .map(|entry| {
            let value = match value_element(state, layout.value) {
                Some(element) => super::heap::read_in_place(state, &element, value_words(layout, entry)),
                None => word_value(state, layout.value, value_of(entry)),
            };
            (key_value(state, layout.key, key_of(layout, entry)), value)
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
    let held = entries(block, layout.width());
    if is_set && held.is_empty() {
        out.push_str("set()");
        return;
    }
    out.push('{');
    for (at, entry) in held.chunks_exact(layout.width()).enumerate() {
        if at > 0 {
            out.push_str(", ");
        }
        word_shown(state, layout.key, key_of(layout, entry), out);
        if !is_set {
            out.push_str(": ");
            match value_element(state, layout.value) {
                Some(element) => {
                    super::heap::shown::item(state, &element, &mut value_words(layout, entry).iter(), out);
                }
                None => word_shown(state, layout.value, &[value_of(entry)], out),
            }
        }
    }
    out.push('}');
}

fn word_shown(state: &State, word: Word, words: &[u64], out: &mut Written) {
    let element = match word {
        Word::Int => Element::Int,
        Word::Float => Element::Float,
        Word::U64 => Element::U64,
        Word::F32 => Element::F32,
        Word::Bool => Element::Bool,
        Word::Text => Element::Text,
        // A value in place is shown by its own kind of item, in `shown`.
        Word::Str | Word::Plain(_) | Word::Block(_) | Word::Nothing | Word::InPlace(_) => Element::Str,
    };
    super::heap::shown::item(state, &element, &mut words.iter(), out);
}

/// A set's block as a `Value`.
fn read_set(state: &State, layout: Layout, block: &[u64]) -> Value {
    let items = entries(block, layout.width())
        .chunks_exact(layout.width())
        .map(|entry| key_value(state, layout.key, key_of(layout, entry)))
        .collect();
    Value::Set(std::rc::Rc::new(items))
}

/// A key's words as a `Value`.
fn key_value(state: &State, word: Word, key: &[u64]) -> Value {
    if let (Word::Text, Ok(&[a, b, c])) = (word, <&[u64; 3]>::try_from(key)) {
        return Value::str(&super::text::text_of(state, &[a, b, c]));
    }
    word_value(state, word, key.first().copied().unwrap_or(0))
}

fn word_value(state: &State, word: Word, bits: u64) -> Value {
    match word {
        Word::Int | Word::U64 => Value::Int(i64::from_ne_bytes(bits.to_ne_bytes())),
        Word::Float | Word::F32 => Value::Float(f64::from_bits(bits)),
        Word::Bool => Value::Bool(bits != 0),
        Word::Str | Word::Plain(_) | Word::Block(_) => super::heap::read_block(state, bits),
        // A value in place is more than a word, and is read by its own kind of item, in `read`.
        Word::Nothing | Word::Text | Word::InPlace(_) => Value::Unit,
    }
}

/// A new block owned once holding a dict from a `Value`, if every key and value is of
/// the kind the shape says.
/// The name of a tuple key's shape, from its items: `(int, str)`, or `(Spot, (int, int))`. A key
/// holds only what can be one, so each item is a number, a string, a bool, a plain struct or enum,
/// or a tuple of those.
fn tuple_name(items: &[Value]) -> Option<String> {
    let names: Vec<String> = items
        .iter()
        .map(|item| match item {
            Value::Int(_) | Value::Float(_) | Value::Bool(_) | Value::Str(_) => Some(item.type_name()),
            Value::Instance(instance) => Some(instance.type_name.to_string()),
            Value::Tuple(inner) => tuple_name(inner),
            _ => None,
        })
        .collect::<Option<_>>()?;
    Some(format!("({})", names.join(", ")))
}

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
            (Word::Int | Word::U64, Value::Int(n)) => u64::from_ne_bytes(n.to_ne_bytes()),
            (Word::Float | Word::F32, Value::Float(x)) => x.to_bits(),
            (Word::Bool, Value::Bool(b)) => u64::from(*b),
            (Word::Str, Value::Str(text)) => u64::try_from(text_address(state, text)).ok()?,
            (Word::Nothing, Value::Unit) => 0,
            (Word::Plain(_), Value::Instance(instance)) => {
                let shape = super::heap::shape_named(state, &instance.type_name)?;
                u64::try_from(super::heap::write_block(state, shape, value)?).ok()?
            }
            // A tuple's shape is named by its items' types, as `(int, str)`.
            (Word::Plain(_), Value::Tuple(items)) => {
                let shape = super::heap::shape_named(state, &tuple_name(items)?)?;
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
    let mut entries = Vec::with_capacity(pairs.len() * layout.width());
    for (key, value) in pairs {
        let key = match (key_word, key) {
            (Word::Text, Value::Str(text)) => Some(super::text::header(state, text).to_vec()),
            _ => words(state, key_word, key).map(|key| vec![key]),
        };
        // A value in place is written as its kind of item is in a list, to exactly its width.
        let value = match value_element(state, value_word) {
            Some(element) => {
                let mut held = Vec::with_capacity(layout.value_words());
                let written = super::heap::write(state, &element, value, &mut held);
                let whole = written.is_some() && held.len() == layout.value_words();
                if !whole {
                    // What was written before it went wrong holds blocks made for it: zeros
                    // stand for the rest, which hold nothing.
                    held.resize(layout.value_words(), 0);
                    let mut made = Vec::new();
                    super::heap::held_addresses(&element, &held, &mut made);
                    for address in made {
                        release(state, address);
                    }
                }
                whole.then_some(held)
            }
            None => words(state, value_word, value).map(|value| vec![value]),
        };
        let (Some(key), Some(value)) = (key, value) else {
            for entry in entries.chunks_exact(layout.width()) {
                release_entry(state, layout, entry);
            }
            return None;
        };
        entries.push(hash_key(state, key_word, &key, Hashing::Fast));
        entries.extend(key);
        entries.extend(value);
    }
    let block = build(state, (shape, Hashing::Fast), layout.width(), &entries, pairs.len());
    Some(state.heap.insert(block))
}

fn release_entry(state: &mut State, layout: Layout, entry: &[u64]) {
    if layout.in_place() {
        for address in held_in_place(state, layout, entry) {
            release(state, address);
        }
        return;
    }
    let held = [key_block(layout.key, key_of(layout, entry)), layout.value.owned().then(|| value_of(entry))];
    for address in held.into_iter().flatten() {
        release(state, address);
    }
}

/// Whether two dicts' blocks hold the same keys with equal values, in any order.
pub(super) fn equal(state: &State, left: &[u64], right: &[u64], later: &mut Vec<(u64, u64)>) -> bool {
    let (Some(layout), theirs) = (layout(state, left), layout(state, right)) else { return false };
    // Dicts of the same type always share a layout, so differing ones are never equal.
    if theirs != Some(layout) {
        return false;
    }
    let (ours, theirs) = (entries(left, layout.width()), entries(right, layout.width()));
    ours.len() == theirs.len()
        && ours.chunks_exact(layout.width()).all(|entry| {
            let (hash, value) = (entry.first().copied().unwrap_or(0), value_of(entry));
            match find(state, right, layout, key_of(layout, entry), hash) {
                // Values in place are the same as two items of their kind in a list are.
                Probe::Found(at) if layout.in_place() => value_place(right, layout, at)
                    .and_then(|place| right.get(place..place + layout.value_words()))
                    .zip(value_element(state, layout.value))
                    .is_some_and(|(other, element)| {
                        super::heap::same_words_here(
                            state,
                            &element,
                            value_words(layout, entry),
                            other,
                            later,
                        )
                    }),
                Probe::Found(at) => value_place(right, layout, at)
                    .and_then(|place| right.get(place))
                    .is_some_and(|other| match layout.value {
                        Word::Float | Word::F32 => {
                            crate::value::floats_same(f64::from_bits(value), f64::from_bits(*other))
                        }
                        Word::Str | Word::Plain(_) | Word::Block(_) => {
                            super::heap::compare_later(later, value, *other)
                        }
                        Word::Int
                        | Word::U64
                        | Word::Bool
                        | Word::Nothing
                        | Word::Text
                        | Word::InPlace(_) => value == *other,
                    }),
                Probe::Empty(_) => false,
            }
        })
}

#[cfg(test)]
mod tests;
