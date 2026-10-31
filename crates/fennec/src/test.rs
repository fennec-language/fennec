//! `fennec test`: checks a program, then runs its `test` blocks. For a kit, that's the tests in
//! every file under `src`. It prints each failure and a line of totals, and lists the tests
//! that pass only for `--verbose`, so that passing tests cost an agent nothing to read.
//!
//! A test that passed is run again only when something it depends on changed: the test
//! itself, the functions it ran and what those rely on (see `fennec_build::TestGraph`). What
//! each test that passed depends on is kept in the cache in `~/.fennec`, and CI reuses it like
//! anywhere else. `--all` runs every test, as do runs with `FENNEC_NO_CACHE` set. A test that
//! failed always runs again, and so does every test after Fennec itself changes.
//!
//! Tests run as native code on every core, reusing machine code from the cache. Property
//! tests, and any test the JIT can't compile yet, run in the interpreter.

use std::collections::HashMap;
use std::process::ExitCode;

use fennec_build::{Cache, TestGraph, TestRecord};

/// How `fennec test` was asked to run.
#[derive(Clone, Copy, Default)]
pub(crate) struct Options {
    /// Run every test, not only what a change affects.
    all: bool,
    /// List the tests that pass too.
    verbose: bool,
}

impl Options {
    /// The path and options in what follows `fennec test`: any order, and at most one path.
    pub(crate) fn of(args: &[&str]) -> Option<(String, Self)> {
        let mut options = Options::default();
        let mut path = None;
        for arg in args {
            match *arg {
                "--all" => options.all = true,
                "--verbose" | "-v" => options.verbose = true,
                flag if flag.starts_with("--") => return None,
                written if path.is_none() => path = Some(written),
                _ => return None,
            }
        }
        Some((path.unwrap_or(".").to_owned(), options))
    }
}

pub(crate) fn test(path: &str, options: Options) -> ExitCode {
    let Some(mut program) = crate::load(path, fennec_kit::Entry::Everything) else {
        return ExitCode::FAILURE;
    };
    let signatures = match fennec_check::check_and_resolve(&mut program.module) {
        Ok(signatures) => signatures,
        Err(diagnostics) => {
            eprint!("{}", program.sources.render_all(&diagnostics));
            return ExitCode::FAILURE;
        }
    };
    let render = |diagnostic: &fennec_syntax::Diagnostic| program.sources.render(diagnostic);
    let results = Results::open(path, options.all);
    let graph = TestGraph::of(&program.module, env!("FENNEC_BUILD"));
    let unchanged: Vec<&TestRecord> = graph
        .tests()
        .filter_map(|name| results.passed.get(name))
        .filter(|record| graph.key(&record.test, record.ran.as_deref()) == Some(record.key))
        .collect();
    let is_unchanged = |name: &str| unchanged.iter().any(|record| record.test == name);
    let wanted: Vec<String> = graph.tests().filter(|name| !is_unchanged(name)).map(str::to_owned).collect();
    let cache = results.machine_code();
    let cache = cache.as_ref().map(|(cache, program)| (cache, program.as_str()));
    let mut native = fennec_jit::run_tests(&program.module, &signatures, &wanted, cache);
    let mut outcomes = std::mem::take(&mut native.outcomes);
    if !native.left.is_empty() {
        match interpreted(&program.module, &native.left) {
            Ok(more) => outcomes.extend(more),
            Err(diagnostic) => {
                eprint!("{}", render(&diagnostic));
                return ExitCode::FAILURE;
            }
        }
    }
    // Workers finish in any order, and a report reads in the order the tests are written.
    let place = |name: &str| graph.tests().position(|test| test == name);
    outcomes.sort_by_key(|outcome| place(&outcome.name));
    if outcomes.is_empty() && unchanged.is_empty() {
        println!("no tests in {path}");
        return ExitCode::SUCCESS;
    }
    let mut failed = 0;
    let mut passing: Vec<TestRecord> = unchanged.iter().map(|record| (*record).clone()).collect();
    for outcome in &outcomes {
        let cases = if outcome.cases > 1 { format!(" ({} inputs)", outcome.cases) } else { String::new() };
        match &outcome.failure {
            None => {
                if options.verbose {
                    println!("  ✓ {}{cases}", outcome.name);
                }
                let ran = native.ran.remove(&outcome.name);
                passing.extend(graph.key(&outcome.name, ran.as_deref()).map(|key| TestRecord {
                    test: outcome.name.clone(),
                    key,
                    ran,
                }));
            }
            Some(diagnostic) => {
                failed += 1;
                println!("  ✗ {}", outcome.name);
                eprint!("{}", render(diagnostic));
            }
        }
    }
    results.store(&passing);
    let passed = outcomes.len() + unchanged.len() - failed;
    if unchanged.is_empty() {
        println!("{passed} passed, {failed} failed");
    } else {
        println!("{passed} passed, {failed} failed ({} run, {} unchanged)", outcomes.len(), unchanged.len());
    }
    if failed == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// Runs the tests in `names` in the interpreter.
fn interpreted(
    module: &fennec_syntax::Module,
    names: &[String],
) -> Result<Vec<fennec_interp::TestOutcome>, fennec_syntax::Diagnostic> {
    let mut out = std::io::sink();
    let mut interp = fennec_interp::Interp::new(&mut out)?;
    interp.load(module)?;
    Ok(interp.run_tests_where(&|name| names.iter().any(|left| left == name)))
}

/// Which tests of a program passed last time, and where to keep this run's.
struct Results {
    cache: Option<(Cache, String)>,
    /// Empty when every test runs.
    passed: HashMap<String, TestRecord>,
}

impl Results {
    fn open(path: &str, all: bool) -> Self {
        let program =
            std::fs::canonicalize(path).map_or_else(|_| path.to_owned(), |path| path.display().to_string());
        let cache = fennec_build::fennec_home()
            .filter(|_| std::env::var_os("FENNEC_NO_CACHE").is_none())
            .map(|home| (Cache::new(&home), program));
        let passed = match &cache {
            Some((cache, program)) if !all => cache.test_records(program),
            _ => HashMap::new(),
        };
        Results { cache, passed }
    }

    /// Where machine code for this program's tests is kept: apart from its `main`'s, since
    /// tests reach other functions.
    fn machine_code(&self) -> Option<(Cache, String)> {
        self.cache.as_ref().map(|(cache, program)| (cache.clone(), format!("{program}#tests")))
    }

    fn store(&self, passing: &[TestRecord]) {
        if let Some((cache, program)) = &self.cache {
            // A cache that can't be written only means running these tests again next time.
            let _ = cache.store_test_records(program, passing);
        }
    }
}
