//! Timing runs and counting instructions. Each program runs several times, taking turns with
//! the others so they share whatever the machine is doing, and its median run counts.
//!
//! Instruction counts are what's compared with the targets: unlike time, they barely move when
//! the machine is busy, and they don't depend on how fast it is. Where the CPU's counters can
//! be read, `perf` counts. Where they can't, as on CI's virtual machines, Valgrind counts by
//! simulating, about 50 times slower. macOS's `time` counts on a Mac.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub(crate) use fennec_count::{Counter, Program};
use fennec_count::{Run, median};

/// One measured program: its median time, and what it printed.
#[derive(Clone, Debug)]
pub(crate) struct Timing {
    pub(crate) median: Duration,
    pub(crate) output: String,
    /// What it wrote to stderr, where the JIT says it fell back to the interpreter.
    pub(crate) notes: String,
}

/// Runs each program `runs` times, taking turns, and keeps each one's median time and count.
pub(crate) fn time_together(programs: &[Program], runs: usize) -> Result<Vec<Timing>, String> {
    let mut all: Vec<Vec<Run>> = programs.iter().map(|_| Vec::with_capacity(runs)).collect();
    for _ in 0..runs {
        for (program, runs) in programs.iter().zip(all.iter_mut()) {
            runs.push(program.run()?);
        }
    }
    let mut timings = Vec::with_capacity(programs.len());
    for runs in all {
        let mut times: Vec<Duration> = runs.iter().map(|run| run.took).collect();
        let Some(first) = runs.into_iter().next() else { continue };
        timings.push(Timing {
            median: median(&mut times, |a, b| (a + b) / 2).unwrap_or_default(),
            output: first.stdout,
            notes: first.stderr,
        });
    }
    Ok(timings)
}

/// Builds the regular expression engine behind `std.re`, for a twin that runs on it, and gives
/// back the arguments that link it.
fn engine_for(source: &Path, out_dir: &Path) -> Result<Vec<String>, String> {
    let uses_it = std::fs::read_to_string(source).is_ok_and(|text| text.contains("fennec_re"));
    if !uses_it {
        return Ok(Vec::new());
    }
    let library = out_dir.join("libfennec_re.rlib");
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc)
        .args(["--edition", "2024", "--crate-type", "rlib", "--crate-name", "fennec_re"])
        .args(OPTIMIZED)
        .args(twin_cpu())
        .arg("-o")
        .arg(&library)
        .arg("crates/re/src/lib.rs")
        .output()
        .map_err(|err| format!("couldn't run rustc: {err}"))?;
    if !output.status.success() {
        return Err(format!("crates/re didn't compile:\n{}", String::from_utf8_lossy(&output.stderr)));
    }
    Ok(vec!["--extern".to_owned(), format!("fennec_re={}", library.display())])
}

/// How twins are optimized: as far as the compiler goes.
const OPTIMIZED: [&str; 8] =
    ["-C", "opt-level=3", "-C", "codegen-units=1", "-C", "panic=abort", "-C", "debug-assertions=no"];

/// The CPU level twins are built for: fixed, so a target doesn't depend on which machine pinned
/// it, and matching what the JIT uses. Cranelift emits VEX instructions on x86-64, which is
/// worth about 20 points on `mandelbrot` against a twin built for the baseline. On 64-bit Arm
/// the baseline already has what the JIT uses.
fn twin_cpu() -> &'static [&'static str] {
    if cfg!(target_arch = "x86_64") { &["-C", "target-cpu=x86-64-v3"] } else { &[] }
}

/// The version of the compiler that builds the twins, which targets depend on.
pub(crate) fn rustc_version() -> Result<String, String> {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output =
        Command::new(rustc).arg("--version").output().map_err(|err| format!("couldn't run rustc: {err}"))?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Compiles a Rust twin, fully optimized, with link-time optimization across everything it uses.
pub(crate) fn build_twin(source: &Path, out_dir: &Path) -> Result<PathBuf, String> {
    let name = source.file_stem().map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
    let binary = out_dir.join(name);
    let engine = engine_for(source, out_dir)?;
    let crate_name =
        binary.file_name().map_or_else(String::new, |name| name.to_string_lossy().replace('.', "_"));
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc)
        .args(["--edition", "2024", "-C", "lto=fat"])
        .args(OPTIMIZED)
        .args(twin_cpu())
        .args(engine)
        // A name like `dicts.std` isn't a crate's name, whatever its file is called.
        .args(["--crate-name", &crate_name])
        .arg("-o")
        .arg(&binary)
        .arg(source)
        .output()
        .map_err(|err| format!("couldn't run rustc: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "{} didn't compile:\n{}",
            source.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(binary)
}
