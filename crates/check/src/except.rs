//! What's only `except`'s, beside what it shares with an `else` block. It follows the call it
//! handles, there's a failure for it to handle, and the failure it names goes somewhere on every
//! way out of its block, so nothing is dropped without a word.

use fennec_syntax::{BinOp, Diagnostic, Expr, ExprKind, MatchArm, Pattern, Span, Stmt, StmtKind};

use crate::function::FnCx;
use crate::types::Type;

/// The name the one-case form, `except InvalidValue(): 8080`, gives the failure, which nobody
/// can write.
const ONE_CASE: &str = "except'failure";

/// What a block that drops its failure is told to do.
fn use_it(name: &str) -> String {
    format!("use it, as `log.warn(f\"{{{name}}}\")`, or pass it on with `fail {name}`")
}

impl FnCx<'_> {
    /// Checks `value except ...` for what's only `except`'s, before its block is checked as an
    /// `else` block is.
    pub(crate) fn except_rules(&mut self, value: &Expr, handler: &Expr) {
        match &value.kind {
            ExprKind::Call { .. } => {}
            ExprKind::Else { .. } => self.error(
                Diagnostic::new("`except` and `else` don't go on one line", value.span).with_help(
                    "handle the failure on a line of its own, as `found = f() except why:`, then the `None`, as `x = found else ...`",
                ),
            ),
            _ => self.error(
                Diagnostic::new("`except` goes right after the call it handles", value.span)
                    .with_help("give the call a line of its own, as `got = f() except why:`, and use `got` here"),
            ),
        }
        let Some((name, body)) = handler.failure_named() else { return };
        if let Some((subject, arms)) = arms_under(name, body) {
            let at = (subject.span.start, subject.span.end);
            self.except_subjects.insert(at);
            for arm in arms {
                if self.except_arm(name, arm) {
                    self.except_refused.insert(at);
                }
            }
            return;
        }
        if let Some(span) = nested_match(name, body) {
            let help = format!("write the `case` arms right under `except {name}:`, with no `match`");
            self.error(
                Diagnostic::new(format!("`match {name}:` goes away under `except`"), span).with_help(help),
            );
        }
        if let Some(span) = unused_way_out(body, name, handler.span) {
            let message = format!("`{name}` isn't used on this way out, so it drops the failure");
            self.error(Diagnostic::new(message, span).with_help(use_it(name)));
        }
    }

    /// The failure `except` handles, when it can't fail: there's nothing to handle.
    pub(crate) fn nothing_to_except(&mut self, value: &Type, span: Span) {
        let help = match self.checker.settled(value) {
            Type::Optional(_) | Type::None => {
                "a value that may be `None` takes `else`, as `x = f() else default`".to_owned()
            }
            _ => "take `except` and its block off".to_owned(),
        };
        self.error(
            Diagnostic::new("this can't fail, so `except` has nothing to handle", span).with_help(help),
        );
    }

    /// One `case` arm under `except why:`, or the one case of `except Case(): value`. Whether it
    /// takes a whole type of error, which is refused.
    fn except_arm(&mut self, name: &str, arm: &MatchArm) -> bool {
        let one_case = name == ONE_CASE;
        let mut whole_type = false;
        for pattern in alternatives(&arm.pattern) {
            match pattern {
                Pattern::Bind(other) if !one_case => {
                    let message =
                        format!("`case {other}:` names the failure again, which `{name}` already names");
                    self.error(
                        Diagnostic::new(message, arm.span)
                            .with_help(format!("write `case _:` and use `{name}` in it")),
                    );
                }
                Pattern::Wildcard if one_case => {
                    self.error(Diagnostic::new("`except _` drops every failure", arm.span).with_help(
                        "name the failure, as `except why:`, and use it, or name the case it handles",
                    ));
                }
                Pattern::Wildcard => {
                    if let Some(span) = unused_way_out(&arm.body, name, arm.span) {
                        let message = format!("`case _:` takes every other failure, so it uses `{name}`");
                        self.error(Diagnostic::new(message, span).with_help(use_it(name)));
                    }
                }
                Pattern::Variant { name: owner, dotted: false, .. } if self.has_cases(owner) => {
                    whole_type = true;
                    let message = format!("`{owner}()` takes every case of `{owner}`, which drops them all");
                    self.error(Diagnostic::new(message, arm.span).with_help(format!(
                        "name the cases it handles, as `case .{}():`, or write `except why:` and use `why`",
                        self.first_case(owner).unwrap_or_else(|| "Other".to_owned())
                    )));
                }
                _ => {}
            }
        }
        whole_type
    }

    fn has_cases(&self, owner: &str) -> bool {
        !owner.contains('.')
            && self.checker.program.type_info(owner).is_some_and(|info| !info.def.cases.is_empty())
    }

    fn first_case(&self, owner: &str) -> Option<String> {
        self.checker.program.type_info(owner)?.def.cases.first().map(|case| case.name.clone())
    }

    /// Whether this `match` is the one an `except`'s arms are written in as, by its subject's span:
    /// then a case it leaves out is the `except`'s.
    pub(crate) fn is_except_subject(&self, span: Span) -> bool {
        self.except_subjects.contains(&(span.start, span.end))
    }

    /// Whether this is such a `match` whose arms were refused already, so what it leaves out
    /// says nothing more.
    pub(crate) fn is_refused_except(&self, span: Span) -> bool {
        self.except_refused.contains(&(span.start, span.end))
    }
}

