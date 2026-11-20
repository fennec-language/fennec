//! The `edits` target: a program is built, edited a piece at a time, and built again after each
//! edit, reusing what the build before it kept. What's reused has to make just what a build that
//! keeps nothing makes, so a stale reuse, which would be silent, is a failure here.
//!
//! - The checker keeps its check (`check_to_build_keeping`) and carries it over into the next,
//!   with its own comparison forced on. What it finds, a check or what's wrong, has to be what a
//!   whole check of the same text finds.
//! - LLVM keeps each function's object in the edit cache. A build reusing it, with
//!   `FENNEC_CACHE_AUDIT` writing each reused unit again to check it, has to make the same
//!   executable and the same objects, byte for byte, as a build with `FENNEC_NO_CACHE`.
//!
//! The program is made of pieces, each with a first way of writing it and others an edit switches
//! to: a body, a signature a test calls, a bound, a callee changing what its caller loops over,
//! directly or two calls down, a constant and a field default from a function whose type changes,
//! a `with` source's body, a function waiting inside a lock, a field's type, a function added,
//! removed or renamed, a field added, and a number that turns negative away from where it's used
//! as an index. The input picks 1 to 4 edits in a row, each switching one or two pieces and
//! sometimes moving one, so later declarations move. An edit may leave the program failing to
//! check, which is compared too.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use fennec_check::{KeptCheck, check_to_build_keeping, check_to_build_saying};

/// A piece of the program: its first way of writing it, then the others an edit may switch to.
struct Piece {
    variants: &'static [&'static str],
}

