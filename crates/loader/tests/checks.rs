//! Each check the loader makes, on objects written here byte by byte: a small program it links
//! exactly as a linker would, and every way an object can be wrong, each refused with its reason.

use fennec_loader::{Checked, Manifest, Plan, Refused};

const CODE_BASE: u64 = 0x1_0000_0000;
const DATA_BASE: u64 = CODE_BASE + 0x1_0000;
/// Where the runtime's `_fennec_fail` is: farther than a branch reaches.
const FAIL_AT: u64 = 0x9_0000_0000;

const BRANCH26: u8 = 2;
const PAGE21: u8 = 3;
const PAGEOFF12: u8 = 4;
const UNSIGNED: u8 = 0;
const SUBTRACTOR: u8 = 1;
const ADDEND: u8 = 10;

#[derive(Clone)]
struct Sect {
    segment: &'static str,
    name: &'static str,
    flags: u32,
    align: u32,
    bytes: Vec<u8>,
    relocations: Vec<Rel>,
}

#[derive(Clone, Copy)]
struct Rel {
    offset: u32,
    symbol: u32,
    pc_relative: bool,
    length: u8,
    external: bool,
    kind: u8,
}

#[derive(Clone)]
struct Sym {
    name: &'static str,
    n_type: u8,
    n_sect: u8,
    value: u64,
    /// `n_desc`: 0x80 marks a weak definition.
    desc: u16,
}

fn text(insns: &[u32], relocations: Vec<Rel>) -> Sect {
    let bytes = insns.iter().flat_map(|insn| insn.to_le_bytes()).collect();
    Sect { segment: "__TEXT", name: "__text", flags: 0x8000_0400, align: 2, bytes, relocations }
}

fn constants(bytes: Vec<u8>) -> Sect {
    Sect { segment: "__DATA", name: "__const", flags: 0, align: 3, bytes, relocations: Vec::new() }
}

fn rel(offset: u32, symbol: u32, kind: u8) -> Rel {
    let pc_relative = matches!(kind, BRANCH26 | PAGE21);
    Rel { offset, symbol, pc_relative, length: 2, external: true, kind }
}

fn defined(name: &'static str, n_sect: u8, value: u64) -> Sym {
    Sym { name, n_type: 0x0f, n_sect, value, desc: 0 }
}

fn undefined(name: &'static str) -> Sym {
    Sym { name, n_type: 0x01, n_sect: 0, value: 0, desc: 0 }
}

fn put32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn name16(out: &mut Vec<u8>, name: &str) {
    let mut field = [0_u8; 16];
    field[..name.len()].copy_from_slice(name.as_bytes());
    out.extend_from_slice(&field);
}

fn n32(value: usize) -> u32 {
    u32::try_from(value).unwrap()
}

/// Each section's header: its names, address, size, where its bytes and relocations are.
fn section_headers(out: &mut Vec<u8>, sections: &[Sect], layout: &[(u64, usize)], reloc_at: &[usize]) {
    for ((section, (addr, offset)), reloff) in sections.iter().zip(layout).zip(reloc_at) {
        name16(out, section.name);
        name16(out, section.segment);
        put64(out, *addr);
        put64(out, section.bytes.len() as u64);
        let words = [
            n32(*offset),
            section.align,
            n32(*reloff),
            n32(section.relocations.len()),
            section.flags,
            0,
            0,
            0,
        ];
        for word in words {
            put32(out, word);
        }
    }
}

