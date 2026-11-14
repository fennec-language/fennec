//! The `Fs` capability and the files it lends, as a program sees them, over the file system
//! `disk.rs` models. Each method is written here on its own, from `docs/language.md` and
//! `std.fs`'s documentation, and checked against the compiled program with `compare`.
//!
//! An `Fs` narrowed with `only` reaches what's inside its folder, by the path's text with `.` and
//! `..` worked out, and works beneath that folder, which has to be there. One narrowed with
//! `read_only` refuses to write, make, remove or create anything. What either refuses fails with
//! `IoError.PermissionDenied`, saying why. Bytes aren't modeled, so reading or writing them is
//! refused.

use fennec_syntax::{Span, WithName};

use super::disk::{Disk, Done, Fault, Reached};
use super::stream::{FileLines, Source};
use super::{Flow, Interp, Run, unsupported};
use crate::value::{Cap, Reach, Value};

/// The methods of an `Fs` that change what's on the disk, which one narrowed with `read_only`
/// refuses. Closing a file read through one is fine: it changes nothing.
const CHANGES: &[&str] = &["write_text", "write_bytes", "make_dir", "remove", "create"];

/// The cases of `IoError`.
pub(crate) const IO_CASES: &[&str] = &["NotFound", "PermissionDenied", "AlreadyExists", "Other"];

/// Where a call through an `Fs` reaches: through the whole file system, by the path's text, or
/// beneath the folder a narrowed `Fs` reaches, by the parts of the path inside it.
enum Whither {
    Whole(String),
    Beneath(Vec<String>, Vec<String>),
}

/// An `IoError` of this case, saying this.
pub(crate) fn io_error(case: &str, reason: String) -> Value {
    Value::instance("IoError", Some(case), vec![("reason".to_owned(), Value::Str(reason))])
}

fn failing<T>(case: &str, reason: String, span: Span) -> Run<T> {
    Err(Flow::Fail(io_error(case, reason), span))
}

/// What a call on the file system gave, made into what the program gets.
pub(crate) fn done<T>(done: Done<T>, span: Span) -> Run<T> {
    match done {
        Ok(value) => Ok(value),
        Err(Fault::Fails(case, reason)) => failing(case, reason, span),
        Err(Fault::Refused(why)) => unsupported(why),
    }
}

impl Interp {
    /// Works on the file system this run shares.
    fn disk<R>(&self, span: Span, work: impl FnOnce(&mut Disk) -> Done<R>) -> Run<R> {
        let Ok(mut disk) = self.shared.disk.lock() else { return unsupported("the file system was lost") };
        done(work(&mut disk), span)
    }

    /// Whether a type the standard library names is the one it means here: a program's own type
    /// by that name stands in its way, and is refused rather than confused with it.
    pub(crate) fn is_std_type(&self, name: &str) -> bool {
        !self.program.types.contains_key(name)
    }

    /// A method of an `Fs`.
    pub(crate) fn fs_method(
        &mut self,
        reach: &Reach,
        name: &str,
        args: &[Value],
        named: &[(String, Value)],
        span: Span,
    ) -> Run<Value> {
        if !named.is_empty() {
            return unsupported(format!("the Fs method `{name}` with a named argument"));
        }
        match (name, args) {
            ("read_only", []) => Ok(Value::Cap(Cap::Fs(Reach { read_only: true, ..reach.clone() }))),
            ("only", [path]) => {
                let text = self.path_text(path)?;
                self.narrowed(reach, &text, span)
            }
            ("write_text", [path, Value::Str(text)]) => {
                let whither = self.whither(reach, name, path, span)?;
                self.disk(span, |disk| {
                    let reached = reached(disk, &whither)?;
                    disk.write_text(reached, text)
                })?;
                Ok(Value::Unit)
            }
            ("make_dir", [path]) => {
                match self.whither(reach, name, path, span)? {
                    Whither::Whole(text) => self.disk(span, |disk| disk.make_whole(&text))?,
                    Whither::Beneath(folder, inside) => {
                        self.disk(span, |disk| disk.make_beneath(&folder, &inside))?;
                    }
                }
                Ok(Value::Unit)
            }
            ("read_text_if_present", [path]) => match self.fs_read(reach, path, span) {
                Err(Flow::Fail(Value::Instance(error), _)) if error.case.as_deref() == Some("NotFound") => {
                    Ok(Value::None)
                }
                other => other,
            },
            ("read_text", [path]) => self.fs_read(reach, path, span),
            ("exists" | "remove" | "list_dir" | "walk" | "metadata", [path]) => {
                self.fs_look(reach, name, path, span)
            }
            ("create" | "open", _) => unsupported(format!("`{name}` outside a `with`")),
            ("read_bytes" | "write_bytes" | "read_bytes_if_present", _) => unsupported("bytes"),
            _ => unsupported(format!("the Fs method `{name}`")),
        }
    }

