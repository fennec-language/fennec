//! The `http-client-scripts` target: `std.net`'s HTTP client, built natively, asks a scripted
//! server here, which answers however its input says. The answer may come whole, a byte at a time
//! or in two pieces with a pause, and then the server closes, closes partway through it, or
//! stalls with the connection open past the client's timeout. The client asks three times in one
//! process. Each time, what it reads, or the `NetError` case it fails with, has to be what this
//! model of `std.net`'s client says, and it has to end holding nothing it made.
//!
//! The model follows the client's own rules (crates/rt/src/stdlib/http.rs):
//! - A status line is `HTTP/1.`, a digit, and a status from 100 to 599. Anything else is `Other`.
//! - A head is 1 MiB at most, 1,000 headers, and 100 KiB a line. Past any of those is `Other`.
//! - Two content-lengths that differ, or one that isn't digits, are `Other`.
//! - A body past `most_body=` is `Other`.
//! - Chunked is taken when it's the last coding, and a chunk whose size isn't hex digits, or that
//!   runs past its size, is `Other`.
//! - What's cut short is `Closed`, or `TimedOut` while the server stalls.
//!
//! An answer can redirect, to the next of up to four scripted servers, to itself, to a path on
//! the same one, or to the next by another of its names (`localhost`), which is another site. The
//! model follows it as the client does:
//! - up to 10 redirects, and past that `Other`;
//! - a 303, and a 301 or 302 to a `POST`, becomes a `GET` with no body;
//! - a narrowed `Net` refuses a host it doesn't reach with `Denied`, before asking it.
//!
//! Each server notes what it's asked, the host the request names among it, which has to be what the
//! model says too. The client sends a made-up key in a header, which may reach only the site it was
//! first sent to: the same host and port.
//!
//! The `http-client-splits` target asks the same servers many times, each splitting its answer
//! after a different byte each time, so the client reads a status line, a header, a length, a chunk
//! or a body cut wherever a network might cut it.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::json_rules::{Json, parse};

/// The client the targets build: it asks the server on the port it's given as many times as it's
/// told, with
/// the timeout, the most a body may be, the method and the hosts it's given, and a made-up key in a
/// header, and prints what it read or the case it failed with, each time.
pub(crate) const SCRIPTED_CLIENT: &str = "\
from std import json
from std.time import Duration

