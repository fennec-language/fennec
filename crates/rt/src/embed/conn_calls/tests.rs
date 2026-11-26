//! The typed calls against the named ones they stand for. One served program, a small HTTP loop,
//! answers the same scripted clients both ways, and the clients get the same bytes back. Its runs
//! are recorded both ways too: the two recordings are the same byte for byte, and each replays,
//! either way, seeing what its recording saw.
//!
//! A recording or a replay is a whole process's, so each such run is this test binary run again
//! with the one test that runs it, [`a_served_run_recorded_or_replayed`].

use std::io::{Read as _, Write as _};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use fennec_syntax::Span;

use super::*;
use crate::embed::{Tables, bytes, capabilities, finish, heap, start};

/// What tells a run of itself what to do: record or replay, into or from which log, the named
/// calls or the typed ones, and where to say what it saw.
const ROLE: &str = "FENNEC_TEST_CONN_ROLE";
const LOG: &str = "FENNEC_TEST_CONN_LOG";
const WAY: &str = "FENNEC_TEST_CONN_WAY";
const SAW: &str = "FENNEC_TEST_CONN_SAW";

const SECOND: i64 = 1_000_000_000;
/// The head's limits: bytes in all, header lines, blank lines before it.
const LIMITS: (i64, i64, i64) = (120, 3, 2);

/// The tables every test that serves gives its workers, whose lists of strings are element
/// [`LINES`]: the rounds run on a server's worker, started with whichever test's tables came first.
fn tables() -> Tables {
    crate::embed::parallel::test_worker_tables()
}

/// The element a list of a head's lines is, in [`tables`].
const LINES: i64 = 1;

/// Whether the rounds call the typed exports, rather than by name.
static TYPED: AtomicBool = AtomicBool::new(false);
/// What each round saw, a line a round, in the order the rounds ended.
static ROUNDS_SAW: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// What a call gave, the same whichever way it was made.
#[derive(Debug, PartialEq)]
enum Got {
    Head(Vec<String>),
    Case(String),
    Body(Vec<u8>),
    Yes(bool),
    Done,
    Failed(String),
}

/// What a call made by name gave, as `fennec_socket_call` gives it.
fn named(conn: i64, name: &str, args: &[Value]) -> Value {
    let at = capabilities::socket_method(conn, name, args, Span::default());
    with(|state| {
        let value = state.temp(at);
        state.temps.clear();
        Some(value)
    })
    .unwrap_or(Value::Unit)
}

/// What a named call's value is.
fn got_of(value: Value) -> Got {
    match value {
        Value::Err(error) => match error.as_ref() {
            Value::Instance(error) => Got::Failed(error.case.as_deref().unwrap_or("?").to_owned()),
            other => Got::Failed(format!("{other:?}")),
        },
        Value::Ok(inside) => match Rc::unwrap_or_clone(inside) {
            Value::Unit => Got::Done,
            Value::Bytes(body) => Got::Body(body.to_vec()),
            Value::Instance(head) => match Head::of_value(&Value::Instance(head)) {
                Some(Head::Lines(lines)) => Got::Head(lines),
                Some(other) => Got::Case(format!("{other:?}")),
                None => Got::Failed("not a head".to_owned()),
            },
            other => Got::Failed(format!("not an answer: {other:?}")),
        },
        Value::Bool(yes) => Got::Yes(yes),
        Value::Unit => Got::Done,
        other => Got::Failed(format!("not an answer: {other:?}")),
    }
}

/// The failure a typed call gave back `word` for.
fn failure_of(word: i64) -> Got {
    let value = with(|state| {
        let value = state.temp(-1 - word);
        state.temps.clear();
        Some(value)
    });
    got_of(value.unwrap_or(Value::Unit))
}

fn reads_within(conn: i64, after_ns: i64) -> Got {
    if !TYPED.load(Ordering::SeqCst) {
        let after = Value::made("Duration", vec![("nanos", Value::Int(after_ns))]);
        return got_of(named(conn, "Connection._reads_within", &[after]));
    }
    match conn_reads_within(conn, after_ns) {
        0 => Got::Done,
        word => failure_of(word),
    }
}

