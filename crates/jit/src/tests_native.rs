//! Running a program's tests as native code, on every core at once. Each worker thread
//! compiles what its share of the tests reaches and runs them in turn, since compiled code
//! runs only on the thread it was compiled for. The machine code is shared through the cache.
//!
//! A property test needs inputs from the interpreter, so it stays there, as does any test that
//! can't be compiled yet. Those come back for the caller to run.

use std::collections::HashMap;

use fennec_interp::TestOutcome;
use fennec_syntax::{Diagnostic, FnDef, Item, Module, Span, Stmt, StmtKind};

use crate::types::Program;
use crate::{NATIVE_STACK_BYTES, STACK_BYTES, lower, runtime};

/// How many tests a worker should have before another one is worth starting: each loads
/// the program and compiles what its tests reach.
const TESTS_PER_WORKER: usize = 16;

/// What running tests natively did: their outcomes, and the names of the ones it didn't run.
#[derive(Default)]
pub struct NativeTests {
    pub outcomes: Vec<TestOutcome>,
    pub left: Vec<String>,
}

/// Runs the tests of a checked `module` named in `wanted`. `program` names the program in
/// `cache`, whose machine code is reused and added to.
pub fn run_tests(
    module: &Module,
    signatures: &fennec_check::Signatures,
    wanted: &[String],
    cache: Option<(&fennec_build::Cache, &str)>,
) -> NativeTests {
    let mut result = NativeTests::default();
    let mut runnable: Vec<(String, FnDef)> = Vec::new();
    let tests = module.items.iter().filter_map(|item| match item {
        Item::Test(test) => Some(test),
        _ => None,
    });
    let tests: Vec<_> = tests.filter(|test| wanted.contains(&test.name)).collect();
    for test in &tests {
        let unique = tests.iter().filter(|other| other.name == test.name).count() == 1;
        if test.inputs.is_empty() && unique {
            runnable.push((test.name.clone(), as_function(&test.name, &test.body, test.span)));
        } else {
            result.left.push(test.name.clone());
        }
    }
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let workers = cores.min(runnable.len().div_ceil(TESTS_PER_WORKER)).max(1);
    let mut shares: Vec<Vec<(String, FnDef)>> = (0..workers).map(|_| Vec::new()).collect();
    for (at, test) in runnable.into_iter().enumerate() {
        if let Some(share) = shares.get_mut(at % workers) {
            share.push(test);
        }
    }
    let prelude: Vec<Module> =
        fennec_syntax::prelude().filter_map(|source| fennec_syntax::parse_prelude(source).ok()).collect();
    let done: Vec<NativeTests> = std::thread::scope(|scope| {
        let prelude = &prelude;
        let handles: Vec<_> = shares
            .iter()
            .filter(|share| !share.is_empty())
            .map(|share| {
                let worker = std::thread::Builder::new()
                    .stack_size(STACK_BYTES)
                    .spawn_scoped(scope, move || run_share(module, prelude, signatures, share, cache));
                (share, worker)
            })
            .collect();
        handles
            .into_iter()
            .map(|(share, worker)| {
                let finished = worker.ok().and_then(|worker| worker.join().ok());
                finished.unwrap_or_else(|| left_alone(share))
            })
            .collect()
    });
    for share in done {
        result.outcomes.extend(share.outcomes);
        result.left.extend(share.left);
    }
    result
}

/// A test as a function with no parameters that gives back nothing or an error, with its key
/// in the typed tree as its name.
fn as_function(name: &str, body: &[Stmt], span: Span) -> FnDef {
    let end = Span { start: span.end, end: span.end };
    let mut body = body.to_vec();
    // The body's last value is not what the test gives back.
    body.push(Stmt { kind: StmtKind::Pass, span: end });
    FnDef {
        name: format!("test \"{name}\""),
        generics: Vec::new(),
        params: Vec::new(),
        ret: None,
        arrow: None,
        body,
        span,
    }
}

fn left_alone(share: &[(String, FnDef)]) -> NativeTests {
    NativeTests { outcomes: Vec::new(), left: share.iter().map(|(name, _)| name.clone()).collect() }
}

/// Compiles and runs one worker's tests on this thread. What goes wrong while compiling
/// leaves them to the interpreter, which reports it properly if it's a real problem.
fn run_share(
    module: &Module,
    prelude: &[Module],
    signatures: &fennec_check::Signatures,
    share: &[(String, FnDef)],
    cache: Option<(&fennec_build::Cache, &str)>,
) -> NativeTests {
    let here = 0_u8;
    let stack_limit = std::ptr::from_ref(&here).addr().saturating_sub(NATIVE_STACK_BYTES);
    let keyed: Vec<(String, FnDef)> = share.iter().map(|(_, def)| (def.name.clone(), def.clone())).collect();
    let names: HashMap<String, String> =
        share.iter().map(|(name, def)| (def.name.clone(), name.clone())).collect();
    let Ok(program) = Program::for_tests(module, prelude, signatures, &keyed) else {
        return left_alone(share);
    };
    let Ok((tests, tables)) = lower::compile_tests(&program, signatures, stack_limit, cache, &names) else {
        return left_alone(share);
    };
    let Ok(results) = runtime::run_tests(&tests, module, tables) else { return left_alone(share) };
    let mut done = NativeTests::default();
    done.left.extend(
        share
            .iter()
            .map(|(name, _)| name)
            .filter(|name| !results.iter().any(|(ran, _)| ran == *name))
            .cloned(),
    );
    done.outcomes.extend(results.into_iter().map(|(name, failure)| TestOutcome {
        name,
        failure: failure.map(test_failure),
        cases: 1,
    }));
    done
}

/// A test that fails with an error is reported like one, not like a failing `main`.
fn test_failure(failure: Diagnostic) -> Diagnostic {
    match failure.message.strip_prefix("`main` failed: ") {
        Some(error) => Diagnostic { message: format!("the test failed with {error}"), ..failure },
        None => failure,
    }
}
