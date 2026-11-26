//! `std.net`: TCP and UDP through the `Net` capability. A `Connection`, `Server` or `UdpSocket` holds
//! a private handle into the program's table of open sockets, so code that was never given `Net` can't
//! make one up to reach a socket. In a test, the network is in memory, in `Fake`.
//! Nothing else runs while a test waits, so a read that would wait forever stops the test and
//! says why, and a server with no connection waiting is done.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use fennec_syntax::{Diagnostic, Span};

use super::http;
use crate::methods::no_method;
use crate::value::{Capability, Value};

pub(crate) mod head;

/// How long connecting waits unless told otherwise.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const SENT_BEFORE_TLS: &str =
    "the other side sent more than was read before TLS started, and that can't be trusted";
const ALREADY_TLS: &str = "this connection already has TLS over it";

/// The hosts a test's fake network has: its own, where its servers are.
const FAKE_HOSTS: [&str; 2] = ["localhost", "127.0.0.1"];

/// The most a UDP packet can hold.
const MOST_IN_A_PACKET: usize = 65_507;

/// How long a UDP read waits before it looks again at whether its socket was closed. `close` can't
/// wake one by itself, as UDP has no `shutdown` to ask for.
#[cfg(not(any(test, feature = "threads")))]
const UDP_LOOKS_EVERY: Duration = Duration::from_millis(100);

/// A real connection, which any thread holding its handle can use: one thread reading and another
/// writing don't wait for each other, and nothing's interleaved within either.
struct Stream {
    peer: String,
    socket: TcpStream,
    reader: Mutex<Reading>,
    /// What a line read that timed out had got, for the next to go on from.
    partial_line: Mutex<Vec<u8>>,
    writing: Mutex<()>,
    /// Whether a server took it, so closing it lingers: see `linger`.
    served: bool,
    /// Whether anything's been read since it last wrote: before anything has, a read is waiting
    /// for a next message, which a server draining ends.
    read_since_write: AtomicBool,
    /// Whether a read is waiting for the first byte of a next message, with nothing of one taken
    /// in: only then does a server stopping end the read, as only then is it between messages.
    awaiting_a_message: AtomicBool,
    /// The label a server's connection prints as, taken as the server accepted it, so connections
    /// are numbered in the order they came, whichever task serving one starts first. A client's is
    /// 0 here, as it's labelled as it's made.
    label: i64,
    /// When every read from now on has to be done by, and how long that was from when it was
    /// set: a server gives a request's head and body one, so a client sending a byte at a time
    /// can't hold a connection open for longer than a request may take.
    reads_by: Mutex<Option<(Instant, Duration)>>,
    /// Its share of the room every server's request bodies have, for the body it's reading or
    /// answering, given back as its next request starts, as it closes, or as it goes.
    body_held: BodyHeld,
    /// Whether it's been closed from here. A task about to park on it looks first, as nothing
    /// would wake it after.
    closed: AtomicBool,
    /// Whether its socket has been made not to block, which it then never does again: from the
    /// first time a task that can park reads from it or writes to it. From then on every wait on
    /// it is on a poll, a worker's for a task and a thread's own for anything else.
    never_blocks: AtomicBool,
    /// On Windows, whether the copy of the socket the reader reads through has been told too.
    #[cfg(windows)]
    reader_never_blocks: AtomicBool,
    /// The tasks parked waiting on it, each with a number of its wait's own, for closing it to
    /// wake: on Windows a worker's poll isn't told that a connection was closed from this side.
    #[cfg(any(test, feature = "threads"))]
    parked: Mutex<Vec<Wait>>,
}

/// A task parked waiting on a connection, as the connection notes it.
#[cfg(any(test, feature = "threads"))]
struct Wait {
    /// A number of this wait's own, to take its note back by.
    number: u64,
    wake: Waking,
    /// Whether its worker's poll was told of the socket itself, and not of a copy of it. One wait
    /// at a time is.
    on_the_socket: bool,
}

/// How to have a wait on a connection look again from another thread.
#[cfg(any(test, feature = "threads"))]
enum Waking {
    /// A task parked on its worker's poll.
    Task(crate::embed::serve::ParkedTask),
    /// A thread waiting in a poll of its own, by that poll's waker.
    Thread(Arc<mio::Waker>),
}

#[cfg(any(test, feature = "threads"))]
impl Waking {
    fn wake(&self) {
        match self {
            Waking::Task(task) => task.wake(),
            // A waker that can't wake leaves the wait to its deadline, as before there was one.
            Waking::Thread(waker) => drop(waker.wake()),
        }
    }
}

/// Has every wait on a connection look again: after it's closed or shut from here, which a
/// worker's poll isn't told of on every system.
#[cfg(any(test, feature = "threads"))]
fn wake_waits(stream: &Stream) {
    let waits = std::mem::take(&mut *stream.parked.lock().unwrap_or_else(PoisonError::into_inner));
    for Wait { wake, .. } in waits {
        wake.wake();
    }
}

/// What a connection reads through: the socket itself, or TLS over it, which writes through the
/// same, so a TLS connection's reads and writes take turns.
enum Reading {
    Plain(BufReader<TcpStream>),
    Tls(Box<dyn SecureStream>),
}

impl Reading {
    /// Whether it holds bytes taken from the socket that haven't been read yet: a message started.
    fn holds_unread(&self) -> bool {
        match self {
            Reading::Plain(plain) => !plain.buffer().is_empty(),
            Reading::Tls(secured) => secured.holds_unread(),
        }
    }
}

/// A connection with TLS over it, read and written as a plain one is. Only the TLS module makes
/// one, so a program that never asks for TLS links none of it.
pub(crate) trait SecureStream: BufRead + Write + Send {
    /// The socket it reads through. It's a copy of the connection's own, and on Windows a copy
    /// keeps its own timeouts, so a read's timeout goes on this one.
    fn socket(&self) -> &TcpStream;

    /// Whether it holds bytes read from the socket that haven't been read from it yet.
    fn holds_unread(&self) -> bool;
}

/// What a TLS session reads from and writes to: a socket that doesn't block.
#[cfg_attr(
    not(any(test, feature = "threads")),
    expect(dead_code, reason = "only a served connection uses it, which needs threads")
)]
pub(crate) trait Transport: Read + Write {}

impl<T: Read + Write> Transport for T {}

/// One connection's TLS on a server, over a socket that doesn't block: anything that has to wait
/// says `WouldBlock`, for the task to park and try again.
#[cfg_attr(
    not(any(test, feature = "threads")),
    expect(dead_code, reason = "only a served connection uses it, which needs threads")
)]
pub(crate) trait ServerSession: Send {
    /// Does as much of the handshake as it can, and says when it's done.
    fn handshake(&mut self, socket: &mut dyn Transport) -> std::io::Result<bool>;
    /// Up to `count` bytes of what the other side sent, onto `into`: 0 at the end.
    fn read_onto(
        &mut self,
        socket: &mut dyn Transport,
        into: &mut Vec<u8>,
        count: usize,
    ) -> std::io::Result<usize>;
    /// Takes as much of `data` as it can, saying how much, and sends what it can of what it holds.
    fn write_some(&mut self, socket: &mut dyn Transport, data: &[u8]) -> std::io::Result<usize>;
    /// Whether it has something to send before anything else can happen.
    fn wants_write(&self) -> bool;
}

/// A server's TLS, what `net.listen(port, tls=...)` proves itself with.
pub(crate) trait TlsServer: Send + Sync {
    /// A session for a connection a task serves.
    #[cfg_attr(
        not(any(test, feature = "threads")),
        expect(dead_code, reason = "only a served connection uses it, which needs threads")
    )]
    fn session(&self) -> Result<Box<dyn ServerSession>, Failure>;
    /// TLS over a connection `server.accept()` took, its handshake done in at most `timeout`.
    ///
    /// With `waits`, the socket doesn't block, and wherever the handshake can't go on it asks
    /// `waits` to wait until the socket can be read or written. So a task that can park does the
    /// handshake where it is, parked for as long as the other side takes, and holds no thread
    /// for it: the other side decides how long a handshake lasts, and a stranger's slow ones
    /// would otherwise each hold a blocking thread for the whole of `timeout`.
    fn accepted(
        &self,
        tcp: TcpStream,
        timeout: Duration,
        waits: Option<Waits<'_>>,
    ) -> Result<Box<dyn SecureStream>, Failure>;
}

/// Waits until a socket that doesn't block may be ready for what's wanted of it, and says true
/// when there's no more waiting to do: the time allowed is up, or the task's been stopped.
pub(crate) type Waits<'a> = &'a mut dyn FnMut(&TcpStream, Wanted) -> bool;

/// A real UDP socket. Only one thread reads at a time, as how long a read waits is the socket's.
struct Udp {
    socket: UdpSocket,
    /// Without threads a receive waits in the kernel, for as long as the socket's own timeout
    /// says, so one receives at a time.
    #[cfg(not(any(test, feature = "threads")))]
    receiving: Mutex<()>,
    #[cfg(any(test, feature = "threads"))]
    waits: SocketWaits,
}

/// What a listener or a UDP socket keeps for the waits on it, as a connection keeps its own.
#[cfg(any(test, feature = "threads"))]
#[derive(Default)]
struct SocketWaits {
    /// Whether the socket has been made not to block, which it then never does again: from the
    /// first wait on it that goes through a poll.
    never_blocks: AtomicBool,
    /// Whether it's been closed from here: a wait about to start looks first, as nothing would
    /// wake it after.
    closed: AtomicBool,
    /// The waits on it, for closing it to wake.
    waiting: Mutex<Vec<Wait>>,
}

#[cfg(any(test, feature = "threads"))]
impl SocketWaits {
    /// It's closed from here: no wait on it starts after this, and those under way look again.
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let waiting = std::mem::take(&mut *self.waiting.lock().unwrap_or_else(PoisonError::into_inner));
        for Wait { wake, .. } in waiting {
            wake.wake();
        }
    }

    /// Makes its socket not block, with `make`, the first time a wait on it goes through a poll.
    fn never_blocking(&self, make: impl FnOnce() -> std::io::Result<()>) -> Result<(), Failure> {
        if !self.never_blocks.load(Ordering::SeqCst) {
            make().map_err(|error| Failure::Other(error.to_string()))?;
            self.never_blocks.store(true, Ordering::SeqCst);
        }
        Ok(())
    }

    /// Waits until `socket` may be ready for `interest`, `deadline` passes, or it's closed from
    /// here. A task that can park parks on its worker's poll. Anything else waits on its thread,
    /// in a poll of its own. True once there's no more waiting to do: the time's up, or the task's
    /// been stopped.
    fn out(&self, socket: &dyn Polled, interest: mio::Interest, deadline: Option<Instant>) -> bool {
        let noted = (&self.closed, &self.waiting);
        if task_parks() {
            return park_noted(noted, socket, interest, deadline) != crate::embed::serve::Parked::Ready;
        }
        own_poll_wait(socket, interest, deadline, Some(noted))
    }
}

/// A socket a wait can be put on a poll for: a connection's, a listener's or a UDP socket's.
#[cfg(any(test, feature = "threads"))]
trait Polled {
    /// A copy of it of the kind a poll takes, which has a descriptor of its own.
    fn poll_copy(&self) -> Option<crate::embed::serve::PollCopy>;
    /// Its own descriptor, which a worker's poll can be told of on Unix.
    #[cfg(unix)]
    fn descriptor(&self) -> std::os::fd::RawFd;
}

#[cfg(any(test, feature = "threads"))]
impl Polled for TcpStream {
    fn poll_copy(&self) -> Option<crate::embed::serve::PollCopy> {
        Some(Box::new(mio::net::TcpStream::from_std(self.try_clone().ok()?)))
    }

    #[cfg(unix)]
    fn descriptor(&self) -> std::os::fd::RawFd {
        std::os::fd::AsRawFd::as_raw_fd(self)
    }
}

#[cfg(any(test, feature = "threads"))]
impl Polled for TcpListener {
    fn poll_copy(&self) -> Option<crate::embed::serve::PollCopy> {
        Some(Box::new(mio::net::TcpListener::from_std(self.try_clone().ok()?)))
    }

    #[cfg(unix)]
    fn descriptor(&self) -> std::os::fd::RawFd {
        std::os::fd::AsRawFd::as_raw_fd(self)
    }
}

#[cfg(any(test, feature = "threads"))]
impl Polled for UdpSocket {
    fn poll_copy(&self) -> Option<crate::embed::serve::PollCopy> {
        Some(Box::new(mio::net::UdpSocket::from_std(self.try_clone().ok()?)))
    }

    #[cfg(unix)]
    fn descriptor(&self) -> std::os::fd::RawFd {
        std::os::fd::AsRawFd::as_raw_fd(self)
    }
}

/// A port listened on, with the TLS each connection it takes starts with, if it has any.
struct Listening {
    listener: TcpListener,
    #[cfg(any(test, feature = "threads"))]
    waits: SocketWaits,
    tls: Option<Arc<dyn TlsServer>>,
    /// How long its rounds have to finish once the program's asked to stop, when `stop_on` said
    /// to drain then.
    stop: Mutex<Option<Duration>>,
}

#[derive(Clone)]
enum Socket {
    Stream(Arc<Stream>),
    Listener(Arc<Listening>),
    Udp(Arc<Udp>),
}

/// The real sockets this process has open, by handle. Held only to find one, never while it waits.
static OPEN: LazyLock<Mutex<HashMap<i64, Socket>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn open<T>(use_them: impl FnOnce(&mut HashMap<i64, Socket>) -> T) -> T {
    use_them(&mut OPEN.lock().unwrap_or_else(PoisonError::into_inner))
}

/// One end of a connection in a test's network.
struct FakeStream {
    /// What the other end wrote that this end hasn't read.
    inbox: VecDeque<u8>,
    peer: i64,
    peer_closed: bool,
}

/// A packet in a test's network: its data and its sender's address.
type FakePacket = (Vec<u8>, String);

/// A test's network, in memory. Handles count up from 1, so a test runs the same every time.
#[derive(Default)]
pub(crate) struct Fake {
    last: i64,
    /// Each server's port, and the connections waiting for it to accept them.
    servers: BTreeMap<i64, (i64, VecDeque<(i64, String)>)>,
    /// The servers listening with a certificate, and the connections with TLS over them. No real TLS runs in a
    /// test: a connection has only to agree with its server about using it.
    secure: BTreeSet<i64>,
    streams: BTreeMap<i64, FakeStream>,
    /// Each UDP socket's port, and the data and sender of what's been sent to it.
    udp: BTreeMap<i64, (i64, VecDeque<FakePacket>)>,
    /// What each request a test scripted answers, by its method and URL.
    answers: BTreeMap<String, Value>,
    /// Each request sent, as its method and URL.
    requests: Vec<String>,
    /// The label of each connection a server accepted, taken as it accepted it.
    labels: BTreeMap<i64, i64>,
}

impl Fake {
    fn handle(&mut self) -> i64 {
        self.last += 1;
        self.last
    }

    /// A method of a test's `Net`, as native code names it: with named arguments on the name, in
    /// the order they're written, like `get.params.timeout`. The fake never waits, so a timeout
    /// changes nothing.
    pub(crate) fn net_method(&mut self, name: &str, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
        let mut parts = name.split('.');
        let method = parts.next().unwrap_or_default();
        if matches!(method, "get" | "post" | "request") {
            let named: Vec<&str> = parts.collect();
            let (plain, last) = args.split_at(args.len().saturating_sub(named.len()));
            let given: Vec<(&str, &Value)> = named.iter().copied().zip(last).collect();
            let request = request_of(method, plain, &given, span)?;
            return Ok(self.request(&request));
        }
        let (method, args) = in_order(name, args);
        let found = match (method, args.as_slice()) {
            ("script", [Value::Str(request), response]) => {
                self.answers.insert(request.to_string(), response.clone());
                return Ok(Value::Unit);
            }
            ("requests", []) => {
                return Ok(Value::list(self.requests.iter().map(|sent| Value::str(sent)).collect()));
            }
            ("_connect", [Value::Str(host), Value::Int(port)]) => {
                self.connect(host, port_of(*port, span)?, false)
            }
            ("_connect", [Value::Str(host), Value::Int(port), Value::Bool(tls)]) => {
                self.connect(host, port_of(*port, span)?, *tls)
            }
            ("_listen", [Value::Int(port)]) => self.listen(port_of(*port, span)?, false),
            ("_listen", [Value::Int(port), certificate]) => {
                self.listen(port_of(*port, span)?, has_certificate(certificate, span)?)
            }
            ("_udp", []) => self.udp(0),
            ("_udp", [Value::Int(port)]) => self.udp(port_of(*port, span)?),
            ("_connect" | "_listen" | "_udp" | "script" | "requests", _) => return Err(wrong(method, span)),
            _ => return Err(no_method(&Value::Capability(Capability::Net), method, span)),
        };
        Ok(made_or_failed(found))
    }

    /// A method of a socket of a test's network, by its handle, as native code names it: like
    /// `Connection.read_line`, ending `.timeout` when the last argument is one.
    pub(crate) fn socket_method(
        &mut self,
        handle: i64,
        name: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let lost = || Diagnostic::new("the native code lost a method's name", span);
        let (kind, method) = name.split_once('.').ok_or_else(lost)?;
        let (method, args, timeout) = timed(method, args, span)?;
        let done = match (kind, method.as_str(), args.as_slice()) {
            ("Connection", "read", [Value::Int(count)]) => {
                let count = bytes_asked(*count, span)?;
                self.read_by(handle, Some(count), timeout, span)?
                    .map(|data| Value::Bytes(Rc::from(data.unwrap_or_default())))
            }
            // A test's connection holds what was sent in memory, so its body needs no room among
            // a real server's: it always has room, and is read whole, or as far as was sent.
            ("Connection", "_room_for_body", [Value::Int(_)]) => return Ok(Value::Bool(true)),
            ("Connection", "_body", [Value::Int(size)]) => {
                let size = bytes_asked(*size, span)?;
                let mut body = Vec::new();
                let mut failed = None;
                while body.len() < size {
                    match self.read_by(handle, Some(size - body.len()), timeout, span)? {
                        Ok(Some(more)) if !more.is_empty() => body.extend(more),
                        Ok(_) => break,
                        Err(failure) => {
                            failed = Some(failure);
                            break;
                        }
                    }
                }
                failed.map_or_else(|| Ok(Value::Bytes(Rc::from(body))), Err)
            }
            ("Connection", "read_line", []) => or_timed_out(self.read_line(handle, false, span), timeout)?,
            ("Connection", "read_line.crlf", [Value::Bool(crlf)]) => {
                or_timed_out(self.read_line(handle, *crlf, span), timeout)?
            }
            ("Connection", "_read_head", given) if let Some(limits) = head::Limits::of(given) => {
                head::read(limits, || {
                    let line = or_timed_out(self.read_line(handle, true, span), timeout)?;
                    Ok(line.map(|line| match line {
                        Value::Str(text) => Some(text.to_string()),
                        _ => None,
                    }))
                })?
            }
            ("Connection", "start_tls", [Value::Str(_)]) => self.start_tls(handle).map(|()| Value::Unit),
            ("Connection", "write", [data @ (Value::Str(_) | Value::Bytes(_))]) => {
                self.write(handle, data_of(data)).map(|()| Value::Unit)
            }
            ("Connection", "_write_parts", [head @ Value::Str(_), body @ Value::Bytes(_)]) => {
                self.write(handle, &parts_of(head, body)).map(|()| Value::Unit)
            }
            ("UdpSocket", "receive", []) => self.receive(handle, timeout).map(packet_value),
            ("UdpSocket", "send_to", [data @ (Value::Str(_) | Value::Bytes(_)), Value::Str(address)]) => {
                self.send_to(handle, data_of(data), address).map(|()| Value::Unit)
            }
            ("Server", "_accept", []) => {
                let accepted = self.accept(handle);
                let made = accepted.map(|(connection, peer)| {
                    let label = self.labels.get(&connection).copied().unwrap_or(0);
                    with_label(connection_made((connection, peer)), label)
                });
                return Ok(made_or_failed(made));
            }
            // A test's server never drains: its connections are taken one at a time. And a test's
            // connection never waits, so it has no time to run out of.
            ("Server", "_stop_on", _) | ("Connection", "_reads_within", [_]) => return Ok(Value::Unit),
            ("Connection", "draining", []) => return Ok(Value::Bool(false)),
            // The label of a connection a task serving it makes its `Connection` with.
            ("Connection", "_label", []) => {
                return Ok(Value::Int(self.labels.get(&handle).copied().unwrap_or(0)));
            }
            ("Connection" | "UdpSocket" | "Server", "close", []) => {
                self.close(handle);
                return Ok(Value::Unit);
            }
            ("Connection" | "UdpSocket" | "Server", _, _) => return Err(wrong(&method, span)),
            _ => return Err(lost()),
        };
        Ok(done_or_failed(done))
    }

    /// What a test scripted for a request. A request it didn't script fails: nothing else could
    /// answer it.
    pub(crate) fn request(&mut self, request: &http::Request) -> Value {
        if let Some(refused) = request.hosts.as_ref().and_then(|hosts| http::outside(&request.url, hosts)) {
            return failed(&refused);
        }
        let sent = format!("{} {}", request.method, http::written(request));
        self.requests.push(sent.clone());
        match self.answers.get(&sent) {
            Some(answer) => Value::Ok(Rc::new(answer.clone())),
            None => failed(&Failure::Other(format!(
                "the test's fake `Net` has no answer for `{sent}`: give it one with `net.script`"
            ))),
        }
    }

    pub(crate) fn listen(&mut self, port: u16, tls: bool) -> Result<Made, Failure> {
        let fake = self;
        let port = i64::from(port);
        if fake.servers.values().any(|(taken, _)| *taken == port) {
            return Err(Failure::Other(format!("port {port} is already in use")));
        }
        // Port 0 asks for any free port, which the fake picks in order.
        let port = if port == 0 { 49152 + i64::try_from(fake.servers.len()).unwrap_or(0) } else { port };
        let handle = fake.handle();
        fake.servers.insert(handle, (port, VecDeque::new()));
        if tls {
            fake.secure.insert(handle);
        }
        Ok(("Server", vec![("_handle", Value::Int(handle)), ("port", Value::Int(port))]))
    }

    pub(crate) fn connect(&mut self, host: &str, port: u16, tls: bool) -> Result<Made, Failure> {
        let fake = self;
        let port = i64::from(port);
        if !FAKE_HOSTS.contains(&host) {
            return Err(Failure::Other(format!(
                "the test's fake `Net` reaches only the test's own servers, at `localhost`, not `{host}`"
            )));
        }
        let Some(server) =
            fake.servers.iter().find(|(_, (taken, _))| *taken == port).map(|(server, _)| *server)
        else {
            return Err(Failure::Refused(host.to_owned(), port));
        };
        if tls != fake.secure.contains(&server) {
            let (client, listening) = if tls { ("with", "without") } else { ("without", "with") };
            return Err(Failure::Tls(format!(
                "a connection {client} TLS reached a server on port {port} {listening} it"
            )));
        }
        let (client, other_end) = (fake.handle(), fake.handle());
        if tls {
            fake.secure.insert(client);
        }
        let stream = |peer| FakeStream { inbox: VecDeque::new(), peer, peer_closed: false };
        fake.streams.insert(client, stream(other_end));
        fake.streams.insert(other_end, stream(client));
        let client_peer = format!("localhost:{}", 50000 + client);
        if let Some((_, waiting)) = fake.servers.get_mut(&server) {
            waiting.push_back((other_end, client_peer));
        }
        Ok((
            "Connection",
            vec![("_handle", Value::Int(client)), ("peer", Value::str(&format!("localhost:{port}")))],
        ))
    }

    /// `connection.start_tls(host)` in a test: refused as a real one is, for what a real one can tell.
    fn start_tls(&mut self, handle: i64) -> Result<(), Failure> {
        let Some(stream) = self.streams.get(&handle) else { return Err(closed_here()) };
        if !stream.inbox.is_empty() {
            return Err(Failure::Tls(SENT_BEFORE_TLS.to_owned()));
        }
        if !self.secure.insert(handle) {
            return Err(Failure::Tls(ALREADY_TLS.to_owned()));
        }
        Ok(())
    }

    pub(crate) fn udp(&mut self, port: u16) -> Result<Made, Failure> {
        let fake = self;
        let port = i64::from(port);
        if fake.udp.values().any(|(taken, _)| *taken == port) {
            return Err(Failure::Other(format!("port {port} is already in use")));
        }
        // Port 0 asks for any free port, which the fake picks in order.
        let port = if port == 0 { 49152 + i64::try_from(fake.udp.len()).unwrap_or(0) } else { port };
        let handle = fake.handle();
        fake.udp.insert(handle, (port, VecDeque::new()));
        Ok(("UdpSocket", vec![("_handle", Value::Int(handle)), ("port", Value::Int(port))]))
    }

