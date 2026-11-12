//! The HTTP client of `std.net`: HTTP/1.1 over a connection of its own for each request, with TLS
//! for `https`, which always checks certificates. Redirects are followed, up to 10. A 404 is an
//! answer, not a failure, since what it means is the caller's to say.

use std::fmt::Write as _;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use super::net::SecureStream;
use super::net::{Failure, TlsServer};

/// How many redirects a request follows.
const MOST_REDIRECTS: usize = 10;

/// The most a response's status line and headers can take, so a server can't send them forever.
const MOST_IN_HEADERS: usize = 1 << 20;

/// The most headers a response can have, as many as any real server sends and more, so a head of
/// a million tiny ones can't be read.
const MOST_HEADERS: usize = 1000;

/// The most one line of a response's head can take, as curl has it.
const MOST_HEAD_LINE: usize = 100 * 1024;

/// The most a response's body can be unless `most_body=` says otherwise, so a server can't send
/// one forever.
pub(crate) const MOST_BODY: usize = 64 << 20;

/// What's asked for.
pub(crate) struct Request {
    pub(crate) method: String,
    pub(crate) url: String,
    pub(crate) params: Vec<(String, String)>,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
    pub(crate) timeout: Duration,
    /// The most the answer's body can be.
    pub(crate) most_body: usize,
}

/// What came back.
pub(crate) struct Answer {
    pub(crate) status: i64,
    /// By their names in lower case, in the order they came.
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

/// A URL's parts.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Url {
    secure: bool,
    host: String,
    port: u16,
    /// The path and query, as a request line writes them.
    target: String,
}

/// A URL as a message can say it: with any name and password in it taken out.
fn without_password(url: &str) -> String {
    let after_scheme = url.find("://").map_or(0, |at| at + 3);
    let rest = url.get(after_scheme..).unwrap_or("");
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    match rest.get(..end).and_then(|authority| authority.rfind('@')) {
        Some(at) => format!("{}{}", url.get(..after_scheme).unwrap_or(""), rest.get(at + 1..).unwrap_or("")),
        None => url.to_owned(),
    }
}

fn parse_url(url: &str) -> Result<Url, Failure> {
    let shown = without_password(url);
    let bad = || Failure::Other(format!("`{shown}` isn't a URL like `https://example.com/path`"));
    let (scheme, rest) = url.split_once("://").ok_or_else(bad)?;
    let secure = match scheme.to_ascii_lowercase().as_str() {
        "http" => false,
        "https" => true,
        _ => return Err(Failure::Other(format!("`{shown}` isn't `http` or `https`"))),
    };
    let without_fragment = rest.split_once('#').map_or(rest, |(before, _)| before);
    let path_at = without_fragment.find(['/', '?']).unwrap_or(without_fragment.len());
    let (authority, target) = without_fragment.split_at(path_at);
    if authority.contains('@') {
        return Err(Failure::Other(format!("`{shown}` has a name and password in it; use `headers=`")));
    }
    let (host, port) = match authority.strip_prefix('[') {
        Some(after) => {
            let (host, rest) = after.split_once(']').ok_or_else(bad)?;
            (host, rest.strip_prefix(':'))
        }
        None => match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        },
    };
    if host.is_empty() {
        return Err(bad());
    }
    let port = match port {
        Some(port) => port.parse().map_err(|_| bad())?,
        None if secure => 443,
        None => 80,
    };
    let target = if target.starts_with('/') { target.to_owned() } else { format!("/{target}") };
    Ok(Url { secure, host: host.to_owned(), port, target })
}

/// `text` with what a URL can't hold written as `%XX`.
fn percent_encoded(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// A URL with its `params` on the end.
fn with_params(url: &str, params: &[(String, String)]) -> String {
    if params.is_empty() {
        return url.to_owned();
    }
    let query: Vec<String> = params
        .iter()
        .map(|(name, value)| format!("{}={}", percent_encoded(name), percent_encoded(value)))
        .collect();
    let joiner = if url.contains('?') { '&' } else { '?' };
    format!("{url}{joiner}{}", query.join("&"))
}

/// A request's URL with its params on the end, as a test's fake sees it.
pub(crate) fn written(request: &Request) -> String {
    with_params(&request.url, &request.params)
}

/// Where a redirect goes: a whole URL, a path on this host, or a path from where this one is.
fn redirected(from: &Url, location: &str) -> String {
    if location.contains("://") {
        return location.to_owned();
    }
    let scheme = if from.secure { "https" } else { "http" };
    let host = if from.host.contains(':') { format!("[{}]", from.host) } else { from.host.clone() };
    let base = format!("{scheme}://{host}:{}", from.port);
    if location.starts_with('/') {
        return format!("{base}{location}");
    }
    let path = from.target.split_once('?').map_or(from.target.as_str(), |(path, _)| path);
    let folder = path.rsplit_once('/').map_or("", |(folder, _)| folder);
    format!("{base}{folder}/{location}")
}

/// Sends a request, following redirects, giving back the last answer.
pub(crate) fn send(request: &Request) -> Result<Answer, Failure> {
    let deadline = Instant::now() + request.timeout;
    let mut method = request.method.clone();
    let mut url = with_params(&request.url, &request.params);
    let mut body: &[u8] = &request.body;
    let first = parse_url(&url)?;
    for _ in 0..=MOST_REDIRECTS {
        let at = parse_url(&url)?;
        // What says who's asking goes only to the site it was written for: the same scheme, host
        // and port. A name the program made up could be a key, so only what's known to be
        // harmless goes anywhere else.
        let same_site =
            at.secure == first.secure && at.port == first.port && at.host.eq_ignore_ascii_case(&first.host);
        let headers = request
            .headers
            .iter()
            .filter(|(name, _)| same_site || is_harmless(name))
            .map(|(name, value)| (name.as_str(), value.as_str()));
        let answer = once(&at, &method, headers, body, (deadline, request.timeout), request.most_body)?;
        if !matches!(answer.status, 301 | 302 | 303 | 307 | 308) {
            return Ok(answer);
        }
        let Some((_, location)) = answer.headers.iter().find(|(name, _)| name == "location") else {
            return Ok(answer);
        };
        url = redirected(&at, location);
        if !matches!(answer.status, 307 | 308) && method != "HEAD" {
            "GET".clone_into(&mut method);
            body = &[];
        }
    }
    Err(Failure::Other(format!("`{}` redirects more than {MOST_REDIRECTS} times", request.url)))
}

/// Whether a header says nothing about who's asking, so it can follow a redirect to another site.
fn is_harmless(name: &str) -> bool {
    ["accept", "accept-encoding", "accept-language", "content-type", "user-agent"]
        .iter()
        .any(|harmless| name.eq_ignore_ascii_case(harmless))
}

/// One request and its answer, on a connection of its own.
fn once<'h>(
    url: &Url,
    method: &str,
    headers: impl Iterator<Item = (&'h str, &'h str)>,
    body: &[u8],
    (deadline, timeout): (Instant, Duration),
    most_body: usize,
) -> Result<Answer, Failure> {
    if !is_token(method) {
        return Err(Failure::Other(format!("`{}` can't be a request's method", method.escape_debug())));
    }
    if !fits_on_a_line(&url.target) || !fits_on_a_line(&url.host) {
        return Err(Failure::Other("the URL has a space, line break or control character in it".to_owned()));
    }
    // Every header's checked before anything's sent, or any connection tried: one whose name
    // isn't one, or whose value would start a line of its own, could make one request two.
    let headers: Vec<(&str, &str)> = headers.collect();
    for (name, value) in &headers {
        if !is_token(name) || value.bytes().any(|byte| (byte < 32 && byte != b'\t') || byte == 127) {
            let shown = name.escape_debug();
            return Err(Failure::Other(format!("the header `{shown}` has a line break or `:` in it")));
        }
    }
    let tcp = connect(url, deadline, timeout)?;
    let mut stream =
        if url.secure { secured(tcp, &url.host, deadline, timeout)? } else { Stream::Plain(tcp) };
    let host = if matches!((url.secure, url.port), (false, 80) | (true, 443)) {
        url.host.clone()
    } else {
        format!("{}:{}", url.host, url.port)
    };
    let mut head = format!("{method} {} HTTP/1.1\r\nHost: {host}\r\n", url.target);
    let (mut agent, mut accept) = (false, false);
    for (name, value) in headers {
        agent |= name.eq_ignore_ascii_case("user-agent");
        accept |= name.eq_ignore_ascii_case("accept");
        let _ = write!(head, "{name}: {value}\r\n");
    }
    if !agent {
        head.push_str("User-Agent: fennec\r\n");
    }
    if !accept {
        head.push_str("Accept: */*\r\n");
    }
    if !body.is_empty() || matches!(method, "POST" | "PUT" | "PATCH") {
        let _ = write!(head, "Content-Length: {}\r\n", body.len());
    }
    head.push_str("Connection: close\r\n\r\n");
    // The head and the body in one write, so a small request goes in one packet: written apart,
    // the body waits for the server to acknowledge the head, which it may hold back for 40 ms.
    let mut request = head.into_bytes();
    request.extend_from_slice(body);
    stream.write_all_by(&request, deadline, timeout)?;
    read_answer(&mut stream, method, (deadline, timeout), most_body)
}