/// An arm64 Mach-O object of these sections and symbols, laid out as LLVM lays one out.
fn object(sections: &[Sect], symbols: &[Sym]) -> Vec<u8> {
    let commands = 72 + 80 * sections.len() + 24;
    let mut at = 32 + commands;
    let mut addr = 0_u64;
    let mut layout = Vec::new();
    for section in sections {
        let unit = 1_u64 << section.align;
        addr = (addr + unit - 1) & !(unit - 1);
        layout.push((addr, at));
        addr += section.bytes.len() as u64;
        at += section.bytes.len();
    }
    let mut reloc_at = Vec::new();
    for section in sections {
        reloc_at.push(at);
        at += 8 * section.relocations.len();
    }
    let symoff = at;
    let mut strings = vec![0_u8];
    let mut strx = Vec::new();
    for symbol in symbols {
        strx.push(n32(strings.len()));
        strings.extend_from_slice(symbol.name.as_bytes());
        strings.push(0);
    }
    let stroff = symoff + 16 * symbols.len();
    let mut out = Vec::new();
    for word in [0xfeed_facf_u32, 0x0100_000c, 0, 1, 2, n32(commands), 0, 0] {
        put32(&mut out, word);
    }
    put32(&mut out, 0x19);
    put32(&mut out, n32(72 + 80 * sections.len()));
    name16(&mut out, "");
    for word in [0_u64, addr, (32 + commands) as u64, addr] {
        put64(&mut out, word);
    }
    for word in [7_u32, 7, n32(sections.len()), 0] {
        put32(&mut out, word);
    }
    section_headers(&mut out, sections, &layout, &reloc_at);
    for word in [0x2_u32, 24, n32(symoff), n32(symbols.len()), n32(stroff), n32(strings.len())] {
        put32(&mut out, word);
    }
    for section in sections {
        out.extend_from_slice(&section.bytes);
    }
    for section in sections {
        for rel in &section.relocations {
            put32(&mut out, rel.offset);
            let packed = rel.symbol
                | (u32::from(rel.pc_relative) << 24)
                | (u32::from(rel.length) << 25)
                | (u32::from(rel.external) << 27)
                | (u32::from(rel.kind) << 28);
            put32(&mut out, packed);
        }
    }
    for (symbol, strx) in symbols.iter().zip(&strx) {
        put32(&mut out, *strx);
        out.push(symbol.n_type);
        out.push(symbol.n_sect);
        out.extend_from_slice(&symbol.desc.to_le_bytes());
        put64(&mut out, symbol.value);
    }
    out.extend_from_slice(&strings);
    out
}

fn checked(bytes: &[u8]) -> Checked {
    Checked::new(bytes.to_vec(), &fennec_blake3::hash(bytes)).unwrap()
}

/// The program: `main` loads a value from the other object's constants, calls the other
/// object's `helper`, then calls the runtime's `_fennec_fail`, which is far away.
fn main_object(insns: &[u32], relocations: Vec<Rel>) -> Vec<u8> {
    let symbols =
        [defined("_main", 1, 0), undefined("_helper"), undefined("_value"), undefined("_fennec_fail")];
    object(&[text(insns, relocations)], &symbols)
}

const MAIN: [u32; 5] = [0x9000_0008, 0xf940_0100, 0x9400_0000, 0x9400_0000, 0xd65f_03c0];

fn main_relocations() -> Vec<Rel> {
    vec![rel(0, 2, PAGE21), rel(4, 2, PAGEOFF12), rel(8, 1, BRANCH26), rel(12, 3, BRANCH26)]
}

fn helper_object() -> Vec<u8> {
    let sections = [text(&[0xd65f_03c0], Vec::new()), constants(42_u64.to_le_bytes().to_vec())];
    object(&sections, &[defined("_helper", 1, 0), defined("_value", 2, 8)])
}

fn runtime(name: &str) -> Option<u64> {
    (name == "_fennec_fail").then_some(FAIL_AT)
}

fn link(objects: &[Vec<u8>]) -> Result<fennec_loader::Image, Refused> {
    let checked: Vec<Checked> = objects.iter().map(|object| checked(object)).collect();
    Plan::new(&checked)?.link(CODE_BASE, DATA_BASE, &runtime)
}

fn insn(code: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(code[at..at + 4].try_into().unwrap())
}

/// Each relocation written as a linker writes it, a far call through its stub, and `main` where
/// it is.
#[test]
fn a_program_is_linked_as_a_linker_links_it() {
    let image = link(&[main_object(&MAIN, main_relocations()), helper_object()]).unwrap();
    // `adrp x8, _value@PAGE`: the constants are 16 pages on.
    assert_eq!(insn(&image.code, 0), 0x9000_0088);
    // `ldr x0, [x8, _value@PAGEOFF]`: at the start of its page.
    assert_eq!(insn(&image.code, 4), 0xf940_0100);
    // `bl _helper`, 12 bytes on.
    assert_eq!(insn(&image.code, 8), 0x9400_0003);
    // `bl _fennec_fail`, through the stub after the code, at 32.
    assert_eq!(insn(&image.code, 12), 0x9400_0005);
    assert_eq!(insn(&image.code, 32), 0x5800_0050);
    assert_eq!(insn(&image.code, 36), 0xd61f_0200);
    assert_eq!(image.code[40..48], FAIL_AT.to_le_bytes());
    assert_eq!(image.data[..8], 42_u64.to_le_bytes());
    assert_eq!(image.entry, CODE_BASE);
}

