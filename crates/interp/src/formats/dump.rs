//! Writing values as JSON or TOML text. Dicts and structs become objects or tables, lists
//! and tuples become arrays. An enum case with no fields is its name, and one with fields is
//! `{"Case": {fields}}`. An `Id` is its slot and generation, and an `IdMap` is every slot
//! with its generation, so the `Id`s a program saved still find their items once it's read
//! back. A value the format can't hold, like a set, stops the program.

use std::fmt::Write;

use std::rc::Rc;

use crate::format;
use crate::value::{IdMap, Value};

/// Whether a dict is written as an object: its keys are text or ints, which are written as
/// their digits. Otherwise JSON writes it as an array of `[key, value]` pairs.
fn is_object(pairs: &crate::pairs::Pairs) -> bool {
    pairs.iter().all(|(key, _)| matches!(key, Value::Str(_) | Value::Int(_)))
}

/// A dict's or struct's entries, by name; `None` for any other value.
fn entries(value: &Value, format: &str) -> Result<Option<Vec<(String, Value)>>, String> {
    match value {
        Value::Dict(pairs) if format == "JSON" && !is_object(pairs) => Ok(None),
        Value::Dict(pairs) => pairs
            .iter()
            .map(|(key, value)| match key {
                Value::Str(key) => Ok((key.to_string(), value.clone())),
                Value::Int(n) => Ok((n.to_string(), value.clone())),
                other => Err(format!("{format} keys have to be strings or ints, not {}", a(other))),
            })
            .collect::<Result<_, _>>()
            .map(Some),
        Value::Instance(instance) if instance.case.is_none() && !instance.is_error => {
            Ok(Some(instance.fields.iter().map(|(name, value)| (name.to_string(), value.clone())).collect()))
        }
        Value::Instance(instance) if !instance.is_error && !instance.fields.is_empty() => {
            let case = instance.case.as_deref().unwrap_or_default().to_owned();
            let fields: Vec<(Value, Value)> =
                instance.fields.iter().map(|(name, value)| (Value::str(name), value.clone())).collect();
            Ok(Some(vec![(case, Value::Dict(Rc::new(fields.into())))]))
        }
        Value::Id(id) => Ok(Some(vec![
            ("slot".to_owned(), Value::Int(i64::try_from(id.slot).unwrap_or(i64::MAX))),
            ("generation".to_owned(), Value::Int(i64::from(id.generation))),
        ])),
        Value::IdMap(map) => Ok(Some(vec![("slots".to_owned(), slots(map))])),
        _ => Ok(None),
    }
}

/// An `IdMap`'s slots, each its generation and, unless it's free, its value.
fn slots(map: &IdMap) -> Value {
    let slots = map.slots.iter().map(|slot| {
        let mut pairs = vec![(Value::str("generation"), Value::Int(i64::from(slot.generation)))];
        if let Some(value) = &slot.value {
            pairs.push((Value::str("value"), value.clone()));
        }
        Value::Dict(Rc::new(pairs.into()))
    });
    Value::list(slots.collect())
}

fn a(value: &Value) -> String {
    let name = value.type_name();
    if name.starts_with(['a', 'e', 'i', 'o', 'u', 'I']) { format!("an {name}") } else { format!("a {name}") }
}

/// A float as both formats write it: always with a `.` or an exponent, so it reads back as a
/// float. Very big and very small numbers get an exponent.
fn float(x: f64) -> String {
    if x != 0.0 && x.is_finite() && !(1e-4..1e16).contains(&x.abs()) {
        return format!("{x:e}");
    }
    let text = format::float(x);
    if text.contains(['.', 'e', 'n']) { text } else { format!("{text}.0") }
}

