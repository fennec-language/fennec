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
use fennec_syntax::{Expr, Span};

use super::parse::{self, Entry, Node, cut_input, holds_too_big, is_float_text};
use super::{Names, Options, SavedField, article, parse_error, spelled_as, type_name};
use crate::interp::calls::ArgVal;
use crate::interp::{Flow, Interp, Run, unsupported};
use crate::load::Kind;
use crate::value::{Cap, Held, Id, Instance, Table, Text, Value};

/// How many fields a struct may have for the oracle to read it: past this, the standard library
/// stops telling a field given twice.
const MOST_FIELDS: usize = 62;

/// What a read keeps track of as it goes.
pub(crate) struct Reading {
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

/// A type's own `def deserialize` reading a value as `json.loads` gives it the value: the value,
/// where the read is, and whether the value has been read yet.
pub(crate) struct Loading {
    node: Node,
    reading: Reading,
    read: bool,
}

impl Reading {
    fn new(options: Options) -> Reading {
        Reading { options, path: Vec::new(), ids: Vec::new(), tables: Vec::new(), tags: HashMap::new() }
    }

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
        let mut reading = Reading::new(options);
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
            // Read as a float first, as std does to check the number, and then straight to the
            // nearest `f32`, rounded once.
            (Type::Sized(fennec_check::Num::F32), Node::Number(number)) => {
                match (number.parse::<f64>(), number.parse::<f32>()) {
                    (Ok(wide), _) if wide.is_infinite() => {
                        Err(says(format!("{} is too big for a float", reading.here())))
                    }
                    (_, Ok(x)) if x.is_infinite() => {
                        Err(says(format!("{} is too big for an f32", reading.here())))
                    }
                    (_, Ok(x)) => Ok(Value::F32(x)),
                    (_, Err(_)) => unsupported("a number Rust can't read"),
                }
            }
            (Type::Sized(fennec_check::Num::F32), _) => Err(reading.misfit("f32", node)),
            (Type::Bool, Node::Bool(truth)) => Ok(Value::Bool(*truth)),
            (Type::Bool, _) => Err(reading.misfit("bool", node)),
            (Type::Str | Type::LiteralStr, Node::Str(text)) => Ok(Value::Str(Text::from(text.clone()))),
            (Type::Str | Type::LiteralStr, _) => Err(reading.misfit("str", node)),
            (Type::Optional(_), Node::Null) => Ok(Value::None),
            (Type::Optional(inner), _) => self.read(node, inner, reading, in_key),
            (Type::List(inner), Node::Array(items)) => {
                self.read_items(items, inner, reading).map(|items| Value::List(Held::new(items)))
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
            (Type::Union(members), _) => self.read_union(node, members, reading),
            (Type::Bytes, _) => unsupported("loading bytes from JSON"),
            (other, _) => unsupported(format!("loading a {} from JSON", type_name(other))),
        }
    }

    /// A union's value, as std's JSON reads one: an object whose first key names a member, by its
    /// type's name, and whose value is that member's. A key after it is a failure once the
    /// member's value has read, naming the first such key, as an enum's case does.
    fn read_union(&mut self, node: &Node, members: &[Type], reading: &mut Reading) -> Run<Value> {
        let names: Vec<String> = members.iter().map(member_name).collect();
        let union = names.join(" | ");
        let Node::Object(entries) = node else { return Err(reading.misfit(&union, node)) };
        let Some(first) = entries.first() else { return Err(reading.misfit(&union, node)) };
        let Some(member) = names.iter().position(|name| *name == first.key).and_then(|at| members.get(at))
        else {
            return Err(says(format!("{} is \"{}\", which is none of `{union}`", reading.here(), first.key)));
        };
        reading.path.push(cut_input(&first.key));
        let value = self.read(&first.value, member, reading, false);
        reading.path.pop();
        let value = value?;
        if let Some(after) = entries.get(1) {
            return Err(says(format!(
                "{} has \"{}\" after its member, but a union is written as an object with its member as the only key",
                reading.here(),
                cut_input(&after.key)
            )));
        }
        Ok(value)
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
        Ok(Value::Tuple(Held::new(values)))
    }

    /// A dict: an object, or an array of `[key, value]` pairs. A key given twice fails, naming it,
    /// once every pair has been read.
    fn read_dict(
        &mut self,
        node: &Node,
        (key_ty, value_ty): (&Type, &Type),
        reading: &mut Reading,
    ) -> Run<Value> {
        let pairs = self.read_entries(node, (key_ty, value_ty), reading)?;
        let mut dict: Vec<(Value, Value)> = Vec::with_capacity(pairs.len());
        for (key, value) in pairs {
            if crate::interp::find_key(&dict, &key)?.is_some() {
                let twice = twice_key(&self.display(&key)?);
                return Err(says(format!("{} has {twice} twice", reading.here())));
            }
            dict.push((key, value));
        }
        Ok(Value::Dict(Held::new(dict)))
    }