    /// The next packet for a socket in a test. With none waiting, none can come, since nothing
    /// else runs while a test waits: given `timeout`, it gives up as a real one would once that
    /// passed, and otherwise it fails as `accept` does, so a loop over the socket ends.
    pub(crate) fn receive(
        &mut self,
        handle: i64,
        timeout: Option<Duration>,
    ) -> Result<(Vec<u8>, String), Failure> {
        let fake = self;
        let Some((port, waiting)) = fake.udp.get_mut(&handle) else { return Err(closed_here()) };
        waiting.pop_front().ok_or_else(|| match timeout {
            Some(after) => Failure::TimedOut(after),
            None => Failure::Other(format!(
                "no packet is waiting for the test's fake socket on port {port}, and none can come while a test waits"
            )),
        })
    }

    /// Sends a packet in a test. Sent to a port nothing has, it's lost, as UDP's are.
    pub(crate) fn send_to(&mut self, handle: i64, data: &[u8], address: &str) -> Result<(), Failure> {
        let fake = self;
        let Some((from, _)) = fake.udp.get(&handle) else { return Err(closed_here()) };
        let sender = format!("localhost:{from}");
        let Some((host, port)) = address.rsplit_once(':') else {
            return Err(Failure::Other(format!("`{address}` isn't an address like `host:port`")));
        };
        if !FAKE_HOSTS.contains(&host) {
            return Err(Failure::Other(format!(
                "the test's fake `Net` reaches only the test's own sockets, at `localhost`, not `{host}`"
            )));
        }
        if let Some((_, waiting)) = fake.udp.values_mut().find(|(taken, _)| port.parse() == Ok(*taken)) {
            waiting.push_back((data.to_vec(), sender));
        }
        Ok(())
    }

    /// The next connection waiting for a server in a test. With none waiting, none can come,
    /// since nothing else runs while a test waits, so the server is done and its loop ends.
    pub(crate) fn accept(&mut self, handle: i64) -> Result<(i64, String), Failure> {
        let fake = self;
        let Some((port, waiting)) = fake.servers.get_mut(&handle) else { return Err(closed_here()) };
        let accepted = waiting.pop_front().ok_or_else(|| {
            Failure::Other(format!(
                "no connection is waiting for the test's fake server on port {port}, and none can come while a test waits"
            ))
        })?;
        fake.labels.insert(accepted.0, next_label(0));
        Ok(accepted)
    }

    /// A read of `count` bytes in a test that's given `timeout=`, which gives up as `or_timed_out`
    /// says.
    fn read_by(
        &mut self,
        handle: i64,
        count: Option<usize>,
        timeout: Option<Duration>,
        span: Span,
    ) -> Result<Result<Option<Vec<u8>>, Failure>, Diagnostic> {
        or_timed_out(self.read(handle, count, span), timeout)
    }

    /// The next `count` bytes, or with no count the next line, of a connection in a test.
    pub(crate) fn read(
        &mut self,
        handle: i64,
        count: Option<usize>,
        span: Span,
    ) -> Result<Result<Option<Vec<u8>>, Failure>, Diagnostic> {
        let fake = self;
        let Some(stream) = fake.streams.get_mut(&handle) else { return Ok(Err(closed_here())) };
        let take = match count {
            Some(count) => count.min(stream.inbox.len()),
            None => stream.inbox.iter().position(|byte| *byte == b'\n').map_or(0, |end| end + 1),
        };
        if take > 0 || count == Some(0) {
            return Ok(Ok(Some(stream.inbox.drain(..take).collect())));
        }
        if stream.peer_closed {
            // What's left is the last line, or there's nothing more.
            let rest: Vec<u8> = stream.inbox.drain(..).collect();
            return Ok(Ok(match count {
                Some(_) => Some(rest),
                None => (!rest.is_empty()).then_some(rest),
            }));
        }
        let wanted = if count.is_some() { "anything" } else { "a whole line" };
        Err(waits_forever(
            &format!(
                "this connection in the test's fake network would wait forever for {wanted}: write to its other end first"
            ),
            span,
        ))
    }

    /// A line from a test's connection, refused as a real one is: too long, or a bare `\n` with `crlf`.
    fn read_line(
        &mut self,
        handle: i64,
        crlf: bool,
        span: Span,
    ) -> Result<Result<Value, Failure>, Diagnostic> {
        if let Some(stream) = self.streams.get(&handle)
            && stream.inbox.len() > MOST_LINE_BYTES
            && !stream.inbox.iter().take(MOST_LINE_BYTES + 1).any(|byte| *byte == b'\n')
        {
            return Ok(Err(Failure::Other(line_too_long())));
        }
        let line = self.read(handle, None, span)?;
        Ok(line.and_then(|line| match line.as_deref().and_then(|line| line_refused(line, crlf)) {
            Some(why) => Err(Failure::Other(why)),
            None => Ok(line_value(line.as_deref())),
        }))
    }

    pub(crate) fn write(&mut self, handle: i64, data: &[u8]) -> Result<(), Failure> {
        let fake = self;
        let Some(stream) = fake.streams.get(&handle) else { return Err(closed_here()) };
        if stream.peer_closed {
            return Err(Failure::Closed);
        }
        let peer = stream.peer;
        if let Some(other) = fake.streams.get_mut(&peer) {
            other.inbox.extend(data);
        }
        Ok(())
    }

    /// Closes a connection or a server in a test. The other end of a connection reads what's
    /// left and then the end.
    pub(crate) fn close(&mut self, handle: i64) {
        let fake = self;
        fake.udp.remove(&handle);
        if let Some((_, waiting)) = fake.servers.remove(&handle) {
            for (served, _) in waiting {
                fake.close_stream(served);
            }
        }
        fake.close_stream(handle);
    }
}

/// What a call made: a `Connection`, `Server` or `UdpSocket`, by its type's name and fields.
type Made = (&'static str, Vec<(&'static str, Value)>);

/// How a network call failed, which `NetError` says.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    NotFound(String),
    Refused(String, i64),
    TimedOut(Duration),
    Closed,
    Tls(String),
    /// A narrowed `Net` doesn't reach that host, or can't do that.
    Denied(String),
    Other(String),
}

/// The failure of using a socket this end closed already: a handle's only taken out of the open
/// ones by its own `close()`, which a `with` block's end calls. Its other side closing is
/// `Failure::Closed`.
fn closed_here() -> Failure {
    Failure::Other("the socket was closed here, so it can't be used any more".to_owned())
}

/// How many writes a run asked of its connections, as `FENNEC_COUNTS` says: a server's response
/// or a client's request should go out in one, so it goes in one packet, not a head and then a body
/// the other side waits for.
static CONNECTION_WRITES: AtomicU64 = AtomicU64::new(0);

/// A connection is written to, counted under `FENNEC_COUNTS`; a run that doesn't count pays only
/// for looking once whether it does.
pub(crate) fn wrote_to_a_connection() {
    static COUNTING: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *COUNTING.get_or_init(|| std::env::var_os("FENNEC_COUNTS").is_some()) {
        CONNECTION_WRITES.fetch_add(1, Ordering::Relaxed);
    }
}

/// How many writes the run asked of its connections, while counting.
pub(crate) fn connection_writes() -> u64 {
    CONNECTION_WRITES.load(Ordering::Relaxed)
}

impl Failure {
    /// The failure an I/O error on an open connection means.
    pub(crate) fn of(error: &std::io::Error, waited: Option<Duration>) -> Failure {
        match error.kind() {
            ErrorKind::WouldBlock | ErrorKind::TimedOut => Failure::TimedOut(waited.unwrap_or_default()),
            ErrorKind::BrokenPipe | ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted => {
                Failure::Closed
            }
            _ => Failure::Other(error.to_string()),
        }
    }
}

/// A random handle for a real socket, which nothing could guess. `_handle` is private, so code
/// outside `std.net` can't make a `Connection` or `Server` at all
/// (`examples/errors/net_forged_handle`). A handle is also looked up in the program's own
/// table, so one this program wasn't given reaches nothing, and the randomness is a third wall.
fn fresh_handle(taken: &HashMap<i64, Socket>) -> i64 {
    loop {
        let mut word = [0; 8];
        if getrandom::fill(&mut word).is_err() {
            return i64::try_from(taken.len()).unwrap_or(i64::MAX) + 1;
        }
        let handle = i64::from_le_bytes(word) & i64::MAX;
        if !taken.contains_key(&handle) {
            return handle;
        }
    }
}

pub(crate) fn duration_of(value: &Value, span: Span) -> Result<Duration, Diagnostic> {
    if let Value::Instance(duration) = value
        && let Some(Value::Int(nanos)) = duration.field("nanos")
    {
        return Ok(Duration::from_nanos(u64::try_from(*nanos).unwrap_or(0)));
    }
    Err(Diagnostic::new("`timeout` should be a `Duration`", span))
}

fn port_of(port: i64, span: Span) -> Result<u16, Diagnostic> {
    u16::try_from(port)
        .map_err(|_| Diagnostic::new(format!("{port} isn't a port: ports go from 0 to 65535"), span))
}

fn wrong(method: &str, span: Span) -> Diagnostic {
    Diagnostic::new(format!("`{method}` was called with the wrong arguments"), span)
}

fn waits_forever(what: &str, span: Span) -> Diagnostic {
    Diagnostic::new(format!("{what}, and nothing else runs while a test waits"), span)
}

/// `net.connect(host, port)`, `net.listen(port)` and `net.udp()` on the real network.
pub(crate) fn real_net(
    method: &str,
    args: &[Value],
    timeout: Option<Duration>,
    span: Span,
) -> Result<Value, Diagnostic> {
    let found = match (method, args) {
        ("_connect", [Value::Str(host), Value::Int(port)]) => {
            connect(host, port_of(*port, span)?, timeout.unwrap_or(CONNECT_TIMEOUT), false)
        }
        // `tls=True`, which native code keeps on the method's name.
        ("_connect.tls" | "_connect", [Value::Str(host), Value::Int(port), Value::Bool(tls)]) => {
            connect(host, port_of(*port, span)?, timeout.unwrap_or(CONNECT_TIMEOUT), *tls)
        }
        ("_listen", [Value::Int(port)]) => listen(port_of(*port, span)?, None),
        // `tls=Certificate(...)`, or `None`, which native code keeps on the method's name.
        ("_listen.tls" | "_listen", [Value::Int(port), certificate]) => {
            match tls_server(certificate, span)? {
                Ok(tls) => listen(port_of(*port, span)?, tls),
                Err(failure) => Err(failure),
            }
        }
        ("_udp", []) => udp(0),
        // Native code keeps a named argument on the method's name.
        ("_udp" | "_udp.port", [Value::Int(port)]) => udp(port_of(*port, span)?),
        ("_connect" | "_connect.tls" | "_listen" | "_listen.tls" | "_udp" | "_udp.port", _) => {
            return Err(wrong(method, span));
        }
        _ => return Err(no_method(&Value::Capability(Capability::Net), method, span)),
    };
    Ok(made_or_failed(found))
}

/// Whether `tls=` on a test's `net.listen` gives a certificate. A value that isn't one is the program's mistake.
fn has_certificate(certificate: &Value, span: Span) -> Result<bool, Diagnostic> {
    match certificate {
        Value::None => Ok(false),
        Value::Instance(instance) if &*instance.type_name == "Certificate" => Ok(true),
        _ => Err(Diagnostic::new("`tls=` takes a `Certificate`", span)),
    }
}

/// The TLS a `Certificate` makes, or `None` for `tls=None`. A `Certificate` that isn't one is the
/// program's mistake, and a chain or key that can't be read is its failure.
fn tls_server(
    certificate: &Value,
    span: Span,
) -> Result<Result<Option<Arc<dyn TlsServer>>, Failure>, Diagnostic> {
    let instance = match certificate {
        Value::None => return Ok(Ok(None)),
        Value::Instance(instance) if &*instance.type_name == "Certificate" => instance,
        _ => return Err(Diagnostic::new("`tls=` takes a `Certificate`", span)),
    };
    let Some(Value::Str(chain)) = instance.field("chain") else {
        return Err(Diagnostic::new("this `Certificate` has no `chain`", span));
    };
    let key = match instance.field("key") {
        Some(Value::Instance(secret)) => secret.field("_value"),
        _ => None,
    };
    let Some(Value::Str(key)) = key else {
        return Err(Diagnostic::new("this `Certificate` has no `key`", span));
    };
    Ok(http::server_tls(chain, key).map(Some))
}

/// A method of a real connection, by its handle.
pub(crate) fn real_connection(
    handle: i64,
    method: &str,
    args: &[Value],
    timeout: Option<Duration>,
    span: Span,
) -> Result<Value, Diagnostic> {
    let done = match (method, args) {
        ("read", [Value::Int(count)]) => {
            read(handle, bytes_asked(*count, span)?, timeout).map(|data| Value::Bytes(Rc::from(data)))
        }
        ("_room_for_body", [Value::Int(size)]) => {
            return Ok(Value::Bool(room_for_body(handle, bytes_asked(*size, span)?)));
        }
        ("_body", [Value::Int(size)]) => {
            body(handle, bytes_asked(*size, span)?, timeout).map(|data| Value::Bytes(Rc::from(data)))
        }
        ("read_line", []) => read_line(handle, timeout).map(|line| line_value(line.as_deref())),
        ("read_line.crlf", [Value::Bool(crlf)]) => {
            read_line_crlf(handle, timeout, *crlf).map(|line| line_value(line.as_deref()))
        }
        ("_read_head", given) if let Some(limits) = head::Limits::of(given) => {
            head::read_lines(limits, || {
                let line = read_line_crlf(handle, timeout, true)?;
                Ok(line.map(|line| String::from_utf8_lossy(trim_line(&line)).into_owned()))
            })
        }
        ("start_tls", [Value::Str(host)]) => {
            start_tls(handle, host, timeout.unwrap_or(CONNECT_TIMEOUT)).map(|()| Value::Unit)
        }
        ("write", [data @ (Value::Str(_) | Value::Bytes(_))]) => {
            write_within(handle, data_of(data), timeout).map(|()| Value::Unit)
        }
        ("_write_parts", [head @ Value::Str(_), body @ Value::Bytes(_)]) => {
            write_within(handle, &parts_of(head, body), timeout).map(|()| Value::Unit)
        }
        ("close", []) => {
            close(handle);
            return Ok(Value::Unit);
        }
        // What a task serving a connection makes its `Connection` from: its peer, and its label.
        ("peer", []) => return Ok(stream(handle).map_or_else(|_| Value::str(""), |it| Value::str(&it.peer))),
        ("_label", []) => return Ok(Value::Int(stream(handle).map_or(0, |stream| stream.label))),
        // Has every read from now on done within `after`.
        // As a server starts on a next request: the last one's body is let go of too.
        ("_reads_within", [after]) => {
            let after = duration_of(after, span)?;
            if let Ok(stream) = stream(handle) {
                *stream.reads_by.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some((Instant::now() + after, after));
                stream.body_held.give_back();
            }
            return Ok(Value::Unit);
        }
        // Its server's draining, when it was told to `stop_on` signals and one has come.
        ("draining", []) => {
            let draining = PLAIN_DRAINS.load(Ordering::SeqCst) && crate::signals::stopping();
            return Ok(Value::Bool(draining && stream(handle).is_ok_and(|stream| stream.served)));
        }
        _ => return Err(wrong(method, span)),
    };
    Ok(done_or_failed(done))
}

/// A method of a real server, by its handle.
pub(crate) fn real_server(
    handle: i64,
    method: &str,
    args: &[Value],
    timeout: Option<Duration>,
    span: Span,
) -> Result<Value, Diagnostic> {
    match (method, args) {
        ("_accept", []) => Ok(made_or_failed(accept(handle, timeout).map(connection_made))),
        ("close", []) => {
            close(handle);
            Ok(Value::Unit)
        }
        ("_stop_on", [grace]) => {
            stop_on(handle, duration_of(grace, span)?);
            Ok(Value::Unit)
        }
        _ => Err(wrong(method, span)),
    }
}

/// A method of a real UDP socket, by its handle.
pub(crate) fn real_udp(
    handle: i64,
    method: &str,
    args: &[Value],
    timeout: Option<Duration>,
    span: Span,
) -> Result<Value, Diagnostic> {
    let done = match (method, args) {
        ("receive", []) => receive(handle, timeout).map(packet_value),
        ("send_to", [data @ (Value::Str(_) | Value::Bytes(_)), Value::Str(address)]) => {
            send_to(handle, data_of(data), address).map(|()| Value::Unit)
        }
        ("close", []) => {
            close(handle);
            return Ok(Value::Unit);
        }
        _ => return Err(wrong(method, span)),
    };
    Ok(done_or_failed(done))
}

/// A request from what a call gave: its URL, with the method first for `net.request`, and its
/// named arguments.
fn request_of(
    method: &str,
    plain: &[Value],
    named: &[(&str, &Value)],
    span: Span,
) -> Result<http::Request, Diagnostic> {
    let (method, url) = match (method, plain) {
        ("get", [Value::Str(url)]) => ("GET".to_owned(), url),
        ("post", [Value::Str(url)]) => ("POST".to_owned(), url),
        ("request", [Value::Str(method), Value::Str(url)]) => (method.to_ascii_uppercase(), url),
        _ => return Err(wrong(method, span)),
    };
    let mut request = http::Request {
        method,
        url: url.to_string(),
        params: Vec::new(),
        headers: Vec::new(),
        body: Vec::new(),
        timeout: CONNECT_TIMEOUT,
        most_body: http::MOST_BODY,
        hosts: None,
    };
    let sends_json = named.iter().any(|(name, _)| *name == "json");
    for (name, value) in named {
        match (*name, value) {
            ("timeout", value) => request.timeout = duration_of(value, span)?,
            ("most_body", Value::Int(most)) => {
                request.most_body = usize::try_from(*most).map_err(|_| {
                    Diagnostic::new(format!("`most_body` is a number of bytes, not {most}"), span)
                })?;
            }
            // Native code and `http_method` both give `json=` as the JSON text already.
            ("body" | "json", Value::Str(text)) => request.body = text.as_bytes().to_vec(),
            ("body", Value::Bytes(data)) => request.body = data.to_vec(),
            ("params", Value::Dict(pairs)) => request.params = text_pairs(pairs, span)?,
            ("headers", Value::Dict(pairs)) => request.headers = text_pairs(pairs, span)?,
            // What a narrowed `Net` reaches, for its client to hold the URL and each redirect to.
            ("_hosts", Value::List(hosts)) => {
                request.hosts = Some(
                    hosts
                        .iter()
                        .filter_map(
                            |host| if let Value::Str(host) = host { Some(host.to_string()) } else { None },
                        )
                        .collect(),
                );
            }
            (other, _) => {
                return Err(Diagnostic::new(format!("this has no parameter called `{other}`"), span));
            }
        }
    }
    if sends_json && !request.headers.iter().any(|(name, _)| name.eq_ignore_ascii_case("content-type")) {
        request.headers.push(("content-type".to_owned(), "application/json".to_owned()));
    }
    Ok(request)
}

fn text_pairs(pairs: &crate::pairs::Pairs, span: Span) -> Result<Vec<(String, String)>, Diagnostic> {
    pairs
        .iter()
        .map(|(key, value)| match (key, value) {
            (Value::Str(key), Value::Str(value)) => Ok((key.to_string(), value.to_string())),
            _ => Err(Diagnostic::new("a request's headers and params are text", span)),
        })
        .collect()
}

/// An answer as a `Response`.
fn answer_value(answer: http::Answer) -> Value {
    // A dict has each name once: a header sent again adds to the one there, as HTTP says it can.
    // Each name's place is found by its hash, so many headers are joined in time that grows with
    // how many there are.
    let mut joined: Vec<(String, String)> = Vec::new();
    let mut places: HashMap<&str, usize> = HashMap::new();
    let mut cookies = Vec::new();
    for (name, value) in &answer.headers {
        if name == "set-cookie" {
            cookies.push(Value::str(value));
        }
        if let Some(&at) = places.get(name.as_str()) {
            if let Some((_, held)) = joined.get_mut(at) {
                held.push_str(", ");
                held.push_str(value);
            }
        } else {
            places.insert(name, joined.len());
            joined.push((name.clone(), value.clone()));
        }
    }
    let headers: Vec<(Value, Value)> =
        joined.iter().map(|(name, value)| (Value::str(name), Value::str(value))).collect();
    Value::made(
        "Response",
        vec![
            ("status", Value::Int(answer.status)),
            ("headers", Value::Dict(Rc::new(headers.into()))),
            ("body", Value::Bytes(Rc::from(answer.body))),
            ("set_cookies", Value::list(cookies)),
        ],
    )
}

/// What `Net` does for native code, which keeps a call's named arguments on the method's name, in
/// the order they're written: `get.params.timeout`.
pub(crate) fn native_net(name: &str, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
    let mut parts = name.split('.');
    let method = parts.next().unwrap_or_default();
    if matches!(method, "get" | "post" | "request") {
        let named: Vec<&str> = parts.collect();
        let (plain, last) = args.split_at(args.len().saturating_sub(named.len()));
        let given: Vec<(&str, &Value)> = named.iter().copied().zip(last).collect();
        let request = request_of(method, plain, &given, span)?;
        // A task parks while its request waits, rather than holding its worker: a server in the
        // same program may need that worker to answer it. `send` sees to that itself.
        return Ok(done_or_failed(http::send(&request).map(answer_value)));
    }
    let (method, args, timeout) = timed(name, args, span)?;
    real_net(&method, &args, timeout, span)
}

/// A `Net` method native code names, with its arguments in the order its parameters are, and
/// without a timeout, which a test's fake network never waits for: `udp.port` is `udp` with the
/// port given by name, and `connect.port.host` has its two the other way round.
fn in_order<'a>(name: &'a str, args: &[Value]) -> (&'a str, Vec<Value>) {
    let mut parts = name.split('.');
    let method = parts.next().unwrap_or_default();
    let named: Vec<&str> = parts.collect();
    let (plain, last) = args.split_at(args.len().saturating_sub(named.len()));
    let mut ordered = plain.to_vec();
    let params: &[&str] = match method {
        "_connect" => &["host", "port", "tls"],
        "_listen" => &["port", "tls"],
        "_udp" => &["port"],
        _ => &[],
    };
    for param in params.iter().skip(plain.len()) {
        if let Some(at) = named.iter().position(|name| name == param)
            && let Some(value) = last.get(at)
        {
            ordered.push(value.clone());
        }
    }
    (method, ordered)
}

/// A method native code names, its arguments and how long it may wait: `read_line.timeout` is
/// `read_line` with `timeout=` given last. Named arguments are on the name in the order they were
/// written, so `timeout=` can come before another, as in `read_line.timeout.crlf`, which is
/// `read_line.crlf` with what `crlf=` was given.
pub(crate) fn timed(
    method: &str,
    args: &[Value],
    span: Span,
) -> Result<(String, Vec<Value>, Option<Duration>), Diagnostic> {
    const NAMED: [&str; 5] = ["timeout", "crlf", "tls", "port", "host"];
    // The named arguments are the parts at the name's end that name one, at most as many as there
    // are arguments, which are the last arguments in the same order. They're read from the end,
    // in place: every call on a socket comes through here, and splitting its name into a list and
    // joining it again cost a fifth of what a served request's calls cost.
    let (mut named, mut end) = (0, method.len());
    // The `timeout` nearest the start, as how many named parts come after it, and its bytes.
    let mut timeout = None;
    while named < args.len() {
        let start = method.get(..end).and_then(|before| before.rfind('.')).map_or(0, |dot| dot + 1);
        let part = method.get(start..end).unwrap_or_default();
        if !NAMED.contains(&part) {
            break;
        }
        if part == "timeout" {
            timeout = Some((named, start, end));
        }
        named += 1;
        if start == 0 {
            break;
        }
        end = start - 1;
    }
    let Some((after, start, end)) = timeout else {
        return Ok((method.to_owned(), args.to_vec(), None));
    };
    let mut args = args.to_vec();
    let given = args.remove(args.len() - 1 - after);
    // The part goes with the dot before it, or, first in the name, with the one after it.
    let (cut_from, cut_to) = if start > 0 { (start - 1, end) } else { (start, (end + 1).min(method.len())) };
    let name =
        [method.get(..cut_from).unwrap_or_default(), method.get(cut_to..).unwrap_or_default()].concat();
    Ok((name, args, Some(duration_of(&given, span)?)))
}

pub(crate) fn bytes_asked(count: i64, span: Span) -> Result<usize, Diagnostic> {
    usize::try_from(count).map_err(|_| Diagnostic::new(format!("`read` can't give {count} bytes"), span))
}

