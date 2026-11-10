//! Every live block by its address, in a page map: an address's high bits pick a region, its
//! middle bits a page in the region, and its low bits the block's place in the page. Blocks
//! made near each other sit near each other here too, which a hash table can't give, and
//! finding one is a few array reads with no hashing.

use std::collections::HashMap;
use std::hash::BuildHasherDefault;

use super::heap::AddressHasher;

/// Bits of an address a region covers, a page covers, and a block's place in a page skips:
/// blocks two words or longer never start within 16 bytes of each other.
const REGION_BITS: u32 = 32;
const PAGE_BITS: u32 = 16;
const PLACE_BITS: u32 = 4;
const PAGES: usize = 1 << (REGION_BITS - PAGE_BITS);
const PLACES: usize = 1 << (PAGE_BITS - PLACE_BITS);

/// How many pages a table makes before it first looks for empty ones to give back. After that
/// it looks once it's made as many as it kept the last time, so looking costs a page's worth
/// of reading for each page made, and a table holds at most about twice the pages its blocks
/// are on.
const PAGES_BEFORE_LOOKING: usize = 64;

/// The shortest block, in words, that has a page's place to itself.
const PLACED_WORDS: usize = 2;

/// A page's places and a region's pages are fixed in size, so finding a place checks no bounds.
type Page = Box<[Option<Box<[u64]>>; PLACES]>;
type Pages = Box<[Option<Page>; PAGES]>;

#[derive(Default)]
pub(super) struct Table {
    /// Each region's pages, by the region's number. A run uses one or two regions.
    regions: Vec<(usize, Pages)>,
    /// Whether every block goes in `short`, for a table that's usually small: pages cost a lot more
    /// than a few blocks do.
    hashed: bool,
    /// Blocks too short to have a place of their own.
    short: HashMap<usize, Box<[u64]>, BuildHasherDefault<AddressHasher>>,
    len: usize,
    /// The block the heap's kept a place in. A block can't leave while the heap still has one,
    /// as its address could come back as another string, and a wrong place gives no failure.
    place_in: usize,
    /// Pages made since it last gave back the empty ones, and how many it kept then. A
    /// long-running program's blocks wander through the allocator's memory, so without giving
    /// pages back the table would grow by a page for each place they'd ever been.
    made: usize,
    kept: usize,
}

/// What taking an owner off a block found.
pub(super) enum Released {
    /// That was its last owner, and here it is, out of the table.
    Gone(Box<[u64]>),
    /// It has owners left.
    Held,
    /// No block of this table's is there.
    Missing,
}

/// An address's region, page in it, and place in the page. A 32-bit address space is one
/// region.
fn split(address: usize) -> (usize, usize, usize) {
    let region = address.checked_shr(REGION_BITS).unwrap_or(0);
    (region, (address >> PAGE_BITS) % PAGES, (address >> PLACE_BITS) % PLACES)
}

fn at(block: &[u64]) -> usize {
    block.as_ptr().addr()
}

/// A region's pages. The first region is looked at before any search: a run's blocks are
/// almost all in one.
fn pages_of(regions: &[(usize, Pages)], region: usize) -> Option<&Pages> {
    match regions.split_first()? {
        ((number, pages), _) if *number == region => Some(pages),
        (_, rest) => rest.iter().find(|(number, _)| *number == region).map(|(_, pages)| pages),
    }
}

/// `pages_of`, to change them.
fn pages_of_mut(regions: &mut [(usize, Pages)], region: usize) -> Option<&mut Pages> {
    match regions.split_first_mut()? {
        ((number, pages), _) if *number == region => Some(pages),
        (_, rest) => rest.iter_mut().find(|(number, _)| *number == region).map(|(_, pages)| pages),
    }
}

/// The place for `address`, on a page that's been made, whatever it holds.
fn place_mut(regions: &mut [(usize, Pages)], address: usize) -> Option<&mut Option<Box<[u64]>>> {
    let (region, page, place) = split(address);
    pages_of_mut(regions, region)?.get_mut(page)?.as_mut()?.get_mut(place)
}

