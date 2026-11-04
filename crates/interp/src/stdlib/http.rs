//! The HTTP client of `std.net`: HTTP/1.1 over a connection of its own for each request, with TLS
//! for `https`, which always checks certificates. Redirects are followed, up to 10. A 404 is an
//! answer, not a failure, since what it means is the caller's to say.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use super::net::Failure;

/// How many redirects a request follows.
const MOST_REDIRECTS: usize = 10;

/// The most a response's status line and headers can take, so a server can't send them forever.
const MOST_IN_HEADERS: usize = 1 << 20;

/// What's asked for.
pub(crate) struct Request {
    pub(crate) method: String,
    pub(crate) url: String,
    pub(crate) params: Vec<(String, String)>,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
    pub(crate) timeout: Duration,
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

fn parse_url(url: &str) -> Result<Url, Failure> {
    let bad = || Failure::Other(format!("`{url}` isn't a URL like `https://example.com/path`"));
    let (scheme, rest) = url.split_once("://").ok_or_else(bad)?;
    let secure = match scheme.to_ascii_lowercase().as_str() {
        "http" => false,
        "https" => true,
        _ => return Err(Failure::Other(format!("`{url}` isn't `http` or `https`"))),
    };
    let without_fragment = rest.split_once('#').map_or(rest, |(before, _)| before);
    let path_at = without_fragment.find(['/', '?']).unwrap_or(without_fragment.len());
    let (authority, target) = without_fragment.split_at(path_at);
    if authority.contains('@') {
        return Err(Failure::Other(format!("`{url}` has a name and password in it; use `headers=`")));
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
            out.push_str(&format!("%{byte:02X}"));
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
        // What says who's asking goes only to the host it was written for.
        let same_host = at.host == first.host;
        let headers = request
            .headers
            .iter()
            .filter(|(name, _)| same_host || !is_private(name))
            .map(|(name, value)| (name.as_str(), value.as_str()));
        let answer = once(&at, &method, headers, body, (deadline, request.timeout))?;
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

fn is_private(name: &str) -> bool {
    name.eq_ignore_ascii_case("authorization") || name.eq_ignore_ascii_case("cookie")
}

/// One request and its answer, on a connection of its own.
fn once<'h>(
    url: &Url,
    method: &str,
    headers: impl Iterator<Item = (&'h str, &'h str)>,
    body: &[u8],
    (deadline, timeout): (Instant, Duration),
) -> Result<Answer, Failure> {
    let tcp = connect(url, deadline, timeout)?;
    let mut stream =
        if url.secure { tls::wrap(tcp, &url.host, deadline, timeout)? } else { Stream::Plain(tcp) };
    let host = if matches!((url.secure, url.port), (false, 80) | (true, 443)) {
        url.host.clone()
    } else {
        format!("{}:{}", url.host, url.port)
    };
    let mut head = format!("{method} {} HTTP/1.1\r\nHost: {host}\r\n", url.target);
    let (mut agent, mut accept) = (false, false);
    for (name, value) in headers {
        if name.contains(['\r', '\n', ':']) || value.contains(['\r', '\n']) {
            return Err(Failure::Other(format!("the header `{name}` has a line break or `:` in it")));
        }
        agent |= name.eq_ignore_ascii_case("user-agent");
        accept |= name.eq_ignore_ascii_case("accept");
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    if !agent {
        head.push_str("User-Agent: fennec\r\n");
    }
    if !accept {
        head.push_str("Accept: */*\r\n");
    }
    if !body.is_empty() || matches!(method, "POST" | "PUT" | "PATCH") {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("Connection: close\r\n\r\n");
    stream.write_all_by(head.as_bytes(), deadline, timeout)?;
    stream.write_all_by(body, deadline, timeout)?;
    read_answer(&mut stream, method, deadline, timeout)
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
            Ok(stream) => return Ok(stream),
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

/// A connection a request goes over.
enum Stream {
    Plain(TcpStream),
    #[cfg(not(target_arch = "wasm32"))]
    Secure(Box<tls::Stream>),
}

impl Stream {
    fn tcp(&self) -> &TcpStream {
        match self {
            Stream::Plain(tcp) => tcp,
            #[cfg(not(target_arch = "wasm32"))]
            Stream::Secure(stream) => &stream.sock,
        }
    }

    /// Reads some, by the time left. Nothing read means the other side is done.
    fn read_by(&mut self, into: &mut [u8], deadline: Instant, timeout: Duration) -> Result<usize, Failure> {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Failure::TimedOut(timeout));
        }
        self.tcp().set_read_timeout(Some(left)).map_err(|error| Failure::of(&error, Some(timeout)))?;
        let read = match self {
            Stream::Plain(tcp) => tcp.read(into),
            #[cfg(not(target_arch = "wasm32"))]
            Stream::Secure(stream) => stream.read(into),
        };
        match read {
            Ok(count) => Ok(count),
            // A server that hangs up without saying goodbye first has still said what it had.
            Err(error) if error.kind() == ErrorKind::UnexpectedEof => Ok(0),
            Err(error) => Err(tls::failure_of(&error, timeout)),
        }
    }

    fn write_all_by(&mut self, data: &[u8], deadline: Instant, timeout: Duration) -> Result<(), Failure> {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Failure::TimedOut(timeout));
        }
        self.tcp().set_write_timeout(Some(left)).map_err(|error| Failure::of(&error, Some(timeout)))?;
        let wrote = match self {
            Stream::Plain(tcp) => tcp.write_all(data).and_then(|()| tcp.flush()),
            #[cfg(not(target_arch = "wasm32"))]
            Stream::Secure(stream) => stream.write_all(data).and_then(|()| stream.flush()),
        };
        wrote.map_err(|error| tls::failure_of(&error, timeout))
    }
}

/// What's read of an answer so far, and more when it's asked for.
struct Reader<'s> {
    stream: &'s mut Stream,
    held: Vec<u8>,
    at: usize,
    deadline: Instant,
    timeout: Duration,
}

