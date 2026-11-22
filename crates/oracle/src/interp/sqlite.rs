//! `std.sqlite`'s methods written in Rust, beneath its Fennec, which the oracle runs from source
//! with `std.sql`'s: opening a database, running a query on it or on a transaction, beginning,
//! keeping and undoing a transaction, and closing the database when its `with` block ends.
//!
//! SQLite itself answers each query, as it does in the compiled program, so what it makes of
//! the query's text, the rows it finds and its own words for what it refuses are SQLite's. What
//! is written here, on its own, from `std.sqlite`'s and `std.sql`'s documentation, is what the
//! runtime does around SQLite: a query is one statement with a place for each value, a NaN is
//! never stored, a query run through `execute` that gives rows changes none, each row's cells go
//! into `SqlRows` by kind, and each failure is the case of `SqlError` it says it is. A database
//! is a file of the oracle's own, in a folder of its own that's gone when the run ends, and the
//! modeled disk holds it at the program's path, where nothing else may use it.
//!
//! What the oracle can't promise is refused: a pragma, a view or trigger, a temporary table,
//! `ATTACH` and anything else past ordinary queries on the database's own tables; SQLite's
//! random and time functions; a database through an `Fs` narrowed with `only` or `read_only`;
//! and a query that waits on another writer while something else runs beside it, which may
//! finish in time, or not.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use fennec_syntax::Span;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::types::{Value as Cell, ValueRef};
use rusqlite::{Connection, ErrorCode, OpenFlags};

use super::{Flow, Interp, Run, unsupported};
use crate::value::{Instance, Text, Value};

/// How long the runtime lets a query wait for another writer before it fails with `Busy`.
const BUSY_SECONDS: u64 = 5;

/// SQLite's functions whose answers hang on chance or on the time.
const CHANCE_AND_TIME: &[&str] =
    &["random", "randomblob", "date", "time", "datetime", "julianday", "strftime", "unixepoch", "timediff"];

/// The databases a run has open, by handle, and the file the oracle keeps each in.
#[derive(Default)]
pub(crate) struct Databases {
    /// The oracle's own folder for them, made when the first is opened.
    folder: Option<PathBuf>,
    /// Each database's file, by its absolute path as the program named it.
    files: BTreeMap<Vec<String>, PathBuf>,
    /// Each open database's connection and each transaction's, by handle, with the path its
    /// database is at.
    open: BTreeMap<i64, (Vec<String>, Connection)>,
    /// The handle the next database or transaction gets.
    next: i64,
    /// Whether a query asked for what the oracle doesn't model since this was last cleared:
    /// SQLite says so in words of its own, some in the words of an ordinary failure.
    denied: Arc<AtomicBool>,
}

impl Drop for Databases {
    fn drop(&mut self) {
        // Closed before their folder goes.
        self.open.clear();
        if let Some(folder) = self.folder.take() {
            let _ = std::fs::remove_dir_all(folder);
        }
    }
}

/// Why a call on a database didn't give what it was asked.
enum Trouble {
    /// It fails with this case of `SqlError`, with these fields.
    Fails(&'static str, Vec<(&'static str, String)>),
    /// It waited on another writer.
    Busy,
    /// It reaches what the oracle doesn't model.
    Refused(String),
}

impl From<rusqlite::Error> for Trouble {
    fn from(err: rusqlite::Error) -> Trouble {
        if matches!(err, rusqlite::Error::MultipleStatement) {
            return failed("a query is one statement: give each its own `execute`");
        }
        match err.sqlite_error_code() {
            Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => Trouble::Busy,
            Some(ErrorCode::ConstraintViolation) => {
                Trouble::Fails("Constraint", vec![("reason", err.to_string())])
            }
            Some(ErrorCode::AuthorizationForStatementDenied) => Trouble::Refused(
                "a query past ordinary ones on a database's own tables, or a function of chance or time"
                    .to_owned(),
            ),
            _ => failed(&err.to_string()),
        }
    }
}

fn failed(reason: &str) -> Trouble {
    Trouble::Fails("Failed", vec![("reason", reason.to_owned())])
}

type Answer<T> = Result<T, Trouble>;

