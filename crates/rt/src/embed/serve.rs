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
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
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

/// A launched task's stack. A server may launch one for each connection, and the kernel's tables
/// for a range's 64 MB cost 16 KB of each, for room a task seldom uses. Native code in one can
/// still nest about 150,000 calls of a small function, three quarters of this.
const LAUNCHED_STACK: usize = 8 << 20;

/// The most ranges of `parallel` calls a worker runs at once. Each has a stack of its own, and past
/// a few thousand there's no room left for more, nor for the tasks they launch, which they wait for.
/// Only ranges count: a launched task may live as long as the program, and a worker holding this
/// many of those would otherwise never start a range, with whatever waits for the range waiting
/// for good.
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
    /// On an I/O worker, its poll's registry, for connections to register with, and keep, so a
    /// task on another worker can tell its connection isn't its own.
    static REGISTRY: RefCell<Option<Arc<Registry>>> = const { RefCell::new(None) };
    /// The token the next connection on this thread gets.
    static NEXT_TOKEN: Cell<usize> = const { Cell::new(0) };
    /// What the wait a task is about to park on waits for, when it gives up, and the note it
    /// leaves if it's a stuck one, for its worker to read. Every park says all three, so none is
    /// ever left over from one the worker didn't take.
    static PARKING: RefCell<Option<Parking>> = const { RefCell::new(None) };
    /// For tests: wakes every parked task each time its worker polls, as a storm of readiness
    /// it doesn't wait for would, like a socket that keeps saying it's writable.
    #[cfg(test)]
    static WAKE_ALL: Cell<bool> = const { Cell::new(false) };
    /// Whether the wait about to park is a connection's read with nothing of a next message in hand:
    /// what a server's drain wakes, to end it, and what running out of files ends the longest
    /// parked of (`Io::end_longest_idle`). The park takes it (`parking`), as it takes `PARKING`.
    static PARKS_IDLE: Cell<bool> = const { Cell::new(false) };
    /// The tokens of idle connections ended to make room for one waiting to be accepted, when the
    /// process ran out of files: each one's read ends, as at the end of what was sent.
    static ENDED_IDLE: RefCell<HashSet<usize>> = RefCell::new(HashSet::new());
    /// When the task running on this worker was last resumed.
    static SLICE: Cell<Option<Instant>> = const { Cell::new(None) };
    /// The task this worker is running, for a server it starts to know where it was started.
    static RESUMED: Cell<Option<TaskId>> = const { Cell::new(None) };
    /// The servers each task on this worker serves, while it does: so a server it's a round of,
    /// left, stops them, and cancels the round once they've ended (see `Io::leave`).
    static SERVING: RefCell<HashMap<TaskId, Vec<Arc<Server>>>> = RefCell::new(HashMap::new());
    /// The rounds on this worker kept, to cancel once their own servers have ended: each stays in
    /// its wait until it's cancelled, whoever sees its server's end first.
    static KEPT: RefCell<HashSet<TaskId>> = RefCell::new(HashSet::new());
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

