//! What's no longer in the language, refused with what to write instead: `Ok`, `Err` and
//! `Result`, which a failure doesn't need now that it's handled with `?` and `else`.

use fennec_syntax::{Diagnostic, Span};

/// Whether a program wrote what's at `span`, rather than a lowering, which still writes `Ok`
/// and `Err` cases in what it makes of `?` and `else`.
pub(crate) fn written_by_hand(span: Span) -> bool {
    !crate::lend_lowering::written_in(span)
}

/// `Ok(value)`, written as the value as it is: `fennec fix` takes the `Ok(` and `)` off.
pub(crate) fn ok_call(call: Span, value: Option<Span>) -> Diagnostic {
    let said = Diagnostic::new("`Ok` is gone: a value is given as it is", call)
        .with_help("write `x` for `Ok(x)`; a function that gives nothing back just ends, or says `return`");
    match value {
        Some(value) => said
            .with_fix(Span { start: call.start, end: value.start }, "")
            .with_fix(Span { start: value.end, end: call.end }, ""),
        None => said,
    }
}

/// `Err(error)`, written as `fail error`. `fennec fix` makes that change in `return Err(error)`,
/// at `returned`, where the two do the same.
pub(crate) fn err_call(call: Span, error: Span, returned: Option<Span>) -> Diagnostic {
    let said = Diagnostic::new("`Err` is gone: a function fails with `fail error`", call)
        .with_help("write `fail e` for `return Err(e)`, in a function that says `fails E`");
    match returned {
        Some(returned) => said
            .with_fix(Span { start: returned.start, end: error.start }, "fail ")
            .with_fix(Span { start: error.end, end: call.end }, ""),
        None => said,
    }
}

/// `case Ok(...)` or `case Err(...)`: a failure is handled where it happens, not matched.
pub(crate) fn result_case(case: Span) -> Diagnostic {
    Diagnostic::new("`Ok` and `Err` are gone: a failure is handled with `except why:`", case).with_help(
        "write `x = f() except why:` and handle the failure in its block, with `case` arms under it \
         to tell failures apart; `fennec fix` rewrites most of these",
    )
}

/// `Result[T, E]` where a type is written.
pub(crate) fn result_type(written: Span) -> Diagnostic {
    Diagnostic::new("`Result` is gone: what can fail says so with `fails`", written).with_help(
        "a function that gives back `T` or fails with `E` says `-> T fails E`; to hand one on, take \
         a function, `run: () -> T fails E`",
    )
}
