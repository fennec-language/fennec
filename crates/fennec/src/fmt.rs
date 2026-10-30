//! `fennec fmt`: formats files in place, or with `--check` lists the ones that aren't
//! formatted. A folder means every `.fen` file under it. With no path, it formats the kit
//! around the current folder, or the current folder if it isn't in a kit.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Folders a walk never goes into: build output and hidden folders like `.git`.
const SKIPPED: [&str; 2] = ["target", "node_modules"];

pub(crate) fn fmt(args: &[&str]) -> ExitCode {
    let check = args.contains(&"--check");
    let mut paths: Vec<&str> = args.iter().copied().filter(|arg| *arg != "--check").collect();
    if let Some(flag) = paths.iter().find(|arg| arg.starts_with('-')) {
        eprintln!("error: `fennec fmt` has no option `{flag}`; it has one style and only `--check`");
        return ExitCode::from(2);
    }
    let kit = kit_root(Path::new("."));
    if paths.is_empty() {
        paths.push(kit.as_deref().and_then(Path::to_str).unwrap_or("."));
    }
    let mut files = Vec::new();
    for path in paths {
        if let Err(err) = collect(Path::new(path), true, &mut files) {
            eprintln!("error: couldn't read {path}: {err}");
            return ExitCode::FAILURE;
        }
    }
    let mut unformatted = 0;
    let mut failed = false;
    for file in &files {
        match format_file(file, check) {
            Ok(changed) => unformatted += usize::from(changed),
            Err(()) => failed = true,
        }
    }
    if check && unformatted > 0 {
        let (files, them) = if unformatted == 1 { ("file isn't", "it") } else { ("files aren't", "them") };
        eprintln!("{unformatted} {files} formatted; run `fennec fmt` to format {them}");
        failed = true;
    }
    if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}

/// The kit `dir` is in: the nearest folder at or above it with a `kit.toml`, written relative
/// to `dir`, like `../..`.
fn kit_root(dir: &Path) -> Option<PathBuf> {
    let absolute = std::fs::canonicalize(dir).ok()?;
    let (levels, _) =
        absolute.ancestors().enumerate().find(|(_, folder)| folder.join("kit.toml").is_file())?;
    let mut root = dir.to_path_buf();
    for _ in 0..levels {
        root.push("..");
    }
    Some(root)
}

/// Adds `path` if it's a file, or the `.fen` files under it if it's a folder, sorted.
fn collect(path: &Path, named: bool, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if !path.is_dir() {
        if named || path.extension().is_some_and(|ext| ext == "fen") {
            files.push(path.to_path_buf());
        }
        return Ok(());
    }
    let name = path.file_name().map(|name| name.to_string_lossy()).unwrap_or_default();
    if !named && (name.starts_with('.') || SKIPPED.contains(&name.as_ref())) {
        return Ok(());
    }
    let mut entries = std::fs::read_dir(path)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort();
    for entry in entries {
        collect(&entry, false, files)?;
    }
    Ok(())
}

/// Formats one file. Returns whether it wasn't formatted already; with `check`, it's only
/// listed, not rewritten. A file with a syntax error is skipped with a note.
fn format_file(path: &Path, check: bool) -> Result<bool, ()> {
    let shown = path.display();
    let source =
        std::fs::read_to_string(path).map_err(|err| eprintln!("error: couldn't read {shown}: {err}"))?;
    let formatted = match fennec_fmt::format(&source) {
        Ok(formatted) => formatted,
        Err(fennec_fmt::Error::Syntax(diagnostic)) => {
            let (line, column) = position(&source, diagnostic.span.start);
            eprintln!("skipped {shown}: it doesn't parse ({line}:{column}: {})", diagnostic.message);
            return Ok(false);
        }
        Err(fennec_fmt::Error::WouldChange(what)) => {
            eprintln!("error: formatting {shown} would change its {what}, so it was left alone");
            eprintln!("  this is a bug in `fennec fmt`; please report it with the file");
            return Err(());
        }
    };
    if formatted == source {
        return Ok(false);
    }
    if check {
        println!("{shown}");
    } else {
        std::fs::write(path, formatted).map_err(|err| eprintln!("error: couldn't write {shown}: {err}"))?;
        println!("formatted {shown}");
    }
    Ok(true)
}

/// The one-based line and column of a byte offset.
fn position(source: &str, offset: u32) -> (usize, usize) {
    let offset = usize::try_from(offset).unwrap_or(0).min(source.len());
    let before = source.get(..offset).unwrap_or("");
    let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
    let column = before.get(line_start..).map_or(0, |text| text.chars().count());
    (before.matches('\n').count() + 1, column + 1)
}
