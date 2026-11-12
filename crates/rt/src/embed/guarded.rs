//! `shared` values on real threads: each one owns a table of the blocks it holds. Everything reachable
//! from a `shared` value is in its table, and nothing outside points into it but its handles, so
//! whoever holds its lock has all of it and nobody else has any of it. A value crossing the lock's
//! edge is copied, as `guard.names.clone()` copies with Rust's `Mutex`. No block moves or changes
//! address: a table moves between threads as a value, in one step.
//!
//! The lock itself is a word native code takes in the cell's block. What's here is what happens once
//! it has that word:
//!
//! - **To write**, a thread makes the table its current one, keeping its own behind it. What it makes
//!   goes in the table, wherever the code that makes it is and whatever it knows, which is what makes
//!   this right: `grow(mut x.names, s)` allocates into the table without knowing it's under a lock.
//!   Blocks in a table have a reserved owner number, and while it holds a write lock so does the
//!   thread, so native code counts them with plain arithmetic and changes them in place.
//! - **To read**, a thread sees the table as a range sees lent blocks: read only and never alone. What
//!   it makes goes in its own table, and how it changes the counts of what it reads goes nowhere, as
//!   anything it keeps past the lock is copied out.
//!
//! A handle's own count is only ever the runtime's, beside its table, so any thread can hold one without
//! touching the table it's in. When the last one goes, the table goes with it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::BuildHasherDefault;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, PoisonError, RwLock};

use fennec_syntax::{Diagnostic, Span};

use super::heap::{AddressHasher, IMMORTAL, OWNER_SHIFT, mark_owner};
use super::table::Table;
use super::{State, attempt};

/// The owner number of a block in a `shared` value's table, and of a thread holding a write lock.
pub const GUARDED_OWNER: u64 = 0xFFFF;

/// The owner number of a `shared` value's own block, which no thread ever is.
pub const HANDLE_OWNER: u64 = 0xFFFE;

/// Where a `shared` value's table is.
enum Held {
    /// With nobody holding it. Boxed, so taking it moves a pointer.
    Free(Box<Table>),
    /// Read by those holding the read lock, or by nobody since.
    Read(Arc<Table>),
    /// With the thread holding the write lock.
    Taken,
}

/// What the runtime keeps of a `shared` value.
struct Slot {
    /// Only ever held for a moment: the lock's word in the block is what makes threads wait. Read by
    /// every reader of a value, so they don't line up here.
    held: RwLock<Held>,
    /// How many handles there are to it.
    handles: AtomicU64,
    /// Its name as a place where tasks meet, in a recording or a replay.
    replay: Option<Arc<str>>,
}

type Slots = HashMap<u64, Arc<Slot>, BuildHasherDefault<AddressHasher>>;

/// Each `shared` value's table, by its block's address.
static TABLES: LazyLock<Mutex<Slots>> = LazyLock::new(|| Mutex::new(Slots::default()));

/// How many `shared` values have gone, so what a thread remembers can't outlive one: its address can
/// come back as another's.
static GONE: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// The tables this thread has asked for, so each lock doesn't wait on every other thread's to find
    /// its table, as of how many values had gone.
    static KNOWN: RefCell<(u64, Slots)> = RefCell::new((0, Slots::default()));
}

/// Whether any `shared` value has been made, so programs with none never look.
static ANY: AtomicBool = AtomicBool::new(false);

fn tables<T>(use_them: impl FnOnce(&mut Slots) -> T) -> T {
    use_them(&mut TABLES.lock().unwrap_or_else(PoisonError::into_inner))
}

/// Uses what's kept of the `shared` value at this address, if it is one.
fn with_slot<T>(cell: u64, use_it: impl FnOnce(&Slot) -> T) -> Option<T> {
    KNOWN.with_borrow_mut(|(gone, known)| {
        let now = GONE.load(Ordering::Acquire);
        if *gone != now {
            known.clear();
            *gone = now;
        }
        if let Some(slot) = known.get(&cell) {
            return Some(use_it(slot));
        }
        let slot = tables(|tables| tables.get(&cell).cloned())?;
        let used = use_it(&slot);
        known.insert(cell, slot);
        Some(used)
    })
}

