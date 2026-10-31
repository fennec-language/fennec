//! Every program in `examples/` runs and prints exactly its `.out` file, and every program in
//! `examples/errors/` fails with exactly its `.err` file, whether the type checker or the
//! interpreter catches the mistake. The examples are the language's working specification,
//! so the site and the compiler can't drift apart.

use std::fs;
use std::path::{Path, PathBuf};

use fennec_jit::JitError;

/// Examples the JIT may hand back to the interpreter whole, because it can't compile their
/// `main` yet. Every other example must run natively.
const INTERPRETED: &[&str] = &[];

/// The functions the JIT can't compile yet, in the examples that have any: generators,
/// functions with functions inside them, and `match`es with a guarded `case`. They run in
/// the interpreter.
const INTERPRETED_FUNCTIONS: &[(&str, &[&str])] = &[
    ("case_guards.fen", &["describe", "first_word", "size"]),
    ("functions_more.fen", &["countdown", "counter_from"]),
    ("operators.fen", &["Grid.iter"]),
];

/// Examples that compile to plain machine code: nothing handed to the interpreter, and no
/// function needing the runtime's frames.
const PLAIN_MACHINE_CODE: &[&str] =
    &["calculator.fen", "hello.fen", "numbers.fen", "robot.fen", "string_building.fen"];

/// Checks how an example compiled, and that its run left nothing behind in the runtime.
fn check_report(name: &str, report: &fennec_jit::Report) {
    let expected = INTERPRETED_FUNCTIONS
        .iter()
        .find(|(example, _)| *example == name)
        .map_or(&[][..], |(_, names)| names);
    assert_eq!(report.interpreted, expected, "{name}: the JIT couldn't compile these");
    assert_eq!(
        (report.leftover_temps, report.leftover_frames, report.leftover_boxes),
        (0, 0, 0),
        "{name} left temporaries, frames or boxed values"
    );
    if PLAIN_MACHINE_CODE.contains(&name) {
        assert_eq!((report.handed_over, report.with_frames), (0, 0), "{name} should be plain machine code");
    }
}

fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

fn programs(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "fen") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

