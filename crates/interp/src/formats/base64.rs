//! Standard base64, with `=` filling out the last four characters, which is how JSON and TOML
//! write `bytes`, here and in std's own readers and writers.

const DIGITS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// `data` as base64.
pub(super) fn encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let [a, b, c] = [0, 1, 2].map(|at| chunk.get(at).copied().unwrap_or(0));
        let n = (u32::from(a) << 16) | (u32::from(b) << 8) | u32::from(c);
        let digit = |shift: u32| {
            let at = usize::try_from((n >> shift) & 63).unwrap_or(0);
            DIGITS.get(at).copied().map_or('=', char::from)
        };
        out.push(digit(18));
        out.push(digit(12));
        out.push(if chunk.len() > 1 { digit(6) } else { '=' });
        out.push(if chunk.len() > 2 { digit(0) } else { '=' });
    }
    out
}

/// The bytes base64 text stands for, or nothing if it isn't base64.
pub(super) fn decode(text: &str) -> Option<Vec<u8>> {
    let text = text.as_bytes();
    if !text.len().is_multiple_of(4) {
        return None;
    }
    let value = |b: u8| DIGITS.iter().position(|&digit| digit == b).and_then(|at| u32::try_from(at).ok());
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let last = text.len() / 4;
    for (at, four) in text.chunks(4).enumerate() {
        let padded = match (at + 1 == last, four.get(2), four.get(3)) {
            (true, Some(b'='), Some(b'=')) => 2,
            (true, _, Some(b'=')) => 1,
            _ => 0,
        };
        let mut n = 0;
        for &b in four.iter().take(4 - padded) {
            n = (n << 6) | value(b)?;
        }
        n <<= 6 * padded;
        let [_, first, second, third] = n.to_be_bytes();
        out.extend([first, second, third].into_iter().take(3 - padded));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::{decode, encode};

    #[test]
    fn bytes_go_to_base64_and_back() {
        for (data, text) in [
            (&b""[..], ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"\xfa\xfb\xfc\xfd", "+vv8/Q=="),
        ] {
            assert_eq!(encode(data), text);
            assert_eq!(decode(text).as_deref(), Some(data));
        }
        for bad in ["a", "ab=c", "a===", "Zg=a", "Z!=="] {
            assert_eq!(decode(bad), None, "{bad}");
        }
    }
}
