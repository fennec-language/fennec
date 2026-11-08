//! The methods of the `Env` capability: the environment variables the program was started with.
//! In a test, `Env` is a fake holding only what the test set, so a test never depends on the
//! machine it runs on. With `fennec test --real` it's the real environment, which `set` can't
//! change.

use std::rc::Rc;


use crate::pairs::Pairs;
use crate::value::Value;

/// The environment variable `name`, or `None` when it isn't set. One that isn't UTF-8 is read
/// with each bad sequence as U+FFFD.
pub(crate) fn variable(name: &str) -> Value {
    std::env::var_os(name).map_or(Value::None, |value| Value::str(&value.to_string_lossy()))
}

/// Every environment variable, sorted by name, so the order is the same on every machine.
pub(crate) fn variables() -> Value {
    let mut all: Vec<(String, String)> = std::env::vars_os()
        .map(|(name, value)| (name.to_string_lossy().into_owned(), value.to_string_lossy().into_owned()))
        .collect();
    all.sort();
    dict_of(all.iter().map(|(name, value)| (name.as_str(), value.as_str())))
}

/// A `dict[str, str]` of `pairs`, in their order.
pub(crate) fn dict_of<'a>(pairs: impl Iterator<Item = (&'a str, &'a str)>) -> Value {
    let mut built = Pairs::default();
    for (name, value) in pairs {
        built.set(Value::str(name), Value::str(value));
    }
    Value::Dict(Rc::new(built))
}
