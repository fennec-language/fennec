//! The ledger's counts and the check's rules, on their own: the waits that use them are tested
//! beside the channels and the block ends.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use super::{
    Awaited, CallName, Counts, Kind, Note, ONE_CONTEXT, ONE_GENERATION, ONE_STUCK, RangeName, ThreadNote,
    Who, said,
};

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

/// Something to wait on that says what a test tells it to, and knows one task's number by its stop,
/// as a block's end knows the task it waits for.
struct Told {
    holds: AtomicBool,
    task: Option<(Arc<AtomicU8>, i64)>,
}

impl Awaited for Told {
    fn holds(&self, _kind: Kind) -> bool {
        self.holds.load(Ordering::SeqCst)
    }

    fn said(&self, kind: Kind) -> String {
        format!("for {kind:?}")
    }

    fn number_of(&self, who: &Who) -> Option<i64> {
        let Who::Task(stop) = who else { return None };
        self.task.as_ref().filter(|(its, _)| Arc::ptr_eq(its, stop)).map(|(_, number)| *number)
    }
}

/// A report names each stuck wait on a line of its own under the one line that says what happened:
/// `main` first, then tasks by number, task 2 before task 10, then any it can't name. A note holds
/// only while what it waits on says so.
#[test]
fn a_report_says_who_waits_for_what_and_a_note_asks_what_it_waits_on() {
    let told = Arc::new(Told { holds: AtomicBool::new(true), task: None });
    let on: Arc<dyn Awaited> = told.clone();
    let (second, tenth) = (Arc::new(AtomicU8::new(0)), Arc::new(AtomicU8::new(0)));
    let knows = |stop: &Arc<AtomicU8>, number| -> Arc<dyn Awaited> {
        Arc::new(Told { holds: AtomicBool::new(true), task: Some((Arc::clone(stop), number)) })
    };
    let notes = [
        Note { who: Who::Other, ..Note::parked(Arc::clone(&on), Kind::Send) },
        Note { who: Who::Task(Arc::clone(&tenth)), ..Note::parked(knows(&tenth, 10), Kind::Receive) },
        Note { who: Who::Task(Arc::clone(&second)), ..Note::parked(knows(&second, 2), Kind::Receive) },
        Note { who: Who::Main, ..Note::parked(Arc::clone(&on), Kind::Receive) },
    ];
    assert_eq!(
        said(&notes),
        "error: a deadlock: every task is waiting for another, and nothing can wake them\n  \
         main is waiting for Receive\n  task 2 is waiting for Receive\n  task 10 is waiting for Receive\n  \
         a task is waiting for Send\n"
    );
    let notes = [notes[0].clone(), notes[3].clone()];
    assert!(notes.iter().all(Note::holds));
    told.holds.store(false, Ordering::SeqCst);
    assert!(!notes.iter().any(Note::holds), "what it waits on changed");
}

/// A range of a waiting `parallel` call is named by its call and where it starts, and comes after
/// `main` and the tasks: one call's ranges together, range 2 before range 10. A task's call is named
/// by the task's number as the report finds it, from the notes too, since a launcher come to its
/// block's end no longer has its task among those to be waited for. A call made in a range is
/// named by that range.
#[test]
fn a_report_names_each_range_by_its_call_and_where_it_starts() {
    let on: Arc<dyn Awaited> = Arc::new(Told { holds: AtomicBool::new(true), task: None });
    let stop = Arc::new(AtomicU8::new(0));
    let block_end: Arc<dyn Awaited> =
        Arc::new(Told { holds: AtomicBool::new(true), task: Some((Arc::clone(&stop), 3)) });
    let mains = Arc::new(CallName::By(Who::Main));
    let tasks = Arc::new(CallName::By(Who::Task(stop)));
    let nested = Arc::new(CallName::InRange(RangeName { call: Arc::clone(&mains), from: 4 }));
    let range = |call: &Arc<CallName>, from| Note {
        range: Some(RangeName { call: Arc::clone(call), from }),
        ..Note::parked(Arc::clone(&on), Kind::Receive)
    };
    let notes = [
        range(&mains, 10),
        range(&nested, 1),
        range(&tasks, 0),
        range(&mains, 2),
        Note { who: Who::Main, ..Note::parked(block_end, Kind::BlockEnd) },
    ];
    assert_eq!(
        said(&notes),
        "error: a deadlock: every task is waiting for another, and nothing can wake them\n  \
         main is waiting for BlockEnd\n  \
         range 2 of main's `parallel` call is waiting for Receive\n  \
         range 10 of main's `parallel` call is waiting for Receive\n  \
         range 0 of task 3's `parallel` call is waiting for Receive\n  \
         range 1 of the `parallel` call in range 4 of main's `parallel` call is waiting for Receive\n"
    );
}

/// A channel wait that blocks its thread holds only while no task its code launched has a crash
/// unsaid: a crash wakes the wait without changing the channel, so its note alone would still read
/// as stuck. Once the block's end has said the crash, a wait is a wait again.
///
/// Tested here rather than by a program: with the check taken out, `crash_wakes` reported falsely
/// 25 times in 900 before blocked waits and idle workers waited 1 ms before counting, but no time in
/// 300 since. The worker that would look waits longer than a woken `main` takes to uncount itself,
/// so only a thread held up past that, on a busy machine, now needs the check.
#[test]
fn a_blocked_waits_note_stops_holding_while_a_crash_of_a_task_its_code_launched_is_unsaid() {
    let told: Arc<dyn Awaited> = Arc::new(Told { holds: AtomicBool::new(true), task: None });
    let launcher = Arc::new(super::super::threads::Launcher::default());
    let note = ThreadNote {
        note: Note::parked(told, Kind::Receive),
        launchers: [Some(Arc::clone(&launcher)), None],
    };
    assert!(note.holds(), "nothing's crashed");
    launcher.task_crashed();
    assert!(!note.holds(), "a crash wakes it");
    launcher.crash_said();
    assert!(note.holds(), "once the crash is said, it waits as before");
}
