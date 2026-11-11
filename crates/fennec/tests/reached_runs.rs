//! `fennec run` checks all of a program the first time and only what `main` reaches while its
//! text stays the same, which must never change what a program does or what's rejected.

use std::path::Path;
use std::process::Output;

const SOURCE: &str = "def used() -> int:\n    41 + 1\n\ndef unused() -> int:\n    2\n\n\
    def main(stdout: Stdout):\n    stdout.print(used())\n";

/// A folder for a program and for Fennec's own files, with `SOURCE` written in it.
fn scratch(name: &str) -> std::io::Result<fennec_temp::Temp> {
    let dir = fennec_temp::Temp::at(
        std::env::temp_dir().join(format!("fennec-reached-{name}-{}", std::process::id())),
    );
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("game.fen"), SOURCE)?;
    Ok(dir)
}

/// Runs a program with the cache in `home` and the environment of a person's machine.
fn run(home: &Path, program: &Path) -> std::io::Result<Output> {
    crate::executables::fennec_command()
        .arg("run")
        .arg(program)
        .env("FENNEC_HOME", home)
        .env_remove("CI")
        .env_remove("FENNEC_NO_CACHE")
        .output()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

#[test]
fn a_mistake_main_doesnt_reach_stops_the_run_even_after_a_run_that_passed() {
    let dir = scratch("mistake").unwrap();
    let (home, game) = (dir.join("home"), dir.join("game.fen"));
    for _ in 0..2 {
        assert_eq!(stdout(&run(&home, &game).unwrap()), "42\n");
    }
    std::fs::write(&game, SOURCE.replace("    2\n", "    \"two\"\n")).unwrap();
    for _ in 0..2 {
        let rejected = run(&home, &game).unwrap();
        assert!(!rejected.status.success());
        assert_eq!(stdout(&rejected), "");
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("error"));
    }
    std::fs::write(&game, SOURCE).unwrap();
    assert_eq!(stdout(&run(&home, &game).unwrap()), "42\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn what_runs_is_what_the_whole_program_does_in_every_example() {
    let dir = scratch("examples").unwrap();
    let projects = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/projects");
    let mut compared = 0;
    for entry in std::fs::read_dir(&projects).unwrap() {
        let kit = entry.unwrap().path();
        if !kit.join("kit.toml").is_file() {
            continue;
        }
        let expected = std::fs::read_to_string(kit.with_extension("out"))
            .or_else(|_| std::fs::read_to_string(kit.join("expected.out")))
            .unwrap()
            .replace("\r\n", "\n");
        for _ in 0..2 {
            assert_eq!(stdout(&run(&dir.join("home"), &kit).unwrap()), expected, "{}", kit.display());
        }
        compared += 1;
    }
    assert!(compared > 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

const TESTED: &str = "def double(x: int) -> int:\n    x * 2\n\ntest \"doubling\":\n    assert double(2) == 4\n\n\
    def main(stdout: Stdout):\n    stdout.print(double(21))\n";

#[test]
fn a_failing_test_stops_the_run_until_its_fixed_or_skipped() {
    let dir = scratch("tested").unwrap();
    let (home, game) = (dir.join("home"), dir.join("game.fen"));
    std::fs::write(&game, TESTED).unwrap();
    let passing = run(&home, &game).unwrap();
    assert_eq!(stdout(&passing), "42\n", "the tests print nothing when they pass");
    assert_eq!(String::from_utf8_lossy(&passing.stderr), "");

    std::fs::write(&game, TESTED.replace("x * 2", "x * 3")).unwrap();
    for _ in 0..2 {
        let stopped = run(&home, &game).unwrap();
        assert_eq!(stopped.status.code(), Some(1));
        assert_eq!(stdout(&stopped), "", "the program doesn't run");
        let said = String::from_utf8_lossy(&stopped.stderr).into_owned();
        assert!(said.contains("✗ doubling") && said.contains("--no-test"), "{said}");
    }

    let skipped = crate::executables::fennec_command()
        .args(["run", "--no-test"])
        .arg(&game)
        .env("FENNEC_HOME", &home)
        .env_remove("FENNEC_NO_CACHE")
        .output()
        .unwrap();
    assert_eq!(stdout(&skipped), "63\n");

    std::fs::write(&game, TESTED).unwrap();
    assert_eq!(stdout(&run(&home, &game).unwrap()), "42\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_kits_tests_run_first_even_in_files_main_doesnt_import() {
    let dir = scratch("kit_tested").unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let manifest =
        format!("name = \"demo\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n", env!("CARGO_PKG_VERSION"));
    std::fs::write(dir.join("kit.toml"), manifest).unwrap();
    std::fs::write(dir.join("src/main.fen"), "def main(stdout: Stdout):\n    stdout.print(1)\n").unwrap();
    std::fs::write(dir.join("src/other.fen"), "test \"other\":\n    assert 1 == 1\n").unwrap();
    let home = dir.join("home");
    assert_eq!(stdout(&run(&home, &dir).unwrap()), "1\n");
    std::fs::write(dir.join("src/other.fen"), "test \"other\":\n    assert 1 == 2\n").unwrap();
    let stopped = run(&home, &dir).unwrap();
    assert_eq!(stopped.status.code(), Some(1));
    assert_eq!(stdout(&stopped), "");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn no_test_doesnt_run_tests() {
    let dir = scratch("flags").unwrap();
    let (home, game) = (dir.join("home"), dir.join("game.fen"));
    std::fs::write(&game, TESTED.replace("x * 2", "x * 3")).unwrap();
    let ran = crate::executables::fennec_command()
        .args(["run", "--no-test"])
        .arg(&game)
        .env("FENNEC_HOME", &home)
        .output()
        .unwrap();
    assert_eq!(stdout(&ran), "63\n", "{}", String::from_utf8_lossy(&ran.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_kit_started_from_what_it_kept_behaves_the_same() {
    let dir = scratch("image").unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let manifest =
        format!("name = \"demo\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n", env!("CARGO_PKG_VERSION"));
    std::fs::write(dir.join("kit.toml"), manifest).unwrap();
    let lib = |value: &str| format!("def value() -> int:\n    {value}\n");
    std::fs::write(dir.join("src/lib.fen"), lib("1")).unwrap();
    let main = "from lib import value\n\ndef main(stdout: Stdout):\n    stdout.print(value())\n    stdout.print([1][value()])\n";
    std::fs::write(dir.join("src/main.fen"), main).unwrap();
    let home = dir.join("home");
    let shown = |output: &Output| String::from_utf8_lossy(&output.stderr).into_owned();
    let first = run(&home, &dir).unwrap();
    assert_eq!(first.status.code(), Some(1));
    assert!(shown(&first).contains("src/main.fen"), "{}", shown(&first));
    for _ in 0..2 {
        let again = run(&home, &dir).unwrap();
        assert_eq!(stdout(&again), stdout(&first));
        assert_eq!(shown(&again), shown(&first), "a failure points at the source just the same");
    }
    // Long enough that a file's time can be told from when it was noted.
    std::thread::sleep(std::time::Duration::from_millis(2_100));
    assert_eq!(stdout(&run(&home, &dir).unwrap()), stdout(&first));
    let noted = run(&home, &dir).unwrap();
    assert_eq!(shown(&noted), shown(&first), "started without reading anything");

    std::fs::write(dir.join("src/lib.fen"), lib("0")).unwrap();
    assert_eq!(stdout(&run(&home, &dir).unwrap()), "0\n1\n", "an edit is seen");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A kit whose `main` prints what it embeds, with `test` added to what it holds.
fn embedding_kit(name: &str, test: &str) -> std::io::Result<fennec_temp::Temp> {
    let dir = scratch(name)?;
    std::fs::create_dir_all(dir.join("src"))?;
    let manifest =
        format!("name = \"demo\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n", env!("CARGO_PKG_VERSION"));
    std::fs::write(dir.join("kit.toml"), manifest)?;
    std::fs::write(dir.join("src/data.bin"), [1, 2])?;
    let main = format!(
        "DATA = embed_file(\"data.bin\")\n\ndef main(stdout: Stdout):\n    stdout.print(DATA[0])\n\n{test}"
    );
    std::fs::write(dir.join("src/main.fen"), main)?;
    Ok(dir)
}

#[test]
fn an_edit_to_an_embedded_file_is_seen_by_a_kit_started_from_what_it_kept() {
    let dir = embedding_kit("embed_image", "").unwrap();
    let home = dir.join("home");
    assert_eq!(stdout(&run(&home, &dir).unwrap()), "1\n");
    // Long enough that a file's time can be told from when it was noted.
    std::thread::sleep(std::time::Duration::from_millis(2_100));
    assert_eq!(stdout(&run(&home, &dir).unwrap()), "1\n");
    assert_eq!(stdout(&run(&home, &dir).unwrap()), "1\n", "started from what it kept, bytes and all");

    std::fs::write(dir.join("src/data.bin"), [7, 2]).unwrap();
    assert_eq!(stdout(&run(&home, &dir).unwrap()), "7\n", "an edit of the same size is seen");

    std::fs::remove_file(dir.join("src/data.bin")).unwrap();
    let gone = run(&home, &dir).unwrap();
    assert!(!gone.status.success());
    assert!(String::from_utf8_lossy(&gone.stderr).contains("there's no file `data.bin` to embed"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_edit_to_an_embedded_file_is_seen_in_a_single_file() {
    let dir = scratch("embed_file").unwrap();
    let (home, game) = (dir.join("home"), dir.join("game.fen"));
    std::fs::write(dir.join("data.bin"), [1]).unwrap();
    std::fs::write(&game, "def main(stdout: Stdout):\n    stdout.print(embed_file(\"data.bin\")[0])\n")
        .unwrap();
    for _ in 0..2 {
        assert_eq!(stdout(&run(&home, &game).unwrap()), "1\n");
    }
    std::fs::write(dir.join("data.bin"), [7]).unwrap();
    assert_eq!(stdout(&run(&home, &game).unwrap()), "7\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_program_with_its_own_embed_function_calls_it() {
    let dir = scratch("embed_own").unwrap();
    let (home, game) = (dir.join("home"), dir.join("game.fen"));
    let source = "def embed_file(path: str) -> int:\n    path.len()\n\n\
        def main(stdout: Stdout):\n    stdout.print(embed_file(\"no such file\"))\n";
    std::fs::write(&game, source).unwrap();
    assert_eq!(stdout(&run(&home, &game).unwrap()), "12\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_edit_to_an_embedded_file_runs_the_tests_before_a_run_again() {
    let test = "test \"the data\":\n    assert DATA[0] == 1\n";
    let dir = embedding_kit("embed_tests", test).unwrap();
    let home = dir.join("home");
    for _ in 0..2 {
        assert_eq!(stdout(&run(&home, &dir).unwrap()), "1\n");
    }
    std::fs::write(dir.join("src/data.bin"), [7, 2]).unwrap();
    let failed = run(&home, &dir).unwrap();
    assert!(!failed.status.success(), "{}", stdout(&failed));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A parameter named like a constant is the parameter inside its function. Native code writes constants out where they're used, which mustn't reach a name that only looks like
/// one. The linter's `naming-case` keeps examples from doing this, so it's checked here.
#[test]
fn a_parameter_named_like_a_constant_is_the_parameter() {
    let source = "LIMIT = 10\n\ndef shadow(LIMIT: int) -> int:\n    LIMIT + 1\n\n\
                  def main(stdout: Stdout):\n    stdout.print(shadow(1), LIMIT)\n";
    let dir = scratch("shadowed_constant").unwrap();
    let game = dir.join("game.fen");
    std::fs::write(&game, source).unwrap();
    let ran = crate::executables::fennec_command()
        .args(["run", "--no-test"])
        .arg(&game)
        .env("FENNEC_HOME", dir.join("home"))
        .output()
        .unwrap();
    assert_eq!(stdout(&ran), "2 10\n", "{}", String::from_utf8_lossy(&ran.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A kit whose `src/pause.fen` has `settle`, and whose `src/main.fen` calls it inside a `lock`
/// in a function only a test uses, which `main` doesn't reach.
fn locking_kit(name: &str) -> std::io::Result<fennec_temp::Temp> {
    let dir = scratch(name)?;
    std::fs::create_dir_all(dir.join("src"))?;
    let manifest =
        format!("name = \"demo\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n", env!("CARGO_PKG_VERSION"));
    std::fs::write(dir.join("kit.toml"), manifest)?;
    std::fs::write(
        dir.join("src/pause.fen"),
        "def settle(clock: Clock) -> int:\n    _ = clock.now()\n    1\n",
    )?;
    let main = "from pause import settle\n\n\
                struct Count:\n    n = 0\n\n\
                def bump(count: shared Count, clock: Clock):\n    lock count as ref current:\n        current.n += settle(clock)\n\n\
                def main(stdout: Stdout):\n    stdout.print(1)\n\n\
                test \"bumps\"(clock: Clock):\n    shared count = Count()\n    bump(count, clock)\n    lock count as current:\n        assert current.n == 1\n";
    std::fs::write(dir.join("src/main.fen"), main)?;
    Ok(dir)
}

/// Runs a program as `run` does, without its tests, so only its check stands between an edit and
/// the run.
fn run_untested(home: &Path, program: &Path) -> std::io::Result<Output> {
    crate::executables::fennec_command()
        .args(["run", "--no-test"])
        .arg(program)
        .env("FENNEC_HOME", home)
        .env_remove("CI")
        .env_remove("FENNEC_NO_CACHE")
        .output()
}

/// An edit to one file's body that makes a function wait is checked against every `lock` that
/// calls it, in files the edit didn't touch too, even after runs that only checked what changed.
#[test]
fn a_body_edit_that_waits_under_another_files_lock_is_refused() {
    let dir = locking_kit("wait_under_lock").unwrap();
    let home = dir.join("home");
    for _ in 0..2 {
        let ran = run_untested(&home, &dir).unwrap();
        assert_eq!(stdout(&ran), "1\n", "{}", String::from_utf8_lossy(&ran.stderr));
    }
    let waits = "def settle(clock: Clock) -> int:\n    clock.sleep(1ms)\n    1\n";
    std::fs::write(dir.join("src/pause.fen"), waits).unwrap();
    for _ in 0..2 {
        let refused = run_untested(&home, &dir).unwrap();
        let said = String::from_utf8_lossy(&refused.stderr);
        assert!(!refused.status.success(), "{said}");
        assert!(said.contains("`settle` waits on"), "{said}");
        assert!(said.contains("src/main.fen"), "{said}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
