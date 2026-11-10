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
//! A wait that can't park, because it isn't in a task or the runtime's state is borrowed,
//! blocks its thread on a poll of its own instead. That keeps it correct but costs a thread, so
//! it's never silent: each one is counted for `FENNEC_COUNTS`, and a dev build says so once for
//! each place it happens.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{ErrorKind, Read as _, Write as _};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use fennec_parked::{Scheduler, Step, TaskId, Wake};
use mio::net::{TcpListener, TcpStream};
use mio::{Events, Interest, Poll, Registry, Token};

use super::parallel::{Latch, Pool};
use crate::stdlib::net::{LINGER, LINGER_QUIET, ServerSession, TlsServer};

/// A connection task's stack. Only the pages a task touches take memory.
const STACK: usize = 1024 * 1024;

/// A range's stack, when it runs as a parked task: as big as a worker's, since a range runs the
/// same native code. Only the pages it touches take memory, and a few are kept for the next.
const RANGE_STACK: usize = 64 << 20;
const RANGE_STACKS_KEPT: usize = 8;

/// The most ranges of `parallel` calls a worker runs at once. Each has a stack of its own, and past
/// a few thousand there's no room left for more, nor for the tasks they launch, which they wait for.
const MOST_RANGES: usize = 1024;

/// A range or task waiting for a stack: the work, its accounts, and what's told its number once it
/// starts.
struct Deferred {
    run: Box<dyn FnOnce() -> i64>,
    accounts: Box<super::threads::Accounts>,
    started: Option<Box<dyn FnOnce(TaskId)>>,
}

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
    /// The tokens of connections parked in a read with nothing of a next message in hand, on a
    /// server that stops when the program's asked to: what its drain wakes, to end them.
    static IDLE_READS: RefCell<HashSet<usize>> = RefCell::new(HashSet::new());
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
///
/// Tasks on its worker share it, one reading while another writes, so each part is borrowed only
/// for one step, and never across a park.
pub struct Conn {
    stream: RefCell<TcpStream>,
    token: Token,
    /// Bytes read past the end of the last line.
    ahead: RefCell<Vec<u8>>,
    /// Whether it's registered with this thread's poll.
    registered: bool,
    /// Whether a task is parked on it. The poll knows one task for each socket, so a second waits
    /// blocking its thread.
    parked: Cell<bool>,
    /// Its TLS, when its server has some.
    tls: RefCell<Option<Box<dyn ServerSession>>>,
    /// Whether its server's draining, when its server stops when the program's asked to: a read
    /// with nothing of a next message in hand then ends, as at the end of what was sent.
    draining: Option<Arc<AtomicBool>>,
    /// Whether anything's been read since it last wrote: a read before anything has is waiting
    /// for a next message, not partway through one.
    read_since_write: Cell<bool>,
}