/// One more handle to a `shared` value.
pub(super) fn count_handle(cell: u64) {
    with_slot(cell, |slot| slot.handles.fetch_add(1, Ordering::Relaxed));
}

/// One handle fewer to a `shared` value: the last takes what it held with it.
pub(super) fn release_handle(state: &mut State, cell: u64) {
    if with_slot(cell, |slot| slot.handles.fetch_sub(1, Ordering::AcqRel)) == Some(1) {
        free_table(state, cell);
    }
}

/// Whether this address is a `shared` value's own block.
#[inline]
pub(super) fn is_handle(address: u64) -> bool {
    ANY.load(Ordering::Relaxed) && handle_now(address)
}

#[cold]
#[inline(never)]
fn handle_now(address: u64) -> bool {
    tables(|tables| tables.contains_key(&address))
}

/// What a value holds that a copy of it needs its own of, as places among its words.
fn places_in(state: &State, address: u64) -> Result<Vec<usize>, Diagnostic> {
    let block = state.heap.block(address).ok_or_else(|| lost(address))?;
    let mut places = Vec::new();
    super::heap::child_places(state, block, &mut places)?;
    Ok(places)
}

fn lost(address: u64) -> Diagnostic {
    super::heap::not_native(address, "copy, into or out of a `shared` value");
    Diagnostic::new(format!("a `shared` value lost the block at {address:#x}"), Span::default())
}

/// Gives the current table a copy of what's at `address`, and all it holds, each with one count:
/// what's already in the current table, a handle, or never freed, is the same block with one more.
/// Anything reached twice is one block in the copy too. By a list of what's left to do, as a value
/// can be deeper than a task's stack.
fn copied(state: &mut State, address: u64) -> Result<u64, Diagnostic> {
    copied_as(state, address, Copying::ForReaders)
}

/// Which never-freed blocks a copy keeps as they are, rather than copy.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Copying {
    /// Those any thread that could read the value finds.
    ForReaders,
    /// Only the string literals every thread can read: for a task on another thread, which runs
    /// beside this one, so can't read its table.
    ForTask,
}

/// A copy of `address` and all it holds, in a table of its own, for a task to take as its own: as a
/// value crossing a lock's edge, keeping nothing of this thread's but `shared` handles and the string
/// literals every thread can read. Gives back the copy's address and the table.
pub(super) fn copied_for_task(state: &mut State, address: u64) -> Result<(u64, Box<Table>), Diagnostic> {
    state.heap.enter_table(Box::new(Table::hashed()), GUARDED_OWNER << OWNER_SHIFT);
    let copy = copied_as(state, address, Copying::ForTask);
    let table = state.heap.leave_table();
    Ok((copy?, table))
}

fn copied_as(state: &mut State, address: u64, how: Copying) -> Result<u64, Diagnostic> {
    let mut copies: HashMap<u64, u64> = HashMap::new();
    let mut left = Vec::new();
    let root = copy_of(state, address, &mut copies, &mut left, how)?;
    if root == address && how == Copying::ForReaders {
        copy_read_in(state, address, &mut copies, &mut left)?;
    }
    while let Some(new) = left.pop() {
        for place in places_in(state, new)? {
            let block = state.heap.block(new).ok_or_else(|| lost(new))?;
            let old = block.get(place).copied().unwrap_or(0);
            if old == 0 {
                continue;
            }
            let held = copy_of(state, old, &mut copies, &mut left, how)?;
            if let Some(word) = state.heap.block_mut(new).and_then(|block| block.get_mut(place)) {
                *word = held;
            }
        }
    }
    Ok(root)
}

