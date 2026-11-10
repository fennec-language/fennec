//! Runtime values. Collections, strings and struct values are reference counted and copied
//! only when changed while shared, which is how Fennec's value semantics stay cheap.

use std::rc::Rc;

#[derive(Clone, Debug)]
pub(crate) enum Value {
    /// What a function returns when it has nothing to return.
    Unit,
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Rc<str>),
    /// Binary data, each byte from 0 to 255.
    Bytes(Rc<[u8]>),
    List(Rc<Vec<Value>>),
    /// A list native code packed into flat memory. The runtime unpacks it to use it.
    Packed(Rc<crate::packed::Packed>),
    /// A native value, not yet looked inside, as native code's temporaries and cells hold it.
    /// The runtime expands a temporary before it uses one, so it never looks inside this.
    Lent(Rc<Lent>),
    Tuple(Rc<Vec<Value>>),
    /// A dict keeps insertion order.
    Dict(Rc<crate::pairs::Pairs>),
    Set(Rc<Vec<Value>>),
    /// A struct, an error, or an enum case.
    Instance(Rc<Instance>),
    Ok(Rc<Value>),
    Err(Rc<Value>),
    Table(Rc<Table>),
    Id(Id),
    Capability(Capability),
    /// A capability narrowed with `only`, which reaches less than the whole capability.
    Limited(Rc<crate::limits::Limited>),
}

/// One owner of a block native code holds, let go when the last copy of this is dropped.
#[derive(Debug)]
pub(crate) struct Lent {
    pub(crate) address: u64,
}

thread_local! {
    /// Blocks whose `Lent` was dropped, for the runtime to let go of when it's next ready.
    static LET_GO: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
}

impl Drop for Lent {
    fn drop(&mut self) {
        // Dropping can happen while the runtime is busy, so this only makes a note of it.
        let address = self.address;
        let _ = LET_GO.try_with(|let_go| let_go.borrow_mut().push(address));
    }
}

