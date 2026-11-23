//! The calls native code makes for `for conn in parallel(server)` and a served connection's
//! methods, which give back what a real connection's do, `Ok(...)` or `Err(NetError...)`.
//!
//! A served connection is a number native code holds, looked up in its I/O worker's table of
//! connections, so a number the runtime didn't give out reads as a closed connection. A call
//! takes its connection out of the table while it runs, so a wait that parks holds nothing
//! borrowed: not the table, and not the runtime's state.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::io::Write as _;
use std::rc::Rc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use fennec_syntax::{Diagnostic, Span};

use super::super::{State, attempt, threads};
use super::{Conn, Failure, Handler, Stopper};
use crate::stdlib::net::Failure as NetFailure;
use crate::value::Value;

/// What runs each round of `for conn in parallel(server)`: `body(env, conn, thread)`. It gives back
/// 0 when the round ended normally, by `continue` too, and 1 when it crashed, with the failure in
/// its thread's runtime, which ends only that round. It gives back 2 when it failed, having handed
/// its failure's value over with `fennec_task_failed`, which stops the server and passes up out of
/// the loop, and 3 at a `break`, which stops the server taking connections while the rounds
/// running finish. Its connection closes when it ends, however it ends.
pub type ConnBody = extern "C" fn(env: i64, conn: i64, thread: i64) -> i64;

/// A connection's number to whatever's kept for it. The runtime counts the numbers out itself,
/// so no client chooses them, and they're hashed by one multiply rather than `SipHash`, which cost
/// a served request 1,400 instructions for its dozen lookups.
type ByHandle<V> = HashMap<i64, V, BuildHasherDefault<HandleHasher>>;

/// Hashes a connection's number by one multiply, which spreads numbers counted one after another
/// over the whole table.
#[derive(Default)]
struct HandleHasher(u64);

impl Hasher for HandleHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = (self.0.rotate_left(5) ^ u64::from(*byte)).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        }
    }

    fn write_i64(&mut self, number: i64) {
        self.0 = (self.0 ^ number.cast_unsigned()).wrapping_mul(0x9e_37_79_b9_7f_4a_7c_15);
    }
}

/// A round's failure, copied out of its worker's runtime for the code serving to take.
type Passed = Arc<Mutex<Option<(u64, Box<crate::embed::table::Table>)>>>;

thread_local! {
    /// The connections this I/O worker serves, by the number native code holds: each round's,
    /// found here first, with no lock.
    static CONNS: RefCell<ByHandle<Arc<Conn>>> = RefCell::new(ByHandle::default());
    /// The failure the last `fennec_net_serve` on this thread passed up, for
    /// `fennec_net_serve_failure`.
    static PASSED_UP: Cell<i64> = const { Cell::new(0) };
}

/// Every connection being served, by its number, for a task on another worker than the one serving
/// it: a `Connection` is `shared`, so one sent to a hub on another worker is written to there
/// (#2065). Each is held weakly: its round ending closes it, wherever else it's been sent.
static SERVED_CONNS: std::sync::LazyLock<Mutex<ByHandle<std::sync::Weak<Conn>>>> =
    std::sync::LazyLock::new(|| Mutex::new(ByHandle::default()));

/// How many connections `SERVED_CONNS` holds, kept beside it, so a call on a connection the program
/// opened itself, which is in neither table, looks no further while nothing is served, rather than
/// taking the table's lock to miss (Fable 5.2's review of #2081, finding 4).
static SERVED_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Puts `handle`'s connection in `SERVED_CONNS`, or takes it out, keeping `SERVED_COUNT` with it.
fn note_served(handle: i64, conn: Option<&Arc<Conn>>) {
    let mut served = SERVED_CONNS.lock().unwrap_or_else(PoisonError::into_inner);
    match conn {
        Some(conn) => drop(served.insert(handle, Arc::downgrade(conn))),
        None => drop(served.remove(&handle)),
    }
    SERVED_COUNT.store(served.len(), Ordering::Release);
}

/// The connection served with `handle`, on this worker or another, while its round runs.
pub(super) fn conn_of(handle: i64) -> Option<Arc<Conn>> {
    if let Some(here) = CONNS.with_borrow(|conns| conns.get(&handle).cloned()) {
        return Some(here);
    }
    // Its number reached this task after its round put it in the table, by a send or a call, so
    // the count says it's there.
    if SERVED_COUNT.load(Ordering::Acquire) == 0 {
        return None;
    }
    SERVED_CONNS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&handle)
        .and_then(std::sync::Weak::upgrade)
}

