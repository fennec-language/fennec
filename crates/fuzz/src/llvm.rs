//! Targets built natively. The `llvm` target: a generated program built by `fennec build --llvm`
//! must print the same and fail the same way built as a dev build and with `--release`, which
//! optimizes the whole program as one, and must print what's pinned for its input, where there's an
//! output pinned. Structs and enums are printed through what they give, and a program the backend
//! doesn't take yet is passed over.
//!
//! The `json`, `toml` and `utf8` targets run their input through a small program built once,
//! which reads it with the standard library's code, where failing is fine and only breaking isn't.
//!
//! These need the `fennec` command beside this one (or where `FENNEC` says), the runtime library
//! beside that, and LLVM's tools. `fennec-fuzz run llvm` stops if one is missing, and the others,
//! like replaying a kept input, pass without them, unless building is asked for (`can_build`).

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crate::generate::printing_plainly;
use crate::supervise::{BUILD_STUCK, MOST_KILOBYTES, STUCK, output_watched, watch};

/// `fennec`: where `FENNEC` says, or beside this program. A test binary, which Cargo puts in a
/// `deps` folder below the one it builds `fennec` in, finds it in that one. Looking only beside
/// itself, every unit test here that builds a program found none, on CI and in the gate, and
/// passed having built nothing (fennec#2557).
pub(crate) fn fennec() -> PathBuf {
    if let Some(fennec) = std::env::var_os("FENNEC") {
        return PathBuf::from(fennec);
    }
    let here = std::env::current_exe().unwrap_or_default();
    match here.parent() {
        Some(folder) if folder.file_name().is_some_and(|name| name == "deps") => {
            folder.with_file_name("fennec")
        }
        _ => here.with_file_name("fennec"),
    }
}

/// This process's own folder to build in, held as its own while it runs (see `owned`). Where it
/// can't be held, it's still used, and is only removed once it's a day old.
pub(crate) fn work() -> PathBuf {
    let folder = work_of(std::process::id());
    let _ = crate::owned::own(&folder);
    folder
}

/// The folder process `pid` builds in.
fn work_of(pid: u32) -> PathBuf {
    std::env::temp_dir().join(format!("{WORK}{pid}"))
}

/// What every process's folder to build in is named from, before its process id.
const WORK: &str = "fennec-fuzz-llvm-";

/// Removes the folder process `pid` built in, once it's ended: this process's own, or a worker's
/// it started, which is only removed once nothing holds it, so a new process given the same id
/// keeps its own. A failing program is kept as its input, which builds it again, so nothing in the
/// folder is needed after.
pub(crate) fn forget(pid: u32) {
    if pid == std::process::id() {
        crate::owned::let_go(&work_of(pid));
    } else {
        crate::owned::remove_ended(&work_of(pid));
    }
}

/// Removes the folders fuzzing processes made that ended without removing them: a run, a worker
/// or a try at cutting a failure down that was stopped, or a test's. A folder whose process is
/// still running is held by it and kept, however new it is.
fn forget_ended() {
    for entry in std::fs::read_dir(std::env::temp_dir()).into_iter().flatten().flatten() {
        if entry.file_name().to_str().and_then(fuzzing_process).is_some() {
            crate::owned::remove_ended(&entry.path());
        }
    }
}

/// The process a folder in the temporary folder belongs to, when fuzzing made it: a run's
/// `fennec-fuzz-`, a builder's `fennec-fuzz-llvm-` or a cutter's `fennec-fuzz-small-`, then
/// its process id.
fn fuzzing_process(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("fennec-fuzz-")?;
    let pid = rest.strip_prefix("llvm-").or_else(|| rest.strip_prefix("small-")).unwrap_or(rest);
    pid.parse().ok()
}

/// Whether programs are to be built natively here, which a target or test that can't build one
/// otherwise passes over: false where nothing can be. Where building is asked for, by
/// `FENNEC_EXPECT_LLVM`, as CI's test jobs and the gate set it, or by the heap audit's
/// `FENNEC_AUDIT=1`, not being able to is a failure that says why, never a pass with nothing built
/// (fennec#2557).
pub(crate) fn can_build() -> Result<bool, String> {
    match (ready(), asked_to_build()) {
        (Ok(()), _) => Ok(true),
        (Err(_), None) => Ok(false),
        (Err(why), Some(asked)) => Err(format!("{asked}, but nothing can be built natively here: {why}")),
    }
}

/// `can_build`, for a program that launches tasks, which needs the runtime with threads beside
/// `fennec` as well.
pub(crate) fn can_build_threads() -> Result<bool, String> {
    if !can_build()? {
        return Ok(false);
    }
    if threads_runtime_here() {
        return Ok(true);
    }
    match asked_to_build() {
        None => Ok(false),
        Some(asked) => Err(format!(
            "{asked}, but there's no runtime with threads, `libfennec_runtime_threads.a`, beside `{}`: build \
             it with `cargo build -p fennec-runtime --features threads --target-dir target/threads` and copy it \
             there",
            fennec().display()
        )),
    }
}

/// What asks for programs to be built natively here, where something does.
fn asked_to_build() -> Option<&'static str> {
    if audited() {
        Some("the heap audit is asked for (`FENNEC_AUDIT=1`)")
    } else if std::env::var_os("FENNEC_EXPECT_LLVM").is_some_and(|expected| !expected.is_empty()) {
        Some("`FENNEC_EXPECT_LLVM` is set")
    } else {
        None
    }
}

