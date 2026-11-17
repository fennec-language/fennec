//! The ledger's counts and the check's rules, on their own: the waits that use them are tested
//! beside the channels and the block ends.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{Awaited, Counts, Kind, Note, ONE_CONTEXT, ONE_GENERATION, ONE_STUCK, Who, said};

/// The counts read back as they were packed, and a second uncount, borrowing from the contexts'
/// field, never reads as every context stuck.
#[test]
fn counts_pack_and_a_double_uncount_never_reads_as_all_stuck() {
    let two_of_two = Counts::of(2 * ONE_CONTEXT + 2 * ONE_STUCK + 5 * ONE_GENERATION);
    assert_eq!((two_of_two.contexts, two_of_two.stuck, two_of_two.generation), (2, 2, 5));
    assert!(two_of_two.all_stuck());
    assert!(!Counts::of(2 * ONE_CONTEXT + ONE_STUCK).all_stuck(), "one of two stuck");
    assert!(!Counts::of(0).all_stuck(), "no context at all");
    // One stuck wait uncounted twice: the stuck field borrows, and reads as far more than the
    // contexts, which isn't all stuck.
    let borrowed =
        Counts::of((ONE_CONTEXT + ONE_STUCK).wrapping_add(ONE_GENERATION.wrapping_sub(ONE_STUCK) * 2));
    assert!(!borrowed.all_stuck(), "{borrowed:?}");
}

/// Something to wait on that says what a test tells it to.
struct Told {
    holds: AtomicBool,
}

impl Awaited for Told {
    fn holds(&self, _kind: Kind) -> bool {
        self.holds.load(Ordering::SeqCst)
    }

    fn said(&self, kind: Kind) -> String {
        format!("for {kind:?}")
    }
}

/// A report names each stuck wait on a line of its own, sorted, under the one line that says what
/// happened; and a note holds only while what it waits on says so.
#[test]
fn a_report_says_who_waits_for_what_and_a_note_asks_what_it_waits_on() {
    let told = Arc::new(Told { holds: AtomicBool::new(true) });
    let on: Arc<dyn Awaited> = told.clone();
    let notes = [
        Note { who: Who::Other, ..Note::here(Arc::clone(&on), Kind::Send) },
        Note { who: Who::Main, ..Note::here(Arc::clone(&on), Kind::Receive) },
    ];
    assert_eq!(
        said(&notes),
        "error: a deadlock: every task is waiting for another, and nothing can wake them\n  \
         a task is waiting for Send\n  main is waiting for Receive\n"
    );
    assert!(notes.iter().all(Note::holds));
    told.holds.store(false, Ordering::SeqCst);
    assert!(!notes.iter().any(Note::holds), "what it waits on changed");
}