/// The number the next connection served gets. There's one count for the whole process, so a
/// number names one connection wherever it's sent.
static NEXT: AtomicI64 = AtomicI64::new(1);

pub(super) fn next_handle() -> i64 {
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// The servers running, so they can be stopped.
static STOPPERS: Mutex<Vec<Stopper>> = Mutex::new(Vec::new());

/// The servers being served, by the handle the program holds, so `server.close()` can end one: its
/// listener is taken out of the open sockets while it's served, where closing would look for it.
static SERVED: Mutex<Option<HashMap<i64, Stopper>>> = Mutex::new(None);

fn served<T>(use_them: impl FnOnce(&mut HashMap<i64, Stopper>) -> T) -> T {
    use_them(SERVED.lock().unwrap_or_else(PoisonError::into_inner).get_or_insert_with(HashMap::new))
}

/// Closes the server with this handle if it's being served, and says whether it was: it takes no
/// more connections, the rounds running finish, and the loop over it ends, as its docs say a
/// closed server's does. Without this, closing a server while a task served it did nothing, and
/// nothing inside the program could end the loop.
pub(crate) fn close_served(server: i64) -> bool {
    served(|served| served.get(&server).map(Stopper::close)).is_some()
}

/// Stops every server running: each cancels its connections' tasks and returns.
pub fn stop_servers() {
    for stopper in STOPPERS.lock().unwrap_or_else(PoisonError::into_inner).drain(..) {
        stopper.stop();
    }
}

fn fail(message: String) -> i64 {
    attempt(|_: &mut State| -> Result<i64, Diagnostic> { Err(Diagnostic::new(message, Span::default())) })
        .max(1)
}

/// `fennec_net_serve(server, body, env)`: serves the server with this handle on its I/O workers,
/// each connection running `body` as a parked task, until a round breaks or fails, or the servers
/// are stopped. The calling thread only waits, lending what it holds to the tasks as it does to a
/// `parallel` call's ranges. Gives back 0, 1 when the server couldn't be served, and 2 when a round
/// failed, its failure's value then given by `fennec_net_serve_failure`.
pub extern "C" fn net_serve(server: i64, body: ConnBody, env: i64) -> i64 {
    // In a recording or a replay, the call serving is a task, whose children are its rounds.
    let call = crate::replay::child();
    // A round already cancelled, as one that made way and was cancelled waiting its turn, serves
    // nothing: it fails as stopped here, which a recording notes. It made a server, and waited for
    // its own worker to leave it, which can't while it runs the round (#2004).
    if fennec_parked::is_cancelled() {
        if !super::super::failed() {
            super::fail_as_stopped();
        }
        crate::replay::stopped_in(crate::replay::StoppedIn::Serving);
        return 1;
    }
    if let Some(rounds) = call.as_ref().and_then(|call| crate::replay::taken_at(&call.name(), "accept")) {
        return replayed(rounds, body, env, call.as_ref());
    }
    let Some((listener, came_with)) = crate::stdlib::net::take_listener(server) else {
        return fail("this server isn't open to serve".to_owned());
    };
    if let Err(error) = listener.set_nonblocking(true) {
        return fail(format!("couldn't serve: {error}"));
    }
    let listener = mio::net::TcpListener::from_std(listener);
    // A task serving can't lend its worker's blocks, which other tasks there are using: it lends a
    // copy of what its rounds are given, in a table of its own.
    let in_task = super::super::parallel::here().is_some();
    let given = if in_task && env != 0 {
        let copied = attempt(|state: &mut State| {
            super::super::guarded::copied_for_task(state, env.cast_unsigned()).map(Some)
        });
        let Some((copy, table)) = copied else { return 1 };
        Some((copy, Arc::new(*table)))
    } else {
        None
    };
    let env = given.as_ref().map_or(env, |(copy, _)| copy.cast_signed());
    let passed: Passed = Arc::new(Mutex::new(None));
    let passing = Arc::clone(&passed);
    let handler: Handler = Arc::new(move |conn: Conn| {
        let handle = next_handle();
        let conn = Arc::new(conn);
        note_served(handle, Some(&conn));
        CONNS.with_borrow_mut(|conns| conns.insert(handle, conn));
        let result = body(env, handle, i64::from(threads::index()));
        // The failure it handed over is taken at once: lingering may park this task, and another
        // task on the thread, a task launched in a round say, starts by clearing what's handed over.
        let failure = if result == super::ROUND_FAILED { super::launch::take_failed_with() } else { None };
        // Its task is done with it: it ends gracefully, and dropping it closes it.
        note_served(handle, None);
        if let Some(conn) = CONNS.with_borrow_mut(|conns| conns.remove(&handle)) {
            conn.linger();
        }
        if let Some(failure) = failure {
            pass_up(&passing, failure);
        }
        result
    });
    let Some(pool) = super::super::parallel::for_server() else {
        return fail("couldn't serve: this program has no workers to serve on".to_owned());
    };
    let id = Cell::new(None);
    let started = |stopper: Stopper| {
        id.set(Some(stopper.id()));
        served(|served| served.insert(server, stopper.again()));
        STOPPERS.lock().unwrap_or_else(PoisonError::into_inner).push(stopper);
    };
    super::super::write_kept_output();
    let lent = match &given {
        Some((_, table)) => Arc::clone(table),
        None => threads::lend(),
    };
    let finished = super::serve(listener, pool, Some(&lent), &handler, (started, call.as_ref(), came_with));
    // Ended, it's nothing left to stop, or to close.
    STOPPERS.lock().unwrap_or_else(PoisonError::into_inner).retain(|stopper| Some(stopper.id()) != id.get());
    served(|served| served.remove(&server));
    if let Some((copy, table)) = given {
        // What the copy holds is this task's now, with what its rounds did to its owners.
        drop(lent);
        let table = Arc::try_unwrap(table).unwrap_or_default();
        super::super::with(|state| state.heap.adopt_made(table.into_blocks()));
        threads::settle(finished.owners);
        attempt(|state: &mut State| {
            super::super::heap::release(state, copy);
            Ok(())
        });
    } else {
        threads::take_back(lent);
        threads::settle(finished.owners);
    }
    ended_serving(&passed)
}

/// What a call serving gives back once its server has ended: what `passed_up` says, unless the
/// task was stopped, or the round cancelled, while it served. Then it stopped its server with it,
/// and fails as any wait cut short does, which a recording notes.
fn ended_serving(passed: &Passed) -> i64 {
    let ended = passed_up(passed);
    let cancelled = fennec_parked::is_cancelled();
    if !threads::stopped() && !cancelled {
        return ended;
    }
    let_go_of_passed_up();
    let cut = if cancelled {
        if !super::super::failed() {
            super::fail_as_stopped();
        }
        true
    } else {
        super::cut_short()
    };
    if cut {
        crate::replay::stopped_in(crate::replay::StoppedIn::Serving);
    }
    i64::from(cut)
}

/// Lets go of a round's failure that got as far as the call serving, when a stop comes first.
fn let_go_of_passed_up() {
    let failure = PASSED_UP.take().cast_unsigned();
    if failure != 0 {
        attempt(|state: &mut State| {
            super::super::heap::release(state, failure);
            Ok(())
        });
    }
}

/// What a call serving gives back once its server has ended: 2 with the failure a round passed
/// up, taken as this thread's own for `fennec_net_serve_failure` to give, and 0 with none.
fn passed_up(passed: &Passed) -> i64 {
    let failure = passed.lock().unwrap_or_else(PoisonError::into_inner).take();
    let Some((failure, table)) = failure else { return 0 };
    super::super::with(|state| state.heap.adopt_made(table.into_blocks()));
    PASSED_UP.set(i64::try_from(failure).unwrap_or(0));
    super::ROUND_FAILED
}

/// `net_serve` in a replay: the recorded rounds run again with no socket, each reading and writing
/// what its recording has, and the server stops once they've all ended, as if stopped there.
///
/// A task serving lends a copy of what its rounds are given, as `net_serve` does, and parks while
/// they run: blocking its thread, it held up the rounds handed to its own worker, and the replay
/// never ended (#1832).
fn replayed(rounds: u64, body: ConnBody, env: i64, call: Option<&crate::replay::Task>) -> i64 {
    let Some(call) = call else { return 0 };
    let in_task = super::super::parallel::here().is_some();
    let given = if in_task && env != 0 {
        let copied = attempt(|state: &mut State| {
            super::super::guarded::copied_for_task(state, env.cast_unsigned()).map(Some)
        });
        let Some((copy, table)) = copied else { return 1 };
        Some((copy, Arc::new(*table)))
    } else {
        None
    };
    let env = given.as_ref().map_or(env, |(copy, _)| copy.cast_signed());
    let passed: Passed = Arc::new(Mutex::new(None));
    let passing = Arc::clone(&passed);
    let replayed: super::Replayed = Arc::new(move |handle: i64| {
        let result = body(env, handle, i64::from(threads::index()));
        if result == super::ROUND_FAILED
            && let Some(failure) = super::launch::take_failed_with()
        {
            pass_up(&passing, failure);
        }
        result
    });
    let Some(pool) = super::super::parallel::for_server() else {
        return fail("couldn't serve: this program has no workers to serve on".to_owned());
    };
    super::super::write_kept_output();
    let lent = match &given {
        Some((_, table)) => Arc::clone(table),
        None => threads::lend(),
    };
    let finished = super::serve_replayed(rounds, pool, Some(&lent), (replayed, call));
    if let Some((copy, table)) = given {
        drop(lent);
        let table = Arc::try_unwrap(table).unwrap_or_default();
        super::super::with(|state| state.heap.adopt_made(table.into_blocks()));
        threads::settle(finished.owners);
        attempt(|state: &mut State| {
            super::super::heap::release(state, copy);
            Ok(())
        });
    } else {
        threads::take_back(lent);
        threads::settle(finished.owners);
    }
    super::super::write_kept_output();
    let _ = writeln!(std::io::stderr(), "replay: the recording ends here, so the server is stopped");
    let ended = passed_up(&passed);
    // Stopped while it served, as its recording says: it fails as stopped, as the run did.
    if crate::replay::stopped_in_as_recorded(crate::replay::StoppedIn::Serving) {
        let_go_of_passed_up();
        return 1;
    }
    ended
}

/// How many connections this worker is serving now.
pub(in crate::embed) fn connections() -> usize {
    CONNS.with_borrow(ByHandle::len)
}

/// `fennec_net_serve_failure()`: the value of the failure the last `fennec_net_serve` on this thread
/// passed up, giving back 2, now this thread's own.
pub extern "C" fn net_serve_failure() -> i64 {
    PASSED_UP.take()
}

/// Copies the failure a round handed over out of its worker's runtime, for the code serving: the
/// first round to fail is the one whose failure passes up.
fn pass_up(passed: &Passed, failure: u64) {
    attempt(|state: &mut State| {
        let mut slot = passed.lock().unwrap_or_else(PoisonError::into_inner);
        let copy =
            if slot.is_none() { Some(super::super::guarded::copied_for_task(state, failure)) } else { None };
        super::super::heap::release(state, failure);
        if let Some(copy) = copy {
            *slot = Some(copy?);
        }
        Ok(())
    });
}

/// Runs `call` on a served connection, held by its own `Arc` so nothing stays borrowed while it
/// waits, its failure as the network's.
fn on<T>(conn: &Conn, call: impl FnOnce(&Conn) -> Result<T, Failure>) -> Result<T, NetFailure> {
    call(conn).map_err(|failure| match failure {
        Failure::TimedOut(after) => NetFailure::TimedOut(after),
        Failure::Closed => NetFailure::Closed,
        Failure::Cancelled => NetFailure::Other("the server stopped".to_owned()),
        Failure::Other(reason) => NetFailure::Other(reason),
    })
}

/// A method of the connection this worker serves with this handle, or `None` when it serves none:
/// each waits by parking its task.
pub(in crate::embed) fn served_call(
    handle: i64,
    method: &str,
    args: &[Value],
    timeout: Option<Duration>,
    at: Span,
) -> Option<Result<Value, Diagnostic>> {
    use crate::stdlib::net::{bytes_asked, data_of, done_or_failed, trimmed_line_value};
    let conn = conn_of(handle)?;
    let done = match (method, args) {
        ("read", [Value::Int(count)]) => match bytes_asked(*count, at) {
            Ok(count) => on(&conn, |conn| conn.read(count, timeout)).map(|data| Value::Bytes(Rc::from(data))),
            Err(wrong) => return Some(Err(wrong)),
        },
        ("_room_for_body", [Value::Int(size)]) => {
            return Some(
                bytes_asked(*size, at)
                    .map(|size| Value::Bool(conn.body_held.take(size, crate::stdlib::net::MOST_BODIES_HELD))),
            );
        }
        ("_body", [Value::Int(size)]) => match bytes_asked(*size, at) {
            Ok(size) => on(&conn, |conn| conn.body(size, timeout)).map(|data| Value::Bytes(Rc::from(data))),
            Err(wrong) => return Some(Err(wrong)),
        },
        ("read_line", []) => {
            on(&conn, |conn| conn.read_line(timeout)).map(|line| trimmed_line_value(line.as_deref()))
        }
        ("read_line.crlf", [Value::Bool(crlf)]) => on(&conn, |conn| conn.read_line_crlf(timeout, *crlf))
            .map(|line| trimmed_line_value(line.as_deref())),
        // A served connection's line has its ending taken off already.
        ("_read_head", given) if let Some(limits) = crate::stdlib::net::head::Limits::of(given) => {
            crate::stdlib::net::head::read_lines(limits, || {
                let line = on(&conn, |conn| conn.read_line_crlf(timeout, true))?;
                Ok(line.map(|line| String::from_utf8_lossy(&line).into_owned()))
            })
        }
        ("write", [data @ (Value::Str(_) | Value::Bytes(_))]) => {
            on(&conn, |conn| conn.write_within(data_of(data), timeout)).map(|()| Value::Unit)
        }
        ("_reads_within", [after]) => {
            let after = match crate::stdlib::net::duration_of(after, at) {
                Ok(after) => after,
                Err(wrong) => return Some(Err(wrong)),
            };
            conn.reads_within(after);
            return Some(Ok(Value::Unit));
        }
        ("close", []) => {
            // Closed by its own round, it's taken out of its worker's table. Closed by a task on
            // another worker, it ends there too: its round, reading, then finds the end.
            note_served(handle, None);
            match CONNS.with_borrow_mut(|conns| conns.remove(&handle)) {
                Some(here) => here.linger(),
                None => conn.end(),
            }
            return Some(Ok(Value::Unit));
        }
        ("draining", []) => {
            let draining = conn.draining();
            return Some(Ok(Value::Bool(draining)));
        }
        ("peer", []) => {
            let peer = conn.peer();
            return Some(Ok(Value::str(&peer)));
        }
        // The label a task serving the connection makes its `Connection` with.
        ("_label", []) => {
            let label = conn.label();
            return Some(Ok(Value::Int(label)));
        }
        _ => {
            return Some(Err(Diagnostic::new(format!("`{method}` was called with the wrong arguments"), at)));
        }
    };
    Some(Ok(done_or_failed(done)))
}

/// `conn.write(data)` on the connection served with `handle`, or `None` when none is: the typed
/// calls' (`embed::conn_calls`) way to what [`served_call`] does by value.
pub(in crate::embed) fn served_write(
    handle: i64,
    data: &[u8],
    timeout: Option<Duration>,
) -> Option<Result<(), NetFailure>> {
    conn_of(handle).map(|conn| on(&conn, |conn| conn.write_within(data, timeout)))
}

/// `conn._reads_within(after)` on the connection served with `handle`, as [`served_write`].
pub(in crate::embed) fn served_reads_within(handle: i64, after: Duration) -> Option<()> {
    conn_of(handle).map(|conn| conn.reads_within(after))
}

/// `conn.draining()` on the connection served with `handle`, as [`served_write`].
pub(in crate::embed) fn served_draining(handle: i64) -> Option<bool> {
    conn_of(handle).map(|conn| conn.draining())
}

/// `conn._room_for_body(size)` on the connection served with `handle`, as [`served_write`].
pub(in crate::embed) fn served_room_for_body(handle: i64, size: usize) -> Option<bool> {
    conn_of(handle).map(|conn| conn.body_held.take(size, crate::stdlib::net::MOST_BODIES_HELD))
}

/// `conn._body(size)` on the connection served with `handle`, as [`served_write`].
pub(in crate::embed) fn served_body(
    handle: i64,
    size: usize,
    timeout: Option<Duration>,
) -> Option<Result<Vec<u8>, NetFailure>> {
    conn_of(handle).map(|conn| on(&conn, |conn| conn.body(size, timeout)))
}

/// `conn._read_head(...)` on the connection served with `handle`, as [`served_write`]. A served
/// connection's line has its ending taken off already.
pub(in crate::embed) fn served_head(
    handle: i64,
    limits: crate::stdlib::net::head::Limits,
    timeout: Option<Duration>,
) -> Option<Result<crate::stdlib::net::head::Head, NetFailure>> {
    let conn = conn_of(handle)?;
    Some(crate::stdlib::net::head::read_head(limits, || {
        let line = on(&conn, |conn| conn.read_line_crlf(timeout, true))?;
        Ok(line.map(|line| String::from_utf8_lossy(&line).into_owned()))
    }))
}
