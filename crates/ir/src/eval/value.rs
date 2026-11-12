//! The values constants are worked out with while compiling. Every value is a copy, as in the
//! IR: changing a list or a struct inside a local changes that local's own copy, through
//! `Rc::make_mut`, so what another local holds never changes with it.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

/// A value a constant's code makes.
#[derive(Clone, Debug)]
pub enum Value {
    /// What a function gives back when it gives nothing, and what a local holds before it's set.
    Unit,
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Rc<str>),
    Bytes(Rc<[u8]>),
    List(Rc<Vec<Value>>),
    Tuple(Rc<Vec<Value>>),
    /// A set keeps the order its items were added in, as a dict does.
    Set(Rc<Ordered>),
    Dict(Rc<Ordered>),
    /// A struct, an error, or an enum's case, with its fields in the order they're written.
    Instance(Rc<Instance>),
    Ok(Rc<Value>),
    Err(Rc<Value>),
    /// A union's member, by its place in the union's types.
    Member(usize, Rc<Value>),
    /// A closure: the function, and the values it captured, which come first when it's called.
    Closure(Rc<str>, Rc<Vec<Value>>),
}

/// A struct, an error or an enum case.
#[derive(Clone, Debug)]
pub struct Instance {
    pub type_name: Rc<str>,
    /// The case, for an enum's value.
    pub case: Option<Rc<str>>,
    pub fields: Vec<Value>,
}

/// A dict's pairs, or a set's items with nothing paired, in the order they were added, with an
/// index to find a key's place.
#[derive(Clone, Debug, Default)]
pub struct Ordered {
    pub pairs: Vec<(Value, Value)>,
    places: HashMap<Value, usize>,
}

impl Ordered {
    /// The value paired with a key.
    pub fn get(&self, key: &Value) -> Option<&Value> {
        let at = *self.places.get(key)?;
        self.pairs.get(at).map(|(_, value)| value)
    }

    pub fn contains(&self, key: &Value) -> bool {
        self.places.contains_key(key)
    }

    /// Pairs a value with a key: a new key goes last, and one already there keeps its place.
    pub fn insert(&mut self, key: Value, value: Value) {
        if let Some(&at) = self.places.get(&key) {
            if let Some(pair) = self.pairs.get_mut(at) {
                pair.1 = value;
            }
            return;
        }
        self.places.insert(key.clone(), self.pairs.len());
        self.pairs.push((key, value));
    }

    /// Takes a key out, and what was paired with it.
    pub fn remove(&mut self, key: &Value) -> Option<Value> {
        let at = self.places.remove(key)?;
        if at >= self.pairs.len() {
            return None;
        }
        let (_, value) = self.pairs.remove(at);
        #[expect(
            clippy::iter_over_hash_type,
            reason = "each place past the removed one moves down by one, in any order"
        )]
        for place in self.places.values_mut() {
            if *place > at {
                *place -= 1;
            }
        }
        Some(value)
    }

    pub fn len(&self) -> usize {
        self.pairs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    pub fn keys(&self) -> impl Iterator<Item = &Value> {
        self.pairs.iter().map(|(key, _)| key)
    }
}

impl FromIterator<(Value, Value)> for Ordered {
    fn from_iter<I: IntoIterator<Item = (Value, Value)>>(pairs: I) -> Self {
        let mut ordered = Ordered::default();
        for (key, value) in pairs {
            ordered.insert(key, value);
        }
        ordered
    }
}

impl Value {
    pub fn str(text: &str) -> Value {
        Value::Str(Rc::from(text))
    }

    pub fn list(items: Vec<Value>) -> Value {
        Value::List(Rc::new(items))
    }

    pub fn set(items: impl IntoIterator<Item = Value>) -> Value {
        Value::Set(Rc::new(items.into_iter().map(|item| (item, Value::Unit)).collect()))
    }

    /// How many items or pairs it holds, or characters for a str.
    pub fn count(&self) -> Option<usize> {
        match self {
            Value::Str(text) => Some(text.chars().count()),
            Value::Bytes(data) => Some(data.len()),
            Value::List(items) | Value::Tuple(items) => Some(items.len()),
            Value::Set(items) | Value::Dict(items) => Some(items.len()),
            _ => None,
        }
    }

    /// Its items, for what holds items in order: a list, a tuple, a set's items or a dict's keys.
    pub fn items(&self) -> Option<Vec<Value>> {
        match self {
            Value::List(items) | Value::Tuple(items) => Some(items.as_ref().clone()),
            Value::Set(items) | Value::Dict(items) => Some(items.keys().cloned().collect()),
            _ => None,
        }
    }
}

/// A float's bits as it hashes and keys: `0.0` and `-0.0` are one key, and every NaN is one.
fn float_key(x: f64) -> u64 {
    if x == 0.0 {
        0
    } else if x.is_nan() {
        f64::NAN.to_bits()
    } else {
        x.to_bits()
    }
}

/// Values are the same as Fennec's `==` has it: `0.0` is `-0.0`, and NaN is NaN.
impl PartialEq for Value {
    fn eq(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Unit, Value::Unit) | (Value::None, Value::None) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => float_key(*a) == float_key(*b),
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Bytes(a), Value::Bytes(b)) => a == b,
            (Value::List(a), Value::List(b)) | (Value::Tuple(a), Value::Tuple(b)) => a == b,
            (Value::Set(a), Value::Set(b)) => a.len() == b.len() && a.keys().all(|key| b.contains(key)),
            (Value::Dict(a), Value::Dict(b)) => {
                a.len() == b.len() && a.pairs.iter().all(|(key, value)| b.get(key) == Some(value))
            }
            (Value::Instance(a), Value::Instance(b)) => {
                a.type_name == b.type_name && a.case == b.case && a.fields == b.fields
            }
            (Value::Ok(a), Value::Ok(b)) | (Value::Err(a), Value::Err(b)) => a == b,
            (Value::Member(a, x), Value::Member(b, y)) => a == b && x == y,
            (Value::Closure(a, x), Value::Closure(b, y)) => a == b && Rc::ptr_eq(x, y),
            _ => false,
        }
    }
}

impl Eq for Value {}

impl Hash for Value {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Value::Unit | Value::None | Value::Closure(..) => {}
            Value::Bool(value) => value.hash(state),
            Value::Int(value) => value.hash(state),
            Value::Float(value) => float_key(*value).hash(state),
            Value::Str(text) => text.hash(state),
            Value::Bytes(data) => data.hash(state),
            Value::List(items) | Value::Tuple(items) => items.hash(state),
            // Sets and dicts that are equal can hold their items in different orders.
            Value::Set(items) | Value::Dict(items) => items.len().hash(state),
            Value::Instance(instance) => {
                instance.type_name.hash(state);
                instance.case.hash(state);
                instance.fields.hash(state);
            }
            Value::Ok(value) | Value::Err(value) => value.hash(state),
            Value::Member(at, value) => {
                at.hash(state);
                value.hash(state);
            }
        }
    }
}
