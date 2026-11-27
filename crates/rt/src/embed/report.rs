//! `intrinsics.report(text)`: one line on standard error, written as the runtime writes a crash's
//! report, for the standard library to leave a trace where nothing else will: a server's handler
//! that failed, answered with a 500, when the program gave its server no `Log`.
//!
//! - **One line, whole.** Part of the text can come from a request, so every line break and
//!   control character in it is written escaped, `\n` or `\u{1b}`, and so is a backslash, so what's
//!   read back is what was said. A line holds at most [`MOST_REPORTED`] bytes of it, then `…`.
//! - **From any thread, never interleaved.** The line is written in one go while standard error's
//!   lock is held, which every other write there takes too, a crash's report and another thread's
//!   line among them.
//! - **After what was printed.** What this thread printed and kept goes out first, as before a
//!   crash's report, so the line comes where it happened.

use std::fmt::Write as _;

use super::with;

/// The most bytes of a report's text one line holds, escapes included. Past it, the line ends with
/// `…`: a request a client made as long as it liked doesn't make a line as long.
pub const MOST_REPORTED: usize = 1024;

/// `fennec_report(text)`: `intrinsics.report(text)`, `text` a string's block. Writes it as one
/// line on standard error, escaped and cut to [`MOST_REPORTED`] bytes.
pub extern "C" fn report(text: i64) {
    let text = with(|state| {
        u64::try_from(text).ok().and_then(|at| state.heap.block(at)).map(super::heap::block_text)
    });
    super::write_kept_output();
    write_whole(&mut std::io::stderr().lock(), &line_of(&text.unwrap_or_default()));
}

/// Writes `line` to `out` in one go, which holds its lock: a failure to write it is let go, as
/// a crash's report would have nowhere else to say it either.
fn write_whole(out: &mut impl std::io::Write, line: &str) {
    let _ = out.write_all(line.as_bytes());
    let _ = out.flush();
}

/// The line a report of `text` writes, its ending included: each line break, control character,
/// line or paragraph separator and backslash escaped, and at most [`MOST_REPORTED`] bytes of it,
/// then `…` when there was more.
pub(crate) fn line_of(text: &str) -> String {
    let mut line = String::with_capacity(text.len().min(MOST_REPORTED) + 4);
    let mut escaped = String::new();
    for character in text.chars() {
        escaped.clear();
        match character {
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\\' => escaped.push_str("\\\\"),
            character if character.is_control() || matches!(character, '\u{2028}' | '\u{2029}') => {
                let _ = write!(escaped, "\\u{{{:x}}}", u32::from(character));
            }
            character => escaped.push(character),
        }
        if line.len() + escaped.len() > MOST_REPORTED {
            line.push('…');
            break;
        }
        line.push_str(&escaped);
    }
    line.push('\n');
    line
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::{MOST_REPORTED, line_of};
    use crate::embed::{Tables, finish, heap, start, with};

    /// What tells a run of this test binary to be the one reporting from two threads at once.
    const REPORTING: &str = "FENNEC_TEST_REPORTING";
    /// How many lines each of the two threads reports.
    const LINES: usize = 400;

    /// Each thread's line: long enough that a write of it could be split, and marked as its own.
    fn said(thread: usize) -> String {
        format!(
            "report from thread {thread}: {}",
            char::from(b'a' + u8::try_from(thread).unwrap()).to_string().repeat(900)
        )
    }

    #[test]
    fn a_report_is_one_line_with_whatever_could_break_it_escaped() {
        assert_eq!(line_of("a handler failed"), "a handler failed\n");
        assert_eq!(
            line_of("GET /x\r\nX-Evil: 1\n\u{1b}[31mred\\ \u{2028}"),
            "GET /x\\r\\nX-Evil: 1\\n\\u{1b}[31mred\\\\ \\u{2028}\n"
        );
        assert_eq!(line_of("tab\there"), "tab\\there\n");
        assert_eq!(line_of("é and 狐 stay"), "é and 狐 stay\n");
        assert_eq!(line_of(""), "\n");
    }

    #[test]
    fn a_report_holds_at_most_its_share_of_a_long_text_and_says_so() {
        let long = "x".repeat(MOST_REPORTED * 3);
        let line = line_of(&long);
        assert_eq!(line.len(), MOST_REPORTED + '…'.len_utf8() + 1);
        assert!(line.ends_with("x…\n"), "{line}");
        // A character is never cut in two, nor an escape.
        let wide = line_of(&"狐".repeat(MOST_REPORTED));
        assert!(wide.len() <= MOST_REPORTED + '…'.len_utf8() + 1);
        assert!(wide.ends_with("狐…\n"));
        let breaks = line_of(&"\n".repeat(MOST_REPORTED));
        assert!(breaks.ends_with("\\n…\n"), "an escape was cut");
        assert_eq!(line_of(&"y".repeat(MOST_REPORTED)), format!("{}\n", "y".repeat(MOST_REPORTED)));
    }

    /// Two threads reporting at once, each from a runtime of its own, as a server's workers would:
    /// run in a process of its own by [`two_threads_reporting_at_once_give_whole_lines`], which reads
    /// what it writes on standard error. On its own, it does nothing.
    #[test]
    #[ignore = "it reports on this process's standard error, which a test of its own reads"]
    fn two_threads_report_at_once() {
        if std::env::var_os(REPORTING).is_none() {
            return;
        }
        let threads: Vec<_> = (0..2)
            .map(|thread| {
                std::thread::spawn(move || {
                    start(Tables::default(), crate::Inputs::default());
                    let text = with(|state| heap::text_address(state, &said(thread)));
                    for _ in 0..LINES {
                        super::report(text);
                    }
                    finish();
                })
            })
            .collect();
        for thread in threads {
            thread.join().expect("a reporting thread ended");
        }
    }

    #[test]
    fn two_threads_reporting_at_once_give_whole_lines() {
        let run = Command::new(std::env::current_exe().expect("this test binary"))
            .args(["--exact", "embed::report::tests::two_threads_report_at_once", "--ignored", "--nocapture"])
            .env(REPORTING, "1")
            .output()
            .expect("the test binary runs again");
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        let written = String::from_utf8_lossy(&run.stderr);
        let reported: Vec<&str> = written.lines().filter(|line| line.starts_with("report from")).collect();
        for thread in 0..2 {
            let whole = said(thread);
            assert_eq!(
                reported.iter().filter(|line| **line == whole).count(),
                LINES,
                "thread {thread}'s lines"
            );
        }
        assert_eq!(reported.len(), 2 * LINES, "a line was split or run into another");
        assert!(
            written.lines().all(|line| !line.contains("report from") || line.starts_with("report from")),
            "a line began inside another"
        );
    }
}
