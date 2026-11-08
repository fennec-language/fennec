//! OpenSSH's signatures of files, as `ssh-keygen -Y sign` makes them, for Ed25519 keys: a key kept
//! on paper (`ssh-ed25519`), and one that never leaves a hardware key (`sk-ssh-ed25519`), which
//! signs only when someone touches it. The format is OpenSSH's `PROTOCOL.sshsig`, and a hardware
//! key's signature is laid out as its `PROTOCOL.u2f` says.
//!
//! Only checking is here. Nothing in Fennec signs, so no private key is ever read or held.

use ring::digest;
use ring::signature::{ED25519, UnparsedPublicKey};

const BEGIN: &str = "-----BEGIN SSH SIGNATURE-----";
const END: &str = "-----END SSH SIGNATURE-----";
const MAGIC: &[u8] = b"SSHSIG";
const PLAIN: &str = "ssh-ed25519";
const HARDWARE: &str = "sk-ssh-ed25519@openssh.com";
/// What a hardware key sets in a signature's flags when someone touched it to sign.
const TOUCHED: u8 = 0x01;

/// An Ed25519 public key, as OpenSSH writes it in its wire format, which is what a signature names
/// its signer by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicKey {
    blob: Vec<u8>,
}

impl PublicKey {
    /// A key from a line of an OpenSSH public key file, like `ssh-ed25519 AAAAC3Nz... chris@paper`.
    /// Only Ed25519 keys, on paper or on a hardware key, are taken.
    ///
    /// # Errors
    ///
    /// When the line isn't an Ed25519 public key.
    pub fn parse(line: &str) -> Result<PublicKey, String> {
        let mut words = line.split_whitespace();
        let (Some(kind), Some(encoded)) = (words.next(), words.next()) else {
            return Err(format!("`{line}` isn't an OpenSSH public key"));
        };
        let blob = base64(encoded).ok_or_else(|| format!("the key `{line}` isn't base64"))?;
        let key = PublicKey { blob };
        let parsed = key.parts().ok_or_else(|| format!("`{line}` isn't an Ed25519 key"))?;
        if parsed.kind != kind {
            return Err(format!("`{line}` says it's {kind}, but it holds {}", parsed.kind));
        }
        Ok(key)
    }

    /// What kind of key it is, its 32 bytes, and for a hardware key what it was made for.
    fn parts(&self) -> Option<KeyParts<'_>> {
        let mut reader = Reader(&self.blob);
        let kind = reader.text()?;
        let key = reader.bytes()?;
        let application = match kind {
            PLAIN => None,
            HARDWARE => Some(reader.bytes()?),
            _ => return None,
        };
        (key.len() == 32 && reader.0.is_empty()).then_some(KeyParts { kind, key, application })
    }
}

struct KeyParts<'k> {
    kind: &'k str,
    key: &'k [u8],
    application: Option<&'k [u8]>,
}

/// Every signature in `armored`, which holds one or more of `ssh-keygen -Y sign`'s, one after
/// another, gives back the public key of each one that signs `message` in `namespace` and checks.
/// A signature that doesn't check, or is for another namespace, is left out, so whoever counts the
/// keys counts only good signatures.
///
/// # Errors
///
/// When `armored` isn't made of OpenSSH signatures.
pub(crate) fn signers(armored: &str, namespace: &str, message: &[u8]) -> Result<Vec<PublicKey>, String> {
    let mut signers = Vec::new();
    let mut rest = armored;
    while let Some((_, after)) = rest.split_once(BEGIN) {
        let (inside, next) = after.split_once(END).ok_or("an SSH signature has no end line")?;
        let encoded: String = inside.split_whitespace().collect();
        let blob = base64(&encoded).ok_or("an SSH signature isn't base64")?;
        if let Some(signer) = checked(&blob, namespace, message) {
            signers.push(signer);
        }
        rest = next;
    }
    if signers.is_empty() && !armored.contains(BEGIN) {
        return Err("there are no SSH signatures here".to_owned());
    }
    Ok(signers)
}