/// A wait that blocked its thread, as a blocking thread's on a lock another holds: counted with
/// the network's that do.
pub(crate) fn waited_blocked() {
    BLOCKED_WAITS.fetch_add(1, Ordering::Relaxed);
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
/// A `Connection` is `shared`, so tasks on any worker use it, one reading while another writes,
/// a hub on another worker writing to each member's say (#2065). So each part is locked only for
/// one step, and never across a park, and the stream's lock comes after the TLS session's.
pub struct Conn {
    stream: Mutex<TcpStream>,
    token: Token,
    /// Bytes read past the end of the last line.
    ahead: Mutex<Vec<u8>>,
    /// How many times bytes have been taken off the front of `ahead`: a reader that has looked
    /// through part of it for a newline looks again from the start once another has taken some.
    taken: AtomicU64,
    /// The registry of the worker whose poll it's registered with, when it is. Only a task on that
    /// worker parks on it: a wait on another thread blocks it, moving the socket off that poll and
    /// back as a second wait there does.
    home: Option<Arc<Registry>>,
    /// Whether a task is parked on it. The poll knows one task for each socket, so a second waits
    /// blocking its thread.
    parked: AtomicBool,
    /// Held by a wait that blocks its thread while it has the socket off its worker's poll, so two
    /// such waits, on two threads, take turns at it.
    blocking: Mutex<()>,
    /// Its TLS, when its server has some.
    tls: Mutex<Option<Box<dyn ServerSession>>>,
    /// Whether its server's draining, when its server stops when the program's asked to: a read
    /// with nothing of a next message in hand then ends, as at the end of what was sent.
    draining: Option<Arc<AtomicBool>>,
    /// Whether anything's been read since it last wrote: a read before anything has is waiting
    /// for a next message, not partway through one.
    read_since_write: AtomicBool,
    /// Whether it was ended, waiting for a next message, to make room for a new connection when
    /// the process ran out of files: it closes at once, as nothing's on the way to linger for, and
    /// its file is wanted now.
    made_room: AtomicBool,
    /// The label it prints as, given as its server took it.
    label: i64,
    /// When every read has to be done by, and how long that was, once `reads_within` is called.
    reads_by: Mutex<Option<(Instant, Duration)>>,
    /// Its share of the room every server's request bodies have, for the body it's reading or
    /// answering: given back as its next request starts, as it ends, or as it goes.
    pub(super) body_held: crate::stdlib::net::BodyHeld,
}

/// One of a connection's parts, locked for one step. Nothing panics while one is held, so a
/// poisoned lock's part is as good as any.
fn locked<T: ?Sized>(part: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    part.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How long a connection has to finish its TLS handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

impl Conn {
    /// Takes a connection, registering it with this thread's poll when this is an I/O worker.
    pub fn new(mut stream: TcpStream) -> Conn {
        crate::stdlib::net::bound_send_buffer(&stream);
        let token = Token(NEXT_TOKEN.get());
        NEXT_TOKEN.set(token.0.wrapping_add(1) % CONNECTIONS);
        let home = REGISTRY.with_borrow(|registry| {
            registry
                .as_ref()
                .filter(|registry| {
                    registry.register(&mut stream, token, Interest::READABLE | Interest::WRITABLE).is_ok()
                })
                .map(Arc::clone)
        });
        Conn {
            stream: Mutex::new(stream),
            token,
            ahead: Mutex::new(Vec::new()),
            taken: AtomicU64::new(0),
            home,
            parked: AtomicBool::new(false),
            blocking: Mutex::new(()),
            tls: Mutex::new(None),
            draining: None,
            read_since_write: AtomicBool::new(false),
            made_room: AtomicBool::new(false),
            label: 0,
            reads_by: Mutex::new(None),
            body_held: crate::stdlib::net::BodyHeld::default(),
        }
    }

    /// Whether this thread is the worker whose poll it's registered with, where a task waiting on it
    /// can park.
    fn is_home(&self) -> bool {
        self.home.as_ref().is_some_and(|home| {
            REGISTRY.with_borrow(|here| here.as_ref().is_some_and(|here| Arc::ptr_eq(here, home)))
        })
    }

    /// It, printing as `label`, which its server gave it as it took it.
    fn labelled(mut self, label: i64) -> Conn {
        self.label = label;
        self
    }

    /// The label it prints as.
    pub fn label(&self) -> i64 {
        self.label
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
    /// since it last wrote, and its server's draining, or it was ended to make room for another
    /// connection. While such a read waits, its server's drain, or running out of files, wakes it,
    /// so it ends at once rather than when the other side next sends.
    fn wait_to_read(&self, until: Until, call: &'static str) -> Result<bool, Failure> {
        let idle = !self.read_since_write.load(Ordering::Relaxed) && locked(&self.ahead).is_empty();
        if !idle {
            self.wait(self.read_interest(), until, call)?;
            return Ok(false);
        }
        if self.draining() || self.ended_idle() {
            return Ok(true);
        }
        PARKS_IDLE.set(true);
        let waited = self.wait(self.read_interest(), until, call);
        // A wait that blocked its thread, rather than parking, didn't take it.
        PARKS_IDLE.set(false);
        if self.ended_idle() {
            return Ok(true);
        }
        // Woken, it reads again: what came may be the next message, which is read whole.
        waited.map(|()| false)
    }

    /// Whether it was ended to make room for another connection, as it waited for a next message.
    /// Nearly always none has been, which is looked at first.
    fn ended_idle(&self) -> bool {
        let ended = !ENDED_IDLE.with_borrow(HashSet::is_empty)
            && self.is_home()
            && ENDED_IDLE.with_borrow_mut(|ended| ended.remove(&self.token.0));
        if ended {
            self.made_room.store(true, Ordering::Relaxed);
        }
        ended
    }

    /// Does the server's half of a TLS handshake, parking while it waits.
    fn secure(&self, tls: &Arc<dyn TlsServer>) -> Result<(), Failure> {
        let mut session = tls.session().map_err(|_| Failure::Closed)?;
        let until = Until::after(Some(HANDSHAKE_TIMEOUT));
        loop {
            let shaken = session.handshake(&mut *locked(&self.stream));
            match shaken {
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
        *locked(&self.tls) = Some(session);
        Ok(())
    }

    /// Up to `count` more bytes onto `bytes`, through TLS if it has any.
    fn fill(&self, bytes: &mut Vec<u8>, count: usize) -> std::io::Result<usize> {
        // The TLS session's lock first, as a write takes them.
        let mut tls = locked(&self.tls);
        let mut stream = locked(&self.stream);
        let filled = match &mut *tls {
            Some(session) => session.read_onto(&mut *stream, bytes, count),
            None => read_onto(&mut stream, bytes, count),
        };
        if filled.as_ref().is_ok_and(|read| *read > 0) {
            self.read_since_write.store(true, Ordering::Relaxed);
        }
        filled
    }

    /// What a read that would block is waiting for: TLS may have something to send first.
    fn read_interest(&self) -> Interest {
        if locked(&self.tls).as_ref().is_some_and(|session| session.wants_write()) {
            Interest::WRITABLE
        } else {
            Interest::READABLE
        }
    }

    /// Has every read from now on done within `after`, however long each is given: as a server
    /// starts on a next request, when the last one's body is let go of too.
    pub fn reads_within(&self, after: Duration) {
        *locked(&self.reads_by) = Some((Instant::now() + after, after));
        self.body_held.give_back();
    }

    /// `_body`: `size` bytes, a request's body, read into room made once at its size, its room
    /// among every server's bodies already taken (`_room_for_body`). Fewer come back when the
    /// client stopped sending before the end.
    pub fn body(&self, size: usize, timeout: Option<Duration>) -> Result<Vec<u8>, Failure> {
        let mut data = Vec::with_capacity(size);
        while data.len() < size {
            let more = self.read(size - data.len(), timeout)?;
            if more.is_empty() {
                break;
            }
            data.extend_from_slice(&more);
        }
        Ok(data)
    }

    /// How long a read given `timeout` waits: no longer than what's left before every read has to
    /// be done by.
    fn read_until(&self, timeout: Option<Duration>) -> Until {
        let reads_by = *locked(&self.reads_by);
        match reads_by {
            Some((by, whole)) if timeout.is_none_or(|timeout| Instant::now() + timeout > by) => {
                Until { timeout: Some(whole), deadline: Some(by) }
            }
            _ => Until::after(timeout),
        }
    }

    /// Up to `count` bytes, empty at the end.
    pub fn read(&self, count: usize, timeout: Option<Duration>) -> Result<Vec<u8>, Failure> {
        let until = self.read_until(timeout);
        loop {
            // Held for the whole step, so another reader's bytes come before or after these, whole.
            let mut ahead = locked(&self.ahead);
            if !ahead.is_empty() {
                let taken = ahead.len().min(count);
                self.taken.fetch_add(1, Ordering::Relaxed);
                return Ok(ahead.drain(..taken).collect());
            }
            let mut bytes = Vec::new();
            let filled = self.fill(&mut bytes, count);
            drop(ahead);
            match filled {
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
        let until = self.read_until(timeout);
        // Where in what's held the newline hasn't been looked for yet, so each byte is looked at
        // once however slowly the line comes: unless another reader has taken some since, when
        // what's held starts somewhere else.
        let (mut looked, mut taken_then) = (0, self.taken.load(Ordering::Relaxed));
        loop {
            // One lock for the whole step: finding the line and taking it, or reading more onto
            // what's held. Two readers on two workers each get whole lines, each line once.
            let mut ahead = locked(&self.ahead);
            let taken_now = self.taken.load(Ordering::Relaxed);
            if taken_now != taken_then {
                (looked, taken_then) = (0, taken_now);
            }
            let from = looked.min(ahead.len());
            looked = ahead.len();
            let found = ahead
                .get(from..)
                .unwrap_or_default()
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|at| from + at);
            if found.is_none() && ahead.len() > most {
                return Err(Failure::Other(crate::stdlib::net::line_too_long()));
            }
            if let Some(end) = found {
                let mut line: Vec<u8> = ahead.drain(..=end).collect();
                self.taken.fetch_add(1, Ordering::Relaxed);
                drop(ahead);
                if let Some(why) = crate::stdlib::net::line_refused(&line, crlf) {
                    return Err(Failure::Other(why));
                }
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(Some(line));
            }
            let filled = self.fill(&mut ahead, READ_BYTES);
            match filled {
                Ok(0) if ahead.is_empty() => return Ok(None),
                Ok(0) => {
                    // A last line with no `\n` loses a `\r` it ends with, as one with a `\n` does.
                    let mut line = std::mem::take(&mut *ahead);
                    self.taken.fetch_add(1, Ordering::Relaxed);
                    if line.last() == Some(&b'\r') {
                        line.pop();
                    }
                    return Ok(Some(line));
                }
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    drop(ahead);
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
        // Closed by its round, so its body's let go of before it waits out what's still coming.
        self.body_held.give_back();
        if self.made_room.load(Ordering::Relaxed) {
            return;
        }
        let _ = locked(&self.stream).shutdown(std::net::Shutdown::Write);
        let until = Until::after(Some(LINGER));
        let mut sink = [0_u8; READ_BYTES];
        loop {
            let read = locked(&self.stream).read(&mut sink);
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

    /// Ends the connection both ways at once, for a task on another worker than its round's
    /// closing it: lingering there would read, and throw away, what its round is reading. Its round
    /// then reads the end, and its client is told nothing more is coming.
    pub fn end(&self) {
        let _ = locked(&self.stream).shutdown(std::net::Shutdown::Both);
        self.body_held.give_back();
    }

    /// The other side's address.
    pub fn peer(&self) -> String {
        locked(&self.stream).peer_addr().map(|peer| peer.to_string()).unwrap_or_default()
    }

    /// Writes all of `data`.
    pub fn write(&self, data: &[u8]) -> Result<(), Failure> {
        self.write_within(data, None)
    }

    /// `write`, giving up once `timeout` passes, as for a client that never reads.
    pub fn write_within(&self, mut data: &[u8], timeout: Option<Duration>) -> Result<(), Failure> {
        crate::stdlib::net::wrote_to_a_connection();
        let until = Until::after(timeout);
        self.read_since_write.store(false, Ordering::Relaxed);
        if locked(&self.tls).is_some() {
            return self.write_securely(data, until);
        }
        while !data.is_empty() {
            let wrote = locked(&self.stream).write(crate::stdlib::net::piece(data));
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
                let mut tls = locked(&self.tls);
                let Some(session) = tls.as_mut() else { return Err(Failure::Closed) };
                session
                    .write_some(&mut *locked(&self.stream), data)
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
        let waited =
            if self.is_home() && !super::state_is_borrowed() && !self.parked.swap(true, Ordering::Acquire) {
                parking(interest, until.deadline, None);
                let woken = fennec_parked::park(u64::try_from(self.token.0).unwrap_or(0));
                self.parked.store(false, Ordering::Release);
                match woken {
                    Some(Wake::Ready(TIMED_OUT)) => Waited::TimedOut,
                    // A round cancelled in its connection's wait fails as stopped, as at any other wait,
                    // so it goes no further: a `NetError` it could catch let it go on (#1950).
                    Some(Wake::Cancelled) => {
                        if !super::failed() {
                            fail_as_stopped();
                        }
                        Waited::Cancelled
                    }
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
        // One such wait at a time has the socket off its worker's poll, from whichever thread.
        let _turn = locked(&self.blocking);
        if let Some(home) = &self.home {
            let _ = home.deregister(&mut *locked(&self.stream));
        }
        let registered = poll.registry().register(&mut *locked(&self.stream), token, interest);
        let polled = registered.and_then(|()| {
            let mut events = Events::with_capacity(1);
            poll.poll(&mut events, timeout).map(|()| events.is_empty())
        });
        let _ = poll.registry().deregister(&mut *locked(&self.stream));
        if let Some(home) = &self.home {
            let both = Interest::READABLE | Interest::WRITABLE;
            let _ = home.register(&mut *locked(&self.stream), self.token, both);
        }
        let quiet = polled.map_err(|error| Failure::of(&error))?;
        Ok(if quiet && timeout.is_some() { Waited::TimedOut } else { Waited::Ready })
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        if let Some(home) = &self.home {
            let _ = home.deregister(self.stream.get_mut().unwrap_or_else(PoisonError::into_inner));
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
    /// Whether it's being left, with rounds that serve servers of their own still to cancel once
    /// those have ended.
    leaving: bool,
}

/// What a pool worker serves: its poll, its tasks, which task waits on which connection, and the
/// servers it's part of. It's the worker's own, so its tasks and what they make stay on it.
pub(in crate::embed) struct Io {
    /// When to try accepting again, after running out of something: sockets, say.
    accept_later: Option<Instant>,
    /// How many connections' reads waiting for a next message have parked here, which numbers each
    /// in its `Waiting`: the lowest waiting has waited longest.
    idle_parks: u64,
    /// The worker last asked to end a connection waiting for a next message, when accepting ran out
    /// of files and this one had none (`end_idle_elsewhere`).
    ended_at: usize,
    poll: Poll,
    events: Events,
    pool: &'static Pool,
    at: usize,
    scheduler: Scheduler,
    /// The task parked on each connection's token, and when its wait gives up.
    waiting: HashMap<usize, Waiting>,
    /// How many of the waits in `waiting` are stuck ones, with a note for the deadlock check (see
    /// `deadlock`). A wait leaves `waiting` only through `unwait`, which keeps this right.
    stuck_waits: usize,
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
    /// How many ranges are running here as tasks, of the tasks in `ranges`: the rest are launched.
    live_ranges: usize,
    deferred_tasks: VecDeque<Deferred>,
    /// The runtime's temporaries and inputs each parked task holds, kept apart while it's parked:
    /// tasks take turns on the thread, so one's can't be left where another's truncating would
    /// cut them off, and an ended task's go with it.
    temporaries: HashMap<TaskId, super::Temporaries>,
    /// How many servers here are being left, with rounds kept: while none are, which is nearly
    /// always, resuming a task looks for no round to cancel.
    leaving: usize,
}

impl Io {
    /// A worker's, on its own thread, with the poll its waker was made with.
    pub(in crate::embed) fn new(poll: Poll, workers: &'static Pool, at: usize) -> Io {
        REGISTRY.set(poll.registry().try_clone().ok().map(Arc::new));
        Io {
            poll,
            events: Events::with_capacity(1024),
            pool: workers,
            at,
            scheduler: Scheduler::new(STACK),
            accept_later: None,
            idle_parks: 0,
            ended_at: at,
            waiting: HashMap::new(),
            stuck_waits: 0,
            ready: VecDeque::new(),
            owners: HashMap::new(),
            servers: HashMap::new(),
            ranges: Scheduler::keeping(RANGE_STACK, RANGE_STACKS_KEPT),
            deferred_ranges: VecDeque::new(),
            live_ranges: 0,
            deferred_tasks: VecDeque::new(),
            accounts: HashMap::new(),
            temporaries: HashMap::new(),
            leaving: 0,
        }
    }

    /// Starts a range as a parked task on this worker, so it can park when it waits. If there's no
    /// stack for it, it runs here and now, as a range whose body can't wait does.
    pub(in crate::embed) fn start_range(&mut self, range: Box<dyn FnOnce() -> i64>) {
        let accounts = Box::new(super::threads::Accounts::for_range());
        let deferred = Deferred { run: range, accounts, started: None };
        if self.live_ranges >= MOST_RANGES || !self.deferred_ranges.is_empty() {
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
        let stack = if accounts.is_range_s() { RANGE_STACK } else { LAUNCHED_STACK };
        let Ok(task) = self.ranges.spawn_with_stack(stack, move || held.take().map_or(0, |run| run())) else {
            // A task that was never made never took its work, so it's still here.
            return match run.take() {
                Some(run) => Err(Deferred { run, accounts: Box::new(accounts), started }),
                None => Err(Deferred { run: Box::new(|| 0), accounts: Box::new(accounts), started }),
            };
        };
        // Its mark is its number's, or the next free one's if a task still alive here has that
        // mark from the last time the marks came round.
        let alive = &self.accounts;
        let number =
            super::heap::number_to_mark(task, |number| alive.values().any(|other| other.marked_as(number)));
        accounts.set_task(number);
        self.live_ranges += usize::from(accounts.is_range_s());
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
        while self.live_ranges < MOST_RANGES
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
            self.unwait(token);
            self.ready.push_back((task, Wake::Ready(STOPPED)));
        }
    }

    /// Cuts short the waits of the ranges parked here that are the task's told to stop with `stop`:
    /// the ranges of a `parallel` call it made, which stop as it does. In the order they started,
    /// so they're woken the same way each time.
    pub(in crate::embed) fn cancel_ranges_of(&mut self, stop: &Arc<AtomicU8>) {
        let mut ranges: Vec<TaskId> = self
            .accounts
            .iter()
            .filter(|(_, accounts)| accounts.is_range_stopped_with(stop))
            .map(|(task, _)| *task)
            .collect();
        ranges.sort_unstable();
        for range in ranges {
            self.cancel(range);
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
            counts.task_held =
                self.accounts.values().map(super::threads::Accounts::listed).max().unwrap_or(0);
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
        // Closed before this worker joined, as a close before serving started is: it drains now.
        let closed = server.draining.load(Ordering::SeqCst);
        let here = Here {
            server,
            listener,
            tasks: HashSet::new(),
            served: Served::default(),
            draining: false,
            leaving: false,
        };
        self.servers.insert(id, here);
        if closed {
            self.drain_waking(id);
        }
    }

    /// Leaves a server: its tasks here are cancelled, each told, ending and closing its
    /// connection, and what this worker served goes into the server's total.
    fn leave(&mut self, id: u64) {
        // A round that serves a server of its own isn't cancelled yet: that server's stopped, and
        // the round is cancelled once it's ended (`cut_round_once_its_server_has`). Cancelled
        // first, the round waited here, holding this worker, for every worker to leave its server,
        // which this one, and any whose round waited for this one's, couldn't (#2004).
        let Some(here) = self.servers.get_mut(&id) else { return };
        let mut serving: Vec<TaskId> = here.tasks.iter().copied().filter(|task| serves(*task)).collect();
        if !serving.is_empty() {
            serving.sort_unstable();
            if !here.leaving {
                here.leaving = true;
                self.leaving += 1;
            }
            KEPT.with_borrow_mut(|kept| kept.extend(serving.iter().copied()));
            if let Some(listener) = &mut here.listener {
                let _ = self.poll.registry().deregister(listener);
            }
            let others: Vec<TaskId> =
                here.tasks.iter().copied().filter(|task| !serving.contains(task)).collect();
            for task in &serving {
                let servers = SERVING.with_borrow(|serving| serving.get(task).cloned().unwrap_or_default());
                for server in servers {
                    Stopper { server, pool: self.pool }.stop();
                }
            }
            self.cut_rounds(id, others);
            return;
        }
        let Some(mut here) = self.servers.remove(&id) else { return };
        if here.leaving {
            self.leaving -= 1;
        }
        let left: Vec<usize> = self
            .waiting
            .iter()
            .filter(|(_, (task, ..))| here.tasks.contains(task))
            .map(|(token, _)| *token)
            .collect();
        for token in left {
            self.unwait(token);
        }
        self.ready.retain(|(task, _)| !here.tasks.contains(task));
        // In the order the tasks started, not the set's, so their cleanups run the same way each
        // time.
        let mut tasks: Vec<TaskId> = here.tasks.drain().collect();
        tasks.sort_unstable();
        for task in tasks {
            self.owners.remove(&task);
            let _ = self.with_own_temporaries(task, |io| io.scheduler.cancel(task));
            self.temporaries.remove(&task);
            // A round cut short ends quietly, however it ended.
            super::clear_failure();
        }
        if let Some(listener) = &mut here.listener {
            let _ = self.poll.registry().deregister(listener);
        }
        // With no server left here, no round is: what this worker still holds that no task made
        // was left by one. It's looked at now, while what the server lent can still be read, and
        // whatever else is parked here: a launched task or a range that can wait keeps its own
        // blocks apart, so a server run from inside a task is looked at as one run from `main` is.
        if self.servers.is_empty() {
            super::aot::worker_stopped_serving();
        }
        let (owners, _) = super::threads::serving_ends();
        here.served.owners.extend(owners);
        here.server.total.lock().unwrap_or_else(PoisonError::into_inner).add(here.served);
        here.server.left.finished();
    }

    /// Cancels `tasks`, rounds of the server `id` here, in the order they started, so their
    /// cleanups run the same way each time. Each ends quietly, however it ended.
    fn cut_rounds(&mut self, id: u64, mut tasks: Vec<TaskId>) {
        tasks.sort_unstable();
        let left: Vec<usize> = self
            .waiting
            .iter()
            .filter(|(_, (task, ..))| tasks.contains(task))
            .map(|(token, _)| *token)
            .collect();
        for token in left {
            self.unwait(token);
        }
        self.ready.retain(|(task, _)| !tasks.contains(task));
        for task in tasks {
            if let Some(here) = self.servers.get_mut(&id) {
                here.tasks.remove(&task);
            }
            self.owners.remove(&task);
            let _ = self.with_own_temporaries(task, |io| io.scheduler.cancel(task));
            KEPT.with_borrow_mut(|kept| kept.remove(&task));
            self.temporaries.remove(&task);
            super::clear_failure();
        }
    }

    /// A round of a server being left, which served a server of its own, is cancelled once that
    /// server has ended, and the server it's a round of is left once its last such round is: gives
    /// back whether it was.
    fn cut_round_once_its_server_has(&mut self, task: TaskId) -> bool {
        let Some(&id) = self.owners.get(&task) else { return false };
        let leaving = self.servers.get(&id).is_some_and(|here| here.leaving);
        if !leaving || serves(task) {
            return false;
        }
        self.cut_rounds(id, vec![task]);
        if self.servers.get(&id).is_some_and(|here| here.tasks.is_empty()) {
            self.leave(id);
        }
        true
    }

    /// For a test: a pause before resuming a round of a server being left, on the pool that
    /// `PAUSE_BEFORE_KEPT` names, as if the thread were taken away between this worker's look at
    /// whether the round's own server has ended and the round's.
    #[cfg(test)]
    fn pause_for_a_test(&self, task: TaskId) {
        let this_pool = std::ptr::from_ref(self.pool).addr();
        if PAUSE_BEFORE_KEPT.load(Ordering::Acquire) == this_pool
            && self.owners.get(&task).is_some_and(|id| self.servers.get(id).is_some_and(|here| here.leaving))
        {
            std::thread::sleep(Duration::from_micros(300));
        }
    }

    /// Makes a task for a connection of the server `id`, ready to start, printing as `label`.
    fn take(&mut self, stream: TcpStream, id: u64, label: i64) {
        // Each write goes out at once. A response is its head and then its body, two writes, and
        // waiting to send the second until the first is acknowledged, which a client delays by up
        // to 40 ms on Linux, held every answer with a body that long. If it can't be set, the
        // connection only answers more slowly.
        let _ = stream.set_nodelay(true);
        let Some(here) = self.servers.get_mut(&id) else { return };
        // A server being left takes no more rounds: dropping the stream closes it.
        if here.leaving {
            return;
        }
        let handler = Arc::clone(&here.server.handler);
        let tls = here.server.tls.clone();
        let draining = here.server.stop.map(|_| Arc::clone(&here.server.draining));
        // A round is a context for the deadlock check until it ends, or is cancelled and let go.
        let context = super::deadlock::Context::began();
        let made = self.scheduler.spawn(move || {
            let _context = context;
            let conn = match draining {
                Some(draining) => Conn::new(stream).ending_when(draining),
                None => Conn::new(stream),
            }
            .labelled(label);
            // A connection that fails its handshake closes, and its round never runs.
            if let Some(tls) = &tls
                && conn.secure(tls).is_err()
            {
                return 0;
            }
            let ended = handler(conn);
            #[cfg(feature = "audit")]
            super::aot::round_ended();
            ended
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
        let context = super::deadlock::Context::began();
        if let Ok(task) = self.scheduler.spawn(move || {
            let _context = context;
            replayed(handle)
        }) {
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
            if self.leaving > 0 && self.cut_round_once_its_server_has(task) {
                continue;
            }
            #[cfg(test)]
            self.pause_for_a_test(task);
            self.slice_starts();
            if self.accounts.contains_key(&task) {
                self.resume_range(task, wake);
                continue;
            }
            match self.with_own_temporaries(task, |io| io.scheduler.resume(task, wake)) {
                Step::Parked(YIELD) => self.ready.push_back((task, Wake::Ready(READY))),
                Step::Parked(token) => self.parked(task, token),
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

    /// `task` has parked on `token`: its wait goes in `waiting`, with what it said it waits for and,
    /// for a stuck one, its note.
    fn parked(&mut self, task: TaskId, token: u64) {
        let token = usize::try_from(token).unwrap_or(usize::MAX);
        let Parking { interest, deadline, stuck, idle } = PARKING.take().unwrap_or(Parking {
            interest: Interest::READABLE,
            deadline: None,
            stuck: None,
            idle: false,
        });
        let idle = idle.then(|| {
            self.idle_parks += 1;
            self.idle_parks
        });
        self.stuck_waits += usize::from(stuck.is_some());
        let stuck = stuck.map(|mut note| {
            note.parked_by(std::ptr::from_ref(self.pool).addr(), self.at, task);
            note
        });
        if let Some(was) = self.waiting.insert(token, (task, interest, deadline, stuck, idle)) {
            self.stuck_waits -= usize::from(was.3.is_some());
        }
    }

    /// Takes the wait on `token` out of `waiting`, giving what it was. Every wait leaves `waiting`
    /// through here, so `stuck_waits` stays right.
    fn unwait(&mut self, token: usize) -> Option<Waiting> {
        unwait(&mut self.waiting, &mut self.stuck_waits, token)
    }

    /// For the deadlock check as this worker goes to sleep: how many stuck waits it has, when
    /// nothing else is left here (nothing ready or put off, nothing served, and no other wait).
    pub(in crate::embed) fn idle_stuck(&self) -> Option<u64> {
        let idle = self.ready.is_empty()
            && self.servers.is_empty()
            && self.deferred_ranges.is_empty()
            && self.deferred_tasks.is_empty()
            && self.stuck_waits == self.waiting.len();
        idle.then(|| u64::try_from(self.stuck_waits).unwrap_or(u64::MAX))
    }

    /// The notes of its stuck waits, for the deadlock check, which copies them only when it needs
    /// them.
    pub(in crate::embed) fn stuck_notes(&self) -> impl Iterator<Item = super::deadlock::Note> + '_ {
        self.waiting.values().filter_map(|waiting| waiting.3.clone())
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
                self.parked(task, token);
            }
            Step::Done(_) | Step::Unknown => {
                debug_assert!(accounts.is_empty(), "a range ended with what it made not handed over");
                self.live_ranges -= usize::from(accounts.is_range_s()).min(self.live_ranges);
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
        let outer = RESUMED.replace(Some(task));
        let step = resume(self);
        RESUMED.set(outer);
        // In a recording or a replay, what the task printed goes out under its own name before its
        // name is put away: a task that makes way at a tick, or ends, parks without writing it, and
        // written after, with no task running here, it was logged as `main`'s, so a replay could
        // put it among `main`'s own lines or tear a line in two (#2033).
        if crate::replay::active() {
            super::write_kept_output();
        }
        super::swap_temporaries(&mut own);
        if !matches!(step, Step::Parked(_)) {
            // A replayed round its recording says a stop or a cancel cut short ends quietly, as a
            // cancelled round does in the run: its server's end clears what it failed with (#1918).
            if own.replay.as_ref().is_some_and(crate::replay::Task::was_stopped) {
                own.failure = None;
            }
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
        if let Some((task, ..)) = self.unwait(token.0) {
            self.ready.push_back((task, Wake::Ready(READY)));
        }
    }

    /// Notes that a task ended, failed or not, giving back the server it served a round of. A
    /// server that's draining is left once its last round here has ended.
    fn ended(&mut self, task: TaskId, failed: bool) -> Option<u64> {
        KEPT.with_borrow_mut(|kept| kept.remove(&task));
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
        if (here.draining || here.leaving) && here.tasks.is_empty() {
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
        super::live::stop_taken();
        self.drain_waking(id);
    }

    /// Drains the server `id`, first waking its rounds here that wait for a next message, to end:
    /// what a stop the program was asked for does, and what closing the server does.
    fn drain_waking(&mut self, id: u64) {
        let mut woken = Vec::new();
        // Only a server that stops when the program's asked to has its reads end as it drains.
        if let Some(here) = self.servers.get(&id).filter(|here| here.server.stop.is_some()) {
            woken = self
                .waiting
                .iter()
                .filter(|(_, (task, ..))| here.tasks.contains(task))
                .filter_map(|(token, (task, .., idle))| idle.map(|order| (order, *token, *task)))
                .collect();
            // In the order they parked, not the map's.
            woken.sort_unstable();
        }
        for (_, token, task) in woken {
            self.unwait(token);
            self.ready.push_back((task, Wake::Ready(READY)));
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
        let soonest = self
            .waiting
            .values()
            .filter_map(|(_, _, deadline, ..)| *deadline)
            .chain(self.accept_later)
            .min()?;
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

    /// Ends the connection here that's waited longest for its next message, its read ending as at
    /// the end of what was sent, to make room for one waiting to be accepted: the process ran out of
    /// files. Gives whether there was one.
    fn end_longest_idle(&mut self) -> bool {
        let longest =
            self.waiting.iter().filter_map(|(token, (.., idle))| idle.map(|order| (order, *token))).min();
        let Some((_, token)) = longest else { return false };
        ENDED_IDLE.with_borrow_mut(|ended| ended.insert(token));
        if let Some((task, ..)) = self.unwait(token) {
            self.ready.push_back((task, Wake::Ready(READY)));
        }
        true
    }

    /// Asks the next worker in turn but this one to end its connection that's waited longest for its
    /// next message (`end_longest_idle`): this one had none, and every worker serves the server's
    /// connections in turn.
    fn end_idle_elsewhere(&mut self) {
        let workers = self.pool.threads();
        if workers < 2 {
            return;
        }
        self.ended_at = (self.ended_at + 1) % workers;
        if self.ended_at == self.at {
            self.ended_at = (self.ended_at + 1) % workers;
        }
        self.pool.run_on(
            self.ended_at,
            Box::new(|io: &mut Io| {
                io.end_longest_idle();
            }),
        );
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
                    AfterAccept::EndIdle => {
                        // Accepted again once the connection ended here has closed, as the next
                        // turn runs its round to its end; one another worker ends, a moment later.
                        let again = if self.end_longest_idle() {
                            Duration::ZERO
                        } else {
                            self.end_idle_elsewhere();
                            ENDED_ELSEWHERE
                        };
                        self.accept_later = Some(Instant::now() + again);
                        return;
                    }
                },
                None => return,
            };
            // Its label is taken here, where connections are accepted one after another, so they're
            // numbered in the order they came, not the order the workers handed them start them.
            let label = crate::stdlib::net::next_label(0);
            let to = here.server.next.fetch_add(1, Ordering::Relaxed) % workers;
            if to == self.at {
                self.take(stream, id, label);
            } else {
                self.pool.run_on(to, Box::new(move |io: &mut Io| io.take(stream, id, label)));
            }
        }
    }

    /// Wakes the tasks whose sockets are ready for what they wait for, and those whose waits
    /// have timed out. An event for something else, like a socket ready to write to while its
    /// task waits to read, leaves the task parked: woken, it would only find it would block
    /// and wait again.
    fn wake(&mut self) {
        let Io { events, waiting, stuck_waits, ready, .. } = self;
        for event in &*events {
            let token = event.token().0;
            let wanted = waiting.get(&token).is_some_and(|(_, interest, ..)| {
                let gone = event.is_error() || event.is_read_closed() || event.is_write_closed();
                gone || (interest.is_readable() && event.is_readable())
                    || (interest.is_writable() && event.is_writable())
            });
            if wanted && let Some((task, ..)) = unwait(waiting, stuck_waits, token) {
                ready.push_back((task, Wake::Ready(READY)));
            }
        }
        #[cfg(test)]
        if WAKE_ALL.get() {
            let tokens: Vec<usize> = self.waiting.keys().copied().collect();
            for token in tokens {
                if let Some((task, ..)) = self.unwait(token) {
                    self.ready.push_back((task, Wake::Ready(READY)));
                }
            }
        }
        for token in timed_out(&self.waiting, Instant::now()) {
            if let Some((task, ..)) = self.unwait(token) {
                self.ready.push_back((task, Wake::Ready(TIMED_OUT)));
            }
        }
    }
}

/// A parked task's wait: the task, what it waits for, when it gives up, and, for a stuck wait, its
/// note for the deadlock check.
/// A parked task, what it waits for, when it gives up, its note if it's a stuck wait, and, for a
/// connection's read waiting for a next message, its place in the order those parked here.
type Waiting = (TaskId, Interest, Option<Instant>, Option<super::deadlock::Note>, Option<u64>);

/// `Io::unwait`, on its fields.
fn unwait(waiting: &mut HashMap<usize, Waiting>, stuck_waits: &mut usize, token: usize) -> Option<Waiting> {
    let waited = waiting.remove(&token)?;
    *stuck_waits -= usize::from(waited.3.is_some());
    Some(waited)
}

/// The waits whose deadlines have passed by `now`, soonest first and, at the same deadline, in the
/// order they parked, rather than the map's: tasks sleeping as long wake in the order they slept.
fn timed_out(waiting: &HashMap<usize, Waiting>, now: Instant) -> Vec<usize> {
    let mut due: Vec<(Instant, usize)> = waiting
        .iter()
        .filter_map(|(token, (_, _, deadline, ..))| {
            deadline.filter(|deadline| *deadline <= now).map(|at| (at, *token))
        })
        .collect();
    due.sort_unstable();
    due.into_iter().map(|(_, token)| token).collect()
}

/// Waits for each worker to leave a server. A task parks meanwhile, looking again now and then, as
/// its own worker is one of those it waits for. Anything else blocks.
///
/// A task told to stop while it serves stops its server with `stop`, once: its rounds are cut
/// short and the workers leave. It waits for them all the same, as its rounds read what it lent,
/// and nothing else would ever end the wait: a server only stops when it's told to.
fn wait_for_every_worker(left: &Latch, stop: impl Fn()) {
    if !can_park() {
        left.wait();
        return;
    }
    let mut stopped = false;
    while !left.is_done() {
        let _ = park_on_token(next_token(), Some(Instant::now() + SERVER_END_LOOKS), false, None);
        // A round cancelled while it serves stops its server too, as a task stopped does. It can't
        // park, so it waits for the other workers to leave the server a moment at a time.
        let cancelled = fennec_parked::is_cancelled();
        if !stopped && (super::threads::stopped() || cancelled) {
            stopped = true;
            stop();
        }
        if cancelled && !left.is_done() {
            std::thread::sleep(SERVER_END_LOOKS);
        }
    }
}

/// How often a task serving looks to see whether its server has ended.
const SERVER_END_LOOKS: Duration = Duration::from_millis(2);

/// How long to wait before accepting again, having run out of something.
const ACCEPT_AGAIN: Duration = Duration::from_millis(100);

/// How long to wait before accepting again, having asked another worker to end a connection waiting
/// for its next message: long enough for it to run that round to its end.
const ENDED_ELSEWHERE: Duration = Duration::from_millis(10);

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
    /// The process, or the system, ran out of files: the connection that's waited longest for its
    /// next message is ended to make room, and what's waiting is looked at again once it's closed.
    EndIdle,
}

fn accept_failed(error: &std::io::Error) -> AfterAccept {
    match error.kind() {
        _ if crate::stdlib::net::gone_before_taken(error) => AfterAccept::Next,
        ErrorKind::WouldBlock => AfterAccept::Stop,
        _ if matches!(error.raw_os_error(), Some(23 | 24)) => AfterAccept::EndIdle,
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
pub(crate) fn park_on_fd(
    token: Token,
    fd: std::os::fd::RawFd,
    interest: Interest,
    deadline: Option<Instant>,
) -> Parked {
    if cut_short() {
        return Parked::Cancelled;
    }
    let mut source = mio::unix::SourceFd(&fd);
    let registered = REGISTRY.with_borrow(|registry| {
        registry.as_ref().is_some_and(|registry| registry.register(&mut source, token, interest).is_ok())
    });
    if !registered {
        return parked_a_moment(token, deadline);
    }
    parking(interest, deadline, None);
    let woken = fennec_parked::park(u64::try_from(token.0).unwrap_or(0));
    REGISTRY.with_borrow(|registry| registry.as_ref().map(|registry| registry.deregister(&mut source)));
    parked_as(woken)
}

/// A task about to park on this worker under a token, for whatever ends its wait from another
/// thread to wake: closing a connection wakes the tasks waiting on it this way, as a worker's poll
/// isn't told of a close made from this side on every system.
pub(crate) struct ParkedTask {
    pool: &'static Pool,
    at: usize,
    token: Token,
}

impl ParkedTask {
    /// The task running here, which is about to park on `token`. Off a worker there's none.
    pub(crate) fn here(token: Token) -> Option<ParkedTask> {
        let (pool, at) = super::parallel::here()?;
        Some(ParkedTask { pool, at, token })
    }

    /// Has it look again, if it's still parked on its token. One that's gone on isn't touched: a
    /// worker's tokens are given out in turn, so no other wait of its has this one.
    pub(crate) fn wake(&self) {
        let token = self.token;
        self.pool.run_on(self.at, Box::new(move |io: &mut Io| io.wake_token(token)));
    }
}

/// For tests: how many waits have parked with a copy of their socket registered.
#[cfg(test)]
pub(crate) static PARKED_ON_COPIES: AtomicU64 = AtomicU64::new(0);

/// Parks this task until `socket` may be ready for `interest`, or `deadline` passes, where a
/// socket itself can't be handed to the worker's poll, as on Windows, where the poll takes only its
/// own kind of socket: a copy of the socket is registered for the wait, under `token`, which
/// nothing else on this worker waits on, and closed after. A copy is the same connection, so the
/// poll says when the socket's ready. Only for a task that `can_park`.
///
/// **The copy is this call's, and nobody else's.** It's made here, lives in this call on the
/// task's own stack, and is taken off the poll and closed before the call gives anything back,
/// whichever way the wait ends: ready, out of time, stopped or cancelled. Each of those resumes
/// the task, and a task is never dropped while it's parked (`fennec_parked`), so there's no way
/// out that skips it. The socket stays its owner's throughout: closing a copy doesn't close the
/// connection, and once the owner closes the socket nothing is left holding it open.
///
/// Without it a task's wait on a socket blocked its worker on Windows. A program that was its own
/// client then waited for ever on one thread: the worker blocked reading an answer was the one
/// that had to take the connection and give it.
///
/// Unix hands the poll the socket itself (`park_on_fd`) for one wait at a time. A second wait on
/// the same connection, as when one task reads it while another writes to it, comes this way: a
/// poll takes a descriptor once, and a copy has one of its own.
#[cfg(test)]
pub(crate) fn park_on_copy(
    token: Token,
    socket: &std::net::TcpStream,
    interest: Interest,
    deadline: Option<Instant>,
) -> Parked {
    let copy = || socket.try_clone().ok().map(|copy| Box::new(TcpStream::from_std(copy)) as PollCopy);
    park_on_a_copy(token, copy, interest, deadline)
}

/// A copy of a socket, of the kind a poll takes: a connection's, a listener's or a UDP socket's.
pub(crate) type PollCopy = Box<dyn mio::event::Source>;

/// `park_on_copy` for any kind of socket, with `copy` to make the copy of it.
pub(crate) fn park_on_a_copy(
    token: Token,
    copy: impl FnOnce() -> Option<PollCopy>,
    interest: Interest,
    deadline: Option<Instant>,
) -> Parked {
    if cut_short() {
        return Parked::Cancelled;
    }
    let copied = copy().and_then(|mut source| {
        let registered = REGISTRY.with_borrow(|registry| {
            registry.as_ref().is_some_and(|registry| registry.register(&mut source, token, interest).is_ok())
        });
        registered.then_some(source)
    });
    let Some(mut source) = copied else {
        return parked_a_moment(token, deadline);
    };
    #[cfg(test)]
    PARKED_ON_COPIES.fetch_add(1, Ordering::Relaxed);
    parking(interest, deadline, None);
    let woken = fennec_parked::park(u64::try_from(token.0).unwrap_or(0));
    REGISTRY.with_borrow(|registry| registry.as_ref().map(|registry| registry.deregister(&mut source)));
    parked_as(woken)
}

#[cfg(test)]
thread_local! {
    /// For tests: how many waits on this thread couldn't be registered, and parked a moment at a
    /// time. A wait that looks again on a timer gets there in the end whatever's wrong with the
    /// poll, so a test of waits that should be woken checks that none of them came this way.
    static NOT_REGISTERED: Cell<u64> = const { Cell::new(0) };
}

/// For tests: how many waits on this thread couldn't be registered.
#[cfg(test)]
pub(crate) fn waits_not_registered_here() -> u64 {
    NOT_REGISTERED.get()
}

/// For tests: a wait on this thread, in a poll of its own, couldn't be registered.
#[cfg(test)]
pub(crate) fn a_wait_wasnt_registered() {
    NOT_REGISTERED.set(NOT_REGISTERED.get() + 1);
}

/// How long a wait that couldn't be registered parks before it looks again.
const LOOKS_AGAIN_AFTER: Duration = Duration::from_millis(1);

/// What a wait on a socket does when the worker's poll wouldn't take the socket, or no copy of it
/// could be made: it parks a moment on `token` alone, so its worker goes on with others and polls,
/// and then says to look again, or that `deadline` has passed.
///
/// This is a retry, and it's what's left when the system is out of something: descriptors for a
/// copy, or room in the poll. Nothing here can make more. Saying "ready" at once, as a wait
/// through the socket itself did, had its task look again and again without its worker ever
/// polling, and a wait with a deadline never ran out of time.
fn parked_a_moment(token: Token, deadline: Option<Instant>) -> Parked {
    #[cfg(test)]
    NOT_REGISTERED.set(NOT_REGISTERED.get() + 1);
    let now = Instant::now();
    if deadline.is_some_and(|deadline| deadline <= now) {
        return Parked::TimedOut;
    }
    let soon = now + LOOKS_AGAIN_AFTER;
    match park_on_token(token, Some(deadline.map_or(soon, |deadline| deadline.min(soon))), true, None) {
        Parked::Cancelled => Parked::Cancelled,
        Parked::Ready | Parked::TimedOut => Parked::Ready,
    }
}

/// What a task says of the wait it's about to park in, for its worker (see `PARKING`).
struct Parking {
    interest: Interest,
    deadline: Option<Instant>,
    stuck: Option<super::deadlock::Note>,
    /// Whether it's a connection's read waiting for a next message (`PARKS_IDLE`).
    idle: bool,
}

/// Says what the wait this task is about to park in waits for, when it gives up, and, for a stuck
/// one, its note: what every park does just before it parks.
fn parking(interest: Interest, deadline: Option<Instant>, stuck: Option<super::deadlock::Note>) {
    let idle = PARKS_IDLE.replace(false);
    PARKING.set(Some(Parking { interest, deadline, stuck, idle }));
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
    park_on_token(token, deadline, true, None)
}

/// `park_on`, with no time limit, for a stuck wait that leaves `note` for the deadlock check: a
/// channel's send or receive.
pub(crate) fn park_stuck(token: Token, note: super::deadlock::Note) -> Parked {
    park_on_token(token, None, true, Some(note))
}

/// Where a replay's task waiting for its turn at a place parks, when it can (`replay::turn_in`):
/// it's woken by the waker it hands out, which whoever moves that place's turn on calls.
pub(crate) struct TurnPark {
    pool: &'static Pool,
    at: usize,
    token: Token,
}

impl TurnPark {
    /// The running task's, when it can park, on its worker.
    pub(crate) fn here() -> Option<TurnPark> {
        if !can_park() {
            return None;
        }
        let (pool, at) = super::parallel::here()?;
        Some(TurnPark { pool, at, token: next_token() })
    }

    /// What wakes it, from any thread: mail to its worker, read only once it has parked.
    pub(crate) fn waker(&self) -> Box<dyn FnOnce() + Send> {
        let (pool, at, token) = (self.pool, self.at, self.token);
        Box::new(move || pool.run_on(at, Box::new(move |io: &mut Io| io.wake_token(token))))
    }

    /// Parks until it's woken, as a stuck wait with `note`. A stop wakes it without ending the wait:
    /// a replay's stops come as their own turns, as its recording had them.
    pub(crate) fn park(self, note: super::deadlock::Note) {
        let _ = park_through_a_stop(self.token, Some(note));
    }
}

/// Parks this task on `token` until `wake_token` is sent it, whatever it's told meanwhile: a stop
/// wakes it, as cancelled, without failing it. For a task waiting for work it handed to other
/// threads, which read what it lent them until they've finished. A `stuck` wait, a waiting
/// `parallel` call's join, leaves its note for the deadlock check.
pub(in crate::embed) fn park_through_a_stop(token: Token, stuck: Option<super::deadlock::Note>) -> Parked {
    park_on_token(token, None, false, stuck)
}

/// `park_on`, which a stop cuts short only if `stoppable`. When it isn't, a stop wakes it, as
/// cancelled, without failing it: for a task that must wait for another to end all the same. A
/// `stuck` wait's note goes with it to its worker, which keeps it while it's parked, counted (see
/// `deadlock`): a wait a stop cuts short before it parks is never counted.
fn park_on_token(
    token: Token,
    deadline: Option<Instant>,
    stoppable: bool,
    stuck: Option<super::deadlock::Note>,
) -> Parked {
    if stoppable && cut_short() {
        return Parked::Cancelled;
    }
    // What a task has printed comes out before it waits, however long that is.
    super::write_kept_output();
    parking(Interest::READABLE, deadline, stuck);
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
        // Cancelled, as a server's rounds are when it stops, every wait it comes to ends at once:
        // it fails as stopped, as at a channel, so it goes no further. Gone on as if its wait had
        // ended, a round asleep ran on, and one that slept in a loop never ended (#1915).
        Some(Wake::Cancelled) => {
            if !super::failed() {
                fail_as_stopped();
            }
            Parked::Cancelled
        }
        _ => Parked::Ready,
    }
}

/// Fails the running task as one that was stopped.
pub(crate) fn fail_as_stopped() {
    super::fail_with(fennec_syntax::Diagnostic::new(super::STOPPED_FAILURE, fennec_syntax::Span::default()));
}

/// Whether this is a launched task that's been stopped. If so, it fails, as a crash does, so it
/// ends with its locks put back and let go; that it was stopped keeps its end quiet.
pub(crate) fn cut_short() -> bool {
    let stopped = super::threads::stopped();
    if stopped {
        fail_as_stopped();
    }
    stopped
}

mod launch;
#[cfg(test)]
pub(in crate::embed) use launch::launch_on;
pub use launch::{TaskBody, launch, launch_end, task_failed};
pub(in crate::embed) use launch::{take_failed_with, task_number};

use super::blocking::{self, Slot, slot_of};

/// Whether every pool of blocking threads is idle, for the deadlock check: a job running there can
/// end in a wake.
pub(crate) fn blocking_idle() -> bool {
    blocking::pool().idle() && blocking::heavy().idle() && blocking::lookups().idle()
}

/// Runs `work`, which blocks its thread, so that this task parks rather than holding its worker:
/// on one of the blocking threads (`embed/blocking.rs`), which hands its result back and wakes the
/// task through its worker's mail. Anywhere a wait can't park, it runs here.
pub(crate) fn off_thread<R: Send + 'static>(work: impl FnOnce() -> R + Send + 'static) -> R {
    off_thread_on(blocking::pool(), work)
}

/// `off_thread` for work that keeps a core busy for long, as hashing a password does: on a pool of
/// its own, a thread to a core and at most 8, so a burst of it runs a few at once while the rest
/// wait their turn parked.
pub(crate) fn off_thread_heavy<R: Send + 'static>(work: impl FnOnce() -> R + Send + 'static) -> R {
    off_thread_on(blocking::heavy(), work)
}

/// `off_thread` on `threads`. A task whose work can't park, as inside a `lock`, runs it where it
/// is, holding its worker meanwhile as a wait that blocks does, and is counted with those.
fn off_thread_on<R: Send + 'static>(
    threads: &'static blocking::Blocking,
    work: impl FnOnce() -> R + Send + 'static,
) -> R {
    let Some((pool, at)) = super::parallel::here() else { return work() };
    if !can_park() {
        if fennec_parked::stack_low().is_some() {
            BLOCKED_WAITS.fetch_add(1, Ordering::Relaxed);
        }
        return work();
    }
    // For a test to see, from another thread, whether this one has work on a blocking thread.
    #[cfg(test)]
    let work = {
        let from = std::thread::current().id();
        blocking_jobs_change(from, 1);
        move || {
            let done = work();
            blocking_jobs_change(from, -1);
            done
        }
    };
    // Whether it's failing already, as a cleanup is, so a stop now isn't what cut the work short.
    let failing = super::failed();
    let token = Token(NEXT_TOKEN.get());
    NEXT_TOKEN.set(token.0.wrapping_add(1) % CONNECTIONS);
    let result: Arc<Mutex<Option<R>>> = Arc::new(Mutex::new(None));
    let given = Arc::clone(&result);
    let job: blocking::Job = Box::new(move || {
        let done = work();
        *given.lock().unwrap_or_else(PoisonError::into_inner) = Some(done);
        // The worker takes its mail only after this task has parked, on that same thread.
        pool.run_on(at, Box::new(move |io: &mut Io| io.wake_token(token)));
    });
    match threads.run(job) {
        Ok(()) => {
            super::write_kept_output();
            parking(Interest::READABLE, None, None);
            let _ = fennec_parked::park(u64::try_from(token.0).unwrap_or(0));
        }
        // No thread could be started to run them, so they run here: this job's wake finds nothing
        // parked, and another task's job wakes that task through its worker's mail.
        Err(jobs) => {
            for job in jobs {
                job();
            }
        }
    }
    let done = loop {
        if let Some(done) = result.lock().unwrap_or_else(PoisonError::into_inner).take() {
            break done;
        }
        // Cancelled before it finished: the work runs to its end, and this waits for it.
        let _ = yield_task_always();
    };
    back_from_blocking(failing);
    done
}

/// What a task that handed work to a blocking thread does once it's back with the work's end. A
/// round cancelled, or a task stopped, while its work ran fails as stopped now, as at any other
/// wait, and goes no further. It went on past the work before (#1950). A replay stops it where its
/// recording says it was. A crash's cleanup goes on: its work is done, and failing now would only
/// say its cleanup crashed. `failing` is whether it was failing already as it handed the work over.
fn back_from_blocking(failing: bool) {
    if super::threads::cleaning_up() {
        return;
    }
    if crate::replay::replaying() {
        let _ = crate::replay::work_ended(false);
        return;
    }
    let cut = if fennec_parked::is_cancelled() {
        if !super::failed() {
            fail_as_stopped();
        }
        !failing
    } else {
        cut_short() && !failing
    };
    let _ = crate::replay::work_ended(cut);
}

/// A name's lookup, which only the system can do and only by blocking, run on a thread of its own
/// while this task parks, as `off_thread` runs work, for no longer than until `deadline`: `None`
/// if it isn't done by then. The thread finishes it all the same, and its answer is dropped.
///
/// Lookups get a pool of their own, of at most `MOST_LOOKUPS` threads, so a name server that never
/// answers holds those and no others: a lookup past them waits its turn parked, and one whose task
/// gave up before its turn came isn't made at all. They once shared the pool every file's read and
/// database call runs on, waiting for as long as the system's resolver took, past the connect's
/// own timeout, so enough of them at once held every thread there (#1826).
///
/// A late answer wakes nothing. Its thread sees its task gave up, and the wake it sends to the
/// task's worker looks again there, where the task marks that it gave up before it parks again,
/// so it never wakes a later wait that parked on the same token, this task's or another's.
///
/// A thread that can't park waits for it on the same pool, with the same deadline
/// (`blocking::waited_until`).
pub(crate) fn off_thread_until<R: Send + 'static>(
    deadline: Instant,
    work: impl FnOnce() -> R + Send + 'static,
) -> Option<R> {
    let Some((pool, at)) = super::parallel::here() else { return blocking::waited_until(deadline, work) };
    if !can_park() {
        if fennec_parked::stack_low().is_some() {
            BLOCKED_WAITS.fetch_add(1, Ordering::Relaxed);
        }
        return blocking::waited_until(deadline, work);
    }
    // Counted until the job ends, however it does: one that finds its task gave up never runs
    // the work.
    #[cfg(test)]
    let counted = {
        let from = std::thread::current().id();
        blocking_jobs_change(from, 1);
        Counted(from)
    };
    let failing = super::failed();
    let token = next_token();
    #[cfg(test)]
    LAST_LOOKUP_TOKEN.set(Some(token));
    let slot: Arc<Mutex<Slot<R>>> = Arc::new(Mutex::new(Slot::Waiting));
    let given = Arc::clone(&slot);
    let job: blocking::Job = Box::new(move || {
        #[cfg(test)]
        let _counted = counted;
        // A lookup that waited its turn past its task's deadline isn't made.
        if matches!(*slot_of(&given), Slot::GaveUp) {
            return;
        }
        let done = work();
        {
            let mut held = slot_of(&given);
            if matches!(*held, Slot::GaveUp) {
                return;
            }
            *held = Slot::Answered(done);
        }
        pool.run_on(
            at,
            Box::new(move |io: &mut Io| {
                if !matches!(*slot_of(&given), Slot::GaveUp) {
                    io.wake_token(token);
                }
            }),
        );
    });
    match blocking::lookups().run(job) {
        Ok(()) => {
            super::write_kept_output();
            parking(Interest::READABLE, Some(deadline), None);
            let _ = fennec_parked::park(u64::try_from(token.0).unwrap_or(0));
        }
        Err(jobs) => {
            for job in jobs {
                job();
            }
        }
    }
    let done = loop {
        let mut held = slot_of(&slot);
        if let Some(done) = blocking::taken(&mut held) {
            break Some(done);
        }
        // Past its deadline, or cancelled or stopped before its answer came, it gives up: the
        // lookup owns everything it reads, so nothing lent is left for its thread to read.
        if Instant::now() >= deadline || fennec_parked::is_cancelled() || super::threads::stopped() {
            *held = Slot::GaveUp;
            break None;
        }
        drop(held);
        // Woken early, as a storm of readiness does: it parks again for what's left.
        parking(Interest::READABLE, Some(deadline), None);
        let _ = fennec_parked::park(u64::try_from(token.0).unwrap_or(0));
    };
    back_from_blocking(failing);
    done
}

#[cfg(test)]
thread_local! {
    /// For tests: the token the last lookup on this thread parked on, for a test to have the next
    /// wait here park on it again.
    static LAST_LOOKUP_TOKEN: Cell<Option<Token>> = const { Cell::new(None) };
}

/// For tests: has the next wait on this thread park on the token the last lookup here parked on, as
/// it would once the tokens had come round again.
#[cfg(test)]
pub(crate) fn reuse_the_last_lookup_s_token() {
    if let Some(token) = LAST_LOOKUP_TOKEN.get() {
        NEXT_TOKEN.set(token.0);
    }
}

/// For tests: how many pieces of work each thread's tasks have on the blocking threads, not yet
/// done, and how many they've ever handed over.
#[cfg(test)]
static BLOCKING_JOBS: Mutex<Option<HashMap<std::thread::ThreadId, (i64, i64)>>> = Mutex::new(None);

/// For tests: a piece of work counted for `blocking_jobs_of` until it's dropped.
#[cfg(test)]
struct Counted(std::thread::ThreadId);

#[cfg(test)]
impl Drop for Counted {
    fn drop(&mut self) {
        blocking_jobs_change(self.0, -1);
    }
}

#[cfg(test)]
fn blocking_jobs_change(from: std::thread::ThreadId, by: i64) {
    let mut jobs = BLOCKING_JOBS.lock().unwrap_or_else(PoisonError::into_inner);
    let (now, ever) = jobs.get_or_insert_with(HashMap::new).entry(from).or_default();
    *now += by;
    *ever += by.max(0);
}

/// How many pieces of work the tasks on `thread` have handed to the blocking threads that aren't
/// done yet, and how many they've handed over in all: for a test to ask, from the other end of a
/// connection or after the fact, whether a task held one.
#[cfg(test)]
pub(crate) fn blocking_jobs_of(thread: std::thread::ThreadId) -> (i64, i64) {
    let jobs = BLOCKING_JOBS.lock().unwrap_or_else(PoisonError::into_inner);
    jobs.as_ref().and_then(|jobs| jobs.get(&thread).copied()).unwrap_or((0, 0))
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

    /// Another stopper of the same server.
    pub(super) fn again(&self) -> Stopper {
        Stopper { server: Arc::clone(&self.server), pool: self.pool }
    }

    pub fn stop(&self) {
        let id = self.server.id;
        for at in 0..self.pool.threads() {
            self.pool.run_on(at, Box::new(move |io: &mut Io| io.leave(id)));
        }
    }

    /// Closes the server, as `server.close()` asks from inside the program, once: it takes no
    /// more connections, its rounds waiting for a next message end where the server drains on a
    /// stop, and those running finish. The loop over it then ends, as after a `break`.
    pub(super) fn close(&self) {
        if self.server.draining.swap(true, Ordering::SeqCst) {
            return;
        }
        self.drain_waking();
    }

    /// Has every worker drain the server, as a close does.
    pub(super) fn drain_waking(&self) {
        let id = self.server.id;
        for at in 0..self.pool.threads() {
            self.pool.run_on(at, Box::new(move |io: &mut Io| io.drain_waking(id)));
        }
    }

    /// The flag the server drains by.
    pub(super) fn draining(&self) -> &Arc<AtomicBool> {
        &self.server.draining
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
    /// What its serving drains by, which a close sets, from when its listener was taken.
    pub(crate) draining: Arc<AtomicBool>,
}

/// Serves `listener` on `pool`'s workers, running `handler` on each connection as a parked task,
/// while this thread waits. The workers' tasks can read what's `lent`. `started` is given a
/// `Stopper` once the server is running, and the server runs until it's stopped.
pub(super) fn serve(
    listener: TcpListener,
    pool: &'static Pool,
    lent: Option<&Arc<super::table::Table>>,
    handler: &Handler,
    (started, replay, Listened { tls, stop, draining }): (
        impl FnOnce(Stopper),
        Option<&crate::replay::Task>,
        Listened,
    ),
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
        draining,
    });
    // The worker that accepts joins last, so every other worker has joined before it hands one
    // a connection: each worker takes what it's given in turn.
    let mut listener = Some(listener);
    for at in (1..workers).chain([0]) {
        let (joining, given) = (Arc::clone(&server), if at == 0 { listener.take() } else { None });
        pool.run_on(at, Box::new(move |io: &mut Io| io.join(joining, given)));
    }
    #[cfg(test)]
    at_gap(Gap::Joined);
    started(Stopper { server: Arc::clone(&server), pool });
    // A server that stops when the program's asked to drains then, and its rounds have `grace` to
    // finish before they're cut short.
    let listening = stop.map(|grace| {
        let stopping = Stopper { server: Arc::clone(&server), pool };
        crate::signals::on_stop(Box::new(move || stopping.drain_for(grace)))
    });
    let stopping = Stopper { server: Arc::clone(&server), pool };
    served_here(&server, || stopping.stop());
    if let Some(listening) = listening {
        crate::signals::forget(listening);
    }
    server.lent.lock().unwrap_or_else(PoisonError::into_inner).take();
    std::mem::take(&mut *server.total.lock().unwrap_or_else(PoisonError::into_inner))
}

/// For a test: where serving a server can be held up, for a close to land there.
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Gap {
    /// Its listener has left the open sockets, and nothing has joined yet.
    Taken,
    /// Its workers have been given it, and its stopper isn't yet known.
    Joined,
}

/// For a test: what to do at a gap, and which.
#[cfg(test)]
pub(super) type AtGap = (Gap, Box<dyn FnOnce()>);

#[cfg(test)]
thread_local! {
    /// For a test: what this thread does at a gap, as it next serves a server, once.
    pub(super) static AT_GAP: RefCell<Option<AtGap>> = const { RefCell::new(None) };
}

/// For a test: runs what it asked to at this gap, on the thread serving.
#[cfg(test)]
pub(super) fn at_gap(gap: Gap) {
    let wanted = AT_GAP.with_borrow_mut(|at| at.take_if(|(when, _)| *when == gap));
    if let Some((_, then)) = wanted {
        then();
    }
}

/// For a test: the pool whose workers pause before resuming a kept round (`Io::pause_for_a_test`).
#[cfg(test)]
static PAUSE_BEFORE_KEPT: AtomicUsize = AtomicUsize::new(0);

/// Waits for the server to end, as `wait_for_every_worker` does, with it noted meanwhile as one the
/// task running here serves, when a task on a worker is: so leaving a server that task is a round
/// of stops this one first, and cancels the round only once it's ended.
fn served_here(server: &Arc<Server>, stop: impl Fn()) {
    let task = RESUMED.get().filter(|_| super::parallel::here().is_some());
    if let Some(task) = task {
        SERVING.with_borrow_mut(|serving| serving.entry(task).or_default().push(Arc::clone(server)));
    }
    wait_for_every_worker(&server.left, stop);
    // A round its worker kept is cancelled by that worker, once it sees this server has ended. The
    // round can see that first, when another worker was the last to leave between its worker's
    // look and its own: it waits for the cancel, rather than go on uncancelled (#2004).
    if let Some(task) = task {
        while KEPT.with_borrow(|kept| kept.contains(&task)) && !fennec_parked::is_cancelled() {
            let _ = park_on_token(next_token(), Some(Instant::now() + SERVER_END_LOOKS), false, None);
        }
    }
    if let Some(task) = task {
        SERVING.with_borrow_mut(|serving| {
            if let Some(servers) = serving.get_mut(&task) {
                servers.retain(|other| !Arc::ptr_eq(other, server));
                if servers.is_empty() {
                    serving.remove(&task);
                }
            }
        });
    }
}

/// Whether `task` serves a server of its own on this worker that hasn't ended.
fn serves(task: TaskId) -> bool {
    SERVING.with_borrow(|serving| {
        serving.get(&task).is_some_and(|servers| servers.iter().any(|server| !server.left.is_done()))
    })
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
    // A task parks, as a live server's does, so its worker runs the rounds it was given (#1832).
    let stopping = Stopper { server: Arc::clone(&server), pool };
    served_here(&server, || stopping.stop());
    server.lent.lock().unwrap_or_else(PoisonError::into_inner).take();
    std::mem::take(&mut *server.total.lock().unwrap_or_else(PoisonError::into_inner))
}

mod calls;

/// Closes the server with this handle once it's being served, for a test's client to end the
/// serving it talked to: only that server, as tests share the process, and [`stop_servers`] would
/// stop another test's too (#2540). Gives up after 20 seconds, when it was never served.
#[cfg(test)]
pub(crate) fn close_when_served(server: i64) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !close_served(server) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
}
pub(super) use calls::served_call;
pub use calls::{ConnBody, net_serve, net_serve_failure, stop_servers};
pub(crate) use calls::{close_served, serving_soon};
pub(in crate::embed) use calls::{
    served_body, served_draining, served_head, served_reads_within, served_room_for_body, served_write,
};

#[cfg(test)]
mod tests;
