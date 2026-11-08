//! `fennec run --record` and `fennec replay`: a recorded run of a single-task program, replayed,
//! prints exactly what it printed, from the inputs it logged, in both engines. A replay that asks
//! for something its recording doesn't have next stops, saying both.

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use crate::executables::{Within, fennec_command, runtime_built};

/// How long one recording or replay of an example may take.
const DEADLINE: Duration = Duration::from_secs(120);

fn folder(name: &str) -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("fennec-replay-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// What a run printed, to standard output and error, and its exit code.
type Outcome = (String, String, Option<i32>);

fn outcome(ran: &Output) -> Outcome {
    (
        String::from_utf8_lossy(&ran.stdout).into_owned(),
        String::from_utf8_lossy(&ran.stderr).into_owned(),
        ran.status.code(),
    )
}

/// Records a run of `program` with `engine` (`--interp`, or nothing for native code) and `input`
/// on its standard input, then replays it, giving back both outcomes.
fn recorded_and_replayed(
    program: &Path,
    engine: Option<&str>,
    input: &[u8],
    log: &Path,
) -> std::io::Result<(Outcome, Outcome)> {
    let mut record = fennec_command();
    record.args(["run", "--record", &log.to_string_lossy()]);
    record.args(engine);
    record.arg(program).arg("given").current_dir(program.parent().unwrap_or(Path::new(".")));
    let recorded = record.output_given(input, DEADLINE)?;
    // With nothing on standard input, so what it read can only come from the recording.
    let replayed = fennec_command()
        .args(["replay", &log.to_string_lossy()])
        .current_dir(program.parent().unwrap_or(Path::new(".")))
        .output_before(DEADLINE)?;
    Ok((outcome(&recorded), outcome(&replayed)))
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
fn a_run_using_the_clock_and_random_numbers_replays_identically_in_both_engines() -> std::io::Result<()> {
    let dir = folder("dice")?;
    let program = dir.join("dice.fen");
    std::fs::write(&program, DICE)?;
    let engines: &[Option<&str>] =
        if runtime_built() { &[Some("--interp"), None] } else { &[Some("--interp")] };
    for engine in engines {
        let log = dir.join("dice.log");
        let (recorded, replayed) = recorded_and_replayed(&program, *engine, b"", &log)?;
        assert_eq!(recorded.2, Some(0), "{engine:?}: {}", recorded.1);
        assert_eq!(replayed, recorded, "{engine:?}");
        // Another run draws other numbers at another time: the replay's are the recording's.
        let again = outcome(
            &fennec_command().args(["run"]).args(*engine).arg(&program).arg("given").output_within()?,
        );
        assert_ne!(again.0, recorded.0, "{engine:?}: a fresh run printed the same");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn a_replay_that_asks_for_other_inputs_than_its_recording_stops_naming_both() -> std::io::Result<()> {
    let dir = folder("diverges")?;
    let program = dir.join("dice.fen");
    std::fs::write(&program, DICE)?;
    let log = dir.join("dice.log");
    let (recorded, _) = recorded_and_replayed(&program, Some("--interp"), b"", &log)?;
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
    if !runtime_built() {
        return Ok(());
    }
    let dir = folder("nap")?;
    let program = dir.join("nap.fen");
    std::fs::write(&program, NAP)?;
    let log = dir.join("nap.log");
    let (recorded, replayed) = recorded_and_replayed(&program, None, b"", &log)?;
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
    let dir = folder("drawn")?;
    let program = dir.join("drawn.fen");
    std::fs::write(&program, DRAWN)?;
    let engines: &[Option<&str>] =
        if runtime_built() { &[Some("--interp"), None] } else { &[Some("--interp")] };
    for engine in engines {
        let log = dir.join("drawn.log");
        let mut record = fennec_command();
        // Every item a range of its own, on the pool, so the ranges take turns on the threads.
        record.env("FENNEC_THREADS", "4").env("FENNEC_PARALLEL_STRESS", "1");
        record.args(["run", "--record", &log.to_string_lossy()]);
        record.args(*engine).arg(&program).current_dir(&dir);
        let recorded = outcome(&record.output_within()?);
        assert_eq!(recorded.2, Some(0), "{engine:?}: {}", recorded.1);
        for threads in ["4", "1", "7", "4", "2"] {
            let replayed = outcome(
                &fennec_command()
                    .env("FENNEC_THREADS", threads)
                    .env("FENNEC_PARALLEL_STRESS", "1")
                    .args(["replay", &log.to_string_lossy()])
                    .current_dir(&dir)
                    .output_within()?,
            );
            assert_eq!(replayed, recorded, "{engine:?}, replayed with FENNEC_THREADS={threads}");
        }
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

const SERVER: &str = "from std.net import NetError

shared Total:
    bytes = 0

    def add(ref self, count: int):
        self.bytes += count

def main(net: Net, stdout: Stdout) fails NetError:
    total = Total()
    with net.listen(0)? as server:
        stdout.print(server.port)
        for conn in parallel(server):
            line = conn.read_line()? else \"\"
            if line == \"stop\":
                break
            total.add(line.len())
            conn.write(f\"{line.len()}\\n\")?
    stdout.print(total.bytes)
";

/// A native server's rounds are each a task named by its connection, and a replay runs them again
/// with no socket, each reading what its connection sent, until the recording runs out.
#[test]
fn a_recorded_server_replays_its_rounds_with_no_network() -> std::io::Result<()> {
    if !runtime_built() {
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

/// Whether an example runs more than one task, whose order between them isn't recorded yet, or
/// serves until it's stopped, so its run doesn't end by itself.
fn several_tasks_or_serves(source: &str) -> bool {
    ["parallel(", "launch(", "Channel[", ".serve("].iter().any(|marker| source.contains(marker))
}

#[test]
fn every_single_task_example_replays_exactly_as_it_ran() -> std::io::Result<()> {
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let mut programs = Vec::new();
    for entry in std::fs::read_dir(&examples)? {
        let path = entry?.path();
        let source = if path.extension().is_some_and(|ext| ext == "fen") {
            std::fs::read_to_string(&path)?
        } else {
            continue;
        };
        if !several_tasks_or_serves(&source) {
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
                        match recorded_and_replayed(program, Some("--interp"), b"a line\nanother\n", &log) {
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
