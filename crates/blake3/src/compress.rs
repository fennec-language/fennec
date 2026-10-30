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

/// The quarter-round: mixes two message words into four state words.
macro_rules! g {
    ($state:ident, $a:literal, $b:literal, $c:literal, $d:literal, $x:expr, $y:expr) => {
        $state[$a] = $state[$a].wrapping_add($state[$b]).wrapping_add($x);
        $state[$d] = ($state[$d] ^ $state[$a]).rotate_right(16);
        $state[$c] = $state[$c].wrapping_add($state[$d]);
        $state[$b] = ($state[$b] ^ $state[$c]).rotate_right(12);
        $state[$a] = $state[$a].wrapping_add($state[$b]).wrapping_add($y);
        $state[$d] = ($state[$d] ^ $state[$a]).rotate_right(8);
        $state[$c] = $state[$c].wrapping_add($state[$d]);
        $state[$b] = ($state[$b] ^ $state[$c]).rotate_right(7);
    };
}

/// One round: the columns of the 4x4 state, then its diagonals.
fn round(state: &mut [u32; 16], m: &[u32; 16]) {
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
fn permute(m: &[u32; 16]) -> [u32; 16] {
    [m[2], m[6], m[3], m[10], m[7], m[0], m[4], m[13], m[1], m[11], m[12], m[5], m[9], m[14], m[15], m[8]]
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
    let mut m = *block;
    for _ in 0..6 {
        round(&mut state, &m);
        m = permute(&m);
    }
    round(&mut state, &m);
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
