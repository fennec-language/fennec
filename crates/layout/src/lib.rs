//! How values are laid out in memory, which every backend and the runtime have to agree on:
//! which types are plain, which are boxed, where fields go, and how the runtime reads them.
//! It knows nothing about generating code.

use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

pub use fennec_rt::embed::dicts::Word;
use fennec_rt::embed::{Tables, dicts};
use fennec_rt::packed::Element;

mod layouts;

pub use layouts::{
    Case, Field, Layout, Layouts, WORD, Written, WrittenField, closure_definition, generator_definition,
};

/// How native code holds a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ty {
    Int,
    Float,
    Bool,
    /// A `u64`: its 64 bits in a word, shown unsigned.
    U64,
    /// An `f32`: in a word as the `f64` that's exactly its value, shown as the shortest text that
    /// reads back as that `f32`.
    F32,
    /// Nothing at all: what a function without a value returns.
    Unit,
    /// A capability, like `stdout`, which needs nothing at run time.
    Cap(Cap),
    /// Anything else, held by the runtime.
    Obj,
    /// A plain struct, in memory native code owns, which it refers to by address. The
    /// number picks its layout.
    Struct(u32),
    /// A boxed struct or enum: the address of a block the runtime owns, shared by every
    /// copy. The number picks its layout.
    Boxed(u32),
    /// A string: the address of a block the runtime owns, shared by every copy.
    Str,
    /// `bytes`: the address of a block the runtime owns, shared by every copy. Only layouts made
    /// by `Layouts::from_definitions` use it.
    Bytes,
    /// A generator or a closure: the address of its block, whose own shape says what it keeps.
    /// Only layouts made by `Layouts::from_definitions` use it.
    Gen,
    /// A dict with these keys and values: the address of a block the runtime owns, shared
    /// by every copy until one changes it.
    Dict(Word, Word),
    /// A list of numbers, bools or plain structs, packed one after another: the address of
    /// a block the runtime owns, shared by every copy until one changes it.
    List(ListItem),
    /// A function known while compiling, by its number there, which needs nothing at run
    /// time: calls to it are lowered right where they are.
    Known(u32),
    /// An optional of something held by a block's address: the address, or 0 for `None`,
    /// with no block for `Some`. Only layouts made by `Layouts::from_definitions` use it.
    Maybe(Block),
    /// Whatever the block type with this number in the `Layouts` is: how a list holds lists,
    /// dicts and optionals, which `ListItem` can't name itself. Only `Layouts::from_definitions`
    /// gives it.
    Block(u32),
}

/// Something held by the address of a block the runtime owns, which an optional of it holds
/// in one word.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Block {
    Str,
    Boxed(u32),
    List(ListItem),
    Dict(Word, Word),
    Cap(Cap),
    Bytes,
}

impl Block {
    /// What's held this way, if it is.
    #[must_use]
    pub fn of(ty: Ty) -> Option<Block> {
        Some(match ty {
            Ty::Str => Block::Str,
            Ty::Boxed(id) => Block::Boxed(id),
            Ty::List(item) => Block::List(item),
            Ty::Dict(key, value) => Block::Dict(key, value),
            Ty::Cap(cap) => Block::Cap(cap),
            Ty::Bytes => Block::Bytes,
            _ => return None,
        })
    }

    /// How native code holds one that isn't `None`.
    #[must_use]
    pub fn ty(self) -> Ty {
        match self {
            Block::Str => Ty::Str,
            Block::Boxed(id) => Ty::Boxed(id),
            Block::List(item) => Ty::List(item),
            Block::Dict(key, value) => Ty::Dict(key, value),
            Block::Cap(cap) => Ty::Cap(cap),
            Block::Bytes => Ty::Bytes,
        }
    }
}

/// What a native list's items are.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ListItem {
    Int,
    Float,
    Bool,
    U64,
    F32,
    /// A plain struct or enum, by its layout's number.
    Struct(u32),
    /// The address of a string's block, which the list owns.
    Str,
    /// The address of a boxed value's block, which the list owns, by its layout's number.
    Boxed(u32),
    /// A block of the type with this number in the `Layouts`, which the list owns: a list, a
    /// dict, or an optional of one held in one word.
    Block(u32),
}

