//! A type's layout, as `std.pack` saves it beside the data: what each value is, with every
//! struct's field names and every enum's case names. Bytes written for an earlier version of a
//! type load by those names, so a field the data has and the type doesn't is passed over, and one
//! the type has and the data doesn't takes its default.
//!
//! The layout is text, a word at a time with a space between:
//!
//! - `i`, `f`, `b`, `s` and `y`: an int, a float, a bool, text and bytes;
//! - `l` then the item: a list, a set, or what saves as one, like a `Deque`;
//! - `m` then the key and the value: a dict;
//! - `o` then what it holds: an optional;
//! - `t`, a count, then each item: a tuple;
//! - `S`, a count, then each field's name and what it holds: a struct;
//! - `E`, a count, then each case's name, its count of fields, and each field's name and what it
//!   holds: an enum;
//! - `u`, a count, then each member's name and what it is: a union;
//! - `r` then a number: the struct or enum that many places out from the first one open, for a
//!   type inside itself;
//! - `T` then the item: a `Table`; `I`: an `Id`;
//! - `O` then the name: a type that saves itself its own way, read as it reads;
//! - `?`: something no format saves.

use std::fmt::Write as _;

use fennec_check::Type;

use super::Context;

/// The name the kit loader gives a kit's `pack.lock` in its program, as `fennec_kit::PACK_LOCK`.
const PACK_LOCK: &str = "\0pack_lock";

impl Context<'_> {
    /// The kit's `pack.lock`: a line for each layout it has saved, `<shape in hex> <layout>`. Empty
    /// when it has none.
    pub(super) fn pack_lock(&self) -> String {
        let Some(written) = self.constants.get(PACK_LOCK) else { return String::new() };
        match &written.value.kind {
            fennec_syntax::ExprKind::Bytes(data) => String::from_utf8_lossy(&data.0).into_owned(),
            _ => String::new(),
        }
    }

    /// The layout of `ty`, as its values are saved.
    pub(super) fn layout(&self, ty: &Type) -> String {
        let mut out = String::new();
        self.layout_into(ty, &mut Vec::new(), &mut out);
        out.trim_end().to_owned()
    }

    fn layout_into(&self, ty: &Type, open: &mut Vec<String>, out: &mut String) {
        let mut word = |text: &str| {
            out.push_str(text);
            out.push(' ');
        };
        match ty {
            Type::Int => word("i"),
            Type::Float => word("f"),
            Type::Bool => word("b"),
            Type::Str => word("s"),
            Type::Bytes => word("y"),
            Type::List(item) | Type::Set(item) => {
                word("l");
                self.layout_into(item, open, out);
            }
            Type::Optional(item) => {
                word("o");
                self.layout_into(item, open, out);
            }
            Type::Dict(key, value) => {
                word("m");
                self.layout_into(key, open, out);
                self.layout_into(value, open, out);
            }
            Type::Tuple(items) => {
                let _ = write!(out, "t {} ", items.len());
                for item in items {
                    self.layout_into(item, open, out);
                }
            }
            Type::Union(members) => {
                let _ = write!(out, "u {} ", members.len());
                for member in members {
                    let _ = write!(out, "{} ", member.source().unwrap_or_default().replace(' ', ""));
                    self.layout_into(member, open, out);
                }
            }
            Type::Table(item) => {
                word("T");
                self.layout_into(item, open, out);
            }
            Type::Id(_) => word("I"),
            Type::Named(name) => self.named_layout(name, open, out),
            Type::Generic(name, args) => {
                let copy =
                    self.concrete(&Type::Generic(name.clone(), args.clone()), &super::generics::Subst::new());
                if let Type::Named(name) = copy {
                    self.named_layout(&name, open, out);
                } else {
                    word("?");
                }
            }
            _ => word("?"),
        }
    }

    /// A struct's or an enum's layout, or what one of the standard library's types saves as.
    fn named_layout(&self, name: &str, open: &mut Vec<String>, out: &mut String) {
        let instance = self.type_instance(name);
        let base = instance.as_ref().map_or(name, |instance| instance.base.as_str());
        let subst = instance.as_ref().map(|instance| instance.subst.clone()).unwrap_or_default();
        let args: Vec<Type> = self
            .generic_types
            .get(base)
            .map(|def| def.generics.iter().filter_map(|generic| subst.get(&generic.name).cloned()).collect())
            .unwrap_or_default();
        let std_saves = self.library.contains(format!("{base}.serialize").as_str());
        if std_saves && let Some(saved_as) = std_saved_as(base, &args) {
            self.layout_into(&saved_as, open, out);
            return;
        }
        if let Some(at) = open.iter().position(|seen| seen == name) {
            let _ = write!(out, "r {at} ");
            return;
        }
        if self.saves_itself.contains(base) {
            let _ = write!(out, "O {} ", name.replace(' ', ""));
            return;
        }
        let signatures = self.signatures;
        open.push(name.to_owned());
        if let Some(found) = signatures.structure(base) {
            let saved: Vec<(&str, Type)> = found
                .fields
                .iter()
                .filter_map(|(field, ty)| {
                    let written = found.annotations.get(field);
                    let skipped =
                        written.is_some_and(|all| all.iter().any(|annotation| annotation.name == "skip"));
                    let shown = written.and_then(|all| renamed(all)).unwrap_or(field);
                    (!skipped).then(|| (shown, self.concrete(ty, &subst)))
                })
                .collect();
            let _ = write!(out, "S {} ", saved.len());
            for (field, ty) in saved {
                let _ = write!(out, "{field} ");
                self.layout_into(&ty, open, out);
            }
        } else if let Some(found) = signatures.enumeration(base) {
            let _ = write!(out, "E {} ", found.cases.len());
            for (case, fields) in &found.cases {
                let _ = write!(out, "{case} {} ", fields.len());
                for (field, ty) in fields {
                    let _ = write!(out, "{field} ");
                    self.layout_into(&self.concrete(ty, &subst), open, out);
                }
            }
        } else {
            out.push_str("? ");
        }
        open.pop();
    }
}

