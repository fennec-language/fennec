//! Places in text as the protocol writes them, and files as it names them.
//!
//! The protocol gives a place as a line and a column, both from 0, with the column counted
//! in UTF-16 code units, as editors written in JavaScript count. Fennec counts bytes. A file
//! is named by a `file://` URI.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::json::Json;

/// The protocol's position of byte `offset` in `text`. An offset past the end, or inside a
/// character, counts as the place just after it.
pub(crate) fn position(text: &str, offset: usize) -> Json {
    let offset = offset.min(text.len());
    let before = text.get(..offset).unwrap_or_else(|| {
        let start = (0..offset).rev().find(|at| text.is_char_boundary(*at)).unwrap_or(0);
        text.get(..start).unwrap_or("")
    });
    let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
    let column: usize = before.get(line_start..).unwrap_or("").chars().map(char::len_utf16).sum();
    Json::object([("line", Json::number(before.matches('\n').count())), ("character", Json::number(column))])
}

/// The protocol's range for bytes `start..end` of `text`.
pub(crate) fn range(text: &str, start: usize, end: usize) -> Json {
    Json::object([("start", position(text, start)), ("end", position(text, end.max(start)))])
}

/// The byte offset of a protocol position in `text`. A line past the end is the end of the
/// text, and a column past the end of its line is the end of the line.
pub(crate) fn offset(text: &str, position: &Json) -> Option<usize> {
    let line = position.get("line")?.as_usize()?;
    let column = position.get("character")?.as_usize()?;
    let mut line_start = 0;
    for _ in 0..line {
        match text.get(line_start..).and_then(|rest| rest.find('\n')) {
            Some(newline) => line_start += newline + 1,
            None => return Some(text.len()),
        }
    }
    let line_text = text.get(line_start..)?.split('\n').next().unwrap_or("");
    let mut units = 0;
    for (at, ch) in line_text.char_indices() {
        if units >= column {
            return Some(line_start + at);
        }
        units += ch.len_utf16();
    }
    Some(line_start + line_text.len())
}

/// The path a `file://` URI names. None for any other kind of URI.
pub fn path_of(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // A URI may name a host before the path; files on this machine have none or `localhost`.
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let decoded = decode(rest)?;
    // Windows writes `file:///C:/folder`, whose path is `C:/folder`.
    let windows =
        decoded.get(1..3).is_some_and(|drive| drive.ends_with(':') && drive.starts_with(char::is_alphabetic));
    Some(PathBuf::from(if windows { decoded.get(1..).unwrap_or(&decoded) } else { &decoded }))
}

/// The `file://` URI of a path.
pub fn uri_of(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut uri = String::from(if text.starts_with('/') { "file://" } else { "file:///" });
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~:".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            let _ = write!(uri, "%{byte:02X}");
        }
    }
    uri
}

/// Undoes `%XX` escapes.
fn decode(text: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text.as_bytes();
    while let Some((&byte, after)) = rest.split_first() {
        if byte == b'%' {
            let hex = std::str::from_utf8(after.get(..2)?).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            rest = after.get(2..)?;
        } else {
            bytes.push(byte);
            rest = after;
        }
    }
    String::from_utf8(bytes).ok()
}
