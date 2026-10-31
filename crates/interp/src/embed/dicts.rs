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

use super::heap::{HEADER_WORDS, TEXT_HEADER_WORDS, release, same, text_address};
use super::{State, attempt, span, with};
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
}

impl Word {
    /// Whether a word of this kind is the address of a block the dict owns.
    fn owned(self) -> bool {
        matches!(self, Word::Str | Word::Plain(_))
    }
}

const KEYS: [Word; 3] = [Word::Int, Word::Str, Word::Plain(0)];
const VALUES: [Word; 5] = [Word::Int, Word::Float, Word::Bool, Word::Str, Word::Nothing];

/// How many shapes dicts take, after the string shape.
pub const SHAPES: u64 = (KEYS.len() * VALUES.len()) as u64;

/// The shape of dicts with these keys and values, counted from the string shape, or `None`
/// for a pair native dicts don't hold.
pub fn shape(str_shape: u64, key: Word, value: Word) -> Option<u64> {
    let key = if let Word::Plain(_) = key { Word::Plain(0) } else { key };
    let key = KEYS.iter().position(|word| *word == key)?;
    let value = VALUES.iter().position(|word| *word == value)?;
    u64::try_from(key * VALUES.len() + value).ok().map(|at| str_shape + 1 + at)
}

/// What a dict's block holds, when it is a dict's.
fn kind(state: &State, block: &[u64]) -> Option<(Word, Word)> {
    let at = usize::try_from(block.get(1)?.checked_sub(state.tables.str_shape + 1)?).ok()?;
    Some((*KEYS.get(at / VALUES.len())?, *VALUES.get(at % VALUES.len())?))
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

/// A string key's hash, from its length and its words of text. Native code works it out
/// the same way.
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

fn hash_key(state: &State, key_word: Word, key: u64) -> u64 {
    if let Word::Plain(_) = key_word {
        return hash_words(
            state.heap.block(key).and_then(|block| block.get(HEADER_WORDS..)).unwrap_or_default(),
        );
    }
    if key_word != Word::Str {
        return hash_int(key);
    }
    let block = state.heap.block(key).unwrap_or_default();
    let length = block.get(2).copied().unwrap_or(0);
    let words = usize::try_from(length).unwrap_or(0).div_ceil(8);
    hash_text(length, block.get(TEXT_HEADER_WORDS..TEXT_HEADER_WORDS + words).unwrap_or_default())
}

fn keys_equal(state: &State, key_word: Word, a: u64, b: u64) -> bool {
    a == b || (key_word.owned() && same(state, a, b))
}

/// The live entries of a dict's block, in order: hash, key and value.
fn entries(block: &[u64]) -> Vec<[u64; 3]> {
    let slots = block.get(4).map_or(0, |mask| usize::try_from(*mask).unwrap_or(0) + 1);
    let used = block.get(3).and_then(|used| usize::try_from(*used).ok()).unwrap_or(0);
    let start = DICT_HEADER_WORDS + slots;
    let words = block.get(start..start + used * ENTRY_WORDS).unwrap_or_default();
    words.as_chunks::<ENTRY_WORDS>().0.iter().filter(|[hash, ..]| *hash != REMOVED).copied().collect()
}

/// A block holding `entries`, owned once, with room for at least `room` keys.
fn build(state: &mut State, shape: u64, entries: &[[u64; 3]], room: usize) -> Box<[u64]> {
    let mut slots = MIN_SLOTS;
    while capacity(slots) < room.max(entries.len()) {
        slots *= 2;
    }
    let mut block = state.heap.fresh(DICT_HEADER_WORDS + slots + capacity(slots) * ENTRY_WORDS);
    let count = |n: usize| u64::try_from(n).unwrap_or(0);
    if let Some(header) = block.get_mut(..DICT_HEADER_WORDS) {
        header.copy_from_slice(&[1, shape, count(entries.len()), count(entries.len()), count(slots - 1)]);
    }
    for (at, entry) in entries.iter().enumerate() {
        let mut slot = usize::try_from(entry[0]).unwrap_or(0) & (slots - 1);
        while block.get(DICT_HEADER_WORDS + slot).is_some_and(|taken| *taken != 0) {
            slot = (slot + 1) & (slots - 1);
        }
        if let Some(taken) = block.get_mut(DICT_HEADER_WORDS + slot) {
            *taken = count(at + 1);
        }
        let start = DICT_HEADER_WORDS + slots + at * ENTRY_WORDS;
        if let Some(words) = block.get_mut(start..start + ENTRY_WORDS) {
            words.copy_from_slice(entry);
        }
    }
    block
}

/// Where a key is: the number of its entry, or of the empty slot where it would go.
enum Probe {
    Found(usize),
    Empty(usize),
}

fn find(state: &State, block: &[u64], key_word: Word, key: u64, hash: u64) -> Probe {
    let mask = block.get(4).and_then(|mask| usize::try_from(*mask).ok()).unwrap_or(0);
    let start = DICT_HEADER_WORDS + mask + 1;
    let mut slot = usize::try_from(hash).unwrap_or(0) & mask;
    loop {
        let taken = block.get(DICT_HEADER_WORDS + slot).and_then(|taken| usize::try_from(*taken).ok());
        let Some(entry) = taken.and_then(|taken| taken.checked_sub(1)) else { return Probe::Empty(slot) };
        let at = start + entry * ENTRY_WORDS;
        if block.get(at) == Some(&hash)
            && block.get(at + 1).is_some_and(|other| keys_equal(state, key_word, *other, key))
        {
            return Probe::Found(entry);
        }
        slot = (slot + 1) & mask;
    }
}

/// A new empty dict of a shape, owned once, with room for `room` keys.
pub extern "C" fn new(shape: i64, room: i64) -> i64 {
    with(|state| {
        let block = build(state, u64::try_from(shape).unwrap_or(0), &[], usize::try_from(room).unwrap_or(0));
        state.heap.insert(block)
    })
}

/// `dict[key] = value`: takes over the owners `dict`, `key` and `value` had, and gives back
/// the dict's. A dict with other owners is copied first, and a full one grows.
pub extern "C" fn set(dict: i64, key: i64, value: i64) -> i64 {
    with(|state| set_in(state, dict, key, value).unwrap_or(dict))
}

fn set_in(state: &mut State, dict: i64, key: i64, value: i64) -> Option<i64> {
    let mut at = u64::try_from(dict).ok()?;
    let (key, value) = (u64::from_ne_bytes(key.to_ne_bytes()), u64::from_ne_bytes(value.to_ne_bytes()));
    let block = state.heap.block(at)?;
    let (key_word, value_word) = kind(state, block)?;
    let hash = hash_key(state, key_word, key);
    let slots = usize::try_from(*block.get(4)?).ok()? + 1;
    let used = usize::try_from(*block.get(3)?).ok()?;
    let shared = block.first() != Some(&1);
    if shared || used >= capacity(slots) {
        let (shape, live) = (*block.get(1)?, entries(block));
        if shared {
            for entry in &live {
                retain_entry(state, key_word, value_word, entry);
            }
            release(state, at);
        } else {
            state.heap.discard(at);
        }
        let grown = build(state, shape, &live, 2 * (live.len() + 1));
        at = u64::try_from(state.heap.insert(grown)).ok()?;
    }
    let block = state.heap.block(at)?;
    let mask = usize::try_from(*block.get(4)?).ok()?;
    let start = DICT_HEADER_WORDS + mask + 1;
    let probe = find(state, block, key_word, key, hash);
    let block = state.heap.block_mut(at)?;
    match probe {
        Probe::Found(entry) => {
            let slot = block.get_mut(start + entry * ENTRY_WORDS + 2)?;
            let old = std::mem::replace(slot, value);
            for (word, address) in [(value_word, old), (key_word, key)] {
                if word.owned() {
                    release(state, address);
                }
            }
        }
        Probe::Empty(slot) => {
            let used = usize::try_from(*block.get(3)?).ok()?;
            *block.get_mut(DICT_HEADER_WORDS + slot)? = u64::try_from(used + 1).ok()?;
            let entry = start + used * ENTRY_WORDS;
            block.get_mut(entry..entry + ENTRY_WORDS)?.copy_from_slice(&[hash, key, value]);
            *block.get_mut(2)? += 1;
            *block.get_mut(3)? += 1;
        }
    }
    i64::try_from(at).ok()
}

fn retain_entry(state: &mut State, key_word: Word, value_word: Word, [_, key, value]: &[u64; 3]) {
    for (word, address) in [(key_word, *key), (value_word, *value)] {
        if word.owned()
            && let Some(count) = state.heap.block_mut(address).and_then(|block| block.first_mut())
        {
            *count += 1;
        }
    }
}

/// A block only one owner holds, taking over the owner `at` had.
fn unshared(state: &mut State, at: u64) -> Option<u64> {
    let block = state.heap.block(at)?;
    if block.first() == Some(&1) {
        return Some(at);
    }
    let (key_word, value_word) = kind(state, block)?;
    let (shape, live) = (*block.get(1)?, entries(block));
    for entry in &live {
        retain_entry(state, key_word, value_word, entry);
    }
    release(state, at);
    let copy = build(state, shape, &live, live.len());
    u64::try_from(state.heap.insert(copy)).ok()
}

/// `set.discard(key)`, or `set.remove(key)` when `must`, failing like the interpreter for a
/// key it doesn't have. Takes over the set's owner and gives back the set's.
pub extern "C" fn set_remove(set: i64, key: i64, must: i8, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let key = key.cast_unsigned();
        let at = u64::try_from(set).ok().and_then(|at| unshared(state, at)).unwrap_or(0);
        let Some((block, (key_word, _))) =
            state.heap.block(at).and_then(|block| Some((block, kind(state, block)?)))
        else {
            return Ok(set);
        };
        let hash = hash_key(state, key_word, key);
        let mask = block.get(4).and_then(|mask| usize::try_from(*mask).ok()).unwrap_or(0);
        match find(state, block, key_word, key, hash) {
            Probe::Found(entry) => {
                let place = DICT_HEADER_WORDS + mask + 1 + entry * ENTRY_WORDS;
                let held = block.get(place + 1).copied().unwrap_or(0);
                if let Some(block) = state.heap.block_mut(at) {
                    if let Some(hash) = block.get_mut(place) {
                        *hash = REMOVED;
                    }
                    if let Some(keys) = block.get_mut(2) {
                        *keys -= 1;
                    }
                }
                if key_word.owned() {
                    release(state, held);
                }
            }
            Probe::Empty(_) if must != 0 => {
                let value = word_value(state, key_word, key);
                let message = format!("{} isn't in the set", crate::format::repr(&value));
                return Err(Diagnostic::new(message, span(start, end)));
            }
            Probe::Empty(_) => {}
        }
        Ok(i64::try_from(at).unwrap_or(0))
    })
}

