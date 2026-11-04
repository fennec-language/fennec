//! The text of a value, read straight from its block by its shape: what printing a list or a
//! struct shows, as `format` writes it for the interpreter, with nothing made along the way.

use std::fmt::Write as _;

use super::named::{is_id_map, is_option, is_result};
use super::{
    CAPABILITY_SHAPE, HEADER_WORDS, State, block_text, cell_value, is_text, shape_of, shape_word, type_name,
};
use crate::packed::{Element, Fields};
use fennec_syntax::Diagnostic;
use fennec_syntax::Span;
use fennec_syntax::rename::shown_type_name;

/// Text being written, and whether a value in it has a type that writes its own.
#[derive(Default)]
pub(in crate::embed) struct Written {
    text: String,
    own_text: bool,
}

impl Written {
    pub(in crate::embed) fn push(&mut self, c: char) {
        self.text.push(c);
    }

    pub(in crate::embed) fn push_str(&mut self, text: &str) {
        self.text.push_str(text);
    }
}

impl std::fmt::Write for Written {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        self.text.push_str(text);
        Ok(())
    }
}

/// A value's text as printing shows it: a string as it is, and anything else as `quoted` writes
/// it. A value holding one whose type writes its own text with `def str` is for the interpreter
/// to show, as that runs the method.
pub(in crate::embed) fn shown(state: &mut State, address: u64) -> Result<String, Diagnostic> {
    let mut out = Written::default();
    match state.heap.block(address) {
        Some(block) if is_text(state, block) => out.push_str(&block_text(block)),
        _ => quoted(state, address, &mut out),
    }
    if !out.own_text {
        return Ok(out.text);
    }
    let value = super::read_block(state, address);
    state.interp.shown(&value, Span::default())
}

/// Whether a layout's type writes its own text.
fn writes_own_text(state: &State, name: &str) -> bool {
    state.interp.type_info(type_name(name)).is_some_and(|info| info.methods.contains_key("str"))
}

/// Writes the value at a block's address as it shows inside a collection: strings are quoted.
pub(in crate::embed) fn quoted(state: &State, address: u64, out: &mut Written) {
    let Some(block) = state.heap.block(address) else {
        out.push_str("None");
        return;
    };
    if let Some(value) = cell_value(state, block) {
        out.push_str(&crate::format::repr(&state.expanded(value)));
    } else if shape_word(block) == Some(CAPABILITY_SHAPE) {
        out.push_str(&crate::format::repr(&crate::embed::capabilities::read(state, block)));
    } else if is_text(state, block) {
        let _ = write!(out, "{:?}", block_text(block));
    } else if crate::embed::dicts::is_dict(state, block) {
        crate::embed::dicts::shown(state, block, out);
    } else if crate::embed::lists::element_of(state, block).is_some() {
        crate::embed::lists::shown(state, block, out);
    } else if let Some(shape) = shape_of(state, block) {
        let mut words = block.get(HEADER_WORDS..).unwrap_or_default().iter();
        item(state, &shape, &mut words, out);
    } else {
        out.push_str("None");
    }
}

/// Writes items, separated, each taking its words from `words`, `count` of them.
pub(in crate::embed) fn items(
    state: &State,
    element: &Element,
    words: &mut dyn Iterator<Item = &u64>,
    count: usize,
    out: &mut Written,
) {
    for at in 0..count {
        if at > 0 {
            out.push_str(", ");
        }
        item(state, element, words, out);
    }
}

fn word(words: &mut dyn Iterator<Item = &u64>) -> u64 {
    words.next().copied().unwrap_or(0)
}

