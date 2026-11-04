//! Running a program's tests through LLVM. Each test becomes a function, and those LLVM can build
//! go into one executable with what they reach, compiled a function at a time from the cache.
//! Worker processes run it, each with its share of the tests, so a crash ends only the tests that
//! worker hadn't run yet. Tests that take inputs, tests LLVM can't build yet and tests a crash left
//! come back for the caller to run another way. So does a test that failed,
//! since the other way says more about why, like what each side of an `assert` was. If it passes
//! there, the two disagree, which is reported as a failure rather than hidden.

use std::collections::{HashMap, HashSet};
use std::process::Command;

use fennec_interp::TestOutcome;
use fennec_syntax::{Diagnostic, ExprKind, FnDef, Item, Module, Span, Stmt, StmtKind, TestDef};

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
    /// The program functions each test that passed ran, by their keys, for the tests that's known
    /// for.
    pub(crate) ran: HashMap<String, Vec<String>>,
}

/// Functions the IR may fuse into what uses them, which then has no flag of its own to set: a
/// generator, wherever it's written. They count as having run, for every test.
fn fusible(module: &Module) -> Vec<String> {
    let mut found = Vec::new();
    let mut add = |owner: Option<&str>, def: &FnDef| {
        if fennec_syntax::fuse::has_yield(&def.body) {
            found.push(owner.map_or_else(|| def.name.clone(), |owner| format!("{owner}.{}", def.name)));
        }
    };
    for item in &module.items {
        match item {
            Item::Function(def) => add(None, def),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                def.methods.iter().for_each(|method| add(Some(&def.name), method));
            }
            Item::Extend(extension) => {
                extension.methods.iter().for_each(|method| add(Some(&extension.name), method));
            }
            _ => {}
        }
    }
    found
}

/// The program functions a test ran, by their keys, from the functions that set their flags, or
/// None if one can't be put down to a definition. Then the test depends on all it can reach, since
/// a function missed here is a test skipped wrongly.
fn traced(
    flagged: &[&str],
    fused: &[String],
    defines: &dyn Fn(&str) -> bool,
    prelude: &HashSet<String>,
) -> Option<Vec<String>> {
    let mut ran: Vec<String> = fused.to_vec();
    for name in flagged {
        if name.starts_with(TEST_FUNCTION) {
            continue;
        }
        // A copy of a generic function for one type, or a function written inside another, is
        // part of the definition around it.
        let plain = name.split('[').next().unwrap_or(name);
        let mut known = plain;
        while !defines(known) {
            match known.rsplit_once('.') {
                Some((around, _)) => known = around,
                None => break,
            }
        }
        if defines(known) {
            ran.push(known.to_owned());
        } else if !prelude.contains(plain) {
            return None;
        }
    }
    ran.sort();
    ran.dedup();
    Some(ran)
}

/// What the standard library defines, whose functions Fennec's build stands for in a test's key.
fn prelude_functions() -> HashSet<String> {
    let mut names = HashSet::new();
    for library in fennec_syntax::prelude_modules().unwrap_or_default() {
        for item in &library.items {
            match item {
                Item::Function(def) => {
                    names.insert(def.name.clone());
                }
                Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                    names.extend(def.methods.iter().map(|method| format!("{}.{}", def.name, method.name)));
                }
                Item::Extend(extension) => {
                    names.extend(
                        extension.methods.iter().map(|method| format!("{}.{}", extension.name, method.name)),
                    );
                }
                _ => {}
            }
        }
    }
    names
}

/// How each test's function is named, which no program can write.
const TEST_FUNCTION: &str = "test:";

/// What a test's function calls its body before it has its real name.
const PLACEHOLDER: &str = "test_body";

/// What each test's function is named, by its number.
fn function_name(number: usize) -> String {
    format!("{TEST_FUNCTION}{number}")
}

/// A test's body as a function that takes the capabilities it names and gives back nothing.
fn as_function(name: String, test: &TestDef) -> FnDef {
    let end = Span { start: test.span.end, end: test.span.end };
    let mut body: Vec<Stmt> = test.body.clone();
    // The body's last value is not what the test gives back.
    body.push(Stmt { kind: StmtKind::Pass, span: end });
    FnDef {
        name,
        generics: Vec::new(),
        params: test.capabilities.clone(),
        ret: None,
        arrow: None,
        fails: None,
        body,
        annotations: Vec::new(),
        span: test.span,
    }
}

/// Whether a test's body can pass a failure up with `?`, which makes its function give back a
/// result.
fn passes_failures_up(test: &TestDef) -> bool {
    let mut found = false;
    let mut body = test.body.clone();
    fennec_syntax::visit::in_stmts(&mut body, &mut |inner| {
        found |= matches!(inner.kind, ExprKind::Try { .. });
    });
    found
}

