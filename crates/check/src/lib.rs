//! Fennec's type checker. It finds mistakes before a program runs (unknown names, changes to
//! names that aren't `mut`, wrong arguments, ignored errors, values that might be None,
//! `match`es that miss a case) and works out every function's signature. Every `def` writes
//! the type it gives back, and one with no arrow gives back nothing; the errors a function
//! can fail with are inferred.
//!
//! Each function is checked on its own, against the signatures of the functions it calls.
//! A function is checked the first time something needs its signature, which is when its
//! errors become known.
//!
//! Every type in a program that checks is known. Where the checker can't tell a type from
//! the code, like the items of an empty list nothing is added to, it says what to write. So
//! besides the signatures, a check gives the type of every expression and every name: the
//! typed tree a compiler builds on. See [`Types`].

// A hash map's order changes with every process, so iterating one would make what the compiler
// emits, or the order it checks in, differ from run to run. Iterate a sorted copy or a BTreeMap.
#![deny(clippy::iter_over_hash_type)]

mod annotations;
mod builtins;
mod call;
mod checker;
mod codec;
mod constants;
pub mod copies;
mod crossing;
mod effects;
mod enumerate_lowering;
mod every;
mod except;
mod except_fix;
mod expect;
mod expr;
mod extend;
pub use crossing::{Crossing, with_crossing};
pub use extend::method_key;
pub use shapes::SavedAs;
mod fields;
mod function;
pub use fields::{each_field_type, fields_of};
mod generic;
mod generic_copies;
mod gone;
mod holds;
mod imports;
mod index;
mod infer;
mod keys;
mod launch;
mod lend;
mod lend_lowering;
mod lent;
mod lock;
mod lock_lowering;
mod loops;
mod lost;
mod messages;
mod moved;
mod named;
mod once;
mod optional_messages;
mod parallel;
mod pattern;
mod place;
mod plain;
mod privacy;
mod program;
mod protocols;
mod reach;
mod ref_lowering;
mod results_fix;
mod routes;
mod rules;
mod saving;
mod secrets;
mod shapes;
mod shared;
mod slots;
mod stdlib;
mod stmt;
mod sum;
mod threading;
pub use threading::add_capability_fixes;
mod tuple_lowering;
mod types;
mod union_sources;
mod waits;

use std::collections::{BTreeMap, BTreeSet, HashSet};

use fennec_syntax::{Diagnostic, Expr, ExprKind, Item, Module, Span};

pub use except_fix::{migration_fixes, reserved_name, std_migration_fixes, syntax_migration_fixes};
pub use messages::missing_method;
pub use named::add_pattern_fixes;
pub use privacy::add_rename_fixes;
pub use results_fix::result_fixes;
pub use tuple_lowering::tuple_function;

pub use reach::std_code;

/// Where an import, or a name used, at `span` belongs, among a program's `files`: std's own code,
/// one of the files by its place, or the code lowering writes in, whose imports have no place in a
/// file. A program parsed from one text, with no files listed, is one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Std,
    File(usize),
    Written,
}

/// Where what's at `span` belongs among `files`, by [`Place`].
#[must_use]
pub fn place(files: &[Span], span: Span) -> Place {
    if span.in_prelude() {
        return Place::Std;
    }
    if lend_lowering::not_written_by_hand(span) {
        return Place::Written;
    }
    if files.is_empty() {
        return Place::File(0);
    }
    match files.iter().position(|file| file.start <= span.start && span.start <= file.end) {
        Some(at) => Place::File(at),
        None => Place::Written,
    }
}

/// Where an import at `span` belongs: as [`place`], but one lowering writes in, at no span of
/// its own, is the written-in code's.
#[must_use]
pub fn import_place(files: &[Span], span: Span) -> Place {
    if span.start == span.end && !span.in_prelude() { Place::Written } else { place(files, span) }
}

/// Whether a top-level item declared at `span` is the compiler's: the standard library's, or one
/// the checker wrote into the program, like a type's saving functions. Any code reaches those by
/// name, as it reaches std's; only what a program wrote is kept from std's code and a dependency's.
#[must_use]
pub fn compiler_written(span: Span) -> bool {
    lend_lowering::not_written_by_hand(span)
}

/// Whether code in the function `caller` reaches the top-level function or constant `name` by
/// writing it bare: std's code reaches only std's names, and a dependency's never the program's
/// own. `caller_std` says whether the caller is std's code, and `std` whether `name` is std's.
#[must_use]
pub fn reaches(caller: &str, caller_std: bool, name: &str, std: bool) -> bool {
    reach::Code::of(caller, caller_std).reaches(name, std)
}
pub use types::{Num, Type};

/// Checks a parsed module. Returns the signature of every function and method, or every
/// problem found, in source order.
pub fn check(module: &Module) -> Result<Signatures, Vec<Diagnostic>> {
    check_typed_refusing(module).map(Checked::into_signatures)
}

/// `check_typed`, refusing all `check` refuses: a definition named after a builtin, and the forms
/// of before 2026-11-11, besides what checking finds. A language server shows this, so an editor
/// says what `fennec check` says.
///
/// # Errors
///
/// Every problem found, in source order.
pub fn check_typed_refusing(module: &Module) -> Result<Checked, Vec<Diagnostic>> {
    refuse_reserved(module)?;
    let checked = check_typed(module)?;
    refuse_old_forms(module, &checked)?;
    Ok(checked)
}

/// The forms of before 2026-11-11 a program still has, refused, from what checking it as it was
/// written found. `check_typed` takes them, so `fennec fix` can type them to rewrite them.
/// A top-level definition named after a builtin, refused before anything's checked: one like
/// `interface str` would turn every other message into one about it.
fn refuse_reserved(module: &Module) -> Result<(), Vec<Diagnostic>> {
    let refused = except_fix::reserved_definitions(module);
    if refused.is_empty() { Ok(()) } else { Err(refused) }
}

fn refuse_old_forms(module: &Module, checked: &Checked) -> Result<(), Vec<Diagnostic>> {
    let refused = except_fix::old_forms(module, checked);
    if refused.is_empty() { Ok(()) } else { Err(refused) }
}

/// Like `check`, for a module `check_to_build` has resolved and trimmed to what `main` reaches,
/// about to be lowered: the standard library's functions are checked only when the module
/// reaches them, or lowering uses them without the program naming them.
pub fn check_to_emit(module: &Module) -> Result<Signatures, Vec<Diagnostic>> {
    checked_typed_in(module, checker::Scope::ReachedWithFields).map(Checked::into_signatures)
}

/// What each kit a program depends on is handed, worked out from the whole program as written, by
/// kit and capability, with where each is first handed over: what `fennec update` records in
/// `kit.lock`'s `holds` lines, which checking a program holds it to.
///
/// # Errors
///
/// What the checker finds wrong with the program, besides any kit being handed what `kit.lock`
/// doesn't list for it.
pub fn holds_worked_out(
    module: &Module,
) -> Result<BTreeMap<String, BTreeMap<String, Span>>, Vec<Diagnostic>> {
    let mut working = module.clone();
    working.holds.only_worked_out = true;
    checked_typed_in(&working, checker::Scope::Reached).map(|checked| checked.holds)
}

/// Checks a parsed module. Returns the signatures and the type of every expression and
/// name, or every problem found, in source order.
pub fn check_typed(module: &Module) -> Result<Checked, Vec<Diagnostic>> {
    checked_typed_in(module, checker::Scope::Everything)
}

/// `check_typed`, of the module's own functions and what they reach, and every type's fields,
/// rather than all of the standard library's functions too. What's written in the module has
/// the same types either way, so a language server answering about a place in it needs no more,
/// and checking every standard library function was nine tenths of the cost.
///
/// # Errors
///
/// What the checker finds wrong with the module, in source order.
pub fn check_typed_reached(module: &Module) -> Result<Checked, Vec<Diagnostic>> {
    checked_typed_in(module, checker::Scope::ReachedWithFields)
}

fn checked_typed_in(module: &Module, scope: checker::Scope) -> Result<Checked, Vec<Diagnostic>> {
    library_parses()?;
    let names = own_type_names(module);
    let mut renamed = module.clone();
    hide_interface_params(&mut renamed);
    if names.is_empty() {
        return checker::Checker::new(&renamed).run(scope).map_err(|problems| as_written(problems, &names));
    }
    fennec_syntax::rename::rename_types(&mut renamed, &names);
    checker::Checker::new(&renamed).run(scope).map_err(|problems| as_written(problems, &names))
}

/// What checking the standard library would find were it a program's own code: everything a
/// program is held to, which the library's code is otherwise let off, and the forms of before
/// 2026-11-11 a program's check refuses. A test holds the library to it, so std is written as its
/// users must write.
#[must_use]
pub fn library_problems() -> Vec<Diagnostic> {
    if let Err(problems) = library_parses() {
        return problems;
    }
    let mut found =
        checker::Checker::new(&Module::default()).run(checker::Scope::Library).err().unwrap_or_default();
    found.extend(std_migration_fixes(&Module::default()));
    found
}

/// The types a program declares with the same name as a standard library type, each with the
/// name it's checked under, as `own_type_names` gives them: for what models a run with the
/// checker's types, which name them so.
#[must_use]
pub fn renamed_own_types(module: &Module) -> BTreeMap<String, String> {
    own_type_names(module)
}

/// The types a program declares with the same name as a standard library type, each with the
/// name it's checked under, so the library's code still means its own.
fn own_type_names(module: &Module) -> BTreeMap<String, String> {
    let library: HashSet<&str> = fennec_syntax::prelude_modules()
        .unwrap_or_default()
        .iter()
        .flat_map(|module| &module.items)
        .filter_map(declared_name)
        .collect();
    module
        .items
        .iter()
        .filter_map(declared_name)
        // `Table` and `Id` are built in rather than written in the library, but a program can name
        // its own types that too, and so are the capabilities: an interpreter's `struct Env` is
        // its own type, not the `Env` `main` can be handed.
        .filter(|name| {
            library.contains(name)
                || fennec_syntax::importable_type(name).is_some()
                || shared::is_capability_type(&Type::named(name))
        })
        .map(|name| (name.to_owned(), format!("{name}{}", fennec_syntax::rename::RENAMED)))
        .collect()
}

fn declared_type(item: &Item) -> Option<&str> {
    match item {
        Item::Struct(def) | Item::Enum(def) | Item::Error(def) => Some(def.name.as_str()),
        _ => None,
    }
}

/// The name an item gives a type or an interface, which share one set of names.
fn declared_name(item: &Item) -> Option<&str> {
    match item {
        Item::Interface(interface) => Some(interface.name.as_str()),
        Item::TypeAlias { name, .. } => Some(name.as_str()),
        item => declared_type(item),
    }
}

/// Problems with the program's own types named as they're written.
fn as_written(mut problems: Vec<Diagnostic>, names: &BTreeMap<String, String>) -> Vec<Diagnostic> {
    for problem in &mut problems {
        for (written, checked) in names {
            problem.message = problem.message.replace(checked, written);
            problem.help = problem.help.take().map(|help| help.replace(checked, written));
            for fix in &mut problem.fixes {
                fix.text = fix.text.replace(checked, written);
            }
        }
        problem.message = without_hidden(&problem.message);
        problem.help = problem.help.take().map(|help| without_hidden(&help));
    }
    problems
}

/// Text with each hidden type parameter shown as the interface it stands for: `Shape'1` as
/// `Shape`.
fn without_hidden(text: &str) -> String {
    let mut shown = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let after_word = shown.chars().last().is_some_and(char::is_alphanumeric);
        if c == '\'' && after_word && chars.peek().is_some_and(char::is_ascii_digit) {
            while chars.peek().is_some_and(char::is_ascii_digit) {
                chars.next();
            }
            continue;
        }
        shown.push(c);
    }
    shown
}

