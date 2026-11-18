//! Every live block by its address, in a page map: an address's high bits pick a region, its
//! middle bits a page in the region, and its low bits the block's place in the page. Blocks
//! made near each other sit near each other here too, which a hash table can't give, and
//! finding one is a few array reads with no hashing.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
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
/// are on, or this many more than it kept, whichever is more.
///
/// It was 64. A server's tables make pages slowly once it's warm, as its blocks wander through
/// the allocator's memory, so a table's first look came half an hour into a soak, and each heap
/// held up to 4 MB of empty 64 KB pages until then: `web_backend`'s memory rose for 25 minutes, past
/// the soak's warm-up, and held at that high-water mark. A look reads only the pages made
/// (`made_pages`), so looking sooner costs no more for each page made.
const PAGES_BEFORE_LOOKING: usize = 8;

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
    /// Every page it has, by its region's number and its place among the region's pages, for a
    /// look to read: a region has room for 65,536 pages, and reading them all for each look cost
    /// more than the pages it made.
    made_pages: Vec<(usize, usize)>,
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

/// Stops the program: a block was put in a table at an address another block there already has.
/// Only a bug in Fennec gets here, two owners of one block's memory, and going on would free it
/// while the new one is used.
#[cfg(not(test))]
#[cold]
#[inline(never)]
fn two_owners(address: usize) -> ! {
    use std::io::Write as _;
    super::write_kept_output();
    let _ = writeln!(
        std::io::stderr(),
        "error: the runtime put a block at {address:#x}, where it already had one\n  \
         = help: this is a bug in Fennec; please report it"
    );
    super::aot::stop_now(1)
}

