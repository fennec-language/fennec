//! Plain structs: structs whose fields are all ints, floats, bools or other plain structs,
//! with no `where` rules. Native code keeps them in memory it owns, laid out field after
//! field, 8 bytes each, a nested struct in line. Every other struct is an object the runtime
//! holds.
//!
//! Plain enums, whose cases hold only such values, are kept the same way: a word for the
//! case's number, then the case's fields, padded to the biggest case. Every word of an enum
//! is a machine int: floats keep their bits and bools are 0 or 1, and words a case doesn't
//! use are 0. So two enum values are equal exactly when their words are.

use std::collections::{BTreeMap, HashMap};

use fennec_check::{Signatures, Type};
use fennec_syntax::{Expr, TypeDef, TypeExpr};

use crate::types::{Cap, ListItem, Ty, Word};

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

/// One case of a plain enum: its name, and its fields, each at its offset from the start
/// of the enum's value.
pub(crate) struct Case {
    pub(crate) name: String,
    pub(crate) fields: Vec<Field>,
}

pub(crate) struct Layout {
    pub(crate) name: String,
    /// A struct's fields. An enum's are its words, the case's number first, all ints.
    pub(crate) fields: Vec<Field>,
    pub(crate) size: u32,
    /// An enum's cases, in the order they're written; `None` for a struct.
    pub(crate) cases: Option<Vec<Case>>,
    /// Whether values of this type are boxed: held by the address of a block the runtime
    /// owns, because the type contains itself or holds a type that does.
    pub(crate) boxed: bool,
}

#[derive(Default)]
pub(crate) struct Layouts {
    by_name: HashMap<String, u32>,
    all: Vec<Layout>,
    /// Every enum with a case of each name, plain or not, for `.Circle` without its type.
    owners: HashMap<String, Vec<String>>,
}

/// What laying out types needs to see: the checker's structs, and the enums as written.
struct Sources<'s> {
    signatures: &'s Signatures,
    enums: HashMap<&'s str, &'s TypeDef>,
}

