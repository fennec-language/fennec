//! The tree: input is split into 1 KiB chunks, each chunk is compressed block by block into
//! a chaining value, and pairs of chaining values are merged into parents up to one root.

use crate::compress::{
    BLOCK_LEN, CHUNK_END, CHUNK_LEN, CHUNK_START, PARENT, ROOT, compress, first_eight, words,
};

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
