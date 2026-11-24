//! Finding a deadlock: when `main` and every task wait on each other for good, the program stops and
//! says who waits for what, as Go's "all goroutines are asleep" does.
//!
//! A false report would stop a program that could still move, so the counts here only say when to
//! look, and [`look`] is the proof. The counts are the contexts that run the program's code, and
//! how many of them are in a stuck wait: a channel's send or receive, a block's end, or the join of
//! a `parallel` call whose ranges can wait, with no time limit. A check fires only when every
//! context is stuck, every worker has gone to sleep with nothing but stuck waits and no job queued,
//! the notes they left number the contexts and each still holds, the blocking threads are idle, and
//! nothing changed while it looked. A count gone wrong costs a late or missed report, never a
//! stopped program.
//!
//! The designs, with the reviews they were built to, in the lanes' notes:
//! `deadlock-detection-design-2026-11-16-runtime.md`, and for `parallel` joins
//! `deadlock-phase2-design-2026-11-18-runtime.md`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// How long a wait that blocks its thread waits before it counts as a stuck one, noted and counted
/// for the check: most end sooner, and a thread that blocks often, as `main` sending to a busy
/// task's inbox does, would otherwise note and count every one. A deadlock with such a wait in it
/// is said this much later.
pub(crate) const PATIENCE: Duration = Duration::from_millis(1);

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
    /// The join of a `parallel` call whose body may wait, for its ranges.
    #[cfg_attr(not(any(test, feature = "threads")), expect(dead_code, reason = "only the pool runs ranges"))]
    Join,
    /// A replay's wait for its turn at a place, where the recording has another's next: when every
    /// context is stuck, no other can come to it, and the replay can't follow its recording.
    Turn,
}

/// What a stuck wait waits on: a channel, or a launched task.
pub(crate) trait Awaited: Send + Sync {
    /// Whether a wait of `kind` on it would still find nothing to do: for a receive, the channel's
    /// empty and open; for a send, full and open; for a block's end, the task hasn't ended.
    fn holds(&self, kind: Kind) -> bool;
    /// What a wait of `kind` on it waits for, as the report says it, after "is waiting".
    fn said(&self, kind: Kind) -> String;

    /// The number of the task `who` is, if it's the task this is: a block's end knows the one it
    /// waits for, which its launcher has already taken out of those any wait can find.
    fn number_of(&self, _who: &Who) -> Option<i64> {
        None
    }

    /// Its name in a recording or a replay, for a stuck wait's line there: a launched task's, or a
    /// `parallel` call's. `None` in a normal run.
    fn replay_place(&self) -> Option<Arc<str>> {
        None
    }
}

impl Kind {
    /// The word for it in a stuck wait's line in a recording.
    fn word(self) -> &'static str {
        match self {
            Kind::Receive => "receive",
            Kind::Send => "send",
            Kind::BlockEnd => "end",
            Kind::Join => "join",
            Kind::Turn => "turn",
        }
    }
}

/// Who's in a stuck wait.
#[derive(Clone)]
pub(crate) enum Who {
    Main,
    /// A launched task in a wait that blocks its thread, by its stop, which finds its number when
    /// it's said.
    #[cfg_attr(not(any(test, feature = "threads")), expect(dead_code, reason = "only tasks are launched"))]
    Task(Arc<std::sync::atomic::AtomicU8>),
    /// A parked task: its worker's pool, by address, the worker's place in it and the task's number
    /// there, which the worker fills in as it parks, and a launched task's name is found from.
    #[cfg_attr(not(any(test, feature = "threads")), expect(dead_code, reason = "only workers park"))]
    Parked {
        pool: usize,
        at: usize,
        task: u64,
    },
    /// Anything else: a server's round, or a range of a `parallel` call. A server's round is in no
    /// report, as a worker serving isn't idle. A range is named by its call and where it starts,
    /// from its note's `RangeName`.
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

    /// Its name in a report of `notes`, with where it goes among the report's lines: `main` first,
    /// then tasks by number, so task 2 comes before task 10. A task's number is found among the
    /// tasks still to be waited for, or else from the block's end waiting for it.
    fn said(&self, notes: &[Note]) -> ((u8, i64), String) {
        if let Who::Main = self {
            return ((0, 0), "main".to_owned());
        }
        #[cfg(any(test, feature = "threads"))]
        let number = super::serve::task_number(self);
        #[cfg(not(any(test, feature = "threads")))]
        let number = None;
        match number.or_else(|| notes.iter().find_map(|note| note.number_of(self))) {
            Some(number) => ((1, number), format!("task {number}")),
            None => ((2, 0), "a task".to_owned()),
        }
    }
}

