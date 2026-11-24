//! A served connection's per-request calls as exports of their own, which take their arguments as
//! words and give back words or blocks: `fennec_conn_*`, where `fennec_socket_call` hands over a
//! method's name and its arguments as values, and gives back a value. Each call `std.net` makes for
//! a request crossed by name at about 2,000 instructions (the runtime lane's
//! `typed-socket-exports-plan-2026-11-23.md`). The rest of a connection's methods stay on
//! `fennec_socket_call`.
//!
//! - **One body.** Each export runs on a served connection what `served_call` runs, the same
//!   methods of its `Conn`, by `serve::served_*`, with nothing borrowed while it waits, so a wait
//!   parks its task as before. A connection no worker serves, one the program opened or any in a
//!   program without threads, goes where `fennec_socket_call` sends it, with its arguments made
//!   values there.
//! - **A recording or a replay sees what it saw.** While one runs, each call is recorded and
//!   replayed under the name `fennec_socket_call` gives it, `Connection.` and the method, with
//!   `.timeout` when it was given one, and as the value it gives, so a recording is the same byte
//!   for byte whichever way the program called. Only then are the name and the value made.
//! - **A test's fake network** answers a call made either way, as it does `fennec_socket_call`'s.
//! - **A failure** is the network's `Err` value, as `fennec_socket_call` would give it, kept as a
//!   temporary: an export gives back `-1 - n` for the temporary `n` that holds it, and native code
//!   takes it from there as it takes a named call's. Every other answer is 0 or more.

use std::rc::Rc;
use std::time::Duration;

use fennec_syntax::{Diagnostic, Span};

use super::serve::{
    served_body, served_draining, served_head, served_reads_within, served_room_for_body, served_write,
};
use super::{fail_with, span, with};
use crate::stdlib::net::head::{Head, Limits};
use crate::stdlib::net::{Failure, bytes_asked, done_or_failed, failed};
use crate::value::Value;

/// `timeout_ns` for a call that wasn't given `timeout=`, which waits as long as it takes.
pub const NO_TIMEOUT: i64 = i64::MIN;

/// What a call gave: its answer, or the `Err` value it failed with.
enum Answer<T> {
    Gave(T),
    Failed(Value),
}

/// A call as `fennec_socket_call` would have been handed it: the method, how long it may wait if
/// it was given `timeout=`, and where it's called.
struct Call {
    method: &'static str,
    timeout: Option<Duration>,
    at: Span,
}

impl Call {
    fn new(method: &'static str, timeout_ns: i64, (start, end): (i64, i64)) -> Call {
        let timeout = (timeout_ns != NO_TIMEOUT).then(|| nanos(timeout_ns));
        Call { method, timeout, at: span(start, end) }
    }

    /// The call's name as `fennec_socket_call` is handed it, `timeout` named last.
    fn name(&self) -> String {
        let timed = if self.timeout.is_some() { ".timeout" } else { "" };
        format!("Connection.{}{timed}", self.method)
    }
}

/// A wait's length in nanoseconds, a negative one as none at all, as `Duration`'s value is read.
fn nanos(count: i64) -> Duration {
    Duration::from_nanos(u64::try_from(count).unwrap_or(0))
}

/// A `Duration` as a value, for the calls that take one as one.
fn duration_value(after: Duration) -> Value {
    let count = i64::try_from(after.as_nanos()).unwrap_or(i64::MAX);
    Value::made("Duration", vec![("nanos", Value::Int(count))])
}

/// How a call's answer is made the value `fennec_socket_call` would give, and taken back from one.
type AsValues<T> = (fn(Result<T, Failure>) -> Value, fn(Value) -> Option<T>);

