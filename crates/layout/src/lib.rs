//! How values are laid out in memory, which every backend and the runtime have to agree on:
//! which types are plain, which are boxed, where fields go, and how the runtime reads them.
//! It knows nothing about generating code.

use std::collections::HashMap;
use std::rc::Rc;

pub use fennec_interp::embed::dicts::Word;
use fennec_interp::embed::{Tables, dicts};
use fennec_interp::packed::Element;

mod layouts;

pub use layouts::{Case, Field, Layout, Layouts, WORD, Written, WrittenField};

/// How native code holds a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ty {
    Int,
    Float,
    Bool,
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
    /// A dict with these keys and values: the address of a block the runtime owns, shared
    /// by every copy until one changes it.
    Dict(Word, Word),
    /// A list of numbers, bools or plain structs, packed one after another: the address of
    /// a block the runtime owns, shared by every copy until one changes it.
    List(ListItem),
    /// A function known while compiling, by its number there, which needs nothing at run
    /// time: calls to it are lowered right where they are.
    Known(u32),
}

/// What a native list's items are.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ListItem {
    Int,
    Float,
    Bool,
    /// A plain struct or enum, by its layout's number.
    Struct(u32),
    /// The address of a string's block, which the list owns.
    Str,
    /// The address of a boxed value's block, which the list owns, by its layout's number.
    Boxed(u32),
}

impl ListItem {
    /// The kind of item a list of `ty` holds natively, if it can.
    pub fn of(ty: Ty) -> Option<ListItem> {
        Some(match ty {
            Ty::Int => ListItem::Int,
            Ty::Float => ListItem::Float,
            Ty::Bool => ListItem::Bool,
            Ty::Struct(id) => ListItem::Struct(id),
            Ty::Str => ListItem::Str,
            Ty::Boxed(id) => ListItem::Boxed(id),
            _ => return None,
        })
    }

    /// How native code holds one item.
    pub fn ty(self) -> Ty {
        match self {
            ListItem::Int => Ty::Int,
            ListItem::Float => Ty::Float,
            ListItem::Bool => Ty::Bool,
            ListItem::Struct(id) => Ty::Struct(id),
            ListItem::Str => Ty::Str,
            ListItem::Boxed(id) => Ty::Boxed(id),
        }
    }
}

impl Ty {
    /// Whether values of this kind are addresses of blocks with an owner count.
    pub fn counted(self) -> bool {
        matches!(self, Ty::Boxed(_) | Ty::Str | Ty::Dict(..) | Ty::List(_))
    }

    pub fn name(self) -> &'static str {
        match self {
            Ty::Int => "int",
            Ty::Float => "float",
            Ty::Bool => "bool",
            Ty::Unit => "nothing",
            Ty::Cap(cap) => cap.name(),
            Ty::Obj => "value",
            Ty::Struct(_) => "struct",
            Ty::Boxed(_) => "boxed value",
            Ty::Str => "str",
            Ty::Dict(..) => "dict",
            Ty::List(_) => "list",
            Ty::Known(_) => "function",
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
}

impl Cap {
    pub fn named(name: &str) -> Option<Cap> {
        match name {
            "Stdout" => Some(Cap::Stdout),
            "Fs" => Some(Cap::Fs),
            "Clock" => Some(Cap::Clock),
            "Args" => Some(Cap::Args),
            "Stdin" => Some(Cap::Stdin),
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
    let fields = |fields: &[Field]| -> Option<Vec<(Rc<str>, Element)>> {
        fields
            .iter()
            .map(|field| Some((Rc::from(field.name.as_str()), describe(layouts, field.ty)?)))
            .collect()
    };
    Some(match ty {
        Ty::Int => Element::Int,
        Ty::Float => Element::Float,
        Ty::Bool => Element::Bool,
        Ty::Boxed(id) => Element::Boxed(usize::try_from(id).ok()?),
        Ty::Str => Element::Str,
        Ty::Obj => Element::Object,
        Ty::List(item) => Element::List(Rc::new(describe(layouts, item.ty())?)),
        Ty::Dict(key, value) => {
            let str_shape = u64::try_from(layouts.len()).ok()?;
            let shape = dicts::shape(str_shape, key, value)?;
            Element::Boxed(usize::try_from(shape).ok()?)
        }
        Ty::Struct(id) => {
            let layout = layouts.get(id)?;
            let name = Rc::from(layout.name.as_str());
            match &layout.cases {
                Some(cases) => {
                    let cases: Option<Vec<_>> = cases
                        .iter()
                        .map(|case| Some((Rc::from(case.name.as_str()), fields(&case.fields)?)))
                        .collect();
                    let words = usize::try_from(layout.size / WORD).ok()?;
                    Element::Enum { name, cases: cases?, words }
                }
                None => Element::Struct { name, fields: fields(&layout.fields)? },
            }
        }
        _ => return None,
    })
}

/// What a program's blocks need the runtime to know, built up from the layouts: shapes and
/// string numbers, and the numbers of the kinds of items its lists hold.
pub struct Registry<'l> {
    layouts: &'l Layouts,
    tables: Tables,
    elements: HashMap<Ty, i64>,
    strings: HashMap<String, i64>,
}

impl<'l> Registry<'l> {
    #[must_use]
    pub fn new(layouts: &'l Layouts) -> Self {
        let tables = Tables {
            shapes: shapes(layouts),
            str_shape: u64::try_from(layouts.len()).unwrap_or(0),
            ..Tables::default()
        };
        Self { layouts, tables, elements: HashMap::new(), strings: HashMap::new() }
    }

    /// The number of the kind of item a list of these holds, which `fennec_list_new` takes.
    pub fn element(&mut self, item: ListItem) -> Option<i64> {
        if let Some(number) = self.elements.get(&item.ty()) {
            return Some(*number);
        }
        let described = describe(self.layouts, item.ty())?;
        let number = i64::try_from(self.tables.elements.len()).ok()?;
        self.tables.elements.push(Rc::new(described));
        self.elements.insert(item.ty(), number);
        Some(number)
    }

    /// The shape number of a boxed value's or a string's block.
    #[must_use]
    pub fn shape(&self, ty: Ty) -> Option<u64> {
        match ty {
            Ty::Boxed(id) => Some(u64::from(id)),
            Ty::Str => Some(self.tables.str_shape),
            _ => None,
        }
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
