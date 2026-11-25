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
        let at_once = super::locked(&conn.stream).nodelay().unwrap_or(false);
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

/// Connections reset as they come end only their own rounds: twenty connect and reset, and the
/// server goes on to answer the next. macOS hands a connection reset before it's taken to `accept`
/// all the same, and refuses to set an option on it, which the server passes over; Windows says
/// the reset from `accept`, which the server passes over too (#1903).
#[test]
fn connections_reset_as_they_come_end_only_their_own_rounds() {
    let served = run(2, &echo(), |address| {
        for _ in 0..20 {
            let reset = std::net::TcpStream::connect(address).unwrap();
            socket2::SockRef::from(&reset).set_linger(Some(Duration::ZERO)).unwrap();
        }
        talk(address, &["still here".to_owned()]);
    });
    assert!(served.connections >= 1, "{served:?}");
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

/// A round serving a server of its own when its server is stopped is cancelled once its own
/// server has ended, on 2 workers, however late the worker that keeps it looks: with a pause
/// between that worker's look and the round's, another worker could be the last to leave the
/// round's server in between, and the round went on uncancelled (#2004, from Fable 5.2's review).
#[test]
fn a_round_serving_its_own_server_is_cancelled_however_late_its_worker_looks() {
    let went_on = Arc::new(AtomicUsize::new(0));
    let serving = Arc::new(AtomicUsize::new(0));
    let (noting, counting) = (Arc::clone(&went_on), Arc::clone(&serving));
    let serves_its_own: Handler = Arc::new(move |_conn: Conn| {
        let Some((pool, _)) = crate::embed::parallel::here() else { return 1 };
        let (inner, _) = listener();
        counting.fetch_add(1, Ordering::SeqCst);
        let _ = serve(inner, pool, None, &echo(), (|_: Stopper| {}, None, Listened::default()));
        if !fennec_parked::is_cancelled() {
            noting.fetch_add(1, Ordering::SeqCst);
        }
        0
    });
    for _ in 0..60 {
        let (listener, address) = listener();
        let (sent, ran) = std::sync::mpsc::channel();
        let seen = Arc::clone(&serving);
        let started = move |stopper: Stopper| {
            std::thread::spawn(move || {
                let streams: Vec<_> =
                    (0..6).map(|_| std::net::TcpStream::connect(address).unwrap()).collect();
                wait_until("every round to serve", || seen.load(Ordering::SeqCst) >= 6);
                stopper.stop();
                let _ = sent.send(streams);
            });
        };
        serving.store(0, Ordering::SeqCst);
        let pool = crate::embed::parallel::with_setup(2, &(Arc::new(|| {}) as Arc<dyn Fn() + Send + Sync>));
        super::PAUSE_BEFORE_KEPT.store(std::ptr::from_ref(pool).addr(), Ordering::Release);
        let _ = serve(listener, pool, None, &serves_its_own, (started, None, Listened::default()));
        drop(ran.recv());
    }
    super::PAUSE_BEFORE_KEPT.store(0, Ordering::Release);
    assert_eq!(went_on.load(Ordering::SeqCst), 0, "rounds went on uncancelled after their servers ended");
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

/// A round sent "wait" waits on a socket of its own, a plain one connected to `to`, through a copy
/// of it: first with 20 ms for something to read, then, having said it's waiting, with 30 s. It
/// answers with what each wait came to and how many bytes it then read. Any other line is echoed.
fn waits_on_a_copy_or_echoes(to: SocketAddr) -> Handler {
    Arc::new(move |conn: Conn| {
        let Ok(Some(mut line)) = conn.read_line(None) else { return 1 };
        if line != b"wait" {
            line.push(b'\n');
            return i64::from(conn.write(&line).is_err());
        }
        let own = std::net::TcpStream::connect(to).unwrap();
        own.set_nonblocking(true).unwrap();
        let wait = |within: Duration| {
            let by = Some(std::time::Instant::now() + within);
            super::park_on_copy(super::next_token(), &own, mio::Interest::READABLE, by)
        };
        let quiet = wait(Duration::from_millis(20));
        if conn.write(b"waiting\n").is_err() {
            return 1;
        }
        let sent = wait(Duration::from_secs(30));
        let mut bytes = [0_u8; 16];
        let read = std::io::Read::read(&mut &own, &mut bytes).unwrap_or(0);
        i64::from(conn.write(format!("{quiet:?} {sent:?} {read}\n").as_bytes()).is_err())
    })
}

/// A task's wait on a plain socket through a copy of it, which is how every such wait goes on
/// Windows, parks the task: its worker, the only one, answers another connection meanwhile. The
/// wait says when its time's up, and when the socket has something to read, and the socket is
/// still open once its copy is closed. On Windows such a wait blocked its worker, so a program
/// that was its own client waited for ever on one thread.
#[test]
fn a_wait_on_a_copy_of_a_socket_parks_its_task_and_says_when_the_socket_is_ready() {
    let between = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let to = between.local_addr().unwrap();
    let served = run(1, &waits_on_a_copy_or_echoes(to), move |address| {
        let mut waiting = std::net::TcpStream::connect(address).unwrap();
        waiting.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        let mut heard = BufReader::new(waiting.try_clone().unwrap());
        waiting.write_all(b"wait\n").unwrap();
        let (mut other_end, _) = between.accept().unwrap();
        let mut said = String::new();
        heard.read_line(&mut said).unwrap();
        assert_eq!(said, "waiting\n");
        // Its round goes from saying so straight into its wait, and only a round that's parked
        // lets another run on the one worker.
        assert_eq!(ask(address, "meanwhile").as_deref(), Some("meanwhile\n"));
        other_end.write_all(b"now\n").unwrap();
        said.clear();
        heard.read_line(&mut said).unwrap();
        assert_eq!(said, "TimedOut Ready 4\n");
        // Its round over, the socket's closed: a copy left open would hold the connection, and
        // this read would wait out its time.
        other_end.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut rest = Vec::new();
        assert_eq!(std::io::Read::read_to_end(&mut other_end, &mut rest).ok(), Some(0), "{rest:?}");
    });
    assert_eq!((served.connections, served.failed), (2, 0));
}

/// A round sent "wait" says it's waiting, then waits 50 ms on a descriptor the worker's poll won't
/// take, as there's none with that number, looking again each time the wait says to. It says how
/// the wait ended and how many times it looked. Any other line is echoed.
#[cfg(unix)]
fn waits_on_what_the_poll_wont_take() -> Handler {
    Arc::new(|conn: Conn| {
        let Ok(Some(mut line)) = conn.read_line(None) else { return 1 };
        if line != b"wait" {
            line.push(b'\n');
            return i64::from(conn.write(&line).is_err());
        }
        if conn.write(b"waiting\n").is_err() {
            return 1;
        }
        let by = Some(std::time::Instant::now() + Duration::from_millis(50));
        let mut looks = 0;
        let ended = loop {
            match super::park_on_fd(super::next_token(), -1, mio::Interest::READABLE, by) {
                super::Parked::Ready if looks < 100_000 => looks += 1,
                ended => break ended,
            }
        };
        i64::from(conn.write(format!("{ended:?} {looks}\n").as_bytes()).is_err())
    })
}

/// A wait on a socket that the worker's poll refuses parks a moment and says to look again, until
/// its time runs out, and its worker goes on with others meanwhile. It used to say "ready" at
/// once: its task looked again and again without the worker ever polling, and a wait with a
/// deadline never ran out of time. That's 100,000 looks here and no end, where it's now 50 at most.
#[cfg(unix)]
#[test]
fn a_wait_the_poll_refuses_parks_a_moment_at_a_time_and_still_runs_out_of_time() {
    let served = run(1, &waits_on_what_the_poll_wont_take(), |address| {
        let mut waiting = std::net::TcpStream::connect(address).unwrap();
        waiting.write_all(b"wait\n").unwrap();
        let mut heard = BufReader::new(waiting.try_clone().unwrap());
        let mut said = String::new();
        heard.read_line(&mut said).unwrap();
        assert_eq!(said, "waiting\n");
        // Its round goes from saying so straight into its wait, and only a round that's parked
        // lets another run on the one worker.
        assert_eq!(ask(address, "meanwhile").as_deref(), Some("meanwhile\n"));
        said.clear();
        heard.read_line(&mut said).unwrap();
        let (ended, looks) = said.trim_end().split_once(' ').unwrap();
        assert_eq!(ended, "TimedOut", "after {looks} looks");
        assert!(looks.parse::<usize>().unwrap() <= 60, "it looked {looks} times in 50 ms");
    });
    assert_eq!((served.connections, served.failed), (2, 0));
}

/// A round sent "wait" says it's waiting, then waits on a socket of its own, a plain one connected
/// to `to`, through a copy of it, for as long as it takes. Whatever ends the wait, it writes on the
/// socket and waits again, and notes in `came` what the two waits came to and whether it could
/// write. Any other line is echoed.
fn waits_on_a_copy_until_its_ended(to: SocketAddr, came: &Arc<std::sync::Mutex<Vec<String>>>) -> Handler {
    let came = Arc::clone(came);
    Arc::new(move |conn: Conn| {
        let Ok(Some(mut line)) = conn.read_line(None) else { return 1 };
        if line != b"wait" {
            line.push(b'\n');
            return i64::from(conn.write(&line).is_err());
        }
        let own = std::net::TcpStream::connect(to).unwrap();
        own.set_nonblocking(true).unwrap();
        if conn.write(b"waiting\n").is_err() {
            return 1;
        }
        let first = super::park_on_copy(super::next_token(), &own, mio::Interest::READABLE, None);
        let wrote = (&own).write_all(b"still here\n").is_ok();
        let again = super::park_on_copy(super::next_token(), &own, mio::Interest::READABLE, None);
        came.lock().unwrap().push(format!("{first:?} {again:?} {wrote}"));
        0
    })
}

/// A wait on a copy of a socket that's cancelled partway, as a round's is when its server stops,
/// gives its copy up like any other: the wait says it was cancelled, the socket is still its
/// round's to write on, and when the round ends the socket closes, which it couldn't with a copy
/// of it left open or left with the worker's poll. A wait begun after that is cancelled at once.
#[test]
fn a_wait_on_a_copy_thats_cancelled_partway_closes_its_copy_and_leaves_the_socket_open() {
    let between = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let to = between.local_addr().unwrap();
    let came = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (hand_over, handed) = std::sync::mpsc::channel();
    let served = run(1, &waits_on_a_copy_until_its_ended(to, &came), move |address| {
        let mut waiting = std::net::TcpStream::connect(address).unwrap();
        waiting.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        waiting.write_all(b"wait\n").unwrap();
        let (other_end, _) = between.accept().unwrap();
        let mut said = String::new();
        BufReader::new(waiting.try_clone().unwrap()).read_line(&mut said).unwrap();
        assert_eq!(said, "waiting\n");
        // Only a round that's parked lets another run on the one worker: it's in its wait.
        assert_eq!(ask(address, "meanwhile").as_deref(), Some("meanwhile\n"));
        hand_over.send((other_end, waiting)).unwrap();
        // The clients are done, so the server's stopped now, with that round still waiting.
    });
    let (mut other_end, _waiting) = handed.recv().unwrap();
    assert_eq!(*came.lock().unwrap(), ["Cancelled Cancelled true"]);
    // What the round wrote after its wait was cancelled, and then the socket's end.
    other_end.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut heard = Vec::new();
    let read = std::io::Read::read_to_end(&mut other_end, &mut heard).ok();
    assert_eq!((read, heard.as_slice()), (Some(11), b"still here\n".as_slice()));
    assert_eq!(served.connections, 2);
}

/// A round sent "fill" writes on a socket of its own, a plain one connected to `to` and made not
/// to block, until a write would block, and says how much went. Then it waits for room through a
/// copy of the socket, writes a last line, and says what the wait came to and whether the line
/// went. Any other line is echoed.
fn fills_then_waits_for_room_on_a_copy(to: SocketAddr) -> Handler {
    Arc::new(move |conn: Conn| {
        let Ok(Some(mut line)) = conn.read_line(None) else { return 1 };
        if line != b"fill" {
            line.push(b'\n');
            return i64::from(conn.write(&line).is_err());
        }
        let own = std::net::TcpStream::connect(to).unwrap();
        own.set_nonblocking(true).unwrap();
        let piece = vec![7_u8; 64 * 1024];
        let mut sent = 0;
        loop {
            match (&own).write(&piece) {
                Ok(wrote) => sent += wrote,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => return 1,
            }
        }
        if conn.write(format!("{sent}\n").as_bytes()).is_err() {
            return 1;
        }
        let by = Some(std::time::Instant::now() + Duration::from_secs(30));
        let room = super::park_on_copy(super::next_token(), &own, mio::Interest::WRITABLE, by);
        own.set_nonblocking(false).unwrap();
        let last = (&own).write_all(b"last\n").is_ok();
        i64::from(conn.write(format!("{room:?} {last}\n").as_bytes()).is_err())
    })
}

/// A task's wait for room to write, through a copy of its socket, parks too, and ends when the
/// other side has read enough: the first time a write can wait this way on Windows, where a
/// task's socket used to block. The round fills its socket until a write would block, waits while
/// its worker answers another connection, and is woken once the other end reads what was sent.
#[test]
fn a_wait_for_room_on_a_copy_of_a_socket_parks_and_ends_when_the_other_side_reads() {
    let between = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let to = between.local_addr().unwrap();
    let served = run(1, &fills_then_waits_for_room_on_a_copy(to), move |address| {
        let mut filling = std::net::TcpStream::connect(address).unwrap();
        filling.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        let mut heard = BufReader::new(filling.try_clone().unwrap());
        filling.write_all(b"fill\n").unwrap();
        let (mut other_end, _) = between.accept().unwrap();
        other_end.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        let mut said = String::new();
        heard.read_line(&mut said).unwrap();
        let sent: usize = said.trim().parse().unwrap();
        assert!(sent > 0, "{said:?}");
        // Nothing's been read, so there's no room: the round is parked, and its worker is free.
        assert_eq!(ask(address, "meanwhile").as_deref(), Some("meanwhile\n"));
        let mut taken = vec![0_u8; sent];
        std::io::Read::read_exact(&mut other_end, &mut taken).unwrap();
        said.clear();
        heard.read_line(&mut said).unwrap();
        assert_eq!(said, "Ready true\n");
        let mut rest = Vec::new();
        assert_eq!(std::io::Read::read_to_end(&mut other_end, &mut rest).ok(), Some(5));
        assert_eq!(rest, b"last\n");
    });
    assert_eq!((served.connections, served.failed), (2, 0));
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
    // server runs on the pool the `parallel` tests share, so it gives the one they all give.
    super::super::parallel::set_test_worker_setup();
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
        // Closed however the talk went, so a failure can't leave the server running.
        super::close_when_served(server);
        talked.unwrap();
    });
    assert_eq!(super::net_serve(server, echo_body, 0), 0);
    client.join().unwrap();
    // A server can be served once: it's no longer open to serve again.
    assert_eq!(super::net_serve(server, echo_body, 0), 1);
    finish();
}

