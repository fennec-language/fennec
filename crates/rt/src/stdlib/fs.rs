//! `std.fs`: the methods of the `Fs` capability. Each takes a `Path` or a `str`, and a
//! failure is an `IoError` value rather than a stop.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use fennec_syntax::{Diagnostic, Span};

use crate::methods::no_method;
use crate::stdlib::faults::{self, Work};

mod left;
use crate::stdlib::turns::Held;
use crate::value::{Capability, Value};

/// A file `fs.create` or `fs.open` has lent, by the handle its `File` holds. One being written
/// goes into a file of its own beside where it's going, which takes that place when its block
/// ends. A test's fake files are held in memory, and go into its fake file system then.
enum OpenFile {
    Writing { out: BufWriter<std::fs::File>, at: Written, durable: bool },
    Reading(BufReader<std::fs::File>),
    FakeWriting { target: String, data: Vec<u8> },
    FakeReading { data: Vec<u8>, at: usize },
}

/// Where a file being written is, and where it goes when its block ends: by path, or, for an `Fs`
/// narrowed with `only`, by name in the folder it's in, held by a handle opened beneath the folder
/// it's narrowed to, so nothing swapped in on the way can send it outside.
enum Written {
    Path {
        temporary: PathBuf,
        target: PathBuf,
    },
    /// `temporary` and `target` are names in `folder`, the folder the file is in.
    Beneath {
        folder: Arc<cap_std::fs::Dir>,
        temporary: PathBuf,
        target: PathBuf,
    },
}

impl Written {
    /// Renames the file written into its target's place.
    fn put_in_place(&self) -> std::io::Result<()> {
        let done = match self {
            Written::Path { temporary, target } => std::fs::rename(temporary, target),
            Written::Beneath { folder, temporary, target } => folder.rename(temporary, folder, target),
        };
        if done.is_ok() {
            self.let_go();
        }
        done
    }

    /// Throws the file written away, leaving its target as it was.
    fn throw_away(&self) {
        let _ = match self {
            Written::Path { temporary, .. } => std::fs::remove_file(temporary),
            Written::Beneath { folder, temporary, .. } => folder.remove_file(temporary),
        };
        self.let_go();
    }

    /// The file written is no longer this run's to hold beside its target.
    fn let_go(&self) {
        let (Written::Path { temporary, .. } | Written::Beneath { temporary, .. }) = self;
        if let Some(name) = temporary.file_name() {
            left::let_go(name);
        }
    }

    /// Puts the rename in the target's folder on the disk.
    #[cfg(unix)]
    fn sync_folder(&self) -> std::io::Result<()> {
        match self {
            Written::Path { target, .. } => sync_folder(target),
            Written::Beneath { folder, target, .. } => {
                let parent = target.parent().filter(|parent| !parent.as_os_str().is_empty());
                // Opened to read, beneath the folder as any path is: a handle `open_dir` gives is
                // only for finding names on Linux (`O_PATH`), which `fsync` refuses with EBADF.
                let mut reading = cap_std::fs::OpenOptions::new();
                reading.read(true);
                folder.open_with(parent.unwrap_or(Path::new(".")), &reading)?.into_std().sync_all()
            }
        }
    }
}

/// Each open file has a lock of its own, so a long write to one never holds up another, and a
/// line of the tasks waiting their turn at it, parked.
type Shared = Arc<Held<OpenFile>>;

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
    OPEN.lock().unwrap_or_else(PoisonError::into_inner).insert(handle, Arc::new(Held::new(file)));
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
/// into place never crosses from one disk to another, and hidden, with a name no other has. The
/// name is held as this run's from here, before the file is made, until it's put in place or
/// thrown away, or [`not_made`] says it wasn't made.
fn temporary_beside(target: &Path) -> PathBuf {
    static MADE: LazyLock<Mutex<u64>> = LazyLock::new(|| Mutex::new(0));
    let mut made = MADE.lock().unwrap_or_else(PoisonError::into_inner);
    *made += 1;
    let name = target.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let temporary = target.with_file_name(format!(".{name}.{}.{}.fennec-partial", std::process::id(), *made));
    if let Some(name) = temporary.file_name() {
        left::hold(name);
    }
    temporary
}

/// A file named by [`temporary_beside`] that wasn't made, or was made and taken away again.
fn not_made(temporary: &Path) {
    if let Some(name) = temporary.file_name() {
        left::let_go(name);
    }
}

/// The folder `target` is in.
fn folder_of(target: &Path) -> &Path {
    target.parent().filter(|folder| !folder.as_os_str().is_empty()).unwrap_or(Path::new("."))
}

/// Windows' `FILE_SHARE_DELETE`: a file being written beside its target is opened letting others
/// rename and remove it, as its own rename and removal do, but not open it to read or write. So
/// one that opens to read has no writer left (`left::clear`).
#[cfg(windows)]
const SHARE_DELETE_ONLY: u32 = 0x0000_0004;

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
    faults::check(Work::Create)?;
    let replaced = std::fs::metadata(target).ok();
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if replaced.is_some() {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        options.share_mode(SHARE_DELETE_ONLY);
    }
    let mut last = std::io::Error::from(std::io::ErrorKind::AlreadyExists);
    for _ in 0..32 {
        let temporary = temporary_beside(target);
        match options.open(&temporary) {
            Ok(file) => {
                let permitted =
                    replaced.as_ref().map_or(Ok(()), |replaced| file.set_permissions(replaced.permissions()));
                if let Err(err) = permitted {
                    let _ = std::fs::remove_file(&temporary);
                    not_made(&temporary);
                    return Err(err);
                }
                return Ok((file, temporary));
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                not_made(&temporary);
                last = err;
            }
            Err(err) => {
                not_made(&temporary);
                return Err(err);
            }
        }
    }
    Err(last)
}

/// Whether a file can be replaced by renaming another over it: not a device or a pipe, which
/// have to be written to where they are.
fn replaceable(target: &Path) -> bool {
    std::fs::metadata(target).map_or(true, |found| found.is_file())
}

/// `fs.write_text` and `fs.write_bytes`: the whole file is written beside the old one, put on the
/// disk, and takes the old one's place at once, as `fs.create`'s does, so neither a reader nor a
/// crash ever finds half of it. A device or a pipe, which has no place to take, is written where it
/// is. A file in a folder the program can't add a file to fails with `PermissionDenied`, even
/// where the file itself could be written: written where it is, a full disk would leave half of it.
fn write_whole(path: &str, data: &[u8], durable: bool) -> std::io::Result<()> {
    let target = through_links(Path::new(path))?;
    if !replaceable(&target) {
        return faults::write_all(&mut std::fs::File::create(&target)?, data);
    }
    left::clear(folder_of(&target));
    let (file, temporary) = new_beside(&target)?;
    put_whole(file, &Written::Path { temporary, target }, data, durable)
}

/// What `write_text` and `write_bytes` are given: the path, what's written, and whether to wait
/// for the disk, as `durable=` says after them, which it does unless told `durable=False`.
fn whole_write(args: &[Value]) -> Option<(&str, Vec<u8>, bool)> {
    match args {
        [Value::Str(path), data @ (Value::Str(_) | Value::Bytes(_))] => {
            Some((path, bytes_of(data).to_vec(), true))
        }
        [Value::Str(path), data @ (Value::Str(_) | Value::Bytes(_)), Value::Bool(durable)] => {
            Some((path, bytes_of(data).to_vec(), *durable))
        }
        _ => None,
    }
}

/// Writes `data` into `file`, made beside its target, puts it on the disk, and renames it into the
/// target's place, then puts the rename on the disk too. Anything failing before the rename throws
/// the file away, leaving the target as it was; the rename's own sync failing leaves the new file
/// in place, and fails all the same, since it may not last a power cut. Not `durable`, neither
/// waits for the disk, as `fs.create(path, durable=False)`'s doesn't: a reader still never finds
/// half of it, but a power cut may.
fn put_whole(mut file: std::fs::File, at: &Written, data: &[u8], durable: bool) -> std::io::Result<()> {
    let synced = |file: &std::fs::File| {
        if durable {
            faults::check(Work::Sync)?;
            file.sync_all()?;
        }
        Ok(())
    };
    let done = faults::write_all(&mut file, data)
        .and_then(|()| synced(&file))
        .and_then(|()| faults::check(Work::Rename))
        .and_then(|()| at.put_in_place());
    if done.is_err() {
        at.throw_away();
    }
    done?;
    if !durable {
        return Ok(());
    }
    // Counted on every system, so a rule fails the same work on each.
    faults::check(Work::Sync)?;
    // Windows has no handle to a folder to sync, and its renames are written through by the file
    // system.
    #[cfg(unix)]
    at.sync_folder()?;
    Ok(())
}

