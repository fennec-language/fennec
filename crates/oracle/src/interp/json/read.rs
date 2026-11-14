//! `json.loads[T]`: JSON text read as a `T`, by its type, with what a load that fails says, as
//! `std.json` says it: what's wrong with text that isn't JSON, by its line and column, and
//! otherwise where in the value the text doesn't fit the type, and how.
//!
//! The standard library reads text in one go, and reads text that failed again, explaining, to
//! find what to say. Reading here is once, through the whole tree, in the order the explaining
//! read goes: an object read as a struct has its keys no field has found first, then its fields
//! in the order the text gives them, then those it left out, in the type's order.

use std::collections::HashMap;

use fennec_check::Type;
use fennec_syntax::Span;

use super::parse::{self, Entry, Node, cut_input, holds_too_big, is_float_text};
use super::{Names, Options, SavedField, article, parse_error, spelled_as, type_name};
use crate::interp::{Flow, Interp, Run, unsupported};
use crate::load::Kind;
use crate::value::{Id, Instance, Table, Value};

/// How many fields a struct may have for the oracle to read it: past this, the standard library
/// stops telling a field given twice.
const MOST_FIELDS: usize = 62;

/// What a read keeps track of as it goes.
struct Reading {
    options: Options,
    /// Where the read is, as a failure says it.
    path: Vec<String>,
    /// Each `Id` read: its type's name, where it is, and where its item is in its `Table`.
    ids: Vec<(String, String, i64)>,
    /// Each `Table` read: its items' type's name, where it is, and how many items it has.
    tables: Vec<(String, String, usize)>,
    /// The tag this read gives the `Table` of each type, and every `Id` into it.
    tags: HashMap<String, u64>,
}

impl Reading {
    fn joined(&self) -> String {
        let mut out = String::new();
        for part in &self.path {
            if !out.is_empty() && !part.starts_with('[') {
                out.push('.');
            }
            out.push_str(part);
        }
        out
    }

    /// Where the read is, as messages say it: "`units[0].name`", or "the text".
    fn here(&self) -> String {
        if self.path.is_empty() { "the text".to_owned() } else { format!("`{}`", self.joined()) }
    }

    /// The failure for a value that isn't what's wanted.
    fn misfit(&self, wanted: &str, node: &Node) -> Flow {
        says(format!("{} should be {}, but it's {}", self.here(), article(wanted), found(node)))
    }

    /// What's wrong with the `Id`s and `Table`s read, if anything is.
    fn problem(&self) -> Option<String> {
        let mut seen: Vec<(&str, &str)> = Vec::new();
        for (kind, at, _) in &self.tables {
            if let Some((_, there)) = seen.iter().find(|(known, _)| known == kind) {
                return Some(format!(
                    "{there} and {at} are both a `Table[{kind}]`, so an Id can't say which it's in"
                ));
            }
            seen.push((kind, at));
        }
        for (kind, at, position) in &self.ids {
            let mut found = false;
            for (table_kind, table_at, count) in &self.tables {
                if table_kind != kind {
                    continue;
                }
                found = true;
                if usize::try_from(*position).is_ok_and(|position| position >= *count) {
                    let items = match count {
                        0 => "nothing".to_owned(),
                        1 => "only 1".to_owned(),
                        _ => format!("only {count}"),
                    };
                    return Some(format!("{at} is item {position} of {table_at}, which has {items}"));
                }
            }
            if !found {
                return Some(format!("{at} is an Id, but the text has no `Table[{kind}]` for it to be in"));
            }
        }
        None
    }
}

/// A load's failure, saying `message`.
fn says(message: String) -> Flow {
    Flow::Fail(parse_error(message, 0, 0), Span::default())
}

/// A failing load whose message the oracle can't be sure of.
fn refused<T>(why: &str) -> Run<T> {
    unsupported(format!("json.loads failing ({why})"))
}

