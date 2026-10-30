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
    assert!(serve(&mut Cursor::new(input), &mut output).unwrap());
    let mut answers = Cursor::new(output);
    let initialized = read(&mut answers).unwrap().unwrap();
    assert_eq!(initialized.at(&["result", "capabilities", "hoverProvider"]), Some(&Json::Bool(true)));
    assert_eq!(read(&mut answers).unwrap().unwrap().get("result"), Some(&Json::Null));
    assert_eq!(read(&mut answers).unwrap(), None);
}

#[test]
fn file_uris_round_trip_through_paths() {
    let path = Path::new("/home/kit/my game/src/main.fen");
    let uri = uri_of(path);
    assert_eq!(uri, "file:///home/kit/my%20game/src/main.fen");
    assert_eq!(path_of(&uri).as_deref(), Some(path));
    assert_eq!(path_of("file:///C:/games/main.fen").as_deref(), Some(Path::new("C:/games/main.fen")));
    assert_eq!(path_of("untitled:Untitled-1"), None);
}
