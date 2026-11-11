//! Freeing blocks: a block whose last owner let it go, then what it held that had no other
//! owner, all the way down. Most blocks are small and are freed one at a time, so the paths
//! for those are kept lean and everything else is out of their way.

use std::collections::HashMap;
use std::hash::BuildHasherDefault;

use super::{CELL_SHAPE, FINISHED, GENERATOR_PC, HEADER_WORDS, Heap, children_into, is_text, shape_word};
use crate::embed::plans::{Holds, Plan};
use crate::embed::table::Released;
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
        if !free_plain_list(state, address) {
            free_block(state, address);
        }
        !state.heap.abandoning.is_empty()
    });
    if abandoning {
        run_abandons();
    }
}

/// Frees a list of this thread's whose items hold nothing, the most usual block let go of: out
/// of the table and kept for reuse, with nothing to walk. False, having done nothing, for any
/// other block, which [`free_block`] frees.
fn free_plain_list(state: &mut State, address: u64) -> bool {
    if crate::embed::threads::held_elsewhere(address) {
        return false;
    }
    let first_list = crate::embed::lists::first_list_shape(state);
    let Some(block) = usize::try_from(address).ok().and_then(|at| state.heap.take_plain_list(at, first_list))
    else {
        return false;
    };
    state.heap.recycle(block);
    true
}

/// Frees what other threads let go of last, which waits for this one, which made it. Only a
/// block this heap holds is freed: anything else on the queue isn't this runtime's to let go of,
/// a `shared` value's handle above all, whose count is never queued.
#[cold]
#[inline(never)]
fn free_waiting(state: &mut State) {
    for waiting in crate::embed::threads::waiting() {
        if usize::try_from(waiting).is_ok_and(|at| state.heap.holds_block(at)) {
            free_block(state, waiting);
        }
    }
}