/// The place holding the block at `address`, when one does.
fn slot_mut(regions: &mut [(usize, Pages)], address: usize) -> Option<&mut Option<Box<[u64]>>> {
    let slot = place_mut(regions, address)?;
    slot.as_ref().is_some_and(|block| at(block) == address).then_some(slot)
}

/// A region's pages or a page's places, as a slice.
fn boxed_slice<T, const N: usize>(items: Box<[T; N]>) -> Box<[T]> {
    items
}

/// A region's pages or a page's places, none made yet.
fn empty<T: Clone, const N: usize>() -> Option<Box<[Option<T>; N]>> {
    vec![None; N].into_boxed_slice().try_into().ok()
}

impl Table {
    /// A table for a `shared` value's blocks.
    pub(super) fn hashed() -> Table {
        Table { hashed: true, ..Table::default() }
    }

    /// All its blocks, emptying it.
    pub(super) fn into_blocks(self) -> impl Iterator<Item = Box<[u64]>> {
        let pages = self.regions.into_iter().flat_map(|(_, pages)| boxed_slice(pages).into_vec());
        let places = pages.flatten().flat_map(|page| boxed_slice(page).into_vec()).flatten();
        places.chain(self.short.into_values())
    }

    pub(super) fn len(&self) -> usize {
        self.len
    }

    /// How many pages it holds, for counting what's live.
    #[cfg(any(test, feature = "threads"))]
    pub(super) fn pages(&self) -> usize {
        self.regions.iter().map(|(_, pages)| pages.iter().flatten().count()).sum()
    }

    /// How many blocks it holds of each size, in words, for counting what's live.
    #[cfg(any(test, feature = "threads"))]
    pub(super) fn sizes(&self) -> std::collections::BTreeMap<usize, usize> {
        let mut sizes = std::collections::BTreeMap::new();
        let placed = self.regions.iter().flat_map(|(_, pages)| pages.iter().flatten());
        for block in placed.flat_map(|page| page.iter().flatten()).chain(self.short.values()) {
            *sizes.entry(block.len()).or_default() += 1;
        }
        sizes
    }

    fn slot(&self, address: usize) -> Option<&[u64]> {
        let (region, page, place) = split(address);
        pages_of(&self.regions, region)?
            .get(page)?
            .as_ref()?
            .get(place)?
            .as_deref()
            .filter(|block| at(block) == address)
    }

    pub(super) fn get(&self, address: usize) -> Option<&[u64]> {
        match self.slot(address) {
            Some(block) => Some(block),
            None if self.short.is_empty() => None,
            None => self.short.get(&address).map(AsRef::as_ref),
        }
    }

    pub(super) fn get_mut(&mut self, address: usize) -> Option<&mut [u64]> {
        match slot_mut(&mut self.regions, address).and_then(|slot| slot.as_deref_mut()) {
            Some(block) => Some(block),
            None if self.short.is_empty() => None,
            None => self.short.get_mut(&address).map(AsMut::as_mut),
        }
    }

    /// Adds a block at its own address, replacing one already there.
    pub(super) fn insert(&mut self, block: Box<[u64]>) {
        let address = at(&block);
        if block.len() < PLACED_WORDS || self.hashed {
            self.insert_short(address, block);
            return;
        }
        match place_mut(&mut self.regions, address) {
            Some(slot) => {
                if slot.is_none() {
                    self.len += 1;
                }
                *slot = Some(block);
            }
            None => self.insert_in_new_page(block),
        }
    }

    /// `insert`, for a block too short to have a place of its own.
    #[cold]
    #[inline(never)]
    fn insert_short(&mut self, address: usize, block: Box<[u64]>) {
        if self.short.insert(address, block).is_none() {
            self.len += 1;
        }
    }

