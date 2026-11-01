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

/// The shortest block, in words, that has a page's place to itself.
const PLACED_WORDS: usize = 2;

type Page = Box<[Option<Box<[u64]>>]>;

#[derive(Default)]
pub(super) struct Table {
    /// Each region's pages, by the region's number. A run uses one or two regions.
    regions: Vec<(usize, Box<[Option<Page>]>)>,
    /// Blocks too short to have a place of their own.
    short: HashMap<usize, Box<[u64]>, BuildHasherDefault<AddressHasher>>,
    len: usize,
}

/// An address's region, page in it, and place in the page.
fn split(address: usize) -> (usize, usize, usize) {
    (address >> REGION_BITS, (address >> PAGE_BITS) % PAGES, (address >> PLACE_BITS) % PLACES)
}

fn at(block: &[u64]) -> usize {
    block.as_ptr().addr()
}

/// The place holding the block at `address`, when one does.
fn slot_mut(regions: &mut [(usize, Box<[Option<Page>]>)], address: usize) -> Option<&mut Option<Box<[u64]>>> {
    let (region, page, place) = split(address);
    let (_, pages) = regions.iter_mut().find(|(number, _)| *number == region)?;
    let slot = pages.get_mut(page)?.as_mut()?.get_mut(place)?;
    slot.as_ref().is_some_and(|block| at(block) == address).then_some(slot)
}

impl Table {
    pub(super) fn len(&self) -> usize {
        self.len
    }

    fn slot(&self, address: usize) -> Option<&Box<[u64]>> {
        let (region, page, place) = split(address);
        let (_, pages) = self.regions.iter().find(|(number, _)| *number == region)?;
        pages.get(page)?.as_ref()?.get(place)?.as_ref().filter(|block| at(block) == address)
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
            None => self.short.get_mut(&address).map(AsMut::as_mut),
        }
    }

    /// Adds a block at its own address, replacing one already there.
    pub(super) fn insert(&mut self, block: Box<[u64]>) {
        let address = at(&block);
        if block.len() < PLACED_WORDS {
            if self.short.insert(address, block).is_none() {
                self.len += 1;
            }
            return;
        }
        let (region, page, place) = split(address);
        let pages = match self.regions.iter().position(|(number, _)| *number == region) {
            Some(found) => self.regions.get_mut(found),
            None => {
                self.regions.push((region, std::iter::repeat_with(|| None).take(PAGES).collect()));
                self.regions.last_mut()
            }
        };
        let Some(page) = pages.and_then(|(_, pages)| pages.get_mut(page)) else {
            return;
        };
        let page = page.get_or_insert_with(|| std::iter::repeat_with(|| None).take(PLACES).collect());
        if let Some(slot) = page.get_mut(place)
            && slot.replace(block).is_none()
        {
            self.len += 1;
        }
    }

    pub(super) fn remove(&mut self, address: usize) -> Option<Box<[u64]>> {
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
