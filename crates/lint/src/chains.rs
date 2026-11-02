//! `elif-to-match`: an `if` chain that compares one name with `==` to a different literal in
//! each of three or more branches says what a `match` says, and `fennec fix` writes the match.
//! A `match` with no case for a value stops the program, so a chain without an `else` gets
//! `case _: pass`, which does nothing, as the chain did.

use std::fmt::Write as _;

use fennec_syntax::{BinOp, Diagnostic, Expr, ExprKind, Sources, Span, Stmt, StmtKind};

use crate::Lint;
use crate::sizes::{blocks, functions};

/// The fewest branches that make a chain a `match`.
const FEWEST: usize = 3;

/// The name and the literal's source when `cond` is `name == literal` or `literal == name`,
/// with an int or a string literal.
fn compared<'e>(cond: &'e Expr, sources: &Sources) -> Option<(&'e str, String)> {
    let ExprKind::Binary { op: BinOp::Eq, lhs, rhs } = &cond.kind else { return None };
    let literal = |expr: &Expr| matches!(expr.kind, ExprKind::Int(_) | ExprKind::Str(_));
    let (name, value) = match (&lhs.kind, &rhs.kind) {
        (ExprKind::Name(name), _) if literal(rhs) => (name, rhs),
        (_, ExprKind::Name(name)) if literal(lhs) => (name, lhs),
        _ => return None,
    };
    Some((name.as_str(), crate::rewrites::written(sources, value.span)?))
}

/// A file's text and the span's place in it.
fn local(sources: &Sources, span: Span) -> Option<(&str, usize, usize)> {
    let (_, text, base) = sources.file_at(span)?;
    let at = |offset: u32| usize::try_from(offset.checked_sub(base)?).ok();
    Some((text, at(span.start)?, at(span.end)?))
}

/// A block's statements as written, one level deeper: each line moved right by four spaces.
/// None when there's something besides the statements that the move would lose, like a
/// comment before the first.
fn body(
    text: &str,
    header_end: usize,
    stmts: &[Stmt],
    sources: &Sources,
    inside: &str,
) -> Option<(String, usize)> {
    let first = local(sources, stmts.first()?.span)?.1;
    // A block's span runs past its last line's newline, which stays where it is.
    let last = local(sources, stmts.last()?.span)?.2;
    let last = first + text.get(first..last)?.trim_end().len();
    let between = text.get(header_end..first)?;
    if between.contains('#') || !between.trim_start().starts_with(':') {
        return None;
    }
    let written = text.get(first..last)?;
    if !between.contains('\n') {
        // `if n == 1: return 0`: the body goes on its own line.
        return (!written.contains('\n')).then(|| (format!("{inside}{written}"), last));
    }
    let line_start = text.get(..first)?.rfind('\n').map_or(0, |at| at + 1);
    let lines = text.get(line_start..last)?;
    let moved: Vec<String> = lines
        .split('\n')
        .map(|line| if line.trim().is_empty() { String::new() } else { format!("    {line}") })
        .collect();
    Some((moved.join("\n"), last))
}

/// The `match` a chain is, written at `indent`, and where the chain ends, if it is one.
fn as_match(stmt: &Stmt, sources: &Sources) -> Option<(String, String, usize)> {
    let StmtKind::If { branches, otherwise } = &stmt.kind else { return None };
    if branches.len() < FEWEST {
        return None;
    }
    let mut subject = None;
    let mut values: Vec<String> = Vec::new();
    for (cond, _) in branches {
        let (name, value) = compared(cond, sources)?;
        if subject.is_some_and(|subject| subject != name) || values.contains(&value) {
            return None;
        }
        subject = Some(name);
        values.push(value);
    }
    let (text, start, _) = local(sources, stmt.span)?;
    let line_start = text.get(..start)?.rfind('\n').map_or(0, |at| at + 1);
    let indent = text.get(line_start..start)?;
    if !indent.chars().all(|ch| ch == ' ') {
        return None;
    }
    let (case, inside) = (format!("{indent}    "), format!("{indent}        "));
    let mut written = format!("match {}:", subject?);
    let mut end = start;
    for ((cond, stmts), value) in branches.iter().zip(&values) {
        let (_, cond_start, cond_end) = local(sources, cond.span)?;
        // Between one branch and the next is only `elif`.
        let keyword = text.get(end..cond_start)?.trim();
        if end != start && keyword != "elif" {
            return None;
        }
        let (lines, last) = body(text, cond_end, stmts, sources, &inside)?;
        let _ = write!(written, "\n{case}case {value}:\n{lines}");
        end = last;
    }
    match otherwise {
        Some(stmts) => {
            let (_, first, _) = local(sources, stmts.first()?.span)?;
            let keyword_end = end + text.get(end..first)?.find("else")? + "else".len();
            if text.get(end..keyword_end)?.trim() != "else" {
                return None;
            }
            let (lines, last) = body(text, keyword_end, stmts, sources, &inside)?;
            let _ = write!(written, "\n{case}case _:\n{lines}");
            end = last;
        }
        None => {
            let _ = write!(written, "\n{case}case _:\n{inside}pass");
        }
    }
    Some((subject?.to_owned(), written, end))
}

/// Each chain in `stmts` and the blocks inside them. A chain's own blocks wait for the next
/// run, so two fixes never overlap.
fn chains(stmts: &[Stmt], sources: &Sources, found: &mut Vec<Lint>) {
    for stmt in stmts {
        if let Some((subject, written, end)) = as_match(stmt, sources)
            && let Some((_, _, base)) = sources.file_at(stmt.span)
            && let Ok(end) = u32::try_from(end)
        {
            let span = Span { start: stmt.span.start, end: base + end };
            let message = format!("each branch compares `{subject}` with a value: write this as a `match`");
            let diagnostic =
                Diagnostic::new(message, Span { start: stmt.span.start, end: stmt.span.start + 2 })
                    .with_fix(span, written);
            found.push(Lint::new("elif-to-match", diagnostic));
            continue;
        }
        for block in blocks(stmt) {
            chains(block, sources, found);
        }
    }
}

pub(crate) fn of(module: &fennec_syntax::Module, sources: &Sources) -> Vec<Lint> {
    let mut found = Vec::new();
    let tests = module.items.iter().filter_map(|item| match item {
        fennec_syntax::Item::Test(test) => Some(&test.body),
        _ => None,
    });
    for body in functions(module).into_iter().map(|def| &def.body).chain(tests) {
        chains(body, sources, &mut found);
    }
    found
}
