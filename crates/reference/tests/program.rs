//! `fennec-reference`, as the website's build runs it.

use std::process::Command;

#[test]
fn it_writes_the_page_and_fails_on_a_decision_that_isnt_there() {
    let dir = std::env::temp_dir().join(format!("fennec-reference-{}", std::process::id()));
    for folder in ["docs", "examples", "site/src/pages"] {
        std::fs::create_dir_all(dir.join(folder)).unwrap();
    }
    std::fs::write(dir.join("docs/decisions.md"), "## Syntax\n\n**A rule.** Why.\n").unwrap();
    std::fs::write(dir.join("examples/one.fen"), "# decision: A rule.\nx = 1\n").unwrap();
    let run = || Command::new(env!("CARGO_BIN_EXE_fennec-reference")).current_dir(&dir).output().unwrap();

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