/// What the next value is, as a message names it.
fn found(node: &Node) -> &'static str {
    match node {
        Node::Object(_) => "an object",
        Node::Null => "null",
        Node::Str(_) => "a str",
        Node::Array(_) => "a list",
        Node::Bool(_) => "a bool",
        Node::Number(number) if is_float_text(number) => "a float",
        Node::Number(_) => "an int",
    }
}

impl Interp {
    /// `json.loads[T](text)`, for a `T` of type `ty`.
    pub(crate) fn loads(&mut self, text: &str, ty: &Type, options: Options, span: Span) -> Run<Value> {
        let parsed = match parse::parse(text) {
            Ok(parsed) => parsed,
            Err(bad) => return Err(Flow::Fail(parse_error(bad.message, bad.line, bad.column), span)),
        };
        let mut reading =
            Reading { options, path: Vec::new(), ids: Vec::new(), tables: Vec::new(), tags: HashMap::new() };
        let read =
            self.read(&parsed.node, ty, &mut reading, false).and_then(|value| match reading.problem() {
                Some(problem) => Err(says(problem)),
                None => Ok(value),
            });
        match read {
            // Text that doesn't load is checked as JSON first: an int too big for an int is what's
            // wrong with it, even where it's not what failed.
            Err(Flow::Fail(error, _)) => match parsed.too_big {
                Some(bad) => Err(Flow::Fail(parse_error(bad.message, bad.line, bad.column), span)),
                None => Err(Flow::Fail(error, span)),
            },
            other => other,
        }
    }

    /// A value of type `ty` read from `node`. `in_key` says it's a dict's key, written in quotes.
    fn read(&mut self, node: &Node, ty: &Type, reading: &mut Reading, in_key: bool) -> Run<Value> {
        self.tick()?;
        match (ty, node) {
            (Type::Int, _) => read_int(node, reading, in_key).map(Value::Int),
            (Type::Float, Node::Number(number)) => match number.parse::<f64>() {
                Ok(x) if x.is_infinite() => Err(says(format!("{} is too big for a float", reading.here()))),
                Ok(x) => Ok(Value::Float(x)),
                Err(_) => unsupported("a number Rust can't read"),
            },
            (Type::Float, _) => Err(reading.misfit("float", node)),
            (Type::Bool, Node::Bool(truth)) => Ok(Value::Bool(*truth)),
            (Type::Bool, _) => Err(reading.misfit("bool", node)),
            (Type::Str | Type::LiteralStr, Node::Str(text)) => Ok(Value::Str(text.clone())),
            (Type::Str | Type::LiteralStr, _) => Err(reading.misfit("str", node)),
            (Type::Optional(_), Node::Null) => Ok(Value::None),
            (Type::Optional(inner), _) => self.read(node, inner, reading, in_key),
            (Type::List(inner), Node::Array(items)) => {
                self.read_items(items, inner, reading).map(Value::List)
            }
            (Type::List(inner), _) => Err(reading.misfit(&format!("list[{}]", type_name(inner)), node)),
            (Type::Tuple(types), _) if types.len() > 1 => self.read_tuple(node, ty, types, reading),
            (Type::Dict(key, value), _) => self.read_dict(node, (key, value), reading),
            (Type::Table(inner), _) => self.read_saved_table(node, inner, reading),
            (Type::Id(inner), _) => {
                let position = read_int(node, reading, in_key)?;
                let Ok(slot) = usize::try_from(position) else {
                    return Err(says(format!("{} is out of range", reading.here())));
                };
                let kind = type_name(inner);
                reading.ids.push((kind.clone(), reading.here(), position));
                Ok(Value::Id(Id { slot, generation: 0, tag: self.tag_of(&kind, reading) }))
            }
            (Type::Named(name), _) => self.read_declared(node, name, &[], reading),
            (Type::Generic(name, args), _) => self.read_declared(node, name, args, reading),
            (Type::Union(_), _) => unsupported("loading a union from JSON"),
            (Type::Bytes, _) => unsupported("loading bytes from JSON"),
            (other, _) => unsupported(format!("loading a {} from JSON", type_name(other))),
        }
    }

