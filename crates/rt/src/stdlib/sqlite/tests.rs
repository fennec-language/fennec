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
    let db = handle_of(&open_fake(&mut fakes, "index.db"))?;
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
    let again = handle_of(&open_fake(&mut fakes, "index.db"))?;
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
    let db = handle_of(&open_fake(&mut fakes, "rules.db"))?;
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
    let other = handle_of(&open_fake(&mut others, "rules.db"))?;
    assert!(
        run(other, "_execute", &query("insert into keys values (?)", &[], &["b"]))?.contains("no such table")
    );
    Ok(())
}

#[test]
fn a_tests_databases_are_files_of_its_own_gone_when_it_ends() -> Result<(), String> {
    let mut fakes = Fakes::default();
    let db = handle_of(&open_fake(&mut fakes, "gone.db"))?;
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
    let db = handle_of(&open_fake(&mut fakes, "open.db"))?;
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
    assert!(super::real::still_the_file(&conn, &path_text, noted));
    std::fs::rename(&path, folder.join("moved.db")).unwrap();
    std::fs::write(&path, b"").unwrap();
    assert!(!super::real::still_the_file(&conn, &path_text, noted));
    drop(conn);
    let _ = std::fs::remove_dir_all(&folder);
}
