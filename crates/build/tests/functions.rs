//! Compiled functions in the cache: their keys, their stored form, and looking them up.

use fennec_blake3::hash;
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
    let tree = hash(b"def f() -> int: 1");
    let base = || FunctionKey::new("aarch64 has_lse").tree(&tree).uses("g(int) -> int");
    let key = base().finish();
    assert_eq!(key, base().finish(), "the same parts give the same key");
    let different = [
        FunctionKey::new("x86_64 has_avx2").tree(&tree).uses("g(int) -> int").finish(),
        FunctionKey::new("aarch64 has_lse").tree(&hash(b"def f() -> int: 2")).uses("g(int) -> int").finish(),
        FunctionKey::new("aarch64 has_lse").tree(&tree).uses("g(float) -> int").finish(),
        base().inlines(&hash(b"def g(x: int) -> int: x")).finish(),
        base().uses("").finish(),
    ];
    for other in different {
        assert_ne!(key, other);
    }
}

#[test]
fn parts_never_run_together() {
    let split = FunctionKey::new("t").uses("ab").uses("c").finish();
    let joined = FunctionKey::new("t").uses("a").uses("bc").finish();
    assert_ne!(split, joined);
    let as_use = FunctionKey::new("t").uses("x").finish();
    let as_target = FunctionKey::new("tx").finish();
    assert_ne!(as_use, as_target);
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
fn functions_are_stored_by_key_and_found_again() {
    let home = std::env::temp_dir().join(format!("fennec-functions-{}", std::process::id()));
    let cache = Cache::new(&home);
    let key = FunctionKey::new("t").tree(&hash(b"f")).finish();
    assert_eq!(cache.compiled(&key), None);
    cache.store_compiled(&key, &function()).unwrap();
    assert_eq!(cache.compiled(&key), Some(function()));
    let hex = key.to_hex();
    let path = home
        .join("cache")
        .join(VERSION)
        .join("functions")
        .join(hex.get(..2).unwrap())
        .join(format!("{hex}.bin"));
    assert!(path.is_file(), "spread over folders by the key's first byte");
    std::fs::write(&path, b"damaged").unwrap();
    assert_eq!(cache.compiled(&key), None, "a damaged entry is compiled again");
    std::fs::remove_dir_all(&home).unwrap();
}