/// Runs a program the way `fennec run` does: its output, or its rendered errors.
fn run(source: &str, shown_as: &str) -> Result<String, String> {
    let mut module =
        fennec_syntax::parse(source).map_err(|diagnostic| diagnostic.render(shown_as, source))?;
    fennec_check::check_and_resolve(&mut module).map_err(|diagnostics| {
        let rendered: Vec<String> =
            diagnostics.iter().map(|diagnostic| diagnostic.render(shown_as, source)).collect();
        rendered.join("\n")
    })?;
    let mut out = Vec::new();
    fennec_interp::run(&module, &mut out).map_err(|diagnostic| diagnostic.render(shown_as, source))?;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// Runs a program's `test` blocks, returning each failure, rendered.
fn failing_tests(source: &str, shown_as: &str) -> Result<Vec<String>, String> {
    let render = |diagnostic: fennec_syntax::Diagnostic| diagnostic.render(shown_as, source);
    let module = fennec_syntax::parse(source).map_err(render)?;
    let mut out = std::io::sink();
    let mut interp = fennec_interp::Interp::new(&mut out).map_err(render)?;
    interp.load(&module).map_err(render)?;
    Ok(interp
        .run_tests()
        .into_iter()
        .filter_map(|outcome| outcome.failure.map(|failure| format!("{}: {}", outcome.name, render(failure))))
        .collect())
}

/// Runs a test on a stack as big as `fennec run` gives programs, so examples can nest calls
/// as deeply as the language allows.
fn on_big_stack(test: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    let worker = std::thread::Builder::new().stack_size(512 * 1024 * 1024).spawn(test)?;
    if let Err(panic) = worker.join() {
        std::panic::resume_unwind(panic);
    }
    Ok(())
}

#[test]
fn examples_print_their_expected_output() {
    on_big_stack(|| {
        let paths = programs(&examples_dir()).unwrap();
        assert!(!paths.is_empty(), "no examples found");
        for path in paths {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let source = fs::read_to_string(&path).unwrap();
            let expected = fs::read_to_string(path.with_extension("out")).unwrap();
            let actual = run(&source, &format!("examples/{name}"))
                .unwrap_or_else(|err| panic!("{name} failed:\n{err}"));
            assert_eq!(actual, expected, "{name} printed something different");
            let failures = failing_tests(&source, &format!("examples/{name}")).unwrap();
            assert!(failures.is_empty(), "{name} has failing tests:\n{}", failures.join("\n"));
        }
    })
    .unwrap();
}

#[test]
fn error_examples_fail_with_their_expected_message() {
    on_big_stack(|| {
        let paths = programs(&examples_dir().join("errors")).unwrap();
        assert!(!paths.is_empty(), "no error examples found");
        for path in paths {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let source = fs::read_to_string(&path).unwrap();
            let expected = fs::read_to_string(path.with_extension("err")).unwrap();
            let actual =
                run(&source, &format!("examples/errors/{name}")).expect_err(&format!("{name} should fail"));
            assert_eq!(actual, expected, "{name} failed with a different message");
        }
    })
    .unwrap();
}

#[test]
fn the_jit_runs_examples_exactly_like_the_interpreter() {
    on_big_stack(|| {
        let dir = examples_dir();
        let mut paths = programs(&dir).unwrap();
        paths.extend(programs(&dir.join("errors")).unwrap());
        for path in paths {
            let name = path.strip_prefix(&dir).unwrap().to_string_lossy().replace('\\', "/");
            let source = fs::read_to_string(&path).unwrap();
            let shown_as = format!("examples/{name}");
            let Ok(module) = fennec_syntax::parse(&source) else { continue };
            if fennec_check::check(&module).is_err() {
                continue;
            }
            let mut out = Vec::new();
            let actual = match fennec_jit::run_with_report(&module, &mut out) {
                Ok(report) => {
                    check_report(&name, &report);
                    Ok(String::from_utf8_lossy(&out).into_owned())
                }
                Err(JitError::Failed(diagnostic)) => Err(diagnostic.render(&shown_as, &source)),
                Err(JitError::Unsupported(why)) => {
                    assert!(
                        INTERPRETED.contains(&name.as_str()),
                        "{name} should compile natively, but: {}",
                        why.message
                    );
                    continue;
                }
            };
            assert_eq!(actual, run(&source, &shown_as), "{name} ran differently as native code");
        }
    })
    .unwrap();
}

#[test]
fn math_compiles_to_machine_code_however_it_is_imported() {
    let source = "import std.math\nfrom std import math\nfrom std.math import sqrt\n\ndef main(stdout: Stdout):\n    x = 2.25\n    stdout.print(sqrt(x) + math.sqrt(x) + std.math.sqrt(x), math.floor(x))\n";
    let module = fennec_syntax::parse(source).unwrap();
    let mut out = Vec::new();
    let report = fennec_jit::run_with_report(&module, &mut out).unwrap();
    assert_eq!(String::from_utf8_lossy(&out), "4.5 2\n");
    assert_eq!((report.handed_over, report.with_frames), (0, 0), "math should be plain machine code");
}

#[test]
fn a_second_run_reuses_every_function_and_an_edit_compiles_only_that_function() {
    let home = std::env::temp_dir().join(format!("fennec-examples-cache-{}", std::process::id()));
    let cache = fennec_build::Cache::new(&home);
    // What a native run through the cache printed, and how many functions came from it.
    let cached_run = |source: &str, cache: &fennec_build::Cache| {
        let module = fennec_syntax::parse(source).unwrap();
        let mut out = Vec::new();
        let inputs = fennec_interp::Inputs::default();
        let report = fennec_jit::run_cached(&module, None, &mut out, inputs, cache, "cached.fen").unwrap();
        (String::from_utf8_lossy(&out).into_owned(), report.cached)
    };
    let program = |greeting: &str| {
        format!(
            "def double(x: int) -> int:\n    x * 2\n\ndef greet(name: str) -> str:\n    \"{greeting}, \" + name\n\n\
            def main(stdout: Stdout):\n    stdout.print(greet(\"kit\"), double(21))\n"
        )
    };
    let (first, none) = cached_run(&program("hello"), &cache);
    let (second, all) = cached_run(&program("hello"), &cache);
    let (edited, rest) = cached_run(&program("hi there"), &cache);
    let _ = fs::remove_dir_all(&home);
    assert_eq!(
        (first.as_str(), second.as_str(), edited.as_str()),
        ("hello, kit 42\n", "hello, kit 42\n", "hi there, kit 42\n")
    );
    assert_eq!(none, 0, "the first run finds nothing");
    assert!(all >= 3, "the second run reuses every function, but reused {all}");
    assert_eq!(rest, all - 1, "the edit compiles only the edited function");
}
