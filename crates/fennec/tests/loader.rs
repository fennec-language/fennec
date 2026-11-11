//! The loader links a program's objects to what the system's linker writes, byte for byte. Each
//! program here is built with `FENNEC_LINK_KEEP`, which keeps the objects its link used and has
//! the linker leave every piece where it is, without rewriting any instruction. Then every
//! section the linker placed is linked again by the loader at the very same address, and the two
//! must be the same bytes, every relocation in them included, and say the same about unwinding.

use std::collections::BTreeMap;
use std::path::Path;

use crate::executables::{Within, fennec_command, runtime_built};

/// Examples of every kind of code: closures, generators, collections, JSON, interfaces, crashes.
const PROGRAMS: &[&str] = &[
    "examples/hello.fen",
    "examples/arithmetic.fen",
    "examples/functions_as_values.fen",
    "examples/generators.fen",
    "examples/dicts_large.fen",
    "examples/json.fen",
    "examples/interfaces.fen",
    "examples/game.fen",
    "examples/baked_constants.fen",
    "examples/fizzbuzz.fen",
    "examples/projects/aliases",
];

/// What an executable holds: each segment's addresses and where in the file it is, and each
/// defined symbol's address.
struct Executable {
    bytes: Vec<u8>,
    segments: Vec<(u64, u64, u64, u64)>,
    symbols: BTreeMap<String, u64>,
    /// Each entry of `__got` holding a program address, by where it is, with that address.
    got: BTreeMap<u64, u64>,
    /// Every defined symbol, local ones too, which many objects may each have one of: a name
    /// with an address, as the symbol table has them.
    every: std::collections::BTreeSet<(String, u64)>,
    /// Each section: its name, address and size. Two segments may each have one of a name.
    sections: Vec<(String, u64, u64)>,
}

fn word<const N: usize>(bytes: &[u8], at: usize) -> [u8; N] {
    bytes[at..at + N].try_into().unwrap()
}

