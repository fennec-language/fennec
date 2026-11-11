//! Ordered streams: `(f(x) for x in parallel(xs))`, read in order while workers run ahead.
//!
//! The list's positions are cut into chunks of about 16 KB of items, as for a comprehension.
//! Workers run the chunks ahead of the reader, at most a window of them: twice the pool's
//! threads. The reader takes each chunk's items as a list of its own, in order, and taking one
//! lets the workers start another, so a slow reader holds them back rather than results piling
//! up. A chunk no worker has started when the reader gets to it, the reader runs itself, so a
//! stream too small for threads, or read on a worker, never waits for one.
//!
//! A stream is a number native code holds, looked up in the reading thread's table of its
//! streams, so a number the runtime didn't give out reads as a stream that has ended. The list
//! and `env` belong to the reader, who keeps them until it ends the stream.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use fennec_syntax::Diagnostic;

use super::super::{State, attempt, heap, lists, set_failed, threads, with};
use super::{MapBody, chunk, extent, number, pool};

/// Where a chunk is.
enum Slot {
    /// Not started: whichever thread gets to it first runs it.
    Waiting,
    Running,
    /// Run: its items (0 when it failed), what a worker made that the reader adopts, and its
    /// failure.
    Done {
        items: i64,
        made: Vec<Box<[u64]>>,
        failure: Option<Diagnostic>,
    },
}

/// The chunks from the next one the reader takes, up to the window's end.
struct Chunks {
    /// The number of the chunk at the front of `slots`.
    first: usize,
    slots: VecDeque<Slot>,
    /// Whether the reader has ended the stream, so chunks not started never will be.
    ended: bool,
}

impl Chunks {
    fn slot(&mut self, at: usize) -> Option<&mut Slot> {
        at.checked_sub(self.first).and_then(|i| self.slots.get_mut(i))
    }
}

struct Stream {
    body: MapBody,
    env: i64,
    element: i64,
    length: usize,
    chunk: usize,
    /// How many chunks run ahead of the reader on the pool: 0 when none do.
    window: usize,
    /// The thread reading the stream, which adopts what the chunks make.
    reader: u16,
    /// The stream, as a task of the one reading it, for naming its chunks' entries in a recording
    /// or a replay: each chunk as a `parallel` call's range is, by where it starts. `None` in a
    /// normal run.
    task: Option<crate::replay::Task>,
    chunks: Mutex<Chunks>,
    /// Signalled whenever a chunk is done.
    done: Condvar,
}

thread_local! {
    /// This thread's streams, by the number native code holds.
    static STREAMS: RefCell<HashMap<i64, Arc<Stream>>> = RefCell::new(HashMap::new());
    static NEXT: Cell<i64> = const { Cell::new(1) };
}

impl Stream {
    fn lock(&self) -> MutexGuard<'_, Chunks> {
        self.chunks.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn count(&self) -> usize {
        self.length.div_ceil(self.chunk)
    }

    /// Runs chunk `at` on this thread, unless another thread has started it or the stream has
    /// ended.
    fn run(&self, at: usize) {
        {
            let mut chunks = self.lock();
            let ended = chunks.ended;
            let Some(slot) = chunks.slot(at) else { return };
            if ended || !matches!(slot, Slot::Waiting) {
                return;
            }
            *slot = Slot::Running;
        }
        let done = self.make(at);
        // Kept even once the stream has ended, for the reader ending it to free.
        if let Some(slot) = self.lock().slot(at) {
            *slot = done;
        }
        self.done.notify_all();
    }

