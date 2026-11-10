//! What `fennec fix` writes in place of a `match` on a call's `Ok` and `Err`, now that a failure
//! is handled with `else as why:`. Only the shapes that mean the same thing written either way
//! are rewritten; the rest are left to be written by hand.
//!
//! ```fennec
//! match load(path):
//!     case Ok(config):
//!         use(config)
//!     case Err(why):
//!         log(why)
//!         return
//! ```
//!
//! becomes
//!
//! ```fennec
//! config = load(path) else as why:
//!     log(why)
//!     return
//! use(config)
//! ```
//!
//! The failure's cases have to leave, or the `match` is what a function gives back, when each
//! one's value is given back with `return`, or it's the last thing a loop does, when each one
//! goes on with `continue`. `x = match load(path):` whose `Ok` case gives its value back as it
//! is becomes `x = load(path) else as why:`, with the failure's value as the block's last line.
//! Several failure cases become one `match why:`. The `match` they were in had a case for every
//! failure, so that one does too.

use std::collections::HashSet;
use std::fmt::Write as _;

use fennec_syntax::{
    Diagnostic, Expr, ExprKind, FnDef, MatchArm, Module, Pattern, Sources, Span, Stmt, StmtKind,
};

/// A fix for each `match` on `Ok` and `Err` that can be written with `else as` and mean the same.
#[must_use]
pub fn result_fixes(module: &Module, sources: &Sources) -> Vec<Diagnostic> {
    let mut defs: Vec<&FnDef> = Vec::new();
    let mut found = Vec::new();
    for item in &module.items {
        match item {
            fennec_syntax::Item::Function(def) => functions(def, &mut defs),
            fennec_syntax::Item::Struct(def)
            | fennec_syntax::Item::Enum(def)
            | fennec_syntax::Item::Error(def) => {
                def.methods.iter().for_each(|def| functions(def, &mut defs));
            }
            fennec_syntax::Item::Extend(extension) => {
                extension.methods.iter().for_each(|def| functions(def, &mut defs));
            }
            fennec_syntax::Item::Test(test) => {
                let mut taken: HashSet<String> =
                    test.capabilities.iter().chain(&test.inputs).map(|param| param.name.clone()).collect();
                declared(&test.body, &mut taken);
                let place = Place { function_tail: false, loop_tail: false, in_test: true };
                block(&test.body, place, (sources, &taken), &mut found);
            }
            _ => {}
        }
    }
    for def in defs.into_iter().filter(|def| !def.span.in_prelude()) {
        found.extend(result_return(def, sources));
        let gives = def.ret.as_ref().is_some_and(|written| written.name != "None");
        let mut taken: HashSet<String> = def.params.iter().map(|param| param.name.clone()).collect();
        declared(&def.body, &mut taken);
        let place = Place { function_tail: gives, loop_tail: false, in_test: false };
        block(&def.body, place, (sources, &taken), &mut found);
    }
    found
}

/// `-> Result[T, E]` on a function, written `-> T fails E`, which means the same.
fn result_return(def: &FnDef, sources: &Sources) -> Option<Diagnostic> {
    let ret = def.ret.as_ref().filter(|ret| ret.name == "Result" && !ret.optional && def.fails.is_none())?;
    let [value, error] = ret.args.as_slice() else { return None };
    let text = Text::of(sources, ret.span)?;
    // A function that gives nothing back says only what it fails with, and that's written apart.
    let value = text.get(value.span).filter(|value| *value != "nothing")?;
    let written = format!("{value} fails {}", text.get(error.span)?);
    Some(crate::gone::result_type(ret.span).with_fix(ret.span, written))
}

/// Makes the first of a block's rewrites that names something `mut`, when a later one in the same
/// block names it again: each `match` had its own case for it, and a block has one name for all.
fn mut_when_given_again(found: &mut [Diagnostic], rewrites: &[usize]) {
    let named = |fix: &Diagnostic| -> Option<String> {
        let text = &fix.fixes.first()?.text;
        let (name, _) = text.split_once(" = ")?;
        (name != "_" && name.chars().all(|c| c.is_alphanumeric() || c == '_')).then(|| name.to_owned())
    };
    let given: Vec<(usize, String)> =
        rewrites.iter().filter_map(|&at| Some((at, named(found.get(at)?)?))).collect();
    for (at, name) in &given {
        let first = given.iter().find(|(_, other)| other == name).map(|(first, _)| *first);
        let again = given.iter().filter(|(_, other)| other == name).count() > 1;
        if first == Some(*at)
            && again
            && let Some(edit) = found.get_mut(*at).and_then(|fix| fix.fixes.first_mut())
        {
            edit.text.insert_str(0, "mut ");
        }
    }
}