impl ListItem {
    /// The kind of item a list of `ty` holds natively, if it can.
    pub fn of(ty: Ty) -> Option<ListItem> {
        Some(match ty {
            Ty::Int => ListItem::Int,
            Ty::Float => ListItem::Float,
            Ty::Bool => ListItem::Bool,
            Ty::U64 => ListItem::U64,
            Ty::F32 => ListItem::F32,
            Ty::Struct(id) => ListItem::Struct(id),
            Ty::Str => ListItem::Str,
            Ty::Boxed(id) => ListItem::Boxed(id),
            Ty::Block(id) => ListItem::Block(id),
            _ => return None,
        })
    }

    /// How native code holds one item.
    pub fn ty(self) -> Ty {
        match self {
            ListItem::Int => Ty::Int,
            ListItem::Float => Ty::Float,
            ListItem::Bool => Ty::Bool,
            ListItem::U64 => Ty::U64,
            ListItem::F32 => Ty::F32,
            ListItem::Struct(id) => Ty::Struct(id),
            ListItem::Str => Ty::Str,
            ListItem::Boxed(id) => Ty::Boxed(id),
            ListItem::Block(id) => Ty::Block(id),
        }
    }
}

impl Ty {
    /// Whether values of this kind are addresses of blocks with an owner count.
    pub fn counted(self) -> bool {
        matches!(
            self,
            Ty::Boxed(_)
                | Ty::Str
                | Ty::Bytes
                | Ty::Gen
                | Ty::Dict(..)
                | Ty::List(_)
                | Ty::Maybe(_)
                | Ty::Block(_)
        )
    }

    pub fn name(self) -> &'static str {
        match self {
            Ty::Int => "int",
            Ty::Float => "float",
            Ty::Bool => "bool",
            Ty::U64 => "u64",
            Ty::F32 => "f32",
            Ty::Unit => "nothing",
            Ty::Cap(cap) => cap.name(),
            Ty::Obj => "value",
            Ty::Struct(_) => "struct",
            Ty::Boxed(_) => "boxed value",
            Ty::Str => "str",
            Ty::Bytes => "bytes",
            Ty::Gen => "iterator",
            Ty::Dict(..) => "dict",
            Ty::List(_) => "list",
            Ty::Known(_) => "function",
            Ty::Maybe(_) => "optional",
            Ty::Block(_) => "block",
        }
    }
}

/// The capabilities `main` can be given.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cap {
    Stdout,
    Fs,
    Clock,
    Args,
    Stdin,
    Process,
    Entropy,
    Net,
    Env,
    Signals,
    Stderr,
}

impl Cap {
    pub fn named(name: &str) -> Option<Cap> {
        match name {
            "Stdout" => Some(Cap::Stdout),
            "Fs" => Some(Cap::Fs),
            "Clock" => Some(Cap::Clock),
            "Args" => Some(Cap::Args),
            "Stdin" => Some(Cap::Stdin),
            "Process" => Some(Cap::Process),
            "Entropy" => Some(Cap::Entropy),
            "Net" => Some(Cap::Net),
            "Env" => Some(Cap::Env),
            "Signals" => Some(Cap::Signals),
            "Stderr" => Some(Cap::Stderr),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Cap::Stdout => "Stdout",
            Cap::Fs => "Fs",
            Cap::Clock => "Clock",
            Cap::Args => "Args",
            Cap::Stdin => "Stdin",
            Cap::Process => "Process",
            Cap::Entropy => "Entropy",
            Cap::Net => "Net",
            Cap::Env => "Env",
            Cap::Signals => "Signals",
            Cap::Stderr => "Stderr",
        }
    }

    /// How the runtime tells capabilities apart.
    pub fn kind(self) -> i64 {
        match self {
            Cap::Stdout => 0,
            Cap::Fs => 1,
            Cap::Clock => 2,
            Cap::Args => 3,
            Cap::Stdin => 4,
            Cap::Process => 5,
            Cap::Entropy => 6,
            Cap::Net => 7,
            Cap::Env => 8,
            Cap::Signals => 9,
            Cap::Stderr => 10,
        }
    }
}

