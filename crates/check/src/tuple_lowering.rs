//! A tuple saves and loads as an array of its items, as in Python's `json.dumps`, when every item
//! can be saved. Once a program that saves checks, each tuple type its code holds gets a function
//! to save it and one to load it, written in Fennec as a program could write them:
//!
//! ```fennec
//! def fennec_tuple_serialize__of_int_and_str_end_[S: Serializer](held: (int, str), ref out: S):
//!     out.items("(int, str)", 2)
//!     out.item(held[0])
//!     out.item(held[1])
//!     out.end_items()
//! ```
//!
//! Their names start `fennec_tuple_`. Lowering calls them for `serialize` and
//! `deserialize` on a tuple, which generic code like `JsonWriter.item[T]` makes once `T` is one.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use fennec_syntax::{Item, Module};

use crate::types::Type;

/// The function written for a tuple type's `serialize` or `deserialize`, by `method`: the type is
/// in its name with each of its marks spelled out, since a function's key keeps brackets for its
/// type arguments. It's a name a program could write but wouldn't, as `fennec_test_` names are,
/// since a program's cached image is kept as source, which a name no program can write wouldn't
/// read back from.
pub fn tuple_function(method: &str, tuple: &Type) -> String {
    let mut name = format!("fennec_tuple_{method}_");
    // A program's own type checked under another name is named as it's written.
    for c in fennec_syntax::rename::shown_key(&tuple.to_string()).chars() {
        match c {
            '(' => name.push_str("_of_"),
            ')' => name.push_str("_end_"),
            '[' => name.push_str("_in_"),
            ']' => name.push_str("_out_"),
            ',' => name.push_str("_and_"),
            '?' => name.push_str("_maybe_"),
            '|' => name.push_str("_or_"),
            '.' => name.push_str("_dot_"),
            ' ' => {}
            other => name.push(other),
        }
    }
    name
}

/// Writes in the functions that save and load each tuple type reached from what the program saves,
/// and says whether it wrote any, for the program to be checked again. `saved` is every type the
/// check asked whether it saves or loads.
pub(crate) fn lower(module: &mut Module, saved: &BTreeSet<Type>) -> bool {
    if !crate::saving::uses_saving(module)
        || !saved.iter().any(|ty| ty.type_name().is_some() || holds_tuple(ty))
    {
        return false;
    }
    let mut checker = crate::checker::Checker::new(module);
    let mut tuples = BTreeSet::new();
    let mut open = BTreeSet::new();
    for ty in saved {
        reach(&mut checker, ty, &mut tuples, &mut open);
    }
    // Only those whose items can all be saved: the rest can't be, so nothing calls for them.
    tuples.retain(|tuple| {
        let Type::Tuple(items) = tuple else { return false };
        // A union is saved through a type made for it, which an item can't be changed to yet.
        !items.iter().any(crate::saving::holds_union)
            && items
                .iter()
                .all(|item| *item == Type::None || crate::saving::saved(&checker, item, &BTreeSet::new()))
    });
    if tuples.is_empty() {
        return false;
    }
    let written: BTreeSet<String> = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(def) => Some(def.name.clone()),
            _ => None,
        })
        .collect();
    let mut text = String::new();
    let mut names = Vec::new();
    for tuple in &tuples {
        let (save, load) = (tuple_function("serialize", tuple), tuple_function("deserialize", tuple));
        if written.contains(&save) {
            continue;
        }
        let Some(source) = functions_for(tuple, names.len()) else { continue };
        text.push_str(&source);
        names.push((save, load));
    }
    if names.is_empty() {
        return false;
    }
    let Ok(mut parsed) = fennec_syntax::parse_prelude(&text) else { return false };
    // They're written with the program's own types named as written, and then named as the
    // program's code names them, as a type named like a library type is checked under another
    // name: that name can't be written.
    fennec_syntax::rename::rename_types(&mut parsed, &renamed_types(module));
    let mut functions = parsed.items.into_iter().filter_map(|item| match item {
        Item::Function(def) => Some(def),
        _ => None,
    });
    for (save, load) in names {
        let (Some(mut saving), Some(mut loading)) = (functions.next(), functions.next()) else {
            return false;
        };
        saving.name = save;
        loading.name = load;
        module.items.push(Item::Function(saving));
        module.items.push(Item::Function(loading));
    }
    true
}

