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

    /// How many connections a database keeps for its next transactions once they're done.
    const KEPT_IDLE: usize = 8;

    /// Where a database is: a file, or a test's database, in a file of the test's own that
    /// nothing else sees.
    #[derive(Clone)]
    enum Place {
        File(String),
        Test(String),
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
    pub(crate) struct Fakes {
        test: u64,
        pub(super) folder: Option<PathBuf>,
        /// Each path's file in the folder, by the path's key in the test's fake file system.
        files: HashMap<String, PathBuf>,
    }

    impl Default for Fakes {
        fn default() -> Self {
            static TESTS: AtomicU64 = AtomicU64::new(0);
            Fakes { test: TESTS.fetch_add(1, Ordering::Relaxed), folder: None, files: HashMap::new() }
        }
    }

    impl Drop for Fakes {
        fn drop(&mut self) {
            if let Some(folder) = self.folder.take() {
                // A database the test left open holds its file, which Windows won't remove while
                // it's open, so the test's own are closed first.
                let ours = |entry: &Entry| {
                    let (Entry::Database(opened) | Entry::Transaction(opened, _)) = entry;
                    matches!(&opened.place, Place::Test(path) if std::path::Path::new(path).starts_with(&folder))
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
                let made = std::env::temp_dir().join(format!(
                    "fennec-test-databases-{}-{}",
                    std::process::id(),
                    self.test
                ));
                std::fs::create_dir_all(&made)?;
                self.folder = Some(made);
            }
            let folder = self.folder.clone().unwrap_or_default();
            let file = folder.join(format!("{}.db", self.files.len()));
            self.files.insert(key.to_owned(), file.clone());
            Ok(file)
        }
    }

    /// A new connection, which never waits on a lock itself: see the module's comment.
    fn connect(place: &Place) -> rusqlite::Result<Connection> {
        // Serialized: SQLite takes a lock of its own around each call, as well as ours.
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_FULL_MUTEX;
        let (Place::File(path) | Place::Test(path)) = place;
        let mut conn = Connection::open_with_flags(path, flags)?;
        kept_to_its_file(&mut conn)?;
        // Readers go on while a writer writes, and a commit is on the disk before it's done,
        // as SQLite's default `synchronous=FULL` has it.
        let _: String = conn.query_row("pragma journal_mode=wal", [], |row| row.get(0))?;
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
    /// was opened through. So no database with a file may be attached, a pragma runs only if it's one of
    /// `std.sqlite`'s own, and SQLite's defensive mode is on, with the schema not trusted to
    /// call functions or views that could do the same.
    fn kept_to_its_file(conn: &mut Connection) -> rusqlite::Result<()> {
        // One, for the database of no file a plain `VACUUM` attaches while it works. The
        // authorizer below refuses any that names a file.
        conn.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 1)?;
        conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
        conn.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)?;
        conn.authorizer(Some(|context: AuthContext<'_>| match context.action {
            // A plain `VACUUM` attaches a database of no file, which reaches nothing; one with
            // a file, as `ATTACH` or `VACUUM INTO` gives, could reach any.
            AuthAction::Attach { filename: "" } => Authorization::Allow,
            AuthAction::Attach { .. } | AuthAction::Detach { .. } => Authorization::Deny,
            AuthAction::Pragma { pragma_name, .. } if !OWN_PRAGMAS.contains(&pragma_name) => {
                Authorization::Deny
            }
            _ => Authorization::Allow,
        }))
    }

    fn open(place: Place) -> Result<i64, String> {
        let first = place.clone();
        let main = blocking(move || connect(&first).map_err(|err| err.to_string()))?;
        let opened = Opened { place, main: Arc::new(Mutex::new(main)), idle: Mutex::new(Vec::new()) };
        Ok(kept(Entry::Database(Arc::new(opened))))
    }

    /// `fs._sqlite_open(path)` on the real disk: the handle of the database in the file at `path`.
    pub(crate) fn open_file(path: &str) -> Value {
        match open(Place::File(path.to_owned())) {
            Ok(handle) => Value::Ok(Rc::new(held("Database", handle))),
            Err(reason) => io_error(&reason),
        }
    }

    /// `fs._sqlite_open(path)` in a test: the test's own database for `path`, by the path's key
    /// in its fake file system.
    pub(crate) fn open_fake(fakes: &mut Fakes, key: &str) -> Value {
        let file = match fakes.file(key) {
            Ok(file) => file,
            Err(err) => return io_error(&err.to_string()),
        };
        match open(Place::Test(file.to_string_lossy().into_owned())) {
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