/// `set.clear()`: takes over the set's owner and gives back an empty set's.
pub extern "C" fn set_clear(set: i64) -> i64 {
    with(|state| {
        let at = u64::try_from(set).unwrap_or(0);
        let shape = state.heap.block(at).and_then(|block| block.get(1)).copied().unwrap_or(0);
        release(state, at);
        let empty = build(state, shape, &[], 0);
        state.heap.insert(empty)
    })
}

/// Fails with the interpreter's message for a key a dict doesn't have: reading it, or, for
/// `augmented`, changing it with `+=` and the like.
pub extern "C" fn missing(dict: i64, key: i64, augmented: i8, start: i64, end: i64) {
    attempt(|state: &mut State| -> Result<(), Diagnostic> {
        let key_word =
            state.heap.block(u64::try_from(dict).unwrap_or(0)).and_then(|block| kind(state, block));
        let key = match key_word {
            Some((word, _)) if word.owned() => {
                super::heap::read_block(state, u64::try_from(key).unwrap_or(0))
            }
            _ => Value::Int(key),
        };
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
    let Some((key_word, value_word)) = kind(state, block) else { return Vec::new() };
    let mut found = Vec::new();
    for [_, key, value] in entries(block) {
        found.extend(key_word.owned().then_some(key));
        found.extend((value_word == Word::Str).then_some(value));
    }
    found
}

/// A dict's block as the interpreter holds a dict.
pub(super) fn read(state: &State, block: &[u64]) -> Value {
    let Some((key_word, value_word)) = kind(state, block) else { return Value::None };
    if value_word == Word::Nothing {
        return read_set(state, key_word, block);
    }
    let pairs: Vec<_> = entries(block)
        .into_iter()
        .map(|[_, key, value]| (word_value(state, key_word, key), word_value(state, value_word, value)))
        .collect();
    Value::Dict(std::rc::Rc::new(pairs.into()))
}

/// A set's block as the interpreter holds a set.
fn read_set(state: &State, key_word: Word, block: &[u64]) -> Value {
    let items = entries(block).into_iter().map(|[_, key, _]| word_value(state, key_word, key)).collect();
    Value::Set(std::rc::Rc::new(items))
}

fn word_value(state: &State, word: Word, bits: u64) -> Value {
    match word {
        Word::Int => Value::Int(i64::from_ne_bytes(bits.to_ne_bytes())),
        Word::Float => Value::Float(f64::from_bits(bits)),
        Word::Bool => Value::Bool(bits != 0),
        Word::Str | Word::Plain(_) => super::heap::read_block(state, bits),
        Word::Nothing => Value::Unit,
    }
}

/// A new block owned once holding a dict the interpreter holds, if every key and value is of
/// the kind the shape says.
pub(super) fn write(state: &mut State, shape: u64, value: &Value) -> Option<i64> {
    let at = usize::try_from(shape.checked_sub(state.tables.str_shape + 1)?).ok()?;
    let (key_word, value_word) = (*KEYS.get(at / VALUES.len())?, *VALUES.get(at % VALUES.len())?);
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
            _ => return None,
        })
    };
    let mut entries = Vec::with_capacity(pairs.len());
    for (key, value) in pairs {
        let (Some(key), Some(value)) = (words(state, key_word, key), words(state, value_word, value)) else {
            for entry in &entries {
                release_entry(state, key_word, value_word, entry);
            }
            return None;
        };
        entries.push([hash_key(state, key_word, key), key, value]);
    }
    let block = build(state, shape, &entries, entries.len());
    Some(state.heap.insert(block))
}