/// `free`, for a block native code knows is a string's. A string's block holds no
/// addresses, so freeing it needs none of the looking `free` does. A block that isn't a
/// string's is freed as `free` would.
pub extern "C" fn free_text(address: i64) {
    // Another thread counted it: `free` waits for that.
    if crate::embed::threads::held_elsewhere(address.cast_unsigned()) {
        free(address);
        return;
    }
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
/// shapes given here are treated specially when they're freed. The program says it once, as it
/// starts, on the thread `main` runs on; every other thread's runtime, started from the program
/// before that, takes it up from `ABANDONS` when it next frees something.
pub extern "C" fn set_abandon(shape: i64, abandon: extern "C" fn(i64)) {
    let shape = u64::try_from(shape).unwrap_or(u64::MAX);
    if SHARES_ABANDONS.get() {
        let mut all = ABANDONS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        all.get_or_insert_with(HashMap::new).insert(shape, abandon);
        ABANDONS_SET.store(all.as_ref().map_or(0, HashMap::len), std::sync::atomic::Ordering::Release);
    }
    with(|state| {
        state.tables.abandons.insert(shape, abandon);
    });
}

/// What closes each generator shape let go partway, for every thread: what `set_abandon` was told.
static ABANDONS: std::sync::Mutex<Option<HashMap<u64, extern "C" fn(i64)>>> = std::sync::Mutex::new(None);
/// How many `ABANDONS` holds, read without the lock on every free.
static ABANDONS_SET: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

thread_local! {
    /// Whether this thread's runtime runs the process's program, as `main`'s thread and the
    /// workers started from it do, and so shares what closes its generators. A runtime started
    /// any other way, as a test's is, has shapes of its own.
    static SHARES_ABANDONS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// This thread's runtime runs the process's program: see `SHARES_ABANDONS`.
pub(in crate::embed) fn share_abandons() {
    SHARES_ABANDONS.set(true);
}

/// Brings this thread's runtime up to date with what closes generators, if the program has said
/// more since it last looked: a worker started before `main`'s thread said it.
fn take_up_abandons(state: &mut State) {
    if !SHARES_ABANDONS.get()
        || state.tables.abandons.len() >= ABANDONS_SET.load(std::sync::atomic::Ordering::Acquire)
    {
        return;
    }
    let all = ABANDONS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(all) = all.as_ref() {
        state.tables.abandons.extend(all.iter().map(|(shape, abandon)| (*shape, *abandon)));
    }
}

/// Runs what closes the generators let go of, each only once nothing here is borrowed, since
/// it's program code calling back in. One that crashes stops what let the generator go, which
/// isn't expecting a failure there. The rest are closed first, with the crash held aside, as a
/// crash's cleanup holds one; a later one that crashes too is said, and the first goes on. In a
/// launched task it stops only that task, as any crash does: the crash stays this thread's,
/// which the task's code finds at its next check. Anywhere else it ends the program on the spot.
pub(in crate::embed) fn run_abandons() {
    let mut held = None;
    while let Some((address, abandon)) = with(next_abandoned) {
        // It reads where the generator stopped to know what's open.
        abandon(i64::try_from(address).unwrap_or(0));
        if crate::embed::failed() {
            if let Some(first) = held {
                // Said, with the first put back, which is held aside again.
                crate::embed::cleanup::crash_resume(first);
            }
            held = Some(crate::embed::cleanup::crash_hold());
        }
        with(|state| {
            if let Some(pc) = state.heap.block_mut(address).and_then(|block| block.get_mut(GENERATOR_PC)) {
                *pc = FINISHED;
            }
            release(state, address);
        });
    }
    if let Some(first) = held {
        crate::embed::cleanup::crash_resume(first);
        if !crate::embed::threads::in_a_task() {
            crate::embed::aot::stop_for_failure();
        }
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
    take_up_abandons(state);
    let mut pending = std::mem::take(&mut state.heap.pending);
    let mut taken = std::mem::take(&mut state.heap.taken);
    pending.push(address);
    while let Some((address, block)) = next_to_free(state, &mut pending, &mut taken) {
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
                free_unplanned(state, &block, &mut pending, &mut taken);
            }
        } else if !release_planned(&mut state.heap, &block, &mut pending, &mut taken) {
            free_unplanned(state, &block, &mut pending, &mut taken);
        }
        state.heap.recycle(block);
    }
    state.heap.pending = pending;
    state.heap.taken = taken;
}

/// The next block to free, out of the table: a child a freed struct held the last owner of, which
/// is out already, or else the next address waiting whose block is still this thread's.
fn next_to_free(
    state: &mut State,
    pending: &mut Vec<u64>,
    taken: &mut Vec<Box<[u64]>>,
) -> Option<(u64, Box<[u64]>)> {
    if let Some(block) = taken.pop() {
        return Some((u64::try_from(block.as_ptr().addr()).unwrap_or(0), block));
    }
    while let Some(address) = pending.pop() {
        if crate::embed::threads::held_elsewhere(address) {
            continue;
        }
        // Only a string has a place kept in it, and that's forgotten after, once it's known to be one.
        if let Some(block) = usize::try_from(address).ok().and_then(|at| state.heap.take_block(at)) {
            return Some((address, block));
        }
        // A `shared` value's handle: its count is the runtime's, wherever its block is.
        if crate::embed::guarded::is_handle(address) {
            crate::embed::guarded::release_handle(state, address);
        } else {
            super::not_native(address, "free");
        }
    }
    None
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

/// Drops the runtime's copy of a freed string's text.
#[inline(never)]
fn forget_text(state: &mut State, address: u64) {
    if let Ok(at) = usize::try_from(address) {
        state.heap.texts.get_mut().remove(&at);
    }
}

/// Frees what a block with no plan holds: a cell's value, or the blocks a list, dict or
/// unplanned struct refers to.
#[inline(never)]
fn free_unplanned(state: &mut State, block: &[u64], pending: &mut Vec<u64>, taken: &mut Vec<Box<[u64]>>) {
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
        release_children(state, block, pending, taken);
    }
}

/// Gives up the owner a block holds of each block it refers to, queueing those it was the last
/// owner of to be freed: taken out of the table at once when this thread alone counts them and
/// holds every block it can reach.
fn release_children(state: &mut State, block: &[u64], pending: &mut Vec<u64>, taken: &mut Vec<Box<[u64]>>) {
    let direct = !crate::embed::threads::counted_elsewhere() && state.heap.holds_all();
    if release_planned_items(state, block, direct, pending, taken) {
        return;
    }
    let mut found = std::mem::take(&mut state.heap.found);
    children_into(state, block, &mut found);
    for child in found.drain(..) {
        release_child(state, child, direct, pending, taken);
    }
    state.heap.found = found;
}

/// Gives up the owner a freed block held of `child`, queueing it to be freed once that was its
/// last.
#[inline]
fn release_child(
    state: &mut State,
    child: u64,
    direct: bool,
    pending: &mut Vec<u64>,
    taken: &mut Vec<Box<[u64]>>,
) {
    if direct {
        match state.heap.blocks.release_one(usize::try_from(child).unwrap_or(usize::MAX)) {
            Released::Gone(block) => {
                taken.push(block);
                return;
            }
            Released::Held => return,
            Released::Missing => {}
        }
    }
    if let Some(block) = state.heap.block_mut(child) {
        if super::uncounted(block) {
            return;
        }
        if let Some(count) = block.first_mut() {
            *count = count.saturating_sub(1);
            if *count == 0 {
                pending.push(child);
            }
        }
    } else if !state.heap.change_lent_owners(child, -1) && crate::embed::guarded::is_handle(child) {
        pending.push(child);
    }
}

/// Gives up what a list's in-place items hold, item by item as their kind's plan says, with no
/// list of the addresses gathered first: json's arrays of values, say, whose strings and lists
/// are freed with them. False, having done nothing, for a block that isn't such a list.
fn release_planned_items(
    state: &mut State,
    block: &[u64],
    direct: bool,
    pending: &mut Vec<u64>,
    taken: &mut Vec<Box<[u64]>>,
) -> bool {
    if shape_word(block).is_none_or(|shape| shape < state.tables.str_shape)
        || is_text(state, block)
        || crate::embed::dicts::is_dict(state, block)
    {
        return false;
    }
    let Some(number) = crate::embed::lists::element_number(state, block) else { return false };
    if state.tables.elements.get(number).is_none_or(|element| crate::embed::lists::owns_items(element)) {
        return false;
    }
    let planned = state.heap.item_plans.get(number);
    if planned.is_none_or(|planned| matches!(planned.plan, Plan::Walk) || planned.plan.holds_nothing()) {
        return false;
    }
    // Letting go of a child changes the table, never the plans, so they're set aside meanwhile.
    let plans = std::mem::take(&mut state.heap.item_plans);
    if let Some(planned) = plans.get(number) {
        let mut letting_go =
            LettingGo { state: &mut *state, direct, pending: &mut *pending, taken: &mut *taken };
        for item in crate::embed::lists::items(block, planned.stride).chunks(planned.stride) {
            if crate::embed::plans::addresses(&planned.plan, item, &mut letting_go).is_none() {
                walk_item(&mut letting_go, number, item);
            }
        }
    }
    state.heap.item_plans = plans;
    true
}

/// Lets go of what an item holds that its plan couldn't say, walking its kind.
#[cold]
#[inline(never)]
fn walk_item(letting_go: &mut LettingGo<'_>, number: usize, item: &[u64]) {
    let mut found = Vec::new();
    if let Some(element) = letting_go.state.tables.elements.get(number) {
        super::held_addresses(element, item, &mut found);
    }
    letting_go.extend(found);
}

/// Lets go of each address a plan finds as it's found, for `release_planned_items`.
struct LettingGo<'a> {
    state: &'a mut State,
    direct: bool,
    pending: &'a mut Vec<u64>,
    taken: &'a mut Vec<Box<[u64]>>,
}