/// Every piece. `main` calls into each.
const PIECES: [Piece; 12] = [
    // A body.
    Piece {
        variants: &[
            "def double(n: int) -> int:\n    return n * 2\n",
            "def double(n: int) -> int:\n    return n * 2 + 1\n",
            "def double(n: int) -> int:\n    if n < 0:\n        return 0\n    return n - 1\n",
        ],
    },
    // A signature, with a test calling it.
    Piece {
        variants: &[
            "def label(n: int) -> int:\n    return n + 1\n\ntest \"label\":\n    total: int = label(2)\n    assert total == 3\n",
            "def label(n: int) -> str:\n    return f\"l{n}\"\n\ntest \"label\":\n    total: int = label(2)\n    assert total == 3\n",
            "def label(n: int) -> int:\n    return n + 2\n\ntest \"label\":\n    total: int = label(2)\n    assert total == 4\n",
        ],
    },
    // A bound its caller's argument doesn't fit.
    Piece {
        variants: &[
            "interface Shape:\n    def area(self) -> float\n\nstruct Dot:\n    x: int\n\ndef measure[T](item: T) -> int:\n    return 1\n",
            "interface Shape:\n    def area(self) -> float\n\nstruct Dot:\n    x: int\n\ndef measure[T: Shape](item: T) -> int:\n    return 1\n",
            "interface Shape:\n    def area(self) -> float\n\nstruct Dot:\n    x: int\n\ndef measure[T](item: T) -> int:\n    return 2\n",
        ],
    },
    // A callee changing what its caller loops over, directly or two calls down.
    Piece {
        variants: &[
            "struct Bag:\n    items = list[int]()\n    total = 0\n\ndef run(ref bag: Bag):\n    for item in bag.items:\n        bump(ref bag, item)\n\ndef bump(ref bag: Bag, by: int):\n    bag.total += by\n",
            "struct Bag:\n    items = list[int]()\n    total = 0\n\ndef run(ref bag: Bag):\n    for item in bag.items:\n        bump(ref bag, item)\n\ndef bump(ref bag: Bag, by: int):\n    bag.items.append(by)\n",
            "struct Bag:\n    items = list[int]()\n    total = 0\n\ndef run(ref bag: Bag):\n    for item in bag.items:\n        middle(ref bag, item)\n\ndef middle(ref bag: Bag, by: int):\n    bump(ref bag, by)\n\ndef bump(ref bag: Bag, by: int):\n    bag.total += by\n",
            "struct Bag:\n    items = list[int]()\n    total = 0\n\ndef run(ref bag: Bag):\n    for item in bag.items:\n        middle(ref bag, item)\n\ndef middle(ref bag: Bag, by: int):\n    bump(ref bag, by)\n\ndef bump(ref bag: Bag, by: int):\n    bag.items.append(by)\n",
        ],
    },
    // A constant from a function whose type changes.
    Piece {
        variants: &[
            "LIMIT = pick()\n\ndef over(n: int) -> bool:\n    return n > LIMIT\n\ndef under(n: int) -> bool:\n    return n < LIMIT\n\ndef pick() -> int:\n    return 2\n",
            "LIMIT = pick()\n\ndef over(n: int) -> bool:\n    return n > LIMIT\n\ndef under(n: int) -> bool:\n    return n < LIMIT\n\ndef pick() -> str:\n    return \"2\"\n",
            "LIMIT = pick()\n\ndef over(n: int) -> bool:\n    return n > LIMIT\n\ndef under(n: int) -> bool:\n    return n < LIMIT\n\ndef pick() -> int:\n    return 5\n",
        ],
    },
    // A field typed by its default, from a function whose type changes.
    Piece {
        variants: &[
            "struct Limit:\n    most = pick_most()\n\ndef pick_most() -> int:\n    return 2\n",
            "struct Limit:\n    most = pick_most()\n\ndef pick_most() -> str:\n    return \"2\"\n",
            "struct Limit:\n    most = pick_most()\n\ndef pick_most() -> int:\n    return -1\n",
        ],
    },
    // A `with` source's body.
    Piece {
        variants: &[
            "def opened(stdout: Stdout) -> with int:\n    stdout.print(\"open\")\n    lend 1\n    stdout.print(\"close\")\n",
            "def opened(stdout: Stdout) -> with int:\n    stdout.print(\"opens\")\n    lend 1\n    stdout.print(\"close\")\n",
            "def opened(stdout: Stdout) -> with int:\n    stdout.print(\"open\")\n    lend 2\n",
        ],
    },
    // A function that starts waiting, called inside a lock.
    Piece {
        variants: &[
            "struct Scores:\n    total = 0\n\ndef rest(clock: Clock):\n    _ = clock.now()\n",
            "struct Scores:\n    total = 0\n\ndef rest(clock: Clock):\n    clock.sleep(1ms)\n",
            "struct Scores:\n    total = 0\n\ndef rest(clock: Clock):\n    _ = clock.now()\n    _ = clock.now()\n",
        ],
    },
    // A field's type.
    Piece {
        variants: &[
            "struct Point:\n    x: int\n\ndef made_point() -> Point:\n    return Point(x=2)\n",
            "struct Point:\n    x: float\n\ndef made_point() -> Point:\n    return Point(x=2.5)\n",
            "struct Point:\n    x: int\n\ndef made_point() -> Point:\n    return Point(x=2.5)\n",
        ],
    },
    // A function added, removed or renamed.
    Piece {
        variants: &[
            "def spare() -> int:\n    return 1\n",
            "",
            "def spare_renamed() -> int:\n    return 1\n",
            "def spare() -> int:\n    return 1\n\ndef another_spare() -> int:\n    return 2\n",
        ],
    },
    // A field added, which changes the struct's layout.
    Piece {
        variants: &[
            "struct Rec:\n    a: int\n\ndef made_rec() -> Rec:\n    return Rec(a=1)\n\ndef rec_a(rec: Rec) -> int:\n    return rec.a\n",
            "struct Rec:\n    a: int\n    b = 0\n\ndef made_rec() -> Rec:\n    return Rec(a=1)\n\ndef rec_a(rec: Rec) -> int:\n    return rec.a\n",
            "struct Rec:\n    b = 7\n    a: int\n\ndef made_rec() -> Rec:\n    return Rec(a=1)\n\ndef rec_a(rec: Rec) -> int:\n    return rec.a + rec.b\n",
        ],
    },
    // A number that turns negative in one function, used as an index in another.
    Piece {
        variants: &[
            "struct Slot:\n    at = 0\n\ndef set_at(ref slot: Slot):\n    slot.at = 1\n\ndef read_slot(items: list[int]) -> int:\n    mut slot = Slot()\n    set_at(ref slot)\n    return items[slot.at]\n",
            "struct Slot:\n    at = 0\n\ndef set_at(ref slot: Slot):\n    slot.at = -1\n\ndef read_slot(items: list[int]) -> int:\n    mut slot = Slot()\n    set_at(ref slot)\n    return items[slot.at]\n",
            "struct Slot:\n    at = 0\n\ndef set_at(ref slot: Slot):\n    slot.at = 2\n\ndef read_slot(items: list[int]) -> int:\n    mut slot = Slot()\n    set_at(ref slot)\n    return items[slot.at]\n",
        ],
    },
];