pub(crate) fn data_of(data: &Value) -> &[u8] {
    match data {
        Value::Str(text) => text.as_bytes(),
        Value::Bytes(data) => data,
        _ => &[],
    }
}

/// What `conn._write_parts(head, body)` writes: a response's head as text, then its body, sent
/// in one write as `conn.write(head.encode() + body)` was, with neither made into a block of
/// its own to be joined.
pub(crate) fn parts_of(head: &Value, body: &Value) -> Vec<u8> {
    let (head, body) = (data_of(head), data_of(body));
    let mut parts = Vec::with_capacity(head.len() + body.len());
    parts.extend_from_slice(head);
    parts.extend_from_slice(body);
    parts
}

/// The most bytes a line read from a connection may hold, its ending included. A line longer
/// than this fails, rather than take as much memory as whoever's sending it likes.
pub(crate) const MOST_LINE_BYTES: usize = 1 << 20;

/// What a line too long to read fails with, the same words wherever it's read.
pub(crate) fn line_too_long() -> String {
    format!("a line was longer than {MOST_LINE_BYTES} bytes")
}

/// Whether a read failed on a line too long to read, as `_read_head` tells a request line too
/// long from a header too long.
pub(crate) fn is_line_too_long(failure: &Failure) -> bool {
    matches!(failure, Failure::Other(why) if *why == line_too_long())
}

/// Why a line read fails: it's too long, or, with `crlf`, it ended with no `\r` before its `\n`.
pub(crate) fn line_refused(line: &[u8], crlf: bool) -> Option<String> {
    if line.len() > MOST_LINE_BYTES {
        return Some(line_too_long());
    }
    let bare =
        line.last() == Some(&b'\n') && line.len().checked_sub(2).and_then(|at| line.get(at)) != Some(&b'\r');
    (crlf && bare).then(|| "a line ended without a carriage return".to_owned())
}

pub(crate) fn line_value(line: Option<&[u8]>) -> Value {
    line.map_or(Value::None, |line| Value::str(String::from_utf8_lossy(trim_line(line)).as_ref()))
}

/// `line_value`, for a line whose ending's already been taken off, as a served connection reads
/// one: taking off another `\r` would make a line holding a bare `\r` look blank.
#[cfg(any(test, feature = "threads"))]
pub(crate) fn trimmed_line_value(line: Option<&[u8]>) -> Value {
    line.map_or(Value::None, |line| Value::str(String::from_utf8_lossy(line).as_ref()))
}

fn connection_made((connection, peer): (i64, String)) -> Made {
    let made = ("Connection", vec![("_handle", Value::Int(connection)), ("peer", Value::str(&peer))]);
    with_label(made, stream(connection).map_or(0, |stream| stream.label))
}

/// A connection with the label it was given as its server accepted it. One with none yet, 0, gets
/// the next as it's made.
fn with_label((type_name, mut fields): Made, label: i64) -> Made {
    if label > 0 {
        fields.push(("_label", Value::Int(label)));
    }
    (type_name, fields)
}

fn packet_value((data, sender): (Vec<u8>, String)) -> Value {
    Value::made("Packet", vec![("data", Value::Bytes(Rc::from(data))), ("sender", Value::str(&sender))])
}

/// The labels the program has given its connections, servers and UDP sockets, each counted on
/// its own. A value's label is what it prints as, `Connection(#3)`. Its handle is random, so that
/// nothing could guess it, and printing it would show a different number every run.
static LABELS: [AtomicI64; 3] = [AtomicI64::new(0), AtomicI64::new(0), AtomicI64::new(0)];

/// Starts the labels from 1 again, for a test, so what it prints doesn't depend on the tests that
/// ran before it.
pub(crate) fn labels_start_over() {
    for count in &LABELS {
        count.store(0, Ordering::Relaxed);
    }
}

/// A network value with its label: the next of its kind's.
fn labelled((type_name, mut fields): Made) -> Made {
    let kind = match type_name {
        "Connection" => 0,
        "Server" => 1,
        "UdpSocket" => 2,
        _ => return (type_name, fields),
    };
    if fields.iter().any(|(name, _)| *name == "_label") {
        return (type_name, fields);
    }
    fields.push(("_label", Value::Int(next_label(kind))));
    (type_name, fields)
}

/// The next label of the kind numbered `kind`: 0 for connections, 1 servers, 2 UDP sockets.
pub(crate) fn next_label(kind: usize) -> i64 {
    LABELS.get(kind).map_or(0, |count| count.fetch_add(1, Ordering::Relaxed) + 1)
}

fn made_or_failed(found: Result<Made, Failure>) -> Value {
    done_or_failed(found.map(|made| {
        let (type_name, fields) = labelled(made);
        Value::made(type_name, fields)
    }))
}

pub(crate) fn done_or_failed(done: Result<Value, Failure>) -> Value {
    match done {
        Ok(value) => Value::Ok(Rc::new(value)),
        Err(failure) => failed(&failure),
    }
}

/// The `Err` holding the case of `NetError` a failure is.
pub(crate) fn failed(failure: &Failure) -> Value {
    let (case, fields) = match failure {
        Failure::NotFound(host) => ("NotFound", vec![("host", Value::str(host))]),
        Failure::Refused(host, port) => {
            ("Refused", vec![("host", Value::str(host)), ("port", Value::Int(*port))])
        }
        Failure::TimedOut(after) => {
            let nanos = i64::try_from(after.as_nanos()).unwrap_or(i64::MAX);
            ("TimedOut", vec![("after", Value::made("Duration", vec![("nanos", Value::Int(nanos))]))])
        }
        Failure::Closed => ("Closed", Vec::new()),
        Failure::Tls(reason) => ("Tls", vec![("reason", Value::str(reason))]),
        Failure::Denied(reason) => ("Denied", vec![("reason", Value::str(reason))]),
        Failure::Other(reason) => ("Other", vec![("reason", Value::str(reason))]),
    };
    Value::Err(Rc::new(Value::made_case("NetError", case, fields)))
}

fn connect(host: &str, port: u16, timeout: Duration, tls: bool) -> Result<Made, Failure> {
    // A task that can park does so while it connects and through its handshake, on a socket that
    // doesn't block from the start: only a name's lookup holds a thread that may block. All
    // three once ran on such a thread, for as long as the network and the other side took.
    if task_parks() {
        let (stream, peer) = connected_parked(host, port, timeout, None)?;
        let secured = if tls {
            let deadline = Instant::now() + timeout;
            let mut waits = |socket: &TcpStream, wanted: Wanted| parked_out_on(socket, wanted, deadline);
            Some(secured(&stream, host, timeout, Some(&mut waits))?)
        } else {
            None
        };
        let handle = keep_stream_made(stream, peer.clone(), secured, (false, true))?;
        return Ok(connection_made((handle, peer)));
    }
    let (stream, peer) = connected(host, port, timeout)?;
    let secured = if tls { Some(secured(&stream, host, timeout, None)?) } else { None };
    let handle = keep_stream(stream, peer.clone(), secured)?;
    Ok(connection_made((handle, peer)))
}

/// TLS over a connection just made, its certificate checked against what the system trusts. With
/// something to wait by, the connection's socket doesn't block, and the handshake waits that way.
fn secured(
    stream: &TcpStream,
    host: &str,
    timeout: Duration,
    waits: Option<Waits<'_>>,
) -> Result<Box<dyn SecureStream>, Failure> {
    let tcp = stream.try_clone().map_err(|error| Failure::Other(error.to_string()))?;
    if waits.is_some() {
        // On every system, the copy TLS reads and writes through doesn't block either.
        tcp.set_nonblocking(true).map_err(|error| Failure::Other(error.to_string()))?;
    }
    http::raw_secured(tcp, host, timeout, waits)
}

/// A connection to `host` on `port` made by a task that can park, on a socket that doesn't block
/// from the start. Each address gets `timeout`, or all of them share what's left until `by`.
///
/// Looking a name up can only be done by blocking, so that alone runs on a thread of its own, and
/// the task waits for it no longer than the connect's `timeout`, or until `by`: past that, it fails
/// as timed out, as a connect does. An address written out needs no looking up. Then the connect is
/// started and the task parks until the socket says how it went.
#[cfg(any(test, feature = "threads"))]
pub(crate) fn connected_parked(
    host: &str,
    port: u16,
    timeout: Duration,
    by: Option<Instant>,
) -> Result<(TcpStream, String), Failure> {
    let addresses: Vec<std::net::SocketAddr> = if let Ok(address) = host.parse::<std::net::IpAddr>() {
        vec![std::net::SocketAddr::new(address, port)]
    } else {
        let name = host.to_owned();
        let deadline = by.unwrap_or_else(|| Instant::now() + timeout);
        crate::embed::serve::off_thread_until(deadline, move || looked_up(&name, port))
            .ok_or(Failure::TimedOut(timeout))?
            .map_err(|_| Failure::NotFound(host.to_owned()))?
    };
    let stream = first_connected((host, port), &addresses, (timeout, by), connected_by)?;
    let peer = stream.peer_addr().map_or_else(|_| format!("{host}:{port}"), |peer| peer.to_string());
    Ok((stream, peer))
}

/// `host`'s addresses for `port`, waited for until `deadline` by a thread that can't park, and
/// failing as timed out after `timeout` past that. An address written out needs no looking up. A
/// name's lookup can only be done by blocking, for as long as the system's resolver takes, so it's
/// done on the lookups' pool, as a parked task's is, which finishes it if it's given up on, its
/// answer dropped. It once ran here, and held the connect past its timeout for as long as that
/// took (#1826).
pub(crate) fn looked_up_by(
    host: &str,
    port: u16,
    deadline: Instant,
    timeout: Duration,
) -> Result<Vec<std::net::SocketAddr>, Failure> {
    if let Ok(address) = host.parse::<std::net::IpAddr>() {
        return Ok(vec![std::net::SocketAddr::new(address, port)]);
    }
    let name = host.to_owned();
    crate::embed::blocking::waited_until(deadline, move || looked_up(&name, port))
        .ok_or(Failure::TimedOut(timeout))?
        .map_err(|_| Failure::NotFound(host.to_owned()))
}

/// `name`'s addresses, as the system looks them up, for `port`. In tests, a name under
/// `stalls.test` is one whose name server never answers, until the test lets it.
fn looked_up(name: &str, port: u16) -> std::io::Result<Vec<std::net::SocketAddr>> {
    #[cfg(test)]
    if name.ends_with(".stalls.test") {
        tests::stalls::stalled(name);
        return Err(ErrorKind::NotFound.into());
    }
    (name, port).to_socket_addrs().map(Iterator::collect)
}

/// How long one of a name's addresses is tried before the next, when it has more than one: RFC
/// 8305's delay between connection attempts.
const NEXT_ADDRESS_AFTER: Duration = Duration::from_millis(250);

/// The first of `host`'s `addresses` that `attempt` connects to by the deadline it's given. Each
/// address has `timeout`, or all of them share what's left until `by`.
///
/// A name with more than one address tries each for `NEXT_ADDRESS_AFTER` at most before going on
/// to the next. Otherwise one that never answers, or is refused slowly, uses up all the time there
/// is before the next is tried. Windows refuses a connect to a port nothing listens on only after
/// trying again for about 2 seconds, so `localhost`, which it looks up as `::1` first, timed out
/// for a server listening only on `127.0.0.1`. If none connects, those that only ran out of their
/// short time are tried again, one at a time, with all the time left.
///
/// The address that connects is remembered for the name, as RFC 6555 asks, and tried first the
/// next time (see `answered_first`). Otherwise each of a chain of redirects to `localhost` paid
/// that short try again, and ten of them used up a 1 second timeout on Windows.
pub(crate) fn first_connected<T>(
    (host, port): (&str, u16),
    addresses: &[std::net::SocketAddr],
    (timeout, by): (Duration, Option<Instant>),
    mut attempt: impl FnMut(std::net::SocketAddr, Instant) -> std::io::Result<T>,
) -> Result<T, Failure> {
    let failed = |error: &std::io::Error| match error.kind() {
        ErrorKind::ConnectionRefused => Failure::Refused(host.to_owned(), i64::from(port)),
        ErrorKind::TimedOut | ErrorKind::WouldBlock => Failure::TimedOut(timeout),
        _ => Failure::Other(error.to_string()),
    };
    let ran_out =
        |error: &std::io::Error| matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock);
    let several = addresses.len() > 1;
    let mut failure = Failure::NotFound(host.to_owned());
    let mut slow = Vec::new();
    let connected = |address: std::net::SocketAddr, made| {
        if several {
            remember(host, address.ip());
        }
        Ok(made)
    };
    for address in answered_first(host, addresses) {
        let now = Instant::now();
        let deadline = by.unwrap_or(now + timeout);
        if deadline <= now {
            return Err(Failure::TimedOut(timeout));
        }
        let short = now + NEXT_ADDRESS_AFTER;
        let cut_short = several && short < deadline;
        match attempt(address, if cut_short { short } else { deadline }) {
            Ok(made) => return connected(address, made),
            Err(error) if cut_short && ran_out(&error) => {
                failure = failed(&error);
                slow.push(address);
            }
            Err(error) => failure = failed(&error),
        }
    }
    for address in slow {
        let now = Instant::now();
        let deadline = by.unwrap_or(now + timeout);
        if deadline <= now {
            return Err(Failure::TimedOut(timeout));
        }
        match attempt(address, deadline) {
            Ok(made) => return connected(address, made),
            Err(error) => failure = failed(&error),
        }
    }
    Err(failure)
}

/// For each name with more than one address, the one that last connected. A process asks few
/// names, so past `MOST_ANSWERED` it starts again rather than choosing which to forget.
static ANSWERED: LazyLock<Mutex<HashMap<String, std::net::IpAddr>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
const MOST_ANSWERED: usize = 256;

/// `host`'s addresses with the one that last connected for it first, the rest in the order they
/// came. One that's stopped answering costs its short try once, and the next to connect is
/// remembered in its place.
fn answered_first(host: &str, addresses: &[std::net::SocketAddr]) -> Vec<std::net::SocketAddr> {
    let mut ordered = addresses.to_vec();
    if ordered.len() > 1 {
        let answered = ANSWERED.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(ip) = answered.get(&host.to_ascii_lowercase()) {
            ordered.sort_by_key(|address| address.ip() != *ip);
        }
    }
    ordered
}

/// Remembers that `ip` connected for `host`.
fn remember(host: &str, ip: std::net::IpAddr) {
    let mut answered = ANSWERED.lock().unwrap_or_else(PoisonError::into_inner);
    let host = host.to_ascii_lowercase();
    if answered.len() >= MOST_ANSWERED && !answered.contains_key(&host) {
        answered.clear();
    }
    answered.insert(host, ip);
}

/// A connection to one address, blocking its thread until it's made or `deadline` passes.
pub(crate) fn connected_until(
    address: std::net::SocketAddr,
    deadline: Instant,
) -> std::io::Result<TcpStream> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(ErrorKind::TimedOut.into());
    }
    TcpStream::connect_timeout(&address, left)
}

/// Without threads no task parks, so nothing connects this way.
#[cfg(not(any(test, feature = "threads")))]
pub(crate) fn connected_parked(
    host: &str,
    port: u16,
    timeout: Duration,
    _by: Option<Instant>,
) -> Result<(TcpStream, String), Failure> {
    connected(host, port, timeout)
}

/// A connection to one address, made without blocking: the connect is started, and this task
/// parks until the socket can be written, which is when a connect is over, or `deadline` passes.
/// The socket then says whether it's connected, and why not.
#[cfg(any(test, feature = "threads"))]
fn connected_by(address: std::net::SocketAddr, deadline: Instant) -> std::io::Result<TcpStream> {
    let socket = TcpStream::from(mio::net::TcpStream::connect(address)?);
    loop {
        if parked_out_on(&socket, Wanted::ToWrite, deadline) {
            return Err(ErrorKind::TimedOut.into());
        }
        if let Some(error) = socket.take_error()? {
            return Err(error);
        }
        match socket.peer_addr() {
            Ok(_) => return Ok(socket),
            // Woken with the connect still under way.
            Err(error) if error.kind() == ErrorKind::NotConnected => {}
            Err(error) => return Err(error),
        }
    }
}

/// `connection.start_tls(host)`: TLS over a connection that began plain, its certificate checked
/// against what the system trusts and against `host`.
fn start_tls(handle: i64, host: &str, timeout: Duration) -> Result<(), Failure> {
    upgrade(handle, host, timeout, http::raw_secured)
}

/// Puts TLS over a plain connection, by `shake`. It holds both of the connection's locks throughout,
/// so nothing else reads or writes in the middle of the handshake.
fn upgrade(handle: i64, host: &str, timeout: Duration, shake: http::RawTls) -> Result<(), Failure> {
    let stream = stream(handle)?;
    let _writing = stream.writing.lock().unwrap_or_else(PoisonError::into_inner);
    let mut reader = stream.reader.lock().unwrap_or_else(PoisonError::into_inner);
    let Reading::Plain(plain) = &*reader else { return Err(Failure::Tls(ALREADY_TLS.to_owned())) };
    // What's buffered came before the handshake, so TLS vouches for none of it. Carrying it over
    // would let anyone on the wire write what the program takes for protected data.
    if !plain.buffer().is_empty() {
        return Err(Failure::Tls(SENT_BEFORE_TLS.to_owned()));
    }
    let unusable = |error: std::io::Error| Failure::Other(error.to_string());
    let tcp = stream.socket.try_clone().map_err(unusable)?;
    // A task that can park does the handshake where it is, parked whenever it waits for the
    // other side, on a socket that doesn't block from here on, as `connect` does.
    if task_parks() {
        never_blocking(&stream).map_err(unusable)?;
        #[cfg(windows)]
        tell_the_reader(&stream, &reader).map_err(unusable)?;
        tcp.set_nonblocking(true).map_err(unusable)?;
        let deadline = Instant::now() + timeout;
        let mut waits = |socket: &TcpStream, wanted: Wanted| parked_out_on(socket, wanted, deadline);
        *reader = Reading::Tls(shake(tcp, host, timeout, Some(&mut waits))?);
        return Ok(());
    }
    *reader = Reading::Tls(shake(tcp, host, timeout, None)?);
    Ok(())
}

/// A connection to `host` on `port`, looked up and connected, blocking its thread.
fn connected(host: &str, port: u16, timeout: Duration) -> Result<(TcpStream, String), Failure> {
    let addresses = looked_up_by(host, port, Instant::now() + timeout, timeout)?;
    let stream = first_connected((host, port), &addresses, (timeout, None), connected_until)?;
    let peer = stream.peer_addr().map_or_else(|_| format!("{host}:{port}"), |peer| peer.to_string());
    Ok((stream, peer))
}

/// How much of what a connection writes the kernel holds before a write waits for the other side to
/// read: Linux's most by default.
#[cfg(windows)]
const SEND_BUFFER_BYTES: usize = 4 << 20;

/// Gives a connection a send buffer of a fixed size. Windows otherwise grows it to take whatever's
/// written, so a client that never reads can make the kernel hold unbounded memory for each
/// connection, and a write's `timeout=` never comes. Elsewhere the kernel bounds it already.
#[cfg(windows)]
pub(crate) fn bound_send_buffer(socket: &impl std::os::windows::io::AsSocket) {
    let _ = socket2::SockRef::from(socket).set_send_buffer_size(SEND_BUFFER_BYTES);
}

/// Gives a connection a send buffer of a fixed size, which only Windows needs.
#[cfg(not(windows))]
pub(crate) fn bound_send_buffer<S>(_socket: &S) {}

/// The most of a write handed to Windows at once. Windows takes the whole of a `send` into the kernel
/// if its send buffer has any room at all, so a big write would go in whole, however small the
/// buffer: in pieces, a write holds at most one piece more than the buffer, and waits, and times out,
/// as it does elsewhere.
#[cfg(windows)]
const WRITE_PIECE: usize = 256 << 10;

/// What of `data` to hand the kernel in one write.
pub(crate) fn piece(data: &[u8]) -> &[u8] {
    #[cfg(windows)]
    {
        data.get(..WRITE_PIECE).unwrap_or(data)
    }
    #[cfg(not(windows))]
    {
        data
    }
}

fn keep_stream(socket: TcpStream, peer: String, secured: Option<Secured>) -> Result<i64, Failure> {
    keep_stream_as(socket, peer, secured, false)
}

/// `keep_stream`, for a connection a server took when `served`.
fn keep_stream_as(
    socket: TcpStream,
    peer: String,
    secured: Option<Secured>,
    served: bool,
) -> Result<i64, Failure> {
    keep_stream_made(socket, peer, secured, (served, false))
}

/// `keep_stream_as`, for a connection whose socket was made not to block already when
/// `never_blocks`, as one whose handshake a task did parked is, its reader's copy included.
fn keep_stream_made(
    socket: TcpStream,
    peer: String,
    secured: Option<Secured>,
    (served, never_blocks): (bool, bool),
) -> Result<i64, Failure> {
    bound_send_buffer(&socket);
    // Each write goes out at once. What std writes, a request or a response, is its head and then
    // its body, and waiting to send the body until the head is acknowledged, which the other end
    // delays by up to 40 ms on Linux, held each one that long. If it can't be set, the connection
    // is only slower.
    let _ = socket.set_nodelay(true);
    let reading = match secured {
        Some(secured) => Reading::Tls(secured),
        None => Reading::Plain(BufReader::new(
            socket.try_clone().map_err(|error| Failure::Other(error.to_string()))?,
        )),
    };
    let stream = Stream {
        peer,
        socket,
        reader: Mutex::new(reading),
        partial_line: Mutex::new(Vec::new()),
        writing: Mutex::new(()),
        served,
        read_since_write: AtomicBool::new(false),
        awaiting_a_message: AtomicBool::new(false),
        label: if served { next_label(0) } else { 0 },
        reads_by: Mutex::new(None),
        body_held: BodyHeld::default(),
        closed: AtomicBool::new(false),
        never_blocks: AtomicBool::new(never_blocks),
        #[cfg(windows)]
        reader_never_blocks: AtomicBool::new(never_blocks),
        #[cfg(any(test, feature = "threads"))]
        parked: Mutex::new(Vec::new()),
    };
    Ok(open(|open| {
        let handle = fresh_handle(open);
        open.insert(handle, Socket::Stream(Arc::new(stream)));
        handle
    }))
}

/// TLS over a connection, as `secured` makes it.
type Secured = Box<dyn SecureStream>;

/// How many connections a server asks the system to keep waiting for it to take them: the most
/// there is. Linux, macOS and Windows each cut that down to their own most: 4,096 on Linux by
/// default (`/proc/sys/net/core/somaxconn`), 128 on macOS (`kern.ipc.somaxconn`), and on Windows
/// what it takes for a reasonable most. Past what's kept, Linux drops a connection's handshake, so
/// its client tries again and may time out, and macOS resets it. Rust's `bind` asks for 128, so a
/// burst bigger than that waited or failed wherever the system allows more, and on the Mac 172 of
/// 300 connections at once were reset (#1903).
const LISTEN_QUEUE: i32 = i32::MAX;

/// A listener on every address of this machine at `port`, or one the system picks for 0, with as
/// long a queue as the system allows.
fn bound(port: u16) -> std::io::Result<TcpListener> {
    let socket = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None)?;
    // As Rust's `bind` does everywhere but Windows, where it would let another socket take the
    // port too: a server started again binds its port at once, not once the last one's closed
    // connections have timed out.
    #[cfg(not(windows))]
    socket.set_reuse_address(true)?;
    socket.bind(&std::net::SocketAddr::from(([0, 0, 0, 0], port)).into())?;
    socket.listen(LISTEN_QUEUE)?;
    Ok(socket.into())
}

fn listen(port: u16, tls: Option<Arc<dyn TlsServer>>) -> Result<Made, Failure> {
    let listener = bound(port).map_err(|error| match error.kind() {
        ErrorKind::AddrInUse => Failure::Other(format!("port {port} is already in use")),
        _ => Failure::Other(error.to_string()),
    })?;
    let port = listener.local_addr().map_or(port, |address| address.port());
    let handle = open(|open| {
        let handle = fresh_handle(open);
        open.insert(
            handle,
            Socket::Listener(Arc::new(Listening {
                listener,
                #[cfg(any(test, feature = "threads"))]
                waits: SocketWaits::default(),
                tls,
                stop: Mutex::new(None),
            })),
        );
        handle
    });
    Ok(("Server", vec![("_handle", Value::Int(handle)), ("port", Value::Int(i64::from(port)))]))
}

fn stream(handle: i64) -> Result<Arc<Stream>, Failure> {
    match open(|open| open.get(&handle).cloned()) {
        Some(Socket::Stream(stream)) => Ok(stream),
        _ => Err(closed_here()),
    }
}

