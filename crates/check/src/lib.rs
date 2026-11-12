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

mod annotations;
mod builtins;
mod call;
mod checker;
mod constants;
mod effects;
mod enumerate_lowering;
mod except;
mod except_fix;
mod expect;
mod expr;
mod extend;
pub use extend::method_key;
mod fields;
mod function;
pub use fields::{each_field_type, fields_of};
mod generic;
mod generic_copies;
mod gone;
mod imports;
mod index;
mod infer;
mod keys;
mod launch;
mod lend;
mod lend_lowering;
mod lock;
mod lock_lowering;
mod loops;
mod lost;
mod messages;
mod named;
mod once;
mod parallel;
mod pattern;
mod place;
mod plain;
mod privacy;
mod program;
mod protocols;
mod ref_lowering;
mod results_fix;
mod rules;
mod saving;
mod secrets;
mod shapes;
mod shared;
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

pub use except_fix::migration_fixes;
pub use messages::missing_method;
pub use named::add_pattern_fixes;
pub use privacy::add_rename_fixes;
pub use results_fix::result_fixes;
pub use tuple_lowering::tuple_function;
pub use types::{Num, Type};

/// Checks a parsed module. Returns the signature of every function and method, or every
/// problem found, in source order.
pub fn check(module: &Module) -> Result<Signatures, Vec<Diagnostic>> {
    check_typed(module).map(Checked::into_signatures)
}

/// Like `check`, for a module `check_to_build` has resolved and trimmed to what `main` reaches,
/// about to be lowered: the standard library's functions are checked only when the module
/// reaches them, or lowering uses them without the program naming them.
pub fn check_to_emit(module: &Module) -> Result<Signatures, Vec<Diagnostic>> {
    checked_typed_in(module, checker::Scope::ReachedWithFields).map(Checked::into_signatures)
}

