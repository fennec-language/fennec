//! The `base64` target: `std.base64`'s `decode` and `url_decode`, built natively, on any text,
//! held to RFC 4648. Tokens, keys and uploads a stranger sends come in through them.
//!
//! What each should give is worked out here: `decode` reads the standard alphabet with `=`
//! filling out the last four characters, `url_decode` the URL-safe one with or without them,
//! and each only text that's the one way its bytes are written, with no bits left over. What
//! they give writes back as the same text, and any bytes written read back.

/// The program that decodes the text both ways, printing `Err`, or `Ok` and the bytes written
/// again, then whether the text's own bytes read back from both kinds of writing.
pub(crate) const READER: &str = "\
from std import base64

def main(fs: Fs, args: Args, stdout: Stdout) fails IoError:
    text = fs.read_text(args[0])?
    data = fs.read_bytes(args[0])?
    standard: bytes? = base64.decode(text) except InvalidValue(): None
    if standard is None:
        stdout.print(\"decode Err\")
    else:
        stdout.print(\"decode Ok\", base64.encode(standard))
    safe: bytes? = base64.url_decode(text) except InvalidValue(): None
    if safe is None:
        stdout.print(\"url_decode Err\")
    else:
        stdout.print(\"url_decode Ok\", base64.url_encode(safe))
    back: bytes? = base64.decode(base64.encode(data)) except InvalidValue(): None
    stdout.print(\"encodes\", back is not None and back == data)
    url_back: bytes? = base64.url_decode(base64.url_encode(data)) except InvalidValue(): None
    stdout.print(\"url_encodes\", url_back is not None and url_back == data)
";

/// The standard alphabet; the URL-safe one has `-` and `_` for its last two.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// The target: both decodes give what RFC 4648 says the text stands for, or fail, and bytes
/// written either way read back.
pub(crate) fn base64(input: &[u8]) -> Result<(), String> {
    let text = String::from_utf8_lossy(input).into_owned();
    let Some(printed) = crate::llvm::read_natively("base64", text.as_bytes())? else { return Ok(()) };
    let printed = String::from_utf8_lossy(&printed).into_owned();
    let lines: Vec<&str> = printed.lines().collect();
    let [decode, url_decode, encodes, url_encodes] = lines.as_slice() else {
        return Err(format!("{text:?}: a decode stopped the program, after printing:\n{printed}"));
    };
    for (line, url) in [(decode, false), (url_decode, true)] {
        let name = if url { "url_decode" } else { "decode" };
        let should = decoded(text.as_bytes(), url).map(|bytes| format!("{name} Ok {}", encoded(&bytes, url)));
        let should = should.unwrap_or_else(|| format!("{name} Err"));
        // An empty text decodes to no bytes, which write as nothing, so the line ends at `Ok`.
        if line.trim_end() != should.trim_end() {
            return Err(format!("{text:?}: {line:?}, where it should be {should:?}"));
        }
    }
    for line in [encodes, url_encodes] {
        if !line.ends_with(" True") {
            return Err(format!("{text:?}: its bytes didn't read back: {line}"));
        }
    }
    Ok(())
}

/// The bytes base64 `text` stands for, or `None` when it isn't base64 written the one way it
/// can be: in the URL-safe alphabet with `=` or without, or else the standard one with it.
fn decoded(text: &[u8], url: bool) -> Option<Vec<u8>> {
    let digits = text.strip_suffix(b"==").or_else(|| text.strip_suffix(b"=")).unwrap_or(text);
    let padded = digits.len() < text.len();
    if (padded || !url) && !text.len().is_multiple_of(4) {
        return None;
    }
    let mut bytes = Vec::new();
    for group in digits.chunks(4) {
        let mut n: u32 = 0;
        for digit in group {
            let digit = match (*digit, url) {
                (b'-', true) => b'+',
                (b'_', true) => b'/',
                (b'+' | b'/', true) => return None,
                (digit, _) => digit,
            };
            let bits = ALPHABET.iter().position(|known| *known == digit)?;
            n = n * 64 + u32::try_from(bits).ok()?;
        }
        n <<= 6 * (4 - group.len());
        let [_, first, second, third] = n.to_be_bytes();
        match group.len() {
            4 => bytes.extend([first, second, third]),
            3 if third == 0 => bytes.extend([first, second]),
            2 if second == 0 && third == 0 => bytes.push(first),
            _ => return None,
        }
    }
    Some(bytes)
}

/// `bytes` written in base64: the standard alphabet with `=`, or the URL-safe one without.
fn encoded(bytes: &[u8], url: bool) -> String {
    let digit = |bits: u32| {
        let digit = ALPHABET.get(usize::try_from(bits & 63).unwrap_or_default()).copied().unwrap_or(b'A');
        char::from(match (digit, url) {
            (b'+', true) => b'-',
            (b'/', true) => b'_',
            (digit, _) => digit,
        })
    };
    let mut text = String::new();
    for group in bytes.chunks(3) {
        let n = group.iter().fold(0_u32, |n, byte| (n << 8) | u32::from(*byte)) << (8 * (3 - group.len()));
        let written = group.len() + 1;
        text.extend((0..4).take(written).map(|at| digit(n >> (18 - 6 * at))));
        if !url {
            text.extend(std::iter::repeat_n('=', 4 - written));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{decoded, encoded};

    #[test]
    fn what_each_decode_should_give_is_worked_out_from_the_text() {
        assert_eq!(decoded(b"aGk=", false), Some(b"hi".to_vec()));
        assert_eq!(decoded(b"aGk", false), None);
        assert_eq!(decoded(b"aGk", true), Some(b"hi".to_vec()));
        assert_eq!(decoded(b"aGl=", false), None);
        assert_eq!(decoded(b"a===", false), None);
        assert_eq!(decoded(b"", false), Some(Vec::new()));
        assert_eq!(decoded(b"-_8=", true), Some(vec![0xfb, 0xff]));
        assert_eq!(decoded(b"+/8=", true), None);
        assert_eq!(encoded(b"hi", false), "aGk=");
        assert_eq!(encoded(&[0xfb, 0xff], true), "-_8");
    }
}
