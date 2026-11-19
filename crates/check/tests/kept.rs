//! A build that keeps what its check found and carries it over into the build after an edit.
//! What the edit build finds is what a build that keeps nothing finds, and each check that
//! carried anything over is compared with checking it all again, as a dev build's sample is.

use fennec_check::{KeptCheck, check_to_build_keeping, check_to_build_saying};

/// What a build gives back: its signatures, shown with each open type variable's number taken
/// out, since two checks that agree can number theirs apart, and whether they're of the module as
/// it's left. Or what it finds wrong.
type Built = Result<(String, bool), Vec<String>>;

/// `text` with each open type variable's number taken out: `Var(12)` as `Var(_)`.
fn without_var_numbers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("Var(") {
        let (before, after) = rest.split_at(at + "Var(".len());
        out.push_str(before);
        let digits = after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits > 0 {
            out.push('_');
        }
        rest = after.get(digits..).unwrap_or_default();
    }
    out.push_str(rest);
    out
}

/// What a build found, as a `Built`.
fn built(found: Result<(fennec_check::Signatures, bool), Vec<fennec_syntax::Diagnostic>>) -> Built {
    found
        .map(|(signatures, as_left)| (without_var_numbers(&format!("{signatures:?}")), as_left))
        .map_err(|problems| problems.iter().map(|problem| format!("{problem:?}")).collect())
}

/// A build of `source` that keeps what its check finds, carrying over from `kept`, with each check
/// that carried anything over compared with checking it all again.
fn kept_build(source: &str, kept: Option<KeptCheck>) -> (Built, Option<KeptCheck>) {
    let mut module = fennec_syntax::parse(source).unwrap();
    match check_to_build_keeping(&mut module, kept, true) {
        Ok((signatures, as_left, kept)) => (built(Ok((signatures, as_left))), Some(kept)),
        Err(problems) => (built(Err(problems)), None),
    }
}

/// A build of `source` that keeps nothing.
fn cold_build(source: &str) -> Built {
    let mut module = fennec_syntax::parse(source).unwrap();
    built(check_to_build_saying(&mut module))
}

/// Builds `before`, keeping its check, then `after`, carrying over from it. The edit build finds
/// just what a build of `after` that keeps nothing finds. Gives back how many functions it could
/// carry over, when it passes, and what it finds wrong.
fn edited(before: &str, after: &str) -> (Option<usize>, Vec<String>) {
    let (first, kept) = kept_build(before, None);
    assert!(first.is_ok(), "{first:?}");
    let (found, kept) = kept_build(after, kept);
    assert_eq!(found, cold_build(after));
    (kept.map(|kept| kept.carried_over()), found.err().unwrap_or_default())
}

/// A change to one function's body that leaves what it gives back as it was carries over every
/// other function, here one before it and one after.
#[test]
fn a_body_edit_carries_over_every_other_function() {
    let before = "def main(stdout: Stdout):\n    stdout.print(double(2) + triple(3))\n\n\
                  def double(n: int) -> int:\n    return n * 2\n\n\
                  def triple(n: int) -> int:\n    return n * 3\n";
    let after = before.replace("return n * 2", "return n + n");
    let (carried, problems) = edited(before, &after);
    assert!(problems.is_empty(), "{problems:?}");
    assert!(carried.is_some_and(|carried| carried >= 2), "{carried:?}");
}

/// A function that gives back something else now has its callers checked again, though they're
/// written as they were: `main` prints a `str` now.
#[test]
fn a_changed_signature_checks_its_callers_again() {
    let before = "def main(stdout: Stdout):\n    stdout.print(double(2))\n\n\
                  def double(n: int) -> int:\n    return n * 2\n";
    let after = before.replace("-> int:\n    return n * 2", "-> str:\n    return \"ab\"");
    let (carried, problems) = edited(before, &after);
    assert!(problems.is_empty(), "{problems:?}");
    assert!(carried.is_some_and(|carried| carried >= 1), "{carried:?}");
    // And one whose call no longer fits is refused, as checking it all again refuses it.
    let before = "def main(stdout: Stdout):\n    total: int = double(2)\n    stdout.print(total)\n\n\
                  def double(n: int) -> int:\n    return n * 2\n";
    let after = before.replace("-> int:\n    return n * 2", "-> str:\n    return \"ab\"");
    let (_, problems) = edited(before, &after);
    assert_eq!(problems.len(), 1, "{problems:?}");
}