/// Gives each parameter typed by an interface a hidden type parameter bound by it, the
/// program's own interfaces and the standard library's alike.
fn hide_interface_params(module: &mut Module) {
    saving::generate(module);
    fennec_syntax::rename::self_as_its_type(module);
    fennec_syntax::rename::discards_apart(module);
    let library = fennec_syntax::prelude_modules().unwrap_or_default();
    let mut interfaces = fennec_syntax::visit::interface_names(library.iter().chain([&*module]));
    // A program's own type with an interface's name is that type.
    for item in &module.items {
        match item {
            Item::TypeAlias { name, .. } => {
                interfaces.remove(name);
            }
            item => {
                if let Some(name) = declared_type(item) {
                    interfaces.remove(name);
                }
            }
        }
    }
    fennec_syntax::visit::interface_params(module, &interfaces);
}

/// Gives a program's types that share a name with a library type the names they're checked
/// under, in place, for a module about to run.
fn own_types_apart(module: &mut Module) -> BTreeMap<String, String> {
    hide_interface_params(module);
    let names = own_type_names(module);
    fennec_syntax::rename::rename_types(module, &names);
    names
}

/// A mistake in the standard library's own code comes first, since without it a program's
/// names would seem not to exist.
fn library_parses() -> Result<(), Vec<Diagnostic>> {
    fennec_syntax::prelude_modules().map(|_| ()).map_err(|problem| vec![problem])
}

/// Whether the language provides `name` everywhere, with no import: a builtin like `len`,
/// `Ok` or `Err`, a built-in type like `str` or `Stdout`, or a built-in interface like `Eq`.
/// An import can't take one of these names with `as`.
pub fn provided_everywhere(name: &str) -> bool {
    (builtins::is_builtin(name) && !name.contains('.'))
        || program::BUILT_IN_TYPES.contains(&name)
        || program::BUILT_IN_INTERFACES.contains(&name)
}

/// The functions the language provides everywhere, called bare, as `round` and `len`. A program may
/// name a function of its own after one that isn't reserved, and it's the program's own: the
/// standard library's code and a dependency's still mean the builtin.
pub fn builtin_function_names() -> impl Iterator<Item = &'static str> {
    builtins::names()
}

/// Whether the engines provide a function of this name, like `sum` or `math.sqrt`.
pub fn is_builtin(name: &str) -> bool {
    builtins::is_builtin(name)
}

/// Whether a method the engines provide on a type changes its value, like `list.append`.
/// `None` when the type has no such method.
pub fn builtin_method_changes(ty: &Type, name: &str) -> Option<bool> {
    builtins::method(ty, name).map(|method| method.changes)
}

/// The signature of a method written in Rust, of a built-in type or one of the standard
/// library's, as a hover shows it: `def split(self, str) -> list[str]`. `None` when the type has
/// no such method: one written in Fennec has a declaration to show instead.
#[must_use]
pub fn builtin_method_shown(ty: &Type, name: &str) -> Option<String> {
    let method = builtins::method(ty, name).or_else(|| match ty {
        Type::Named(owner) | Type::Generic(owner, _) => stdlib::method(owner, name),
        _ => None,
    })?;
    Some(method.shown(name))
}

/// The types a method the engines provide on a type takes, after the value it's called on:
/// `dict.get` on a `dict[int | str, int]` takes an `int | str`. `None` when the type has no
/// such method.
pub fn builtin_method_params(ty: &Type, name: &str) -> Option<Vec<Type>> {
    builtins::method(ty, name).map(|method| method.params)
}

/// Checks a parsed module, then writes into it the enum of each leading-dot case whose name
/// more than one enum has, as the type expected where it's written decided: `.Int(value)`
/// given back as an `ExprKind` becomes `ExprKind.Int(value)`. A `sum` of floats gets its
/// start of `0.0` written in. Running a module needs this, since what runs it doesn't know
/// types.
pub fn check_and_resolve(module: &mut Module) -> Result<Signatures, Vec<Diagnostic>> {
    checked_and_resolved_in(module, checker::Scope::Everything)
}

/// Like `check_and_resolve`, checking as much of the library as `scope` says.
fn checked_and_resolved_in(
    module: &mut Module,
    scope: checker::Scope,
) -> Result<Signatures, Vec<Diagnostic>> {
    lend_lowering::start_program();
    let names = own_types_apart(module);
    refuse_reserved(module)?;
    let checked = checked_typed_in(module, scope).map_err(|problems| as_written(problems, &names))?;
    refuse_old_forms(module, &checked).map_err(|problems| as_written(problems, &names))?;
    resolved(module, checked, scope)
}

/// What routing a module about to be emitted did, as `route_natively` gives it back.
pub enum Routed {
    /// Something now calls std's code written in Fennec, so the module is to be checked again.
    Changed,
    /// Nothing changed. When routing had to check the module to find that out, these are the
    /// signatures `check_to_emit` would give for it, so it needn't be checked again.
    Unchanged(Option<Box<Signatures>>),
}

/// Routes a checked, resolved and trimmed module's sorts and formats to std's code written in
/// Fennec, as `route_sorts_natively` and then `route_formats_natively` each do, so they build
/// natively. A module that sorts or reads and writes a format is checked to find out, once, over
/// what it reaches, as `check_to_emit` checks it: not over all of the library.
pub fn route_natively(module: &mut Module) -> Routed {
    let sorts = calls_sorted(module);
    let formats = imports_a_format(module) || sends_json(module);
    if !sorts && !formats {
        return Routed::Unchanged(None);
    }
    let Ok(checked) = checked_typed_in(module, checker::Scope::ReachedWithFields) else {
        return Routed::Unchanged(None);
    };
    let sorted = if sorts { route_sorts_natively(module, &checked) } else { None };
    // Formats look at the module as sorting left it, which sorting checked if it changed it.
    let formatted = formats && route_formats_natively(module, sorted.as_ref().unwrap_or(&checked));
    if sorted.is_some() || formatted {
        Routed::Changed
    } else {
        Routed::Unchanged(Some(Box::new(checked.into_signatures())))
    }
}

/// Makes the calls to `json.dumps`, `json.loads`, `toml.dumps` and `toml.loads` in a checked and
/// resolved module call std's readers and writers written in Fennec instead, where what's read or
/// written has its own `def serialize` or `def deserialize`, so they build natively. The rest
/// stay as they were. `checked` is what checking the module found. Says whether anything changed.
/// If the changed module doesn't check, which would be a mistake here, it's left as it was.
fn route_formats_natively(module: &mut Module, checked: &Checked) -> bool {
    if checked.native_formats.is_empty() && checked.native_json_bodies.is_empty() {
        return false;
    }
    let mut routed = module.clone();
    fennec_syntax::visit::exprs_mut(&mut routed, &mut |expr| {
        // `net.post(url, json=value)` sends `json.native_dumps(value)`: the JSON text.
        if let ExprKind::Call { args, .. } = &mut expr.kind {
            for arg in args.iter_mut().filter(|arg| arg.name.as_deref() == Some("json")) {
                let at = arg.value.span;
                if checked.native_json_bodies.contains(&(at.start, at.end)) {
                    // The call gets an empty span of its own: what the checker writes in, like
                    // the defaults `native_dumps` leaves out, is kept by span, and the value's is
                    // the value's.
                    let call = Span { start: at.start, end: at.start };
                    let placeholder = Expr { kind: ExprKind::Bool(false), span: at };
                    let value = std::mem::replace(&mut arg.value, placeholder);
                    let callee = Expr { kind: ExprKind::Name("json.native_dumps".to_owned()), span: call };
                    let args = vec![fennec_syntax::Arg { name: None, is_mut: false, value }];
                    arg.value = Expr { kind: ExprKind::Call { callee: Box::new(callee), args }, span: call };
                }
            }
        }
        let Some(native) = checked.native_formats.get(&(expr.span.start, expr.span.end)) else { return };
        if let ExprKind::Call { callee, args } = &mut expr.kind {
            let named = match &mut callee.kind {
                ExprKind::Index { object, .. } => object,
                _ => callee,
            };
            named.kind = ExprKind::Name(native.clone());
            // std's code takes an option as its own type, where the rule that leaves out an
            // enum it's known to be applies: `names=JsonNames.camel_case` is `.camel_case` there.
            for arg in args.iter_mut().filter(|arg| arg.name.is_some()) {
                if let ExprKind::Attr { object, name, optional: false } = &arg.value.kind
                    && let ExprKind::Name(owner) = &object.kind
                    && fennec_syntax::importable_type(owner).is_some()
                {
                    arg.value.kind = ExprKind::Variant(name.clone());
                }
            }
        }
    });
    if checked_and_resolved_in(&mut routed, checker::Scope::ReachedWithFields).is_err() {
        return false;
    }
    *module = routed;
    true
}

/// Whether a call is given `json=`, which only an HTTP call takes.
fn sends_json(module: &Module) -> bool {
    let mut sends = false;
    fennec_syntax::read::exprs(module, &mut |expr| {
        if let ExprKind::Call { args, .. } = &expr.kind {
            sends |= args.iter().any(|arg| arg.name.as_deref() == Some("json"));
        }
    });
    sends
}

/// The function `route_sorts_natively` calls in place of `sorted`, written in Fennec: a
/// merge sort, which splits a list at its middle and, merging, takes from the right
/// only when its item is `<` the left one, so the order is the same where `<` is.
const SORTED_IN_ORDER: &str = "\
def fennec_sorted_in_order[T: Ordered](items: list[T]) -> list[T]:
    if len(items) <= 1:
        return items
    half = len(items) // 2
    left = fennec_sorted_in_order(items[:half])
    right = fennec_sorted_in_order(items[half:])
    mut merged: list[T] = []
    mut at_left = 0
    mut at_right = 0
    while at_left < len(left) and at_right < len(right):
        if right[at_right] < left[at_left]:
            merged.append(right[at_right])
            at_right += 1
        else:
            merged.append(left[at_left])
            at_left += 1
    return merged + left[at_left:] + right[at_right:]
";

/// Whether a module calls `sorted`: only then is it checked for `route_sorts_natively`, since
/// checking again costs a build its time.
fn calls_sorted(module: &Module) -> bool {
    let mut sorts = false;
    fennec_syntax::read::exprs(module, &mut |expr| {
        if let ExprKind::Call { callee, .. } = &expr.kind {
            sorts |= matches!(&callee.kind, ExprKind::Name(name) if name == "sorted");
        }
    });
    sorts
}

/// Makes each `sorted(items)` whose items have their own `<` call a merge sort written in Fennec,
/// which calls that `<` as any code does, so native code sorts them. `checked` is what checking the
/// module found. Gives back what checking the changed module found, when anything changed. If the
/// changed module doesn't check, which would be a mistake here, it's left as it was. It's left
/// unresolved, so run this before `route_formats_natively`.
fn route_sorts_natively(module: &mut Module, checked: &Checked) -> Option<Checked> {
    let types: BTreeMap<(u32, u32), Type> = checked
        .types
        .iter()
        .flat_map(|(_, body)| body.exprs().map(|(span, ty)| ((span.start, span.end), ty.clone())))
        .collect();
    // The type of the items, when they have their own `<`.
    let own_order = |ty: Option<&Type>| match ty {
        Some(Type::List(item)) => match item.as_ref() {
            Type::Named(name) if checked.signatures.get(&format!("{name}.<")).is_some() => Some(name.clone()),
            _ => None,
        },
        _ => None,
    };
    let mut routed = module.clone();
    let mut changed = false;
    fennec_syntax::visit::exprs_mut(&mut routed, &mut |expr| {
        let ExprKind::Call { callee, args } = &mut expr.kind else { return };
        let (ExprKind::Name(name), [items]) = (&callee.kind, args.as_slice()) else { return };
        if name != "sorted" || items.name.is_some() {
            return;
        }
        let Some(item) = own_order(types.get(&(items.value.span.start, items.value.span.end))) else {
            return;
        };
        // The type written in, as `fennec_sorted_in_order[Score](scores)`, for an empty list.
        let span = callee.span;
        let named = |name: String| Box::new(Expr { kind: ExprKind::Name(name), span });
        callee.kind =
            ExprKind::Index { object: named("fennec_sorted_in_order".to_owned()), index: named(item) };
        changed = true;
    });
    if !changed {
        return None;
    }
    let parsed = fennec_syntax::parse_prelude(SORTED_IN_ORDER).ok()?;
    routed.items.extend(parsed.items);
    // Checked as it is, and left unresolved for what checks it next: resolving makes some of it,
    // like `list[Score]()` turned into `[]`, what can't be checked twice.
    let again = checked_typed_in(&routed, checker::Scope::ReachedWithFields).ok()?;
    *module = routed;
    Some(again)
}

