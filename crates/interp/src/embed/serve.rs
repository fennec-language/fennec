//! Servers: `for conn in parallel(server)`, each connection a task that parks while it waits
//! on the network, so a few threads serve thousands of connections (decisions.md: "A task
//! waiting on the network is parked, not its thread").
//!
//! A server runs on I/O workers, one per core, the calling thread among them. Each has its own
//! runtime, its own `mio::Poll` and its own scheduler of parked tasks (`fennec_parked`). The
//! calling thread accepts connections and hands each to the next worker in turn, through the
//! worker's inbox and waker. A connection's task stays on the worker that took it, so what it
//! makes stays that worker's, as in `threads`.
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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use fennec_parked::{Scheduler, Step, TaskId, Wake};
use mio::net::{TcpListener, TcpStream};
use mio::{Events, Interest, Poll, Registry, Token, Waker};

/// A connection task's stack. Only the pages a task touches take memory.
const STACK: usize = 256 * 1024;

/// What a parked wait is woken with.
const READY: u64 = 0;
const TIMED_OUT: u64 = 1;

/// The listener's and the waker's tokens; connections count up from 0.
const LISTENER: Token = Token(usize::MAX - 1);
const WAKER: Token = Token(usize::MAX - 2);

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
        NEXT_TOKEN.set(token.0.wrapping_add(1) % WAKER.0);
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

/// Connections handed to a worker, and how to wake it to take them.
struct Inbox {
    new: Mutex<Vec<TcpStream>>,
    waker: Waker,
}

impl Inbox {
    fn give(&self, stream: TcpStream) {
        self.new.lock().unwrap_or_else(PoisonError::into_inner).push(stream);
        let _ = self.waker.wake();
    }

