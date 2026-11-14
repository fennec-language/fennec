//! SHA-256 and HMAC-SHA256, copied from the runtime's own (`crates/rt/src/stdlib/crypto/sha256.rs`),
//! so the twin hashes with the very code `std.crypto` does: Rust's standard library has none.


const ROUND: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

const START: [u32; 8] =
    [0x6a09_e667, 0xbb67_ae85, 0x3c6e_f372, 0xa54f_f53a, 0x510e_527f, 0x9b05_688c, 0x1f83_d9ab, 0x5be0_cd19];

/// How many bytes SHA-256 takes at a time.
const BLOCK: usize = 64;

/// The SHA-256 digest of `data`.
pub fn digest(data: &[u8]) -> [u8; 32] {
    let mut hasher = Hasher::new();
    hasher.update(data);
    hasher.finish()
}

/// SHA-256 over bytes given a piece at a time, with only a block's worth held between them.
pub(crate) struct Hasher {
    state: [u32; 8],
    /// What's been given since the last whole block.
    waiting: Vec<u8>,
    /// How many bytes have been given in all.
    length: u64,
}

impl Hasher {
    pub(crate) fn new() -> Self {
        Hasher { state: START, waiting: Vec::with_capacity(BLOCK), length: 0 }
    }

    pub(crate) fn update(&mut self, mut data: &[u8]) {
        self.length = self.length.wrapping_add(u64::try_from(data.len()).unwrap_or(u64::MAX));
        if !self.waiting.is_empty() {
            let wanted = (BLOCK - self.waiting.len()).min(data.len());
            let (taken, rest) = data.split_at(wanted);
            self.waiting.extend_from_slice(taken);
            data = rest;
            if self.waiting.len() == BLOCK {
                if let Ok(block) = <&[u8; BLOCK]>::try_from(self.waiting.as_slice()) {
                    compress(&mut self.state, block);
                }
                self.waiting.clear();
            }
        }
        let (blocks, rest) = data.as_chunks::<BLOCK>();
        for block in blocks {
            compress(&mut self.state, block);
        }
        self.waiting.extend_from_slice(rest);
    }

    pub(crate) fn finish(mut self) -> [u8; 32] {
        let bits = self.length.wrapping_mul(8);
        let mut tail = std::mem::take(&mut self.waiting);
        tail.push(0x80);
        while tail.len() % BLOCK != BLOCK - 8 {
            tail.push(0);
        }
        tail.extend_from_slice(&bits.to_be_bytes());
        for block in tail.as_chunks::<BLOCK>().0 {
            compress(&mut self.state, block);
        }
        let mut out = [0u8; 32];
        for (word, chunk) in self.state.iter().zip(out.as_chunks_mut::<4>().0) {
            *chunk = word.to_be_bytes();
        }
        out
    }
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
pub fn hmac(key: &[u8], data: &[u8]) -> [u8; 32] {
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