def asked(net: Net, method: str, url: str, timeout: Duration, most: int, stdout: Stdout):
    body = \"sent\".encode() if method == \"POST\" else bytes([])
    headers = {\"x-key\": \"made-up\"}
    response = net.request(method, url, body=body, headers=headers, timeout=timeout, most_body=most) except why:
        case .NotFound():
            stdout.print(\"err NotFound\")
            return
        case .Refused():
            stdout.print(\"err Refused\")
            return
        case .TimedOut():
            stdout.print(\"err TimedOut\")
            return
        case .Closed:
            stdout.print(\"err Closed\")
            return
        case .Denied(reason=reason):
            stdout.print(\"err Denied\", reason)
            return
        case .Tls(reason=reason):
            stdout.print(\"err Tls\", reason)
            return
        case .Status():
            stdout.print(\"err Status\")
            return
        case .Other(reason=reason):
            stdout.print(\"err Other\", reason)
            return
    stdout.print(\"ok\", response.status, json.dumps([b for b in response.body]))

def main(net: Net, args: Args, stdout: Stdout):
    url = f\"http://127.0.0.1:{args[0]}/asked\"
    timeout = 3s if args[1] == \"3\" else 1s
    most = 64 if args[2] == \"small\" else 67108864
    reach = net.hosts([\"127.0.0.1\"]) if args[4] == \"narrowed\" else net
    asks = int.parse(args[5]) except InvalidValue(): 3
    for _ in range(asks):
        asked(reach, method=args[3], url=url, timeout=timeout, most=most, stdout=stdout)
";

/// How many times the client asks for `http-client-scripts`, each on a connection of its own.
const ASKS: usize = 3;

/// The client's limits, as std.net has them.
const MOST_HEAD_LINE: usize = 100 * 1024;
const MOST_IN_HEADERS: usize = 1 << 20;
const MOST_HEADERS: usize = 1000;
const MOST_BODY: usize = 64 << 20;
/// The most the client reads at once: a line between `MOST_HEAD_LINE` and this much past it may or
/// may not be read whole, as it depends on how the bytes came.
const READ_AT_ONCE: usize = 16 * 1024;
/// `most_body=` when the script says it's small.
const SMALL_BODY: usize = 64;
/// How many redirects the client follows.
const MOST_REDIRECTS: usize = 10;

/// What goes between one server's answer and the next one's in an input, and the most servers.
const NEXT_SERVER: &[u8] = b"\0hop\0";
const MOST_SERVERS: usize = 4;

/// How the server sends its answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sent {
    Whole,
    /// Its first `DRIPPED` bytes one at a time, `DRIP` apart, then the rest.
    Dripped,
    /// Up to the split, then `PAUSE`, then the rest.
    Split,
}

const DRIPPED: usize = 32;
const DRIP: Duration = Duration::from_millis(2);
const PAUSE: Duration = Duration::from_millis(30);
/// The pause between an answer's pieces for `http-client-splits`, which asks many times.
const SPLIT_PAUSE: Duration = Duration::from_millis(1);

/// How the server ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ends {
    /// It closes once it's sent everything.
    Closes,
    /// It sends everything and keeps the connection open, saying nothing more, until the client
    /// goes or long past its timeout.
    Stalls,
    /// It sends up to the split, and closes.
    Cut,
}

/// A script: the client's timeout, most body, method and hosts; how each server sends and ends,
/// and where it splits its answer; and each server's answer.
#[derive(Debug)]
struct Script {
    seconds: u64,
    small: bool,
    posts: bool,
    narrowed: bool,
    sent: Sent,
    ends: Ends,
    split: usize,
    /// How long a split answer's pieces are apart.
    pause: Duration,
    answers: Vec<Vec<u8>>,
}

impl Script {
    /// How long a server waits while sending an answer, besides the time its bytes take.
    fn waits(&self) -> Duration {
        match self.sent {
            Sent::Whole => Duration::ZERO,
            Sent::Dripped => DRIP.saturating_mul(DRIPPED.try_into().unwrap_or(u32::MAX)),
            Sent::Split => self.pause,
        }
    }
}

/// The script an input says. Its first byte is flags, from its lowest bits:
/// - one for a timeout of 3 seconds rather than 1;
/// - one for a most body of 64 bytes rather than the default;
/// - two for how the answer's sent: whole (0 or 3), dripped (1) or split (2);
/// - two for how the server ends: it closes (0 or 3), stalls (1) or closes at the split (2);
/// - one for a `POST` with a body rather than a `GET`;
/// - one for a `Net` narrowed to `127.0.0.1`.
///
/// Then two bytes, big-endian, say where each answer is split, and the rest is the answers, the
/// first server's first, `NEXT_SERVER` between each and the next.
fn script(input: &[u8]) -> Script {
    let flags = input.first().copied().unwrap_or(0);
    let high = usize::from(input.get(1).copied().unwrap_or(0));
    let low = usize::from(input.get(2).copied().unwrap_or(0));
    let rest = input.get(3..).unwrap_or_default();
    let mut answers = Vec::new();
    let mut from = 0;
    while answers.len() + 1 < MOST_SERVERS
        && let Some(at) =
            rest.get(from..).and_then(|left| left.windows(NEXT_SERVER.len()).position(|w| w == NEXT_SERVER))
    {
        answers.push(rest.get(from..from + at).unwrap_or_default().to_vec());
        from += at + NEXT_SERVER.len();
    }
    answers.push(rest.get(from..).unwrap_or_default().to_vec());
    Script {
        seconds: if flags & 1 == 1 { 3 } else { 1 },
        small: flags & 2 == 2,
        posts: flags & 0x40 != 0,
        narrowed: flags & 0x80 != 0,
        sent: match flags >> 2 & 3 {
            1 => Sent::Dripped,
            2 => Sent::Split,
            _ => Sent::Whole,
        },
        ends: match flags >> 4 & 3 {
            1 => Ends::Stalls,
            2 => Ends::Cut,
            _ => Ends::Closes,
        },
        split: high << 8 | low,
        pause: PAUSE,
        answers,
    }
}

/// A server's answer with the places it names written in: `@next` for the next server, or the
/// first after the last; `@self` for itself; `@out` for the next by the name `localhost`, which is
/// another site and outside a `Net` narrowed to `127.0.0.1`; `@here` for a path on whichever
/// server was asked.
fn placed(answer: &[u8], at: usize, ports: &[u16]) -> Vec<u8> {
    let next = ports.get(at + 1).or(ports.first()).copied().unwrap_or(0);
    let own = ports.get(at).copied().unwrap_or(0);
    let names: [(&[u8], String); 4] = [
        (b"@next", format!("http://127.0.0.1:{next}/next")),
        (b"@self", format!("http://127.0.0.1:{own}/self")),
        (b"@out", format!("http://localhost:{next}/out")),
        (b"@here", "/here".to_owned()),
    ];
    let mut out = Vec::new();
    let mut from = 0;
    while from < answer.len() {
        let rest = answer.get(from..).unwrap_or_default();
        if let Some((name, place)) = names.iter().find(|(name, _)| rest.starts_with(name)) {
            out.extend_from_slice(place.as_bytes());
            from += name.len();
        } else {
            out.extend_from_slice(rest.get(..1).unwrap_or_default());
            from += 1;
        }
    }
    out
}

/// What the client should make of an answer.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Outcome {
    /// It reads this status and body.
    Read(i64, Vec<u8>),
    /// It fails with this `NetError` case.
    Fails(&'static str),
    /// Which, isn't promised.
    Unpromised,
}

/// The client reading an answer, as the model has it: the bytes the server sends, whether it then
/// stalls rather than closing, and the most a body may be.
struct Reading<'a> {
    bytes: &'a [u8],
    at: usize,
    stalls: bool,
    most_body: usize,
    head_bytes: usize,
}

impl Reading<'_> {
    /// What the client gets waiting for bytes that never come: the end of what was sent, or its
    /// timeout while the server stalls.
    fn ran_out(&self) -> Outcome {
        Outcome::Fails(if self.stalls { "TimedOut" } else { "Closed" })
    }

    fn rest(&self) -> &[u8] {
        self.bytes.get(self.at..).unwrap_or_default()
    }

    /// The next line, without its ending: a line the client gives up on before its end is `Other`.
    fn line(&mut self) -> Result<(String, usize), Outcome> {
        let rest = self.rest();
        let Some(end) = rest.iter().position(|byte| *byte == b'\n') else {
            return Err(if rest.len() > MOST_HEAD_LINE { Outcome::Fails("Other") } else { self.ran_out() });
        };
        if end > MOST_HEAD_LINE {
            return Err(if end >= MOST_HEAD_LINE + READ_AT_ONCE {
                Outcome::Fails("Other")
            } else {
                Outcome::Unpromised
            });
        }
        let line = rest.get(..end).unwrap_or_default();
        let line = String::from_utf8_lossy(line.strip_suffix(b"\r").unwrap_or(line)).into_owned();
        self.at += end + 1;
        Ok((line, end + 1))
    }

    /// The next line of a head or trailer, counted against the most they may be together.
    fn head_line(&mut self) -> Result<String, Outcome> {
        let (line, took) = self.line()?;
        self.head_bytes += took;
        if self.head_bytes > MOST_IN_HEADERS {
            return Err(Outcome::Fails("Other"));
        }
        Ok(line)
    }

    fn take(&mut self, count: usize) -> Result<Vec<u8>, Outcome> {
        let taken = self.rest().get(..count).ok_or_else(|| self.ran_out())?.to_vec();
        self.at += count;
        Ok(taken)
    }

    /// Everything to the end of what was sent.
    fn all(&self) -> Result<Vec<u8>, Outcome> {
        if self.rest().len() > self.most_body {
            return Err(Outcome::Fails("Other"));
        }
        if self.stalls {
            return Err(Outcome::Fails("TimedOut"));
        }
        Ok(self.rest().to_vec())
    }

    fn head(&mut self) -> Result<(i64, Vec<(String, String)>), Outcome> {
        let status = status_of(&self.head_line()?).ok_or(Outcome::Fails("Other"))?;
        let mut headers = Vec::new();
        loop {
            let line = self.head_line()?;
            if line.is_empty() {
                return Ok((status, headers));
            }
            if headers.len() == MOST_HEADERS {
                return Err(Outcome::Fails("Other"));
            }
            if let Some((name, value)) = line.split_once(':') {
                headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
            }
        }
    }

    fn chunked(&mut self) -> Result<Vec<u8>, Outcome> {
        let mut body = Vec::new();
        loop {
            let (line, _) = self.line()?;
            let digits = line.split_once(';').map_or(line.as_str(), |(digits, _)| digits).trim();
            let size = Some(digits)
                .filter(|digits| !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_hexdigit()))
                .and_then(|digits| usize::from_str_radix(digits, 16).ok())
                .ok_or(Outcome::Fails("Other"))?;
            if size == 0 {
                while !self.head_line()?.is_empty() {}
                return Ok(body);
            }
            if size > self.most_body.saturating_sub(body.len()) {
                return Err(Outcome::Fails("Other"));
            }
            body.extend(self.take(size)?);
            if !self.line()?.0.is_empty() {
                return Err(Outcome::Fails("Other"));
            }
        }
    }

    /// What the client reads, its headers too, or why it fails.
    fn answer(&mut self) -> Result<Answered, Outcome> {
        let (status, headers) = loop {
            let (status, headers) = self.head()?;
            if !(100..200).contains(&status) || status == 101 {
                break (status, headers);
            }
        };
        let header =
            |wanted: &str| headers.iter().find(|(name, _)| name == wanted).map(|(_, value)| value.as_str());
        let mut lengths =
            headers.iter().filter(|(name, _)| name == "content-length").map(|(_, value)| value.trim());
        if let Some(first) = lengths.next()
            && lengths.any(|other| other != first)
        {
            return Err(Outcome::Fails("Other"));
        }
        let body = if matches!(status, 204 | 304) || (100..200).contains(&status) {
            Vec::new()
        } else if let Some(coding) = header("transfer-encoding") {
            if coding.rsplit(',').next().is_some_and(|last| last.trim().eq_ignore_ascii_case("chunked")) {
                self.chunked()?
            } else {
                self.all()?
            }
        } else if let Some(length) = header("content-length") {
            let length = length.trim();
            let size: usize = Some(length)
                .filter(|length| !length.is_empty() && length.bytes().all(|byte| byte.is_ascii_digit()))
                .and_then(|length| length.parse().ok())
                .ok_or(Outcome::Fails("Other"))?;
            if size > self.most_body {
                return Err(Outcome::Fails("Other"));
            }
            self.take(size)?
        } else {
            self.all()?
        };
        Ok(Answered { status, headers, body })
    }
}

/// An answer as the client reads it.
struct Answered {
    status: i64,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// The status a status line gives, as the client reads it: `HTTP/1.` and a digit, then three
/// digits from 100 to 599, each after whitespace.
fn status_of(line: &str) -> Option<i64> {
    let mut words = line.split([' ', '\t', '\x0b', '\x0c']).filter(|word| !word.is_empty());
    let minor = words.next()?.strip_prefix("HTTP/1.")?;
    let code = words.next()?;
    let written = matches!(minor.as_bytes(), [digit] if digit.is_ascii_digit())
        && code.len() == 3
        && code.bytes().all(|byte| byte.is_ascii_digit());
    let status: i64 = code.parse().ok()?;
    (written && (100..=599).contains(&status)).then_some(status)
}

/// What a server is asked: by which of them, naming which host and port, with what method and body,
/// and whether the client's key came with it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Asked {
    server: usize,
    host: String,
    method: String,
    key: bool,
    body: Vec<u8>,
}

/// A URL's parts as the client reads them: whether it's `https`, its host, its port and what's
/// after them, or `None` where it isn't a URL the client takes.
fn url_parts(url: &str) -> Option<(bool, String, u16, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let secure = match scheme.to_ascii_lowercase().as_str() {
        "http" => false,
        "https" => true,
        _ => return None,
    };
    let without_fragment = rest.split_once('#').map_or(rest, |(before, _)| before);
    let path_at = without_fragment.find(['/', '?']).unwrap_or(without_fragment.len());
    let (authority, target) = without_fragment.split_at(path_at);
    if authority.contains('@') {
        return None;
    }
    let (host, port) = match authority.strip_prefix('[') {
        Some(after) => {
            let (host, rest) = after.split_once(']')?;
            (host, rest.strip_prefix(':'))
        }
        None => match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        },
    };
    if host.is_empty() {
        return None;
    }
    let port = match port {
        Some(port) => port.parse().ok()?,
        None if secure => 443,
        None => 80,
    };
    let target = if target.starts_with('/') { target.to_owned() } else { format!("/{target}") };
    Some((secure, host.to_owned(), port, target))
}

/// Where a redirect from `(host, port, target)` to `location` goes, as the client works it out.
fn redirected((host, port, target): (&str, u16, &str), location: &str) -> String {
    if location.contains("://") {
        return location.to_owned();
    }
    let base = format!("http://{host}:{port}");
    if location.starts_with('/') {
        return format!("{base}{location}");
    }
    let path = target.split_once('?').map_or(target, |(path, _)| path);
    let folder = path.rsplit_once('/').map_or("", |(folder, _)| folder);
    format!("{base}{folder}/{location}")
}

/// What the client should make of the script, given its servers' answers with their places
/// written in and their ports, and what it should ask each of them on the way.
fn wanted(script: &Script, answers: &[Vec<u8>], ports: &[u16]) -> (Outcome, Vec<Asked>) {
    let first = ports.first().copied().unwrap_or(0);
    let mut asked = Vec::new();
    let mut url = format!("http://127.0.0.1:{first}/asked");
    let (mut method, mut body) = if script.posts { ("POST", b"sent".to_vec()) } else { ("GET", Vec::new()) };
    for _ in 0..=MOST_REDIRECTS {
        let Some((secure, host, port, target)) = url_parts(&url) else {
            return (Outcome::Fails("Other"), asked);
        };
        let local = host.eq_ignore_ascii_case("127.0.0.1");
        if script.narrowed && !local {
            return (Outcome::Fails("Denied"), asked);
        }
        let server = ports.iter().position(|ours| *ours == port);
        let Some(server) = server.filter(|_| !secure && (local || host.eq_ignore_ascii_case("localhost")))
        else {
            return (Outcome::Unpromised, asked);
        };
        if !target.bytes().chain(host.bytes()).all(|byte| byte > 32 && byte != 127) {
            return (Outcome::Fails("Other"), asked);
        }
        // Drips and pauses add up over many servers asked, until the timeout may run out first.
        if asked.len() >= 4 && script.waits() >= PAUSE {
            return (Outcome::Unpromised, asked);
        }
        asked.push(Asked {
            server,
            host: format!("{host}:{port}"),
            method: method.to_owned(),
            key: local && port == first,
            body: body.clone(),
        });
        let answer = answers.get(server).map_or(&[][..], Vec::as_slice);
        let sent = if script.ends == Ends::Cut {
            answer.get(..script.split.min(answer.len())).unwrap_or_default()
        } else {
            answer
        };
        let mut reading = Reading {
            bytes: sent,
            at: 0,
            stalls: script.ends == Ends::Stalls,
            most_body: if script.small { SMALL_BODY } else { MOST_BODY },
            head_bytes: 0,
        };
        let Answered { status, headers, body: read } = match reading.answer() {
            Ok(answer) => answer,
            Err(outcome) => return (outcome, asked),
        };
        let location = headers.iter().find(|(name, _)| name == "location");
        let Some((_, location)) = location.filter(|_| matches!(status, 301 | 302 | 303 | 307 | 308)) else {
            return (Outcome::Read(status, read), asked);
        };
        url = redirected((&host, port, &target), location);
        if !matches!(status, 307 | 308) {
            method = "GET";
            body.clear();
        }
    }
    (Outcome::Fails("Other"), asked)
}

/// What the client printed for one ask, as an outcome.
fn heard(line: &str) -> Result<Outcome, String> {
    if let Some(rest) = line.strip_prefix("err ") {
        let case = rest.split(' ').next().unwrap_or_default();
        let known = ["NotFound", "Refused", "TimedOut", "Closed", "Denied", "Tls", "Status", "Other"];
        return known
            .iter()
            .find(|known| **known == case)
            .map(|case| Outcome::Fails(case))
            .ok_or_else(|| format!("the client printed {line:?}"));
    }
    let parts: Vec<&str> = line.splitn(3, ' ').collect();
    let [_, status, body] = parts.as_slice() else { return Err(format!("the client printed {line:?}")) };
    let status = status.parse().map_err(|_| format!("the client printed {line:?}"))?;
    let Ok(Json::Array(bytes)) = parse(body) else { return Err(format!("the client printed {line:?}")) };
    let body = bytes.iter().filter_map(|byte| if let Json::Number(n) = byte { n.parse().ok() } else { None });
    Ok(Outcome::Read(status, body.collect()))
}

/// One server's side of being asked: it reads the request, notes it, sends its answer as the
/// script says, split after `split` bytes, and ends as it says.
fn served(
    mut stream: TcpStream,
    script: &Script,
    (server, split): (usize, usize),
    answer: &[u8],
    noted: &Mutex<Vec<Asked>>,
) {
    let waits = Duration::from_secs(script.seconds + 4);
    let _ = stream.set_read_timeout(Some(waits));
    let _ = stream.set_nodelay(true);
    let mut head = Vec::new();
    let mut byte = [0];
    while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).is_ok_and(|read| read == 1) {
        head.extend_from_slice(&byte);
    }
    if head.is_empty() {
        return;
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let method = head.split(' ').next().unwrap_or_default().to_owned();
    let header = |wanted: &str| {
        head.lines().skip(1).find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim().eq_ignore_ascii_case(wanted).then(|| value.trim().to_owned())
        })
    };
    let length = header("content-length").and_then(|length| length.parse().ok()).unwrap_or(0);
    let mut body = vec![0; length];
    if stream.read_exact(&mut body).is_err() {
        body.clear();
    }
    let key = header("x-key").is_some();
    let host = header("host").unwrap_or_default();
    noted.lock().unwrap_or_else(PoisonError::into_inner).push(Asked { server, host, method, key, body });
    let split = split.min(answer.len());
    let (first, rest) = answer.split_at(split);
    let wrote = match (script.sent, script.ends) {
        (_, Ends::Cut) => stream.write_all(first),
        (Sent::Whole, _) => stream.write_all(answer),
        (Sent::Dripped, _) => {
            let (dripped, rest) = answer.split_at(DRIPPED.min(answer.len()));
            dripped
                .iter()
                .try_for_each(|byte| {
                    std::thread::sleep(DRIP);
                    stream.write_all(&[*byte])
                })
                .and_then(|()| stream.write_all(rest))
        }
        (Sent::Split, _) => stream.write_all(first).and_then(|()| {
            std::thread::sleep(script.pause);
            stream.write_all(rest)
        }),
    };
    if wrote.is_ok() && script.ends == Ends::Stalls {
        // Open and quiet until the client goes, which it does by its timeout at the latest.
        let mut left = [0; 1024];
        while stream.read(&mut left).is_ok_and(|read| read > 0) {}
    }
    let _ = stream.shutdown(Shutdown::Both);
}

/// What's wrong if the client took longer than its timeout lets it for its `asks`, however each
/// ended. A timeout is a promise on the clock, so this is checked on the clock, with room for a
/// gate on a machine under heavy load: three times the timeout and ten seconds more, on each ask.
/// Past it, the client is said to be that late.
fn on_time(took: Duration, script: &Script, asks: usize) -> Result<(), String> {
    let asks_now = u64::try_from(asks).unwrap_or(u64::MAX);
    let promised = Duration::from_secs(asks_now.saturating_mul(script.seconds));
    let most = Duration::from_secs(asks_now.saturating_mul(3 * script.seconds + 10));
    if took > most {
        return Err(format!(
            "the client took {took:?} for {asks} asks with a timeout of {} s each, {:?} past the {promised:?} \
             they could take, and {:?} past the room for a busy machine: {script:?}",
            script.seconds,
            took.saturating_sub(promised),
            took.saturating_sub(most),
        ));
    }
    Ok(())
}

/// About how long one connection to a scripted server takes for `http-client-splits`: the pause
/// between its answer's pieces, and as much again for the rest.
const CONNECTION_TAKES: Duration = Duration::from_millis(2);
/// How long the client may expect to spend asking for `http-client-splits` on one input.
const SPLITS_TAKE_AT_MOST: Duration = Duration::from_secs(3);
/// An answer this long or shorter is split after every byte, even where a longer one is split at a
/// few places to keep to `SPLITS_TAKE_AT_MOST`.
const SPLIT_EVERY_BYTE_UP_TO: usize = 64;

/// About how long one ask takes for `http-client-splits`, asking each server in `asked`. Asking for
/// `localhost` takes longer on Windows, which looks it up as `::1` first and refuses a port nothing
/// listens on there only slowly, so the client gives that address its short try and then asks
/// `127.0.0.1`.
fn ask_takes(asked: &[Asked]) -> Duration {
    let by_name =
        asked.iter().filter(|asked| asked.host.to_ascii_lowercase().starts_with("localhost:")).count();
    let by_name = if cfg!(windows) { u32::try_from(by_name).unwrap_or(u32::MAX) } else { 0 };
    let connections = u32::try_from(asked.len()).unwrap_or(u32::MAX);
    CONNECTION_TAKES
        .saturating_mul(connections)
        .saturating_add(Duration::from_millis(250).saturating_mul(by_name))
}

/// Where each server splits its answer on each connection for `http-client-splits`, round and round,
/// and how many times the client asks so each server meets every place it's given. That's after
/// every byte of its answer but the last, or, where asking for them all would take longer than
/// `SPLITS_TAKE_AT_MOST`, the few places `split_points` gives a long answer, as many of those as fit.
fn split_places(answers: &[Vec<u8>], asked: &[Asked]) -> (Vec<Vec<usize>>, usize) {
    let planned = |every_byte_up_to| {
        let places: Vec<Vec<usize>> = answers
            .iter()
            .map(|answer| crate::http_sessions::split_points(answer.len(), every_byte_up_to))
            .collect();
        let asks = places.iter().enumerate().map(|(server, places)| {
            let visits = asked.iter().filter(|asked| asked.server == server).count();
            if visits == 0 { 0 } else { places.len().div_ceil(visits) }
        });
        let asks = asks.max().unwrap_or(0).max(1);
        (places, asks)
    };
    let takes = ask_takes(asked).max(CONNECTION_TAKES);
    let fit = usize::try_from(SPLITS_TAKE_AT_MOST.as_micros() / takes.as_micros()).unwrap_or(1).max(1);
    let (places, asks) = planned(usize::MAX);
    if asks <= fit {
        return (places, asks);
    }
    let (places, asks) = planned(SPLIT_EVERY_BYTE_UP_TO);
    (places, asks.min(fit))
}

/// The `http-client-scripts` target: what the client reads, three times, from servers answering as
/// the script says, against what the model says, and what it asked each of them on the way.
pub(crate) fn http_client_scripts(input: &[u8]) -> Result<(), String> {
    met(script(input), false)
}

/// The `http-client-splits` target: an `http-client-scripts` input whose servers each split their
/// answer after a different byte on each connection, its two pieces a millisecond apart, and then
/// close, whatever its flags say of how they send and end. The client asks until each server has
/// split its answer at every place `split_places` gives it, and each time it has to read what the
/// model says the whole answer gets, and ask each server what the model says.
pub(crate) fn http_client_splits(input: &[u8]) -> Result<(), String> {
    met(Script { sent: Sent::Split, ends: Ends::Closes, pause: SPLIT_PAUSE, ..script(input) }, true)
}

/// What the scripted servers share: the script, each one's answer and the places it splits it at
/// on each connection, whether they're done, and what each was asked.
struct Shared {
    script: Script,
    answers: Vec<Vec<u8>>,
    places: Vec<Vec<usize>>,
    done: AtomicBool,
    noted: Mutex<Vec<Asked>>,
}

/// A server answering each connection as the script says until it's done. Where it has places to
/// split its answer at, each connection takes the next, round and round.
fn serving(listener: TcpListener, server: usize, shared: Arc<Shared>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let answer = shared.answers.get(server).map_or(&[][..], Vec::as_slice);
        let places = shared.places.get(server).map_or(&[][..], Vec::as_slice);
        for (nth, stream) in listener.incoming().enumerate() {
            if shared.done.load(Ordering::Relaxed) {
                return;
            }
            let Ok(stream) = stream else { return };
            let split = nth.checked_rem(places.len()).and_then(|at| places.get(at)).copied();
            served(
                stream,
                &shared.script,
                (server, split.unwrap_or(shared.script.split)),
                answer,
                &shared.noted,
            );
        }
    })
}

/// A server's listener on a port of its own, and the port.
fn listening() -> Result<(TcpListener, u16), String> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|err| err.to_string())?;
    let port = listener.local_addr().map_err(|err| err.to_string())?.port();
    Ok((listener, port))
}