/// For a value already in the current table, like a list made under a read lock: what it holds that
/// it only found in a table being read, all the way down, gets its own copy here. Native code made
/// such a value holding blocks of the `shared` value's table, whose counts went nowhere, as what's
/// kept past a read lock is copied out. Without the copy, once the lock's let go, the runtime can't
/// find those blocks, and a string of them prints as nothing.
fn copy_read_in(
    state: &mut State,
    address: u64,
    copies: &mut HashMap<u64, u64>,
    left: &mut Vec<u64>,
) -> Result<(), Diagnostic> {
    if state.heap.reading.is_empty() {
        return Ok(());
    }
    let mut seen = std::collections::HashSet::from([address]);
    let mut kept = vec![address];
    while let Some(here) = kept.pop() {
        for place in places_in(state, here)? {
            let held = state.heap.block(here).and_then(|block| block.get(place).copied()).unwrap_or(0);
            if held == 0 || is_handle(held) {
                continue;
            }
            let at = usize::try_from(held).unwrap_or(usize::MAX);
            if state.heap.blocks.get(at).is_some() {
                // Its own, so what it holds may be read in too.
                if seen.insert(held) {
                    kept.push(held);
                }
                continue;
            }
            if !state.heap.reading.iter().any(|table| table.get(at).is_some()) {
                continue;
            }
            let copy = copy_of(state, held, copies, left, Copying::ForReaders)?;
            if let Some(word) = state.heap.block_mut(here).and_then(|block| block.get_mut(place)) {
                *word = copy;
            }
        }
    }
    Ok(())
}

/// The block in the copy for what's at `address`, added to `left` if it's new.
fn copy_of(
    state: &mut State,
    address: u64,
    copies: &mut HashMap<u64, u64>,
    left: &mut Vec<u64>,
    how: Copying,
) -> Result<u64, Diagnostic> {
    if is_handle(address) {
        count_handle(address);
        return Ok(address);
    }
    if let Some(copy) = copies.get(&address).copied() {
        bump(state, copy);
        return Ok(copy);
    }
    if usize::try_from(address).is_ok_and(|at| state.heap.blocks.get(at).is_some()) {
        bump(state, address);
        return Ok(address);
    }
    let block = state.heap.block(address).ok_or_else(|| lost(address))?;
    let kept = match how {
        Copying::ForReaders => everyone_finds(state, address),
        Copying::ForTask => super::heap::shared_static(address),
    };
    if block.first().is_some_and(|count| *count >= IMMORTAL) && kept {
        return Ok(address);
    }
    let mut copy: Box<[u64]> = state.heap.block(address).ok_or_else(|| lost(address))?.into();
    if let Some(count) = copy.first_mut() {
        *count = 1;
    }
    mark_owner(&mut copy, state.heap.owner_bits());
    state.heap.copied(copy.len());
    let new = u64::try_from(state.heap.insert(copy)).unwrap_or(0);
    copies.insert(address, new);
    left.push(new);
    Ok(new)
}

/// Whether a block that's never freed can be found from any thread while it could be read: the
/// program's own thread's are, as it lends them to any other that runs. String literals are its.
fn everyone_finds(state: &State, address: u64) -> bool {
    super::threads::index() == 0 || state.heap.lent_has(address) || super::heap::shared_static(address)
}

fn bump(state: &mut State, address: u64) {
    if let Some(block) = state.heap.block_mut(address)
        && !super::heap::uncounted(block)
        && let Some(count) = block.first_mut()
    {
        *count += 1;
    }
}