/// A served connection's body that reads each request's head with `_read_head`, within 40
/// bytes, 3 header lines and 2 blank lines before it, and sends back its lines joined by `|`, or
/// the case it was, until the head says the stream ended.
extern "C" fn heads_body(_env: i64, conn: i64, _thread: i64) -> i64 {
    use crate::value::Value;
    let span = fennec_syntax::Span::default();
    let limits = [Value::Int(40), Value::Int(3), Value::Int(2)];
    loop {
        let Some(Ok(Value::Ok(head))) = super::served_call(conn, "_read_head", &limits, None, span) else {
            return 1;
        };
        let Value::Instance(head) = head.as_ref() else { return 1 };
        let case = head.case.as_deref().unwrap_or("").to_owned();
        let said = match head.field("lines") {
            Some(Value::List(lines)) => {
                let text = |line: &Value| match line {
                    Value::Str(text) => text.to_string(),
                    other => format!("not a str: {other:?}"),
                };
                let lines: Vec<String> = lines.iter().map(text).collect();
                lines.join("|")
            }
            _ => case.clone(),
        };
        let written = super::served_call(conn, "write", &[Value::str(&format!("{said}\n"))], None, span);
        assert!(matches!(written, Some(Ok(Value::Ok(_)))));
        if case == "Ended" {
            return 0;
        }
    }
}

