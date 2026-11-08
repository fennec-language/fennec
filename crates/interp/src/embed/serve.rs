//! Servers: `for conn in parallel(server)`, each connection a task that parks while it waits
//! on the network, so a few threads serve thousands of connections (decisions.md: "A task
//! waiting on the network is parked, not its thread").
//!
//! A server runs on I/O workers, one per core, while the calling thread waits, lending them what
//! it holds, as it does for a `parallel` call's ranges (see `threads`). Each has its own runtime,
//! its own `mio::Poll` and its own scheduler of parked tasks (`fennec_parked`). The first accepts
//! connections and hands each to the next worker in turn, through the worker's inbox and waker. A
//! connection's task stays on the worker that took it, so what it makes stays that worker's. A task
//! ends with its connection, and what it made goes with it, so nothing's handed back.
//!
//! A connection is registered with its worker's poll once, for reading and writing, under a
//! token of its own. Its calls read and write until the socket says it would block, then park
//! on that token, and the worker resumes the task when the poll says the socket is ready or
//! its timeout passes. Readiness is edge-triggered, which is safe because a task only waits
//! after the socket said it would block.
//!
//! A wait that can't park, because it isn't in a task or the runtime's state is borrowed (in
//! the interpreter), blocks its thread on a poll of its own instead. That keeps it correct but
//! costs a thread, so it's never silent: each one is counted for `FENNEC_COUNTS`, and a dev
//! build says so once for each place it happens.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{ErrorKind, Read as _, Write as _};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use fennec_parked::{Scheduler, Step, TaskId, Wake};
use mio::net::{TcpListener, TcpStream};
use mio::{Events, Interest, Poll, Registry, Token};

use super::parallel::{Latch, Pool};

/// A connection task's stack. Only the pages a task touches take memory.
const STACK: usize = 1024 * 1024;

/// A range's stack, when it runs as a parked task: as big as a worker's, since a range runs the
/// same native code. Only the pages it touches take memory, and a few are kept for the next.
const RANGE_STACK: usize = 64 << 20;
const RANGE_STACKS_KEPT: usize = 8;

/// What a task parks with to let the others on its thread run, and be resumed after them: a lock
/// that's taken, or a test of tasks taking turns.
pub(in crate::embed) const YIELD: u64 = u64::MAX;

/// What a parked wait is woken with.
const READY: u64 = 0;
const TIMED_OUT: u64 = 1;
/// A launched task's wait, woken by its stop. Not `Wake::Cancelled`, after which every wait a task
/// comes to ends at once: a cleanup in its grace has to be able to wait.
const STOPPED: u64 = 2;

/// The workers' wakers' token, and the listeners' below it, one for each of up to `SERVERS`
/// servers running at once; connections count up from 0, below them all.
pub(in crate::embed) const WAKER: Token = Token(usize::MAX - 2);
const LISTENERS: usize = usize::MAX - 3;
const SERVERS: u64 = 4096;
const CONNECTIONS: usize = LISTENERS - 4096;

/// Waits that blocked their thread because they couldn't park, for `FENNEC_COUNTS`.
static BLOCKED_WAITS: AtomicU64 = AtomicU64::new(0);

/// The places that have said a wait blocked, so each says so once.
static WARNED: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);

