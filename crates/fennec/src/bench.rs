//! `fennec bench`: runs a program's `bench` blocks and says how each compares with the last run.
//!
//! Each block is built through LLVM into a program of its own, whose `main` runs the block and
//! says how long it took, measured around the block alone. That runs several times. Its
//! instructions are the whole run's minus those of a run that does all but the block, as the
//! benchmark suite counts them.

use std::fmt::Write as _;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use fennec_count::{Counter, Program, median};
use fennec_syntax::{Item, Module};

/// How many times each block runs. Its median run counts.
const RUNS: usize = 5;

/// Where the last run's measurements are kept. Its times are in nanoseconds, so it isn't where
/// those kept in microseconds were.
const TIMES: &str = "bench-nanos";

/// How many times each block's instructions are counted.
const COUNTS: usize = 3;

/// A block's measurements.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Measured {
    nanos: u64,
    /// None where nothing can count instructions.
    instructions: Option<u64>,
}

/// Keeps the program's benchmarks, or only the one called `only`, written as tests so they
/// check and compile like them. Its tests go, since a test may share a benchmark's name.
fn keep_benches(module: &mut Module, only: Option<&str>) {
    module.items.retain_mut(|item| match item {
        Item::Test(test) if test.bench && only.is_none_or(|only| only == test.name) => {
            test.bench = false;
            true
        }
        Item::Test(_) => false,
        _ => true,
    });
}

fn bench_names(module: &Module) -> Vec<String> {
    let tests = module.items.iter().filter_map(|item| match item {
        Item::Test(test) => Some(test.name.clone()),
        _ => None,
    });
    tests.collect()
}

pub(crate) fn bench(path: &str) -> ExitCode {
    let Some(mut program) = crate::load(path, fennec_kit::Entry::Everything) else {
        return ExitCode::FAILURE;
    };
    keep_benches(&mut program.module, None);
    if let Err(diagnostics) = fennec_check::check_and_resolve(&mut program.module) {
        eprint!("{}", program.render(&diagnostics));
        return ExitCode::FAILURE;
    }
    let names = bench_names(&program.module);
    if names.is_empty() {
        println!("no benches in {path}");
        return ExitCode::SUCCESS;
    }
    match measure_all(path, &names) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprint!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn measure_all(path: &str, names: &[String]) -> Result<(), String> {
    let cache = crate::run::cache();
    let previous = cache.as_ref().map_or_else(Vec::new, |cache| last_run(cache, path));
    let folder = crate::own_temp("bench");
    let counter = std::fs::create_dir_all(&folder).ok().and_then(|()| Counter::find(&folder));
    let measured = measure_each(path, names, (&folder, counter.as_ref()), &previous);
    let _ = std::fs::remove_dir_all(&folder);
    let now = measured?;
    if let Some(cache) = &cache {
        // A cache that can't be written only means nothing to compare with next time.
        let times = cache.write(TIMES, &crate::run::passed_name(path), encode(&now).as_bytes());
        fennec_build::kept("keep the benchmarks' times", times);
    }
    Ok(())
}

/// Measures each block, printing its line as soon as it's known.
fn measure_each(
    path: &str,
    names: &[String],
    (folder, counter): (&Path, Option<&Counter>),
    previous: &[(String, Measured)],
) -> Result<Vec<(String, Measured)>, String> {
    let mut now = Vec::with_capacity(names.len());
    for name in names {
        let executable = folder.join("bench");
        built(path, name, &executable)?;
        let measured = measure(&executable, counter)?;
        let before = previous.iter().find(|(old, _)| old == name).map(|(_, measured)| *measured);
        println!("{}", line(name, measured, before));
        now.push((name.clone(), measured));
    }
    Ok(now)
}

/// The executable for one block, run whole, or with `start only` doing all but the block.
fn child(executable: &Path, execute: bool) -> Program {
    let args = if execute { Vec::new() } else { vec!["start only".to_owned()] };
    Program { command: executable.to_path_buf(), args, stdin: None }
}

fn measure(executable: &Path, counter: Option<&Counter>) -> Result<Measured, String> {
    let whole = child(executable, true);
    let mut times = Vec::with_capacity(RUNS);
    for _ in 0..RUNS {
        let run = whole.run().map_err(|err| format!("{err}\n"))?;
        times.extend(run.stdout.trim().parse::<u64>());
    }
    let instructions = counter.and_then(|counter| {
        let all = median_count(counter, &whole)?;
        let start = median_count(counter, &child(executable, false))?;
        // A block whose work is smaller than how much starting varies can't be told apart.
        all.checked_sub(start).filter(|work| *work > 0)
    });
    Ok(Measured { nanos: median(&mut times, u64::midpoint).unwrap_or(0), instructions })
}

/// Builds the program for block `name` to `executable` through LLVM: the program with the block
/// as a function, and a `main` that runs it between two readings of the clock and prints the
/// nanoseconds between them. Given an argument, it does all but run the block.
fn built(path: &str, name: &str, executable: &Path) -> Result<(), String> {
    let program = crate::load(path, fennec_kit::Entry::Everything).ok_or_else(String::new)?;
    let mut program = program;
    keep_benches(&mut program.module, Some(name));
    let block = program.module.items.iter().find_map(|item| match item {
        Item::Test(test) if test.name == name => Some(test.clone()),
        _ => None,
    });
    let block = block.ok_or_else(|| format!("error: there's no bench called {name:?}\n"))?;
    program.module.items.retain(|item| match item {
        Item::Test(_) => false,
        Item::Function(def) => def.name != "main",
        _ => true,
    });
    let functions = crate::llvm_tests::timing_functions(&block)
        .ok_or_else(|| format!("error: the bench {name:?} can't be built natively yet\n"))?;
    program.module.items.extend(functions.into_iter().map(Item::Function));
    if !crate::run::check_to_build(&mut program) {
        return Err(String::new());
    }
    crate::aot::executable(&program, executable, false)
        .map(|_| ())
        .map_err(|message| format!("error: {message}\n"))
}

/// The middle of a few counts, since starting a process varies by more than a small block does.
fn median_count(counter: &Counter, program: &Program) -> Option<u64> {
    let mut counts = (0..COUNTS).map(|_| counter.count(program)).collect::<Option<Vec<u64>>>()?;
    median(&mut counts, u64::midpoint)
}

/// One block's line of the report.
fn line(name: &str, now: Measured, before: Option<Measured>) -> String {
    let mut line = format!("  {name}: {:.3} ms", Duration::from_nanos(now.nanos).as_secs_f64() * 1000.0);
    if let Some(count) = now.instructions {
        let _ = write!(line, ", {} instructions", grouped(count));
    }
    if let Some(change) = before.and_then(|before| change(now, before)) {
        let _ = write!(line, " ({change:+.1}% from the last run)");
    }
    line
}

/// How much more a block takes than last time, as a percentage: by instructions where both runs
/// counted them, since they barely move when the machine is busy, and otherwise by time.
fn change(now: Measured, before: Measured) -> Option<f64> {
    let (now, before) = match (now.instructions, before.instructions) {
        (Some(now), Some(before)) => (now, before),
        _ => (now.nanos, before.nanos),
    };
    #[expect(clippy::cast_precision_loss, reason = "a percentage doesn't need every digit of a count")]
    (before > 0).then(|| (now as f64 / before as f64 - 1.0) * 100.0)
}

/// `1234567` as `1,234,567`.
fn grouped(count: u64) -> String {
    let digits = count.to_string();
    let mut text = String::new();
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            text.push(',');
        }
        text.push(digit);
    }
    text
}