/// What a stuck wait waits on.
#[derive(Clone)]
enum On {
    /// A channel, by its number, found again only when a check looks: a parked wait's note then
    /// shares nothing with the threads sending and receiving on it.
    Channel(i64),
    /// A launched task, at a block's end.
    Awaited(Arc<dyn Awaited>),
}

/// What a stuck wait leaves for the check: who waits, on what, for which, and, for a range of a
/// waiting `parallel` call, how the report names it.
#[derive(Clone)]
pub(crate) struct Note {
    who: Who,
    on: On,
    kind: Kind,
    range: Option<RangeName>,
    /// In a recording or a replay, its line there, `task kind place`, made by the task that waits,
    /// where its name is at hand: a report holds a replay's lines to the recording's.
    replay: Option<Arc<str>>,
}

/// A range of a waiting `parallel` call, as a report names it: "range 3 of main's `parallel` call",
/// by its call and the item it starts at.
#[derive(Clone)]
pub(crate) struct RangeName {
    pub(crate) call: Arc<CallName>,
    pub(crate) from: usize,
}

impl RangeName {
    fn said(&self, notes: &[Note]) -> String {
        format!("range {} of {}", self.from, self.call.said(notes))
    }
}

/// A waiting `parallel` call, as a report names it, by who made it: "main's `parallel` call", or
/// "the `parallel` call in range 3 of task 1's `parallel` call". It's said when the report is, as
/// a task's number may be found only from the notes then: a task's launcher that has come to its
/// block's end no longer has it among the tasks to be waited for.
pub(crate) enum CallName {
    By(Who),
    InRange(RangeName),
}

impl CallName {
    /// The call that the code running here makes.
    pub(crate) fn here() -> Arc<CallName> {
        Arc::new(range_here().map_or_else(|| CallName::By(Who::here()), CallName::InRange))
    }

    /// Where its maker goes among a report's lines: for a call made in a range, that range's
    /// call's maker.
    fn maker(&self, notes: &[Note]) -> (u8, i64) {
        match self {
            CallName::By(who) => who.said(notes).0,
            CallName::InRange(range) => range.call.maker(notes),
        }
    }

    fn said(&self, notes: &[Note]) -> String {
        match self {
            CallName::By(who) => format!("{}'s `parallel` call", who.said(notes).1),
            CallName::InRange(range) => format!("the `parallel` call in {}", range.said(notes)),
        }
    }
}

/// The name of the range running here, if it's a range of a waiting `parallel` call.
fn range_here() -> Option<RangeName> {
    #[cfg(any(test, feature = "threads"))]
    return super::threads::range_named();
    #[cfg(not(any(test, feature = "threads")))]
    None
}

impl Note {
    /// What a replay's wait for its turn says, when every context is stuck: the turn it waits for
    /// that nothing left running can come to. `None` for any other wait.
    #[cfg_attr(test, expect(dead_code, reason = "only a report, which tests don't make, says it"))]
    fn turn_never_comes(&self) -> Option<String> {
        match (&self.on, self.kind) {
            (On::Awaited(on), Kind::Turn) => Some(on.said(Kind::Turn)),
            _ => None,
        }
    }

    /// The note of a parked wait of `kind` on the channel numbered `channel`, whose worker says who
    /// it is as the task parks (`parked_by`): nothing's looked up or shared on the way to a park.
    /// `place` is the channel's name in a recording or a replay.
    pub(crate) fn on_channel(channel: i64, kind: Kind, place: Option<&str>) -> Note {
        let replay = place.map(|place| crate::replay::line(kind.word(), place));
        Note { who: Who::Other, on: On::Channel(channel), kind, range: range_here(), replay }
    }

    /// The note of a parked wait of `kind` on `on`, as `on_channel`'s.
    pub(crate) fn parked(on: Arc<dyn Awaited>, kind: Kind) -> Note {
        let replay = on.replay_place().map(|place| crate::replay::line(kind.word(), &place));
        Note { who: Who::Other, on: On::Awaited(on), kind, range: range_here(), replay }
    }

