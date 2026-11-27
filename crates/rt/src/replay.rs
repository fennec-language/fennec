//! Recording a run, and replaying it bit for bit. A recorded run logs every outside input a program
//! takes, as it comes through a capability: the clock, entropy, files, the network, standard input,
//! the environment and other programs' results. What each call gave is logged, errors from the
//! operating system included. A replay gives the program the logged results in place of the calls
//! themselves. So it reads no clock, file or socket, and a single-task program does exactly what it
//! did.
//!
//! - **Recording and replaying** is asked for with `FENNEC_RECORD=log` or `FENNEC_REPLAY=log`, or
//!   from the same process with `record_to` and `replay_from`. `fennec run --record` and
//!   `fennec replay` do it for a program, with what it was run with in the log's header.
//! - **Divergence:** each entry names its call. A replay that asks for a different call than the log
//!   has next, or for more calls than it has, or ends with entries left, stops with both named. That
//!   catches any outside input this doesn't record yet.
//! - **A normal run** pays one load to see that nothing's recorded.
//!
//! - **Tasks:** each entry is logged under the task that made the call, and a replay hands each
//!   task its own entries in order, whatever order the tasks run in. A task is named by its path:
//!   its parent's name, a dot, and how many tasks the parent had started before it. `main` is `0`,
//!   its third launch `0.2`, and a `parallel` call's range is named by the call, then where the
//!   range starts, as `0.1.128`. No counter is shared between tasks, so the names don't depend on
//!   how the threads interleave. How many workers the run had is logged first, and a replay has as
//!   many, so its ranges are cut the same way.
//!
//! - **`parallel` calls that fail:** when a range fails, ranges after it that haven't begun don't
//!   begin, and which had begun depends on timing. A recording logs, under the call, each range a
//!   failure kept from starting, and a replay skips exactly those and runs every other.
//!
//! - **Where tasks meet:** a channel is a place, named by the task that made it, as `0.2:c1`. What
//!   each task did there, send, receive, listen or close, is logged in the order it took effect,
//!   with what it came to, and a replay makes each task wait for its turn. So messages go to the
//!   same receivers, and a receive that timed out times out again, without waiting.
//!
//! - **Stops:** a stop cuts short the wait its task is in, and the task fails as stopped. When it
//!   comes depends on time, which a replay doesn't spend: its sleeps are over at once. So a wait a
//!   stop cut short says so in the recording. A capability's call, like a sleep or a socket's read,
//!   has a `cut short` entry right after its own, and a channel's send or receive comes to
//!   "stopped" there. A task its stop finds waiting at its own `with launch` block's end, for a
//!   task it launched, has a `stopped at its block's end` entry, a server's round cancelled there a
//!   `cancelled at its block's end` entry, and one its stop found serving a `stopped serving`
//!   entry. A piece of work on a blocking thread outside a capability's call, as a hash, waits to
//!   its end and then fails its task: a `stopped in its work` entry says which of the task's
//!   pieces of such work it was, as each is counted. A server's round is cancelled, not stopped,
//!   when its server stops, and its waits' entries say they were cut short as a stopped task's do.
//!   A replay stops the task at that wait, as the recorded run did. So a replay stops nothing by
//!   the clock: its block's ends wait for their tasks without a deadline, and stop none when they
//!   fail, as the recording says where each task was stopped.
//!
//! - **Locks:** a `shared` value is a place too, named as a channel is, as `0:s2`. Each time a task
//!   takes its lock, to read or to write, it's logged once the lock is held, and a replay makes
//!   each task wait its turn before it tries to take it. Native code takes a free lock without a
//!   call, so it asks for these only while a run is recorded or replayed.
//!
//! - **Servers:** the call serving is a task, and each connection it accepts is its next child,
//!   named in the order they were taken: `0.3.0`, `0.3.1`. The count is logged under the call. A
//!   replay opens no socket: it runs as many rounds as were recorded, each with its own entries, its
//!   connection's reads and writes among them, and when they've all ended the server stops, as if
//!   it had been stopped there, and says so.
//!
//! - **Deadlocks:** a run whose tasks all come to wait on each other for good is stopped by the
//!   deadlock check (`embed::deadlock`), recorded or not. A recording logs, under `main`, a last
//!   `deadlock` entry with each stuck wait as a line, `task kind place`, as `0.2 receive 0:c1`,
//!   then its end mark. A replay of it lets a task whose recording has nothing more for it at a
//!   channel go on into the real wait, unturned, as it was when the check fired: past there it may
//!   only wait, and a step that takes effect is a divergence. When the check fires, the replay's
//!   lines are held to the recording's, and nothing may be left over. A replay of a recording
//!   that didn't end in a deadlock, and comes to one, has diverged, and says so.
//!
//! The log is `FENREPL9`, the header's length as four bytes and the header, then the entries. Each
//! entry is its task's name and its call's name, each with its length as two bytes, then what it
//! gave, with its length as four. All numbers are little-endian. A `FENREPL1` log, from before
//! tasks were named, has no task names, and replays as `main`'s. A log from an older Fennec, which
//! recorded less of what tasks do, replays as it did then, saying so when it has tasks: a
//! `FENREPL8` log has no deadlock, a `FENREPL7` log has no piece of blocking work a stop cut short, a `FENREPL6` log no `parallel` ranges a stop kept from beginning, a `FENREPL5` log no task
//! stopped while it served, a `FENREPL4` log no `parallel` ranges a
//! failure kept from starting, and a `FENREPL3` or `FENREPL2` log no stops.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io::Write as _;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};

use fennec_syntax::{Diagnostic, Span};

use crate::value::{Instance, Value};

const MAGIC: &[u8; 8] = b"FENREPL9";

/// A log from before deadlocks were recorded: one whose run deadlocked was cut short, without its
/// end mark.
const MAGIC_BEFORE_DEADLOCKS: &[u8; 8] = b"FENREPL8";

/// A log from before a stop that cut a blocking piece of work short was recorded: its replay may go
/// on past the work.
const MAGIC_BEFORE_WORK_STOPS: &[u8; 8] = b"FENREPL7";

/// A log from before a `parallel` call's ranges a stop kept from beginning were recorded: its
/// replay may begin others.
const MAGIC_BEFORE_UNBEGUN: &[u8; 8] = b"FENREPL6";

/// A log from before a task stopped while it served was recorded: its replay may go on past the
/// call serving.
const MAGIC_BEFORE_SERVING_STOPS: &[u8; 8] = b"FENREPL5";

/// A log from before a `parallel` call's skipped ranges were recorded: one where a piece failed
/// may run other pieces in its replay.
const MAGIC_BEFORE_SKIPS: &[u8; 8] = b"FENREPL4";

/// A log from before stops were recorded: a task stopped in it may go on in its replay.
const MAGIC_BEFORE_STOPS: &[u8; 8] = b"FENREPL3";

/// A log from before recordings were marked at their end: one cut short can't be told apart.
const MAGIC_UNMARKED: &[u8; 8] = b"FENREPL2";

/// The entry a recording ends with when the run it records ends, logged under no task. A log
/// without it was cut short: the run was killed, or writing the log failed.
const END: &str = "end";

/// A log from before tasks were named: every entry is `main`'s.
const MAGIC_UNNAMED: &[u8; 8] = b"FENREPL1";

/// The entry a recording ends with, under `main`, when the deadlock check stopped its run: each
/// stuck wait's line, `task kind place`, in order, split by line breaks.
const DEADLOCK: &str = "deadlock";

/// What a channel's turn comes to in a replay of a run that deadlocked, past where its recording
/// has anything for the task there: it goes on into the real wait, unturned, as it was when the
/// check fired.
pub(crate) const PAST_THE_END: i64 = i64::MIN;

/// The entry that says how many workers the recorded run had, logged first, under no task.
const WORKERS: &str = "workers";

/// The entry that says the recorded run had `FENNEC_PARALLEL_STRESS`, logged next, under no task,
/// when it did. It cuts a `parallel` call's ranges to one item each, and ranges are named by where
/// they start, so a replay with it otherwise ran other ranges, with nothing said (#1917).
const STRESSED: &str = "parallel stress";

/// The entry that says a stop cut short the call logged just before it, in the same task: the
/// task failed as stopped in it.
const CUT_SHORT: &str = "cut short";

/// A wait that's no capability's call, where a task's stop can find it and fail it as stopped.
#[cfg_attr(
    not(any(test, feature = "threads")),
    expect(dead_code, reason = "only launched tasks are stopped")
)]
#[derive(Clone, Copy)]
pub(crate) enum StoppedIn {
    /// Its own `with launch` block's end, waiting for a task it launched to end.
    BlockEnd,
    /// The same, for a server's round cancelled there as its server stopped, which leaves its task
    /// behind, unwaited for: so a replay can let that task end quietly past its recording.
    CancelledAtBlockEnd,
    /// A call serving, which a stop ends with its server.
    Serving,
}

