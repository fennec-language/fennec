//! Dead code: imports nothing uses, and locals nothing reads.

use std::collections::HashMap;

use fennec_syntax::{
    Diagnostic, ExprKind, FnDef, Import, Item, Module, Sources, Span, Stmt, StmtKind, TokenKind, lex_source,
    parse_at,
};

use crate::Lint;
use crate::sizes::{blocks, functions};

/// Whether `text` has `name` in it as a whole word.
fn has_word(text: &str, name: &str) -> bool {
    text.match_indices(name).any(|(at, _)| {
        let word = |ch: char| ch.is_alphanumeric() || ch == '_';
        let before = text.get(..at).and_then(|before| before.chars().next_back());
        let after = text.get(at + name.len()..).and_then(|after| after.chars().next());
        !before.is_some_and(word) && !after.is_some_and(word)
    })
}

/// The names a file uses outside its imports, as names or inside f-strings.
fn used_in(text: &str, imports: &[Span]) -> (Vec<String>, Vec<String>) {
    let Ok(lexed) = lex_source(text) else { return (Vec::new(), Vec::new()) };
    let outside =
        |span: Span| !imports.iter().any(|import| import.start <= span.start && span.end <= import.end);
    let mut names = Vec::new();
    let mut fstrings = Vec::new();
    for token in lexed.tokens.iter().filter(|token| outside(token.span)) {
        let Some(written) = text.get(token.span.start as usize..token.span.end as usize) else { continue };
        match token.kind {
            TokenKind::Name => names.push(written.to_owned()),
            TokenKind::FStr => fstrings.push(written.to_owned()),
            _ => {}
        }
    }
    (names, fstrings)
}

/// An import as it's written, with only some of its names.
fn written(import: &Import, keep: &[usize]) -> String {
    let names: Vec<String> = keep
        .iter()
        .filter_map(|at| {
            let name = import.names.get(*at)?;
            Some(match import.aliases.get(*at).and_then(Option::as_deref) {
                Some(alias) => format!("{name} as {alias}"),
                None => name.clone(),
            })
        })
        .collect();
    format!("from {} import {}", import.module.join("."), names.join(", "))
}

/// The span from `start` to the end of its line, newline included, in a file at `base`.
fn through_line_end(text: &str, base: u32, span: Span) -> Span {
    let end = (span.end - base) as usize;
    let line_end = text.get(end..).and_then(|rest| rest.find('\n')).map_or(text.len(), |at| end + at + 1);
    Span { start: span.start, end: base + u32::try_from(line_end).unwrap_or(0) }
}

/// Imports in one file that nothing in that file uses.
fn unused_imports(text: &str, base: u32, found: &mut Vec<Lint>) {
    let Ok(module) = parse_at(text, base) else { return };
    let imports: Vec<&Import> = module
        .items
        .iter()
        .filter_map(|item| if let Item::Import(import) = item { Some(import) } else { None })
        .collect();
    let local = |span: Span| Span { start: span.start - base, end: span.end - base };
    let (names, fstrings) =
        used_in(text, &imports.iter().map(|import| local(import.span)).collect::<Vec<_>>());
    let used = |bound: &str| {
        names.iter().any(|name| name == bound) || fstrings.iter().any(|text| has_word(text, bound))
    };
    for import in imports {
        if import.names.is_empty() {
            let bound =
                import.alias.as_deref().or_else(|| import.module.first().map(String::as_str)).unwrap_or("");
            if !used(bound) {
                let span = through_line_end(text, base, import.span);
                let diagnostic =
                    Diagnostic::new(format!("nothing uses `{bound}`"), import.span).with_fix(span, "");
                found.push(Lint::new("unused-import", diagnostic));
            }
            continue;
        }
        let bound: Vec<&str> = import.bound().map(|(_, bound)| bound).collect();
        let keep: Vec<usize> =
            (0..bound.len()).filter(|at| bound.get(*at).is_some_and(|name| used(name))).collect();
        if keep.len() == bound.len() {
            continue;
        }
        let unused: Vec<&str> = (0..bound.len())
            .filter(|at| !keep.contains(at))
            .filter_map(|at| bound.get(at).copied())
            .collect();
        let message = format!(
            "nothing uses {}",
            unused.iter().map(|name| format!("`{name}`")).collect::<Vec<_>>().join(", ")
        );
        let diagnostic = if keep.is_empty() {
            Diagnostic::new(message, import.span).with_fix(through_line_end(text, base, import.span), "")
        } else {
            Diagnostic::new(message, import.span).with_fix(import.span, written(import, &keep))
        };
        found.push(Lint::new("unused-import", diagnostic));
    }
}

/// How often each name is read in a body: every time it appears, less when it's only assigned.
fn reads(body: &[Stmt]) -> HashMap<String, i64> {
    let mut counts: HashMap<String, i64> = HashMap::new();
    let mut walked = FnDef {
        name: String::new(),
        generics: Vec::new(),
        params: Vec::new(),
        ret: None,
        arrow: None,
        body: body.to_vec(),
        span: Span::default(),
    };
    fennec_syntax::visit::in_function(&mut walked, &mut |expr| {
        if let ExprKind::Name(name) = &expr.kind {
            *counts.entry(name.clone()).or_default() += 1;
        }
    });
    assigned_only(body, &mut counts);
    counts
}

/// Takes away each plain assignment to a name, which writes it without reading it.
fn assigned_only(stmts: &[Stmt], counts: &mut HashMap<String, i64>) {
    for stmt in stmts {
        if let StmtKind::Assign { target, op: None, .. } = &stmt.kind
            && let ExprKind::Name(name) = &target.kind
        {
            *counts.entry(name.clone()).or_default() -= 1;
        }
        if let StmtKind::Def(def) = &stmt.kind {
            assigned_only(&def.body, counts);
        }
        for block in blocks(stmt) {
            assigned_only(block, counts);
        }
    }
}

/// Locals assigned and never read. A block's last statement is left alone, since its value may be
/// what the block gives back.
fn unused_names(stmts: &[Stmt], counts: &HashMap<String, i64>, found: &mut Vec<Lint>) {
    let last = stmts.len().saturating_sub(1);
    for (at, stmt) in stmts.iter().enumerate() {
        if let StmtKind::Assign { target, op: None, value, .. } = &stmt.kind
            && let ExprKind::Name(name) = &target.kind
            && at != last
            && !name.starts_with('_')
            && counts.get(name).is_some_and(|reads| *reads <= 0)
        {
            let prefix = Span { start: stmt.span.start, end: value.span.start };
            let diagnostic =
                Diagnostic::new(format!("nothing reads `{name}`"), target.span).with_fix(prefix, "");
            found.push(Lint::new("unused-name", diagnostic));
        }
        for block in blocks(stmt) {
            unused_names(block, counts, found);
        }
    }
}

pub(crate) fn of(module: &Module, sources: &Sources) -> Vec<Lint> {
    let mut found = Vec::new();
    for (path, text) in sources.files() {
        if let Some((_, base)) = sources.by_path(path) {
            unused_imports(text, base, &mut found);
        }
    }
    let tests = module
        .items
        .iter()
        .filter_map(|item| if let Item::Test(test) = item { Some(&test.body) } else { None });
    for body in functions(module).into_iter().map(|def| &def.body).chain(tests) {
        unused_names(body, &reads(body), &mut found);
    }
    found
}
