//! Building the Rust twins and timing runs.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// One timed program: its fastest run and what it printed.
#[derive(Clone, Debug)]
pub(crate) struct Timing {
    pub(crate) best: Duration,
    pub(crate) output: String,
    /// What it wrote to stderr, where the JIT says it fell back to the interpreter.
    pub(crate) notes: String,
}

/// A program to run: a command and its arguments.
pub(crate) struct Program {
    pub(crate) command: PathBuf,
    pub(crate) args: Vec<String>,
}

impl Program {
    fn run(&self) -> Result<(Duration, String, String), String> {
        let started = Instant::now();
        let output = Command::new(&self.command)
            .args(&self.args)
            .output()
            .map_err(|err| format!("couldn't run {}: {err}", self.command.display()))?;
        let took = started.elapsed();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if !output.status.success() {
            return Err(format!("{} {} failed:\n{stderr}", self.command.display(), self.args.join(" ")));
        }
        Ok((took, stdout, stderr))
    }
}

/// Runs each program `runs` times, taking turns so they share whatever the machine is doing,
/// and keeps each one's fastest run.
pub(crate) fn time_together(programs: &[Program], runs: usize) -> Result<Vec<Timing>, String> {
    let mut timings: Vec<Option<Timing>> = vec![None; programs.len()];
    for _ in 0..runs {
        for (program, timing) in programs.iter().zip(timings.iter_mut()) {
            let (took, output, notes) = program.run()?;
            match timing {
                Some(timing) if timing.best <= took => {}
                _ => *timing = Some(Timing { best: took, output, notes }),
            }
        }
    }
    Ok(timings.into_iter().flatten().collect())
}

/// Compiles a Rust twin the way a release build would, for this machine's CPU.
pub(crate) fn build_twin(source: &Path, out_dir: &Path) -> Result<PathBuf, String> {
    let name = source.file_stem().map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
    let binary = out_dir.join(name);
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc)
        .args(["-C", "opt-level=3", "-C", "target-cpu=native", "-o"])
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