/// `fennec_shared_new(cell)`: makes a new `shared` value's block the start of a table of its own, with
/// a copy of all it holds. Its count becomes the runtime's. Gives back its address.
pub extern "C" fn shared_new(cell: i64) -> i64 {
    attempt(|state: &mut State| {
        let address = u64::try_from(cell).unwrap_or(0);
        let at = usize::try_from(address).unwrap_or(0);
        ANY.store(true, Ordering::Relaxed);
        let places = places_in(state, address)?;
        let mut block = state.heap.blocks.remove_any(at).ok_or_else(|| lost(address))?;
        let count = block.first().copied().unwrap_or(1);
        let held: Vec<u64> = places.iter().filter_map(|place| block.get(*place).copied()).collect();
        // Put back for what it holds to be read from: the table only takes it once they're copied.
        state.heap.enter_table(Box::new(Table::hashed()), GUARDED_OWNER << OWNER_SHIFT);
        for (place, old) in places.iter().zip(&held) {
            if *old != 0 {
                // A copy that fails leaves the new table, so whatever runs next isn't in it, and the
                // block goes back as it was.
                let new = match copied(state, *old) {
                    Ok(new) => new,
                    Err(failure) => {
                        drop(state.heap.leave_table());
                        state.heap.blocks.insert(block);
                        return Err(failure);
                    }
                };
                if let Some(word) = block.get_mut(*place) {
                    *word = new;
                }
            }
        }
        mark_owner(&mut block, HANDLE_OWNER << OWNER_SHIFT);
        if let Some(word) = block.first_mut() {
            *word = 0;
        }
        state.heap.blocks.insert(block);
        let table = state.heap.leave_table();
        let slot = Slot {
            held: RwLock::new(Held::Free(table)),
            handles: AtomicU64::new(count),
            replay: crate::replay::place("s"),
        };
        // A value kept at this address before, by a run that ended without letting go of it, has
        // gone: what any thread remembers of it goes too.
        if tables(|tables| tables.insert(address, Arc::new(slot))).is_some() {
            GONE.fetch_add(1, Ordering::Release);
        }
        // The counts it held of what it was given go, with their copies in the table.
        for old in held.into_iter().filter(|old| *old != 0) {
            super::heap::release(state, old);
        }
        Ok(cell)
    })
}

/// `fennec_shared_enter(cell, write)`: the thread holds the lock's word, and now has the table. Gives
/// back what native code's `@fennec_thread` is to hold while it does.
pub extern "C" fn shared_enter(cell: i64, write: i64) -> i64 {
    attempt(|state: &mut State| {
        let address = u64::try_from(cell).unwrap_or(0);
        with_slot(address, |slot| enter(state, slot, write != 0)).ok_or_else(|| lost(address))??;
        Ok(state.heap.owner_number())
    })
}

fn enter(state: &mut State, slot: &Slot, write: bool) -> Result<(), Diagnostic> {
    if write {
        let mut held = slot.held.write().unwrap_or_else(PoisonError::into_inner);
        // Taken only once this thread has the table: a reader still holding it leaves it read.
        let table = match std::mem::replace(&mut *held, Held::Taken) {
            Held::Free(table) => table,
            Held::Read(table) => match Arc::try_unwrap(table) {
                Ok(table) => Box::new(table),
                Err(table) => {
                    *held = Held::Read(table);
                    return Err(still_read());
                }
            },
            Held::Taken => return Err(still_read()),
        };
        drop(held);
        state.heap.enter_table(table, GUARDED_OWNER << OWNER_SHIFT);
        return Ok(());
    }
    if let Held::Read(table) = &*slot.held.read().unwrap_or_else(PoisonError::into_inner) {
        state.heap.reading.push(Arc::clone(table));
        return Ok(());
    }
    // The first to read it since it was written, or another thread that was.
    let mut held = slot.held.write().unwrap_or_else(PoisonError::into_inner);
    let reading = match std::mem::replace(&mut *held, Held::Taken) {
        Held::Free(table) => Arc::new(*table),
        Held::Read(table) => table,
        Held::Taken => return Err(still_read()),
    };
    state.heap.reading.push(Arc::clone(&reading));
    *held = Held::Read(reading);
    Ok(())
}

/// What's wrong when a `shared` value's lock is let go of and the value has gone, though this
/// thread held its table: something let go of a value it didn't hold.
fn gone_while_held(address: u64) -> Diagnostic {
    Diagnostic::new(
        format!("the `shared` value at {address:#x} went while its lock was held"),
        Span::default(),
    )
    .with_help("this is a bug in Fennec; please report it")
}

/// What's wrong when a lock's word lets in more than it should, which would be a bug in native code.
fn still_read() -> Diagnostic {
    Diagnostic::new("a `shared` value's lock let two threads in", Span::default())
}