impl Layouts {
    /// The layouts of every plain struct the checker knows and every plain enum, the
    /// standard library's included.
    pub(crate) fn new<'s>(
        signatures: &'s Signatures,
        names: impl Iterator<Item = String>,
        enums: impl Iterator<Item = &'s TypeDef>,
    ) -> Layouts {
        let sources = Sources { signatures, enums: enums.map(|def| (def.name.as_str(), def)).collect() };
        let mut layouts = Layouts::default();
        for def in sources.enums.values() {
            for case in &def.cases {
                layouts.owners.entry(case.name.clone()).or_default().push(def.name.clone());
            }
        }
        let mut enum_names: Vec<&str> = sources.enums.keys().copied().collect();
        enum_names.sort_unstable();
        let names: Vec<String> = names.chain(enum_names.into_iter().map(str::to_owned)).collect();
        for name in &names {
            layouts.add(&sources, name, &mut Vec::new());
        }
        layouts.add_boxed(&sources, &names);
        layouts
    }

    /// Lays out the boxed types: every struct and enum that isn't plain and has no `where`
    /// rules. A field native code can't hold is an interpreter value in a cell of its own.
    fn add_boxed(&mut self, sources: &Sources<'_>, names: &[String]) {
        let candidates: BTreeMap<String, Written> = names
            .iter()
            .filter(|name| !self.by_name.contains_key(*name))
            .filter_map(|name| Some((name.clone(), written(sources, name)?)))
            .collect();
        let first = self.all.len();
        let reserved: HashMap<String, u32> = candidates
            .keys()
            .enumerate()
            .filter_map(|(at, name)| Some((name.clone(), u32::try_from(first + at).ok()?)))
            .collect();
        let ty_of = |layouts: &Layouts, ty: &Type| boxed_field_ty(layouts, &reserved, ty);
        let mut built = Vec::with_capacity(candidates.len());
        for (name, written) in &candidates {
            let place = |layouts: &Layouts, fields: &[(String, Type, Option<Expr>)], start: u32| {
                let typed = fields
                    .iter()
                    .map(|(field, ty, default)| (field.clone(), ty_of(layouts, ty), default.clone()))
                    .collect();
                layouts.place_fields(typed, start)
            };
            let layout = match written {
                Written::Struct(fields) => place(self, fields, 0).map(|(fields, size)| Layout {
                    name: name.clone(),
                    fields,
                    size: size.max(WORD),
                    cases: None,
                    boxed: true,
                }),
                Written::Enum(cases) => {
                    let mut placed = Vec::with_capacity(cases.len());
                    let mut size = WORD;
                    for (case, fields) in cases {
                        let Some((fields, end)) = place(self, fields, WORD) else { break };
                        size = size.max(end);
                        placed.push(Case { name: case.clone(), fields });
                    }
                    (placed.len() == cases.len()).then(|| enum_of(name, placed, size, true))
                }
            };
            // Every candidate's fields fit, so each lays out; a gap would shift the ids.
            let Some(layout) = layout else { return };
            built.push(layout);
        }
        for layout in built {
            if let Some(id) = reserved.get(&layout.name) {
                self.by_name.insert(layout.name.clone(), *id);
            }
            self.all.push(layout);
        }
    }

    /// Lays out a struct or enum if it's plain, after the plain types inside it. `visiting`
    /// stops a type that contains itself.
    fn add(&mut self, sources: &Sources<'_>, name: &str, visiting: &mut Vec<String>) -> Option<u32> {
        if let Some(id) = self.by_name.get(name) {
            return Some(*id);
        }
        if visiting.iter().any(|seen| seen == name) {
            return None;
        }
        visiting.push(name.to_owned());
        let layout = match sources.signatures.structure(name) {
            Some(structure) if !structure.rules => {
                let mut fields = Vec::with_capacity(structure.fields.len());
                for (field, ty) in &structure.fields {
                    fields.push((field.clone(), self.field_ty(sources, ty, visiting)?, None));
                }
                let (fields, size) = self.place_fields(fields, 0)?;
                Layout { name: name.to_owned(), fields, size: size.max(WORD), cases: None, boxed: false }
            }
            Some(_) => return None,
            None => self.enum_layout(sources, name, visiting)?,
        };
        visiting.pop();
        let id = u32::try_from(self.all.len()).ok()?;
        self.all.push(layout);
        self.by_name.insert(name.to_owned(), id);
        Some(id)
    }

    /// Lays out a plain enum: the case's number, then the biggest case's worth of words.
    fn enum_layout(
        &mut self,
        sources: &Sources<'_>,
        name: &str,
        visiting: &mut Vec<String>,
    ) -> Option<Layout> {
        let def = sources.enums.get(name)?;
        if def.cases.is_empty() {
            return None;
        }
        let mut cases = Vec::with_capacity(def.cases.len());
        let mut size = WORD;
        for case in &def.cases {
            let mut fields = Vec::with_capacity(case.fields.len());
            for field in &case.fields {
                if field.rule.is_some() {
                    return None;
                }
                let ty = written_type(field.ty.as_ref()?);
                fields.push((
                    field.name.clone(),
                    self.field_ty(sources, &ty, visiting)?,
                    field.default.clone(),
                ));
            }
            let (fields, end) = self.place_fields(fields, WORD)?;
            size = size.max(end);
            cases.push(Case { name: case.name.clone(), fields });
        }
        Some(enum_of(name, cases, size, false))
    }

    /// How a plain field is held, laying out the type it names first.
    fn field_ty(&mut self, sources: &Sources<'_>, ty: &Type, visiting: &mut Vec<String>) -> Option<Ty> {
        Some(match ty {
            Type::Int => Ty::Int,
            Type::Float => Ty::Float,
            Type::Bool => Ty::Bool,
            Type::Named(inner) => Ty::Struct(self.add(sources, inner, visiting)?),
            _ => return None,
        })
    }

    /// Places fields one after another from `start`, giving back where they end.
    fn place_fields(&self, fields: Vec<(String, Ty, Option<Expr>)>, start: u32) -> Option<(Vec<Field>, u32)> {
        let mut placed = Vec::with_capacity(fields.len());
        let mut offset = start;
        for (name, ty, default) in fields {
            let size = match ty {
                Ty::Struct(id) => self.get(id)?.size,
                _ => WORD,
            };
            placed.push(Field { name, ty, offset, default });
            offset += size;
        }
        Some((placed, offset))
    }

    /// The enum a case without its type, like `.Circle`, belongs to, when exactly one enum
    /// has a case of that name.
    pub(crate) fn owner(&self, case: &str) -> Option<&str> {
        match self.owners.get(case)?.as_slice() {
            [owner] => Some(owner),
            _ => None,
        }
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

    /// How many layouts there are.
    pub(crate) fn len(&self) -> usize {
        self.all.len()
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
            Type::Str => Ty::Str,
            Type::Dict(key, value) => dict_ty(self, key, value),
            Type::Set(item) => set_ty(self, item),
            Type::List(item) => ListItem::of(self.ty(item)).map_or(Ty::Obj, Ty::List),
            Type::Named(name) => match (Cap::named(name), self.id(name)) {
                (Some(cap), _) => Ty::Cap(cap),
                (None, Some(id)) if self.get(id).is_some_and(|layout| layout.boxed) => Ty::Boxed(id),
                (None, Some(id)) => Ty::Struct(id),
                (None, None) => Ty::Obj,
            },
            _ => Ty::Obj,
        }
    }
}

/// Whether a type mentions a type parameter.
fn has_param(ty: &Type) -> bool {
    match ty {
        Type::Param(_) => true,
        Type::List(inner)
        | Type::Set(inner)
        | Type::Optional(inner)
        | Type::Id(inner)
        | Type::IdMap(inner) => has_param(inner),
        Type::Dict(a, b) | Type::Result(a, b) => has_param(a) || has_param(b),
        Type::Tuple(parts) | Type::Generic(_, parts) => parts.iter().any(has_param),
        Type::Arrow(parts, result) => parts.iter().any(has_param) || has_param(result),
        _ => false,
    }
}