/// How long a connection has to finish its TLS handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

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
        Conn {
            stream: RefCell::new(stream),
            token,
            ahead: RefCell::new(Vec::new()),
            registered,
            parked: Cell::new(false),
            tls: RefCell::new(None),
            draining: None,
            read_since_write: Cell::new(false),
        }
    }

    /// It, ending a read that's waiting for a next message once `draining` is set.
    fn ending_when(mut self, draining: Arc<AtomicBool>) -> Conn {
        self.draining = Some(draining);
        self
    }

    /// Whether its server's draining.
    pub fn draining(&self) -> bool {
        self.draining.as_ref().is_some_and(|draining| draining.load(Ordering::SeqCst))
    }

    /// Waits to read, or gives true when the read should end instead: when nothing's been read
    /// since it last wrote, and its server's draining. While such a read waits, its server's drain
    /// wakes it, so it ends at once rather than when the other side next sends.
    fn wait_to_read(&self, until: Until, call: &'static str) -> Result<bool, Failure> {
        let idle = self.draining.is_some() && !self.read_since_write.get() && self.ahead.borrow().is_empty();
        if !idle {
            self.wait(self.read_interest(), until, call)?;
            return Ok(false);
        }
        if self.draining() {
            return Ok(true);
        }
        IDLE_READS.with_borrow_mut(|idle| idle.insert(self.token.0));
        let waited = self.wait(self.read_interest(), until, call);
        IDLE_READS.with_borrow_mut(|idle| idle.remove(&self.token.0));
        // Woken, it reads again: what came may be the next message, which is read whole.
        waited.map(|()| false)
    }

    /// Does the server's half of a TLS handshake, parking while it waits.
    fn secure(&self, tls: &Arc<dyn TlsServer>) -> Result<(), Failure> {
        let mut session = tls.session().map_err(|_| Failure::Closed)?;
        let until = Until::after(Some(HANDSHAKE_TIMEOUT));
        loop {
            match session.handshake(&mut *self.stream.borrow_mut()) {
                Ok(_) => break,
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    let interest =
                        if session.wants_write() { Interest::WRITABLE } else { Interest::READABLE };
                    self.wait(interest, until, "read")?;
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(Failure::of(&error)),
            }
        }
        *self.tls.borrow_mut() = Some(session);
        Ok(())
    }

    /// Up to `count` more bytes onto `bytes`, through TLS if it has any.
    fn fill(&self, bytes: &mut Vec<u8>, count: usize) -> std::io::Result<usize> {
        let mut stream = self.stream.borrow_mut();
        let filled = match &mut *self.tls.borrow_mut() {
            Some(session) => session.read_onto(&mut *stream, bytes, count),
            None => read_onto(&mut stream, bytes, count),
        };
        if filled.as_ref().is_ok_and(|read| *read > 0) {
            self.read_since_write.set(true);
        }
        filled
    }

    /// What a read that would block is waiting for: TLS may have something to send first.
    fn read_interest(&self) -> Interest {
        if self.tls.borrow().as_ref().is_some_and(|session| session.wants_write()) {
            Interest::WRITABLE
        } else {
            Interest::READABLE
        }
    }

    /// Up to `count` bytes, empty at the end.
    pub fn read(&self, count: usize, timeout: Option<Duration>) -> Result<Vec<u8>, Failure> {
        let until = Until::after(timeout);
        {
            let mut ahead = self.ahead.borrow_mut();
            if !ahead.is_empty() {
                let taken = ahead.len().min(count);
                return Ok(ahead.drain(..taken).collect());
            }
        }
        loop {
            let mut bytes = Vec::new();
            match self.fill(&mut bytes, count) {
                Ok(_) => return Ok(bytes),
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    if self.wait_to_read(until, "read")? {
                        return Ok(Vec::new());
                    }
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(Failure::of(&error)),
            }
        }
    }

    /// The next line, without its line ending, or `None` at the end.
    pub fn read_line(&self, timeout: Option<Duration>) -> Result<Option<Vec<u8>>, Failure> {
        self.read_line_crlf(timeout, false)
    }

    /// `read_line`, which with `crlf` fails on a line ended by a bare `\n`.
    pub fn read_line_crlf(&self, timeout: Option<Duration>, crlf: bool) -> Result<Option<Vec<u8>>, Failure> {
        let most = crate::stdlib::net::MOST_LINE_BYTES;
        let until = Until::after(timeout);
        // Where in what's held the newline hasn't been looked for yet, so each byte is looked at
        // once however slowly the line comes.
        let mut looked = 0;
        loop {
            let found = {
                let ahead = self.ahead.borrow();
                let from = looked.min(ahead.len());
                looked = ahead.len();
                ahead
                    .get(from..)
                    .unwrap_or_default()
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map(|at| from + at)
            };
            if found.is_none() && self.ahead.borrow().len() > most {
                return Err(Failure::Other(format!("a line was longer than {most} bytes")));
            }
            if let Some(end) = found {
                let mut line: Vec<u8> = self.ahead.borrow_mut().drain(..=end).collect();
                if let Some(why) = crate::stdlib::net::line_refused(&line, crlf) {
                    return Err(Failure::Other(why));
                }
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(Some(line));
            }
            let mut ahead = self.ahead.take();
            let filled = self.fill(&mut ahead, READ_BYTES);
            *self.ahead.borrow_mut() = ahead;
            match filled {
                Ok(0) if self.ahead.borrow().is_empty() => return Ok(None),
                Ok(0) => {
                    // A last line with no `\n` loses a `\r` it ends with, as one with a `\n` does.
                    let mut line = self.ahead.take();
                    if line.last() == Some(&b'\r') {
                        line.pop();
                    }
                    return Ok(Some(line));
                }
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    if self.wait_to_read(until, "read_line")? {
                        return Ok(None);
                    }
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(Failure::of(&error)),
            }
        }
    }

    /// Ends the connection gracefully: says no more is coming, then reads and throws away what the
    /// other side still sends, for a while. Closing with what it sent unread would reset the
    /// connection, and the other side would lose what it hadn't read yet: the answer it was sent.
    pub fn linger(&self) {
        let _ = self.stream.borrow().shutdown(std::net::Shutdown::Write);
        let until = Until::after(Some(LINGER));
        let mut sink = [0_u8; READ_BYTES];
        loop {
            let read = self.stream.borrow_mut().read(&mut sink);
            match read {
                Ok(0) => return,
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    // A client that's gone quiet has nothing more on the way.
                    let quiet = Until::after(Some(LINGER_QUIET));
                    let soonest = match (quiet.deadline, until.deadline) {
                        (Some(quiet), Some(all)) if all < quiet => until,
                        _ => quiet,
                    };
                    if self.wait(Interest::READABLE, soonest, "read").is_err() {
                        return;
                    }
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
    }

    /// The other side's address.
    pub fn peer(&self) -> String {
        self.stream.borrow().peer_addr().map(|peer| peer.to_string()).unwrap_or_default()
    }

    /// Writes all of `data`.
    pub fn write(&self, data: &[u8]) -> Result<(), Failure> {
        self.write_within(data, None)
    }

    /// `write`, giving up once `timeout` passes, as for a client that never reads.
    pub fn write_within(&self, mut data: &[u8], timeout: Option<Duration>) -> Result<(), Failure> {
        let until = Until::after(timeout);
        self.read_since_write.set(false);
        if self.tls.borrow().is_some() {
            return self.write_securely(data, until);
        }
        while !data.is_empty() {
            let wrote = self.stream.borrow_mut().write(data);
            match wrote {
                Ok(0) => return Err(Failure::Closed),
                Ok(written) => data = data.get(written..).unwrap_or_default(),
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    self.wait(Interest::WRITABLE, until, "write")?;
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(Failure::of(&error)),
            }
        }
        Ok(())
    }

    /// Writes all of `data` through TLS, waiting whenever what it holds can't go yet.
    fn write_securely(&self, mut data: &[u8], until: Until) -> Result<(), Failure> {
        loop {
            let wrote = {
                let mut tls = self.tls.borrow_mut();
                let Some(session) = tls.as_mut() else { return Err(Failure::Closed) };
                session
                    .write_some(&mut *self.stream.borrow_mut(), data)
                    .map(|taken| (taken, session.wants_write()))
            };
            match wrote {
                Ok((taken, wants_write)) => {
                    data = data.get(taken..).unwrap_or_default();
                    if wants_write {
                        self.wait(Interest::WRITABLE, until, "write")?;
                    } else if data.is_empty() {
                        return Ok(());
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    self.wait(Interest::WRITABLE, until, "write")?;
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(Failure::of(&error)),
            }
        }
    }

    /// Waits until the socket may be ready for `interest`: parked, when this is a task on an
    /// I/O worker and the runtime's state isn't borrowed, and blocking the thread otherwise.
    fn wait(&self, interest: Interest, until: Until, call: &'static str) -> Result<(), Failure> {
        // A wait woken early, to find the socket still not ready, gives up once its time's up.
        if until.deadline.is_some_and(|deadline| deadline <= Instant::now()) {
            return Err(Failure::TimedOut(until.timeout.unwrap_or_default()));
        }
        // What a task has printed comes out before it waits, however long that is.
        super::write_kept_output();
        let waited = if self.registered && !super::state_is_borrowed() && !self.parked.get() {
            PARKING.set(Some((interest, until.deadline)));
            self.parked.set(true);
            let woken = fennec_parked::park(u64::try_from(self.token.0).unwrap_or(0));
            self.parked.set(false);
            match woken {
                Some(Wake::Ready(TIMED_OUT)) => Waited::TimedOut,
                Some(Wake::Cancelled) => Waited::Cancelled,
                // A stop cuts the wait short, as any other's, unless it's a cleanup's in its grace.
                Some(Wake::Ready(STOPPED)) if cut_short() => Waited::Cancelled,
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
    fn block(&self, interest: Interest, until: Until, call: &'static str) -> Result<Waited, Failure> {
        let timeout = until.left();
        BLOCKED_WAITS.fetch_add(1, Ordering::Relaxed);
        warn_once(call);
        let mut poll = Poll::new().map_err(|error| Failure::of(&error))?;
        let token = Token(0);
        if self.registered {
            REGISTRY.with_borrow(|registry| {
                registry.as_ref().map(|registry| registry.deregister(&mut *self.stream.borrow_mut()))
            });
        }
        poll.registry()
            .register(&mut *self.stream.borrow_mut(), token, interest)
            .map_err(|error| Failure::of(&error))?;
        let mut events = Events::with_capacity(1);
        let polled = poll.poll(&mut events, timeout);
        let _ = poll.registry().deregister(&mut *self.stream.borrow_mut());
        if self.registered {
            let both = Interest::READABLE | Interest::WRITABLE;
            REGISTRY.with_borrow(|registry| {
                registry
                    .as_ref()
                    .map(|registry| registry.register(&mut *self.stream.borrow_mut(), self.token, both))
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
                registry.as_ref().map(|registry| registry.deregister(self.stream.get_mut()))
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
             ran outside a task; a server loses its scalability this way\n"
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
    /// Taken back when the call serving ends, so that its table comes back whoever holds this.
    lent: Mutex<Option<Arc<super::table::Table>>>,
    /// Which worker the next connection goes to, in turn.
    next: AtomicUsize,
    /// What each worker served, added in as it leaves, and the count of those still serving.
    total: Mutex<Served>,
    left: Latch,
    /// In a recording or a replay, the call serving, which names each round by its connection.
    replay: Option<Arc<str>>,
    /// How many connections it's taken, to number the next round's name.
    accepted: AtomicU64,
    /// In a replay: what a recorded round runs, given the handle of its connection, which is
    /// never a real one, and how many rounds are still to end.
    replayed: Option<(Replayed, AtomicU64)>,
    /// The TLS each connection starts with, if it has any.
    tls: Option<Arc<dyn TlsServer>>,
    /// Whether it's draining because the program was asked to stop, when `stop_on` asked it to:
    /// how long its rounds then have to finish.
    stop: Option<Duration>,
    draining: Arc<AtomicBool>,
}

/// A recorded round, replayed: what it runs with the handle of its connection.
pub type Replayed = Arc<dyn Fn(i64) -> i64 + Send + Sync>;

/// Where a replayed round's connection handles start, above any real one's.
const REPLAYED_HANDLES: i64 = 1 << 40;

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
    /// When to try accepting again, after running out of something: sockets, say.
    accept_later: Option<Instant>,
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
    /// Ranges, and launched tasks, waiting for a stack, which they get as others end. Launched tasks
    /// go first: ranges may be waiting for them.
    deferred_ranges: VecDeque<Deferred>,
    deferred_tasks: VecDeque<Deferred>,
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
            accept_later: None,
            waiting: HashMap::new(),
            ready: VecDeque::new(),
            owners: HashMap::new(),
            servers: HashMap::new(),
            ranges: Scheduler::keeping(RANGE_STACK, RANGE_STACKS_KEPT),
            deferred_ranges: VecDeque::new(),
            deferred_tasks: VecDeque::new(),
            accounts: HashMap::new(),
            temporaries: HashMap::new(),
        }
    }

    /// Starts a range as a parked task on this worker, so it can park when it waits. If there's no
    /// stack for it, it runs here and now, as a range whose body can't wait does.
    pub(in crate::embed) fn start_range(&mut self, range: Box<dyn FnOnce() -> i64>) {
        let deferred = Deferred { run: range, accounts: Box::default(), started: None };
        if self.ranges.len() >= MOST_RANGES || !self.deferred_ranges.is_empty() {
            self.deferred_ranges.push_back(deferred);
            return;
        }
        if let Err(deferred) = self.try_start(deferred) {
            self.defer_or_run(deferred, false);
        }
    }

    /// Starts `run` as a parked task on this worker with `accounts` its own, telling `started` its
    /// number. With no stack for it now, it waits for one, which it gets as another task here ends.
    pub(in crate::embed) fn start_task(
        &mut self,
        run: Box<dyn FnOnce() -> i64>,
        accounts: super::threads::Accounts,
        started: Box<dyn FnOnce(TaskId)>,
    ) {
        if let Err(deferred) =
            self.try_start(Deferred { run, accounts: Box::new(accounts), started: Some(started) })
        {
            self.defer_or_run(deferred, true);
        }
    }

    /// Starts a task, or gives it back when there's no stack for it.
    fn try_start(&mut self, deferred: Deferred) -> Result<TaskId, Deferred> {
        let Deferred { run, accounts, started } = deferred;
        let mut accounts = *accounts;
        // The task runs it from its first resume, so it's kept here until then in case it can't.
        let run = Rc::new(Cell::new(Some(run)));
        let held = Rc::clone(&run);
        let Ok(task) = self.ranges.spawn(move || held.take().map_or(0, |run| run())) else {
            // A task that was never made never took its work, so it's still here.
            return match run.take() {
                Some(run) => Err(Deferred { run, accounts: Box::new(accounts), started }),
                None => Err(Deferred { run: Box::new(|| 0), accounts: Box::new(accounts), started }),
            };
        };
        accounts.set_task(task);
        // Named as a server's round is: a task's temporaries are swapped last, so they say who it is.
        if let Some(named) = accounts.take_replay() {
            self.temporaries.insert(task, super::Temporaries::of_task(named));
        }
        self.accounts.insert(task, accounts);
        self.ready.push_back((task, Wake::Ready(READY)));
        if let Some(started) = started {
            started(task);
        }
        Ok(task)
    }

    /// What has no stack waits for one while other tasks here can end and free theirs. With none,
    /// nothing would, so it runs here and now, as it always could.
    fn defer_or_run(&mut self, deferred: Deferred, launched: bool) {
        if !self.ranges.is_empty() {
            if launched {
                self.deferred_tasks.push_back(deferred);
            } else {
                self.deferred_ranges.push_back(deferred);
            }
            return;
        }
        let Deferred { run, mut accounts, .. } = deferred;
        super::threads::swap_accounts(&mut accounts);
        run();
        super::threads::swap_accounts(&mut accounts);
    }

    /// Starts what's waiting for a stack, now that a task here has ended: launched tasks first.
    fn start_deferred(&mut self) {
        while let Some(deferred) = self.deferred_tasks.pop_front() {
            if let Err(deferred) = self.try_start(deferred) {
                self.deferred_tasks.push_front(deferred);
                return;
            }
        }
        while self.ranges.len() < MOST_RANGES
            && let Some(deferred) = self.deferred_ranges.pop_front()
        {
            if let Err(deferred) = self.try_start(deferred) {
                self.deferred_ranges.push_front(deferred);
                return;
            }
        }
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

    /// What this worker's runtime holds, and the tasks and connections it keeps, for
    /// `FENNEC_LIVE_REPORT` (see `live`).
    pub(in crate::embed) fn publish_live(&self, now: bool) {
        super::live::publish(now, || {
            let mut counts = super::live::of_this_thread();
            counts.tasks = self.accounts.len()
                + self.temporaries.len()
                + self.waiting.len()
                + self.ready.len()
                + self.owners.len();
            counts.connections = calls::connections();
            counts
        });
    }

    /// Whether a task is ready to run.
    pub(in crate::embed) fn has_ready(&self) -> bool {
        !self.ready.is_empty()
    }

    /// Joins a server: its tasks can read what's lent, and the worker given its listener accepts.
    fn join(&mut self, server: Arc<Server>, mut listener: Option<TcpListener>) {
        super::threads::serving_starts(server.lent.lock().unwrap_or_else(PoisonError::into_inner).clone());
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
        let tls = here.server.tls.clone();
        let draining = here.server.stop.map(|_| Arc::clone(&here.server.draining));
        let made = self.scheduler.spawn(move || {
            let conn = match draining {
                Some(draining) => Conn::new(stream).ending_when(draining),
                None => Conn::new(stream),
            };
            // A connection that fails its handshake closes, and its round never runs.
            if let Some(tls) = &tls
                && conn.secure(tls).is_err()
            {
                return 0;
            }
            handler(conn)
        });
        if let Ok(task) = made {
            here.served.connections += 1;
            here.tasks.insert(task);
            self.owners.insert(task, id);
            // In a recording, the round is named by the order its connection was taken in.
            let named = here.server.replay.as_ref().map(|call| {
                let n = here.server.accepted.fetch_add(1, Ordering::Relaxed);
                crate::replay::log_at(call, "accept", n);
                crate::replay::task_named(&format!("{call}.{n}"))
            });
            // A round reads what its own server lent, whatever other server this worker serves.
            let lent = here.server.lent.lock().unwrap_or_else(PoisonError::into_inner).clone();
            self.temporaries.insert(task, super::Temporaries::of_round(named, lent));
            self.ready.push_back((task, Wake::Ready(READY)));
        }
    }

    /// Starts the `n`th of a replayed server's recorded rounds, as `take` starts a connection's,
    /// with a connection that's only a handle: what it read and wrote comes from the recording.
    fn take_recorded(&mut self, id: u64, n: u64) {
        let Some(here) = self.servers.get_mut(&id) else { return };
        let (Some(call), Some((replayed, _))) = (&here.server.replay, &here.server.replayed) else { return };
        let replayed = Arc::clone(replayed);
        let handle = REPLAYED_HANDLES + i64::try_from(n).unwrap_or(0);
        let named = crate::replay::task_named(&format!("{call}.{n}"));
        if let Ok(task) = self.scheduler.spawn(move || replayed(handle)) {
            here.served.connections += 1;
            here.tasks.insert(task);
            self.owners.insert(task, id);
            self.temporaries.insert(task, super::Temporaries::of_task(named));
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
                self.start_deferred();
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
        if !matches!(step, Step::Parked(_)) {
            super::take_failure_of(&mut own);
        }
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
        // A replayed server stops once its last recorded round has ended.
        if let Some((_, left)) = &here.server.replayed
            && left.fetch_sub(1, Ordering::AcqRel) == 1
        {
            Stopper { server: Arc::clone(&here.server), pool: self.pool }.stop();
        }
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
    /// Drains the server `id` because the program was asked to stop: its rounds waiting for a
    /// next message are woken, to end, before it drains as `break` does.
    fn drain_stopping(&mut self, id: u64) {
        if let Some(here) = self.servers.get(&id) {
            let idle: Vec<usize> = IDLE_READS.with_borrow(|idle| idle.iter().copied().collect());
            for token in idle {
                let parked = self.waiting.get(&token).map(|(task, ..)| *task);
                if let Some(task) = parked.filter(|task| here.tasks.contains(task)) {
                    self.waiting.remove(&token);
                    self.ready.push_back((task, Wake::Ready(READY)));
                }
            }
        }
        self.drain(id);
    }

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

    /// How long until the soonest wait gives up, or it's time to try accepting again.
    pub(in crate::embed) fn timeout(&self) -> Option<Duration> {
        let soonest =
            self.waiting.values().filter_map(|(_, _, deadline)| *deadline).chain(self.accept_later).min()?;
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
        // What was left waiting when accepting ran out of something won't announce itself again.
        if self.accept_later.is_some_and(|when| when <= Instant::now()) {
            self.accept_later = None;
            let every: Vec<u64> =
                self.servers.iter().filter(|(_, here)| here.listener.is_some()).map(|(id, _)| *id).collect();
            for id in every {
                self.accept(id);
            }
        }
        self.wake();
    }

    /// Takes every connection waiting on the server `id`'s listener, handing them out in turn
    /// to the pool's workers, this one among them.
    fn accept(&mut self, id: u64) {
        let workers = self.pool.threads().max(1);
        loop {
            let Some(here) = self.servers.get(&id) else { return };
            let stream = match here.listener.as_ref().map(TcpListener::accept) {
                Some(Ok((stream, _))) => stream,
                Some(Err(error)) => match accept_failed(&error) {
                    AfterAccept::Stop => return,
                    AfterAccept::Next => continue,
                    AfterAccept::Later => {
                        self.accept_later = Some(Instant::now() + ACCEPT_AGAIN);
                        return;
                    }
                },
                None => return,
            };
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

/// Waits for each worker to leave a server. A task parks meanwhile, looking again now and then, as
/// its own worker is one of those it waits for. Anything else blocks.
fn wait_for_every_worker(left: &Latch) {
    if !can_park() {
        left.wait();
        return;
    }
    while !left.is_done() {
        let _ = park_on_token(next_token(), Some(Instant::now() + SERVER_END_LOOKS), false);
    }
}

/// How often a task serving looks to see whether its server has ended.
const SERVER_END_LOOKS: Duration = Duration::from_millis(2);

/// How long to wait before accepting again, having run out of something.
const ACCEPT_AGAIN: Duration = Duration::from_millis(100);

/// What to do about a listener's failure to accept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AfterAccept {
    /// There's nothing more waiting, or it won't work: the poll says when there's more.
    Stop,
    /// That connection's gone, and those behind it are still there.
    Next,
    /// The process ran out of something. What's waiting stays waiting, and the poll won't announce
    /// it again, so it's looked at again soon.
    Later,
}

fn accept_failed(error: &std::io::Error) -> AfterAccept {
    match error.kind() {
        ErrorKind::Interrupted | ErrorKind::ConnectionAborted | ErrorKind::ConnectionReset => {
            AfterAccept::Next
        }
        ErrorKind::WouldBlock => AfterAccept::Stop,
        _ if matches!(error.raw_os_error(), Some(23 | 24)) => AfterAccept::Later,
        ErrorKind::OutOfMemory => AfterAccept::Later,
        _ => AfterAccept::Stop,
    }
}

/// Whether a wait here can park its task: it's a task on a worker, whose poll it can register with,
/// and the runtime's state isn't borrowed. Otherwise a wait blocks its thread.
pub(crate) fn can_park() -> bool {
    fennec_parked::stack_low().is_some()
        && !super::state_is_borrowed()
        && !super::threads::holds_locks()
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
    // What a task has printed comes out before it waits, however long that is.
    super::write_kept_output();
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

/// Fails the running task as one that was stopped.
pub(crate) fn fail_as_stopped() {
    super::fail_with(fennec_syntax::Diagnostic::new("this task was stopped", fennec_syntax::Span::default()));
}

/// Whether this is a launched task that's been stopped. If so, it fails, as a crash does, so it
/// ends with its locks put back and let go; that it was stopped keeps its end quiet.
fn cut_short() -> bool {
    let stopped = super::threads::stopped();
    if stopped {
        fail_as_stopped();
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
            super::write_kept_output();
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
/// Lets the other tasks on this thread run, giving back whether it did: for a task waiting for
/// something another task here holds.
pub(crate) fn yield_now() -> bool {
    can_park() && yield_task_always()
}

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
    /// Which server it stops.
    pub(super) fn id(&self) -> u64 {
        self.server.id
    }

    pub fn stop(&self) {
        let id = self.server.id;
        for at in 0..self.pool.threads() {
            self.pool.run_on(at, Box::new(move |io: &mut Io| io.leave(id)));
        }
    }

    /// Drains the server because the program was asked to stop, once: it takes no more
    /// connections, its rounds waiting for a next message end, and those still running after
    /// `grace` are cut short.
    fn drain_for(&self, grace: Duration) {
        if self.server.draining.swap(true, Ordering::SeqCst) {
            return;
        }
        let id = self.server.id;
        for at in 0..self.pool.threads() {
            self.pool.run_on(at, Box::new(move |io: &mut Io| io.drain_stopping(id)));
        }
        let stopping = Stopper { server: Arc::clone(&self.server), pool: self.pool };
        let _ = std::thread::Builder::new().name("fennec-grace".to_owned()).spawn(move || {
            std::thread::sleep(grace);
            // A server that's ended by then has nothing left to stop.
            stopping.stop();
        });
    }
}

/// What a server's listener came with: its TLS, and how long its rounds have to finish once the
/// program's asked to stop, when it drains then.
#[derive(Default)]
pub(crate) struct Listened {
    pub(crate) tls: Option<Arc<dyn TlsServer>>,
    pub(crate) stop: Option<Duration>,
}

/// Serves `listener` on `pool`'s workers, running `handler` on each connection as a parked task,
/// while this thread waits. The workers' tasks can read what's `lent`. `started` is given a
/// `Stopper` once the server is running, and the server runs until it's stopped.
pub(super) fn serve(
    listener: TcpListener,
    pool: &'static Pool,
    lent: Option<&Arc<super::table::Table>>,
    handler: &Handler,
    (started, replay, Listened { tls, stop }): (impl FnOnce(Stopper), Option<&crate::replay::Task>, Listened),
) -> Served {
    let workers = pool.threads();
    let server = Arc::new(Server {
        id: NEXT_SERVER.fetch_add(1, Ordering::Relaxed),
        handler: Arc::clone(handler),
        lent: Mutex::new(lent.cloned()),
        next: AtomicUsize::new(0),
        total: Mutex::new(Served::default()),
        left: Latch::of(workers),
        replay: replay.map(crate::replay::Task::name),
        accepted: AtomicU64::new(0),
        replayed: None,
        tls,
        stop,
        draining: Arc::new(AtomicBool::new(false)),
    });
    // The worker that accepts joins last, so every other worker has joined before it hands one
    // a connection: each worker takes what it's given in turn.
    let mut listener = Some(listener);
    for at in (1..workers).chain([0]) {
        let (joining, given) = (Arc::clone(&server), if at == 0 { listener.take() } else { None });
        pool.run_on(at, Box::new(move |io: &mut Io| io.join(joining, given)));
    }
    started(Stopper { server: Arc::clone(&server), pool });
    // A server that stops when the program's asked to drains then, and its rounds have `grace` to
    // finish before they're cut short.
    let listening = stop.map(|grace| {
        let stopping = Stopper { server: Arc::clone(&server), pool };
        crate::signals::on_stop(Box::new(move || stopping.drain_for(grace)))
    });
    wait_for_every_worker(&server.left);
    if let Some(listening) = listening {
        crate::signals::forget(listening);
    }
    server.lent.lock().unwrap_or_else(PoisonError::into_inner).take();
    std::mem::take(&mut *server.total.lock().unwrap_or_else(PoisonError::into_inner))
}

/// Replays a recorded server: its `rounds` recorded rounds run on `pool`'s workers as `serve` runs
/// connections, each with `replayed` and a connection that's only a handle, and when the last has
/// ended the server stops. Its call is `call`, which named its rounds.
pub(super) fn serve_replayed(
    rounds: u64,
    pool: &'static Pool,
    lent: Option<&Arc<super::table::Table>>,
    (replayed, call): (Replayed, &crate::replay::Task),
) -> Served {
    let workers = pool.threads();
    let unused: Handler = Arc::new(|_: Conn| 0);
    let server = Arc::new(Server {
        id: NEXT_SERVER.fetch_add(1, Ordering::Relaxed),
        handler: unused,
        lent: Mutex::new(lent.cloned()),
        next: AtomicUsize::new(0),
        total: Mutex::new(Served::default()),
        left: Latch::of(workers),
        replay: Some(call.name()),
        accepted: AtomicU64::new(0),
        replayed: Some((replayed, AtomicU64::new(rounds))),
        tls: None,
        stop: None,
        draining: Arc::new(AtomicBool::new(false)),
    });
    for at in 0..workers {
        let joining = Arc::clone(&server);
        pool.run_on(at, Box::new(move |io: &mut Io| io.join(joining, None)));
    }
    if rounds == 0 {
        Stopper { server: Arc::clone(&server), pool }.stop();
    }
    for n in 0..rounds {
        let id = server.id;
        let to = usize::try_from(n).unwrap_or(0) % workers.max(1);
        pool.run_on(to, Box::new(move |io: &mut Io| io.take_recorded(id, n)));
    }
    server.left.wait();
    server.lent.lock().unwrap_or_else(PoisonError::into_inner).take();
    std::mem::take(&mut *server.total.lock().unwrap_or_else(PoisonError::into_inner))
}

mod calls;

pub(super) use calls::served_call;
pub use calls::{ConnBody, net_serve, net_serve_failure, stop_servers};

#[cfg(test)]
mod tests;
