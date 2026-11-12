//! Every program in `examples/errors/` that the checker catches fails with exactly its `.err`
//! file. `llvm.rs` holds every example to its `.out`, and `executables.rs` holds what fails while
//! running to its `.err`. The examples are the language's working specification, so the site and
//! the compiler can't drift apart.

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

/// What the checker says of a program, rendered, or `None` when it passes.
fn check_errors(source: &str, shown_as: &str) -> Option<String> {
    let mut module = match fennec_syntax::parse(source) {
        Ok(module) => module,
        Err(diagnostic) => return Some(diagnostic.render(shown_as, source)),
    };
    let diagnostics = fennec_check::check_and_resolve(&mut module).err()?;
    let rendered: Vec<String> =
        diagnostics.iter().map(|diagnostic| diagnostic.render(shown_as, source)).collect();
    Some(rendered.join("\n"))
}

/// One a program can only make at run time is `executables.rs`'s to compare.
#[test]
fn error_examples_the_checker_catches_fail_with_their_expected_message() {
    let paths = programs(&examples_dir().join("errors")).unwrap();
    assert!(!paths.is_empty(), "no error examples found");
    let mut caught = 0;
    for path in paths {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let source = fs::read_to_string(&path).unwrap();
        let expected = fs::read_to_string(path.with_extension("err")).unwrap();
        if let Some(actual) = check_errors(&source, &format!("examples/errors/{name}")) {
            assert_eq!(actual, expected, "{name} failed with a different message");
            caught += 1;
        }
    }
    assert!(caught > 100, "only {caught} error examples were caught by the checker");
}

#[test]
fn a_run_checking_only_what_it_reaches_finds_the_same_mistakes() {
    let mut compared = 0;
    for path in programs(&examples_dir().join("errors")).unwrap() {
        let source = fs::read_to_string(&path).unwrap();
        // An example of a mistake the parser finds has nothing for either check to look at.
        let Ok(mut module) = fennec_syntax::parse(&source) else { continue };
        let whole = fennec_check::check(&module).err();
        let reached = fennec_check::check_to_run(&mut module).err();
        assert_eq!(reached, whole, "{} found different mistakes", path.display());
        compared += 1;
    }
    // Skipping every example, as a parser change could make it, would pass with nothing compared.
    assert!(compared > 200, "only {compared} error examples parsed, so only they were compared");
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
