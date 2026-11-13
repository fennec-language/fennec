//! Fetching a release's files over HTTPS, checking certificates against what the operating system
//! trusts, as `std.net` does. What's fetched is trusted only once its signatures and hashes check,
//! so this only has to get the bytes. Plain HTTP is taken only from this machine, for tests.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a connection may wait on the other side before it's given up on.
const WAIT: Duration = Duration::from_secs(30);

/// How many redirects a fetch follows: a release page sends its files elsewhere, once.
const MOST_REDIRECTS: usize = 5;

/// The most a fetch takes in when nothing says how big what it fetches is: a release's manifest,
/// its signatures and its `latest`, each a few lines. An archive's size comes from its signed
/// manifest, and is all its fetch takes in.
const MOST_BYTES: u64 = 1 << 20;

/// The longest a line of the answer's head may be. A mirror that sends one without end would
/// otherwise have it read into memory until there's none left.
const LONGEST_LINE: u64 = 8 << 10;

/// How long a whole fetch may take, however it's sent: a mirror that sends a byte every few
/// seconds never trips the wait on each read, and is stopped here.
const WHOLE_FETCH: Duration = Duration::from_mins(15);

/// What one fetch may take in, and until when.
#[derive(Clone, Copy)]
struct Limits {
    most: u64,
    until: Instant,
}

/// The bytes at `url`.
///
/// # Errors
///
/// When they can't be fetched, saying why.
pub(crate) fn fetch(url: &str) -> Result<Vec<u8>, String> {
    fetch_at_most(url, MOST_BYTES)
}

/// The bytes at `url`, of which there are at most `most`, as a release's signed manifest says
/// how big each archive is. More is refused, before it's all read.
///
/// # Errors
///
/// When they can't be fetched, or there are more, saying why.
pub(crate) fn fetch_at_most(url: &str, most: u64) -> Result<Vec<u8>, String> {
    fetch_within(url, Limits { most, until: Instant::now() + WHOLE_FETCH })
}

fn fetch_within(url: &str, limits: Limits) -> Result<Vec<u8>, String> {
    let mut url = url.to_owned();
    for _ in 0..=MOST_REDIRECTS {
        match get(&url, limits)? {
            Got::Bytes(bytes) => return Ok(bytes),
            Got::Elsewhere(next) => url = next,
        }
    }
    Err(format!("{url} redirects more than {MOST_REDIRECTS} times"))
}

/// Where `url` redirects to, without following it.
///
/// # Errors
///
/// When it can't be asked, or doesn't redirect.
pub(crate) fn redirect(url: &str) -> Result<String, String> {
    match get(url, Limits { most: MOST_BYTES, until: Instant::now() + WHOLE_FETCH })? {
        Got::Elsewhere(next) => Ok(next),
        Got::Bytes(_) => Err(format!("{url} doesn't redirect")),
    }
}

enum Got {
    Bytes(Vec<u8>),
    Elsewhere(String),
}

/// Where a URL points: whether it's HTTPS, its host, its port and its path.
fn parts(url: &str) -> Result<(bool, String, u16, String), String> {
    let (secure, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (false, rest)
    } else {
        return Err(format!("{url} isn't an https:// address"));
    };
    let (authority, path) = rest.find('/').map_or((rest, "/"), |at| rest.split_at(at));
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => {
            (host, port.parse().map_err(|_| format!("{url} has no port that can be read"))?)
        }
        None => (authority, if secure { 443 } else { 80 }),
    };
    if host.is_empty() || authority.contains('@') {
        return Err(format!("{url} has no host that can be used"));
    }
    if !secure && !matches!(host, "127.0.0.1" | "localhost" | "[::1]") {
        return Err(format!("{url} isn't HTTPS, which a release is only fetched with from another machine"));
    }
    Ok((secure, host.to_owned(), port, path.to_owned()))
}

