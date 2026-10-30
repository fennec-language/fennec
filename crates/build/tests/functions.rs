//! Compiled functions in the cache: their keys, their stored form, and looking them up.

use fennec_build::{Cache, CompiledFunction, FunctionKey, Relocation, VERSION};

fn function() -> CompiledFunction {
    CompiledFunction {
        code: vec![0x90; 32],
        alignment: 16,
        relocations: vec![
            Relocation { offset: 4, kind: 3, symbol: "fennec.helper.print".to_owned(), addend: -4 },
            Relocation { offset: 20, kind: 7, symbol: "fennec.literal.ab12".to_owned(), addend: 0 },
        ],
    }
}

#[test]
fn a_key_changes_with_every_part() {
    let key = FunctionKey::new("aarch64 has_lse").ir("v0 = iconst.i64 1").finish();
    assert_eq!(key, FunctionKey::new("aarch64 has_lse").ir("v0 = iconst.i64 1").finish());
    assert_ne!(key, FunctionKey::new("x86_64 has_avx2").ir("v0 = iconst.i64 1").finish(), "another target");
    assert_ne!(key, FunctionKey::new("aarch64 has_lse").ir("v0 = iconst.i64 2").finish(), "other code");
}

#[test]
fn parts_never_run_together() {
    assert_ne!(FunctionKey::new("t").ir("x").finish(), FunctionKey::new("tx").ir("").finish());
    assert_ne!(
        FunctionKey::new("t").ir("ab").ir("c").finish(),
        FunctionKey::new("t").ir("a").ir("bc").finish()
    );
}

#[test]
fn stored_functions_read_back() {
    let stored = function();
    assert_eq!(CompiledFunction::decode(&stored.encode()), Some(stored));
    let empty = CompiledFunction { code: Vec::new(), alignment: 1, relocations: Vec::new() };
    assert_eq!(CompiledFunction::decode(&empty.encode()), Some(empty));
}

#[test]
fn damaged_functions_are_compiled_again() {
    let bytes = function().encode();
    for cut in 0..bytes.len() {
        assert_eq!(CompiledFunction::decode(bytes.get(..cut).unwrap()), None, "cut at {cut}");
    }
    let mut extra = bytes.clone();
    extra.push(0);
    assert_eq!(CompiledFunction::decode(&extra), None, "trailing bytes");
    let mut other_format = bytes.clone();
    other_format[4] = 2;
    assert_eq!(CompiledFunction::decode(&other_format), None, "another format");
    let mut wrong_magic = bytes;
    wrong_magic[0] = b'X';
    assert_eq!(CompiledFunction::decode(&wrong_magic), None, "not a stored function");
    let outside = CompiledFunction {
        code: vec![0; 4],
        alignment: 1,
        relocations: vec![Relocation { offset: 4, kind: 1, symbol: "x".to_owned(), addend: 0 }],
    };
    assert_eq!(CompiledFunction::decode(&outside.encode()), None, "a relocation past the code");
}

#[test]
fn packs_hold_a_programs_functions_by_key() {
    let home = std::env::temp_dir().join(format!("fennec-functions-{}", std::process::id()));
    let cache = Cache::new(&home);
    let first = FunctionKey::new("t").ir("f").finish();
    let second = FunctionKey::new("t").ir("g").finish();
    assert!(cache.pack("/kit").is_empty());
    let empty = CompiledFunction { code: Vec::new(), alignment: 1, relocations: Vec::new() };
    cache.store_pack("/kit", &[(first, function()), (second, empty.clone())]).unwrap();
    let pack = cache.pack("/kit");
    assert_eq!((pack.get(&first), pack.get(&second)), (Some(&function()), Some(&empty)));
    assert!(cache.pack("/another kit").is_empty(), "each program has its own pack");

    let packs = home.join("cache").join(VERSION).join("packs");
    let file = std::fs::read_dir(&packs).unwrap().next().unwrap().unwrap().path();
    let bytes = std::fs::read(&file).unwrap();
    for cut in [3, 8, 12, 20, bytes.len() - 1] {
        assert!(fennec_build::decode_pack(&bytes[..cut]).is_empty(), "cut at {cut}");
    }
    std::fs::write(&file, b"damaged").unwrap();
    assert!(cache.pack("/kit").is_empty(), "a damaged pack is compiled again");
    std::fs::remove_dir_all(&home).unwrap();
}
