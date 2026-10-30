//! Runtime values. Lists and strings are reference counted and copied when changed while
//! shared, which is how Fennec's value semantics stay cheap.

use std::rc::Rc;

use fennec_syntax::Expr;

#[derive(Clone, Debug)]
pub(crate) enum Value {
    /// What a function returns when it has nothing to return.
    Unit,
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Rc<str>),
    List(Rc<Vec<Value>>),
    Function(Rc<str>),
    Builtin(&'static str),
    Lambda(Rc<Lambda>),
    Stdout,
}

/// An arrow function, with the names it could see when it was created.
#[derive(Debug)]
pub(crate) struct Lambda {
    pub(crate) params: Vec<String>,
    pub(crate) body: Expr,
    pub(crate) captured: Vec<(String, Value)>,
}

impl Value {
    pub(crate) fn str(text: &str) -> Value {
        Value::Str(Rc::from(text))
    }

    pub(crate) fn list(items: Vec<Value>) -> Value {
        Value::List(Rc::new(items))
    }

    pub(crate) fn type_name(&self) -> &'static str {
        match self {
            Value::Unit => "nothing",
            Value::None => "None",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Str(_) => "str",
            Value::List(_) => "list",
            Value::Function(_) | Value::Builtin(_) | Value::Lambda(_) => "function",
            Value::Stdout => "Stdout",
        }
    }
}

impl PartialEq for Value {
    #[expect(clippy::match_same_arms, reason = "each arm compares a different type")]
    fn eq(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Unit, Value::Unit) | (Value::None, Value::None) | (Value::Stdout, Value::Stdout) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a.total_cmp(b).is_eq(),
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::List(a), Value::List(b)) => a == b,
            (Value::Function(a), Value::Function(b)) => a == b,
            (Value::Builtin(a), Value::Builtin(b)) => a == b,
            (Value::Lambda(a), Value::Lambda(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }
}
