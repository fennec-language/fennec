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

use crate::{Block, Cap, ListItem, Ty, Word};

/// The size of every field that isn't a nested struct: an int, a float, or a bool.
pub const WORD: u32 = 8;

pub struct Field {
    pub name: String,
    pub ty: Ty,
    /// Where it starts, in bytes from the start of the struct.
    pub offset: u32,
    /// Its default, used when a value is made without it.
    pub default: Option<Expr>,
}

/// One case of a plain enum: its name, and its fields, each at its offset from the start
/// of the enum's value.
pub struct Case {
    pub name: String,
    pub fields: Vec<Field>,
}

pub struct Layout {
    pub name: String,
    /// A struct's fields. An enum's are its words, the case's number first, all ints.
    pub fields: Vec<Field>,
    pub size: u32,
    /// An enum's cases, in the order they're written; `None` for a struct.
    pub cases: Option<Vec<Case>>,
    /// Whether values of this type are boxed: held by the address of a block the runtime
    /// owns, because the type contains itself or holds a type that does.
    pub boxed: bool,
    /// Whether it's a `shared` type: every copy of a value is a name for one block, which changes
    /// in place however many names it has.
    pub cell: bool,
}

#[derive(Default)]
pub struct Layouts {
    by_name: HashMap<String, u32>,
    all: Vec<Layout>,
    /// Every enum with a case of each name, plain or not, for `.Circle` without its type.
    owners: HashMap<String, Vec<String>>,
    /// Whether an optional of something held by a block is that block's address, or 0, and a
    /// list can hold lists, dicts and those optionals.
    one_word_optionals: bool,
    /// Whether a `Buffer[T]` is laid out as a list's block, for a backend that does its
    /// methods. Otherwise it's an object the runtime holds.
    native_buffers: bool,
    /// What's held in place rather than by a block's address.
    in_place: InPlace,
    /// The types `Ty::Block` names, by number.
    blocks: std::cell::RefCell<Vec<Ty>>,
    /// The types laid out by `add_through_lists`, whose items a list's description names by
    /// their place here.
    through_lists: Vec<u32>,
}

/// What a backend holds in place, where others hold the address of a block.
#[derive(Clone, Copy, Default)]
struct InPlace {
    /// Whether a `str` is held as its 24-byte header (see the runtime's `text`), rather than
    /// as its block's address.
    text_headers: bool,
    /// Whether a struct's string and list fields live inside it, so a struct holding them is
    /// still plain: kept in place, its fields owned by whatever holds it.
    counted_fields: bool,
}

/// What laying out types needs to see: the checker's structs, and the enums as written.
struct Sources<'s> {
    signatures: Option<&'s Signatures>,
    enums: HashMap<&'s str, &'s TypeDef>,
    /// Types already worked out, like the copies of generic ones the IR makes.
    definitions: HashMap<&'s str, &'s Written>,
}

