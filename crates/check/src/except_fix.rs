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

use fennec_syntax::{Diagnostic, Expr, ExprKind, FnDef, Item, Module, Sources, Span, Stmt, StmtKind};

use crate::types::Type;

/// The fixes that move a program to the forms of 2026-11-11, for `fennec fix --migrate`.
#[must_use]
pub fn migration_fixes(module: &Module, sources: &Sources) -> Vec<Diagnostic> {
    let mut found = Vec::new();
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
    for def in all.iter().filter(|def| !def.span.in_prelude()) {
        found.extend(def_return(def));
        let mut stmts = Vec::new();
        statements(&def.body, &mut stmts);
        found.extend(stmts.iter().filter_map(|stmt| lend_except(stmt, sources)));
    }
    found.extend(failure_fixes(module, sources));
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

/// `return` before a `def`'s last line, when that line is the value it gives back: a value, or
/// a `match` whose cases each give one, or a `with` block whose last line does.
fn def_return(def: &FnDef) -> Option<Diagnostic> {
    let ret = def.ret.as_ref()?;
    if ret.name == "None"
        || ret.name == "with"
        || def.body.iter().any(|stmt| matches!(stmt.kind, StmtKind::Yield(_)))
    {
        return None;
    }
    let value = tail(&def.body)?;
    let at = Span { start: value.span.start, end: value.span.start };
    Some(Diagnostic::new("a `def` gives its value with `return`", value.span).with_fix(at, "return "))
}

/// The value a block's last line gives, through a `with` block that ends it.
fn tail(body: &[Stmt]) -> Option<&Expr> {
    match &body.last()?.kind {
        StmtKind::Expr(value) => Some(value),
        StmtKind::With { body, .. } => tail(body),
        _ => None,
    }
}

/// `lend x else:` as `lend x except:`.
fn lend_except(stmt: &Stmt, sources: &Sources) -> Option<Diagnostic> {
    let StmtKind::Lend { value, otherwise } = &stmt.kind else { return None };
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

/// Each `else` on a call that can fail, written as `except`.
fn failure_fixes(module: &Module, sources: &Sources) -> Vec<Diagnostic> {
    let Ok(checked) = crate::check_typed(module) else { return Vec::new() };
    let types: HashMap<(u32, u32), &Type> = checked
        .types
        .iter()
        .flat_map(|(_, body)| body.exprs().map(|(span, ty)| ((span.start, span.end), ty)))
        .collect();
    let whole = whole_values(module);
    let mut found = Vec::new();
    fennec_syntax::read::exprs(module, &mut |expr| {
        let ExprKind::Else { value, default, except: false } = &expr.kind else { return };
        if expr.span.in_prelude() || !whole.contains(&(expr.span.start, expr.span.end)) {
            return;
        }
        let Some(Type::Result(_, error)) = types.get(&(value.span.start, value.span.end)) else { return };
        if let Some(fix) = failure_fix((value, default), error, &checked.signatures, sources) {
            found.push(fix);
        }
    });
    found
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
            StmtKind::Expr(value) | StmtKind::Return(Some(value)) | StmtKind::Assign { value, .. } => {
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
    error: &Type,
    signatures: &crate::Signatures,
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
    let mut diagnostic = Diagnostic::new("a failure is handled with `except` now", between);
    match default.else_block() {
        // A `match why:` that's all of the block is its arms, which needn't read `why`.
        Some((Some(name), body)) if arms_under(name, body, &text).is_some() => {
            let (at, arms) = arms_under(name, body, &text)?;
            diagnostic = diagnostic.with_fix(between, format!(" except {name}")).with_fix(at, arms);
        }
        Some((Some(name), body)) if body.iter().any(|stmt| crate::except::stmt_uses(stmt, name)) => {
            diagnostic = diagnostic.with_fix(between, format!(" except {name}"));
        }
        _ => {
            let cases = failures(error, signatures)?;
            let after = if block { "" } else { ": " };
            diagnostic = diagnostic.with_fix(between, format!(" except {cases}{after}"));
        }
    }
    Some(diagnostic)
}

/// A `match name:` that's all of a block, as its arms written right under `except name:`: the
/// `match` line goes, and its arms come out a step.
fn arms_under(name: &str, body: &[Stmt], text: &Text<'_>) -> Option<(Span, String)> {
    let [Stmt { kind: StmtKind::Expr(matched), .. }] = body else { return None };
    let ExprKind::Match { subject, arms } = &matched.kind else { return None };
    if !matches!(&subject.kind, ExprKind::Name(found) if found == name) {
        return None;
    }
    let first = arms.first()?.span.start;
    // The `match`'s span runs on to where the next line starts, which stays as it is.
    let written = text.get(Span { start: first, end: matched.span.end })?.trim_end();
    let end = first + u32::try_from(written.len()).ok()?;
    let mut lines = written.split('\n');
    let mut out = lines.next()?.to_owned();
    for line in lines {
        out.push('\n');
        out.push_str(line.strip_prefix("    ").unwrap_or(line));
    }
    Some((Span { start: matched.span.start, end }, out))
}

/// Each way a call can fail with `error`, as the patterns of one case on the line: each error's
/// cases, named on their own where no other error has one by that name, or the error itself.
fn failures(error: &Type, signatures: &crate::Signatures) -> Option<String> {
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
                    let named = if owners.get(case.as_str()).copied().unwrap_or(0) > 1 {
                        format!("{shown}.{case}")
                    } else {
                        format!(".{case}")
                    };
                    patterns.push(if fields.is_empty() { named } else { format!("{named}()") });
                }
            }
            None if signatures.error_type(member).is_some() => patterns.push(format!("{shown}()")),
            None => return None,
        }
    }
    Some(patterns.join(" | "))
}

/// The text of the file a span is in, read by spans.
struct Text<'a> {
    text: &'a str,
    base: u32,
}

impl<'a> Text<'a> {
    fn of(sources: &'a Sources, span: Span) -> Option<Self> {
        let (_, text, base) = sources.file_at(span)?;
        Some(Text { text, base })
    }

    fn get(&self, span: Span) -> Option<&'a str> {
        let from = usize::try_from(span.start.checked_sub(self.base)?).ok()?;
        let to = usize::try_from(span.end.checked_sub(self.base)?).ok()?;
        self.text.get(from..to)
    }
}

#[cfg(test)]
mod tests {
    /// What `migration_fixes` writes for `source`, and that what it writes checks.
    fn migrated(source: &str) -> String {
        let mut sources = fennec_syntax::Sources::new();
        let base = sources.add("forms.fen", source);
        let module = fennec_syntax::parse(source).unwrap();
        let fixes = super::migration_fixes(&module, &sources);
        let written = fennec_syntax::apply_fixes(source, base, &fixes).unwrap();
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

    /// A def's last value gets `return`, and a def that gives nothing is left as it is.
    #[test]
    fn a_defs_last_value_is_returned() {
        let source = "def two() -> int:\n    1 + 1\n\n\
            def shown(stdout: Stdout):\n    stdout.print(two())\n\n\
            def main(stdout: Stdout):\n    shown(stdout)\n";
        assert_eq!(migrated(source), source.replace("    1 + 1", "    return 1 + 1"));
    }
}
