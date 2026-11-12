//! Loops that say in many lines what a comprehension, a builtin or a `for` says in one. Each is
//! only flagged when the rewrite does exactly the same, which is why each looks so closely.

use std::collections::HashMap;

use fennec_syntax::{
    BinOp, CompKind, Diagnostic, Expr, ExprKind, FnDef, Module, Sources, Span, Stmt, StmtKind, TypeExpr,
};

use crate::Lint;
use crate::dead::{changed, has_word};
use crate::rewrites::{replaceable, written};
use crate::sizes::{blocks, end_of, functions};

/// How often each name appears in some statements, nested functions included.
fn names_in(stmts: &[Stmt]) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    fennec_syntax::read::in_stmts(stmts, &mut |expr| {
        if let ExprKind::Name(name) = &expr.kind {
            *counts.entry(name.clone()).or_default() += 1;
        }
    });
    counts
}

/// Whether an expression might change a name from outside a loop over `names`: it gives a call
/// a `ref`, or calls a method on an outside name, which may change it. Moved into a generator,
/// that's an error, since a generator only reads.
fn may_change_around(expr: &Expr, names: &[String]) -> bool {
    let mut changes = false;
    fennec_syntax::visit::in_expr(&mut expr.clone(), &mut |part| {
        if let ExprKind::Call { callee, args } = &part.kind {
            let outside_method = matches!(&callee.kind, ExprKind::Attr { object, .. }
                if root(object).is_some_and(|root| !names.iter().any(|name| name == root)));
            changes |= outside_method || args.iter().any(|arg| arg.is_mut);
        }
    });
    changes
}

/// The name a place starts from: `units` in `units[i].hp`.
fn root(expr: &Expr) -> Option<&str> {
    match &expr.kind {
        ExprKind::Name(name) => Some(name),
        ExprKind::Attr { object, .. } | ExprKind::Index { object, .. } => root(object),
        _ => None,
    }
}

fn count(counts: &HashMap<String, usize>, name: &str) -> usize {
    counts.get(name).copied().unwrap_or(0)
}

/// The source of an expression, if it can be moved whole. Spans leave out the brackets around a
/// leading or trailing operand, and moving text cut there would break it. One that leaves with
/// `else` stays in its loop, since a comprehension can't leave.
fn moved(sources: &Sources, expr: &Expr) -> Option<String> {
    let mut leaves = false;
    fennec_syntax::visit::in_expr(&mut expr.clone(), &mut |inner| leaves |= inner.leaves());
    (!leaves && replaceable(sources, expr)).then(|| written(sources, expr.span)).flatten()
}

/// Where a statement really ends: its span can run on to the end of its last line.
fn end_of_stmt(sources: &Sources, stmt: &Stmt) -> u32 {
    let end = end_of(std::slice::from_ref(stmt)).max(stmt.span.end);
    let text = written(sources, Span { start: stmt.span.start, end }).unwrap_or_default();
    end - u32::try_from(text.len() - text.trim_end().len()).unwrap_or(0)
}

/// The whole lines a span is on, from the start of its first to just past its last.
fn whole_lines(sources: &Sources, span: Span) -> Option<Span> {
    let (_, text, base) = sources.file_at(span)?;
    let start = usize::try_from(span.start.checked_sub(base)?).ok()?;
    let end = usize::try_from(span.end.checked_sub(base)?).ok()?;
    let line_start = text.get(..start)?.rfind('\n').map_or(0, |at| at + 1);
    let line_end = text.get(end..)?.find('\n').map_or(text.len(), |at| end + at + 1);
    let at = |offset: usize| u32::try_from(offset).ok().map(|offset| offset + base);
    Some(Span { start: at(line_start)?, end: at(line_end)? })
}

/// Whether a name appears anywhere in the file a span is in.
fn in_file(sources: &Sources, span: Span, name: &str) -> bool {
    sources.file_at(span).is_none_or(|(_, text, _)| has_word(text, name))
}

/// `mut name = value` as written, if a statement is that.
fn declared(stmt: &Stmt) -> Option<(&str, &Expr)> {
    let StmtKind::Assign { target, op: None, value, declare_mut: true, ty: None } = &stmt.kind else {
        return None;
    };
    let ExprKind::Name(name) = &target.kind else { return None };
    Some((name, value))
}

