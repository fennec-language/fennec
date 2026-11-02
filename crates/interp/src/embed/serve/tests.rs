use std::io::{BufRead as _, BufReader, Write as _};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use mio::net::TcpListener;

use super::{Conn, Failure, Handler, Served, Stopper, serve};

fn listener() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let address = listener.local_addr().unwrap();
    (listener, address)
}

/// Sends back each line it's sent until the other side is done; a line saying "fail" fails
/// its connection.
fn echo() -> Handler {
    Arc::new(|mut conn: Conn| {
        loop {
            match conn.read_line(None) {
                Ok(Some(line)) if line == b"fail" => return 1,
                Ok(Some(mut line)) => {
                    line.push(b'\n');
                    if conn.write(&line).is_err() {
                        return 1;
                    }
                }
                Ok(None) => return 0,
                Err(_) => return 1,
            }
        }
    })
}

/// Serves with `handler` on `workers` workers while `clients` runs, then stops.
fn run(workers: usize, handler: &Handler, clients: impl FnOnce(SocketAddr) + Send + 'static) -> Served {
    let (listener, address) = listener();
    let started = move |stopper: Stopper| {
        std::thread::spawn(move || {
            clients(address);
            stopper.stop();
        });
    };
    serve(listener, workers, &(Arc::new(|| {}) as Arc<dyn Fn() + Send + Sync>), handler, started).unwrap()
}

/// A client that sends `lines` and checks each comes back.
fn talk(address: SocketAddr, lines: &[String]) {
    let mut stream = std::net::TcpStream::connect(address).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    for line in lines {
        stream.write_all(format!("{line}\n").as_bytes()).unwrap();
        let mut back = String::new();
        reader.read_line(&mut back).unwrap();
        assert_eq!(back, format!("{line}\n"));
    }
}

#[test]
fn hundreds_of_connections_are_served_by_a_few_workers_each_a_parked_task() {
    let served = run(3, &echo(), |address| {
        let clients: Vec<_> = (0..40)
            .map(|client| {
                std::thread::spawn(move || {
                    for connection in 0..5 {
                        let lines: Vec<String> =
                            (0..3).map(|line| format!("{client}.{connection}.{line}")).collect();
                        talk(address, &lines);
                    }
                })
            })
            .collect();
        for client in clients {
            client.join().unwrap();
        }
    });
    assert_eq!(served, Served { connections: 200, failed: 0 });
}

#[test]
fn many_connections_wait_at_once_on_one_worker() {
    let served = run(1, &echo(), |address| {
        // All connect and are parked waiting before any of them says anything.
        let mut streams: Vec<_> = (0..100).map(|_| std::net::TcpStream::connect(address).unwrap()).collect();
        for (at, stream) in streams.iter_mut().enumerate() {
            stream.write_all(format!("line {at}\n").as_bytes()).unwrap();
        }
        for (at, stream) in streams.into_iter().enumerate() {
            let mut back = String::new();
            BufReader::new(stream).read_line(&mut back).unwrap();
            assert_eq!(back, format!("line {at}\n"));
        }
    });
    assert_eq!(served.connections, 100);
}

#[test]
fn a_failing_connection_ends_only_its_own_task() {
    let served = run(2, &echo(), |address| {
        let mut failing = std::net::TcpStream::connect(address).unwrap();
        failing.write_all(b"fail\n").unwrap();
        talk(address, &["still here".to_owned()]);
    });
    assert_eq!(served, Served { connections: 2, failed: 1 });
}

#[test]
fn a_wait_with_a_timeout_gives_up_while_others_go_on() {
    let timing_out: Handler =
        Arc::new(|mut conn: Conn| match conn.read_line(Some(Duration::from_millis(50))) {
            Err(Failure::TimedOut(after)) if after == Duration::from_millis(50) => 7,
            Ok(Some(line)) => {
                let _ = conn.write(&[line, b"\n".to_vec()].concat());
                0
            }
            _ => 1,
        });
    let served = run(1, &timing_out, |address| {
        let quiet = std::net::TcpStream::connect(address).unwrap();
        talk(address, &["answered".to_owned()]);
        std::thread::sleep(Duration::from_millis(300));
        drop(quiet);
    });
    // The quiet one timed out and failed; the other was answered.
    assert_eq!(served, Served { connections: 2, failed: 1 });
}

#[test]
fn stopping_a_server_cancels_its_parked_connections() {
    let cancelled: Handler = Arc::new(|mut conn: Conn| match conn.read_line(None) {
        Err(Failure::Cancelled) => 0,
        _ => 1,
    });
    let (keep, kept) = std::sync::mpsc::channel();
    let served = run(2, &cancelled, move |address| {
        let streams: Vec<_> = (0..10).map(|_| std::net::TcpStream::connect(address).unwrap()).collect();
        // Time for the connections to be accepted and parked; any that aren't yet are
        // cancelled before they start, which tells them the same.
        std::thread::sleep(Duration::from_millis(100));
        // Kept open until the server has stopped, so no task sees its connection end first.
        keep.send(streams).unwrap();
    });
    drop(kept);
    assert_eq!(served.failed, 0);
}

