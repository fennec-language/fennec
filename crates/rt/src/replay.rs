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
//!   task it launched, has a `stopped at its block's end` entry, and one its stop found serving has a
//!   `stopped serving` entry. A replay stops the task at that
//!   wait, as the recorded run did. So a replay stops nothing by the clock: its block's ends wait
//!   for their tasks without a deadline, and stop none when they fail, as the recording says
//!   where each task was stopped.
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
//! The log is `FENREPL6`, the header's length as four bytes and the header, then the entries. Each
//! entry is its task's name and its call's name, each with its length as two bytes, then what it
//! gave, with its length as four. All numbers are little-endian. A `FENREPL1` log, from before
//! tasks were named, has no task names, and replays as `main`'s. A log from an older Fennec, which
//! recorded less of what tasks do, replays as it did then, saying so when it has tasks: a
//! `FENREPL5` log has no task stopped while it served, a `FENREPL4` log no `parallel` ranges a
//! failure kept from starting, and a `FENREPL3` or `FENREPL2` log no stops.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io::Write as _;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use fennec_syntax::{Diagnostic, Span};

use crate::value::{Instance, Value};

const MAGIC: &[u8; 8] = b"FENREPL6";

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

/// The entry that says how many workers the recorded run had, logged first, under no task.
const WORKERS: &str = "workers";

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
    /// Whether the recording logged standard output's bursts, so a replay writes them in that
    /// order: not one made before that was recorded, nor one of a run that printed nothing.
    orders_output: bool,
    /// What tasks have written that isn't due yet, each task's bytes in order, by its name.
    held: HashMap<String, VecDeque<u8>>,
    /// Whether the log ends with the mark its run wrote as it ended, so a replay that runs out of
    /// it is the program's doing, not the log's.
    whole: bool,
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
}

impl Task {
    fn named(name: Arc<str>) -> Task {
        Task { name, started: 0, made: 0, stopped: false }
    }

    /// Whether, in a replay, its recording said a stop or a cancel cut it short.
    #[cfg_attr(
        not(any(test, feature = "threads")),
        expect(dead_code, reason = "only a server's rounds are cancelled, which need threads")
    )]
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
        Mode::Replay(player) => end_turn(player, place),
    }
}

/// Ends the turn at the front of `place`'s stream.
fn end_turn(player: &Mutex<Player>, place: &str) {
    let mut player = player.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(stream) = player.streams.get_mut(place) {
        stream.pop_front();
    }
}

/// How long a replay waits for its turn at a place before saying it's diverged: long enough for
/// any task the turn is waiting on to get there.
const TURN_PATIENCE: std::time::Duration = std::time::Duration::from_secs(10);

/// In a replay, waits until it's the running task's turn to do `what` at `place`, and gives back
/// what it came to in the recording. The turn stays this task's until `met` ends it. `Ok(None)`
/// when this isn't a replay. A replay that asks for something the recording didn't do there has
/// diverged, and says so.
pub(crate) fn turn(place: Option<&str>, what: &str) -> Result<Option<i64>, String> {
    let (Some(place), Mode::Replay(player)) = (place, mode()) else { return Ok(None) };
    turn_in(player, (place, what), TURN_PATIENCE, crate::embed::make_way).map(Some)
}

/// Waits up to `patience` for the running task's turn to do `what` at `place` in `player`'s log,
/// calling `make_way` between looks, and gives back what it came to.
fn turn_in(
    player: &Mutex<Player>,
    (place, what): (&str, &str),
    patience: std::time::Duration,
    make_way: fn(),
) -> Result<i64, String> {
    let asked = format!("{} {what}", running());
    let started = std::time::Instant::now();
    loop {
        let waiting_for = {
            let player = player.lock().unwrap_or_else(PoisonError::into_inner);
            let front = player.streams.get(place).and_then(|stream| stream.front());
            match front.map(|(next, payload)| (next.clone(), payload.clone())) {
                None if !player.whole => return Err(cut_short(what, place)),
                None => {
                    return Err(format!(
                        "this replay's task asked for `{asked}` at {place}, where its recording had nothing \
                         more, so it isn't the run that was recorded: something it depends on wasn't \
                         recorded, or the program has changed"
                    ));
                }
                Some((next, payload)) if next == asked => {
                    let bytes: [u8; 8] =
                        payload.try_into().map_err(|_| format!("this replay's {place} is damaged"))?;
                    return Ok(i64::from_le_bytes(bytes));
                }
                Some((next, _)) => next,
            }
        };
        if started.elapsed() > patience {
            return Err(format!(
                "this replay asked for `{asked}` at {place}, where its recording has `{waiting_for}` next, \
                 so it isn't the run that was recorded: something it depends on wasn't recorded, or the \
                 program has changed"
            ));
        }
        make_way();
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
        TASK.with_borrow_mut(|task| {
            if let Some(task) = task.as_mut() {
                task.stopped = true;
            }
        });
        crate::embed::serve::fail_as_stopped();
    }
    // Without threads there are no tasks to stop, so no recording a runtime like this one made says
    // a wait was cut short. One made with them that does still fails here, as it did.
    #[cfg(not(any(test, feature = "threads")))]
    crate::embed::fail_with(Diagnostic::new("this task was stopped", Span::default()));
}

