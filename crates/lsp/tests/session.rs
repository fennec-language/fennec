//! An editor's session with the server, message by message, on a small kit.

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
    power * 2 + fighter.health // 100
";

/// An editor talking to a session, with a kit on disk.
struct Editor {
    session: Session,
    dir: fennec_temp::Temp,
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
        let dir = fennec_temp::Temp::at(std::fs::canonicalize(dir)?);
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
fn a_format_that_doesnt_fit_is_found_before_running() {
    let mut editor = Editor::new("format_spec").unwrap();
    let published = editor.open("src/main.fen", &MAIN.replace("damage, SIZE", "f\"{damage:02x}\", SIZE"));
    let in_main = for_file(&published, "main.fen").unwrap();
    assert_eq!(in_main.len(), 1, "{:?}", messages(in_main));
    assert!(
        messages(in_main)[0].starts_with("Fennec has no format `02x`\nhelp: "),
        "{:?}",
        messages(in_main)
    );
    assert_eq!(line_of(&in_main[0]), 8);
}

#[test]
fn a_duration_with_an_unknown_unit_is_found() {
    let mut editor = Editor::new("duration_unit").unwrap();
    let published = editor.open("src/main.fen", &MAIN.replace("damage, SIZE", "5m, SIZE"));
    let in_main = for_file(&published, "main.fen").unwrap();
    assert_eq!(in_main.len(), 1, "{:?}", messages(in_main));
    let message = &messages(in_main)[0];
    assert!(message.starts_with("`5m` uses `m`, which isn't a unit of time"), "{message}");
    assert!(message.contains("minutes are `min`"), "{message}");
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

#[test]
fn a_fix_to_make_by_hand_is_offered_but_not_preferred() {
    let mut editor = Editor::new("by-hand").unwrap();
    let text = "def main(stdout: Stdout):\n    names = [1]\n    names = [2]\n    stdout.print(names)\n";
    let published = editor.open("src/main.fen", text);
    let shown = messages(for_file(&published, "main.fen").unwrap());
    assert!(shown.iter().any(|message| message.contains("fix: declare it `mut names`")), "{shown:?}");
    let params = Json::object([
        ("textDocument", Json::object([("uri", Json::string(editor.uri("src/main.fen")))])),
        (
            "range",
            Json::object([
                ("start", position(text, "names = [2]", 0)),
                ("end", position(text, "names = [2]", 5)),
            ]),
        ),
        ("context", Json::object([("diagnostics", Json::Array(Vec::new()))])),
    ]);
    let actions = editor.request("textDocument/codeAction", params);
    let [action] = actions.as_array().unwrap() else { panic!("{actions:?}") };
    assert_eq!(action.get("title"), Some(&Json::string("Fix by hand: declare it `mut names`")));
    assert_eq!(action.get("isPreferred"), Some(&Json::Bool(false)));
    let edits =
        action.at(&["edit", "changes"]).and_then(|changes| changes.get(&editor.uri("src/main.fen"))).unwrap();
    let edit = &edits.as_array().unwrap()[0];
    assert_eq!(edit.get("newText"), Some(&Json::string("mut ")));
    assert_eq!(edit.at(&["range", "start"]), Some(&position(text, "names", 0)));
}

/// What a published diagnostic's `code` is: a lint's rule.
fn codes(diagnostics: &[Json]) -> Vec<&str> {
    diagnostics.iter().filter_map(|diagnostic| diagnostic.get("code")?.as_str()).collect()
}

#[test]
fn lints_and_copies_show_with_their_rules_and_fixes() {
    let mut editor = Editor::new("lints").unwrap();
    let text = "def one() -> int:\n    pass\n    return 1\n\ndef main(stdout: Stdout):\n    mut items = [1]\n    \
                held = items\n    items.append(2)\n    stdout.print(held.len(), items.len(), one())\n";
    let published = editor.open("src/main.fen", text);
    let in_main = for_file(&published, "main.fen").unwrap();
    assert_eq!(codes(in_main), ["needless-pass", "copies"], "{:?}", messages(in_main));
    let severity = |at: usize| in_main[at].get("severity").and_then(Json::as_usize);
    assert_eq!(
        (severity(0), severity(1)),
        (Some(2), Some(4)),
        "a lint warns; a copy that runs once is a hint"
    );
    assert!(messages(in_main)[1].contains("`items` is copied here"), "{:?}", messages(in_main));

    let params = Json::object([
        ("textDocument", Json::object([("uri", Json::string(editor.uri("src/main.fen")))])),
        ("range", Json::object([("start", position(text, "pass", 0)), ("end", position(text, "pass", 4))])),
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
    editor.open("src/combat.fen", &COMBAT.replace("    power * 2", "    pass\n    power * 2"));
    editor.open("src/main.fen", MAIN);
    let published = editor.change("src/main.fen", &MAIN.replace("hit(fighter, 3)", "hit(fighter, 4)"));
    assert_eq!(codes(for_file(&published, "combat.fen").unwrap()), ["needless-pass"], "kept");
    assert_eq!(for_file(&published, "main.fen"), Some(&[][..]));
    let with_lint =
        MAIN.replace("    stdout.print(fighter", "    stdout.print(f\"go\")\n    stdout.print(fighter");
    let published = editor.change("src/main.fen", &with_lint);
    assert_eq!(codes(for_file(&published, "main.fen").unwrap()), ["plain-fstring"], "found in the edit");
    assert_eq!(codes(for_file(&published, "combat.fen").unwrap()), ["needless-pass"], "still kept");
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
fn a_parameter_typed_by_an_interface_shows_the_interface() {
    let text = "interface Shape:\n    def area(self) -> float\n\nstruct Square:\n    def area(self) -> float:\n        1.0\n\n\
                def total(shape: Shape) -> float:\n    held = shape\n    held.area()\n\n\
                def main(stdout: Stdout):\n    stdout.print(total(Square()))\n";
    let mut editor = Editor::new("interface_params").unwrap();
    editor.open("src/main.fen", text);
    let shown = |hover: &Json| hover.at(&["contents", "value"]).and_then(Json::as_str).unwrap().to_owned();
    let total = editor.at("textDocument/hover", "src/main.fen", text, "total(Square", 0);
    assert_eq!(shown(&total), "```fennec\ndef total(shape: Shape) -> float\n```");
    let held = editor.at("textDocument/hover", "src/main.fen", text, "held.area", 0);
    assert_eq!(shown(&held), "```fennec\nheld: Shape\n```");
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

/// Inside `except`, what it names and what its `case` arms bind are found, shown and offered as
/// a `match`'s are, and a program written with every form has nothing to say about it.
#[test]
fn names_under_except_are_found_shown_and_offered() {
    let mut editor = Editor::new("excepts").unwrap();
    let text = "error Bad:\n    Empty\n    Long(size: int)\n\nerror Big:\n    why: str\n\n\
        def risky(n: int) -> int fails Big:\n    if n > 5:\n        fail Big(\"big\")\n    return n\n\n\
        def act(n: int) fails Big:\n    if n == 2:\n        fail Big(\"two\")\n\n\
        def load(n: int) -> int fails Bad:\n    if n == 0:\n        fail .Empty\n    if n > 5:\n        fail .Long(size=n)\n    return n\n\n\
        def main(stdout: Stdout):\n    small = load(9) except why:\n        stdout.print(f\"{why}\")\n        0\n    \
        sized = load(9) except why:\n        case .Long(size):\n            size\n        case .Empty:\n            0\n    \
        port = risky(7) except Big(): 8080\n    \
        act(2) except failed:\n        _ = failed\n    \
        stdout.print(small + sized + port)\n    return\n";
    let published = editor.open("src/main.fen", text);
    assert_eq!(
        for_file(&published, "main.fen"),
        Some(&[][..]),
        "{:?}",
        messages(for_file(&published, "main.fen").unwrap())
    );
    let bound = position(text, "size):", 0);
    let found = editor.at("textDocument/definition", "src/main.fen", text, "size\n        case .Empty", 0);
    assert_eq!(found.as_array().unwrap()[0].at(&["range", "start"]), Some(&bound));
    let named = position(text, "why:\n        stdout", 0);
    let found = editor.at("textDocument/definition", "src/main.fen", text, "why}", 0);
    assert_eq!(found.as_array().unwrap()[0].at(&["range", "start"]), Some(&named));
    let shown =
        |hover: &Json| hover.at(&["contents", "value"]).and_then(Json::as_str).unwrap_or("").to_owned();
    let hover = editor.at("textDocument/hover", "src/main.fen", text, "failed\n    stdout", 0);
    assert_eq!(shown(&hover), "```fennec\nfailed: Big\n```");
    let typing = text.replace("        _ = failed\n", "        _ = fai\n");
    editor.change("src/main.fen", &typing);
    let offered = labels(&editor.at("textDocument/completion", "src/main.fen", &typing, "fai\n", 3));
    assert_eq!(offered.first().map(String::as_str), Some("failed"), "{offered:?}");
    let keyword = labels(&editor.at("textDocument/completion", "src/main.fen", &typing, "fai\n", 0));
    assert!(keyword.iter().any(|label| label == "except"), "{keyword:?}");
}

/// Names that aren't plain uses: a named argument, a case in a pattern, a test's capability, a
/// type parameter, a comprehension's name where it's bound, and a module named in front of what
/// it holds or in its import.
#[test]
fn definitions_of_arguments_cases_capabilities_parameters_and_modules() {
    let mut editor = Editor::new("named").unwrap();
    let text = "import combat\n\nenum Shape:\n    Dot\n    Line(length: int)\n\n\
        struct Point:\n    x: int\n\n    def moved(self, by: int) -> Point:\n        Point(x=self.x + by)\n\n\
        def scaled(value: int, times: int) -> int:\n    value * times\n\n\
        def first[T](items: list[T]) -> T:\n    items[0]\n\n\
        def size_of(shape: Shape) -> int:\n    match shape:\n        case .Line(length): length\n        case .Dot: 0\n\n\
        def main(stdout: Stdout):\n    point = Point(x=1)\n    \
        stdout.print(scaled(2, times=3), point.moved(by=2).x, size_of(.Line(length=4)), first([n for n in [1]]))\n    \
        stdout.print(combat.hit(combat.Fighter(name=\"Kit\"), 3))\n\n\
        test \"prints\"(screen: Stdout):\n    screen.print(1)\n";
    let published = editor.open("src/main.fen", text);
    let errors = for_file(&published, "main.fen").unwrap_or_default().iter();
    let errors: Vec<&Json> = errors.filter(|found| found.get("severity") == Some(&Json::number(1))).collect();
    assert!(errors.is_empty(), "{errors:?}");
    let cases = [
        ("x=1", "x: int"),
        ("times=3", "times: int"),
        ("by=2", "by: int"),
        ("length=4", "length: int"),
        ("Line(length):", "Line(length: int)"),
        ("T:\n    items", "T](items"),
        ("n for n", "n in [1]"),
        ("screen.print", "screen: Stdout"),
    ];
    for (needle, declared) in cases {
        let found = editor.at("textDocument/definition", "src/main.fen", text, needle, 0);
        let found = found.as_array().map(|found| &found[0]);
        assert_eq!(
            found.and_then(|found| found.at(&["range", "start"])),
            Some(&position(text, declared, 0)),
            "{needle}"
        );
    }
    let combat = Json::string(editor.uri("src/combat.fen"));
    for needle in ["combat.hit", "combat.Fighter", "combat\n\nenum"] {
        let found = editor.at("textDocument/definition", "src/main.fen", text, needle, 0);
        let found = found.as_array().map(|found| &found[0]);
        assert_eq!(found.and_then(|found| found.get("uri")), Some(&combat), "{needle}");
        assert_eq!(found.map(line_of), Some(0), "{needle}");
    }
}

/// The standard library's source is built into `fennec`, so going to what it declares goes to a
/// copy of its file, which shows in a hover too, and renaming it is refused.
#[test]
fn the_standard_librarys_declarations_are_found_in_a_copy_of_its_files() {
    let mut editor = Editor::new("library").unwrap();
    let text = "from std.math import Vec3\n\ndef main(stdout: Stdout):\n    at = Vec3(x=1.0)\n    stdout.print(at.x, at.length())\n";
    editor.open("src/main.fen", text);
    for (needle, declared) in
        [("Vec3(x", "export struct Vec3:"), ("x=1.0", "    x = 0.0"), ("length()", "    def length(self)")]
    {
        let found = editor.at("textDocument/definition", "src/main.fen", text, needle, 0);
        let found = &found.as_array().unwrap()[0];
        let uri = found.get("uri").and_then(Json::as_str).unwrap();
        // The folder is named by the version and a hash of the library, so another build's never
        // writes over it.
        assert!(uri.contains(&format!("/std/{}-", env!("CARGO_PKG_VERSION"))), "{needle}: {uri}");
        assert!(uri.ends_with("/math.fen"), "{needle}: {uri}");
        let copy = std::fs::read_to_string(fennec_lsp::path_of(uri).unwrap()).unwrap();
        let line = copy.lines().nth(line_of(found)).unwrap();
        assert!(line.starts_with(declared), "{needle}: {line}");
    }
    let hover = editor.at("textDocument/hover", "src/main.fen", text, "Vec3(x", 0);
    let shown = hover.at(&["contents", "value"]).and_then(Json::as_str).unwrap_or("");
    assert!(shown.contains("struct Vec3"), "{shown}");
    let params = params_at(&editor, "src/main.fen", text, "Vec3(x", vec![("newName", Json::string("Point"))]);
    assert_eq!(editor.request("textDocument/rename", params), Json::string("no result"));
}

/// A request about the place in `file` where `needle` starts, plus `into` characters, with more
/// parameters.
fn params_at(editor: &Editor, file: &str, text: &str, needle: &str, more: Vec<(&str, Json)>) -> Json {
    let mut fields = vec![
        ("textDocument".to_owned(), Json::object([("uri", Json::string(editor.uri(file)))])),
        ("position".to_owned(), position(text, needle, 0)),
    ];
    fields.extend(more.into_iter().map(|(key, value)| (key.to_owned(), value)));
    Json::Object(fields)
}

/// Each location's file name and line.
fn places(locations: &Json) -> Vec<(String, usize)> {
    let all = locations.as_array().unwrap_or(&[]);
    let place = |location: &Json| {
        let uri = location.get("uri").and_then(Json::as_str).unwrap_or("");
        (uri.rsplit('/').next().unwrap_or(uri).to_owned(), line_of(location))
    };
    let mut found: Vec<(String, usize)> = all.iter().map(place).collect();
    found.sort();
    found
}

/// An edit to a function's body is checked by itself, and what's asked about a place after it
/// is answered from that quick check: a definition in another file, a local's type, and, from
/// the whole program, every reference.
#[test]
fn after_an_edit_to_a_body_definitions_hovers_and_references_still_answer() {
    let mut editor = Editor::new("answers_after_a_body_edit").unwrap();
    editor.open("src/main.fen", MAIN);
    let edited = MAIN.replace("hit(fighter, 3)", "hit(fighter, 4)");
    let published = editor.change("src/main.fen", &edited);
    assert_eq!(for_file(&published, "main.fen"), Some(&[][..]));
    let hit = editor.at("textDocument/definition", "src/main.fen", &edited, "hit(fighter", 1);
    let hit = &hit.as_array().unwrap()[0];
    assert_eq!(hit.get("uri"), Some(&Json::string(editor.uri("src/combat.fen"))));
    assert_eq!(hit.at(&["range", "start"]), Some(&position(COMBAT, "hit(", 0)));
    let hover = editor.at("textDocument/hover", "src/main.fen", &edited, "damage =", 0);
    let shown = hover.at(&["contents", "value"]).and_then(Json::as_str).unwrap_or("");
    assert!(shown.contains("damage: int"), "{shown}");
    let with = Json::object([("includeDeclaration", Json::Bool(true))]);
    let params = params_at(&editor, "src/main.fen", &edited, "Fighter(", vec![("context", with)]);
    assert_eq!(places(&editor.request("textDocument/references", params)).len(), 4);
}

#[test]
fn references_reach_every_file_and_can_leave_out_the_declaration() {
    let mut editor = Editor::new("references").unwrap();
    editor.open("src/main.fen", MAIN);
    let with = |include: bool| Json::object([("includeDeclaration", Json::Bool(include))]);
    let params = params_at(&editor, "src/main.fen", MAIN, "Fighter(", vec![("context", with(true))]);
    let found = editor.request("textDocument/references", params);
    assert_eq!(
        places(&found),
        [
            ("combat.fen".to_owned(), 1),
            ("combat.fen".to_owned(), 12),
            ("main.fen".to_owned(), 0),
            ("main.fen".to_owned(), 6)
        ]
    );
    let params = params_at(&editor, "src/main.fen", MAIN, "Fighter, hit", vec![("context", with(true))]);
    assert_eq!(places(&editor.request("textDocument/references", params)).len(), 4, "from the import");
    let params = params_at(&editor, "src/main.fen", MAIN, "Fighter(", vec![("context", with(false))]);
    let found = editor.request("textDocument/references", params);
    assert_eq!(places(&found).len(), 3, "without where it's declared");
    let params = params_at(&editor, "src/main.fen", MAIN, "damage =", vec![("context", with(true))]);
    let found = editor.request("textDocument/references", params);
    assert_eq!(places(&found), [("main.fen".to_owned(), 7), ("main.fen".to_owned(), 8)], "a local");
}

#[test]
fn a_rename_changes_every_use_and_refuses_what_it_cant_do_safely() {
    let mut editor = Editor::new("rename").unwrap();
    editor.open("src/main.fen", MAIN);
    let rename = |editor: &mut Editor, needle: &str, name: &str| {
        let params = params_at(editor, "src/main.fen", MAIN, needle, vec![("newName", Json::string(name))]);
        editor.request("textDocument/rename", params)
    };
    let edit = rename(&mut editor, "hit(", "strike");
    let changes = edit.get("changes").unwrap();
    let edits = |file: &str| changes.get(&editor.uri(file)).and_then(Json::as_array).map_or(0, <[Json]>::len);
    assert_eq!((edits("src/main.fen"), edits("src/combat.fen")), (2, 1));
    assert_eq!(rename(&mut editor, "hit(", "for"), Json::string("no result"), "a keyword");
    assert_eq!(rename(&mut editor, "print", "say"), Json::string("no result"), "the standard library's");
    editor.change("src/main.fen", &MAIN.replace("damage, SIZE", "damage + True, SIZE"));
    let params = params_at(&editor, "src/main.fen", MAIN, "hit(", vec![("newName", Json::string("strike"))]);
    assert_eq!(editor.request("textDocument/rename", params), Json::string("no result"), "with mistakes");
}

#[test]
fn a_documents_outline_has_its_types_members_and_functions() {
    let mut editor = Editor::new("outline").unwrap();
    editor.open("src/combat.fen", COMBAT);
    let params =
        Json::object([("textDocument", Json::object([("uri", Json::string(editor.uri("src/combat.fen")))]))]);
    let outline = editor.request("textDocument/documentSymbol", params);
    let names = |symbols: &Json| -> Vec<String> {
        let all = symbols.as_array().unwrap_or(&[]);
        all.iter().filter_map(|symbol| symbol.get("name")?.as_str().map(str::to_owned)).collect()
    };
    assert_eq!(names(&outline), ["Fighter", "hit"]);
    let fighter = &outline.as_array().unwrap()[0];
    assert_eq!(names(fighter.get("children").unwrap()), ["name", "health", "describe"]);
}

/// The labels a completion offers, in the order it shows them.
fn labels(completion: &Json) -> Vec<String> {
    let items = completion.get("items").and_then(Json::as_array).unwrap_or(&[]);
    items.iter().filter_map(|item| item.get("label")?.as_str().map(str::to_owned)).collect()
}

#[test]
fn completion_after_a_dot_offers_what_the_objects_type_has() {
    let mut editor = Editor::new("complete-members").unwrap();
    let typing = MAIN.replace("    damage = hit", "    shown = fighter.\n    damage = hit");
    editor.open("src/main.fen", &typing);
    let offered = labels(&editor.at("textDocument/completion", "src/main.fen", &typing, "fighter.\n", 8));
    assert_eq!(offered, ["name", "health", "describe"]);
    let typing = MAIN.replace("    damage = hit", "    shown = fighter.name.st\n    damage = hit");
    editor.change("src/main.fen", &typing);
    let offered = labels(&editor.at("textDocument/completion", "src/main.fen", &typing, "name.st", 7));
    for method in ["startswith", "upper", "split", "len"] {
        assert!(offered.contains(&method.to_owned()), "text has {method}: {offered:?}");
    }
    assert!(!offered.contains(&"health".to_owned()), "{offered:?}");
}

#[test]
fn completion_of_a_name_offers_whats_in_scope_first() {
    let mut editor = Editor::new("complete-names").unwrap();
    let typing = MAIN.replace("    stdout.print", "    total = da\n    stdout.print");
    editor.open("src/main.fen", &typing);
    let completion = editor.at("textDocument/completion", "src/main.fen", &typing, "= da\n", 4);
    let offered = labels(&completion);
    assert_eq!(offered.get(..3), Some(&["damage".to_owned(), "fighter".to_owned(), "stdout".to_owned()][..]));
    for name in ["SIZE", "main", "Fighter", "hit", "range", "for"] {
        assert!(offered.contains(&name.to_owned()), "{name}: {offered:?}");
    }
    assert!(!offered.contains(&"power".to_owned()), "another function's parameter: {offered:?}");
    let items = completion.get("items").and_then(Json::as_array).unwrap();
    let size = items.iter().find(|item| item.get("label") == Some(&Json::string("SIZE"))).unwrap();
    assert_eq!(size.at(&["documentation", "value"]), Some(&Json::string("The arena's size.")));
}

#[test]
fn completion_after_a_bare_dot_offers_enum_cases_and_a_type_offers_its_own() {
    let mut editor = Editor::new("complete-cases").unwrap();
    let typing = format!("{MAIN}\nenum Mood:\n    Calm\n    Angry\n\ndef mood() -> Mood:\n    .\n");
    editor.open("src/main.fen", &typing);
    let offered = labels(&editor.at("textDocument/completion", "src/main.fen", &typing, "    .\n", 5));
    assert_eq!(offered.get(..2), Some(&["Calm".to_owned(), "Angry".to_owned()][..]), "{offered:?}");
    assert!(!offered.contains(&"damage".to_owned()), "{offered:?}");
    let typing = typing.replace("    .\n", "    Mood.\n");
    editor.change("src/main.fen", &typing);
    let offered = labels(&editor.at("textDocument/completion", "src/main.fen", &typing, "Mood.\n", 5));
    assert_eq!(offered, ["Calm", "Angry"]);
}

/// The labels of the signatures offered, and which parameter is marked.
fn signatures(help: &Json) -> (Vec<String>, Option<f64>) {
    let all = help.get("signatures").and_then(Json::as_array).unwrap_or(&[]);
    let labels = all.iter().filter_map(|shape| shape.get("label")?.as_str().map(str::to_owned)).collect();
    let active = match help.get("activeParameter") {
        Some(Json::Number(at)) => Some(*at),
        _ => None,
    };
    (labels, active)
}

#[test]
fn signature_help_shows_what_a_call_takes_and_marks_the_argument() {
    let mut editor = Editor::new("signature-help").unwrap();
    let typing = MAIN.replace("hit(fighter, 3)", "hit(fighter, ");
    editor.open("src/main.fen", &typing);
    let help = editor.at("textDocument/signatureHelp", "src/main.fen", &typing, "hit(fighter, ", 13);
    assert_eq!(signatures(&help), (vec!["hit(fighter: Fighter, power: int) -> int".to_owned()], Some(1.0)));
    let doc = help
        .get("signatures")
        .and_then(Json::as_array)
        .and_then(|all| all[0].at(&["documentation", "value"]));
    assert_eq!(doc, Some(&Json::string("How much a hit does,\ndoubled.")));
    let typing = MAIN.replace("hit(fighter, 3)", "hit(power=2, ");
    editor.change("src/main.fen", &typing);
    let help = editor.at("textDocument/signatureHelp", "src/main.fen", &typing, "power=2", 6);
    assert_eq!(signatures(&help).1, Some(1.0), "a keyword argument marks the parameter it names");
    let typing = MAIN.replace("Fighter(name=\"Kit\")", "Fighter(name=\"Kit\", )");
    editor.change("src/main.fen", &typing);
    let help = editor.at("textDocument/signatureHelp", "src/main.fen", &typing, "\", )", 3);
    assert_eq!(signatures(&help), (vec!["Fighter(name: str, health = 100)".to_owned()], Some(1.0)));
    assert_eq!(editor.at("textDocument/signatureHelp", "src/main.fen", &typing, "damage =", 0), Json::Null);
}

#[test]
fn signature_help_for_a_method_comes_from_the_objects_type() {
    let mut editor = Editor::new("signature-methods").unwrap();
    let typing = MAIN.replace("fighter.describe()", "fighter.name.startswith(");
    editor.open("src/main.fen", &typing);
    let help = editor.at("textDocument/signatureHelp", "src/main.fen", &typing, "startswith(", 11);
    assert_eq!(signatures(&help), (vec!["startswith(prefix: str) -> bool".to_owned()], Some(0.0)));
    let typing = MAIN.replace("fighter.describe()", "fighter.describe(");
    editor.change("src/main.fen", &typing);
    let help = editor.at("textDocument/signatureHelp", "src/main.fen", &typing, "describe(", 9);
    assert_eq!(signatures(&help).0, ["describe() -> str"]);
}

/// Each inlay hint's label, with the line and column it's at.
fn inlays(editor: &mut Editor, file: &str) -> Vec<(String, usize, usize)> {
    let params = Json::object([("textDocument", Json::object([("uri", Json::string(editor.uri(file)))]))]);
    let hints = editor.request("textDocument/inlayHint", params);
    let place =
        |hint: &Json, key: &str| hint.at(&["position", key]).and_then(Json::as_usize).unwrap_or(usize::MAX);
    let all = hints.as_array().unwrap_or(&[]);
    all.iter()
        .filter_map(|hint| {
            let label = hint.get("label")?.as_str()?.to_owned();
            Some((label, place(hint, "line"), place(hint, "character")))
        })
        .collect()
}

#[test]
fn inlay_hints_show_inferred_types_and_parameter_names() {
    let mut editor = Editor::new("inlay").unwrap();
    editor.open("src/main.fen", MAIN);
    let hints = inlays(&mut editor, "src/main.fen");
    assert!(hints.contains(&(": Fighter".to_owned(), 6, 11)), "{hints:?}");
    assert!(hints.contains(&(": int".to_owned(), 7, 10)), "{hints:?}");
    assert!(hints.contains(&("power:".to_owned(), 7, 26)), "{hints:?}");
    assert!(!hints.iter().any(|(label, ..)| label == "fighter:"), "an argument that says it: {hints:?}");
    assert!(
        !hints.iter().any(|(label, line, _)| label.starts_with(": ") && *line == 5),
        "given types: {hints:?}"
    );
}

#[test]
fn copies_in_loops_are_marked_and_none_is_a_mistake() {
    let mut editor = Editor::new("inlay-copies").unwrap();
    let once = format!(
        "{MAIN}\ndef once(n: int) -> int:\n    mut xs = [n]\n    snapshot = xs\n    for at in range(n):\n        \
         xs[0] = at\n    snapshot.len() + xs.len()\n"
    );
    let published = editor.open("src/main.fen", &once);
    assert!(
        !for_file(&published, "main.fen")
            .map(messages)
            .unwrap_or_default()
            .iter()
            .any(|message| message.contains("each time")),
        "{published:?}"
    );
    let hints = inlays(&mut editor, "src/main.fen");
    assert!(hints.iter().any(|(label, line, _)| label == "copies once" && *line == 14), "{hints:?}");
    let each = format!(
        "{MAIN}\ndef each(n: int) -> int:\n    mut xs = [n]\n    mut total = 0\n    for at in range(n):\n        \
         snapshot = xs\n        xs[0] = at\n        total += snapshot.len()\n    total\n"
    );
    let published = editor.change("src/main.fen", &each);
    let found = for_file(&published, "main.fen").unwrap();
    let mistakes: Vec<&Json> =
        found.iter().filter(|diagnostic| diagnostic.get("severity") == Some(&Json::number(1))).collect();
    // A copy each time round is a hint now: every copy is a snapshot, as the program says.
    assert!(mistakes.is_empty(), "{:?}", messages(found));
    let hints = inlays(&mut editor, "src/main.fen");
    assert!(hints.iter().any(|(label, ..)| label == "copies each pass"), "{hints:?}");
}
