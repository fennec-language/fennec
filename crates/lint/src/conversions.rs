//! `needless-conversion`: `str(s)` on what's text already, `int(n)` or `i64(n)` on an int,
//! `float(x)` or `f64(x)` on a float, and `f"{str(x)}"`, since an f-string shows any value as
//! `str` does. Only the typed tree knows what a value is, so this looks at a program that checks.
//! The fixes are tried together: without the conversions the program has to check and make no
//! copy each time round a loop (`fine_together`). A conversion where a place is named is left
//! alone: `mut y = str(s)` makes a value of `y`'s own, and `mut y = s` would make `y` stand for
//! `s`.

use std::collections::HashSet;

use fennec_check::Type;
use fennec_syntax::{Diagnostic, Expr, ExprKind, FStrPart, Module, Sources, Span};

use crate::Lint;
use crate::copies::{places, redefined};
use crate::rewrites::{operand, replaceable};
use crate::typed::Types;

/// The builtins this looks at.
const CONVERSIONS: [&str; 5] = ["str", "int", "float", "i64", "f64"];

/// Whether a builtin gives back what a value of this type is already.
fn gives(builtin: &str, ty: &Type) -> bool {
    matches!((builtin, ty), ("str", Type::Str) | ("int" | "i64", Type::Int) | ("float" | "f64", Type::Float))
}

/// A call to one of the builtins with one plain value, as its name and the value.
fn conversion(expr: &Expr) -> Option<(&str, &Expr)> {
    let ExprKind::Call { callee, args } = &expr.kind else { return None };
    let ExprKind::Name(name) = &callee.kind else { return None };
    let [arg] = args.as_slice() else { return None };
    (CONVERSIONS.contains(&name.as_str()) && arg.name.is_none() && !arg.is_mut)
        .then_some((name.as_str(), &arg.value))
}

/// A conversion taken out, keeping what it was given.
fn take_out(expr: &mut Expr) {
    if let Some(value) = conversion(expr).map(|(_, value)| value.clone()) {
        *expr = value;
    }
}

