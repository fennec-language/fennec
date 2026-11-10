//! `std.net`: TCP and UDP through the `Net` capability. A `Connection`, `Server` or `UdpSocket` holds
//! a private handle into the program's table of open sockets, so code that was never given `Net` can't
//! make one up to reach a socket. In a test, the network is in memory, in `Fake`.
//! Nothing else runs while a test waits, so a read that would wait forever stops the test and
//! says why, and a server with no connection waiting is done.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use fennec_syntax::{Diagnostic, Span};

use super::http;
use crate::methods::no_method;
use crate::value::{Capability, Value};

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
}

/// What a connection reads through: the socket itself, or TLS over it, which writes through the
/// same, so a TLS connection's reads and writes take turns.
enum Reading {
    Plain(BufReader<TcpStream>),
    Tls(Box<dyn SecureStream>),
}

/// A connection with TLS over it, read and written as a plain one is. Only the TLS module makes
/// one, so a program that never asks for TLS links none of it.
pub(crate) trait SecureStream: BufRead + Write + Send {
    /// The socket it reads through. It's a copy of the connection's own, and on Windows a copy
    /// keeps its own timeouts, so a read's timeout goes on this one.
    fn socket(&self) -> &TcpStream;
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
    fn accepted(&self, tcp: TcpStream, timeout: Duration) -> Result<Box<dyn SecureStream>, Failure>;
}

/// A real UDP socket. Only one thread reads at a time, as how long a read waits is the socket's.
struct Udp {
    socket: UdpSocket,
    receiving: Mutex<()>,
}

/// A port listened on, with the TLS each connection it takes starts with, if it has any.
struct Listening {
    listener: TcpListener,
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
            ("Connection", "read_line", []) => or_timed_out(self.read_line(handle, false, span), timeout)?,
            ("Connection", "read_line.crlf", [Value::Bool(crlf)]) => {
                or_timed_out(self.read_line(handle, *crlf, span), timeout)?
            }
            ("Connection", "start_tls", [Value::Str(_)]) => self.start_tls(handle).map(|()| Value::Unit),
            ("Connection", "write", [data @ (Value::Str(_) | Value::Bytes(_))]) => {
                self.write(handle, data_of(data)).map(|()| Value::Unit)
            }
            ("UdpSocket", "receive", []) => self.receive(handle, span)?.map(packet_value),
            ("UdpSocket", "send_to", [data @ (Value::Str(_) | Value::Bytes(_)), Value::Str(address)]) => {
                self.send_to(handle, data_of(data), address).map(|()| Value::Unit)
            }
            ("Server", "_accept", []) => return Ok(made_or_failed(self.accept(handle).map(connection_made))),
            // A test's server never drains: its connections are taken one at a time.
            ("Server", "_stop_on", _) => return Ok(Value::Unit),
            ("Connection", "draining", []) => return Ok(Value::Bool(false)),
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
        let Some(stream) = self.streams.get(&handle) else { return Err(Failure::Closed) };
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
    /// else runs while a test waits.
    pub(crate) fn receive(
        &mut self,
        handle: i64,
        span: Span,
    ) -> Result<Result<(Vec<u8>, String), Failure>, Diagnostic> {
        let fake = self;
        let Some((port, waiting)) = fake.udp.get_mut(&handle) else { return Ok(Err(Failure::Closed)) };
        match waiting.pop_front() {
            Some(packet) => Ok(Ok(packet)),
            None => Err(waits_forever(
                &format!(
                    "this socket on port {port} in the test's fake network would wait forever for a packet"
                ),
                span,
            )),
        }
    }

