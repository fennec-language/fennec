//! What native code and the runtime pass each other has to mean the same on every platform. Words
//! and floats do. A struct doesn't: Windows returns one over 8 bytes through a hidden first
//! argument where Linux and macOS use two registers up to 16, so a declaration that's right on one
//! gives wrong answers on another, with no crash. So the runtime's exports stick to words and
//! floats, and pass anything bigger out through a pointer.

use std::path::Path;

/// What an export may take or give back.
const WORDS: [&str; 5] = ["i64", "u64", "i8", "f64", "i32"];

/// `text::Text`, at 24 bytes, goes through a hidden argument on every platform, and native code
/// declares it that way.
const TOO_BIG_FOR_REGISTERS: &str = "text::Text";

/// An export's name, its arguments' types and what it gives back.
type Export = (String, Vec<String>, Option<String>);

/// Each export in the `export!` list.
fn exports(source: &str) -> Vec<Export> {
    let list = source.split("export! {").nth(1).unwrap_or_default();
    let list = list.split("\n}").next().unwrap_or_default();
    let mut found = Vec::new();
    for line in list.lines().map(str::trim).filter(|line| line.starts_with("fennec_")) {
        let Some((signature, _)) = line.split_once("=>") else { continue };
        let Some((name, rest)) = signature.split_once('(') else { continue };
        let Some(close) = closing_bracket(rest) else { continue };
        let (args, after) = rest.split_at(close);
        let returns = after.trim_start_matches(')').trim().strip_prefix("->").map(|ty| ty.trim().to_owned());
        found.push((name.to_owned(), arguments(args), returns));
    }
    found
}

/// Where the bracket that closes one already open is.
fn closing_bracket(text: &str) -> Option<usize> {
    let mut depth = 1;
    for (at, character) in text.char_indices() {
        depth += i32::from(character == '(') - i32::from(character == ')');
        if depth == 0 {
            return Some(at);
        }
    }
    None
}

/// The types in an export's arguments, like `a0: i64, a1: extern "C" fn(i64, i64) -> i64`.
fn arguments(args: &str) -> Vec<String> {
    let mut found = Vec::new();
    let (mut depth, mut start) = (0, 0);
    for (at, character) in args.char_indices() {
        depth += i32::from(character == '(') - i32::from(character == ')');
        if character == ',' && depth == 0 {
            found.push(args.get(start..at).unwrap_or_default());
            start = at + 1;
        }
    }
    found.push(args.get(start..).unwrap_or_default());
    found.iter().filter_map(|arg| arg.split_once(':')).map(|(_, ty)| ty.trim().to_owned()).collect()
}

/// Whether a type passes in one register on every platform. A reference or a function is a word,
/// and `parallel`'s and `serve`'s bodies are functions by another name.
fn fits_in_a_register(ty: &str) -> bool {
    WORDS.contains(&ty)
        || ty.starts_with('&')
        || ty.starts_with("extern \"C\" fn")
        || ty.starts_with("parallel::")
        || ty.starts_with("serve::")
}

#[test]
fn the_runtime_takes_and_gives_back_only_what_fits_in_a_register() {
    let found = exports(include_str!("../src/lib.rs"));
    assert_eq!(found.len(), fennec_runtime::SYMBOLS.len(), "every export has to be read to be checked");
    for (name, args, returns) in found {
        for ty in args {
            assert!(fits_in_a_register(&ty), "`{name}` takes a `{ty}`, which Windows passes differently");
        }
        if let Some(ty) = returns {
            assert!(
                fits_in_a_register(&ty) || ty == TOO_BIG_FOR_REGISTERS,
                "`{name}` gives back a `{ty}`, which Windows returns differently: pass it out through a `&mut i64`"
            );
        }
    }
}

/// A struct as a type the checks above would catch, so they can't quietly stop catching one.
#[test]
fn a_struct_given_back_is_seen() {
    let source = "export! {\n    fennec_parse_int(a0: i64) -> numbers::Parsed => numbers::parse_int(a0);\n}";
    let found = exports(source);
    assert_eq!(found.len(), 1);
    assert_eq!(found.first().and_then(|(_, _, returns)| returns.as_deref()), Some("numbers::Parsed"));
    assert!(!fits_in_a_register("numbers::Parsed"));
}

#[test]
fn native_code_declares_no_runtime_function_with_a_struct() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../llvm/src");
    let mut checked = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        checked += 1;
        for line in std::fs::read_to_string(&path).unwrap().lines() {
            // LLVM's own functions aren't compiled by Rust, so they mean the same everywhere.
            let declared_struct =
                line.contains("declare {{") && !line.contains("intrinsic") && !line.contains("@llvm.");
            assert!(!declared_struct, "{} declares a function giving back a struct: {line}", path.display());
        }
    }
    assert!(checked > 5, "only {checked} files read: LLVM's sources have moved");
}
