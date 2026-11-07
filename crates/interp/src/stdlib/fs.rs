//! `std.fs`: the methods of the `Fs` capability. Each takes a `Path` or a `str`, and a
//! failure is an `IoError` value rather than a stop.

use std::rc::Rc;

use fennec_syntax::{Diagnostic, Span};

use super::{Module, no_constants, no_functions};
use crate::interp::Interp;
use crate::methods::no_method;
use crate::value::{Capability, Value};

pub(crate) const MODULE: Module = Module { functions: &[], call: no_functions, constant: no_constants, methods: &[] };

impl Interp<'_> {
    pub(crate) fn fs_method(&mut self, method: &str, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
        let paths: Vec<Value> = args.iter().map(path_text).collect();
        if let ("only", [Value::Str(folder)]) = (method, paths.as_slice()) {
            return Ok(crate::limits::Limited::to(Capability::Fs, vec![Rc::from(crate::limits::absolute(folder))]));
        }
        if let Some(faked) = self.fake_fs(method, &paths, span) {
            return faked;
        }
        real_fs(method, &paths, span)
    }
}

/// A method of the real `Fs`, which needs nothing of a program's, on paths as `path_text` gives
/// them.
pub(crate) fn real_fs(method: &str, paths: &[Value], span: Span) -> Result<Value, Diagnostic> {
    let done = match (method, paths) {
        ("exists", [Value::Str(path)]) => return Ok(Value::Bool(std::path::Path::new(path.as_ref()).exists())),
        ("read_text", [Value::Str(path)]) => std::fs::read_to_string(path.as_ref()).map(|text| Value::str(&text)),
        ("write_text", [Value::Str(path), Value::Str(text)]) => {
            std::fs::write(path.as_ref(), text.as_bytes()).map(|()| Value::Unit)
        }
        ("read_bytes", [Value::Str(path)]) => std::fs::read(path.as_ref()).map(|data| Value::Bytes(Rc::from(data))),
        ("write_bytes", [Value::Str(path), Value::Bytes(data)]) => {
            std::fs::write(path.as_ref(), data).map(|()| Value::Unit)
        }
        ("list_dir", [Value::Str(path)]) => return Ok(list_dir(path)),
        ("walk", [Value::Str(path)]) => {
            let mut found = Vec::new();
            return Ok(match walk_into(path, &mut found) {
                Ok(()) => paths_of(found),
                Err(err) => io_error(&err.to_string()),
            });
        }
        ("make_dir", [Value::Str(path)]) => std::fs::create_dir_all(path.as_ref()).map(|()| Value::Unit),
        ("remove", [Value::Str(path)]) => remove(path).map(|()| Value::Unit),
        ("metadata", [Value::Str(path)]) => std::fs::metadata(path.as_ref()).map(|found| {
            let size = i64::try_from(found.len()).unwrap_or(i64::MAX);
            Value::made(
                "Metadata",
                vec![
                    ("size", Value::Int(size)),
                    ("is_file", Value::Bool(found.is_file())),
                    ("is_dir", Value::Bool(found.is_dir())),
                ],
            )
        }),
        ("exists" | "read_text" | "write_text" | "read_bytes" | "write_bytes" | "list_dir" | "walk" | "make_dir"
        | "remove" | "metadata", _) => {
            return Err(Diagnostic::new(format!("`{method}` was called with the wrong arguments"), span));
        }
        _ => return Err(no_method(&Value::Capability(Capability::Fs), method, span)),
    };
    Ok(match done {
        Ok(value) => Value::Ok(Rc::new(value)),
        Err(err) => io_error(&err.to_string()),
    })
}

/// The paths in a folder, sorted by name so every system lists them in the same order.
fn list_dir(folder: &str) -> Value {
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(err) => return io_error(&err.to_string()),
    };
    let mut names = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => names.push(entry.file_name().to_string_lossy().into_owned()),
            Err(err) => return io_error(&err.to_string()),
        }
    }
    names.sort();
    paths_in(folder, names)
}

/// The paths of the things called `names` in a folder.
pub(crate) fn paths_in(folder: &str, names: Vec<String>) -> Value {
    paths_of(names.iter().map(|name| joined(folder, name)).collect())
}

/// A result holding a list of `Path`s.
pub(crate) fn paths_of(texts: Vec<String>) -> Value {
    let paths = texts.iter().map(|text| Value::made("Path", vec![("text", Value::str(text))]));
    Value::Ok(Rc::new(Value::list(paths.collect())))
}

/// The path of the thing called `name` in `folder`.
pub(crate) fn joined(folder: &str, name: &str) -> String {
    if folder.is_empty() || folder.ends_with('/') { format!("{folder}{name}") } else { format!("{folder}/{name}") }
}

/// Every path under `folder`, onto `found`: each folder's before what's in it, and each folder's
/// things sorted by name, as `list_dir` gives them. A link to a folder is listed but not gone
/// into, so a loop of links can't go on forever.
fn walk_into(folder: &str, found: &mut Vec<String>) -> std::io::Result<()> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(folder)? {
        names.push(entry?.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    for name in names {
        let path = joined(folder, &name);
        let is_folder = std::fs::symlink_metadata(&path)?.is_dir();
        found.push(path.clone());
        if is_folder {
            walk_into(&path, found)?;
        }
    }
    Ok(())
}

/// A failed result holding an `IoError`.
pub(crate) fn io_error(reason: &str) -> Value {
    Value::Err(Rc::new(Value::made_error("IoError", vec![("reason", Value::str(reason))])))
}

/// Removes a file, or a folder if it's empty.
fn remove(path: &str) -> std::io::Result<()> {
    if std::fs::symlink_metadata(path)?.is_dir() { std::fs::remove_dir(path) } else { std::fs::remove_file(path) }
}

/// A `Path`'s text, or a `str` as it is.
pub(crate) fn path_text(value: &Value) -> Value {
    match value {
        Value::Instance(instance) if instance.type_name.as_ref() == "Path" => {
            instance.field("text").cloned().unwrap_or_else(|| value.clone())
        }
        other => other.clone(),
    }
}
