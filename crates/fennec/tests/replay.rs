//! `fennec run --record` and `fennec replay`: a recorded run of a single-task program, replayed,
//! prints exactly what it printed, from the inputs it logged. A replay that asks for something its
//! recording doesn't have next stops, saying both. Skipped where native code can't be built, as
//! `llvm.rs` is.

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use crate::executables::{Within, fennec_command, threads_runtime_built};
use crate::llvm::ready;

/// How long one recording or replay of an example may take.
const DEADLINE: Duration = Duration::from_secs(120);

fn folder(name: &str) -> std::io::Result<fennec_temp::Temp> {
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec-replay-{name}-{}", std::process::id())),
    );
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// What a run printed, to standard output and error, and its exit code.
type Outcome = (String, String, Option<i32>);

fn outcome(ran: &Output) -> Outcome {
    let said = String::from_utf8_lossy(&ran.stderr).into_owned();
    // A program a signal stopped has no exit code, so which signal goes with what it said: a run
    // killed with nothing printed is otherwise only `None`, which says nothing of why.
    #[cfg(unix)]
    let said = match std::os::unix::process::ExitStatusExt::signal(&ran.status) {
        Some(signal) => format!("{said}[stopped by signal {signal}]"),
        None => said,
    };
    (String::from_utf8_lossy(&ran.stdout).into_owned(), said, ran.status.code())
}

/// Records a run of `program` with `input` on its standard input, then replays it, giving back both
/// outcomes.
fn recorded_and_replayed(program: &Path, input: &[u8], log: &Path) -> std::io::Result<(Outcome, Outcome)> {
    let mut record = fennec_command();
    // Without the program's tests first: a replay never runs them, nothing they do is recorded,
    // and `unchanged_tests` runs every example's.
    record.args(["run", "--no-test", "--record", &log.to_string_lossy()]);
    record.arg(program).arg("given").current_dir(program.parent().unwrap_or(Path::new(".")));
    let recorded = record.output_given(input, DEADLINE)?;
    // With nothing on standard input, so what it read can only come from the recording.
    let replayed = fennec_command()
        .args(["replay", &log.to_string_lossy()])
        .current_dir(program.parent().unwrap_or(Path::new(".")))
        .output_before(DEADLINE)?;
    Ok((without_build_hints(outcome(&recorded)), without_build_hints(outcome(&replayed))))
}

/// What a run said, less the hints its build gave. Only a build that wasn't cached gives them, so
/// the recording, which builds, has them when no test built the program first, and the replay,
/// which finds it cached, never does.
fn without_build_hints((out, said, code): Outcome) -> Outcome {
    let said = said.split_inclusive('\n').filter(|line| !line.starts_with("hint: ")).collect();
    (out, said, code)
}

const DICE: &str = "from std.random import Random