/// Whether this is what HTTP calls a token, as a method or a header's name has to be.
fn is_token(text: &str) -> bool {
    !text.is_empty()
        && text.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

/// Whether this can be written on a request's first line: it has no space or control character,
/// either of which would end the line or the part of it, and start something a server would read.
fn fits_on_a_line(text: &str) -> bool {
    text.bytes().all(|byte| byte > 32 && byte != 127)
}

fn connect(url: &Url, deadline: Instant, timeout: Duration) -> Result<TcpStream, Failure> {
    let addresses: Vec<_> = (url.host.as_str(), url.port)
        .to_socket_addrs()
        .map_err(|_| Failure::NotFound(url.host.clone()))?
        .collect();
    let mut failure = Failure::NotFound(url.host.clone());
    for address in addresses {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Failure::TimedOut(timeout));
        }
        match TcpStream::connect_timeout(&address, left) {
            Ok(stream) => {
                // A request is its head and then its body, two writes. Each goes out at once,
                // rather than the body waiting for the head to be acknowledged, which a server
                // may delay by up to 40 ms. If it can't be set, the request is only slower.
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Err(error) => {
                failure = match error.kind() {
                    ErrorKind::ConnectionRefused => Failure::Refused(url.host.clone(), i64::from(url.port)),
                    ErrorKind::TimedOut | ErrorKind::WouldBlock => Failure::TimedOut(timeout),
                    _ => Failure::Other(error.to_string()),
                };
            }
        }
    }
    Err(failure)
}

/// A connection with TLS over it.
trait Secure: Read + Write {
    fn tcp(&self) -> &TcpStream;
}

/// Puts TLS over a connection, once something has said it's wanted. A program that makes no
/// requests links nothing of TLS, or of what the system checks certificates with: a pointer here
/// is all that reaches them, and only `use_tls` sets it.
type WithTls = fn(TcpStream, &str, Instant, Duration) -> Result<Box<dyn Secure>, Failure>;

static WITH_TLS: OnceLock<WithTls> = OnceLock::new();

/// Says TLS is wanted: compiled code that makes a request calls this first, and so does the
/// `fennec` command.
pub fn use_tls() {
    WITH_TLS.get_or_init(|| tls::wrap);
    RAW_TLS.get_or_init(|| tls::raw);
    SERVER_TLS.get_or_init(|| tls::server);
}

/// A raw connection with TLS over it, as `net.connect(host, port, tls=True)` makes: the TCP
/// connection, its host's name, and how long the handshake may take.
pub(crate) type RawTls = fn(TcpStream, &str, Duration) -> Result<Box<dyn SecureStream>, Failure>;

static RAW_TLS: OnceLock<RawTls> = OnceLock::new();

/// Puts TLS over a raw connection, with its handshake done, checking the certificate against what
/// the system trusts: or says why it can't be.
pub(crate) fn raw_secured(
    tcp: TcpStream,
    host: &str,
    timeout: Duration,
) -> Result<Box<dyn SecureStream>, Failure> {
    let Some(raw) = RAW_TLS.get() else {
        return Err(Failure::Tls("this program was built without TLS: build it with LLVM".to_owned()));
    };
    raw(tcp, host, timeout)
}

/// What a server's certificate chain and private key, as PEM text, make: `net.listen`'s TLS.
pub(crate) type ServerTlsFrom = fn(&str, &str) -> Result<Arc<dyn TlsServer>, Failure>;

static SERVER_TLS: OnceLock<ServerTlsFrom> = OnceLock::new();

/// A server's TLS from its certificate chain and private key, or why there can't be one.
pub(crate) fn server_tls(chain: &str, key: &str) -> Result<Arc<dyn TlsServer>, Failure> {
    let Some(server) = SERVER_TLS.get() else {
        return Err(Failure::Tls("this program was built without TLS: build it with LLVM".to_owned()));
    };
    server(chain, key)
}

/// Puts TLS over a connection, or says why it can't be.
fn secured(tcp: TcpStream, host: &str, deadline: Instant, timeout: Duration) -> Result<Stream, Failure> {
    let Some(wrap) = WITH_TLS.get() else {
        return Err(Failure::Tls("this program was built without TLS: build it with LLVM".to_owned()));
    };
    Ok(Stream::Secure(wrap(tcp, host, deadline, timeout)?))
}

/// A connection a request goes over.
enum Stream {
    Plain(TcpStream),
    Secure(Box<dyn Secure>),
}

impl Stream {
    fn tcp(&self) -> &TcpStream {
        match self {
            Stream::Plain(tcp) => tcp,
            Stream::Secure(stream) => stream.tcp(),
        }
    }