/// Whether a program can be built and run here, or why not.
pub(crate) fn ready() -> Result<(), String> {
    static READY: OnceLock<Result<(), String>> = OnceLock::new();
    READY
        .get_or_init(|| {
            forget_ended();
            let source = "def main(stdout: Stdout):\n    stdout.print(\"ready\")\n";
            match build(source, &work(), false)? {
                Some(executable) if ran(&executable, &[], &work())?.0 == b"ready\n" => Ok(()),
                _ => Err(format!("`{}` couldn't build and run a program with LLVM", fennec().display())),
            }
        })
        .clone()
}

/// What `fennec build --llvm` says when building went wrong, rather than the backend saying what
/// it doesn't take yet. A runtime library that's missing, or from another build than the `fennec`
/// building, is one, as a `fennec-std-tests` left from an earlier build finds it.
pub(crate) const BROKEN: [&str; 6] =
    ["linking failed", "failed on", "couldn't run", "once trimmed", "panicked", "the runtime library `"];

/// What `fennec build` gave, run with the tools it starts in a group of their own: one whose
/// tools together take more than `BUILD_STUCK` CPU time or `MOST_KILOBYTES` of memory is
/// stopped, every tool with it, and that's a failure of its own, since building a program
/// should never take that.
pub(crate) fn built_within(command: &mut Command) -> Result<std::process::Output, String> {
    let (built, failure) = output_watched(command, BUILD_STUCK, MOST_KILOBYTES)?;
    match failure {
        Some(failure) => Err(format!(
            "building it {failure}, `fennec build` and the tools it started together, and all were stopped:\n{}",
            String::from_utf8_lossy(&built.stderr)
        )),
        None => Ok(built),
    }
}

/// Builds `source` in `work`, giving back the executable, or None when the backend doesn't take
/// the program yet.
pub(crate) fn build(source: &str, work: &Path, release: bool) -> Result<Option<PathBuf>, String> {
    build_with(&fennec(), source, work, release)
}

/// [`build`], with the `fennec` at `fennec`, whose runtime is the one beside it.
pub(crate) fn build_with(
    fennec: &Path,
    source: &str,
    work: &Path,
    release: bool,
) -> Result<Option<PathBuf>, String> {
    std::fs::create_dir_all(work).map_err(|err| format!("{}: {err}", work.display()))?;
    let (program, executable) = (work.join("program.fen"), work.join("program"));
    std::fs::write(&program, source).map_err(|err| format!("{}: {err}", program.display()))?;
    let mut command = Command::new(fennec);
    command
        .args(["build", "--llvm"])
        .args(release.then_some("--release"))
        .arg(&program)
        .arg("-o")
        .arg(&executable)
        // Its own home and no cache, so fuzzing leaves nothing behind in the real ones, and no
        // daemon, which a home of its own would start and keep for half an hour.
        .env("FENNEC_HOME", work)
        .env("FENNEC_NO_CACHE", "1")
        .env("FENNEC_NO_DAEMON", "1");
    let built = built_within(&mut command)?;
    if built.status.success() {
        return Ok(Some(executable));
    }
    let said = String::from_utf8_lossy(&built.stderr);
    if built.status.code() == Some(1) && !BROKEN.iter().any(|broken| said.contains(broken)) {
        return Ok(None);
    }
    Err(format!("it didn't build with LLVM ({}):\n{said}", built.status))
}

/// A program built with `fennec build --llvm`, and `--release` when asked, then run: what it
/// printed, the line it said on standard error that says why it stopped, and whether it
/// succeeded. A build that fails gives what `fennec` said, whatever the reason, for programs that
/// should always build.
pub(crate) fn built_and_run(
    source: &str,
    name: &str,
    release: bool,
) -> Result<(Vec<u8>, String, bool), String> {
    built_and_run_laid_out(source, name, release, false)
}

/// [`built_and_run`], with every type held in a block of its own (`FENNEC_BOXED_TYPES=1`) where
/// `boxed` says, as it was before types were laid out in place, rather than as the compiler lays
/// them out now.
pub(crate) fn built_and_run_laid_out(
    source: &str,
    name: &str,
    release: bool,
    boxed: bool,
) -> Result<(Vec<u8>, String, bool), String> {
    let work = work().join(name);
    std::fs::create_dir_all(&work).map_err(|err| format!("{}: {err}", work.display()))?;
    let (program, executable) = (work.join("program.fen"), work.join("program"));
    std::fs::write(&program, source).map_err(|err| format!("{}: {err}", program.display()))?;
    let mut command = Command::new(fennec());
    command
        .args(["build", "--llvm"])
        .args(release.then_some("--release"))
        .arg(&program)
        .arg("-o")
        .arg(&executable)
        .env("FENNEC_HOME", &work)
        .env("FENNEC_NO_CACHE", "1")
        .env("FENNEC_NO_DAEMON", "1");
    if boxed {
        command.env("FENNEC_BOXED_TYPES", "1");
    } else {
        command.env_remove("FENNEC_BOXED_TYPES");
    }
    let built = built_within(&mut command)?;
    if !built.status.success() {
        let how = match (release, boxed) {
            (true, _) => "with --release",
            (false, true) => "as a dev build with FENNEC_BOXED_TYPES=1",
            (false, false) => "as a dev build",
        };
        return Err(format!("it didn't build {how}:\n{}", String::from_utf8_lossy(&built.stderr)));
    }
    let (printed, said, succeeded) = ran(&executable, &[], &work)?;
    Ok((printed, cut(&said), succeeded))
}