thread_local! {
    /// On an I/O worker, its poll's registry, for connections to register with.
    static REGISTRY: RefCell<Option<Registry>> = const { RefCell::new(None) };
    /// The token the next connection on this thread gets.
    static NEXT_TOKEN: Cell<usize> = const { Cell::new(0) };
    /// What the wait a task is about to park on waits for, and when it gives up, for its
    /// worker to read.
    static PARKING: Cell<Option<(Interest, Option<Instant>)>> = const { Cell::new(None) };
    /// For tests: wakes every parked task each time its worker polls, as a storm of readiness
    /// it doesn't wait for would, like a socket that keeps saying it's writable.
    #[cfg(test)]
    static WAKE_ALL: Cell<bool> = const { Cell::new(false) };
    /// When the task running on this worker was last resumed.
    static SLICE: Cell<Option<Instant>> = const { Cell::new(None) };
    /// Whether, when it was, anything else on the worker wanted the thread: another task ready or
    /// waiting, or a server served.
    static OTHERS: Cell<bool> = const { Cell::new(false) };
    /// What a connection reads into, shared by the thread's tasks, which run one at a time and
    /// never park while reading: so a parked task holds no buffer of its own.
    static BUFFER: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// How much one read takes at most.
const READ_BYTES: usize = 16 * 1024;

/// Reads what's there, up to `limit` bytes, onto the end of `into`, through the thread's
/// shared buffer.
fn read_onto(stream: &mut TcpStream, into: &mut Vec<u8>, limit: usize) -> std::io::Result<usize> {
    BUFFER.with_borrow_mut(|buffer| {
        buffer.resize(READ_BYTES, 0);
        let room = buffer.get_mut(..limit.min(READ_BYTES)).unwrap_or_default();
        let read = stream.read(room)?;
        into.extend(room.get(..read).unwrap_or_default());
        Ok(read)
    })
}

/// How many waits have blocked their thread rather than parking.
pub fn blocked_waits() -> u64 {
    BLOCKED_WAITS.load(Ordering::Relaxed)
}

/// How a network call fails.
#[derive(Debug, PartialEq, Eq)]
pub enum Failure {
    TimedOut(Duration),
    /// The other side hung up during a write.
    Closed,
    /// The task was cancelled, as its server stopped.
    Cancelled,
    Other(String),
}

impl Failure {
    fn of(error: &std::io::Error) -> Failure {
        match error.kind() {
            ErrorKind::BrokenPipe | ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted => {
                Failure::Closed
            }
            _ => Failure::Other(error.to_string()),
        }
    }
}

/// How long a call may wait in all, and so when it gives up: fixed when the call starts, so
/// waking early to find the socket still not ready doesn't put it off.
#[derive(Clone, Copy)]
struct Until {
    timeout: Option<Duration>,
    deadline: Option<Instant>,
}

impl Until {
    fn after(timeout: Option<Duration>) -> Until {
        Until { timeout, deadline: timeout.map(|timeout| Instant::now() + timeout) }
    }

    /// How long is left, for a wait that blocks its thread.
    fn left(self) -> Option<Duration> {
        self.deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }
}

/// What waiting on a socket came to.
#[derive(Debug, PartialEq, Eq)]
enum Waited {
    Ready,
    TimedOut,
    Cancelled,
}

/// A connection, read and written by its task, which parks while it would block.
pub struct Conn {
    stream: TcpStream,
    token: Token,
    /// Bytes read past the end of the last line.
    ahead: Vec<u8>,
    /// Whether it's registered with this thread's poll.
    registered: bool,
}

impl Conn {
    /// Takes a connection, registering it with this thread's poll when this is an I/O worker.
    pub fn new(mut stream: TcpStream) -> Conn {
        let token = Token(NEXT_TOKEN.get());
        NEXT_TOKEN.set(token.0.wrapping_add(1) % CONNECTIONS);
        let registered = REGISTRY.with_borrow(|registry| {
            registry.as_ref().is_some_and(|registry| {
                registry.register(&mut stream, token, Interest::READABLE | Interest::WRITABLE).is_ok()
            })
        });
        Conn { stream, token, ahead: Vec::new(), registered }
    }

    /// Up to `count` bytes, empty at the end.
    pub fn read(&mut self, count: usize, timeout: Option<Duration>) -> Result<Vec<u8>, Failure> {
        let until = Until::after(timeout);
        if !self.ahead.is_empty() {
            let taken = self.ahead.len().min(count);
            return Ok(self.ahead.drain(..taken).collect());
        }
        loop {
            let mut bytes = Vec::new();
            match read_onto(&mut self.stream, &mut bytes, count) {
                Ok(_) => return Ok(bytes),
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    self.wait(Interest::READABLE, until, "read")?;
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(Failure::of(&error)),
            }
        }
    }

    /// The next line, without its line ending, or `None` at the end.
    pub fn read_line(&mut self, timeout: Option<Duration>) -> Result<Option<Vec<u8>>, Failure> {
        let until = Until::after(timeout);
        loop {
            if let Some(end) = self.ahead.iter().position(|byte| *byte == b'\n') {
                let mut line: Vec<u8> = self.ahead.drain(..=end).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(Some(line));
            }
            match read_onto(&mut self.stream, &mut self.ahead, READ_BYTES) {
                Ok(0) if self.ahead.is_empty() => return Ok(None),
                Ok(0) => return Ok(Some(std::mem::take(&mut self.ahead))),
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    self.wait(Interest::READABLE, until, "read_line")?;
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(Failure::of(&error)),
            }
        }
    }

    /// The other side's address.
    pub fn peer(&self) -> String {
        self.stream.peer_addr().map(|peer| peer.to_string()).unwrap_or_default()
    }

    /// Writes all of `data`.
    pub fn write(&mut self, mut data: &[u8]) -> Result<(), Failure> {
        while !data.is_empty() {
            match self.stream.write(data) {
                Ok(0) => return Err(Failure::Closed),
                Ok(written) => data = data.get(written..).unwrap_or_default(),
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    self.wait(Interest::WRITABLE, Until::after(None), "write")?;
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(Failure::of(&error)),
            }
        }
        Ok(())
    }

    /// Waits until the socket may be ready for `interest`: parked, when this is a task on an
    /// I/O worker and the runtime's state isn't borrowed, and blocking the thread otherwise.
    fn wait(&mut self, interest: Interest, until: Until, call: &'static str) -> Result<(), Failure> {
        // A wait woken early, to find the socket still not ready, gives up once its time's up.
        if until.deadline.is_some_and(|deadline| deadline <= Instant::now()) {
            return Err(Failure::TimedOut(until.timeout.unwrap_or_default()));
        }
        // What a task has printed comes out before it waits, however long that is.
        super::write_kept_output();
        let waited = if self.registered && !super::state_is_borrowed() {
            PARKING.set(Some((interest, until.deadline)));
            match fennec_parked::park(u64::try_from(self.token.0).unwrap_or(0)) {
                Some(Wake::Ready(TIMED_OUT)) => Waited::TimedOut,
                Some(Wake::Cancelled) => Waited::Cancelled,
                Some(Wake::Ready(_)) => Waited::Ready,
                None => self.block(interest, until, call)?,
            }
        } else {
            self.block(interest, until, call)?
        };
        match waited {
            Waited::Ready => Ok(()),
            Waited::TimedOut => Err(Failure::TimedOut(until.timeout.unwrap_or_default())),
            Waited::Cancelled => Err(Failure::Cancelled),
        }
    }

    /// Waits on a poll of its own, blocking the thread: counted, and said once per call in a
    /// dev build.
    fn block(&mut self, interest: Interest, until: Until, call: &'static str) -> Result<Waited, Failure> {
        let timeout = until.left();
        BLOCKED_WAITS.fetch_add(1, Ordering::Relaxed);
        warn_once(call);
        let mut poll = Poll::new().map_err(|error| Failure::of(&error))?;
        let token = Token(0);
        if self.registered {
            REGISTRY.with_borrow(|registry| {
                registry.as_ref().map(|registry| registry.deregister(&mut self.stream))
            });
        }
        poll.registry().register(&mut self.stream, token, interest).map_err(|error| Failure::of(&error))?;
        let mut events = Events::with_capacity(1);
        let polled = poll.poll(&mut events, timeout);
        let _ = poll.registry().deregister(&mut self.stream);
        if self.registered {
            let both = Interest::READABLE | Interest::WRITABLE;
            REGISTRY.with_borrow(|registry| {
                registry.as_ref().map(|registry| registry.register(&mut self.stream, self.token, both))
            });
        }
        polled.map_err(|error| Failure::of(&error))?;
        Ok(if events.is_empty() && timeout.is_some() { Waited::TimedOut } else { Waited::Ready })
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        if self.registered {
            REGISTRY.with_borrow(|registry| {
                registry.as_ref().map(|registry| registry.deregister(&mut self.stream))
            });
        }
    }
}

/// In a dev build, says once for each kind of call that a wait blocked its thread.
fn warn_once(call: &'static str) {
    if !cfg!(debug_assertions) {
        return;
    }
    let mut warned = WARNED.lock().unwrap_or_else(PoisonError::into_inner);
    if warned.get_or_insert_with(HashSet::new).insert(call) {
        let message = format!(
            "fennec: a network `{call}` blocked its thread instead of parking its task, because it \
             ran outside a task or inside the interpreter; a server loses its scalability this way\n"
        );
        let _ = std::io::stderr().write_all(message.as_bytes());
    }
}

/// What runs each connection: given it, it gives back 0, or anything else when it failed,
/// which ends only that connection.
pub type Handler = Arc<dyn Fn(Conn) -> i64 + Send + Sync>;

/// What a server's run came to.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Served {
    pub connections: u64,
    /// Connections whose handler gave back a failure.
    pub failed: u64,
    /// How tasks changed the owners of the blocks lent to them, for the lender to apply.
    pub owners: HashMap<u64, i64>,
}

