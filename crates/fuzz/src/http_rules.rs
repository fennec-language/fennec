//! The `http-server` target: `std.net`'s HTTP server, built natively, sent any bytes on one
//! connection. A server answering with what it was asked, as JSON, runs for each input; the input
//! is sent and the connection's sending side closed, and everything the server says is read.
//!
//! A reader here takes the same bytes apart as HTTP/1.1 says (RFC 9112), with the limits
//! `std.net` gives, and says what each request should get: what it asked for, echoed, or the
//! status that refuses it, after which the server closes. What it can't be sure of, like where a
//! limit's edge falls, or text that isn't UTF-8, it passes over. The server must never crash, get
//! stuck or hold far more memory than it was sent.
//!
//! The `http-client` target is the other way round: `std.net`'s client asks a server here, which
//! answers with the input. What it reads is held to what a reader here makes of the same bytes as
//! a response, where that's certain: its status and body, or failing.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use crate::json_rules::{Json, parse};

/// The server, which answers each request with what it was asked.
pub(crate) const SERVER: &str = "\
from std import json
from std.net import Request, Response

struct Echo:
    method: str
    path: str
    query: dict[str, str]
    headers: dict[str, str]
    body: list[int]

def respond(request: Request) -> Response:
    echo = Echo(
        method=request.method,
        path=request.path,
        query=request.query,
        headers=request.headers,
        body=[b for b in request.body],
    )
    return Response(status=200, body=json.dumps(echo).encode())

def main(net: Net, stdout: Stdout):
    with net.listen(0)? as server:
        stdout.print(server.port)
        server.serve(respond, timeout=5s)
";

/// The client, which asks the server on the port it's given and says what it got.
pub(crate) const CLIENT: &str = "\
from std import json

