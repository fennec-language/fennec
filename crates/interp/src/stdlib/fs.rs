//! `std.fs`: the methods of the `Fs` capability. Each takes a `Path` or a `str`, and a
//! failure is an `IoError` value rather than a stop.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{LazyLock, Mutex, PoisonError};

use fennec_syntax::{Diagnostic, Span};

use super::{Module, no_constants, no_functions};
use crate::call::CallArg;
use crate::interp::Interp;
use crate::methods::no_method;
use crate::value::{Capability, Instance, Value};

pub(crate) const MODULE: Module =
    Module { functions: &[], call: no_functions, constant: no_constants, methods: &[("File", file_method)] };

/// A file `fs.create` or `fs.open` has lent, by the handle its `File` holds. One being written
/// goes into a file of its own beside where it's going, which takes that place when its block
/// ends. A test's fake files are held in memory, and go into its fake file system then.
enum OpenFile {
    Writing { out: BufWriter<std::fs::File>, temporary: PathBuf, target: PathBuf },
    Reading(BufReader<std::fs::File>),
    FakeWriting { target: String, data: Vec<u8> },
    FakeReading { data: Vec<u8>, at: usize },
}

static OPEN: LazyLock<Mutex<HashMap<i64, OpenFile>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT: LazyLock<Mutex<i64>> = LazyLock::new(|| Mutex::new(1));

/// Keeps an open file, giving back the `File` that holds its handle.
fn lent(file: OpenFile) -> Value {
    let mut next = NEXT.lock().unwrap_or_else(PoisonError::into_inner);
    let handle = *next;
    *next += 1;
    OPEN.lock().unwrap_or_else(PoisonError::into_inner).insert(handle, file);
    Value::made("File", vec![("_handle", Value::Int(handle))])
}

fn handle_of(file: &Value) -> Option<i64> {
    match file {
        Value::Instance(instance) => match instance.field("_handle") {
            Some(Value::Int(handle)) => Some(*handle),
            _ => None,
        },
        _ => None,
    }
}

/// Takes an open file out of the table, to finish or drop it.
fn taken(file: &Value) -> Option<OpenFile> {
    OPEN.lock().unwrap_or_else(PoisonError::into_inner).remove(&handle_of(file)?)
}

/// Where a file being written to `target` goes until its block ends: beside it, so renaming it
/// into place never crosses from one disk to another, and hidden, with a name no other has.
fn temporary_beside(target: &Path) -> PathBuf {
    static MADE: LazyLock<Mutex<u64>> = LazyLock::new(|| Mutex::new(0));
    let mut made = MADE.lock().unwrap_or_else(PoisonError::into_inner);
    *made += 1;
    let name = target.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    target.with_file_name(format!(".{name}.{}.{}.fennec-partial", std::process::id(), *made))
}

/// `fs._create(path)`: a new file beside `path` to write.
fn create(path: &str) -> std::io::Result<Value> {
    let target = PathBuf::from(path);
    let temporary = temporary_beside(&target);
    let out = BufWriter::new(std::fs::File::create(&temporary)?);
    Ok(lent(OpenFile::Writing { out, temporary, target }))
}

/// `fs._open(path)`: the file at `path`, to read.
fn open(path: &str) -> std::io::Result<Value> {
    Ok(lent(OpenFile::Reading(BufReader::new(std::fs::File::open(path)?))))
}

/// `fs._finish(file)`: a file written takes its target's place, and one read is closed. A fake
/// one goes into the test's fake file system, by `fake`.
fn finish(file: &Value, fake: &mut dyn FnMut(&str, &[u8]) -> Result<(), String>) -> Result<(), String> {
    match taken(file) {
        Some(OpenFile::Writing { mut out, temporary, target }) => {
            let done = out.flush().and_then(|()| std::fs::rename(&temporary, &target));
            if done.is_err() {
                let _ = std::fs::remove_file(&temporary);
            }
            done.map_err(|err| err.to_string())
        }
        Some(OpenFile::FakeWriting { target, data }) => fake(&target, &data),
        Some(OpenFile::Reading(_) | OpenFile::FakeReading { .. }) | None => Ok(()),
    }
}

