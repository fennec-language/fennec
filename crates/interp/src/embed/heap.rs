//! Boxed values for native code: structs and enums that contain themselves, like syntax
//! trees, and the types that hold them. Each value is a block of words the runtime owns.
//! Word 0 counts the block's owners, word 1 says which shape it has, and the rest hold its
//! fields as packed lists hold items, a boxed field as the address of its own block.
//!
//! Native code reads and writes blocks directly, and counts owners itself. Copies share a
//! block; a change to a shared block first gives the changer a copy of its own. When the
//! count reaches zero, native code calls `free`, which releases the block's fields too.
//! The runtime never follows an address it didn't hand out: it finds every block by its
//! address in a table.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::rc::Rc;

use fennec_syntax::Diagnostic;

use super::{State, Tables, attempt, span, with};
use crate::packed::Element;
use crate::value::{Instance, Lent, Value};

/// Words before a block's fields: its owner count and its shape.
pub const HEADER_WORDS: usize = 2;

/// Hashes block addresses, which are already spread out, by mixing their bits once.
#[derive(Default)]
pub struct AddressHasher(u64);

impl Hasher for AddressHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = (self.0 << 8 | u64::from(*byte)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        }
    }

    fn write_usize(&mut self, n: usize) {
        self.0 = u64::try_from(n).unwrap_or(0).wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(29);
    }
}

/// Every live block, by its address, and freed blocks kept for reuse, by size.
#[derive(Default)]
pub(crate) struct Heap {
    blocks: HashMap<usize, Box<[u64]>, BuildHasherDefault<AddressHasher>>,
    spare: Vec<Vec<Box<[u64]>>>,
    /// How many of the blocks live for the whole run.
    statics: usize,
    /// Strings' text as the interpreter holds it, made once per block.
    texts: RefCell<HashMap<usize, Rc<str>, BuildHasherDefault<AddressHasher>>>,
    /// Generators let go partway, waiting for what closes what they hold.
    abandoning: Vec<u64>,
    /// Blocks waiting to be freed and a freed block's children, kept to save making them anew.
    pending: Vec<u64>,
    found: Vec<u64>,
    /// The interpreter values cells hold, by the number in each cell, and unused numbers.
    cells: Vec<Value>,
    spare_cells: Vec<usize>,
    /// What native code's run has cost so far.
    pub(super) counts: Counts,
}

/// What a run of native code allocated and copied. Blocks that live for the whole run, and what
/// the interpreter holds for it, aren't counted. Blocks are made and copied in the runtime,
/// which does far more than add to these, so counting costs a run nothing it would notice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub allocations: u64,
    pub allocated_bytes: u64,
    /// Shared values copied so that one owner could change them.
    pub copies: u64,
    pub copied_bytes: u64,
    pub largest_copy: u64,
}

impl Counts {
    /// Notes that a shared value of `words` words was copied.
    pub(super) fn copied(&mut self, words: usize) {
        let bytes = u64::try_from(words).unwrap_or(0) * 8;
        self.copies += 1;
        self.copied_bytes += bytes;
        self.largest_copy = self.largest_copy.max(bytes);
    }
}

/// The shape of a cell: a block holding an interpreter value for a boxed value's field.
const CELL_SHAPE: u64 = u64::MAX - 1;

/// Words a cell takes: owner count, shape and the number of its value.
const CELL_WORDS: usize = 3;

/// Which word of a generator's block says where it goes on from, and what says it's finished.
const GENERATOR_PC: usize = (super::layout::GEN_PC / 8).unsigned_abs() as usize;
const FINISHED: u64 = u64::MAX;

/// The owner count of a block that's never freed: retaining and releasing it never get
/// near zero.
pub const IMMORTAL: u64 = 1 << 48;

/// How many freed blocks of each size are kept for reuse.
const SPARE_BLOCKS: usize = 1 << 16;

/// The biggest block, in words, kept for reuse.
const SPARE_WORDS: usize = 256;

impl Heap {
    /// A heap holding the blocks that live for the whole run.
    pub(crate) fn new(statics: Vec<Box<[u64]>>) -> Heap {
        let mut heap = Heap { statics: statics.len(), ..Heap::default() };
        for block in statics {
            heap.insert(block);
        }
        heap.counts = Counts::default();
        heap
    }