    /// Reads some, by the time left. Nothing read means the other side is done, and `cut_off` says
    /// whether it went without saying so.
    fn read_by(
        &mut self,
        into: &mut [u8],
        (deadline, timeout): (Instant, Duration),
        cut_off: &mut bool,
    ) -> Result<usize, Failure> {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Failure::TimedOut(timeout));
        }
        self.tcp().set_read_timeout(Some(left)).map_err(|error| Failure::of(&error, Some(timeout)))?;
        let read = match self {
            Stream::Plain(tcp) => tcp.read(into),
            Stream::Secure(stream) => stream.read(into),
        };
        match read {
            Ok(count) => Ok(count),
            // A server that hangs up without saying goodbye first has still said what it had. Only
            // an answer with no other way to say it's whole needs the goodbye.
            Err(error) if error.kind() == ErrorKind::UnexpectedEof => {
                *cut_off = true;
                Ok(0)
            }
            Err(error) => Err(failure_of(&error, timeout)),
        }
    }

    fn write_all_by(&mut self, data: &[u8], deadline: Instant, timeout: Duration) -> Result<(), Failure> {
        crate::stdlib::net::wrote_to_a_connection();
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Failure::TimedOut(timeout));
        }
        self.tcp().set_write_timeout(Some(left)).map_err(|error| Failure::of(&error, Some(timeout)))?;
        let wrote = match self {
            Stream::Plain(tcp) => tcp.write_all(data).and_then(|()| tcp.flush()),
            Stream::Secure(stream) => stream.write_all(data).and_then(|()| stream.flush()),
        };
        wrote.map_err(|error| failure_of(&error, timeout))
    }
}

/// What's read of an answer so far, and more when it's asked for.
struct Reader<'s> {
    stream: &'s mut Stream,
    held: Vec<u8>,
    at: usize,
    deadline: Instant,
    timeout: Duration,
    /// Whether TLS ended with no word from the other side that it was finished.
    cut_off: bool,
    /// How much of what's been read was heads and trailers, which together have a most.
    head_bytes: usize,
    /// How many bytes the last line took, its ending included.
    last_line: usize,
    /// How many bytes finding lines has looked at, each once, however slowly they come.
    looked_at: usize,
    /// The most the body can be.
    most_body: usize,
}

impl Reader<'_> {
    /// Reads more in. False once the other side is done.
    fn more(&mut self) -> Result<bool, Failure> {
        let mut chunk = [0; 16 * 1024];
        let read = self.stream.read_by(&mut chunk, (self.deadline, self.timeout), &mut self.cut_off)?;
        // What's been taken goes, once there's a good deal of it, so a long answer isn't all held.
        if self.at > 1 << 16 && self.at * 2 > self.held.len() {
            self.held.drain(..self.at);
            self.at = 0;
        }
        self.held.extend_from_slice(chunk.get(..read).unwrap_or_default());
        Ok(read > 0)
    }

    fn rest(&self) -> &[u8] {
        self.held.get(self.at..).unwrap_or_default()
    }

    /// The next line, without its ending. What's held is looked through from where the last look
    /// ended, so a line that comes a byte at a time is still looked at once.
    fn line(&mut self) -> Result<String, Failure> {
        let mut looked = 0;
        loop {
            let found = self.rest().get(looked..).unwrap_or_default().iter().position(|byte| *byte == b'\n');
            let now = self.rest().len();
            self.looked_at += found.map_or(now - looked.min(now), |at| at + 1);
            if let Some(at) = found {
                let end = looked + at;
                let line = self.rest().get(..end).unwrap_or_default();
                let line = String::from_utf8_lossy(line.strip_suffix(b"\r").unwrap_or(line)).into_owned();
                self.at += end + 1;
                self.last_line = end + 1;
                return Ok(line);
            }
            looked = now;
            if now > MOST_HEAD_LINE {
                let most = crate::embed::numbers::with_commas(MOST_HEAD_LINE);
                return Err(Failure::Other(format!("a line the server sent is longer than {most} bytes")));
            }
            if !self.more()? {
                return Err(Failure::Closed);
            }
        }
    }

    /// The next line of a head or trailer, counted against the most they may come to together.
    fn head_line(&mut self) -> Result<String, Failure> {
        let line = self.line()?;
        self.head_bytes += self.last_line;
        if self.head_bytes > MOST_IN_HEADERS {
            return Err(Failure::Other("the server sent too much before the response's body".to_owned()));
        }
        Ok(line)
    }

    /// The next `count` bytes.
    fn take(&mut self, count: usize) -> Result<Vec<u8>, Failure> {
        while self.rest().len() < count {
            if !self.more()? {
                return Err(Failure::Closed);
            }
        }
        let taken = self.rest().get(..count).unwrap_or_default().to_vec();
        self.at += count;
        Ok(taken)
    }

    /// Everything until the other side is done.
    fn all(&mut self) -> Result<Vec<u8>, Failure> {
        while self.more()? {
            if self.rest().len() > self.most_body {
                return Err(self.too_big());
            }
        }
        if self.cut_off {
            return Err(Failure::Tls("the connection was cut off before the answer ended".to_owned()));
        }
        let taken = self.rest().to_vec();
        self.at = self.held.len();
        Ok(taken)
    }

    /// A body longer than `most_body=` allows.
    fn too_big(&self) -> Failure {
        let most = crate::embed::numbers::with_commas(self.most_body);
        Failure::Other(format!(
            "the response's body is longer than {most} bytes: give `most_body=` a bigger number if it needs one"
        ))
    }

    fn chunked(&mut self) -> Result<Vec<u8>, Failure> {
        let mut body = Vec::new();
        loop {
            let line = self.line()?;
            let digits = line.split_once(';').map_or(line.as_str(), |(digits, _)| digits).trim();
            let size = usize::from_str_radix(digits, 16)
                .map_err(|_| Failure::Other(format!("`{digits}` isn't the size of a chunk")))?;
            if size == 0 {
                while !self.head_line()?.is_empty() {}
                return Ok(body);
            }
            if size > self.most_body.saturating_sub(body.len()) {
                return Err(self.too_big());
            }
            body.extend(self.take(size)?);
            if !self.line()?.is_empty() {
                return Err(Failure::Other("a chunk ran past the size it gave".to_owned()));
            }
        }
    }
}