    /// The tag this read gives the `Table` of items of the type named `kind`.
    fn tag_of(&self, kind: &str, reading: &mut Reading) -> u64 {
        *reading.tags.entry(kind.to_owned()).or_insert_with(|| self.shared.new_tag())
    }

    /// An array's items, each a `T` of type `ty`.
    fn read_items(&mut self, items: &[Node], ty: &Type, reading: &mut Reading) -> Run<Vec<Value>> {
        let mut values = Vec::with_capacity(items.len());
        for (at, item) in items.iter().enumerate() {
            reading.path.push(format!("[{at}]"));
            values.push(self.read(item, ty, reading, false)?);
            reading.path.pop();
        }
        Ok(values)
    }

    /// A tuple, as an array of exactly its items. One of another length doesn't fit, said of the
    /// whole array, once the items it has room for are read.
    fn read_tuple(&mut self, node: &Node, ty: &Type, types: &[Type], reading: &mut Reading) -> Run<Value> {
        let label = type_name(ty);
        let Node::Array(items) = node else { return Err(reading.misfit(&label, node)) };
        let mut values = Vec::with_capacity(types.len());
        for (at, item_ty) in types.iter().enumerate() {
            let Some(item) = items.get(at) else { return Err(reading.misfit(&label, node)) };
            reading.path.push(format!("[{at}]"));
            values.push(self.read(item, item_ty, reading, false)?);
            reading.path.pop();
        }
        if items.len() > types.len() {
            return Err(reading.misfit(&label, node));
        }
        Ok(Value::Tuple(values))
    }

    /// A dict: an object, or an array of `[key, value]` pairs. A key given twice fails, naming it,
    /// once every pair has been read.
    fn read_dict(
        &mut self,
        node: &Node,
        (key_ty, value_ty): (&Type, &Type),
        reading: &mut Reading,
    ) -> Run<Value> {
        let pairs = match node {
            Node::Array(items) => self.read_pairs(items, (key_ty, value_ty), reading)?,
            Node::Object(entries) => {
                let mut pairs = Vec::with_capacity(entries.len());
                for entry in entries {
                    let key = self.read(&Node::Str(entry.key.clone()), key_ty, reading, true)?;
                    reading.path.push(format!("[{}]", cut_input(&entry.written)));
                    let value = self.read(&entry.value, value_ty, reading, false)?;
                    reading.path.pop();
                    pairs.push((key, value));
                }
                pairs
            }
            _ => {
                let wanted = format!("dict[{}, {}]", type_name(key_ty), type_name(value_ty));
                return Err(reading.misfit(&wanted, node));
            }
        };
        let mut dict: Vec<(Value, Value)> = Vec::with_capacity(pairs.len());
        for (key, value) in pairs {
            if crate::interp::find_key(&dict, &key)?.is_some() {
                let twice = twice_key(&self.display(&key)?);
                return Err(says(format!("{} has {twice} twice", reading.here())));
            }
            dict.push((key, value));
        }
        Ok(Value::Dict(dict))
    }

    fn read_pairs(
        &mut self,
        items: &[Node],
        (key_ty, value_ty): (&Type, &Type),
        reading: &mut Reading,
    ) -> Run<Vec<(Value, Value)>> {
        let mut pairs = Vec::with_capacity(items.len());
        for (at, item) in items.iter().enumerate() {
            reading.path.push(format!("[{at}]"));
            let Node::Array(pair) = item else { return Err(reading.misfit("[key, value] pair", item)) };
            reading.path.push("[0]".to_owned());
            // An empty pair's key is read at its `]`, which isn't the start of any value, and is
            // said to be an int.
            let Some(key) = pair.first() else {
                if let Type::Named(name) | Type::Generic(name, _) = key_ty {
                    self.saved_type(name)?;
                }
                let wanted = wanted(key_ty)?;
                return Err(says(format!(
                    "{} should be {}, but it's an int",
                    reading.here(),
                    article(&wanted)
                )));
            };
            let key = self.read(key, key_ty, reading, false)?;
            reading.path.pop();
            let Some(value) = pair.get(1) else { return Err(not_a_pair(reading)) };
            reading.path.push("[1]".to_owned());
            let value = self.read(value, value_ty, reading, false)?;
            reading.path.pop();
            if pair.len() > 2 {
                return Err(not_a_pair(reading));
            }
            reading.path.pop();
            pairs.push((key, value));
        }
        Ok(pairs)
    }

