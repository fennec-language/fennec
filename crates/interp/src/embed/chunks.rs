//! A prototype, for measuring: small blocks in chunks of one size, found from their address with
//! one map read and arithmetic, instead of each having its own allocation and a table entry.
//! See `slab-and-shared-threads.md`. One thread only.
//!
//! A chunk's slots are never given back. What's handed out is only ever the start of a slot in
//! use, so an address that isn't one is refused here as the table refuses it.

/// Bits of an address a chunk covers.
const CHUNK_BITS: u32 = 16;
/// Words a chunk holds.
const CHUNK_WORDS: usize = 1 << (CHUNK_BITS - 3);
/// Bits of an address a region covers, and how many chunks that makes.
const REGION_BITS: u32 = 32;
const CHUNKS: usize = 1 << (REGION_BITS - CHUNK_BITS);

/// The longest block, in words, kept in a chunk.
pub(super) const LARGEST_WORDS: usize = 64;
/// The shortest: its second word says whether its slot is in use.
const SHORTEST_WORDS: usize = 2;
/// What a slot not in use has for a shape word. No shape has this number.
const DEAD: u64 = u64::MAX;

#[derive(Clone)]
pub(super) struct Chunk {
    /// More than a chunk's words, so that a chunk's worth of them start on a chunk's boundary.
    words: Box<[u64]>,
    /// Where in `words` the chunk's first word is, and its address.
    first: usize,
    /// How long a block here is, how far apart they are to keep them 16 bytes apart, and a
    /// number for dividing by that with a multiplication.
    size: usize,
    stride: usize,
    inverse: usize,
    /// Words of the chunk given out so far, which are the only ones that can be blocks.
    used: usize,
}

type Region = Box<[Option<Box<Chunk>>]>;

/// Every small block.
pub(super) struct Chunks {
    regions: Vec<(usize, Region)>,
    /// The address of each size's first free slot, or 0. Each free slot's first word has the next.
    free: [usize; LARGEST_WORDS + 1],
    /// The address of the chunk each size is being taken from, or 0.
    current: [usize; LARGEST_WORDS + 1],
    /// Slots in use.
    len: usize,
}

impl Default for Chunks {
    fn default() -> Chunks {
        Chunks {
            regions: Vec::new(),
            free: [0; LARGEST_WORDS + 1],
            current: [0; LARGEST_WORDS + 1],
            len: 0,
        }
    }
}

fn split(address: usize) -> (usize, usize) {
    (address.checked_shr(REGION_BITS).unwrap_or(0), (address >> CHUNK_BITS) % CHUNKS)
}

impl Chunk {
    fn new(size: usize) -> Chunk {
        let words = vec![0; CHUNK_WORDS * 2].into_boxed_slice();
        let start = words.as_ptr().addr();
        let aligned = start.next_multiple_of(1 << CHUNK_BITS);
        let stride = size.next_multiple_of(2);
        Chunk {
            words,
            first: (aligned - start) / 8,
            size,
            stride,
            inverse: (u32::MAX as usize) / stride + 1,
            used: 0,
        }
    }

    fn address(&self) -> usize {
        self.words.as_ptr().addr() + self.first * 8
    }

    /// Where the block starting at `address` is in `words`, if a slot given out starts there.
    #[inline]
    fn place(&self, address: usize) -> Option<usize> {
        if !address.is_multiple_of(8) {
            return None;
        }
        let offset = (address % (1 << CHUNK_BITS)) / 8;
        let slot = (offset * self.inverse) >> 32;
        (slot * self.stride == offset && offset < self.used).then_some(self.first + offset)
    }
}

/// Blocks up to this long are copied and cleared with plain stores, which costs far less than
/// calling `memcpy` or `memset`.
pub(super) const SMALL_WORDS: usize = 8;

fn copy_words<const N: usize>(from: &[u64], to: &mut [u64]) -> bool {
    match (<&[u64; N]>::try_from(from), to.first_chunk_mut::<N>()) {
        (Ok(from), Some(to)) => {
            *to = *from;
            true
        }
        _ => false,
    }
}