fn release_entry(state: &mut State, key_word: Word, value_word: Word, [_, key, value]: &[u64; 3]) {
    for (word, address) in [(key_word, *key), (value_word, *value)] {
        if word.owned() {
            release(state, address);
        }
    }
}

/// Whether two dicts' blocks hold the same keys with equal values, in any order, as the
/// interpreter compares dicts.
pub(super) fn equal(state: &State, left: &[u64], right: &[u64]) -> bool {
    let Some((key_word, value_word)) = kind(state, left) else { return false };
    let (ours, theirs) = (entries(left), entries(right));
    ours.len() == theirs.len()
        && ours.iter().all(|[hash, key, value]| match find(state, right, key_word, *key, *hash) {
            Probe::Found(entry) => theirs_value(right, entry).is_some_and(|other| match value_word {
                Word::Float => f64::from_bits(*value).total_cmp(&f64::from_bits(other)).is_eq(),
                Word::Str => same(state, *value, other),
                Word::Int | Word::Bool | Word::Nothing | Word::Plain(_) => *value == other,
            }),
            Probe::Empty(_) => false,
        })
}

fn theirs_value(block: &[u64], entry: usize) -> Option<u64> {
    let slots = usize::try_from(*block.get(4)?).ok()? + 1;
    block.get(DICT_HEADER_WORDS + slots + entry * ENTRY_WORDS + 2).copied()
}
