//! `fennec build`: every example that fails while running fails the same way as a native
//! program, with the interpreter's message. That every example builds and prints exactly its
//! `.out` file is `llvm.rs`'s `the_examples_it_takes_print_what_they_should`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

/// A `fennec` to start, with the runtime library built first. `fennec run` links against that
/// library, and one left from before a rebase links without a word and gets things wrong: a
/// dict's layout that no longer matches, say, printing zeroes. So nothing starts `fennec` until
/// the library is current. Under mutation that's each mutant's own library, or a mutant in the
/// runtime would meet the old one. A test that needs the library says so when it couldn't be built.
pub(crate) fn fennec_command() -> Command {
    runtime_built();
    Command::new(env!("CARGO_BIN_EXE_fennec"))
}

/// The runtime library executables link against, which `fennec build` expects next to
/// itself. `cargo test` doesn't build it, so this does, once a run: an old one would be
/// missing what the interpreter has learned since. Only once, because even when it's
/// current Cargo puts the library in place again, removing it first, so a test linking
/// while another builds it could find it missing. `llvm.rs` uses this too.
pub(crate) fn runtime_built() -> bool {
    static BUILT: OnceLock<bool> = OnceLock::new();
    *BUILT.get_or_init(|| {
        let profile = if cfg!(debug_assertions) { None } else { Some("--release") };
        Command::new(env!("CARGO"))
            .args(["build", "--quiet", "-p", "fennec-runtime"])
            .args(profile)
            .status()
            .is_ok_and(|status| status.success())
    })
}

/// The runtime library's file name, as Cargo names a static library here.
pub(crate) fn runtime_file() -> &'static str {
    if cfg!(windows) { "fennec_runtime.lib" } else { "libfennec_runtime.a" }
}

/// Whether the runtime library is beside the `fennec` these tests run, which is where it's looked
/// for. Under coverage, `fennec` is built somewhere else, without one.
pub(crate) fn runtime_beside_fennec() -> bool {
    Path::new(env!("CARGO_BIN_EXE_fennec")).with_file_name(runtime_file()).is_file()
}

/// The runtime library with `threads`, which `fennec build` takes for a program that uses
/// `parallel` when `FENNEC_THREADS_RUNTIME=1` asks, next to `fennec` as
/// `libfennec_runtime_threads.a`. It's built in a folder of its own, since Cargo puts the same
/// file name in place for both, once a run as `runtime_built` does.
pub(crate) fn threads_runtime_built() -> bool {
    static BUILT: OnceLock<bool> = OnceLock::new();
    *BUILT.get_or_init(|| runtime_variant_built("threads", "threads", "fennec_runtime_threads"))
}

/// The runtime library with no interpreter in it, which `fennec build` takes when
/// `FENNEC_LEAN_RUNTIME=1` asks, next to `fennec` as `libfennec_runtime_lean.a`.
pub(crate) fn lean_runtime_built() -> bool {
    static BUILT: OnceLock<bool> = OnceLock::new();
    *BUILT.get_or_init(|| runtime_variant_built("lean", "lean", "fennec_runtime_lean"))
}

/// Builds the runtime library with `features`, in a folder of its own named `folder`, beside the
/// plain build's `target/debug`, and puts it next to `fennec` as `name`.
fn runtime_variant_built(features: &str, folder: &str, name: &str) -> bool {
    let Some(beside) = Path::new(env!("CARGO_BIN_EXE_fennec")).parent() else { return false };
    let Some(own) = beside.parent().map(|target| target.join(folder)) else { return false };
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    let built = Command::new(env!("CARGO"))
        .args(["build", "--quiet", "-p", "fennec-runtime", "--features", features, "--target-dir"])
        .arg(&own)
        .args((profile == "release").then_some("--release"))
        .status()
        .is_ok_and(|status| status.success());
    let library = own.join(profile).join(runtime_file());
    let file = if cfg!(windows) { format!("{name}.lib") } else { format!("lib{name}.a") };
    // Copied under another name and then renamed, so a build linking it at that moment, in this
    // test run or another, finds the old library or the new one and never half of one.
    let copying = beside.join(format!("{file}.{}.copying", std::process::id()));
    built && fs::copy(library, &copying).is_ok() && fs::rename(&copying, beside.join(file)).is_ok()
}

/// Whether mutation testing is running the tests (CI's mutation jobs set
/// `FENNEC_UNDER_MUTANTS`). Linking every example through the JIT's objects takes longer than it's
/// worth for each mutant, so these two tests stand aside there: every other run of the tests runs
/// them, and the LLVM tests run under mutants too.
fn under_mutants() -> bool {
    std::env::var_os("FENNEC_UNDER_MUTANTS").is_some()
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

/// Builds a program into `work`, giving back the executable, or what `fennec build` said
/// when it doesn't build.
fn build(path: &Path, work: &Path) -> std::io::Result<Result<PathBuf, Output>> {
    let executable = work.join(path.file_stem().unwrap_or_default());
    let built = fennec_command().arg("build").arg(path).arg("-o").arg(&executable).output()?;
    Ok(if built.status.success() { Ok(executable) } else { Err(built) })
}

fn run(executable: &Path) -> std::io::Result<Output> {
    Command::new(executable).output()
}

/// How many builds the examples' tests run at once: one per core, up to 8. One at a time, LLVM's
/// took 71 seconds and 8 take 29; holding under 100 megabytes each, they stay well inside 4
/// gigabytes. `llvm.rs` uses this too.
fn builds_at_once() -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZero::get).min(8)
}

