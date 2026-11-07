//! The calls native code makes for `for conn in parallel(server)` and a served connection's
//! methods, which give back what a real connection's do, `Ok(...)` or `Err(NetError...)`.
//!
//! A served connection is a number native code holds, looked up in its I/O worker's table of
//! connections, so a number the runtime didn't give out reads as a closed connection. A call
//! takes its connection out of the table while it runs, so a wait that parks holds nothing
//! borrowed: not the table, and not the runtime's state.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
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

/// A round's failure, copied out of its worker's runtime for the code serving to take.
type Passed = Arc<Mutex<Option<(u64, Box<crate::embed::table::Table>)>>>;

thread_local! {
    /// The connections this I/O worker serves, by the number native code holds.
    static CONNS: RefCell<HashMap<i64, Conn>> = RefCell::new(HashMap::new());
    static NEXT: Cell<i64> = const { Cell::new(1) };
    /// The failure the last `fennec_net_serve` on this thread passed up, for
    /// `fennec_net_serve_failure`.
    static PASSED_UP: Cell<i64> = const { Cell::new(0) };
}

/// The servers running, so they can be stopped.
static STOPPERS: Mutex<Vec<Stopper>> = Mutex::new(Vec::new());

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
    let Some(listener) = crate::stdlib::net::take_listener(server) else {
        return fail("this server isn't open to serve".to_owned());
    };
    if let Err(error) = listener.set_nonblocking(true) {
        return fail(format!("couldn't serve: {error}"));
    }
    let listener = mio::net::TcpListener::from_std(listener);
    let passed: Passed = Arc::new(Mutex::new(None));
    let passing = Arc::clone(&passed);
    let handler: Handler = Arc::new(move |conn: Conn| {
        let handle = NEXT.get();
        NEXT.set(handle + 1);
        CONNS.with_borrow_mut(|conns| conns.insert(handle, conn));
        let result = body(env, handle, i64::from(threads::index()));
        // Its task is done with it: dropping it closes it.
        CONNS.with_borrow_mut(|conns| conns.remove(&handle));
        if result == super::ROUND_FAILED {
            pass_up(&passing);
        }
        result
    });
    let Some(pool) = super::super::parallel::for_server() else {
        return fail("couldn't serve: this program has no workers to serve on".to_owned());
    };
    let started = |stopper| STOPPERS.lock().unwrap_or_else(PoisonError::into_inner).push(stopper);
    super::super::write_kept_output();
    let lent = threads::lend();
    let finished = super::serve(listener, pool, Some(&lent), &handler, started);
    threads::take_back(lent);
    threads::settle(finished.owners);
    let failure = passed.lock().unwrap_or_else(PoisonError::into_inner).take();
    let Some((failure, table)) = failure else { return 0 };
    super::super::with(|state| state.heap.adopt_made(table.into_blocks()));
    PASSED_UP.set(i64::try_from(failure).unwrap_or(0));
    super::ROUND_FAILED
}

/// `fennec_net_serve_failure()`: the value of the failure the last `fennec_net_serve` on this thread
/// passed up, giving back 2, now this thread's own.
pub extern "C" fn net_serve_failure() -> i64 {
    PASSED_UP.take()
}

/// Copies the failure a round handed over out of its worker's runtime, for the code serving: the
/// first round to fail is the one whose failure passes up.
fn pass_up(passed: &Passed) {
    let Some(failure) = super::launch::take_failed_with() else { return };
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

/// Runs `call` on a served connection taken out of the table, so nothing stays borrowed while
/// it waits, and puts it back. A connection that isn't there is closed.
fn on_conn<T>(conn: i64, call: impl FnOnce(&mut Conn) -> Result<T, Failure>) -> Result<T, NetFailure> {
    let Some(mut taken) = CONNS.with_borrow_mut(|conns| conns.remove(&conn)) else {
        return Err(NetFailure::Closed);
    };
    let result = call(&mut taken);
    CONNS.with_borrow_mut(|conns| conns.insert(conn, taken));
    result.map_err(|failure| match failure {
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
    use crate::stdlib::net::{bytes_asked, data_of, done_or_failed, line_value};
    if !CONNS.with_borrow(|conns| conns.contains_key(&handle)) {
        return None;
    }
    let done = match (method, args) {
        ("read", [Value::Int(count)]) => match bytes_asked(*count, at) {
            Ok(count) => {
                on_conn(handle, |conn| conn.read(count, timeout)).map(|data| Value::Bytes(Rc::from(data)))
            }
            Err(wrong) => return Some(Err(wrong)),
        },
        ("read_line", []) => {
            on_conn(handle, |conn| conn.read_line(timeout)).map(|line| line_value(line.as_deref()))
        }
        ("write", [data @ (Value::Str(_) | Value::Bytes(_))]) => {
            on_conn(handle, |conn| conn.write(data_of(data))).map(|()| Value::Unit)
        }
        ("close", []) => {
            CONNS.with_borrow_mut(|conns| conns.remove(&handle));
            return Some(Ok(Value::Unit));
        }
        ("peer", []) => {
            let peer = CONNS.with_borrow(|conns| conns.get(&handle).map(Conn::peer)).unwrap_or_default();
            return Some(Ok(Value::str(&peer)));
        }
        _ => {
            return Some(Err(Diagnostic::new(format!("`{method}` was called with the wrong arguments"), at)));
        }
    };
    Some(Ok(done_or_failed(done)))
}