def main(net: Net, args: Args, stdout: Stdout):
    response = net.get(f\"http://127.0.0.1:{args[0]}/\", timeout=5s) except .NotFound() | .Refused() | .TimedOut() | .Closed | .Denied() | .Tls() | .Status() | .Other():
        stdout.print(\"err\")
        return
    stdout.print(\"ok\", response.status, json.dumps([b for b in response.body]))
";

/// The most a request's headers may hold, in lines and in bytes, and the most its body may be,
/// as `std.net` has them.
const MOST_HEADERS: usize = 100;
const MOST_HEADER_BYTES: usize = 16384;
const MOST_BODY: usize = 10_485_760;
/// The most blank lines `std.net` passes over before a request line.
const MOST_BLANK_LINES: usize = 8;

/// How long the server may take to answer and close.
const ANSWERED_WITHIN: Duration = Duration::from_secs(10);

/// What a request should get.
#[derive(Debug)]
enum Wanted {
    /// Its parts, echoed: the method, the path and query as read, the headers and the body.
    Echoed {
        method: String,
        path: String,
        query: Vec<(String, String)>,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    /// No body, since it's a `HEAD`.
    Head,
    /// Refused with this status, and the connection closed.
    Refused(u16),
}

/// Why the reader here stops.
enum Stop {
    /// The input's done, or asked the server to close: nothing more is answered.
    End,
    /// What comes next isn't promised anything.
    Unpromised,
    /// The server closed with input unread on the plain runtime, which resets the connection and
    /// can lose answers. Reported: the plain runtime's `close` doesn't linger as the threads one
    /// does. Take this out once it does.
    ClosedUnread,
}

/// A line of `input` from `at`, without its ending, and where the next starts, as the server's
/// connection reads lines: up to a `\n`, or what's left at the end, with a `\r` at its end taken off.
fn line(input: &[u8], at: usize) -> Option<(&[u8], usize)> {
    let rest = input.get(at..)?;
    if rest.is_empty() {
        return None;
    }
    match rest.iter().position(|byte| *byte == b'\n') {
        Some(end) => {
            let line = rest.get(..end)?;
            Some((line.strip_suffix(b"\r").unwrap_or(line), at + end + 1))
        }
        None => Some((rest.strip_suffix(b"\r").unwrap_or(rest), input.len())),
    }
}

/// A line of a request, as `line` gives it, or `Err` for one ended by a bare `\n`, which the
/// server refuses with a 400: only `\r\n` ends a line of a request, or the end of what was sent.
fn request_line(input: &[u8], at: usize) -> Option<Result<(&[u8], usize), ()>> {
    let (found, next) = line(input, at)?;
    let before = |back: usize| next.checked_sub(back).and_then(|at| input.get(at)).copied();
    // The `\r` has to be in the line, not the last byte of a body before it.
    let bare = before(1) == Some(b'\n') && (next < at + 2 || before(2) != Some(b'\r'));
    Some(if bare { Err(()) } else { Ok((found, next)) })
}

/// Whether text is what HTTP calls a token.
fn is_token(text: &[u8]) -> bool {
    !text.is_empty()
        && text.iter().all(|byte| byte.is_ascii_alphanumeric() || b"-_.!#$%&'*+^`|~".contains(byte))
}

fn has_control(text: &[u8], tab: bool) -> bool {
    text.iter().any(|byte| (*byte < 32 && !(tab && *byte == b'\t')) || *byte == 127)
}

/// `text` with `%` and two hex digits read as the byte they write, and `+` as a space when `plus`
/// says so, as `std.url` reads them: when the bytes read aren't UTF-8, the text as it was.
fn decoded(text: &str, plus: bool) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(byte) = bytes.get(at) {
        let hex = |at: usize| bytes.get(at).and_then(|byte| char::from(*byte).to_digit(16));
        if *byte == b'+' && plus {
            out.push(b' ');
            at += 1;
        } else if let (b'%', Some(high), Some(low)) = (*byte, hex(at + 1), hex(at + 2)) {
            out.push(u8::try_from(high * 16 + low).unwrap_or_default());
            at += 3;
        } else {
            out.push(*byte);
            at += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

/// A query's pairs as `std.url`'s `decode_query` reads them, a name given twice keeping its first.
fn query(text: &str) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = Vec::new();
    for pair in text.split('&').filter(|pair| !pair.is_empty()) {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        let (name, value) = (decoded(name, true), decoded(value, true));
        if !found.iter().any(|(seen, _)| *seen == name) {
            found.push((name, value));
        }
    }
    found
}

/// What each request in `input` should get, in order, and whether more could follow.
fn wanted(input: &[u8], resets: bool) -> (Vec<Wanted>, Stop) {
    // Reported: the runtime takes a `\r` off a line twice, so `\r\r\n` ends a line as `\r\n` does,
    // where a bare `\r` should make it invalid. Take this out once
    // `a_bare_carriage_return_isnt_taken_off_a_line` passes.
    if input.windows(3).any(|three| three == b"\r\r\n") {
        return (Vec::new(), Stop::Unpromised);
    }
    let closed = |at: usize| if resets && at < input.len() { Stop::ClosedUnread } else { Stop::End };
    let mut answers = Vec::new();
    let mut at = 0;
    loop {
        match request(input, &mut at) {
            Ok((answer, keep)) => {
                let refused = matches!(answer, Wanted::Refused(_));
                answers.push(answer);
                if refused || !keep {
                    return (answers, closed(at));
                }
            }
            Err(Stop::End) => return (answers, closed(at)),
            Err(stop) => return (answers, stop),
        }
    }
}

/// The next request from `at`, and whether the connection's kept open after it.
fn request(input: &[u8], at: &mut usize) -> Result<(Wanted, bool), Stop> {
    let refused = |status| Ok((Wanted::Refused(status), false));
    let Ok((first, next)) = request_line(input, *at).ok_or(Stop::End)? else { return refused(400) };
    *at = next;
    // A few blank lines before a request line are passed over, as HTTP/1.1 says.
    let mut first = first;
    let mut blank = 0;
    while first.is_empty() && blank < MOST_BLANK_LINES {
        blank += 1;
        let Ok((line, next)) = request_line(input, *at).ok_or(Stop::End)? else { return refused(400) };
        (first, *at) = (line, next);
    }
    if first.is_empty() {
        return Err(Stop::End);
    }
    let first = std::str::from_utf8(first).map_err(|_| Stop::Unpromised)?;
    let parts: Vec<&str> = first.split(' ').collect();
    let [method, target, version] = parts.as_slice() else { return refused(400) };
    if !["HTTP/1.1", "HTTP/1.0"].contains(version)
        || !is_token(method.as_bytes())
        || target.is_empty()
        || has_control(target.as_bytes(), false)
    {
        return refused(400);
    }
    let headers = match headers(input, at, first.chars().count()) {
        Ok(headers) => headers,
        Err(Early::Refused(status)) => return refused(status),
        Err(Early::Stop(stop)) => return Err(stop),
    };
    let header =
        |wanted: &str| headers.iter().find(|(name, _)| name == wanted).map(|(_, value)| value.as_str());
    if header("transfer-encoding").is_some() {
        return refused(501);
    }
    let mut body = Vec::new();
    if let Some(written) = header("content-length") {
        if written.is_empty() || !written.bytes().all(|byte| byte.is_ascii_digit()) {
            return refused(400);
        }
        let Ok(length) = written.parse::<usize>() else { return refused(400) };
        if length > MOST_BODY {
            return refused(413);
        }
        let Some(taken) = input.get(*at..*at + length) else { return refused(400) };
        body = taken.to_vec();
        *at += length;
    }
    let asked = header("connection").unwrap_or_default().to_lowercase();
    let keep = if *version == "HTTP/1.1" { asked != "close" } else { asked == "keep-alive" };
    if *method == "HEAD" {
        return Ok((Wanted::Head, keep));
    }
    let (path, query_text) = target.split_once('?').unwrap_or((target, ""));
    let answer = Wanted::Echoed {
        method: (*method).to_owned(),
        path: decoded(path, false),
        query: if target.contains('?') { query(query_text) } else { Vec::new() },
        headers,
        body,
    };
    Ok((answer, keep))
}

/// How reading headers ended early.
enum Early {
    Refused(u16),
    Stop(Stop),
}

/// The headers of a request from `at`, past the empty line that ends them, with `size` counted
/// from its first line.
fn headers(input: &[u8], at: &mut usize, size: usize) -> Result<Vec<(String, String)>, Early> {
    let mut headers: Vec<(String, String)> = Vec::new();
    let (mut size, mut lines) = (size, 0);
    loop {
        let Some(Ok((line, next))) = request_line(input, *at) else { return Err(Early::Refused(400)) };
        *at = next;
        if line.is_empty() {
            break;
        }
        let line = std::str::from_utf8(line).map_err(|_| Early::Stop(Stop::Unpromised))?;
        size += line.chars().count();
        lines += 1;
        // Near a limit, how it's counted decides, which isn't promised.
        if lines > MOST_HEADERS || size > MOST_HEADER_BYTES {
            return if lines > MOST_HEADERS + 1 || size > MOST_HEADER_BYTES + 4096 {
                Err(Early::Refused(431))
            } else {
                Err(Early::Stop(Stop::Unpromised))
            };
        }
        let bytes = line.as_bytes();
        if matches!(bytes.first(), Some(b' ' | b'\t')) || has_control(bytes, true) {
            return Err(Early::Refused(400));
        }
        let Some((name, value)) = line.split_once(':') else { return Err(Early::Refused(400)) };
        if !is_token(name.as_bytes()) {
            return Err(Early::Refused(400));
        }
        let name = name.to_ascii_lowercase();
        if ["content-length", "transfer-encoding", "host"].contains(&name.as_str())
            && headers.iter().any(|(seen, _)| *seen == name)
        {
            return Err(Early::Refused(400));
        }
        // `strip` takes off any whitespace, and only spaces and tabs are certain here.
        let trimmed = value.trim_matches([' ', '\t']);
        if trimmed.trim() != trimmed {
            return Err(Early::Stop(Stop::Unpromised));
        }
        headers.retain(|(seen, _)| *seen != name);
        headers.push((name, trimmed.to_owned()));
    }
    Ok(headers)
}

/// One response the server wrote: its status and its body.
struct Heard {
    status: u16,
    body: Vec<u8>,
}

/// The responses in what the server said, in order, up to `most`, or what's wrong with them.
fn heard(said: &[u8], heads: &[bool], most: usize, reset: bool) -> Result<Vec<Heard>, String> {
    let mut responses = Vec::new();
    let mut at = 0;
    while at < said.len() && responses.len() < most {
        let (status_line, mut next) = line(said, at).ok_or("a response ended early")?;
        let status_line = String::from_utf8_lossy(status_line);
        let status = status_line
            .strip_prefix("HTTP/1.1 ")
            .and_then(|rest| rest.get(..3))
            .and_then(|code| code.parse::<u16>().ok())
            .ok_or_else(|| format!("a response starts {status_line:?}"))?;
        let mut length = 0;
        loop {
            let (header, after) = line(said, next).ok_or("a response's headers didn't end")?;
            next = after;
            if header.is_empty() {
                break;
            }
            let header = String::from_utf8_lossy(header).to_lowercase();
            if let Some(written) = header.strip_prefix("content-length: ") {
                length = written.parse().map_err(|_| format!("a response's length is {written:?}"))?;
            }
        }
        // A `HEAD`'s answer says how long the body would be and doesn't send it.
        if heads.get(responses.len()).copied().unwrap_or(false) {
            length = 0;
        }
        // A reset can cut what came last short.
        let Some(body) = said.get(next..next + length) else {
            if reset {
                break;
            }
            return Err("a response's body was cut short".to_owned());
        };
        let body = body.to_vec();
        at = next + length;
        responses.push(Heard { status, body });
    }
    Ok(responses)
}

/// Whether an echo the server gave is what was wanted.
fn echoes(echo: &Json, wanted: &Wanted) -> bool {
    let Wanted::Echoed { method, path, query, headers, body } = wanted else { return false };
    let Json::Object(fields) = echo else { return false };
    let field = |name: &str| fields.iter().find(|(key, _)| key == name).map(|(_, value)| value);
    let text = |value: &str| Json::Text(value.to_owned());
    let pairs = |pairs: &[(String, String)]| {
        let mut sorted: Vec<(String, Json)> =
            pairs.iter().map(|(name, value)| (name.clone(), text(value))).collect();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        sorted
    };
    let sorted = |value: Option<&Json>| match value {
        Some(Json::Object(pairs)) => {
            let mut pairs = pairs.clone();
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            Some(pairs)
        }
        _ => None,
    };
    let bytes: Vec<Json> = body.iter().map(|byte| Json::Number(byte.to_string())).collect();
    field("method") == Some(&text(method))
        && field("path") == Some(&text(path))
        && sorted(field("query")) == Some(pairs(query))
        && sorted(field("headers")) == Some(pairs(headers))
        && field("body") == Some(&Json::Array(bytes))
}

/// A server started for one input, stopped when it's dropped.
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The target: what the server says to the input, against what HTTP/1.1 says it should.
pub(crate) fn http_server(input: &[u8]) -> Result<(), String> {
    let Some(executable) = crate::llvm::built_reader("http-server")? else { return Ok(()) };
    let mut server = Server(
        Command::new(&executable)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| format!("couldn't start the server: {err}"))?,
    );
    let mut first = String::new();
    if let Some(out) = server.0.stdout.take() {
        BufReader::new(out).read_line(&mut first).map_err(|err| err.to_string())?;
    }
    let port: u16 = first.trim().parse().map_err(|_| format!("the server said {first:?}, not its port"))?;
    let mut stream =
        TcpStream::connect(("127.0.0.1", port)).map_err(|err| format!("couldn't connect: {err}"))?;
    stream.set_read_timeout(Some(ANSWERED_WITHIN)).map_err(|err| err.to_string())?;
    // The server may close before it's read everything, which isn't wrong.
    let _ = stream.write_all(input);
    let _ = stream.shutdown(Shutdown::Write);
    let mut said = Vec::new();
    if let Err(err) = stream.read_to_end(&mut said)
        && matches!(err.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)
    {
        return Err(format!("the server neither answered nor closed within {ANSWERED_WITHIN:?}"));
    }
    if let Ok(Some(status)) = server.0.try_wait() {
        return Err(format!("the server stopped ({status}) on {input:?}"));
    }
    if crate::supervise::kilobytes(server.0.id()).is_some_and(|held| held > 512 * 1024) {
        return Err(format!("the server holds more than 512 MB after {} bytes", input.len()));
    }
    let resets = !crate::llvm::threads_runtime_here();
    let (wanted, stop) = wanted(input, resets);
    let heads: Vec<bool> = wanted.iter().map(|wanted| matches!(wanted, Wanted::Head)).collect();
    // Past what's unpromised, which may be a `HEAD`, a response's length can't be known.
    let most = if matches!(stop, Stop::Unpromised) { wanted.len() } else { usize::MAX };
    let responses = heard(&said, &heads, most, matches!(stop, Stop::ClosedUnread))
        .map_err(|wrong| format!("{wrong} in {:?}", String::from_utf8_lossy(&said)))?;
    let shown = || {
        format!("asked {:?}, it said {:?}", String::from_utf8_lossy(input), String::from_utf8_lossy(&said))
    };
    for (wanted, response) in wanted.iter().zip(&responses) {
        let right = match wanted {
            Wanted::Refused(status) => response.status == *status,
            Wanted::Head => response.status == 200,
            Wanted::Echoed { .. } => {
                response.status == 200
                    && parse(&String::from_utf8_lossy(&response.body)).is_ok_and(|echo| echoes(&echo, wanted))
            }
        };
        if !right {
            return Err(format!("wanted {wanted:?}: {}", shown()));
        }
    }
    let complete = match stop {
        Stop::End => responses.len() == wanted.len(),
        Stop::Unpromised => responses.len() >= wanted.len(),
        Stop::ClosedUnread => true,
    };
    if !complete {
        return Err(format!("wanted {} answers, got {}: {}", wanted.len(), responses.len(), shown()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Stop, Wanted, wanted};

    #[test]
    fn the_reader_here_takes_requests_apart_as_http_says() {
        let (answers, stop) = wanted(b"GET /a%20b?x=1+2&x=3 HTTP/1.1\r\nHost: h\r\nX-Y:  z \r\n\r\nPOST / HTTP/1.0\r\nContent-Length: 2\r\n\r\nhiGET", false);
        assert!(matches!(stop, Stop::End));
        let [Wanted::Echoed { path, query, headers, .. }, Wanted::Echoed { body, .. }] = answers.as_slice()
        else {
            assert_eq!(answers.len(), 2, "{answers:?}");
            return;
        };
        assert_eq!(path, "/a b");
        assert_eq!(query, &[("x".to_owned(), "1 2".to_owned())]);
        assert_eq!(headers, &[("host".to_owned(), "h".to_owned()), ("x-y".to_owned(), "z".to_owned())]);
        assert_eq!(body, b"hi");
        for (asked, status) in [
            (&b"GET / HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\nx"[..], 400),
            (b"GET / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n", 501),
            (b"GET / HTTP/1.1\r\n folded\r\n\r\n", 400),
            (b"GET  / HTTP/1.1\r\n\r\n", 400),
            (b"GET / HTTP/1.1\r\nHost : h\r\n\r\n", 400),
            (b"GET / HTTP/1.1\r\nContent-Length: +1\r\n\r\nx", 400),
            (b"GET / HTTP/1.1\nHost: a\r\n\r\n", 400),
            (b"\nGET / HTTP/1.1\r\n\r\n", 400),
        ] {
            let (answers, _) = wanted(asked, false);
            assert!(
                matches!(answers.as_slice(), [Wanted::Refused(found)] if *found == status),
                "{answers:?}"
            );
        }
    }
}

/// What a response should give the client: its status and body, or failing, or nothing certain.
#[derive(Debug, PartialEq, Eq)]
enum Answer {
    Read(i64, Vec<u8>),
    Fails,
    Unpromised,
}

/// A response line from `at`, which has to end in `\r\n`: a bare `\n` isn't certain.
fn crlf_line(input: &[u8], at: &mut usize) -> Result<Vec<u8>, Answer> {
    let rest = input.get(*at..).unwrap_or_default();
    let Some(end) = rest.iter().position(|byte| *byte == b'\n') else { return Err(Answer::Fails) };
    let line = rest.get(..end).unwrap_or_default();
    let line = line.strip_suffix(b"\r").ok_or(Answer::Unpromised)?;
    if line.contains(&b'\r') {
        return Err(Answer::Unpromised);
    }
    *at += end + 1;
    Ok(line.to_vec())
}

/// What the client should make of `input` as the answer to a `GET`, read as HTTP/1.1 says.
fn answer(input: &[u8]) -> Result<Answer, Answer> {
    let mut at = 0;
    let (status, headers) = loop {
        let line = crlf_line(input, &mut at)?;
        let line = String::from_utf8(line).map_err(|_| Answer::Unpromised)?;
        let parts: Vec<&str> = line.splitn(3, ' ').collect();
        let (Some(version), Some(code)) = (parts.first(), parts.get(1)) else {
            return Err(Answer::Unpromised);
        };
        if !version.starts_with("HTTP/1.")
            || code.len() != 3
            || !code.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(Answer::Unpromised);
        }
        let status: u16 = code.parse().map_err(|_| Answer::Unpromised)?;
        if !(100..=599).contains(&status) {
            return Err(Answer::Unpromised);
        }
        let mut headers: Vec<(String, String)> = Vec::new();
        loop {
            let line = crlf_line(input, &mut at)?;
            if line.is_empty() {
                break;
            }
            let line = String::from_utf8(line).map_err(|_| Answer::Unpromised)?;
            let (name, value) = line.split_once(':').ok_or(Answer::Unpromised)?;
            if !is_token(name.as_bytes()) {
                return Err(Answer::Unpromised);
            }
            headers.push((name.to_ascii_lowercase(), value.trim_matches([' ', '\t']).to_owned()));
        }
        if !(100..200).contains(&status) || status == 101 {
            break (status, headers);
        }
    };
    let all = |wanted: &str| -> Vec<String> {
        headers.iter().filter(|(name, _)| name == wanted).map(|(_, value)| value.clone()).collect()
    };
    if matches!(status, 204 | 304) || (100..200).contains(&status) {
        return Ok(Answer::Read(i64::from(status), Vec::new()));
    }
    let (codings, lengths) = (all("transfer-encoding"), all("content-length"));
    if !codings.is_empty() {
        if codings != ["chunked"] {
            return Err(Answer::Unpromised);
        }
        return chunked(input, at).map(|body| Answer::Read(i64::from(status), body));
    }
    match lengths.as_slice() {
        [] => Ok(Answer::Read(i64::from(status), input.get(at..).unwrap_or_default().to_vec())),
        [length] if !length.is_empty() && length.bytes().all(|byte| byte.is_ascii_digit()) => {
            let length: usize = length.parse().map_err(|_| Answer::Unpromised)?;
            input
                .get(at..at + length)
                .map(|body| Answer::Read(i64::from(status), body.to_vec()))
                .ok_or(Answer::Fails)
        }
        _ => Err(Answer::Unpromised),
    }
}

/// A chunked body from `at`.
fn chunked(input: &[u8], mut at: usize) -> Result<Vec<u8>, Answer> {
    let mut body = Vec::new();
    loop {
        let line = crlf_line(input, &mut at)?;
        let line = String::from_utf8(line).map_err(|_| Answer::Unpromised)?;
        let digits = line.split_once(';').map_or(line.as_str(), |(digits, _)| digits);
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Answer::Unpromised);
        }
        let size = usize::from_str_radix(digits, 16).map_err(|_| Answer::Unpromised)?;
        if size == 0 {
            while !crlf_line(input, &mut at)?.is_empty() {}
            return Ok(body);
        }
        body.extend_from_slice(input.get(at..at + size).ok_or(Answer::Fails)?);
        at += size;
        if !crlf_line(input, &mut at)?.is_empty() {
            return Err(Answer::Unpromised);
        }
    }
}

/// The target: what the client reads from a server answering with the input.
pub(crate) fn http_client(input: &[u8]) -> Result<(), String> {
    if crate::llvm::built_reader("http-client")?.is_none() {
        return Ok(());
    }
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).map_err(|err| err.to_string())?;
    let port = listener.local_addr().map_err(|err| err.to_string())?.port();
    let answer_with = input.to_vec();
    let serving = std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else { return };
        let _ = stream.set_read_timeout(Some(ANSWERED_WITHIN));
        // The request, up to the empty line that ends it.
        let mut asked = Vec::new();
        let mut byte = [0];
        while !asked.ends_with(b"\r\n\r\n") && stream.read(&mut byte).is_ok_and(|read| read == 1) {
            asked.extend_from_slice(&byte);
        }
        let _ = stream.write_all(&answer_with);
    });
    let printed = crate::llvm::run_reader("http-client", &[std::ffi::OsStr::new(&port.to_string())])?;
    let _ = serving.join();
    let Some(printed) = printed else { return Ok(()) };
    let printed = String::from_utf8_lossy(&printed).into_owned();
    let said = printed.trim_end();
    let got = if said == "err" {
        Answer::Fails
    } else {
        let parts: Vec<&str> = said.splitn(3, ' ').collect();
        let [_, status, body] = parts.as_slice() else {
            return Err(format!("the client printed {printed:?}"));
        };
        let status = status.parse().map_err(|_| format!("the client printed {printed:?}"))?;
        let Ok(Json::Array(bytes)) = parse(body) else {
            return Err(format!("the client printed {printed:?}"));
        };
        let body =
            bytes.iter().filter_map(|byte| if let Json::Number(n) = byte { n.parse().ok() } else { None });
        Answer::Read(status, body.collect())
    };
    let wanted = answer(input).unwrap_or_else(|stop| stop);
    if wanted != Answer::Unpromised && got != wanted {
        return Err(format!(
            "answered {:?}, the client read {got:?}, not {wanted:?}",
            String::from_utf8_lossy(input)
        ));
    }
    Ok(())
}
