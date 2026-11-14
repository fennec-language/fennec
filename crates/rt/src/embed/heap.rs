//! Boxed values for native code: structs and enums that contain themselves, like syntax
//! trees, and the types that hold them. Each value is a block of words the runtime owns.
//! Word 0 counts the block's owners, word 1 says which shape it has, and the rest hold its
//! fields as packed lists hold items, a boxed field as the address of its own block.
//!
//! Native code reads and writes blocks directly, and counts owners itself. Copies share a
//! block; a change to a shared block first gives the changer a copy of its own. When the
//! count reaches zero, native code calls `free`, which releases the block's fields too.
//! The runtime never follows an address it didn't hand out: it finds every block by its
//! address in a table, `super::table`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::io::Write as _;
use std::rc::Rc;

use fennec_syntax::Diagnostic;

use super::{State, Tables, attempt, span, with};
use crate::packed::{Element, Fields};
use crate::value::{Instance, Lent, Value};

#[cfg(any(test, feature = "audit"))]
pub(crate) mod audit;
mod freeing;
mod named;
pub(super) mod shown;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(super) use freeing::free_waiting;
pub use freeing::{free, free_text, set_abandon};
pub(super) use freeing::{release, run_abandons, share_abandons, strays};
pub(super) use named::packable;
pub use shown::{own_text, own_text_written};

/// Words before a block's fields: its owner count and its shape.
pub const HEADER_WORDS: usize = 2;

/// Hashes block addresses, which are already spread out, by mixing their bits once.
#[derive(Default)]
pub struct AddressHasher(u64);

impl Hasher for AddressHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = (self.0 << 8 | u64::from(*byte)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        }
    }

    fn write_usize(&mut self, n: usize) {
        self.0 = u64::try_from(n).unwrap_or(0).wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(29);
    }

    fn write_u64(&mut self, n: u64) {
        self.0 = n.wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(29);
    }
}

/// A place in a string's text: its `chars`th character starts `bytes` bytes in. Reading a
/// string in order goes on from the last place rather than counting from its start.
#[derive(Clone, Copy, Default)]
pub(super) struct Cursor {
    pub(super) address: usize,
    pub(super) chars: usize,
    pub(super) bytes: usize,
}

/// Every live block, by its address, and freed blocks kept for reuse, by size.
#[derive(Default)]
pub(crate) struct Heap {
    pub(super) blocks: super::table::Table,
    spare: Vec<Vec<Box<[u64]>>>,
    /// How many words `spare`'s blocks hold.
    spare_words: usize,
    /// How many of the blocks live for the whole run.
    statics: usize,
    /// Whether this thread's string literals were moved where every thread can read them.
    shared_statics: bool,
    /// Strings' text as a `Value` holds it, made once per block.
    pub(super) texts: RefCell<HashMap<usize, Rc<str>, BuildHasherDefault<AddressHasher>>>,
    /// Where a string that isn't ASCII was last read.
    cursor: Cursor,
    /// Generators let go partway, waiting for what closes what they hold.
    abandoning: Vec<u64>,
    /// Blocks waiting to be freed and a freed block's children, kept to save making them anew:
    /// by address, and those already taken out of the table, each at its own address.
    pending: Vec<u64>,
    taken: Vec<Box<[u64]>>,
    found: Vec<u64>,
    /// The `Value`s cells hold, by the number in each cell, and unused numbers.
    cells: Vec<Value>,
    spare_cells: Vec<usize>,
    /// What native code's run has cost so far, counted only when something will read it.
    pub(super) counts: Counts,
    /// On a worker that counts, where it keeps `counts` for the run's report to read: a worker's
    /// heap is its own thread's, which the report on the main thread can't see.
    shared_counts: Option<std::sync::Arc<std::sync::Mutex<Counts>>>,
    /// Whether `counts` is kept: under `FENNEC_COUNTS`, which reports it, after `count_allocations`,
    /// and in tests.
    counting: bool,
    /// In a program built to trace what it costs, the same for each place in its source, and the
    /// place native code said it's at last. Empty otherwise.
    pub(super) sites: HashMap<u32, Counts>,
    site: Option<u32>,
    /// Where each shape's blocks hold addresses, by the shape's number.
    plans: Vec<super::plans::Plan>,
    /// Where each kind of list item laid out in place holds addresses, by the kind's number. A
    /// worker's heap has none, and walks them.
    item_plans: Vec<super::plans::ItemPlan>,
    /// Which of those hold no blocks at all, as most lists' items don't, to free them at once.
    empty_items: Vec<bool>,
    /// Which of those are strings held as their headers, which hold no block when every one is
    /// inline, so a list of them all inline is freed at once too.
    text_items: Vec<bool>,
    /// How many words an item of each kind takes, worked out once rather than at each new list.
    /// A worker's heap has none, and works it out.
    item_words: Vec<usize>,
    /// The number of the thread this heap is, in a shape word's owner bits: 0 on the main
    /// thread, so its blocks' shape words are just their shapes.
    owner_bits: u64,
    /// On a worker, the blocks it made since it last handed what it made over.
    made: Vec<u64>,
    /// Whether this heap serves connections, whose tasks hand nothing over.
    pub(super) serving: bool,
    /// The tables behind the current one while this thread holds write locks, each with the owner
    /// number it makes blocks with: its own first (see `guarded`).
    behind: Vec<(Box<super::table::Table>, u64)>,
    /// The tables this thread holds read locks on.
    pub(super) reading: Vec<std::sync::Arc<super::table::Table>>,
    /// How many words each shape's blocks take, by the shape's number.
    sizes: Vec<usize>,
    /// On a worker running a range, the blocks of the thread that made the `parallel` call,
    /// which is waiting for it: read here, and never written (see `threads`).
    pub(super) lent: Option<std::sync::Arc<super::table::Table>>,
    /// How this worker's range has changed the owners of lent blocks, for that thread to apply.
    pub(super) lent_owners: HashMap<u64, i64, BuildHasherDefault<AddressHasher>>,
    /// Words copied out of one block on the way to another, kept to save making them anew.
    pub(super) scratch: Vec<u64>,
    /// Where blocks freed now go (see `give_back`).
    freed: Freed,
}

/// Where a heap puts the blocks it frees.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Freed {
    /// Into `spare`, for the next blocks it makes.
    #[default]
    Kept,
    /// Back to the allocator.
    GivenBack,
}

/// Every worker's counts, kept for as long as the run, whether its thread has ended or is still
/// running, as a server's or a pool's do, so a run's report holds what its tasks cost too.
static WORKER_COUNTS: std::sync::Mutex<Vec<std::sync::Arc<std::sync::Mutex<Counts>>>> =
    std::sync::Mutex::new(Vec::new());

/// What every worker that counts has cost so far, all together.
pub(super) fn worker_counts() -> Counts {
    let slots = WORKER_COUNTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut all = Counts::default();
    for slot in slots.iter() {
        all.add(&slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
    }
    all
}

/// Whether heaps made from now on count what they allocate, for a caller that reads `Counts`
/// back: `fennec dev copies --run` does. A run under `FENNEC_COUNTS` counts too.
static COUNTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The string literals the program's own thread made, which live for the whole run and never
/// change, where every thread can read them: once a task runs beside that thread, it can't read
/// that thread's table (see `share_statics`). A program shares once. Tests, each starting a
/// runtime of its own, take a place each.
static SHARED_STATICS: [std::sync::OnceLock<super::table::Table>; SHARES] =
    [const { std::sync::OnceLock::new() }; SHARES];

/// How many runtimes can share their literals in one process.
const SHARES: usize = 64;

/// How many places in `SHARED_STATICS` are taken.
static SHARED_PLACES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The block at `at` among the literals every thread can read, if it's one.
fn shared_block(at: usize) -> Option<&'static [u64]> {
    let taken = SHARED_PLACES.load(std::sync::atomic::Ordering::Acquire).min(SHARES);
    SHARED_STATICS.iter().take(taken).find_map(|shared| shared.get()?.get(at))
}

/// Moves the blocks this thread made to live for the whole run, its string literals and constants
/// among them, where every thread can read them, once. Native code holds their addresses, which don't change.
/// For the program's own thread, before a task first runs beside it: a parked task elsewhere reads
/// a literal it prints there.
#[cfg_attr(not(any(test, feature = "threads")), expect(dead_code, reason = "only launched tasks use it"))]
pub(super) fn share_statics(state: &mut State) {
    if state.heap.shared_statics {
        return;
    }
    state.heap.shared_statics = true;
    let mut shared = super::table::Table::default();
    let mut kept = super::table::Table::default();
    let mut moved = 0;
    for block in std::mem::take(&mut state.heap.blocks).into_blocks() {
        // Never freed by its count, or by its owner, whatever its count has come to.
        if block.first().is_some_and(|count| *count >= IMMORTAL)
            || block.get(1).is_some_and(|shape| *shape >> OWNER_SHIFT == NEVER_FREED_OWNER)
        {
            shared.insert(block);
            moved += 1;
        } else {
            kept.insert(block);
        }
    }
    state.heap.blocks = kept;
    state.heap.statics = state.heap.statics.saturating_sub(moved);
    state.heap.forget_cursor();
    let place = SHARED_PLACES.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    let set = match SHARED_STATICS.get(place) {
        Some(slot) => slot.set(shared),
        None => Err(shared),
    };
    // No place left: they go back, for only this thread to read.
    if let Err(shared) = set {
        for block in shared.into_blocks() {
            state.heap.blocks.insert(block);
        }
        state.heap.statics += moved;
    }
}

/// Whether a block is one of the program's string literals or constants every thread can read.
pub(super) fn shared_static(address: u64) -> bool {
    usize::try_from(address).is_ok_and(|at| shared_block(at).is_some())
}

/// `fennec_constant_kept(block)`: a constant's value, worked out once, lives for the whole run, and
/// so does every block it holds. Marked so, a task on another thread can read it, and nothing frees
/// or changes it in place.
pub extern "C" fn constant_kept(address: i64) {
    with(|state| {
        let mut waiting = vec![address.cast_unsigned()];
        while let Some(at) = waiting.pop() {
            let Some(block) = state.heap.block_mut(at) else { continue };
            let Some(count) = block.first_mut() else { continue };
            if *count >= IMMORTAL {
                continue;
            }
            // Its owners now stay far above the mark, so letting one go doesn't take it below.
            *count = count.saturating_add(NEVER_FREED_COUNT);
            mark_never_freed(block);
            if let Some(block) = state.heap.block(at) {
                waiting.extend(children(state, block));
            }
        }
    });
}

