//! How a value of each type is held: in LLVM's terms, and in memory the way the runtime lays it
//! out, with `fennec_layout` deciding. Numbers are a word each, a bool included.

use std::collections::BTreeMap;

use fennec_check::Type;
use fennec_layout::{Block, Layouts, ListItem, Ty, Written};
use fennec_rt::embed::dicts::Word;

const WORD: u32 = 8;

/// The name a tuple, optional, result or union goes by, laid out as a struct or enum of that
/// name, so that where one fits in memory, and what it holds on to, works as for any other type.
pub(crate) fn canon(ty: &Type) -> Type {
    match ty {
        // An array kept in place is a struct with a field for each item, named by its position.
        Type::Tuple(_) | Type::Generic(..) | Type::Errors(_) | Type::Array(..) | Type::Union(_) => {
            Type::Named(ty.to_string())
        }
        Type::Optional(inner) => Type::Named(format!("Option[{inner}]")),
        Type::Result(value, error) => Type::Named(format!("Result[{value}, {error}]")),
        // Every `Id` looks the same whatever it's of.
        Type::Id(_) => Type::Named("Id".to_owned()),
        Type::Table(item) => Type::Named(format!("Table[{item}]")),
        Type::List(item) => Type::List(Box::new(canon(item))),
        Type::Set(item) => Type::Set(Box::new(canon(item))),
        Type::Dict(key, value) => Type::Dict(Box::new(canon(key)), Box::new(canon(value))),
        other => other.clone(),
    }
}

/// As [`canon`], but an optional of a block, outside a list or dict, stays an optional: one
/// word, the block or null for `None`. `layouts` says which named types are blocks.
/// With no layouts, as the first is made, an optional of what's a block whatever the layouts say,
/// like a string or a list, is one word already, as it is in the finished layout: only an optional
/// of a named type waits to know whether that type is a block.
pub(crate) fn canon_in(layouts: Option<&Layouts>, ty: &Type) -> Type {
    if let Type::Optional(inner) = ty {
        let one_word = match layouts {
            Some(layouts) => one_word_optional(layouts, inner),
            None => always_a_block(inner),
        };
        if one_word {
            return Type::Optional(Box::new(canon(inner)));
        }
    }
    canon(ty)
}

/// Whether a value of this type is held by a block whatever the layouts say.
fn always_a_block(ty: &Type) -> bool {
    matches!(
        canon(ty),
        Type::Str | Type::List(_) | Type::Dict(..) | Type::Set(_) | Type::Bytes | Type::Buffer(_)
    )
}

/// Whether an optional of `inner` is one word, the address of the block `inner` is held by or null
/// for `None`, which it is when `inner` is held by a block. The one place this is decided, for the
/// layouts and every local alike, so a field and a local never hold the same optional two ways.
pub(crate) fn one_word_optional(layouts: &Layouts, inner: &Type) -> bool {
    Block::of(layouts.ty(&canon(inner))).is_some()
}

/// The definitions of the tuples, optionals and results a type mentions.
pub(crate) fn define_named(ty: &Type, layouts: Option<&Layouts>, out: &mut BTreeMap<String, Written>) {
    let field = |name: &str, ty: &Type| (name.to_owned(), canon_in(layouts, ty), None);
    match ty {
        Type::Tuple(items) => {
            for item in items {
                define_named(item, layouts, out);
            }
            let fields = items.iter().enumerate().map(|(at, item)| field(&format!("_{at}"), item)).collect();
            out.insert(ty.to_string(), Written::Struct(fields));
        }
        Type::Optional(inner) => {
            define_named(inner, layouts, out);
            let cases =
                vec![("None".to_owned(), Vec::new()), ("Some".to_owned(), vec![field("value", inner)])];
            out.insert(format!("Option[{inner}]"), Written::Enum(cases));
        }
        Type::Result(value, error) => {
            define_named(value, layouts, out);
            define_named(error, layouts, out);
            // A result of nothing holds nothing when it succeeds.
            let ok = if **value == Type::Unit { Vec::new() } else { vec![field("value", value)] };
            let cases = vec![("Ok".to_owned(), ok), ("Err".to_owned(), vec![field("error", error)])];
            out.insert(format!("Result[{value}, {error}]"), Written::Enum(cases));
        }
        // A union: a case for each member, by its place among them, holding the member. It's kept
        // in place like a plain enum, the case's number first, sized for its largest member.
        Type::Union(members) => {
            for member in members {
                define_named(member, layouts, out);
            }
            let cases =
                members.iter().map(|member| (member.to_string(), vec![field("value", member)])).collect();
            out.insert(ty.to_string(), Written::Enum(cases));
        }
        // One of several errors: a case for each, named for its type.
        Type::Errors(names) => {
            let held = |name: &String| (name.clone(), vec![field("error", &Type::Named(name.clone()))]);
            out.insert(ty.to_string(), Written::Enum(names.iter().map(held).collect()));
        }
        Type::Id(_) => {
            out.insert(
                "Id".to_owned(),
                // Its slot in the high 32 bits of `key` and its generation in the low 32, so an
                // `Id` is two words.
                Written::Struct(vec![field("key", &Type::Int), field("tag", &Type::Int)]),
            );
        }
        Type::Table(item) => {
            define_named(item, layouts, out);
            let fields = vec![
                field("items", &Type::List(item.clone())),
                field("generations", &Type::List(Box::new(Type::Int))),
                field("live", &Type::Int),
                field("free_from", &Type::Int),
                field("tag", &Type::Int),
            ];
            out.insert(format!("Table[{item}]"), Written::Struct(fields));
        }
        Type::Array(item, size) => {
            define_named(item, layouts, out);
            let fields = (0..*size).map(|at| field(&at.to_string(), item)).collect();
            out.insert(ty.to_string(), Written::Struct(fields));
        }
        Type::List(item) | Type::Set(item) => define_named(item, layouts, out),
        Type::Dict(key, value) => {
            define_named(key, layouts, out);
            define_named(value, layouts, out);
        }
        _ => {}
    }
}