fn functions<'a>(def: &'a FnDef, found: &mut Vec<&'a FnDef>) {
    found.push(def);
    for stmt in &def.body {
        if let StmtKind::Def(nested) = &stmt.kind {
            functions(nested, found);
        }
    }
}

/// Where a block's last statement is: what a function gives back, or what a loop does last.
#[derive(Clone, Copy)]
struct Place {
    function_tail: bool,
    loop_tail: bool,
    /// In a `test`, which may use `?` and `failure_of`.
    in_test: bool,
}

/// Every name some statements declare, by assigning it or looping with it, anywhere in them.
fn declared(stmts: &[Stmt], found: &mut HashSet<String>) {
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Assign { target, .. } => {
                fennec_syntax::read::in_expr(target, &mut |expr| {
                    if let ExprKind::Name(name) = &expr.kind {
                        found.insert(name.clone());
                    }
                });
            }
            StmtKind::For { names, .. } => found.extend(names.iter().cloned()),
            StmtKind::With { names, .. } => found.extend(names.iter().map(|held| held.name.clone())),
            _ => {}
        }
        for inner in crate::loops::blocks(stmt) {
            declared(inner, found);
        }
    }
}

/// The file's text and the names the function already holds, which a rewrite can't take.
type Around<'a> = (&'a Sources, &'a HashSet<String>);

fn block(stmts: &[Stmt], place: Place, around: Around<'_>, found: &mut Vec<Diagnostic>) {
    let mut rewrites = Vec::new();
    for (at, stmt) in stmts.iter().enumerate() {
        let last = at + 1 == stmts.len();
        let here =
            Place { function_tail: place.function_tail && last, loop_tail: place.loop_tail && last, ..place };
        if let Some(fix) = rewritten(stmt, here, around) {
            rewrites.push(found.len());
            found.push(fix);
            continue;
        }
        match &stmt.kind {
            StmtKind::If { branches, otherwise } => {
                for (_, body) in branches {
                    block(body, here, around, found);
                }
                if let Some(body) = otherwise {
                    block(body, here, around, found);
                }
            }
            StmtKind::For { body, .. } | StmtKind::While { body, .. } => {
                block(body, Place { function_tail: false, loop_tail: true, ..place }, around, found);
            }
            StmtKind::With { body, .. } => block(body, here, around, found),
            _ => {}
        }
    }
    mut_when_given_again(found, &rewrites);
}

/// A `match`'s cases, as the `Ok` one and the failure's.
struct Cases<'a> {
    ok: (&'a Pattern, &'a [Stmt]),
    failed: Vec<(&'a MatchArm, Failure<'a>)>,
}

/// What a failure case matches: `case Err(pattern)`, a failure's own case written bare, like
/// `case NotFound(name)`, or `case _` after the `Ok` one.
#[derive(Clone, Copy)]
enum Failure<'a> {
    Wrapped(&'a Pattern),
    Bare,
    Rest,
}

/// The `Ok` case and the failure's cases, when every case is one of those and none has a guard.
fn cases(arms: &[MatchArm]) -> Option<Cases<'_>> {
    let mut ok = None;
    let mut failed = Vec::new();
    for arm in arms {
        if arm.guard.is_some() {
            return None;
        }
        match &arm.pattern {
            Pattern::Variant { name, args, .. } if name == "Ok" => match args.as_slice() {
                [inner @ (Pattern::Bind(_) | Pattern::Wildcard)] if ok.is_none() => {
                    ok = Some((inner, arm.body.as_slice()));
                }
                _ => return None,
            },
            Pattern::Variant { name, args, .. } if name == "Err" => match args.as_slice() {
                [inner] => failed.push((arm, Failure::Wrapped(inner))),
                _ => return None,
            },
            Pattern::Variant { .. } => failed.push((arm, Failure::Bare)),
            Pattern::Wildcard if ok.is_some() => failed.push((arm, Failure::Rest)),
            _ => return None,
        }
    }
    Some(Cases { ok: ok?, failed }).filter(|cases| !cases.failed.is_empty())
}