/// How long a read on this socket may wait. A socket the other side has reset refuses to be told on
/// macOS, and reading it says so at once, so that says nothing here.
fn wait_at_most(socket: &TcpStream, timeout: Option<Duration>) -> Result<(), Failure> {
    match socket.set_read_timeout(timeout) {
        Err(error) if error.kind() != ErrorKind::InvalidInput => Err(Failure::of(&error, timeout)),
        _ => Ok(()),
    }
}

/// The most one read takes in. A read gives what's there, up to what it was asked for, so a
/// count far past that, as one from a request's header can be, reads no more at once, and room
/// is never made for what only a count says is coming.
const MOST_READ_AT_ONCE: usize = 64 * 1024;

/// The most bytes of request bodies the program's servers hold at once, as they read them and
/// answer. A body is at most 10 MiB (`MOST_BODY` in `std.net`), so it's room for 25 of the biggest
/// at once. One that would go past it is answered 503 without being read, so many clients each
/// sending a big body at once can't take a server's memory (runtime exhaustion audit, 2026-11-20).
pub(crate) const MOST_BODIES_HELD: usize = 256 * 1024 * 1024;

/// The bytes of request bodies the program's servers hold now, out of `MOST_BODIES_HELD`.
static BODIES_HELD: AtomicUsize = AtomicUsize::new(0);

/// A connection's share of the room its pool has, `BODIES_HELD` for a real one: the length of the
/// body it's reading or answering, given back when it's let go of, as the connection's next
/// request starts, it closes, or it goes.
pub(crate) struct BodyHeld {
    held: AtomicUsize,
    pool: &'static AtomicUsize,
}

impl Default for BodyHeld {
    fn default() -> BodyHeld {
        BodyHeld { held: AtomicUsize::new(0), pool: &BODIES_HELD }
    }
}

impl BodyHeld {
    /// Takes `size` of its pool's room, in place of what it held, when there's that much left of
    /// `most`.
    pub(crate) fn take(&self, size: usize, most: usize) -> bool {
        self.give_back();
        let taken = self.pool.try_update(Ordering::SeqCst, Ordering::SeqCst, |held| {
            held.checked_add(size).filter(|after| *after <= most)
        });
        if taken.is_ok() {
            self.held.store(size, Ordering::SeqCst);
        }
        taken.is_ok()
    }

    /// Gives back what it holds.
    pub(crate) fn give_back(&self) {
        let held = self.held.swap(0, Ordering::SeqCst);
        if held > 0 {
            self.pool.fetch_sub(held, Ordering::SeqCst);
        }
    }
}

impl Drop for BodyHeld {
    fn drop(&mut self) {
        self.give_back();
    }
}

/// `connection._room_for_body(size)`: takes room for a request's body of `size` bytes among every
/// server's bodies, in place of the room its last one had, and says whether there was that much.
fn room_for_body(handle: i64, size: usize) -> bool {
    stream(handle).is_ok_and(|stream| stream.body_held.take(size, MOST_BODIES_HELD))
}

/// `connection._body(size)`: a request's body of `size` bytes, read whole, once its room is taken.
/// Room is made for it once, at its size, and filled as it comes, so what a slow client hasn't sent
/// is never held. Fewer bytes come back when the client stopped sending before the end.
fn body(handle: i64, size: usize, timeout: Option<Duration>) -> Result<Vec<u8>, Failure> {
    let stream = stream(handle)?;
    let mut data = Vec::with_capacity(size);
    while data.len() < size {
        let got = data.len();
        data.resize(got + (size - got).min(MOST_READ_AT_ONCE), 0);
        let read =
            reading(&stream, timeout, false, |reader| reader.read(data.get_mut(got..).unwrap_or_default()));
        data.truncate(got + *read.as_ref().unwrap_or(&0));
        if read? == 0 {
            break;
        }
        stream.read_since_write.store(true, Ordering::SeqCst);
    }
    Ok(data)
}

fn read(handle: i64, count: usize, timeout: Option<Duration>) -> Result<Vec<u8>, Failure> {
    let stream = stream(handle)?;
    // With nothing to give, it mustn't wait for something to come.
    if count == 0 {
        return Ok(Vec::new());
    }
    let mut data = vec![0; count.min(MOST_READ_AT_ONCE)];
    let between = !stream.read_since_write.load(Ordering::SeqCst);
    let read = reading(&stream, timeout, between, |reader| reader.read(&mut data))?;
    data.truncate(read);
    if read > 0 {
        stream.read_since_write.store(true, Ordering::SeqCst);
    }
    Ok(data)
}

fn read_line(handle: i64, timeout: Option<Duration>) -> Result<Option<Vec<u8>>, Failure> {
    read_line_crlf(handle, timeout, false)
}

/// `read_line`, which with `crlf` fails on a line ended by a bare `\n`. A line longer than
/// `MOST_LINE_BYTES` fails too.
fn read_line_crlf(handle: i64, timeout: Option<Duration>, crlf: bool) -> Result<Option<Vec<u8>>, Failure> {
    let stream = stream(handle)?;
    let mut line = std::mem::take(&mut *stream.partial_line.lock().unwrap_or_else(PoisonError::into_inner));
    let between = line.is_empty() && !stream.read_since_write.load(Ordering::SeqCst);
    // What a read cut short by having to wait got is kept in `line`, and the next adds to it. It
    // reads at most what's left of the most a line may hold, and one more to tell it's too long.
    let read = reading(&stream, timeout, between, |reader| {
        let left = (MOST_LINE_BYTES + 1).saturating_sub(line.len());
        let mut limited = reader.take(u64::try_from(left).unwrap_or(u64::MAX));
        limited.read_until(b'\n', &mut line)
    });
    if matches!(read, Err(Failure::TimedOut(_))) {
        *stream.partial_line.lock().unwrap_or_else(PoisonError::into_inner) = line;
        return read.map(|_| None);
    }
    if read? > 0 {
        stream.read_since_write.store(true, Ordering::SeqCst);
    }
    if let Some(why) = line_refused(&line, crlf) {
        return Err(Failure::Other(why));
    }
    Ok((!line.is_empty()).then_some(line))
}

#[cfg(test)]
fn write(handle: i64, data: &[u8]) -> Result<(), Failure> {
    write_within(handle, data, None)
}

