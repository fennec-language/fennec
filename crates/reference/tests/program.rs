//! `fennec-reference`, as the website's build runs it.

use std::process::Command;

#[test]
fn it_writes_the_page_and_fails_on_a_decision_that_isnt_there() {
    let dir =
        fennec_temp::Temp::at(std::env::temp_dir().join(format!("fennec-reference-{}", std::process::id())));
    for folder in ["docs", "examples", "site/src/pages"] {
        std::fs::create_dir_all(dir.join(folder)).unwrap();
    }
    std::fs::write(dir.join("docs/decisions.md"), "## Syntax\n\n**A rule.** Why.\n").unwrap();
    std::fs::write(dir.join("examples/one.fen"), "# decision: A rule.\nx = 1\n").unwrap();
    // A run that can't start says which of the two wasn't there: on 2026-11-15 one failed with
    // only "No such file or directory", once, and a sweep of old build files (fixed in #1821)
    // most likely took the program from under the gate.
    let run = || {
        let program = std::path::Path::new(env!("CARGO_BIN_EXE_fennec-reference"));
        Command::new(program).current_dir(&dir).output().unwrap_or_else(|err| {
            panic!(
                "couldn't run {} in {}: {err}; the program is {}there, and the folder is {}there",
                program.display(),
                dir.display(),
                if program.is_file() { "" } else { "not " },
                if dir.is_dir() { "" } else { "not " },
            )
        })
    };

    assert!(run().status.success());
    let page = std::fs::read_to_string(dir.join("site/src/pages/reference.md")).unwrap();
    assert!(page.contains("**A rule.**"), "{page}");

    std::fs::write(dir.join("examples/one.fen"), "# decision: Another rule.\nx = 1\n").unwrap();
    let stale = run();
    assert_eq!(stale.status.code(), Some(1));
    let said = String::from_utf8_lossy(&stale.stderr).into_owned();
    assert!(said.starts_with("error: one names a decision"), "{said}");
    std::fs::remove_dir_all(&dir).unwrap();
}