impl StoppedIn {
    /// The entry that says the task's stop found it here, and it failed as stopped.
    #[cfg_attr(
        not(any(test, feature = "threads")),
        expect(dead_code, reason = "only launched tasks are stopped")
    )]
    fn entry(self) -> &'static str {
        match self {
            StoppedIn::BlockEnd => "stopped at its block's end",
            StoppedIn::CancelledAtBlockEnd => "cancelled at its block's end",
            StoppedIn::Serving => "stopped serving",
        }
    }
}

enum Mode {
    Off,
    Record(Mutex<std::fs::File>),
    Replay(Mutex<Player>),
}

/// A replay's log, read into a stream of entries for each task.
struct Player {
    streams: HashMap<String, VecDeque<(String, Vec<u8>)>>,
    /// How many workers the recorded run had, when the log says.
    workers: Option<usize>,
    /// Whether the recorded run had `FENNEC_PARALLEL_STRESS`.
    stressed: bool,
    /// Whether the recording logged standard output's bursts, so a replay writes them in that
    /// order: not one made before that was recorded, nor one of a run that printed nothing.
    orders_output: bool,
    /// What tasks have written that isn't due yet, each task's bytes in order, by its name.
    held: HashMap<String, VecDeque<u8>>,
    /// Whether the log ends with the mark its run wrote as it ended, so a replay that runs out of
    /// it is the program's doing, not the log's.
    whole: bool,
    /// Each stuck wait's line, when the recorded run ended in a deadlock.
    deadlock: Option<Vec<String>>,
    /// The report the recorded run said at its deadlock, which a replay that comes to the same one
    /// says: its words name tasks by numbers handed out in whatever order they launched, which the
    /// replay's own may not have (Fable 5.2's review, 2026-11-20).
    deadlock_said: Option<String>,
    /// The first step a replay took past where its recording ended in a deadlock: a divergence,
    /// said when the replay comes to its deadlock or its end.
    past: Option<String>,
}

/// A task, for its entries: its name, and how many tasks it has started, to name the next one.
pub(crate) struct Task {
    name: Arc<str>,
    started: u32,
    /// How many places where tasks meet, like channels, it has made, to name the next one.
    made: u32,
    /// In a replay, whether its recording said a stop or a cancel cut it short, and it's failing as
    /// stopped: so a server's round, which has no stop of its own to keep its end quiet, ends as
    /// quietly as the run's did (#1918).
    stopped: bool,
    /// Whether it's in a capability's call, which a replay gives back without running: a stop in
    /// work it does there is the call's to note, as cut short.
    in_call: bool,
    /// How many pieces of blocking work it's done outside a capability's call, as hashes: which one a
    /// stop cut short is logged by its number.
    works: u64,
}

impl Task {
    fn named(name: Arc<str>) -> Task {
        Task { name, started: 0, made: 0, stopped: false, in_call: false, works: 0 }
    }

    /// Whether, in a replay, its recording said a stop or a cancel cut it short.
    pub(crate) fn was_stopped(&self) -> bool {
        self.stopped
    }

    /// The task that's `main`, and whatever runs on a thread outside any other task.
    fn main() -> Task {
        Task::named(Arc::from("0"))
    }
}

thread_local! {
    /// The task running on this thread, in a recording or a replay. `None` is `main`.
    static TASK: RefCell<Option<Task>> = const { RefCell::new(None) };
}

/// The name of the task running on this thread.
fn running() -> Arc<str> {
    TASK.with_borrow(|task| task.as_ref().map_or_else(|| Arc::from("0"), |task| Arc::clone(&task.name)))
}

/// The next task the running one starts, named after it, in a recording or a replay: `None` in a
/// normal run, which pays one load for it.
pub(crate) fn child() -> Option<Task> {
    if matches!(mode(), Mode::Off) {
        return None;
    }
    TASK.with_borrow_mut(|task| {
        let parent = task.get_or_insert_with(Task::main);
        let name = format!("{}.{}", parent.name, parent.started);
        parent.started += 1;
        Some(Task::named(Arc::from(name)))
    })
}

/// A name for a place where tasks meet that the running task makes, like a channel, in a recording
/// or a replay: the task's name, a colon, `kind`, and how many it had made before, as `0.2:c1`.
/// `None` in a normal run.
pub(crate) fn place(kind: &str) -> Option<Arc<str>> {
    if matches!(mode(), Mode::Off) {
        return None;
    }
    TASK.with_borrow_mut(|task| {
        let maker = task.get_or_insert_with(Task::main);
        let name = format!("{}:{kind}{}", maker.name, maker.made);
        maker.made += 1;
        Some(Arc::from(name))
    })
}

/// Says the running task has done `what` at `place`, coming to `outcome`. It's called where what
/// it did takes effect, with the place held. In a recording, that logs it, so the log has them in
/// the order they happened. In a replay, that ends the task's turn, so the next task's turn only
/// starts once this one's has taken effect: its message is queued, or its lock is held.
pub(crate) fn met(place: Option<&str>, what: &str, outcome: i64) {
    let Some(place) = place else { return };
    match mode() {
        Mode::Off => {}
        Mode::Record(file) => log(file, place, &format!("{} {what}", running()), &outcome.to_le_bytes()),
        Mode::Replay(player) => end_turn(player, place, what),
    }
}

/// Ends the running task's turn at `place`: the front of its stream, or for a read, the reader's
/// own entry, wherever it is (see [`turn_at`]). A step that takes effect where the recording has
/// nothing more, past where its run deadlocked, is a divergence: past there a task may only wait.
fn end_turn(player: &Mutex<Player>, place: &str, what: &str) {
    let mut player = player.lock().unwrap_or_else(PoisonError::into_inner);
    let asked = format!("{} {what}", running());
    let ended = player.streams.get_mut(place).and_then(|stream| {
        let at = turn_at(stream, &asked)?;
        stream.remove(at)
    });
    moved(&mut player, place);
    if ended.is_none() && player.deadlock.is_some() && player.past.is_none() {
        player.past = Some(diverged(format!(
            "this replay's `{} {what}` at {place} went through past where its recording ended in a \
             deadlock, where it only waited, so it isn't the run that was recorded: something it \
             depends on wasn't recorded, or the program has changed",
            running()
        )));
    }
}

/// Where in a place's stream the turn `asked` for is, if it's that task's turn now: at the front,
/// or, for a lock taken to read, anywhere with no write ahead of it. Readers between two writes held
/// the value together, so the order their entries were logged in says nothing: two readers of two
/// values at once can log them crossed, and a replay that gave each its turn in the log's order
/// had each wait for the other (#2313).
fn turn_at(stream: &VecDeque<(String, Vec<u8>)>, asked: &str) -> Option<usize> {
    if !asked.ends_with(" read") {
        return stream.front().is_some_and(|(next, _)| next == asked).then_some(0);
    }
    for (at, (entry, _)) in stream.iter().enumerate() {
        if entry == asked {
            return Some(at);
        }
        if entry.ends_with(" write") {
            return None;
        }
    }
    None
}

/// Told whenever a place's turn moves on, for a thread waiting for its turn that can't park.
static TURN_MOVED: Condvar = Condvar::new();

/// A parked task waiting for its turn at a place: its number among every waiter the run has had, and
/// what wakes it. Not its park's token, which each worker counts on its own: two tasks on two
/// workers can share one, and taking one's waker back took the other's, which then slept for good.
type TurnWaiter = (u64, Box<dyn FnOnce() + Send>);

/// The number the next waiter for a turn has.
#[cfg(any(test, feature = "threads"))]
static NEXT_TURN_WAITER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The parked tasks waiting for their turns at each place: whoever moves the place's turn on wakes
/// them all to look again (`moved`). Taken only with the player held, after it.
static TURN_WAITERS: Mutex<Option<HashMap<String, Vec<TurnWaiter>>>> = Mutex::new(None);