fn read_head(conn: i64, timeout_ns: i64) -> Got {
    let (bytes, lines, blank) = LIMITS;
    if !TYPED.load(Ordering::SeqCst) {
        let timeout = Value::made("Duration", vec![("nanos", Value::Int(timeout_ns))]);
        let args = [Value::Int(bytes), Value::Int(lines), Value::Int(blank), timeout];
        return got_of(named(conn, "Connection._read_head.timeout", &args));
    }
    let mut list = 0;
    match conn_read_head(conn, bytes, lines, blank, timeout_ns, LINES, &mut list, 0, 0) {
        0 => {
            let lines = with(|state| {
                let Value::List(lines) = heap::read_block(state, list.cast_unsigned()).unpacked() else {
                    return None;
                };
                heap::release(state, list.cast_unsigned());
                lines
                    .iter()
                    .map(|line| match line {
                        Value::Str(text) => Some(text.to_string()),
                        _ => None,
                    })
                    .collect::<Option<Vec<String>>>()
            });
            lines.map_or_else(|| Got::Failed("not a list of lines".to_owned()), Got::Head)
        }
        case @ 1..=4 => {
            let names = ["Lines", "Ended", "Unended", "LongFirst", "TooBig"];
            Got::Case(names.get(usize::try_from(case).unwrap_or(0)).copied().unwrap_or("?").to_owned())
        }
        word => failure_of(word),
    }
}

fn room_for_body(conn: i64, size: i64) -> Got {
    if !TYPED.load(Ordering::SeqCst) {
        return got_of(named(conn, "Connection._room_for_body", &[Value::Int(size)]));
    }
    Got::Yes(conn_room_for_body(conn, size, 0, 0) == 1)
}

fn body(conn: i64, size: i64, timeout_ns: i64) -> Got {
    if !TYPED.load(Ordering::SeqCst) {
        let timeout = Value::made("Duration", vec![("nanos", Value::Int(timeout_ns))]);
        return got_of(named(conn, "Connection._body.timeout", &[Value::Int(size), timeout]));
    }
    match conn_body(conn, size, timeout_ns, 0, 0) {
        word if word > 0 => {
            let data = with(|state| {
                let data = state.heap.block(word.cast_unsigned()).map(bytes::data_of);
                heap::release(state, word.cast_unsigned());
                data
            });
            data.map_or_else(|| Got::Failed("not a body".to_owned()), Got::Body)
        }
        word => failure_of(word),
    }
}

fn draining(conn: i64) -> Got {
    if !TYPED.load(Ordering::SeqCst) {
        return got_of(named(conn, "Connection.draining", &[]));
    }
    Got::Yes(conn_draining(conn) == 1)
}

/// Writes `data`, as a `str` when `text`, else as `bytes`.
fn write(conn: i64, data: &[u8], text: bool) -> Got {
    if !TYPED.load(Ordering::SeqCst) {
        let data =
            if text { Value::str(&String::from_utf8_lossy(data)) } else { Value::Bytes(Rc::from(data)) };
        return got_of(named(conn, "Connection.write", &[data]));
    }
    let block = with(|state| {
        if text {
            heap::text_address(state, &String::from_utf8_lossy(data))
        } else {
            bytes::make(state, data)
        }
    });
    let word = conn_write(conn, block, NO_TIMEOUT, 0, 0);
    with(|state| heap::release(state, block.cast_unsigned()));
    if word == 0 { Got::Done } else { failure_of(word) }
}

