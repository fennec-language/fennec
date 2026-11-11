//! `fennec test`: checks a program, then runs its `test` blocks and the examples in its `##`
//! doc comments. For a kit, that's the tests in every file under `src`. It prints each failure and a line of totals, and lists the tests
//! that pass only for `--verbose`, so that passing tests cost an agent nothing to read.
//!
//! A test that passed is run again only when something it depends on changed: the test
//! itself, the functions it ran and what those rely on (see `fennec_build::TestGraph`). What
//! each test that passed depends on is kept in the cache in `~/.fennec`, and CI reuses it like
//! anywhere else. `--all` runs every test, as do runs with `FENNEC_NO_CACHE` set. A test that
//! failed always runs again, and so does every test after Fennec itself changes.
//!
//! `fennec run` does this first, for the tests a change affects, unless told not to.
//!
//! Tests run through LLVM's cached dev build, in worker processes on every core (see
//! `llvm_tests`). A failing property test's input is shrunk there to the simplest that fails. A
//! test LLVM can't take yet fails, saying why.

use std::collections::HashMap;
use std::process::ExitCode;

use fennec_build::{Cache, TestGraph, TestRecord};

use crate::verdict::{Modules, Plan};

/// How `fennec test` was asked to run.
#[derive(Clone, Copy, Default)]
pub(crate) struct Options {
    /// Which tests run, and what they get.
    run: Run,
    /// List the tests that pass too.
    verbose: bool,
    /// Keep standard output for the program that runs next, and say nothing if all is well.
    before_a_run: bool,
}

/// Which tests run, and what they get.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Run {
    /// The tests a change affects.
    #[default]
    Affected,
    /// Every test.
    All,
    /// Every test, with the real capabilities rather than fakes. The world can change between
    /// runs, so passing says nothing about the next run.
    Real,
}

impl Options {
    /// The path and options in what follows `fennec test`: any order, and at most one path.
    pub(crate) fn of(args: &[&str]) -> Option<(String, Self)> {
        let mut options = Options::default();
        let mut path = None;
        for arg in args {
            match *arg {
                "--all" if options.run == Run::Affected => options.run = Run::All,
                "--all" => {}
                "--verbose" | "-v" => options.verbose = true,
                "--real" => options.run = Run::Real,
                flag if flag.starts_with("--") => return None,
                written if path.is_none() => path = Some(written),
                _ => return None,
            }
        }
        Some((path.unwrap_or(".").to_owned(), options))
    }
}

/// Runs the tests a change affects before a program runs, giving back how to stop if one fails.
/// When no file has changed since they last passed, this reads little or nothing.
pub(crate) fn before_a_run(path: &str) -> Option<ExitCode> {
    // A kit whose files are as they were when all its tests passed needs nothing read at all.
    if let Some(cache) = &crate::run::cache() {
        let name = crate::run::absolute(path);
        if let Some(state) = crate::verdict::last_state((cache, &name))
            && let Some(files) = crate::verdict::kit_files(path, Some(&state))
            && crate::verdict::same_files(&state, &files)
        {
            return None;
        }
    }
    let texts = fennec_kit::all_texts(std::path::Path::new(path))?;
    if !texts.iter().any(|(_, text)| text.contains("test \"")) {
        return None;
    }
    let passed = crate::run::fingerprint(
        texts.iter().map(|(path, text)| (path.as_str(), fennec_blake3::hash(text.as_bytes()))),
    );
    let cache = crate::run::cache();
    let name = crate::run::passed_name(path);
    if let Some(cache) = &cache
        && cache.read("tested", &name).is_some_and(|last| last == passed.as_bytes())
    {
        return None;
    }
    let code = test(path, Options { before_a_run: true, ..Options::default() });
    if code != ExitCode::SUCCESS {
        return Some(code);
    }
    if let Some(cache) = &cache {
        // A cache that can't be written only means looking at the tests again next time.
        fennec_build::kept("keep which tests passed", cache.write("tested", &name, passed.as_bytes()));
    }
    None
}

pub(crate) fn test(path: &str, options: Options) -> ExitCode {
    let results = Results::open(path, options.run != Run::Affected);
    let cached = results.cache.as_ref().map(|(cache, name)| (cache, name.as_str()));
    let state = cached.and_then(crate::verdict::last_state);
    let files = crate::verdict::kit_files(path, state.as_ref());
    let planned = match (cached, &state, &files) {
        (Some(cached), Some(state), Some(files)) if options.run == Run::Affected => {
            crate::verdict::plan(path, cached, (state, files), &results.passed)
        }
        _ => None,
    };
    let plan = match planned {
        Some(plan) => plan,
        None => match whole_program(path, &results) {
            Ok(plan) => plan,
            Err(code) => return code,
        },
    };
    run_plan(path, options, &results, files.as_deref(), plan)
}