/// Has every run from now on count what it allocates.
pub fn count_allocations() {
    COUNTING.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// What a run of native code allocated and copied. Blocks that live for the whole run, and the
/// `Value`s the runtime holds for it, aren't counted. Blocks are made and copied in the runtime,
/// which does far more than add to these, so counting costs a run nothing it would notice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub allocations: u64,
    pub allocated_bytes: u64,
    /// Shared values copied so that one owner could change them.
    pub copies: u64,
    pub copied_bytes: u64,
    pub largest_copy: u64,
}

impl Counts {
    fn allocated(&mut self, bytes: u64) {
        self.allocations += 1;
        self.allocated_bytes += bytes;
    }

    /// Adds `other`'s counts to these, the largest copy being the larger of the two.
    pub(super) fn add(&mut self, other: &Counts) {
        self.allocations += other.allocations;
        self.allocated_bytes += other.allocated_bytes;
        self.copies += other.copies;
        self.copied_bytes += other.copied_bytes;
        self.largest_copy = self.largest_copy.max(other.largest_copy);
    }

    /// Notes that a shared value of `words` words was copied.
    pub(super) fn copied(&mut self, words: usize) {
        let bytes = u64::try_from(words).unwrap_or(0) * 8;
        self.copies += 1;
        self.copied_bytes += bytes;
        self.largest_copy = self.largest_copy.max(bytes);
    }
}

/// The shape of a cell: a block holding a `Value` for a boxed value's field.
const CELL_SHAPE: u64 = SHAPE_BITS - 1;

/// Where a block's shape word holds the number of the thread that made it: its top 16 bits,
/// which are 0 for the main thread's blocks. Native code counts a block it didn't make
/// through the runtime (see `threads`).
pub const OWNER_SHIFT: u32 = 48;

/// Where a block's shape word holds which task on its thread made it, in the bits below the thread.
const TASK_SHIFT: u32 = 24;

/// The bits of a shape word that say which task made a block.
const TASK_BITS: u64 = ((1 << OWNER_SHIFT) - 1) & !SHAPE_BITS;

/// The bits of a shape word that say the shape.
pub const SHAPE_BITS: u64 = (1 << TASK_SHIFT) - 1;

/// Whether a block is a `shared` value's own, by the number in its shape word.
pub(super) fn is_handle_block(block: &[u64]) -> bool {
    block.get(1).is_some_and(|word| word >> OWNER_SHIFT == super::guarded::HANDLE_OWNER)
}

/// A block's shape, without the thread that made it.
pub(crate) fn shape_word(block: &[u64]) -> Option<u64> {
    block.get(1).map(|word| word & SHAPE_BITS)
}

/// A task's mark for the blocks it makes, from its number on its thread, never 0, which is what a
/// thread makes outside any task. Two tasks alive together could only share one after one of them
/// has seen 16 million others start on its thread.
#[cfg(any(test, feature = "threads"))]
pub(super) fn task_mark(task: u64) -> u64 {
    ((task % (TASK_BITS >> TASK_SHIFT)) + 1) << TASK_SHIFT
}

/// The owner in the shape word of a block that's never freed, like a string literal's or a
/// constant's: no thread's. Only a block's own thread may read or change its count, which it does
/// without atomics, so another thread can't read the count to see that it's never freed: it reads
/// this, in the shape word, which doesn't change once the block's in use.
pub const NEVER_FREED_OWNER: u64 = 0xFFFD;

/// Whether a block is never freed: by its owner, or by a count no thread's owners can reach.
#[cfg(any(test, feature = "threads", feature = "audit"))]
pub(super) fn is_never_freed(block: &[u64]) -> bool {
    block.first().is_some_and(|count| *count >= IMMORTAL)
        || block.get(1).is_some_and(|shape| *shape >> OWNER_SHIFT == NEVER_FREED_OWNER)
}

/// Marks a block as never freed, by its owner.
pub(super) fn mark_never_freed(block: &mut [u64]) {
    mark_owner(block, NEVER_FREED_OWNER << OWNER_SHIFT);
}

/// Sets the thread and task a block was made by, in its shape word.
pub(super) fn mark_owner(block: &mut [u64], owner_bits: u64) {
    if let Some(word) = block.get_mut(1) {
        *word = (*word & SHAPE_BITS) | owner_bits;
    }
}

/// The most words a block may be asked for: far past what any machine has, and small enough that
/// asking for it in bytes doesn't wrap around, on 32-bit targets too.
const MOST_WORDS: usize = usize::MAX >> 16;

/// How many words a block needs for `header` words and `count` items of `each` words. More than can
/// ever exist stops the program, as nothing could give it the room.
pub(super) fn words_for(header: usize, count: usize, each: usize) -> usize {
    match count.checked_mul(each).and_then(|words| words.checked_add(header)) {
        Some(words) if words <= MOST_WORDS => words,
        _ => too_big(),
    }
}

thread_local! {
    /// The blocks generated code made itself and hasn't freed, by address, in a build that watches
    /// them: a dev build, or one checking proofs (see the LLVM backend's `native` module). The
    /// runtime must never be given one, and does nothing with an address it holds no block for, so
    /// without this a mistake there would leak or corrupt what the block holds, silently. Each
    /// thread knows the ones it made.
    static NATIVE: RefCell<std::collections::HashSet<u64, BuildHasherDefault<AddressHasher>>> =
        RefCell::new(std::collections::HashSet::default());
}

/// Whether a build watches native blocks: none is ever looked up in one that doesn't.
static WATCHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `fennec_native_made(block)`: generated code made a block itself, in a build that watches them.
pub extern "C" fn native_made(address: i64) {
    WATCHING.store(true, std::sync::atomic::Ordering::Relaxed);
    NATIVE.with_borrow_mut(|made| made.insert(address.cast_unsigned()));
}

/// `fennec_native_freed(block)`: generated code freed a block it made, in a build that watches them.
pub extern "C" fn native_freed(address: i64) {
    NATIVE.with_borrow_mut(|made| made.remove(&address.cast_unsigned()));
}

/// Stops the program if `address`, which the runtime holds no block for, is a block generated
/// code made, saying what it was asked to do with it: a bug in the compiler, which would
/// otherwise leak or corrupt what the block holds. Only a build that watches them can tell.
pub(in crate::embed) fn not_native(address: u64, what: &str) {
    if WATCHING.load(std::sync::atomic::Ordering::Relaxed)
        && NATIVE.with_borrow(|made| made.contains(&address))
    {
        unowned(what);
    }
}

/// Stops the program: the runtime was handed a block generated code made. This is reached with
/// the runtime's state borrowed, so it says so itself.
#[cfg(not(test))]
#[cold]
#[inline(never)]
fn unowned(what: &str) -> ! {
    super::write_kept_output();
    let _ =
        writeln!(std::io::stderr(), "error: the runtime was handed a native block it doesn't own: {what}");
    super::aot::stop_now(1)
}

/// [`unowned`] in a test, which goes on to be caught, where a program stops.
#[cfg(test)]
fn unowned(what: &str) -> ! {
    panic!("the runtime was handed a native block it doesn't own: {what}");
}

/// Stops the program: the runtime found `what`, a bug in Fennec, which going on would hide. This
/// is reached with the runtime's state borrowed, so it says so itself.
#[cfg(not(test))]
#[cold]
#[inline(never)]
pub(in crate::embed) fn runtime_bug(what: &str) -> ! {
    super::write_kept_output();
    let _ = writeln!(std::io::stderr(), "error: {what}\n  = help: this is a bug in Fennec; please report it");
    super::aot::stop_now(1)
}

/// [`runtime_bug`] in a test, which goes on to be caught, where a program stops.
#[cfg(test)]
pub(in crate::embed) fn runtime_bug(what: &str) -> ! {
    panic!("{what}");
}

