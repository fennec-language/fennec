//! Every program in `examples/` runs and prints exactly its `.out` file, and every program in
//! `examples/errors/` fails with exactly its `.err` file, whether the type checker or the
//! interpreter catches the mistake. The examples are the language's working specification,
//! so the site and the compiler can't drift apart.

use std::fs;
use std::path::{Path, PathBuf};

use fennec_jit::JitError;

/// Examples the JIT may hand back to the interpreter whole, because it can't compile their
/// `main` yet. Every other example must run natively.
const INTERPRETED: &[&str] = &[];

/// The functions the JIT can't compile yet, in the examples that have any: generators,
/// functions with functions inside them, and `match`es with a guarded `case`. They run in
/// the interpreter.
const INTERPRETED_FUNCTIONS: &[(&str, &[&str])] = &[
    ("case_guards.fen", &["describe", "first_word", "size"]),
    ("functions_more.fen", &["countdown"]),
    ("generators.fen", &["checked", "countdown", "doubled", "holding", "naturals", "words"]),
    ("generators_fused.fen", &["checked", "odd", "running", "three", "upto"]),
    ("operators.fen", &["Grid.iter"]),
];

/// Examples that compile to plain machine code: nothing handed to the interpreter, and no
/// function needing the runtime's frames.
const PLAIN_MACHINE_CODE: &[&str] =
    &["calculator.fen", "hello.fen", "numbers.fen", "robot.fen", "string_building.fen"];

/// Checks how an example compiled, and that its run left nothing behind in the runtime.
fn check_report(name: &str, report: &fennec_jit::Report) {
    let expected = INTERPRETED_FUNCTIONS
        .iter()
        .find(|(example, _)| *example == name)
        .map_or(&[][..], |(_, names)| names);
    assert_eq!(report.interpreted, expected, "{name}: the JIT couldn't compile these");
    assert_eq!(
        (report.leftover_temps, report.leftover_frames, report.leftover_boxes),
        (0, 0, 0),
        "{name} left temporaries, frames or boxed values"
    );
    if PLAIN_MACHINE_CODE.contains(&name) {
        assert_eq!((report.handed_over, report.with_frames), (0, 0), "{name} should be plain machine code");
    }
}

fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

fn programs(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "fen") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