/// The line of what a program said that says why it stopped: the first that says what a
/// watching build caught, which a sanitizer's report doesn't start with, or else the last
/// `error:`, since a task that crashed and was handled says its own first, or else the first.
/// A heap audit's lines are all kept, as each names a block.
fn cut(said: &str) -> String {
    let audit: Vec<&str> = said.lines().filter(|line| line.starts_with(AUDIT)).collect();
    if !audit.is_empty() {
        return audit.join("\n");
    }
    let caught = said.lines().find(|line| CAUGHT.iter().any(|what| line.contains(what)));
    let stopped = || said.lines().rev().find(|line| line.starts_with("error: "));
    caught.or_else(stopped).or_else(|| said.lines().next()).unwrap_or_default().to_owned()
}

/// What a build that watches itself says when it catches something: a false proof, memory the
/// program doesn't own, a native block handed to the runtime.
pub(crate) const CAUGHT: [&str; 3] = ["proof broken", "AddressSanitizer", "handed a native block"];

/// What every line a heap audit says starts with. A program built for one says nothing unless its
/// audit found a block left with no holder, or one counted otherwise than its holders.
pub(crate) const AUDIT: &str = "heap audit: ";

/// What a program exits with when its heap audit found something.
const AUDIT_FOUND: i32 = 87;

/// Whether the programs are built for a heap audit, as `FENNEC_AUDIT=1` asks of `fennec build`,
/// which reads it the same way (`fennec_llvm::audited`). A program built for one keeps no native
/// blocks, and audits its heap as it ends when it's run with the same set.
pub(crate) fn audited() -> bool {
    std::env::var("FENNEC_AUDIT").as_deref() == Ok("1")
}

/// Runs what `built_and_run` last made under `name` again, on `threads` threads, with what it
/// said cut as `built_and_run` cuts it.
pub(crate) fn ran_again_on(name: &str, threads: usize) -> Result<(Vec<u8>, String, bool), String> {
    let work = work().join(name);
    let (printed, said, succeeded) =
        ran_on(&work.join("program"), &[], &work, MOST_KILOBYTES, Some(threads))?;
    Ok((printed, cut(&said), succeeded))
}

/// The executable `built_and_run` last made under `name`.
pub(crate) fn built_executable(name: &str) -> PathBuf {
    work().join(name).join("program")
}

/// What an executable printed, what it said on standard error, and whether it succeeded. One
/// that crashes, gets stuck or holds too much memory is stopped and said so.
fn ran(executable: &Path, args: &[&std::ffi::OsStr], work: &Path) -> Result<(Vec<u8>, String, bool), String> {
    ran_holding(executable, args, work, MOST_KILOBYTES)
}

/// The most memory a reader may hold. Inputs are small, so one that holds more was asked by its
/// bytes for far more than they are.
const READER_KILOBYTES: u64 = 512 * 1024;

/// `ran`, stopping the executable if it holds more than `most_kilobytes`.
fn ran_holding(
    executable: &Path,
    args: &[&std::ffi::OsStr],
    work: &Path,
    most_kilobytes: u64,
) -> Result<(Vec<u8>, String, bool), String> {
    ran_on(executable, args, work, most_kilobytes, None)
}

/// `ran_holding`, on `threads` threads where it says, through `FENNEC_THREADS`.
fn ran_on(
    executable: &Path,
    args: &[&std::ffi::OsStr],
    work: &Path,
    most_kilobytes: u64,
    threads: Option<usize>,
) -> Result<(Vec<u8>, String, bool), String> {
    let (out, err) = (work.join("stdout"), work.join("stderr"));
    let file = |path: &Path| File::create(path).map_err(|err| format!("{}: {err}", path.display()));
    // Each run starts in an empty folder of its own, so a program that writes files finds none
    // left by an earlier run, and none land beside the fuzzer.
    let folder = work.join("folder");
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir(&folder).map_err(|err| format!("{}: {err}", folder.display()))?;
    let mut command = Command::new(executable);
    command.current_dir(&folder);
    if let Some(threads) = threads {
        command.env("FENNEC_THREADS", threads.to_string());
    }
    // A program counts what it holds at its end, and one that ran to its end holding anything
    // let a block go unfreed.
    command.env("FENNEC_COUNTS", "1");
    let mut child = command
        .args(args)
        .stdout(file(&out)?)
        .stderr(file(&err)?)
        .spawn()
        .map_err(|err| format!("couldn't run what LLVM built: {err}"))?;
    let failure = watch(&mut child, STUCK, None, most_kilobytes, || false);
    let printed = std::fs::read(&out).unwrap_or_default();
    let said = String::from_utf8_lossy(&std::fs::read(&err).unwrap_or_default()).into_owned();
    let (said, live) = counted(&said);
    match (failure, child.wait()) {
        (None, Ok(status)) if status.success() && live > 0 => {
            Err(format!("it ran to its end with {live} blocks still live, which nothing freed:\n{said}"))
        }
        (None, Ok(status)) => Ok((printed, said, status.success())),
        (_, Ok(status)) if status.code() == Some(1) => Ok((printed, said, false)),
        // A heap audit that found something ends the program with a code of its own, having said
        // what it found: a failure for whoever built it that way to read, and no crash.
        (_, Ok(status)) if status.code() == Some(AUDIT_FOUND) && said.contains(AUDIT) => {
            Ok((printed, said, false))
        }
        (failure, status) => Err(format!("what LLVM built {failure:?} ({status:?}):\n{said}")),
    }
}

