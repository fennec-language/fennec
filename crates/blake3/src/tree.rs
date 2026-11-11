//! The tree: input is split into 1 KiB chunks, each chunk is compressed block by block into
//! a chaining value, and pairs of chaining values are merged into parents up to one root.

use crate::compress::{
    BLOCK_LEN, CHUNK_END, CHUNK_LEN, CHUNK_START, LANES, PARENT, ROOT, chunks_at_once, compress, first_eight,
    words,
};

/// Whether whole chunks are compressed `LANES` at a time. On arm64 each operation on a lane of
/// words is one vector instruction, rotations included, so it takes well under half the
/// instructions. Baseline x86-64 has no vector rotation and too few vector registers, so there
/// the compiler leaves the lanes as scalar code that costs as much as a chunk at a time.
const AT_ONCE: bool = cfg!(target_arch = "aarch64");

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
        let mut rest = input;
        while !rest.is_empty() {
            if self.block_len == BLOCK_LEN {
                self.compress(&words(&self.block));
                self.block_len = 0;
            }
            // Whole blocks are compressed straight from the input, while more input follows them.
            while self.block_len == 0
                && let Some((block, after)) = rest.split_first_chunk::<BLOCK_LEN>()
                && !after.is_empty()
            {
                self.compress(&words(block));
                rest = after;
            }
            let room = BLOCK_LEN - self.block_len;
            let (piece, after) = rest.split_at(room.min(rest.len()));
            self.append(piece);
            rest = after;
        }
    }

    /// Compresses a block that isn't the chunk's last into its chaining value.
    fn compress(&mut self, block: &[u32; 16]) {
        let flags = self.flags | self.start_flag();
        self.cv = first_eight(&compress(&self.cv, block, self.counter, block_len(BLOCK_LEN), flags));
        self.blocks_compressed += 1;
    }

    fn append(&mut self, bytes: &[u8]) {
        let end = self.block_len + bytes.len();
        if let Some(slots) = self.block.get_mut(self.block_len..end) {
            slots.copy_from_slice(bytes);
            self.block_len = end;
        }
    }

    /// The last block, its unused bytes zero.
    fn last_block(&self) -> [u32; 16] {
        let mut block = [0; BLOCK_LEN];
        if let (Some(slots), Some(bytes)) =
            (block.get_mut(..self.block_len), self.block.get(..self.block_len))
        {
            slots.copy_from_slice(bytes);
        }
        words(&block)
    }

    fn output(&self) -> Output {
        Output {
            cv: self.cv,
            block: self.last_block(),
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
        let mut rest = input;
        while !rest.is_empty() {
            // A full chunk is finished once more input follows it: only the last can be the root.
            if self.chunk.len() == CHUNK_LEN {
                let cv = self.chunk.output().chaining_value();
                let total = self.chunk.counter + 1;
                self.push(cv, total);
                self.chunk = Chunk::new(self.key, total, self.flags);
            }
            // Whole chunks are compressed `LANES` at a time, while more input follows them.
            if AT_ONCE
                && self.chunk.len() == 0
                && let Some((chunks, after)) = rest.split_first_chunk::<{ LANES * CHUNK_LEN }>()
                && !after.is_empty()
                && let Ok(chunks) = <&[[u8; CHUNK_LEN]; LANES]>::try_from(chunks.as_chunks::<CHUNK_LEN>().0)
            {
                let counter = self.chunk.counter;
                for (total, cv) in (counter + 1..).zip(chunks_at_once(&self.key, chunks, counter, self.flags))
                {
                    self.push(cv, total);
                }
                self.chunk = Chunk::new(self.key, counter + LANES as u64, self.flags);
                rest = after;
                continue;
            }
            let room = CHUNK_LEN - self.chunk.len();
            let (piece, after) = rest.split_at(room.min(rest.len()));
            self.chunk.update(piece);
            rest = after;
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