thread_local! {
    /// The lowest place on this thread's stack the runtime may read a value into a tree from: 0
    /// where nothing has said, as in a test, which is no limit.
    static DEEPEST_READ: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How much of a parked task's stack is kept clear below a read: what the read's own caller and
/// a failure's way out still need.
#[cfg(any(test, feature = "threads"))]
const TASK_STACK_KEPT: usize = 64 << 10;

/// Says how low on this thread's stack the runtime may still read a value into a tree: for the
/// program's own thread and each worker, some way below where native code may go. A parked task's
/// stack says for itself.
pub(in crate::embed) fn reads_may_reach(lowest: usize) {
    DEEPEST_READ.set(lowest);
}

/// Whether there's stack left here to read one more block into a tree, which calls itself for
/// each block the value holds.
fn room_to_read() -> bool {
    let here = 0_u8;
    let at = std::ptr::from_ref(&here).addr();
    #[cfg(any(test, feature = "threads"))]
    if let Some(low) = fennec_parked::stack_low() {
        return at > low + TASK_STACK_KEPT;
    }
    at > DEEPEST_READ.get()
}

/// Stops the program: a value is nested deeper than there's stack left to read it into a tree, as
/// hashing a dict's key that holds blocks does. Comparing and printing read nothing into a tree,
/// and take any depth. This is reached with the runtime's state borrowed, so it says so itself.
#[cold]
#[inline(never)]
fn too_deep() -> ! {
    super::write_kept_output();
    let _ = writeln!(
        std::io::stderr(),
        "error: a value is nested too deep to be a dict's key or a set's item: it holds values \
         inside values further down than there's stack to follow them"
    );
    super::aot::stop_now(1)
}

/// Stops the program: it asked for a block too big to exist. This is reached with the runtime's
/// state borrowed, so it says so itself.
pub(super) fn too_big() -> ! {
    super::write_kept_output();
    let _ = writeln!(std::io::stderr(), "error: this is too big to make: there isn't that much memory");
    super::aot::stop_now(1)
}

/// The shape of a capability's block (see `super::capabilities`).
pub const CAPABILITY_SHAPE: u64 = SHAPE_BITS - 2;

/// The shape of a block of `bytes` (see `super::bytes`).
pub const BYTES_SHAPE: u64 = SHAPE_BITS - 3;

/// Words a cell takes: owner count, shape and the number of its value.
const CELL_WORDS: usize = 3;

/// Which word of a generator's block says where it goes on from, and what says it's finished.
const GENERATOR_PC: usize = (super::layout::GEN_PC / 8).unsigned_abs() as usize;
const FINISHED: u64 = u64::MAX;

/// The owner count of a block that's never freed: retaining and releasing it never get
/// near zero.
pub const IMMORTAL: u64 = 1 << 48;

/// Where a block that's never freed starts counting: 2^62, 2^14 times [`IMMORTAL`], as a shared
/// enum case's does. Its count isn't kept exactly: native code that may run on a worker leaves a
/// never-freed block's count alone wherever it runs, while code that runs only on the main thread
/// counts it, so one taking an owner and the other giving it back moves the count a little each
/// time. Starting this far above the mark, no run moves it below, or near the top.
pub const NEVER_FREED_COUNT: u64 = IMMORTAL << 14;

/// Whether this thread leaves `block`'s count alone: a block that's never freed, on a worker
/// thread. Native code on a worker doesn't count one, since it's no thread's, so the runtime there
/// doesn't either. Otherwise a worker that made one, which keeps it in its own table, would take
/// off owners its native code never gave it, until the count fell below [`IMMORTAL`] and the block
/// read as one that can be freed. The main thread's native code counts it, so its runtime does
/// too. Only a count at the mark or above asks more, so other blocks pay nothing.
pub(super) fn uncounted(block: &[u64]) -> bool {
    super::threads::ON
        && block.first().is_some_and(|count| *count >= IMMORTAL)
        && block.get(1).is_some_and(|word| word >> OWNER_SHIFT == NEVER_FREED_OWNER)
        && super::threads::index() != 0
}

/// How many words of freed blocks a heap keeps for reuse, across all sizes: 8 MB. Reuse is only
/// a little cheaper than the allocator, which needs this much to match on programs that free a
/// lot at once. More just keeps memory from the rest of the program, with blocks scattered
/// through allocator and table pages that then can't be given back.
const SPARE_TOTAL_WORDS: usize = 1 << 20;

/// How many words of freed blocks of any one size a heap keeps: 1 MB.
const SPARE_SIZE_WORDS: usize = 1 << 17;

/// The biggest block, in words, kept for reuse.
const SPARE_WORDS: usize = 256;

/// Zeroes a block, with plain stores for the small blocks most are, which costs less than
/// calling `memset`.
fn zero(block: &mut [u64]) {
    let zeroed = match block.len() {
        2 => zero_words::<2>(block),
        3 => zero_words::<3>(block),
        4 => zero_words::<4>(block),
        5 => zero_words::<5>(block),
        6 => zero_words::<6>(block),
        _ => false,
    };
    if !zeroed {
        block.fill(0);
    }
}

/// Zeroes a block of `N` words, if it is that long.
fn zero_words<const N: usize>(block: &mut [u64]) -> bool {
    <&mut [u64; N]>::try_from(block).map(|words| *words = [0; N]).is_ok()
}

/// Whether every item of a list of strings' headers is inline, holding its text in itself: then
/// the list holds no block. A header's last byte has the inline mark when it's inline.
fn all_inline(block: &[u64]) -> bool {
    let start = super::lists::LIST_HEADER_WORDS;
    let count = block.get(2).and_then(|count| usize::try_from(*count).ok()).unwrap_or(usize::MAX);
    let Some(items) = count.checked_mul(3).and_then(|words| block.get(start..start + words)) else {
        return false;
    };
    let inline = u64::from(super::text::INLINE) << 56;
    items.as_chunks::<3>().0.iter().all(|[_, _, last]| last & inline != 0)
}

impl Heap {
    /// Whether a list of this kind of item is known to hold no blocks. A worker's heap has no
    /// plans, so it doesn't know, and looks.
    /// How many words an item of kind `number` takes, if this heap worked it out.
    pub(super) fn item_words(&self, number: usize) -> Option<usize> {
        self.item_words.get(number).copied()
    }

    fn items_hold_nothing(&self, number: usize) -> bool {
        self.empty_items.get(number).copied().unwrap_or(false)
    }

    /// A heap holding the blocks that live for the whole run.
    /// The heap, knowing where blocks of each shape hold addresses, for freeing them.
    pub(crate) fn planned(mut self, shapes: &[Rc<Element>], elements: &[Rc<Element>]) -> Heap {
        self.plans = shapes.iter().map(|shape| super::plans::plan(shape)).collect();
        self.item_plans = elements.iter().map(|element| super::plans::item_plan(element)).collect();
        self.empty_items = self.item_plans.iter().map(|items| items.plan.holds_nothing()).collect();
        self.text_items = elements.iter().map(|element| **element == Element::Text).collect();
        self.item_words = elements.iter().map(|element| element.words()).collect();
        self.sizes = shapes.iter().map(|shape| HEADER_WORDS + shape.words()).collect();
        self
    }

    pub(crate) fn new(statics: Vec<Box<[u64]>>) -> Heap {
        let owner_bits = u64::from(super::threads::index()) << OWNER_SHIFT;
        let counting = cfg!(test)
            || COUNTING.load(std::sync::atomic::Ordering::Relaxed)
            || std::env::var_os("FENNEC_COUNTS").is_some();
        let mut heap = Heap { statics: statics.len(), owner_bits, counting, ..Heap::default() };
        for block in statics {
            heap.insert(block);
        }
        heap.counts = Counts::default();
        if counting && owner_bits != 0 {
            let slot = std::sync::Arc::new(std::sync::Mutex::new(Counts::default()));
            WORKER_COUNTS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(std::sync::Arc::clone(&slot));
            heap.shared_counts = Some(slot);
        }
        heap
    }

    /// Keeps a worker's counts where the run's report reads them.
    fn publish_counts(&self) {
        if let Some(slot) = &self.shared_counts {
            *slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = self.counts;
        }
    }

    /// Takes blocks made after the run started that live for the rest of it, like an
    /// executable's string literals.
    pub(crate) fn adopt(&mut self, statics: &mut Vec<Box<[u64]>>) {
        self.statics += statics.len();
        let (counts, made) = (self.counts, self.made.len());
        for block in statics.drain(..) {
            self.insert(block);
        }
        self.counts = counts;
        self.publish_counts();
        // They stay this thread's for the whole run, so a range never hands them over.
        self.made.truncate(made);
    }

    /// A zeroed block of `words` words, reusing a freed one when there is one.
    pub(super) fn fresh(&mut self, words: usize) -> Box<[u64]> {
        self.fresh_written(words, 0)
    }

    /// A block of `words` words whose first `written` the caller writes straight away: those of a
    /// freed one reused are left as they were, and the rest are zero.
    pub(super) fn fresh_written(&mut self, words: usize, written: usize) -> Box<[u64]> {
        match self.spare.get_mut(words).and_then(Vec::pop) {
            Some(mut block) => {
                self.spare_words -= block.len();
                match block.get_mut(written..) {
                    // What's left after what's written is a few words, mostly, for plain stores.
                    Some(rest) if written > 0 => zero(rest),
                    _ => zero(&mut block),
                }
                block
            }
            None => vec![0; words].into_boxed_slice(),
        }
    }

    /// Says where blocks freed from now on go, giving back where they went before. They should go
    /// back to the allocator while letting go of blocks another thread made, which this one won't
    /// make more of: kept here, they'd never be reused, and would hold that thread's memory from
    /// it. Given back, the thread that made them takes their memory back when it next makes some.
    #[cfg_attr(
        not(any(test, feature = "threads")),
        expect(dead_code, reason = "only a launched task frees what another thread made")
    )]
    pub(super) fn freeing(&mut self, freed: Freed) -> Freed {
        std::mem::replace(&mut self.freed, freed)
    }

    /// Keeps a freed block for reuse, when it's small: big ones, like a long list's, are
    /// rare enough to give back.
    pub(super) fn recycle(&mut self, block: Box<[u64]>) {
        // Under AddressSanitizer every freed block is given back, for it to watch.
        if cfg!(feature = "sanitize") || self.freed == Freed::GivenBack {
            return;
        }
        if self.spare_words + block.len() > SPARE_TOTAL_WORDS {
            return;
        }
        let words = block.len();
        match self.spare.get_mut(words) {
            // Each size gets a share, so one nothing reuses, like most strings', can't take all of it.
            Some(spare) if (spare.len() + 1) * words > SPARE_SIZE_WORDS => {}
            Some(spare) => {
                spare.push(block);
                self.spare_words += words;
            }
            None => self.recycle_first(block),
        }
    }

    /// `recycle`, for the first block of its size.
    #[cold]
    #[inline(never)]
    fn recycle_first(&mut self, block: Box<[u64]>) {
        let words = block.len();
        if words > SPARE_WORDS {
            return;
        }
        if self.spare.len() <= words {
            self.spare.resize_with(words + 1, Vec::new);
        }
        if let Some(spare) = self.spare.get_mut(words) {
            spare.push(block);
            self.spare_words += words;
        }
    }

    /// Its live blocks, by how many words each takes, and apart from them how many are never freed.
    #[cfg(any(test, feature = "threads"))]
    pub(crate) fn live_by_size(&self) -> (std::collections::BTreeMap<usize, usize>, usize) {
        self.blocks.sizes()
    }

    /// How many blocks are left that a run could still have freed: what a run that ends with any
    /// left has leaked. Blocks kept for the whole run aren't among them, whenever they were made:
    /// `live` leaves out only those made as it starts.
    pub(crate) fn leftover(&self) -> usize {
        self.blocks.freeable()
    }

    /// How many blocks are live but those made as it started, for tests.
    #[cfg(test)]
    pub(crate) fn live(&self) -> usize {
        self.blocks.len().saturating_sub(self.statics)
    }

    /// Native code is about to do what's at this place in the source.
    pub(super) fn trace_at(&mut self, site: u32) {
        self.site = Some(site);
    }

    /// Whether this heap counts what's allocated.
    pub(super) fn counting(&self) -> bool {
        self.counting
    }

    /// Notes that `bytes` bytes were allocated, when anything will read it: every block made
    /// comes here, so the rest is out of its way.
    #[inline]
    pub(super) fn allocated(&mut self, bytes: u64) {
        if self.counting || self.site.is_some() {
            self.count_allocated(bytes);
        }
    }

    #[cold]
    #[inline(never)]
    fn count_allocated(&mut self, bytes: u64) {
        self.counts.allocated(bytes);
        self.publish_counts();
        if let Some(site) = self.site {
            self.sites.entry(site).or_default().allocated(bytes);
        }
    }

    /// Notes that a shared value of `words` words was copied.
    pub(super) fn copied(&mut self, words: usize) {
        self.counts.copied(words);
        self.publish_counts();
        if let Some(site) = self.site {
            self.sites.entry(site).or_default().copied(words);
        }
    }

    pub(crate) fn insert(&mut self, mut block: Box<[u64]>) -> i64 {
        self.allocated(u64::try_from(block.len()).unwrap_or(0) * 8);
        #[cfg(feature = "audit")]
        audit::made_one();
        let address = block.as_ptr().addr();
        if block.first().is_some_and(|count| *count >= IMMORTAL) {
            // It's no thread's to count, and never handed over.
            mark_never_freed(&mut block);
        } else if self.owner_bits >> OWNER_SHIFT != 0 {
            mark_owner(&mut block, self.owner_bits);
            // What's made under a lock stays with what the lock guards, and a server's tasks hand
            // nothing over.
            if super::threads::ON
                && !self.serving
                && self.owner_bits >> OWNER_SHIFT != super::guarded::GUARDED_OWNER
            {
                self.made.push(u64::try_from(address).unwrap_or(0));
            }
        }
        self.blocks.insert(block);
        i64::try_from(address).unwrap_or(0)
    }

    /// How many blocks this heap has made since it last handed them over, to hand over only
    /// the ones made after this.
    pub(crate) fn made_so_far(&self) -> usize {
        self.made.len()
    }

    /// Puts `made` in place of the blocks this heap's made since it last handed them over, and
    /// those in `made`'s place: for a parked range, which keeps its own.
    #[cfg(any(test, feature = "threads"))]
    pub(super) fn swap_made(&mut self, made: &mut Vec<u64>, task: &mut u64) {
        std::mem::swap(&mut self.made, made);
        // The main thread's blocks hold no thread's number, and it doesn't hand any over.
        if self.owner_bits >> OWNER_SHIFT != 0 {
            let was = self.owner_bits & TASK_BITS;
            self.owner_bits = (self.owner_bits & !TASK_BITS) | *task;
            *task = was;
        }
    }

    /// Takes every block this heap made after it had made `since`, and that doesn't live for
    /// the whole run, for another thread's heap to adopt: what a task made that outlives it.
    pub(crate) fn hand_over(&mut self, since: usize) -> Vec<Box<[u64]>> {
        let mut handed = Vec::new();
        let task = self.owner_bits & TASK_BITS;
        for address in self.made.split_off(since.min(self.made.len())) {
            // An address it freed may have come back as another task's block.
            let block = self.block(address);
            if block.and_then(|block| block.get(1)).is_none_or(|word| word & TASK_BITS != task) {
                continue;
            }
            let lives = block.and_then(<[u64]>::first).is_some_and(|count| *count < IMMORTAL);
            self.forget_place(address);
            if lives && let Some(block) = usize::try_from(address).ok().and_then(|at| self.blocks.remove(at))
            {
                // Its address can come back holding another string.
                self.texts.get_mut().remove(&block.as_ptr().addr());
                handed.push(block);
            }
        }
        handed
    }

    /// Takes over blocks, as this thread's own and as made since it last handed any over: what a
    /// launched task was given to start with, or left when it ended, for whoever runs next to hand
    /// over in turn.
    /// How many pages its table of blocks holds.
    #[cfg(any(test, feature = "threads"))]
    pub(crate) fn table_pages(&self) -> usize {
        self.blocks.pages()
    }

    /// How many words of freed blocks it's keeping for reuse.
    #[cfg(any(test, feature = "threads"))]
    pub(crate) fn spare_words(&self) -> usize {
        self.spare_words
    }

    pub(crate) fn adopt_made(&mut self, blocks: impl IntoIterator<Item = Box<[u64]>>) {
        let counts = self.counts;
        for mut block in blocks {
            mark_owner(&mut block, self.owner_bits);
            self.insert(block);
        }
        self.counts = counts;
        self.publish_counts();
    }

    /// Takes over blocks another thread's heap handed over, as this thread's own.
    pub(crate) fn adopt_blocks(&mut self, blocks: Vec<Box<[u64]>>) {
        for mut block in blocks {
            mark_owner(&mut block, self.owner_bits);
            self.blocks.insert(block);
        }
    }

    /// Takes a block out of the heap for reuse, leaving the blocks it refers to as they are:
    /// for a block whose contents moved to a new one.
    pub(super) fn discard(&mut self, address: u64) {
        if let Some(block) = self.take_out(address) {
            self.recycle(block);
        }
    }

    /// Takes a block of this thread's out of the heap, for its contents to move to a new one.
    pub(super) fn take_out(&mut self, address: u64) -> Option<Box<[u64]>> {
        self.forget_place(address);
        self.blocks.remove(usize::try_from(address).ok()?)
    }

    /// Where a string that isn't ASCII was last read.
    pub(super) fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// Forgets where a string was last read, when that block goes: its address can come back
    /// as another string. Only what may be a string comes here, so other blocks don't pay.
    pub(super) fn forget_place(&mut self, address: u64) {
        if usize::try_from(address).is_ok_and(|at| self.cursor.address == at) {
            self.set_cursor(Cursor::default());
        }
    }

    pub(super) fn set_cursor(&mut self, cursor: Cursor) {
        self.cursor = cursor;
        self.blocks.note_place(cursor.address);
    }

    pub(crate) fn block(&self, address: u64) -> Option<&[u64]> {
        let at = usize::try_from(address).ok()?;
        let own = self.blocks.get(at);
        if own.is_none() {
            return self.elsewhere(at);
        }
        own
    }

    /// A block in a table behind the current one, one this thread holds to read, or one lent for the
    /// `parallel` call this thread is running a range of, if it's any of those.
    #[cold]
    #[inline(never)]
    fn elsewhere(&self, at: usize) -> Option<&[u64]> {
        self.behind
            .iter()
            .rev()
            .find_map(|(table, _)| table.get(at))
            .or_else(|| self.reading.iter().rev().find_map(|table| table.get(at)))
            .or_else(|| self.lent.as_ref()?.get(at))
            .or_else(|| shared_block(at))
    }

    /// Makes a `shared` value's table the current one, with what's made in it owned as `owner_bits`
    /// say. Its box goes on holding the table that was current.
    pub(super) fn enter_table(&mut self, mut table: Box<super::table::Table>, owner_bits: u64) {
        self.forget_cursor();
        super::guarded::rollback::tables_changing();
        std::mem::swap(&mut self.blocks, &mut table);
        self.behind.push((table, self.owner_bits));
        super::guarded::rollback::tables_are(self.behind.len());
        self.owner_bits = owner_bits;
    }

    /// How many tables are behind the current one: one for each write lock held with a table.
    pub(super) fn tables_behind(&self) -> usize {
        self.behind.len()
    }

    /// Whether the block at `address` is in this heap's own table or one behind it: one that goes
    /// when this heap does.
    pub(super) fn holds_block(&self, address: usize) -> bool {
        self.blocks.get(address).is_some()
            || self.behind.iter().any(|(table, _)| table.get(address).is_some())
    }

    /// Takes the current table back out, leaving the one behind it current.
    #[expect(
        clippy::unnecessary_box_returns,
        reason = "the box goes back to be reused, so a lock doesn't move a table"
    )]
    pub(super) fn leave_table(&mut self) -> Box<super::table::Table> {
        self.forget_cursor();
        super::guarded::rollback::tables_changing();
        let Some((mut table, owner_bits)) = self.behind.pop() else { return Box::default() };
        super::guarded::rollback::tables_are(self.behind.len());
        self.owner_bits = owner_bits;
        std::mem::swap(&mut self.blocks, &mut table);
        table
    }

    fn forget_cursor(&mut self) {
        if self.cursor.address != 0 {
            self.set_cursor(Cursor::default());
        }
    }

    /// Swaps the current table with the one behind the top `held`, for a copy to go there, and back
    /// again.
    pub(super) fn swap_behind(&mut self, held: usize) {
        let Heap { blocks, owner_bits, behind, .. } = self;
        if let Some((table, bits)) = behind.len().checked_sub(held).and_then(|at| behind.get_mut(at)) {
            std::mem::swap(blocks, &mut **table);
            std::mem::swap(owner_bits, bits);
        }
    }

    /// Which of the tables behind the current one holds this block.
    pub(super) fn behind_holding(&self, at: usize) -> Option<usize> {
        self.behind.iter().position(|(table, _)| table.get(at).is_some())
    }

    /// Swaps the current table with one behind it, which holds a lock's too: for each of several held
    /// to write to be current in turn, and back again.
    pub(super) fn swap_table(&mut self, number: usize) {
        let Heap { blocks, behind, .. } = self;
        if let Some((table, _)) = behind.get_mut(number) {
            std::mem::swap(blocks, &mut **table);
        }
    }

    pub(super) fn lent_has(&self, address: u64) -> bool {
        usize::try_from(address).is_ok_and(|at| self.lent.as_ref().is_some_and(|lent| lent.get(at).is_some()))
    }

    /// Takes a list out of this thread's own table, not one behind it, if its items hold nothing and
    /// it isn't a `shared` value's handle, looking it up once. Lists' shapes start at `first_list`.
    /// `None`, having taken nothing, for any other block.
    pub(super) fn take_plain_list(
        &mut self,
        at: usize,
        first_list: u64,
        str_shape: u64,
    ) -> Option<Box<[u64]>> {
        let (empty_items, text_items) = (&self.empty_items, &self.text_items);
        self.blocks.take_if(at, |block| {
            // A string, which holds no other block, or a list of a kind whose items hold nothing,
            // or one with no items at all, as a list made empty and never added to is, or one of
            // strings' headers all inline, as a `split` into short parts makes. Only a kind of
            // list's shape is looked at: past them are other blocks, like cells, whose third word
            // means something else.
            !is_handle_block(block)
                && shape_word(block).is_some_and(|shape| {
                    shape == str_shape
                        || shape
                            .checked_sub(first_list)
                            .and_then(|number| usize::try_from(number).ok())
                            .is_some_and(|number| {
                                empty_items.get(number).is_some_and(|empty| *empty)
                                    || block.get(2) == Some(&0)
                                    || (text_items.get(number).is_some_and(|text| *text) && all_inline(block))
                            })
                })
        })
    }

    /// Takes a block out of the current table or one behind it.
    pub(super) fn take_block(&mut self, at: usize) -> Option<Box<[u64]>> {
        let found = self.blocks.remove_any(at);
        if found.is_some() || self.behind.is_empty() {
            return found;
        }
        self.behind.iter_mut().rev().find_map(|(table, _)| table.remove_any(at))
    }

    pub(super) fn owner_bits(&self) -> u64 {
        self.owner_bits
    }

    /// The number native code knows this thread by now.
    pub(super) fn owner_number(&self) -> i64 {
        i64::try_from(self.owner_bits >> OWNER_SHIFT).unwrap_or(0)
    }

    /// Whether `block`, the block at `address`, has one owner and is this thread's to change: a
    /// lent block never is, however many owners it has.
    pub(super) fn is_alone(&self, address: u64, block: &[u64]) -> bool {
        block.first() == Some(&1)
            && (self.holds_all() || usize::try_from(address).is_ok_and(|at| self.blocks.get(at).is_some()))
    }

    /// Whether every block this thread can reach is in its own table: nothing's lent to it, read
    /// under a lock or left behind one. Then a block with one owner is always its own to change.
    pub(super) fn holds_all(&self) -> bool {
        self.lent.is_none() && self.reading.is_empty() && self.behind.is_empty()
    }

    /// Notes that this thread's range changed a lent block's owners by `by`, when it's a lent
    /// block, for the thread that lent it to apply when the call ends.
    pub(super) fn change_lent_owners(&mut self, address: u64, by: i64) -> bool {
        if !super::threads::ON {
            return false;
        }
        // What's kept past a read lock is copied out, so what it does here comes to nothing.
        let at = usize::try_from(address).unwrap_or(0);
        if self.reading.iter().any(|table| table.get(at).is_some()) {
            return true;
        }
        let Some(block) = self.lent.as_ref().and_then(|lent| lent.get(at)) else { return false };
        // A worker leaves the count of a block that's never freed alone, as its native code does,
        // so `threads::settle` never has a change to one to make on the thread that lent it.
        if !uncounted(block) {
            *self.lent_owners.entry(address).or_default() += by;
        }
        true
    }

    pub(super) fn block_mut(&mut self, address: u64) -> Option<&mut [u64]> {
        let at = usize::try_from(address).ok()?;
        if self.behind.is_empty() {
            return self.blocks.get_mut(at);
        }
        // What's behind a lock is this thread's too, like a closure's capture read under it.
        let Heap { blocks, behind, .. } = self;
        match blocks.get_mut(at) {
            Some(block) => Some(block),
            None => behind.iter_mut().rev().find_map(|(table, _)| table.get_mut(at)),
        }
    }
}

