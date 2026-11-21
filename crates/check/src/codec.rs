//! What a check finds, as bytes and back, for a build handed to another process: the daemon
//! checks, and a short-lived child lowers and compiles from what it sends.
//!
//! It's read only by the same version of Fennec that wrote it, so it's written plainly: each
//! struct's fields in order, a tag before each kind of type.

use fennec_syntax::codec::{Decode, Encode, Reader, Writer};
use fennec_syntax::{codec_fields, codec_units};

use crate::shapes::Writer as Saver;
use crate::{Body, Crossing, Enum, Num, ParamType, Signature, Signatures, Struct, Type, Types};

codec_fields!(Signatures {
    functions,
    structs,
    enums,
    errors,
    locals,
    types,
    lock_verdicts,
    crossing,
    own_saving
});
codec_fields!(Signature { params, returns, can_fail, generator, generics });
codec_fields!(ParamType { name, ty, is_mut, default });
codec_fields!(Struct { generics, fields, rules, annotations });
codec_fields!(Enum { generics, cases, rules });
codec_fields!(Types { functions });
codec_fields!(Body { exprs, names, names_outer });
codec_fields!(Crossing { types, worked_out });
codec_units!(Num[I8, I16, I32, U8, U16, U32, U64, F32]);
codec_units!(Saver[Library, Program]);

impl Encode for Type {
    fn encode(&self, to: &mut Writer) {
        to.tag(tag_of(self));
        match self {
            Type::Unknown
            | Type::Unit
            | Type::None
            | Type::Bool
            | Type::Int
            | Type::Float
            | Type::Str
            | Type::LiteralStr
            | Type::Bytes => {}
            Type::Sized(num) => num.encode(to),
            Type::List(held)
            | Type::Buffer(held)
            | Type::Set(held)
            | Type::Optional(held)
            | Type::Id(held)
            | Type::Table(held) => held.encode(to),
            Type::Array(item, length) => (item, length).encode(to),
            Type::Dict(first, second) | Type::Result(first, second) => (first, second).encode(to),
            Type::Iterator(item, sent) => (item, sent).encode(to),
            Type::Tuple(types) | Type::Union(types) => types.encode(to),
            Type::Named(name) | Type::Function(name) | Type::Param(name) => name.encode(to),
            Type::Generic(name, args) => (name, args).encode(to),
            Type::Arrow(params, returns) => (params, returns).encode(to),
            Type::Errors(names) => names.encode(to),
            Type::Var(at) => at.encode(to),
        }
    }
}

/// The tag written before a type, which says which kind it is.
fn tag_of(ty: &Type) -> u8 {
    match ty {
        Type::Unknown => 0,
        Type::Unit => 1,
        Type::None => 2,
        Type::Bool => 3,
        Type::Int => 4,
        Type::Float => 5,
        Type::Sized(_) => 6,
        Type::Str => 7,
        Type::LiteralStr => 8,
        Type::Bytes => 9,
        Type::List(_) => 10,
        Type::Buffer(_) => 11,
        Type::Array(..) => 12,
        Type::Set(_) => 13,
        Type::Dict(..) => 14,
        Type::Iterator(..) => 15,
        Type::Tuple(_) => 16,
        Type::Optional(_) => 17,
        Type::Result(..) => 18,
        Type::Named(_) => 19,
        Type::Generic(..) => 20,
        Type::Id(_) => 21,
        Type::Table(_) => 22,
        Type::Function(_) => 23,
        Type::Arrow(..) => 24,
        Type::Param(_) => 25,
        Type::Errors(_) => 26,
        Type::Union(_) => 27,
        Type::Var(_) => 28,
    }
}

impl Decode for Type {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        Some(match from.tag()? {
            0 => Type::Unknown,
            1 => Type::Unit,
            2 => Type::None,
            3 => Type::Bool,
            4 => Type::Int,
            5 => Type::Float,
            6 => Type::Sized(Decode::decode(from)?),
            7 => Type::Str,
            8 => Type::LiteralStr,
            9 => Type::Bytes,
            10 => Type::List(Decode::decode(from)?),
            11 => Type::Buffer(Decode::decode(from)?),
            12 => Type::Array(Decode::decode(from)?, Decode::decode(from)?),
            13 => Type::Set(Decode::decode(from)?),
            14 => Type::Dict(Decode::decode(from)?, Decode::decode(from)?),
            15 => Type::Iterator(Decode::decode(from)?, Decode::decode(from)?),
            16 => Type::Tuple(Decode::decode(from)?),
            17 => Type::Optional(Decode::decode(from)?),
            18 => Type::Result(Decode::decode(from)?, Decode::decode(from)?),
            19 => Type::Named(Decode::decode(from)?),
            20 => Type::Generic(Decode::decode(from)?, Decode::decode(from)?),
            21 => Type::Id(Decode::decode(from)?),
            22 => Type::Table(Decode::decode(from)?),
            23 => Type::Function(Decode::decode(from)?),
            24 => Type::Arrow(Decode::decode(from)?, Decode::decode(from)?),
            25 => Type::Param(Decode::decode(from)?),
            26 => Type::Errors(Decode::decode(from)?),
            27 => Type::Union(Decode::decode(from)?),
            28 => Type::Var(Decode::decode(from)?),
            _ => return None,
        })
    }
}

impl Signatures {
    /// These signatures as bytes, which [`Signatures::from_bytes`] reads back as they are.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut to = Writer::default();
        self.encode(&mut to);
        to.into_bytes()
    }

    /// Signatures [`Signatures::to_bytes`] wrote, or nothing for bytes it didn't write: cut short,
    /// damaged, or with any left over.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Signatures> {
        let mut from = Reader::new(bytes);
        let signatures = Signatures::decode(&mut from)?;
        from.is_done().then_some(signatures)
    }
}

#[cfg(test)]
mod tests {
    use crate::{Signatures, check_to_emit, with_crossing};

    /// What checking each example finds, what crosses between tasks worked out, reads back from
    /// its bytes as it was and writes the same bytes again.
    #[test]
    fn what_every_example_checks_to_comes_back_as_it_was() {
        let examples = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
        let mut checked = 0;
        for entry in std::fs::read_dir(examples).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|ext| ext != "fen") {
                continue;
            }
            let Ok(module) = fennec_syntax::parse(&std::fs::read_to_string(&path).unwrap()) else { continue };
            let Ok(signatures) = with_crossing(|| check_to_emit(&module)) else { continue };
            let bytes = signatures.to_bytes();
            let back = Signatures::from_bytes(&bytes);
            assert!(back.as_ref() == Some(&signatures), "{} read back differently", path.display());
            assert!(
                back.is_some_and(|back| back.to_bytes() == bytes),
                "{} wrote other bytes",
                path.display()
            );
            checked += 1;
        }
        assert!(checked > 50, "only {checked} examples checked");
    }

    /// Bytes cut short or with more after them read as nothing.
    #[test]
    fn damaged_bytes_give_nothing() {
        let module = fennec_syntax::parse("def main(stdout: Stdout):\n    stdout.print(\"hi\")\n").unwrap();
        let bytes = check_to_emit(&module).unwrap().to_bytes();
        assert!(Signatures::from_bytes(&bytes).is_some());
        assert!(Signatures::from_bytes(&bytes[..bytes.len() - 1]).is_none());
        assert!(Signatures::from_bytes(&[bytes.as_slice(), &[0]].concat()).is_none());
        assert!(Signatures::from_bytes(&[]).is_none());
    }
}
