//! `Connection._read_head(most_bytes, most_lines, most_blank, timeout=)`: a request's head in one
//! call, its request line and each header line up to the blank line that ends it, where std.net
//! read it a line a call, each call crossing into the runtime by name with its arguments as values
//! (`runtime-lane-perf/http-2026-11-21.md` in the coordinator's notes). Each line is read as
//! `read_line(timeout=, crlf=True)` reads one, by the connection's own line reader, so a line is
//! split, refused and timed as it was. Only the head's own rules are here: the blank lines passed
//! over before it, where it ends, and its limits. std keeps every other check of what's in it.
//!
//! What it gives is std.net's private `Head`:
//! - `Lines(lines)`: the request line, then each header line, without their endings;
//! - `Ended`: the stream ended, or held only blank lines, before a request line came;
//! - `Unended`: it ended after the request line, before the blank line;
//! - `LongFirst`: the request line alone is over `most_bytes`, or too long to read at all;
//! - `TooBig`: the head is over `most_bytes` in all, has over `most_lines` header lines, or has a
//!   header line too long to read at all.
//!
//! Sizes are of the lines as text, without their endings, as std's `text_size` counts them. It
//! stops reading at a limit, so nothing past the line that crossed it is read, and what comes after
//! the blank line stays where it is for the body and the next request.

use std::rc::Rc;

use super::Failure;
use crate::value::Value;

/// The most a head may hold, as the call's `most_bytes`, `most_lines` and `most_blank` give it:
/// `bytes` in all, `lines` header lines, and `blank` blank lines passed over before its request
/// line.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    bytes: usize,
    lines: usize,
    blank: usize,
}

impl Limits {
    /// The limits a call gave as its three arguments, or `None` when they aren't three ints. A
    /// negative one is none at all.
    pub(crate) fn of(args: &[Value]) -> Option<Limits> {
        let [Value::Int(bytes), Value::Int(lines), Value::Int(blank)] = args else { return None };
        let most = |given: &i64| usize::try_from(*given).unwrap_or(0);
        Some(Limits { bytes: most(bytes), lines: most(lines), blank: most(blank) })
    }
}

/// What a head read gave.
#[derive(Debug, PartialEq, Eq)]
enum Head {
    Lines(Vec<String>),
    Ended,
    Unended,
    LongFirst,
    TooBig,
}

/// Why reading a head stopped short of its end: a line too long to read, which gives the head it
/// makes, the connection failing, or `E`, the call itself failing, as when a test's network would
/// wait forever.
enum Stopped<E> {
    Gave(Head),
    Failed(Failure),
    Call(E),
}

/// A head read with `line`, which gives the next line as `read_line(crlf=True)` gives it, as text
/// with its ending taken off, or `None` at the end. A line the connection fails on fails the head.
pub(crate) fn read<E>(
    limits: Limits,
    mut line: impl FnMut() -> Result<Result<Option<String>, Failure>, E>,
) -> Result<Result<Value, Failure>, E> {
    match head(limits, &mut line) {
        Ok(head) | Err(Stopped::Gave(head)) => Ok(Ok(value(head))),
        Err(Stopped::Failed(failure)) => Ok(Err(failure)),
        Err(Stopped::Call(wrong)) => Err(wrong),
    }
}

/// [`read`] for a connection whose line reader has no failure of its own besides the connection's.
pub(crate) fn read_lines(
    limits: Limits,
    mut line: impl FnMut() -> Result<Option<String>, Failure>,
) -> Result<Value, Failure> {
    read(limits, || Ok::<_, std::convert::Infallible>(line())).unwrap_or_else(|never| match never {})
}

fn head<E>(
    limits: Limits,
    line: &mut impl FnMut() -> Result<Result<Option<String>, Failure>, E>,
) -> Result<Head, Stopped<E>> {
    // A line too long to read is over the most bytes a head may hold, so it's the head that
    // makes, `too_long`: `LongFirst` for the request line and `TooBig` for a header, the 414 and
    // the 431. Nothing more is read. Any other failure of a line fails the head.
    let mut next = |too_long: fn() -> Head| match line() {
        Ok(Ok(read)) => Ok(read),
        Ok(Err(failure)) if super::is_line_too_long(&failure) => Err(Stopped::Gave(too_long())),
        Ok(Err(failure)) => Err(Stopped::Failed(failure)),
        Err(wrong) => Err(Stopped::Call(wrong)),
    };
    let first_too_long = || Head::LongFirst;
    let Some(mut first) = next(first_too_long)? else { return Ok(Head::Ended) };
    let mut blank = 0;
    while first.is_empty() && blank < limits.blank {
        blank += 1;
        let Some(again) = next(first_too_long)? else { return Ok(Head::Ended) };
        first = again;
    }
    if first.is_empty() {
        return Ok(Head::Ended);
    }
    if first.len() > limits.bytes {
        return Ok(Head::LongFirst);
    }
    let mut size = first.len();
    let mut lines = vec![first];
    loop {
        let Some(header) = next(|| Head::TooBig)? else { return Ok(Head::Unended) };
        if header.is_empty() {
            return Ok(Head::Lines(lines));
        }
        size += header.len();
        // `lines` holds the request line too, so this is one more header than it has.
        if lines.len() > limits.lines || size > limits.bytes {
            return Ok(Head::TooBig);
        }
        lines.push(header);
    }
}

/// A head as std.net's `Head`.
fn value(head: Head) -> Value {
    let (case, fields) = match head {
        Head::Lines(lines) => {
            let lines = lines.iter().map(|line| Value::str(line)).collect();
            ("Lines", vec![("lines", Value::List(Rc::new(lines)))])
        }
        Head::Ended => ("Ended", Vec::new()),
        Head::Unended => ("Unended", Vec::new()),
        Head::LongFirst => ("LongFirst", Vec::new()),
        Head::TooBig => ("TooBig", Vec::new()),
    };
    Value::made_case("Head", case, fields)
}

#[cfg(test)]
mod tests;
