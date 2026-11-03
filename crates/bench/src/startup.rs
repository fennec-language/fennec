//! What starting a program costs in each language, from a hello-world in each one beside the
//! kernels, `bench/startup/empty.*`, built the same way as the kernels and their twins. It's
//! shown beside the kernels' times, since a short kernel's whole run is mostly its startup,
//! Java's above all.

use std::path::Path;

use crate::measure::{Overflow, Program};
use crate::other_twins::Language;

/// Each language's startup, as the best of some runs of its hello-world, in seconds.
pub(crate) struct Startups {
    pub(crate) fennec: Option<f64>,
    pub(crate) rust: Option<f64>,
    /// In the order of `Language::ALL`.
    pub(crate) others: Vec<Option<f64>>,
}

/// The best of `runs` runs of a program that prints `hello`, or None if it doesn't.
fn best(program: &Program, runs: usize) -> Option<f64> {
    let mut best: Option<f64> = None;
    for _ in 0..runs {
        let run = program.run().ok()?;
        if run.stdout.trim() != "hello" {
            return None;
        }
        let took = run.took.as_secs_f64();
        best = Some(best.map_or(took, |best: f64| best.min(took)));
    }
    best
}

/// Builds and times each language's hello-world from `dir`. A language that isn't installed,
/// or whose hello-world doesn't build, has no startup.
pub(crate) fn measure(
    fennec: &Path,
    dir: &Path,
    out_dir: &Path,
    installed: &[Option<Language>],
    runs: usize,
) -> Startups {
    let source = dir.join("empty.fen");
    let fennec_startup = crate::llvm_suite::built(fennec, &source.to_string_lossy(), out_dir, "empty")
        .ok()
        .and_then(|executable| best(&Program { command: executable, args: Vec::new(), stdin: None }, runs));
    let rust = crate::measure::build_twin(&dir.join("empty.rs"), out_dir, Overflow::Checked)
        .ok()
        .and_then(|command| best(&Program { command, args: Vec::new(), stdin: None }, runs));
    let others = installed
        .iter()
        .map(|language| {
            let language = (*language)?;
            let program = language.build(&dir.join(format!("empty.{}", language.key())), out_dir).ok()?;
            best(&program, runs)
        })
        .collect();
    Startups { fennec: fennec_startup, rust, others }
}