fn leaves(body: &[Stmt]) -> bool {
    matches!(
        body.last().map(|last| &last.kind),
        Some(StmtKind::Return(_) | StmtKind::Fail(_) | StmtKind::Break | StmtKind::Continue)
    )
}

/// The statement written with `else as`, as the fix that replaces it.
fn rewritten(stmt: &Stmt, place: Place, (sources, taken): Around<'_>) -> Option<Diagnostic> {
    let (named, matched) = match &stmt.kind {
        StmtKind::Expr(matched) => (None, matched),
        StmtKind::Assign { target, op: None, value, declare_mut, ty: None } => match &target.kind {
            ExprKind::Name(name) => (Some((name.as_str(), *declare_mut)), value),
            _ => return None,
        },
        _ => return None,
    };
    let ExprKind::Match { subject, arms } = &matched.kind else { return None };
    let cases = cases(arms)?;
    let text = Text::of(sources, stmt.span)?;
    let indent = text.column(stmt.span.start)?;
    let called = text.get(subject.span)?;
    let (ok_pattern, ok_body) = cases.ok;
    let ok_name = match ok_pattern {
        Pattern::Bind(name) => name.as_str(),
        _ => "_",
    };
    // A name the `Ok` case gave its own scope would be one the function holds already.
    if named.is_none() && taken.contains(ok_name) {
        return None;
    }
    if place.in_test && named.is_none() {
        return in_test(stmt, (subject, &cases), &text, taken);
    }
    // How each failure case ends: as it is when it leaves, or with the way out the place gives.
    let ending = |body: &[Stmt]| -> Option<Ending> {
        if leaves(body) || named.is_some() {
            Some(Ending::AsItIs)
        } else if place.function_tail && matches!(body.last()?.kind, StmtKind::Expr(_)) {
            Some(Ending::Return)
        } else if place.loop_tail {
            Some(Ending::Continue)
        } else {
            None
        }
    };
    let mut out = String::new();
    // `x = match ...:` keeps its name only when the `Ok` case gives back what it holds, as it is.
    match named {
        Some((name, declare_mut)) => {
            let [Stmt { kind: StmtKind::Expr(Expr { kind: ExprKind::Name(given), .. }), .. }] = ok_body
            else {
                return None;
            };
            if given != ok_name {
                return None;
            }
            let word = if declare_mut { "mut " } else { "" };
            let _ = write!(out, "{word}{name} = {called} else");
        }
        None => {
            let _ = write!(out, "{ok_name} = {called} else");
        }
    }
    failure_cases(&mut out, &cases.failed, (&text, indent), &ending)?;
    let rest = ok_body.iter().all(|stmt| matches!(stmt.kind, StmtKind::Pass));
    if named.is_none() && !rest {
        out.push_str(&text.body(ok_body, indent, Ending::AsItIs)?);
    }
    let replaced = out.strip_suffix('\n').unwrap_or(&out).to_owned();
    // The statement as far as its last case's last line, not the blank lines after it.
    let written = Span { start: stmt.span.start, end: stmt_end(stmt)? };
    Some(
        Diagnostic::new(MESSAGE, written)
            .with_help("`fennec fix` writes this `match` with `else as`")
            .with_fix(written, replaced.trim_start().to_owned()),
    )
}

