//! Reading an arm64 Mach-O object, as LLVM writes one, with every offset and size checked
//! against the bytes before it's used. Only what such an object holds is taken: one segment, a
//! symbol table, and the sections, symbols and relocations listed here.

use crate::{Refused, refuse};

const MH_MAGIC_64: u32 = 0xfeed_facf;
const CPU_TYPE_ARM64: u32 = 0x0100_000c;
const MH_OBJECT: u32 = 1;
const HEADER: usize = 32;

const LC_SYMTAB: u32 = 0x2;
const LC_DYSYMTAB: u32 = 0xb;
const LC_SEGMENT_64: u32 = 0x19;
const LC_LINKER_OPTIMIZATION_HINT: u32 = 0x2e;
const LC_BUILD_VERSION: u32 = 0x32;

/// The relocation that gives the next one its addend, in place of a symbol.
const ADDEND: u8 = 10;

const SEGMENT: usize = 72;
const SECTION: usize = 80;
const NLIST: usize = 16;
const RELOCATION: usize = 8;

/// The largest alignment a section may ask for: 2^15.
const MOST_ALIGN: u32 = 15;

/// What a section holds, which says where it goes and what a relocation in it may be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Instructions: `__TEXT,__text`.
    Code,
    /// Constants, which nothing writes.
    ReadOnly,
    /// Data the program may write, zeroed if it has no bytes in the file.
    ReadWrite { zeroed: bool },
    /// `__LD,__compact_unwind`: how each function unwinds, for registering, never mapped as is.
    CompactUnwind,
    /// `__TEXT,__eh_frame`: DWARF unwind information, for registering.
    EhFrame,
}

/// A section, by where its bytes are in the object and what they are.
#[derive(Clone, Debug)]
pub(crate) struct Section {
    pub(crate) name: String,
    pub(crate) kind: Kind,
    /// Its address in the object's own address space, which symbols and relocations count from.
    pub(crate) addr: u64,
    pub(crate) size: u64,
    pub(crate) align: u32,
    /// Its bytes, or none for a zeroed section.
    pub(crate) bytes: Vec<u8>,
    pub(crate) relocations: Vec<Relocation>,
}

/// A symbol: defined in a section at an address in the object's space, or undefined.
#[derive(Clone, Debug)]
pub(crate) struct Symbol {
    pub(crate) name: String,
    pub(crate) external: bool,
    /// The section it's in, from 0, and its address, when it's defined.
    pub(crate) defined: Option<(usize, u64)>,
    /// Whether it's a weak definition, which other objects may define too, the same: LLVM's
    /// `linkonce_odr`. One is kept, and every use goes to it.
    pub(crate) weak: bool,
}

/// A relocation as written, before anything's checked about its type.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Relocation {
    pub(crate) offset: u32,
    /// A symbol's index when `external`, else a section's number, from 1.
    pub(crate) symbol: u32,
    pub(crate) pc_relative: bool,
    pub(crate) length: u8,
    pub(crate) external: bool,
    pub(crate) kind: u8,
}

/// An object, read.
#[derive(Clone, Debug)]
pub(crate) struct Object {
    pub(crate) sections: Vec<Section>,
    pub(crate) symbols: Vec<Symbol>,
}

fn bytes(of: &[u8], at: usize, len: usize) -> Result<&[u8], Refused> {
    let end = at.checked_add(len).ok_or_else(|| Refused("an object's offset overflows".to_owned()))?;
    of.get(at..end).map_or_else(|| refuse(format!("an object ends before byte {end}")), Ok)
}

fn u32_at(of: &[u8], at: usize) -> Result<u32, Refused> {
    let got = bytes(of, at, 4)?;
    Ok(u32::from_le_bytes(got.try_into().map_err(|_| Refused("an object can't be read".to_owned()))?))
}

fn u64_at(of: &[u8], at: usize) -> Result<u64, Refused> {
    let got = bytes(of, at, 8)?;
    Ok(u64::from_le_bytes(got.try_into().map_err(|_| Refused("an object can't be read".to_owned()))?))
}

fn index(value: u64) -> Result<usize, Refused> {
    usize::try_from(value).map_or_else(|_| refuse("an object's offset is too big"), Ok)
}

/// A fixed-width name, up to its first zero byte.
fn name_at(of: &[u8], at: usize) -> Result<String, Refused> {
    let raw = bytes(of, at, 16)?;
    let len = raw.iter().position(|byte| *byte == 0).unwrap_or(raw.len());
    let name = raw.get(..len).unwrap_or_default();
    String::from_utf8(name.to_vec()).map_or_else(|_| refuse("a section's name isn't text"), Ok)
}