/// The blocks to let go of, leaving none.
pub(crate) fn blocks_let_go() -> Vec<u64> {
    LET_GO.try_with(std::cell::RefCell::take).unwrap_or_default()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Capability {
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

#[derive(Clone, Debug)]
pub(crate) struct Instance {
    pub(crate) type_name: Rc<str>,
    /// The case, for an enum value.
    pub(crate) case: Option<Rc<str>>,
    pub(crate) fields: Vec<(Rc<str>, Value)>,
    pub(crate) is_error: bool,
    /// The error this one was raised `from`, if any.
    pub(crate) cause: Option<Value>,
    /// An error's notes, last added last, which print under it when it reaches `main`. Kept
    /// apart from its fields, so `==`, printing, `match` and saving never see them. `None`
    /// until a note is added.
    pub(crate) notes: Option<Rc<Vec<Rc<str>>>>,
}

impl Instance {
    fn of(type_name: &str, fields: Vec<(&str, Value)>, is_error: bool) -> Instance {
        Instance {
            type_name: Rc::from(type_name),
            case: None,
            fields: fields.into_iter().map(|(name, value)| (Rc::from(name), value)).collect(),
            is_error,
            cause: None,
            notes: None,
        }
    }
}

impl Instance {
    pub(crate) fn field(&self, name: &str) -> Option<&Value> {
        self.fields.iter().find(|(field, _)| field.as_ref() == name).map(|(_, value)| value)
    }
}

/// Slots that hand out generational IDs, so a stale ID never finds the wrong thing.
#[derive(Clone, Debug)]
pub(crate) struct Table {
    pub(crate) slots: Vec<Slot>,
    /// Which `Table` this is, so an `Id` from another finds nothing in it. A copy keeps it, since
    /// it holds the same items in the same slots.
    pub(crate) tag: u64,
}

/// The next `Table`'s tag. Tags count up from 1 for the whole run and are never used again; a
/// 64-bit count can't run out.
static NEXT_TAG: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// A tag no `Table` has had.
pub(crate) fn new_tag() -> u64 {
    NEXT_TAG.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl Default for Table {
    fn default() -> Table {
        Table::new(Vec::new())
    }
}

/// Maps are the same when their slots are: where one's looking for a free slot is no part of it.
impl PartialEq for Table {
    fn eq(&self, other: &Table) -> bool {
        self.slots == other.slots
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Slot {
    pub(crate) generation: u32,
    pub(crate) value: Option<Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Id {
    /// A `u32`, so an `Id` with its tag is as big as a `Value`'s other contents and no bigger.
    pub(crate) slot: u32,
    pub(crate) generation: u32,
    /// The tag of the `Table` that handed it out.
    pub(crate) tag: u64,
}

// An `Id` stays as big as a `str`'s handle, so holding one doesn't make every `Value` bigger.
const _: () = assert!(size_of::<Id>() == 16);

impl Id {
    pub(crate) fn new(slot: usize, generation: u32, tag: u64) -> Id {
        Id { slot: u32::try_from(slot).unwrap_or(u32::MAX), generation, tag }
    }

    /// Where its slot is.
    pub(crate) fn at(self) -> usize {
        self.slot as usize
    }

    /// Its slot and generation in one number, as native code holds them: the slot in the high 32
    /// bits, and the generation in the low 32.
    pub(crate) fn key(self) -> u64 {
        (u64::from(self.slot) << 32) | u64::from(self.generation)
    }

    pub(crate) fn from_key(key: u64, tag: u64) -> Id {
        let slot = u32::try_from(key >> 32).unwrap_or(u32::MAX);
        let generation = u32::try_from(key & u64::from(u32::MAX)).unwrap_or(u32::MAX);
        Id { slot, generation, tag }
    }
}

impl Table {
    /// A `Table` with a tag of its own.
    pub(crate) fn new(slots: Vec<Slot>) -> Table {
        Table::tagged(slots, new_tag())
    }

    pub(crate) fn tagged(slots: Vec<Slot>, tag: u64) -> Table {
        Table { slots, tag }
    }

    pub(crate) fn get(&self, id: Id) -> Option<&Value> {
        if id.tag != self.tag {
            return None;
        }
        self.slots
            .get(id.at())
            .filter(|slot| slot.generation == id.generation)
            .and_then(|slot| slot.value.as_ref())
    }

    /// Live entries, in slot order.
    pub(crate) fn entries(&self) -> impl Iterator<Item = (Id, &Value)> {
        self.slots.iter().enumerate().filter_map(|(at, slot)| {
            slot.value.as_ref().map(|value| (Id::new(at, slot.generation, self.tag), value))
        })
    }
}

/// How two floats order: as numbers, with `0.0` the same as `-0.0`, and NaN after everything and
/// the same as any other NaN, so its sign can't change a result.
pub(crate) fn float_order(a: f64, b: f64) -> std::cmp::Ordering {
    match (a.is_nan(), b.is_nan()) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        (false, false) => a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal),
    }
}

/// Whether two floats are the same: equal as numbers, with `0.0` the same as `-0.0`, or both NaN.
#[expect(clippy::float_cmp, reason = "this is what equal means for a float in Fennec")]
pub(crate) fn floats_same(a: f64, b: f64) -> bool {
    a == b || (a.is_nan() && b.is_nan())
}

/// A float's bits as they hash and sort: the one form of each float that's equal to others.
pub(crate) fn float_key(x: f64) -> u64 {
    if x == 0.0 {
        0
    } else if x.is_nan() {
        f64::NAN.to_bits()
    } else {
        x.to_bits()
    }
}

impl Value {
    /// A struct of the standard library, with its fields in the order they're declared.
    pub(crate) fn made(type_name: &str, fields: Vec<(&str, Value)>) -> Value {
        Value::Instance(Rc::new(Instance::of(type_name, fields, false)))
    }

    /// An error of the standard library, with its fields in the order they're declared.
    pub(crate) fn made_error(type_name: &str, fields: Vec<(&str, Value)>) -> Value {
        Value::Instance(Rc::new(Instance::of(type_name, fields, true)))
    }

    /// A case of an error of the standard library, with its fields in the order they're declared.
    pub(crate) fn made_case(type_name: &str, case: &str, fields: Vec<(&str, Value)>) -> Value {
        let mut made = Instance::of(type_name, fields, true);
        made.case = Some(Rc::from(case));
        Value::Instance(Rc::new(made))
    }

    pub(crate) fn str(text: &str) -> Value {
        Value::Str(Rc::from(text))
    }

    pub(crate) fn list(items: Vec<Value>) -> Value {
        Value::List(Rc::new(items))
    }

    pub(crate) fn tuple(items: Vec<Value>) -> Value {
        Value::Tuple(Rc::new(items))
    }

    /// The value as the runtime uses it: a packed list unpacked, anything else as it is.
    pub(crate) fn unpacked(self) -> Value {
        match self {
            Value::Packed(packed) => packed.unpack(),
            other => other,
        }
    }

    pub(crate) fn is_failure(&self) -> bool {
        matches!(self, Value::Err(_))
    }

    pub(crate) fn type_name(&self) -> String {
        match self {
            Value::Unit => "nothing".to_owned(),
            Value::None => "None".to_owned(),
            Value::Bool(_) => "bool".to_owned(),
            Value::Int(_) => "int".to_owned(),
            Value::Float(_) => "float".to_owned(),
            Value::Str(_) => "str".to_owned(),
            Value::Bytes(_) => "bytes".to_owned(),
            Value::List(_) | Value::Packed(_) => "list".to_owned(),
            Value::Lent(_) => "value".to_owned(),
            Value::Tuple(_) => "tuple".to_owned(),
            Value::Dict(_) => "dict".to_owned(),
            Value::Set(_) => "set".to_owned(),
            Value::Instance(instance) => instance.type_name.to_string(),
            Value::Ok(_) | Value::Err(_) => "result".to_owned(),
            Value::Table(_) => "Table".to_owned(),
            Value::Id(_) => "Id".to_owned(),
            Value::Capability(Capability::Stdout) => "Stdout".to_owned(),
            Value::Capability(Capability::Fs) => "Fs".to_owned(),
            Value::Capability(Capability::Clock) => "Clock".to_owned(),
            Value::Capability(Capability::Args) => "Args".to_owned(),
            Value::Capability(Capability::Stdin) => "Stdin".to_owned(),
            Value::Capability(Capability::Process) => "Process".to_owned(),
            Value::Capability(Capability::Entropy) => "Entropy".to_owned(),
            Value::Capability(Capability::Net) => "Net".to_owned(),
            Value::Capability(Capability::Env) => "Env".to_owned(),
            Value::Capability(Capability::Signals) => "Signals".to_owned(),
            Value::Capability(Capability::Stderr) => "Stderr".to_owned(),
            Value::Limited(limited) => Value::Capability(limited.capability).type_name(),
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Unit, Value::Unit) | (Value::None, Value::None) => true,
            (Value::Lent(a), Value::Lent(b)) => a.address == b.address,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => floats_same(*a, *b),
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Bytes(a), Value::Bytes(b)) => a == b,
            (Value::List(a), Value::List(b)) | (Value::Tuple(a), Value::Tuple(b)) => a == b,
            (Value::Set(a), Value::Set(b)) => a.len() == b.len() && a.iter().all(|item| b.contains(item)),
            (Value::Dict(a), Value::Dict(b)) => {
                a.len() == b.len() && a.iter().all(|(key, value)| b.get(key) == Some(value))
            }
            (Value::Instance(a), Value::Instance(b)) => {
                a.type_name == b.type_name && a.case == b.case && a.fields == b.fields
            }
            (Value::Ok(a), Value::Ok(b)) | (Value::Err(a), Value::Err(b)) => a == b,
            (Value::Table(a), Value::Table(b)) => a == b,
            (Value::Id(a), Value::Id(b)) => a == b,
            (Value::Capability(a), Value::Capability(b)) => a == b,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Id;

    /// An `Id`'s key packs its slot above its generation, and comes back the same.
    #[test]
    fn an_id_goes_through_its_key_unchanged() {
        let id = Id { slot: 7, generation: u32::MAX, tag: 3 };
        assert_eq!(id.key(), (7 << 32) | u64::from(u32::MAX));
        assert_eq!(Id::from_key(id.key(), 3), id);
    }
}