    /// The same note, of a wait that blocks this thread, by whoever's running here.
    pub(crate) fn blocking(self) -> Note {
        Note { who: Who::here(), ..self }
    }

    /// The task this parked wait is, as its worker has it.
    #[cfg(any(test, feature = "threads"))]
    pub(crate) fn parked_by(&mut self, pool: usize, at: usize, task: u64) {
        self.who = Who::Parked { pool, at, task };
    }

    fn holds(&self) -> bool {
        match &self.on {
            On::Channel(channel) => super::channels::still_waits_on(*channel, self.kind),
            On::Awaited(on) => on.holds(self.kind),
        }
    }

    fn number_of(&self, who: &Who) -> Option<i64> {
        match &self.on {
            On::Channel(_) => None,
            On::Awaited(on) => on.number_of(who),
        }
    }

    fn said(&self, notes: &[Note]) -> (Order, String) {
        // A range is named by its call and where it starts, and comes after `main` and the tasks,
        // with the other ranges of its call, by where it starts.
        let (order, who) = if let Some(range) = &self.range {
            let call = range.call.said(notes);
            let who = format!("range {} of {call}", range.from);
            // Calls in the order of who made them, as their lines are: `main`'s, then task 2's
            // before task 10's.
            let (rank, number) = range.call.maker(notes);
            ((3, 0, (rank, number, call), i64::try_from(range.from).unwrap_or(i64::MAX)), who)
        } else {
            let ((rank, number), who) = self.who.said(notes);
            ((rank, number, (0, 0, String::new()), 0), who)
        };
        let what = match &self.on {
            On::Channel(channel) => match self.kind {
                Kind::Send => "to send on a channel that's full".to_owned(),
                Kind::Receive if super::channels::is_listener(*channel) => {
                    "to receive as a listener of a channel to `.Every`, with nothing new for it".to_owned()
                }
                Kind::Receive | Kind::BlockEnd | Kind::Join | Kind::Turn => {
                    "to receive on a channel that's empty".to_owned()
                }
            },
            On::Awaited(on) => on.said(self.kind),
        };
        (order, format!("  {who} is waiting {what}"))
    }
}

/// The note of a stuck wait that blocks its thread, with the launchers a crash of whose tasks wakes
/// it, when that's a channel's wait: a crash wakes it without changing what it waits on, so it holds
/// only while none has. A parked wait's crash wake is mail, which the check sees, and which takes it
/// out of its worker's waits.
#[derive(Clone)]
struct ThreadNote {
    note: Note,
    #[cfg(any(test, feature = "threads"))]
    launchers: [Option<Arc<super::threads::Launcher>>; 2],
}

impl ThreadNote {
    fn holds(&self) -> bool {
        #[cfg(any(test, feature = "threads"))]
        if self.launchers.iter().flatten().any(|launcher| launcher.cut_short()) {
            return false;
        }
        self.note.holds()
    }
}

/// A thread's slot for the note of the stuck wait it's blocked in, while it is: a thread is in one
/// wait at a time. Each thread that ever blocks in one has a slot of its own, made the first time,
/// so a wait takes no lock but its own, and the check reads every thread's.
type ThreadSlot = Arc<Mutex<Option<ThreadNote>>>;

/// Every slot of a thread that's still running.
static THREAD_SLOTS: Mutex<Vec<std::sync::Weak<Mutex<Option<ThreadNote>>>>> = Mutex::new(Vec::new());

thread_local! {
    static THREAD_SLOT: ThreadSlot = {
        let slot = ThreadSlot::default();
        let mut slots = THREAD_SLOTS.lock().unwrap_or_else(PoisonError::into_inner);
        slots.retain(|slot| slot.strong_count() > 0);
        slots.push(Arc::downgrade(&slot));
        slot
    };
}

/// A thread is about to block in a stuck wait, leaving `note`, and counts it: gives whether the
/// counts now say every context is stuck, so it should look. A wait a crash of a task its code
/// launched wakes says so, `crash_wakes`: a channel's.
pub(crate) fn thread_waits(note: Note, crash_wakes: bool) -> bool {
    #[cfg(any(test, feature = "threads"))]
    let launchers = if crash_wakes { super::threads::Launcher::those_here() } else { [None, None] };
    #[cfg(not(any(test, feature = "threads")))]
    let _ = crash_wakes;
    let note = ThreadNote {
        note,
        #[cfg(any(test, feature = "threads"))]
        launchers,
    };
    THREAD_SLOT.with(|slot| *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(note));
    stuck_began().all_stuck()
}