    fn fs_read(&mut self, reach: &Reach, path: &Value, span: Span) -> Run<Value> {
        let whither = self.whither(reach, "read_text", path, span)?;
        self.disk(span, |disk| {
            let reached = reached(disk, &whither)?;
            disk.read_text(reached)
        })
        .map(Value::Str)
    }

    /// `exists`, `remove`, `list_dir`, `walk` and `metadata`.
    fn fs_look(&mut self, reach: &Reach, name: &str, path: &Value, span: Span) -> Run<Value> {
        let whither = self.whither(reach, name, path, span)?;
        let shown = self.path_text(path)?;
        if matches!(name, "list_dir" | "walk") && !self.is_std_type("Path") {
            return unsupported("a program's own type named `Path`, as `std.fs` names its own");
        }
        if name == "metadata" && !self.is_std_type("Metadata") {
            return unsupported("a program's own type named `Metadata`, as `std.fs` names its own");
        }
        self.disk(span, |disk| {
            let reached = reached(disk, &whither)?;
            Ok(match name {
                "exists" => Value::Bool(disk.exists(reached)?),
                "remove" => {
                    disk.remove(reached)?;
                    Value::Unit
                }
                "list_dir" => paths(disk.list_dir(reached, &shown)?),
                "walk" => paths(disk.walk_from(reached, &shown)?),
                _ => {
                    let found = disk.metadata(reached)?;
                    let size = match found.size {
                        Some(size) => Value::Int(i64::try_from(size).unwrap_or(i64::MAX)),
                        None if found.is_dir => Value::Unknown(
                            "a folder's size, which differs from one system to another".to_owned(),
                        ),
                        None => Value::Unknown(
                            "the size of a file that was there before the program started".to_owned(),
                        ),
                    };
                    let fields = vec![
                        ("size".to_owned(), size),
                        ("is_file".to_owned(), Value::Bool(!found.is_dir)),
                        ("is_dir".to_owned(), Value::Bool(found.is_dir)),
                    ];
                    Value::instance("Metadata", None, fields)
                }
            })
        })
    }

    /// The text of a path a method of `Fs` is given: a `str`, or a `Path`'s.
    pub(crate) fn path_text(&self, value: &Value) -> Run<String> {
        match value {
            Value::Str(text) => Ok(text.clone()),
            Value::Instance(path) if path.ty == "Path" && self.is_std_type("Path") => {
                match path.field("text") {
                    Some(Value::Str(text)) => Ok(text.clone()),
                    _ => unsupported("a Path without its text"),
                }
            }
            other => unsupported(format!("a path that's a {}", other.kind())),
        }
    }

    /// `fs.only(path)`: an `Fs` that reaches only what's inside `path`, or, narrowed already,
    /// only what's inside both.
    fn narrowed(&self, reach: &Reach, text: &str, span: Span) -> Run<Value> {
        let full = self.disk(span, |disk| disk.absolute(text))?;
        let folders = match &reach.folders {
            None => vec![full],
            Some(folders) if folders.iter().any(|folder| full.starts_with(folder)) => vec![full],
            Some(_) => Vec::new(),
        };
        Ok(Value::Cap(Cap::Fs(Reach { folders: Some(folders), read_only: reach.read_only })))
    }

