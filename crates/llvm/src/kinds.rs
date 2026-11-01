//! How a value of each type is held: in LLVM's terms, and in memory the way the runtime and the
//! JIT lay it out, with `fennec_layout` deciding. Numbers are a word each, a bool included.

use fennec_check::Type;
use fennec_layout::{Layouts, ListItem, Ty};

const WORD: u32 = 8;

/// What this backend can hold so far.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Int,
    Float,
    Bool,
    Unit,
    /// A capability, which needs nothing at run time.
    Cap,
    /// A plain struct, by its layout's number, in a slot of its own.
    Struct(u32),
    /// A tuple, laid out like a plain struct.
    Tuple(Vec<Kind>),
    /// The address of a list's block, which something owns.
    List(ListItem),
    /// The address of a string's block, which something owns.
    Str,
}

impl Kind {
    pub(crate) fn of(layouts: &Layouts, ty: &Type) -> Result<Kind, String> {
        if let Type::Tuple(items) = ty {
            let items: Result<Vec<Kind>, String> = items.iter().map(|item| Kind::of(layouts, item)).collect();
            let items = items?;
            if items.iter().any(Kind::counted) {
                return Err("it uses a tuple holding a list or string, which this backend doesn't take yet"
                    .to_owned());
            }
            return Ok(Kind::Tuple(items));
        }
        let held = layouts.ty(ty);
        Kind::from_ty(layouts, held)
            .ok_or_else(|| format!("it uses a {ty:?}, which this backend doesn't take yet"))
    }

    pub(crate) fn from_ty(layouts: &Layouts, ty: Ty) -> Option<Kind> {
        Some(match ty {
            Ty::Int => Kind::Int,
            Ty::Float => Kind::Float,
            Ty::Bool => Kind::Bool,
            Ty::Unit => Kind::Unit,
            Ty::Cap(_) => Kind::Cap,
            Ty::Struct(id) if layouts.get(id).is_some_and(|layout| layout.cases.is_none()) => {
                Kind::Struct(id)
            }
            Ty::List(item) if matches!(item, ListItem::Int | ListItem::Float | ListItem::Bool) => {
                Kind::List(item)
            }
            Ty::List(ListItem::Struct(id))
                if layouts.get(id).is_some_and(|layout| layout.cases.is_none()) =>
            {
                Kind::List(ListItem::Struct(id))
            }
            Ty::Str => Kind::Str,
            _ => return None,
        })
    }

    /// Whether this is the address of a block with an owner count.
    pub(crate) fn counted(&self) -> bool {
        matches!(self, Kind::List(_) | Kind::Str)
    }

    /// Whether this is held in memory and passed by address.
    pub(crate) fn in_memory(&self) -> bool {
        matches!(self, Kind::Struct(_) | Kind::Tuple(_))
    }

    /// How LLVM holds it in a local's slot.
    pub(crate) fn slot(&self, layouts: &Layouts) -> String {
        match self {
            Kind::Bool => "i1".to_owned(),
            other => other.memory(layouts),
        }
    }

    /// How LLVM holds it inside a struct, tuple or list.
    pub(crate) fn memory(&self, layouts: &Layouts) -> String {
        match self {
            Kind::Int | Kind::Bool => "i64".to_owned(),
            Kind::Float => "double".to_owned(),
            Kind::Unit => "void".to_owned(),
            Kind::Cap | Kind::List(_) | Kind::Str => "ptr".to_owned(),
            Kind::Struct(id) => {
                format!("%\"s.{}\"", layouts.get(*id).map_or("?", |layout| layout.name.as_str()))
            }
            Kind::Tuple(items) => {
                let items: Vec<String> = items.iter().map(|item| item.memory(layouts)).collect();
                format!("{{ {} }}", items.join(", "))
            }
        }
    }

    /// How many bytes it takes up inside something else.
    pub(crate) fn size(&self, layouts: &Layouts) -> u32 {
        match self {
            Kind::Unit => 0,
            Kind::Struct(id) => layouts.get(*id).map_or(0, |layout| layout.size),
            Kind::Tuple(items) => items.iter().map(|item| item.size(layouts)).sum(),
            _ => WORD,
        }
    }
}

/// The symbol a list item's kind has its number under.
pub(crate) fn element_symbol(layouts: &Layouts, item: ListItem) -> String {
    match item {
        ListItem::Int => "e_int".to_owned(),
        ListItem::Float => "e_float".to_owned(),
        ListItem::Bool => "e_bool".to_owned(),
        ListItem::Struct(id) => format!("e_s_{}", layouts.get(id).map_or("?", |layout| layout.name.as_str())),
        ListItem::Str => "e_str".to_owned(),
        ListItem::Boxed(id) => format!("e_b_{}", layouts.get(id).map_or("?", |layout| layout.name.as_str())),
    }
}

/// The line defining a plain struct's type, and those of the structs in it.
pub(crate) fn struct_definitions(layouts: &Layouts, id: u32, out: &mut Vec<String>) {
    let Some(layout) = layouts.get(id) else { return };
    let mut fields = Vec::with_capacity(layout.fields.len());
    for field in &layout.fields {
        if let Ty::Struct(inner) = field.ty {
            struct_definitions(layouts, inner, out);
        }
        fields.push(
            Kind::from_ty(layouts, field.ty).map_or_else(|| "i64".to_owned(), |kind| kind.memory(layouts)),
        );
    }
    out.push(format!("%\"s.{}\" = type {{ {} }}\n", layout.name, fields.join(", ")));
}