impl Executable {
    fn read(path: &Path) -> Executable {
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(u32::from_le_bytes(word(&bytes, 0)), 0xfeed_facf, "an arm64 executable");
        let commands = u32::from_le_bytes(word(&bytes, 16));
        let (mut segments, mut symbols, mut at) = (Vec::new(), BTreeMap::new(), 32);
        // Every symbol's name by its index, the indirect symbol table, and `__stubs`: where it
        // is, how big, its first entry in the indirect table, and each stub's size.
        let (mut names, mut indirect, mut stubs, mut table) = (Vec::new(), Vec::new(), None, None);
        let mut every = std::collections::BTreeSet::new();
        let mut listed = Vec::new();
        for _ in 0..commands {
            let (cmd, size) =
                (u32::from_le_bytes(word(&bytes, at)), u32::from_le_bytes(word(&bytes, at + 4)));
            let u32_at = |offset: usize| u32::from_le_bytes(word(&bytes, at + offset)) as usize;
            if cmd == 0x19 {
                let field = |offset: usize| u64::from_le_bytes(word(&bytes, at + offset));
                segments.push((field(24), field(32), field(40), field(48)));
                for n in 0..u32_at(64) {
                    let header = at + 72 + 80 * n;
                    let name = String::from_utf8_lossy(&bytes[header..header + 16])
                        .trim_end_matches('\0')
                        .to_owned();
                    let addr = u64::from_le_bytes(word(&bytes, header + 32));
                    listed.push((name, addr, u64::from_le_bytes(word(&bytes, header + 40))));
                    if bytes[header..header + 16].starts_with(b"__got\0") {
                        let addr = u64::from_le_bytes(word(&bytes, header + 32));
                        table = Some((addr, u64::from_le_bytes(word(&bytes, header + 40))));
                    }
                    if bytes[header..header + 16].starts_with(b"__stubs\0") {
                        let addr = u64::from_le_bytes(word(&bytes, header + 32));
                        let size = u64::from_le_bytes(word(&bytes, header + 40));
                        let first = u32::from_le_bytes(word(&bytes, header + 68)) as usize;
                        let each = u64::from(u32::from_le_bytes(word(&bytes, header + 72)));
                        stubs = Some((addr, size, first, each));
                    }
                }
            } else if cmd == 0xb {
                let (offset, count) = (u32_at(56), u32_at(60));
                indirect =
                    (0..count).map(|n| u32::from_le_bytes(word(&bytes, offset + 4 * n)) as usize).collect();
            } else if cmd == 0x2 {
                let (symoff, nsyms, stroff) = (u32_at(8), u32_at(12), u32_at(16));
                for n in 0..nsyms {
                    let entry = symoff + 16 * n;
                    let (strx, n_type) = (u32::from_le_bytes(word(&bytes, entry)) as usize, bytes[entry + 4]);
                    let value = u64::from_le_bytes(word(&bytes, entry + 8));
                    let name_at = stroff + strx;
                    let len = bytes[name_at..].iter().position(|byte| *byte == 0).unwrap();
                    let name = String::from_utf8_lossy(&bytes[name_at..name_at + len]).into_owned();
                    if n_type & 0xe0 == 0 && n_type & 0x0e == 0x0e {
                        every.insert((name.clone(), value));
                        // Only an external symbol's name is the program's alone.
                        if n_type & 0x01 != 0 {
                            symbols.insert(name.clone(), value);
                        }
                    }
                    names.push(name);
                }
            }
            at += size as usize;
        }
        // An imported symbol, as the C library's `_memcpy`, is called through its stub.
        if let Some((addr, size, first, each)) = stubs.filter(|stubs| stubs.3 > 0) {
            for n in 0..usize::try_from(size / each).unwrap() {
                if let Some(name) = indirect.get(first + n).and_then(|symbol| names.get(*symbol)) {
                    symbols.entry(name.clone()).or_insert(addr + each * n as u64);
                }
            }
        }
        let mut exe = Executable { bytes, segments, symbols, got: BTreeMap::new(), every, sections: listed };
        // A rebase, in the chained fixups' 64-bit offset form: the target's offset from the image's
        // start in the low 36 bits, with the top bit clear. A bind, to another library, is left out.
        if let Some((addr, size)) = table {
            for n in 0..size / 8 {
                let entry = addr + 8 * n;
                let Some(raw) = exe.at(entry, 8) else { continue };
                let raw = u64::from_le_bytes(raw.try_into().unwrap());
                if raw >> 63 == 0 {
                    exe.got.insert(entry, 0x1_0000_0000 + (raw & 0xf_ffff_ffff));
                }
            }
        }
        exe
    }

    /// The bytes of the executable's file at `address`, as many as `len`, where it holds them.
    fn at(&self, address: u64, len: usize) -> Option<&[u8]> {
        let (vmaddr, _, fileoff, filesize) = *self
            .segments
            .iter()
            .find(|(vmaddr, vmsize, ..)| address >= *vmaddr && address < vmaddr + vmsize)?;
        let into = address - vmaddr;
        if into + len as u64 > filesize {
            return None;
        }
        let start = usize::try_from(fileoff + into).ok()?;
        self.bytes.get(start..start + len)
    }
}

/// The groups of instructions an object's `LC_LINKER_OPTIMIZATION_HINT` lets a linker rewrite,
/// each as the addresses of its instructions in the object's own address space.
fn hints(object: &[u8]) -> Vec<Vec<u64>> {
    let commands = u32::from_le_bytes(word(object, 16));
    let mut at = 32;
    for _ in 0..commands {
        let (cmd, size) = (u32::from_le_bytes(word(object, at)), u32::from_le_bytes(word(object, at + 4)));
        if cmd == 0x2e {
            let (dataoff, datasize) = (
                u32::from_le_bytes(word(object, at + 8)) as usize,
                u32::from_le_bytes(word(object, at + 12)) as usize,
            );
            let data = &object[dataoff..dataoff + datasize];
            let mut read = 0;
            let mut next = || {
                let (mut value, mut shift) = (0_u64, 0);
                while read < data.len() {
                    let byte = data[read];
                    read += 1;
                    value |= u64::from(byte & 0x7f) << shift;
                    shift += 7;
                    if byte & 0x80 == 0 {
                        break;
                    }
                }
                value
            };
            let mut groups = Vec::new();
            loop {
                let kind = next();
                if kind == 0 {
                    return groups;
                }
                let count = next();
                groups.push((0..count).map(|_| next()).collect());
            }
        }
        at += size as usize;
    }
    Vec::new()
}