/// What a section of this segment, name and type holds, or a refusal for one it doesn't know.
fn kind_of(segment: &str, name: &str, flags: u32) -> Result<Kind, Refused> {
    let section_type = flags & 0xff;
    let kind = match (segment, name, section_type) {
        ("__TEXT", "__text", 0) => Kind::Code,
        ("__TEXT", "__const" | "__cstring", 0 | 2)
        | ("__TEXT", "__literal4", 3)
        | ("__TEXT", "__literal8", 4)
        | ("__TEXT", "__literal16", 0xe)
        | ("__DATA", "__const", 0) => Kind::ReadOnly,
        ("__DATA", "__data", 0) => Kind::ReadWrite { zeroed: false },
        ("__DATA", "__common" | "__bss", 1) => Kind::ReadWrite { zeroed: true },
        ("__LD", "__compact_unwind", 0) => Kind::CompactUnwind,
        ("__TEXT", "__eh_frame", 0xb) => Kind::EhFrame,
        _ => return refuse(format!("a section it doesn't take: {segment},{name} of type {section_type:#x}")),
    };
    // Instructions only where instructions go.
    let instructions = flags & 0x8000_0400 != 0;
    if instructions != (kind == Kind::Code) {
        return refuse(format!(
            "{segment},{name} says it holds instructions where it can't, or doesn't where it must"
        ));
    }
    Ok(kind)
}

/// Reads an object, refusing anything that isn't as an arm64 object LLVM wrote would be.
pub(crate) fn read(of: &[u8]) -> Result<Object, Refused> {
    if u32_at(of, 0)? != MH_MAGIC_64 || u32_at(of, 4)? != CPU_TYPE_ARM64 {
        return refuse("an object isn't arm64 Mach-O");
    }
    if u32_at(of, 12)? != MH_OBJECT {
        return refuse("an object isn't a relocatable object");
    }
    let commands = u32_at(of, 16)?;
    let commands_size = index(u64::from(u32_at(of, 20)?))?;
    let commands_end =
        HEADER.checked_add(commands_size).ok_or_else(|| Refused("load commands overflow".to_owned()))?;
    bytes(of, HEADER, commands_size)?;
    let (mut sections, mut symtab, mut segments) = (Vec::new(), None, 0);
    let mut at = HEADER;
    for _ in 0..commands {
        let cmd = u32_at(of, at)?;
        let size =
            index(u64::from(u32_at(of, at.checked_add(4).ok_or_else(|| Refused("overflow".to_owned()))?)?))?;
        let end = at.checked_add(size).ok_or_else(|| Refused("a load command overflows".to_owned()))?;
        if size < 8 || end > commands_end {
            return refuse("a load command runs past the load commands");
        }
        match cmd {
            LC_SEGMENT_64 => {
                segments += 1;
                sections = read_segment(of, at, size)?;
            }
            LC_SYMTAB => {
                if size != 24 || symtab.is_some() {
                    return refuse("a symbol table command isn't as it should be");
                }
                symtab = Some((
                    u32_at(of, at + 8)?,
                    u32_at(of, at + 12)?,
                    u32_at(of, at + 16)?,
                    u32_at(of, at + 20)?,
                ));
            }
            // What these hold isn't needed to link: the order of the symbol table, which platform
            // it's for, and hints a linker may use to shorten instructions.
            LC_DYSYMTAB | LC_BUILD_VERSION | LC_LINKER_OPTIMIZATION_HINT => {}
            _ => return refuse(format!("a load command it doesn't take: {cmd:#x}")),
        }
        at = end;
    }
    if segments != 1 {
        return refuse(format!("an object has {segments} segments, not one"));
    }
    let symbols = match symtab {
        Some((symoff, nsyms, stroff, strsize)) => {
            read_symbols(of, (symoff, nsyms), (stroff, strsize), sections.len())?
        }
        None => Vec::new(),
    };
    for section in &sections {
        // An addend's symbol field is the addend itself.
        for relocation in section.relocations.iter().filter(|relocation| relocation.kind != ADDEND) {
            let limit = if relocation.external { symbols.len() } else { sections.len() };
            let number = index(u64::from(relocation.symbol))?;
            if (relocation.external && number >= limit)
                || (!relocation.external && (number == 0 || number > limit))
            {
                return refuse(format!(
                    "a relocation in {} names a symbol or section that isn't there",
                    section.name
                ));
            }
        }
    }
    Ok(Object { sections, symbols })
}

