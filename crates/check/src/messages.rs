//! Diagnostics the checker shares among its passes, so a mistake reads the same wherever it's
//! caught.

use fennec_syntax::{Diagnostic, Span};

use crate::types::{Type, a, article};

/// What an `IdMap` is called now, and where it is.
pub(crate) const TABLE: &str = "it's called `Table` now, from `std.collections`";

/// An unknown name, with a pointer to the Fennec spelling when it's a habit from elsewhere.
pub(crate) fn unknown_name(name: &str, span: Span) -> Diagnostic {
    if name.starts_with(&format!("_{}", fennec_syntax::visit::HIDDEN)) {
        return Diagnostic::new("`_` throws away what it's given, so it can't be read", span)
            .with_help("write a name where `_` is, as `item`, and read that");
    }
    let diagnostic = Diagnostic::new(format!("`{name}` doesn't exist"), span);
    match name {
        "print" | "console" => diagnostic.with_help(
            "there's no global output: a function reaches outside itself only through what's in its signature, like `stdout: Stdout`",
        ),
        "IdMap" => diagnostic.with_help(TABLE).with_fix(span, "Table"),
        "true" | "false" => diagnostic.with_help("write `True` or `False`"),
        "null" | "nil" | "none" | "undefined" => diagnostic.with_help("write `None`"),
        "lambda" => diagnostic.with_help("write an arrow function: `x => x * 2`"),
        "raise" => diagnostic.with_help("Fennec has no exceptions: write `fail SomeError(...)`"),
        "ord" => diagnostic.with_help("a character's code is a method: write `c.code()`"),
        "abs" | "len" => diagnostic.with_help(format!("`{name}` is called on a value: write `{name}(x)`, or `x => {name}(x)` to pass it")),
        "chr" => diagnostic.with_help("make a character from its code with `str.from_code(n)`"),
        "embed_file" | "embed_folder" => diagnostic.with_help(format!(
            "`{name}` reads what's beside the source, and this source isn't in a file"
        )),
        "embed" => diagnostic.with_help("`embed` is now `embed_file`: `fennec fix` makes this change").with_fix(span, "embed_file"),
        _ => match crate::builtins::modules_with(name).as_slice() {
            [] => diagnostic,
            modules @ [module, ..] => diagnostic.with_help(format!(
                "import it from std.{}, like `from std.{module} import {name}`",
                modules.join(" or std.")
            )),
        },
    }
}

/// Which of `names` was probably meant by a name that doesn't exist: the closest, if it's a slip
/// of a letter or two and not another word. A name of one or two letters is too short to tell.
pub(crate) fn spelled_most_like<'n>(name: &str, names: impl Iterator<Item = &'n str>) -> Option<&'n str> {
    let wanted: Vec<char> = name.chars().collect();
    let allowed = (wanted.len() / 3).min(2);
    let mut distances: Vec<(usize, &str)> = names
        .filter_map(|other| {
            let distance = edits(&wanted, &other.chars().collect::<Vec<char>>());
            (distance <= allowed && distance > 0).then_some((distance, other))
        })
        .collect();
    distances.sort_unstable();
    distances.first().map(|(_, closest)| *closest)
}

/// How many letters to add, remove, change or swap with the next one, to get from `a` to `b`.
fn edits(a: &[char], b: &[char]) -> usize {
    let at = |row: &[usize], column: usize| row.get(column).copied().unwrap_or(usize::MAX - 1);
    let mut two_back: Vec<usize> = Vec::new();
    let mut last: Vec<usize> = (0..=b.len()).collect();
    for (i, letter) in a.iter().enumerate() {
        let mut row = vec![i + 1];
        for (j, other) in b.iter().enumerate() {
            let changed = at(&last, j) + usize::from(letter != other);
            let mut best = changed.min(at(&last, j + 1) + 1).min(at(&row, j) + 1);
            if i > 0 && j > 0 && a.get(i - 1) == Some(other) && b.get(j - 1) == Some(letter) {
                best = best.min(at(&two_back, j - 1) + 1);
            }
            row.push(best);
        }
        two_back = std::mem::replace(&mut last, row);
    }
    at(&last, b.len())
}

/// `name` given a new value, with where `mut ` would go in its declaration when there's one.
pub(crate) fn not_mut(name: &str, span: Span, mut_at: Option<u32>) -> Diagnostic {
    let diagnostic = Diagnostic::new(format!("`{name}` isn't mut"), span);
    match mut_at {
        // Adding `mut` changes what the program means, so it's a fix to make by hand.
        Some(at) => {
            diagnostic.with_hint(format!("declare it `mut {name}`"), Span { start: at, end: at }, "mut ")
        }
        None => diagnostic.with_help(format!("declare it with `mut {name} = ...` if it needs to change")),
    }
}