#[test]
fn a_served_connection_reads_each_head_in_one_call_and_leaves_what_follows() {
    use super::super::{Tables, finish, start};
    use crate::value::Value;
    super::super::parallel::set_test_worker_setup();
    start(Tables::default(), crate::Inputs::default());
    let span = fennec_syntax::Span::default();
    let listened = crate::stdlib::net::real_net("_listen", &[Value::Int(0)], None, span).unwrap();
    let Value::Ok(server) = listened else { panic!("not listening: {listened:?}") };
    let Value::Instance(instance) = server.as_ref() else { panic!("no server") };
    let (Some(Value::Int(port)), Some(Value::Int(server))) =
        (instance.field("port"), instance.field("_handle"))
    else {
        panic!("no port")
    };
    let (port, server) = (*port, *server);
    let client = std::thread::spawn(move || {
        let talked = std::panic::catch_unwind(|| {
            let address: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
            let mut stream = std::net::TcpStream::connect(address).unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            // Two heads in one write: the second stays held for the next call. Then one with a
            // blank line before it, one over the most header lines, and the end.
            stream
                .write_all(
                    b"GET / HTTP/1.1\r\nhost: a\r\n\r\nGET /b HTTP/1.1\r\n\r\n\r\nPUT /c HTTP/1.1\r\n\r\n",
                )
                .unwrap();
            stream.write_all(b"G\r\na\r\nb\r\nc\r\nd\r\n\r\n").unwrap();
            let mut said = Vec::new();
            for _ in 0..4 {
                let mut back = String::new();
                reader.read_line(&mut back).unwrap();
                said.push(back);
            }
            assert_eq!(
                said,
                ["GET / HTTP/1.1|host: a\n", "GET /b HTTP/1.1\n", "PUT /c HTTP/1.1\n", "TooBig\n"]
            );
            stream.shutdown(std::net::Shutdown::Write).unwrap();
            let mut last = String::new();
            reader.read_line(&mut last).unwrap();
            assert_eq!(last, "Ended\n");
        });
        super::close_when_served(server);
        talked.unwrap();
    });
    assert_eq!(super::net_serve(server, heads_body, 0), 0);
    client.join().unwrap();
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
    assert!(super::calls::conn_of(on_other_worker).is_none());
}

