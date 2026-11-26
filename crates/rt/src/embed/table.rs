//! Every live block by its address, in a page map: an address's high bits pick a region, its
//! middle bits a page in the region, and its low bits the block's place in the page. Blocks
//! made near each other sit near each other here too, which a hash table can't give, and
//! finding one is a few array reads with no hashing.
//!
//!
//! A page's places hold numbers, not blocks: each is 0 for none, or one more than its block's
//! index in a dense list, `live`. A place is 4 bytes where a block's pointer is 8 or 16, so a page
//! is a quarter the size to make and read, and a look through every block reads the list, not
//! every place of every page: peak memory is 12% to 49% lower on the scoreboard's programs.
//!
//! - **A block is found only at its own address.** A lookup reads the place, then the entry it
//!   points at, and gives the block only when its address is the one asked for. Whatever a place
//!   holds, a block elsewhere is a miss, never a find.
//! - **Nothing outside this file sees an index.** Callers pass addresses and get blocks, so an
//!   index can't be held past its block's leaving.
//! - **The spare entries chain through themselves.** An empty entry holds the number of the next
//!   spare one, and the table the first, so taking a block out writes its entry and one number,
//!   and nothing grows or calls out.
//! - **The list shrinks.** A new block takes the index freed last, wherever it is, so the list's
//!   end stays in use. When pages are given back and fewer than half the entries hold a block,
//!   the blocks at its end move down into its holes, each place rewritten, and its room is let go.
//!   That's safe because an index is only ever in its block's place, and it runs under `&mut`, so
//!   never on a table that's lent.

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
/// are on.
const PAGES_BEFORE_LOOKING: usize = 64;

/// The same, for a worker going to sleep with nothing to do (`look_while_idle`).
///
/// A server's tables make pages slowly once it's warm, as its blocks wander through the
/// allocator's memory, so with 64 a table's first look came half an hour into a soak, and each
/// heap held up to 4 MB of empty 64 KB pages until then: `web_backend`'s memory rose for 25
/// minutes, past the soak's warm-up, and held at that high-water mark. A worker that's idle has
/// time to look sooner. A busy one keeps 64, which some programs reuse: `pathfind`, never idle,
/// ran 6.7% more instructions with 8 for every table, making pages again that it had given back.
#[cfg(any(test, feature = "threads"))]
const PAGES_BEFORE_LOOKING_IDLE: usize = 8;

/// How many entries `live` may have past twice the blocks it holds before it's cut down
/// (`cut_down`): 16 KB, so a small table isn't moved about for little.
const ENTRIES_KEPT_SPARE: usize = 1_024;

/// The shortest block, in words, that has a page's place to itself.
const PLACED_WORDS: usize = 2;

/// A page's places and a region's pages are fixed in size, so finding a place checks no bounds.
/// A place holds 0 for no block, or one more than its block's index in `live`.
type Page = Box<[u32; PLACES]>;
type Pages = Box<[Option<Page>; PAGES]>;
/// An entry of `live`: a placed block, or at an index that's spare the number of the next spare
/// one, 0 for none. A block's pointer is never null, so both fit in a block's 16 bytes.
enum Held {
    Block(Box<[u64]>),
    Spare(u32),
}

const _: () = assert!(size_of::<Held>() == 16);

impl Held {
    fn block(&self) -> Option<&[u64]> {
        match self {
            Held::Block(block) => Some(block),
            Held::Spare(_) => None,
        }
    }

    fn into_block(self) -> Option<Box<[u64]>> {
        match self {
            Held::Block(block) => Some(block),
            Held::Spare(_) => None,
        }
    }
}

#[derive(Default)]
pub(super) struct Table {
    /// Each region's pages, by the region's number. A run uses one or two regions.
    regions: Regions,
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
    /// The placed blocks, each at the index its place holds, less one. An entry moves only when
    /// the list is cut down (`compact`), with its place rewritten.
    live: Vec<Held>,
    /// The number of the spare entry of `live` used next, which holds the number of the one after
    /// it: 0 for none.
    first_spare: u32,
    /// The most entries `live` may have, in place of the most a place can number, for a test.
    #[cfg(test)]
    most_entries: Option<usize>,
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

/// Each region's pages, by the region's number. The first region made is held in the table
/// itself, not in a list, so finding a place in it reads no list first: a run's blocks are almost
/// all in one region, and every lookup, made, read or freed, began with that read.
#[derive(Default)]
struct Regions {
    first: Option<(usize, Pages)>,
    rest: Vec<(usize, Pages)>,
}

impl Regions {
    fn iter(&self) -> impl Iterator<Item = &(usize, Pages)> {
        self.first.iter().chain(&self.rest)
    }

