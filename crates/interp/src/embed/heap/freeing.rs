//! Freeing blocks: a block whose last owner let it go, then what it held that had no other
//! owner, all the way down. Most blocks are small and are freed one at a time, so the paths
//! for those are kept lean and everything else is out of their way.

use std::collections::HashMap;
use std::hash::BuildHasherDefault;

use super::{CELL_SHAPE, FINISHED, GENERATOR_PC, HEADER_WORDS, Heap, children_into, is_text, shape_word};
use crate::embed::plans::{Holds, Plan};
use crate::embed::{State, with};
use crate::value::Value;

/// Frees a block whose last owner let it go, and releases its boxed fields, freeing those
/// that had no other owner, all the way down.
///
/// Program code only runs here for a generator let go partway, by the function its shape was
/// given with `set_abandon`. So freeing a block whose type holds no such generator and no
/// `del` runs no program code.
pub extern "C" fn free(address: i64) {
    let abandoning = with(|state| {
        // What other threads let go of last waits for this one, which made it, to free it.
        if crate::embed::threads::any_waiting() {
            free_waiting(state);
        }
        let address = u64::try_from(address).unwrap_or(0);
        free_block(state, address);
        !state.heap.abandoning.is_empty()
    });
    if abandoning {
        run_abandons();
    }
}

/// Frees what other threads let go of last, which waits for this one, which made it.
#[cold]
#[inline(never)]
fn free_waiting(state: &mut State) {
    for waiting in crate::embed::threads::waiting() {
        free_block(state, waiting);
    }
}

/// `free`, for a block native code knows is a string's. A string's block holds no
/// addresses, so freeing it needs none of the looking `free` does. A block that isn't a
/// string's is freed as `free` would.
pub extern "C" fn free_text(address: i64) {
    let freed = with(|state| {
        let at = usize::try_from(address).ok()?;
        state.heap.forget_place(u64::try_from(at).unwrap_or(0));
        let block = state.heap.blocks.remove(at)?;
        if !is_text(state, &block) {
            state.heap.blocks.insert(block);
            return None;
        }
        let texts = state.heap.texts.get_mut();
        if !texts.is_empty() {
            texts.remove(&at);
        }
        state.heap.recycle(block);
        Some(())
    });
    if freed.is_none() {
        free(address);
    }
}

/// Says what closes what's open in a generator with this shape that's let go partway. Only
/// shapes given here are treated specially when they're freed.
pub extern "C" fn set_abandon(shape: i64, abandon: extern "C" fn(i64)) {
    with(|state| {
        state.tables.abandons.insert(u64::try_from(shape).unwrap_or(u64::MAX), abandon);
    });
}

/// Runs what closes the generators let go of, each only once nothing here is borrowed, since
/// it's program code calling back in. A failure then ends the program on the spot: whatever
/// released the generator isn't expecting one.
pub(in crate::embed) fn run_abandons() {
    while let Some((address, abandon)) = with(next_abandoned) {
        // It reads where the generator stopped to know what's open.
        abandon(i64::try_from(address).unwrap_or(0));
        if crate::embed::failed() {
            crate::embed::aot::stop_for_failure();
        }
        with(|state| {
            if let Some(pc) = state.heap.block_mut(address).and_then(|block| block.get_mut(GENERATOR_PC)) {
                *pc = FINISHED;
            }
            release(state, address);
        });
    }
}

/// The next generator to close.
fn next_abandoned(state: &mut State) -> Option<(u64, extern "C" fn(i64))> {
    let address = state.heap.abandoning.pop()?;
    let shape = shape_word(state.heap.block(address)?)?;
    Some((address, *state.tables.abandons.get(&shape)?))
}

/// Whether a block being freed is a generator stopped at a `yield` with something to close.
fn is_abandoned(state: &State, block: &[u64]) -> bool {
    !state.tables.abandons.is_empty()
        && shape_word(block).is_some_and(|shape| state.tables.abandons.contains_key(&shape))
        && block.get(GENERATOR_PC).is_some_and(|pc| i64::from_ne_bytes(pc.to_ne_bytes()) > 0)
}

/// Frees a block and releases its boxed fields, freeing those with no other owner, all the
/// way down.
fn free_block(state: &mut State, address: u64) {
    let mut pending = std::mem::take(&mut state.heap.pending);
    pending.push(address);
    while let Some(address) = pending.pop() {
        if crate::embed::threads::held_elsewhere(address) {
            continue;
        }
        // Only a string has a place kept in it, and that's forgotten below, once it's known to be one.
        let Some(block) = usize::try_from(address).ok().and_then(|at| state.heap.take_block(at)) else {
            // A `shared` value's handle: its count is the runtime's, wherever its block is.
            if crate::embed::guarded::is_handle(address) {
                crate::embed::guarded::release_handle(state, address);
            }
            continue;
        };
        if super::is_handle_block(&block) {
            state.heap.blocks.insert(block);
            crate::embed::guarded::release_handle(state, address);
            continue;
        }
        let shape = shape_word(&block).unwrap_or(u64::MAX);
        debug_assert!(
            shape == state.tables.str_shape
                || usize::try_from(address).is_ok_and(|at| state.heap.cursor().address != at),
            "a place was kept in something that isn't a string"
        );
        if is_abandoned(state, &block) {
            keep_abandoned(state, address, block);
            continue;
        }
        if shape == state.tables.str_shape {
            state.heap.forget_place(address);
            if !state.heap.texts.get_mut().is_empty() {
                forget_text(state, address);
            }
        } else if shape >= state.tables.str_shape {
            // A list of numbers, the most usual kind, has nothing to look through.
            let number = crate::embed::lists::element_number(state, &block);
            if !number.is_some_and(|number| state.heap.items_hold_nothing(number)) {
                free_unplanned(state, &block, &mut pending);
            }
        } else if !release_planned(&mut state.heap, &block, &mut pending) {
            free_unplanned(state, &block, &mut pending);
        }
        state.heap.recycle(block);
    }
    state.heap.pending = pending;
}