/// `fs._discard(file)`: a file written is thrown away, leaving its target as it was.
fn discard(file: &Value) {
    if let Some(OpenFile::Writing { out, temporary, .. }) = taken(file) {
        drop(out);
        let _ = std::fs::remove_file(&temporary);
    }
}

/// A method of a lent `File` by its handle: `write(data)`, `read()` or `read_line()`, as native
/// code calls it and the interpreter does.
pub(crate) fn real_file(handle: i64, method: &str, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
    let mut open = OPEN.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(file) = open.get_mut(&handle) else {
        return Ok(io_error("this file was closed when its `with` block ended"));
    };
    let done: Result<Value, String> = match (method, args, file) {
        ("write", [data @ (Value::Str(_) | Value::Bytes(_))], OpenFile::Writing { out, .. }) => {
            out.write_all(bytes_of(data)).map(|()| Value::Unit).map_err(|err| err.to_string())
        }
        ("write", [data @ (Value::Str(_) | Value::Bytes(_))], OpenFile::FakeWriting { data: kept, .. }) => {
            kept.extend_from_slice(bytes_of(data));
            Ok(Value::Unit)
        }
        ("write", [_], _) => Err("this file was opened to read, not to write".to_owned()),
        ("read", [], OpenFile::Reading(reader)) => {
            let mut data = Vec::new();
            reader.read_to_end(&mut data).map(|_| Value::Bytes(Rc::from(data))).map_err(|err| err.to_string())
        }
        ("read", [], OpenFile::FakeReading { data, at }) => {
            let rest = data.get(*at..).unwrap_or_default().to_vec();
            *at = data.len();
            Ok(Value::Bytes(Rc::from(rest)))
        }
        ("read_line", [], OpenFile::Reading(reader)) => {
            let mut line = Vec::new();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) => Ok(Value::None),
                Ok(_) => line_value(line),
                Err(err) => Err(err.to_string()),
            }
        }
        ("read_line", [], OpenFile::FakeReading { data, at }) => {
            let rest = data.get(*at..).unwrap_or_default();
            if rest.is_empty() {
                Ok(Value::None)
            } else {
                let end = rest.iter().position(|byte| *byte == b'\n').map_or(rest.len(), |end| end + 1);
                *at += end;
                line_value(rest.get(..end).unwrap_or_default().to_vec())
            }
        }
        ("read" | "read_line", [], _) => Err("this file was made to write, not to read".to_owned()),
        _ => return Err(Diagnostic::new(format!("a File has no method `{method}`"), span)),
    };
    Ok(match done {
        Ok(value) => Value::Ok(Rc::new(value)),
        Err(reason) => io_error(&reason),
    })
}

fn bytes_of(data: &Value) -> &[u8] {
    match data {
        Value::Str(text) => text.as_bytes(),
        Value::Bytes(data) => data,
        _ => &[],
    }
}

/// A line read, without its line break, as text.
fn line_value(mut line: Vec<u8>) -> Result<Value, String> {
    if line.last() == Some(&b'\n') {
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
    }
    String::from_utf8(line).map(|text| Value::str(&text)).map_err(|_| "this line isn't UTF-8 text".to_owned())
}

/// `File`'s methods written in Rust, as the interpreter calls them.
fn file_method(
    _: &mut Interp<'_>,
    file: &Instance,
    method: &str,
    args: Vec<CallArg>,
    span: Span,
) -> Result<Value, Diagnostic> {
    let Some(Value::Int(handle)) = file.field("_handle") else {
        return Err(no_method(&Value::str("File"), method, span));
    };
    let args: Vec<Value> = args.into_iter().map(|arg| arg.value).collect();
    real_file(*handle, method, &args, span)
}

