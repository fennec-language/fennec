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
//! like replaying a kept input, pass without them.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crate::generate::printing_plainly;
use crate::supervise::{BUILD_STUCK, MOST_KILOBYTES, STUCK, output_watched, watch};

/// `fennec`: where `FENNEC` says, or beside this program.
fn fennec() -> PathBuf {
    std::env::var_os("FENNEC")
        .map_or_else(|| std::env::current_exe().unwrap_or_default().with_file_name("fennec"), PathBuf::from)
}

/// This process's own folder to build in.
fn work() -> PathBuf {
    work_of(std::process::id())
}

/// The folder process `pid` builds in.
fn work_of(pid: u32) -> PathBuf {
    std::env::temp_dir().join(format!("{WORK}{pid}"))
}

/// What every process's folder to build in is named from, before its process id.
const WORK: &str = "fennec-fuzz-llvm-";

/// Removes the folder process `pid` built in, once it's ended. A failing program is kept as its
/// input, which builds it again, so nothing in the folder is needed after.
pub(crate) fn forget(pid: u32) {
    let _ = std::fs::remove_dir_all(work_of(pid));
}

/// Removes the folders fuzzing processes made that ended without removing them: a run, a worker
/// or a try at cutting a failure down that was stopped, or a test's. One whose process id is
/// running again, as another process, is removed once it's a day old. When which processes are
/// running can't be asked, it removes nothing.
fn forget_ended() {
    let Ok(listed) = Command::new("ps").args(["-A", "-o", "pid="]).output() else { return };
    if !listed.status.success() {
        return;
    }
    let running: std::collections::HashSet<u32> = String::from_utf8_lossy(&listed.stdout)
        .split_whitespace()
        .filter_map(|pid| pid.parse().ok())
        .collect();
    if running.is_empty() {
        return;
    }
    let day = std::time::Duration::from_hours(24);
    for entry in std::fs::read_dir(std::env::temp_dir()).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(fuzzing_process) else { continue };
        let changed = entry.metadata().and_then(|meta| meta.modified()).ok();
        let old = changed.and_then(|changed| changed.elapsed().ok()).is_some_and(|age| age > day);
        if !running.contains(&pid) || old {
            let _ = std::fs::remove_dir_all(entry.path());
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
/// it doesn't take yet.
const BROKEN: [&str; 5] = ["linking failed", "failed on", "couldn't run", "once trimmed", "panicked"];

/// What `fennec build` gave, run with the tools it starts in a group of their own: one whose
/// tools together take more than `BUILD_STUCK` CPU time or `MOST_KILOBYTES` of memory is
/// stopped, every tool with it, and that's a failure of its own, since building a program
/// should never take that.
fn built_within(command: &mut Command) -> Result<std::process::Output, String> {
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
fn build(source: &str, work: &Path, release: bool) -> Result<Option<PathBuf>, String> {
    std::fs::create_dir_all(work).map_err(|err| format!("{}: {err}", work.display()))?;
    let (program, executable) = (work.join("program.fen"), work.join("program"));
    std::fs::write(&program, source).map_err(|err| format!("{}: {err}", program.display()))?;
    let mut command = Command::new(fennec());
    command
        .args(["build", "--llvm"])
        .args(release.then_some("--release"))
        .arg(&program)
        .arg("-o")
        .arg(&executable)
        // Its own home and no cache, so fuzzing leaves nothing behind in the real ones.
        .env("FENNEC_HOME", work)
        .env("FENNEC_NO_CACHE", "1");
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
        .env("FENNEC_NO_CACHE", "1");
    let built = built_within(&mut command)?;
    if !built.status.success() {
        let how = if release { "with --release" } else { "as a dev build" };
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
    let mut command = Command::new(executable);
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
    if ready().is_err() {
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
const READERS: [(&str, &str); 9] = [
    ("postgres", crate::postgres_rules::READER),
    ("http-client", crate::http_rules::CLIENT),
    ("http-server", crate::http_rules::SERVER),
    ("url", crate::url_rules::READER),
    ("csv", crate::csv_rules::READER),
    ("json-rules", crate::json_rules::READER),
    ("json", JSON_READER),
    ("toml", TOML_READER),
    ("utf8", UTF8_READER),
];

/// The Fennec program the target `name` reads its inputs through, if it has one.
pub(crate) fn reader_source(name: &str) -> Option<String> {
    let source = READERS.iter().find(|(named, _)| *named == name).map(|(_, source)| (*source).to_owned());
    source.or_else(|| match name {
        "pack" => Some(crate::pack_rules::reader()),
        "toml-rules" => Some(crate::toml_rules::reader()),
        _ => None,
    })
}

/// The names of the targets that read their inputs through a Fennec program.
pub(crate) fn reader_names() -> Vec<&'static str> {
    READERS.iter().map(|(name, _)| *name).chain(["pack", "toml-rules"]).collect()
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
    // Built once for every process, by what it's built from, since cutting a failure down runs a
    // process for each try.
    let readers = std::env::temp_dir().join("fennec-fuzz-readers");
    let folder = readers.join(built_from(&source));
    let kept = folder.join("program");
    if !kept.exists() {
        forget_old(&readers);
    }
    let executable = if kept.exists() {
        kept
    } else {
        let made = build(&source, &work().join(name), false)?
            .ok_or_else(|| format!("LLVM doesn't take the `{name}` reader"))?;
        std::fs::create_dir_all(&folder).map_err(|err| format!("{}: {err}", folder.display()))?;
        // Moved into place whole, so another process never runs half of one.
        let copying = folder.join(format!("program.{}", std::process::id()));
        std::fs::copy(&made, &copying).map_err(|err| format!("{}: {err}", copying.display()))?;
        std::fs::rename(&copying, &kept).map_err(|err| format!("{}: {err}", kept.display()))?;
        kept
    };
    used(&executable);
    built.push((name.to_owned(), executable.clone()));
    Ok(executable)
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

/// A name for what a reader's built from: its source, and the `fennec` and runtimes that build it.
fn built_from(source: &str) -> String {
    let mut hasher = fennec_blake3::Hasher::new();
    hasher.update(source.as_bytes());
    let fennec = fennec();
    let runtimes =
        ["libfennec_runtime.a", "libfennec_runtime_threads.a"].map(|file| fennec.with_file_name(file));
    for made in std::iter::once(fennec.clone()).chain(runtimes) {
        let changed = std::fs::metadata(&made).and_then(|meta| meta.modified()).ok();
        let since = changed.and_then(|changed| changed.duration_since(std::time::UNIX_EPOCH).ok());
        hasher.update(&since.unwrap_or_default().as_nanos().to_le_bytes());
    }
    let hex = hasher.finalize().to_hex();
    hex.get(..16).unwrap_or(&hex).to_owned()
}

/// The program built for `name`, or None where nothing can be built here.
pub(crate) fn built_reader(name: &str) -> Result<Option<PathBuf>, String> {
    if ready().is_err() {
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
    if ready().is_err() {
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
    use super::fuzzing_process;

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