    /// `insert`, for a block on a page with no block yet.
    #[cold]
    #[inline(never)]
    fn insert_in_new_page(&mut self, block: Box<[u64]>) {
        let (region, page, place) = split(at(&block));
        if pages_of(&self.regions, region).is_none() {
            let Some(pages) = empty() else { return };
            self.regions.push((region, pages));
        }
        let Some(page) = pages_of_mut(&mut self.regions, region).and_then(|pages| pages.get_mut(page)) else {
            return;
        };
        if page.is_none() {
            *page = empty();
            self.made += 1;
        }
        if let Some(slot) = page.as_mut().and_then(|page| page.get_mut(place)) {
            if slot.is_none() {
                self.len += 1;
            }
            *slot = Some(block);
        }
        if self.made >= self.kept.max(PAGES_BEFORE_LOOKING) {
            self.give_back_empty_pages();
        }
    }

    /// Lets go of every page with no block on it.
    #[cold]
    #[inline(never)]
    fn give_back_empty_pages(&mut self) {
        let mut kept = 0;
        for (_, pages) in &mut self.regions {
            for page in pages.iter_mut().filter(|page| page.is_some()) {
                if page.as_ref().is_some_and(|places| places.iter().all(Option::is_none)) {
                    *page = None;
                } else {
                    kept += 1;
                }
            }
        }
        self.kept = kept;
        self.made = 0;
    }

    /// Takes one owner off the block at `address`, and takes the block out when that was its last,
    /// in one look.
    pub(super) fn release_one(&mut self, address: usize) -> Released {
        let Some(slot) = slot_mut(&mut self.regions, address) else { return self.release_short(address) };
        let Some(count) = slot.as_mut().and_then(|block| block.first_mut()) else { return Released::Missing };
        if *count > 1 {
            *count -= 1;
            return Released::Held;
        }
        self.len -= 1;
        slot.take().map_or(Released::Missing, Released::Gone)
    }

    /// `release_one`, for a block too short to have a place of its own.
    #[cold]
    #[inline(never)]
    fn release_short(&mut self, address: usize) -> Released {
        let Some(count) = self.short.get_mut(&address).and_then(|block| block.first_mut()) else {
            return Released::Missing;
        };
        if *count > 1 {
            *count -= 1;
            return Released::Held;
        }
        self.len -= 1;
        self.short.remove(&address).map_or(Released::Missing, Released::Gone)
    }

    /// Says which block the heap's keeping a place in, or 0 for none.
    pub(super) fn note_place(&mut self, address: usize) {
        self.place_in = address;
    }

    pub(super) fn remove(&mut self, address: usize) -> Option<Box<[u64]>> {
        // No block has address 0, which says no place is kept.
        debug_assert!(address == 0 || address != self.place_in, "a string left with its place still kept");
        self.remove_any(address)
    }

    /// Takes the block at `address` out when `wanted` says so of it, finding it once. `None`,
    /// having changed nothing, when it isn't here or isn't wanted.
    pub(super) fn take_if(&mut self, address: usize, wanted: impl FnOnce(&[u64]) -> bool) -> Option<Box<[u64]>> {
        let taken = match slot_mut(&mut self.regions, address) {
            Some(slot) => slot.take_if(|block| wanted(block)),
            None if self.short.get(&address).is_some_and(|block| wanted(block)) => self.short.remove(&address),
            None => None,
        };
        if taken.is_some() {
            self.len -= 1;
        }
        taken
    }

    /// `remove` for what can't yet tell whether this is a string, and so forgets its place after.
    pub(super) fn remove_any(&mut self, address: usize) -> Option<Box<[u64]>> {
        let removed = match slot_mut(&mut self.regions, address) {
            Some(slot) => slot.take(),
            None => self.short.remove(&address),
        };
        if removed.is_some() {
            self.len -= 1;
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "a string left with its place still kept")]
    #[cfg(debug_assertions)]
    fn a_block_cant_leave_while_a_place_is_kept_in_it() {
        let mut table = Table::default();
        let block: Box<[u64]> = vec![1, 2, 3, 4].into_boxed_slice();
        let at = block.as_ptr().addr();
        table.insert(block);
        table.note_place(at);
        table.remove(at);
    }