impl Served {
    fn add(&mut self, theirs: Served) {
        self.connections += theirs.connections;
        self.failed += theirs.failed;
        for (address, by) in theirs.owners {
            *self.owners.entry(address).or_default() += by;
        }
    }
}

/// What a round of `for conn in parallel(server)` gives back when it failed, passing its failure up
/// out of the loop, which stops the server and cuts the other rounds short. 0 is a round that ended
/// normally, by `continue` too, and 1 one that crashed, which ends only itself.
pub(in crate::embed) const ROUND_FAILED: i64 = 2;
/// What a round gives back at a `break`: the server takes no more connections, and the rounds
/// running finish.
pub(in crate::embed) const ROUND_BROKE: i64 = 3;

/// Numbers servers apart, for their listeners' tokens and their tasks.
static NEXT_SERVER: AtomicU64 = AtomicU64::new(0);

/// One server, shared by every worker serving it.
struct Server {
    id: u64,
    handler: Handler,
    /// What the thread serving it lends its tasks.
    lent: Option<Arc<super::table::Table>>,
    /// Which worker the next connection goes to, in turn.
    next: AtomicUsize,
    /// What each worker served, added in as it leaves, and the count of those still serving.
    total: Mutex<Served>,
    left: Latch,
}

impl Server {
    /// Its listener's token: counting down from below the waker's, which connections' never reach.
    fn token(&self) -> Token {
        Token(LISTENERS - usize::try_from(self.id % SERVERS).unwrap_or(0))
    }
}

/// What a worker keeps of a server it serves.
struct Here {
    server: Arc<Server>,
    /// Its listener, on the one worker that accepts for it.
    listener: Option<TcpListener>,
    tasks: HashSet<TaskId>,
    served: Served,
    /// Whether a round's `break` has stopped it taking connections, for it to be left once the
    /// rounds running finish.
    draining: bool,
}

