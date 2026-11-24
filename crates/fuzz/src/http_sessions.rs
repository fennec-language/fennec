//! The `http-sessions` target: one `std.net` HTTP server, built natively, met by a script of
//! clients one after another, as a server on the network is. Where `http-server` sends each input
//! whole on one connection to a server of its own, here each client sends its bytes in pieces,
//! with pauses between them, and then:
//! - closes its sending side and reads every answer;
//! - drops the connection, maybe in the middle of a request;
//! - or goes quiet without closing, until the server stops waiting for it.
//!
//! Each answer a client reads is held to what `http-server`'s reader says the same bytes should
//! get, however they were split. Clients may meet the server at the same time, up to four at once.
//! After the script, a last client asks for one plain request, which the server has to answer:
//! whatever came before, it's still up for the next one. By then it must also have let go of what
//! the clients held: its sockets come back to where they were and its memory near it. The server
//! runs with one worker or four.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::time::{Duration, Instant};

use crate::http_rules::{answers_before_end, heard_whole, judged, judged_before_end, started};

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

/// A script: how many workers the server has, how many clients meet it at once, and its clients
/// in order.
#[derive(Debug)]
struct Script {
    threads: usize,
    together: usize,
    clients: Vec<Client>,
}

/// The script an input says. Its first byte picks the workers, one or four with its lowest bit
/// set, and in its next two bits how many clients meet the server at once, one to four: each group
/// of that many starts together, and the next group starts once they've all ended. Then each client is a byte saying how it goes, two bytes saying how many bytes it sends,
/// and those bytes: fewer where the input ends first. The byte saying how it goes is, from its
/// lowest bits:
/// - two bits for the end: `Reads` (0 or 3), `Drops` (1) or `Quiet` (2), at most one quiet client
///   to a script, as each takes the server's whole wait;
/// - two bits for the pieces: whole (0), a byte each (1), seven times the next two bits plus one
///   each (2), or two halves (3);
/// - two bits for the pause: none, 1, 4 or 10 ms.
fn script(input: &[u8]) -> Script {
    let flags = input.first().copied().unwrap_or(0);
    let threads = if flags & 1 == 1 { 4 } else { 1 };
    let together = usize::from(flags >> 1 & 3) + 1;
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
    Script { threads, together, clients }
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

/// How long the server may take to let go of a connection after its client has heard it close.
const CLOSED_WITHIN: Duration = Duration::from_secs(3);

/// How much more memory the server may hold after a script than before it: a script's clients send
/// at most a few hundred kilobytes, so more than this is held for good.
const MOST_GROWTH_KILOBYTES: u64 = 64 * 1024;

/// Whether the server answers one plain request.
fn answers(port: u16) -> Result<(), String> {
    let canary = Client { pieces: vec![CANARY.to_vec()], pause: Duration::ZERO, end: End::Reads };
    let (said, _) = met(port, &canary)?;
    if !said.starts_with(b"HTTP/1.1 200 ") {
        return Err(format!("it heard {:?}", String::from_utf8_lossy(&said)));
    }
    Ok(())
}

/// Whether a descriptor, as `supervise::descriptors` says it, is a socket: the listener or a
/// connection. Every other one a server holds is the runtime's own: its workers' polls, their
/// copies and their wakers.
fn socket(open: &str) -> bool {
    let what = open.split_once(' ').map_or("", |(_, what)| what);
    what.starts_with("socket:")
        || ["IPv4 ", "IPv6 ", "unix ", "sock "].iter().any(|kind| what.starts_with(kind))
}

/// The descriptors of the server `id` that `counted` picks.
fn descriptors(id: u32, counted: fn(&str) -> bool) -> Option<Vec<String>> {
    crate::supervise::descriptors(id).map(|open| open.into_iter().filter(|open| counted(open)).collect())
}

/// What's wrong if the server `id` still holds more of the descriptors `counted` picks than it did
/// `before` its clients, once it's had `CLOSED_WITHIN` to close theirs: how many, and what the
/// ones it didn't hold are.
fn still_held(id: u32, before: Option<Vec<String>>, counted: fn(&str) -> bool) -> Option<String> {
    let before = before?;
    let waited = Instant::now();
    let mut now = descriptors(id, counted)?;
    while now.len() > before.len() && waited.elapsed() < CLOSED_WITHIN {
        std::thread::sleep(Duration::from_millis(50));
        now = descriptors(id, counted)?;
    }
    (now.len() > before.len()).then(|| {
        let new: Vec<&String> = now.iter().filter(|open| !before.contains(open)).collect();
        format!(
            "the server holds {} descriptors {CLOSED_WITHIN:?} after its clients closed, and held {} before them: \
             {new:?} are new",
            now.len(),
            before.len()
        )
    })
}

/// What each client of a group heard, in order, the group meeting the server at once.
fn group_met(port: u16, group: &[Client]) -> Vec<Result<(Vec<u8>, bool), String>> {
    if let [client] = group {
        return vec![met(port, client)];
    }
    std::thread::scope(|scope| {
        let meeting: Vec<_> = group.iter().map(|client| scope.spawn(move || met(port, client))).collect();
        meeting
            .into_iter()
            .map(|client| client.join().unwrap_or_else(|_| Err("its thread panicked".to_owned())))
            .collect()
    })
}

/// The target: a script of clients against one server, each heard as HTTP/1.1 says, and the
/// server still up after them, holding no more than it did before them.
pub(crate) fn http_sessions(input: &[u8]) -> Result<(), String> {
    let Some(executable) = crate::llvm::built_reader("http-sessions")? else { return Ok(()) };
    let script = script(input);
    let (mut server, port) = started(&executable, Some(script.threads))?;
    let id = server.0.id();
    answers(port).map_err(|wrong| format!("the first client: {wrong}"))?;
    // Only sockets are counted, which is what the clients held. The runtime's own descriptors
    // aren't all there yet when the first client is answered: each worker copies its poll's
    // registry when its thread starts, which may come after. That `the runtime lets go of what
    // connections held` is held to every descriptor, over many rounds, by the tests.
    let before = descriptors(id, socket);
    let held = crate::supervise::kilobytes(id);
    let shown = |at: usize| {
        format!(
            "client {at} of {:?}, {} at once, at {} workers",
            script.clients, script.together, script.threads
        )
    };
    for (group, clients) in script.clients.chunks(script.together).enumerate() {
        for (within, (client, heard)) in clients.iter().zip(group_met(port, clients)).enumerate() {
            let at = group * script.together + within;
            let (said, slow) = heard.map_err(|wrong| format!("{wrong}: {}", shown(at)))?;
            if client.end != End::Drops {
                // Sent slowly, the last request may run out of time before it's whole.
                let cut = client.end == End::Quiet || slow;
                judged(&client.bytes(), &said, cut).map_err(|wrong| format!("{wrong}: {}", shown(at)))?;
            }
        }
        if let Ok(Some(status)) = server.0.try_wait() {
            return Err(format!("the server stopped ({status}): {}", shown(group * script.together)));
        }
    }
    let after =
        || format!("after {:?}, {} at once, at {} workers", script.clients, script.together, script.threads);
    answers(port).map_err(|wrong| format!("the last client: {wrong}, {}", after()))?;
    if let Some(wrong) = still_held(id, before, socket) {
        return Err(format!("{wrong}, {}", after()));
    }
    if let Some(now) = crate::supervise::kilobytes(id) {
        if now > 512 * 1024 {
            return Err(format!("the server holds more than 512 MB, {}", after()));
        }
        if let Some(held) = held.filter(|&held| now > held + MOST_GROWTH_KILOBYTES) {
            return Err(format!(
                "the server holds {now} KB after its clients and held {held} KB before them, {}",
                after()
            ));
        }
    }
    Ok(())
}

/// How the server's every answer starts.
const STATUS_LINE: &[u8] = b"HTTP/1.1 ";

/// What a client heard that sends `pieces` a millisecond apart and, once it's heard the answers
/// `heads` says come before the input's end, resets the connection, or once the server's closed.
///
/// A client that closes its side first waits out `TIME_WAIT` after, holding a port of its own for a
/// minute or so, and a run splitting inputs thousands of ways held more than 3,000 of them on
/// macOS (CI's time-wait check, main at 17a9f206d). A reset leaves nothing waiting on either side.
/// A server that closes first, as after refusing a request, waits on the one port it listens on.
fn reset(port: u16, (first, rest): (&[u8], &[u8]), heads: &[bool]) -> Result<Vec<u8>, String> {
    let mut stream =
        TcpStream::connect(("127.0.0.1", port)).map_err(|err| format!("couldn't connect: {err}"))?;
    stream.set_read_timeout(Some(ANSWERED_WITHIN)).map_err(|err| err.to_string())?;
    // The server may close before it's read everything, which isn't wrong.
    let _ = stream.write_all(first).and_then(|()| {
        std::thread::sleep(Duration::from_millis(1));
        stream.write_all(rest)
    });
    let mut said = Vec::new();
    let mut chunk = vec![0; 16 * 1024];
    // Each answer starts with a status line, so until as many have come as are wanted, there's no
    // need to read what's come as answers: counted as it comes, the whole of what's heard isn't
    // read again after every read, which for 20,000 answers took a minute.
    let (mut started, mut counted) = (0, 0_usize);
    while started < heads.len() || !heard_whole(&said, heads) {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                said.extend_from_slice(chunk.get(..read).unwrap_or_default());
                let from = counted.saturating_sub(STATUS_LINE.len() - 1);
                let new = said.get(from..).unwrap_or_default();
                started += new.windows(STATUS_LINE.len()).filter(|window| *window == STATUS_LINE).count();
                counted = said.len();
            }
            Err(err)
                if matches!(err.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) =>
            {
                return Err(format!("the server neither answered nor closed within {ANSWERED_WITHIN:?}"));
            }
            Err(_) => break,
        }
    }
    socket2::SockRef::from(&stream).set_linger(Some(Duration::ZERO)).map_err(|err| err.to_string())?;
    Ok(said)
}