    /// Chunk `at`'s items, made on this thread.
    fn make(&self, at: usize) -> Slot {
        let from = at * self.chunk;
        let to = (from + self.chunk).min(self.length);
        let since = threads::made_so_far();
        let mut was = crate::replay::range(self.task.as_ref(), from);
        crate::replay::swap(&mut was);
        let mut items = lists::new(self.element, number(to - from));
        let thread = i64::from(threads::index());
        let mut failure = None;
        if (self.body)(self.env, number(from), number(to), thread, items) != 0 {
            heap::free(items);
            items = 0;
            failure = with(|state| state.failure.take());
            set_failed(0);
        }
        let made = if threads::index() == self.reader { Vec::new() } else { threads::hand_over(since) };
        // In a recording or a replay, what the chunk printed goes out under its name.
        if crate::replay::active() {
            super::super::write_kept_output();
        }
        crate::replay::swap(&mut was);
        Slot::Done { items, made, failure }
    }

    /// Starts chunk `at` on the pool.
    fn start(self: &Arc<Self>, at: usize) {
        let stream = Arc::clone(self);
        pool::spawn(move || stream.run(at));
    }

    /// The next chunk, once it's done: run here if no thread has started it. With it taken,
    /// the window moves on by one, and the chunk now at its end starts on the pool.
    fn take(self: &Arc<Self>) -> Option<Slot> {
        let mut chunks = self.lock();
        loop {
            if chunks.ended {
                return None;
            }
            let at = chunks.first;
            match chunks.slots.front() {
                None => return None,
                Some(Slot::Waiting) => {
                    drop(chunks);
                    self.run(at);
                    chunks = self.lock();
                }
                Some(Slot::Running) => {
                    // What's been printed comes out before this thread waits.
                    super::super::write_kept_output();
                    chunks = self.done.wait(chunks).unwrap_or_else(PoisonError::into_inner);
                }
                Some(Slot::Done { .. }) => break,
            }
        }
        let taken = chunks.slots.pop_front();
        chunks.first += 1;
        let next = chunks.first + chunks.slots.len();
        let more = next < self.count();
        if more {
            chunks.slots.push_back(Slot::Waiting);
        }
        drop(chunks);
        if more && self.window > 0 {
            self.start(next);
        }
        taken
    }
}

/// `fennec_parallel_stream(list, body, env, element) -> stream`: `(f(x) for x in
/// parallel(list))`, read in order with `fennec_stream_next` and ended with
/// `fennec_stream_end`. `body(env, from, to, thread, out)` writes the item for each position
/// `from + i` at `i` in `out`, a list the runtime made of `to - from` items of the element
/// numbered `element`. The first chunks start on the pool straight away.
pub extern "C" fn parallel_stream(list: i64, body: MapBody, env: i64, element: i64) -> i64 {
    let (length, words) = with(|state| extent(state, list)).unwrap_or((0, 1));
    let window = pool::window(length * words);
    let chunk = chunk(words);
    let ahead = window.max(1).min(length.div_ceil(chunk));
    let slots = (0..ahead).map(|_| Slot::Waiting).collect();
    let stream = Arc::new(Stream {
        body,
        env,
        element,
        length,
        chunk,
        window,
        reader: threads::index(),
        task: crate::replay::child(),
        chunks: Mutex::new(Chunks { first: 0, slots, ended: false }),
        done: Condvar::new(),
    });
    if window > 0 {
        (0..ahead).for_each(|at| stream.start(at));
    }
    let number = NEXT.get();
    NEXT.set(number + 1);
    STREAMS.with_borrow_mut(|streams| streams.insert(number, stream));
    number
}

/// `fennec_stream_next(stream, out) -> i64`: the next chunk's items, in order, as a list
/// owned once by the reader, written into `out`, or 0 once there are no more. Gives back 0, or
/// 1 when the chunk failed, after every chunk before it was read. A stream that failed has no
/// more.
pub extern "C" fn stream_next(stream: i64, out: &mut i64) -> i64 {
    *out = 0;
    let Some(stream) = STREAMS.with_borrow(|streams| streams.get(&stream).cloned()) else { return 0 };
    let Some(Slot::Done { items, made, failure }) = stream.take() else { return 0 };
    threads::adopt(made);
    let Some(failure) = failure else {
        *out = items;
        return 0;
    };
    stream.lock().ended = true;
    attempt(|_: &mut State| -> Result<i64, Diagnostic> { Err(failure) }).max(1)
}