/// `main`, which calls into every piece.
const MAIN: &str = "def main(stdout: Stdout, clock: Clock):
    stdout.print(double(3))
    stdout.print(label(2))
    stdout.print(measure(Dot(x=1)))
    mut bag = Bag()
    bag.items.append(1)
    run(ref bag)
    stdout.print(bag.total)
    stdout.print(over(3), under(3))
    stdout.print(Limit().most)
    with opened(stdout) as one:
        stdout.print(one)
    shared scores = Scores()
    lock scores as held:
        rest(clock)
        stdout.print(held.total)
    stdout.print(made_point().x)
    stdout.print(rec_a(made_rec()))
    stdout.print(read_slot([4, 5, 6]))
    return
";

/// What the input chooses: the order the pieces are written in at first, and each edit, as the
/// pieces it switches and the variant each switches to, and whether it moves the first of them to
/// the end.
struct Plan {
    order: Vec<usize>,
    edits: Vec<(Vec<(usize, usize)>, bool)>,
}

fn plan(input: &[u8]) -> Plan {
    let mut bytes = input.iter().copied();
    let mut byte = || bytes.next().unwrap_or(0);
    let mut order: Vec<usize> = (0..PIECES.len()).collect();
    // The first order: a few swaps the input picks.
    for _ in 0..usize::from(byte() % 4) {
        let (one, other) = (usize::from(byte()) % PIECES.len(), usize::from(byte()) % PIECES.len());
        order.swap(one, other);
    }
    let edits = (0..=usize::from(byte() % 4))
        .map(|_| {
            let switched = (0..=usize::from(byte() % 2))
                .map(|_| {
                    let piece = usize::from(byte()) % PIECES.len();
                    let count = PIECES.get(piece).map_or(1, |piece| piece.variants.len());
                    (piece, usize::from(byte()) % count)
                })
                .collect();
            (switched, byte() % 6 == 0)
        })
        .collect();
    Plan { order, edits }
}

/// The program with each piece written as `chosen` says, in `order`, then `main`.
fn written(order: &[usize], chosen: &[usize]) -> String {
    let mut out = String::new();
    for &piece in order {
        let variant = chosen.get(piece).copied().unwrap_or(0);
        let text = PIECES.get(piece).and_then(|piece| piece.variants.get(variant)).copied().unwrap_or("");
        if !text.is_empty() {
            let _ = writeln!(out, "{text}");
        }
    }
    out.push_str(MAIN);
    out
}

/// Each version of the program the input makes: the first, then one after each edit.
fn versions(plan: &Plan) -> Vec<String> {
    let mut order = plan.order.clone();
    let mut chosen = vec![0; PIECES.len()];
    let mut out = vec![written(&order, &chosen)];
    for (switched, moved) in &plan.edits {
        for &(piece, variant) in switched {
            if let Some(held) = chosen.get_mut(piece) {
                *held = variant;
            }
        }
        if *moved && let Some(&(first, _)) = switched.first() {
            order.retain(|&piece| piece != first);
            order.push(first);
        }
        out.push(written(&order, &chosen));
    }
    out
}

/// The program the input makes, each version after the one before it, for `fennec-fuzz show
/// edits`.
pub(crate) fn shown(input: &[u8]) -> String {
    versions(&plan(input)).join("\n# ---- the next edit ----\n\n")
}

/// What a check gives back: its signatures, shown with each open type variable's number taken out,
/// since two checks that agree can number theirs apart, and whether they're of the module as it's
/// left. Or what it finds wrong.
type Checked = Result<(String, bool), Vec<String>>;

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

fn as_checked(found: Result<(fennec_check::Signatures, bool), Vec<fennec_syntax::Diagnostic>>) -> Checked {
    found
        .map(|(signatures, as_left)| (without_var_numbers(&format!("{signatures:?}")), as_left))
        .map_err(|problems| problems.iter().map(|problem| format!("{problem:?}")).collect())
}

