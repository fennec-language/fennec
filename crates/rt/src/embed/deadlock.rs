//! Finding a deadlock: when `main` and every task wait on each other for good, the program stops and
//! says who waits for what, as Go's "all goroutines are asleep" does.
//!
//! A false report would stop a program that could still move, so the counts here only say when to
//! look, and [`look`] is the proof. The counts are the contexts that run the program's code, and
//! how many of them are in a stuck wait: a channel's send or receive, or a block's end, with no time
//! limit. A check fires only when every context is stuck, every worker has gone to sleep with
//! nothing but stuck waits, the notes they left number the contexts and each still holds, the
//! blocking threads are idle, and nothing changed while it looked. A count gone wrong costs a late or
//! missed report, never a stopped program.
//!
//! The design, with the review it was built to: `deadlock-detection-design-2026-11-16-runtime.md`
//! in the lanes' notes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// Bits for each of the two counts in [`COUNTS`]; the generation has the rest.
const FIELD: u32 = 21;
const ONE_CONTEXT: u64 = 1;
const ONE_STUCK: u64 = 1 << FIELD;
const ONE_GENERATION: u64 = 1 << (2 * FIELD);
const MASK: u64 = ONE_STUCK - 1;

/// The contexts, the stuck waits among them, and a generation each end and each uncount bumps, in
/// one word, so one atomic operation changes and reads them all.
static COUNTS: AtomicU64 = AtomicU64::new(0);

/// The counts as one look found them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Counts {
    pub(crate) contexts: u64,
    pub(crate) stuck: u64,
    generation: u64,
}

impl Counts {
    fn of(word: u64) -> Counts {
        Counts { contexts: word & MASK, stuck: (word >> FIELD) & MASK, generation: word >> (2 * FIELD) }
    }

    /// Whether these say every context is stuck: at least one, and no more stuck than there are,
    /// which a second uncount of one wait, borrowing from the other count, would make.
    pub(crate) fn all_stuck(self) -> bool {
        self.contexts > 0 && self.stuck == self.contexts
    }
}

pub(crate) fn counts() -> Counts {
    Counts::of(COUNTS.load(Ordering::SeqCst))
}

/// Something that runs the program's code has begun: `main`, a task, a job, a piece of mail.
pub(crate) fn context_began() {
    COUNTS.fetch_add(ONE_CONTEXT, Ordering::SeqCst);
}

#[cfg(any(test, feature = "threads"))]
/// A context has ended. It's a change, so the generation moves.
pub(crate) fn context_ended() {
    COUNTS.fetch_add(ONE_GENERATION.wrapping_sub(ONE_CONTEXT), Ordering::SeqCst);
}

/// A context is now in a stuck wait: gives the counts as it left them.
pub(crate) fn stuck_began() -> Counts {
    Counts::of(COUNTS.fetch_add(ONE_STUCK, Ordering::SeqCst).wrapping_add(ONE_STUCK))
}

/// A stuck wait has ended, or is about to: the generation moves with it, in the same operation.
pub(crate) fn stuck_ended() {
    COUNTS.fetch_add(ONE_GENERATION.wrapping_sub(ONE_STUCK), Ordering::SeqCst);
}

#[cfg(any(test, feature = "threads"))]
/// A context for as long as it's held: for a job, from its push until it has run.
pub(crate) struct Context;

#[cfg(any(test, feature = "threads"))]
impl Context {
    pub(crate) fn began() -> Context {
        context_began();
        Context
    }
}

#[cfg(any(test, feature = "threads"))]
impl Drop for Context {
    fn drop(&mut self) {
        context_ended();
    }
}

/// The kinds of stuck wait.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    /// A receive from a channel with nothing in it.
    Receive,
    /// A send to a channel with no room.
    Send,
    /// The end of a `with launch` block, for its task.
    #[cfg_attr(not(any(test, feature = "threads")), expect(dead_code, reason = "only tasks are launched"))]
    BlockEnd,
}

/// What a stuck wait waits on: a channel, or a launched task.
pub(crate) trait Awaited: Send + Sync {
    /// Whether a wait of `kind` on it would still find nothing to do: for a receive, the channel's
    /// empty and open; for a send, full and open; for a block's end, the task hasn't ended.
    fn holds(&self, kind: Kind) -> bool;
    /// What a wait of `kind` on it waits for, as the report says it, after "is waiting".
    fn said(&self, kind: Kind) -> String;

    /// The number of the task whose stop is `stop`, if it's that task: a block's end knows the one
    /// it waits for, which its launcher has already taken out of those any wait can find.
    fn number_of(&self, _stop: &Arc<std::sync::atomic::AtomicU8>) -> Option<i64> {
        None
    }
}

