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

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Mutex, PoisonError};

/// The next number a thread taking one gets. The main thread is 0.
static NEXT: AtomicU16 = AtomicU16::new(1);

thread_local! {
    static INDEX: std::cell::Cell<u16> = const { std::cell::Cell::new(0) };
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

/// `fennec_retain_shared(address, owner)`: one more count of a block, from a thread that
/// didn't make it. `owner` is the number in the block's shape word, which native code has
/// just read to see the block isn't its own.
pub extern "C" fn retain_shared(address: i64, owner: i64) {
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
pub extern "C" fn release_shared(address: i64, _owner: i64) {
    let address = address.cast_unsigned();
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
pub(super) fn held_elsewhere(address: u64) -> bool {
    if !ANY_SHARED.load(Ordering::Relaxed) {
        return false;
    }
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

/// The blocks waiting on this thread's queue, for it to free.
pub(super) fn waiting() -> Vec<u64> {
    if !ANY_REMOTE.load(Ordering::Acquire) {
        return Vec::new();
    }
    remote(|queues| queues.remove(&index()).unwrap_or_default())
}

/// What this thread's runtime made that outlives the task it ran, handed over as blocks for
/// the thread that started the task to adopt when the task ends.
pub fn hand_over() -> Vec<Box<[u64]>> {
    super::with(|state| state.heap.hand_over())
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
            let handed = super::hand_over();
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