/// This thread's note goes, for a stuck wait whose waker already uncounted it: a join the last job
/// of its call woke.
#[cfg(any(test, feature = "threads"))]
pub(crate) fn thread_unnoted() {
    let taken = THREAD_SLOT.with(|slot| slot.lock().unwrap_or_else(PoisonError::into_inner).take());
    drop(taken);
}

/// This thread's stuck wait has ended, or is about to: its note goes, and it's uncounted.
pub(crate) fn thread_woke() {
    let taken = THREAD_SLOT.with(|slot| slot.lock().unwrap_or_else(PoisonError::into_inner).take());
    drop(taken);
    stuck_ended();
}

/// What a worker published, in a slot of its own, as it last went to sleep: whether it had nothing
/// left but stuck waits, and their notes. Each pool's workers have one each (see `Pool`), so going to
/// sleep takes no lock but the worker's own.
///
/// It copies its notes only when a check could pass: when the counts it left say every context is
/// stuck, or a check found its notes missing and asked for them, waking it to. Copying every note
/// each time a worker went to sleep was most of what finding deadlocks cost a program that never
/// has one.
#[cfg(any(test, feature = "threads"))]
#[derive(Default)]
pub(crate) struct Published {
    idle: bool,
    /// Whether `notes` are this sleep's: they're left out while no check could pass.
    noted: bool,
    /// Whether a check found it idle with its notes left out, and asked for them.
    asked: bool,
    notes: Vec<Note>,
}

#[cfg(any(test, feature = "threads"))]
impl Published {
    /// Whether it's idle, and when it is and a check could pass, `notes`, its waits'.
    pub(crate) fn set(&mut self, idle: bool, may_pass: bool, notes: impl Iterator<Item = Note>) {
        self.idle = idle;
        self.noted = idle && (may_pass || self.asked);
        self.asked = false;
        self.notes.clear();
        if self.noted {
            self.notes.extend(notes);
        }
    }

