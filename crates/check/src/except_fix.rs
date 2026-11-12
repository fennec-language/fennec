//! What `fennec fix --migrate` writes for the failure design Chris locked on 2026-11-11, in place
//! of the forms it replaces. Each rewrite means what the old form did:
//!
//! - `x = f() else as why:` becomes `x = f() except why:`, and a `match why:` that's all its
//!   block becomes `case` arms right under it;
//! - an `else` on a call that can fail drops every failure, so it becomes one case on the line
//!   naming each one, as `x = f() except Bad() | .Gone: value`, the same block or way out after;
//! - `lend x else:` becomes `lend x except:`;
//! - a `def`'s last line, when it's the value given back, gets its `return`.
//!
//! An `else` on a failing call inside a larger expression has no form `except` can take there, so
//! it's left to be written by hand, as the migration's refusal of it will say.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use fennec_syntax::{Diagnostic, Expr, ExprKind, FnDef, Item, Module, Sources, Span, Stmt, StmtKind};

use crate::types::Type;

/// The fixes that move a program to the forms of 2026-11-11, for `fennec fix --migrate`.
#[must_use]
pub fn migration_fixes(module: &Module, sources: &Sources) -> Vec<Diagnostic> {
    rewrites(&[module], sources, Some(module), false)
}

/// The same for the standard library's own code, checked with `module`. Its spans are past
/// `Span::PRELUDE`, each file in its own range, which `fennec_syntax::prelude_file_at` names.
#[must_use]
pub fn std_migration_fixes(module: &Module) -> Vec<Diagnostic> {
    let library: Vec<&Module> = fennec_syntax::prelude_modules().unwrap_or_default().iter().collect();
    rewrites(&library, &Sources::new(), Some(module), true)
}

/// The rewrites that need no types, for code that doesn't check on its own, like an example
/// of a mistake or a piece of a program in a test: `return`, `lend x except:`, `except why:`
/// for `else as why:`, and `len(x)` and `abs(x)`. A plain `else` on a failure needs its types,
/// so it's left.
#[must_use]
pub fn syntax_migration_fixes(module: &Module, sources: &Sources) -> Vec<Diagnostic> {
    rewrites(&[module], sources, None, false)
}

/// The rewrites in `modules`, those of the standard library's code when `library`, typed by
/// checking `typed` when there is one.
fn rewrites(
    modules: &[&Module],
    sources: &Sources,
    typed: Option<&Module>,
    library: bool,
) -> Vec<Diagnostic> {
    let mut found = Vec::new();
    let mut defs = Vec::new();
    for item in modules.iter().flat_map(|module| &module.items) {
        match item {
            Item::Function(def) => defs.push(def),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => defs.extend(&def.methods),
            Item::Extend(extension) => defs.extend(&extension.methods),
            _ => {}
        }
    }
    let mut all = Vec::new();
    for def in defs {
        nested(def, &mut all);
    }
    for def in all.iter().filter(|def| def.span.in_prelude() == library) {
        found.extend(def_return(def));
        let mut stmts = Vec::new();
        statements(&def.body, &mut stmts);
        found.extend(stmts.iter().filter_map(|stmt| lend_except(stmt, sources)));
    }
    found.extend(failure_fixes(modules, sources, typed, library));
    found
}

/// A `def` and each one written inside it, however deep.
fn nested<'d>(def: &'d FnDef, all: &mut Vec<&'d FnDef>) {
    all.push(def);
    let mut stmts = Vec::new();
    statements(&def.body, &mut stmts);
    for stmt in stmts {
        if let StmtKind::Def(inner) = &stmt.kind {
            nested(inner, all);
        }
    }
}

/// Every statement in `body`, those in blocks inside it included, a `match` case's among them,
/// but not those in a `def` written inside it, which is walked as its own.
fn statements<'s>(body: &'s [Stmt], all: &mut Vec<&'s Stmt>) {
    for stmt in body {
        all.push(stmt);
        match &stmt.kind {
            StmtKind::If { branches, otherwise } => {
                for (_, body) in branches {
                    statements(body, all);
                }
                if let Some(body) = otherwise {
                    statements(body, all);
                }
            }
            StmtKind::While { body, .. }
            | StmtKind::For { body, .. }
            | StmtKind::With { body, .. }
            | StmtKind::Lend { otherwise: body, .. } => statements(body, all),
            StmtKind::OnFailure { cleanup, body, .. } => {
                statements(cleanup, all);
                statements(body, all);
            }
            _ => {}
        }
        if !matches!(stmt.kind, StmtKind::Def(_)) {
            let mut cases = Vec::new();
            fennec_syntax::read::in_stmts(std::slice::from_ref(stmt), &mut |expr| {
                if let ExprKind::Match { arms, .. } = &expr.kind {
                    cases.extend(arms.iter().map(|arm| arm.body.as_slice()));
                }
            });
            for body in cases {
                statements(body, all);
            }
        }
    }
}

/// `return` before a `def`'s last line, when that line is the value it gives back: a value,
/// or each case's or branch's last line, of a `match` or an `if` that ends it, or a `with` block's.
fn def_return(def: &FnDef) -> Vec<Diagnostic> {
    let Some(ret) = def.ret.as_ref() else { return Vec::new() };
    if ret.name == "None"
        || ret.name == "with"
        || def.body.iter().any(|stmt| matches!(stmt.kind, StmtKind::Yield(_)))
    {
        return Vec::new();
    }
    let mut found = Vec::new();
    tails(&def.body, &mut found);
    found
        .into_iter()
        .map(|line| {
            let at = Span { start: line.span.start, end: line.span.start };
            Diagnostic::new("a `def` gives its value with `return`", line.span).with_fix(at, "return ")
        })
        .collect()
}