/// The program's own types checked under another name, each by the name it's written with.
fn renamed_types(module: &Module) -> std::collections::BTreeMap<String, String> {
    module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => Some(def.name.as_str()),
            _ => None,
        })
        .filter(|name| name.ends_with(fennec_syntax::rename::RENAMED))
        .map(|name| (fennec_syntax::rename::shown_type_name(name).to_owned(), name.to_owned()))
        .collect()
}

/// Whether a type holds a tuple anywhere in it.
fn holds_tuple(ty: &Type) -> bool {
    matches!(ty, Type::Tuple(_)) || ty.children().iter().any(|child| holds_tuple(child))
}

/// Notes each tuple type in `ty` and in what it holds: its items, a collection's, a type's type
/// arguments, and the fields of a type of the program's own, once each.
fn reach(
    checker: &mut crate::checker::Checker,
    ty: &Type,
    tuples: &mut BTreeSet<Type>,
    open: &mut BTreeSet<String>,
) {
    for child in ty.children() {
        reach(checker, child, tuples, open);
    }
    // One of a single item can't be saved (see `Generic::fits_interface`).
    if let Type::Tuple(items) = ty
        && items.len() > 1
        && written_plainly(ty)
    {
        tuples.insert(ty.clone());
    }
    let Some(name) = ty.type_name().map(str::to_owned) else { return };
    if !open.insert(ty.to_string()) {
        return;
    }
    let Some(def) = checker.program.type_info(&name).map(|info| info.def.clone()) else { return };
    let given: Vec<(String, Type)> =
        def.generics.iter().map(|generic| generic.name.clone()).zip(ty.type_args().iter().cloned()).collect();
    let fields = def.fields.iter().map(|field| (None, field.name.clone()));
    let in_cases = def
        .cases
        .iter()
        .flat_map(|case| case.fields.iter().map(|field| (Some(case.name.clone()), field.name.clone())));
    let all: Vec<(Option<String>, String)> = fields.chain(in_cases).collect();
    for (case, field) in all {
        let held = checker.field_type(&name, case.as_deref(), &field).substitute(&given);
        reach(checker, &held, tuples, open);
    }
}

/// Whether a type is written the same wherever it's used: no type parameter, nothing still being
/// worked out, and a written form.
fn written_plainly(ty: &Type) -> bool {
    !matches!(ty, Type::Param(_) | Type::Var(_) | Type::Unknown)
        && ty.source().is_some()
        && ty.children().into_iter().all(written_plainly)
}

/// The source of the two functions for a tuple type, under names numbered `at` that lowering
/// gives back their own.
fn functions_for(tuple: &Type, at: usize) -> Option<String> {
    let Type::Tuple(items) = tuple else { return None };
    let shown = fennec_syntax::rename::shown_key(&tuple.source()?);
    let count = items.len();
    let mut out = format!(
        "def tuple_save__{at}[S: Serializer](held: {shown}, ref out: S):\n    out.items(\"{shown}\", {count})\n"
    );
    for (at, item) in items.iter().enumerate() {
        // An item that's always `None` is saved as missing, as an optional's `None` is.
        if *item == Type::None {
            let _ = writeln!(out, "    out.item[int?](None)");
        } else {
            let _ = writeln!(out, "    out.item(held[{at}])");
        }
    }
    out.push_str("    out.end_items()\n\n");
    let _ = writeln!(
        out,
        "def tuple_load__{at}[D: Deserializer](ref src: D) -> {shown} fails ParseError:\n    src.items(\"{shown}\", {count})?"
    );
    let mut made = Vec::with_capacity(count);
    for (at, item) in items.iter().enumerate() {
        if *item == Type::None {
            let _ = writeln!(out, "    _ = src.item[int?]()?");
            made.push("None".to_owned());
        } else {
            let written = fennec_syntax::rename::shown_key(&item.source()?);
            let _ = writeln!(out, "    item{at} = src.item[{}]()?", crate::saving::one_type(&written));
            made.push(format!("item{at}"));
        }
    }
    let made = if count == 1 { format!("({},)", made.join(", ")) } else { format!("({})", made.join(", ")) };
    let _ = write!(out, "    src.end_items()?\n    return {made}\n\n");
    Some(out)
}
