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
/// are empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cap {
    Stdout,
    Args,
    Stdin,
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
        }
    }

    pub(crate) fn instance(ty: &str, case: Option<&str>, fields: Vec<(String, Value)>) -> Value {
        Value::Instance(Box::new(Instance { ty: ty.to_owned(), case: case.map(str::to_owned), fields }))
    }
}
