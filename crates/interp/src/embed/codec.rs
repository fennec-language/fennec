//! Turning a program and the tables its native code uses into bytes and back, so a
//! compiled executable can carry them: the interpreter runs the parts native code hands
//! over, and reports failures against the program's source.
//!
//! The encoding is plain: little-endian numbers, lengths before lists and strings, a tag
//! before each variant. It only has to be read by the same version of Fennec that wrote it.

mod ast;

use std::rc::Rc;

use fennec_syntax::{Module, Sources, Span};

use super::Tables;
use crate::packed::Element;

/// Bytes being written.
#[derive(Default)]
pub struct Writer {
    bytes: Vec<u8>,
}

/// Bytes being read. Every read gives `None` once the bytes run out or don't make sense.
pub struct Reader<'b> {
    bytes: &'b [u8],
}

impl Writer {
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    fn word(&mut self, n: u64) {
        self.bytes.extend_from_slice(&n.to_le_bytes());
    }

    fn tag(&mut self, tag: u8) {
        self.bytes.push(tag);
    }
}

impl<'b> Reader<'b> {
    pub fn new(bytes: &'b [u8]) -> Self {
        Reader { bytes }
    }

    fn word(&mut self) -> Option<u64> {
        let (word, rest) = self.bytes.split_first_chunk::<8>()?;
        self.bytes = rest;
        Some(u64::from_le_bytes(*word))
    }

    fn tag(&mut self) -> Option<u8> {
        let (tag, rest) = self.bytes.split_first()?;
        self.bytes = rest;
        Some(*tag)
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
        self.start.encode(to);
        self.end.encode(to);
    }
}

impl Decode for Span {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        Some(Span { start: u32::decode(from)?, end: u32::decode(from)? })
    }
}

impl Encode for Element {
    fn encode(&self, to: &mut Writer) {
        match self {
            Element::Int => to.tag(0),
            Element::Float => to.tag(1),
            Element::Bool => to.tag(2),
            Element::Struct { name, fields } => {
                to.tag(3);
                name.encode(to);
                fields.encode(to);
            }
            Element::Boxed(shape) => {
                to.tag(4);
                shape.encode(to);
            }
            Element::Str => to.tag(5),
            Element::Object => to.tag(8),
            Element::List(item) => {
                to.tag(7);
                item.encode(to);
            }
            Element::Enum { name, cases, words } => {
                to.tag(6);
                name.encode(to);
                cases.encode(to);
                words.encode(to);
            }
        }
    }
}

impl Decode for Element {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        Some(match from.tag()? {
            0 => Element::Int,
            1 => Element::Float,
            2 => Element::Bool,
            3 => Element::Struct { name: Decode::decode(from)?, fields: Decode::decode(from)? },
            4 => Element::Boxed(Decode::decode(from)?),
            5 => Element::Str,
            8 => Element::Object,
            7 => Element::List(Decode::decode(from)?),
            6 => Element::Enum {
                name: Decode::decode(from)?,
                cases: Decode::decode(from)?,
                words: Decode::decode(from)?,
            },
            _ => return None,
        })
    }
}

/// What a compiled executable carries: its program, the files to show failures in, and
/// the tables its native code refers to by number.
pub struct Image {
    pub module: Module,
    pub sources: Sources,
    pub tables: Tables,
    /// The text of each string literal, in the order native code numbers them.
    pub literals: Vec<String>,
}

impl Image {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut to = Writer::default();
        self.module.encode(&mut to);
        let files: Vec<(String, String)> =
            self.sources.files().map(|(path, text)| (path.to_owned(), text.to_owned())).collect();
        files.encode(&mut to);
        let tables = &self.tables;
        tables.names.encode(&mut to);
        tables.strings.encode(&mut to);
        tables.exprs.encode(&mut to);
        tables.stmts.encode(&mut to);
        tables.patterns.encode(&mut to);
        tables.elements.encode(&mut to);
        tables.shapes.encode(&mut to);
        tables.str_shape.encode(&mut to);
        self.literals.encode(&mut to);
        to.into_bytes()
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Image> {
        let from = &mut Reader::new(bytes);
        let module = Module::decode(from)?;
        let mut sources = Sources::new();
        for (path, text) in Vec::<(String, String)>::decode(from)? {
            sources.add(path, text);
        }
        let tables = Tables {
            names: Decode::decode(from)?,
            strings: Decode::decode(from)?,
            exprs: Decode::decode(from)?,
            stmts: Decode::decode(from)?,
            patterns: Decode::decode(from)?,
            elements: Decode::decode(from)?,
            shapes: Decode::decode(from)?,
            str_shape: Decode::decode(from)?,
            ..Tables::default()
        };
        let literals = Decode::decode(from)?;
        Some(Image { module, sources, tables, literals })
    }
}

#[cfg(test)]
mod tests {
    use super::{Decode, Encode, Image, Reader, Writer};
    use crate::packed::Element;

    fn round_trip<T: Encode + Decode + PartialEq + std::fmt::Debug>(value: &T) {
        let mut to = Writer::default();
        value.encode(&mut to);
        let bytes = to.into_bytes();
        let mut from = Reader::new(&bytes);
        assert_eq!(T::decode(&mut from).as_ref(), Some(value));
        assert!(from.bytes.is_empty(), "bytes left over");
    }

    #[test]
    fn every_example_and_the_standard_library_come_back_unchanged() {
        let examples = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
        let mut sources: Vec<String> = fennec_syntax::prelude().map(str::to_owned).collect();
        for entry in std::fs::read_dir(examples).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|ext| ext == "fen") {
                sources.push(std::fs::read_to_string(path).unwrap());
            }
        }
        for source in &sources {
            if let Ok(module) = fennec_syntax::parse(source) {
                round_trip(&module);
            }
        }
    }

    #[test]
    fn packed_items_come_back_unchanged() {
        let point = Element::Struct { name: "Point".into(), fields: vec![("x".into(), Element::Float)] };
        round_trip(&Element::Enum {
            name: "Shape".into(),
            cases: vec![
                ("Dot".into(), vec![("at".into(), point)]),
                ("Text".into(), vec![("s".into(), Element::Str)]),
            ],
            words: 3,
        });
        round_trip(&Element::Boxed(4));
    }

    #[test]
    fn damaged_bytes_give_nothing() {
        assert!(Image::from_bytes(&[1, 2, 3]).is_none());
        assert!(Image::from_bytes(&[]).is_none());
    }
}