    fn push(&mut self, region: (usize, Pages)) {
        if self.first.is_none() {
            self.first = Some(region);
        } else {
            self.rest.push(region);
        }
    }
}

/// A region's pages. The first region is looked at before any search: a run's blocks are
/// almost all in one.
fn pages_of(regions: &Regions, region: usize) -> Option<&Pages> {
    match &regions.first {
        Some((number, pages)) if *number == region => Some(pages),
        _ => regions.rest.iter().find(|(number, _)| *number == region).map(|(_, pages)| pages),
    }
}

/// `pages_of`, to change them.
fn pages_of_mut(regions: &mut Regions, region: usize) -> Option<&mut Pages> {
    match &mut regions.first {
        Some((number, pages)) if *number == region => Some(pages),
        _ => regions.rest.iter_mut().find(|(number, _)| *number == region).map(|(_, pages)| pages),
    }
}

/// Stops the program: the table's places and its list of blocks disagree. Only a bug in Fennec
/// gets here.
#[cfg(all(feature = "audit", not(test)))]
#[cold]
#[inline(never)]
fn disagrees(wrong: &str) -> ! {
    use std::io::Write as _;
    super::write_kept_output();
    let _ = writeln!(
        std::io::stderr(),
        "error: the runtime's table of blocks disagrees with itself: {wrong}\n  \
         = help: this is a bug in Fennec; please report it"
    );
    super::aot::stop_now(1)
}

/// [`disagrees`] in a test, which goes on to be caught, where a program stops.
#[cfg(test)]
fn disagrees(wrong: &str) -> ! {
    panic!("the table disagrees with itself: {wrong}");
}

/// The place for `address`, on a page that's been made, whatever it holds.
fn place_mut(regions: &mut Regions, address: usize) -> Option<&mut u32> {
    let (region, page, place) = split(address);
    pages_of_mut(regions, region)?.get_mut(page)?.as_mut()?.get_mut(place)
}

/// Where in `live` a place's number points. An empty place's 0 points past any list's end.
fn index_of(number: u32) -> usize {
    usize::try_from(number).map_or(usize::MAX, |number| number.wrapping_sub(1))
}

/// The place for `address` and the entry of `live` it points at, when that holds the block at
/// `address`.
fn found_mut<'table>(
    regions: &'table mut Regions,
    live: &'table mut [Held],
    address: usize,
) -> Option<(&'table mut u32, &'table mut Held)> {
    let place = place_mut(regions, address)?;
    let held = live.get_mut(index_of(*place))?;
    held.block().is_some_and(|block| at(block) == address).then_some((place, held))
}

/// Takes the block out of a place `found_mut` gave: its entry is the first spare one, and the
/// place empty.
fn take_found(place: &mut u32, held: &mut Held, first_spare: &mut u32) -> Option<Box<[u64]>> {
    let block = std::mem::replace(held, Held::Spare(*first_spare)).into_block();
    *first_spare = *place;
    *place = 0;
    block
}

/// Puts a block in `live`, at a spare index or else a new one, and its index in its place.
/// Gives the block back when `live` has `most` entries, for the hash map to hold.
fn hold(
    live: &mut Vec<Held>,
    first_spare: &mut u32,
    place: &mut u32,
    block: Box<[u64]>,
    most: usize,
) -> Result<(), Box<[u64]>> {
    let number = *first_spare;
    if let Some(held) = live.get_mut(index_of(number))
        && let Held::Spare(next) = *held
    {
        *held = Held::Block(block);
        *first_spare = next;
        *place = number;
        return Ok(());
    }
    if live.len() >= most {
        return Err(block);
    }
    let Ok(number) = u32::try_from(live.len() + 1) else { return Err(block) };
    live.push(Held::Block(block));
    *place = number;
    Ok(())
}

/// A region's pages, none made yet.
fn empty<T: Clone, const N: usize>() -> Option<Box<[Option<T>; N]>> {
    vec![None; N].into_boxed_slice().try_into().ok()
}

/// A page with every place empty.
fn empty_page() -> Option<Page> {
    vec![0_u32; PLACES].into_boxed_slice().try_into().ok()
}

impl Table {
    /// A table for a `shared` value's blocks.
    pub(super) fn hashed() -> Table {
        Table { hashed: true, ..Table::default() }
    }

    /// All its blocks, emptying it.
    pub(super) fn into_blocks(self) -> impl Iterator<Item = Box<[u64]>> {
        self.live.into_iter().filter_map(Held::into_block).chain(self.short.into_values())
    }

    /// The most entries `live` may have: as many as a place can number, 4,294,967,295. Past
    /// them a block goes in the hash map, which every look falls back to.
    #[cfg_attr(not(test), expect(clippy::unused_self, reason = "only a test's table sets its own"))]
    fn most_entries(&self) -> usize {
        let most = usize::try_from(u32::MAX).unwrap_or(usize::MAX);
        #[cfg(test)]
        let most = self.most_entries.unwrap_or(most);
        most
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.len
    }

    /// How many entries looking through all of it reads (`all`): a block each, but those the
    /// list has spare. Kept as it goes, so asking costs nothing.
    pub(super) fn entries_to_look_through(&self) -> usize {
        self.live.len() + self.short.len()
    }

