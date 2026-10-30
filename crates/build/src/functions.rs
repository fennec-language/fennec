//! Compiled functions in the cache: what a function's key is made of, how its machine code is
//! stored, and looking it up.
//!
//! A function's machine code is reused only when everything it was compiled from is the same.
//! The JIT lowers every function each run, which is quick, and keys its machine code by the
//! lowered IR, written with everything it refers to named rather than numbered, plus the
//! Fennec version and the target. The code refers to everything outside itself by symbol
//! name, never by address, so it's the same in every run; the JIT fills in the addresses when
//! it loads it.

use std::collections::HashMap;

use fennec_blake3::{Hash, Hasher};

use crate::cache::{Cache, VERSION};

/// Builds a function's cache key from everything its machine code depends on.
#[derive(Clone)]
pub struct FunctionKey {
    hasher: Hasher,
}

impl FunctionKey {
    /// A key for a function compiled by this Fennec for `target`: the CPU and every code
    /// generation setting, written out.
    pub fn new(target: &str) -> Self {
        let mut key =
            FunctionKey { hasher: Hasher::new_derive_key("fennec build cache 2026-10-30 function key") };
        key.part(b'v', VERSION.as_bytes());
        key.part(b't', target.as_bytes());
        key
    }

    /// The function's lowered IR, with what it refers to written by name.
    #[must_use]
    pub fn ir(mut self, text: &str) -> Self {
        self.part(b'i', text.as_bytes());
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

/// The first bytes of a pack, then the format's version.
const PACK_MAGIC: &[u8; 4] = b"FNPK";

/// The name of a program's pack: a hash of what names the program, like its absolute path.
fn pack_name(program: &str) -> String {
    format!("{}.bin", fennec_blake3::hash(program.as_bytes()).to_hex())
}

/// Every function one program compiled to, stored together, so a run reads one file and
/// writes one, however many functions it has. Each function is still found by its own key.
pub fn encode_pack(functions: &[(Hash, CompiledFunction)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(PACK_MAGIC);
    out.extend_from_slice(&FORMAT.to_le_bytes());
    out.extend_from_slice(&u32::try_from(functions.len()).unwrap_or(u32::MAX).to_le_bytes());
    for (key, function) in functions {
        out.extend_from_slice(key.as_bytes());
        put_bytes(&mut out, &function.encode());
    }
    out
}

/// Reads a pack written by `encode_pack`. A damaged pack reads as empty, so every function in
/// it is compiled again.
pub fn decode_pack(bytes: &[u8]) -> HashMap<Hash, CompiledFunction> {
    let mut reader = Reader { rest: bytes };
    let read = |reader: &mut Reader<'_>| -> Option<HashMap<Hash, CompiledFunction>> {
        if reader.take(4)? != PACK_MAGIC || reader.u32()? != FORMAT {
            return None;
        }
        let count = reader.u32()?;
        let mut functions = HashMap::new();
        for _ in 0..count {
            let key = Hash(reader.take(32)?.try_into().ok()?);
            functions.insert(key, CompiledFunction::decode(reader.bytes()?)?);
        }
        reader.rest.is_empty().then_some(functions)
    };
    read(&mut reader).unwrap_or_default()
}

impl Cache {
    /// The functions `program` compiled to last time, by key, marking its pack as used.
    pub fn pack(&self, program: &str) -> HashMap<Hash, CompiledFunction> {
        self.read("packs", &pack_name(program)).map(|bytes| decode_pack(&bytes)).unwrap_or_default()
    }

    /// Stores every function `program` compiled to this time, replacing its last pack.
    pub fn store_pack(&self, program: &str, functions: &[(Hash, CompiledFunction)]) -> std::io::Result<()> {
        self.write("packs", &pack_name(program), &encode_pack(functions))
    }
}