/// How `turn_in` waits while another's turn is next at its place (`wait_for_turn`).
type WaitForTurn<'w> = dyn for<'g> FnMut(MutexGuard<'g, Player>, &str, &str) -> Result<(), String> + 'w;

/// `place`'s turn has moved on, with the player held: each task and thread waiting for its turn
/// there looks again.
fn moved(_held: &mut Player, place: &str) {
    let waiters =
        TURN_WAITERS.lock().unwrap_or_else(PoisonError::into_inner).as_mut().and_then(|by| by.remove(place));
    for (_, wake) in waiters.unwrap_or_default() {
        wake();
    }
    TURN_MOVED.notify_all();
}

/// Waits, with `player` held, until `place`'s turn moves on, for a task whose turn to do `asked`
/// there isn't next: as a stuck wait, so that when every context is stuck the deadlock check finds
/// none of those turns can come, and says the replay couldn't follow its recording. There's no
/// clock: a turn waits however long the work before it takes (#2276). A task parks, woken through
/// `turn_waiters`; a thread that can't park blocks, as a channel's wait does.
fn wait_for_turn(held: &Mutex<Player>, player: MutexGuard<'_, Player>, place: &str, asked: &str) {
    use crate::embed::deadlock::{self, Kind, Note};
    let note =
        Note::parked(Arc::new(TurnWait { place: place.to_owned(), asked: asked.to_owned() }), Kind::Turn);
    #[cfg(any(test, feature = "threads"))]
    if let Some(park) = crate::embed::serve::TurnPark::here() {
        let me = NEXT_TURN_WAITER.fetch_add(1, Ordering::Relaxed);
        let mut waiters = TURN_WAITERS.lock().unwrap_or_else(PoisonError::into_inner);
        waiters.get_or_insert_default().entry(place.to_owned()).or_default().push((me, park.waker()));
        drop(waiters);
        drop(player);
        park.park(note);
        // Woken by a stop, or with others, its waker may still be there.
        let player = held.lock().unwrap_or_else(PoisonError::into_inner);
        let mut waiters = TURN_WAITERS.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(here) = waiters.as_mut().and_then(|by| by.get_mut(place)) {
            here.retain(|(waiter, _)| *waiter != me);
        }
        drop((waiters, player));
        return;
    }
    // A moment first: most turns come by then, and cost the check nothing.
    let (player, waited) =
        TURN_MOVED.wait_timeout(player, deadlock::PATIENCE).unwrap_or_else(PoisonError::into_inner);
    if !waited.timed_out() || !another_is_next(&player, place, asked) {
        return;
    }
    let player = if deadlock::thread_waits(note.blocking(), false) {
        // Every context may be stuck: the check looks, with the player let go of, as it reads it.
        drop(player);
        deadlock::look();
        let player = held.lock().unwrap_or_else(PoisonError::into_inner);
        if !another_is_next(&player, place, asked) {
            deadlock::thread_woke();
            return;
        }
        player
    } else {
        player
    };
    drop(TURN_MOVED.wait(player).unwrap_or_else(PoisonError::into_inner));
    deadlock::thread_woke();
}

/// Whether others' entries than `asked` are ahead of its turn at `place` in `player`: what a turn
/// waits on, as `TurnWait::holds` asks. Anything else goes back to `turn_in` to say: the running
/// task's turn ([`turn_at`]), or a place with nothing more. A thread that asked only whether its
/// own entry was next slept for good when the place ran out instead (Fable 5.2's review of #2276).
fn another_is_next(player: &Player, place: &str, asked: &str) -> bool {
    player.streams.get(place).is_some_and(|stream| !stream.is_empty() && turn_at(stream, asked).is_none())
}

/// A replay's wait for its turn to do `asked` at `place`, for the deadlock check.
struct TurnWait {
    place: String,
    asked: String,
}

impl crate::embed::deadlock::Awaited for TurnWait {
    /// Whether its turn still isn't next, with something else next there.
    fn holds(&self, _: crate::embed::deadlock::Kind) -> bool {
        let Mode::Replay(player) = mode() else { return false };
        let player = player.lock().unwrap_or_else(PoisonError::into_inner);
        another_is_next(&player, &self.place, &self.asked)
    }

    /// What the replay says, when its turn never comes.
    fn said(&self, _: crate::embed::deadlock::Kind) -> String {
        never_comes(&self.asked, &self.place, &mode_front(&self.place).unwrap_or_default())
    }
}

/// What a replay says when the running task's turn to do `asked` at `place` never comes: `front` is
/// next there, and nothing left running can come to it.
fn never_comes(asked: &str, place: &str, front: &str) -> String {
    format!(
        "this replay asked for `{asked}` at {place}, where its recording has `{front}` next, and nothing \
         left running can come to it, so it isn't the run that was recorded: something it depends on \
         wasn't recorded, or the program has changed"
    )
}

/// The entry next at `place` in a replay, if there is one.
fn mode_front(place: &str) -> Option<String> {
    let Mode::Replay(player) = mode() else { return None };
    let player = player.lock().unwrap_or_else(PoisonError::into_inner);
    player.streams.get(place).and_then(VecDeque::front).map(|(next, _)| next.clone())
}

/// In a replay, waits until it's the running task's turn to do `what` at `place`, and gives back
/// what it came to in the recording. The turn stays this task's until `met` ends it. `Ok(None)`
/// when this isn't a replay. A replay that asks for something the recording didn't do there has
/// diverged, and says so.
pub(crate) fn turn(place: Option<&str>, what: &str) -> Result<Option<i64>, String> {
    let (Some(place), Mode::Replay(player)) = (place, mode()) else { return Ok(None) };
    followed(player, place, what, false)
}

/// [`turn_in`] for [`turn`] and [`turn_to_wait`] (`waits`), noting a divergence (`diverged`).
fn followed(player: &Mutex<Player>, place: &str, what: &str, waits: bool) -> Result<Option<i64>, String> {
    // Past where it couldn't follow, no turn is anyone's: each task waiting for one would wait out
    // its patience, crash, and leave cleanups that don't follow either.
    if let Some(first) = divergence() {
        return Err(first.to_owned());
    }
    let mut wait = |held: MutexGuard<'_, Player>, place: &str, asked: &str| {
        wait_for_turn(player, held, place, asked);
        Ok(())
    };
    turn_in(player, (place, what, waits), &mut wait).map(Some).map_err(|why| {
        // Another task's turn was next there: the replay can't follow its recording. A place with
        // nothing more for it, as a round cancelled at its end asks for, is said as it was.
        let others = player
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .streams
            .get(place)
            .is_some_and(|stream| !stream.is_empty());
        if others { diverged(why) } else { why }
    })
}

/// Where a replay first couldn't follow its recording at a place where tasks meet, once it's found
/// it: a task waited out its patience for its turn while the recording had another's next. What
/// goes wrong after, like a task's cleanup asking for other calls than its recording's, comes of
/// it, and isn't what the replay says.
static DIVERGED: OnceLock<String> = OnceLock::new();

/// Notes `why` as where the replay couldn't follow its recording, if it's the first, and gives it
/// back.
pub(crate) fn diverged(why: String) -> String {
    let _ = DIVERGED.set(why.clone());
    why
}

/// Where the replay first couldn't follow its recording, if it's found one.
pub fn divergence() -> Option<&'static str> {
    DIVERGED.get().map(String::as_str)
}

/// `turn`, for a wait, a channel's send or receive: in a replay of a run that deadlocked, one the
/// recording has nothing more for at `place` comes to [`PAST_THE_END`], and goes on into the real
/// wait.
pub(crate) fn turn_to_wait(place: Option<&str>, what: &str) -> Result<Option<i64>, String> {
    let (Some(place), Mode::Replay(player)) = (place, mode()) else { return Ok(None) };
    followed(player, place, what, true)
}

/// Waits for the running task's turn to do `what` at `place` in `player`'s log, and gives back what
/// it came to. While another's turn is next there, it waits with `wait` (`wait_for_turn`), which
/// gives back once the place's turn has moved on, however long that takes: never with a failure,
/// as a turn that can't come ends the program from the deadlock check. A wait (`waits`) the
/// recording has nothing more for, where it ended in a deadlock, comes to [`PAST_THE_END`].
fn turn_in(
    player: &Mutex<Player>,
    (place, what, waits): (&str, &str, bool),
    wait: &mut WaitForTurn<'_>,
) -> Result<i64, String> {
    let asked = format!("{} {what}", running());
    loop {
        let held = player.lock().unwrap_or_else(PoisonError::into_inner);
        let stream = held.streams.get(place);
        if let Some((_, payload)) = stream.and_then(|stream| stream.get(turn_at(stream, &asked)?)) {
            let bytes: [u8; 8] =
                payload.clone().try_into().map_err(|_| format!("this replay's {place} is damaged"))?;
            return Ok(i64::from_le_bytes(bytes));
        }
        match stream.and_then(VecDeque::front) {
            None if waits && held.whole && held.deadlock.is_some() => return Ok(PAST_THE_END),
            None if !held.whole => return Err(cut_short(what, place)),
            None => {
                return Err(format!(
                    "this replay's task asked for `{asked}` at {place}, where its recording had nothing more, \
                     so it isn't the run that was recorded: something it depends on wasn't recorded, or the \
                     program has changed"
                ));
            }
            Some(_) => wait(held, place, &asked)?,
        }
    }
}

impl Task {
    /// The task's name.
    pub(crate) fn name(&self) -> Arc<str> {
        Arc::clone(&self.name)
    }
}

/// A task named `name`, like a server's round, `0.3.12`.
#[cfg_attr(
    not(any(test, feature = "threads")),
    expect(dead_code, reason = "only servers use it, which need threads")
)]
pub(crate) fn task_named(name: &str) -> Task {
    Task::named(Arc::from(name))
}