/// `fs._create(path, durable)`: a new file beside `path` to write.
fn create(path: &str, durable: bool) -> Value {
    let given = PathBuf::from(path);
    let made = blocking(move || {
        let target = through_links(&given).map_err(Fail::from)?;
        if !replaceable(&target) {
            return Err(Fail::from(format!("`{}` isn't a file that can be replaced", target.display())));
        }
        left::clear(folder_of(&target));
        let (file, temporary) = new_beside(&target).map_err(Fail::from)?;
        Ok((file, temporary, target))
    });
    match made {
        Ok((file, temporary, target)) => {
            let out = BufWriter::new(file);
            let at = Written::Path { temporary, target };
            Value::Ok(Rc::new(lent(OpenFile::Writing { out, at, durable })))
        }
        Err(fail) => io_failed(&fail),
    }
}

/// `fs._open(path)`: the file at `path`, to read.
fn open(path: &str) -> Value {
    let path = path.to_owned();
    let opened = move || {
        let file =
            std::fs::File::open(&path).map_err(|err| read_failure(err, || Path::new(&path).is_dir()))?;
        not_a_folder(file)
    };
    match blocking(opened) {
        Ok(file) => Value::Ok(Rc::new(lent(OpenFile::Reading(BufReader::new(file))))),
        Err(fail) => io_failed(&fail),
    }
}

/// A file written takes its target's place: everything written is on the disk first, and the
/// rename is too, after, unless it was made with `durable=False`. Anything left is thrown away
/// when that fails, leaving the target as it was.
fn finish_writing(out: &mut BufWriter<std::fs::File>, at: &Written, durable: bool) -> Result<(), Fail> {
    let done = (|| {
        faults::check(Work::Flush)?;
        out.flush()?;
        if durable {
            faults::check(Work::Sync)?;
            out.get_ref().sync_all()?;
        }
        faults::check(Work::Rename)?;
        at.put_in_place()?;
        if durable {
            // Counted on every system, so a rule fails the same work on each.
            faults::check(Work::Sync)?;
            // Windows has no handle to a folder to sync, and its renames are written through by
            // the file system.
            #[cfg(unix)]
            at.sync_folder()?;
        }
        Ok::<(), std::io::Error>(())
    })();
    if done.is_err() {
        at.throw_away();
    }
    done.map_err(Fail::from)
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
fn finish(file: &Value, fake: &mut FakeWrite<'_>) -> Result<(), Fail> {
    let Some(file) = taken(file) else { return Ok(()) };
    let fake_written = match &mut *file.lock() {
        OpenFile::FakeWriting { target, data } => Some((std::mem::take(target), std::mem::take(data))),
        _ => None,
    };
    if let Some((target, data)) = fake_written {
        return fake(&target, &data).map_err(Fail::from);
    }
    let Ok(turn) = file.turn() else {
        // Stopped, or cancelled, waiting its turn to finish it: it's failed as stopped, so what was
        // written never takes its target's place, and is thrown away as a failed block's is.
        throw_away(file);
        return Err(Fail::from(STOPPED.to_owned()));
    };
    let held = Arc::clone(&file);
    let done = blocking(move || match &mut *held.lock_for_work() {
        OpenFile::Writing { out, at, durable } => finish_writing(out, at, *durable),
        _ => Ok(()),
    });
    drop(turn);
    done
}

/// Whether a lent file is one being written, which finishing puts in its target's place. One
/// being read is only closed, which an `Fs` that only reads may do.
pub(crate) fn written(file: &Value) -> bool {
    let Some(file) = handle_of(file).and_then(shared) else { return false };
    matches!(&*file.lock(), OpenFile::Writing { .. } | OpenFile::FakeWriting { .. })
}

/// `fs._discard(file)`: a file written is thrown away, leaving its target as it was.
fn discard(file: &Value) {
    let Some(file) = taken(file) else { return };
    // Stopped, or cancelled, waiting its turn, it's thrown away all the same, once the turn before
    // it ends: it's no work of the program's, only what's left of it, and nothing else would.
    let turn = file.turn().ok().flatten();
    throw_away(Arc::clone(&file));
    drop(turn);
}

/// A file being written is thrown away, on a blocking thread, once no one else has it.
fn throw_away(file: Shared) {
    blocking(move || {
        if let OpenFile::Writing { at, .. } = &*file.lock_for_work() {
            at.throw_away();
        }
    });
}

/// Why a file's work wasn't done, for a task stopped, or a round cancelled, while it waited its
/// turn at the file. The task has failed as stopped, so nothing reads it.
const STOPPED: &str = "stopped while it waited its turn at this file";

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
    // The file's turn, waited for parked, so the blocking thread below finds it free. Stopped, or
    // cancelled, while it waits, it's failed as stopped, and doesn't do what it waited to.
    let Ok(turn) = file.turn() else { return Ok(io_error(STOPPED)) };
    let held = Arc::clone(&file);
    let done = blocking(move || file_work(&mut held.lock_for_work(), &method, data, size));
    drop(turn);
    Ok(match done {
        Ok(Given::Nothing) => Value::Ok(Rc::new(Value::Unit)),
        Ok(Given::Data(data)) => Value::Ok(Rc::new(Value::Bytes(Rc::from(data)))),
        Ok(Given::Line(None)) => Value::Ok(Rc::new(Value::None)),
        Ok(Given::Line(Some(line))) => match line_text(line) {
            Ok(text) => Value::Ok(Rc::new(Value::str(&text))),
            Err(reason) => io_error(&reason),
        },
        Err(fail) => io_failed(&fail),
    })
}

/// The work of a `File`'s method, which may wait on the disk.
fn file_work(
    file: &mut OpenFile,
    method: &str,
    data: Option<Vec<u8>>,
    size: Option<usize>,
) -> Result<Given, Fail> {
    // Said in Fennec's words, as a path's failure is, which are the same on every system.
    let io = Fail::from;
    match (method, file) {
        ("write", OpenFile::Writing { out, .. }) => {
            faults::write_all(out, &data.unwrap_or_default()).map_err(io)?;
            Ok(Given::Nothing)
        }
        ("write", OpenFile::FakeWriting { data: kept, .. }) => {
            kept.extend_from_slice(&data.unwrap_or_default());
            Ok(Given::Nothing)
        }
        ("sync", OpenFile::Writing { out, .. }) => {
            faults::check(Work::Flush).and_then(|()| out.flush()).map_err(io)?;
            faults::check(Work::Sync).and_then(|()| out.get_ref().sync_all()).map_err(io)?;
            Ok(Given::Nothing)
        }
        // What's been written goes to the file now, without waiting for the disk.
        ("flush", OpenFile::Writing { out, .. }) => {
            faults::check(Work::Flush).and_then(|()| out.flush()).map_err(io)?;
            Ok(Given::Nothing)
        }
        ("sync" | "flush", _) => Ok(Given::Nothing),
        ("write", _) => Err(Fail::from("this file was opened to read, not to write".to_owned())),
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
        _ => Err(Fail::from("this file was made to write, not to read".to_owned())),
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
        ("_sqlite_open", [Value::Str(path)]) => Some(Ok(world.open_database(path, false))),
        ("_sqlite_read", [Value::Str(path)]) => Some(Ok(world.open_database(path, true))),
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
            Some(Ok(match done.and(written.map_err(Fail::from)) {
                Ok(()) => Value::Ok(Rc::new(Value::Unit)),
                Err(fail) => io_failed(&fail),
            }))
        }
        _ => world.fs(method, paths, span),
    }
}

/// Whether an `Fs` opens databases, and how: the whole one's way, and the way beneath a folder.
/// It's a type the functions here are compiled for, once each way, so the ones for an `Fs` that
/// opens none are compiled with no word of SQLite in them, and a program that opens no database
/// links none of it. Handing the two ways in as functions didn't do that: only one pair is ever
/// handed in, so the compiler called them by name in the one body both callers shared.
pub(crate) trait Databases {
    /// What `method`, `_sqlite_open` or `_sqlite_read`, gives for the database at `path`. The
    /// second is through an `Fs` narrowed with `read_only`: for reading only, and never made.
    fn whole(method: &str, path: &str, span: Span) -> Result<Value, Diagnostic>;
    /// The same for a database at `at` beneath `folder`, whose whole path is `full`.
    fn beneath(
        method: &str,
        place: (Arc<cap_std::fs::Dir>, PathBuf, &str),
        span: Span,
    ) -> Result<Value, Diagnostic>;
}