/// `fennec_shared_leave(cell, write)`: the thread is about to let go of the lock's word. Gives back what
/// native code's `@fennec_thread` is to hold again.
pub extern "C" fn shared_leave(cell: i64, write: i64) -> i64 {
    attempt(|state: &mut State| {
        let address = u64::try_from(cell).unwrap_or(0);
        if write != 0 {
            // The table leaves this thread only for its slot: with none, it stays, and what it holds
            // with it, rather than going with nowhere to be put back.
            let left = with_slot(address, |slot| {
                let table = state.heap.leave_table();
                *slot.held.write().unwrap_or_else(PoisonError::into_inner) = Held::Free(table);
            });
            left.ok_or_else(|| gone_while_held(address))?;
        } else {
            state.heap.reading.pop();
        }
        // Text kept of its blocks is no use once another thread can change or free them.
        let texts = state.heap.texts.get_mut();
        if !texts.is_empty() {
            texts.clear();
        }
        Ok(state.heap.owner_number())
    })
}

/// `fennec_shared_in(block)`: a copy of a value in the table of the lock this thread holds to write,
/// holding one count.
pub extern "C" fn shared_in(block: i64) -> i64 {
    attempt(|state: &mut State| {
        let new = copied(state, u64::try_from(block).unwrap_or(0))?;
        Ok(i64::try_from(new).unwrap_or(0))
    })
}

/// `fennec_shared_out(block, held)`: a copy of a value in the table behind the `held` write locks a
/// `with` is about to leave, holding one count: this thread's own, or that of a lock around it. With
/// only read locks, that's the current table.
pub extern "C" fn shared_out(block: i64, held: i64) -> i64 {
    attempt(|state: &mut State| {
        let address = u64::try_from(block).unwrap_or(0);
        let held = usize::try_from(held).unwrap_or(0);
        state.heap.swap_behind(held);
        let new = copied(state, address);
        state.heap.swap_behind(held);
        Ok(i64::try_from(new?).unwrap_or(0))
    })
}

/// `fennec_shared_settle(cell)`, for each of the `shared` values a `with` holds two or more of to
/// write, before it leaves any: what's made under such a lock goes in whichever table is on top, and
/// one value can be given what's in another's. So each is walked, and given its own copy of anything
/// it reaches that's in a table that isn't its own. It costs the size of each, for this rare shape.
pub extern "C" fn shared_settle(cell: i64) {
    attempt(|state: &mut State| {
        let address = u64::try_from(cell).unwrap_or(0);
        let at = usize::try_from(address).unwrap_or(0);
        if state.heap.blocks.get(at).is_some() {
            return settle(state, address);
        }
        let number = state.heap.behind_holding(at).ok_or_else(|| lost(address))?;
        state.heap.swap_table(number);
        let settled = settle(state, address);
        state.heap.swap_table(number);
        settled
    });
}

/// Walks what's in the current table from `cell`, giving it a copy of what isn't.
fn settle(state: &mut State, cell: u64) -> Result<(), Diagnostic> {
    let mut seen = std::collections::HashSet::from([cell]);
    let mut left = vec![cell];
    while let Some(address) = left.pop() {
        for place in places_in(state, address)? {
            let block = state.heap.block(address).ok_or_else(|| lost(address))?;
            let held = block.get(place).copied().unwrap_or(0);
            if held == 0 || is_handle(held) {
                continue;
            }
            if usize::try_from(held).is_ok_and(|at| state.heap.blocks.get(at).is_some()) {
                if seen.insert(held) {
                    left.push(held);
                }
                continue;
            }
            let found = state.heap.block(held).ok_or_else(|| lost(held))?;
            if found.first().is_some_and(|count| *count >= IMMORTAL) && everyone_finds(state, held) {
                continue;
            }
            let new = copied(state, held)?;
            if let Some(word) = state.heap.block_mut(address).and_then(|block| block.get_mut(place)) {
                *word = new;
            }
            super::heap::release(state, held);
        }
    }
    Ok(())
}