/// In a recording, logs under `place` that `what` happened, the `n`th: as a server's accepts are,
/// under the call serving.
#[cfg_attr(
    not(any(test, feature = "threads")),
    expect(dead_code, reason = "only servers use it, which need threads")
)]
pub(crate) fn log_at(place: &str, what: &str, n: u64) {
    if let Mode::Record(file) = mode() {
        log(file, place, what, &n.to_le_bytes());
    }
}

/// In a replay, how many times `what` was logged under `place`, taking those entries: how many
/// connections a server accepted. `None` when this isn't a replay.
#[cfg_attr(
    not(any(test, feature = "threads")),
    expect(dead_code, reason = "only servers use it, which need threads")
)]
pub(crate) fn taken_at(place: &str, what: &str) -> Option<u64> {
    let Mode::Replay(player) = mode() else { return None };
    let mut player = player.lock().unwrap_or_else(PoisonError::into_inner);
    let stream = player.streams.entry(place.to_owned()).or_default();
    let before = stream.len();
    stream.retain(|(call, _)| call != what);
    Some(u64::try_from(before - stream.len()).unwrap_or(0))
}

/// The place standard output is: each burst a task writes is logged there, in the order the bursts
/// went out.
const OUTPUT: &str = "out";

/// Keeps a recorded burst and its entry together, so the log has the bursts in the order they
/// went out.
static WRITING: Mutex<()> = Mutex::new(());

/// Writes a burst of standard output with `write`. In a recording, it's logged under the task that
/// wrote it, with how long it was, so the log has how tasks' output interleaved. How tasks' output
/// interleaves is an outside input, as the clock is, and a replay repeats it: what a task writes
/// is held until every burst recorded before it has gone out, and whichever task writes next
/// writes all that are then due. No task waits for another to write, so one whose turn is behind
/// work queued on its own thread doesn't hold that work up.
///
/// **A replay goes by each task's bytes, not by how many times it wrote.** Where a task's output
/// is cut into bursts depends on when it happened to wait: a thread writes what it has printed
/// before it sleeps on a channel, and in a replay it waits for its turn there instead, without
/// sleeping. So the same output can come in fewer bursts than were recorded, or in more. The
/// recorded lengths say where each burst ended, and a replay cuts what a task writes at those.
pub(crate) fn writing(bytes: &[u8], write: impl Fn(&[u8]) -> std::io::Result<()>) -> std::io::Result<()> {
    match mode() {
        Mode::Off => write(bytes),
        Mode::Record(file) => {
            let _order = WRITING.lock().unwrap_or_else(PoisonError::into_inner);
            write(bytes)?;
            let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            log(file, OUTPUT, &format!("{} write", running()), &length.to_le_bytes());
            Ok(())
        }
        Mode::Replay(player) => {
            let mut player = player.lock().unwrap_or_else(PoisonError::into_inner);
            if !player.orders_output {
                return write(bytes);
            }
            player.takes_output(&running(), bytes, write)
        }
    }
}

/// What the running task, `main`, kept printing to the end, which the program writes once it's
/// over, with `write`, and gives back what's left for it to write after. In a replay, it goes
/// through the bursts its recording logged that are still due first, as `writing` sends a burst.
///
/// **A replay waits less than its recording did,** so it can keep to its end bytes its recording
/// wrote at a wait: a receive its recording says timed out gives up at once, without the wait that
/// wrote what the task had printed first. Those bytes came out at the end, past the burst ordering,
/// so the recording's burst was left over and the replay said it ended where its recording asked
/// for a write (the nightly's random programs). Now the due bursts take them, in their order, and
/// what's left is what the recording's end wrote too, which no burst holds.
pub(crate) fn kept_at_end(
    bytes: Vec<u8>,
    write: impl Fn(&[u8]) -> std::io::Result<()>,
) -> std::io::Result<Vec<u8>> {
    let Mode::Replay(player) = mode() else { return Ok(bytes) };
    let mut player = player.lock().unwrap_or_else(PoisonError::into_inner);
    if !player.orders_output {
        return Ok(bytes);
    }
    let task = running();
    player.takes_output(&task, &bytes, write)?;
    Ok(player.held.remove(&*task).map(Vec::from).unwrap_or_default())
}

impl Player {
    /// Takes what `task` has written, and writes with `write` every recorded burst that's now
    /// due, in the order they were recorded: each as long as it was then, from its task's bytes,
    /// once that task has written that many.
    fn takes_output(
        &mut self,
        task: &str,
        bytes: &[u8],
        write: impl Fn(&[u8]) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        self.held.entry(task.to_owned()).or_default().extend(bytes);
        loop {
            let next = self.streams.get(OUTPUT).and_then(VecDeque::front).and_then(|(call, payload)| {
                let task = call.strip_suffix(" write")?;
                let length = <[u8; 8]>::try_from(payload.as_slice()).ok().map(u64::from_le_bytes)?;
                Some((task.to_owned(), usize::try_from(length).ok()?))
            });
            let Some((next, length)) = next else { break };
            let held = self.held.entry(next).or_default();
            if held.len() < length {
                break;
            }
            let burst: Vec<u8> = held.drain(..length).collect();
            write(&burst)?;
            if let Some(stream) = self.streams.get_mut(OUTPUT) {
                stream.pop_front();
            }
        }
        Ok(())
    }
}

/// In a recording, logs a list of numbers under `task`, as `what`: which of a stream's chunks ran.
pub(crate) fn log_numbers(task: &str, what: &str, numbers: &[u64]) {
    if let Mode::Record(file) = mode() {
        let bytes: Vec<u8> = numbers.iter().flat_map(|number| number.to_le_bytes()).collect();
        log(file, task, what, &bytes);
    }
}

/// In a replay, the list of numbers logged under `task` as `what`, taken: `None` when this isn't a
/// replay, or the recording logged none.
pub(crate) fn logged_numbers(task: &str, what: &str) -> Option<Vec<u64>> {
    let Mode::Replay(player) = mode() else { return None };
    let mut player = player.lock().unwrap_or_else(PoisonError::into_inner);
    let stream = player.streams.get_mut(task)?;
    let at = stream.iter().position(|(call, _)| call == what)?;
    let (_, payload) = stream.remove(at)?;
    Some(payload.as_chunks::<8>().0.iter().map(|word| u64::from_le_bytes(*word)).collect())
}

/// A `parallel` call's range, named by its call and where it starts.
pub(crate) fn range(call: Option<&Task>, from: usize) -> Option<Task> {
    call.map(|call| Task::named(Arc::from(format!("{}.{from}", call.name))))
}

/// Swaps the task running on this thread with `task`: once to run it, and again to put back what
/// was running. Nothing happens in a normal run.
pub(crate) fn swap(task: &mut Option<Task>) {
    if matches!(mode(), Mode::Off) {
        return;
    }
    TASK.with_borrow_mut(|running| std::mem::swap(running, task));
}

/// In a replay, at a wait its recording says a stop cut short: the running task is stopped here, as
/// it was then, and fails as stopped. A replay doesn't wait, so the stop that cut the wait short
/// hasn't come yet, and may never: without this the task would go on past the wait, doing what the
/// recorded run never did. Its stop is set as well as its failure, so it ends quietly, as a stopped
/// task does, and its `with launch` block ends as it did.
pub(crate) fn stopped_as_recorded() {
    // What it printed goes out first, under its own name, as it did in the run when it parked in
    // the wait the stop cut short: a replay doesn't park there, and a round's output written later,
    // by its worker, didn't line up with its recording (#1918).
    crate::embed::write_kept_output();
    #[cfg(any(test, feature = "threads"))]
    {
        if let Some(stop) = crate::embed::threads::task_stop() {
            stop.fetch_max(crate::embed::threads::STOPPING, Ordering::AcqRel);
        }
        mark_stopped();
        crate::embed::serve::fail_as_stopped();
    }
    // Without threads there are no tasks to stop, so no recording a runtime like this one made says
    // a wait was cut short. One made with them that does still fails here, as it did.
    #[cfg(not(any(test, feature = "threads")))]
    crate::embed::fail_with(Diagnostic::new(crate::embed::STOPPED_FAILURE, Span::default()));
}

/// In a replay, marks the running task as one its recording said a stop or a cancel cut short (see
/// `Task::stopped`): for the code that made a `parallel` call or read a stream, once a piece of it
/// was, as that piece ran under a name of its own and the code's end looks at the code's (#1949).
pub(crate) fn mark_stopped() {
    TASK.with_borrow_mut(|task| {
        if let Some(task) = task.as_mut() {
            task.stopped = true;
        }
    });
}

/// Whether this run is a replay, whose outside inputs all come from its recording.
pub(crate) fn replaying() -> bool {
    matches!(mode(), Mode::Replay(_))
}

/// Whether this run is being recorded or replayed.
pub(crate) fn active() -> bool {
    !matches!(mode(), Mode::Off)
}