    /// How many entries its list has room for, for counting what's cached.
    #[cfg(any(test, feature = "threads"))]
    pub(super) fn room(&self) -> usize {
        self.live.capacity()
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
        for block in self.live.iter().filter_map(Held::block).chain(self.short.values().map(|block| &**block))
        {
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
        self.live.iter().filter_map(Held::block).chain(self.short.values().map(|block| &**block))
    }

    /// How many blocks it holds that can still be freed: all but those kept for the whole run, like
    /// literals and the cases with no fields made the first time they're asked for.
    pub(super) fn freeable(&self) -> usize {
        self.freeable_where(|_| true)
    }

    /// How many of the blocks that can still be freed `which` picks.
    pub(super) fn freeable_where(&self, which: impl Fn(&[u64]) -> bool) -> usize {
        let never_freed = |block: &[u64]| {
            block
                .get(1)
                .is_some_and(|shape| *shape >> super::heap::OWNER_SHIFT == super::heap::NEVER_FREED_OWNER)
        };
        self.live
            .iter()
            .filter_map(Held::block)
            .chain(self.short.values().map(|block| &**block))
            .filter(|block| !never_freed(block) && which(block))
            .count()
    }

    fn slot(&self, address: usize) -> Option<&[u64]> {
        let (region, page, place) = split(address);
        let number = *pages_of(&self.regions, region)?.get(page)?.as_ref()?.get(place)?;
        self.live.get(index_of(number))?.block().filter(|block| at(block) == address)
    }

    pub(super) fn get(&self, address: usize) -> Option<&[u64]> {
        match self.slot(address) {
            Some(block) => Some(block),
            None if self.short.is_empty() => None,
            None => self.short.get(&address).map(AsRef::as_ref),
        }
    }

    pub(super) fn get_mut(&mut self, address: usize) -> Option<&mut [u64]> {
        let found = found_mut(&mut self.regions, &mut self.live, address).and_then(|(_, held)| match held {
            Held::Block(block) => Some(&mut **block),
            Held::Spare(_) => None,
        });
        match found {
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
            Some(place) => {
                if *place != 0 {
                    two_owners(address);
                }
                let number = self.first_spare;
                if let Some(held) = self.live.get_mut(index_of(number))
                    && let Held::Spare(next) = *held
                {
                    *held = Held::Block(block);
                    self.first_spare = next;
                    *place = number;
                    self.len += 1;
                    return;
                }
                self.insert_at_the_end(block);
            }
            None => self.insert_in_new_page(block),
        }
    }

    /// `insert`, for a block on a page that's made, with no index spare: the list grows.
    #[cold]
    #[inline(never)]
    fn insert_at_the_end(&mut self, block: Box<[u64]>) {
        let address = at(&block);
        let most = self.most_entries();
        let Some(place) = place_mut(&mut self.regions, address) else {
            return self.insert_in_new_page(block);
        };
        match hold(&mut self.live, &mut self.first_spare, place, block, most) {
            Ok(()) => self.len += 1,
            Err(block) => self.insert_short(address, block),
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
        let most = self.most_entries();
        match place_mut(&mut self.regions, address) {
            Some(place) => {
                // One taken over before its page was made is in the hash map, not the place.
                if *place != 0 || (!self.short.is_empty() && self.short.contains_key(&address)) {
                    two_owners(address);
                }
                match hold(&mut self.live, &mut self.first_spare, place, block, most) {
                    Ok(()) => self.len += 1,
                    Err(block) => self.insert_short(address, block),
                }
            }
            None => self.insert_short(address, block),
        }
    }

    /// `insert`, for a block on a page with no block yet.
    #[cold]
    #[inline(never)]
    fn insert_in_new_page(&mut self, block: Box<[u64]>) {
        let address = at(&block);
        let most = self.most_entries();
        let (region, at_page, place) = split(address);
        if pages_of(&self.regions, region).is_none() {
            let Some(pages) = empty() else { return };
            self.regions.push((region, pages));
        }
        let Some(page) = pages_of_mut(&mut self.regions, region).and_then(|pages| pages.get_mut(at_page))
        else {
            return;
        };
        if page.is_none() {
            *page = empty_page();
            self.made += 1;
            self.made_pages.push((region, at_page));
        }
        if let Some(place) = page.as_mut().and_then(|page| page.get_mut(place)) {
            if *place != 0 {
                two_owners(address);
            }
            match hold(&mut self.live, &mut self.first_spare, place, block, most) {
                Ok(()) => self.len += 1,
                Err(block) => self.insert_short(address, block),
            }
        }
        if self.made >= self.kept.max(PAGES_BEFORE_LOOKING) {
            self.give_back_empty_pages();
        }
    }

    /// Looks for empty pages to give back, as `insert_in_new_page` does, for a worker going to sleep
    /// with nothing to do: once it's made a few pages, not 64, or once its list is mostly holes.
    /// Each look still costs about a page's reading for each page made since the last.
    ///
    /// A burst's blocks can all go with no page made after, as a server's worker takes a burst,
    /// lets it go and waits. No look would come by pages made, so the burst's list and pages would
    /// stay: a list of 200,000 entries and 100 pages, 5.6 MB, for one block. After a look the
    /// list holds what's live, so the next comes only once it's mostly holes again.
    #[cfg(any(test, feature = "threads"))]
    pub(super) fn look_while_idle(&mut self) {
        if self.made >= self.kept.max(PAGES_BEFORE_LOOKING_IDLE) || self.mostly_holes() {
            self.give_back_empty_pages();
        }
    }

    /// Whether fewer than half the list's entries hold a block, past the few a short list keeps
    /// spare (`ENTRIES_KEPT_SPARE`): when `cut_down` moves its blocks down.
    fn mostly_holes(&self) -> bool {
        let held = self.len.saturating_sub(self.short.len());
        self.live.len() >= 2 * held + ENTRIES_KEPT_SPARE
    }

    /// Lets go of every page with no block on it, reading only the pages it has, and cuts `live`
    /// down to what it holds.
    #[cold]
    #[inline(never)]
    fn give_back_empty_pages(&mut self) {
        let regions = &mut self.regions;
        self.made_pages.retain(|&(region, at_page)| {
            let Some(page) = pages_of_mut(regions, region).and_then(|pages| pages.get_mut(at_page)) else {
                return false;
            };
            if page.as_ref().is_none_or(|places| places.iter().all(|place| *place == 0)) {
                *page = None;
                return false;
            }
            true
        });
        self.kept = self.made_pages.len();
        self.made = 0;
        self.cut_down();
    }

    /// Cuts `live` down to what it holds. A new block takes the index freed last, wherever that
    /// is, so after a burst is freed the list's end stays in use, and popping its spare end alone
    /// almost never shortens it: a burst of 200,000 blocks, nearly all freed, left it 200,000
    /// entries long. So where fewer than half its entries hold a block, the blocks at its end move
    /// down into its holes. Its room is let go of too, which popping keeps.
    fn cut_down(&mut self) {
        if self.mostly_holes() {
            self.compact(self.len.saturating_sub(self.short.len()));
        } else if matches!(self.live.last(), Some(Held::Spare(_))) {
            while matches!(self.live.last(), Some(Held::Spare(_))) {
                self.live.pop();
            }
            self.chain_spares();
        }
        let room = self.live.len() + self.live.len() / 4;
        if self.live.capacity() >= 2 * room + ENTRIES_KEPT_SPARE {
            self.live.shrink_to(room);
        }
    }

    /// Moves the blocks past the first `held` entries down into the holes below, each with its
    /// place rewritten: two stores a block. An index is only ever in its block's place, so no
    /// look can find a stale one.
    fn compact(&mut self, held: usize) {
        let mut hole = 0;
        while self.live.len() > held {
            let Some(Held::Block(block)) = self.live.pop() else { continue };
            while matches!(self.live.get(hole), Some(Held::Block(_))) {
                hole += 1;
            }
            let address = at(&block);
            let place = place_mut(&mut self.regions, address);
            match (place, self.live.get_mut(hole), u32::try_from(hole + 1)) {
                (Some(place), Some(entry), Ok(number)) => {
                    *place = number;
                    *entry = Held::Block(block);
                }
                // There's a hole below for each block past `held`, and every placed block's page is
                // made, so this can't happen. Were it to, the block is kept in the hash map, where
                // a look goes once its place points past the list or at another block.
                _ => {
                    self.short.insert(address, block);
                }
            }
        }
        self.chain_spares();
    }

    /// Has each spare entry hold the next again, lowest first, after some have gone or moved.
    fn chain_spares(&mut self) {
        let mut first = 0;
        for (index, held) in self.live.iter_mut().enumerate().rev() {
            if let Held::Spare(next) = held {
                *next = first;
                first = u32::try_from(index + 1).unwrap_or(0);
            }
        }
        self.first_spare = first;
    }

    /// Takes one owner off the block at `address`, and takes the block out when that was its last,
    /// in one look.
    #[expect(
        clippy::inline_always,
        reason = "every free goes through it, and it stopped being inlined once it reads the list too"
    )]
    #[inline(always)]
    pub(super) fn release_one(&mut self, address: usize) -> Released {
        let Some((place, held)) = found_mut(&mut self.regions, &mut self.live, address) else {
            return self.release_short(address);
        };
        let Held::Block(block) = held else { return Released::Missing };
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
        take_found(place, held, &mut self.first_spare).map_or(Released::Missing, Released::Gone)
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
        let taken = match found_mut(&mut self.regions, &mut self.live, address) {
            Some((place, held)) => {
                if held.block().is_some_and(wanted) {
                    take_found(place, held, &mut self.first_spare)
                } else {
                    None
                }
            }
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
        let removed = match found_mut(&mut self.regions, &mut self.live, address) {
            Some((place, held)) => take_found(place, held, &mut self.first_spare),
            None => self.short.remove(&address),
        };
        if removed.is_some() {
            self.len -= 1;
        }
        removed
    }

    /// How many entries the list has, how many of them are spare, and its room.
    #[cfg(test)]
    fn entries(&self) -> (usize, usize, usize) {
        (self.live.len(), self.spare_chain().map_or(usize::MAX, |chain| chain.len()), self.live.capacity())
    }

    /// The spare entries' indices, in the order they'd be used, or what's wrong with them.
    #[cfg(any(test, feature = "audit"))]
    fn spare_chain(&self) -> Result<Vec<usize>, String> {
        let mut chain = Vec::new();
        let mut number = self.first_spare;
        while number != 0 {
            let index = index_of(number);
            match self.live.get(index) {
                Some(Held::Spare(next)) => {
                    chain.push(index);
                    number = *next;
                }
                Some(Held::Block(_)) => return Err(format!("spare index {index} holds a block")),
                None => return Err(format!("spare index {index} is past the end of `live`")),
            }
            if chain.len() > self.live.len() {
                return Err("the spare entries go round in a ring".to_owned());
            }
        }
        Ok(chain)
    }

    /// Stops the program where a place and the list disagree, for the heap's audit: a stale place
    /// is a block that's never found, which nothing else would say.
    #[cfg(any(test, feature = "audit"))]
    pub(super) fn check_agrees(&self) {
        if let Err(wrong) = self.agrees() {
            disagrees(&wrong);
        }
    }

    /// Whether every place and every entry of `live` agree, for a test to ask after any step: each place that isn't empty points at the block whose address is that place's, each
    /// block in `live` is pointed at by its own place, each spare index is an empty entry named
    /// once, and `len` counts what's held.
    #[cfg(any(test, feature = "audit"))]
    fn agrees(&self) -> Result<(), String> {
        let mut pointed = 0_usize;
        for (region, pages) in self.regions.iter() {
            for (page, places) in pages.iter().enumerate().filter_map(|(n, page)| Some((n, page.as_ref()?))) {
                for (place, number) in places.iter().enumerate().filter(|(_, number)| **number != 0) {
                    let Some(Held::Block(block)) = self.live.get(index_of(*number)) else {
                        return Err(format!("a place points at index {number}, which holds no block"));
                    };
                    if split(at(block)) != (*region, page, place) {
                        return Err(format!(
                            "a place points at the block at {:#x}, another place's",
                            at(block)
                        ));
                    }
                    pointed += 1;
                }
            }
        }
        let held = self.live.iter().filter(|held| held.block().is_some()).count();
        if pointed != held {
            return Err(format!("{pointed} places point at blocks, and `live` holds {held}"));
        }
        let mut spare = self.spare_chain()?;
        spare.sort_unstable();
        if spare.windows(2).any(|pair| pair.first() == pair.get(1)) {
            return Err("an index is spare twice".to_owned());
        }
        if spare.len() != self.live.len() - held {
            return Err(format!("{} empty entries, {} spare indices", self.live.len() - held, spare.len()));
        }
        if self.len != held + self.short.len() {
            return Err(format!("len is {}, and it holds {}", self.len, held + self.short.len()));
        }
        Ok(())
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

    /// A table of few blocks, looked at as its worker goes idle, holds few empty pages: it looks for
    /// them once it's made a few, so a server's tables, making pages slowly as their blocks wander,
    /// give them back within minutes rather than the half hour 64 took.
    #[test]
    fn a_table_of_few_blocks_holds_few_empty_pages_when_looked_at_idle() {
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
            table.look_while_idle();
            assert!(
                table.pages() <= 2 + PAGES_BEFORE_LOOKING_IDLE,
                "{} pages kept for one block",
                table.pages()
            );
        }
        assert_eq!(table.len(), 1);
    }

    /// A worker that takes a burst, lets it all go but one block, and waits, makes no page after:
    /// its first idle look still gives back the burst's pages and cuts its list down, as a busy
    /// table's would. Before, the list kept 200,000 entries and the pages stayed, 5.6 MB.
    #[test]
    fn a_quiet_table_lets_its_burst_go_at_its_first_idle_look() {
        let mut table = Table::default();
        let burst: Vec<Box<[u64]>> = (0..200_000_u64).map(|n| vec![1, n, 7, 8].into()).collect();
        let addresses: Vec<usize> = burst.iter().map(|block| at(block)).collect();
        for block in burst {
            table.insert(block);
        }
        let Some((&kept, gone)) = addresses.split_last() else { return };
        let mut elsewhere = Vec::new();
        for address in gone {
            elsewhere.extend(table.remove(*address));
        }
        let pages = table.pages();
        assert!(pages > 2 + PAGES_BEFORE_LOOKING_IDLE, "the burst took only {pages} pages");
        table.look_while_idle();
        table.agrees().unwrap_or_else(|wrong| panic!("{wrong}"));
        let (entries, _, room) = table.entries();
        assert!(entries <= 3, "{entries} entries for one block after an idle look");
        assert!(room <= 2 * ENTRIES_KEPT_SPARE, "room for {room} entries for one block");
        assert!(table.pages() <= 2, "{} of {pages} pages kept for one block", table.pages());
        assert_eq!(table.get(kept).map(|block| block[1]), Some(199_999));
        // Once the list holds what's live, an idle look doesn't come again for nothing.
        assert!(!table.mostly_holes());
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

    /// A sequence of numbers from a seed, the same on every machine.
    struct Seeded(u64);

    impl Seeded {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 33
        }

        fn below(&mut self, n: usize) -> usize {
            usize::try_from(self.next()).unwrap() % n
        }
    }

    /// What the model knows of a block: its tag, its owner count and its length in words.
    type Known = (u64, u64, usize);

    fn check_against(table: &Table, model: &HashMap<usize, Known>, address: usize) {
        match (table.get(address), model.get(&address)) {
            (Some(block), Some((tag, count, len))) => {
                assert_eq!(at(block), address, "found a block at another address");
                assert_eq!(block.len(), *len);
                assert_eq!(block.first().copied(), Some(*count), "its count is another block's");
                if *len > 1 {
                    assert_eq!(block.get(1).copied(), Some(*tag), "found another block's words");
                }
            }
            (None, None) => {}
            (found, known) => {
                panic!("at {address:#x} the table has {:?}, the model {known:?}", found.map(<[u64]>::len))
            }
        }
    }

    /// Blocks made here and taken over from another thread, in any order, on pages made before
    /// and after them and given back under them, released one owner at a time, removed and taken:
    /// after every step the table holds exactly what a plain map of addresses says, each block
    /// found by its own address with its own words, and none at an address that's gone. Every so
    /// often every place and every entry of the dense list are checked against each other.
    #[test]
    #[expect(clippy::too_many_lines, reason = "one model's steps, each beside the others")]
    fn the_table_agrees_with_a_model_through_random_work() {
        for seed in [1_u64, 2042, 53, 64, 99_999, 20_261_118] {
            let mut random = Seeded(seed);
            let mut table = Table::default();
            let mut model: HashMap<usize, Known> = HashMap::new();
            let mut live: Vec<usize> = Vec::new();
            let mut gone: Vec<usize> = Vec::new();
            let (mut hashed_under_pages, mut misses, mut most_pages) = (0_usize, 0_usize, 0_usize);
            let (mut reused, mut shrank) = (0_usize, 0_usize);
            let page_of = |address: usize| address >> PAGE_BITS;
            for step in 0..120_000_u64 {
                let tag = step + 1;
                let entries = table.entries().0;
                match random.below(100) {
                    // Make one here, or take one over: short, a few words, 64 words, or a page's worth.
                    0..=44 => {
                        let len = match random.below(40) {
                            0 => 1,
                            1 => 1 << 13,
                            2 | 3 => 64,
                            _ => 2 + random.below(6),
                        };
                        let count = 1 + random.next() % 3;
                        let mut words = vec![tag; len];
                        words[0] = count;
                        let block: Box<[u64]> = words.into();
                        let address = at(&block);
                        assert!(!model.contains_key(&address), "the allocator gave a live address twice");
                        // Early on, and in bursts after, most are taken over: before their pages are made.
                        let adopted = if step < 4_000 || (step / 3_000) % 4 == 3 {
                            random.below(10) < 8
                        } else {
                            random.below(10) < 3
                        };
                        let spare = table.entries().1;
                        if adopted {
                            table.insert_adopted(block);
                        } else {
                            table.insert(block);
                        }
                        // An index that's spare is used before the list grows.
                        if table.entries().0 > entries {
                            assert_eq!(spare, 0, "the list grew with an index spare");
                        } else if table.entries().1 < spare {
                            reused += 1;
                        }
                        model.insert(address, (tag, count, len));
                        live.push(address);
                    }
                    // Take an owner off one, which takes it out at its last.
                    45..=74 if !live.is_empty() => {
                        let which = random.below(live.len());
                        let address = live[which];
                        let (_, count, len) = model[&address];
                        match table.release_one(address) {
                            Released::Held => {
                                assert!(count > 1, "held with one owner");
                                model.get_mut(&address).unwrap().1 = count - 1;
                            }
                            Released::Gone(block) => {
                                assert_eq!(count, 1, "gone with owners left");
                                assert_eq!((at(&block), block.len()), (address, len));
                                model.remove(&address);
                                live.swap_remove(which);
                                gone.push(address);
                            }
                            Released::Missing => {
                                panic!("a live block at {address:#x} wasn't found to release")
                            }
                        }
                    }
                    // Take one out whole, by `remove` or `take_if`.
                    75..=84 if !live.is_empty() => {
                        let which = random.below(live.len());
                        let address = live[which];
                        let (_, _, len) = model[&address];
                        let taken = if random.below(2) == 0 {
                            table.remove(address)
                        } else {
                            assert!(table.take_if(address, |_| false).is_none());
                            check_against(&table, &model, address);
                            table.take_if(address, |block| block.len() == len)
                        };
                        let block = taken.expect("a live block wasn't found to take out");
                        assert_eq!((at(&block), block.len()), (address, len));
                        model.remove(&address);
                        live.swap_remove(which);
                        gone.push(address);
                    }
                    // Ask at an address that's gone, unless something's there again, and inside a block.
                    85..=94 if !gone.is_empty() => {
                        let address = gone[random.below(gone.len())];
                        check_against(&table, &model, address);
                        if !model.contains_key(&address) {
                            let before = table.len();
                            assert!(matches!(table.release_one(address), Released::Missing));
                            assert!(table.remove_any(address).is_none());
                            assert!(table.take_if(address, |_| true).is_none());
                            assert!(table.get_mut(address).is_none());
                            assert_eq!(table.len(), before, "a miss changed the table");
                            misses += 1;
                        }
                        if !model.contains_key(&(address + 8)) {
                            assert!(table.get(address + 8).is_none(), "found a block from inside it");
                        }
                    }
                    // Change one in place.
                    _ if !live.is_empty() => {
                        let address = live[random.below(live.len())];
                        let known = model.get_mut(&address).unwrap();
                        if known.2 > 1 {
                            known.0 = tag;
                            table.get_mut(address).unwrap()[1] = tag;
                        }
                    }
                    _ => {}
                }
                assert_eq!(table.len(), model.len());
                shrank += usize::from(table.entries().0 < entries);
                if gone.len() > 4_096 {
                    gone.drain(..2_048);
                }
                // Now and then pages are given back whether or not enough were made, so the list is
                // cut down, its blocks moved, in the middle of the work.
                if step % 7_000 == 6_999 || step % 30_000 == 29_999 {
                    table.give_back_empty_pages();
                    table.agrees().unwrap_or_else(|wrong| panic!("seed {seed}, step {step}: {wrong}"));
                    for address in &live {
                        check_against(&table, &model, *address);
                    }
                }
                // Often, every place against every entry.
                if step % 500 == 0 {
                    table.agrees().unwrap_or_else(|wrong| panic!("seed {seed}, step {step}: {wrong}"));
                }
                // Now and then, everything: each block by its address, and the table's own list.
                if step % 2_500 == 0 {
                    for address in &live {
                        check_against(&table, &model, *address);
                    }
                    assert_eq!(table.all().count(), model.len());
                    let mut listed = std::collections::HashSet::new();
                    for block in table.all() {
                        assert!(model.contains_key(&at(block)), "the table lists a block that's gone");
                        assert!(listed.insert(at(block)), "the table lists a block twice");
                    }
                    assert_eq!(table.freeable(), model.len());
                    assert_eq!(table.sizes().0.values().sum::<usize>(), model.len());
                    let made: std::collections::HashSet<usize> = table
                        .regions
                        .iter()
                        .flat_map(|(region, pages)| {
                            pages
                                .iter()
                                .enumerate()
                                .filter(|(_, page)| page.is_some())
                                .map(move |(page, _)| (*region, page))
                        })
                        .map(|(region, page)| (region << (REGION_BITS - PAGE_BITS)) | page)
                        .collect();
                    hashed_under_pages += table
                        .short
                        .iter()
                        .filter(|(address, block)| {
                            block.len() >= PLACED_WORDS && made.contains(&page_of(**address))
                        })
                        .count();
                    most_pages = most_pages.max(table.pages());
                }
                // Now and then, most of what's live goes, so pages empty and are given back.
                if step % 30_000 == 29_999 {
                    let keep = live.len() / 8;
                    while live.len() > keep {
                        let which = random.below(live.len());
                        let address = live.swap_remove(which);
                        assert!(table.remove(address).is_some());
                        model.remove(&address);
                        gone.push(address);
                    }
                    table.agrees().unwrap_or_else(|wrong| panic!("seed {seed}, step {step}: {wrong}"));
                    // With most of it gone, giving pages back cuts the list down.
                    let before = table.entries().0;
                    table.give_back_empty_pages();
                    shrank += usize::from(table.entries().0 < before);
                    table.agrees().unwrap_or_else(|wrong| panic!("seed {seed}, step {step}: {wrong}"));
                    for address in &live {
                        check_against(&table, &model, *address);
                    }
                }
            }
            assert!(hashed_under_pages > 0, "no hashed block was ever under a page that was made");
            assert!(misses > 1_000);
            assert!(reused > 1_000, "indices were hardly used again");
            assert!(shrank > 0, "the list was never cut down");
            // What's left comes out through `into_blocks`, each block once.
            let left: std::collections::HashSet<usize> =
                table.into_blocks().map(|block| at(&block)).collect();
            assert_eq!(left.len(), model.len());
            assert!(live.iter().all(|address| left.contains(address)));
        }
    }

    /// Indices are used again after frees, and a block is never found through an index another
    /// block now holds.
    #[test]
    fn indices_are_used_again_and_never_find_another_block() {
        let mut table = Table::default();
        let first: Vec<Box<[u64]>> = (0..5_000_u64).map(|n| vec![1, n, 7].into()).collect();
        let first_at: Vec<usize> = first.iter().map(|block| at(block)).collect();
        for block in first {
            table.insert(block);
        }
        assert_eq!((table.entries().0, table.entries().1), (5_000, 0));
        // Kept, so the second lot's addresses are all new.
        let kept: Vec<Box<[u64]>> = first_at.iter().map(|address| table.remove(*address).unwrap()).collect();
        assert_eq!((table.entries().0, table.entries().1, table.len()), (5_000, 5_000, 0));
        let second: Vec<Box<[u64]>> = (0..5_000_u64).map(|n| vec![1, n + 10_000, 9].into()).collect();
        let second_at: Vec<usize> = second.iter().map(|block| at(block)).collect();
        for block in second {
            table.insert(block);
        }
        assert_eq!((table.entries().0, table.entries().1), (5_000, 0), "every index was used again");
        for address in &first_at {
            assert!(table.get(*address).is_none(), "an address that's gone finds the block at its old index");
            assert!(matches!(table.release_one(*address), Released::Missing));
        }
        for (n, address) in (0..5_000_u64).zip(&second_at) {
            assert_eq!(table.get(*address).map(|block| block[1]), Some(n + 10_000));
        }
        table.agrees().unwrap();
        drop(kept);
    }

    /// After a burst is freed, nearly all of it, the list comes down to about what it still holds
    /// once pages are given back, and lets its room go, wherever the blocks left are. A new block
    /// takes the index freed last, so without moving blocks down the list stayed 200,000 entries
    /// long, 3.1 MB, whatever it held.
    #[test]
    fn the_list_comes_down_after_a_burst_wherever_its_blocks_are_left() {
        for kept_every in [0_usize, 100, usize::MAX] {
            let keep = |n: usize| match kept_every {
                0 => n == 199_999,
                usize::MAX => false,
                every => n.is_multiple_of(every),
            };
            let mut table = Table::default();
            let burst: Vec<Box<[u64]>> = (0..200_000_u64).map(|n| vec![1, n, 7, 8].into()).collect();
            let addresses: Vec<usize> = burst.iter().map(|block| at(block)).collect();
            for block in burst {
                table.insert(block);
            }
            assert_eq!(table.entries().0, 200_000);
            let mut elsewhere = Vec::new();
            for (n, address) in addresses.iter().enumerate() {
                if !keep(n) {
                    elsewhere.extend(table.remove(*address));
                }
            }
            let held = table.len();
            // Blocks go on being made elsewhere, a few at a time, as a server's do: pages are made
            // and the empty ones given back.
            let mut wandered = Vec::new();
            for round in 0..400_u64 {
                let blocks: Vec<Box<[u64]>> = (0..3).map(|n| vec![round + n; 1 << 13].into()).collect();
                let these: Vec<usize> = blocks.iter().map(|block| at(block)).collect();
                for block in blocks {
                    table.insert(block);
                }
                wandered.extend(these.into_iter().filter_map(|address| table.remove(address)));
            }
            table.agrees().unwrap_or_else(|wrong| panic!("keeping every {kept_every}: {wrong}"));
            let (entries, _, room) = table.entries();
            assert!(entries <= held + 3, "{entries} entries for {held} blocks, keeping every {kept_every}");
            assert!(room <= 4 * held + 2 * ENTRIES_KEPT_SPARE, "room for {room} entries for {held} blocks");
            for (n, address) in addresses.iter().enumerate() {
                let tag = u64::try_from(n).unwrap_or(u64::MAX);
                assert_eq!(table.get(*address).map(|block| block[1]), keep(n).then_some(tag));
            }
        }
    }

    /// A list of few entries isn't moved about for little: under `ENTRIES_KEPT_SPARE` past twice
    /// what it holds, only its spare end goes.
    #[test]
    fn a_short_list_only_loses_its_spare_end() {
        let mut table = Table::default();
        let blocks: Vec<Box<[u64]>> = (0..600_u64).map(|n| vec![1, n, 7].into()).collect();
        let addresses: Vec<usize> = blocks.iter().map(|block| at(block)).collect();
        for block in blocks {
            table.insert(block);
        }
        let mut elsewhere = Vec::new();
        // The first and the last 100 go: the end is spare, the start a hole.
        for address in addresses.iter().take(100).chain(addresses.iter().skip(500)) {
            elsewhere.extend(table.remove(*address));
        }
        table.give_back_empty_pages();
        table.agrees().unwrap();
        assert_eq!((table.entries().0, table.entries().1), (500, 100), "the end goes and the hole stays");
        for (n, address) in addresses.iter().enumerate() {
            assert_eq!(table.get(*address).is_some(), (100..500).contains(&n));
        }
    }

    /// Past the most entries a place can number, a block goes in the hash map, and is found,
    /// released and taken out from there as any other is.
    #[test]
    fn past_the_most_entries_blocks_go_in_the_hash_map() {
        let mut table = Table { most_entries: Some(4), ..Table::default() };
        let blocks: Vec<Box<[u64]>> = (0..10_u64).map(|n| vec![2, n, 7].into()).collect();
        let addresses: Vec<usize> = blocks.iter().map(|block| at(block)).collect();
        for block in blocks {
            table.insert(block);
        }
        assert_eq!((table.entries().0, table.short.len(), table.len()), (4, 6, 10));
        table.agrees().unwrap();
        for (n, address) in (0..10_u64).zip(&addresses) {
            assert_eq!(table.get(*address).map(|block| block[1]), Some(n));
            assert!(matches!(table.release_one(*address), Released::Held));
        }
        let mut taken = Vec::new();
        for address in &addresses {
            taken.extend(table.remove(*address));
        }
        assert_eq!((taken.len(), table.len()), (10, 0));
        table.agrees().unwrap();
    }

    /// A lent table's lookups match the owner's, from four threads at once.
    #[test]
    fn a_lent_table_is_read_as_its_owner_reads_it() {
        let mut table = Table::default();
        let blocks: Vec<Box<[u64]>> = (0..20_000_u64).map(|n| vec![1, n, 7].into()).collect();
        let addresses: Vec<usize> = blocks.iter().map(|block| at(block)).collect();
        for (n, block) in blocks.into_iter().enumerate() {
            if n % 5 == 0 {
                table.insert_adopted(block);
            } else {
                table.insert(block);
            }
        }
        // Holes, and indices used again, before it's lent.
        let mut gone = Vec::new();
        for address in addresses.iter().step_by(3) {
            gone.push((*address, table.remove(*address).unwrap()));
        }
        let more: Vec<Box<[u64]>> = (0..3_000_u64).map(|n| vec![1, n + 100_000, 7].into()).collect();
        let more_at: Vec<usize> = more.iter().map(|block| at(block)).collect();
        for block in more {
            table.insert(block);
        }
        let owner: Vec<Option<u64>> = addresses
            .iter()
            .chain(&more_at)
            .map(|address| table.get(*address).map(|block| block[1]))
            .collect();
        let lent = std::sync::Arc::new(table);
        let asked: Vec<usize> = addresses.iter().chain(&more_at).copied().collect();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    for _ in 0..20 {
                        let seen: Vec<Option<u64>> =
                            asked.iter().map(|address| lent.get(*address).map(|block| block[1])).collect();
                        assert_eq!(seen, owner);
                        assert_eq!(lent.all().count(), lent.len());
                    }
                });
            }
        });
        let table = std::sync::Arc::try_unwrap(lent).ok().unwrap();
        table.agrees().unwrap();
        drop(gone);
    }
}
