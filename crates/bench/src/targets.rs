//! The pinned targets, one file per CPU architecture in `bench/targets`: how many instructions
//! each kernel's work should take. `fennec-bench pin` measures them from the kernels' twins, so
//! everyday runs need only Fennec.
//!
//! A kernel's `enforced` mark is its ratchet. The change that first brings a kernel to the JIT's
//! floor sets it, and from then on a run fails if that kernel drops below.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// A kernel's target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) instructions: u64,
    /// How long its work took when pinned, in microseconds, on the machine that pinned it.
    pub(crate) micros: u64,
    /// A hash of what the kernel must print.
    pub(crate) output: String,
    /// A hash of the kernel and its twin as they were pinned. The suite is locked: a kernel
    /// that changes has to be pinned again, in a change that says why.
    pub(crate) source: String,
    pub(crate) enforced: bool,
}

pub(crate) type Targets = BTreeMap<String, Target>;

const HEADER: &str = "# Written by `fennec-bench pin`. Only the last column is edited by hand: `yes` once a kernel\n\
                      # reaches the JIT's floor, so it can't drop below it again.\n\
                      # kernel instructions micros output source enforced\n";

/// This machine's targets file under `dir`.
pub(crate) fn path(dir: &Path) -> PathBuf {
    dir.join(format!("{}.txt", std::env::consts::ARCH))
}

/// Reads a targets file. A file that isn't there means no targets yet.
pub(crate) fn read(path: &Path) -> Result<Targets, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text).map_err(|line| format!("{}: can't read `{line}`", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Targets::new()),
        Err(err) => Err(format!("couldn't read {}: {err}", path.display())),
    }
}

fn parse(text: &str) -> Result<Targets, String> {
    let mut targets = Targets::new();
    for line in text.lines().filter(|line| !line.is_empty() && !line.starts_with('#')) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let [kernel, instructions, micros, output, source, enforced] = parts.as_slice() else {
            return Err(line.to_owned());
        };
        let instructions = instructions.parse().map_err(|_| line.to_owned())?;
        let micros = micros.parse().map_err(|_| line.to_owned())?;
        let enforced = match *enforced {
            "yes" => true,
            "no" => false,
            _ => return Err(line.to_owned()),
        };
        targets.insert(
            (*kernel).to_owned(),
            Target {
                instructions,
                micros,
                output: (*output).to_owned(),
                source: (*source).to_owned(),
                enforced,
            },
        );
    }
    Ok(targets)
}

/// Writes a targets file, saying which compiler built the twins.
pub(crate) fn write(path: &Path, targets: &Targets, compiler: &str) -> Result<(), String> {
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)
            .map_err(|err| format!("couldn't make {}: {err}", folder.display()))?;
    }
    std::fs::write(path, format!("# Pinned with {compiler}.\n{}", text(targets)))
        .map_err(|err| format!("couldn't write {}: {err}", path.display()))
}

fn text(targets: &Targets) -> String {
    let mut text = HEADER.to_owned();
    for (kernel, target) in targets {
        let enforced = if target.enforced { "yes" } else { "no" };
        let _ = writeln!(
            text,
            "{kernel} {} {} {} {} {enforced}",
            target.instructions, target.micros, target.output, target.source
        );
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{Target, Targets, parse, text};

    #[test]
    fn targets_read_back_as_they_were_written() {
        let mut targets = Targets::new();
        targets.insert(
            "fib".to_owned(),
            Target {
                instructions: 5,
                micros: 9,
                output: "abc".to_owned(),
                source: "s".to_owned(),
                enforced: true,
            },
        );
        targets.insert(
            "sort".to_owned(),
            Target {
                instructions: 7,
                micros: 8,
                output: "def".to_owned(),
                source: "t".to_owned(),
                enforced: false,
            },
        );
        assert_eq!(parse(&text(&targets)), Ok(targets));
    }

    #[test]
    fn a_broken_line_is_named() {
        assert_eq!(parse("fib 5 9 abc s\n"), Err("fib 5 9 abc s".to_owned()));
        assert_eq!(parse("fib five 9 abc s yes\n"), Err("fib five 9 abc s yes".to_owned()));
        assert_eq!(parse("fib 5 9 abc s maybe\n"), Err("fib 5 9 abc s maybe".to_owned()));
    }
}