fn shape_of(state: &State, block: &[u64]) -> Option<Rc<Element>> {
    let shape = usize::try_from(shape_word(block)?).ok()?;
    state.tables.shapes.get(shape).cloned()
}

/// Whether a block is a struct's of plain fields, which a lock's log can mark and save word by word:
/// no closure, generator or error, whose blocks hold more than their fields, and no enum.
pub(super) fn is_plain_struct(state: &State, block: &[u64]) -> bool {
    let Some(shape) = shape_word(block).filter(|shape| *shape < state.tables.str_shape) else { return false };
    let element = usize::try_from(shape).ok().and_then(|shape| state.tables.shapes.get(shape));
    matches!(
        element.map(AsRef::as_ref),
        Some(Element::Struct { fields, .. }) if !fields.iter().any(|(name, _)| name.starts_with('\0'))
    )
}

/// Where a struct's block holds others, as `child_places` finds them, from the plan its shape has.
pub(super) fn struct_places(state: &State, block: &[u64], found: &mut impl Extend<usize>) {
    let fields = block.get(HEADER_WORDS..).unwrap_or_default();
    let plan = shape_word(block)
        .and_then(|shape| usize::try_from(shape).ok())
        .and_then(|at| state.heap.plans.get(at));
    if plan.and_then(|plan| super::plans::places(plan, fields, HEADER_WORDS, found)).is_some() {
        return;
    }
    let mut walked = Vec::new();
    let _ = child_places(state, block, &mut walked);
    found.extend(walked);
}

