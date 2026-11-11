//! A crash inside a `with` runs its source's cleanup, `lend x else:` and the rest of the source, as a
//! failure of the block would, before the crash goes on out. Native code reaches that cleanup on its
//! way out of the function, at its one failure exit, innermost first, beside the rollback of locks.
//!
//! The crash is held aside while the cleanup runs (`hold`), so the cleanup's own calls run as
//! normal, and put back after (`resume`). It's held by a number native code keeps in its frame,
//! not by the thread: a cleanup may park, a rollback over the network say, and other tasks run on
//! the thread meanwhile.

use std::collections::HashMap;
use std::io::Write as _;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, PoisonError};

use fennec_syntax::Diagnostic;

use super::{set_failed, threads, with};

/// The crashes held aside while their cleanups run, by the number native code keeps.
static HELD: Mutex<Option<HashMap<i64, Held>>> = Mutex::new(None);
static NEXT: AtomicI64 = AtomicI64::new(1);

/// A crash held aside: its failure, and its call chain so far.
type Held = (Option<Diagnostic>, fennec_syntax::Trace);

fn held<T>(use_it: impl FnOnce(&mut HashMap<i64, Held>) -> T) -> T {
    use_it(HELD.lock().unwrap_or_else(PoisonError::into_inner).get_or_insert_with(HashMap::new))
}

/// `fennec_crash_hold()`: a crash's cleanup starts. The crash is taken aside, and the failure flag
/// cleared, until `fennec_crash_resume` is given the number this gives back. A stop doesn't cut the
/// cleanup's waits short until its grace is up (see `serve::launch`).
pub extern "C" fn crash_hold() -> i64 {
    let failure = super::take_failure();
    set_failed(0);
    threads::cleaning(true);
    let number = NEXT.fetch_add(1, Ordering::Relaxed);
    held(|held| held.insert(number, failure));
    number
}

/// `fennec_crash_resume(held)`: the cleanup's ended, and the crash goes on. If the cleanup crashed
/// too, that's reported here, and the first crash is the one that goes on.
pub extern "C" fn crash_resume(number: i64) {
    let crash = held(|held| held.remove(&number)).unwrap_or_default();
    threads::cleaning(false);
    if let Some(again) = with(|state| state.failure.take()) {
        super::write_kept_output();
        let _ = writeln!(std::io::stderr(), "error: {} (while cleaning up after a crash)", again.message);
    }
    super::put_failure(crash);
    set_failed(1);
}

/// `fennec_crash_note(text)`: a crash's cleanup ended, giving back `text`, a string's block it lets
/// go of: how it failed, said under the crash as a note, as a failed block's cleanup is noted on
/// its failure, or nothing when it didn't fail.
pub extern "C" fn crash_note(text: i64) {
    let Ok(at) = u64::try_from(text) else { return };
    let note = with(|state| {
        let note = state.heap.block(at).map(super::heap::block_text);
        super::heap::release(state, at);
        note
    });
    if let Some(note) = note.filter(|note| !note.is_empty()) {
        super::write_kept_output();
        let _ = writeln!(std::io::stderr(), "note: {note} (while cleaning up after a crash)");
    }
}

#[cfg(test)]
mod tests;
