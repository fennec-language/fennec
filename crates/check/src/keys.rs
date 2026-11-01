//! What can be a dict key or set item. Keys compare by their bits and hash the same way, so a
//! struct holding a float, which can be `-0.0` or NaN, or one with its own idea of equal, would
//! find the wrong entry or none.

use std::collections::{BTreeMap, HashSet};

use fennec_syntax::{Diagnostic, Span};

use crate::function::FnCx;
use crate::program::TypeKind;
use crate::types::Type;

/// Reports each dict or set in a function whose keys can't be keys.
pub(crate) fn check_dict_keys(cx: &mut FnCx<'_>, exprs: &BTreeMap<(u32, u32), Type>) {
    let mut seen = HashSet::new();
    for ((start, end), ty) in exprs {
        let mut keys = Vec::new();
        keys_in(ty, &mut keys);
        for key in keys {
            if let Some(problem) = key_problem(cx, &key, &mut Vec::new())
                && seen.insert(key.to_string())
            {
                let message = format!("`{key}` can't be a dict key or set item, because {problem}");
                let span = Span { start: *start, end: *end };
                cx.error(Diagnostic::new(message, span).with_help("key it by one of its fields instead"));
            }
        }
    }
}

/// The key types of the dicts and sets in a type.
fn keys_in(ty: &Type, keys: &mut Vec<Type>) {
    match ty {
        Type::Dict(key, _) | Type::Set(key) => keys.push(key.as_ref().clone()),
        _ => {}
    }
    for child in ty.children() {
        keys_in(child, keys);
    }
}

/// Why a type can't be a key, if it can't. `open` holds the types being looked at, since a type
/// can hold itself.
fn key_problem(cx: &mut FnCx<'_>, ty: &Type, open: &mut Vec<String>) -> Option<String> {
    let name = match ty {
        Type::Named(name) => name,
        Type::Generic(name, args) => {
            if let Some(problem) = args.iter().find_map(|arg| key_problem(cx, arg, open)) {
                return Some(problem);
            }
            name
        }
        _ => return ty.children().into_iter().find_map(|child| key_problem(cx, child, open)),
    };
    if open.contains(name) {
        return None;
    }
    let info = cx.checker.program.type_info(name)?;
    let (kind, def) = (info.kind, info.def.clone());
    if !matches!(kind, TypeKind::Struct | TypeKind::Enum) {
        return None;
    }
    if cx.checker.program.method(name, "==").is_some() {
        return Some(format!("`{name}` has its own `==`"));
    }
    open.push(name.clone());
    let fields = def.fields.iter().map(|field| (None, field));
    let in_cases =
        def.cases.iter().flat_map(|case| case.fields.iter().map(|field| (Some(case.name.as_str()), field)));
    let mut found = None;
    for (case, field) in fields.chain(in_cases) {
        let held = cx.checker.field_type(name, case, &field.name);
        found = if held == Type::Float {
            Some(format!("its field `{}` is a float", field.name))
        } else {
            key_problem(cx, &held, open)
        };
        if found.is_some() {
            break;
        }
    }
    open.pop();
    found
}