    /// A `Table`, as an array of its items, made new with this read's tag for its type.
    fn read_saved_table(&mut self, node: &Node, ty: &Type, reading: &mut Reading) -> Run<Value> {
        let Node::Array(items) = node else {
            return Err(reading.misfit(&format!("list[{}]", type_name(ty)), node));
        };
        let items = self.read_items(items, ty, reading)?;
        let kind = type_name(ty);
        reading.tables.push((kind.clone(), reading.here(), items.len()));
        let mut table = Table::new(self.tag_of(&kind, reading));
        for item in items {
            table.add(item);
        }
        Ok(Value::Table(Box::new(table)))
    }

    /// A struct or enum the program declares.
    fn read_declared(&mut self, node: &Node, name: &str, args: &[Type], reading: &mut Reading) -> Run<Value> {
        let name = fennec_syntax::rename::shown_type_name(name);
        match self.saved_type(name)? {
            Kind::Struct => {
                let Node::Object(entries) = node else { return Err(reading.misfit(name, node)) };
                let fields = self.saved_fields(name, None, args)?;
                let values = self.read_record(entries, &fields, name, reading)?;
                let instance = Instance { ty: name.to_owned(), case: None, fields: values };
                if let Some(broken) = self.broken_rule(&instance)? {
                    return Err(says(format!(
                        "in {}, {name}.{broken} breaks its `where` rule",
                        reading.here()
                    )));
                }
                Ok(Value::Instance(Box::new(instance)))
            }
            Kind::Enum => self.read_case(node, name, args, reading),
            Kind::Error => unsupported(format!("loading a {name} from JSON")),
        }
    }

    /// An enum's case: its name in quotes when it has no fields, or an object holding its fields
    /// under its name.
    fn read_case(&mut self, node: &Node, name: &str, args: &[Type], reading: &mut Reading) -> Run<Value> {
        let Some(declared) = self.program.types.get(name) else {
            return unsupported(format!("the enum {name}"));
        };
        let cases = &declared.def.cases;
        if cases.iter().any(|case| case.fields.iter().any(|field| field.rule.is_some())) {
            return unsupported("loading a case with a `where` rule from JSON");
        }
        let entries = match node {
            Node::Str(key) => {
                let Some(case) = cases.iter().find(|case| case.name == *key) else {
                    return Err(says(format!(
                        "{} is \"{key}\", which isn't a case of `{name}`",
                        reading.here()
                    )));
                };
                if !case.fields.is_empty() {
                    return Err(says(format!(
                        "{} is `{name}.{key}`, which needs its fields",
                        reading.here()
                    )));
                }
                return Ok(Value::instance(name, Some(key), Vec::new()));
            }
            Node::Object(entries) => entries,
            _ => return Err(reading.misfit(name, node)),
        };
        let Some(Entry { key, value: Node::Object(inner), .. }) = entries.first() else {
            return Err(reading.misfit(name, node));
        };
        let Some(case) = cases.iter().find(|case| case.name == *key) else {
            return Err(says(format!("{} is \"{key}\", which isn't a case of `{name}`", reading.here())));
        };
        let values = if case.fields.is_empty() {
            if let Some(first) = inner.first() {
                let field = cut_input(&first.key);
                return Err(says(format!(
                    "`{name}.{key}` has no field \"{field}\", but {} has one",
                    reading.here()
                )));
            }
            Vec::new()
        } else {
            let fields = self.saved_fields(name, Some(key), args)?;
            self.read_record(inner, &fields, &format!("{name}.{key}"), reading)?
        };
        // A key after the case is a failure once the case's own fields have read, naming the first
        // such key as a field is named (#1686). Anything else after the case is a syntax error,
        // which the reader has already said.
        if let Some(after) = entries.get(1) {
            return Err(says(format!(
                "{} has \"{}\" after its case, but an enum is written as an object with its case as the only key",
                reading.here(),
                cut_input(&after.key)
            )));
        }
        Ok(Value::instance(name, Some(key), values))
    }

