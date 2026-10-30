//! The interpreter's limits for hosts like the browser playground: a step budget, a shallower
//! call depth, and no files.

use fennec_interp::Interp;

/// Loads `source` into an interpreter that `setup` configures, then runs `main`.
fn run(source: &str, setup: impl FnOnce(&mut Interp<'_>)) -> Result<String, String> {
    let module = fennec_syntax::parse(source).map_err(|diagnostic| diagnostic.message)?;
    let mut out = Vec::new();
    let mut interp = Interp::new(&mut out).map_err(|diagnostic| diagnostic.message)?;
    setup(&mut interp);
    interp.run_program(&module).map_err(|diagnostic| diagnostic.message)?;
    drop(interp);
    Ok(String::from_utf8_lossy(&out).into_owned())
}

const FOREVER: &str = "def main(stdout: Stdout):\n    while True:\n        pass\n";

const COUNT: &str =
    "def main(stdout: Stdout):\n    mut n = 0\n    while n < 10:\n        n += 1\n    stdout.print(n)\n";

#[test]
fn a_step_limit_stops_a_loop_that_never_ends() {
    let error = run(FOREVER, |interp| interp.limit_steps(1000)).unwrap_err();
    assert_eq!(error, "stopped after 1000 steps: this ran too long");
}

#[test]
fn a_step_limit_lets_short_programs_finish() {
    assert_eq!(run(COUNT, |interp| interp.limit_steps(1000)).unwrap(), "10\n");
    assert!(run(COUNT, |interp| interp.limit_steps(10)).is_err());
}

#[test]
fn each_test_gets_the_whole_step_budget() {
    let test = "    mut n = 0\n    while n < 50:\n        n += 1\n";
    let source = format!("test \"one\":\n{test}\ntest \"two\":\n{test}\ntest \"three\":\n{test}");
    let module = fennec_syntax::parse(&source).unwrap();
    let mut out = std::io::sink();
    let mut interp = Interp::new(&mut out).unwrap();
    interp.limit_steps(300);
    interp.load(&module).unwrap();
    let failures: Vec<_> = interp.run_tests().into_iter().filter_map(|outcome| outcome.failure).collect();
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn a_depth_limit_stops_deep_recursion() {
    let source = |depth: u32| {
        format!(
            "def down(n: int) -> int:\n    if n == 0:\n        return 0\n    return down(n - 1)\n\ndef main(stdout: Stdout):\n    stdout.print(down({depth}))\n"
        )
    };
    assert_eq!(run(&source(5), |interp| interp.limit_depth(10)).unwrap(), "0\n");
    let error = run(&source(20), |interp| interp.limit_depth(10)).unwrap_err();
    assert_eq!(error, "calls nested more than 10 deep");
}

#[test]
#[expect(
    clippy::redundant_closure_for_method_calls,
    reason = "the method fits one lifetime, and `run` needs a closure that fits them all"
)]
fn files_can_be_denied() {
    let source = "def main(stdout: Stdout, fs: Fs):\n    stdout.print(\"ran\")\n";
    assert_eq!(run(source, |_| {}).unwrap(), "ran\n");
    let error = run(source, |interp| interp.deny_files()).unwrap_err();
    assert_eq!(error, "`main` can't receive `Fs` here: there are no files");
}