/// The lines that give a block's value: its last, when that's a value, or the last of each
/// case or branch of a `match` or an `if` that ends it, or of a `with` block that does.
fn tails<'s>(body: &'s [Stmt], found: &mut Vec<&'s Stmt>) {
    let Some(last) = body.last() else { return };
    match &last.kind {
        StmtKind::Expr(Expr { kind: ExprKind::Match { arms, .. }, .. }) => {
            for arm in arms {
                tails(&arm.body, found);
            }
        }
        StmtKind::Expr(_) => found.push(last),
        StmtKind::With { body, .. } => tails(body, found),
        StmtKind::If { branches, otherwise: Some(otherwise) } => {
            for (_, branch) in branches {
                tails(branch, found);
            }
            tails(otherwise, found);
        }
        _ => {}
    }
}

/// `lend x else:` as `lend x except:`.
fn lend_except(stmt: &Stmt, sources: &Sources) -> Option<Diagnostic> {
    let StmtKind::Lend { value, otherwise, .. } = &stmt.kind else { return None };
    if otherwise.is_empty() {
        return None;
    }
    let text = Text::of(sources, stmt.span)?;
    let after = text.get(Span { start: value.span.end, end: stmt.span.end })?;
    let at = after.find("else")?;
    if !after.get(..at)?.trim().is_empty() {
        return None;
    }
    let start = value.span.end + u32::try_from(at).ok()?;
    let word = Span { start, end: start + 4 };
    Some(Diagnostic::new("a `with` source's cleanup is `lend x except:`", word).with_fix(word, "except"))
}

/// Each `else` on a call that can fail, written as `except`, and `x.len()` as `len(x)`. With
/// no types, only `else as why:`, which only a failure takes.
fn failure_fixes(
    modules: &[&Module],
    sources: &Sources,
    typed: Option<&Module>,
    library: bool,
) -> Vec<Diagnostic> {
    let checked = match typed.map(crate::check_typed) {
        Some(Ok(checked)) => Some(checked),
        Some(Err(_)) => return Vec::new(),
        None => None,
    };
    let typed_exprs: Option<HashMap<(u32, u32), &Type>> = checked.as_ref().map(|checked| {
        checked
            .types
            .iter()
            .flat_map(|(_, body)| body.exprs().map(|(span, ty)| ((span.start, span.end), ty)))
            .collect()
    });
    let whole: HashSet<(u32, u32)> = modules.iter().flat_map(|module| whole_values(module)).collect();
    let mut found = Vec::new();
    for module in modules {
        fennec_syntax::read::exprs(module, &mut |expr| {
            if expr.span.in_prelude() != library {
                return;
            }
            if let Some(fix) = builtin_call(expr, typed_exprs.as_ref(), sources) {
                found.push(fix);
            }
            let ExprKind::Else { value, default, except: false } = &expr.kind else { return };
            if !whole.contains(&(expr.span.start, expr.span.end)) {
                return;
            }
            let failing = match (&typed_exprs, &checked) {
                (Some(typed_exprs), Some(checked)) => {
                    match typed_exprs.get(&(value.span.start, value.span.end)) {
                        Some(Type::Result(_, error)) => Some((error.as_ref(), &checked.signatures)),
                        _ => return,
                    }
                }
                // Only a failure is named with `else as`.
                _ if matches!(default.else_block(), Some((Some(_), _))) => None,
                _ => return,
            };
            if let Some(fix) = failure_fix((value, default), failing, sources) {
                found.push(fix);
            }
        });
    }
    if let (Some(typed_exprs), Some(checked)) = (&typed_exprs, &checked) {
        for module in modules {
            found.extend(hoisted(module, (typed_exprs, &checked.signatures), sources, library));
        }
    }
    found
}

/// Each `else` on a call that can fail inside a statement's expression, where `except` can't
/// stand, written on a line of its own before the statement, into a name the statement then
/// reads: `stdout.print(load(n) else -1)` becomes `load_value = load(n) except .Bad(): -1` and
/// `stdout.print(load_value)`. Only where moving the call first changes nothing: not inside an
/// arrow function, a comprehension, a conditional, the right of `and` or `or`, or another
/// `else`'s value, and with nothing before it in the statement that calls anything.
fn hoisted(
    module: &Module,
    (types, signatures): (&HashMap<(u32, u32), &Type>, &crate::Signatures),
    sources: &Sources,
    library: bool,
) -> Vec<Diagnostic> {
    let mut found = Vec::new();
    // The file's own names, which a new local mustn't hide.
    let top: HashSet<String> = module.items.iter().filter_map(defined_name).collect();
    let mut bodies: Vec<&[Stmt]> = Vec::new();
    for item in &module.items {
        match item {
            Item::Function(def) => bodies.push(&def.body),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                bodies.extend(def.methods.iter().map(|def| def.body.as_slice()));
            }
            Item::Extend(extension) => bodies.extend(extension.methods.iter().map(|def| def.body.as_slice())),
            Item::Test(test) => bodies.push(&test.body),
            _ => {}
        }
    }
    for body in bodies {
        let mut stmts = Vec::new();
        statements(body, &mut stmts);
        // The names the body uses already, which a new one takes a number past.
        let mut taken = top.clone();
        fennec_syntax::read::in_stmts(body, &mut |expr| {
            if let ExprKind::Name(name) = &expr.kind {
                taken.insert(name.clone());
            }
        });
        for stmt in stmts {
            if stmt.span.in_prelude() != library {
                continue;
            }
            // What runs once, before anything else of the statement: its value, a loop's
            // iterable, or the first condition of an `if`.
            let value = match &stmt.kind {
                StmtKind::Expr(value) | StmtKind::Return(Some(value)) | StmtKind::Assign { value, .. } => {
                    value
                }
                StmtKind::For { iter, .. } => iter,
                StmtKind::If { branches, .. } => match branches.first() {
                    Some((cond, _)) => cond,
                    None => continue,
                },
                _ => continue,
            };
            if let Some(fix) = hoist_in(stmt, value, (types, signatures), sources, &mut taken) {
                found.push(fix);
            }
        }
    }
    found
}

/// The name an item declares at the top of a file.
fn defined_name(item: &Item) -> Option<String> {
    match item {
        Item::Function(def) => Some(def.name.clone()),
        Item::Struct(def) | Item::Enum(def) | Item::Error(def) => Some(def.name.clone()),
        Item::Const { name, .. } | Item::TypeAlias { name, .. } => Some(name.clone()),
        _ => None,
    }
}