/// A check of `source` that keeps what it finds, carrying over from `kept`, with the checker's own
/// comparison on.
fn kept_check(source: &str, kept: Option<KeptCheck>) -> (Checked, Option<KeptCheck>) {
    let mut module = match fennec_syntax::parse(source) {
        Ok(module) => module,
        Err(diagnostic) => return (Err(vec![format!("parse: {}", diagnostic.message)]), None),
    };
    match check_to_build_keeping(&mut module, kept, true) {
        Ok((signatures, as_left, kept)) => (as_checked(Ok((signatures, as_left))), Some(kept)),
        Err(problems) => (as_checked(Err(problems)), None),
    }
}

/// A whole check of `source`, keeping nothing.
fn whole_check(source: &str) -> Checked {
    let mut module =
        fennec_syntax::parse(source).map_err(|diagnostic| vec![format!("parse: {}", diagnostic.message)])?;
    as_checked(check_to_build_saying(&mut module))
}

/// Each version checked carrying over what the check before it kept: each finds what a whole
/// check finds. How many versions carried anything over.
fn checks_agree(versions: &[String]) -> Result<usize, String> {
    let mut kept = None;
    let mut carried = 0;
    for (at, source) in versions.iter().enumerate() {
        let (found, next) = kept_check(source, kept);
        if next.as_ref().is_some_and(|next| next.carried_over() > 0) {
            carried += 1;
        }
        let whole = whole_check(source);
        if found != whole {
            return Err(format!(
                "after edit {at}, a check carrying over what the one before kept found\n{found:?}\nwhere a whole \
                 check found\n{whole:?}\nin:\n{source}"
            ));
        }
        kept = next;
    }
    Ok(carried)
}

/// `fennec build` of the program at `program` into `executable`, with `envs` set: whether it
/// built. A build that goes wrong, rather than finding the program wrong, is an error.
fn built(
    fennec: &Path,
    program: &Path,
    executable: &Path,
    envs: &[(&str, &Path)],
    cached: bool,
) -> Result<bool, String> {
    let mut command = Command::new(fennec);
    command.args(["build", "--llvm"]).arg(program).arg("-o").arg(executable).env("FENNEC_NO_DAEMON", "1");
    for (name, value) in envs {
        command.env(name, value);
    }
    if cached {
        command.env("FENNEC_CACHE_AUDIT", "1");
    } else {
        command.env("FENNEC_NO_CACHE", "1");
    }
    let built = crate::llvm::built_within(&mut command)?;
    let said = String::from_utf8_lossy(&built.stderr);
    if built.status.success() {
        return Ok(true);
    }
    if built.status.code() == Some(1) && !crate::llvm::BROKEN.iter().any(|broken| said.contains(broken)) {
        return Ok(false);
    }
    Err(format!("it didn't build ({}):\n{said}", built.status))
}

/// An empty folder at `path`, made afresh, reachable only by this user, as a build's home has to
/// be.
fn fresh_private(path: &Path) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(path);
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(path).map_err(|err| format!("{}: {err}", path.display()))
}

/// The files in `folder`, by name, with what each holds.
fn files_in(folder: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<(String, Vec<u8>)> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            std::fs::read(entry.path()).ok().map(|bytes| (name, bytes))
        })
        .collect();
    files.sort();
    files
}

