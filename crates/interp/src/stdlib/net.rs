//! `std.net`: TCP and UDP through the `Net` capability. A `Connection`, `Server` or `UdpSocket` holds
//! a private handle into the program's table of open sockets, so code that was never given `Net` can't
//! make one up to reach a socket. What's real here needs nothing of the interpreter, as native
//! code runs it too. In a test, the network is in memory, in `Fake`.
//! Nothing else runs while a test waits, so a read that would wait forever stops the test and
//! says why, and a server with no connection waiting is done.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::rc::Rc;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use fennec_syntax::{Diagnostic, Span};

use super::{Module, http, no_constants, no_functions};
use crate::call::CallArg;
use crate::interp::Interp;
use crate::methods::no_method;
use crate::value::{Capability, Instance, Value};

pub(crate) const MODULE: Module = Module {
    functions: &[],
    call: no_functions,
    constant: no_constants,
    methods: &[
        ("Connection", connection_method),
        ("Server", server_method),
        ("UdpSocket", udp_method),
        ("Response", response_method),
    ],
};

/// How long connecting waits unless told otherwise.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

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
    writing: Mutex<()>,
}

/// What a connection reads through: the socket itself, or TLS over it, which writes through the
/// same, so a TLS connection's reads and writes take turns.
enum Reading {
    Plain(BufReader<TcpStream>),
    #[cfg(not(target_arch = "wasm32"))]
    Tls(Box<dyn SecureStream>),
}

/// A connection with TLS over it, read and written as a plain one is. Only the TLS module makes
/// one, so a program that never asks for TLS links none of it.
pub(crate) trait SecureStream: BufRead + Write + Send {}

impl<T: BufRead + Write + Send> SecureStream for T {}

/// What a TLS session reads from and writes to: a socket that doesn't block.
pub(crate) trait Transport: Read + Write {}

impl<T: Read + Write> Transport for T {}

/// One connection's TLS on a server, over a socket that doesn't block: anything that has to wait
/// says `WouldBlock`, for the task to park and try again.
pub(crate) trait ServerSession: Send {
    /// Does as much of the handshake as it can, and says when it's done.
    fn handshake(&mut self, socket: &mut dyn Transport) -> std::io::Result<bool>;
    /// Up to `count` bytes of what the other side sent, onto `into`: 0 at the end.
    fn read_onto(&mut self, socket: &mut dyn Transport, into: &mut Vec<u8>, count: usize) -> std::io::Result<usize>;
    /// Takes as much of `data` as it can, saying how much, and sends what it can of what it holds.
    fn write_some(&mut self, socket: &mut dyn Transport, data: &[u8]) -> std::io::Result<usize>;
    /// Whether it has something to send before anything else can happen.
    fn wants_write(&self) -> bool;
}