/// The function for test `number`, which takes the capabilities the test names and gives back
/// nothing, as the executable calls it. A body that passes failures up goes in a function of its
/// own, and this one fails if that gives back a failure: `fennec test` runs a failed test again
/// another way, which says what the failure was.
fn test_functions(number: usize, test: &TestDef) -> Vec<FnDef> {
    let name = function_name(number);
    if !passes_failures_up(test) {
        return vec![as_function(name, test)];
    }
    let body_name = format!("{name}:body");
    let args: Vec<&str> = test.capabilities.iter().map(|param| param.name.as_str()).collect();
    let source = format!(
        "def caller():\n    match {PLACEHOLDER}({}):\n        case Err(_):\n            assert False\n        case _:\n            pass\n",
        args.join(", ")
    );
    let Ok(parsed) = fennec_syntax::parse_at(&source, test.span.start) else {
        return vec![as_function(name, test)];
    };
    let Some(Item::Function(mut caller)) = parsed.items.into_iter().next() else {
        return vec![as_function(name, test)];
    };
    fennec_syntax::visit::in_stmts(&mut caller.body, &mut |inner| {
        if matches!(&inner.kind, ExprKind::Name(named) if named == PLACEHOLDER) {
            inner.kind = ExprKind::Name(body_name.clone());
        }
    });
    caller.name = name;
    caller.params.clone_from(&test.capabilities);
    caller.span = test.span;
    vec![as_function(body_name, test), caller]
}

/// `module` with only the functions of `tests` and what they reach.
fn trimmed_for(module: &Module, tests: &[&TestDef]) -> Module {
    let mut trimmed = module.clone();
    for (number, test) in tests.iter().enumerate() {
        trimmed.items.extend(test_functions(number, test).into_iter().map(Item::Function));
    }
    fennec_build::keep_what_functions_reach(&mut trimmed, &|name| name.starts_with(TEST_FUNCTION));
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

/// What a test executable said about one test: its number, how it failed if it did, and the
/// numbers of the flags it set.
type TestLine = (usize, Option<Diagnostic>, Vec<usize>);

/// Reads a line a test executable wrote.
fn read_line(line: &str) -> Option<TestLine> {
    let mut words = line.split(' ');
    let number = words.next()?.parse().ok()?;
    match words.next()? {
        "ok" => {
            let ran: Option<Vec<usize>> = words
                .next()
                .unwrap_or("")
                .split(',')
                .filter(|at| !at.is_empty())
                .map(|at| at.parse().ok())
                .collect();
            Some((number, None, ran?))
        }
        "failed" => {
            let start = words.next()?.parse().ok()?;
            let end = words.next()?.parse().ok()?;
            let message = from_hex(words.next()?)?;
            let help = from_hex(words.next().unwrap_or(""))?;
            let failure = Diagnostic::new(message, Span { start, end });
            let failure = if help.is_empty() { failure } else { failure.with_help(help) };
            Some((number, Some(failure), Vec::new()))
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

/// Runs a test executable in worker processes, each with its share of `tests` tests, giving back
/// what each wrote.
fn run_workers(work: &std::path::Path, tests: usize) -> Vec<String> {
    let workers = std::thread::available_parallelism().map_or(1, usize::from).min(tests);
    let shares: Vec<Vec<String>> = (0..workers)
        .map(|worker| {
            (0..tests).filter(|number| number % workers == worker).map(|number| number.to_string()).collect()
        })
        .collect();
    std::thread::scope(|scope| {
        let running: Vec<_> = shares
            .iter()
            .map(|share| {
                scope.spawn(move || {
                    Command::new(work)
                        .args(share)
                        .output()
                        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
                })
            })
            .collect();
        running.into_iter().filter_map(|worker| worker.join().ok()?.ok()).collect()
    })
}

/// Runs the tests of a checked `module` named in `wanted` that LLVM can take. `defines` says
/// whether the program defines a function by a name.
pub(crate) fn run(module: &Module, wanted: &[String], defines: &dyn Fn(&str) -> bool) -> LlvmTests {
    let mut result = LlvmTests::default();
    let mut plain = Vec::new();
    let tests = module.items.iter().filter_map(|item| match item {
        Item::Test(test) if !test.bench && wanted.contains(&test.name) => Some(test),
        _ => None,
    });
    let tests: Vec<&TestDef> = tests.collect();
    for test in &tests {
        let unique = tests.iter().filter(|other| other.name == test.name).count() == 1;
        if test.inputs.is_empty() && unique {
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
    let work = std::env::temp_dir().join(format!(
        "fennec-tests-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    let flags = match crate::aot::tests_executable(&trimmed_for(module, &taken), &names, &work) {
        Ok(flags) => flags,
        Err(why) => {
            result.left.extend(taken.iter().map(|test| test.name.clone()));
            result.refused.get_or_insert(why);
            return result;
        }
    };
    let fused = fusible(module);
    let prelude = prelude_functions();
    let ran = run_workers(&work, taken.len());
    let _ = std::fs::remove_file(&work);
    let mut seen = vec![false; taken.len()];
    for (number, failure, set) in ran.iter().flat_map(|output| output.lines()).filter_map(read_line) {
        let (Some(test), Some(done)) = (taken.get(number), seen.get_mut(number)) else { continue };
        *done = true;
        match failure {
            None => {
                let flagged: Vec<&str> =
                    set.iter().filter_map(|at| flags.get(*at)).map(String::as_str).collect();
                if let Some(ran) = traced(&flagged, &fused, defines, &prelude) {
                    result.ran.insert(test.name.clone(), ran);
                }
                result.outcomes.push(TestOutcome { name: test.name.clone(), failure: None, cases: 1 });
            }
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
