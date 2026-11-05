//! A type saves and loads through `def serialize` and `def deserialize`, which most types never
//! write: they're generated here from its fields, as code like any other, for the types whose
//! fields can all be saved. It only happens in a program that uses saving, so others check as if
//! it weren't there. A type that writes either one keeps what it wrote.

use std::collections::BTreeSet;
use std::fmt::Write;

use fennec_syntax::{Expr, ExprKind, Field, Item, Module, TypeDef};

use crate::checker::Checker;
use crate::types::Type;

/// The modules that save through these methods.
const SAVING_MODULES: [&str; 3] = ["json", "toml", "pack"];

/// The names that show a program uses saving.
const SAVING_NAMES: [&str; 4] = ["serialize", "deserialize", "Serializable", "Deserializable"];

/// Adds the methods a program's types don't write, when it uses saving.
pub(crate) fn generate(module: &mut Module) {
    if !uses_saving(module) {
        return;
    }
    let mut checker = Checker::new(module);
    let types: Vec<TypeDef> = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Struct(def) | Item::Enum(def) if !def.shared => Some(def.clone()),
            _ => None,
        })
        .collect();
    let written = written_by_hand(module);
    let mut generated: BTreeSet<String> = types
        .iter()
        .filter(|def| !written.contains(&def.name) && !has_method(def))
        .map(|def| def.name.clone())
        .collect();
    // What can't be saved takes what holds it with it, until nothing more goes.
    loop {
        let before = generated.len();
        for def in &types {
            if generated.contains(&def.name) && !all_saved(&mut checker, def, &generated) {
                generated.remove(&def.name);
            }
        }
        if generated.len() == before {
            break;
        }
    }
    for def in types.iter().filter(|def| generated.contains(&def.name)) {
        if let Some(items) = pair_for(&mut checker, def) {
            module.items.extend(items);
        }
    }
}

fn uses_saving(module: &Module) -> bool {
    // A program's own interface by one of these names is its own business.
    let own = module.items.iter().any(|item| match item {
        Item::Interface(interface) => SAVING_NAMES.contains(&interface.name.as_str()),
        _ => false,
    });
    if own {
        return false;
    }
    let imported = module.items.iter().any(|item| match item {
        Item::Import(import) => {
            import.module.first().is_some_and(|first| first == "std")
                && import.module.get(1).is_some_and(|name| SAVING_MODULES.contains(&name.as_str()))
        }
        _ => false,
    });
    if imported {
        return true;
    }
    let mut named = false;
    let mut walked = module.clone();
    fennec_syntax::visit::exprs_mut(&mut walked, &mut |expr| {
        named |= match &expr.kind {
            ExprKind::Name(name) | ExprKind::Attr { name, .. } => SAVING_NAMES.contains(&name.as_str()),
            _ => false,
        };
    });
    fennec_syntax::visit::functions_mut(&mut walked, &mut |def| {
        named |= def
            .generics
            .iter()
            .flat_map(|generic| &generic.bounds)
            .any(|bound| SAVING_NAMES.contains(&bound.name.as_str()))
            || def
                .params
                .iter()
                .filter_map(|param| param.ty.as_ref())
                .any(|ty| SAVING_NAMES.contains(&ty.name.as_str()));
    });
    named
}

fn has_method(def: &TypeDef) -> bool {
    def.methods.iter().any(|method| method.name == "serialize" || method.name == "deserialize")
}

fn written_by_hand(module: &Module) -> BTreeSet<String> {
    let extended = module.items.iter().filter_map(|item| match item {
        Item::Extend(extension)
            if extension
                .methods
                .iter()
                .any(|method| method.name == "serialize" || method.name == "deserialize") =>
        {
            Some(extension.name.clone())
        }
        _ => None,
    });
    extended.collect()
}

fn skipped(field: &Field) -> bool {
    field.annotations.iter().any(|annotation| annotation.name == "skip")
}

/// The name a field is saved under, when `@serialize(name=...)` gave it one.
fn renamed(field: &Field) -> Option<&str> {
    let annotation = field.annotations.iter().find(|annotation| annotation.name == "serialize")?;
    annotation.args.iter().find_map(|arg| match &arg.value.kind {
        ExprKind::Str(name) if arg.name.as_deref() == Some("name") => Some(name.as_str()),
        _ => None,
    })
}