fn read_answer(
    stream: &mut Stream,
    method: &str,
    (deadline, timeout): (Instant, Duration),
    most_body: usize,
) -> Result<Answer, Failure> {
    let mut reader = Reader {
        stream,
        held: Vec::new(),
        at: 0,
        deadline,
        timeout,
        cut_off: false,
        head_bytes: 0,
        last_line: 0,
        looked_at: 0,
        most_body,
    };
    let (status, headers) = loop {
        let (status, headers) = read_head(&mut reader)?;
        // `100 Continue` and the like say something's coming.
        if !(100..200).contains(&status) || status == 101 {
            break (status, headers);
        }
    };
    let header =
        |wanted: &str| headers.iter().find(|(name, _)| name == wanted).map(|(_, value)| value.as_str());
    let body = if method == "HEAD" || matches!(status, 204 | 304) || (100..200).contains(&status) {
        Vec::new()
    } else if header("transfer-encoding")
        .is_some_and(|coding| coding.to_ascii_lowercase().contains("chunked"))
    {
        reader.chunked()?
    } else if let Some(length) = header("content-length") {
        let size = length
            .trim()
            .parse()
            .map_err(|_| Failure::Other(format!("`{length}` isn't the length of a response")))?;
        // Refused before any of it's read, so a length that's too big makes no room for itself.
        if size > reader.most_body {
            return Err(reader.too_big());
        }
        reader.take(size)?
    } else {
        reader.all()?
    };
    Ok(Answer { status, headers, body })
}

fn read_head(reader: &mut Reader<'_>) -> Result<(i64, Vec<(String, String)>), Failure> {
    let status_line = reader.head_line()?;
    let status = status_line
        .strip_prefix("HTTP/")
        .and_then(|rest| rest.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| Failure::Other(format!("the server answered `{status_line}`, which isn't HTTP")))?;
    let mut headers = Vec::new();
    loop {
        let line = reader.head_line()?;
        if line.is_empty() {
            return Ok((status, headers));
        }
        if headers.len() == MOST_HEADERS {
            let most = crate::embed::numbers::with_commas(MOST_HEADERS);
            return Err(Failure::Other(format!("the server sent more than {most} headers")));
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
}

/// The failure an I/O error means, for a connection that may have TLS over it.
fn failure_of(error: &std::io::Error, timeout: Duration) -> Failure {
    match error.kind() {
        ErrorKind::InvalidData | ErrorKind::InvalidInput => Failure::Tls(error.to_string()),
        ErrorKind::UnexpectedEof => Failure::Tls("the other side hung up during the handshake".to_owned()),
        _ => Failure::of(error, Some(timeout)),
    }
}

/// TLS, which always checks certificates, with what the operating system trusts. The playground
/// has no network, so it has no TLS either.
pub(crate) mod tls {
    use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
    use std::net::TcpStream;
    use std::sync::{Arc, OnceLock};
    use std::time::{Duration, Instant};

    use rustls::pki_types::pem::PemObject as _;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
    use rustls::{ClientConfig, ClientConnection, ServerConfig, ServerConnection, StreamOwned};

    use super::super::net::{Failure, SecureStream, ServerSession, TlsServer, Transport};
    use super::{Secure, failure_of};

    impl Secure for StreamOwned<ClientConnection, TcpStream> {
        fn tcp(&self) -> &TcpStream {
            &self.sock
        }
    }

    /// How connections are made, worked out once: loading what's trusted takes a while.
    fn config() -> Result<Arc<ClientConfig>, Failure> {
        static CONFIG: OnceLock<Result<Arc<ClientConfig>, String>> = OnceLock::new();
        CONFIG
            .get_or_init(|| {
                let provider = Arc::new(rustls::crypto::ring::default_provider());
                let verifier = rustls_platform_verifier::Verifier::new(Arc::clone(&provider))
                    .map_err(|error| error.to_string())?;
                let config = ClientConfig::builder_with_provider(provider)
                    .with_safe_default_protocol_versions()
                    .map_err(|error| error.to_string())?
                    .dangerous()
                    .with_custom_certificate_verifier(Arc::new(verifier))
                    .with_no_client_auth();
                Ok(Arc::new(config))
            })
            .clone()
            .map_err(Failure::Tls)
    }

    /// A TLS connection read through a buffer. It's only ever seen as a `SecureStream`, so a
    /// program that never asks for TLS links none of this.
    struct Buffered<C>(BufReader<StreamOwned<C, TcpStream>>)
    where
        StreamOwned<C, TcpStream>: Read + Write;

    impl<C> Read for Buffered<C>
    where
        StreamOwned<C, TcpStream>: Read + Write,
    {
        fn read(&mut self, into: &mut [u8]) -> std::io::Result<usize> {
            self.0.read(into)
        }
    }

    impl<C> BufRead for Buffered<C>
    where
        StreamOwned<C, TcpStream>: Read + Write,
    {
        fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
            self.0.fill_buf()
        }

        fn consume(&mut self, count: usize) {
            self.0.consume(count);
        }
    }

    impl<C: Send> SecureStream for Buffered<C>
    where
        StreamOwned<C, TcpStream>: Read + Write,
    {
        fn socket(&self) -> &TcpStream {
            &self.0.get_ref().sock
        }
    }

    impl<C> Write for Buffered<C>
    where
        StreamOwned<C, TcpStream>: Read + Write,
    {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.0.get_mut().write(data)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.0.get_mut().flush()
        }
    }

    /// A raw connection with TLS over it, its handshake done against what the system trusts, in at
    /// most `timeout`. The socket's left blocking, with no timeouts of its own.
    pub(super) fn raw(
        tcp: TcpStream,
        host: &str,
        timeout: Duration,
    ) -> Result<Box<dyn SecureStream>, Failure> {
        handshake(tcp, host, timeout, config()?)
    }

    /// The server's TLS: what it'll prove itself with, for each connection it takes.
    struct Server(Arc<ServerConfig>);

    /// One connection's TLS on a server, driven over a socket that doesn't block.
    #[cfg_attr(
        not(any(test, feature = "threads")),
        expect(dead_code, reason = "only a served connection uses it, which needs threads")
    )]
    struct Session(ServerConnection);

    /// A server's TLS from its certificate chain and private key, as PEM text.
    pub(crate) fn server(chain: &str, key: &str) -> Result<Arc<dyn TlsServer>, Failure> {
        let unreadable = |what: &str| Failure::Tls(format!("the {what} isn't PEM that can be read"));
        let certificates = CertificateDer::pem_slice_iter(chain.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| unreadable("certificate chain"))?;
        if certificates.is_empty() {
            return Err(unreadable("certificate chain"));
        }
        let private = PrivateKeyDer::from_pem_slice(key.as_bytes()).map_err(|_| unreadable("private key"))?;
        let config = ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|error| Failure::Tls(error.to_string()))?
            .with_no_client_auth()
            .with_single_cert(certificates, private)
            .map_err(|error| Failure::Tls(error.to_string()))?;
        Ok(Arc::new(Server(Arc::new(config))))
    }

    impl TlsServer for Server {
        fn session(&self) -> Result<Box<dyn ServerSession>, Failure> {
            let connection = ServerConnection::new(Arc::clone(&self.0))
                .map_err(|error| Failure::Tls(error.to_string()))?;
            Ok(Box::new(Session(connection)))
        }

        fn accepted(&self, tcp: TcpStream, timeout: Duration) -> Result<Box<dyn SecureStream>, Failure> {
            let mut connection = ServerConnection::new(Arc::clone(&self.0))
                .map_err(|error| Failure::Tls(error.to_string()))?;
            let mut tcp = tcp;
            shake_hands(&mut connection, &mut tcp, timeout)?;
            Ok(Box::new(Buffered(BufReader::new(StreamOwned::new(connection, tcp)))))
        }
    }

    impl ServerSession for Session {
        fn handshake(&mut self, socket: &mut dyn Transport) -> std::io::Result<bool> {
            while self.0.is_handshaking() {
                while self.0.wants_write() {
                    self.0.write_tls(socket)?;
                }
                if !self.0.is_handshaking() {
                    break;
                }
                if self.0.read_tls(socket)? == 0 {
                    return Err(ErrorKind::UnexpectedEof.into());
                }
                self.0.process_new_packets().map_err(std::io::Error::other)?;
            }
            while self.0.wants_write() {
                self.0.write_tls(socket)?;
            }
            Ok(true)
        }

        fn read_onto(
            &mut self,
            socket: &mut dyn Transport,
            into: &mut Vec<u8>,
            count: usize,
        ) -> std::io::Result<usize> {
            let mut chunk = vec![0; count];
            loop {
                match self.0.reader().read(&mut chunk) {
                    Ok(read) => {
                        into.extend_from_slice(chunk.get(..read).unwrap_or_default());
                        return Ok(read);
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                    Err(error) => return Err(error),
                }
                // Alerts and handshake messages go out before it waits for more.
                while self.0.wants_write() {
                    match self.0.write_tls(socket) {
                        Ok(_) => {}
                        Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                        Err(error) => return Err(error),
                    }
                }
                if self.0.read_tls(socket)? == 0 {
                    return Ok(0);
                }
                self.0.process_new_packets().map_err(std::io::Error::other)?;
            }
        }

        fn write_some(&mut self, socket: &mut dyn Transport, data: &[u8]) -> std::io::Result<usize> {
            let taken = self.0.writer().write(data)?;
            while self.0.wants_write() {
                match self.0.write_tls(socket) {
                    Ok(_) => {}
                    // What was taken stays taken: it goes when the socket has room.
                    Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                    Err(error) => return Err(error),
                }
            }
            Ok(taken)
        }

        fn wants_write(&self) -> bool {
            self.0.wants_write()
        }
    }

    /// Drives a handshake on a blocking socket, in at most `timeout`, leaving it with no timeouts.
    fn shake_hands<C>(connection: &mut C, tcp: &mut TcpStream, timeout: Duration) -> Result<(), Failure>
    where
        C: std::ops::DerefMut<Target = rustls::ConnectionCommon<<C as Handshaking>::Data>> + Handshaking,
    {
        let deadline = Instant::now() + timeout;
        while connection.is_handshaking() {
            let left = deadline.saturating_duration_since(Instant::now()).max(Duration::from_millis(1));
            tcp.set_read_timeout(Some(left)).map_err(|error| Failure::of(&error, Some(timeout)))?;
            tcp.set_write_timeout(Some(left)).map_err(|error| Failure::of(&error, Some(timeout)))?;
            connection.complete_io(tcp).map_err(|error| failure_of(&error, timeout))?;
        }
        tcp.set_read_timeout(None).map_err(|error| Failure::of(&error, None))?;
        tcp.set_write_timeout(None).map_err(|error| Failure::of(&error, None))?;
        Ok(())
    }

    /// What a connection's handshake needs to know of it.
    trait Handshaking {
        type Data;
    }

    impl Handshaking for ClientConnection {
        type Data = rustls::client::ClientConnectionData;
    }

    impl Handshaking for ServerConnection {
        type Data = rustls::server::ServerConnectionData;
    }

    /// `raw`, trusting what `config` does: tests trust a certificate authority of their own.
    pub(crate) fn handshake(
        tcp: TcpStream,
        host: &str,
        timeout: Duration,
        config: Arc<ClientConfig>,
    ) -> Result<Box<dyn SecureStream>, Failure> {
        let name = ServerName::try_from(host.to_owned())
            .map_err(|_| Failure::Tls(format!("`{host}` can't be checked against a certificate")))?;
        let mut connection =
            ClientConnection::new(config, name).map_err(|error| Failure::Tls(error.to_string()))?;
        let mut tcp = tcp;
        shake_hands(&mut connection, &mut tcp, timeout)?;
        Ok(Box::new(Buffered(BufReader::new(StreamOwned::new(connection, tcp)))))
    }

    /// A TCP connection with TLS over it, with its handshake done.
    pub(super) fn wrap(
        tcp: TcpStream,
        host: &str,
        deadline: Instant,
        timeout: Duration,
    ) -> Result<Box<dyn Secure>, Failure> {
        let name = ServerName::try_from(host.to_owned())
            .map_err(|_| Failure::Tls(format!("`{host}` can't be checked against a certificate")))?;
        let mut connection =
            ClientConnection::new(config()?, name).map_err(|error| Failure::Tls(error.to_string()))?;
        let mut tcp = tcp;
        let left = deadline.saturating_duration_since(Instant::now());
        tcp.set_read_timeout(Some(left)).map_err(|error| Failure::of(&error, Some(timeout)))?;
        tcp.set_write_timeout(Some(left)).map_err(|error| Failure::of(&error, Some(timeout)))?;
        while connection.is_handshaking() {
            connection.complete_io(&mut tcp).map_err(|error| failure_of(&error, timeout))?;
        }
        Ok(Box::new(StreamOwned::new(connection, tcp)))
    }
}

