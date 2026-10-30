//! `fennec dev changes [path]`: what changed since the last time it ran on this program,
//! file by file and definition by definition, from the build engine's content hashes. A
//! file whose bytes are unchanged isn't parsed again.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use fennec_build::{Change, Kind, Run, Snapshot};

pub(crate) fn changes(path: &str) -> ExitCode {
    let Some(home) = fennec_build::fennec_home() else {
        eprintln!("error: couldn't find a home folder; set FENNEC_HOME");
        return ExitCode::FAILURE;
    };
    let (program, files) = match program_files(Path::new(path)) {
        Ok(found) => found,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::FAILURE;
        }
    };
    let previous = fennec_build::load(&home, &program);
    let first_run = previous.files.is_empty();
    let run = Snapshot::take(&files, &previous);
    if let Err(err) = fennec_build::save(&home, &program, &run.snapshot) {
        eprintln!("error: couldn't save this run's hashes in {}: {err}", home.display());
        return ExitCode::FAILURE;
    }
    report(&run, &previous, first_run);
    for (file, diagnostic) in &run.errors {
        let text = files.iter().find(|(path, _)| path == file).map_or("", |(_, text)| text.as_str());
        let (line, column) = crate::fmt::position(text, diagnostic.span.start);
        eprintln!("error: {file} doesn't parse ({line}:{column}: {})", diagnostic.message);
    }
    if run.errors.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

fn report(run: &Run, previous: &Snapshot, first_run: bool) {
    if first_run {
        let definitions: usize = run.snapshot.files.values().map(|file| file.definitions.len()).sum();
        println!("first run: hashed {} files and {definitions} definitions", run.snapshot.files.len());
        return;
    }
    let reused = run.reused.len();
    println!("{} parsed, {reused} unchanged and not parsed again", files(run.parsed.len()));
    let found = fennec_build::changes(previous, &run.snapshot);
    if found.is_empty() {
        println!("nothing changed");
    }
    let mut last_file = "";
    for change in &found {
        match &change.definition {
            Some(name) => {
                if change.file != last_file {
                    println!("{}", change.file);
                    last_file = &change.file;
                }
                println!("  {:<8} {name}", word(change));
            }
            None => println!("{}: {}", change.file, word(change)),
        }
    }
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
        return Ok((path.to_path_buf(), vec![(shown, read(path)?)]));
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
