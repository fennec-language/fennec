//! A build that keeps what its check found and carries it over into the build after an edit.
//! What the edit build finds is what a build that keeps nothing finds, and each check that
//! carried anything over is compared with checking it all again, as a dev build's sample is.

use fennec_check::{Edit, KeptCheck, check_to_build_keeping, check_to_build_saying};
use fennec_syntax::Span;

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

/// `source` parsed, or what's wrong with it.
fn parsed(source: &str) -> Result<fennec_syntax::Module, Vec<String>> {
    fennec_syntax::parse(source).map_err(|diagnostic| vec![format!("parse: {}", diagnostic.message)])
}

/// A build of `source` that keeps what its check finds, carrying over from `kept`, with each check
/// that carried anything over compared with checking it all again.
fn kept_build(source: &str, kept: Option<KeptCheck>) -> (Built, Option<KeptCheck>) {
    kept_build_after(source, kept, None)
}

/// `kept_build`, told what the edit since the kept check changed, as a build reads the texts.
fn kept_build_after(
    source: &str,
    kept: Option<KeptCheck>,
    edit: Option<&Edit<'_>>,
) -> (Built, Option<KeptCheck>) {
    let mut module = match parsed(source) {
        Ok(module) => module,
        Err(problems) => return (Err(problems), None),
    };
    match check_to_build_keeping(&mut module, (kept, edit), true) {
        Ok((signatures, as_left, kept)) => (built(Ok((signatures, as_left))), Some(kept)),
        Err(problems) => (built(Err(problems)), None),
    }
}

/// A build of `source` that keeps nothing.
fn cold_build(source: &str) -> Built {
    let mut module = parsed(source)?;
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

/// The text of a one-file program that an edit from `before` to `after` changed or moved, as a
/// build reads it: from the first byte that differs to the last, when the length is the same, or
/// else to the file's end.
fn dirty_between(before: &str, after: &str) -> Vec<Span> {
    let prefix = before.bytes().zip(after.bytes()).take_while(|(was, is)| was == is).count();
    if prefix == before.len() && prefix == after.len() {
        return Vec::new();
    }
    let room = before.len().min(after.len()) - prefix;
    let suffix =
        before.bytes().rev().zip(after.bytes().rev()).take_while(|(was, is)| was == is).count().min(room);
    let end = if before.len() == after.len() { before.len() - suffix } else { before.len() + 1 };
    let at = |offset: usize| u32::try_from(offset).unwrap_or(u32::MAX);
    vec![Span { start: at(prefix), end: at(end) }]
}

/// Builds `before`, keeping its check, then `after`, carrying over from it as a build told what the
/// edit changed or moved does. The edit build finds just what a build of `after` that keeps nothing
/// finds. Gives back how many functions it could carry over, when it passes, and what it finds
/// wrong.
fn edited_moving(before: &str, after: &str) -> (Option<usize>, Vec<String>) {
    let (first, kept) = kept_build(before, None);
    assert!(first.is_ok(), "{first:?}");
    let text = |source: &str, span: Span| {
        source
            .get(
                usize::try_from(span.start).unwrap_or(usize::MAX)
                    ..usize::try_from(span.end).unwrap_or(usize::MAX),
            )
            .map(str::to_owned)
    };
    let same_text = |was: Span, is: Span| text(before, was).is_some_and(|was| text(after, is) == Some(was));
    let edit = Edit { dirty: dirty_between(before, after), same_text: &same_text };
    let (found, kept) = kept_build_after(after, kept, Some(&edit));
    assert_eq!(found, cold_build(after));
    (kept.map(|kept| kept.carried_over()), found.err().unwrap_or_default())
}

/// An edit that makes a function longer moves everything after it in its file. What's before it is
/// carried over, a type after it whose text is the same is still the same type, and what's after it
/// is checked again.
#[test]
fn an_edit_that_moves_the_rest_of_its_file_carries_over_what_comes_before() {
    let before = "def main(stdout: Stdout):\n    stdout.print(first(1) + edited(2) + later(3))\n\n\
                  def first(n: int) -> int:\n    return n + 1\n\n\
                  def edited(n: int) -> int:\n    return n * 2\n\n\
                  struct Point:\n    x = 1\n\n\
                  def later(n: int) -> int:\n    return Point().x + n\n";
    let after = before.replace("    return n * 2\n", "    total = n * 2\n    return total\n");
    let (carried, problems) = edited_moving(before, &after);
    assert!(problems.is_empty(), "{problems:?}");
    assert!(carried.is_some_and(|carried| carried >= 2), "{carried:?}");
}

/// A function before an edit that took a default from a type the edit moved, as `Point()` takes
/// `x = 1`, is checked again: what it found holds where the default was. Each check that carried
/// anything over agrees with checking it all again, which it wouldn't if this were carried over.
#[test]
fn what_took_a_default_from_text_the_edit_moved_is_checked_again() {
    let before = "def main(stdout: Stdout):\n    stdout.print(Point().x + edited(1))\n\n\
                  def edited(n: int) -> int:\n    return n * 2\n\n\
                  struct Point:\n    x = 1\n";
    let after = before.replace("    return n * 2\n", "    total = n * 2\n    return total\n");
    let (_, problems) = edited_moving(before, &after);
    assert!(problems.is_empty(), "{problems:?}");
    // And one calling a function whose parameter's default the edit moved.
    let before = "def main(stdout: Stdout):\n    stdout.print(scaled(1) + edited(1))\n\n\
                  def edited(n: int) -> int:\n    return n * 2\n\n\
                  def scaled(n: int, by = 3) -> int:\n    return n * by\n";
    let after = before.replace("    return n * 2\n", "    total = n * 2\n    return total\n");
    let (_, problems) = edited_moving(before, &after);
    assert!(problems.is_empty(), "{problems:?}");
}

/// A declaration whose text changed, a field's default here, carries nothing over; one whose method
/// changed carries over the rest, since a method is a function.
#[test]
fn a_declarations_text_decides_whether_anything_is_carried_over() {
    let before = "struct Point:\n    x = 1\n\n    def doubled(self) -> int:\n        return self.x * 2\n\n\
                  def main(stdout: Stdout):\n    stdout.print(Point().doubled() + other(1))\n\n\
                  def other(n: int) -> int:\n    return n + 1\n";
    let changed = before.replace("    x = 1\n", "    x = 2\n");
    assert_eq!(edited_moving(before, &changed), (Some(0), Vec::new()));
    let method =
        before.replace("        return self.x * 2\n", "        twice = self.x * 2\n        return twice\n");
    let (carried, problems) = edited_moving(before, &method);
    assert!(problems.is_empty(), "{problems:?}");
    assert!(carried.is_some_and(|carried| carried >= 1), "{carried:?}");
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
    // The last number of one digit on a line inside a function written at the top, before any
    // comment or text.
    let (mut inside, mut last, mut offset) = (false, None, 0);
    for line in source.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        if !line.starts_with(' ') && !line.trim().is_empty() {
            inside = line.starts_with("def ");
            continue;
        }
        if !inside {
            continue;
        }
        let code = line.split(['#', '"']).next().unwrap_or_default();
        let chars: Vec<(usize, char)> = code.char_indices().collect();
        let word = |at: Option<&(usize, char)>| {
            at.is_some_and(|(_, found)| found.is_alphanumeric() || *found == '_' || *found == '.')
        };
        for (index, (column, found)) in chars.iter().enumerate() {
            let before = index.checked_sub(1).and_then(|before| chars.get(before));
            if ('1'..='8').contains(found) && !word(before) && !word(chars.get(index + 1)) {
                last = Some(start + column);
            }
        }
    }
    let at = last?;
    let digit = source.get(at..=at)?.chars().next()?.to_digit(10)?;
    let bumped = char::from_digit(digit + 1, 10)?;
    Some(format!("{}{bumped}{}", source.get(..at)?, source.get(at + 1..)?))
}

