//! Lists packed into flat memory: lists of ints, floats or bools, or of structs made only of
//! those, stored one machine word per number, a struct's fields one after another. Native
//! code reads and changes them in place. To the interpreter they're ordinary lists: it
//! unpacks one whenever it uses it.

use std::rc::Rc;

use crate::value::{Instance, Value};

/// What a packed list's items are.
#[derive(Clone, Debug, PartialEq)]
pub enum Element {
    Int,
    Float,
    Bool,
    /// A struct, with its fields in the order they're written.
    Struct {
        name: Rc<str>,
        fields: Vec<(Rc<str>, Element)>,
    },
}

impl Element {
    /// How many words one item takes.
    pub fn words(&self) -> usize {
        match self {
            Element::Struct { fields, .. } => fields.iter().map(|(_, field)| field.words()).sum(),
            _ => 1,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Packed {
    pub(crate) element: Rc<Element>,
    pub(crate) words: Vec<u64>,
}

impl Packed {
    /// A list's items packed, if every one is the kind of item expected.
    pub(crate) fn pack(element: &Rc<Element>, items: &[Value]) -> Option<Packed> {
        let mut words = Vec::with_capacity(items.len() * element.words());
        for item in items {
            pack_item(element, item, &mut words)?;
        }
        Some(Packed { element: Rc::clone(element), words })
    }

    /// The list as the interpreter holds lists.
    pub(crate) fn unpack(&self) -> Value {
        let stride = self.element.words().max(1);
        let items =
            self.words.chunks(stride).map(|chunk| unpack_item(&self.element, &mut chunk.iter())).collect();
        Value::list(items)
    }

    /// How many items it has.
    pub(crate) fn len(&self) -> usize {
        self.words.len() / self.element.words().max(1)
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
        _ => return None,
    }
    Some(())
}

fn unpack_item<'w>(element: &Element, words: &mut impl Iterator<Item = &'w u64>) -> Value {
    let word = |words: &mut dyn Iterator<Item = &'w u64>| words.next().copied().unwrap_or(0);
    match element {
        Element::Int => Value::Int(i64::from_ne_bytes(word(words).to_ne_bytes())),
        Element::Float => Value::Float(f64::from_bits(word(words))),
        Element::Bool => Value::Bool(word(words) != 0),
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
    }
}
