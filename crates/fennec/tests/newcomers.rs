//! The mistakes a Python programmer makes first in Fennec. Each is an example in
//! `examples/errors/`, held to its `.err` there by `examples.rs`. Here each must also give a fix
//! hint, and say the same through `fennec check --json` and the language server as in the
//! terminal, at the same place, so a newcomer's editor and tools read what the terminal shows.

use fennec_lsp::{Json, Session, uri_of};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Each mistake: its example's name in `examples/errors/`, and the Python habit it's written from.
const MISTAKES: [(&str, &str); 56] = [
    // Missing values.
    ("default_none", "`name: str = None` for a parameter that may be left out"),
    ("return_none_from_plain_type", "`return None` from a function that gives back an `int`"),
    ("union_with_none", "`str | None` for an optional"),
    ("arrow_to_none", "`-> None` on a function that gives back nothing"),
    ("import_typing", "`from typing import Optional`"),
    // Python's own syntax.
    ("lambda", "`lambda name: len(name)`"),
    ("try_except", "`try:` around the code that can fail"),
    ("except_as", "`except InvalidValue as problem:`"),
    ("except_type_without_brackets", "`except InvalidValue:`, which names any failure in Fennec"),
    ("except_bare", "a bare `except:`"),
    ("finally", "`finally:` after a handler"),
    ("raise", "`raise TooBig(size=size)`"),
    ("kwargs", "`**options` for named options"),
    ("star_args", "`*numbers` for any number of arguments"),
    ("star_unpacking", "`first, *rest = numbers`"),
    ("global", "`global count` to change a name at the top of the file"),
    ("nonlocal", "`nonlocal count` to change a name around a nested function"),
    ("del_item", "`del names[0]`"),
    ("walrus", "`if (count := len(names)) > 0:`"),
    ("async_def", "`async def fetch():`"),
    ("dataclass", "`@dataclass` above a struct"),
    ("class", "`class Point:`"),
    ("dunder_init", "`def __init__(self, x, y):`"),
    ("dunder_str", "`def __str__(self):`"),
    ("dunder_name_main", "`if __name__ == \"__main__\":`"),
    ("docstring", "a `\"\"\"docstring\"\"\"` at the top of a function"),
    ("chain_on_next_line", "a chain of calls going on to the next line, as in a builder"),
    ("backslash_continuation", "a `\\` at a line's end to go on to the next"),
    ("semicolon", "two statements on a line with `;`"),
    ("single_quotes", "a string in `'`s"),
    ("chained_assignment", "`a = b = 0`"),
    ("fstring_repr", "`{name!r}` in an f-string"),
    ("assert_message", "`assert count > 0, \"message\"`"),
    ("tabs", "indenting with tabs"),
    ("tabs_mixed_with_spaces", "a tab among lines indented with spaces"),
    // Habits that compile in Python and mean something else here.
    ("used_before_assigned", "a name used above the line that gives it a value"),
    ("mutable_default", "`names=[]` as a default"),
    ("list_not_mut", "`scores.append(4)` on a name that isn't `mut`"),
    ("loop_item_not_ref", "changing each item in `for player in players:`"),
    ("mut_self", "changing `self.health` in a method"),
    ("self_not_a_parameter", "`self` used in a method that doesn't take it"),
    ("print", "`print(...)` with no `stdout`"),
    ("input", "`input(\"name? \")`"),
    ("open", "`with open(path) as file:`"),
    ("exit", "`exit(1)`"),
    ("isinstance", "`isinstance(shape, Circle)`"),
    ("import_os", "`import os`"),
    ("percent_format", "`\"%d items\" % count`"),
    ("format_method", "`\"{} items\".format(count)`"),
    ("add_int_and_str", "`\"total: \" + count`"),
    ("truthiness", "`if names:` for a list that isn't empty"),
    ("dict_get_with_a_default", "`ages.get(name, 0)`"),
    ("int_of_text", "`int(\"42\")` to read a number"),
    ("failure_not_handled", "a call that can fail, with nothing around it"),
    ("value_without_return", "a function's last value with no `return`"),
    ("print_with_end", "`stdout.print(text, end=\"\")`"),
];

