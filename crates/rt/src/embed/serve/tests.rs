use std::io::{BufRead as _, BufReader, Write as _};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use mio::net::TcpListener;

use super::{Conn, Failure, Handler, Listened, Served, Stopper, serve};

fn listener() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let address = listener.local_addr().unwrap();
    (listener, address)
}

/// Sends back each line it's sent until the other side is done; a line saying "fail" fails
/// its connection.
fn echo() -> Handler {
    Arc::new(|conn: Conn| {
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

/// What each connection's handler gave back, as they finish, for a test to wait on rather
/// than on a clock.
type Finished = Arc<(std::sync::Mutex<Vec<i64>>, std::sync::Condvar)>;

/// `handler`, noting what it gives back for each connection in `finished`.
fn noting(handler: Handler, finished: &Finished) -> Handler {
    let finished = Arc::clone(finished);
    Arc::new(move |conn: Conn| {
        let result = handler(conn);
        finished.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(result);
        finished.1.notify_all();
        result
    })
}

/// Waits until `count` connections have finished, giving back what each gave back, in the
/// order they finished. The bound is only there so a broken server fails the test rather than
/// hanging it.
fn wait_for(finished: &Finished, count: usize) -> Vec<i64> {
    let ended = finished.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let waited = finished.1.wait_timeout_while(ended, Duration::from_secs(30), |ended| ended.len() < count);
    // Copied out and let go before checking, so a failure here can't leave the lock poisoned
    // for a task still running.
    let ended = waited.map_or_else(|poisoned| poisoned.into_inner().0.clone(), |(ended, _)| ended.clone());
    assert!(ended.len() >= count, "only {} of {count} connections finished", ended.len());
    ended
}

/// Serves with `handler` on `workers` workers while `clients` runs, then stops.
fn run(workers: usize, handler: &Handler, clients: impl FnOnce(SocketAddr) + Send + 'static) -> Served {
    let (listener, address) = listener();
    let (sent, ran) = std::sync::mpsc::channel();
    let started = move |stopper: Stopper| {
        std::thread::spawn(move || {
            // Stopped however the clients went, so a failing one fails the test rather than
            // leaving the server running and the test hanging.
            let went = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| clients(address)));
            stopper.stop();
            let _ = sent.send(went);
        });
    };
    let pool = crate::embed::parallel::with_setup(workers, &(Arc::new(|| {}) as Arc<dyn Fn() + Send + Sync>));
    let served = serve(listener, pool, None, handler, (started, None, Listened::default()));
    if let Ok(Err(panic)) = ran.recv() {
        std::panic::resume_unwind(panic);
    }
    served
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
    goodbye(&mut stream, &mut reader);
}

/// Ends a talk with a server as a client that's done: it says so, and once the server has closed
/// its side, lets go of the connection without holding its port. The side that closes first
/// holds its port for a minute or so afterwards, and a test making hundreds of connections
/// beside other runs would take every port there is: "Can't assign requested address". The
/// server sees the same end as ever, and nothing waits on either side.
fn goodbye(stream: &mut std::net::TcpStream, reader: &mut impl std::io::Read) {
    // Set while the connection is open: macOS refuses it on one already waiting out its close.
    socket2::SockRef::from(&*stream).set_linger(Some(Duration::ZERO)).unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    let mut rest = Vec::new();
    reader.read_to_end(&mut rest).unwrap();
    assert!(rest.is_empty(), "{rest:?}");
}

/// A connection the server takes sends each write at once, without waiting for the last to be
/// acknowledged: a response's head and body are two writes, and the wait held each response with a
/// body up to 40 ms on Linux.
#[test]
fn a_taken_connection_sends_each_write_at_once() {
    let says_if_it_waits: Handler = Arc::new(|conn: Conn| {
        let at_once = conn.stream.borrow().nodelay().unwrap_or(false);
        i64::from(conn.write(if at_once { b"at once\n" } else { b"waits\n" }).is_err())
    });
    run(1, &says_if_it_waits, |address| {
        let stream = std::net::TcpStream::connect(address).unwrap();
        let mut said = String::new();
        BufReader::new(stream).read_line(&mut said).unwrap();
        assert_eq!(said, "at once\n");
    });
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
    assert_eq!(served, Served { connections: 200, failed: 0, owners: std::collections::HashMap::new() });
}

#[test]
fn many_connections_wait_at_once_on_one_worker() {
    let served = run(1, &echo(), |address| {
        // All connect and are parked waiting before any of them says anything.
        let mut streams: Vec<_> = (0..100).map(|_| std::net::TcpStream::connect(address).unwrap()).collect();
        for (at, stream) in streams.iter_mut().enumerate() {
            stream.write_all(format!("line {at}\n").as_bytes()).unwrap();
        }
        for (at, mut stream) in streams.into_iter().enumerate() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut back = String::new();
            reader.read_line(&mut back).unwrap();
            assert_eq!(back, format!("line {at}\n"));
            goodbye(&mut stream, &mut reader);
        }
    });
    assert_eq!(served.connections, 100);
}