/// What a block of a struct, enum or tuple is laid out as.
pub(super) fn element_of_block(state: &State, block: &[u64]) -> Option<Rc<Element>> {
    shape_of(state, block)
}

/// Whether a block is a string's.
pub(super) fn is_text(state: &State, block: &[u64]) -> bool {
    shape_word(block) == Some(state.tables.str_shape)
}

/// Words a string's block starts with: its owner count, shape, length in bytes, length in
/// characters, how many bytes its block has room for, and a cursor native code keeps (see
/// `layout::TEXT_CURSOR`).
pub const TEXT_HEADER_WORDS: usize = 6;

/// A string's block: owned `count` times, its bytes packed eight to a word.
pub fn text_block(shape: u64, text: &str, count: u64) -> Box<[u64]> {
    let bytes = text.as_bytes();
    let length = |n: usize| u64::try_from(n).unwrap_or(0);
    let words = bytes.len().div_ceil(8).max(1);
    let mut block = Vec::with_capacity(TEXT_HEADER_WORDS + words);
    block.extend([count, shape, length(bytes.len()), length(text.chars().count()), length(words * 8), 0]);
    // Native code reads the first word of text without checking the length.
    if bytes.is_empty() {
        block.push(0);
    }
    for chunk in bytes.chunks(8) {
        let mut word = [0; 8];
        for (to, from) in word.iter_mut().zip(chunk) {
            *to = *from;
        }
        block.push(u64::from_le_bytes(word));
    }
    block.into_boxed_slice()
}

/// A new string owned once from bytes already known to be `chars` characters of UTF-8.
pub(super) fn text_from_bytes(state: &mut State, bytes: &[u8], chars: usize) -> i64 {
    if let [byte] = bytes
        && let Some(address) = state.tables.ascii.get(usize::from(*byte))
    {
        return i64::try_from(*address).unwrap_or(0);
    }
    let words = bytes.len().div_ceil(8).max(1);
    // The header and each word with bytes in it are written here, and only an empty string's one
    // word is left to be zero.
    let mut block =
        state.heap.fresh_written(TEXT_HEADER_WORDS + words, TEXT_HEADER_WORDS + bytes.len().div_ceil(8));
    let number = |n: usize| u64::try_from(n).unwrap_or(0);
    let header = [1, state.tables.str_shape, number(bytes.len()), number(chars), number(words * 8), 0];
    if let Some(start) = block.get_mut(..TEXT_HEADER_WORDS) {
        start.copy_from_slice(&header);
    }
    for (word, chunk) in block.iter_mut().skip(TEXT_HEADER_WORDS).zip(bytes.chunks(8)) {
        let mut packed = [0; 8];
        packed.get_mut(..chunk.len()).unwrap_or_default().copy_from_slice(chunk);
        *word = u64::from_le_bytes(packed);
    }
    state.heap.insert(block)
}