/// What a pool worker serves: its poll, its tasks, which task waits on which connection, and the
/// servers it's part of. It's the worker's own, so its tasks and what they make stay on it.
pub(in crate::embed) struct Io {
    poll: Poll,
    events: Events,
    pool: &'static Pool,
    at: usize,
    scheduler: Scheduler,
    /// The task parked on each connection's token, and when its wait gives up.
    waiting: HashMap<usize, (TaskId, Interest, Option<Instant>)>,
    ready: VecDeque<(TaskId, Wake)>,
    /// Which server each task serves a connection of.
    owners: HashMap<TaskId, u64>,
    servers: HashMap<u64, Here>,
    /// Ranges of `parallel` calls whose bodies may wait, each a parked task with its own accounts
    /// (see `threads::Accounts`). Task numbers are never given twice on a thread, so these and
    /// the connections' tasks share `ready` and `waiting`.
    ranges: Scheduler,
    accounts: HashMap<TaskId, super::threads::Accounts>,
    /// The runtime's temporaries and inputs each parked task holds, kept apart while it's parked:
    /// tasks take turns on the thread, so one's can't be left where another's truncating would
    /// cut them off, and an ended task's go with it.
    temporaries: HashMap<TaskId, super::Temporaries>,
}

impl Io {
    /// A worker's, on its own thread, with the poll its waker was made with.
    pub(in crate::embed) fn new(poll: Poll, workers: &'static Pool, at: usize) -> Io {
        REGISTRY.set(poll.registry().try_clone().ok());
        Io {
            poll,
            events: Events::with_capacity(1024),
            pool: workers,
            at,
            scheduler: Scheduler::new(STACK),
            waiting: HashMap::new(),
            ready: VecDeque::new(),
            owners: HashMap::new(),
            servers: HashMap::new(),
            ranges: Scheduler::keeping(RANGE_STACK, RANGE_STACKS_KEPT),
            accounts: HashMap::new(),
            temporaries: HashMap::new(),
        }
    }

    /// Starts a range as a parked task on this worker, so it can park when it waits. If there's no
    /// stack for it, it runs here and now, as a range whose body can't wait does.
    pub(in crate::embed) fn start_range(&mut self, range: Box<dyn FnOnce() -> i64>) {
        self.start_task(range, super::threads::Accounts::default());
    }

    /// Starts `run` as a parked task on this worker with `accounts` its own, giving back its number.
    /// If there's no stack for it, it runs here and now, and there's none.
    pub(in crate::embed) fn start_task(
        &mut self,
        run: Box<dyn FnOnce() -> i64>,
        mut accounts: super::threads::Accounts,
    ) -> Option<TaskId> {
        // The task runs it from its first resume, so it's kept here until then in case it can't.
        let run = Rc::new(Cell::new(Some(run)));
        let held = Rc::clone(&run);
        let made = self.ranges.spawn(move || held.take().map_or(0, |run| run()));
        if let Ok(task) = made {
            self.accounts.insert(task, accounts);
            self.ready.push_back((task, Wake::Ready(READY)));
            return Some(task);
        }
        if let Some(run) = run.take() {
            super::threads::swap_accounts(&mut accounts);
            run();
            super::threads::swap_accounts(&mut accounts);
        }
        None
    }

    /// Cuts short the wait `task` is parked in, if it's parked here.
    pub(in crate::embed) fn cancel(&mut self, task: TaskId) {
        let token = self.waiting.iter().find(|(_, (parked, ..))| *parked == task).map(|(token, _)| *token);
        if let Some(token) = token {
            self.waiting.remove(&token);
            self.ready.push_back((task, Wake::Ready(STOPPED)));
        }
    }

    /// Whether it serves anything or has tasks waiting, so its poll has to be looked at between
    /// jobs, for a socket that's ready or a wait whose time is up.
    pub(in crate::embed) fn active(&self) -> bool {
        !self.servers.is_empty() || !self.waiting.is_empty()
    }

    /// Whether a task is ready to run.
    pub(in crate::embed) fn has_ready(&self) -> bool {
        !self.ready.is_empty()
    }

    /// Joins a server: its tasks can read what's lent, and the worker given its listener accepts.
    fn join(&mut self, server: Arc<Server>, mut listener: Option<TcpListener>) {
        super::threads::serving_starts(server.lent.clone());
        if let Some(listener) = &mut listener
            && self.poll.registry().register(listener, server.token(), Interest::READABLE).is_err()
        {
            // Nothing accepts, so nothing's served: it stops only when it's stopped.
            return;
        }
        let id = server.id;
        let here =
            Here { server, listener, tasks: HashSet::new(), served: Served::default(), draining: false };
        self.servers.insert(id, here);
    }

    /// Leaves a server: its tasks here are cancelled, each told, ending and closing its
    /// connection, and what this worker served goes into the server's total.
    fn leave(&mut self, id: u64) {
        let Some(mut here) = self.servers.remove(&id) else { return };
        self.waiting.retain(|_, (task, ..)| !here.tasks.contains(task));
        self.ready.retain(|(task, _)| !here.tasks.contains(task));
        for task in here.tasks.drain() {
            self.owners.remove(&task);
            let _ = self.with_own_temporaries(task, |io| io.scheduler.cancel(task));
            self.temporaries.remove(&task);
            // A round cut short ends quietly, however it ended.
            super::clear_failure();
        }
        if let Some(listener) = &mut here.listener {
            let _ = self.poll.registry().deregister(listener);
        }
        let (owners, _) = super::threads::serving_ends();
        here.served.owners.extend(owners);
        here.server.total.lock().unwrap_or_else(PoisonError::into_inner).add(here.served);
        here.server.left.finished();
    }