/// What this backend can hold so far.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Int,
    Float,
    /// A `u64`: its bits in a word, read as unsigned.
    U64,
    /// An `f32`: held as the `f64` that's exactly its value, rounded to an `f32` after each
    /// operation.
    F32,
    Bool,
    Unit,
    /// A capability: the address of a block saying what it may do, owned like any block. The
    /// whole ones are the runtime's, and never go.
    Cap,
    /// A plain struct, by its layout's number, in a slot of its own.
    Struct(u32),
    /// A plain enum, by its layout's number: the case's number, then its fields, a word each.
    Enum(u32),
    /// The address of a boxed struct's or enum's block, which something owns.
    Boxed(u32),
    /// The address of a list's block, which something owns.
    List(ListItem),
    /// The address of a string's block, which something owns; or its 24-byte header, when the
    /// layouts hold strings as headers.
    Str,
    /// The address of a block of `bytes`, which something owns.
    Bytes,
    /// A string as its block's address where strings are otherwise headers: how a dict holds one.
    TextBlock,
    /// An optional string where strings are headers: a header, all zero for `None`.
    MaybeText,
    /// The address of a dict's or set's block, which something owns, by what it holds.
    Dict(Word, Word),
    /// The address of a closure, which something owns: what it takes, and gives back.
    Closure(Vec<Kind>, Box<Kind>),
    /// The address of a generator's block, which something owns. Every generator's block starts
    /// the same way, so this needn't say which.
    Gen,
    /// An optional of a block: the block's address, or null for `None`.
    Maybe(Box<Kind>),
}

impl Kind {
    pub(crate) fn of(layouts: &Layouts, ty: &Type) -> Result<Kind, String> {
        if let Type::Arrow(params, returns) = ty {
            let params: Result<Vec<Kind>, String> =
                params.iter().map(|param| Kind::of(layouts, param)).collect();
            return Ok(Kind::Closure(params?, Box::new(Kind::of(layouts, returns)?)));
        }
        if let Type::Iterator(_) = ty {
            return Ok(Kind::Gen);
        }
        let held = layouts.ty(&canon_in(Some(layouts), ty));
        Kind::from_ty(layouts, held)
            .ok_or_else(|| format!("it uses a {ty:?}, which this backend doesn't take yet"))
    }

    pub(crate) fn from_ty(layouts: &Layouts, ty: Ty) -> Option<Kind> {
        Some(match ty {
            Ty::Int => Kind::Int,
            Ty::Float => Kind::Float,
            Ty::U64 => Kind::U64,
            Ty::F32 => Kind::F32,
            Ty::Bool => Kind::Bool,
            Ty::Unit => Kind::Unit,
            Ty::Cap(_) => Kind::Cap,
            Ty::Struct(id) if layouts.get(id).is_some_and(|layout| layout.cases.is_none()) => {
                Kind::Struct(id)
            }
            Ty::Struct(id) => Kind::Enum(id),
            Ty::Boxed(id) => Kind::Boxed(id),
            Ty::List(item) => Kind::List(item),
            Ty::Str => Kind::Str,
            Ty::Bytes => Kind::Bytes,
            Ty::Gen => Kind::Gen,
            Ty::Dict(key, value) => Kind::Dict(key, value),
            Ty::Maybe(Block::Str) if layouts.text_headers() => Kind::MaybeText,
            Ty::Maybe(block) => Kind::Maybe(Box::new(Kind::from_ty(layouts, block.ty())?)),
            // A list's item that's a block of its own: what that block is.
            Ty::Block(id) => Kind::from_ty(layouts, layouts.block(id)?)?,
            _ => return None,
        })
    }