/// A stuck wait's line in a deadlock, `task kind place`, as `0.2 receive 0:c1`: the running task,
/// doing `kind` at `place`, a place's or a task's name in this recording or replay.
pub(crate) fn line(kind: &str, place: &str) -> Arc<str> {
    Arc::from(format!("{} {kind} {place}", running()))
}

/// The deadlock check has proven a deadlock whose stuck waits are `lines`, in order, and that it
/// says as `said`. A recording logs both under `main`, whichever thread found it. A replay holds
/// its lines to its recording's: the same, with nothing of the recording left over or held back,
/// is the run that was recorded, and gives the recording's report to say; anything else is a
/// divergence, said in the `Err`. A normal run has nothing to do.
#[cfg_attr(
    test,
    expect(dead_code, reason = "only a deadlock's report calls it, which this crate's tests never make")
)]
pub(crate) fn deadlocked(lines: &[String], said: &str) -> Result<Option<String>, String> {
    match mode() {
        Mode::Off => Ok(None),
        Mode::Record(file) => {
            log(file, "0", DEADLOCK, format!("{}\n\n{said}", lines.join("\n")).as_bytes());
            Ok(None)
        }
        Mode::Replay(player) => {
            deadlocked_in(&player.lock().unwrap_or_else(PoisonError::into_inner), lines).map_err(diverged)
        }
    }
}

/// [`deadlocked`], in a replay of `player`.
fn deadlocked_in(player: &Player, lines: &[String]) -> Result<Option<String>, String> {
    let quoted =
        |lines: Vec<&String>| lines.iter().map(|line| format!("`{line}`")).collect::<Vec<_>>().join(", ");
    let not_recorded = "so it isn't the run that was recorded: something it depends on wasn't recorded, or the \
                        program has changed";
    if let Some(past) = &player.past {
        return Err(past.clone());
    }
    if let Some(task) = printed_more(player) {
        return Err(format!(
            "this replay printed more than its recording did{}, {not_recorded}",
            in_task(task)
        ));
    }
    let Some(recorded) = &player.deadlock else {
        return Err(format!(
            "this replay came to a deadlock its recording didn't have, its tasks waiting at {}, {not_recorded}",
            quoted(lines.iter().collect())
        ));
    };
    if recorded.as_slice() != lines {
        // Only the lines one side has: the rest, which may be many, are the same.
        let in_replay_alone: Vec<&String> = lines.iter().filter(|line| !recorded.contains(line)).collect();
        let in_recording_alone: Vec<&String> = recorded.iter().filter(|line| !lines.contains(line)).collect();
        return Err(format!(
            "this replay came to a deadlock other than its recording's: waiting at {} where the recording \
             wasn't, and not at {} where it was, {not_recorded}",
            if in_replay_alone.is_empty() { "nothing more".to_owned() } else { quoted(in_replay_alone) },
            if in_recording_alone.is_empty() {
                "nothing less".to_owned()
            } else {
                quoted(in_recording_alone)
            },
        ));
    }
    if let Some((task, next)) = left_over(player) {
        return Err(format!(
            "this replay came to its recording's deadlock where its recording asked for `{next}` next{}, \
             {not_recorded}",
            in_task(&task)
        ));
    }
    Ok(player.deadlock_said.clone())
}

/// The first task, by name, that has printed more than its recording did, which a replay holds
/// back and never writes.
fn printed_more(player: &Player) -> Option<&String> {
    let mut held: Vec<&String> =
        player.held.iter().filter(|(_, bytes)| !bytes.is_empty()).map(|(task, _)| task).collect();
    held.sort();
    held.first().copied()
}

/// How many workers the recorded run had, for a replay to have as many.
#[cfg_attr(
    not(any(test, feature = "threads")),
    expect(dead_code, reason = "without threads there's no pool to size")
)]
pub(crate) fn workers() -> Option<usize> {
    match mode() {
        Mode::Replay(player) => player.lock().unwrap_or_else(PoisonError::into_inner).workers,
        _ => None,
    }
}

/// In a replay, whether the recorded run had `FENNEC_PARALLEL_STRESS`, which the replay then has
/// too, whatever it's run with.
#[cfg_attr(
    not(any(test, feature = "threads")),
    expect(dead_code, reason = "without threads there's no pool to stress")
)]
pub(crate) fn stressed() -> Option<bool> {
    match mode() {
        Mode::Replay(player) => Some(player.lock().unwrap_or_else(PoisonError::into_inner).stressed),
        _ => None,
    }
}

static MODE: OnceLock<Mode> = OnceLock::new();

fn mode() -> &'static Mode {
    MODE.get_or_init(|| {
        if let Some(path) = std::env::var_os("FENNEC_REPLAY") {
            return opened("replay", Path::new(&path), playing(Path::new(&path)));
        }
        match std::env::var_os("FENNEC_RECORD") {
            Some(path) => opened("record to", Path::new(&path), recording(Path::new(&path))),
            None => Mode::Off,
        }
    })
}

/// The mode a log was opened for. A run that was asked to record or replay and can't says so and
/// stops: going on live would look as if it had worked.
fn opened(doing: &str, path: &Path, mode: std::io::Result<Mode>) -> Mode {
    mode.unwrap_or_else(|error| {
        let _ = writeln!(std::io::stderr(), "error: couldn't {doing} {}: {error}", path.display());
        crate::embed::aot::stop_now(1)
    })
}

fn recording(path: &Path) -> std::io::Result<Mode> {
    let mut file = std::fs::OpenOptions::new().append(true).open(path)?;
    let workers = u64::try_from(crate::embed::workers_wanted()).unwrap_or(1);
    file.write_all(&entry("", WORKERS, &workers.to_le_bytes()))?;
    if crate::embed::stress_asked() {
        file.write_all(&entry("", STRESSED, &[]))?;
    }
    Ok(Mode::Record(Mutex::new(file)))
}

fn playing(path: &Path) -> std::io::Result<Mode> {
    let log = std::fs::read(path)?;
    entries_start(&log).ok_or_else(|| std::io::Error::other("this isn't a recording"))?;
    let player = player_of(&log).ok_or_else(|| std::io::Error::other("this recording is damaged"))?;
    if made_by_an_older_fennec(&log, &player) {
        let _ = writeln!(
            std::io::stderr(),
            "warning: this recording was made by an older Fennec, which recorded less of what tasks \
             do, like where one was stopped, so a task in it may not replay as it ran: record it again \
             to replay it exactly"
        );
    }
    Ok(Mode::Replay(Mutex::new(player)))
}

/// Whether a replay of `log` should say an older Fennec made it: any log older than this one whose
/// run had tasks, as one may have been stopped, or a `parallel` call's piece may have failed.
fn made_by_an_older_fennec(log: &[u8], player: &Player) -> bool {
    !log.starts_with(MAGIC) && player.has_tasks()
}

impl Player {
    /// Whether the recording has entries of any task but `main`: of a launched task, a server's
    /// round or a `parallel` call's range, in a stream of its own, or named where tasks meet, as a
    /// task that only used a channel is, or in what was written.
    fn has_tasks(&self) -> bool {
        self.streams.iter().any(|(name, stream)| {
            if name.contains(':') || name == OUTPUT {
                stream.iter().any(|(call, _)| call.split(' ').next().is_some_and(|task| task != "0"))
            } else {
                name != "0"
            }
        })
    }
}

/// A log's entries, each task's in a stream of its own: `None` if it's damaged.
fn player_of(log: &[u8]) -> Option<Player> {
    let named = [
        MAGIC,
        MAGIC_BEFORE_DEADLOCKS,
        MAGIC_BEFORE_WORK_STOPS,
        MAGIC_BEFORE_UNBEGUN,
        MAGIC_BEFORE_SERVING_STOPS,
        MAGIC_BEFORE_SKIPS,
        MAGIC_BEFORE_STOPS,
        MAGIC_UNMARKED,
    ]
    .iter()
    .any(|magic| log.starts_with(*magic));
    let mut player = Player {
        streams: HashMap::new(),
        workers: None,
        stressed: false,
        orders_output: false,
        held: HashMap::new(),
        deadlock: None,
        deadlock_said: None,
        past: None,
        whole: ![
            MAGIC,
            MAGIC_BEFORE_DEADLOCKS,
            MAGIC_BEFORE_WORK_STOPS,
            MAGIC_BEFORE_UNBEGUN,
            MAGIC_BEFORE_SERVING_STOPS,
            MAGIC_BEFORE_SKIPS,
            MAGIC_BEFORE_STOPS,
        ]
        .iter()
        .any(|magic| log.starts_with(*magic)),
    };
    let mut reader = Entries { log, at: entries_start(log)? };
    while reader.at < log.len() {
        let task = if named { reader.name()? } else { "0".to_owned() };
        let call = reader.name()?;
        let payload = reader.payload()?;
        if task.is_empty() && call == END {
            player.whole = true;
            continue;
        }
        if task.is_empty() && call == STRESSED {
            player.stressed = true;
            continue;
        }
        // Its lines, then a blank line, then its report. Only a log new enough to have one: in an
        // older one, it's an entry of `main`'s like any.
        if task == "0" && call == DEADLOCK && log.starts_with(MAGIC) {
            let entry = String::from_utf8_lossy(&payload).into_owned();
            let (lines, said) =
                entry.split_once("\n\n").map_or((entry.as_str(), None), |(lines, said)| (lines, Some(said)));
            player.deadlock = Some(lines.lines().map(str::to_owned).collect());
            player.deadlock_said = said.map(str::to_owned);
            continue;
        }
        if task.is_empty() && call == WORKERS {
            let bytes: [u8; 8] = payload.as_slice().try_into().ok()?;
            player.workers = usize::try_from(u64::from_le_bytes(bytes)).ok();
            continue;
        }
        player.streams.entry(task).or_default().push_back((call, payload));
    }
    player.orders_output = player.streams.contains_key(OUTPUT);
    Some(player)
}