/// A round, as std.net's loop is: each request's head read in one call, with the whole request
/// to come within 5 s, a POST's body read whole, and an answer written, a GET's as text and a
/// POST's as bytes. A head that isn't one is answered with its case, and a failure with its case,
/// and either ends the round, as the stream's end does.
extern "C" fn http_body(_env: i64, conn: i64, _thread: i64) -> i64 {
    let mut saw = Vec::new();
    loop {
        saw.push(format!("{:?}", reads_within(conn, 5 * SECOND)));
        let head = read_head(conn, 5 * SECOND);
        saw.push(format!("{head:?}"));
        let lines = match head {
            Got::Head(lines) => lines,
            Got::Case(case) if case == "Ended" => break,
            Got::Case(case) | Got::Failed(case) => {
                saw.push(format!("{:?}", write(conn, format!("{case}\n").as_bytes(), true)));
                break;
            }
            other => {
                saw.push(format!("not a head: {other:?}"));
                break;
            }
        };
        let first = lines.first().cloned().unwrap_or_default();
        let length = lines.iter().find_map(|line| line.strip_prefix("content-length: ")?.parse::<i64>().ok());
        let answer = match length {
            Some(length) if first.starts_with("POST") => {
                saw.push(format!("{:?}", room_for_body(conn, length)));
                let read = body(conn, length, 5 * SECOND);
                saw.push(format!("{read:?}"));
                let Got::Body(read) = read else { break };
                let mut answer = format!("got {first} with ").into_bytes();
                answer.extend(read);
                answer.push(b'\n');
                write(conn, &answer, false)
            }
            _ => write(conn, format!("got {first}\n").as_bytes(), true),
        };
        saw.push(format!("{answer:?}"));
        saw.push(format!("{:?}", draining(conn)));
    }
    ROUNDS_SAW.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(saw.join(" / "));
    0
}

/// The scripts each client follows, one connection each, one after another: what each writes,
/// a piece at a time with a pause between, before it says it's done.
fn scripts() -> Vec<Vec<&'static [u8]>> {
    vec![
        // Two requests in one write, then a POST with its body.
        vec![
            b"GET /a HTTP/1.1\r\nhost: x\r\n\r\nGET /b HTTP/1.1\r\n\r\n",
            b"POST /orders HTTP/1.1\r\ncontent-length: 5\r\n\r\nhello",
        ],
        // A head and a body a few bytes at a time, after a blank line.
        vec![b"\r\nPO", b"ST /c HTTP/1.1\r", b"\ncontent-len", b"gth: 3\r\n\r\n", b"a", b"bc"],
        // Too many header lines, then a request line too long.
        vec![b"GET / HTTP/1.1\r\na: 1\r\nb: 2\r\nc: 3\r\nd: 4\r\n\r\n"],
        vec![b"GET /", &[b'x'; 200], b" HTTP/1.1\r\n\r\n"],
        // A head the stream ends inside, a line ended by a bare line feed, and a body cut short.
        vec![b"GET / HTTP/1.1\r\nhost: x\r\n"],
        vec![b"GET / HTTP/1.1\n\r\n"],
        vec![b"POST /d HTTP/1.1\r\ncontent-length: 10\r\n\r\nabc"],
    ]
}

/// Runs every script against the server at `address`: the bytes each got back.
fn clients(address: SocketAddr) -> Vec<Vec<u8>> {
    scripts()
        .into_iter()
        .map(|pieces| {
            let mut stream = TcpStream::connect(address).expect("the server takes a connection");
            stream.set_read_timeout(Some(Duration::from_secs(20))).expect("a read timeout");
            for piece in pieces {
                stream.write_all(piece).expect("the server takes what's sent");
                std::thread::sleep(Duration::from_millis(5));
            }
            stream.shutdown(Shutdown::Write).expect("the client can say it's done");
            let mut back = Vec::new();
            let _ = stream.read_to_end(&mut back);
            back
        })
        .collect()
}

