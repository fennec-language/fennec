//! `std.net`: TCP through the `Net` capability. A `Connection` or `Server` holds a private
//! handle into the program's table of open sockets, so code that was never given `Net` can't
//! make one up to reach a socket. The real network is free functions over that one table, which
//! the interpreter and compiled code both call, on any thread. In a test, the network is in
//! memory, in `Fake`, which only the interpreter has.
//! Nothing else runs while a test waits, so a read that would wait forever stops the test and
//! says why, and a server with no connection waiting is done.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::rc::Rc;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};
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

enum Socket {
    /// Read and written apart, so a read that's waiting doesn't stop a write.
    Stream { reader: Mutex<BufReader<TcpStream>>, writer: TcpStream },
    Listener(TcpListener),
    Udp(UdpSocket),
}

/// The real sockets a program has open, by handle, for every thread: a connection made on one
/// thread can be used on another.
static SOCKETS: LazyLock<Mutex<HashMap<i64, Arc<Socket>>>> = LazyLock::new(Mutex::default);

fn sockets() -> MutexGuard<'static, HashMap<i64, Arc<Socket>>> {
    SOCKETS.lock().unwrap_or_else(PoisonError::into_inner)
}

fn socket(handle: i64) -> Option<Arc<Socket>> {
    sockets().get(&handle).cloned()
}

/// Keeps a socket, giving back its handle.
fn keep(socket: Socket) -> i64 {
    let mut open = sockets();
    let handle = fresh_handle(&open);
    open.insert(handle, Arc::new(socket));
    handle
}

/// A connection's two ends, kept as one socket.
fn keep_stream(stream: TcpStream) -> Result<i64, Failure> {
    let writer = stream.try_clone().map_err(|error| Failure::Other(error.to_string()))?;
    Ok(keep(Socket::Stream { reader: Mutex::new(BufReader::new(stream)), writer }))
}

/// One end of a connection in a test's network.
struct FakeStream {
    /// What the other end wrote that this end hasn't read.
    inbox: VecDeque<u8>,
    peer: i64,
    peer_closed: bool,
}

/// A test's network, in memory. Handles count up from 1, so a test runs the same every time.
#[derive(Default)]
pub(crate) struct Fake {
    last: i64,
    /// Each server's port, and the connections waiting for it to accept them.
    servers: BTreeMap<i64, (i64, VecDeque<(i64, String)>)>,
    streams: BTreeMap<i64, FakeStream>,
    /// Each UDP socket's port, and what's been sent to it and by whom.
    udp: BTreeMap<i64, (i64, VecDeque<(Vec<u8>, String)>)>,
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

/// What a call made: a `Connection` or `Server`, by its type's name and fields.
type Made = (&'static str, Vec<(&'static str, Value)>);