    /// Takes blocks made after the run started that live for the rest of it, like an
    /// executable's string literals.
    pub(crate) fn adopt(&mut self, statics: &mut Vec<Box<[u64]>>) {
        self.statics += statics.len();
        let counts = self.counts;
        for block in statics.drain(..) {
            self.insert(block);
        }
        self.counts = counts;
    }

    /// A zeroed block of `words` words, reusing a freed one when there is one.
    pub(super) fn fresh(&mut self, words: usize) -> Box<[u64]> {
        match self.spare.get_mut(words).and_then(Vec::pop) {
            Some(mut block) => {
                block.fill(0);
                block
            }
            None => vec![0; words].into_boxed_slice(),
        }
    }

    /// Keeps a freed block for reuse, when it's small: big ones, like a long list's, are
    /// rare enough to give back.
    fn recycle(&mut self, block: Box<[u64]>) {
        let words = block.len();
        if words > SPARE_WORDS {
            return;
        }
        if self.spare.len() <= words {
            self.spare.resize_with(words + 1, Vec::new);
        }
        if let Some(spare) = self.spare.get_mut(words).filter(|spare| spare.len() < SPARE_BLOCKS) {
            spare.push(block);
        }
    }

    pub(crate) fn live(&self) -> usize {
        self.blocks.len().saturating_sub(self.statics)
    }

    pub(crate) fn insert(&mut self, block: Box<[u64]>) -> i64 {
        self.counts.allocations += 1;
        self.counts.allocated_bytes += u64::try_from(block.len()).unwrap_or(0) * 8;
        let address = block.as_ptr().addr();
        self.blocks.insert(address, block);
        i64::try_from(address).unwrap_or(0)
    }

    /// Takes a block out of the heap for reuse, leaving the blocks it refers to as they are:
    /// for a block whose contents moved to a new one.
    pub(super) fn discard(&mut self, address: u64) {
        if let Some(block) = usize::try_from(address).ok().and_then(|at| self.blocks.remove(&at)) {
            self.recycle(block);
        }
    }

    pub(crate) fn block(&self, address: u64) -> Option<&[u64]> {
        self.blocks.get(&usize::try_from(address).ok()?).map(AsRef::as_ref)
    }

    pub(super) fn block_mut(&mut self, address: u64) -> Option<&mut [u64]> {
        self.blocks.get_mut(&usize::try_from(address).ok()?).map(AsMut::as_mut)
    }
}

fn shape_of(state: &State, block: &[u64]) -> Option<Rc<Element>> {
    let shape = usize::try_from(*block.get(1)?).ok()?;
    state.tables.shapes.get(shape).cloned()
}

/// Whether a block is a string's.
pub(super) fn is_text(state: &State, block: &[u64]) -> bool {
    block.get(1) == Some(&state.tables.str_shape)
}

/// Words a string's block starts with: its owner count, shape, length in bytes, length in
/// characters, and how many bytes its block has room for.
pub const TEXT_HEADER_WORDS: usize = 5;

/// A string's block: owned `count` times, its bytes packed eight to a word.
pub fn text_block(shape: u64, text: &str, count: u64) -> Box<[u64]> {
    let bytes = text.as_bytes();
    let length = |n: usize| u64::try_from(n).unwrap_or(0);
    let words = bytes.len().div_ceil(8).max(1);
    let mut block = Vec::with_capacity(TEXT_HEADER_WORDS + words);
    block.extend([count, shape, length(bytes.len()), length(text.chars().count()), length(words * 8)]);
    // Native code reads the first word of text without checking the length.
    if bytes.is_empty() {
        block.push(0);
    }
    for chunk in bytes.chunks(8) {
        let mut word = [0; 8];
        for (to, from) in word.iter_mut().zip(chunk) {
            *to = *from;
        }
        block.push(u64::from_le_bytes(word));
    }
    block.into_boxed_slice()
}