/// Running out of file descriptors leaves connections waiting that the poll won't announce again,
/// so it ends a connection waiting for its next message to make room, and running out of memory
/// looks again soon. A connection that gave up before it was accepted leaves the rest waiting too.
#[test]
fn a_failure_to_accept_doesnt_strand_what_waits_behind_it() {
    use std::io::{Error, ErrorKind};

    use super::{AfterAccept, accept_failed};
    assert_eq!(accept_failed(&Error::from(ErrorKind::ConnectionAborted)), AfterAccept::Next);
    assert_eq!(accept_failed(&Error::from(ErrorKind::Interrupted)), AfterAccept::Next);
    assert_eq!(accept_failed(&Error::from_raw_os_error(24)), AfterAccept::EndIdle);
    assert_eq!(accept_failed(&Error::from_raw_os_error(23)), AfterAccept::EndIdle);
    assert_eq!(accept_failed(&Error::from(ErrorKind::OutOfMemory)), AfterAccept::Later);
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
        waiting.insert(token, (u64::try_from(token).unwrap(), mio::Interest::READABLE, deadline, None, None));
    }
    let woken = super::timed_out(&waiting, now);
    let wanted: Vec<usize> =
        (0..64).filter(|token| token % 4 == 1).chain((0..64).filter(|token| token % 4 == 0)).collect();
    assert_eq!(woken, wanted);
}

