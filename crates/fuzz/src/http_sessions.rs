//! The `http-sessions` target: one `std.net` HTTP server, built natively, met by a script of
//! clients one after another, as a server on the network is. Where `http-server` sends each input
//! whole on one connection to a server of its own, here each client sends its bytes in pieces,
//! with pauses between them, and then:
//! - closes its sending side and reads every answer;
//! - drops the connection, maybe in the middle of a request;
//! - or goes quiet without closing, until the server stops waiting for it.
//!
//! Each answer a client reads is held to what `http-server`'s reader says the same bytes should
//! get, however they were split. After the script, a last client asks for one plain request, which
//! the server has to answer: whatever came before, it's still up for the next one. The server runs
//! with one worker or four.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::time::{Duration, Instant};

use crate::http_rules::{judged, started};

/// How long a client waits for the server to answer and close: past the 2 seconds
/// `SESSIONS_SERVER` waits for a request to come whole, with room for a busy machine.
const ANSWERED_WITHIN: Duration = Duration::from_secs(10);

/// The most clients in a script, and the most pieces a client's bytes are sent in: past that, the
/// rest goes in one.
const MOST_CLIENTS: usize = 6;
const MOST_PIECES: usize = 32;

/// How long a client may have spent sending before what the server makes of it hangs on time:
/// past this, its wait for the request may run out first, so a missing answer isn't held against
/// it.
const SLOW_SEND: Duration = Duration::from_millis(1500);

/// How a client ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum End {
    /// It closes its sending side and reads every answer to the end.
    Reads,
    /// It drops the connection as soon as it's sent, reading nothing.
    Drops,
    /// It neither closes nor reads until the server lets it go.
    Quiet,
}

/// One client: its bytes in the pieces it sends them in, the pause before each piece but the
/// first, and how it ends.
#[derive(Debug)]
struct Client {
    pieces: Vec<Vec<u8>>,
    pause: Duration,
    end: End,
}

impl Client {
    fn bytes(&self) -> Vec<u8> {
        self.pieces.concat()
    }
}

/// A script: how many workers the server has, and its clients in order.
#[derive(Debug)]
struct Script {
    threads: usize,
    clients: Vec<Client>,
}

/// The script an input says. Its first byte picks the workers: one, or four with its lowest bit
/// set. Then each client is a byte saying how it goes, two bytes saying how many bytes it sends,
/// and those bytes: fewer where the input ends first. The byte saying how it goes is, from its
/// lowest bits:
/// - two bits for the end: `Reads` (0 or 3), `Drops` (1) or `Quiet` (2), at most one quiet client
///   to a script, as each takes the server's whole wait;
/// - two bits for the pieces: whole (0), a byte each (1), seven times the next two bits plus one
///   each (2), or two halves (3);
/// - two bits for the pause: none, 1, 4 or 10 ms.
fn script(input: &[u8]) -> Script {
    let threads = if input.first().is_some_and(|flags| flags & 1 == 1) { 4 } else { 1 };
    let mut clients = Vec::new();
    let mut at = 1;
    let mut quiet = false;
    while at < input.len() && clients.len() < MOST_CLIENTS {
        let how = input.get(at).copied().unwrap_or(0);
        let high = usize::from(input.get(at + 1).copied().unwrap_or(0));
        let low = usize::from(input.get(at + 2).copied().unwrap_or(0));
        let start = (at + 3).min(input.len());
        let stop = (start + (high << 8 | low)).min(input.len());
        let bytes = input.get(start..stop).unwrap_or_default();
        at = stop.max(at + 3);
        let mut end = match how & 3 {
            1 => End::Drops,
            2 => End::Quiet,
            _ => End::Reads,
        };
        if end == End::Quiet {
            if quiet {
                end = End::Reads;
            }
            quiet = true;
        }
        let size = match how >> 2 & 3 {
            0 => bytes.len().max(1),
            1 => 1,
            2 => 7 * (usize::from(how >> 4 & 3) + 1),
            _ => bytes.len().div_ceil(2).max(1),
        };
        let mut pieces: Vec<Vec<u8>> = bytes.chunks(size).map(<[u8]>::to_vec).collect();
        if pieces.len() > MOST_PIECES {
            let rest = pieces.split_off(MOST_PIECES - 1).concat();
            pieces.push(rest);
        }
        let pause = Duration::from_millis(match how >> 6 {
            0 => 0,
            1 => 1,
            2 => 4,
            _ => 10,
        });
        clients.push(Client { pieces, pause, end });
    }
    Script { threads, clients }
}

/// What a client heard, and whether it sent so slowly that what's missing hangs on time.
fn met(port: u16, client: &Client) -> Result<(Vec<u8>, bool), String> {
    let mut stream =
        TcpStream::connect(("127.0.0.1", port)).map_err(|err| format!("couldn't connect: {err}"))?;
    stream.set_read_timeout(Some(ANSWERED_WITHIN)).map_err(|err| err.to_string())?;
    let started = Instant::now();
    for (at, piece) in client.pieces.iter().enumerate() {
        if at > 0 && !client.pause.is_zero() {
            std::thread::sleep(client.pause);
        }
        // The server may close before it's read everything, which isn't wrong.
        if stream.write_all(piece).is_err() {
            break;
        }
    }
    let slow = started.elapsed() > SLOW_SEND;
    match client.end {
        End::Drops => return Ok((Vec::new(), slow)),
        End::Reads => {
            let _ = stream.shutdown(Shutdown::Write);
        }
        End::Quiet => {}
    }
    let mut said = Vec::new();
    if let Err(err) = stream.read_to_end(&mut said)
        && matches!(err.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)
    {
        let waited = if client.end == End::Quiet { " after its wait for a request ran out" } else { "" };
        return Err(format!("the server neither answered nor closed within {ANSWERED_WITHIN:?}{waited}"));
    }
    Ok((said, slow))
}