    /// Sends a packet in a test. Sent to a port nothing has, it's lost, as UDP's are.
    pub(crate) fn send_to(&mut self, handle: i64, data: &[u8], address: &str) -> Result<(), Failure> {
        let fake = self;
        let Some((from, _)) = fake.udp.get(&handle) else { return Err(Failure::Closed) };
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
        let Some((port, waiting)) = fake.servers.get_mut(&handle) else { return Err(Failure::Closed) };
        waiting.pop_front().ok_or_else(|| {
            Failure::Other(format!(
                "no connection is waiting for the test's fake server on port {port}, and none can come while a test waits"
            ))
        })
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
        let Some(stream) = fake.streams.get_mut(&handle) else { return Ok(Err(Failure::Closed)) };
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
            return Ok(Err(Failure::Other(format!("a line was longer than {MOST_LINE_BYTES} bytes"))));
        }
        let line = self.read(handle, None, span)?;
        Ok(line.and_then(|line| match line.as_deref().and_then(|line| line_refused(line, crlf)) {
            Some(why) => Err(Failure::Other(why)),
            None => Ok(line_value(line.as_deref())),
        }))
    }

    pub(crate) fn write(&mut self, handle: i64, data: &[u8]) -> Result<(), Failure> {
        let fake = self;
        let Some(stream) = fake.streams.get(&handle) else { return Err(Failure::Closed) };
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
    Other(String),
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

fn duration_of(value: &Value, span: Span) -> Result<Duration, Diagnostic> {
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
        ("read_line", []) => read_line(handle, timeout).map(|line| line_value(line.as_deref())),
        ("read_line.crlf", [Value::Bool(crlf)]) => {
            read_line_crlf(handle, timeout, *crlf).map(|line| line_value(line.as_deref()))
        }
        ("start_tls", [Value::Str(host)]) => {
            start_tls(handle, host, timeout.unwrap_or(CONNECT_TIMEOUT)).map(|()| Value::Unit)
        }
        ("write", [data @ (Value::Str(_) | Value::Bytes(_))]) => {
            write_within(handle, data_of(data), timeout).map(|()| Value::Unit)
        }
        ("close", []) => {
            close(handle);
            return Ok(Value::Unit);
        }
        // What a task serving a connection makes its `Connection` from.
        ("peer", []) => return Ok(stream(handle).map_or_else(|_| Value::str(""), |it| Value::str(&it.peer))),
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
    };
    let sends_json = named.iter().any(|(name, _)| *name == "json");
    for (name, value) in named {
        match (*name, value) {
            ("timeout", value) => request.timeout = duration_of(value, span)?,
            // Native code and `http_method` both give `json=` as the JSON text already.
            ("body" | "json", Value::Str(text)) => request.body = text.as_bytes().to_vec(),
            ("body", Value::Bytes(data)) => request.body = data.to_vec(),
            ("params", Value::Dict(pairs)) => request.params = text_pairs(pairs, span)?,
            ("headers", Value::Dict(pairs)) => request.headers = text_pairs(pairs, span)?,
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
    let mut joined: Vec<(String, String)> = Vec::new();
    let mut cookies = Vec::new();
    for (name, value) in &answer.headers {
        if name == "set-cookie" {
            cookies.push(Value::str(value));
        }
        match joined.iter_mut().find(|(seen, _)| seen == name) {
            Some((_, held)) => {
                held.push_str(", ");
                held.push_str(value);
            }
            None => joined.push((name.clone(), value.clone())),
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
    let parts: Vec<&str> = method.split('.').collect();
    let named = parts.iter().rev().take_while(|part| NAMED.contains(part)).count().min(args.len());
    let first_named = parts.len() - named;
    let Some(at) = parts.iter().skip(first_named).position(|part| *part == "timeout") else {
        return Ok((method.to_owned(), args.to_vec(), None));
    };
    let mut args = args.to_vec();
    let timeout = args.remove(args.len() - named + at);
    let mut parts = parts;
    parts.remove(first_named + at);
    Ok((parts.join("."), args, Some(duration_of(&timeout, span)?)))
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

/// The most bytes a line read from a connection may hold, its ending included. A line longer
/// than this fails, rather than take as much memory as whoever's sending it likes.
pub(crate) const MOST_LINE_BYTES: usize = 1 << 20;

/// Why a line read fails: it's too long, or, with `crlf`, it ended with no `\r` before its `\n`.
pub(crate) fn line_refused(line: &[u8], crlf: bool) -> Option<String> {
    if line.len() > MOST_LINE_BYTES {
        return Some(format!("a line was longer than {MOST_LINE_BYTES} bytes"));
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
    ("Connection", vec![("_handle", Value::Int(connection)), ("peer", Value::str(&peer))])
}

fn packet_value((data, sender): (Vec<u8>, String)) -> Value {
    Value::made("Packet", vec![("data", Value::Bytes(Rc::from(data))), ("sender", Value::str(&sender))])
}

fn made_or_failed(found: Result<Made, Failure>) -> Value {
    done_or_failed(found.map(|(type_name, fields)| Value::made(type_name, fields)))
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
        Failure::Other(reason) => ("Other", vec![("reason", Value::str(reason))]),
    };
    Value::Err(Rc::new(Value::made_case("NetError", case, fields)))
}

fn connect(host: &str, port: u16, timeout: Duration, tls: bool) -> Result<Made, Failure> {
    // A task parks while its lookup, connect and handshake wait, on a thread that may block,
    // rather than holding its worker.
    let opened = move |host: &str| {
        let (stream, peer) = connected(host, port, timeout)?;
        let secured = if tls { Some(secured(&stream, host, timeout)?) } else { None };
        Ok::<_, Failure>((stream, peer, secured))
    };
    let (stream, peer, secured) = if task_parks() {
        let host = host.to_owned();
        off_thread(move || opened(&host))?
    } else {
        opened(host)?
    };
    let handle = keep_stream(stream, peer.clone(), secured)?;
    Ok(connection_made((handle, peer)))
}

/// TLS over a connection just made, its certificate checked against what the system trusts.
fn secured(stream: &TcpStream, host: &str, timeout: Duration) -> Result<Box<dyn SecureStream>, Failure> {
    let tcp = stream.try_clone().map_err(|error| Failure::Other(error.to_string()))?;
    http::raw_secured(tcp, host, timeout)
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
    let tcp = stream.socket.try_clone().map_err(|error| Failure::Other(error.to_string()))?;
    let host = host.to_owned();
    let handshake = move || shake(tcp, &host, timeout);
    // A task parks while the handshake waits, on a thread that may block, as `connect` does.
    let secured = if task_parks() { off_thread(handshake)? } else { handshake()? };
    *reader = Reading::Tls(secured);
    Ok(())
}

/// A connection to `host` on `port`, looked up and connected, blocking its thread.
fn connected(host: &str, port: u16, timeout: Duration) -> Result<(TcpStream, String), Failure> {
    let addresses: Vec<_> =
        (host, port).to_socket_addrs().map_err(|_| Failure::NotFound(host.to_owned()))?.collect();
    if addresses.is_empty() {
        return Err(Failure::NotFound(host.to_owned()));
    }
    let mut failure = Failure::NotFound(host.to_owned());
    for address in addresses {
        match TcpStream::connect_timeout(&address, timeout) {
            Ok(stream) => {
                let peer =
                    stream.peer_addr().map_or_else(|_| format!("{host}:{port}"), |peer| peer.to_string());
                return Ok((stream, peer));
            }
            Err(error) => {
                failure = match error.kind() {
                    ErrorKind::ConnectionRefused => Failure::Refused(host.to_owned(), i64::from(port)),
                    ErrorKind::TimedOut | ErrorKind::WouldBlock => Failure::TimedOut(timeout),
                    _ => Failure::Other(error.to_string()),
                };
            }
        }
    }
    Err(failure)
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
    bound_send_buffer(&socket);
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
    };
    Ok(open(|open| {
        let handle = fresh_handle(open);
        open.insert(handle, Socket::Stream(Arc::new(stream)));
        handle
    }))
}

/// TLS over a connection, as `secured` makes it.
type Secured = Box<dyn SecureStream>;
fn listen(port: u16, tls: Option<Arc<dyn TlsServer>>) -> Result<Made, Failure> {
    let listener = TcpListener::bind(("0.0.0.0", port)).map_err(|error| match error.kind() {
        ErrorKind::AddrInUse => Failure::Other(format!("port {port} is already in use")),
        _ => Failure::Other(error.to_string()),
    })?;
    let port = listener.local_addr().map_or(port, |address| address.port());
    let handle = open(|open| {
        let handle = fresh_handle(open);
        open.insert(handle, Socket::Listener(Arc::new(Listening { listener, tls, stop: Mutex::new(None) })));
        handle
    });
    Ok(("Server", vec![("_handle", Value::Int(handle)), ("port", Value::Int(i64::from(port)))]))
}

fn stream(handle: i64) -> Result<Arc<Stream>, Failure> {
    match open(|open| open.get(&handle).cloned()) {
        Some(Socket::Stream(stream)) => Ok(stream),
        _ => Err(Failure::Closed),
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

fn read(handle: i64, count: usize, timeout: Option<Duration>) -> Result<Vec<u8>, Failure> {
    let stream = stream(handle)?;
    // With nothing to give, it mustn't wait for something to come.
    if count == 0 {
        return Ok(Vec::new());
    }
    let mut data = vec![0; count];
    let read = reading(&stream, timeout, |reader| reader.read(&mut data))?;
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
    // What a read cut short by having to wait got is kept in `line`, and the next adds to it. It
    // reads at most what's left of the most a line may hold, and one more to tell it's too long.
    let read = reading(&stream, timeout, |reader| {
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
    let stream = stream(handle)?;
    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    stream.read_since_write.store(false, Ordering::SeqCst);
    let _writing = lock_parking(&stream.writing);
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
                if wait_to_write(socket, deadline) {
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
    data: &[u8],
    (timeout, deadline): (Option<Duration>, Option<Instant>),
) -> Result<(), Failure> {
    let mut given = false;
    loop {
        let sent = {
            let mut reader = stream.reader.lock().unwrap_or_else(PoisonError::into_inner);
            let Reading::Tls(secured) = &mut *reader else { return Err(Failure::Closed) };
            if !given {
                secured.write_all(data).map_err(|error| Failure::of(&error, timeout))?;
                given = true;
            }
            secured.flush()
        };
        match sent {
            Ok(()) => return Ok(()),
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                if wait_to_write(&stream.socket, deadline) {
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
        if !socket_parks() || !yield_to_others() {
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

/// Whether this call is in a task that can park while it waits for something to look up or connect
/// to, on a thread that may block.
fn task_parks() -> bool {
    #[cfg(any(test, feature = "threads"))]
    {
        crate::embed::serve::can_park()
    }
    #[cfg(not(any(test, feature = "threads")))]
    {
        false
    }
}

/// Whether this call is in a task that can park on a socket itself: a task, on Unix, whose poll can
/// be told of the socket. On Windows, a socket's wait blocks its thread.
fn socket_parks() -> bool {
    cfg!(unix) && task_parks()
}

/// `work`, run on a thread that may block while this task parks, or here when it can't.
fn off_thread<R: Send + 'static>(work: impl FnOnce() -> R + Send + 'static) -> R {
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
fn reading<T>(
    stream: &Arc<Stream>,
    timeout: Option<Duration>,
    mut read: impl FnMut(&mut dyn BufRead) -> std::io::Result<T>,
) -> Result<T, Failure> {
    let mut attempt = || {
        let mut reader = stream.reader.lock().unwrap_or_else(PoisonError::into_inner);
        match &mut *reader {
            Reading::Plain(plain) => read(plain),
            Reading::Tls(secured) => read(&mut **secured),
        }
    };
    if !socket_parks() {
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
    let socket = &stream.socket;
    socket.set_nonblocking(true).map_err(|error| Failure::Other(error.to_string()))?;
    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    let done = loop {
        match attempt() {
            Ok(done) => break Ok(done),
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                if deadline.is_some_and(|deadline| deadline <= Instant::now()) {
                    break Err(Failure::TimedOut(timeout.unwrap_or_default()));
                }
                if park_until_readable(socket, deadline) {
                    break Err(Failure::TimedOut(timeout.unwrap_or_default()));
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => break Err(Failure::of(&error, timeout)),
        }
    };
    let blocking = socket.set_nonblocking(false);
    let done = done?;
    blocking.map_err(|error| Failure::Other(error.to_string()))?;
    Ok(done)
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

/// The connections launched tasks are blocked reading, each by its task's stop. On Windows a task's
/// read blocks its thread, so nothing run there can end it: its stop shuts the socket instead.
#[cfg(any(test, feature = "threads"))]
static BLOCKED_READS: Mutex<Vec<(usize, Arc<Stream>)>> = Mutex::new(Vec::new());

/// A launched task's read blocking its thread, noted while it lasts.
struct BlockedRead {
    #[cfg(any(test, feature = "threads"))]
    stop: Option<usize>,
}

impl BlockedRead {
    fn of(stream: &Arc<Stream>) -> BlockedRead {
        #[cfg(any(test, feature = "threads"))]
        {
            let stop = crate::embed::threads::task_stop().map(|stop| Arc::as_ptr(&stop).addr());
            if let Some(stop) = stop {
                BLOCKED_READS.lock().unwrap_or_else(PoisonError::into_inner).push((stop, Arc::clone(stream)));
            }
            BlockedRead { stop }
        }
        #[cfg(not(any(test, feature = "threads")))]
        {
            let _ = stream;
            BlockedRead {}
        }
    }
}

impl Drop for BlockedRead {
    fn drop(&mut self) {
        #[cfg(any(test, feature = "threads"))]
        if let Some(stop) = self.stop {
            let mut blocked = BLOCKED_READS.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(at) = blocked.iter().position(|(task, _)| *task == stop) {
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
    }
}

/// Parks this task until `socket` may be readable or `deadline` passes: true if it passed, or the
/// task was cancelled.
#[cfg(all(unix, any(test, feature = "threads")))]
fn park_until_readable(socket: &TcpStream, deadline: Option<Instant>) -> bool {
    use crate::embed::serve::Parked;
    let fd = std::os::fd::AsRawFd::as_raw_fd(socket);
    crate::embed::serve::park_on_fd(fd, mio::Interest::READABLE, deadline) != Parked::Ready
}

#[cfg(not(all(unix, any(test, feature = "threads"))))]
fn park_until_readable(_socket: &TcpStream, _deadline: Option<Instant>) -> bool {
    false
}

/// Waits until `socket` may take more to write: parked in a task that can, and otherwise a moment,
/// as only a socket another task made not block gets here.
/// Waits for room to write, or until `deadline` passes: true once it has.
fn wait_to_write(socket: &TcpStream, deadline: Option<Instant>) -> bool {
    if deadline.is_some_and(|deadline| deadline <= Instant::now()) {
        return true;
    }
    #[cfg(all(unix, any(test, feature = "threads")))]
    if socket_parks() {
        let fd = std::os::fd::AsRawFd::as_raw_fd(socket);
        let _ = crate::embed::serve::park_on_fd(fd, mio::Interest::WRITABLE, deadline);
        return deadline.is_some_and(|deadline| deadline <= Instant::now());
    }
    let _ = socket;
    std::thread::sleep(Duration::from_millis(1));
    false
}

fn accept(handle: i64, timeout: Option<Duration>) -> Result<(i64, String), Failure> {
    let Some(Socket::Listener(listening)) = open(|open| open.get(&handle).cloned()) else {
        return Err(Failure::Closed);
    };
    let listener = &listening.listener;
    let accepted = match timeout {
        None => listener.accept().map_err(|error| Failure::of(&error, None))?,
        Some(limit) => {
            // A listener has no timeout of its own, so it's asked again until one passes.
            listener.set_nonblocking(true).map_err(|error| Failure::Other(error.to_string()))?;
            let started = Instant::now();
            let accepted = loop {
                match listener.accept() {
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
            accepted?
        }
    };
    let (stream, peer) = accepted;
    stream.set_nonblocking(false).map_err(|error| Failure::Other(error.to_string()))?;
    let peer = peer.to_string();
    let secured = match &listening.tls {
        Some(tls) => Some(accepted_securely(tls, &stream)?),
        None => None,
    };
    Ok((keep_stream_as(stream, peer.clone(), secured, true)?, peer))
}

/// How long a connection a server took has to finish its handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// The server's half of a handshake over a connection just taken, on a thread that may block when
/// this is a task.
fn accepted_securely(tls: &Arc<dyn TlsServer>, stream: &TcpStream) -> Result<Secured, Failure> {
    let tcp = stream.try_clone().map_err(|error| Failure::Other(error.to_string()))?;
    if task_parks() {
        let tls = Arc::clone(tls);
        return off_thread(move || tls.accepted(tcp, HANDSHAKE_TIMEOUT));
    }
    tls.accepted(tcp, HANDSHAKE_TIMEOUT)
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
        let _ = TcpStream::connect_timeout(&address, Duration::from_secs(1));
        if let Some(stream) = being_served().filter(|stream| !stream.read_since_write.load(Ordering::SeqCst))
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
        open.insert(handle, Socket::Udp(Arc::new(Udp { socket, receiving: Mutex::new(()) })));
        handle
    });
    Ok(("UdpSocket", vec![("_handle", Value::Int(handle)), ("port", Value::Int(i64::from(port)))]))
}

fn udp_socket(handle: i64) -> Result<Arc<Udp>, Failure> {
    match open(|open| open.get(&handle).cloned()) {
        Some(Socket::Udp(udp)) => Ok(udp),
        _ => Err(Failure::Closed),
    }
}

/// The next packet and who sent it. It looks up its socket each time it looks for a packet, so
/// closing the socket on another thread ends the wait.
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
    udp.socket.send_to(data, target).map(|_| ()).map_err(|error| Failure::of(&error, None))
}

/// Closes a socket. Whoever else was using it finds out: a read waiting on another thread ends.
fn close(handle: i64) {
    if let Some(Socket::Stream(stream)) = open(|open| open.remove(&handle)) {
        if stream.served && task_parks() {
            let lingering = Arc::clone(&stream);
            off_thread(move || linger(&lingering.socket));
        } else if stream.served {
            linger(&stream.socket);
        }
        // It may be closed already from the other side, which is what closing wanted.
        let _ = stream.socket.shutdown(Shutdown::Both);
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
fn linger(socket: &TcpStream) {
    let _ = socket.shutdown(Shutdown::Write);
    let until = Instant::now() + LINGER;
    let mut sink = [0_u8; 16 * 1024];
    loop {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() || socket.set_read_timeout(Some(LINGER_QUIET.min(left))).is_err() {
            return;
        }
        match (&*socket).read(&mut sink) {
            Ok(0) => return,
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            // Gone quiet, timed out or reset: nothing more is coming that closing would lose.
            Err(_) => return,
        }
    }
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
            Some((listening.listener.try_clone().ok()?, crate::embed::serve::Listened { tls, stop }))
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
            super::timed("read_line.crlf.timeout", &[Value::Bool(true), wait], span).unwrap();
        assert_eq!(
            (method.as_str(), args, timeout),
            ("read_line.crlf", vec![Value::Bool(true)], Some(std::time::Duration::from_nanos(5)))
        );
        let (method, args, timeout) = super::timed("read_line", &[], span).unwrap();
        assert_eq!((method.as_str(), args, timeout), ("read_line", vec![], None));
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

    /// UDP has no `shutdown`, so a read on another thread has to find out for itself.
    #[test]
    fn closing_a_udp_socket_ends_a_read_waiting_on_another_thread() {
        let Ok(made) = udp(0) else { panic!("no UDP socket") };
        let handle = handle_of_made(&made);
        let waiting = std::thread::spawn(move || receive(handle, None));
        std::thread::sleep(std::time::Duration::from_millis(20));
        close(handle);
        assert!(matches!(waiting.join().ok(), Some(Err(Failure::Closed))));
    }

    #[test]
    fn a_udp_read_gives_up_when_told_to() {
        let Ok(made) = udp(0) else { panic!("no UDP socket") };
        let handle = handle_of_made(&made);
        let limit = std::time::Duration::from_millis(5);
        assert!(matches!(receive(handle, Some(limit)), Err(Failure::TimedOut(after)) if after == limit));
        close(handle);
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
            let Ok(secured) =
                handshake(tcp.try_clone().unwrap(), "localhost", Duration::from_secs(10), client_config())
            else {
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
            let Ok(mut secured) = handshake(tcp, "localhost", Duration::from_secs(10), client_config())
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
        ) -> Result<Box<dyn crate::stdlib::net::SecureStream>, Failure> {
            handshake(tcp, host, timeout, client_config())
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
            let secured =
                handshake(tcp.try_clone().unwrap(), "localhost", Duration::from_secs(10), client_config());
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
            let got =
                handshake(connected_to(port), "fennec.example", Duration::from_secs(10), client_config());
            assert!(matches!(got, Err(Failure::Tls(_))), "the handshake didn't fail as TLS");
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
            let got = handshake(connected_to(port), "localhost", Duration::from_secs(10), Arc::new(config));
            assert!(matches!(got, Err(Failure::Tls(_))), "the handshake didn't fail as TLS");
            drop(got);
            assert!(server.join().unwrap().is_err());
        }
    }
}