    #[test]
    fn takes_a_block_only_when_wanted() {
        let mut table = Table::default();
        let placed: Box<[u64]> = vec![7; PLACED_WORDS].into_boxed_slice();
        let short: Box<[u64]> = vec![8; 2].into_boxed_slice();
        let (placed_at, short_at) = (at(&placed), at(&short));
        table.insert(placed);
        table.insert(short);
        for address in [placed_at, short_at] {
            assert!(table.take_if(address, |_| false).is_none());
            assert!(table.get(address).is_some(), "a block not wanted stays");
        }
        assert_eq!(table.len(), 2);
        assert!(table.take_if(placed_at + 8, |_| true).is_none(), "nothing's taken from inside a block");
        assert_eq!(table.take_if(placed_at, |block| block[0] == 7).map(|block| at(&block)), Some(placed_at));
        assert_eq!(table.take_if(short_at, |block| block[0] == 8).map(|block| at(&block)), Some(short_at));
        assert_eq!(table.len(), 0);
        assert!(table.get(placed_at).is_none() && table.get(short_at).is_none());
        assert!(table.take_if(placed_at, |_| true).is_none(), "a block's taken once");
    }

    #[test]
    fn gives_back_pages_whose_blocks_have_all_left() {
        let mut table = Table::default();
        // Kept outside the table, so each round's blocks land where no earlier round's did, as a
        // long-running server's blocks wander through the allocator's memory.
        let mut elsewhere = Vec::new();
        for _ in 0..10 {
            // 64 KB each, so each starts on a page of its own.
            let blocks: Vec<Box<[u64]>> = (0..50).map(|_| vec![1; 1 << 13].into()).collect();
            let addresses: Vec<usize> = blocks.iter().map(|block| at(block)).collect();
            for block in blocks {
                table.insert(block);
            }
            elsewhere.extend(addresses.into_iter().filter_map(|address| table.remove(address)));
        }
        assert_eq!(table.len(), 0);
        assert!(table.pages() < 200, "{} pages kept for no blocks", table.pages());
    }

    #[test]
    fn with_no_place_kept_any_address_can_be_asked_for() {
        assert!(Table::default().remove(0).is_none());
    }

    #[test]
    fn a_block_can_leave_once_its_place_is_forgotten() {
        let mut table = Table::default();
        let block: Box<[u64]> = vec![1, 2, 3, 4].into_boxed_slice();
        let at = block.as_ptr().addr();
        table.insert(block);
        table.note_place(at);
        table.note_place(0);
        assert!(table.remove(at).is_some());
    }

    #[test]
    fn finds_each_block_only_by_its_own_address() {
        let mut table = Table::default();
        let blocks: Vec<Box<[u64]>> =
            (0..1000_u64).map(|n| vec![n; 2 + usize::try_from(n % 5).unwrap_or(0)].into()).collect();
        let addresses: Vec<usize> = blocks.iter().map(|block| at(block)).collect();
        for block in blocks {
            table.insert(block);
        }
        assert_eq!(table.len(), 1000);
        for (n, address) in (0..1000_u64).zip(&addresses) {
            assert_eq!(table.get(*address).and_then(<[u64]>::first), Some(&n));
            assert_eq!(table.get(address + 8), None);
        }
        for address in addresses.iter().step_by(2) {
            assert!(table.remove(*address).is_some());
            assert!(table.remove(*address).is_none());
        }
        assert_eq!(table.len(), 500);
        assert!(addresses.iter().skip(1).step_by(2).all(|address| table.get(*address).is_some()));
    }

    #[test]
    fn keeps_short_blocks_and_changes_blocks_in_place() {
        let mut table = Table::default();
        let short: Box<[u64]> = vec![7].into();
        let long: Box<[u64]> = vec![1, 2, 3].into();
        let (short_at, long_at) = (at(&short), at(&long));
        table.insert(short);
        table.insert(long);
        if let Some(word) = table.get_mut(long_at).and_then(<[u64]>::first_mut) {
            *word = 9;
        }
        assert_eq!(table.get(long_at), Some(&[9, 2, 3][..]));
        assert_eq!(table.get(short_at), Some(&[7][..]));
        assert_eq!(table.get(0), None);
        assert_eq!(table.get(usize::MAX), None);
        assert_eq!(table.len(), 2);
    }
}
