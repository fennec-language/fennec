//! The compression function: mixes one 64-byte block into a chaining value.

/// The first words of SHA-256's fractional square roots, as BLAKE3 uses them.
pub(crate) const IV: [u32; 8] =
    [0x6A09_E667, 0xBB67_AE85, 0x3C6E_F372, 0xA54F_F53A, 0x510E_527F, 0x9B05_688C, 0x1F83_D9AB, 0x5BE0_CD19];

pub(crate) const BLOCK_LEN: usize = 64;
pub(crate) const CHUNK_LEN: usize = 1024;

pub(crate) const CHUNK_START: u32 = 1;
pub(crate) const CHUNK_END: u32 = 1 << 1;
pub(crate) const PARENT: u32 = 1 << 2;
pub(crate) const ROOT: u32 = 1 << 3;
pub(crate) const KEYED_HASH: u32 = 1 << 4;
pub(crate) const DERIVE_KEY_CONTEXT: u32 = 1 << 5;
pub(crate) const DERIVE_KEY_MATERIAL: u32 = 1 << 6;

/// How many chunks are compressed at once, one in each lane.
pub(crate) const LANES: usize = 4;

/// One word of each of `LANES` chunks, side by side: each operation on it works on every lane,
/// which the compiler turns into one vector instruction.
type Lanes = [u32; LANES];

/// What the compression function does to a word, for one chunk's words or for lanes of them.
trait Word: Copy {
    fn add(self, other: Self) -> Self;
    fn xor(self, other: Self) -> Self;
    fn rotate(self, by: u32) -> Self;
}

impl Word for u32 {
    #[inline]
    fn add(self, other: Self) -> Self {
        self.wrapping_add(other)
    }

    #[inline]
    fn xor(self, other: Self) -> Self {
        self ^ other
    }

    #[inline]
    fn rotate(self, by: u32) -> Self {
        self.rotate_right(by)
    }
}

impl Word for Lanes {
    #[inline]
    fn add(mut self, other: Self) -> Self {
        for (lane, other) in self.iter_mut().zip(other) {
            *lane = lane.wrapping_add(other);
        }
        self
    }

    #[inline]
    fn xor(mut self, other: Self) -> Self {
        for (lane, other) in self.iter_mut().zip(other) {
            *lane ^= other;
        }
        self
    }

    #[inline]
    fn rotate(mut self, by: u32) -> Self {
        for lane in &mut self {
            *lane = lane.rotate_right(by);
        }
        self
    }
}

/// The quarter-round: mixes two message words into four state words.
macro_rules! g {
    ($state:ident, $a:literal, $b:literal, $c:literal, $d:literal, $x:expr, $y:expr) => {
        $state[$a] = $state[$a].add($state[$b]).add($x);
        $state[$d] = $state[$d].xor($state[$a]).rotate(16);
        $state[$c] = $state[$c].add($state[$d]);
        $state[$b] = $state[$b].xor($state[$c]).rotate(12);
        $state[$a] = $state[$a].add($state[$b]).add($y);
        $state[$d] = $state[$d].xor($state[$a]).rotate(8);
        $state[$c] = $state[$c].add($state[$d]);
        $state[$b] = $state[$b].xor($state[$c]).rotate(7);
    };
}

/// One round: the columns of the 4x4 state, then its diagonals.
#[inline]
fn round<W: Word>(state: &mut [W; 16], m: &[W; 16]) {
    g!(state, 0, 4, 8, 12, m[0], m[1]);
    g!(state, 1, 5, 9, 13, m[2], m[3]);
    g!(state, 2, 6, 10, 14, m[4], m[5]);
    g!(state, 3, 7, 11, 15, m[6], m[7]);
    g!(state, 0, 5, 10, 15, m[8], m[9]);
    g!(state, 1, 6, 11, 12, m[10], m[11]);
    g!(state, 2, 7, 8, 13, m[12], m[13]);
    g!(state, 3, 4, 9, 14, m[14], m[15]);
}

/// The message words reordered for the next round.
#[inline]
fn permute<W: Word>(m: &[W; 16]) -> [W; 16] {
    [m[2], m[6], m[3], m[10], m[7], m[0], m[4], m[13], m[1], m[11], m[12], m[5], m[9], m[14], m[15], m[8]]
}

/// The seven rounds, each with the message reordered from the last.
#[inline]
fn rounds<W: Word>(state: &mut [W; 16], block: &[W; 16]) {
    let mut m = *block;
    for _ in 0..6 {
        round(state, &m);
        m = permute(&m);
    }
    round(state, &m);
}

/// One block's message words from each of four chunks, a lane for each.
#[inline]
fn message(blocks: [&[u8; BLOCK_LEN]; LANES]) -> [Lanes; 16] {
    let [b0, b1, b2, b3] =
        blocks.map(|block| block.as_chunks::<4>().0.iter().map(|four| u32::from_le_bytes(*four)));
    let mut m = [[0; LANES]; 16];
    for (word, (((w0, w1), w2), w3)) in m.iter_mut().zip(b0.zip(b1).zip(b2).zip(b3)) {
        *word = [w0, w1, w2, w3];
    }
    m
}