fn get(url: &str, limits: Limits) -> Result<Got, String> {
    let (secure, host, port, path) = parts(url)?;
    let failed = |err: std::io::Error| format!("couldn't fetch {url}: {err}");
    let tcp =
        TcpStream::connect((host.trim_start_matches('[').trim_end_matches(']'), port)).map_err(failed)?;
    tcp.set_read_timeout(Some(WAIT)).map_err(failed)?;
    tcp.set_write_timeout(Some(WAIT)).map_err(failed)?;
    let mut stream: Box<dyn ReadWrite> = if secure { Box::new(tls(tcp, &host)?) } else { Box::new(tcp) };
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: fennec-launcher/{}\r\nAccept: */*\r\nConnection: close\r\n\r\n",
        env!("CARGO_PKG_VERSION")
    );
    stream.write_all(request.as_bytes()).map_err(failed)?;
    let mut reader = BufReader::new(Until { inner: stream, until: limits.until });
    let mut line = String::new();
    read_line(&mut reader, &mut line).map_err(failed)?;
    let status: u16 = line
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or(format!("{url} didn't answer in HTTP"))?;
    let mut length: Option<u64> = None;
    let mut chunked = false;
    let mut location = None;
    loop {
        line.clear();
        read_line(&mut reader, &mut line).map_err(failed)?;
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        let Some((name, value)) = header.split_once(':') else { continue };
        let value = value.trim();
        match name.to_ascii_lowercase().as_str() {
            "content-length" => length = value.parse().ok(),
            "transfer-encoding" => chunked = value.eq_ignore_ascii_case("chunked"),
            "location" => location = Some(value.to_owned()),
            _ => {}
        }
    }
    if matches!(status, 301 | 302 | 303 | 307 | 308) {
        let location = location.ok_or(format!("{url} redirects nowhere"))?;
        // A redirect to a path stays on the same host.
        let next = if location.starts_with('/') {
            format!("{}://{host}:{port}{location}", if secure { "https" } else { "http" })
        } else {
            location
        };
        return Ok(Got::Elsewhere(next));
    }
    if status != 200 {
        return Err(format!("{url} answered {}", line_status(status)));
    }
    if length.is_some_and(|length| length > limits.most) {
        return Err(format!("{url} is bigger than the {} bytes it should be", limits.most));
    }
    let body = if chunked {
        chunks(&mut reader, limits.most).map_err(failed)?
    } else {
        whole(&mut reader, length, limits.most).map_err(failed)?
    };
    Ok(Got::Bytes(body))
}

fn line_status(status: u16) -> String {
    match status {
        404 => "404: there's no such file".to_owned(),
        _ => status.to_string(),
    }
}

fn whole(reader: &mut impl Read, length: Option<u64>, most: u64) -> std::io::Result<Vec<u8>> {
    let mut body = Vec::new();
    // One byte more than may come, to tell an answer that's too big from one that's just right.
    reader.take(length.unwrap_or(most).min(most).saturating_add(1)).read_to_end(&mut body)?;
    if u64::try_from(body.len()).unwrap_or(u64::MAX) > most {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("it's more than {most} bytes"),
        ));
    }
    if length.is_some_and(|length| u64::try_from(body.len()).ok() != Some(length)) {
        return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "the connection ended early"));
    }
    Ok(body)
}

/// A line of the answer, at most `LONGEST_LINE` long.
fn read_line(reader: &mut impl BufRead, line: &mut String) -> std::io::Result<()> {
    reader.take(LONGEST_LINE + 1).read_line(line)?;
    if u64::try_from(line.len()).unwrap_or(u64::MAX) > LONGEST_LINE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("a line of its answer is longer than {LONGEST_LINE} bytes"),
        ));
    }
    Ok(())
}

/// A connection read only until `until`: each read after it fails, so a whole fetch ends in time
/// however slowly what it fetches is sent.
struct Until<R> {
    inner: R,
    until: Instant,
}

impl<R: Read> Read for Until<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if Instant::now() > self.until {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "it's taken longer than a whole fetch may",
            ));
        }
        self.inner.read(buf)
    }
}

fn chunks(reader: &mut impl BufRead, most: u64) -> std::io::Result<Vec<u8>> {
    let bad = || std::io::Error::new(std::io::ErrorKind::InvalidData, "a chunk's size can't be read");
    let mut body = Vec::new();
    loop {
        let mut line = String::new();
        read_line(reader, &mut line)?;
        let size = line
            .trim_end()
            .split(';')
            .next()
            .and_then(|size| u64::from_str_radix(size, 16).ok())
            .ok_or_else(bad)?;
        if size == 0 {
            return Ok(body);
        }
        if u64::try_from(body.len()).unwrap_or(u64::MAX).saturating_add(size) > most {
            return Err(bad());
        }
        let before = body.len();
        reader.take(size).read_to_end(&mut body)?;
        if u64::try_from(body.len() - before).ok() != Some(size) {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "the connection ended early"));
        }
        line.clear();
        read_line(reader, &mut line)?;
    }
}

trait ReadWrite: Read + Write {}

impl<T: Read + Write> ReadWrite for T {}

/// A TCP connection with TLS over it, checking the other side's certificate.
fn tls(
    tcp: TcpStream,
    host: &str,
) -> Result<rustls::StreamOwned<rustls::ClientConnection, TcpStream>, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier =
        rustls_platform_verifier::Verifier::new(Arc::clone(&provider)).map_err(|err| err.to_string())?;
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|err| err.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(host.to_owned())
        .map_err(|_| format!("`{host}` can't be checked against a certificate"))?;
    let connection = rustls::ClientConnection::new(Arc::new(config), name).map_err(|err| err.to_string())?;
    Ok(rustls::StreamOwned::new(connection, tcp))
}

#[cfg(test)]
mod tests {
    use super::{Limits, fetch_at_most, fetch_within, parts};
    use std::io::{Read as _, Write as _};
    use std::time::{Duration, Instant};

    /// A mirror on this machine that answers each request as its path asks: a header line with no
    /// end, more than it should send, or a byte at a time.
    fn misbehaving() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                std::thread::spawn(move || {
                    let mut stream = stream;
                    let mut asked = [0; 512];
                    let read = stream.read(&mut asked).unwrap_or(0);
                    let asked = String::from_utf8_lossy(&asked[..read]).into_owned();
                    let _ = if asked.starts_with("GET /endless") {
                        stream
                            .write_all(b"HTTP/1.1 200 OK\r\nx-long: ")
                            .and_then(|()| stream.write_all(&vec![b'a'; 20_000]))
                    } else if asked.starts_with("GET /big") {
                        stream
                            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 5000\r\n\r\n")
                            .and_then(|()| stream.write_all(&[b'b'; 5000]))
                    } else {
                        stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 1000\r\n\r\n").and_then(|()| {
                            for _ in 0..1000 {
                                std::thread::sleep(Duration::from_millis(20));
                                stream.write_all(b"c")?;
                            }
                            Ok(())
                        })
                    };
                });
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    /// A mirror can't take the launcher's memory or time: a header line longer than 8 KiB is
    /// refused, an answer bigger than what was asked for is refused, and one sent so slowly that
    /// the whole fetch would take too long is stopped, though each byte comes in time.
    #[test]
    fn a_mirror_that_misbehaves_is_stopped() {
        let mirror = misbehaving();
        let endless = fetch_at_most(&format!("{mirror}/endless"), 1 << 20).unwrap_err();
        assert!(endless.contains("longer than 8192 bytes"), "{endless}");
        let big = fetch_at_most(&format!("{mirror}/big"), 100).unwrap_err();
        assert!(big.contains("bigger than the 100 bytes"), "{big}");
        let started = Instant::now();
        let limits = Limits { most: 1 << 20, until: Instant::now() + Duration::from_millis(300) };
        let slow = fetch_within(&format!("{mirror}/drip"), limits).unwrap_err();
        assert!(slow.contains("longer than a whole fetch may"), "{slow}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn only_https_reaches_another_machine() {
        assert_eq!(
            parts("https://github.com/a/b"),
            Ok((true, "github.com".to_owned(), 443, "/a/b".to_owned()))
        );
        assert_eq!(
            parts("http://127.0.0.1:8080/x"),
            Ok((false, "127.0.0.1".to_owned(), 8080, "/x".to_owned()))
        );
        assert!(parts("http://github.com/a").unwrap_err().contains("isn't HTTPS"));
        assert!(parts("ftp://github.com/a").is_err());
        assert!(parts("https://user@github.com/a").is_err());
        assert!(parts("https:///a").is_err());
    }
}