impl Interp<'_> {
    pub(crate) fn fs_method(&mut self, method: &str, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
        let paths: Vec<Value> = args.iter().map(path_text).collect();
        if let ("only", [Value::Str(folder)]) = (method, paths.as_slice()) {
            return Ok(crate::limits::Limited::to(Capability::Fs, vec![Rc::from(crate::limits::absolute(folder))]));
        }
        if self.world.as_ref().is_some_and(|world| !world.real()) {
            match (method, paths.as_slice()) {
                ("_create", [Value::Str(path)]) => {
                    let file = OpenFile::FakeWriting { target: path.to_string(), data: Vec::new() };
                    return Ok(Value::Ok(Rc::new(lent(file))));
                }
                ("_open", [Value::Str(_)]) => {
                    let read = self.fake_fs("read_bytes", &paths, span);
                    return match read {
                        Some(Ok(Value::Ok(data))) => match data.as_ref() {
                            Value::Bytes(data) => {
                                let file = OpenFile::FakeReading { data: data.to_vec(), at: 0 };
                                Ok(Value::Ok(Rc::new(lent(file))))
                            }
                            _ => Ok(io_error("the fake file system gave something that isn't bytes")),
                        },
                        Some(other) => other,
                        None => Ok(io_error("there's no fake file system here")),
                    };
                }
                ("_finish", [file]) => {
                    let mut written = Ok(());
                    let done = finish(file, &mut |target, data| {
                        let given = [Value::str(target), Value::Bytes(Rc::from(data.to_vec()))];
                        written = match self.fake_fs("write_bytes", &given, span) {
                            Some(Ok(Value::Ok(_))) => Ok(()),
                            Some(Ok(failed)) => Err(crate::format::repr(&failed)),
                            Some(Err(problem)) => Err(problem.message),
                            None => Err("there's no fake file system here".to_owned()),
                        };
                        Ok(())
                    });
                    return Ok(match done.and(written) {
                        Ok(()) => Value::Ok(Rc::new(Value::Unit)),
                        Err(reason) => io_error(&reason),
                    });
                }
                _ => {}
            }
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
        ("exists", [Value::Str(path)]) => return Ok(Value::Bool(Path::new(path.as_ref()).exists())),
        ("read_text", [Value::Str(path)]) => std::fs::read_to_string(path.as_ref()).map(|text| Value::str(&text)),
        ("write_text", [Value::Str(path), Value::Str(text)]) => {
            std::fs::write(path.as_ref(), text.as_bytes()).map(|()| Value::Unit)
        }
        ("read_bytes", [Value::Str(path)]) => std::fs::read(path.as_ref()).map(|data| Value::Bytes(Rc::from(data))),
        ("write_bytes", [Value::Str(path), Value::Bytes(data)]) => {
            std::fs::write(path.as_ref(), data).map(|()| Value::Unit)
        }
        ("list_dir", [Value::Str(path)]) => return Ok(list_dir(path)),
        ("_create", [Value::Str(path)]) => create(path),
        ("_open", [Value::Str(path)]) => open(path),
        ("_finish", [file]) => {
            return Ok(match finish(file, &mut |_, _| Ok(())) {
                Ok(()) => Value::Ok(Rc::new(Value::Unit)),
                Err(reason) => io_error(&reason),
            });
        }
        ("_discard", [file]) => {
            discard(file);
            return Ok(Value::Unit);
        }
        ("walk", [Value::Str(path)]) => {
            let mut found = Vec::new();
            return Ok(match walk_into(path, &mut found) {
                Ok(()) => paths_of(&found),
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
    paths_in(folder, &names)
}

/// The paths of the things called `names` in a folder.
pub(crate) fn paths_in(folder: &str, names: &[String]) -> Value {
    paths_of(&names.iter().map(|name| joined(folder, name)).collect::<Vec<_>>())
}

/// A result holding a list of `Path`s.
pub(crate) fn paths_of(texts: &[String]) -> Value {
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
