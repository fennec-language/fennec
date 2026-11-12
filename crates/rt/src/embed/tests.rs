//! Running several tests on one thread, one after another.

use fennec_syntax::Diagnostic;

use super::{STATE, set_failed};

/// Ends one test of several run on this thread, giving back how it failed, if it did, and
/// clearing what a failure left behind so the next one starts clean.
pub fn next_test() -> Option<Diagnostic> {
    set_failed(0);
    STATE.with_borrow_mut(|state| {
        let state = state.as_mut()?;
        let failure = state.failure.take();
        if failure.is_some() {
            state.temps.clear();
            state.inputs.clear();
        }
        // Its call chain, for whoever reports the failure to take.
        super::keep_trace(std::mem::take(&mut state.trace));
        failure
    })
}

/// What a task or range a test started prints goes into the test's capture a whole line at a time:
/// a `print` arrives in pieces, and another thread's line printed between them lands before or
/// after it, never inside it.
#[test]
fn a_tasks_printed_line_goes_into_its_tests_capture_whole() {
    use std::sync::{Arc, Barrier};
    let capture = crate::fakes::Capture::new();
    let turns = Arc::new(Barrier::new(2));
    let other = {
        let (capture, turns) = (Arc::clone(&capture), Arc::clone(&turns));
        std::thread::spawn(move || {
            turns.wait();
            super::capture_lines(&capture, "other 1\n");
            turns.wait();
        })
    };
    super::capture_lines(&capture, "item ");
    turns.wait();
    turns.wait();
    super::capture_lines(&capture, "0");
    super::capture_lines(&capture, "\n");
    other.join().unwrap();
    assert_eq!(capture.printed(), "other 1\nitem 0\n");
}

/// What's held of an unfinished line goes to the capture it was printed for: when the thread
/// prints into another, and when what's captured is read.
#[test]
fn a_held_line_goes_into_the_capture_it_was_printed_for() {
    let (first, second) = (crate::fakes::Capture::new(), crate::fakes::Capture::new());
    super::capture_lines(&first, "first");
    super::capture_lines(&second, "second");
    assert_eq!((first.printed(), second.printed()), ("first".to_owned(), String::new()));
    super::flush_held_line();
    assert_eq!(second.printed(), "second");
}

/// A block a runtime function would give native code as 0, no block at all, stops the run as a bug
/// in Fennec rather than reaching native code, which uses it at once with no check.
#[test]
fn a_block_made_as_nothing_is_said_as_a_bug() {
    assert_eq!(super::made("fennec_list_new", 8), 8);
    let said = std::panic::catch_unwind(|| super::made("fennec_list_new", 0)).expect_err("0 is said");
    let said = said.downcast_ref::<String>().cloned().unwrap_or_default();
    assert_eq!(said, "`fennec_list_new` made no block");
}

/// A header given back where there's nothing else to give, as with no runtime on the thread, is
/// the empty string's, never one pointing at no block.
#[test]
fn a_header_by_default_is_the_empty_strings() {
    let empty = super::text::Text::default();
    assert_eq!(super::text::inline_bytes(&empty.words), Some(Vec::new()));
}