/// Every method of an `Fs`, opening a database among them.
pub(crate) struct WithDatabases;

/// The file system's own methods: all but the two that open a database.
pub(crate) struct NoDatabases;

impl Databases for WithDatabases {
    fn whole(method: &str, path: &str, _: Span) -> Result<Value, Diagnostic> {
        Ok(crate::stdlib::sqlite::open_file(path, method == "_sqlite_read"))
    }

    fn beneath(
        method: &str,
        (folder, at, full): (Arc<cap_std::fs::Dir>, PathBuf, &str),
        _: Span,
    ) -> Result<Value, Diagnostic> {
        Ok(sqlite_beneath(folder, at, full, method == "_sqlite_read"))
    }
}

/// A method that opens a database, asked of a call that opens none: the call was for the file
/// system's own methods, and compiled code picked the wrong one.
impl Databases for NoDatabases {
    fn whole(method: &str, _: &str, span: Span) -> Result<Value, Diagnostic> {
        Err(no_databases(method, span))
    }

    fn beneath(
        method: &str,
        _: (Arc<cap_std::fs::Dir>, PathBuf, &str),
        span: Span,
    ) -> Result<Value, Diagnostic> {
        Err(no_databases(method, span))
    }
}

fn no_databases(method: &str, span: Span) -> Diagnostic {
    Diagnostic::new(format!("`{method}` opens a database, which this call into the runtime isn't for"), span)
}

/// A method of the real `Fs`, which needs nothing of a program's, on paths as `path_text` gives
/// them.
pub(crate) fn real_fs(method: &str, paths: &[Value], span: Span) -> Result<Value, Diagnostic> {
    real_fs_with::<WithDatabases>(method, paths, span)
}

/// `real_fs` for the file system's own methods: all but the two that open a database.
pub(crate) fn real_files(method: &str, paths: &[Value], span: Span) -> Result<Value, Diagnostic> {
    real_fs_with::<NoDatabases>(method, paths, span)
}

fn real_fs_with<D: Databases>(method: &str, paths: &[Value], span: Span) -> Result<Value, Diagnostic> {
    if let Some(refused) = holding_nul(paths) {
        return Ok(refused);
    }
    on_real_fs::<D>(method, paths, span)
}

