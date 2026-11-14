//! `json.dumps`: a value as what it saves, by its type, then that written as JSON text, on one
//! line or indented. A value holding a float JSON can't write, or an `Id` that can't be saved,
//! stops the program, saying where it is.

use std::collections::HashMap;
use std::fmt::Write;

use fennec_check::Type;
use fennec_syntax::Span;

use super::{Names, Options, SavedField, spelled_as, type_name};
use crate::interp::{Interp, Run, crash, unsupported};
use crate::load::Kind;
use crate::value::{Id, Value};

/// What a value saves, as the calls its `serialize` makes.
#[derive(Debug)]
enum Saved {
    Int(i64),
    Float(f64),
    Bool(bool),
    Text(String),
    None,
    /// An optional's value, which is there.
    Some(Box<Saved>),
    Sequence(Vec<Saved>),
    Map(Vec<(Saved, Saved)>),
    /// A struct's saved fields, or an enum's case's, each by the name it's written under.
    Record(Vec<(FieldName, Saved)>),
    /// A tuple's items.
    Items(Vec<Saved>),
    /// An enum's case with no fields.
    Case(String),
    CaseRecord(String, Vec<(FieldName, Saved)>),
    /// A `Table` of items of the type named `kind`: its tag, and each item with its `Id`.
    Table {
        kind: String,
        tag: u64,
        ids: Vec<Id>,
        items: Vec<Saved>,
    },
    Id {
        kind: String,
        id: Id,
    },
}

/// A field's name as it's saved: its own, which `names=` may spell another way, or the one
/// `@serialize(name=...)` gave it, written just so.
#[derive(Debug)]
enum FieldName {
    Own(String),
    Renamed(String),
}

impl Interp {
    /// `json.dumps(value)`, of a value of type `ty`.
    pub(crate) fn dumps(&mut self, value: &Value, ty: &Type, options: Options, span: Span) -> Run<Value> {
        let saved = self.saved(value, ty)?;
        if unwritable(&saved) {
            let mut finder = Finder { floats: true, ..Finder::default() };
            finder.go(&saved, false);
            let problem = finder.problem().unwrap_or_else(|| "JSON can't write NaN or infinity".to_owned());
            return crash(problem, span);
        }
        let mut writer = Writer::new(options);
        if !has_ids(&saved) {
            writer.go(&saved, false);
            return Ok(Value::Str(writer.text));
        }
        let mut finder = Finder::default();
        finder.go(&saved, false);
        if let Some(problem) = finder.problem() {
            return crash(problem, span);
        }
        writer.tables = Some(finder.positions);
        writer.go(&saved, false);
        Ok(Value::Str(writer.text))
    }

    /// What a value of type `ty` saves.
    fn saved(&mut self, value: &Value, ty: &Type) -> Run<Saved> {
        self.tick()?;
        Ok(match (ty, value) {
            (Type::Int, Value::Int(n)) => Saved::Int(*n),
            (Type::Float, Value::Float(x)) => Saved::Float(*x),
            (Type::Float, Value::Int(n)) => Saved::Float(crate::text::int_as_float(*n)),
            (Type::Bool, Value::Bool(truth)) => Saved::Bool(*truth),
            (Type::Str | Type::LiteralStr, Value::Str(text)) => Saved::Text(text.clone()),
            (Type::Optional(_) | Type::None, Value::None) => Saved::None,
            (Type::Optional(inner), held) => Saved::Some(Box::new(self.saved(held, inner)?)),
            (Type::List(inner), Value::List(items)) => {
                let mut saved = Vec::with_capacity(items.len());
                for item in items {
                    saved.push(self.saved(item, inner)?);
                }
                Saved::Sequence(saved)
            }
            (Type::Dict(key_ty, value_ty), Value::Dict(pairs)) => {
                let mut saved = Vec::with_capacity(pairs.len());
                for (key, held) in pairs {
                    saved.push((self.saved(key, key_ty)?, self.saved(held, value_ty)?));
                }
                Saved::Map(saved)
            }
            (Type::Tuple(types), Value::Tuple(items)) if types.len() == items.len() && types.len() > 1 => {
                let mut saved = Vec::with_capacity(items.len());
                for (item, item_ty) in items.iter().zip(types) {
                    saved.push(self.saved(item, item_ty)?);
                }
                Saved::Items(saved)
            }
            (Type::Table(inner), Value::Table(table)) => {
                let mut ids = Vec::new();
                let mut items = Vec::new();
                for (id, item) in table.live() {
                    ids.push(id);
                    items.push(self.saved(item, inner)?);
                }
                Saved::Table { kind: type_name(inner), tag: table.tag, ids, items }
            }
            (Type::Id(inner), Value::Id(id)) => Saved::Id { kind: type_name(inner), id: *id },
            (Type::Named(name), Value::Instance(_)) => self.saved_instance(value, name, &[])?,
            (Type::Generic(name, args), Value::Instance(_)) => self.saved_instance(value, name, args)?,
            (Type::Union(_), _) => return unsupported("saving a union as JSON"),
            (Type::Bytes, _) => return unsupported("saving bytes as JSON"),
            (ty, held) => return unsupported(format!("saving a {} as a {}", held.kind(), type_name(ty))),
        })
    }