/// Copies a short block to the start of `to`, saying whether it was short enough.
#[inline]
pub(super) fn copy_small(from: &[u64], to: &mut [u64]) -> bool {
    match from.len() {
        2 => copy_words::<2>(from, to),
        3 => copy_words::<3>(from, to),
        4 => copy_words::<4>(from, to),
        5 => copy_words::<5>(from, to),
        6 => copy_words::<6>(from, to),
        7 => copy_words::<7>(from, to),
        8 => copy_words::<8>(from, to),
        _ => false,
    }
}

/// The chunk holding `address`, if one does.
#[inline]
fn chunk_in(regions: &mut [(usize, Region)], address: usize) -> Option<&mut Chunk> {
    let (region, chunk) = split(address);
    let (_, chunks) = regions.iter_mut().find(|(number, _)| *number == region)?;
    chunks.get_mut(chunk)?.as_deref_mut()
}

impl Chunk {
    /// The block at `address`, if it's one in use.
    #[inline]
    pub(super) fn block(&self, address: usize) -> Option<&[u64]> {
        let at = self.place(address)?;
        self.words.get(at..at + self.size).filter(|block| block.get(1) != Some(&DEAD))
    }

    /// The block at `address`, if it's one in use, to change.
    #[inline]
    pub(super) fn block_mut(&mut self, address: usize) -> Option<&mut [u64]> {
        let at = self.place(address)?;
        let size = self.size;
        self.words.get_mut(at..at + size).filter(|block| block.get(1) != Some(&DEAD))
    }
}

impl Chunks {
    pub(super) fn len(&self) -> usize {
        self.len
    }

    /// The chunk holding `address`, if one does: if one does, nothing else can have it.
    #[inline]
    pub(super) fn chunk(&self, address: usize) -> Option<&Chunk> {
        let (region, chunk) = split(address);
        let (_, chunks) = self.regions.iter().find(|(number, _)| *number == region)?;
        chunks.get(chunk)?.as_deref()
    }

    #[inline]
    pub(super) fn chunk_mut(&mut self, address: usize) -> Option<&mut Chunk> {
        chunk_in(&mut self.regions, address)
    }

    /// The block at `address`, if it's one in use.
    #[cfg(test)]
    fn get(&self, address: usize) -> Option<&[u64]> {
        self.chunk(address)?.block(address)
    }

    /// A block of `words` words with its address, its words whatever they were. None for a block
    /// too long or too short.
    #[inline]
    pub(super) fn make_unset(&mut self, words: usize) -> Option<(usize, &mut [u64])> {
        if !(SHORTEST_WORDS..=LARGEST_WORDS).contains(&words) {
            return None;
        }
        let head = *self.free.get(words)?;
        self.len += 1;
        if head == 0 {
            let address = self.new_slot(words)?;
            let chunk = chunk_in(&mut self.regions, address)?;
            let at = chunk.place(address)?;
            let size = chunk.size;
            return Some((address, chunk.words.get_mut(at..at + size)?));
        }
        let chunk = chunk_in(&mut self.regions, head)?;
        let at = chunk.place(head)?;
        let size = chunk.size;
        let block = chunk.words.get_mut(at..at + size)?;
        *self.free.get_mut(words)? = usize::try_from(block.first().copied().unwrap_or(0)).unwrap_or(0);
        // A slot no one has set can't be told from one in use, so this is only ever seen set.
        Some((head, block))
    }

    /// A block of `words` words, all zero, with its address.
    #[inline]
    pub(super) fn make(&mut self, words: usize) -> Option<(usize, &mut [u64])> {
        let (address, block) = self.make_unset(words)?;
        match block {
            [a, b] => (*a, *b) = (0, 0),
            [a, b, c] => (*a, *b, *c) = (0, 0, 0),
            [a, b, c, d] => (*a, *b, *c, *d) = (0, 0, 0, 0),
            [a, b, c, d, e] => (*a, *b, *c, *d, *e) = (0, 0, 0, 0, 0),
            [a, b, c, d, e, f] => (*a, *b, *c, *d, *e, *f) = (0, 0, 0, 0, 0, 0),
            _ => block.fill(0),
        }
        Some((address, block))
    }