/// The `match` an `except`'s arms are written in as: one statement that's a `match` over the
/// failure's name, which is written before the arms, after `except`. A `match` written in the
/// block has its subject after its own `match`.
fn arms_under<'h>(name: &str, body: &'h [Stmt]) -> Option<(&'h Expr, &'h [MatchArm])> {
    let [Stmt { kind: StmtKind::Expr(matched), .. }] = body else { return None };
    let ExprKind::Match { subject, arms } = &matched.kind else { return None };
    let named = matches!(&subject.kind, ExprKind::Name(found) if found == name);
    (named && subject.span.start < matched.span.start).then_some((subject.as_ref(), arms.as_slice()))
}

/// A `match` over the failure written in an `except` block, as its own statement.
fn nested_match(name: &str, body: &[Stmt]) -> Option<Span> {
    body.iter().find_map(|stmt| match &stmt.kind {
        StmtKind::Expr(Expr { kind: ExprKind::Match { subject, .. }, span })
            if matches!(&subject.kind, ExprKind::Name(found) if found == name) =>
        {
            Some(*span)
        }
        _ => None,
    })
}

fn alternatives(pattern: &Pattern) -> Vec<&Pattern> {
    match pattern {
        Pattern::Or(each) => each.iter().collect(),
        other => vec![other],
    }
}

/// The first way out of `body` on which `name` hasn't been used yet, or where it ends without
/// having used it: `at` when it's the block's end. A `fail` is a way out that's fine as it is,
/// since it carries the failure along as a note.
fn unused_way_out(body: &[Stmt], name: &str, at: Span) -> Option<Span> {
    match ways_out(body, name, false) {
        Err(span) => Some(span),
        Ok(true) => None,
        Ok(false) => Some(body.last().map_or(at, |last| last.span)),
    }
}

/// Walks `body` with `used` saying whether `name` was used before it: the first way out that
/// leaves without using it, or whether it's used by the end.
fn ways_out(body: &[Stmt], name: &str, mut used: bool) -> Result<bool, Span> {
    for stmt in body {
        match &stmt.kind {
            StmtKind::Fail(_) => return Ok(true),
            StmtKind::Return(_) | StmtKind::Break | StmtKind::Continue => {
                return if used || stmt_uses(stmt, name) { Ok(true) } else { Err(stmt.span) };
            }
            StmtKind::If { branches, otherwise } => {
                let mut every = otherwise.is_some();
                for (at, (cond, body)) in branches.iter().enumerate() {
                    let tested = used || uses(cond, name);
                    if at == 0 {
                        used = tested;
                    }
                    every &= ways_out(body, name, tested)?;
                }
                if let Some(body) = otherwise {
                    every &= ways_out(body, name, used)?;
                }
                used |= every;
            }
            StmtKind::While { cond, body } | StmtKind::For { iter: cond, body, .. } => {
                used |= uses(cond, name);
                ways_out(body, name, used)?;
            }
            _ => used |= stmt_uses(stmt, name),
        }
    }
    Ok(used)
}

/// Whether a statement reads `name` into something that goes somewhere. Throwing it away with
/// `_ =` doesn't count.
fn stmt_uses(stmt: &Stmt, name: &str) -> bool {
    if let StmtKind::Assign { target: Expr { kind: ExprKind::Name(target), .. }, .. } = &stmt.kind
        && target.starts_with('_')
    {
        return false;
    }
    let body = std::slice::from_ref(stmt);
    reads_in(&mut |look| fennec_syntax::read::in_stmts(body, look), name)
}

/// Whether `value` reads `name` into something that goes somewhere.
fn uses(value: &Expr, name: &str) -> bool {
    reads_in(&mut |look| fennec_syntax::read::in_expr(value, look), name)
}

/// Walks some code, calling what it's given on each expression in it.
type Walk<'w, 'a> = &'w mut dyn FnMut(&mut dyn FnMut(&'a Expr));

/// Whether what `walk` walks reads `name` anywhere but as what a `match` or an `is` looks at,
/// which says nothing of what failed.
fn reads_in(walk: Walk<'_, '_>, name: &str) -> bool {
    let is_name = |value: &Expr| matches!(&value.kind, ExprKind::Name(found) if found == name);
    let mut looked_at = Vec::new();
    walk(&mut |expr| match &expr.kind {
        ExprKind::Match { subject, .. } if is_name(subject) => looked_at.push(subject.span),
        ExprKind::Binary { op: BinOp::Is | BinOp::IsNot, lhs, .. } if is_name(lhs) => {
            looked_at.push(lhs.span);
        }
        _ => {}
    });
    let mut found = false;
    walk(&mut |expr| found |= is_name(expr) && !looked_at.contains(&expr.span));
    found
}