/// The failure cases of a `match` written as an `else` block on `out`: one case as the block
/// itself, and several as a `match why:` in it, each ending as `ending` says.
fn failure_cases(
    out: &mut String,
    failed: &[(&MatchArm, Failure<'_>)],
    (text, indent): (&Text<'_>, usize),
    ending: &dyn Fn(&[Stmt]) -> Option<Ending>,
) -> Option<()> {
    let pad = " ".repeat(indent);
    match failed {
        [(arm, inner @ (Failure::Rest | Failure::Wrapped(Pattern::Bind(_) | Pattern::Wildcard)))] => {
            let name = match inner {
                Failure::Wrapped(Pattern::Bind(name)) => Some(name.as_str()),
                _ => None,
            };
            match name {
                Some(name) => {
                    let _ = writeln!(out, " as {name}:");
                }
                None => out.push_str(":\n"),
            }
            out.push_str(&text.body(&arm.body, indent + 4, ending(&arm.body)?)?);
        }
        failed => {
            let why = "why";
            let _ = writeln!(out, " as {why}:\n{pad}    match {why}:");
            for (arm, inner) in failed {
                let shown = match inner {
                    Failure::Rest => "_".to_owned(),
                    Failure::Wrapped(Pattern::Bind(name)) => {
                        let _ = writeln!(out, "{pad}        case _:");
                        let mut body = format!("{pad}            {name} = {why}\n");
                        body.push_str(&text.body(&arm.body, indent + 12, ending(&arm.body)?)?);
                        out.push_str(&body);
                        continue;
                    }
                    Failure::Wrapped(_) => text.err_pattern(arm.span)?,
                    Failure::Bare => text.bare_pattern(arm.span)?,
                };
                let _ = writeln!(out, "{pad}        case {shown}:");
                out.push_str(&text.body(&arm.body, indent + 12, ending(&arm.body)?)?);
            }
        }
    }
    Some(())
}

/// The two ways a test checks a call with `match`: that it fails, with `case Ok(_): assert False`
/// beside what it checks of the failure, written with `failure_of`, or that it doesn't, with
/// `case Err(_): assert False`, written with `?`, which fails the test as the `assert` did.
fn in_test(
    stmt: &Stmt,
    (subject, cases): (&Expr, &Cases<'_>),
    text: &Text<'_>,
    taken: &HashSet<String>,
) -> Option<Diagnostic> {
    let asserts_false = |body: &[Stmt]| {
        matches!(body, [Stmt { kind: StmtKind::Assert(Expr { kind: ExprKind::Bool(false), .. }), .. }])
    };
    let called = text.get(subject.span)?;
    let indent = text.column(stmt.span.start)?;
    let pad = " ".repeat(indent);
    let (ok_pattern, ok_body) = cases.ok;
    let mut out = String::new();
    if asserts_false(ok_body) {
        // The call goes in an arrow function, which can't pass a failure up or change a name.
        let mut plain = true;
        fennec_syntax::read::in_expr(subject, &mut |expr| match &expr.kind {
            ExprKind::Try { .. } => plain = false,
            ExprKind::Call { args, .. } => plain &= args.iter().all(|arg| !arg.is_mut),
            _ => {}
        });
        let [(arm, failure)] = cases.failed.as_slice() else { return None };
        if !plain {
            return None;
        }
        let quiet = arm.body.iter().all(|stmt| matches!(stmt.kind, StmtKind::Pass));
        match failure {
            Failure::Wrapped(Pattern::Bind(name)) if !taken.contains(name.as_str()) => {
                let _ = writeln!(out, "{name} = failure_of(() => {called})\n{pad}assert {name} is not None");
            }
            Failure::Rest | Failure::Wrapped(Pattern::Wildcard) => {
                let _ = writeln!(out, "assert failure_of(() => {called}) is not None");
            }
            _ => return None,
        }
        if !quiet {
            out.push_str(&text.body(&arm.body, indent, Ending::AsItIs)?);
        }
    } else {
        let [(arm, Failure::Rest | Failure::Wrapped(Pattern::Wildcard))] = cases.failed.as_slice() else {
            return None;
        };
        if !asserts_false(&arm.body) {
            return None;
        }
        let name = match ok_pattern {
            Pattern::Bind(name) if !taken.contains(name.as_str()) => name.as_str(),
            Pattern::Bind(_) => return None,
            _ => "_",
        };
        let _ = writeln!(out, "{name} = {called}?");
        if !ok_body.iter().all(|stmt| matches!(stmt.kind, StmtKind::Pass)) {
            out.push_str(&text.body(ok_body, indent, Ending::AsItIs)?);
        }
    }
    let end = stmt_end(stmt)?;
    let written = Span { start: stmt.span.start, end };
    let replaced = out.strip_suffix('\n').unwrap_or(&out).to_owned();
    Some(
        Diagnostic::new(MESSAGE, written)
            .with_help("`fennec fix` writes this `match` with `failure_of` or `?`")
            .with_fix(written, replaced),
    )
}

const MESSAGE: &str = "`Ok` and `Err` are gone: a failure is handled with `else as why:`";

/// Where a `match` statement's last case's last line ends, before the blank lines after it.
fn stmt_end(stmt: &Stmt) -> Option<u32> {
    let (StmtKind::Expr(matched) | StmtKind::Assign { value: matched, .. }) = &stmt.kind else { return None };
    let ExprKind::Match { arms, .. } = &matched.kind else { return None };
    arms.iter().filter_map(|arm| arm.body.last()).map(|last| last.span.end).max()
}

/// How a failure case's block ends once it's written under `else as`.
#[derive(Clone, Copy, PartialEq)]
enum Ending {
    AsItIs,
    /// Its last line's value is given back.
    Return,
    /// The loop goes on.
    Continue,
}

/// The text of the file a statement is in, read by spans.
struct Text<'a> {
    text: &'a str,
    base: u32,
}

impl<'a> Text<'a> {
    fn of(sources: &'a Sources, span: Span) -> Option<Self> {
        let (_, text, base) = sources.file_at(span)?;
        Some(Text { text, base })
    }

    fn offset(&self, at: u32) -> Option<usize> {
        usize::try_from(at.checked_sub(self.base)?).ok()
    }

    fn get(&self, span: Span) -> Option<&'a str> {
        self.text.get(self.offset(span.start)?..self.offset(span.end)?)
    }

    /// How far into its line `at` is, when only spaces come before it.
    fn column(&self, at: u32) -> Option<usize> {
        let at = self.offset(at)?;
        let start = self.text.get(..at)?.rfind('\n').map_or(0, |line| line + 1);
        let before = self.text.get(start..at)?;
        before.chars().all(|c| c == ' ').then_some(before.len())
    }

    /// The pattern of `case Pattern:`, as written, up to the colon its brackets don't hold.
    fn bare_pattern(&self, arm: Span) -> Option<String> {
        let inside = self.get(arm)?.strip_prefix("case ")?;
        let mut depth = 0;
        for (at, c) in inside.char_indices() {
            match c {
                '(' | '[' => depth += 1,
                ')' | ']' => depth -= 1,
                ':' if depth == 0 => return Some(inside.get(..at)?.trim_end().to_owned()),
                _ => {}
            }
        }
        None
    }

    /// The pattern inside `case Err(...)`, as written.
    fn err_pattern(&self, arm: Span) -> Option<String> {
        let written = self.get(arm)?;
        let inside = written.strip_prefix("case Err(")?;
        let mut depth = 1;
        for (at, c) in inside.char_indices() {
            match c {
                '(' | '[' => depth += 1,
                ')' | ']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(inside.get(..at)?.to_owned());
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Some statements' lines, moved to start at `indent`, ending as `ending` says.
    fn body(&self, stmts: &[Stmt], indent: usize, ending: Ending) -> Option<String> {
        let (first, last) = (stmts.first()?, stmts.last()?);
        let pad = " ".repeat(indent);
        // A body written on its case's line, `case Err(_): return 0`, is that one statement.
        let lines: Vec<String> = match self.column(first.span.start) {
            Some(from) => {
                let start = self.offset(first.span.start)? - from;
                let end = self.offset(last.span.end)?;
                let written = self.text.get(start..end)?;
                written
                    .lines()
                    .map(|line| {
                        let kept = line.len() - line.trim_start_matches(' ').len();
                        let cut = kept.min(from);
                        if line.trim().is_empty() {
                            String::new()
                        } else {
                            format!("{pad}{}", line.get(cut..).unwrap_or(line))
                        }
                    })
                    .collect()
            }
            None => vec![format!("{pad}{}", self.get(Span { start: first.span.start, end: last.span.end })?)],
        };
        let mut out = lines.join("\n");
        match ending {
            Ending::AsItIs => {}
            Ending::Return => {
                // `return` goes in front of the last statement, where it starts.
                let StmtKind::Expr(_) = last.kind else { return None };
                let into = out.len() - self.get(last.span)?.len();
                if !out.ends_with(self.get(last.span)?) {
                    return None;
                }
                out.insert_str(into, "return ");
            }
            Ending::Continue => {
                let _ = write!(out, "\n{pad}continue");
            }
        }
        out.push('\n');
        Some(out)
    }
}