    /// Lets the block at `address` go, if it's one in use, saying how long it was and putting its
    /// words in `small` if they fit there, or else in `long`.
    #[inline]
    pub(super) fn take(
        &mut self,
        address: usize,
        small: &mut [u64; SMALL_WORDS],
        long: &mut Vec<u64>,
    ) -> Option<usize> {
        let block = chunk_in(&mut self.regions, address)?.block_mut(address)?;
        let words = block.len();
        if !copy_small(block, small) {
            long.clear();
            long.extend_from_slice(block);
        }
        let head = self.free.get_mut(words)?;
        if let [next, shape, ..] = block {
            *next = u64::try_from(*head).unwrap_or(0);
            *shape = DEAD;
        }
        *head = address;
        self.len -= 1;
        Some(words)
    }

    /// Lets the block at `address` go, if it's one in use and `keep` doesn't say to keep it.
    #[inline]
    pub(super) fn remove_unless(&mut self, address: usize, keep: impl Fn(&[u64]) -> bool) -> bool {
        let Some(block) = chunk_in(&mut self.regions, address).and_then(|chunk| chunk.block_mut(address))
        else {
            return false;
        };
        if keep(block) {
            return false;
        }
        let Some(head) = self.free.get_mut(block.len()) else { return false };
        if let [next, shape, ..] = block {
            *next = u64::try_from(*head).unwrap_or(0);
            *shape = DEAD;
        }
        *head = address;
        self.len -= 1;
        true
    }

    /// Lets the block at `address` go, if it's one in use.
    #[inline]
    pub(super) fn remove(&mut self, address: usize) -> bool {
        self.remove_unless(address, |_| false)
    }

    /// The next slot never used, in the size's chunk or a new one.
    fn new_slot(&mut self, words: usize) -> Option<usize> {
        let current = *self.current.get(words)?;
        if current != 0
            && let Some(chunk) = self.chunk_mut(current)
            && chunk.used + chunk.stride <= CHUNK_WORDS
        {
            let address = chunk.address() + chunk.used * 8;
            chunk.used += chunk.stride;
            return Some(address);
        }
        self.new_chunk(words)
    }

    #[cold]
    #[inline(never)]
    fn new_chunk(&mut self, words: usize) -> Option<usize> {
        let mut chunk = Box::new(Chunk::new(words));
        let address = chunk.address();
        chunk.used = chunk.stride;
        let (region, at) = split(address);
        let found = self.regions.iter().position(|(number, _)| *number == region).unwrap_or_else(|| {
            self.regions.push((region, vec![None; CHUNKS].into_boxed_slice()));
            self.regions.len() - 1
        });
        *self.regions.get_mut(found)?.1.get_mut(at)? = Some(chunk);
        *self.current.get_mut(words)? = address;
        Some(address)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hands_out_blocks_it_can_find_and_refuses_everything_else() {
        let mut chunks = Chunks::default();
        let mut made = Vec::new();
        for n in 0..20_000_usize {
            let size = 2 + n % 63;
            let (address, block) = chunks.make(size).unwrap();
            assert_eq!(block.len(), size);
            assert!(block.iter().all(|word| *word == 0));
            block[0] = u64::try_from(n).unwrap();
            assert_eq!(address % 16, 0);
            made.push((address, n));
        }
        assert_eq!(chunks.len(), 20_000);
        for (address, n) in &made {
            let block = chunks.get(*address).unwrap();
            assert_eq!(block[0], u64::try_from(*n).unwrap());
            assert_eq!(block.len(), 2 + n % 63);
            // Inside a block, or between two, is never a block: they start 16 bytes apart at least.
            assert!(chunks.get(address + 1).is_none());
            assert!(chunks.get(address + 8).is_none());
        }
        for (address, _) in made.iter().step_by(2) {
            assert!(chunks.remove(*address));
            assert!(chunks.get(*address).is_none());
            assert!(!chunks.remove(*address));
        }
        assert_eq!(chunks.len(), 10_000);
        let (again, block) = chunks.make(2).unwrap();
        assert!(block.iter().all(|word| *word == 0));
        assert_eq!(chunks.get(again).map(<[u64]>::len), Some(2));
        assert_eq!(chunks.get(0), None);
        assert_eq!(chunks.get(usize::MAX), None);
    }

    #[test]
    fn leaves_what_it_doesnt_keep_alone() {
        let mut chunks = Chunks::default();
        assert!(chunks.make(1).is_none());
        assert!(chunks.make(LARGEST_WORDS + 1).is_none());
        assert!(chunks.chunk(16).is_none());
    }
}
