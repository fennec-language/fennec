//! The calls native code makes for `for conn in parallel(server)` and a served connection's
//! methods. Values cross as temporaries, the interpreter's values, as the calls into the
//! interpreter's standard library give them: each method gives back what the interpreter's
//! does, `Ok(...)` or `Err(NetError...)`.
//!
//! A served connection is a number native code holds, looked up in its I/O worker's table of
//! connections, so a number the runtime didn't give out reads as a closed connection. A call
//! takes its connection out of the table while it runs, so a wait that parks holds nothing
//! borrowed: not the table, and not the runtime's state.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::num::NonZero;
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};

use fennec_syntax::{Diagnostic, Span};

use super::super::{State, attempt, threads, with};
use super::{Conn, Failure, Handler, Stopper};
use crate::stdlib::net::Failure as NetFailure;
use crate::value::Value;

/// What runs each served connection: `body(env, conn, thread)`, giving back 0, or anything else
/// when it failed, with the failure in its thread's runtime, which ends only that connection.
pub type ConnBody = extern "C" fn(env: i64, conn: i64, thread: i64) -> i64;

thread_local! {
    /// The connections this I/O worker serves, by the number native code holds.
    static CONNS: RefCell<HashMap<i64, Conn>> = RefCell::new(HashMap::new());
    static NEXT: Cell<i64> = const { Cell::new(1) };
}

/// The servers running, so they can be stopped.
static STOPPERS: Mutex<Vec<Stopper>> = Mutex::new(Vec::new());

/// Stops every server running: each cancels its connections' tasks and returns.
pub fn stop_servers() {
    for stopper in std::mem::take(&mut *STOPPERS.lock().unwrap_or_else(PoisonError::into_inner)) {
        stopper.stop();
    }
}

/// Fails the call with `message`, in this thread's runtime: 1.
fn fail(message: String) -> i64 {
    attempt(|_: &mut State| -> Result<i64, Diagnostic> { Err(Diagnostic::new(message, Span::default())) })
        .max(1)
}

/// `fennec_net_serve(server, body, env) -> i64`: `for conn in parallel(server)`, `server` a
/// temporary holding the `Server`. Each connection runs `body` as a parked task on one of the
/// I/O workers, one per core, this thread among them. It returns once the server is stopped,
/// giving back 0, or 1 when the server couldn't be served.
pub extern "C" fn net_serve(server: i64, body: ConnBody, env: i64) -> i64 {
    let listener = with(|state| {
        let server = state.take(server);
        crate::stdlib::net::take_listener(&server)
    });
    let Some(listener) = listener else { return fail("this server isn't open to serve".to_owned()) };
    if let Err(error) = listener.set_nonblocking(true) {
        return fail(format!("couldn't serve: {error}"));
    }
    let listener = mio::net::TcpListener::from_std(listener);
    let handler: Handler = Arc::new(move |conn: Conn| {
        let handle = NEXT.get();
        NEXT.set(handle + 1);
        CONNS.with_borrow_mut(|conns| conns.insert(handle, conn));
        let result = body(env, handle, i64::from(threads::index()));
        // Its task is done with it: dropping it closes it.
        CONNS.with_borrow_mut(|conns| conns.remove(&handle));
        result
    });
    // Other I/O workers need runtimes of their own; without a way to start them, this thread
    // serves alone.
    let workers = if super::super::parallel::has_worker_setup() {
        std::thread::available_parallelism().map_or(1, NonZero::get)
    } else {
        1
    };
    let setup: Arc<dyn Fn() + Send + Sync> = Arc::new(super::super::parallel::run_worker_setup);
    let started = |stopper| STOPPERS.lock().unwrap_or_else(PoisonError::into_inner).push(stopper);
    match super::serve(listener, workers, &setup, &handler, started) {
        Ok(_) => 0,
        Err(error) => fail(format!("couldn't serve: {error}")),
    }
}

/// Runs `call` on a served connection taken out of the table, so nothing stays borrowed while
/// it waits, and puts it back. A connection that isn't there is closed.
fn on_conn<T>(conn: i64, call: impl FnOnce(&mut Conn) -> Result<T, Failure>) -> Result<T, Failure> {
    let Some(mut taken) = CONNS.with_borrow_mut(|conns| conns.remove(&conn)) else {
        return Err(Failure::Closed);
    };
    let result = call(&mut taken);
    CONNS.with_borrow_mut(|conns| conns.insert(conn, taken));
    result
}

/// What a connection's method gives back, as a temporary: `Ok(value)`, or `Err(NetError)`.
fn answer(found: Result<Value, Failure>) -> i64 {
    with(|state| {
        let value = match found {
            Ok(value) => Value::Ok(Rc::new(value)),
            Err(failure) => {
                let failure = match failure {
                    Failure::TimedOut(after) => NetFailure::TimedOut(after),
                    Failure::Closed => NetFailure::Closed,
                    Failure::Cancelled => NetFailure::Other("the server stopped".to_owned()),
                    Failure::Other(reason) => NetFailure::Other(reason),
                };
                state.interp.net_error(&failure, Span::default()).unwrap_or(Value::None)
            }
        };
        state.push(value)
    })
}

/// `fennec_conn_read(conn, count) -> temp`: `conn.read(count)`, up to `count` bytes, empty at
/// the end.
pub extern "C" fn conn_read(conn: i64, count: i64) -> i64 {
    let count = usize::try_from(count).unwrap_or(0);
    answer(on_conn(conn, |conn| conn.read(count, None)).map(|data| Value::Bytes(Rc::from(data))))
}

/// `fennec_conn_read_line(conn) -> temp`: `conn.read_line()`, the next line, or `None` at the
/// end.
pub extern "C" fn conn_read_line(conn: i64) -> i64 {
    let line = on_conn(conn, |conn| conn.read_line(None));
    answer(line.map(|line| line.map_or(Value::None, |line| Value::str(&String::from_utf8_lossy(&line)))))
}

/// `fennec_conn_write(conn, data) -> temp`: `conn.write(data)`, `data` a temporary holding a
/// `str` or `bytes`.
pub extern "C" fn conn_write(conn: i64, data: i64) -> i64 {
    let data = with(|state| match state.take(data) {
        Value::Str(text) => text.as_bytes().to_vec(),
        Value::Bytes(data) => data.to_vec(),
        _ => Vec::new(),
    });
    answer(on_conn(conn, |conn| conn.write(&data)).map(|()| Value::Unit))
}

/// `fennec_conn_peer(conn) -> temp`: `conn.peer`, the other side's address.
pub extern "C" fn conn_peer(conn: i64) -> i64 {
    let peer = CONNS.with_borrow(|conns| conns.get(&conn).map(Conn::peer)).unwrap_or_default();
    with(|state| state.push(Value::str(&peer)))
}

/// `fennec_conn_close(conn)`: closes a served connection early. Its task ending closes it too.
pub extern "C" fn conn_close(conn: i64) {
    CONNS.with_borrow_mut(|conns| conns.remove(&conn));
}
