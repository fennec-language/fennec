//! Lists packed into flat memory: lists of ints, floats or bools, or of structs and enums
//! made only of those, stored one machine word per number, a struct's fields one after
//! another. An enum is its case's number, then that case's fields, then zeros up to the
//! size of its biggest case. Native
//! code reads and changes them in place. To the interpreter they're ordinary lists: it
//! unpacks one whenever it uses it.

use std::rc::Rc;

use crate::value::{Instance, Value};

/// A struct's or case's fields, in the order they're written.
pub type Fields = Vec<(Rc<str>, Element)>;

/// What a packed list's items are.
#[derive(Clone, Debug, PartialEq)]
pub enum Element {
    Int,
    Float,
    Bool,
    /// A struct, with its fields in the order they're written.
    Struct {
        name: Rc<str>,
        fields: Fields,
    },
    /// A boxed value inside another, by the address of its block, with the number of
    /// the block's shape. Packed lists never hold these.
    Boxed(usize),
    /// A string inside a boxed value, by the address of its block. Packed lists never hold
    /// these either.
    Str,
    /// A list inside a boxed value, by the address of its block, with the kind of its items.
    /// Packed lists never hold these either.
    List(Rc<Element>),
    /// An enum, with each case's fields in the order they're written, taking `words`
    /// words.
    Enum {
        name: Rc<str>,
        cases: Vec<(Rc<str>, Fields)>,
        words: usize,
    },
}

impl Element {
    /// How many words one item takes.
    pub fn words(&self) -> usize {
        match self {
            Element::Struct { fields, .. } => fields.iter().map(|(_, field)| field.words()).sum(),
            Element::Enum { words, .. } => *words,
            _ => 1,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Packed {
    pub(crate) element: Rc<Element>,
    pub(crate) words: Vec<u64>,
    /// How many words one item takes, worked out once.
    pub(crate) stride: usize,
}

impl Packed {
    /// A list's items packed, if every one is the kind of item expected.
    pub(crate) fn pack(element: &Rc<Element>, items: &[Value]) -> Option<Packed> {
        let mut words = Vec::with_capacity(items.len() * element.words());
        for item in items {
            pack_item(element, item, &mut words)?;
        }
        Some(Packed::new(Rc::clone(element), words))
    }

    pub(crate) fn new(element: Rc<Element>, words: Vec<u64>) -> Packed {
        let stride = element.words().max(1);
        Packed { element, words, stride }
    }

    /// The list as the interpreter holds lists.
    pub(crate) fn unpack(&self) -> Value {
        let items = self
            .words
            .chunks(self.stride)
            .map(|chunk| unpack_item(&self.element, &mut chunk.iter()))
            .collect();
        Value::list(items)
    }
}

fn pack_item(element: &Element, item: &Value, words: &mut Vec<u64>) -> Option<()> {
    match (element, item) {
        (Element::Int, Value::Int(n)) => words.push(u64::from_ne_bytes(n.to_ne_bytes())),
        (Element::Float, Value::Float(x)) => words.push(x.to_bits()),
        (Element::Bool, Value::Bool(b)) => words.push(u64::from(*b)),
        (Element::Struct { name, fields }, Value::Instance(instance))
            if instance.type_name == *name && instance.case.is_none() && !instance.is_error =>
        {
            for (field, element) in fields {
                pack_item(element, instance.field(field)?, words)?;
            }
        }
        (Element::Enum { name, cases, words: size }, Value::Instance(instance))
            if instance.type_name == *name && !instance.is_error =>
        {
            let case = instance.case.as_ref()?;
            let at = cases.iter().position(|(written, _)| written == case)?;
            let start = words.len();
            words.push(u64::try_from(at).ok()?);
            for (field, element) in &cases.get(at)?.1 {
                pack_item(element, instance.field(field)?, words)?;
            }
            words.resize(start + size, 0);
        }
        _ => return None,
    }
    Some(())
}

fn unpack_item<'w>(element: &Element, words: &mut dyn Iterator<Item = &'w u64>) -> Value {
    let word = |words: &mut dyn Iterator<Item = &'w u64>| words.next().copied().unwrap_or(0);
    match element {
        Element::Int => Value::Int(i64::from_ne_bytes(word(words).to_ne_bytes())),
        Element::Float => Value::Float(f64::from_bits(word(words))),
        Element::Bool => Value::Bool(word(words) != 0),
        Element::Boxed(_) | Element::Str | Element::List(_) => {
            word(words);
            Value::None
        }
        Element::Struct { name, fields } => {
            let fields = fields
                .iter()
                .map(|(field, element)| (Rc::clone(field), unpack_item(element, words)))
                .collect();
            Value::Instance(Rc::new(Instance {
                type_name: Rc::clone(name),
                case: None,
                fields,
                is_error: false,
                cause: None,
            }))
        }
        Element::Enum { name, cases, words: size } => {
            let mut mine = (&mut *words).take(*size);
            let at = usize::try_from(word(&mut mine)).unwrap_or(0);
            let (case, fields) = cases.get(at).or_else(|| cases.first()).cloned().unwrap_or_default();
            let fields = fields
                .iter()
                .map(|(field, element)| (Rc::clone(field), unpack_item(element, &mut mine)))
                .collect();
            let item = Value::Instance(Rc::new(Instance {
                type_name: Rc::clone(name),
                case: Some(case),
                fields,
                is_error: false,
                cause: None,
            }));
            // Skip the words a smaller case leaves zero.
            mine.for_each(drop);
            item
        }
    }
}