    /// Makes a task for a connection of the server `id`, ready to start.
    fn take(&mut self, stream: TcpStream, id: u64) {
        let Some(here) = self.servers.get_mut(&id) else { return };
        let handler = Arc::clone(&here.server.handler);
        let made = self.scheduler.spawn(move || handler(Conn::new(stream)));
        if let Ok(task) = made {
            here.served.connections += 1;
            here.tasks.insert(task);
            self.owners.insert(task, id);
            self.ready.push_back((task, Wake::Ready(READY)));
        }
    }

    /// Runs each task that was ready when it was called until it parks or ends. One that parks to
    /// let others run goes after them, for the next call: the worker takes a job between, and
    /// tasks take turns.
    pub(in crate::embed) fn run_ready(&mut self) {
        if self.ready.is_empty() {
            return;
        }
        for _ in 0..self.ready.len() {
            let Some((task, wake)) = self.ready.pop_front() else { break };
            self.slice_starts();
            if self.accounts.contains_key(&task) {
                self.resume_range(task, wake);
                continue;
            }
            match self.with_own_temporaries(task, |io| io.scheduler.resume(task, wake)) {
                Step::Parked(YIELD) => self.ready.push_back((task, Wake::Ready(READY))),
                Step::Parked(token) => {
                    let token = usize::try_from(token).unwrap_or(usize::MAX);
                    let (interest, deadline) = PARKING.take().unwrap_or((Interest::READABLE, None));
                    self.waiting.insert(token, (task, interest, deadline));
                }
                Step::Done(0) | Step::Unknown => {
                    let _ = self.ended(task, false);
                }
                Step::Done(ROUND_BROKE) => {
                    if let Some(id) = self.ended(task, false) {
                        self.for_every_worker(move |io| io.drain(id));
                    }
                }
                Step::Done(ROUND_FAILED) => {
                    // Its failure passes up, out of the loop, which it's the code serving's to say.
                    // The other rounds are cut short.
                    if let Some(id) = self.ended(task, true) {
                        self.for_every_worker(move |io| io.leave(id));
                    }
                    super::clear_failure();
                }
                Step::Done(_) => {
                    // A crash ends only its round's task, and the server goes on.
                    let _ = self.ended(task, true);
                    // A failure ends only its connection, which says why, and the runtime's record
                    // of it goes. A task that failed holding a lock leaves what it guards half
                    // changed, and others waiting for it, so the program ends rather than only
                    // this task.
                    if super::threads::holds_locks() {
                        super::aot::stop_for_failure();
                    }
                    super::report_failure();
                }
            }
        }
        // What the tasks that just ran printed comes out before this worker goes on.
        super::write_kept_output();
    }

    /// Resumes a range's task with its own accounts in the thread's runtime, and puts them back
    /// after. A range sees to its own failure, which its call keeps.
    fn resume_range(&mut self, task: TaskId, wake: Wake) {
        let Some(mut accounts) = self.accounts.remove(&task) else { return };
        super::threads::swap_accounts(&mut accounts);
        let step = self.with_own_temporaries(task, |io| io.ranges.resume(task, wake));
        super::threads::swap_accounts(&mut accounts);
        match step {
            Step::Parked(YIELD) => {
                self.accounts.insert(task, accounts);
                self.ready.push_back((task, Wake::Ready(READY)));
            }
            Step::Parked(token) => {
                self.accounts.insert(task, accounts);
                let token = usize::try_from(token).unwrap_or(usize::MAX);
                let (interest, deadline) = PARKING.take().unwrap_or((Interest::READABLE, None));
                self.waiting.insert(token, (task, interest, deadline));
            }
            Step::Done(_) | Step::Unknown => {
                debug_assert!(accounts.is_empty(), "a range ended with what it made not handed over");
            }
        }
    }

    /// Resumes `task` with its own temporaries and inputs in the runtime, keeping them apart again
    /// if it parks. What's left when it ends is let go of: native code that left a function by a
    /// failure may leave some.
    fn with_own_temporaries(&mut self, task: TaskId, resume: impl FnOnce(&mut Io) -> Step) -> Step {
        let mut own = self.temporaries.remove(&task).unwrap_or_default();
        super::swap_temporaries(&mut own);
        let step = resume(self);
        super::swap_temporaries(&mut own);
        if matches!(step, Step::Parked(_)) && !own.is_empty() {
            self.temporaries.insert(task, own);
        }
        step
    }

    /// A task's about to be resumed: its slice starts now, and whether anything else here wants the
    /// thread is noted, for `fennec_task_tick` to read without this.
    fn slice_starts(&self) {
        SLICE.set(Some(Instant::now()));
        OTHERS.set(!self.ready.is_empty() || !self.waiting.is_empty() || !self.servers.is_empty());
    }

    /// Makes the task parked on `token` ready, for a wait that finished on another thread.
    pub(in crate::embed) fn wake_token(&mut self, token: Token) {
        if let Some((task, ..)) = self.waiting.remove(&token.0) {
            self.ready.push_back((task, Wake::Ready(READY)));
        }
    }