fn imports_a_format(module: &Module) -> bool {
    module.items.iter().any(|item| match item {
        Item::Import(import) => match import.module.as_slice() {
            [std, name, ..] if std == "std" => matches!(name.as_str(), "json" | "toml"),
            [std] if std == "std" => import.bound().any(|(name, _)| matches!(name, "json" | "toml")),
            _ => false,
        },
        _ => false,
    })
}

/// Like `check_and_resolve`, for a module about to run: the standard library's functions are
/// checked only when the module reaches them, so its signatures hold just those. The
/// library's own tests check all of it.
pub fn check_to_run(module: &mut Module) -> Result<Signatures, Vec<Diagnostic>> {
    checked_in(module, checker::Scope::Reached)
}

/// Checks a module to run, as `check_to_lower` does, and gives back the signatures
/// `check_to_emit` would give the module as it's left, when they're known without checking it
/// again: a build of the module can then skip that check.
///
/// # Errors
///
/// What the checker finds wrong with it.
pub fn check_to_build(module: &mut Module) -> Result<Option<Signatures>, Vec<Diagnostic>> {
    check_to_build_saying(module).map(|(signatures, as_left)| as_left.then_some(signatures))
}

/// `check_to_build`, giving back the signatures it found either way, with whether they're those
/// `check_to_emit` would give the module as it's left: they are once the last check saw it as it's
/// left, however much was written in before.
///
/// # Errors
///
/// What the checker finds wrong with it.
pub fn check_to_build_saying(module: &mut Module) -> Result<(Signatures, bool), Vec<Diagnostic>> {
    library_parses()?;
    lend_lowering::start_program();
    let names = own_types_apart(module);
    refuse_reserved(module)?;
    let checked = checker::Checker::new(module)
        .run(checker::Scope::Building)
        .map_err(|problems| as_written(problems, &names))?;
    refuse_old_forms(module, &checked).map_err(|problems| as_written(problems, &names))?;
    // Resolving writes in what the check found, as a default left out of a call, which checking
    // the module again types as what's written there: the first check's types are only those of
    // a module it left as it was.
    let seen = module.clone();
    resolved_saying(module, (checked, seen), checker::Scope::Building)
}

/// What a build's first check of a program found, kept for checking the program again once it's
/// edited (`check_to_build_keeping`): what it found for each function, kept apart by function,
/// and what the program declared besides its functions.
pub struct KeptCheck {
    first: Checked,
    declared: Vec<Item>,
    /// How many functions and tests the check that kept this could carry over from the build
    /// before it.
    carriable: usize,
    /// The module as that check saw it, when it carried anything over, for checking it all again
    /// later and comparing (`take_module`, `differs_from`).
    module: Option<Module>,
}

/// A check of all of a program, as a build's first check finds it, keeping nothing apart: what a
/// kept check that carried anything over is compared with (`KeptCheck::differs_from`).
pub struct WholeCheck(Result<Checked, Vec<Diagnostic>>);

/// Checks all of `module`, as a build's first check of it does, keeping nothing apart. `module` is
/// as a check saw it (`KeptCheck::take_module`).
#[must_use]
pub fn check_whole(module: &Module) -> WholeCheck {
    lend_lowering::start_program();
    WholeCheck(checker::Checker::new(module).run(checker::Scope::Building))
}

impl KeptCheck {
    /// How many functions and tests the check that kept this could carry over from the build
    /// before it, those written inside another counted with it: none when that build kept
    /// nothing, or the edit changed what the program declares or which functions it has.
    #[must_use]
    pub fn carried_over(&self) -> usize {
        self.carriable
    }

    /// The module as the check that kept this saw it, when it carried anything over: given once,
    /// to check it all again (`check_whole`).
    pub fn take_module(&mut self) -> Option<Module> {
        self.module.take()
    }

    /// Whether checking it all again, `whole`, agrees with what the check that kept this found,
    /// as can be seen without copying either: everything compared as it is, but for the open type
    /// variables of generic calls, which each check numbers itself. False when that can't be seen
    /// so; `differs_from` then says whether they differ.
    #[must_use]
    pub fn agrees_quickly(&self, whole: &WholeCheck) -> bool {
        let (Ok(whole), carried) = (&whole.0, &self.first) else { return false };
        whole.signatures == carried.signatures
            && whole.types == carried.types
            && differing_table(whole, carried) == "something else"
    }

    /// Where checking it all again, `whole`, disagrees with what the check that kept this found:
    /// whether it passes, its signatures, typed bodies or what it found to write in. Nothing when
    /// they agree, which is found without copying either when `agrees_quickly` sees it.
    #[must_use]
    pub fn differs_from(&self, whole: &WholeCheck) -> Option<String> {
        if self.agrees_quickly(whole) {
            return None;
        }
        let Ok(whole) = &whole.0 else {
            return Some(
                "whether it passes: checking it all again doesn't, and carrying over did".to_owned(),
            );
        };
        let carried = &self.first;
        // Two checks that agree can number their open type variables apart.
        differs_from_checked_again_given(Ok(whole.clone()), &Ok(carried.clone()))
    }
}

/// What an edit changed in a program since a build kept a check of it (`check_to_build_keeping`),
/// as the build reads the files' texts. The build gives it only when the program has the same files,
/// in the same order, each where it was: an edit to one file moves no other file's spans.
pub struct Edit {
    /// The spans of the kept check's program whose text changed or moved: in each changed file,
    /// from its first changed byte to its last when its length is the same, or else to the end of
    /// the file's slots, where the next file starts.
    pub dirty: Vec<Span>,
    /// The span each file covers, the same before the edit and after.
    pub files: Vec<Span>,
}

/// How far apart files start while there's room for slots, as `Sources::add` lays them out.
const SLOT: u32 = 1 << 18;

impl Edit {
    /// What changed from `before` to `now`, two loads of one program, one after the other. None
    /// when they aren't the same files, by path, in the same order, each starting where it did:
    /// then an edit moved one file's spans by changing another's length, and nothing kept is
    /// carried over.
    #[must_use]
    pub fn between(before: &fennec_syntax::Sources, now: &fennec_syntax::Sources) -> Option<Edit> {
        let (before, now) = (laid_out(before), laid_out(now));
        let same = before.len() == now.len()
            && before
                .iter()
                .zip(&now)
                .all(|(was, is)| was.path == is.path && was.span.start == is.span.start);
        same.then(|| Edit { dirty: dirty(&before, &now), files: slots(&before, &now) })
    }
}

/// A file of a program: its path, the span its text covers, and its text.
struct LaidOut<'s> {
    path: &'s str,
    span: Span,
    text: &'s str,
}

fn laid_out(sources: &fennec_syntax::Sources) -> Vec<LaidOut<'_>> {
    sources.files().zip(sources.spans()).map(|((path, text), span)| LaidOut { path, span, text }).collect()
}

/// The span each file's slots cover, the same before an edit and after: to where the next file
/// starts, or for the last, past the longer of its texts to the slot's end.
fn slots(before: &[LaidOut<'_>], now: &[LaidOut<'_>]) -> Vec<Span> {
    before
        .iter()
        .zip(now)
        .enumerate()
        .map(|(at, (was, is))| {
            let end = before.get(at + 1).map_or_else(
                || {
                    was.span
                        .end
                        .max(is.span.end)
                        .saturating_add(1)
                        .checked_next_multiple_of(SLOT)
                        .unwrap_or(u32::MAX)
                },
                |next| next.span.start,
            );
            Span { start: was.span.start, end }
        })
        .collect()
}

/// The text of the first program's files that an edit changed or moved, in their spans: in each
/// changed file, from the first byte that differs to the last, when its length is the same, or
/// else to the end of its slots.
fn dirty(before: &[LaidOut<'_>], now: &[LaidOut<'_>]) -> Vec<Span> {
    let slots = slots(before, now);
    before
        .iter()
        .zip(now)
        .zip(slots)
        .filter(|((was, is), _)| was.text != is.text)
        .map(|((was, is), slot)| {
            let (was_text, is_text) = (was.text.as_bytes(), is.text.as_bytes());
            let prefix = was_text.iter().zip(is_text).take_while(|(a, b)| a == b).count();
            let room = was_text.len().min(is_text.len()) - prefix;
            let suffix =
                was_text.iter().rev().zip(is_text.iter().rev()).take_while(|(a, b)| a == b).count().min(room);
            let offset = |at: usize| was.span.start.saturating_add(u32::try_from(at).unwrap_or(u32::MAX));
            let end =
                if was_text.len() == is_text.len() { offset(was_text.len() - suffix) } else { slot.end };
            Span { start: offset(prefix), end }
        })
        .collect()
}

/// `check_to_build_saying`, keeping what the first check finds for a build after an edit, and
/// carrying over from `kept`, what the build before it kept, what was found for each function the
/// edit left as it was: its text the same, where it was, and nothing found for it from text `edit`
/// changed or moved, as a default written in from a declaration after the edit. Nothing's carried
/// over when the edit changed anything the program declares besides what's inside its functions
/// and tests, or added, took away or renamed a function. With `edit`, a declaration whose text reads
/// the same, its methods' aside, is the same, wherever it is; without, it has to be where it was.
/// The rounds of rewriting that follow check what they write all again, as a build that keeps
/// nothing does.
///
/// With `compare`, each check that carried anything over is compared with checking it all again,
/// and a difference is the compiler's mistake, said as one (`compares_kept_check`).
///
/// # Errors
///
/// What the checker finds wrong with it, then nothing's kept.
pub fn check_to_build_keeping(
    module: &mut Module,
    (kept, edit): (Option<KeptCheck>, Option<&Edit>),
    compare: bool,
) -> Result<(Signatures, bool, KeptCheck), Vec<Diagnostic>> {
    library_parses()?;
    lend_lowering::start_program();
    let names = own_types_apart(module);
    refuse_reserved(module)?;
    let declared = declarations(module);
    let usable = kept.filter(|kept| match edit {
        Some(edit) => same_declarations(&kept.declared, &declared, edit),
        None => kept.declared == declared,
    });
    let dirty = edit.map_or(&[][..], |edit| edit.dirty.as_slice());
    let first_check = match usable {
        Some(kept) => checker::Checker::carrying(module, kept.first, dirty),
        None => checker::Checker::keeping(module),
    };
    let mut carriable = first_check.carriable();
    let (mut found, moved) = first_check.run_saying(checker::Scope::Building);
    // A constant's or a field's type worked out differently: what read it was carried over with
    // the type it had, so it's all checked again.
    if moved {
        carriable = 0;
        found = checker::Checker::keeping(module).run(checker::Scope::Building);
    }
    if compare
        && carriable > 0
        && let Some(part) = differs_from_checked_again(module, checker::Scope::Building, &found)
    {
        return Err(vec![kept_check_differs(&part)]);
    }
    let checked = found.map_err(|problems| as_written(problems, &names))?;
    refuse_old_forms(module, &checked).map_err(|problems| as_written(problems, &names))?;
    // The rounds only read the first check, and hand it back for the next build to carry over from.
    let checked_module = (carriable > 0 && !compare).then(|| module.clone());
    let seen = module.clone();
    let (signatures, as_left, _, first) =
        resolved_from(module, checked, checker::Scope::Building, (MOST_ROUNDS, Some(seen)), true)?;
    // It's always handed back: an empty one would carry nothing over.
    let first = first.unwrap_or_default();
    Ok((signatures, as_left, KeptCheck { first, declared, carriable, module: checked_module }))
}

/// Whether a dev build whose inputs hash to `inputs` compares a check that carried anything over
/// with checking it all again: one in 8, picked by the inputs, so the same build always does or
/// doesn't. A release compiler compares only when it's asked to.
#[must_use]
pub fn compares_kept_check(inputs: u64) -> bool {
    cfg!(debug_assertions) && inputs.is_multiple_of(8)
}