/// A string's text, from its block.
pub(crate) fn block_text(block: &[u64]) -> String {
    let length = block.get(2).and_then(|n| usize::try_from(*n).ok()).unwrap_or(0);
    // Copied a word at a time, then cut to its length.
    let mut bytes = Vec::with_capacity(length.div_ceil(8) * 8);
    for word in block.get(TEXT_HEADER_WORDS..).unwrap_or_default().iter().take(length.div_ceil(8)) {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    bytes.truncate(length);
    String::from_utf8(bytes).unwrap_or_default()
}

/// The address of a string literal's block, made while compiling, which lives for the
/// whole run.
pub fn literal(tables: &mut Tables, text: &str) -> i64 {
    if let Some(address) = tables.literals.get(text) {
        return *address;
    }
    let block = text_block(tables.str_shape, text, NEVER_FREED_COUNT);
    let address = i64::try_from(block.as_ptr().addr()).unwrap_or(0);
    tables.statics.push(block);
    tables.literals.insert(text.to_owned(), address);
    address
}

/// The address of a table of the one-character strings of ASCII, by character, made the
/// first time it's needed.
pub fn ascii_table(tables: &mut Tables) -> i64 {
    if tables.ascii.is_empty() {
        let addresses: Vec<u64> = (0_u8..128)
            .map(|byte| u64::try_from(literal(tables, &char::from(byte).to_string())).unwrap_or(0))
            .collect();
        tables.ascii = addresses.into_boxed_slice();
    }
    i64::try_from(tables.ascii.as_ptr().addr()).unwrap_or(0)
}

/// The three words of a string's header, starting at `at`.
fn header_at(words: &[u64], at: usize) -> Option<[u64; 3]> {
    let [a, b, c] = *words.get(at..at + 3)? else { return None };
    Some([a, b, c])
}

/// Where each boxed field of a value sits among its words, from `at`.
fn boxed_words(element: &Element, words: &[u64], at: &mut usize, found: &mut Vec<usize>) {
    match element {
        // An optional string's header: all zero, `None`, holds nothing, as an inline one doesn't.
        element if element.is_optional_text() => boxed_words(&Element::Text, words, at, found),
        Element::Boxed(_)
        | Element::Str
        | Element::List(_)
        | Element::ListOf(_)
        | Element::Object
        | Element::Maybe(_) => {
            found.push(*at);
            *at += 1;
        }
        Element::Text => {
            if header_at(words, *at).and_then(|header| super::text::block_of(&header)).is_some() {
                found.push(*at);
            }
            *at += 3;
        }
        Element::Struct { fields, .. } => {
            for (_, field) in fields {
                boxed_words(field, words, at, found);
            }
        }
        Element::Enum { cases, words: size, .. } => {
            let start = *at;
            let case = words.get(start).and_then(|tag| usize::try_from(*tag).ok());
            *at += 1;
            for (_, field) in case.and_then(|case| cases.get(case)).map_or(&[][..], |(_, fields)| fields) {
                boxed_words(field, words, at, found);
            }
            *at = start + size;
        }
        Element::Int | Element::Float | Element::Bool | Element::U64 | Element::F32 => *at += 1,
    }
}

/// A new cell owned once holding a value.
fn new_cell(state: &mut State, value: Value) -> i64 {
    let value = value.unpacked();
    let number = state.heap.spare_cells.pop().unwrap_or_else(|| {
        state.heap.cells.push(Value::Unit);
        state.heap.cells.len() - 1
    });
    if let Some(cell) = state.heap.cells.get_mut(number) {
        *cell = value;
    }
    let mut block = state.heap.fresh(CELL_WORDS);
    if let Some(words) = block.get_mut(..CELL_WORDS) {
        words.copy_from_slice(&[1, CELL_SHAPE, u64::try_from(number).unwrap_or(0)]);
    }
    state.heap.insert(block)
}

/// The value a cell's block holds.
fn cell_value(state: &State, block: &[u64]) -> Option<Value> {
    if shape_word(block) != Some(CELL_SHAPE) {
        return None;
    }
    let number = usize::try_from(*block.get(2)?).ok()?;
    state.heap.cells.get(number).cloned()
}

/// A new cell owned once holding a temporary's value.
pub extern "C" fn cell_new(value: i64) -> i64 {
    with(|state| {
        let value = state.raw(value);
        new_cell(state, value)
    })
}

/// A temporary with the value a cell holds. The cell keeps its owners.
pub extern "C" fn cell_read(address: i64) -> i64 {
    with(|state| {
        let block = u64::try_from(address).ok().and_then(|at| state.heap.block(at));
        let value = block.and_then(|block| cell_value(state, block)).unwrap_or(Value::None);
        state.push(value)
    })
}

/// The addresses a value holds among its words, from `at`, added to `found`.
fn boxed_addresses(element: &Element, words: &[u64], at: &mut usize, found: &mut Vec<u64>) {
    match element {
        element if element.is_optional_text() => boxed_addresses(&Element::Text, words, at, found),
        Element::Boxed(_)
        | Element::Str
        | Element::List(_)
        | Element::ListOf(_)
        | Element::Object
        | Element::Maybe(_) => {
            found.extend(words.get(*at).copied().filter(|address| *address != 0));
            *at += 1;
        }
        Element::Text => {
            let held = header_at(words, *at).and_then(|header| super::text::block_of(&header));
            found.extend(held.filter(|address| *address != 0));
            *at += 3;
        }
        Element::Struct { fields, .. } => {
            for (_, field) in fields {
                boxed_addresses(field, words, at, found);
            }
        }
        Element::Enum { cases, words: size, .. } => {
            let start = *at;
            let case = words.get(start).and_then(|tag| usize::try_from(*tag).ok());
            *at += 1;
            for (_, field) in case.and_then(|case| cases.get(case)).map_or(&[][..], |(_, fields)| fields) {
                boxed_addresses(field, words, at, found);
            }
            *at = start + size;
        }
        Element::Int | Element::Float | Element::Bool | Element::U64 | Element::F32 => *at += 1,
    }
}

/// Whether values of a kind, laid out in place, hold blocks.
pub(super) fn holds_floats(element: &Element) -> bool {
    match element {
        Element::Float | Element::F32 => true,
        Element::Struct { fields, .. } => fields.iter().any(|(_, field)| holds_floats(field)),
        Element::Enum { cases, .. } => {
            cases.iter().any(|(_, fields)| fields.iter().any(|(_, field)| holds_floats(field)))
        }
        _ => false,
    }
}

/// Whether a kind is an error's, with a place for notes.
fn holds_notes(element: &Element) -> bool {
    let noted = |fields: &Fields| fields.iter().any(|(field, _)| field.as_ref() == crate::packed::NOTES);
    match element {
        Element::Struct { fields, .. } => noted(fields),
        Element::Enum { cases, .. } => cases.iter().any(|(_, fields)| noted(fields)),
        _ => false,
    }
}

pub(super) fn holds_blocks(element: &Element) -> bool {
    match element {
        Element::Boxed(_)
        | Element::Str
        | Element::Text
        | Element::List(_)
        | Element::ListOf(_)
        | Element::Object
        | Element::Maybe(_) => true,
        Element::Struct { fields, .. } => fields.iter().any(|(_, field)| holds_blocks(field)),
        Element::Enum { cases, .. } => {
            cases.iter().any(|(_, fields)| fields.iter().any(|(_, field)| holds_blocks(field)))
        }
        Element::Int | Element::Float | Element::Bool | Element::U64 | Element::F32 => false,
    }
}

/// Adds the addresses of the blocks a value of a kind, laid out in place as `words`, holds.
pub(super) fn held_addresses(element: &Element, words: &[u64], found: &mut Vec<u64>) {
    boxed_addresses(element, words, &mut 0, found);
}

/// Where among its words a block holds the addresses of others, as `children_into` finds them, for
/// a copy of it to hold its own. A `Value` in a cell or a generator can't be copied like that,
/// but a native closure can.
pub(super) fn child_places(state: &State, block: &[u64], found: &mut Vec<usize>) -> Result<(), Diagnostic> {
    let Some(shape) = shape_word(block) else { return Ok(()) };
    if shape == CAPABILITY_SHAPE {
        found.extend((block.get(3).copied().unwrap_or(0) != 0).then_some(3));
        return Ok(());
    }
    if shape == CELL_SHAPE {
        return Err(not_copied());
    }
    if shape >= state.tables.str_shape {
        if is_text(state, block) {
            return Ok(());
        }
        if super::dicts::is_dict(state, block) {
            super::dicts::child_places(state, block, found);
            return Ok(());
        }
        if let Some(element) = super::lists::element_in(state, block) {
            let start = super::lists::LIST_HEADER_WORDS;
            if super::lists::owns_items(element) {
                let items = super::lists::items(block, 1);
                found.extend(
                    items.iter().enumerate().filter(|(_, item)| **item != 0).map(|(at, _)| start + at),
                );
            } else if holds_blocks(element) {
                let stride = element.words().max(1);
                let count = super::lists::items(block, stride).len() / stride;
                let mut at = start;
                for _ in 0..count {
                    let mut from = at;
                    boxed_words(element, block, &mut from, found);
                    at += stride;
                }
            }
        }
        return Ok(());
    }
    let Some(element) = shape_of(state, block) else { return Ok(()) };
    // A generator's block is in the middle of running, so it can't be copied. A closure's can: its
    // code is the same for every thread, and what it captured copies as any field does. An
    // error's notes are an ordinary field too.
    let hidden = |fields: &[(Rc<str>, Element)]| {
        fields.iter().any(|(name, _)| name.starts_with('\0') && &**name != "\0notes")
    };
    let closure = |fields: &[(Rc<str>, Element)]| fields.first().is_some_and(|(name, _)| &**name == "\0code");
    if matches!(element.as_ref(), Element::Struct { fields, .. } if hidden(fields) && !closure(fields)) {
        return Err(not_copied());
    }
    let mut from = HEADER_WORDS;
    boxed_words(&element, block, &mut from, found);
    Ok(())
}

fn not_copied() -> Diagnostic {
    Diagnostic::new(
        "this value can't be kept in or taken from a `shared` value yet",
        fennec_syntax::Span::default(),
    )
}

/// Adds the addresses of a block's boxed fields to `found`.
fn children_into(state: &State, block: &[u64], found: &mut Vec<u64>) {
    let Some(shape) = shape_word(block) else { return };
    if shape == CAPABILITY_SHAPE {
        found.extend(block.get(3).copied().filter(|list| *list != 0));
        return;
    }
    if shape == BYTES_SHAPE {
        return;
    }
    if shape >= state.tables.str_shape {
        if is_text(state, block) {
            return;
        }
        if super::dicts::is_dict(state, block) {
            found.extend(super::dicts::children(state, block));
            return;
        }
        if let Some(element) = super::lists::element_in(state, block) {
            let planned =
                super::lists::element_number(state, block).and_then(|at| state.heap.item_plans.get(at));
            if super::lists::owns_items(element) {
                found.extend(super::lists::items(block, 1).iter().copied().filter(|address| *address != 0));
            } else if let Some(planned) = planned {
                if !planned.plan.holds_nothing() {
                    for item in super::lists::items(block, planned.stride).chunks(planned.stride) {
                        if super::plans::addresses(&planned.plan, item, found).is_none() {
                            held_addresses(element, item, found);
                        }
                    }
                }
            } else if holds_blocks(element) {
                // Items laid out in place that hold blocks, like structs with a string field.
                let stride = element.words().max(1);
                for item in super::lists::items(block, stride).chunks(stride) {
                    held_addresses(element, item, found);
                }
            }
        }
        return;
    }
    let fields = block.get(HEADER_WORDS..).unwrap_or_default();
    let at = usize::try_from(shape).ok();
    let planned = at.and_then(|at| state.heap.plans.get(at));
    if planned.and_then(|plan| super::plans::addresses(plan, fields, found)).is_some() {
        return;
    }
    if let Some(shape) = at.and_then(|at| state.tables.shapes.get(at)) {
        boxed_addresses(shape, fields, &mut 0, found);
    }
}

/// The addresses of a block's boxed fields.
fn children(state: &State, block: &[u64]) -> Vec<u64> {
    let mut found = Vec::new();
    children_into(state, block, &mut found);
    found
}

/// A new `Table`'s tag: the next of one count for the whole process, which never comes round
/// again, so an `Id` from another `Table` finds nothing.
pub extern "C" fn new_tag() -> i64 {
    crate::value::new_tag().cast_signed()
}

/// A new block of a shape, owned once, its fields zero.
pub extern "C" fn alloc(shape: i64) -> i64 {
    super::made_with("fennec_alloc", |state| {
        let at = usize::try_from(shape).ok();
        let Some(words) = at.and_then(|at| state.heap.sizes.get(at).copied()).or_else(|| {
            at.and_then(|at| state.tables.shapes.get(at)).map(|element| HEADER_WORDS + element.words())
        }) else {
            return 0;
        };
        let mut block = state.heap.fresh(words);
        if let [count, kind, ..] = &mut *block {
            *count = 1;
            *kind = u64::try_from(shape).unwrap_or(0);
        }
        state.heap.insert(block)
    })
}

/// A copy of a shared block for one owner to change: owned once, sharing its boxed
/// fields. The block copied loses that owner.
pub extern "C" fn unshare(address: i64) -> i64 {
    super::made_with("fennec_unshare", |state| {
        let address = u64::try_from(address).unwrap_or(0);
        let Some(block) = state.heap.block(address) else {
            not_native(address, "copy, to change");
            return 0;
        };
        let mut copy: Box<[u64]> = block.into();
        // The block copied loses an owner, in its count, or for its lender to take off.
        if let Some(block) = state.heap.block_mut(address) {
            if !uncounted(block)
                && let Some(count) = block.first_mut()
            {
                *count = count.saturating_sub(1);
            }
        } else {
            state.heap.change_lent_owners(address, -1);
        }
        if let Some(count) = copy.first_mut() {
            *count = 1;
        }
        // The copy is this thread's, whoever made what it was copied from.
        mark_owner(&mut copy, state.heap.owner_bits);
        state.heap.copied(copy.len());
        for child in children(state, &copy) {
            retain(state, child);
        }
        state.heap.insert(copy)
    })
}

/// A temporary with a block's value, which holds an owner of the block rather than a copy of
/// what's in it: most temporaries are only passed along. The block keeps its other owners.
pub extern "C" fn to_value(address: i64) -> i64 {
    with(|state| {
        let address = u64::try_from(address).unwrap_or(0);
        if state.heap.block(address).is_none_or(|block| is_text(state, block)) {
            let value = read_block(state, address);
            return state.push(value);
        }
        retain(state, address);
        state.push(Value::Lent(Rc::new(Lent { address })))
    })
}

/// One more owner for a block.
pub(super) fn retain(state: &mut State, address: u64) {
    if let Some(block) = state.heap.block_mut(address) {
        if !uncounted(block)
            && let Some(count) = block.first_mut()
        {
            *count += 1;
        }
    } else if state.heap.change_lent_owners(address, 1) {
    } else if super::guarded::is_handle(address) {
        super::guarded::count_handle(address);
    } else {
        not_native(address, "retain");
    }
}

/// A block's owner from a temporary lent to it, if that's what it is and `fits` likes the block.
pub(super) fn lent_block(
    state: &mut State,
    value: i64,
    fits: impl Fn(&State, &[u64]) -> bool,
) -> Option<i64> {
    let Value::Lent(lent) = state.raw(value) else { return None };
    let block = state.heap.block(lent.address)?;
    if !fits(state, block) {
        return None;
    }
    retain(state, lent.address);
    i64::try_from(lent.address).ok()
}

pub(super) fn read_block(state: &State, address: u64) -> Value {
    if !room_to_read() {
        too_deep();
    }
    let at = usize::try_from(address).unwrap_or(0);
    // A string only changes in place by growing, so a text of the same length is current.
    let length = state.heap.block(address).and_then(|block| block.get(2)).copied();
    if let Some(text) =
        state.heap.texts.borrow().get(&at).filter(|text| u64::try_from(text.len()).ok() == length)
    {
        return Value::Str(Rc::clone(text));
    }
    let Some(block) = state.heap.block(address).map(<[u64]>::to_vec) else {
        not_native(address, "read");
        return Value::None;
    };
    if let Some(value) = cell_value(state, &block) {
        return state.expanded(value);
    }
    if shape_word(&block) == Some(CAPABILITY_SHAPE) {
        return super::capabilities::read(state, &block);
    }
    if shape_word(&block) == Some(BYTES_SHAPE) {
        return Value::Bytes(Rc::from(super::bytes::data_of(&block)));
    }
    if is_text(state, &block) {
        let text: Rc<str> = Rc::from(block_text(&block));
        state.heap.texts.borrow_mut().insert(at, Rc::clone(&text));
        return Value::Str(text);
    }
    if super::dicts::is_dict(state, &block) {
        return super::dicts::read(state, &block);
    }
    if super::lists::element_of(state, &block).is_some() {
        return super::lists::read(state, &block);
    }
    let Some(shape) = shape_of(state, &block) else { return Value::None };
    let mut words = block.get(HEADER_WORDS..).unwrap_or_default().iter();
    read(state, &shape, &mut words)
}

/// A list's `count` items, laid out as `words`, as `Value`s, one by one.
pub(super) fn read_items(state: &State, element: &Element, words: &[u64], count: usize) -> Value {
    let mut words = words.iter();
    Value::list((0..count).map(|_| read(state, element, &mut words)).collect())
}

/// A value of a word, as a `Value`.
pub(super) fn read_word(state: &State, element: &Element, word: u64) -> Value {
    read(state, element, &mut std::iter::once(&word))
}

fn read<'w>(state: &State, element: &Element, words: &mut dyn Iterator<Item = &'w u64>) -> Value {
    let word = |words: &mut dyn Iterator<Item = &'w u64>| words.next().copied().unwrap_or(0);
    match element {
        // A `Value` has no `u64`: it holds one as an `int` of the same bits, so it goes back
        // unchanged, though a value of 2^63 or more reads there as negative.
        Element::Int | Element::U64 => Value::Int(i64::from_ne_bytes(word(words).to_ne_bytes())),
        Element::Float | Element::F32 => Value::Float(f64::from_bits(word(words))),
        Element::Bool => Value::Bool(word(words) != 0),
        Element::Boxed(_) | Element::Str | Element::Object => read_block(state, word(words)),
        Element::Text => {
            let header = [word(words), word(words), word(words)];
            Value::str(&super::text::text_of(state, &header))
        }
        // Only the top of a `Value` may be a packed list.
        Element::List(_) | Element::ListOf(_) => read_block(state, word(words)).unpacked(),
        element if element.is_optional_text() => match [word(words), word(words), word(words)] {
            [0, 0, 0] => Value::None,
            header => Value::str(&super::text::text_of(state, &header)),
        },
        Element::Maybe(inner) => match word(words) {
            0 => Value::None,
            address => read(state, inner, &mut std::iter::once(&address)),
        },
        // A tuple a backend lays out as a struct, named after its type, like `(int, str)`.
        Element::Struct { name, fields } if name.starts_with('(') => {
            Value::tuple(fields.iter().map(|(_, element)| read(state, element, words)).collect())
        }
        // An array kept in place, which a `Value` holds as a list.
        Element::Struct { name, fields } if named::is_array(name) => {
            Value::list(fields.iter().map(|(_, element)| read(state, element, words)).collect())
        }
        Element::Struct { name, fields } => {
            let value = instance(name, None, read_fields(state, fields, words));
            // An error's block is laid out like a struct's, so its type says which it is.
            // Nothing looks at this on a value read back.
            let error = false;
            match value {
                Value::Instance(mut made) if error => {
                    if let Some(made) = Rc::get_mut(&mut made) {
                        made.is_error = true;
                    }
                    Value::Instance(made)
                }
                other => named::pinned_outcome(name, other),
            }
        }
        Element::Enum { name, cases, words: size } => {
            let mut mine = (&mut *words).take(*size);
            let at = usize::try_from(word(&mut mine)).unwrap_or(0);
            let (case, fields) = cases.get(at).or_else(|| cases.first()).cloned().unwrap_or_default();
            let fields = read_fields(state, &fields, &mut mine);
            let value = named::pinned_outcome(name, instance(name, Some(case), fields));
            mine.for_each(drop);
            value
        }
    }
}