/// What the runtime needs to know about each layout's blocks, by the layout's number. A plain
/// layout has blocks too, for a dict's keys.
pub fn shapes(layouts: &Layouts) -> Vec<Rc<Element>> {
    (0..layouts.len())
        .map(|id| {
            let id = u32::try_from(id).unwrap_or(0);
            let described = describe(layouts, Ty::Struct(id));
            Rc::new(described.unwrap_or(Element::Int))
        })
        .chain(std::iter::once(Rc::new(Element::Str)))
        .collect()
}

/// How the runtime reads values of a kind from words.
pub fn describe(layouts: &Layouts, ty: Ty) -> Option<Element> {
    describe_numbered(layouts, ty, &Numbering::all(layouts))
}

/// `describe`, with each layout's shape numbered as `numbering` has it.
fn describe_numbered(layouts: &Layouts, ty: Ty, numbering: &Numbering) -> Option<Element> {
    let describe = |layouts: &Layouts, ty: Ty| describe_numbered(layouts, ty, numbering);
    let fields = |fields: &[Field]| -> Option<Vec<(Rc<str>, Element)>> {
        fields
            .iter()
            .map(|field| Some((Rc::from(field.name.as_str()), describe(layouts, field.ty)?)))
            .collect()
    };
    Some(match ty {
        Ty::Int => Element::Int,
        Ty::U64 => Element::U64,
        Ty::F32 => Element::F32,
        Ty::Float => Element::Float,
        Ty::Bool => Element::Bool,
        Ty::Boxed(id) => Element::Boxed(usize::try_from(numbering.number(id)?).ok()?),
        // By its block's address, whose own shape says it's a capability.
        Ty::Cap(_) => Element::Boxed(usize::try_from(fennec_rt::embed::heap::CAPABILITY_SHAPE).ok()?),
        Ty::Bytes => Element::Boxed(usize::try_from(fennec_rt::embed::heap::BYTES_SHAPE).ok()?),
        // By its block's address, whose own shape says what it keeps.
        Ty::Gen => Element::Boxed(0),
        Ty::Str if layouts.text_headers() => Element::Text,
        Ty::Str => Element::Str,
        Ty::Obj => Element::Object,
        // An optional string is a string's header, all zero for `None`, when strings are held
        // as headers. Otherwise an optional's one word is a block's address, a string's too.
        Ty::Maybe(Block::Str) if layouts.text_headers() => Element::Maybe(Rc::new(Element::Text)),
        Ty::Maybe(Block::Str) => Element::Maybe(Rc::new(Element::Str)),
        Ty::Maybe(block) => Element::Maybe(Rc::new(describe(layouts, block.ty())?)),
        Ty::Block(id) => describe(layouts, layouts.block(id)?)?,
        Ty::List(ListItem::Struct(id)) if layouts.through_lists(id).is_some() => {
            Element::ListOf(layouts.through_lists(id)?)
        }
        Ty::List(item) => Element::List(Rc::new(describe(layouts, item.ty())?)),
        Ty::Dict(key, value) => {
            let str_shape = numbering.str_shape();
            let shape = dicts::shape(str_shape, key, value)?;
            Element::Boxed(usize::try_from(shape).ok()?)
        }
        Ty::Struct(id) => {
            let layout = layouts.get(id)?;
            let name = Rc::from(layout.name.as_str());
            if let Some(cases) = &layout.cases {
                let cases: Option<Vec<_>> = cases
                    .iter()
                    .map(|case| Some((Rc::from(case.name.as_str()), fields(&case.fields)?)))
                    .collect();
                let words = usize::try_from(layout.size / WORD).ok()?;
                Element::Enum { name, cases: cases?, words }
            } else {
                let mut held = fields(&layout.fields)?;
                if layout.cell {
                    held.push((Rc::from(fennec_rt::packed::LOCK), Element::Int));
                }
                Element::Struct { name, fields: held }
            }
        }
        _ => return None,
    })
}

