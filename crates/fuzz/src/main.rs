//! `fennec-fuzz`: fuzzing Fennec with no help from outside, repeatably.
//!
//! ```text
//! fennec-fuzz run [--seed N] [--seconds N] [--inputs N] [--found DIR] [target...]
//! fennec-fuzz one <target> <file>
//! fennec-fuzz show [llvm] <file>
//! fennec-fuzz worker <target> <seed> <first input> <input to stop at> <progress file>
//! ```
//!
//! Inputs run in a worker process, so a crash, a stack overflow or running out of memory can't
//! take the fuzzer down. The worker says which input it's on, and every input comes from the
//! seed and its number, so whatever it was doing can be made again. What's found is cut down to
//! the smallest input that still fails the same way, and kept in `found/<target>/`. The tests
//! run everything kept there, so a failure stays fixed.
//!
//! Running with no targets named runs every one but `llvm`, which needs `fennec` beside this
//! program and LLVM's tools, so it runs only when named.
#![expect(clippy::print_stdout, reason = "the fuzzer reports what it finds")]
#![expect(clippy::print_stderr, reason = "the fuzzer reports how it was misused")]

mod engines;
mod generate;
mod llvm;
mod minimize;
mod mutate;
mod supervise;
mod targets;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

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
        ["worker", name, seed, first, end, progress] => worker(name, seed, (first, end), Path::new(progress)),
        ["run", rest @ ..] => run(rest),
        _ => Err("usage: fennec-fuzz run [--seed N] [--seconds N] [--inputs N] [--found DIR] [target...]"
            .to_owned()),
    };
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

/// Prints the program an input to the `jit` target writes, or with `llvm` that target's, since
/// that input is only choices.
fn show(file: &Path, written: fn(&[u8]) -> String) -> Result<ExitCode, String> {
    let input = std::fs::read(file).map_err(|err| format!("{}: {err}", file.display()))?;
    print!("{}", written(&input));
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
    seconds: u64,
    inputs: u64,
    found: PathBuf,
    names: Vec<String>,
}

fn options(args: &[&str]) -> Result<Options, String> {
    let mut options = Options {
        seed: 1,
        seconds: 10,
        inputs: u64::MAX,
        found: repository().join("crates/fuzz/found"),
        names: Vec::new(),
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("`{arg}` needs a value"));
        match *arg {
            "--seed" => options.seed = value()?.parse().map_err(|_| "--seed needs a number")?,
            "--seconds" => options.seconds = value()?.parse().map_err(|_| "--seconds needs a number")?,
            "--inputs" => options.inputs = value()?.parse().map_err(|_| "--inputs needs a number")?,
            "--found" => options.found = PathBuf::from(value()?),
            name => options.names.push(name.to_owned()),
        }
    }
    Ok(options)
}

/// Fuzzes each target for its share of the time, and fails if anything new turned up.
fn run(args: &[&str]) -> Result<ExitCode, String> {
    let options = options(args)?;
    let everything = targets::TARGETS.iter().chain(std::iter::once(&targets::UTF8));
    let chosen: Vec<&Target> = if options.names.is_empty() {
        everything.collect()
    } else {
        options.names.iter().map(|name| target_named(name)).collect::<Result<_, _>>()?
    };
    if chosen.iter().any(|target| target.name == targets::LLVM.name) {
        llvm::ready()?;
    }
    let fennec_fuzz = std::env::current_exe().map_err(|err| err.to_string())?;
    let mut found_any = false;
    for target in chosen {
        let run = supervise::Run { fennec_fuzz: &fennec_fuzz, target, seed: options.seed };
        let outcome = run.until(options.seconds, options.inputs)?;
        println!("{}: {} inputs, {} failures", target.name, outcome.inputs, outcome.failures.len());
        for (failure, number) in outcome.failures {
            found_any = true;
            keep(&fennec_fuzz, target, &options, failure, number)?;
        }
    }
    Ok(if found_any { ExitCode::FAILURE } else { ExitCode::SUCCESS })
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