#[cfg(test)]
mod tests {
    /// TLS that ends with what it had, then the other side gone without saying it's finished.
    struct CutOff {
        tcp: std::net::TcpStream,
        left: &'static [u8],
    }

    impl Read for CutOff {
        fn read(&mut self, into: &mut [u8]) -> std::io::Result<usize> {
            if self.left.is_empty() {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            let count = self.left.len().min(into.len());
            let (now, later) = self.left.split_at(count);
            into[..count].copy_from_slice(now);
            self.left = later;
            Ok(count)
        }
    }

    impl Write for CutOff {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            Ok(data.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl super::Secure for CutOff {
        fn tcp(&self) -> &std::net::TcpStream {
            &self.tcp
        }
    }

    /// An answer that lasts until the connection closes has only the closing to say it's whole, and
    /// over TLS a closing nobody said was coming may be someone cutting it short.
    #[test]
    fn an_answer_read_to_the_end_fails_if_tls_was_cut_off() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let tcp = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let mut stream = super::Stream::Secure(Box::new(CutOff { tcp, left: b"half of an answ" }));
        let timeout = Duration::from_secs(5);
        let mut reader = super::Reader {
            stream: &mut stream,
            held: Vec::new(),
            at: 0,
            deadline: std::time::Instant::now() + timeout,
            timeout,
            cut_off: false,
            head_bytes: 0,
            last_line: 0,
            looked_at: 0,
            most_body: super::MOST_BODY,
        };
        assert!(matches!(reader.all(), Err(Failure::Tls(_))));
    }

    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    /// A server's answer that comes one byte to each read, as a slow or hostile one sends it.
    struct Trickle {
        tcp: std::net::TcpStream,
        left: Vec<u8>,
        at: usize,
    }