/// Who's in a stuck wait.
#[derive(Clone)]
pub(crate) enum Who {
    Main,
    /// A launched task, by its stop, which finds its number when it's said.
    #[cfg_attr(not(any(test, feature = "threads")), expect(dead_code, reason = "only tasks are launched"))]
    Task(Arc<std::sync::atomic::AtomicU8>),
    /// Anything else: a server's round, or a range of a `parallel` call. Neither is in a report
    /// yet: a worker serving isn't idle, and a call's caller counts as running while it waits.
    #[cfg_attr(
        not(any(test, feature = "threads")),
        expect(dead_code, reason = "only a pool's workers run these")
    )]
    Other,
}

impl Who {
    /// Who's running here.
    pub(crate) fn here() -> Who {
        #[cfg(any(test, feature = "threads"))]
        {
            if let Some(stop) = super::threads::task_stop().filter(|_| super::threads::in_a_task()) {
                return Who::Task(stop);
            }
            if super::parallel::here().is_some() {
                return Who::Other;
            }
        }
        Who::Main
    }

    /// Its name in a report of `notes`: a task by its number, found among the tasks still to be
    /// waited for, or else from the block's end waiting for it.
    fn said(&self, notes: &[Note]) -> String {
        match self {
            Who::Main => "main".to_owned(),
            Who::Task(stop) => {
                #[cfg(any(test, feature = "threads"))]
                let number = super::serve::task_number(stop);
                #[cfg(not(any(test, feature = "threads")))]
                let number = None;
                match number.or_else(|| notes.iter().find_map(|note| note.on.number_of(stop))) {
                    Some(number) => format!("task {number}"),
                    None => "a task".to_owned(),
                }
            }
            Who::Other => "a task".to_owned(),
        }
    }
}

/// What a stuck wait leaves for the check: who waits, on what, for which.
#[derive(Clone)]
pub(crate) struct Note {
    who: Who,
    on: Arc<dyn Awaited>,
    kind: Kind,
    /// For a wait that blocks its thread, the launchers a crash of whose tasks wakes it, which a
    /// crash does without changing what it waits on: it holds only while none has. A parked wait's
    /// crash wake is mail, which counts as running, and takes it out of its worker's waits.
    #[cfg(any(test, feature = "threads"))]
    launchers: Vec<Arc<super::threads::Launcher>>,
}

impl Note {
    /// The note of a parked wait of `kind` on `on`, by whoever's running here.
    pub(crate) fn here(on: Arc<dyn Awaited>, kind: Kind) -> Note {
        Note {
            who: Who::here(),
            on,
            kind,
            #[cfg(any(test, feature = "threads"))]
            launchers: Vec::new(),
        }
    }

    /// The note of a wait of `kind` on `on` that blocks this thread.
    pub(crate) fn blocking(on: Arc<dyn Awaited>, kind: Kind) -> Note {
        Note {
            #[cfg(any(test, feature = "threads"))]
            launchers: super::threads::Launcher::those_here(),
            ..Note::here(on, kind)
        }
    }

    fn holds(&self) -> bool {
        #[cfg(any(test, feature = "threads"))]
        if self.launchers.iter().any(|launcher| launcher.cut_short()) {
            return false;
        }
        self.on.holds(self.kind)
    }

    fn said(&self, notes: &[Note]) -> String {
        format!("  {} is waiting {}", self.who.said(notes), self.on.said(self.kind))
    }
}

/// The notes of stuck waits that block their threads, by a number each was given.
static THREAD_NOTES: Mutex<Option<HashMap<u64, Note>>> = Mutex::new(None);
static NEXT_THREAD_NOTE: AtomicU64 = AtomicU64::new(1);

/// A thread is about to block in a stuck wait, leaving `note`, and counts it: gives the number to
/// take it back by, and whether the counts now say every context is stuck, so it should look.
pub(crate) fn thread_waits(note: Note) -> (u64, bool) {
    let number = NEXT_THREAD_NOTE.fetch_add(1, Ordering::Relaxed);
    THREAD_NOTES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get_or_insert_with(HashMap::new)
        .insert(number, note);
    (number, stuck_began().all_stuck())
}

/// A thread's stuck wait has ended, or is about to: its note goes, and it's uncounted.
pub(crate) fn thread_woke(number: u64) {
    if let Some(notes) = THREAD_NOTES.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
        notes.remove(&number);
    }
    stuck_ended();
}

/// What a worker published, in a slot of its own, as it last went to sleep: whether it had nothing
/// left but stuck waits, and their notes. Each pool's workers have one each (see `Pool`), so going to
/// sleep takes no lock but the worker's own.
#[cfg(any(test, feature = "threads"))]
#[derive(Default)]
pub(crate) struct Published {
    idle: bool,
    notes: Vec<Note>,
}

#[cfg(any(test, feature = "threads"))]
impl Published {
    /// Whether it's idle, with the notes of its waits when it is.
    pub(crate) fn set(&mut self, idle: bool, notes: impl Iterator<Item = Note>) {
        self.idle = idle;
        self.notes.clear();
        if idle {
            self.notes.extend(notes);
        }
    }

