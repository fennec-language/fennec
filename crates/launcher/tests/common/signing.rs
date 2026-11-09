//! Keys made for a test or a dry run, and signatures made with them as `ssh-keygen -Y sign` makes
//! them: what lets a launcher built to trust such a key install a release nobody has signed.
//! Shared by the launcher's tests and `examples/dry_run.rs`; never part of `fennec` itself.

use fennec_dist::PublicKey;
use ring::digest;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};

fn put(to: &mut Vec<u8>, part: &[u8]) {
    // No part of a key or a signature comes near 4 GiB.
    to.extend_from_slice(&u32::try_from(part.len()).unwrap_or(u32::MAX).to_be_bytes());
    to.extend_from_slice(part);
}

fn base64(bytes: &[u8]) -> String {
    const LETTERS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::new();
    for chunk in bytes.chunks(3) {
        let word =
            chunk.iter().enumerate().fold(0u32, |word, (at, byte)| word | u32::from(*byte) << (16 - 8 * at));
        for at in 0..4 {
            let six = usize::try_from(word >> (18 - 6 * at) & 63).unwrap_or(0);
            let letter = LETTERS.get(six).copied().unwrap_or(b'=');
            text.push(if at <= chunk.len() { char::from(letter) } else { '=' });
        }
    }
    text
}

/// A key made for a test or a dry run, and its public key as the launcher trusts one.
pub(crate) fn made_key() -> Result<(Ed25519KeyPair, PublicKey), String> {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map_err(|_| "couldn't make a key: the system gave no randomness".to_owned())?;
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
        .map_err(|err| format!("couldn't read the key made: {err}"))?;
    let mut blob = Vec::new();
    put(&mut blob, b"ssh-ed25519");
    put(&mut blob, pair.public_key().as_ref());
    let public = PublicKey::parse(&format!("ssh-ed25519 {} test", base64(&blob)))?;
    Ok((pair, public))
}

/// A signature as `ssh-keygen -Y sign` makes one.
pub(crate) fn sign(pair: &Ed25519KeyPair, namespace: &str, message: &[u8]) -> String {
    let mut public = Vec::new();
    put(&mut public, b"ssh-ed25519");
    put(&mut public, pair.public_key().as_ref());
    let mut signed = b"SSHSIG".to_vec();
    for part in [namespace.as_bytes(), b"", b"sha512", digest::digest(&digest::SHA512, message).as_ref()] {
        put(&mut signed, part);
    }
    let mut signature = Vec::new();
    put(&mut signature, b"ssh-ed25519");
    put(&mut signature, pair.sign(&signed).as_ref());
    let mut blob = b"SSHSIG".to_vec();
    blob.extend_from_slice(&1u32.to_be_bytes());
    for part in [&public[..], namespace.as_bytes(), b"", b"sha512", &signature] {
        put(&mut blob, part);
    }
    format!("-----BEGIN SSH SIGNATURE-----\n{}\n-----END SSH SIGNATURE-----\n", base64(&blob))
}