    /// Where a call through `reach` reaches with `path`, or the failure it gives instead: a
    /// read-only `Fs` asked to change something, a narrowed one asked for something outside its
    /// folder, and a path holding a NUL, in that order, as the runtime checks them.
    fn whither(&self, reach: &Reach, method: &str, path: &Value, span: Span) -> Run<Whither> {
        let text = self.path_text(path)?;
        if reach.read_only && CHANGES.contains(&method) {
            let reason = format!("this Fs only reads, so it can't change `{}`", cut(&text));
            return failing("PermissionDenied", reason, span);
        }
        let Some(folders) = &reach.folders else {
            holding_nul(&text, span)?;
            return Ok(Whither::Whole(text));
        };
        let full = self.disk(span, |disk| disk.absolute(&text))?;
        let Some(folder) = folders.iter().find(|folder| full.starts_with(folder)) else {
            let listed: Vec<String> = self.disk(span, |disk| {
                Ok(folders.iter().map(|folder| format!("`{}`", disk.shown(folder))).collect())
            })?;
            let listed = if listed.is_empty() { "nothing".to_owned() } else { listed.join(" or ") };
            return failing(
                "PermissionDenied",
                format!("`{text}` is outside what this Fs can reach: {listed}"),
                span,
            );
        };
        holding_nul(&text, span)?;
        let inside = full.get(folder.len()..).unwrap_or_default().to_vec();
        Ok(Whither::Beneath(folder.clone(), inside))
    }

    /// A case of `IoError` the program makes, with its reason.
    pub(crate) fn io_case(&mut self, case: Option<&str>, given: Vec<super::calls::ArgVal>) -> Run<Value> {
        let Some(case) = case.filter(|case| IO_CASES.contains(case)) else {
            return unsupported("making an `IoError` without a case it has");
        };
        let [arg] = <[_; 1]>::try_from(given)
            .map_err(|_| Flow::Unsupported("an `IoError` with these fields".to_owned()))?;
        if arg.name().is_some_and(|name| name != "reason") {
            return unsupported("an `IoError` with these fields");
        }
        match self.arg_as_value(arg)? {
            Value::Str(reason) => Ok(io_error(case, reason)),
            other => unsupported(format!("an `IoError` whose reason is a {}", other.kind())),
        }
    }

    /// Whether a call is `fs.create(...)` or `fs.open(...)`, a source a `with` lends a file from.
    pub(crate) fn is_file_source(&self, target: &super::calls::Target, span: Span) -> Run<Option<Reach>> {
        let super::calls::Target::Method { receiver, name, .. } = target else { return Ok(None) };
        if !matches!(name.as_str(), "create" | "open") {
            return Ok(None);
        }
        let reach_of = |held: &Value| match held {
            Value::Cap(Cap::Fs(reach)) => Some(reach.clone()),
            _ => None,
        };
        match receiver {
            super::calls::Receiver::Place(place) => self.peek_place(place, span, reach_of),
            super::calls::Receiver::Value(value) => Ok(reach_of(value)),
        }
    }

    /// Opens the file `fs.create(path, durable=...)` or `fs.open(path)` lends, by its handle.
    pub(crate) fn file_source(
        &mut self,
        reach: &Reach,
        create: bool,
        given: Vec<super::calls::ArgVal>,
        span: Span,
    ) -> Run<i64> {
        let mut args = Vec::new();
        for arg in given {
            let label = arg.name().map(str::to_owned);
            match (label.as_deref(), self.arg_as_value(arg)?) {
                (None, value) => args.push(value),
                (Some("durable"), Value::Bool(_)) if create => {}
                _ => return unsupported("a file's source with these arguments"),
            }
        }
        let ((true, [path] | [path, Value::Bool(_)]) | (false, [path])) = (create, args.as_slice()) else {
            return unsupported("a file's source with these arguments");
        };
        if !self.is_std_type("File") {
            return unsupported("a program's own type named `File`, as `std.fs` names its own");
        }
        let method = if create { "create" } else { "open" };
        let whither = self.whither(reach, method, path, span)?;
        let shown = match &whither {
            Whither::Whole(text) if create && text.ends_with('/') => {
                return unsupported("making a file at a path that ends with `/`");
            }
            Whither::Whole(text) => text.clone(),
            Whither::Beneath(_, inside) if inside.is_empty() => ".".to_owned(),
            Whither::Beneath(_, inside) => inside.join("/"),
        };
        self.disk(span, |disk| {
            let reached = reached(disk, &whither)?;
            if create { disk.create(reached, &shown) } else { disk.open_file(reached) }
        })
    }

