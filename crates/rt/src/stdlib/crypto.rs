//! `std.crypto`: hashes of text or bytes, as lowercase hex digits, or as the digest's own bytes
//! from the `_digest` forms. Text is hashed as its UTF-8 bytes. A BLAKE3 key is written as hex
//! too, so a derived key can be used as a key directly. `equal` compares two secrets in time that
//! depends only on their lengths, so it can't tell an attacker how much of a guess was right.

mod argon2;
mod blake2b;
mod sha256;

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};

use fennec_syntax::{Diagnostic, Span};

use crate::value::Value;

/// The bytes of a str or bytes value.
fn bytes_of(value: &Value) -> Option<&[u8]> {
    match value {
        Value::Str(text) => Some(text.as_bytes()),
        Value::Bytes(data) => Some(data),
        _ => None,
    }
}

/// A digest as its own bytes, for the `_digest` forms.
fn digest(bytes: &[u8]) -> Value {
    Value::Bytes(std::rc::Rc::from(bytes))
}

fn hex(bytes: &[u8]) -> Value {
    use std::fmt::Write;
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    Value::str(&text)
}

/// A hash being worked out a piece at a time, for `blake3` or `sha256` of chunks, by its handle.
enum Hashing {
    Blake3(Box<fennec_blake3::Hasher>),
    Sha256(sha256::Hasher),
}

static HASHING: LazyLock<Mutex<HashMap<i64, Hashing>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT: LazyLock<Mutex<i64>> = LazyLock::new(|| Mutex::new(1));

/// `crypto._start(kind)`, `crypto._add(handle, data)` and `crypto._end(handle)`: a hash of many
/// pieces, which `crypto.fen` gives each chunk to in turn.
fn hashing(name: &str, values: &[Value], span: Span) -> Result<Value, Diagnostic> {
    let wrong = || Diagnostic::new(format!("`{name}` was called with the wrong arguments"), span);
    let mut open = HASHING.lock().unwrap_or_else(PoisonError::into_inner);
    match (name, values) {
        ("crypto._start", [Value::Str(kind)]) => {
            let hashing = match kind.as_ref() {
                "blake3" => Hashing::Blake3(Box::new(fennec_blake3::Hasher::new())),
                "sha256" => Hashing::Sha256(sha256::Hasher::new()),
                _ => return Err(wrong()),
            };
            let mut next = NEXT.lock().unwrap_or_else(PoisonError::into_inner);
            let handle = *next;
            *next += 1;
            open.insert(handle, hashing);
            Ok(Value::Int(handle))
        }
        ("crypto._add", [Value::Int(handle), data]) => {
            let data =
                bytes_of(data).ok_or_else(|| Diagnostic::new("a chunk to hash is text or bytes", span))?;
            match open.get_mut(handle) {
                Some(Hashing::Blake3(hasher)) => {
                    hasher.update(data);
                }
                Some(Hashing::Sha256(hasher)) => hasher.update(data),
                None => return Err(wrong()),
            }
            Ok(Value::Unit)
        }
        ("crypto._end" | "crypto._end_digest", [Value::Int(handle)]) => {
            let written = if name == "crypto._end" { hex } else { digest };
            match open.remove(handle) {
                Some(Hashing::Blake3(hasher)) => Ok(written(hasher.finalize().as_bytes())),
                Some(Hashing::Sha256(hasher)) => Ok(written(&hasher.finish())),
                None => Err(wrong()),
            }
        }
        _ => Err(wrong()),
    }
}

/// Writes over `bytes` with zeros, through `black_box`, so a copy of a password is gone once it's
/// used rather than left for the allocator to hand out again.
pub(super) fn wipe(bytes: &mut [u8]) {
    for byte in bytes.iter_mut() {
        *byte = 0;
    }
    std::hint::black_box(bytes);
}

/// `work` on a thread of the pool for work that keeps a core busy, while this task parks, as a
/// password's hash takes tens of milliseconds and 19 MiB, or here outside a task.
fn off_worker<R: Send + 'static>(work: impl FnOnce() -> R + Send + 'static) -> R {
    #[cfg(any(test, feature = "threads"))]
    {
        crate::embed::serve::off_thread_heavy(work)
    }
    #[cfg(not(any(test, feature = "threads")))]
    {
        work()
    }
}

/// `crypto._hash_password(password, salt)`, `crypto._password_problem(stored)` and
/// `crypto._check_password(password, stored)`: Argon2id, which `crypto.fen` wraps.
fn passwords(name: &str, values: &[Value], span: Span) -> Result<Value, Diagnostic> {
    let wrong = || Diagnostic::new(format!("`{name}` was called with the wrong arguments"), span);
    match (name, values) {
        ("crypto._hash_password", [Value::Str(password), Value::Bytes(salt)]) => {
            let (mut password, salt) = (password.as_bytes().to_vec(), salt.to_vec());
            Ok(Value::str(&off_worker(move || {
                let stored = argon2::hash_password(&password, &salt);
                wipe(&mut password);
                stored
            })))
        }
        ("crypto._password_problem", [Value::Str(stored)]) => {
            Ok(Value::str(&argon2::read(stored).err().unwrap_or_default()))
        }
        ("crypto._check_password", [Value::Str(password), Value::Str(stored)]) => {
            let stored = argon2::read(stored).map_err(|_| wrong())?;
            let mut password = password.as_bytes().to_vec();
            Ok(Value::Bool(off_worker(move || {
                let right = argon2::matches(&password, &stored);
                wipe(&mut password);
                right
            })))
        }
        _ => Err(wrong()),
    }
}

/// One of these functions, by its full name.
pub(crate) fn run(name: &str, values: &[Value], span: Span) -> Result<Value, Diagnostic> {
    if matches!(name, "crypto._hash_password" | "crypto._password_problem" | "crypto._check_password") {
        return passwords(name, values, span);
    }
    if name.starts_with("crypto._") {
        return hashing(name, values, span);
    }
    let wrong = || Diagnostic::new(format!("`{name}` takes text or bytes"), span);
    let data = |at: usize| values.get(at).and_then(bytes_of).ok_or_else(wrong);
    // A `_digest` form gives the same digest as its own bytes, where the plain one gives hex.
    let (name, written): (&str, fn(&[u8]) -> Value) = match name.strip_suffix("_digest") {
        Some(plain) => (plain, digest),
        None => (name, hex),
    };
    match name {
        "crypto.blake3" => Ok(written(fennec_blake3::hash(data(0)?).as_bytes())),
        "crypto.sha256" => Ok(written(&sha256::digest(data(0)?))),
        "crypto.hmac_sha256" => Ok(written(&sha256::hmac(data(0)?, data(1)?))),
        "crypto.equal" => Ok(Value::Bool(equal(data(0)?, data(1)?))),
        "crypto.keyed_blake3" => {
            let Some(Value::Str(key)) = values.first() else { return Err(wrong()) };
            let key = fennec_blake3::Hash::from_hex(key).ok_or_else(|| {
                Diagnostic::new("a BLAKE3 key is 64 hex digits", span)
                    .with_help("make one with `derive_key(context, secret)`")
            })?;
            Ok(written(fennec_blake3::keyed_hash(key.as_bytes(), data(1)?).as_bytes()))
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
