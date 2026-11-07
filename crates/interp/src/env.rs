//! The methods of the `Env` capability: the environment variables the program was started with.
//! In a test, `Env` is a fake holding only what the test set, so a test never depends on the
//! machine it runs on. With `fennec test --real` it's the real environment, which `set` can't
//! change.

use std::rc::Rc;

use fennec_syntax::{Diagnostic, Span};

use crate::interp::Interp;
use crate::methods::no_method;
use crate::pairs::Pairs;
use crate::value::{Capability, Value};

impl Interp<'_> {
    pub(crate) fn env_method(
        &mut self,
        method: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Diagnostic> {
        let fake = self.world.as_mut().filter(|world| !world.real());
        match (method, args, fake) {
            ("get", [Value::Str(name)], Some(world)) => {
                Ok(world.variables.get(name.as_ref()).map_or(Value::None, |value| Value::str(value)))
            }
            ("get", [Value::Str(name)], None) => Ok(variable(name)),
            ("all", [], Some(world)) => {
                Ok(dict_of(world.variables.iter().map(|(name, value)| (name.as_str(), value.as_str()))))
            }
            ("all", [], None) => Ok(variables()),
            ("set", [Value::Str(name), Value::Str(value)], Some(world)) => {
                world.variables.insert(name.to_string(), value.to_string());
                Ok(Value::Unit)
            }
            ("set", [Value::Str(_), Value::Str(_)], None) => {
                Err(Diagnostic::new("`set` changes only a test's fake `Env`", span).with_help(
                    "with `fennec test --real`, `env` is the real environment, which a test can't change",
                ))
            }
            ("get" | "all" | "set", _, _) => {
                Err(Diagnostic::new(format!("wrong arguments to `{method}`"), span))
            }
            _ => Err(no_method(&Value::Capability(Capability::Env), method, span)),
        }
    }
}

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
fn dict_of<'a>(pairs: impl Iterator<Item = (&'a str, &'a str)>) -> Value {
    let mut built = Pairs::default();
    for (name, value) in pairs {
        built.set(Value::str(name), Value::str(value));
    }
    Value::Dict(Rc::new(built))
}