    /// Its worker has woken.
    pub(crate) fn woke(&mut self) {
        self.idle = false;
        self.notes.clear();
    }
}

/// Every pool made, whose workers' slots the check reads.
#[cfg(any(test, feature = "threads"))]
static POOLS: Mutex<Vec<&'static super::parallel::Pool>> = Mutex::new(Vec::new());

/// A pool's been made: the check reads its workers from now on.
#[cfg(any(test, feature = "threads"))]
pub(in crate::embed) fn pool_made(pool: &'static super::parallel::Pool) {
    POOLS.lock().unwrap_or_else(PoisonError::into_inner).push(pool);
}

/// A worker going to sleep with nothing but `stuck` stuck waits counts them, so the counts can say
/// when every context is stuck. A worker's parked waits are counted only while it sleeps: while it's
/// awake, the program's still moving.
#[cfg(any(test, feature = "threads"))]
pub(crate) fn sleeps_stuck(stuck: u64) {
    if stuck > 0 {
        COUNTS.fetch_add(stuck.wrapping_mul(ONE_STUCK), Ordering::SeqCst);
    }
}

/// The worker that counted `stuck` as it went to sleep has woken. It's a change, so the generation
/// moves with it.
#[cfg(any(test, feature = "threads"))]
pub(crate) fn woke_stuck(stuck: u64) {
    if stuck > 0 {
        COUNTS.fetch_add(ONE_GENERATION.wrapping_sub(stuck.wrapping_mul(ONE_STUCK)), Ordering::SeqCst);
    }
}

/// What a deadlock says: one line, then one for each stuck wait, in the order of who waits.
fn said(notes: &[Note]) -> String {
    let mut lines: Vec<String> = notes.iter().map(|note| note.said(notes)).collect();
    lines.sort();
    let mut text =
        "error: a deadlock: every task is waiting for another, and nothing can wake them\n".to_owned();
    for line in lines {
        text.push_str(&line);
        text.push('\n');
    }
    text
}

/// Looks for a deadlock, where the counts say there may be one: from a worker going to sleep, or a
/// thread about to block in a stuck wait, holding no lock a note looks at, nor `THREAD_NOTES` or a
/// worker's slot. Stops the program if it's proven; otherwise does nothing.
pub(crate) fn look() {
    let before = counts();
    if !before.all_stuck() || crate::replay::active() {
        return;
    }
    let Some(notes) = proven(before) else { return };
    report(&notes);
}

/// The notes of every stuck wait, if they prove a deadlock with the counts at `before`: every
/// worker asleep with nothing but stuck waits and no mail, as many notes as contexts, each still
/// holding, the blocking threads idle, and the counts as they were.
fn proven(before: Counts) -> Option<Vec<Note>> {
    #[cfg(any(test, feature = "threads"))]
    let pools = POOLS.lock().unwrap_or_else(PoisonError::into_inner).clone();
    // Mail on its way, a wake or a stop, is the program still moving: looked at first, as it's what
    // most often says so, and without a lock.
    #[cfg(any(test, feature = "threads"))]
    if pools.iter().any(|pool| pool.has_mail()) {
        return None;
    }
    #[cfg_attr(not(any(test, feature = "threads")), expect(unused_mut, reason = "only workers add notes"))]
    let mut notes: Vec<Note> = THREAD_NOTES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .map(|notes| notes.values().cloned().collect())
        .unwrap_or_default();
    #[cfg(any(test, feature = "threads"))]
    for published in pools.iter().flat_map(|pool| pool.published()) {
        let published = published.lock().unwrap_or_else(PoisonError::into_inner);
        if !published.idle {
            return None;
        }
        notes.extend(published.notes.iter().cloned());
    }
    let all_noted = u64::try_from(notes.len()).is_ok_and(|noted| noted == before.contexts);
    if !all_noted || !notes.iter().all(Note::holds) || !super::serve::blocking_idle() {
        return None;
    }
    // A blocking thread's job sends its wake as mail before the thread's idle, so with the blocking
    // threads found idle, any such wake is in a worker's mail by now.
    #[cfg(any(test, feature = "threads"))]
    if pools.iter().any(|pool| pool.has_mail()) {
        return None;
    }
    (counts() == before).then_some(notes)
}

/// In the runtime's own tests nothing's reported: `main` isn't a context there, and the tests run
/// side by side, so the counts mean nothing, and a report would stop every test at once. The
/// programs' tests are where deadlocks are found and said.
#[cfg(test)]
fn report(_notes: &[Note]) {}

/// Stops the program for a deadlock: what it printed goes out first, then what each waits for.
#[cfg(not(test))]
fn report(notes: &[Note]) {
    use std::io::Write as _;
    super::write_kept_output();
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().write_all(said(notes).as_bytes());
    super::aot::stop_now(1);
}

#[cfg(test)]
mod tests;