/// Runs a call on the connection with this handle. `served` runs it on a connection a worker
/// serves, or gives `None` when none does, and in a normal run nothing else is made. Anything else
/// goes by value: `given` is the call's arguments without `timeout`, `as_value` makes what
/// `served` gave the value `fennec_socket_call` would have, and `of_value` takes such a value
/// back.
fn answered<T>(
    handle: i64,
    call: &Call,
    served: impl FnOnce() -> Option<Result<Result<T, Failure>, Diagnostic>>,
    given: impl Fn() -> Vec<Value>,
    (as_value, of_value): AsValues<T>,
) -> Result<Answer<T>, Diagnostic> {
    if let Some(faked) = faked(handle, call, &given) {
        return taken(call, faked?, of_value);
    }
    super::write_kept_output();
    let by_value =
        || crate::stdlib::net::real_connection(handle, call.method, &given(), call.timeout, call.at);
    if !crate::replay::active() {
        return match served() {
            Some(done) => Ok(match done? {
                Ok(gave) => Answer::Gave(gave),
                Err(failure) => Answer::Failed(failed(&failure)),
            }),
            None => taken(call, by_value()?, of_value),
        };
    }
    // Whether a wait timed out is in what it gave, so a replay doesn't time it again.
    let value = crate::replay::value(format_args!("{}", call.name()), call.at, || match served() {
        Some(done) => done.map(as_value),
        None => by_value(),
    })?;
    taken(call, value, of_value)
}

/// What a test's fake network gives for the call, or `None` outside a test.
fn faked(handle: i64, call: &Call, given: impl Fn() -> Vec<Value>) -> Option<Result<Value, Diagnostic>> {
    if !with(|state| state.world().is_some_and(|world| !world.real())) {
        return None;
    }
    let mut args = given();
    args.extend(call.timeout.map(duration_value));
    let name = call.name();
    with(|state| state.world_mut().map(|world| world.net.socket_method(handle, &name, &args, call.at)))
}

/// A call's value as its answer: an `Err` as its failure, and anything else as `of_value` reads
/// it, or the program stops, as a replay does whose recording doesn't fit.
fn taken<T>(call: &Call, value: Value, of_value: fn(Value) -> Option<T>) -> Result<Answer<T>, Diagnostic> {
    if matches!(value, Value::Err(_)) {
        return Ok(Answer::Failed(value));
    }
    of_value(value)
        .map(Answer::Gave)
        .ok_or_else(|| Diagnostic::new(format!("`{}` gave back what it never gives", call.name()), call.at))
}

/// What an export gives back for `answer`: `word` for what it gave, or `-1 - n` for the
/// temporary `n` its failure is kept in. A call that stopped the program gives 0.
fn word_for<T>(answer: Result<Answer<T>, Diagnostic>, word: impl FnOnce(T) -> i64) -> i64 {
    match answer {
        Ok(Answer::Gave(gave)) => word(gave),
        Ok(Answer::Failed(error)) => -1 - with(|state| state.push(error)),
        Err(wrong) => {
            fail_with(wrong);
            0
        }
    }
}

/// What an answer that's a value inside `Ok` is.
fn inside_ok(value: Value) -> Option<Value> {
    match value {
        Value::Ok(inside) => Some(Rc::unwrap_or_clone(inside)),
        _ => None,
    }
}

/// `fennec_conn_write(conn, data, timeout_ns, start, end)`: `conn.write(data)`, `data` a `str`'s or
/// `bytes`' block. Gives back 0 once it's all sent, or its failure.
pub extern "C" fn conn_write(handle: i64, data: i64, timeout_ns: i64, start: i64, end: i64) -> i64 {
    let call = Call::new("write", timeout_ns, (start, end));
    // Copied out first: the heap isn't borrowed while the write waits.
    let (bytes, text) = with(|state| {
        let block = u64::try_from(data).ok().and_then(|at| state.heap.block(at))?;
        Some(if super::heap::is_text(state, block) {
            (super::heap::block_text(block).into_bytes(), true)
        } else {
            (super::bytes::data_of(block), false)
        })
    })
    .unwrap_or_default();
    let given = || {
        vec![if text {
            Value::str(&String::from_utf8_lossy(&bytes))
        } else {
            Value::Bytes(Rc::from(bytes.as_slice()))
        }]
    };
    let answer = answered(
        handle,
        &call,
        || served_write(handle, &bytes, call.timeout).map(Ok),
        given,
        (|done| done_or_failed(done.map(|()| Value::Unit)), |value| inside_ok(value).map(|_| ())),
    );
    word_for(answer, |()| 0)
}