impl Extend<u64> for LettingGo<'_> {
    fn extend<I: IntoIterator<Item = u64>>(&mut self, addresses: I) {
        for child in addresses {
            if child != 0 {
                release_child(self.state, child, self.direct, self.pending, self.taken);
            }
        }
    }
}

/// Gives up the owner a struct's or enum's block holds of each block its plan says it refers
/// to, adding those it was the last owner of to `pending`. False, having done nothing, when
/// its shape's plan doesn't say.
fn release_planned(
    heap: &mut Heap,
    block: &[u64],
    pending: &mut Vec<u64>,
    taken: &mut Vec<Box<[u64]>>,
) -> bool {
    let Heap { plans, blocks, lent, lent_owners, .. } = heap;
    let freeing = &mut Freeing { pending, taken, direct: !crate::embed::threads::counted_elsewhere() };
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
            release_planned_holds(holds, fields, blocks, lent.as_ref(), lent_owners, freeing);
            return true;
        }
        Some(Plan::CasesWithTexts(cases)) => {
            let case = fields.first().and_then(|case| usize::try_from(*case).ok());
            let Some(holds) = case.and_then(|case| cases.get(case)) else { return false };
            release_planned_holds(holds, fields, blocks, lent.as_ref(), lent_owners, freeing);
            return true;
        }
        _ => return false,
    };
    for child in words.iter().filter_map(|at| fields.get(*at).copied()).filter(|child| *child != 0) {
        release_planned_child(blocks, lent.as_ref(), lent_owners, freeing, child);
    }
    true
}

