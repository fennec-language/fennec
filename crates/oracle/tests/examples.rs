//! Every example the oracle is listed as running, in `runs.txt`, still runs and prints exactly
//! its `.out` file, so what it covers can't shrink without a word, and what it runs agrees with
//! the compiled program the examples' tests hold to the same files.

use std::path::Path;

use fennec_oracle::report::{Verdict, examples, judge};

#[test]
fn every_example_listed_as_running_matches_its_out_file() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let listed = std::fs::read_to_string(here.join("runs.txt")).unwrap();
    let listed: Vec<&str> =
        listed.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')).collect();
    assert!(listed.len() >= 80, "only {} examples are listed as running", listed.len());
    let all = examples(&here.join("../../examples")).unwrap();
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

/// Every example in `never.txt`, the ones the oracle can never model, is an example, and none of
/// them is listed as running: one that comes to run is taken off the list.
#[test]
fn the_examples_it_can_never_model_are_examples_it_doesnt_run() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let running = std::fs::read_to_string(here.join("runs.txt")).unwrap();
    let all = examples(&here.join("../../examples")).unwrap();
    let never = fennec_oracle::report::never();
    assert!(!never.is_empty(), "never.txt lists nothing");
    for (name, why) in never {
        assert!(all.iter().any(|example| example.name == name), "{name} in never.txt isn't an example");
        assert!(!why.is_empty(), "{name} in never.txt says no why");
        assert!(!running.lines().any(|line| line.trim() == name), "{name} runs now: take it off never.txt");
    }
}
