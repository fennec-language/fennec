//! A dict's keys and values, in the order they were added. A dict of more than a few keys
//! finds them through a hash table of their places, made the first time it's needed, so
//! looking one up doesn't take as long as the dict is long.

use std::cell::RefCell;

use crate::value::Value;

/// The most keys found by looking through them all.
const FEW: usize = 16;

const MIX: u64 = 0x9E37_79B9_7F4A_7C15;

/// Where each key is: slots holding a place plus one, or zero, and each place's hash.
#[derive(Clone, Debug)]
struct Index {
    slots: Vec<u32>,
    hashes: Vec<u64>,
}

impl Index {
    fn with_room(keys: usize) -> Index {
        Index { slots: vec![0; (keys * 2).max(FEW).next_power_of_two()], hashes: Vec::with_capacity(keys) }
    }

    /// Adds the next place, with its hash.
    fn add(&mut self, hash: u64) {
        if (self.hashes.len() + 1) * 4 > self.slots.len() * 3 {
            let hashes = std::mem::take(&mut self.hashes);
            *self = Index::with_room(hashes.len() * 2);
            for hash in hashes {
                self.add(hash);
            }
        }
        let mask = self.slots.len() - 1;
        let mut slot = usize::try_from(hash).unwrap_or(0) & mask;
        while self.slots.get(slot).is_some_and(|taken| *taken != 0) {
            slot = (slot + 1) & mask;
        }
        self.hashes.push(hash);
        if let Some(taken) = self.slots.get_mut(slot) {
            *taken = u32::try_from(self.hashes.len()).unwrap_or(0);
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Pairs {
    items: Vec<(Value, Value)>,
    index: RefCell<Option<Index>>,
}

impl From<Vec<(Value, Value)>> for Pairs {
    /// Pairs whose keys are already different from each other.
    fn from(items: Vec<(Value, Value)>) -> Pairs {
        Pairs { items, index: RefCell::new(None) }
    }
}

impl std::ops::Deref for Pairs {
    type Target = [(Value, Value)];

    fn deref(&self) -> &Self::Target {
        &self.items
    }
}

impl Pairs {
    /// A key's place.
    pub(crate) fn find(&self, key: &Value) -> Option<usize> {
        if self.items.len() <= FEW {
            return self.items.iter().position(|(existing, _)| existing == key);
        }
        let mut index = self.index.borrow_mut();
        let index = index.get_or_insert_with(|| {
            let mut made = Index::with_room(self.items.len());
            for (existing, _) in &self.items {
                made.add(hash_of(existing));
            }
            made
        });
        let hash = hash_of(key);
        let mask = index.slots.len() - 1;
        let mut slot = usize::try_from(hash).unwrap_or(0) & mask;
        loop {
            let place =
                index.slots.get(slot).and_then(|taken| usize::try_from(*taken).ok())?.checked_sub(1)?;
            if index.hashes.get(place) == Some(&hash)
                && self.items.get(place).is_some_and(|(existing, _)| existing == key)
            {
                return Some(place);
            }
            slot = (slot + 1) & mask;
        }
    }

    /// A key's value.
    pub(crate) fn get(&self, key: &Value) -> Option<&Value> {
        self.items.get(self.find(key)?).map(|(_, value)| value)
    }

    /// A key's value, to change it.
    pub(crate) fn get_mut(&mut self, key: &Value) -> Option<&mut Value> {
        let place = self.find(key)?;
        self.items.get_mut(place).map(|(_, value)| value)
    }

    /// Gives a key a value, adding the key on the end if it's new.
    pub(crate) fn set(&mut self, key: Value, value: Value) {
        if let Some(slot) = self.get_mut(&key) {
            *slot = value;
            return;
        }
        if let Some(index) = self.index.get_mut() {
            index.add(hash_of(&key));
        }
        self.items.push((key, value));
    }

    /// Takes a key out, giving back its value.
    pub(crate) fn remove(&mut self, key: &Value) -> Option<Value> {
        let place = self.find(key)?;
        *self.index.get_mut() = None;
        Some(self.items.remove(place).1)
    }

    pub(crate) fn clear(&mut self) {
        *self.index.get_mut() = None;
        self.items.clear();
    }
}

/// A hash that's the same for any two values `==` says are equal.
fn hash_of(value: &Value) -> u64 {
    let hash = hash_in(0, value);
    hash ^ (hash >> 29)
}

fn mixed(hash: u64, word: u64) -> u64 {
    (hash.rotate_left(23) ^ word).wrapping_mul(MIX)
}

fn hash_text(hash: u64, bytes: &[u8]) -> u64 {
    bytes.chunks(8).fold(mixed(hash, u64::try_from(bytes.len()).unwrap_or(0)), |hash, chunk| {
        let mut word = [0; 8];
        for (to, from) in word.iter_mut().zip(chunk) {
            *to = *from;
        }
        mixed(hash, u64::from_le_bytes(word))
    })
}

fn hash_in(hash: u64, value: &Value) -> u64 {
    match value {
        Value::Bool(b) => mixed(hash, u64::from(*b) + 1),
        Value::Int(n) => mixed(hash, n.cast_unsigned()),
        Value::Float(x) => mixed(hash, x.to_bits()),
        Value::Str(text) => hash_text(hash, text.as_bytes()),
        Value::Bytes(data) => hash_text(hash, data),
        Value::List(items) | Value::Tuple(items) => items.iter().fold(mixed(hash, 7), hash_in),
        Value::Instance(instance) => {
            let hash = hash_text(hash, instance.type_name.as_bytes());
            let hash = instance.case.as_ref().map_or(hash, |case| hash_text(hash, case.as_bytes()));
            instance.fields.iter().fold(hash, |hash, (_, field)| hash_in(hash, field))
        }
        Value::Ok(inner) | Value::Err(inner) => hash_in(mixed(hash, 11), inner),
        // Equal sets or dicts can list their items in different orders.
        Value::Set(items) => mixed(hash, u64::try_from(items.len()).unwrap_or(0)),
        Value::Dict(pairs) => mixed(hash, u64::try_from(pairs.len()).unwrap_or(0)),
        // Whatever's left is rare as a key, and looking through those is enough.
        _ => mixed(hash, 13),
    }
}
