//! `fennec-bench`: the benchmark suite. Each kernel in `bench/kernels/` is a Fennec program
//! with a Rust twin that does the same work. Both run on the same machine, taking turns, and
//! must print the same thing. The table shows Fennec's speed against Rust and against the
//! previous commit's `fennec`, and the run fails if a kernel got slower than before: by its
//! median time and, where `perf` counts them, by its instructions too.
//!
//! ```text
//! fennec-bench [--fennec PATH] [--previous PATH] [--runs N] [--kernels DIR]
//! ```
#![expect(clippy::print_stdout, reason = "the suite prints its table")]
#![expect(clippy::print_stderr, reason = "the suite reports failures")]

mod measure;
mod report;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use measure::{Program, build_twin, count_with_valgrind, perf_report, time_together, valgrind_available};
use report::Result;

struct Options {
    fennec: PathBuf,
    previous: Option<PathBuf>,
    runs: usize,
    kernels: PathBuf,
}

fn main() -> ExitCode {
    let options = match options(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::from(2);
        }
    };
    match suite(&options) {
        Ok((results, mut failures)) => {
            let table = report::table(&results);
            print!("{table}");
            if let Some(summary) = std::env::var_os("GITHUB_STEP_SUMMARY") {
                let text = format!("## Benchmarks\n\n{table}");
                if let Err(err) = append(Path::new(&summary), &text) {
                    failures.push(format!("couldn't write the summary: {err}"));
                }
            }
            for failure in &failures {
                eprintln!("error: {failure}");
            }
            if failures.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
        }
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn append(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    std::fs::OpenOptions::new().create(true).append(true).open(path)?.write_all(text.as_bytes())
}

fn options(mut args: impl Iterator<Item = String>) -> std::result::Result<Options, String> {
    let mut options = Options {
        fennec: PathBuf::from("target/release/fennec"),
        previous: None,
        runs: 7,
        kernels: PathBuf::from("bench/kernels"),
    };
    while let Some(flag) = args.next() {
        let value = args.next().ok_or_else(|| format!("`{flag}` needs a value"))?;
        match flag.as_str() {
            "--fennec" => options.fennec = PathBuf::from(value),
            "--previous" => options.previous = Some(PathBuf::from(value)),
            "--kernels" => options.kernels = PathBuf::from(value),
            "--runs" => {
                options.runs =
                    value.parse().map_err(|_| format!("`--runs` needs a number, not `{value}`"))?;
            }
            _ => return Err(format!("unknown option `{flag}`")),
        }
    }
    Ok(options)
}

/// Runs every kernel. Returns each one's result and every failure.
fn suite(options: &Options) -> std::result::Result<(Vec<Result>, Vec<String>), String> {
    let out_dir = PathBuf::from("target/bench");
    std::fs::create_dir_all(&out_dir).map_err(|err| format!("couldn't make {}: {err}", out_dir.display()))?;
    let mut kernels: Vec<PathBuf> = std::fs::read_dir(&options.kernels)
        .map_err(|err| format!("couldn't read {}: {err}", options.kernels.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "fen"))
        .collect();
    kernels.sort();
    let count = perf_report(&out_dir);
    let valgrind = count.is_none() && valgrind_available();
    eprintln!(
        "{}",
        match (&count, valgrind) {
            (Some(_), _) => "counting instructions with perf",
            (None, true) => "counting instructions with Valgrind to confirm slowdowns",
            (None, false) =>
                "nothing can count instructions here, so slowdowns are measured again before they fail",
        }
    );
    let mut results = Vec::new();
    let mut failures = Vec::new();
    for kernel in kernels {
        let name = kernel.file_stem().map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
        eprintln!("running {name}");
        let mut result = run_kernel(&kernel, &out_dir, options.runs, options, count.as_deref());
        if let Ok(slower) = &mut result
            && slower.failure().is_some()
            && slower.instructions.is_none()
        {
            if valgrind {
                // A slowdown with no instruction count is confirmed by counting both
                // versions' instructions, once each: the count doesn't wobble like time does.
                eprintln!("counting {name}'s instructions");
                let fennec =
                    |command: &Path| Program { command: command.to_path_buf(), args: kernel_args(&kernel) };
                slower.instructions = count_with_valgrind(&fennec(&options.fennec));
                slower.previous_instructions =
                    options.previous.as_deref().and_then(|previous| count_with_valgrind(&fennec(previous)));
            } else {
                // Otherwise it's measured again, three times as long, before it fails.
                eprintln!("measuring {name} again");
                result = run_kernel(&kernel, &out_dir, options.runs * 3, options, count.as_deref());
            }
        }
        match result {
            Ok(result) => {
                failures.extend(result.failure());
                results.push(result);
            }
            Err(message) => failures.push(format!("{name}: {message}")),
        }
    }
    Ok((results, failures))
}

/// Times one kernel against its twin, and against the previous `fennec` if there's one that
/// can still run it.
fn run_kernel(
    kernel: &Path,
    out_dir: &Path,
    runs: usize,
    options: &Options,
    count: Option<&Path>,
) -> std::result::Result<Result, String> {
    let name = kernel.file_stem().map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
    let twin = build_twin(&kernel.with_extension("rs"), out_dir)?;
    let fennec = |command: &Path| Program { command: command.to_path_buf(), args: kernel_args(kernel) };
    let mut programs = vec![Program { command: twin, args: Vec::new() }, fennec(&options.fennec)];
    if let Some(previous) = &options.previous {
        // A previous `fennec` that can't run a new kernel just isn't compared on it.
        if time_together(&[fennec(previous)], 1, None).is_ok() {
            programs.push(fennec(previous));
        }
    }
    let timings = time_together(&programs, runs, count)?;
    let [twin, ours, others @ ..] = timings.as_slice() else { return Err("nothing was timed".to_owned()) };
    if ours.output != twin.output {
        return Err(format!("Fennec printed\n{}but Rust printed\n{}", ours.output, twin.output));
    }
    let previous = others.first().filter(|previous| previous.output == twin.output);
    Ok(Result {
        kernel: name,
        rust: twin.median,
        fennec: ours.median,
        previous: previous.map(|previous| previous.median),
        instructions: ours.instructions,
        previous_instructions: previous.and_then(|previous| previous.instructions),
        fell_back: ours.notes.contains("using the interpreter"),
    })
}

/// How `fennec` runs a kernel: natively, with the JIT.
fn kernel_args(kernel: &Path) -> Vec<String> {
    vec!["run".to_owned(), "--jit".to_owned(), kernel.to_string_lossy().into_owned()]
}
