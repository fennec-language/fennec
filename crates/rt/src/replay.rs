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
//! - **Where tasks meet:** a channel is a place, named by the task that made it, as `0.2:c1`. What
//!   each task did there, send, receive, listen or close, is logged in the order it took effect,
//!   with what it came to, and a replay makes each task wait for its turn. So messages go to the
//!   same receivers, and a receive that timed out times out again, without waiting.
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
//! The log is `FENREPL2`, the header's length as four bytes and the header, then the entries. Each
//! entry is its task's name and its call's name, each with its length as two bytes, then what it
//! gave, with its length as four. All numbers are little-endian. A `FENREPL1` log, from before
//! tasks were named, has no task names, and replays as `main`'s.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io::Write as _;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use fennec_syntax::{Diagnostic, Span};

use crate::value::{Instance, Value};

const MAGIC: &[u8; 8] = b"FENREPL2";

/// A log from before tasks were named: every entry is `main`'s.
const MAGIC_UNNAMED: &[u8; 8] = b"FENREPL1";

/// The entry that says how many workers the recorded run had, logged first, under no task.
const WORKERS: &str = "workers";

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
}

/// A task, for its entries: its name, and how many tasks it has started, to name the next one.
pub(crate) struct Task {
    name: Arc<str>,
    started: u32,
    /// How many places where tasks meet, like channels, it has made, to name the next one.
    made: u32,
}

impl Task {
    fn named(name: Arc<str>) -> Task {
        Task { name, started: 0, made: 0 }
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
    #[cfg_attr(
        not(any(test, feature = "threads")),
        expect(dead_code, reason = "only servers use it, which need threads")
    )]
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
    Ok(Mode::Replay(Mutex::new(player)))
}

/// A log's entries, each task's in a stream of its own: `None` if it's damaged.
fn player_of(log: &[u8]) -> Option<Player> {
    let named = log.starts_with(MAGIC);
    let mut player = Player { streams: HashMap::new(), workers: None };
    let mut reader = Entries { log, at: entries_start(log)? };
    while reader.at < log.len() {
        let task = if named { reader.name()? } else { "0".to_owned() };
        let call = reader.name()?;
        let payload = reader.payload()?;
        if task.is_empty() && call == WORKERS {
            let bytes: [u8; 8] = payload.as_slice().try_into().ok()?;
            player.workers = usize::try_from(u64::from_le_bytes(bytes)).ok();
            continue;
        }
        player.streams.entry(task).or_default().push_back((call, payload));
    }
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
    if magic != MAGIC && magic != MAGIC_UNNAMED {
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
    let (task, next) = left_over(&player.lock().unwrap_or_else(PoisonError::into_inner))?;
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
            let result = live();
            let mut payload = Vec::new();
            encode_result(&result, &mut payload);
            log(file, &running(), &call.to_string(), &payload);
            result
        }
        Mode::Replay(player) => {
            let call = call.to_string();
            let payload = replayed(player, &call).map_err(|why| Diagnostic::new(why, span))?;
            decode_result(&payload, span).ok_or_else(|| {
                Diagnostic::new(format!("this replay's recording of `{call}` couldn't be read back"), span)
            })?
        }
    }
}

/// A word an outside call gives, like the clock's reading, recorded and replayed as `value` is.
pub(crate) fn word(call: &str, live: impl FnOnce() -> i64) -> Result<i64, String> {
    match mode() {
        Mode::Off => Ok(live()),
        Mode::Record(file) => {
            let word = live();
            log(file, &running(), call, &word.to_le_bytes());
            Ok(word)
        }
        Mode::Replay(player) => {
            let payload = replayed(player, call)?;
            let bytes: [u8; 8] =
                payload.try_into().map_err(|_| format!("this replay's recording of `{call}` is damaged"))?;
            Ok(i64::from_le_bytes(bytes))
        }
    }
}

fn log(file: &Mutex<std::fs::File>, task: &str, call: &str, payload: &[u8]) {
    // Written at once, so a run that crashes or is killed leaves what it took.
    let wrote = file.lock().unwrap_or_else(PoisonError::into_inner).write_all(&entry(task, call, payload));
    // Said once: the run goes on, but what it records from here can't be replayed.
    if let Err(error) = wrote
        && !LOG_FAILED.swap(true, Ordering::Relaxed)
    {
        let _ = writeln!(std::io::stderr(), "warning: this run's recording stopped: {error}");
    }
}

static LOG_FAILED: AtomicBool = AtomicBool::new(false);

/// One entry of a log.
fn entry(task: &str, call: &str, payload: &[u8]) -> Vec<u8> {
    let mut entry = Vec::with_capacity(task.len() + call.len() + payload.len() + 8);
    entry.extend(u16::try_from(task.len()).unwrap_or(u16::MAX).to_le_bytes());
    entry.extend(task.as_bytes());
    entry.extend(u16::try_from(call.len()).unwrap_or(u16::MAX).to_le_bytes());
    entry.extend(call.as_bytes());
    entry.extend(u32::try_from(payload.len()).unwrap_or(u32::MAX).to_le_bytes());
    entry.extend(payload);
    entry
}

/// The logged result of the running task's next call, which must be `call`.
fn replayed(player: &Mutex<Player>, call: &str) -> Result<Vec<u8>, String> {
    let task = running();
    let mut player = player.lock().unwrap_or_else(PoisonError::into_inner);
    let stream = player.streams.get_mut(&*task);
    let Some((next, _)) = stream.as_ref().and_then(|stream| stream.front()) else {
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
}

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
    let mut reader = Reader { bytes: payload, at: 0 };
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