/// Reads a log's entries in turn.
struct Entries<'l> {
    log: &'l [u8],
    at: usize,
}

impl Entries<'_> {
    fn bytes(&mut self, count: usize) -> Option<&[u8]> {
        let got = self.log.get(self.at..self.at.checked_add(count)?)?;
        self.at += count;
        Some(got)
    }

    /// A task's or a call's name, after its length as two bytes.
    fn name(&mut self) -> Option<String> {
        let length = usize::from(u16::from_le_bytes(self.bytes(2)?.try_into().ok()?));
        Some(String::from_utf8_lossy(self.bytes(length)?).into_owned())
    }

    /// What a call gave, after its length as four bytes.
    fn payload(&mut self) -> Option<Vec<u8>> {
        let length = usize::try_from(u32::from_le_bytes(self.bytes(4)?.try_into().ok()?)).ok()?;
        Some(self.bytes(length)?.to_vec())
    }
}

/// Starts a log at `path` with `header`, for a run recorded into it.
pub fn start_log(path: &Path, header: &[u8]) -> std::io::Result<()> {
    let mut log = MAGIC.to_vec();
    log.extend(u32::try_from(header.len()).unwrap_or(u32::MAX).to_le_bytes());
    log.extend(header);
    std::fs::write(path, log)
}

/// The header of the log at `path`.
pub fn header_of(path: &Path) -> std::io::Result<Vec<u8>> {
    let log = std::fs::read(path)?;
    let start = entries_start(&log).ok_or_else(|| std::io::Error::other("this isn't a recording"))?;
    Ok(log.get(MAGIC.len() + 4..start).unwrap_or_default().to_vec())
}

fn entries_start(log: &[u8]) -> Option<usize> {
    let magic = log.get(..MAGIC.len())?;
    if ![
        MAGIC,
        MAGIC_BEFORE_DEADLOCKS,
        MAGIC_BEFORE_WORK_STOPS,
        MAGIC_BEFORE_UNBEGUN,
        MAGIC_BEFORE_SERVING_STOPS,
        MAGIC_BEFORE_SKIPS,
        MAGIC_BEFORE_STOPS,
        MAGIC_UNMARKED,
        MAGIC_UNNAMED,
    ]
    .iter()
    .any(|known| known.as_slice() == magic)
    {
        return None;
    }
    let length = u32::from_le_bytes(log.get(MAGIC.len()..MAGIC.len() + 4)?.try_into().ok()?);
    let start = MAGIC.len() + 4 + usize::try_from(length).ok()?;
    (start <= log.len()).then_some(start)
}

/// Records this process's run into the log at `path`, which `start_log` began, if nothing's been
/// recorded or replayed yet.
pub fn record_to(path: &Path) -> std::io::Result<()> {
    let _ = MODE.set(recording(path)?);
    Ok(())
}

/// Replays the run logged at `path` in this process, if nothing's been recorded or replayed yet.
pub fn replay_from(path: &Path) -> std::io::Result<()> {
    let _ = MODE.set(playing(path)?);
    Ok(())
}

/// What a replay that's run out of its log, or ended before it, says: `None` when it hasn't.
pub fn unfinished() -> Option<String> {
    let Mode::Replay(player) = mode() else { return None };
    let player = player.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(past) = &player.past {
        return Some(past.clone());
    }
    if player.deadlock.is_some() {
        return Some(
            "this replay ended, where its recording ended in a deadlock, so it isn't the run that was \
             recorded: something it depends on wasn't recorded, or the program has changed"
                .to_owned(),
        );
    }
    if let Some(task) = printed_more(&player) {
        return Some(format!(
            "this replay printed more than its recording did{}, so it isn't the run that was recorded: \
             something it depends on wasn't recorded, or the program has changed",
            in_task(task)
        ));
    }
    let (task, next) = left_over(&player)?;
    Some(format!(
        "this replay ended where its recording asked for `{next}` next{}, so it isn't the run that was \
         recorded: something it depends on wasn't recorded, or the program has changed",
        in_task(&task)
    ))
}

/// The first task, by name, with entries a replay didn't ask for, and the next of them: by name,
/// so what a replay says is the same every time.
fn left_over(player: &Player) -> Option<(String, String)> {
    let mut left: Vec<_> = player.streams.iter().filter(|(_, stream)| !stream.is_empty()).collect();
    left.sort_by_key(|(a, _)| *a);
    let (task, stream) = left.first()?;
    Some(((*task).clone(), stream.front()?.0.clone()))
}

/// Where a call was asked for, in what a replay says: nothing for `main`'s, and a place where
/// tasks meet by its name.
fn in_task(task: &str) -> String {
    if task == "0" {
        String::new()
    } else if task.contains(':') {
        format!(" at {task}")
    } else {
        format!(" in task {task}")
    }
}

/// The result of a capability's call named `call`: in a normal run what `live` gives, in a
/// recording what it gives, logged, and in a replay the logged one, without `live` running.
///
/// The call's name is written out only in a recording or a replay, so a normal run pays nothing for
/// it.
pub(crate) fn value(
    call: std::fmt::Arguments<'_>,
    span: Span,
    live: impl FnOnce() -> Result<Value, Diagnostic>,
) -> Result<Value, Diagnostic> {
    match mode() {
        Mode::Off => live(),
        Mode::Record(file) => {
            let failing = crate::embed::failed();
            let result = in_call(live);
            let mut payload = Vec::new();
            encode_result(&result, &mut payload);
            log_call(file, &running(), &call.to_string(), &payload, failing);
            result
        }
        Mode::Replay(player) => {
            let call = call.to_string();
            let payload = replayed(player, &call).map_err(|why| Diagnostic::new(why, span))?;
            let result = decode_result(&payload, span).ok_or_else(|| {
                Diagnostic::new(format!("this replay's recording of `{call}` couldn't be read back"), span)
            })?;
            stopped_if_cut_short(player);
            result
        }
    }
}

/// A call that writes, like standard error's: in a recording, logged as `value` logs one. A
/// replay writes too, as the run did, then gives back what the call gave in the recording, so the
/// program goes on as it did even where the write turns out differently.
pub(crate) fn written(
    call: std::fmt::Arguments<'_>,
    span: Span,
    live: impl FnOnce() -> Result<Value, Diagnostic>,
) -> Result<Value, Diagnostic> {
    let Mode::Replay(_) = mode() else { return value(call, span, live) };
    // In the call, as it was in the recording, where `value` ran it: its work isn't counted.
    let _ = in_call(live);
    value(call, span, || unreachable!("a replay gives back what was recorded"))
}

/// A word an outside call gives, like the clock's reading, recorded and replayed as `value` is.
pub(crate) fn word(call: &str, live: impl FnOnce() -> i64) -> Result<i64, String> {
    match mode() {
        Mode::Off => Ok(live()),
        Mode::Record(file) => {
            let failing = crate::embed::failed();
            let word = in_call(live);
            log_call(file, &running(), call, &word.to_le_bytes(), failing);
            Ok(word)
        }
        Mode::Replay(player) => {
            let payload = replayed(player, call)?;
            let bytes: [u8; 8] =
                payload.try_into().map_err(|_| format!("this replay's recording of `{call}` is damaged"))?;
            stopped_if_cut_short(player);
            Ok(i64::from_le_bytes(bytes))
        }
    }
}

/// Runs a capability's call live, in a recording, with the running task marked as in it.
fn in_call<T>(live: impl FnOnce() -> T) -> T {
    let mark = |inside: bool| {
        TASK.with_borrow_mut(|task| {
            std::mem::replace(&mut task.get_or_insert_with(Task::main).in_call, inside)
        })
    };
    let outer = mark(true);
    let given = live();
    mark(outer);
    given
}

