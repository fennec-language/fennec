//! `BLAKE2b`, as RFC 7693 describes it, with a digest of 1 to 64 bytes and no key: what Argon2
//! hashes with, and nothing else uses.

const IV: [u64; 8] = [
    0x6a09_e667_f3bc_c908,
    0xbb67_ae85_84ca_a73b,
    0x3c6e_f372_fe94_f82b,
    0xa54f_f53a_5f1d_36f1,
    0x510e_527f_ade6_82d1,
    0x9b05_688c_2b3e_6c1f,
    0x1f83_d9ab_fb41_bd6b,
    0x5be0_cd19_137e_2179,
];

/// Which message word each step of each round takes.
const SIGMA: [[usize; 16]; 12] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
    [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
    [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
    [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
    [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
    [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
    [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
    [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
    [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
];

const BLOCK: usize = 128;

/// The mixing function, on the four working words at `places` and two message words.
fn mix(work: &mut [u64; 16], places: [usize; 4], (first, second): (u64, u64)) {
    let [Some(&one), Some(&two), Some(&three), Some(&four)] = places.map(|place| work.get(place)) else {
        return;
    };
    let one = one.wrapping_add(two).wrapping_add(first);
    let four = (four ^ one).rotate_right(32);
    let three = three.wrapping_add(four);
    let two = (two ^ three).rotate_right(24);
    let one = one.wrapping_add(two).wrapping_add(second);
    let four = (four ^ one).rotate_right(16);
    let three = three.wrapping_add(four);
    let two = (two ^ three).rotate_right(63);
    for (place, value) in places.into_iter().zip([one, two, three, four]) {
        if let Some(slot) = work.get_mut(place) {
            *slot = value;
        }
    }
}

/// Folds one block into the state. `taken` is how many bytes have been taken in all, this block's
/// included, and `last` marks the final block.
fn compress(state: &mut [u64; 8], block: &[u8; BLOCK], taken: u128, last: bool) {
    let (eights, _) = block.as_chunks::<8>();
    let mut words = [0u64; 16];
    for (word, eight) in words.iter_mut().zip(eights) {
        *word = u64::from_le_bytes(*eight);
    }
    let [s0, s1, s2, s3, s4, s5, s6, s7] = *state;
    let [i0, i1, i2, i3, i4, i5, i6, i7] = IV;
    let low = u64::try_from(taken & u128::from(u64::MAX)).unwrap_or(0);
    let high = u64::try_from(taken >> 64).unwrap_or(0);
    let flag = if last { u64::MAX } else { 0 };
    let mut work = [s0, s1, s2, s3, s4, s5, s6, s7, i0, i1, i2, i3, i4 ^ low, i5 ^ high, i6 ^ flag, i7];
    for order in &SIGMA {
        let [m0, m1, m2, m3, m4, m5, m6, m7, m8, m9, m10, m11, m12, m13, m14, m15] =
            order.map(|at| words.get(at).copied().unwrap_or(0));
        mix(&mut work, [0, 4, 8, 12], (m0, m1));
        mix(&mut work, [1, 5, 9, 13], (m2, m3));
        mix(&mut work, [2, 6, 10, 14], (m4, m5));
        mix(&mut work, [3, 7, 11, 15], (m6, m7));
        mix(&mut work, [0, 5, 10, 15], (m8, m9));
        mix(&mut work, [1, 6, 11, 12], (m10, m11));
        mix(&mut work, [2, 7, 8, 13], (m12, m13));
        mix(&mut work, [3, 4, 9, 14], (m14, m15));
    }
    let (low_half, high_half) = work.split_at(8);
    for ((held, first), second) in state.iter_mut().zip(low_half).zip(high_half) {
        *held ^= first ^ second;
    }
}

/// A `BLAKE2b` hash being taken in a piece at a time, as Argon2 gives it its parts.
pub(super) struct Hasher {
    state: [u64; 8],
    held: [u8; BLOCK],
    filled: usize,
    taken: u128,
    size: usize,
}

impl Hasher {
    /// A hash with a digest of `size` bytes, 1 to 64.
    pub(super) fn new(size: usize) -> Hasher {
        let size = size.clamp(1, 64);
        let mut state = IV;
        // No key: the parameter block is the digest's size, a key of 0 bytes, fanout 1, depth 1.
        if let Some(first) = state.first_mut() {
            *first ^= 0x0101_0000 ^ u64::try_from(size).unwrap_or(64);
        }
        Hasher { state, held: [0; BLOCK], filled: 0, taken: 0, size }
    }

    pub(super) fn update(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            // A full block is folded in only once more comes, since the last one is marked.
            if self.filled == BLOCK {
                self.taken += BLOCK as u128;
                compress(&mut self.state, &self.held, self.taken, false);
                self.filled = 0;
            }
            let room = BLOCK - self.filled;
            let (now, later) = data.split_at(room.min(data.len()));
            if let Some(into) = self.held.get_mut(self.filled..self.filled + now.len()) {
                into.copy_from_slice(now);
            }
            self.filled += now.len();
            data = later;
        }
    }

    pub(super) fn finish(mut self) -> Vec<u8> {
        self.taken += self.filled as u128;
        if let Some(rest) = self.held.get_mut(self.filled..) {
            rest.fill(0);
        }
        compress(&mut self.state, &self.held, self.taken, true);
        let mut out: Vec<u8> = self.state.iter().flat_map(|word| word.to_le_bytes()).collect();
        out.truncate(self.size);
        out
    }
}

/// The `BLAKE2b` digest of `data`, `size` bytes long, 1 to 64.
pub(super) fn digest(size: usize, data: &[u8]) -> Vec<u8> {
    let mut hasher = Hasher::new(size);
    hasher.update(data);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::{Hasher, digest};

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        })
    }

    /// RFC 7693, Appendix A, and the empty message's digest from the reference implementation.
    #[test]
    fn digests_match_rfc_7693() {
        assert_eq!(
            hex(&digest(64, b"abc")),
            "ba80a53f981c4d0d6a2797b69f12f6e94c212f14685ac4b74b12bb6fdbffa2d1\
             7d87c5392aab792dc252d5de4533cc9518d38aa8dbf1925ab92386edd4009923"
        );
        assert_eq!(
            hex(&digest(64, b"")),
            "786a02f742015903c6c6fd852552d272912f4740e15847618a86e217f71f5419\
             d25e1031afee585313896444934eb04b903a685b1448b755d56f701afe9be2ce"
        );
    }

    /// The digest of many blocks is the same however its pieces come, across a block's edge too.
    #[test]
    fn pieces_hash_as_the_whole() {
        let data: Vec<u8> = (0..1000u16).map(|at| u8::try_from(at * 7 % 251).unwrap_or(0)).collect();
        let whole = digest(48, &data);
        for size in [1, 63, 127, 128, 129, 999] {
            let mut hasher = Hasher::new(48);
            for piece in data.chunks(size) {
                hasher.update(piece);
            }
            assert_eq!(hasher.finish(), whole, "in pieces of {size}");
        }
        // Exactly one block: the last is marked and isn't followed by an empty one.
        assert_ne!(digest(64, &[0; 128]), digest(64, &[0; 256]));
    }
}