/// Whether a dev compiler was asked to leave out one of the rules for carrying a check over,
/// `FENNEC_KEPT_RULE_OFF` naming it, so a test can make a carried check go wrong and see what
/// catches it. Only `bounds` is named: a callee whose type parameters' bounds changed has its
/// callers checked again. A release compiler leaves out none.
pub(crate) fn kept_rule_off(rule: &str) -> bool {
    leaves_out_a_kept_rule() && std::env::var("FENNEC_KEPT_RULE_OFF").is_ok_and(|off| off == rule)
}

/// Whether a dev compiler was asked to leave out a rule for carrying a check over
/// (`kept_rule_off`). Its own check of what a build hands on again, which a release compiler
/// doesn't make, is then left out too, so a wrong carried check gets as far as it would in a
/// release compiler, and only what a release compiler has can catch it.
#[must_use]
pub fn leaves_out_a_kept_rule() -> bool {
    cfg!(debug_assertions) && std::env::var_os("FENNEC_KEPT_RULE_OFF").is_some()
}

/// Like `check_to_run`, with every standard library type's fields typed too, so the module can
/// be lowered to find what it costs, as `fennec lint` does, without checking all of the library.
pub fn check_to_lower(module: &mut Module) -> Result<Signatures, Vec<Diagnostic>> {
    checked_in(module, checker::Scope::ReachedWithFields)
}

fn checked_in(module: &mut Module, scope: checker::Scope) -> Result<Signatures, Vec<Diagnostic>> {
    library_parses()?;
    lend_lowering::start_program();
    let names = own_types_apart(module);
    refuse_reserved(module)?;
    let checked =
        checker::Checker::new(module).run(scope).map_err(|problems| as_written(problems, &names))?;
    refuse_old_forms(module, &checked).map_err(|problems| as_written(problems, &names))?;
    resolved(module, checked, scope)
}

/// A name written in as the standard library's own, as `sha256` imported from std.crypto is
/// `crypto.sha256`, and a call the checker sent to another function: std's, as `crypto.blake3`
/// given a list goes to `crypto.blake3_of_str_list`, or a builtin's, as `i64(x)` is `int(x)`. A
/// call is seen before its callee, so `sent` holds each such callee: the name it was written with,
/// which may be std's too, mustn't then replace where it was sent.
fn std_written(expr: &mut Expr, checked: &Checked, span: (u32, u32), sent: &mut BTreeSet<(u32, u32)>) {
    if let Some(function) = checked.std_names.get(&span)
        && !sent.contains(&span)
    {
        expr.kind = ExprKind::Name(function.clone());
    }
    if let Some(function) = checked.std_calls.get(&span)
        && let ExprKind::Call { callee, .. } = &mut expr.kind
    {
        sent.insert((callee.span.start, callee.span.end));
        callee.kind = ExprKind::Name(function.clone());
    }
}

/// A call the checker sent to a standard library function, like `sum` of a list of floats to
/// `math.fixed_sum`, needs that function's module where it runs, though the program may never have
/// imported it: it's imported under a name of its own, which nothing written can reach or clash
/// with.
fn imports_for_std_calls<'a>(module: &mut Module, functions: impl Iterator<Item = &'a String>) {
    // Only a std function, named with its module, needs one. A builtin a call was sent to, as `int`
    // for `i64(x)`, has no dot, so it goes by: a dotted builtin would pull in an import here.
    let mut wanted: Vec<&str> =
        functions.filter_map(|function| function.split_once('.')).map(|(from, _)| from).collect();
    wanted.sort_unstable();
    wanted.dedup();
    for from in wanted {
        let imported = module.items.iter().any(|item| {
            matches!(item, Item::Import(import) if matches!(import.module.as_slice(), [std, name] if std == "std" && name == from))
        });
        if imported {
            continue;
        }
        module.items.insert(
            0,
            Item::Import(fennec_syntax::Import {
                module: vec!["std".to_owned(), from.to_owned()],
                names: Vec::new(),
                aliases: Vec::new(),
                alias: Some(format!("\0{from}")),
                span: Span::default(),
            }),
        );
    }
}

/// A leading-dot case whose name more than one enum has is written in with its enum's name, which
/// a program needn't have imported when it's the standard library's: `.Error` for a `LogLevel`.
/// Such an enum is imported where it runs. A program's own type by that name was renamed apart
/// already, so the name is free.
fn imports_for_cases(module: &mut Module, cases: &BTreeMap<(u32, u32), String>) {
    // Only a case the program writes: the library's own code sees its enums where it's written.
    let mut wanted: Vec<(&str, &str)> = cases
        .iter()
        .filter(|((start, _), _)| !Span { start: *start, end: *start }.in_prelude())
        .filter_map(|(_, owner)| Some((fennec_syntax::importable_type(owner)?, owner.as_str())))
        .collect();
    wanted.sort_unstable();
    wanted.dedup();
    for (from, owner) in wanted {
        let seen = module.items.iter().any(|item| match item {
            Item::Import(import) => import.bound().any(|(_, bound)| bound == owner),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => def.name == owner,
            _ => false,
        });
        if seen {
            continue;
        }
        module.items.insert(
            0,
            Item::Import(fennec_syntax::Import {
                module: vec!["std".to_owned(), from.to_owned()],
                names: vec![owner.to_owned()],
                aliases: vec![None],
                alias: None,
                span: Span::default(),
            }),
        );
    }
}

/// The start the checker writes into a `sum` with none: a float's zero for floats and `f32`s, and
/// an int's for the other sized numbers.
fn start_written(call: &mut Expr, checked: &Checked, span: (u32, u32)) {
    if checked.float_sums.contains(&span) || checked.sized_sums.get(&span) == Some(&true) {
        fennec_syntax::visit::add_float_start(call);
    }
    if checked.sized_sums.get(&span) == Some(&false) {
        fennec_syntax::visit::add_int_start(call);
    }
}

fn resolved(
    module: &mut Module,
    checked: Checked,
    scope: checker::Scope,
) -> Result<Signatures, Vec<Diagnostic>> {
    resolved_within(module, checked, scope, MOST_ROUNDS, None).map(|(signatures, _, _)| signatures)
}

/// Like `resolved`, saying too whether the signatures are of the module as it's left: whether the
/// last check, of `checked` the module, saw it as it's left. What's written in is checked again
/// over `scope`, as much of the library as the first round checked: written-in code that reaches
/// more of it reaches it in the round that checks it.
fn resolved_saying(
    module: &mut Module,
    (checked, seen): (Checked, Module),
    scope: checker::Scope,
) -> Result<(Signatures, bool), Vec<Diagnostic>> {
    resolved_within(module, checked, scope, MOST_ROUNDS, Some(seen))
        .map(|(signatures, as_left, _)| (signatures, as_left))
}

/// The most rounds of writing in what a check found, and checking again, a program is given. This
/// is a limit: each round writes in something it found, which the next has no more of to find, so
/// a program reaches the end in a few, as every program here does. One that hasn't by then is
/// a mistake in the compiler, said as one, naming the function the last round changed, never a
/// check that goes on round after round.
const MOST_ROUNDS: usize = 64;

/// `resolved_saying`, given at most `most` rounds, saying too how many it took. `seen` is the
/// module `checked` is of, when whether the signatures are of the module as it's left is wanted:
/// they are when the last check saw it as it's left, however many rounds wrote in before that one.
/// Without it, they're said to be only when nothing was written in.
fn resolved_within(
    module: &mut Module,
    checked: Checked,
    scope: checker::Scope,
    most: usize,
    seen: Option<Module>,
) -> Result<(Signatures, bool, usize), Vec<Diagnostic>> {
    resolved_from(module, checked, scope, (most, seen), false)
        .map(|(signatures, as_left, rounds, _)| (signatures, as_left, rounds))
}

/// `resolved_within`, giving back too, with `keep_first`, the first check, `checked`, once the
/// rounds are done with it: they only read it.
fn resolved_from(
    module: &mut Module,
    mut checked: Checked,
    scope: checker::Scope,
    (most, mut seen): (usize, Option<Module>),
    keep_first: bool,
) -> Result<(Signatures, bool, usize, Option<Checked>), Vec<Diagnostic>> {
    let mut first = true;
    let mut kept = None;
    for round in 1.. {
        // Only the last round's changes are looked at, to name what didn't stop changing.
        let before = (round >= most).then(|| module.clone());
        // The library's functions this program needs rewritten go through this round with its own.
        let std_in = std_rewritten_in(module, &checked);
        let written = resolved_round(module, &checked);
        std_rewritten_out(module);
        if lend_lowering::program_copies_ran_out() {
            return Err(vec![no_room_for_copies()]);
        }
        if lend_lowering::std_copies_ran_out() {
            return Err(vec![no_room_for_std_copies()]);
        }
        let again = match written {
            Round::Again => {
                // Each `_` has a name of its own, as the first check gave it.
                fennec_syntax::rename::discards_apart(module);
                true
            }
            // What was written in the library's functions is checked as what runs.
            Round::Done => std_in,
        };
        if !again {
            let moved = fennec_syntax::fuse::stored_stages(module);
            // What every round does, and moving stages, can change the module after the last check.
            let as_left = match &seen {
                Some(seen) => seen == module,
                None => first && !moved,
            };
            // When no round checked again, the signatures are the first check's too.
            if keep_first && kept.is_none() {
                kept = Some(checked.clone());
            }
            return Ok((checked.into_signatures(), as_left, round, kept));
        }
        if let Some(before) = before {
            return Err(vec![still_changing(&before, module)]);
        }
        first = false;
        if let Some(seen) = &mut seen {
            seen.clone_from(module);
        }
        let mut next = checker::Checker::new(module);
        next.again = true;
        let found = next.run(scope).map_err(|problems| {
            problems.iter().map(|problem| not_checked_again(problem, &module.copied)).collect::<Vec<_>>()
        })?;
        let done_with = std::mem::replace(&mut checked, found);
        if keep_first && kept.is_none() {
            kept = Some(done_with);
        }
    }
    unreachable!("the rounds go on until one returns")
}

/// What a module declares besides its functions and tests: its types and extensions without
/// their methods, which are functions, and its interfaces, aliases, constants and imports.
fn declarations(module: &Module) -> Vec<Item> {
    use fennec_syntax::{Extension, TypeDef};
    let without = |def: &TypeDef| TypeDef { methods: Vec::new(), ..def.clone() };
    module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(_) | Item::Test(_) | Item::Stmt(_) => None,
            Item::Struct(def) => Some(Item::Struct(without(def))),
            Item::Enum(def) => Some(Item::Enum(without(def))),
            Item::Error(def) => Some(Item::Error(without(def))),
            Item::Extend(extension) => {
                Some(Item::Extend(Extension { methods: Vec::new(), ..extension.clone() }))
            }
            other => Some(other.clone()),
        })
        .collect()
}

/// Where a declaration starts.
fn declared_at(item: &Item) -> Span {
    match item {
        Item::Struct(def) | Item::Enum(def) | Item::Error(def) => def.span,
        Item::Interface(interface) => interface.span,
        Item::Extend(extension) => extension.span,
        Item::TypeAlias { span, .. } | Item::Const { span, .. } => *span,
        Item::Import(import) => import.span,
        Item::Function(def) => def.span,
        Item::Test(test) => test.span,
        Item::Stmt(stmt) => stmt.span,
    }
}

/// Whether a program declares what it did before `edit`, item by item in the same order, as values:
/// each as it was, or one the edit moved, in the same file as before, the same once each of its spans
/// is moved by as much as it moved. The value is what's compared, not the text: the module the
/// checker reads isn't only its parse, as each file's imports rewrite names in it.
fn same_declarations(before: &[Item], now: &[Item], edit: &Edit) -> bool {
    // A file's slots end where the next file's start, so a span there is the next file's.
    let file =
        |span: Span| edit.files.iter().position(|file| file.start <= span.start && span.start < file.end);
    before.len() == now.len()
        && before.iter().zip(now).all(|(was, is)| {
            if was == is {
                return true;
            }
            let (from, to) = (declared_at(was), declared_at(is));
            if file(from).is_none() || file(from) != file(to) {
                return false;
            }
            let by = i64::from(to.start) - i64::from(from.start);
            let shift = |at: u32| u32::try_from(i64::from(at) + by).unwrap_or(at);
            moved::moved_item(was, &mut |span| Span { start: shift(span.start), end: shift(span.end) }) == *is
        })
}

