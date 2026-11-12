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
    let mut said = String::from_utf8_lossy(&ran.stderr).into_owned();
    // A program a signal stopped has no exit code, so which signal goes with what it said: a run
    // killed with nothing printed is otherwise only `None`, which says nothing of why.
    #[cfg(unix)]
    if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(&ran.status) {
        let _ = std::fmt::Write::write_fmt(&mut said, format_args!("[stopped by signal {signal}]"));
    }
    (String::from_utf8_lossy(&ran.stdout).into_owned(), said, ran.status.code())
}

/// Records a run of `program` with `input` on its standard input, then replays it, giving back both
/// outcomes.
fn recorded_and_replayed(program: &Path, input: &[u8], log: &Path) -> std::io::Result<(Outcome, Outcome)> {
    let mut record = fennec_command();
    record.args(["run", "--record", &log.to_string_lossy()]);
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
                held.add(line.len())
            conn.write(f\"{line.len()}\\n\")?
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

/// A server that stops when the program's asked to, with 2 seconds' grace, and says so after.
#[cfg(unix)]
const STOPPING_SERVER: &str = "from std.net import Request, Response

def respond(request: Request) -> Response:
    Response(status=200, body=request.path.encode())

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
        stdout.print(held.seen.len(), \"\".join(held.seen))
";

/// Ranges of a `parallel` call taking turns at one `shared` value.
const RANGES_ONE_LOCK: &str = "struct Order:
    seen = list[int]()

def visit(order: shared Order, at: int) -> int:
    lock order as ref held:
        held.seen.append(at)
    at

def main(stdout: Stdout):
    shared order = Order()
    done = [visit(order, at) for at in parallel(range(32))]
    lock order as held:
        stdout.print(done.len(), held.seen)
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
