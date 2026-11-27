//! Running the oracle over the examples, and comparing what two runs printed.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::{Ended, Outcome};

/// One example: a `.fen` file, or a kit's folder, with the `.out` file of what it prints.
pub struct Example {
    pub name: String,
    pub path: PathBuf,
    pub expected: PathBuf,
}

/// What the oracle made of one example.
pub enum Verdict {
    /// It ran and printed exactly the `.out` file.
    Matches,
    /// It ran, and printed something else or stopped.
    Differs(String),
    /// It reached something it doesn't model, named.
    Skipped(String),
}

/// Every example in `dir`: each `*.fen` beside a `.out`, then each kit in `projects/` beside one,
/// by name.
pub fn examples(dir: &Path) -> std::io::Result<Vec<Example>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let expected = path.with_extension("out");
        if path.extension().is_some_and(|ext| ext == "fen") && expected.is_file() {
            let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
            found.push(Example { name, path, expected });
        }
    }
    let projects = dir.join("projects");
    if let Ok(entries) = std::fs::read_dir(&projects) {
        for entry in entries {
            let path = entry?.path();
            let Some(name) = path.file_name().map(|name| name.to_string_lossy().into_owned()) else {
                continue;
            };
            let expected = projects.join(format!("{name}.out"));
            if path.join("kit.toml").is_file() && expected.is_file() {
                found.push(Example { name: format!("projects/{name}"), path, expected });
            }
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(found)
}

/// Runs the oracle on one example and holds it to its `.out` file.
pub fn judge(example: &Example) -> Verdict {
    let shown = format!("examples/{}", example.name);
    let outcome = crate::run_path(&example.path, &shown);
    let expected = std::fs::read(&example.expected).unwrap_or_default();
    match &outcome.ended {
        Ended::Unsupported(why) => Verdict::Skipped(first_line(why)),
        Ended::Exited(0) if outcome.stdout == expected => Verdict::Matches,
        ended => {
            let mut said =
                diff(&String::from_utf8_lossy(&expected), &String::from_utf8_lossy(&outcome.stdout));
            match ended {
                Ended::Exited(0) | Ended::Unsupported(_) => {}
                Ended::Exited(code) => {
                    let _ = writeln!(said, "it exited with {code}");
                }
                Ended::Stopped(why) => {
                    let _ = write!(said, "it stopped:\n{why}");
                }
            }
            Verdict::Differs(said)
        }
    }
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").to_owned()
}

/// The examples the oracle can never model, from `never.txt`, each with why.
pub fn never() -> Vec<(&'static str, &'static str)> {
    include_str!("../never.txt")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once(" | "))
        .collect()
}

/// The examples left out of the useful set, from `excluded.txt`, each with why and the check that
/// covers it instead.
pub fn excluded() -> Vec<(&'static str, &'static str, &'static str)> {
    include_str!("../excluded.txt")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let mut parts = line.splitn(3, " | ");
            Some((parts.next()?, parts.next()?, parts.next()?))
        })
        .collect()
}

/// The examples the oracle could model but doesn't yet, from `gaps.txt`, each with what stops it.
pub fn gaps() -> Vec<(&'static str, &'static str)> {
    include_str!("../gaps.txt")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once(" | "))
        .collect()
}

/// Why an example is left out of the useful set, and what covers it, when it's one.
fn excluded_why(name: &str) -> Option<(&'static str, &'static str)> {
    excluded().into_iter().find(|(excluded, ..)| *excluded == name).map(|(_, why, check)| (why, check))
}

/// Why an example is one the oracle can never model, when it's one.
fn never_why(name: &str) -> Option<&'static str> {
    never().into_iter().find(|(never, _)| *never == name).map(|(_, why)| why)
}

/// The line the coverage report gives one example: run, a mismatch, skipped and why, or one it
/// can never model and why.
pub fn line(example: &Example, verdict: &Verdict) -> String {
    match verdict {
        Verdict::Matches => format!("run      {}\n", example.name),
        Verdict::Differs(_) => format!("MISMATCH {}\n", example.name),
        Verdict::Skipped(why) => match (never_why(&example.name), excluded_why(&example.name)) {
            (Some(never), _) => format!("never    {}: {never}\n", example.name),
            (None, Some((excluded, _))) => format!("excluded {}: {excluded}\n", example.name),
            (None, None) => format!("skipped  {}: {why}\n", example.name),
        },
    }
}