/// `text` with each open type variable's number taken out: `Var(12)` as `Var(_)`.
fn without_var_numbers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("Var(") {
        let (before, after) = rest.split_at(at + "Var(".len());
        out.push_str(before);
        let digits = after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits > 0 {
            out.push('_');
        }
        rest = after.get(digits..).unwrap_or_default();
    }
    out.push_str(rest);
    out
}

/// The first of what a check found to write in that two checks disagree on, by its name.
fn differing_table(one: &Checked, other: &Checked) -> &'static str {
    macro_rules! first {
        ($($field:ident),* $(,)?) => {
            $(if one.$field != other.$field {
                return stringify!($field);
            })*
        };
    }
    first!(
        cases,
        float_sums,
        sized_sums,
        defaults,
        generator_types,
        not_equals,
        method_calls,
        array_zeros,
        optional_steps,
        std_names,
        std_calls,
        field_args,
        named_patterns,
        shared_values,
        sources,
        widened_values,
        generic_sources,
        union_sources,
        opened_apart,
        launches,
        test_errors,
        routes,
        ref_matches,
        live_refs,
        ref_items,
        native_formats,
        native_json_bodies,
        saved_types,
        holds,
    );
    // A generic call in the standard library can leave a type variable open, which each check
    // numbers itself.
    let calls_differ = one.generic_calls.len() != other.generic_calls.len()
        || one.generic_calls.iter().zip(&other.generic_calls).any(|(one, other)| {
            one != other
                && without_var_numbers(&format!("{one:?}")) != without_var_numbers(&format!("{other:?}"))
        });
    if calls_differ {
        return "generic_calls";
    }
    "something else"
}

/// Where a check that carried over what a check before it found, `carried`, disagrees with
/// checking all of `module` again: whether it passes, what it finds wrong, its signatures, typed
/// bodies and what it found to write in. Nothing when they agree.
fn differs_from_checked_again(
    module: &Module,
    scope: checker::Scope,
    carried: &Result<Checked, Vec<Diagnostic>>,
) -> Option<String> {
    differs_from_checked_again_given(checker::Checker::new(module).run(scope), carried)
}

/// `differs_from_checked_again`, given what checking it all again found, `whole`.
fn differs_from_checked_again_given(
    whole: Result<Checked, Vec<Diagnostic>>,
    carried: &Result<Checked, Vec<Diagnostic>>,
) -> Option<String> {
    let (whole, carried) = match (whole, carried) {
        (Ok(whole), Ok(carried)) => (whole, carried.clone()),
        (Err(whole), Err(carried)) => {
            return (&whole != carried)
                .then(|| format!("what it finds wrong: {:?} against {:?}", whole.first(), carried.first()));
        }
        (whole, _) => {
            let passes = |passes: bool| if passes { "passes" } else { "doesn't" };
            return Some(format!(
                "whether it passes: checking it all again {}, and carrying over {}",
                passes(whole.is_ok()),
                passes(carried.is_ok())
            ));
        }
    };
    // What's kept apart for a later check isn't what the check found.
    let comparable = |mut checked: Checked| {
        checked.tables_of.clear();
        checked.graph_of.clear();
        checked.defs.clear();
        checked.tests.clear();
        checked.const_types.clear();
        checked.field_types.clear();
        checked
    };
    let (whole, carried) = (comparable(whole), comparable(carried));
    // A type variable left open is numbered by the check that made it, so two checks that
    // agree on everything else can number theirs apart.
    (whole != carried
        && without_var_numbers(&format!("{whole:?}")) != without_var_numbers(&format!("{carried:?}")))
    .then(|| differing_part(&whole, &carried))
}

/// What's said when a check that carried over what one before it found disagrees with checking
/// it all again, in `part`: the compiler's mistake, which fails the build.
fn kept_check_differs(part: &str) -> Diagnostic {
    let message = format!(
        "internal error: a check that carried over what one before it found disagrees with checking it all again, in {part}"
    );
    Diagnostic::new(message, Span::default())
        .with_help("this is a mistake in the compiler, not in the program: please report it with the program")
}

/// What two checks of a module disagree on, first: a function's signature, then the rest of the
/// signatures, the typed bodies, then what they found to write in.
fn differing_part(again: &Checked, carried: &Checked) -> String {
    {
        let differs = again
            .signatures
            .functions
            .iter()
            .find(|(key, signature)| carried.signatures.functions.get(*key) != Some(*signature));
        let missing =
            carried.signatures.functions.keys().find(|key| !again.signatures.functions.contains_key(*key));
        if let Some((key, signature)) = differs {
            format!(
                "what `{key}` gives back and fails with: {signature:?} against {:?}",
                carried.signatures.functions.get(key)
            )
        } else if let Some(key) = missing {
            format!("`{key}`, which checking it all again doesn't have")
        } else if again.signatures.crossing != carried.signatures.crossing {
            let shown = |crossing: &Crossing| {
                crossing.types().map(ToString::to_string).collect::<Vec<_>>().join(", ")
            };
            format!(
                "what crosses between threads: {} against {}",
                shown(&again.signatures.crossing),
                shown(&carried.signatures.crossing)
            )
        } else if again.signatures != carried.signatures {
            let (one, other) = (&again.signatures, &carried.signatures);
            let part =
                if one.structs != other.structs || one.enums != other.enums || one.errors != other.errors {
                    "the types"
                } else if one.locals != other.locals {
                    "the locals"
                } else if one.lock_verdicts != other.lock_verdicts {
                    "the lock verdicts"
                } else if one.own_saving != other.own_saving {
                    "how types save themselves"
                } else {
                    "the typed bodies the signatures hold"
                };
            format!("the rest of the signatures: {part}")
        } else if again.types != carried.types {
            "the typed bodies".to_owned()
        } else {
            format!("what it found to write in, in `{}`", differing_table(again, carried))
        }
    }
}

/// What's said when the program's text leaves no room for the copies of its `with` sources
/// written in where they're used: each file takes a slot of 256 KiB or more, and the copies go
/// below what the compiler writes in, past the last file.
fn no_room_for_copies() -> Diagnostic {
    Diagnostic::new(
        "this program's files leave no room for the copies of its `with` sources written in where they're used",
        Span::default(),
    )
    .with_help("each file takes a slot of 256 KiB, or more for a bigger one, of the room spans have: a program of fewer files fits (#2114)")
}

/// What's said when the copies of the standard library's text a program needs written in, a
/// source's body and its cleanup at each way out of its block, find no room left: a limit of the
/// compiler's, said as one, never copies written over each other.
fn no_room_for_std_copies() -> Diagnostic {
    Diagnostic::new(
        "this program writes in more copies of the standard library's text than the compiler has room for",
        Span::default(),
    )
    .with_help(
        "this is a limit of the compiler, not a mistake in the program: please report it with the program",
    )
}

/// What's said when writing in what the check found is still changing the program after every
/// round it's given: a mistake in the compiler, naming the first function the last round changed.
fn still_changing(before: &Module, after: &Module) -> Diagnostic {
    let functions = |module: &Module| -> Vec<(String, fennec_syntax::FnDef)> {
        let mut found = Vec::new();
        fennec_syntax::read::each_function(module, &mut |def| {
            found.push((def.name.clone(), def.clone()));
        });
        found
    };
    let (was, now) = (functions(before), functions(after));
    let changed = now
        .iter()
        .find(|(name, def)| !was.iter().any(|(known, old)| known == name && old == def))
        .map(|(name, def)| (name.clone(), def.span));
    let (name, span) = changed.unwrap_or_else(|| ("the program".to_owned(), Span::default()));
    let message = format!(
        "internal error: writing in what the check found kept changing `{name}` after {MOST_ROUNDS} rounds"
    );
    Diagnostic::new(message, span)
        .with_help("this is a mistake in the compiler, not in the program: please report it with the program")
}

/// Whether a round of resolving wrote anything in, which is then checked again.
enum Round {
    Again,
    Done,
}

/// Puts into the program's items, under hidden names, the library's functions it runs that hold
/// a `with` over a source or a `launch`, and those rewritten already, so a round rewrites them as
/// it does the program's own. Says whether any was new, which the check after takes up.
fn std_rewritten_in(module: &mut Module, checked: &Checked) -> bool {
    let wanted: BTreeSet<(u32, u32)> = checked
        .sources
        .keys()
        .chain(checked.launches.keys())
        .filter(|(start, _)| Span { start: *start, end: *start }.in_prelude())
        .copied()
        .collect();
    let mut new = false;
    if !wanted.is_empty() {
        for (key, def) in library_functions() {
            // A source is written in where it's used, and its body lowered there.
            let source = def.ret.as_ref().is_some_and(|ret| ret.name == lend::WITH);
            if source || module.std_rewritten.iter().any(|(done, _)| *done == key) {
                continue;
            }
            let mut copy = def.clone();
            let mut holds = false;
            fennec_syntax::visit::in_function(&mut copy, &mut |expr| {
                holds |= wanted.contains(&(expr.span.start, expr.span.end));
            });
            if holds {
                module.std_rewritten.push((key, def.clone()));
                new = true;
            }
        }
    }
    for (key, mut def) in std::mem::take(&mut module.std_rewritten) {
        def.name = format!("{}{key}\0{}", reach::STD_AT, def.name);
        module.items.push(Item::Function(def));
    }
    new
}

/// Takes the library's functions back out of the program's items, with their own names.
fn std_rewritten_out(module: &mut Module) {
    let items = std::mem::take(&mut module.items);
    for item in items {
        match item {
            Item::Function(mut def) if def.name.starts_with(reach::STD_AT) => {
                let marked = def.name.trim_start_matches(reach::STD_AT).to_owned();
                let (key, name) = marked.split_once('\0').unwrap_or((marked.as_str(), marked.as_str()));
                name.clone_into(&mut def.name);
                module.std_rewritten.push((key.to_owned(), def));
            }
            other => module.items.push(other),
        }
    }
}

/// Each of the standard library's functions and methods, by the key the checker gives it.
fn library_functions() -> Vec<(String, fennec_syntax::FnDef)> {
    let mut found = Vec::new();
    for module in fennec_syntax::prelude_modules().unwrap_or_default() {
        let mut first: BTreeMap<String, &fennec_syntax::FnDef> = BTreeMap::new();
        for item in &module.items {
            match item {
                Item::Function(def) => found.push((def.name.clone(), def.clone())),
                Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                    for method in &def.methods {
                        let plain = format!("{}.{}", def.name, method.name);
                        let key = method_key(&def.name, method, first.get(&plain).copied());
                        first.entry(plain).or_insert(method);
                        found.push((key, method.clone()));
                    }
                }
                Item::Extend(extension) => {
                    for method in &extension.methods {
                        let plain = format!("{}.{}", extension.name, method.name);
                        let key = method_key(&extension.name, method, first.get(&plain).copied());
                        first.entry(plain).or_insert(method);
                        found.push((key, method.clone()));
                    }
                }
                _ => {}
            }
        }
    }
    found
}

/// One round of writing in what the check found, saying whether anything was written that needs
/// checking again.
fn resolved_round(module: &mut Module, checked: &Checked) -> Round {
    // What a round writes in more than once is noted where it went, with what's there already.
    lend_lowering::copies_from(module);
    let round = written_in_round(module, checked);
    lend_lowering::copies_into(module);
    round
}

