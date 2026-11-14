//! `std.crypto`: hashes, HMAC and comparing secrets, which the runtime does.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "crypto",
    names: &["Secret", "blake3", "keyed_blake3", "derive_key", "sha256", "hmac_sha256", "equal", "blake3_digest", "sha256_digest", "hmac_sha256_digest", "keyed_blake3_digest", "_start", "_add", "_end", "_end_digest", "blake3_of_str_chunks", "blake3_of_str_list", "blake3_of_bytes_chunks", "blake3_of_bytes_list", "sha256_of_str_chunks", "sha256_of_str_list", "sha256_of_bytes_chunks", "sha256_of_bytes_list", "blake3_digest_of_str_chunks", "blake3_digest_of_str_list", "blake3_digest_of_bytes_chunks", "blake3_digest_of_bytes_list", "sha256_digest_of_str_chunks", "sha256_digest_of_str_list", "sha256_digest_of_bytes_chunks", "sha256_digest_of_bytes_list"],
    source: include_str!("crypto.fen"),
    gives: &[],
};
