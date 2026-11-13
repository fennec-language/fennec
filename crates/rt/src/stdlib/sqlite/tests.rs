//! SQLite's side of `std.sqlite`, by the handles sqlite.fen holds: a test's database in memory,
//! kept while the test lasts, a query's values bound by kind, rows given back cell by cell, and
//! transactions that keep what they did or none of it.

use fennec_syntax::Span;

use super::{Fakes, call, open_fake};
use crate::value::Value;

/// A query as sqlite.fen hands it over: its text, then its values' kinds and each kind's list.
fn query(text: &str, ints: &[i64], texts: &[&str]) -> Vec<Value> {
    let kinds = ints.iter().map(|_| Value::Int(0)).chain(texts.iter().map(|_| Value::Int(2)));
    vec![
        Value::str(text),
        Value::list(kinds.collect()),
        Value::list(ints.iter().map(|n| Value::Int(*n)).collect()),
        Value::list(Vec::new()),
        Value::list(texts.iter().map(|text| Value::str(text)).collect()),
        Value::list(Vec::new()),
    ]
}

/// The handle a `Database` or `Transaction` holds, or what was given back instead.
fn handle_of(value: &Value) -> Result<i64, String> {
    if let Value::Ok(made) = value
        && let Value::Instance(made) = made.as_ref()
        && let Some(Value::Int(handle)) = made.field("_handle")
    {
        return Ok(*handle);
    }
    Err(crate::format::repr(value))
}

fn run(handle: i64, method: &str, args: &[Value]) -> Result<String, String> {
    call(handle, method, args, Span::default())
        .map(|done| crate::format::repr(&done))
        .map_err(|problem| problem.message)
}

