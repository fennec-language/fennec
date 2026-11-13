//! `fennec-bench ops`: what one call of an everyday operation costs, in instructions, beside
//! its Rust twin. Each program in `bench/ops` does one operation `OPS` times on inputs that vary,
//! like `line.find(": ")` or `json.loads[Order](text)`. It's built twice, as it is and with
//! `OPS = 0`, and the difference over `OPS` is what one operation takes, with setup, startup and
//! printing left out.
//!
//! Each operation's count is pinned, per architecture, in `bench/ops/<architecture>.txt`, with
//! its twin's and a hash of what both print. An operation that takes more than its pin, by more
//! than counts drift, fails the run, so a slow path fails CI the day it's added. One that takes
//! less shows as faster until it's pinned again. `fennec-bench ops --pin` writes the file, and
//! checks that each program prints what its twin prints.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::kernels::hash;
use crate::measure::{Counter, Overflow, Program};

/// How far over its pin an operation may go before it fails: counts drift a little between
/// builds, and the smallest operations take only tens of instructions.
const SLACK_PERCENT: u64 = 5;
const SLACK_INSTRUCTIONS: u64 = 10;

/// How many times a program is counted where counts vary from run to run.
const COUNTS_READ: usize = 5;

/// What's pinned for one operation.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Pinned {
    fennec: u64,
    rust: u64,
    output: String,
}

/// The file each architecture's pins are in.
fn pins_path(dir: &Path) -> PathBuf {
    dir.join(format!("{}.txt", std::env::consts::ARCH))
}

/// The pins in `text`: one line per operation, `name fennec rust output`, and `#` comments.
fn parse(text: &str) -> Result<Vec<(String, Pinned)>, String> {
    let mut pins = Vec::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
        let words: Vec<&str> = line.split_whitespace().collect();
        let [name, fennec, rust, output] = words.as_slice() else {
            return Err(format!("a pin is `name fennec rust output`, not `{line}`"));
        };
        let number = |text: &str| text.parse::<u64>().map_err(|err| format!("`{text}` in `{line}`: {err}"));
        pins.push((
            (*name).to_owned(),
            Pinned { fennec: number(fennec)?, rust: number(rust)?, output: (*output).to_owned() },
        ));
    }
    Ok(pins)
}

/// The text of a pins file.
fn text(pins: &[(String, Pinned)], compiler: &str) -> String {
    let mut out = format!(
        "# Pinned with {compiler}.\n# Written by `fennec-bench ops --pin`: instructions for one operation, Fennec's and its\n# Rust twin's, and a hash of what both print.\n# operation fennec rust output\n"
    );
    for (name, pinned) in pins {
        let _ = writeln!(out, "{name} {} {} {}", pinned.fennec, pinned.rust, pinned.output);
    }
    out
}

/// How many times the program in `source` does its operation: its `OPS = ...` line.
fn ops_in(source: &str) -> Option<u64> {
    source
        .lines()
        .find_map(|line| line.strip_prefix("OPS = "))
        .and_then(|ops| ops.replace('_', "").trim().parse().ok())
}

/// `source` with no operations done: Fennec's `OPS = 0`, or the twin's `black_box(0_i64)`.
fn with_none(source: &str, ops: u64) -> Option<String> {
    let fennec = source.lines().find(|line| line.starts_with("OPS = "));
    if let Some(line) = fennec {
        return Some(source.replacen(line, "OPS = 0", 1));
    }
    let grouped = grouped(ops);
    let twin = format!("black_box({grouped}_i64)");
    source.contains(&twin).then(|| source.replacen(&twin, "black_box(0_i64)", 1))
}

/// `ops` written with `_` between each group of three digits, as the programs write it.
fn grouped(ops: u64) -> String {
    let digits = ops.to_string();
    let mut out = String::new();
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            out.push('_');
        }
        out.push(digit);
    }
    out
}

/// One operation measured: instructions for one call, and what it printed.
struct Measured {
    per_op: u64,
    output: String,
}

/// What `program` runs, in instructions. Valgrind counts the same every time; a counter reading
/// the CPU's counts a little more on some runs than others, so the least of several counts.
fn least(counter: &Counter, program: &Program) -> Option<u64> {
    let runs = if matches!(counter, Counter::Valgrind) { 1 } else { COUNTS_READ };
    (0..runs).map(|_| counter.count(program)).collect::<Option<Vec<u64>>>()?.into_iter().min()
}