impl Reader<'_> {
    /// Reads more in. False once the other side is done.
    fn more(&mut self) -> Result<bool, Failure> {
        let mut chunk = [0; 16 * 1024];
        let read = self.stream.read_by(&mut chunk, self.deadline, self.timeout)?;
        self.held.extend_from_slice(chunk.get(..read).unwrap_or_default());
        Ok(read > 0)
    }

    fn rest(&self) -> &[u8] {
        self.held.get(self.at..).unwrap_or_default()
    }

    /// The next line, without its ending.
    fn line(&mut self) -> Result<String, Failure> {
        loop {
            if let Some(end) = self.rest().iter().position(|byte| *byte == b'\n') {
                let line = self.rest().get(..end).unwrap_or_default();
                let line = String::from_utf8_lossy(line.strip_suffix(b"\r").unwrap_or(line)).into_owned();
                self.at += end + 1;
                return Ok(line);
            }
            if self.rest().len() > MOST_IN_HEADERS {
                return Err(Failure::Other("the server sent too much before the response's body".to_owned()));
            }
            if !self.more()? {
                return Err(Failure::Closed);
            }
        }
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
        while self.more()? {}
        let taken = self.rest().to_vec();
        self.at = self.held.len();
        Ok(taken)
    }

    fn chunked(&mut self) -> Result<Vec<u8>, Failure> {
        let mut body = Vec::new();
        loop {
            let line = self.line()?;
            let digits = line.split_once(';').map_or(line.as_str(), |(digits, _)| digits).trim();
            let size = usize::from_str_radix(digits, 16)
                .map_err(|_| Failure::Other(format!("`{digits}` isn't the size of a chunk")))?;
            if size == 0 {
                while !self.line()?.is_empty() {}
                return Ok(body);
            }
            body.extend(self.take(size)?);
            self.line()?;
        }
    }
}