/// `for i, x in enumerate(xs)` where `i` isn't used.
fn needless_enumerate(stmt: &Stmt, sources: &Sources) -> Option<Lint> {
    let StmtKind::For { names, is_mut: false, iter, body } = &stmt.kind else { return None };
    let [index, item] = names.as_slice() else { return None };
    let ExprKind::Call { callee, args } = &iter.kind else { return None };
    let (ExprKind::Name(called), [arg]) = (&callee.kind, args.as_slice()) else { return None };
    if called != "enumerate" || arg.name.is_some() || count(&names_in(body), index) > 0 {
        return None;
    }
    let header = Span { start: stmt.span.start, end: iter.span.end };
    let fixed = format!("for {item} in {}", moved(sources, &arg.value)?);
    let diagnostic =
        Diagnostic::new(format!("`{index}` isn't used: write `{fixed}`"), header).with_fix(header, fixed);
    Some(Lint::new("needless-enumerate", diagnostic))
}

/// What a loop body adds to a list, and when, if that's all it does: `out.append(e)`, maybe
/// inside an `if` with no `else`.
fn appended<'s>(body: &'s [Stmt], list: &str) -> Option<(&'s Expr, Option<&'s Expr>)> {
    let ([stmt], cond) = (match body {
        [Stmt { kind: StmtKind::If { branches, otherwise: None }, .. }] => match branches.as_slice() {
            [(cond, inner)] => (inner.as_slice(), Some(cond)),
            _ => return None,
        },
        _ => (body, None),
    }) else {
        return None;
    };
    let StmtKind::Expr(Expr { kind: ExprKind::Call { callee, args }, .. }) = &stmt.kind else { return None };
    let ExprKind::Attr { object, name, .. } = &callee.kind else { return None };
    let [arg] = args.as_slice() else { return None };
    let on_list = matches!(&object.kind, ExprKind::Name(object) if object == list);
    (on_list && name == "append" && arg.name.is_none() && !arg.is_mut).then_some((&arg.value, cond))
}

/// `mut out = []` then a loop that only appends, or `mut total = 0` then one that only adds.
fn built_by_loop(declare: &Stmt, loop_stmt: &Stmt, function: &FnDef, sources: &Sources) -> Option<Lint> {
    let (name, value) = declared(declare)?;
    let StmtKind::For { names, is_mut: false, iter, body } = &loop_stmt.kind else { return None };
    // A sum or a list built by the loop is only added to, never read inside it.
    let only_built = count(&names_in(body), name) <= 1;
    let looped = names;
    let names = names.join(", ");
    let iter = moved(sources, iter)?;
    let (rule, built) = match &value.kind {
        ExprKind::List(items) if items.is_empty() && only_built => {
            let (element, cond) = appended(body, name)?;
            let cond = match cond {
                Some(cond) => format!(" if {}", moved(sources, cond)?),
                None => String::new(),
            };
            ("manual-comprehension", format!("[{} for {names} in {iter}{cond}]", moved(sources, element)?))
        }
        ExprKind::Int(0) | ExprKind::Float(0.0) if only_built => {
            let [Stmt { kind: StmtKind::Assign { target, op: Some(BinOp::Add), value: added, .. }, .. }] =
                body.as_slice()
            else {
                return None;
            };
            let to_total = matches!(&target.kind, ExprKind::Name(target) if target == name);
            if !to_total || may_change_around(added, looped) {
                return None;
            }
            ("manual-builtin", format!("sum({} for {names} in {iter})", moved(sources, added)?))
        }
        ExprKind::Bool(false) => {
            let cond = found_when(body, name).filter(|cond| !may_change_around(cond, looped))?;
            ("manual-builtin", format!("any({} for {names} in {iter})", moved(sources, cond)?))
        }
        ExprKind::Index { object, index } if matches!(index.kind, ExprKind::Int(0)) => {
            let ExprKind::Name(list) = &object.kind else { return None };
            ("manual-builtin", extreme(loop_stmt, name, list, function)?)
        }
        _ => return None,
    };
    let span = Span { start: declare.span.start, end: end_of_stmt(sources, loop_stmt) };
    let fixed = format!("mut {name} = {built}");
    let diagnostic = Diagnostic::new(format!("write this as `{fixed}`"), span).with_fix(span, fixed);
    Some(Lint::new(rule, diagnostic))
}