/// How a boxed type's field is held, given the numbers the boxed types will have.
fn boxed_field_ty(layouts: &Layouts, reserved: &HashMap<String, u32>, ty: &Type) -> Ty {
    match ty {
        Type::Int => Ty::Int,
        Type::Float => Ty::Float,
        Type::Bool => Ty::Bool,
        Type::Str => Ty::Str,
        Type::Named(name) => match (reserved.get(name), layouts.id(name)) {
            (Some(id), _) => Ty::Boxed(*id),
            (None, Some(id)) => Ty::Struct(id),
            (None, None) => Ty::Obj,
        },
        Type::List(item) => ListItem::of(boxed_field_ty(layouts, reserved, item)).map_or(Ty::Obj, Ty::List),
        Type::Dict(key, value) => dict_ty(layouts, key, value),
        Type::Set(item) => set_ty(layouts, item),
        _ => Ty::Obj,
    }
}

/// A set natively, as a dict whose values are nothing, when dict keys can be its items.
fn set_ty(layouts: &Layouts, item: &Type) -> Ty {
    key_word(layouts, item).map_or(Ty::Obj, |key| Ty::Dict(key, Word::Nothing))
}

/// How a dict holds keys of a type: ints, strings, and plain structs and enums.
fn key_word(layouts: &Layouts, key: &Type) -> Option<Word> {
    match key {
        Type::Int => Some(Word::Int),
        Type::Str => Some(Word::Str),
        Type::Named(name) => {
            let id = layouts.id(name)?;
            layouts.get(id).filter(|layout| !layout.boxed)?;
            Some(Word::Plain(id))
        }
        _ => None,
    }
}

/// A dict natively, when dict keys can be its keys and its values are plain words.
fn dict_ty(layouts: &Layouts, key: &Type, value: &Type) -> Ty {
    let word = match value {
        Type::Int => Some(Word::Int),
        Type::Float => Some(Word::Float),
        Type::Bool => Some(Word::Bool),
        Type::Str => Some(Word::Str),
        _ => None,
    };
    match (key_word(layouts, key), word) {
        (Some(key), Some(value)) => Ty::Dict(key, value),
        _ => Ty::Obj,
    }
}

/// The type a field's written type names, when it's a plain one: a number, a bool, or a
/// struct or enum by name.
fn written_type(written: &TypeExpr) -> Type {
    if written.optional {
        return Type::Unknown;
    }
    match (written.name.as_str(), written.args.as_slice()) {
        ("list", [item]) => Type::List(Box::new(written_type(item))),
        ("set", [item]) => Type::Set(Box::new(written_type(item))),
        ("dict", [key, value]) => Type::Dict(Box::new(written_type(key)), Box::new(written_type(value))),
        (_, [_, ..]) | ("list" | "dict" | "set" | "tuple" | "Id" | "IdMap", _) => Type::Unknown,
        ("int", _) => Type::Int,
        ("float", _) => Type::Float,
        ("bool", _) => Type::Bool,
        ("str", _) => Type::Str,
        (name, _) => Type::Named(name.to_owned()),
    }
}

/// An enum's layout from its cases: its words are the case's number, then `size` bytes in
/// all.
fn enum_of(name: &str, cases: Vec<Case>, size: u32, boxed: bool) -> Layout {
    let words = size / WORD;
    let fields = (0..words)
        .map(|word| Field { name: format!("\0{word}"), ty: Ty::Int, offset: word * WORD, default: None })
        .collect();
    Layout { name: name.to_owned(), fields, size, cases: Some(cases), boxed }
}

/// A field as written: its name, type and default.
type WrittenField = (String, Type, Option<Expr>);

/// A type's fields as written: a struct's, or each case's.
enum Written {
    Struct(Vec<WrittenField>),
    Enum(Vec<(String, Vec<WrittenField>)>),
}

/// A struct's or enum's fields as written, when none has a `where` rule.
fn written(sources: &Sources<'_>, name: &str) -> Option<Written> {
    if let Some(structure) = sources.signatures.structure(name) {
        // Native code doesn't know what a generic type's parameters are.
        if structure.rules || structure.fields.iter().any(|(_, ty)| has_param(ty)) {
            return None;
        }
        let fields = structure.fields.iter().map(|(field, ty)| (field.clone(), ty.clone(), None));
        return Some(Written::Struct(fields.collect()));
    }
    let def = sources.enums.get(name).filter(|def| def.generics.is_empty())?;
    let mut cases = Vec::with_capacity(def.cases.len());
    for case in &def.cases {
        let mut fields = Vec::with_capacity(case.fields.len());
        for field in &case.fields {
            if field.rule.is_some() {
                return None;
            }
            let ty = field.ty.as_ref().map_or(Type::Unknown, written_type);
            fields.push((field.name.clone(), ty, field.default.clone()));
        }
        cases.push((case.name.clone(), fields));
    }
    (!cases.is_empty()).then_some(Written::Enum(cases))
}
