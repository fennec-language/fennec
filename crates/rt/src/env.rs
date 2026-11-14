//! The methods of the `Env` capability: the environment variables the program was started with.
//! In a test, `Env` is a fake holding only what the test set, so a test never depends on the
//! machine it runs on. With `fennec test --real` it's the real environment, which `set` can't
//! change.

use std::rc::Rc;

use crate::pairs::Pairs;
use crate::value::Value;

/// The environment variable `name`, or `None` when it isn't set. One that isn't UTF-8 is read
/// with each bad sequence as U+FFFD. A name that no variable can have is `None` without asking
/// the system: macOS's `getenv` reads `PATH=` as `PATH`, so a name from outside could reach
/// another variable than it says.
pub(crate) fn variable(name: &str) -> Value {
    if !is_a_name(name) {
        return Value::None;
    }
    std::env::var_os(name).map_or(Value::None, |value| Value::str(&value.to_string_lossy()))
}

/// Whether a variable can be called `name`: not empty, and holding no `=` or NUL, which end a
/// name in the system's list of them.
pub(crate) fn is_a_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['=', '\0'])
}

/// Every environment variable, sorted by name, so the order is the same on every machine. On
/// Windows, where names are the same in any case, each is upper case, as `variable` finds `Path`
/// by `PATH` there: `env.all()["PATH"]` finds what `env.get("PATH")` does.
pub(crate) fn variables() -> Value {
    let mut all: Vec<(String, String)> = std::env::vars_os()
        .map(|(name, value)| (written(&name.to_string_lossy()), value.to_string_lossy().into_owned()))
        .collect();
    all.sort();
    dict_of(all.iter().map(|(name, value)| (name.as_str(), value.as_str())))
}

/// A variable's name as `variables` gives it: upper case on Windows, and as it is elsewhere.
fn written(name: &str) -> String {
    if cfg!(windows) { name.to_uppercase() } else { name.to_owned() }
}

/// A `dict[str, str]` of `pairs`, in their order.
pub(crate) fn dict_of<'a>(pairs: impl Iterator<Item = (&'a str, &'a str)>) -> Value {
    let mut built = Pairs::default();
    for (name, value) in pairs {
        built.set(Value::str(name), Value::str(value));
    }
    Value::Dict(Rc::new(built))
}

#[cfg(test)]
mod tests {
    /// A name is upper case on Windows, where `Path` and `PATH` are the same variable, and as it
    /// is elsewhere, where they aren't.
    #[test]
    fn names_are_upper_case_only_where_case_doesnt_matter() {
        let wanted = if cfg!(windows) { "PATH" } else { "Path" };
        assert_eq!(super::written("Path"), wanted);
    }

    /// A name no variable can have reads as none, on the real environment, where macOS's
    /// `getenv` would read `PATH=` as `PATH`.
    #[test]
    fn a_name_no_variable_can_have_is_none() {
        assert!(!matches!(super::variable("PATH"), super::Value::None), "PATH isn't set here");
        for name in ["", "PATH=", "=", "PATH\0", "\0"] {
            assert!(matches!(super::variable(name), super::Value::None), "{name:?} read as a variable");
        }
    }
}