/// What `resolved_round` writes in.
fn written_in_round(module: &mut Module, checked: &Checked) -> Round {
    fennec_syntax::visit::type_fields_from_defaults(module);
    named::put_in_order(module, &checked.named_patterns);
    unqualified_cases(module, &checked.signatures);
    if !checked.cases.is_empty()
        || !checked.float_sums.is_empty()
        || !checked.sized_sums.is_empty()
        || !checked.generator_types.is_empty()
        || !checked.defaults.is_empty()
        || !checked.not_equals.is_empty()
        || !checked.method_calls.is_empty()
        || !checked.array_zeros.is_empty()
        || !checked.optional_steps.is_empty()
        || !checked.std_names.is_empty()
        || !checked.std_calls.is_empty()
        || !checked.field_args.is_empty()
    {
        // The callees of calls the checker sent to another function, which `std_written` keeps.
        let mut sent: BTreeSet<(u32, u32)> = BTreeSet::new();
        fennec_syntax::visit::exprs_mut(module, &mut |expr| {
            let span = (expr.span.start, expr.span.end);
            if let Some(owner) = checked.cases.get(&span) {
                fennec_syntax::visit::name_enum(expr, owner);
            }
            start_written(expr, checked, span);
            if let Some(defaults) = checked.defaults.get(&span) {
                fennec_syntax::visit::add_defaults(expr, defaults);
            }
            std_written(expr, checked, span, &mut sent);
            if let ExprKind::Call { args, .. } = &mut expr.kind {
                for arg in args.iter_mut().filter(|arg| arg.name.is_none()) {
                    if let Some(field) = checked.field_args.get(&(arg.value.span.start, arg.value.span.end)) {
                        arg.name = Some(field.clone());
                    }
                }
            }
            if checked.optional_steps.contains(&span)
                && let ExprKind::Attr { optional, .. } = &mut expr.kind
            {
                *optional = true;
            }
            if let Some(zero) = checked.array_zeros.get(&span) {
                fennec_syntax::visit::filled_array(expr, zero);
            }
            operators_written(expr, checked, span);
            if let Some(shown) = checked.generator_types.get(&span) {
                fennec_syntax::visit::name_generator_type(expr, shown);
            }
        });
    }
    imports_for_std_calls(module, checked.std_calls.values());
    imports_for_cases(module, &checked.cases);
    let rewritten = sources_copied(module, checked) || widened_values(module, &checked.widened_values);
    if rewritten || routes::lower(module, &checked.routes) {
        return Round::Again;
    }
    // `shared T` and `lock` written in as today's `shared` types and `with` blocks are checked
    // again, so the signatures and types given back are those of what runs.
    let nested = lend_lowering::nest(module, &checked.opened_apart);
    let sources = cases_named(&checked.sources, &checked.cases);
    let test_fails = tests_failing(module, &checked.test_errors);
    let sources_written =
        lend_lowering::lower(module, &sources, &checked.launches, (&checked.ref_matches, &test_fails));
    // A copy just written in has spans of its own: what the check found in a source's text is
    // found in each copy too.
    let (ref_items, live_refs) = (
        lend_lowering::found_in_copies(&checked.ref_items),
        lend_lowering::found_in_copies(&checked.live_refs),
    );
    let shared_values = lend_lowering::found_in_copies(&checked.shared_values);
    let counted = enumerate_lowering::lower(module, &ref_items);
    let live = ref_lowering::lower(module, &live_refs);
    let tuples = tuple_lowering::lower(module, &checked.saved_types);
    if lock_lowering::lower(module, &shared_values) || sources_written || counted || live || nested || tuples
    {
        return Round::Again;
    }
    Round::Done
}

/// What each test fails with, worked out from its body, by its span, as `errors` has it, written. One
/// that can't fail isn't here.
fn tests_failing(
    module: &Module,
    errors: &BTreeMap<(u32, u32), Type>,
) -> BTreeMap<(u32, u32), fennec_syntax::TypeExpr> {
    // The program's own types, which its files name. Any other is the standard library's, written
    // where that library's code would name it, which needs no import.
    let own: BTreeSet<&str> = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) if !def.span.in_prelude() => {
                Some(def.name.as_str())
            }
            _ => None,
        })
        .collect();
    let mut found = BTreeMap::new();
    for item in &module.items {
        let Item::Test(test) = item else { continue };
        let at = (test.span.start, test.span.end);
        if let Some(error) = errors.get(&at)
            && let Some(mut written) = error.written(test.span)
        {
            library_named(&mut written, &own);
            found.insert(at, written);
        }
    }
    found
}

/// A written type with each type in it that isn't one of `own` given a span in the standard
/// library's text, where its name needs no import.
fn library_named(written: &mut fennec_syntax::TypeExpr, own: &BTreeSet<&str>) {
    if written.name != "|" && !own.contains(written.name.as_str()) {
        written.span = Span { start: Span::PRELUDE, end: Span::PRELUDE };
    }
    for arg in &mut written.args {
        library_named(arg, own);
    }
}

/// What the checker found an operator or builtin calls written in: `a != b` as `not (a == b)`
/// on a type with its own `def ==`, and `len(x)` and `abs(x)` as `x.len()` and `x.abs()`.
fn operators_written(expr: &mut Expr, checked: &Checked, span: (u32, u32)) {
    if checked.not_equals.contains(&span) {
        fennec_syntax::visit::not_of_equals(expr);
    }
    if checked.method_calls.contains(&span) {
        fennec_syntax::visit::method_of_call(expr);
    }
}

/// Writes each function named where one failing more ways goes as an arrow calling it, as
/// `either(load)` becomes `either(() => load()?)`, saying whether there were any. The arrow is
/// checked again as failing each of the ways the function type it's given for says.
fn widened_values(module: &mut Module, widened: &BTreeMap<(u32, u32), usize>) -> bool {
    if widened.is_empty() {
        return false;
    }
    let mut wrote = false;
    fennec_syntax::visit::exprs_mut(module, &mut |expr| {
        let Some(count) = widened.get(&(expr.span.start, expr.span.end)) else { return };
        let ExprKind::Name(_) = &expr.kind else { return };
        let at = expr.span;
        let params: Vec<String> = (0..*count).map(|number| format!("widened__{number}")).collect();
        let args = params
            .iter()
            .map(|param| fennec_syntax::Arg {
                name: None,
                is_mut: false,
                value: lend_lowering::name(param, at),
            })
            .collect();
        let function = Expr { kind: expr.kind.clone(), span: lend_lowering::here(at) };
        let call =
            Expr { kind: ExprKind::Call { callee: Box::new(function), args }, span: lend_lowering::here(at) };
        let passed = Expr {
            kind: ExprKind::Try { value: Box::new(call), context: None },
            span: lend_lowering::here(at),
        };
        expr.kind = ExprKind::Lambda { params, body: Box::new(passed) };
        expr.span = lend_lowering::here(at);
        wrote = true;
    });
    wrote
}

/// Copies what calls a `with` source that isn't known until the program runs, saying whether
/// any was, for the copies to be checked again, where each one's own source is found. A generic
/// function calling an interface's source is copied for each type it's called with, and a `with`
/// on a union's source is written in once for each member, under an `is` for it.
fn sources_copied(module: &mut Module, checked: &Checked) -> bool {
    generic_copies::lower(module, &checked.generic_sources, &checked.generic_calls)
        || union_sources::lower(module, &checked.union_sources)
}

/// Each source with its leading-dot cases named, as the program's own text was above: what
/// lowering writes in comes from the source as it was checked, where `fail .Stuck(...)` can sit
/// where its enum isn't known any more, as a note's text.
fn cases_named(
    sources: &BTreeMap<(u32, u32), lend_lowering::Source>,
    cases: &BTreeMap<(u32, u32), String>,
) -> BTreeMap<(u32, u32), lend_lowering::Source> {
    let mut named = sources.clone();
    if cases.is_empty() {
        return named;
    }
    for source in named.values_mut() {
        let def = std::sync::Arc::make_mut(&mut source.def);
        fennec_syntax::visit::in_function(def, &mut |expr| {
            if let Some(owner) = cases.get(&(expr.span.start, expr.span.end)) {
                fennec_syntax::visit::name_enum(expr, owner);
            }
        });
    }
    named
}

/// Each `Type.Case` in a `case`, as `case Err(SqlError.Failed(reason)):` is written on a result that
/// fails with a union, made `Case`, as what runs reads it: the check found which type it's from.
fn unqualified_cases(module: &mut Module, signatures: &Signatures) {
    fn unqualified(pattern: &mut fennec_syntax::Pattern, signatures: &Signatures) {
        use fennec_syntax::Pattern;
        match pattern {
            Pattern::Variant { name, args, .. } => {
                if let Some((owner, case)) = name.rsplit_once('.')
                    && signatures
                        .enumeration(owner)
                        .is_some_and(|found| found.cases.iter().any(|(known, _)| known == case))
                {
                    *name = case.to_owned();
                }
                for arg in args.iter_mut() {
                    unqualified(arg, signatures);
                }
            }
            Pattern::Tuple(items) | Pattern::Or(items) => {
                for item in items.iter_mut() {
                    unqualified(item, signatures);
                }
            }
            Pattern::Wildcard
            | Pattern::Bind(_)
            | Pattern::RefBind(_)
            | Pattern::MutBind(_)
            | Pattern::Literal(_) => {}
        }
    }
    fennec_syntax::visit::exprs_mut(module, &mut |expr| {
        if let ExprKind::Match { arms, .. } = &mut expr.kind {
            for arm in arms.iter_mut() {
                unqualified(&mut arm.pattern, signatures);
            }
        }
    });
}

/// What a program rewritten by lowering doesn't check for: the compiler's mistake, not the
/// program's, said as one, where the text is when it's in a copy written in (`copied`).
fn not_checked_again(problem: &Diagnostic, copied: &[fennec_syntax::Copied]) -> Diagnostic {
    let message =
        format!("internal error: once rewritten for running, this doesn't check: {}", problem.message);
    Diagnostic::new(message, problem.span.copied_from(copied))
        .with_help("this is a mistake in the compiler, not in the program: please report it with the program")
}

impl Checked {
    /// Whether what this check found for `key`, kept apart, holds a span `hit` says yes to: its
    /// typed body, what it wrote in the tables and adds to what's followed, or its parameters'
    /// defaults.
    pub(crate) fn holds_span(&self, key: &str, hit: &dyn Fn(Span) -> bool) -> bool {
        use checker::HoldsSpan as _;
        self.types.functions.get(key).is_some_and(|body| body.holds_span(hit))
            || self.tables_of.get(key).is_some_and(|tables| tables.holds_span(hit))
            || self.graph_of.get(key).is_some_and(|graph| graph.holds_span(hit))
            || self
                .signatures
                .functions
                .get(key)
                .is_some_and(|signature| signature.params.iter().any(|param| param.default.holds_span(hit)))
    }

    /// The signatures, holding the type of every expression too, for compilers to use.
    fn into_signatures(self) -> Signatures {
        Signatures { types: self.types, ..self.signatures }.without_literal()
    }

    /// Writes into `module`, a copy of the one checked, what `check_and_resolve` would, and gives
    /// back the signatures lowering takes: a program checked with `check_typed` can then be
    /// lowered without being checked twice.
    ///
    /// # Errors
    ///
    /// When the program as lowering rewrote it doesn't check, which is the compiler's mistake.
    pub fn resolve(&self, module: &mut Module) -> Result<Signatures, Vec<Diagnostic>> {
        resolved(module, self.clone(), checker::Scope::Everything)
    }
}

