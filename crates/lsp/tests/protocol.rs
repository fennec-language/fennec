//! JSON, message framing and file URIs.

use std::io::Cursor;
use std::path::Path;

use fennec_lsp::{Json, path_of, read, serve, uri_of, write};

#[test]
fn json_reads_and_writes_back_the_same() {
    let text = r#"{"a":[1,2.5,-3e2,true,false,null],"b":"line\nquote\" slash\\ tab\t é 🦊","c":{}}"#;
    let value = Json::parse(text).unwrap();
    assert_eq!(value.at(&["a"]).and_then(Json::as_array).map(<[Json]>::len), Some(6));
    assert_eq!(value.get("b").and_then(Json::as_str), Some("line\nquote\" slash\\ tab\t é 🦊"));
    assert_eq!(Json::parse(&value.to_string()), Some(value));
}

#[test]
fn json_escapes_read_as_their_characters() {
    let value = Json::parse(r#""\u00e9\ud83e\udd8a\/""#).unwrap();
    assert_eq!(value.as_str(), Some("é🦊/"));
}

#[test]
fn broken_json_reads_as_nothing() {
    for text in ["", "{", "[1,]", r#"{"a" 1}"#, "tru", r#""\ud83e""#, "1 2", r#""\x""#] {
        assert_eq!(Json::parse(text), None, "{text}");
    }
    let deep = "[".repeat(1000) + &"]".repeat(1000);
    assert_eq!(Json::parse(&deep), None);
}

#[test]
fn messages_are_framed_by_their_length() {
    let mut bytes = Vec::new();
    let message = Json::object([("id", Json::number(1)), ("text", Json::string("é"))]);
    write(&mut bytes, &message).unwrap();
    assert!(bytes.starts_with(b"Content-Length: 20\r\n\r\n"), "{}", String::from_utf8_lossy(&bytes));
    write(&mut bytes, &Json::Null).unwrap();
    let mut input = Cursor::new(bytes);
    assert_eq!(read(&mut input).unwrap(), Some(message));
    assert_eq!(read(&mut input).unwrap(), Some(Json::Null));
    assert_eq!(read(&mut input).unwrap(), None);
}

#[test]
fn a_session_served_over_bytes_shuts_down_cleanly() {
    let mut input = Vec::new();
    for message in [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"shutdown"}"#,
        r#"{"jsonrpc":"2.0","method":"exit"}"#,
    ] {
        write(&mut input, &Json::parse(message).unwrap()).unwrap();
    }
    let mut output = Vec::new();
    assert!(serve(Cursor::new(input), &mut output).unwrap());
    let mut answers = Cursor::new(output);
    let initialized = read(&mut answers).unwrap().unwrap();
    assert_eq!(initialized.at(&["result", "capabilities", "hoverProvider"]), Some(&Json::Bool(true)));
    assert_eq!(read(&mut answers).unwrap().unwrap().get("result"), Some(&Json::Null));
    assert_eq!(read(&mut answers).unwrap(), None);
}

/// Served, a session publishes a program's mistakes first, then again with what the linter finds
/// and what may copy, and `fennec/settled` is answered only once that's published.
#[test]
fn a_served_session_publishes_lints_after_mistakes_and_before_it_says_it_has_settled() {
    let dir =
        fennec_temp::Temp::at(std::env::temp_dir().join(format!("fennec-lsp-served-{}", std::process::id())));
    std::fs::create_dir_all(&dir).unwrap();
    let path = std::fs::canonicalize(&*dir).unwrap().join("main.fen");
    let text = "def main(stdout: Stdout):\n    pass\n    stdout.print(1)\n";
    std::fs::write(&path, text).unwrap();
    let document = Json::object([
        ("uri", Json::string(uri_of(&path))),
        ("languageId", Json::string("fennec")),
        ("version", Json::number(1)),
        ("text", Json::string(text)),
    ]);
    let mut input = Vec::new();
    for message in [
        Json::object([
            ("id", Json::number(1)),
            ("method", Json::string("initialize")),
            ("params", Json::object([])),
        ]),
        Json::object([
            ("method", Json::string("textDocument/didOpen")),
            ("params", Json::object([("textDocument", document)])),
        ]),
        Json::object([("id", Json::number(2)), ("method", Json::string("fennec/settled"))]),
        Json::object([("id", Json::number(3)), ("method", Json::string("shutdown"))]),
        Json::object([("method", Json::string("exit"))]),
    ] {
        write(&mut input, &message).unwrap();
    }
    let mut output = Vec::new();
    assert!(serve(Cursor::new(input), &mut output).unwrap());
    let mut answers = Cursor::new(output);
    let mut said = Vec::new();
    while let Some(answer) = read(&mut answers).unwrap() {
        let codes: Vec<&str> = answer
            .at(&["params", "diagnostics"])
            .and_then(Json::as_array)
            .unwrap_or(&[])
            .iter()
            .filter_map(|diagnostic| diagnostic.get("code")?.as_str())
            .collect();
        said.push(match answer.get("id").and_then(Json::as_usize) {
            Some(id) => format!("answer {id}"),
            None => format!("published {codes:?}"),
        });
    }
    assert_eq!(said, ["answer 1", "published []", "published [\"needless-pass\"]", "answer 2", "answer 3"]);
}

#[test]
fn file_uris_round_trip_through_paths() {
    let path = Path::new("/home/kit/my game/src/main.fen");
    let uri = uri_of(path);
    assert_eq!(uri, "file:///home/kit/my%20game/src/main.fen");
    assert_eq!(path_of(&uri).as_deref(), Some(path));
    assert_eq!(path_of("file:///C:/games/main.fen").as_deref(), Some(Path::new("C:/games/main.fen")));
    assert_eq!(path_of("untitled:Untitled-1"), None);
    let resolved = uri_of(Path::new(r"\\?\C:\games\main.fen"));
    assert_eq!(resolved, "file:///C:/games/main.fen", "a resolved Windows path");
}