/// Writes the value laid out at the start of `words`, taking the words it uses.
pub(in crate::embed) fn item(
    state: &State,
    element: &Element,
    words: &mut dyn Iterator<Item = &u64>,
    out: &mut Written,
) {
    match element {
        Element::Int => {
            let _ = write!(out, "{}", i64::from_ne_bytes(word(words).to_ne_bytes()));
        }
        Element::Float => out.push_str(&crate::format::float(f64::from_bits(word(words)))),
        Element::Bool => out.push_str(if word(words) == 0 { "False" } else { "True" }),
        Element::Boxed(_) | Element::Str | Element::Object | Element::List(_) => {
            quoted(state, word(words), out);
        }
        Element::Text => header(state, [word(words), word(words), word(words)], out),
        element if element.is_optional_text() => match [word(words), word(words), word(words)] {
            [0, 0, 0] => out.push_str("None"),
            held => header(state, held, out),
        },
        Element::Maybe(inner) => match word(words) {
            0 => out.push_str("None"),
            address => item(state, inner, &mut std::iter::once(&address), out),
        },
        Element::Struct { name, fields } if name.starts_with('(') => {
            out.push('(');
            for (at, (_, element)) in fields.iter().enumerate() {
                if at > 0 {
                    out.push_str(", ");
                }
                item(state, element, words, out);
            }
            out.push_str(if fields.len() == 1 { ",)" } else { ")" });
        }
        Element::Struct { name, fields } if name.as_ref() == "Id" => id(fields, words, out),
        Element::Struct { name, fields } if is_id_map(name) => id_map(state, fields, words, out),
        Element::Struct { name, fields } => {
            out.own_text |= writes_own_text(state, name);
            out.push_str(shown_type_name(type_name(name)));
            named_fields(state, fields, words, out);
        }
        Element::Enum { name, cases, words: size } => {
            let mut mine = (&mut *words).take(*size);
            let at = usize::try_from(word(&mut mine)).unwrap_or(0);
            let (case, fields) = cases.get(at).or_else(|| cases.first()).cloned().unwrap_or_default();
            enum_case(state, (name, &case), &fields, &mut mine, out);
            mine.for_each(drop);
        }
    }
}

fn header(state: &State, words: [u64; 3], out: &mut Written) {
    let _ = write!(out, "{:?}", crate::embed::text::text_of(state, &words));
}

fn enum_case(
    state: &State,
    (name, case): (&str, &str),
    fields: &Fields,
    words: &mut dyn Iterator<Item = &u64>,
    out: &mut Written,
) {
    if is_option(name) {
        match fields.first() {
            Some((_, element)) if case == "Some" => item(state, element, words, out),
            _ => out.push_str("None"),
        }
    } else if is_result(name) {
        out.push_str(if case == "Err" { "Err(" } else { "Ok(" });
        if let Some((_, element)) = fields.first() {
            item(state, element, words, out);
        }
        out.push(')');
    } else {
        out.own_text |= writes_own_text(state, name);
        let _ = write!(out, "{}.{case}", shown_type_name(type_name(name)));
        if !fields.is_empty() {
            named_fields(state, fields, words, out);
        }
    }
}

/// `(x=1, y=2)`.
fn named_fields(state: &State, fields: &Fields, words: &mut dyn Iterator<Item = &u64>, out: &mut Written) {
    out.push('(');
    for (at, (field, element)) in fields.iter().enumerate() {
        if at > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "{field}=");
        item(state, element, words, out);
    }
    out.push(')');
}

/// `Id(3)`: only its slot shows.
fn id(fields: &Fields, words: &mut dyn Iterator<Item = &u64>, out: &mut Written) {
    let mut slot = 0;
    for (field, element) in fields {
        let mut mine = (&mut *words).take(element.words());
        let first = word(&mut mine);
        mine.for_each(drop);
        if field.as_ref() == "slot" {
            slot = first;
        }
    }
    let _ = write!(out, "Id({slot})");
}

/// `IdMap([a, b])`: what's in the slots still in use.
fn id_map(state: &State, fields: &Fields, words: &mut dyn Iterator<Item = &u64>, out: &mut Written) {
    let (mut held, mut generations) = (0, 0);
    for (field, element) in fields {
        let mut mine = (&mut *words).take(element.words());
        let address = word(&mut mine);
        mine.for_each(drop);
        match field.as_ref() {
            "items" => held = address,
            "generations" => generations = address,
            _ => {}
        }
    }
    out.push_str("IdMap([");
    crate::embed::lists::shown_where_used(state, held, generations, out);
    out.push_str("])");
}