/// Mistakes whose message gives no fix hint yet, which is the checker's to add: the tooling lane's
/// audit gave the checker lane the list. Each comes out of here as its hint lands.
const NO_HINT_YET: [&str; 2] = ["format_method", "value_without_return"];

/// What a message says, where, and its fix hint: the same in every tool.
#[derive(Debug, PartialEq)]
struct Said {
    message: String,
    line: usize,
    column: usize,
    help: Option<String>,
    fix: Option<String>,
}

#[test]
fn a_python_programmers_first_mistakes_each_say_what_to_write_in_every_tool() {
    let mut editor = Editor::new().unwrap();
    for name in NO_HINT_YET {
        assert!(MISTAKES.iter().any(|(mistake, _)| *mistake == name), "{name} isn't one of the mistakes");
    }
    for (name, python) in MISTAKES {
        let path = format!("examples/errors/{name}.fen");
        let source = std::fs::read_to_string(repo().join(&path)).unwrap();
        let expected = std::fs::read_to_string(repo().join(format!("examples/errors/{name}.err"))).unwrap();
        let said = said_in_text(&expected);
        assert!(!said.is_empty(), "{name}, {python}, is refused with nothing:\n{expected}");
        if !NO_HINT_YET.contains(&name) {
            assert!(
                said.iter().all(|one| one.help.is_some() || one.fix.is_some()),
                "{name}, {python}, gives no fix hint:\n{expected}"
            );
        }
        let checked = fennec(&["check", &path]).unwrap();
        assert_eq!(String::from_utf8_lossy(&checked.stderr), expected, "`fennec check` on {name}, {python}");
        assert_eq!(editor.diagnostics(&source), said, "the language server on {name}, {python}");
        let json = fennec(&["check", "--json", &path]).unwrap();
        assert_eq!(
            said_in_json(&String::from_utf8_lossy(&json.stdout)),
            Some(said),
            "the JSON of {name}, {python}"
        );
    }
}

/// Each message in a rendered `.err`: its `error:` line, its `-->` place, and the `= help:` and
/// `= fix:` lines under it.
fn said_in_text(rendered: &str) -> Vec<Said> {
    let mut found: Vec<Said> = Vec::new();
    for line in rendered.lines() {
        let trimmed = line.trim_start();
        if let Some(message) = line.strip_prefix("error: ") {
            found.push(Said { message: message.to_owned(), line: 0, column: 0, help: None, fix: None });
        } else if let (Some(place), Some(last)) = (trimmed.strip_prefix("--> "), found.last_mut()) {
            let mut parts = place.rsplitn(3, ':');
            last.column = parts.next().and_then(|column| column.parse().ok()).unwrap_or(0);
            last.line = parts.next().and_then(|line| line.parse().ok()).unwrap_or(0);
        } else if let (Some(help), Some(last)) = (trimmed.strip_prefix("= help: "), found.last_mut()) {
            last.help = Some(help.to_owned());
        } else if let (Some(fix), Some(last)) = (trimmed.strip_prefix("= fix: "), found.last_mut()) {
            last.fix = Some(fix.to_owned());
        }
    }
    found
}

/// Each diagnostic in `fennec check --json`'s report, or `None` when it isn't JSON.
fn said_in_json(report: &str) -> Option<Vec<Said>> {
    let report = Json::parse(report.trim())?;
    let diagnostics = report.get("diagnostics").and_then(Json::as_array).unwrap_or(&[]);
    let text =
        |diagnostic: &Json, path: &[&str]| diagnostic.at(path).and_then(Json::as_str).map(str::to_owned);
    let said = diagnostics
        .iter()
        .map(|diagnostic| Said {
            message: text(diagnostic, &["message"]).unwrap_or_default(),
            line: diagnostic.get("line").and_then(Json::as_usize).unwrap_or(0),
            column: diagnostic.get("column").and_then(Json::as_usize).unwrap_or(0),
            help: text(diagnostic, &["help"]),
            fix: text(diagnostic, &["hint", "says"]),
        })
        .collect();
    Some(said)
}