    impl Read for Trickle {
        fn read(&mut self, into: &mut [u8]) -> std::io::Result<usize> {
            let (Some(byte), Some(first)) = (self.left.get(self.at), into.first_mut()) else { return Ok(0) };
            *first = *byte;
            self.at += 1;
            Ok(1)
        }
    }

    impl Write for Trickle {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            Ok(data.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl super::Secure for Trickle {
        fn tcp(&self) -> &std::net::TcpStream {
            &self.tcp
        }
    }

    /// A response's status and headers, or why they couldn't be read.
    type Head = Result<(i64, Vec<(String, String)>), Failure>;

    /// The head of `answer`, read a byte at a time, and how many bytes finding its lines looked at.
    fn head_read_slowly(answer: Vec<u8>) -> (Head, usize) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let tcp = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let mut stream = super::Stream::Secure(Box::new(Trickle { tcp, left: answer, at: 0 }));
        let timeout = Duration::from_secs(30);
        let mut reader = super::Reader {
            stream: &mut stream,
            held: Vec::new(),
            at: 0,
            deadline: std::time::Instant::now() + timeout,
            timeout,
            cut_off: false,
            head_bytes: 0,
            last_line: 0,
            looked_at: 0,
            most_body: super::MOST_BODY,
        };
        let head = super::read_head(&mut reader);
        (head, reader.looked_at)
    }

    /// A head that comes a byte at a time is looked through once, not once for each byte that
    /// came: the work is counted, so a slow machine can't make this pass or fail.
    #[test]
    fn a_head_sent_a_byte_at_a_time_is_looked_at_once() {
        let mut answer = b"HTTP/1.1 200 OK\r\n".to_vec();
        for n in 0..999 {
            answer.extend_from_slice(format!("x-header-{n}: {}\r\n", "v".repeat(40)).as_bytes());
        }
        answer.extend_from_slice(b"\r\n");
        let size = answer.len();
        let (head, looked_at) = head_read_slowly(answer);
        let (status, headers) = head.ok().unwrap();
        assert_eq!((status, headers.len()), (200, 999));
        // Each byte once. Looking from the start of the line on each byte, as before, looked at
        // about 30 times as many.
        assert_eq!(looked_at, size);
    }

    /// A head with more headers, or a longer line, than any real server sends is refused.
    #[test]
    fn a_head_past_its_limits_is_refused() {
        let head_of = |count: usize, line: usize| {
            let mut answer = b"HTTP/1.1 200 OK\r\n".to_vec();
            for n in 0..count {
                answer.extend_from_slice(format!("x-{n}: v\r\n").as_bytes());
            }
            answer.extend_from_slice(format!("x-long: {}\r\n\r\n", "v".repeat(line)).as_bytes());
            head_read_slowly(answer).0
        };
        assert_eq!(head_of(999, 10).ok().map(|(_, headers)| headers.len()), Some(1000));
        let Err(Failure::Other(many)) = head_of(1000, 10) else {
            panic!("a thousand and one headers were read")
        };
        assert_eq!(many, "the server sent more than 1,000 headers");
        assert!(head_of(0, 100 * 1024 - 20).is_ok());
        let Err(Failure::Other(long)) = head_of(0, 100 * 1024) else {
            panic!("a line past the most was read")
        };
        assert_eq!(long, "a line the server sent is longer than 102,400 bytes");
    }

    use super::super::net::Failure;
    use super::{Answer, Request, Url, parse_url, percent_encoded, redirected, send, with_params};

    /// A server on a port of its own that gives each connection the next of `answers`, and gives
    /// back the port and what it was asked, once it's answered them all.
    fn serving(answers: Vec<String>) -> (u16, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let done = std::thread::spawn(move || {
            let mut asked = Vec::new();
            for answer in answers {
                let (mut conn, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut chunk = [0; 4096];
                while !has_whole_request(&request) {
                    let read = conn.read(&mut chunk).unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(chunk.get(..read).unwrap());
                }
                asked.push(String::from_utf8_lossy(&request).into_owned());
                conn.write_all(answer.as_bytes()).unwrap();
            }
            asked
        });
        (port, done)
    }

    /// Whether a request's head has come, and as much of its body as its head says.
    fn has_whole_request(request: &[u8]) -> bool {
        let text = String::from_utf8_lossy(request);
        let Some((head, body)) = text.split_once("\r\n\r\n") else { return false };
        let length = head
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length: "))
            .map_or(0, |length| length.parse::<usize>().unwrap());
        body.len() >= length
    }

    fn get(url: String) -> Result<Answer, Failure> {
        send(&Request {
            method: "GET".to_owned(),
            url,
            params: Vec::new(),
            headers: Vec::new(),
            body: Vec::new(),
            timeout: Duration::from_secs(5),
            most_body: super::MOST_BODY,
        })
    }

    /// A server that answers one request with `head`, then sends `piece` again and again until
    /// the client hangs up.
    fn streaming_forever(head: &'static str, piece: &'static [u8]) -> (u16, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let done = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let _ = conn.read(&mut request);
            if conn.write_all(head.as_bytes()).is_err() {
                return;
            }
            while conn.write_all(piece).is_ok() {}
        });
        (port, done)
    }

    #[test]
    fn a_response_body_past_most_body_fails_however_it_comes() {
        let x = &[b'x'; 16 * 1024][..];
        let chunk: &'static [u8] =
            Box::leak(format!("4000\r\n{}\r\n", "x".repeat(0x4000)).into_bytes().into_boxed_slice());
        for (head, piece) in [
            // Read until the server hangs up, which it never does.
            ("HTTP/1.1 200 OK\r\n\r\n", x),
            // Chunks that never end.
            ("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n", chunk),
            // A length far past the most, refused before any of it's read.
            ("HTTP/1.1 200 OK\r\nContent-Length: 1099511627776\r\n\r\n", x),
        ] {
            let (port, done) = streaming_forever(head, piece);
            let failed = send(&Request {
                method: "GET".to_owned(),
                url: format!("http://127.0.0.1:{port}/"),
                params: Vec::new(),
                headers: Vec::new(),
                body: Vec::new(),
                timeout: Duration::from_secs(30),
                most_body: 1 << 20,
            });
            let Err(Failure::Other(reason)) = failed else {
                panic!("{head}: {:?}", failed.map(|answer| answer.body.len()));
            };
            assert_eq!(
                reason,
                "the response's body is longer than 1,048,576 bytes: give `most_body=` a bigger number if it needs one",
                "{head}"
            );
            done.join().unwrap();
        }
    }