/// What taking a lock is called in a recording: reading or writing.
fn taking(write: i64) -> &'static str {
    if write == 0 { "read" } else { "write" }
}

/// `fennec_replay_lock(cell, write)`: in a replay, waits for this task's turn to take `cell`'s
/// lock, to write if `write` is 1. The turn is this task's until `fennec_replay_locked`, so no
/// other task can take the lock first. Native code calls it before trying for the lock, only while
/// the run is recorded or replayed. A replay that's diverged fails.
pub extern "C" fn replay_lock(cell: i64, write: i64) {
    let place = with_slot(u64::try_from(cell).unwrap_or(0), |slot| slot.replay.clone()).flatten();
    if let Err(why) = crate::replay::turn(place.as_deref(), taking(write)) {
        super::fail_with(Diagnostic::new(why, Span::default()));
    }
}

/// `fennec_replay_locked(cell, write)`: called with `cell`'s lock held. In a recording, logs that
/// this task has taken it, so the log has the order it was taken in. In a replay, ends this task's
/// turn, so the next one's can start.
pub extern "C" fn replay_locked(cell: i64, write: i64) {
    let place = with_slot(u64::try_from(cell).unwrap_or(0), |slot| slot.replay.clone()).flatten();
    crate::replay::met(place.as_deref(), taking(write), 0);
}

/// `fennec_shared_wait()`: a lock's word is taken, and native code has tried for a while. What this
/// thread printed comes out, as it does before anything else that waits.
pub extern "C" fn shared_wait() {
    super::write_kept_output();
    std::thread::yield_now();
}

/// Lets go of what a `shared` value held, now nothing holds a handle to it: its whole table, and a
/// count of each handle inside it.
pub(super) fn free_table(state: &mut State, cell: u64) {
    let Some(slot) = tables(|tables| tables.remove(&cell)) else { return };
    GONE.fetch_add(1, Ordering::Release);
    let held =
        std::mem::replace(&mut *slot.held.write().unwrap_or_else(PoisonError::into_inner), Held::Taken);
    let table = match held {
        Held::Free(table) => *table,
        Held::Read(table) => match Arc::try_unwrap(table) {
            Ok(table) => table,
            // A thread reading it holds a handle while it does, so the last can't go while one
            // reads: something let go of a handle it didn't hold. Freeing an empty table in its
            // place, as this once did, would hide that.
            Err(_) => super::heap::runtime_bug(&format!(
                "the `shared` value at {cell:#x} went while another thread was reading it"
            )),
        },
        Held::Taken => return,
    };
    let mut handles = Vec::new();
    for block in table.into_blocks() {
        let mut places = Vec::new();
        if super::heap::child_places(state, &block, &mut places).is_ok() {
            handles.extend(
                places.iter().filter_map(|place| block.get(*place).copied()).filter(|held| is_handle(*held)),
            );
        }
        state.heap.recycle(block);
    }
    for handle in handles {
        release_handle(state, handle);
    }
}

/// Forgets each `shared` value whose table goes with this heap, at the end of a run whose heap is freed
/// rather than left for the process's exit: one the run ended holding the write lock of, as a test
/// may. A value's own block is in its table, so it goes too, and what's kept of the value would
/// outlive it. The next block put at that address would look like a handle to it, and copying,
/// locking or letting go of that block would go to a table that's gone. A value whose lock isn't held
/// keeps its table, and its block, so stays. Only a process with more than one run in it has this to
/// do.
pub(super) fn forget_ending(heap: &super::heap::Heap) {
    if !ANY.load(Ordering::Relaxed) {
        return;
    }
    let forgot = tables(|tables| {
        let before = tables.len();
        tables.retain(|address, _| !usize::try_from(*address).is_ok_and(|at| heap.holds_block(at)));
        tables.len() != before
    });
    if forgot {
        GONE.fetch_add(1, Ordering::Release);
    }
}

pub mod rollback;

#[cfg(test)]
mod tests;
