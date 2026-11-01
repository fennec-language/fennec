//! The `llvm` target: a generated program built by `fennec build --llvm` must print what the
//! interpreter prints, and fail the same way. Half the inputs build with `--release`, which
//! optimizes the whole program as one. Structs and enums are printed through what they give, since
//! the backend can't print them whole yet, and a program it doesn't take yet is passed over.
//!
//! This needs the `fennec` command beside this one (or where `FENNEC` says), the runtime library
//! beside that, and LLVM's tools, so it runs only when named. `fennec-fuzz run llvm` stops if one
//! is missing, and replaying a kept input without them passes.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crate::generate::printing_plainly;
use crate::supervise::{STUCK, watch};

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
                Some(executable) if ran(&executable, &work())?.0 == b"ready\n" => Ok(()),
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

/// What an executable printed, what it said on standard error, and whether it succeeded. One
/// that crashes, gets stuck or holds too much memory is stopped and said so.
fn ran(executable: &Path, work: &Path) -> Result<(Vec<u8>, String, bool), String> {
    let (out, err) = (work.join("stdout"), work.join("stderr"));
    let file = |path: &Path| File::create(path).map_err(|err| format!("{}: {err}", path.display()));
    let mut child = Command::new(executable)
        .stdout(file(&out)?)
        .stderr(file(&err)?)
        .spawn()
        .map_err(|err| format!("couldn't run what LLVM built: {err}"))?;
    let failure = watch(&mut child, STUCK, None, || false);
    let printed = std::fs::read(&out).unwrap_or_default();
    let said = String::from_utf8_lossy(&std::fs::read(&err).unwrap_or_default()).into_owned();
    match (failure, child.wait()) {
        (None, Ok(status)) => Ok((printed, said, status.success())),
        (_, Ok(status)) if status.code() == Some(1) => Ok((printed, said, false)),
        (failure, status) => Err(format!("what LLVM built {failure:?} ({status:?}):\n{said}")),
    }
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
    let mut expected = Vec::new();
    let failure = fennec_interp::run(&module, &mut expected).err().map(|failure| failure.message);
    let work = work();
    let release = input.len() % 2 == 1;
    let built = build(&source, &work, release).map_err(|wrong| format!("{wrong}\n--- program\n{source}"))?;
    let Some(executable) = built else { return Ok(()) };
    let (printed, said, succeeded) =
        ran(&executable, &work).map_err(|wrong| format!("{wrong}\n--- program\n{source}"))?;
    let first_line = said.lines().next().unwrap_or_default();
    let same_failure = match &failure {
        None => succeeded,
        Some(message) => {
            !succeeded && first_line == format!("error: {}", message.lines().next().unwrap_or_default())
        }
    };
    if printed != expected || !same_failure {
        return Err(format!(
            "LLVM disagrees with the interpreter{}:\n--- interpreter\n{}{failure:?}\n--- LLVM\n{}{said}\n--- program\n{source}",
            if release { " with --release" } else { "" },
            String::from_utf8_lossy(&expected),
            String::from_utf8_lossy(&printed),
        ));
    }
    Ok(())
}