/// Whether this run is a replay, whose outside inputs all come from its recording.
pub(crate) fn replaying() -> bool {
    matches!(mode(), Mode::Replay(_))
}

/// Whether this run is being recorded or replayed.
pub(crate) fn active() -> bool {
    !matches!(mode(), Mode::Off)
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
    let named = [MAGIC, MAGIC_BEFORE_SERVING_STOPS, MAGIC_BEFORE_SKIPS, MAGIC_BEFORE_STOPS, MAGIC_UNMARKED]
        .iter()
        .any(|magic| log.starts_with(*magic));
    let mut player = Player {
        streams: HashMap::new(),
        workers: None,
        orders_output: false,
        held: HashMap::new(),
        whole: ![MAGIC, MAGIC_BEFORE_SERVING_STOPS, MAGIC_BEFORE_SKIPS, MAGIC_BEFORE_STOPS]
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
    let mut held: Vec<&String> =
        player.held.iter().filter(|(_, bytes)| !bytes.is_empty()).map(|(task, _)| task).collect();
    held.sort();
    if let Some(task) = held.first() {
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
            let result = live();
            let mut payload = Vec::new();
            encode_result(&result, &mut payload);
            let task = running();
            log(file, &task, &call.to_string(), &payload);
            note_cut_short(file, &task, failing);
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
    let _ = live();
    value(call, span, || unreachable!("a replay gives back what was recorded"))
}

/// A word an outside call gives, like the clock's reading, recorded and replayed as `value` is.
pub(crate) fn word(call: &str, live: impl FnOnce() -> i64) -> Result<i64, String> {
    match mode() {
        Mode::Off => Ok(live()),
        Mode::Record(file) => {
            let failing = crate::embed::failed();
            let word = live();
            let task = running();
            log(file, &task, call, &word.to_le_bytes());
            note_cut_short(file, &task, failing);
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

/// In a recording, after a call `task` made, which it made `failing` or not: if a stop cut the
/// call short, and so it's failing now as a stopped task, logs that, for a replay to stop it there.
fn note_cut_short(file: &Mutex<std::fs::File>, task: &str, failing: bool) {
    // A server's round is cancelled when its server stops, rather than stopped, and fails as
    // stopped at its wait all the same.
    #[cfg(any(test, feature = "threads"))]
    if !failing
        && crate::embed::failed()
        && (crate::embed::threads::stopped() || fennec_parked::is_cancelled())
    {
        log(file, task, CUT_SHORT, &[]);
    }
    #[cfg(not(any(test, feature = "threads")))]
    let _ = (file, task, failing);
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
    // Once an entry is lost, none comes after it: the log stops there, without its end mark, so a
    // replay says it was cut short rather than finding a gap and blaming the program.
    if LOG_FAILED.load(Ordering::Relaxed) {
        return;
    }
    let too_big = u16::try_from(task.len()).is_err()
        || u16::try_from(call.len()).is_err()
        || u32::try_from(payload.len()).is_err();
    if too_big {
        stopped(&format!("`{call}` gave {} bytes, more than a recording can hold", payload.len()));
        return;
    }
    // Written at once, so a run that crashes or is killed leaves what it took.
    let wrote = file.lock().unwrap_or_else(PoisonError::into_inner).write_all(&entry(task, call, payload));
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