pub(crate) fn might_be_none(span: Span) -> Diagnostic {
    Diagnostic::new("this might be None", span)
        .with_help("check with `if x is None:` first, or give a default with `else`")
}

/// A field or method of something that might be None, which a chain ending in `else` can read.
pub(crate) fn missing_step(span: Span) -> Diagnostic {
    Diagnostic::new("this might be None", span)
        .with_help("end the chain with `else` and a default, like `x.name else \"\"`, or check with `if x is None:` first")
}

/// A chain ending in `else` that goes through a call that can fail, at that call: `else` is for a
/// value that's missing, and a failure is handled with `except`, on a line of its own.
pub(crate) fn else_through_failure(call: Span) -> Diagnostic {
    Diagnostic::new(
        "this can fail, and `else` doesn't handle a failure: `else` is for a value that's missing",
        call,
    )
    .with_help(
        "give the call its own line with `except`, as `found = find(id) except why:` or `found = find(id) except Lost(): None`, then go on from it: `found.name else \"?\"`",
    )
}

/// What to do with a failure nothing handles, in the forms of 2026-11-11.
const HANDLE_IT: &str = "handle the failure with `except why:`, give a value for the cases it fails with, as `x = f() except Missing(): 0`, or pass it up with `?` from a function that says `fails`";

pub(crate) fn unhandled_result(span: Span) -> Diagnostic {
    Diagnostic::new("this is the result of something that can fail", span).with_help(
        "handle the failure where it's made: `x = f()?` passes it up, `x = f() except Missing(): 0` gives a value instead, and `x = f() except why:` handles it in a block",
    )
}

pub(crate) fn dropped_failure(span: Span) -> Diagnostic {
    Diagnostic::new("`_ =` would drop this failure", span).with_help(HANDLE_IT)
}

pub(crate) fn unhandled(span: Span) -> Diagnostic {
    Diagnostic::new("this can fail, but nothing handles it", span).with_help(HANDLE_IT)
}