    /// What a struct or an enum's case saves: its fields, but those `@skip` leaves out.
    fn saved_instance(&mut self, value: &Value, name: &str, args: &[Type]) -> Run<Saved> {
        let Value::Instance(instance) = value else { return unsupported("saving what isn't a struct") };
        let kind = self.saved_type(name)?;
        if (kind == Kind::Enum) != instance.case.is_some() {
            return unsupported("saving a value of another type than its own");
        }
        let fields = self.saved_fields(name, instance.case.as_deref(), args)?;
        let mut saved = Vec::new();
        for field in fields.iter().filter(|field| !field.skipped) {
            let Some(held) = instance.field(&field.name) else {
                return unsupported(format!("the field `{}`", field.name));
            };
            let held = held.clone();
            saved.push((field_name(field), self.saved(&held, &field.ty)?));
        }
        // A case is written as its name when it saves no fields, even when it has some `@skip`
        // leaves out.
        Ok(match &instance.case {
            Some(case) if saved.is_empty() => Saved::Case(case.clone()),
            Some(case) => Saved::CaseRecord(case.clone(), saved),
            None => Saved::Record(saved),
        })
    }
}

fn field_name(field: &SavedField) -> FieldName {
    match &field.renamed {
        Some(renamed) => FieldName::Renamed(renamed.clone()),
        None => FieldName::Own(field.name.clone()),
    }
}

/// Whether what's saved holds a float JSON can't write: NaN or infinity.
fn unwritable(saved: &Saved) -> bool {
    match saved {
        Saved::Float(x) => !x.is_finite(),
        Saved::Some(inner) => unwritable(inner),
        Saved::Sequence(items) | Saved::Items(items) | Saved::Table { items, .. } => {
            items.iter().any(unwritable)
        }
        Saved::Map(pairs) => pairs.iter().any(|(key, value)| unwritable(key) || unwritable(value)),
        Saved::Record(fields) | Saved::CaseRecord(_, fields) => {
            fields.iter().any(|(_, value)| unwritable(value))
        }
        _ => false,
    }
}

/// Whether what's saved holds an `Id`, whose place in its `Table` is found first.
fn has_ids(saved: &Saved) -> bool {
    match saved {
        Saved::Id { .. } => true,
        Saved::Some(inner) => has_ids(inner),
        Saved::Sequence(items) | Saved::Items(items) | Saved::Table { items, .. } => {
            items.iter().any(has_ids)
        }
        Saved::Map(pairs) => pairs.iter().any(|(key, value)| has_ids(key) || has_ids(value)),
        Saved::Record(fields) | Saved::CaseRecord(_, fields) => {
            fields.iter().any(|(_, value)| has_ids(value))
        }
        _ => false,
    }
}

/// A key of a dict, as an object's key: text as it is, and an int as its digits. Any other key
/// makes the dict an array of `[key, value]` pairs.
fn key_text(key: &Saved) -> Option<String> {
    match key {
        Saved::Int(n) => Some(n.to_string()),
        Saved::Text(text) => Some(text.clone()),
        _ => None,
    }
}

/// Where each `Id` is in its `Table`, by the name of its type, then its slot, generation and tag.
type Positions = HashMap<String, HashMap<(usize, u32, u64), usize>>;

/// Writes JSON text.
struct Writer {
    text: String,
    names: Names,
    indent: i64,
    depth: i64,
    first: bool,
    /// Where each `Id` is in its `Table`, once that's been found.
    tables: Option<Positions>,
}

impl Writer {
    fn new(options: Options) -> Writer {
        Writer {
            text: String::new(),
            names: options.names,
            indent: options.indent,
            depth: 0,
            first: true,
            tables: None,
        }
    }

    fn open(&mut self, opening: &str) {
        self.text.push_str(opening);
        self.depth += 1;
        self.first = true;
    }