/// Runs `each` on every item, `builds_at_once()` at a time, each worker with its own number for a
/// folder of its own. The results come back in the items' order.
pub(crate) fn at_once<T: Sync, R: Send>(
    items: &[T],
    each: impl Fn(usize, &T) -> std::io::Result<R> + Sync,
) -> std::io::Result<Vec<R>> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let done = std::sync::Mutex::new(Vec::with_capacity(items.len()));
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..builds_at_once().min(items.len()))
            .map(|worker| {
                let (next, done, each) = (&next, &done, &each);
                scope.spawn(move || -> std::io::Result<()> {
                    loop {
                        let at = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(item) = items.get(at) else { return Ok(()) };
                        let result = each(worker, item)?;
                        done.lock()
                            .map_err(|_| std::io::Error::other("a worker panicked"))?
                            .push((at, result));
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().map_err(|_| std::io::Error::other("a worker panicked"))??;
        }
        Ok::<_, std::io::Error>(())
    })?;
    let mut done = done.into_inner().map_err(|_| std::io::Error::other("a worker panicked"))?;
    done.sort_by_key(|(at, _)| *at);
    Ok(done.into_iter().map(|(_, result)| result).collect())
}

/// A fresh folder for this test's executables.
fn work_dir(test: &str) -> std::io::Result<PathBuf> {
    let work = std::env::temp_dir().join(format!("fennec-executables-{test}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work)?;
    Ok(work)
}

/// Error examples whose native program fails otherwise than the interpreter does, with what
/// differs. Each must still differ, so the list only shrinks. The first two are a check native
/// code doesn't make yet: it runs on, where the interpreter stops.
const FAILS_DIFFERENTLY: &[(&str, &str)] = &[
    ("expect_without_reason.fen", "native code doesn't check `expect`'s reason isn't blank, and runs on"),
    ("ignore_without_reason.fen", "native code doesn't check `ignore`'s reason isn't blank, and runs on"),
    ("random_choice_empty.fen", "native code points at the standard library, not at the call"),
    (
        "change_missing_key.fen",
        "native code underlines all of `+=`, as `llvm.rs` notes, and adds a help line",
    ),
    ("where_on_copy.fen", "native code leaves out the help line"),
    ("where_on_write.fen", "native code underlines all of the write and leaves out the help line"),
    ("where_rule.fen", "native code leaves out the help line"),
    ("where_through_an_item.fen", "native code underlines all of the write and leaves out the help line"),
];

#[test]
#[cfg_attr(windows, ignore = "on Windows, `llvm.rs` builds and runs the examples")]
fn failing_examples_fail_the_same_way_as_executables() {
    if under_mutants() {
        return;
    }
    assert!(runtime_built(), "couldn't build the runtime library");
    let work = work_dir("errors").unwrap();
    let paths = programs(&examples_dir().join("errors")).unwrap();
    // For each that built, what was wrong with it, if anything.
    let found = at_once(&paths, |_, path| {
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        // Most error examples are mistakes the checker catches, which don't build.
        let Ok(executable) = build(path, &work)? else { return Ok(None) };
        let ran = run(&executable)?;
        // `fennec run` runs an executable itself where LLVM builds the program, so the
        // interpreter is what an executable's failure is held to.
        let interpreted = fennec_command().args(["run", "--interp"]).arg(path).output()?;
        let (ours, theirs) =
            (String::from_utf8_lossy(&ran.stderr), String::from_utf8_lossy(&interpreted.stderr));
        let differs = if ran.status.success() {
            Some(format!("{name} should fail"))
        } else if ours != theirs {
            Some(format!("{name} failed differently:\n{ours}\nwhere the interpreter said\n{theirs}"))
        } else if ran.stdout != interpreted.stdout {
            Some(format!("{name} printed something different before failing"))
        } else {
            None
        };
        let known = FAILS_DIFFERENTLY.iter().any(|(differing, _)| *differing == name);
        Ok(Some(match differs {
            Some(_) if known => None,
            None if known => {
                Some(format!("{name} fails as the interpreter does now: take it off `FAILS_DIFFERENTLY`"))
            }
            differs => differs,
        }))
    })
    .unwrap();
    let built: Vec<Option<String>> = found.into_iter().flatten().collect();
    assert!(!built.is_empty(), "no error example built");
    let wrong: Vec<String> = built.into_iter().flatten().collect();
    assert!(wrong.is_empty(), "{}", wrong.join("\n\n"));
    let _ = fs::remove_dir_all(&work);
}
