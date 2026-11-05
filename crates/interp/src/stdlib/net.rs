//! `std.net`: TCP through the `Net` capability. A `Connection` or `Server` holds a private
//! handle into the program's table of open sockets, so code that was never given `Net` can't
//! make one up to reach a socket. What's real here needs nothing of the interpreter, as native
//! code runs it too. In a test, the network is in memory, in `Fake`.
//! Nothing else runs while a test waits, so a read that would wait forever stops the test and
//! says why, and a server with no connection waiting is done.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream, ToSocketAddrs};
use std::rc::Rc;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
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

/// A real connection, which any thread holding its handle can use: one thread reading and another
/// writing don't wait for each other, and nothing's interleaved within either.
struct Stream {
    socket: TcpStream,
    reader: Mutex<BufReader<TcpStream>>,
    writing: Mutex<()>,
}

#[derive(Clone)]
enum Socket {
    Stream(Arc<Stream>),
    Listener(Arc<TcpListener>),
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
pub(crate) enum Failure {
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

    /// The types what's real gives back: a program using `Net` needn't have named them.
    fn load_net_types(&mut self) -> Result<(), Diagnostic> {
        for name in ["Connection", "NetError", "Duration"] {
            self.load_std_providing(name)?;
        }
        Ok(())
    }

    /// `net.connect(host, port)` and `net.listen(port)`.
    pub(crate) fn net_method(&mut self, method: &str, args: Vec<CallArg>, span: Span) -> Result<Value, Diagnostic> {
        let (args, timeout) = with_timeout(args, method, span)?;
        self.load_net_types()?;
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
            ("connect" | "listen", _) => return Err(wrong(method, span)),
            _ => return Err(no_method(&Value::Capability(Capability::Net), method, span)),
        };
        Ok(made_or_failed(found))
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

/// `net.connect(host, port)` and `net.listen(port)` on the real network: needing nothing of the
/// interpreter, for native code too.
pub(crate) fn real_net(method: &str, args: &[Value], timeout: Option<Duration>, span: Span) -> Result<Value, Diagnostic> {
    let found = match (method, args) {
        ("connect", [Value::Str(host), Value::Int(port)]) => {
            connect(host, port_of(*port, span)?, timeout.unwrap_or(CONNECT_TIMEOUT))
        }
        ("listen", [Value::Int(port)]) => listen(port_of(*port, span)?),
        ("connect" | "listen", _) => return Err(wrong(method, span)),
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

fn bytes_asked(count: i64, span: Span) -> Result<usize, Diagnostic> {
    usize::try_from(count).map_err(|_| Diagnostic::new(format!("`read` can't give {count} bytes"), span))
}

fn data_of(data: &Value) -> &[u8] {
    match data {
        Value::Str(text) => text.as_bytes(),
        Value::Bytes(data) => data,
        _ => &[],
    }
}

fn line_value(line: Option<&[u8]>) -> Value {
    line.map_or(Value::None, |line| Value::str(String::from_utf8_lossy(trim_line(line)).as_ref()))
}

fn connection_made((connection, peer): (i64, String)) -> Made {
    ("Connection", vec![("_handle", Value::Int(connection)), ("peer", Value::str(&peer))])
}

fn made_or_failed(found: Result<Made, Failure>) -> Value {
    done_or_failed(found.map(|(type_name, fields)| Value::made(type_name, fields)))
}

fn done_or_failed(done: Result<Value, Failure>) -> Value {
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
        Failure::Other(reason) => ("Other", vec![("reason", Value::str(reason))]),
    };
    Value::Err(Rc::new(Value::made_case("NetError", case, fields)))
}

fn connect(host: &str, port: u16, timeout: Duration) -> Result<Made, Failure> {
    let addresses: Vec<_> = (host, port).to_socket_addrs().map_err(|_| Failure::NotFound(host.to_owned()))?.collect();
    if addresses.is_empty() {
        return Err(Failure::NotFound(host.to_owned()));
    }
    let mut failure = Failure::NotFound(host.to_owned());
    for address in addresses {
        match TcpStream::connect_timeout(&address, timeout) {
            Ok(stream) => {
                let peer = stream.peer_addr().map_or_else(|_| format!("{host}:{port}"), |peer| peer.to_string());
                let handle = keep_stream(stream)?;
                return Ok(connection_made((handle, peer)));
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

fn keep_stream(socket: TcpStream) -> Result<i64, Failure> {
    let reader = socket.try_clone().map_err(|error| Failure::Other(error.to_string()))?;
    let stream = Stream { socket, reader: Mutex::new(BufReader::new(reader)), writing: Mutex::new(()) };
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

fn read(handle: i64, count: usize, timeout: Option<Duration>) -> Result<Vec<u8>, Failure> {
    let stream = stream(handle)?;
    let mut reader = stream.reader.lock().unwrap_or_else(PoisonError::into_inner);
    reader.get_ref().set_read_timeout(timeout).map_err(|error| Failure::of(&error, timeout))?;
    let mut data = vec![0; count];
    let read = reader.read(&mut data).map_err(|error| Failure::of(&error, timeout))?;
    data.truncate(read);
    Ok(data)
}

fn read_line(handle: i64, timeout: Option<Duration>) -> Result<Option<Vec<u8>>, Failure> {
    let stream = stream(handle)?;
    let mut reader = stream.reader.lock().unwrap_or_else(PoisonError::into_inner);
    reader.get_ref().set_read_timeout(timeout).map_err(|error| Failure::of(&error, timeout))?;
    let mut line = Vec::new();
    let read = reader.read_until(b'\n', &mut line).map_err(|error| Failure::of(&error, timeout))?;
    Ok((read > 0).then_some(line))
}

fn write(handle: i64, data: &[u8]) -> Result<(), Failure> {
    let stream = stream(handle)?;
    let _writing = stream.writing.lock().unwrap_or_else(PoisonError::into_inner);
    let mut socket = &stream.socket;
    socket.write_all(data).and_then(|()| socket.flush()).map_err(|error| Failure::of(&error, None))
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
    Ok((keep_stream(stream)?, peer.to_string()))
}

/// Closes a socket. Whoever else was using it finds out: a read waiting on another thread ends.
fn close(handle: i64) {
    if let Some(Socket::Stream(stream)) = open(|open| open.remove(&handle)) {
        // It may be closed already from the other side, which is what closing wanted.
        let _ = stream.socket.shutdown(Shutdown::Both);
    }
}

/// Takes a real server's listener out of the open sockets, for `parallel(server)` to serve from.
/// A server in a test's fake network, or one already closed, has none.
#[cfg(any(test, feature = "threads"))]
pub(crate) fn take_listener(server: &Value) -> Option<TcpListener> {
    let Value::Instance(server) = server else { return None };
    let handle = handle_of(server);
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
