//! Runtime values. Collections, strings and struct values are reference counted and copied
//! only when changed while shared, which is how Fennec's value semantics stay cheap.

use std::rc::Rc;

use fennec_syntax::{Expr, FnDef};

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
    /// A list native code packed into flat memory. The interpreter unpacks it to use it.
    Packed(Rc<crate::packed::Packed>),
    /// A generator: items one at a time, as they're asked for, once.
    Generator(Rc<crate::generators::Generator>),
    /// A native value, not yet looked inside, as native code's temporaries and cells hold it.
    /// The interpreter never sees one: temporaries are expanded before it's handed them.
    Lent(Rc<Lent>),
    Tuple(Rc<Vec<Value>>),
    /// A dict keeps insertion order.
    Dict(Rc<crate::pairs::Pairs>),
    Set(Rc<Vec<Value>>),
    /// A struct, an error, or an enum case.
    Instance(Rc<Instance>),
    Ok(Rc<Value>),
    Err(Rc<Value>),
    IdMap(Rc<IdMap>),
    Id(Id),
    /// A type used as a value, as in `Enemy.at(...)` or `IdMap[Enemy]()`.
    Type(Rc<str>),
    Function(Rc<Callable>),
    Builtin(&'static str),
    Lambda(Rc<Lambda>),
    Capability(Capability),
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
}

impl Instance {
    pub(crate) fn field(&self, name: &str) -> Option<&Value> {
        self.fields.iter().find(|(field, _)| field.as_ref() == name).map(|(_, value)| value)
    }

    pub(crate) fn field_mut(&mut self, name: &str) -> Option<&mut Value> {
        self.fields.iter_mut().find(|(field, _)| field.as_ref() == name).map(|(_, value)| value)
    }
}

/// A function: top-level, nested (with the names it captured), or a method.
#[derive(Debug)]
pub(crate) struct Callable {
    pub(crate) def: Rc<FnDef>,
    pub(crate) captured: Vec<(String, Value)>,
    /// Whether it can fail: its body contains `fail` or `?`.
    pub(crate) fallible: bool,
    /// Whether it's a generator: its body contains `yield`.
    pub(crate) generator: bool,
}

/// An arrow function, with the names it could see when it was created.
#[derive(Debug)]
pub(crate) struct Lambda {
    pub(crate) params: Vec<String>,
    pub(crate) body: Expr,
    pub(crate) captured: Vec<(String, Value)>,
}

/// Slots that hand out generational IDs, so a stale ID never finds the wrong thing.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct IdMap {
    pub(crate) slots: Vec<Slot>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Slot {
    pub(crate) generation: u32,
    pub(crate) value: Option<Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Id {
    pub(crate) slot: usize,
    pub(crate) generation: u32,
}

impl IdMap {
    pub(crate) fn get(&self, id: Id) -> Option<&Value> {
        self.slots
            .get(id.slot)
            .filter(|slot| slot.generation == id.generation)
            .and_then(|slot| slot.value.as_ref())
    }

    pub(crate) fn get_mut(&mut self, id: Id) -> Option<&mut Value> {
        self.slots
            .get_mut(id.slot)
            .filter(|slot| slot.generation == id.generation)
            .and_then(|slot| slot.value.as_mut())
    }

    pub(crate) fn insert(&mut self, value: Value) -> Id {
        if let Some((at, slot)) = self.slots.iter_mut().enumerate().find(|(_, slot)| slot.value.is_none()) {
            slot.generation += 1;
            slot.value = Some(value);
            return Id { slot: at, generation: slot.generation };
        }
        self.slots.push(Slot { generation: 0, value: Some(value) });
        Id { slot: self.slots.len() - 1, generation: 0 }
    }

    pub(crate) fn remove(&mut self, id: Id) -> Option<Value> {
        self.slots
            .get_mut(id.slot)
            .filter(|slot| slot.generation == id.generation)
            .and_then(|slot| slot.value.take())
    }

    /// Live entries, in slot order.
    pub(crate) fn entries(&self) -> impl Iterator<Item = (Id, &Value)> {
        self.slots.iter().enumerate().filter_map(|(at, slot)| {
            slot.value.as_ref().map(|value| (Id { slot: at, generation: slot.generation }, value))
        })
    }
}

impl Value {
    pub(crate) fn str(text: &str) -> Value {
        Value::Str(Rc::from(text))
    }

    pub(crate) fn list(items: Vec<Value>) -> Value {
        Value::List(Rc::new(items))
    }

    pub(crate) fn tuple(items: Vec<Value>) -> Value {
        Value::Tuple(Rc::new(items))
    }

    /// The value as the interpreter uses it: a packed list unpacked, anything else as it is.
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
            Value::Generator(_) => "generator".to_owned(),
            Value::Tuple(_) => "tuple".to_owned(),
            Value::Dict(_) => "dict".to_owned(),
            Value::Set(_) => "set".to_owned(),
            Value::Instance(instance) => instance.type_name.to_string(),
            Value::Ok(_) | Value::Err(_) => "result".to_owned(),
            Value::IdMap(_) => "IdMap".to_owned(),
            Value::Id(_) => "Id".to_owned(),
            Value::Type(_) => "type".to_owned(),
            Value::Function(_) | Value::Builtin(_) | Value::Lambda(_) => "function".to_owned(),
            Value::Capability(Capability::Stdout) => "Stdout".to_owned(),
            Value::Capability(Capability::Fs) => "Fs".to_owned(),
            Value::Capability(Capability::Clock) => "Clock".to_owned(),
            Value::Capability(Capability::Args) => "Args".to_owned(),
            Value::Capability(Capability::Stdin) => "Stdin".to_owned(),
            Value::Capability(Capability::Process) => "Process".to_owned(),
        }
    }
}

impl PartialEq for Value {
    #[expect(clippy::match_same_arms, reason = "each arm compares a different type")]
    fn eq(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Unit, Value::Unit) | (Value::None, Value::None) => true,
            (Value::Lent(a), Value::Lent(b)) => a.address == b.address,
            (Value::Generator(a), Value::Generator(b)) => Rc::ptr_eq(a, b),
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a.total_cmp(b).is_eq(),
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
            (Value::IdMap(a), Value::IdMap(b)) => a == b,
            (Value::Id(a), Value::Id(b)) => a == b,
            (Value::Type(a), Value::Type(b)) => a == b,
            (Value::Function(a), Value::Function(b)) => Rc::ptr_eq(a, b),
            (Value::Builtin(a), Value::Builtin(b)) => a == b,
            (Value::Lambda(a), Value::Lambda(b)) => Rc::ptr_eq(a, b),
            (Value::Capability(a), Value::Capability(b)) => a == b,
            _ => false,
        }
    }
}