#[test]
fn a_failing_connection_ends_only_its_own_task() {
    let finished = Finished::default();
    let waited = Arc::clone(&finished);
    let served = run(2, &noting(echo(), &finished), move |address| {
        let mut failing = std::net::TcpStream::connect(address).unwrap();
        failing.write_all(b"fail\n").unwrap();
        talk(address, &["still here".to_owned()]);
        // Stopped only once both have finished, so neither is cancelled by the stop first.
        wait_for(&waited, 2);
    });
    assert_eq!(served, Served { connections: 2, failed: 1, owners: std::collections::HashMap::new() });
    let mut ended = wait_for(&finished, 2);
    ended.sort_unstable();
    assert_eq!(ended, [0, 1]);
}

#[test]
fn a_wait_with_a_timeout_gives_up_while_others_go_on() {
    let timing_out: Handler = Arc::new(|conn: Conn| match conn.read_line(Some(Duration::from_millis(50))) {
        Err(Failure::TimedOut(after)) if after == Duration::from_millis(50) => 7,
        Ok(Some(line)) => {
            let _ = conn.write(&[line, b"\n".to_vec()].concat());
            0
        }
        _ => 1,
    });
    let finished = Finished::default();
    let waited = Arc::clone(&finished);
    let served = run(1, &noting(timing_out, &finished), move |address| {
        let quiet = std::net::TcpStream::connect(address).unwrap();
        talk(address, &["answered".to_owned()]);
        // However coarse the clock, the quiet one gives up before it's closed or the server
        // stops: both wait until it and the answered one have finished.
        wait_for(&waited, 2);
        drop(quiet);
    });
    // The quiet one timed out, and the other was answered, whichever finished first.
    assert_eq!(served, Served { connections: 2, failed: 1, owners: std::collections::HashMap::new() });
    let mut ended = wait_for(&finished, 2);
    ended.sort_unstable();
    assert_eq!(ended, [0, 7]);
}

#[test]
fn a_connection_that_sends_nothing_times_out_however_often_its_task_is_woken() {
    // Woken at every poll, as a socket that keeps saying it's writable wakes its task, a read
    // that's never answered still gives up when its time's up, not later each time.
    let quiet: Handler = Arc::new(|conn: Conn| {
        super::WAKE_ALL.set(true);
        let read = conn.read_line(Some(Duration::from_millis(100)));
        super::WAKE_ALL.set(false);
        match read {
            Err(Failure::TimedOut(after)) if after == Duration::from_millis(100) => 7,
            _ => 1,
        }
    });
    let finished = Finished::default();
    let waited = Arc::clone(&finished);
    let served = run(1, &noting(quiet, &finished), move |address| {
        let silent = std::net::TcpStream::connect(address).unwrap();
        wait_for(&waited, 1);
        drop(silent);
    });
    assert_eq!(wait_for(&finished, 1), [7]);
    assert_eq!(served, Served { connections: 1, failed: 1, owners: std::collections::HashMap::new() });
}