fn read_segment(of: &[u8], at: usize, size: usize) -> Result<Vec<Section>, Refused> {
    let count = index(u64::from(u32_at(of, at + 64)?))?;
    let wanted = count.checked_mul(SECTION).and_then(|all| all.checked_add(SEGMENT));
    if wanted != Some(size) {
        return refuse("a segment's size doesn't fit its sections");
    }
    let mut sections: Vec<Section> = Vec::with_capacity(count);
    for n in 0..count {
        let header = at + SEGMENT + n * SECTION;
        let name = name_at(of, header)?;
        let segment = name_at(of, header + 16)?;
        let addr = u64_at(of, header + 32)?;
        let size = u64_at(of, header + 40)?;
        let offset = u32_at(of, header + 48)?;
        let align = u32_at(of, header + 52)?;
        let (reloff, nreloc, flags) =
            (u32_at(of, header + 56)?, u32_at(of, header + 60)?, u32_at(of, header + 64)?);
        let kind = kind_of(&segment, &name, flags)?;
        if align > MOST_ALIGN {
            return refuse(format!("{name} asks for alignment 2^{align}"));
        }
        if addr.checked_add(size).is_none() {
            return refuse(format!("{name}'s size overflows"));
        }
        let content = match kind {
            Kind::ReadWrite { zeroed: true } => Vec::new(),
            _ => bytes(of, index(u64::from(offset))?, index(size)?)?.to_vec(),
        };
        // Sections may not overlap, in the object's address space.
        let end = addr + size;
        if sections.iter().any(|other| addr < other.addr + other.size && other.addr < end) {
            return refuse(format!("{name} overlaps another section"));
        }
        let mut relocations = Vec::with_capacity(index(u64::from(nreloc))?);
        let table = bytes(
            of,
            index(u64::from(reloff))?,
            index(u64::from(nreloc))?
                .checked_mul(RELOCATION)
                .ok_or_else(|| Refused("overflow".to_owned()))?,
        )?;
        for entry in table.as_chunks::<RELOCATION>().0 {
            let address = u32_at(entry, 0)?;
            let packed = u32_at(entry, 4)?;
            // A scattered relocation, which arm64 objects never have.
            if address & 0x8000_0000 != 0 {
                return refuse(format!("a scattered relocation in {name}"));
            }
            relocations.push(Relocation {
                offset: address,
                symbol: packed & 0x00ff_ffff,
                pc_relative: (packed >> 24) & 1 == 1,
                length: u8::try_from((packed >> 25) & 3).unwrap_or(0),
                external: (packed >> 27) & 1 == 1,
                kind: u8::try_from(packed >> 28).unwrap_or(u8::MAX),
            });
        }
        sections.push(Section { name, kind, addr, size, align, bytes: content, relocations });
    }
    Ok(sections)
}

fn read_symbols(
    of: &[u8],
    (symoff, nsyms): (u32, u32),
    (stroff, strsize): (u32, u32),
    sections: usize,
) -> Result<Vec<Symbol>, Refused> {
    let table_len =
        index(u64::from(nsyms))?.checked_mul(NLIST).ok_or_else(|| Refused("overflow".to_owned()))?;
    let table = bytes(of, index(u64::from(symoff))?, table_len)?;
    let strings = bytes(of, index(u64::from(stroff))?, index(u64::from(strsize))?)?;
    let mut symbols = Vec::with_capacity(table.len() / NLIST);
    for entry in table.as_chunks::<NLIST>().0 {
        let strx = index(u64::from(u32_at(entry, 0)?))?;
        let (n_type, n_sect) = (*entry.get(4).unwrap_or(&0), *entry.get(5).unwrap_or(&0));
        let n_desc = u16::from_le_bytes([*entry.get(6).unwrap_or(&0), *entry.get(7).unwrap_or(&0)]);
        let value = u64_at(entry, 8)?;
        let rest = strings
            .get(strx..)
            .ok_or_else(|| Refused("a symbol's name is outside the string table".to_owned()))?;
        let len = rest
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| Refused("a symbol's name never ends".to_owned()))?;
        let name = String::from_utf8(rest.get(..len).unwrap_or_default().to_vec())
            .map_err(|_| Refused("a symbol's name isn't text".to_owned()))?;
        // Debugging entries, which LLVM writes only with debug information.
        if n_type & 0xe0 != 0 {
            return refuse(format!("a debugging symbol it doesn't take: {name}"));
        }
        let defined = match n_type & 0x0e {
            0x0 => None,
            0xe => {
                let number = usize::from(n_sect);
                if number == 0 || number > sections {
                    return refuse(format!("{name} is in a section that isn't there"));
                }
                Some((number - 1, value))
            }
            other => return refuse(format!("{name} is a kind of symbol it doesn't take: {other:#x}")),
        };
        // N_WEAK_DEF, which only a definition may have.
        let weak = n_desc & 0x0080 != 0;
        if weak && defined.is_none() {
            return refuse(format!("{name} is weak but isn't defined"));
        }
        symbols.push(Symbol { name, external: n_type & 0x01 != 0, defined, weak });
    }
    Ok(symbols)
}