/// `real_fs` for a path it's checked.
fn on_real_fs<D: Databases>(method: &str, paths: &[Value], span: Span) -> Result<Value, Diagnostic> {
    let wrong = || Err(Diagnostic::new(format!("`{method}` was called with the wrong arguments"), span));
    let io = Fail::from;
    Ok(match (method, paths) {
        // Whether there's anything at `path`, and a failure when it can't be looked at, as when
        // a folder on the way can't be read: a `False` then could be wrong.
        ("exists", [Value::Str(path)]) => {
            let path = path.to_string();
            done(blocking(move || Path::new(&path).try_exists().map_err(io)), Value::Bool)
        }
        ("read_text" | "read_bytes", [Value::Str(path)]) => {
            let path = path.to_string();
            read_whole(method == "read_text", move || {
                std::fs::read(&path).map_err(|err| read_failure(err, || Path::new(&path).is_dir()))
            })
        }
        ("write_text" | "write_bytes", _) if let Some((path, data, durable)) = whole_write(paths) => {
            let path = path.to_owned();
            done(blocking(move || write_whole(&path, &data, durable).map_err(io)), |()| Value::Unit)
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
        ("_sqlite_open" | "_sqlite_read", [Value::Str(path)]) => D::whole(method, path, span)?,
        ("_finish", [file]) => done(finish(file, &mut |_, _| Ok(())), |()| Value::Unit),
        ("_discard", [file]) => {
            discard(file);
            Value::Unit
        }
        (
            "exists" | "read_text" | "write_text" | "read_bytes" | "write_bytes" | "list_dir" | "walk"
            | "make_dir" | "remove" | "metadata" | "_create" | "_open" | "_sqlite_open" | "_sqlite_read",
            _,
        ) => return wrong(),
        _ => return Err(no_method(&Value::Capability(Capability::Fs), method, span)),
    })
}

/// A method of an `Fs` narrowed with `only` to `folder`, on the real file system, for a path
/// `relative` to that folder, whose whole form is `full`. Everything it does goes through a
/// handle held on the folder, by names beneath it, so a folder or a link swapped in partway down
/// the path between the narrowed `Fs`'s check and the work can't lead outside: the system refuses
/// any step out. What's given back reads as `real_fs` gives it. It opens databases or not as `D`
/// says.
pub(crate) fn beneath_with<D: Databases>(
    folder: &str,
    (full, relative): (&str, &Path),
    (method, paths): (&str, &[Value]),
    span: Span,
) -> Result<Value, Diagnostic> {
    if let Some(refused) = holding_nul(paths) {
        return Ok(refused);
    }
    let root = PathBuf::from(folder);
    let folder = match open_narrowed(folder) {
        Ok(folder) => Arc::new(folder),
        Err(fail) => return Ok(io_failed(&fail)),
    };
    let io = Fail::from;
    let at = relative.to_path_buf();
    Ok(match (method, paths) {
        ("exists", [Value::Str(_)]) => {
            done(blocking(move || exists_beneath(&folder, &at).map_err(io)), Value::Bool)
        }
        ("read_text" | "read_bytes", [Value::Str(_)]) => read_whole(method == "read_text", move || {
            read_beneath(&folder, &at).map_err(|err| read_failure(err, || is_folder_beneath(&folder, &at)))
        }),
        ("write_text" | "write_bytes", _) if let Some((_, data, durable)) = whole_write(paths) => {
            let written = move || write_whole_beneath(&root, &folder, &at, &data, durable).map_err(io);
            done(blocking(written), |()| Value::Unit)
        }
        ("list_dir", [Value::Str(path)]) => {
            let shown = path.to_string();
            match blocking(move || names_beneath(&folder, &at, &shown).map_err(io)) {
                Ok(names) => paths_in(path, &names),
                Err(fail) => io_failed(&fail),
            }
        }
        ("walk", [Value::Str(path)]) => {
            let shown = path.to_string();
            let walked = blocking(move || {
                let mut found = Vec::new();
                walk_beneath(&folder, &at, &shown, &mut found).map(|()| found).map_err(io)
            });
            match walked {
                Ok(found) => paths_of(&found),
                Err(fail) => io_failed(&fail),
            }
        }
        ("make_dir", [Value::Str(_)]) => {
            done(blocking(move || folder.create_dir_all(&at).map_err(io)), |()| Value::Unit)
        }
        ("remove", [Value::Str(_)]) => {
            done(blocking(move || remove_beneath(&folder, &at).map_err(io)), |()| Value::Unit)
        }
        ("metadata", [Value::Str(_)]) => {
            let found = blocking(move || metadata_beneath(&folder, &at).map_err(io));
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
        ("_create", [Value::Str(_), Value::Bool(durable)]) => create_beneath(root, folder, at, *durable),
        ("_open", [Value::Str(_)]) => {
            let opened = move || {
                let file = open_beneath(&folder, &at)
                    .map_err(|err| read_failure(err, || is_folder_beneath(&folder, &at)))?;
                not_a_folder(file)
            };
            match blocking(opened) {
                Ok(file) => Value::Ok(Rc::new(lent(OpenFile::Reading(BufReader::new(file))))),
                Err(fail) => io_failed(&fail),
            }
        }
        ("_sqlite_open" | "_sqlite_read", [Value::Str(_)]) => D::beneath(method, (folder, at, full), span)?,
        // Wrong arguments, and a method that isn't the file system's, say so as the whole `Fs` does.
        _ => return real_fs_with::<D>(method, paths, span),
    })
}

/// Whether there's anything at `at` beneath `folder`, in one call where it can be (`one_call`).
fn exists_beneath(folder: &cap_std::fs::Dir, at: &Path) -> std::io::Result<bool> {
    match one_call::look(folder, at) {
        Some(Ok(_)) => Ok(true),
        Some(Err(err)) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Some(Err(err)) => Err(err),
        None => folder.try_exists(at),
    }
}

/// The bytes of the file at `at` beneath `folder`, opened in one call where it can be.
fn read_beneath(folder: &cap_std::fs::Dir, at: &Path) -> std::io::Result<Vec<u8>> {
    match one_call::open(folder, at) {
        Some(opened) => opened.and_then(|mut file| {
            let mut data = Vec::new();
            file.read_to_end(&mut data).map(|_| data)
        }),
        None => folder.read(at),
    }
}

/// The size of what's at `at` beneath `folder`, and whether it's a file or a folder, looked at in
/// one call where it can be.
fn metadata_beneath(folder: &cap_std::fs::Dir, at: &Path) -> std::io::Result<(u64, bool, bool)> {
    match one_call::look(folder, at) {
        Some(found) => found.map(|found| (found.size, found.is_file, found.is_dir)),
        None => folder.metadata(at).map(|found| (found.len(), found.is_file(), found.is_dir())),
    }
}

/// The file at `at` beneath `folder`, opened to read in one call where it can be.
fn open_beneath(folder: &cap_std::fs::Dir, at: &Path) -> std::io::Result<std::fs::File> {
    one_call::open(folder, at).unwrap_or_else(|| folder.open(at).map(cap_std::fs::File::into_std))
}

/// One system call beneath a narrowed `Fs`'s folder, on macOS, in place of cap-std's walk of a path a
/// folder at a time, which there costs an `openat` and a `close` a folder.
///
/// - The kernel's `O_RESOLVE_BENEATH` refuses a lookup that leaves the folder's handle, and
///   `O_NOFOLLOW_ANY` one that meets a link anywhere on the path.
/// - Only both together, on a path with no `..`, keep a lookup beneath: `O_RESOLVE_BENEATH` alone
///   was raced out through a link to `..` while a folder was moved out mid-lookup (Fable 5.2's
///   review of the design, finding 1). With no link followed and no `..` written, nothing can send
///   a lookup up.
/// - A path that meets a link takes cap-std's walk, which follows a link only while it stays
///   beneath, so links answer as they did. So do a path the kernel finds too long for one call, and
///   a name starting `..`, as a resource fork's `..namedfork` does, which the walk refuses.
/// - Neither `libc` nor `rustix` names the beneath flags for Apple yet, so their values are the
///   SDK's. Both flags are probed once a process before they're trusted, for `openat` and `fstatat`
///   alike: a system that ignored them would follow the links and `..` they refuse.
#[cfg(target_os = "macos")]
mod one_call {
    use std::io;
    use std::path::{Component, Path};
    use std::sync::OnceLock;

    use rustix::fs::{AtFlags, FileType, Mode, OFlags};
    use rustix::io::Errno;

    /// `O_RESOLVE_BENEATH`, `AT_RESOLVE_BENEATH` and `AT_SYMLINK_NOFOLLOW_ANY`, from the macOS
    /// SDK's `sys/fcntl.h`.
    const O_RESOLVE_BENEATH: u32 = 0x1000;
    const AT_RESOLVE_BENEATH: u32 = 0x2000;
    const AT_SYMLINK_NOFOLLOW_ANY: u32 = 0x0800;
    /// `ENOTCAPABLE`: a lookup that would have left the folder.
    const ENOTCAPABLE: i32 = 107;

    #[cfg(test)]
    thread_local! {
        /// For tests: every call on this thread takes the walk, to compare its answers.
        pub(in crate::stdlib::fs) static WALK_ONLY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// What a found file is, as `metadata` says it.
    pub(super) struct Found {
        pub(super) size: u64,
        pub(super) is_file: bool,
        pub(super) is_dir: bool,
    }

    /// `at` opened beneath `folder` to read, in one call, or `None` to take the walk.
    pub(super) fn open(folder: &cap_std::fs::Dir, at: &Path) -> Option<io::Result<std::fs::File>> {
        if !plain(at) || !holds() {
            return None;
        }
        answered(rustix::fs::openat(folder, at, beneath(OFlags::empty()), Mode::empty()))
            .map(|opened| opened.map(std::fs::File::from))
    }

    /// The folder at `at` beneath `folder`, opened in one call, or `None` to take the walk.
    pub(super) fn open_folder(folder: &cap_std::fs::Dir, at: &Path) -> Option<io::Result<cap_std::fs::Dir>> {
        if !plain(at) || !holds() {
            return None;
        }
        answered(rustix::fs::openat(folder, at, beneath(OFlags::DIRECTORY), Mode::empty()))
            .map(|opened| opened.map(|opened| cap_std::fs::Dir::from_std_file(std::fs::File::from(opened))))
    }

    /// What's at `at` beneath `folder`, looked at in one call, or `None` to take the walk, as for a
    /// link at the end, which this call describes where the walk follows it.
    pub(super) fn look(folder: &cap_std::fs::Dir, at: &Path) -> Option<io::Result<Found>> {
        if !plain(at) || !holds() {
            return None;
        }
        let flags = AtFlags::from_bits_retain(AT_RESOLVE_BENEATH | AT_SYMLINK_NOFOLLOW_ANY);
        match answered(rustix::fs::statat(folder, at, flags))? {
            Ok(stat) => {
                let kind = FileType::from_raw_mode(stat.st_mode);
                if kind == FileType::Symlink {
                    return None;
                }
                let size = u64::try_from(stat.st_size).unwrap_or(0);
                Some(Ok(Found {
                    size,
                    is_file: kind == FileType::RegularFile,
                    is_dir: kind == FileType::Directory,
                }))
            }
            Err(err) => Some(Err(err)),
        }
    }

    /// The flags for opening beneath a folder, to read, following no link.
    fn beneath(more: OFlags) -> OFlags {
        OFlags::RDONLY
            | OFlags::CLOEXEC
            | OFlags::NOFOLLOW_ANY
            | OFlags::from_bits_retain(O_RESOLVE_BENEATH)
            | more
    }

    /// Whether `at` can be looked up in one call: names only, none starting `..`, so neither `..`
    /// itself nor a resource fork's `..namedfork`.
    fn plain(at: &Path) -> bool {
        !at.as_os_str().is_empty()
            && at.components().all(|part| match part {
                Component::Normal(name) => !name.as_encoded_bytes().starts_with(b".."),
                _ => false,
            })
    }

    /// What one call came to: `None` to take the walk, for a link on the way or a path too long for
    /// one call, and otherwise what the walk would give. A plain path can't lead out, so a kernel
    /// saying one did leaves it to the walk to say so in its own words.
    fn answered<T>(result: rustix::io::Result<T>) -> Option<io::Result<T>> {
        match result {
            Ok(found) => Some(Ok(found)),
            Err(err)
                if err == Errno::LOOP || err == Errno::NAMETOOLONG || err.raw_os_error() == ENOTCAPABLE =>
            {
                None
            }
            Err(err) => Some(Err(err.into())),
        }
    }

    /// Whether the kernel honours both flags, for `openat` and `fstatat`: probed once a process,
    /// opening two folders to look from and making nothing.
    fn holds() -> bool {
        static HOLDS: OnceLock<bool> = OnceLock::new();
        #[cfg(test)]
        if WALK_ONLY.get() {
            return false;
        }
        *HOLDS.get_or_init(probe)
    }

    /// `..` beneath `/usr` has to be refused as leaving it, and `/etc`, a link on every Mac, as a
    /// link: by `openat` and by `fstatat`. Anything else, an older system that doesn't know the
    /// flags included, and every call takes the walk.
    fn probe() -> bool {
        let folder = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
        let (Ok(usr), Ok(root)) =
            (rustix::fs::open("/usr", folder, Mode::empty()), rustix::fs::open("/", folder, Mode::empty()))
        else {
            return false;
        };
        let left = |err: Option<Errno>| err.is_some_and(|err| err.raw_os_error() == ENOTCAPABLE);
        let up_opened = left(
            rustix::fs::openat(
                &usr,
                "..",
                folder | OFlags::from_bits_retain(O_RESOLVE_BENEATH),
                Mode::empty(),
            )
            .err(),
        );
        let up_looked =
            left(rustix::fs::statat(&usr, "..", AtFlags::from_bits_retain(AT_RESOLVE_BENEATH)).err());
        let link_opened = rustix::fs::openat(&root, "etc", folder | OFlags::NOFOLLOW_ANY, Mode::empty())
            .err()
            == Some(Errno::LOOP);
        let link_looked =
            rustix::fs::statat(&root, "etc/hosts", AtFlags::from_bits_retain(AT_SYMLINK_NOFOLLOW_ANY)).err()
                == Some(Errno::LOOP);
        up_opened && up_looked && link_opened && link_looked
    }
}

/// For tests: every narrowed `Fs` call on this thread takes cap-std's walk, or goes back to one call
/// where it can.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn walk_only(only: bool) {
    one_call::WALK_ONLY.set(only);
}

/// Elsewhere, every call takes cap-std's walk: Linux's cap-std already opens beneath in one
/// `openat2`, and Windows has no such call.
#[cfg(not(target_os = "macos"))]
mod one_call {
    use std::io;
    use std::path::Path;

    pub(super) struct Found {
        pub(super) size: u64,
        pub(super) is_file: bool,
        pub(super) is_dir: bool,
    }

    pub(super) fn open(_: &cap_std::fs::Dir, _: &Path) -> Option<io::Result<std::fs::File>> {
        None
    }

    pub(super) fn open_folder(_: &cap_std::fs::Dir, _: &Path) -> Option<io::Result<cap_std::fs::Dir>> {
        None
    }

    pub(super) fn look(_: &cap_std::fs::Dir, _: &Path) -> Option<io::Result<Found>> {
        None
    }
}

/// The folder a narrowed `Fs` keeps as `kept` (see `limits::folder_parts`), opened: the folder it
/// was narrowed to from the whole `Fs`, by its path, and for one narrowed again since, the path
/// beneath that, a folder at a time, following no link. A folder on that path swapped for a link
/// since the `Fs` was narrowed is refused, wherever the link points. Only the holder of the whole
/// `Fs` chose the first folder's path, so that one is opened as its path is now.
fn open_narrowed(kept: &str) -> Result<cap_std::fs::Dir, Fail> {
    let (_, outer, below) = crate::limits::folder_parts(kept);
    let outer = cap_std::fs::Dir::open_ambient_dir(outer, cap_std::ambient_authority())?;
    // The way down in one call where it can be: it follows no link either. Anything that call
    // can't answer, a link on the way included, goes a folder at a time, which says which.
    if let Some(Ok(here)) = below.and_then(|below| one_call::open_folder(&outer, Path::new(below))) {
        return Ok(here);
    }
    let mut here = outer.into_std_file();
    for name in below.into_iter().flat_map(|below| below.split('/')).filter(|name| !name.is_empty()) {
        here = match cap_primitives::fs::open_dir_nofollow(&here, Path::new(name)) {
            Ok(next) => next,
            Err(err) => {
                let found = cap_std::fs::Dir::from_std_file(here).symlink_metadata(name);
                if found.is_ok_and(|found| found.file_type().is_symlink()) {
                    let whole = crate::limits::whole_folder(kept);
                    return Err(Fail::permission_denied(format!(
                        "a folder on the way to `{whole}` is a link now, and this Fs follows no link to its folder"
                    )));
                }
                return Err(Fail::from(err));
            }
        };
    }
    Ok(cap_std::fs::Dir::from_std_file(here))
}

/// The real path of `rest`, a path beneath the folder a narrowed `Fs` keeps as `kept`, found from
/// that folder's handle, as `open_narrowed` opens it: a link on the way is followed only while it
/// stays beneath the folder, and what isn't there yet is kept as written. Its names are joined by
/// `/` on every system, each kept whole. `None` when the folder can't be opened, the path leads out
/// of it, or a name on it isn't UTF-8, which `only` then reaches nothing for.
pub(crate) fn real_beneath(kept: &str, rest: &Path) -> Option<String> {
    let folder = open_narrowed(kept).ok()?;
    let mut found = rest;
    let mut missing = Vec::new();
    let real = loop {
        if found.as_os_str().is_empty() {
            break PathBuf::new();
        }
        match folder.canonicalize(found) {
            Ok(real) => break real,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                missing.push(found.file_name()?);
                found = found.parent()?;
            }
            Err(_) => return None,
        }
    };
    let mut names = Vec::new();
    for part in real.components() {
        match part {
            std::path::Component::Normal(name) => names.push(name.to_str()?),
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    for name in missing.into_iter().rev() {
        names.push(name.to_str()?);
    }
    Some(names.join("/"))
}

/// Where writing to `at` beneath `folder` puts a file, for each step of writing it to be done by
/// name in the folder it's in.
struct WrittenAt {
    /// The folder the file is in, opened beneath `folder` once, or `folder` itself.
    here: Arc<cap_std::fs::Dir>,
    /// The file's name in `here`.
    name: PathBuf,
    /// Its path from `folder`, for a message.
    target: PathBuf,
    /// What's there now, if anything can be found.
    found: Option<cap_std::fs::Metadata>,
}

/// Where writing to `at` beneath `folder` puts a file: what a link there points at, when that's
/// beneath the folder too, so the link stays a link. One pointing out is refused.
///
/// The folder the file is in is found once, and every step of writing it, looking at what's there,
/// making a file beside it and renaming that into its place, is then done by name in that folder.
/// Each step found the folder from `folder` again before, opening every folder on the way, which
/// on macOS is a system call each. One folder for every step also keeps them together: a folder on
/// the way swapped partway can't split a write between two folders.
fn written_at(folder: &Arc<cap_std::fs::Dir>, at: &Path) -> std::io::Result<WrittenAt> {
    let mut place = in_its_folder(folder, at)?;
    let found = place.here.symlink_metadata(&place.name);
    if found.as_ref().is_ok_and(|found| found.file_type().is_symlink()) {
        let target = folder.canonicalize(at)?;
        place = in_its_folder(folder, &target)?;
        place.found = place.here.metadata(&place.name).ok();
        return Ok(place);
    }
    place.found = found.ok();
    Ok(place)
}

/// The folder `target`, a path beneath `folder`, is in, opened beneath it, with the name `target`
/// has there, and nothing yet found there. A path with no name of its own, as `.`, is left whole,
/// in `folder`.
fn in_its_folder(folder: &Arc<cap_std::fs::Dir>, target: &Path) -> std::io::Result<WrittenAt> {
    let parent = target.parent().filter(|parent| !parent.as_os_str().is_empty());
    let (here, name) = match (target.file_name(), parent) {
        (Some(name), Some(parent)) => {
            let here = one_call::open_folder(folder, parent).unwrap_or_else(|| folder.open_dir(parent))?;
            (Arc::new(here), PathBuf::from(name))
        }
        (Some(name), None) => (Arc::clone(folder), PathBuf::from(name)),
        (None, _) => (Arc::clone(folder), target.to_path_buf()),
    };
    Ok(WrittenAt { here, name, target: target.to_path_buf(), found: None })
}

impl WrittenAt {
    /// [`replaceable`]: whether what's there can be replaced by renaming another file over it.
    fn replaceable(&self) -> bool {
        self.found.as_ref().is_none_or(cap_std::fs::Metadata::is_file)
    }

    /// [`new_beside`], in the file's folder: a new file beside it, with its permissions.
    fn new_beside(&self) -> std::io::Result<(std::fs::File, PathBuf)> {
        faults::check(Work::Create)?;
        let mut options = cap_std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        if self.found.is_some() {
            use cap_std::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        #[cfg(windows)]
        {
            use cap_std::fs::OpenOptionsExt as _;
            options.share_mode(SHARE_DELETE_ONLY);
        }
        let mut last = std::io::Error::from(std::io::ErrorKind::AlreadyExists);
        for _ in 0..32 {
            let temporary = temporary_beside(&self.name);
            match self.here.open_with(&temporary, &options) {
                Ok(file) => {
                    let file = file.into_std();
                    let permitted =
                        self.found.as_ref().map_or(Ok(()), |replaced| permissions_like(&file, replaced));
                    if let Err(err) = permitted {
                        let _ = self.here.remove_file(&temporary);
                        not_made(&temporary);
                        return Err(err);
                    }
                    return Ok((file, temporary));
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    not_made(&temporary);
                    last = err;
                }
                Err(err) => {
                    not_made(&temporary);
                    return Err(err);
                }
            }
        }
        Err(last)
    }
}

/// Gives `file` the permissions of the file it's to replace.
fn permissions_like(file: &std::fs::File, replaced: &cap_std::fs::Metadata) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use cap_std::fs::PermissionsExt as _;
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(std::fs::Permissions::from_mode(replaced.permissions().mode()))
    }
    #[cfg(not(unix))]
    {
        let mut permissions = file.metadata()?.permissions();
        permissions.set_readonly(replaced.permissions().readonly());
        file.set_permissions(permissions)
    }
}

/// [`write_whole`], beneath `folder`, an `Fs` narrowed with `only` to `root`.
fn write_whole_beneath(
    root: &Path,
    folder: &Arc<cap_std::fs::Dir>,
    at: &Path,
    data: &[u8],
    durable: bool,
) -> std::io::Result<()> {
    let place = written_at(folder, at)?;
    if !place.replaceable() {
        return faults::write_all(&mut place.here.create(&place.name)?, data);
    }
    left::clear_beneath(&place.here, root.join(folder_of(&place.target)));
    let (file, temporary) = place.new_beside()?;
    let written = Written::Beneath { folder: Arc::clone(&place.here), temporary, target: place.name.clone() };
    put_whole(file, &written, data, durable)
}

/// [`create`], beneath `folder`: a new file beside `at` to write, which takes its place by name
/// beneath the folder when its block ends.
fn create_beneath(root: PathBuf, folder: Arc<cap_std::fs::Dir>, at: PathBuf, durable: bool) -> Value {
    let made = blocking(move || {
        let place = written_at(&folder, &at).map_err(Fail::from)?;
        if !place.replaceable() {
            return Err(Fail::from(format!(
                "`{}` isn't a file that can be replaced",
                place.target.display()
            )));
        }
        left::clear_beneath(&place.here, root.join(folder_of(&place.target)));
        let (file, temporary) = place.new_beside().map_err(Fail::from)?;
        Ok((file, Written::Beneath { folder: place.here, temporary, target: place.name }))
    });
    match made {
        Ok((file, at)) => {
            Value::Ok(Rc::new(lent(OpenFile::Writing { out: BufWriter::new(file), at, durable })))
        }
        Err(fail) => io_failed(&fail),
    }
}

/// [`names_in`], beneath `folder`, with `shown` the folder's path as the program gave it.
fn names_beneath(folder: &cap_std::fs::Dir, at: &Path, shown: &str) -> std::io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in folder.read_dir(at)? {
        names.push(name_of(entry?.file_name(), shown)?);
    }
    names.sort();
    Ok(names)
}

/// [`walk_into`], beneath `folder`: every path under `at`, as the program would write it, each
/// folder's before what's in it, and a link to a folder listed but not gone into.
fn walk_beneath(
    folder: &cap_std::fs::Dir,
    at: &Path,
    shown: &str,
    found: &mut Vec<String>,
) -> std::io::Result<()> {
    for name in names_beneath(folder, at, shown)? {
        let path = joined(shown, &name);
        let inside = at.join(&name);
        let is_folder = folder.symlink_metadata(&inside)?.is_dir();
        found.push(path.clone());
        if is_folder {
            walk_beneath(folder, &inside, &path, found)?;
        }
    }
    Ok(())
}

/// [`remove`], beneath `folder`.
fn remove_beneath(folder: &cap_std::fs::Dir, at: &Path) -> std::io::Result<()> {
    if folder.symlink_metadata(at)?.is_dir() { folder.remove_dir(at) } else { folder.remove_file(at) }
}

/// `fs._sqlite_open` beneath `folder`. SQLite opens its file by path, and can't be handed a
/// folder's handle, so the file is opened, or made, beneath the folder first and what it is noted.
/// SQLite then opens it by the path with every link resolved, never making a file and refusing one
/// with a link in it, and the database is refused unless SQLite's file is still that one, as the
/// path, and on Unix SQLite itself, say (see `sqlite::open_checked`). Its `-wal` and `-shm` files
/// beside it are opened by SQLite by path, without following a link at the last step. Closing the
/// window between the looks needs SQLite handed the open file, through a VFS of Fennec's own.
fn sqlite_beneath(folder: Arc<cap_std::fs::Dir>, at: PathBuf, full: &str, read_only: bool) -> Value {
    let full = full.to_owned();
    #[cfg(unix)]
    let noted = blocking(move || {
        use std::os::unix::fs::MetadataExt as _;
        let mut options = cap_std::fs::OpenOptions::new();
        // Read only, it's opened as it is, and one that isn't there isn't made.
        options.read(true).write(!read_only).create(!read_only);
        let found = folder.open_with(&at, &options)?.into_std().metadata()?;
        // With every link resolved, so SQLite can be told to refuse one.
        let resolved = std::fs::canonicalize(&full)?.to_string_lossy().into_owned();
        Ok::<_, std::io::Error>((resolved, (found.dev(), found.ino())))
    });
    #[cfg(windows)]
    let noted = blocking(move || {
        let mut options = cap_std::fs::OpenOptions::new();
        // Read only, it's opened as it is, and one that isn't there isn't made.
        options.read(true).write(!read_only).create(!read_only);
        let found = same_file::Handle::from_file(folder.open_with(&at, &options)?.into_std())?;
        Ok::<_, std::io::Error>((full, Arc::new(found)))
    });
    #[cfg(any(unix, windows))]
    match noted {
        Ok((path, file)) => crate::stdlib::sqlite::open_checked(&path, file, read_only),
        Err(err) => io_failed(&Fail::from(err)),
    }
    // Elsewhere, there's no SQLite.
    #[cfg(not(any(unix, windows)))]
    {
        drop((folder, at));
        crate::stdlib::sqlite::open_file(&full, read_only)
    }
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
        // A failure made with words of its own, like a library's or the runtime's, keeps them. The
        // system's are said in Fennec's words, which are the same on every system, where its own,
        // like "No such file or directory (os error 2)", differ from one to the next.
        let reason = if err.get_ref().is_some() { err.to_string() } else { in_own_words(err.kind()) };
        // cap-std's words for a link that leads out of a narrowed `Fs`'s folder, in this `Fs`'s.
        let reason = if reason == "a path led outside of the filesystem" {
            "a link on the path leads outside what this Fs can reach".to_owned()
        } else {
            reason
        };
        Self { case, reason }
    }
}