/// Keeps a generator let go partway until what closes it has run, which holds it as its one
/// owner.
#[cold]
#[inline(never)]
fn keep_abandoned(state: &mut State, address: u64, mut block: Box<[u64]>) {
    if let Some(count) = block.first_mut() {
        *count = 1;
    }
    state.heap.blocks.insert(block);
    state.heap.abandoning.push(address);
}

/// Drops the interpreter's copy of a freed string's text.
#[inline(never)]
fn forget_text(state: &mut State, address: u64) {
    if let Ok(at) = usize::try_from(address) {
        state.heap.texts.get_mut().remove(&at);
    }
}

/// Frees what a block with no plan holds: a cell's value, or the blocks a list, dict or
/// unplanned struct refers to.
#[inline(never)]
fn free_unplanned(state: &mut State, block: &[u64], pending: &mut Vec<u64>) {
    if shape_word(block) == Some(CELL_SHAPE) {
        let number = block.get(2).and_then(|number| usize::try_from(*number).ok());
        if let Some(number) = number {
            // Whatever the value held is let go now, not when the number's used again.
            if let Some(cell) = state.heap.cells.get_mut(number) {
                *cell = Value::Unit;
            }
            state.heap.spare_cells.push(number);
        }
    } else {
        release_children(state, block, pending);
    }
}

/// Gives up the owner a block holds of each block it refers to, adding those it was the last
/// owner of to `pending`.
fn release_children(state: &mut State, block: &[u64], pending: &mut Vec<u64>) {
    let mut found = std::mem::take(&mut state.heap.found);
    children_into(state, block, &mut found);
    for child in found.drain(..) {
        if let Some(count) = state.heap.block_mut(child).and_then(|child| child.first_mut()) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                pending.push(child);
            }
        } else if !state.heap.change_lent_owners(child, -1) && crate::embed::guarded::is_handle(child) {
            pending.push(child);
        }
    }
    state.heap.found = found;
}

/// Gives up the owner a struct's or enum's block holds of each block its plan says it refers
/// to, adding those it was the last owner of to `pending`. False, having done nothing, when
/// its shape's plan doesn't say.
fn release_planned(heap: &mut Heap, block: &[u64], pending: &mut Vec<u64>) -> bool {
    let Heap { plans, blocks, lent, lent_owners, .. } = heap;
    let shape = shape_word(block).and_then(|shape| usize::try_from(shape).ok());
    let fields = block.get(HEADER_WORDS..).unwrap_or_default();
    let words = match shape.and_then(|shape| plans.get(shape)) {
        Some(Plan::Fixed(words)) => words,
        Some(Plan::Cases(cases)) => {
            let case = fields.first().and_then(|case| usize::try_from(*case).ok());
            match case.and_then(|case| cases.get(case)) {
                Some(words) => words,
                None => return false,
            }
        }
        Some(Plan::FixedWithTexts(holds)) => {
            release_planned_holds(holds, fields, blocks, lent.as_ref(), lent_owners, pending);
            return true;
        }
        Some(Plan::CasesWithTexts(cases)) => {
            let case = fields.first().and_then(|case| usize::try_from(*case).ok());
            let Some(holds) = case.and_then(|case| cases.get(case)) else { return false };
            release_planned_holds(holds, fields, blocks, lent.as_ref(), lent_owners, pending);
            return true;
        }
        _ => return false,
    };
    for child in words.iter().filter_map(|at| fields.get(*at).copied()).filter(|child| *child != 0) {
        release_planned_child(blocks, lent.as_ref(), lent_owners, pending, child);
    }
    true
}

/// Gives up the owner a block holds of what's in its words and of the strings' blocks in its
/// headers.
fn release_planned_holds(
    holds: &Holds,
    fields: &[u64],
    blocks: &mut crate::embed::table::Table,
    lent: Option<&std::sync::Arc<crate::embed::table::Table>>,
    lent_owners: &mut HashMap<u64, i64, BuildHasherDefault<super::AddressHasher>>,
    pending: &mut Vec<u64>,
) {
    for child in holds.blocks_in(fields).filter(|child| *child != 0) {
        release_planned_child(blocks, lent, lent_owners, pending, child);
    }
    for child in holds.texts_in(fields).filter(|child| *child != 0) {
        release_planned_child(blocks, lent, lent_owners, pending, child);
    }
}

