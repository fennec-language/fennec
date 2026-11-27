//! Every example the oracle is listed as running, in `runs.txt`, still runs and prints exactly
//! its `.out` file, so what it covers can't shrink without a word, and what it runs agrees with
//! the compiled program the examples' tests hold to the same files.

use std::path::Path;

use fennec_oracle::report::{Example, Verdict, examples, judge};

/// The examples, and the programs that test std's private parts from outside it, which
/// `fennec-std-tests` builds: each is held to its `.out` file the same way.
fn every_example(here: &Path) -> std::io::Result<Vec<Example>> {
    // Those programs import std's private names, as `fennec-std-tests` lets them: the oracle, a
    // test-only crate, takes them too.
    fennec_syntax::let_programs_import_std_private_names();
    let mut all = examples(&here.join("../../examples"))?;
    all.extend(examples(&here.join("../fennec/tests/fixtures/std_private"))?);
    Ok(all)
}

#[test]
fn every_example_listed_as_running_matches_its_out_file() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let listed = std::fs::read_to_string(here.join("runs.txt")).unwrap();
    let listed: Vec<&str> =
        listed.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')).collect();
    assert!(listed.len() >= 80, "only {} examples are listed as running", listed.len());
    let all = every_example(here).unwrap();
    let mut wrong = Vec::new();
    for name in listed {
        let Some(example) = all.iter().find(|example| example.name == name) else {
            wrong.push(format!("{name} is listed, but isn't an example with a .out file"));
            continue;
        };
        match judge(example) {
            Verdict::Matches => {}
            Verdict::Differs(how) => wrong.push(format!("{name} prints something else:\n{how}")),
            Verdict::Skipped(why) => wrong.push(format!("{name} no longer runs: {why}")),
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Every example in `excluded.txt`, those left out of the useful set, is an example, says why and
/// what covers it, and isn't in `never.txt` too. One that comes to run is taken off the list, so
/// the useful set grows.
#[test]
fn the_examples_left_out_of_the_useful_set_are_examples_it_doesnt_run() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let running = std::fs::read_to_string(here.join("runs.txt")).unwrap();
    let all = every_example(here).unwrap();
    let never = fennec_oracle::report::never();
    let excluded = fennec_oracle::report::excluded();
    assert!(!excluded.is_empty(), "excluded.txt lists nothing");
    for (name, why, check) in excluded {
        assert!(all.iter().any(|example| example.name == name), "{name} in excluded.txt isn't an example");
        assert!(!why.is_empty() && !check.is_empty(), "{name} in excluded.txt says no why or no check");
        assert!(!never.iter().any(|(never, _)| *never == name), "{name} is in never.txt and excluded.txt");
        assert!(
            !running.lines().any(|line| line.trim() == name),
            "{name} runs now: take it off excluded.txt"
        );
    }
}

/// Every example in `never.txt`, the ones the oracle can never model, is an example, and none of
/// them is listed as running: one that comes to run is taken off the list.
#[test]
fn the_examples_it_can_never_model_are_examples_it_doesnt_run() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let running = std::fs::read_to_string(here.join("runs.txt")).unwrap();
    let all = every_example(here).unwrap();
    let never = fennec_oracle::report::never();
    assert!(!never.is_empty(), "never.txt lists nothing");
    for (name, why) in never {
        assert!(all.iter().any(|example| example.name == name), "{name} in never.txt isn't an example");
        assert!(!why.is_empty(), "{name} in never.txt says no why");
        assert!(!running.lines().any(|line| line.trim() == name), "{name} runs now: take it off never.txt");
    }
}

/// Every example is in exactly one of `runs.txt`, `never.txt`, `excluded.txt` and `gaps.txt`. So a
/// new example the oracle runs is held to its `.out` file from the day it comes, and one it can't
/// run comes with a line saying why, rather than the share it checks going down without a word.
#[test]
fn every_example_is_in_one_list() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let running = std::fs::read_to_string(here.join("runs.txt")).unwrap();
    let mut lists: Vec<(&str, Vec<&str>)> = vec![(
        "runs.txt",
        running.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')).collect(),
    )];
    lists.push(("never.txt", fennec_oracle::report::never().into_iter().map(|(name, _)| name).collect()));
    lists.push((
        "excluded.txt",
        fennec_oracle::report::excluded().into_iter().map(|(name, ..)| name).collect(),
    ));
    lists.push(("gaps.txt", fennec_oracle::report::gaps().into_iter().map(|(name, _)| name).collect()));
    let mut wrong = Vec::new();
    for example in every_example(here).unwrap() {
        let holding: Vec<&str> = lists
            .iter()
            .filter(|(_, names)| names.contains(&example.name.as_str()))
            .map(|(list, _)| *list)
            .collect();
        match holding.as_slice() {
            [_] => {}
            [] => wrong.push(format!(
                "{} is in no list: add it to runs.txt if the oracle runs it, or to gaps.txt with what stops it",
                example.name
            )),
            several => wrong.push(format!("{} is in {}", example.name, several.join(" and "))),
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Every example in `gaps.txt` is an example, says what stops it, and still doesn't run: one that
/// comes to run is taken off for `runs.txt`, where it's held to its `.out` file.
#[test]
fn the_gaps_are_examples_it_doesnt_run_yet() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let all = every_example(here).unwrap();
    let mut wrong = Vec::new();
    for (name, why) in fennec_oracle::report::gaps() {
        let Some(example) = all.iter().find(|example| example.name == name) else {
            wrong.push(format!("{name} in gaps.txt isn't an example with a .out file"));
            continue;
        };
        if why.is_empty() {
            wrong.push(format!("{name} in gaps.txt says nothing of what stops it"));
        }
        match judge(example) {
            Verdict::Skipped(_) => {}
            Verdict::Matches => {
                wrong.push(format!("{name} runs now: take it off gaps.txt and add it to runs.txt"));
            }
            Verdict::Differs(how) => wrong.push(format!("{name} runs now and prints something else:\n{how}")),
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