/// The failure for a path that holds a NUL, if the path, the first of `args`, does: the rest are
/// what's written, which may hold any character. The system can't take a NUL in a path, and says
/// only that its input was bad; C code given the path would stop at it, so the file it reached
/// would be another than the one named.
pub(crate) fn holding_nul(args: &[Value]) -> Option<Value> {
    args.first().and_then(|path| match path {
        Value::Str(text) if text.contains('\0') => {
            // Shown with the NUL written as `\0`, and cut short, as a path from outside may be long.
            let mut shown: String = text.replace('\0', "\\0").chars().take(80).collect();
            if text.chars().count() > 80 {
                shown.push('…');
            }
            Some(io_failed(&Fail {
                case: "Other",
                reason: format!("`{shown}` can't be a path: it holds a NUL"),
            }))
        }
        _ => None,
    })
}

/// A failure of the system's, in Fennec's words.
fn in_own_words(kind: std::io::ErrorKind) -> String {
    use std::io::ErrorKind;
    match kind {
        ErrorKind::NotFound => "nothing is there",
        ErrorKind::PermissionDenied => "the system won't allow it",
        ErrorKind::AlreadyExists => "something is there already",
        ErrorKind::IsADirectory => "it's a folder, not a file",
        ErrorKind::NotADirectory => "part of the path is a file, not a folder",
        ErrorKind::DirectoryNotEmpty => "the folder isn't empty",
        ErrorKind::InvalidData => "the file isn't UTF-8 text",
        ErrorKind::ReadOnlyFilesystem => "the disk can't be written to",
        ErrorKind::StorageFull => "the disk is full",
        ErrorKind::QuotaExceeded => "the disk space allowed is used up",
        ErrorKind::FileTooLarge => "the file is too big for the system",
        ErrorKind::InvalidFilename => "that can't be a file's name",
        ErrorKind::TooManyLinks => "the path goes through too many links",
        ErrorKind::CrossesDevices => "it would move from one disk to another",
        ErrorKind::ResourceBusy => "something else is using it",
        ErrorKind::ExecutableFileBusy => "the file is a program that's running",
        ErrorKind::Interrupted => "the system broke off",
        ErrorKind::TimedOut => "the system took too long",
        ErrorKind::UnexpectedEof => "it ended early",
        ErrorKind::OutOfMemory => "the system ran out of memory",
        ErrorKind::Unsupported => "the system can't do that",
        // What else the system can fail with, said the same way everywhere.
        other => return format!("the system failed: {other}"),
    }
    .to_owned()
}