/// The key that made the signature `blob`, if it signs `message` in `namespace`.
fn checked(blob: &[u8], namespace: &str, message: &[u8]) -> Option<PublicKey> {
    let mut reader = Reader(blob);
    if reader.take(MAGIC.len())? != MAGIC || reader.number()? != 1 {
        return None;
    }
    let key = PublicKey { blob: reader.bytes()?.to_vec() };
    let signed_in = reader.bytes()?;
    let reserved = reader.bytes()?;
    let hash = reader.text()?;
    let signature = reader.bytes()?;
    if signed_in != namespace.as_bytes() || !reader.0.is_empty() {
        return None;
    }
    let digest = match hash {
        "sha512" => digest::digest(&digest::SHA512, message),
        "sha256" => digest::digest(&digest::SHA256, message),
        _ => return None,
    };
    // What the signature is over: the namespace and the message's hash, so one made for another
    // purpose, or another file, never checks here.
    let mut signed = Vec::new();
    signed.extend_from_slice(MAGIC);
    for part in [namespace.as_bytes(), reserved, hash.as_bytes(), digest.as_ref()] {
        put(&mut signed, part);
    }
    let parts = key.parts()?;
    let mut signature = Reader(signature);
    if signature.text()? != parts.kind {
        return None;
    }
    let raw = signature.bytes()?;
    let over = match parts.application {
        None => signed,
        Some(application) => {
            // A hardware key signs a hash of what it was made for, whether it was touched, a count
            // of its signatures, and the hash of what's signed.
            let flags = *signature.take(1)?.first()?;
            let counter = signature.take(4)?;
            if flags & TOUCHED == 0 {
                return None;
            }
            let mut over = digest::digest(&digest::SHA256, application).as_ref().to_vec();
            over.push(flags);
            over.extend_from_slice(counter);
            over.extend_from_slice(digest::digest(&digest::SHA256, &signed).as_ref());
            over
        }
    };
    if !signature.0.is_empty() {
        return None;
    }
    UnparsedPublicKey::new(&ED25519, parts.key).verify(&over, raw).ok()?;
    Some(key)
}

/// Writes a part as OpenSSH's wire format writes a string: its length, then its bytes.
fn put(to: &mut Vec<u8>, part: &[u8]) {
    let length = u32::try_from(part.len()).unwrap_or(u32::MAX);
    to.extend_from_slice(&length.to_be_bytes());
    to.extend_from_slice(part);
}

/// Reads OpenSSH's wire format.
struct Reader<'b>(&'b [u8]);

impl<'b> Reader<'b> {
    fn take(&mut self, count: usize) -> Option<&'b [u8]> {
        let (taken, rest) = (self.0.get(..count)?, self.0.get(count..)?);
        self.0 = rest;
        Some(taken)
    }

    fn number(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }

    fn bytes(&mut self) -> Option<&'b [u8]> {
        let length = usize::try_from(self.number()?).ok()?;
        self.take(length)
    }

    fn text(&mut self) -> Option<&'b str> {
        std::str::from_utf8(self.bytes()?).ok()
    }
}

/// Standard base64, with its padding.
fn base64(text: &str) -> Option<Vec<u8>> {
    let value = |byte: u8| -> Option<u32> {
        let found = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        Some(u32::from(found))
    };
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut decoded = Vec::with_capacity(bytes.len() / 4 * 3);
    for (at, chunk) in bytes.chunks(4).enumerate() {
        let last = at + 1 == bytes.len() / 4;
        let padding = chunk.iter().rev().take_while(|byte| **byte == b'=').count();
        if padding > 2 || (padding > 0 && !last) {
            return None;
        }
        let mut word = 0;
        for byte in chunk.get(..4 - padding)? {
            word = word << 6 | value(*byte)?;
        }
        word <<= 6 * u32::try_from(padding).ok()?;
        let three = word.to_be_bytes();
        decoded.extend_from_slice(three.get(1..4 - padding)?);
    }
    Some(decoded)
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_reads_what_it_should_and_nothing_else() {
        assert_eq!(super::base64("Zm9vYmFy").as_deref(), Some(&b"foobar"[..]));
        assert_eq!(super::base64("Zm9vYg==").as_deref(), Some(&b"foob"[..]));
        assert_eq!(super::base64("Zm9vYmE=").as_deref(), Some(&b"fooba"[..]));
        assert_eq!(super::base64("").as_deref(), Some(&b""[..]));
        for bad in ["Zm9", "Zm9v!mFy", "Zg==Zm9v", "Z==="] {
            assert_eq!(super::base64(bad), None, "{bad}");
        }
    }
}