fn all_saved(checker: &mut Checker, def: &TypeDef, generated: &BTreeSet<String>) -> bool {
    let fields = def.fields.iter().map(|field| (None, field));
    let in_cases =
        def.cases.iter().flat_map(|case| case.fields.iter().map(|field| (Some(case.name.as_str()), field)));
    let all: Vec<(Option<&str>, &Field)> = fields.chain(in_cases).collect();
    all.into_iter().filter(|(_, field)| !skipped(field)).all(|(case, field)| {
        let ty = checker.field_type(&def.name, case, &field.name);
        saved(checker, &ty, generated)
    })
}

/// Whether a value of this type can be saved and loaded. A type parameter is the extension's to
/// ask of what it stands for.
fn saved(checker: &Checker, ty: &Type, generated: &BTreeSet<String>) -> bool {
    match ty {
        Type::Int | Type::Float | Type::Bool | Type::Str | Type::Param(_) => true,
        Type::List(inner) | Type::Set(inner) | Type::Optional(inner) => saved(checker, inner, generated),
        Type::Dict(key, value) => saved(checker, key, generated) && saved(checker, value, generated),
        Type::Named(name) | Type::Generic(name, _) => {
            let own = generated.contains(name) || has_both(checker, name);
            own && ty.type_args().iter().all(|arg| saved(checker, arg, generated))
        }
        _ => false,
    }
}

fn has_both(checker: &Checker, name: &str) -> bool {
    !checker.program.is_shared(&Type::named(name))
        && ["serialize", "deserialize"].iter().all(|method| checker.program.method(name, method).is_some())
}

/// One field to read: its type as it's written, and whether it holds an optional already.
struct Read<'f> {
    field: &'f Field,
    ty: String,
    optional: bool,
}

/// What loading a type's fields needs: those it reads, and those it leaves out, with what's known
/// of their types.
type Fields<'f> = (Vec<Read<'f>>, Vec<(&'f Field, Option<String>)>);

fn reads<'f>(
    checker: &mut Checker,
    owner: &str,
    case: Option<&str>,
    fields: &'f [Field],
) -> Option<Fields<'f>> {
    let mut saved = Vec::new();
    let mut left_out = Vec::new();
    for field in fields {
        let ty = checker.field_type(owner, case, &field.name);
        if skipped(field) {
            left_out.push((field, ty.source()));
        } else {
            saved.push(Read { field, ty: ty.source()?, optional: matches!(ty, Type::Optional(_)) });
        }
    }
    Some((saved, left_out))
}

/// The constant and the extension for a type, parsed.
fn pair_for(checker: &mut Checker, def: &TypeDef) -> Option<Vec<Item>> {
    let name = &def.name;
    let mut text = String::new();
    let (saving, loading) = if def.cases.is_empty() {
        let fields = reads(checker, name, None, &def.fields)?;
        let list = field_list(&def.fields);
        let _ = writeln!(text, "{} = [{list}]\n", constant(name, ""));
        (save_struct(name, &fields.0), load_struct(name, &def.fields, &fields))
    } else {
        let cases = def.cases.iter().map(|case| {
            format!("(\"{}\", {})", case.name, if case.fields.is_empty() { "False" } else { "True" })
        });
        let list = cases.collect::<Vec<_>>().join(", ");
        let _ = writeln!(text, "{} = [{list}]\n", constant(name, ""));
        let mut saving = String::from("        match self:\n");
        let mut loading = format!("        match src__.case(\"{name}\", {})?:\n", constant(name, ""));
        for (at, case) in def.cases.iter().enumerate() {
            let fields = reads(checker, name, Some(&case.name), &case.fields)?;
            save_case(&mut saving, name, &case.name, &fields.0);
            load_case(&mut text, &mut loading, name, (at, &case.name), (&case.fields, &fields));
        }
        let _ = writeln!(
            loading,
            "            case _:\n                fail src__.rule(\"`{name}` has no such case\")"
        );
        (saving, loading)
    };
    let shown = bounded(def, "Serializable");
    let _ =
        writeln!(text, "extend {name}{shown}:\n    def serialize(self, mut out__: Serializer):\n{saving}");
    let shown = bounded(def, "Deserializable");
    let _ = writeln!(
        text,
        "extend {name}{shown}:\n    def deserialize(mut src__: Deserializer) -> Self fails ParseError:\n{loading}"
    );
    let mut parsed = fennec_syntax::parse_prelude(&text).ok()?;
    fill_in(&mut parsed, def);
    Some(parsed.items)
}

fn bounded(def: &TypeDef, bound: &str) -> String {
    if def.generics.is_empty() {
        return String::new();
    }
    let params: Vec<String> =
        def.generics.iter().map(|generic| format!("{}: {bound}", generic.name)).collect();
    format!("[{}]", params.join(", "))
}