/// What an instruction a hint may cover does, as far as the address it works out goes.
#[derive(Clone, Copy, Debug)]
enum Op {
    Nop,
    /// `adrp` or `adr`: a register set to an address relative to the instruction.
    Address {
        register: u32,
        value: i64,
    },
    /// `add` of an immediate.
    Add {
        register: u32,
        from: u32,
        value: i64,
    },
    /// A load or store at an unsigned offset from a register.
    Access {
        register: u32,
        base: u32,
        offset: i64,
    },
    /// A load of what's at an address relative to the instruction.
    Literal {
        register: u32,
        value: i64,
    },
}

fn signed(value: u32, bits: u32) -> i64 {
    let shift = 64 - bits;
    (i64::from(value) << shift) >> shift
}

fn decode(insn: u32, pc: u64) -> Option<Op> {
    let pc = pc.cast_signed();
    let register = insn & 0x1f;
    let from = (insn >> 5) & 0x1f;
    let imm12 = i64::from((insn >> 10) & 0xfff);
    if insn == 0xd503_201f {
        return Some(Op::Nop);
    }
    if insn & 0x1f00_0000 == 0x1000_0000 {
        let imm = signed(((insn >> 5) & 0x7_ffff) << 2 | ((insn >> 29) & 3), 21);
        let value = if insn & 0x8000_0000 != 0 { (pc & !0xfff) + (imm << 12) } else { pc + imm };
        return Some(Op::Address { register, value });
    }
    if insn & 0x7f80_0000 == 0x1100_0000 {
        let shifted = if insn & (1 << 22) != 0 { imm12 << 12 } else { imm12 };
        return Some(Op::Add { register, from, value: shifted });
    }
    if insn & 0x3b00_0000 == 0x3900_0000 {
        let size = insn >> 30;
        let scale = if (insn >> 26) & 1 == 1 && (insn >> 23) & 1 == 1 && size == 0 { 4 } else { size };
        return Some(Op::Access { register, base: from, offset: imm12 << scale });
    }
    if insn & 0x3b00_0000 == 0x1800_0000 {
        return Some(Op::Literal { register, value: pc + signed((insn >> 5) & 0x7_ffff, 19) * 4 });
    }
    None
}

/// What a hinted group of instructions works out in the end: the register its last instruction
/// sets and to what address, or the register its last access loads or stores and at what address.
fn effect(words: &[(u64, u32)]) -> Option<(u32, i64)> {
    let mut known: BTreeMap<u32, i64> = BTreeMap::new();
    let mut last = None;
    for (pc, insn) in words {
        match decode(*insn, *pc)? {
            Op::Nop => {}
            Op::Address { register, value } => {
                known.insert(register, value);
                last = Some((register, value));
            }
            Op::Add { register, from, value } => {
                let value = known.get(&from)? + value;
                known.insert(register, value);
                last = Some((register, value));
            }
            Op::Access { register, base, offset } => last = Some((register, known.get(&base)? + offset)),
            Op::Literal { register, value } => last = Some((register, value)),
        }
    }
    last
}

/// The bytes of section `number` of an object, read from its headers.
fn section_bytes(object: &[u8], number: usize) -> Option<Vec<u8>> {
    let header = 32 + 72 + 80 * number;
    let size = usize::try_from(u64::from_le_bytes(word(object, header + 40))).unwrap();
    let offset = u32::from_le_bytes(word(object, header + 48)) as usize;
    object.get(offset..offset + size).map(<[u8]>::to_vec)
}

