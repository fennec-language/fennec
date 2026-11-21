//! Turning a program, and what's worked out about it, into bytes and back: a compiled
//! executable carries its program this way, and a build handed to another process sends it.
//!
//! The encoding is plain: little-endian numbers, lengths before lists and strings, a tag
//! before each variant. It only has to be read by the same version of Fennec that wrote it.

mod ast;

/// Encodes and decodes a struct field by field.
#[macro_export]
macro_rules! codec_fields {
    ($ty:ident { $($field:ident),* $(,)? }) => {
        impl $crate::codec::Encode for $ty {
            fn encode(&self, to: &mut $crate::codec::Writer) {
                $($crate::codec::Encode::encode(&self.$field, to);)*
            }
        }

        impl $crate::codec::Decode for $ty {
            fn decode(from: &mut $crate::codec::Reader<'_>) -> Option<Self> {
                Some($ty { $($field: $crate::codec::Decode::decode(from)?),* })
            }
        }
    };
}

/// Encodes and decodes an enum whose variants have no fields, by position.
#[macro_export]
macro_rules! codec_units {
    ($ty:ident [$($variant:ident),* $(,)?]) => {
        impl $crate::codec::Encode for $ty {
            fn encode(&self, to: &mut $crate::codec::Writer) {
                const ALL: &[$ty] = &[$($ty::$variant),*];
                let at = ALL.iter().position(|known| known == self).unwrap_or(0);
                to.tag(u8::try_from(at).unwrap_or(0));
            }
        }

        impl $crate::codec::Decode for $ty {
            fn decode(from: &mut $crate::codec::Reader<'_>) -> Option<Self> {
                const ALL: &[$ty] = &[$($ty::$variant),*];
                ALL.get(usize::from(from.tag()?)).copied()
            }
        }
    };
}

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use crate::Span;

/// Bytes being written.
#[derive(Default)]
pub struct Writer {
    bytes: Vec<u8>,
    /// What's taken off every span written, so they're kept as from the start of their file.
    spans_from: u32,
    /// Whether spans are left out, for a tree's hash, which a move or a reformat doesn't change.
    no_spans: bool,
}

/// Bytes being read. Every read gives `None` once the bytes run out or don't make sense.
pub struct Reader<'b> {
    bytes: &'b [u8],
    /// What's added to every span read, to put them at their file's offset among a program's.
    spans_at: u32,
}

impl Writer {
    /// A writer that keeps spans as offsets from `base`, for something parsed at `base` that may
    /// be read back at another, as `Reader::with_spans_at` reads it.
    #[must_use]
    pub fn with_spans_from(base: u32) -> Self {
        Writer { bytes: Vec::new(), spans_from: base, no_spans: false }
    }

    /// A writer that leaves spans out: what's written says what a tree holds, wherever it is.
    /// It can't be read back.
    pub fn without_spans() -> Self {
        Writer { bytes: Vec::new(), spans_from: 0, no_spans: true }
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    pub fn word(&mut self, n: u64) {
        self.bytes.extend_from_slice(&n.to_le_bytes());
    }

    pub fn tag(&mut self, tag: u8) {
        self.bytes.push(tag);
    }
}

impl<'b> Reader<'b> {
    pub fn new(bytes: &'b [u8]) -> Self {
        Reader { bytes, spans_at: 0 }
    }

    /// A reader that puts every span `base` bytes later, for what `Writer::with_spans_from` wrote.
    #[must_use]
    pub fn with_spans_at(bytes: &'b [u8], base: u32) -> Self {
        Reader { bytes, spans_at: base }
    }

    pub fn word(&mut self) -> Option<u64> {
        let (word, rest) = self.bytes.split_first_chunk::<8>()?;
        self.bytes = rest;
        Some(u64::from_le_bytes(*word))
    }

    pub fn tag(&mut self) -> Option<u8> {
        let (tag, rest) = self.bytes.split_first()?;
        self.bytes = rest;
        Some(*tag)
    }

    /// Whether every byte has been read.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.bytes.is_empty()
    }

    fn take(&mut self, count: usize) -> Option<&'b [u8]> {
        let (taken, rest) = self.bytes.split_at_checked(count)?;
        self.bytes = rest;
        Some(taken)
    }
}

pub trait Encode {
    fn encode(&self, to: &mut Writer);
}

pub trait Decode: Sized {
    fn decode(from: &mut Reader<'_>) -> Option<Self>;
}

impl Encode for u64 {
    fn encode(&self, to: &mut Writer) {
        to.word(*self);
    }
}

impl Decode for u64 {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        from.word()
    }
}

impl Encode for i64 {
    fn encode(&self, to: &mut Writer) {
        to.word(u64::from_le_bytes(self.to_le_bytes()));
    }
}

impl Decode for i64 {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        Some(i64::from_le_bytes(from.word()?.to_le_bytes()))
    }
}

impl Encode for u32 {
    fn encode(&self, to: &mut Writer) {
        to.word(u64::from(*self));
    }
}

impl Decode for u32 {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        u32::try_from(from.word()?).ok()
    }
}

impl Encode for usize {
    fn encode(&self, to: &mut Writer) {
        to.word(u64::try_from(*self).unwrap_or(u64::MAX));
    }
}

impl Decode for usize {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        usize::try_from(from.word()?).ok()
    }
}

impl Encode for f64 {
    fn encode(&self, to: &mut Writer) {
        to.word(self.to_bits());
    }
}

impl Decode for f64 {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        Some(f64::from_bits(from.word()?))
    }
}

impl Encode for u8 {
    fn encode(&self, to: &mut Writer) {
        to.bytes.push(*self);
    }
}

impl Decode for u8 {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        let (first, rest) = from.bytes.split_first()?;
        from.bytes = rest;
        Some(*first)
    }
}