/// The condition a loop body sets a flag on and stops at, if that's all it does:
/// `if cond: flag = True` then `break`. Stopping at the first is what `any` does, so the
/// condition runs just as many times either way. Without the `break` the loop would run the
/// condition for every item, and `any` wouldn't.
fn found_when<'s>(body: &'s [Stmt], flag: &str) -> Option<&'s Expr> {
    let [Stmt { kind: StmtKind::If { branches, otherwise: None }, .. }] = body else { return None };
    let [(cond, inner)] = branches.as_slice() else { return None };
    let [set, Stmt { kind: StmtKind::Break, .. }] = inner.as_slice() else { return None };
    let StmtKind::Assign { target, op: None, value, declare_mut: false, ty: None } = &set.kind else {
        return None;
    };
    let sets_flag = matches!(&target.kind, ExprKind::Name(target) if target == flag)
        && matches!(value.kind, ExprKind::Bool(true));
    // The flag is only set, never read.
    (sets_flag && count(&names_in(body), flag) == 1).then_some(cond)
}

/// Whether a name is a parameter written as a list of ints or strings, whose items have one
/// order that `max` and `min` share with `<` and `>`. Floats don't: `max` puts NaN above
/// everything, and `>` says nothing is above NaN.
fn ints_or_strs(name: &str, function: &FnDef) -> bool {
    let written_as = |ty: &TypeExpr| {
        ty.name == "list"
            && !ty.optional
            && matches!(ty.args.as_slice(), [item] if matches!(item.name.as_str(), "int" | "str") && !item.optional)
    };
    function.params.iter().any(|param| param.name == name && param.ty.as_ref().is_some_and(written_as))
}

/// `max(xs)` or `min(xs)` for a loop that keeps the largest or smallest of `xs` in `best`,
/// which starts as `xs[0]`: `for x in xs` or `for x in xs[1:]`, with `if x > best: best = x`.
/// Both fail on an empty list, as `xs[0]` does.
fn extreme(loop_stmt: &Stmt, best: &str, list: &str, function: &FnDef) -> Option<String> {
    let StmtKind::For { names, iter, body, .. } = &loop_stmt.kind else { return None };
    let [item] = names.as_slice() else { return None };
    let whole = match &iter.kind {
        ExprKind::Name(name) => name == list,
        ExprKind::Slice { object, start: Some(start), end: None } => {
            matches!(&object.kind, ExprKind::Name(name) if name == list)
                && matches!(start.kind, ExprKind::Int(1))
        }
        _ => false,
    };
    if !whole || !ints_or_strs(list, function) {
        return None;
    }
    let [Stmt { kind: StmtKind::If { branches, otherwise: None }, .. }] = body.as_slice() else {
        return None;
    };
    let [(cond, inner)] = branches.as_slice() else { return None };
    let [Stmt { kind: StmtKind::Assign { target, op: None, value, declare_mut: false, ty: None }, .. }] =
        inner.as_slice()
    else {
        return None;
    };
    let is = |expr: &Expr, name: &str| matches!(&expr.kind, ExprKind::Name(written) if written == name);
    if !is(target, best) || !is(value, item) {
        return None;
    }
    let ExprKind::Binary { op, lhs, rhs } = &cond.kind else { return None };
    let larger = match (op, is(lhs, item) && is(rhs, best), is(lhs, best) && is(rhs, item)) {
        (BinOp::Gt | BinOp::GtEq, true, _) | (BinOp::Lt | BinOp::LtEq, _, true) => true,
        (BinOp::Lt | BinOp::LtEq, true, _) | (BinOp::Gt | BinOp::GtEq, _, true) => false,
        _ => return None,
    };
    Some(format!("{}({list})", if larger { "max" } else { "min" }))
}

/// Whether a value stays the same while a loop runs: a number, or a name or a length of one that
/// the function never changes.
fn steady(expr: &Expr, never_changed: &dyn Fn(&str) -> bool) -> bool {
    match &expr.kind {
        ExprKind::Int(_) => true,
        ExprKind::Name(name) => never_changed(name),
        ExprKind::Call { .. } => length_of(expr).is_some_and(never_changed),
        _ => false,
    }
}

