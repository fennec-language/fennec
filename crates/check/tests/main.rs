//! The type checker's tests, as one program. Each test checks a small Fennec program and
//! compares what the checker reports, message by message, with the code each one points at.

mod builtins;
mod calls;
mod cases;
mod constants;
mod errors;
mod generics;
mod keys;
mod leading_dot;
mod library;
mod mutation;
mod names;
mod narrowing;
mod places;
mod returns;
mod shared;
mod signatures;
mod strict;
mod typed;
mod types;

use fennec_check::{Signatures, Type};

/// Removes the indentation the test's own source code adds to a Fennec program. Fennec
/// strings use double quotes, so single quotes stand in for them in the tests.
fn dedent(text: &str) -> String {
    let text = &text.replace('\'', "\"");
    let indent = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    let lines: Vec<&str> = text.lines().map(|line| line.get(indent..).unwrap_or("")).collect();
    format!("{}\n", lines.join("\n").trim_matches('\n'))
}

/// The message of the problem a `def` that gives back a value without writing its type has.
const MISSING_RETURN_TYPE: &str = "so its return type has to be written";

/// A program as `fennec fix` leaves it, with the types its `def`s give back written in. Most
/// tests are about something else, so their programs leave return types out.
fn with_return_types(source: String) -> String {
    let mut source = source;
    for _ in 0..4 {
        let Ok(module) = fennec_syntax::parse(&source) else { return source };
        let Err(diagnostics) = fennec_check::check(&module) else { return source };
        let missing: Vec<fennec_syntax::Diagnostic> = diagnostics
            .into_iter()
            .filter(|diagnostic| diagnostic.message.ends_with(MISSING_RETURN_TYPE))
            .collect();
        match fennec_syntax::apply_fixes(&source, 0, &missing) {
            Some(fixed) => source = fixed,
            None => return source,
        }
    }
    source
}

/// Checks a program, returning its signatures or every problem as `message @ code`. The
/// return types its `def`s give back are written in first.
fn check(text: &str) -> Result<Signatures, Vec<String>> {
    check_as_written(&with_return_types(dedent(text)))
}

/// Checks a program exactly as it's written.
fn check_as_written(source: &str) -> Result<Signatures, Vec<String>> {
    let module =
        fennec_syntax::parse(source).map_err(|diagnostic| vec![format!("parse: {}", diagnostic.message)])?;
    fennec_check::check(&module).map_err(|diagnostics| {
        diagnostics
            .iter()
            .map(|diagnostic| {
                let start = usize::try_from(diagnostic.span.start).unwrap_or(0);
                let end = usize::try_from(diagnostic.span.end).unwrap_or(0);
                format!("{} @ {}", diagnostic.message, source.get(start..end).unwrap_or("?"))
            })
            .collect()
    })
}

/// What the checker reports about a program exactly as it's written.
fn problems_as_written(text: &str) -> Vec<String> {
    check_as_written(&dedent(text)).err().unwrap_or_default()
}

/// What the checker reports about a program: nothing when it's fine.
fn problems(text: &str) -> Vec<String> {
    check(text).err().unwrap_or_default()
}

/// The help on each problem.
fn helps(text: &str) -> Vec<String> {
    let source = with_return_types(dedent(text));
    let Ok(module) = fennec_syntax::parse(&source) else { return vec!["parse error".to_owned()] };
    match fennec_check::check(&module) {
        Ok(_) => Vec::new(),
        Err(diagnostics) => {
            diagnostics.into_iter().map(|diagnostic| diagnostic.help.unwrap_or_default()).collect()
        }
    }
}

/// The problems in statements, wrapped in a `main` that receives `stdout`.
fn in_main(body: &str) -> Vec<String> {
    in_main_after("", body)
}

/// The problems in statements, wrapped in a `main` after some declarations.
fn in_main_after(declarations: &str, body: &str) -> Vec<String> {
    let body: String = dedent(body).lines().map(|line| ["    ", line, "\n"].concat()).collect();
    // `main` gives back nothing, whatever its last statement is.
    problems(&format!("{declarations}\ndef main(stdout: Stdout):\n{body}    pass\n"))
}

/// The return type inferred for a function, or the problems that stopped the check.
fn returns(text: &str, function: &str) -> Result<Type, Vec<String>> {
    let signatures = check(text)?;
    signatures
        .get(function)
        .map(|signature| signature.returns.clone())
        .ok_or_else(|| vec![format!("no {function}")])
}

fn list(item: Type) -> Type {
    Type::List(Box::new(item))
}

fn optional(inner: Type) -> Type {
    Type::Optional(Box::new(inner))
}

fn result(value: Type, error: Type) -> Type {
    Type::Result(Box::new(value), Box::new(error))
}

fn named(name: &str) -> Type {
    Type::Named(name.to_owned())
}
