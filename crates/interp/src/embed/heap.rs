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

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::rc::Rc;

use fennec_syntax::Diagnostic;

use super::{State, attempt, span, with};
use crate::packed::Element;
use crate::value::{Instance, Value};

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
}

/// How many freed blocks of each size are kept for reuse.
const SPARE_BLOCKS: usize = 1 << 16;

impl Heap {
    /// A zeroed block of `words` words, reusing a freed one when there is one.
    fn fresh(&mut self, words: usize) -> Box<[u64]> {
        match self.spare.get_mut(words).and_then(Vec::pop) {
            Some(mut block) => {
                block.fill(0);
                block
            }
            None => vec![0; words].into_boxed_slice(),
        }
    }

    /// Keeps a freed block for reuse.
    fn recycle(&mut self, block: Box<[u64]>) {
        let words = block.len();
        if self.spare.len() <= words {
            self.spare.resize_with(words + 1, Vec::new);
        }
        if let Some(spare) = self.spare.get_mut(words).filter(|spare| spare.len() < SPARE_BLOCKS) {
            spare.push(block);
        }
    }

    pub(crate) fn live(&self) -> usize {
        self.blocks.len()
    }

    fn insert(&mut self, block: Box<[u64]>) -> i64 {
        let address = block.as_ptr().addr();
        self.blocks.insert(address, block);
        i64::try_from(address).unwrap_or(0)
    }

    fn block(&self, address: u64) -> Option<&[u64]> {
        self.blocks.get(&usize::try_from(address).ok()?).map(AsRef::as_ref)
    }

    fn block_mut(&mut self, address: u64) -> Option<&mut [u64]> {
        self.blocks.get_mut(&usize::try_from(address).ok()?).map(AsMut::as_mut)
    }
}

fn shape_of(state: &State, block: &[u64]) -> Option<Rc<Element>> {
    let shape = usize::try_from(*block.get(1)?).ok()?;
    state.tables.shapes.get(shape).cloned()
}

/// Where each boxed field of a value sits among its words, from `at`.
fn boxed_words(element: &Element, words: &[u64], at: &mut usize, found: &mut Vec<usize>) {
    match element {
        Element::Boxed(_) => {
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

/// The addresses of a block's boxed fields.
fn children(state: &State, block: &[u64]) -> Vec<u64> {
    let Some(shape) = shape_of(state, block) else { return Vec::new() };
    let fields = block.get(HEADER_WORDS..).unwrap_or_default();
    let mut at = 0;
    let mut found = Vec::new();
    boxed_words(&shape, fields, &mut at, &mut found);
    found.into_iter().filter_map(|at| fields.get(at).copied()).filter(|address| *address != 0).collect()
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
pub extern "C" fn free(address: i64) {
    with(|state| {
        let mut pending = vec![u64::try_from(address).unwrap_or(0)];
        while let Some(address) = pending.pop() {
            let Some(block) = usize::try_from(address).ok().and_then(|at| state.heap.blocks.remove(&at))
            else {
                continue;
            };
            for child in children(state, &block) {
                if let Some(count) = state.heap.block_mut(child).and_then(|child| child.first_mut()) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        pending.push(child);
                    }
                }
            }
            state.heap.recycle(block);
        }
    });
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
        for child in children(state, &copy) {
            if let Some(count) = state.heap.block_mut(child).and_then(|child| child.first_mut()) {
                *count += 1;
            }
        }
        state.heap.insert(copy)
    })
}

/// A temporary with a block's value, as the interpreter holds values. The block keeps its
/// owners.
pub extern "C" fn to_value(address: i64) -> i64 {
    with(|state| {
        let value = read_block(state, u64::try_from(address).unwrap_or(0));
        state.push(value)
    })
}

fn read_block(state: &State, address: u64) -> Value {
    let Some(block) = state.heap.block(address) else { return Value::None };
    let Some(shape) = shape_of(state, block) else { return Value::None };
    let mut words = block.get(HEADER_WORDS..).unwrap_or_default().iter();
    read(state, &shape, &mut words)
}

fn read<'w>(state: &State, element: &Element, words: &mut dyn Iterator<Item = &'w u64>) -> Value {
    let word = |words: &mut dyn Iterator<Item = &'w u64>| words.next().copied().unwrap_or(0);
    match element {
        Element::Int => Value::Int(i64::from_ne_bytes(word(words).to_ne_bytes())),
        Element::Float => Value::Float(f64::from_bits(word(words))),
        Element::Bool => Value::Bool(word(words) != 0),
        Element::Boxed(_) => read_block(state, word(words)),
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

fn write_block(state: &mut State, shape: usize, value: &Value) -> Option<i64> {
    let element = state.tables.shapes.get(shape).cloned()?;
    let mut words = vec![1, u64::try_from(shape).ok()?];
    write(state, &element, value, &mut words)?;
    Some(state.heap.insert(words.into_boxed_slice()))
}

fn write(state: &mut State, element: &Element, value: &Value, words: &mut Vec<u64>) -> Option<()> {
    match (element, value) {
        (Element::Int, Value::Int(n)) => words.push(u64::from_ne_bytes(n.to_ne_bytes())),
        (Element::Float, Value::Float(x)) => words.push(x.to_bits()),
        (Element::Bool, Value::Bool(b)) => words.push(u64::from(*b)),
        (Element::Boxed(shape), value) => {
            let address = write_block(state, *shape, value)?;
            words.push(u64::try_from(address).ok()?);
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

fn same(state: &State, a: u64, b: u64) -> bool {
    if a == b {
        return true;
    }
    let (Some(left), Some(right)) = (state.heap.block(a), state.heap.block(b)) else { return false };
    if left.get(1) != right.get(1) || left.len() != right.len() {
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