/// Which layouts a program's image has shapes for, and each one's number there: those it
/// uses, numbered in order, then strings' shape after them. A program's image needs nothing
/// for the rest, like the standard library's types it never makes.
pub struct Numbering {
    kept: Vec<u32>,
    numbers: HashMap<u32, u64>,
}

impl Numbering {
    /// Every layout, each numbered as itself.
    #[must_use]
    pub fn all(layouts: &Layouts) -> Numbering {
        let kept: Vec<u32> = (0..layouts.len()).filter_map(|id| u32::try_from(id).ok()).collect();
        let numbers = kept.iter().map(|id| (*id, u64::from(*id))).collect();
        Numbering { kept, numbers }
    }

    /// The layouts `used` names and every one their fields, cases, items and dicts hold,
    /// numbered in the order of their layouts.
    #[must_use]
    pub fn of(layouts: &Layouts, used: impl IntoIterator<Item = u32>) -> Numbering {
        let mut kept = BTreeSet::new();
        let mut waiting: Vec<u32> = used.into_iter().collect();
        while let Some(id) = waiting.pop() {
            if layouts.get(id).is_none() || !kept.insert(id) {
                continue;
            }
            let Some(layout) = layouts.get(id) else { continue };
            let cases = layout.cases.iter().flatten().flat_map(|case| &case.fields);
            for field in layout.fields.iter().chain(cases) {
                held_layouts(layouts, field.ty, &mut waiting);
            }
        }
        let kept: Vec<u32> = kept.into_iter().collect();
        let numbers =
            kept.iter().enumerate().filter_map(|(at, id)| Some((*id, u64::try_from(at).ok()?))).collect();
        Numbering { kept, numbers }
    }

    /// A layout's shape number, if the image has it.
    #[must_use]
    pub fn number(&self, id: u32) -> Option<u64> {
        self.numbers.get(&id).copied()
    }

    /// Strings' shape number, after every layout's.
    #[must_use]
    pub fn str_shape(&self) -> u64 {
        u64::try_from(self.kept.len()).unwrap_or(0)
    }
}

/// Adds to `found` each layout a value of `ty` holds: itself, or what its items, its optional's
/// value or its dict's words are.
pub fn held_layouts(layouts: &Layouts, ty: Ty, found: &mut Vec<u32>) {
    match ty {
        Ty::Struct(id) | Ty::Boxed(id) => found.push(id),
        Ty::List(item) => held_layouts(layouts, item.ty(), found),
        Ty::Maybe(block) => held_layouts(layouts, block.ty(), found),
        Ty::Block(number) => {
            if let Some(held) = layouts.block(number) {
                held_layouts(layouts, held, found);
            }
        }
        Ty::Dict(key, value) => {
            for word in [key, value] {
                held_by_word(layouts, word, found);
            }
        }
        _ => {}
    }
}

/// Adds to `found` the layout a dict's word holds, if it holds one.
pub fn held_by_word(layouts: &Layouts, word: Word, found: &mut Vec<u32>) {
    match word {
        Word::Plain(id) => found.push(id),
        Word::Block(number) => {
            if let Some(held) = layouts.block(number) {
                held_layouts(layouts, held, found);
            }
        }
        _ => {}
    }
}

/// What a program's blocks need the runtime to know, built up from the layouts: shapes and
/// string numbers, and the numbers of the kinds of items its lists hold.
pub struct Registry<'l> {
    layouts: &'l Layouts,
    tables: Tables,
    elements: HashMap<Ty, i64>,
    strings: HashMap<String, i64>,
    numbering: Numbering,
}

impl<'l> Registry<'l> {
    #[must_use]
    pub fn new(layouts: &'l Layouts) -> Self {
        Self::numbered(layouts, Numbering::all(layouts))
    }