/// A new string owned once from bytes already known to be `chars` characters of UTF-8.
pub(super) fn text_from_bytes(state: &mut State, bytes: &[u8], chars: usize) -> i64 {
    if let [byte] = bytes
        && let Some(address) = state.tables.ascii.get(usize::from(*byte))
    {
        return i64::try_from(*address).unwrap_or(0);
    }
    let words = bytes.len().div_ceil(8).max(1);
    let mut block = state.heap.fresh(TEXT_HEADER_WORDS + words);
    let number = |n: usize| u64::try_from(n).unwrap_or(0);
    let header = [1, state.tables.str_shape, number(bytes.len()), number(chars), number(words * 8)];
    if let Some(start) = block.get_mut(..TEXT_HEADER_WORDS) {
        start.copy_from_slice(&header);
    }
    for (word, chunk) in block.iter_mut().skip(TEXT_HEADER_WORDS).zip(bytes.chunks(8)) {
        let mut packed = [0; 8];
        packed.get_mut(..chunk.len()).unwrap_or_default().copy_from_slice(chunk);
        *word = u64::from_le_bytes(packed);
    }
    state.heap.insert(block)
}

/// A string's text, from its block.
pub(crate) fn block_text(block: &[u64]) -> String {
    let length = block.get(2).and_then(|n| usize::try_from(*n).ok()).unwrap_or(0);
    let bytes: Vec<u8> = block
        .get(TEXT_HEADER_WORDS..)
        .unwrap_or_default()
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .take(length)
        .collect();
    String::from_utf8(bytes).unwrap_or_default()
}

/// The address of a string literal's block, made while compiling, which lives for the
/// whole run.
pub fn literal(tables: &mut Tables, text: &str) -> i64 {
    if let Some(address) = tables.literals.get(text) {
        return *address;
    }
    let block = text_block(tables.str_shape, text, IMMORTAL);
    let address = i64::try_from(block.as_ptr().addr()).unwrap_or(0);
    tables.statics.push(block);
    tables.literals.insert(text.to_owned(), address);
    address
}

/// The address of a table of the one-character strings of ASCII, by character, made the
/// first time it's needed.
pub fn ascii_table(tables: &mut Tables) -> i64 {
    if tables.ascii.is_empty() {
        let addresses: Vec<u64> = (0_u8..128)
            .map(|byte| u64::try_from(literal(tables, &char::from(byte).to_string())).unwrap_or(0))
            .collect();
        tables.ascii = addresses.into_boxed_slice();
    }
    i64::try_from(tables.ascii.as_ptr().addr()).unwrap_or(0)
}

/// Where each boxed field of a value sits among its words, from `at`.
fn boxed_words(element: &Element, words: &[u64], at: &mut usize, found: &mut Vec<usize>) {
    match element {
        Element::Boxed(_) | Element::Str | Element::List(_) | Element::Object => {
            found.push(*at);
            *at += 1;
        }
        Element::Struct { fields, .. } => {
            for (_, field) in fields {
                boxed_words(field, words, at, found);
            }
        }
        Element::Enum { cases, words: size, .. } => {
            let start = *at;
            let case = words.get(start).and_then(|tag| usize::try_from(*tag).ok());
            *at += 1;
            for (_, field) in case.and_then(|case| cases.get(case)).map_or(&[][..], |(_, fields)| fields) {
                boxed_words(field, words, at, found);
            }
            *at = start + size;
        }
        Element::Int | Element::Float | Element::Bool => *at += 1,
    }
}

/// A new cell owned once holding a value.
fn new_cell(state: &mut State, value: Value) -> i64 {
    let value = value.unpacked();
    let number = state.heap.spare_cells.pop().unwrap_or_else(|| {
        state.heap.cells.push(Value::Unit);
        state.heap.cells.len() - 1
    });
    if let Some(cell) = state.heap.cells.get_mut(number) {
        *cell = value;
    }
    let mut block = state.heap.fresh(CELL_WORDS);
    if let Some(words) = block.get_mut(..CELL_WORDS) {
        words.copy_from_slice(&[1, CELL_SHAPE, u64::try_from(number).unwrap_or(0)]);
    }
    state.heap.insert(block)
}

/// The value a cell's block holds.
fn cell_value(state: &State, block: &[u64]) -> Option<Value> {
    if block.get(1) != Some(&CELL_SHAPE) {
        return None;
    }
    let number = usize::try_from(*block.get(2)?).ok()?;
    state.heap.cells.get(number).cloned()
}

