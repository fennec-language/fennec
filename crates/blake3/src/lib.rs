//! BLAKE3, written in-house so Fennec has no outside dependencies. The build cache hashes
//! source files and syntax trees with it, and `std.hash` gives it to Fennec programs.
//!
//! This is the portable reference design: no SIMD and no threads. It's checked against the
//! official test vectors. Comparing two `Hash`es isn't constant-time, so don't use `==` to
//! check a secret MAC.

mod compress;
mod tree;

use std::fmt;

use compress::{DERIVE_KEY_CONTEXT, DERIVE_KEY_MATERIAL, IV, KEYED_HASH, words};
use tree::Tree;

/// How many bytes a hash has.
pub const OUT_LEN: usize = 32;

/// How many bytes a key has.
pub const KEY_LEN: usize = 32;

/// The hash of some input.
pub fn hash(input: &[u8]) -> Hash {
    Hasher::new().update(input).finalize()
}

/// A keyed hash: a MAC, or a hash only someone with the key can reproduce.
pub fn keyed_hash(key: &[u8; KEY_LEN], input: &[u8]) -> Hash {
    Hasher::new_keyed(key).update(input).finalize()
}

/// A key derived from `material` for the purpose `context` names. The context should be a
/// fixed string unique to one use, like `"fennec build cache 2026-10-30 function keys"`.
pub fn derive_key(context: &str, material: &[u8]) -> [u8; KEY_LEN] {
    Hasher::new_derive_key(context).update(material).finalize().0
}

/// Hashes input given in any number of pieces. The result is the same however it's split.
#[derive(Clone)]
pub struct Hasher {
    tree: Tree,
}

impl Default for Hasher {
    fn default() -> Self {
        Self::new()
    }
}

impl Hasher {
    pub fn new() -> Self {
        Hasher { tree: Tree::new(IV, 0) }
    }

    pub fn new_keyed(key: &[u8; KEY_LEN]) -> Self {
        Hasher { tree: Tree::new(words(key), KEYED_HASH) }
    }

    pub fn new_derive_key(context: &str) -> Self {
        let mut context_key = [0; KEY_LEN];
        let mut context_tree = Tree::new(IV, DERIVE_KEY_CONTEXT);
        context_tree.update(context.as_bytes());
        context_tree.finalize(&mut context_key);
        Hasher { tree: Tree::new(words(&context_key), DERIVE_KEY_MATERIAL) }
    }

    pub fn update(&mut self, input: &[u8]) -> &mut Self {
        self.tree.update(input);
        self
    }

    pub fn finalize(&self) -> Hash {
        let mut out = [0; OUT_LEN];
        self.tree.finalize(&mut out);
        Hash(out)
    }

    /// Fills `out` with as many bytes of output as it holds. The first 32 are the hash.
    pub fn finalize_xof(&self, out: &mut [u8]) {
        self.tree.finalize(out);
    }
}

/// A 32-byte BLAKE3 hash. It prints as 64 lowercase hex digits.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Hash(pub [u8; OUT_LEN]);

impl Hash {
    pub fn as_bytes(&self) -> &[u8; OUT_LEN] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        self.to_string()
    }

    /// Reads 64 hex digits, in either case. Returns None for anything else.
    pub fn from_hex(text: &str) -> Option<Hash> {
        let digits = text.as_bytes();
        if digits.len() != OUT_LEN * 2 {
            return None;
        }
        let mut bytes = [0; OUT_LEN];
        for (byte, [high, low]) in bytes.iter_mut().zip(digits.as_chunks::<2>().0) {
            *byte = hex_value(*high)? << 4 | hex_value(*low)?;
        }
        Some(Hash(bytes))
    }
}

fn hex_value(digit: u8) -> Option<u8> {
    char::from(digit).to_digit(16).and_then(|value| u8::try_from(value).ok())
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hash({self})")
    }
}
