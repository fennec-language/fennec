//! A head read from lines given in turn, against what std.net's `request_line` and
//! `read_request` made of the same lines a call at a time: each case, each limit at and past its
//! edge, and a failure partway.

use super::{Failure, Head, Limits, Stopped, head};

const LIMITS: Limits = Limits { bytes: 40, lines: 3, blank: 2 };

/// The head `lines` make, each given as a connection gives a line, then the end, and how many
/// lines it read.
fn read_from(limits: Limits, lines: &[&str]) -> (Head, usize) {
    let mut given = lines.iter();
    let mut taken = 0;
    let mut line = || {
        let next = given.next().map(|line| (*line).to_owned());
        taken += usize::from(next.is_some());
        Ok::<_, ()>(Ok(next))
    };
    let Ok(head) = head(limits, &mut line) else { panic!("no line failed") };
    (head, taken)
}

fn lines(lines: &[&str]) -> Head {
    Head::Lines(lines.iter().map(|line| (*line).to_owned()).collect())
}

#[test]
fn a_head_is_its_lines_up_to_the_blank_one_and_what_follows_is_left() {
    let (head, taken) = read_from(LIMITS, &["GET / HTTP/1.1", "host: a", "", "the body", "next"]);
    assert_eq!(head, lines(&["GET / HTTP/1.1", "host: a"]));
    assert_eq!(taken, 3, "nothing past the blank line is read");
    assert_eq!(read_from(LIMITS, &["GET / HTTP/1.1", ""]).0, lines(&["GET / HTTP/1.1"]));
}

#[test]
fn blank_lines_before_the_request_line_are_passed_over_up_to_the_most() {
    assert_eq!(read_from(LIMITS, &["", "", "GET / HTTP/1.1", ""]).0, lines(&["GET / HTTP/1.1"]));
    // One more than the most: the line after the last passed over is taken as the request line,
    // and it's blank, so there's none, as `request_line` gave `None`.
    let (head, taken) = read_from(LIMITS, &["", "", "", "GET / HTTP/1.1", ""]);
    assert_eq!((head, taken), (Head::Ended, 3));
}

#[test]
fn a_stream_that_ends_before_a_request_line_or_its_blank_line_says_which() {
    assert_eq!(read_from(LIMITS, &[]).0, Head::Ended);
    assert_eq!(read_from(LIMITS, &["", ""]).0, Head::Ended);
    assert_eq!(read_from(LIMITS, &["GET / HTTP/1.1"]).0, Head::Unended);
    assert_eq!(read_from(LIMITS, &["GET / HTTP/1.1", "host: a"]).0, Head::Unended);
}

#[test]
fn a_request_line_over_the_bytes_is_long_and_nothing_after_it_is_read() {
    let at_most = "G".repeat(40);
    assert_eq!(read_from(LIMITS, &[&at_most, ""]).0, lines(&[&at_most]));
    let over = "G".repeat(41);
    assert_eq!(read_from(LIMITS, &[&over, "host: a", ""]), (Head::LongFirst, 1));
    // Bytes, not characters: 20 two-byte characters is 40, and 21 is over.
    let wide = "é".repeat(21);
    assert_eq!(read_from(LIMITS, &[&wide, ""]).0, Head::LongFirst);
}

#[test]
fn a_head_over_the_bytes_or_header_lines_is_too_big_where_it_crosses() {
    // 14 bytes of request line, then headers of 10, 10 and 6: 40 in all is the most.
    let first = "GET / HTTP/1.1";
    let at_most = read_from(LIMITS, &[first, "host: aaaa", "host: bbbb", "x: yy!", ""]);
    assert_eq!(at_most.0, lines(&[first, "host: aaaa", "host: bbbb", "x: yy!"]));
    let over = read_from(LIMITS, &[first, "host: aaaa", "host: bbbb", "x: yyy!", "never read", ""]);
    assert_eq!(over, (Head::TooBig, 4));
    // Three header lines is the most, and a fourth is too many, however short.
    assert_eq!(read_from(LIMITS, &["G", "a", "b", "c", ""]).0, lines(&["G", "a", "b", "c"]));
    assert_eq!(read_from(LIMITS, &["G", "a", "b", "c", "d", ""]), (Head::TooBig, 5));
}

#[test]
fn a_line_that_fails_fails_the_head_and_a_call_that_fails_stops_it() {
    let mut given = [Ok(Some("GET / HTTP/1.1".to_owned())), Err(Failure::Closed)].into_iter();
    let mut line = || Ok::<_, ()>(given.next().unwrap_or(Ok(None)));
    assert!(matches!(head(LIMITS, &mut line), Err(Stopped::Failed(Failure::Closed))));
    let mut never = || Err::<Result<Option<String>, Failure>, &str>("waits forever");
    assert!(matches!(head(LIMITS, &mut never), Err(Stopped::Call("waits forever"))));
}

#[test]
fn limits_come_from_three_ints_and_a_negative_one_is_none() {
    use crate::value::Value;
    let limits = Limits::of(&[Value::Int(5), Value::Int(-1), Value::Int(0)]).unwrap();
    assert_eq!((limits.bytes, limits.lines, limits.blank), (5, 0, 0));
    assert!(Limits::of(&[Value::Int(5), Value::Int(1)]).is_none());
    assert!(Limits::of(&[Value::Int(5), Value::Int(1), Value::str("2")]).is_none());
}