/// `write`, giving up once `timeout` passes, as for a server that never reads.
fn write_within(handle: i64, mut data: &[u8], timeout: Option<Duration>) -> Result<(), Failure> {
    wrote_to_a_connection();
    let stream = stream(handle)?;
    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    stream.read_since_write.store(false, Ordering::SeqCst);
    let _writing = lock_parking(&stream.writing);
    // A task that can park never holds its worker in the kernel for room: the socket's made not
    // to block, for good, and the write parks whenever it's full.
    if task_parks() {
        never_blocking(&stream).map_err(|error| Failure::Other(error.to_string()))?;
    }
    if matches!(&*stream.reader.lock().unwrap_or_else(PoisonError::into_inner), Reading::Tls(_)) {
        return write_securely(&stream, data, (timeout, deadline));
    }
    let mut socket = &stream.socket;
    // A read parked on this connection's other end can have made the socket not block, so a full
    // buffer says so here, and this waits for room.
    while !data.is_empty() {
        // A socket that blocks is given what's left of the time, so its write can't outlast it.
        if let Some(deadline) = deadline {
            let left = deadline.saturating_duration_since(Instant::now()).max(Duration::from_millis(1));
            let _ = socket.set_write_timeout(Some(left));
        }
        let wrote = socket.write(piece(data));
        if deadline.is_some() {
            let _ = socket.set_write_timeout(None);
        }
        match wrote {
            Ok(0) => return Err(Failure::Closed),
            Ok(written) => data = data.get(written..).unwrap_or_default(),
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                if wait_to_write(&stream, deadline)? {
                    return Err(Failure::TimedOut(timeout.unwrap_or_default()));
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => return Err(Failure::of(&error, None)),
        }
    }
    socket.flush().map_err(|error| Failure::of(&error, None))
}

/// `write_within` over TLS. It holds the connection's TLS only to hand it data and to send
/// what it holds, never while it waits for room, so a task reading meanwhile can go on. TLS takes
/// all of `data` at once, and sends it as the socket has room.
fn write_securely(
    stream: &Stream,
    mut data: &[u8],
    (timeout, deadline): (Option<Duration>, Option<Instant>),
) -> Result<(), Failure> {
    loop {
        let sent = {
            let mut reader = stream.reader.lock().unwrap_or_else(PoisonError::into_inner);
            #[cfg(windows)]
            tell_the_reader(stream, &reader).map_err(|error| Failure::Other(error.to_string()))?;
            let Reading::Tls(secured) = &mut *reader else { return Err(Failure::Closed) };
            // TLS takes some of `data` to hold, up to a limit of its own, and sends what it holds
            // as the socket has room. So it's handed what's left until it has it all, and then
            // asked to send until it says it has. `write_all` gave up partway on a socket that
            // doesn't block: with more than TLS holds at once, and no room, it said so with part
            // taken and no way to tell how much.
            if data.is_empty() { secured.flush().map(|()| None) } else { secured.write(data).map(Some) }
        };
        match sent {
            Ok(None) => return Ok(()),
            Ok(Some(0)) => return Err(Failure::Closed),
            Ok(Some(taken)) => data = data.get(taken..).unwrap_or_default(),
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                if wait_to_write(stream, deadline)? {
                    return Err(Failure::TimedOut(timeout.unwrap_or_default()));
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => return Err(Failure::of(&error, timeout)),
        }
    }
}

/// Lets the other tasks on this thread run, giving back whether it did.
fn yield_to_others() -> bool {
    #[cfg(any(test, feature = "threads"))]
    {
        crate::embed::serve::yield_now()
    }
    #[cfg(not(any(test, feature = "threads")))]
    {
        false
    }
}

/// Takes a lock another task may hold while it waits: a task that can park lets the others on its
/// thread run until it's free, rather than block the thread the holder needs.
fn lock_parking<T>(lock: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    loop {
        match lock.try_lock() {
            Ok(held) => return held,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => return poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {}
        }
        if !task_parks() || !yield_to_others() {
            return lock.lock().unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// Connects to `host` on `port`, reads a line and closes, for tests of waits that park.
#[cfg(test)]
pub(crate) fn fetch_line(host: &str, port: u16) -> Result<Vec<u8>, Failure> {
    let (stream, peer) = if task_parks() {
        let host = host.to_owned();
        off_thread(move || connected(&host, port, Duration::from_secs(10)))?
    } else {
        connected(host, port, Duration::from_secs(10))?
    };
    let handle = keep_stream(stream, peer, None)?;
    let line = read_line(handle, Some(Duration::from_secs(10)));
    close(handle);
    line?.ok_or(Failure::Closed)
}

/// Connects to `host` on `port` and keeps the connection, for tests of waits that park.
#[cfg(test)]
pub(crate) fn kept(host: &str, port: u16) -> Result<i64, Failure> {
    let (stream, peer) = if task_parks() {
        let host = host.to_owned();
        off_thread(move || connected(&host, port, Duration::from_secs(10)))?
    } else {
        connected(host, port, Duration::from_secs(10))?
    };
    keep_stream(stream, peer, None)
}

/// A line from a kept connection, however long it takes.
#[cfg(test)]
pub(crate) fn line_from(handle: i64) -> Result<Option<Vec<u8>>, Failure> {
    read_line(handle, None)
}

/// Closes a kept connection.
#[cfg(test)]
pub(crate) fn shut(handle: i64) {
    close(handle);
}

/// What a `Server` or a `UdpSocket` was made with: its handle and its port.
#[cfg(test)]
fn handle_and_port(made: &Made) -> (i64, u16) {
    let number = |name: &str| match made.1.iter().find(|(field, _)| *field == name) {
        Some((_, Value::Int(number))) => *number,
        _ => -1,
    };
    (number("_handle"), u16::try_from(number("port")).unwrap_or(0))
}

/// A listener on a port of its own, for tests of taking connections by hand: its handle and port.
#[cfg(test)]
pub(crate) fn listened() -> (i64, u16) {
    handle_and_port(&listen(0, None).ok().unwrap())
}

/// The next connection on a listener, waiting no longer than `limit`, or as long as it takes.
#[cfg(test)]
pub(crate) fn accepted_within(server: i64, limit: Option<Duration>) -> Result<i64, Failure> {
    accept(server, limit).map(|(connection, _)| connection)
}

/// A UDP socket on a port of its own: its handle and port.
#[cfg(test)]
pub(crate) fn udp_bound() -> (i64, u16) {
    handle_and_port(&udp(0).ok().unwrap())
}

/// The next packet on a UDP socket, waiting no longer than `limit`.
#[cfg(test)]
pub(crate) fn received_within(socket: i64, limit: Duration) -> Result<Vec<u8>, Failure> {
    receive(socket, Some(limit)).map(|(data, _)| data)
}

/// Sends a packet from a UDP socket to a port on this machine.
#[cfg(test)]
pub(crate) fn sent_to_port(socket: i64, data: &[u8], port: u16) -> Result<(), Failure> {
    send_to(socket, data, &format!("127.0.0.1:{port}"))
}

/// How many waits are on a listener, or about to be.
#[cfg(test)]
pub(crate) fn waiting_on_a_listener(server: i64) -> usize {
    match open(|open| open.get(&server).cloned()) {
        Some(Socket::Listener(listening)) => {
            listening.waits.waiting.lock().unwrap_or_else(PoisonError::into_inner).len()
        }
        _ => 0,
    }
}

/// A line from a kept connection, waiting no longer than `limit` for it.
#[cfg(test)]
pub(crate) fn line_from_within(handle: i64, limit: Duration) -> Result<Option<Vec<u8>>, Failure> {
    read_line(handle, Some(limit))
}

/// Writes `data` to a kept connection, waiting no longer than `limit` for room.
#[cfg(test)]
pub(crate) fn written_within(handle: i64, data: &[u8], limit: Duration) -> Result<(), Failure> {
    write_within(handle, data, Some(limit))
}

/// How many tasks are parked waiting on a kept connection, or about to be: those with the socket
/// itself, and those with a copy of it.
#[cfg(test)]
pub(crate) fn waiting_on(handle: i64) -> (usize, usize) {
    let Ok(stream) = stream(handle) else { return (0, 0) };
    let parked = stream.parked.lock().unwrap_or_else(PoisonError::into_inner);
    let on_the_socket = parked.iter().filter(|wait| wait.on_the_socket).count();
    (on_the_socket, parked.len() - on_the_socket)
}

/// Writes `data` to a kept connection over and over until a write fails: how it failed.
#[cfg(test)]
pub(crate) fn written_until_it_fails(handle: i64, data: &[u8]) -> Failure {
    loop {
        if let Err(failure) = write(handle, data) {
            return failure;
        }
    }
}

/// Connects to `host` on `port`, sends `line` and reads a line back, for tests of waits that park.
#[cfg(test)]
pub(crate) fn said_and_heard(host: &str, port: u16, line: &[u8]) -> Result<Vec<u8>, Failure> {
    let host = host.to_owned();
    let (stream, peer) = off_thread(move || connected(&host, port, Duration::from_secs(10)))?;
    let handle = keep_stream(stream, peer, None)?;
    let heard = write(handle, line).and_then(|()| read_line(handle, Some(Duration::from_secs(10))));
    close(handle);
    heard?.ok_or(Failure::Closed)
}

/// Whether this call is in a task that can park while it waits for something to look up or connect
/// to, on a thread that may block.
pub(crate) fn task_parks() -> bool {
    #[cfg(any(test, feature = "threads"))]
    {
        crate::embed::serve::can_park()
    }
    #[cfg(not(any(test, feature = "threads")))]
    {
        false
    }
}

/// `work`, run on a thread that may block while this task parks, or here when it can't. A name's
/// lookup comes this way, and a handshake's arithmetic: every wait on a socket parks on the socket.
pub(crate) fn off_thread<R: Send + 'static>(work: impl FnOnce() -> R + Send + 'static) -> R {
    #[cfg(any(test, feature = "threads"))]
    {
        crate::embed::serve::off_thread(work)
    }
    #[cfg(not(any(test, feature = "threads")))]
    {
        work()
    }
}

/// Runs `read` on a connection, plain or through TLS: parked if it has to wait in a task, and
/// otherwise waiting at most `timeout`. A task holds the connection only while it tries, never
/// while it waits, so another task can write to it meanwhile.
/// Reads from `stream` with `read`, which `between` says starts between messages, with nothing of
/// one taken in since the connection last wrote.
fn reading<T>(
    stream: &Arc<Stream>,
    timeout: Option<Duration>,
    between: bool,
    mut read: impl FnMut(&mut dyn BufRead) -> std::io::Result<T>,
) -> Result<T, Failure> {
    // A read waits no longer than what's left before the connection's reads have to be done by.
    let timeout = match *stream.reads_by.lock().unwrap_or_else(PoisonError::into_inner) {
        Some((by, whole)) => {
            let left = by.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Failure::TimedOut(whole));
            }
            Some(timeout.map_or(left, |timeout| timeout.min(left)))
        }
        None => timeout,
    };
    let attempt = || {
        let mut reader = stream.reader.lock().unwrap_or_else(PoisonError::into_inner);
        #[cfg(windows)]
        tell_the_reader(stream, &reader)?;
        match &mut *reader {
            Reading::Plain(plain) => read(plain),
            Reading::Tls(secured) => read(&mut **secured),
        }
    };
    // A thread that can't park reads a socket that blocks as it always has, in the kernel. One a
    // task has made not block is read as a task reads it, with the waits on a poll of its own.
    if !task_parks() && !stream.never_blocks.load(Ordering::SeqCst) {
        let mut reader = stream.reader.lock().unwrap_or_else(PoisonError::into_inner);
        // The reader reads through a copy of the socket, and on Windows a copy keeps its own
        // timeouts: one set on the connection's own socket leaves a read on the copy waiting.
        let socket = match &*reader {
            Reading::Plain(plain) => plain.get_ref(),
            Reading::Tls(secured) => secured.socket(),
        };
        wait_at_most(socket, timeout)?;
        // A launched task blocks its thread here, so a stop shuts the socket from where it's made,
        // and the read, ended, fails as stopped. Told before it began, the read never starts.
        let blocked = BlockedRead::of(stream);
        if stopped_while_reading() {
            return Err(Failure::TimedOut(timeout.unwrap_or_default()));
        }
        // Waiting for a next message's first byte, with none of it in hand, a server's connection
        // ends when the server's told to stop: it says so before it waits, and a stop that comes
        // while it waits ends the wait (see `drain_on_stop`). Once a byte has come, the read goes
        // on to the end of what it reads, as a message partway in is answered.
        if between && stream.served && !reader.holds_unread() {
            stream.awaiting_a_message.store(true, Ordering::SeqCst);
            if PLAIN_DRAINS.load(Ordering::SeqCst) && crate::signals::stopping() {
                stream.awaiting_a_message.store(false, Ordering::SeqCst);
                drop(blocked);
                return read(&mut std::io::empty()).map_err(|error| Failure::of(&error, timeout));
            }
            let first = match &mut *reader {
                Reading::Plain(plain) => plain.fill_buf().map(|_| ()),
                Reading::Tls(secured) => secured.fill_buf().map(|_| ()),
            };
            stream.awaiting_a_message.store(false, Ordering::SeqCst);
            if let Err(error) = first {
                drop(blocked);
                return Err(Failure::of(&error, timeout));
            }
        }
        let done = match &mut *reader {
            Reading::Plain(plain) => read(plain),
            Reading::Tls(secured) => read(&mut **secured),
        };
        drop(blocked);
        if stopped_while_reading() {
            return Err(Failure::TimedOut(timeout.unwrap_or_default()));
        }
        return done.map_err(|error| Failure::of(&error, timeout));
    }
    read_waiting(stream, timeout, attempt)
}

/// `reading`, on a socket that doesn't block: it tries, and between tries waits on a poll until
/// the socket may be readable, its time's up, or the connection's closed. A task that can park
/// parks on its worker's poll. A thread that can't, `main` or a task holding a lock, waits in a
/// poll of its own.
fn read_waiting<T>(
    stream: &Arc<Stream>,
    timeout: Option<Duration>,
    mut attempt: impl FnMut() -> std::io::Result<T>,
) -> Result<T, Failure> {
    never_blocking(stream).map_err(|error| Failure::Other(error.to_string()))?;
    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    let mut closed = false;
    loop {
        match attempt() {
            Ok(done) => return Ok(done),
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                // Closed from here while it waited, and there's still nothing: there never will be.
                if closed {
                    return Err(Failure::Closed);
                }
                if deadline.is_some_and(|deadline| deadline <= Instant::now()) {
                    return Err(Failure::TimedOut(timeout.unwrap_or_default()));
                }
                if wait_until_readable(stream, deadline) {
                    return Err(Failure::TimedOut(timeout.unwrap_or_default()));
                }
                closed = stream.closed.load(Ordering::SeqCst);
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => return Err(Failure::of(&error, timeout)),
        }
    }
}

/// Makes a connection's socket not block, for good: its reads and writes come back at once to
/// say they would wait, and whoever asked waits on a poll. Done the first time a task that can
/// park reads or writes the connection, and never undone.
///
/// It used to be switched on for each parked read and off again after. A write found the socket
/// as the last read left it, which was blocking unless a read was parked at that moment, so a
/// task writing to a connection whose other side wasn't reading held its worker in the kernel.
/// And a read that ended switched it back under a writer parked for room, whose next write then
/// blocked too.
fn never_blocking(stream: &Stream) -> std::io::Result<()> {
    if !stream.never_blocks.load(Ordering::SeqCst) {
        stream.socket.set_nonblocking(true)?;
        stream.never_blocks.store(true, Ordering::SeqCst);
    }
    Ok(())
}

/// On Windows, has the copy of the socket the reader reads through, and TLS writes through, not
/// block either, once the socket's been made not to. A copy on Unix shares that with the socket;
/// whether one on Windows does isn't said anywhere, so there it's told too. Called with the
/// reader held, by whoever is about to use it.
#[cfg(windows)]
fn tell_the_reader(stream: &Stream, reader: &Reading) -> std::io::Result<()> {
    if !stream.never_blocks.load(Ordering::SeqCst) || stream.reader_never_blocks.load(Ordering::SeqCst) {
        return Ok(());
    }
    let copy = match reader {
        Reading::Plain(plain) => plain.get_ref(),
        Reading::Tls(secured) => secured.socket(),
    };
    copy.set_nonblocking(true)?;
    stream.reader_never_blocks.store(true, Ordering::SeqCst);
    Ok(())
}

/// Whether this is a launched task that's been stopped, which then fails as a stopped one does.
fn stopped_while_reading() -> bool {
    #[cfg(any(test, feature = "threads"))]
    {
        crate::embed::serve::cut_short()
    }
    #[cfg(not(any(test, feature = "threads")))]
    {
        false
    }
}

/// The connections launched tasks are blocked reading, each by its task's stop. A read by a task
/// that can't park, one holding a lock say, blocks its thread, so nothing run there can end it:
/// its stop shuts the socket instead.
#[cfg(any(test, feature = "threads"))]
static BLOCKED_READS: Mutex<Vec<(usize, Arc<Stream>)>> = Mutex::new(Vec::new());

/// A launched task's read blocking its thread, noted while it lasts: by its task's stop and its
/// connection. The ranges of a task's `parallel` call share the task's stop, so several reads can
/// be noted under one stop at once, and each takes back its own.
struct BlockedRead {
    #[cfg(any(test, feature = "threads"))]
    noted: Option<(usize, usize)>,
}

impl BlockedRead {
    fn of(stream: &Arc<Stream>) -> BlockedRead {
        #[cfg(any(test, feature = "threads"))]
        {
            let stop = crate::embed::threads::task_stop().map(|stop| Arc::as_ptr(&stop).addr());
            BlockedRead::noted(stop, stream)
        }
        #[cfg(not(any(test, feature = "threads")))]
        {
            let _ = stream;
            BlockedRead {}
        }
    }
}

#[cfg(any(test, feature = "threads"))]
impl BlockedRead {
    /// Notes a read of `stream` blocking the thread of the task stopped by `stop`, if a task's is.
    fn noted(stop: Option<usize>, stream: &Arc<Stream>) -> BlockedRead {
        if let Some(stop) = stop {
            BLOCKED_READS.lock().unwrap_or_else(PoisonError::into_inner).push((stop, Arc::clone(stream)));
        }
        BlockedRead { noted: stop.map(|stop| (stop, Arc::as_ptr(stream).addr())) }
    }
}

impl Drop for BlockedRead {
    fn drop(&mut self) {
        #[cfg(any(test, feature = "threads"))]
        if let Some((stop, connection)) = self.noted {
            let mut blocked = BLOCKED_READS.lock().unwrap_or_else(PoisonError::into_inner);
            let own = |(task, stream): &(usize, Arc<Stream>)| {
                *task == stop && Arc::as_ptr(stream).addr() == connection
            };
            if let Some(at) = blocked.iter().position(own) {
                blocked.swap_remove(at);
            }
        }
    }
}

/// Ends the read the launched task with this stop is blocked in, if it's in one, by shutting its
/// socket: the read then finds the task stopped.
#[cfg(any(test, feature = "threads"))]
pub(crate) fn cut_blocked_reads(stop: &Arc<std::sync::atomic::AtomicU8>) {
    let stop = Arc::as_ptr(stop).addr();
    let blocked = BLOCKED_READS.lock().unwrap_or_else(PoisonError::into_inner);
    for (_, stream) in blocked.iter().filter(|(task, _)| *task == stop) {
        let _ = stream.socket.shutdown(Shutdown::Both);
        // A read waiting in a poll of its thread's own is told, as a poll isn't told of a
        // shutdown from this side on every system.
        wake_waits(stream);
    }
}

/// Waits until `stream`'s socket may be readable or `deadline` passes: true if it passed, or the
/// task was stopped or cancelled. A task that can park parks. Anything else waits on its thread,
/// in a poll of its own.
#[cfg(any(test, feature = "threads"))]
fn wait_until_readable(stream: &Arc<Stream>, deadline: Option<Instant>) -> bool {
    if task_parks() {
        return park_on_socket(stream, mio::Interest::READABLE, deadline)
            != crate::embed::serve::Parked::Ready;
    }
    // A launched task that can't park holds its thread while it waits, so nothing run there can
    // end the wait: its stop shuts the socket and wakes the wait, which then finds it stopped.
    let blocked = BlockedRead::of(stream);
    if stopped_while_reading() {
        return true;
    }
    let over = wait_in_own_poll(stream, mio::Interest::READABLE, deadline);
    drop(blocked);
    over || stopped_while_reading()
}

/// Without threads no socket is made not to block, so nothing waits this way.
#[cfg(not(any(test, feature = "threads")))]
fn wait_until_readable(_stream: &Arc<Stream>, _deadline: Option<Instant>) -> bool {
    false
}

/// A poll of a thread's own, for its waits on sockets that don't block when it can't park.
#[cfg(any(test, feature = "threads"))]
struct OwnPoll {
    poll: mio::Poll,
    events: mio::Events,
    /// Wakes the poll from another thread: closing a connection ends a wait on it this way.
    waker: Arc<mio::Waker>,
}

#[cfg(any(test, feature = "threads"))]
impl OwnPoll {
    /// What the socket waited on is registered as, and the waker.
    const SOCKET: mio::Token = mio::Token(0);
    const WAKER: mio::Token = mio::Token(1);

    fn new() -> std::io::Result<OwnPoll> {
        let poll = mio::Poll::new()?;
        let waker = Arc::new(mio::Waker::new(poll.registry(), OwnPoll::WAKER)?);
        Ok(OwnPoll { poll, events: mio::Events::with_capacity(4), waker })
    }
}

#[cfg(any(test, feature = "threads"))]
thread_local! {
    /// This thread's own poll, made the first time it waits on a socket that doesn't block.
    static OWN_POLL: std::cell::RefCell<Option<OwnPoll>> = const { std::cell::RefCell::new(None) };
}

/// Waits on this thread until `stream`'s socket may be ready for `interest`, `deadline` passes,
/// or the connection's closed or shut from here: for a thread that can't park, on a socket that
/// doesn't block. True once `deadline` has passed. It may come back early with nothing ready,
/// and whoever called tries again.
///
/// The poll is the thread's own and is told of a copy of the socket, which has a descriptor of
/// its own, so it shares nothing with a task parked on the same connection on a worker's poll.
/// The wait is noted on the connection, as a parked task's is, so a close wakes it.
///
/// With no poll or no copy to be had, it sleeps a millisecond and says to look again. **That's a
/// retry**, and what's left when the system is out of descriptors, which nothing here can make.
#[cfg(any(test, feature = "threads"))]
fn wait_in_own_poll(stream: &Stream, interest: mio::Interest, deadline: Option<Instant>) -> bool {
    own_poll_wait(&stream.socket, interest, deadline, Some((&stream.closed, &stream.parked)))
}

/// `wait_in_own_poll` on `socket`. With `noted`, the wait is noted on that connection, and is over
/// at once if it's closed already. Without, nothing wakes it but the socket and its deadline, as
/// for the last reads of a connection that's closing.
#[cfg(any(test, feature = "threads"))]
fn own_poll_wait(
    socket: &dyn Polled,
    interest: mio::Interest,
    deadline: Option<Instant>,
    noted: Option<(&AtomicBool, &Mutex<Vec<Wait>>)>,
) -> bool {
    let waited = OWN_POLL.with_borrow_mut(|own| {
        if own.is_none() {
            *own = OwnPoll::new().ok();
        }
        let own = own.as_mut()?;
        let mut copy = socket.poll_copy()?;
        own.poll.registry().register(&mut copy, OwnPoll::SOCKET, interest).ok()?;
        // Noted first, and then a look at whether it's closed already, as a close that came
        // before the note woke nobody: a close either finds the note or is seen here.
        let wait = NEXT_WAIT.fetch_add(1, Ordering::Relaxed);
        if let Some((_, waiting)) = noted {
            let wake = Waking::Thread(Arc::clone(&own.waker));
            let note = Wait { number: wait, wake, on_the_socket: false };
            waiting.lock().unwrap_or_else(PoisonError::into_inner).push(note);
        }
        if !noted.is_some_and(|(closed, _)| closed.load(Ordering::SeqCst)) {
            let left = deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
            // Interrupted, or woken for a wait that's over: either way whoever called looks again.
            let _ = own.poll.poll(&mut own.events, left);
        }
        if let Some((_, waiting)) = noted {
            let mut parked = waiting.lock().unwrap_or_else(PoisonError::into_inner);
            parked.retain(|noted| noted.number != wait);
        }
        let _ = own.poll.registry().deregister(&mut copy);
        Some(())
    });
    if waited.is_none() {
        #[cfg(test)]
        crate::embed::serve::a_wait_wasnt_registered();
        let moment = Duration::from_millis(1);
        let left = deadline.map_or(moment, |deadline| deadline.saturating_duration_since(Instant::now()));
        std::thread::sleep(left.min(moment));
    }
    deadline.is_some_and(|deadline| deadline <= Instant::now())
}

/// For tests: how many of them are having sockets park on a copy on every platform, as Windows
/// always does. A count, so one that's done doesn't turn it off under another.
#[cfg(test)]
static PARKING_ON_COPIES: AtomicUsize = AtomicUsize::new(0);

/// For tests: while one of these lives, sockets park on a copy on every platform.
#[cfg(test)]
pub(crate) struct OnCopies;

#[cfg(test)]
impl OnCopies {
    pub(crate) fn begin() -> OnCopies {
        PARKING_ON_COPIES.fetch_add(1, Ordering::SeqCst);
        OnCopies
    }
}

#[cfg(test)]
impl Drop for OnCopies {
    fn drop(&mut self) {
        PARKING_ON_COPIES.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The number the next wait parked on a connection is noted under.
#[cfg(any(test, feature = "threads"))]
static NEXT_WAIT: AtomicU64 = AtomicU64::new(0);

/// Parks this task until `stream`'s socket may be ready for `interest`, `deadline` passes, or the
/// connection is closed from here. On Unix the worker's poll is told of the socket itself, and on
/// Windows of a copy of it.
///
/// **One wait at a time has the socket itself.** A poll takes a descriptor once, and a connection
/// can have two tasks waiting on it at once: one reading while another writes, as a chat's reader
/// and the task that broadcasts to it do. Both used to hand their worker's poll the socket. On
/// Linux the second was refused, and tried again at once for as long as it had to wait, its worker
/// never polling. On macOS both were taken, and the first to finish took both off the poll, so the
/// other was never woken. So a wait that finds another with the socket goes through a copy of it,
/// which has a descriptor of its own.
///
/// The wait is noted on the connection first, so closing it wakes this task, and then it looks at
/// whether it's closed already, as a close that came before the note woke nobody. A close either
/// finds the note or is seen here.
#[cfg(any(test, feature = "threads"))]
fn park_on_socket(
    stream: &Stream,
    interest: mio::Interest,
    deadline: Option<Instant>,
) -> crate::embed::serve::Parked {
    park_noted((&stream.closed, &stream.parked), &stream.socket, interest, deadline)
}

/// `park_on_socket` for any socket, with whether it's closed from here and the notes of the waits
/// on it, which a connection, a listener and a UDP socket each keep.
#[cfg(any(test, feature = "threads"))]
fn park_noted(
    (closed, waiting): (&AtomicBool, &Mutex<Vec<Wait>>),
    socket: &dyn Polled,
    interest: mio::Interest,
    deadline: Option<Instant>,
) -> crate::embed::serve::Parked {
    use crate::embed::serve::{Parked, ParkedTask};
    let token = crate::embed::serve::next_token();
    let wait = NEXT_WAIT.fetch_add(1, Ordering::Relaxed);
    let on_the_socket = {
        // Looked at and noted with the list held, so of two waits that come at once, one sees the
        // other.
        let mut parked = waiting.lock().unwrap_or_else(PoisonError::into_inner);
        let on_the_socket = !parked.iter().any(|other| other.on_the_socket);
        if let Some(task) = ParkedTask::here(token) {
            parked.push(Wait { number: wait, wake: Waking::Task(task), on_the_socket });
        }
        on_the_socket
    };
    let parked = if closed.load(Ordering::SeqCst) {
        Parked::Ready
    } else {
        park_on(token, socket, (interest, deadline), on_the_socket)
    };
    waiting.lock().unwrap_or_else(PoisonError::into_inner).retain(|noted| noted.number != wait);
    parked
}

/// `park_on_socket`'s wait itself, on `token`: through the socket's own descriptor if it's
/// `on_the_socket`, on Unix, and otherwise through a copy.
#[cfg(any(test, feature = "threads"))]
fn park_on(
    token: mio::Token,
    socket: &dyn Polled,
    (interest, deadline): (mio::Interest, Option<Instant>),
    on_the_socket: bool,
) -> crate::embed::serve::Parked {
    #[cfg(unix)]
    {
        #[cfg(test)]
        let on_the_socket = on_the_socket && PARKING_ON_COPIES.load(Ordering::SeqCst) == 0;
        if on_the_socket {
            return crate::embed::serve::park_on_fd(token, socket.descriptor(), interest, deadline);
        }
        crate::embed::serve::park_on_a_copy(token, || socket.poll_copy(), interest, deadline)
    }
    #[cfg(not(unix))]
    {
        let _ = on_the_socket;
        crate::embed::serve::park_on_a_copy(token, || socket.poll_copy(), interest, deadline)
    }
}

/// What a task waits for on a socket of its own.
#[derive(Clone, Copy)]
pub(crate) enum Wanted {
    ToRead,
    ToWrite,
    /// To read, with something of its own to send first, as TLS can have.
    Either,
}

/// Parks this task until a socket of its own may be ready for what's `wanted`, or `deadline`
/// passes: for a socket that doesn't block and that no other task waits on, as a request's is.
/// True once there's no more waiting to do: its time is up, or the task's been stopped.
#[cfg(any(test, feature = "threads"))]
pub(crate) fn parked_out_on(socket: &TcpStream, wanted: Wanted, deadline: Instant) -> bool {
    let interest = match wanted {
        Wanted::ToRead => mio::Interest::READABLE,
        Wanted::ToWrite => mio::Interest::WRITABLE,
        Wanted::Either => mio::Interest::READABLE | mio::Interest::WRITABLE,
    };
    let token = crate::embed::serve::next_token();
    park_on(token, socket, (interest, Some(deadline)), true) != crate::embed::serve::Parked::Ready
}

/// Without threads nothing parks, so nothing waits this way.
#[cfg(not(any(test, feature = "threads")))]
pub(crate) fn parked_out_on(_socket: &TcpStream, _wanted: Wanted, _deadline: Instant) -> bool {
    true
}

/// Waits until `stream`'s socket may take more to write: parked in a task that can, and otherwise
/// in a poll of the thread's own, when the socket is one a task has made not block. Gives true
/// once `deadline` has passed, or the task's been stopped or cancelled, which ends its write as
/// it does a read: thrown away, that left a stopped task writing for ever, never waiting and
/// never done. A connection closed from here has no room coming, and fails as closed.
fn wait_to_write(stream: &Stream, deadline: Option<Instant>) -> Result<bool, Failure> {
    if stream.closed.load(Ordering::SeqCst) {
        return Err(Failure::Closed);
    }
    if deadline.is_some_and(|deadline| deadline <= Instant::now()) {
        return Ok(true);
    }
    #[cfg(any(test, feature = "threads"))]
    if task_parks() {
        let parked = park_on_socket(stream, mio::Interest::WRITABLE, deadline);
        if parked == crate::embed::serve::Parked::Cancelled {
            return Ok(true);
        }
        return Ok(deadline.is_some_and(|deadline| deadline <= Instant::now()));
    }
    #[cfg(any(test, feature = "threads"))]
    if stream.never_blocks.load(Ordering::SeqCst) {
        return Ok(wait_in_own_poll(stream, mio::Interest::WRITABLE, deadline));
    }
    // A socket that blocks, whose own timeout ran out a moment before the deadline: it's asked
    // again, and waits in the kernel for what's left.
    std::thread::sleep(Duration::from_millis(1));
    Ok(false)
}

/// Whether an error taking a connection is that connection's own, gone before it was taken, so the
/// next one is taken instead and the program never hears of it: one reset or given up while it
/// waited, which Windows says from `accept` itself, and on Linux a network error the connection
/// met, which its manual says to take as nothing to take yet.
pub(crate) fn gone_before_taken(error: &std::io::Error) -> bool {
    if matches!(
        error.kind(),
        ErrorKind::ConnectionAborted
            | ErrorKind::ConnectionReset
            | ErrorKind::Interrupted
            | ErrorKind::NetworkDown
            | ErrorKind::NetworkUnreachable
            | ErrorKind::HostUnreachable
    ) {
        return true;
    }
    // EPROTO, ENOPROTOOPT, EHOSTDOWN, ENONET and EOPNOTSUPP, the rest of Linux's list.
    cfg!(target_os = "linux") && matches!(error.raw_os_error(), Some(71 | 92 | 112 | 64 | 95))
}

/// The next connection on `listener`, past any gone before it was taken.
fn taken(listener: &TcpListener) -> std::io::Result<(TcpStream, std::net::SocketAddr)> {
    loop {
        match listener.accept() {
            Err(error) if gone_before_taken(&error) => {}
            other => return other,
        }
    }
}

/// The next connection on a listener, waited for on a poll: a task that can park parks, so the
/// other tasks on its worker run meanwhile, the one that's about to connect among them. It used
/// to wait in the kernel, or asleep 5 ms at a time when it had a timeout, holding its worker
/// either way. Anything else waits in a poll of its thread's own, and a listener closed from here
/// ends the wait.
///
/// The listener is made not to block the first time, and never switched back. A thread that
/// can't park, waiting as long as it takes on a listener no task has used, still waits in the
/// kernel as it always has.
#[cfg(any(test, feature = "threads"))]
fn accept_on_a_poll(
    listening: &Listening,
    timeout: Option<Duration>,
) -> Result<(TcpStream, std::net::SocketAddr), Failure> {
    let (listener, waits) = (&listening.listener, &listening.waits);
    if !task_parks() && timeout.is_none() && !waits.never_blocks.load(Ordering::SeqCst) {
        return taken(listener).map_err(|error| Failure::of(&error, None));
    }
    waits.never_blocking(|| listener.set_nonblocking(true))?;
    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    loop {
        match taken(listener) {
            Ok(accepted) => return Ok(accepted),
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                if waits.closed.load(Ordering::SeqCst) {
                    return Err(closed_here());
                }
                let late = deadline.is_some_and(|deadline| deadline <= Instant::now());
                if late || waits.out(listener, mio::Interest::READABLE, deadline) {
                    return Err(Failure::TimedOut(timeout.unwrap_or_default()));
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => return Err(Failure::of(&error, timeout)),
        }
    }
}

/// The next connection on a listener, without threads: waited for in the kernel, or asked for
/// again and again when there's a timeout, as a listener has none of its own.
#[cfg(not(any(test, feature = "threads")))]
fn accept_in_the_kernel(
    listener: &TcpListener,
    timeout: Option<Duration>,
) -> Result<(TcpStream, std::net::SocketAddr), Failure> {
    let Some(limit) = timeout else {
        return taken(listener).map_err(|error| Failure::of(&error, None));
    };
    listener.set_nonblocking(true).map_err(|error| Failure::Other(error.to_string()))?;
    let started = Instant::now();
    let accepted = loop {
        match taken(listener) {
            Ok(accepted) => break Ok(accepted),
            Err(error) if error.kind() == ErrorKind::WouldBlock && started.elapsed() < limit => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                break Err(Failure::TimedOut(limit));
            }
            Err(error) => break Err(Failure::of(&error, Some(limit))),
        }
    };
    listener.set_nonblocking(false).map_err(|error| Failure::Other(error.to_string()))?;
    accepted
}

fn accept(handle: i64, timeout: Option<Duration>) -> Result<(i64, String), Failure> {
    let Some(Socket::Listener(listening)) = open(|open| open.get(&handle).cloned()) else {
        return Err(closed_here());
    };
    #[cfg(any(test, feature = "threads"))]
    let accepted = accept_on_a_poll(&listening, timeout)?;
    #[cfg(not(any(test, feature = "threads")))]
    let accepted = accept_in_the_kernel(&listening.listener, timeout)?;
    let (stream, peer) = accepted;
    stream.set_nonblocking(false).map_err(|error| Failure::Other(error.to_string()))?;
    kept_after_accept(listening.tls.as_ref(), stream, peer.to_string())
}

/// Keeps a connection a server just took, by its handle, with TLS over it first when the server
/// has it: the server's half of the handshake is done here.
fn kept_after_accept(
    tls: Option<&Arc<dyn TlsServer>>,
    stream: TcpStream,
    peer: String,
) -> Result<(i64, String), Failure> {
    let (secured, never_blocks) = match tls {
        Some(tls) => {
            let (secured, never_blocks) = accepted_securely(tls, &stream)?;
            (Some(secured), never_blocks)
        }
        None => (None, false),
    };
    Ok((keep_stream_made(stream, peer.clone(), secured, (true, never_blocks))?, peer))
}

/// How long a connection a server took has to finish its handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// The server's half of a handshake over a connection just taken, and whether the connection's
/// socket was made not to block for it.
///
/// A task that can park does it where it is, on a socket that doesn't block from here on, parked
/// on its worker's poll whenever the handshake waits for the other side. It once ran on a blocking
/// thread, held until the handshake was done or `HANDSHAKE_TIMEOUT` was up: the other side
/// decides which, so a stranger with as many slow handshakes as there are blocking threads held
/// them all, and every file read and database call in the program waited behind them.
fn accepted_securely(tls: &Arc<dyn TlsServer>, stream: &TcpStream) -> Result<(Secured, bool), Failure> {
    let unusable = |error: std::io::Error| Failure::Other(error.to_string());
    if task_parks() {
        // Before the copy is made, so on every system the copy doesn't block either.
        stream.set_nonblocking(true).map_err(unusable)?;
        let tcp = stream.try_clone().map_err(unusable)?;
        tcp.set_nonblocking(true).map_err(unusable)?;
        let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        let mut waits = |socket: &TcpStream, wanted: Wanted| parked_out_on(socket, wanted, deadline);
        return Ok((tls.accepted(tcp, HANDSHAKE_TIMEOUT, Some(&mut waits))?, true));
    }
    let tcp = stream.try_clone().map_err(unusable)?;
    Ok((tls.accepted(tcp, HANDSHAKE_TIMEOUT, None)?, false))
}

/// The next connection a server takes, by its handle, for a server taking them one at a time.
#[cfg(not(any(test, feature = "threads")))]
pub(crate) fn accepted(server: i64) -> Result<i64, Failure> {
    accept(server, None).map(|(connection, _)| connection)
}

/// Closes a connection a server took one at a time.
#[cfg(not(any(test, feature = "threads")))]
pub(crate) fn closed(connection: i64) {
    close(connection);
}

/// Whether a server taking its connections one at a time was told to `stop_on` signals, so its
/// connections are draining once one comes.
static PLAIN_DRAINS: AtomicBool = AtomicBool::new(false);

/// The connection a server taking them one at a time is serving, for a stop to reach.
#[cfg(not(any(test, feature = "threads")))]
static SERVING: Mutex<Option<Arc<Stream>>> = Mutex::new(None);

/// Says which connection a server taking them one at a time is serving, or that it's serving none.
#[cfg(not(any(test, feature = "threads")))]
pub(crate) fn serving(connection: Option<i64>) {
    *SERVING.lock().unwrap_or_else(PoisonError::into_inner) =
        connection.and_then(|handle| stream(handle).ok());
}

/// For a server taking connections one at a time that was told to `stop_on` signals: when one
/// comes, the `accept` waiting is woken by a connection of the server's own, which it then drops,
/// a connection waiting for its next message ends, and the one being served is cut short once
/// `grace` is up. Gives what `signals::forget` takes, when it was told to.
#[cfg(not(any(test, feature = "threads")))]
pub(crate) fn drain_on_stop(server: i64) -> Option<u64> {
    let Some(Socket::Listener(listening)) = open(|open| open.get(&server).cloned()) else { return None };
    let grace = (*listening.stop.lock().unwrap_or_else(PoisonError::into_inner))?;
    let mut address = listening.listener.local_addr().ok()?;
    if address.ip().is_unspecified() {
        let loopback: std::net::IpAddr = if address.is_ipv4() {
            std::net::Ipv4Addr::LOCALHOST.into()
        } else {
            std::net::Ipv6Addr::LOCALHOST.into()
        };
        address.set_ip(loopback);
    }
    PLAIN_DRAINS.store(true, Ordering::SeqCst);
    let being_served = || SERVING.lock().unwrap_or_else(PoisonError::into_inner).clone();
    Some(crate::signals::on_stop(Box::new(move || {
        // A read now sees the stop and drains. Said first, as what follows can let the program end.
        crate::embed::live::stop_taken();
        let _ = TcpStream::connect_timeout(&address, Duration::from_secs(1));
        // Only a read waiting for a next message's first byte is ended: one that's taken in any
        // of a message goes on to the end of it, and its message is answered.
        if let Some(stream) = being_served().filter(|stream| stream.awaiting_a_message.load(Ordering::SeqCst))
        {
            let _ = stream.socket.shutdown(Shutdown::Read);
        }
        let _ = std::thread::Builder::new().name("fennec-grace".to_owned()).spawn(move || {
            std::thread::sleep(grace);
            if let Some(stream) = being_served() {
                let _ = stream.socket.shutdown(Shutdown::Both);
            }
        });
    })))
}

fn udp(port: u16) -> Result<Made, Failure> {
    let socket = UdpSocket::bind(("0.0.0.0", port)).map_err(|error| match error.kind() {
        ErrorKind::AddrInUse => Failure::Other(format!("port {port} is already in use")),
        _ => Failure::Other(error.to_string()),
    })?;
    let port = socket.local_addr().map_or(port, |address| address.port());
    let handle = open(|open| {
        let handle = fresh_handle(open);
        open.insert(
            handle,
            Socket::Udp(Arc::new(Udp {
                socket,
                #[cfg(not(any(test, feature = "threads")))]
                receiving: Mutex::new(()),
                #[cfg(any(test, feature = "threads"))]
                waits: SocketWaits::default(),
            })),
        );
        handle
    });
    Ok(("UdpSocket", vec![("_handle", Value::Int(handle)), ("port", Value::Int(i64::from(port)))]))
}

fn udp_socket(handle: i64) -> Result<Arc<Udp>, Failure> {
    match open(|open| open.get(&handle).cloned()) {
        Some(Socket::Udp(udp)) => Ok(udp),
        _ => Err(closed_here()),
    }
}

/// The next packet and who sent it, waited for on a poll: a task that can park parks, and anything
/// else waits in a poll of its thread's own. Closing the socket from here ends the wait. It used
/// to wait in the kernel a tenth of a second at a time, to notice a close, holding its worker.
#[cfg(any(test, feature = "threads"))]
fn receive(handle: i64, timeout: Option<Duration>) -> Result<(Vec<u8>, String), Failure> {
    let udp = udp_socket(handle)?;
    let (socket, waits) = (&udp.socket, &udp.waits);
    waits.never_blocking(|| socket.set_nonblocking(true))?;
    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    let mut data = vec![0; MOST_IN_A_PACKET];
    loop {
        match socket.recv_from(&mut data) {
            Ok((size, sender)) => {
                data.truncate(size);
                return Ok((data, sender.to_string()));
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                if waits.closed.load(Ordering::SeqCst) {
                    return Err(closed_here());
                }
                let late = deadline.is_some_and(|deadline| deadline <= Instant::now());
                if late || waits.out(socket, mio::Interest::READABLE, deadline) {
                    return Err(Failure::TimedOut(timeout.unwrap_or_default()));
                }
            }
            // Windows says the first of a packet this socket sent earlier that nothing took.
            // Nothing's wrong with the socket, and UDP doesn't say what's lost.
            Err(error) if matches!(error.kind(), ErrorKind::ConnectionReset | ErrorKind::Interrupted) => {}
            Err(error) => return Err(Failure::of(&error, timeout)),
        }
    }
}

/// The next packet and who sent it, without threads. It looks up its socket each time it looks
/// for a packet, so closing the socket ends the wait.
#[cfg(not(any(test, feature = "threads")))]
fn receive(handle: i64, timeout: Option<Duration>) -> Result<(Vec<u8>, String), Failure> {
    let started = Instant::now();
    let mut data = vec![0; MOST_IN_A_PACKET];
    loop {
        let udp = udp_socket(handle)?;
        let _receiving = udp.receiving.lock().unwrap_or_else(PoisonError::into_inner);
        let left = timeout.map_or(UDP_LOOKS_EVERY, |limit| limit.saturating_sub(started.elapsed()));
        if left.is_zero() {
            return Err(Failure::TimedOut(timeout.unwrap_or_default()));
        }
        udp.socket
            .set_read_timeout(Some(left.min(UDP_LOOKS_EVERY)))
            .map_err(|error| Failure::of(&error, timeout))?;
        match udp.socket.recv_from(&mut data) {
            Ok((size, sender)) => {
                data.truncate(size);
                return Ok((data, sender.to_string()));
            }
            // Windows says this of a packet this socket sent earlier that nothing took. Nothing's
            // wrong with the socket, and UDP doesn't say what's lost.
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::ConnectionReset
                ) => {}
            Err(error) => return Err(Failure::of(&error, timeout)),
        }
    }
}

/// Sends one packet to an address like `"127.0.0.1:9001"`.
fn send_to(handle: i64, data: &[u8], address: &str) -> Result<(), Failure> {
    let udp = udp_socket(handle)?;
    let host = address.rsplit_once(':').map_or(address, |(host, _)| host);
    let target = address
        .to_socket_addrs()
        .map_err(|_| Failure::NotFound(host.to_owned()))?
        .next()
        .ok_or_else(|| Failure::NotFound(host.to_owned()))?;
    #[cfg(not(any(test, feature = "threads")))]
    {
        udp.socket.send_to(data, target).map(|_| ()).map_err(|error| Failure::of(&error, None))
    }
    // A socket a receive has made not block says so when it has no room: it waits for some on a
    // poll, as a receive waits for a packet.
    #[cfg(any(test, feature = "threads"))]
    loop {
        match udp.socket.send_to(data, target) {
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                if udp.waits.closed.load(Ordering::SeqCst) {
                    return Err(closed_here());
                }
                if udp.waits.out(&udp.socket, mio::Interest::WRITABLE, None) {
                    return Err(Failure::TimedOut(Duration::default()));
                }
            }
            Err(error) => return Err(Failure::of(&error, None)),
        }
    }
}

/// Closes a socket. Whoever else was using it finds out: a read waiting on another thread ends,
/// and a server being served stops taking connections, so the loop over it ends.
/// For a test: `server.close()`, from a thread of its own.
#[cfg(test)]
pub(crate) fn close_for_a_test(handle: i64) {
    close(handle);
}

fn close(handle: i64) {
    let closed = open(|open| open.remove(&handle));
    if let Some(Socket::Stream(stream)) = &closed {
        stream.closed.store(true, Ordering::SeqCst);
        stream.body_held.give_back();
        if stream.served {
            // A task that can park lingers parked, on a socket that doesn't block, as it reads
            // and writes. It used to hand the wait to a blocking thread, for up to `LINGER` a
            // close, so a few closes at once held every one of them.
            if task_parks() {
                let _ = never_blocking(stream);
            }
            linger(stream);
        }
        // It may be closed already from the other side, which is what closing wanted.
        let _ = stream.socket.shutdown(Shutdown::Both);
        // A task parked waiting on it looks again, and finds it shut. Unix's poll would say so
        // itself; Windows' doesn't report a close made from this side, and the task would wait on.
        #[cfg(any(test, feature = "threads"))]
        wake_waits(stream);
    }
    // A task or a thread waiting on a listener or a UDP socket looks again, and finds it closed.
    #[cfg(any(test, feature = "threads"))]
    match &closed {
        Some(Socket::Listener(listening)) => listening.waits.close(),
        Some(Socket::Udp(udp)) => udp.waits.close(),
        _ => {}
    }
    // A server that's being served isn't among the open sockets: its workers have its listener.
    #[cfg(any(test, feature = "threads"))]
    {
        if closed.is_none() {
            let _ = crate::embed::serve::close_served(handle);
        }
    }
}

/// How long a connection a server took reads what the other side still sends, as it closes.
pub(crate) const LINGER: Duration = Duration::from_secs(2);

/// How long that waits for more once the other side has gone quiet.
pub(crate) const LINGER_QUIET: Duration = Duration::from_millis(200);

/// Closes `socket`'s sending side and reads what the other side still sends, until it closes,
/// goes quiet for `LINGER_QUIET`, or `LINGER` passes. Closing with what it sent still unread
/// would reset the connection, which throws away what it hadn't read yet: a server's refusal of
/// a request still being sent, and the answers before it. A client's connection doesn't linger,
/// as the server it talks to may keep it open, and closing would then always wait.
///
/// A socket that doesn't block is read the same, with each wait for more on a poll: a task parks,
/// where it used to take a blocking thread for as long as this lasted, and anything else waits in
/// a poll of its thread's own.
fn linger(stream: &Stream) {
    let socket = &stream.socket;
    let _ = socket.shutdown(Shutdown::Write);
    let until = Instant::now() + LINGER;
    let on_a_poll = stream.never_blocks.load(Ordering::SeqCst);
    let mut sink = [0_u8; 16 * 1024];
    loop {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        let quiet = LINGER_QUIET.min(left);
        if !on_a_poll && socket.set_read_timeout(Some(quiet)).is_err() {
            return;
        }
        match (&*socket).read(&mut sink) {
            Ok(0) => return,
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            // Nothing yet, on a socket that doesn't block: more may come, until it's gone quiet.
            Err(error) if on_a_poll && error.kind() == ErrorKind::WouldBlock => {
                if !more_comes_by(socket, Instant::now() + quiet) {
                    return;
                }
            }
            // Gone quiet, timed out or reset: nothing more is coming that closing would lose.
            Err(_) => return,
        }
    }
}

/// Waits on a poll until a closing connection's socket may have more to read, or `by` passes:
/// whether it's worth another read. The connection's closed already, so the wait isn't noted on
/// it, and a task parks through a copy of the socket: a task the close is about to wake may still
/// have the socket itself with its worker's poll.
#[cfg(any(test, feature = "threads"))]
fn more_comes_by(socket: &TcpStream, by: Instant) -> bool {
    if task_parks() {
        let token = crate::embed::serve::next_token();
        let parked = park_on(token, socket, (mio::Interest::READABLE, Some(by)), false);
        return parked == crate::embed::serve::Parked::Ready;
    }
    !own_poll_wait(socket, mio::Interest::READABLE, Some(by), None)
}

/// Without threads no socket is made not to block, so nothing waits this way.
#[cfg(not(any(test, feature = "threads")))]
fn more_comes_by(_socket: &TcpStream, _by: Instant) -> bool {
    false
}

/// Has the server `handle` drain when the program's asked to stop, its rounds given `grace`.
fn stop_on(handle: i64, grace: Duration) {
    if let Some(Socket::Listener(listening)) = open(|open| open.get(&handle).cloned()) {
        *listening.stop.lock().unwrap_or_else(PoisonError::into_inner) = Some(grace);
    }
}

/// Takes a real server's listener out of the open sockets, for `parallel(server)` to serve from.
/// One already closed has none.
#[cfg(any(test, feature = "threads"))]
pub(crate) fn take_listener(handle: i64) -> Option<(TcpListener, crate::embed::serve::Listened)> {
    open(|open| match open.remove(&handle) {
        Some(Socket::Listener(listening)) => {
            let tls = listening.tls.clone();
            let stop = *listening.stop.lock().unwrap_or_else(PoisonError::into_inner);
            let listener = listening.listener.try_clone().ok()?;
            // Listed as served while the open sockets are held, so a close finds it in one or the other.
            let draining = crate::embed::serve::serving_soon(handle);
            Some((listener, crate::embed::serve::Listened { tls, stop, draining }))
        }
        Some(other) => {
            open.insert(handle, other);
            None
        }
        None => None,
    })
}

impl Fake {
    fn close_stream(&mut self, handle: i64) {
        if let Some(stream) = self.streams.remove(&handle)
            && let Some(other) = self.streams.get_mut(&stream.peer)
        {
            other.peer_closed = true;
        }
    }
}

/// A line without its ending, `\n` or `\r\n`.
fn trim_line(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// A read in a test that was given `timeout=`. Nothing can arrive while a test waits, so one that
/// would wait forever gives up at once, as a real one would once its time ran out.
fn or_timed_out<T>(
    read: Result<Result<T, Failure>, Diagnostic>,
    timeout: Option<Duration>,
) -> Result<Result<T, Failure>, Diagnostic> {
    match (read, timeout) {
        (Err(_), Some(after)) => Ok(Err(Failure::TimedOut(after))),
        (read, _) => read,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use fennec_syntax::Span;

    use std::rc::Rc;

    use super::{Failure, Fake, MOST_LINE_BYTES, Made, close, receive, udp};
    use crate::value::Value;

    /// How many connect at once to a server that takes none yet.
    const CONNECTING: usize = 200;

    /// A connect that can't park, to a name whose name server never answers, fails as timed out
    /// once its timeout has passed: the lookup, done on the lookups' pool, is given up on. It once
    /// held the connect for as long as the system's resolver took (#1826).
    #[test]
    fn a_blocking_connect_to_a_name_that_never_answers_times_out() {
        let _alone = stalls::one_at_a_time();
        let timeout = std::time::Duration::from_millis(200);
        let started = std::time::Instant::now();
        let made = super::connected("blocking.stalls.test", 80, timeout);
        let took = started.elapsed();
        stalls::let_go("blocking.stalls.test");
        assert!(matches!(made, Err(Failure::TimedOut(_))), "it didn't fail as timed out");
        assert!(took >= timeout, "it gave up after {took:?}, before its timeout");
        assert!(took < timeout * 25, "it gave up only after {took:?}");
        assert!(stalls::answered_within("blocking.stalls.test", std::time::Duration::from_secs(10)));
    }

    /// More connects that can't park than the lookups' pool has threads, all at once, to a name
    /// whose name server never answers, each fail as timed out, and hold no more threads than the
    /// pool's most: those past it wait their turn, and their lookups, given up on before it came,
    /// are never made. Each once started a thread of its own, so a burst against a name server
    /// that never answers started as many as there were connects (#1826).
    #[test]
    fn a_burst_of_blocking_connects_to_a_name_that_never_answers_holds_the_pools_threads_at_most() {
        let _alone = stalls::one_at_a_time();
        let timeout = std::time::Duration::from_millis(200);
        let connects: Vec<_> = (0..crate::embed::blocking::MOST_LOOKUPS + 8)
            .map(|_| std::thread::spawn(move || super::connected("burst.stalls.test", 80, timeout)))
            .collect();
        let made: Vec<_> = connects.into_iter().map(|connect| connect.join().expect("it ran")).collect();
        let held = crate::embed::blocking::lookup_threads();
        stalls::let_go("burst.stalls.test");
        assert!(made.iter().all(|made| matches!(made, Err(Failure::TimedOut(_)))), "{made:?}");
        let most = crate::embed::blocking::MOST_LOOKUPS;
        assert_eq!(held, most, "the stalled lookups held {held} threads, not the pool's {most}");
        assert!(stalls::answered_within("burst.stalls.test", std::time::Duration::from_secs(10)));
    }

    /// Request bodies take room from their pool as they're read, and give it back when let go
    /// of, taking another, or gone: past the most, one is refused, and the others keep theirs. A
    /// pool of the test's own, so other tests' bodies don't count.
    #[test]
    fn bodies_take_room_from_their_pool_and_give_it_back() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static POOL: AtomicUsize = AtomicUsize::new(0);
        let hold = || super::BodyHeld { held: AtomicUsize::new(0), pool: &POOL };
        let (first, second) = (hold(), hold());
        assert!(first.take(60, 100));
        assert!(!second.take(50, 100), "past the most");
        assert!(second.take(40, 100));
        assert_eq!(POOL.load(Ordering::SeqCst), 100);
        // A next request's body takes the room in place of the last one's.
        assert!(first.take(10, 100));
        assert_eq!(POOL.load(Ordering::SeqCst), 50);
        first.give_back();
        drop(second);
        assert_eq!(POOL.load(Ordering::SeqCst), 0);
        assert!(hold().take(100, 100));
        assert_eq!(POOL.load(Ordering::SeqCst), 0, "a hold that goes gives its room back");
    }

    /// A name with several addresses tries each for a short time before the next, so one refused
    /// slowly, as Windows refuses `::1` for a server listening only on `127.0.0.1`, doesn't use up
    /// the time the next one needed. If none connects, one that only ran out of its short time
    /// gets all the time left. One address has all the time there is from the start.
    #[test]
    fn a_names_addresses_each_get_a_short_try_before_the_next() {
        use std::io::{Error, ErrorKind};
        use std::net::SocketAddr;
        use std::time::{Duration, Instant};
        let (six, four): (SocketAddr, SocketAddr) =
            ("[::1]:9".parse().unwrap(), "127.0.0.1:9".parse().unwrap());
        let by = Instant::now() + Duration::from_secs(10);
        // Each case asks a name of its own, as the one that connects is remembered for its name.
        let connect = |name: &str,
                       addresses: &[SocketAddr],
                       answers: &dyn Fn(SocketAddr, Instant) -> Option<ErrorKind>| {
            let mut tried = Vec::new();
            let made = super::first_connected(
                (name, 9),
                addresses,
                (Duration::from_secs(10), Some(by)),
                |to, until| {
                    tried.push((to, until));
                    answers(to, until).map_or(Ok(to), |kind| Err(Error::from(kind)))
                },
            );
            (made, tried)
        };
        let short = |until: Instant| until < by && until <= Instant::now() + super::NEXT_ADDRESS_AFTER;
        // `::1` runs out of its short time, and `127.0.0.1` connects.
        let six_slow = |to: SocketAddr, _| (to == six).then_some(ErrorKind::TimedOut);
        let (made, tried) = connect("slow-six.test", &[six, four], &six_slow);
        assert_eq!(made.ok(), Some(four));
        assert!(matches!(tried.as_slice(), [(a, first), (b, _)] if *a == six && *b == four && short(*first)));
        // The next time, `127.0.0.1` is tried first, as it connected, and the name's case doesn't
        // matter.
        let (made, tried) = connect("Slow-Six.test", &[six, four], &six_slow);
        assert_eq!((made.ok(), tried.len()), (Some(four), 1));
        // If it then stops answering, `::1` is tried after its short try, and remembered instead.
        let four_gone = |to: SocketAddr, _| (to == four).then_some(ErrorKind::ConnectionRefused);
        let (made, _) = connect("slow-six.test", &[six, four], &four_gone);
        assert_eq!(made.ok(), Some(six));
        let (_, tried) = connect("slow-six.test", &[six, four], &four_gone);
        assert_eq!(tried.first().map(|(to, _)| *to), Some(six));
        // `::1` is slow, `127.0.0.1` refuses: `::1` is tried again with all the time left.
        let (made, tried) =
            connect("slow-six-refused-four.test", &[six, four], &|to, until| match to == six {
                true if until < by => Some(ErrorKind::TimedOut),
                true => None,
                false => Some(ErrorKind::ConnectionRefused),
            });
        assert_eq!(made.ok(), Some(six));
        assert_eq!(tried.iter().map(|(to, _)| *to).collect::<Vec<_>>(), [six, four, six]);
        assert_eq!(tried.last().map(|(_, until)| *until), Some(by));
        // Both refuse: refused, with nothing tried again.
        let (made, tried) =
            connect("both-refuse.test", &[six, four], &|_, _| Some(ErrorKind::ConnectionRefused));
        assert!(matches!(made, Err(Failure::Refused(..))));
        assert_eq!(tried.len(), 2);
        // One address has all the time, and none is not found.
        let (_, tried) = connect("one.test", &[four], &|_, _| Some(ErrorKind::TimedOut));
        assert_eq!(tried, [(four, by)]);
        assert!(matches!(connect("none.test", &[], &|_, _| None).0, Err(Failure::NotFound(_))));
    }

    /// A server's queue of connections waiting for it to take them holds as many as the system
    /// allows, where Rust's `bind` asked for 128 (#1903): of 200 that connect at once, as many as
    /// the system's most are kept, all 200 on Linux by default and 128 on macOS, and on Windows
    /// more than 128. Past what's kept, Linux has the client try again, and macOS resets it, as 172
    /// of 300 at once were. Each taken is closed at once, so the test holds no more than 200 sockets.
    #[test]
    fn a_servers_queue_keeps_as_many_connections_as_the_system_allows() {
        let listener = super::bound(0).unwrap();
        let port = listener.local_addr().unwrap().port();
        let address = socket2::SockAddr::from(std::net::SocketAddr::from(([127, 0, 0, 1], port)));
        let clients: Vec<socket2::Socket> = (0..CONNECTING)
            .map(|_| {
                let client =
                    socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).unwrap();
                client.set_nonblocking(true).unwrap();
                // A connect under way says it would block, which is all this one needs to do.
                let _ = client.connect(&address);
                client
            })
            .collect();
        // Long enough for the system's handshakes, which need nothing taken, and short of the
        // second a client waits before trying again where its first try was dropped.
        std::thread::sleep(std::time::Duration::from_millis(500));
        listener.set_nonblocking(true).unwrap();
        let kept = std::iter::from_fn(|| listener.accept().ok()).count();
        drop(clients);
        match system_most() {
            Some(most) => assert!(
                kept >= CONNECTING.min(most),
                "{kept} of {CONNECTING} were kept waiting, where the system allows {most}"
            ),
            None => assert!(kept > 128, "{kept} of {CONNECTING} were kept waiting, no more than Rust's 128"),
        }
    }

    /// An error taking a connection that is that connection's own, gone before it was taken, is
    /// passed over for the next, and only those: a reset or an abort while it waited, which Windows
    /// says from `accept` itself, and a signal's interruption. Running out of descriptors, or a
    /// listener with nothing waiting, still says so.
    #[test]
    fn only_a_connection_gone_before_it_was_taken_is_passed_over() {
        use std::io::{Error, ErrorKind};
        for gone in [ErrorKind::ConnectionReset, ErrorKind::ConnectionAborted, ErrorKind::Interrupted] {
            assert!(super::gone_before_taken(&Error::from(gone)), "{gone:?}");
        }
        for kept in [ErrorKind::WouldBlock, ErrorKind::InvalidInput, ErrorKind::OutOfMemory] {
            assert!(!super::gone_before_taken(&Error::from(kept)), "{kept:?}");
        }
        assert!(!super::gone_before_taken(&Error::from_raw_os_error(24)), "out of descriptors");
    }

    /// Connections reset before a program takes them don't fail its `accept`: twenty connect and
    /// reset, then one says hello, and each `accept` gives back a connection until the one that
    /// says it. macOS and Linux hand a reset connection over, and reading it fails; Windows says
    /// the reset from `accept` itself, which reached the program as its `accept` failing, so a
    /// server's loop of `accept`s ended at a stranger's reset.
    #[test]
    fn connections_reset_before_theyre_taken_dont_fail_a_programs_accept() {
        use std::io::{Read as _, Write as _};
        let Ok((_, fields)) = super::listen(0, None) else { panic!("no server") };
        let field = |name: &str| {
            fields
                .iter()
                .find_map(|(at, value)| match value {
                    Value::Int(number) if *at == name => Some(*number),
                    _ => None,
                })
                .unwrap()
        };
        let (server, port) = (field("_handle"), u16::try_from(field("port")).unwrap());
        for _ in 0..20 {
            let reset = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            socket2::SockRef::from(&reset).set_linger(Some(std::time::Duration::ZERO)).unwrap();
        }
        let mut hello = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        hello.write_all(b"hello").unwrap();
        // Long enough for the resets to arrive before anything is taken.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let mut heard = false;
        for _ in 0..21 {
            let (taken, _) = super::accept(server, Some(std::time::Duration::from_secs(10)))
                .unwrap_or_else(|failure| panic!("a reset failed the accept: {failure:?}"));
            let mut said = [0; 5];
            let read = super::stream(taken).ok().map(|stream| (&stream.socket).read_exact(&mut said));
            close(taken);
            if matches!(read, Some(Ok(()))) && &said == b"hello" {
                heard = true;
                break;
            }
        }
        close(server);
        assert!(heard, "the connection that said hello was never taken");
    }

    /// The longest queue of connections this system keeps for a server, where it says.
    fn system_most() -> Option<usize> {
        if cfg!(target_os = "linux") {
            return std::fs::read_to_string("/proc/sys/net/core/somaxconn").ok()?.trim().parse().ok();
        }
        if cfg!(target_os = "macos") {
            let said =
                std::process::Command::new("sysctl").args(["-n", "kern.ipc.somaxconn"]).output().ok()?;
            return String::from_utf8_lossy(&said.stdout).trim().parse().ok();
        }
        None
    }

    /// The ranges of a task's `parallel` call share the task's stop, so reads blocking their
    /// threads can be noted under one stop at once: one that ends takes back its own note, and
    /// leaves the other's for the stop to find. It once took back the first note under its stop,
    /// whichever read's that was.
    #[test]
    fn a_blocked_read_takes_back_its_own_note_among_others_under_its_stop() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let opened = || {
            let Ok((stream, peer)) = super::connected("127.0.0.1", port, std::time::Duration::from_secs(10))
            else {
                panic!("couldn't connect to the test's own listener")
            };
            let Ok(handle) = super::keep_stream(stream, peer, None) else { panic!("no handle") };
            let Ok(stream) = super::stream(handle) else { panic!("no stream") };
            (handle, stream)
        };
        let ((first_handle, first), (second_handle, second)) = (opened(), opened());
        // A stop's address no task has.
        let stop = usize::MAX - 7;
        let under_stop = || -> Vec<usize> {
            let blocked = super::BLOCKED_READS.lock().unwrap();
            let own = blocked.iter().filter(|(task, _)| *task == stop);
            own.map(|(_, stream)| std::sync::Arc::as_ptr(stream).addr()).collect()
        };
        let first_read = super::BlockedRead::noted(Some(stop), &first);
        let second_read = super::BlockedRead::noted(Some(stop), &second);
        drop(second_read);
        assert_eq!(under_stop(), [std::sync::Arc::as_ptr(&first).addr()]);
        drop(first_read);
        assert_eq!(under_stop(), Vec::<usize>::new());
        close(first_handle);
        close(second_handle);
    }

    /// A response's headers become a dict with each name once, a name sent again joined to the
    /// first with `, `, in the order the names first came, however many there are.
    #[test]
    fn many_headers_are_joined_by_name() {
        let mut headers: Vec<(String, String)> =
            (0..999).map(|n| (format!("x-{n}"), n.to_string())).collect();
        headers.push(("x-7".to_owned(), "again".to_owned()));
        headers.push(("set-cookie".to_owned(), "a=1".to_owned()));
        let answer = super::http::Answer { status: 200, headers, body: Vec::new() };
        let made = super::answer_value(answer);
        let Value::Instance(made) = &made else { panic!("not a Response") };
        let Some(Value::Dict(joined)) = made.field("headers") else { panic!("no headers") };
        assert_eq!(joined.len(), 1000);
        let shown = crate::format::repr(&Value::Dict(Rc::clone(joined)));
        assert!(shown.starts_with("{\"x-0\": \"0\", \"x-1\": \"1\""), "{shown}");
        assert!(shown.contains("\"x-7\": \"7, again\""), "{shown}");
    }

    /// Native code keeps `tls=` on the method's name.
    #[test]
    fn a_tests_connection_and_server_have_to_agree_about_tls() {
        let span = Span::default();
        let certificate = Value::made("Certificate", vec![]);
        let port = |made: &Value| {
            let Value::Ok(made) = made else { panic!("it failed: {made:?}") };
            let Value::Instance(instance) = made.as_ref() else { panic!("not an instance") };
            instance.field("port").cloned().unwrap()
        };
        let host = || Value::str("localhost");
        let mut fake = Fake::default();

        let secure = fake.net_method("_listen.tls", &[Value::Int(0), certificate], span).unwrap();
        let plain = fake.net_method("_listen", &[Value::Int(0)], span).unwrap();
        let (secure, plain) = (port(&secure), port(&plain));
        let tls = Value::Bool(true);

        let both = fake.net_method("_connect.tls", &[host(), secure.clone(), tls.clone()], span).unwrap();
        assert!(matches!(both, Value::Ok(_)), "{both:?}");
        for (name, args) in [("_connect", vec![host(), secure]), ("_connect.tls", vec![host(), plain, tls])] {
            let refused = fake.net_method(name, &args, span).unwrap();
            let Value::Err(error) = refused else { panic!("{name} connected") };
            assert!(format!("{error:?}").contains("Tls"), "{error:?}");
        }
    }

    /// A test's `start_tls` refuses what a real one does: plain bytes waiting, or doing it twice.
    #[test]
    fn a_tests_start_tls_refuses_bytes_sent_before_it_and_a_second_try() {
        let span = Span::default();
        let refused = |got: Value| {
            let Value::Err(error) = got else { panic!("it wasn't refused") };
            assert!(format!("{error:?}").contains("Tls"), "{error:?}");
        };
        let start = |fake: &mut Fake, handle: i64| {
            fake.socket_method(handle, "Connection.start_tls", &[Value::str("localhost")], span).unwrap()
        };
        let mut fake = Fake::default();
        let Ok((_, server)) = fake.listen(0, false) else { panic!("it didn't listen") };
        let Some((_, Value::Int(port))) = server.get(1).cloned() else { panic!("no port") };
        let Ok((_, client)) = fake.connect("localhost", u16::try_from(port).unwrap(), false) else {
            panic!("it didn't connect")
        };
        let Some((_, Value::Int(client))) = client.first().cloned() else { panic!("no handle") };
        assert!(matches!(start(&mut fake, client), Value::Ok(_)));
        refused(start(&mut fake, client));

        let Ok((_, second)) = fake.connect("localhost", u16::try_from(port).unwrap(), false) else {
            panic!("it didn't connect")
        };
        let Some((_, Value::Int(second))) = second.first().cloned() else { panic!("no handle") };
        let Some(other_end) = fake.streams.get(&second).map(|stream| stream.peer) else {
            panic!("no other end")
        };
        assert!(matches!(fake.write(other_end, b"Sinjected"), Ok(())));
        refused(start(&mut fake, second));
    }

    /// A test's connection refuses the lines a real one does: too long, or ended by a bare `\n` when
    /// `crlf=True` asks for `\r\n`. Native code keeps `crlf=` on the method's name, before or after
    /// `timeout=`.
    #[test]
    fn a_tests_read_line_refuses_long_lines_and_bare_line_feeds() {
        let span = Span::default();
        let refused = |got: Value, why: &str| {
            let Value::Err(error) = &got else { panic!("it wasn't refused: {got:?}") };
            assert!(format!("{error:?}").contains(why), "{error:?}");
        };
        let mut fake = Fake::default();
        let Ok((_, server)) = fake.listen(0, false) else { panic!("it didn't listen") };
        let Some((_, Value::Int(port))) = server.get(1).cloned() else { panic!("no port") };
        let Ok((_, client)) = fake.connect("localhost", u16::try_from(port).unwrap(), false) else {
            panic!("it didn't connect")
        };
        let Some((_, Value::Int(client))) = client.first().cloned() else { panic!("no handle") };
        let Some(other_end) = fake.streams.get(&client).map(|stream| stream.peer) else {
            panic!("no other end")
        };
        assert!(matches!(fake.write(other_end, b"good\r\nbad\n"), Ok(())));
        let crlf = Value::Bool(true);
        let good = fake
            .socket_method(client, "Connection.read_line.crlf", std::slice::from_ref(&crlf), span)
            .unwrap();
        assert_eq!(good, Value::Ok(Rc::new(Value::str("good"))));
        refused(
            fake.socket_method(client, "Connection.read_line.crlf", &[crlf], span).unwrap(),
            "carriage return",
        );
        assert!(matches!(fake.write(other_end, &vec![b'x'; MOST_LINE_BYTES + 1]), Ok(())));
        refused(fake.socket_method(client, "Connection.read_line", &[], span).unwrap(), "longer than");
    }

    /// `timeout=` comes off a method's name wherever it is among the named arguments.
    #[test]
    fn timeout_comes_off_wherever_it_was_written() {
        let span = Span::default();
        let wait = Value::made("Duration", vec![("nanos", Value::Int(5))]);
        let (method, args, timeout) =
            super::timed("Connection.read_line.timeout.crlf", &[wait.clone(), Value::Bool(true)], span)
                .unwrap();
        assert_eq!(
            (method.as_str(), args, timeout),
            ("Connection.read_line.crlf", vec![Value::Bool(true)], Some(std::time::Duration::from_nanos(5)))
        );
        let (method, args, timeout) =
            super::timed("read_line.crlf.timeout", &[Value::Bool(true), wait.clone()], span).unwrap();
        assert_eq!(
            (method.as_str(), args, timeout),
            ("read_line.crlf", vec![Value::Bool(true)], Some(std::time::Duration::from_nanos(5)))
        );
        let (method, args, timeout) = super::timed("read_line", &[], span).unwrap();
        assert_eq!((method.as_str(), args, timeout), ("read_line", vec![], None));
        // A name that's all named parts, a `timeout` first or alone, and named parts past the
        // arguments there are, which aren't named arguments.
        let once = |name: &str, args: &[Value]| {
            let (method, args, timeout) = super::timed(name, args, span).unwrap();
            (method, args.len(), timeout.map(|after| after.as_nanos()))
        };
        assert_eq!(once("timeout", std::slice::from_ref(&wait)), (String::new(), 0, Some(5)));
        assert_eq!(once("timeout.crlf", &[wait.clone(), Value::Bool(true)]), ("crlf".to_owned(), 1, Some(5)));
        assert_eq!(once("write.timeout", &[Value::str("x"), wait.clone()]), ("write".to_owned(), 1, Some(5)));
        assert_eq!(
            once("read_line.timeout.crlf", &[Value::Bool(true)]),
            ("read_line.timeout.crlf".to_owned(), 1, None)
        );
        assert_eq!(
            once("read_line.port.timeout", std::slice::from_ref(&wait)),
            ("read_line.port".to_owned(), 0, Some(5))
        );
        // Two `timeout`s: the one nearer the start is the argument, as before.
        assert_eq!(
            once("x.timeout.timeout", &[wait.clone(), Value::Int(9)]),
            ("x.timeout".to_owned(), 1, Some(5))
        );
    }

    #[test]
    fn a_tests_listen_takes_only_a_certificate() {
        let mut fake = Fake::default();
        assert!(fake.net_method("_listen.tls", &[Value::Int(0), Value::Int(1)], Span::default()).is_err());
    }

    fn handle_of_made((_, fields): &Made) -> i64 {
        match fields.first() {
            Some((_, Value::Int(handle))) => *handle,
            _ => -1,
        }
    }

    /// A connection kept, whether a server took it or a client made it, sends each write at once,
    /// without waiting for the last to be acknowledged.
    #[test]
    fn a_kept_connection_sends_each_write_at_once() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (server, _) = listener.accept().unwrap();
        assert!(!client.nodelay().unwrap(), "a new socket waits, until it's kept");
        for (socket, served) in [(client, false), (server, true)] {
            let Ok(handle) = super::keep_stream_as(socket, "peer".to_owned(), None, served) else {
                panic!("no stream")
            };
            let kept = super::open(|open| open.get(&handle).cloned());
            let Some(super::Socket::Stream(stream)) = kept else { panic!("not kept") };
            assert!(stream.socket.nodelay().unwrap(), "served: {served}");
            close(handle);
        }
    }

    /// A thread that can't park, `main` say, uses a connection a task has made not block as it
    /// would one that blocks: a read waits for what comes or runs out of time, a write waits for
    /// room, and a close from another thread ends a wait. Each wait is in a poll of the thread's
    /// own. The read used to come back at once saying it timed out, as a socket that doesn't
    /// block says "nothing yet" where one that blocks waits.
    #[test]
    fn a_thread_that_cant_park_waits_in_a_poll_of_its_own_on_a_socket_that_doesnt_block() {
        use std::io::{Read as _, Write as _};
        use std::time::{Duration, Instant};
        const MUCH: usize = 8 << 20;
        let patient = Some(Duration::from_secs(10));
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        let Ok(handle) = super::keep_stream(client, "server".to_owned(), None) else { panic!("no stream") };
        let Ok(stream) = super::stream(handle) else { panic!("not kept") };
        super::never_blocking(&stream).unwrap();
        // Nothing comes: the read runs out of time, and not before.
        let started = Instant::now();
        let read = super::read_line(handle, Some(Duration::from_millis(50)));
        assert!(matches!(read, Err(Failure::TimedOut(_))), "{read:?}");
        assert!(started.elapsed() >= Duration::from_millis(50), "{:?}", started.elapsed());
        // A line comes while it waits.
        let sending = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            server.write_all(b"hello\n").unwrap();
            server
        });
        assert_eq!(super::read_line(handle, patient), Ok(Some(b"hello\n".to_vec())));
        let mut server = sending.join().unwrap();
        // More than the buffers hold, to a server that reads only after a moment: the write waits
        // for room, and all of it arrives.
        let reading = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            let mut got = vec![0; MUCH];
            let whole = server.read_exact(&mut got).is_ok() && got.iter().all(|byte| *byte == b'x');
            (server, whole)
        });
        assert_eq!(super::write_within(handle, &vec![b'x'; MUCH], patient), Ok(()));
        let (server, whole) = reading.join().unwrap();
        assert!(whole, "what was written didn't all arrive");
        // Closed from another thread while it waits to read: the wait ends, long before its time.
        let closing = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            close(handle);
        });
        let started = Instant::now();
        let read = super::read_line(handle, patient);
        assert!(matches!(read, Ok(None) | Err(Failure::Closed)), "{read:?}");
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
        closing.join().unwrap();
        assert_eq!(crate::embed::serve::waits_not_registered_here(), 0, "a wait slept and looked again");
        drop(server);
    }

    /// A client still sending while the server's end closes: 16 megabytes in pieces, back to back,
    /// more than the sockets hold, and then it closes its side. It has sent the first piece when
    /// this gives it back, so it's partway through whenever the close starts. Its thread gives back
    /// whether every piece went, and when it had sent the last.
    ///
    /// It never sleeps. A close reads until the other side has been quiet for `LINGER_QUIET`, and
    /// this client used to sleep 50 ms before its first piece and 5 ms before each after: on a busy
    /// machine one of those sleeps ran past the quiet, the close took the client for done, and its
    /// next piece was refused (main's CI on macOS, run 44032657746). Now it can look quiet only if
    /// its thread, ready to run, isn't run for that long.
    pub(crate) fn still_sending(
        mut client: std::net::TcpStream,
    ) -> std::thread::JoinHandle<(bool, std::time::Instant)> {
        use std::io::Write as _;
        let (begun, beginning) = std::sync::mpsc::channel();
        let sending = std::thread::spawn(move || {
            let piece = vec![b'z'; 1 << 16];
            let first = client.write_all(&piece).is_ok();
            let _ = begun.send(());
            let rest = first && (1..256).all(|_| client.write_all(&piece).is_ok());
            // Noted before the close of its side, which the server's close waits for.
            let done = std::time::Instant::now();
            (rest && client.shutdown(std::net::Shutdown::Write).is_ok(), done)
        });
        let _ = beginning.recv();
        sending
    }

    /// A connection a server took, kept as one, to a client that's `still_sending`.
    pub(crate) fn served_while_the_client_sends() -> (i64, std::thread::JoinHandle<(bool, std::time::Instant)>)
    {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (server, _) = listener.accept().unwrap();
        let Ok(handle) = super::keep_stream_as(server, "client".to_owned(), None, true) else {
            panic!("no stream")
        };
        (handle, still_sending(client))
    }

    /// A server's connection whose socket doesn't block still reads what the other side is
    /// sending as it closes, until that side is done, with its waits in a poll. Closing with that
    /// unread resets the connection, and the client's sends then fail. The reads that do this
    /// were written for a socket that blocks: on one that doesn't they found nothing yet, took
    /// that for the end, and closed at once.
    #[test]
    fn a_servers_connection_that_doesnt_block_still_reads_what_is_sent_as_it_closes() {
        let (handle, sending) = served_while_the_client_sends();
        let Ok(stream) = super::stream(handle) else { panic!("not kept") };
        super::never_blocking(&stream).unwrap();
        let started = std::time::Instant::now();
        close(handle);
        let closed = std::time::Instant::now();
        let (sent, done) = sending.join().unwrap();
        assert!(sent, "a piece the client sent was refused");
        assert!(closed >= done, "it closed before the client had sent everything");
        let lasted = closed.duration_since(started);
        assert!(lasted < super::LINGER, "it closed after {lasted:?}");
        assert_eq!(crate::embed::serve::waits_not_registered_here(), 0, "a wait slept and looked again");
    }

    /// A thread that can't park, `main` say, waits for a connection or a packet in a poll of its
    /// own: for as long as its timeout and no longer, until one comes, or until the socket's
    /// closed from another thread. With a timeout a listener used to be asked again every 5 ms,
    /// and a UDP socket every tenth of a second, which is how a close was noticed.
    #[test]
    fn a_thread_waits_for_a_connection_or_a_packet_in_a_poll_of_its_own() {
        use std::time::{Duration, Instant};
        let patient = Duration::from_secs(10);
        let short = Duration::from_millis(50);
        // A listener: nothing comes, then a connection does, then it's closed under a wait.
        let (server, port) = super::listened();
        let started = Instant::now();
        let none = super::accepted_within(server, Some(short));
        assert!(matches!(none, Err(Failure::TimedOut(_))), "{none:?}");
        assert!(started.elapsed() >= short, "{:?}", started.elapsed());
        let connecting = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            std::net::TcpStream::connect(("127.0.0.1", port)).unwrap()
        });
        let taken = super::accepted_within(server, Some(patient));
        assert!(taken.is_ok(), "{taken:?}");
        close(taken.unwrap_or(0));
        drop(connecting.join().unwrap());
        let closing = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            close(server);
        });
        let started = Instant::now();
        let ended = super::accepted_within(server, Some(patient));
        assert!(matches!(ended, Err(Failure::Other(_))), "{ended:?}");
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
        closing.join().unwrap();
        // A UDP socket, the same three ways.
        let (socket, port) = super::udp_bound();
        let (other, _) = super::udp_bound();
        let started = Instant::now();
        let none = super::received_within(socket, short);
        assert!(matches!(none, Err(Failure::TimedOut(_))), "{none:?}");
        assert!(started.elapsed() >= short, "{:?}", started.elapsed());
        let sending = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            super::sent_to_port(other, b"ping", port)
        });
        assert_eq!(super::received_within(socket, patient), Ok(b"ping".to_vec()));
        assert_eq!(sending.join().unwrap(), Ok(()));
        let closing = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            close(socket);
        });
        let started = Instant::now();
        let ended = super::received_within(socket, patient);
        assert!(matches!(ended, Err(Failure::Other(_))), "{ended:?}");
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
        closing.join().unwrap();
        close(other);
        assert_eq!(crate::embed::serve::waits_not_registered_here(), 0, "a wait slept and looked again");
    }

    /// A write to a server that never reads gives up once its `timeout=` passes.
    #[test]
    fn a_write_nobody_reads_times_out() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (server, _) = listener.accept().unwrap();
        let Ok(handle) = super::keep_stream(client, "server".to_owned(), None) else { panic!("no stream") };
        let wrote =
            super::write_within(handle, &vec![b'x'; 64 << 20], Some(std::time::Duration::from_millis(200)));
        assert!(matches!(wrote, Err(Failure::TimedOut(_))), "{wrote:?}");
        close(handle);
        drop(server);
    }

    /// A read of a line that times out keeps what it got, for the next read to go on from.
    #[test]
    fn a_line_read_that_times_out_keeps_what_it_got() {
        use std::io::Write as _;
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        let Ok(handle) = super::keep_stream(client, "server".to_owned(), None) else { panic!("no stream") };
        server.write_all(b"hel").unwrap();
        let short = Some(std::time::Duration::from_millis(50));
        assert!(matches!(super::read_line(handle, short), Err(Failure::TimedOut(_))));
        server.write_all(b"lo\n").unwrap();
        let line = super::read_line(handle, Some(std::time::Duration::from_secs(10)));
        assert_eq!(line.ok().flatten().as_deref(), Some(&b"hello\n"[..]));
        close(handle);
    }

    /// Once a connection's reads are given a time to be done by, a client sending a little at a
    /// time, each read well inside its own timeout, is cut off when that time comes, rather than
    /// holding the connection as long as it keeps sending.
    #[test]
    fn reads_given_a_time_to_be_done_by_stop_then_however_steadily_bytes_come() {
        use std::io::Write as _;
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        let Ok(handle) = super::keep_stream(client, "server".to_owned(), None) else { panic!("no stream") };
        // A line every 20 ms, for five seconds, far longer than the reads are given.
        let sender = std::thread::spawn(move || {
            for _ in 0..250 {
                if server.write_all(b"x\n").is_err() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        });
        let within = Value::made("Duration", vec![("nanos", Value::Int(300_000_000))]);
        let set = super::real_connection(handle, "_reads_within", &[within], None, Span::default());
        assert!(set.is_ok());
        let started = std::time::Instant::now();
        let each = Some(std::time::Duration::from_secs(1));
        let ended = loop {
            match super::read_line(handle, each) {
                Ok(Some(_)) => {}
                other => break other,
            }
        };
        let took = started.elapsed();
        close(handle);
        let _ = sender.join();
        assert!(matches!(ended, Err(Failure::TimedOut(_))), "{ended:?}");
        // Cut off at about 300 ms; the sender would have gone on for five seconds.
        assert!(took < std::time::Duration::from_secs(3), "{took:?}");
    }

    /// A read asked for far more than could ever come, as with a count from a request's header,
    /// gives what's there without making room for the rest.
    #[test]
    fn a_read_of_a_huge_count_gives_what_there_is() {
        use std::io::Write as _;
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        let Ok(handle) = super::keep_stream(client, "server".to_owned(), None) else { panic!("no stream") };
        server.write_all(b"abc").unwrap();
        let read = super::read(handle, 1 << 50, Some(std::time::Duration::from_secs(10)));
        assert_eq!(read.ok().as_deref(), Some(&b"abc"[..]));
        close(handle);
    }

    /// A client's line read refuses what a served one does: a line past the most a line may hold,
    /// and with `crlf` one ended by a bare `\n`.
    #[test]
    fn a_clients_line_read_refuses_long_lines_and_bare_line_feeds() {
        use std::io::Write as _;
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        let Ok(handle) = super::keep_stream(client, "server".to_owned(), None) else { panic!("no stream") };
        let wait = Some(std::time::Duration::from_secs(10));
        let writing = std::thread::spawn(move || {
            server.write_all(b"good\r\nbad\n").unwrap();
            let _ = server.write_all(&vec![b'x'; MOST_LINE_BYTES + 10]);
            server
        });
        assert_eq!(
            super::read_line_crlf(handle, wait, true).ok().flatten().as_deref(),
            Some(&b"good\r\n"[..])
        );
        let bare = super::read_line_crlf(handle, wait, true);
        assert!(matches!(&bare, Err(Failure::Other(why)) if why.contains("carriage return")), "{bare:?}");
        let long = super::read_line(handle, wait);
        assert!(matches!(&long, Err(Failure::Other(why)) if why.contains("longer than")), "{long:?}");
        close(handle);
        drop(writing.join());
    }

    /// A test's socket with no packet waiting fails at once, as nothing can come while a test
    /// waits: given `timeout=`, it gave up after it, and otherwise it says none can come, so a loop
    /// over the socket ends.
    #[test]
    fn a_tests_receive_with_nothing_waiting_fails_rather_than_waiting() {
        let mut fake = Fake::default();
        let Ok((_, socket)) = fake.udp(0) else { panic!("no socket") };
        let Some((_, Value::Int(handle))) = socket.first().cloned() else { panic!("no handle") };
        let limit = std::time::Duration::from_millis(10);
        assert!(matches!(fake.receive(handle, Some(limit)), Err(Failure::TimedOut(after)) if after == limit));
        let none = fake.receive(handle, None);
        assert!(matches!(&none, Err(Failure::Other(why)) if why.contains("none can come")), "{none:?}");
    }

    /// A test's connection or socket closed at this end says so when it's used again, not that the
    /// other side closed it.
    #[test]
    fn a_tests_socket_closed_here_says_so_when_used_again() {
        let mut fake = Fake::default();
        let Ok((_, server)) = fake.listen(0, false) else { panic!("it didn't listen") };
        let Some((_, Value::Int(port))) = server.get(1).cloned() else { panic!("no port") };
        let Ok((_, client)) = fake.connect("localhost", u16::try_from(port).unwrap(), false) else {
            panic!("it didn't connect")
        };
        let Some((_, Value::Int(client))) = client.first().cloned() else { panic!("no handle") };
        let Some(other_end) = fake.streams.get(&client).map(|stream| stream.peer) else {
            panic!("no other end")
        };
        fake.close(client);
        let here = |failed: Result<(), Failure>| matches!(failed, Err(Failure::Other(why)) if why.contains("closed here"));
        assert!(here(fake.write(client, b"gone")));
        assert!(
            matches!(fake.write(other_end, b"to nobody"), Err(Failure::Closed)),
            "the other end's is closed"
        );
        let Ok((_, socket)) = fake.udp(0) else { panic!("no socket") };
        let Some((_, Value::Int(socket))) = socket.first().cloned() else { panic!("no handle") };
        fake.close(socket);
        assert!(here(fake.send_to(socket, b"gone", "localhost:9")));
    }

    /// UDP has no `shutdown`, so a read on another thread has to find out for itself.
    #[test]
    fn closing_a_udp_socket_ends_a_read_waiting_on_another_thread() {
        let Ok(made) = udp(0) else { panic!("no UDP socket") };
        let handle = handle_of_made(&made);
        let waiting = std::thread::spawn(move || receive(handle, None));
        std::thread::sleep(std::time::Duration::from_millis(20));
        close(handle);
        assert!(matches!(waiting.join().ok(), Some(Err(Failure::Other(why))) if why.contains("closed here")));
    }

    #[test]
    fn a_udp_read_gives_up_when_told_to() {
        let Ok(made) = udp(0) else { panic!("no UDP socket") };
        let handle = handle_of_made(&made);
        let limit = std::time::Duration::from_millis(5);
        assert!(matches!(receive(handle, Some(limit)), Err(Failure::TimedOut(after)) if after == limit));
        close(handle);
    }

    /// Names whose name server never answers, until a test lets their lookups go: any name under
    /// `stalls.test`.
    pub(crate) mod stalls {
        use std::sync::{Condvar, Mutex, PoisonError};
        use std::time::{Duration, Instant};

        /// The names let go, and those whose lookups have answered since.
        static LET_GO: Mutex<(Vec<String>, Vec<String>)> = Mutex::new((Vec::new(), Vec::new()));
        static CHANGED: Condvar = Condvar::new();

        /// How long a stalled lookup waits at most, so a test whose lookup isn't given up on fails
        /// rather than waiting for ever.
        const AT_MOST: Duration = Duration::from_secs(10);

        /// A lookup of `name`: it waits until a test lets `name` go, or `AT_MOST`.
        pub(crate) fn stalled(name: &str) {
            let by = Instant::now() + AT_MOST;
            let mut names = LET_GO.lock().unwrap_or_else(PoisonError::into_inner);
            while !names.0.iter().any(|gone| gone == name) {
                let left = by.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                names = CHANGED.wait_timeout(names, left).unwrap_or_else(PoisonError::into_inner).0;
            }
            names.1.push(name.to_owned());
            CHANGED.notify_all();
        }

        static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

        /// Held by each test that stalls lookups, for as long as it runs. The lookups' pool is the
        /// program's, so one test's stalls would hold the threads another's lookups wait for, and
        /// that one's lookup, given up on before its turn came, would never be made.
        pub(crate) fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
            ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner)
        }

        /// Lets lookups of `name` answer.
        pub(crate) fn let_go(name: &str) {
            LET_GO.lock().unwrap_or_else(PoisonError::into_inner).0.push(name.to_owned());
            CHANGED.notify_all();
        }

        /// Whether a lookup of `name` answers within `within`.
        pub(crate) fn answered_within(name: &str, within: Duration) -> bool {
            let by = Instant::now() + within;
            let mut names = LET_GO.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                if names.1.iter().any(|answered| answered == name) {
                    return true;
                }
                let left = by.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return false;
                }
                names = CHANGED.wait_timeout(names, left).unwrap_or_else(PoisonError::into_inner).0;
            }
        }
    }

    pub(crate) mod tls {
        use std::io::{BufRead as _, BufReader, Write as _};
        use std::net::{TcpListener, TcpStream};
        use std::rc::Rc;
        use std::sync::Arc;
        use std::time::Duration;

        use rustls::pki_types::pem::PemObject as _;
        use rustls::pki_types::{CertificateDer, PrivateKeyDer};
        use rustls::{ClientConfig, RootCertStore, ServerConfig, ServerConnection, StreamOwned};

        use fennec_syntax::Span;

        use super::super::http::tls::handshake;
        use super::super::{
            Failure, close, http, keep_stream, read, read_line, real_connection, real_net, real_server,
            upgrade, write,
        };
        use crate::value::Value;

        const CA: &[u8] = include_bytes!("testdata/tls-test-ca.pem");
        const CHAIN: &[u8] = include_bytes!("testdata/tls-test-chain.pem");
        const KEY: &[u8] = include_bytes!("testdata/tls-test-key.pem");

        fn provider() -> Arc<rustls::crypto::CryptoProvider> {
            Arc::new(rustls::crypto::ring::default_provider())
        }

        /// A client that trusts only the tests' own certificate authority.
        fn client_config() -> Arc<ClientConfig> {
            let mut roots = RootCertStore::empty();
            for certificate in CertificateDer::pem_slice_iter(CA) {
                roots.add(certificate.unwrap()).unwrap();
            }
            let config = ClientConfig::builder_with_provider(provider())
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(roots)
                .with_no_client_auth();
            Arc::new(config)
        }

        /// A server on a port of its own, with the tests' certificate for `localhost`, that answers
        /// one line on one connection with `pong`.
        fn serving() -> (u16, std::thread::JoinHandle<Result<String, String>>) {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = std::thread::spawn(move || {
                let chain: Vec<CertificateDer<'static>> =
                    CertificateDer::pem_slice_iter(CHAIN).map(Result::unwrap).collect();
                let key = PrivateKeyDer::from_pem_slice(KEY).unwrap();
                let config = ServerConfig::builder_with_provider(provider())
                    .with_safe_default_protocol_versions()
                    .unwrap()
                    .with_no_client_auth()
                    .with_single_cert(chain, key)
                    .unwrap();
                let (tcp, _) = listener.accept().unwrap();
                let connection = ServerConnection::new(Arc::new(config)).unwrap();
                let mut reader = BufReader::new(StreamOwned::new(connection, tcp));
                let mut line = String::new();
                reader.read_line(&mut line).map_err(|error| error.to_string())?;
                let secured = reader.get_mut();
                secured.write_all(b"pong\n").map_err(|error| error.to_string())?;
                secured.flush().map_err(|error| error.to_string())?;
                Ok(line)
            });
            (port, server)
        }

        fn connected_to(port: u16) -> TcpStream {
            TcpStream::connect(("127.0.0.1", port)).unwrap()
        }

        /// A TLS connection's handle, to a server that's slow to read: once the handshake's done it
        /// waits a moment, reads `count` bytes, and answers a line. What its thread gives back is
        /// whether every byte it read was a `y`.
        pub(crate) fn connected_to_a_slow_reader(count: usize) -> (i64, std::thread::JoinHandle<bool>) {
            use std::io::Read as _;
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = std::thread::spawn(move || {
                let chain: Vec<CertificateDer<'static>> =
                    CertificateDer::pem_slice_iter(CHAIN).map(Result::unwrap).collect();
                let config = ServerConfig::builder_with_provider(provider())
                    .with_safe_default_protocol_versions()
                    .unwrap()
                    .with_no_client_auth()
                    .with_single_cert(chain, PrivateKeyDer::from_pem_slice(KEY).unwrap())
                    .unwrap();
                let (tcp, _) = listener.accept().unwrap();
                let connection = ServerConnection::new(Arc::new(config)).unwrap();
                let mut secured = StreamOwned::new(connection, tcp);
                // The handshake's done by the first read, which waits for the client to write.
                let mut first = [0; 1];
                let _ = secured.read(&mut first);
                std::thread::sleep(Duration::from_millis(50));
                let mut got = vec![0; count];
                let whole = secured.read_exact(&mut got).is_ok() && got.iter().all(|byte| *byte == b'y');
                let _ = secured.write_all(b"done\n");
                let _ = secured.flush();
                std::thread::sleep(Duration::from_millis(200));
                whole
            });
            let tcp = connected_to(port);
            let Ok(secured) = handshake(
                tcp.try_clone().unwrap(),
                "localhost",
                Duration::from_secs(10),
                client_config(),
                None,
            ) else {
                panic!("the handshake failed");
            };
            let handle = keep_stream(tcp, "server".to_owned(), Some(secured)).ok().unwrap();
            assert!(write(handle, b"x").is_ok());
            (handle, server)
        }

        /// A server on a port of its own that takes one connection, with the tests' certificate
        /// when `secure`, answers its one line with `pong`, and gives back what `looks` said each
        /// time it looked: as the connection came, and, with TLS, once the handshake was done. So
        /// a test sees, from the server's end, what its client was doing while it connected and
        /// while it shook hands. Connections to its port trust the tests' authority while the
        /// third thing it gives lives.
        pub(crate) fn answering_and_looking(
            secure: bool,
            looks: fn() -> i64,
        ) -> (u16, std::thread::JoinHandle<Vec<i64>>, http::tls::TrustedByTests) {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            let trusting = trusting_the_tests(&listener);
            let server = std::thread::spawn(move || {
                let (mut tcp, _) = listener.accept().unwrap();
                let mut seen = vec![looks()];
                let mut line = String::new();
                if !secure {
                    let _ = BufReader::new(&tcp).read_line(&mut line);
                    let _ = tcp.write_all(b"pong\n");
                    return seen;
                }
                let chain: Vec<CertificateDer<'static>> =
                    CertificateDer::pem_slice_iter(CHAIN).map(Result::unwrap).collect();
                let config = ServerConfig::builder_with_provider(provider())
                    .with_safe_default_protocol_versions()
                    .unwrap()
                    .with_no_client_auth()
                    .with_single_cert(chain, PrivateKeyDer::from_pem_slice(KEY).unwrap())
                    .unwrap();
                let mut connection = ServerConnection::new(Arc::new(config)).unwrap();
                while connection.is_handshaking() {
                    if connection.complete_io(&mut tcp).is_err() {
                        return seen;
                    }
                }
                seen.push(looks());
                let mut reader = BufReader::new(StreamOwned::new(connection, tcp));
                let _ = reader.read_line(&mut line);
                let _ = reader.get_mut().write_all(b"pong\n");
                let _ = reader.get_mut().flush();
                seen
            });
            (port, server, trusting)
        }

        /// Connects as `net.connect(host, port, tls=...)` does, says `ping`, and gives back the
        /// line it hears.
        pub(crate) fn connected_and_pinged(host: &str, port: u16, secure: bool) -> Result<Vec<u8>, Failure> {
            let made = super::super::connect(host, port, Duration::from_secs(20), secure)?;
            let handle = super::handle_of_made(&made);
            let heard = match write(handle, b"ping\n") {
                Ok(()) => read_line(handle, Some(Duration::from_secs(20))),
                Err(failure) => Err(failure),
            };
            close(handle);
            heard.map(Option::unwrap_or_default)
        }

        /// A server's TLS with the tests' own certificate for `localhost`.
        pub(crate) fn server_tls() -> Arc<dyn super::super::TlsServer> {
            let (chain, key) = (std::str::from_utf8(CHAIN).unwrap(), std::str::from_utf8(KEY).unwrap());
            let Ok(server) = http::tls::server(chain, key) else {
                panic!("the tests' certificate was refused")
            };
            server
        }

        /// What `accept` does with a connection it took for a server with TLS: the server's half of
        /// the handshake, and the connection's handle.
        pub(crate) fn taken_by(
            tls: &Arc<dyn super::super::TlsServer>,
            stream: TcpStream,
        ) -> Result<i64, Failure> {
            super::super::kept_after_accept(Some(tls), stream, "client".to_owned()).map(|(handle, _)| handle)
        }

        /// Reads a line from a connection a server took, answers `pong`, and closes it: whether a
        /// line came.
        pub(crate) fn answered(handle: i64) -> bool {
            let heard = matches!(read_line(handle, Some(Duration::from_secs(30))), Ok(Some(_)));
            let answered = heard && write(handle, b"pong\n").is_ok();
            close(handle);
            answered
        }

        /// A client that connects to `port`, shakes hands trusting the tests' certificate, says
        /// `ping`, and gives back the line it hears.
        pub(crate) fn pinged(port: u16) -> Result<Vec<u8>, String> {
            let tcp = connected_to(port);
            let copy = tcp.try_clone().map_err(|error| error.to_string())?;
            let secured = handshake(copy, "localhost", Duration::from_secs(30), client_config(), None)
                .map_err(|_| "the client's handshake failed".to_owned())?;
            let handle = keep_stream(tcp, "server".to_owned(), Some(secured))
                .map_err(|_| "the client's connection couldn't be kept".to_owned())?;
            let heard = match write(handle, b"ping\n") {
                Ok(()) => read_line(handle, Some(Duration::from_secs(30))),
                Err(failure) => Err(failure),
            };
            close(handle);
            match heard {
                Ok(Some(line)) => Ok(line),
                Ok(None) => Err("the server closed without answering".to_owned()),
                Err(_) => Err("the client heard no answer".to_owned()),
            }
        }

        /// While what this gives back lives, a connection made to the port `listener` is on trusts
        /// the tests' own certificate authority, and nothing else: for a test's own server there.
        /// Every other connection goes by what the system trusts. It holds a copy of the listener,
        /// so the port is the server's for as long.
        fn trusting_the_tests(listener: &TcpListener) -> http::tls::TrustedByTests {
            http::tls::TrustedByTests::begin(listener.try_clone().unwrap(), client_config())
        }

        /// An HTTPS server on a port of its own, with the tests' certificate for `localhost`, for
        /// `count` requests. It waits a moment before it reads each one's body, so a long one has to
        /// wait for room, and answers none until all have come whole, each with its own body back.
        /// Requests to that port trust the tests' authority while the second thing it gives lives.
        pub(crate) fn https_echoing(count: usize) -> (u16, http::tls::TrustedByTests) {
            use std::io::Read as _;
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            let trusting = trusting_the_tests(&listener);
            let chain: Vec<CertificateDer<'static>> =
                CertificateDer::pem_slice_iter(CHAIN).map(Result::unwrap).collect();
            let config = ServerConfig::builder_with_provider(provider())
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(chain, PrivateKeyDer::from_pem_slice(KEY).unwrap())
                .unwrap();
            let config = Arc::new(config);
            let all = Arc::new(std::sync::Barrier::new(count));
            std::thread::spawn(move || {
                for tcp in listener.incoming().flatten().take(count) {
                    let (config, all) = (Arc::clone(&config), Arc::clone(&all));
                    std::thread::spawn(move || {
                        let connection = ServerConnection::new(config).unwrap();
                        let mut reader = BufReader::new(StreamOwned::new(connection, tcp));
                        let mut length = 0;
                        loop {
                            let mut line = String::new();
                            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                                return;
                            }
                            if line == "\r\n" {
                                break;
                            }
                            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                                length = value.trim().parse().unwrap_or(0);
                            }
                        }
                        std::thread::sleep(Duration::from_millis(50));
                        let mut body = vec![0; length];
                        if reader.read_exact(&mut body).is_err() {
                            return;
                        }
                        all.wait();
                        let secured = reader.get_mut();
                        let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {length}\r\n\r\n");
                        let _ = secured.write_all(head.as_bytes());
                        let _ = secured.write_all(&body);
                        secured.conn.send_close_notify();
                        let _ = secured.flush();
                    });
                }
            });
            (port, trusting)
        }

        /// A TLS connection's handle, to a server that answers its one line only once `ready` is
        /// set, for a test that a read waiting for it parks.
        pub(crate) fn connected_once_ready(ready: &'static std::sync::atomic::AtomicBool) -> i64 {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            std::thread::spawn(move || {
                let chain: Vec<CertificateDer<'static>> =
                    CertificateDer::pem_slice_iter(CHAIN).map(Result::unwrap).collect();
                let config = ServerConfig::builder_with_provider(provider())
                    .with_safe_default_protocol_versions()
                    .unwrap()
                    .with_no_client_auth()
                    .with_single_cert(chain, PrivateKeyDer::from_pem_slice(KEY).unwrap())
                    .unwrap();
                let (tcp, _) = listener.accept().unwrap();
                let connection = ServerConnection::new(Arc::new(config)).unwrap();
                let mut secured = StreamOwned::new(connection, tcp);
                // The handshake's done by the first read, which waits for the client to write.
                let mut first = [0; 1];
                let _ = std::io::Read::read(&mut secured, &mut first);
                let started = std::time::Instant::now();
                while !ready.load(std::sync::atomic::Ordering::SeqCst)
                    && started.elapsed() < Duration::from_secs(5)
                {
                    std::thread::sleep(Duration::from_millis(1));
                }
                let _ = secured.write_all(b"late\n");
                let _ = secured.flush();
                std::thread::sleep(Duration::from_millis(200));
            });
            let tcp = connected_to(port);
            let Ok(secured) = handshake(
                tcp.try_clone().unwrap(),
                "localhost",
                Duration::from_secs(10),
                client_config(),
                None,
            ) else {
                panic!("the handshake failed");
            };
            let handle = keep_stream(tcp, "server".to_owned(), Some(secured)).ok().unwrap();
            assert!(write(handle, b"x").is_ok());
            handle
        }

        /// A `Certificate`, as a program makes one.
        fn certificate() -> Value {
            let text = |bytes: &[u8]| Value::str(std::str::from_utf8(bytes).unwrap());
            let key = Value::made("Secret", vec![("_value", text(KEY))]);
            Value::made("Certificate", vec![("chain", text(CHAIN)), ("key", key)])
        }

        fn field(made: &Value, name: &str) -> i64 {
            let Value::Ok(made) = made else { panic!("it failed: {made:?}") };
            let Value::Instance(instance) = made.as_ref() else { panic!("not an instance: {made:?}") };
            let Some(Value::Int(number)) = instance.field(name) else { panic!("no {name}") };
            *number
        }

        /// `net.listen(port, tls=...)` and `server.accept()`: the connection it gives reads and
        /// writes through TLS.
        #[test]
        fn a_server_listening_with_a_certificate_accepts_over_tls() {
            http::use_tls();
            let span = Span::default();
            let listened = real_net("_listen.tls", &[Value::Int(0), certificate()], None, span).unwrap();
            let (server, port) = (field(&listened, "_handle"), field(&listened, "port"));
            let serving = std::thread::spawn(move || {
                let span = Span::default();
                let taken = real_server(server, "_accept", &[], Some(Duration::from_secs(10)), span).unwrap();
                let conn = field(&taken, "_handle");
                let line =
                    real_connection(conn, "read_line", &[], Some(Duration::from_secs(10)), span).unwrap();
                let wrote = real_connection(conn, "write", &[Value::str("pong\n")], None, span).unwrap();
                close(conn);
                (line == Value::Ok(Rc::new(Value::str("ping"))), matches!(wrote, Value::Ok(_)))
            });
            let tcp = connected_to(u16::try_from(port).unwrap());
            let Ok(mut secured) = handshake(tcp, "localhost", Duration::from_secs(10), client_config(), None)
            else {
                panic!("the handshake failed");
            };
            secured.write_all(b"ping\n").unwrap();
            secured.flush().unwrap();
            let mut back = String::new();
            secured.read_line(&mut back).unwrap();
            assert_eq!(back, "pong\n");
            assert_eq!(serving.join().unwrap(), (true, true));
            close(server);
        }

        /// `tls=` takes a `Certificate`, and one that can't be read is the program's failure.
        #[test]
        fn a_certificate_that_cant_be_read_fails_as_tls() {
            http::use_tls();
            let span = Span::default();
            let bad = Value::made(
                "Certificate",
                vec![
                    ("chain", Value::str("nope")),
                    ("key", Value::made("Secret", vec![("_value", Value::str("nope"))])),
                ],
            );
            let listened = real_net("_listen.tls", &[Value::Int(0), bad], None, span).unwrap();
            let Value::Err(error) = listened else { panic!("it listened") };
            assert!(format!("{error:?}").contains("Tls"), "{error:?}");
            assert!(real_net("_listen.tls", &[Value::Int(0), Value::Int(1)], None, span).is_err());
        }

        fn shake_trusting_the_tests(
            tcp: TcpStream,
            host: &str,
            timeout: Duration,
            waits: Option<super::super::Waits<'_>>,
        ) -> Result<Box<dyn crate::stdlib::net::SecureStream>, Failure> {
            handshake(tcp, host, timeout, client_config(), waits)
        }

        const WAIT: Option<Duration> = Some(Duration::from_secs(10));

        /// A plain connection and a server that writes `first` when it's accepted one, and says so
        /// on the channel, then does TLS.
        fn accepted_writing(
            first: &'static [u8],
        ) -> (i64, std::sync::mpsc::Receiver<()>, std::thread::JoinHandle<Result<String, String>>) {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            let (wrote, written) = std::sync::mpsc::channel();
            let server = std::thread::spawn(move || {
                let chain: Vec<CertificateDer<'static>> =
                    CertificateDer::pem_slice_iter(CHAIN).map(Result::unwrap).collect();
                let config = ServerConfig::builder_with_provider(provider())
                    .with_safe_default_protocol_versions()
                    .unwrap()
                    .with_no_client_auth()
                    .with_single_cert(chain, PrivateKeyDer::from_pem_slice(KEY).unwrap())
                    .unwrap();
                let (mut tcp, _) = listener.accept().unwrap();
                tcp.write_all(first).map_err(|error| error.to_string())?;
                wrote.send(()).map_err(|error| error.to_string())?;
                let connection = ServerConnection::new(Arc::new(config)).unwrap();
                let mut reader = BufReader::new(StreamOwned::new(connection, tcp));
                let mut line = String::new();
                reader.read_line(&mut line).map_err(|error| error.to_string())?;
                let secured = reader.get_mut();
                secured.write_all(b"pong\n").map_err(|error| error.to_string())?;
                secured.flush().map_err(|error| error.to_string())?;
                Ok(line)
            });
            let handle = keep_stream(connected_to(port), "server".to_owned(), None).ok().unwrap();
            (handle, written, server)
        }

        /// What Postgres does: one byte read in plain text, then TLS for the rest.
        #[test]
        fn a_plain_connection_can_start_tls_after_reading_all_it_was_sent() {
            let (handle, written, server) = accepted_writing(b"S");
            written.recv().unwrap();
            assert_eq!(read(handle, 1, WAIT).ok().unwrap(), b"S");
            assert!(upgrade(handle, "localhost", Duration::from_secs(10), shake_trusting_the_tests).is_ok());
            assert!(write(handle, b"ping\n").is_ok());
            assert_eq!(read_line(handle, WAIT).ok().unwrap().unwrap(), b"pong\n");
            assert_eq!(server.join().unwrap().unwrap(), "ping\n");
            close(handle);
        }

        /// Bytes that came before the handshake aren't TLS's to vouch for: someone on the wire can
        /// send them after the one byte a client waits for, as in the Postgres client's CVE-2021-23214.
        #[test]
        fn starting_tls_is_refused_when_plain_bytes_are_waiting_in_the_buffer() {
            let (handle, written, _server) = accepted_writing(b"Sinjected");
            written.recv().unwrap();
            assert_eq!(read(handle, 1, WAIT).ok().unwrap(), b"S");
            let got = upgrade(handle, "localhost", Duration::from_secs(10), shake_trusting_the_tests);
            assert!(
                matches!(&got, Err(Failure::Tls(reason)) if reason.contains("before TLS")),
                "it was let through"
            );
            close(handle);
        }

        /// `start_tls` trusts what the system does, which doesn't include the tests' authority.
        #[test]
        fn starting_tls_checks_the_certificate() {
            http::use_tls();
            let (handle, written, _server) = accepted_writing(b"S");
            written.recv().unwrap();
            assert_eq!(read(handle, 1, WAIT).ok().unwrap(), b"S");
            let got = real_connection(handle, "start_tls", &[Value::str("localhost")], WAIT, Span::default())
                .unwrap();
            let Value::Err(error) = got else { panic!("it trusted a certificate nothing vouches for") };
            assert!(format!("{error:?}").contains("Tls"), "{error:?}");
            close(handle);
        }

        #[test]
        fn starting_tls_twice_is_refused() {
            let (handle, written, _server) = accepted_writing(b"");
            written.recv().unwrap();
            assert!(upgrade(handle, "localhost", Duration::from_secs(10), shake_trusting_the_tests).is_ok());
            let got = upgrade(handle, "localhost", Duration::from_secs(10), shake_trusting_the_tests);
            assert!(
                matches!(&got, Err(Failure::Tls(reason)) if reason.contains("already")),
                "it did it twice"
            );
            close(handle);
        }

        /// A connection with TLS over it reads and writes as a plain one does.
        #[test]
        fn a_tls_connection_reads_and_writes_lines() {
            let (port, server) = serving();
            let tcp = connected_to(port);
            let secured = handshake(
                tcp.try_clone().unwrap(),
                "localhost",
                Duration::from_secs(10),
                client_config(),
                None,
            );
            let Ok(secured) = secured else { panic!("the handshake failed") };
            let handle = keep_stream(tcp, "server".to_owned(), Some(secured)).ok().unwrap();
            assert!(write(handle, b"ping\n").is_ok());
            let back = read_line(handle, Some(Duration::from_secs(10)));
            assert_eq!(back.ok().flatten().as_deref(), Some(&b"pong\n"[..]));
            close(handle);
            assert_eq!(server.join().unwrap().unwrap(), "ping\n");
        }

        /// The certificate is for `localhost`, so a connection that asked for anything else fails:
        /// checking can't be left out, whatever the host.
        #[test]
        fn a_certificate_for_another_name_is_refused() {
            let (port, server) = serving();
            let got = handshake(
                connected_to(port),
                "fennec.example",
                Duration::from_secs(10),
                client_config(),
                None,
            );
            let Err(Failure::Tls(reason)) = &got else { panic!("the handshake didn't fail as TLS") };
            assert_eq!(reason, "the server's certificate isn't for `fennec.example`");
            drop(got);
            assert!(server.join().unwrap().is_err());
        }

        /// A clock two centuries on, when the tests' certificate has expired.
        #[derive(Debug)]
        struct Later;

        impl rustls::time_provider::TimeProvider for Later {
            fn current_time(&self) -> Option<rustls::pki_types::UnixTime> {
                // 2200-01-01, past the certificate's end in 2126.
                Some(rustls::pki_types::UnixTime::since_unix_epoch(Duration::from_hours(2_016_144)))
            }
        }

        /// A certificate past its end is refused, however trusted its authority, saying so.
        #[test]
        fn an_expired_certificate_is_refused() {
            let (port, server) = serving();
            let mut roots = RootCertStore::empty();
            for certificate in CertificateDer::pem_slice_iter(CA) {
                roots.add(certificate.unwrap()).unwrap();
            }
            let config = ClientConfig::builder_with_details(provider(), Arc::new(Later))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(roots)
                .with_no_client_auth();
            let got =
                handshake(connected_to(port), "localhost", Duration::from_secs(10), Arc::new(config), None);
            let Err(Failure::Tls(reason)) = &got else { panic!("the handshake didn't fail as TLS") };
            assert_eq!(reason, "the server's certificate has expired");
            drop(got);
            assert!(server.join().unwrap().is_err());
        }

        /// A client that doesn't trust the certificate's authority refuses it.
        #[test]
        fn a_certificate_from_an_authority_nobody_trusts_is_refused() {
            let (port, server) = serving();
            let config = ClientConfig::builder_with_provider(provider())
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(RootCertStore::empty())
                .with_no_client_auth();
            let got =
                handshake(connected_to(port), "localhost", Duration::from_secs(10), Arc::new(config), None);
            let Err(Failure::Tls(reason)) = &got else { panic!("the handshake didn't fail as TLS") };
            assert_eq!(reason, "the server's certificate isn't from an authority this system trusts");
            drop(got);
            assert!(server.join().unwrap().is_err());
        }
    }
}