/// Each version built in order with the edit cache, and each after the first built again with
/// nothing kept: where both build, the executables and the linked objects are the same, byte for
/// byte. Gives back how many versions were compared.
fn builds_agree(fennec: &Path, versions: &[String], work: &Path) -> Result<usize, String> {
    let (kept_home, cold_home) = (work.join("home-kept"), work.join("home-cold"));
    fresh_private(&kept_home)?;
    let program = work.join("program.fen");
    let mut compared = 0;
    for (at, source) in versions.iter().enumerate() {
        std::fs::write(&program, source).map_err(|err| format!("{}: {err}", program.display()))?;
        let (kept_objects, cold_objects) = (work.join("objects-kept"), work.join("objects-cold"));
        fresh_private(&kept_objects)?;
        let kept_executable = work.join("kept");
        let kept_built = built(
            fennec,
            &program,
            &kept_executable,
            &[("FENNEC_HOME", &kept_home), ("FENNEC_LINK_KEEP", &kept_objects)],
            true,
        )
        .map_err(|err| format!("version {at} built with the cache: {err}\n{source}"))?;
        if at == 0 || !kept_built {
            continue;
        }
        fresh_private(&cold_home)?;
        fresh_private(&cold_objects)?;
        let cold_executable = work.join("cold");
        let cold_built = built(
            fennec,
            &program,
            &cold_executable,
            &[("FENNEC_HOME", &cold_home), ("FENNEC_LINK_KEEP", &cold_objects)],
            false,
        )
        .map_err(|err| format!("version {at} built with nothing kept: {err}\n{source}"))?;
        if !cold_built {
            return Err(format!("version {at} built with the cache but not with nothing kept:\n{source}"));
        }
        let read = |path: &PathBuf| std::fs::read(path).map_err(|err| format!("{}: {err}", path.display()));
        if read(&kept_executable)? != read(&cold_executable)? {
            return Err(format!(
                "after edit {at}, the executable built with the cache differs from one built with nothing kept:\n{source}"
            ));
        }
        let (kept_files, cold_files) = (files_in(&kept_objects), files_in(&cold_objects));
        // A program whose whole build is kept links nothing, so it leaves no objects to compare.
        if !kept_files.is_empty() && kept_files != cold_files {
            let differing: Vec<&str> = kept_files
                .iter()
                .zip(&cold_files)
                .filter(|(kept, cold)| kept != cold)
                .map(|(kept, _)| kept.0.as_str())
                .collect();
            return Err(format!(
                "after edit {at}, the objects built with the cache differ from those built with nothing kept \
                 ({} against {} objects; differing: {differing:?}):\n{source}",
                kept_files.len(),
                cold_files.len()
            ));
        }
        compared += 1;
    }
    Ok(compared)
}

/// The target: every check carrying over agrees with a whole one, and where LLVM can build here,
/// every build reusing the edit cache makes what one keeping nothing makes.
pub(crate) fn edits(input: &[u8]) -> Result<(), String> {
    let versions = versions(&plan(input));
    checks_agree(&versions)?;
    if crate::llvm::ready().is_err() {
        return Ok(());
    }
    builds_agree(&crate::llvm::fennec(), &versions, &crate::llvm::work().join("edits"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{PIECES, Plan, builds_agree, checks_agree, versions};

    /// The first way of writing every piece makes a program that checks.
    #[test]
    fn the_first_program_checks() {
        let versions = versions(&Plan { order: (0..PIECES.len()).collect(), edits: Vec::new() });
        let first = versions.first().cloned().unwrap_or_default();
        let mut module = fennec_syntax::parse(&first).unwrap_or_else(|err| unreachable!("{err:?}\n{first}"));
        if let Err(problems) = fennec_check::check_to_build_saying(&mut module) {
            unreachable!("{problems:?}\n{first}");
        }
    }

    /// A short run of edits, as the gate runs: a body, a number turning negative away from where it
    /// indexes, a field added and a declaration moved, each built reusing the edit cache and with
    /// nothing kept, byte for byte the same. It needs `fennec` built beside the tests, and says so
    /// where it isn't rather than passing.
    #[test]
    fn a_short_run_of_edits_builds_the_same_with_the_cache_and_without() {
        let fennec = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/fennec");
        if !fennec.exists() {
            eprintln!("skipped: {} isn't built", fennec.display());
            return;
        }
        let plan = Plan {
            order: (0..PIECES.len()).collect(),
            edits: vec![
                (vec![(0, 1)], false),
                (vec![(11, 1)], false),
                (vec![(10, 1), (0, 2)], true),
                (vec![(11, 2), (10, 2)], false),
            ],
        };
        let work = crate::llvm::work().join("edits-test");
        let compared =
            builds_agree(&fennec, &versions(&plan), &work).unwrap_or_else(|err| unreachable!("{err}"));
        let _ = std::fs::remove_dir_all(&work);
        assert!(compared >= 3, "only {compared} edits were built both ways");
    }

    /// Every switch of every piece, one at a time and then back, checks carrying over just what a
    /// whole check finds, and some of them carry something over.
    #[test]
    fn every_single_edit_checks_carrying_over_what_a_whole_check_finds() {
        let mut carried = 0;
        for (piece, kind) in PIECES.iter().enumerate() {
            for variant in 1..kind.variants.len() {
                let plan = Plan {
                    order: (0..PIECES.len()).collect(),
                    edits: vec![(vec![(piece, variant)], false), (vec![(piece, 0)], false)],
                };
                carried += checks_agree(&versions(&plan)).unwrap_or_else(|err| unreachable!("{err}"));
            }
        }
        assert!(carried > 0, "no edit carried anything over");
    }
}
