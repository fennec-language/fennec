//! What can fail is read only through `?`, `else`, `.expect` or `assert ... fails`. Anywhere
//! else, held in a name, printed, compared, put in a collection or handed on, it would be a value
//! that might be a failure, so it's a compile error. An arrow function may give one back, which is
//! how `failure_of` takes a call, and so may what a function that fails gives back, which the
//! checker already holds to `?`.

use fennec_syntax::{Diagnostic, Expr, ExprKind, Stmt, StmtKind, TypeExpr};

use crate::Body;
use crate::checker::Checker;
use crate::types::Type;

/// Each place in a function's body, or a test's, that reads what can fail as a plain value.
/// `fails` is what the function says it fails with, and a test may pass any failure up: there,
/// the fix passes the failure up with `?`.
pub(crate) fn check_body(checker: &Checker, key: &str, body: &[Stmt], fails: Passes<'_>) -> Vec<Diagnostic> {
    let Some(typed) = checker.typed.get(key) else { return Vec::new() };
    let mut walk = Walk { typed, fails, lambdas: 0, problems: Vec::new() };
    walk.block(body);
    walk.problems
}

/// Where a failure can be passed up with `?`.
#[derive(Clone, Copy)]
pub(crate) enum Passes<'a> {
    /// Nowhere: the function doesn't fail.
    Nowhere,
    /// Out of a function that says `fails` with these.
    Declared(&'a TypeExpr),
    /// Out of a test, which takes any failure as its own.
    Test,
}

struct Walk<'b> {
    typed: &'b Body,
    fails: Passes<'b>,
    /// How many arrow functions this is inside, where `?` passes a failure up as the arrow's.
    lambdas: usize,
    problems: Vec<Diagnostic>,
}