/// Whether evaluating `expr` itself does nothing that can be seen: a literal, a name, a field read
/// or something put together of those. A call, an index, arithmetic and `?` can each print, fail
/// or crash, so moving a call past one could change what happens first.
fn quiet(expr: &Expr) -> bool {
    matches!(
        expr.kind,
        ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Duration(_)
            | ExprKind::Str(_)
            | ExprKind::FStr(_)
            | ExprKind::Bool(_)
            | ExprKind::None
            | ExprKind::Name(_)
            | ExprKind::Variant(_)
            | ExprKind::List(_)
            | ExprKind::Tuple(_)
            | ExprKind::Dict(_)
            | ExprKind::Set(_)
            | ExprKind::Attr { optional: false, .. }
            | ExprKind::Else { .. }
            | ExprKind::Lambda { .. }
    )
}

/// The hoist for one statement whose value is `value`, when it has any `else` to move.
fn hoist_in(
    stmt: &Stmt,
    value: &Expr,
    (types, signatures): (&HashMap<(u32, u32), &Type>, &crate::Signatures),
    sources: &Sources,
    taken: &mut HashSet<String>,
) -> Option<Diagnostic> {
    // The parts of the statement that may not run, or run later or many times.
    let mut held: Vec<Span> = Vec::new();
    // What runs and could print, fail or crash: anything but a literal, a name or a field read.
    let mut loud: Vec<Span> = Vec::new();
    let mut elses: Vec<&Expr> = Vec::new();
    fennec_syntax::read::in_expr(value, &mut |inner| match &inner.kind {
        ExprKind::Lambda { .. } | ExprKind::Comprehension { .. } | ExprKind::Match { .. } => {
            held.push(inner.span);
        }
        ExprKind::Cond { then, cond, otherwise } => held.extend([then.span, cond.span, otherwise.span]),
        ExprKind::Binary { op: fennec_syntax::BinOp::And | fennec_syntax::BinOp::Or, rhs, .. } => {
            held.push(rhs.span);
        }
        ExprKind::Else { value: failing, default, except: false } => {
            held.push(default.span);
            if matches!(types.get(&(failing.span.start, failing.span.end)), Some(Type::Result(..))) {
                elses.push(inner);
            }
        }
        ExprKind::Else { default, .. } => held.push(default.span),
        _ if !quiet(inner) => loud.push(inner.span),
        _ => {}
    });
    let within = |outer: Span, inner: Span| outer.start <= inner.start && inner.end <= outer.end;
    // The statement's own `else` that's all of its value is the other rewrite's.
    elses.retain(|found| {
        found.span != value.span
            || !matches!(
                stmt.kind,
                StmtKind::Expr(_) | StmtKind::Return(_) | StmtKind::Assign { op: None, .. }
            )
    });
    let text = Text::of(sources, stmt.span)?;
    let line = text.get(Span { start: text.base, end: stmt.span.start })?;
    let indent = line.rsplit('\n').next().unwrap_or_default();
    if !indent.trim().is_empty() {
        return None;
    }
    let mut lines = String::new();
    let mut diagnostic =
        Diagnostic::new("a failure is handled with `except` on a line of its own", stmt.span);
    let mut moved: Vec<Span> = Vec::new();
    for found in elses {
        let ExprKind::Else { value: failing, default, .. } = &found.kind else { continue };
        if held.iter().any(|outer| within(*outer, found.span) && *outer != found.span)
            || default.else_block().is_some()
            // Anything before it that can print, fail or crash, outside what's moved already,
            // would run after it.
            || loud.iter().any(|before| before.end <= found.span.start && !moved.iter().any(|done| within(*done, *before)))
        {
            return None;
        }
        let Some(Type::Result(_, error)) = types.get(&(failing.span.start, failing.span.end)) else {
            return None;
        };
        let patterns: Vec<String> =
            failures(error, signatures)?.into_iter().map(|(_, pattern)| pattern).collect();
        let base = match &failing.kind {
            ExprKind::Call { callee, .. } => match &callee.kind {
                ExprKind::Name(name) => name.rsplit('.').next().unwrap_or(name).to_owned(),
                ExprKind::Attr { name, .. } => name.clone(),
                _ => "got".to_owned(),
            },
            _ => "got".to_owned(),
        };
        let mut name = format!("{base}_value");
        let mut number = 2;
        while taken.contains(&name) {
            name = format!("{base}_value{number}");
            number += 1;
        }
        taken.insert(name.clone());
        let called = text.get(failing.span)?;
        let instead = text.get(default.span)?.trim();
        let _ = writeln!(lines, "{indent}{name} = {called} except {}: {instead}", patterns.join(" | "));
        diagnostic = diagnostic.with_fix(Span { start: failing.span.start, end: default.span.end }, name);
        moved.push(found.span);
    }
    if moved.is_empty() {
        return None;
    }
    // Where the statement's line starts, ahead of anything written at the statement itself, as
    // the `return` its `def` may get.
    let line_start = stmt.span.start - u32::try_from(indent.len()).ok()?;
    Some(diagnostic.with_fix(Span { start: line_start, end: line_start }, lines))
}