/// In a recording, logs a call `task` made, which it made `failing` or not, giving `payload`, and
/// after it, if a stop cut the call short, that it did, for a replay to stop the task there. Both go
/// in one write, so a run that ends meanwhile keeps both or neither. A task left behind by a round
/// cancelled at its block's end is stopped as the program ends: its sleep was kept without the mark
/// once, and a replay went on past it, printing what the run never did.
fn log_call(file: &Mutex<std::fs::File>, task: &str, call: &str, payload: &[u8], failing: bool) {
    if cut_short_by_a_stop(failing) {
        log_entries(file, task, &[(call, payload), (CUT_SHORT, &[])]);
    } else {
        log_entries(file, task, &[(call, payload)]);
    }
}

/// Whether the call just made, which was made `failing` or not, was cut short by a stop, so the
/// task's failing now as a stopped one.
fn cut_short_by_a_stop(failing: bool) -> bool {
    // A server's round is cancelled when its server stops, rather than stopped, and fails as
    // stopped at its wait all the same.
    #[cfg(any(test, feature = "threads"))]
    {
        !failing
            && crate::embed::failed()
            && (crate::embed::threads::stopped() || fennec_parked::is_cancelled())
    }
    #[cfg(not(any(test, feature = "threads")))]
    {
        let _ = failing;
        false
    }
}

/// In a replay, after a call the running task made: if a stop cut it short in the recording, the
/// task is stopped there now.
fn stopped_if_cut_short(player: &Mutex<Player>) {
    let task = running();
    let cut = {
        let mut player = player.lock().unwrap_or_else(PoisonError::into_inner);
        let stream = player.streams.get_mut(&*task);
        let cut =
            stream.as_ref().and_then(|stream| stream.front()).is_some_and(|(next, _)| next == CUT_SHORT);
        if cut {
            stream.and_then(VecDeque::pop_front);
        }
        cut
    };
    if cut {
        stopped_as_recorded();
    }
}

/// The running task's stop found it waiting `in` a wait that's no capability's call, and it's
/// failing as stopped: in a recording, logged, for a replay to stop it there too.
#[cfg_attr(
    not(any(test, feature = "threads")),
    expect(dead_code, reason = "only launched tasks are stopped")
)]
pub(crate) fn stopped_in(waiting: StoppedIn) {
    if let Mode::Record(file) = mode() {
        log(file, &running(), waiting.entry(), &[]);
    }
}

/// The entry that says a stop cut short a task's piece of blocking work, outside a capability's
/// call: its number among the task's pieces of such work, as eight bytes.
const WORK_STOPPED: &str = "stopped in its work";

/// The running task's piece of blocking work outside a capability's call, as a hash, has ended.
/// In a recording, a stop that cut it short (`cut`) is logged with the piece's number, and this
/// gives back `cut`. In a replay, the task is stopped at the piece the recording names, and this
/// gives back whether it was. Each piece is counted, in both, so a task that does two with no
/// call between is stopped at the right one: matched as the next entry, the mark stopped a replay
/// at the first. Work in a capability's call is the call's to note, as cut short.
#[cfg_attr(
    not(any(test, feature = "threads")),
    expect(dead_code, reason = "only launched tasks are stopped")
)]
pub(crate) fn work_ended(cut: bool) -> bool {
    if matches!(mode(), Mode::Off) {
        return cut;
    }
    let number = TASK.with_borrow_mut(|task| {
        let task = task.get_or_insert_with(Task::main);
        if task.in_call {
            return None;
        }
        task.works += 1;
        Some(task.works - 1)
    });
    let Some(number) = number else { return cut };
    match mode() {
        Mode::Off => cut,
        Mode::Record(file) => {
            if cut {
                log(file, &running(), WORK_STOPPED, &number.to_le_bytes());
            }
            cut
        }
        Mode::Replay(player) => {
            let task = running();
            let stopped = {
                let mut player = player.lock().unwrap_or_else(PoisonError::into_inner);
                let stream = player.streams.get_mut(&*task);
                let here =
                    stream.as_ref().and_then(|stream| stream.front()).is_some_and(|(next, payload)| {
                        next == WORK_STOPPED && payload.as_slice() == number.to_le_bytes()
                    });
                if here {
                    stream.and_then(VecDeque::pop_front);
                }
                here
            };
            if stopped {
                stopped_as_recorded();
            }
            stopped
        }
    }
}

/// In a replay, where the running task's wait `in` this ended: if its recording says its stop
/// found it there, it's stopped now, as it was then, and this gives back true.
#[cfg_attr(
    not(any(test, feature = "threads")),
    expect(dead_code, reason = "only launched tasks are stopped")
)]
pub(crate) fn stopped_in_as_recorded(waiting: StoppedIn) -> bool {
    let stopped = will_be_stopped_in(waiting, true);
    if stopped {
        stopped_as_recorded();
    }
    stopped
}

/// In a replay, whether the running task's recording says its stop found it `in` this wait next,
/// taking that entry if `taking`: so a block's end can tell before it waits whether it's stopped
/// there.
#[cfg_attr(
    not(any(test, feature = "threads")),
    expect(dead_code, reason = "only launched tasks are stopped")
)]
pub(crate) fn will_be_stopped_in(waiting: StoppedIn, taking: bool) -> bool {
    let Mode::Replay(player) = mode() else { return false };
    let task = running();
    let mut player = player.lock().unwrap_or_else(PoisonError::into_inner);
    let stream = player.streams.get_mut(&*task);
    let stopped =
        stream.as_ref().and_then(|stream| stream.front()).is_some_and(|(next, _)| next == waiting.entry());
    if stopped && taking {
        stream.and_then(VecDeque::pop_front);
    }
    stopped
}

fn log(file: &Mutex<std::fs::File>, task: &str, call: &str, payload: &[u8]) {
    log_entries(file, task, &[(call, payload)]);
}

/// Logs `task`'s entries, each a call and what it gave, together.
fn log_entries(file: &Mutex<std::fs::File>, task: &str, entries: &[(&str, &[u8])]) {
    // Once an entry is lost, none comes after it: the log stops there, without its end mark, so a
    // replay says it was cut short rather than finding a gap and blaming the program.
    if LOG_FAILED.load(Ordering::Relaxed) {
        return;
    }
    let mut bytes = Vec::new();
    for (call, payload) in entries {
        let too_big = u16::try_from(task.len()).is_err()
            || u16::try_from(call.len()).is_err()
            || u32::try_from(payload.len()).is_err();
        if too_big {
            stopped(&format!("`{call}` gave {} bytes, more than a recording can hold", payload.len()));
            return;
        }
        bytes.extend(entry(task, call, payload));
    }
    // Written at once, so a run that crashes or is killed leaves what it took.
    let wrote = file.lock().unwrap_or_else(PoisonError::into_inner).write_all(&bytes);
    if let Err(error) = wrote {
        stopped(&error.to_string());
    }
}

/// Stops recording, saying why once: the run goes on, but what it records from here can't be
/// replayed, and a replay of it will say it was cut short.
fn stopped(why: &str) {
    if !LOG_FAILED.swap(true, Ordering::Relaxed) {
        let _ = writeln!(
            std::io::stderr(),
            "warning: this run's recording stopped, as {why}; a replay of it will stop where it does"
        );
    }
}

/// The run is ending: in a recording, the log gets its end mark, once, so a replay can tell it
/// whole from one cut short.
pub fn ended() {
    static ENDED: AtomicBool = AtomicBool::new(false);
    // Only a mode already set: a recording that couldn't be opened stops the program while its
    // mode is still being worked out, and has nothing to mark.
    if let Some(Mode::Record(file)) = MODE.get()
        && !ENDED.swap(true, Ordering::Relaxed)
    {
        log(file, "", END, &[]);
    }
}

static LOG_FAILED: AtomicBool = AtomicBool::new(false);

/// One entry of a log.
/// One entry of a log. Lengths too big for their fields are refused by `log` before it gets here.
fn entry(task: &str, call: &str, payload: &[u8]) -> Vec<u8> {
    let mut entry = Vec::with_capacity(task.len() + call.len() + payload.len() + 8);
    entry.extend(u16::try_from(task.len()).unwrap_or(0).to_le_bytes());
    entry.extend(task.as_bytes());
    entry.extend(u16::try_from(call.len()).unwrap_or(0).to_le_bytes());
    entry.extend(call.as_bytes());
    entry.extend(u32::try_from(payload.len()).unwrap_or(0).to_le_bytes());
    entry.extend(payload);
    entry
}