/// What the client's told: the first server's port, the timeout, the most body, the method, the
/// hosts and how many times to ask.
fn client_args(script: &Script, port: u16, asks: usize) -> [String; 6] {
    [
        port.to_string(),
        script.seconds.to_string(),
        if script.small { "small" } else { "default" }.to_owned(),
        if script.posts { "POST" } else { "GET" }.to_owned(),
        if script.narrowed { "narrowed" } else { "open" }.to_owned(),
        asks.to_string(),
    ]
}

/// What the client reads from servers answering as `script` says, against what the model says,
/// and what it asked each of them on the way: three times, or, `splitting`, as many as it takes
/// each server to split its answer at each place `split_places` gives it.
fn met(script: Script, splitting: bool) -> Result<(), String> {
    if crate::llvm::built_reader("http-client-scripts")?.is_none() {
        return Ok(());
    }
    let listeners = script.answers.iter().map(|_| listening()).collect::<Result<Vec<_>, String>>()?;
    let ports: Vec<u16> = listeners.iter().map(|(_, port)| *port).collect();
    let answers: Vec<Vec<u8>> =
        script.answers.iter().enumerate().map(|(at, answer)| placed(answer, at, &ports)).collect();
    let (wanted, asked) = wanted(&script, &answers, &ports);
    let (places, asks) = if splitting {
        // Which outcome isn't promised, however it's split, and the scripted target meets it.
        if wanted == Outcome::Unpromised {
            return Ok(());
        }
        split_places(&answers, &asked)
    } else {
        (Vec::new(), ASKS)
    };
    let shared = Arc::new(Shared {
        script,
        answers,
        places,
        done: AtomicBool::new(false),
        noted: Mutex::new(Vec::new()),
    });
    let serving: Vec<_> = listeners
        .into_iter()
        .enumerate()
        .map(|(server, (listener, _))| serving(listener, server, Arc::clone(&shared)))
        .collect();
    let script = &shared.script;
    let started = Instant::now();
    let first = ports.first().copied().unwrap_or(0);
    let args = client_args(script, first, asks);
    let args: Vec<&std::ffi::OsStr> = args.iter().map(std::ffi::OsStr::new).collect();
    let printed = crate::llvm::run_reader("http-client-scripts", &args);
    let took = started.elapsed();
    // Each server waits on its next ask: one connection each lets it see it's done.
    shared.done.store(true, Ordering::Relaxed);
    for port in &ports {
        let _ = TcpStream::connect(("127.0.0.1", *port));
    }
    for server in serving {
        let _ = server.join();
    }
    let shown = || {
        let splits =
            if splitting { ", each server's answer split at its next place on each ask" } else { "" };
        format!("{script:?}{splits}")
    };
    let Some(printed) = printed.map_err(|wrong| format!("{wrong}: {}", shown()))? else { return Ok(()) };
    let printed = String::from_utf8_lossy(&printed).into_owned();
    on_time(took, script, asks)?;
    let lines: Vec<&str> = printed.lines().collect();
    if lines.len() != asks {
        return Err(format!("the client printed {printed:?}: {}", shown()));
    }
    if wanted == Outcome::Unpromised {
        return Ok(());
    }
    for (nth, line) in lines.into_iter().enumerate() {
        let got = heard(line)?;
        if got != wanted {
            return Err(format!(
                "the client read {got:?}, not {wanted:?}, on ask {} of {asks}: {}",
                nth + 1,
                shown()
            ));
        }
    }
    let noted = shared.noted.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let asked: Vec<Asked> = (0..asks).flat_map(|_| asked.iter().cloned()).collect();
    if noted != asked {
        return Err(format!("the servers were asked {noted:?}, not {asked:?}: {}", shown()));
    }
    Ok(())
}