    fn close(&mut self, closing: &str) {
        self.depth -= 1;
        if self.indent >= 0 && !self.first {
            self.new_line();
        }
        self.text.push_str(closing);
        self.first = false;
    }

    fn entry(&mut self) {
        if self.first {
            self.first = false;
        } else if self.indent < 0 {
            self.text.push_str(", ");
        } else {
            self.text.push(',');
        }
        if self.indent >= 0 {
            self.new_line();
        }
    }

    fn new_line(&mut self) {
        self.text.push('\n');
        let width = usize::try_from(self.indent.saturating_mul(self.depth)).unwrap_or(0);
        self.text.push_str(&" ".repeat(width));
    }

    /// Writes what's saved. `optional` says it's an optional's value, for an `Id` whose item has
    /// gone, which is then written as null.
    fn go(&mut self, saved: &Saved, optional: bool) {
        match saved {
            Saved::Int(n) => self.text.push_str(&n.to_string()),
            Saved::Float(x) => self.text.push_str(&float_text(*x)),
            Saved::Bool(truth) => self.text.push_str(if *truth { "true" } else { "false" }),
            Saved::Text(text) => self.text.push_str(&quoted(text)),
            Saved::None => self.text.push_str("null"),
            Saved::Some(inner) => self.go(inner, true),
            Saved::Sequence(items) | Saved::Items(items) | Saved::Table { items, .. } => self.sequence(items),
            Saved::Map(pairs) => self.map(pairs),
            Saved::Record(fields) => {
                self.open("{");
                self.fields(fields);
                self.close("}");
            }
            Saved::Case(case) => {
                let _ = write!(self.text, "\"{case}\"");
            }
            Saved::CaseRecord(case, fields) => {
                self.open("{");
                self.entry();
                let _ = write!(self.text, "\"{case}\": ");
                self.open("{");
                self.fields(fields);
                self.close("}");
                self.close("}");
            }
            Saved::Id { kind, id } => self.id(kind, *id, optional),
        }
    }

    fn sequence(&mut self, items: &[Saved]) {
        self.open("[");
        for item in items {
            self.entry();
            self.go(item, false);
        }
        self.close("]");
    }

    fn map(&mut self, pairs: &[(Saved, Saved)]) {
        if pairs.first().is_none_or(|(key, _)| key_text(key).is_some()) {
            self.open("{");
            for (key, value) in pairs {
                let name = key_text(key).unwrap_or_default();
                self.entry();
                self.text.push_str(&quoted(&name));
                self.text.push_str(": ");
                self.go(value, false);
            }
            self.close("}");
            return;
        }
        self.open("[");
        for (key, value) in pairs {
            self.entry();
            self.open("[");
            self.entry();
            self.go(key, false);
            self.entry();
            self.go(value, false);
            self.close("]");
        }
        self.close("]");
    }

    fn fields(&mut self, fields: &[(FieldName, Saved)]) {
        for (name, value) in fields {
            self.entry();
            match name {
                FieldName::Own(name) => {
                    let _ = write!(self.text, "\"{}\": ", spelled_as(name, self.names));
                }
                FieldName::Renamed(name) => {
                    let _ = write!(self.text, "{}: ", quoted(name));
                }
            }
            self.go(value, false);
        }
    }

    fn id(&mut self, kind: &str, id: Id, optional: bool) {
        let position =
            self.tables.as_ref().and_then(|tables| tables.get(kind)?.get(&(id.slot, id.generation, id.tag)));
        match position {
            Some(position) => self.text.push_str(&position.to_string()),
            None if optional => self.text.push_str("null"),
            None => self.text.push('0'),
        }
    }
}

/// A float as JSON writes it: as Fennec prints it, with an exponent that has no `+` and no
/// leading zeros, as in `1e20` and `1e-7`.
fn float_text(x: f64) -> String {
    let text = crate::text::float(x);
    let Some((mantissa, exponent)) = text.split_once('e') else { return text };
    let (sign, digits) = match exponent.strip_prefix('-') {
        Some(digits) => ("-", digits),
        None => ("", exponent.trim_start_matches('+')),
    };
    let digits = digits.trim_start_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    format!("{mantissa}e{sign}{digits}")
}

/// Text as a JSON string, in double quotes, with what a string can't hold as it is escaped:
/// quotes, backslashes and control characters, U+007F to U+009F among them.
pub(crate) fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        let code = u32::from(c);
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            _ if code < 32 || (127..160).contains(&code) => {
                let _ = write!(out, "\\u{code:04x}");
            }
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// An `Id` gone through, for checking it can be saved: its type's name, where it points, whether
/// it's an optional's value, and where it is.
struct FoundId {
    kind: String,
    key: (usize, u32, u64),
    optional: bool,
    at: String,
}

