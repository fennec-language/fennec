//! A grep, the twin of `grep_tool.fen`: the project's tree written with `std::fs`, walked in
//! sorted order, each file read in chunks through a `File` and cut into lines, and every line
//! matched against the patterns on the engine behind Fennec's `std.re`.

use fennec_re as re;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs::File;
use std::hint::black_box;
use std::io::Read;
use std::path::Path;

const ROOT: &str = "target/bench-grep-tool";
const PROJECTS: [&str; 4] = ["engine", "server", "tools", "editor"];
const FOLDERS: i64 = 4;
const FILES: i64 = 6;
const LINES: i64 = 60;
const CHUNK: usize = 4096;
const OWNERS: [&str; 5] = ["fox", "kit", "tod", "vix", "reynard"];
const VERBS: [&str; 6] = ["get", "put", "load", "save", "send", "find"];
const NOUNS: [&str; 8] = ["user", "order", "item", "room", "tile", "path", "frame", "mesh"];

/// The next state of a small linear congruential generator, the same on every machine.
fn next(seed: i64) -> i64 {
    (seed * 1_103_515_245 + 12_345) % 2_147_483_648
}

/// A number from 0 up to `top` taken from a generator's state.
fn below(seed: i64, top: i64) -> i64 {
    seed / 65_536 % top
}

/// A source file's text, made from a generator's state.
fn source(seed: i64) -> String {
    let mut state = seed;
    let mut lines = Vec::new();
    for number in 0..LINES {
        state = next(state);
        let roll = below(state, 100);
        state = next(state);
        let verb = VERBS[below(state, VERBS.len() as i64) as usize];
        let noun = NOUNS[(below(state, NOUNS.len() as i64 * 7) % NOUNS.len() as i64) as usize];
        if roll < 12 {
            lines.push(format!("fn {noun}_{verb}(id: u32, limit: u32) -> Result<{noun}, Error> {{"));
        } else if roll < 20 {
            let owner = OWNERS[(number % OWNERS.len() as i64) as usize];
            lines.push(format!("    // TODO({owner}): {verb} the {noun} without a copy"));
        } else if roll < 27 {
            lines.push(format!("    log.warn(\"{noun} {verb}: timeout after {}ms\")", state % 900 + 100));
        } else if roll < 40 {
            lines.push(format!("    let mask = {}; // {noun} flags", state % 65_536));
        } else if roll < 55 {
            lines.push("    }".to_owned());
        } else if roll < 60 {
            lines.push(String::new());
        } else {
            lines.push(format!("    let {noun} = store.{verb}_{noun}(id + {roll}, limit)?;"));
        }
    }
    lines.join("\n") + "\n"
}

/// Writes the project's tree, every file at its own path under `ROOT`.
fn make_tree(seed: i64) -> std::io::Result<()> {
    let mut state = seed;
    for project in PROJECTS {
        for folder in 0..FOLDERS {
            let here = format!("{ROOT}/{project}/part{folder}");
            std::fs::create_dir_all(&here)?;
            for file in 0..FILES {
                state = next(state);
                std::fs::write(format!("{here}/file{file}.src"), source(state))?;
            }
        }
    }
    Ok(())
}

/// Every path under `folder`, at any depth, in sorted order, a folder before what's in it.
fn walk(folder: &Path, found: &mut Vec<String>) -> std::io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(folder)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        found.push(path.to_string_lossy().into_owned());
        if entry.file_type()?.is_dir() {
            walk(&path, found)?;
        }
    }
    Ok(())
}

/// What the patterns found in the whole tree.
#[derive(Default)]
struct Counts {
    lines: i64,
    bytes: i64,
    by_pattern: Vec<i64>,
    files_with: Vec<i64>,
    by_file: HashMap<String, i64>,
    owners: HashMap<String, i64>,
    first: Vec<String>,
    places: i64,
}

