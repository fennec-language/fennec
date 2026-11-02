//! The kernels: every Fennec program in `bench/kernels`, each with a twin in Rust that does the
//! same work the idiomatic way, and the self-hosted front end, whose twin is the Rust front end.

use std::path::{Path, PathBuf};

use crate::measure::{Overflow, Program};

/// One program the suite measures.
pub(crate) struct Kernel {
    pub(crate) name: String,
    /// What `fennec run` is given: a file or a kit.
    path: String,
    /// What it reads on standard input.
    stdin: Option<PathBuf>,
    twin: Twin,
}

enum Twin {
    /// A Rust file, built for the target.
    Source(PathBuf),
    /// `fennec dev tree` on this file, which is the Rust front end.
    FrontEnd(String),
}

/// How many times the self-hosted front end's input repeats the kernels' own source.
const CORPUS_REPEATS: usize = 2;

/// Every kernel in `dir`, by name, then the self-hosted front end when `selfhost` is there.
pub(crate) fn find(dir: &Path, out_dir: &Path, selfhost: &Path) -> Result<Vec<Kernel>, String> {
    let mut sources: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|err| format!("couldn't read {}: {err}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "fen"))
        .collect();
    sources.sort();
    let mut kernels: Vec<Kernel> = sources
        .iter()
        .map(|source| Kernel {
            name: source.file_stem().map_or_else(String::new, |stem| stem.to_string_lossy().into_owned()),
            path: source.to_string_lossy().into_owned(),
            stdin: None,
            twin: Twin::Source(source.with_extension("rs")),
        })
        .collect();
    // A kernel can have a second twin, `name.std.rs`, for a comparison worth showing but not
    // holding it to, like a twin with its standard library's defaults.
    for source in sources.clone() {
        let alternate = source.with_extension("std.rs");
        if alternate.is_file() {
            let stem =
                source.file_stem().map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
            kernels.push(Kernel {
                name: format!("{stem}.std"),
                path: source.to_string_lossy().into_owned(),
                stdin: None,
                twin: Twin::Source(alternate),
            });
        }
    }
    kernels.sort_by(|a, b| a.name.cmp(&b.name));
    if selfhost.join("kit.toml").is_file() {
        kernels.push(front_end(&sources, out_dir, selfhost)?);
    }
    Ok(kernels)
}

/// The self-hosted front end, parsing and dumping the kernels' own source. Paths stay relative,
/// since what it prints names its input.
fn front_end(sources: &[PathBuf], out_dir: &Path, selfhost: &Path) -> Result<Kernel, String> {
    let mut corpus = String::new();
    for source in sources {
        corpus.push_str(
            &std::fs::read_to_string(source).map_err(|err| format!("{}: {err}", source.display()))?,
        );
    }
    let corpus_path = out_dir.join("corpus.fen");
    let input_path = out_dir.join("corpus.input");
    let written = std::fs::write(&corpus_path, corpus.repeat(CORPUS_REPEATS))
        .and_then(|()| std::fs::write(&input_path, format!("tree {}\n", corpus_path.display())));
    written.map_err(|err| format!("couldn't write the front end's input: {err}"))?;
    Ok(Kernel {
        name: "selfhost".to_owned(),
        path: selfhost.to_string_lossy().into_owned(),
        stdin: Some(input_path),
        twin: Twin::FrontEnd(corpus_path.to_string_lossy().into_owned()),
    })
}

impl Kernel {
    /// Running the whole kernel, natively.
    pub(crate) fn run(&self, fennec: &Path) -> Program {
        Program {
            command: fennec.to_path_buf(),
            // `--jit` says when the interpreter stands in, and means native code in older
            // builds too, which the suite compares against.
            args: vec!["run".to_owned(), "--jit".to_owned(), self.path.clone()],
            stdin: self.stdin.clone(),
        }
    }

    /// Everything before its `main`.
    pub(crate) fn startup(&self, fennec: &Path) -> Program {
        Program {
            command: fennec.to_path_buf(),
            args: vec!["dev".to_owned(), "startup".to_owned(), self.path.clone()],
            stdin: None,
        }
    }

    /// How many places in its compiled code aren't native, as `fennec dev fallbacks` reports.
    pub(crate) fn not_native(&self, fennec: &Path) -> Option<usize> {
        let output =
            std::process::Command::new(fennec).args(["dev", "fallbacks", &self.path]).output().ok()?;
        output.status.success().then(|| places(&String::from_utf8_lossy(&output.stdout)))
    }

    /// The Fennec file `fennec build` takes, when the kernel is one file with a Rust twin of its
    /// own: not a second twin, and not the self-hosted front end.
    pub(crate) fn single_file(&self) -> Option<&str> {
        let own_twin = matches!(&self.twin, Twin::Source(_)) && !self.name.contains('.');
        let file = Path::new(&self.path).extension().is_some_and(|ext| ext.eq_ignore_ascii_case("fen"));
        (own_twin && file).then_some(self.path.as_str())
    }

    /// Its twin, built if it needs to be.
    pub(crate) fn twin(&self, fennec: &Path, out_dir: &Path, checks: Overflow) -> Result<Program, String> {
        match &self.twin {
            Twin::Source(source) => {
                let command = crate::measure::build_twin(source, out_dir, checks)?;
                Ok(Program { command, args: Vec::new(), stdin: None })
            }
            Twin::FrontEnd(corpus) => Ok(Program {
                command: fennec.to_path_buf(),
                args: vec!["dev".to_owned(), "tree".to_owned(), corpus.clone()],
                stdin: None,
            }),
        }
    }

    /// A hash of the kernel and its twin, which locks them to what was pinned. The self-hosted
    /// front end changes with the language, so only what it prints locks it.
    pub(crate) fn source_hash(&self) -> Result<String, String> {
        let Twin::Source(twin) = &self.twin else { return Ok("-".to_owned()) };
        let read =
            |path: &Path| std::fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()));
        Ok(hash(&(read(Path::new(&self.path))? + &read(twin)?)))
    }

    /// A hash of what it printed, for comparing with its target. The self-hosted front end
    /// starts by naming its input, which the Rust front end doesn't.
    pub(crate) fn output_hash(&self, output: &str) -> String {
        let output = match self.stdin {
            Some(_) => output.split_once('\n').map_or(output, |(_, rest)| rest),
            None => output,
        };
        hash(output)
    }
}

/// The count in the last line of `fennec dev fallbacks`: `3 places aren't native`, or nothing
/// when everything is.
fn places(report: &str) -> usize {
    let last = report.lines().last().unwrap_or("");
    last.strip_suffix(" places aren't native").and_then(|count| count.parse().ok()).unwrap_or(0)
}

/// A short hash of what a program printed.
pub(crate) fn hash(output: &str) -> String {
    let hex = fennec_blake3::hash(output.as_bytes()).to_hex();
    hex.get(..16).unwrap_or(&hex).to_owned()
}

#[cfg(test)]
mod tests {
    use super::places;

    #[test]
    fn the_fallback_report_gives_its_count() {
        assert_eq!(places("a.fen:1:1 in `f`: why\n    code\n3 places aren't native\n"), 3);
        assert_eq!(places("everything compiles to native code\n"), 0);
        assert_eq!(places(""), 0);
    }
}