/// The name whose length an expression is: `xs` in `len(xs)`, or in `xs.len()`, as code written
/// before `len` was a builtin has it.
fn length_of(expr: &Expr) -> Option<&str> {
    let ExprKind::Call { callee, args } = &expr.kind else { return None };
    let measured = match (&callee.kind, args.as_slice()) {
        (ExprKind::Name(called), [arg]) if called == "len" && arg.name.is_none() && !arg.is_mut => &arg.value,
        (ExprKind::Attr { object, name, .. }, []) if name == "len" => object,
        _ => return None,
    };
    match &measured.kind {
        ExprKind::Name(name) => Some(name),
        _ => None,
    }
}

/// Whether a block has a `continue` anywhere in it, which would skip a counter's last line.
fn continues(stmts: &[Stmt]) -> bool {
    stmts
        .iter()
        .any(|stmt| matches!(stmt.kind, StmtKind::Continue) || blocks(stmt).into_iter().any(continues))
}

/// `mut i = 0` then `while i < n:` whose last line is `i += 1`, as `for i in range(n):`.
fn counted_while(declare: &Stmt, loop_stmt: &Stmt, function: &FnDef, sources: &Sources) -> Option<Vec<Lint>> {
    let (name, value) = declared(declare)?;
    let ExprKind::Int(start) = value.kind else { return None };
    let StmtKind::While { cond, body } = &loop_stmt.kind else { return None };
    let ExprKind::Binary { op: BinOp::Lt, lhs, rhs: limit } = &cond.kind else { return None };
    let (last, rest) = body.split_last()?;
    let StmtKind::Assign { target, op: Some(BinOp::Add), value: step, .. } = &last.kind else { return None };
    let counts = matches!(&lhs.kind, ExprKind::Name(counter) if counter == name)
        && matches!(&target.kind, ExprKind::Name(counter) if counter == name)
        && matches!(step.kind, ExprKind::Int(1));
    let changed_in_function = changed(&function.body);
    let never_changed = |other: &str| other != name && !changed_in_function.contains(other);
    let only_here =
        count(&names_in(&function.body), name) == 1 + count(&names_in(std::slice::from_ref(loop_stmt)), name);
    if !counts
        || !only_here
        || continues(body)
        || changed(rest).contains(name)
        || !steady(limit, &never_changed)
    {
        return None;
    }
    let range = match start {
        0 => format!("range({})", moved(sources, limit)?),
        start => format!("range({start}, {})", moved(sources, limit)?),
    };
    let header = Span { start: declare.span.start, end: cond.span.end };
    let fixed = format!("for {name} in {range}");
    let diagnostic = Diagnostic::new(format!("this counts with `{name}`: write `{fixed}:`"), header)
        .with_fix(header, fixed)
        .with_fix(whole_lines(sources, last.span)?, "");
    Some(vec![Lint::new("counted-while", diagnostic)])
}

/// Whether a name holds a list or a string for certain: a parameter written as one, or a local
/// made from a list literal or a list comprehension.
fn is_list_or_str(name: &str, function: &FnDef) -> bool {
    let written_as = |ty: &TypeExpr| matches!(ty.name.as_str(), "list" | "str") && !ty.optional;
    let param =
        function.params.iter().any(|param| param.name == name && param.ty.as_ref().is_some_and(written_as));
    param || made_as_list(&function.body, name)
}

fn made_as_list(stmts: &[Stmt], name: &str) -> bool {
    stmts.iter().any(|stmt| {
        let made = matches!(&stmt.kind, StmtKind::Assign { target, value, op: None, .. }
            if matches!(&target.kind, ExprKind::Name(target) if target == name)
                && matches!(value.kind, ExprKind::List(_) | ExprKind::Comprehension { kind: CompKind::List, .. }));
        made || blocks(stmt).into_iter().any(|block| made_as_list(block, name))
    })
}