/// The most bytes an input may have to be split at every one of them by `http-splits`. A longer
/// one, like a kept input of a million blank lines or 20,000 requests, is split at its first and
/// last `FEW_SPLITS` bytes and at `FEW_SPLITS` places between, as meeting it once for each byte
/// would take hours.
const EVERY_BYTE_UP_TO: usize = 4096;
const FEW_SPLITS: usize = 16;

/// Where `http-splits` splits an input `length` bytes long: after each of its bytes but the last,
/// or, past `EVERY_BYTE_UP_TO`, at a few places from its start, its end and between.
fn split_points(length: usize) -> Vec<usize> {
    if length <= EVERY_BYTE_UP_TO {
        return (1..length).collect();
    }
    let mut points: Vec<usize> = (1..=FEW_SPLITS).collect();
    points.extend((1..=FEW_SPLITS).map(|at| at * length / (FEW_SPLITS + 1)));
    points.extend(length - FEW_SPLITS..length);
    points
}

/// The `http-splits` target: an `http-server` input sent to one server once for each place it
/// can be split, its two pieces a millisecond apart, so the server reads a head, a line, a length
/// or a body cut wherever a network might cut it, or two requests in one read. Each time, the
/// answers have to be what `http-server`'s reader says the whole input gets, and the server has to
/// answer a plain request after them all.
pub(crate) fn http_splits(input: &[u8]) -> Result<(), String> {
    let Some(executable) = crate::llvm::built_reader("http-server")? else { return Ok(()) };
    let (mut server, port) = started(&executable, None)?;
    let heads = answers_before_end(input);
    for (nth, at) in split_points(input.len()).into_iter().enumerate() {
        let (first, rest) = input.split_at(at);
        let shown = || format!("split after byte {at} of {}", input.len());
        // An input's end is the same however it's split, so the first split closes its side to
        // say so, and the rest each read the answers that come before it and reset (see `reset`).
        let heard = if nth == 0 {
            let client = Client {
                pieces: vec![first.to_vec(), rest.to_vec()],
                pause: Duration::from_millis(1),
                end: End::Reads,
            };
            met(port, &client).and_then(|(said, slow)| judged(input, &said, slow))
        } else {
            reset(port, (first, rest), &heads).and_then(|said| judged_before_end(input, &said))
        };
        if let Ok(Some(status)) = server.0.try_wait() {
            return Err(format!("the server stopped ({status}), {}", shown()));
        }
        heard.map_err(|wrong| format!("{wrong}, {}", shown()))?;
    }
    answers(port).map_err(|wrong| format!("the last client: {wrong}"))?;
    if crate::supervise::kilobytes(server.0.id()).is_some_and(|held| held > 512 * 1024) {
        return Err(format!("the server holds more than 512 MB after {} bytes, split", input.len()));
    }
    Ok(())
}

