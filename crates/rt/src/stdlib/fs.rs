//! `std.fs`: the methods of the `Fs` capability. Each takes a `Path` or a `str`, and a
//! failure is an `IoError` value rather than a stop.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use fennec_syntax::{Diagnostic, Span};

use crate::methods::no_method;
use crate::value::{Capability, Value};

/// A file `fs.create` or `fs.open` has lent, by the handle its `File` holds. One being written
/// goes into a file of its own beside where it's going, which takes that place when its block
/// ends. A test's fake files are held in memory, and go into its fake file system then.
enum OpenFile {
    Writing { out: BufWriter<std::fs::File>, temporary: PathBuf, target: PathBuf, durable: bool },
    Reading(BufReader<std::fs::File>),
    FakeWriting { target: String, data: Vec<u8> },
    FakeReading { data: Vec<u8>, at: usize },
}

/// Each open file has a lock of its own, so a long write to one never holds up another.
type Shared = Arc<Mutex<OpenFile>>;

static OPEN: LazyLock<Mutex<HashMap<i64, Shared>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT: LazyLock<Mutex<i64>> = LazyLock::new(|| Mutex::new(1));

/// `work`, which may wait on the disk, on one of the runtime's blocking threads when it's in a
/// task, so a slow disk pauses only that task, as a network wait does. Outside a task it runs
/// here, and so does all of it where there are no threads.
pub(crate) fn blocking<R: Send + 'static>(work: impl FnOnce() -> R + Send + 'static) -> R {
    #[cfg(any(test, feature = "threads"))]
    {
        crate::embed::serve::off_thread(work)
    }
    #[cfg(not(any(test, feature = "threads")))]
    {
        work()
    }
}