/// [`two_owners`] in a test, which goes on to be caught, where a program stops.
#[cfg(test)]
fn two_owners(address: usize) -> ! {
    panic!("the runtime put a block at {address:#x}, where it already had one");
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

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.len
    }

    /// How many places looking through all of it reads (`all`): each page a region can have, and
    /// each place on every page it has. Kept as it goes, so asking costs nothing.
    pub(super) fn places_to_look_through(&self) -> usize {
        self.regions.len() * PAGES + (self.kept + self.made) * PLACES
    }

    /// How many pages it holds, for counting what's live.
    #[cfg(any(test, feature = "threads"))]
    pub(super) fn pages(&self) -> usize {
        self.regions.iter().map(|(_, pages)| pages.iter().flatten().count()).sum()
    }

    /// How many blocks it holds of each size, in words, for counting what's live, and apart from
    /// them how many are never freed: each a literal's, a constant's, a capability's or a case's
    /// with no fields, made once, the first time it's asked for, and kept for the run.
    #[cfg(any(test, feature = "threads"))]
    pub(super) fn sizes(&self) -> (std::collections::BTreeMap<usize, usize>, usize) {
        let mut sizes = std::collections::BTreeMap::new();
        let mut never_freed = 0;
        let placed = self.regions.iter().flat_map(|(_, pages)| pages.iter().flatten());
        for block in placed.flat_map(|page| page.iter().flatten()).chain(self.short.values()) {
            if super::heap::is_never_freed(block) {
                never_freed += 1;
            } else {
                *sizes.entry(block.len()).or_default() += 1;
            }
        }
        (sizes, never_freed)
    }

    /// Every block it holds, in no order: for the heap's audit to read where it lies, and for a
    /// long task's end to find the blocks with its mark.
    #[cfg(any(test, feature = "audit", feature = "threads"))]
    pub(super) fn all(&self) -> impl Iterator<Item = &[u64]> {
        let placed = self.regions.iter().flat_map(|(_, pages)| pages.iter().flatten());
        placed.flat_map(|page| page.iter().flatten()).chain(self.short.values()).map(|block| &**block)
    }

    /// How many blocks it holds that can still be freed: all but those kept for the whole run, like
    /// literals and the cases with no fields made the first time they're asked for.
    pub(super) fn freeable(&self) -> usize {
        self.freeable_where(|_| true)
    }

    /// How many of the blocks that can still be freed `which` picks.
    pub(super) fn freeable_where(&self, which: impl Fn(&[u64]) -> bool) -> usize {
        let placed = self.regions.iter().flat_map(|(_, pages)| pages.iter().flatten());
        let never_freed = |block: &[u64]| {
            block
                .get(1)
                .is_some_and(|shape| *shape >> super::heap::OWNER_SHIFT == super::heap::NEVER_FREED_OWNER)
        };
        placed
            .flat_map(|page| page.iter().flatten())
            .chain(self.short.values())
            .filter(|block| !never_freed(block) && which(block))
            .count()
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

    /// Adds a block at its own address. Another already there would be a second owner of the same
    /// memory, which only a bug in Fennec makes, so the program stops rather than free it under
    /// the new one.
    pub(super) fn insert(&mut self, block: Box<[u64]>) {
        let address = at(&block);
        if block.len() < PLACED_WORDS || self.hashed {
            self.insert_short(address, block);
            return;
        }
        // A block taken over may be in the hash map at this address, under a page made since. Every
        // block passes here, so only audited and test builds look.
        if (cfg!(test) || cfg!(feature = "audit"))
            && !self.short.is_empty()
            && self.short.contains_key(&address)
        {
            two_owners(address);
        }
        match place_mut(&mut self.regions, address) {
            Some(slot) => {
                if slot.is_some() {
                    two_owners(address);
                }
                self.len += 1;
                *slot = Some(block);
            }
            None => self.insert_in_new_page(block),
        }
    }

    /// `insert`, for a block too short to have a place of its own.
    #[cold]
    #[inline(never)]
    fn insert_short(&mut self, address: usize, block: Box<[u64]>) {
        // Found once, through its entry.
        match self.short.entry(address) {
            Entry::Occupied(_) => two_owners(address),
            Entry::Vacant(place) => {
                place.insert(block);
            }
        }
        self.len += 1;
    }

    /// `insert`, for a block another thread made, as a message's is: placed when its page is
    /// made, and otherwise in the hash map, as its page would cover that thread's memory, mostly
    /// that thread's own blocks. Each thread a server's messages reached made a page there for
    /// each, and kept it while any block on it lived: about 22 KB of a chat server's 113 KB a
    /// connection, at 4 threads.
    pub(super) fn insert_adopted(&mut self, block: Box<[u64]>) {
        let address = at(&block);
        if block.len() < PLACED_WORDS || self.hashed {
            self.insert_short(address, block);
            return;
        }
        match place_mut(&mut self.regions, address) {
            Some(slot) => {
                // One taken over before its page was made is in the hash map, not the place.
                if slot.is_some() || (!self.short.is_empty() && self.short.contains_key(&address)) {
                    two_owners(address);
                }
                self.len += 1;
                *slot = Some(block);
            }
            None => self.insert_short(address, block),
        }
    }

    /// `insert`, for a block on a page with no block yet.
    #[cold]
    #[inline(never)]
    fn insert_in_new_page(&mut self, block: Box<[u64]>) {
        let (region, at_page, place) = split(at(&block));
        if pages_of(&self.regions, region).is_none() {
            let Some(pages) = empty() else { return };
            self.regions.push((region, pages));
        }
        let Some(page) = pages_of_mut(&mut self.regions, region).and_then(|pages| pages.get_mut(at_page))
        else {
            return;
        };
        if page.is_none() {
            *page = empty();
            self.made += 1;
            self.made_pages.push((region, at_page));
        }
        if let Some(slot) = page.as_mut().and_then(|page| page.get_mut(place)) {
            if slot.is_some() {
                two_owners(at(&block));
            }
            self.len += 1;
            *slot = Some(block);
        }
        if self.made >= self.kept.max(PAGES_BEFORE_LOOKING) {
            self.give_back_empty_pages();
        }
    }

    /// Lets go of every page with no block on it, reading only the pages it has.
    #[cold]
    #[inline(never)]
    fn give_back_empty_pages(&mut self) {
        let regions = &mut self.regions;
        self.made_pages.retain(|&(region, at_page)| {
            let Some(page) = pages_of_mut(regions, region).and_then(|pages| pages.get_mut(at_page)) else {
                return false;
            };
            if page.as_ref().is_none_or(|places| places.iter().all(Option::is_none)) {
                *page = None;
                return false;
            }
            true
        });
        self.kept = self.made_pages.len();
        self.made = 0;
    }

    /// Takes one owner off the block at `address`, and takes the block out when that was its last,
    /// in one look.
    pub(super) fn release_one(&mut self, address: usize) -> Released {
        let Some(slot) = slot_mut(&mut self.regions, address) else { return self.release_short(address) };
        let Some(block) = slot.as_deref_mut() else { return Released::Missing };
        let Some(&count) = block.first() else { return Released::Missing };
        if count > 1 {
            if (count < super::heap::IMMORTAL || !super::heap::uncounted(block))
                && let Some(first) = block.first_mut()
            {
                *first = count - 1;
            }
            return Released::Held;
        }
        self.len -= 1;
        slot.take().map_or(Released::Missing, Released::Gone)
    }

    /// `release_one`, for a block too short to have a place of its own.
    #[cold]
    #[inline(never)]
    fn release_short(&mut self, address: usize) -> Released {
        // Found once, through its entry, whether it's held or taken out.
        let Entry::Occupied(mut entry) = self.short.entry(address) else { return Released::Missing };
        let block = entry.get_mut();
        let Some(&count) = block.first() else { return Released::Missing };
        if count > 1 {
            if !super::heap::uncounted(block)
                && let Some(first) = block.first_mut()
            {
                *first = count - 1;
            }
            return Released::Held;
        }
        self.len -= 1;
        Released::Gone(entry.remove())
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
    pub(super) fn take_if(
        &mut self,
        address: usize,
        wanted: impl FnOnce(&[u64]) -> bool,
    ) -> Option<Box<[u64]>> {
        let taken = match slot_mut(&mut self.regions, address) {
            Some(slot) => slot.take_if(|block| wanted(block)),
            None if self.short.get(&address).is_some_and(|block| wanted(block)) => {
                self.short.remove(&address)
            }
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

    /// A block put where the table already has one stops the program, rather than free one under
    /// the other: two empty blocks are the only ones safe code can give one address.
    #[test]
    #[should_panic(expected = "where it already had one")]
    fn a_block_put_where_another_is_stops_rather_than_frees_it() {
        let mut table = Table::hashed();
        let first: Box<[u64]> = Box::new([]);
        let second: Box<[u64]> = Box::new([]);
        table.insert(first);
        table.insert(second);
    }

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

    /// A table of few blocks holds few empty pages: it looks for them once it's made a few, so a
    /// server's tables, making pages slowly as their blocks wander, give them back within
    /// minutes rather than the half hour 64 took.
    #[test]
    fn a_table_of_few_blocks_holds_few_empty_pages() {
        let mut table = Table::default();
        // Kept outside the table, so each block lands where no earlier one did.
        let mut elsewhere = Vec::new();
        // One block that lives all along, as a server's own state does.
        table.insert(vec![1; 1 << 13].into());
        for _ in 0..100 {
            // 64 KB each, so each starts on a page of its own.
            let block: Box<[u64]> = vec![1; 1 << 13].into();
            let address = at(&block);
            table.insert(block);
            elsewhere.extend(table.remove(address));
            assert!(table.pages() <= 2 + PAGES_BEFORE_LOOKING, "{} pages kept for one block", table.pages());
        }
        assert_eq!(table.len(), 1);
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

    /// A block another thread made makes no page: it's placed when its page is made, and otherwise
    /// kept in the hash map. Either way it's found, released and taken out as any other is.
    #[test]
    fn a_block_another_thread_made_makes_no_page() {
        let mut table = Table::default();
        // 64 KB each, so each starts on a page of its own.
        let adopted: Vec<Box<[u64]>> = (0..4).map(|_| vec![1; 1 << 13].into()).collect();
        let addresses: Vec<usize> = adopted.iter().map(|block| at(block)).collect();
        for block in adopted {
            table.insert_adopted(block);
        }
        assert_eq!(table.pages(), 0, "no page is made for blocks another thread made");
        assert_eq!(table.len(), 4);
        // One made here makes its page, and a block adopted onto that page is placed there.
        let here: Box<[u64]> = vec![2; 1 << 13].into();
        let (here_at, here_len) = (at(&here), here.len());
        table.insert(here);
        assert_eq!(table.pages(), 1);
        for address in &addresses {
            assert_eq!(table.get(*address).map(<[u64]>::len), Some(1 << 13));
            assert!(matches!(table.release_one(*address), Released::Gone(_)));
            assert!(table.get(*address).is_none());
        }
        assert_eq!(table.get(here_at).map(<[u64]>::len), Some(here_len));
        let beside: Box<[u64]> = vec![3; 2].into();
        let beside_at = at(&beside);
        let page_of = |address: usize| address >> PAGE_BITS;
        if page_of(beside_at) == page_of(here_at) {
            table.insert_adopted(beside);
            assert_eq!(table.pages(), 1, "a block adopted onto a page that's made is placed there");
            assert!(table.remove(beside_at).is_some());
        }
        assert!(table.remove(here_at).is_some());
        assert_eq!(table.len(), 0);
    }

    /// three blocks on one 64 KB page. The first is adopted before the page is made,
    /// so it's in the hash map. The second is made here, which makes the page over the first one's
    /// address. The third is adopted after, onto the page. Each is then found, changed, counted
    /// down and taken out by its own address, and none by another's.
    #[test]
    fn a_page_made_over_a_hashed_block_leaves_every_block_found() {
        let page_of = |address: usize| address >> PAGE_BITS;
        // Small blocks made one after another: many share a page.
        let mut blocks: Vec<Box<[u64]>> = (0..4000_u64).map(|n| vec![2, n, n + 1].into()).collect();
        blocks.sort_by_key(|block| at(block));
        let mut same: Vec<Box<[u64]>> = Vec::new();
        while same.len() < 3 {
            let block = blocks.pop().expect("three blocks on one page");
            if same.first().is_some_and(|first| page_of(at(first)) != page_of(at(&block))) {
                same.clear();
            }
            same.push(block);
        }
        let (third, second, first) = (same.pop().unwrap(), same.pop().unwrap(), same.pop().unwrap());
        let (first_at, second_at, third_at) = (at(&first), at(&second), at(&third));
        let (first_n, second_n, third_n) = (first[1], second[1], third[1]);
        let mut table = Table::default();
        table.insert_adopted(first);
        assert_eq!((table.pages(), table.short.len(), table.len()), (0, 1, 1));
        table.insert(second);
        assert_eq!((table.pages(), table.short.len(), table.len()), (1, 1, 2));
        table.insert_adopted(third);
        assert_eq!((table.pages(), table.short.len(), table.len()), (1, 1, 3), "onto the page that's made");
        for (address, n) in [(first_at, first_n), (second_at, second_n), (third_at, third_n)] {
            assert_eq!(table.get(address).map(|block| block[1]), Some(n));
            assert_eq!(table.get(address + 8), None);
            assert_eq!(table.get_mut(address).map(|block| block[1]), Some(n));
            assert!(table.take_if(address, |_| false).is_none());
        }
        assert_eq!(table.all().count(), 3);
        assert_eq!(table.freeable(), 3);
        assert_eq!(table.sizes().0.get(&3), Some(&3));
        // Each has two owners: the first release holds it, the second takes it out.
        for address in [first_at, second_at, third_at] {
            assert!(matches!(table.release_one(address), Released::Held));
            assert_eq!(table.get(address).map(|block| block[0]), Some(1));
        }
        assert!(matches!(table.release_one(first_at), Released::Gone(_)));
        assert!(matches!(table.release_one(first_at), Released::Missing));
        assert_eq!((table.short.len(), table.len()), (0, 2));
        assert!(
            table.get(first_at).is_none() && table.get(second_at).is_some() && table.get(third_at).is_some()
        );
        assert!(table.take_if(third_at, |_| true).is_some());
        assert!(table.remove(second_at).is_some());
        assert_eq!(table.len(), 0);
        assert_eq!(table.into_blocks().count(), 0);
    }

    /// pages are given back while hashed blocks have addresses on them, and the
    /// hashed blocks are still found after, and after their pages are made again.
    #[test]
    fn hashed_blocks_outlive_pages_given_back_under_them() {
        let mut table = Table::default();
        let mut hashed = Vec::new();
        // The blocks made here and taken out, kept to the end, so each round's land elsewhere.
        let mut kept: Vec<Box<[u64]>> = Vec::new();
        for round in 0..6_u64 {
            // Small blocks, so several share each page: every other one is adopted, all of those
            // first, with no page made. The rest are made here after, which makes pages over the
            // adopted ones' addresses.
            let blocks: Vec<Box<[u64]>> = (0..40_000_u64).map(|n| vec![1, round, n].into()).collect();
            let mut here = Vec::new();
            let mut made_here = Vec::new();
            for (n, block) in blocks.into_iter().enumerate() {
                if n % 2 == 0 {
                    hashed.push((at(&block), block[1], block[2]));
                    table.insert_adopted(block);
                } else {
                    made_here.push(block);
                }
            }
            for block in made_here {
                here.push(at(&block));
                table.insert(block);
            }
            // The blocks made here go, leaving their pages empty, to be given back as more are made.
            let gone: Vec<Box<[u64]>> = here.iter().filter_map(|address| table.remove(*address)).collect();
            assert_eq!(gone.len(), here.len());
            for (address, round, n) in &hashed {
                assert_eq!(table.get(*address).map(|block| (block[1], block[2])), Some((*round, *n)));
            }
            kept.extend(gone);
        }
        assert_eq!(table.len(), hashed.len());
        assert_eq!(table.all().count(), hashed.len());
        for (address, ..) in &hashed {
            assert!(matches!(table.release_one(*address), Released::Gone(_)));
        }
        assert_eq!((table.len(), table.short.len()), (0, 0));
    }

    /// A block that's never freed is counted apart from the live ones, by its count or its owner:
    /// a case's block whose count has drifted below the mark is still its case's.
    #[test]
    fn blocks_never_freed_are_counted_apart() {
        use super::super::heap::{IMMORTAL, NEVER_FREED_OWNER, OWNER_SHIFT};
        let mut table = Table::default();
        table.insert(vec![1, 2, 3].into_boxed_slice());
        table.insert(vec![IMMORTAL, 2, 3, 4].into_boxed_slice());
        table.insert(
            vec![IMMORTAL - 2, (NEVER_FREED_OWNER << OWNER_SHIFT) | 0x17, 2, 0, 0].into_boxed_slice(),
        );
        assert_eq!(table.sizes(), ([(3, 1)].into(), 2));
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
