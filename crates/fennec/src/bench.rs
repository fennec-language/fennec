//! `fennec bench`: runs a program's `bench` blocks and says how each compares with the last run.
//!
//! Each block runs in a process of its own, several times, with its code compiled exactly as a
//! program's is. Its time is measured around the block alone. Its instructions are the whole
//! run's minus those of a run that does all but the block, as the benchmark suite counts them.

use std::fmt::Write as _;
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use fennec_count::{Counter, Program, median};
use fennec_syntax::{Item, Module};

/// How many times each block runs. Its median run counts.
const RUNS: usize = 5;

/// How many times each block's instructions are counted.
const COUNTS: usize = 3;

/// A block's measurements.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Measured {
    micros: u64,
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
        eprint!("{}", program.sources.render_all(&diagnostics));
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
    let fennec = std::env::current_exe().map_err(|err| format!("error: can't find fennec: {err}\n"))?;
    let cache = crate::run::cache();
    let previous = cache.as_ref().map_or_else(Vec::new, |cache| last_run(cache, path));
    let folder = std::env::temp_dir().join(format!("fennec-bench-{}", std::process::id()));
    let counter = std::fs::create_dir_all(&folder).ok().and_then(|()| Counter::find(&folder));
    let measured = measure_each(&fennec, path, names, counter.as_ref(), &previous);
    let _ = std::fs::remove_dir_all(&folder);
    let now = measured?;
    if let Some(cache) = &cache {
        // A cache that can't be written only means nothing to compare with next time.
        let _ = cache.write("bench", &crate::run::passed_name(path), encode(&now).as_bytes());
    }
    Ok(())
}

/// Measures each block, printing its line as soon as it's known.
fn measure_each(
    fennec: &Path,
    path: &str,
    names: &[String],
    counter: Option<&Counter>,
    previous: &[(String, Measured)],
) -> Result<Vec<(String, Measured)>, String> {
    let mut now = Vec::with_capacity(names.len());
    for name in names {
        let measured = measure(fennec, path, name, counter)?;
        let before = previous.iter().find(|(old, _)| old == name).map(|(_, measured)| *measured);
        println!("{}", line(name, measured, before));
        now.push((name.clone(), measured));
    }
    Ok(now)
}

fn child(fennec: &Path, path: &str, name: &str, execute: bool) -> Program {
    let mut args = vec!["dev".to_owned(), "bench".to_owned(), path.to_owned(), name.to_owned()];
    if !execute {
        args.push("--start-only".to_owned());
    }
    Program { command: fennec.to_path_buf(), args, stdin: None }
}

fn measure(fennec: &Path, path: &str, name: &str, counter: Option<&Counter>) -> Result<Measured, String> {
    let whole = child(fennec, path, name, true);
    let mut times = Vec::with_capacity(RUNS);
    for _ in 0..RUNS {
        let run = whole.run().map_err(|err| format!("{err}\n"))?;
        times.extend(run.stdout.trim().parse::<u64>());
    }
    let instructions = counter.and_then(|counter| {
        let all = median_count(counter, &whole)?;
        let start = median_count(counter, &child(fennec, path, name, false))?;
        // A block whose work is smaller than how much starting varies can't be told apart.
        all.checked_sub(start).filter(|work| *work > 0)
    });
    Ok(Measured { micros: median(&mut times, u64::midpoint).unwrap_or(0), instructions })
}

/// The middle of a few counts, since starting a process varies by more than a small block does.
fn median_count(counter: &Counter, program: &Program) -> Option<u64> {
    let mut counts = (0..COUNTS).map(|_| counter.count(program)).collect::<Option<Vec<u64>>>()?;
    median(&mut counts, u64::midpoint)
}

/// `fennec dev bench <path> <name>`: runs one block and prints how many microseconds it took,
/// or with `execute` false, does all but run it.
pub(crate) fn one(path: &str, name: &str, execute: bool) -> ExitCode {
    let Some(mut program) = crate::load(path, fennec_kit::Entry::Everything) else {
        return ExitCode::FAILURE;
    };
    keep_benches(&mut program.module, Some(name));
    let signatures = match fennec_check::check_and_resolve(&mut program.module) {
        Ok(signatures) => signatures,
        Err(diagnostics) => {
            eprint!("{}", program.sources.render_all(&diagnostics));
            return ExitCode::FAILURE;
        }
    };
    let native = crate::run::cache().and_then(|cache| {
        let name_in_cache = format!("{}#bench", crate::run::absolute(path));
        fennec_jit::run_bench(&program.module, &signatures, name, (&cache, &name_in_cache), execute)
    });
    let ran = match native {
        Some(Ok(ran)) => ran,
        Some(Err(diagnostic)) => Some((Duration::ZERO, Some(diagnostic))),
        None => match interpreted(&program.module, name, execute) {
            Ok(ran) => ran,
            Err(diagnostic) => Some((Duration::ZERO, Some(diagnostic))),
        },
    };
    match ran {
        Some((_, Some(diagnostic))) => {
            eprint!("{}", program.sources.render(&diagnostic));
            ExitCode::FAILURE
        }
        Some((took, None)) => {
            println!("{}", took.as_micros());
            ExitCode::SUCCESS
        }
        None => ExitCode::SUCCESS,
    }
}

/// A block the JIT can't compile yet, or any block with no cache to compile through.
fn interpreted(
    module: &Module,
    name: &str,
    execute: bool,
) -> Result<fennec_jit::BenchRun, fennec_syntax::Diagnostic> {
    let mut out = std::io::sink();
    let mut interp = fennec_interp::Interp::new(&mut out)?;
    interp.load(module)?;
    if !execute {
        return Ok(None);
    }
    let started = Instant::now();
    let outcomes = interp.run_tests_where(&|test| test == name);
    let took = started.elapsed();
    let failure = outcomes.into_iter().find_map(|outcome| outcome.failure);
    Ok(Some((took, failure)))
}

/// One block's line of the report.
fn line(name: &str, now: Measured, before: Option<Measured>) -> String {
    let mut line = format!("  {name}: {:.3} ms", Duration::from_micros(now.micros).as_secs_f64() * 1000.0);
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
        _ => (now.micros, before.micros),
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
        let _ = writeln!(text, "{} {count} {}", measured.micros, name.replace('\n', " "));
    }
    text
}

fn decode(text: &str) -> Vec<(String, Measured)> {
    let read = |line: &str| {
        let mut fields = line.splitn(3, ' ');
        let micros = fields.next()?.parse().ok()?;
        let instructions = fields.next()?.parse().ok();
        Some((fields.next()?.to_owned(), Measured { micros, instructions }))
    };
    text.lines().filter_map(read).collect()
}

fn last_run(cache: &fennec_build::Cache, path: &str) -> Vec<(String, Measured)> {
    let bytes = cache.read("bench", &crate::run::passed_name(path)).unwrap_or_default();
    decode(&String::from_utf8_lossy(&bytes))
}

#[cfg(test)]
mod tests {
    use super::{Measured, change, decode, encode, grouped, keep_benches, line};

    fn measured(micros: u64, instructions: Option<u64>) -> Measured {
        Measured { micros, instructions }
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
