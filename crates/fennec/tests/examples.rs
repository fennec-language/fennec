//! Every program in `examples/` runs and prints exactly its `.out` file, and every program in
//! `examples/errors/` fails with exactly its `.err` file, whether the type checker or the
//! interpreter catches the mistake. The examples are the language's working specification,
//! so the site and the compiler can't drift apart.

use std::fs;
use std::path::{Path, PathBuf};

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
    let mut module = fennec_syntax::parse(source).map_err(render)?;
    // What runs a module needs it resolved, as `fennec test` does.
    fennec_check::check_and_resolve(&mut module)
        .map_err(|problems| problems.into_iter().map(render).collect::<Vec<_>>().join("\n"))?;
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
fn a_run_checking_only_what_it_reaches_finds_the_same_mistakes() {
    for path in programs(&examples_dir().join("errors")).unwrap() {
        let source = fs::read_to_string(&path).unwrap();
        let Ok(mut module) = fennec_syntax::parse(&source) else { continue };
        let whole = fennec_check::check(&module).err();
        let reached = fennec_check::check_to_run(&mut module).err();
        assert_eq!(reached, whole, "{} found different mistakes", path.display());
    }
}

/// How many functions in each benchmark kernel LLVM doesn't take can only go down: a change that
/// adds one fails here, and one that removes some lowers the count in `bench/fallbacks.txt`.
#[test]
fn benchmark_kernels_only_lose_fallbacks() {
    let bench = examples_dir().join("../bench");
    let written = fs::read_to_string(bench.join("fallbacks.txt")).unwrap();
    let allowed: Vec<(String, usize)> = written
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .map(|line| {
            let (kernel, count) = line.split_once(' ').unwrap();
            (kernel.to_owned(), count.parse().unwrap())
        })
        .collect();
    let mut wrong = Vec::new();
    let paths = programs(&bench.join("kernels")).unwrap();
    for path in &paths {
        let kernel = path.file_stem().unwrap().to_string_lossy().into_owned();
        let output = crate::executables::fennec_command()
            .args(["dev", "fallbacks", "--llvm", &path.to_string_lossy()])
            .output()
            .unwrap();
        assert!(output.status.success(), "{kernel}: {}", String::from_utf8_lossy(&output.stderr));
        let said = String::from_utf8_lossy(&output.stdout);
        let last = said.lines().last().unwrap_or("");
        let count = last.strip_suffix(" places aren't native").map_or(0, |count| count.parse().unwrap());
        match allowed.iter().find(|(name, _)| *name == kernel) {
            Some((_, limit)) if count == *limit => {}
            Some((_, limit)) => {
                wrong.push(format!("{kernel}: {count} fallbacks, the list says {limit}\n{said}"));
            }
            None => wrong.push(format!("{kernel}: {count} fallbacks, and it isn't on the list")),
        }
    }
    for (name, _) in &allowed {
        if !paths.iter().any(|path| path.file_stem().is_some_and(|stem| stem == name.as_str())) {
            wrong.push(format!("{name} is on the list, but isn't a kernel"));
        }
    }
    assert!(
        wrong.is_empty(),
        "bench/fallbacks.txt is out of date. A count that went up means LLVM stopped taking part of a \
         kernel; see `fennec dev fallbacks --llvm`. One that went down should be lowered there:\n{}",
        wrong.join("\n")
    );
}
