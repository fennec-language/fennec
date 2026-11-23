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

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::json_rules::{Json, parse};

/// The client the target builds: it asks the server on the port it's given three times, with
/// the timeout and the most a body may be that it's given, and prints what it read or the case it
/// failed with, each time.
pub(crate) const SCRIPTED_CLIENT: &str = "\
from std import json
from std.time import Duration

def asked(net: Net, url: str, timeout: Duration, most: int, stdout: Stdout):
    response = net.get(url, timeout=timeout, most_body=most) except why:
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
    for _ in range(3):
        asked(net, url, timeout, most, stdout)
";

/// How many times the client asks, each on a connection of its own.
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

/// A script: the client's timeout and most body, how the server sends and ends, where it splits
/// the answer, and the answer.
#[derive(Debug)]
struct Script {
    seconds: u64,
    small: bool,
    sent: Sent,
    ends: Ends,
    split: usize,
    answer: Vec<u8>,
}

/// The script an input says. Its first byte is flags, from its lowest bits:
/// - one for a timeout of 3 seconds rather than 1;
/// - one for a most body of 64 bytes rather than the default;
/// - two for how the answer's sent: whole (0 or 3), dripped (1) or split (2);
/// - two for how the server ends: it closes (0 or 3), stalls (1) or closes at the split (2).
///
/// Then two bytes, big-endian, say where the split is, and the rest is the answer.
fn script(input: &[u8]) -> Script {
    let flags = input.first().copied().unwrap_or(0);
    let high = usize::from(input.get(1).copied().unwrap_or(0));
    let low = usize::from(input.get(2).copied().unwrap_or(0));
    let answer = input.get(3..).unwrap_or_default().to_vec();
    Script {
        seconds: if flags & 1 == 1 { 3 } else { 1 },
        small: flags & 2 == 2,
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
        split: (high << 8 | low).min(answer.len()),
        answer,
    }
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

    /// What the client reads, or why it fails.
    fn answer(&mut self) -> Result<Outcome, Outcome> {
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
        Ok(Outcome::Read(status, body))
    }
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

/// What the client should make of the script's answer.
fn wanted(script: &Script) -> Outcome {
    let sent = if script.ends == Ends::Cut {
        script.answer.get(..script.split).unwrap_or_default()
    } else {
        &script.answer
    };
    let mut reading = Reading {
        bytes: sent,
        at: 0,
        stalls: script.ends == Ends::Stalls,
        most_body: if script.small { SMALL_BODY } else { MOST_BODY },
        head_bytes: 0,
    };
    reading.answer().unwrap_or_else(|outcome| outcome)
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

/// The server's side of one ask: it reads the request's head, sends the answer as the script
/// says, and ends as it says.
fn served(mut stream: TcpStream, script: &Script) {
    let waits = Duration::from_secs(script.seconds + 4);
    let _ = stream.set_read_timeout(Some(waits));
    let _ = stream.set_nodelay(true);
    let mut asked = Vec::new();
    let mut byte = [0];
    while !asked.ends_with(b"\r\n\r\n") && stream.read(&mut byte).is_ok_and(|read| read == 1) {
        asked.extend_from_slice(&byte);
    }
    let answer = &script.answer;
    let (first, rest) = answer.split_at(script.split);
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
            std::thread::sleep(PAUSE);
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

/// The target: what the client reads, three times, from a server answering as the script says,
/// against what the model says.
pub(crate) fn http_client_scripts(input: &[u8]) -> Result<(), String> {
    if crate::llvm::built_reader("http-client-scripts")?.is_none() {
        return Ok(());
    }
    let script = Arc::new(script(input));
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|err| err.to_string())?;
    let port = listener.local_addr().map_err(|err| err.to_string())?.port();
    let done = Arc::new(AtomicBool::new(false));
    let serving = {
        let (script, done) = (Arc::clone(&script), Arc::clone(&done));
        std::thread::spawn(move || {
            for _ in 0..ASKS {
                let Ok((stream, _)) = listener.accept() else { return };
                if done.load(Ordering::Relaxed) {
                    return;
                }
                served(stream, &script);
            }
        })
    };
    let started = Instant::now();
    let args = [
        port.to_string(),
        script.seconds.to_string(),
        if script.small { "small" } else { "default" }.to_owned(),
    ];
    let args: Vec<&std::ffi::OsStr> = args.iter().map(std::ffi::OsStr::new).collect();
    let printed = crate::llvm::run_reader("http-client-scripts", &args);
    let took = started.elapsed();
    // The server may still wait on an ask that never came, if the client stopped early: one
    // connection each lets it see it's done.
    done.store(true, Ordering::Relaxed);
    for _ in 0..ASKS {
        let _ = TcpStream::connect(("127.0.0.1", port));
    }
    let _ = serving.join();
    let shown = || format!("{script:?}");
    let Some(printed) = printed.map_err(|wrong| format!("{wrong}: {}", shown()))? else { return Ok(()) };
    let printed = String::from_utf8_lossy(&printed).into_owned();
    // However it ends, each ask keeps to its timeout. A timeout is a promise on the clock, so this
    // is checked on the clock, with room for a gate on a machine under heavy load: three times the
    // timeout and ten seconds more, on each ask. Past it, the client is said to be that late.
    let promised = Duration::from_secs(ASKS as u64 * script.seconds);
    let most = Duration::from_secs(ASKS as u64 * (3 * script.seconds + 10));
    if took > most {
        return Err(format!(
            "the client took {took:?} for {ASKS} asks with a timeout of {} s each, {:?} past the {promised:?} \
             they could take, and {:?} past the room for a busy machine: {}",
            script.seconds,
            took.saturating_sub(promised),
            took.saturating_sub(most),
            shown()
        ));
    }
    let lines: Vec<&str> = printed.lines().collect();
    if lines.len() != ASKS {
        return Err(format!("the client printed {printed:?}: {}", shown()));
    }
    let wanted = wanted(&script);
    for line in lines {
        let got = heard(line)?;
        if wanted != Outcome::Unpromised && got != wanted {
            return Err(format!("the client read {got:?}, not {wanted:?}: {}", shown()));
        }
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
    use super::{Ends, Outcome, Sent, http_client_scripts, script, wanted, written};

    const OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";

    fn outcome(flags: u8, split: u16, answer: &[u8]) -> Outcome {
        wanted(&script(&written(flags, split, answer)))
    }

    #[test]
    fn an_input_says_its_script() {
        let read = script(&written(0b0010_1011, 7, OK));
        assert_eq!(
            (read.seconds, read.small, read.sent, read.ends, read.split),
            (3, true, Sent::Split, Ends::Cut, 7)
        );
        assert_eq!(script(&written(0, 999, b"ab")).split, 2);
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

    /// The client asks a server answering each way, sent whole, dripped and split, and closing,
    /// stalling and cut, and reads what the model says each time, ending holding nothing.
    #[test]
    fn the_client_reads_what_the_model_says() {
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n";
        let cases: [(u8, u16, &[u8]); 8] = [
            (0b00_0000, 0, OK),
            (0b00_0100, 0, OK),
            (0b00_1000, 20, chunked),
            (0b10_0000, 40, OK),
            (0b01_0000, 0, b"HTTP/1.1 200 OK\r\n\r\nto the end"),
            (0b01_0000, 0, OK),
            (0b00_0000, 0, b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nab"),
            (0b00_0010, 0, b"HTTP/1.1 200 OK\r\n\r\nmore than sixty-four bytes of body, which most_body=64 refuses, as it must"),
        ];
        for (flags, split, answer) in cases {
            if let Err(wrong) = http_client_scripts(&written(flags, split, answer)) {
                unreachable!("{wrong}");
            }
        }
    }
}