/// Runs a program the way `fennec run` does: its output, or its rendered errors.
fn run(source: &str, shown_as: &str) -> Result<String, String> {
    let mut module =
        fennec_syntax::parse(source).map_err(|diagnostic| diagnostic.render(shown_as, source))?;
    fennec_check::check_and_resolve(&mut module).map_err(|diagnostics| {
        let rendered: Vec<String> =
            diagnostics.iter().map(|diagnostic| diagnostic.render(shown_as, source)).collect();
        rendered.join("\n")
    })?;
    let mut out = Vec::new();
    fennec_interp::run(&module, &mut out).map_err(|diagnostic| diagnostic.render(shown_as, source))?;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// Runs a program's `test` blocks, returning each failure, rendered.
fn failing_tests(source: &str, shown_as: &str) -> Result<Vec<String>, String> {
    let render = |diagnostic: fennec_syntax::Diagnostic| diagnostic.render(shown_as, source);
    let mut module = fennec_syntax::parse(source).map_err(render)?;
    // What runs a module needs it resolved, as `fennec test` does.
    fennec_check::check_and_resolve(&mut module)
        .map_err(|problems| problems.into_iter().map(render).collect::<Vec<_>>().join("\n"))?;
    let mut out = std::io::sink();
    let mut interp = fennec_interp::Interp::new(&mut out).map_err(render)?;
    interp.load(&module).map_err(render)?;
    Ok(interp
        .run_tests()
        .into_iter()
        .filter_map(|outcome| outcome.failure.map(|failure| format!("{}: {}", outcome.name, render(failure))))
        .collect())
}

/// Runs a test on a stack as big as `fennec run` gives programs, so examples can nest calls
/// as deeply as the language allows.
fn on_big_stack(test: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    let worker = std::thread::Builder::new().stack_size(512 * 1024 * 1024).spawn(test)?;
    if let Err(panic) = worker.join() {
        std::panic::resume_unwind(panic);
    }
    Ok(())
}

#[test]
fn examples_print_their_expected_output() {
    on_big_stack(|| {
        let paths = programs(&examples_dir()).unwrap();
        assert!(!paths.is_empty(), "no examples found");
        for path in paths {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let source = fs::read_to_string(&path).unwrap();
            let expected = fs::read_to_string(path.with_extension("out")).unwrap();
            let actual = run(&source, &format!("examples/{name}"))
                .unwrap_or_else(|err| panic!("{name} failed:\n{err}"));
            assert_eq!(actual, expected, "{name} printed something different");
            let failures = failing_tests(&source, &format!("examples/{name}")).unwrap();
            assert!(failures.is_empty(), "{name} has failing tests:\n{}", failures.join("\n"));
        }
    })
    .unwrap();
}

#[test]
fn error_examples_fail_with_their_expected_message() {
    on_big_stack(|| {
        let paths = programs(&examples_dir().join("errors")).unwrap();
        assert!(!paths.is_empty(), "no error examples found");
        for path in paths {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let source = fs::read_to_string(&path).unwrap();
            let expected = fs::read_to_string(path.with_extension("err")).unwrap();
            let actual =
                run(&source, &format!("examples/errors/{name}")).expect_err(&format!("{name} should fail"));
            assert_eq!(actual, expected, "{name} failed with a different message");
        }
    })
    .unwrap();
}

#[test]
fn the_jit_runs_examples_exactly_like_the_interpreter() {
    on_big_stack(|| {
        let dir = examples_dir();
        let mut paths = programs(&dir).unwrap();
        paths.extend(programs(&dir.join("errors")).unwrap());
        for path in paths {
            let name = path.strip_prefix(&dir).unwrap().to_string_lossy().replace('\\', "/");
            let source = fs::read_to_string(&path).unwrap();
            let shown_as = format!("examples/{name}");
            let Ok(mut module) = fennec_syntax::parse(&source) else { continue };
            if fennec_check::check_and_resolve(&mut module).is_err() {
                continue;
            }
            let mut out = Vec::new();
            let actual = match fennec_jit::run_with_report(&module, &mut out) {
                Ok(report) => {
                    check_report(&name, &report);
                    Ok(String::from_utf8_lossy(&out).into_owned())
                }
                Err(JitError::Failed(diagnostic)) => Err(diagnostic.render(&shown_as, &source)),
                Err(JitError::Unsupported(why)) => {
                    assert!(
                        INTERPRETED.contains(&name.as_str()),
                        "{name} should compile natively, but: {}",
                        why.message
                    );
                    continue;
                }
            };
            assert_eq!(actual, run(&source, &shown_as), "{name} ran differently as native code");
        }
    })
    .unwrap();
}

#[test]
fn a_run_checking_only_what_it_reaches_compiles_the_same() {
    let home = std::env::temp_dir().join(format!("fennec-examples-reached-{}", std::process::id()));
    let cache = fennec_build::Cache::new(&home);
    let dir = examples_dir();
    let mut paths = programs(&dir).unwrap();
    paths.extend(programs(&dir.join("../bench/kernels")).unwrap());
    let on_a_big_stack = std::thread::Builder::new().stack_size(1 << 28).spawn(move || {
        for path in paths {
            let source = fs::read_to_string(&path).unwrap();
            let mut module = fennec_syntax::parse(&source).unwrap();
            let signatures = fennec_check::check_to_run(&mut module).unwrap();
            let Ok(whole) = fennec_jit::run_with_report(&module, &mut Vec::new()) else { continue };
            let inputs = fennec_interp::Inputs::default();
            let reached = fennec_jit::run_cached(
                &module,
                Some(&signatures),
                &mut Vec::new(),
                inputs,
                &cache,
                "reached",
            )
            .unwrap();
            assert_eq!(
                (reached.interpreted, reached.handed_over, reached.with_frames),
                (whole.interpreted, whole.handed_over, whole.with_frames),
                "{} compiled differently",
                path.display()
            );
        }
    });
    on_a_big_stack.unwrap().join().unwrap();
    fs::remove_dir_all(home).ok();
}

#[test]
fn a_run_checking_only_what_it_reaches_finds_the_same_mistakes() {
    for path in programs(&examples_dir().join("errors")).unwrap() {
        let source = fs::read_to_string(&path).unwrap();
        let Ok(mut module) = fennec_syntax::parse(&source) else { continue };
        let whole = fennec_check::check(&module).err();
        let reached = fennec_check::check_to_run(&mut module).err();
        assert_eq!(reached, whole, "{} found different mistakes", path.display());
    }
}

/// How many places each benchmark kernel isn't native can only go down: a change that adds
/// one fails here, and one that removes some lowers the count in `bench/fallbacks.txt`.
#[test]
fn benchmark_kernels_only_lose_fallbacks() {
    let bench = examples_dir().join("../bench");
    let written = fs::read_to_string(bench.join("fallbacks.txt")).unwrap();
    let allowed: Vec<(String, usize)> = written
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .map(|line| {
            let (kernel, count) = line.split_once(' ').unwrap();
            (kernel.to_owned(), count.parse().unwrap())
        })
        .collect();
    let on_a_big_stack = std::thread::Builder::new().stack_size(1 << 28).spawn(move || {
        let mut wrong = Vec::new();
        let paths = programs(&bench.join("kernels")).unwrap();
        for path in &paths {
            let kernel = path.file_stem().unwrap().to_string_lossy().into_owned();
            let module = fennec_syntax::parse(&fs::read_to_string(path).unwrap()).unwrap();
            let count = fennec_jit::fallbacks(&module).unwrap().len();
            match allowed.iter().find(|(name, _)| *name == kernel) {
                Some((_, limit)) if count == *limit => {}
                Some((_, limit)) => wrong.push(format!("{kernel}: {count} fallbacks, the list says {limit}")),
                None => wrong.push(format!("{kernel}: {count} fallbacks, and it isn't on the list")),
            }
        }
        for (name, _) in &allowed {
            if !paths.iter().any(|path| path.file_stem().is_some_and(|stem| stem == name.as_str())) {
                wrong.push(format!("{name} is on the list, but isn't a kernel"));
            }
        }
        assert!(
            wrong.is_empty(),
            "bench/fallbacks.txt is out of date. A count that went up means a kernel lost native code; \
             see `fennec dev fallbacks`. One that went down should be lowered there:\n{}",
            wrong.join("\n")
        );
    });
    on_a_big_stack.unwrap().join().unwrap();
}

#[test]
fn math_compiles_to_machine_code_however_it_is_imported() {
    let source = "import std.math\nfrom std import math\nfrom std.math import sqrt\n\ndef main(stdout: Stdout):\n    x = 2.25\n    stdout.print(sqrt(x) + math.sqrt(x) + std.math.sqrt(x), math.floor(x))\n";
    let module = fennec_syntax::parse(source).unwrap();
    let mut out = Vec::new();
    let report = fennec_jit::run_with_report(&module, &mut out).unwrap();
    assert_eq!(String::from_utf8_lossy(&out), "4.5 2\n");
    assert_eq!((report.handed_over, report.with_frames), (0, 0), "math should be plain machine code");
}

#[test]
fn a_second_run_reuses_every_function_and_an_edit_compiles_only_that_function() {
    let home = std::env::temp_dir().join(format!("fennec-examples-cache-{}", std::process::id()));
    let cache = fennec_build::Cache::new(&home);
    // What a native run through the cache printed, and how many functions came from it.
    let cached_run = |source: &str, cache: &fennec_build::Cache| {
        let module = fennec_syntax::parse(source).unwrap();
        let mut out = Vec::new();
        let inputs = fennec_interp::Inputs::default();
        let report = fennec_jit::run_cached(&module, None, &mut out, inputs, cache, "cached.fen").unwrap();
        (String::from_utf8_lossy(&out).into_owned(), report.cached)
    };
    let program = |greeting: &str| {
        format!(
            "def double(x: int) -> int:\n    x * 2\n\ndef greet(name: str) -> str:\n    \"{greeting}, \" + name\n\n\
            def main(stdout: Stdout):\n    stdout.print(greet(\"kit\"), double(21))\n"
        )
    };
    let (first, none) = cached_run(&program("hello"), &cache);
    let (second, all) = cached_run(&program("hello"), &cache);
    let (edited, rest) = cached_run(&program("hi there"), &cache);
    assert_eq!(
        (first.as_str(), second.as_str(), edited.as_str()),
        ("hello, kit 42\n", "hello, kit 42\n", "hi there, kit 42\n")
    );
    assert_eq!(none, 0, "the first run finds nothing");
    assert!(all >= 3, "the second run reuses every function, but reused {all}");
    assert_eq!(rest, all - 1, "the edit compiles only the edited function");
    let (_, added) =
        cached_run(&format!("def unrelated(x: int) -> int:\n    x + 1\n\n{}", program("hi there")), &cache);
    assert_eq!(added, all, "a new function leaves the machine code of everything else alone");
    let _ = fs::remove_dir_all(&home);
}

/// Each test's name and how it failed, if it did, sorted by name.
type Results = Vec<(String, Option<fennec_syntax::Diagnostic>)>;

fn results(outcomes: Vec<fennec_interp::TestOutcome>) -> Results {
    let mut results: Results = outcomes.into_iter().map(|outcome| (outcome.name, outcome.failure)).collect();
    results.sort_by(|a, b| a.0.cmp(&b.0));
    results
}

/// What a program's tests give natively, with the interpreter running those left over; what
/// they give in the interpreter alone; and how many ran natively.
fn native_and_interpreted(source: &str) -> Result<(Results, Results, usize), fennec_syntax::Diagnostic> {
    let mut module = fennec_syntax::parse(source)?;
    let signatures = fennec_check::check_and_resolve(&mut module).map_err(|mut found| found.remove(0))?;
    let names: Vec<String> = fennec_build::test_keys(&module).into_iter().map(|(name, _)| name).collect();
    let native = fennec_jit::run_tests(&module, &signatures, &names, None);
    let count = native.outcomes.len();
    let mut out = std::io::sink();
    let mut interp = fennec_interp::Interp::new(&mut out)?;
    interp.load(&module)?;
    let mut together = native.outcomes;
    together.extend(interp.run_tests_where(&|name| native.left.iter().any(|left| left == name)));
    let alone = interp.run_tests();
    Ok((results(together), results(alone), count))
}

#[test]
fn tests_give_the_same_results_natively_and_in_the_interpreter() {
    on_big_stack(|| {
        let mut natively = 0;
        for path in programs(&examples_dir()).unwrap() {
            let source = fs::read_to_string(&path).unwrap();
            if !source.contains("\ntest \"") {
                continue;
            }
            let (native, interpreted, count) = native_and_interpreted(&source).unwrap();
            assert_eq!(native, interpreted, "{}", path.display());
            natively += count;
        }
        assert!(natively > 0, "no example's tests ran natively");
    })
    .unwrap();
}

#[test]
fn failing_tests_report_the_same_natively_and_in_the_interpreter() {
    on_big_stack(|| {
        let source = "error Oops:\n    why: str\n\ndef double(x: int) -> int:\n    x * 2\n\n\
            def risky(x: int) -> int:\n    if x > 1: fail Oops(\"big\")\n    x\n\n\
            test \"passes\":\n    assert double(2) == 4\n\n\
            test \"compares wrongly\":\n    assert double(2) == 5\n\n\
            test \"plain condition\":\n    assert double(2) > 5 or double(1) == 9\n\n\
            test \"fails with an error\":\n    risky(3)?\n    assert True\n\n\
            test \"overflows\":\n    big = 9223372036854775807\n    assert double(big) == 0\n\n\
            test \"indexes past the end\":\n    items = [1]\n    assert items[double(2)] == 1\n\n\
            test \"passes after failures\":\n    assert double(1) == 2\n";
        let (native, interpreted, count) = native_and_interpreted(source).unwrap();
        assert_eq!(native, interpreted);
        assert_eq!(count, 7, "every test here runs natively");
    })
    .unwrap();
}

/// A `for` loop from the IR counts right up to the biggest int without overflowing, and
/// counts down right to the smallest, and what's computed from its count is checked as usual.
#[test]
fn for_loops_from_the_ir_count_to_the_ends_of_an_int() {
    let source = "def up() -> int:
    mut seen = 0
    for i in range(9223372036854775805, 9223372036854775807):
        seen += i % 7
    seen

def down() -> int:
    mut seen = 0
    for i in range(-9223372036854775807, -9223372036854775807 - 1, -1):
        seen += i % 7
    seen

def empty() -> int:
    mut seen = 0
    for i in range(5, 2):
        seen += i
    seen

def main(stdout: Stdout):
    stdout.print(up(), down(), empty())
";
    let mut module = fennec_syntax::parse(source).unwrap();
    fennec_check::check_and_resolve(&mut module).unwrap();
    let mut out = Vec::new();
    let report = fennec_jit::run_with_report(&module, &mut out).unwrap();
    assert_eq!(String::from_utf8_lossy(&out), run(source, "for_loops.fen").unwrap());
    assert_eq!(report.from_ir, ["up", "down", "empty"]);
}

/// The builtins on numbers come from the IR too, and behave like the interpreter's, failing
/// where it does.
#[test]
fn builtins_on_numbers_are_lowered_from_the_ir() {
    let source = "from std.math import sqrt, floor, ceil, sin, cos
import std.math as m

def mixed(n: int, x: float) -> float:
    a = float(n) + float(x)
    b = float(int(x) + int(n))
    c = float(min(n, 3) + max(n, 3)) + min(x, 0.5) + max(x, 0.5)
    d = sqrt(x) + float(floor(x)) + float(ceil(x)) + m.sqrt(float(n.abs()))
    e = sin(x) + cos(x)
    f = float(n.abs()) + x.abs() + float((-n).abs()) + (-x).abs()
    a + b + c + d + e + f

def too_big(x: float) -> int:
    int(x)

def main(stdout: Stdout):
    stdout.print(mixed(7, 2.5), mixed(-1, 0.25))
    stdout.print(too_big(1.0e300))
";
    let mut module = fennec_syntax::parse(source).unwrap();
    fennec_check::check_and_resolve(&mut module).unwrap();
    let mut out = Vec::new();
    let result = fennec_jit::run_with_report(&module, &mut out);
    let Err(JitError::Failed(failure)) = result else { panic!("too_big should fail") };
    let text = String::from_utf8_lossy(&out).into_owned();
    assert_eq!(Err(failure.render("builtins.fen", source)), run(source, "builtins.fen").map(|_| text));
    let mut module =
        fennec_syntax::parse(&source.replace("    stdout.print(too_big(1.0e300))\n", "")).unwrap();
    fennec_check::check_and_resolve(&mut module).unwrap();
    let report = fennec_jit::run_with_report(&module, &mut Vec::new()).unwrap();
    assert_eq!(report.from_ir, ["mixed"]);
}

/// A small function is lowered into its caller from the IR, and goes into itself one level.
/// A failure inside it is reported where it happens, as the interpreter does.
#[test]
fn small_functions_are_inlined_from_the_ir() {
    let source = "def half(n: int) -> int:
    if n == 0:
        return 0
    10 // (n - 1)

def twice(n: int) -> int:
    half(n) + half(n + 1)

def main(stdout: Stdout):
    stdout.print(twice(3), twice(5))
    stdout.print(twice(0))
";
    let mut module = fennec_syntax::parse(source).unwrap();
    fennec_check::check_and_resolve(&mut module).unwrap();
    let mut out = Vec::new();
    let result = fennec_jit::run_with_report(&module, &mut out);
    let Err(JitError::Failed(failure)) = result else { panic!("half(1) in twice(0) should fail") };
    assert_eq!(Err(failure.render("inlined.fen", source)), run(source, "inlined.fen"));
    let mut module = fennec_syntax::parse(&source.replace("    stdout.print(twice(0))\n", "")).unwrap();
    fennec_check::check_and_resolve(&mut module).unwrap();
    let report = fennec_jit::run_with_report(&module, &mut Vec::new()).unwrap();
    assert_eq!(report.from_ir, ["half", "twice"]);
}

/// What a branch shows about an int leaves out checks only where it's true: past the branch,
/// the same sum still overflows and fails as the interpreter does.
#[test]
fn branches_from_the_ir_only_leave_out_checks_they_prove() {
    let source = "def small(n: int) -> int:
    if n > 5:
        return n - 1
    if 2 > n:
        return n + 1
    n + 9223372036854775802

def main(stdout: Stdout):
    stdout.print(small(9), small(0), small(5))
    stdout.print(small(6), small(2))
    stdout.print(9223372036854775807 + small(1))
";
    let mut module = fennec_syntax::parse(source).unwrap();
    fennec_check::check_and_resolve(&mut module).unwrap();
    let mut out = Vec::new();
    let result = fennec_jit::run_with_report(&module, &mut out);
    let Err(JitError::Failed(failure)) = result else { panic!("the last sum should overflow") };
    assert_eq!(Err(failure.render("branches.fen", source)), run(source, "branches.fen"));
}

/// Number-only functions come from the typed IR.
#[test]
fn number_only_functions_are_lowered_from_the_ir() {
    let source = "def fib(n: int) -> int:
    if n < 2: return n
    fib(n - 1) + fib(n - 2)

def steps(n: int) -> int:
    mut at = n
    mut count = 0
    while at != 1:
        at = at // 2 if at % 2 == 0 else 3 * at + 1
        count += 1
    count

def main(stdout: Stdout):
    stdout.print(fib(10), steps(27))
";
    let mut module = fennec_syntax::parse(source).unwrap();
    fennec_check::check_and_resolve(&mut module).unwrap();
    let mut out = Vec::new();
    let report = fennec_jit::run_with_report(&module, &mut out).unwrap();
    assert_eq!(String::from_utf8_lossy(&out), "55 111\n");
    assert_eq!(report.from_ir, ["fib", "steps"]);
}

/// Runs a program in both engines, giving each one's output and failure.
fn run_both(source: &str) -> Result<[(String, Option<String>); 2], Vec<fennec_syntax::Diagnostic>> {
    let mut module = fennec_syntax::parse(source).map_err(|mistake| vec![mistake])?;
    fennec_check::check_to_run(&mut module)?;
    let mut interpreted = Vec::new();
    let failure = fennec_interp::run(&module, &mut interpreted).err();
    let first = (String::from_utf8_lossy(&interpreted).into_owned(), failure.map(|failed| failed.message));
    let mut native = Vec::new();
    let failure = fennec_jit::run_with_report(&module, &mut native).err();
    let failure = failure.map(|failed| match failed {
        JitError::Unsupported(diagnostic) | JitError::Failed(diagnostic) => diagnostic.message,
    });
    let second = (String::from_utf8_lossy(&native).into_owned(), failure);
    Ok([first, second])
}

#[test]
fn using_a_generator_twice_fails_in_both_engines() {
    let source = "def main(stdout: Stdout):
    made = (n for n in [1, 2])
    stdout.print(list(made))
    stdout.print(list(made))
";
    for (output, failure) in run_both(source).unwrap() {
        assert_eq!(output, "[1, 2]\n");
        assert!(failure.unwrap().contains("this generator has already been used"));
    }
}

#[test]
fn a_failure_with_nowhere_to_go_fails_in_both_engines() {
    let source = "error Bad:
    at: int

def checked() -> Iterator[int]:
    yield 1
    fail Bad(at=2)

def total(numbers: Iterator[int]) -> int:
    mut sum_so_far = 0
    for n in numbers:
        sum_so_far += n
    sum_so_far

def run_it() -> int:
    total(checked()?)

def main(stdout: Stdout):
    stdout.print(run_it())
";
    for (_, failure) in run_both(source).unwrap() {
        assert!(failure.unwrap().contains("can't pass up Bad(at=2)"));
    }
}

/// `in`, `remove` and `==` on lists use an item type's own `def ==`, the same natively as
/// in the interpreter.
#[test]
fn list_searches_use_an_items_own_equality_the_same_natively() {
    on_big_stack(|| {
        let source = "struct Pair:\n    a: int\n    b: int\n\n    def ==(self, other: Pair) -> bool:\n        \
            self.a + self.b == other.a + other.b\n\n\
            test \"lists\":\n    one = Pair(a=1, b=2)\n    two = Pair(a=2, b=1)\n    pairs = [Pair(a=0, b=0), two]\n    \
            assert one in pairs\n    assert Pair(a=9, b=9) not in pairs\n    assert [one] == [two]\n    \
            mut kept = [Pair(a=0, b=0), two]\n    kept.remove(one)\n    assert kept.len() == 1\n";
        let (native, interpreted, _) = native_and_interpreted(source).unwrap();
        assert_eq!(native, interpreted);
        assert_eq!(native, [("lists".to_owned(), None)]);
    })
    .unwrap();
}