/// What a program said with its counts line taken out, and how many blocks that line says were
/// still live at its end, 0 if it wrote none. The line is the last that starts `copies ` and ends
/// `live <n>`, which `FENNEC_COUNTS` has the program write, and it's taken out so what's compared
/// between builds is only what the program itself said.
fn counted(said: &str) -> (String, u64) {
    let lines: Vec<&str> = said.lines().collect();
    let counts = lines.iter().rposition(|line| line.starts_with("copies ") && line.contains(" live "));
    let Some(at) = counts else { return (said.to_owned(), 0) };
    let live = lines
        .get(at)
        .and_then(|line| line.rsplit(' ').next())
        .and_then(|count| count.parse().ok())
        .unwrap_or(0);
    let kept: Vec<&str> =
        lines.iter().enumerate().filter(|(number, _)| *number != at).map(|(_, line)| *line).collect();
    let mut kept = kept.join("\n");
    if !kept.is_empty() && said.ends_with('\n') {
        kept.push('\n');
    }
    (kept, live)
}

/// What a run did: what it printed, the first line it said on standard error, and whether it
/// succeeded.
type Outcome = (Vec<u8>, String, bool);

/// What a build of `source` did when run, or None when the backend doesn't take it yet.
fn outcome(source: &str, work: &Path, release: bool) -> Result<Option<Outcome>, String> {
    let Some(executable) = build(source, work, release)? else { return Ok(None) };
    let (printed, said, succeeded) = ran(&executable, &[], work)?;
    Ok(Some((printed, said.lines().next().unwrap_or_default().to_owned(), succeeded)))
}

/// The folders of inputs the `llvm` target has outputs pinned for, beside each input: what it
/// printed in `<input>.out`, and when it failed, the first line it said in `<input>.fails`. They
/// were pinned before the engines were removed.
const PINNED: [&str; 2] = ["seeds/llvm", "found/llvm"];

/// Whether a file is a pinned output rather than an input.
pub(crate) fn is_pinned_output(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension == "out" || extension == "fails")
}

/// The outcome pinned for an input, if it's one of those with an output pinned.
fn pinned(input: &[u8]) -> Option<Outcome> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let entries = PINNED.iter().flat_map(|folder| std::fs::read_dir(root.join(folder)).into_iter().flatten());
    let path = entries
        .flatten()
        .map(|entry| entry.path())
        .find(|path| !is_pinned_output(path) && std::fs::read(path).is_ok_and(|bytes| bytes == input))?;
    let printed = std::fs::read(path.with_extension("out")).ok()?;
    let fails = std::fs::read_to_string(path.with_extension("fails")).ok();
    let said = fails.as_deref().map(str::trim_end).unwrap_or_default().to_owned();
    Some((printed, said, fails.is_none()))
}

fn shown((printed, said, _): &Outcome) -> String {
    format!("{}{said}", String::from_utf8_lossy(printed))
}

pub(crate) fn llvm(input: &[u8]) -> Result<(), String> {
    if !can_build()? {
        return Ok(());
    }
    let source = printing_plainly(input);
    let mut module =
        fennec_syntax::parse(&source).map_err(|err| format!("it wrote bad code: {err:?}\n{source}"))?;
    fennec_check::check_and_resolve(&mut module)
        .map_err(|err| format!("it wrote bad code: {err:?}\n{source}"))?;
    let work = work();
    let with_program = |wrong: String| format!("{wrong}\n--- program\n{source}");
    let Some(dev) = outcome(&source, &work, false).map_err(with_program)? else { return Ok(()) };
    let Some(release) = outcome(&source, &work, true).map_err(with_program)? else {
        return Err(with_program("it builds as a dev build but not with --release".to_owned()));
    };
    // Built checking proofs (`FENNEC_CHECK_PROOFS=1`), a stop on one is a false proof, whether
    // or not both builds stop the same way.
    for (how, (_, said, _)) in [("dev", &dev), ("--release", &release)] {
        if said.contains("proof broken") {
            return Err(with_program(format!(
                "a proof the compiler made is false, in the {how} build: {said}"
            )));
        }
    }
    if dev != release {
        return Err(with_program(format!(
            "the dev build and the --release build disagree:\n--- dev\n{}\n--- release\n{}",
            shown(&dev),
            shown(&release)
        )));
    }
    if let Some(expected) = pinned(input)
        && expected != dev
    {
        return Err(with_program(format!(
            "LLVM disagrees with the output pinned for this input:\n--- pinned\n{}\n--- LLVM\n{}",
            shown(&expected),
            shown(&dev)
        )));
    }
    Ok(())
}

/// The `json` target's program, which reads its file as several types, printing `Ok(value)` or
/// `Err(why)` for each.
const JSON_READER: &str = "\
from std import json