    #[test]
    fn a_response_ends_by_its_length_by_its_chunks_or_when_the_server_hangs_up() {
        for answer in [
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nhe\r\n3;x=y\r\nllo\r\n0\r\nTrailer: z\r\n\r\n",
            "HTTP/1.1 200 OK\r\n\r\nhello",
            "HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello",
        ] {
            let (port, done) = serving(vec![answer.to_owned()]);
            let got = get(format!("http://127.0.0.1:{port}/a")).ok().unwrap();
            assert_eq!((got.status, got.body.as_slice()), (200, &b"hello"[..]), "{answer}");
            done.join().unwrap();
        }
    }

    #[test]
    fn an_error_status_is_an_answer_and_a_cut_off_body_is_a_failure() {
        let (port, done) = serving(vec![
            "HTTP/1.1 404 Not Found\r\nX-Why: GONE\r\nContent-Length: 2\r\n\r\nno".to_owned(),
            "HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nshort".to_owned(),
            "nonsense\r\n\r\n".to_owned(),
            "HTTP/1.1 204 No Content\r\nContent-Length: 9\r\n\r\n".to_owned(),
        ]);
        let url = format!("http://127.0.0.1:{port}/");
        let missing = get(url.clone()).ok().unwrap();
        assert_eq!(missing.status, 404);
        assert_eq!(
            missing.headers,
            [("x-why".to_owned(), "GONE".to_owned()), ("content-length".to_owned(), "2".to_owned())]
        );
        assert!(matches!(get(url.clone()), Err(Failure::Closed)));
        assert!(matches!(get(url.clone()), Err(Failure::Other(why)) if why.contains("isn't HTTP")));
        assert_eq!(get(url).ok().unwrap().status, 204);
        done.join().unwrap();
    }