#[test]
fn a_tests_database_keeps_what_its_transactions_commit_and_none_of_what_they_roll_back() -> Result<(), String>
{
    let mut fakes = Fakes::default();
    let db = handle_of(&open_fake(&mut fakes, "index.db", false))?;
    assert_eq!(run(db, "_execute", &query("create table notes (n integer, text text)", &[], &[]))?, "Ok(0)");
    assert_eq!(run(db, "_execute", &query("insert into notes values (?, ?)", &[1], &["fox"]))?, "Ok(1)");
    let kept = handle_of(&call(db, "_begin", &[], Span::default()).map_err(|problem| problem.message)?)?;
    assert_eq!(run(kept, "_execute", &query("insert into notes values (?, ?)", &[2], &["owl"]))?, "Ok(1)");
    assert_eq!(run(kept, "_commit", &[])?, "Ok()");
    let undone = handle_of(&call(db, "_begin", &[], Span::default()).map_err(|problem| problem.message)?)?;
    assert_eq!(run(undone, "_execute", &query("delete from notes", &[], &[]))?, "Ok(2)");
    assert_eq!(run(undone, "_rollback", &[])?, "");
    // Closed and opened again, it's the same database while the test lasts.
    assert_eq!(run(db, "_close", &[])?, "");
    let again = handle_of(&open_fake(&mut fakes, "index.db", false))?;
    let found = run(again, "_query", &query("select n, text from notes order by n", &[], &[]))?;
    assert!(found.contains(r#"columns=["n", "text"]"#), "{found}");
    assert!(found.contains("kinds=[1, 3, 1, 3]") && found.contains(r#"texts=["fox", "owl"]"#), "{found}");
    // A finished transaction's handle is gone.
    assert!(run(kept, "_execute", &query("select 1", &[], &[]))?.contains("was closed"));
    Ok(())
}

#[test]
fn a_broken_rule_and_a_query_in_two_statements_fail_as_what_they_are() -> Result<(), String> {
    let mut fakes = Fakes::default();
    let db = handle_of(&open_fake(&mut fakes, "rules.db", false))?;
    assert_eq!(run(db, "_execute", &query("create table keys (k text primary key)", &[], &[]))?, "Ok(0)");
    assert_eq!(run(db, "_execute", &query("insert into keys values (?)", &[], &["a"]))?, "Ok(1)");
    let taken = run(db, "_execute", &query("insert into keys values (?)", &[], &["a"]))?;
    assert!(taken.contains("Constraint") && taken.contains("UNIQUE"), "{taken}");
    let two = run(db, "_execute", &query("select 1; select 2", &[], &[]))?;
    assert!(two.contains("one statement"), "{two}");
    // A `?` written in the query's own text has no value to take.
    let extra = run(db, "_execute", &query("insert into keys values (?)", &[], &[]))?;
    assert!(extra.contains("places for them"), "{extra}");
    // A test's databases are its own.
    let mut others = Fakes::default();
    let other = handle_of(&open_fake(&mut others, "rules.db", false))?;
    assert!(
        run(other, "_execute", &query("insert into keys values (?)", &[], &["b"]))?.contains("no such table")
    );
    Ok(())
}

#[test]
fn a_tests_databases_are_files_of_its_own_gone_when_it_ends() -> Result<(), String> {
    let mut fakes = Fakes::default();
    let db = handle_of(&open_fake(&mut fakes, "gone.db", false))?;
    assert_eq!(run(db, "_execute", &query("create table notes (n integer)", &[], &[]))?, "Ok(0)");
    let folder = fakes.folder.clone().ok_or("no folder was made")?;
    assert!(folder.join("0.db").exists());
    assert_eq!(run(db, "_close", &[])?, "");
    drop(fakes);
    assert!(!folder.exists(), "{} is left", folder.display());
    Ok(())
}

/// A database the test never closed, and a transaction it never finished, are closed when the
/// test ends, so its folder goes even where an open file can't be removed.
#[test]
fn a_tests_databases_left_open_are_closed_and_gone_when_it_ends() -> Result<(), String> {
    let mut fakes = Fakes::default();
    let db = handle_of(&open_fake(&mut fakes, "open.db", false))?;
    assert_eq!(run(db, "_execute", &query("create table notes (n integer)", &[], &[]))?, "Ok(0)");
    let began = run(db, "_begin", &[])?;
    assert!(began.starts_with("Ok("), "{began}");
    let folder = fakes.folder.clone().ok_or("no folder was made")?;
    drop(fakes);
    assert!(!folder.exists(), "{} is left", folder.display());
    let after = run(db, "_execute", &query("select 1", &[], &[]))?;
    assert!(after.contains("was closed"), "{after}");
    Ok(())
}

/// A database opened for a narrowed `Fs` is kept only while SQLite's file is the one the narrowed
/// `Fs` found: once another file is swapped in at its path, SQLite and the path both say so.
#[cfg(unix)]
#[test]
fn a_database_whose_file_was_swapped_is_told_apart() {
    use std::os::unix::fs::MetadataExt as _;
    let folder = std::env::temp_dir().join(format!("fennec-sqlite-swapped-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("data.db");
    let path_text = path.to_string_lossy().into_owned();
    std::fs::write(&path, b"").unwrap();
    let found = std::fs::metadata(&path).unwrap();
    let noted = (found.dev(), found.ino());
    let conn = rusqlite::Connection::open(&path).unwrap();
    assert!(super::real::still_the_file(&conn, &path_text, &noted));
    std::fs::rename(&path, folder.join("moved.db")).unwrap();
    std::fs::write(&path, b"").unwrap();
    assert!(!super::real::still_the_file(&conn, &path_text, &noted));
    drop(conn);
    let _ = std::fs::remove_dir_all(&folder);
}

/// A database a narrowed `Fs` opens is opened with no link in its path and never made by SQLite:
/// a link swapped in on the way is refused, and a path that names no file makes none.
#[cfg(unix)]
#[test]
fn a_checked_database_is_never_made_and_never_reached_through_a_link() {
    use std::os::unix::fs::MetadataExt as _;
    let folder = std::env::temp_dir().join(format!("fennec-sqlite-nofollow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(folder.join("real")).unwrap();
    let folder = std::fs::canonicalize(&folder).unwrap();
    let file = folder.join("real").join("data.db");
    std::fs::write(&file, b"").unwrap();
    let found = std::fs::metadata(&file).unwrap();
    let noted = (found.dev(), found.ino());
    // The same file, reached through a link to its folder.
    std::os::unix::fs::symlink(folder.join("real"), folder.join("linked")).unwrap();
    let through_link = folder.join("linked").join("data.db").to_string_lossy().into_owned();
    let refused = crate::format::repr(&super::open_checked(&through_link, noted, false));
    assert!(refused.contains("IoError"), "{refused}");
    // A path that names no file: SQLite makes none.
    let missing = folder.join("real").join("missing.db");
    let refused = crate::format::repr(&super::open_checked(&missing.to_string_lossy(), noted, false));
    assert!(refused.contains("IoError"), "{refused}");
    assert!(!missing.exists(), "SQLite made a file the narrowed Fs didn't");
    // By its own path, with no link in it, it opens.
    let opened = super::open_checked(&file.to_string_lossy(), noted, false);
    assert!(handle_of(&opened).is_ok(), "{}", crate::format::repr(&opened));
    let _ = std::fs::remove_dir_all(&folder);
}

/// A `Database` reaches only its own file. A query's text is the program's, so without a guard
/// it could `ATTACH` any file the process can open, write a copy of itself anywhere with
/// `VACUUM INTO`, or move SQLite's temporary files with a pragma: each fails with `SqlError`,
/// and no file appears. The database itself still works, its own pragmas included.
#[test]
fn a_database_cant_attach_or_vacuum_into_another_file() -> Result<(), String> {
    let folder = std::env::temp_dir().join(format!("fennec-sqlite-reach-{}", std::process::id()));
    std::fs::create_dir_all(&folder).map_err(|err| err.to_string())?;
    let file = |name: &str| folder.join(name).to_string_lossy().into_owned();
    let db = handle_of(&super::open_file(&file("own.db"), false))?;
    assert_eq!(run(db, "_execute", &query("create table notes (n integer)", &[], &[]))?, "Ok(0)");
    assert_eq!(run(db, "_execute", &query("insert into notes values (?)", &[7], &[]))?, "Ok(1)");
    let attach = format!("attach database '{}' as other", file("other.db"));
    let vacuum = format!("vacuum into '{}'", file("copy.db"));
    let temp = format!("pragma temp_store_directory = '{}'", folder.display());
    let second = format!("attach database '{}' as second", file("own.db"));
    for text in
        [attach.as_str(), second.as_str(), vacuum.as_str(), temp.as_str(), "pragma writable_schema = on"]
    {
        let said = run(db, "_execute", &query(text, &[], &[]))?;
        assert!(said.contains("SqlError"), "{text}: {said}");
    }
    assert!(!std::path::Path::new(&file("other.db")).exists(), "an attached file was made");
    assert!(!std::path::Path::new(&file("copy.db")).exists(), "a vacuumed copy was made");
    let found = run(db, "_query", &query("select n from notes", &[], &[]))?;
    assert!(found.contains("ints=[7]"), "{found}");
    // Its own file can still be vacuumed.
    let vacuumed = run(db, "_execute", &query("vacuum", &[], &[]))?;
    assert!(vacuumed.starts_with("Ok("), "{vacuumed}");
    assert_eq!(run(db, "_close", &[])?, "");
    std::fs::remove_dir_all(&folder).map_err(|err| err.to_string())?;
    Ok(())
}

/// The one attach a connection allows is for the database of no file a plain `VACUUM` makes, and
/// it can't be stretched: a user's own `ATTACH ''` gets a database of no file and makes none, and
/// every other way of naming one is refused by what it is, not by the count. A name worked out by
/// an expression is the way to watch, since SQLite's authorizer isn't told what it comes to.
#[test]
fn the_attach_left_for_vacuum_reaches_no_file() -> Result<(), String> {
    let folder = std::env::temp_dir().join(format!("fennec-sqlite-stretch-{}", std::process::id()));
    std::fs::create_dir_all(&folder).map_err(|err| err.to_string())?;
    let file = |name: &str| folder.join(name).to_string_lossy().into_owned();
    let other = file("other.db");
    let refused = [
        ("attach database ':memory:' as m".to_owned(), Vec::new()),
        (format!("attach database 'file:{other}?mode=rwc' as u"), Vec::new()),
        (format!("attach database '{other}?mode=rwc' as q"), Vec::new()),
        (format!("attach database '' || '{other}' as e"), Vec::new()),
        (format!("attach database printf('%s', '{other}') as p"), Vec::new()),
        ("attach database ? as b".to_owned(), vec![other.as_str()]),
    ];
    // Each on a connection of its own, so the one attach a connection allows is still free, and
    // what refuses it is that it names something.
    for (at, (text, given)) in refused.iter().enumerate() {
        let db = handle_of(&super::open_file(&file(&format!("own{at}.db")), false))?;
        let said = run(db, "_execute", &query(text, &[], given))?;
        assert!(said.contains("not authorized"), "{text}: {said}");
        assert_eq!(run(db, "_close", &[])?, "");
    }
    // Its own `ATTACH ''` is a database of no file, which makes none, and tables go only in the
    // database it opened, not in that one.
    let db = handle_of(&super::open_file(&file("own.db"), false))?;
    assert_eq!(run(db, "_execute", &query("attach database '' as scratch", &[], &[]))?, "Ok(0)");
    let made = run(db, "_execute", &query("create table scratch.notes (n integer)", &[], &[]))?;
    assert!(made.contains("not authorized"), "{made}");
    assert_eq!(run(db, "_close", &[])?, "");
    let made: Vec<String> = std::fs::read_dir(&folder)
        .map_err(|err| err.to_string())?
        .filter_map(|entry| entry.ok().map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .filter(|name| !name.starts_with("own"))
        .collect();
    assert!(made.is_empty(), "files were made: {made:?}");
    std::fs::remove_dir_all(&folder).map_err(|err| err.to_string())?;
    Ok(())
}

/// A connection takes only what ordinary queries need, so an action nobody looked at is refused,
/// rare ones included, while tables, indexes, migrations, transactions and recursive queries work.
#[test]
fn a_database_takes_only_the_actions_ordinary_queries_need() -> Result<(), String> {
    let mut fakes = Fakes::default();
    let db = handle_of(&open_fake(&mut fakes, "only.db", false))?;
    let works = [
        "create table notes (n integer, text text)",
        "create index by_n on notes (n)",
        "alter table notes add column kept integer",
        "insert into notes (n, text) values (1, 'fox')",
        "update notes set text = upper(text) where n = 1",
        "savepoint here",
        "release here",
        "with recursive count(n) as (select 1 union all select n + 1 from count where n < 3) select n from count",
        "delete from notes where n = 2",
        "reindex notes",
        "drop index by_n",
        "create temp table scratch (n integer)",
        "create temp view scratch_seen as select n from scratch",
        "create temp trigger scratch_kept after insert on scratch begin select 1; end",
        "insert into scratch (n) values (1)",
        "drop trigger scratch_kept",
        "drop view scratch_seen",
        "drop table scratch",
    ];
    for text in works {
        let said = run(db, "_execute", &query(text, &[], &[]))?;
        assert!(said.starts_with("Ok("), "{text}: {said}");
    }
    let refused = [
        "attach database 'other.db' as other",
        "attach database ':memory:' as other",
        "vacuum into 'copy.db'",
        "analyze",
        "pragma foreign_keys = on",
        "pragma table_info(notes)",
        "pragma main.user_version = 3",
        "pragma temp.user_version = 3",
        "pragma writable_schema = on",
        "pragma temp_store_directory = '/tmp'",
        "detach database main",
        "detach database temp",
        // Refused by SQLite's own default, extension loading being off, and by the allow-list's
        // refusal of a virtual table, so a later rusqlite feature can't open either.
        "select load_extension('x')",
        "create virtual table found using fts5(text)",
    ];
    for text in refused {
        let said = run(db, "_execute", &query(text, &[], &[]))?;
        // `VACUUM INTO` is refused as it attaches, which SQLite words as "authorization denied".
        assert!(said.contains("not authorized") || said.contains("authorization denied"), "{text}: {said}");
    }
    Ok(())
}

#[test]
fn a_view_and_a_trigger_in_main_work() -> Result<(), String> {
    let mut fakes = Fakes::default();
    let db = handle_of(&open_fake(&mut fakes, "kept.db", false))?;
    for text in [
        "create table notes (n integer)",
        "create table counted (n integer)",
        "create trigger count_notes after insert on notes begin insert into counted (n) values (new.n); end",
        "create view big as select n from notes where n > 1",
        "insert into notes (n) values (1)",
        "insert into notes (n) values (2)",
    ] {
        let said = run(db, "_execute", &query(text, &[], &[]))?;
        assert!(said.starts_with("Ok("), "{text}: {said}");
    }
    let counted = run(db, "_query", &query("select n from counted order by n", &[], &[]))?;
    assert!(counted.contains("ints=[1, 2]"), "{counted}");
    let big = run(db, "_query", &query("select n from big", &[], &[]))?;
    assert!(big.contains("ints=[2]"), "{big}");
    for text in ["drop view big", "drop trigger count_notes"] {
        let said = run(db, "_execute", &query(text, &[], &[]))?;
        assert!(said.starts_with("Ok("), "{text}: {said}");
    }
    Ok(())
}

/// A test's databases go in a folder made new for it, named at random, so one a crashed run left
/// behind, with its databases in it, is never taken for this test's.
#[test]
fn each_test_s_databases_go_in_a_new_folder_of_its_own() -> Result<(), String> {
    let (mut first, mut second) = (Fakes::default(), Fakes::default());
    handle_of(&open_fake(&mut first, "same.db", false))?;
    handle_of(&open_fake(&mut second, "same.db", false))?;
    let (Some(one), Some(two)) = (first.folder.clone(), second.folder.clone()) else {
        return Err("a test opened a database with no folder for it".to_owned());
    };
    assert_ne!(one, two);
    for folder in [&one, &two] {
        let name = folder.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        let random = name.strip_prefix("fennec-test-databases-").unwrap_or_default();
        assert!(random.len() == 16 && random.chars().all(|c| c.is_ascii_hexdigit()), "{name}");
    }
    drop((first, second));
    assert!(!one.exists() && !two.exists(), "a test's folder outlived it");
    Ok(())
}
