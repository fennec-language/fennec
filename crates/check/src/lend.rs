//! `with` sources: a function that gives back `with T` lends a `T` to its caller's `with`
//! block, with exactly one `lend`. Setup goes above the `lend` and cleanup below it, and
//! `lend x else:` runs when the caller's block fails. The end of the block is the finish: code
//! after the `lend` may fail, and its failure is the `with` statement's. Once checked,
//! `lend_lowering` writes each `with` over a source in as the source's own body.

use fennec_syntax::{Diagnostic, Expr, ExprKind, FnDef, Span, Stmt, StmtKind};

use crate::function::FnCx;
use crate::stmt::Flow;
use crate::types::{Type, a};

/// The name `with T` has as a type, the return type of a source.
pub(crate) const WITH: &str = "with";

/// What a source lends, if `ty` is one's `with T`.
pub(crate) fn lent(ty: &Type) -> Option<&Type> {
    match ty {
        Type::Generic(name, args) if name == WITH => args.first(),
        _ => None,
    }
}

impl FnCx<'_> {
    /// `lend value`, or `lend value else:` with what runs when the caller's block fails.
    pub(crate) fn lend(&mut self, value: &Expr, otherwise: &[Stmt], span: Span) -> Flow {
        self.not_under_lock(
            "a `lend` runs its caller's block, which can wait,",
            span,
            "lend after the lock's block ends, and take the lock again after the `lend` if it's needed",
        );
        let Some(lends) = self.lends.clone() else {
            self.expr(value);
            self.error(
                Diagnostic::new("`lend` is only in a function that gives back `with T`", span).with_help(
                    format!("write `-> with T` as `{}`'s return type, where `T` is what it lends", self.name),
                ),
            );
            return Flow::unit();
        };
        let ty = self.expr_as(value, &lends);
        if !self.checker.fits(&lends, &ty) {
            let message = format!("`{}` lends {}, but this is {}", self.name, a(&lends), a(&ty));
            self.error(Diagnostic::new(message, value.span));
        }
        self.lent += 1;
        if self.loops > 0 {
            self.error(
                Diagnostic::new("this `lend` is in a loop, so it could lend more than once", span).with_help(
                    "lend once, outside the loop: a `with` source lends one value for its caller's block",
                ),
            );
        }
        if self.lent > 1 {
            self.error(
                Diagnostic::new(format!("`{}` lends twice, but a `with` source lends once", self.name), span)
                    .with_help("lend in one place, with setup above it and cleanup below"),
            );
        }
        if !otherwise.is_empty() {
            self.push(false);
            self.stmts(otherwise, false);
            self.pop();
        }
        Flow::unit()
    }

    /// Keeps a call to a source, by its span, for lowering to write in where it's used.
    pub(crate) fn note_source(&mut self, key: &str, returns: &Type, method: bool, span: Span) {
        let gives = match returns {
            Type::Result(value, _) => value.as_ref(),
            other => other,
        };
        if lent(gives).is_none() {
            return;
        }
        if let Some(entry) = self.checker.program.functions.get(key) {
            // A method's type parameters start with its type's: `T` in `Pool[T].lent`.
            let owner = entry.owner.as_deref().and_then(|owner| self.checker.program.type_info(owner));
            let generics = owner
                .iter()
                .flat_map(|info| &info.def.generics)
                .chain(&entry.def.generics)
                .map(|generic| generic.name.clone())
                .collect();
            let source = crate::lend_lowering::Source {
                def: entry.def.shared(),
                method,
                gives: gives.clone(),
                generics,
            };
            self.checker.sources.insert((span.start, span.end), source);
        }
    }

    /// The type of what a `with` takes, where a source may be called.
    pub(crate) fn with_value(&mut self, value: &Expr) -> Type {
        let before = self.with_values.len();
        self.with_values.extend(Self::with_values_of(value));
        let ty = self.expr(value);
        self.with_values.truncate(before);
        ty
    }

    /// What a `with` block's name holds: what a source lends, or the value itself.
    pub(crate) fn lent_to(&mut self, ty: Type, body: &[Stmt]) -> Type {
        match lent(&ty).cloned() {
            Some(lent) => {
                self.unplaced_try_in(body);
                lent
            }
            None => ty,
        }
    }

    /// Reports a `?` in a source's caller's block where its failure couldn't run the `else:`.
    pub(crate) fn unplaced_try_in(&mut self, body: &[Stmt]) {
        if let Some(span) = unplaced_try(body) {
            self.error(
                Diagnostic::new("this `?` might not run, and in a source's block that isn't built yet", span)
                    .with_help("take it to its own line first, like `got = f()?`, and use `got` here"),
            );
        }
    }

    /// A source called anywhere but as what a `with` takes.
    pub(crate) fn source_outside_with(&mut self, call: &Expr, ty: &Type) {
        let lent = lent(ty).map_or_else(|| "something".to_owned(), a);
        self.error(
            Diagnostic::new(format!("this lends {lent} for a block, so it's used in a `with`"), call.span)
                .with_help("write `with ... as name:`, and use `name` inside the block"),
        );
    }

    /// The values a `with` takes, where a source may be called: each one, and what's in a `?` or
    /// an `else` that leaves, as in `with launch(...) else fail E:`.
    pub(crate) fn with_values_of(value: &Expr) -> Vec<(u32, u32)> {
        let items: Vec<&Expr> = match &value.kind {
            ExprKind::Tuple(items) => items.iter().collect(),
            _ => vec![value],
        };
        let mut spans = vec![(value.span.start, value.span.end)];
        for item in items {
            spans.push((item.span.start, item.span.end));
            match &item.kind {
                ExprKind::Try { value: inner, .. } => spans.push((inner.span.start, inner.span.end)),
                ExprKind::Else { value: inner, default } if default.leaves() => {
                    spans.push((inner.span.start, inner.span.end));
                }
                _ => {}
            }
        }
        spans
    }

    /// What's checked of a source once its body is: it lends once.
    pub(crate) fn source_finished(&mut self, def: &FnDef) {
        if self.lent == 0 {
            let lends = self.lends.clone().map_or_else(|| "something".to_owned(), |ty| a(&ty));
            self.error(
                Diagnostic::new(
                    format!("`{}` gives back `with`, so it lends {lends} with `lend`", self.name),
                    def.span,
                )
                .with_help("write `lend value` where the caller's block runs"),
            );
        }
    }
}