/// The folder of LLVM's own tools, as the build finds them.
fn llvm_tool(name: &str) -> std::path::PathBuf {
    if let Some(folder) = std::env::var_os("FENNEC_LLVM_BIN") {
        return Path::new(&folder).join(name);
    }
    let sysroot = std::process::Command::new("rustc").args(["--print", "sysroot"]).output().unwrap();
    let host = std::process::Command::new("rustc").arg("-vV").output().unwrap();
    let host = String::from_utf8_lossy(&host.stdout);
    let host = host.lines().find_map(|line| line.strip_prefix("host: ")).unwrap().to_owned();
    Path::new(String::from_utf8_lossy(&sysroot.stdout).trim())
        .join("lib/rustlib")
        .join(host)
        .join("bin")
        .join(name)
}

/// What the linker says each function unwinds with, by its address.
fn linked_unwinding(executable: &Path) -> BTreeMap<u64, u32> {
    let said = std::process::Command::new(llvm_tool("llvm-objdump"))
        .args(["--macho", "--unwind-info"])
        .arg(executable)
        .output()
        .unwrap();
    let mut found = BTreeMap::new();
    for line in String::from_utf8_lossy(&said.stdout).lines() {
        let Some(rest) = line.split("function offset=0x").nth(1) else { continue };
        let Some((offset, rest)) = rest.split_once(',') else { continue };
        let Some(encoding) = rest.split("]=0x").nth(1) else { continue };
        let (Ok(offset), Ok(encoding)) =
            (u64::from_str_radix(offset, 16), u32::from_str_radix(encoding.trim(), 16))
        else {
            continue;
        };
        found.insert(0x1_0000_0000 + offset, encoding);
    }
    found
}

/// How much of a program was compared.
#[derive(Default)]
struct Tally {
    sections: usize,
    skipped: Vec<String>,
    unwinds: usize,
}

/// Builds `program`, keeping its objects, and gives them back with the executable.
fn built(program: &str, scratch: &Path) -> (Vec<Vec<u8>>, Executable, std::path::PathBuf) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(program);
    let (objects, executable) = (scratch.join("objects"), scratch.join("program"));
    let _ = std::fs::remove_dir_all(&objects);
    let built = fennec_command()
        .arg("build")
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .env("FENNEC_LINK_KEEP", &objects)
        .env("FENNEC_HOME", scratch.join("home"))
        .output_within()
        .unwrap();
    assert!(built.status.success(), "{program}: {}", String::from_utf8_lossy(&built.stderr));
    let mut paths: Vec<_> =
        std::fs::read_dir(&objects).unwrap().flatten().map(|entry| entry.path()).collect();
    paths.sort_by_key(|path| path.file_stem().and_then(|stem| stem.to_str()?.parse::<usize>().ok()));
    let objects = paths.iter().map(|path| std::fs::read(path).unwrap()).collect();
    let exe = Executable::read(&executable);
    (objects, exe, executable)
}