/// Counts what `full` and `none` run, and gives what one of `ops` operations took.
fn per_op(
    counter: &Counter,
    full: &Program,
    none: &Program,
    ops: u64,
    name: &str,
) -> Result<Measured, String> {
    let output = full.run()?.stdout;
    let all = least(counter, full).ok_or_else(|| format!("couldn't count {name}"))?;
    let setup = least(counter, none).ok_or_else(|| format!("couldn't count {name} with no operations"))?;
    Ok(Measured { per_op: all.saturating_sub(setup) / ops.max(1), output })
}

/// Writes `text` as `name` in `out_dir`, for building.
fn written(out_dir: &Path, name: &str, text: &str) -> Result<PathBuf, String> {
    let path = out_dir.join(name);
    std::fs::write(&path, text).map_err(|err| format!("{}: {err}", path.display()))?;
    Ok(path)
}

/// The Fennec program in `source`, built as it is and with no operations.
fn built_both(
    fennec: &Path,
    out_dir: &Path,
    name: &str,
    source: &str,
    ops: u64,
) -> Result<(Program, Program), String> {
    let none = with_none(source, ops).ok_or_else(|| format!("{name}.fen has no `OPS = ` line"))?;
    let mut programs = Vec::new();
    for (tag, text) in [("all", source), ("none", none.as_str())] {
        let path = written(out_dir, &format!("op_{name}_{tag}.fen"), text)?;
        let command =
            crate::llvm_suite::built(fennec, &path.to_string_lossy(), out_dir, &format!("op_{name}_{tag}"))?;
        programs.push(Program { command, args: Vec::new(), stdin: None });
    }
    let none = programs.pop().ok_or("nothing was built")?;
    let all = programs.pop().ok_or("nothing was built")?;
    Ok((all, none))
}

/// The Rust twin in `source`, built as it is and with no operations, overflow-checked.
fn twin_both(out_dir: &Path, name: &str, source: &str, ops: u64) -> Result<(Program, Program), String> {
    let none = with_none(source, ops)
        .ok_or_else(|| format!("{name}.rs doesn't read its count as `black_box({}_i64)`", grouped(ops)))?;
    let mut programs = Vec::new();
    for (tag, text) in [("all", source), ("none", none.as_str())] {
        let path = written(out_dir, &format!("op_{name}_{tag}.rs"), text)?;
        let command = crate::measure::build_twin(&path, out_dir, Overflow::Checked)?;
        programs.push(Program { command, args: Vec::new(), stdin: None });
    }
    let none = programs.pop().ok_or("nothing was built")?;
    let all = programs.pop().ok_or("nothing was built")?;
    Ok((all, none))
}

/// Every operation in `dir`, by name, with its Fennec source and its twin's.
fn operations(dir: &Path) -> Result<Vec<(String, String, String)>, String> {
    let read =
        |path: &Path| std::fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()));
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map_err(|err| format!("{}: {err}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "fen"))
        .filter_map(|path| path.file_stem().map(|stem| stem.to_string_lossy().into_owned()))
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let fennec = read(&dir.join(format!("{name}.fen")))?;
            let twin = read(&dir.join(format!("{name}.rs")))?;
            Ok((name, fennec, twin))
        })
        .collect()
}

/// Why the operation `name`, measured as `ours`, fails against its pin, if it does: it isn't
/// pinned, it prints something else, or it takes more than its pin allows.
fn judged(name: &str, ours: &Measured, pin: Option<&Pinned>, output: &str) -> Option<String> {
    let Some(pin) = pin else {
        return Some(format!("{name} isn't pinned: `fennec-bench ops --pin` pins it"));
    };
    if pin.output != output {
        return Some(format!("{name} printed something else than when it was pinned:\n{}", ours.output));
    }
    let allowed = pin.fennec + pin.fennec * SLACK_PERCENT / 100 + SLACK_INSTRUCTIONS;
    (ours.per_op > allowed).then(|| {
        format!("{name} takes {} instructions an operation, more than the {} pinned", ours.per_op, pin.fennec)
    })
}