/// Checks a parsed module. Returns the signatures and the type of every expression and
/// name, or every problem found, in source order.
pub fn check_typed(module: &Module) -> Result<Checked, Vec<Diagnostic>> {
    checked_typed_in(module, checker::Scope::Everything)
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

/// Whether the engines provide a function of this name, like `sum` or `math.sqrt`.
pub fn is_builtin(name: &str) -> bool {
    builtins::is_builtin(name)
}

/// Whether a method the engines provide on a type changes its value, like `list.append`.
/// `None` when the type has no such method.
pub fn builtin_method_changes(ty: &Type, name: &str) -> Option<bool> {
    builtins::method(ty, name).map(|method| method.changes)
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
    let names = own_types_apart(module);
    let checked = check_typed(module).map_err(|problems| as_written(problems, &names))?;
    resolved(module, checked)
}

/// Makes the calls to `json.dumps`, `json.loads`, `toml.dumps` and `toml.loads` in a checked and
/// resolved module call std's readers and writers written in Fennec instead, where what's read or
/// written has its own `def serialize` or `def deserialize`, so they build natively. The rest
/// stay as they were. Says whether anything changed. If the changed module
/// doesn't check, which would be a mistake here, it's left as it was.
pub fn route_formats_natively(module: &mut Module) -> bool {
    if !imports_a_format(module) && !sends_json(module) {
        return false;
    }
    let Ok(checked) = check_typed(module) else { return false };
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
    if check_and_resolve(&mut routed).is_err() {
        return false;
    }
    *module = routed;
    true
}

/// Whether a call is given `json=`, which only an HTTP call takes.
fn sends_json(module: &Module) -> bool {
    let mut sends = false;
    let mut walked = module.clone();
    fennec_syntax::visit::exprs_mut(&mut walked, &mut |expr| {
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
    if items.len() <= 1:
        return items
    half = items.len() // 2
    left = fennec_sorted_in_order(items[:half])
    right = fennec_sorted_in_order(items[half:])
    mut merged: list[T] = []
    mut at_left = 0
    mut at_right = 0
    while at_left < left.len() and at_right < right.len():
        if right[at_right] < left[at_left]:
            merged.append(right[at_right])
            at_right += 1
        else:
            merged.append(left[at_left])
            at_left += 1
    merged + left[at_left:] + right[at_right:]
";

/// Makes each `sorted(items)` whose items have their own `<` call a merge sort written in Fennec,
/// which calls that `<` as any code does, so native code sorts them. Says whether anything changed.
/// If the changed module doesn't check, which would be a mistake here, it's left as it was. It's
/// left unresolved, so run this before `route_formats_natively`.
pub fn route_sorts_natively(module: &mut Module) -> bool {
    // Checking again costs a build its time, so only a program that sorts is.
    let mut sorts = false;
    fennec_syntax::visit::exprs_mut(&mut module.clone(), &mut |expr| {
        if let ExprKind::Call { callee, .. } = &expr.kind {
            sorts |= matches!(&callee.kind, ExprKind::Name(name) if name == "sorted");
        }
    });
    if !sorts {
        return false;
    }
    let Ok(checked) = check_typed(module) else { return false };
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
        return false;
    }
    let Ok(parsed) = fennec_syntax::parse_prelude(SORTED_IN_ORDER) else { return false };
    routed.items.extend(parsed.items);
    // Checked as it is, and left unresolved for what checks it next: resolving makes some of it,
    // like `list[Score]()` turned into `[]`, what can't be checked twice.
    if check_typed(&routed).is_err() {
        return false;
    }
    *module = routed;
    true
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
    library_parses()?;
    let names = own_types_apart(module);
    let checked = checker::Checker::new(module)
        .run(checker::Scope::ReachedWithFields)
        .map_err(|problems| as_written(problems, &names))?;
    // Resolving writes in what the check found, as a default left out of a call, which checking
    // the module again types as what's written there: the first check's types are only those of
    // a module it left as it was.
    let checked_module = module.clone();
    let (signatures, as_left) = resolved_saying(module, checked)?;
    let reused = as_left && *module == checked_module;
    Ok(reused.then_some(signatures))
}

/// Like `check_to_run`, with every standard library type's fields typed too, so the module can
/// be lowered to find what it costs, as `fennec lint` does, without checking all of the library.
pub fn check_to_lower(module: &mut Module) -> Result<Signatures, Vec<Diagnostic>> {
    checked_in(module, checker::Scope::ReachedWithFields)
}

fn checked_in(module: &mut Module, scope: checker::Scope) -> Result<Signatures, Vec<Diagnostic>> {
    library_parses()?;
    let names = own_types_apart(module);
    let checked =
        checker::Checker::new(module).run(scope).map_err(|problems| as_written(problems, &names))?;
    resolved(module, checked)
}

/// A call the checker sent to a standard library function, like `sum` of a list of floats to
/// `math.fixed_sum`, needs that function's module where it runs, though the program may never have
/// imported it: it's imported under a name of its own, which nothing written can reach or clash
/// with.
fn imports_for_std_calls<'a>(module: &mut Module, functions: impl Iterator<Item = &'a String>) {
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

fn resolved(module: &mut Module, checked: Checked) -> Result<Signatures, Vec<Diagnostic>> {
    resolved_saying(module, checked).map(|(signatures, _)| signatures)
}

/// Like `resolved`, saying too whether the signatures are those the first check found of the
/// module as it's left: they aren't once anything was written in, which is checked again with all
/// of the library, or once stages kept in names were moved to where they're used.
fn resolved_saying(module: &mut Module, checked: Checked) -> Result<(Signatures, bool), Vec<Diagnostic>> {
    // The library's functions this program needs rewritten go through this round with its own.
    let std_in = std_rewritten_in(module, &checked);
    let round = resolved_round(module, &checked);
    std_rewritten_out(module);
    match round {
        Round::Again => {
            // Each `_` has a name of its own, as the first check gave it.
            fennec_syntax::rename::discards_apart(module);
            let mut second = checker::Checker::new(module);
            second.again = true;
            match second.run(checker::Scope::Everything) {
                Ok(again) => resolved_saying(module, again).map(|(signatures, _)| (signatures, false)),
                Err(problems) => Err(problems.iter().map(not_checked_again).collect()),
            }
        }
        Round::Done if std_in => {
            // What was written in the library's functions is checked as what runs.
            let mut second = checker::Checker::new(module);
            second.again = true;
            match second.run(checker::Scope::Everything) {
                Ok(again) => resolved_saying(module, again).map(|(signatures, _)| (signatures, false)),
                Err(problems) => Err(problems.iter().map(not_checked_again).collect()),
            }
        }
        Round::Done => {
            let moved = fennec_syntax::fuse::stored_stages(module);
            Ok((checked.into_signatures(), !moved))
        }
    }
}

/// Whether a round of resolving wrote anything in, which is then checked again.
enum Round {
    Again,
    Done,
}

/// What starts the name a library function goes under in a program's items while it's rewritten:
/// no name a program writes starts with it.
const STD_AT: &str = "\0std:";

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
        def.name = format!("{STD_AT}{key}\0{}", def.name);
        module.items.push(Item::Function(def));
    }
    new
}

/// Takes the library's functions back out of the program's items, with their own names.
fn std_rewritten_out(module: &mut Module) {
    let items = std::mem::take(&mut module.items);
    for item in items {
        match item {
            Item::Function(mut def) if def.name.starts_with(STD_AT) => {
                let marked = def.name.trim_start_matches(STD_AT).to_owned();
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
        fennec_syntax::visit::exprs_mut(module, &mut |expr| {
            let span = (expr.span.start, expr.span.end);
            if let Some(owner) = checked.cases.get(&span) {
                fennec_syntax::visit::name_enum(expr, owner);
            }
            start_written(expr, checked, span);
            if let Some(defaults) = checked.defaults.get(&span) {
                fennec_syntax::visit::add_defaults(expr, defaults);
            }
            if let Some(function) = checked.std_names.get(&span) {
                expr.kind = ExprKind::Name(function.clone());
            }
            if let Some(function) = checked.std_calls.get(&span)
                && let ExprKind::Call { callee, .. } = &mut expr.kind
            {
                callee.kind = ExprKind::Name(function.clone());
            }
            if let ExprKind::Call { args, .. } = &mut expr.kind {
                for arg in args.iter_mut().filter(|arg| arg.name.is_none()) {
                    if let Some(field) = checked.field_args.get(&(arg.value.span.start, arg.value.span.end)) {
                        arg.name = Some(field.clone());
                    }
                }
            }
            if checked.failing_steps.contains(&span) {
                fennec_syntax::visit::none_on_failure(expr);
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
    if sources_copied(module, checked) || widened_values(module, &checked.widened_values) {
        return Round::Again;
    }
    // `shared T` and `lock` written in as today's `shared` types and `with` blocks are checked
    // again, so the signatures and types given back are those of what runs.
    let nested = lend_lowering::nest(module, &checked.opened_apart);
    let sources = cases_named(&checked.sources, &checked.cases);
    let sources_written = lend_lowering::lower(module, &sources, &checked.launches, &checked.ref_matches);
    let counted = enumerate_lowering::lower(module);
    let live = ref_lowering::lower(module, &checked.live_refs);
    let tuples = tuple_lowering::lower(module, &checked.saved_types);
    if lock_lowering::lower(module, &checked.shared_values)
        || sources_written
        || counted
        || live
        || nested
        || tuples
    {
        return Round::Again;
    }
    Round::Done
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
            Pattern::Wildcard | Pattern::Bind(_) | Pattern::RefBind(_) | Pattern::Literal(_) => {}
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
/// program's, said as one.
fn not_checked_again(problem: &Diagnostic) -> Diagnostic {
    let message =
        format!("internal error: once rewritten for running, this doesn't check: {}", problem.message);
    Diagnostic::new(message, problem.span)
        .with_help("this is a mistake in the compiler, not in the program: please report it with the program")
}

impl Checked {
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
        resolved(module, self.clone())
    }
}

/// Everything a check works out about a program that has no mistakes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Checked {
    pub signatures: Signatures,
    pub types: Types,
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
    /// Each `match` with a `ref` case, with the fields each such case puts back.
    ref_matches: BTreeMap<(u32, u32), Vec<Option<Vec<String>>>>,
    /// Each `for ref` whose body reads what it loops over, written in to go by position.
    live_refs: BTreeMap<(u32, u32), ref_lowering::Through>,
    /// The format calls native code can make in Fennec, by the call's span.
    native_formats: BTreeMap<(u32, u32), String>,
    /// The `json=` values of HTTP calls native code writes in Fennec, by the value's span.
    native_json_bodies: BTreeSet<(u32, u32)>,
    /// Every type asked whether it saves or loads.
    saved_types: BTreeSet<Type>,
    /// The `!=`s on a type with its own `def ==`, which become `not (a == b)`.
    not_equals: BTreeSet<(u32, u32)>,
    /// The calls `len(x)` and `abs(x)`, which become `x.len()` and `x.abs()`.
    method_calls: BTreeSet<(u32, u32)>,
    /// The fields and method calls on a value that might be missing, in a chain an `else`
    /// ends, which are written in as optional.
    optional_steps: BTreeSet<(u32, u32)>,
    /// Those read from something that can fail, which is written in as `value else None`.
    failing_steps: BTreeSet<(u32, u32)>,
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
}

impl Body {
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
}

/// A struct's fields, with their types, in the order they're written.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Struct {
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
    pub cases: Vec<(String, Vec<(String, Type)>)>,
    /// Whether any field has a `where` rule, checked whenever a value is made.
    pub rules: bool,
}

impl Signatures {
    /// What each function waits on or locks, itself or however many calls down, by its key, as
    /// a `lock` that calls it is judged: a function that does neither isn't there. An edit that
    /// changes one of these can break a `lock` anywhere that calls the function.
    #[must_use]
    pub fn lock_verdicts(&self) -> &BTreeMap<String, String> {
        &self.lock_verdicts
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