impl Walk<'_> {
    fn block(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            self.stmt(stmt);
        }
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            // What a statement gives back, a `return` and an `assert` are held to `?` and `else`
            // where they are: a failure as a whole line, or passed up unmarked, says so there.
            StmtKind::Expr(value) | StmtKind::Return(Some(value)) | StmtKind::Assert(value) => {
                self.read(value);
            }
            // `_ = f()` says it drops the failure, where it's checked.
            StmtKind::Assign { target, value, .. } if is_discard(target) => self.read(value),
            StmtKind::Assign { target, value, .. } => {
                self.inside(target);
                self.expr(value);
            }
            // A `with` holds what it opens to its own rules: a source that fails to open, or a
            // task that fails, fails the block.
            StmtKind::With { value, .. } => self.read(value),
            StmtKind::Lend { value, otherwise } => {
                self.expr(value);
                self.block(otherwise);
            }
            StmtKind::OnFailure { cleanup, body, .. } => {
                self.block(cleanup);
                self.block(body);
            }
            _ => {
                for value in crate::loops::exprs(stmt) {
                    self.expr(value);
                }
            }
        }
        for inner in crate::loops::blocks(stmt) {
            if !matches!(stmt.kind, StmtKind::OnFailure { .. }) {
                self.block(inner);
            }
        }
    }

    /// An expression where what can fail may stand as it is: it's read by what's around it, or
    /// held to that elsewhere.
    fn read(&mut self, expr: &Expr) {
        self.inside(expr);
    }

    /// What `?` or `else` reads: a failure anywhere along a chain of steps, as in
    /// `find(1).name else "?"`, stops it there.
    fn chain(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Attr { object, .. } => self.chain(object),
            ExprKind::Index { object, index } => {
                self.chain(object);
                self.expr(index);
            }
            ExprKind::Call { callee, args } if matches!(callee.kind, ExprKind::Attr { .. }) => {
                if let ExprKind::Attr { object, .. } = &callee.kind {
                    self.chain(object);
                }
                for arg in args {
                    self.expr(&arg.value);
                }
            }
            _ => self.read(expr),
        }
    }

    /// An expression whose value is used as it is. What's inside it that can fail is said first,
    /// as what's around it, like `keep(half(3))`, can fail only through it.
    fn expr(&mut self, expr: &Expr) {
        let before = self.problems.len();
        self.inside(expr);
        if self.problems.len() == before && self.can_fail(expr) {
            self.problems.push(self.unhandled(expr));
        }
    }

    /// What's inside an expression, each part as what's around it reads it.
    fn inside(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Try { value, context } => {
                self.chain(value);
                if let Some(context) = context {
                    self.expr(context);
                }
            }
            ExprKind::Else { value, default } => {
                self.chain(value);
                self.expr(default);
            }
            ExprKind::Call { callee, args } if is_expect(callee) => {
                if let ExprKind::Attr { object, .. } = &callee.kind {
                    self.read(object);
                }
                for arg in args {
                    self.expr(&arg.value);
                }
            }
            ExprKind::Lambda { body, .. } => {
                self.lambdas += 1;
                self.read(body);
                self.lambdas -= 1;
            }
            ExprKind::Match { subject, arms } => {
                // `assert value fails Pattern`, what an `else` block is written as, and the
                // `match` lowering writes for a `?` or an `else`, whose cases are its own.
                let lowered = arms.iter().any(|arm| crate::lend_lowering::written_in(arm.span));
                if lowered || expr.asserted_failure().is_some() || expr.else_block().is_some() {
                    self.read(subject);
                } else {
                    self.expr(subject);
                }
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.expr(guard);
                    }
                    self.block(&arm.body);
                }
            }
            _ => {
                for inner in crate::effects::children(expr) {
                    self.expr(inner);
                }
            }
        }
    }

    /// Whether `expr` can fail. A name holding what can fail was given it where that's said.
    fn can_fail(&self, expr: &Expr) -> bool {
        !matches!(expr.kind, ExprKind::Name(_))
            && crate::gone::written_by_hand(expr.span)
            && !expr.span.in_prelude()
            && matches!(self.typed.expr(expr.span), Some(Type::Result(..)))
    }

    fn unhandled(&self, expr: &Expr) -> Diagnostic {
        let said = Diagnostic::new("this can fail, but nothing handles it", expr.span);
        let error = match self.typed.expr(expr.span) {
            Some(Type::Result(_, error)) => error.as_ref(),
            _ => &Type::Unknown,
        };
        // From inside an arrow function, `?` passes the failure up as the arrow's.
        if self.lambdas > 0 || self.passes(error) {
            let end = fennec_syntax::Span { start: expr.span.end, end: expr.span.end };
            return said
                .with_help("pass the failure up with `?`: `fennec fix` makes this change")
                .with_fix(end, "?");
        }
        said.with_help(
            "give a value for a failure with `else`, handle it with `else as why:`, or pass it up with `?` from a function that says `fails`",
        )
    }

    /// Whether `?` can pass a failure of type `error` up from here.
    fn passes(&self, error: &Type) -> bool {
        match self.fails {
            Passes::Nowhere => false,
            Passes::Test => true,
            Passes::Declared(declared) => {
                let mut named = Vec::new();
                names(declared, &mut named);
                match error {
                    Type::Named(name) => named.contains(&name.as_str()),
                    Type::Errors(errors) => errors.iter().all(|error| named.contains(&error.as_str())),
                    _ => false,
                }
            }
        }
    }
}

/// The errors a written `fails` names: one, or several joined with `|`.
fn names<'a>(written: &'a TypeExpr, found: &mut Vec<&'a str>) {
    if written.name == "|" {
        for arg in &written.args {
            names(arg, found);
        }
    } else {
        found.push(written.name.as_str());
    }
}

/// `_`, or a `_` given a name of its own before lowering.
fn is_discard(target: &Expr) -> bool {
    matches!(&target.kind, ExprKind::Name(name) if name == "_" || name.starts_with("_'"))
}

fn is_expect(callee: &Expr) -> bool {
    matches!(&callee.kind, ExprKind::Attr { name, .. } if name == "expect")
}