/// Each section at the address the linker gave it: found by any symbol in it, or for data no
/// external symbol names, as a literal pool the linker may have merged with others' or a table
/// of constants, at the first place in the executable holding its bytes.
fn placed(
    plan: &fennec_loader::Plan,
    exe: &Executable,
    objects: &[Vec<u8>],
) -> BTreeMap<(usize, usize), u64> {
    let mut placed = BTreeMap::new();
    // A weak symbol is the first object's copy, which is the one a linker keeps.
    let mut first = std::collections::BTreeSet::new();
    for section in plan.sections() {
        let key = (section.object, section.section);
        if section.name == "__compact_unwind" || section.name == "__eh_frame" {
            continue;
        }
        let named = section.symbols.iter().find_map(|(name, offset, weak)| {
            if *weak && !first.insert(name.clone()) {
                return None;
            }
            exe.symbols.get(name).map(|address| address - offset)
        });
        if let Some(at) = named {
            placed.insert(key, at);
            continue;
        }
        // Code is only ever placed by its symbols.
        if section.name == "__text" || section.size == 0 {
            continue;
        }
        let Some(bytes) = section_bytes(&objects[section.object], section.section) else { continue };
        let found = exe.segments.iter().find_map(|(vmaddr, _, fileoff, filesize)| {
            let from = usize::try_from(*fileoff).unwrap();
            let file = &exe.bytes[from..from + usize::try_from(*filesize).unwrap()];
            (0..file.len().saturating_sub(bytes.len()))
                .step_by(bytes.len())
                .find(|at| file[*at..*at + bytes.len()] == bytes[..])
                .map(|at| vmaddr + at as u64)
        });
        if let Some(address) = found {
            placed.insert(key, address);
        }
    }
    placed
}

/// Each symbol's slot in the offset table, and what each slot holds: the executable's entry
/// holding its address, or where there's none, because the linker loads it directly, a slot of its
/// own past the executable, in reach.
fn slots(exe: &Executable) -> (BTreeMap<u64, u64>, BTreeMap<String, u64>) {
    let mut slots: BTreeMap<u64, u64> = exe.got.clone();
    let mut slot_of: BTreeMap<String, u64> = BTreeMap::new();
    for (name, address) in &exe.symbols {
        let entry = exe.got.iter().find(|(_, held)| *held == address).map(|(entry, _)| *entry);
        let entry = entry.unwrap_or_else(|| {
            let fresh = 0x1_8000_0000 + 8 * slots.len() as u64;
            slots.insert(fresh, *address);
            fresh
        });
        slot_of.insert(name.clone(), entry);
    }
    (slots, slot_of)
}

/// Each piece of data a symbol starts, up to the next symbol, at the first place in the
/// executable holding its bytes: where the linker put a table or a literal it merged with the same
/// one in other objects, apart from the rest of its section.
fn pieces(
    plan: &fennec_loader::Plan,
    exe: &Executable,
    objects: &[Vec<u8>],
) -> BTreeMap<(usize, usize, u64), u64> {
    // Every piece, by where the executable may have it and its bytes, in the order of the objects.
    let mut wanted: BTreeMap<Wanted, Vec<Owner>> = BTreeMap::new();
    for section in plan.sections() {
        if section.name == "__text" || section.name == "__compact_unwind" || section.name == "__eh_frame" {
            continue;
        }
        let Some(bytes) = section_bytes(&objects[section.object], section.section) else { continue };
        let mut starts: Vec<u64> = section.symbols.iter().map(|(_, offset, _)| *offset).collect();
        starts.sort_unstable();
        starts.dedup();
        for (n, from) in starts.iter().enumerate() {
            let to = starts.get(n + 1).copied().unwrap_or(section.size);
            let piece = bytes[usize::try_from(*from).unwrap()..usize::try_from(to).unwrap()].to_vec();
            if piece.is_empty() {
                continue;
            }
            // A literal the linker merges into the constants.
            let place = if section.name.starts_with("__literal") {
                "__const".to_owned()
            } else {
                section.name.clone()
            };
            let unit = 1_u64 << section.align.min(from.trailing_zeros());
            wanted.entry((place, unit, piece)).or_default().push((section.object, section.section, *from));
        }
    }
    // Each copy the executable holds, aligned as the piece is. As many copies as objects have the
    // piece are theirs in order, as the linker lays objects out; fewer were merged into one.
    let mut found = BTreeMap::new();
    for ((place, unit, piece), owners) in wanted {
        let mut copies = Vec::new();
        for (_, addr, size) in exe.sections.iter().filter(|(name, ..)| *name == place) {
            let Some(file) = exe.at(*addr, usize::try_from(*size).unwrap()) else { continue };
            let aligned = (0..file.len().saturating_sub(piece.len()))
                .filter(|at| (addr + *at as u64).is_multiple_of(unit));
            copies.extend(
                aligned.filter(|at| file[*at..*at + piece.len()] == piece[..]).map(|at| addr + at as u64),
            );
        }
        for (n, owner) in owners.iter().enumerate() {
            let copy = if copies.len() == owners.len() { copies.get(n) } else { copies.first() };
            if let Some(copy) = copy {
                found.insert(*owner, *copy);
            }
        }
    }
    found
}