/// A `?` in a source's caller's block that might not run where it's written: past `and` or
/// `or`, in a branch of `a if c else b`, after `else`, in a comprehension, in a loop's
/// condition or in an `elif`'s. Its failure has to run the `lend`'s `else:`, which isn't built
/// for those places yet.
pub(crate) fn unplaced_try(body: &[Stmt]) -> Option<Span> {
    body.iter().find_map(|stmt| match &stmt.kind {
        StmtKind::While { cond, body } => conditional_try(cond, true).or_else(|| unplaced_try(body)),
        StmtKind::If { branches, otherwise } => branches
            .iter()
            .enumerate()
            .find_map(|(at, (cond, body))| conditional_try(cond, at > 0).or_else(|| unplaced_try(body)))
            .or_else(|| otherwise.as_deref().and_then(unplaced_try)),
        StmtKind::For { iter, body, .. } => conditional_try(iter, false).or_else(|| unplaced_try(body)),
        StmtKind::With { value, body, .. } => conditional_try(value, false).or_else(|| unplaced_try(body)),
        StmtKind::Assign { target, value, .. } => {
            conditional_try(target, false).or_else(|| conditional_try(value, false))
        }
        StmtKind::Expr(value)
        | StmtKind::Fail(value)
        | StmtKind::Yield(value)
        | StmtKind::Assert(value)
        | StmtKind::Return(Some(value))
        | StmtKind::Lend { value, .. } => conditional_try(value, false),
        _ => None,
    })
}

fn conditional_try(value: &Expr, conditional: bool) -> Option<Span> {
    let each = |values: &mut dyn Iterator<Item = &Expr>, conditional: bool| {
        for value in values {
            if let Some(span) = conditional_try(value, conditional) {
                return Some(span);
            }
        }
        None
    };
    match &value.kind {
        ExprKind::Try { value: inner, .. } => {
            if conditional {
                Some(value.span)
            } else {
                conditional_try(inner, false)
            }
        }
        ExprKind::Comprehension { elem, value: held, iter, cond, .. } => {
            let parts = [Some(elem.as_ref()), held.as_deref(), Some(iter.as_ref()), cond.as_deref()];
            each(&mut parts.into_iter().flatten(), true)
        }
        ExprKind::Binary { op: fennec_syntax::BinOp::And | fennec_syntax::BinOp::Or, lhs, rhs } => {
            conditional_try(lhs, conditional).or_else(|| conditional_try(rhs, true))
        }
        ExprKind::Binary { lhs, rhs, .. } => each(&mut [lhs.as_ref(), rhs.as_ref()].into_iter(), conditional),
        ExprKind::Cond { then, cond, otherwise } => conditional_try(cond, conditional)
            .or_else(|| each(&mut [then.as_ref(), otherwise.as_ref()].into_iter(), true)),
        ExprKind::Else { value: held, default } => {
            conditional_try(held, conditional).or_else(|| conditional_try(default, true))
        }
        ExprKind::Match { subject, arms } => conditional_try(subject, conditional)
            .or_else(|| arms.iter().find_map(|arm| unplaced_try(&arm.body))),
        ExprKind::Call { callee, args } => conditional_try(callee, conditional)
            .or_else(|| each(&mut args.iter().map(|arg| &arg.value), conditional)),
        ExprKind::Attr { object, .. } | ExprKind::Unary { expr: object, .. } => {
            conditional_try(object, conditional)
        }
        ExprKind::Index { object, index } => {
            each(&mut [object.as_ref(), index.as_ref()].into_iter(), conditional)
        }
        ExprKind::List(items) | ExprKind::Tuple(items) | ExprKind::Set(items) => {
            each(&mut items.iter(), conditional)
        }
        ExprKind::Dict(pairs) => each(&mut pairs.iter().flat_map(|(key, value)| [key, value]), conditional),
        ExprKind::FStr(parts) => each(
            &mut parts.iter().filter_map(|part| match part {
                fennec_syntax::FStrPart::Expr { expr, .. } => Some(expr),
                fennec_syntax::FStrPart::Text(_) => None,
            }),
            conditional,
        ),
        _ => None,
    }
}