/// `x.len()` or `x.abs()` on a value, written as the builtin, `len(x)` or `abs(x)`. One holding
/// another, like `x.len().abs()`, is left for the next round, so the two never write over each
/// other.
fn builtin_call(
    expr: &Expr,
    types: Option<&HashMap<(u32, u32), &Type>>,
    sources: &Sources,
) -> Option<Diagnostic> {
    let ExprKind::Call { callee, args } = &expr.kind else { return None };
    let ExprKind::Attr { object, name, optional: false } = &callee.kind else { return None };
    if !args.is_empty() || !matches!(name.as_str(), "len" | "abs") {
        return None;
    }
    // On a value: with no types, anything but a type's or module's name, like `Grid.len()`. Not
    // at the end of a chain a step of which can fail or be missing, as `find(1).name.len() else 0`,
    // whose `else` ends the chain: `len(...)` would hold the chain as an argument.
    let value = if let Some(types) = types {
        let mut chained = false;
        let mut step = object.as_ref();
        loop {
            if matches!(
                types.get(&(step.span.start, step.span.end)),
                Some(Type::Optional(_) | Type::Result(..))
            ) {
                chained = true;
            }
            step = match &step.kind {
                ExprKind::Attr { object, .. } | ExprKind::Index { object, .. } => object,
                ExprKind::Call { callee, .. } => match &callee.kind {
                    ExprKind::Attr { object, .. } => object,
                    _ => break,
                },
                _ => break,
            };
        }
        !chained && !matches!(types.get(&(object.span.start, object.span.end)), None | Some(Type::Unknown))
    } else {
        // With no types, only a receiver with no call in it, which can't be a step that fails
        // or is missing.
        let mut calls = false;
        fennec_syntax::read::in_expr(object, &mut |inner| {
            calls |= matches!(inner.kind, ExprKind::Call { .. });
        });
        !calls && !matches!(&object.kind, ExprKind::Name(named) if named.starts_with(char::is_uppercase))
    };
    if !value {
        return None;
    }
    let mut holds_one = false;
    fennec_syntax::read::in_expr(object, &mut |inner| {
        holds_one |= matches!(&inner.kind, ExprKind::Call { callee, args } if args.is_empty()
            && matches!(&callee.kind, ExprKind::Attr { name, .. } if matches!(name.as_str(), "len" | "abs")));
    });
    if holds_one {
        return None;
    }
    // The receiver as written, with the parentheses a builtin's own make needless.
    let text = Text::of(sources, expr.span)?;
    let written = text.get(Span { start: expr.span.start, end: callee.span.end })?;
    let receiver = written.strip_suffix(name.as_str())?.trim_end().strip_suffix('.')?.trim_end();
    // A receiver in parentheses, like `(a + b).len()`, is written from inside them: its `(`
    // is before where the call starts, and its `)` is the receiver's last, which no `(` opens.
    let mut at = expr.span;
    let mut receiver = one_group(receiver).unwrap_or(receiver);
    if receiver.matches(')').count() > receiver.matches('(').count() {
        let before = text.get(Span { start: text.base, end: at.start })?.trim_end();
        let opened = before.strip_suffix('(')?;
        at.start = text.base + u32::try_from(opened.len()).ok()?;
        receiver = receiver.strip_suffix(')')?.trim_end();
    }
    let message = format!("`{name}` is a builtin: write `{name}(x)`");
    Some(Diagnostic::new(message, at).with_fix(at, format!("{name}({receiver})")))
}

/// What's inside `text` when it's all one pair of parentheses, as `(a + b)` is and
/// `(a) + (b)` isn't.
fn one_group(text: &str) -> Option<&str> {
    let inner = text.strip_prefix('(')?.strip_suffix(')')?;
    let mut depth = 0_usize;
    for c in inner.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.checked_sub(1)?,
            _ => {}
        }
    }
    (depth == 0).then_some(inner)
}

/// The `else`s that are all of what a statement is written with, where `except` may stand: an
/// assignment's right side, a bare call, or what `return` gives.
fn whole_values(module: &Module) -> HashSet<(u32, u32)> {
    let mut whole = HashSet::new();
    let mut note = |value: &Expr| {
        if matches!(value.kind, ExprKind::Else { .. }) {
            whole.insert((value.span.start, value.span.end));
        }
    };
    let mut bodies: Vec<&[Stmt]> = Vec::new();
    for item in &module.items {
        match item {
            Item::Function(def) => bodies.push(&def.body),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                bodies.extend(def.methods.iter().map(|def| def.body.as_slice()));
            }
            Item::Extend(extension) => bodies.extend(extension.methods.iter().map(|def| def.body.as_slice())),
            Item::Test(test) => bodies.push(&test.body),
            _ => {}
        }
    }
    let mut stmts = Vec::new();
    for body in bodies {
        statements(body, &mut stmts);
    }
    let mut defs: Vec<&FnDef> = stmts
        .iter()
        .filter_map(|stmt| match &stmt.kind {
            StmtKind::Def(def) => Some(def),
            _ => None,
        })
        .collect();
    while let Some(def) = defs.pop() {
        let at = stmts.len();
        statements(&def.body, &mut stmts);
        defs.extend(stmts.get(at..).unwrap_or_default().iter().filter_map(|stmt| match &stmt.kind {
            StmtKind::Def(def) => Some(def),
            _ => None,
        }));
    }
    for stmt in stmts {
        match &stmt.kind {
            // Not `x += f() else d`: `except` can't stand there, so that's left to be written by hand.
            StmtKind::Expr(value)
            | StmtKind::Return(Some(value))
            | StmtKind::Assign { value, op: None, .. } => {
                note(value);
            }
            _ => {}
        }
    }
    whole
}

/// `value else ...` on a call that can fail with `error`, written with `except`.
fn failure_fix(
    (value, default): (&Expr, &Expr),
    failing: Option<(&Type, &crate::Signatures)>,
    sources: &Sources,
) -> Option<Diagnostic> {
    let text = Text::of(sources, value.span)?;
    let between = Span { start: value.span.end, end: default.span.start };
    let written = text.get(between)?;
    if !written.trim_start().starts_with("else") {
        return None;
    }
    // A block starts at its `:`, and a way out on the line, as `else return`, at its word.
    let block = text.get(default.span)?.starts_with(':');
    let all = failing.and_then(|(error, signatures)| failures(error, signatures)).unwrap_or_default();
    let mut diagnostic = Diagnostic::new("a failure is handled with `except` now", between);
    match default.else_block() {
        // A `match why:` that's all of the block is its arms, which needn't read `why`.
        Some((Some(name), body)) if arms_under(name, body, &text, &all).is_some() => {
            let (at, arms) = arms_under(name, body, &text, &all)?;
            diagnostic = diagnostic.with_fix(between, format!(" except {name}")).with_fix(at, arms);
        }
        Some((Some(name), body)) if body.iter().any(|stmt| crate::except::stmt_uses(stmt, name)) => {
            diagnostic = diagnostic.with_fix(between, format!(" except {name}"));
        }
        _ => {
            if all.is_empty() {
                return None;
            }
            let cases = all.iter().map(|(_, pattern)| pattern.as_str()).collect::<Vec<_>>().join(" | ");
            let after = if block { "" } else { ": " };
            diagnostic = diagnostic.with_fix(between, format!(" except {cases}{after}"));
        }
    }
    Some(diagnostic)
}