/// An input for `script`.
#[cfg(test)]
fn written(flags: u8, split: u16, answer: &[u8]) -> Vec<u8> {
    [&[flags][..], &split.to_be_bytes(), answer].concat()
}

#[cfg(test)]
mod tests {
    use super::{
        Asked, Ends, NEXT_SERVER, Outcome, Sent, http_client_scripts, http_client_splits, placed, script,
        split_places, wanted, written,
    };

    const OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";

    /// The ports the model's tests give their servers.
    const PORTS: [u16; 4] = [4001, 4002, 4003, 4004];

    /// Held by a test running the client: both targets run the one program, which writes what it
    /// prints into one folder, so two at once would read each other's.
    static CLIENT: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// What the model says of a script, and what it asks each server.
    fn followed(flags: u8, split: u16, answer: &[u8]) -> (Outcome, Vec<Asked>) {
        let script = script(&written(flags, split, answer));
        let ports = PORTS.get(..script.answers.len()).unwrap_or_default();
        let answers: Vec<Vec<u8>> =
            script.answers.iter().enumerate().map(|(at, answer)| placed(answer, at, ports)).collect();
        wanted(&script, &answers, ports)
    }

    fn outcome(flags: u8, split: u16, answer: &[u8]) -> Outcome {
        followed(flags, split, answer).0
    }

