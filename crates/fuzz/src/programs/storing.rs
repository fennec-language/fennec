//! `stored{id}(fs, stdout)`: a SQLite database of the program's own, through `std.sqlite`, with
//! one table of rows, each an `id` that's its primary key, a `name` that can't be null and a
//! `size` that can't go below zero. Rows go in, change and go out, some breaking one of the
//! table's rules, which fails with `SqlError.Constraint` and changes nothing; they're read back
//! as structs, as one column and as a sum; and a transaction does some of that, kept when its
//! block ends or undone when it fails, while the database outside it still sees what was there
//! before.
//!
//! What each step prints is worked out here, from a model of the table, never from SQLite.

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// The names a row may have.
const NAMES: [&str; 3] = ["ash", "birch", "cedar"];

/// One thing done to the table, through the database or the transaction it's in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// `insert` a row, its name by place in `NAMES`, or null.
    Insert { key: i64, name: Option<usize>, size: i64 },
    /// `update` each row from `from` on, adding `by` to its size.
    Update { by: i64, from: i64 },
    /// `delete` each row smaller than `below`.
    Delete { below: i64 },
    /// Every row, as a struct, in order.
    Rows,
    /// The sizes of the rows past `above`, as `int`s, in order.
    Sizes { above: i64 },
    /// The sum of the sizes, null for no rows, as an `int?`.
    Sum,
    /// How many rows, always through the database: in a transaction, what was kept before it.
    Count,
    /// A transaction doing these steps, then kept or failing.
    Transaction { steps: Vec<Step>, fails: bool },
}

/// One `Stored` statement.
#[derive(Clone, Debug)]
pub(crate) struct Storing {
    pub(crate) id: usize,
    pub(crate) steps: Vec<Step>,
}

fn name(at: usize) -> &'static str {
    NAMES.get(at).copied().unwrap_or("ash")
}

/// A plain step: none is a transaction.
fn plain(choose: &mut impl FnMut(usize) -> usize) -> Step {
    let small = |n: usize| i64::try_from(n).unwrap_or(0);
    match choose(7) {
        0 | 1 => Step::Insert {
            key: 1 + small(choose(5)),
            name: if choose(8) == 0 { None } else { Some(choose(NAMES.len())) },
            size: small(choose(8)) - 1,
        },
        2 => Step::Update { by: small(choose(7)) - 3, from: 1 + small(choose(5)) },
        3 => Step::Delete { below: small(choose(6)) },
        4 => Step::Rows,
        5 => Step::Sizes { above: small(choose(5)) },
        _ => {
            if choose(2) == 0 {
                Step::Sum
            } else {
                Step::Count
            }
        }
    }
}

/// A `Stored` statement's steps: 3 to 10, a fifth of them transactions of 1 to 4 plain steps,
/// half of those failing.
pub(crate) fn picked(id: usize, choose: &mut impl FnMut(usize) -> usize) -> Storing {
    let mut steps = Vec::new();
    for _ in 0..3 + choose(8) {
        if choose(5) == 0 {
            let inner = (0..=choose(4)).map(|_| plain(choose)).collect();
            steps.push(Step::Transaction { steps: inner, fails: choose(2) == 0 });
        } else {
            steps.push(plain(choose));
        }
    }
    Storing { id, steps }
}