/// A query as `std.sqlite` hands it over: its text, with a `?` for each value, and the values.
struct Query {
    text: String,
    values: Vec<Cell>,
}

impl Databases {
    /// A new file for the database at `path`, or the one it's in already, with a connection
    /// to it.
    fn connect(&mut self, path: &[String]) -> Answer<Connection> {
        let file = if let Some(file) = self.files.get(path) {
            file.clone()
        } else {
            if self.folder.is_none() {
                self.folder = Some(fresh_folder().map_err(|err| Trouble::Refused(err.to_string()))?);
            }
            let folder = self.folder.clone().unwrap_or_default();
            let file = folder.join(format!("{}.db", self.files.len()));
            self.files.insert(path.to_vec(), file.clone());
            file
        };
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE;
        let mut conn = Connection::open_with_flags(&file, flags)?;
        // A database here is in WAL mode, as the runtime opens each: a query outside a
        // transaction reads what was kept before it.
        let _: String = conn.query_row("pragma journal_mode=wal", [], |row| row.get(0))?;
        conn.busy_timeout(std::time::Duration::ZERO)?;
        let denied = Arc::clone(&self.denied);
        conn.authorizer(Some(move |context: AuthContext<'_>| {
            if modeled(&context) {
                return Authorization::Allow;
            }
            denied.store(true, Ordering::SeqCst);
            Authorization::Deny
        }))?;
        Ok(conn)
    }

    fn keep(&mut self, path: Vec<String>, conn: Connection) -> i64 {
        self.next += 1;
        self.open.insert(self.next, (path, conn));
        self.next
    }
}

/// What a query may do for the oracle to model it: read and change the database's own tables,
/// make and drop them and their indexes, and call SQLite's functions but those of chance and
/// time. The runtime allows a little more, which the oracle refuses.
fn modeled(context: &AuthContext<'_>) -> bool {
    let in_main = context.database_name.is_none_or(|name| name == "main");
    match context.action {
        AuthAction::Select
        | AuthAction::Read { .. }
        | AuthAction::Insert { .. }
        | AuthAction::Update { .. }
        | AuthAction::Delete { .. }
        | AuthAction::Transaction { .. }
        | AuthAction::Recursive => true,
        AuthAction::Function { function_name } => {
            !CHANCE_AND_TIME.contains(&function_name.to_lowercase().as_str())
        }
        AuthAction::CreateTable { .. }
        | AuthAction::CreateIndex { .. }
        | AuthAction::DropTable { .. }
        | AuthAction::DropIndex { .. } => in_main,
        _ => false,
    }
}

/// A folder of the oracle's own for its databases, made new.
fn fresh_folder() -> std::io::Result<PathBuf> {
    let started = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    for tried in 0_u32..1000 {
        let name = format!("fennec-oracle-databases-{}-{}-{tried}", std::process::id(), started.as_nanos());
        let made = std::env::temp_dir().join(name);
        match std::fs::create_dir(&made) {
            Ok(()) => return Ok(made),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }
    Err(std::io::Error::other("no fresh folder for the databases"))
}

/// The statement a query's text makes, its values bound, failing as the runtime does where the
/// query is more than one statement, has a different number of places than values, or holds a
/// NaN.
fn prepared<'c>(conn: &'c Connection, query: &Query) -> Answer<rusqlite::Statement<'c>> {
    let mut statement = conn.prepare(&query.text)?;
    let places = statement.parameter_count();
    if places != query.values.len() {
        return Err(failed(&format!(
            "the query has {} values but {places} places for them: a `?` or `:name` in its text is one",
            query.values.len()
        )));
    }
    if query.values.iter().any(|value| matches!(value, Cell::Real(x) if x.is_nan())) {
        return Err(failed("NaN can't be stored: SQLite would turn it into NULL"));
    }
    for (at, value) in query.values.iter().enumerate() {
        statement.raw_bind_parameter(at + 1, value)?;
    }
    Ok(statement)
}