/// A piece by where the executable may have it, the alignment it has, and its bytes.
type Wanted = (String, u64, Vec<u8>);

/// Where a piece is in the objects: an object, a section, and how far into it.
type Owner = (usize, usize, u64);

/// Checks one section the loader linked against the linker's bytes for it: every word the same,
/// but for the groups the object's hints let a linker rewrite, which must work out the same.
fn same_section(
    program: &str,
    section: &fennec_loader::Linked,
    (theirs, start, object): (&[u8], u64, &[u8]),
    (slots, kept): (&BTreeMap<u64, u64>, &[(u64, u64)]),
) {
    let end = start + section.bytes.len() as u64;
    let inside = |offset: u64| kept.iter().any(|(from, to)| (*from..*to).contains(&offset));
    let groups: Vec<Vec<u64>> = hints(object)
        .into_iter()
        .filter(|group| group.iter().all(|at| (start..end).contains(at) && inside(at - start)))
        .collect();
    let hinted: std::collections::BTreeSet<u64> = groups.iter().flatten().map(|at| at - start).collect();
    let place =
        |offset: u64| format!("{program}: object {} at {:#x}", section.object, section.address + offset);
    for offset in (0..section.bytes.len() as u64).step_by(4) {
        if hinted.contains(&offset) || !inside(offset) {
            continue;
        }
        let at = usize::try_from(offset).unwrap();
        let end = (at + 4).min(section.bytes.len());
        assert_eq!(theirs[at..end], section.bytes[at..end], "{} isn't what the linker wrote", place(offset));
    }
    let words = |bytes: &[u8], group: &[u64]| -> Vec<(u64, u32)> {
        group
            .iter()
            .map(|at| {
                (
                    section.address + at - start,
                    u32::from_le_bytes(word(bytes, usize::try_from(at - start).unwrap())),
                )
            })
            .collect()
    };
    for group in &groups {
        // A load of a slot holding an address is that address.
        let meaning = |found: Option<(u32, i64)>| {
            found.map(|(register, value)| {
                let held =
                    u64::try_from(value).ok().and_then(|at| slots.get(&at)).map(|held| held.cast_signed());
                (register, held.unwrap_or(value))
            })
        };
        let (ours, linked) =
            (meaning(effect(&words(&section.bytes, group))), meaning(effect(&words(theirs, group))));
        assert!(
            ours.is_some() && ours == linked,
            "{}: a hinted group works out {ours:?}, not {linked:?}",
            place(group[0] - start)
        );
    }
}