/// A read of a file that failed, as the failure says it, unless what's there is a folder, which
/// is said as one on every system: Windows says a folder opened as a file is missing, or that it's
/// not allowed, where Unix says it's a folder, and an error's kind alone can't tell a real refusal
/// apart from a folder there.
fn read_failure(err: std::io::Error, is_folder: impl FnOnce() -> bool) -> Fail {
    if is_folder() {
        return Fail { case: "Other", reason: in_own_words(std::io::ErrorKind::IsADirectory) };
    }
    Fail::from(err)
}

/// A whole file's bytes, as `read` gives them, as text or as bytes. Bytes that aren't UTF-8 fail
/// as text, as the standard library's own read of text does.
fn read_whole(text: bool, read: impl FnOnce() -> Result<Vec<u8>, Fail> + Send + 'static) -> Value {
    let data = blocking(read);
    if !text {
        return done(data, |data| Value::Bytes(Rc::from(data)));
    }
    let text = data.and_then(|data| {
        String::from_utf8(data).map_err(|_| Fail::from(std::io::Error::from(std::io::ErrorKind::InvalidData)))
    });
    done(text, |text| Value::str(&text))
}

/// A file opened to read, unless it's a folder, which Unix opens as a file and Windows doesn't:
/// it fails as one on every system.
fn not_a_folder(file: std::fs::File) -> Result<std::fs::File, Fail> {
    if file.metadata().is_ok_and(|found| found.is_dir()) {
        return Err(Fail { case: "Other", reason: in_own_words(std::io::ErrorKind::IsADirectory) });
    }
    Ok(file)
}

