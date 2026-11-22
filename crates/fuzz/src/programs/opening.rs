//! `opened{id}(stdout)`: one `with` opening two or three handles from sources the program
//! writes, from left to right, each lending its name, and inside it, half the time, a `with`
//! taking a lent pair apart into two names. A handle may fail to open, which runs the `except`
//! cleanups of those opened before it, innermost first, and passes the failure up. The block then
//! ends, closing every handle innermost first, or fails or crashes, running every `except`
//! cleanup innermost first. A cleanup may crash: one in an `except` lets the outer cleanups run,
//! and stops the program after them, its crash winning over the block's failure; one in a normal
//! close runs the outer handles' `except` cleanups, and stops the program.

use std::fmt::Write as _;

/// How a handle opens and closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Gate {
    /// Opens, lends, and closes, or runs its `except` cleanup.
    Plain,
    /// Fails before it lends, with `Refused(code=len(name))`.
    FailsOpen,
    /// Crashes in its `except` cleanup, after printing `undo`.
    CrashesUndo,
    /// Crashes in its normal close, before printing `close`.
    CrashesClose,
}

impl Gate {
    /// The `how` the source is written with.
    fn how(self) -> i64 {
        match self {
            Gate::Plain => 0,
            Gate::FailsOpen => 1,
            Gate::CrashesUndo => 2,
            Gate::CrashesClose => 3,
        }
    }
}

/// How the innermost block ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlockEnd {
    Ends,
    /// `fail Refused(code=len(first) + 98)`, which is 99.
    Fails,
    /// `_ = [0][len(first) + 4]`, which crashes.
    Crashes,
}

/// One `Opened` statement.
#[derive(Clone, Debug)]
pub(crate) struct Opening {
    pub(crate) id: usize,
    /// The handles, from left to right: two or three.
    pub(crate) gates: Vec<Gate>,
    /// Whether a `with` inside takes a lent pair apart.
    pub(crate) pair: bool,
    pub(crate) end: BlockEnd,
}

/// What each handle lends, which its name in the source holds.
const NAMES: [&str; 3] = ["a", "bb", "ccc"];

/// The names the `with` gives what each handle lends.
const HELD: [&str; 3] = ["first", "second", "third"];

fn name(at: usize) -> &'static str {
    NAMES.get(at).copied().unwrap_or("a")
}

/// What `opened{id}` and the line calling it print, and whether the program stops there, on a
/// crash.
pub(crate) fn printed(opening: &Opening) -> (Vec<String>, bool) {
    let mut lines = Vec::new();
    let mut crashed = false;
    let mut opened = Vec::new();
    let mut failed = None;
    for (at, gate) in opening.gates.iter().enumerate() {
        lines.push(format!("open {}", name(at)));
        if *gate == Gate::FailsOpen {
            failed = Some(i64::try_from(name(at).len()).unwrap_or(0));
            break;
        }
        opened.push(at);
    }
    // The `except` cleanup of each handle in `undone`, innermost first: one that crashes lets the
    // outer ones run.
    let undo = |undone: &[usize], lines: &mut Vec<String>, crashed: &mut bool| {
        for at in undone.iter().rev() {
            lines.push(format!("undo {}", name(*at)));
            if opening.gates.get(*at) == Some(&Gate::CrashesUndo) {
                *crashed = true;
            }
        }
    };
    if failed.is_some() {
        undo(&opened, &mut lines, &mut crashed);
    } else {
        if opening.pair {
            lines.push("open pair".to_owned());
        }
        let mut shown: Vec<&str> = opened.iter().map(|at| name(*at)).collect();
        if opening.pair {
            shown.extend(["p", "7"]);
        }
        lines.push(format!("in {}", shown.join(" ")));
        match opening.end {
            BlockEnd::Ends => {
                lines.push("out".to_owned());
                if opening.pair {
                    lines.push("close pair".to_owned());
                }
                let mut closed = true;
                for (from, at) in opened.iter().enumerate().rev() {
                    if opening.gates.get(*at) == Some(&Gate::CrashesClose) {
                        crashed = true;
                        closed = false;
                        undo(opened.get(..from).unwrap_or_default(), &mut lines, &mut crashed);
                        break;
                    }
                    lines.push(format!("close {}", name(*at)));
                }
                if closed {
                    lines.push("after".to_owned());
                }
            }
            BlockEnd::Fails | BlockEnd::Crashes => {
                if opening.end == BlockEnd::Fails {
                    failed = Some(99);
                } else {
                    crashed = true;
                }
                if opening.pair {
                    lines.push("undo pair".to_owned());
                }
                undo(&opened, &mut lines, &mut crashed);
            }
        }
    }
    if crashed {
        return (lines, true);
    }
    if let Some(code) = failed {
        lines.push(format!("refused {code}"));
    }
    (lines, false)
}

/// The sources `opened{id}` opens, and `opened{id}` itself.
pub(crate) fn written(opening: &Opening) -> String {
    let id = opening.id;
    let mut out = format!(
        "def gate{id}(name: str, how: int, stdout: Stdout) -> with str fails Refused:\n    \
         stdout.print(\"open\", name)\n    if how == 1:\n        fail Refused(code=len(name))\n    \
         lend name except:\n        stdout.print(\"undo\", name)\n        if how == 2:\n            \
         _ = [0][len(name)]\n    if how == 3:\n        _ = [0][len(name) + 10]\n    \
         stdout.print(\"close\", name)\n\n"
    );
    if opening.pair {
        let _ = write!(
            out,
            "def pair{id}(stdout: Stdout) -> with (str, int):\n    stdout.print(\"open pair\")\n    \
             lend (\"p\", 7) except:\n        stdout.print(\"undo pair\")\n    stdout.print(\"close pair\")\n\n"
        );
    }
    let count = opening.gates.len();
    let gates: Vec<String> = opening
        .gates
        .iter()
        .enumerate()
        .map(|(at, gate)| format!("gate{id}(\"{}\", how={}, stdout=stdout)?", name(at), gate.how()))
        .collect();
    let held = HELD.get(..count).unwrap_or(&HELD);
    let _ = writeln!(out, "def opened{id}(stdout: Stdout) fails Refused:");
    let _ = writeln!(out, "    with {} as {}:", gates.join(", "), held.join(", "));
    let mut indent = "        ".to_owned();
    let mut shown: Vec<&str> = held.to_vec();
    if opening.pair {
        let _ = writeln!(out, "{indent}with pair{id}(stdout) as word, count:");
        indent.push_str("    ");
        shown.extend(["word", "count"]);
    }
    let _ = writeln!(out, "{indent}stdout.print(\"in\", {})", shown.join(", "));
    match opening.end {
        BlockEnd::Ends => {
            let _ = writeln!(out, "{indent}stdout.print(\"out\")");
        }
        BlockEnd::Fails => {
            let _ = writeln!(out, "{indent}fail Refused(code=len(first) + 98)");
        }
        BlockEnd::Crashes => {
            let _ = writeln!(out, "{indent}_ = [0][len(first) + 4]");
            let _ = writeln!(out, "{indent}stdout.print(\"out\")");
        }
    }
    let _ = writeln!(out, "    stdout.print(\"after\")\n");
    out
}