/// Gives up one owner of a block a struct or enum held, adding it to `pending` if that was the
/// last.
#[expect(clippy::inline_always, reason = "every block's free goes through this, and a call here costs 2%")]
#[inline(always)]
fn release_planned_child(
    blocks: &mut crate::embed::table::Table,
    lent: Option<&std::sync::Arc<crate::embed::table::Table>>,
    lent_owners: &mut HashMap<u64, i64, BuildHasherDefault<super::AddressHasher>>,
    pending: &mut Vec<u64>,
    child: u64,
) {
    let count = usize::try_from(child).ok().and_then(|at| blocks.get_mut(at)).and_then(<[u64]>::first_mut);
    if let Some(count) = count {
        *count = count.saturating_sub(1);
        if *count == 0 {
            pending.push(child);
        }
    } else if crate::embed::threads::ON
        && lent.is_some_and(|lent| usize::try_from(child).is_ok_and(|at| lent.get(at).is_some()))
    {
        *lent_owners.entry(child).or_default() -= 1;
    } else if crate::embed::guarded::is_handle(child) {
        pending.push(child);
    }
}

/// For measuring parts of making and freeing a block.
#[cfg(test)]
pub(in crate::embed) fn measure_part(part: u8, address: usize) {
    with(|state| match part {
        1 => {
            let block = state.heap.fresh(4);
            state.heap.recycle(std::hint::black_box(block));
        }
        2 => {
            if let Some(block) = state.heap.spare.get_mut(4).and_then(Vec::pop) {
                state.heap.spare_words -= block.len();
                state.heap.recycle(std::hint::black_box(block));
            } else {
                let block = state.heap.fresh(4);
                state.heap.recycle(block);
            }
        }
        3 => {
            std::hint::black_box(&mut state.heap);
        }
        5 => {
            if let Some(block) = state.heap.blocks.remove(address) {
                state.heap.recycle(block);
            }
        }
        4 => {
            let block = state.heap.fresh(4);
            let at = block.as_ptr().addr();
            state.heap.blocks.insert(block);
            if let Some(block) = state.heap.blocks.remove(std::hint::black_box(at)) {
                state.heap.recycle(block);
            }
        }
        _ => {}
    });
}

/// Gives up one owner of a block, freeing it when that was the last.
pub(in crate::embed) fn release(state: &mut State, address: u64) {
    let last = match state.heap.block_mut(address).and_then(|block| block.first_mut()) {
        Some(count) if *count > 1 => {
            *count -= 1;
            false
        }
        Some(_) => true,
        None => {
            if !state.heap.change_lent_owners(address, -1) && crate::embed::guarded::is_handle(address) {
                crate::embed::guarded::release_handle(state, address);
            }
            false
        }
    };
    if last {
        free_block(state, address);
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::Heap;
    use crate::embed::{Tables, finish, start, with};
    use crate::packed::Element;

    /// Frees `count` blocks of `words` words at once, as a program dropping a big structure does.
    fn free_many(count: usize, words: usize) {
        with(|state| {
            let blocks: Vec<_> = (0..count).map(|_| state.heap.fresh(words)).collect();
            for block in blocks {
                state.heap.recycle(block);
            }
        });
    }

    #[test]
    fn a_heap_knows_which_lists_hold_no_blocks_only_from_its_plans() {
        let elements = [Rc::new(Element::Int), Rc::new(Element::Str)];
        let heap = Heap::default().planned(&[], &elements);
        assert!(heap.items_hold_nothing(0));
        assert!(!heap.items_hold_nothing(1));
        assert!(!heap.items_hold_nothing(2), "a kind it hasn't a plan for");
        assert!(!Heap::default().items_hold_nothing(0), "a worker's heap, which has none");
    }

    #[test]
    fn what_a_heap_keeps_for_reuse_stays_under_its_limit_however_much_is_freed() {
        let module = fennec_syntax::Module { items: Vec::new() };
        start(&module, Tables::default(), crate::Inputs::default()).unwrap();
        // Blocks of four words fill the limit exactly, and all of it gets used.
        free_many(400_000, 4);
        with(|state| assert_eq!(state.heap.spare_words, super::super::SPARE_TOTAL_WORDS));
        for words in [4, 5, 16, 100] {
            free_many(400_000, words);
            with(|state| {
                let kept: usize =
                    state.heap.spare.iter().enumerate().map(|(size, blocks)| size * blocks.len()).sum();
                assert_eq!(kept, state.heap.spare_words);
                assert!(kept <= super::super::SPARE_TOTAL_WORDS, "{kept} words kept");
                assert!(kept > 0, "nothing's kept for reuse");
            });
        }
        // A block taken back out counts as gone, so the limit is what's kept now, not ever.
        with(|state| {
            let before = state.heap.spare_words;
            let block = state.heap.fresh(4);
            assert_eq!(block.len(), 4);
            assert_eq!(state.heap.spare_words, before - 4);
        });
        finish();
    }
}