/// An input for `script`: its flags, then each client as how it goes and its bytes.
#[cfg(test)]
fn written(flags: u8, clients: &[(u8, &[u8])]) -> Vec<u8> {
    let mut input = vec![flags];
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
    use super::{
        End, MOST_PIECES, answers, descriptors, group_met, http_sessions, script, socket, still_held, written,
    };
    use crate::http_rules::started;
    use crate::supervise::kilobytes;

    const GET: &[u8] = b"GET /a?b=c HTTP/1.1\r\nHost: h\r\n\r\n";
    const POST: &[u8] = b"POST /p HTTP/1.1\r\nHost: h\r\nContent-Length: 5\r\n\r\nhello";
    const REFUSED: &[u8] =
        b"GET / HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nGET / HTTP/1.1\r\n\r\n";

    #[test]
    fn an_input_says_its_clients() {
        let input = written(1, &[(0b0000_0100, GET), (0b1000_1101, POST), (2, GET), (2, GET)]);
        let read = script(&input);
        assert_eq!(read.threads, 4);
        assert_eq!(read.together, 1);
        assert_eq!(script(&[0b0000_0010]).together, 2);
        assert_eq!((script(&[0b0000_0111]).threads, script(&[0b0000_0111]).together), (4, 4));
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
    /// that goes quiet, one client at a time and four at once, at one worker and four, are each
    /// heard as HTTP/1.1 says, and the server answers the next client after them, holding no more
    /// than before them.
    #[test]
    fn a_server_hears_every_client_as_http_says_and_stays_up() {
        let pipelined = [GET, POST, GET].concat();
        for flags in [0b000, 0b001, 0b110, 0b111] {
            let input = written(
                flags,
                &[
                    (0b0100_0100, &pipelined),
                    (0b0000_1100, POST),
                    (0b0000_0101, &POST[..30]),
                    (0b0000_1000, REFUSED),
                    (0b0000_0010, &[GET, &POST[..40]].concat()),
                    (0b1100_1000, &pipelined),
                ],
            );
            if let Err(wrong) = http_sessions(&input) {
                unreachable!("{wrong}");
            }
        }
    }

    /// How many rounds of clients the server meets, and after how many its memory has settled.
    const ROUNDS: usize = 120;
    const SETTLED: usize = 20;

    /// A server met by the same clients round after round, four at once, lets go of what each round
    /// held: its descriptors come back to where they were and its memory stops growing, at one
    /// worker and four.
    #[test]
    fn a_server_lets_go_of_what_round_after_round_held() {
        let executable = match crate::llvm::built_reader("http-sessions") {
            Ok(Some(executable)) => executable,
            Ok(None) => return,
            Err(wrong) => unreachable!("{wrong}"),
        };
        let pipelined = [GET, POST, GET].concat();
        let clients = script(&written(
            0b110,
            &[
                (0b0000_0101, &POST[..30]),
                (0b0000_1000, REFUSED),
                (0b0100_0100, &pipelined),
                (0b0000_0001, GET),
            ],
        ))
        .clients;
        for threads in [1, 4] {
            let (server, port) = match started(&executable, Some(threads)) {
                Ok(started) => started,
                Err(wrong) => unreachable!("{wrong}"),
            };
            let id = server.0.id();
            assert_eq!(answers(port), Ok(()));
            let sockets = descriptors(id, socket);
            // The listener at least is a socket, or the check counts nothing.
            assert!(
                sockets.as_ref().is_none_or(|open| !open.is_empty()),
                "no sockets in {:?}",
                descriptors(id, |_| true)
            );
            let (mut settled, mut every) = (None, None);
            for round in 0..ROUNDS {
                for heard in group_met(port, &clients) {
                    if let Err(wrong) = heard {
                        unreachable!("round {round} at {threads} workers: {wrong}");
                    }
                }
                // By now every worker has long started and made its own descriptors, so from
                // here on all of them are counted.
                if round == SETTLED {
                    settled = kilobytes(id);
                    every = descriptors(id, |_| true);
                }
            }
            assert_eq!(answers(port), Ok(()));
            assert_eq!(still_held(id, sockets, socket), None, "sockets, at {threads} workers");
            assert_eq!(still_held(id, every, |_| true), None, "after round {SETTLED}, at {threads} workers");
            if let (Some(settled), Some(now)) = (settled, kilobytes(id)) {
                assert!(
                    now <= settled + 8 * 1024,
                    "{now} KB after {ROUNDS} rounds, {settled} KB after {SETTLED}, at {threads} workers"
                );
            }
        }
    }
}