/// How many rows a query run through `execute` changed: none for one that gives rows, which are
/// read through and dropped.
fn executed(conn: &Connection, query: &Query) -> Answer<i64> {
    let mut statement = prepared(conn, query)?;
    if statement.column_count() > 0 {
        let mut rows = statement.raw_query();
        while rows.next()?.is_some() {}
        return Ok(0);
    }
    Ok(i64::try_from(statement.raw_execute()?).unwrap_or(i64::MAX))
}

/// What a query found, as `SqlRows`: each row's cells in column order, each by its kind, 0 for
/// null, 1 an int, 2 a float, 3 text and 4 bytes, and where it is in the list of its kind.
fn found(conn: &Connection, query: &Query) -> Answer<Value> {
    let mut statement = prepared(conn, query)?;
    let columns: Vec<String> = statement.column_names().map(str::to_owned).collect();
    let (mut kinds, mut at) = (Vec::new(), Vec::new());
    let (mut ints, mut floats, mut texts, mut blobs) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut rows = statement.raw_query();
    while let Some(row) = rows.next()? {
        for (column, name) in columns.iter().enumerate() {
            let (kind, place) = match row.get_ref(column)? {
                ValueRef::Null => (0, 0),
                ValueRef::Integer(n) => {
                    ints.push(Value::Int(n));
                    (1, ints.len() - 1)
                }
                ValueRef::Real(x) => {
                    floats.push(Value::Float(x));
                    (2, floats.len() - 1)
                }
                ValueRef::Text(text) => {
                    let Ok(text) = std::str::from_utf8(text) else {
                        return Err(Trouble::Fails(
                            "Column",
                            vec![
                                ("name", name.clone()),
                                ("reason", "it holds text that isn't UTF-8".to_owned()),
                            ],
                        ));
                    };
                    texts.push(Value::Str(Text::from(text.to_owned())));
                    (3, texts.len() - 1)
                }
                ValueRef::Blob(data) => {
                    blobs.push(Value::Bytes(data.to_vec()));
                    (4, blobs.len() - 1)
                }
            };
            kinds.push(Value::Int(kind));
            at.push(Value::Int(i64::try_from(place).unwrap_or(i64::MAX)));
        }
    }
    let list = |items: Vec<Value>| Value::List(items.into_iter().collect());
    let names = columns.into_iter().map(|name| Value::Str(Text::from(name))).collect();
    Ok(Value::instance(
        "SqlRows",
        None,
        vec![
            ("columns".to_owned(), list(names)),
            ("kinds".to_owned(), list(kinds)),
            ("at".to_owned(), list(at)),
            ("ints".to_owned(), list(ints)),
            ("floats".to_owned(), list(floats)),
            ("texts".to_owned(), list(texts)),
            ("blobs".to_owned(), list(blobs)),
        ],
    ))
}

/// The items of a list `std.sqlite` hands over.
fn items(value: &Value) -> Option<Vec<Value>> {
    match value {
        Value::List(items) => Some(items.iter().cloned().collect()),
        _ => None,
    }
}

/// The query in the arguments of `_execute` or `_query`: its text, then what kind each value is,
/// 0 for an `int`, 1 a `float`, 2 a `str` and 3 `bytes`, and a list of the values of each kind.
fn query_in(args: &[Value]) -> Option<Query> {
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
                Value::Str(text) => Cell::Text(text.as_str().to_owned()),
                _ => return None,
            },
            Value::Int(3) => match blobs.next()? {
                Value::Bytes(data) => Cell::Blob(data),
                _ => return None,
            },
            _ => return None,
        });
    }
    Some(Query { text: text.as_str().to_owned(), values })
}

/// A `Database` or `Transaction` holding this handle.
fn holding(ty: &str, handle: i64) -> Value {
    Value::instance(ty, None, vec![("_handle".to_owned(), Value::Int(handle))])
}

impl Interp {
    /// `fs._sqlite_open(path)`: the database at the path, made if it isn't there, as a
    /// `Database` holding its handle.
    pub(crate) fn sqlite_open(&mut self, path: Vec<String>, span: Span) -> Run<Value> {
        let handle = self.databases(span, |databases| {
            let conn = databases.connect(&path)?;
            Ok(databases.keep(path, conn))
        })?;
        Ok(holding("Database", handle))
    }