/// What `count` threads reading lines from one connection at once got between them, of `lines`
/// numbered lines a client sent: how many weren't a whole line, how many were never read, how many
/// were read twice, and how many threads panicked.
fn read_by(count: usize, lines: usize) -> (usize, usize, usize, usize) {
    use std::fmt::Write as _;
    let (listener, address) = listener();
    let client = std::thread::spawn(move || {
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        let all = (0..lines).fold(String::new(), |mut all, n| {
            let _ = writeln!(all, "L {n} pppppppppppppppp");
            all
        });
        stream.write_all(all.as_bytes()).unwrap();
    });
    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(_) => std::thread::yield_now(),
        }
    };
    let conn = Arc::new(Conn::new(stream));
    let readers: Vec<_> = (0..count)
        .map(|_| {
            let conn = Arc::clone(&conn);
            std::thread::spawn(move || {
                let mut got = Vec::new();
                while let Ok(Some(line)) = conn.read_line(Some(Duration::from_secs(20))) {
                    got.push(line);
                }
                got
            })
        })
        .collect();
    client.join().unwrap();
    let mut seen = vec![0_u32; lines];
    let (mut torn, mut panicked) = (0, 0);
    for reader in readers {
        let Ok(got) = reader.join() else {
            panicked += 1;
            continue;
        };
        for line in got {
            let text = String::from_utf8_lossy(&line).into_owned();
            let mut parts = text.split(' ');
            let number = match (parts.next(), parts.next(), parts.next(), parts.next()) {
                (Some("L"), Some(number), Some("pppppppppppppppp"), None) => number.parse::<usize>().ok(),
                _ => None,
            };
            match number.and_then(|number| seen.get_mut(number)) {
                Some(times) => *times += 1,
                None => torn += 1,
            }
        }
    }
    let missing = seen.iter().filter(|times| **times == 0).count();
    let twice = seen.iter().filter(|times| **times > 1).count();
    (torn, missing, twice, panicked)
}