/// The name a field is saved under, when `@serialize(name=...)` gives it one.
fn renamed(annotations: &[fennec_syntax::Annotation]) -> Option<&str> {
    let annotation = annotations.iter().find(|annotation| annotation.name == "serialize")?;
    annotation.args.iter().find_map(|arg| match &arg.value.kind {
        fennec_syntax::ExprKind::Str(name) if arg.name.as_deref() == Some("name") => Some(name.as_str()),
        _ => None,
    })
}

/// What one of the standard library's types that saves itself writes, as a type that saves the
/// same way: a `Deque` saves as a list of its items, a `Time` as its text.
fn std_saved_as(base: &str, args: &[Type]) -> Option<Type> {
    let first = || args.first().cloned().unwrap_or(Type::Unknown);
    Some(match base {
        "Deque" | "SortedSet" => Type::List(Box::new(first())),
        "SortedMap" => Type::Dict(Box::new(first()), Box::new(args.get(1).cloned().unwrap_or(Type::Unknown))),
        "Packed" | "Grid" => Type::List(Box::new(Type::List(Box::new(first())))),
        "Uuid" | "Time" => Type::Str,
        "Duration" => Type::Int,
        _ => return None,
    })
}

/// A field as it was saved: its name, and what it held.
type Field = (String, Saved);

/// A layout read back from its text, as `pack.lock` holds it.
#[derive(Debug)]
enum Saved {
    /// One of `i`, `f`, `b`, `s`, `y`, `I`, `?`: what it is, by its letter.
    Plain(char),
    List(Box<Saved>),
    Map(Box<Saved>, Box<Saved>),
    Optional(Box<Saved>),
    Tuple(Vec<Saved>),
    Struct(Vec<(String, Saved)>),
    Enum(Vec<(String, Vec<(String, Saved)>)>),
    Union(Vec<(String, Saved)>),
    Table(Box<Saved>),
    /// A type that saves itself, by its name.
    Own(String),
    /// A struct or enum inside itself, which the outer one is compared as.
    Same,
}

/// The layout in `text`, or `None` if it isn't one `std.pack` wrote.
fn saved_layout(text: &str) -> Option<Saved> {
    let mut words = text.split(' ');
    let found = saved_node(&mut words)?;
    words.next().is_none().then_some(found)
}

fn saved_node<'t>(words: &mut impl Iterator<Item = &'t str>) -> Option<Saved> {
    let count = |words: &mut dyn Iterator<Item = &'t str>| words.next()?.parse::<usize>().ok();
    Some(match words.next()? {
        "l" => Saved::List(Box::new(saved_node(words)?)),
        "o" => Saved::Optional(Box::new(saved_node(words)?)),
        "T" => Saved::Table(Box::new(saved_node(words)?)),
        "m" => Saved::Map(Box::new(saved_node(words)?), Box::new(saved_node(words)?)),
        "t" => Saved::Tuple((0..count(words)?).map(|_| saved_node(words)).collect::<Option<_>>()?),
        "S" => {
            let fields = count(words)?;
            Saved::Struct(named_nodes(words, fields)?)
        }
        "u" => {
            let members = count(words)?;
            Saved::Union(named_nodes(words, members)?)
        }
        "E" => {
            let cases = (0..count(words)?)
                .map(|_| {
                    let name = words.next()?.to_owned();
                    let fields = count(words)?;
                    Some((name, named_nodes(words, fields)?))
                })
                .collect::<Option<_>>()?;
            Saved::Enum(cases)
        }
        "r" => {
            words.next()?;
            Saved::Same
        }
        "O" => Saved::Own(words.next()?.to_owned()),
        letter @ ("i" | "f" | "b" | "s" | "y" | "I" | "?") => Saved::Plain(letter.chars().next()?),
        _ => return None,
    })
}