/// What to run, from all of a program: what has changed since each test last passed.
fn whole_program(path: &str, results: &Results) -> Result<Plan, ExitCode> {
    let Some(mut program) = crate::load(path, fennec_kit::Entry::Tests) else {
        return Err(ExitCode::FAILURE);
    };
    // The standard library as a run checks it: just what the tests reach.
    let facts = match crate::copy_rule::check_to_run(&mut program.module, &program.sources) {
        Ok(signatures) => {
            crate::verdict::KitFacts::of(crate::verdict::KitNames::of(&program.module.items), &signatures)
        }
        Err(diagnostics) => {
            eprint!("{}", program.sources.render_all(&diagnostics));
            return Err(ExitCode::FAILURE);
        }
    };
    let graph = TestGraph::of(&program.module, env!("FENNEC_BUILD"));
    let unchanged: Vec<TestRecord> = graph
        .tests()
        .filter_map(|name| results.passed.get(name))
        .filter(|record| graph.key(&record.test, record.ran.as_deref()) == Some(record.key))
        .cloned()
        .collect();
    let is_unchanged = |name: &str| unchanged.iter().any(|record| record.test == name);
    let wanted = graph.tests().filter(|name| !is_unchanged(name)).map(str::to_owned).collect();
    Ok(Plan { program, wanted, unchanged, facts })
}