    /// Servers' answers, as an input has them.
    fn servers(answers: &[&[u8]]) -> Vec<u8> {
        answers.join(NEXT_SERVER)
    }

    /// A redirect with this status to this place.
    fn redirect(status: u16, to: &str) -> Vec<u8> {
        format!("HTTP/1.1 {status} Moved\r\nLocation: {to}\r\nContent-Length: 0\r\n\r\n").into_bytes()
    }

    /// What a server is asked for `127.0.0.1`.
    fn asked(server: usize, method: &str, key: bool, body: &[u8]) -> Asked {
        let port = PORTS.get(server).copied().unwrap_or(0);
        Asked {
            server,
            host: format!("127.0.0.1:{port}"),
            method: method.to_owned(),
            key,
            body: body.to_vec(),
        }
    }

    /// What a server is asked for `localhost`.
    fn by_name(asked: Asked) -> Asked {
        let port = PORTS.get(asked.server).copied().unwrap_or(0);
        Asked { host: format!("localhost:{port}"), ..asked }
    }

    #[test]
    fn an_input_says_its_script() {
        let read = script(&written(0b1110_1011, 7, &servers(&[OK, b"a", b"b", b"c", b"d"])));
        assert_eq!(
            (read.seconds, read.small, read.sent, read.ends, read.split, read.posts, read.narrowed),
            (3, true, Sent::Split, Ends::Cut, 7, true, true)
        );
        // The fourth server's answer is all that's left.
        let answers: Vec<&[u8]> = read.answers.iter().map(Vec::as_slice).collect();
        assert_eq!(answers, [OK, b"a", b"b", &servers(&[b"c", b"d"])]);
        assert_eq!(
            placed(b"@next @self @out @here", 3, &PORTS),
            b"http://127.0.0.1:4001/next http://127.0.0.1:4004/self http://localhost:4001/out /here"
        );
    }