    /// A dict's pairs, in the order the text gives them: an object's, or an array's of
    /// `[key, value]` pairs.
    fn read_entries(
        &mut self,
        node: &Node,
        (key_ty, value_ty): (&Type, &Type),
        reading: &mut Reading,
    ) -> Run<Vec<(Value, Value)>> {
        match node {
            Node::Array(items) => self.read_pairs(items, (key_ty, value_ty), reading),
            Node::Object(entries) => {
                let mut pairs = Vec::with_capacity(entries.len());
                for entry in entries {
                    let key = self.read(&Node::Str(entry.key.clone()), key_ty, reading, true)?;
                    reading.path.push(format!("[{}]", cut_input(&entry.written)));
                    let value = self.read(&entry.value, value_ty, reading, false)?;
                    reading.path.pop();
                    pairs.push((key, value));
                }
                Ok(pairs)
            }
            _ => {
                let wanted = format!("dict[{}, {}]", type_name(key_ty), type_name(value_ty));
                Err(reading.misfit(&wanted, node))
            }
        }
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
        let tag = self.tag_of(&kind, reading);
        let mut table = Table::new(tag);
        for item in items {
            table.add(item, tag);
        }
        Ok(Value::Table(Box::new(table)))
    }

    /// A struct or enum the program declares.
    fn read_declared(&mut self, node: &Node, name: &str, args: &[Type], reading: &mut Reading) -> Run<Value> {
        // Looked up by the name it's checked under, and named in messages as it's written.
        let shown = fennec_syntax::rename::shown_type_name(name);
        if let Some(def) = self.program.method(name, "deserialize") {
            return self.read_through_own(node, def, (name, args), reading);
        }
        match self.saved_type(name)? {
            Kind::Struct => {
                let Node::Object(entries) = node else { return Err(reading.misfit(shown, node)) };
                let fields = self.saved_fields(name, None, args)?;
                let values = self.read_record(entries, &fields, shown, reading)?;
                let instance =
                    Instance { ty: name.to_owned(), case: None, fields: values, notes: Vec::new() };
                if let Some(broken) = self.broken_rule(&instance)? {
                    return Err(says(format!(
                        "in {}, {shown}.{broken} breaks its `where` rule",
                        reading.here()
                    )));
                }
                Ok(Value::Instance(Held::new(instance)))
            }
            Kind::Enum => self.read_case(node, name, args, reading),
            Kind::Error => unsupported(format!("loading a {name} from JSON")),
        }
    }

    /// A value of a type that writes its own `def deserialize`, which runs given a `Deserializer`
    /// over `node`, as `loader_method` reads it.
    fn read_through_own(
        &mut self,
        node: &Node,
        def: &'static fennec_syntax::FnDef,
        (name, args): (&str, &[Type]),
        reading: &mut Reading,
    ) -> Run<Value> {
        if def.takes_self() {
            return unsupported(format!("a {name} whose `deserialize` takes `self`"));
        }
        let bound = self.own_bound(def, name, args)?;
        let src = self.hidden_local(Value::Cap(Cap::Loader))?;
        let options = reading.options;
        let held = std::mem::replace(reading, Reading::new(options));
        self.loadings.push(Loading { node: node.clone(), reading: held, read: false });
        let given = vec![ArgVal::Ref(None, src.clone())];
        let ran = self.call_def_bound((def, bound), None, None, given, Span::default());
        self.drop_hidden(&src);
        if let Some(done) = self.loadings.pop() {
            *reading = done.reading;
        }
        ran
    }

    /// A method of the `Deserializer` a type's own `def deserialize` is given by `json.loads`:
    /// one that reads takes the whole value, as the type the checker gave the call, and one
    /// that makes a failure says where the read is. Reading through `record` and its fields, or
    /// more than one value, isn't modeled.
    pub(crate) fn loader_method(&mut self, name: &str, args: &[Value], expr: &'static Expr) -> Run<Value> {
        let Some(mut loading) = self.loadings.pop() else {
            return unsupported("JSON's `Deserializer` used outside `json.loads`");
        };
        let done = self.loader_call(&mut loading, name, args, expr);
        self.loadings.push(loading);
        done
    }