/// A test as it's kept once it has passed.
fn record_of(
    (graph, program, modules): (&TestGraph, &fennec_kit::Program, &Modules<'_>),
    test: &str,
    ran: Option<Vec<String>>,
) -> Option<TestRecord> {
    let key = graph.key(test, ran.as_deref())?;
    let span = program.module.items.iter().find_map(|item| match item {
        fennec_syntax::Item::Test(found) if found.name == test => Some(found.span),
        _ => None,
    });
    let shown = span.and_then(|span| program.sources.file_at(span)).map(|(shown, ..)| shown);
    let module = shown.and_then(|shown| modules.of_file(shown)).unwrap_or_default().to_owned();
    let reach = if ran.is_some() { Vec::new() } else { graph.reach(test) };
    Some(TestRecord { test: test.to_owned(), module, key, ran, reach })
}

fn run_plan(
    path: &str,
    options: Options,
    results: &Results,
    files: Option<&[fennec_build::KitFile]>,
    plan: Plan,
) -> ExitCode {
    let Plan { program, wanted, unchanged, facts } = plan;
    let render = |diagnostic: &fennec_syntax::Diagnostic| program.sources.render(diagnostic);
    let graph = TestGraph::of(&program.module, env!("FENNEC_BUILD"));
    if let Some(why) = crate::aot::not_ready() {
        eprintln!("error: {why}");
        return ExitCode::FAILURE;
    }
    let real = options.run == Run::Real;
    let through_llvm = crate::llvm_tests::run(&program.module, &wanted, &|name| graph.defines(name), real);
    if options.verbose {
        say_how_llvm_went(&through_llvm, wanted.len());
    }
    let mut outcomes = through_llvm.outcomes;
    let mut ran_through_llvm = through_llvm.ran;
    // A test LLVM can't build yet fails, saying why, as `fennec build` would.
    let why = through_llvm.refused.as_deref().unwrap_or("LLVM couldn't build it");
    for name in through_llvm.left {
        let span = program.module.items.iter().find_map(|item| match item {
            fennec_syntax::Item::Test(found) if found.name == name => Some(found.span),
            _ => None,
        });
        let failure = fennec_syntax::Diagnostic::new(
            format!("LLVM can't build what this test reaches yet: {why}"),
            span.unwrap_or_default(),
        );
        let trace = fennec_syntax::Trace::default();
        outcomes.push(fennec_rt::TestOutcome { name, failure: Some(failure), trace, cases: 1 });
    }
    // Workers finish in any order, and a report reads in the order the tests are written.
    let place = |name: &str| graph.tests().position(|test| test == name);
    outcomes.sort_by_key(|outcome| place(&outcome.name));
    if outcomes.is_empty() && unchanged.is_empty() {
        if !options.before_a_run {
            println!("no tests in {path}");
        }
        return ExitCode::SUCCESS;
    }
    let modules = Modules::of(files.unwrap_or_default());
    let mut failed = 0;
    let mut passing: Vec<TestRecord> = unchanged.clone();
    for outcome in &outcomes {
        let cases = if outcome.cases > 1 { format!(" ({} inputs)", outcome.cases) } else { String::new() };
        match &outcome.failure {
            None => {
                if options.verbose {
                    println!("  ✓ {}{cases}", outcome.name);
                }
                let ran = ran_through_llvm.remove(&outcome.name);
                passing.extend(record_of((&graph, &program, &modules), &outcome.name, ran));
            }
            Some(diagnostic) => {
                failed += 1;
                if options.before_a_run {
                    eprintln!("  ✗ {}", outcome.name);
                } else {
                    println!("  ✗ {}", outcome.name);
                }
                eprint!("{}", render(diagnostic));
                // Past a call outside the program's own files, the chain is what runs the test, a
                // property test's shrinking say, which says nothing of the test's own failure.
                let own = outcome.trace.until(|span| program.sources.locate(span).is_none());
                eprint!("{}", program.sources.render_trace(diagnostic.span, &own));
            }
        }
    }
    // A run with real capabilities says nothing about the next one, so none of it is kept.
    if options.run != Run::Real {
        results.store(&passing);
        if let (Some((cache, name)), Some(files)) = (&results.cache, files) {
            if failed == 0 {
                // A cache that can't be written only means looking at all of it next time.
                let remembered = crate::verdict::remember((cache, name), (files, &program), &facts);
                fennec_build::kept("keep what the tests depend on", remembered);
            } else {
                crate::verdict::forget((cache, name));
            }
        }
    }
    let passed = outcomes.len() + unchanged.len() - failed;
    say_totals(options, (passed, failed), (outcomes.len(), unchanged.len()));
    if failed == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// The line of totals, or when a program runs next, nothing unless a test failed.
fn say_totals(options: Options, (passed, failed): (usize, usize), (ran, unchanged): (usize, usize)) {
    let totals = if unchanged == 0 {
        format!("{passed} passed, {failed} failed")
    } else {
        format!("{passed} passed, {failed} failed ({ran} run, {unchanged} unchanged)")
    };
    if !options.before_a_run {
        println!("{totals}");
    } else if failed > 0 {
        eprintln!("{totals}");
        eprintln!("the program didn't run; `fennec run --no-test` runs it anyway");
    }
}

/// What LLVM took. Most of a program's tests still reach something it doesn't, and every run saying
/// so would cost whoever reads it for nothing they can act on, so only `--verbose` says.
fn say_how_llvm_went(through_llvm: &crate::llvm_tests::LlvmTests, wanted: usize) {
    if let Some(why) = through_llvm.refused.as_deref().and_then(|why| why.lines().next()) {
        eprintln!("note: some tests fail, since LLVM can't build what they reach yet: {why}");
    }
    eprintln!("note: {} of {wanted} tests ran through LLVM", through_llvm.outcomes.len());
}

/// What names a kit wherever it is: its name in `kit.toml`. `None` for a program that isn't a kit.
fn kit_named(path: &str) -> Option<String> {
    let manifest = std::fs::read_to_string(std::path::Path::new(path).join("kit.toml")).ok()?;
    let manifest = fennec_kit::Manifest::parse(&manifest).ok()?;
    Some(format!("kit {}", manifest.name))
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
            Some((cache, program)) if !all => {
                let here = cache.test_records(program);
                // A kit that's moved has no results where it is now, so they're taken from what
                // it's called. A result only counts when its key matches the test as it is, so a
                // kit of the same name elsewhere can't pass a test for this one.
                match kit_named(path) {
                    Some(named) if here.is_empty() => cache.test_records(&named),
                    _ => here,
                }
            }
            _ => HashMap::new(),
        };
        Results { cache, passed }
    }

    /// Where machine code for this program's tests is kept: apart from its `main`'s, since
    /// tests reach other functions.
    fn store(&self, passing: &[TestRecord]) {
        if let Some((cache, program)) = &self.cache {
            // A cache that can't be written only means running these tests again next time.
            fennec_build::kept("keep what the tests ran", cache.store_test_records(program, passing));
            if let Some(named) = kit_named(program) {
                fennec_build::kept("keep what the tests ran", cache.store_test_records(&named, passing));
            }
        }
    }
}
