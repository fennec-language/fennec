//! Running a program's tests through LLVM. Each test becomes a function, and those LLVM can build
//! go into one executable with what they reach, compiled a function at a time from the cache.
//! Worker processes run it, each with its share of the tests, so a crash ends only the tests that
//! worker hadn't run yet. Tests that take capabilities or inputs, tests LLVM can't build yet and
//! tests a crash left come back for the caller to run another way. So does a test that failed,
//! since the other way says more about why, like what each side of an `assert` was. If it passes
//! there, the two disagree, which is reported as a failure rather than hidden.

use std::process::Command;

use fennec_interp::TestOutcome;
use fennec_syntax::{Diagnostic, FnDef, Item, Module, Span, Stmt, StmtKind, TestDef};

/// What running tests through LLVM did.
#[derive(Default)]
pub(crate) struct LlvmTests {
    /// The tests that passed.
    pub(crate) outcomes: Vec<TestOutcome>,
    /// The names of the tests to run another way.
    pub(crate) left: Vec<String>,
    /// The tests that failed, which are in `left` too, with how.
    pub(crate) failed: Vec<(String, Diagnostic)>,
    /// Why LLVM wouldn't build some of them, the first it said.
    pub(crate) refused: Option<String>,
}

/// What each test's function is named, by its number.
fn function_name(number: usize) -> String {
    format!("__test_{number}")
}

/// A test's body as a function that takes nothing and gives back nothing.
fn as_function(name: String, test: &TestDef) -> FnDef {
    let end = Span { start: test.span.end, end: test.span.end };
    let mut body: Vec<Stmt> = test.body.clone();
    // The body's last value is not what the test gives back.
    body.push(Stmt { kind: StmtKind::Pass, span: end });
    FnDef {
        name,
        generics: Vec::new(),
        params: Vec::new(),
        ret: None,
        arrow: None,
        fails: None,
        body,
        annotations: Vec::new(),
        span: test.span,
    }
}

/// `module` with only the functions of `tests` and what they reach.
fn trimmed_for(module: &Module, tests: &[&TestDef]) -> Module {
    let mut trimmed = module.clone();
    for (number, test) in tests.iter().enumerate() {
        trimmed.items.push(Item::Function(as_function(function_name(number), test)));
    }
    fennec_build::keep_what_functions_reach(&mut trimmed, &|name| name.starts_with("__test_"));
    trimmed
}

/// The tests LLVM has text for, from `tests`, which may mean splitting them to find the ones it
/// can't take: what one of them reaches may stop all of them. Each that can't be taken goes in
/// `refused`.
fn takeable<'t>(
    module: &Module,
    tests: &[&'t TestDef],
    refused: &mut Vec<(&'t TestDef, String)>,
) -> Vec<&'t TestDef> {
    match crate::aot::takes(&trimmed_for(module, tests)) {
        Ok(()) => tests.to_vec(),
        Err(why) => match tests {
            [] => Vec::new(),
            [test] => {
                refused.push((test, why));
                Vec::new()
            }
            _ => {
                let (first, second) = tests.split_at(tests.len() / 2);
                let mut taken = takeable(module, first, refused);
                taken.extend(takeable(module, second, refused));
                taken
            }
        },
    }
}

/// Reads two hexadecimal digits at a time.
fn from_hex(text: &str) -> Option<String> {
    let bytes: Option<Vec<u8>> = text
        .as_bytes()
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect();
    String::from_utf8(bytes?).ok()
}

/// A line a test executable wrote: which test, and how it failed if it did.
fn read_line(line: &str) -> Option<(usize, Option<Diagnostic>)> {
    let mut words = line.split(' ');
    let number = words.next()?.parse().ok()?;
    match words.next()? {
        "ok" => Some((number, None)),
        "failed" => {
            let start = words.next()?.parse().ok()?;
            let end = words.next()?.parse().ok()?;
            let message = from_hex(words.next()?)?;
            let help = from_hex(words.next().unwrap_or(""))?;
            let failure = Diagnostic::new(message, Span { start, end });
            let failure = if help.is_empty() { failure } else { failure.with_help(help) };
            Some((number, Some(failure)))
        }
        _ => None,
    }
}

/// How a failure reads for a test, whose function nobody wrote.
fn test_failure(failure: Diagnostic) -> Diagnostic {
    match failure.message.strip_prefix("`main` failed: ") {
        Some(error) => Diagnostic { message: format!("the test failed with {error}"), ..failure },
        None => failure,
    }
}

/// Runs the tests of a checked `module` named in `wanted` that LLVM can take.
pub(crate) fn run(module: &Module, wanted: &[String]) -> LlvmTests {
    let mut result = LlvmTests::default();
    let mut plain = Vec::new();
    let tests = module.items.iter().filter_map(|item| match item {
        Item::Test(test) if !test.bench && wanted.contains(&test.name) => Some(test),
        _ => None,
    });
    let tests: Vec<&TestDef> = tests.collect();
    for test in &tests {
        let unique = tests.iter().filter(|other| other.name == test.name).count() == 1;
        if test.inputs.is_empty() && test.capabilities.is_empty() && unique {
            plain.push(*test);
        } else {
            result.left.push(test.name.clone());
        }
    }
    let mut refused = Vec::new();
    let taken = takeable(module, &plain, &mut refused);
    for (test, why) in refused {
        result.left.push(test.name.clone());
        result.refused.get_or_insert(why);
    }
    if taken.is_empty() {
        return result;
    }
    let names: Vec<String> = (0..taken.len()).map(function_name).collect();
    let work = std::env::temp_dir().join(format!("fennec-tests-{}", std::process::id()));
    let built = crate::aot::tests_executable(&trimmed_for(module, &taken), &names, &work);
    if let Err(why) = built {
        result.left.extend(taken.iter().map(|test| test.name.clone()));
        result.refused.get_or_insert(why);
        return result;
    }
    let workers = std::thread::available_parallelism().map_or(1, usize::from).min(taken.len());
    let shares: Vec<Vec<String>> = (0..workers)
        .map(|worker| {
            (0..taken.len())
                .filter(|number| number % workers == worker)
                .map(|number| number.to_string())
                .collect()
        })
        .collect();
    let ran: Vec<String> = std::thread::scope(|scope| {
        let running: Vec<_> = shares
            .iter()
            .map(|share| {
                let work = &work;
                scope.spawn(move || {
                    Command::new(work)
                        .args(share)
                        .output()
                        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
                })
            })
            .collect();
        running.into_iter().filter_map(|worker| worker.join().ok()?.ok()).collect()
    });
    let _ = std::fs::remove_file(&work);
    let mut seen = vec![false; taken.len()];
    for (number, failure) in ran.iter().flat_map(|output| output.lines()).filter_map(read_line) {
        let (Some(test), Some(done)) = (taken.get(number), seen.get_mut(number)) else { continue };
        *done = true;
        match failure {
            None => result.outcomes.push(TestOutcome { name: test.name.clone(), failure: None, cases: 1 }),
            Some(failure) => {
                result.left.push(test.name.clone());
                result.failed.push((test.name.clone(), test_failure(failure)));
            }
        }
    }
    // A worker that crashed leaves tests no one has an answer for.
    result
        .left
        .extend(taken.iter().zip(&seen).filter(|(_, done)| !**done).map(|(test, _)| test.name.clone()));
    result
}