    fn take(&self) -> Vec<TcpStream> {
        std::mem::take(&mut *self.new.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// What a server's run came to.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Served {
    pub connections: u64,
    /// Connections whose handler gave back a failure.
    pub failed: u64,
}

/// One I/O worker: its poll, its tasks, and which task waits on which connection.
struct Worker {
    poll: Poll,
    inbox: Arc<Inbox>,
    scheduler: Scheduler,
    /// The task parked on each connection's token, and when its wait gives up.
    waiting: HashMap<usize, (TaskId, Interest, Option<Instant>)>,
    ready: VecDeque<(TaskId, Wake)>,
    served: Served,
}

/// A worker's poll and inbox, made before its thread starts so others can hand it connections.
struct Waiting {
    poll: Poll,
    inbox: Arc<Inbox>,
}

impl Waiting {
    fn new() -> std::io::Result<Waiting> {
        let poll = Poll::new()?;
        let waker = Waker::new(poll.registry(), WAKER)?;
        Ok(Waiting { poll, inbox: Arc::new(Inbox { new: Mutex::new(Vec::new()), waker }) })
    }
}

impl Worker {
    /// The worker, made on its own thread, since its tasks stay there.
    fn new(waiting: Waiting) -> Worker {
        Worker {
            poll: waiting.poll,
            inbox: waiting.inbox,
            scheduler: Scheduler::new(STACK),
            waiting: HashMap::new(),
            ready: VecDeque::new(),
            served: Served::default(),
        }
    }

    /// Makes a task for a connection, ready to start.
    fn take(&mut self, stream: TcpStream, handler: &Handler) {
        let handler = Arc::clone(handler);
        let made = self.scheduler.spawn(move || handler(Conn::new(stream)));
        if let Ok(task) = made {
            self.served.connections += 1;
            self.ready.push_back((task, Wake::Ready(READY)));
        }
    }

    /// Runs every task that's ready until each parks or ends.
    fn run_ready(&mut self) {
        while let Some((task, wake)) = self.ready.pop_front() {
            match self.scheduler.resume(task, wake) {
                Step::Parked(token) => {
                    let token = usize::try_from(token).unwrap_or(usize::MAX);
                    let (interest, deadline) = PARKING.take().unwrap_or((Interest::READABLE, None));
                    self.waiting.insert(token, (task, interest, deadline));
                }
                Step::Done(0) | Step::Unknown => {}
                Step::Done(_) => {
                    self.served.failed += 1;
                    // A failure ends only its connection: the runtime's record of it goes.
                    super::clear_failure();
                }
            }
        }
    }

    /// How long until the soonest wait gives up.
    fn timeout(&self) -> Option<Duration> {
        let soonest = self.waiting.values().filter_map(|(_, _, deadline)| *deadline).min()?;
        Some(soonest.saturating_duration_since(Instant::now()))
    }

    /// Wakes the tasks whose sockets are ready for what they wait for, and those whose waits
    /// have timed out. An event for something else, like a socket ready to write to while its
    /// task waits to read, leaves the task parked: woken, it would only find it would block
    /// and wait again.
    fn wake(&mut self, events: &Events) {
        for event in events {
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

    /// Runs this worker until `stop` is set, accepting from `listener` when it's given one and
    /// handing connections out in turn to `others` and itself.
    fn run(
        mut self,
        listener: Option<TcpListener>,
        others: &[Arc<Inbox>],
        handler: &Handler,
        stop: &AtomicBool,
    ) -> std::io::Result<Served> {
        REGISTRY.set(Some(self.poll.registry().try_clone()?));
        let mut listener = listener;
        if let Some(listener) = &mut listener {
            self.poll.registry().register(listener, LISTENER, Interest::READABLE)?;
        }
        let mut events = Events::with_capacity(1024);
        let mut turn = 0;
        while !stop.load(Ordering::Acquire) {
            for stream in self.inbox.take() {
                self.take(stream, handler);
            }
            self.run_ready();
            // What the tasks that just ran printed comes out before this worker waits.
            super::write_kept_output();
            let waited = self.poll.poll(&mut events, self.timeout());
            if let Err(error) = waited
                && error.kind() != ErrorKind::Interrupted
            {
                return Err(error);
            }
            if let Some(listener) = &listener
                && events.iter().any(|event| event.token() == LISTENER)
            {
                while let Ok((stream, _)) = listener.accept() {
                    turn = (turn + 1) % (others.len() + 1);
                    match others.get(turn) {
                        Some(inbox) => inbox.give(stream),
                        None => self.take(stream, handler),
                    }
                }
            }
            self.wake(&events);
        }
        // Stopping cancels what's left: each task is told, ends, and closes its connection.
        let served = std::mem::take(&mut self.served);
        self.waiting.clear();
        drop(self.scheduler);
        REGISTRY.set(None);
        Ok(served)
    }
}

/// A server that's running: stop it to have its workers cancel their tasks and finish.
pub struct Stopper {
    stop: Arc<AtomicBool>,
    wakers: Vec<Arc<Inbox>>,
}

impl Stopper {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        for inbox in &self.wakers {
            let _ = inbox.waker.wake();
        }
    }
}

/// Serves `listener` on `workers` I/O workers, the calling thread among them, running
/// `handler` on each connection as a parked task. Each other worker runs `setup` first, to
/// start a runtime of its own. `started` is given a `Stopper` once the server is running,
/// and the server runs until it's stopped.
#[cfg(not(feature = "lean"))]
pub fn serve(
    listener: TcpListener,
    workers: usize,
    setup: &Arc<dyn Fn() + Send + Sync>,
    handler: &Handler,
    started: impl FnOnce(Stopper),
) -> std::io::Result<Served> {
    let stop = Arc::new(AtomicBool::new(false));
    let here = Waiting::new()?;
    let mut others = Vec::new();
    for _ in 1..workers.max(1) {
        others.push(Waiting::new()?);
    }
    let inboxes: Vec<Arc<Inbox>> = others.iter().map(|worker| Arc::clone(&worker.inbox)).collect();
    let mut wakers = inboxes.clone();
    wakers.push(Arc::clone(&here.inbox));
    started(Stopper { stop: Arc::clone(&stop), wakers });
    let threads: Vec<_> = others
        .into_iter()
        .map(|waiting| {
            let (setup, handler, stop) = (Arc::clone(setup), Arc::clone(handler), Arc::clone(&stop));
            std::thread::spawn(move || {
                super::threads::take_index();
                setup();
                let served = Worker::new(waiting).run(None, &[], &handler, &stop);
                super::finish();
                served
            })
        })
        .collect();
    let mut served = Worker::new(here).run(Some(listener), &inboxes, handler, &stop)?;
    for thread in threads {
        if let Ok(Ok(theirs)) = thread.join() {
            served.connections += theirs.connections;
            served.failed += theirs.failed;
        }
    }
    Ok(served)
}

mod calls;

pub use calls::{ConnBody, conn_close, conn_peer, stop_servers};
#[cfg(not(feature = "lean"))]
pub use calls::{conn_read, conn_read_line, conn_write, net_serve};

#[cfg(test)]
mod tests;
