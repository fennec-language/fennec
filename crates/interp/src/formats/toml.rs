//! Reading TOML documents: key/value pairs, dotted keys, `[table]` headers and `[[array]]`
//! headers. Tables become dicts and arrays of tables become lists of dicts. Each key and
//! table can be defined only once, as TOML requires.

use std::rc::Rc;

use super::{Cursor, Failure, Parsed, toml_value};
use crate::value::Value;

/// What a key holds while the document is read.
enum Item {
    Value(Value),
    Table(Table),
    /// An array of tables, from `[[name]]` headers.
    Tables(Vec<Table>),
}

/// A table while the document is read, with how it came to exist.
#[derive(Default)]
pub(super) struct Table {
    entries: Vec<(String, Item)>,
    /// Defined by its own `[name]` header.
    header: bool,
    /// Made by a dotted key like `a.b = 1`.
    dotted: bool,
}

impl Table {
    fn get_mut(&mut self, key: &str) -> Option<&mut Item> {
        self.entries.iter_mut().find(|(name, _)| name == key).map(|(_, item)| item)
    }

    /// The table under `key`, made if it's missing, for a header or dotted key to go into.
    fn open(&mut self, key: &str, dotted: bool, at: usize) -> Parsed<&mut Table> {
        if self.get_mut(key).is_none() {
            self.entries.push((key.to_owned(), Item::Table(Table { dotted, ..Table::default() })));
        }
        match self.get_mut(key) {
            Some(Item::Table(table)) if !dotted || table.dotted => Ok(table),
            Some(Item::Tables(tables)) if !dotted => {
                tables.last_mut().ok_or(Failure { message: format!("`{key}` is empty"), at })
            }
            Some(Item::Table(_) | Item::Tables(_)) => Err(Failure {
                message: format!("`{key}` is a table defined elsewhere, so a dotted key can't add to it"),
                at,
            }),
            Some(Item::Value(_)) | None => {
                Err(Failure { message: format!("`{key}` already has a value, so it can't hold a table"), at })
            }
        }
    }

    /// Adds `keys = value`, making the tables that dotted keys name.
    pub(super) fn insert(&mut self, keys: &[String], value: Value, at: usize) -> Parsed<()> {
        let Some((last, parents)) = keys.split_last() else {
            return Err(Failure { message: "expected a key".to_owned(), at });
        };
        let mut table = self;
        for key in parents {
            table = table.open(key, true, at)?;
        }
        if table.get_mut(last).is_some() {
            return Err(Failure { message: format!("`{}` is defined twice", keys.join(".")), at });
        }
        table.entries.push((last.clone(), Item::Value(value)));
        Ok(())
    }

    pub(super) fn into_value(self) -> Value {
        let pairs = self
            .entries
            .into_iter()
            .map(|(key, item)| {
                let value = match item {
                    Item::Value(value) => value,
                    Item::Table(table) => table.into_value(),
                    Item::Tables(tables) => Value::list(tables.into_iter().map(Table::into_value).collect()),
                };
                (Value::str(&key), value)
            })
            .collect();
        Value::Dict(Rc::new(pairs))
    }
}

pub(super) fn parse(text: &str) -> Parsed<Value> {
    let mut cursor = Cursor::new(text);
    let mut root = Table::default();
    let mut current: Vec<String> = Vec::new();
    loop {
        skip_blank_lines(&mut cursor);
        match cursor.peek() {
            None => break,
            Some('[') => current = header(&mut cursor, &mut root)?,
            Some(_) => {
                let at = cursor.at;
                let (keys, value) = key_value(&mut cursor)?;
                let table = descend(&mut root, &current)
                    .ok_or(Failure { message: "this table can't be found again".to_owned(), at })?;
                table.insert(&keys, value, at)?;
            }
        }
        end_of_line(&mut cursor)?;
    }
    Ok(root.into_value())
}

/// The table a header named, found again for the key/value pairs under it.
fn descend<'a>(mut table: &'a mut Table, path: &[String]) -> Option<&'a mut Table> {
    for key in path {
        table = match table.get_mut(key)? {
            Item::Table(table) => table,
            Item::Tables(tables) => tables.last_mut()?,
            Item::Value(_) => return None,
        };
    }
    Some(table)
}

/// `[a.b]` or `[[a.b]]`. Returns the path of the table it starts.
fn header(cursor: &mut Cursor<'_>, root: &mut Table) -> Parsed<Vec<String>> {
    let at = cursor.at;
    cursor.bump();
    let array = cursor.eat('[');
    skip_spaces(cursor);
    let keys = key_path(cursor)?;
    skip_spaces(cursor);
    let closing = if array { "]]" } else { "]" };
    if !cursor.eat_str(closing) {
        return cursor.fail(format!("expected `{closing}` to end the header, but found {}", cursor.found()));
    }
    let Some((last, parents)) = keys.split_last() else { return cursor.fail("expected a key") };
    let mut table = root;
    for key in parents {
        table = table.open(key, false, at)?;
    }
    let name = keys.join(".");
    match table.get_mut(last) {
        None if array => table.entries.push((last.clone(), Item::Tables(vec![defined()]))),
        None => table.entries.push((last.clone(), Item::Table(defined()))),
        Some(Item::Tables(tables)) if array => tables.push(defined()),
        Some(Item::Table(table)) if !array && !table.header && !table.dotted => table.header = true,
        Some(Item::Table(_)) if !array => {
            return Err(Failure { message: format!("the table `[{name}]` is defined twice"), at });
        }
        Some(_) => {
            return Err(Failure {
                message: format!(
                    "`{name}` is already defined as something else, so it can't be `{}`",
                    if array { format!("[[{name}]]") } else { format!("[{name}]") }
                ),
                at,
            });
        }
    }
    Ok(keys)
}