fn named_nodes<'t>(words: &mut impl Iterator<Item = &'t str>, count: usize) -> Option<Vec<(String, Saved)>> {
    (0..count).map(|_| Some((words.next()?.to_owned(), saved_node(words)?))).collect()
}

impl Context<'_> {
    /// Notes why `ty`, called `name`, can't load what its kit's `pack.lock` holds for a type of
    /// that name in another shape: data saved then would fail to load now.
    pub(super) fn check_against_lock(&self, ty: &Type, name: &str, shape: i64) {
        let lock = self.pack_lock();
        let own = format!("{:016x} ", shape.cast_unsigned());
        for line in lock.lines().filter(|line| !line.starts_with(&own)) {
            let mut parts = line.splitn(3, ' ');
            let (Some(_), Some(locked_name), Some(layout)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            if locked_name != name {
                continue;
            }
            let Some(saved) = saved_layout(layout) else { continue };
            let mut found = Vec::new();
            self.loads_from(&saved, ty, name, &mut Vec::new(), &mut found);
            let mut problems = self.layout_problems.borrow_mut();
            for problem in found {
                if !problems.contains(&problem) {
                    problems.push(problem);
                }
            }
        }
    }

    /// Why a `ty` can't be loaded from data `saved` that way, onto `found`, at `path` in it.
    fn loads_from(
        &self,
        saved: &Saved,
        ty: &Type,
        path: &str,
        open: &mut Vec<String>,
        found: &mut Vec<String>,
    ) {
        let mut mismatch = |was: &str| {
            found.push(format!("`{path}` was saved as {was}, which it can't load as now"));
        };
        match (ty, saved) {
            (Type::Optional(inner), Saved::Optional(held)) => self.loads_from(held, inner, path, open, found),
            (Type::Optional(inner), _) => self.loads_from(saved, inner, path, open, found),
            (_, Saved::Optional(held)) => self.loads_from(held, ty, path, open, found),
            // What loads as it is, and a struct inside itself, which the outer one is compared as.
            (_, Saved::Same)
            | (Type::Int | Type::Id(_), Saved::Plain('i' | 'I'))
            | (Type::Float, Saved::Plain('i' | 'f'))
            | (Type::Bool, Saved::Plain('b'))
            | (Type::Str, Saved::Plain('s'))
            | (Type::Bytes, Saved::Plain('y')) => {}
            (Type::Int | Type::Float | Type::Bool | Type::Str | Type::Bytes, _) => {
                mismatch(&described(saved));
            }
            (Type::List(item) | Type::Set(item), Saved::List(held))
            | (Type::Table(item), Saved::Table(held)) => {
                self.loads_from(held, item, &format!("{path}[]"), open, found);
            }
            (Type::Dict(key, value), Saved::Map(held_key, held_value)) => {
                self.loads_from(held_key, key, &format!("{path}[key]"), open, found);
                self.loads_from(held_value, value, &format!("{path}[]"), open, found);
            }
            (Type::Tuple(items), Saved::Tuple(held)) if items.len() == held.len() => {
                for (at, (item, held)) in items.iter().zip(held).enumerate() {
                    self.loads_from(held, item, &format!("{path}[{at}]"), open, found);
                }
            }
            (Type::Union(members), Saved::Union(held)) => {
                for (member, saved_member) in held {
                    let now =
                        members.iter().find(|ty| ty.source().unwrap_or_default().replace(' ', "") == *member);
                    match now {
                        Some(now) => self.loads_from(saved_member, now, path, open, found),
                        None => found.push(format!(
                            "`{path}` may hold a `{member}`, saved before, which isn't one of its members now"
                        )),
                    }
                }
            }
            (Type::Named(_) | Type::Generic(..), _) => self.named_loads_from(saved, ty, path, open, found),
            _ => mismatch(&described(saved)),
        }
    }

    /// A struct or an enum loaded from what was saved: by its fields' and cases' names.
    fn named_loads_from(
        &self,
        saved: &Saved,
        ty: &Type,
        path: &str,
        open: &mut Vec<String>,
        found: &mut Vec<String>,
    ) {
        let concrete = self.concrete(ty, &super::generics::Subst::new());
        let Type::Named(name) = &concrete else { return };
        let name = name.clone();
        let instance = self.type_instance(&name);
        let base = instance.as_ref().map_or(name.as_str(), |instance| instance.base.as_str()).to_owned();
        let subst = instance.as_ref().map(|instance| instance.subst.clone()).unwrap_or_default();
        if open.contains(&name) || self.saves_itself.contains(&base) {
            return;
        }
        if self.library.contains(format!("{base}.serialize").as_str()) {
            let args: Vec<Type> = self
                .generic_types
                .get(base.as_str())
                .map(|def| {
                    def.generics.iter().filter_map(|generic| subst.get(&generic.name).cloned()).collect()
                })
                .unwrap_or_default();
            if let Some(saved_as) = std_saved_as(&base, &args) {
                return self.loads_from(saved, &saved_as, path, open, found);
            }
        }
        open.push(name.clone());
        let signatures = self.signatures;
        match (signatures.structure(&base), signatures.enumeration(&base), saved) {
            (Some(structure), _, Saved::Struct(held)) => {
                let fields: Vec<(String, Type)> = structure
                    .fields
                    .iter()
                    .filter(|(field, _)| {
                        !structure
                            .annotations
                            .get(field)
                            .is_some_and(|all| all.iter().any(|a| a.name == "skip"))
                    })
                    .map(|(field, ty)| {
                        let shown =
                            structure.annotations.get(field).and_then(|all| renamed(all)).unwrap_or(field);
                        (shown.to_owned(), self.concrete(ty, &subst))
                    })
                    .collect();
                self.fields_load_from((held, &fields), (&base, None), path, open, found);
            }
            (_, Some(enumeration), Saved::Enum(held)) => {
                for (case, held_fields) in held {
                    let Some((_, fields)) = enumeration.cases.iter().find(|(now, _)| now == case) else {
                        found.push(format!(
                            "`{path}` may hold `{case}`, saved before, a case it doesn't have now"
                        ));
                        continue;
                    };
                    let fields: Vec<(String, Type)> =
                        fields.iter().map(|(field, ty)| (field.clone(), self.concrete(ty, &subst))).collect();
                    self.fields_load_from(
                        (held_fields, &fields),
                        (&base, Some(case)),
                        &format!("{path}.{case}"),
                        open,
                        found,
                    );
                }
            }
            (Some(_) | None, _, _) => {
                found.push(format!("`{path}` was saved as {}, which it can't load as now", described(saved)));
            }
        }
        open.pop();
    }

    /// A struct's or a case's fields loaded from the ones saved, by name. A field that's new needs
    /// a default, or to be optional, and one that's gone has to be one that can be passed over.
    fn fields_load_from(
        &self,
        (held, fields): (&[Field], &[(String, Type)]),
        (owner, case): (&str, Option<&str>),
        path: &str,
        open: &mut Vec<String>,
        found: &mut Vec<String>,
    ) {
        for (field, ty) in fields {
            if let Some((_, saved)) = held.iter().find(|(saved, _)| saved == field) {
                self.loads_from(saved, ty, &format!("{path}.{field}"), open, found);
                continue;
            }
            let defaulted = self.defaults.contains_key(&(owner, case, field.as_str()));
            if !defaulted && !matches!(ty, Type::Optional(_)) {
                found.push(format!(
                    "`{path}.{field}` has no default, and data saved before it doesn't have one"
                ));
            }
        }
        for (gone, saved) in held {
            if let Saved::Own(kind) = saved
                && !fields.iter().any(|(field, _)| field == gone)
            {
                found.push(format!(
                    "`{path}.{gone}` is gone, and holds a `{kind}`, which saves itself, so saved data can't pass over it"
                ));
            }
        }
    }
}

/// What a saved value was, in words.
fn described(saved: &Saved) -> String {
    match saved {
        Saved::Plain('i' | 'I') => "an int".to_owned(),
        Saved::Plain('f') => "a float".to_owned(),
        Saved::Plain('b') => "a bool".to_owned(),
        Saved::Plain('s') => "text".to_owned(),
        Saved::Plain('y') => "bytes".to_owned(),
        Saved::List(_) | Saved::Table(_) => "a list".to_owned(),
        Saved::Map(..) => "a dict".to_owned(),
        Saved::Optional(_) => "an optional".to_owned(),
        Saved::Tuple(items) => format!("a tuple of {}", items.len()),
        Saved::Struct(_) => "a struct".to_owned(),
        Saved::Enum(_) => "an enum".to_owned(),
        Saved::Union(_) => "a union".to_owned(),
        Saved::Own(name) => format!("a `{name}`"),
        Saved::Plain(_) | Saved::Same => "something else".to_owned(),
    }
}