    /// Runs a `with` block over a file lent by its handle: when the block ends, a file written
    /// takes its path's place, and one read is closed, and a failure then is the `with`'s. A
    /// block that fails throws a file written away, leaving its path as it was.
    pub(crate) fn with_file(
        &mut self,
        handle: i64,
        names: &'static [WithName],
        span: Span,
        run: impl FnOnce(&mut Interp) -> Run<()>,
    ) -> Run<()> {
        let file = Value::instance("File", None, vec![("_handle".to_owned(), Value::Int(handle))]);
        let ran = self.lend_names(names, file, span).and_then(|()| run(self));
        match ran {
            Ok(()) => self.disk(span, |disk| disk.finish(handle)),
            Err(flow @ (Flow::Return(_) | Flow::Break | Flow::Continue)) => {
                self.disk(span, |disk| disk.finish(handle))?;
                Err(flow)
            }
            Err(other) => {
                self.disk(span, |disk| {
                    disk.discard(handle);
                    Ok(())
                })?;
                Err(other)
            }
        }
    }

    /// A method of a `File` a `with` lent.
    pub(crate) fn file_method(&mut self, file: &Value, name: &str, args: &[Value], span: Span) -> Run<Value> {
        let handle = match file {
            Value::Instance(file) => match file.field("_handle") {
                Some(Value::Int(handle)) => *handle,
                _ => return unsupported("a File without its handle"),
            },
            _ => return unsupported("a File that isn't one"),
        };
        match (name, args) {
            ("write" | "write_text", [Value::Str(text)]) => {
                self.disk(span, |disk| disk.write_to(handle, text))?;
                Ok(Value::Unit)
            }
            ("write_line", [Value::Str(text)]) => {
                self.disk(span, |disk| disk.write_to(handle, text))?;
                self.disk(span, |disk| disk.write_to(handle, "\n"))?;
                Ok(Value::Unit)
            }
            ("flush" | "sync", []) => {
                self.disk(span, |disk| disk.still_open(handle))?;
                Ok(Value::Unit)
            }
            ("read_line", []) => {
                Ok(self.disk(span, |disk| disk.read_line(handle))?.map_or(Value::None, Value::Str))
            }
            ("read_text", []) => self.disk(span, |disk| disk.read_rest(handle)).map(Value::Str),
            ("lines", []) => {
                let lines = FileLines { disk: std::sync::Arc::clone(&self.shared.disk), handle, span };
                let shown = self.program.type_at(span).map(super::calls::shown_type);
                Ok(Interp::stream_of(Source::Lines(lines), shown))
            }
            ("write" | "write_bytes" | "read" | "read_bytes" | "read_chunk" | "chunks", _) => {
                unsupported("bytes")
            }
            _ => unsupported(format!("the File method `{name}`")),
        }
    }
}

/// Where a call reaches, with every folder on the way found.
fn reached(disk: &mut Disk, whither: &Whither) -> Done<Reached> {
    match whither {
        Whither::Whole(text) => disk.whole(text),
        Whither::Beneath(folder, inside) => disk.beneath(folder, inside),
    }
}

/// A list of `Path`s with these texts.
fn paths(texts: Vec<String>) -> Value {
    Value::List(texts.into_iter().map(path_value).collect())
}

/// A `Path` with this text.
pub(crate) fn path_value(text: String) -> Value {
    Value::instance("Path", None, vec![("text".to_owned(), Value::Str(text))])
}

/// The failure for a path that holds a NUL, which no system takes: it's shown with the NUL
/// written as `\0`, and cut short past 80 characters.
fn holding_nul(text: &str, span: Span) -> Run<()> {
    if !text.contains('\0') {
        return Ok(());
    }
    let mut shown: String = text.replace('\0', "\\0").chars().take(80).collect();
    if text.chars().count() > 80 {
        shown.push('…');
    }
    failing("Other", format!("`{shown}` can't be a path: it holds a NUL"), span)
}

/// A path from outside as a message shows it: whole, or past 40 characters its first 20 and `…`,
/// with what isn't printable escaped as Rust's debug form escapes it.
fn cut(text: &str) -> String {
    let kept: String = if text.chars().count() <= 40 {
        text.to_owned()
    } else {
        text.chars().take(20).chain(['…']).collect()
    };
    kept.escape_debug().to_string()
}