/// What a last client, asking for one plain request, has to get: an answer, whatever came before.
const CANARY: &[u8] = b"GET /still-up HTTP/1.1\r\nHost: canary\r\nConnection: close\r\n\r\n";

/// The target: a script of clients against one server, each heard as HTTP/1.1 says, and the
/// server still up after them.
pub(crate) fn http_sessions(input: &[u8]) -> Result<(), String> {
    let Some(executable) = crate::llvm::built_reader("http-sessions")? else { return Ok(()) };
    let script = script(input);
    let (mut server, port) = started(&executable, Some(script.threads))?;
    for (at, client) in script.clients.iter().enumerate() {
        let shown = || format!("client {at} of {:?} at {} workers", script.clients, script.threads);
        let (said, slow) = met(port, client).map_err(|wrong| format!("{wrong}: {}", shown()))?;
        if let Ok(Some(status)) = server.0.try_wait() {
            return Err(format!("the server stopped ({status}): {}", shown()));
        }
        if client.end != End::Drops {
            // Sent slowly, the last request may run out of time before it's whole.
            let cut = client.end == End::Quiet || slow;
            judged(&client.bytes(), &said, cut).map_err(|wrong| format!("{wrong}: {}", shown()))?;
        }
    }
    let canary = Client { pieces: vec![CANARY.to_vec()], pause: Duration::ZERO, end: End::Reads };
    let shown = || format!("after {:?} at {} workers", script.clients, script.threads);
    let (said, _) = met(port, &canary).map_err(|wrong| format!("the last client: {wrong}, {}", shown()))?;
    if !said.starts_with(b"HTTP/1.1 200 ") {
        return Err(format!("the last client heard {:?}, {}", String::from_utf8_lossy(&said), shown()));
    }
    if crate::supervise::kilobytes(server.0.id()).is_some_and(|held| held > 512 * 1024) {
        return Err(format!("the server holds more than 512 MB, {}", shown()));
    }
    Ok(())
}

/// An input for `script`: the workers' flag, then each client as how it goes and its bytes.
#[cfg(test)]
fn written(threads: u8, clients: &[(u8, &[u8])]) -> Vec<u8> {
    let mut input = vec![threads];
    for (how, bytes) in clients {
        let length = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
        input.push(*how);
        input.extend(length.to_be_bytes());
        input.extend(*bytes);
    }
    input
}

#[cfg(test)]
mod tests {
    use super::{End, MOST_PIECES, http_sessions, script, written};

    const GET: &[u8] = b"GET /a?b=c HTTP/1.1\r\nHost: h\r\n\r\n";
    const POST: &[u8] = b"POST /p HTTP/1.1\r\nHost: h\r\nContent-Length: 5\r\n\r\nhello";

    #[test]
    fn an_input_says_its_clients() {
        let input = written(1, &[(0b0000_0100, GET), (0b1000_1101, POST), (2, GET), (2, GET)]);
        let read = script(&input);
        assert_eq!(read.threads, 4);
        let ends: Vec<End> = read.clients.iter().map(|client| client.end).collect();
        // A second quiet client reads.
        assert_eq!(ends, [End::Reads, End::Drops, End::Quiet, End::Reads]);
        assert_eq!(read.clients.first().map(|client| client.pieces.len()), Some(MOST_PIECES));
        assert_eq!(read.clients.get(1).map(|client| client.pieces.len()), Some(2));
        assert!(read.clients.iter().all(|client| client.bytes() == GET || client.bytes() == POST));
        // An input cut short gives what it has.
        let short = script(&input[..5]);
        assert_eq!(short.clients.len(), 1);
        assert_eq!(short.clients.first().map(super::Client::bytes), Some(GET[..1].to_vec()));
    }

    /// Requests sent a byte at a time, halved, dropped half sent, and left unfinished by a client
    /// that goes quiet, at one worker and four, are each heard as HTTP/1.1 says, and the server
    /// answers the next client after them.
    #[test]
    fn a_server_hears_every_client_as_http_says_and_stays_up() {
        let pipelined = [GET, POST, GET].concat();
        let refused =
            b"GET / HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nGET / HTTP/1.1\r\n\r\n";
        for threads in [0, 1] {
            let input = written(
                threads,
                &[
                    (0b0100_0100, &pipelined),
                    (0b0000_1100, POST),
                    (0b0000_0101, &POST[..30]),
                    (0b0000_1000, refused),
                    (0b0000_0010, &[GET, &POST[..40]].concat()),
                    (0b1100_1000, &pipelined),
                ],
            );
            if let Err(wrong) = http_sessions(&input) {
                unreachable!("{wrong}");
            }
        }
    }
}