    /// Notes that a task ended, failed or not, giving back the server it served a round of. A
    /// server that's draining is left once its last round here has ended.
    fn ended(&mut self, task: TaskId, failed: bool) -> Option<u64> {
        let id = self.owners.remove(&task)?;
        let here = self.servers.get_mut(&id)?;
        here.tasks.remove(&task);
        here.served.failed += u64::from(failed);
        if here.draining && here.tasks.is_empty() {
            self.leave(id);
        }
        Some(id)
    }

    /// Sends every worker, this one among them, `job` for when it next reads its mail.
    fn for_every_worker(&self, job: impl Fn(&mut Io) + Send + Sync + 'static) {
        let job = Arc::new(job);
        for at in 0..self.pool.threads() {
            let job = Arc::clone(&job);
            self.pool.run_on(at, Box::new(move |io: &mut Io| job(io)));
        }
    }

    /// Stops a server taking connections, after a round's `break`: its listener goes, and its
    /// rounds here finish, after which it's left.
    fn drain(&mut self, id: u64) {
        let Some(here) = self.servers.get_mut(&id) else { return };
        here.draining = true;
        if let Some(mut listener) = here.listener.take() {
            let _ = self.poll.registry().deregister(&mut listener);
        }
        if here.tasks.is_empty() {
            self.leave(id);
        }
    }

    /// How long until the soonest wait gives up.
    pub(in crate::embed) fn timeout(&self) -> Option<Duration> {
        let soonest = self.waiting.values().filter_map(|(_, _, deadline)| *deadline).min()?;
        Some(soonest.saturating_duration_since(Instant::now()))
    }

    /// Looks at the poll without waiting, when this worker serves anything.
    pub(in crate::embed) fn poll_now(&mut self) {
        if self.active() {
            self.poll(Some(Duration::ZERO));
        }
    }

    /// Waits on the poll up to `timeout`, or until its waker is woken: then accepts what's
    /// waiting on its listeners, and wakes the tasks whose sockets are ready or whose waits
    /// have timed out.
    pub(in crate::embed) fn poll(&mut self, timeout: Option<Duration>) {
        if self.poll.poll(&mut self.events, timeout).is_err() {
            // Interrupted, or the poll's gone wrong: either way, the next turn looks again.
            return;
        }
        let listening: Vec<u64> = self
            .servers
            .iter()
            .filter(|(_, here)| here.listener.is_some())
            .filter(|(_, here)| self.events.iter().any(|event| event.token() == here.server.token()))
            .map(|(id, _)| *id)
            .collect();
        for id in listening {
            self.accept(id);
        }
        self.wake();
    }

    /// Takes every connection waiting on the server `id`'s listener, handing them out in turn
    /// to the pool's workers, this one among them.
    fn accept(&mut self, id: u64) {
        let workers = self.pool.threads().max(1);
        loop {
            let Some(here) = self.servers.get(&id) else { return };
            let Some(Ok((stream, _))) = here.listener.as_ref().map(TcpListener::accept) else { return };
            let to = here.server.next.fetch_add(1, Ordering::Relaxed) % workers;
            if to == self.at {
                self.take(stream, id);
            } else {
                self.pool.run_on(to, Box::new(move |io: &mut Io| io.take(stream, id)));
            }
        }
    }

    /// Wakes the tasks whose sockets are ready for what they wait for, and those whose waits
    /// have timed out. An event for something else, like a socket ready to write to while its
    /// task waits to read, leaves the task parked: woken, it would only find it would block
    /// and wait again.
    fn wake(&mut self) {
        for event in &self.events {
            let token = event.token().0;
            let wanted = self.waiting.get(&token).is_some_and(|(_, interest, _)| {
                let gone = event.is_error() || event.is_read_closed() || event.is_write_closed();
                gone || (interest.is_readable() && event.is_readable())
                    || (interest.is_writable() && event.is_writable())
            });
            if wanted && let Some((task, ..)) = self.waiting.remove(&token) {
                self.ready.push_back((task, Wake::Ready(READY)));
            }
        }
        #[cfg(test)]
        if WAKE_ALL.get() {
            for (task, ..) in std::mem::take(&mut self.waiting).into_values() {
                self.ready.push_back((task, Wake::Ready(READY)));
            }
        }
        let now = Instant::now();
        let expired: Vec<usize> = self
            .waiting
            .iter()
            .filter(|(_, (_, _, deadline))| deadline.is_some_and(|deadline| deadline <= now))
            .map(|(token, _)| *token)
            .collect();
        for token in expired {
            if let Some((task, ..)) = self.waiting.remove(&token) {
                self.ready.push_back((task, Wake::Ready(TIMED_OUT)));
            }
        }
    }
}

/// Whether a wait here can park its task: it's a task on a worker, whose poll it can register with,
/// and the runtime's state isn't borrowed. Otherwise a wait blocks its thread.
pub(crate) fn can_park() -> bool {
    fennec_parked::stack_low().is_some()
        && !super::state_is_borrowed()
        && REGISTRY.with_borrow(Option::is_some)
        && super::parallel::here().is_some()
}

/// What waiting for a socket came to, for a wait that parked.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Parked {
    Ready,
    TimedOut,
    Cancelled,
}

