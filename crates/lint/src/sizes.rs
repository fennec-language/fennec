//! Lints that count: nothing to fix automatically, and nothing they can get wrong.

use fennec_syntax::{Diagnostic, FnDef, Item, Module, Param, Sources, Span, Stmt, StmtKind};

use crate::Lint;

const MOST_FUNCTION_LINES: usize = 80;
const MOST_FILE_LINES: usize = 800;
const MOST_PARAMS: usize = 6;
const MOST_NESTING: usize = 4;

/// What programs are handed rather than what they work on, so they don't count as parameters.
const CAPABILITIES: [&str; 5] = ["Stdout", "Fs", "Clock", "Args", "Stdin"];

/// Every function of a module, with methods and those nested inside others.
pub(crate) fn functions(module: &Module) -> Vec<&FnDef> {
    let mut found = Vec::new();
    for item in &module.items {
        match item {
            Item::Function(def) => found.push(def),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => found.extend(&def.methods),
            Item::Extend(extension) => found.extend(&extension.methods),
            _ => {}
        }
    }
    let mut at = 0;
    while let Some(def) = found.get(at).copied() {
        nested(&def.body, &mut found);
        at += 1;
    }
    found
}

/// The blocks directly inside a statement.
pub(crate) fn blocks(stmt: &Stmt) -> Vec<&[Stmt]> {
    match &stmt.kind {
        StmtKind::If { branches, otherwise } => branches
            .iter()
            .map(|(_, body)| body.as_slice())
            .chain(otherwise.iter().map(Vec::as_slice))
            .collect(),
        StmtKind::While { body, .. } | StmtKind::For { body, .. } | StmtKind::With { body, .. } => {
            vec![body.as_slice()]
        }
        _ => Vec::new(),
    }
}

fn nested<'m>(stmts: &'m [Stmt], found: &mut Vec<&'m FnDef>) {
    for stmt in stmts {
        if let StmtKind::Def(def) = &stmt.kind {
            found.push(def);
        }
        for block in blocks(stmt) {
            nested(block, found);
        }
    }
}

/// How many blocks deep the deepest statement in `stmts` is, with where it is.
fn deepest(stmts: &[Stmt], depth: usize) -> (usize, Span) {
    let mut found = (depth, stmts.first().map_or_else(Span::default, |stmt| stmt.span));
    for stmt in stmts {
        for block in blocks(stmt) {
            let inner = deepest(block, depth + 1);
            if inner.0 > found.0 {
                found = inner;
            }
        }
    }
    found
}

/// Where the last thing in a block ends.
pub(crate) fn end_of(stmts: &[Stmt]) -> u32 {
    let inner = stmts.iter().flat_map(blocks).map(end_of);
    inner.chain(stmts.iter().map(|stmt| stmt.span.end)).max().unwrap_or(0)
}

fn counts(param: &Param) -> bool {
    !param.is_self && !param.ty.as_ref().is_some_and(|ty| CAPABILITIES.contains(&ty.name.as_str()))
}

fn function(def: &FnDef, sources: &Sources) -> Vec<Lint> {
    let mut found = Vec::new();
    let span = def.span;
    let end = end_of(&def.body).max(span.end);
    let text = sources.file_at(span).and_then(|(_, text, base)| {
        let start = usize::try_from(span.start.checked_sub(base)?).ok()?;
        let stop = usize::try_from(end.checked_sub(base)?).ok()?;
        text.get(start..stop)
    });
    let lines = text.map_or(0, |text| text.lines().count());
    if lines > MOST_FUNCTION_LINES {
        let message = format!("`{}` is {lines} lines, and a function can be {MOST_FUNCTION_LINES}", def.name);
        found.push(Lint::new("long-function", Diagnostic::new(message, span)));
    }
    let params = def.params.iter().filter(|param| counts(param)).count();
    if params > MOST_PARAMS {
        let message =
            format!("`{}` takes {params} parameters, and a function can take {MOST_PARAMS}", def.name);
        let diagnostic = Diagnostic::new(message, span).with_help("group them in a struct with defaults");
        found.push(Lint::new("too-many-params", diagnostic));
    }
    let (depth, at) = deepest(&def.body, 0);
    if depth > MOST_NESTING {
        let message =
            format!("this is {depth} blocks deep in `{}`, and code can go {MOST_NESTING}", def.name);
        let diagnostic =
            Diagnostic::new(message, at).with_help("return early, or move the inside to a function");
        found.push(Lint::new("deep-nesting", diagnostic));
    }
    found
}

pub(crate) fn of(module: &Module, sources: &Sources) -> Vec<Lint> {
    let mut found: Vec<Lint> = functions(module).into_iter().flat_map(|def| function(def, sources)).collect();
    for (path, text) in sources.files() {
        let lines = text.lines().count();
        if lines > MOST_FILE_LINES {
            let base = sources.by_path(path).map_or(0, |(_, base)| base);
            let message = format!("this file is {lines} lines, and a file can be {MOST_FILE_LINES}");
            let span = Span { start: base, end: base };
            found.push(Lint::new("long-file", Diagnostic::new(message, span)));
        }
    }
    found
}