    /// A struct's fields, or a case's, from an object's entries, in the type's order, each the
    /// text left out taking its default.
    fn read_record(
        &mut self,
        entries: &[Entry],
        fields: &[SavedField],
        owner: &str,
        reading: &mut Reading,
    ) -> Run<Vec<(String, Value)>> {
        let saved: Vec<&SavedField> = fields.iter().filter(|field| !field.skipped).collect();
        let mut listed: Vec<(String, String)> =
            saved.iter().map(|field| (field.saved_name().to_owned(), field.name.clone())).collect();
        listed.extend(
            fields.iter().filter(|field| field.skipped).map(|field| (String::new(), field.name.clone())),
        );
        if listed.len() > MOST_FIELDS {
            return unsupported("loading a struct of more than 62 fields from JSON");
        }
        let names = reading.options.names;
        if !reading.options.ignore
            && let Some(entry) = entries.iter().find(|entry| place_of(&entry.key, &listed, names).is_none())
        {
            return Err(says(unknown(&entry.key, &listed, (owner, &reading.here()), names)));
        }
        let mut read: Vec<Option<Value>> = saved.iter().map(|_| None).collect();
        for entry in entries {
            let Some(at) = place_of(&entry.key, &listed, names) else {
                if holds_too_big(&entry.value) {
                    return Err(says("an int too big for an int".to_owned()));
                }
                continue;
            };
            let (Some(slot), Some(field)) = (read.get(at), saved.get(at)) else {
                return refused("a key that names a field `@skip` leaves out");
            };
            if slot.is_some() {
                let shown = listed.get(at).map_or("", |(shown, _)| shown.as_str());
                return Err(says(format!("{} has `{shown}` twice", reading.here())));
            }
            reading.path.push(entry.key.clone());
            let value = self.read(&entry.value, &field.ty, reading, false)?;
            reading.path.pop();
            if let Some(slot) = read.get_mut(at) {
                *slot = Some(value);
            }
        }
        self.fields_made(fields, read, &listed, reading)
    }

    /// The value of each field, in the type's order: what was read, or the default of one left
    /// out, or `None` for an optional one with no default. One with neither fails as missing.
    fn fields_made(
        &mut self,
        fields: &[SavedField],
        read: Vec<Option<Value>>,
        listed: &[(String, String)],
        reading: &Reading,
    ) -> Run<Vec<(String, Value)>> {
        let mut read = read.into_iter();
        let mut values: Vec<(String, Option<Value>)> = Vec::with_capacity(fields.len());
        for field in fields {
            if field.skipped {
                values.push((field.name.clone(), None));
                continue;
            }
            let value = match (read.next().flatten(), &field.field.default) {
                (Some(value), _) => value,
                (None, Some(default)) => self.plain_value(default)?,
                (None, None) if matches!(field.ty, Type::Optional(_)) => Value::None,
                (None, None) => return Err(says(missing(field.saved_name(), listed, reading))),
            };
            values.push((field.name.clone(), Some(value)));
        }
        let mut made = Vec::with_capacity(values.len());
        for ((name, value), field) in values.into_iter().zip(fields) {
            let value = match (value, &field.field.default) {
                (Some(value), _) => value,
                (None, Some(default)) => self.plain_value(default)?,
                (None, None) => return unsupported("a field `@skip` leaves out with no default"),
            };
            made.push((name, value));
        }
        Ok(made)
    }
}

