//! SHA-256, as FIPS 180-4 describes it, and HMAC-SHA256, as RFC 2104 does.

const ROUND: [u32; 64] = [
    0x428a_2f98, 0x7137_4491, 0xb5c0_fbcf, 0xe9b5_dba5, 0x3956_c25b, 0x59f1_11f1, 0x923f_82a4, 0xab1c_5ed5,
    0xd807_aa98, 0x1283_5b01, 0x2431_85be, 0x550c_7dc3, 0x72be_5d74, 0x80de_b1fe, 0x9bdc_06a7, 0xc19b_f174,
    0xe49b_69c1, 0xefbe_4786, 0x0fc1_9dc6, 0x240c_a1cc, 0x2de9_2c6f, 0x4a74_84aa, 0x5cb0_a9dc, 0x76f9_88da,
    0x983e_5152, 0xa831_c66d, 0xb003_27c8, 0xbf59_7fc7, 0xc6e0_0bf3, 0xd5a7_9147, 0x06ca_6351, 0x1429_2967,
    0x27b7_0a85, 0x2e1b_2138, 0x4d2c_6dfc, 0x5338_0d13, 0x650a_7354, 0x766a_0abb, 0x81c2_c92e, 0x9272_2c85,
    0xa2bf_e8a1, 0xa81a_664b, 0xc24b_8b70, 0xc76c_51a3, 0xd192_e819, 0xd699_0624, 0xf40e_3585, 0x106a_a070,
    0x19a4_c116, 0x1e37_6c08, 0x2748_774c, 0x34b0_bcb5, 0x391c_0cb3, 0x4ed8_aa4a, 0x5b9c_ca4f, 0x682e_6ff3,
    0x748f_82ee, 0x78a5_636f, 0x84c8_7814, 0x8cc7_0208, 0x90be_fffa, 0xa450_6ceb, 0xbef9_a3f7, 0xc671_78f2,
];

const START: [u32; 8] =
    [0x6a09_e667, 0xbb67_ae85, 0x3c6e_f372, 0xa54f_f53a, 0x510e_527f, 0x9b05_688c, 0x1f83_d9ab, 0x5be0_cd19];

/// How many bytes SHA-256 takes at a time.
const BLOCK: usize = 64;

/// The SHA-256 digest of `data`.
pub(super) fn digest(data: &[u8]) -> [u8; 32] {
    let mut state = START;
    let bits = u64::try_from(data.len()).unwrap_or(u64::MAX).wrapping_mul(8);
    let mut padded = data.to_vec();
    padded.push(0x80);
    while padded.len() % BLOCK != BLOCK - 8 {
        padded.push(0);
    }
    padded.extend_from_slice(&bits.to_be_bytes());
    for block in padded.as_chunks::<BLOCK>().0 {
        compress(&mut state, block);
    }
    let mut out = [0u8; 32];
    for (word, chunk) in state.iter().zip(out.as_chunks_mut::<4>().0) {
        *chunk = word.to_be_bytes();
    }
    out
}

/// The 64 words a block is spread into: its own 16, then each worked out from earlier ones.
fn schedule(block: &[u8; BLOCK]) -> [u32; 64] {
    let mut words = [0u32; 64];
    for (word, bytes) in words.iter_mut().zip(block.as_chunks::<4>().0) {
        *word = u32::from_be_bytes(*bytes);
    }
    for at in 16..64 {
        let earlier = |back: usize| words.get(at - back).copied().unwrap_or(0);
        let (w15, w2) = (earlier(15), earlier(2));
        let s0 = w15.rotate_right(7) ^ w15.rotate_right(18) ^ (w15 >> 3);
        let s1 = w2.rotate_right(17) ^ w2.rotate_right(19) ^ (w2 >> 10);
        let next = earlier(16).wrapping_add(s0).wrapping_add(earlier(7)).wrapping_add(s1);
        if let Some(word) = words.get_mut(at) {
            *word = next;
        }
    }
    words
}

#[expect(clippy::many_single_char_names, reason = "the names FIPS 180-4 gives the working variables")]
fn compress(state: &mut [u32; 8], block: &[u8; BLOCK]) {
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for (round, word) in ROUND.iter().zip(schedule(block)) {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let choose = (e & f) ^ (!e & g);
        let first = h.wrapping_add(s1).wrapping_add(choose).wrapping_add(*round).wrapping_add(word);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let majority = (a & b) ^ (a & c) ^ (b & c);
        let second = s0.wrapping_add(majority);
        (h, g, f, e, d, c, b, a) = (g, f, e, d.wrapping_add(first), c, b, a, first.wrapping_add(second));
    }
    for (held, new) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *held = held.wrapping_add(new);
    }
}

/// HMAC-SHA256 of `data` under `key`. A key longer than a block is hashed first.
pub(super) fn hmac(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut block = [0u8; BLOCK];
    let hashed;
    let key = if key.len() > BLOCK {
        hashed = digest(key);
        &hashed[..]
    } else {
        key
    };
    for (slot, byte) in block.iter_mut().zip(key) {
        *slot = *byte;
    }
    let pad = |with: u8| block.iter().map(|byte| byte ^ with).collect::<Vec<u8>>();
    let mut inner = pad(0x36);
    inner.extend_from_slice(data);
    let mut outer = pad(0x5c);
    outer.extend_from_slice(&digest(&inner));
    digest(&outer)
}

#[cfg(test)]
mod tests {
    use super::{digest, hmac};

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write;
        bytes.iter().fold(String::new(), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        })
    }

    /// The examples from FIPS 180-4 and NIST's test vectors.
    #[test]
    fn digests_match_the_standard() {
        let cases: [(&[u8], &str); 4] = [
            (b"", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
            (b"abc", "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
            (
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
                "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            ),
            (
                b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu",
                "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1",
            ),
        ];
        for (data, expected) in cases {
            assert_eq!(hex(&digest(data)), expected);
        }
        assert_eq!(
            hex(&digest(&vec![b'a'; 1_000_000])),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    /// Lengths around the 55- and 56-byte edge, where the length no longer fits the last block.
    #[test]
    fn padding_edges_match_the_standard() {
        assert_eq!(hex(&digest(&[b'a'; 55])), "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318");
        assert_eq!(hex(&digest(&[b'a'; 56])), "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a");
        assert_eq!(hex(&digest(&[b'a'; 64])), "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb");
    }

    /// The test cases from RFC 4231.
    #[test]
    fn hmacs_match_rfc_4231() {
        let cases: [(Vec<u8>, &[u8], &str); 4] = [
            (vec![0x0b; 20], b"Hi There", "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"),
            (b"Jefe".to_vec(), b"what do ya want for nothing?", "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"),
            (vec![0xaa; 20], &[0xdd; 50], "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe"),
            (
                vec![0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First",
                "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54",
            ),
        ];
        for (key, data, expected) in cases {
            assert_eq!(hex(&hmac(&key, data)), expected);
        }
    }
}
