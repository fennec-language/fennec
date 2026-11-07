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
    reader: Mutex<BufReader<TcpStream>>,
    writing: Mutex<()>,
}

/// A real UDP socket. Only one thread reads at a time, as how long a read waits is the socket's.
struct Udp {
    socket: UdpSocket,
    receiving: Mutex<()>,
}

#[derive(Clone)]
enum Socket {
    Stream(Arc<Stream>),
    Listener(Arc<TcpListener>),
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
    interp.connection_method(connection, method, args, span)
}

fn server_method(
    interp: &mut Interp<'_>,
    server: &Instance,
    method: &str,
    args: Vec<CallArg>,
    span: Span,
) -> Result<Value, Diagnostic> {
    interp.server_method(server, method, args, span)
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
    interp.udp_method(socket, method, args, span)
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
    fn faked_net(&self) -> bool {
        self.world.as_ref().is_some_and(|world| !world.real())
    }

    fn fake(&mut self) -> Option<&mut Fake> {
        self.world.as_mut().map(|world| &mut world.net)
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
                self.fake_connect(host, port)
            }
            ("listen", [Value::Int(port)]) => {
                let port = port_of(*port, span)?;
                self.fake_listen(port)
            }
            ("udp", []) => self.fake_udp(0),
            ("udp", [Value::Int(port)]) => {
                let port = port_of(*port, span)?;
                self.fake_udp(port)
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
        let Some(fake) = self.fake() else {
            return Err(no_method(&Value::Capability(Capability::Net), method, span));
        };
        match (method, args) {
            ("script", [request, response]) => {
                let Value::Str(request) = &request.value else { return Err(wrong(method, span)) };
                fake.answers.insert(request.to_string(), response.value.clone());
                Ok(Value::Unit)
            }
            ("requests", []) => Ok(Value::list(fake.requests.iter().map(|sent| Value::str(sent)).collect())),
            _ => Err(wrong(method, span)),
        }
    }

    /// What a test scripted for a request. A request it didn't script fails: nothing else could
    /// answer it.
    fn fake_request(&mut self, request: &http::Request) -> Value {
        let Some(fake) = self.fake() else { return failed(&Failure::Closed) };
        let sent = format!("{} {}", request.method, http::written(request));
        fake.requests.push(sent.clone());
        match fake.answers.get(&sent) {
            Some(answer) => Value::Ok(Rc::new(answer.clone())),
            None => failed(&Failure::Other(format!(
                "the test's fake `Net` has no answer for `{sent}`: give it one with `net.script`"
            ))),
        }
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
                self.fake_read(handle, Some(count), span)?.map(|data| Value::Bytes(Rc::from(data.unwrap_or_default())))
            }
            ("read_line", []) => self.fake_read(handle, None, span)?.map(|line| line_value(line.as_deref())),
            ("write", [data @ (Value::Str(_) | Value::Bytes(_))]) => {
                self.fake_write(handle, data_of(data)).map(|()| Value::Unit)
            }
            ("close", []) => {
                self.fake_close(handle);
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
            ("receive", []) => self.fake_receive(handle, span)?.map(packet_value),
            ("send_to", [data @ (Value::Str(_) | Value::Bytes(_)), Value::Str(address)]) => {
                self.fake_send_to(handle, data_of(data), address).map(|()| Value::Unit)
            }
            ("close", []) => {
                self.fake_close(handle);
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
            ("accept", []) => Ok(made_or_failed(self.fake_accept(handle).map(connection_made))),
            ("close", []) => {
                self.fake_close(handle);
                Ok(Value::Unit)
            }
            _ => Err(wrong(method, span)),
        }
    }

    fn fake_listen(&mut self, port: u16) -> Result<Made, Failure> {
        let Some(fake) = self.fake() else { return Err(Failure::Closed) };
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

    fn fake_connect(&mut self, host: &str, port: u16) -> Result<Made, Failure> {
        let Some(fake) = self.fake() else { return Err(Failure::Closed) };
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

    fn fake_udp(&mut self, port: u16) -> Result<Made, Failure> {
        let Some(fake) = self.fake() else { return Err(Failure::Closed) };
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
    fn fake_receive(&mut self, handle: i64, span: Span) -> Result<Result<(Vec<u8>, String), Failure>, Diagnostic> {
        let Some(fake) = self.fake() else { return Ok(Err(Failure::Closed)) };
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
    fn fake_send_to(&mut self, handle: i64, data: &[u8], address: &str) -> Result<(), Failure> {
        let Some(fake) = self.fake() else { return Err(Failure::Closed) };
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
    fn fake_accept(&mut self, handle: i64) -> Result<(i64, String), Failure> {
        let Some(fake) = self.fake() else { return Err(Failure::Closed) };
        let Some((port, waiting)) = fake.servers.get_mut(&handle) else { return Err(Failure::Closed) };
        waiting.pop_front().ok_or_else(|| {
            Failure::Other(format!(
                "no connection is waiting for the test's fake server on port {port}, and none can come while a test waits"
            ))
        })
    }

    /// The next `count` bytes, or with no count the next line, of a connection in a test.
    fn fake_read(&mut self, handle: i64, count: Option<usize>, span: Span) -> Result<Result<Option<Vec<u8>>, Failure>, Diagnostic> {
        let Some(fake) = self.fake() else { return Ok(Err(Failure::Closed)) };
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

    fn fake_write(&mut self, handle: i64, data: &[u8]) -> Result<(), Failure> {
        let Some(fake) = self.fake() else { return Err(Failure::Closed) };
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
    fn fake_close(&mut self, handle: i64) {
        let Some(fake) = self.fake() else { return };
        fake.udp.remove(&handle);
        if let Some((_, waiting)) = fake.servers.remove(&handle) {
            for (served, _) in waiting {
                fake.close_stream(served);
            }
        }
        fake.close_stream(handle);
    }
}

/// `net.connect(host, port)`, `net.listen(port)` and `net.udp()` on the real network: needing nothing of the
/// interpreter, for native code too.
pub(crate) fn real_net(method: &str, args: &[Value], timeout: Option<Duration>, span: Span) -> Result<Value, Diagnostic> {
    let found = match (method, args) {
        ("connect", [Value::Str(host), Value::Int(port)]) => {
            connect(host, port_of(*port, span)?, timeout.unwrap_or(CONNECT_TIMEOUT))
        }
        ("listen", [Value::Int(port)]) => listen(port_of(*port, span)?),
        ("udp", []) => udp(0),
        // Native code keeps a named argument on the method's name.
        ("udp" | "udp.port", [Value::Int(port)]) => udp(port_of(*port, span)?),
        ("connect" | "listen" | "udp" | "udp.port", _) => return Err(wrong(method, span)),
        _ => return Err(no_method(&Value::Capability(Capability::Net), method, span)),
    };
    Ok(made_or_failed(found))
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

fn connect(host: &str, port: u16, timeout: Duration) -> Result<Made, Failure> {
    // A task parks while its lookup and connect wait, on a thread that may block, rather than
    // holding its worker.
    let (stream, peer) = if task_parks() {
        let host = host.to_owned();
        off_thread(move || connected(&host, port, timeout))?
    } else {
        connected(host, port, timeout)?
    };
    let handle = keep_stream(stream, peer.clone())?;
    Ok(connection_made((handle, peer)))
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

fn keep_stream(socket: TcpStream, peer: String) -> Result<i64, Failure> {
    let reader = socket.try_clone().map_err(|error| Failure::Other(error.to_string()))?;
    let stream = Stream { peer, socket, reader: Mutex::new(BufReader::new(reader)), writing: Mutex::new(()) };
    Ok(open(|open| {
        let handle = fresh_handle(open);
        open.insert(handle, Socket::Stream(Arc::new(stream)));
        handle
    }))
}

fn listen(port: u16) -> Result<Made, Failure> {
    let listener = TcpListener::bind(("0.0.0.0", port)).map_err(|error| match error.kind() {
        ErrorKind::AddrInUse => Failure::Other(format!("port {port} is already in use")),
        _ => Failure::Other(error.to_string()),
    })?;
    let port = listener.local_addr().map_or(port, |address| address.port());
    let handle = open(|open| {
        let handle = fresh_handle(open);
        open.insert(handle, Socket::Listener(Arc::new(listener)));
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
    let read = if socket_parks() {
        parked(&mut reader, timeout, |reader| reader.read(&mut data))?
    } else {
        wait_at_most(reader.get_ref(), timeout)?;
        reader.read(&mut data).map_err(|error| Failure::of(&error, timeout))?
    };
    data.truncate(read);
    Ok(data)
}

fn read_line(handle: i64, timeout: Option<Duration>) -> Result<Option<Vec<u8>>, Failure> {
    let stream = stream(handle)?;
    let mut reader = stream.reader.lock().unwrap_or_else(PoisonError::into_inner);
    let mut line = Vec::new();
    // What a read cut short by having to wait got is kept in `line`, and the next adds to it.
    let read = if socket_parks() {
        parked(&mut reader, timeout, |reader| reader.read_until(b'\n', &mut line))?;
        line.len()
    } else {
        wait_at_most(reader.get_ref(), timeout)?;
        reader.read_until(b'\n', &mut line).map_err(|error| Failure::of(&error, timeout))?
    };
    Ok((read > 0).then_some(line))
}

fn write(handle: i64, mut data: &[u8]) -> Result<(), Failure> {
    let stream = stream(handle)?;
    let _writing = stream.writing.lock().unwrap_or_else(PoisonError::into_inner);
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
    let handle = keep_stream(stream, peer)?;
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

/// Runs a read that may have to wait, in a task that parks rather than block: with the socket not
/// blocking, `read` is tried, and each time it would block, the task parks until the socket's
/// readable or `timeout` passes. The socket blocks again after.
fn parked<T>(
    reader: &mut BufReader<TcpStream>,
    timeout: Option<Duration>,
    mut read: impl FnMut(&mut BufReader<TcpStream>) -> std::io::Result<T>,
) -> Result<T, Failure> {
    reader.get_ref().set_nonblocking(true).map_err(|error| Failure::Other(error.to_string()))?;
    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    let done = loop {
        match read(reader) {
            Ok(done) => break Ok(done),
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                if deadline.is_some_and(|deadline| deadline <= Instant::now()) {
                    break Err(Failure::TimedOut(timeout.unwrap_or_default()));
                }
                if park_until_readable(reader.get_ref(), deadline) {
                    break Err(Failure::TimedOut(timeout.unwrap_or_default()));
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => break Err(Failure::of(&error, timeout)),
        }
    };
    let blocking = reader.get_ref().set_nonblocking(false);
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
    let Some(Socket::Listener(listener)) = open(|open| open.get(&handle).cloned()) else {
        return Err(Failure::Closed);
    };
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
    Ok((keep_stream(stream, peer.clone())?, peer))
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
pub(crate) fn take_listener(handle: i64) -> Option<TcpListener> {
    open(|open| match open.remove(&handle) {
        Some(Socket::Listener(listener)) => {
            Arc::try_unwrap(listener).or_else(|listener| listener.try_clone()).ok()
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
mod tests {
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
}