/// Matches one line against every pattern and counts what it finds.
fn match_line(patterns: &[re::Regex], path: &str, number: i64, line: &str, counts: &mut Counts, seen: &mut [bool]) {
    counts.lines += 1;
    for (at, pattern) in patterns.iter().enumerate() {
        let Some(found) = pattern.find_at(line, 0, re::Anchor::Search) else { continue };
        let (start, _) = found.whole();
        counts.by_pattern[at] += 1;
        *counts.by_file.entry(path.to_owned()).or_insert(0) += 1;
        counts.places = (counts.places * 31 + number * 7 + at as i64 + start as i64) % 1_000_000_007;
        if !seen[at] {
            seen[at] = true;
            counts.files_with[at] += 1;
        }
        if counts.first[at].is_empty() {
            counts.first[at] = format!("{path}:{number}: {}", line.trim());
        }
        if at == 0 {
            let owner = found.groups[1].map_or("", |(from, to)| &line[from..to]);
            *counts.owners.entry(owner.to_owned()).or_insert(0) += 1;
        }
    }
}

/// Reads one file in chunks, cutting them into lines, and matches every line.
fn grep_file(patterns: &[re::Regex], path: &str, counts: &mut Counts) -> std::io::Result<()> {
    let mut seen = vec![false; patterns.len()];
    let mut carry = String::new();
    let mut number = 0;
    let mut file = File::open(path)?;
    let mut chunk = vec![0; CHUNK];
    loop {
        let read = file.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        counts.bytes += read as i64;
        carry.push_str(std::str::from_utf8(&chunk[..read]).expect("the tree is ASCII"));
        let mut rest = carry.as_str();
        while let Some(end) = rest.find('\n') {
            number += 1;
            match_line(patterns, path, number, &rest[..end], counts, &mut seen);
            rest = &rest[end + 1..];
        }
        carry = rest.to_owned();
    }
    if !carry.is_empty() {
        number += 1;
        match_line(patterns, path, number, &carry, counts, &mut seen);
    }
    Ok(())
}

fn main() -> std::io::Result<()> {
    let started = std::time::Instant::now();
    make_tree(black_box(17))?;
    let sources = [r"TODO\((\w+)\)", r"\btimeout after \d+ms", r"mask = \d{3,5};", r"^fn \w+_(get|put)\("];
    let patterns: Vec<re::Regex> =
        sources.iter().map(|source| re::Regex::new(source).expect("the pattern is valid")).collect();
    let mut counts = Counts {
        by_pattern: vec![0; patterns.len()],
        files_with: vec![0; patterns.len()],
        first: vec![String::new(); patterns.len()],
        ..Counts::default()
    };
    let mut paths = Vec::new();
    walk(Path::new(ROOT), &mut paths)?;
    let mut files = 0;
    for path in paths.iter().filter(|path| path.ends_with(".src")) {
        files += 1;
        grep_file(&patterns, path, &mut counts)?;
    }
    let mut report = String::new();
    let _ = writeln!(report, "{files} files {} lines {} bytes", counts.lines, counts.bytes);
    for (at, source) in sources.iter().enumerate() {
        let _ = writeln!(report, "{source} {} lines in {} files", counts.by_pattern[at], counts.files_with[at]);
        let _ = writeln!(report, "  first {}", counts.first[at]);
    }
    let mut busiest: Vec<(i64, &String)> = counts.by_file.iter().map(|(path, count)| (-count, path)).collect();
    busiest.sort();
    for (count, path) in busiest.iter().take(5) {
        let _ = writeln!(report, "busiest {path} {}", -count);
    }
    let mut owners: Vec<&String> = counts.owners.keys().collect();
    owners.sort();
    for owner in owners {
        let _ = writeln!(report, "owner {owner} {}", counts.owners[owner]);
    }
    let _ = writeln!(report, "places {}", counts.places);
    print!("{report}");
    if std::env::args().len() > 1 {
        println!("kernel-ns {}", started.elapsed().as_nanos());
    }
    Ok(())
}