/// `fennec_stream_end(stream)`: ends a stream, read to its end or not. Chunks not started
/// never will be; the reader waits for those running, and frees what every chunk not read
/// made.
pub extern "C" fn stream_end(stream: i64) {
    let Some(stream) = STREAMS.with_borrow_mut(|streams| streams.remove(&stream)) else { return };
    let mut chunks = stream.lock();
    chunks.ended = true;
    super::super::write_kept_output();
    while chunks.slots.iter().any(|slot| matches!(slot, Slot::Running)) {
        chunks = stream.done.wait(chunks).unwrap_or_else(PoisonError::into_inner);
    }
    let slots = std::mem::take(&mut chunks.slots);
    drop(chunks);
    for slot in slots {
        if let Slot::Done { items, made, .. } = slot {
            threads::adopt(made);
            if items != 0 {
                heap::free(items);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicI64, Ordering};

    use super::super::super::{Tables, finish, heap, lists, start, with};
    use crate::packed::Element;

    fn begin() {
        let tables = || Tables {
            shapes: vec![Rc::new(Element::Str)],
            elements: vec![Rc::new(Element::Int)],
            ..Tables::default()
        };
        super::super::set_worker_setup(Box::new(move || {
            start(tables(), crate::Inputs::default());
        }));
        start(tables(), crate::Inputs::default());
    }

    /// Enough items to run on the pool.
    const ITEMS: i64 = 100_000;

    fn live() -> usize {
        with(|state| state.heap.live())
    }

    /// Writes each position as its item.
    fn write_positions(from: i64, to: i64, out: i64) {
        with(|state| {
            let Some(block) = state.heap.block_mut(out.cast_unsigned()) else { return };
            for (at, position) in (from..to).enumerate() {
                if let Some(word) = block.get_mut(lists::LIST_HEADER_WORDS + at) {
                    *word = position.cast_unsigned();
                }
            }
        });
    }

    fn items_of(list: i64) -> Vec<u64> {
        with(|state| state.heap.block(list.cast_unsigned()).map(|block| lists::items(block, 1).to_vec()))
            .unwrap_or_default()
    }

    /// Reads up to `chunks` chunks, freeing each: the items read, and whether it failed.
    fn read(stream: i64, chunks: usize) -> (Vec<u64>, bool) {
        let mut items = Vec::new();
        for _ in 0..chunks {
            let mut chunk = 0;
            if super::stream_next(stream, &mut chunk) != 0 {
                return (items, true);
            }
            if chunk == 0 {
                break;
            }
            items.extend(items_of(chunk));
            heap::free(chunk);
        }
        (items, false)
    }

    static THREADS: Mutex<Vec<i64>> = Mutex::new(Vec::new());

    /// Writes the positions, noting the thread. The first chunk waits until a thread other
    /// than the reader, numbered `reader`, has run one, so the test doesn't hang on whether the
    /// workers or the reader get to the chunks first.
    extern "C" fn positions(reader: i64, from: i64, to: i64, thread: i64, out: i64) -> i64 {
        write_positions(from, to, out);
        THREADS.lock().unwrap().push(thread);
        for _ in 0..10_000 {
            if from != 0 || THREADS.lock().unwrap().iter().any(|ran| *ran != reader) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        0
    }

    #[test]
    fn a_stream_gives_its_items_in_order_while_workers_run_ahead() {
        begin();
        let before = live();
        let list = lists::new(0, ITEMS);
        let here = i64::from(super::threads::index());
        let stream = super::parallel_stream(list, positions, here, 0);
        let (items, failed) = read(stream, usize::MAX);
        assert!(!failed);
        assert!(items.iter().copied().eq(0..ITEMS.cast_unsigned()));
        super::stream_end(stream);
        // Chunks ran on the pool's workers, and the reader adopted their items.
        assert!(THREADS.lock().unwrap().iter().any(|thread| *thread != here));
        heap::free(list);
        assert_eq!(live(), before);
        finish();
    }

    /// The furthest chunk `paced` has started, by where it starts.
    static FURTHEST: AtomicI64 = AtomicI64::new(0);

    extern "C" fn paced(_env: i64, from: i64, to: i64, _thread: i64, out: i64) -> i64 {
        write_positions(from, to, out);
        FURTHEST.fetch_max(from, Ordering::Relaxed);
        0
    }

    #[test]
    fn a_slow_reader_holds_the_workers_back() {
        begin();
        let before = live();
        let list = lists::new(0, ITEMS);
        let stream = super::parallel_stream(list, paced, 0, 0);
        let (items, _) = read(stream, 3);
        // However long the workers have, they don't run more than a window past the reader.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let window = super::pool::window(usize::MAX / 2);
        let chunk = i64::try_from(super::chunk(1)).unwrap();
        let furthest_chunk = usize::try_from(FURTHEST.load(Ordering::Relaxed) / chunk).unwrap();
        assert!(window > 0 && furthest_chunk < 3 + window, "chunk {furthest_chunk} started, window {window}");
        assert_eq!(items.len(), 3 * super::chunk(1));
        // Ending it early frees what the chunks not read made.
        super::stream_end(stream);
        heap::free(list);
        assert_eq!(live(), before);
        finish();
    }

    extern "C" fn fails_twice(_env: i64, from: i64, to: i64, _thread: i64, out: i64) -> i64 {
        write_positions(from, to, out);
        let at = [90_000, 70_000].into_iter().find(|at| (from..to).contains(at));
        let Some(at) = at else { return 0 };
        super::attempt(|_: &mut super::State| -> Result<i64, super::Diagnostic> {
            Err(super::Diagnostic::new(format!("failed at {at}"), fennec_syntax::Span::default()))
        });
        1
    }

    #[test]
    fn a_stream_fails_where_its_first_failing_chunk_is_read() {
        begin();
        let before = live();
        let list = lists::new(0, ITEMS);
        let stream = super::parallel_stream(list, fails_twice, 0, 0);
        let (items, failed) = read(stream, usize::MAX);
        assert!(failed);
        // Every chunk before the failing one was read, in order, and nothing after.
        let chunk = super::chunk(1);
        assert_eq!(items.len(), 70_000 / chunk * chunk);
        assert_eq!(read(stream, usize::MAX), (Vec::new(), false));
        super::stream_end(stream);
        heap::free(list);
        assert_eq!(live(), before);
        let (_, failure, _) = finish();
        assert!(failure.is_some_and(|failure| failure.message == "failed at 70000"));
    }

    static SMALL: Mutex<Vec<i64>> = Mutex::new(Vec::new());

    extern "C" fn small(_env: i64, from: i64, to: i64, thread: i64, out: i64) -> i64 {
        write_positions(from, to, out);
        SMALL.lock().unwrap().push(thread);
        0
    }

    #[test]
    fn a_small_stream_runs_on_its_reader_and_a_stranger_s_number_has_ended() {
        begin();
        let list = lists::new(0, 100);
        let stream = super::parallel_stream(list, small, 0, 0);
        let (items, _) = read(stream, usize::MAX);
        assert!(items.iter().copied().eq(0..100));
        super::stream_end(stream);
        let here = i64::from(super::threads::index());
        assert!(SMALL.lock().unwrap().iter().all(|thread| *thread == here));
        let mut chunk = 7;
        assert_eq!(super::stream_next(stream, &mut chunk), 0);
        assert_eq!(chunk, 0);
        super::stream_end(12_345);
        heap::free(list);
        finish();
    }
}
