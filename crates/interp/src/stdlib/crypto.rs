//! `std.crypto`: hashes of text or bytes, as lowercase hex digits. Text is hashed as its UTF-8
//! bytes. A BLAKE3 key is written as hex too, so a derived key can be used as a key directly.
//! `equal` compares two secrets in time that depends only on their lengths, so it can't tell
//! an attacker how much of a guess was right.

mod sha256;

use fennec_syntax::{Diagnostic, Span};

use super::{Module, no_constants};
use crate::call::CallArg;
use crate::collections::plain;
use crate::interp::Interp;
use crate::value::Value;

pub(crate) const MODULE: Module = Module {
    functions: &[
        "crypto.blake3",
        "crypto.derive_key",
        "crypto.equal",
        "crypto.hmac_sha256",
        "crypto.keyed_blake3",
        "crypto.sha256",
    ],
    call,
    constant: no_constants,
    methods: &[],
};

/// The bytes of a str or bytes value.
fn bytes_of(value: &Value) -> Option<&[u8]> {
    match value {
        Value::Str(text) => Some(text.as_bytes()),
        Value::Bytes(data) => Some(data),
        _ => None,
    }
}

fn hex(bytes: &[u8]) -> Value {
    use std::fmt::Write;
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    Value::str(&text)
}

fn call(_: &mut Interp<'_>, name: &str, args: Vec<CallArg>, span: Span) -> Result<Value, Diagnostic> {
    let values = plain(args, name, span)?;
    let wrong = || Diagnostic::new(format!("`{name}` takes text or bytes"), span);
    let data = |at: usize| values.get(at).and_then(bytes_of).ok_or_else(wrong);
    match name {
        "crypto.blake3" => Ok(hex(fennec_blake3::hash(data(0)?).as_bytes())),
        "crypto.sha256" => Ok(hex(&sha256::digest(data(0)?))),
        "crypto.hmac_sha256" => Ok(hex(&sha256::hmac(data(0)?, data(1)?))),
        "crypto.equal" => Ok(Value::Bool(equal(data(0)?, data(1)?))),
        "crypto.keyed_blake3" => {
            let Some(Value::Str(key)) = values.first() else { return Err(wrong()) };
            let key = fennec_blake3::Hash::from_hex(key).ok_or_else(|| {
                Diagnostic::new("a BLAKE3 key is 64 hex digits", span)
                    .with_help("make one with `derive_key(context, secret)`")
            })?;
            Ok(hex(fennec_blake3::keyed_hash(key.as_bytes(), data(1)?).as_bytes()))
        }
        "crypto.derive_key" => {
            let Some(Value::Str(context)) = values.first() else { return Err(wrong()) };
            Ok(hex(&fennec_blake3::derive_key(context, data(1)?)))
        }
        _ => Err(wrong()),
    }
}

/// Whether two byte strings are equal, looking at every byte whatever it finds, so the time
/// taken depends only on the lengths.
fn equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let differences = left.iter().zip(right).fold(0u8, |seen, (a, b)| seen | (a ^ b));
    std::hint::black_box(differences) == 0
}

#[cfg(test)]
mod tests {
    #[test]
    fn equal_needs_every_byte_the_same() {
        assert!(super::equal(b"secret", b"secret"));
        assert!(!super::equal(b"secret", b"secreT"));
        assert!(!super::equal(b"secret", b"secrets"));
        assert!(super::equal(b"", b""));
    }
}
