//! `needless-conversion`: `str(s)` on what's text already, `int(n)` on an int, `float(x)` on a
//! float, and `f"{str(x)}"`, since an f-string shows any value as `str` does. Only the typed
//! tree knows what a value is, so this looks at a program that checks. Each fix is tried:
//! without the conversion the program has to check and make no copy each
//! time round a loop. A conversion where a place is named is left alone: `mut y = str(s)` makes
//! a value of `y`'s own, and `mut y = s` would make `y` stand for `s`.

use std::collections::{HashMap, HashSet};

use fennec_check::Type;
use fennec_syntax::{Diagnostic, Expr, ExprKind, FStrPart, Module, Sources, Span};

use crate::Lint;
use crate::copies::{places, redefined};
use crate::rewrites::{operand, replaceable};

/// The builtins this looks at.
const CONVERSIONS: [&str; 3] = ["str", "int", "float"];

/// Whether a builtin gives back what a value of this type is already.
fn gives(builtin: &str, ty: &Type) -> bool {
    matches!((builtin, ty), ("str", Type::Str) | ("int", Type::Int) | ("float", Type::Float))
}

/// A call to one of the builtins with one plain value, as its name and the value.
fn conversion(expr: &Expr) -> Option<(&str, &Expr)> {
    let ExprKind::Call { callee, args } = &expr.kind else { return None };
    let ExprKind::Name(name) = &callee.kind else { return None };
    let [arg] = args.as_slice() else { return None };
    (CONVERSIONS.contains(&name.as_str()) && arg.name.is_none() && !arg.is_mut)
        .then_some((name.as_str(), &arg.value))
}

/// The expression with `call` taken out of it, keeping what it was given.
fn without(module: &Module, call: Span) -> Module {
    let mut trial = module.clone();
    fennec_syntax::visit::exprs_mut(&mut trial, &mut |expr| {
        if expr.span == call
            && let Some(value) = conversion(expr).map(|(_, value)| value.clone())
        {
            *expr = value;
        }
    });
    trial
}

/// Whether a program checks and makes no copy each time round a loop, which a fix mustn't add.
pub(crate) fn still_fine(mut trial: Module) -> bool {
    let Ok(signatures) = fennec_check::check_and_resolve(&mut trial) else { return false };
    !crate::costs::copies_each_round(&fennec_ir::costs(&fennec_ir::lower(&trial, &signatures)))
}

/// The lint for taking a conversion out, written as its value is.
fn lint(sources: &Sources, call: Span, value: &Expr, message: &str) -> Option<Lint> {
    if !replaceable(sources, value) {
        return None;
    }
    let fixed = operand(sources, value)?;
    let diagnostic = Diagnostic::new(format!("{message}: write `{fixed}`"), call).with_fix(call, fixed);
    Some(Lint::new("needless-conversion", diagnostic))
}

pub(crate) fn of(module: &Module, sources: &Sources) -> Vec<Lint> {
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
    let Ok(checked) = fennec_check::check_typed(module) else { return Vec::new() };
    let types: HashMap<(u32, u32), &Type> = checked
        .types
        .iter()
        .flat_map(|(_, body)| body.exprs())
        .map(|(span, ty)| ((span.start, span.end), ty))
        .collect();
    let places = places(module);
    let mut found = Vec::new();
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
        if still_fine(without(module, call)) {
            found.extend(lint(sources, call, &value, &message));
        }
    }
    found
}