/// `fennec` with `args`, run in the repository, on nothing it kept from before.
fn fennec(args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new(env!("CARGO_BIN_EXE_fennec"))
        .args(args)
        .current_dir(repo())
        .env("FENNEC_NO_CACHE", "1")
        .env("FENNEC_NO_DAEMON", "1")
        .output()
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// An editor with a kit open, whose `main.fen` it changes to each program in turn.
struct Editor {
    session: Session,
    dir: fennec_temp::Temp,
    version: usize,
}

impl Editor {
    fn new() -> std::io::Result<Self> {
        let dir = std::env::temp_dir().join(format!("fennec-newcomers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src"))?;
        let manifest =
            format!("name = \"demo\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n", env!("CARGO_PKG_VERSION"));
        std::fs::write(dir.join("kit.toml"), manifest)?;
        std::fs::write(dir.join("src/main.fen"), "def main(stdout: Stdout):\n    pass\n")?;
        let dir = fennec_temp::Temp::at(std::fs::canonicalize(dir)?);
        let mut editor = Editor { session: Session::new(), dir, version: 0 };
        editor.send(Some(1), "initialize", Json::object([]));
        let document = Json::object([
            ("uri", Json::string(editor.uri())),
            ("languageId", Json::string("fennec")),
            ("version", Json::number(0)),
            ("text", Json::string("def main(stdout: Stdout):\n    pass\n")),
        ]);
        editor.send(None, "textDocument/didOpen", Json::object([("textDocument", document)]));
        Ok(editor)
    }

    fn uri(&self) -> String {
        uri_of(&self.dir.join("src/main.fen"))
    }

    fn send(&mut self, id: Option<usize>, method: &str, params: Json) -> Vec<Json> {
        let mut fields =
            vec![("jsonrpc".to_owned(), Json::string("2.0")), ("method".to_owned(), Json::string(method))];
        if let Some(id) = id {
            fields.push(("id".to_owned(), Json::number(id)));
        }
        fields.push(("params".to_owned(), params));
        self.session.handle(&Json::Object(fields))
    }

    /// The errors the server publishes for `main.fen` once it holds `text`, each split back into
    /// its message, help and fix, which the protocol puts in one message.
    fn diagnostics(&mut self, text: &str) -> Vec<Said> {
        self.version += 1;
        let document =
            Json::object([("uri", Json::string(self.uri())), ("version", Json::number(self.version))]);
        let changes = Json::Array(vec![Json::object([("text", Json::string(text))])]);
        let params = Json::object([("textDocument", document), ("contentChanges", changes)]);
        let uri = self.uri();
        let published = self.send(None, "textDocument/didChange", params);
        let mine =
            published.iter().find(|one| one.at(&["params", "uri"]).and_then(Json::as_str) == Some(&uri));
        let diagnostics =
            mine.and_then(|one| one.at(&["params", "diagnostics"])).and_then(Json::as_array).unwrap_or(&[]);
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.get("severity").and_then(Json::as_usize) == Some(1))
            .map(|diagnostic| {
                let whole = diagnostic.get("message").and_then(Json::as_str).unwrap_or("");
                let (rest, fix) = match whole.split_once("\nfix: ") {
                    Some((rest, fix)) => (rest, Some(fix.to_owned())),
                    None => (whole, None),
                };
                let (message, help) = match rest.split_once("\nhelp: ") {
                    Some((message, help)) => (message, Some(help.to_owned())),
                    None => (rest, None),
                };
                let start = |field: &str| {
                    diagnostic.at(&["range", "start", field]).and_then(Json::as_usize).map_or(0, |at| at + 1)
                };
                Said {
                    message: message.to_owned(),
                    line: start("line"),
                    column: start("character"),
                    help,
                    fix,
                }
            })
            .collect()
    }
}
