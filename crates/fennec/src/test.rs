//! `fennec test`: checks a program, then runs its `test` blocks and reports each one. For a
//! kit, that's the tests in every file under `src`.
//!
//! A test that passed is run again only when something it depends on changed: the test
//! itself, or anything it can reach (see `fennec_build::test_keys`). The keys of the tests
//! that passed are kept in the cache in `~/.fennec`. `--all` runs every test, as do runs with
//! `CI` or `FENNEC_NO_CACHE` set. A test that failed always runs again.

use std::collections::{HashMap, HashSet};
use std::process::ExitCode;

use fennec_blake3::Hash;
use fennec_build::Cache;

pub(crate) fn test(path: &str, all: bool) -> ExitCode {
    let Some(mut program) = crate::load(path, fennec_kit::Entry::Everything) else {
        return ExitCode::FAILURE;
    };
    if !crate::run::check(&mut program) {
        return ExitCode::FAILURE;
    }
    let render = |diagnostic: &fennec_syntax::Diagnostic| program.sources.render(diagnostic);
    let mut out = std::io::sink();
    let mut interp = match fennec_interp::Interp::new(&mut out) {
        Ok(interp) => interp,
        Err(diagnostic) => {
            eprint!("{}", render(&diagnostic));
            return ExitCode::FAILURE;
        }
    };
    if let Err(diagnostic) = interp.load(&program.module) {
        eprint!("{}", render(&diagnostic));
        return ExitCode::FAILURE;
    }
    let keys: HashMap<String, Hash> = fennec_build::test_keys(&program.module).into_iter().collect();
    let results = Results::open(path, all);
    let unchanged: HashSet<&str> =
        keys.iter().filter(|(_, key)| results.passed.contains(*key)).map(|(name, _)| name.as_str()).collect();
    let outcomes = interp.run_tests_where(&|name| !unchanged.contains(name));
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

    fn store(&self, passing: &[Hash]) {
        if let Some((cache, program)) = &self.cache {
            // A cache that can't be written only means running these tests again next time.
            let _ = cache.store_passed_tests(program, passing);
        }
    }
}