/// How a network call failed, which `NetError` says.
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
fn fresh_handle(taken: &HashMap<i64, Arc<Socket>>) -> i64 {
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
fn fresh_handle(taken: &HashMap<i64, Arc<Socket>>) -> i64 {
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

fn response_method(
    _interp: &mut Interp<'_>,
    response: &Instance,
    method: &str,
    args: Vec<CallArg>,
    span: Span,
) -> Result<Value, Diagnostic> {
    match (method, args.as_slice()) {
        ("text", []) => Ok(response_text(response)),
        _ => Err(no_method(&Value::Instance(Rc::new(response.clone())), method, span)),
    }
}

fn response_text(response: &Instance) -> Value {
    match response.field("body") {
        Some(Value::Bytes(body)) => Value::str(&String::from_utf8_lossy(body)),
        _ => Value::str(""),
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

fn server_method(
    interp: &mut Interp<'_>,
    server: &Instance,
    method: &str,
    args: Vec<CallArg>,
    span: Span,
) -> Result<Value, Diagnostic> {
    interp.server_method(server, method, args, span)
}

/// The arguments, and the `Duration` given as `timeout=`, if it was.
fn with_timeout(args: Vec<CallArg>, method: &str, span: Span) -> Result<(Vec<Value>, Option<Duration>), Diagnostic> {
    let mut plain = Vec::new();
    let mut timeout = None;
    for arg in args {
        match arg.name.as_deref() {
            None | Some("port") => plain.push(arg.value),
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
    pub(crate) fn faked_net(&self) -> bool {
        self.world.as_ref().is_some_and(|world| !world.real())
    }

    fn fake(&mut self) -> Option<&mut Fake> {
        self.world.as_mut().map(|world| &mut world.net)
    }

    /// `net.connect(host, port)` and `net.listen(port)`.
    pub(crate) fn net_method(&mut self, method: &str, args: Vec<CallArg>, span: Span) -> Result<Value, Diagnostic> {
        match method {
            "get" | "post" | "request" => return self.http_method(method, &args, span),
            "script" | "requests" => return self.fake_answers(method, &args, span),
            _ => {}
        }
        let (args, timeout) = with_timeout(args, method, span)?;
        let found = match (method, args.as_slice()) {
            ("connect", [Value::Str(host), Value::Int(port)]) => {
                let port = port_of(*port, span)?;
                let timeout = timeout.unwrap_or(CONNECT_TIMEOUT);
                if self.faked_net() {
                    self.fake_connect(host, port)
                } else {
                    connect(host, port, timeout).map(|(handle, peer)| connection_made(handle, &peer))
                }
            }
            ("listen", [Value::Int(port)]) => {
                let port = port_of(*port, span)?;
                if self.faked_net() {
                    self.fake_listen(port)
                } else {
                    listen(port).map(|(handle, port)| server_made(handle, port))
                }
            }
            ("udp", [] | [Value::Int(_)]) => {
                let port = if let [Value::Int(port)] = args.as_slice() { port_of(*port, span)? } else { 0 };
                if self.faked_net() {
                    self.fake_udp(port)
                } else {
                    udp(port).map(|(handle, port)| udp_made(handle, port))
                }
            }
            ("connect" | "listen" | "udp", _) => return Err(wrong(method, span)),
            _ => return Err(no_method(&Value::Capability(Capability::Net), method, span)),
        };
        match found {
            Ok((type_name, fields)) => {
                let made = self.make_value(type_name, fields.iter().map(|(name, value)| (*name, value.clone())).collect(), span)?;
                Ok(Value::Ok(Rc::new(made)))
            }
            Err(failure) => self.net_error(&failure, span),
        }
    }

    /// `net.get(url)`, `net.post(url)` and `net.request(method, url)`.
    fn http_method(&mut self, method: &str, args: &[CallArg], span: Span) -> Result<Value, Diagnostic> {
        let plain: Vec<Value> = args.iter().filter(|arg| arg.name.is_none()).map(|arg| arg.value.clone()).collect();
        let named: Vec<(&str, &Value)> =
            args.iter().filter_map(|arg| arg.name.as_deref().map(|name| (name, &arg.value))).collect();
        let request = request_of(method, &plain, &named, span)?;
        if self.faked_net() {
            return self.fake_request(&request, span);
        }
        match http::send(&request) {
            Ok(answer) => Ok(Value::Ok(Rc::new(answer_value(answer)))),
            Err(failure) => self.net_error(&failure, span),
        }
    }

    /// `net.script(request, response)` and `net.requests()`, for a test.
    fn fake_answers(&mut self, method: &str, args: &[CallArg], span: Span) -> Result<Value, Diagnostic> {
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
    fn fake_request(&mut self, request: &http::Request, span: Span) -> Result<Value, Diagnostic> {
        let Some(fake) = self.fake() else {
            return self.net_error(&Failure::Closed, span);
        };
        let sent = format!("{} {}", request.method, http::written(request));
        fake.requests.push(sent.clone());
        let answer = fake.answers.get(&sent).cloned();
        match answer {
            Some(answer) => Ok(Value::Ok(Rc::new(answer))),
            None => {
                let reason = format!("the test's fake `Net` has no answer for `{sent}`: give it one with `net.script`");
                self.net_error(&Failure::Other(reason), span)
            }
        }
    }

    fn connection_method(
        &mut self,
        connection: &Instance,
        method: &str,
        args: Vec<CallArg>,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let handle = handle_of(connection);
        let (args, timeout) = with_timeout(args, method, span)?;
        let faked = self.faked_net();
        let done = match (method, args.as_slice()) {
            ("read", [Value::Int(count)]) => {
                let count = usize::try_from(*count)
                    .map_err(|_| Diagnostic::new(format!("`read` can't give {count} bytes"), span))?;
                let read = if faked {
                    self.fake_read(handle, Some(count), span)?
                } else {
                    read(handle, count, timeout).map(Some)
                };
                read.map(|data| Value::Bytes(Rc::from(data.unwrap_or_default())))
            }
            ("read_line", []) => {
                let read = if faked { self.fake_read(handle, None, span)? } else { read_line(handle, timeout) };
                read.map(|line| match line {
                    Some(line) => Value::str(String::from_utf8_lossy(trim_line(&line)).as_ref()),
                    None => Value::None,
                })
            }
            ("write", [data @ (Value::Str(_) | Value::Bytes(_))]) => {
                let data: &[u8] = match data {
                    Value::Str(text) => text.as_bytes(),
                    Value::Bytes(data) => data,
                    _ => &[],
                };
                let data = data.to_vec();
                if faked { self.fake_write(handle, &data) } else { write(handle, &data) }.map(|()| Value::Unit)
            }
            ("close", []) => {
                if faked { self.fake_close(handle) } else { close(handle) }
                return Ok(Value::Unit);
            }
            ("read" | "read_line" | "write" | "close", _) => return Err(wrong(method, span)),
            _ => return Err(no_method(&Value::Instance(Rc::new(connection.clone())), method, span)),
        };
        match done {
            Ok(value) => Ok(Value::Ok(Rc::new(value))),
            Err(failure) => self.net_error(&failure, span),
        }
    }

    fn udp_method(
        &mut self,
        socket: &Instance,
        method: &str,
        args: Vec<CallArg>,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let handle = handle_of(socket);
        let (args, timeout) = with_timeout(args, method, span)?;
        let faked = self.faked_net();
        let done = match (method, args.as_slice()) {
            ("receive", []) => {
                let received = if faked { self.fake_receive(handle, span)? } else { receive(handle, timeout) };
                match received {
                    Ok((data, sender)) => {
                        let fields = vec![("data", Value::Bytes(Rc::from(data))), ("sender", Value::str(&sender))];
                        Ok(self.make_value("Packet", fields, span)?)
                    }
                    Err(failure) => Err(failure),
                }
            }
            ("send_to", [data @ (Value::Str(_) | Value::Bytes(_)), Value::Str(address)]) => {
                let data: &[u8] = match data {
                    Value::Str(text) => text.as_bytes(),
                    Value::Bytes(data) => data,
                    _ => &[],
                };
                let data = data.to_vec();
                if faked { self.fake_send_to(handle, &data, address) } else { send_to(handle, &data, address) }
                    .map(|()| Value::Unit)
            }
            ("close", []) => {
                if faked { self.fake_close(handle) } else { close(handle) }
                return Ok(Value::Unit);
            }
            ("receive" | "send_to" | "close", _) => return Err(wrong(method, span)),
            _ => return Err(no_method(&Value::Instance(Rc::new(socket.clone())), method, span)),
        };
        match done {
            Ok(value) => Ok(Value::Ok(Rc::new(value))),
            Err(failure) => self.net_error(&failure, span),
        }
    }

    fn server_method(&mut self, server: &Instance, method: &str, args: Vec<CallArg>, span: Span) -> Result<Value, Diagnostic> {
        let handle = handle_of(server);
        let (args, timeout) = with_timeout(args, method, span)?;
        let faked = self.faked_net();
        match (method, args.as_slice()) {
            ("accept", []) => {
                let accepted = if faked { self.fake_accept(handle) } else { accept(handle, timeout) };
                match accepted {
                    Ok((connection, peer)) => {
                        let fields = vec![("_handle", Value::Int(connection)), ("peer", Value::str(&peer))];
                        Ok(Value::Ok(Rc::new(self.make_value("Connection", fields, span)?)))
                    }
                    Err(failure) => self.net_error(&failure, span),
                }
            }
            ("close", []) => {
                if faked { self.fake_close(handle) } else { close(handle) }
                Ok(Value::Unit)
            }
            ("accept" | "close", _) => Err(wrong(method, span)),
            _ => Err(no_method(&Value::Instance(Rc::new(server.clone())), method, span)),
        }
    }

    /// Takes a real server's listener out of the program's open sockets, for `parallel(server)`
    /// to serve from. A server in a test's fake network, or one already closed, has none.
    #[cfg(any(test, feature = "threads"))]
    pub(crate) fn take_listener(&mut self, server: &Value) -> Option<TcpListener> {
        let Value::Instance(server) = server else { return None };
        take_listener(handle_of(server))
    }

    /// The case of `NetError` a failure is.
    pub(crate) fn net_error(&mut self, failure: &Failure, span: Span) -> Result<Value, Diagnostic> {
        let (case, fields) = match failure {
            Failure::NotFound(host) => ("NotFound", vec![Value::str(host)]),
            Failure::Refused(host, port) => ("Refused", vec![Value::str(host), Value::Int(*port)]),
            Failure::TimedOut(after) => {
                let nanos = i64::try_from(after.as_nanos()).unwrap_or(i64::MAX);
                ("TimedOut", vec![self.make_value("Duration", vec![("nanos", Value::Int(nanos))], span)?])
            }
            Failure::Closed => ("Closed", Vec::new()),
            Failure::Tls(reason) => ("Tls", vec![Value::str(reason)]),
            Failure::Other(reason) => ("Other", vec![Value::str(reason)]),
        };
        let args = fields.into_iter().map(|value| CallArg::plain(value, span)).collect();
        let error = self.construct_variant(Some("NetError"), case, args, span)?;
        Ok(Value::Err(Rc::new(error)))
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

impl Fake {
    fn close_stream(&mut self, handle: i64) {
        if let Some(stream) = self.streams.remove(&handle)
            && let Some(other) = self.streams.get_mut(&stream.peer)
        {
            other.peer_closed = true;
        }
    }
}

/// Takes a real server's listener out of the table, for `parallel(server)` to serve from.
#[cfg(any(test, feature = "threads"))]
fn take_listener(handle: i64) -> Option<TcpListener> {
    let mut open = sockets();
    if !matches!(open.get(&handle).map(AsRef::as_ref), Some(Socket::Listener(_))) {
        return None;
    }
    let taken = open.remove(&handle)?;
    match Arc::try_unwrap(taken) {
        Ok(Socket::Listener(listener)) => Some(listener),
        Ok(_) => None,
        Err(shared) => match shared.as_ref() {
            Socket::Listener(listener) => listener.try_clone().ok(),
            _ => None,
        },
    }
}

/// Connects to a host, giving back the connection's handle and the other side's address.
pub(crate) fn connect(host: &str, port: u16, timeout: Duration) -> Result<(i64, String), Failure> {
    let addresses: Vec<_> = (host, port).to_socket_addrs().map_err(|_| Failure::NotFound(host.to_owned()))?.collect();
    if addresses.is_empty() {
        return Err(Failure::NotFound(host.to_owned()));
    }
    let mut failure = Failure::NotFound(host.to_owned());
    for address in addresses {
        match TcpStream::connect_timeout(&address, timeout) {
            Ok(stream) => {
                let peer = stream.peer_addr().map_or_else(|_| format!("{host}:{port}"), |peer| peer.to_string());
                return Ok((keep_stream(stream)?, peer));
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

/// Listens on a port, giving back the server's handle and the port it got.
pub(crate) fn listen(port: u16) -> Result<(i64, u16), Failure> {
    let listener = TcpListener::bind(("0.0.0.0", port)).map_err(|error| match error.kind() {
        ErrorKind::AddrInUse => Failure::Other(format!("port {port} is already in use")),
        _ => Failure::Other(error.to_string()),
    })?;
    let port = listener.local_addr().map_or(port, |address| address.port());
    Ok((keep(Socket::Listener(listener)), port))
}

/// Up to `count` bytes, empty once the other side is done.
pub(crate) fn read(handle: i64, count: usize, timeout: Option<Duration>) -> Result<Vec<u8>, Failure> {
    let open = socket(handle).ok_or(Failure::Closed)?;
    let Socket::Stream { reader, .. } = open.as_ref() else { return Err(Failure::Closed) };
    let mut stream = reader.lock().unwrap_or_else(PoisonError::into_inner);
    stream.get_ref().set_read_timeout(timeout).map_err(|error| Failure::of(&error, timeout))?;
    let mut data = vec![0; count];
    let read = stream.read(&mut data).map_err(|error| Failure::of(&error, timeout))?;
    data.truncate(read);
    Ok(data)
}

/// The next line with its ending, or `None` once the other side is done.
pub(crate) fn read_line(handle: i64, timeout: Option<Duration>) -> Result<Option<Vec<u8>>, Failure> {
    let open = socket(handle).ok_or(Failure::Closed)?;
    let Socket::Stream { reader, .. } = open.as_ref() else { return Err(Failure::Closed) };
    let mut stream = reader.lock().unwrap_or_else(PoisonError::into_inner);
    stream.get_ref().set_read_timeout(timeout).map_err(|error| Failure::of(&error, timeout))?;
    let mut line = Vec::new();
    let read = stream.read_until(b'\n', &mut line).map_err(|error| Failure::of(&error, timeout))?;
    Ok((read > 0).then_some(line))
}

/// Sends all of `data`.
pub(crate) fn write(handle: i64, data: &[u8]) -> Result<(), Failure> {
    let open = socket(handle).ok_or(Failure::Closed)?;
    let Socket::Stream { writer, .. } = open.as_ref() else { return Err(Failure::Closed) };
    let mut stream: &TcpStream = writer;
    stream.write_all(data).and_then(|()| stream.flush()).map_err(|error| Failure::of(&error, None))
}

/// The next connection to a server, as its handle and the other side's address.
pub(crate) fn accept(handle: i64, timeout: Option<Duration>) -> Result<(i64, String), Failure> {
    let open = socket(handle).ok_or(Failure::Closed)?;
    let Socket::Listener(listener) = open.as_ref() else { return Err(Failure::Closed) };
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
    Ok((keep_stream(stream)?, peer.to_string()))
}

/// Opens a UDP port, giving back the socket's handle and the port it got.
pub(crate) fn udp(port: u16) -> Result<(i64, u16), Failure> {
    let socket = UdpSocket::bind(("0.0.0.0", port)).map_err(|error| match error.kind() {
        ErrorKind::AddrInUse => Failure::Other(format!("port {port} is already in use")),
        _ => Failure::Other(error.to_string()),
    })?;
    let port = socket.local_addr().map_or(port, |address| address.port());
    Ok((keep(Socket::Udp(socket)), port))
}

/// The most a UDP packet can hold.
const MOST_IN_A_PACKET: usize = 65_507;

/// The next packet and who sent it.
pub(crate) fn receive(handle: i64, timeout: Option<Duration>) -> Result<(Vec<u8>, String), Failure> {
    let open = socket(handle).ok_or(Failure::Closed)?;
    let Socket::Udp(socket) = open.as_ref() else { return Err(Failure::Closed) };
    socket.set_read_timeout(timeout).map_err(|error| Failure::of(&error, timeout))?;
    let mut data = vec![0; MOST_IN_A_PACKET];
    let (size, sender) = socket.recv_from(&mut data).map_err(|error| Failure::of(&error, timeout))?;
    data.truncate(size);
    Ok((data, sender.to_string()))
}

/// Sends one packet to an address like `"127.0.0.1:9001"`.
pub(crate) fn send_to(handle: i64, data: &[u8], address: &str) -> Result<(), Failure> {
    let open = socket(handle).ok_or(Failure::Closed)?;
    let Socket::Udp(socket) = open.as_ref() else { return Err(Failure::Closed) };
    let host = address.rsplit_once(':').map_or(address, |(host, _)| host);
    let target = address
        .to_socket_addrs()
        .map_err(|_| Failure::NotFound(host.to_owned()))?
        .next()
        .ok_or_else(|| Failure::NotFound(host.to_owned()))?;
    socket.send_to(data, target).map(|_| ()).map_err(|error| Failure::of(&error, None))
}

/// Closes a connection or server. A read or accept waiting on another thread ends what it's doing
/// first, and the socket goes when it does.
pub(crate) fn close(handle: i64) {
    sockets().remove(&handle);
}

fn connection_made(handle: i64, peer: &str) -> Made {
    ("Connection", vec![("_handle", Value::Int(handle)), ("peer", Value::str(peer))])
}

fn udp_made(handle: i64, port: u16) -> Made {
    ("UdpSocket", vec![("_handle", Value::Int(handle)), ("port", Value::Int(i64::from(port)))])
}

fn server_made(handle: i64, port: u16) -> Made {
    ("Server", vec![("_handle", Value::Int(handle)), ("port", Value::Int(i64::from(port)))])
}

/// A `NetError` as a value, for compiled code, which has no interpreter to make one.
fn net_error_value(failure: &Failure) -> Value {
    let (case, fields): (&str, Vec<(&str, Value)>) = match failure {
        Failure::NotFound(host) => ("NotFound", vec![("host", Value::str(host))]),
        Failure::Refused(host, port) => ("Refused", vec![("host", Value::str(host)), ("port", Value::Int(*port))]),
        Failure::TimedOut(after) => {
            let nanos = i64::try_from(after.as_nanos()).unwrap_or(i64::MAX);
            ("TimedOut", vec![("after", Value::made("Duration", vec![("nanos", Value::Int(nanos))]))])
        }
        Failure::Closed => ("Closed", Vec::new()),
        Failure::Tls(reason) => ("Tls", vec![("reason", Value::str(reason))]),
        Failure::Other(reason) => ("Other", vec![("reason", Value::str(reason))]),
    };
    Value::Err(Rc::new(Value::made_error_case("NetError", case, fields)))
}

/// What a call that can fail gives compiled code.
fn outcome(done: Result<Value, Failure>) -> Value {
    match done {
        Ok(value) => Value::Ok(Rc::new(value)),
        Err(failure) => net_error_value(&failure),
    }
}

fn made(made: Made) -> Value {
    Value::made(made.0, made.1)
}

/// The arguments compiled code handed in and the `Duration` given as `timeout=`: `named` says
/// which named arguments come last.
fn native_timeout<'a>(args: &'a [Value], named: &[&str], span: Span) -> Result<(&'a [Value], Option<Duration>), Diagnostic> {
    let plain = args.len().saturating_sub(named.len());
    let (args, last) = args.split_at(plain);
    let mut timeout = None;
    for (name, value) in named.iter().zip(last) {
        match *name {
            "timeout" => timeout = Some(duration_of(value, span)?),
            // Only `net.udp(port=...)`, whose one argument this is.
            "port" => return Ok((last, timeout)),
            other => return Err(Diagnostic::new(format!("this has no parameter called `{other}`"), span)),
        }
    }
    Ok((args, timeout))
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
    for (name, value) in named {
        match (*name, value) {
            ("timeout", value) => request.timeout = duration_of(value, span)?,
            ("body", Value::Str(text)) => request.body = text.as_bytes().to_vec(),
            ("body", Value::Bytes(data)) => request.body = data.to_vec(),
            ("params", Value::Dict(pairs)) => request.params = text_pairs(pairs, span)?,
            ("headers", Value::Dict(pairs)) => request.headers = text_pairs(pairs, span)?,
            (other, _) => {
                return Err(Diagnostic::new(format!("this has no parameter called `{other}`"), span));
            }
        }
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

/// What `Net` does for compiled code. `method` has any named arguments on the end as a call writes
/// them, like `connect.timeout`.
pub(crate) fn native_net(method: &str, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
    let mut parts = method.split('.');
    let method = parts.next().unwrap_or_default();
    let named: Vec<&str> = parts.collect();
    if matches!(method, "get" | "post" | "request") {
        let (plain, last) = args.split_at(args.len().saturating_sub(named.len()));
        let given: Vec<(&str, &Value)> = named.iter().copied().zip(last).collect();
        let request = request_of(method, plain, &given, span)?;
        crate::embed::write_kept_output();
        return Ok(outcome(http::send(&request).map(answer_value)));
    }
    let (args, timeout) = native_timeout(args, &named, span)?;
    crate::embed::write_kept_output();
    match (method, args) {
        ("connect", [Value::Str(host), Value::Int(port)]) => {
            let port = port_of(*port, span)?;
            let timeout = timeout.unwrap_or(CONNECT_TIMEOUT);
            Ok(outcome(connect(host, port, timeout).map(|(handle, peer)| made(connection_made(handle, &peer)))))
        }
        ("listen", [Value::Int(port)]) => {
            let port = port_of(*port, span)?;
            Ok(outcome(listen(port).map(|(handle, port)| made(server_made(handle, port)))))
        }
        ("udp", [] | [Value::Int(_)]) => {
            let port = if let [Value::Int(port)] = args { port_of(*port, span)? } else { 0 };
            Ok(outcome(udp(port).map(|(handle, port)| made(udp_made(handle, port)))))
        }
        _ => Err(wrong(method, span)),
    }
}

/// A method of a `Connection`, `Server` or `UdpSocket` for compiled code.
pub(crate) fn native_method(
    instance: &Instance,
    method: &str,
    args: &[Value],
    named: &[&str],
    span: Span,
) -> Result<Value, Diagnostic> {
    if &*instance.type_name == "Response" {
        return match method {
            "text" => Ok(response_text(instance)),
            _ => Err(wrong(method, span)),
        };
    }
    let handle = handle_of(instance);
    let (args, timeout) = native_timeout(args, named, span)?;
    if method == "close" {
        close(handle);
        return Ok(Value::Unit);
    }
    // What's been printed comes out before anything that waits.
    crate::embed::write_kept_output();
    Ok(match (&*instance.type_name, method, args) {
        ("Connection", "read", [Value::Int(count)]) => {
            let count = usize::try_from(*count)
                .map_err(|_| Diagnostic::new(format!("`read` can't give {count} bytes"), span))?;
            outcome(read(handle, count, timeout).map(|data| Value::Bytes(Rc::from(data))))
        }
        ("Connection", "read_line", []) => outcome(read_line(handle, timeout).map(|line| match line {
            Some(line) => Value::str(String::from_utf8_lossy(trim_line(&line)).as_ref()),
            None => Value::None,
        })),
        ("Connection", "write", [Value::Str(text)]) => {
            outcome(write(handle, text.as_bytes()).map(|()| Value::Unit))
        }
        ("Connection", "write", [Value::Bytes(data)]) => outcome(write(handle, data).map(|()| Value::Unit)),
        ("Server", "accept", []) => {
            outcome(accept(handle, timeout).map(|(connection, peer)| made(connection_made(connection, &peer))))
        }
        ("UdpSocket", "receive", []) => outcome(receive(handle, timeout).map(|(data, sender)| {
            Value::made("Packet", vec![("data", Value::Bytes(Rc::from(data))), ("sender", Value::str(&sender))])
        })),
        ("UdpSocket", "send_to", [Value::Str(text), Value::Str(address)]) => {
            outcome(send_to(handle, text.as_bytes(), address).map(|()| Value::Unit))
        }
        ("UdpSocket", "send_to", [Value::Bytes(data), Value::Str(address)]) => {
            outcome(send_to(handle, data, address).map(|()| Value::Unit))
        }
        _ => return Err(wrong(method, span)),
    })
}

/// A line without its ending, `\n` or `\r\n`.
fn trim_line(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}