/// A new cell owned once holding a temporary's value.
pub extern "C" fn cell_new(value: i64) -> i64 {
    with(|state| {
        let value = state.raw(value);
        new_cell(state, value)
    })
}

/// A temporary with the value a cell holds. The cell keeps its owners.
pub extern "C" fn cell_read(address: i64) -> i64 {
    with(|state| {
        let block = u64::try_from(address).ok().and_then(|at| state.heap.block(at));
        let value = block.and_then(|block| cell_value(state, block)).unwrap_or(Value::None);
        state.push(value)
    })
}

/// The addresses a value holds among its words, from `at`, added to `found`.
fn boxed_addresses(element: &Element, words: &[u64], at: &mut usize, found: &mut Vec<u64>) {
    match element {
        Element::Boxed(_) | Element::Str | Element::List(_) | Element::Object => {
            found.extend(words.get(*at).copied().filter(|address| *address != 0));
            *at += 1;
        }
        Element::Struct { fields, .. } => {
            for (_, field) in fields {
                boxed_addresses(field, words, at, found);
            }
        }
        Element::Enum { cases, words: size, .. } => {
            let start = *at;
            let case = words.get(start).and_then(|tag| usize::try_from(*tag).ok());
            *at += 1;
            for (_, field) in case.and_then(|case| cases.get(case)).map_or(&[][..], |(_, fields)| fields) {
                boxed_addresses(field, words, at, found);
            }
            *at = start + size;
        }
        Element::Int | Element::Float | Element::Bool => *at += 1,
    }
}

/// Adds the addresses of a block's boxed fields to `found`.
fn children_into(state: &State, block: &[u64], found: &mut Vec<u64>) {
    let Some(shape) = block.get(1).copied() else { return };
    if shape >= state.tables.str_shape {
        if is_text(state, block) {
            return;
        }
        if super::dicts::is_dict(state, block) {
            found.extend(super::dicts::children(state, block));
            return;
        }
        if let Some(element) = super::lists::element_of(state, block)
            && super::lists::owns_items(&element)
        {
            found.extend(super::lists::items(block, 1).iter().copied().filter(|address| *address != 0));
        }
        return;
    }
    let shape = usize::try_from(shape).ok().and_then(|shape| state.tables.shapes.get(shape));
    if let Some(shape) = shape {
        boxed_addresses(shape, block.get(HEADER_WORDS..).unwrap_or_default(), &mut 0, found);
    }
}

/// The addresses of a block's boxed fields.
fn children(state: &State, block: &[u64]) -> Vec<u64> {
    let mut found = Vec::new();
    children_into(state, block, &mut found);
    found
}

/// A new block of a shape, owned once, its fields zero.
pub extern "C" fn alloc(shape: i64) -> i64 {
    with(|state| {
        let Some(element) = usize::try_from(shape).ok().and_then(|at| state.tables.shapes.get(at)) else {
            return 0;
        };
        let words = HEADER_WORDS + element.words();
        let mut block = state.heap.fresh(words);
        if let [count, kind, ..] = &mut *block {
            *count = 1;
            *kind = u64::try_from(shape).unwrap_or(0);
        }
        state.heap.insert(block)
    })
}

/// Frees a block whose last owner let it go, and releases its boxed fields, freeing those
/// that had no other owner, all the way down.
///
/// Program code only runs here for a generator let go partway, by the function its shape was
/// given with `set_abandon`. So freeing a block whose type holds no such generator and no
/// `del` runs no program code.
pub extern "C" fn free(address: i64) {
    with(|state| free_block(state, u64::try_from(address).unwrap_or(0)));
    run_abandons();
}

/// Says what closes what's open in a generator with this shape that's let go partway. Only
/// shapes given here are treated specially when they're freed.
pub extern "C" fn set_abandon(shape: i64, abandon: extern "C" fn(i64)) {
    with(|state| {
        state.tables.abandons.insert(u64::try_from(shape).unwrap_or(u64::MAX), abandon);
    });
}