/// Keeps an open file, giving back the `File` that holds its handle.
fn lent(file: OpenFile) -> Value {
    let mut next = NEXT.lock().unwrap_or_else(PoisonError::into_inner);
    let handle = *next;
    *next += 1;
    OPEN.lock().unwrap_or_else(PoisonError::into_inner).insert(handle, Arc::new(Mutex::new(file)));
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

fn shared(handle: i64) -> Option<Shared> {
    OPEN.lock().unwrap_or_else(PoisonError::into_inner).get(&handle).cloned()
}

/// Takes an open file out of the table, to finish or drop it.
fn taken(file: &Value) -> Option<Shared> {
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

/// Where writing to `path` really puts a file: what a link points at, so the link stays a link.
fn through_links(path: &Path) -> std::io::Result<PathBuf> {
    match std::fs::symlink_metadata(path) {
        Ok(found) if found.file_type().is_symlink() => std::fs::canonicalize(path),
        _ => Ok(path.to_path_buf()),
    }
}

/// A new file beside `target` to write, which nothing else can already be: a name someone else
/// made first, or a link waiting there, is passed over. It has `target`'s permissions before
/// anything's in it, so what's being written is never readable by more than the file was.
fn new_beside(target: &Path) -> std::io::Result<(std::fs::File, PathBuf)> {
    let replaced = std::fs::metadata(target).ok();
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if replaced.is_some() {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut last = std::io::Error::from(std::io::ErrorKind::AlreadyExists);
    for _ in 0..32 {
        let temporary = temporary_beside(target);
        match options.open(&temporary) {
            Ok(file) => {
                if let Some(replaced) = &replaced {
                    file.set_permissions(replaced.permissions())?;
                }
                return Ok((file, temporary));
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => last = err,
            Err(err) => return Err(err),
        }
    }
    Err(last)
}

/// Whether a file can be replaced by renaming another over it: not a device or a pipe, which
/// have to be written to where they are.
fn replaceable(target: &Path) -> bool {
    std::fs::metadata(target).map_or(true, |found| found.is_file())
}

/// `fs.write_text` and `fs.write_bytes`: the whole file takes the old one's place at once, so a
/// reader never sees half of it. Where that can't be done, as for a device or in a folder this
/// program can't add to, it's written where it is.
fn write_whole(path: &str, data: &[u8]) -> std::io::Result<()> {
    let target = through_links(Path::new(path))?;
    if !replaceable(&target) {
        return std::fs::write(target, data);
    }
    let (mut file, temporary) = match new_beside(&target) {
        Ok(made) => made,
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            return std::fs::write(target, data);
        }
        Err(err) => return Err(err),
    };
    let done = file.write_all(data).and_then(|()| std::fs::rename(&temporary, &target));
    if done.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    done
}

/// `fs._create(path, durable)`: a new file beside `path` to write.
fn create(path: &str, durable: bool) -> Value {
    let given = PathBuf::from(path);
    let made = blocking(move || {
        let target = through_links(&given).map_err(Fail::from)?;
        if !replaceable(&target) {
            return Err(Fail::from(format!("`{}` isn't a file that can be replaced", target.display())));
        }
        let (file, temporary) = new_beside(&target).map_err(Fail::from)?;
        Ok((file, temporary, target))
    });
    match made {
        Ok((file, temporary, target)) => {
            let out = BufWriter::new(file);
            Value::Ok(Rc::new(lent(OpenFile::Writing { out, temporary, target, durable })))
        }
        Err(fail) => io_failed(&fail),
    }
}

/// `fs._open(path)`: the file at `path`, to read.
fn open(path: &str) -> Value {
    let path = path.to_owned();
    match blocking(move || std::fs::File::open(path).map_err(Fail::from)) {
        Ok(file) => Value::Ok(Rc::new(lent(OpenFile::Reading(BufReader::new(file))))),
        Err(fail) => io_failed(&fail),
    }
}

/// A file written takes its target's place: everything written is on the disk first, and the
/// rename is too, after, unless it was made with `durable=False`. Anything left is thrown away
/// when that fails, leaving the target as it was.
fn finish_writing(
    out: &mut BufWriter<std::fs::File>,
    temporary: &Path,
    target: &Path,
    durable: bool,
) -> Result<(), String> {
    let done = (|| {
        out.flush()?;
        if durable {
            out.get_ref().sync_all()?;
        }
        std::fs::rename(temporary, target)?;
        // Windows has no handle to a folder to sync, and its renames are written through by
        // the file system.
        #[cfg(unix)]
        if durable {
            sync_folder(target)?;
        }
        Ok::<(), std::io::Error>(())
    })();
    if done.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    done.map_err(|err| err.to_string())
}

/// Puts a rename in `path`'s folder on the disk.
#[cfg(unix)]
fn sync_folder(path: &Path) -> std::io::Result<()> {
    let folder = path.parent().filter(|folder| !folder.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::File::open(folder)?.sync_all()
}

/// What puts a fake file's bytes into a test's fake file system, by its path.
type FakeWrite<'a> = dyn FnMut(&str, &[u8]) -> Result<(), String> + 'a;

/// `fs._finish(file)`: a file written takes its target's place, and one read is closed. A fake
/// one goes into the test's fake file system, by `fake`.
fn finish(file: &Value, fake: &mut FakeWrite<'_>) -> Result<(), String> {
    let Some(file) = taken(file) else { return Ok(()) };
    let fake_written = match &mut *file.lock().unwrap_or_else(PoisonError::into_inner) {
        OpenFile::FakeWriting { target, data } => Some((std::mem::take(target), std::mem::take(data))),
        _ => None,
    };
    if let Some((target, data)) = fake_written {
        return fake(&target, &data);
    }
    blocking(move || match &mut *file.lock().unwrap_or_else(PoisonError::into_inner) {
        OpenFile::Writing { out, temporary, target, durable } => {
            finish_writing(out, temporary, target, *durable)
        }
        _ => Ok(()),
    })
}

/// `fs._discard(file)`: a file written is thrown away, leaving its target as it was.
fn discard(file: &Value) {
    let Some(file) = taken(file) else { return };
    blocking(move || {
        if let OpenFile::Writing { temporary, .. } = &*file.lock().unwrap_or_else(PoisonError::into_inner) {
            let _ = std::fs::remove_file(temporary);
        }
    });
}

/// What a `File`'s method gives back, as plain data that can come back from another thread.
enum Given {
    Nothing,
    Data(Vec<u8>),
    Line(Option<Vec<u8>>),
}

/// A method of a lent `File` by its handle: `write(data)`, `read()`, `read_chunk(size)`,
/// `read_line()` or `sync()`.
pub(crate) fn real_file(handle: i64, method: &str, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
    let Some(file) = shared(handle) else {
        return Ok(io_error("this file was closed when its `with` block ended"));
    };
    let data = match args {
        [data @ (Value::Str(_) | Value::Bytes(_))] => Some(bytes_of(data).to_vec()),
        _ => None,
    };
    let size = match args {
        [Value::Int(size)] => usize::try_from(*size).ok(),
        _ => None,
    };
    let method = match (method, &data, size) {
        ("write", Some(_), _)
        | ("read" | "read_line" | "sync" | "flush", None, None)
        | ("read_chunk", None, Some(_)) => method.to_owned(),
        ("read_chunk", None, None) => {
            return Err(Diagnostic::new("`read_chunk` takes how many bytes to read, 0 or more", span));
        }
        _ => return Err(Diagnostic::new(format!("a File has no method `{method}` that takes these"), span)),
    };
    let done = blocking(move || {
        file_work(&mut file.lock().unwrap_or_else(PoisonError::into_inner), &method, data, size)
    });
    Ok(match done {
        Ok(Given::Nothing) => Value::Ok(Rc::new(Value::Unit)),
        Ok(Given::Data(data)) => Value::Ok(Rc::new(Value::Bytes(Rc::from(data)))),
        Ok(Given::Line(None)) => Value::Ok(Rc::new(Value::None)),
        Ok(Given::Line(Some(line))) => match line_text(line) {
            Ok(text) => Value::Ok(Rc::new(Value::str(&text))),
            Err(reason) => io_error(&reason),
        },
        Err(reason) => io_error(&reason),
    })
}

/// The work of a `File`'s method, which may wait on the disk.
fn file_work(
    file: &mut OpenFile,
    method: &str,
    data: Option<Vec<u8>>,
    size: Option<usize>,
) -> Result<Given, String> {
    let io = |err: std::io::Error| err.to_string();
    match (method, file) {
        ("write", OpenFile::Writing { out, .. }) => {
            out.write_all(&data.unwrap_or_default()).map_err(io)?;
            Ok(Given::Nothing)
        }
        ("write", OpenFile::FakeWriting { data: kept, .. }) => {
            kept.extend_from_slice(&data.unwrap_or_default());
            Ok(Given::Nothing)
        }
        ("sync", OpenFile::Writing { out, .. }) => {
            out.flush().map_err(io)?;
            out.get_ref().sync_all().map_err(io)?;
            Ok(Given::Nothing)
        }
        // What's been written goes to the file now, without waiting for the disk.
        ("flush", OpenFile::Writing { out, .. }) => {
            out.flush().map_err(io)?;
            Ok(Given::Nothing)
        }
        ("sync" | "flush", _) => Ok(Given::Nothing),
        ("write", _) => Err("this file was opened to read, not to write".to_owned()),
        ("read", OpenFile::Reading(reader)) => {
            let mut all = Vec::new();
            reader.read_to_end(&mut all).map_err(io)?;
            Ok(Given::Data(all))
        }
        ("read_chunk", OpenFile::Reading(reader)) => {
            // Up to `size` bytes, growing as they're read, so a `size` far past what the file
            // holds makes no room for what isn't there.
            let mut chunk = Vec::new();
            let limit = u64::try_from(size.unwrap_or(0)).unwrap_or(u64::MAX);
            reader.by_ref().take(limit).read_to_end(&mut chunk).map_err(io)?;
            Ok(Given::Data(chunk))
        }
        ("read_line", OpenFile::Reading(reader)) => {
            let mut line = Vec::new();
            match reader.read_until(b'\n', &mut line).map_err(io)? {
                0 => Ok(Given::Line(None)),
                _ => Ok(Given::Line(Some(line))),
            }
        }
        (reading, OpenFile::FakeReading { data, at }) => {
            let rest = data.get(*at..).unwrap_or_default();
            let end = match reading {
                "read" => rest.len(),
                "read_chunk" => size.unwrap_or(0).min(rest.len()),
                _ if rest.is_empty() => return Ok(Given::Line(None)),
                _ => rest.iter().position(|byte| *byte == b'\n').map_or(rest.len(), |end| end + 1),
            };
            let taken = rest.get(..end).unwrap_or_default().to_vec();
            *at += end;
            Ok(if reading == "read_line" { Given::Line(Some(taken)) } else { Given::Data(taken) })
        }
        _ => Err("this file was made to write, not to read".to_owned()),
    }
}

fn bytes_of(data: &Value) -> &[u8] {
    match data {
        Value::Str(text) => text.as_bytes(),
        Value::Bytes(data) => data,
        _ => &[],
    }
}

/// A line read, without its line break, as text.
fn line_text(mut line: Vec<u8>) -> Result<String, String> {
    if line.last() == Some(&b'\n') {
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
    }
    String::from_utf8(line).map_err(|_| "this line isn't UTF-8 text".to_owned())
}

/// A method of a test's fake `Fs`, on paths as `path_text` gives them, with its files in memory,
/// or nothing if the arguments are wrong.
pub(crate) fn fake_method(
    world: &mut crate::fakes::TestWorld,
    method: &str,
    paths: &[Value],
    span: Span,
) -> Option<Result<Value, Diagnostic>> {
    match (method, paths) {
        ("_create", [Value::Str(path), _]) => {
            let file = OpenFile::FakeWriting { target: path.to_string(), data: Vec::new() };
            Some(Ok(Value::Ok(Rc::new(lent(file)))))
        }
        ("_open", [Value::Str(_)]) => Some(match world.fs("read_bytes", paths, span) {
            Some(Ok(Value::Ok(data))) => match data.as_ref() {
                Value::Bytes(data) => {
                    let file = OpenFile::FakeReading { data: data.to_vec(), at: 0 };
                    Ok(Value::Ok(Rc::new(lent(file))))
                }
                _ => Ok(io_error("the fake file system gave something that isn't bytes")),
            },
            Some(other) => other,
            None => Ok(io_error("there's no fake file system here")),
        }),
        ("_sqlite_open", [Value::Str(path)]) => Some(Ok(world.open_database(path))),
        ("_finish", [file]) => {
            let mut written = Ok(());
            let done = finish(file, &mut |target, data| {
                let given = [Value::str(target), Value::Bytes(Rc::from(data.to_vec()))];
                written = match world.fs("write_bytes", &given, span) {
                    Some(Ok(Value::Ok(_))) => Ok(()),
                    Some(Ok(failed)) => Err(crate::format::repr(&failed)),
                    Some(Err(problem)) => Err(problem.message),
                    None => Err("there's no fake file system here".to_owned()),
                };
                Ok(())
            });
            Some(Ok(match done.and(written) {
                Ok(()) => Value::Ok(Rc::new(Value::Unit)),
                Err(reason) => io_error(&reason),
            }))
        }
        _ => world.fs(method, paths, span),
    }
}

/// A method of the real `Fs`, which needs nothing of a program's, on paths as `path_text` gives
/// them.
pub(crate) fn real_fs(method: &str, paths: &[Value], span: Span) -> Result<Value, Diagnostic> {
    let wrong = || Err(Diagnostic::new(format!("`{method}` was called with the wrong arguments"), span));
    let io = Fail::from;
    Ok(match (method, paths) {
        // Whether there's anything at `path`, and a failure when it can't be looked at, as when
        // a folder on the way can't be read: a `False` then could be wrong.
        ("exists", [Value::Str(path)]) => {
            let path = path.to_string();
            done(blocking(move || Path::new(&path).try_exists().map_err(io)), Value::Bool)
        }
        ("read_text", [Value::Str(path)]) => {
            let path = path.to_string();
            done(blocking(move || std::fs::read_to_string(path).map_err(io)), |text| Value::str(&text))
        }
        ("read_bytes", [Value::Str(path)]) => {
            let path = path.to_string();
            done(blocking(move || std::fs::read(path).map_err(io)), |data| Value::Bytes(Rc::from(data)))
        }
        ("write_text" | "write_bytes", [Value::Str(path), data @ (Value::Str(_) | Value::Bytes(_))]) => {
            let (path, data) = (path.to_string(), bytes_of(data).to_vec());
            done(blocking(move || write_whole(&path, &data).map_err(io)), |()| Value::Unit)
        }
        ("list_dir", [Value::Str(path)]) => {
            let folder = path.to_string();
            let listed = blocking(move || names_in(&folder).map_err(io));
            match listed {
                Ok(names) => paths_in(path, &names),
                Err(fail) => io_failed(&fail),
            }
        }
        ("walk", [Value::Str(path)]) => {
            let folder = path.to_string();
            let walked = blocking(move || {
                let mut found = Vec::new();
                walk_into(&folder, &mut found).map(|()| found).map_err(io)
            });
            match walked {
                Ok(found) => paths_of(&found),
                Err(fail) => io_failed(&fail),
            }
        }
        ("make_dir", [Value::Str(path)]) => {
            let path = path.to_string();
            done(blocking(move || std::fs::create_dir_all(path).map_err(io)), |()| Value::Unit)
        }
        ("remove", [Value::Str(path)]) => {
            let path = path.to_string();
            done(blocking(move || remove(&path).map_err(io)), |()| Value::Unit)
        }
        ("metadata", [Value::Str(path)]) => {
            let path = path.to_string();
            let found = blocking(move || {
                std::fs::metadata(path)
                    .map(|found| (found.len(), found.is_file(), found.is_dir()))
                    .map_err(io)
            });
            done(found, |(size, is_file, is_dir)| {
                Value::made(
                    "Metadata",
                    vec![
                        ("size", Value::Int(i64::try_from(size).unwrap_or(i64::MAX))),
                        ("is_file", Value::Bool(is_file)),
                        ("is_dir", Value::Bool(is_dir)),
                    ],
                )
            })
        }
        ("_create", [Value::Str(path), Value::Bool(durable)]) => create(path, *durable),
        ("_open", [Value::Str(path)]) => open(path),
        ("_sqlite_open", [Value::Str(path)]) => crate::stdlib::sqlite::open_file(path),
        ("_finish", [file]) => done(finish(file, &mut |_, _| Ok(())).map_err(Fail::from), |()| Value::Unit),
        ("_discard", [file]) => {
            discard(file);
            Value::Unit
        }
        (
            "exists" | "read_text" | "write_text" | "read_bytes" | "write_bytes" | "list_dir" | "walk"
            | "make_dir" | "remove" | "metadata" | "_create" | "_open" | "_sqlite_open",
            _,
        ) => return wrong(),
        _ => return Err(no_method(&Value::Capability(Capability::Fs), method, span)),
    })
}

/// A result's value, made into what a program gets back, or its failure as an `IoError`.
fn done<T>(result: Result<T, Fail>, made: impl FnOnce(T) -> Value) -> Value {
    match result {
        Ok(value) => Value::Ok(Rc::new(made(value))),
        Err(fail) => io_failed(&fail),
    }
}

/// A file's name as text. One that isn't text fails: changing it would name another file.
fn name_of(name: std::ffi::OsString, folder: &str) -> std::io::Result<String> {
    name.into_string().map_err(|odd| {
        let shown = odd.to_string_lossy().into_owned();
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("a name in `{folder}` isn't UTF-8: {shown}"),
        )
    })
}

/// The names of what's in a folder, sorted so every system lists them in the same order.
fn names_in(folder: &str) -> std::io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(folder)? {
        names.push(name_of(entry?.file_name(), folder)?);
    }
    names.sort();
    Ok(names)
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
    if folder.is_empty() || folder.ends_with('/') {
        format!("{folder}{name}")
    } else {
        format!("{folder}/{name}")
    }
}

/// Every path under `folder`, onto `found`: each folder's before what's in it, and each folder's
/// things sorted by name, as `list_dir` gives them. A link to a folder is listed but not gone
/// into, so a loop of links can't go on forever.
fn walk_into(folder: &str, found: &mut Vec<String>) -> std::io::Result<()> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(folder)? {
        names.push(name_of(entry?.file_name(), folder)?);
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

/// A failure of the file system, as the case of `IoError` it is and what it says.
#[derive(Debug)]
pub(crate) struct Fail {
    case: &'static str,
    reason: String,
}

impl Fail {
    /// Nothing is at the path.
    pub(crate) fn not_found(reason: String) -> Self {
        Self { case: "NotFound", reason }
    }

    /// The system, or a narrowed `Fs`, won't let the program do that there.
    pub(crate) fn permission_denied(reason: String) -> Self {
        Self { case: "PermissionDenied", reason }
    }
}

/// A failure said only in words is `Other`.
impl From<String> for Fail {
    fn from(reason: String) -> Self {
        Self { case: "Other", reason }
    }
}

/// The system's failure, by its kind: the three a caller may handle differently have cases of
/// their own, and the rest are `Other`.
impl From<std::io::Error> for Fail {
    fn from(err: std::io::Error) -> Self {
        let case = match err.kind() {
            std::io::ErrorKind::NotFound => "NotFound",
            std::io::ErrorKind::PermissionDenied => "PermissionDenied",
            std::io::ErrorKind::AlreadyExists => "AlreadyExists",
            _ => "Other",
        };
        Self { case, reason: err.to_string() }
    }
}

/// A failed result holding the `IoError` a failure is.
pub(crate) fn io_failed(fail: &Fail) -> Value {
    let fields = vec![("reason", Value::str(&fail.reason))];
    Value::Err(Rc::new(Value::made_case("IoError", fail.case, fields)))
}

/// A failed result holding an `IoError` said only in words, which is `Other`.
pub(crate) fn io_error(reason: &str) -> Value {
    io_failed(&Fail::from(reason.to_owned()))
}

/// Removes a file, or a folder if it's empty.
fn remove(path: &str) -> std::io::Result<()> {
    if std::fs::symlink_metadata(path)?.is_dir() {
        std::fs::remove_dir(path)
    } else {
        std::fs::remove_file(path)
    }
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

#[cfg(test)]
mod tests {
    use super::Fail;

    /// The system's failures that a caller may handle differently each have a case of their own,
    /// on the real file system, and the rest are `Other`, each keeping the system's words.
    #[test]
    fn a_systems_failure_is_the_case_its_kind_is() {
        let gone = std::fs::read("/no/such/place/for/fennec").expect_err("it isn't there");
        let fail = Fail::from(gone);
        assert_eq!((fail.case, fail.reason.is_empty()), ("NotFound", false));
        for (kind, case) in [
            (std::io::ErrorKind::PermissionDenied, "PermissionDenied"),
            (std::io::ErrorKind::AlreadyExists, "AlreadyExists"),
            (std::io::ErrorKind::InvalidData, "Other"),
            (std::io::ErrorKind::IsADirectory, "Other"),
        ] {
            let fail = Fail::from(std::io::Error::new(kind, "said"));
            assert_eq!((fail.case, fail.reason.as_str()), (case, "said"));
        }
        assert_eq!(Fail::from("words".to_owned()).case, "Other");
    }
}
