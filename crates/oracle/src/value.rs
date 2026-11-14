//! The values a program works with while the oracle runs it: plain Rust values, each one its own.
//! Every assignment, argument and return clones one, so a name always holds its own value.

use std::sync::{Arc, Mutex};

use fennec_check::Num;
use fennec_syntax::{Expr, FnDef};

use crate::interp::stream::Source;

/// A value while the program runs.
#[derive(Clone, Debug)]
pub(crate) enum Value {
    /// What a function that gives back nothing gives.
    Unit,
    None,
    Bool(bool),
    /// An `int`, which is an `i64`.
    Int(i64),
    /// An integer of another size, `i8` to `u64`, held wide enough for every one of them.
    Sized(Num, i128),
    /// A `float`, which is an `f64`.
    Float(f64),
    Str(String),
    List(Vec<Value>),
    Tuple(Vec<Value>),
    /// A set's items, in the order they went in.
    Set(Vec<Value>),
    /// A dict's pairs, in the order their keys went in.
    Dict(Vec<(Value, Value)>),
    /// A struct, an error, or an enum's case.
    Instance(Box<Instance>),
    /// A function, as a value.
    Func(Box<Func>),
    /// Items one at a time: a generator, or `enumerate` or `zip` over one.
    Stream(Stream),
    /// A capability `main` was given.
    Cap(Cap),
    /// A `Table`, its slots and its tag.
    Table(Box<Table>),
    /// An `Id`: where its item is in a `Table`, and which one.
    Id(Id),
    /// A value the oracle can't know, as a folder's size, which differs from one system to
    /// another: it can be held and passed on, and anything else done with it is refused, as this
    /// says why.
    Unknown(String),
}

/// A struct, an error, or a case of an enum or an error, with its fields in the order the type
/// writes them.
#[derive(Clone, Debug)]
pub(crate) struct Instance {
    /// The type's name, as the program writes it.
    pub(crate) ty: String,
    /// The case, for an enum or an error with cases.
    pub(crate) case: Option<String>,
    pub(crate) fields: Vec<(String, Value)>,
}

impl Instance {
    pub(crate) fn field(&self, name: &str) -> Option<&Value> {
        self.fields.iter().find(|(field, _)| field == name).map(|(_, value)| value)
    }

    pub(crate) fn field_mut(&mut self, name: &str) -> Option<&mut Value> {
        self.fields.iter_mut().find(|(field, _)| field == name).map(|(_, value)| value)
    }
}

/// A function held as a value.
#[derive(Clone, Debug)]
pub(crate) enum Func {
    /// A top-level function, by its name.
    Named(String),
    /// A builtin, like `len` or `str`, passed as a value.
    Builtin(String),
    /// A function written inside another, which reads the names of the call it was written in
    /// while that call is still running.
    Nested { def: &'static FnDef, frame: u64 },
    /// An arrow function, with the names it reads copied when it was made.
    Lambda { params: Vec<String>, body: &'static Expr, captured: Vec<(String, Value)> },
}

/// A capability `main` was given. Standard output is real; the arguments and standard input
/// are empty; the file system is the one `interp/disk.rs` models.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Cap {
    Stdout,
    Args,
    Stdin,
    Fs(Reach),
}

/// What an `Fs` reaches: everything, as the one `main` is given does, or less, once `only` or
/// `read_only` has narrowed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reach {
    /// The folders it reaches, each as the parts of its absolute path, or `None` for everywhere.
    pub(crate) folders: Option<Vec<Vec<String>>>,
    /// Whether it only reads.
    pub(crate) read_only: bool,
}

/// Items made one at a time, shared by every copy of the value: a generator is used once, so a
/// copy reads on from where the other is.
#[derive(Clone, Debug)]
pub(crate) struct Stream {
    pub(crate) source: Arc<Mutex<Source>>,
    /// The type it prints as, like `Iterator[int]`, when that's known.
    pub(crate) shown: Option<String>,
}