/// Runs what closes the generators let go of, each only once nothing here is borrowed, since
/// it's program code calling back in. A failure then ends the program on the spot: whatever
/// released the generator isn't expecting one.
pub(super) fn run_abandons() {
    while let Some((address, abandon)) = with(next_abandoned) {
        // It reads where the generator stopped to know what's open.
        abandon(i64::try_from(address).unwrap_or(0));
        if super::failed() {
            super::aot::stop_for_failure();
        }
        with(|state| {
            if let Some(pc) = state.heap.block_mut(address).and_then(|block| block.get_mut(GENERATOR_PC)) {
                *pc = FINISHED;
            }
            release(state, address);
        });
    }
}

/// The next generator to close.
fn next_abandoned(state: &mut State) -> Option<(u64, extern "C" fn(i64))> {
    let address = state.heap.abandoning.pop()?;
    let shape = *state.heap.block(address)?.get(1)?;
    Some((address, *state.tables.abandons.get(&shape)?))
}

/// Whether a block being freed is a generator stopped at a `yield` with something to close.
fn is_abandoned(state: &State, block: &[u64]) -> bool {
    !state.tables.abandons.is_empty()
        && block.get(1).is_some_and(|shape| state.tables.abandons.contains_key(shape))
        && block.get(GENERATOR_PC).is_some_and(|pc| i64::from_ne_bytes(pc.to_ne_bytes()) > 0)
}

/// Frees a block and releases its boxed fields, freeing those with no other owner, all the
/// way down.
fn free_block(state: &mut State, address: u64) {
    let mut pending = std::mem::take(&mut state.heap.pending);
    let mut found = std::mem::take(&mut state.heap.found);
    pending.push(address);
    while let Some(address) = pending.pop() {
        let Some(block) = usize::try_from(address).ok().and_then(|at| state.heap.blocks.remove(&at)) else {
            continue;
        };
        if is_abandoned(state, &block) {
            // It lives until what closes it has run, which holds it as its one owner.
            let mut block = block;
            if let Some(count) = block.first_mut() {
                *count = 1;
            }
            if let Ok(at) = usize::try_from(address) {
                state.heap.blocks.insert(at, block);
            }
            state.heap.abandoning.push(address);
            continue;
        }
        if is_text(state, &block) {
            if let Ok(at) = usize::try_from(address) {
                state.heap.texts.get_mut().remove(&at);
            }
        } else if block.get(1) == Some(&CELL_SHAPE) {
            let number = block.get(2).and_then(|number| usize::try_from(*number).ok());
            if let Some(number) = number {
                // Whatever the value held is let go now, not when the number's used again.
                if let Some(cell) = state.heap.cells.get_mut(number) {
                    *cell = Value::Unit;
                }
                state.heap.spare_cells.push(number);
            }
        } else {
            children_into(state, &block, &mut found);
        }
        for child in found.drain(..) {
            if let Some(count) = state.heap.block_mut(child).and_then(|child| child.first_mut()) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    pending.push(child);
                }
            }
        }
        state.heap.recycle(block);
    }
    state.heap.pending = pending;
    state.heap.found = found;
}

/// Gives up one owner of a block, freeing it when that was the last.
pub(super) fn release(state: &mut State, address: u64) {
    let last = match state.heap.block_mut(address).and_then(|block| block.first_mut()) {
        Some(count) if *count > 1 => {
            *count -= 1;
            false
        }
        Some(_) => true,
        None => false,
    };
    if last {
        free_block(state, address);
    }
}

/// A copy of a shared block for one owner to change: owned once, sharing its boxed
/// fields. The block copied loses that owner.
pub extern "C" fn unshare(address: i64) -> i64 {
    with(|state| {
        let address = u64::try_from(address).unwrap_or(0);
        let Some(block) = state.heap.block_mut(address) else { return 0 };
        if let Some(count) = block.first_mut() {
            *count = count.saturating_sub(1);
        }
        let mut copy: Box<[u64]> = block.into();
        if let Some(count) = copy.first_mut() {
            *count = 1;
        }
        state.heap.counts.copied(copy.len());
        for child in children(state, &copy) {
            if let Some(count) = state.heap.block_mut(child).and_then(|child| child.first_mut()) {
                *count += 1;
            }
        }
        state.heap.insert(copy)
    })
}

