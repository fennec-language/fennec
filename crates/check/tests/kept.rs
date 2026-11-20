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

/// `source` parsed, or what's wrong with it.
fn parsed(source: &str) -> Result<fennec_syntax::Module, Vec<String>> {
    fennec_syntax::parse(source).map_err(|diagnostic| vec![format!("parse: {}", diagnostic.message)])
}

/// A build of `source` that keeps what its check finds, carrying over from `kept`, with each check
/// that carried anything over compared with checking it all again.
fn kept_build(source: &str, kept: Option<KeptCheck>) -> (Built, Option<KeptCheck>) {
    let mut module = match parsed(source) {
        Ok(module) => module,
        Err(problems) => return (Err(problems), None),
    };
    match check_to_build_keeping(&mut module, kept, true) {
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

/// A callee whose type parameter gets a bound its caller's argument doesn't fit has its callers
/// checked again, though what it takes and gives back is as it was.
#[test]
fn a_callee_given_a_bound_checks_its_callers_again() {
    let before = "interface Shape:\n    def area(self) -> float\n\n\
                  struct Dot:\n    x: int\n\n\
                  def main(stdout: Stdout):\n    stdout.print(measure(Dot(1)))\n\n\
                  def measure[T](item: T) -> int:\n    return 1\n";
    let after = before.replace("measure[T](item", "measure[T: Shape](item");
    let (_, problems) = edited(before, &after);
    assert_eq!(problems.len(), 1, "{problems:?}");
}

/// A callee that now changes what its caller loops over, or replaces what its caller holds a `ref`
/// into, is refused there, though the caller is written as it was: directly, or two calls down.
#[test]
fn what_a_callee_now_changes_under_a_ref_is_refused_in_its_caller() {
    let loops = "struct Bag:\n    items = list[int]()\n    total = 0\n\n\
                 def run(ref bag: Bag):\n    for item in bag.items:\n        bump(ref bag, item)\n\n\
                 def main(stdout: Stdout):\n    mut bag = Bag()\n    bag.items.append(1)\n    run(ref bag)\n    stdout.print(bag.total)\n\n\
                 def bump(ref bag: Bag, by: int):\n    bag.total += by\n";
    let (_, problems) = edited(loops, &loops.replace("bag.total += by", "bag.items.append(by)"));
    assert_eq!(problems.len(), 1, "{problems:?}");
    let deeper = loops.replace("        bump(ref bag, item)", "        middle(ref bag, item)").replace(
        "def bump(ref bag: Bag, by: int):",
        "def middle(ref bag: Bag, by: int):\n    bump(ref bag, by)\n\ndef bump(ref bag: Bag, by: int):",
    );
    let (_, problems) = edited(&deeper, &deeper.replace("bag.total += by", "bag.items.append(by)"));
    assert_eq!(problems.len(), 1, "{problems:?}");
    let lent = "struct Bag:\n    items = list[int]()\n\n\
                def main(stdout: Stdout):\n    mut bag = Bag()\n    ref first = bag.items\n    touch(ref bag)\n    first.append(1)\n    stdout.print(bag.items)\n\n\
                def touch(ref bag: Bag):\n    look(bag)\n\n\
                def look(bag: Bag):\n    pass\n";
    let (_, problems) = edited(lent, &lent.replace("    look(bag)\n", "    bag.items = list[int]()\n"));
    assert_eq!(problems.len(), 1, "{problems:?}");
}

/// A constant, or a field typed by its default, whose value calls a function that gives back
/// something else now has every function that reads it checked again, not only the first.
#[test]
fn a_constant_from_a_function_that_changed_checks_every_reader_again() {
    let changed = |source: &str| {
        source.replace("def pick() -> int:\n    return 2", "def pick() -> str:\n    return \"2\"")
    };
    let two_readers = "LIMIT = pick()\n\n\
                       def main(stdout: Stdout):\n    stdout.print(over(3), under(3))\n\n\
                       def over(n: int) -> bool:\n    return n > LIMIT\n\n\
                       def under(n: int) -> bool:\n    return n < LIMIT\n\n\
                       def pick() -> int:\n    return 2\n";
    let (_, problems) = edited(two_readers, &changed(two_readers));
    assert_eq!(problems.len(), 2, "{problems:?}");
    let first_still_fits = "LIMIT = pick()\n\n\
                            def main(stdout: Stdout):\n    stdout.print(LIMIT)\n    stdout.print(under(3))\n\n\
                            def under(n: int) -> bool:\n    return n < LIMIT\n\n\
                            def pick() -> int:\n    return 2\n";
    let (_, problems) = edited(first_still_fits, &changed(first_still_fits));
    assert_eq!(problems.len(), 1, "{problems:?}");
    let field = "struct Limit:\n    most = pick()\n\n\
                 def main(stdout: Stdout):\n    stdout.print(Limit().most)\n    stdout.print(under(3))\n\n\
                 def under(n: int) -> bool:\n    return n < Limit().most\n\n\
                 def pick() -> int:\n    return 2\n";
    let (_, problems) = edited(field, &changed(field));
    assert_eq!(problems.len(), 1, "{problems:?}");
}

/// A test that calls a function giving back something else now is checked again.
#[test]
fn a_test_calling_a_changed_function_is_checked_again() {
    let before = "def main(stdout: Stdout):\n    stdout.print(1)\n\n\
                  test \"doubles\":\n    total: int = double(2)\n    assert total == 4\n\n\
                  def double(n: int) -> int:\n    return n * 2\n";
    let after = before.replace("-> int:\n    return n * 2", "-> str:\n    return \"ab\"");
    let (_, problems) = edited(before, &after);
    assert_eq!(problems.len(), 2, "{problems:?}");
}

/// A `with` source whose body changed is written in where it's used as it is now: its callers keep
/// its whole definition, so they're checked again, though what it gives is as it was.
#[test]
fn a_with_sources_body_edit_checks_where_it_is_used_again() {
    let before = "def main(stdout: Stdout):\n    with opened(stdout) as one:\n        stdout.print(one)\n\n\
                  def opened(stdout: Stdout) -> with int:\n    stdout.print(\"open\")\n    lend 1\n    stdout.print(\"close\")\n";
    let after = before.replace("stdout.print(\"open\")", "stdout.print(\"opens\")");
    let (_, problems) = edited(before, &after);
    assert!(problems.is_empty(), "{problems:?}");
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
