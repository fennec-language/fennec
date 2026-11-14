//! `fennec-fuzz`: fuzzing Fennec with no help from outside, repeatably.
//!
//! ```text
//! fennec-fuzz run [--seed N] [--seconds N] [--inputs N] [--stuck SECONDS] [--found DIR] [target...]
//! fennec-fuzz one <target> <file>
//! fennec-fuzz show [llvm] <file>
//! fennec-fuzz worker <target> <seed> <first input> <input to stop at> <progress file>
//! fennec-fuzz input <target> <seed> <input> <file>
//! ```
//!
//! Inputs run in a worker process, so a crash, a stack overflow or running out of memory can't
//! take the fuzzer down. The worker says which input it's on, and every input comes from the
//! seed and its number, so whatever it was doing can be made again. What's found is cut down to
//! the smallest input that still fails the same way, and kept in `found/<target>/`. The tests
//! run everything kept there, so a failure stays fixed.
//!
//! `llvm` and the targets in `targets::NATIVE` build what they run natively, so they need `fennec`
//! beside this program and LLVM's tools. Those that read outside input, `json-rules`, `toml-rules`,
//! `pack`, `csv`, `url`, `http-server`, `http-client` and `postgres`, hold what they read to the
//! format's own rules, each in its own module, not only to not crashing. Running with no targets
//! named runs every one but `llvm`, which builds every input twice and runs only when named, and
//! leaves out the other native ones, saying so, where nothing can be built. `programs` runs only
//! when named too: random, well-typed programs built both ways and checked against a reference
//! evaluator written here, which `fennec-fuzz show programs <file>` writes out.
#![expect(clippy::print_stdout, reason = "the fuzzer reports what it finds")]
#![expect(clippy::print_stderr, reason = "the fuzzer reports how it was misused")]

mod csv_rules;
mod engines;
mod generate;
mod http_rules;
mod json_rules;
mod llvm;
mod minimize;
mod mutate;
mod pack_rules;
mod parked;
mod postgres_rules;
mod programs;
mod supervise;
mod targets;
mod toml_rules;
mod url_rules;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use supervise::Failure;
use targets::Target;

/// The stack inputs run on, as much as `fennec` itself gives programs.
const STACK_BYTES: usize = 1 << 30;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let code = match args.as_slice() {
        ["one", name, file] => one(name, Path::new(file)),
        ["show", file] => show(Path::new(file), generate::program),
        ["show", "llvm", file] => show(Path::new(file), generate::printing_plainly),
        ["show", "programs", file] => show(Path::new(file), programs::written),
        ["show", "reader", name] => llvm::reader_source(name).map_or_else(
            || Err(format!("there's no reader for `{name}`; there are {}", llvm::reader_names().join(", "))),
            |source| {
                print!("{source}");
                Ok(ExitCode::SUCCESS)
            },
        ),
        ["worker", name, seed, first, end, progress] => worker(name, seed, (first, end), Path::new(progress)),
        ["input", name, seed, number, file] => input(name, (seed, number), Path::new(file)),
        ["run", rest @ ..] => run(rest),
        _ => Err("usage: fennec-fuzz run [--seed N] [--seconds N] [--inputs N] [--stuck SECONDS] [--found DIR] [target...]"
            .to_owned()),
    };
    llvm::forget(std::process::id());
    match code {
        Ok(code) => code,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::from(2)
        }
    }
}

/// The repository's top folder, for inputs to start from.
fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn target_named(name: &str) -> Result<&'static Target, String> {
    targets::find(name).ok_or_else(|| format!("there's no target called `{name}`"))
}

/// Runs a function on a big stack. If it panics, the panic ends the process.
fn on_big_stack(work: impl FnOnce() -> Result<(), String> + Send + 'static) -> ExitCode {
    let worker = std::thread::Builder::new().stack_size(STACK_BYTES).spawn(work);
    match worker.map(std::thread::JoinHandle::join) {
        Ok(Ok(Ok(()))) => ExitCode::SUCCESS,
        Ok(Ok(Err(wrong))) => {
            eprintln!("wrong: {wrong}");
            ExitCode::from(supervise::WRONG)
        }
        _ => ExitCode::from(supervise::PANICKED),
    }
}

/// Runs one input, for cutting a failure down and for keeping it fixed.
fn one(name: &str, file: &Path) -> Result<ExitCode, String> {
    let target = target_named(name)?;
    let input = std::fs::read(file).map_err(|err| format!("{}: {err}", file.display()))?;
    Ok(on_big_stack(move || (target.run)(&input)))
}

/// Prints the program an input writes, or with `llvm` the `llvm` target's, since
/// that input is only choices.
fn show(file: &Path, written: fn(&[u8]) -> String) -> Result<ExitCode, String> {
    let input = std::fs::read(file).map_err(|err| format!("{}: {err}", file.display()))?;
    print!("{}", written(&input));
    Ok(ExitCode::SUCCESS)
}

/// Writes input `number` of a run with `seed`, as it is before it's cut down, to look at it.
fn input(name: &str, (seed, number): (&str, &str), file: &Path) -> Result<ExitCode, String> {
    let target = target_named(name)?;
    let parsed = |text: &str| text.parse::<u64>().map_err(|_| format!("`{text}` isn't a number"));
    let corpus = target.corpus(&repository());
    let bytes = mutate::input(&corpus, target.tokens, parsed(seed)?, parsed(number)?);
    std::fs::write(file, bytes).map_err(|err| format!("{}: {err}", file.display()))?;
    Ok(ExitCode::SUCCESS)
}