/// A `match name:` that's all of a block, as its arms written right under `except name:`: the
/// A block that starts with `match why:` as the arms of `except why:`, moved out a level.
/// What follows the `match` in the block goes at the end of each case that doesn't leave, and a
/// `case _:` that doesn't use `why` names the failures the cases before it leave instead.
fn arms_under(
    name: &str,
    body: &[Stmt],
    text: &Text<'_>,
    all: &[(String, String)],
) -> Option<(Span, String)> {
    let [Stmt { kind: StmtKind::Expr(matched), .. }, rest @ ..] = body else { return None };
    let ExprKind::Match { subject, arms } = &matched.kind else { return None };
    if !matches!(&subject.kind, ExprKind::Name(found) if found == name) {
        return None;
    }
    if rest.iter().any(|stmt| crate::except::stmt_uses(stmt, name)) {
        return None;
    }
    let line = text.get(Span { start: text.base, end: matched.span.start })?;
    let indent = line.rsplit('\n').next().unwrap_or_default();
    let after = rest.last().map_or(matched.span.end, |last| last.span.end);
    // The `match`'s span runs on to where the next line's code starts, so that line's indent is
    // the block's.
    let rest_text = match rest.first() {
        Some(first) => {
            format!("{indent}{}", text.get(Span { start: first.span.start, end: after })?.trim_end())
        }
        None => String::new(),
    };
    let mut cased: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for (at, arm) in arms.iter().enumerate() {
        let end = arms.get(at + 1).map_or(matched.span.end, |next| next.span.start);
        let mut written = text.get(Span { start: arm.span.start, end })?.trim_end().to_owned();
        let line = text.get(Span { start: text.base, end: arm.span.start })?;
        let body_indent = format!("{}    ", line.rsplit('\n').next().unwrap_or_default());
        // A body on the case's own line, `case X: f()`, goes on a line of its own, so lines can
        // follow it.
        if let Some(first) = arm.body.first()
            && let Some(between) = text.get(Span { start: arm.span.start, end: first.span.start })
            && !between.contains('\n')
        {
            let head = written.get(..between.len())?.trim_end().to_owned();
            let tail = written.get(between.len()..)?.to_owned();
            written = format!("{head}\n{body_indent}{tail}");
        }
        if matches!(arm.pattern, fennec_syntax::Pattern::Wildcard)
            && arm.guard.is_none()
            && !arm.body.iter().any(|stmt| crate::except::stmt_uses(stmt, name))
        {
            let left: Vec<&str> = all
                .iter()
                .filter(|(key, _)| !cased.iter().any(|named| covers(named, key)))
                .map(|(_, pattern)| pattern.as_str())
                .collect();
            if !left.is_empty() {
                written = written.replacen("case _", &format!("case {}", left.join(" | ")), 1);
            }
        }
        // `case error:` names the failure again, which `why` names already: it's `case _:`, with
        // `error` given `why` first.
        if let fennec_syntax::Pattern::Bind(other) = &arm.pattern
            && arm.guard.is_none()
            && !arm.body.is_empty()
        {
            let named = format!("case {other}:");
            if written.starts_with(&named) {
                written = written.replacen(&named, &format!("case _:\n{body_indent}{other} = {name}"), 1);
            }
        }
        variant_names(&arm.pattern, &mut cased);
        let mut lines = written.split('\n');
        let mut arm_text = lines.next()?.to_owned();
        for line in lines {
            arm_text.push('\n');
            arm_text.push_str(line.strip_prefix("    ").unwrap_or(line));
        }
        if !rest_text.is_empty() && !arm.body.last().is_some_and(leaves) {
            for line in rest_text.split('\n') {
                arm_text.push('\n');
                if !line.trim().is_empty() {
                    arm_text.push_str("    ");
                }
                arm_text.push_str(line);
            }
        }
        out.push(arm_text);
    }
    let written = text.get(Span { start: matched.span.start, end: after })?.trim_end();
    let end = matched.span.start + u32::try_from(written.len()).ok()?;
    Some((Span { start: matched.span.start, end }, out.join(&format!("\n{indent}"))))
}

/// Whether a line leaves its block, so nothing after it runs.
fn leaves(stmt: &Stmt) -> bool {
    matches!(stmt.kind, StmtKind::Return(_) | StmtKind::Fail(_) | StmtKind::Break | StmtKind::Continue)
}

/// The cases and errors a pattern names, as written: `Case` for `.Case`, `T.Case`, or `T`.
fn variant_names(pattern: &fennec_syntax::Pattern, names: &mut Vec<String>) {
    match pattern {
        fennec_syntax::Pattern::Variant { name, .. } => names.push(name.clone()),
        fennec_syntax::Pattern::Or(alternatives) => {
            for alternative in alternatives {
                variant_names(alternative, names);
            }
        }
        _ => {}
    }
}

/// Whether a case named `named` in a pattern takes the failure `key` stands for.
fn covers(named: &str, key: &str) -> bool {
    named == key || key.ends_with(&format!(".{named}")) || named.ends_with(&format!(".{key}"))
}

