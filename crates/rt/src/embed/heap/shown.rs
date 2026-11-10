//! The text of a value, read straight from its block by its shape: what printing a list or a
//! struct shows, as `format` writes it for a `Value`, with nothing made along the way.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::rc::Rc;
use std::sync::Mutex;

use fennec_syntax::rename::shown_type_name;

use super::named::{is_array, is_id_map, is_one_of_several, is_option, is_result};
use super::{
    CAPABILITY_SHAPE, HEADER_WORDS, State, block_text, cell_value, is_text, shape_of, shape_word, type_name,
    with,
};
use crate::packed::{Element, Fields};
use crate::value::Value;

/// How native code writes a value of a type that writes its own text: given where the value's
/// fields are, it hands the text to `own_text_written`.
type WriteOwn = extern "C" fn(i64);

/// The types that write their own text, by the names of their layouts, for every thread.
static OWN_TEXTS: Mutex<Vec<(String, WriteOwn)>> = Mutex::new(Vec::new());

thread_local! {
    /// The text native code just handed over.
    static HANDED_OVER: RefCell<String> = const { RefCell::new(String::new()) };
}

/// `fennec_own_text(name, write)`: says how to write a type, by its layout's name, a string's
/// block.
pub extern "C" fn own_text(name: i64, write: WriteOwn) {
    let name = with(|state| {
        u64::try_from(name).ok().and_then(|at| state.heap.block(at)).map(block_text).unwrap_or_default()
    });
    OWN_TEXTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((name, write));
}

/// `fennec_own_text_written(header)`: the text a value wrote, which is owned once.
pub extern "C" fn own_text_written(t0: u64, t1: u64, t2: u64) {
    let words = [t0, t1, t2];
    let text = with(|state| {
        let text = crate::embed::text::text_of(state, &words);
        if let Some(block) = crate::embed::text::block_of(&words) {
            super::release(state, block);
        }
        text
    });
    HANDED_OVER.set(text);
}

/// How a layout's type writes its own text, if it does.
fn own_writer(name: &str) -> Option<WriteOwn> {
    let known = OWN_TEXTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    known.iter().find(|(own, _)| own == name).map(|(_, write)| *write)
}

/// Text being written. Native code writes a value whose type has its own text, which can't be
/// done while the runtime's state is in use, so those are noted here, each with the text before
/// it, and done by `finished`.
#[derive(Default)]
pub(in crate::embed) struct Written {
    done: Vec<(String, WriteOwn, i64)>,
    text: String,
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
/// it. Runs what native code has to write, so nothing of the runtime can be in use.
pub(in crate::embed) fn shown(address: u64) -> String {
    let mut out = with(|state| {
        let mut out = Written::default();
        match state.heap.block(address) {
            Some(block) if is_text(state, block) => out.push_str(&block_text(block)),
            _ => quoted(state, address, &mut out),
        }
        out
    });
    let mut text = String::new();
    for (before, write, at) in out.done.drain(..) {
        text.push_str(&before);
        write(at);
        text.push_str(&HANDED_OVER.take());
    }
    text.push_str(&out.text);
    text
}

/// Notes a value of a type that writes its own text, if this is one, with the words it takes
/// from `words`. Says whether it was.
fn own_value((name, size): (&str, usize), words: &mut dyn Iterator<Item = &u64>, out: &mut Written) -> bool {
    let Some(write) = own_writer(name) else { return false };
    let mut mine = words.take(size);
    let at = mine.next().map_or(0, |first| std::ptr::from_ref(first).addr());
    mine.for_each(drop);
    out.done.push((std::mem::take(&mut out.text), write, i64::try_from(at).unwrap_or(0)));
    true
}

/// Writes the value at a block's address as it shows inside a collection: strings are quoted.
pub(in crate::embed) fn quoted(state: &State, address: u64, out: &mut Written) {
    let Some(block) = state.heap.block(address) else {
        super::not_native(address, "print");
        out.push_str("None");
        return;
    };
    if let Some(value) = cell_value(state, block) {
        out.push_str(&crate::format::repr(&state.expanded(value)));
    } else if shape_word(block) == Some(CAPABILITY_SHAPE) {
        out.push_str(&crate::format::repr(&crate::embed::capabilities::read(state, block)));
    } else if shape_word(block) == Some(super::BYTES_SHAPE) {
        let data = crate::embed::bytes::data_of(block);
        out.push_str(&crate::format::repr(&Value::Bytes(Rc::from(data))));
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
        Element::U64 => {
            let _ = write!(out, "{}", word(words));
        }
        Element::F32 => out.push_str(&crate::format::float32(f64::from_bits(word(words)))),
        Element::Bool => out.push_str(if word(words) == 0 { "False" } else { "True" }),
        Element::Boxed(_) | Element::Str | Element::Object | Element::List(_) | Element::ListOf(_) => {
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
        Element::Struct { name, fields } if is_array(name) => {
            out.push('[');
            for (at, (_, element)) in fields.iter().enumerate() {
                if at > 0 {
                    out.push_str(", ");
                }
                item(state, element, words, out);
            }
            out.push(']');
        }
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
        Element::Struct { name, .. } if own_value((name, element.words()), words, out) => {}
        Element::Struct { name, fields } => {
            out.push_str(shown_type_name(type_name(name)));
            named_fields(state, fields, words, out);
        }
        Element::Enum { name, words: size, .. }
            if !is_option(name)
                && !is_result(name)
                && !is_one_of_several(name)
                && own_value((name, *size), words, out) => {}
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
    } else if is_one_of_several(name) {
        if let Some((_, element)) = fields.first() {
            item(state, element, words, out);
        }
    } else if is_result(name) {
        out.push_str(if case == "Err" { "Err(" } else { "Ok(" });
        if let Some((_, element)) = fields.first() {
            item(state, element, words, out);
        }
        out.push(')');
    } else {
        let _ = write!(out, "{}.{case}", shown_type_name(type_name(name)));
        if fields.iter().any(|(field, _)| !crate::packed::hidden(field)) {
            named_fields(state, fields, words, out);
        }
    }
}

/// `(x=1, y=2)`.
fn named_fields(state: &State, fields: &Fields, words: &mut dyn Iterator<Item = &u64>, out: &mut Written) {
    out.push('(');
    let mut first = true;
    for (field, element) in fields {
        // A field the layout keeps for itself, like an error's notes, is passed over unseen.
        if crate::packed::hidden(field) {
            (&mut *words).take(element.words()).for_each(drop);
            continue;
        }
        if !first {
            out.push_str(", ");
        }
        first = false;
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
        // The slot is the key's high 32 bits.
        if field.as_ref() == "key" {
            slot = first >> 32;
        }
    }
    let _ = write!(out, "Id({slot})");
}

/// `Table([a, b])`: what's in the slots still in use.
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
    out.push_str("Table([");
    crate::embed::lists::shown_where_used(state, held, generations, out);
    out.push_str("])");
}
