//! The kernels: every Fennec program in `bench/kernels`, each with a twin in Rust that does the
//! same work the idiomatic way, and the self-hosted front end, whose twin is the Rust front end.

use std::path::{Path, PathBuf};

use crate::measure::Program;

/// One program the suite measures.
pub(crate) struct Kernel {
    pub(crate) name: String,
    /// What `fennec run --jit` is given: a file or a kit.
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

    /// Its twin, built if it needs to be.
    pub(crate) fn twin(&self, fennec: &Path, out_dir: &Path) -> Result<Program, String> {
        match &self.twin {
            Twin::Source(source) => {
                let command = crate::measure::build_twin(source, out_dir)?;
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

/// A short hash of what a program printed.
pub(crate) fn hash(output: &str) -> String {
    let hex = fennec_blake3::hash(output.as_bytes()).to_hex();
    hex.get(..16).unwrap_or(&hex).to_owned()
}
