//! `fennec test`: checks a program, then runs its `test` blocks and reports each one. For a
//! kit, that's the tests in every file under `src`.
//!
//! A test that passed is run again only when something it depends on changed: the test
//! itself, or anything it can reach (see `fennec_build::test_keys`). The keys of the tests
//! that passed are kept in the cache in `~/.fennec`. `--all` runs every test, as do runs with
//! `CI` or `FENNEC_NO_CACHE` set. A test that failed always runs again.
//!
//! Tests run as native code on every core, reusing machine code from the cache. Property
//! tests, and any test the JIT can't compile yet, run in the interpreter.

use std::collections::{HashMap, HashSet};
use std::process::ExitCode;

use fennec_blake3::Hash;
use fennec_build::Cache;

pub(crate) fn test(path: &str, all: bool) -> ExitCode {
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
    let keyed = fennec_build::test_keys(&program.module);
    let keys: HashMap<String, Hash> = keyed.iter().cloned().collect();
    let results = Results::open(path, all);
    let unchanged: HashSet<&str> =
        keys.iter().filter(|(_, key)| results.passed.contains(*key)).map(|(name, _)| name.as_str()).collect();
    let wanted: Vec<String> = keyed
        .iter()
        .map(|(name, _)| name)
        .filter(|name| !unchanged.contains(name.as_str()))
        .cloned()
        .collect();
    let cache = results.machine_code();
    let cache = cache.as_ref().map(|(cache, program)| (cache, program.as_str()));
    let native = fennec_jit::run_tests(&program.module, &signatures, &wanted, cache);
    let mut outcomes = native.outcomes;
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
    let place = |name: &str| keyed.iter().position(|(test, _)| test == name);
    outcomes.sort_by_key(|outcome| place(&outcome.name));
    if outcomes.is_empty() && unchanged.is_empty() {
        println!("no tests in {path}");
        return ExitCode::SUCCESS;
    }
    let mut failed = 0;
    let mut passing: Vec<Hash> = unchanged.iter().filter_map(|name| keys.get(*name).copied()).collect();
    for outcome in &outcomes {
        let cases = if outcome.cases > 1 { format!(" ({} inputs)", outcome.cases) } else { String::new() };
        match &outcome.failure {
            None => {
                println!("  ✓ {}{cases}", outcome.name);
                passing.extend(keys.get(&outcome.name).copied());
            }
            Some(diagnostic) => {
                failed += 1;
                println!("  ✗ {}", outcome.name);
                eprint!("{}", render(diagnostic));
            }
        }
    }
    results.store(&passing);
    if !unchanged.is_empty() {
        let total = outcomes.len() + unchanged.len();
        println!("{total} tests: {} run, {} unchanged", outcomes.len(), unchanged.len());
    }
    let passed = outcomes.len() + unchanged.len() - failed;
    println!("{passed} passed, {failed} failed");
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
    passed: HashSet<Hash>,
}

impl Results {
    fn open(path: &str, all: bool) -> Self {
        let program =
            std::fs::canonicalize(path).map_or_else(|_| path.to_owned(), |path| path.display().to_string());
        let cache = fennec_build::fennec_home()
            .filter(|_| std::env::var_os("FENNEC_NO_CACHE").is_none())
            .map(|home| (Cache::new(&home), program));
        let reuse = !all && std::env::var_os("CI").is_none();
        let passed = match &cache {
            Some((cache, program)) if reuse => cache.passed_tests(program),
            _ => HashSet::new(),
        };
        Results { cache, passed }
    }

    /// Where machine code for this program's tests is kept: apart from its `main`'s, since
    /// tests reach other functions.
    fn machine_code(&self) -> Option<(Cache, String)> {
        self.cache.as_ref().map(|(cache, program)| (cache.clone(), format!("{program}#tests")))
    }

    fn store(&self, passing: &[Hash]) {
        if let Some((cache, program)) = &self.cache {
            // A cache that can't be written only means running these tests again next time.
            let _ = cache.store_passed_tests(program, passing);
        }
    }
}