/// A string in double quotes, with the escapes JSON and TOML share.
fn quoted(text: &str) -> String {
    let mut out = String::from('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub(super) fn json(value: &Value, indent: Option<usize>) -> Result<String, String> {
    let mut out = String::new();
    write_json(&mut out, value, indent, 0)?;
    Ok(out)
}

fn write_json(out: &mut String, value: &Value, indent: Option<usize>, depth: usize) -> Result<(), String> {
    if let Some(entries) = entries(value, "JSON")? {
        let entries = entries.into_iter().map(|(key, value)| (Some(key), value)).collect();
        return json_container(out, ('{', '}'), entries, indent, depth);
    }
    match value {
        Value::None => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(n) => out.push_str(&n.to_string()),
        Value::Float(x) if x.is_finite() => out.push_str(&float(*x)),
        Value::Float(_) => return Err("JSON can't hold inf or nan".to_owned()),
        Value::Str(text) => out.push_str(&quoted(text)),
        Value::List(items) | Value::Tuple(items) => {
            let entries = items.iter().map(|item| (None, item.clone())).collect();
            json_container(out, ('[', ']'), entries, indent, depth)?;
        }
        Value::Dict(pairs) => {
            let entries = pairs
                .iter()
                .map(|(key, value)| (None, Value::tuple(vec![key.clone(), value.clone()])))
                .collect();
            json_container(out, ('[', ']'), entries, indent, depth)?;
        }
        Value::Instance(instance) if !instance.is_error && instance.case.is_some() => {
            out.push_str(&quoted(instance.case.as_deref().unwrap_or_default()));
        }
        other => return Err(format!("JSON can't hold {}", a(other))),
    }
    Ok(())
}

/// An array or object: on one line, or one entry per line when indenting.
fn json_container(
    out: &mut String,
    (open, close): (char, char),
    entries: Vec<(Option<String>, Value)>,
    indent: Option<usize>,
    depth: usize,
) -> Result<(), String> {
    out.push(open);
    let is_empty = entries.is_empty();
    for (at, (key, value)) in entries.into_iter().enumerate() {
        if at > 0 {
            out.push(',');
            if indent.is_none() {
                out.push(' ');
            }
        }
        if let Some(width) = indent {
            out.push('\n');
            out.push_str(&" ".repeat(width * (depth + 1)));
        }
        if let Some(key) = key {
            out.push_str(&quoted(&key));
            out.push_str(": ");
        }
        write_json(out, &value, indent, depth + 1)?;
    }
    if let (Some(width), false) = (indent, is_empty) {
        out.push('\n');
        out.push_str(&" ".repeat(width * depth));
    }
    out.push(close);
    Ok(())
}

pub(super) fn toml(value: &Value) -> Result<String, String> {
    let Some(entries) = entries(value, "TOML")? else {
        return Err(format!(
            "a TOML document is a table, so `toml.dumps` needs a dict or a struct, not {}",
            a(value)
        ));
    };
    let mut out = String::new();
    write_table(&mut out, &[], entries, false)?;
    Ok(out)
}

/// What a key in a table holds, for deciding how to write it.
enum Shape {
    Plain,
    Table(Vec<(String, Value)>),
    Tables(Vec<Vec<(String, Value)>>),
}

fn shape(value: &Value) -> Result<Shape, String> {
    if let Some(entries) = entries(value, "TOML")? {
        return Ok(Shape::Table(entries));
    }
    if let Value::List(items) = value
        && !items.is_empty()
    {
        let tables: Option<Vec<_>> = items.iter().map(|item| entries(item, "TOML").ok().flatten()).collect();
        if let Some(tables) = tables {
            return Ok(Shape::Tables(tables));
        }
    }
    Ok(Shape::Plain)
}

/// A table's plain keys under its header, then its tables and arrays of tables.
fn write_table(
    out: &mut String,
    path: &[String],
    entries: Vec<(String, Value)>,
    array: bool,
) -> Result<(), String> {
    let mut plain = Vec::new();
    let mut nested = Vec::new();
    for (key, value) in entries {
        match shape(&value)? {
            Shape::Plain => plain.push((key, value)),
            other => nested.push((key, other)),
        }
    }
    if !path.is_empty() && (array || !plain.is_empty() || nested.is_empty()) {
        if !out.is_empty() {
            out.push('\n');
        }
        let name: Vec<String> = path.iter().map(|key| toml_key(key)).collect();
        let (open, close) = if array { ("[[", "]]") } else { ("[", "]") };
        let _ = writeln!(out, "{open}{}{close}", name.join("."));
    }
    for (key, value) in plain {
        let _ = writeln!(out, "{} = {}", toml_key(&key), toml_value(&value)?);
    }
    for (key, shape) in nested {
        let path: Vec<String> = path.iter().cloned().chain([key]).collect();
        match shape {
            Shape::Table(entries) => write_table(out, &path, entries, false)?,
            Shape::Tables(tables) => {
                for entries in tables {
                    write_table(out, &path, entries, true)?;
                }
            }
            Shape::Plain => {}
        }
    }
    Ok(())
}

fn toml_key(key: &str) -> String {
    let bare = !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if bare { key.to_owned() } else { quoted(key) }
}

/// A value written on one line: inside an array or inline table, or after `key =`.
fn toml_value(value: &Value) -> Result<String, String> {
    if let Some(entries) = entries(value, "TOML")? {
        if entries.is_empty() {
            return Ok("{}".to_owned());
        }
        let pairs: Result<Vec<String>, String> = entries
            .iter()
            .map(|(key, value)| Ok(format!("{} = {}", toml_key(key), toml_value(value)?)))
            .collect();
        return Ok(format!("{{ {} }}", pairs?.join(", ")));
    }
    Ok(match value {
        Value::Bool(b) => if *b { "true" } else { "false" }.to_owned(),
        Value::Int(n) => n.to_string(),
        Value::Float(x) => float(*x),
        Value::Str(text) => quoted(text),
        Value::List(items) | Value::Tuple(items) => {
            let items: Result<Vec<String>, String> = items.iter().map(toml_value).collect();
            format!("[{}]", items?.join(", "))
        }
        Value::None => return Err("TOML has no None, so leave the key out instead".to_owned()),
        other => return Err(format!("TOML can't hold {}", a(other))),
    })
}

#[cfg(test)]
mod tests {
    use super::super::{json, toml};

    fn json_round_trip(text: &str, indent: Option<usize>) -> String {
        json::parse(text)
            .map_or_else(|failure| failure.message, |value| super::json(&value, indent).unwrap_or_default())
    }

    fn toml_round_trip(text: &str) -> String {
        toml::parse(text)
            .map_or_else(|failure| failure.message, |value| super::toml(&value).unwrap_or_default())
    }

    #[test]
    fn writes_json() {
        let text = r#"{"a": [1, 2.0, "x\"y\n"], "b": {}, "c": [], "d": null, "e": true}"#;
        assert_eq!(json_round_trip(text, None), text);
        assert_eq!(
            json_round_trip(r#"{"a": [1, {"b": 2}], "c": []}"#, Some(2)),
            "{\n  \"a\": [\n    1,\n    {\n      \"b\": 2\n    }\n  ],\n  \"c\": []\n}"
        );
        assert_eq!(json_round_trip("1e300", None), "1e300");
    }

    #[test]
    fn writes_toml() {
        let text = "name = \"fox\"\nlist = [1, 2.5, { a = 1 }]\n\n[a.b]\nc = 1\n\n[[d]]\ne = 1\n\n[d.f]\ng = \"h i\"\n\n[[d]]\n\n[\"key with space\"]\n";
        assert_eq!(toml_round_trip(text), text);
    }
}