/// `fennec_conn_reads_within(conn, after_ns)`: `conn._reads_within(after)`, after which the rest of
/// a request has to come within `after` of now. Gives back 0.
pub extern "C" fn conn_reads_within(handle: i64, after_ns: i64) -> i64 {
    let call = Call::new("_reads_within", NO_TIMEOUT, (0, 0));
    let after = nanos(after_ns);
    let answer = answered(
        handle,
        &call,
        || served_reads_within(handle, after).map(|()| Ok(Ok(()))),
        || vec![duration_value(after)],
        (|_| Value::Unit, |value| matches!(value, Value::Unit).then_some(())),
    );
    word_for(answer, |()| 0)
}

/// `fennec_conn_draining(conn)`: `conn.draining()`, whether its server is draining, as 1 or 0.
pub extern "C" fn conn_draining(handle: i64) -> i64 {
    let call = Call::new("draining", NO_TIMEOUT, (0, 0));
    let answer = answered(
        handle,
        &call,
        || served_draining(handle).map(|draining| Ok(Ok(draining))),
        Vec::new,
        (|draining| Value::Bool(draining.unwrap_or(false)), |value| as_bool(&value)),
    );
    word_for(answer, i64::from)
}

/// `fennec_conn_room_for_body(conn, size)`: `conn._room_for_body(size)`, whether a body this big
/// fits in what every server's bodies may hold, taking its room if it does, as 1 or 0.
pub extern "C" fn conn_room_for_body(handle: i64, size: i64, start: i64, end: i64) -> i64 {
    let call = Call::new("_room_for_body", NO_TIMEOUT, (start, end));
    let answer = answered(
        handle,
        &call,
        || match bytes_asked(size, call.at) {
            Ok(size) => served_room_for_body(handle, size).map(|room| Ok(Ok(room))),
            Err(wrong) => Some(Err(wrong)),
        },
        || vec![Value::Int(size)],
        (|room| Value::Bool(room.unwrap_or(false)), |value| as_bool(&value)),
    );
    word_for(answer, i64::from)
}

fn as_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(yes) => Some(*yes),
        _ => None,
    }
}

/// `fennec_conn_body(conn, size, timeout_ns, start, end)`: `conn._body(size)`, a request's body,
/// read whole. Gives back its `bytes`' block, or its failure.
pub extern "C" fn conn_body(handle: i64, size: i64, timeout_ns: i64, start: i64, end: i64) -> i64 {
    let call = Call::new("_body", timeout_ns, (start, end));
    let answer = answered(
        handle,
        &call,
        || match bytes_asked(size, call.at) {
            Ok(size) => served_body(handle, size, call.timeout).map(Ok),
            Err(wrong) => Some(Err(wrong)),
        },
        || vec![Value::Int(size)],
        (
            |body| done_or_failed(body.map(|body| Value::Bytes(Rc::from(body)))),
            |value| match inside_ok(value)? {
                Value::Bytes(body) => Some(body.to_vec()),
                _ => None,
            },
        ),
    );
    word_for(answer, |body| with(|state| super::bytes::make(state, &body)))
}

/// `fennec_conn_read_head(conn, most_bytes, most_lines, most_blank, timeout_ns, element, lines,
/// start, end)`: `conn._read_head(most_bytes, most_lines, most_blank)`, a request's head read in
/// one call. Gives back the case of std.net's `Head` it is, `Head::case`, and for `Lines` sets
/// `lines` to a new `list[str]` of them, `element` that list's element. Or its failure.
pub extern "C" fn conn_read_head(
    handle: i64,
    most_bytes: i64,
    most_lines: i64,
    most_blank: i64,
    timeout_ns: i64,
    element: i64,
    lines: &mut i64,
    start: i64,
    end: i64,
) -> i64 {
    let call = Call::new("_read_head", timeout_ns, (start, end));
    let limits = Limits::new(most_bytes, most_lines, most_blank);
    let answer = answered(
        handle,
        &call,
        || served_head(handle, limits, call.timeout).map(Ok),
        || limits.values(),
        (
            |head| done_or_failed(head.map(crate::stdlib::net::head::value)),
            |value| Head::of_value(&inside_ok(value)?),
        ),
    );
    word_for(answer, |head| {
        let case = head.case();
        if let Head::Lines(read) = head {
            let read: Vec<&str> = read.iter().map(String::as_str).collect();
            *lines = with(|state| super::lists::list_of_strings(state, element, &read));
        }
        case
    })
}

#[cfg(test)]
mod tests;
