//! Diagnostics the checker shares with the interpreter. The wording matches what the
//! interpreter says when it finds the same mistake while running, so a mistake reads the
//! same whichever one catches it.

use fennec_syntax::{Diagnostic, Span};

use crate::types::{Type, a, article};

/// An unknown name, with a pointer to the Fennec spelling when it's a habit from elsewhere.
pub(crate) fn unknown_name(name: &str, span: Span) -> Diagnostic {
    let diagnostic = Diagnostic::new(format!("`{name}` doesn't exist"), span);
    match name {
        "print" | "console" => diagnostic.with_help(
            "there's no global output: write with the `stdout` that `main` receives, like `stdout.print(x)`",
        ),
        "true" | "false" => diagnostic.with_help("write `True` or `False`"),
        "null" | "nil" | "none" | "undefined" => diagnostic.with_help("write `None`"),
        "lambda" => diagnostic.with_help("write an arrow function: `x => x * 2`"),
        "raise" => diagnostic.with_help("Fennec has no exceptions: write `fail SomeError(...)`"),
        _ => match crate::builtins::modules_with(name).as_slice() {
            [] => diagnostic,
            modules @ [module, ..] => diagnostic.with_help(format!(
                "import it from std.{}, like `from std.{module} import {name}`",
                modules.join(" or std.")
            )),
        },
    }
}

pub(crate) fn not_mut(name: &str, span: Span) -> Diagnostic {
    Diagnostic::new(format!("`{name}` isn't mut"), span)
        .with_help(format!("declare it with `mut {name} = ...` if it needs to change"))
}

pub(crate) fn might_be_none(span: Span) -> Diagnostic {
    Diagnostic::new("this might be None", span)
        .with_help("check with `if x is None:` first, use `?.`, or give a default with `else`")
}

pub(crate) fn unhandled_result(span: Span) -> Diagnostic {
    Diagnostic::new("this is the result of something that can fail", span)
        .with_help("get the value out with `?`, `else`, or `match` first")
}

pub(crate) fn unhandled(span: Span) -> Diagnostic {
    Diagnostic::new("this can fail, but nothing handles it", span)
        .with_help("pass the error up with `?`, give a default with `else`, or handle it with `match`")
}

/// A condition that isn't a bool: Fennec has no truthiness.
pub(crate) fn not_bool(keyword: &str, ty: &Type, span: Span) -> Diagnostic {
    let help = if ty.is_collection() {
        "Fennec has no truthiness: write `len(x) > 0`"
    } else if ty.maybe_none() {
        "Fennec has no truthiness: write `x is None` or `x is not None`"
    } else {
        "Fennec has no truthiness: compare explicitly, like `count > 0`"
    };
    Diagnostic::new(format!("`{keyword}` needs a bool, but this is {}", article(&ty.kind())), span)
        .with_help(help)
}

pub(crate) fn no_method(ty: &Type, method: &str, span: Span) -> Diagnostic {
    let diagnostic = Diagnostic::new(format!("{} has no method `{method}`", describe(ty)), span);
    match method {
        "len" | "length" | "size" | "count" => diagnostic.with_help("use `len(x)`"),
        "push" | "add" => diagnostic.with_help("use `append`"),
        "trim" => diagnostic.with_help("use `strip()`"),
        "toUpperCase" | "to_uppercase" => diagnostic.with_help("use `upper()`"),
        "toLowerCase" | "to_lowercase" => diagnostic.with_help("use `lower()`"),
        "log" | "println" => diagnostic.with_help("use `stdout.print(...)`"),
        "copy" | "clone" => diagnostic.with_help("values are copied automatically when assigned: `b = a`"),
        _ => diagnostic,
    }
}

pub(crate) fn wrong_args(function: &str, span: Span) -> Diagnostic {
    Diagnostic::new(format!("`{function}` was called with the wrong arguments"), span)
}

pub(crate) fn needless_mut(function: &str, span: Span) -> Diagnostic {
    Diagnostic::new(format!("`{function}` doesn't change its arguments, so don't pass `mut`"), span)
}

/// A value of the wrong type for a parameter or field.
pub(crate) fn mismatch(what: &str, expected: &Type, actual: &Type, span: Span) -> Diagnostic {
    if actual.maybe_none() && !expected.maybe_none() {
        return might_be_none(span);
    }
    if matches!(actual, Type::Result(..)) {
        return unhandled_result(span);
    }
    let diagnostic =
        Diagnostic::new(format!("{what} should be {}, but this is {}", a(expected), a(actual)), span);
    match (expected, actual) {
        (Type::Float, Type::Int) => {
            diagnostic.with_help("write it with a decimal point, like `1.0`, or convert it with float(...)")
        }
        (Type::Int, Type::Float) => {
            diagnostic.with_help("convert it with int(...), round(...) or math.floor(...)")
        }
        _ => diagnostic,
    }
}

/// A type for the start of a sentence: `Enemy` for a named type, "a list" for the others.
pub(crate) fn describe(ty: &Type) -> String {
    match ty {
        Type::Named(name) => format!("`{name}`"),
        other => article(&other.kind()),
    }
}