/// Every `list[index]` in some statements.
fn indexes(stmts: &[Stmt], list: &str, index: &str) -> Vec<Span> {
    let mut found = Vec::new();
    fennec_syntax::read::in_stmts(stmts, &mut |expr| {
        if let ExprKind::Index { object, index: at } = &expr.kind
            && matches!(&object.kind, ExprKind::Name(object) if object == list)
            && matches!(&at.kind, ExprKind::Name(at) if at == index)
        {
            found.push(expr.span);
        }
    });
    found
}

/// A name for one item of a list called `list`: `enemy` for `enemies`, `point` for `points`.
fn item_name(list: &str) -> String {
    if let Some(stem) = list.strip_suffix("ies").filter(|stem| !stem.is_empty()) {
        return format!("{stem}y");
    }
    match list.strip_suffix('s') {
        Some(stem) if !stem.is_empty() && !stem.ends_with('s') => stem.to_owned(),
        _ => "item".to_owned(),
    }
}

/// `for i in range(len(xs)):`, or `range(xs.len())`, where `i` is only used as `xs[i]`, as
/// `for x in xs:`.
fn range_len(stmt: &Stmt, function: &FnDef, sources: &Sources) -> Option<Lint> {
    let StmtKind::For { names, is_mut: false, iter, body } = &stmt.kind else { return None };
    let [index] = names.as_slice() else { return None };
    let ExprKind::Call { callee, args } = &iter.kind else { return None };
    let (ExprKind::Name(called), [arg]) = (&callee.kind, args.as_slice()) else { return None };
    let list = length_of(&arg.value)?;
    let uses = indexes(body, list, index);
    let only_indexes = !uses.is_empty() && uses.len() == count(&names_in(body), index);
    let item = item_name(list);
    let usable = called == "range" && arg.name.is_none() && only_indexes;
    if !usable
        || !is_list_or_str(list, function)
        || changed(&without_header(&function.body, stmt.span)).contains(list)
    {
        return None;
    }
    if in_file(sources, stmt.span, &item) {
        return None;
    }
    let header = Span { start: stmt.span.start, end: iter.span.end };
    let fixed = format!("for {item} in {list}");
    let mut diagnostic = Diagnostic::new(format!("`{index}` only indexes `{list}`: write `{fixed}`"), header)
        .with_fix(header, fixed);
    for span in uses {
        diagnostic = diagnostic.with_fix(span, item.clone());
    }
    Some(Lint::new("range-len", diagnostic))
}

/// A function's body with the loop at `span` reading `0` in place of what it loops over, so that
/// asking for the length there doesn't count as a call that may change the list.
fn without_header(stmts: &[Stmt], span: Span) -> Vec<Stmt> {
    let mut body = stmts.to_vec();
    blank_header(&mut body, span);
    body
}

fn blank_header(stmts: &mut [Stmt], span: Span) {
    for stmt in stmts {
        if stmt.span == span
            && let StmtKind::For { iter, .. } = &mut stmt.kind
        {
            iter.kind = ExprKind::Int(0);
            return;
        }
        match &mut stmt.kind {
            StmtKind::If { branches, otherwise } => {
                for (_, body) in branches {
                    blank_header(body, span);
                }
                blank_header(otherwise.as_deref_mut().unwrap_or_default(), span);
            }
            StmtKind::While { body, .. }
            | StmtKind::For { body, .. }
            | StmtKind::With { body, .. }
            | StmtKind::OnFailure { body, .. } => {
                blank_header(body, span);
            }
            _ => {}
        }
    }
}

fn block(stmts: &[Stmt], function: &FnDef, sources: &Sources, found: &mut Vec<Lint>) {
    for (at, stmt) in stmts.iter().enumerate() {
        found.extend(needless_enumerate(stmt, sources));
        found.extend(range_len(stmt, function, sources));
        if let Some(next) = stmts.get(at + 1) {
            found.extend(built_by_loop(stmt, next, function, sources));
            found.extend(counted_while(stmt, next, function, sources).into_iter().flatten());
        }
        for inner in blocks(stmt) {
            block(inner, function, sources, found);
        }
    }
}

pub(crate) fn of(module: &Module, sources: &Sources) -> Vec<Lint> {
    let mut found = Vec::new();
    for function in functions(module) {
        block(&function.body, function, sources, &mut found);
    }
    found
}
