//! Plain structs: structs whose fields are all ints, floats, bools or other plain structs,
//! with no `where` rules. Native code keeps them in memory it owns, laid out field after
//! field, 8 bytes each, a nested struct in line. Every other struct is an object the runtime
//! holds.

use std::collections::HashMap;

use fennec_check::{Signatures, Type};
use fennec_syntax::{Expr, TypeDef};

use crate::types::{Cap, Ty};

/// The size of every field that isn't a nested struct: an int, a float, or a bool.
pub(crate) const WORD: u32 = 8;

pub(crate) struct Field {
    pub(crate) name: String,
    pub(crate) ty: Ty,
    /// Where it starts, in bytes from the start of the struct.
    pub(crate) offset: u32,
    /// Its default, used when a value is made without it.
    pub(crate) default: Option<Expr>,
}

pub(crate) struct Layout {
    pub(crate) name: String,
    pub(crate) fields: Vec<Field>,
    pub(crate) size: u32,
}

#[derive(Default)]
pub(crate) struct Layouts {
    by_name: HashMap<String, u32>,
    all: Vec<Layout>,
}

impl Layouts {
    /// The layouts of every plain struct the checker knows, the standard library's included.
    pub(crate) fn new(signatures: &Signatures, names: impl Iterator<Item = String>) -> Layouts {
        let mut layouts = Layouts::default();
        for name in names {
            layouts.add(signatures, &name, &mut Vec::new());
        }
        layouts
    }

    /// Lays out a struct if it's plain, after the plain structs inside it. `visiting` stops
    /// a struct that contains itself.
    fn add(&mut self, signatures: &Signatures, name: &str, visiting: &mut Vec<String>) -> Option<u32> {
        if let Some(id) = self.by_name.get(name) {
            return Some(*id);
        }
        let structure = signatures.structure(name)?;
        if structure.rules || visiting.iter().any(|seen| seen == name) {
            return None;
        }
        visiting.push(name.to_owned());
        let mut fields = Vec::with_capacity(structure.fields.len());
        let mut offset = 0;
        for (field, ty) in &structure.fields {
            let (ty, size) = match ty {
                Type::Int => (Ty::Int, WORD),
                Type::Float => (Ty::Float, WORD),
                Type::Bool => (Ty::Bool, WORD),
                Type::Named(inner) => {
                    let id = self.add(signatures, inner, visiting)?;
                    (Ty::Struct(id), self.get(id)?.size)
                }
                _ => return None,
            };
            fields.push(Field { name: field.clone(), ty, offset, default: None });
            offset += size;
        }
        visiting.pop();
        let id = u32::try_from(self.all.len()).ok()?;
        self.all.push(Layout { name: name.to_owned(), fields, size: offset.max(WORD) });
        self.by_name.insert(name.to_owned(), id);
        Some(id)
    }

    /// Records the defaults a struct's definition gives its fields.
    pub(crate) fn add_defaults(&mut self, def: &TypeDef) {
        let Some(layout) =
            self.by_name.get(&def.name).and_then(|id| self.all.get_mut(usize::try_from(*id).ok()?))
        else {
            return;
        };
        for field in &mut layout.fields {
            field.default = def
                .fields
                .iter()
                .find(|written| written.name == field.name)
                .and_then(|written| written.default.clone());
        }
    }

    pub(crate) fn get(&self, id: u32) -> Option<&Layout> {
        self.all.get(usize::try_from(id).ok()?)
    }

    pub(crate) fn id(&self, name: &str) -> Option<u32> {
        self.by_name.get(name).copied()
    }

    /// How native code holds a value of a type the checker worked out.
    pub(crate) fn ty(&self, ty: &Type) -> Ty {
        match ty {
            Type::Int => Ty::Int,
            Type::Float => Ty::Float,
            Type::Bool => Ty::Bool,
            Type::Unit => Ty::Unit,
            Type::Named(name) => match Cap::named(name) {
                Some(cap) => Ty::Cap(cap),
                None => self.id(name).map_or(Ty::Obj, Ty::Struct),
            },
            _ => Ty::Obj,
        }
    }
}