/// Two threads read lines from one connection at once, as two tasks on two workers may: every line
/// is read once, whole, by one of them. With the read-ahead locked for each piece of a read rather
/// than for the whole of it, lines were lost and torn, and a reader panicked (Fable 5.2's review
/// of #2081: 0 of 5 rounds right).
#[test]
fn two_threads_reading_one_connection_get_every_line_once_whole() {
    let mut wrong = Vec::new();
    for round in 0..5 {
        let (torn, missing, twice, panicked) = read_by(2, 200_000);
        if (torn, missing, twice, panicked) != (0, 0, 0, 0) {
            wrong.push((round, torn, missing, twice, panicked));
        }
    }
    assert!(wrong.is_empty(), "(round, torn, missing, twice, panicked): {wrong:?}");
}

/// Two threads each write 20,000 lines of 60 bytes to one connection, each line one `write` that
/// goes out in one piece: every line arrives whole.
#[test]
fn two_threads_writing_short_lines_to_one_connection_keep_each_whole() {
    use std::io::Read as _;
    let (listener, address) = listener();
    let client = std::thread::spawn(move || {
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        let mut all = Vec::new();
        stream.read_to_end(&mut all).unwrap();
        all
    });
    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(_) => std::thread::yield_now(),
        }
    };
    let conn = Arc::new(Conn::new(stream));
    let writers: Vec<_> = b"ab"
        .iter()
        .copied()
        .map(|letter| {
            let conn = Arc::clone(&conn);
            std::thread::spawn(move || {
                let mut message = vec![letter; 59];
                message.push(b'\n');
                (0..20_000).filter(|_| conn.write(&message).is_ok()).count()
            })
        })
        .collect();
    let wrote: Vec<usize> = writers.into_iter().map(|writer| writer.join().unwrap_or(0)).collect();
    conn.end();
    let all = client.join().unwrap();
    let lines: Vec<&[u8]> = all.split(|byte| *byte == b'\n').filter(|line| !line.is_empty()).collect();
    let cut = lines
        .iter()
        .filter(|line| line.len() != 59 || line.iter().any(|byte| Some(byte) != line.first()))
        .count();
    assert_eq!((wrote, lines.len(), cut), (vec![20_000, 20_000], 40_000, 0));
}