/// Whether `at`, beneath `folder`, is a folder.
fn is_folder_beneath(folder: &cap_std::fs::Dir, at: &Path) -> bool {
    folder.metadata(at).is_ok_and(|found| found.is_dir())
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
    use std::rc::Rc;

    use fennec_syntax::Span;

    use super::{Fail, handle_of, real_file, real_fs};
    use crate::stdlib::faults;
    use crate::value::Value;

    /// The system's failures that a caller may handle differently each have a case of their own,
    /// on the real file system, and the rest are `Other`. Each is said in Fennec's words, the same
    /// on every system, and one made with words of its own keeps them.
    #[test]
    fn a_systems_failure_is_the_case_its_kind_is() {
        let gone = std::fs::read("/no/such/place/for/fennec").expect_err("it isn't there");
        let fail = Fail::from(gone);
        assert_eq!((fail.case, fail.reason.as_str()), ("NotFound", "nothing is there"));
        // A folder read as a file is said as one on every system, though Windows calls it missing.
        for read in ["read_text", "read_bytes", "_open"] {
            let folder = Value::str(&std::env::temp_dir().to_string_lossy());
            let shown = crate::format::repr(&real_fs(read, &[folder], Span::default()).unwrap());
            assert!(shown.contains("reason=\"it's a folder, not a file\""), "{read}: {shown}");
        }
        let binary = std::io::read_to_string(&[0xFF, 0xFE][..]).expect_err("it isn't text");
        assert_eq!(Fail::from(binary).reason, "the file isn't UTF-8 text");
        assert_eq!(
            Fail::from(std::io::Error::from(std::io::ErrorKind::PermissionDenied)).reason,
            "the system won't allow it"
        );
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

    /// The names in `folder`, in order.
    fn names(folder: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(folder)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// What `fs`'s method gives, as Fennec shows it.
    fn shown(method: &str, args: &[Value]) -> String {
        crate::format::repr(
            &real_fs(method, args, Span::default()).unwrap_or_else(|why| panic!("{}", why.message)),
        )
    }

    /// A `File` made to write `target`, its block not yet ended.
    fn made(target: &std::path::Path, durable: bool) -> Rc<Value> {
        let path = Value::str(&target.to_string_lossy());
        match real_fs("_create", &[path, Value::Bool(durable)], Span::default()) {
            Ok(Value::Ok(file)) => file,
            other => panic!("{:?}", other.map(|made| crate::format::repr(&made))),
        }
    }

    /// What a `File`'s method gives, as Fennec shows it.
    fn file_does(file: &Value, method: &str, args: &[Value]) -> String {
        let handle = handle_of(file).expect("the file is open");
        crate::format::repr(
            &real_file(handle, method, args, Span::default()).unwrap_or_else(|why| panic!("{}", why.message)),
        )
    }

    /// A whole file written that fails, at any step, leaves the file it was to replace as it was and
    /// nothing beside it, and says why in Fennec's words, the same on every system.
    #[test]
    fn a_whole_write_that_fails_leaves_the_file_as_it_was() -> std::io::Result<()> {
        let temp = fennec_temp::Temp::folder("faults-whole")?;
        let target = temp.path().join("kept.txt");
        std::fs::write(&target, "old")?;
        let path = Value::str(&target.to_string_lossy());
        // Each error, as the `IoError` case and words a program is given.
        for (rule, said) in [
            ("create:1:nospace", "Other(reason=\"the disk is full\")"),
            ("write:1:quota", "Other(reason=\"the disk space allowed is used up\")"),
            ("write:1:readonly", "Other(reason=\"the disk can't be written to\")"),
            ("write:1:denied", "PermissionDenied(reason=\"the system won't allow it\")"),
            ("write:1:nospace:2", "Other(reason=\"the disk is full\")"),
            ("write:1:toolarge:100", "Other(reason=\"the file is too big for the system\")"),
            ("sync:1:readonly", "Other(reason=\"the disk can't be written to\")"),
            ("rename:1:io", "Other(reason=\"the system failed: other error\")"),
        ] {
            faults::put_in(rule);
            let written = shown("write_text", &[path.clone(), Value::str("new text")]);
            assert!(written.contains(said), "{rule}: {written}");
            assert_eq!(std::fs::read_to_string(&target)?, "old", "{rule}");
            assert_eq!(names(temp.path()), ["kept.txt"], "{rule}");
        }
        // A refusal to add a file beside it fails the write: it's never written where it is, where
        // a full disk would leave half of it.
        faults::put_in("create:1:denied");
        let refused = shown("write_text", &[path.clone(), Value::str("new text")]);
        assert!(refused.contains("PermissionDenied(reason=\"the system won't allow it\")"), "{refused}");
        assert_eq!(std::fs::read_to_string(&target)?, "old");
        // The rename's own sync failing leaves the new file in place, and fails all the same.
        faults::put_in("sync:2:io");
        let unsynced = shown("write_text", &[path.clone(), Value::str("new text")]);
        assert!(unsynced.contains("the system failed: other error"), "{unsynced}");
        assert_eq!(std::fs::read_to_string(&target)?, "new text");
        assert_eq!(names(temp.path()), ["kept.txt"]);
        // A rule for more of a kind of work than the program does fails nothing. A whole write is
        // one create, one write, a sync, a rename and the folder's sync, with no flush.
        faults::put_in("create:2:io,write:2:nospace,sync:3:io,rename:2:io,flush:1:io");
        assert_eq!(shown("write_text", &[path.clone(), Value::str("newer")]), "Ok()");
        assert_eq!(std::fs::read_to_string(&target)?, "newer");
        assert_eq!(names(temp.path()), ["kept.txt"]);
        // `durable=False`: neither sync is done, or counted, so a rule for one fails nothing.
        faults::put_in("sync:1:io");
        assert_eq!(shown("write_text", &[path.clone(), Value::str("quick"), Value::Bool(false)]), "Ok()");
        assert_eq!(std::fs::read_to_string(&target)?, "quick");
        faults::put_in("");
        Ok(())
    }

    /// A run killed at each step of a write, run again as this test in a process of its own,
    /// leaves the file it was to replace whole, old or new, never part of either. The file it was
    /// writing beside it is left where the kill came before the rename, as nothing's left to throw
    /// it away, holding what reached it. Nothing clears it on a later run.
    #[test]
    fn a_run_killed_partway_leaves_its_target_old_or_new() -> std::io::Result<()> {
        if let Some(rule) = std::env::var_os("FENNEC_FAULTS_KILL_RULE") {
            let rule = rule.to_string_lossy().into_owned();
            let target =
                std::path::PathBuf::from(std::env::var_os("FENNEC_FAULTS_KILL_IN").unwrap_or_default())
                    .join("kept.txt");
            faults::put_in(&rule);
            if std::env::var_os("FENNEC_FAULTS_KILL_WHOLE").is_some() {
                shown("write_text", &[Value::str(&target.to_string_lossy()), Value::str("new")]);
            } else {
                let file = made(&target, true);
                file_does(&file, "write", &[Value::str("n")]);
                file_does(&file, "write", &[Value::str("ew")]);
                shown("_finish", &[(*file).clone()]);
            }
            panic!("{rule} didn't kill the run");
        }
        // Each rule, whether the write is whole, what the target holds after, and what the file
        // beside it holds, if one's left.
        for (rule, whole, left, beside) in [
            ("create:1:kill", false, "old", None),
            ("write:2:kill:1", false, "old", Some("ne")),
            ("flush:1:kill", false, "old", Some("")),
            ("sync:1:kill", false, "old", Some("new")),
            ("rename:1:kill", false, "old", Some("new")),
            ("sync:2:kill", false, "new", None),
            ("create:1:kill", true, "old", None),
            ("write:1:kill:2", true, "old", Some("ne")),
            ("sync:1:kill", true, "old", Some("new")),
            ("rename:1:kill", true, "old", Some("new")),
            ("sync:2:kill", true, "new", None),
        ] {
            let temp = fennec_temp::Temp::folder("faults-killed")?;
            std::fs::write(temp.path().join("kept.txt"), "old")?;
            let mut run = std::process::Command::new(std::env::current_exe()?);
            run.args(["--exact", "stdlib::fs::tests::a_run_killed_partway_leaves_its_target_old_or_new"])
                .env("FENNEC_FAULTS_KILL_RULE", rule)
                .env("FENNEC_FAULTS_KILL_IN", temp.path());
            if whole {
                run.env("FENNEC_FAULTS_KILL_WHOLE", "1");
            }
            let ran = run.output()?;
            let said =
                String::from_utf8_lossy(&ran.stdout).into_owned() + &String::from_utf8_lossy(&ran.stderr);
            assert!(!ran.status.success(), "{rule}: {said}");
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt as _;
                assert_eq!(ran.status.signal(), Some(9), "{rule}: {said}");
            }
            assert_eq!(std::fs::read_to_string(temp.path().join("kept.txt"))?, left, "{rule}");
            let partial: Vec<String> =
                names(temp.path()).into_iter().filter(|name| name.ends_with(".fennec-partial")).collect();
            match beside {
                None => assert!(partial.is_empty(), "{rule}: {partial:?}"),
                Some(held) => {
                    assert_eq!(partial.len(), 1, "{rule}: {partial:?}");
                    let found = partial.first().map(|name| temp.path().join(name)).unwrap_or_default();
                    assert_eq!(std::fs::read_to_string(found)?, held, "{rule}");
                }
            }
        }
        Ok(())
    }

    /// A `File`'s write the disk cuts short leaves what reached the file, and fails in Fennec's
    /// words. A program that goes on writing past it has the rest after what reached it.
    #[test]
    fn a_files_write_cut_short_keeps_what_reached_it() -> std::io::Result<()> {
        let temp = fennec_temp::Temp::folder("faults-file")?;
        let target = temp.path().join("log.txt");
        faults::put_in("write:2:nospace:3");
        let file = made(&target, true);
        assert_eq!(file_does(&file, "write", &[Value::str("one\n")]), "Ok()");
        let cut = file_does(&file, "write", &[Value::str("two\n")]);
        assert!(cut.contains("Other(reason=\"the disk is full\")"), "{cut}");
        assert_eq!(file_does(&file, "write", &[Value::str("three\n")]), "Ok()");
        assert_eq!(shown("_finish", &[(*file).clone()]), "Ok()");
        assert_eq!(std::fs::read_to_string(&target)?, "one\ntwothree\n");
        assert_eq!(names(temp.path()), ["log.txt"]);
        faults::put_in("");
        Ok(())
    }

    /// A `File`'s block that fails as it ends, at any step before its rename, leaves its target as
    /// it was, and nothing beside it. A durable file's folder is synced after the rename, and
    /// counted on every system, so `sync:2` fails there on each, with the file already in place.
    #[test]
    fn a_file_whose_end_fails_leaves_its_target_as_it_was() -> std::io::Result<()> {
        let temp = fennec_temp::Temp::folder("faults-end")?;
        let target = temp.path().join("kept.txt");
        for (rule, durable, said, left) in [
            ("flush:1:nospace", true, "the disk is full", "old"),
            ("sync:1:readonly", true, "the disk can't be written to", "old"),
            ("rename:1:denied", false, "the system won't allow it", "old"),
            ("sync:2:io", true, "the system failed: other error", "new"),
        ] {
            std::fs::write(&target, "old")?;
            faults::put_in(rule);
            let file = made(&target, durable);
            assert_eq!(file_does(&file, "write", &[Value::str("new")]), "Ok()", "{rule}");
            let ended = shown("_finish", &[(*file).clone()]);
            assert!(ended.contains(&format!("reason=\"{said}\"")), "{rule}: {ended}");
            assert_eq!(std::fs::read_to_string(&target)?, left, "{rule}");
            assert_eq!(names(temp.path()), ["kept.txt"], "{rule}");
        }
        // A `flush` or `sync` the program asks for fails where it's asked, and is counted with
        // those its block's end does.
        faults::put_in("flush:1:quota,sync:2:io");
        let file = made(&target, true);
        assert!(file_does(&file, "flush", &[]).contains("the disk space allowed is used up"));
        assert_eq!(file_does(&file, "sync", &[]), "Ok()");
        assert_eq!(file_does(&file, "write", &[Value::str("synced")]), "Ok()");
        assert!(shown("_finish", &[(*file).clone()]).contains("the system failed: other error"));
        faults::put_in("");
        Ok(())
    }

    /// A file a gone run left beside its target, writing it when it was killed, is cleared the
    /// first time this run writes in its folder. On Unix, one whose maker's process number a live
    /// process has is kept, as that process may be writing it. (One this run holds is kept too:
    /// `left::tests`.)
    #[test]
    fn what_gone_runs_left_is_cleared_when_a_folder_is_first_written() -> std::io::Result<()> {
        let temp = fennec_temp::Temp::folder("left-behind")?;
        faults::put_in("");
        // A process that's ended, whose number nothing has yet.
        let mut ended = std::process::Command::new(std::env::current_exe()?).arg("--list").spawn()?;
        let gone = ended.id();
        ended.wait()?;
        let left =
            [format!(".kept.txt.{gone}.1.fennec-partial"), format!(".other.txt.{gone}.9.fennec-partial")];
        // This run's own number, which no file this run holds has: left by an earlier run that had it.
        let earlier = format!(".kept.txt.{}.999999.fennec-partial", std::process::id());
        for name in left.iter().chain([&earlier]) {
            std::fs::write(temp.path().join(name), "half")?;
        }
        // One a live process may be writing, by its number.
        #[cfg(unix)]
        let alive = format!(".kept.txt.{}.1.fennec-partial", std::os::unix::process::parent_id());
        #[cfg(unix)]
        std::fs::write(temp.path().join(&alive), "half")?;
        let target = Value::str(&temp.path().join("kept.txt").to_string_lossy());
        assert_eq!(shown("write_text", &[target, Value::str("whole")]), "Ok()");
        let mut kept = vec!["kept.txt".to_owned()];
        #[cfg(unix)]
        kept.push(alive);
        kept.sort();
        assert_eq!(names(temp.path()), kept);
        // Once a folder: a file left after the first write there is left for a later run.
        let later = format!(".kept.txt.{gone}.2.fennec-partial");
        std::fs::write(temp.path().join(&later), "half")?;
        let target = Value::str(&temp.path().join("kept.txt").to_string_lossy());
        assert_eq!(shown("write_text", &[target, Value::str("again")]), "Ok()");
        assert!(names(temp.path()).contains(&later));
        Ok(())
    }

    /// A file in a folder the program can't add a file to isn't written where it is, though the
    /// file itself could be: written in place, a full disk would leave half of it. It fails with
    /// `PermissionDenied`, and the file keeps what it held.
    #[cfg(unix)]
    #[test]
    fn a_file_in_a_folder_that_cant_be_added_to_isnt_written_in_place() -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        let temp = fennec_temp::Temp::folder("no-room-beside")?;
        let folder = temp.path().join("shut");
        std::fs::create_dir(&folder)?;
        let target = folder.join("kept.txt");
        std::fs::write(&target, "old")?;
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555))?;
        // Someone the system lets add a file anyway, as root is, can't see this.
        let shut = std::fs::write(folder.join("probe"), "").is_err();
        faults::put_in("");
        let written = shown("write_text", &[Value::str(&target.to_string_lossy()), Value::str("new")]);
        let held = std::fs::read_to_string(&target)?;
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755))?;
        if shut {
            assert!(written.contains("PermissionDenied(reason=\"the system won't allow it\")"), "{written}");
            assert_eq!(held, "old");
        }
        Ok(())
    }
}