/// The table: each row's name and size, by its id.
type Table = BTreeMap<i64, (&'static str, i64)>;

/// What a step that changes the table makes of it, or the reason it's refused.
fn changed(id: usize, step: &Step, table: &mut Table) -> Result<i64, String> {
    match step {
        Step::Insert { key, name: named, size } => {
            // SQLite checks a column's `not null` and the table's `check` before the key.
            let Some(at) = named else { return Err(format!("NOT NULL constraint failed: t{id}.name")) };
            if *size < 0 {
                return Err("CHECK constraint failed: size >= 0".to_owned());
            }
            if table.contains_key(key) {
                return Err(format!("UNIQUE constraint failed: t{id}.id"));
            }
            table.insert(*key, (name(*at), *size));
            Ok(1)
        }
        Step::Update { by, from } => {
            let hit: Vec<i64> = table.range(*from..).map(|(key, _)| *key).collect();
            // A row the update would take below zero refuses the whole of it.
            if table.range(*from..).any(|(_, (_, size))| size + by < 0) {
                return Err("CHECK constraint failed: size >= 0".to_owned());
            }
            for key in &hit {
                if let Some((_, size)) = table.get_mut(key) {
                    *size += by;
                }
            }
            Ok(i64::try_from(hit.len()).unwrap_or(0))
        }
        Step::Delete { below } => {
            let before = table.len();
            table.retain(|_, (_, size)| *size >= *below);
            Ok(i64::try_from(before - table.len()).unwrap_or(0))
        }
        _ => Ok(0),
    }
}

/// What a step prints, on the table it works on, `kept` being what the database outside a
/// transaction sees.
fn step_printed(id: usize, step: &Step, table: &mut Table, kept: &Table, lines: &mut Vec<String>) {
    let word = match step {
        Step::Insert { .. } => "insert",
        Step::Update { .. } => "update",
        Step::Delete { .. } => "delete",
        Step::Rows => {
            let rows: Vec<String> = table
                .iter()
                .map(|(key, (name, size))| format!("Row{id}(id={key}, name=\"{name}\", size={size})"))
                .collect();
            lines.push(format!("rows [{}]", rows.join(", ")));
            return;
        }
        Step::Sizes { above } => {
            let sizes: Vec<String> =
                table.range(above + 1..).map(|(_, (_, size))| size.to_string()).collect();
            lines.push(format!("sizes [{}]", sizes.join(", ")));
            return;
        }
        Step::Sum => {
            let sum = if table.is_empty() {
                "None".to_owned()
            } else {
                table.values().map(|(_, size)| size).sum::<i64>().to_string()
            };
            lines.push(format!("sum [{sum}]"));
            return;
        }
        Step::Count => {
            lines.push(format!("count [{}]", kept.len()));
            return;
        }
        Step::Transaction { steps, fails } => {
            let mut working = table.clone();
            for step in steps {
                step_printed(id, step, &mut working, table, lines);
            }
            if *fails {
                lines.push("undone undone on purpose".to_owned());
            } else {
                *table = working;
            }
            return;
        }
    };
    match changed(id, step, table) {
        Ok(count) => lines.push(format!("{word} {count}")),
        Err(reason) => {
            lines.push(format!("refused {reason}"));
            lines.push(format!("{word} -1"));
        }
    }
}

/// What `stored{id}` prints.
pub(crate) fn printed(storing: &Storing) -> Vec<String> {
    let mut table = Table::new();
    let mut lines = Vec::new();
    for step in &storing.steps {
        let kept = table.clone();
        step_printed(storing.id, step, &mut table, &kept, &mut lines);
    }
    lines
}

/// The lines doing a step through `on`, `db` or `tx`, at `indent`, its name `at` keeping its
/// locals apart from the other steps'.
fn step_written(id: usize, at: usize, step: &Step, on: &str, indent: &str, out: &mut String) {
    let change = |out: &mut String, word: &str, query: String| {
        let _ = write!(
            out,
            "{indent}changed{at} = {on}.execute(t\"{query}\") except why:\n{indent}    stdout.print(\"refused\", \
             why.message())\n{indent}    -1\n{indent}stdout.print(\"{word}\", changed{at})\n"
        );
    };
    match step {
        Step::Insert { key, name: named, size } => {
            let shown = named.map_or_else(|| "null".to_owned(), |at| format!("{{named{id}({at})}}"));
            change(out, "insert", format!("insert into t{id} values ({key}, {shown}, {size})"));
        }
        Step::Update { by, from } => {
            change(out, "update", format!("update t{id} set size = size + ({by}) where id >= {from}"));
        }
        Step::Delete { below } => change(out, "delete", format!("delete from t{id} where size < {below}")),
        Step::Rows => {
            let _ = writeln!(
                out,
                "{indent}stdout.print(\"rows\", {on}.query[Row{id}](t\"select id, name, size from t{id} order by id\")?)"
            );
        }
        Step::Sizes { above } => {
            let _ = writeln!(
                out,
                "{indent}stdout.print(\"sizes\", {on}.query[int](t\"select size from t{id} where id > {above} order by id\")?)"
            );
        }
        Step::Sum => {
            let _ = writeln!(
                out,
                "{indent}stdout.print(\"sum\", {on}.query[int?](t\"select sum(size) from t{id}\")?)"
            );
        }
        Step::Count => {
            let _ = writeln!(
                out,
                "{indent}stdout.print(\"count\", db.query[int](t\"select count(*) from t{id}\")?)"
            );
        }
        Step::Transaction { .. } => {
            let _ = write!(
                out,
                "{indent}kept{id}_{at}(db, stdout) except why:\n{indent}    stdout.print(\"undone\", why.message())\n"
            );
        }
    }
}

/// The row type, the names, each transaction's function and `stored{id}` itself.
pub(crate) fn written(storing: &Storing) -> String {
    let id = storing.id;
    let names: Vec<String> = NAMES.iter().map(|name| format!("\"{name}\"")).collect();
    let mut out = format!(
        "struct Row{id}:\n    id: int\n    name: str\n    size: int\n\ndef named{id}(at: int) -> str:\n    return \
         [{}][at]\n\n",
        names.join(", ")
    );
    for (at, step) in storing.steps.iter().enumerate() {
        if let Step::Transaction { steps, fails } = step {
            let _ = writeln!(
                out,
                "def kept{id}_{at}(db: sqlite.Database, stdout: Stdout) fails SqlError:\n    with db.transaction()? as tx:"
            );
            for (inner, step) in steps.iter().enumerate() {
                step_written(id, at * 10 + inner, step, "tx", "        ", &mut out);
            }
            if *fails {
                out.push_str("        fail .Failed(reason=\"undone on purpose\")\n");
            }
            out.push('\n');
        }
    }
    let _ = write!(
        out,
        "def stored{id}(fs: Fs, stdout: Stdout) fails SqlError:\n    with sqlite.open(fs, \"s{id}.db\")? as db:\n        \
         _ = db.execute(t\"drop table if exists t{id}\")?\n        _ = db.execute(t\"create table t{id} (id int primary \
         key, name text not null, size int check (size >= 0))\")?\n"
    );
    for (at, step) in storing.steps.iter().enumerate() {
        step_written(id, at, step, "db", "        ", &mut out);
    }
    out.push('\n');
    out
}
