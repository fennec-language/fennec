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
    let Type::Named(name) = ty else {
        return ty.children().into_iter().find_map(|child| key_problem(cx, child, open));
    };
    if open.contains(name) {
        return None;
    }
    let info = cx.checker.program.type_info(name)?;
    let (kind, def) = (info.kind, info.def.clone());
    if kind != TypeKind::Struct {
        return None;
    }
    if cx.checker.program.method(name, "==").is_some() {
        return Some(format!("`{name}` has its own `==`"));
    }
    open.push(name.clone());
    let mut found = None;
    for field in &def.fields {
        let held = cx.checker.field_type(name, None, &field.name);
        if held == Type::Float {
            found = Some(format!("its field `{}` is a float", field.name));
        } else {
            found = key_problem(cx, &held, open);
        }
        if found.is_some() {
            break;
        }
    }
    open.pop();
    found
}