/// The end of the coverage report: the features that skipped examples, most first, the ones it
/// can never model, those left out of the useful set, the mismatches, the share it runs, of every
/// example and of those it can model, and how much of the useful set it runs, with the gaps
/// left to build.
pub fn summary(examples: &[Example], judged: &[Verdict]) -> String {
    let mut text = String::new();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut ran = 0;
    let mut differ = Vec::new();
    let mut nevers = Vec::new();
    let mut left_out = Vec::new();
    let mut gaps = Vec::new();
    let mut useful = 0;
    let mut useful_ran = 0;
    for (example, verdict) in examples.iter().zip(judged) {
        let excluded = excluded_why(&example.name);
        if never_why(&example.name).is_none() && excluded.is_none() {
            useful += 1;
            useful_ran += usize::from(!matches!(verdict, Verdict::Skipped(_)));
        }
        match verdict {
            Verdict::Matches => ran += 1,
            Verdict::Differs(why) => {
                ran += 1;
                differ.push((example.name.as_str(), why.as_str()));
            }
            Verdict::Skipped(why) => match (never_why(&example.name), excluded) {
                (Some(never), _) => nevers.push((example.name.as_str(), never)),
                (None, Some((why, check))) => left_out.push((example.name.as_str(), why, check)),
                (None, None) => {
                    *counts.entry(why.as_str()).or_default() += 1;
                    gaps.push(example.name.as_str());
                }
            },
        }
    }
    let mut ranked: Vec<(&str, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let _ = writeln!(text, "\nwhat skipped them, most first:");
    for (why, count) in ranked {
        let _ = writeln!(text, "{count:5}  {why}");
    }
    if !nevers.is_empty() {
        let _ = writeln!(text, "\nwhat it can never model (never.txt):");
        for (name, why) in &nevers {
            let _ = writeln!(text, "       {name}: {why}");
        }
    }
    if !left_out.is_empty() {
        let _ = writeln!(text, "\nleft out of the useful set, and what covers each (excluded.txt):");
        for (name, why, check) in &left_out {
            let _ = writeln!(text, "       {name}: {why}; covered by {check}");
        }
    }
    if !differ.is_empty() {
        let _ = writeln!(text, "\nmismatches with their .out files:");
        for (name, why) in &differ {
            let _ = writeln!(text, "--- {name}\n{why}");
        }
    }
    let total = examples.len();
    let modeled = total - nevers.len();
    let _ = writeln!(
        text,
        "\nruns {ran} of {total} examples ({:.1}%), and {ran} of the {modeled} it can model ({:.1}%), {} mismatched",
        percent(ran, total),
        percent(ran, modeled),
        differ.len()
    );
    let _ = write!(text, "useful set: {useful_ran} of {useful} ({:.1}%)", percent(useful_ran, useful));
    if gaps.is_empty() {
        let _ = writeln!(text);
    } else {
        let _ = writeln!(text, ", {} to build: {}", gaps.len(), gaps.join(", "));
    }
    text
}

/// `part` of `whole`, in percent.
fn percent(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    crate::text::int_as_float(crate::interp::as_int(part)) * 100.0
        / crate::text::int_as_float(crate::interp::as_int(whole))
}

/// A line diff of what was expected and what came: each line only one of them has, `-` for the
/// expected and `+` for what came, by line number, from the longest run of lines they share.
pub fn diff(expected: &str, got: &str) -> String {
    let want: Vec<&str> = expected.lines().collect();
    let have: Vec<&str> = got.lines().collect();
    let mut text = String::new();
    if want == have {
        if expected != got {
            let _ = writeln!(text, "(they differ only in how the last line ends)");
        }
        return text;
    }
    // Too long to line up cheaply: everything from the first line that differs.
    if want.len().saturating_mul(have.len()) > 4_000_000 {
        let same = want.iter().zip(&have).take_while(|(a, b)| a == b).count();
        let _ = writeln!(text, "@@ from line {} @@", same + 1);
        for line in want.iter().skip(same).take(20) {
            let _ = writeln!(text, "-{line}");
        }
        for line in have.iter().skip(same).take(20) {
            let _ = writeln!(text, "+{line}");
        }
        return text;
    }
    // How many lines `want[i..]` and `have[j..]` share, at most, in order.
    let width = have.len() + 1;
    let mut shared = vec![0_usize; (want.len() + 1) * width];
    for i in (0..want.len()).rev() {
        for j in (0..have.len()).rev() {
            let here = if want.get(i) == have.get(j) {
                shared.get((i + 1) * width + j + 1).map_or(0, |after| after + 1)
            } else {
                let down = shared.get((i + 1) * width + j).copied().unwrap_or(0);
                let right = shared.get(i * width + j + 1).copied().unwrap_or(0);
                down.max(right)
            };
            if let Some(slot) = shared.get_mut(i * width + j) {
                *slot = here;
            }
        }
    }
    let (mut i, mut j, mut shown) = (0, 0, 0);
    while (i < want.len() || j < have.len()) && shown < 40 {
        let at = |i: usize, j: usize| shared.get(i * width + j).copied().unwrap_or(0);
        if i < want.len() && j < have.len() && want.get(i) == have.get(j) {
            i += 1;
            j += 1;
        } else if j < have.len() && (i == want.len() || at(i, j + 1) >= at(i + 1, j)) {
            let _ = writeln!(text, "+{:>4} {}", j + 1, have.get(j).unwrap_or(&""));
            j += 1;
            shown += 1;
        } else {
            let _ = writeln!(text, "-{:>4} {}", i + 1, want.get(i).unwrap_or(&""));
            i += 1;
            shown += 1;
        }
    }
    text
}

/// What a run printed, with its lines sorted when the program's order isn't fixed.
pub fn as_compared(stdout: &[u8], sorted: bool) -> String {
    let text = String::from_utf8_lossy(stdout).into_owned();
    if !sorted {
        return text;
    }
    let mut lines: Vec<&str> = text.lines().collect();
    lines.sort_unstable();
    lines.join("\n")
}

/// The oracle's outcome as an exit code and what it printed, for comparing with a compiled run.
pub fn oracle_side(outcome: &Outcome) -> Option<(u8, &[u8])> {
    outcome.exit_code().map(|code| (code, outcome.stdout.as_slice()))
}