    #[test]
    fn a_request_says_what_it_was_given() {
        let (port, done) = serving(vec!["HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_owned()]);
        let answer = send(&Request {
            method: "POST".to_owned(),
            url: format!("http://127.0.0.1:{port}/a?b=1"),
            params: vec![("q".to_owned(), "x y".to_owned())],
            headers: vec![
                ("X-Mine".to_owned(), "yes".to_owned()),
                ("Accept".to_owned(), "text/plain".to_owned()),
            ],
            body: b"data".to_vec(),
            timeout: Duration::from_secs(5),
            most_body: super::MOST_BODY,
        });
        assert_eq!(answer.ok().unwrap().status, 200);
        let asked = done.join().unwrap();
        let asked = asked.first().unwrap();
        assert!(asked.starts_with("POST /a?b=1&q=x%20y HTTP/1.1\r\n"), "{asked}");
        assert!(asked.contains(&format!("Host: 127.0.0.1:{port}\r\n")), "{asked}");
        assert!(asked.contains("X-Mine: yes\r\n"), "{asked}");
        assert!(asked.contains("Accept: text/plain\r\n") && !asked.contains("Accept: */*"), "{asked}");
        assert!(asked.contains("User-Agent: fennec\r\n"), "{asked}");
        assert!(asked.contains("Content-Length: 4\r\n"), "{asked}");
        assert!(asked.ends_with("\r\n\r\ndata"), "{asked}");
    }

    /// A header whose value would start a line of its own is refused before any connection is
    /// tried: nothing listens on port 1, so a refusal that came only from connecting would say
    /// so instead. A name that isn't one is refused the same way, and so is a control character.
    #[test]
    fn a_header_cant_smuggle_in_another_line() {
        let refused = |name: &str, value: &str| {
            let answer = send(&Request {
                method: "GET".to_owned(),
                url: "http://127.0.0.1:1/".to_owned(),
                params: Vec::new(),
                headers: vec![(name.to_owned(), value.to_owned())],
                body: Vec::new(),
                timeout: Duration::from_millis(200),
                most_body: super::MOST_BODY,
            });
            match answer {
                Err(Failure::Other(why)) => why,
                Err(other) => format!("{other:?}"),
                Ok(_) => "an answer".to_owned(),
            }
        };
        let said = "the header `X` has a line break or `:` in it";
        assert_eq!(refused("X", "a\r\nEvil: b"), said);
        assert_eq!(refused("X", "a\nEvil: b"), said);
        assert_eq!(refused("X", "a\u{7f}"), said);
        assert_eq!(refused("Evil: b\r\nX", "a"), "the header `Evil: b\\r\\nX` has a line break or `:` in it");
        // A good header gets as far as connecting, which fails on its own terms.
        assert!(!refused("X", "a").contains("header"));
    }

    #[test]
    fn a_redirect_is_followed_and_turns_a_post_into_a_get_unless_it_says_not_to() {
        let (port, done) = serving(vec![
            "HTTP/1.1 302 Found\r\nLocation: /b\r\nContent-Length: 0\r\n\r\n".to_owned(),
            "HTTP/1.1 307 Temporary\r\nLocation: c\r\nContent-Length: 0\r\n\r\n".to_owned(),
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_owned(),
        ]);
        let answer = send(&Request {
            method: "POST".to_owned(),
            url: format!("http://127.0.0.1:{port}/a"),
            params: Vec::new(),
            headers: vec![("Authorization".to_owned(), "secret".to_owned())],
            body: b"data".to_vec(),
            timeout: Duration::from_secs(5),
            most_body: super::MOST_BODY,
        });
        assert_eq!(answer.ok().unwrap().body, b"ok");
        let asked = done.join().unwrap();
        assert!(asked.first().unwrap().starts_with("POST /a "));
        assert!(asked.get(1).unwrap().starts_with("GET /b "));
        assert!(asked.get(2).unwrap().starts_with("GET /c "));
        // The same host keeps what says who's asking.
        assert!(asked.get(2).unwrap().contains("Authorization: secret"));
    }

    #[test]
    fn a_redirect_to_another_host_leaves_behind_what_says_who_is_asking() {
        let (there, there_done) = serving(vec!["HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_owned()]);
        let (port, done) = serving(vec![format!(
            "HTTP/1.1 301 Moved\r\nLocation: http://localhost:{there}/\r\nContent-Length: 0\r\n\r\n"
        )]);
        let answer = send(&Request {
            method: "GET".to_owned(),
            url: format!("http://127.0.0.1:{port}/"),
            params: Vec::new(),
            headers: vec![
                ("Authorization".to_owned(), "secret".to_owned()),
                ("User-Agent".to_owned(), "kept".to_owned()),
            ],
            body: Vec::new(),
            timeout: Duration::from_secs(5),
            most_body: super::MOST_BODY,
        });
        assert_eq!(answer.ok().unwrap().status, 200);
        done.join().unwrap();
        let asked = there_done.join().unwrap();
        assert!(!asked.first().unwrap().contains("secret"), "{asked:?}");
        assert!(asked.first().unwrap().contains("User-Agent: kept"), "{asked:?}");
    }

    /// Another port on the same host is another site, as far as who's asking goes.
    #[test]
    fn a_redirect_to_another_port_leaves_behind_every_header_but_the_harmless() {
        let (there, there_done) = serving(vec!["HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_owned()]);
        let (port, done) = serving(vec![format!(
            "HTTP/1.1 301 Moved\r\nLocation: http://127.0.0.1:{there}/\r\nContent-Length: 0\r\n\r\n"
        )]);
        let answer = send(&Request {
            method: "GET".to_owned(),
            url: format!("http://127.0.0.1:{port}/"),
            params: Vec::new(),
            headers: vec![
                ("Authorization".to_owned(), "secret".to_owned()),
                ("X-Api-Key".to_owned(), "also-secret".to_owned()),
                ("Accept".to_owned(), "text/plain".to_owned()),
            ],
            body: Vec::new(),
            timeout: Duration::from_secs(5),
            most_body: super::MOST_BODY,
        });
        assert_eq!(answer.ok().unwrap().status, 200);
        done.join().unwrap();
        let asked = there_done.join().unwrap();
        let request = asked.first().unwrap();
        assert!(!request.contains("secret"), "{asked:?}");
        assert!(request.contains("Accept: text/plain"), "{asked:?}");
    }

    /// A name and password in a URL are refused, and the message doesn't repeat them.
    #[test]
    fn a_url_with_a_password_in_it_is_not_repeated_in_the_failure() {
        for url in ["http://me:hunter2@example.com/", "ftp://me:hunter2@example.com/"] {
            let failed = parse_url(url).err().map(|failure| match failure {
                Failure::Other(why) => why,
                _ => String::new(),
            });
            let why = failed.unwrap();
            assert!(!why.contains("hunter2"), "{why}");
            assert!(why.contains("example.com"), "{why}");
        }
    }

    #[test]
    fn redirects_stop_after_ten() {
        let moved = "HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 0\r\n\r\n".to_owned();
        let (port, done) = serving(vec![moved; 11]);
        assert!(matches!(
            get(format!("http://127.0.0.1:{port}/")),
            Err(Failure::Other(why)) if why.contains("redirects more than 10 times")
        ));
        assert_eq!(done.join().unwrap().len(), 11);
    }

    /// What would start a line of its own on the wire is refused before anything's connected to:
    /// port 1 has nothing listening, so getting past the check says `Refused`.
    #[test]
    fn a_line_break_never_reaches_the_wire() {
        let asked = |method: &str, url: &str, headers: Vec<(String, String)>| {
            send(&Request {
                method: method.to_owned(),
                url: url.to_owned(),
                params: Vec::new(),
                headers,
                body: Vec::new(),
                timeout: Duration::from_secs(5),
                most_body: super::MOST_BODY,
            })
        };
        let plain = "http://127.0.0.1:1/";
        assert!(matches!(asked("GET", plain, Vec::new()), Err(Failure::Refused(..))));
        let injected = "http://127.0.0.1:1/a\r\nX-Injected: 1";
        assert!(matches!(asked("GET", injected, Vec::new()), Err(Failure::Other(_))), "the URL");
        assert!(matches!(asked("GET /x", plain, Vec::new()), Err(Failure::Other(_))), "the method");
        assert!(matches!(asked("GE\rT", plain, Vec::new()), Err(Failure::Other(_))), "the method");
    }

    #[test]
    fn nothing_coming_gives_up_at_the_timeout() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let answer = send(&Request {
            method: "GET".to_owned(),
            url: format!("http://127.0.0.1:{port}/"),
            params: Vec::new(),
            headers: Vec::new(),
            body: Vec::new(),
            timeout: Duration::from_millis(50),
            most_body: super::MOST_BODY,
        });
        assert!(matches!(answer, Err(Failure::TimedOut(after)) if after == Duration::from_millis(50)));
        drop(listener);
    }

    #[test]
    fn nothing_listening_is_refused_and_no_such_host_is_not_found() {
        assert!(
            matches!(get("http://127.0.0.1:1/".to_owned()), Err(Failure::Refused(host, 1)) if host == "127.0.0.1")
        );
        assert!(matches!(get("http://no-such-host.invalid/".to_owned()), Err(Failure::NotFound(_))));
    }

    /// TLS can't reach a server that doesn't speak it, and says so rather than sending anything.
    #[test]
    fn tls_with_a_server_that_doesnt_speak_it_fails_as_tls() {
        super::use_tls();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let done = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            conn.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").unwrap();
            // Held open until the client gives up, so it reads the answer rather than a reset,
            // which Windows reports first when a socket closes with the client's hello unread.
            let _ = conn.read_to_end(&mut Vec::new());
        });
        let answer = get(format!("https://localhost:{port}/"));
        done.join().unwrap();
        assert!(matches!(answer, Err(Failure::Tls(_))));
    }

    fn url(text: &str) -> Url {
        parse_url(text).ok().unwrap()
    }

    #[test]
    fn a_url_gives_its_parts() {
        let plain = Url { secure: false, host: "a.com".to_owned(), port: 80, target: "/".to_owned() };
        assert_eq!(url("http://a.com"), plain);
        assert_eq!(url("HTTP://a.com/"), plain);
        assert_eq!(url("http://a.com#top"), plain);
        assert_eq!(url("https://a.com/x/y?z=1#top").target, "/x/y?z=1");
        assert_eq!(url("https://a.com?z=1").target, "/?z=1");
        assert_eq!(url("https://a.com").port, 443);
        assert_eq!(url("http://a.com:8080/").port, 8080);
        assert_eq!(url("http://[::1]:8080/").host, "::1");
        assert_eq!(url("http://[::1]/").port, 80);
        for bad in
            ["a.com", "ftp://a.com", "http://", "http://a.com:port/", "http://u:p@a.com/", "http://[::1/"]
        {
            assert!(parse_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn params_go_on_the_end_written_as_a_url_holds_them() {
        assert_eq!(percent_encoded("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(percent_encoded("a b/é"), "a%20b%2F%C3%A9");
        let params = [("q".to_owned(), "a b".to_owned()), ("n".to_owned(), "1".to_owned())];
        assert_eq!(with_params("http://a.com/x", &params), "http://a.com/x?q=a%20b&n=1");
        assert_eq!(with_params("http://a.com/x?y=2", &params), "http://a.com/x?y=2&q=a%20b&n=1");
        assert_eq!(with_params("http://a.com/x", &[]), "http://a.com/x");
    }

    #[test]
    fn a_redirect_goes_from_where_the_request_was() {
        let from = url("https://a.com/x/y?z=1");
        assert_eq!(redirected(&from, "http://b.com/w"), "http://b.com/w");
        assert_eq!(redirected(&from, "/w"), "https://a.com:443/w");
        assert_eq!(redirected(&from, "w"), "https://a.com:443/x/w");
        assert_eq!(redirected(&url("http://[::1]:81/"), "/w"), "http://[::1]:81/w");
    }
}
