//! `fennec test` runs only what a change affects, reporting the rest as unchanged.

use std::path::{Path, PathBuf};
use std::process::Output;

const SOURCE: &str = "def double(x: int) -> int:\n    x * 2\n\ndef triple(x: int) -> int:\n    x * 3\n\n\
    test \"doubling\":\n    assert double(2) == 4\n\ntest \"tripling\":\n    assert triple(2) == 6\n";

/// A folder for a program and for Fennec's own files, with `SOURCE` written in it.
fn scratch(name: &str) -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("fennec-unchanged-{name}-{}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("game.fen"), SOURCE)?;
    Ok(dir)
}

/// Runs `fennec test` on the program, with the cache in `dir` and the environment of a
/// person's machine rather than CI's.
fn run_tests(dir: &Path, args: &[&str]) -> std::io::Result<Output> {
    crate::executables::fennec_command()
        .arg("test")
        .args(args)
        .arg(dir.join("game.fen"))
        .env("FENNEC_HOME", dir.join("home"))
        .env_remove("CI")
        .env_remove("FENNEC_NO_CACHE")
        .output()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

#[test]
fn only_the_tests_a_change_affects_run_again() {
    let dir = scratch("affected").unwrap();
    let first = run_tests(&dir, &[]).unwrap();
    assert_eq!(stdout(&first), "2 passed, 0 failed\n");

    let again = run_tests(&dir, &[]).unwrap();
    assert_eq!(stdout(&again), "2 passed, 0 failed (0 run, 2 unchanged)\n");

    let reformatted = format!("# A comment.\n\n{SOURCE}");
    std::fs::write(dir.join("game.fen"), reformatted).unwrap();
    let comment = run_tests(&dir, &[]).unwrap();
    assert_eq!(stdout(&comment), "2 passed, 0 failed (0 run, 2 unchanged)\n");

    std::fs::write(dir.join("game.fen"), SOURCE.replace("x * 2", "x + x")).unwrap();
    let edited = run_tests(&dir, &[]).unwrap();
    assert_eq!(stdout(&edited), "2 passed, 0 failed (1 run, 1 unchanged)\n");

    let all = run_tests(&dir, &["--all"]).unwrap();
    assert_eq!(stdout(&all), "2 passed, 0 failed\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_failing_test_runs_every_time_and_a_break_is_never_hidden() {
    let dir = scratch("failing").unwrap();
    run_tests(&dir, &[]).unwrap();

    std::fs::write(dir.join("game.fen"), SOURCE.replace("x * 2", "x * 5")).unwrap();
    let broken = run_tests(&dir, &[]).unwrap();
    assert_eq!(broken.status.code(), Some(1));
    assert_eq!(stdout(&broken), "  ✗ doubling\n1 passed, 1 failed (1 run, 1 unchanged)\n");

    let still = run_tests(&dir, &[]).unwrap();
    assert_eq!(still.status.code(), Some(1));
    assert_eq!(stdout(&still), "  ✗ doubling\n1 passed, 1 failed (1 run, 1 unchanged)\n");

    // Going back to code that passed still has to run, since the failure replaced its result.
    std::fs::write(dir.join("game.fen"), SOURCE).unwrap();
    let fixed = run_tests(&dir, &[]).unwrap();
    assert_eq!(stdout(&fixed), "2 passed, 0 failed (1 run, 1 unchanged)\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn ci_reuses_what_passed_like_anywhere_else() {
    let dir = scratch("ci").unwrap();
    run_tests(&dir, &[]).unwrap();
    let output = crate::executables::fennec_command()
        .arg("test")
        .arg(dir.join("game.fen"))
        .env("FENNEC_HOME", dir.join("home"))
        .env("CI", "true")
        .env_remove("FENNEC_NO_CACHE")
        .output()
        .unwrap();
    assert_eq!(stdout(&output), "2 passed, 0 failed (0 run, 2 unchanged)\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_change_in_another_file_of_a_kit_reaches_tests_through_aliases() {
    let dir = scratch("kit").unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let manifest =
        format!("name = \"demo\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n", env!("CARGO_PKG_VERSION"));
    std::fs::write(dir.join("kit.toml"), manifest).unwrap();
    std::fs::write(dir.join("src/maths.fen"), "def double(x: int) -> int:\n    x * 2\n").unwrap();
    std::fs::write(dir.join("src/other.fen"), "def one() -> int:\n    1\n").unwrap();
    let tests = "from maths import double as twice\nimport maths as numbers\nfrom other import one\n\n\
        test \"by alias\":\n    assert twice(2) == 4\n\ntest \"by module\":\n    assert numbers.double(1) == 2\n\n\
        test \"elsewhere\":\n    assert one() == 1\n";
    std::fs::write(dir.join("src/main.fen"), tests).unwrap();
    let run = || {
        crate::executables::fennec_command()
            .arg("test")
            .arg(&dir)
            .env("FENNEC_HOME", dir.join("home"))
            .env_remove("CI")
            .env_remove("FENNEC_NO_CACHE")
            .output()
            .unwrap()
    };
    assert!(stdout(&run()).ends_with("3 passed, 0 failed\n"));
    std::fs::write(dir.join("src/maths.fen"), "def double(x: int) -> int:\n    x + x\n").unwrap();
    let edited = stdout(&run());
    assert_eq!(edited, "3 passed, 0 failed (2 run, 1 unchanged)\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A program whose test takes one branch of a function, written to `dir`.
fn with_branches(dir: &Path, taken: &str, untaken: &str, interface: &str) -> std::io::Result<()> {
    let source = format!(
        "def taken() -> int:\n    {taken}\n\ndef untaken() -> int:\n    {untaken}\n\n\
         def choose(x: int) -> int:\n    if x > 0:\n        return taken()\n    untaken()\n\n\
         interface Sized:\n    def size(self) -> int\n\n\
         struct Box:\n    n: int\n\n    def size(self) -> int:\n        {interface}\n\n\
         def size_of[T: Sized](s: T) -> int:\n    s.size()\n\n\
         test \"branch\":\n    assert choose(1) == 1\n\n\
         test \"through an interface\":\n    assert size_of(Box(n=1)) == 1\n"
    );
    std::fs::write(dir.join("game.fen"), source)
}

#[test]
fn only_what_a_test_ran_can_make_it_run_again() {
    let dir = scratch("traced").unwrap();
    with_branches(&dir, "1", "2", "self.n").unwrap();
    assert_eq!(stdout(&run_tests(&dir, &[]).unwrap()), "2 passed, 0 failed\n");

    with_branches(&dir, "1", "3", "self.n").unwrap();
    let branch = run_tests(&dir, &[]).unwrap();
    assert_eq!(stdout(&branch), "2 passed, 0 failed (0 run, 2 unchanged)\n", "a branch it never takes");

    with_branches(&dir, "1 + 0", "3", "self.n").unwrap();
    let taken = run_tests(&dir, &[]).unwrap();
    assert_eq!(stdout(&taken), "2 passed, 0 failed (1 run, 1 unchanged)\n", "a function it calls");

    with_branches(&dir, "1 + 0", "3", "self.n + 0").unwrap();
    let method = run_tests(&dir, &[]).unwrap();
    assert_eq!(stdout(&method), "2 passed, 0 failed (1 run, 1 unchanged)\n", "a method it finds at run time");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_test_of_inlined_code_still_knows_what_it_ran() {
    let dir = scratch("inlined").unwrap();
    let source = |body: &str| {
        format!(
            "def small(x: int) -> int:\n    x + {body}\n\ndef twice(x: int) -> int:\n    small(x) + small(x)\n\n\
             test \"t\":\n    assert twice(1) > 0\n"
        )
    };
    std::fs::write(dir.join("game.fen"), source("1")).unwrap();
    run_tests(&dir, &[]).unwrap();
    std::fs::write(dir.join("game.fen"), source("2")).unwrap();
    assert_eq!(stdout(&run_tests(&dir, &[]).unwrap()), "1 passed, 0 failed\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_property_test_depends_on_all_it_can_reach() {
    let dir = scratch("property").unwrap();
    let source = |untaken: &str| {
        format!(
            "def untaken() -> int:\n    {untaken}\n\ndef choose(x: int) -> int:\n    if x > 1000000:\n        return untaken()\n    x\n\n\
             test \"any\" for x: int:\n    assert choose(0) == 0\n"
        )
    };
    std::fs::write(dir.join("game.fen"), source("1")).unwrap();
    run_tests(&dir, &[]).unwrap();
    assert!(stdout(&run_tests(&dir, &[]).unwrap()).contains("(0 run, 1 unchanged)"));
    std::fs::write(dir.join("game.fen"), source("2")).unwrap();
    assert_eq!(stdout(&run_tests(&dir, &[]).unwrap()), "1 passed, 0 failed\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A kit in a folder of its own, with a function called from a test in another file and a test
/// that doesn't touch it.
fn kit_with_tests(name: &str) -> std::io::Result<PathBuf> {
    let dir = scratch(name)?;
    std::fs::create_dir_all(dir.join("src"))?;
    let manifest =
        format!("name = \"demo\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n", env!("CARGO_PKG_VERSION"));
    std::fs::write(dir.join("kit.toml"), manifest)?;
    std::fs::write(dir.join("src/maths.fen"), "def double(x: int) -> int:\n    x * 2\n")?;
    std::fs::write(
        dir.join("src/uses.fen"),
        "from maths import double\n\ndef twice(x: int) -> int:\n    double(x)\n\n\
         test \"twice\":\n    assert twice(2) == 4\n",
    )?;
    std::fs::write(dir.join("src/other.fen"), "test \"other\":\n    assert 1 == 1\n")?;
    Ok(dir)
}

fn test_kit(dir: &Path) -> std::io::Result<Output> {
    crate::executables::fennec_command()
        .arg("test")
        .arg(dir)
        .env("FENNEC_HOME", dir.join("home"))
        .env_remove("CI")
        .env_remove("FENNEC_NO_CACHE")
        .output()
}

#[test]
fn an_edit_to_an_embedded_file_runs_only_the_tests_that_use_it() {
    let dir = kit_with_tests("kit_embed").unwrap();
    std::fs::write(dir.join("src/data.bin"), [1]).unwrap();
    let uses = "DATA = embed(\"data.bin\")\n\ntest \"the data\":\n    assert DATA.len() == 1\n";
    std::fs::write(dir.join("src/with_data.fen"), uses).unwrap();
    assert_eq!(stdout(&test_kit(&dir).unwrap()), "3 passed, 0 failed\n");
    assert_eq!(stdout(&test_kit(&dir).unwrap()), "3 passed, 0 failed (0 run, 3 unchanged)\n");

    std::fs::write(dir.join("src/data.bin"), [2]).unwrap();
    assert_eq!(stdout(&test_kit(&dir).unwrap()), "3 passed, 0 failed (1 run, 2 unchanged)\n");
    assert_eq!(stdout(&test_kit(&dir).unwrap()), "3 passed, 0 failed (0 run, 3 unchanged)\n");

    std::fs::write(dir.join("src/data.bin"), [2, 3]).unwrap();
    let failed = test_kit(&dir).unwrap();
    assert_eq!(failed.status.code(), Some(1));
    assert_eq!(stdout(&failed), "  ✗ the data\n2 passed, 1 failed (1 run, 2 unchanged)\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_edit_to_a_body_runs_only_what_ran_it_across_files() {
    let dir = kit_with_tests("kit_body").unwrap();
    assert_eq!(stdout(&test_kit(&dir).unwrap()), "2 passed, 0 failed\n");
    assert_eq!(stdout(&test_kit(&dir).unwrap()), "2 passed, 0 failed (0 run, 2 unchanged)\n");

    std::fs::write(dir.join("src/maths.fen"), "def double(x: int) -> int:\n    x + x\n").unwrap();
    assert_eq!(stdout(&test_kit(&dir).unwrap()), "2 passed, 0 failed (1 run, 1 unchanged)\n");
    assert_eq!(stdout(&test_kit(&dir).unwrap()), "2 passed, 0 failed (0 run, 2 unchanged)\n");

    std::fs::write(dir.join("src/maths.fen"), "def double(x: int) -> int:\n    x * 3\n").unwrap();
    let broken = test_kit(&dir).unwrap();
    assert_eq!(broken.status.code(), Some(1));
    assert_eq!(stdout(&broken), "  ✗ twice\n1 passed, 1 failed (1 run, 1 unchanged)\n");

    std::fs::write(dir.join("src/maths.fen"), "def double(x: int) -> int:\n    x + x\n").unwrap();
    assert!(test_kit(&dir).unwrap().status.success());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_edit_that_breaks_a_caller_in_another_file_is_still_found() {
    for (edit, what) in [
        ("def double(x: float) -> float:\n    x * 2.0\n", "a changed signature"),
        (
            "error Bad:\n    n: int\n\ndef double(x: int) -> int:\n    if x > 9: fail Bad(n=x)\n    x * 2\n",
            "a body that can now fail",
        ),
    ] {
        let dir = kit_with_tests("kit_caller").unwrap();
        assert!(test_kit(&dir).unwrap().status.success());
        std::fs::write(dir.join("src/maths.fen"), edit).unwrap();
        let broken = test_kit(&dir).unwrap();
        assert_eq!(broken.status.code(), Some(1), "{what}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[test]
fn a_mistake_in_an_edited_body_is_reported() {
    let dir = kit_with_tests("kit_mistake").unwrap();
    assert!(test_kit(&dir).unwrap().status.success());
    std::fs::write(dir.join("src/maths.fen"), "def double(x: int) -> int:\n    x + \"a\"\n").unwrap();
    let broken = test_kit(&dir).unwrap();
    assert_eq!(broken.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&broken.stderr).starts_with("error"), "{broken:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_new_file_and_a_new_test_are_noticed() {
    let dir = kit_with_tests("kit_new").unwrap();
    assert!(test_kit(&dir).unwrap().status.success());
    std::fs::write(dir.join("src/third.fen"), "test \"third\":\n    assert 1 == 2\n").unwrap();
    assert_eq!(test_kit(&dir).unwrap().status.code(), Some(1));
    std::fs::remove_file(dir.join("src/third.fen")).unwrap();
    assert!(test_kit(&dir).unwrap().status.success());
    let added = "test \"other\":\n    assert 1 == 1\n\ntest \"new\":\n    assert 1 == 2\n";
    std::fs::write(dir.join("src/other.fen"), added).unwrap();
    assert_eq!(test_kit(&dir).unwrap().status.code(), Some(1));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `double` with a doc comment holding the given example.
fn documented(example: &str) -> String {
    format!(
        "## Doubles a number.\n##\n## ```\n{example}## ```\ndef double(x: int) -> int:\n    x * 2\n\n\
         test \"doubling\":\n    assert double(2) == 4\n"
    )
}

#[test]
fn a_doc_example_runs_as_a_test_and_can_fail() {
    let dir = scratch("doc-example").unwrap();
    let game = dir.join("game.fen");
    std::fs::write(&game, documented("## assert double(4) == 8\n")).unwrap();
    let passing = run_tests(&dir, &[]).unwrap();
    assert_eq!(stdout(&passing), "2 passed, 0 failed\n", "{}", String::from_utf8_lossy(&passing.stderr));

    std::fs::write(&game, documented("## assert double(4) == 9\n")).unwrap();
    let failing = run_tests(&dir, &[]).unwrap();
    assert!(!failing.status.success(), "a failing example fails the run");
    let said = format!("{}{}", stdout(&failing), String::from_utf8_lossy(&failing.stderr));
    assert!(said.contains("1 passed, 1 failed"), "{said}");
    assert!(said.contains("doc example 1 in"), "it says which: {said}");
    assert!(said.contains("game.fen:4"), "it points at the example's line: {said}");

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_doc_example_runs_again_only_when_it_or_what_it_ran_changes() {
    let dir = scratch("doc-example-cache").unwrap();
    let game = dir.join("game.fen");
    let source = documented("## assert double(4) == 8\n");
    std::fs::write(&game, &source).unwrap();
    assert_eq!(stdout(&run_tests(&dir, &[]).unwrap()), "2 passed, 0 failed\n");
    assert_eq!(stdout(&run_tests(&dir, &[]).unwrap()), "2 passed, 0 failed (0 run, 2 unchanged)\n");

    std::fs::write(&game, source.replace("Doubles a number.", "Twice a number.")).unwrap();
    assert_eq!(
        stdout(&run_tests(&dir, &[]).unwrap()),
        "2 passed, 0 failed (0 run, 2 unchanged)\n",
        "only the words changed"
    );

    let edited = source.replace("double(4) == 8", "double(5) == 10");
    std::fs::write(&game, &edited).unwrap();
    assert_eq!(stdout(&run_tests(&dir, &[]).unwrap()), "2 passed, 0 failed (1 run, 1 unchanged)\n");

    std::fs::write(&game, edited.replace("x * 2", "x + x")).unwrap();
    assert_eq!(stdout(&run_tests(&dir, &[]).unwrap()), "2 passed, 0 failed\n", "both ran `double`");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn only_a_closed_fence_of_fennec_in_a_doc_comment_is_an_example() {
    let dir = scratch("doc-example-kinds").unwrap();
    let game = dir.join("game.fen");
    let write = |doc: &str| {
        let source = format!(
            "{doc}def double(x: int) -> int:\n    x * 2\n\ntest \"doubling\":\n    assert double(2) == 4\n"
        );
        std::fs::write(&game, source)
    };
    // Another language is only text, whatever it holds.
    write("## ```json\n## not fennec at all\n## ```\n").unwrap();
    assert_eq!(stdout(&run_tests(&dir, &["--all"]).unwrap()), "1 passed, 0 failed\n");
    // A fence named for Fennec runs, and so does one that's indented with what it documents.
    write("## ```fennec\n## assert double(1) == 2\n## ```\n").unwrap();
    assert_eq!(stdout(&run_tests(&dir, &["--all"]).unwrap()), "2 passed, 0 failed\n");
    // A plain comment isn't a doc comment.
    write("# ```\n# assert False\n# ```\n").unwrap();
    assert_eq!(stdout(&run_tests(&dir, &["--all"]).unwrap()), "1 passed, 0 failed\n");
    // Code that can't run is a mistake rather than something to skip.
    write("## ```\n## assert (\n## ```\n").unwrap();
    assert!(!run_tests(&dir, &["--all"]).unwrap().status.success());
    write("## ```\n## assert missing == 1\n## ```\n").unwrap();
    assert!(!run_tests(&dir, &["--all"]).unwrap().status.success());
    // As is a fence that's never closed, which would otherwise hide what's in it.
    write("## ```\n## assert False\n").unwrap();
    let unclosed = run_tests(&dir, &["--all"]).unwrap();
    assert!(!unclosed.status.success());
    assert!(String::from_utf8_lossy(&unclosed.stderr).contains("never closed"));
    // Text that looks like one in a string, or a comment after code, isn't one.
    let text = "def words() -> str:\n    \"## ```\" ## ```\n\ntest \"words\":\n    assert words() != \"\"\n";
    std::fs::write(&game, text).unwrap();
    let in_a_string = run_tests(&dir, &["--all"]).unwrap();
    assert_eq!(
        stdout(&in_a_string),
        "1 passed, 0 failed\n",
        "{}",
        String::from_utf8_lossy(&in_a_string.stderr)
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_doc_example_in_a_kit_sees_what_its_file_sees_and_a_run_does_not_need_it() {
    let dir = kit_with_tests("doc-example-kit").unwrap();
    let helpers = dir.join("src/maths.fen");
    let text = std::fs::read_to_string(&helpers).unwrap();
    let documented = format!(
        "{text}\n## A private helper's example.\n##\n## ```\n## assert hidden() == 3\n## ```\ndef hidden() -> int:\n    3\n"
    );
    std::fs::write(&helpers, documented).unwrap();
    let first = test_kit(&dir).unwrap();
    let said = stdout(&first);
    assert!(
        first.status.success() && said.contains("0 failed"),
        "{said}{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let again = test_kit(&dir).unwrap();
    assert!(stdout(&again).contains("(0 run,"), "{}", stdout(&again));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Whether `fennec test --llvm` can build through LLVM here: it needs the tools and the runtime
/// library, and a run under coverage has `fennec` somewhere without one. CI's Linux jobs set
/// `FENNEC_EXPECT_LLVM`, so these tests can't quietly stop running there.
fn llvm_here() -> bool {
    let ready = !cfg!(windows)
        && std::env::var_os("FENNEC_UNDER_MUTANTS").is_none()
        && fennec_llvm::Tools::find().is_ok()
        && crate::executables::runtime_built()
        && Path::new(env!("CARGO_BIN_EXE_fennec")).with_file_name("libfennec_runtime.a").is_file();
    let expected = std::env::var_os("FENNEC_EXPECT_LLVM").is_some_and(|expected| !expected.is_empty());
    assert!(ready || !expected, "LLVM's tools or the runtime library aren't here");
    ready
}

#[test]
fn tests_through_llvm_pass_and_fail_as_they_do_on_the_jit() {
    if !llvm_here() {
        return;
    }
    let dir = scratch("llvm-tests").unwrap();
    let game = dir.join("game.fen");
    let source = |wrong: &str| {
        format!(
            "struct Unit:\n    size: int\n\ndef grown(unit: Unit) -> int:\n    unit.size + 1\n\n\
             ## An example.\n##\n## ```\n## assert double(2) == 4\n## ```\ndef double(x: int) -> int:\n    x * 2\n\n\
             test \"struct\":\n    assert grown(Unit(size=1)) == 2\n\n\
             test \"list\":\n    numbers = [1, 2, 3]\n    assert sum(numbers) == 6\n\n\
             test \"may fail\":\n    assert double(2) == {wrong}\n\n\
             test \"takes a capability\" (stdout: Stdout):\n    stdout.print(1)\n\n\
             test \"any\" for x: int:\n    assert double(x) == x + x\n"
        )
    };
    std::fs::write(&game, source("4")).unwrap();
    let passing = run_tests(&dir, &["--all", "--llvm"]).unwrap();
    assert_eq!(stdout(&passing), "6 passed, 0 failed\n", "{}", String::from_utf8_lossy(&passing.stderr));

    std::fs::write(&game, source("5")).unwrap();
    let jit = run_tests(&dir, &["--all"]).unwrap();
    let llvm = run_tests(&dir, &["--all", "--llvm"]).unwrap();
    assert_eq!(stdout(&llvm), stdout(&jit));
    assert_eq!(String::from_utf8_lossy(&llvm.stderr), String::from_utf8_lossy(&jit.stderr));
    assert!(!llvm.status.success());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_test_llvm_cannot_build_runs_another_way() {
    if !llvm_here() {
        return;
    }
    let dir = scratch("llvm-tests-refused").unwrap();
    let game = dir.join("game.fen");
    // LLVM doesn't take a dict of lists yet. When it does, this still has to pass.
    let text = "def lengths() -> dict[str, list[int]]:\n    {\"a\": [1, 2]}\n\ntest \"plain\":\n    assert 1 + 1 == 2\n\n\
                test \"a dict of lists\":\n    assert lengths()[\"a\"].len() == 2\n";
    std::fs::write(&game, text).unwrap();
    let ran = run_tests(&dir, &["--all", "--llvm"]).unwrap();
    assert_eq!(stdout(&ran), "2 passed, 0 failed\n", "{}", String::from_utf8_lossy(&ran.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// What `fennec test` says about a program, without the note about tests LLVM couldn't build.
fn tested(program: &Path, args: &[&str], home: &Path) -> std::io::Result<(String, String)> {
    let output = crate::executables::fennec_command()
        .arg("test")
        .args(args)
        .arg(program)
        .env("FENNEC_HOME", home)
        .env_remove("CI")
        .output()?;
    let said = String::from_utf8_lossy(&output.stderr);
    let kept: Vec<&str> = said.lines().filter(|line| !line.starts_with("note: ")).collect();
    Ok((String::from_utf8_lossy(&output.stdout).into_owned(), kept.join("\n")))
}

/// The functions each test ran, as `fennec test` kept them for a program.
fn kept_ran(
    home: &Path,
    program: &Path,
) -> std::io::Result<std::collections::HashMap<String, Option<Vec<String>>>> {
    let absolute = std::fs::canonicalize(program)?.display().to_string();
    let records = fennec_build::Cache::new(home).test_records(&absolute);
    Ok(records.into_iter().map(|(name, record)| (name, record.ran)).collect())
}

/// Every example with tests says the same whichever way its tests run. And a wrong "unchanged" is
/// a test skipped that should have run, so everything the JIT says a test ran, LLVM has to say too.
#[test]
fn every_example_tested_through_llvm_says_and_traces_what_it_does_on_the_jit() {
    if !llvm_here() {
        return;
    }
    let home = std::env::temp_dir().join(format!("fennec-unchanged-llvm-examples-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let (mut programs, mut compared, mut traced) = (0, 0, 0);
    for entry in std::fs::read_dir(&examples).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|ext| ext != "fen") {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        if !source.lines().any(|line| line.trim_start().starts_with("test \"")) {
            continue;
        }
        programs += 1;
        let on_the_jit = tested(&path, &["--all"], &home).unwrap();
        let jit = kept_ran(&home, &path).unwrap();
        let through_llvm = tested(&path, &["--all", "--llvm"], &home).unwrap();
        let llvm = kept_ran(&home, &path).unwrap();
        assert_eq!(through_llvm, on_the_jit, "{} says something else through LLVM", path.display());
        for (test, ran) in &jit {
            let Some(ran) = ran else { continue };
            compared += 1;
            let Some(Some(ran_through_llvm)) = llvm.get(test) else { continue };
            traced += 1;
            let missing: Vec<_> = ran.iter().filter(|name| !ran_through_llvm.contains(name)).collect();
            assert!(
                missing.is_empty(),
                "{}: `{test}` ran {missing:?} on the JIT, and LLVM doesn't say so",
                path.display()
            );
        }
    }
    println!("{programs} examples, {traced} of {compared} tests' traces compared");
    assert!(programs > 20, "only {programs} examples have tests");
    assert!(traced > 20, "only {traced} of {compared} tests were compared");
    std::fs::remove_dir_all(&home).unwrap();
}

/// As on the JIT, a test isn't run again for a change to code it never reached.
#[test]
fn only_what_a_test_ran_through_llvm_can_make_it_run_again() {
    if !llvm_here() {
        return;
    }
    let dir = scratch("llvm-ran").unwrap();
    let game = dir.join("game.fen");
    let source = |untaken: &str, taken: &str| {
        format!(
            "def untaken() -> int:\n    {untaken}\n\ndef taken() -> int:\n    {taken}\n\n\
             def choose(x: int) -> int:\n    if x > 1000000:\n        return untaken()\n    taken()\n\n\
             test \"small\":\n    assert choose(0) >= 1\n"
        )
    };
    std::fs::write(&game, source("1", "1")).unwrap();
    assert_eq!(stdout(&run_tests(&dir, &["--llvm"]).unwrap()), "1 passed, 0 failed\n");
    std::fs::write(&game, source("2", "1")).unwrap();
    assert_eq!(
        stdout(&run_tests(&dir, &["--llvm"]).unwrap()),
        "1 passed, 0 failed (0 run, 1 unchanged)\n",
        "it never ran `untaken`"
    );
    std::fs::write(&game, source("2", "2")).unwrap();
    assert_eq!(stdout(&run_tests(&dir, &["--llvm"]).unwrap()), "1 passed, 0 failed\n", "it ran `taken`");
    std::fs::remove_dir_all(&dir).unwrap();
}