#[test]
fn stopping_a_server_cancels_its_parked_connections() {
    let cancelled: Handler = Arc::new(|conn: Conn| match conn.read_line(None) {
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
    let before = super::blocked_waits();
    let client = std::thread::spawn(move || {
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        // Sent only once the read below has blocked, so it can't find the line there already.
        wait_until("the read to block", || super::blocked_waits() > before);
        stream.write_all(b"late\n").unwrap();
    });
    let accepted = accepted(&listener);
    // Not on an I/O worker, so not a task: the read blocks this thread.
    let conn = Conn::new(accepted);
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
/// `ulimit -n 30000; FENNEC_CONNECTIONS=10000 cargo test -p fennec-rt --release
/// measure_memory -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, run on request"]
fn measure_memory_per_parked_connection() {
    let count: usize = std::env::var("FENNEC_CONNECTIONS").ok().and_then(|n| n.parse().ok()).unwrap_or(1_000);
    let parked = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&parked);
    let waiting: Handler = Arc::new(move |conn: Conn| {
        counted.fetch_add(1, Ordering::Relaxed);
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
        let connect = || {
            let started = std::time::Instant::now();
            loop {
                match std::net::TcpStream::connect(address) {
                    Ok(stream) => break stream,
                    Err(error) => {
                        assert!(
                            started.elapsed() < Duration::from_secs(60),
                            "couldn't connect for a minute: {error}"
                        );
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
            }
        };
        let streams: Vec<_> = (0..count).map(|_| connect()).collect();
        while parked.load(Ordering::Relaxed) < count {
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
    use crate::value::Value;
    let span = fennec_syntax::Span::default();
    loop {
        let Some(Ok(Value::Ok(line))) = super::served_call(conn, "read_line", &[], None, span) else {
            return 1;
        };
        let Value::Str(line) = line.as_ref() else { return 0 };
        let line = Value::str(&format!("{line}\n"));
        let written = super::served_call(conn, "write", &[line], None, span);
        assert!(matches!(written, Some(Ok(Value::Ok(_)))));
    }
}

#[test]
fn native_code_serves_a_server_the_runtime_opened() {
    use super::super::{Tables, finish, start};
    use crate::value::Value;
    // The workers' setup is the process's, set by whichever test gets there first, and this
    // server runs on the pool the `parallel` tests share. So it sets workers up as theirs do: with
    // default tables, their workers had no `Int` element, and a stream's items came out wrong.
    super::super::parallel::set_worker_setup(Box::new(|| {
        let tables = Tables {
            shapes: vec![std::rc::Rc::new(crate::packed::Element::Str)],
            elements: vec![std::rc::Rc::new(crate::packed::Element::Int)],
            ..Tables::default()
        };
        start(tables, crate::Inputs::default());
    }));
    start(Tables::default(), crate::Inputs::default());
    let span = fennec_syntax::Span::default();
    let (server, port) = (|| {
        let listened = crate::stdlib::net::real_net("_listen", &[Value::Int(0)], None, span).ok()?;
        let Value::Ok(server) = listened else { return None };
        let Value::Instance(instance) = server.as_ref() else { return None };
        let (Some(Value::Int(port)), Some(Value::Int(handle))) =
            (instance.field("port"), instance.field("_handle"))
        else {
            return None;
        };
        Some((*handle, *port))
    })()
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

/// Waits until `line` has been written since `mark`. The bound is only there so a broken server
/// fails the test rather than hanging it.
fn wait_for_written(mark: usize, line: &str) {
    let written = || super::super::written_since(mark);
    let started = std::time::Instant::now();
    while !written().contains(line) {
        assert!(started.elapsed() < Duration::from_secs(30), "`{line}` wasn't written: {}", written());
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Prints the first line a connection sends, waits for a second, and prints that: so its first
/// print has to come out while it's parked.
fn printing() -> Handler {
    Arc::new(|conn: Conn| {
        while let Ok(Some(line)) = conn.read_line(None) {
            super::super::with(|state| {
                let _ = super::super::io::write(state, &format!("{}\n", String::from_utf8_lossy(&line)));
            });
        }
        0
    })
}

#[test]
fn what_a_parked_task_printed_comes_out_while_another_task_runs() {
    let mark = super::super::written_mark();
    let (listener, address) = listener();
    let setup: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {
        let inputs = crate::Inputs { live: true, ..crate::Inputs::default() };
        let tables = super::super::Tables::default();
        super::super::start(tables, inputs);
    });
    // This thread is one of the workers, and runs no setup of its own.
    setup();
    let clients = move || {
        let mut parked = std::net::TcpStream::connect(address).unwrap();
        parked.write_all(b"from the parked task\n").unwrap();
        // It's written with this connection still open, its task parked for its next line.
        wait_for_written(mark, "from the parked task\n");
        let mut other = std::net::TcpStream::connect(address).unwrap();
        other.write_all(b"from the other task\n").unwrap();
        wait_for_written(mark, "from the other task\n");
        // One task's prints stay in order, whatever comes between them.
        parked.write_all(b"from the parked task, later\n").unwrap();
        wait_for_written(mark, "from the parked task, later\n");
    };
    let (sent, ran) = std::sync::mpsc::channel();
    let started = move |stopper: Stopper| {
        std::thread::spawn(move || {
            let went = std::panic::catch_unwind(std::panic::AssertUnwindSafe(clients));
            stopper.stop();
            let _ = sent.send(went);
        });
    };
    let pool = crate::embed::parallel::with_setup(2, &setup);
    let served = serve(listener, pool, None, &printing(), (started, None, Listened::default()));
    super::super::finish();
    if let Ok(Err(panic)) = ran.recv() {
        std::panic::resume_unwind(panic);
    }
    assert_eq!(served.failed, 0);
    let text = super::super::written_since(mark);
    let (first, later) =
        (text.find("from the parked task\n").unwrap(), text.find("from the parked task, later\n").unwrap());
    assert!(first < text.find("from the other task\n").unwrap());
    assert!(first < later);
}

/// Answers each request with its line, except every tenth, which crashes on purpose.
fn crashes_every_tenth() -> Handler {
    let count = Arc::new(AtomicUsize::new(0));
    Arc::new(move |conn: Conn| {
        let n = count.fetch_add(1, Ordering::SeqCst) + 1;
        let Ok(Some(mut line)) = conn.read_line(None) else { return 1 };
        if n.is_multiple_of(10) {
            return 1;
        }
        line.push(b'\n');
        i64::from(conn.write(&line).is_err())
    })
}

/// Sends a line on a connection of its own and gives back the line that came back, or nothing
/// when the connection closed first.
fn ask(address: SocketAddr, line: &str) -> Option<String> {
    let mut stream = std::net::TcpStream::connect(address).unwrap();
    stream.write_all(format!("{line}\n").as_bytes()).unwrap();
    let mut back = String::new();
    let read = BufReader::new(stream).read_line(&mut back).unwrap_or(0);
    (read > 0).then_some(back)
}

#[test]
fn a_round_that_crashes_ends_only_itself_and_the_server_answers_the_other_nine() {
    let (answered, closed) = (Arc::new(std::sync::Mutex::new(0)), Arc::new(std::sync::Mutex::new(0)));
    let (counted, shut) = (Arc::clone(&answered), Arc::clone(&closed));
    let served = run(2, &crashes_every_tenth(), move |address| {
        // One after another, so which is tenth is known.
        for request in 0..30 {
            match ask(address, &format!("request {request}")) {
                Some(back) => {
                    assert_eq!(back, format!("request {request}\n"));
                    *counted.lock().unwrap() += 1;
                }
                None => *shut.lock().unwrap() += 1,
            }
        }
    });
    assert_eq!((*answered.lock().unwrap(), *closed.lock().unwrap()), (27, 3));
    assert_eq!((served.connections, served.failed), (30, 3));
}

/// The next connection `listener` takes, which a client has made or is making. Only a listener with
/// none waiting yet is asked again: any other error fails the test, as does a minute with none.
fn accepted(listener: &TcpListener) -> mio::net::TcpStream {
    let started = std::time::Instant::now();
    loop {
        match listener.accept() {
            Ok((stream, _)) => return stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(started.elapsed() < Duration::from_secs(60), "no connection came for a minute");
                std::thread::yield_now();
            }
            Err(error) => panic!("the listener failed to accept: {error}"),
        }
    }
}

/// Waits until `done`, however long the machine takes to get there, as a test that must reach a
/// state before going on does. Only a test that would otherwise hang gives up, after a minute.
fn wait_until(what: &str, done: impl Fn() -> bool) {
    let started = std::time::Instant::now();
    while !done() {
        assert!(started.elapsed() < Duration::from_secs(60), "waited a minute for {what}");
        std::thread::yield_now();
    }
}

/// Serves with `handler` while `clients` runs, until the server ends by itself, as a round's
/// `break` or failure ends it. If it hasn't 10 s after the clients finish, it's stopped, and the
/// result says so, so a server that doesn't end fails its test rather than hang it.
fn run_until_it_ends(handler: &Handler, clients: impl FnOnce(SocketAddr) + Send + 'static) -> (Served, bool) {
    let (listener, address) = listener();
    let ended = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let (watching, had_to_stop) = (Arc::clone(&ended), Arc::new(AtomicBool::new(false)));
    let stopped = Arc::clone(&had_to_stop);
    let (sent, ran) = std::sync::mpsc::channel();
    let started = move |stopper: Stopper| {
        std::thread::spawn(move || {
            let went = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| clients(address)));
            let done = watching.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let (done, waited) = watching
                .1
                .wait_timeout_while(done, Duration::from_secs(10), |done| !*done)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !*done && waited.timed_out() {
                stopped.store(true, Ordering::SeqCst);
                stopper.stop();
            }
            let _ = sent.send(went);
        });
    };
    let pool = crate::embed::parallel::with_setup(2, &(Arc::new(|| {}) as Arc<dyn Fn() + Send + Sync>));
    let served = serve(listener, pool, None, handler, (started, None, Listened::default()));
    *ended.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = true;
    ended.1.notify_all();
    if let Ok(Err(panic)) = ran.recv() {
        std::panic::resume_unwind(panic);
    }
    (served, had_to_stop.load(Ordering::SeqCst))
}

/// Echoes the line it's sent, ending its round with what the line asks: "break", "fail" or
/// nothing.
fn ends_as_asked(started: &Arc<AtomicUsize>) -> Handler {
    let started = Arc::clone(started);
    Arc::new(move |conn: Conn| {
        started.fetch_add(1, Ordering::SeqCst);
        let Ok(Some(mut line)) = conn.read_line(None) else { return 1 };
        let how = match line.as_slice() {
            b"break" => super::ROUND_BROKE,
            b"fail" => super::ROUND_FAILED,
            _ => 0,
        };
        line.push(b'\n');
        let _ = conn.write(&line);
        how
    })
}

#[test]
fn a_break_stops_taking_connections_and_the_rounds_running_finish() {
    let started = Arc::new(AtomicUsize::new(0));
    let rounds = Arc::clone(&started);
    let (served, had_to_stop) = run_until_it_ends(&ends_as_asked(&started), move |address| {
        // A round that's running when another breaks, still waiting for its line.
        let mut running = std::net::TcpStream::connect(address).unwrap();
        running.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        wait_until("the first round to start", || rounds.load(Ordering::SeqCst) > 0);
        assert_eq!(ask(address, "break").as_deref(), Some("break\n"));
        running.write_all(b"still served\n").unwrap();
        let mut back = String::new();
        BufReader::new(running).read_line(&mut back).unwrap();
        assert_eq!(back, "still served\n", "the running round finished");
    });
    assert!(!had_to_stop, "the server ended by itself once its rounds finished");
    assert_eq!((served.connections, served.failed), (2, 0));
}

#[test]
fn a_round_that_fails_stops_the_server_and_cuts_the_others_short() {
    let started = Arc::new(AtomicUsize::new(0));
    let rounds = Arc::clone(&started);
    let (served, had_to_stop) = run_until_it_ends(&ends_as_asked(&started), move |address| {
        // A round waiting for a line it never gets.
        let waiting = std::net::TcpStream::connect(address).unwrap();
        waiting.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        wait_until("the waiting round to start", || rounds.load(Ordering::SeqCst) > 0);
        assert_eq!(ask(address, "fail").as_deref(), Some("fail\n"));
        let mut back = String::new();
        let read = BufReader::new(waiting).read_line(&mut back).unwrap_or(0);
        assert_eq!(read, 0, "the waiting round was cut short, its connection closed");
    });
    assert!(!had_to_stop, "the server ended by itself");
    assert_eq!(served.connections, 2);
}

static STOP_SPIN: AtomicBool = AtomicBool::new(false);

/// A round that's sent "spin" computes, ticking every few thousand turns as native code does,
/// until it's told to stop; any other line is answered "pong" at once.
fn spins_or_pongs(spinning: &Arc<AtomicBool>) -> Handler {
    let spinning = Arc::clone(spinning);
    Arc::new(move |conn: Conn| {
        let line = conn.read_line(None).ok().flatten().unwrap_or_default();
        if line == b"spin" {
            spinning.store(true, Ordering::SeqCst);
            let started = std::time::Instant::now();
            let mut x = 1_u64;
            let mut turns = 0_u64;
            while !STOP_SPIN.load(Ordering::SeqCst) && started.elapsed() < Duration::from_secs(20) {
                x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                turns += 1;
                if turns.is_multiple_of(4096) {
                    super::task_tick();
                }
            }
            std::hint::black_box(x);
            return i64::from(conn.write(b"spun\n").is_err());
        }
        i64::from(conn.write(b"pong\n").is_err())
    })
}

/// A round that computes without ever waiting makes way at its ticks, so on a single worker
/// another connection that arrives while it spins is still answered.
#[test]
fn a_round_that_never_waits_makes_way_for_a_connection_that_arrives_meanwhile() {
    STOP_SPIN.store(false, Ordering::SeqCst);
    let spinning = Arc::new(AtomicBool::new(false));
    let spun = Arc::clone(&spinning);
    run(1, &spins_or_pongs(&spinning), move |address| {
        let mut spinner = std::net::TcpStream::connect(address).unwrap();
        spinner.write_all(b"spin\n").unwrap();
        // The ping comes only once the round spins, so it's answered while it does.
        wait_until("the round to spin", || spun.load(Ordering::SeqCst));
        let mut pinger = std::net::TcpStream::connect(address).unwrap();
        pinger.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        pinger.write_all(b"ping\n").unwrap();
        let mut back = String::new();
        let answered = BufReader::new(pinger).read_line(&mut back);
        STOP_SPIN.store(true, Ordering::SeqCst);
        let mut spun = String::new();
        BufReader::new(spinner).read_line(&mut spun).unwrap();
        assert!(answered.is_ok(), "the ping wasn't answered while the round spun: {answered:?}");
        assert_eq!(back, "pong\n");
        assert_eq!(spun, "spun\n");
    });
}

/// A served connection with TLS: its handshake is done before its round runs, its reads and writes
/// go through it, and one that fails its handshake closes without its round running.
mod tls {
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use rustls::pki_types::CertificateDer;
    use rustls::pki_types::ServerName;
    use rustls::pki_types::pem::PemObject as _;
    use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

    use super::{Conn, Handler, Listened, Served, Stopper, listener, serve};
    use crate::stdlib::http::tls::server;

    const CA: &[u8] = include_bytes!("../../stdlib/testdata/tls-test-ca.pem");
    const CHAIN: &str = include_str!("../../stdlib/testdata/tls-test-chain.pem");
    const KEY: &str = include_str!("../../stdlib/testdata/tls-test-key.pem");

    fn client() -> Arc<ClientConfig> {
        let mut roots = RootCertStore::empty();
        for certificate in CertificateDer::pem_slice_iter(CA) {
            roots.add(certificate.unwrap()).unwrap();
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        Arc::new(config)
    }

    static ROUNDS: AtomicUsize = AtomicUsize::new(0);

    /// Answers each line it's sent with the same line.
    fn echo() -> Handler {
        Arc::new(|conn: Conn| {
            ROUNDS.fetch_add(1, Ordering::SeqCst);
            while let Ok(Some(mut line)) = conn.read_line(Some(Duration::from_secs(10))) {
                line.push(b'\n');
                if conn.write(&line).is_err() {
                    return 1;
                }
            }
            0
        })
    }

    #[test]
    fn a_served_connection_shakes_hands_before_its_round_and_talks_through_tls() {
        ROUNDS.store(0, Ordering::SeqCst);
        let (listening, address) = listener();
        let (sent, ran) = std::sync::mpsc::channel();
        let started = move |stopper: Stopper| {
            std::thread::spawn(move || {
                let went = std::panic::catch_unwind(|| {
                    // A client that doesn't speak TLS fails its handshake, and no round runs for it.
                    let mut plain = std::net::TcpStream::connect(address).unwrap();
                    plain.write_all(b"this isn't TLS\n").unwrap();
                    plain.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
                    let mut closed = Vec::new();
                    let _ = std::io::Read::read_to_end(&mut plain, &mut closed);
                    assert_eq!(ROUNDS.load(Ordering::SeqCst), 0, "a round ran with no handshake");
                    let tcp = std::net::TcpStream::connect(address).unwrap();
                    tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
                    let name = ServerName::try_from("localhost").unwrap();
                    let connection = ClientConnection::new(client(), name).unwrap();
                    let mut stream = BufReader::new(StreamOwned::new(connection, tcp));
                    for line in ["one", "two"] {
                        stream.get_mut().write_all(format!("{line}\n").as_bytes()).unwrap();
                        stream.get_mut().flush().unwrap();
                        let mut back = String::new();
                        stream.read_line(&mut back).unwrap();
                        assert_eq!(back, format!("{line}\n"));
                    }
                    assert_eq!(ROUNDS.load(Ordering::SeqCst), 1);
                });
                stopper.stop();
                let _ = sent.send(went);
            });
        };
        let pool = crate::embed::parallel::with_setup(1, &(Arc::new(|| {}) as Arc<dyn Fn() + Send + Sync>));
        let Ok(tls) = server(CHAIN, KEY) else { panic!("the certificate didn't load") };
        let served: Served =
            serve(listening, pool, None, &echo(), (started, None, Listened { tls: Some(tls), stop: None }));
        if let Ok(Err(panic)) = ran.recv() {
            std::panic::resume_unwind(panic);
        }
        assert_eq!(served.connections, 2);
    }

    #[test]
    fn a_chain_or_key_that_cant_be_read_fails() {
        assert!(server("not pem", KEY).is_err());
        assert!(server(CHAIN, "not pem").is_err());
    }
}

/// A connection's number means the same connection in the whole process: one sent to a task on
/// another worker can't name a different client there.
#[test]
fn connection_numbers_are_never_shared_between_workers() {
    let on_other_worker = std::thread::spawn(super::calls::next_handle).join().unwrap();
    let here = super::calls::next_handle();
    assert_ne!(here, on_other_worker);
    let refused = super::calls::on_conn(on_other_worker, |_| Ok(()));
    assert!(matches!(refused, Err(crate::stdlib::net::Failure::Closed)));
}

/// Running out of file descriptors leaves connections waiting that the poll won't announce again,
/// and a connection that gave up before it was accepted leaves the rest waiting too.
#[test]
fn a_failure_to_accept_doesnt_strand_what_waits_behind_it() {
    use std::io::{Error, ErrorKind};

    use super::{AfterAccept, accept_failed};
    assert_eq!(accept_failed(&Error::from(ErrorKind::ConnectionAborted)), AfterAccept::Next);
    assert_eq!(accept_failed(&Error::from(ErrorKind::Interrupted)), AfterAccept::Next);
    assert_eq!(accept_failed(&Error::from_raw_os_error(24)), AfterAccept::Later);
    assert_eq!(accept_failed(&Error::from(ErrorKind::WouldBlock)), AfterAccept::Stop);
}

/// A connection whose other end writes `sent` and goes, read here as a plain thread would. The
/// writer goes on beside the reader, as what it sends can be more than a socket holds.
fn conn_sent(sent: Vec<u8>) -> Conn {
    let (listener, address) = listener();
    let client = std::thread::spawn(move || {
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        let _ = stream.write_all(&sent);
    });
    let accepted = accepted(&listener);
    drop(client);
    Conn::new(accepted)
}

/// A line with no end in sight stops being read once it's past the most a line may hold, rather
/// than take all the memory its sender can make it.
#[test]
fn a_line_longer_than_the_most_a_line_may_hold_fails() {
    let mut sent = vec![b'x'; crate::stdlib::net::MOST_LINE_BYTES + 10];
    sent.push(b'\n');
    let conn = conn_sent(sent);
    let read = conn.read_line(Some(Duration::from_secs(10)));
    assert!(matches!(&read, Err(Failure::Other(why)) if why.contains("longer than")), "{read:?}");
}

/// Asked for lines ended `\r\n`, a line ended by a bare `\n` fails, as HTTP has to refuse one.
#[test]
fn a_line_with_no_carriage_return_fails_when_one_is_asked_for() {
    let conn = conn_sent(b"good\r\nbad\nlast".to_vec());
    let wait = Some(Duration::from_secs(10));
    assert_eq!(conn.read_line_crlf(wait, true), Ok(Some(b"good".to_vec())));
    let read = conn.read_line_crlf(wait, true);
    assert!(matches!(&read, Err(Failure::Other(why)) if why.contains("carriage return")), "{read:?}");
}

/// A write to a client that never reads gives up once its `timeout=` passes, rather than hold its
/// round forever.
#[test]
fn a_write_nobody_reads_times_out() {
    let (listener, address) = listener();
    let client = std::net::TcpStream::connect(address).unwrap();
    let accepted = accepted(&listener);
    let conn = Conn::new(accepted);
    let started = std::time::Instant::now();
    let wrote = conn.write_within(&vec![b'x'; 64 << 20], Some(Duration::from_millis(200)));
    assert!(matches!(wrote, Err(Failure::TimedOut(_))), "{wrote:?}");
    assert!(started.elapsed() < Duration::from_secs(20));
    drop(client);
}

/// Waits that time out together are woken soonest deadline first and, at one deadline, in the order
/// they parked, whatever order the map keeps them in, so tasks sleeping as long resume the same way
/// each run. Waits not yet due, or with no deadline, stay.
#[test]
fn waits_that_time_out_together_wake_by_deadline_then_in_the_order_they_parked() {
    let now = std::time::Instant::now();
    let earlier = now.checked_sub(Duration::from_millis(5)).unwrap();
    let later = now + Duration::from_secs(60);
    let mut waiting = std::collections::HashMap::new();
    for token in 0..64_usize {
        let deadline = match token % 4 {
            0 => Some(now),
            1 => Some(earlier),
            2 => Some(later),
            _ => None,
        };
        waiting.insert(token, (u64::try_from(token).unwrap(), mio::Interest::READABLE, deadline));
    }
    let woken = super::timed_out(&waiting, now);
    let wanted: Vec<usize> =
        (0..64).filter(|token| token % 4 == 1).chain((0..64).filter(|token| token % 4 == 0)).collect();
    assert_eq!(woken, wanted);
}
