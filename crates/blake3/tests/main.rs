//! BLAKE3 against the official test vectors, in every mode, at every length the vectors
//! cover, with extended output, and fed in pieces of many sizes.

mod reference;
mod vectors;

use fennec_blake3::{Hash, Hasher, derive_key, hash, keyed_hash};
use vectors::{CASES, CONTEXT, KEY};

/// The vectors' input: `len` bytes counting 0 to 250 over and over.
fn input(len: usize) -> Vec<u8> {
    (0..len).map(|at| u8::try_from(at % 251).unwrap_or(0)).collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .flat_map(|byte| [byte >> 4, byte & 0xF])
        .filter_map(|digit| char::from_digit(u32::from(digit), 16))
        .collect()
}

/// The first `len` bytes of extended output.
fn xof(hasher: &Hasher, len: usize) -> String {
    let mut out = vec![0; len];
    hasher.finalize_xof(&mut out);
    hex(&out)
}

#[test]
fn hashes_match_the_official_vectors() {
    for &(len, expected, _, _) in CASES {
        let input = input(len);
        assert_eq!(xof(Hasher::new().update(&input), expected.len() / 2), expected, "hash of {len} bytes");
        assert_eq!(hash(&input).to_hex(), expected.get(..64).unwrap(), "hash of {len} bytes");
    }
}

#[test]
fn keyed_hashes_match_the_official_vectors() {
    for &(len, _, expected, _) in CASES {
        let input = input(len);
        assert_eq!(xof(Hasher::new_keyed(KEY).update(&input), expected.len() / 2), expected, "{len} bytes");
        assert_eq!(keyed_hash(KEY, &input).to_hex(), expected.get(..64).unwrap(), "{len} bytes");
    }
}

#[test]
fn derived_keys_match_the_official_vectors() {
    for &(len, _, _, expected) in CASES {
        let input = input(len);
        let hasher = Hasher::new_derive_key(CONTEXT);
        assert_eq!(xof(hasher.clone().update(&input), expected.len() / 2), expected, "{len} bytes");
        assert_eq!(hex(&derive_key(CONTEXT, &input)), expected.get(..64).unwrap(), "{len} bytes");
    }
}

#[test]
fn pieces_of_any_size_give_the_same_hash() {
    let input = input(5_000);
    let whole = hash(&input);
    for size in [1, 3, 63, 64, 65, 1_000, 1_023, 1_024, 1_025, 4_096] {
        let mut hasher = Hasher::default();
        for piece in input.chunks(size) {
            hasher.update(piece);
        }
        assert_eq!(hasher.finalize(), whole, "in pieces of {size}");
    }
}

/// A way of hashing: its name, a hasher for it, and the key or context the old code takes.
type Mode = (&'static str, fn() -> Hasher, Option<&'static [u8; 32]>, Option<&'static str>);

#[test]
fn the_code_hashes_as_the_one_chunk_at_a_time_code_it_replaced() {
    let lengths = [
        0, 1, 63, 64, 65, 1_023, 1_024, 1_025, 2_048, 3_071, 3_072, 4_095, 4_096, 4_097, 4_159, 5_120, 8_191,
        8_192, 8_193, 9_216, 16_384, 16_385, 20_481, 31_744, 65_537, 102_407,
    ];
    let modes: [Mode; 3] = [
        ("hash", Hasher::new, None, None),
        ("keyed hash", || Hasher::new_keyed(KEY), Some(KEY), None),
        ("derived key", || Hasher::new_derive_key(CONTEXT), None, Some(CONTEXT)),
    ];
    for len in lengths {
        let input = input(len);
        for (mode, new, key, context) in modes {
            let old = hex(&reference::output(key, context, &input, 131));
            for size in [len.max(1), 1, 64, 1_000, 1_024, 4_096, 4_097, 5_000, 12_288] {
                let mut hasher = new();
                for piece in input.chunks(size) {
                    hasher.update(piece);
                }
                assert_eq!(xof(&hasher, 131), old, "{mode} of {len} bytes in pieces of {size}");
            }
        }
    }
}

#[test]
fn hashes_print_and_read_back_as_hex() {
    let hash = hash(b"fennec");
    let text = hash.to_hex();
    assert_eq!(text.len(), 64);
    assert_eq!(format!("{hash:?}"), format!("Hash({text})"));
    assert_eq!(hash.as_bytes(), &hash.0);
    assert_eq!(Hash::from_hex(&text), Some(hash));
    assert_eq!(Hash::from_hex(&text.to_uppercase()), Some(hash));
    assert_eq!(Hash::from_hex(text.get(..63).unwrap()), None);
    assert_eq!(Hash::from_hex(&format!("{}g", text.get(..63).unwrap())), None);
    assert_eq!(Hash::from_hex(&format!("{text}00")), None);
}