/// A struct's or a case's fields, read in order. A field the layout keeps for itself is read
/// past, except an error's notes, which are kept apart from its fields, under `NOTES`, for
/// `instance` to take out.
fn read_fields(
    state: &State,
    fields: &Fields,
    words: &mut dyn Iterator<Item = &u64>,
) -> Vec<(Rc<str>, Value)> {
    let mut read_ones = Vec::with_capacity(fields.len());
    for (field, element) in fields {
        let value = read(state, element, words);
        if field.as_ref() == crate::packed::NOTES || !crate::packed::hidden(field) {
            read_ones.push((Rc::clone(field), value));
        }
    }
    read_ones
}

/// The name a `Value` gives instances of a layout: a generic type's layout is named
/// with its type's arguments, like `Grid[Tile]`, and its instances just `Grid`.
fn type_name(layout: &str) -> &str {
    layout.split_once('[').map_or(layout, |(name, _)| name)
}

fn instance(name: &Rc<str>, case: Option<Rc<str>>, mut fields: Vec<(Rc<str>, Value)>) -> Value {
    let type_name = if name.contains('[') { Rc::from(type_name(name)) } else { Rc::clone(name) };
    // Only an error's layout has notes, so a value with them is an error.
    let notes_at = fields.iter().position(|(field, _)| field.as_ref() == crate::packed::NOTES);
    let notes = notes_at.and_then(|at| match fields.remove(at).1.unpacked() {
        Value::List(items) if !items.is_empty() => Some(Rc::new(
            items
                .iter()
                .filter_map(|item| if let Value::Str(text) = item { Some(Rc::clone(text)) } else { None })
                .collect(),
        )),
        _ => None,
    });
    Value::Instance(Rc::new(Instance {
        type_name,
        case,
        fields,
        is_error: notes_at.is_some(),
        cause: None,
        notes,
    }))
}

/// A new block owned once, with a temporary's value. Fails when it isn't of the shape's type.
pub extern "C" fn from_value(value: i64, shape: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let wanted = u64::try_from(shape).unwrap_or(u64::MAX);
        if let Some(address) = lent_block(state, value, |_, block| shape_word(block) == Some(wanted)) {
            return Ok(address);
        }
        let value = state.temp(value);
        let shape = usize::try_from(shape).unwrap_or(0);
        write_block(state, shape, &value).ok_or_else(|| {
            let wanted = match state.tables.shapes.get(shape).map(AsRef::as_ref) {
                Some(Element::Struct { name, .. } | Element::Enum { name, .. }) => name.to_string(),
                _ => "value".to_owned(),
            };
            Diagnostic::new(
                format!("expected a `{wanted}` here, but this is a {}", value.type_name()),
                span(start, end),
            )
        })
    })
}

/// The number of the shape of a struct or enum, by its name.
pub(super) fn shape_named(state: &State, name: &str) -> Option<usize> {
    state.tables.shapes.iter().position(|shape| match shape.as_ref() {
        Element::Struct { name: found, .. } | Element::Enum { name: found, .. } => found.as_ref() == name,
        _ => false,
    })
}

pub(super) fn write_block(state: &mut State, shape: usize, value: &Value) -> Option<i64> {
    if u64::try_from(shape).ok()? == BYTES_SHAPE {
        return super::bytes::write(state, value);
    }
    if u64::try_from(shape).ok()? > state.tables.str_shape {
        return super::dicts::write(state, u64::try_from(shape).ok()?, value);
    }
    let element = state.tables.shapes.get(shape).cloned()?;
    let mut words = vec![1, u64::try_from(shape).ok()?];
    write(state, &element, value, &mut words)?;
    Some(state.heap.insert(words.into_boxed_slice()))
}

pub(super) fn write(state: &mut State, element: &Element, value: &Value, words: &mut Vec<u64>) -> Option<()> {
    match named::write_named(state, element, value, words) {
        named::Written::Done => return Some(()),
        named::Written::Failed => return None,
        named::Written::NotNamed => {}
    }
    match (element, value) {
        (Element::Int | Element::U64, Value::Int(n)) => words.push(u64::from_ne_bytes(n.to_ne_bytes())),
        (Element::Float, Value::Float(x)) => words.push(x.to_bits()),
        (Element::Bool, Value::Bool(b)) => words.push(u64::from(*b)),
        (Element::F32, Value::Float(x)) => words.push(crate::packed::as_f32(*x).to_bits()),
        (Element::Boxed(shape), value) => {
            let address = write_block(state, *shape, value)?;
            words.push(u64::try_from(address).ok()?);
        }
        (Element::Str, Value::Str(text)) => words.push(u64::try_from(text_address(state, text)).ok()?),
        (Element::Text, Value::Str(text)) => words.extend(super::text::header(state, text)),
        (Element::Object, value) => words.push(u64::try_from(new_cell(state, value.clone())).ok()?),
        (Element::Maybe(_), Value::None) => words.resize(words.len() + element.words(), 0),
        (Element::Maybe(inner), value) => write(state, inner, value, words)?,
        (Element::List(item), value) => {
            words.push(u64::try_from(super::lists::write_list(state, item, value)?).ok()?);
        }
        (Element::ListOf(number), value) => {
            let item = Rc::clone(state.tables.elements.get(*number)?);
            words.push(u64::try_from(super::lists::write_list(state, &item, value)?).ok()?);
        }
        (Element::Struct { name, fields }, value) if named::is_array(name) => {
            let Value::List(items) = value.clone().unpacked() else { return None };
            if items.len() != fields.len() {
                return None;
            }
            for ((_, element), item) in fields.iter().zip(items.iter()) {
                write(state, element, item, words)?;
            }
        }
        (Element::Struct { name, fields }, Value::Tuple(items))
            if name.starts_with('(') && items.len() == fields.len() =>
        {
            for ((_, element), item) in fields.iter().zip(items.iter()) {
                write(state, element, item, words)?;
            }
        }
        (Element::Struct { name, fields }, Value::Instance(instance))
            if *instance.type_name == *type_name(name) && instance.case.is_none() =>
        {
            // An error is written like a struct: native code holds it as a block of its layout.
            for (field, element) in fields {
                write_field(state, instance, field, element, words)?;
            }
        }
        (Element::Enum { name, cases, words: size }, Value::Instance(instance))
            if *instance.type_name == *type_name(name) =>
        {
            let case = instance.case.as_ref()?;
            let at = cases.iter().position(|(written, _)| written == case)?;
            let start = words.len();
            words.push(u64::try_from(at).ok()?);
            for (field, element) in &cases.get(at)?.1 {
                write_field(state, instance, field, element, words)?;
            }
            words.resize(start + size, 0);
        }
        _ => return None,
    }
    Some(())
}