    /// A registry whose tables have shapes for the layouts `numbering` keeps, numbered as it
    /// numbers them, and none for the rest.
    #[must_use]
    pub fn numbered(layouts: &'l Layouts, numbering: Numbering) -> Self {
        let blocks = (0..layouts.block_count())
            .filter_map(|number| describe_numbered(layouts, layouts.block(number)?, &numbering))
            .map(Rc::new)
            .collect();
        let shapes = numbering
            .kept
            .iter()
            .map(|id| {
                Rc::new(describe_numbered(layouts, Ty::Struct(*id), &numbering).unwrap_or(Element::Int))
            })
            .chain(std::iter::once(Rc::new(Element::Str)))
            .collect();
        let cells = numbering
            .kept
            .iter()
            .filter(|id| layouts.get(**id).is_some_and(|layout| layout.cell))
            .filter_map(|id| numbering.number(*id))
            .collect();
        let mut tables = Tables {
            shapes,
            cells,
            blocks,
            str_shape: numbering.str_shape(),
            text_headers: layouts.text_headers(),
            ..Tables::default()
        };
        // The numbers a type holding itself through a list refers to in its own description.
        let mut elements = HashMap::new();
        for (number, id) in layouts.all_through_lists().iter().enumerate() {
            let described = describe_numbered(layouts, Ty::Struct(*id), &numbering).unwrap_or(Element::Int);
            tables.elements.push(Rc::new(described));
            elements.insert(Ty::Struct(*id), i64::try_from(number).unwrap_or(0));
        }
        Self { layouts, tables, elements, strings: HashMap::new(), numbering }
    }

    /// The number of the kind of item a list of these holds, which `fennec_list_new` takes.
    pub fn element(&mut self, item: ListItem) -> Option<i64> {
        if let Some(number) = self.elements.get(&item.ty()) {
            return Some(*number);
        }
        let described = describe_numbered(self.layouts, item.ty(), &self.numbering)?;
        let number = i64::try_from(self.tables.elements.len()).ok()?;
        self.tables.elements.push(Rc::new(described));
        self.elements.insert(item.ty(), number);
        Some(number)
    }

    /// The shape number of a boxed value's or a string's block, or of a block holding a
    /// plain struct, enum or tuple laid out as it is inline: a backend copies one into such a
    /// block to hand it to `fennec_box_to_value`, and out of what `fennec_box_from_value`
    /// gives back.
    #[must_use]
    pub fn shape(&self, ty: Ty) -> Option<u64> {
        match ty {
            Ty::Boxed(id) | Ty::Struct(id) => self.numbering.number(id),
            Ty::Str => Some(self.tables.str_shape),
            _ => None,
        }
    }

    /// Strings' shape number in these tables, after every layout's they have.
    #[must_use]
    pub fn str_shape(&self) -> u64 {
        self.tables.str_shape
    }

    /// The number of a string literal, which `fennec_aot_literal` takes.
    pub fn string(&mut self, text: &str) -> i64 {
        if let Some(number) = self.strings.get(text) {
            return *number;
        }
        let number = i64::try_from(self.tables.strings.len()).unwrap_or(0);
        self.tables.strings.push(text.to_owned());
        self.strings.insert(text.to_owned(), number);
        number
    }

    /// The finished tables, for a program's image.
    #[must_use]
    pub fn finish(self) -> Tables {
        self.tables
    }
}

#[cfg(test)]
mod tests {
    use super::{Layouts, ListItem, Registry, Ty};

    #[test]
    fn each_kind_and_string_gets_one_number() {
        let layouts = Layouts::default();
        let mut registry = Registry::new(&layouts);
        assert_eq!(registry.element(ListItem::Int), Some(0));
        assert_eq!(registry.element(ListItem::Float), Some(1));
        assert_eq!(registry.element(ListItem::Int), Some(0));
        assert_eq!(registry.string("a"), 0);
        assert_eq!(registry.string("b"), 1);
        assert_eq!(registry.string("a"), 0);
        assert_eq!(registry.shape(Ty::Str), Some(0));
        let tables = registry.finish();
        assert_eq!((tables.elements.len(), tables.strings.len(), tables.shapes.len()), (2, 2, 1));
    }
}