/// The chaining values of `LANES` whole chunks that follow one another, the first numbered
/// `counter`, none of them the root. It's what compressing each chunk's blocks in turn gives,
/// done for all of them at once.
pub(crate) fn chunks_at_once(
    key: &[u32; 8],
    chunks: &[[u8; CHUNK_LEN]; LANES],
    counter: u64,
    flags: u32,
) -> [[u32; 8]; LANES] {
    let [c0, c1, c2, c3] = chunks.each_ref().map(|chunk| chunk.as_chunks::<BLOCK_LEN>().0.iter());
    let counters: [u64; LANES] = [counter, counter + 1, counter + 2, counter + 3];
    #[expect(clippy::cast_possible_truncation, reason = "the counter goes in as its low and high words")]
    let (low, high) =
        (counters.map(|counter| counter as u32), counters.map(|counter| (counter >> 32) as u32));
    let block_len = [u32::try_from(BLOCK_LEN).unwrap_or(u32::MAX); LANES];
    let mut cv: [Lanes; 8] = key.map(|word| [word; LANES]);
    let blocks = c0.zip(c1).zip(c2).zip(c3);
    let last = CHUNK_LEN / BLOCK_LEN - 1;
    for (at, (((b0, b1), b2), b3)) in blocks.enumerate() {
        let start = if at == 0 { CHUNK_START } else { 0 };
        let end = if at == last { CHUNK_END } else { 0 };
        let [c0, c1, c2, c3, c4, c5, c6, c7] = cv;
        let [i0, i1, i2, i3, ..] = IV;
        let mut state = [
            c0,
            c1,
            c2,
            c3,
            c4,
            c5,
            c6,
            c7,
            [i0; LANES],
            [i1; LANES],
            [i2; LANES],
            [i3; LANES],
            low,
            high,
            block_len,
            [flags | start | end; LANES],
        ];
        rounds(&mut state, &message([b0, b1, b2, b3]));
        let (front, back) = state.split_at(8);
        for ((word, front), back) in cv.iter_mut().zip(front).zip(back) {
            *word = front.xor(*back);
        }
    }
    let [w0, w1, w2, w3, w4, w5, w6, w7] = cv;
    let lane = |at: usize| [w0, w1, w2, w3, w4, w5, w6, w7].map(|word| word.get(at).copied().unwrap_or(0));
    [lane(0), lane(1), lane(2), lane(3)]
}

/// Compresses `block` into `cv`. `counter` is the chunk's index, or the output block's
/// index for root output. The first 8 words of the result are the next chaining value; all
/// 16 are root output.
pub(crate) fn compress(
    cv: &[u32; 8],
    block: &[u32; 16],
    counter: u64,
    block_len: u32,
    flags: u32,
) -> [u32; 16] {
    #[expect(clippy::cast_possible_truncation, reason = "the counter goes in as its low and high words")]
    let (low, high) = (counter as u32, (counter >> 32) as u32);
    let mut state = [
        cv[0], cv[1], cv[2], cv[3], cv[4], cv[5], cv[6], cv[7], IV[0], IV[1], IV[2], IV[3], low, high,
        block_len, flags,
    ];
    rounds(&mut state, block);
    let (front, back) = state.split_at_mut(8);
    for ((front, back), cv) in front.iter_mut().zip(back.iter_mut()).zip(cv) {
        *front ^= *back;
        *back ^= cv;
    }
    state
}

/// The first 8 words of a compression: a chaining value.
pub(crate) fn first_eight(words: &[u32; 16]) -> [u32; 8] {
    [words[0], words[1], words[2], words[3], words[4], words[5], words[6], words[7]]
}

/// Little-endian words from bytes. Missing bytes count as zero.
pub(crate) fn words<const N: usize>(bytes: &[u8]) -> [u32; N] {
    let mut words = [0; N];
    for (word, four) in words.iter_mut().zip(bytes.as_chunks::<4>().0) {
        *word = u32::from_le_bytes(*four);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::{
        BLOCK_LEN, CHUNK_END, CHUNK_LEN, CHUNK_START, LANES, chunks_at_once, compress, first_eight, words,
    };

    /// A chunk's chaining value as compressing its blocks one at a time gives it.
    fn one_at_a_time(key: &[u32; 8], chunk: &[u8; CHUNK_LEN], counter: u64, flags: u32) -> [u32; 8] {
        let mut cv = *key;
        let blocks = chunk.as_chunks::<BLOCK_LEN>().0;
        for (at, block) in blocks.iter().enumerate() {
            let start = if at == 0 { CHUNK_START } else { 0 };
            let end = if at + 1 == blocks.len() { CHUNK_END } else { 0 };
            cv = first_eight(&compress(&cv, &words(block), counter, 64, flags | start | end));
        }
        cv
    }

    /// Wherever it's used, chunks at once give what they give one at a time, counters past
    /// 32 bits included.
    #[test]
    fn chunks_at_once_give_what_each_does_alone() {
        let mut chunks = [[0; CHUNK_LEN]; LANES];
        for (at, byte) in chunks.as_flattened_mut().iter_mut().enumerate() {
            *byte = u8::try_from(at % 251).unwrap();
        }
        let key = [0x0123_4567, 0x89AB_CDEF, 7, 11, 13, 17, 19, 23];
        for counter in [0, 1, 5, u64::from(u32::MAX) - 1, u64::from(u32::MAX) + 9] {
            for flags in [0, 1 << 4, 1 << 6] {
                let alone: Vec<[u32; 8]> = (0..)
                    .zip(&chunks)
                    .map(|(more, chunk)| one_at_a_time(&key, chunk, counter + more, flags))
                    .collect();
                assert_eq!(
                    chunks_at_once(&key, &chunks, counter, flags).to_vec(),
                    alone,
                    "counter {counter}"
                );
            }
        }
    }
}