impl Encode for crate::Blob {
    fn encode(&self, to: &mut Writer) {
        self.0.as_ref().encode(to);
    }
}

impl Decode for crate::Blob {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        Vec::<u8>::decode(from).map(crate::Blob::from)
    }
}

impl Encode for bool {
    fn encode(&self, to: &mut Writer) {
        to.tag(u8::from(*self));
    }
}

impl Decode for bool {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        match from.tag()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }
}

impl Encode for str {
    fn encode(&self, to: &mut Writer) {
        self.len().encode(to);
        to.bytes.extend_from_slice(self.as_bytes());
    }
}

impl Encode for String {
    fn encode(&self, to: &mut Writer) {
        self.as_str().encode(to);
    }
}

impl Decode for String {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        let length = usize::decode(from)?;
        String::from_utf8(from.take(length)?.to_vec()).ok()
    }
}

impl<T: Encode> Encode for [T] {
    fn encode(&self, to: &mut Writer) {
        self.len().encode(to);
        for item in self {
            item.encode(to);
        }
    }
}

impl<T: Encode> Encode for Vec<T> {
    fn encode(&self, to: &mut Writer) {
        self.as_slice().encode(to);
    }
}

impl<T: Decode> Decode for Vec<T> {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        let length = usize::decode(from)?;
        // Each item takes at least a byte, so a length past the bytes left is corrupt.
        if length > from.bytes.len() {
            return None;
        }
        (0..length).map(|_| T::decode(from)).collect()
    }
}

impl<T: Encode> Encode for Option<T> {
    fn encode(&self, to: &mut Writer) {
        match self {
            None => to.tag(0),
            Some(value) => {
                to.tag(1);
                value.encode(to);
            }
        }
    }
}

impl<T: Decode> Decode for Option<T> {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        match from.tag()? {
            0 => Some(None),
            1 => Some(Some(T::decode(from)?)),
            _ => None,
        }
    }
}

impl<T: Encode> Encode for Box<T> {
    fn encode(&self, to: &mut Writer) {
        self.as_ref().encode(to);
    }
}

impl<T: Decode> Decode for Box<T> {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        T::decode(from).map(Box::new)
    }
}

impl<T: Encode> Encode for Rc<T> {
    fn encode(&self, to: &mut Writer) {
        self.as_ref().encode(to);
    }
}

impl<T: Decode> Decode for Rc<T> {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        T::decode(from).map(Rc::new)
    }
}

impl Encode for Rc<str> {
    fn encode(&self, to: &mut Writer) {
        self.as_ref().encode(to);
    }
}

impl Decode for Rc<str> {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        String::decode(from).map(Rc::from)
    }
}

impl<T: Encode + ?Sized> Encode for &T {
    fn encode(&self, to: &mut Writer) {
        (**self).encode(to);
    }
}

impl<A: Encode, B: Encode, C: Encode> Encode for (A, B, C) {
    fn encode(&self, to: &mut Writer) {
        self.0.encode(to);
        self.1.encode(to);
        self.2.encode(to);
    }
}

impl<A: Encode, B: Encode, C: Encode, D: Encode, E: Encode> Encode for (A, B, C, D, E) {
    fn encode(&self, to: &mut Writer) {
        self.0.encode(to);
        self.1.encode(to);
        self.2.encode(to);
        self.3.encode(to);
        self.4.encode(to);
    }
}

impl<A: Encode, B: Encode> Encode for (A, B) {
    fn encode(&self, to: &mut Writer) {
        self.0.encode(to);
        self.1.encode(to);
    }
}

impl<A: Decode, B: Decode> Decode for (A, B) {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        Some((A::decode(from)?, B::decode(from)?))
    }
}

impl Encode for Span {
    fn encode(&self, to: &mut Writer) {
        if to.no_spans {
            return;
        }
        self.start.saturating_sub(to.spans_from).encode(to);
        self.end.saturating_sub(to.spans_from).encode(to);
    }
}

impl Decode for Span {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        let at = from.spans_at;
        Some(Span { start: u32::decode(from)?.checked_add(at)?, end: u32::decode(from)?.checked_add(at)? })
    }
}

impl<K: Encode, V: Encode> Encode for BTreeMap<K, V> {
    fn encode(&self, to: &mut Writer) {
        self.len().encode(to);
        for (key, value) in self {
            key.encode(to);
            value.encode(to);
        }
    }
}

impl<K: Decode + Ord, V: Decode> Decode for BTreeMap<K, V> {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        let length = usize::decode(from)?;
        if length > from.bytes.len() {
            return None;
        }
        let mut map = BTreeMap::new();
        for _ in 0..length {
            let key = K::decode(from)?;
            let value = V::decode(from)?;
            // Written in order, so a key out of order, or twice, is corrupt.
            if map.last_key_value().is_some_and(|(last, _)| *last >= key) {
                return None;
            }
            map.insert(key, value);
        }
        Some(map)
    }
}

impl<T: Encode> Encode for BTreeSet<T> {
    fn encode(&self, to: &mut Writer) {
        self.len().encode(to);
        for item in self {
            item.encode(to);
        }
    }
}

impl<T: Decode + Ord> Decode for BTreeSet<T> {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        let length = usize::decode(from)?;
        if length > from.bytes.len() {
            return None;
        }
        let mut set = BTreeSet::new();
        for _ in 0..length {
            let item = T::decode(from)?;
            if set.last().is_some_and(|last| *last >= item) {
                return None;
            }
            set.insert(item);
        }
        Some(set)
    }
}

impl<A: Decode, B: Decode, C: Decode> Decode for (A, B, C) {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        Some((A::decode(from)?, B::decode(from)?, C::decode(from)?))
    }
}