#[cfg(test)]
thread_local! {
    /// How many programs `still_fine` has checked on this thread, for the tests to count.
    static TRIALS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Whether a program checks and makes no copy each time round a loop, which a fix mustn't add.
fn still_fine(mut trial: Module) -> bool {
    #[cfg(test)]
    TRIALS.with(|trials| trials.set(trials.get() + 1));
    let Ok(signatures) = fennec_check::check_and_resolve(&mut trial) else { return false };
    !crate::costs::copies_each_round(&fennec_ir::costs(&fennec_ir::lower(&trial, &signatures)))
}

/// The candidates whose rewrites leave the program fine (`still_fine`) when they're all made, each
/// found at the span `span` gives it and rewritten by `rewrite`. They're all tried in one check.
/// Only when that fails are they split in halves, each half tried the same way, down to each one
/// that fails alone. A program that none of them breaks is checked once, however many there are.
/// Trying each alone checked all of the program again for each, so a program of 467 conversions
/// took 49 s to lint (#2315).
pub(crate) fn fine_together<T>(
    module: &Module,
    mut candidates: Vec<T>,
    span: &dyn Fn(&T) -> Span,
    rewrite: &dyn Fn(&mut Expr),
) -> Vec<T> {
    if candidates.is_empty() {
        return candidates;
    }
    let chosen: HashSet<(u32, u32)> = candidates
        .iter()
        .map(|candidate| {
            let at = span(candidate);
            (at.start, at.end)
        })
        .collect();
    let mut trial = module.clone();
    fennec_syntax::visit::exprs_mut(&mut trial, &mut |expr| {
        if chosen.contains(&(expr.span.start, expr.span.end)) {
            rewrite(expr);
        }
    });
    if still_fine(trial) {
        return candidates;
    }
    if candidates.len() == 1 {
        return Vec::new();
    }
    let second = candidates.split_off(candidates.len() / 2);
    let mut fine = fine_together(module, candidates, span, rewrite);
    fine.extend(fine_together(module, second, span, rewrite));
    fine
}

/// The lint for taking a conversion out, written as its value is: in brackets where it goes
/// beside an operator, and as it is where it's a call's argument, as `len(str(a + b))` is.
fn lint(sources: &Sources, (call, argument): (Span, bool), value: &Expr, message: &str) -> Option<Lint> {
    if !replaceable(sources, value) {
        return None;
    }
    let fixed =
        if argument { crate::rewrites::written(sources, value.span)? } else { operand(sources, value)? };
    let diagnostic = Diagnostic::new(format!("{message}: write `{fixed}`"), call).with_fix(call, fixed);
    Some(Lint::new("needless-conversion", diagnostic))
}

pub(crate) fn of(module: &Module, sources: &Sources, types: &Types<'_>) -> Vec<Lint> {
    let mut calls: Vec<(String, Span, Expr)> = Vec::new();
    let mut shown: HashSet<(u32, u32)> = HashSet::new();
    fennec_syntax::read::exprs(module, &mut |expr| {
        if let ExprKind::FStr(parts) = &expr.kind {
            for part in parts {
                if let FStrPart::Expr { expr, spec: None } = part {
                    shown.insert((expr.span.start, expr.span.end));
                }
            }
        }
        if let Some((name, value)) = conversion(expr)
            && CONVERSIONS.contains(&name)
            && !expr.span.in_prelude()
        {
            calls.push((name.to_owned(), expr.span, value.clone()));
        }
    });
    // Whether a program names a conversion as its own takes a while to find out, so it waits
    // until there's a call to one.
    let named: Vec<&str> = CONVERSIONS
        .into_iter()
        .filter(|name| calls.iter().any(|(called, ..)| called == name) && !redefined(module, name))
        .collect();
    calls.retain(|(called, ..)| named.contains(&called.as_str()));
    if calls.is_empty() {
        return Vec::new();
    }
    let Some(types) = types.by_span() else { return Vec::new() };
    let places = places(module);
    // The conversions that are a call's argument, where what's left needs no brackets.
    let mut arguments: HashSet<(u32, u32)> = HashSet::new();
    fennec_syntax::read::exprs(module, &mut |expr| {
        if let ExprKind::Call { args, .. } = &expr.kind {
            arguments.extend(args.iter().map(|arg| (arg.value.span.start, arg.value.span.end)));
        }
    });
    // Those the types say are needless, which a fix that takes them out is tried for.
    let mut needless = Vec::new();
    for (name, call, value) in calls {
        if places.contains(&(call.start, call.end)) {
            continue;
        }
        let same = types.get(&(value.span.start, value.span.end)).is_some_and(|ty| gives(&name, ty));
        let in_fstring = name == "str" && shown.contains(&(call.start, call.end));
        let message = if same {
            format!("this is a {name} already")
        } else if in_fstring {
            "an f-string shows any value as `str` does".to_owned()
        } else {
            continue;
        };
        needless.push((call, value, message));
    }
    fine_together(module, needless, &|(call, ..)| *call, &take_out)
        .into_iter()
        .filter_map(|(call, value, message)| {
            let argument = arguments.contains(&(call.start, call.end));
            lint(sources, (call, argument), &value, &message)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use fennec_syntax::{Expr, ExprKind, Sources, Span};

    use super::{TRIALS, fine_together, of, take_out};

    /// A program of `functions` functions, each converting an int to an int, parsed, with its
    /// sources.
    fn converting(functions: usize) -> (fennec_syntax::Module, Sources) {
        let mut source = String::new();
        for number in 0..functions {
            let _ =
                write!(source, "def kept_{number}(count: int) -> int:\n    return int(count) + {number}\n\n");
        }
        let mut sources = Sources::new();
        let base = sources.add("many.fen", source.clone());
        (fennec_syntax::parse_at(&source, base).expect("it parses"), sources)
    }

    fn trials() -> usize {
        TRIALS.with(std::cell::Cell::take)
    }

    /// Each conversion's span, in the order written.
    fn calls(module: &fennec_syntax::Module) -> Vec<Span> {
        let mut found = Vec::new();
        fennec_syntax::read::exprs(module, &mut |expr| {
            if let ExprKind::Call { callee, .. } = &expr.kind
                && matches!(&callee.kind, ExprKind::Name(name) if name == "int")
            {
                found.push(expr.span);
            }
        });
        found
    }

    /// Forty needless conversions are found with one check of the program with all of them taken
    /// out, not one check each, which took a program of 467 most of a minute (#2315).
    #[test]
    fn needless_conversions_are_all_tried_in_one_check() {
        let (module, sources) = converting(40);
        trials();
        assert_eq!(of(&module, &sources, &crate::typed::Types::new(&module)).len(), 40);
        assert_eq!(trials(), 1);
    }

    /// A fix that breaks the program is found by halving: the rest are kept, and the checks are
    /// a few, far fewer than one for each.
    #[test]
    fn a_fix_that_breaks_the_program_is_found_by_halving() {
        let (module, _) = converting(40);
        let spans = calls(&module);
        let broken = spans[17];
        let rewrite = |expr: &mut Expr| {
            if expr.span == broken {
                expr.kind = ExprKind::Str("not a number".to_owned());
            } else {
                take_out(expr);
            }
        };
        trials();
        let fine = fine_together(&module, spans.clone(), &|span| *span, &rewrite);
        assert_eq!(fine.len(), 39);
        assert!(!fine.contains(&broken));
        // Each halving checks two halves, one of which fails, down to the one: about twice the
        // halvings of 40, and one more for all of them at first.
        let checked = trials();
        assert!(checked <= 13, "checked {checked} times");
    }
}
