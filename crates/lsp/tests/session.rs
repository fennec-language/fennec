//! An editor's session with the server, message by message, on a small kit.

use std::path::PathBuf;

use fennec_lsp::{Json, Session, uri_of};

const MAIN: &str = "from combat import Fighter, hit

## The arena's size.
SIZE = 10

def main(stdout: Stdout):
    fighter = Fighter(name=\"Kit\")
    damage = hit(fighter, 3)
    stdout.print(fighter.describe(), damage, SIZE)
";

const COMBAT: &str = "## Someone in the arena.
struct Fighter:
    name: str
    health = 100

    ## A line about this fighter.
    def describe(self) -> str:
        f\"{self.name}: {self.health}\"

## How much a hit does,
## doubled.
def hit(
    fighter: Fighter,
    power: int,
) -> int:
    power * 2
";

/// An editor talking to a session, with a kit on disk.
struct Editor {
    session: Session,
    dir: PathBuf,
    requests: usize,
}

impl Editor {
    fn new(name: &str) -> std::io::Result<Self> {
        let dir = std::env::temp_dir().join(format!("fennec-lsp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src"))?;
        let manifest =
            format!("name = \"demo\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n", env!("CARGO_PKG_VERSION"));
        std::fs::write(dir.join("kit.toml"), manifest)?;
        std::fs::write(dir.join("src/main.fen"), MAIN)?;
        std::fs::write(dir.join("src/combat.fen"), COMBAT)?;
        let dir = std::fs::canonicalize(dir)?;
        let mut editor = Editor { session: Session::new(), dir, requests: 0 };
        editor.request("initialize", Json::object([]));
        Ok(editor)
    }

    fn uri(&self, file: &str) -> String {
        uri_of(&self.dir.join(file))
    }

    fn request(&mut self, method: &str, params: Json) -> Json {
        self.requests += 1;
        let message = Json::object([
            ("jsonrpc", Json::string("2.0")),
            ("id", Json::number(self.requests)),
            ("method", Json::string(method)),
            ("params", params),
        ]);
        let answers = self.session.handle(&message);
        let response = answers.iter().find(|answer| answer.get("id") == Some(&Json::number(self.requests)));
        response.and_then(|response| response.get("result")).cloned().unwrap_or(Json::string("no result"))
    }

    /// Sends a notification, and gives back the diagnostics published, by file name.
    fn notify(&mut self, method: &str, params: Json) -> Vec<(String, Vec<Json>)> {
        let message = Json::object([
            ("jsonrpc", Json::string("2.0")),
            ("method", Json::string(method)),
            ("params", params),
        ]);
        self.session
            .handle(&message)
            .iter()
            .map(|published| {
                let uri = published.at(&["params", "uri"]).and_then(Json::as_str).unwrap_or("");
                let name = uri.rsplit('/').next().unwrap_or(uri).to_owned();
                let diagnostics =
                    published.at(&["params", "diagnostics"]).and_then(Json::as_array).unwrap_or(&[]);
                (name, diagnostics.to_vec())
            })
            .collect()
    }

    fn open(&mut self, file: &str, text: &str) -> Vec<(String, Vec<Json>)> {
        let document = Json::object([
            ("uri", Json::string(self.uri(file))),
            ("languageId", Json::string("fennec")),
            ("version", Json::number(1)),
            ("text", Json::string(text)),
        ]);
        self.notify("textDocument/didOpen", Json::object([("textDocument", document)]))
    }

    fn change(&mut self, file: &str, text: &str) -> Vec<(String, Vec<Json>)> {
        let document = Json::object([("uri", Json::string(self.uri(file))), ("version", Json::number(2))]);
        let changes = Json::Array(vec![Json::object([("text", Json::string(text))])]);
        self.notify(
            "textDocument/didChange",
            Json::object([("textDocument", document), ("contentChanges", changes)]),
        )
    }

    /// A request about the place in `file` where `needle` starts, plus `into` characters.
    fn at(&mut self, method: &str, file: &str, text: &str, needle: &str, into: usize) -> Json {
        let params = Json::object([
            ("textDocument", Json::object([("uri", Json::string(self.uri(file)))])),
            ("position", position(text, needle, into)),
        ]);
        self.request(method, params)
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The protocol position of `needle`'s first appearance in `text`, plus `into` characters.
fn position(text: &str, needle: &str, into: usize) -> Json {
    let at = text.find(needle).map_or(0, |at| at + into);
    let before = text.get(..at).unwrap_or("");
    let line = before.matches('\n').count();
    let column = at - before.rfind('\n').map_or(0, |newline| newline + 1);
    Json::object([("line", Json::number(line)), ("character", Json::number(column))])
}

fn line_of(location: &Json) -> usize {
    location.at(&["range", "start", "line"]).and_then(Json::as_usize).unwrap_or(usize::MAX)
}

fn messages(diagnostics: &[Json]) -> Vec<String> {
    diagnostics
        .iter()
        .map(|diagnostic| diagnostic.get("message").and_then(Json::as_str).unwrap_or("").to_owned())
        .collect()
}

/// What was published for a file, if anything was.
fn for_file<'a>(published: &'a [(String, Vec<Json>)], file: &str) -> Option<&'a [Json]> {
    published.iter().find(|(name, _)| name == file).map(|(_, diagnostics)| diagnostics.as_slice())
}

#[test]
fn diagnostics_cover_every_file_and_clear_when_fixed() {
    let mut editor = Editor::new("diagnostics").unwrap();
    let broken = MAIN.replace("damage, SIZE", "damage + True, SIZE");
    let published = editor.open("src/main.fen", &broken);
    let in_main = for_file(&published, "main.fen").unwrap();
    assert_eq!(in_main.len(), 1, "{:?}", messages(in_main));
    assert!(messages(in_main)[0].contains("can't use `+`"), "{:?}", messages(in_main));
    assert_eq!(line_of(&in_main[0]), 8);
    assert_eq!(for_file(&published, "combat.fen"), Some(&[][..]));

    let published = editor.change("src/main.fen", MAIN);
    assert_eq!(for_file(&published, "main.fen"), Some(&[][..]));
}

#[test]
fn a_mistake_in_an_edited_body_is_found_and_cleared() {
    let mut editor = Editor::new("edited_body").unwrap();
    editor.open("src/main.fen", MAIN);
    editor.open("src/combat.fen", COMBAT);
    let published = editor.change("src/combat.fen", &COMBAT.replace("power * 2", "power * True"));
    let in_combat = for_file(&published, "combat.fen").unwrap();
    assert_eq!(in_combat.len(), 1, "{:?}", messages(in_combat));
    assert_eq!(line_of(&in_combat[0]), 15, "a line in the edited file");
    let published = editor.change("src/combat.fen", &COMBAT.replace("power * 2", "power * 3"));
    assert_eq!(for_file(&published, "combat.fen"), Some(&[][..]));
}

#[test]
fn a_changed_signature_still_finds_what_it_breaks_in_other_files() {
    let mut editor = Editor::new("edited_signature").unwrap();
    editor.open("src/combat.fen", COMBAT);
    let published = editor.change("src/combat.fen", &COMBAT.replace("power: int,", "power: str,"));
    let in_main = messages(for_file(&published, "main.fen").unwrap());
    assert!(!in_main.is_empty(), "main.fen isn't open, but it's what broke");

    let published = editor.change("src/combat.fen", COMBAT);
    assert_eq!(for_file(&published, "main.fen"), Some(&[][..]), "and it clears when that's undone");
}

#[test]
fn asking_about_code_after_a_quick_edit_looks_at_the_whole_program() {
    let mut editor = Editor::new("after_edit").unwrap();
    editor.open("src/main.fen", MAIN);
    let edited = COMBAT.replace("power * 2", "power * 3");
    editor.open("src/combat.fen", COMBAT);
    editor.change("src/combat.fen", &edited);
    let hit = editor.at("textDocument/hover", "src/main.fen", MAIN, "hit(fighter", 0);
    let shown = hit.at(&["contents", "value"]).and_then(Json::as_str).unwrap();
    assert!(shown.contains("def hit("), "{shown}");
    let found = editor.at("textDocument/definition", "src/main.fen", MAIN, "hit(fighter", 0);
    assert!(found.to_string().contains("combat.fen"), "{found:?}");
}

#[test]
fn unsaved_text_is_checked_in_place_of_the_file() {
    let mut editor = Editor::new("unsaved").unwrap();
    editor.open("src/main.fen", MAIN);
    let published = editor.open("src/combat.fen", &COMBAT.replace("def hit(", "def strike("));
    let in_main = messages(for_file(&published, "main.fen").unwrap());
    assert!(in_main.iter().any(|message| message.contains("hit")), "{in_main:?}");
}

#[test]
fn a_diagnostics_fix_is_a_code_action() {
    let mut editor = Editor::new("fixes").unwrap();
    let text = "def double(x: int):\n    x * 2\n\ndef main(stdout: Stdout):\n    stdout.print(double(2))\n";
    editor.open("src/main.fen", text);
    let params = Json::object([
        ("textDocument", Json::object([("uri", Json::string(editor.uri("src/main.fen")))])),
        ("range", Json::object([("start", position(text, "def", 0)), ("end", position(text, "def", 3))])),
        ("context", Json::object([("diagnostics", Json::Array(Vec::new()))])),
    ]);
    let actions = editor.request("textDocument/codeAction", params);
    let action = &actions.as_array().unwrap()[0];
    assert_eq!(action.get("kind"), Some(&Json::string("quickfix")));
    let edits =
        action.at(&["edit", "changes"]).and_then(|changes| changes.get(&editor.uri("src/main.fen"))).unwrap();
    let edit = &edits.as_array().unwrap()[0];
    assert_eq!(edit.get("newText"), Some(&Json::string(" -> int")));
    assert_eq!(edit.at(&["range", "start"]), Some(&position(text, ":\n", 0)));
}

/// What a published diagnostic's `code` is: a lint's rule.
fn codes(diagnostics: &[Json]) -> Vec<&str> {
    diagnostics.iter().filter_map(|diagnostic| diagnostic.get("code")?.as_str()).collect()
}

#[test]
fn lints_and_copies_show_with_their_rules_and_fixes() {
    let mut editor = Editor::new("lints").unwrap();
    let text = "def one() -> int:\n    return 1\n\ndef main(stdout: Stdout):\n    mut items = [1]\n    \
                held = items\n    items.append(2)\n    stdout.print(held.len(), one())\n";
    let published = editor.open("src/main.fen", text);
    let in_main = for_file(&published, "main.fen").unwrap();
    assert_eq!(codes(in_main), ["needless-return", "copies"], "{:?}", messages(in_main));
    let severity = |at: usize| in_main[at].get("severity").and_then(Json::as_usize);
    assert_eq!(
        (severity(0), severity(1)),
        (Some(2), Some(4)),
        "a lint warns; a copy that runs once is a hint"
    );
    assert!(messages(in_main)[1].contains("`items` is copied here"), "{:?}", messages(in_main));

    let params = Json::object([
        ("textDocument", Json::object([("uri", Json::string(editor.uri("src/main.fen")))])),
        (
            "range",
            Json::object([("start", position(text, "return", 0)), ("end", position(text, "return", 6))]),
        ),
        ("context", Json::object([("diagnostics", Json::Array(Vec::new()))])),
    ]);
    let actions = editor.request("textDocument/codeAction", params);
    let action = &actions.as_array().unwrap()[0];
    let edits =
        action.at(&["edit", "changes"]).and_then(|changes| changes.get(&editor.uri("src/main.fen"))).unwrap();
    assert_eq!(edits.as_array().unwrap()[0].get("newText"), Some(&Json::string("")));
}

#[test]
fn a_quick_edit_keeps_what_other_files_show_and_lints_the_edited_one() {
    let mut editor = Editor::new("quick_lints").unwrap();
    editor.open("src/combat.fen", &COMBAT.replace("    power * 2\n", "    return power * 2\n"));
    editor.open("src/main.fen", MAIN);
    let published = editor.change("src/main.fen", &MAIN.replace("hit(fighter, 3)", "hit(fighter, 4)"));
    assert_eq!(codes(for_file(&published, "combat.fen").unwrap()), ["needless-return"], "kept");
    assert_eq!(for_file(&published, "main.fen"), Some(&[][..]));
    let with_lint =
        MAIN.replace("    stdout.print(fighter", "    stdout.print(f\"go\")\n    stdout.print(fighter");
    let published = editor.change("src/main.fen", &with_lint);
    assert_eq!(codes(for_file(&published, "main.fen").unwrap()), ["plain-fstring"], "found in the edit");
    assert_eq!(codes(for_file(&published, "combat.fen").unwrap()), ["needless-return"], "still kept");
}

#[test]
fn definitions_of_functions_locals_members_and_constants() {
    let mut editor = Editor::new("definitions").unwrap();
    editor.open("src/main.fen", MAIN);
    let combat = editor.uri("src/combat.fen");

    let hit = editor.at("textDocument/definition", "src/main.fen", MAIN, "hit(fighter", 1);
    let hit = &hit.as_array().unwrap()[0];
    assert_eq!(hit.get("uri"), Some(&Json::string(&combat)));
    assert_eq!(hit.at(&["range", "start"]), Some(&position(COMBAT, "hit(", 0)));

    let local = editor.at("textDocument/definition", "src/main.fen", MAIN, "fighter.describe", 2);
    assert_eq!(local.as_array().unwrap()[0].at(&["range", "start"]), Some(&position(MAIN, "fighter =", 0)));

    let method = editor.at("textDocument/definition", "src/main.fen", MAIN, "describe()", 3);
    assert_eq!(line_of(&method.as_array().unwrap()[0]), 6);

    let constant = editor.at("textDocument/definition", "src/main.fen", MAIN, "SIZE)", 0);
    assert_eq!(line_of(&constant.as_array().unwrap()[0]), 3);

    let in_type = editor.at("textDocument/definition", "src/combat.fen", COMBAT, "Fighter,", 1);
    assert_eq!(line_of(&in_type.as_array().unwrap()[0]), 1);

    let nothing = editor.at("textDocument/definition", "src/main.fen", MAIN, "Kit", 0);
    assert_eq!(nothing, Json::Null);
}

#[test]
fn members_are_found_while_the_program_has_mistakes() {
    let mut editor = Editor::new("mistakes").unwrap();
    let broken = MAIN.replace("damage, SIZE", "damage + True, SIZE");
    editor.open("src/main.fen", &broken);
    let method = editor.at("textDocument/definition", "src/main.fen", &broken, "describe()", 3);
    assert_eq!(line_of(&method.as_array().unwrap()[0]), 6);
}

#[test]
fn hovers_show_signatures_docs_and_what_names_hold() {
    let mut editor = Editor::new("hovers").unwrap();
    editor.open("src/main.fen", MAIN);
    let shown = |hover: &Json| hover.at(&["contents", "value"]).and_then(Json::as_str).unwrap().to_owned();

    let hit = editor.at("textDocument/hover", "src/main.fen", MAIN, "hit(fighter", 0);
    assert_eq!(
        shown(&hit),
        "```fennec\ndef hit(\n    fighter: Fighter,\n    power: int,\n) -> int\n```\n\nHow much a hit does,\ndoubled."
    );
    let describe = editor.at("textDocument/hover", "src/main.fen", MAIN, "describe()", 0);
    assert_eq!(shown(&describe), "```fennec\ndef describe(self) -> str\n```\n\nA line about this fighter.");
    let fighter = editor.at("textDocument/hover", "src/main.fen", MAIN, "Fighter(name", 0);
    assert_eq!(shown(&fighter), "```fennec\nstruct Fighter\n```\n\nSomeone in the arena.");
    let damage = editor.at("textDocument/hover", "src/main.fen", MAIN, "damage, SIZE", 0);
    assert_eq!(shown(&damage), "```fennec\ndamage: int\n```");
    let field = editor.at("textDocument/hover", "src/combat.fen", COMBAT, "health = 100", 0);
    assert_eq!(shown(&field), "```fennec\nhealth = 100\n```");
    let size = editor.at("textDocument/hover", "src/main.fen", MAIN, "SIZE)", 0);
    assert_eq!(shown(&size), "```fennec\nSIZE = 10\n```\n\nThe arena's size.");
}

#[test]
fn formatting_rewrites_the_whole_document() {
    let mut editor = Editor::new("formatting").unwrap();
    let messy = "def main( stdout: Stdout ):\n  stdout.print( 1 )\n";
    editor.open("src/main.fen", messy);
    let params = Json::object([
        ("textDocument", Json::object([("uri", Json::string(editor.uri("src/main.fen")))])),
        ("options", Json::object([("tabSize", Json::number(4)), ("insertSpaces", Json::Bool(true))])),
    ]);
    let edits = editor.request("textDocument/formatting", params.clone());
    let edit = &edits.as_array().unwrap()[0];
    assert_eq!(edit.get("newText"), Some(&Json::string("def main(stdout: Stdout):\n    stdout.print(1)\n")));
    editor.change("src/main.fen", "def main(stdout: Stdout):\n    stdout.print(1)\n");
    assert_eq!(editor.request("textDocument/formatting", params), Json::Array(Vec::new()));
}

#[test]
fn a_file_outside_a_kit_is_a_program_of_its_own() {
    let mut editor = Editor::new("single").unwrap();
    let published = editor.open("loose.fen", "def main(stdout: Stdout):\n    stdout.print(nope)\n");
    assert_eq!(published.len(), 1);
    assert!(messages(for_file(&published, "loose.fen").unwrap())[0].contains("nope"));
    let closed = editor.notify(
        "textDocument/didClose",
        Json::object([("textDocument", Json::object([("uri", Json::string(editor.uri("loose.fen")))]))]),
    );
    assert_eq!(closed, vec![("loose.fen".to_owned(), Vec::new())]);
}

#[test]
fn requests_after_shutdown_are_refused_and_exit_ends_the_session() {
    let mut session = Session::new();
    let shutdown = Json::parse(r#"{"jsonrpc":"2.0","id":1,"method":"shutdown"}"#).unwrap();
    assert_eq!(session.handle(&shutdown)[0].get("result"), Some(&Json::Null));
    let hover = Json::parse(r#"{"jsonrpc":"2.0","id":2,"method":"textDocument/hover","params":{}}"#).unwrap();
    assert!(session.handle(&hover)[0].get("error").is_some());
    assert!(!session.exited());
    session.handle(&Json::parse(r#"{"jsonrpc":"2.0","method":"exit"}"#).unwrap());
    assert!(session.exited() && session.shut_down());
}

#[test]
fn names_bound_in_a_case_are_found_from_its_guard_and_body() {
    let mut editor = Editor::new("cases").unwrap();
    let text = "def size(value: int) -> str:\n    match value:\n        case 1 | 2:\n            \"small\"\n        \
        case count if count > 9:\n            f\"{count} is big\"\n        case _:\n            \"medium\"\n\n\
        def main(stdout: Stdout):\n    stdout.print(size(12))\n";
    let published = editor.open("src/main.fen", text);
    assert_eq!(for_file(&published, "main.fen"), Some(&[][..]));
    let bound = position(text, "count if", 0);
    for (needle, into) in [("count > 9", 0), ("count} is", 1)] {
        let found = editor.at("textDocument/definition", "src/main.fen", text, needle, into);
        assert_eq!(found.as_array().unwrap()[0].at(&["range", "start"]), Some(&bound), "{needle}");
    }
}