/// What's kept of a run: a line per block, with the name last, since it may hold anything.
fn encode(run: &[(String, Measured)]) -> String {
    let mut text = String::new();
    for (name, measured) in run {
        let count = measured.instructions.map_or_else(|| "-".to_owned(), |count| count.to_string());
        let _ = writeln!(text, "{} {count} {}", measured.nanos, name.replace('\n', " "));
    }
    text
}

fn decode(text: &str) -> Vec<(String, Measured)> {
    let read = |line: &str| {
        let mut fields = line.splitn(3, ' ');
        let nanos = fields.next()?.parse().ok()?;
        let instructions = fields.next()?.parse().ok();
        Some((fields.next()?.to_owned(), Measured { nanos, instructions }))
    };
    text.lines().filter_map(read).collect()
}

fn last_run(cache: &fennec_build::Cache, path: &str) -> Vec<(String, Measured)> {
    let bytes = cache.read(TIMES, &crate::run::passed_name(path)).unwrap_or_default();
    decode(&String::from_utf8_lossy(&bytes))
}

#[cfg(test)]
mod tests {
    use super::{Measured, change, decode, encode, grouped, keep_benches, line};

    fn measured(micros: u64, instructions: Option<u64>) -> Measured {
        Measured { nanos: micros * 1_000, instructions }
    }

    #[test]
    fn only_benchmarks_stay_written_as_tests() {
        let source = "bench \"b\":\n    assert 1 == 1\n\nbench \"c\":\n    assert 1 == 1\n\n\
                      test \"b\":\n    assert 1 == 2\n";
        let mut module = fennec_syntax::parse(source).unwrap();
        keep_benches(&mut module, Some("b"));
        assert_eq!(super::bench_names(&module), ["b"]);
        assert!(matches!(&module.items[0], fennec_syntax::Item::Test(test) if !test.bench));
    }

    #[test]
    fn a_change_is_by_instructions_where_both_runs_counted_them() {
        assert_eq!(change(measured(9, Some(110)), measured(1, Some(100))).map(f64::round), Some(10.0));
        assert_eq!(change(measured(150, None), measured(100, Some(5))), Some(50.0));
        assert_eq!(change(measured(150, Some(5)), measured(100, None)), Some(50.0));
        assert_eq!(change(measured(1, None), measured(0, None)), None);
    }

    #[test]
    fn a_line_says_what_was_measured_and_how_it_changed() {
        let now = measured(1_500, Some(1_234_567));
        assert_eq!(line("b", now, None), "  b: 1.500 ms, 1,234,567 instructions");
        let before = measured(1, Some(1_234_567));
        assert_eq!(
            line("b", now, Some(before)),
            "  b: 1.500 ms, 1,234,567 instructions (+0.0% from the last run)"
        );
        assert_eq!(line("b", measured(2_000, None), None), "  b: 2.000 ms");
    }

    #[test]
    fn counts_are_grouped_by_thousands() {
        assert_eq!(grouped(1), "1");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_000), "1,000");
        assert_eq!(grouped(1_234_567), "1,234,567");
    }

    #[test]
    fn a_run_reads_back() {
        let run =
            [("with spaces".to_owned(), measured(5, Some(7))), ("no count".to_owned(), measured(6, None))];
        assert_eq!(decode(&encode(&run)), run);
        assert_eq!(decode("garbage\n1\n"), []);
    }
}
