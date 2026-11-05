//! What the runtime needs when native code runs on more than one thread. Each thread has its
//! own runtime: its own heap and table of the blocks it made, which only it frees. A block's
//! shape word holds the number of the thread that made it (see `heap::OWNER_SHIFT`), 0 for the
//! main thread.
//!
//! Counts are biased. The thread that made a block counts it in its count word with plain
//! arithmetic. Any other thread counts it here, in a shared count kept beside it, so the two
//! never race on one word. When the owner lets go of its last count while others still hold
//! the block, the block waits; when the last of those lets go, the block goes on its owner's
//! queue, and the owner frees it the next time it's in the runtime. No thread ever frees a
//! block another made, and no table is shared.
//!
//! A `parallel` call works differently, since the thread that made the call only waits for it.
//! For the length of the call it lends its table of blocks (`lend`), which nothing can change
//! while it waits, so each range can read its blocks. Nothing writes a lent block: `alone` is
//! false for one, so it's copied rather than changed in place. Each range keeps how it changed
//! lent blocks' owners to itself, with no lock, and hands that over with what it made. When
//! the call ends, the calling thread takes its table back, adopts what the ranges made, and
//! applies those changes (`settle`), freeing what they took the last owner from.

use std::collections::HashMap;
#[cfg(any(test, feature = "threads"))]
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Mutex, PoisonError};

#[cfg(any(test, feature = "threads"))]
use super::table::Table;

/// Whether this runtime is built for native code on several threads. Without it, the checks
/// on the paths every block takes are compiled away.
pub(super) const ON: bool = cfg!(any(test, feature = "threads"));

/// The next number a thread taking one gets. The main thread is 0.
static NEXT: AtomicU16 = AtomicU16::new(1);

thread_local! {
    static INDEX: std::cell::Cell<u16> = const { std::cell::Cell::new(0) };
}

