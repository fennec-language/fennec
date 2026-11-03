//! The commands that add a section to the suite's report, up to where they need a real build.

use std::io::Result;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Runs a section with a compiler and a folder that aren't there, from a folder of its own.
fn run(section: &str) -> Result<Output> {
    let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join(section);
    std::fs::create_dir_all(&home)?;
    let missing = PathBuf::from("/nonexistent/fennec-bench");
    Command::new(env!("CARGO_BIN_EXE_fennec-bench"))
        .args([section, "--runs", "1", "--fennec"])
        .arg(&missing)
        .arg("--dir")
        .arg(&missing)
        .current_dir(home)
        .output()
}

#[test]
fn parallel_fails_on_a_kernel_it_cant_build() -> Result<()> {
    let output = run("parallel")?;
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{said}");
    assert!(said.contains("couldn't run /nonexistent/fennec-bench"), "{said}");
    Ok(())
}

#[test]
fn realworld_fails_on_twins_it_cant_build() -> Result<()> {
    let output = run("realworld")?;
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{said}");
    assert!(said.contains("couldn't run cargo"), "{said}");
    Ok(())
}

#[test]
fn a_section_refuses_an_option_it_doesnt_know() -> Result<()> {
    let output = Command::new(env!("CARGO_BIN_EXE_fennec-bench"))
        .args(["realworld", "--nothing", "1"])
        .current_dir(env!("CARGO_TARGET_TMPDIR"))
        .output()?;
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{said}");
    assert!(said.contains("unknown option `--nothing`"), "{said}");
    Ok(())
}
