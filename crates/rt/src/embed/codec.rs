//! Turning a program and the tables its native code uses into bytes and back, so a
//! compiled executable can carry them and report failures against the program's source.
//!
//! The encoding is plain: little-endian numbers, lengths before lists and strings, a tag
//! before each variant. It only has to be read by the same version of Fennec that wrote it.

use fennec_syntax::Sources;

pub use fennec_syntax::codec::{Decode, Encode, Reader, Writer};

use super::Tables;
use crate::packed::Element;

impl Encode for Element {
    fn encode(&self, to: &mut Writer) {
        match self {
            Element::Int => to.tag(0),
            Element::Float => to.tag(1),
            Element::Bool => to.tag(2),
            Element::U64 => to.tag(12),
            Element::F32 => to.tag(13),
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
            Element::Text => to.tag(10),
            Element::Object => to.tag(8),
            Element::Maybe(inner) => {
                to.tag(9);
                inner.encode(to);
            }
            Element::List(item) => {
                to.tag(7);
                item.encode(to);
            }
            Element::ListOf(number) => {
                to.tag(11);
                number.encode(to);
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
            12 => Element::U64,
            13 => Element::F32,
            3 => Element::Struct { name: Decode::decode(from)?, fields: Decode::decode(from)? },
            4 => Element::Boxed(Decode::decode(from)?),
            5 => Element::Str,
            10 => Element::Text,
            8 => Element::Object,
            9 => Element::Maybe(Decode::decode(from)?),
            7 => Element::List(Decode::decode(from)?),
            11 => Element::ListOf(Decode::decode(from)?),
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
    pub sources: Sources,
    pub tables: Tables,
    /// The text of each string literal, in the order native code numbers them.
    pub literals: Vec<String>,
}

impl Image {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut to = Writer::default();
        let files: Vec<(String, String)> =
            self.sources.files().map(|(path, text)| (path.to_owned(), text.to_owned())).collect();
        files.encode(&mut to);
        let tables = &self.tables;
        tables.names.encode(&mut to);
        tables.strings.encode(&mut to);
        tables.elements.encode(&mut to);
        tables.shapes.encode(&mut to);
        tables.cells.encode(&mut to);
        tables.blocks.encode(&mut to);
        tables.str_shape.encode(&mut to);
        tables.text_headers.encode(&mut to);
        self.literals.encode(&mut to);
        to.into_bytes()
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Image> {
        let from = &mut Reader::new(bytes);
        let mut sources = Sources::new();
        for (path, text) in Vec::<(String, String)>::decode(from)? {
            sources.add(path, text);
        }
        let tables = Tables {
            names: Decode::decode(from)?,
            strings: Decode::decode(from)?,
            elements: Decode::decode(from)?,
            shapes: Decode::decode(from)?,
            cells: Decode::decode(from)?,
            blocks: Decode::decode(from)?,
            str_shape: Decode::decode(from)?,
            text_headers: Decode::decode(from)?,
            ..Tables::default()
        };
        let literals = Decode::decode(from)?;
        Some(Image { sources, tables, literals })
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
        assert!(from.is_done(), "bytes left over");
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
        round_trip(&Element::Maybe(std::rc::Rc::new(Element::Str)));
    }

    /// An image comes back with its sources, tables and literals.
    #[test]
    fn an_image_comes_back_with_what_it_holds() {
        let mut sources = fennec_syntax::Sources::new();
        sources.add("main.fen".to_owned(), "def main(stdout: Stdout):\n".to_owned());
        let image = Image {
            sources,
            tables: crate::embed::Tables { str_shape: 7, ..crate::embed::Tables::default() },
            literals: vec!["fox".to_owned()],
        };
        let back = Image::from_bytes(&image.to_bytes()).unwrap();
        assert_eq!(back.tables.str_shape, 7);
        assert_eq!(back.literals, ["fox"]);
        assert_eq!(back.sources.files().count(), 1);
    }

    #[test]
    fn damaged_bytes_give_nothing() {
        assert!(Image::from_bytes(&[1, 2, 3]).is_none());
        assert!(Image::from_bytes(&[]).is_none());
    }
}