/// A server's TLS, what `net.listen(port, tls=...)` proves itself with.
pub(crate) trait TlsServer: Send + Sync {
    /// A session for a connection a task serves.
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
            ("connect", [Value::Str(host), Value::Int(port)]) => self.connect(host, port_of(*port, span)?),
            ("listen", [Value::Int(port)]) => self.listen(port_of(*port, span)?),
            ("udp", []) => self.udp(0),
            ("udp", [Value::Int(port)]) => self.udp(port_of(*port, span)?),
            ("connect" | "listen" | "udp" | "script" | "requests", _) => return Err(wrong(method, span)),
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
        let (method, args, _) = timed(method, args, span)?;
        let done = match (kind, method, args) {
            ("Connection", "read", [Value::Int(count)]) => {
                let count = bytes_asked(*count, span)?;
                self.read(handle, Some(count), span)?.map(|data| Value::Bytes(Rc::from(data.unwrap_or_default())))
            }
            ("Connection", "read_line", []) => self.read(handle, None, span)?.map(|line| line_value(line.as_deref())),
            ("Connection", "write", [data @ (Value::Str(_) | Value::Bytes(_))]) => {
                self.write(handle, data_of(data)).map(|()| Value::Unit)
            }
            ("UdpSocket", "receive", []) => self.receive(handle, span)?.map(packet_value),
            ("UdpSocket", "send_to", [data @ (Value::Str(_) | Value::Bytes(_)), Value::Str(address)]) => {
                self.send_to(handle, data_of(data), address).map(|()| Value::Unit)
            }
            ("Server", "accept", []) => return Ok(made_or_failed(self.accept(handle).map(connection_made))),
            ("Connection" | "UdpSocket" | "Server", "close", []) => {
                self.close(handle);
                return Ok(Value::Unit);
            }
            ("Connection" | "UdpSocket" | "Server", _, _) => return Err(wrong(method, span)),
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

    pub(crate) fn listen(&mut self, port: u16) -> Result<Made, Failure> {
        let fake = self;
        let port = i64::from(port);
        if fake.servers.values().any(|(taken, _)| *taken == port) {
            return Err(Failure::Other(format!("port {port} is already in use")));
        }
        // Port 0 asks for any free port, which the fake picks in order.
        let port = if port == 0 { 49152 + i64::try_from(fake.servers.len()).unwrap_or(0) } else { port };
        let handle = fake.handle();
        fake.servers.insert(handle, (port, VecDeque::new()));
        Ok(("Server", vec![("_handle", Value::Int(handle)), ("port", Value::Int(port))]))
    }

    pub(crate) fn connect(&mut self, host: &str, port: u16) -> Result<Made, Failure> {
        let fake = self;
        let port = i64::from(port);
        if !FAKE_HOSTS.contains(&host) {
            return Err(Failure::Other(format!(
                "the test's fake `Net` reaches only the test's own servers, at `localhost`, not `{host}`"
            )));
        }
        let Some(server) = fake.servers.iter().find(|(_, (taken, _))| *taken == port).map(|(server, _)| *server) else {
            return Err(Failure::Refused(host.to_owned(), port));
        };
        let (client, other_end) = (fake.handle(), fake.handle());
        let stream = |peer| FakeStream { inbox: VecDeque::new(), peer, peer_closed: false };
        fake.streams.insert(client, stream(other_end));
        fake.streams.insert(other_end, stream(client));
        let client_peer = format!("localhost:{}", 50000 + client);
        if let Some((_, waiting)) = fake.servers.get_mut(&server) {
            waiting.push_back((other_end, client_peer));
        }
        Ok(("Connection", vec![("_handle", Value::Int(client)), ("peer", Value::str(&format!("localhost:{port}")))]))
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
    pub(crate) fn receive(&mut self, handle: i64, span: Span) -> Result<Result<(Vec<u8>, String), Failure>, Diagnostic> {
        let fake = self;
        let Some((port, waiting)) = fake.udp.get_mut(&handle) else { return Ok(Err(Failure::Closed)) };
        match waiting.pop_front() {
            Some(packet) => Ok(Ok(packet)),
            None => Err(waits_forever(
                &format!("this socket on port {port} in the test's fake network would wait forever for a packet"),
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

    /// The next `count` bytes, or with no count the next line, of a connection in a test.
    pub(crate) fn read(&mut self, handle: i64, count: Option<usize>, span: Span) -> Result<Result<Option<Vec<u8>>, Failure>, Diagnostic> {
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
            &format!("this connection in the test's fake network would wait forever for {wanted}: write to its other end first"),
            span,
        ))
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
            ErrorKind::BrokenPipe | ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted => Failure::Closed,
            _ => Failure::Other(error.to_string()),
        }
    }
}

/// A random handle for a real socket, which nothing could guess. `_handle` is private, so code
/// outside `std.net` can't make a `Connection` or `Server` at all
/// (`examples/errors/net_forged_handle`). A handle is also looked up in the program's own
/// table, so one this program wasn't given reaches nothing, and the randomness is a third wall.
#[cfg(not(target_arch = "wasm32"))]
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

/// The playground has no sockets, so its handles never reach one.
#[cfg(target_arch = "wasm32")]
fn fresh_handle(taken: &HashMap<i64, Socket>) -> i64 {
    i64::try_from(taken.len()).unwrap_or(i64::MAX) + 1
}

fn connection_method(
    interp: &mut Interp<'_>,
    connection: &Instance,
    method: &str,
    args: Vec<CallArg>,
    span: Span,
) -> Result<Value, Diagnostic> {
        crate::replay::value(format_args!("Connection.{method}"), span, || interp.connection_method(connection, method, args, span))
}

fn server_method(
    interp: &mut Interp<'_>,
    server: &Instance,
    method: &str,
    args: Vec<CallArg>,
    span: Span,
) -> Result<Value, Diagnostic> {
        crate::replay::value(format_args!("Server.{method}"), span, || interp.server_method(server, method, args, span))
}

#[expect(clippy::needless_pass_by_value, reason = "every method in `MODULE` takes these")]
fn response_method(
    _interp: &mut Interp<'_>,
    response: &Instance,
    method: &str,
    args: Vec<CallArg>,
    span: Span,
) -> Result<Value, Diagnostic> {
    match (method, args.as_slice(), response.field("body")) {
        ("text", [], Some(Value::Bytes(body))) => Ok(Value::str(&String::from_utf8_lossy(body))),
        ("text", _, _) => Err(wrong(method, span)),
        _ => Err(no_method(&Value::Instance(Rc::new(response.clone())), method, span)),
    }
}

fn udp_method(
    interp: &mut Interp<'_>,
    socket: &Instance,
    method: &str,
    args: Vec<CallArg>,
    span: Span,
) -> Result<Value, Diagnostic> {
        crate::replay::value(format_args!("UdpSocket.{method}"), span, || interp.udp_method(socket, method, args, span))
}

/// The arguments, and the `Duration` given as `timeout=`, if it was. `net.udp(port=…)` is the port
/// as the last argument.
fn with_timeout(args: Vec<CallArg>, method: &str, span: Span) -> Result<(Vec<Value>, Option<Duration>), Diagnostic> {
    let mut plain = Vec::new();
    let mut timeout = None;
    for arg in args {
        match arg.name.as_deref() {
            None => plain.push(arg.value),
            Some("port") if method == "udp" => plain.push(arg.value),
            Some("timeout") => timeout = Some(duration_of(&arg.value, span)?),
            Some(other) => {
                return Err(Diagnostic::new(format!("`{method}` has no parameter called `{other}`"), arg.span));
            }
        }
    }
    Ok((plain, timeout))
}

fn duration_of(value: &Value, span: Span) -> Result<Duration, Diagnostic> {
    if let Value::Instance(duration) = value
        && let Some(Value::Int(nanos)) = duration.field("nanos")
    {
        return Ok(Duration::from_nanos(u64::try_from(*nanos).unwrap_or(0)));
    }
    Err(Diagnostic::new("`timeout` should be a `Duration`", span))
}

fn handle_of(instance: &Instance) -> i64 {
    match instance.field("_handle") {
        Some(Value::Int(handle)) => *handle,
        _ => -1,
    }
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

impl Interp<'_> {
    /// Whether this is a test's world, whose network is a fake in memory.
    pub(crate) fn faked_net(&self) -> bool {
        self.world.as_ref().is_some_and(|world| !world.real())
    }

    fn fake(&mut self) -> Option<&mut Fake> {
        self.world.as_mut().map(|world| &mut world.net)
    }

    /// What `call` does on the test's network, or that it's closed where there's none.
    fn on_fake<T>(&mut self, call: impl FnOnce(&mut Fake) -> Result<T, Failure>) -> Result<T, Failure> {
        self.fake().map_or(Err(Failure::Closed), call)
    }

    /// The same, for a call that can also stop the test.
    fn fake_or_closed<T>(
        &mut self,
        call: impl FnOnce(&mut Fake) -> Result<Result<T, Failure>, Diagnostic>,
    ) -> Result<Result<T, Failure>, Diagnostic> {
        self.fake().map_or(Ok(Err(Failure::Closed)), call)
    }

    /// The types what's real gives back: a program using `Net` needn't have named them.
    fn load_net_types(&mut self) -> Result<(), Diagnostic> {
        for name in ["Connection", "UdpSocket", "Packet", "Response", "NetError", "Duration"] {
            self.load_std_providing(name)?;
        }
        Ok(())
    }

    /// `net.connect(host, port)`, `net.listen(port)`, `net.udp()` and requests.
    pub(crate) fn net_method(&mut self, method: &str, args: Vec<CallArg>, span: Span) -> Result<Value, Diagnostic> {
        self.load_net_types()?;
        match method {
            "get" | "post" | "request" => return self.http_method(method, &args, span),
            "script" | "requests" => return self.fake_answers(method, &args, span),
            _ => {}
        }
        let (args, timeout) = with_timeout(args, method, span)?;
        if !self.faked_net() {
            return real_net(method, &args, timeout, span);
        }
        let found = match (method, args.as_slice()) {
            ("connect", [Value::Str(host), Value::Int(port)]) => {
                let port = port_of(*port, span)?;
                self.on_fake(|fake| fake.connect(host, port))
            }
            ("listen", [Value::Int(port)]) => {
                let port = port_of(*port, span)?;
                self.on_fake(|fake| fake.listen(port))
            }
            ("udp", []) => self.on_fake(|fake| fake.udp(0)),
            ("udp", [Value::Int(port)]) => {
                let port = port_of(*port, span)?;
                self.on_fake(|fake| fake.udp(port))
            }
            ("connect" | "listen" | "udp", _) => return Err(wrong(method, span)),
            _ => return Err(no_method(&Value::Capability(Capability::Net), method, span)),
        };
        Ok(made_or_failed(found))
    }

    /// `net.get(url)`, `net.post(url)` and `net.request(method, url)`.
    fn http_method(&mut self, method: &str, args: &[CallArg], span: Span) -> Result<Value, Diagnostic> {
        let plain: Vec<Value> = args.iter().filter(|arg| arg.name.is_none()).map(|arg| arg.value.clone()).collect();
        // `json=` is sent as its JSON text, which native code writes before the call.
        let mut sent = None;
        if let Some(arg) = args.iter().find(|arg| arg.name.as_deref() == Some("json")) {
            let value = CallArg::plain(arg.value.clone(), arg.span);
            sent = Some(self.format_builtin("json.dumps", vec![value], arg.span)?);
        }
        let named: Vec<(&str, &Value)> = args
            .iter()
            .filter_map(|arg| arg.name.as_deref().map(|name| (name, &arg.value)))
            .map(|(name, value)| match &sent {
                Some(text) if name == "json" => (name, text),
                _ => (name, value),
            })
            .collect();
        let request = request_of(method, &plain, &named, span)?;
        if self.faked_net() {
            return Ok(self.fake_request(&request));
        }
        Ok(done_or_failed(http::send(&request).map(answer_value)))
    }

    /// `net.script(request, response)` and `net.requests()`, for a test.
    fn fake_answers(&mut self, method: &str, args: &[CallArg], span: Span) -> Result<Value, Diagnostic> {
        if !self.faked_net() {
            return Err(no_method(&Value::Capability(Capability::Net), method, span));
        }
        let given: Vec<Value> = args.iter().map(|arg| arg.value.clone()).collect();
        match self.fake().filter(|_| matches!(method, "script" | "requests")) {
            Some(fake) => fake.net_method(method, &given, span),
            None => Err(no_method(&Value::Capability(Capability::Net), method, span)),
        }
    }

    /// What a test scripted for a request.
    fn fake_request(&mut self, request: &http::Request) -> Value {
        self.fake().map_or_else(|| failed(&Failure::Closed), |fake| fake.request(request))
    }

    fn connection_method(
        &mut self,
        connection: &Instance,
        method: &str,
        args: Vec<CallArg>,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        if !matches!(method, "read" | "read_line" | "write" | "close") {
            return Err(no_method(&Value::Instance(Rc::new(connection.clone())), method, span));
        }
        let handle = handle_of(connection);
        let (args, timeout) = with_timeout(args, method, span)?;
        self.load_net_types()?;
        if !self.faked_net() {
            return real_connection(handle, method, &args, timeout, span);
        }
        let done = match (method, args.as_slice()) {
            ("read", [Value::Int(count)]) => {
                let count = bytes_asked(*count, span)?;
                self.fake_or_closed(|fake| fake.read(handle, Some(count), span))?.map(|data| Value::Bytes(Rc::from(data.unwrap_or_default())))
            }
            ("read_line", []) => self.fake_or_closed(|fake| fake.read(handle, None, span))?.map(|line| line_value(line.as_deref())),
            ("write", [data @ (Value::Str(_) | Value::Bytes(_))]) => {
                self.on_fake(|fake| fake.write(handle, data_of(data))).map(|()| Value::Unit)
            }
            ("close", []) => {
                if let Some(fake) = self.fake() {
                    fake.close(handle);
                }
                return Ok(Value::Unit);
            }
            _ => return Err(wrong(method, span)),
        };
        Ok(done_or_failed(done))
    }

    fn udp_method(&mut self, socket: &Instance, method: &str, args: Vec<CallArg>, span: Span) -> Result<Value, Diagnostic> {
        if !matches!(method, "receive" | "send_to" | "close") {
            return Err(no_method(&Value::Instance(Rc::new(socket.clone())), method, span));
        }
        let handle = handle_of(socket);
        let (args, timeout) = with_timeout(args, method, span)?;
        self.load_net_types()?;
        if !self.faked_net() {
            return real_udp(handle, method, &args, timeout, span);
        }
        let done = match (method, args.as_slice()) {
            ("receive", []) => self.fake_or_closed(|fake| fake.receive(handle, span))?.map(packet_value),
            ("send_to", [data @ (Value::Str(_) | Value::Bytes(_)), Value::Str(address)]) => {
                self.on_fake(|fake| fake.send_to(handle, data_of(data), address)).map(|()| Value::Unit)
            }
            ("close", []) => {
                if let Some(fake) = self.fake() {
                    fake.close(handle);
                }
                return Ok(Value::Unit);
            }
            _ => return Err(wrong(method, span)),
        };
        Ok(done_or_failed(done))
    }

    fn server_method(&mut self, server: &Instance, method: &str, args: Vec<CallArg>, span: Span) -> Result<Value, Diagnostic> {
        if !matches!(method, "accept" | "close") {
            return Err(no_method(&Value::Instance(Rc::new(server.clone())), method, span));
        }
        let handle = handle_of(server);
        let (args, timeout) = with_timeout(args, method, span)?;
        self.load_net_types()?;
        if !self.faked_net() {
            return real_server(handle, method, &args, timeout, span);
        }
        match (method, args.as_slice()) {
            ("accept", []) => Ok(made_or_failed(self.on_fake(|fake| fake.accept(handle)).map(connection_made))),
            ("close", []) => {
                if let Some(fake) = self.fake() {
                    fake.close(handle);
                }
                Ok(Value::Unit)
            }
            _ => Err(wrong(method, span)),
        }
    }

}

/// `net.connect(host, port)`, `net.listen(port)` and `net.udp()` on the real network: needing nothing of the
/// interpreter, for native code too.
pub(crate) fn real_net(method: &str, args: &[Value], timeout: Option<Duration>, span: Span) -> Result<Value, Diagnostic> {
    let found = match (method, args) {
        ("connect", [Value::Str(host), Value::Int(port)]) => {
            connect(host, port_of(*port, span)?, timeout.unwrap_or(CONNECT_TIMEOUT), false)
        }
        // `tls=True`, which native code keeps on the method's name.
        ("connect.tls" | "connect", [Value::Str(host), Value::Int(port), Value::Bool(tls)]) => {
            connect(host, port_of(*port, span)?, timeout.unwrap_or(CONNECT_TIMEOUT), *tls)
        }
        ("listen", [Value::Int(port)]) => listen(port_of(*port, span)?, None),
        // `tls=Certificate(...)`, or `None`, which native code keeps on the method's name.
        ("listen.tls" | "listen", [Value::Int(port), certificate]) => {
            match tls_server(certificate, span)? {
                Ok(tls) => listen(port_of(*port, span)?, tls),
                Err(failure) => Err(failure),
            }
        }
        ("udp", []) => udp(0),
        // Native code keeps a named argument on the method's name.
        ("udp" | "udp.port", [Value::Int(port)]) => udp(port_of(*port, span)?),
        ("connect" | "connect.tls" | "listen" | "listen.tls" | "udp" | "udp.port", _) => {
            return Err(wrong(method, span));
        }
        _ => return Err(no_method(&Value::Capability(Capability::Net), method, span)),
    };
    Ok(made_or_failed(found))
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
    let chain = match instance.field("chain") {
        Some(Value::Str(chain)) => chain,
        _ => return Err(Diagnostic::new("this `Certificate` has no `chain`", span)),
    };
    let key = match instance.field("key") {
        Some(Value::Instance(secret)) => match secret.field("_value") {
            Some(Value::Str(key)) => key,
            _ => return Err(Diagnostic::new("this `Certificate` has no `key`", span)),
        },
        _ => return Err(Diagnostic::new("this `Certificate` has no `key`", span)),
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
        ("write", [data @ (Value::Str(_) | Value::Bytes(_))]) => {
            write(handle, data_of(data)).map(|()| Value::Unit)
        }
        ("close", []) => {
            close(handle);
            return Ok(Value::Unit);
        }
        // What a task serving a connection makes its `Connection` from.
        ("peer", []) => return Ok(stream(handle).map_or_else(|_| Value::str(""), |it| Value::str(&it.peer))),
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
        ("accept", []) => Ok(made_or_failed(accept(handle, timeout).map(connection_made))),
        ("close", []) => {
            close(handle);
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
fn request_of(method: &str, plain: &[Value], named: &[(&str, &Value)], span: Span) -> Result<http::Request, Diagnostic> {
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
    let headers: Vec<(Value, Value)> =
        answer.headers.iter().map(|(name, value)| (Value::str(name), Value::str(value))).collect();
    Value::made(
        "Response",
        vec![
            ("status", Value::Int(answer.status)),
            ("headers", Value::Dict(Rc::new(headers.into()))),
            ("body", Value::Bytes(Rc::from(answer.body))),
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
    real_net(method, args, timeout, span)
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
        "connect" => &["host", "port", "tls"],
        "listen" | "udp" => &["port"],
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
/// `read_line` with `timeout=` given last.
pub(crate) fn timed<'a>(
    method: &'a str,
    args: &'a [Value],
    span: Span,
) -> Result<(&'a str, &'a [Value], Option<Duration>), Diagnostic> {
    let (Some(method), Some((timeout, args))) = (method.strip_suffix(".timeout"), args.split_last()) else {
        return Ok((method, args, None));
    };
    Ok((method, args, Some(duration_of(timeout, span)?)))
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

pub(crate) fn line_value(line: Option<&[u8]>) -> Value {
    line.map_or(Value::None, |line| Value::str(String::from_utf8_lossy(trim_line(line)).as_ref()))
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
#[cfg(not(target_arch = "wasm32"))]
fn secured(stream: &TcpStream, host: &str, timeout: Duration) -> Result<Box<dyn SecureStream>, Failure> {
    let tcp = stream.try_clone().map_err(|error| Failure::Other(error.to_string()))?;
    http::raw_secured(tcp, host, timeout)
}

#[cfg(target_arch = "wasm32")]
fn secured(_stream: &TcpStream, _host: &str, _timeout: Duration) -> Result<std::convert::Infallible, Failure> {
    Err(Failure::Tls("the playground has no network".to_owned()))
}

/// A connection to `host` on `port`, looked up and connected, blocking its thread.
fn connected(host: &str, port: u16, timeout: Duration) -> Result<(TcpStream, String), Failure> {
    let addresses: Vec<_> = (host, port).to_socket_addrs().map_err(|_| Failure::NotFound(host.to_owned()))?.collect();
    if addresses.is_empty() {
        return Err(Failure::NotFound(host.to_owned()));
    }
    let mut failure = Failure::NotFound(host.to_owned());
    for address in addresses {
        match TcpStream::connect_timeout(&address, timeout) {
            Ok(stream) => {
                let peer = stream.peer_addr().map_or_else(|_| format!("{host}:{port}"), |peer| peer.to_string());
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

fn keep_stream(socket: TcpStream, peer: String, secured: Option<Secured>) -> Result<i64, Failure> {
    let reading = match secured {
        #[cfg(not(target_arch = "wasm32"))]
        Some(secured) => Reading::Tls(secured),
        #[cfg(target_arch = "wasm32")]
        Some(never) => match never {},
        None => Reading::Plain(BufReader::new(socket.try_clone().map_err(|error| Failure::Other(error.to_string()))?)),
    };
    let stream = Stream { peer, socket, reader: Mutex::new(reading), writing: Mutex::new(()) };
    Ok(open(|open| {
        let handle = fresh_handle(open);
        open.insert(handle, Socket::Stream(Arc::new(stream)));
        handle
    }))
}

/// TLS over a connection, as `secured` makes it.
#[cfg(not(target_arch = "wasm32"))]
type Secured = Box<dyn SecureStream>;
#[cfg(target_arch = "wasm32")]
type Secured = std::convert::Infallible;

fn listen(port: u16, tls: Option<Arc<dyn TlsServer>>) -> Result<Made, Failure> {
    let listener = TcpListener::bind(("0.0.0.0", port)).map_err(|error| match error.kind() {
        ErrorKind::AddrInUse => Failure::Other(format!("port {port} is already in use")),
        _ => Failure::Other(error.to_string()),
    })?;
    let port = listener.local_addr().map_or(port, |address| address.port());
    let handle = open(|open| {
        let handle = fresh_handle(open);
        open.insert(handle, Socket::Listener(Arc::new(Listening { listener, tls })));
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
    let mut reader = stream.reader.lock().unwrap_or_else(PoisonError::into_inner);
    let mut data = vec![0; count];
    let read = reading(&stream.socket, &mut reader, timeout, |reader| reader.read(&mut data))?;
    data.truncate(read);
    Ok(data)
}

fn read_line(handle: i64, timeout: Option<Duration>) -> Result<Option<Vec<u8>>, Failure> {
    let stream = stream(handle)?;
    let mut reader = stream.reader.lock().unwrap_or_else(PoisonError::into_inner);
    let mut line = Vec::new();
    // What a read cut short by having to wait got is kept in `line`, and the next adds to it.
    reading(&stream.socket, &mut reader, timeout, |reader| reader.read_until(b'\n', &mut line))?;
    Ok((!line.is_empty()).then_some(line))
}

fn write(handle: i64, mut data: &[u8]) -> Result<(), Failure> {
    let stream = stream(handle)?;
    let _writing = stream.writing.lock().unwrap_or_else(PoisonError::into_inner);
    #[cfg(not(target_arch = "wasm32"))]
    if let Reading::Tls(secured) = &mut *stream.reader.lock().unwrap_or_else(PoisonError::into_inner) {
        return secured.write_all(data).and_then(|()| secured.flush()).map_err(|error| Failure::of(&error, None));
    }
    let mut socket = &stream.socket;
    // A read parked on this connection's other end can have made the socket not block, so a full
    // buffer says so here, and this waits for room.
    while !data.is_empty() {
        match socket.write(data) {
            Ok(0) => return Err(Failure::Closed),
            Ok(written) => data = data.get(written..).unwrap_or_default(),
            Err(error) if error.kind() == ErrorKind::WouldBlock => wait_to_write(socket),
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => return Err(Failure::of(&error, None)),
        }
    }
    socket.flush().map_err(|error| Failure::of(&error, None))
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
/// otherwise waiting at most `timeout`.
fn reading<T>(
    socket: &TcpStream,
    reader: &mut Reading,
    timeout: Option<Duration>,
    mut read: impl FnMut(&mut dyn BufRead) -> std::io::Result<T>,
) -> Result<T, Failure> {
    match reader {
        Reading::Plain(plain) => read_on(socket, plain, timeout, |reader| read(reader)),
        #[cfg(not(target_arch = "wasm32"))]
        Reading::Tls(secured) => read_on(socket, &mut **secured, timeout, |reader| read(reader)),
    }
}

/// `reading`, on one kind of reader.
fn read_on<R: BufRead + ?Sized, T>(
    socket: &TcpStream,
    reader: &mut R,
    timeout: Option<Duration>,
    mut read: impl FnMut(&mut R) -> std::io::Result<T>,
) -> Result<T, Failure> {
    if socket_parks() {
        return parked(socket, reader, timeout, read);
    }
    wait_at_most(socket, timeout)?;
    read(reader).map_err(|error| Failure::of(&error, timeout))
}

/// Runs a read that may have to wait, in a task that parks rather than block: with the socket not
/// blocking, `read` is tried, and each time it would block, the task parks until the socket's
/// readable or `timeout` passes. The socket blocks again after.
fn parked<R: ?Sized, T>(
    socket: &TcpStream,
    reader: &mut R,
    timeout: Option<Duration>,
    mut read: impl FnMut(&mut R) -> std::io::Result<T>,
) -> Result<T, Failure> {
    socket.set_nonblocking(true).map_err(|error| Failure::Other(error.to_string()))?;
    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    let done = loop {
        match read(reader) {
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
fn wait_to_write(socket: &TcpStream) {
    #[cfg(all(unix, any(test, feature = "threads")))]
    if socket_parks() {
        let fd = std::os::fd::AsRawFd::as_raw_fd(socket);
        let _ = crate::embed::serve::park_on_fd(fd, mio::Interest::WRITABLE, None);
        return;
    }
    let _ = socket;
    std::thread::sleep(Duration::from_millis(1));
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
                    Err(error) if error.kind() == ErrorKind::WouldBlock => break Err(Failure::TimedOut(limit)),
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
    Ok((keep_stream(stream, peer.clone(), secured)?, peer))
}

/// How long a connection a server took has to finish its handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// The server's half of a handshake over a connection just taken, on a thread that may block when
/// this is a task.
#[cfg(not(target_arch = "wasm32"))]
fn accepted_securely(tls: &Arc<dyn TlsServer>, stream: &TcpStream) -> Result<Secured, Failure> {
    let tcp = stream.try_clone().map_err(|error| Failure::Other(error.to_string()))?;
    if task_parks() {
        let tls = Arc::clone(tls);
        return off_thread(move || tls.accepted(tcp, HANDSHAKE_TIMEOUT));
    }
    tls.accepted(tcp, HANDSHAKE_TIMEOUT)
}

#[cfg(target_arch = "wasm32")]
fn accepted_securely(_tls: &Arc<dyn TlsServer>, _stream: &TcpStream) -> Result<Secured, Failure> {
    Err(Failure::Tls("the playground has no network".to_owned()))
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
        udp.socket.set_read_timeout(Some(left.min(UDP_LOOKS_EVERY))).map_err(|error| Failure::of(&error, timeout))?;
        match udp.socket.recv_from(&mut data) {
            Ok((size, sender)) => {
                data.truncate(size);
                return Ok((data, sender.to_string()));
            }
            // Windows says this of a packet this socket sent earlier that nothing took. Nothing's
            // wrong with the socket, and UDP doesn't say what's lost.
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::ConnectionReset) => {}
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
        // It may be closed already from the other side, which is what closing wanted.
        let _ = stream.socket.shutdown(Shutdown::Both);
    }
}

/// Takes a real server's listener out of the open sockets, for `parallel(server)` to serve from.
/// One already closed has none.
#[cfg(any(test, feature = "threads"))]
pub(crate) fn take_listener(handle: i64) -> Option<(TcpListener, Option<Arc<dyn TlsServer>>)> {
    open(|open| match open.remove(&handle) {
        Some(Socket::Listener(listening)) => {
            let tls = listening.tls.clone();
            Some((listening.listener.try_clone().ok()?, tls))
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

#[cfg(test)]
pub(crate) mod tests {
    use super::{Failure, Made, close, receive, udp};
    use crate::value::Value;

    fn handle_of_made((_, fields): &Made) -> i64 {
        match fields.first() {
            Some((_, Value::Int(handle))) => *handle,
            _ => -1,
        }
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
        use std::sync::Arc;
        use std::time::Duration;

        use rustls::pki_types::pem::PemObject as _;
        use rustls::pki_types::{CertificateDer, PrivateKeyDer};
        use rustls::{ClientConfig, RootCertStore, ServerConfig, ServerConnection, StreamOwned};

        use super::super::http::tls::handshake;
        use super::super::{Failure, close, keep_stream, read_line, write};

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
                while !ready.load(std::sync::atomic::Ordering::SeqCst) && started.elapsed() < Duration::from_secs(5) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                let _ = secured.write_all(b"late\n");
                let _ = secured.flush();
                std::thread::sleep(Duration::from_millis(200));
            });
            let tcp = connected_to(port);
            let Ok(secured) = handshake(tcp.try_clone().unwrap(), "localhost", Duration::from_secs(10), client_config()) else {
                panic!("the handshake failed");
            };
            let handle = keep_stream(tcp, "server".to_owned(), Some(secured)).ok().unwrap();
            assert!(write(handle, b"x").is_ok());
            handle
        }

        /// A connection with TLS over it reads and writes as a plain one does.
        #[test]
        fn a_tls_connection_reads_and_writes_lines() {
            let (port, server) = serving();
            let tcp = connected_to(port);
            let secured = handshake(tcp.try_clone().unwrap(), "localhost", Duration::from_secs(10), client_config());
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
            let got = handshake(connected_to(port), "fennec.example", Duration::from_secs(10), client_config());
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
