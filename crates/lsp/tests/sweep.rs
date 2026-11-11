//! The server on every example and on the self-hosted front end, file by file: each opens with no
//! errors but the ones it's meant to have, and every name that goes to a definition goes to one
//! that names it. `survey` says how much of each file has a definition and a hover, and how long
//! each took.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fennec_lsp::{Json, Session, path_of, uri_of};

/// Words that are the language's own, which have nothing to go to.
const KEYWORDS: &[&str] = &[
    "and",
    "as",
    "assert",
    "break",
    "case",
    "continue",
    "def",
    "elif",
    "else",
    "enum",
    "error",
    "except",
    "export",
    "extend",
    "fail",
    "fails",
    "False",
    "for",
    "from",
    "if",
    "import",
    "in",
    "interface",
    "is",
    "match",
    "mut",
    "None",
    "not",
    "or",
    "pass",
    "ref",
    "return",
    "self",
    "Self",
    "shared",
    "struct",
    "test",
    "True",
    "type",
    "where",
    "while",
    "with",
    "yield",
    "parallel",
    "_",
];

fn repo() -> std::io::Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize()
}

/// Every `.fen` file of the examples and of the self-hosted front end, in order.
fn programs() -> std::io::Result<Vec<PathBuf>> {
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) -> std::io::Result<()> {
        let mut paths: Vec<PathBuf> =
            std::fs::read_dir(dir)?.map(|entry| entry.map(|entry| entry.path())).collect::<Result<_, _>>()?;
        paths.sort();
        for path in paths {
            if path.is_dir() {
                walk(&path, found)?;
            } else if path.extension().is_some_and(|extension| extension == "fen") {
                found.push(path);
            }
        }
        Ok(())
    }
    let root = repo()?;
    let mut found = Vec::new();
    for folder in ["examples", "selfhost/src"] {
        walk(&root.join(folder), &mut found)?;
    }
    Ok(found)
}

/// Each name in `text` outside comments and strings, by line and column in UTF-16 code units.
fn names(text: &str) -> Vec<(usize, usize, String)> {
    let mut found = Vec::new();
    for (line, content) in text.lines().enumerate() {
        let (mut quote, mut column, mut start, mut word) = (None, 0, 0, String::new());
        let mut chars = content.chars().peekable();
        while let Some(ch) = chars.next() {
            if quote.is_none() && ch == '#' {
                break;
            }
            let was_quoted = quote.is_some();
            match quote {
                Some(_) if ch == '\\' => {
                    column += chars.next().map_or(0, char::len_utf16);
                }
                Some(open) if ch == open => quote = None,
                None if ch == '"' || ch == '\'' => {
                    quote = Some(ch);
                    // `f"..."`: the `f` is part of the string.
                    if word == "f" || word == "b" || word == "r" {
                        word.clear();
                    }
                }
                _ => {}
            }
            if !was_quoted && quote.is_none() && (ch.is_alphanumeric() || ch == '_') {
                if word.is_empty() {
                    start = column;
                }
                word.push(ch);
            } else if !word.is_empty() {
                found.push((line, start, std::mem::take(&mut word)));
            }
            column += ch.len_utf16();
        }
        if !word.is_empty() {
            found.push((line, start, word));
        }
    }
    found.retain(|(_, _, word)| {
        !word.starts_with(|ch: char| ch.is_ascii_digit()) && !KEYWORDS.contains(&word.as_str())
    });
    found
}

/// A session on one file.
struct Editor {
    session: Session,
    requests: usize,
    uri: String,
}

impl Editor {
    fn request(&mut self, method: &str, params: Json) -> Option<Json> {
        self.requests += 1;
        let message = Json::object([
            ("jsonrpc", Json::string("2.0")),
            ("id", Json::number(self.requests)),
            ("method", Json::string(method)),
            ("params", params),
        ]);
        let id = Json::number(self.requests);
        let answers = self.session.handle(&message);
        let answer = answers.into_iter().find(|answer| answer.get("id") == Some(&id))?;
        answer.get("result").filter(|result| **result != Json::Null).cloned()
    }

    /// Opens the file, giving back the errors published for it.
    fn open(path: &Path, text: &str) -> (Editor, Vec<String>) {
        let mut editor = Editor { session: Session::new(), requests: 0, uri: uri_of(path) };
        editor.request("initialize", Json::object([]));
        let document = Json::object([
            ("uri", Json::string(editor.uri.as_str())),
            ("languageId", Json::string("fennec")),
            ("version", Json::number(1)),
            ("text", Json::string(text)),
        ]);
        let message = Json::object([
            ("jsonrpc", Json::string("2.0")),
            ("method", Json::string("textDocument/didOpen")),
            ("params", Json::object([("textDocument", document)])),
        ]);
        let mut errors = Vec::new();
        for published in editor.session.handle(&message) {
            let diagnostics =
                published.at(&["params", "diagnostics"]).and_then(Json::as_array).unwrap_or(&[]);
            for diagnostic in diagnostics {
                if diagnostic.get("severity").and_then(Json::as_usize).unwrap_or(1) == 1 {
                    errors.push(diagnostic.get("message").and_then(Json::as_str).unwrap_or("").to_owned());
                }
            }
        }
        (editor, errors)
    }

