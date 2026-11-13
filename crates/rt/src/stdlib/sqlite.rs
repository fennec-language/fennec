//! `std.sqlite`: the methods of a `Database` and a `Transaction` written in Rust, by the handle
//! each holds, which only these make. SQLite itself is compiled in, through rusqlite, whose
//! `unsafe` stays its own.
//!
//! A database has a connection its own queries run on, one at a time, and each transaction takes
//! a connection of its own, so two tasks' transactions never mix. Every wait on SQLite runs on
//! the runtime's blocking threads inside a task, so it pauses only that task. SQLite is never
//! left to wait on a lock itself: a query that finds the database's writer taken gives the thread
//! back, and its task pauses and tries again, so a waiting task never holds a thread that the
//! writer needs to finish.

use std::rc::Rc;

use crate::value::Value;

/// The `Err` holding the case of `SqlError` a failure is.
fn failed(case: &str, reason: &str) -> Value {
    Value::Err(Rc::new(Value::made_case("SqlError", case, vec![("reason", Value::str(reason))])))
}

#[cfg(any(unix, windows))]
pub(crate) use real::open_checked;
pub(crate) use real::{Fakes, call, open_fake, open_file};

#[cfg(test)]
mod tests;

mod real {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, LazyLock, Mutex, PoisonError};
    use std::time::{Duration, Instant};

    use fennec_syntax::{Diagnostic, Span};
    use rusqlite::config::DbConfig;
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    use rusqlite::limits::Limit;
    use rusqlite::types::{Value as Cell, ValueRef};
    use rusqlite::{Connection, ErrorCode, OpenFlags};

    use super::failed;
    use crate::stdlib::fs::{blocking, io_error};
    use crate::value::Value;

    /// How long a query waits for another writer to finish before failing with `Busy`.
    const BUSY_WAIT: Duration = Duration::from_secs(5);

    /// What a narrowed `Fs` found at a database's path, to tell it's the file SQLite opened: its
    /// device and number on Unix, and on Windows the file itself, held open, which says its volume
    /// and index.
    #[cfg(unix)]
    pub(crate) type FileId = (u64, u64);
    #[cfg(windows)]
    pub(crate) type FileId = Arc<same_file::Handle>;

    /// How many connections a database keeps for its next transactions once they're done.
    const KEPT_IDLE: usize = 8;

    /// Where a database is: a file, or a test's database, in a file of the test's own that
    /// nothing else sees.
    #[derive(Clone)]
    enum Place {
        File(String),
        Test(String),
        /// A file an `Fs` narrowed with `only` opened, by its path with no link in it and the
        /// file the narrowed `Fs` found there: each connection checks it opened that one.
        #[cfg(any(unix, windows))]
        Checked {
            path: String,
            file: FileId,
        },
        /// Any of those, opened through an `Fs` narrowed with `read_only`: for reading only, and
        /// never made.
        ReadOnly(Box<Place>),
    }

    /// An open database.
    struct Opened {
        place: Place,
        /// The connection the database's own queries run on, one at a time.
        main: Arc<Mutex<Connection>>,
        /// Connections transactions are done with, for the next ones to take.
        idle: Mutex<Vec<Connection>>,
    }

    enum Entry {
        Database(Arc<Opened>),
        /// A transaction's connection, which holds the database's writer until it's done.
        Transaction(Arc<Opened>, Arc<Mutex<Connection>>),
    }

    static OPEN: LazyLock<Mutex<HashMap<i64, Entry>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
    static NEXT: AtomicU64 = AtomicU64::new(1);

    fn kept(entry: Entry) -> i64 {
        let handle = i64::try_from(NEXT.fetch_add(1, Ordering::Relaxed)).unwrap_or(i64::MAX);
        OPEN.lock().unwrap_or_else(PoisonError::into_inner).insert(handle, entry);
        handle
    }

    /// A `Database` or a `Transaction`, holding its handle.
    fn held(type_name: &str, handle: i64) -> Value {
        Value::made(type_name, vec![("_handle", Value::Int(handle))])
    }

    /// The databases a test has opened, each a file in a folder of the test's own, made when it
    /// first opens one and gone when the test ends. A test's database is a real one in WAL mode,
    /// so it does what a database on the disk does: a query outside a transaction reads what was
    /// there before it, while it's still going.
    #[derive(Default)]
    pub(crate) struct Fakes {
        pub(super) folder: Option<PathBuf>,
        /// Each path's file in the folder, by the path's key in the test's fake file system.
        files: HashMap<String, PathBuf>,
    }

    impl Drop for Fakes {
        fn drop(&mut self) {
            if let Some(folder) = self.folder.take() {
                // A database the test left open holds its file, which Windows won't remove while
                // it's open, so the test's own are closed first.
                let ours = |entry: &Entry| {
                    let (Entry::Database(opened) | Entry::Transaction(opened, _)) = entry;
                    matches!(path_of_test(&opened.place), Some(path) if std::path::Path::new(path).starts_with(&folder))
                };
                let closed: Vec<Entry> = {
                    let mut open = OPEN.lock().unwrap_or_else(PoisonError::into_inner);
                    let handles: Vec<i64> =
                        open.iter().filter(|(_, entry)| ours(entry)).map(|(handle, _)| *handle).collect();
                    handles.iter().filter_map(|handle| open.remove(handle)).collect()
                };
                // Closed outside the lock, which other tests' databases take.
                drop(closed);
                let _ = std::fs::remove_dir_all(folder);
            }
        }
    }

    impl Fakes {
        /// The file for the database at `key`, and its folder made if this is the first.
        fn file(&mut self, key: &str) -> std::io::Result<PathBuf> {
            if let Some(file) = self.files.get(key) {
                return Ok(file.clone());
            }
            if self.folder.is_none() {
                self.folder = Some(fresh_folder()?);
            }
            let folder = self.folder.clone().unwrap_or_default();
            let file = folder.join(format!("{}.db", self.files.len()));
            self.files.insert(key.to_owned(), file.clone());
            Ok(file)
        }
    }

    /// A folder of a test's own for its databases, named at random and made new: one a crashed
    /// run left behind, with its databases still in it, is never taken for this test's.
    fn fresh_folder() -> std::io::Result<PathBuf> {
        loop {
            let mut name = [0; 8];
            getrandom::fill(&mut name).map_err(std::io::Error::other)?;
            let made =
                std::env::temp_dir().join(format!("fennec-test-databases-{:016x}", u64::from_le_bytes(name)));
            match std::fs::create_dir(&made) {
                Ok(()) => return Ok(made),
                // Taken already, by chance: draw another.
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
    }

    /// A new connection, which never waits on a lock itself: see the module's comment.
    fn connect(place: &Place) -> rusqlite::Result<Connection> {
        // Serialized: SQLite takes a lock of its own around each call, as well as ours.
        let (place, read_only) = match place {
            Place::ReadOnly(inner) => (&**inner, true),
            other => (other, false),
        };
        let flags = if read_only {
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_FULL_MUTEX
        } else {
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_FULL_MUTEX
        };
        let (path, flags) = match place {
            // Nothing's made for a database read only: one that isn't there fails to open.
            Place::File(path) | Place::Test(path) if read_only => (path, flags),
            Place::File(path) | Place::Test(path) => (path, flags | OpenFlags::SQLITE_OPEN_CREATE),
            // The narrowed `Fs` made the file already, beneath its folder, so SQLite makes none,
            // and refuses a path with a link in it, which the `Fs` resolved to one with none: a
            // link swapped in on the way can't lead it to make or open a file outside.
            #[cfg(any(unix, windows))]
            Place::Checked { path, .. } => (path, flags | OpenFlags::SQLITE_OPEN_NOFOLLOW),
            Place::ReadOnly(_) => return Err(rusqlite::Error::InvalidPath(path_of(place).into())),
        };
        let mut conn = Connection::open_with_flags(path, flags)?;
        kept_to_its_file(&mut conn)?;
        #[cfg(any(unix, windows))]
        if let Place::Checked { path, file } = place
            && !still_the_file(&conn, path, file)
        {
            let message = format!(
                "the database at `{path}` changed while it was being opened, so it isn't the one this Fs \
                 can reach"
            );
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
                Some(message),
            ));
        }
        // Readers go on while a writer writes, and a commit is on the disk before it's done,
        // as SQLite's default `synchronous=FULL` has it. One read only can't change its file's
        // journal, and reads it as it is.
        if !read_only {
            let _: String = conn.query_row("pragma journal_mode=wal", [], |row| row.get(0))?;
        }
        if let Place::Test(_) = place {
            // A test's database is gone when the test ends, so there's nothing to keep through
            // a power cut, and its commits needn't wait on the disk.
            conn.execute_batch("pragma synchronous=off")?;
        }
        conn.busy_timeout(Duration::ZERO)?;
        Ok(conn)
    }

    /// The pragmas `std.sqlite` itself runs, the only ones a connection takes.
    const OWN_PRAGMAS: [&str; 2] = ["journal_mode", "synchronous"];

    /// Keeps a connection to its own file. A query's text is the program's, so without this it
    /// could `ATTACH` any file the process can open, write a copy of the database anywhere with
    /// `VACUUM INTO`, or move SQLite's temporary files with a pragma, reaching past the `Fs` it
    /// was opened through. So a connection takes only the actions ordinary queries need, no
    /// database with a file may be attached, a pragma runs only if it's one of
    /// `std.sqlite`'s own, and SQLite's defensive mode is on, with the schema not trusted to
    /// call functions or views that could do the same.
    fn kept_to_its_file(conn: &mut Connection) -> rusqlite::Result<()> {
        // One, for the database of no file a plain `VACUUM` attaches while it works. The
        // authorizer below refuses any that names a file.
        conn.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 1)?;
        conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
        conn.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)?;
        conn.authorizer(Some(
            |context: AuthContext<'_>| {
                if allowed(&context) { Authorization::Allow } else { Authorization::Deny }
            },
        ))
    }

    /// Whether a connection takes an action a statement asks for. It's what ordinary queries
    /// need, and nothing else: whatever isn't named here is refused, an action SQLite adds later
    /// included, until it's looked at and allowed.
    fn allowed(context: &AuthContext<'_>) -> bool {
        let in_main = context.database_name.is_none_or(|name| name == "main");
        let in_temp = context.database_name == Some("temp");
        // A plain `VACUUM` builds the database again in a scratch one of no file, `vacuum_...`.
        let in_vacuum = context.database_name.is_some_and(|name| name.starts_with("vacuum_"));
        match context.action {
            AuthAction::Select
            | AuthAction::Read { .. }
            | AuthAction::Insert { .. }
            | AuthAction::Update { .. }
            | AuthAction::Delete { .. }
            | AuthAction::Transaction { .. }
            | AuthAction::Savepoint { .. }
            | AuthAction::Function { .. }
            | AuthAction::Recursive => true,
            // Its own tables, indexes, views and triggers, and changing a table as a migration
            // does. A new index asks to be built, as `REINDEX` does. None of them can reach a
            // file, with `ATTACH` refused and the schema not trusted.
            AuthAction::CreateTable { .. } | AuthAction::CreateIndex { .. } => in_main || in_vacuum,
            AuthAction::DropTable { .. }
            | AuthAction::DropIndex { .. }
            | AuthAction::Reindex { .. }
            | AuthAction::CreateView { .. }
            | AuthAction::DropView { .. }
            | AuthAction::CreateTrigger { .. }
            | AuthAction::DropTrigger { .. } => in_main,
            // The connection's own temporary tables, their indexes, views and triggers.
            AuthAction::CreateTempTable { .. }
            | AuthAction::CreateTempIndex { .. }
            | AuthAction::CreateTempView { .. }
            | AuthAction::CreateTempTrigger { .. }
            | AuthAction::DropTempTable { .. }
            | AuthAction::DropTempIndex { .. }
            | AuthAction::DropTempView { .. }
            | AuthAction::DropTempTrigger { .. } => in_temp,
            AuthAction::AlterTable { database_name, .. } => database_name == "main",
            AuthAction::Pragma { pragma_name, .. } => OWN_PRAGMAS.contains(&pragma_name),
            // A plain `VACUUM` attaches a database of no file, which reaches nothing; one with
            // a file, as `ATTACH` or `VACUUM INTO` gives, could reach any, and one whose name is
            // an expression comes as `Unknown`, refused below with everything else.
            AuthAction::Attach { filename } => filename.is_empty(),
            _ => false,
        }
    }

    fn open(place: Place) -> Result<i64, String> {
        let first = place.clone();
        let main = blocking(move || connect(&first).map_err(|err| err.to_string()))?;
        let opened = Opened { place, main: Arc::new(Mutex::new(main)), idle: Mutex::new(Vec::new()) };
        Ok(kept(Entry::Database(Arc::new(opened))))
    }

    /// Where `place` is, for a message.
    fn path_of(place: &Place) -> &str {
        match place {
            Place::File(path) | Place::Test(path) => path,
            #[cfg(any(unix, windows))]
            Place::Checked { path, .. } => path,
            Place::ReadOnly(inner) => path_of(inner),
        }
    }

    /// Where a test's database is, read only or not, or `None` for one that isn't a test's.
    fn path_of_test(place: &Place) -> Option<&str> {
        match place {
            Place::Test(path) => Some(path),
            Place::ReadOnly(inner) => path_of_test(inner),
            _ => None,
        }
    }

    /// `place`, read only when `read_only` says so.
    fn reading(place: Place, read_only: bool) -> Place {
        if read_only { Place::ReadOnly(Box::new(place)) } else { place }
    }

    /// `fs._sqlite_open(path)` on the real disk: the handle of the database in the file at `path`,
    /// for reading only when it's opened through an `Fs` narrowed with `read_only`.
    pub(crate) fn open_file(path: &str, read_only: bool) -> Value {
        match open(reading(Place::File(path.to_owned()), read_only)) {
            Ok(handle) => Value::Ok(Rc::new(held("Database", handle))),
            Err(reason) => io_error(&reason),
        }
    }

    /// Whether the database `conn` opened at `path` is still the file the narrowed `Fs` noted:
    /// the path names it before and after SQLite is asked, and SQLite says its own open file is
    /// what the path names. A folder or link swapped in on the way is caught, short of one swapped
    /// back and forth between those looks.
    #[cfg(unix)]
    pub(in crate::stdlib::sqlite) fn still_the_file(conn: &Connection, path: &str, file: &FileId) -> bool {
        use std::os::unix::fs::MetadataExt as _;
        let named = || std::fs::metadata(path).is_ok_and(|found| (found.dev(), found.ino()) == *file);
        let before = named();
        let mut moved: std::ffi::c_int = -1;
        let asked = conn.file_control(rusqlite::DEFAULT_NAME, rusqlite::FileControl::HasMoved(&mut moved));
        before && asked.is_ok() && moved == 0 && named()
    }

    /// On Windows, whether the path names the file the narrowed `Fs` noted, after SQLite has
    /// opened it. SQLite can't say there whether its own open file has moved, so a file swapped
    /// out and back while it opens isn't caught, as on Unix one swapped between the looks isn't.
    #[cfg(windows)]
    pub(in crate::stdlib::sqlite) fn still_the_file(_conn: &Connection, path: &str, file: &FileId) -> bool {
        same_file::Handle::from_path(path).is_ok_and(|found| found == **file)
    }

    /// `fs._sqlite_open(path)` for an `Fs` narrowed with `only`, on the real disk: the database in
    /// the file at `path`, refused unless SQLite opened `file`, the one the narrowed `Fs` found.
    #[cfg(any(unix, windows))]
    pub(crate) fn open_checked(path: &str, file: FileId, read_only: bool) -> Value {
        match open(reading(Place::Checked { path: path.to_owned(), file }, read_only)) {
            Ok(handle) => Value::Ok(Rc::new(held("Database", handle))),
            Err(reason) => io_error(&reason),
        }
    }

    /// `fs._sqlite_open(path)` in a test: the test's own database for `path`, by the path's key
    /// in its fake file system.
    pub(crate) fn open_fake(fakes: &mut Fakes, key: &str, read_only: bool) -> Value {
        let file = match fakes.file(key) {
            Ok(file) => file,
            Err(err) => return io_error(&err.to_string()),
        };
        match open(reading(Place::Test(file.to_string_lossy().into_owned()), read_only)) {
            Ok(handle) => Value::Ok(Rc::new(held("Database", handle))),
            Err(reason) => io_error(&reason),
        }
    }

    /// A query's text and its values, as sqlite.fen hands them over.
    struct Query {
        text: String,
        values: Vec<Cell>,
    }

    /// The items of a list argument, however it was handed over.
    fn items(value: &Value) -> Option<Vec<Value>> {
        match value.clone().unpacked() {
            Value::List(items) => Some(items.as_ref().clone()),
            _ => None,
        }
    }

    fn query_of(args: &[Value]) -> Option<Query> {
        let [Value::Str(text), kinds, ints, floats, texts, blobs] = args else { return None };
        let (mut ints, mut floats, mut texts, mut blobs) = (
            items(ints)?.into_iter(),
            items(floats)?.into_iter(),
            items(texts)?.into_iter(),
            items(blobs)?.into_iter(),
        );
        let mut values = Vec::new();
        for kind in items(kinds)? {
            values.push(match kind {
                Value::Int(0) => match ints.next()? {
                    Value::Int(n) => Cell::Integer(n),
                    _ => return None,
                },
                Value::Int(1) => match floats.next()? {
                    Value::Float(x) => Cell::Real(x),
                    _ => return None,
                },
                Value::Int(2) => match texts.next()? {
                    Value::Str(text) => Cell::Text(text.to_string()),
                    _ => return None,
                },
                Value::Int(3) => match blobs.next()? {
                    Value::Bytes(data) => Cell::Blob(data.to_vec()),
                    _ => return None,
                },
                _ => return None,
            });
        }
        Some(Query { text: text.to_string(), values })
    }

    /// A query's statement, with its values bound.
    fn prepared<'c>(conn: &'c Connection, query: &Query) -> rusqlite::Result<rusqlite::Statement<'c>> {
        let mut statement = conn.prepare(&query.text)?;
        if statement.parameter_count() != query.values.len() {
            // Only text written in the source is a query's, so this is a `?` written there.
            let message = format!(
                "the query has {} values but {} places for them: a `?` or `:name` in its text is one",
                query.values.len(),
                statement.parameter_count()
            );
            return Err(rusqlite::Error::InvalidParameterName(message));
        }
        // SQLite stores a NaN as NULL, so the value would quietly be gone.
        if query.values.iter().any(|value| matches!(value, Cell::Real(x) if x.is_nan())) {
            return Err(rusqlite::Error::ToSqlConversionFailure(
                "NaN can't be stored: SQLite would turn it into NULL".into(),
            ));
        }
        for (at, value) in query.values.iter().enumerate() {
            statement.raw_bind_parameter(at + 1, value)?;
        }
        Ok(statement)
    }

    fn execute(conn: &Connection, query: &Query) -> rusqlite::Result<i64> {
        let mut statement = prepared(conn, query)?;
        if statement.column_count() > 0 {
            // A query that gives rows, like a `pragma`, runs to its end, its rows dropped.
            let mut rows = statement.raw_query();
            while rows.next()?.is_some() {}
            return Ok(0);
        }
        Ok(i64::try_from(statement.raw_execute()?).unwrap_or(i64::MAX))
    }

    /// What a query found, as plain data, which can come back from another thread.
    #[derive(Default)]
    struct Found {
        columns: Vec<String>,
        kinds: Vec<i64>,
        at: Vec<i64>,
        ints: Vec<i64>,
        floats: Vec<f64>,
        texts: Vec<String>,
        blobs: Vec<Vec<u8>>,
    }

    /// What a query found, or, for text that isn't UTF-8, its column's name.
    fn query(conn: &Connection, query: &Query) -> rusqlite::Result<Result<Found, String>> {
        let mut statement = prepared(conn, query)?;
        let columns: Vec<String> = statement.column_names().map(str::to_owned).collect();
        let width = columns.len();
        let mut found = Found { columns, ..Found::default() };
        let mut rows = statement.raw_query();
        let place = |n: usize| i64::try_from(n).unwrap_or(i64::MAX);
        while let Some(row) = rows.next()? {
            for column in 0..width {
                let (kind, at) = match row.get_ref(column)? {
                    ValueRef::Null => (0, 0),
                    ValueRef::Integer(n) => {
                        found.ints.push(n);
                        (1, found.ints.len() - 1)
                    }
                    ValueRef::Real(x) => {
                        found.floats.push(x);
                        (2, found.floats.len() - 1)
                    }
                    ValueRef::Text(text) => match std::str::from_utf8(text) {
                        Ok(text) => {
                            found.texts.push(text.to_owned());
                            (3, found.texts.len() - 1)
                        }
                        Err(_) => return Ok(Err(found.columns.get(column).cloned().unwrap_or_default())),
                    },
                    ValueRef::Blob(data) => {
                        found.blobs.push(data.to_vec());
                        (4, found.blobs.len() - 1)
                    }
                };
                found.kinds.push(kind);
                found.at.push(place(at));
            }
        }
        Ok(Ok(found))
    }

    fn rows_value(found: Found) -> Value {
        let list = |values: Vec<Value>| Value::list(values);
        Value::made(
            "SqlRows",
            vec![
                ("columns", list(found.columns.iter().map(|name| Value::str(name)).collect())),
                ("kinds", list(found.kinds.into_iter().map(Value::Int).collect())),
                ("at", list(found.at.into_iter().map(Value::Int).collect())),
                ("ints", list(found.ints.into_iter().map(Value::Int).collect())),
                ("floats", list(found.floats.into_iter().map(Value::Float).collect())),
                ("texts", list(found.texts.iter().map(|text| Value::str(text)).collect())),
                ("blobs", list(found.blobs.into_iter().map(|data| Value::Bytes(Rc::from(data))).collect())),
            ],
        )
    }

    fn busy(err: &rusqlite::Error) -> bool {
        matches!(err.sqlite_error_code(), Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked))
    }

    /// `work` on `conn`, on a blocking thread in a task, tried again while another writer holds
    /// the database, for up to `BUSY_WAIT`. Between tries, the task pauses with no thread held.
    fn on<T: Send + 'static>(
        conn: &Arc<Mutex<Connection>>,
        work: impl Fn(&Connection) -> rusqlite::Result<T> + Send + Sync + 'static,
    ) -> rusqlite::Result<T> {
        let work = Arc::new(work);
        let started = Instant::now();
        let mut wait = Duration::from_millis(1);
        loop {
            let (conn, work) = (Arc::clone(conn), Arc::clone(&work));
            let done = blocking(move || work(&conn.lock().unwrap_or_else(PoisonError::into_inner)));
            match done {
                Err(err) if busy(&err) && started.elapsed() < BUSY_WAIT => {
                    pause(wait);
                    wait = (wait * 2).min(Duration::from_millis(50));
                }
                done => return done,
            }
        }
    }

    /// Waits `length`: a task parks for it, so its thread runs others meanwhile.
    fn pause(length: Duration) {
        #[cfg(any(test, feature = "threads"))]
        if crate::embed::serve::can_park() {
            let deadline = Instant::now() + length;
            let token = crate::embed::serve::next_token();
            while Instant::now() < deadline {
                if crate::embed::serve::park_on(token, Some(deadline))
                    == crate::embed::serve::Parked::Cancelled
                {
                    return;
                }
            }
            return;
        }
        std::thread::sleep(length);
    }

    /// The `Err` a failure of SQLite's is, as the case of `SqlError` it is.
    fn sql_failed(err: &rusqlite::Error) -> Value {
        match err {
            rusqlite::Error::MultipleStatement => {
                failed("Failed", "a query is one statement: give each its own `execute`")
            }
            _ if busy(err) => failed(
                "Busy",
                &format!(
                    "another writer held the database for over {} seconds; inside a transaction, query through it rather than the database",
                    BUSY_WAIT.as_secs()
                ),
            ),
            _ => match err.sqlite_error_code() {
                Some(ErrorCode::ConstraintViolation) => failed("Constraint", &err.to_string()),
                _ => failed("Failed", &err.to_string()),
            },
        }
    }

    fn done<T>(result: rusqlite::Result<T>, made: impl FnOnce(T) -> Value) -> Value {
        match result {
            Ok(value) => Value::Ok(Rc::new(made(value))),
            Err(err) => sql_failed(&err),
        }
    }

    /// The connection a handle's queries run on, and its database.
    fn connection_of(handle: i64) -> Option<(Arc<Opened>, Arc<Mutex<Connection>>)> {
        match OPEN.lock().unwrap_or_else(PoisonError::into_inner).get(&handle)? {
            Entry::Database(opened) => Some((Arc::clone(opened), Arc::clone(&opened.main))),
            Entry::Transaction(opened, conn) => Some((Arc::clone(opened), Arc::clone(conn))),
        }
    }

    /// A method of a `Database` or a `Transaction`, by its handle: `_execute` and `_query` with a
    /// query, `_begin` and `_close` of a database, and `_commit` and `_rollback` of a transaction.
    pub(crate) fn call(handle: i64, method: &str, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
        let Some((opened, conn)) = connection_of(handle) else {
            return Ok(failed("Failed", "this database was closed when its `with` block ended"));
        };
        let wrong = || Diagnostic::new(format!("a database has no method `{method}` that takes these"), span);
        Ok(match (method, args) {
            ("_execute", _) => {
                let query = Arc::new(query_of(args).ok_or_else(wrong)?);
                done(on(&conn, move |conn| execute(conn, &query)), Value::Int)
            }
            ("_query", _) => {
                let query = Arc::new(query_of(args).ok_or_else(wrong)?);
                match on(&conn, move |conn| self::query(conn, &query)) {
                    Ok(Ok(found)) => Value::Ok(Rc::new(rows_value(found))),
                    Ok(Err(column)) => Value::Err(Rc::new(Value::made_case(
                        "SqlError",
                        "Column",
                        vec![
                            ("name", Value::str(&column)),
                            ("reason", Value::str("it holds text that isn't UTF-8")),
                        ],
                    ))),
                    Err(err) => sql_failed(&err),
                }
            }
            ("_begin", []) => begin(&opened),
            ("_commit", []) => {
                let committed = on(&conn, |conn| conn.execute_batch("commit"));
                finish(handle, &opened, &conn, committed.is_err());
                done(committed, |()| Value::Unit)
            }
            ("_rollback", []) => {
                finish(handle, &opened, &conn, true);
                Value::Unit
            }
            ("_close", []) => {
                OPEN.lock().unwrap_or_else(PoisonError::into_inner).remove(&handle);
                // Its connections close on a blocking thread, as a close may write to the disk.
                blocking(move || drop((opened, conn)));
                Value::Unit
            }
            _ => return Err(wrong()),
        })
    }

    /// `db._begin()`: a transaction's handle, on a connection of its own with the database's
    /// writer taken.
    fn begin(opened: &Arc<Opened>) -> Value {
        let idle = opened.idle.lock().unwrap_or_else(PoisonError::into_inner).pop();
        let place = opened.place.clone();
        let made = match idle {
            Some(conn) => Ok(conn),
            None => blocking(move || connect(&place)),
        };
        let conn = match made {
            Ok(conn) => conn,
            Err(err) => return sql_failed(&err),
        };
        let conn = Arc::new(Mutex::new(conn));
        // `immediate` takes the writer now, so a transaction never finds it taken halfway through.
        match on(&conn, |conn| conn.execute_batch("begin immediate")) {
            Ok(()) => {
                let handle = kept(Entry::Transaction(Arc::clone(opened), conn));
                Value::Ok(Rc::new(held("Transaction", handle)))
            }
            Err(err) => sql_failed(&err),
        }
    }

    /// A transaction is done: rolled back if `undo`, then its connection kept for the next.
    fn finish(handle: i64, opened: &Arc<Opened>, conn: &Arc<Mutex<Connection>>, undo: bool) {
        OPEN.lock().unwrap_or_else(PoisonError::into_inner).remove(&handle);
        let (opened, conn) = (Arc::clone(opened), Arc::clone(conn));
        blocking(move || {
            let conn =
                Arc::into_inner(conn).map(|conn| conn.into_inner().unwrap_or_else(PoisonError::into_inner));
            let Some(conn) = conn else { return };
            if undo || !conn.is_autocommit() {
                let _ = conn.execute_batch("rollback");
            }
            let mut idle = opened.idle.lock().unwrap_or_else(PoisonError::into_inner);
            if conn.is_autocommit() && idle.len() < KEPT_IDLE {
                idle.push(conn);
            }
        });
    }
}