/// The same objects link to the same image, every time.
#[test]
fn linking_is_the_same_each_time() {
    let objects = [main_object(&MAIN, main_relocations()), helper_object()];
    assert_eq!(link(&objects).unwrap(), link(&objects).unwrap());
}

/// An object is taken only when its bytes are the ones the build made: one byte changed anywhere
/// refuses it.
#[test]
fn an_object_is_taken_only_with_the_hash_the_build_gave() {
    let object = main_object(&MAIN, main_relocations());
    let hash = fennec_blake3::hash(&object);
    assert!(Checked::new(object.clone(), &hash).is_ok());
    for at in [0, object.len() / 2, object.len() - 1] {
        let mut changed = object.clone();
        changed[at] ^= 1;
        let refused = Checked::new(changed, &hash).unwrap_err();
        assert!(refused.0.contains("hash"), "{refused}");
    }
}

/// What's linked is what was read and checked: a file replaced after it's read changes nothing.
#[test]
fn a_file_replaced_after_its_check_isnt_what_gets_linked() {
    let dir = std::env::temp_dir().join(format!("fennec-loader-once-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (main, helper) = (main_object(&MAIN, main_relocations()), helper_object());
    let paths = [dir.join("main.o"), dir.join("helper.o")];
    std::fs::write(&paths[0], &main).unwrap();
    std::fs::write(&paths[1], &helper).unwrap();
    let objects = vec![
        (paths[0].clone(), fennec_blake3::hash(&main)),
        (paths[1].clone(), fennec_blake3::hash(&helper)),
    ];
    let manifest = Manifest { build: "this build".to_owned(), objects };
    let read = manifest.checked("this build").unwrap();
    // Swapped for other code after the check.
    std::fs::write(&paths[0], main_object(&[0xd65f_03c0; 5], Vec::new())).unwrap();
    let image = Plan::new(&read).unwrap().link(CODE_BASE, DATA_BASE, &runtime).unwrap();
    assert_eq!(image, link(&[main, helper]).unwrap());
    // And read again now, it's refused.
    assert!(manifest.checked("this build").is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Objects from another build of Fennec are never loaded with this one's runtime.
#[test]
fn objects_from_another_build_are_refused() {
    let manifest = Manifest { build: "an older build".to_owned(), objects: Vec::new() };
    let refused = manifest.checked("this build").unwrap_err();
    assert!(refused.0.contains("an older build") && refused.0.contains("this build"), "{refused}");
}

/// Refuses `objects`, saying something with `why` in it.
fn refused(objects: &[Vec<u8>], why: &str) {
    match link(objects) {
        Err(Refused(said)) => assert!(said.contains(why), "refused saying {said:?}, not {why:?}"),
        Ok(_) => panic!("linked what should be refused for {why:?}"),
    }
}

fn with_helper(main: Vec<u8>) -> Vec<Vec<u8>> {
    vec![main, helper_object()]
}

#[test]
fn a_relocation_it_doesnt_know_is_refused() {
    let mut relocations = main_relocations();
    // TLVP_LOAD_PAGE21, a thread-local's, which no Fennec object has.
    relocations[0].kind = 8;
    refused(&with_helper(main_object(&MAIN, relocations)), "doesn't take");
}

#[test]
fn a_relocation_of_the_wrong_shape_is_refused() {
    let mut long = main_relocations();
    long[2].length = 3;
    refused(&with_helper(main_object(&MAIN, long)), "isn't code");
    let mut relative = main_relocations();
    relative[1].pc_relative = true;
    refused(&with_helper(main_object(&MAIN, relative)), "pc-relative");
    let mut on_ret = main_relocations();
    on_ret[2].offset = 16;
    refused(&with_helper(main_object(&MAIN, on_ret)), "isn't `b` or `bl`");
}

#[test]
fn an_addend_or_subtraction_left_hanging_is_refused() {
    let mut addend = main_relocations();
    addend.push(Rel { offset: 16, symbol: 4, pc_relative: false, length: 2, external: false, kind: ADDEND });
    refused(&with_helper(main_object(&MAIN, addend)), "partway");
    let mut before_unsigned = main_relocations();
    before_unsigned.insert(
        0,
        Rel { offset: 0, symbol: 4, pc_relative: false, length: 2, external: false, kind: ADDEND },
    );
    before_unsigned[1].kind = UNSIGNED;
    before_unsigned[1].pc_relative = false;
    refused(&with_helper(main_object(&MAIN, before_unsigned)), "isn't followed");
    let data = Sect {
        relocations: vec![
            Rel { offset: 0, symbol: 0, pc_relative: false, length: 3, external: true, kind: SUBTRACTOR },
            Rel { offset: 8, symbol: 1, pc_relative: false, length: 3, external: true, kind: UNSIGNED },
        ],
        ..constants(vec![0; 16])
    };
    // The constants start at 8, after the 4 bytes of code, aligned.
    let symbols = [defined("_main", 1, 0), defined("_value", 2, 8)];
    let lone = object(&[text(&[0xd65f_03c0], Vec::new()), data], &symbols);
    refused(&[lone], "isn't followed by its address");
}

#[test]
fn a_symbol_nothing_defines_or_two_objects_define_is_refused() {
    let symbols = [defined("_main", 1, 0), undefined("_helper"), undefined("_value"), undefined("_nowhere")];
    let nowhere = object(&[text(&MAIN, main_relocations())], &symbols);
    refused(&with_helper(nowhere), "nothing defines _nowhere");
    let second_main = object(&[text(&[0xd65f_03c0], Vec::new())], &[defined("_main", 1, 0)]);
    refused(
        &[main_object(&MAIN, main_relocations()), helper_object(), second_main],
        "_main is defined twice",
    );
}

#[test]
fn a_relocation_outside_its_section_is_refused() {
    let mut past = main_relocations();
    past[0].offset = 100;
    refused(&with_helper(main_object(&MAIN, past)), "past the end");
    let mut overhanging = main_relocations();
    overhanging[0].offset = 18;
    refused(&with_helper(main_object(&MAIN, overhanging)), "past the end");
    let mut no_symbol = main_relocations();
    no_symbol[0].symbol = 99;
    refused(&with_helper(main_object(&MAIN, no_symbol)), "isn't there");
}

#[test]
fn a_target_out_of_reach_or_misaligned_is_refused() {
    let checked: Vec<Checked> =
        with_helper(main_object(&MAIN, main_relocations())).iter().map(|it| checked(it)).collect();
    let plan = Plan::new(&checked).unwrap();
    // Data 5 GB from code: past what `adrp` reaches.
    let far = plan.link(CODE_BASE, CODE_BASE + (5 << 30), &runtime).unwrap_err();
    assert!(far.0.contains("can't reach"), "{far}");
    // `_value` 4 bytes into the constants, which an 8-byte load can't take.
    let sections = [text(&[0xd65f_03c0], Vec::new()), constants(vec![0; 16])];
    let odd = object(&sections, &[defined("_helper", 1, 0), defined("_value", 2, 12)]);
    refused(&[main_object(&MAIN, main_relocations()), odd], "isn't aligned");
}

#[test]
fn sections_that_overlap_or_run_past_the_object_are_refused() {
    // The second section's header starts 80 bytes after the first's.
    let (first, second) = (32 + 72, 32 + 72 + 80);
    let mut overlapping = helper_object();
    overlapping[second + 32..second + 40].copy_from_slice(&0_u64.to_le_bytes());
    refused(&[main_object(&MAIN, main_relocations()), overlapping], "overlaps");
    let mut long = helper_object();
    long[first + 40..first + 48].copy_from_slice(&(1_u64 << 40).to_le_bytes());
    refused(&[main_object(&MAIN, main_relocations()), long], "ends before");
    let mut aligned = helper_object();
    aligned[first + 52..first + 56].copy_from_slice(&16_u32.to_le_bytes());
    refused(&[main_object(&MAIN, main_relocations()), aligned], "alignment");
}

#[test]
fn a_section_or_kind_of_object_it_doesnt_know_is_refused() {
    let weird = Sect { name: "__weird", ..constants(vec![0; 8]) };
    refused(
        &[object(&[text(&[0xd65f_03c0], Vec::new()), weird], &[defined("_main", 1, 0)])],
        "__DATA,__weird",
    );
    let claims_code = Sect { flags: 0x8000_0400, ..constants(vec![0; 8]) };
    refused(
        &[object(&[text(&[0xd65f_03c0], Vec::new()), claims_code], &[defined("_main", 1, 0)])],
        "instructions",
    );
    let mut x86 = main_object(&MAIN, main_relocations());
    x86[4..8].copy_from_slice(&0x0100_0007_u32.to_le_bytes());
    refused(&[x86], "isn't arm64");
    let mut executable = main_object(&MAIN, main_relocations());
    executable[12..16].copy_from_slice(&2_u32.to_le_bytes());
    refused(&[executable], "relocatable");
}

#[test]
fn load_commands_and_tables_out_of_bounds_are_refused() {
    let mut short_commands = main_object(&MAIN, main_relocations());
    short_commands[20..24].copy_from_slice(&40_u32.to_le_bytes());
    refused(&[short_commands], "past the load commands");
    // The symbol table command, after the segment with one section.
    let symtab = 32 + 72 + 80;
    let mut unknown = main_object(&MAIN, main_relocations());
    unknown[symtab..symtab + 4].copy_from_slice(&0x22_u32.to_le_bytes());
    refused(&[unknown], "load command it doesn't take");
    let mut name = main_object(&MAIN, main_relocations());
    let symoff = u32::from_le_bytes(name[symtab + 8..symtab + 12].try_into().unwrap()) as usize;
    name[symoff..symoff + 4].copy_from_slice(&0xffff_u32.to_le_bytes());
    refused(&[name], "outside the string table");
}

#[test]
fn a_debugging_symbol_or_a_scattered_relocation_is_refused() {
    let stab = Sym { name: "_main", n_type: 0x24, n_sect: 1, value: 0, desc: 0 };
    refused(&[object(&[text(&[0xd65f_03c0], Vec::new())], &[stab])], "debugging symbol");
    let mut scattered = main_object(&MAIN, main_relocations());
    let reloff = u32::from_le_bytes(scattered[32 + 72 + 56..32 + 72 + 60].try_into().unwrap()) as usize;
    scattered[reloff + 3] |= 0x80;
    refused(&[scattered], "scattered");
}

#[test]
fn a_program_without_main_is_refused() {
    let lone = object(&[text(&[0xd65f_03c0], Vec::new())], &[defined("_other", 1, 0)]);
    refused(&[lone], "no object defines main");
}

const GOT_LOAD_PAGE21: u8 = 5;
const GOT_LOAD_PAGEOFF12: u8 = 6;

/// `main` loads `_fennec_fail`'s address out of its slot in the offset table: `adrp x8, slot@PAGE;
/// ldr x8, [x8, slot@PAGEOFF]; ret`.
fn got_main(relocations: Vec<Rel>) -> Vec<u8> {
    let symbols = [defined("_main", 1, 0), undefined("_fennec_fail")];
    object(&[text(&[0x9000_0008, 0xf940_0108, 0xd65f_03c0], relocations)], &symbols)
}

fn got_relocations() -> Vec<Rel> {
    vec![
        Rel { offset: 0, symbol: 1, pc_relative: true, length: 2, external: true, kind: GOT_LOAD_PAGE21 },
        Rel { offset: 4, symbol: 1, pc_relative: false, length: 2, external: true, kind: GOT_LOAD_PAGEOFF12 },
    ]
}

/// A load through the offset table gets the page of a slot after the data, which holds the
/// symbol's address, and the slot's offset in that page.
#[test]
fn a_load_through_the_offset_table_reads_its_slot() {
    let image = link(&[got_main(got_relocations())]).unwrap();
    // No data but the one slot, at the data's start: 16 pages on.
    assert_eq!(image.data, FAIL_AT.to_le_bytes());
    assert_eq!(insn(&image.code, 0), 0x9000_0088);
    assert_eq!(insn(&image.code, 4), 0xf940_0108);
}

#[test]
fn a_load_through_the_offset_table_of_the_wrong_shape_is_refused() {
    let mut not_external = got_relocations();
    not_external[0].external = false;
    refused(&[got_main(not_external)], "names no symbol");
    let mut relative = got_relocations();
    relative[1].pc_relative = true;
    refused(&[got_main(relative)], "isn't as it should be");
    let mut absolute = got_relocations();
    absolute[0].pc_relative = false;
    refused(&[got_main(absolute)], "isn't as it should be");
    let mut with_addend = got_relocations();
    with_addend.insert(
        0,
        Rel { offset: 0, symbol: 8, pc_relative: false, length: 2, external: false, kind: ADDEND },
    );
    refused(&[got_main(with_addend)], "isn't followed by a relocation that takes it");
    // An `add` can't take an address out of a slot: only a 64-bit `ldr` can.
    let symbols = [defined("_main", 1, 0), undefined("_fennec_fail")];
    let on_add = object(&[text(&[0x9000_0008, 0x9100_0108, 0xd65f_03c0], got_relocations())], &symbols);
    refused(&[on_add], "not a 64-bit `ldr`");
    let on_ldr_w = object(&[text(&[0x9000_0008, 0xb940_0108, 0xd65f_03c0], got_relocations())], &symbols);
    refused(&[on_ldr_w], "not a 64-bit `ldr`");
}

fn weak(name: &'static str, n_sect: u8, value: u64) -> Sym {
    Sym { desc: 0x80, ..defined(name, n_sect, value) }
}

/// The same weak function in two objects, as LLVM's `linkonce_odr` makes it: one copy is kept,
/// the first, and every call goes to it, from either object.
#[test]
fn a_weak_function_two_objects_define_is_kept_once() {
    let caller = |first: bool| {
        // `bl _shared; ret`, with `_shared` its own copy after `main` in the first object.
        let main = if first { "_main" } else { "_other" };
        let symbols = [defined(main, 1, 0), weak("_shared", 1, 8)];
        object(&[text(&[0x9400_0000, 0xd65f_03c0, 0xd65f_03c0], vec![rel(0, 1, BRANCH26)])], &symbols)
    };
    let image = link(&[caller(true), caller(false)]).unwrap();
    // The first object's code is at 0, its `_shared` at 8; the second's call, at 12, goes back to
    // that copy, 4 bytes before.
    assert_eq!(insn(&image.code, 0), 0x9400_0002);
    assert_eq!(insn(&image.code, 12), 0x97ff_ffff);
}

#[test]
fn a_strong_definition_beside_a_weak_one_or_a_weak_undefined_symbol_is_refused() {
    let strong =
        object(&[text(&[0xd65f_03c0], Vec::new())], &[defined("_main", 1, 0), defined("_shared", 1, 0)]);
    let soft = object(&[text(&[0xd65f_03c0], Vec::new())], &[weak("_shared", 1, 0)]);
    refused(&[strong, soft], "_shared is defined twice");
    let nowhere = Sym { desc: 0x80, ..undefined("_shared") };
    refused(
        &[object(&[text(&[0xd65f_03c0], Vec::new())], &[defined("_main", 1, 0), nowhere])],
        "weak but isn't defined",
    );
}

/// A pool of literals is constants, put with the data.
#[test]
fn a_literal_pool_goes_with_the_constants() {
    let pool = Sect {
        segment: "__TEXT",
        name: "__literal8",
        flags: 4,
        align: 3,
        bytes: 7.5_f64.to_le_bytes().to_vec(),
        relocations: Vec::new(),
    };
    // `adrp x8, pool@PAGE; ldr d0, [x8, pool@PAGEOFF]; ret`
    let insns = [0x9000_0008, 0xfd40_0100, 0xd65f_03c0];
    let relocations = vec![rel(0, 1, PAGE21), rel(4, 1, PAGEOFF12)];
    let objects = [object(
        &[text(&insns, relocations), pool],
        &[defined("_main", 1, 0), Sym { n_type: 0x0e, ..defined("lCPI0_0", 2, 16) }],
    )];
    let image = link(&objects).unwrap();
    assert_eq!(image.data, 7.5_f64.to_le_bytes());
    assert_eq!(insn(&image.code, 0), 0x9000_0088);
}
