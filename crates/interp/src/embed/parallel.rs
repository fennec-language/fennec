//! `parallel(xs)` for native code: a loop body, a comprehension and a reduction run over a
//! collection's positions, split into ranges.
//!
//! A range is halved until it's no bigger than a chunk picked from the item's size, so the
//! ranges depend on the length alone, and a reduction's partials combine in the same order
//! whether one thread runs them or many. With the `threads` feature, the ranges of a loop big
//! enough to be worth it run on a work-stealing pool, each worker with its own runtime (see
//! `threads`). Otherwise they run one after another on the calling thread, which gives the
//! same results.
//!
//! Native code gives a body as a function of `(env, from, to, thread)`, where `env` holds what
//! the body reads from around it and `thread` is the running thread's number, for counting
//! (see `threads`). A body gives back 0, or anything else when it failed, with the failure in
//! its thread's runtime. Once one fails, no range after it starts, but those before it still
//! run: the failure passed up is the lowest range's that fails, so it's the same however many
//! threads run.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

use fennec_syntax::Diagnostic;

use super::{State, attempt, with};

/// A loop body over the positions `from..to`.
pub type Body = extern "C" fn(env: i64, from: i64, to: i64, thread: i64) -> i64;

/// A comprehension's body, which writes each position's item into `out`, a list the runtime
/// made with room for all of them.
pub type MapBody = extern "C" fn(env: i64, from: i64, to: i64, thread: i64, out: i64) -> i64;

/// A reduction's body, which writes the range's partial into `partial`.
pub type ReduceBody = extern "C" fn(env: i64, from: i64, to: i64, thread: i64, partial: &mut i64) -> i64;

/// Combines two partials, the earlier range's first, into `out`.
pub type Combine = extern "C" fn(env: i64, a: i64, b: i64, out: &mut i64) -> i64;

/// The most bytes of items a range is split down to.
const CHUNK_BYTES: usize = 16 * 1024;

/// How many positions a range is split down to, for items of `words` words.
fn chunk(words: usize) -> usize {
    (CHUNK_BYTES / (8 * words.max(1))).max(1)
}