def main(fs: Fs, args: Args, stdout: Stdout) fails IoError:
    text = fs.read_text(args[0])?
    ints: list[int]? = json.loads[list[int]](text) except why:
        stdout.print(f\"Err({why})\")
        None
    if ints is not None:
        stdout.print(f\"Ok({ints})\")
    names: dict[str, str]? = json.loads[dict[str, str]](text) except why:
        stdout.print(f\"Err({why})\")
        None
    if names is not None:
        stdout.print(f\"Ok({names})\")
    floats: dict[str, list[float]]? = json.loads[dict[str, list[float]]](text) except why:
        stdout.print(f\"Err({why})\")
        None
    if floats is not None:
        stdout.print(f\"Ok({floats})\")
    words: list[str?]? = json.loads[list[str?]](text) except why:
        stdout.print(f\"Err({why})\")
        None
    if words is not None:
        stdout.print(f\"Ok({words})\")
    number: float? = json.loads[float](text) except why:
        stdout.print(f\"Err({why})\")
        None
    if number is not None:
        stdout.print(f\"Ok({number})\")
    truth: bool? = json.loads[bool](text) except why:
        stdout.print(f\"Err({why})\")
        None
    if truth is not None:
        stdout.print(f\"Ok({truth})\")
";

/// The `toml` target's program, which reads its file as several types, printing `Ok(value)` or
/// `Err(why)` for each.
const TOML_READER: &str = "\
from std import toml

def main(fs: Fs, args: Args, stdout: Stdout) fails IoError:
    text = fs.read_text(args[0])?
    names: dict[str, str]? = toml.loads[dict[str, str]](text) except why:
        stdout.print(f\"Err({why})\")
        None
    if names is not None:
        stdout.print(f\"Ok({names})\")
    counts: dict[str, int]? = toml.loads[dict[str, int]](text) except why:
        stdout.print(f\"Err({why})\")
        None
    if counts is not None:
        stdout.print(f\"Ok({counts})\")
    tables: dict[str, dict[str, str]]? = toml.loads[dict[str, dict[str, str]]](text) except why:
        stdout.print(f\"Err({why})\")
        None
    if tables is not None:
        stdout.print(f\"Ok({tables})\")
    floats: dict[str, list[float]]? = toml.loads[dict[str, list[float]]](text) except why:
        stdout.print(f\"Err({why})\")
        None
    if floats is not None:
        stdout.print(f\"Ok({floats})\")
";

/// The `utf8` target's program, which decodes its file's bytes, printing `Ok(text)` or `Err(why)`.
const UTF8_READER: &str = "\
def main(fs: Fs, args: Args, stdout: Stdout) fails IoError:
    decoded = fs.read_bytes(args[0])?.decode() except why:
        stdout.print(f\"Err({why})\")
        return
    stdout.print(f\"Ok({decoded})\")
";

/// The programs the targets that read an input natively run it through, each reading the file
/// it's given. A program here may fail, but never crash or get stuck.
const READERS: [(&str, &str); 14] = [
    ("postgres", crate::postgres_rules::READER),
    ("http-client", crate::http_rules::CLIENT),
    ("http-client-scripts", crate::http_client_scripts::SCRIPTED_CLIENT),
    ("http-server", crate::http_rules::SERVER),
    ("http-sessions", crate::http_rules::SESSIONS_SERVER),
    ("url", crate::url_rules::READER),
    ("csv", crate::csv_rules::READER),
    ("json-rules", crate::json_rules::READER),
    ("json", JSON_READER),
    ("toml", TOML_READER),
    ("utf8", UTF8_READER),
    ("numbers", crate::numbers_rules::READER),
    ("time", crate::time_rules::READER),
    ("base64", crate::base64_rules::READER),
];

/// The Fennec program the target `name` reads its inputs through, if it has one.
pub(crate) fn reader_source(name: &str) -> Option<String> {
    let source = READERS.iter().find(|(named, _)| *named == name).map(|(_, source)| (*source).to_owned());
    source.or_else(|| match name {
        "pack" => Some(crate::pack_rules::reader()),
        "toml-rules" => Some(crate::toml_rules::reader()),
        "router" => Some(crate::router_rules::reader()),
        "html" => Some(crate::html_rules::reader()),
        _ => None,
    })
}

/// The names of the targets that read their inputs through a Fennec program.
pub(crate) fn reader_names() -> Vec<&'static str> {
    READERS.iter().map(|(name, _)| *name).chain(["pack", "toml-rules", "router", "html"]).collect()
}

/// The readers that read what std keeps for its own code, as `postgres`'s reads `PgReader`: a
/// program can import it only through `fennec-std-tests`, so that builds them.
const PRIVATE_READERS: [&str; 1] = ["postgres"];

/// How to build `fennec-std-tests` beside `fennec`.
const BUILD_STD_TESTS: &str =
    "`cargo build -p fennec --features std-private`, with `--release` for a release `fennec`";

/// What builds the reader for `name`: the `fennec` the fuzzer uses, or for a reader of std's own
/// names, `fennec-std-tests` beside it, which only a build with fennec's `std-private` feature
/// makes. One that isn't there is said, with how to build it, rather than built without.
fn compiler_for(name: &str) -> Result<PathBuf, String> {
    let fennec = fennec();
    if !PRIVATE_READERS.contains(&name) {
        return Ok(fennec);
    }
    std_tests_beside(&fennec, name, std::env::consts::EXE_SUFFIX)
}

/// `fennec-std-tests` beside `fennec`, to build the reader for `name`, when it's there and from
/// the same build. One left from an earlier build, as rebuilding `fennec` alone leaves it, would
/// build the reader with another std, and its inputs would all read as findings, so it's refused
/// as a setup to fix. So is one whose build can't be told, from a file with no stamp or one that
/// can't be read, which says which. A system's programs end in `suffix`, as Windows's in `.exe`.
fn std_tests_beside(fennec: &Path, name: &str, suffix: &str) -> Result<PathBuf, String> {
    let beside = fennec.with_file_name(format!("fennec-std-tests{suffix}"));
    if !beside.exists() {
        return Err(format!(
            "the `{name}` reader reads std's private names, so `fennec-std-tests` builds it, and there's none beside {}: build it with {BUILD_STD_TESTS}",
            fennec.display()
        ));
    }
    let fennec = command_file(fennec, suffix);
    let (theirs, ours) = (command_build(&beside)?, command_build(&fennec)?);
    for (build, path) in [(&theirs, &beside), (&ours, &fennec)] {
        if build.is_none() {
            return Err(format!(
                "`{}` holds no stamp saying which build of Fennec it's from, so the `{name}` reader may not be built with the same std as `{}`: build them again with {BUILD_STD_TESTS}",
                path.display(),
                fennec.display()
            ));
        }
    }
    if theirs != ours {
        return Err(format!(
            "`{}` is from another build of Fennec than `{}`, so the `{name}` reader it built would read another std: build it again with {BUILD_STD_TESTS}",
            beside.display(),
            fennec.display()
        ));
    }
    Ok(beside)
}

/// The file the command at `path` runs from. `fennec()` names `fennec` with no suffix, which runs
/// either way, but on a system whose programs end in `suffix`, as Windows's in `.exe`, its file
/// has the suffix, so that's the one read when only it is there.
fn command_file(path: &Path, suffix: &str) -> PathBuf {
    if suffix.is_empty() || path.is_file() {
        return path.to_path_buf();
    }
    let mut named = path.as_os_str().to_owned();
    named.push(suffix);
    let with_suffix = PathBuf::from(named);
    if with_suffix.is_file() { with_suffix } else { path.to_path_buf() }
}

/// The build of Fennec the command at `path` is from, as the stamp `fennec` writes into its file
/// says, or `None` for a file with none. One that can't be read is an error that names it.
fn command_build(path: &Path) -> Result<Option<String>, String> {
    let bytes = std::fs::read(path).map_err(|err| {
        format!("couldn't read `{}` to tell which build of Fennec it's from: {err}", path.display())
    })?;
    // Written in two pieces, so this program's own file doesn't hold the marker whole.
    let marker = ["fennec-command-", "build:"].concat().into_bytes();
    let Some(at) = bytes.windows(marker.len()).position(|window| window == marker.as_slice()) else {
        return Ok(None);
    };
    let hex =
        bytes.get(at + marker.len()..at + marker.len() + 64).and_then(|hex| std::str::from_utf8(hex).ok());
    Ok(hex.filter(|hex| hex.bytes().all(|digit| digit.is_ascii_hexdigit())).map(str::to_owned))
}

/// The readers built so far in this process, by target.
type Built = std::sync::Mutex<Vec<(String, PathBuf)>>;

/// The reader built for `name`, built the first time it's asked for in this process.
fn reader(name: &str) -> Result<PathBuf, String> {
    static BUILT: OnceLock<Built> = OnceLock::new();
    let built = BUILT.get_or_init(Built::default);
    let mut built = built.lock().map_err(|_| "a reader's build failed before".to_owned())?;
    // Another process may have removed one this process built, once it went unused for a while.
    built.retain(|(_, executable)| executable.exists());
    if let Some((_, executable)) = built.iter().find(|(made, _)| made == name) {
        used(executable);
        return Ok(executable.clone());
    }
    let source = reader_source(name).ok_or_else(|| format!("there's no reader for `{name}`"))?;
    let compiler = compiler_for(name)?;
    // Built once for every process, by what it's built from, since cutting a failure down runs a
    // process for each try.
    let readers = std::env::temp_dir().join("fennec-fuzz-readers");
    let folder = readers.join(built_from(&source, &compiler));
    let kept = folder.join("program");
    if !kept.exists() {
        forget_old(&readers);
    }
    let executable = if kept.exists() {
        kept
    } else {
        let made = build_with(&compiler, &source, &work().join(name), false)
            .map_err(|err| {
                // A `fennec-std-tests` left from an earlier build is refused the runtime beside it,
                // as `cargo test -p fennec-fuzz` alone leaves it.
                if PRIVATE_READERS.contains(&name) && err.contains("the runtime library `") {
                    format!(
                        "{err}\n`{}` built it, and it, `fennec` and the runtimes have to be from one build: build it again with {BUILD_STD_TESTS}",
                        compiler.display()
                    )
                } else {
                    err
                }
            })?
            .ok_or_else(|| format!("LLVM doesn't take the `{name}` reader"))?;
        std::fs::create_dir_all(&folder).map_err(|err| format!("{}: {err}", folder.display()))?;
        let copying = folder.join(format!("program.{}", std::process::id()));
        copied(&made, &copying)?;
        put_in_place(&copying, &kept)?;
        kept
    };
    used(&executable);
    built.push((name.to_owned(), executable.clone()));
    Ok(executable)
}

/// Copies a program from `from` to `to`, by another process on Unix, so that this one never holds
/// the copy open for writing.
///
/// A program can't be run while anything holds it open for writing: Linux says the text file is
/// busy. Closing the copy before it's run isn't enough, nor is writing it under another name and
/// renaming it, which keeps the same file. Each of this process's threads that starts a process
/// while the copy is open hands that child a handle to it, which the child holds until it runs
/// its own program. Tests start processes on many threads at once, and one did, between the copy's
/// open and its close, just before the copy was run (main's CI at 17a9f206d). A copy made by `cp`
/// is open only in `cp`, and no thread here can hand it on. Windows hands on no handles that way.
fn copied(from: &Path, to: &Path) -> Result<(), String> {
    let failed = |err: String| format!("couldn't copy `{}` to `{}`: {err}", from.display(), to.display());
    if cfg!(unix) {
        let status = Command::new("cp").arg(from).arg(to).stdin(std::process::Stdio::null()).status();
        return match status {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(failed(format!("`cp` ended with {status}"))),
            Err(err) => Err(failed(err.to_string())),
        };
    }
    std::fs::copy(from, to).map(drop).map_err(|err| failed(err.to_string()))
}

/// Moves a reader just built from `copying` to `kept` whole, so another process never runs half of
/// one.
///
/// Two processes needing the same reader both build it when neither finds it, as tests running
/// side by side do, and the second's lands where the first's may already be running. Unix replaces
/// it, and the first runs on from the file it opened. Windows won't replace a program that's
/// running, and says access is denied. Both were built from the same source by the same `fennec`,
/// so the one in place is kept, and this one's copy is let go.
fn put_in_place(copying: &Path, kept: &Path) -> Result<(), String> {
    let Err(err) = std::fs::rename(copying, kept) else { return Ok(()) };
    let _ = std::fs::remove_file(copying);
    if kept.is_file() { Ok(()) } else { Err(format!("{}: {err}", kept.display())) }
}

/// Marks the folder a reader's in as used now, so it isn't forgotten while it's being used.
fn used(executable: &Path) {
    if let Some(folder) = executable.parent() {
        let _ = File::open(folder).and_then(|folder| folder.set_modified(std::time::SystemTime::now()));
    }
}

/// Removes readers unused for an hour. Each `fennec` build makes its own, so a day of building
/// left 1,500 of them, 6.6 GB.
fn forget_old(readers: &Path) {
    let hour = std::time::Duration::from_hours(1);
    for entry in std::fs::read_dir(readers).into_iter().flatten().flatten() {
        let changed = entry.metadata().and_then(|meta| meta.modified()).ok();
        if changed.and_then(|changed| changed.elapsed().ok()).is_some_and(|age| age > hour) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Whether the runtime with `threads` is beside `fennec`, which it builds a program running
/// `parallel` or `launch` with, as a release does. Without it, such a program runs on the plain
/// runtime, where `launch` fails.
pub(crate) fn threads_runtime_here() -> bool {
    fennec().with_file_name("libfennec_runtime_threads.a").is_file()
}

/// A name for what a reader's built from: its source, the `compiler` that builds it, and the
/// runtimes beside it.
fn built_from(source: &str, compiler: &Path) -> String {
    let mut hasher = fennec_blake3::Hasher::new();
    hasher.update(source.as_bytes());
    let runtimes =
        ["libfennec_runtime.a", "libfennec_runtime_threads.a"].map(|file| compiler.with_file_name(file));
    for made in std::iter::once(compiler.to_path_buf()).chain(runtimes) {
        let changed = std::fs::metadata(&made).and_then(|meta| meta.modified()).ok();
        let since = changed.and_then(|changed| changed.duration_since(std::time::UNIX_EPOCH).ok());
        hasher.update(&since.unwrap_or_default().as_nanos().to_le_bytes());
    }
    let hex = hasher.finalize().to_hex();
    hex.get(..16).unwrap_or(&hex).to_owned()
}

/// The program built for `name`, or None where nothing can be built here.
pub(crate) fn built_reader(name: &str) -> Result<Option<PathBuf>, String> {
    if !can_build()? {
        return Ok(None);
    }
    reader(name).map(Some)
}

/// What the program built for `name` printed, run with `args`, or None where nothing can be built
/// here. It may fail, but must not crash, get stuck or hold too much.
pub(crate) fn run_reader(name: &str, args: &[&std::ffi::OsStr]) -> Result<Option<Vec<u8>>, String> {
    let Some(executable) = built_reader(name)? else { return Ok(None) };
    let work = work().join(name);
    std::fs::create_dir_all(&work).map_err(|err| format!("{}: {err}", work.display()))?;
    ran_holding(&executable, args, &work, READER_KILOBYTES).map(|(printed, _, _)| Some(printed))
}

/// Runs `input` through the reader for `name`, giving what it printed, or None where nothing can
/// be built here. It may fail, but must not crash or get stuck.
pub(crate) fn read_natively(name: &str, input: &[u8]) -> Result<Option<Vec<u8>>, String> {
    if !can_build()? {
        return Ok(None);
    }
    let executable = reader(name)?;
    let work = work().join(name);
    std::fs::create_dir_all(&work).map_err(|err| format!("{}: {err}", work.display()))?;
    let file = work.join("input");
    std::fs::write(&file, input).map_err(|err| format!("{}: {err}", file.display()))?;
    ran_holding(&executable, &[file.as_os_str()], &work, READER_KILOBYTES)
        .map(|(printed, _, _)| Some(printed))
}

#[cfg(test)]
mod placing_tests {
    use std::process::{Command, Stdio};

    use super::{copied, put_in_place};

    /// A reader built while the same one is in place and running lands without an error and leaves
    /// no copy behind. On Windows, which won't replace a program that's running, the one running is
    /// kept: two tests building the `pack` reader at once failed there with access denied.
    #[test]
    fn a_reader_built_twice_lands_while_the_first_runs() {
        #[cfg(windows)]
        let (program, args) = (r"C:\Windows\System32\PING.EXE", ["-n", "30", "127.0.0.1"]);
        #[cfg(not(windows))]
        let (program, args) = ("/bin/sleep", ["30"]);
        let folder = std::env::temp_dir().join(format!("fennec-fuzz-placing-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let kept = folder.join("program");
        assert_eq!(copied(std::path::Path::new(program), &kept), Ok(()));
        let mut running = Command::new(&kept).args(args).stdout(Stdio::null()).spawn().unwrap();
        let copying = folder.join("program.2");
        assert_eq!(copied(std::path::Path::new(program), &copying), Ok(()));
        let placed = put_in_place(&copying, &kept);
        let _ = running.kill();
        let _ = running.wait();
        assert_eq!(placed, Ok(()));
        assert!(kept.is_file(), "{} is gone", kept.display());
        assert!(!copying.exists(), "{} is left", copying.display());
        let _ = std::fs::remove_dir_all(&folder);
    }
}

#[cfg(test)]
mod counted_tests {
    use super::counted;

    #[test]
    fn the_counts_line_is_taken_out_and_its_live_count_read() {
        let said =
            "error: boom\ncopies 1 copied_bytes 8 largest_copy 8 allocations 3 allocated_bytes 90 live 2\n";
        assert_eq!(counted(said), ("error: boom\n".to_owned(), 2));
        assert_eq!(
            counted("copies 0 copied_bytes 0 largest_copy 0 allocations 0 allocated_bytes 0 live 0\n"),
            (String::new(), 0)
        );
        assert_eq!(counted("just this\n"), ("just this\n".to_owned(), 0));
    }
}

#[cfg(test)]
mod cut_tests {
    use super::cut;

    /// A heap audit's report is kept whole, whatever else the program said, and a program the
    /// audit said nothing about is cut as it always was.
    #[test]
    fn every_line_of_a_heap_audit_is_kept() {
        let said = "error: boom\nheap audit: 1 wrong among 3 live blocks\n\
                    heap audit: leaked: str at 0x10 is counted 1 and nothing holds it\nlast\n";
        assert_eq!(
            cut(said),
            "heap audit: 1 wrong among 3 live blocks\n\
             heap audit: leaked: str at 0x10 is counted 1 and nothing holds it"
        );
        assert_eq!(cut("first\nerror: one\nerror: two\n"), "error: two");
        assert_eq!(cut("first\nproof broken here\nerror: two\n"), "proof broken here");
    }
}

#[cfg(test)]
mod tests {
    use super::{fuzzing_process, std_tests_beside};

    /// A `fennec-std-tests` from another build than the `fennec` beside it is refused, saying how
    /// to build it again, and one from the same build is used. Each is a file holding a stamp. A
    /// file with none, or one that can't be read, is refused, saying which. On a system whose
    /// programs end in `.exe`, `fennec` is named without it, as the fuzzer names it, and read with
    /// it: Windows CI found the plain name unreadable, and read it as another build.
    #[test]
    fn a_fennec_std_tests_from_another_build_is_refused() {
        let folder = std::env::temp_dir().join(format!("fennec-fuzz-stamps-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        for suffix in ["", ".exe"] {
            let file = |name: &str, holding: &str| {
                let path = folder.join(format!("{name}{suffix}"));
                std::fs::write(&path, format!("code before\0{holding}\0code after")).unwrap();
                path
            };
            let stamp =
                |digit: &str| format!("{}{}", ["fennec-command-", "build:"].concat(), digit.repeat(64));
            file("fennec", &stamp("a"));
            // As the fuzzer names it, with no suffix.
            let fennec = folder.join("fennec");
            file("fennec-std-tests", &stamp("b"));
            let refused = std_tests_beside(&fennec, "postgres", suffix).unwrap_err();
            assert!(refused.contains("is from another build of Fennec"), "{suffix}: {refused}");
            assert!(refused.contains("cargo build -p fennec --features std-private"), "{suffix}: {refused}");
            let same = file("fennec-std-tests", &stamp("a"));
            assert_eq!(std_tests_beside(&fennec, "postgres", suffix), Ok(same), "{suffix}");
            file("fennec-std-tests", "no stamp");
            let unstamped = std_tests_beside(&fennec, "postgres", suffix).unwrap_err();
            assert!(unstamped.contains("holds no stamp"), "{suffix}: {unstamped}");
            std::fs::remove_dir_all(&folder).unwrap();
            std::fs::create_dir_all(&folder).unwrap();
        }
        // A `fennec` that isn't there can't be read, and that's what's said.
        std::fs::write(folder.join("fennec-std-tests"), "no stamp").unwrap();
        let unread = std_tests_beside(&folder.join("fennec"), "postgres", "").unwrap_err();
        assert!(unread.contains("couldn't read"), "{unread}");
        std::fs::remove_dir_all(&folder).unwrap();
    }

    /// The sweep knows each kind of folder fuzzing makes by its process id, and leaves the
    /// readers, the tests' own folders and anything else alone.
    #[test]
    fn it_knows_the_folders_fuzzing_made() {
        let names = ["fennec-fuzz-12", "fennec-fuzz-llvm-34", "fennec-fuzz-small-56"];
        assert_eq!(names.map(fuzzing_process), [Some(12), Some(34), Some(56)]);
        let others = [
            "fennec-fuzz-readers",
            "fennec-fuzz-group-7",
            "fennec-fuzz-short-8",
            "fennec-cli-9",
            "fennec-fuzz-",
        ];
        assert_eq!(others.map(fuzzing_process), [None; 5]);
    }
}