/// `fennec-bench ops [--pin] [--fennec path]`: measures each operation, and says whether all of
/// them are within their pins, or writes the pins.
pub(crate) fn run(mut args: impl Iterator<Item = String>) -> Result<bool, String> {
    let mut fennec = PathBuf::from("target/release/fennec");
    let mut dir = PathBuf::from("bench/ops");
    let mut pinning = false;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--pin" => pinning = true,
            "--fennec" => fennec = args.next().map(PathBuf::from).ok_or("--fennec takes a path")?,
            "--dir" => dir = args.next().map(PathBuf::from).ok_or("--dir takes a folder")?,
            _ => return Err(format!("`fennec-bench ops` doesn't take `{flag}`")),
        }
    }
    let out_dir = PathBuf::from("target/bench/ops");
    std::fs::create_dir_all(&out_dir).map_err(|err| format!("couldn't make {}: {err}", out_dir.display()))?;
    let counter = Counter::find(&out_dir).ok_or("nothing can count instructions on this machine")?;
    let path = pins_path(&dir);
    let pinned = if path.exists() {
        parse(&std::fs::read_to_string(&path).map_err(|err| format!("{}: {err}", path.display()))?)?
    } else {
        Vec::new()
    };
    let mut fresh = Vec::new();
    let mut failures = Vec::new();
    let mut report =
        format!("{:<14} {:>9} {:>9} {:>9} {:>11}\n", "operation", "Fennec", "pinned", "Rust", "Rust÷Fennec");
    for (name, source, twin) in operations(&dir)? {
        let ops = ops_in(&source).ok_or_else(|| format!("{name}.fen has no `OPS = ` line"))?;
        eprintln!("measuring {name}");
        let (all, none) = built_both(&fennec, &out_dir, &name, &source, ops)?;
        let ours = per_op(&counter, &all, &none, ops, &name)?;
        let pin = pinned.iter().find(|(pinned, _)| *pinned == name).map(|(_, pin)| pin.clone());
        let rust = if pinning {
            let (all, none) = twin_both(&out_dir, &name, &twin, ops)?;
            let theirs = per_op(&counter, &all, &none, ops, &name)?;
            if theirs.output != ours.output {
                return Err(format!(
                    "{name} printed\n{}but its twin printed\n{}",
                    ours.output, theirs.output
                ));
            }
            theirs.per_op
        } else {
            pin.as_ref().map_or(0, |pin| pin.rust)
        };
        let output = hash(&ours.output);
        let shown_pin = pin.as_ref().map_or_else(|| "-".to_owned(), |pin| pin.fennec.to_string());
        // Rust's share in tenths of a percent, kept in whole numbers.
        let share = (rust * 1000).checked_div(ours.per_op).unwrap_or(0);
        let share = format!("{}.{}%", share / 10, share % 10);
        let _ = writeln!(report, "{name:<14} {:>9} {shown_pin:>9} {rust:>9} {share:>11}", ours.per_op);
        if !pinning && let Some(failure) = judged(&name, &ours, pin.as_ref(), &output) {
            failures.push(failure);
        }
        fresh.push((name, Pinned { fennec: ours.per_op, rust, output }));
    }
    print!("{report}");
    if pinning {
        let compiler = crate::measure::rustc_version()?;
        std::fs::write(&path, text(&fresh, &compiler)).map_err(|err| format!("{}: {err}", path.display()))?;
        eprintln!("pinned {}", path.display());
        return Ok(true);
    }
    for failure in &failures {
        eprintln!("error: {failure}");
    }
    Ok(failures.is_empty())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Pinned, grouped, ops_in, parse, text, with_none};

    /// Every operation pins file under `bench/ops`, for every architecture, reads cleanly, and
    /// each line names an operation there is, so a line copied in from another file fails here.
    #[test]
    fn every_pins_file_reads_and_names_only_operations() {
        let ops = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/ops");
        let mut files = 0;
        for entry in std::fs::read_dir(&ops).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|ext| ext != "txt") {
                continue;
            }
            files += 1;
            let pins = parse(&std::fs::read_to_string(&path).unwrap())
                .unwrap_or_else(|line| panic!("{}: {line}", path.display()));
            assert!(!pins.is_empty(), "{} has no pins", path.display());
            for (name, _) in &pins {
                let known = ops.join(format!("{name}.fen")).exists();
                assert!(known, "{} names `{name}`, which isn't an operation", path.display());
            }
        }
        assert!(files >= 2, "found only {files} pins files");
    }

    #[test]
    fn a_count_is_found_and_taken_out_on_both_sides() {
        let fennec = "OPS = 20_000\n\ndef main(stdout: Stdout):\n    pass\n";
        assert_eq!(ops_in(fennec), Some(20_000));
        assert_eq!(
            with_none(fennec, 20_000).as_deref(),
            Some("OPS = 0\n\ndef main(stdout: Stdout):\n    pass\n")
        );
        let twin = "fn main() {\n    let ops = black_box(20_000_i64);\n}\n";
        assert_eq!(
            with_none(twin, 20_000).as_deref(),
            Some("fn main() {\n    let ops = black_box(0_i64);\n}\n")
        );
        assert_eq!(grouped(1_234_567), "1_234_567");
        assert_eq!(grouped(500), "500");
    }

    #[test]
    fn pins_read_back_as_written() {
        let pins = vec![("find".to_owned(), Pinned { fennec: 3622, rust: 191, output: "ab12".to_owned() })];
        assert_eq!(parse(&text(&pins, "rustc 1.99.0")), Ok(pins));
        assert!(parse("find 1 2").is_err());
    }
}
