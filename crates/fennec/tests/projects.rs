//! Every kit in `examples/projects/` runs and prints exactly its `.out` file, in the
//! interpreter and the JIT, and passes its tests. Every kit in `examples/projects/errors/`
//! fails with exactly its `.err` file, which names the file the problem is in.

use std::fs;
use std::path::{Path, PathBuf};

use fennec_jit::JitError;
use fennec_kit::{Entry, Program};

fn projects_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/projects")
}

/// The kits in a folder, by name.
fn kits(dir: &Path) -> std::io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if let Some(name) = path.file_name().filter(|_| path.join("kit.toml").is_file()) {
            names.push(name.to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

fn load(dir: &Path, shown_as: &str, entry: Entry) -> Result<Program, String> {
    let mut program = fennec_kit::load(dir, shown_as, entry)?;
    fennec_check::check_and_resolve(&mut program.module)
        .map_err(|diagnostics| program.sources.render_all(&diagnostics))?;
    Ok(program)
}

/// Runs a kit the way `fennec run` does: its output, or its rendered errors.
fn run(dir: &Path, shown_as: &str) -> Result<String, String> {
    let program = load(dir, shown_as, Entry::Main)?;
    let mut out = Vec::new();
    fennec_interp::run(&program.module, &mut out)
        .map_err(|diagnostic| program.sources.render(&diagnostic))?;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// Runs a kit as native code, the way `fennec run --jit` does.
fn run_jit(dir: &Path, shown_as: &str) -> Result<String, String> {
    let program = load(dir, shown_as, Entry::Main)?;
    let mut out = Vec::new();
    match fennec_jit::run_with_report(&program.module, &mut out) {
        Ok(report) => {
            assert_eq!(
                (report.leftover_temps, report.leftover_frames),
                (0, 0),
                "{shown_as} left things behind"
            );
            Ok(String::from_utf8_lossy(&out).into_owned())
        }
        Err(JitError::Failed(diagnostic)) => Err(program.sources.render(&diagnostic)),
        Err(JitError::Unsupported(why)) => {
            Err(format!("{shown_as} should compile natively, but: {}", why.message))
        }
    }
}

/// Runs a test on a stack as big as `fennec run` gives programs.
fn on_big_stack(test: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    let worker = std::thread::Builder::new().stack_size(512 * 1024 * 1024).spawn(test)?;
    if let Err(panic) = worker.join() {
        std::panic::resume_unwind(panic);
    }
    Ok(())
}

#[test]
fn projects_print_their_expected_output_in_every_engine() {
    on_big_stack(|| {
        let dir = projects_dir();
        let names = kits(&dir).unwrap();
        assert!(!names.is_empty(), "no projects found");
        for name in names {
            let shown_as = format!("examples/projects/{name}");
            let expected = fs::read_to_string(dir.join(format!("{name}.out"))).unwrap();
            let actual =
                run(&dir.join(&name), &shown_as).unwrap_or_else(|err| panic!("{name} failed:\n{err}"));
            assert_eq!(actual, expected, "{name} printed something different");
            assert_eq!(
                run_jit(&dir.join(&name), &shown_as),
                Ok(expected),
                "{name} ran differently as native code"
            );

            let program = load(&dir.join(&name), &shown_as, Entry::Everything).unwrap();
            let mut out = std::io::sink();
            let mut interp = fennec_interp::Interp::new(&mut out).unwrap();
            interp.load(&program.module).unwrap();
            let outcomes = interp.run_tests();
            assert!(!outcomes.is_empty(), "{name} has no tests");
            for outcome in outcomes {
                assert!(outcome.failure.is_none(), "{name}: test {:?} failed", outcome.name);
            }
        }
    })
    .unwrap();
}

#[test]
fn error_projects_fail_with_their_expected_message_in_every_engine() {
    on_big_stack(|| {
        let dir = projects_dir().join("errors");
        let names = kits(&dir).unwrap();
        assert!(!names.is_empty(), "no error projects found");
        for name in names {
            let shown_as = format!("examples/projects/errors/{name}");
            let expected = fs::read_to_string(dir.join(format!("{name}.err"))).unwrap();
            let actual = run(&dir.join(&name), &shown_as).expect_err(&format!("{name} should fail"));
            assert_eq!(actual, expected, "{name} failed with a different message");
            let native =
                run_jit(&dir.join(&name), &shown_as).expect_err(&format!("{name} should fail natively"));
            assert_eq!(native, expected, "{name} failed differently as native code");
        }
    })
    .unwrap();
}

#[test]
fn kit_toml_takes_comments_and_an_empty_dependencies_table() {
    let text = "# my game\nname = \"arena\"  # its name\nversion = \"1.2.3\"\nfennec = \"0.0.1\"\n\n[dependencies]\n";
    let manifest = fennec_kit::Manifest::parse(text).unwrap();
    assert_eq!(
        (manifest.name.as_str(), manifest.version.as_str(), manifest.fennec.as_str()),
        ("arena", "1.2.3", "0.0.1")
    );
}

#[test]
fn kit_toml_reports_each_mistake() {
    let messages = |text: &str| -> Vec<String> {
        fennec_kit::Manifest::parse(text)
            .unwrap_err()
            .into_iter()
            .map(|diagnostic| diagnostic.message)
            .collect()
    };
    let valid = "name = \"arena\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n";
    assert_eq!(messages(&valid.replace("arena", "Arena")), ["`Arena` can't name a kit"]);
    assert_eq!(messages(&valid.replace("arena", "star-fox")), ["`star-fox` can't name a kit"]);
    assert_eq!(messages(&valid.replace("arena", "9lives")), ["`9lives` can't name a kit"]);
    for version in ["1.0", "1.0.0.0", "+1.0.0", "1..0", "a.b.c"] {
        assert_eq!(messages(&valid.replace("0.1.0", version)), [format!("`{version}` isn't a version")]);
    }
    assert_eq!(
        messages(&valid.replace("0.1.0", "0.1#0")),
        ["`0.1#0` isn't a version"],
        "a # in a string isn't a comment"
    );
    assert_eq!(
        messages(&valid.replace("0.0.1", "99.0.0")),
        [format!("this kit needs Fennec 99.0.0, and this is Fennec {}", env!("CARGO_PKG_VERSION"))]
    );
    assert_eq!(
        messages(&valid.replace("\"arena\"", "arena")),
        ["`name` should be a string in double quotes"]
    );
    assert_eq!(messages(&format!("{valid}name = \"other\"\n")), ["`name` is set twice"]);
    assert_eq!(messages(&format!("{valid}[package]\n")), ["kit.toml has no table `[package]`"]);
    assert_eq!(messages(&format!("{valid}just words\n")), ["expected `key = \"value\"` or a `[table]`"]);
    assert_eq!(messages("fennec = \"0.0.1\"\n"), ["kit.toml needs `name`", "kit.toml needs `version`"]);
}

#[test]
fn kit_toml_errors_point_at_their_line() {
    let text = "name = \"arena\"\n  colour = \"red\"  # indented\n";
    let errors = fennec_kit::Manifest::parse(text).unwrap_err();
    let start = text.find("colour").unwrap();
    let end = text.find("  #").unwrap();
    assert_eq!(errors.first().map(|error| error.span), Some(fennec_syntax::Span::new(start, end)));
}