    fn at(&self, line: usize, column: usize) -> Json {
        Json::object([
            ("textDocument", Json::object([("uri", Json::string(self.uri.as_str()))])),
            ("position", Json::object([("line", Json::number(line)), ("character", Json::number(column))])),
        ])
    }
}

/// The text a location's range covers, read from its file.
fn covered(location: &Json) -> Option<String> {
    let path = path_of(location.get("uri")?.as_str()?)?;
    let text = std::fs::read_to_string(path).ok()?;
    let line = location.at(&["range", "start", "line"])?.as_usize()?;
    let from = location.at(&["range", "start", "character"])?.as_usize()?;
    let to = location.at(&["range", "end", "character"])?.as_usize()?;
    let content: Vec<u16> = text.lines().nth(line)?.encode_utf16().collect();
    Some(String::from_utf16_lossy(content.get(from..to.max(from))?))
}

/// What one file came to.
#[derive(Default)]
struct Seen {
    names: usize,
    defined: usize,
    hovered: usize,
    errors: Vec<String>,
    wrong: Vec<String>,
    undefined: Vec<String>,
    slowest: Duration,
    opened: Duration,
}

/// Opens `path` and asks for each name's definition, and its hover when `hovers` says so.
fn seen(path: &Path, root: &Path, hovers: bool) -> std::io::Result<Seen> {
    let text = std::fs::read_to_string(path)?;
    let shown = path.strip_prefix(root).unwrap_or(path).display().to_string();
    let started = Instant::now();
    let (mut editor, errors) = Editor::open(path, &text);
    let mut seen = Seen { opened: started.elapsed(), ..Seen::default() };
    // The examples that are meant to fail are the ones in an `errors` folder.
    if path.strip_prefix(root).unwrap_or(path).components().any(|part| part.as_os_str() == "errors") {
        return Ok(seen);
    }
    seen.errors = errors.into_iter().map(|error| format!("{shown}: {error}")).collect();
    for (line, column, word) in names(&text) {
        seen.names += 1;
        let place = format!("{shown}:{}:{} `{word}`", line + 1, column + 1);
        let started = Instant::now();
        let location = match editor.request("textDocument/definition", editor.at(line, column)) {
            Some(Json::Array(items)) => items.first().cloned(),
            other => other,
        };
        if hovers && editor.request("textDocument/hover", editor.at(line, column)).is_some() {
            seen.hovered += 1;
        }
        seen.slowest = seen.slowest.max(started.elapsed());
        let Some(location) = location else {
            seen.undefined.push(place);
            continue;
        };
        seen.defined += 1;
        let named = covered(&location);
        // A module goes to the start of its file, and an alias to the name it renames.
        let module = named.as_deref() == Some("");
        let alias = text.contains(&format!(" as {word}"));
        if !module && !alias && !named.as_deref().is_some_and(|named| named.contains(word.as_str())) {
            seen.wrong.push(format!("{place} goes to {named:?}"));
        }
    }
    Ok(seen)
}

/// Every file seen, on every core.
fn all_seen(hovers: bool) -> std::io::Result<Vec<(PathBuf, Seen)>> {
    let root = repo()?;
    let files = programs()?;
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                while let Some(path) = files.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed)) {
                    let found = seen(path, &root, hovers);
                    results
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push((path.clone(), found));
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner);
    results.sort_by(|a, b| a.0.cmp(&b.0));
    results.into_iter().map(|(path, found)| found.map(|found| (path, found))).collect()
}

#[test]
fn every_file_opens_clean_and_each_definition_names_what_was_asked() {
    let results = all_seen(false).unwrap();
    assert!(results.len() > 300, "found only {} files", results.len());
    let errors: Vec<&String> = results.iter().flat_map(|(_, seen)| &seen.errors).collect();
    assert!(errors.is_empty(), "errors where none are meant:\n{errors:#?}");
    let wrong: Vec<&String> = results.iter().flat_map(|(_, seen)| &seen.wrong).collect();
    assert!(wrong.is_empty(), "definitions that don't name what was asked:\n{wrong:#?}");
}

#[test]
#[ignore = "a survey: run with --ignored --nocapture"]
fn survey() {
    let results = all_seen(true).unwrap();
    let total = |count: fn(&Seen) -> usize| results.iter().map(|(_, seen)| count(seen)).sum::<usize>();
    println!(
        "{} files, {} names: {} have a definition, {} a hover",
        results.len(),
        total(|seen| seen.names),
        total(|seen| seen.defined),
        total(|seen| seen.hovered)
    );
    let mut undefined: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for place in results.iter().flat_map(|(_, seen)| &seen.undefined) {
        *undefined.entry(place.rsplit('`').nth(1).unwrap_or("")).or_default() += 1;
    }
    let mut common: Vec<_> = undefined.into_iter().collect();
    common.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    println!("most common with no definition: {:?}", common.iter().take(40).collect::<Vec<_>>());
    let slowest = results.iter().max_by_key(|(_, seen)| seen.slowest);
    let opened = results.iter().max_by_key(|(_, seen)| seen.opened);
    println!("slowest definition and hover: {:?}", slowest.map(|(path, seen)| (seen.slowest, path)));
    println!("slowest to open: {:?}", opened.map(|(path, seen)| (seen.opened, path)));
}
