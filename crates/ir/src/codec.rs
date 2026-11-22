//! The IR's operands as bytes, for what a backend works out about a function and keeps from one
//! build to the next, handed to the process that builds (see `fennec_syntax::codec`).

use fennec_syntax::codec::{Decode, Encode, Reader, Writer};

use crate::{LocalId, Operand};

impl Encode for LocalId {
    fn encode(&self, to: &mut Writer) {
        self.0.encode(to);
    }
}

impl Decode for LocalId {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        usize::decode(from).map(LocalId)
    }
}

impl Encode for Operand {
    fn encode(&self, to: &mut Writer) {
        match self {
            Operand::Local(local) => {
                to.tag(0);
                local.encode(to);
            }
            Operand::Int(value) => {
                to.tag(1);
                value.encode(to);
            }
            Operand::Float(value) => {
                to.tag(2);
                value.encode(to);
            }
            Operand::Bool(value) => {
                to.tag(3);
                value.encode(to);
            }
            Operand::Str(value) => {
                to.tag(4);
                value.encode(to);
            }
            Operand::Bytes(value) => {
                to.tag(5);
                value.encode(to);
            }
            Operand::None => to.tag(6),
        }
    }
}

impl Decode for Operand {
    fn decode(from: &mut Reader<'_>) -> Option<Self> {
        Some(match from.tag()? {
            0 => Operand::Local(LocalId::decode(from)?),
            1 => Operand::Int(i64::decode(from)?),
            2 => Operand::Float(f64::decode(from)?),
            3 => Operand::Bool(bool::decode(from)?),
            4 => Operand::Str(String::decode(from)?),
            5 => Operand::Bytes(fennec_syntax::Blob::decode(from)?),
            6 => Operand::None,
            _ => return None,
        })
    }
}
