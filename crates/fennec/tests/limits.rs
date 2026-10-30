//! The interpreter's limits for hosts like the browser playground: a step budget, a shallower
//! call depth, and no files.

use std::sync::mpsc;
use std::time::Duration;

use fennec_interp::Interp;

/// Runs `work` on its own thread, giving up after ten seconds. Several tests run programs that
/// never end on their own, so a broken limit fails them quickly instead of hanging the run.
fn within_deadline<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (done, result) = mpsc::channel();
    std::thread::spawn(move || done.send(work()));
    result.recv_timeout(Duration::from_secs(10)).ok()
}

/// Loads `source` into an interpreter that `setup` configures, then runs `main`.
fn run(source: &str, setup: impl FnOnce(&mut Interp<'_>) + Send + 'static) -> Result<String, String> {
    let source = source.to_owned();
    within_deadline(move || {
        let module = fennec_syntax::parse(&source).map_err(|diagnostic| diagnostic.message)?;
        let mut out = Vec::new();
        let mut interp = Interp::new(&mut out).map_err(|diagnostic| diagnostic.message)?;
        setup(&mut interp);
        interp.run_program(&module).map_err(|diagnostic| diagnostic.message)?;
        drop(interp);
        Ok(String::from_utf8_lossy(&out).into_owned())
    })
    .unwrap_or_else(|| Err("still running after ten seconds".to_owned()))
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

/// Runs `source`'s tests with a step limit, returning the first failure's help.
fn first_failure_help(source: &str, steps: u64) -> Option<String> {
    let source = source.to_owned();
    within_deadline(move || {
        let module = fennec_syntax::parse(&source).ok()?;
        let mut out = std::io::sink();
        let mut interp = Interp::new(&mut out).ok()?;
        interp.limit_steps(steps);
        interp.load(&module).ok()?;
        interp.run_tests().into_iter().find_map(|outcome| outcome.failure)?.help
    })?
}

#[test]
fn property_tests_shrink_to_the_smallest_failure() {
    let help = first_failure_help("test \"small\" for n: int:\n    assert n < 5\n", 100_000).unwrap();
    assert!(help.starts_with("it failed for n = 5"), "{help}");
}

#[test]
fn running_out_of_steps_doesnt_shrink() {
    // Every input after the first failure would fail too, out of steps, so shrinking would
    // wrongly report n = 0, which passes.
    let source = "test \"spins\" for n: int:\n    while n != 0:\n        pass\n";
    let help = first_failure_help(source, 1000).unwrap();
    assert!(help.starts_with("it failed for n = "), "{help}");
    assert!(!help.starts_with("it failed for n = 0;"), "{help}");
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
    assert_eq!(error, "calls nested too deeply");
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

#[test]
#[expect(
    clippy::redundant_closure_for_method_calls,
    reason = "the method fits one lifetime, and `run` needs a closure that fits them all"
)]
fn the_clock_can_be_denied() {
    let source = "def main(stdout: Stdout, clock: Clock):\n    stdout.print(\"ran\")\n";
    assert_eq!(run(source, |_| {}).unwrap(), "ran\n");
    let error = run(source, |interp| interp.deny_clock()).unwrap_err();
    assert_eq!(error, "`main` can't receive `Clock` here: there's no clock");
}
