//! `std.hash`: BLAKE3 hashes of a string's UTF-8 bytes, as 64 lowercase hex digits. Keys
//! are written the same way, so a derived key can be used as a key directly.

use fennec_syntax::{Diagnostic, Span};

use super::{Module, no_constants};
use crate::call::CallArg;
use crate::collections::plain;
use crate::interp::Interp;
use crate::value::Value;

pub(crate) const MODULE: Module = Module {
    functions: &["hash.blake3", "hash.derive_key", "hash.keyed_blake3"],
    call,
    constant: no_constants,
    methods: &[],
};

fn call(_: &mut Interp<'_>, name: &str, args: Vec<CallArg>, span: Span) -> Result<Value, Diagnostic> {
    let hex = |hash: fennec_blake3::Hash| Value::str(&hash.to_hex());
    match (name, plain(args, name, span)?.as_slice()) {
        ("hash.blake3", [Value::Str(text)]) => Ok(hex(fennec_blake3::hash(text.as_bytes()))),
        ("hash.keyed_blake3", [Value::Str(key), Value::Str(text)]) => {
            let key = fennec_blake3::Hash::from_hex(key).ok_or_else(|| {
                Diagnostic::new("a BLAKE3 key is 64 hex digits", span)
                    .with_help("make one with `derive_key(context, secret)`")
            })?;
            Ok(hex(fennec_blake3::keyed_hash(key.as_bytes(), text.as_bytes())))
        }
        ("hash.derive_key", [Value::Str(context), Value::Str(material)]) => {
            Ok(hex(fennec_blake3::Hash(fennec_blake3::derive_key(context, material.as_bytes()))))
        }
        _ => Err(Diagnostic::new(format!("`{name}` takes strings"), span)),
    }
}