fn read_answer(
    stream: &mut Stream,
    method: &str,
    deadline: Instant,
    timeout: Duration,
) -> Result<Answer, Failure> {
    let mut reader = Reader { stream, held: Vec::new(), at: 0, deadline, timeout };
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
        reader.take(size)?
    } else {
        reader.all()?
    };
    Ok(Answer { status, headers, body })
}

fn read_head(reader: &mut Reader<'_>) -> Result<(i64, Vec<(String, String)>), Failure> {
    let status_line = reader.line()?;
    let status = status_line
        .strip_prefix("HTTP/")
        .and_then(|rest| rest.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| Failure::Other(format!("the server answered `{status_line}`, which isn't HTTP")))?;
    let mut headers = Vec::new();
    loop {
        let line = reader.line()?;
        if line.is_empty() {
            return Ok((status, headers));
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
}

/// TLS, which always checks certificates, with what the operating system trusts.
#[cfg(not(target_arch = "wasm32"))]
mod tls {
    use std::io::ErrorKind;
    use std::net::TcpStream;
    use std::sync::{Arc, OnceLock};
    use std::time::{Duration, Instant};

    use rustls::pki_types::ServerName;
    use rustls::{ClientConfig, ClientConnection, StreamOwned};

    use super::super::net::Failure;

    pub(super) type Stream = StreamOwned<ClientConnection, TcpStream>;

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

    /// A TCP connection with TLS over it, with its handshake done.
    pub(super) fn wrap(
        tcp: TcpStream,
        host: &str,
        deadline: Instant,
        timeout: Duration,
    ) -> Result<super::Stream, Failure> {
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
        Ok(super::Stream::Secure(Box::new(StreamOwned::new(connection, tcp))))
    }

    /// The failure an I/O error means, for a connection that may have TLS over it.
    pub(super) fn failure_of(error: &std::io::Error, timeout: Duration) -> Failure {
        match error.kind() {
            ErrorKind::InvalidData | ErrorKind::InvalidInput => Failure::Tls(error.to_string()),
            ErrorKind::UnexpectedEof => {
                Failure::Tls("the other side hung up during the handshake".to_owned())
            }
            _ => Failure::of(error, Some(timeout)),
        }
    }
}

/// The playground has no network, so it has no TLS either.
#[cfg(target_arch = "wasm32")]
mod tls {
    use std::net::TcpStream;
    use std::time::{Duration, Instant};

    use super::super::net::Failure;

    pub(super) fn wrap(
        _tcp: TcpStream,
        _host: &str,
        _deadline: Instant,
        _timeout: Duration,
    ) -> Result<super::Stream, Failure> {
        Err(Failure::Other("the playground has no network".to_owned()))
    }

    pub(super) fn failure_of(error: &std::io::Error, timeout: Duration) -> Failure {
        Failure::of(error, Some(timeout))
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

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
        })
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

    #[test]
    fn a_header_cant_smuggle_in_another_line() {
        let answer = send(&Request {
            method: "GET".to_owned(),
            url: "http://127.0.0.1:1/".to_owned(),
            params: Vec::new(),
            headers: vec![("X".to_owned(), "a\r\nEvil: b".to_owned())],
            body: Vec::new(),
            timeout: Duration::from_millis(200),
        });
        assert!(answer.is_err());
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
                ("X-Other".to_owned(), "kept".to_owned()),
            ],
            body: Vec::new(),
            timeout: Duration::from_secs(5),
        });
        assert_eq!(answer.ok().unwrap().status, 200);
        done.join().unwrap();
        let asked = there_done.join().unwrap();
        assert!(!asked.first().unwrap().contains("secret"), "{asked:?}");
        assert!(asked.first().unwrap().contains("X-Other: kept"), "{asked:?}");
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
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let done = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            conn.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").unwrap();
        });
        let answer = get(format!("https://localhost:{port}/"));
        done.join().unwrap();
        assert!(matches!(answer, Err(Failure::Tls(_))));
    }
}
