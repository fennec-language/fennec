//! Fetching a release's files over HTTPS, checking certificates against what the operating system
//! trusts, as `std.net` does. What's fetched is trusted only once its signatures and hashes check,
//! so this only has to get the bytes. Plain HTTP is taken only from this machine, for tests.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

/// How long a connection may wait on the other side before it's given up on.
const WAIT: Duration = Duration::from_secs(30);

/// How many redirects a fetch follows: a release page sends its files elsewhere, once.
const MOST_REDIRECTS: usize = 5;

/// The most a fetch takes in: more than any release's archive.
const MOST_BYTES: u64 = 1 << 31;

/// The bytes at `url`.
///
/// # Errors
///
/// When they can't be fetched, saying why.
pub(crate) fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let mut url = url.to_owned();
    for _ in 0..=MOST_REDIRECTS {
        match get(&url)? {
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
    match get(url)? {
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

fn get(url: &str) -> Result<Got, String> {
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
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).map_err(failed)?;
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
        reader.read_line(&mut line).map_err(failed)?;
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
    let body = if chunked {
        chunks(&mut reader).map_err(failed)?
    } else {
        whole(&mut reader, length).map_err(failed)?
    };
    Ok(Got::Bytes(body))
}

fn line_status(status: u16) -> String {
    match status {
        404 => "404: there's no such file".to_owned(),
        _ => status.to_string(),
    }
}

fn whole(reader: &mut impl Read, length: Option<u64>) -> std::io::Result<Vec<u8>> {
    let mut body = Vec::new();
    reader.take(length.unwrap_or(MOST_BYTES).min(MOST_BYTES)).read_to_end(&mut body)?;
    if length.is_some_and(|length| u64::try_from(body.len()).ok() != Some(length)) {
        return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "the connection ended early"));
    }
    Ok(body)
}

fn chunks(reader: &mut impl BufRead) -> std::io::Result<Vec<u8>> {
    let bad = || std::io::Error::new(std::io::ErrorKind::InvalidData, "a chunk's size can't be read");
    let mut body = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let size = line
            .trim_end()
            .split(';')
            .next()
            .and_then(|size| u64::from_str_radix(size, 16).ok())
            .ok_or_else(bad)?;
        if size == 0 {
            return Ok(body);
        }
        if u64::try_from(body.len()).unwrap_or(u64::MAX).saturating_add(size) > MOST_BYTES {
            return Err(bad());
        }
        let before = body.len();
        reader.take(size).read_to_end(&mut body)?;
        if u64::try_from(body.len() - before).ok() != Some(size) {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "the connection ended early"));
        }
        line.clear();
        reader.read_line(&mut line)?;
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
    use super::parts;

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