#[test]
fn a_wait_that_cant_park_blocks_its_thread_and_is_counted() {
    let (listener, address) = listener();
    let client = std::thread::spawn(move || {
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        stream.write_all(b"late\n").unwrap();
    });
    let accepted = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    };
    let before = super::blocked_waits();
    // Not on an I/O worker, so not a task: the read blocks this thread.
    let mut conn = Conn::new(accepted);
    assert_eq!(conn.read_line(Some(Duration::from_secs(10))), Ok(Some(b"late".to_vec())));
    assert!(super::blocked_waits() > before);
    client.join().unwrap();
}

/// A shell, started before the connections are opened, that reports the process's resident
/// memory, in bytes, as `ps` does, each time it's asked: starting one with thousands of
/// sockets open fails on macOS.
struct Resident(std::process::Child);

impl Resident {
    fn start() -> Resident {
        let script = format!("while read _; do ps -o rss= -p {}; done", std::process::id());
        let child = std::process::Command::new("sh")
            .args(["-c", &script])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        Resident(child)
    }

    fn now(&mut self) -> u64 {
        writeln!(self.0.stdin.as_mut().unwrap()).unwrap();
        let mut line = String::new();
        BufReader::new(self.0.stdout.as_mut().unwrap()).read_line(&mut line).unwrap();
        line.trim().parse::<u64>().unwrap() * 1024
    }
}

/// Measures the memory each parked connection takes: `FENNEC_CONNECTIONS` (1,000 unless set)
/// connections each parked in `read_line`, served by one worker. Run on request, with the
/// open-file limit raised to twice the connections and more:
/// `ulimit -n 30000; FENNEC_CONNECTIONS=10000 cargo test -p fennec-interp --release
/// measure_memory -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, run on request"]
fn measure_memory_per_parked_connection() {
    let count: usize = std::env::var("FENNEC_CONNECTIONS").ok().and_then(|n| n.parse().ok()).unwrap_or(1_000);
    let parked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&parked);
    let waiting: Handler = Arc::new(move |mut conn: Conn| {
        counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        match conn.read_line(None) {
            Err(Failure::Cancelled) => 0,
            _ => 1,
        }
    });
    let (keep, kept) = std::sync::mpsc::channel();
    let mut resident = Resident::start();
    let before = resident.now();
    let served = run(1, &waiting, move |address| {
        // macOS keeps only 128 connections waiting to be accepted, so a refused one tries again.
        let connect = || loop {
            match std::net::TcpStream::connect(address) {
                Ok(stream) => break stream,
                Err(_) => std::thread::sleep(Duration::from_millis(1)),
            }
        };
        let streams: Vec<_> = (0..count).map(|_| connect()).collect();
        while parked.load(std::sync::atomic::Ordering::Relaxed) < count {
            std::thread::sleep(Duration::from_millis(10));
        }
        let after = resident.now();
        let _ = resident.0.kill();
        let each = (after.saturating_sub(before)) / u64::try_from(count).unwrap();
        println!("{count} parked connections: {} KiB in all, {each} bytes each", (after - before) / 1024);
        keep.send(streams).unwrap();
    });
    drop(kept);
    assert_eq!(served.connections, u64::try_from(count).unwrap());
}

/// A served connection's body, as native code would be: sends back each line it's sent until
/// the other side is done.
extern "C" fn echo_body(_env: i64, conn: i64, _thread: i64) -> i64 {
    use super::super::with;
    use crate::value::Value;
    loop {
        let line = super::conn_read_line(conn);
        let line = with(|state| match state.take(line) {
            Value::Ok(line) => match line.as_ref() {
                Value::Str(text) => Some(format!("{text}\n")),
                _ => None,
            },
            _ => None,
        });
        let Some(line) = line else { return 0 };
        let data = with(|state| state.push(Value::str(&line)));
        let written = super::conn_write(conn, data);
        with(|state| drop(state.take(written)));
    }
}

#[test]
fn native_code_serves_a_server_the_interpreter_opened() {
    use super::super::{Tables, finish, start, with};
    use crate::call::CallArg;
    use crate::value::Value;
    super::super::parallel::set_worker_setup(Box::new(|| {
        let _ =
            start(&fennec_syntax::Module { items: Vec::new() }, Tables::default(), crate::Inputs::default());
    }));
    let module = fennec_syntax::parse("from std.net import Connection, NetError, Server\n").unwrap();
    start(&module, Tables::default(), crate::Inputs::default()).unwrap();
    let span = fennec_syntax::Span::default();
    let (server, port) = with(|state| {
        let listened =
            state.interp.net_method("listen", vec![CallArg::plain(Value::Int(0), span)], span).ok()?;
        let Value::Ok(server) = listened else { return None };
        let Value::Instance(instance) = server.as_ref() else { return None };
        let Some(Value::Int(port)) = instance.field("port") else { return None };
        let port = *port;
        Some((state.push(server.as_ref().clone()), port))
    })
    .unwrap();
    let client = std::thread::spawn(move || {
        let address: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let talked = std::panic::catch_unwind(|| {
            talk(address, &["one".to_owned(), "two".to_owned()]);
            talk(address, &["three".to_owned()]);
        });
        // Stopped however the talk went, so a failure can't leave the server running.
        super::stop_servers();
        talked.unwrap();
    });
    assert_eq!(super::net_serve(server, echo_body, 0), 0);
    client.join().unwrap();
    // A server can be served once: it's no longer open to serve again.
    assert_eq!(super::net_serve(server, echo_body, 0), 1);
    finish();
}