/// Where a freed struct's children go: by address when they have to be looked up again, and taken
/// out of the table already when this thread alone counts them.
struct Freeing<'f> {
    pending: &'f mut Vec<u64>,
    taken: &'f mut Vec<Box<[u64]>>,
    /// Whether no block can be counted by another thread, so one with no owner left is this
    /// thread's to take out at once.
    direct: bool,
}

/// Gives up the owner a block holds of what's in its words and of the strings' blocks in its
/// headers.
fn release_planned_holds(
    holds: &Holds,
    fields: &[u64],
    blocks: &mut crate::embed::table::Table,
    lent: Option<&std::sync::Arc<crate::embed::table::Table>>,
    lent_owners: &mut HashMap<u64, i64, BuildHasherDefault<super::AddressHasher>>,
    freeing: &mut Freeing<'_>,
) {
    for child in holds.blocks_in(fields).filter(|child| *child != 0) {
        release_planned_child(blocks, lent, lent_owners, freeing, child);
    }
    for child in holds.texts_in(fields).filter(|child| *child != 0) {
        release_planned_child(blocks, lent, lent_owners, freeing, child);
    }
}

/// Gives up one owner of a block a struct or enum held, queueing it to be freed if that was the
/// last.
#[expect(clippy::inline_always, reason = "every block's free goes through this, and a call here costs 2%")]
#[inline(always)]
fn release_planned_child(
    blocks: &mut crate::embed::table::Table,
    lent: Option<&std::sync::Arc<crate::embed::table::Table>>,
    lent_owners: &mut HashMap<u64, i64, BuildHasherDefault<super::AddressHasher>>,
    freeing: &mut Freeing<'_>,
    child: u64,
) {
    let at = usize::try_from(child).unwrap_or(usize::MAX);
    if freeing.direct {
        match blocks.release_one(at) {
            Released::Gone(block) => return freeing.taken.push(block),
            Released::Held => return,
            Released::Missing => {}
        }
    } else if let Some(block) = blocks.get_mut(at) {
        if super::uncounted(block) {
            return;
        }
        if let Some(count) = block.first_mut() {
            *count = count.saturating_sub(1);
            if *count == 0 {
                freeing.pending.push(child);
            }
        }
        return;
    }
    if let Some(block) = lent.filter(|_| crate::embed::threads::ON).and_then(|lent| lent.get(at)) {
        // Left alone on a worker, as `change_lent_owners` leaves it.
        if !super::uncounted(block) {
            *lent_owners.entry(child).or_default() -= 1;
        }
    } else if crate::embed::guarded::is_handle(child) {
        freeing.pending.push(child);
    } else {
        super::not_native(child, "free, a block a freed one held");
    }
}