/// One of an instance's fields, written where its layout puts it. A lock starts as 0, and so do
/// an error's notes until it has some.
fn write_field(
    state: &mut State,
    instance: &Instance,
    field: &str,
    element: &Element,
    words: &mut Vec<u64>,
) -> Option<()> {
    match field {
        crate::packed::NOTES => {
            if let Some(notes) = &instance.notes {
                write(state, element, &Value::list(notes.iter().cloned().map(Value::Str).collect()), words)
            } else {
                words.push(0);
                Some(())
            }
        }
        crate::packed::LOCK => {
            words.push(0);
            Some(())
        }
        _ => write(state, element, instance.field(field)?, words),
    }
}

/// Whether two blocks hold equal values: the same bits, field by field, and equal boxed
/// fields.
pub extern "C" fn equal(a: i64, b: i64) -> i8 {
    with(|state| i8::from(same(state, u64::try_from(a).unwrap_or(0), u64::try_from(b).unwrap_or(0))))
}

thread_local! {
    /// The pairs of blocks a compare has still to look at, kept to save making the list anew.
    static PAIRS: std::cell::Cell<Vec<(u64, u64)>> = const { std::cell::Cell::new(Vec::new()) };
}

/// Whether the blocks at `a` and `b` hold equal values. The blocks they hold are compared from a
/// list of what's left, never by this calling itself, so a value as deep as memory allows, like
/// a chain of millions of structs, each in the last one's field, compares on any stack.
pub(super) fn same(state: &State, a: u64, b: u64) -> bool {
    if a == b {
        return true;
    }
    let mut left = PAIRS.take();
    left.clear();
    let equal = same_here(state, a, b, &mut left) && rest_same(state, &mut left);
    left.clear();
    PAIRS.set(left);
    equal
}

/// Whether every pair `left` holds, and every pair those lead to, is equal.
fn rest_same(state: &State, left: &mut Vec<(u64, u64)>) -> bool {
    while let Some((a, b)) = left.pop() {
        if !same_here(state, a, b, left) {
            return false;
        }
    }
    true
}

/// Notes that the blocks at `a` and `b` are still to compare, for a value to be equal: true, as
/// nothing's found unequal yet.
pub(super) fn compare_later(left: &mut Vec<(u64, u64)>, a: u64, b: u64) -> bool {
    if a != b {
        left.push((a, b));
    }
    true
}

/// Whether the blocks at `a` and `b` are equal in themselves, with each pair of blocks they hold
/// put on `left` to compare after.
fn same_here(state: &State, a: u64, b: u64, later: &mut Vec<(u64, u64)>) -> bool {
    if a == b {
        return true;
    }
    let (Some(left), Some(right)) = (state.heap.block(a), state.heap.block(b)) else {
        not_native(a, "compare");
        not_native(b, "compare");
        return false;
    };
    let shape = shape_word(left);
    if shape != shape_word(right) {
        return false;
    }
    // Two `shared` values are the same only if they're one value, whatever they hold now.
    if shape.is_some_and(|shape| state.tables.cells.contains(&shape)) {
        return false;
    }
    if shape_word(left) == Some(BYTES_SHAPE) {
        return super::bytes::equal(left, right);
    }
    if let (Some(mine), Some(theirs)) = (cell_value(state, left), cell_value(state, right)) {
        return mine == theirs;
    }
    if is_text(state, left) {
        // Compare the lengths and the bytes in use, not the room to grow.
        let length = left.get(2).and_then(|n| usize::try_from(*n).ok()).unwrap_or(0);
        let text = TEXT_HEADER_WORDS..TEXT_HEADER_WORDS + length.div_ceil(8);
        return shape_word(left) == shape_word(right)
            && left.get(2..4) == right.get(2..4)
            && left.get(text.clone()) == right.get(text);
    }
    if super::dicts::is_dict(state, left) {
        return super::dicts::equal(state, left, right, later);
    }
    if let Some(element) = super::lists::element_of(state, left) {
        // Items compare bit by bit, unless they hold floats or blocks.
        let stride = element.words();
        let (mine, theirs) = (super::lists::items(left, stride), super::lists::items(right, stride));
        if super::lists::owns_items(&element) {
            return mine.len() == theirs.len()
                && mine.iter().zip(theirs).all(|(x, y)| compare_later(later, *x, *y));
        }
        if holds_blocks(&element) || holds_floats(&element) {
            let stride = stride.max(1);
            return mine.len() == theirs.len()
                && mine
                    .chunks(stride)
                    .zip(theirs.chunks(stride))
                    .all(|(x, y)| same_words_here(state, &element, x, y, later));
        }
        return mine == theirs;
    }
    if left.len() != right.len() {
        return false;
    }
    let Some(shape) = shape_of(state, left) else { return false };
    let (left, right) =
        (left.get(HEADER_WORDS..).unwrap_or_default(), right.get(HEADER_WORDS..).unwrap_or_default());
    if let Element::Struct { name, fields } = shape.as_ref()
        && named::is_id_map(name)
    {
        return same_tables(fields, left, right, later);
    }
    same_words_here(state, &shape, left, right, later)
}

/// Whether two `Table`s are equal: the same items in the same slots, with the same generations.
/// Which `Table` each is, its tag, and where a search for a free slot starts don't count.
fn same_tables(
    fields: &[(Rc<str>, Element)],
    left: &[u64],
    right: &[u64],
    later: &mut Vec<(u64, u64)>,
) -> bool {
    let mut at = 0;
    for (field, element) in fields {
        if matches!(field.as_ref(), "items" | "generations") {
            let (Some(x), Some(y)) = (left.get(at), right.get(at)) else { return false };
            compare_later(later, *x, *y);
        }
        at += element.words();
    }
    true
}

/// Whether two values of a kind, laid out as words, are equal: the same plain words, and
/// equal blocks where they hold blocks.
pub(super) fn same_words(state: &State, element: &Element, left: &[u64], right: &[u64]) -> bool {
    let mut later = Vec::new();
    same_words_here(state, element, left, right, &mut later) && rest_same(state, &mut later)
}

/// [`same_words`] of the words themselves, with each pair of blocks they hold put on `later` to
/// compare after.
fn same_words_here(
    state: &State,
    element: &Element,
    left: &[u64],
    right: &[u64],
    later: &mut Vec<(u64, u64)>,
) -> bool {
    // An error compares by its values, whose notes don't count.
    if holds_notes(element) {
        return read(state, element, &mut left.iter()) == read(state, element, &mut right.iter());
    }
    // A float's bits don't say what's equal, so one with floats in it goes field by field.
    if holds_floats(element) {
        return same_fields(element, (left, right), &mut 0, later);
    }
    let mut boxed = Vec::new();
    boxed_words(element, left, &mut 0, &mut boxed);
    // The case decides where the boxed fields are, so compare the plain words first.
    let plain_same = left.iter().zip(right).enumerate().all(|(at, (x, y))| boxed.contains(&at) || x == y);
    plain_same
        && boxed.iter().all(|at| {
            let (Some(x), Some(y)) = (left.get(*at), right.get(*at)) else { return false };
            compare_later(later, *x, *y)
        })
}

/// Whether two values of `element`, from `at` in `left` and `right`, are equal, a field at a time:
/// a float as Fennec compares floats, `-0.0` the same as `0.0` and every NaN the same, as they hash;
/// a block by what it holds; and an enum by its case, then only that case's fields. Nothing is
/// read into a `Value`, so a value whose type holds a float anywhere costs what one without does.
fn same_fields(
    element: &Element,
    (left, right): (&[u64], &[u64]),
    at: &mut usize,
    later: &mut Vec<(u64, u64)>,
) -> bool {
    let word = |words: &[u64], at: usize| words.get(at).copied().unwrap_or(0);
    let (x, y) = (word(left, *at), word(right, *at));
    match element {
        element if element.is_optional_text() => same_fields(&Element::Text, (left, right), at, later),
        Element::Float | Element::F32 => {
            *at += 1;
            crate::value::floats_same(f64::from_bits(x), f64::from_bits(y))
        }
        Element::Int | Element::Bool | Element::U64 => {
            *at += 1;
            x == y
        }
        Element::Boxed(_)
        | Element::Str
        | Element::List(_)
        | Element::ListOf(_)
        | Element::Object
        | Element::Maybe(_) => {
            *at += 1;
            // No block is `None`, which is equal only to another.
            x == y || (x != 0 && y != 0 && compare_later(later, x, y))
        }
        Element::Text => {
            let (mine, theirs) = (header_at(left, *at), header_at(right, *at));
            *at += 3;
            if mine == theirs {
                return true;
            }
            // Equal strings have the same form, so only two blocks need their bytes compared.
            let block = |header: Option<[u64; 3]>| header.and_then(|header| super::text::block_of(&header));
            matches!((block(mine), block(theirs)), (Some(a), Some(b)) if compare_later(later, a, b))
        }
        Element::Struct { fields, .. } => {
            fields.iter().all(|(_, field)| same_fields(field, (left, right), at, later))
        }
        Element::Enum { cases, words: size, .. } => {
            let start = *at;
            *at = start + size;
            if x != y {
                return false;
            }
            let case = usize::try_from(x).ok().and_then(|case| cases.get(case));
            let mut inside = start + 1;
            case.map_or(&[][..], |(_, fields)| fields)
                .iter()
                .all(|(_, field)| same_fields(field, (left, right), &mut inside, later))
        }
    }
}

/// A string's block owned once, or a one-character ASCII string's block that lives for the
/// whole run.
pub(super) fn text_address(state: &mut State, text: &str) -> i64 {
    if let [byte] = text.as_bytes()
        && let Some(address) = state.tables.ascii.get(usize::from(*byte))
    {
        return i64::try_from(*address).unwrap_or(0);
    }
    text_from_bytes(state, text.as_bytes(), text.chars().count())
}

#[cfg(test)]
mod worker_counts_tests {
    /// A counted allocation on a worker reaches what the run reports, while the worker is still
    /// running, as a server's are when the program ends, and once it's ended.
    #[test]
    fn a_workers_counts_reach_the_report() {
        let before = super::worker_counts().allocated_bytes;
        let (counted, wait) = std::sync::mpsc::channel();
        let (go_on, told) = std::sync::mpsc::channel::<()>();
        let worker = std::thread::spawn(move || {
            super::super::threads::take_index();
            let mut heap = super::Heap::new(Vec::new());
            heap.allocated(12_345);
            counted.send(()).unwrap();
            told.recv().unwrap();
            heap.allocated(1_000);
        });
        wait.recv().unwrap();
        // Other tests' workers may count meanwhile, so this one's are at least what's there.
        assert!(super::worker_counts().allocated_bytes >= before + 12_345);
        go_on.send(()).unwrap();
        worker.join().unwrap();
        assert!(super::worker_counts().allocated_bytes >= before + 13_345);
    }
}