/// A condition that isn't a bool: Fennec has no truthiness. A bool from something that can fail,
/// like `fs.exists(path)`, gets `?` from `fennec fix`, which passes the failure up.
pub(crate) fn not_bool(keyword: &str, ty: &Type, span: Span) -> Diagnostic {
    if let Type::Result(value, _) = ty
        && **value == Type::Bool
    {
        return Diagnostic::new(format!("`{keyword}` needs a bool, but this can fail"), span)
            .with_help("pass the failure up with `?`, or handle it with `except why:` first")
            .with_fix(Span { start: span.end, end: span.end }, "?");
    }
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

/// How Fennec formats text, for Python's `%` and `.format()`.
pub(crate) const F_STRINGS: &str = "Fennec formats text with f-strings, as `f\"{count} items\"`";

pub(crate) fn no_method(ty: &Type, method: &str, span: Span) -> Diagnostic {
    let diagnostic = Diagnostic::new(format!("{} has no method `{method}`", describe(ty)), span);
    match method {
        "length" | "size" | "count" => diagnostic.with_help("use `len(x)`"),
        "push" | "add" => diagnostic.with_help("use `append`"),
        "trim" => diagnostic.with_help("use `strip()`"),
        "toUpperCase" | "to_uppercase" => diagnostic.with_help("use `upper()`"),
        "toLowerCase" | "to_lowercase" => diagnostic.with_help("use `lower()`"),
        "log" | "println" => diagnostic.with_help("use `stdout.print(...)`"),
        "copy" | "clone" => diagnostic.with_help("a plain `=` makes a copy already: `kept = value`"),
        "format" if matches!(ty, Type::Str | Type::LiteralStr) => diagnostic.with_help(F_STRINGS),
        _ => diagnostic,
    }
}

/// What a problem `no_method` made says is missing: who it's missing from, as it says it, and the
/// method. For telling a caller that can see more of a program than the checker does.
#[must_use]
pub fn missing_method(diagnostic: &Diagnostic) -> Option<(&str, &str)> {
    let (who, method) = diagnostic.message.split_once(" has no method `")?;
    Some((who, method.strip_suffix('`')?))
}

pub(crate) fn wrong_args(function: &str, span: Span) -> Diagnostic {
    Diagnostic::new(format!("`{function}` was called with the wrong arguments"), span)
}

pub(crate) fn needless_mut(function: &str, span: Span) -> Diagnostic {
    Diagnostic::new(format!("`{function}` doesn't change its arguments, so don't pass `ref`"), span)
        .with_help("take `ref` off the argument")
}

/// A value of the wrong type for a parameter or field.
/// What a `LiteralStr` is, for where a `str` was given instead.
pub(crate) const LITERAL_HELP: &str = "a `LiteralStr` is text written in quotes here in the source, or such text joined with `+`; \
     an f-string or a `str` made while the program runs isn't one, so pass what changes as a value instead";

/// How to use a generator that can fail where it can't go: it fails where it's looped over, so
/// only the function that made it, which passes its failure up, can loop over it.
const FAILING_GENERATOR_HELP: &str = "loop over a generator that can fail where it's made, so its failure passes up from there, or keep its items with `list(...)` first";

/// A generator that can fail, or a value holding one, used inside a nested function, an arrow
/// function, a generator kept for later or a `parallel` body, which would loop over it where its
/// failure can't pass up as it says.
pub(crate) fn failing_generator_inside(name: &str, holds: bool, span: Span) -> Diagnostic {
    let what = if holds { "holds a generator that can fail" } else { "is a generator that can fail" };
    let message = format!(
        "`{name}` {what}, so a nested function, an arrow function, a generator kept for later or a `parallel` body can't use it"
    );
    Diagnostic::new(message, span).with_help(FAILING_GENERATOR_HELP)
}

/// An arrow function's own parameter given a generator that can fail, or a value holding one: the
/// arrow is a function of its own, so looping over it there can't pass its failure up.
pub(crate) fn failing_generator_given_to_arrow(name: &str, holds: bool, span: Span) -> Diagnostic {
    let what = if holds { "a value holding a generator that can fail" } else { "a generator that can fail" };
    let message = format!(
        "this arrow function's `{name}` is given {what}, and its failure can't pass up from inside the arrow"
    );
    Diagnostic::new(message, span).with_help(FAILING_GENERATOR_HELP)
}

/// `except` on a `def`'s call given a function: the function can make a generator that can fail,
/// which the call gives back with its failure counted nowhere here.
pub(crate) fn except_on_a_call_given_a_function(span: Span) -> Diagnostic {
    let message = "this gives back a generator that can fail, which a function it was given may have made, and `except` on it leaves that generator's failure nowhere to go";
    Diagnostic::new(message, span)
        .with_help("write `?`: then this function passes the generator's failure up where it's looped over")
}

/// `parallel` over items that hold a generator that can fail: each item goes to another task,
/// where its failure can't pass up as it says.
pub(crate) fn failing_generator_in_parallel(span: Span) -> Diagnostic {
    let message =
        "`parallel` hands each item to another task, so its items can't hold a generator that can fail";
    Diagnostic::new(message, span).with_help(FAILING_GENERATOR_HELP)
}

/// `except` on an arrow function's call whose value holds a generator that can fail: the arrow's
/// `?` counted that failure as the arrow's own, and `except` leaves it nowhere to go here.
pub(crate) fn except_on_an_arrow_giving_one(span: Span) -> Diagnostic {
    let message = "this arrow function gives back a generator that can fail, and `except` on its call leaves nothing to pass that generator's failure up";
    Diagnostic::new(message, span)
        .with_help("write `?`: then this function passes the generator's failure up where it's looped over")
}

/// `except` on a generator function's call. Making a generator runs none of its body, so it can't
/// fail there: its failure comes from where it's looped over, and the `except` would never run.
pub(crate) fn except_on_a_generator(span: Span) -> Diagnostic {
    let message =
        "making a generator runs none of its body, so this can't fail here and `except` would never run";
    Diagnostic::new(message, span).with_help(
        "write `?`: its failure passes up from where it's looped over, so handle it around the call that loops over it",
    )
}

pub(crate) fn mismatch(what: &str, expected: &Type, actual: &Type, span: Span) -> Diagnostic {
    if actual.maybe_none() && !expected.maybe_none() {
        return might_be_none(span);
    }
    if matches!(actual, Type::Result(..)) && !matches!(expected, Type::Result(..)) {
        return unhandled_result(span);
    }
    let diagnostic =
        Diagnostic::new(format!("{what} should be {}, but this is {}", a(expected), a(actual)), span);
    match (expected, actual) {
        (expected, _) if expected.has_literal() => diagnostic.with_help(LITERAL_HELP),
        (Type::Float, Type::Int) => {
            diagnostic.with_help("write it with a decimal point, like `1.0`, or convert it with float(...)")
        }
        (Type::Int, Type::Float) => {
            diagnostic.with_help("convert it with int(...), round(...) or math.floor(...)")
        }
        (Type::Int | Type::Float, Type::Str) => {
            let number = if *expected == Type::Int { "int" } else { "float" };
            diagnostic
                .with_help(format!("read the text as a number, as `{number}(text)?`, or write a number"))
        }
        (Type::Str, Type::Int | Type::Float | Type::Bool) => {
            diagnostic.with_help("turn it into text with an f-string, as `f\"{count}\"`, or `str(count)`")
        }
        (Type::Iterator(_, None), Type::Iterator(_, Some(_))) => diagnostic.with_help(FAILING_GENERATOR_HELP),
        (Type::Arrow(..), Type::Function(key)) => {
            let name = key.rsplit('.').next().unwrap_or(key);
            diagnostic.with_help(format!(
                "`{name}` has to take and give back what {} says, with no `ref` parameter or type parameters of its own",
                a(expected)
            ))
        }
        _ => diagnostic,
    }
}

/// A type for the start of a sentence: `Enemy` for a named type, "a list" for the others.
pub(crate) fn describe(ty: &Type) -> String {
    match ty {
        Type::Named(name) | Type::Param(name) => format!("`{name}`"),
        other => article(&other.kind()),
    }
}

/// An f-string format spec that doesn't fit its value, or `None` when it fits.
/// A spec is an optional `,` for thousands separators, an optional `.N` for decimal places,
/// and an optional `f` or `d`; only a number takes one.
pub(crate) fn bad_spec(spec: &str, ty: &Type, span: Span) -> Option<Diagnostic> {
    const FORMS: &str = "a format is an optional `,` for thousands separators, then `.N` for N decimal places, then `f` or `d`: like `{x:,}`, `{x:.2f}` or `{n:,d}`";
    let rest = spec.strip_prefix(',').unwrap_or(spec);
    let (places, kind) = match rest.strip_prefix('.') {
        Some(rest) => {
            let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
            let (number, kind) = rest.split_at(digits);
            if number.parse::<usize>().is_err() {
                return Some(
                    Diagnostic::new(format!("`{spec}` needs a number after `.`"), span).with_help(FORMS),
                );
            }
            (true, kind)
        }
        None => (false, rest),
    };
    if !matches!(kind, "" | "d" | "f") {
        return Some(Diagnostic::new(format!("Fennec has no format `{spec}`"), span).with_help(FORMS));
    }
    // A sized integer formats as an int does, and an `f32` as a float.
    let number = match ty {
        Type::Sized(num) if num.is_float() => &Type::Float,
        Type::Sized(_) => &Type::Int,
        other => other,
    };
    let help = match (number, kind, places) {
        (Type::Unknown, _, _)
        | (Type::Int, "" | "d", false)
        | (Type::Float, "" | "f", false)
        | (Type::Int | Type::Float, "" | "f", true) => return None,
        (_, _, _) if spec.is_empty() => return None,
        (Type::Int, "f", false) => "say how many decimal places, like `.2f`",
        (Type::Int, "d", true) => {
            "`d` shows a whole number: drop the `.N`, or write `.Nf` for decimal places"
        }
        (Type::Float, "d", _) => {
            "show it with no decimal places with `.0f`, or round it to an int with `round(x)`"
        }
        (Type::Str, _, _) => "drop the format, or read the text as a number first, as `{float.parse(text)?:.2f}`",
        _ => "only an int or a float takes a format: turn it into one first, or drop the format",
    };
    Some(Diagnostic::new(format!("the format `{spec}` doesn't apply to {}", a(ty)), span).with_help(help))
}

/// `.NotFound` where more than one of the errors it could be has that case, matched or built: it
/// names each and shows each written out.
pub(crate) fn shared_case(owners: &[String], case: &str, span: Span) -> Diagnostic {
    let shown: Vec<String> = owners.iter().map(|owner| format!("`{owner}`")).collect();
    let named = match shown.split_last() {
        Some((last, [only])) => format!("both {only} and {last}"),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => String::new(),
    };
    let written: Vec<String> = owners.iter().map(|owner| format!("`{owner}.{case}(...)`")).collect();
    let message = format!("`.{case}` is a case of {named}, so say which");
    let help = format!("write which: {}", written.join(" or "));
    Diagnostic::new(message, span).with_help(help)
}
