//! Compiled functions in the cache: what a function's key is made of, how its machine code is
//! stored, and looking it up.
//!
//! A function's machine code is reused only when everything it was compiled from is the same.
//! Its key covers its own syntax tree, the signatures and layouts of what it uses, the bodies
//! of the functions it inlined, the Fennec version and the target. The code refers to
//! everything outside itself by symbol name, never by address, so it's the same in every run;
//! the JIT fills in the addresses when it loads it.

use fennec_blake3::{Hash, Hasher};

use crate::cache::{Cache, VERSION};

/// Builds a function's cache key from everything its machine code depends on.
#[derive(Clone)]
pub struct FunctionKey {
    hasher: Hasher,
}

impl FunctionKey {
    /// A key for a function compiled by this Fennec for `target`, like the ISA and its flags.
    pub fn new(target: &str) -> Self {
        let mut key =
            FunctionKey { hasher: Hasher::new_derive_key("fennec build cache 2026-10-30 function key") };
        key.part(b'v', VERSION.as_bytes());
        key.part(b't', target.as_bytes());
        key
    }

    /// The function's own syntax tree, as `tree_hash` gives it.
    #[must_use]
    pub fn tree(mut self, hash: &Hash) -> Self {
        self.part(b'f', hash.as_bytes());
        self
    }

    /// A signature or layout the function uses, like a callee's parameter types or a
    /// struct's fields, written out in full.
    #[must_use]
    pub fn uses(mut self, signature: &str) -> Self {
        self.part(b'u', signature.as_bytes());
        self
    }

    /// The tree hash of a function whose body was inlined into this one.
    #[must_use]
    pub fn inlines(mut self, hash: &Hash) -> Self {
        self.part(b'i', hash.as_bytes());
        self
    }

    pub fn finish(&self) -> Hash {
        self.hasher.finalize()
    }

    /// Adds one part, tagged and length-prefixed, so different parts can never run together
    /// into the same bytes.
    fn part(&mut self, tag: u8, bytes: &[u8]) {
        self.hasher.update(&[tag]);
        self.hasher.update(&u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
        self.hasher.update(bytes);
    }
}

/// A function's machine code, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompiledFunction {
    pub code: Vec<u8>,
    pub alignment: u64,
    pub relocations: Vec<Relocation>,
}

/// A place in the code to fill in with a symbol's address when it's loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relocation {
    /// Where in the code, in bytes.
    pub offset: u32,
    /// What kind of reference it is, numbered by the JIT, which knows the target's kinds.
    pub kind: u16,
    /// The symbol it refers to: a function, a runtime helper or a data object.
    pub symbol: String,
    pub addend: i64,
}

/// The first bytes of every stored function, then the format's version.
const MAGIC: &[u8; 4] = b"FNFC";
const FORMAT: u32 = 1;

impl CompiledFunction {
    /// The function as bytes: the magic and format, the alignment, the code, then each
    /// relocation. Numbers are little-endian, and every variable part is length-prefixed.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.code.len() + 16 + self.relocations.len() * 32);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&FORMAT.to_le_bytes());
        out.extend_from_slice(&self.alignment.to_le_bytes());
        put_bytes(&mut out, &self.code);
        out.extend_from_slice(&u32::try_from(self.relocations.len()).unwrap_or(u32::MAX).to_le_bytes());
        for relocation in &self.relocations {
            out.extend_from_slice(&relocation.offset.to_le_bytes());
            out.extend_from_slice(&relocation.kind.to_le_bytes());
            out.extend_from_slice(&relocation.addend.to_le_bytes());
            put_bytes(&mut out, relocation.symbol.as_bytes());
        }
        out
    }

    /// Reads a function written by `encode`. None if it's damaged or in another format, so
    /// the caller compiles the function again instead.
    pub fn decode(bytes: &[u8]) -> Option<CompiledFunction> {
        let mut reader = Reader { rest: bytes };
        if reader.take(4)? != MAGIC || reader.u32()? != FORMAT {
            return None;
        }
        let alignment = reader.u64()?;
        let code = reader.bytes()?.to_vec();
        let count = reader.u32()?;
        let mut relocations = Vec::new();
        for _ in 0..count {
            let offset = reader.u32()?;
            let kind = reader.u16()?;
            let addend = i64::from_le_bytes(reader.take(8)?.try_into().ok()?);
            let symbol = String::from_utf8(reader.bytes()?.to_vec()).ok()?;
            let in_code = usize::try_from(offset).is_ok_and(|offset| offset < code.len());
            if !in_code {
                return None;
            }
            relocations.push(Relocation { offset, kind, symbol, addend });
        }
        reader.rest.is_empty().then_some(CompiledFunction { code, alignment, relocations })
    }
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&u32::try_from(bytes.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(bytes);
}

/// Reads stored bytes front to back, failing on anything cut short.
struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let (taken, rest) = self.rest.split_at_checked(count)?;
        self.rest = rest;
        Some(taken)
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn bytes(&mut self) -> Option<&'a [u8]> {
        let count = usize::try_from(self.u32()?).ok()?;
        self.take(count)
    }
}

/// The name of a function's entry in the cache: spread over 256 folders by the key's first
/// byte, so no folder grows huge.
fn entry_name(key: &Hash) -> String {
    let hex = key.to_hex();
    format!("{}/{hex}.bin", hex.get(..2).unwrap_or("00"))
}

impl Cache {
    /// The machine code stored for a function's key, marking it as just used. None if there's
    /// none, or what's stored is damaged.
    pub fn compiled(&self, key: &Hash) -> Option<CompiledFunction> {
        self.read("functions", &entry_name(key)).and_then(|bytes| CompiledFunction::decode(&bytes))
    }

    /// Stores a function's machine code under its key.
    pub fn store_compiled(&self, key: &Hash, function: &CompiledFunction) -> std::io::Result<()> {
        self.write("functions", &entry_name(key), &function.encode())
    }
}