impl Value {
    /// The name of the value's kind, for messages.
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Value::Unit => "nothing",
            Value::None => "None",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Sized(..) => "sized number",
            Value::Float(_) => "float",
            Value::Str(_) => "str",
            Value::List(_) => "list",
            Value::Tuple(_) => "tuple",
            Value::Set(_) => "set",
            Value::Dict(_) => "dict",
            Value::Instance(_) => "struct",
            Value::Func(_) => "function",
            Value::Stream(_) => "generator",
            Value::Cap(_) => "capability",
            Value::Table(_) => "Table",
            Value::Id(_) => "Id",
            Value::Unknown(_) => "value the oracle can't know",
        }
    }

    pub(crate) fn instance(ty: &str, case: Option<&str>, fields: Vec<(String, Value)>) -> Value {
        Value::Instance(Box::new(Instance { ty: ty.to_owned(), case: case.map(str::to_owned), fields }))
    }
}

/// A `Table`: a slot for each item it's been given, in the order they went in, a slot whose item
/// was removed given to the next item added, the lowest such slot first. Each slot counts how
/// many items it's held, so an `Id` of one that's gone finds nothing.
#[derive(Clone, Debug)]
pub(crate) struct Table {
    pub(crate) slots: Vec<Slot>,
    /// Which `Table` it is, so an `Id` from another finds nothing in it. A copy keeps it.
    pub(crate) tag: u64,
}

/// One slot of a `Table`: the generation of the item it holds, or of the last it held, and the
/// item, while it has one.
#[derive(Clone, Debug)]
pub(crate) struct Slot {
    pub(crate) generation: u32,
    pub(crate) item: Option<Value>,
}

/// An `Id`: a slot, the generation of the item it was given for, and the `Table`'s tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Id {
    pub(crate) slot: usize,
    pub(crate) generation: u32,
    pub(crate) tag: u64,
}

impl Table {
    pub(crate) fn new(tag: u64) -> Table {
        Table { slots: Vec::new(), tag }
    }

    /// Where the item an `Id` names is, while it's there.
    pub(crate) fn find(&self, id: Id) -> Option<usize> {
        let slot = self.slots.get(id.slot).filter(|_| id.tag == self.tag)?;
        (slot.generation == id.generation && slot.item.is_some()).then_some(id.slot)
    }

    /// What stops a program that reaches into the `Table` with an `Id` that finds nothing.
    pub(crate) fn missing(&self, id: Id) -> &'static str {
        if id.tag == self.tag {
            "this Id refers to something that's been removed"
        } else {
            "this Id is from another Table"
        }
    }

    /// Adds an item, in the lowest slot whose item was removed, at the next generation, or in a
    /// new slot at generation 0. A slot that's held as many items as an `Id` can count is never
    /// used again.
    pub(crate) fn add(&mut self, item: Value) -> Id {
        let tag = self.tag;
        let free = self
            .slots
            .iter_mut()
            .enumerate()
            .find(|(_, slot)| slot.item.is_none() && slot.generation < u32::MAX);
        if let Some((at, slot)) = free {
            slot.generation += 1;
            slot.item = Some(item);
            return Id { slot: at, generation: slot.generation, tag };
        }
        self.slots.push(Slot { generation: 0, item: Some(item) });
        Id { slot: self.slots.len() - 1, generation: 0, tag }
    }

    /// Takes out the item an `Id` names, if it's there.
    pub(crate) fn remove(&mut self, id: Id) -> Option<Value> {
        let at = self.find(id)?;
        self.slots.get_mut(at).and_then(|slot| slot.item.take())
    }

    /// Each item there, with its `Id`, in slot order.
    pub(crate) fn live(&self) -> impl Iterator<Item = (Id, &Value)> {
        self.slots.iter().enumerate().filter_map(|(at, slot)| {
            let id = Id { slot: at, generation: slot.generation, tag: self.tag };
            slot.item.as_ref().map(|item| (id, item))
        })
    }

    pub(crate) fn len(&self) -> usize {
        self.slots.iter().filter(|slot| slot.item.is_some()).count()
    }
}