/// Everything a check works out about a program that has no mistakes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Checked {
    pub signatures: Signatures,
    pub types: Types,
    /// What each function's check wrote in the tables below, by its key, settled: what a later
    /// check carries over for a function an edit left as it was. Empty unless the check kept it.
    tables_of: BTreeMap<String, checker::Tables>,
    /// What each function's check added to what's followed across functions, by its key, settled.
    graph_of: BTreeMap<String, checker::Graph>,
    /// Every function the check knew, by its key, as it was: what a later check compares its own
    /// with, to carry over what was found for those left as they were.
    defs: BTreeMap<String, program::FnEntry>,
    /// Every test the check knew, by its key, as it was, for the same: those whose name another
    /// shares aren't here, since their key is both's.
    tests: BTreeMap<String, fennec_syntax::TestDef>,
    /// Each constant's type, and each field's, as the check worked them out, for a later check to
    /// tell whether it worked any out differently (`Checker::types_moved`). Empty unless kept.
    const_types: BTreeMap<String, Option<Type>>,
    field_types: BTreeMap<String, Option<Type>>,
    /// The enum the expected type chose for each leading-dot case more than one enum has.
    cases: BTreeMap<(u32, u32), String>,
    /// The calls to `sum` of floats with no start, which start at `0.0`.
    float_sums: BTreeSet<(u32, u32)>,
    sized_sums: BTreeMap<(u32, u32), bool>,
    /// The defaults each call leaves out, to write into it.
    defaults: BTreeMap<(u32, u32), Vec<(String, Expr)>>,
    /// The type of each generator expression, which printing one shows.
    generator_types: BTreeMap<(u32, u32), String>,
    /// What each name for a standard library function written in Fennec means.
    std_names: BTreeMap<(u32, u32), String>,
    /// The calls a standard library function written in Fennec makes instead.
    std_calls: BTreeMap<(u32, u32), String>,
    /// The field each value given by position goes to, where that isn't the one at its place.
    field_args: BTreeMap<(u32, u32), String>,
    /// The fields, in order, of each pattern written with them by name.
    named_patterns: BTreeMap<(u32, u32, String), Vec<String>>,
    /// What each `shared name = value` holds, which lowering writes in.
    shared_values: BTreeMap<(u32, u32), Type>,
    /// The source each call that lends is to, which lowering writes in where it's used.
    sources: BTreeMap<(u32, u32), lend_lowering::Source>,
    /// Each function named where one failing more ways goes, written in as an arrow calling it.
    widened_values: BTreeMap<(u32, u32), usize>,
    /// Each generic function that calls a `with` source an interface gives its type parameter.
    generic_sources: BTreeSet<String>,
    /// Each call to a generic function, with the types its type parameters are there.
    generic_calls: BTreeMap<(u32, u32), generic_copies::Call>,
    /// Each `with` source called on a union, which lowering writes in for each member.
    union_sources: BTreeMap<(u32, u32), union_sources::Dispatch>,
    /// The `with`s that open several handles, which lowering nests one inside another.
    opened_apart: BTreeSet<(u32, u32)>,
    /// Each `with launch(...)`, which lowering writes in as a source.
    launches: BTreeMap<(u32, u32), launch::Launch>,
    /// What each test or bench can fail with, worked out from its body, by its span.
    test_errors: BTreeMap<(u32, u32), Type>,
    /// Each route written with a method, as `router.get(...)`, which lowering writes in as a call
    /// to `handle`.
    routes: BTreeMap<(u32, u32), routes::Route>,
    /// Each `match` with a `ref` case, with the fields each such case puts back.
    ref_matches: BTreeMap<(u32, u32), Vec<Option<Vec<String>>>>,
    /// Each `for ref` whose body reads what it loops over, written in to go by position.
    live_refs: BTreeMap<(u32, u32), ref_lowering::Through>,
    /// Each `for key, ref value in x.items()`: whether `x` is a `Table`.
    ref_items: BTreeMap<(u32, u32), bool>,
    /// The format calls native code can make in Fennec, by the call's span.
    native_formats: BTreeMap<(u32, u32), String>,
    /// The `json=` values of HTTP calls native code writes in Fennec, by the value's span.
    native_json_bodies: BTreeSet<(u32, u32)>,
    /// Every type asked whether it saves or loads.
    saved_types: BTreeSet<Type>,
    /// What each kit the program depends on is handed, by kit and capability, with where it's
    /// first handed over: worked out only when the module asks for it, for `fennec update`.
    holds: BTreeMap<String, BTreeMap<String, Span>>,
    /// The `!=`s on a type with its own `def ==`, which become `not (a == b)`.
    not_equals: BTreeSet<(u32, u32)>,
    /// The calls `len(x)` and `abs(x)`, which become `x.len()` and `x.abs()`.
    method_calls: BTreeSet<(u32, u32)>,
    /// The fields and method calls on a value that might be missing, in a chain an `else`
    /// ends, which are written in as optional.
    optional_steps: BTreeSet<(u32, u32)>,
    /// Each `array[T, N]()` and the zero it's filled with, written in as `array.new(zero)`.
    array_zeros: BTreeMap<(u32, u32), Expr>,
}

/// The typed tree: the type of every expression and every declared name in a program,
/// the standard library's included, grouped by the code they're in.
///
/// Each group is keyed like [`Signatures`]: `name` for a function, `Type.name` for a method,
/// `outer.inner` for a nested function. Field defaults and `where` rules are under
/// `fields of Type`, and each `test` block under `test "its name"`.
///
/// Within a group, an expression is found by its span, which is unique: no two
/// expressions in one file cover the same bytes. A name is found by the span of what
/// declares it and its name:
/// - an assignment that makes it: the name's own span (each name's, in `a, b = pair`);
/// - a parameter: the parameter's span;
/// - a `for` loop or comprehension: the span of the whole `for` statement or comprehension;
/// - a `with ... as name`: the `with` statement's span;
/// - a `match` case: the case's span;
/// - an arrow function's parameter: the arrow function's span;
/// - a nested function's name: the nested `def`'s span.
///
/// Every value has a type here, nothing in it is `Type::Unknown` or `Type::Var`, and a
/// function called with a type parameter has its call typed with the parameter filled in.
/// Some expressions aren't values and have no type: a type's name used to reach a
/// constructor or enum case (`Vec3` in `Vec3.zero()`), a module's name, and the
/// `object.method` part of a method call (the call itself has a type).
///
/// Inside a generic function, types mention its parameters as `Type::Param`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Types {
    functions: BTreeMap<String, Body>,
}

impl Types {
    /// The types in one function, method, test or type's fields.
    pub fn body(&self, key: &str) -> Option<&Body> {
        self.functions.get(key)
    }

    /// Every group of types, by key in alphabetical order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Body)> {
        self.functions.iter().map(|(key, body)| (key.as_str(), body))
    }
}

/// The types of the expressions and names in one function, method, test or type's fields.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Body {
    pub(crate) exprs: BTreeMap<(u32, u32), Type>,
    pub(crate) names: BTreeMap<((u32, u32), String), Type>,
    pub(crate) names_outer: bool,
}

impl checker::HoldsSpan for Body {
    /// Where each type it knows is: a call's default, typed where it's written in, is in the
    /// callee's text.
    fn holds_span(&self, hit: &dyn Fn(Span) -> bool) -> bool {
        let Body { exprs, names, names_outer: _ } = self;
        exprs.keys().any(|at| at.holds_span(hit)) || names.keys().any(|(at, _)| at.holds_span(hit))
    }
}

impl Body {
    /// Whether the function names a `with` source's own function, constant or module where it's
    /// written in, with something in sight there named the same (`sight::outer`).
    #[must_use]
    pub fn names_outer(&self) -> bool {
        self.names_outer
    }

    /// The type of the expression at `span`.
    pub fn expr(&self, span: Span) -> Option<&Type> {
        self.exprs.get(&(span.start, span.end))
    }

    /// The type a name is declared with, by the span of what declares it. This is the type
    /// every value given to it has to fit, like `Enemy?`; what it holds at one point can be
    /// narrower, like `Enemy` after a check for None, and that's the type of the expression
    /// reading it there.
    pub fn name(&self, declared_at: Span, name: &str) -> Option<&Type> {
        self.names.get(&((declared_at.start, declared_at.end), name.to_owned()))
    }

    /// Every expression's span and type, in source order.
    pub fn exprs(&self) -> impl Iterator<Item = (Span, &Type)> {
        self.exprs.iter().map(|((start, end), ty)| (Span { start: *start, end: *end }, ty))
    }

    /// Every declared name, with the span of what declares it and its type, in source order.
    pub fn names(&self) -> impl Iterator<Item = (Span, &str, &Type)> {
        self.names
            .iter()
            .map(|(((start, end), name), ty)| (Span { start: *start, end: *end }, name.as_str(), ty))
    }
}

/// The signature of every function, method and nested function in a program, including the
/// standard library's: `name` for a function, `Type.name` for a method, and `outer.inner`
/// for a function defined inside another.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Signatures {
    functions: BTreeMap<String, Signature>,
    structs: BTreeMap<String, Struct>,
    enums: BTreeMap<String, Enum>,
    errors: BTreeMap<String, Struct>,
    locals: BTreeMap<String, Type>,
    types: Types,
    lock_verdicts: BTreeMap<String, String>,
    crossing: Crossing,
    /// The types that write their own `serialize`, by name, with whose it is: the shape and the
    /// layout `pack` saves go by what those save rather than by their fields.
    own_saving: BTreeMap<String, shapes::Writer>,
}

/// A struct's fields, with their types, in the order they're written.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Struct {
    /// Its type parameters' names, in order: `T` for `Pair[T]`, which its fields' types name.
    pub generics: Vec<String>,
    pub fields: Vec<(String, Type)>,
    /// Whether any field has a `where` rule, checked whenever a value is made.
    pub rules: bool,
    /// What's written above each field that has annotations, like `@skip`, by the field's
    /// name: data for formats and other code that reads the type.
    pub annotations: BTreeMap<String, Vec<fennec_syntax::Annotation>>,
}

/// An enum's cases, in the order they're written, each with its fields and their types.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Enum {
    /// Its type parameters' names, in order, which its cases' fields' types name.
    pub generics: Vec<String>,
    pub cases: Vec<(String, Vec<(String, Type)>)>,
    /// Whether any field has a `where` rule, checked whenever a value is made.
    pub rules: bool,
}

impl Signatures {
    /// These signatures, of a whole module, for the module cut down from it to what `main` reaches:
    /// the entries of what was cut away, by their keys (`cut`), are let go. What's left is what
    /// checking the cut module would find of the program's own, as `agree_within` holds a dev build
    /// to, with at most library entries more that only what was cut reached.
    #[must_use]
    pub fn cut_to(mut self, cut: &BTreeSet<String>) -> Signatures {
        self.functions.retain(|key, _| !cut.contains(key));
        self.structs.retain(|key, _| !cut.contains(key));
        self.enums.retain(|key, _| !cut.contains(key));
        self.errors.retain(|key, _| !cut.contains(key));
        self.types.functions.retain(|key, _| !cut.contains(key));
        self.lock_verdicts.retain(|key, _| !cut.contains(key));
        self.own_saving.retain(|key, _| !cut.contains(key));
        self
    }

    /// Whether these signatures, of a module cut down to what `main` reaches, agree with `wider`,
    /// those of the whole module the cut one came from: every function, type, local, verdict and
    /// body here is there too and the same. `wider` may hold more: what was cut away, and the
    /// library functions only that reached. What crosses between threads is worked out from every
    /// function the program has, so `wider`'s may say more types cross, never fewer, which keeps
    /// a count two threads can change where one would do.
    #[must_use]
    pub fn agree_within(&self, wider: &Signatures) -> bool {
        fn within<V: PartialEq>(narrow: &BTreeMap<String, V>, wide: &BTreeMap<String, V>) -> bool {
            narrow.iter().all(|(key, value)| wide.get(key) == Some(value))
        }
        within(&self.functions, &wider.functions)
            && within(&self.structs, &wider.structs)
            && within(&self.enums, &wider.enums)
            && within(&self.errors, &wider.errors)
            && within(&self.locals, &wider.locals)
            && within(&self.types.functions, &wider.types.functions)
            && within(&self.lock_verdicts, &wider.lock_verdicts)
            && within(&self.own_saving, &wider.own_saving)
            && self.crossing.within(&wider.crossing)
    }

    /// What each function waits on or locks, itself or however many calls down, by its key, as
    /// a `lock` that calls it is judged: a function that does neither isn't there. An edit that
    /// changes one of these can break a `lock` anywhere that calls the function.
    #[must_use]
    pub fn lock_verdicts(&self) -> &BTreeMap<String, String> {
        &self.lock_verdicts
    }

    /// Which types can ever cross to another task, so whose count needs keeping so two threads
    /// can change it. Signatures made without a check that worked it out say every type does.
    #[must_use]
    pub fn crossing(&self) -> &Crossing {
        &self.crossing
    }