/// A temporary with a block's value, which holds an owner of the block rather than a copy of
/// what's in it: most temporaries are only passed along. The block keeps its other owners.
pub extern "C" fn to_value(address: i64) -> i64 {
    with(|state| {
        let address = u64::try_from(address).unwrap_or(0);
        if state.heap.block(address).is_none_or(|block| is_text(state, block)) {
            let value = read_block(state, address);
            return state.push(value);
        }
        retain(state, address);
        state.push(Value::Lent(Rc::new(Lent { address })))
    })
}

/// One more owner for a block.
pub(super) fn retain(state: &mut State, address: u64) {
    if let Some(count) = state.heap.block_mut(address).and_then(|block| block.first_mut()) {
        *count += 1;
    }
}

/// A block's owner from a temporary lent to it, if that's what it is and `fits` likes the block.
pub(super) fn lent_block(
    state: &mut State,
    value: i64,
    fits: impl Fn(&State, &[u64]) -> bool,
) -> Option<i64> {
    let Value::Lent(lent) = state.raw(value) else { return None };
    let block = state.heap.block(lent.address)?;
    if !fits(state, block) {
        return None;
    }
    retain(state, lent.address);
    i64::try_from(lent.address).ok()
}

/// A field of a struct a temporary is lent, without looking at the rest of it. `None` for
/// anything else, which the interpreter then reads.
pub(super) fn lent_field(state: &mut State, value: &Value, name: i64) -> Option<Value> {
    let Value::Lent(lent) = value else { return None };
    let name = State::name_ref(&state.tables, name);
    let block = state.heap.block(lent.address)?;
    let shape = Rc::clone(state.tables.shapes.get(usize::try_from(*block.get(1)?).ok()?)?);
    let Element::Struct { fields, .. } = shape.as_ref() else { return None };
    let mut at = HEADER_WORDS;
    let (element, word) = fields.iter().find_map(|(field, element)| {
        let word = block.get(at).copied();
        at += element.words();
        (field.as_ref() == name).then_some((element, word))
    })?;
    let word = word?;
    Some(match element {
        Element::Int | Element::Float | Element::Bool | Element::Str => read_word(state, element, word),
        Element::Object => cell_value(state, state.heap.block(word)?)?,
        Element::Boxed(_) | Element::List(_) => {
            retain(state, word);
            Value::Lent(Rc::new(Lent { address: word }))
        }
        _ => return None,
    })
}

/// The name of the type of a struct or enum a temporary is lent.
pub(super) fn lent_type(state: &State, value: &Value) -> Option<Rc<str>> {
    let Value::Lent(lent) = value else { return None };
    let shape = usize::try_from(*state.heap.block(lent.address)?.get(1)?).ok()?;
    match state.tables.shapes.get(shape)?.as_ref() {
        Element::Struct { name, .. } | Element::Enum { name, .. } => Some(Rc::clone(name)),
        _ => None,
    }
}

pub(super) fn read_block(state: &State, address: u64) -> Value {
    let at = usize::try_from(address).unwrap_or(0);
    // A string only changes in place by growing, so a text of the same length is current.
    let length = state.heap.block(address).and_then(|block| block.get(2)).copied();
    if let Some(text) =
        state.heap.texts.borrow().get(&at).filter(|text| u64::try_from(text.len()).ok() == length)
    {
        return Value::Str(Rc::clone(text));
    }
    let Some(block) = state.heap.block(address).map(<[u64]>::to_vec) else { return Value::None };
    if let Some(value) = cell_value(state, &block) {
        return state.expanded(value);
    }
    if is_text(state, &block) {
        let text: Rc<str> = Rc::from(block_text(&block));
        state.heap.texts.borrow_mut().insert(at, Rc::clone(&text));
        return Value::Str(text);
    }
    if super::dicts::is_dict(state, &block) {
        return super::dicts::read(state, &block);
    }
    if super::lists::element_of(state, &block).is_some() {
        return super::lists::read(state, &block);
    }
    let Some(shape) = shape_of(state, &block) else { return Value::None };
    let mut words = block.get(HEADER_WORDS..).unwrap_or_default().iter();
    read(state, &shape, &mut words)
}