    /// Its worker has woken.
    pub(crate) fn woke(&mut self) {
        self.idle = false;
        self.noted = false;
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
/// when every context is stuck, and gives the counts it left. A worker's parked waits are counted
/// only while it sleeps: while it's awake, the program's still moving.
#[cfg(any(test, feature = "threads"))]
pub(crate) fn sleeps_stuck(stuck: u64) -> Counts {
    if stuck == 0 {
        return counts();
    }
    Counts::of(
        COUNTS
            .fetch_add(stuck.wrapping_mul(ONE_STUCK), Ordering::SeqCst)
            .wrapping_add(stuck.wrapping_mul(ONE_STUCK)),
    )
}

/// A worker that went to sleep idle, counting `stuck`, has woken. It's a change, so the generation
/// moves with it, stuck waits or none: a check that read its slot as idle then fails on the counts,
/// whatever its mail does.
#[cfg(any(test, feature = "threads"))]
pub(crate) fn woke_idle(stuck: u64) {
    COUNTS.fetch_add(ONE_GENERATION.wrapping_sub(stuck.wrapping_mul(ONE_STUCK)), Ordering::SeqCst);
}

/// Where a stuck wait's line goes in a report: `main`, then tasks by number, then ranges by their
/// call and where they start.
type Order = (u8, i64, (u8, i64, String), i64);

/// Each stuck wait's line in a recording or a replay, in order: the same deadlock gives the same
/// lines however its run went. A wait whose line isn't known, as one begun before the run was
/// recorded, is `?`, which no recording's deadlock has.
#[cfg_attr(test, expect(dead_code, reason = "only a report, which tests don't make, says them"))]
fn replay_lines(notes: &[Note]) -> Vec<String> {
    let mut lines: Vec<String> =
        notes.iter().map(|note| note.replay.as_deref().unwrap_or("?").to_owned()).collect();
    lines.sort();
    lines
}

/// What a deadlock says: one line, then one for each stuck wait, in the order of who waits.
fn said(notes: &[Note]) -> String {
    let mut lines: Vec<(Order, String)> = notes.iter().map(|note| note.said(notes)).collect();
    lines.sort();
    let mut text =
        "error: a deadlock: every task is waiting for another, and nothing can wake them\n".to_owned();
    for (_, line) in lines {
        text.push_str(&line);
        text.push('\n');
    }
    text
}

/// Looks for a deadlock, where the counts say there may be one: from a worker going to sleep, or a
/// thread about to block in a stuck wait, holding no lock a note looks at, nor a thread's or a
/// worker's slot. Stops the program if it's proven; otherwise does nothing.
pub(crate) fn look() {
    let before = counts();
    if !before.all_stuck() {
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
    // A job queued is the program still moving: a waiting `parallel` call's, which nothing counts
    // until a worker takes it, while its caller's join is counted as stuck. A worker takes one only
    // once its slot says it's awake, so the slots and this look, first and last, see every job of
    // a call that hasn't become a range yet (phase 2 design, §3).
    #[cfg(any(test, feature = "threads"))]
    if pools.iter().any(|pool| pool.has_jobs()) {
        return None;
    }
    #[cfg_attr(not(any(test, feature = "threads")), expect(unused_mut, reason = "only workers add notes"))]
    let mut notes: Vec<Note> = {
        // Copied out, and asked whether they hold only once no slot is held: a thread takes its
        // slot with what its wait is on held, a channel's queue or a task's end, which asking takes.
        let slots: Vec<ThreadSlot> = THREAD_SLOTS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter_map(std::sync::Weak::upgrade)
            .collect();
        let thread_notes: Vec<ThreadNote> = slots
            .iter()
            .filter_map(|slot| slot.lock().unwrap_or_else(PoisonError::into_inner).clone())
            .collect();
        if !thread_notes.iter().all(ThreadNote::holds) {
            return None;
        }
        thread_notes.into_iter().map(|note| note.note).collect()
    };
    #[cfg(any(test, feature = "threads"))]
    {
        let mut unnoted = Vec::new();
        for pool in &pools {
            for (at, published) in pool.published().iter().enumerate() {
                let published = published.lock().unwrap_or_else(PoisonError::into_inner);
                if !published.idle {
                    return None;
                }
                if published.noted {
                    notes.extend(published.notes.iter().cloned());
                } else {
                    unnoted.push((*pool, at));
                }
            }
        }
        if !notes.iter().all(Note::holds) {
            return None;
        }
        // Workers that went to sleep before a check could pass left their notes out: each is asked
        // for them, and woken to go to sleep again, which leaves them and looks once more.
        if !unnoted.is_empty() {
            for (pool, at) in unnoted {
                if let Some(published) = pool.published().get(at) {
                    published.lock().unwrap_or_else(PoisonError::into_inner).asked = true;
                    pool.wake(at);
                }
            }
            return None;
        }
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
    // The queues again, last.
    #[cfg(any(test, feature = "threads"))]
    if pools.iter().any(|pool| pool.has_jobs()) {
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
    // Two threads can each prove it, as a worker going to sleep and `main` about to wait: the first
    // says it, and the other leaves it to that one, which ends the process.
    static REPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if REPORTED.swap(true, Ordering::SeqCst) {
        return;
    }
    super::write_kept_output();
    let _ = std::io::stdout().flush();
    // A replay's task waiting for its turn, with every context stuck, waits for a turn no one left
    // can come to: the replay can't follow its recording, and says so now, however long the work
    // before a turn took, rather than after a turn's wait ran out (#2276).
    // The least, as several may wait: the same replay says the same line every run.
    if let Some(why) = notes.iter().filter_map(Note::turn_never_comes).min() {
        let said = format!("error: {}{}\n", super::aot::COULDNT_FOLLOW, crate::replay::diverged(why));
        let _ = std::io::stderr().write_all(said.as_bytes());
        super::aot::stop_now(1);
    }
    // A recording logs the deadlock, and a replay holds its own to the recording's: one that isn't
    // the same has diverged, and says that instead.
    let words = said(notes);
    let said = match crate::replay::deadlocked(&replay_lines(notes), &words) {
        // A replay that came to its recording's deadlock says it as the recording did.
        Ok(recorded) => recorded.unwrap_or(words),
        Err(why) => format!("error: {why}\n"),
    };
    let _ = std::io::stderr().write_all(said.as_bytes());
    super::aot::stop_now(1);
}

#[cfg(test)]
mod tests;
