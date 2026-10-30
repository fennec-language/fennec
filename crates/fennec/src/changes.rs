//! `fennec dev changes [path]`: what changed since the last time it ran on this program,
//! file by file and definition by definition, from the build engine's content hashes. A
//! file whose bytes are unchanged isn't parsed again.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use fennec_build::{Change, Kind, Run, Snapshot};

use crate::daemon::{self, Outcome};

/// The last snapshot of each program, which the daemon keeps in memory between runs.
pub(crate) type Snapshots = HashMap<PathBuf, Snapshot>;

pub(crate) fn changes(path: &str) -> ExitCode {
    let Some(home) = fennec_build::fennec_home() else {
        eprintln!("error: couldn't find a home folder; set FENNEC_HOME");
        return ExitCode::FAILURE;
    };
    let absolute = std::env::current_dir().map_or_else(|_| PathBuf::from(path), |dir| dir.join(path));
    let outcome = daemon::ask(&home, &["changes", &absolute.to_string_lossy()])
        .unwrap_or_else(|| report(&absolute, &home, &mut Snapshots::new()));
    outcome.print()
}

/// What changed in the program at `path` since its last snapshot, which comes from `memory`
/// or else from the cache. The new snapshot goes to both.
pub(crate) fn report(path: &Path, home: &Path, memory: &mut Snapshots) -> Outcome {
    let (program, files) = match program_files(path) {
        Ok(found) => found,
        Err(message) => return Outcome::failure(format!("error: {message}\n")),
    };
    let previous = memory.remove(&program).unwrap_or_else(|| fennec_build::load(home, &program));
    let first_run = previous.files.is_empty();
    let run = Snapshot::take(&files, &previous);
    if let Err(err) = fennec_build::save(home, &program, &run.snapshot) {
        return Outcome::failure(format!(
            "error: couldn't save this run's hashes in {}: {err}\n",
            home.display()
        ));
    }
    let errors = run
        .errors
        .iter()
        .map(|(file, diagnostic)| {
            let text = files.iter().find(|(path, _)| path == file).map_or("", |(_, text)| text.as_str());
            let (line, column) = crate::fmt::position(text, diagnostic.span.start);
            format!("error: {file} doesn't parse ({line}:{column}: {})\n", diagnostic.message)
        })
        .collect::<Vec<_>>()
        .concat();
    let outcome = Outcome {
        stdout: summary(&run, &previous, first_run),
        code: u8::from(!errors.is_empty()),
        stderr: errors,
    };
    memory.insert(program, run.snapshot);
    outcome
}

fn summary(run: &Run, previous: &Snapshot, first_run: bool) -> String {
    if first_run {
        let definitions: usize = run.snapshot.files.values().map(|file| file.definitions.len()).sum();
        return format!(
            "first run: hashed {} files and {definitions} definitions\n",
            run.snapshot.files.len()
        );
    }
    let reused = run.reused.len();
    let mut lines =
        vec![format!("{} parsed, {reused} unchanged and not parsed again", files(run.parsed.len()))];
    let found = fennec_build::changes(previous, &run.snapshot);
    if found.is_empty() {
        lines.push("nothing changed".to_owned());
    }
    let mut last_file = "";
    for change in &found {
        match &change.definition {
            Some(name) => {
                if change.file != last_file {
                    lines.push(change.file.clone());
                    last_file = &change.file;
                }
                lines.push(format!("  {:<8} {name}", word(change)));
            }
            None => lines.push(format!("{}: {}", change.file, word(change))),
        }
    }
    lines.join("\n") + "\n"
}

fn files(count: usize) -> String {
    if count == 1 { "1 file".to_owned() } else { format!("{count} files") }
}

fn word(change: &Change) -> &'static str {
    match change.kind {
        Kind::Added => "added",
        Kind::Removed => "removed",
        Kind::Changed => "changed",
        Kind::Reworded => "only its layout or comments changed",
    }
}

/// The program at `path` and its files, each with the path it's shown by: one `.fen` file,
/// or every file under the `src` folder of the kit around a folder.
fn program_files(path: &Path) -> Result<(PathBuf, Vec<(String, String)>), String> {
    let read = |file: &Path| {
        std::fs::read_to_string(file).map_err(|err| format!("couldn't read {}: {err}", file.display()))
    };
    if !path.is_dir() {
        let shown = path.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        let text = read(path)?;
        return Ok((std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()), vec![(shown, text)]));
    }
    let absolute =
        std::fs::canonicalize(path).map_err(|err| format!("couldn't read {}: {err}", path.display()))?;
    let root = absolute
        .ancestors()
        .find(|folder| folder.join("kit.toml").is_file())
        .ok_or_else(|| format!("there's no kit.toml in {} or any folder above it", path.display()))?;
    let mut found = Vec::new();
    sources(&root.join("src"), &mut found);
    found.sort();
    let mut files = Vec::with_capacity(found.len());
    for file in found {
        let shown = file.strip_prefix(root).unwrap_or(&file).to_string_lossy().replace('\\', "/");
        files.push((shown, read(&file)?));
    }
    Ok((root.to_path_buf(), files))
}

/// Every `.fen` file under `dir`.
fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for path in entries.flatten().map(|entry| entry.path()) {
        if path.is_dir() {
            sources(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "fen") {
            found.push(path);
        }
    }
}
