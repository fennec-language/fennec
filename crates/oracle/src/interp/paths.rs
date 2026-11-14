//! `std.path`'s `Path`, a struct holding a path's text, whose methods only look at that text,
//! each written here on its own from what `std.path` says it does.

use std::fmt::Write as _;

use fennec_syntax::Span;

use super::builtins::invalid;
use super::files::path_value;
use super::{Run, unsupported};
use crate::value::Value;

/// A method of a `Path` whose text is `text`.
pub(crate) fn method(text: &str, name: &str, args: &[Value], span: Span) -> Run<Value> {
    Ok(match (name, args) {
        ("join" | "/", [Value::Str(part)]) => path_value(joined(text, part)),
        ("inside", [Value::Str(part)]) => path_value(inside(text, part, span)?),
        ("is_absolute", []) => Value::Bool(text.starts_with('/')),
        ("parts", []) => Value::List(parts(text).into_iter().map(Value::Str).collect()),
        ("name", []) => Value::Str(name_of(text)),
        ("extension", []) => Value::Str(extension(text)),
        ("stem", []) => Value::Str(stem(text)),
        ("parent", []) => path_value(parent(text)),
        ("normalize", []) => path_value(normalized(text)),
        ("rebuild", [Value::List(parts)]) => {
            let mut texts = Vec::with_capacity(parts.len());
            for part in parts {
                match part {
                    Value::Str(part) => texts.push(part.clone()),
                    other => return unsupported(format!("`rebuild` of a list of {}", other.kind())),
                }
            }
            path_value(rebuilt(text, &texts))
        }
        _ => return unsupported(format!("the Path method `{name}`")),
    })
}

/// The path with `part` added: a `part` that starts with `/` replaces it.
fn joined(text: &str, part: &str) -> String {
    if part.starts_with('/') || text.is_empty() {
        part.to_owned()
    } else if text.ends_with('/') {
        format!("{text}{part}")
    } else {
        format!("{text}/{part}")
    }
}

/// The names between the `/`s, leaving out empty ones and `.`.
fn parts(text: &str) -> Vec<String> {
    text.split('/').filter(|part| !matches!(*part, "" | ".")).map(str::to_owned).collect()
}

fn name_of(text: &str) -> String {
    parts(text).pop().unwrap_or_default()
}

/// What follows the name's last `.`: nothing for a name with no `.`, or only one in front.
fn extension(text: &str) -> String {
    let name = name_of(text);
    let pieces: Vec<&str> = name.split('.').collect();
    if pieces.len() < 2 || (pieces.len() == 2 && pieces.first() == Some(&"")) {
        return String::new();
    }
    pieces.last().map_or_else(String::new, |last| (*last).to_owned())
}

/// The name without its extension and the `.` before it.
fn stem(text: &str) -> String {
    let name = name_of(text);
    let extension = extension(text);
    if extension.is_empty() {
        return name;
    }
    let keep = name.chars().count().saturating_sub(extension.chars().count() + 1);
    name.chars().take(keep).collect()
}

/// The path without its last part, further up from a `..`.
fn parent(text: &str) -> String {
    let mut parts = parts(text);
    if parts.last().is_some_and(|last| last == "..") {
        parts.push("..".to_owned());
    } else {
        parts.pop();
    }
    rebuilt(text, &parts)
}

/// The same path written simply: a `..` takes off the name before it, and one that would go
/// above the start stays in a relative path and goes from an absolute one.
fn normalized(text: &str) -> String {
    let absolute = text.starts_with('/');
    let mut kept: Vec<String> = Vec::new();
    for part in parts(text) {
        if part != ".." {
            kept.push(part);
        } else if kept.last().is_some_and(|last| last != "..") {
            kept.pop();
        } else if !absolute {
            kept.push(part);
        }
    }
    rebuilt(text, &kept)
}

/// `parts` joined by `/`, from the root when `text` is, and `.` for none of a relative path.
fn rebuilt(text: &str, parts: &[String]) -> String {
    let joined = parts.join("/");
    if text.starts_with('/') {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".to_owned()
    } else {
        joined
    }
}

/// `part`, a path from outside, inside `text`: a leading `/` is `text`'s own top and empty parts
/// are passed over, but anything that could lead out, or that Windows reads as other than its
/// text, fails with an `InvalidValue`.
fn inside(text: &str, part: &str, span: Span) -> Run<String> {
    let leaves = || Err(invalid(format!("`{}` would leave `{}`", cut_input(part), cut_input(text)), span));
    let cant =
        || Err(invalid(format!("`{}` can't be a path inside `{}`", cut_input(part), cut_input(text)), span));
    if part.chars().any(|c| u32::from(c) < 32 || u32::from(c) == 127 || c == '\\') {
        return cant();
    }
    let kept = parts(part);
    if kept.first().is_some_and(|first| is_drive(first)) {
        return leaves();
    }
    for name in &kept {
        let dropped = name.ends_with('.') || name.ends_with(' ');
        if name == ".." || (dropped && name.starts_with("..")) {
            return leaves();
        }
        if dropped || is_device(name) {
            return cant();
        }
    }
    Ok(joined(text, &kept.join("/")))
}

/// Whether a name starts as a Windows drive does: a letter, then `:`.
fn is_drive(name: &str) -> bool {
    let mut chars = name.chars();
    matches!((chars.next(), chars.next()), (Some(letter), Some(':')) if letter.is_ascii_alphabetic())
}

/// Whether Windows reads a name as a device, like `CON` or `com1`: alone or before any extension,
/// in any case, with spaces before the extension dropped.
fn is_device(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or_default().trim_end().to_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    if stem.chars().count() != 4 || !(stem.starts_with("COM") || stem.starts_with("LPT")) {
        return false;
    }
    stem.chars().last().is_some_and(|last| "123456789¹²³".contains(last))
}

/// Text from outside as a message shows it: whole up to 40 characters, or its first 20 and `…`,
/// with control characters escaped, as `std.string`'s `cut_input` does.
fn cut_input(text: &str) -> String {
    if text.chars().count() <= 40 {
        return escaped(text);
    }
    let first: String = text.chars().take(20).collect();
    format!("{}…", escaped(&first))
}

/// Each control character written as an escape, as `\n` or `\u{1b}`.
fn escaped(text: &str) -> String {
    let control = |c: char| u32::from(c) < 32 || (127..160).contains(&u32::from(c));
    if !text.chars().any(control) {
        return text.to_owned();
    }
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if control(c) => {
                let _ = write!(out, "\\u{{{:02x}}}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out
}