    /// A method of a `Database` or a `Transaction` written in Rust, which `std.sqlite`'s own
    /// methods call. `None` for a method that isn't one.
    pub(crate) fn sqlite_method(
        &mut self,
        instance: &Instance,
        name: &str,
        args: &[Value],
        span: Span,
    ) -> Option<Run<Value>> {
        let ty = instance.ty.as_str();
        let known = matches!(
            (ty, name),
            ("Database" | "Transaction", "_execute" | "_query")
                | ("Database", "_begin" | "_close")
                | ("Transaction", "_commit" | "_rollback")
        );
        if !known {
            return None;
        }
        let Some(Value::Int(handle)) = instance.field("_handle").cloned() else {
            return Some(unsupported(format!("a {ty} without its handle")));
        };
        let query = if matches!(name, "_execute" | "_query") {
            match query_in(args) {
                Some(query) => Some(query),
                None => return Some(unsupported(format!("`{ty}.{name}` with these arguments"))),
            }
        } else {
            None
        };
        Some(self.databases(span, |databases| {
            let Some((path, conn)) = databases.open.get(&handle) else {
                return Err(failed("this database was closed when its `with` block ended"));
            };
            match (name, query) {
                ("_execute", Some(query)) => executed(conn, &query).map(Value::Int),
                ("_query", Some(query)) => found(conn, &query),
                ("_begin", _) => {
                    let path = path.clone();
                    let tx = databases.connect(&path)?;
                    // `immediate` takes the writer now, as the runtime's transactions do.
                    tx.execute_batch("begin immediate")?;
                    Ok(holding("Transaction", databases.keep(path, tx)))
                }
                ("_commit", _) => {
                    let Some((_, conn)) = databases.open.remove(&handle) else { return Ok(Value::Unit) };
                    let kept = conn.execute_batch("commit");
                    // A commit that failed is undone.
                    if !conn.is_autocommit() {
                        let _ = conn.execute_batch("rollback");
                    }
                    kept?;
                    Ok(Value::Unit)
                }
                ("_rollback", _) => {
                    if let Some((_, conn)) = databases.open.remove(&handle) {
                        let _ = conn.execute_batch("rollback");
                    }
                    Ok(Value::Unit)
                }
                _ => {
                    databases.open.remove(&handle);
                    Ok(Value::Unit)
                }
            }
        }))
    }

    /// Works on the databases this run shares, making what it gives into what the program gets.
    fn databases<R>(&self, span: Span, work: impl FnOnce(&mut Databases) -> Answer<R>) -> Run<R> {
        // A database is on the disk, and is used in order with the rest of it.
        self.reach_out(super::parallel::Outside::Disk)?;
        self.outside_in_order(super::parallel::Outside::Disk)?;
        let done = {
            let Ok(mut databases) = self.shared.databases.lock() else {
                return unsupported("the databases were lost");
            };
            databases.denied.store(false, Ordering::SeqCst);
            let done = work(&mut databases);
            if databases.denied.load(Ordering::SeqCst) {
                return unsupported(
                    "a query past ordinary ones on a database's own tables, or a function of chance or time",
                );
            }
            done
        };
        match done {
            Ok(value) => Ok(value),
            Err(Trouble::Fails(case, fields)) => {
                let fields = fields
                    .into_iter()
                    .map(|(name, text)| (name.to_owned(), Value::Str(Text::from(text))))
                    .collect();
                Err(Flow::Fail(Value::instance("SqlError", Some(case), fields), span))
            }
            // The writer is held by a transaction that can't end while this waits, short of
            // something running beside it, so the wait runs out, and the query fails.
            Err(Trouble::Busy) if !self.tasks_beside()? => {
                let reason = format!(
                    "another writer held the database for over {BUSY_SECONDS} seconds; inside a transaction, query through it rather than the database"
                );
                Err(Flow::Fail(
                    Value::instance(
                        "SqlError",
                        Some("Busy"),
                        vec![("reason".to_owned(), Value::Str(Text::from(reason)))],
                    ),
                    span,
                ))
            }
            Err(Trouble::Busy) => {
                unsupported("a query waiting on another writer while something runs beside it")
            }
            Err(Trouble::Refused(why)) => unsupported(why),
        }
    }
}