fn constant(owner: &str, case: &str) -> String {
    let owner = owner.replace('.', "_");
    format!("SAVED_NAMES_{owner}_{case}")
}

fn field_list(fields: &[Field]) -> String {
    let saved = fields.iter().filter(|field| !skipped(field));
    let saved =
        saved.map(|field| format!("(\"{}\", \"{}\")", renamed(field).unwrap_or(&field.name), field.name));
    let left_out =
        fields.iter().filter(|field| skipped(field)).map(|field| format!("(\"\", \"{}\")", field.name));
    saved.chain(left_out).collect::<Vec<_>>().join(", ")
}

fn save_fields(out: &mut String, indent: &str, fields: &[Read<'_>], value: impl Fn(&str) -> String) {
    for read in fields {
        let name = &read.field.name;
        let _ = match renamed(read.field) {
            Some(shown) => writeln!(out, "{indent}out__.renamed(\"{shown}\", {})", value(name)),
            None => writeln!(out, "{indent}out__.field(\"{name}\", {})", value(name)),
        };
    }
    let _ = writeln!(out, "{indent}out__.end()");
}

fn save_struct(name: &str, fields: &[Read<'_>]) -> String {
    let mut out = format!("        out__.record(\"{name}\", {})\n", fields.len());
    save_fields(&mut out, "        ", fields, |field| format!("self.{field}"));
    out
}

fn save_case(out: &mut String, name: &str, case: &str, fields: &[Read<'_>]) {
    if fields.is_empty() {
        let _ =
            writeln!(out, "            case .{case}:\n                out__.case(\"{name}\", \"{case}\")");
        return;
    }
    let names: Vec<&str> = fields.iter().map(|read| read.field.name.as_str()).collect();
    let _ = writeln!(out, "            case .{case}({}):", names.join(", "));
    let _ = writeln!(out, "                out__.case_record(\"{name}\", \"{case}\", {})", fields.len());
    save_fields(out, "                ", fields, str::to_owned);
    let _ = writeln!(out, "                out__.end()");
}

/// Reads fields into names of their own, each what the type's constructor is given.
fn load_fields(out: &mut String, indent: &str, list: &str, (fields, left_out): &Fields<'_>) {
    for read in fields {
        let _ = writeln!(
            out,
            "{indent}mut read_{}: {}{} = None",
            read.field.name,
            read.ty,
            if read.optional { "" } else { "?" }
        );
    }
    let _ = writeln!(out, "{indent}while src__.next_field({list})?:\n{indent}    match src__.field_at():");
    for (at, read) in fields.iter().enumerate() {
        let wanted = if read.optional { read.ty.clone() } else { format!("{}?", read.ty) };
        let _ = writeln!(
            out,
            "{indent}        case {at}:\n{indent}            read_{} = src__.value[{wanted}]()?",
            read.field.name
        );
    }
    let _ = writeln!(out, "{indent}        case _:\n{indent}            pass");
    for read in fields {
        let name = &read.field.name;
        let shown = renamed(read.field).unwrap_or(name);
        if read.field.default.is_some() {
            let _ = writeln!(out, "{indent}{name} = read_{name} else DEFAULT_{name}");
        } else if read.optional {
            let _ = writeln!(out, "{indent}{name} = read_{name}");
        } else {
            let _ = writeln!(out, "{indent}if read_{name} is None: fail src__.missing(\"{shown}\")");
            let _ = writeln!(out, "{indent}{name} = read_{name}");
        }
    }
    for (field, ty) in left_out {
        let name = &field.name;
        let _ = match ty {
            Some(ty) => writeln!(out, "{indent}{name}: {ty} = DEFAULT_{name}"),
            None => writeln!(out, "{indent}{name} = DEFAULT_{name}"),
        };
    }
}

fn given(all: &[Field]) -> String {
    all.iter().map(|field| format!("{0}={0}", field.name)).collect::<Vec<_>>().join(", ")
}

/// Gives back the value. One with a `where` rule fails to load when what was read breaks it.
fn made(out: &mut String, indent: &str, make: &str, all: &[Field]) {
    if all.iter().all(|field| field.rule.is_none()) {
        let _ = writeln!(out, "{indent}{make}({})", given(all));
        return;
    }
    let value = make.strip_prefix("return ").unwrap_or(make);
    let _ = writeln!(out, "{indent}match {value}({}):", given(all));
    let _ = writeln!(out, "{indent}    case Ok(value__):\n{indent}        return value__");
    let _ =
        writeln!(out, "{indent}    case Err(broken__):\n{indent}        fail src__.rule(broken__.message)");
}

fn load_struct(name: &str, all: &[Field], fields: &Fields<'_>) -> String {
    let mut out = format!("        src__.record(\"{name}\")?\n");
    load_fields(&mut out, "        ", &constant(name, ""), fields);
    made(&mut out, "        ", "Self", all);
    out
}

fn load_case(
    text: &mut String,
    out: &mut String,
    name: &str,
    (at, case): (usize, &str),
    (all, fields): (&[Field], &Fields<'_>),
) {
    let _ = writeln!(out, "            case {at}:");
    let indent = "                ";
    if all.is_empty() {
        let _ = writeln!(out, "{indent}return {name}.{case}");
        return;
    }
    let list = constant(name, case);
    let _ = writeln!(text, "{list} = [{}]\n", field_list(all));
    let _ = writeln!(out, "{indent}src__.record(\"{case}\")?");
    load_fields(out, indent, &list, fields);
    let _ = writeln!(out, "{indent}src__.end_case()?");
    made(out, indent, &format!("return {name}.{case}"), all);
}

/// Puts each field's default where the generated code names it, as it's written in the type.
fn fill_in(parsed: &mut Module, def: &TypeDef) {
    let fields: Vec<&Field> =
        def.fields.iter().chain(def.cases.iter().flat_map(|case| &case.fields)).collect();
    fennec_syntax::visit::exprs_mut(parsed, &mut |expr: &mut Expr| {
        let ExprKind::Name(name) = &expr.kind else { return };
        let Some(wanted) = name.strip_prefix("DEFAULT_") else { return };
        if let Some(default) =
            fields.iter().find(|field| field.name == wanted).and_then(|field| field.default.as_ref())
        {
            *expr = default.clone();
        }
    });
}

/// The first thing in a value of this type that stops it being saved: where it is, what it is, the
/// field to `@skip`, and the type to write the methods for.
pub(crate) fn what_stops_saving(
    checker: &mut Checker,
    ty: &Type,
) -> Option<(String, String, String, String)> {
    let name = ty.type_name()?.to_owned();
    stops_in(checker, &name, &name, &mut Vec::new())
}

fn stops_in(
    checker: &mut Checker,
    name: &str,
    path: &str,
    open: &mut Vec<String>,
) -> Option<(String, String, String, String)> {
    if open.iter().any(|seen| seen == name) || has_both(checker, name) {
        return None;
    }
    let def = checker.program.type_info(name)?.def.clone();
    open.push(name.to_owned());
    let fields = def.fields.iter().map(|field| (None, field));
    let in_cases =
        def.cases.iter().flat_map(|case| case.fields.iter().map(|field| (Some(case.name.as_str()), field)));
    let all: Vec<(Option<&str>, &Field)> =
        fields.chain(in_cases).filter(|(_, field)| !skipped(field)).collect();
    let mut found = None;
    for (case, field) in all {
        let ty = checker.field_type(name, case, &field.name);
        let place = format!("{path}.{}", field.name);
        found = field_stops(checker, &ty, &place, (&field.name, name), open);
        if found.is_some() {
            break;
        }
    }
    open.pop();
    found
}

fn field_stops(
    checker: &mut Checker,
    ty: &Type,
    place: &str,
    (field, owner): (&str, &str),
    open: &mut Vec<String>,
) -> Option<(String, String, String, String)> {
    let stop = |what: String| Some((place.to_owned(), what, field.to_owned(), owner.to_owned()));
    match ty {
        Type::Int | Type::Float | Type::Bool | Type::Str | Type::Param(_) | Type::Unknown => None,
        Type::List(inner) | Type::Set(inner) | Type::Optional(inner) => {
            field_stops(checker, inner, place, (field, owner), open)
        }
        Type::Dict(key, value) => field_stops(checker, key, place, (field, owner), open)
            .or_else(|| field_stops(checker, value, place, (field, owner), open)),
        Type::Function(_) | Type::Arrow(..) => stop("a function".to_owned()),
        Type::Named(_) | Type::Generic(..) if checker.program.is_shared(ty) => stop("`shared`".to_owned()),
        Type::Named(name) | Type::Generic(name, _) if checker.program.type_info(name).is_some() => {
            stops_in(checker, name, place, open)
        }
        other => stop(crate::types::a(other)),
    }
}