fn number(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// How many positions a list has and how many words each takes.
fn extent(state: &State, list: i64) -> Option<(usize, usize)> {
    let block = state.heap.block(list.cast_unsigned())?;
    let element = super::lists::element_in(state, block)?;
    let length = usize::try_from(*block.get(2)?).ok()?;
    Some((length, element.words()))
}

/// One `parallel` call's ranges: where they stop, and the lowest failure.
struct Run {
    chunk: usize,
    /// The thread that started the call, whose runtime adopts what the ranges make.
    caller: u16,
    /// Where the lowest range that failed so far starts. Ranges after it don't start, and
    /// ranges before it still run, since one of them failing is the failure passed up.
    stop: AtomicUsize,
    /// The failure from the lowest range that failed, by where it starts.
    failure: Mutex<Option<(usize, Diagnostic)>>,
    /// What each range made on a worker that outlives it, for the calling thread to adopt.
    made: Mutex<Vec<Box<[u64]>>>,
}

impl Run {
    fn new(chunk: usize) -> Run {
        Run {
            chunk,
            caller: super::threads::index(),
            stop: AtomicUsize::new(usize::MAX),
            failure: Mutex::new(None),
            made: Mutex::new(Vec::new()),
        }
    }

    /// Notes that the range starting at `from` failed, with the failure in this thread's
    /// runtime, keeping the lowest range's.
    fn failed(&self, from: usize) {
        self.stop.fetch_min(from, Ordering::Relaxed);
        let found = with(|state| state.failure.take().map(|failure| (from, failure)));
        super::FAILED.set(0);
        let mut kept = self.failure.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(found) = found
            && kept.as_ref().is_none_or(|(at, _)| found.0 < *at)
        {
            *kept = Some(found);
        }
    }

    /// Keeps what the range just run on this thread made, when this thread is a worker.
    fn keep_made(&self) {
        if super::threads::index() != self.caller {
            let made = super::threads::hand_over();
            self.made.lock().unwrap_or_else(PoisonError::into_inner).extend(made);
        }
    }

    /// Passes the failure up, in the calling thread's runtime, after adopting what the ranges
    /// made: 1 if there was one, else 0.
    fn finish(self) -> i64 {
        super::threads::adopt(self.made.into_inner().unwrap_or_else(PoisonError::into_inner));
        let failure = self.failure.into_inner().unwrap_or_else(PoisonError::into_inner);
        match failure {
            Some((_, failure)) => attempt(|_: &mut State| -> Result<i64, Diagnostic> { Err(failure) }).max(1),
            None => 0,
        }
    }

    /// Runs `leaf` on each range of `from..to`, halving down to the chunk.
    fn each(&self, (from, to): (usize, usize), leaf: &(dyn Fn(usize, usize, i64) -> i64 + Sync)) {
        if from > self.stop.load(Ordering::Relaxed) || from >= to {
            return;
        }
        if to - from <= self.chunk {
            let failed = leaf(from, to, i64::from(super::threads::index())) != 0;
            self.keep_made();
            if failed {
                self.failed(from);
            }
            return;
        }
        let middle = from + (to - from) / 2;
        pool::join(|| self.each((from, middle), leaf), || self.each((middle, to), leaf));
    }

    /// A reduction over `from..to`: each range's partial, combined with the next in order.
    fn reduce(&self, (from, to): (usize, usize), steps: (ReduceBody, Combine, i64)) -> Option<i64> {
        let (body, combine, env) = steps;
        if from > self.stop.load(Ordering::Relaxed) {
            return None;
        }
        let thread = i64::from(super::threads::index());
        if to - from <= self.chunk {
            let mut partial = 0;
            let failed = body(env, number(from), number(to), thread, &mut partial) != 0;
            self.keep_made();
            if failed {
                self.failed(from);
                return None;
            }
            return Some(partial);
        }
        let middle = from + (to - from) / 2;
        let (left, right) =
            pool::join(|| self.reduce((from, middle), steps), || self.reduce((middle, to), steps));
        let mut combined = 0;
        let failed = combine(env, left?, right?, &mut combined) != 0;
        self.keep_made();
        if failed {
            self.failed(from);
            return None;
        }
        Some(combined)
    }
}

/// `fennec_parallel_for(list, body, env)`: `for x in parallel(list)` and `for mut x in
/// parallel(list)`. Gives back 0, or 1 when the body failed.
pub extern "C" fn parallel_for(list: i64, body: Body, env: i64) -> i64 {
    let Some((length, words)) = with(|state| extent(state, list)) else { return 0 };
    let run = Run::new(chunk(words));
    let leaf = |from: usize, to: usize, thread: i64| body(env, number(from), number(to), thread);
    pool::install(length * words, || run.each((0, length), &leaf));
    run.finish()
}

/// `fennec_parallel_map(list, body, env, element) -> list`: `[f(x) for x in parallel(list)]`.
/// The result is a new list owned once of the element numbered `element`, with each item
/// at its position. On a failure it's 0.
pub extern "C" fn parallel_map(list: i64, body: MapBody, env: i64, element: i64) -> i64 {
    let Some((length, words)) = with(|state| extent(state, list)) else { return 0 };
    let out = super::lists::new(element, number(length));
    let run = Run::new(chunk(words));
    let leaf = |from: usize, to: usize, thread: i64| body(env, number(from), number(to), thread, out);
    pool::install(length * words, || run.each((0, length), &leaf));
    if run.finish() != 0 {
        super::heap::free(out);
        return 0;
    }
    out
}

/// `fennec_parallel_reduce(list, body, combine, env, result) -> i64`: a reduction like
/// `sum(f(x) for x in parallel(list))`. Writes what it comes to into `result`, each range's
/// partial combined with the next in a fixed order. Gives back 0, or 1 when it failed. A list
/// with nothing in it leaves `result` as it was.
pub extern "C" fn parallel_reduce(
    list: i64,
    body: ReduceBody,
    combine: Combine,
    env: i64,
    result: &mut i64,
) -> i64 {
    let Some((length, words)) = with(|state| extent(state, list)) else { return 0 };
    if length == 0 {
        return 0;
    }
    let run = Run::new(chunk(words));
    let reduced = pool::install(length * words, || run.reduce((0, length), (body, combine, env)));
    if let Some(reduced) = reduced {
        *result = reduced;
    }
    run.finish()
}

#[cfg(not(any(test, feature = "threads")))]
mod pool {
    //! Without threads, every range runs on the calling thread, one after another.

    /// Without threads, there are no workers to set up.
    pub fn set_worker_setup(_setup: Box<dyn Fn() + Send + Sync>) {}

    pub(super) fn install<R>(_words: usize, run: impl FnOnce() -> R) -> R {
        run()
    }

    pub(super) fn join<A, B>(first: impl FnOnce() -> A, second: impl FnOnce() -> B) -> (A, B) {
        (first(), second())
    }
}

#[cfg(any(test, feature = "threads"))]
mod pool;

pub use pool::set_worker_setup;

#[cfg(test)]
mod tests {
    use std::rc::Rc;
    use std::sync::Mutex;

    use fennec_syntax::{Diagnostic, Span};

    use super::super::{State, Tables, attempt, finish, heap, lists, start, with};
    use crate::packed::Element;
    use crate::value::Value;

    fn tables() -> Tables {
        Tables {
            shapes: vec![Rc::new(Element::Str)],
            elements: vec![Rc::new(Element::Int)],
            ..Tables::default()
        }
    }

    fn begin() {
        super::set_worker_setup(Box::new(|| {
            let _ = start(&fennec_syntax::Module { items: Vec::new() }, tables(), crate::Inputs::default());
        }));
        start(&fennec_syntax::Module { items: Vec::new() }, tables(), crate::Inputs::default()).unwrap();
    }

    /// Enough items to run on the pool.
    const ITEMS: i64 = 100_000;

    extern "C" fn harmonic(_env: i64, from: i64, to: i64, _thread: i64, partial: &mut i64) -> i64 {
        let sum: f64 = (from..to).map(|n| 1.0 / crate::ops::to_float(n + 1)).sum();
        *partial = sum.to_bits().cast_signed();
        0
    }

    extern "C" fn add(_env: i64, a: i64, b: i64, out: &mut i64) -> i64 {
        let sum = f64::from_bits(a.cast_unsigned()) + f64::from_bits(b.cast_unsigned());
        *out = sum.to_bits().cast_signed();
        0
    }

    #[test]
    fn a_reduction_combines_in_an_order_that_depends_on_the_length_alone() {
        begin();
        let list = lists::new(0, ITEMS);
        let mut on_the_pool = 0;
        assert_eq!(super::parallel_reduce(list, harmonic, add, 0, &mut on_the_pool), 0);
        // The same ranges, combined the same way on this thread alone.
        let run = super::Run::new(super::chunk(1));
        let alone = run.reduce((0, usize::try_from(ITEMS).unwrap()), (harmonic, add, 0));
        assert_eq!(Some(on_the_pool), alone);
        heap::free(list);
        finish();
    }

    extern "C" fn fails_twice(_env: i64, from: i64, to: i64, _thread: i64) -> i64 {
        let at = [90_000, 70_000].into_iter().find(|at| (from..to).contains(at));
        let Some(at) = at else { return 0 };
        attempt(|_: &mut State| -> Result<i64, Diagnostic> {
            Err(Diagnostic::new(format!("failed in the range from {from}, at {at}"), Span::default()))
        });
        1
    }

    #[test]
    fn of_several_failures_the_lowest_range_s_is_passed_up() {
        begin();
        let list = lists::new(0, ITEMS);
        assert_eq!(super::parallel_for(list, fails_twice, 0), 1);
        heap::free(list);
        let (_, failure, _) = finish();
        assert!(failure.is_some_and(|failure| failure.message.ends_with("at 70000")));
    }

    static MADE: Mutex<Vec<(i64, i64, i64)>> = Mutex::new(Vec::new());

    extern "C" fn names(_env: i64, from: i64, to: i64, thread: i64) -> i64 {
        if from == 0 || to == ITEMS {
            let name = with(|state| heap::text_address(state, &format!("made for the range from {from}")));
            MADE.lock().unwrap().push((from, name, thread));
        }
        0
    }

    #[test]
    fn what_tasks_make_is_adopted_by_the_thread_that_ran_the_loop() {
        begin();
        let before = with(|state| state.heap.live());
        let list = lists::new(0, ITEMS);
        assert_eq!(super::parallel_for(list, names, 0), 0);
        heap::free(list);
        let made = std::mem::take(&mut *MADE.lock().unwrap());
        assert_eq!(made.len(), 2);
        // The ranges ran on the pool's workers, not on this thread.
        let here = i64::from(super::super::threads::index());
        assert!(made.iter().all(|(_, _, thread)| *thread != here), "{made:?}");
        for (from, name, _) in made {
            let read = with(|state| Some(heap::read_block(state, name.cast_unsigned()))).unwrap();
            assert_eq!(read, Value::str(&format!("made for the range from {from}")));
            heap::free(name);
        }
        assert_eq!(with(|state| state.heap.live()), before);
        finish();
    }
}