fn defined() -> Table {
    Table { header: true, ..Table::default() }
}

/// `key = value`, where the key may be dotted.
pub(super) fn key_value(cursor: &mut Cursor<'_>) -> Parsed<(Vec<String>, Value)> {
    let keys = key_path(cursor)?;
    skip_spaces(cursor);
    if !cursor.eat('=') {
        return cursor.fail(format!("expected `=` after the key, but found {}", cursor.found()));
    }
    skip_spaces(cursor);
    let value = toml_value::value(cursor, 0)?;
    Ok((keys, value))
}

/// A key, or several joined by dots: `name`, `"quoted key"`, `kit.version`.
fn key_path(cursor: &mut Cursor<'_>) -> Parsed<Vec<String>> {
    let mut keys = Vec::new();
    loop {
        keys.push(simple_key(cursor)?);
        skip_spaces(cursor);
        if !cursor.eat('.') {
            return Ok(keys);
        }
        skip_spaces(cursor);
    }
}

fn simple_key(cursor: &mut Cursor<'_>) -> Parsed<String> {
    match cursor.peek() {
        Some('"') => toml_value::basic_string(cursor),
        Some('\'') => toml_value::literal_string(cursor),
        _ => {
            let key = cursor.take_while(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
            if key.is_empty() {
                return cursor.fail(format!("expected a key, but found {}", cursor.found()));
            }
            Ok(key.to_owned())
        }
    }
}

fn skip_spaces(cursor: &mut Cursor<'_>) {
    cursor.take_while(|c| c == ' ' || c == '\t');
}

fn skip_comment(cursor: &mut Cursor<'_>) {
    if cursor.peek() == Some('#') {
        cursor.take_while(|c| c != '\n');
    }
}

/// Skips spaces, line breaks and comments, as between the items of a multi-line array.
pub(super) fn skip_blank_lines(cursor: &mut Cursor<'_>) {
    loop {
        cursor.take_while(char::is_whitespace);
        if cursor.peek() != Some('#') {
            return;
        }
        skip_comment(cursor);
    }
}

/// After a header or key/value pair, only spaces and a comment can come before the line ends.
fn end_of_line(cursor: &mut Cursor<'_>) -> Parsed<()> {
    skip_spaces(cursor);
    skip_comment(cursor);
    cursor.eat('\r');
    if cursor.peek().is_none() || cursor.eat('\n') {
        return Ok(());
    }
    cursor.fail(format!("expected the end of the line, but found {}", cursor.found()))
}

#[cfg(test)]
mod tests {
    use super::parse;
    use crate::format::repr;

    fn read(text: &str) -> String {
        match parse(text) {
            Ok(value) => repr(&value),
            Err(failure) => format!("{} @ {}", failure.message, failure.at),
        }
    }

    #[test]
    fn reads_tables_and_arrays_of_tables() {
        let text = "title = 'x' # note\n[a.b]\nc = 1\n[a]\nd.e = 2\n[[f]]\ng = 1\n[[f]]\n[f.h]\ni = 3\n";
        assert_eq!(
            read(text),
            r#"{"title": "x", "a": {"b": {"c": 1}, "d": {"e": 2}}, "f": [{"g": 1}, {"h": {"i": 3}}]}"#
        );
        assert_eq!(read("\"quoted key\" = 1\n'lit' = 2\n"), r#"{"quoted key": 1, "lit": 2}"#);
    }

    #[test]
    fn rejects_defining_things_twice() {
        assert_eq!(read("a = 1\na = 2"), "`a` is defined twice @ 6");
        assert_eq!(read("[a]\n[a]"), "the table `[a]` is defined twice @ 4");
        assert_eq!(read("a = 1\n[a]"), "`a` is already defined as something else, so it can't be `[a]` @ 6");
        assert_eq!(read("[a]\nb.c = 1\n[a.b]"), "the table `[a.b]` is defined twice @ 12");
        assert_eq!(read("a = {b = 1}\n[a.c]"), "`a` already has a value, so it can't hold a table @ 12");
        assert_eq!(
            read("[a]\n[[a]]"),
            "`a` is already defined as something else, so it can't be `[[a]]` @ 4"
        );
    }

    #[test]
    fn says_where_bad_lines_go_wrong() {
        assert_eq!(read("a = 1 b = 2"), "expected the end of the line, but found `b` @ 6");
        assert_eq!(read("a 1"), "expected `=` after the key, but found `1` @ 2");
        assert_eq!(read("[a"), "expected `]` to end the header, but found the end of the text @ 2");
        assert_eq!(read("= 1"), "expected a key, but found `=` @ 0");
    }
}
