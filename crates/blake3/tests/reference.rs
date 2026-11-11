//! The code this crate hashed with before it compressed four chunks at once: one chunk, and
//! one block of it, at a time, as BLAKE3's reference design does. It's kept, unchanged, so
//! the faster code can be checked against it on any input, not only the official vectors'.

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

/// What's needed to finish a node: either its chaining value or, for the root, its output.
#[derive(Clone, Copy)]
struct Output {
    cv: [u32; 8],
    block: [u32; 16],
    counter: u64,
    block_len: u32,
    flags: u32,
}

impl Output {
    fn chaining_value(&self) -> [u32; 8] {
        first_eight(&compress(&self.cv, &self.block, self.counter, self.block_len, self.flags))
    }

    /// Fills `out` with root output, as long as asked: 64 bytes per output block.
    fn root_bytes(&self, out: &mut [u8]) {
        for (index, out_block) in (0_u64..).zip(out.chunks_mut(BLOCK_LEN)) {
            let words = compress(&self.cv, &self.block, index, self.block_len, self.flags | ROOT);
            let bytes = words.iter().flat_map(|word| word.to_le_bytes());
            for (out, byte) in out_block.iter_mut().zip(bytes) {
                *out = byte;
            }
        }
    }
}

/// The output of a parent node over two chaining values.
fn parent(left: [u32; 8], right: [u32; 8], key: [u32; 8], flags: u32) -> Output {
    let mut block = [0; 16];
    for (word, value) in block.iter_mut().zip(left.into_iter().chain(right)) {
        *word = value;
    }
    Output { cv: key, block, counter: 0, block_len: block_len(BLOCK_LEN), flags: flags | PARENT }
}

fn block_len(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

/// One chunk being compressed: its chaining value so far and the block not yet compressed.
#[derive(Clone)]
struct Chunk {
    cv: [u32; 8],
    counter: u64,
    block: [u8; BLOCK_LEN],
    block_len: usize,
    blocks_compressed: usize,
    flags: u32,
}

impl Chunk {
    fn new(key: [u32; 8], counter: u64, flags: u32) -> Self {
        Chunk { cv: key, counter, block: [0; BLOCK_LEN], block_len: 0, blocks_compressed: 0, flags }
    }

    fn len(&self) -> usize {
        self.blocks_compressed * BLOCK_LEN + self.block_len
    }

    fn start_flag(&self) -> u32 {
        if self.blocks_compressed == 0 { CHUNK_START } else { 0 }
    }

    /// Adds input, which must fit in the chunk. A full block is compressed only once more
    /// input arrives, since the last block of a chunk is compressed differently.
    fn update(&mut self, input: &[u8]) {
        let room = BLOCK_LEN - self.block_len;
        let (first, rest) = input.split_at(room.min(input.len()));
        self.append(first);
        for piece in rest.chunks(BLOCK_LEN) {
            let flags = self.flags | self.start_flag();
            let block = compress(&self.cv, &words(&self.block), self.counter, block_len(BLOCK_LEN), flags);
            self.cv = first_eight(&block);
            self.blocks_compressed += 1;
            self.block = [0; BLOCK_LEN];
            self.block_len = 0;
            self.append(piece);
        }
    }

    fn append(&mut self, bytes: &[u8]) {
        for (slot, byte) in self.block.iter_mut().skip(self.block_len).zip(bytes) {
            *slot = *byte;
        }
        self.block_len += bytes.len();
    }

    fn output(&self) -> Output {
        Output {
            cv: self.cv,
            block: words(&self.block),
            counter: self.counter,
            block_len: block_len(self.block_len),
            flags: self.flags | self.start_flag() | CHUNK_END,
        }
    }
}

/// Hashes input given in any number of pieces.
#[derive(Clone)]
pub(crate) struct Tree {
    key: [u32; 8],
    flags: u32,
    chunk: Chunk,
    /// The chaining values of finished subtrees, one per set bit of the chunk count.
    stack: Vec<[u32; 8]>,
}

impl Tree {
    pub(crate) fn new(key: [u32; 8], flags: u32) -> Self {
        Tree { key, flags, chunk: Chunk::new(key, 0, flags), stack: Vec::new() }
    }

    pub(crate) fn update(&mut self, input: &[u8]) {
        let room = CHUNK_LEN - self.chunk.len();
        let (first, rest) = input.split_at(room.min(input.len()));
        self.chunk.update(first);
        for piece in rest.chunks(CHUNK_LEN) {
            let cv = self.chunk.output().chaining_value();
            let total = self.chunk.counter + 1;
            self.push(cv, total);
            self.chunk = Chunk::new(self.key, total, self.flags);
            self.chunk.update(piece);
        }
    }

    /// Adds a finished chunk's chaining value, merging every subtree it completes: one merge
    /// per trailing zero bit of the chunk count.
    fn push(&mut self, mut cv: [u32; 8], total_chunks: u64) {
        for _ in 0..total_chunks.trailing_zeros() {
            let Some(left) = self.stack.pop() else { break };
            cv = parent(left, cv, self.key, self.flags).chaining_value();
        }
        self.stack.push(cv);
    }

    /// Fills `out` with the hash, as long as asked.
    pub(crate) fn finalize(&self, out: &mut [u8]) {
        let mut output = self.chunk.output();
        for left in self.stack.iter().rev() {
            output = parent(*left, output.chaining_value(), self.key, self.flags);
        }
        output.root_bytes(out);
    }
}

/// The old code's first `len` bytes of output for `input`: a hash, a keyed hash with `key`, or
/// a key derived for `context`.
pub(crate) fn output(key: Option<&[u8; 32]>, context: Option<&str>, input: &[u8], len: usize) -> Vec<u8> {
    let mut tree = match (key, context) {
        (Some(key), _) => Tree::new(words(key), KEYED_HASH),
        (None, Some(context)) => {
            let mut context_key = [0; 32];
            let mut context_tree = Tree::new(IV, DERIVE_KEY_CONTEXT);
            context_tree.update(context.as_bytes());
            context_tree.finalize(&mut context_key);
            Tree::new(words(&context_key), DERIVE_KEY_MATERIAL)
        }
        (None, None) => Tree::new(IV, 0),
    };
    tree.update(input);
    let mut out = vec![0; len];
    tree.finalize(&mut out);
    out
}
