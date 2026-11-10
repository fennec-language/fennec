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
use crate::supervise::{MOST_KILOBYTES, STUCK, watch};

/// `fennec`: where `FENNEC` says, or beside this program.
fn fennec() -> PathBuf {
    std::env::var_os("FENNEC")
        .map_or_else(|| std::env::current_exe().unwrap_or_default().with_file_name("fennec"), PathBuf::from)
}

/// This process's own folder to build in.
fn work() -> PathBuf {
    std::env::temp_dir().join(format!("fennec-fuzz-llvm-{}", std::process::id()))
}

/// Whether a program can be built and run here, or why not.
pub(crate) fn ready() -> Result<(), String> {
    static READY: OnceLock<Result<(), String>> = OnceLock::new();
    READY
        .get_or_init(|| {
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

/// Builds `source` in `work`, giving back the executable, or None when the backend doesn't take
/// the program yet.
fn build(source: &str, work: &Path, release: bool) -> Result<Option<PathBuf>, String> {
    std::fs::create_dir_all(work).map_err(|err| format!("{}: {err}", work.display()))?;
    let (program, executable) = (work.join("program.fen"), work.join("program"));
    std::fs::write(&program, source).map_err(|err| format!("{}: {err}", program.display()))?;
    let built = Command::new(fennec())
        .args(["build", "--llvm"])
        .args(release.then_some("--release"))
        .arg(&program)
        .arg("-o")
        .arg(&executable)
        // Its own home and no cache, so fuzzing leaves nothing behind in the real ones.
        .env("FENNEC_HOME", work)
        .env("FENNEC_NO_CACHE", "1")
        .output()
        .map_err(|err| format!("couldn't run `{}`: {err}", fennec().display()))?;
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
/// printed, the first line it said on standard error, and whether it succeeded. A build that
/// fails gives what `fennec` said, whatever the reason, for programs that should always build.
pub(crate) fn built_and_run(
    source: &str,
    name: &str,
    release: bool,
) -> Result<(Vec<u8>, String, bool), String> {
    let work = work().join(name);
    std::fs::create_dir_all(&work).map_err(|err| format!("{}: {err}", work.display()))?;
    let (program, executable) = (work.join("program.fen"), work.join("program"));
    std::fs::write(&program, source).map_err(|err| format!("{}: {err}", program.display()))?;
    let built = Command::new(fennec())
        .args(["build", "--llvm"])
        .args(release.then_some("--release"))
        .arg(&program)
        .arg("-o")
        .arg(&executable)
        .env("FENNEC_HOME", &work)
        .env("FENNEC_NO_CACHE", "1")
        .output()
        .map_err(|err| format!("couldn't run `{}`: {err}", fennec().display()))?;
    if !built.status.success() {
        let how = if release { "with --release" } else { "as a dev build" };
        return Err(format!("it didn't build {how}:\n{}", String::from_utf8_lossy(&built.stderr)));
    }
    let (printed, said, succeeded) = ran(&executable, &[], &work)?;
    // The first line, or the first that says what a watching build caught, which a sanitizer's
    // report doesn't start with.
    let caught = said.lines().find(|line| CAUGHT.iter().any(|what| line.contains(what)));
    Ok((printed, caught.or_else(|| said.lines().next()).unwrap_or_default().to_owned(), succeeded))
}

/// What a build that watches itself says when it catches something: a false proof, memory the
/// program doesn't own, a native block handed to the runtime.
pub(crate) const CAUGHT: [&str; 3] = ["proof broken", "AddressSanitizer", "handed a native block"];

/// Runs what `built_and_run` last made under `name` again, on `threads` threads, with what it
/// said cut as `built_and_run` cuts it.
pub(crate) fn ran_again_on(name: &str, threads: usize) -> Result<(Vec<u8>, String, bool), String> {
    let work = work().join(name);
    let (printed, said, succeeded) =
        ran_on(&work.join("program"), &[], &work, MOST_KILOBYTES, Some(threads))?;
    let caught = said.lines().find(|line| CAUGHT.iter().any(|what| line.contains(what)));
    Ok((printed, caught.or_else(|| said.lines().next()).unwrap_or_default().to_owned(), succeeded))
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
    let mut child = command
        .args(args)
        .stdout(file(&out)?)
        .stderr(file(&err)?)
        .spawn()
        .map_err(|err| format!("couldn't run what LLVM built: {err}"))?;
    let failure = watch(&mut child, STUCK, None, most_kilobytes, || false);
    let printed = std::fs::read(&out).unwrap_or_default();
    let said = String::from_utf8_lossy(&std::fs::read(&err).unwrap_or_default()).into_owned();
    match (failure, child.wait()) {
        (None, Ok(status)) => Ok((printed, said, status.success())),
        (_, Ok(status)) if status.code() == Some(1) => Ok((printed, said, false)),
        (failure, status) => Err(format!("what LLVM built {failure:?} ({status:?}):\n{said}")),
    }
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
/// were written while the interpreter could say what each program does.
const PINNED: [&str; 2] = ["seeds/jit", "found/llvm"];

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
    ints: list[int]? = json.loads[list[int]](text) else as why:
        stdout.print(f\"Err({why})\")
        None
    if ints is not None:
        stdout.print(f\"Ok({ints})\")
    names: dict[str, str]? = json.loads[dict[str, str]](text) else as why:
        stdout.print(f\"Err({why})\")
        None
    if names is not None:
        stdout.print(f\"Ok({names})\")
    floats: dict[str, list[float]]? = json.loads[dict[str, list[float]]](text) else as why:
        stdout.print(f\"Err({why})\")
        None
    if floats is not None:
        stdout.print(f\"Ok({floats})\")
    words: list[str?]? = json.loads[list[str?]](text) else as why:
        stdout.print(f\"Err({why})\")
        None
    if words is not None:
        stdout.print(f\"Ok({words})\")
    number: float? = json.loads[float](text) else as why:
        stdout.print(f\"Err({why})\")
        None
    if number is not None:
        stdout.print(f\"Ok({number})\")
    truth: bool? = json.loads[bool](text) else as why:
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
    names: dict[str, str]? = toml.loads[dict[str, str]](text) else as why:
        stdout.print(f\"Err({why})\")
        None
    if names is not None:
        stdout.print(f\"Ok({names})\")
    counts: dict[str, int]? = toml.loads[dict[str, int]](text) else as why:
        stdout.print(f\"Err({why})\")
        None
    if counts is not None:
        stdout.print(f\"Ok({counts})\")
    tables: dict[str, dict[str, str]]? = toml.loads[dict[str, dict[str, str]]](text) else as why:
        stdout.print(f\"Err({why})\")
        None
    if tables is not None:
        stdout.print(f\"Ok({tables})\")
    floats: dict[str, list[float]]? = toml.loads[dict[str, list[float]]](text) else as why:
        stdout.print(f\"Err({why})\")
        None
    if floats is not None:
        stdout.print(f\"Ok({floats})\")
";

/// The `utf8` target's program, which decodes its file's bytes, printing `Ok(text)` or `Err(why)`.
const UTF8_READER: &str = "\
def main(fs: Fs, args: Args, stdout: Stdout) fails IoError:
    decoded = fs.read_bytes(args[0])?.decode() else as why:
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

/// The readers built so far in this process, by target.
type Built = std::sync::Mutex<Vec<(String, PathBuf)>>;

/// The reader built for `name`, built the first time it's asked for in this process.
fn reader(name: &str) -> Result<PathBuf, String> {
    static BUILT: OnceLock<Built> = OnceLock::new();
    let built = BUILT.get_or_init(Built::default);
    let mut built = built.lock().map_err(|_| "a reader's build failed before".to_owned())?;
    if let Some((_, executable)) = built.iter().find(|(made, _)| made == name) {
        return Ok(executable.clone());
    }
    let source = READERS.iter().find(|(named, _)| *named == name).map(|(_, source)| (*source).to_owned());
    let source = source.or_else(|| match name {
        "pack" => Some(crate::pack_rules::reader()),
        "toml-rules" => Some(crate::toml_rules::reader()),
        _ => None,
    });
    let source = source.ok_or_else(|| format!("there's no reader for `{name}`"))?;
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
    built.push((name.to_owned(), executable.clone()));
    Ok(executable)
}

/// Removes readers built more than a day ago, which a rebuilt `fennec` no longer uses.
fn forget_old(readers: &Path) {
    let day = std::time::Duration::from_hours(24);
    for entry in std::fs::read_dir(readers).into_iter().flatten().flatten() {
        let changed = entry.metadata().and_then(|meta| meta.modified()).ok();
        if changed.and_then(|changed| changed.elapsed().ok()).is_some_and(|age| age > day) {
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