def main(stdout: Stdout, clock: Clock, entropy: Entropy, args: Args, env: Env):
    started = clock.now()
    mut dice = Random(seed=entropy.seed())
    stdout.print([dice.randint(1, high=6) for i in range(8)])
    stdout.print(clock.time())
    stdout.print(clock.now() - started)
    stdout.print(list(args), env.get(\"HOME\") != None)
";

#[test]
fn a_run_using_the_clock_and_random_numbers_replays_identically() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("dice")?;
    let program = dir.join("dice.fen");
    std::fs::write(&program, DICE)?;
    let log = dir.join("dice.log");
    let (recorded, replayed) = recorded_and_replayed(&program, b"", &log)?;
    assert_eq!(recorded.2, Some(0), "{}", recorded.1);
    assert_eq!(replayed, recorded);
    // Another run draws other numbers at another time: the replay's are the recording's.
    let again = outcome(&fennec_command().args(["run"]).arg(&program).arg("given").output_within()?);
    assert_ne!(again.0, recorded.0, "a fresh run printed the same");
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// `fennec run --no-test --record` records as `--no-test` runs, without the program's tests first,
/// which a replay never runs either: a program whose test fails is recorded and replays the same,
/// where `--record` alone stops at the failing test and records nothing that ran.
#[test]
fn a_recording_without_the_tests_first_replays_as_it_ran() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("untested")?;
    let program = dir.join("untested.fen");
    std::fs::write(
        &program,
        "def main(stdout: Stdout):\n    stdout.print(\"ran\")\n    return\n\ntest \"one is two\":\n    assert 1 == 2\n",
    )?;
    let log = dir.join("untested.log");
    let tested =
        fennec_command().args(["run", "--record", &log.to_string_lossy()]).arg(&program).output_within()?;
    assert!(!tested.status.success() && !outcome(&tested).0.contains("ran"), "{}", outcome(&tested).1);
    let recorded = outcome(
        &fennec_command()
            .args(["run", "--no-test", "--record", &log.to_string_lossy()])
            .arg(&program)
            .output_within()?,
    );
    assert_eq!((recorded.0.as_str(), recorded.2), ("ran\n", Some(0)), "{}", recorded.1);
    let replayed = outcome(&fennec_command().args(["replay", &log.to_string_lossy()]).output_within()?);
    assert_eq!(without_build_hints(replayed), without_build_hints(recorded));
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn a_replay_that_asks_for_other_inputs_than_its_recording_stops_naming_both() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("diverges")?;
    let program = dir.join("dice.fen");
    std::fs::write(&program, DICE)?;
    let log = dir.join("dice.log");
    let (recorded, _) = recorded_and_replayed(&program, b"", &log)?;
    assert_eq!(recorded.2, Some(0), "{}", recorded.1);
    // One more reading of the clock than was recorded, before the time.
    std::fs::write(
        &program,
        DICE.replace(
            "    stdout.print(clock.time())",
            "    stdout.print(clock.now())\n    stdout.print(clock.time())",
        ),
    )?;
    let more = outcome(
        &fennec_command().args(["replay", &log.to_string_lossy()]).current_dir(&dir).output_within()?,
    );
    assert_ne!(more.2, Some(0));
    assert!(
        more.1.contains("asked for `Clock.now`, where its recording asked for `Clock.time`"),
        "{}",
        more.1
    );
    // One fewer, the last: the recording has it left over.
    std::fs::write(
        &program,
        DICE.replace("stdout.print(list(args), env.get(\"HOME\") != None)", "stdout.print(list(args))"),
    )?;
    let fewer = outcome(
        &fennec_command().args(["replay", &log.to_string_lossy()]).current_dir(&dir).output_within()?,
    );
    assert_ne!(fewer.2, Some(0));
    assert!(fewer.1.contains("ended where its recording asked for `Env.get` next"), "{}", fewer.1);
    // A crash in place of the last: the recording has it left over all the same, and the replay
    // says so, as well as how it crashed. It said only the crash (#1917).
    std::fs::write(
        &program,
        DICE.replace(
            "stdout.print(list(args), env.get(\"HOME\") != None)",
            "stdout.print(list(args), 1 // (len(list(args)) - len(list(args))))",
        ),
    )?;
    let crashed = outcome(
        &fennec_command().args(["replay", &log.to_string_lossy()]).current_dir(&dir).output_within()?,
    );
    assert_ne!(crashed.2, Some(0));
    assert!(crashed.1.contains("ended where its recording asked for `Env.get` next"), "{}", crashed.1);
    assert!(crashed.1.contains("the replay crashed first: division by zero"), "{}", crashed.1);
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A run recorded with `FENNEC_PARALLEL_STRESS` replays as it ran without it, and one recorded
/// without it replays as it ran with it: a replay has the setting its recording had. It cuts a
/// call's ranges to one item each, and ranges are named by where they start, so a replay with it
/// changed ran other ranges, and printed a line or two of the run's hundred, with nothing said but
/// the program's crash (#1917).
#[test]
fn a_replay_has_the_parallel_stress_its_recording_had() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("stress")?;
    let program = dir.join("program.fen");
    std::fs::write(&program, include_str!("fixtures/pieces_print_then_one_crashes.fen"))?;
    let log = dir.join("recorded.log");
    for (recorded_stressed, replayed_stressed) in [(true, false), (false, true)] {
        let _ = std::fs::remove_file(&log);
        let mut record = fennec_command();
        record.env("FENNEC_THREADS", "4").env_remove("FENNEC_PARALLEL_STRESS");
        if recorded_stressed {
            record.env("FENNEC_PARALLEL_STRESS", "1");
        }
        record.args(["run", "--record", &log.to_string_lossy()]).arg(&program).current_dir(&dir);
        let recorded = without_build_hints(outcome(&record.output_before(DEADLINE)?));
        assert_eq!(recorded.2, Some(1), "{}", recorded.1);
        let mut replay = fennec_command();
        replay.env("FENNEC_THREADS", "4").env_remove("FENNEC_PARALLEL_STRESS");
        if replayed_stressed {
            replay.env("FENNEC_PARALLEL_STRESS", "1");
        }
        replay.args(["replay", &log.to_string_lossy()]).current_dir(&dir);
        let replayed = without_build_hints(outcome(&replay.output_before(DEADLINE)?));
        assert_eq!(replayed, recorded, "recorded stressed {recorded_stressed}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

const NAP: &str = "from std.time import Duration

def main(stdout: Stdout, clock: Clock):
    clock.sleep(Duration.millis(5))
    stdout.print(clock.time())
";

/// Native code's `clock.sleep` is in its recording, so a replay of a program that no longer sleeps
/// there stops, naming both.
#[test]
fn a_native_sleep_is_recorded_and_a_replay_without_it_stops() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("nap")?;
    let program = dir.join("nap.fen");
    std::fs::write(&program, NAP)?;
    let log = dir.join("nap.log");
    let (recorded, replayed) = recorded_and_replayed(&program, b"", &log)?;
    assert_eq!(recorded.2, Some(0), "{}", recorded.1);
    assert_eq!(replayed, recorded);
    std::fs::write(&program, NAP.replace("    clock.sleep(Duration.millis(5))\n", ""))?;
    let woke = outcome(
        &fennec_command().args(["replay", &log.to_string_lossy()]).current_dir(&dir).output_within()?,
    );
    assert_ne!(woke.2, Some(0));
    assert!(
        woke.1.contains("asked for `Clock.time`, where its recording asked for `Clock.sleep`"),
        "{}",
        woke.1
    );
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

const DRAWN: &str = "def main(stdout: Stdout, clock: Clock, entropy: Entropy):
    drawn = [entropy.seed() % 1000 + item for item in parallel(range(64))]
    stdout.print(drawn)
    read = [clock.time() for item in parallel(range(16))]
    stdout.print(read)
";

/// A `parallel` call's ranges each take inputs of their own, in whatever order the threads run
/// them: each range replays its own, so the output is the recording's every time, on as many
/// workers as it was recorded with, whatever the replay is told.
#[test]
fn a_parallel_call_whose_ranges_take_inputs_replays_identically() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("drawn")?;
    let program = dir.join("drawn.fen");
    std::fs::write(&program, DRAWN)?;
    let log = dir.join("drawn.log");
    let mut record = fennec_command();
    // Every item a range of its own, on the pool, so the ranges take turns on the threads.
    record.env("FENNEC_THREADS", "4").env("FENNEC_PARALLEL_STRESS", "1");
    record.args(["run", "--record", &log.to_string_lossy()]);
    record.arg(&program).current_dir(&dir);
    let recorded = outcome(&record.output_within()?);
    assert_eq!(recorded.2, Some(0), "{}", recorded.1);
    for threads in ["4", "1", "7", "4", "2"] {
        let replayed = outcome(
            &fennec_command()
                .env("FENNEC_THREADS", threads)
                .env("FENNEC_PARALLEL_STRESS", "1")
                .args(["replay", &log.to_string_lossy()])
                .current_dir(&dir)
                .output_within()?,
        );
        assert_eq!(replayed, recorded, "replayed with FENNEC_THREADS={threads}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A comprehension over `parallel` whose pieces sleep, one in five of them failing.
const PIECES_FAILING: &str = "error TooFar:
    at: int

def checked(number: int, clock: Clock) -> int fails TooFar:
    clock.sleep(1ms)
    if number % 5 == 4:
        fail TooFar(at=number)
    return number

def lowest(clock: Clock) -> int fails TooFar:
    passed = [checked(number, clock=clock)? for number in parallel(range(12))]
    return len(passed)

def main(clock: Clock, stdout: Stdout):
    passed = lowest(clock) except why:
        stdout.print(f\"{why}\")
        return
    stdout.print(passed)
";

/// A `for` loop over `parallel` whose pieces work and print, one crashing. A loop's body that
/// fails with `?` runs as a plain loop, so a crash is how a piece of a loop fails.
const PIECES_FAILING_IN_A_LOOP: &str = "def busy(turns: int) -> int:
    return sum(step % 7 for step in range(turns)) % 2

def checked(number: int, stdout: Stdout):
    worked = busy(100000)
    stdout.print(\"did\", number, 1000 // (77 - number) + worked)

def main(stdout: Stdout):
    for number in parallel(range(200)):
        checked(number, stdout=stdout)
    stdout.print(\"never said\")
";

/// A sum over `parallel` of enough items to be cut into many leaves, whose pieces sleep now and
/// then and print, one crashing.
const PIECES_FAILING_IN_A_SUM: &str = "def weighed(number: int, clock: Clock, stdout: Stdout) -> int:
    if number % 97 == 0:
        clock.sleep(1ms)
        stdout.print(\"at\", number)
    return 1000 // (2100 - number)

def main(clock: Clock, stdout: Stdout):
    added = sum(weighed(number, clock=clock, stdout=stdout) for number in parallel(range(20000)))
    stdout.print(\"never said\", added)
";

/// The first piece fails at once and the rest sleep, so most pieces never begin.
const FIRST_PIECE_FAILING: &str = "error TooFar:
    at: int

def checked(number: int, clock: Clock) -> int fails TooFar:
    if number == 0:
        fail TooFar(at=number)
    clock.sleep(5ms)
    return number

def lowest(clock: Clock) -> int fails TooFar:
    passed = [checked(number, clock=clock)? for number in parallel(range(64))]
    return len(passed)

def main(clock: Clock, stdout: Stdout):
    passed = lowest(clock) except why:
        stdout.print(f\"{why}\")
        return
    stdout.print(passed)
";

/// Records `source` at 1 thread and at 4, with every piece on the pool or not, and replays each
/// recording `rounds` times at each of 1, 4 and 16 threads: every recording ends with `ends`, and
/// every replay is the recorded run.
fn replays_at_one_four_and_sixteen(
    name: &str,
    source: &str,
    rounds: usize,
    ends: i32,
) -> std::io::Result<()> {
    let dir = folder(name)?;
    let program = dir.join("program.fen");
    std::fs::write(&program, source)?;
    for (recorded_threads, stress) in [("1", "0"), ("4", "0"), ("4", "1")] {
        let log = dir.join(format!("recorded-{recorded_threads}-{stress}.log"));
        let mut record = fennec_command();
        record.env("FENNEC_THREADS", recorded_threads).env("FENNEC_PARALLEL_STRESS", stress);
        record.args(["run", "--record", &log.to_string_lossy()]).arg(&program).current_dir(&dir);
        let recorded = outcome(&record.output_before(DEADLINE)?);
        assert_eq!(recorded.2, Some(ends), "{name} recorded at {recorded_threads}: {}", recorded.1);
        for round in 0..rounds {
            for threads in ["1", "4", "16"] {
                let replayed = outcome(
                    &fennec_command()
                        .env("FENNEC_THREADS", threads)
                        .env("FENNEC_PARALLEL_STRESS", stress)
                        .args(["replay", &log.to_string_lossy()])
                        .current_dir(&dir)
                        .output_before(DEADLINE)?,
                );
                assert_eq!(
                    replayed, recorded,
                    "{name} recorded at {recorded_threads} (stress {stress}), replay {round} at {threads}"
                );
            }
        }
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A `parallel` call where a piece fails replays from its own recording. Which later pieces had
/// begun when the failure was noted depends on timing, and pieces that hadn't don't begin, so a
/// replay used to run another set: 11 of 12 asked for an input its recording didn't have, or ended
/// with one left over (#1804). Now the recording says which pieces a failure kept from starting,
/// and a replay skips exactly those.
#[test]
fn a_parallel_call_where_a_piece_fails_replays_the_pieces_it_ran() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    replays_at_one_four_and_sixteen("pieces-failing", PIECES_FAILING, 17, 0)?;
    replays_at_one_four_and_sixteen("first-piece-failing", FIRST_PIECE_FAILING, 17, 0)
}

/// A launched task stopped inside a `parallel` call whose pieces work a while and then sleep:
/// pieces that haven't begun when the stop comes don't begin.
const STOPPED_IN_A_CALL: &str = "from std.concurrent import Crashed

def busy(turns: int) -> int:
    return sum(step % 7 for step in range(turns)) % 2

def slow(number: int, clock: Clock, stdout: Stdout) -> int:
    worked = busy(12000000)
    stdout.print(\"worked\", number)
    clock.sleep(2h)
    stdout.print(\"never said\", number)
    return number + worked

def works(clock: Clock, stdout: Stdout):
    made = [slow(number, clock=clock, stdout=stdout) for number in parallel(range(32))]
    stdout.print(\"never said\", len(made))

def main(clock: Clock, stdout: Stdout) fails Crashed:
    with launch(() => works(clock, stdout=stdout), stop_after=400ms)?:
        pass
    stdout.print(\"the task is over\")
";

/// The same with the stop sooner, before most pieces have begun.
const STOPPED_EARLY_IN_A_CALL: &str = "from std.concurrent import Crashed

def busy(turns: int) -> int:
    return sum(step % 7 for step in range(turns)) % 2

def slow(number: int, clock: Clock, stdout: Stdout) -> int:
    worked = busy(12000000)
    stdout.print(\"worked\", number)
    clock.sleep(2h)
    stdout.print(\"never said\", number)
    return number + worked

def works(clock: Clock, stdout: Stdout):
    made = [slow(number, clock=clock, stdout=stdout) for number in parallel(range(32))]
    stdout.print(\"never said\", len(made))

def main(clock: Clock, stdout: Stdout) fails Crashed:
    with launch(() => works(clock, stdout=stdout), stop_after=60ms)?:
        pass
    stdout.print(\"the task is over\")
";

/// The same with low pieces that work longest, so a lower piece is still to begin when a higher one
/// has failed as stopped: the stop, not the failure, keeps it from beginning, and the recording
/// says so (`not begun`).
const STOPPED_WITH_LOW_PIECES_TO_BEGIN: &str = "from std.concurrent import Crashed

def busy(turns: int) -> int:
    return sum(step % 7 for step in range(turns)) % 2

def slow(number: int, clock: Clock, stdout: Stdout) -> int:
    worked = busy(400000 * (64 - number))
    stdout.print(\"worked\", number)
    clock.sleep(2h)
    stdout.print(\"never said\", number)
    return number + worked

def works(clock: Clock, stdout: Stdout):
    made = [slow(number, clock=clock, stdout=stdout) for number in parallel(range(64))]
    stdout.print(\"never said\", len(made))

def main(clock: Clock, stdout: Stdout) fails Crashed:
    with launch(() => works(clock, stdout=stdout), stop_after=150ms)?:
        pass
    stdout.print(\"the task is over\")
";

/// A launched task's `parallel` call that its stop finds replays the pieces it began. Which had
/// begun depends on timing, and nothing said which a stop kept from beginning, so a replay began
/// others, asking for what its recording didn't have: 48 of 48 replays (#1916). Now the recording
/// says, and a replay keeps exactly those from beginning.
#[test]
fn a_parallel_call_its_task_s_stop_finds_replays_the_pieces_it_began() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    replays_at_one_four_and_sixteen("stopped-in-a-call", STOPPED_IN_A_CALL, 3, 0)?;
    replays_at_one_four_and_sixteen("stopped-early-in-a-call", STOPPED_EARLY_IN_A_CALL, 3, 0)?;
    replays_at_one_four_and_sixteen(
        "stopped-with-low-pieces-to-begin",
        STOPPED_WITH_LOW_PIECES_TO_BEGIN,
        3,
        0,
    )
}

/// The same in a `for` loop and in a sum, which split their pieces their own way, and whose pieces
/// fail by crashing, which ends the program: what it printed before is the recording's.
#[test]
fn a_parallel_loop_or_sum_where_a_piece_fails_replays_the_pieces_it_ran() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    replays_at_one_four_and_sixteen("pieces-failing-in-a-loop", PIECES_FAILING_IN_A_LOOP, 5, 1)?;
    replays_at_one_four_and_sixteen("pieces-failing-in-a-sum", PIECES_FAILING_IN_A_SUM, 5, 1)
}

const DRAWN_IN_A_TASK: &str = "from std.concurrent import Crashed

def drawn(clock: Clock, entropy: Entropy) -> int:
    clock.sleep(1ms)
    return entropy.seed() % 1000

def some(item: int, clock: Clock, entropy: Entropy) -> int:
    if item % 400 != 0:
        return item
    clock.sleep(1ms)
    return entropy.seed() % 1000

def draws(place: str, stdout: Stdout, clock: Clock, entropy: Entropy):
    stdout.print(place, [drawn(clock, entropy=entropy) + item for item in parallel(range(24))])
    stdout.print(place, sum(some(item, clock=clock, entropy=entropy) for item in parallel(range(3000))))

def main(stdout: Stdout, clock: Clock, entropy: Entropy) fails Crashed:
    draws(\"main\", stdout=stdout, clock=clock, entropy=entropy)
    with launch(() => draws(\"task\", stdout=stdout, clock=clock, entropy=entropy))?:
        pass
";

/// A launched task's comprehension and waiting sum hand their pieces to the workers, as `main`'s
/// do, with the pieces reading a copy of what they capture. Each piece still replays its own
/// inputs, by its place in its call, so the recorded run comes out again, `main`'s calls and the
/// task's alike: recorded on 1, 4 and 16 threads, with the lists cut as they come and cut down to an
/// item each, and replayed on other numbers of threads than it was recorded with.
///
/// No piece here fails and no task is stopped: neither replays yet, from `main` either (#1804,
/// #1805).
#[test]
fn a_task_s_parallel_calls_whose_pieces_take_inputs_replay_identically() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("drawn-in-a-task")?;
    let program = dir.join("drawn_in_a_task.fen");
    std::fs::write(&program, DRAWN_IN_A_TASK)?;
    for (recorded_on, stressed) in [("1", false), ("4", false), ("16", false), ("4", true), ("16", true)] {
        let log = dir.join(format!("drawn-{recorded_on}-{stressed}.log"));
        let on = |threads: &str| {
            let mut command = fennec_command();
            command.env("FENNEC_THREADS", threads).current_dir(&dir);
            if stressed {
                command.env("FENNEC_PARALLEL_STRESS", "1");
            }
            command
        };
        let how = format!("recorded with FENNEC_THREADS={recorded_on}, stressed: {stressed}");
        let recorded = outcome(
            &on(recorded_on)
                .args(["run", "--record", &log.to_string_lossy()])
                .arg(&program)
                .output_before(DEADLINE)?,
        );
        assert_eq!(recorded.2, Some(0), "{how}: {}", recorded.1);
        assert_eq!(recorded.0.lines().count(), 4, "{how}: {}", recorded.0);
        for threads in ["1", "4", "7", "16", "1", "4"] {
            let replayed =
                outcome(&on(threads).args(["replay", &log.to_string_lossy()]).output_before(DEADLINE)?);
            assert_eq!(replayed, recorded, "{how}, replayed with {threads}");
        }
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

const SERVER: &str = "from std.net import NetError

struct Total:
    bytes = 0

    def add(ref self, count: int):
        self.bytes += count

def main(net: Net, stdout: Stdout) fails NetError:
    shared total = Total()
    with net.listen(0)? as server:
        stdout.print(server.port)
        for conn in parallel(server):
            line = conn.read_line()? else \"\"
            if line == \"stop\":
                break
            lock total as ref held:
                held.add(len(line))
            conn.write(f\"{len(line)}\\n\")?
    lock total as held:
        stdout.print(held.bytes)
";

/// Asked to record or replay and unable to, a program says so and stops, rather than run live as if
/// nothing was asked of it.
#[test]
fn a_recording_that_cant_be_opened_stops_the_program() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("unopenable")?;
    let program = dir.join("hello.fen");
    std::fs::write(&program, "def main(stdout: Stdout, clock: Clock):\n    stdout.print(clock.now())\n")?;
    let executable = dir.join("hello");
    let built = fennec_command()
        .args(["build", "--llvm"])
        .arg(&program)
        .arg("-o")
        .arg(&executable)
        .output_within()?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let missing = dir.join("no such folder").join("log");
    for variable in ["FENNEC_RECORD", "FENNEC_REPLAY"] {
        let ran = std::process::Command::new(&executable).env(variable, &missing).output_within()?;
        assert_eq!(ran.status.code(), Some(1), "{variable}");
        assert!(ran.stdout.is_empty(), "{variable}");
        assert!(String::from_utf8_lossy(&ran.stderr).contains("no such folder"), "{variable}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A native server's rounds are each a task named by its connection, and a replay runs them again
/// with no socket, each reading what its connection sent, until the recording runs out.
#[test]
fn a_recorded_server_replays_its_rounds_with_no_network() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("server")?;
    let program = dir.join("server.fen");
    std::fs::write(&program, SERVER)?;
    let log = dir.join("server.log");
    let mut record = fennec_command()
        .args(["run", "--record", &log.to_string_lossy()])
        .arg(&program)
        .env("FENNEC_THREADS", "4")
        .current_dir(&dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let mut out =
        std::io::BufReader::new(record.stdout.take().ok_or_else(|| std::io::Error::other("no stdout"))?);
    let mut port = String::new();
    std::io::BufRead::read_line(&mut out, &mut port)?;
    let port: u16 = port.trim().parse().map_err(std::io::Error::other)?;
    for n in 0..20 {
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port))?;
        std::io::Write::write_all(&mut stream, format!("hello {}\n", "x".repeat(n)).as_bytes())?;
        let mut back = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(&stream), &mut back)?;
        assert_eq!(back.trim(), (6 + n).to_string());
    }
    let mut stopping = std::net::TcpStream::connect(("127.0.0.1", port))?;
    std::io::Write::write_all(&mut stopping, b"stop\n")?;
    let mut rest = String::new();
    std::io::Read::read_to_string(&mut out, &mut rest)?;
    let status = record.wait()?;
    assert!(status.success(), "the recorded server failed");
    let recorded = format!("{port}\n{rest}");
    assert_eq!(rest.trim(), (0..20).map(|n| 6 + n).sum::<usize>().to_string());
    for threads in ["4", "1"] {
        let replayed = fennec_command()
            .env("FENNEC_THREADS", threads)
            .args(["replay", &log.to_string_lossy()])
            .current_dir(&dir)
            .output_within()?;
        let printed = String::from_utf8_lossy(&replayed.stdout);
        assert_eq!(
            printed,
            recorded,
            "replayed with FENNEC_THREADS={threads}: {}",
            String::from_utf8_lossy(&replayed.stderr)
        );
        assert!(replayed.status.success(), "{}", String::from_utf8_lossy(&replayed.stderr));
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// Three tasks taking two `shared` values: `second` takes `a` first, then `main` takes both, then
/// `first`. Each is taken in order of address, `b` before `a` here. A replay gave `first` its turn
/// at `b` once `main` had taken it, not given it back, so `first` waited for `b` on the only worker,
/// unable to make way, while `main` waited for `second`'s turn at `a`, and `second` was behind
/// `first` on that worker: the replay hung, where its recording ran.
const TURNS_AT_LOCKS: &str = "from std.concurrent import Closed, Crashed, Inbox
from std.net import Connection, NetError, Server

struct Account:
    total: int

def first(a: shared Account, b: shared Account, inbox: Inbox[int], stdout: Stdout):
    got = inbox.receive() else -1
    lock a, b as ref x, ref y:
        x.total += got
        y.total += 1
    stdout.print(\"first done\")

def second(a: shared Account, stdout: Stdout):
    lock a as ref x:
        x.total += 3
    stdout.print(\"second done\")

def answered(conn: Connection, stdout: Stdout):
    line = conn.read_line() except why:
        stdout.print(\"a read failed\", f\"{why}\")
        return
    said = line else \"nothing\"
    conn.write(f\"{said} back\\n\") except why:
        stdout.print(\"a write failed\", f\"{why}\")

def answering(server: Server, stdout: Stdout):
    for conn in parallel(server):
        answered(conn, stdout=stdout)

def main(net: Net, stdout: Stdout) fails NetError | Crashed | Closed:
    shared a = Account(total=0)
    shared b = Account(total=0)
    with net.listen(0)? as server:
        port = server.port
        with launch(() => answering(server, stdout=stdout), stop_after=100ms)?:
            with launch((inbox) => first(a=a, b=b, inbox=inbox, stdout=stdout))? as to_first:
                with launch(() => second(a=a, stdout=stdout))?:
                    with net.connect(\"127.0.0.1\", port)? as asking:
                        asking.write(\"hello\\n\")?
                        to_first.send(5)?
                        lock a, b as ref x, ref y:
                            x.total += 2
                            y.total += 2
                        stdout.print(asking.read_line()? else \"nothing\")
    lock a as x:
        stdout.print(\"a\", x.total)
    lock b as y:
        stdout.print(\"b\", y.total)
";

/// A task takes a lock's turn in a replay once the task before it has given the lock back, so it
/// never waits on the lock itself, which holds up its worker, while the task holding it waits for a
/// turn that belongs to a task behind it on that worker.
#[test]
fn a_replay_hands_on_a_locks_turn_when_its_given_back() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("turns-at-locks")?;
    let program = dir.join("accounts.fen");
    std::fs::write(&program, TURNS_AT_LOCKS)?;
    // The order the tasks take the values in is the recording's, and only some orders hung: most
    // of three recordings had one that did.
    for recording in 0..3 {
        let log = dir.join(format!("accounts-{recording}.log"));
        let recorded = fennec_command()
            .args(["run", "--no-test", "--record", &log.to_string_lossy()])
            .arg(&program)
            .env("FENNEC_THREADS", "1")
            .current_dir(&dir)
            .output_within()?;
        let recorded = outcome(&recorded);
        assert_eq!(recorded.2, Some(0), "{}", recorded.1);
        assert!(recorded.0.ends_with("a 10\nb 3\n"), "{}", recorded.0);
        for threads in ["1", "4"] {
            let replayed = fennec_command()
                .env("FENNEC_THREADS", threads)
                .args(["replay", &log.to_string_lossy()])
                .current_dir(&dir)
                .output_limited(DEADLINE, Duration::from_secs(60))?;
            let replayed = outcome(&replayed);
            let how = format!("recording {recording} replayed on {threads} threads: {}", replayed.1);
            assert_eq!(replayed.0, recorded.0, "{how}");
            assert_eq!(replayed.2, Some(0), "{how}");
        }
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// The replay fuzz target's program that hung after #2283: `t1` takes `b`, then `t0` takes `a` and
/// `b` at once, recorded on one worker. A replay took `a`, then waited for its turn at `b`, which
/// was `t1`'s. Holding `a`, `t0` couldn't make way on its worker, and `t1` was behind it there.
const TURNS_AT_TWO_LOCKS: &str = "from std.concurrent import Closed, Crashed, Inbox, Sender, Timeout
from std.net import Connection, NetError, Server
from std.time import Duration

struct Account:
    total: int
    moves: list[int]

# Counts `turns` turns, making way for others at the runtime's ticks.
def counted(turns: int) -> int:
    mut total = 0
    for turn in range(turns):
        total = (total + turn * turn) % 1_000_003
    return total

# The next number, if it comes within `wait`: -1 when it doesn't, -2 when the inbox closed.
def got_within(inbox: Inbox[int], wait: Duration) -> int:
    got = inbox.receive_within(wait) except Timeout(): return -1
    return got else -2

# Sends `number` within `wait`, or gives up, when the task has ended or its inbox stays full.
def handed(to: Sender[int], number: int, wait: Duration):
    to.send_within(number, wait) except Closed() | Timeout(): return

def task_0(a: shared Account, b: shared Account, inbox: Inbox[int], stdout: Stdout):
    got_0 = inbox.receive() else -1
    stdout.print(\"t0 got\", got_0)
    lock a, b as ref x, ref y:
        x.total += 1
        x.moves.append(1)
        y.total += 1
        y.moves.append(1)
        stdout.print(\"t0 locked\", x.total)
    stdout.print(\"t0 s2\")

def task_1(b: shared Account, inbox: Inbox[int], stdout: Stdout):
    lock b as ref y:
        y.total += 3
        y.moves.append(3)
        stdout.print(\"t1 locked\", y.total)
    got_1 = got_within(inbox, 2ms)
    stdout.print(\"t1 got\", got_1)

def round_0(stdout: Stdout):
    stdout.print(\"r0 s0\")

def answered(conn: Connection, stdout: Stdout):
    line = conn.read_line() except why:
        stdout.print(\"a read failed\", f\"{why}\")
        return
    asked = line else \"\"
    match asked:
        case \"r0\":
            round_0(stdout=stdout)
        case _:
            stdout.print(\"asked for no round\", asked)
    conn.write(f\"done {asked}\\n\") except why:
        stdout.print(\"a write failed\", f\"{why}\")

def serving(server: Server, stdout: Stdout):
    for conn in parallel(server):
        answered(conn, stdout=stdout)

def main(net: Net, stdout: Stdout) fails NetError | Crashed | Closed:
    shared a = Account(total=0, moves=[])
    shared b = Account(total=0, moves=[])
    with net.listen(0)? as server:
        port = server.port
        with launch(() => serving(server, stdout=stdout), stop_after=100ms)?:
            with launch((inbox) => task_0(a=a, b=b, inbox=inbox, stdout=stdout))? as to_0:
                with launch((inbox) => task_1(b=b, inbox=inbox, stdout=stdout), inbox_size=1)? as to_1:
                    with net.connect(\"127.0.0.1\", port)? as asking_0:
                        asking_0.write(\"r0\\n\")?
                        to_0.send(0)?
                        stdout.print(\"main s1\")
                        handed(to_1, 100, 1ms)
                        stdout.print(\"main s3\")
                        lock a, b as ref x, ref y:
                            x.total += 2
                            x.moves.append(2)
                            y.total += 2
                            y.moves.append(2)
                            stdout.print(\"main locked\", x.total)
                        stdout.print(asking_0.read_line()? else \"nothing\")
    lock a as x:
        stdout.print(\"total a\", x.total, len(x.moves), x.total == sum(x.moves))
    lock b as y:
        stdout.print(\"total b\", y.total, len(y.moves), y.total == sum(y.moves))
    stdout.print(\"over\")
";

/// A `lock` taking two values waits for its turn at each before it takes either, so it never waits
/// for a turn holding a lock, which no task behind it on its worker could take while it waits.
#[test]
fn a_replay_waits_for_every_turn_a_lock_of_two_values_has_before_taking_either() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("turns-at-two-locks")?;
    let program = dir.join("accounts.fen");
    std::fs::write(&program, TURNS_AT_TWO_LOCKS)?;
    let executable = dir.join("accounts");
    let built = fennec_command()
        .args(["build", "--llvm"])
        .arg(&program)
        .arg("-o")
        .arg(&executable)
        .env("FENNEC_THREADS_RUNTIME", "1")
        .output_within()?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    // Recorded and replayed as the replay fuzz target runs it, which found it: built once, then
    // run with `FENNEC_RECORD` started as `fennec run --record` starts it, and with
    // `FENNEC_REPLAY`. The order the tasks take the values in is the recording's, and only some
    // orders hung.
    for recording in 0..3 {
        let log = dir.join(format!("accounts-{recording}.log"));
        fennec_rt::replay::start_log(&log, format!("native\0{}", program.display()).as_bytes())?;
        let recorded = std::process::Command::new(&executable)
            .env("FENNEC_RECORD", &log)
            .env("FENNEC_THREADS", "1")
            .current_dir(&dir)
            .output_within()?;
        let recorded = outcome(&recorded);
        assert_eq!(recorded.2, Some(0), "{}", recorded.1);
        assert!(recorded.0.ends_with("total a 3 2 True\ntotal b 6 3 True\nover\n"), "{}", recorded.0);
        for threads in ["1", "4"] {
            let replayed = std::process::Command::new(&executable)
                .env("FENNEC_REPLAY", &log)
                .env("FENNEC_THREADS", threads)
                .current_dir(&dir)
                .output_limited(DEADLINE, Duration::from_secs(60))?;
            let replayed = outcome(&replayed);
            let how = format!("recording {recording} replayed on {threads} threads: {}", replayed.1);
            assert_eq!(replayed.0, recorded.0, "{how}");
            assert_eq!(replayed.2, Some(0), "{how}");
        }
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// Tasks moving amounts between two accounts, each `lock` taking both, named in either order, and
/// doing inside it what may be done there: reading a file and printing. Nothing may wait inside a
/// lock, which the checker holds to across calls, so in a replay no task waits for a turn while it
/// holds one, once a lock asks for every turn it takes before taking any.
const ALLOWED_INSIDE_LOCKS: &str = "from std.concurrent import Crashed

struct Account:
    total: int

# Moves `amount` between the accounts, reading a note and printing inside the lock, which may.
def moved(fs: Fs, giving: shared Account, taking: shared Account, amount: int, stdout: Stdout) fails IoError:
    lock giving, taking as ref giver, ref taker:
        note = fs.read_text(\"note.txt\")?
        giver.total -= amount
        taker.total += amount
        stdout.print(note.strip(), amount, giver.total, taker.total)

def mover(fs: Fs, savings: shared Account, checking: shared Account, start: int, stdout: Stdout) fails IoError:
    for step in range(4):
        amount = start + step
        if step % 2 == 0:
            moved(fs, giving=savings, taking=checking, amount=amount, stdout=stdout)?
        else:
            moved(fs, giving=checking, taking=savings, amount=amount, stdout=stdout)?

def main(fs: Fs, stdout: Stdout) fails IoError | Crashed:
    shared savings = Account(total=100)
    shared checking = Account(total=100)
    with launch(() => mover(fs, savings=savings, checking=checking, start=10, stdout=stdout)?)?:
        with launch(() => mover(fs, savings=savings, checking=checking, start=20, stdout=stdout)?)?:
            with launch(() => mover(fs, savings=savings, checking=checking, start=30, stdout=stdout)?)?:
                moved(fs, giving=savings, taking=checking, amount=1, stdout=stdout)?
    lock savings, checking as held_savings, held_checking:
        stdout.print(\"savings\", held_savings.total, \"checking\", held_checking.total)
";

/// What may be done inside a lock, a file read and a print, under locks of two values taken by
/// several tasks, replays as it ran, recorded and replayed on 1 thread and on 4.
#[test]
fn reads_and_prints_inside_locks_of_two_values_replay_as_they_ran() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("inside-locks")?;
    let program = dir.join("moves.fen");
    std::fs::write(&program, ALLOWED_INSIDE_LOCKS)?;
    std::fs::write(dir.join("note.txt"), "kept\n")?;
    let executable = dir.join("moves");
    let built = fennec_command()
        .args(["build", "--llvm"])
        .arg(&program)
        .arg("-o")
        .arg(&executable)
        .env("FENNEC_THREADS_RUNTIME", "1")
        .output_within()?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    for recorded_on in ["1", "4"] {
        let log = dir.join(format!("moves-{recorded_on}.log"));
        fennec_rt::replay::start_log(&log, format!("native\0{}", program.display()).as_bytes())?;
        let recorded = std::process::Command::new(&executable)
            .env("FENNEC_RECORD", &log)
            .env("FENNEC_THREADS", recorded_on)
            .current_dir(&dir)
            .output_within()?;
        let recorded = outcome(&recorded);
        assert_eq!(recorded.2, Some(0), "{}", recorded.1);
        // 13 moves of their amounts, each line read inside its lock.
        assert_eq!(recorded.0.matches("kept ").count(), 13, "{}", recorded.0);
        assert!(recorded.0.ends_with("savings 105 checking 95\n"), "{}", recorded.0);
        // The note changes after the recording: the replay reads it from the recording.
        std::fs::write(dir.join("note.txt"), "changed\n")?;
        for threads in ["1", "4"] {
            let replayed = std::process::Command::new(&executable)
                .env("FENNEC_REPLAY", &log)
                .env("FENNEC_THREADS", threads)
                .current_dir(&dir)
                .output_limited(DEADLINE, Duration::from_secs(60))?;
            let replayed = outcome(&replayed);
            let how = format!("recorded on {recorded_on} threads, replayed on {threads}: {}", replayed.1);
            assert_eq!(replayed.0, recorded.0, "{how}");
            assert_eq!(replayed.2, Some(0), "{how}");
        }
        std::fs::write(dir.join("note.txt"), "kept\n")?;
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// Four tasks each read two shared values at once, 200 times, and `main` writes both 20 times
/// (Fable 5.2's review of #2276). Readers hold both together, so two can log their reads of `a` in
/// one order and of `b` in the other. A replay gave each reader its turn at `a` in the log's order
/// and kept it until it had taken `b`: the first held `a`'s turn waiting for the second's at `b`,
/// and the second waited at `a` behind the first, so every replay was refused (#2313).
const TWO_READERS: &str = "from std.concurrent import Crashed

struct Account:
    total: int

def reads_both(a: shared Account, b: shared Account) -> int:
    mut seen = 0
    for turn in range(200):
        lock a, b as x, y:
            seen += x.total + y.total
    return seen

def reading(a: shared Account, b: shared Account, stdout: Stdout):
    stdout.print(\"a reader saw\", reads_both(a=a, b=b) >= 0)

def writes_both(a: shared Account, b: shared Account):
    for turn in range(20):
        lock a, b as ref x, ref y:
            x.total += 1
            y.total += 2

def main(stdout: Stdout) fails Crashed:
    shared a = Account(total=0)
    shared b = Account(total=0)
    with launch(() => reading(a=a, b=b, stdout=stdout))?:
        with launch(() => reading(a=a, b=b, stdout=stdout))?:
            with launch(() => reading(a=a, b=b, stdout=stdout))?:
                with launch(() => reading(a=a, b=b, stdout=stdout))?:
                    writes_both(a=a, b=b)
    lock a, b as x, y:
        stdout.print(\"a\", x.total, \"b\", y.total)
";

/// A read's turn comes whenever no write is ahead of it, so readers that held a value together
/// replay in whatever order their entries crossed in.
#[test]
fn readers_of_two_values_at_once_replay_however_their_entries_crossed() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("two-readers")?;
    let program = dir.join("readers.fen");
    std::fs::write(&program, TWO_READERS)?;
    let executable = dir.join("readers");
    let built = fennec_command()
        .args(["build", "--llvm"])
        .arg(&program)
        .arg("-o")
        .arg(&executable)
        .env("FENNEC_THREADS_RUNTIME", "1")
        .output_within()?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    // Recorded on 8 threads, where readers cross most; each recording Fable made had hundreds of
    // crossed pairs.
    for recording in 0..3 {
        let log = dir.join(format!("readers-{recording}.log"));
        fennec_rt::replay::start_log(&log, format!("native\0{}", program.display()).as_bytes())?;
        let recorded = std::process::Command::new(&executable)
            .env("FENNEC_RECORD", &log)
            .env("FENNEC_THREADS", "8")
            .current_dir(&dir)
            .output_within()?;
        let recorded = outcome(&recorded);
        assert_eq!(recorded.2, Some(0), "{}", recorded.1);
        assert!(recorded.0.ends_with("a 20 b 40\n"), "{}", recorded.0);
        for threads in ["4", "1"] {
            let replayed = std::process::Command::new(&executable)
                .env("FENNEC_REPLAY", &log)
                .env("FENNEC_THREADS", threads)
                .current_dir(&dir)
                .output_limited(DEADLINE, Duration::from_secs(60))?;
            let replayed = outcome(&replayed);
            let how = format!("recording {recording} replayed on {threads} threads: {}", replayed.1);
            assert_eq!(replayed.0, recorded.0, "{how}");
            assert_eq!(replayed.2, Some(0), "{how}");
        }
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

const FLAT_NOTES: &str = "from std.concurrent import Channel, Closed, Crashed, Inbox, Sender, Timeout, channel
from std.time import Duration

struct Note:
    user: int
    room: str
    text: str

# Counts `turns` turns, making way for others at the runtime's ticks.
def counted(turns: int) -> int:
    mut total = 0
    for turn in range(turns):
        total = (total + turn * turn) % 1_000_003
    return total

# A text long enough to need a block of its own.
def long_text(user: int) -> str:
    return f\"user {user} says something long enough to need a block of its own\"

# Whether a note came within a millisecond, which none does: `main` sends nothing until it hears
# that this one gave up.
def waited(inbox: Inbox[Note]) -> str:
    _ = inbox.receive_within(1ms) except Timeout(): return \"nothing came in time\"
    return \"a note came early\"

# Takes notes once `main` says to, which it does once its offers are done, so they find the inbox
# full but for the first.
def reader(inbox: Inbox[Note], heard: Sender[str], go: Channel[int], stdout: Stdout) fails Closed:
    heard.send(waited(inbox))?
    stdout.print(\"the reader may go:\", (go.receive() else 0) == 1)
    for note in inbox:
        stdout.print(\"got\", note.user, note.room, len(note.text), counted(20_000) >= 0)
    stdout.print(\"the reader saw its inbox closed\")

# Sends a note from `user` if there's room for it within `wait`, saying whether it went.
def offered(to: Sender[Note], user: int, wait: Duration) -> str:
    to.send_within(Note(user=user, room=\"lobby\", text=long_text(user)), wait) except Closed() | Timeout(): return \"full\"
    return \"sent\"

def main(stdout: Stdout) fails Closed | Crashed:
    with channel[str](size=1) as heard:
        back = heard.sender()
        with channel[int](size=1) as go:
            with launch((inbox) => reader(inbox, heard=back, go=go, stdout=stdout)?, inbox_size=1)? as to_reader:
                stdout.print(\"main heard\", heard.receive() else \"nothing\")
                for user in range(6):
                    stdout.print(\"main\", user, offered(to_reader, user=user, wait=0ms if user % 2 == 0 else 1ms))
                go.send(1)?
                for user in range(6, 9):
                    to_reader.send(Note(user=user, room=\"hall\", text=long_text(user)))?
    stdout.print(\"over\")
";

/// Flat messages, a struct of a number and strings, replay as they ran: a timed receive with
/// nothing sent, sends into a full inbox that give up, sends that wait for room, and a receive the
/// inbox's closing ends. Recorded on 1 thread and replayed on 4, and the other way round. These
/// are the messages cheap messages' step 2 sends as their words (`fennec_channel_send_words`),
/// whose recording and replay `channels::words::replayed` checks in the runtime.
#[test]
fn flat_messages_replay_as_they_ran_on_one_thread_and_four() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("flat-notes")?;
    let program = dir.join("notes.fen");
    std::fs::write(&program, FLAT_NOTES)?;
    let executable = dir.join("notes");
    let built = fennec_command()
        .args(["build", "--llvm"])
        .arg(&program)
        .arg("-o")
        .arg(&executable)
        .env("FENNEC_THREADS_RUNTIME", "1")
        .output_within()?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    for (recording, (recorded_on, replayed_on)) in
        [("1", "4"), ("4", "1")].into_iter().cycle().take(6).enumerate()
    {
        let log = dir.join(format!("notes-{recording}.log"));
        fennec_rt::replay::start_log(&log, format!("native\0{}", program.display()).as_bytes())?;
        let recorded = std::process::Command::new(&executable)
            .env("FENNEC_RECORD", &log)
            .env("FENNEC_THREADS", recorded_on)
            .current_dir(&dir)
            .output_within()?;
        let recorded = outcome(&recorded);
        assert_eq!(recorded.2, Some(0), "{}", recorded.1);
        for said in ["main heard nothing came in time", " full\n", "the reader saw its inbox closed\nover\n"]
        {
            assert!(
                recorded.0.contains(said),
                "the recording on {recorded_on} has `{said}`:\n{}",
                recorded.0
            );
        }
        let replayed = std::process::Command::new(&executable)
            .env("FENNEC_REPLAY", &log)
            .env("FENNEC_THREADS", replayed_on)
            .current_dir(&dir)
            .output_limited(DEADLINE, Duration::from_secs(60))?;
        let replayed = outcome(&replayed);
        let how = format!("recorded on {recorded_on} threads, replayed on {replayed_on}: {}", replayed.1);
        assert_eq!(replayed.0, recorded.0, "{how}");
        assert_eq!(replayed.2, Some(0), "{how}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A listener of a channel to `.Every`, two messages behind before its task starts, whose first
/// take crashes its task, which `main` handles.
const FALLEN_BEHIND: &str = "from std.concurrent import Closed, Crashed, Inbox, channel

# Takes every number from an inbox that has fallen behind its channel, so its first take crashes.
def read(hits: Inbox[int], stdout: Stdout):
    for hit in hits:
        stdout.print(\"took\", hit)

def reading(hits: Inbox[int], stdout: Stdout) fails Crashed:
    with launch(() => read(hits, stdout)) as task:
        pass

def main(stdout: Stdout) fails Closed:
    with channel[int](size=2, to=.Every) as hits:
        behind = hits.inbox()
        for hit in range(1, 5):
            hits.send(hit)?
        reading(behind, stdout) except Crashed():
            stdout.print(\"behind\")
    stdout.print(\"over\")
";

/// A listener that fell behind a channel to `.Every` crashes its task at its next take, and a
/// replay crashes it there too. The take wasn't recorded, so every replay stopped at it, asking
/// for a receive where its recording had the channel's close: the nightly's determinism run found
/// one to three random programs like this every night from 2026-11-21.
#[test]
fn a_listener_that_fell_behind_replays_its_crash() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    replays_at_one_four_and_sixteen("fallen-behind", FALLEN_BEHIND, 2, 0)
}

/// `Table`s copied every way a run can copy one replay as they ran (#2424): every item added takes a
/// tag of its own from one count, which a replay has to hand out the same way. Recorded on 4
/// threads and replayed on 1, and the other way.
#[test]
fn copied_tables_replay_as_they_ran_on_four_threads_and_one() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("copied-tables")?;
    let program = dir.join("tables.fen");
    std::fs::write(&program, include_str!("fixtures/tables_copied.fen"))?;
    let executable = dir.join("tables");
    let built = fennec_command()
        .args(["build", "--llvm"])
        .arg(&program)
        .arg("-o")
        .arg(&executable)
        .env("FENNEC_THREADS_RUNTIME", "1")
        .output_within()?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    for (recording, (recorded_on, replayed_on)) in [("4", "1"), ("1", "4")].into_iter().enumerate() {
        let log = dir.join(format!("tables-{recording}.log"));
        fennec_rt::replay::start_log(&log, format!("native\0{}", program.display()).as_bytes())?;
        let recorded = std::process::Command::new(&executable)
            .env("FENNEC_RECORD", &log)
            .env("FENNEC_THREADS", recorded_on)
            .current_dir(&dir)
            .output_within()?;
        let recorded = outcome(&recorded);
        assert_eq!(recorded.2, Some(0), "{}", recorded.1);
        assert_eq!(recorded.0, include_str!("fixtures/tables_copied.out"), "recorded on {recorded_on}");
        let replayed = std::process::Command::new(&executable)
            .env("FENNEC_REPLAY", &log)
            .env("FENNEC_THREADS", replayed_on)
            .current_dir(&dir)
            .output_limited(DEADLINE, Duration::from_secs(60))?;
        let replayed = outcome(&replayed);
        let how = format!("recorded on {recorded_on} threads, replayed on {replayed_on}: {}", replayed.1);
        assert_eq!(replayed.0, recorded.0, "{how}");
        assert_eq!(replayed.2, Some(0), "{how}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A replay that can't follow its recording says where it first couldn't, once, and ends. Here
/// `main` takes a lock its recording didn't. A replay of that waited out its patience, crashed, and
/// then its connection's cleanup asked for a close where the recording read a line: the replay said
/// only that, "while cleaning up after a crash", and hung. A turn has no clock now (#2276): it's the
/// deadlock check that finds no turn waited for can come, once every context is stuck, so a replay
/// that didn't would run into the time limit.
#[test]
fn a_replay_that_cant_follow_its_recording_says_where_it_first_couldnt() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("unfollowed-lock")?;
    let program = dir.join("accounts.fen");
    std::fs::write(&program, TURNS_AT_LOCKS)?;
    let log = dir.join("accounts.log");
    let recorded = fennec_command()
        .args(["run", "--no-test", "--record", &log.to_string_lossy()])
        .arg(&program)
        .env("FENNEC_THREADS", "1")
        .current_dir(&dir)
        .output_within()?;
    assert_eq!(recorded.status.code(), Some(0), "{}", String::from_utf8_lossy(&recorded.stderr));
    // `main` takes `a` once more than it did.
    std::fs::write(
        &program,
        TURNS_AT_LOCKS.replace(
            "                        to_first.send(5)?\n",
            "                        to_first.send(5)?\n                        lock a as ref x:\n                            x.total += 0\n",
        ),
    )?;
    let replayed = fennec_command()
        .env("FENNEC_THREADS", "1")
        .args(["replay", &log.to_string_lossy()])
        .current_dir(&dir)
        .output_limited(DEADLINE, Duration::from_secs(60))?;
    let (_, said, code) = outcome(&replayed);
    assert_eq!(code, Some(1), "{said}");
    // Which waiting turn it names depends on the order the tasks ran in, `main` at its lock or a
    // task at a channel waiting for what `main`, held up, never does, but it's said once: the least
    // turn that can't come.
    assert_eq!(said.matches("the replay couldn't follow its recording: ").count(), 1, "{said}");
    assert!(said.contains("` next, and nothing left running can come to it"), "{said}");
    assert_eq!(said.matches("isn't the run that was recorded").count(), 1, "{said}");
    assert!(!said.contains("while cleaning up after a crash"), "{said}");
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A hub's writes to connections served on other workers are recorded as the hub's, and a replay
/// at 4 threads or 1 gives what they gave, so it counts the same writes, with no network (#2065).
#[test]
fn a_hub_writing_to_connections_on_other_workers_replays_its_writes() -> std::io::Result<()> {
    const MEMBERS: usize = 4;
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("hub")?;
    let program = dir.join("hub.fen");
    std::fs::write(&program, crate::llvm::HUB_WRITES_TO_EVERY_MEMBER)?;
    let log = dir.join("hub.log");
    let mut record = fennec_command()
        .args(["run", "--record", &log.to_string_lossy()])
        .arg(&program)
        .env("FENNEC_THREADS", "4")
        .current_dir(&dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let mut out =
        std::io::BufReader::new(record.stdout.take().ok_or_else(|| std::io::Error::other("no stdout"))?);
    let mut port = String::new();
    std::io::BufRead::read_line(&mut out, &mut port)?;
    let port: u16 = port.trim().parse().map_err(std::io::Error::other)?;
    crate::llvm::every_member_hears_every_line(port, MEMBERS, "4")?;
    let mut rest = String::new();
    std::io::Read::read_to_string(&mut out, &mut rest)?;
    let status = record.wait()?;
    assert!(status.success(), "the recorded hub failed");
    assert_eq!(rest, format!("{} delivered\n", MEMBERS * MEMBERS));
    let recorded = format!("{port}\n{rest}");
    for threads in ["4", "1"] {
        let replayed = fennec_command()
            .env("FENNEC_THREADS", threads)
            .args(["replay", &log.to_string_lossy()])
            .current_dir(&dir)
            .output_within()?;
        let printed = String::from_utf8_lossy(&replayed.stdout);
        assert_eq!(
            printed,
            recorded,
            "replayed with FENNEC_THREADS={threads}: {}",
            String::from_utf8_lossy(&replayed.stderr)
        );
        assert!(replayed.status.success(), "{}", String::from_utf8_lossy(&replayed.stderr));
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// Asks `localhost` on the port it's given, with a short timeout, and prints the answer.
const ASKS_LOCALHOST: &str = "from std.net import NetError

def main(net: Net, args: Args, stdout: Stdout) fails NetError:
    got = net.get(f\"http://localhost:{args[0]}/one\", timeout=1s)?
    stdout.print(got.status, got.text())
";

/// A request of `localhost`, to a server listening only on `127.0.0.1`, is recorded as what it
/// came to, so a replay prints the same with no server there. How long connecting took, trying
/// `::1` first where the system looks it up so, is in no recording: a replay never connects.
#[test]
fn a_request_of_localhost_replays_as_what_it_came_to() -> std::io::Result<()> {
    use std::io::{Read, Write};
    if !ready() {
        return Ok(());
    }
    let dir = folder("localhost")?;
    let program = dir.join("asks.fen");
    std::fs::write(&program, ASKS_LOCALHOST)?;
    let log = dir.join("asks.log");
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    let serving = std::thread::spawn(move || -> std::io::Result<()> {
        let (mut connection, _) = listener.accept()?;
        let mut head = Vec::new();
        let mut byte = [0];
        while !head.ends_with(b"\r\n\r\n") && connection.read(&mut byte)? == 1 {
            head.extend_from_slice(&byte);
        }
        connection.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello")
    });
    let recorded = fennec_command()
        .args(["run", "--no-test", "--record", &log.to_string_lossy()])
        .arg(&program)
        .arg(port.to_string())
        .current_dir(&dir)
        .output_before(DEADLINE)?;
    // A run that never connected leaves the server waiting: one connection lets it go.
    if !serving.is_finished() {
        let _ = std::net::TcpStream::connect(("127.0.0.1", port));
    }
    let _ = serving.join();
    let recorded = without_build_hints(outcome(&recorded));
    assert_eq!(recorded.0, "200 hello\n", "{recorded:?}");
    let replayed = fennec_command()
        .args(["replay", &log.to_string_lossy()])
        .current_dir(&dir)
        .output_before(DEADLINE)?;
    assert_eq!(without_build_hints(outcome(&replayed)), recorded);
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A task serves, `main` makes one request of it and closes the server: no stop comes into it.
const SERVED_IN_A_TASK: &str = "from std.concurrent import Crashed
from std.net import NetError, Request, Response, Server

def answered(request: Request) -> Response:
    return Response(status=200, body=f\"hello {request.path}\".encode())

def serving(server: Server, stdout: Stdout):
    server.serve((request) => answered(request))
    stdout.print(\"the server ended\")

def main(net: Net, stdout: Stdout) fails NetError | Crashed:
    with net.listen(0)? as server:
        port = server.port
        with launch(() => serving(server, stdout=stdout))?:
            got = net.get(f\"http://127.0.0.1:{port}/one\")?
            stdout.print(got.text())
            server.close()
    stdout.print(\"the server is over\")
";

/// A task serves, and its block's end stops it there, 20 ms after one request was answered.
const SERVED_IN_A_TASK_AND_STOPPED: &str = "from std.concurrent import Crashed
from std.net import NetError, Request, Response, Server

def answered(request: Request) -> Response:
    return Response(status=200, body=f\"hello {request.path}\".encode())

def serving(server: Server, stdout: Stdout):
    server.serve((request) => answered(request))
    stdout.print(\"the server never said\")

def main(net: Net, stdout: Stdout) fails NetError | Crashed:
    with net.listen(0)? as server:
        port = server.port
        with launch(() => serving(server, stdout=stdout), stop_after=20ms)?:
            got = net.get(f\"http://127.0.0.1:{port}/one\")?
            stdout.print(got.text())
    stdout.print(\"the server is over\")
";

/// A client connects and sends nothing, so its round is waiting to read when the serving task's
/// block ends and stops it.
const SERVED_IN_A_TASK_STOPPED_WHILE_A_ROUND_READS: &str = "from std.concurrent import Crashed
from std.net import NetError, Request, Response, Server

def answered(request: Request) -> Response:
    return Response(status=200, body=f\"hello {request.path}\".encode())

def serving(server: Server, stdout: Stdout):
    server.serve((request) => answered(request))
    stdout.print(\"the server never said\")

def main(net: Net, stdout: Stdout) fails NetError | Crashed:
    with net.listen(0)? as server:
        port = server.port
        with net.connect(\"127.0.0.1\", port)? as quiet:
            with launch(() => serving(server, stdout=stdout), stop_after=50ms)?:
                got = net.get(f\"http://127.0.0.1:{port}/one\")?
                stdout.print(got.text())
            stdout.print(\"the block ended\", quiet.peer != \"\")
    stdout.print(\"the server is over\")
";

/// One round's handler is asleep when the serving task's block ends and stops it.
const SERVED_IN_A_TASK_STOPPED_WHILE_A_HANDLER_SLEEPS: &str = "from std.concurrent import Crashed
from std.net import NetError, Request, Response, Server

def answered(request: Request, clock: Clock, stdout: Stdout) -> Response:
    if request.path == \"/slow\":
        stdout.print(\"the slow one began\")
        clock.sleep(2h)
        stdout.print(\"the slow one never said\")
    return Response(status=200, body=f\"hello {request.path}\".encode())

def serving(server: Server, clock: Clock, stdout: Stdout):
    server.serve((request) => answered(request, clock=clock, stdout=stdout))
    stdout.print(\"the server never said\")

def main(net: Net, clock: Clock, stdout: Stdout) fails NetError | Crashed:
    with net.listen(0)? as server:
        port = server.port
        with net.connect(\"127.0.0.1\", port)? as slow:
            slow.write(\"GET /slow HTTP/1.1\\r\\nhost: here\\r\\n\\r\\n\")?
            with launch(() => serving(server, clock=clock, stdout=stdout), stop_after=100ms)?:
                got = net.get(f\"http://127.0.0.1:{port}/one\")?
                stdout.print(got.text())
    stdout.print(\"the server is over\")
";

/// A task loops over a server's connections itself, and its block's end stops it there.
const LOOPED_IN_A_TASK_AND_STOPPED: &str = "from std.concurrent import Crashed
from std.net import NetError, Server

def serving(server: Server, stdout: Stdout) fails NetError:
    for conn in parallel(server):
        line = conn.read_line()? else \"\"
        conn.write(f\"{len(line)}\\n\")?
    stdout.print(\"the loop never said\")

def main(net: Net, stdout: Stdout) fails NetError | Crashed:
    with net.listen(0)? as server:
        port = server.port
        with launch(() => serving(server, stdout=stdout)?, stop_after=50ms)?:
            with net.connect(\"127.0.0.1\", port)? as first:
                first.write(\"hello\\n\")?
                stdout.print(first.read_line()? else \"nothing\")
    stdout.print(\"the server is over\")
";

/// A server served from a launched task replays, closed or stopped, with a stop that races a round
/// reading or asleep, and a task looping over its connections itself. A replayed server blocked its
/// thread until its rounds ended, and a task's thread is a worker its rounds run on, so the replay
/// never ended (#1832). Now it parks, as a live one does, and lends its rounds a copy of what
/// they're given. A stop that ended it is recorded, and its replay fails as stopped there too.
/// What a replay prints and how it ends are the recording's; a replayed server says on standard
/// error where its recording ended.
#[test]
fn a_server_served_from_a_task_replays_closed_or_stopped() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("served-in-a-task")?;
    for (name, source) in [
        ("closed", SERVED_IN_A_TASK),
        ("stopped", SERVED_IN_A_TASK_AND_STOPPED),
        ("stopped-while-a-round-reads", SERVED_IN_A_TASK_STOPPED_WHILE_A_ROUND_READS),
        ("stopped-while-a-handler-sleeps", SERVED_IN_A_TASK_STOPPED_WHILE_A_HANDLER_SLEEPS),
        ("looped-and-stopped", LOOPED_IN_A_TASK_AND_STOPPED),
    ] {
        let program = dir.join(format!("{name}.fen"));
        std::fs::write(&program, source)?;
        for recorded_threads in ["1", "4"] {
            let log = dir.join(format!("{name}-{recorded_threads}.log"));
            let mut record = fennec_command();
            record.env("FENNEC_THREADS", recorded_threads);
            record.args(["run", "--record", &log.to_string_lossy()]).arg(&program).current_dir(&dir);
            let recorded = outcome(&record.output_before(DEADLINE)?);
            assert_eq!(recorded.2, Some(0), "{name} recorded at {recorded_threads}: {}", recorded.1);
            for round in 0..8 {
                for threads in ["1", "4", "16"] {
                    let replayed = outcome(
                        &fennec_command()
                            .env("FENNEC_THREADS", threads)
                            .args(["replay", &log.to_string_lossy()])
                            .current_dir(&dir)
                            .output_before(DEADLINE)?,
                    );
                    let at = format!("{name} recorded at {recorded_threads}, replay {round} at {threads}");
                    assert_eq!((&replayed.0, replayed.2), (&recorded.0, recorded.2), "{at}: {}", replayed.1);
                    assert!(
                        replayed.1.contains("the recording ends here, so the server is stopped"),
                        "{at}: {}",
                        replayed.1
                    );
                }
            }
        }
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A server's round in each kind of wait when the task serving it is stopped (see
/// `parallel::stopped_rounds`) replays as it ran. Its recording says the wait was cut short, and a
/// replay ends the round there, writing what it printed first and ending as quietly as the run's
/// did. A round stopped on a channel used to replay as a crash, with its output lost (#1918).
#[test]
fn a_round_stopped_in_any_wait_replays_as_it_ran() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("stopped-rounds")?;
    for (at, (name, source, first)) in crate::parallel::stopped_rounds().into_iter().enumerate() {
        let program = dir.join(format!("stopped_{at}.fen"));
        std::fs::write(&program, source)?;
        let printed = crate::parallel::stopped_round_printed(name, first);
        for recorded_threads in ["1", "4"] {
            let log = dir.join(format!("stopped_{at}-{recorded_threads}.log"));
            let mut record = fennec_command();
            record.env("FENNEC_THREADS", recorded_threads);
            record.args(["run", "--record", &log.to_string_lossy()]).arg(&program).current_dir(&dir);
            let recorded = outcome(&record.output_before(DEADLINE)?);
            assert_eq!(recorded.2, Some(0), "{name} recorded at {recorded_threads}: {}", recorded.1);
            let mut lines: Vec<&str> = recorded.0.lines().collect();
            lines.sort_unstable();
            let mut wanted: Vec<&str> = printed.lines().collect();
            wanted.sort_unstable();
            assert_eq!(lines, wanted, "{name} recorded at {recorded_threads}: {}", recorded.1);
            for round in 0..3 {
                for threads in ["1", "4", "16"] {
                    let replayed = outcome(
                        &fennec_command()
                            .env("FENNEC_THREADS", threads)
                            .args(["replay", &log.to_string_lossy()])
                            .current_dir(&dir)
                            .output_before(DEADLINE)?,
                    );
                    let at = format!("{name} recorded at {recorded_threads}, replay {round} at {threads}");
                    assert_eq!((&replayed.0, replayed.2), (&recorded.0, recorded.2), "{at}: {}", replayed.1);
                    assert!(!replayed.1.contains("error"), "{at}: {}", replayed.1);
                }
            }
        }
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A block stopped at its end replays as it ran: a task waiting there for a task that serves. A
/// replay once marked the task of any block recorded as stopped at its end stopped before it waited:
/// a task serving there then stopped its server with a round still to send. From Fable 5.2's
/// review.
#[test]
fn a_block_stopped_at_its_end_replays_as_it_ran() -> std::io::Result<()> {
    replays_as_it_ran(
        "block-ends",
        "stopped waiting for a task that serves",
        include_str!("fixtures/stopped_rounds/outerchat.fen"),
    )
}

/// A round cancelled at its block's end replays as it ran: the task it leaves behind is let end
/// quietly past its recording. A replay once marked it stopped before its round waited, and it
/// began none of its `parallel` call's pieces. From Fable 5.2's review. A recording that kept its
/// sleep, which the program's end cut short, but not that the sleep was cut short, replayed the
/// task past it, printing what the run never did, about once in 35 runs.
#[test]
fn a_round_cancelled_at_its_end_replays_as_it_ran() -> std::io::Result<()> {
    replays_as_it_ran(
        "cancelled-ends",
        "a cancelled round's task left behind",
        include_str!("fixtures/stopped_rounds/leftparallel.fen"),
    )
}

/// A server's round stopped in one of its pieces of blocking work, a password's check, with no
/// capability's call between them, replays stopped at the same piece: its recording says which, by
/// number. Matched as the next entry, the mark stopped a replay at its first, and the replay failed
/// waiting for the writes to a tally the run had made. From Fable 5.2's review of #1993.
#[test]
fn a_round_stopped_in_one_of_its_pieces_of_work_replays_as_it_ran() -> std::io::Result<()> {
    replays_as_it_ran(
        "work-round",
        "a round checking passwords",
        include_str!("fixtures/stopped_rounds/stopverify.fen"),
    )
}

/// The same for a launched task stopped at its block's end.
#[test]
fn a_task_stopped_in_one_of_its_pieces_of_work_replays_as_it_ran() -> std::io::Result<()> {
    replays_as_it_ran(
        "work-task",
        "a task checking passwords",
        include_str!("fixtures/stopped_rounds/taskverify.fen"),
    )
}

/// Two rounds on two workers, each serving a server of its own when their server is stopped,
/// replay as they ran: each is cancelled once its own server has ended, which the run never got to
/// (#2004).
#[test]
fn two_rounds_serving_servers_of_their_own_replay_as_they_ran() -> std::io::Result<()> {
    replays_as_it_ran(
        "two-own-servers",
        "two rounds serving servers of their own",
        include_str!("fixtures/stopped_rounds/twoserving.fen"),
    )
}

/// A round that prints a line and then makes way at its ticks replays its lines as they ran. Its
/// worker wrote what it printed after it parked, with no task running, and a recording logged that
/// as `main`'s: replays of a 4-thread recording put the round's lines among `main`'s, or tore one in
/// two, every time (#2033). From Fable 5.2's review of #2028.
#[test]
fn a_round_that_prints_then_makes_way_replays_its_lines_as_they_ran() -> std::io::Result<()> {
    replays_as_it_ran(
        "prints-then-makes-way",
        "a round that prints, then makes way",
        include_str!("fixtures/round_prints_then_makes_way.fen"),
    )
}

/// `source`, recorded at 1 and 4 threads, replays as it ran twice at 1, 4 and 16, with nothing said
/// on its standard error.
fn replays_as_it_ran(folder_name: &str, name: &str, source: &str) -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = folder(folder_name)?;
    let program = dir.join("program.fen");
    std::fs::write(&program, source)?;
    for recorded_threads in ["1", "4"] {
        let log = dir.join(format!("recorded-{recorded_threads}.log"));
        let mut record = fennec_command();
        record.env("FENNEC_THREADS", recorded_threads);
        record.args(["run", "--record", &log.to_string_lossy()]).arg(&program).current_dir(&dir);
        let recorded = outcome(&record.output_before(DEADLINE)?);
        assert_eq!(recorded.2, Some(0), "{name} recorded at {recorded_threads}: {}", recorded.1);
        for round in 0..2 {
            for threads in ["1", "4", "16"] {
                let replayed = outcome(
                    &fennec_command()
                        .env("FENNEC_THREADS", threads)
                        .args(["replay", &log.to_string_lossy()])
                        .current_dir(&dir)
                        .output_before(DEADLINE)?,
                );
                let at = format!("{name} recorded at {recorded_threads}, replay {round} at {threads}");
                assert_eq!((&replayed.0, replayed.2), (&recorded.0, recorded.2), "{at}: {}", replayed.1);
                assert!(!replayed.1.contains("error"), "{at}: {}", replayed.1);
            }
        }
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A server's round asleep in a piece of a `parallel` call when its serving task is stopped replays
/// as quietly as it ran. A replay marked the piece stopped, under the piece's own name, while the
/// round's end looks at the round's, so it said `error: this task was stopped` (#1949). From
/// Fable 5.2's review.
#[test]
fn a_round_stopped_in_a_parallel_piece_replays_quietly() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("stopped-in-a-piece")?;
    let program = dir.join("program.fen");
    std::fs::write(&program, include_str!("fixtures/stopped_rounds/stopparallel.fen"))?;
    for recorded_threads in ["1", "4"] {
        let log = dir.join(format!("recorded-{recorded_threads}.log"));
        let mut record = fennec_command();
        record.env("FENNEC_THREADS", recorded_threads);
        record.args(["run", "--record", &log.to_string_lossy()]).arg(&program).current_dir(&dir);
        let recorded = outcome(&record.output_before(DEADLINE)?);
        assert_eq!(recorded.2, Some(0), "recorded at {recorded_threads}: {}", recorded.1);
        assert!(!recorded.1.contains("error"), "recorded at {recorded_threads}: {}", recorded.1);
        for round in 0..3 {
            for threads in ["1", "4", "16"] {
                let replayed = outcome(
                    &fennec_command()
                        .env("FENNEC_THREADS", threads)
                        .args(["replay", &log.to_string_lossy()])
                        .current_dir(&dir)
                        .output_before(DEADLINE)?,
                );
                let at = format!("recorded at {recorded_threads}, replay {round} at {threads}");
                assert_eq!((&replayed.0, replayed.2), (&recorded.0, recorded.2), "{at}: {}", replayed.1);
                assert!(!replayed.1.contains("error"), "{at}: {}", replayed.1);
            }
        }
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A server that stops when the program's asked to, with 2 seconds' grace, and says so after.
#[cfg(unix)]
const STOPPING_SERVER: &str = "from std.net import Request, Response

def respond(request: Request) -> Response:
    return Response(status=200, body=request.path.encode())

def main(net: Net, signals: Signals, stdout: Stdout):
    with net.listen(0)? as server:
        stdout.print(server.port)
        server.serve(respond, timeout=60s, until=signals, grace=2s)
    stdout.print(\"stopped\", signals.stopping())
";

/// A server recorded as it stops when asked replays exactly, with no network and no signal: the
/// connection waiting for its next request ends where it ended, the one partway through a request
/// is answered and closed, the one that never finished is cut short as it was at `grace`, and
/// what the program does after the server is the same.
#[cfg(unix)]
#[test]
fn a_server_recorded_stopping_when_asked_replays_its_drain() -> std::io::Result<()> {
    use std::io::{Read, Write};
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("server_stopping")?;
    let program = dir.join("server.fen");
    std::fs::write(&program, STOPPING_SERVER)?;
    let executable = dir.join("server");
    let built = fennec_command()
        .args(["build", "--llvm"])
        .arg(&program)
        .arg("-o")
        .arg(&executable)
        .env("FENNEC_THREADS_RUNTIME", "1")
        .output_within()?;
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    // Started as `fennec run --record` starts it. The program is run here rather than through
    // `fennec run`, so the signal reaches it.
    let log = dir.join("server.log");
    let header = format!("native\0{}", program.display());
    fennec_rt::replay::start_log(&log, header.as_bytes())?;
    let mut record = std::process::Command::new(&executable)
        .env("FENNEC_RECORD", &log)
        .env("FENNEC_THREADS", "4")
        .env("FENNEC_LIVE_REPORT", dir.join("live"))
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    let mut out =
        std::io::BufReader::new(record.stdout.take().ok_or_else(|| std::io::Error::other("no stdout"))?);
    let mut port = String::new();
    std::io::BufRead::read_line(&mut out, &mut port)?;
    let port: u16 = port.trim().parse().map_err(std::io::Error::other)?;
    let connect = || -> std::io::Result<std::net::TcpStream> {
        let stream = std::net::TcpStream::connect(("127.0.0.1", port))?;
        stream.set_read_timeout(Some(crate::executables::WALL_DEADLINE))?;
        Ok(stream)
    };
    let mut idle = connect()?;
    idle.write_all(b"GET /a HTTP/1.1\r\n\r\n")?;
    let mut first = [0; 40];
    idle.read_exact(&mut first)?;
    let mut partway = connect()?;
    partway.write_all(b"GET /b HTTP/1.1\r\n")?;
    let mut stuck = connect()?;
    stuck.write_all(b"GET /c HTTP/1.1\r\n")?;
    // The server has taken all three before it's asked to stop, as its live count says.
    crate::llvm::serving(&dir.join("live"), 3);
    let sent = std::process::Command::new("kill").args(["-TERM", &record.id().to_string()]).status()?;
    assert!(sent.success(), "couldn't send SIGTERM");
    let mut rest = Vec::new();
    idle.read_to_end(&mut rest)?;
    partway.write_all(b"\r\n")?;
    let mut answered = String::new();
    partway.read_to_string(&mut answered)?;
    assert!(answered.ends_with("connection: close\r\n\r\n/b"), "{answered}");
    let _ = stuck.read_to_end(&mut rest);
    let mut after = String::new();
    out.read_to_string(&mut after)?;
    assert!(record.wait()?.success(), "the recorded server failed");
    let recorded = format!("{port}\n{after}");
    assert_eq!(after, "stopped True\n");
    for threads in ["4", "1"] {
        let replayed = fennec_command()
            .args(["replay", &log.to_string_lossy()])
            .env("FENNEC_THREADS", threads)
            .env("FENNEC_THREADS_RUNTIME", "1")
            .current_dir(&dir)
            .output_within()?;
        assert_eq!(
            String::from_utf8_lossy(&replayed.stdout),
            recorded,
            "replayed with FENNEC_THREADS={threads}: {}",
            String::from_utf8_lossy(&replayed.stderr)
        );
        assert!(replayed.status.success(), "{}", String::from_utf8_lossy(&replayed.stderr));
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// Two tasks taking turns at one `shared` value, as they come.
const TWO_TASKS_ONE_LOCK: &str = "from std.concurrent import Crashed

struct Order:
    seen = list[str]()

def add(order: shared Order, text: str):
    lock order as ref held:
        held.seen.append(text)

def takes(order: shared Order, name: str):
    for at in range(600):
        add(order, f\"{name}{at} \")

def main(stdout: Stdout) fails Crashed:
    shared order = Order()
    with launch(() => takes(order, \"a\"))?:
        with launch(() => takes(order, \"b\"))?:
            pass
    lock order as held:
        stdout.print(len(held.seen), \"\".join(held.seen))
";

/// Ranges of a `parallel` call taking turns at one `shared` value.
const RANGES_ONE_LOCK: &str = "struct Order:
    seen = list[int]()

def visit(order: shared Order, at: int) -> int:
    lock order as ref held:
        held.seen.append(at)
    return at

def main(stdout: Stdout):
    shared order = Order()
    done = [visit(order, at) for at in parallel(range(32))]
    lock order as held:
        stdout.print(len(done), held.seen)
";

/// Tasks contending for one `shared` value take its lock in the order the recording did, however
/// many threads the replay is given, so what they left in it reads the same.
#[test]
fn tasks_contending_for_a_lock_replay_the_order_they_took_it() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("lock-order")?;
    for (name, source) in [("two_tasks", TWO_TASKS_ONE_LOCK), ("ranges", RANGES_ONE_LOCK)] {
        let program = dir.join(format!("{name}.fen"));
        std::fs::write(&program, source)?;
        let log = dir.join(format!("{name}.log"));
        let mut record = fennec_command();
        record.env("FENNEC_THREADS", "4").env("FENNEC_PARALLEL_STRESS", "1");
        record.args(["run", "--record", &log.to_string_lossy()]).arg(&program).current_dir(&dir);
        let recorded = outcome(&record.output_before(DEADLINE)?);
        assert_eq!(recorded.2, Some(0), "{name}: {}", recorded.1);
        for threads in ["1", "4", "1", "4", "7"] {
            let replayed = outcome(
                &fennec_command()
                    .env("FENNEC_THREADS", threads)
                    .env("FENNEC_PARALLEL_STRESS", "1")
                    .args(["replay", &log.to_string_lossy()])
                    .current_dir(&dir)
                    .output_before(DEADLINE)?,
            );
            assert_eq!(replayed, recorded, "{name}, replayed with FENNEC_THREADS={threads}");
        }
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// The main thread and a task trading messages through two channels with waits of 5 µs, so how
/// many sends and receives time out, and what's received, are the machine's to say.
const WAITS_TIMED_OUT_OR_NOT: &str = "from std.concurrent import Channel, Closed, Crashed, Timeout, channel
from std.time import Duration

def echo(to: Channel[int], back: Channel[int]):
    for n in to:
        if n < 0:
            return
        back.send(n * 2) except Closed():
            return

def trade(stdout: Stdout, to: Channel[int], back: Channel[int]):
    mut waited = 0
    mut total = 0
    for n in range(3000):
        to.send_within(n, Duration(nanos=5_000)) except Timeout() | Closed():
            waited += 1
        got: int? = back.receive_within(Duration(nanos=5_000)) except Timeout():
            waited += 1
            None
        if got is not None:
            total += got
    stdout.print(waited, total)

def main(stdout: Stdout) fails Closed | Crashed | Timeout:
    with channel[int](size=1) as to:
        with channel[int](size=4096) as back:
            with launch(() => echo(to, back=back))?:
                trade(stdout, to=to, back=back)
                to.send(-1)?
";

/// Every send and receive that waited, whether what it waited for came or it timed out, replays to
/// what it came to in the recording, however many threads the replay is given.
#[test]
fn a_wait_that_timed_out_or_not_replays_as_it_came_out() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("waits")?;
    let program = dir.join("waits.fen");
    std::fs::write(&program, WAITS_TIMED_OUT_OR_NOT)?;
    for recorded_threads in ["1", "4"] {
        let log = dir.join(format!("waits-{recorded_threads}.log"));
        let mut record = fennec_command();
        record.env("FENNEC_THREADS", recorded_threads);
        record.args(["run", "--record", &log.to_string_lossy()]).arg(&program).current_dir(&dir);
        let recorded = outcome(&record.output_before(DEADLINE)?);
        assert_eq!(recorded.2, Some(0), "recorded with FENNEC_THREADS={recorded_threads}: {}", recorded.1);
        for threads in ["1", "4", "7"] {
            let replayed = outcome(
                &fennec_command()
                    .env("FENNEC_THREADS", threads)
                    .args(["replay", &log.to_string_lossy()])
                    .current_dir(&dir)
                    .output_before(DEADLINE)?,
            );
            assert_eq!(
                replayed, recorded,
                "recorded with FENNEC_THREADS={recorded_threads}, replayed with {threads}"
            );
        }
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A task sleeps two hours, and its block's end stops it after 20 ms.
const STOPPED_IN_A_SLEEP: &str = "from std.concurrent import Crashed

def waits(clock: Clock, stdout: Stdout):
    clock.sleep(2h)
    stdout.print(\"never said\")

def main(clock: Clock, stdout: Stdout) fails Crashed:
    with launch(() => waits(clock, stdout=stdout), stop_after=20ms)?:
        pass
    stdout.print(\"the stopped one is over\")
";

/// A task stopped in a sleep is stopped there in its replay too. A replay doesn't sleep, so the
/// task's sleep used to end at once, before its block's 20 ms were up, and it printed what the
/// recorded run never did, with nothing said (#1805).
#[test]
fn a_task_stopped_in_a_sleep_is_stopped_there_in_its_replay() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    replays_fifty_times_each("stopped-in-a-sleep", STOPPED_IN_A_SLEEP, |printed| {
        printed == "the stopped one is over\n"
    })
}

/// A task waits on a channel nothing is sent on until its block's end has stopped it, after 20 ms.
const STOPPED_ON_A_CHANNEL: &str = "from std.concurrent import Channel, Closed, Crashed, channel

def waits(numbers: Channel[int], stdout: Stdout):
    got = numbers.receive() else -1
    stdout.print(\"never said\", got)

def main(stdout: Stdout) fails Closed | Crashed:
    with channel[int](size=1) as numbers:
        with launch(() => waits(numbers, stdout=stdout), stop_after=20ms)?:
            pass
        numbers.send(7)?
        stdout.print(\"the stopped one is over\", numbers.receive() else -1)
";

/// A task stopped in a channel's wait is stopped there in its replay too. Its receive used to be
/// in no recording, so its replay waited for a turn that never came, until it gave up after ten
/// seconds, by when the block's end had stopped it again.
#[test]
fn a_task_stopped_waiting_on_a_channel_is_stopped_there_in_its_replay() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    replays_fifty_times_each("stopped-on-a-channel", STOPPED_ON_A_CHANNEL, |printed| {
        printed == "the stopped one is over 7\n"
    })
}

/// A task launches one of its own, which sleeps two hours, and waits for it at its block's end,
/// where its own block's end stops it after 20 ms.
const STOPPED_AT_ITS_BLOCK_END: &str = "from std.concurrent import Crashed

def inner(clock: Clock, stdout: Stdout):
    clock.sleep(2h)
    stdout.print(\"inner never said\")

def outer(clock: Clock, stdout: Stdout) fails Crashed:
    with launch(() => inner(clock, stdout=stdout))?:
        pass
    stdout.print(\"outer never said\")

def main(clock: Clock, stdout: Stdout) fails Crashed:
    with launch(() => outer(clock, stdout=stdout)?, stop_after=20ms)?:
        pass
    stdout.print(\"both are over\")
";

/// A task stopped while it waits at its own block's end, for a task it launched, is stopped there
/// in its replay too. Nothing recorded that, so its replay went on past the block and printed what
/// the run never did, unless the replay's clock happened to stop it in time (#1831).
#[test]
fn a_task_stopped_at_its_own_block_s_end_is_stopped_there_in_its_replay() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    replays_fifty_times_each("stopped-at-its-block-end", STOPPED_AT_ITS_BLOCK_END, |printed| {
        printed == "both are over\n"
    })
}

/// The same, stopped at once by the block that launched it failing, rather than by the clock.
const STOPPED_AT_ITS_BLOCK_END_BY_A_FAILURE: &str = "from std.concurrent import Crashed

error Gave:
    at: int

def inner(clock: Clock, stdout: Stdout):
    clock.sleep(2h)
    stdout.print(\"inner never said\")

def outer(clock: Clock, stdout: Stdout) fails Crashed:
    with launch(() => inner(clock, stdout=stdout))?:
        pass
    stdout.print(\"outer never said\")

def gives_up(clock: Clock, stdout: Stdout) fails Crashed | Gave:
    with launch(() => outer(clock, stdout=stdout)?)?:
        clock.sleep(5ms)
        fail Gave(at=1)

def main(clock: Clock, stdout: Stdout):
    gives_up(clock, stdout=stdout) except why:
        stdout.print(f\"the block gave up: {why}\")
        return
    stdout.print(\"not this\")
";

/// A task its launcher's failed block stopped at its own block's end is stopped there in its
/// replay too, which stops nothing at once itself.
#[test]
fn a_task_a_failed_block_stopped_at_its_own_block_s_end_is_stopped_there_in_its_replay() -> std::io::Result<()>
{
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    replays_fifty_times_each(
        "stopped-at-its-block-end-by-a-failure",
        STOPPED_AT_ITS_BLOCK_END_BY_A_FAILURE,
        |printed| printed == "the block gave up: Gave(at=1)\n",
    )
}

/// A task reads a line from a connection of its own that nothing is ever written to, and its
/// block's end stops it after 20 ms.
const STOPPED_IN_A_READ: &str = "from std.concurrent import Crashed
from std.net import Connection, NetError

def waits(conn: Connection, stdout: Stdout) fails NetError:
    line = conn.read_line()?
    stdout.print(\"never said\", line)

def main(net: Net, stdout: Stdout) fails Crashed | NetError:
    with net.listen(0)? as server:
        with net.connect(\"127.0.0.1\", server.port)? as first:
            with server.accept()? as taken:
                with launch(() => waits(first, stdout=stdout)?, stop_after=20ms)?:
                    pass
                stdout.print(\"the stopped one is over\", taken)
";

/// A task stopped in a socket's read is stopped there in its replay too. The read's result was
/// recorded, a timeout, but not that a stop had cut it short, so the replay failed with the timeout.
#[test]
fn a_task_stopped_in_a_socket_read_is_stopped_there_in_its_replay() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    replays_fifty_times_each("stopped-in-a-read", STOPPED_IN_A_READ, |printed| {
        printed.starts_with("the stopped one is over ")
    })
}

/// A block that fails stops its task at once, there in its two-hour sleep.
const STOPPED_BY_A_FAILED_BLOCK: &str = "from std.concurrent import Crashed

error Gave:
    at: int

def waits(clock: Clock, stdout: Stdout):
    clock.sleep(2h)
    stdout.print(\"never said\")

def gives_up(clock: Clock, stdout: Stdout) fails Crashed | Gave:
    with launch(() => waits(clock, stdout=stdout))?:
        clock.sleep(5ms)
        fail Gave(at=1)

def main(clock: Clock, stdout: Stdout):
    gives_up(clock, stdout=stdout) except why:
        stdout.print(f\"the block gave up: {why}\")
        return
    stdout.print(\"not this\")
";

/// A task stopped at once by its block's failure is stopped at the same wait in its replay.
#[test]
fn a_task_its_failed_block_stopped_is_stopped_there_in_its_replay() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    replays_fifty_times_each("stopped-by-a-failed-block", STOPPED_BY_A_FAILED_BLOCK, |printed| {
        printed == "the block gave up: Gave(at=1)\n"
    })
}

/// A program connecting to a server of its own and printing what it made.
const NET_LABELS: &str = "from std.net import NetError

def main(stdout: Stdout, net: Net) fails NetError:
    with net.listen(0)? as server:
        with net.connect(\"127.0.0.1\", server.port)? as first:
            with server.accept()? as taken:
                stdout.print(server, first, taken)
    with net.udp()? as socket:
        stdout.print(socket)
    return
";

/// Connections, servers and UDP sockets print as their labels, counted as the program makes them,
/// not their handles, which are random: the same every run, and in a replay.
#[test]
fn network_values_print_the_same_every_run_and_in_a_replay() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("net-labels")?;
    let program = dir.join("labels.fen");
    std::fs::write(&program, NET_LABELS)?;
    let wanted = "Server(#1) Connection(#1) Connection(#2)\nUdpSocket(#1)\n";
    for _ in 0..2 {
        let ran =
            outcome(&fennec_command().arg("run").arg(&program).current_dir(&dir).output_before(DEADLINE)?);
        assert_eq!((ran.0.as_str(), ran.2), (wanted, Some(0)), "{}", ran.1);
    }
    let log = dir.join("labels.log");
    let recorded = outcome(
        &fennec_command()
            .args(["run", "--record", &log.to_string_lossy()])
            .arg(&program)
            .current_dir(&dir)
            .output_before(DEADLINE)?,
    );
    assert_eq!(recorded.0, wanted, "{}", recorded.1);
    let replayed = outcome(
        &fennec_command()
            .args(["replay", &log.to_string_lossy()])
            .current_dir(&dir)
            .output_before(DEADLINE)?,
    );
    assert_eq!(replayed, recorded);
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A server whose tasks each say which connection they serve, back to its client and on its own
/// output.
const LABELLED_SERVER: &str = "from std.net import NetError

def main(net: Net, stdout: Stdout) fails NetError:
    with net.listen(0)? as server:
        stdout.print(server.port)
        for conn in parallel(server):
            line = conn.read_line()? else \"\"
            if line == \"stop\":
                break
            stdout.print(line, conn)
            conn.write(f\"{conn}\\n\")?
    stdout.print(\"done\")
    return
";

/// How many connections `serve_labelled` keeps open at once.
const LABELLED: usize = 6;

/// Runs `LABELLED_SERVER`, recording to `log` if it's given, with several connections open at
/// once that are asked in the opposite order to the one they came in. Gives back what each
/// connection was told it is, in the order they came, and everything the server printed.
fn serve_labelled(dir: &Path, program: &Path, log: Option<&Path>) -> std::io::Result<(Vec<String>, String)> {
    let mut command = fennec_command();
    command.arg("run");
    if let Some(log) = log {
        command.args(["--record", &log.to_string_lossy()]);
    }
    let mut server = command
        .arg(program)
        .env("FENNEC_THREADS", "4")
        .env("FENNEC_PARALLEL_STRESS", "1")
        .current_dir(dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let mut out =
        std::io::BufReader::new(server.stdout.take().ok_or_else(|| std::io::Error::other("no stdout"))?);
    let mut port = String::new();
    std::io::BufRead::read_line(&mut out, &mut port)?;
    let port: u16 = port.trim().parse().map_err(std::io::Error::other)?;
    let mut open = Vec::new();
    for _ in 0..LABELLED {
        let stream = std::net::TcpStream::connect(("127.0.0.1", port))?;
        stream.set_read_timeout(Some(Duration::from_secs(20)))?;
        open.push(stream);
    }
    let mut told = vec![String::new(); LABELLED];
    for (at, stream) in open.iter().enumerate().rev() {
        std::io::Write::write_all(&mut &*stream, format!("asked{at}\n").as_bytes())?;
        let mut back = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(stream), &mut back)?;
        if let Some(slot) = told.get_mut(at) {
            back.trim().clone_into(slot);
        }
    }
    let mut stopping = std::net::TcpStream::connect(("127.0.0.1", port))?;
    std::io::Write::write_all(&mut stopping, b"stop\n")?;
    let mut rest = String::new();
    std::io::Read::read_to_string(&mut out, &mut rest)?;
    let status = server.wait()?;
    assert!(status.success(), "the server failed");
    Ok((told, format!("{port}\n{rest}")))
}

/// The connections a server serves in parallel are numbered in the order it took them, not the
/// order their tasks start: the same every run, and in a replay.
#[test]
fn served_connections_are_numbered_in_the_order_they_came_every_run_and_in_a_replay() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("served-labels")?;
    let program = dir.join("served.fen");
    std::fs::write(&program, LABELLED_SERVER)?;
    let wanted: Vec<String> = (1..=LABELLED).map(|n| format!("Connection(#{n})")).collect();
    for _ in 0..2 {
        let (told, _) = serve_labelled(&dir, &program, None)?;
        assert_eq!(told, wanted);
    }
    let log = dir.join("served.log");
    let (told, recorded) = serve_labelled(&dir, &program, Some(&log))?;
    assert_eq!(told, wanted);
    for threads in ["4", "1"] {
        let replayed = fennec_command()
            .env("FENNEC_THREADS", threads)
            .args(["replay", &log.to_string_lossy()])
            .current_dir(&dir)
            .output_within()?;
        assert_eq!(
            String::from_utf8_lossy(&replayed.stdout),
            recorded,
            "replayed with FENNEC_THREADS={threads}: {}",
            String::from_utf8_lossy(&replayed.stderr)
        );
        assert!(replayed.status.success(), "{}", String::from_utf8_lossy(&replayed.stderr));
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// The first thread prints, waits on a channel for a task that's slow to send, and prints what
/// came. It writes what it's printed before it sleeps on the channel, so its output goes out in
/// two bursts.
const PRINTS_BETWEEN_WAITS: &str = "from std.concurrent import Closed, Crashed, Sender, channel
from std.time import Duration

def late(clock: Clock, to: Sender[int]) fails Closed:
    clock.sleep(Duration.millis(20))
    to.send(7)?

def main(stdout: Stdout, clock: Clock) fails Closed | Crashed:
    with channel[int](size=1) as numbers:
        with launch(() => late(clock, to=numbers.sender())?)?:
            stdout.print(\"before\")
            stdout.print(numbers.receive() else -1)
";

/// Records `source` once at each of 1 and 4 threads, and replays each recording 25 times at 1
/// thread and 25 at 4: every replay is the recorded run, output and all. `whole` says whether
/// what a recording printed is all there.
fn replays_fifty_times_each(name: &str, source: &str, whole: impl Fn(&str) -> bool) -> std::io::Result<()> {
    let dir = folder(name)?;
    let program = dir.join("program.fen");
    std::fs::write(&program, source)?;
    for recorded_threads in ["1", "4"] {
        let log = dir.join(format!("recorded-{recorded_threads}.log"));
        let mut record = fennec_command();
        record.env("FENNEC_THREADS", recorded_threads);
        record.args(["run", "--record", &log.to_string_lossy()]).arg(&program).current_dir(&dir);
        let recorded = outcome(&record.output_before(DEADLINE)?);
        assert_eq!(recorded.2, Some(0), "recorded with FENNEC_THREADS={recorded_threads}: {}", recorded.1);
        assert!(whole(&recorded.0), "recorded with FENNEC_THREADS={recorded_threads}: {}", recorded.0);
        for round in 0..25 {
            for threads in ["1", "4"] {
                let replayed = outcome(
                    &fennec_command()
                        .env("FENNEC_THREADS", threads)
                        .args(["replay", &log.to_string_lossy()])
                        .current_dir(&dir)
                        .output_before(DEADLINE)?,
                );
                assert_eq!(
                    replayed, recorded,
                    "recorded with FENNEC_THREADS={recorded_threads}, replay {round} with {threads}"
                );
            }
        }
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A thread that printed and then slept on a channel wrote what it had printed first, as its own
/// burst. In a replay it waits there for its turn instead, without sleeping, and writes it all at
/// the end as one. The output is the same, so the replay is: it goes by each task's bytes, where
/// it went by how many times the task wrote, and said the recording had a write it never made.
/// Fifty replays of each recording, as how a replay's output is cut changes from one to the next.
#[test]
fn a_thread_that_printed_before_it_slept_on_a_channel_replays_as_it_ran() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    replays_fifty_times_each("prints-between-waits", PRINTS_BETWEEN_WAITS, |printed| printed == "before\n7\n")
}

/// Two tasks take turns: each prints a line, sends the other its go, and waits for the other's.
/// A task writes what it's printed whenever it has to wait, so their bursts interleave, differently
/// from run to run, and each task's lines are its own.
const TASKS_TAKING_TURNS: &str = "from std.concurrent import Channel, Closed, Crashed, channel

def turns(stdout: Stdout, name: str, mine: Channel[int], theirs: Channel[int], count: int) fails Closed:
    mut seen = 0
    for at in range(count):
        stdout.print(name, at)
        theirs.send(at)?
        seen += mine.receive() else 0
    stdout.print(name, \"saw\", seen)

def main(stdout: Stdout) fails Closed | Crashed:
    with channel[int](size=1) as to_first:
        with channel[int](size=1) as to_second:
            with launch(() => turns(stdout, \"first\", mine=to_first, theirs=to_second, count=200)?)?:
                with launch(() => turns(stdout, \"second\", mine=to_second, theirs=to_first, count=200)?)?:
                    pass
    stdout.print(\"done\")
";

/// Two tasks whose output interleaves replay byte for byte, fifty times each recording: a replay
/// cuts each task's bytes at the lengths that task's bursts were recorded with, so one task's
/// bytes never come out in another's burst, however the replay's tasks happened to cut theirs.
#[test]
fn two_tasks_whose_output_interleaves_replay_byte_for_byte() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    replays_fifty_times_each("tasks-taking-turns", TASKS_TAKING_TURNS, |printed| {
        let lines: Vec<&str> = printed.lines().collect();
        let of = |name: &str| lines.iter().filter(|line| line.starts_with(name)).count();
        // Each task's own lines are all there and in its own order, whatever came between them.
        let in_order = |name: &str| {
            let mine = lines.iter().filter_map(|line| line.strip_prefix(name)?.trim().parse::<usize>().ok());
            mine.eq(0..200)
        };
        lines.len() == 403
            && of("first") == 201
            && of("second") == 201
            && in_order("first")
            && in_order("second")
    })
}

/// Ranges of a `parallel` call and launched tasks all printing, enough that each task's output
/// goes out in several bursts, which interleave differently from run to run.
const TASKS_PRINTING: &str = "from std.concurrent import Crashed

def chatter(stdout: Stdout, name: str):
    for at in range(2500):
        stdout.print(name, at)

def main(stdout: Stdout) fails Crashed:
    numbers = [n for n in range(64)]
    for n in parallel(numbers):
        stdout.print(\"range\", n)
    with launch(() => chatter(stdout, \"a\"))?:
        with launch(() => chatter(stdout, \"b\"))?:
            chatter(stdout, \"main\")
    stdout.print(\"done\")
";

/// How tasks' output interleaved is recorded, and a replay writes it in the same order, byte for
/// byte, however many threads it's given.
#[test]
fn tasks_printing_replay_their_output_in_the_order_it_went_out() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("output-order")?;
    let program = dir.join("printing.fen");
    std::fs::write(&program, TASKS_PRINTING)?;
    let log = dir.join("printing.log");
    let mut record = fennec_command();
    record.env("FENNEC_THREADS", "4").env("FENNEC_PARALLEL_STRESS", "1");
    record.args(["run", "--record", &log.to_string_lossy()]).arg(&program).current_dir(&dir);
    let recorded = outcome(&record.output_before(DEADLINE)?);
    assert_eq!(recorded.2, Some(0), "{}", recorded.1);
    assert_eq!(recorded.0.lines().count(), 64 + 3 * 2500 + 1);
    for threads in ["1", "4", "1", "4", "7"] {
        let replayed = outcome(
            &fennec_command()
                .env("FENNEC_THREADS", threads)
                .env("FENNEC_PARALLEL_STRESS", "1")
                .args(["replay", &log.to_string_lossy()])
                .current_dir(&dir)
                .output_before(DEADLINE)?,
        );
        assert_eq!(replayed, recorded, "replayed with FENNEC_THREADS={threads}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// An ordered stream whose body prints, read until a `break`, while its chunks run ahead.
const STREAM_PAST_A_BREAK: &str = "def shown(stdout: Stdout, n: int) -> int:
    stdout.print(\"chunk\", n)
    return n * n

def main(stdout: Stdout):
    many = [n for n in range(20000)]
    for square in (shown(stdout, n) for n in parallel(many)):
        if square > 900:
            break
    stdout.print(\"after break\")
";

/// A stream's chunks that ran ahead of a `break` in the recording run in its replay too, and no
/// others do, so what they printed comes out the same, however many threads the replay is given.
#[test]
fn a_streams_chunks_run_past_a_break_replay_as_they_ran() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("stream-break")?;
    let program = dir.join("stream.fen");
    std::fs::write(&program, STREAM_PAST_A_BREAK)?;
    let log = dir.join("stream.log");
    let mut record = fennec_command();
    // The reader waits for the workers to run each chunk in its window, so chunks run past the
    // break however loaded the machine: left to itself, a reader that gets to every chunk first
    // runs them all, and none runs past it.
    record.env("FENNEC_THREADS", "4").env("FENNEC_PARALLEL_STRESS", "1").env("FENNEC_STREAM_RUN_AHEAD", "1");
    record.args(["run", "--record", &log.to_string_lossy()]).arg(&program).current_dir(&dir);
    let recorded = outcome(&record.output_before(DEADLINE)?);
    assert_eq!(recorded.2, Some(0), "{}", recorded.1);
    let read = recorded.0.lines().filter(|line| line.starts_with("chunk ")).count();
    assert!(read > 32, "no chunk ran past the break, so this tests nothing: {}", recorded.0);
    for threads in ["1", "7", "1", "7"] {
        let replayed = outcome(
            &fennec_command()
                .env("FENNEC_THREADS", threads)
                .env("FENNEC_PARALLEL_STRESS", "1")
                .args(["replay", &log.to_string_lossy()])
                .current_dir(&dir)
                .output_before(DEADLINE)?,
        );
        assert_eq!(replayed, recorded, "replayed with FENNEC_THREADS={threads}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// An ordered stream whose body prints, with an item that fails partway.
const STREAM_FAILING: &str = "def shown(stdout: Stdout, n: int) -> int:
    stdout.print(\"chunk\", n)
    return 1000 // (n - 3000)

def main(stdout: Stdout):
    many = [n for n in range(20000)]
    stdout.print(\"never\", sum(part for part in (shown(stdout, n) for n in parallel(many))))
";

/// A stream whose item fails stops the program where it did when recorded: its replay prints
/// what the chunks that ran printed and fails the same way, however many threads it's given.
#[test]
fn a_stream_that_fails_replays_as_it_failed() -> std::io::Result<()> {
    if !ready() || !threads_runtime_built() {
        return Ok(());
    }
    let dir = folder("stream-fails")?;
    let program = dir.join("stream.fen");
    std::fs::write(&program, STREAM_FAILING)?;
    let log = dir.join("stream.log");
    let mut record = fennec_command();
    record.env("FENNEC_THREADS", "4").env("FENNEC_PARALLEL_STRESS", "1");
    record.args(["run", "--record", &log.to_string_lossy()]).arg(&program).current_dir(&dir);
    let recorded = outcome(&record.output_before(DEADLINE)?);
    assert_eq!(recorded.2, Some(1), "{}", recorded.1);
    assert!(recorded.1.contains("division by zero") && !recorded.0.contains("never"), "{recorded:?}");
    for threads in ["1", "7", "1", "7"] {
        let replayed = outcome(
            &fennec_command()
                .env("FENNEC_THREADS", threads)
                .env("FENNEC_PARALLEL_STRESS", "1")
                .args(["replay", &log.to_string_lossy()])
                .current_dir(&dir)
                .output_before(DEADLINE)?,
        );
        assert_eq!(replayed, recorded, "replayed with FENNEC_THREADS={threads}");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// Whether an example serves until it's stopped, so its run doesn't end by itself.
fn serves(source: &str) -> bool {
    source.contains(".serve(")
}

/// A program that makes a database, writes to it and reads it back.
const DATABASE: &str = "from std import sqlite
from std.sql import SqlError

struct Item:
    name: str
    count: int

def main(stdout: Stdout, fs: Fs) fails SqlError:
    with sqlite.open(fs, \"items.db\")? as db:
        _ = db.execute(t\"create table items (name text, count int)\")?
        for n in range(3):
            name = f\"item{n}\"
            _ = db.execute(t\"insert into items values ({name}, {n})\")?
        for item in db.query[Item](t\"select name, count from items order by count\")?:
            stdout.print(item.name, item.count)
    return
";

/// A program writing to standard output and standard error in turn.
const BOTH_OUTPUTS: &str = "def main(stdout: Stdout, stderr: Stderr) fails IoError:
    for n in range(3):
        stdout.print(\"out\", n)
        stderr.write_line(f\"err {n}\")?
    return
";

/// What a run read from and wrote to a database replays without the database: each call's result
/// is in the recording, under the task that made it, so the replay opens nothing, and gives the
/// same output with the file gone.
#[test]
fn a_run_that_reads_and_writes_a_database_replays_identically() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("database-replay")?;
    let program = dir.join("database.fen");
    std::fs::write(&program, DATABASE)?;
    let log = dir.join("database.log");
    let recorded = outcome(
        &fennec_command()
            .args(["run", "--record", &log.to_string_lossy()])
            .arg(&program)
            .current_dir(&dir)
            .output_before(DEADLINE)?,
    );
    assert_eq!((recorded.0.as_str(), recorded.2), ("item0 0\nitem1 1\nitem2 2\n", Some(0)), "{}", recorded.1);
    std::fs::remove_file(dir.join("items.db"))?;
    let replayed = outcome(
        &fennec_command()
            .args(["replay", &log.to_string_lossy()])
            .current_dir(&dir)
            .output_before(DEADLINE)?,
    );
    assert_eq!(replayed, recorded);
    assert!(!dir.join("items.db").exists(), "the replay opened the database");
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// A replay writes standard error as the run did, beside standard output.
#[test]
fn a_replay_writes_standard_error_too() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let dir = folder("stderr-replay")?;
    let program = dir.join("both.fen");
    std::fs::write(&program, BOTH_OUTPUTS)?;
    let log = dir.join("both.log");
    let recorded = outcome(
        &fennec_command()
            .args(["run", "--record", &log.to_string_lossy()])
            .arg(&program)
            .current_dir(&dir)
            .output_before(DEADLINE)?,
    );
    assert_eq!(recorded.0, "out 0\nout 1\nout 2\n", "{}", recorded.1);
    assert!(recorded.1.ends_with("err 0\nerr 1\nerr 2\n"), "{}", recorded.1);
    let replayed = outcome(
        &fennec_command()
            .args(["replay", &log.to_string_lossy()])
            .current_dir(&dir)
            .output_before(DEADLINE)?,
    );
    assert_eq!(replayed, recorded);
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// Whether an example runs more than one task, which needs the threads runtime.
fn several_tasks(source: &str) -> bool {
    ["parallel(", "launch(", "Channel["].iter().any(|marker| source.contains(marker))
}

/// Every example that ends by itself replays exactly as it ran, its output byte for byte, tasks
/// and `parallel` calls among them: how their output interleaved is recorded, and a replay repeats
/// it.
#[test]
fn every_example_that_ends_replays_exactly_as_it_ran() -> std::io::Result<()> {
    if !ready() {
        return Ok(());
    }
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let mut programs = Vec::new();
    for entry in std::fs::read_dir(&examples)? {
        let path = entry?.path();
        let source = if path.extension().is_some_and(|ext| ext == "fen") {
            std::fs::read_to_string(&path)?
        } else {
            continue;
        };
        if !serves(&source) && (threads_runtime_built() || !several_tasks(&source)) {
            programs.push(path);
        }
    }
    programs.sort();
    assert!(programs.len() > 100, "only {} examples found", programs.len());
    let dir = folder("examples")?;
    let lanes = std::thread::available_parallelism().map_or(4, std::num::NonZero::get).min(8);
    let chunks: Vec<Vec<PathBuf>> =
        (0..lanes).map(|lane| programs.iter().skip(lane).step_by(lanes).cloned().collect()).collect();
    let differences: Vec<String> = std::thread::scope(|scope| {
        let running: Vec<_> = chunks
            .iter()
            .map(|chunk| {
                let dir = &dir;
                scope.spawn(move || {
                    let mut found = Vec::new();
                    for program in chunk {
                        let name = program
                            .file_stem()
                            .map(|stem| stem.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        let log = dir.join(format!("{name}.log"));
                        match recorded_and_replayed(program, b"a line\nanother\n", &log) {
                            Ok((recorded, replayed)) if recorded == replayed => {}
                            Ok((recorded, replayed)) => {
                                found.push(format!("{name}: recorded {recorded:?}\n  replayed {replayed:?}"));
                            }
                            Err(err) => found.push(format!("{name}: {err}")),
                        }
                    }
                    found
                })
            })
            .collect();
        running.into_iter().flat_map(|lane| lane.join().unwrap_or_default()).collect()
    });
    assert!(
        differences.is_empty(),
        "{} of {} examples replayed differently:\n{}",
        differences.len(),
        programs.len(),
        differences.join("\n")
    );
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}