/// Runs the inputs from `first` up to `end`, noting each one's number first.
fn worker(name: &str, seed: &str, (first, end): (&str, &str), progress: &Path) -> Result<ExitCode, String> {
    let target = target_named(name)?;
    let number = |text: &str| text.parse::<u64>().map_err(|_| format!("`{text}` isn't a number"));
    let (seed, first, end) = (number(seed)?, number(first)?, number(end)?);
    let corpus = target.corpus(&repository());
    let mut progress = supervise::Progress::create(progress)?;
    Ok(on_big_stack(move || {
        for number in first..end {
            progress.note(number);
            (target.run)(&mutate::input(&corpus, target.tokens, seed, number))?;
        }
        Ok(())
    }))
}

/// What `run` was asked to do.
struct Options {
    seed: u64,
    /// How long each target runs. A run asked for a number of inputs and no time runs them all,
    /// however long the machine takes, so it finds the same thing on a busy machine.
    seconds: Option<u64>,
    inputs: u64,
    /// How much CPU time one input may take before it counts as stuck, in place of each target's
    /// own.
    stuck: Option<u64>,
    found: PathBuf,
    names: Vec<String>,
}

fn options(args: &[&str]) -> Result<Options, String> {
    let mut options = Options {
        seed: 1,
        seconds: None,
        inputs: u64::MAX,
        stuck: None,
        found: repository().join("crates/fuzz/found"),
        names: Vec::new(),
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("`{arg}` needs a value"));
        match *arg {
            "--seed" => options.seed = value()?.parse().map_err(|_| "--seed needs a number")?,
            "--seconds" => options.seconds = Some(value()?.parse().map_err(|_| "--seconds needs a number")?),
            "--inputs" => options.inputs = value()?.parse().map_err(|_| "--inputs needs a number")?,
            "--stuck" => options.stuck = Some(value()?.parse().map_err(|_| "--stuck needs a number")?),
            "--found" => options.found = PathBuf::from(value()?),
            name => options.names.push(name.to_owned()),
        }
    }
    if options.seconds.is_none() && options.inputs == u64::MAX {
        options.seconds = Some(10);
    }
    Ok(options)
}

/// Fuzzes each target for its share of the time, and fails if anything new turned up.
fn run(args: &[&str]) -> Result<ExitCode, String> {
    let options = options(args)?;
    let everything = targets::TARGETS.iter().chain(std::iter::once(&targets::UTF8));
    let chosen: Vec<&Target> = if options.names.is_empty() {
        let built = llvm::ready().is_ok();
        if !built {
            println!("{}: left out, since nothing can be built natively here", targets::NATIVE.join(", "));
        }
        everything.filter(|target| built || !targets::NATIVE.contains(&target.name)).collect()
    } else {
        options.names.iter().map(|name| target_named(name)).collect::<Result<_, _>>()?
    };
    let native = |target: &&Target| {
        [targets::LLVM.name, targets::RANDOM_PROGRAMS.name].contains(&target.name)
            || targets::NATIVE.contains(&target.name)
    };
    if chosen.iter().any(native) {
        llvm::ready()?;
    }
    let fennec_fuzz = std::env::current_exe().map_err(|err| err.to_string())?;
    let mut found_any = false;
    for target in chosen {
        let stuck = options.stuck.map_or(target.stuck, Duration::from_secs);
        let run = supervise::Run { fennec_fuzz: &fennec_fuzz, target, seed: options.seed, stuck };
        let outcome = run.until(options.seconds, options.inputs)?;
        println!("{}: {} inputs, {} failures", target.name, outcome.inputs, outcome.failures.len());
        for failed in outcome.failures {
            found_any = true;
            println!("{} {}: {}", target.name, failed.failure, how_many(&failed.inputs, outcome.inputs));
            if let Some(&first) = failed.inputs.first() {
                keep(&fennec_fuzz, target, &options, failed.failure, first)?;
            }
        }
    }
    Ok(if found_any { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

/// How many of the inputs run failed one way, and the first few: `37 of 400 inputs (2, 5, 9, …)`.
fn how_many(failing: &[u64], run: u64) -> String {
    const SHOWN: usize = 8;
    let mut first: Vec<String> = failing.iter().take(SHOWN).map(u64::to_string).collect();
    if failing.len() > SHOWN {
        first.push("…".to_owned());
    }
    format!("{} of {run} inputs ({})", failing.len(), first.join(", "))
}

/// Cuts a failure's input down and keeps it, named by its contents.
fn keep(
    fennec_fuzz: &Path,
    target: &Target,
    options: &Options,
    failure: Failure,
    number: u64,
) -> Result<(), String> {
    let corpus = target.corpus(&repository());
    let input = mutate::input(&corpus, target.tokens, options.seed, number);
    let small = minimize::minimize(fennec_fuzz, target.name, input, failure);
    let folder = options.found.join(target.name);
    std::fs::create_dir_all(&folder).map_err(|err| err.to_string())?;
    let name = fennec_blake3::hash(&small).to_hex();
    let name = name.get(..16).unwrap_or(&name).to_owned();
    let path = folder.join(name);
    std::fs::write(&path, &small).map_err(|err| err.to_string())?;
    println!("{} {failure}: seed {}, input {number}, kept in {}", target.name, options.seed, path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::how_many;

    /// A kind of failure says how many inputs failed that way, of how many run, and the first few.
    #[test]
    fn how_many_failed_says_the_count_and_the_first_few() {
        assert_eq!(how_many(&[2], 40), "1 of 40 inputs (2)");
        let many: Vec<u64> = (1..=10).collect();
        assert_eq!(how_many(&many, 400), "10 of 400 inputs (1, 2, 3, 4, 5, 6, 7, 8, …)");
    }
}