thread_local! {
    /// The `shared` values' lock words this thread holds beside the one native code remembers.
    static LOCKS: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Notes that this thread is taking the lock at `word`: 1 if it holds it already, which would
/// wait forever, and 0 otherwise.
pub extern "C" fn lock_note(word: u64) -> i64 {
    LOCKS.with_borrow_mut(|held| {
        if held.contains(&word) {
            return 1;
        }
        held.push(word);
        0
    })
}

/// Notes that this thread has given back the lock at `word`.
pub extern "C" fn lock_forget(word: u64) {
    LOCKS.with_borrow_mut(|held| {
        if let Some(at) = held.iter().rposition(|taken| *taken == word) {
            held.swap_remove(at);
        }
    });
}

/// This thread's number: 0 for the main thread, until it takes another.
pub fn index() -> u16 {
    INDEX.get()
}

/// Gives this thread a number of its own, for a worker before its runtime starts.
pub fn take_index() -> u16 {
    let index = NEXT.fetch_add(1, Ordering::Relaxed);
    INDEX.set(index);
    index
}

/// A block another thread holds: how many counts the other threads hold, and whether its
/// owner has let go of all of its own.
#[derive(Default)]
struct Shared {
    count: u64,
    owner_done: bool,
    owner: u16,
}

/// Whether any block has ever been counted across threads, so a program with none never
/// looks in the table.
static ANY_SHARED: AtomicBool = AtomicBool::new(false);

/// Blocks counted across threads, by address.
static SHARED: Mutex<Option<HashMap<u64, Shared>>> = Mutex::new(None);

/// Blocks waiting for the thread that made them to free them, by the thread's number.
static REMOTE: Mutex<Option<HashMap<u16, Vec<u64>>>> = Mutex::new(None);

/// Whether any thread has blocks waiting on its queue.
static ANY_REMOTE: AtomicBool = AtomicBool::new(false);

fn shared<T>(f: impl FnOnce(&mut HashMap<u64, Shared>) -> T) -> T {
    let mut table = SHARED.lock().unwrap_or_else(PoisonError::into_inner);
    f(table.get_or_insert_with(HashMap::new))
}

fn remote<T>(f: impl FnOnce(&mut HashMap<u16, Vec<u64>>) -> T) -> T {
    let mut queues = REMOTE.lock().unwrap_or_else(PoisonError::into_inner);
    f(queues.get_or_insert_with(HashMap::new))
}

/// Lends this thread's blocks to the ranges of the call it's about to wait for.
#[cfg(any(test, feature = "threads"))]
pub(super) fn lend() -> Arc<Table> {
    super::with(|state| Arc::new(std::mem::take(&mut state.heap.blocks)))
}

/// Takes this thread's blocks back, the call over. Every range has let go of them by now; if
/// one somehow hadn't, they're kept where this thread can read them, as native code still
/// points at them.
#[cfg(any(test, feature = "threads"))]
pub(super) fn take_back(lent: Arc<Table>) {
    super::with(|state| match Arc::try_unwrap(lent) {
        Ok(table) => state.heap.blocks = table,
        Err(table) => state.heap.lent = Some(table),
    });
}

/// Starts a range on a worker: it can read the blocks lent for the call.
#[cfg(any(test, feature = "threads"))]
pub(super) fn range_starts(lent: Option<Arc<Table>>) {
    super::with(|state| state.heap.lent = lent);
}

/// Ends a range on a worker: it lets go of the blocks lent for the call, and of the text it
/// kept of them, as the same addresses may hold other strings by its next range. Writes what it
/// printed, so it all comes before what the calling thread prints next. Gives back how it changed
/// their owners, and what it printed if this worker's runtime doesn't write it.
#[cfg(any(test, feature = "threads"))]
pub(super) fn range_ends() -> (Vec<(u64, i64)>, Vec<u8>) {
    super::write_kept_output();
    super::with(|state| {
        if state.heap.lent.take().is_some() {
            state.heap.texts.get_mut().clear();
            state.heap.set_cursor(super::heap::Cursor::default());
        }
        (state.heap.lent_owners.drain().collect(), state.output.0.take())
    })
}

/// Applies the changes the ranges of a call made to this thread's blocks' owners, freeing those
/// left with none.
#[cfg(any(test, feature = "threads"))]
pub(super) fn settle(changes: HashMap<u64, i64>) {
    let gone: Vec<u64> = super::with(|state| {
        let mut gone = Vec::new();
        for (address, by) in changes {
            if let Some(count) = state.heap.block_mut(address).and_then(|block| block.first_mut()) {
                *count = count.saturating_add_signed(by);
                if *count == 0 {
                    gone.push(address);
                }
            }
        }
        gone
    });
    for address in gone {
        super::heap::free(i64::try_from(address).unwrap_or(0));
    }
}

/// `fennec_retain_shared(address, owner)`: one more count of a block, from a thread that
/// didn't make it. `owner` is the number in the block's shape word, which native code has
/// just read to see the block isn't its own.
pub extern "C" fn retain_shared(address: i64, owner: i64) {
    if owner.cast_unsigned() == super::guarded::HANDLE_OWNER {
        super::guarded::count_handle(address.cast_unsigned());
        return;
    }
    if super::with(|state| state.heap.change_lent_owners(address.cast_unsigned(), 1)) {
        return;
    }
    ANY_SHARED.store(true, Ordering::Relaxed);
    let owner = u16::try_from(owner).unwrap_or(0);
    shared(|table| {
        let entry =
            table.entry(address.cast_unsigned()).or_insert_with(|| Shared { owner, ..Shared::default() });
        entry.count += 1;
    });
}

/// `fennec_release_shared(address, owner)`: one count fewer of a block, from a thread that
/// didn't make it. When that was the last count anyone held, the block goes on its owner's
/// queue.
pub extern "C" fn release_shared(address: i64, owner: i64) {
    let address = address.cast_unsigned();
    if owner.cast_unsigned() == super::guarded::HANDLE_OWNER {
        super::with(|state| super::guarded::release_handle(state, address));
        return;
    }
    if super::with(|state| state.heap.change_lent_owners(address, -1)) {
        return;
    }
    let freed = shared(|table| {
        let entry = table.get_mut(&address)?;
        entry.count = entry.count.saturating_sub(1);
        if entry.count > 0 || !entry.owner_done {
            return None;
        }
        table.remove(&address).map(|entry| entry.owner)
    });
    if let Some(owner) = freed {
        remote(|queues| queues.entry(owner).or_default().push(address));
        ANY_REMOTE.store(true, Ordering::Release);
    }
}

/// Whether a block whose owner has just let go of its last count is still held by another
/// thread, in which case it waits for that thread to let go, rather than being freed now.
#[inline]
pub(super) fn held_elsewhere(address: u64) -> bool {
    ON && ANY_SHARED.load(Ordering::Relaxed) && held_elsewhere_now(address)
}

/// `held_elsewhere`, once some block has been counted across threads.
#[cold]
#[inline(never)]
fn held_elsewhere_now(address: u64) -> bool {
    shared(|table| match table.get_mut(&address) {
        Some(entry) if entry.count > 0 => {
            entry.owner_done = true;
            true
        }
        Some(_) => {
            table.remove(&address);
            false
        }
        None => false,
    })
}

/// Whether any thread has blocks waiting on its queue: one load, for every `free` to ask.
#[inline]
pub(super) fn any_waiting() -> bool {
    ON && ANY_REMOTE.load(Ordering::Acquire)
}

/// The blocks waiting on this thread's queue, for it to free.
#[cold]
#[inline(never)]
pub(super) fn waiting() -> Vec<u64> {
    remote(|queues| queues.remove(&index()).unwrap_or_default())
}

/// How many blocks this thread's runtime has made since it last handed them over: where a
/// task starts, so it hands over only what it made. Work a worker steals while the task waits
/// starts and ends inside it, and hands its own over first.
pub fn made_so_far() -> usize {
    super::with(|state| state.heap.made_so_far())
}

/// What this thread's runtime made since `since` that outlives the task it ran, handed over
/// as blocks for the thread that started the task to adopt when the task ends.
pub fn hand_over(since: usize) -> Vec<Box<[u64]>> {
    super::with(|state| state.heap.hand_over(since))
}

/// Takes over blocks a task's thread handed over, as this thread's own.
pub fn adopt(blocks: Vec<Box<[u64]>>) {
    super::with(|state| state.heap.adopt_blocks(blocks));
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::super::{Tables, finish, heap, start, with};
    use crate::packed::Element;
    use crate::value::Value;

    fn begin() {
        let tables = Tables { shapes: vec![Rc::new(Element::Str)], ..Tables::default() };
        start(&fennec_syntax::Module { items: Vec::new() }, tables, crate::Inputs::default()).unwrap();
    }

    fn live() -> usize {
        with(|state| state.heap.live())
    }

    fn text_of(address: i64) -> Value {
        with(|state| Some(heap::read_block(state, address.cast_unsigned()))).unwrap()
    }

    fn shape_word(address: i64) -> u64 {
        with(|state| state.heap.block(address.cast_unsigned())?.get(1).copied()).unwrap_or(0)
    }

    #[test]
    fn a_task_s_blocks_are_its_own_until_the_thread_that_started_it_adopts_them() {
        begin();
        let before = live();
        let (owner, made) = std::thread::spawn(|| {
            let owner = super::take_index();
            begin();
            let names: Vec<i64> = ["a fox's name, longer than a word", "a fennec's"]
                .iter()
                .map(|name| with(|state| heap::text_address(state, name)))
                .collect();
            // Its own runtime reads its own blocks, whatever their owner bits say.
            assert_eq!(text_of(names[0]), Value::str("a fox's name, longer than a word"));
            let marked = names.iter().all(|name| shape_word(*name) >> heap::OWNER_SHIFT == u64::from(owner));
            assert!(marked);
            let handed = super::hand_over(0);
            finish();
            (owner, (names, handed))
        })
        .join()
        .unwrap();
        assert_ne!(owner, 0);
        let (names, handed) = made;
        super::adopt(handed);
        // Adopted, they're this thread's ordinary blocks.
        assert_eq!(shape_word(names[1]) >> heap::OWNER_SHIFT, 0);
        assert_eq!(text_of(names[1]), Value::str("a fennec's"));
        for name in names {
            heap::free(name);
        }
        assert_eq!(live(), before);
        finish();
    }

    #[test]
    fn a_block_another_thread_still_holds_waits_and_its_owner_frees_it_after() {
        let owner = super::take_index();
        begin();
        let before = live();
        let kept = with(|state| heap::text_address(state, "kept by another thread for a while"));
        let other = with(|state| heap::text_address(state, "something else to free"));
        std::thread::spawn(move || super::retain_shared(kept, i64::from(owner))).join().unwrap();
        // The owner lets go of its count, but the other thread still holds the block.
        heap::free(kept);
        assert_eq!(text_of(kept), Value::str("kept by another thread for a while"));
        std::thread::spawn(move || super::release_shared(kept, i64::from(owner))).join().unwrap();
        // The next time the owner frees anything, it frees what waited on its queue too.
        heap::free(other);
        assert_eq!(live(), before);
        finish();
    }
}
