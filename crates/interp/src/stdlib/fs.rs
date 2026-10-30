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
        let done = match (method, paths.as_slice()) {
            ("exists", [Value::Str(path)]) => return Ok(Value::Bool(std::path::Path::new(path.as_ref()).exists())),
            ("read_text", [Value::Str(path)]) => std::fs::read_to_string(path.as_ref()).map(|text| Value::str(&text)),
            ("write_text", [Value::Str(path), Value::Str(text)]) => {
                std::fs::write(path.as_ref(), text.as_bytes()).map(|()| Value::Unit)
            }
            ("read_bytes", [Value::Str(path)]) => std::fs::read(path.as_ref()).map(|data| Value::Bytes(Rc::from(data))),
            ("write_bytes", [Value::Str(path), Value::Bytes(data)]) => {
                std::fs::write(path.as_ref(), data).map(|()| Value::Unit)
            }
            ("list_dir", [Value::Str(path)]) => return self.list_dir(path, span),
            ("make_dir", [Value::Str(path)]) => std::fs::create_dir_all(path.as_ref()).map(|()| Value::Unit),
            ("remove", [Value::Str(path)]) => remove(path).map(|()| Value::Unit),
            ("metadata", [Value::Str(path)]) => match std::fs::metadata(path.as_ref()) {
                Ok(found) => {
                    let size = i64::try_from(found.len()).unwrap_or(i64::MAX);
                    let fields = vec![
                        ("size", Value::Int(size)),
                        ("is_file", Value::Bool(found.is_file())),
                        ("is_dir", Value::Bool(found.is_dir())),
                    ];
                    Ok(self.make_value("Metadata", fields, span)?)
                }
                Err(err) => Err(err),
            },
            ("exists" | "read_text" | "write_text" | "read_bytes" | "write_bytes" | "list_dir" | "make_dir" | "remove" | "metadata", _) => {
                return Err(Diagnostic::new(format!("`{method}` was called with the wrong arguments"), span));
            }
            _ => return Err(no_method(&Value::Capability(Capability::Fs), method, span)),
        };
        match done {
            Ok(value) => Ok(Value::Ok(Rc::new(value))),
            Err(err) => self.io_error(&err, span),
        }
    }

    /// The paths in a folder, sorted by name so every system lists them in the same order.
    fn list_dir(&mut self, folder: &str, span: Span) -> Result<Value, Diagnostic> {
        let entries = match std::fs::read_dir(folder) {
            Ok(entries) => entries,
            Err(err) => return self.io_error(&err, span),
        };
        let mut names = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) => names.push(entry.file_name().to_string_lossy().into_owned()),
                Err(err) => return self.io_error(&err, span),
            }
        }
        names.sort();
        let mut paths = Vec::with_capacity(names.len());
        for name in names {
            let text = if folder.is_empty() || folder.ends_with('/') {
                format!("{folder}{name}")
            } else {
                format!("{folder}/{name}")
            };
            paths.push(self.make_value("Path", vec![("text", Value::str(&text))], span)?);
        }
        Ok(Value::Ok(Rc::new(Value::list(paths))))
    }

    fn io_error(&mut self, err: &std::io::Error, span: Span) -> Result<Value, Diagnostic> {
        let error = self.make_value("IoError", vec![("reason", Value::str(&err.to_string()))], span)?;
        Ok(Value::Err(Rc::new(error)))
    }
}

/// Removes a file, or a folder if it's empty.
fn remove(path: &str) -> std::io::Result<()> {
    if std::fs::symlink_metadata(path)?.is_dir() { std::fs::remove_dir(path) } else { std::fs::remove_file(path) }
}

/// A `Path`'s text, or a `str` as it is.
fn path_text(value: &Value) -> Value {
    match value {
        Value::Instance(instance) if instance.type_name.as_ref() == "Path" => {
            instance.field("text").cloned().unwrap_or_else(|| value.clone())
        }
        other => other.clone(),
    }
}
