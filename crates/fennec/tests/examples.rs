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
    let module = fennec_syntax::parse(source).map_err(|diagnostic| diagnostic.render(shown_as, source))?;
    fennec_check::check(&module).map_err(|diagnostics| {
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

#[test]
fn examples_print_their_expected_output() {
    let paths = programs(&examples_dir()).unwrap();
    assert!(!paths.is_empty(), "no examples found");
    for path in paths {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let source = fs::read_to_string(&path).unwrap();
        let expected = fs::read_to_string(path.with_extension("out")).unwrap();
        let actual =
            run(&source, &format!("examples/{name}")).unwrap_or_else(|err| panic!("{name} failed:\n{err}"));
        assert_eq!(actual, expected, "{name} printed something different");
        let failures = failing_tests(&source, &format!("examples/{name}")).unwrap();
        assert!(failures.is_empty(), "{name} has failing tests:\n{}", failures.join("\n"));
    }
}

#[test]
fn error_examples_fail_with_their_expected_message() {
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
}