    /// The signatures with each `LiteralStr` written as the `str` it is to the engines: it's a
    /// rule about where a value came from, checked already, not a type of its own when it runs.
    fn without_literal(mut self) -> Signatures {
        let plain = |ty: &mut Type| *ty = ty.without_literal();
        for signature in self.functions.values_mut() {
            signature.params.iter_mut().for_each(|param| plain(&mut param.ty));
            plain(&mut signature.returns);
        }
        for each in self.structs.values_mut().chain(self.errors.values_mut()) {
            each.fields.iter_mut().for_each(|(_, ty)| plain(ty));
        }
        for each in self.enums.values_mut() {
            each.cases.iter_mut().flat_map(|(_, fields)| fields).for_each(|(_, ty)| plain(ty));
        }
        self.locals.values_mut().for_each(plain);
        self.crossing = std::mem::take(&mut self.crossing).without_literal();
        for body in self.types.functions.values_mut() {
            body.exprs.values_mut().chain(body.names.values_mut()).for_each(plain);
        }
        self
    }

    /// Whether `==` on values of this type calls a type's own `def ==`: its own, or one of a
    /// type it holds, in a field, an enum case or a collection.
    pub fn own_equality(&self, ty: &Type) -> bool {
        self.holds_own_equality(ty, &mut Vec::new())
    }

    fn holds_own_equality(&self, ty: &Type, open: &mut Vec<String>) -> bool {
        let name = match ty {
            Type::Named(name) => name,
            Type::Generic(name, args) => {
                if args.iter().any(|arg| self.holds_own_equality(arg, open)) {
                    return true;
                }
                name
            }
            other => return other.children().into_iter().any(|child| self.holds_own_equality(child, open)),
        };
        if open.contains(name) {
            return false;
        }
        if self.functions.contains_key(&format!("{name}.==")) {
            return true;
        }
        open.push(name.clone());
        let fields = self.structs.get(name).or_else(|| self.errors.get(name)).map(|found| &found.fields);
        let in_fields =
            fields.is_some_and(|fields| fields.iter().any(|(_, ty)| self.holds_own_equality(ty, open)));
        let cases = self.enums.get(name).map(|found| &found.cases);
        let in_cases = cases.is_some_and(|cases| {
            cases.iter().flat_map(|(_, fields)| fields).any(|(_, ty)| self.holds_own_equality(ty, open))
        });
        in_fields || in_cases
    }

    /// The typed tree of the check that made these signatures.
    pub fn types(&self) -> &Types {
        &self.types
    }

    pub fn get(&self, name: &str) -> Option<&Signature> {
        self.functions.get(name)
    }

    /// Each other definition of an operator, `key` being its first's, like `Mat4.*`: the ones for
    /// other types on its right, as `Mat4.*@Vec4`.
    pub fn operator_again<'s>(&'s self, key: &str) -> impl Iterator<Item = (&'s str, &'s Signature)> + 's {
        let again = format!("{key}{}", extend::OTHER_RIGHT);
        self.functions
            .range(again.clone()..)
            .take_while(move |(found, _)| found.starts_with(&again))
            .map(|(found, signature)| (found.as_str(), signature))
    }

    /// A struct's fields, the standard library's included.
    pub fn structure(&self, name: &str) -> Option<&Struct> {
        self.structs.get(name)
    }

    /// An enum's cases, the standard library's included.
    pub fn enumeration(&self, name: &str) -> Option<&Enum> {
        self.enums.get(name)
    }

    /// An error type's fields, the standard library's included.
    pub fn error_type(&self, name: &str) -> Option<&Struct> {
        self.errors.get(name)
    }

    /// The name of every error type, the standard library's included.
    pub fn error_types(&self) -> impl Iterator<Item = &str> {
        self.errors.keys().map(String::as_str)
    }

    /// The name of every struct, the standard library's included.
    pub fn structures(&self) -> impl Iterator<Item = &str> {
        self.structs.keys().map(String::as_str)
    }

    /// The type of the expression at `span` in a function.
    pub fn expr_type(&self, function: &str, span: Span) -> Option<&Type> {
        self.types.body(function)?.expr(span).filter(|ty| ty.is_known())
    }

    /// The type a name holds in a function (`name`, `Type.name` or `outer.inner`), when it's
    /// declared with the same type everywhere in it. [`Types`] has each declaration's type.
    pub fn local(&self, function: &str, name: &str) -> Option<&Type> {
        self.locals.get(&format!("{function}/{name}")).filter(|ty| ty.is_known())
    }

    /// Every signature, by name in alphabetical order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Signature)> {
        self.functions.iter().map(|(name, signature)| (name.as_str(), signature))
    }
}

/// What a function takes and gives back.
#[derive(Clone, Debug, PartialEq)]
pub struct Signature {
    /// Every parameter, including `self` for a method.
    pub params: Vec<ParamType>,
    /// The type of what a call gives back: a `Type::Result` when it can fail, and a list of
    /// what it yields when it's a generator.
    pub returns: Type,
    /// Whether it can fail: it contains `fail` or `?`.
    pub can_fail: bool,
    /// Whether it's a generator: it contains `yield`.
    pub generator: bool,
    /// Its own type parameters, in order, which its parameters and what it gives back may not
    /// mention: only what's in brackets at a call can say what those stand for.
    pub generics: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParamType {
    pub name: String,
    pub ty: Type,
    pub is_mut: bool,
    /// What it holds when a call leaves it out.
    pub default: Option<Expr>,
}

#[cfg(test)]
mod rounds {
    use super::*;

    /// A program whose files leave no room for the copies of its `with` sources, written in where
    /// they're used, is refused, saying so: its copies never share spans, which once read one
    /// copy's types as another's (#2056). Each file takes a slot of 256 KiB, so a program of
    /// many files gets here sooner than one of a few; here the file runs up to just below where
    /// what the compiler writes in starts.
    #[test]
    fn a_program_whose_files_leave_no_room_for_copies_is_refused() {
        let source = "def opened(stdout: Stdout) -> with int:\n    stdout.print(\"open\")\n    lend 1\n    stdout.print(\"close\")\n\ndef main(stdout: Stdout):\n    with opened(stdout) as one:\n        stdout.print(one)\n";
        library_parses().unwrap();
        let mut module = fennec_syntax::parse(source).unwrap();
        module.files = vec![Span { start: 0, end: Span::WRITTEN - 16 }];
        let checked = checked_typed_in(&module, checker::Scope::Reached).unwrap();
        let resolved = resolved_within(&mut module, checked, checker::Scope::Reached, MOST_ROUNDS, None);
        let Err(problems) = resolved else { panic!("a program with no room for its copies was written in") };
        assert!(problems.iter().any(|problem| problem.message.contains("leave no room")), "{problems:?}");
    }

    /// The standard library's text written into a function, once the rounds are done, is on spans
    /// nothing else in the function has, the functions written inside it included: a copy's types
    /// and what's found for it, by span, are never another copy's (#2056, #2208). A source's
    /// cleanup is written in at each way out of its block and again in the function a crash runs,
    /// each copy on spans of its own. Two kinds still share spans and are left out here, #2236: a
    /// call's left-out literal default, written in with the callee's span, and the `deserialize`
    /// methods the compiler generates.
    #[test]
    fn std_text_written_into_a_function_is_on_spans_of_its_own() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut files = Vec::new();
        for folder in ["bench/kernels", "examples", "bench/realworld"] {
            for entry in std::fs::read_dir(root.join(folder)).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().is_some_and(|extension| extension == "fen") {
                    files.push(path);
                }
            }
        }
        files.sort();
        library_parses().unwrap();
        let (mut shared, mut programs) = (Vec::new(), 0);
        for path in files {
            let source = std::fs::read_to_string(&path).unwrap();
            let Ok(mut module) = fennec_syntax::parse(&source) else { continue };
            let _ = own_types_apart(&mut module);
            if refuse_reserved(&module).is_err() {
                continue;
            }
            let Ok(checked) = checked_typed_in(&module, checker::Scope::Reached) else { continue };
            if refuse_old_forms(&module, &checked).is_err() {
                continue;
            }
            if resolved_within(&mut module, checked, checker::Scope::Reached, MOST_ROUNDS, None).is_err() {
                continue;
            }
            programs += 1;
            for item in &module.items {
                let defs: Vec<&fennec_syntax::FnDef> = match item {
                    Item::Function(def) => vec![def],
                    Item::Struct(def) | Item::Enum(def) | Item::Error(def) => def.methods.iter().collect(),
                    Item::Extend(extension) => extension.methods.iter().collect(),
                    _ => Vec::new(),
                };
                let generated = |def: &&fennec_syntax::FnDef| {
                    def.name == "deserialize" || def.name.starts_with("fennec_tuple_deserialize")
                };
                for def in defs.into_iter().filter(|def| !generated(def)) {
                    let mut body = def.body.clone();
                    let mut seen: BTreeMap<(u32, u32), u32> = BTreeMap::new();
                    fennec_syntax::visit::in_stmts(&mut body, &mut |expr| {
                        let literal = matches!(
                            expr.kind,
                            ExprKind::Bool(_)
                                | ExprKind::None
                                | ExprKind::Int(_)
                                | ExprKind::Float(_)
                                | ExprKind::Str(_)
                        );
                        if expr.span.in_prelude() && expr.span.start < expr.span.end && !literal {
                            *seen.entry((expr.span.start, expr.span.end)).or_default() += 1;
                        }
                    });
                    if let Some((at, count)) = seen.iter().find(|(_, count)| **count > 1) {
                        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                        shared.push(format!("{name}: `{}` holds {at:?} {count} times", def.name));
                    }
                }
            }
        }
        assert!(programs >= 100, "only {programs} programs were written in");
        assert!(shared.is_empty(), "{shared:#?}");
    }

    /// Every kernel, the eight real programs among them, every example and every real-world
    /// program is written in and checked again in a quarter of the rounds it's given, at most:
    /// the bound is never what ends a real program's check.
    #[test]
    fn every_kernel_example_and_real_program_ends_well_inside_its_rounds() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut files = Vec::new();
        for folder in ["bench/kernels", "examples", "bench/realworld"] {
            for entry in std::fs::read_dir(root.join(folder)).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().is_some_and(|extension| extension == "fen") {
                    files.push(path);
                }
            }
        }
        files.sort();
        library_parses().unwrap();
        let (mut programs, mut most) = (0, (0, String::new()));
        for path in files {
            let source = std::fs::read_to_string(&path).unwrap();
            let Ok(mut module) = fennec_syntax::parse(&source) else { continue };
            let _ = own_types_apart(&mut module);
            if refuse_reserved(&module).is_err() {
                continue;
            }
            let Ok(checked) = checked_typed_in(&module, checker::Scope::Reached) else { continue };
            if refuse_old_forms(&module, &checked).is_err() {
                continue;
            }
            let shown = path.display().to_string();
            let resolved = resolved_within(&mut module, checked, checker::Scope::Reached, MOST_ROUNDS, None);
            let rounds = match resolved {
                Ok((_, _, rounds)) => rounds,
                Err(problems) => panic!("{shown}: {problems:?}"),
            };
            if rounds > most.0 {
                most = (rounds, shown);
            }
            programs += 1;
        }
        assert!(programs >= 100, "only {programs} programs were checked");
        assert!(most.0 * 4 <= MOST_ROUNDS, "{} took {} rounds", most.1, most.0);
    }

    /// Writing in what a check found, and checking again, is given a number of rounds. A program
    /// still changing after them is a compiler error naming the function the last round changed,
    /// never a check that goes on: here a `ref` case needs a second round and is given one.
    #[test]
    fn a_program_still_changing_after_its_rounds_is_an_error_naming_the_function() {
        let source = "enum Slot:\n    Held(count: int)\n    Empty\n\n\
                      def bump(ref slot: Slot):\n    match slot:\n        case .Held(ref count):\n            count += 1\n        case _:\n            pass\n\n\
                      def main(stdout: Stdout):\n    mut slot = Slot.Held(count=1)\n    bump(ref slot)\n    stdout.print(slot)\n";
        let mut module = fennec_syntax::parse(source).unwrap();
        library_parses().unwrap();
        let _ = own_types_apart(&mut module);
        let checked = checker::Checker::new(&module).run(checker::Scope::Reached).unwrap();
        let problems = resolved_within(&mut module, checked, checker::Scope::Reached, 1, None).unwrap_err();
        let [problem] = problems.as_slice() else { panic!("{problems:?}") };
        assert!(
            problem
                .message
                .starts_with("internal error: writing in what the check found kept changing `bump`"),
            "{problem:?}"
        );
    }
}