    fn loader_call(
        &mut self,
        loading: &mut Loading,
        name: &str,
        args: &[Value],
        expr: &'static Expr,
    ) -> Run<Value> {
        let reading = &mut loading.reading;
        match (name, args) {
            ("exact", []) => return Ok(Value::Bool(false)),
            ("none_next", []) => return Ok(Value::Bool(matches!(loading.node, Node::Null))),
            ("at_item", [Value::Int(at), Value::Str(message)]) => {
                reading.path.push(format!("[{at}]"));
                let here = reading.here();
                reading.path.pop();
                return Ok(parse_error(format!("{here} {message}"), 0, 0));
            }
            ("here", [Value::Str(message)]) => {
                return Ok(parse_error(format!("{} {message}", reading.here()), 0, 0));
            }
            ("int" | "float" | "bool" | "text" | "value" | "sequence" | "map" | "none", []) => {}
            _ => return unsupported(format!("a type's own `deserialize` calling `{name}` as JSON is read")),
        }
        if loading.read {
            return unsupported("a type's own `deserialize` that reads more than one value");
        }
        if name == "none" {
            // Only a missing value is taken; anything else is left for the read that follows.
            if matches!(loading.node, Node::Null) {
                loading.read = true;
                return Ok(Value::Bool(true));
            }
            return Ok(Value::Bool(false));
        }
        loading.read = true;
        let ty = match self.static_type(expr.span) {
            Some(Type::Result(value, _)) => *value,
            Some(ty) => ty,
            None => return unsupported(format!("`{name}` read as a type that isn't known")),
        };
        if name != "map" {
            return self.read(&loading.node, &ty, &mut loading.reading, false);
        }
        let Type::List(pair) = &ty else { return unsupported("`map` read as what isn't a list of pairs") };
        let Type::Tuple(types) = pair.as_ref() else {
            return unsupported("`map` read as what isn't a list of pairs");
        };
        let [key_ty, value_ty] = types.as_slice() else {
            return unsupported("`map` read as what isn't a list of pairs");
        };
        if !matches!(loading.node, Node::Object(_) | Node::Array(_)) {
            return unsupported("a map read from what's neither an object nor an array");
        }
        let pairs = self.read_entries(&loading.node, (key_ty, value_ty), &mut loading.reading)?;
        for (at, (key, _)) in pairs.iter().enumerate() {
            if crate::interp::find_key(pairs.get(..at).unwrap_or_default(), key)?.is_some() {
                return unsupported("a map read with a key given twice");
            }
        }
        Ok(Value::List(
            pairs.into_iter().map(|(key, value)| Value::Tuple(Held::new(vec![key, value]))).collect(),
        ))
    }

    /// An enum's case: its name in quotes when it has no fields, or an object holding its fields
    /// under its name.
    fn read_case(&mut self, node: &Node, name: &str, args: &[Type], reading: &mut Reading) -> Run<Value> {
        let shown = fennec_syntax::rename::shown_type_name(name);
        let Some(declared) = self.program.types.get(name) else {
            return unsupported(format!("the enum {shown}"));
        };
        let cases = &declared.def.cases;
        if cases.iter().any(|case| case.fields.iter().any(|field| field.rule.is_some())) {
            return unsupported("loading a case with a `where` rule from JSON");
        }
        let entries = match node {
            Node::Str(key) => {
                let Some(case) = cases.iter().find(|case| case.name == *key) else {
                    return Err(says(format!(
                        "{} is \"{key}\", which isn't a case of `{shown}`",
                        reading.here()
                    )));
                };
                if !case.fields.is_empty() {
                    return Err(says(format!(
                        "{} is `{shown}.{key}`, which needs its fields",
                        reading.here()
                    )));
                }
                return Ok(Value::instance(name, Some(key), Vec::new()));
            }
            Node::Object(entries) => entries,
            _ => return Err(reading.misfit(shown, node)),
        };
        let Some(Entry { key, value: Node::Object(inner), .. }) = entries.first() else {
            return Err(reading.misfit(shown, node));
        };
        let Some(case) = cases.iter().find(|case| case.name == *key) else {
            return Err(says(format!("{} is \"{key}\", which isn't a case of `{shown}`", reading.here())));
        };
        let values = if case.fields.is_empty() {
            if let Some(first) = inner.first() {
                let field = cut_input(&first.key);
                return Err(says(format!(
                    "`{shown}.{key}` has no field \"{field}\", but {} has one",
                    reading.here()
                )));
            }
            Vec::new()
        } else {
            let fields = self.saved_fields(name, Some(key), args)?;
            self.read_record(inner, &fields, &format!("{shown}.{key}"), reading)?
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
        Type::Sized(fennec_check::Num::F32) => "f32".to_owned(),
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
pub(crate) fn twice_key(shown: &str) -> String {
    let cut = if shown.chars().count() <= 40 {
        shown.to_owned()
    } else {
        format!("{}…", shown.chars().take(20).collect::<String>())
    };
    format!("`{}`", parse::escaped(&cut))
}

/// A union's member as a union names it, saved or read: its type as written.
pub(crate) fn member_name(member: &Type) -> String {
    member.source().map_or_else(
        || type_name(member),
        |source| fennec_syntax::rename::shown_type_name(&source).to_owned(),
    )
}