/// Builds `program`, keeping its objects, and checks the loader links them as the linker did.
fn compare(program: &str, scratch: &Path, tally: &mut Tally) {
    let (objects, exe, executable) = built(program, scratch);
    let checked: Vec<fennec_loader::Checked> = objects
        .iter()
        .map(|bytes| fennec_loader::Checked::new(bytes.clone(), &fennec_blake3::hash(bytes)).unwrap())
        .collect();
    let plan = fennec_loader::Plan::new(&checked).unwrap_or_else(|refused| panic!("{program}: {refused}"));
    let placed = placed(&plan, &exe, &objects);
    // A name the executable defines once, external or not, is what the objects mean by it.
    let runtime = |name: &str| {
        exe.symbols.get(name).copied().or_else(|| {
            let mut found = exe.every.iter().filter(|(defined, _)| defined == name);
            match (found.next(), found.next()) {
                (Some((_, address)), None) => Some(*address),
                _ => None,
            }
        })
    };
    let (slots, slot_of) = slots(&exe);
    let pieces = pieces(&plan, &exe, &objects);
    let placement = fennec_loader::Placement {
        section: &|object, section| placed.get(&(object, section)).copied(),
        symbol: &|object, section, offset| pieces.get(&(object, section, offset)).copied(),
        slot: &|name| slot_of.get(name).copied(),
        runtime: &runtime,
    };
    let fennec_loader::PlacedLink { sections, unwind } =
        plan.link_placed(&placement).unwrap_or_else(|refused| panic!("{program}: {refused}"));
    let infos: BTreeMap<(usize, usize), fennec_loader::SectionInfo> =
        plan.sections().into_iter().map(|info| ((info.object, info.section), info)).collect();
    for section in sections {
        let section = match section {
            Ok(section) => section,
            // Only a section left out may keep another from linking.
            Err(refused) if refused.0.contains("isn't placed") => {
                tally.skipped.push(format!("{program}: {refused}"));
                continue;
            }
            Err(refused) => panic!("{program}: {refused}"),
        };
        // A zeroed section has nothing in the file to compare with.
        let Some(theirs) = exe.at(section.address, section.bytes.len()).filter(|_| !section.bytes.is_empty())
        else {
            continue;
        };
        // Only what the linker kept where this section put it: each symbol's piece, from it to the
        // next, at the address the linker gave that symbol. A weak copy it dropped is left out.
        let info = &infos[&(section.object, section.section)];
        let mut pieces: Vec<(u64, &str)> =
            info.symbols.iter().map(|(name, offset, _)| (*offset, name.as_str())).collect();
        pieces.sort_unstable();
        let mut kept = Vec::new();
        for (n, (offset, name)) in pieces.iter().enumerate() {
            let to = pieces.get(n + 1).map_or(info.size, |(next, _)| *next);
            if exe.every.contains(&((*name).to_owned(), section.address + offset)) {
                kept.push((*offset, to));
            }
        }
        let found = (theirs, info.start, objects[section.object].as_slice());
        same_section(program, &section, found, (&slots, &kept));
        tally.sections += 1;
    }
    let linked_unwind = linked_unwinding(&executable);
    for entry in &unwind {
        let Some(theirs) = linked_unwind.get(&entry.function) else { continue };
        // A function described in DWARF has its entry's place in `__eh_frame` in the low bits,
        // which depends on how the linker laid that out: only the mode has to be the same.
        let dwarf = |encoding: u32| encoding & 0x0f00_0000 == 0x0300_0000;
        if dwarf(entry.encoding) {
            assert!(
                dwarf(*theirs),
                "{program}: {:#x} unwinds as {:#x}, not {theirs:#x}",
                entry.function,
                entry.encoding
            );
        } else {
            assert_eq!(entry.encoding, *theirs, "{program}: how {:#x} unwinds", entry.function);
        }
        tally.unwinds += 1;
    }
}

#[test]
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn the_loader_links_each_program_as_the_linker_does() {
    if !runtime_built() {
        return;
    }
    let scratch =
        fennec_temp::Temp::at(std::env::temp_dir().join(format!("fennec-loader-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();
    // A section can't be compared when it refers to constants the linker split up and merged with
    // other objects', as a pool of float literals: no one address holds them as the object does.
    // Those stay a small part of what's compared.
    let mut tally = Tally::default();
    for program in PROGRAMS {
        compare(program, &scratch, &mut tally);
    }
    println!(
        "{} sections and {} unwind entries the same as the linker's; {} left out",
        tally.sections,
        tally.unwinds,
        tally.skipped.len()
    );
    assert!(
        tally.sections > 50 && tally.unwinds > 20 && tally.skipped.len() * 20 <= tally.sections,
        "{} sections and {} unwind entries compared, these left out:\n{}",
        tally.sections,
        tally.unwinds,
        tally.skipped.join("\n")
    );
}
