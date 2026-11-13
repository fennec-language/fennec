//! A type saves and loads through `def serialize` and `def deserialize`, which most types never
//! write: they're generated here from its fields, as code like any other, for the types whose
//! fields can all be saved. It only happens in a program that uses saving, so others check as if
//! it weren't there. A type that writes either one keeps what it wrote.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use fennec_syntax::{Expr, ExprKind, Field, Item, Module, Pattern, TypeDef};

use crate::checker::Checker;
use crate::types::Type;

/// The modules that save through these methods.
const SAVING_MODULES: [&str; 5] = ["json", "toml", "pack", "sql", "sqlite"];

/// The standard library's types that are never saved, though their fields could be: a `Secret`,
/// whose text mustn't leave the program that way, an `Instant`, which means nothing outside
/// the run that read it, and an `Html`, which loading would make from text `html.render` never saw.
/// Each is named as a message says it, with what to save instead.
const NEVER_SAVED: [(&str, &str, &str); 3] = [
    (
        "Secret",
        "a `Secret`",
        "a secret never leaves the program by being saved: save how to get it again, like the name of the variable it's read from",
    ),
    (
        "Instant",
        "an `Instant`",
        "an `Instant` means nothing outside this run: save a `Time`, from `clock.time()`",
    ),
    (
        "Html",
        "an `Html`",
        "a page loaded from saved text wasn't made by `html.render`: save what it's made from, and render it again",
    ),
];

/// The capabilities, through which all I/O goes, each named as a message says it. A capability is
/// handed down from `main`, never made: loading one from text would make one nobody handed over,
/// and saving one means nothing.
const CAPABILITIES: [(&str, &str); 11] = [
    ("Stdout", "a `Stdout`"),
    ("Stderr", "a `Stderr`"),
    ("Stdin", "a `Stdin`"),
    ("Fs", "an `Fs`"),
    ("Net", "a `Net`"),
    ("Clock", "a `Clock`"),
    ("Args", "an `Args`"),
    ("Process", "a `Process`"),
    ("Entropy", "an `Entropy`"),
    ("Env", "an `Env`"),
    ("Signals", "a `Signals`"),
];

/// What to do instead of saving a capability.
const CAPABILITY_INSTEAD: &str =
    "a capability is handed down from `main`, never saved or loaded: pass it to the code that needs it";

/// How a message names a standard library type that's never saved, if it's one, and what to save
/// instead.
pub(crate) fn never_saved(name: &str) -> Option<(&'static str, &'static str)> {
    NEVER_SAVED.iter().find(|(never, ..)| *never == name).map(|(_, what, instead)| (*what, *instead)).or_else(
        || {
            CAPABILITIES
                .iter()
                .find(|(capability, _)| *capability == name)
                .map(|(_, what)| (*what, CAPABILITY_INSTEAD))
        },
    )
}

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
        .filter(|def| !written.contains(&def.name) && !has_method(def) && never_saved(&def.name).is_none())
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
    let mut unions = Unions::default();
    let mut written = Written::default();
    let mut made = Vec::new();
    for (number, def) in types.iter().filter(|def| generated.contains(&def.name)).enumerate() {
        if let Some(items) = pair_for((&mut checker, &mut unions, &mut written), number, def) {
            made.extend(items);
        }
    }
    if !unions.text.is_empty()
        && let Ok(parsed) = fennec_syntax::parse_prelude(&unions.text)
    {
        made.extend(parsed.items);
    }
    // Every slot is put in at once, since a type's pair names the functions its unions are saved
    // through, which are written last.
    if written.fill(&mut made).is_some() {
        module.items.extend(made);
    }
}