/// The program with a comment put in as the second line of its first function written at the top,
/// which moves everything after it. Nothing when it has none.
fn with_a_line_put_in(source: &str) -> Option<String> {
    let first = source.find("\ndef ").map(|at| at + 1).or_else(|| source.starts_with("def ").then_some(0))?;
    let line_end = first + source.get(first..)?.find('\n')? + 1;
    Some(format!("{}    # an edit\n{}", source.get(..line_end)?, source.get(line_end..)?))
}

/// Every kernel, the eight real programs among them, every example and every real-world program
/// that builds is built again as it is, carrying everything over, then with a number in its last
/// function changed, then with a line put in near its top, told what that moved: each build finds
/// what a build that keeps nothing finds, and each check that carries anything over agrees with
/// checking it all again.
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
    let (mut programs, mut edits, mut moves) = (0, 0, 0);
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
        let (found, kept) = kept_build(&edited, Some(kept));
        assert_eq!(found, cold_build(&edited), "{shown}, with a number changed");
        edits += 1;
        // Then a line put in near the top, which moves most of the file.
        let Some(kept) = kept else { continue };
        let Some(moved) = with_a_line_put_in(&edited) else { continue };
        let text = |source: &str, span: Span| {
            let at = |offset: u32| usize::try_from(offset).unwrap_or(usize::MAX);
            source.get(at(span.start)..at(span.end)).map(str::to_owned)
        };
        let same_text =
            |was: Span, is: Span| text(&edited, was).is_some_and(|was| text(&moved, is) == Some(was));
        let edit = Edit { dirty: dirty_between(&edited, &moved), same_text: &same_text };
        let (found, _) = kept_build_after(&moved, Some(kept), Some(&edit));
        assert_eq!(found, cold_build(&moved), "{shown}, with a line put in");
        moves += 1;
    }
    assert!(programs >= 100, "only {programs} programs were built");
    assert!(edits >= 50, "only {edits} programs were edited");
    assert!(moves >= 50, "only {moves} programs had a line put in");
}

/// One dev build in 8 compares, picked by what it's built from; a release compiler compares only
/// when asked to.
#[test]
fn one_dev_build_in_eight_compares() {
    let compared = (0..64u64).filter(|inputs| fennec_check::compares_kept_check(*inputs)).count();
    assert_eq!(compared, if cfg!(debug_assertions) { 8 } else { 0 });
}