/// What a misfit says a dict's key of this type should be.
fn wanted(ty: &Type) -> Run<String> {
    Ok(match ty {
        Type::Int | Type::Id(_) => "int".to_owned(),
        Type::Float => "float".to_owned(),
        Type::Bool => "bool".to_owned(),
        Type::Str | Type::LiteralStr => "str".to_owned(),
        Type::List(inner) => format!("list[{}]", type_name(inner)),
        Type::Dict(key, value) => format!("dict[{}, {}]", type_name(key), type_name(value)),
        Type::Tuple(items) if items.len() > 1 => type_name(ty),
        Type::Named(name) | Type::Generic(name, _) => fennec_syntax::rename::shown_type_name(name).to_owned(),
        other => return unsupported(format!("loading a {} from JSON", type_name(other))),
    })
}

/// The failure for what isn't an array of a key and a value, where a dict's pair should be.
fn not_a_pair(reading: &Reading) -> Flow {
    says(format!("{} should be a [key, value] pair", reading.here()))
}

/// An int, written as one, or for a dict's key in quotes, as its plain digits.
fn read_int(node: &Node, reading: &Reading, in_key: bool) -> Run<i64> {
    match node {
        // One too big for an int fails the read, and the text is said to be wrong for it.
        Node::Number(number) if !is_float_text(number) => {
            number.parse::<i64>().map_err(|_| says("an int too big for an int".to_owned()))
        }
        Node::Str(key) if in_key => {
            // `int()` reads it, past the spaces around it and a `+`, and then it has to be
            // written just as that int prints.
            match key.trim().parse::<i64>() {
                Ok(n) if n.to_string() == *key => Ok(n),
                Ok(_) => Err(says(format!(
                    "{} has the key \"{key}\", which isn't how that int is written",
                    reading.here()
                ))),
                Err(_) => Err(says(format!(
                    "{} has the key \"{}\", which isn't an int",
                    reading.here(),
                    cut_input(key)
                ))),
            }
        }
        _ => Err(reading.misfit("int", node)),
    }
}

/// Which field a key names, as a place among them: one `@serialize` named by that name, or one
/// whose own name `names` spells that way.
fn place_of(key: &str, listed: &[(String, String)], names: Names) -> Option<usize> {
    let spelled_so = |own: &str| spelled_as(own, names) == key;
    let first = listed.iter().position(|(saved, own)| {
        saved == key && !(saved == own && names != Names::Snake && !spelled_so(own))
    });
    if first.is_some() || names == Names::Snake {
        return first;
    }
    listed.iter().position(|(saved, own)| saved == own && spelled_so(own))
}

/// What a key no field has fails with: why, when the key is the name of a field saved under
/// another, or one that isn't saved.
fn unknown(key: &str, listed: &[(String, String)], (owner, here): (&str, &str), names: Names) -> String {
    for (saved, own) in listed {
        if spelled_as(own, names) == key {
            if saved.is_empty() {
                return format!("`{owner}` doesn't save `{own}`, which has `@skip`, but {here} has it");
            }
            return format!("`{owner}` saves `{own}` as \"{saved}\", but {here} has \"{key}\"");
        }
    }
    format!("`{owner}` has no field \"{key}\", but {here} has one")
}

/// What a field the text left out fails with, by the name it's saved under, spelled as the
/// text spells it.
fn missing(shown: &str, listed: &[(String, String)], reading: &Reading) -> String {
    let mut wire = shown.to_owned();
    for (saved, own) in listed {
        if saved == shown && saved == own {
            wire = spelled_as(own, reading.options.names);
        }
    }
    let joined = reading.joined();
    if joined.is_empty() {
        format!("`{wire}` is missing")
    } else if wire.starts_with('[') {
        format!("`{joined}{wire}` is missing")
    } else {
        format!("`{joined}.{wire}` is missing")
    }
}

/// A key given twice, as a message names it: as it prints, in backticks, cut to its first 20
/// characters past 40, with its control characters written as escapes.
fn twice_key(shown: &str) -> String {
    let cut = if shown.chars().count() <= 40 {
        shown.to_owned()
    } else {
        format!("{}…", shown.chars().take(20).collect::<String>())
    };
    format!("`{}`", parse::escaped(&cut))
}
