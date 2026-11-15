//! A type's shape: what a format that saves no names has to agree on to read back what it
//! wrote. Each struct's fields, in order, with their names and the shapes of their types, and
//! each enum's cases, in order, with theirs. Two types with the same shape read each other's
//! data; any change to a field, its type or its place changes the shape.

use std::fmt::Write as _;

use crate::Signatures;
use crate::types::Type;

impl Signatures {
    /// The shape of `ty`, written out. A struct or enum inside itself, as in a tree of nodes, is
    /// written by its name the second time.
    pub fn shape(&self, ty: &Type) -> String {
        let mut out = String::new();
        self.shape_into(ty, &mut Vec::new(), &mut out);
        out
    }

    fn shape_into(&self, ty: &Type, open: &mut Vec<String>, out: &mut String) {
        match ty {
            Type::List(item) => self.wrapped(("list[", "]"), &[item], open, out),
            Type::Buffer(item) => self.wrapped(("Buffer[", "]"), &[item], open, out),
            Type::Set(item) => self.wrapped(("set[", "]"), &[item], open, out),
            Type::Iterator(item) => self.wrapped(("Iterator[", "]"), &[item], open, out),
            Type::Id(item) => self.wrapped(("Id[", "]"), &[item], open, out),
            Type::Table(item) => self.wrapped(("Table[", "]"), &[item], open, out),
            Type::Optional(item) => self.wrapped(("", "?"), &[item], open, out),
            Type::Dict(key, value) => self.wrapped(("dict[", "]"), &[key, value], open, out),
            Type::Result(value, error) => self.wrapped(("Result[", "]"), &[value, error], open, out),
            Type::Array(item, size) => {
                out.push_str("array[");
                self.shape_into(item, open, out);
                let _ = write!(out, ", {size}]");
            }
            Type::Tuple(items) => {
                let items: Vec<&Type> = items.iter().collect();
                self.wrapped(("(", ")"), &items, open, out);
            }
            // Each member's shape, in order: a member's place is what a format saving no names
            // writes for it.
            Type::Union(members) => {
                out.push('(');
                for (at, member) in members.iter().enumerate() {
                    if at > 0 {
                        out.push_str(" | ");
                    }
                    self.shape_into(member, open, out);
                }
                out.push(')');
            }
            Type::Named(name) => self.named_shape(name, &[], open, out),
            Type::Generic(name, args) => self.named_shape(name, args, open, out),
            other => {
                let _ = write!(out, "{other}");
            }
        }
    }

    /// `items`' shapes between `opening` and `closing`, apart by commas.
    fn wrapped(
        &self,
        (opening, closing): (&str, &str),
        items: &[&Type],
        open: &mut Vec<String>,
        out: &mut String,
    ) {
        out.push_str(opening);
        for (at, item) in items.iter().enumerate() {
            if at > 0 {
                out.push_str(", ");
            }
            self.shape_into(item, open, out);
        }
        out.push_str(closing);
    }

    /// A struct's or an enum's shape: its name and type arguments, then its fields or cases, a
    /// generic one's with its arguments filled in, as `Pair[int]{first: int, second: int}`. Only
    /// the same type with the same arguments inside itself is written by its name alone.
    fn named_shape(&self, name: &str, args: &[Type], open: &mut Vec<String>, out: &mut String) {
        let start = out.len();
        out.push_str(name);
        if !args.is_empty() {
            let args: Vec<&Type> = args.iter().collect();
            self.wrapped(("[", "]"), &args, open, out);
        }
        let instance = out.get(start..).unwrap_or(name).to_owned();
        if open.contains(&instance) {
            return;
        }
        open.push(instance);
        if let Some(found) = self.structure(name) {
            let saved = found.fields.iter().filter(|(field, _)| {
                !found
                    .annotations
                    .get(field)
                    .is_some_and(|written| written.iter().any(|annotation| annotation.name == "skip"))
            });
            let fields: Vec<(String, Type)> =
                saved.map(|(field, ty)| (field.clone(), filled(ty, &found.generics, args))).collect();
            let fields: Vec<&(String, Type)> = fields.iter().collect();
            self.fields_shape(&fields, ("{", "}"), open, out);
        } else if let Some(found) = self.enumeration(name) {
            out.push('<');
            for (at, (case, fields)) in found.cases.iter().enumerate() {
                if at > 0 {
                    out.push_str(", ");
                }
                out.push_str(case);
                if !fields.is_empty() {
                    let fields: Vec<(String, Type)> = fields
                        .iter()
                        .map(|(field, ty)| (field.clone(), filled(ty, &found.generics, args)))
                        .collect();
                    let fields: Vec<&(String, Type)> = fields.iter().collect();
                    self.fields_shape(&fields, ("(", ")"), open, out);
                }
            }
            out.push('>');
        }
        open.pop();
    }

    fn fields_shape(
        &self,
        fields: &[&(String, Type)],
        (opening, closing): (&str, &str),
        open: &mut Vec<String>,
        out: &mut String,
    ) {
        out.push_str(opening);
        for (at, (field, ty)) in fields.iter().enumerate() {
            if at > 0 {
                out.push_str(", ");
            }
            let _ = write!(out, "{field}: ");
            self.shape_into(ty, open, out);
        }
        out.push_str(closing);
    }
}

/// A field's type with the type's parameters, named in `generics`, filled in with `args`: `int` for
/// `T` in a `Pair[int]`.
fn filled(ty: &Type, generics: &[String], args: &[Type]) -> Type {
    ty.map(&mut |part| match part {
        Type::Param(name) => {
            let at = generics.iter().position(|generic| generic == name)?;
            args.get(at).cloned()
        }
        _ => None,
    })
}