/// Parks this task until the socket `fd` may be ready for `interest`, or `deadline` passes: it's
/// registered with the worker's poll for the wait, under a token of its own, and taken off after.
/// Only for a task that `can_park`.
#[cfg(unix)]
pub(crate) fn park_on_fd(fd: std::os::fd::RawFd, interest: Interest, deadline: Option<Instant>) -> Parked {
    if cut_short() {
        return Parked::Cancelled;
    }
    let token = next_token();
    let mut source = mio::unix::SourceFd(&fd);
    let registered = REGISTRY.with_borrow(|registry| {
        registry.as_ref().is_some_and(|registry| registry.register(&mut source, token, interest).is_ok())
    });
    if !registered {
        return Parked::Ready;
    }
    PARKING.set(Some((interest, deadline)));
    let woken = fennec_parked::park(u64::try_from(token.0).unwrap_or(0));
    REGISTRY.with_borrow(|registry| registry.as_ref().map(|registry| registry.deregister(&mut source)));
    parked_as(woken)
}

/// A token for a wait on this thread that no other has.
pub(crate) fn next_token() -> Token {
    let token = Token(NEXT_TOKEN.get());
    NEXT_TOKEN.set(token.0.wrapping_add(1) % CONNECTIONS);
    token
}

/// Parks this task on `token`, which nothing's registered under, until `wake_token` is sent it from
/// another thread, `deadline` passes, or the task's stopped: for a sleep, or for another task's end.
/// Only for a task that `can_park`.
pub(crate) fn park_on(token: Token, deadline: Option<Instant>) -> Parked {
    park_on_token(token, deadline, true)
}

/// `park_on`, which a stop cuts short only if `stoppable`. When it isn't, a stop wakes it, as
/// cancelled, without failing it: for a task that must wait for another to end all the same.
fn park_on_token(token: Token, deadline: Option<Instant>, stoppable: bool) -> Parked {
    if stoppable && cut_short() {
        return Parked::Cancelled;
    }
    PARKING.set(Some((Interest::READABLE, deadline)));
    let woken = fennec_parked::park(u64::try_from(token.0).unwrap_or(0));
    if stoppable {
        return parked_as(woken);
    }
    match woken {
        Some(Wake::Ready(TIMED_OUT)) => Parked::TimedOut,
        Some(Wake::Cancelled | Wake::Ready(STOPPED)) => Parked::Cancelled,
        _ => Parked::Ready,
    }
}

fn parked_as(woken: Option<Wake>) -> Parked {
    match woken {
        Some(Wake::Ready(TIMED_OUT)) => Parked::TimedOut,
        // A stop cuts the wait short, unless it's a cleanup's in its grace, which looks again.
        Some(Wake::Ready(STOPPED)) => {
            if cut_short() {
                Parked::Cancelled
            } else {
                Parked::Ready
            }
        }
        Some(Wake::Cancelled) => Parked::Cancelled,
        _ => Parked::Ready,
    }
}

/// Whether this is a launched task that's been stopped. If so, it fails, as a crash does, so it
/// ends with its locks put back and let go; that it was stopped keeps its end quiet.
fn cut_short() -> bool {
    let stopped = super::threads::stopped();
    if stopped {
        super::fail_with(fennec_syntax::Diagnostic::new(
            "this task was stopped",
            fennec_syntax::Span::default(),
        ));
    }
    stopped
}

mod launch;
#[cfg(test)]
pub(in crate::embed) use launch::launch_on;
pub(in crate::embed) use launch::take_failed_with;
pub use launch::{TaskBody, launch, launch_end, task_failed};

/// Work the operating system can't be asked to say is ready for, run off this task's thread,
/// a DNS lookup and a connect among them.
type OffThread = Box<dyn FnOnce() + Send>;

/// How many threads run blocking work for parked tasks. A few, as each holds only a lookup or a
/// connect, and more of them waiting at once wait their turn.
const OFF_THREADS: usize = 8;

/// The threads that run blocking work for parked tasks, started on first use.
fn off_threads() -> Option<&'static Mutex<std::sync::mpsc::Sender<OffThread>>> {
    static SENDER: std::sync::OnceLock<Option<Mutex<std::sync::mpsc::Sender<OffThread>>>> =
        std::sync::OnceLock::new();
    SENDER
        .get_or_init(|| {
            let (sender, receiver) = std::sync::mpsc::channel::<OffThread>();
            let receiver = Arc::new(Mutex::new(receiver));
            for at in 0..OFF_THREADS {
                let receiver = Arc::clone(&receiver);
                let started =
                    std::thread::Builder::new().name(format!("fennec-blocking-{at}")).spawn(move || {
                        loop {
                            let next = receiver.lock().unwrap_or_else(PoisonError::into_inner).recv();
                            let Ok(work) = next else { return };
                            work();
                        }
                    });
                if started.is_err() && at == 0 {
                    return None;
                }
            }
            Some(Mutex::new(sender))
        })
        .as_ref()
}