/// Serves `http_body` until the clients are done, if there are any: what the clients got back,
/// and what the rounds saw.
fn served(with_clients: bool) -> (Vec<Vec<u8>>, Vec<String>) {
    crate::embed::parallel::set_test_worker_setup();
    start(tables(), crate::Inputs::default());
    let listened =
        crate::stdlib::net::real_net("_listen", &[Value::Int(0)], None, Span::default()).expect("a server");
    let Value::Ok(server) = listened else { panic!("not listening: {listened:?}") };
    let Value::Instance(server) = server.as_ref() else { panic!("not a server: {server:?}") };
    let (Some(Value::Int(port)), Some(Value::Int(handle))) = (server.field("port"), server.field("_handle"))
    else {
        panic!("a server with no port")
    };
    let (port, handle) = (*port, *handle);
    let client = with_clients.then(|| {
        std::thread::spawn(move || {
            let address: SocketAddr = format!("127.0.0.1:{port}").parse().expect("an address");
            let got = std::panic::catch_unwind(|| clients(address));
            // Closed however the clients did, so a failure can't leave the server running.
            crate::embed::serve::close_when_served(handle);
            got.expect("the clients ran")
        })
    });
    crate::embed::serve::net_serve(handle, http_body, 0);
    let got = client.map(|client| client.join().expect("the clients ran")).unwrap_or_default();
    finish();
    let saw = std::mem::take(&mut *ROUNDS_SAW.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
    (got, saw)
}

/// What each script got back, the way the rounds called, made readable.
fn readable(got: &[Vec<u8>]) -> Vec<String> {
    got.iter().map(|back| String::from_utf8_lossy(back).into_owned()).collect()
}

/// A recording or a replay of [`served`], as the environment says, saying what it saw into a file:
/// what the clients got back, if there were any, then what each round saw. Run by
/// [`typed_calls_record_what_named_ones_do_and_replay_either_way`], each in a process of its own;
/// on its own, it does nothing.
#[test]
#[ignore = "a recording or a replay needs a process of its own"]
fn a_served_run_recorded_or_replayed() {
    let (Some(role), Some(log), Some(saw_into)) =
        (std::env::var_os(ROLE), std::env::var_os(LOG).map(PathBuf::from), std::env::var_os(SAW))
    else {
        return;
    };
    TYPED.store(std::env::var(WAY).is_ok_and(|way| way == "typed"), Ordering::SeqCst);
    let recording = role == "record";
    if recording {
        crate::replay::start_log(&log, b"conn").expect("a log to record into");
        crate::replay::record_to(&log).expect("a recording");
    } else {
        crate::replay::replay_from(&log).expect("a replay");
    }
    let (got, saw) = served(recording);
    if recording {
        crate::replay::ended();
    }
    let mut said = readable(&got);
    said.push("--".to_owned());
    said.extend(saw);
    if let Some(why) = crate::replay::divergence() {
        said.push(format!("diverged: {why}"));
    }
    if let Some(why) = crate::replay::unfinished().filter(|_| !recording) {
        said.push(format!("unfinished: {why}"));
    }
    std::fs::write(saw_into, said.join("\n")).expect("somewhere to say what it saw");
}

/// Runs [`a_served_run_recorded_or_replayed`] in a process of its own, on one worker, as `role`,
/// calling `way`: what it saw. It's given 60 seconds.
fn ran(role: &str, log: &Path, way: &str) -> String {
    let saw = log.with_extension(format!("{role}-{way}-saw"));
    let _ = std::fs::remove_file(&saw);
    let mut child = Command::new(std::env::current_exe().expect("this test's binary"))
        .args([
            "--exact",
            "embed::conn_calls::tests::a_served_run_recorded_or_replayed",
            "--ignored",
            "--quiet",
        ])
        .env(ROLE, role)
        .env(LOG, log)
        .env(WAY, way)
        .env(SAW, &saw)
        .env("FENNEC_THREADS", "1")
        .spawn()
        .expect("this test's binary runs");
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().expect("the run can be waited for") {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the {role} calling {way} was still running after 60 s");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success(), "the {role} calling {way} failed: {status}");
    let said = std::fs::read_to_string(&saw).unwrap_or_default();
    let _ = std::fs::remove_file(&saw);
    said
}

/// The clients get the same bytes back whichever way the rounds call, and the rounds see the same.
#[test]
fn typed_calls_answer_every_byte_as_named_ones_do() {
    let (named_got, named_saw) = {
        TYPED.store(false, Ordering::SeqCst);
        served(true)
    };
    let (typed_got, typed_saw) = {
        TYPED.store(true, Ordering::SeqCst);
        served(true)
    };
    TYPED.store(false, Ordering::SeqCst);
    let readable_got = readable(&typed_got);
    assert_eq!(
        readable_got,
        [
            "got GET /a HTTP/1.1\ngot GET /b HTTP/1.1\ngot POST /orders HTTP/1.1 with hello\n",
            "got POST /c HTTP/1.1 with abc\n",
            "TooBig\n",
            "LongFirst\n",
            "Unended\n",
            "Other\n",
            // A body cut short is what came, which std refuses as shorter than it said.
            "got POST /d HTTP/1.1 with abc\n",
        ],
        "what the scripts got back, calling typed"
    );
    assert_eq!(typed_got, named_got, "every byte answered, typed against named");
    assert_eq!(typed_saw, named_saw, "what the rounds saw, typed against named");
}

/// A run recorded calling by name and one calling typed are the same recording byte for byte,
/// and each replays calling either way, seeing what its recording saw.
#[test]
fn typed_calls_record_what_named_ones_do_and_replay_either_way() {
    let folder = std::env::temp_dir().join(format!("fennec-conn-calls-{}", std::process::id()));
    std::fs::create_dir_all(&folder).expect("a folder for the logs");
    let (named_log, typed_log) = (folder.join("named.log"), folder.join("typed.log"));
    let named = ran("record", &named_log, "named");
    let typed = ran("record", &typed_log, "typed");
    assert!(named.contains("got POST /orders HTTP/1.1 with hello"), "the recording served:\n{named}");
    assert_eq!(typed, named, "what the two recordings saw");
    assert!(
        std::fs::read(&typed_log).expect("the typed recording")
            == std::fs::read(&named_log).expect("the named one"),
        "the typed calls' recording isn't the named calls' byte for byte"
    );
    // A replay has no clients: it sees what the rounds saw.
    let rounds =
        |said: &str| said.split_once("--\n").map(|(_, rounds)| rounds.to_owned()).unwrap_or_default();
    for (log, way) in [(&named_log, "typed"), (&typed_log, "named"), (&typed_log, "typed")] {
        let replayed = ran("replay", log, way);
        assert_eq!(rounds(&replayed), rounds(&named), "{} replayed calling {way}", log.display());
    }
    let _ = std::fs::remove_dir_all(&folder);
}

/// A connection no worker serves, one the program opened, goes where `fennec_socket_call` sends
/// it, typed or not: a write and a head read reach the other side and come back, and a call
/// on a connection there isn't fails as a named one does.
#[test]
fn typed_calls_on_a_connection_the_program_opened_go_by_value() {
    start(tables(), crate::Inputs::default());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a listener");
    let port = listener.local_addr().expect("its address").port();
    let other_side = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("a connection");
        let mut asked = [0; 6];
        stream.read_exact(&mut asked).expect("what was written");
        stream.write_all(b"HTTP/1.1 200 OK\r\nx: y\r\n\r\n").expect("an answer");
        asked
    });
    let args = [Value::str("127.0.0.1"), Value::Int(i64::from(port))];
    let connected =
        crate::stdlib::net::real_net("_connect", &args, None, Span::default()).expect("a connection");
    let Value::Ok(conn) = connected else { panic!("not connected: {connected:?}") };
    let Value::Instance(conn) = conn.as_ref() else { panic!("not a connection: {conn:?}") };
    let Some(Value::Int(conn)) = conn.field("_handle") else { panic!("a connection with no handle") };
    TYPED.store(true, Ordering::SeqCst);
    assert_eq!(write(*conn, b"hello\n", true), Got::Done);
    assert_eq!(&other_side.join().expect("the other side ran"), b"hello\n");
    assert_eq!(
        read_head(*conn, 5 * SECOND),
        Got::Head(vec!["HTTP/1.1 200 OK".to_owned(), "x: y".to_owned()])
    );
    let typed = write(i64::MAX, b"gone\n", false);
    TYPED.store(false, Ordering::SeqCst);
    assert!(matches!(typed, Got::Failed(_)), "a write to no connection fails: {typed:?}");
    assert_eq!(typed, write(i64::MAX, b"gone\n", false), "as a named one does");
    finish();
}
