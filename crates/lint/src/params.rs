//! `unused-param`: a parameter the function never reads. The fix removes it, and the argument
//! for it at every call. Only a function whose every call it can see is touched: a top-level
//! function that isn't `pub`, so no other kit calls it, isn't `main`, which the program is
//! started with, and is never used except by calling it by name, so no one holds it as a value
//! that has to take the parameter. Methods are left alone, since an interface may need the
//! parameter. One parameter of a function is removed at a time, so its edits never overlap.

use fennec_syntax::{Arg, Diagnostic, ExprKind, FnDef, Item, Module, Param, Sources, Span};

use crate::Lint;
use crate::dead::reads;

/// A file's text and the span's place in it.
fn local(sources: &Sources, span: Span) -> Option<(&str, u32, usize, usize)> {
    let (_, text, base) = sources.file_at(span)?;
    let at = |offset: u32| usize::try_from(offset.checked_sub(base)?).ok();
    Some((text, base, at(span.start)?, at(span.end)?))
}

/// Whether a function is written `pub def`, which other kits can call.
fn public(def: &FnDef, sources: &Sources) -> bool {
    let Some((text, _, start, _)) = local(sources, def.span) else { return true };
    let line_start = text.get(..start).and_then(|before| before.rfind('\n')).map_or(0, |at| at + 1);
    text.get(line_start..).is_none_or(|line| line.trim_start().starts_with("pub "))
}

/// Where a call's argument starts as written: before `name=` or `ref`, which its value's
/// span leaves out.
fn arg_start(text: &str, base: u32, arg: &Arg) -> Option<u32> {
    let value = usize::try_from(arg.value.span.start.checked_sub(base)?).ok()?;
    let mut before = text.get(..value)?.trim_end();
    if arg.is_mut {
        before = before.strip_suffix("ref")?;
    }
    if let Some(name) = &arg.name {
        before = before.strip_suffix('=')?.trim_end().strip_suffix(name.as_str())?;
    }
    let start = if arg.is_mut || arg.name.is_some() { before.len() } else { value };
    base.checked_add(u32::try_from(start).ok()?)
}

/// The edit that takes one item out of a comma-separated list, given each item's start and
/// end: up to the next item's start, or for the last one, from the end of the one before.
fn without(items: &[(u32, u32)], at: usize) -> Option<Span> {
    let (start, end) = *items.get(at)?;
    match (items.get(at + 1), at.checked_sub(1).and_then(|before| items.get(before))) {
        (Some((next, _)), _) => Some(Span { start, end: *next }),
        (None, Some((_, before_end))) => Some(Span { start: *before_end, end }),
        (None, None) => Some(Span { start, end }),
    }
}

/// What becomes of one call when a parameter goes.
enum AtCall {
    /// This edit takes the argument out.
    Remove(Span),
    /// The call leaves the parameter to its default, so nothing changes.
    Nothing,
    /// The argument's source can't be found, so the call can't be changed safely.
    Unknown,
}

/// What becomes of a call when the parameter at `index`, named `name`, goes.
fn at_call(args: &[Arg], index: usize, name: &str, sources: &Sources) -> AtCall {
    let Some(first) = args.first() else { return AtCall::Nothing };
    let Some((text, base, _, _)) = local(sources, first.value.span) else { return AtCall::Unknown };
    let items: Option<Vec<(u32, u32)>> =
        args.iter().map(|arg| Some((arg_start(text, base, arg)?, arg.value.span.end))).collect();
    let Some(items) = items else { return AtCall::Unknown };
    let positional = args.iter().take_while(|arg| arg.name.is_none()).count();
    let at = if index < positional {
        Some(index)
    } else {
        args.iter().position(|arg| arg.name.as_deref() == Some(name))
    };
    match at.map(|at| without(&items, at)) {
        Some(Some(edit)) => AtCall::Remove(edit),
        Some(None) => AtCall::Unknown,
        None => AtCall::Nothing,
    }
}

/// The arguments of each call of a function named `name` in the module, or None if the name
/// is used any other way, or a member anywhere has the name.
fn calls(module: &Module, name: &str) -> Option<Vec<Vec<Arg>>> {
    let mut walked = module.clone();
    let mut callees = Vec::new();
    let mut found = Vec::new();
    let mut names = Vec::new();
    let mut member = false;
    fennec_syntax::visit::exprs_mut(&mut walked, &mut |expr| match &expr.kind {
        ExprKind::Call { callee, args } if matches!(&callee.kind, ExprKind::Name(called) if called == name) =>
        {
            callees.push(callee.span);
            found.push(args.clone());
        }
        ExprKind::Name(used) if used == name => names.push(expr.span),
        ExprKind::Attr { name: used, .. } if used == name => member = true,
        _ => {}
    });
    (!member && names.iter().all(|span| callees.contains(span))).then_some(found)
}

/// The first parameter `def` never reads, with where it is among the parameters.
fn unread(def: &FnDef) -> Option<(usize, &Param)> {
    let read = reads(&def.body);
    def.params.iter().enumerate().find(|(_, param)| {
        !param.is_self && !param.name.starts_with('_') && read.get(&param.name).copied().unwrap_or(0) <= 0
    })
}

pub(crate) fn of(module: &Module, sources: &Sources) -> Vec<Lint> {
    let mut found = Vec::new();
    for item in &module.items {
        let Item::Function(def) = item else { continue };
        if def.name == "main" || def.body.is_empty() || !def.annotations.is_empty() || public(def, sources) {
            continue;
        }
        let Some((index, param)) = unread(def) else { continue };
        let Some(calls) = calls(module, &def.name) else { continue };
        let params: Vec<(u32, u32)> =
            def.params.iter().map(|param| (param.span.start, param.span.end)).collect();
        let Some(removed) = without(&params, index) else { continue };
        let mut edits = vec![removed];
        let mut ok = true;
        for args in calls {
            match at_call(&args, index, &param.name, sources) {
                AtCall::Remove(edit) => edits.push(edit),
                AtCall::Nothing => {}
                AtCall::Unknown => ok = false,
            }
        }
        if !ok {
            continue;
        }
        let message = format!("`{}` never reads `{}`", def.name, param.name);
        let mut diagnostic =
            Diagnostic::new(message, param.span).with_help("take it out, and its argument at every call");
        for edit in edits {
            diagnostic = diagnostic.with_fix(edit, "");
        }
        found.push(Lint::new("unused-param", diagnostic));
    }
    found
}