impl Layouts {
    /// The layouts of every plain struct the checker knows and every plain enum, the
    /// standard library's included.
    pub fn new<'s>(
        signatures: &'s Signatures,
        names: impl Iterator<Item = String>,
        enums: impl Iterator<Item = &'s TypeDef>,
    ) -> Layouts {
        let sources = Sources {
            signatures: Some(signatures),
            definitions: HashMap::new(),
            enums: enums.map(|def| (def.name.as_str(), def)).collect(),
        };
        let mut layouts = Layouts::default();
        // In the enums' names' order, so each case's owners come out the same every run.
        let mut enums: Vec<&TypeDef> = sources.enums.values().copied().collect();
        enums.sort_unstable_by(|first, second| first.name.cmp(&second.name));
        for def in enums {
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

    /// The layouts of types already worked out, each given by name, like the copies of generic
    /// types the IR makes. What contains itself is boxed, and everything else is plain.
    #[must_use]
    pub fn from_definitions(definitions: &[(String, Written)]) -> Layouts {
        let sources = Sources {
            signatures: None,
            enums: HashMap::new(),
            definitions: definitions.iter().map(|(name, def)| (name.as_str(), def)).collect(),
        };
        let mut layouts = Layouts {
            one_word_optionals: true,
            native_buffers: true,
            in_place: InPlace { text_headers: true, counted_fields: true },
            ..Layouts::default()
        };
        for (name, def) in definitions {
            if let Written::Enum(cases) = def {
                for (case, _) in cases {
                    layouts.owners.entry(case.clone()).or_default().push(name.clone());
                }
            }
        }
        let names: Vec<String> = definitions.iter().map(|(name, _)| name.clone()).collect();
        for name in &names {
            layouts.add(&sources, name, &mut Vec::new());
        }
        layouts.add_through_lists(&sources, &names);
        layouts.add_boxed(&sources, &names);
        layouts
    }

    /// Lays out a type given by name if it's plain.
    fn add_definition(
        &mut self,
        sources: &Sources<'_>,
        name: &str,
        def: &Written,
        visiting: &mut Vec<String>,
    ) -> Option<Layout> {
        let mut place = |fields: &[WrittenField], start: u32, holds: bool| {
            let mut typed = Vec::with_capacity(fields.len());
            for (field, ty, default) in fields {
                typed.push((field.clone(), self.field_ty(sources, ty, visiting, holds)?, default.clone()));
            }
            self.place_fields(typed, start)
        };
        match def {
            Written::Generator(_) | Written::Cell(_) => None,
            Written::Struct(fields) => {
                // A `Table` is a block of its own, shared until it changes, like a list.
                let (fields, size) = place(fields, 0, !name.starts_with("Table["))?;
                Some(Layout {
                    name: name.to_owned(),
                    fields,
                    size: size.max(WORD),
                    cases: None,
                    boxed: false,
                    cell: false,
                })
            }
            Written::Enum(cases) => {
                let mut placed = Vec::with_capacity(cases.len());
                let mut size = WORD;
                for (case, fields) in cases {
                    let (fields, end) = place(fields, WORD, true)?;
                    size = size.max(end);
                    placed.push(Case { name: case.clone(), fields });
                }
                Some(enum_of(name, placed, size, false))
            }
        }
    }

    /// Lays out what's left that can be in place because its lists are one word each, whatever
    /// their items are: `Items(values: list[Json])`, with `Json` in place. A type directly inside
    /// itself has no size, however far round, so it's left to be boxed, as is anything holding
    /// one. Types go in the order that what's directly inside each comes before it. A regex's
    /// `Match` stays boxed, as native code builds that block itself.
    fn add_through_lists(&mut self, sources: &Sources<'_>, names: &[String]) {
        if !self.in_place.counted_fields {
            return;
        }
        let mut candidates: BTreeMap<String, Written> = names
            .iter()
            .filter(|name| {
                !self.by_name.contains_key(*name) && !name.starts_with("Table[") && *name != "Match"
            })
            .filter_map(|name| Some((name.clone(), written(sources, name)?)))
            // A `shared` type's whole point is to be one block, however it's held.
            .filter(|(_, def)| !matches!(def, Written::Generator(_) | Written::Cell(_)))
            .collect();
        let order = loop {
            let unfit: Vec<String> = candidates
                .iter()
                .filter(|(_, def)| !written_fields(def).all(|(_, ty, _)| self.fits_in_place(&candidates, ty)))
                .map(|(name, _)| name.clone())
                .collect();
            if !unfit.is_empty() {
                candidates.retain(|name, _| !unfit.contains(name));
                continue;
            }
            match inside_first(&candidates) {
                Ok(order) => break order,
                Err(stuck) => candidates.retain(|name, _| !stuck.contains(name)),
            }
        };
        let first = self.all.len();
        let reserved: HashMap<String, u32> = order
            .iter()
            .enumerate()
            .filter_map(|(at, name)| Some((name.clone(), u32::try_from(first + at).ok()?)))
            .collect();
        for name in &order {
            let (Some(def), Some(id)) = (candidates.get(name), reserved.get(name)) else { continue };
            // Everything that fits has a layout, and a gap would shift the numbers.
            let Some(layout) = self.layout_in_place(name, def, &reserved) else { return };
            self.all.push(layout);
            self.by_name.insert(name.clone(), *id);
            self.through_lists.push(*id);
        }
    }

    /// Whether a field can be held in place, given the types being laid out with it.
    fn fits_in_place(&self, candidates: &BTreeMap<String, Written>, ty: &Type) -> bool {
        let plain = |ty: &Type| match ty {
            Type::Int | Type::Float | Type::Bool | Type::Str => true,
            Type::Named(name) => {
                candidates.contains_key(name)
                    || self.id(name).and_then(|id| self.get(id)).is_some_and(|layout| !layout.boxed)
            }
            _ => false,
        };
        match ty {
            Type::List(item) => plain(item),
            other => plain(other),
        }
    }

    fn layout_in_place(&self, name: &str, def: &Written, reserved: &HashMap<String, u32>) -> Option<Layout> {
        let held = |ty: &Type| match ty {
            Type::Int => Some(Ty::Int),
            Type::Float => Some(Ty::Float),
            Type::Bool => Some(Ty::Bool),
            Type::Str => Some(Ty::Str),
            Type::Named(name) => reserved.get(name).copied().or_else(|| self.id(name)).map(Ty::Struct),
            _ => None,
        };
        let place = |fields: &[WrittenField], start: u32| {
            let mut typed = Vec::with_capacity(fields.len());
            for (field, ty, default) in fields {
                let ty = match ty {
                    Type::List(item) => Ty::List(self.list_item(held(item)?)?),
                    other => held(other)?,
                };
                typed.push((field.clone(), ty, default.clone()));
            }
            self.place_fields(typed, start)
        };
        match def {
            Written::Struct(fields) => {
                let (fields, size) = place(fields, 0)?;
                Some(Layout {
                    name: name.to_owned(),
                    fields,
                    size: size.max(WORD),
                    cases: None,
                    boxed: false,
                    cell: false,
                })
            }
            Written::Enum(cases) => {
                let mut placed = Vec::with_capacity(cases.len());
                let mut size = WORD;
                for (case, fields) in cases {
                    let (fields, end) = place(fields, WORD)?;
                    size = size.max(end);
                    placed.push(Case { name: case.clone(), fields });
                }
                Some(enum_of(name, placed, size, false))
            }
            Written::Generator(_) | Written::Cell(_) => None,
        }
    }

    /// Where a type laid out by `add_through_lists` is among them.
    #[must_use]
    pub fn through_lists(&self, id: u32) -> Option<usize> {
        self.through_lists.iter().position(|known| *known == id)
    }

    /// The types laid out by `add_through_lists`.
    #[must_use]
    pub fn all_through_lists(&self) -> &[u32] {
        &self.through_lists
    }

    /// Lays out the boxed types: every struct and enum that isn't plain and has no `where`
    /// rules. A field native code can't hold is a runtime `Value` in a cell of its own.
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
                Written::Struct(fields) | Written::Generator(fields) | Written::Cell(fields) => {
                    place(self, fields, 0).map(|(fields, size)| Layout {
                        name: name.clone(),
                        fields,
                        // A cell's lock is one more word, after its fields.
                        size: size.max(WORD) + if matches!(written, Written::Cell(_)) { WORD } else { 0 },
                        cases: None,
                        boxed: true,
                        cell: matches!(written, Written::Cell(_)),
                    })
                }
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
        let structure = sources.signatures.and_then(|signatures| signatures.structure(name));
        let layout = match structure {
            _ if sources.definitions.contains_key(name) => {
                let def = sources.definitions.get(name)?;
                self.add_definition(sources, name, def, visiting)?
            }
            Some(structure) if !structure.rules => {
                let mut fields = Vec::with_capacity(structure.fields.len());
                for (field, ty) in &structure.fields {
                    fields.push((field.clone(), self.field_ty(sources, ty, visiting, true)?, None));
                }
                let (fields, size) = self.place_fields(fields, 0)?;
                Layout {
                    name: name.to_owned(),
                    fields,
                    size: size.max(WORD),
                    cases: None,
                    boxed: false,
                    cell: false,
                }
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
                    self.field_ty(sources, &ty, visiting, false)?,
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
    fn field_ty(
        &mut self,
        sources: &Sources<'_>,
        ty: &Type,
        visiting: &mut Vec<String>,
        holds: bool,
    ) -> Option<Ty> {
        Some(match ty {
            Type::Sized(fennec_check::Num::U64) => Ty::U64,
            Type::Sized(fennec_check::Num::F32) => Ty::F32,
            Type::Int | Type::Sized(_) => Ty::Int,
            Type::Float => Ty::Float,
            Type::Bool => Ty::Bool,
            Type::Named(inner) => Ty::Struct(self.add(sources, inner, visiting)?),
            Type::Str if holds && self.in_place.counted_fields => Ty::Str,
            Type::List(item) if holds && self.in_place.counted_fields => {
                let item = match item.as_ref() {
                    Type::Sized(fennec_check::Num::U64) => Ty::U64,
                    Type::Sized(fennec_check::Num::F32) => Ty::F32,
                    Type::Int | Type::Sized(_) => Ty::Int,
                    Type::Float => Ty::Float,
                    Type::Bool => Ty::Bool,
                    Type::Str => Ty::Str,
                    _ => return None,
                };
                Ty::List(self.list_item(item)?)
            }
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
                Ty::Str | Ty::Maybe(Block::Str) if self.in_place.text_headers => 3 * WORD,
                _ => WORD,
            };
            placed.push(Field { name, ty, offset, default });
            offset += size;
        }
        Some((placed, offset))
    }

    /// The enum a case without its type, like `.Circle`, belongs to, when exactly one enum
    /// has a case of that name.
    pub fn owner(&self, case: &str) -> Option<&str> {
        match self.owners.get(case)?.as_slice() {
            [owner] => Some(owner),
            _ => None,
        }
    }

    /// Records the defaults a struct's definition gives its fields.
    pub fn add_defaults(&mut self, def: &TypeDef) {
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
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.all.is_empty()
    }

    pub fn len(&self) -> usize {
        self.all.len()
    }

    pub fn get(&self, id: u32) -> Option<&Layout> {
        self.all.get(usize::try_from(id).ok()?)
    }

    /// Whether a `str` is held in place as its 24-byte header.
    #[must_use]
    pub fn text_headers(&self) -> bool {
        self.in_place.text_headers
    }

    /// Whether a plain struct or enum holds a block anywhere, in a field or a case.
    pub fn holds_counted(&self, id: u32) -> bool {
        let Some(layout) = self.get(id) else { return false };
        let cases = layout.cases.iter().flatten().flat_map(|case| &case.fields);
        layout.fields.iter().chain(cases).any(|field| match field.ty {
            Ty::Struct(inner) => self.holds_counted(inner),
            ty => ty.counted(),
        })
    }

    /// The type a `Ty::Block` names.
    #[must_use]
    pub fn block(&self, id: u32) -> Option<Ty> {
        self.blocks.borrow().get(usize::try_from(id).ok()?).copied()
    }

    /// How a list holds an item of this type, if it can: a block a list owns is named by number
    /// when `ListItem` can't name it itself.
    fn list_item(&self, item: Ty) -> Option<ListItem> {
        if let Some(item) = ListItem::of(item) {
            return Some(item);
        }
        if !self.one_word_optionals || !item.counted() {
            return None;
        }
        self.block_number(item).map(ListItem::Block)
    }

    /// The number of a kind of block, which a list's items and a dict's values go by.
    fn block_number(&self, item: Ty) -> Option<u32> {
        let mut blocks = self.blocks.borrow_mut();
        let at = blocks.iter().position(|known| *known == item).unwrap_or_else(|| {
            blocks.push(item);
            blocks.len() - 1
        });
        u32::try_from(at).ok()
    }

    /// How a dict holds a value of a kind that isn't one word already: in a block.
    fn value_word(&self, value: Ty) -> Option<Word> {
        if !self.native_buffers {
            return None;
        }
        match value {
            Ty::Struct(id) => Some(Word::Plain(id)),
            // An optional string is three words, where strings are headers.
            Ty::Maybe(Block::Str) if self.text_headers() => None,
            value if value.counted() => self.block_number(value).map(Word::Block),
            _ => None,
        }
    }

    /// How many kinds of block have numbers.
    #[must_use]
    pub fn block_count(&self) -> u32 {
        u32::try_from(self.blocks.borrow().len()).unwrap_or(0)
    }

    pub fn id(&self, name: &str) -> Option<u32> {
        self.by_name.get(name).copied()
    }

    /// How native code holds a value of a type the checker worked out.
    pub fn ty(&self, ty: &Type) -> Ty {
        match ty {
            Type::Sized(fennec_check::Num::U64) => Ty::U64,
            Type::Sized(fennec_check::Num::F32) => Ty::F32,
            Type::Int | Type::Sized(_) => Ty::Int,
            Type::Float => Ty::Float,
            Type::Bool => Ty::Bool,
            Type::Unit => Ty::Unit,
            Type::Str => Ty::Str,
            Type::Dict(key, value) => dict_ty(self, key, value, &|value| self.ty(value)),
            Type::Set(item) => set_ty(self, item),
            Type::List(item) => self.list_item(self.ty(item)).map_or(Ty::Obj, Ty::List),
            // A buffer is a list's block.
            Type::Buffer(item) if self.native_buffers => {
                self.list_item(self.ty(item)).map_or(Ty::Obj, Ty::List)
            }
            Type::Optional(inner) if self.one_word_optionals => {
                Block::of(self.ty(inner)).map_or(Ty::Obj, Ty::Maybe)
            }
            Type::Bytes if self.native_buffers => Ty::Bytes,
            // A generator or a closure: a block whose own shape says what it keeps.
            Type::Iterator(_) | Type::Arrow(..) if self.native_buffers => Ty::Gen,
            // `None` itself, as a tuple can hold: an optional that's never anything else.
            Type::None if self.native_buffers => Ty::Maybe(Block::Bytes),
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

/// Every field of a type, in every case of an enum.
fn written_fields(def: &Written) -> Box<dyn Iterator<Item = &WrittenField> + '_> {
    match def {
        Written::Struct(fields) | Written::Generator(fields) | Written::Cell(fields) => {
            Box::new(fields.iter())
        }
        Written::Enum(cases) => Box::new(cases.iter().flat_map(|(_, fields)| fields)),
    }
}

/// These types with what's directly inside each before it, or those that can't be: a type
/// directly inside itself, and what holds one.
fn inside_first(candidates: &BTreeMap<String, Written>) -> Result<Vec<String>, Vec<String>> {
    let mut order: Vec<String> = Vec::with_capacity(candidates.len());
    while order.len() < candidates.len() {
        let next: Vec<String> = candidates
            .iter()
            .filter(|(name, def)| {
                !order.contains(name)
                    && written_fields(def).all(|(_, ty, _)| match ty {
                        Type::Named(inner) => !candidates.contains_key(inner) || order.contains(inner),
                        _ => true,
                    })
            })
            .map(|(name, _)| name.clone())
            .collect();
        if next.is_empty() {
            return Err(candidates.keys().filter(|name| !order.contains(name)).cloned().collect());
        }
        order.extend(next);
    }
    Ok(order)
}

/// Whether a type mentions a type parameter.
fn has_param(ty: &Type) -> bool {
    match ty {
        Type::Param(_) => true,
        Type::List(inner)
        | Type::Buffer(inner)
        | Type::Set(inner)
        | Type::Optional(inner)
        | Type::Id(inner)
        | Type::Table(inner) => has_param(inner),
        Type::Dict(a, b) | Type::Result(a, b) => has_param(a) || has_param(b),
        Type::Tuple(parts) | Type::Generic(_, parts) => parts.iter().any(has_param),
        Type::Arrow(parts, result) => parts.iter().any(has_param) || has_param(result),
        _ => false,
    }
}

/// How a boxed type's field is held, given the numbers the boxed types will have.
fn boxed_field_ty(layouts: &Layouts, reserved: &HashMap<String, u32>, ty: &Type) -> Ty {
    match ty {
        Type::Sized(fennec_check::Num::U64) => Ty::U64,
        Type::Sized(fennec_check::Num::F32) => Ty::F32,
        Type::Int | Type::Sized(_) => Ty::Int,
        Type::Float => Ty::Float,
        Type::Bool => Ty::Bool,
        Type::Str => Ty::Str,
        Type::Named(name) if layouts.native_buffers && Cap::named(name).is_some() => {
            Cap::named(name).map_or(Ty::Obj, Ty::Cap)
        }
        Type::Bytes if layouts.native_buffers => Ty::Bytes,
        Type::Iterator(_) | Type::Arrow(..) if layouts.native_buffers => Ty::Gen,
        Type::None if layouts.native_buffers => Ty::Maybe(Block::Bytes),
        Type::Named(name) => match (reserved.get(name), layouts.id(name)) {
            (Some(id), _) => Ty::Boxed(*id),
            (None, Some(id)) => Ty::Struct(id),
            (None, None) => Ty::Obj,
        },
        Type::List(item) => {
            layouts.list_item(boxed_field_ty(layouts, reserved, item)).map_or(Ty::Obj, Ty::List)
        }
        Type::Buffer(item) if layouts.native_buffers => {
            layouts.list_item(boxed_field_ty(layouts, reserved, item)).map_or(Ty::Obj, Ty::List)
        }
        Type::Dict(key, value) => {
            dict_ty(layouts, key, value, &|value| boxed_field_ty(layouts, reserved, value))
        }
        Type::Set(item) => set_ty(layouts, item),
        Type::Optional(inner) if layouts.one_word_optionals => {
            Block::of(boxed_field_ty(layouts, reserved, inner)).map_or(Ty::Obj, Ty::Maybe)
        }
        _ => Ty::Obj,
    }
}

/// A set natively, as a dict whose values are nothing, when dict keys can be its items.
fn set_ty(layouts: &Layouts, item: &Type) -> Ty {
    key_word(layouts, item).map_or(Ty::Obj, |key| Ty::Dict(key, Word::Nothing))
}

/// The word a dict holds a sized number as: an int's, but for a `u64`, shown unsigned, and an `f32`,
/// shown as one.
fn sized_word(num: fennec_check::Num) -> Word {
    match num {
        fennec_check::Num::U64 => Word::U64,
        fennec_check::Num::F32 => Word::F32,
        _ => Word::Int,
    }
}

/// How a dict holds keys of a type: ints, strings, and plain structs and enums.
fn key_word(layouts: &Layouts, key: &Type) -> Option<Word> {
    match key {
        Type::Int => Some(Word::Int),
        // A sized number is a key as an int is, but a `u64` and an `f32`, which have words of their
        // own, as they're shown differently.
        Type::Sized(num) => Some(sized_word(*num)),
        Type::Float if layouts.native_buffers => Some(Word::Float),
        // A string key is held in the entry as its header, when strings are headers.
        Type::Str if layouts.text_headers() => Some(Word::Text),
        Type::Str => Some(Word::Str),
        Type::Named(name) => {
            let id = layouts.id(name)?;
            layouts.get(id).filter(|layout| !layout.boxed)?;
            // Keys are compared and hashed by their words, which only numbers can be, unless the
            // runtime does it by what they hold.
            (layouts.native_buffers || !layouts.holds_counted(id)).then_some(Word::Plain(id))
        }
        _ => None,
    }
}

/// A dict natively, when dict keys can be its keys and its values are plain words.
fn dict_ty(layouts: &Layouts, key: &Type, value: &Type, held: &dyn Fn(&Type) -> Ty) -> Ty {
    let word = match value {
        Type::Int => Some(Word::Int),
        Type::Sized(num) => Some(sized_word(*num)),
        Type::Float => Some(Word::Float),
        Type::Bool => Some(Word::Bool),
        Type::Str => Some(Word::Str),
        other => layouts.value_word(held(other)),
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
        ("Buffer", [item]) => Type::Buffer(Box::new(written_type(item))),
        ("set", [item]) => Type::Set(Box::new(written_type(item))),
        ("dict", [key, value]) => Type::Dict(Box::new(written_type(key)), Box::new(written_type(value))),
        (_, [_, ..]) | ("list" | "dict" | "set" | "tuple" | "Id" | "Table", _) => Type::Unknown,
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
    Layout { name: name.to_owned(), fields, size, cases: Some(cases), boxed, cell: false }
}

/// A field as written: its name, type and default.
pub type WrittenField = (String, Type, Option<Expr>);

/// A type's fields as written: a struct's, or each case's.
#[derive(Clone)]
pub enum Written {
    /// A `shared` type's fields: a block however it could be held, one for all its copies.
    Cell(Vec<WrittenField>),
    Struct(Vec<WrittenField>),
    /// A generator's block: always boxed, with the fields every generator has first.
    Generator(Vec<WrittenField>),
    Enum(Vec<(String, Vec<WrittenField>)>),
}

/// A struct's or enum's fields as written, when none has a `where` rule.
fn written(sources: &Sources<'_>, name: &str) -> Option<Written> {
    if let Some(def) = sources.definitions.get(name) {
        return Some((*def).clone());
    }
    if let Some(structure) = sources.signatures.and_then(|signatures| signatures.structure(name)) {
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

/// A generator's block, by what it gives, what it can fail with, and the locals it keeps. A
/// field's offset counts from the first word after the block's count and shape.
#[must_use]
pub fn generator_definition(item: &Type, failure: Option<&Type>, slots: &[Type]) -> Written {
    let field = |name: &str, ty: &Type| (name.to_owned(), ty.clone(), None);
    let mut fields = vec![
        field("\0resume", &Type::Int),
        field("\0pc", &Type::Int),
        field("\0started", &Type::Bool),
        field("\0failure", failure.unwrap_or(&Type::Int)),
        field("\0item", item),
    ];
    for (at, slot) in slots.iter().enumerate() {
        fields.push(field(&format!("\0slot{at}"), slot));
    }
    Written::Generator(fields)
}

/// A closure's block, when the runtime holds closures: the code it calls and the code native code
/// lets go of it with, then the values it captured, in order. Each field's offset counts from the
/// first word after the block's count and shape, so the code is at 16 bytes and the first capture
/// at 32, as native code reads them.
#[must_use]
pub fn closure_definition(captures: &[Type]) -> Written {
    let field = |name: String, ty: &Type| (name, ty.clone(), None);
    let mut fields = vec![field("\0code".to_owned(), &Type::Int), field("\0let_go".to_owned(), &Type::Int)];
    for (at, capture) in captures.iter().enumerate() {
        fields.push(field(format!("\0capture{at}"), capture));
    }
    Written::Generator(fields)
}

#[cfg(test)]
mod tests {
    use fennec_check::Type;
    use fennec_rt::embed::layout::{FIELDS, GEN_FAILURE, GEN_ITEM, GEN_PC, GEN_RESUME, GEN_STARTED};

    use super::{Layouts, Written, generator_definition};
    use crate::{Block, ListItem, Ty};

    fn fields(names: &[(&str, Type)]) -> Vec<super::WrittenField> {
        names.iter().map(|(name, ty)| ((*name).to_owned(), ty.clone(), None)).collect()
    }

    #[test]
    fn a_copy_of_a_generic_type_gets_its_own_layout() {
        let pair = Type::Named("Pair[int]".to_owned());
        let definitions = vec![
            ("Pair[int]".to_owned(), Written::Struct(fields(&[("a", Type::Int), ("b", Type::Int)]))),
            ("Outer".to_owned(), Written::Struct(fields(&[("pair", pair.clone()), ("c", Type::Float)]))),
            (
                "Shape".to_owned(),
                Written::Enum(vec![
                    ("Dot".to_owned(), Vec::new()),
                    ("Line".to_owned(), fields(&[("from", pair.clone()), ("to", pair.clone())])),
                ]),
            ),
        ];
        let layouts = Layouts::from_definitions(&definitions);
        let id = layouts.id("Pair[int]").unwrap();
        assert_eq!(layouts.ty(&pair), Ty::Struct(id));
        assert_eq!(layouts.get(id).unwrap().size, 16);
        let outer = layouts.get(layouts.id("Outer").unwrap()).unwrap();
        assert_eq!((outer.size, outer.fields[1].offset), (24, 16));
        let shape = layouts.get(layouts.id("Shape").unwrap()).unwrap();
        assert_eq!(shape.size, 40);
        assert_eq!(layouts.owner("Line"), Some("Shape"));
    }

    #[test]
    fn a_struct_holding_strings_and_lists_is_kept_in_place() {
        let list = |ty: Type| Type::List(Box::new(ty));
        let definitions = vec![(
            "Named".to_owned(),
            Written::Struct(fields(&[
                ("name", Type::Str),
                ("scores", list(Type::Int)),
                ("tags", list(Type::Str)),
                ("n", Type::Int),
            ])),
        )];
        let layouts = Layouts::from_definitions(&definitions);
        let id = layouts.id("Named").unwrap();
        assert_eq!(layouts.ty(&Type::Named("Named".to_owned())), Ty::Struct(id));
        let layout = layouts.get(id).unwrap();
        let types: Vec<Ty> = layout.fields.iter().map(|field| field.ty).collect();
        assert_eq!(types, [Ty::Str, Ty::List(ListItem::Int), Ty::List(ListItem::Str), Ty::Int]);
        // The string is its three-word header.
        assert_eq!(layout.size, 48);
    }

    #[test]
    fn a_type_containing_itself_is_boxed() {
        let tree = Type::Named("Tree".to_owned());
        let definitions = vec![(
            "Tree".to_owned(),
            Written::Enum(vec![
                ("Leaf".to_owned(), fields(&[("value", Type::Int)])),
                ("Node".to_owned(), fields(&[("left", tree.clone()), ("right", tree.clone())])),
            ]),
        )];
        let layouts = Layouts::from_definitions(&definitions);
        let id = layouts.id("Tree").unwrap();
        assert_eq!(layouts.ty(&tree), Ty::Boxed(id));
    }

    #[test]
    fn a_generator_is_boxed_with_the_header_every_generator_has() {
        let definitions =
            vec![("g".to_owned(), generator_definition(&Type::Str, None, &[Type::Int, Type::Str]))];
        let layouts = Layouts::from_definitions(&definitions);
        let id = layouts.id("g").unwrap();
        assert_eq!(layouts.ty(&Type::Named("g".to_owned())), Ty::Boxed(id));
        let at = |name: &str| {
            let field = layouts.get(id).unwrap().fields.iter().find(|field| field.name == name).unwrap();
            i32::try_from(field.offset).unwrap() + FIELDS
        };
        assert_eq!(
            [at("\0resume"), at("\0pc"), at("\0started"), at("\0failure"), at("\0item")],
            [GEN_RESUME, GEN_PC, GEN_STARTED, GEN_FAILURE, GEN_ITEM]
        );
    }

    #[test]
    fn an_optional_of_a_block_is_one_word() {
        let maybe = |ty: Type| Type::Optional(Box::new(ty));
        let node = Type::Named("Node".to_owned());
        let definitions = vec![(
            "Node".to_owned(),
            Written::Struct(fields(&[
                ("name", maybe(Type::Str)),
                ("next", maybe(node.clone())),
                ("n", maybe(Type::Int)),
            ])),
        )];
        let layouts = Layouts::from_definitions(&definitions);
        let id = layouts.id("Node").unwrap();
        let layout = layouts.get(id).unwrap();
        let types: Vec<Ty> = layout.fields.iter().map(|field| field.ty).collect();
        assert_eq!(types, [Ty::Maybe(Block::Str), Ty::Maybe(Block::Boxed(id)), Ty::Obj]);
        assert_eq!(layouts.ty(&maybe(Type::Str)), Ty::Maybe(Block::Str));
        assert_eq!(layouts.ty(&maybe(Type::Int)), Ty::Obj);
    }

    #[test]
    fn a_type_holding_itself_through_a_list_is_in_place_and_one_directly_inside_itself_is_boxed() {
        let held = |item: &str| Type::List(Box::new(Type::Named(item.to_owned())));
        let field = |name: &str, ty: Type| (name.to_owned(), ty, None);
        let definitions = vec![
            (
                "Json".to_owned(),
                Written::Enum(vec![("Items".to_owned(), vec![field("values", held("Json"))])]),
            ),
            ("Tree".to_owned(), Written::Struct(vec![field("left", Type::Named("Tree".to_owned()))])),
            ("Outer".to_owned(), Written::Struct(vec![field("tree", Type::Named("Tree".to_owned()))])),
            ("Folder".to_owned(), Written::Struct(vec![field("file", Type::Named("File".to_owned()))])),
            ("File".to_owned(), Written::Struct(vec![field("inside", held("Folder"))])),
        ];
        let layouts = Layouts::from_definitions(&definitions);
        let boxed = |name: &str| layouts.id(name).and_then(|id| layouts.get(id)).map(|layout| layout.boxed);
        for (name, wanted) in
            [("Json", false), ("Tree", true), ("Outer", true), ("Folder", false), ("File", false)]
        {
            assert_eq!(boxed(name), Some(wanted), "{name}");
        }
        // What's directly inside comes first, so its size is known.
        assert!(layouts.id("File") < layouts.id("Folder"));
        let json = layouts.id("Json").and_then(|id| layouts.through_lists(id));
        assert!(json.is_some());
    }

    #[test]
    fn a_list_holds_lists_dicts_and_optionals_by_number() {
        let list = |ty: Type| Type::List(Box::new(ty));
        let layouts = Layouts::from_definitions(&[]);
        let nested = layouts.ty(&list(list(Type::Int)));
        let Ty::List(ListItem::Block(id)) = nested else { panic!("{nested:?}") };
        assert_eq!(layouts.block(id), Some(Ty::List(ListItem::Int)));
        assert_eq!(layouts.ty(&list(list(Type::Int))), nested);
        let maybe = layouts.ty(&list(Type::Optional(Box::new(Type::Str))));
        let Ty::List(ListItem::Block(other)) = maybe else { panic!("{maybe:?}") };
        assert_eq!(layouts.block(other), Some(Ty::Maybe(Block::Str)));
        assert_eq!(
            crate::describe(&layouts, nested),
            Some(fennec_rt::packed::Element::List(std::rc::Rc::new(fennec_rt::packed::Element::List(
                std::rc::Rc::new(fennec_rt::packed::Element::Int)
            ))))
        );
        assert_eq!(Layouts::default().ty(&list(list(Type::Int))), Ty::Obj);
    }
}