/// A value of a word, as the interpreter holds it.
pub(super) fn read_word(state: &State, element: &Element, word: u64) -> Value {
    read(state, element, &mut std::iter::once(&word))
}

fn read<'w>(state: &State, element: &Element, words: &mut dyn Iterator<Item = &'w u64>) -> Value {
    let word = |words: &mut dyn Iterator<Item = &'w u64>| words.next().copied().unwrap_or(0);
    match element {
        Element::Int => Value::Int(i64::from_ne_bytes(word(words).to_ne_bytes())),
        Element::Float => Value::Float(f64::from_bits(word(words))),
        Element::Bool => Value::Bool(word(words) != 0),
        Element::Boxed(_) | Element::Str | Element::Object => read_block(state, word(words)),
        // Only the top of a value the interpreter is handed may be a packed list.
        Element::List(_) => read_block(state, word(words)).unpacked(),
        Element::Struct { name, fields } => {
            let fields =
                fields.iter().map(|(field, element)| (Rc::clone(field), read(state, element, words)));
            instance(name, None, fields.collect())
        }
        Element::Enum { name, cases, words: size } => {
            let mut mine = (&mut *words).take(*size);
            let at = usize::try_from(word(&mut mine)).unwrap_or(0);
            let (case, fields) = cases.get(at).or_else(|| cases.first()).cloned().unwrap_or_default();
            let fields =
                fields.iter().map(|(field, element)| (Rc::clone(field), read(state, element, &mut mine)));
            let value = instance(name, Some(case), fields.collect());
            mine.for_each(drop);
            value
        }
    }
}

fn instance(name: &Rc<str>, case: Option<Rc<str>>, fields: Vec<(Rc<str>, Value)>) -> Value {
    Value::Instance(Rc::new(Instance {
        type_name: Rc::clone(name),
        case,
        fields,
        is_error: false,
        cause: None,
    }))
}

/// A new block owned once, with a temporary's value, failing like the interpreter when the
/// value isn't of the shape's type.
pub extern "C" fn from_value(value: i64, shape: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let wanted = u64::try_from(shape).unwrap_or(u64::MAX);
        if let Some(address) = lent_block(state, value, |_, block| block.get(1) == Some(&wanted)) {
            return Ok(address);
        }
        let value = state.temp(value);
        let shape = usize::try_from(shape).unwrap_or(0);
        write_block(state, shape, &value).ok_or_else(|| {
            let wanted = match state.tables.shapes.get(shape).map(AsRef::as_ref) {
                Some(Element::Struct { name, .. } | Element::Enum { name, .. }) => name.to_string(),
                _ => "value".to_owned(),
            };
            Diagnostic::new(
                format!("expected a `{wanted}` here, but this is a {}", value.type_name()),
                span(start, end),
            )
        })
    })
}

/// The number of the shape of a struct or enum, by its name.
pub(super) fn shape_named(state: &State, name: &str) -> Option<usize> {
    state.tables.shapes.iter().position(|shape| match shape.as_ref() {
        Element::Struct { name: found, .. } | Element::Enum { name: found, .. } => found.as_ref() == name,
        _ => false,
    })
}

pub(super) fn write_block(state: &mut State, shape: usize, value: &Value) -> Option<i64> {
    if u64::try_from(shape).ok()? > state.tables.str_shape {
        return super::dicts::write(state, u64::try_from(shape).ok()?, value);
    }
    let element = state.tables.shapes.get(shape).cloned()?;
    let mut words = vec![1, u64::try_from(shape).ok()?];
    write(state, &element, value, &mut words)?;
    Some(state.heap.insert(words.into_boxed_slice()))
}

