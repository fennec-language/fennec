//! What's said of a method a value doesn't have.

use fennec_syntax::{Diagnostic, Span};

use crate::value::Value;

pub(crate) fn no_method(value: &Value, method: &str, span: Span) -> Diagnostic {
    let name = value.type_name();
    let article =
        if name.starts_with(['a', 'e', 'i', 'o', 'u', 'A', 'E', 'I', 'O', 'U']) { "an" } else { "a" };
    let diagnostic = Diagnostic::new(format!("{article} {name} has no method `{method}`"), span);
    match method {
        "length" | "size" | "count" => diagnostic.with_help("use `len(x)`"),
        "push" | "add" => diagnostic.with_help("use `append`"),
        "trim" => diagnostic.with_help("use `strip()`"),
        "toUpperCase" | "to_uppercase" => diagnostic.with_help("use `upper()`"),
        "toLowerCase" | "to_lowercase" => diagnostic.with_help("use `lower()`"),
        "log" | "println" => diagnostic.with_help("use `stdout.print(...)`"),
        "copy" | "clone" => diagnostic.with_help("values are copied automatically when assigned: `b = a`"),
        _ => diagnostic,
    }
}