/// The logged result of the running task's next call, which must be `call`.
fn replayed(player: &Mutex<Player>, call: &str) -> Result<Vec<u8>, String> {
    let task = running();
    let mut player = player.lock().unwrap_or_else(PoisonError::into_inner);
    let stream = player.streams.get_mut(&*task);
    let Some((next, _)) = stream.as_ref().and_then(|stream| stream.front()) else {
        if !player.whole {
            return Err(cut_short(call, &task));
        }
        return Err(format!(
            "this replay asked for `{call}`{}, where its recording had nothing more, so it isn't the run \
             that was recorded: something it depends on wasn't recorded, or the program has changed",
            in_task(&task)
        ));
    };
    if next != call {
        return Err(format!(
            "this replay asked for `{call}`{}, where its recording asked for `{next}`, so it isn't the run \
             that was recorded: something it depends on wasn't recorded, or the program has changed",
            in_task(&task)
        ));
    }
    let (_, payload) = stream.and_then(VecDeque::pop_front).unwrap_or_default();
    Ok(payload)
}

/// What a replay says when it runs past the end of a recording that was cut short: the log's
/// fault, not the program's.
fn cut_short(call: &str, task: &str) -> String {
    format!(
        "this replay asked for `{call}`{}, past the end of its recording, which was cut short: the run \
         it records was stopped, or writing it failed, before it ended",
        in_task(task)
    )
}

/// What a value is, in the log.
mod tag {
    pub(super) const UNIT: u8 = 0;
    pub(super) const NONE: u8 = 1;
    pub(super) const BOOL: u8 = 2;
    pub(super) const INT: u8 = 3;
    pub(super) const FLOAT: u8 = 4;
    pub(super) const STR: u8 = 5;
    pub(super) const BYTES: u8 = 6;
    pub(super) const LIST: u8 = 7;
    pub(super) const TUPLE: u8 = 8;
    pub(super) const DICT: u8 = 9;
    pub(super) const SET: u8 = 10;
    pub(super) const INSTANCE: u8 = 11;
    pub(super) const OK: u8 = 12;
    pub(super) const ERR: u8 = 13;
    /// A value that isn't plain data, which a replay can't give back.
    pub(super) const UNRECORDABLE: u8 = 14;
    pub(super) const FAILED: u8 = 15;
}

fn encode_result(result: &Result<Value, Diagnostic>, out: &mut Vec<u8>) {
    match result {
        Ok(value) => encode(value, out),
        Err(diagnostic) => {
            out.push(tag::FAILED);
            text(&diagnostic.message, out);
            match &diagnostic.help {
                Some(help) => {
                    out.push(1);
                    text(help, out);
                }
                None => out.push(0),
            }
        }
    }
}

fn text(text: &str, out: &mut Vec<u8>) {
    out.extend(u32::try_from(text.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend(text.as_bytes());
}

fn count(count: usize, out: &mut Vec<u8>) {
    out.extend(u32::try_from(count).unwrap_or(u32::MAX).to_le_bytes());
}

fn encode(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Unit => out.push(tag::UNIT),
        Value::None => out.push(tag::NONE),
        Value::Bool(bool) => out.extend([tag::BOOL, u8::from(*bool)]),
        Value::Int(int) => {
            out.push(tag::INT);
            out.extend(int.to_le_bytes());
        }
        Value::Float(float) => {
            out.push(tag::FLOAT);
            out.extend(float.to_bits().to_le_bytes());
        }
        Value::Str(string) => {
            out.push(tag::STR);
            text(string, out);
        }
        Value::Bytes(bytes) => {
            out.push(tag::BYTES);
            count(bytes.len(), out);
            out.extend(bytes.iter());
        }
        Value::List(items) | Value::Tuple(items) | Value::Set(items) => {
            out.push(match value {
                Value::List(_) => tag::LIST,
                Value::Tuple(_) => tag::TUPLE,
                _ => tag::SET,
            });
            count(items.len(), out);
            for item in items.iter() {
                encode(item, out);
            }
        }
        Value::Packed(packed) => encode(&packed.unpack(), out),
        Value::Dict(pairs) => {
            out.push(tag::DICT);
            count(pairs.len(), out);
            for (key, item) in pairs.iter() {
                encode(key, out);
                encode(item, out);
            }
        }
        Value::Instance(instance) => {
            out.push(tag::INSTANCE);
            text(&instance.type_name, out);
            match &instance.case {
                Some(case) => {
                    out.push(1);
                    text(case, out);
                }
                None => out.push(0),
            }
            out.push(u8::from(instance.is_error));
            match &instance.cause {
                Some(cause) => {
                    out.push(1);
                    encode(cause, out);
                }
                None => out.push(0),
            }
            count(instance.fields.len(), out);
            for (name, field) in &instance.fields {
                text(name, out);
                encode(field, out);
            }
        }
        Value::Ok(held) | Value::Err(held) => {
            out.push(if matches!(value, Value::Ok(_)) { tag::OK } else { tag::ERR });
            encode(held, out);
        }
        _ => out.push(tag::UNRECORDABLE),
    }
}

/// Reads values back from a log entry.
struct Reader<'log> {
    bytes: &'log [u8],
    at: usize,
    /// How deep in values inside values it's reading: a log is read back with a limit, so one
    /// made to nest without end can't take `fennec replay` down.
    depth: usize,
}

/// How deep a recorded value may nest: far past anything a capability gives back.
const DEEPEST: usize = 256;

impl Reader<'_> {
    fn byte(&mut self) -> Option<u8> {
        let byte = *self.bytes.get(self.at)?;
        self.at += 1;
        Some(byte)
    }

    fn take(&mut self, length: usize) -> Option<&[u8]> {
        let taken = self.bytes.get(self.at..self.at + length)?;
        self.at += length;
        Some(taken)
    }

    fn eight(&mut self) -> Option<[u8; 8]> {
        self.take(8)?.try_into().ok()
    }

    fn count(&mut self) -> Option<usize> {
        usize::try_from(u32::from_le_bytes(self.take(4)?.try_into().ok()?)).ok()
    }

    fn text(&mut self) -> Option<Rc<str>> {
        let length = self.count()?;
        Some(Rc::from(std::str::from_utf8(self.take(length)?).ok()?))
    }

    fn values(&mut self) -> Option<Vec<Value>> {
        let length = self.count()?;
        (0..length).map(|_| self.value()).collect()
    }

    fn value(&mut self) -> Option<Value> {
        if self.depth >= DEEPEST {
            return None;
        }
        self.depth += 1;
        let value = self.value_inside();
        self.depth -= 1;
        value
    }

    fn value_inside(&mut self) -> Option<Value> {
        Some(match self.byte()? {
            tag::UNIT => Value::Unit,
            tag::NONE => Value::None,
            tag::BOOL => Value::Bool(self.byte()? != 0),
            tag::INT => Value::Int(i64::from_le_bytes(self.eight()?)),
            tag::FLOAT => Value::Float(f64::from_bits(u64::from_le_bytes(self.eight()?))),
            tag::STR => Value::Str(self.text()?),
            tag::BYTES => {
                let length = self.count()?;
                Value::Bytes(Rc::from(self.take(length)?))
            }
            tag::LIST => Value::List(Rc::new(self.values()?)),
            tag::TUPLE => Value::Tuple(Rc::new(self.values()?)),
            tag::SET => Value::Set(Rc::new(self.values()?)),
            tag::DICT => {
                let length = self.count()?;
                let pairs: Option<Vec<(Value, Value)>> =
                    (0..length).map(|_| Some((self.value()?, self.value()?))).collect();
                Value::Dict(Rc::new(crate::pairs::Pairs::from(pairs?)))
            }
            tag::INSTANCE => {
                let type_name = self.text()?;
                let case = if self.byte()? == 1 { Some(self.text()?) } else { None };
                let is_error = self.byte()? != 0;
                let raised_from = if self.byte()? == 1 { Some(self.value()?) } else { None };
                let length = self.count()?;
                let fields: Option<Vec<(Rc<str>, Value)>> =
                    (0..length).map(|_| Some((self.text()?, self.value()?))).collect();
                Value::Instance(Rc::new(Instance {
                    type_name,
                    case,
                    fields: fields?,
                    is_error,
                    cause: raised_from,
                    // What a capability gives back never has notes: a program adds them after.
                    notes: None,
                }))
            }
            tag::OK => Value::Ok(Rc::new(self.value()?)),
            tag::ERR => Value::Err(Rc::new(self.value()?)),
            _ => return None,
        })
    }
}

fn decode_result(payload: &[u8], span: Span) -> Option<Result<Value, Diagnostic>> {
    let mut reader = Reader { bytes: payload, at: 0, depth: 0 };
    if payload.first() == Some(&tag::FAILED) {
        reader.byte()?;
        let message = reader.text()?;
        let help = if reader.byte()? == 1 { Some(reader.text()?) } else { None };
        let mut diagnostic = Diagnostic::new(message.to_string(), span);
        if let Some(help) = help {
            diagnostic = diagnostic.with_help(help.to_string());
        }
        return Some(Err(diagnostic));
    }
    Some(Ok(reader.value()?))
}

#[cfg(test)]
mod tests;