/// Goes through what's saved for its `Table`s and `Id`s, writing nothing, so each `Id` can be
/// written as where its item is, and one that can't be is said with where it is. With `floats`,
/// a float JSON can't write is said with where it is too.
#[derive(Default)]
struct Finder {
    positions: Positions,
    /// Where each `Table` is, and its tag, by the name of its items' type.
    tables: HashMap<String, (String, u64)>,
    ids: Vec<FoundId>,
    path: Vec<String>,
    problem: Option<String>,
    floats: bool,
}

impl Finder {
    fn here(&self) -> String {
        place_text(&self.path)
    }

    fn go(&mut self, saved: &Saved, optional: bool) {
        match saved {
            Saved::Float(x) if self.floats && self.problem.is_none() && !x.is_finite() => {
                let here = self.here();
                self.problem = Some(format!(
                    "{here} is {}, which JSON can't write: it has no NaN or infinity",
                    crate::text::float(*x)
                ));
            }
            Saved::Some(inner) => self.go(inner, true),
            Saved::Sequence(items) => self.sequence(items),
            Saved::Items(items) => items.iter().for_each(|item| self.go(item, false)),
            Saved::Map(pairs) => {
                for (key, value) in pairs {
                    let part = key_text(key)
                        .map_or_else(|| "[...]".to_owned(), |text| format!("[{}]", quoted(&text)));
                    self.path.push(part);
                    self.go(key, false);
                    self.go(value, false);
                    self.path.pop();
                }
            }
            Saved::Record(fields) | Saved::CaseRecord(_, fields) => {
                for (name, value) in fields {
                    let (FieldName::Own(name) | FieldName::Renamed(name)) = name;
                    self.path.push(name.clone());
                    self.go(value, false);
                    self.path.pop();
                }
            }
            Saved::Table { kind, tag, ids, items } => {
                self.table(kind, *tag, ids);
                self.sequence(items);
            }
            Saved::Id { kind, id } => {
                let at = self.here();
                self.ids.push(FoundId {
                    kind: kind.clone(),
                    key: (id.slot, id.generation, id.tag),
                    optional,
                    at,
                });
            }
            _ => {}
        }
    }

    fn sequence(&mut self, items: &[Saved]) {
        for (at, item) in items.iter().enumerate() {
            self.path.push(format!("[{at}]"));
            self.go(item, false);
            self.path.pop();
        }
    }

    fn table(&mut self, kind: &str, tag: u64, ids: &[Id]) {
        let here = self.here();
        if let Some((there, _)) = self.tables.get(kind) {
            if self.problem.is_none() {
                self.problem = Some(format!(
                    "{there} and {here} are both a `Table[{kind}]`, so an Id can't say which it's in"
                ));
            }
            return;
        }
        self.tables.insert(kind.to_owned(), (here, tag));
        let positions =
            ids.iter().enumerate().map(|(at, id)| ((id.slot, id.generation, id.tag), at)).collect();
        self.positions.insert(kind.to_owned(), positions);
    }

    /// What's wrong with saving the `Id`s and `Table`s gone through, if anything is.
    fn problem(&self) -> Option<String> {
        if let Some(problem) = &self.problem {
            return Some(problem.clone());
        }
        for found in &self.ids {
            let kind = &found.kind;
            let Some(positions) = self.positions.get(kind) else {
                return Some(format!(
                    "{} is an Id, but nothing saved with it is a `Table[{kind}]`",
                    found.at
                ));
            };
            if found.optional {
                continue;
            }
            let (there, tag) = self.tables.get(kind).cloned().unwrap_or_else(|| ("its Table".to_owned(), 0));
            if found.key.2 != tag {
                return Some(format!("{} is an Id from another `Table[{kind}]` than {there}", found.at));
            }
            if !positions.contains_key(&found.key) {
                return Some(format!("{} is an Id of something that's been taken out of {there}", found.at));
            }
        }
        None
    }
}

/// Where a path through a value is, as messages say it: "`units[0].target`", or "the value".
fn place_text(path: &[String]) -> String {
    let mut out = String::new();
    for part in path {
        if !out.is_empty() && !part.starts_with('[') {
            out.push('.');
        }
        out.push_str(part);
    }
    if out.is_empty() { "the value".to_owned() } else { format!("`{out}`") }
}