pub(crate) fn uses_saving(module: &Module) -> bool {
    // A program's own interface by one of these names is its own business.
    let own = module.items.iter().any(|item| match item {
        Item::Interface(interface) => SAVING_NAMES.contains(&interface.name.as_str()),
        _ => false,
    });
    if own {
        return false;
    }
    let imported = module.items.iter().any(|item| match item {
        // `import std.json`, `from std.json import loads`, or `from std import json`.
        Item::Import(import) => match import.module.as_slice() {
            [std, name, ..] if std == "std" => SAVING_MODULES.contains(&name.as_str()),
            [std] if std == "std" => import.bound().any(|(name, _)| SAVING_MODULES.contains(&name)),
            _ => false,
        },
        _ => false,
    });
    if imported {
        return true;
    }
    let mut named = false;
    let mut walked = module.clone();
    fennec_syntax::visit::exprs_mut(&mut walked, &mut |expr| {
        named |= match &expr.kind {
            ExprKind::Name(name) => SAVING_NAMES.contains(&name.as_str()),
            // `response.json[T]()` reads what an HTTP call gave back as a `T`.
            ExprKind::Attr { name, .. } => SAVING_NAMES.contains(&name.as_str()) || name == "json",
            // `net.post(url, json=value)` writes the value as JSON.
            ExprKind::Call { args, .. } => args.iter().any(|arg| arg.name.as_deref() == Some("json")),
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
pub(crate) fn saved(checker: &Checker, ty: &Type, generated: &BTreeSet<String>) -> bool {
    match ty {
        Type::Int | Type::Float | Type::Bool | Type::Str | Type::Bytes | Type::Param(_) | Type::Id(_) => true,
        // A union is saved through a type made for it, which a set's items and a dict's keys
        // can't be changed to and from.
        Type::Set(inner) | Type::Table(inner) if holds_union(inner) => false,
        Type::Dict(key, _) if holds_union(key) => false,
        Type::List(inner) | Type::Set(inner) | Type::Optional(inner) | Type::Table(inner) => {
            saved(checker, inner, generated)
        }
        Type::Dict(key, value) => saved(checker, key, generated) && saved(checker, value, generated),
        // A tuple is saved as an array of its items (see `tuple_lowering`).
        // One of a single item can't be (see `Generic::fits_interface`).
        Type::Tuple(items) => {
            items.len() > 1
                && !items.iter().any(holds_union)
                && items.iter().all(|item| saved(checker, item, generated))
        }
        Type::Union(members) => {
            members.iter().all(|member| tested_by_name(member) && saved(checker, member, generated))
        }
        Type::Named(name) | Type::Generic(name, _) if never_saved(name).is_some() => false,
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

/// What the saving functions' text names by a slot of its own, put in once it's parsed, so nothing
/// a program names is ever read again: each type, as a written type built from it (see `slots`);
/// each string that holds a name, as `@serialize(name=...)` gives one; and each field's name where
/// code names the field, as `self.if` would be, since a field may be named by any word, keywords
/// included. A local holding a field's value keeps its slot's name.
#[derive(Default)]
struct Written {
    types: Vec<(String, Type)>,
    texts: Vec<(String, String)>,
    fields: Vec<(String, String)>,
}

/// What stands for the type a pair is written for, where it's extended.
const OWNER: &str = "Owner__slot";

impl Written {
    /// The slot standing for a type.
    fn ty(&mut self, ty: &Type) -> String {
        if let Some((slot, _)) = self.types.iter().find(|(_, held)| held == ty) {
            return slot.clone();
        }
        let slot = format!("Type{}__slot", self.types.len());
        self.types.push((slot.clone(), ty.clone()));
        slot
    }

    /// A string literal holding `text`.
    fn text(&mut self, text: &str) -> String {
        let slot = format!("text{}__slot", self.texts.len());
        self.texts.push((slot.clone(), text.to_owned()));
        format!("\"{slot}\"")
    }

    /// The slot standing for a field's name.
    fn field(&mut self, name: &str) -> String {
        if let Some((slot, _)) = self.fields.iter().find(|(_, held)| held == name) {
            return slot.clone();
        }
        let slot = format!("field{}__slot", self.fields.len());
        self.fields.push((slot.clone(), name.to_owned()));
        slot
    }

    /// Puts what each slot stands for in, in each item. `None` when a type has no written form.
    fn fill(&self, items: &mut [Item]) -> Option<()> {
        let types: Vec<(&str, Type)> =
            self.types.iter().map(|(slot, ty)| (slot.as_str(), ty.clone())).collect();
        for item in items {
            match item {
                Item::Function(def) => self.fill_function(def, &types)?,
                Item::Extend(extension) => {
                    for method in &mut extension.methods {
                        self.fill_function(method, &types)?;
                    }
                }
                Item::Struct(def) => {
                    for ty in def.fields.iter_mut().filter_map(|field| field.ty.as_mut()) {
                        crate::slots::fill_type(ty, &types)?;
                    }
                }
                Item::Const { value, .. } => {
                    fennec_syntax::visit::in_expr(value, &mut |expr| self.texts_in(expr));
                }
                _ => {}
            }
        }
        Some(())
    }

    fn fill_function(&self, def: &mut fennec_syntax::FnDef, types: &[(&str, Type)]) -> Option<()> {
        crate::slots::fill(def, types)?;
        fennec_syntax::visit::in_function(def, &mut |expr| {
            self.texts_in(expr);
            match &mut expr.kind {
                // `self.if`: the field, by its own name.
                ExprKind::Attr { name, .. } => self.field_named(name),
                // `Self(if=...)`: the field given, by its own name, from the local holding it.
                ExprKind::Call { args, .. } => {
                    for name in args.iter_mut().filter_map(|arg| arg.name.as_mut()) {
                        self.field_named(name);
                    }
                }
                // `case .Gate(if=field0__slot)`: the case's field, read into the local.
                ExprKind::Match { arms, .. } => {
                    for arm in arms {
                        if let Pattern::Variant { fields, .. } = &mut arm.pattern {
                            for name in fields {
                                self.field_named(name);
                            }
                        }
                    }
                }
                _ => {}
            }
        });
        Some(())
    }

    fn texts_in(&self, expr: &mut Expr) {
        if let ExprKind::Str(written) = &expr.kind
            && let Some((_, text)) = self.texts.iter().find(|(slot, _)| slot == written)
        {
            expr.kind = ExprKind::Str(text.clone());
        }
    }

    fn field_named(&self, name: &mut String) {
        if let Some((_, field)) = self.fields.iter().find(|(slot, _)| slot == name) {
            field.clone_into(name);
        }
    }
}

/// One field to read: the slot standing for its type, and whether it holds an optional already.
/// One that holds a union is saved as what `Unions` makes for its type.
struct Read<'f> {
    field: &'f Field,
    /// The local holding its value.
    local: String,
    ty: String,
    /// Its type made optional, when it isn't one already.
    or_none: String,
    optional: bool,
    union: Option<Changed>,
}

impl Read<'_> {
    /// The field's value as it's saved.
    fn saved(&self, value: &str) -> String {
        match &self.union {
            Some(changed) => format!("saved_union__{}({value})", changed.helper),
            None => value.to_owned(),
        }
    }

    /// Reads the field's value.
    fn read(&self) -> String {
        match &self.union {
            Some(changed) => {
                format!("loaded_union__{}(src__.value[{}]()?)", changed.helper, changed.saved_as)
            }
            None => format!("src__.value[{}]()?", self.ty),
        }
    }
}

/// A type that holds a union, as it's saved: `saved_as`, the type it's changed to, by the
/// functions numbered `helper`.
#[derive(Clone)]
struct Changed {
    saved_as: String,
    helper: usize,
}

/// What a program's unions are saved through: for each union, a struct holding one, which saves
/// it tagged with its member, and for each type holding one, functions to change it to and from
/// the type it's saved as.
#[derive(Default)]
struct Unions {
    text: String,
    wrappers: BTreeMap<String, usize>,
    helpers: BTreeMap<String, Changed>,
}

impl Unions {
    /// How a value of `ty` is saved, when it holds a union.
    fn change(&mut self, written: &mut Written, ty: &Type) -> Option<Changed> {
        if !holds_union(ty) {
            return None;
        }
        let key = ty.to_string();
        if let Some(changed) = self.helpers.get(&key) {
            return Some(changed.clone());
        }
        let (saved_as, saving, loading) = match ty {
            Type::Union(members) => {
                let wrapper = self.wrapper(written, ty, members)?;
                (wrapper.clone(), format!("return {wrapper}(value=x__)"), "return x__.value".to_owned())
            }
            Type::Optional(inner) => {
                let Changed { saved_as, helper } = self.change(written, inner)?;
                let saving = format!("y__ = x__ else return None\n    return saved_union__{helper}(y__)");
                let loading = format!("y__ = x__ else return None\n    return loaded_union__{helper}(y__)");
                (format!("{saved_as}?"), saving, loading)
            }
            Type::List(inner) => {
                let Changed { saved_as, helper } = self.change(written, inner)?;
                let each = |way: &str| format!("return [{way}_union__{helper}(item__) for item__ in x__]");
                (format!("list[{saved_as}]"), each("saved"), each("loaded"))
            }
            Type::Dict(key, value) => {
                let Changed { saved_as, helper } = self.change(written, value)?;
                let each = |way: &str| {
                    format!(
                        "return {{key__: {way}_union__{helper}(value__) for key__, value__ in x__.items()}}"
                    )
                };
                (format!("dict[{}, {saved_as}]", written.ty(key)), each("saved"), each("loaded"))
            }
            _ => return None,
        };
        let helper = self.helpers.len();
        let source = written.ty(ty);
        let _ = writeln!(
            self.text,
            "def saved_union__{helper}(x__: {source}) -> {saved_as}:\n    {saving}\n\ndef loaded_union__{helper}(x__: {saved_as}) -> {source}:\n    {loading}\n"
        );
        let changed = Changed { saved_as, helper };
        self.helpers.insert(key, changed.clone());
        Some(changed)
    }

    /// The struct a union is saved through: its member, by the member's type's name and where it
    /// is among the members, in the union's own order, then the member's own value.
    fn wrapper(&mut self, written: &mut Written, ty: &Type, members: &[Type]) -> Option<String> {
        let key = ty.to_string();
        let number = match self.wrappers.get(&key) {
            Some(&number) => return Some(format!("SavedUnion__{number}")),
            None => self.wrappers.len(),
        };
        self.wrappers.insert(key, number);
        let name = format!("SavedUnion__{number}");
        // Each member is saved under its name as the program writes it.
        let shown: Vec<String> = members
            .iter()
            .map(|member| member.source().map(|source| fennec_syntax::rename::shown_key(&source)))
            .collect::<Option<_>>()?;
        let listed: Vec<String> = shown.iter().map(|member| written.text(member)).collect();
        let mut saving = String::new();
        let mut loading = format!("        at__ = src__.member([{}])?\n", listed.join(", "));
        for (at, (member, shown)) in members.iter().zip(&shown).enumerate() {
            let member = written.ty(member);
            let shown = written.text(shown);
            // Each test on its own, so each sees every member.
            let _ = writeln!(
                saving,
                "        if self.value is {member}:\n            out__.member({shown}, {at})\n            out__.value(self.value)"
            );
            let read = format!(
                "value__ = src__.value[{member}]()?\n            src__.end_member()?\n            return {name}(value=value__)"
            );
            if at + 1 < members.len() {
                let _ = writeln!(loading, "        if at__ == {at}:\n            {read}");
            } else {
                let read = read.replace("\n    ", "\n");
                let _ = writeln!(loading, "        {read}");
            }
        }
        let _ = writeln!(saving, "        out__.end_member()");
        let _ = writeln!(
            self.text,
            "struct {name}:\n    value: {}\n\nextend {name}:\n    def serialize(self, ref out__: Serializer):\n{saving}\n    def deserialize(ref src__: Deserializer) -> {name} fails ParseError:\n{loading}",
            written.ty(ty)
        );
        Some(name)
    }
}

/// Whether a type is a union, or holds one.
pub(crate) fn holds_union(ty: &Type) -> bool {
    match ty {
        Type::Union(_) => true,
        Type::List(inner) | Type::Set(inner) | Type::Optional(inner) | Type::Table(inner) => {
            holds_union(inner)
        }
        Type::Dict(key, value) => holds_union(key) || holds_union(value),
        _ => false,
    }
}

/// Whether a union's member can be told apart by its name alone, as `value is Circle` does.
pub(crate) fn tested_by_name(member: &Type) -> bool {
    match member {
        Type::Int | Type::Float | Type::Bool | Type::Str | Type::Bytes => true,
        Type::Named(name) => !name.contains('.'),
        _ => false,
    }
}

/// What loading a type's fields needs: those it reads, and those it leaves out, with the slot of
/// each one's type, when it has a written form.
type Fields<'f> = (Vec<Read<'f>>, Vec<(&'f Field, Option<String>)>);

fn reads<'f>(
    (checker, unions, written): (&mut Checker, &mut Unions, &mut Written),
    owner: &str,
    case: Option<&str>,
    fields: &'f [Field],
) -> Option<Fields<'f>> {
    let mut saved = Vec::new();
    let mut left_out = Vec::new();
    for field in fields {
        let held = checker.field_type(owner, case, &field.name);
        if skipped(field) {
            left_out.push((field, held.source().map(|_| written.ty(&held))));
        } else {
            held.source()?;
            let optional = matches!(held, Type::Optional(_));
            let union = unions.change(written, &held);
            let or_none = written.ty(&Type::optional(held.clone()));
            let ty = written.ty(&held);
            let local = written.field(&field.name);
            saved.push(Read { field, local, ty, or_none, optional, union });
        }
    }
    Some((saved, left_out))
}

/// The constant and the extension for the type numbered `number`, parsed, with their slots put in
/// by `written` once every type's are written.
fn pair_for(
    (checker, unions, written): (&mut Checker, &mut Unions, &mut Written),
    number: usize,
    def: &TypeDef,
) -> Option<Vec<Item>> {
    let name = &def.name;
    let owner = written.ty(&Type::named(name));
    let shown = written.text(name);
    let mut text = String::new();
    let (saving, loading) = if def.cases.is_empty() {
        let fields = reads((checker, unions, written), name, None, &def.fields)?;
        let list = field_list(written, &def.fields);
        let _ = writeln!(text, "{} = [{list}]\n", constant(number, None));
        let saving = save_struct(written, &shown, &fields.0);
        (saving, load_struct((written, &shown), number, &def.fields, &fields))
    } else {
        let cases: Vec<String> = def
            .cases
            .iter()
            .map(|case| {
                format!(
                    "({}, {})",
                    written.text(&case.name),
                    if case.fields.is_empty() { "False" } else { "True" }
                )
            })
            .collect();
        let _ = writeln!(text, "{} = [{}]\n", constant(number, None), cases.join(", "));
        let mut saving = String::from("        match self:\n");
        let mut loading = format!("        match src__.enum_case({shown}, {})?:\n", constant(number, None));
        for (at, case) in def.cases.iter().enumerate() {
            let fields = reads((checker, unions, written), name, Some(&case.name), &case.fields)?;
            save_case(written, &mut saving, &shown, (at, &case.name), &fields.0);
            load_case(
                written,
                (&mut text, &mut loading),
                (number, &owner),
                (at, &case.name),
                (&case.fields, &fields),
            );
        }
        let _ = writeln!(
            loading,
            "            case _:\n                fail src__.rule({})",
            written.text(&format!("`{name}` has no such case"))
        );
        (saving, loading)
    };
    let bounds = bounded(def, "Serializable");
    let _ =
        writeln!(text, "extend {OWNER}{bounds}:\n    def serialize(self, ref out__: Serializer):\n{saving}");
    let bounds = bounded(def, "Deserializable");
    let _ = writeln!(
        text,
        "extend {OWNER}{bounds}:\n    def deserialize(ref src__: Deserializer) -> Self fails ParseError:\n{loading}"
    );
    let mut parsed = fennec_syntax::parse_prelude(&text).ok()?;
    for item in &mut parsed.items {
        if let Item::Extend(extension) = item {
            name.clone_into(&mut extension.name);
        }
    }
    fill_in(&mut parsed, written, def);
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

/// The constant listing the fields of the type numbered `number`, or of one of its cases, by
/// where the case is among them.
fn constant(number: usize, case: Option<usize>) -> String {
    match case {
        Some(case) => format!("SAVED_NAMES_{number}_{case}"),
        None => format!("SAVED_NAMES_{number}_"),
    }
}

fn field_list(written: &mut Written, fields: &[Field]) -> String {
    let mut listed = Vec::new();
    for field in fields.iter().filter(|field| !skipped(field)) {
        let shown = written.text(renamed(field).unwrap_or(&field.name));
        listed.push(format!("({shown}, {})", written.text(&field.name)));
    }
    for field in fields.iter().filter(|field| skipped(field)) {
        listed.push(format!("(\"\", {})", written.text(&field.name)));
    }
    listed.join(", ")
}

fn save_fields(
    written: &mut Written,
    out: &mut String,
    indent: &str,
    fields: &[Read<'_>],
    value: impl Fn(&Read<'_>) -> String,
) {
    for read in fields {
        let saved = read.saved(&value(read));
        let _ = match renamed(read.field) {
            Some(shown) => writeln!(out, "{indent}out__.renamed({}, {saved})", written.text(shown)),
            None => writeln!(out, "{indent}out__.field({}, {saved})", written.text(&read.field.name)),
        };
    }
    let _ = writeln!(out, "{indent}out__.end()");
}

fn save_struct(written: &mut Written, shown: &str, fields: &[Read<'_>]) -> String {
    let mut out = format!("        out__.record({shown}, {})\n", fields.len());
    save_fields(written, &mut out, "        ", fields, |read| format!("self.{}", read.local));
    out
}

/// Writes a case by its name, and by `at`, where it is among the type's cases, for a format that
/// saves no names.
fn save_case(
    written: &mut Written,
    out: &mut String,
    shown: &str,
    (at, case): (usize, &str),
    fields: &[Read<'_>],
) {
    let case_shown = written.text(case);
    if fields.is_empty() {
        let _ = writeln!(
            out,
            "            case .{case}:\n                out__.enum_case({shown}, {case_shown}, {at})"
        );
        return;
    }
    // Each field by its own name, read into the local that holds it.
    let names: Vec<String> = fields.iter().map(|read| format!("{0}={0}", read.local)).collect();
    let _ = writeln!(out, "            case .{case}({}):", names.join(", "));
    let _ = writeln!(out, "                out__.case_record({shown}, {case_shown}, {at}, {})", fields.len());
    save_fields(written, out, "                ", fields, |read| read.local.clone());
    let _ = writeln!(out, "                out__.end()");
}

/// Reads fields into names of their own, each what the type's constructor is given.
fn load_fields(
    written: &mut Written,
    out: &mut String,
    indent: &str,
    list: &str,
    (fields, left_out): &Fields<'_>,
) {
    for read in fields {
        let _ = writeln!(out, "{indent}mut read_{}: {} = None", read.local, read.or_none);
        // An optional field's own None can't be told from one not read, so whether it was read
        // is kept apart, and a None read stays None rather than taking the default.
        if read.optional && read.field.default.is_some() {
            let _ = writeln!(out, "{indent}mut seen_{} = False", read.local);
        }
    }
    let _ = writeln!(out, "{indent}while src__.next_field({list})?:\n{indent}    match src__.field_at():");
    for (at, read) in fields.iter().enumerate() {
        let local = &read.local;
        let _ = writeln!(out, "{indent}        case {at}:");
        if read.optional {
            let _ = writeln!(out, "{indent}            read_{local} = {}", read.read());
            if read.field.default.is_some() {
                let _ = writeln!(out, "{indent}            seen_{local} = True");
            }
            continue;
        }
        // A format whose saved data can say a value isn't there, as pack's does for a field that
        // was optional when it was saved, may give one for any field, which then takes its
        // default. JSON's and SQL's null is a value, which a field that isn't optional can't
        // hold, so they say no. Either way the value is read as what the field holds, never
        // wrapped as an optional on the way.
        let _ = writeln!(
            out,
            "{indent}            if src__.none_next() and src__.none()?:\n{indent}                read_{local} = None\n{indent}            else:\n{indent}                read_{local} = {}",
            read.read()
        );
    }
    let _ = writeln!(out, "{indent}        case _:\n{indent}            pass");
    for read in fields {
        let local = &read.local;
        if read.field.default.is_some() && read.optional {
            // Read, it holds what was read, None included; left out, it takes its default.
            let _ = writeln!(
                out,
                "{indent}mut {local}: {} = DEFAULT_{local}\n{indent}if seen_{local}: {local} = read_{local}",
                read.ty
            );
        } else if read.field.default.is_some() && read.union.is_some() {
            // A default that's one of a union's members is given as the union.
            let _ = writeln!(
                out,
                "{indent}mut {local}: {} = DEFAULT_{local}\n{indent}if read_{local} is not None: {local} = read_{local}",
                read.ty
            );
        } else if read.field.default.is_some() {
            let _ = writeln!(out, "{indent}{local} = read_{local} else DEFAULT_{local}");
        } else if read.optional {
            let _ = writeln!(out, "{indent}{local} = read_{local}");
        } else {
            let shown = written.text(renamed(read.field).unwrap_or(&read.field.name));
            let _ = writeln!(out, "{indent}if read_{local} is None: fail src__.missing({shown})");
            let _ = writeln!(out, "{indent}{local} = read_{local}");
        }
    }
    for (field, ty) in left_out {
        let local = written.field(&field.name);
        let _ = match ty {
            Some(ty) => writeln!(out, "{indent}{local}: {ty} = DEFAULT_{local}"),
            None => writeln!(out, "{indent}{local} = DEFAULT_{local}"),
        };
    }
}

/// Each field given to the type's constructor by its own name, from the local holding it.
fn given(written: &mut Written, all: &[Field]) -> String {
    all.iter().map(|field| format!("{0}={0}", written.field(&field.name))).collect::<Vec<_>>().join(", ")
}

/// Gives back the value. One with a `where` rule fails to load when what was read breaks it.
fn made(written: &mut Written, out: &mut String, indent: &str, make: &str, all: &[Field]) {
    let given = given(written, all);
    if all.iter().all(|field| field.rule.is_none()) {
        let _ = writeln!(out, "{indent}return {make}({given})");
        return;
    }
    let _ = writeln!(out, "{indent}value__ = {make}({given}) except broken__:");
    let _ = writeln!(out, "{indent}    fail src__.rule(broken__.message)\n{indent}return value__");
}

/// Reads fields that come in order, every one there, as a format that saves no names gives them,
/// each straight into a name of its own, then gives back the value made of them.
fn load_in_order(
    written: &mut Written,
    out: &mut String,
    indent: &str,
    (shown, make, case): (&str, &str, bool),
    all: &[Field],
    fields: &Fields<'_>,
) {
    let (reads, left_out) = fields;
    let _ = writeln!(out, "{indent}if src__.ordered({shown})?:");
    for read in reads {
        let _ = writeln!(out, "{indent}    in_{} = {}", read.local, read.read());
    }
    let _ = writeln!(out, "{indent}    src__.end_ordered()");
    if case {
        let _ = writeln!(out, "{indent}    src__.end_case()?");
    }
    let given: Vec<String> = all
        .iter()
        .map(|field| {
            let local = written.field(&field.name);
            if left_out.iter().any(|(gone, _)| gone.name == field.name) {
                format!("{local}=DEFAULT_{local}")
            } else {
                format!("{local}=in_{local}")
            }
        })
        .collect();
    if all.iter().all(|field| field.rule.is_none()) {
        let _ = writeln!(out, "{indent}    return {make}({})", given.join(", "));
        return;
    }
    let _ = writeln!(out, "{indent}    value__ = {make}({}) except broken__:", given.join(", "));
    let _ = writeln!(out, "{indent}        fail src__.rule(broken__.message)\n{indent}    return value__");
}

fn load_struct(
    (written, shown): (&mut Written, &str),
    number: usize,
    all: &[Field],
    fields: &Fields<'_>,
) -> String {
    let mut out = String::new();
    load_in_order(written, &mut out, "        ", (shown, "Self", false), all, fields);
    let _ = writeln!(out, "        src__.record({shown})?");
    load_fields(written, &mut out, "        ", &constant(number, None), fields);
    made(written, &mut out, "        ", "Self", all);
    out
}

fn load_case(
    written: &mut Written,
    (text, out): (&mut String, &mut String),
    (number, owner): (usize, &str),
    (at, case): (usize, &str),
    (all, fields): (&[Field], &Fields<'_>),
) {
    let _ = writeln!(out, "            case {at}:");
    let indent = "                ";
    if all.is_empty() {
        let _ = writeln!(out, "{indent}return {owner}.{case}");
        return;
    }
    let list = constant(number, Some(at));
    let _ = writeln!(text, "{list} = [{}]\n", field_list(written, all));
    let case_shown = written.text(case);
    load_in_order(written, out, indent, (&case_shown, &format!("{owner}.{case}"), true), all, fields);
    let _ = writeln!(out, "{indent}src__.record({case_shown})?");
    load_fields(written, out, indent, &list, fields);
    let _ = writeln!(out, "{indent}src__.end_case()?");
    made(written, out, indent, &format!("{owner}.{case}"), all);
}

/// Puts each field's default where the generated code names it, as it's written in the type.
fn fill_in(parsed: &mut Module, written: &Written, def: &TypeDef) {
    let fields: Vec<&Field> =
        def.fields.iter().chain(def.cases.iter().flat_map(|case| &case.fields)).collect();
    fennec_syntax::visit::exprs_mut(parsed, &mut |expr: &mut Expr| {
        let ExprKind::Name(name) = &expr.kind else { return };
        let Some(slot) = name.strip_prefix("DEFAULT_") else { return };
        let Some((_, wanted)) = written.fields.iter().find(|(held, _)| held == slot) else { return };
        if let Some(default) =
            fields.iter().find(|field| field.name == *wanted).and_then(|field| field.default.as_ref())
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
    // What a collection or an optional holds is what stops it, as `Note` does in `list[Note]`.
    match ty {
        Type::List(inner) | Type::Set(inner) | Type::Optional(inner) | Type::Table(inner) => {
            return what_stops_saving(checker, inner);
        }
        Type::Dict(key, value) => {
            return what_stops_saving(checker, key).or_else(|| what_stops_saving(checker, value));
        }
        _ => {}
    }
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
        Type::Int
        | Type::Float
        | Type::Bool
        | Type::Str
        | Type::Bytes
        | Type::Param(_)
        | Type::Unknown
        | Type::Id(_) => None,
        Type::Set(inner) | Type::Table(inner) if holds_union(inner) => stop(crate::types::a(ty)),
        Type::Dict(key, _) if holds_union(key) => stop(crate::types::a(ty)),
        Type::List(inner) | Type::Set(inner) | Type::Optional(inner) | Type::Table(inner) => {
            field_stops(checker, inner, place, (field, owner), open)
        }
        Type::Dict(key, value) => field_stops(checker, key, place, (field, owner), open)
            .or_else(|| field_stops(checker, value, place, (field, owner), open)),
        Type::Function(_) | Type::Arrow(..) => stop("a function".to_owned()),
        Type::Union(members) if members.iter().all(tested_by_name) => {
            members.iter().find_map(|member| field_stops(checker, member, place, (field, owner), open))
        }
        Type::Named(_) | Type::Generic(..) if checker.program.is_shared(ty) => stop("`shared`".to_owned()),
        Type::Named(name) | Type::Generic(name, _) if let Some((what, _)) = never_saved(name) => {
            stop(what.to_owned())
        }
        Type::Named(name) | Type::Generic(name, _) if checker.program.type_info(name).is_some() => {
            stops_in(checker, name, place, open)
        }
        other => stop(crate::types::a(other)),
    }
}
