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