/// A function that waits now, called inside a lock by one written as it was, is refused there:
/// what's called inside a lock is followed over every function, carried over or not.
#[test]
fn a_function_that_waits_now_is_refused_inside_a_lock_carried_over() {
    let before = "struct Scores:\n    total = 0\n\n\
                  def main(stdout: Stdout, clock: Clock):\n    shared scores = Scores()\n    \
                  lock scores as held:\n        rest(clock)\n        stdout.print(held.total)\n\n\
                  def rest(clock: Clock):\n    _ = clock.now()\n";
    let after = before.replace("_ = clock.now()", "clock.sleep(1s)");
    let (_, problems) = edited(before, &after);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("waits on `Clock.sleep`"), "{problems:?}");
}

/// An edit that adds a function, or changes what the program declares, carries nothing over: a
/// name written as it was may mean something else now.
#[test]
fn an_edit_to_what_is_declared_carries_nothing_over() {
    let before = "struct Scores:\n    total = 0\n\n\
                  def main(stdout: Stdout):\n    stdout.print(Scores().total)\n";
    let added = format!("{before}\ndef unused() -> int:\n    return 1\n");
    assert_eq!(edited(before, &added), (Some(0), Vec::new()));
    let declared = before.replace("total = 0", "total = 5");
    assert_eq!(edited(before, &declared), (Some(0), Vec::new()));
}

/// The program as written, but with a number inside its last function that has one made one more:
/// a change to a body, as an edit makes. Nothing when it has none.
fn with_a_number_changed(source: &str) -> Option<String> {
    let lines: Vec<&str> = source.lines().collect();
    // The last line inside a function written at the top, with a number of one digit on it.
    let mut inside = false;
    let mut last = None;
    for (at, line) in lines.iter().enumerate() {
        if !line.starts_with(' ') && !line.trim().is_empty() {
            inside = line.starts_with("def ");
            continue;
        }
        if !inside {
            continue;
        }
        let bytes = line.as_bytes();
        let word = |at: usize| bytes.get(at).is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_' || *byte == b'.');
        let digit = (0..bytes.len()).find(|&column| {
            (b'1'..=b'8').contains(&bytes[column])
                && (column == 0 || !word(column - 1))
                && !word(column + 1)
                && !line[..column].contains('"')
                && !line[..column].contains('#')
        });
        if let Some(column) = digit {
            last = Some((at, column));
        }
    }
    let (line, column) = last?;
    let mut edited: Vec<String> = lines.iter().map(|line| (*line).to_owned()).collect();
    let digit = edited[line].as_bytes()[column];
    edited[line].replace_range(column..=column, &char::from(digit + 1).to_string());
    Some(format!("{}\n", edited.join("\n")))
}

/// Every kernel, the eight real programs among them, every example and every real-world program
/// that builds is built again as it is, carrying everything over, then with a number in its last
/// function changed: each build finds what a build that keeps nothing finds, and each check that
/// carries anything over agrees with checking it all again.
#[test]
fn every_program_built_again_after_an_edit_finds_what_a_whole_check_finds() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    for folder in ["bench/kernels", "examples", "bench/realworld"] {
        for entry in std::fs::read_dir(root.join(folder)).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|extension| extension == "fen") {
                files.push(path);
            }
        }
    }
    files.sort();
    let (mut programs, mut edits) = (0, 0);
    for path in files {
        let source = std::fs::read_to_string(&path).unwrap();
        if fennec_syntax::parse(&source).is_err() {
            continue;
        }
        let shown = path.display();
        let (first, kept) = kept_build(&source, None);
        let Some(kept) = kept else { continue };
        let (again, kept) = kept_build(&source, Some(kept));
        assert_eq!(again, first, "{shown}, built again as it is");
        let kept = kept.unwrap_or_else(|| panic!("{shown}: built again as it is, nothing was kept"));
        assert!(kept.carried_over() > 0, "{shown}: built again as it is, nothing was carried over");
        programs += 1;
        let Some(edited) = with_a_number_changed(&source) else { continue };
        if fennec_syntax::parse(&edited).is_err() {
            continue;
        }
        let (found, _) = kept_build(&edited, Some(kept));
        assert_eq!(found, cold_build(&edited), "{shown}, with a number changed");
        edits += 1;
    }
    assert!(programs >= 100, "only {programs} programs were built");
    assert!(edits >= 50, "only {edits} programs were edited");
}

/// One dev build in 8 compares, picked by what it's built from; a release compiler compares only
/// when asked to.
#[test]
fn one_dev_build_in_eight_compares() {
    let compared = (0..64u64).filter(|inputs| fennec_check::compares_kept_check(*inputs)).count();
    assert_eq!(compared, if cfg!(debug_assertions) { 8 } else { 0 });
}