/// Gives up one owner of a block, freeing it when that was the last.
pub(in crate::embed) fn release(state: &mut State, address: u64) {
    let block = state.heap.block_mut(address);
    if block.as_deref().is_some_and(super::uncounted) {
        return;
    }
    let last = match block.and_then(|block| block.first_mut()) {
        Some(count) if *count > 1 => {
            *count -= 1;
            false
        }
        Some(_) => true,
        None => {
            if state.heap.change_lent_owners(address, -1) {
            } else if crate::embed::guarded::is_handle(address) {
                crate::embed::guarded::release_handle(state, address);
            } else {
                super::not_native(address, "release");
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

    /// A list of values laid out in place, each holding a string and a list of strings, lets go
    /// of all of them when it's freed, as its items' plan finds them, leaving nothing behind.
    #[test]
    fn a_freed_list_lets_go_of_what_its_items_hold() {
        use crate::value::{Instance, Value};
        let held = Rc::new(Element::Struct {
            name: Rc::from("Held"),
            fields: vec![
                (Rc::from("name"), Element::Str),
                (Rc::from("tags"), Element::List(Rc::new(Element::Str))),
            ],
        });
        let tables = Tables { elements: vec![held, Rc::new(Element::Str)], ..Tables::default() };
        start(tables, crate::Inputs::default());
        with(|state| {
            let planned = state.heap.item_plans.first().map(|planned| &planned.plan);
            assert!(planned.is_some_and(|plan| !matches!(plan, crate::embed::plans::Plan::Walk)));
        });
        let before = with(|state| state.heap.live());
        let item = |at: usize| {
            let text =
                |what: &str| Value::str(&format!("{what} {at}, long enough to need a block of its own"));
            Value::Instance(Rc::new(Instance {
                type_name: Rc::from("Held"),
                case: None,
                fields: vec![
                    (Rc::from("name"), text("name")),
                    (Rc::from("tags"), Value::list(vec![text("first tag"), text("second tag")])),
                ],
                is_error: false,
                cause: None,
                notes: None,
            }))
        };
        let pushed = with(|state| state.push(Value::list((0..3).map(item).collect())));
        let block = crate::embed::lists::from_value(pushed, 0, 0, 0);
        assert_ne!(block, 0);
        assert!(with(|state| state.heap.live()) >= before + 10, "the list, 3 names, 3 lists and 6 tags");
        crate::embed::heap::free(block);
        with(|state| assert_eq!(state.heap.live(), before));
        finish();
    }

    /// A block generated code made, handed to the runtime anyway, stops the program saying so,
    /// rather than being let go of with nothing done. An address it holds no block for that
    /// generated code didn't make is let go of with nothing done, as before.
    #[test]
    #[should_panic(expected = "the runtime was handed a native block it doesn't own: release")]
    fn a_native_block_handed_to_the_runtime_stops_the_program() {
        start(Tables::default(), crate::Inputs::default());
        let made = Box::new([1_u64, 0, 0]);
        let address = i64::try_from(std::ptr::from_ref(&made[0]).addr()).unwrap();
        // Not one of generated code's, and inside a block of this test's own, so no other test's
        // block or `shared` value can be there: nothing happens.
        with(|state| super::release(state, address.cast_unsigned() + 8));
        super::super::native_made(address);
        with(|state| super::release(state, address.cast_unsigned()));
    }

    /// A list of numbers is freed on the quick way when its owner lets go of it, and not while
    /// another thread still counts it: then it waits, as any block does, until that thread lets go,
    /// and is freed where it was made the next time that thread frees something.
    #[test]
    fn a_list_another_thread_counts_waits_for_it() {
        // A number of its own: what other threads give back waits on a queue by the number of the
        // thread that made it, and every other test's thread is the main thread's 0, so another
        // test freeing a block could take this one's off the queue first.
        let me = i64::from(crate::embed::threads::take_index());
        let elements = vec![Rc::new(Element::Int), Rc::new(Element::Str)];
        start(Tables { elements, ..Tables::default() }, crate::Inputs::default());
        let before = with(|state| state.heap.live());
        let alone = crate::embed::lists::new(0, 1);
        crate::embed::heap::free(alone);
        with(|state| assert_eq!(state.heap.live(), before, "a list no one else counts is freed at once"));
        // A list of strings goes the long way, letting go of each string it holds.
        let long = |at: usize| crate::value::Value::str(&format!("string {at}, long enough to need a block"));
        let pushed = with(|state| state.push(crate::value::Value::list((0..3).map(long).collect())));
        let texts = crate::embed::lists::from_value(pushed, 1, 0, 0);
        assert!(with(|state| state.heap.live()) >= before + 4, "the list and its three strings");
        crate::embed::heap::free(texts);
        with(|state| assert_eq!(state.heap.live(), before, "a list of strings lets go of its strings"));
        let counted = crate::embed::lists::new(0, 1);
        crate::embed::threads::retain_shared(counted, me);
        crate::embed::heap::free(counted);
        with(|state| {
            assert!(state.heap.block(counted.cast_unsigned()).is_some(), "freed while another counted it");
        });
        crate::embed::threads::release_shared(counted, me);
        let other = crate::embed::lists::new(0, 1);
        crate::embed::heap::free(other);
        with(|state| assert_eq!(state.heap.live(), before, "what the other thread let go of is freed here"));
        finish();
    }

    /// A block that's never freed, as a shared enum case's is, made in this thread's table.
    fn never_freed() -> u64 {
        let elements = vec![Rc::new(Element::Int)];
        start(Tables { elements, ..Tables::default() }, crate::Inputs::default());
        let kept = crate::embed::lists::new(0, 0).cast_unsigned();
        with(|state| {
            let block = state.heap.block_mut(kept).unwrap();
            block[0] = super::super::IMMORTAL;
            super::super::mark_never_freed(block);
        });
        kept
    }

    /// Takes three owners of `kept` and gives them back the three ways the runtime lets go of a
    /// block, `rounds` times: as what a freed block held, straight from the table and not, and as
    /// a release. Gives its count after.
    fn count_after_rounds(kept: u64, rounds: u64) -> u64 {
        for _ in 0..rounds {
            with(|state| {
                super::super::retain(state, kept);
                super::super::retain(state, kept);
                super::super::retain(state, kept);
                let (mut pending, mut taken) = (Vec::new(), Vec::new());
                super::release_child(state, kept, true, &mut pending, &mut taken);
                super::release_child(state, kept, false, &mut pending, &mut taken);
                super::release(state, kept);
                assert!(pending.is_empty() && taken.is_empty(), "never freed");
            });
        }
        with(|state| state.heap.block(kept).unwrap()[0])
    }

    /// On a worker, nothing counts a block that's never freed: native code there doesn't, so
    /// neither does the runtime, and the count stays where it started however often the block is
    /// let go of. Counting only the runtime's half had a shared case's count drop by one for each
    /// connection a server's worker answered, until it read as a block that can be freed.
    #[test]
    fn a_worker_leaves_a_never_freed_blocks_count_alone() {
        crate::embed::threads::take_index();
        let kept = never_freed();
        let native_skipped = with(|state| {
            let block = state.heap.block(kept).unwrap();
            super::super::uncounted(block)
        });
        assert!(native_skipped, "a worker leaves it, as its native code does");
        assert_eq!(count_after_rounds(kept, 1_000), super::super::IMMORTAL);
        crate::embed::heap::unshare(kept.cast_signed());
        with(|state| assert_eq!(state.heap.block(kept).unwrap()[0], super::super::IMMORTAL, "copied"));
        finish();
    }

    /// The main thread's native code counts a block that's never freed, so its runtime does too:
    /// each owner it takes, it gives back.
    #[test]
    fn the_main_thread_counts_a_never_freed_block() {
        let kept = never_freed();
        assert_eq!(crate::embed::threads::index(), 0, "a test runs as the main thread");
        assert!(!with(|state| super::super::uncounted(state.heap.block(kept).unwrap())));
        assert_eq!(count_after_rounds(kept, 1_000), super::super::IMMORTAL, "as many taken as given back");
        with(|state| super::release(state, kept));
        assert_eq!(with(|state| state.heap.block(kept).unwrap()[0]), super::super::IMMORTAL - 1);
        finish();
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

    /// One size freed over and over, which nothing asks for again, takes only its share, leaving room
    /// for the others: a string of each length is mostly made fresh, not reused.
    #[test]
    fn no_one_size_takes_all_a_heap_keeps_for_reuse() {
        start(Tables::default(), crate::Inputs::default());
        free_many(400_000, 7);
        with(|state| {
            let sevens = state.heap.spare.get(7).map_or(0, Vec::len) * 7;
            assert!(sevens <= super::super::SPARE_SIZE_WORDS, "{sevens} words of one size");
            assert!(sevens > super::super::SPARE_SIZE_WORDS - 7, "it kept less than its share");
        });
        free_many(1_000, 4);
        with(|state| assert_eq!(state.heap.spare.get(4).map_or(0, Vec::len), 1_000, "room for the next"));
        finish();
    }

    /// A block native code makes outside the table is counted when it says so, and only while
    /// the run counts at all, which a test's always does.
    #[test]
    fn native_code_counts_the_blocks_it_makes_itself_while_counting_is_on() {
        start(Tables::default(), crate::Inputs::default());
        assert_eq!(crate::embed::aot::counting(), 1, "a test's run counts");
        let before = with(|state| state.heap.counts);
        crate::embed::aot::trace_allocated(40);
        let after = with(|state| state.heap.counts);
        assert_eq!(after.allocations, before.allocations + 1);
        assert_eq!(after.allocated_bytes, before.allocated_bytes + 40);
        finish();
    }

    #[test]
    fn what_a_heap_keeps_for_reuse_stays_under_its_limit_however_much_is_freed() {
        start(Tables::default(), crate::Inputs::default());
        // Blocks of four words fill their size's share exactly, and all of it gets used.
        free_many(400_000, 4);
        with(|state| assert_eq!(state.heap.spare_words, super::super::SPARE_SIZE_WORDS));
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