/// Runs `work`, which blocks its thread, so that this task parks rather than holding its worker:
/// on one of the blocking threads, which hands its result back and wakes the task through its
/// worker's mail. Anywhere a wait can't park, it runs here.
pub(crate) fn off_thread<R: Send + 'static>(work: impl FnOnce() -> R + Send + 'static) -> R {
    let (Some((pool, at)), Some(sender)) = (super::parallel::here(), off_threads()) else { return work() };
    if !can_park() {
        return work();
    }
    let token = Token(NEXT_TOKEN.get());
    NEXT_TOKEN.set(token.0.wrapping_add(1) % CONNECTIONS);
    let result: Arc<Mutex<Option<R>>> = Arc::new(Mutex::new(None));
    let given = Arc::clone(&result);
    let job: OffThread = Box::new(move || {
        let done = work();
        *given.lock().unwrap_or_else(PoisonError::into_inner) = Some(done);
        // The worker takes its mail only after this task has parked, on that same thread.
        pool.run_on(at, Box::new(move |io: &mut Io| io.wake_token(token)));
    });
    let sent = sender.lock().unwrap_or_else(PoisonError::into_inner).send(job);
    match sent {
        Ok(()) => {
            PARKING.set(Some((Interest::READABLE, None)));
            let _ = fennec_parked::park(u64::try_from(token.0).unwrap_or(0));
        }
        // No thread left to run it, so it runs here: its wake finds nothing parked.
        Err(std::sync::mpsc::SendError(job)) => job(),
    }
    loop {
        if let Some(done) = result.lock().unwrap_or_else(PoisonError::into_inner).take() {
            return done;
        }
        // Cancelled before it finished: the work runs to its end, and this waits for it.
        let _ = yield_task_always();
    }
}

/// How long a task runs before it makes way for others on its worker, at the next tick.
const TASK_SLICE: Duration = Duration::from_millis(1);

/// `fennec_task_tick()`: native code calls it every few thousand turns of a loop in code that runs as
/// a task. Once the task's run for its slice, and something else on its worker wants the thread, it
/// makes way: it parks as ready, and is resumed after the others. Gives back 1 when it parked, for
/// native code to read its stack limit again, and 0 otherwise. Outside a task, it's 0 at once.
///
/// This is fairness without interrupts: no signal, no preemption, no priorities. A task that's alone
/// on its worker never parks for it.
pub extern "C" fn task_tick() -> i64 {
    let Some(started) = SLICE.get() else { return 0 };
    if started.elapsed() < TASK_SLICE || !can_park() {
        return 0;
    }
    let others = OTHERS.get() || super::parallel::here().is_some_and(|(pool, at)| pool.has_work_for(at));
    if !others {
        return 0;
    }
    i64::from(yield_task_always())
}

/// Lets the other tasks on this thread run, if this is a task, and gives back whether it did.
pub(in crate::embed) fn yield_task_always() -> bool {
    fennec_parked::stack_low().is_some() && fennec_parked::park(YIELD).is_some()
}

/// Lets the other tasks on this thread run, when this is a task and the runtime's state isn't
/// borrowed: it's resumed after them. Gives back whether it did. Tests make ranges take turns
/// with it; a lock that's taken will wait this way too.
#[cfg(test)]
pub(in crate::embed) fn yield_task() -> bool {
    if fennec_parked::stack_low().is_none() || super::state_is_borrowed() {
        return false;
    }
    fennec_parked::park(YIELD).is_some()
}

/// A server that's running: stop it to have its workers cancel their tasks and finish.
pub struct Stopper {
    server: Arc<Server>,
    pool: &'static Pool,
}

impl Stopper {
    pub fn stop(&self) {
        let id = self.server.id;
        for at in 0..self.pool.threads() {
            self.pool.run_on(at, Box::new(move |io: &mut Io| io.leave(id)));
        }
    }
}

/// Serves `listener` on `pool`'s workers, running `handler` on each connection as a parked task,
/// while this thread waits. The workers' tasks can read what's `lent`. `started` is given a
/// `Stopper` once the server is running, and the server runs until it's stopped.
pub(super) fn serve(
    listener: TcpListener,
    pool: &'static Pool,
    lent: Option<&Arc<super::table::Table>>,
    handler: &Handler,
    started: impl FnOnce(Stopper),
) -> Served {
    let workers = pool.threads();
    let server = Arc::new(Server {
        id: NEXT_SERVER.fetch_add(1, Ordering::Relaxed),
        handler: Arc::clone(handler),
        lent: lent.cloned(),
        next: AtomicUsize::new(0),
        total: Mutex::new(Served::default()),
        left: Latch::of(workers),
    });
    // The worker that accepts joins last, so every other worker has joined before it hands one
    // a connection: each worker takes what it's given in turn.
    let mut listener = Some(listener);
    for at in (1..workers).chain([0]) {
        let (joining, given) = (Arc::clone(&server), if at == 0 { listener.take() } else { None });
        pool.run_on(at, Box::new(move |io: &mut Io| io.join(joining, given)));
    }
    started(Stopper { server: Arc::clone(&server), pool });
    server.left.wait();
    std::mem::take(&mut *server.total.lock().unwrap_or_else(PoisonError::into_inner))
}

mod calls;

pub(super) use calls::served_call;
pub use calls::{ConnBody, net_serve, net_serve_failure, stop_servers};

#[cfg(test)]
mod tests;