/// Each way a call can fail with `error`, as the patterns of one case on the line: each error's
/// cases, named on their own where no other error has one by that name, or the error itself.
/// Each comes with the name a pattern takes it by: `Case`, `T.Case` or `T`.
fn failures(error: &Type, signatures: &crate::Signatures) -> Option<Vec<(String, String)>> {
    let members: Vec<String> = match error {
        Type::Named(name) => vec![name.clone()],
        Type::Errors(names) => names.clone(),
        _ => return None,
    };
    let mut owners: HashMap<&str, usize> = HashMap::new();
    let cases_of = |member: &str| signatures.enumeration(member).filter(|found| !found.cases.is_empty());
    for member in &members {
        for (case, _) in cases_of(member).map(|found| found.cases.as_slice()).unwrap_or_default() {
            *owners.entry(case.as_str()).or_default() += 1;
        }
    }
    let mut patterns = Vec::new();
    for member in &members {
        let shown = fennec_syntax::rename::shown_type_name(member);
        match cases_of(member) {
            Some(found) => {
                for (case, fields) in &found.cases {
                    let (key, named) = if owners.get(case.as_str()).copied().unwrap_or(0) > 1 {
                        (format!("{shown}.{case}"), format!("{shown}.{case}"))
                    } else {
                        (case.clone(), format!(".{case}"))
                    };
                    patterns.push((key, if fields.is_empty() { named } else { format!("{named}()") }));
                }
            }
            None if signatures.error_type(member).is_some() => {
                patterns.push((shown.to_string(), format!("{shown}()")));
            }
            None => return None,
        }
    }
    Some(patterns)
}

/// The text of the file a span is in, read by spans.
struct Text<'a> {
    text: &'a str,
    base: u32,
}

impl<'a> Text<'a> {
    fn of(sources: &'a Sources, span: Span) -> Option<Self> {
        let (_, text, base) = sources.file_at(span).or_else(|| fennec_syntax::prelude_file_at(span))?;
        Some(Text { text, base })
    }

    fn get(&self, span: Span) -> Option<&'a str> {
        let from = usize::try_from(span.start.checked_sub(self.base)?).ok()?;
        let to = usize::try_from(span.end.checked_sub(self.base)?).ok()?;
        self.text.get(from..to)
    }
}

/// Whether a local can't take `name`: a builtin's, a built-in type's, or `case`.
pub fn reserved_name(name: &str) -> bool {
    RESERVED.contains(&name)
}

/// The names a local can't take: the builtins and the built-in types, and `case`, which starts an
/// arm under `except why:`.
const RESERVED: [&str; 29] = [
    "len",
    "abs",
    "sum",
    "sorted",
    "min",
    "max",
    "range",
    "enumerate",
    "zip",
    "int",
    "float",
    "bool",
    "str",
    "bytes",
    "list",
    "dict",
    "set",
    "array",
    "i8",
    "i16",
    "i32",
    "i64",
    "u8",
    "u16",
    "u32",
    "u64",
    "f32",
    "f64",
    "case",
];

/// The forms of before 2026-11-11 that a program checked as it was written still has, refused
/// with what to write: `else` on a call that can fail, `else as why:` among them, `lend x else:`, a
/// `def`'s value on its last line without `return`, and a local named after a builtin. `checked`
/// is what checking `module` found, before anything was written in. `fennec fix` makes the
/// rewrites `fennec fix --migrate` does, which these point at.
pub(crate) fn old_forms(module: &Module, checked: &crate::Checked) -> Vec<Diagnostic> {
    let mut found = elses_on_failures(module, checked);
    found.extend(old_lends_and_returns(module));
    for (span, name, _) in checked.types.functions.values().flat_map(crate::Body::names) {
        if RESERVED.contains(&name) && by_hand(span) {
            found.push(
                Diagnostic::new(format!("`{name}` is a builtin's name, which a local can't take"), span)
                    .with_help("name it for what it holds, as `count` or `items`"),
            );
        }
    }
    found.sort_by_key(|diagnostic| (diagnostic.span.start, diagnostic.span.end));
    found.dedup_by_key(|diagnostic| (diagnostic.span.start, diagnostic.span.end));
    found
}

/// Whether a program wrote what's at `span`, rather than the library or a lowering.
fn by_hand(span: Span) -> bool {
    !span.in_prelude() && !crate::lend_lowering::written_in(span) && span.start < span.end
}

/// Each `else` on a call that can fail, `else as why:` among them.
fn elses_on_failures(module: &Module, checked: &crate::Checked) -> Vec<Diagnostic> {
    // `with source() else fail E as x:` is refused too, with what to write instead.
    let mut sources = HashSet::new();
    for body in bodies_of(module) {
        let mut stmts = Vec::new();
        statements(body, &mut stmts);
        for stmt in stmts {
            if let StmtKind::With { value, .. } = &stmt.kind {
                sources.insert((value.span.start, value.span.end));
            }
        }
    }
    let mut failing = HashSet::new();
    fennec_syntax::read::exprs(module, &mut |expr| {
        if let ExprKind::Else { value, except: false, .. } = &expr.kind
            && by_hand(expr.span)
        {
            failing.insert((value.span.start, value.span.end));
        }
    });
    if failing.is_empty() {
        return Vec::new();
    }
    let fails: HashSet<(u32, u32)> = checked
        .types
        .functions
        .values()
        .flat_map(crate::Body::exprs)
        .filter(|(span, ty)| failing.contains(&(span.start, span.end)) && matches!(ty, Type::Result(..)))
        .map(|(span, _)| (span.start, span.end))
        .collect();
    let mut found = Vec::new();
    fennec_syntax::read::exprs(module, &mut |expr| {
        let ExprKind::Else { value, default, except: false } = &expr.kind else { return };
        if !fails.contains(&(value.span.start, value.span.end)) {
            return;
        }
        let word = Span { start: value.span.end, end: default.span.start.max(value.span.end) };
        let help = if sources.contains(&(expr.span.start, expr.span.end)) {
            "replace the failure inside the source (`except why: fail E(...)` there), then `with source()? as name:`"
        } else {
            "write `x = f() except why:` and use `why`, or name what it handles, as `x = f() except Missing(): 0`; `fennec fix` makes this change"
        };
        found.push(
            Diagnostic::new(
                "a failure is handled with `except` now: `else` is for a value that's missing",
                word,
            )
            .with_help(help),
        );
    });
    found
}