    /// The model says what the client's rules say for each way an answer can go wrong.
    #[test]
    fn the_model_gives_each_case() {
        let read = |status, body: &[u8]| Outcome::Read(status, body.to_vec());
        assert_eq!(outcome(0, 0, OK), read(200, b"hello"));
        // Cut in the body, and stalled there.
        assert_eq!(outcome(0b10_0000, 40, OK), Outcome::Fails("Closed"));
        assert_eq!(
            outcome(0b01_0000, 0, b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nhello"),
            Outcome::Fails("TimedOut")
        );
        // Stalled with the body whole: read, with no wait.
        assert_eq!(outcome(0b01_0000, 0, OK), read(200, b"hello"));
        // To the end, which a stall never reaches.
        assert_eq!(outcome(0, 0, b"HTTP/1.1 200 OK\r\n\r\nhello"), read(200, b"hello"));
        assert_eq!(outcome(0b01_0000, 0, b"HTTP/1.1 200 OK\r\n\r\nhello"), Outcome::Fails("TimedOut"));
        // Not a status line, two lengths, a length that isn't one, too big for `most_body=`.
        assert_eq!(outcome(0, 0, b"HTTP/2 200 OK\r\n\r\n"), Outcome::Fails("Other"));
        assert_eq!(
            outcome(0, 0, b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nab"),
            Outcome::Fails("Other")
        );
        assert_eq!(
            outcome(0, 0, b"HTTP/1.1 200 OK\r\nContent-Length: +1\r\n\r\nab"),
            Outcome::Fails("Other")
        );
        assert_eq!(
            outcome(0b10, 0, b"HTTP/1.1 200 OK\r\nContent-Length: 65\r\n\r\n"),
            Outcome::Fails("Other")
        );
        // Chunked, a bad size, a chunk past its size, a trailer.
        let chunked =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\nx-t: 1\r\n\r\n";
        assert_eq!(outcome(0, 0, chunked), read(200, b"hello"));
        assert_eq!(
            outcome(0, 0, b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nz\r\n"),
            Outcome::Fails("Other")
        );
        assert_eq!(
            outcome(0, 0, b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nab\r\n0\r\n\r\n"),
            Outcome::Fails("Other")
        );
        // An interim response first, and none with no body.
        assert_eq!(
            outcome(0, 0, &[b"HTTP/1.1 100 Continue\r\n\r\n".as_slice(), OK].concat()),
            read(200, b"hello")
        );
        assert_eq!(outcome(0b01_0000, 0, b"HTTP/1.1 204 No Content\r\n\r\n"), read(204, b""));
        // 1,000 headers are read; one more isn't.
        let headers = |count: usize| {
            format!("HTTP/1.1 200 OK\r\n{}Content-Length: 0\r\n\r\n", "x-a: b\r\n".repeat(count))
        };
        assert_eq!(outcome(0, 0, headers(999).as_bytes()), read(200, b""));
        assert_eq!(outcome(0, 0, headers(1000).as_bytes()), Outcome::Fails("Other"));
        // A line past 100 KiB is given up on, but near it, when isn't promised.
        let long = |length: usize| format!("HTTP/1.1 200 OK\r\nx-a: {}\r\n\r\n", "b".repeat(length));
        assert_eq!(outcome(0, 0, long(100 * 1024 - 10).as_bytes()), read(200, b""));
        assert_eq!(outcome(0, 0, long(100 * 1024 + 10).as_bytes()), Outcome::Unpromised);
        assert_eq!(outcome(0, 0, long(120 * 1024).as_bytes()), Outcome::Fails("Other"));
    }

    /// The model follows redirects as the client does: to the next server, to itself, to a path on
    /// the same one, and to another site, which doesn't get the key and is outside a narrowed `Net`.
    #[test]
    fn the_model_follows_redirects() {
        let read = |status, body: &[u8]| Outcome::Read(status, body.to_vec());
        // A 303 from a POST asks the next with a GET and no body, and the key stays behind.
        let (got, asked_on) = followed(0x40, 0, &servers(&[&redirect(303, "@next"), OK]));
        assert_eq!(got, read(200, b"hello"));
        assert_eq!(asked_on, [asked(0, "POST", true, b"sent"), asked(1, "GET", false, b"")]);
        // A 307 keeps the method and body; to itself, the key goes too.
        let (_, asked_on) = followed(0x40, 0, &servers(&[&redirect(307, "@self"), OK]));
        assert_eq!(
            asked_on.get(..2),
            Some(&[asked(0, "POST", true, b"sent"), asked(0, "POST", true, b"sent")][..])
        );
        // A path on the same server, then another site: open, the key stays behind, and narrowed,
        // it's refused before it's asked.
        let path_then_out = servers(&[&redirect(302, "@here"), OK]);
        let out = servers(&[&redirect(301, "@out"), OK]);
        assert_eq!(followed(0, 0, &path_then_out).1.len(), 11);
        assert_eq!(outcome(0, 0, &path_then_out), Outcome::Fails("Other"));
        assert_eq!(
            followed(0, 0, &out),
            (read(200, b"hello"), vec![asked(0, "GET", true, b""), by_name(asked(1, "GET", false, b""))])
        );
        assert_eq!(followed(0x80, 0, &out), (Outcome::Fails("Denied"), vec![asked(0, "GET", true, b"")]));
        // Ten redirects are followed, the eleventh isn't.
        let ten = servers(&[
            &redirect(302, "@next"),
            &redirect(302, "@next"),
            &redirect(302, "@next"),
            &redirect(302, "@next"),
        ]);
        assert_eq!(outcome(0, 0, &ten), Outcome::Fails("Other"));
        // A redirect with no `Location` is the answer.
        assert_eq!(outcome(0, 0, b"HTTP/1.1 302 Found\r\nContent-Length: 2\r\n\r\nhi"), read(302, b"hi"));
    }

    /// The client asks a server answering each way, sent whole, dripped and split, and closing,
    /// stalling and cut, and reads what the model says each time, ending holding nothing.
    #[test]
    fn the_client_reads_what_the_model_says() {
        let _client = CLIENT.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n";
        let cases: [(u8, u16, &[u8]); 7] = [
            (0b00_0000, 0, OK),
            (0b00_0100, 0, OK),
            (0b00_1000, 20, chunked),
            (0b10_0000, 40, OK),
            (0b01_0000, 0, b"HTTP/1.1 200 OK\r\n\r\nto the end"),
            (0b01_0000, 0, OK),
            (0b00_0000, 0, b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nab"),
        ];
        for (flags, split, answer) in cases {
            if let Err(wrong) = http_client_scripts(&written(flags, split, answer)) {
                unreachable!("{wrong}");
            }
        }
        let redirects = [
            (0x40, servers(&[&redirect(303, "@next"), OK])),
            (0x40, servers(&[&redirect(307, "@self"), OK])),
            (0x40, servers(&[&redirect(308, "@next"), &redirect(301, "@next"), OK])),
            (0x00, servers(&[&redirect(301, "@out"), OK])),
            (0x80, servers(&[&redirect(301, "@out"), OK])),
            (0x00, servers(&[&redirect(302, "@next"), &redirect(302, "@next")])),
            (0x00, servers(&[&redirect(302, "@here"), OK])),
        ];
        for (flags, answers) in redirects {
            if let Err(wrong) = http_client_scripts(&written(flags, 0, &answers)) {
                unreachable!("{wrong}");
            }
        }
        // A body past `most_body=` that comes with the head is refused too, however it comes.
        let long = b"HTTP/1.1 200 OK\r\n\r\nmore than sixty-four bytes of body, which most_body=64 refuses, as it must";
        for flags in [0b00_0010, 0b00_1010] {
            if let Err(wrong) = http_client_scripts(&written(flags, 19, long)) {
                unreachable!("{wrong}");
            }
        }
    }

    /// Each server is given every place its answer can be split, and the client asks often enough
    /// for each to meet them all: a server asked twice an ask meets two places an ask. An answer too
    /// long to split everywhere in time is split at a few places.
    #[test]
    fn each_server_meets_every_place_to_split() {
        let answers = vec![vec![b'a'; 5], vec![b'b'; 9]];
        let once = [asked(0, "GET", true, b""), asked(1, "GET", false, b"")];
        let (places, asks) = split_places(&answers, &once);
        assert_eq!(places, [vec![1, 2, 3, 4], (1..9).collect::<Vec<_>>()]);
        assert_eq!(asks, 8);
        let twice = [asked(0, "GET", true, b""), asked(1, "GET", false, b""), asked(1, "GET", false, b"")];
        assert_eq!(split_places(&answers, &twice).1, 4);
        // A server not asked takes no asks.
        assert_eq!(split_places(&answers, &once[..1]).1, 4);
        let long = vec![vec![b'c'; 100_000]];
        let (places, asks) = split_places(&long, &once[..1]);
        assert_eq!((places.first().map(Vec::len), asks), (Some(48), 48));
    }

    /// The client reads what the model says of an answer split after any of its bytes, a header,
    /// a length, a chunk or a body cut there, from one server and across redirects.
    #[test]
    fn the_client_reads_an_answer_split_anywhere() {
        let _client = CLIENT.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let chunked =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\nx-t: 1\r\n\r\n";
        let cases: [(u8, Vec<u8>); 5] = [
            (0b11_0000, OK.to_vec()),
            (0b01_0110, chunked.to_vec()),
            (0x40, servers(&[&redirect(303, "@next"), OK])),
            (0x00, servers(&[&redirect(301, "@out"), &redirect(307, "@next"), OK])),
            (0x00, b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nhello".to_vec()),
        ];
        for (flags, answers) in cases {
            if let Err(wrong) = http_client_splits(&written(flags, 0, &answers)) {
                unreachable!("{wrong}");
            }
        }
    }
}