pub(super) fn write(state: &mut State, element: &Element, value: &Value, words: &mut Vec<u64>) -> Option<()> {
    match (element, value) {
        (Element::Int, Value::Int(n)) => words.push(u64::from_ne_bytes(n.to_ne_bytes())),
        (Element::Float, Value::Float(x)) => words.push(x.to_bits()),
        (Element::Bool, Value::Bool(b)) => words.push(u64::from(*b)),
        (Element::Boxed(shape), value) => {
            let address = write_block(state, *shape, value)?;
            words.push(u64::try_from(address).ok()?);
        }
        (Element::Str, Value::Str(text)) => words.push(u64::try_from(text_address(state, text)).ok()?),
        (Element::Object, value) => words.push(u64::try_from(new_cell(state, value.clone())).ok()?),
        (Element::List(item), value) => {
            words.push(u64::try_from(super::lists::write_list(state, item, value)?).ok()?);
        }
        (Element::Struct { name, fields }, Value::Instance(instance))
            if instance.type_name == *name && instance.case.is_none() && !instance.is_error =>
        {
            for (field, element) in fields {
                write(state, element, instance.field(field)?, words)?;
            }
        }
        (Element::Enum { name, cases, words: size }, Value::Instance(instance))
            if instance.type_name == *name && !instance.is_error =>
        {
            let case = instance.case.as_ref()?;
            let at = cases.iter().position(|(written, _)| written == case)?;
            let start = words.len();
            words.push(u64::try_from(at).ok()?);
            for (field, element) in &cases.get(at)?.1 {
                write(state, element, instance.field(field)?, words)?;
            }
            words.resize(start + size, 0);
        }
        _ => return None,
    }
    Some(())
}

/// Whether two blocks hold equal values: the same bits, field by field, and equal boxed
/// fields, which is how the interpreter compares values.
pub extern "C" fn equal(a: i64, b: i64) -> i8 {
    with(|state| i8::from(same(state, u64::try_from(a).unwrap_or(0), u64::try_from(b).unwrap_or(0))))
}

pub(super) fn same(state: &State, a: u64, b: u64) -> bool {
    if a == b {
        return true;
    }
    let (Some(left), Some(right)) = (state.heap.block(a), state.heap.block(b)) else { return false };
    if left.get(1) != right.get(1) {
        return false;
    }
    if let (Some(mine), Some(theirs)) = (cell_value(state, left), cell_value(state, right)) {
        return mine == theirs;
    }
    if is_text(state, left) {
        // Compare the lengths and the bytes in use, not the room to grow.
        let length = left.get(2).and_then(|n| usize::try_from(*n).ok()).unwrap_or(0);
        let text = TEXT_HEADER_WORDS..TEXT_HEADER_WORDS + length.div_ceil(8);
        return left.get(1..4) == right.get(1..4) && left.get(text.clone()) == right.get(text);
    }
    if super::dicts::is_dict(state, left) {
        return super::dicts::equal(state, left, right);
    }
    if let Some(element) = super::lists::element_of(state, left) {
        // Items compare bit by bit, as the interpreter compares numbers and plain structs.
        let stride = element.words();
        let (mine, theirs) = (super::lists::items(left, stride), super::lists::items(right, stride));
        if super::lists::owns_items(&element) {
            return mine.len() == theirs.len() && mine.iter().zip(theirs).all(|(x, y)| same(state, *x, *y));
        }
        return mine == theirs;
    }
    if left.len() != right.len() {
        return false;
    }
    let Some(shape) = shape_of(state, left) else { return false };
    let (left, right) =
        (left.get(HEADER_WORDS..).unwrap_or_default(), right.get(HEADER_WORDS..).unwrap_or_default());
    let mut boxed = Vec::new();
    boxed_words(&shape, left, &mut 0, &mut boxed);
    // The case decides where the boxed fields are, so compare the plain words first.
    let plain_same = left.iter().zip(right).enumerate().all(|(at, (x, y))| boxed.contains(&at) || x == y);
    plain_same
        && boxed.iter().all(|at| {
            let (Some(x), Some(y)) = (left.get(*at), right.get(*at)) else { return false };
            same(state, *x, *y)
        })
}

/// A string's block owned once, or a one-character ASCII string's block that lives for the
/// whole run.
pub(super) fn text_address(state: &mut State, text: &str) -> i64 {
    if let [byte] = text.as_bytes()
        && let Some(address) = state.tables.ascii.get(usize::from(*byte))
    {
        return i64::try_from(*address).unwrap_or(0);
    }
    let block = text_block(state.tables.str_shape, text, 1);
    state.heap.insert(block)
}