    /// Whether this is the address of a block with an owner count.
    pub(crate) fn counted(&self) -> bool {
        matches!(
            self,
            Kind::Cap
                | Kind::List(_)
                | Kind::Str
                | Kind::Bytes
                | Kind::TextBlock
                | Kind::MaybeText
                | Kind::Boxed(_)
                | Kind::Dict(..)
                | Kind::Closure(..)
                | Kind::Gen
                | Kind::Maybe(_)
        )
    }

    /// Whether this is held in memory and passed by address.
    pub(crate) fn in_memory(&self) -> bool {
        matches!(self, Kind::Struct(_) | Kind::Enum(_))
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
            Kind::Int | Kind::Bool | Kind::U64 => "i64".to_owned(),
            Kind::Float | Kind::F32 => "double".to_owned(),
            Kind::Unit => "void".to_owned(),
            Kind::Str if layouts.text_headers() => crate::headers::HEADER.to_owned(),
            Kind::MaybeText => crate::headers::HEADER.to_owned(),
            Kind::Cap
            | Kind::List(_)
            | Kind::Str
            | Kind::Bytes
            | Kind::TextBlock
            | Kind::Boxed(_)
            | Kind::Dict(..)
            | Kind::Closure(..)
            | Kind::Gen
            | Kind::Maybe(_) => "ptr".to_owned(),
            Kind::Enum(id) => format!("[{} x i64]", layouts.get(*id).map_or(1, |layout| layout.size / WORD)),
            Kind::Struct(id) => {
                format!("%\"s.{}\"", layouts.get(*id).map_or("?", |layout| layout.name.as_str()))
            }
        }
    }

    /// How many bytes it takes up inside something else.
    pub(crate) fn size(&self, layouts: &Layouts) -> u32 {
        match self {
            Kind::Unit => 0,
            Kind::Struct(id) | Kind::Enum(id) => layouts.get(*id).map_or(0, |layout| layout.size),
            Kind::Str if layouts.text_headers() => 3 * WORD,
            Kind::MaybeText => 3 * WORD,
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
        ListItem::U64 => "e_u64".to_owned(),
        ListItem::F32 => "e_f32".to_owned(),
        ListItem::Struct(id) => format!("e_s_{}", layouts.get(id).map_or("?", |layout| layout.name.as_str())),
        ListItem::Str => "e_str".to_owned(),
        ListItem::Boxed(id) => format!("e_b_{}", layouts.get(id).map_or("?", |layout| layout.name.as_str())),
        // Named by what it holds, so the name stays the same while the program's numbering moves.
        ListItem::Block(id) => format!("e_k_{:?}", layouts.block(id)),
    }
}

/// The line defining a plain struct's type, and those of the structs in it.
pub(crate) fn struct_definitions(layouts: &Layouts, id: u32, out: &mut Vec<String>) {
    let Some(layout) = layouts.get(id) else { return };
    // An array's items, as LLVM's own array of them: one type however many there are.
    if is_array(layout)
        && let Some(first) = layout.fields.first()
    {
        if let Ty::Struct(inner) = first.ty
            && layouts.get(inner).is_some_and(|layout| layout.cases.is_none())
        {
            struct_definitions(layouts, inner, out);
        }
        let item =
            Kind::from_ty(layouts, first.ty).map_or_else(|| "i64".to_owned(), |kind| kind.memory(layouts));
        out.push(format!("%\"s.{}\" = type {{ [{} x {item}] }}\n", layout.name, layout.fields.len()));
        return;
    }
    let mut fields = Vec::with_capacity(layout.fields.len());
    for field in &layout.fields {
        if let Ty::Struct(inner) = field.ty
            && layouts.get(inner).is_some_and(|layout| layout.cases.is_none())
        {
            struct_definitions(layouts, inner, out);
        }
        fields.push(
            Kind::from_ty(layouts, field.ty).map_or_else(|| "i64".to_owned(), |kind| kind.memory(layouts)),
        );
    }
    out.push(format!("%\"s.{}\" = type {{ {} }}\n", layout.name, fields.join(", ")));
}

/// Whether a layout is an array kept in place: a field for each item, `0` first.
pub(crate) fn is_array(layout: &fennec_layout::Layout) -> bool {
    layout.name.starts_with("array[") && layout.cases.is_none()
}
