//! `std.net`: TCP through the `Net` capability. A `Connection` or `Server` holds a handle into
//! the program's table of open sockets. Real handles are random, so code that was never given
//! `Net` can't make one up to reach a socket. In a test, the network is in memory, in `Fake`.
//! Nothing else runs while a test waits, so a read that would wait forever stops the test and
//! says why, and a server with no connection waiting is done.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::rc::Rc;
use std::time::{Duration, Instant};

use fennec_syntax::{Diagnostic, Span};

use super::{Module, no_constants, no_functions};
use crate::call::CallArg;
use crate::interp::Interp;
use crate::methods::no_method;
use crate::value::{Capability, Instance, Value};

pub(crate) const MODULE: Module = Module {
    functions: &[],
    call: no_functions,
    constant: no_constants,
    methods: &[("Connection", connection_method), ("Server", server_method)],
};

/// How long connecting waits unless told otherwise.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// The hosts a test's fake network has: its own, where its servers are.
const FAKE_HOSTS: [&str; 2] = ["localhost", "127.0.0.1"];

enum Socket {
    Stream(BufReader<TcpStream>),
    Listener(TcpListener),
}

/// The real sockets a program has open, by handle.
#[derive(Default)]
pub(crate) struct Sockets {
    open: HashMap<i64, Socket>,
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
enum Failure {
    NotFound(String),
    Refused(String, i64),
    TimedOut(Duration),
    Closed,
    Other(String),
}

impl Failure {
    /// The failure an I/O error on an open connection means.
    fn of(error: &std::io::Error, waited: Option<Duration>) -> Failure {
        match error.kind() {
            ErrorKind::WouldBlock | ErrorKind::TimedOut => Failure::TimedOut(waited.unwrap_or_default()),
            ErrorKind::BrokenPipe | ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted => Failure::Closed,
            _ => Failure::Other(error.to_string()),
        }
    }
}

/// A random handle for a real socket, which nothing could guess. A handle is looked up in the
/// program's own table, so one this program wasn't given reaches nothing.
///
/// TODO: once `_` fields are private, as decided under Visibility, making a `Connection` or
/// `Server` outside `std.net` is a compile error, and the randomness is only a second wall.
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

/// The arguments, and the `Duration` given as `timeout=`, if it was.
fn with_timeout(args: Vec<CallArg>, method: &str, span: Span) -> Result<(Vec<Value>, Option<Duration>), Diagnostic> {
    let mut plain = Vec::new();
    let mut timeout = None;
    for arg in args {
        match arg.name.as_deref() {
            None => plain.push(arg.value),
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

    /// `net.connect(host, port)` and `net.listen(port)`.
    pub(crate) fn net_method(&mut self, method: &str, args: Vec<CallArg>, span: Span) -> Result<Value, Diagnostic> {
        let (args, timeout) = with_timeout(args, method, span)?;
        let found = match (method, args.as_slice()) {
            ("connect", [Value::Str(host), Value::Int(port)]) => {
                let port = port_of(*port, span)?;
                let timeout = timeout.unwrap_or(CONNECT_TIMEOUT);
                if self.faked_net() { self.fake_connect(host, port) } else { self.real_connect(host, port, timeout) }
            }
            ("listen", [Value::Int(port)]) => {
                let port = port_of(*port, span)?;
                if self.faked_net() { self.fake_listen(port) } else { self.real_listen(port) }
            }
            ("connect" | "listen", _) => return Err(wrong(method, span)),
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
                let read = if faked { self.fake_read(handle, Some(count), span)? } else { self.real_read(handle, count, timeout) };
                read.map(|data| Value::Bytes(Rc::from(data.unwrap_or_default())))
            }
            ("read_line", []) => {
                let read = if faked { self.fake_read(handle, None, span)? } else { self.real_read_line(handle, timeout) };
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
                if faked { self.fake_write(handle, &data) } else { self.real_write(handle, &data) }.map(|()| Value::Unit)
            }
            ("close", []) => {
                if faked { self.fake_close(handle) } else { self.sockets.open.remove(&handle); }
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

    fn server_method(&mut self, server: &Instance, method: &str, args: Vec<CallArg>, span: Span) -> Result<Value, Diagnostic> {
        let handle = handle_of(server);
        let (args, timeout) = with_timeout(args, method, span)?;
        let faked = self.faked_net();
        match (method, args.as_slice()) {
            ("accept", []) => {
                let accepted = if faked { self.fake_accept(handle) } else { self.real_accept(handle, timeout) };
                match accepted {
                    Ok((connection, peer)) => {
                        let fields = vec![("_handle", Value::Int(connection)), ("peer", Value::str(&peer))];
                        Ok(Value::Ok(Rc::new(self.make_value("Connection", fields, span)?)))
                    }
                    Err(failure) => self.net_error(&failure, span),
                }
            }
            ("close", []) => {
                if faked { self.fake_close(handle) } else { self.sockets.open.remove(&handle); }
                Ok(Value::Unit)
            }
            ("accept" | "close", _) => Err(wrong(method, span)),
            _ => Err(no_method(&Value::Instance(Rc::new(server.clone())), method, span)),
        }
    }

    /// The case of `NetError` a failure is.
    fn net_error(&mut self, failure: &Failure, span: Span) -> Result<Value, Diagnostic> {
        let (case, fields) = match failure {
            Failure::NotFound(host) => ("NotFound", vec![Value::str(host)]),
            Failure::Refused(host, port) => ("Refused", vec![Value::str(host), Value::Int(*port)]),
            Failure::TimedOut(after) => {
                let nanos = i64::try_from(after.as_nanos()).unwrap_or(i64::MAX);
                ("TimedOut", vec![self.make_value("Duration", vec![("nanos", Value::Int(nanos))], span)?])
            }
            Failure::Closed => ("Closed", Vec::new()),
            Failure::Other(reason) => ("Other", vec![Value::str(reason)]),
        };
        let args = fields.into_iter().map(|value| CallArg::plain(value, span)).collect();
        let error = self.construct_variant(Some("NetError"), case, args, span)?;
        Ok(Value::Err(Rc::new(error)))
    }

    fn real_connect(&mut self, host: &str, port: u16, timeout: Duration) -> Result<Made, Failure> {
        let addresses: Vec<_> = (host, port).to_socket_addrs().map_err(|_| Failure::NotFound(host.to_owned()))?.collect();
        if addresses.is_empty() {
            return Err(Failure::NotFound(host.to_owned()));
        }
        let mut failure = Failure::NotFound(host.to_owned());
        for address in addresses {
            match TcpStream::connect_timeout(&address, timeout) {
                Ok(stream) => {
                    let peer = stream.peer_addr().map_or_else(|_| format!("{host}:{port}"), |peer| peer.to_string());
                    let handle = fresh_handle(&self.sockets.open);
                    self.sockets.open.insert(handle, Socket::Stream(BufReader::new(stream)));
                    return Ok(("Connection", vec![("_handle", Value::Int(handle)), ("peer", Value::str(&peer))]));
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

    fn real_listen(&mut self, port: u16) -> Result<Made, Failure> {
        let listener = TcpListener::bind(("0.0.0.0", port)).map_err(|error| match error.kind() {
            ErrorKind::AddrInUse => Failure::Other(format!("port {port} is already in use")),
            _ => Failure::Other(error.to_string()),
        })?;
        let port = listener.local_addr().map_or(port, |address| address.port());
        let handle = fresh_handle(&self.sockets.open);
        self.sockets.open.insert(handle, Socket::Listener(listener));
        Ok(("Server", vec![("_handle", Value::Int(handle)), ("port", Value::Int(i64::from(port)))]))
    }

    fn stream(&mut self, handle: i64) -> Result<&mut BufReader<TcpStream>, Failure> {
        match self.sockets.open.get_mut(&handle) {
            Some(Socket::Stream(stream)) => Ok(stream),
            _ => Err(Failure::Closed),
        }
    }

    fn real_read(&mut self, handle: i64, count: usize, timeout: Option<Duration>) -> Result<Option<Vec<u8>>, Failure> {
        let stream = self.stream(handle)?;
        stream.get_ref().set_read_timeout(timeout).map_err(|error| Failure::of(&error, timeout))?;
        let mut data = vec![0; count];
        let read = stream.read(&mut data).map_err(|error| Failure::of(&error, timeout))?;
        data.truncate(read);
        Ok(Some(data))
    }

    fn real_read_line(&mut self, handle: i64, timeout: Option<Duration>) -> Result<Option<Vec<u8>>, Failure> {
        let stream = self.stream(handle)?;
        stream.get_ref().set_read_timeout(timeout).map_err(|error| Failure::of(&error, timeout))?;
        let mut line = Vec::new();
        let read = stream.read_until(b'\n', &mut line).map_err(|error| Failure::of(&error, timeout))?;
        Ok((read > 0).then_some(line))
    }

    fn real_write(&mut self, handle: i64, data: &[u8]) -> Result<(), Failure> {
        let stream = self.stream(handle)?.get_mut();
        stream.write_all(data).and_then(|()| stream.flush()).map_err(|error| Failure::of(&error, None))
    }

    fn real_accept(&mut self, handle: i64, timeout: Option<Duration>) -> Result<(i64, String), Failure> {
        let Some(Socket::Listener(listener)) = self.sockets.open.get(&handle) else { return Err(Failure::Closed) };
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
        let connection = fresh_handle(&self.sockets.open);
        self.sockets.open.insert(connection, Socket::Stream(BufReader::new(stream)));
        Ok((connection, peer.to_string()))
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

/// A line without its ending, `\n` or `\r\n`.
fn trim_line(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}