/// The body of each function, method and test in a module, those written inside them included.
fn bodies_of(module: &Module) -> Vec<&[Stmt]> {
    let mut defs = Vec::new();
    let mut bodies: Vec<&[Stmt]> = Vec::new();
    for item in &module.items {
        match item {
            Item::Function(def) => defs.push(def),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => defs.extend(&def.methods),
            Item::Extend(extension) => defs.extend(&extension.methods),
            Item::Test(test) => bodies.push(&test.body),
            _ => {}
        }
    }
    let mut all = Vec::new();
    for def in defs {
        nested(def, &mut all);
    }
    bodies.extend(all.iter().map(|def| def.body.as_slice()));
    bodies
}

/// Each `lend x else:`, and each `def`'s value on its last line without `return`.
fn old_lends_and_returns(module: &Module) -> Vec<Diagnostic> {
    let mut defs = Vec::new();
    for item in &module.items {
        match item {
            Item::Function(def) => defs.push(def),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => defs.extend(&def.methods),
            Item::Extend(extension) => defs.extend(&extension.methods),
            _ => {}
        }
    }
    let mut all = Vec::new();
    for def in defs {
        nested(def, &mut all);
    }
    let mut found: Vec<Diagnostic> =
        all.iter().filter(|def| by_hand(def.span)).flat_map(|def| def_return(def)).collect();
    let mut bodies: Vec<&[Stmt]> =
        all.iter().filter(|def| by_hand(def.span)).map(|def| def.body.as_slice()).collect();
    bodies.extend(module.items.iter().filter_map(|item| match item {
        Item::Test(test) if by_hand(test.span) => Some(test.body.as_slice()),
        _ => None,
    }));
    for body in bodies {
        let mut stmts = Vec::new();
        statements(body, &mut stmts);
        for stmt in stmts {
            if let StmtKind::Lend { otherwise, except: false, .. } = &stmt.kind
                && !otherwise.is_empty()
                && by_hand(stmt.span)
            {
                found.push(
                    Diagnostic::new("a `with` source's cleanup is written `lend x except:` now", stmt.span)
                        .with_help("write `except` for `else`; `fennec fix` makes this change"),
                );
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    /// What `migration_fixes` writes for `source`, and that what it writes checks.
    fn migrated(source: &str) -> String {
        let mut sources = fennec_syntax::Sources::new();
        let base = sources.add("forms.fen", source);
        let module = fennec_syntax::parse(source).unwrap();
        let fixes = super::migration_fixes(&module, &sources);
        let written = fennec_syntax::apply_fixes(source, base, &fixes).unwrap_or_else(|| source.to_owned());
        let rewritten = fennec_syntax::parse(&written).unwrap();
        assert!(crate::check_typed(&rewritten).is_ok(), "{written}");
        written
    }

    /// A case two errors both have is named with its error, so each pattern says which one it
    /// is, and an error with no cases is matched whole.
    #[test]
    fn a_case_two_errors_share_is_written_with_its_error() {
        let source = "error Disk:\n    Gone\n    Full(left: int)\n\nerror Net:\n    Gone\n\nerror Odd:\n    n: int\n\n\
            def load(n: int) -> int fails Disk | Net | Odd:\n    if n == 1: fail Net.Gone\n    return n\n\n\
            def main(stdout: Stdout):\n    got = load(1) else 0\n    stdout.print(got)\n";
        assert_eq!(
            migrated(source),
            source.replace("load(1) else 0", "load(1) except Disk.Gone | .Full() | Net.Gone | Odd(): 0")
        );
    }

    /// `x.len()` and `x.abs()` are written as the builtins, without the parentheses a receiver
    /// needed, one at a time where one holds another, and a type's own `def len` too.
    #[test]
    fn a_len_or_abs_method_is_written_as_the_builtin() {
        let source = "struct Bag:\n    items: list[int]\n\n    def len(self) -> int:\n        return len(self.items)\n\n\
            def main(stdout: Stdout):\n    numbers = [1, -2]\n    bag = Bag(items=numbers)\n    \
            stdout.print(numbers.len(), (numbers + [3]).len(), bag.len(), (-2).abs(), numbers.len().abs())\n";
        let once = migrated(source);
        assert_eq!(
            once,
            source.replace(
                "numbers.len(), (numbers + [3]).len(), bag.len(), (-2).abs(), numbers.len().abs()",
                "len(numbers), len(numbers + [3]), len(bag), abs(-2), len(numbers).abs()"
            )
        );
        assert_eq!(migrated(&once), once.replace("len(numbers).abs()", "abs(len(numbers))"));
    }

    /// `return` goes where the line starts, before a parenthesis, and into each case of a
    /// `match` and each branch of an `if` that ends a def, where they don't leave already.
    #[test]
    fn return_goes_on_each_line_that_gives_the_value() {
        let source = "enum Order:\n    Hold\n    Move\n\n\
            def half(n: int) -> int:\n    (n + 1) // 2\n\n\
            def steps(order: Order) -> int:\n    match order:\n        case .Hold:\n            return 0\n        case .Move:\n            2\n\n\
            def sign(n: int) -> int:\n    if n < 0:\n        -1\n    else:\n        1\n\n\
            def main(stdout: Stdout):\n    stdout.print(half(3), steps(.Move), sign(-2))\n";
        assert_eq!(
            migrated(source),
            source
                .replace("    (n + 1) // 2", "    return (n + 1) // 2")
                .replace("            2\n", "            return 2\n")
                .replace("        -1\n", "        return -1\n")
                .replace("        1\n\n", "        return 1\n\n")
        );
    }

    /// A block that's a `match why:` and then more lines has its cases moved up, each that
    /// doesn't leave ending with those lines, and a `case _:` that doesn't use `why` names what
    /// the cases before it leave.
    #[test]
    fn a_match_and_what_follows_it_become_the_cases() {
        let source = "error Bad:\n    Empty\n    Long(size: int)\n    Odd\n\n\
            def load(n: int) -> int fails Bad:\n    if n == 1: fail .Empty\n    return n\n\n\
            def shown(n: int, stdout: Stdout) -> int:\n    got = load(n) else as why:\n        match why:\n            \
            case .Long(size):\n                return size\n            case _:\n                stdout.print(\"short\")\n        \
            0\n    return got\n\n\
            def main(stdout: Stdout):\n    stdout.print(shown(1, stdout))\n";
        let expected = "    got = load(n) except why:\n        case .Long(size):\n            return size\n        \
            case .Empty | .Odd:\n            stdout.print(\"short\")\n            0\n    return got\n";
        let migrated = migrated(source);
        assert!(migrated.contains(expected), "{migrated}");
    }

    /// A case that names the failure again, `case error:`, becomes `case _:` with `error`
    /// given `why`, and `x += f() else d`, where `except` can't stand, gets a line of its own.
    #[test]
    fn a_case_naming_the_failure_again_reads_why() {
        let source = "error Bad:\n    Empty\n    Long(size: int)\n\n\
            def load(n: int) -> int fails Bad:\n    if n == 1: fail .Empty\n    return n\n\n\
            def shown(n: int, stdout: Stdout) -> int:\n    mut total = 0\n    total += load(n) else 1\n    \
            got = load(n) else as why:\n        match why:\n            case .Long(size):\n                return size\n            \
            case problem:\n                stdout.print(problem)\n                return -1\n    return got + total\n\n\
            def main(stdout: Stdout):\n    stdout.print(shown(1, stdout))\n";
        let expected = "    load_value = load(n) except .Empty | .Long(): 1\n    total += load_value\n    got = load(n) except why:\n        case .Long(size):\n            \
            return size\n        case _:\n            problem = why\n            stdout.print(problem)\n            return -1\n";
        let migrated = migrated(source);
        assert!(migrated.contains(expected), "{migrated}");
    }

    /// An `else` on a failing call inside a statement goes on a line of its own before it, into a
    /// name the statement reads, one for each, in order, and a `def`'s last line still gets its
    /// `return`. One inside an arrow function, or after a call that runs first, stays.
    #[test]
    fn an_else_inside_a_statement_goes_on_a_line_of_its_own() {
        let source = "error Bad:\n    Empty\n\n\
            def load(n: int) -> int fails Bad:\n    if n == 1: fail .Empty\n    return n\n\n\
            def shout(stdout: Stdout) -> int:\n    stdout.print(\"!\")\n    return 0\n\n\
            def both(n: int) -> int:\n    (load(n) else 0) + (load(n + 1) else 1)\n\n\
            def main(stdout: Stdout):\n    stdout.print(\"a\", load(1) else -1, load(2) else -2)\n    \
            stdout.print(both(2), shout(stdout), load(3) else 0)\n";
        let once = migrated(source);
        let twice = migrated(&once);
        assert!(
            twice.contains("    load_value = load(1) except .Empty: -1\n    load_value2 = load(2) except .Empty: -2\n    stdout.print(\"a\", load_value, load_value2)\n"),
            "{twice}"
        );
        assert!(twice.contains("    load_value = load(n) except .Empty: 0\n    load_value2 = load(n + 1) except .Empty: 1\n    return (load_value) + (load_value2)\n"), "{twice}");
        assert!(twice.contains("stdout.print(both(2), shout(stdout), load(3) else 0)"), "{twice}");
    }

    /// A loop's iterable runs once, before the loop: its `else` on a failing call goes on a line
    /// before it.
    #[test]
    fn an_else_a_loop_goes_over_goes_on_a_line_before_it() {
        let source = "error Bad:\n    Empty\n\n\
            def load(n: int) -> list[int] fails Bad:\n    if n == 1: fail .Empty\n    return [n]\n\n\
            def main(stdout: Stdout):\n    for found in load(2) else []:\n        stdout.print(found)\n";
        let migrated = migrated(source);
        assert!(
            migrated.contains("    load_value = load(2) except .Empty: []\n    for found in load_value:\n"),
            "{migrated}"
        );
    }

    /// A case whose body is on its own line, `case .Long(size): stdout.print(size)`, gets a
    /// block once the lines after the `match` are added to it.
    #[test]
    fn a_case_on_one_line_becomes_a_block_when_lines_follow_it() {
        let source = "error Bad:\n    Empty\n    Long(size: int)\n\n\
            def load(n: int) -> int fails Bad:\n    if n == 1: fail .Empty\n    return n\n\n\
            def shown(n: int, stdout: Stdout):\n    got = load(n) else as why:\n        match why:\n            \
            case .Long(size): stdout.print(size)\n            case .Empty: stdout.print(\"empty\")\n        return\n    \
            stdout.print(got)\n\n\
            def main(stdout: Stdout):\n    shown(1, stdout)\n";
        let migrated = migrated(source);
        assert!(
            migrated.contains("    got = load(n) except why:\n        case .Long(size):\n            stdout.print(size)\n            return\n        case .Empty:\n            stdout.print(\"empty\")\n            return\n"),
            "{migrated}"
        );
    }

    /// `x.len()` at the end of a chain whose step can fail or be missing stays: the chain's
    /// `else` ends it, and `len(...)` would hold the chain as an argument.
    #[test]
    fn a_len_ending_a_chain_that_can_fail_stays_a_method() {
        let source = "error Lost:\n    why: str\n\nstruct Part:\n    name = \"\"\n\n\
            def find(n: int) -> Part fails Lost:\n    if n > 1: fail Lost(why=\"big\")\n    return Part()\n\n\
            def main(stdout: Stdout):\n    size: int = find(1).name.len() else 0\n    \
            ages = {\"a\": \"x\"}\n    kept = ages.get(\"a\").len() else 0\n    stdout.print(size, kept, \"ab\".len())\n";
        let migrated = migrated(source);
        assert!(migrated.contains("find(1).name.len() else 0"), "{migrated}");
        assert!(migrated.contains("ages.get(\"a\").len() else 0"), "{migrated}");
        assert!(migrated.contains("len(\"ab\")"), "{migrated}");
    }

    /// A def's last value gets `return`, and a def that gives nothing is left as it is.
    #[test]
    fn a_defs_last_value_is_returned() {
        let source = "def two() -> int:\n    1 + 1\n\n\
            def shown(stdout: Stdout):\n    stdout.print(two())\n\n\
            def main(stdout: Stdout):\n    shown(stdout)\n";
        assert_eq!(migrated(source), source.replace("    1 + 1", "    return 1 + 1"));
    }
}
