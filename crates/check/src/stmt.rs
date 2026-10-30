//! Checking statements: blocks and their values, `if` and loops (and how they narrow
//! optionals), `return`, `fail`, `yield`, `with`, and nested functions.

use fennec_syntax::{BinOp, Diagnostic, Expr, ExprKind, FnDef, Stmt, StmtKind, UnaryOp};

use crate::effects;
use crate::function::{Binding, FnCx, Origin};
use crate::messages::unhandled;
use crate::types::{Type, article, join_all};

/// How a block or statement finishes: its value, and whether it always leaves early
/// (`return`, `fail`, `break` or `continue`), so nothing after it runs.
pub(crate) struct Flow {
    pub(crate) ty: Type,
    pub(crate) exits: bool,
}

impl Flow {
    fn unit() -> Flow {
        Flow { ty: Type::Unit, exits: false }
    }

    fn exit() -> Flow {
        Flow { ty: Type::Unknown, exits: true }
    }
}

impl FnCx<'_> {
    /// Checks a block in its own scope. `used` says whether its value is used, as a function
    /// body's or a `match` arm's is.
    pub(crate) fn block(&mut self, stmts: &[Stmt], used: bool) -> Flow {
        self.push(false);
        let flow = self.stmts(stmts, used);
        self.pop();
        flow
    }

    /// Checks statements in the current scope. A block's value is its last statement's.
    pub(crate) fn stmts(&mut self, stmts: &[Stmt], used: bool) -> Flow {
        let mut flow = Flow::unit();
        for (at, stmt) in stmts.iter().enumerate() {
            let last = at + 1 == stmts.len();
            let this = self.stmt(stmt, last && used);
            if matches!(stmt.kind, StmtKind::Expr(_))
                && !(last && used)
                && matches!(this.ty, Type::Result(..))
            {
                self.error(unhandled(stmt.span));
            }
            // Code after leaving early never runs, but it's still checked.
            if !flow.exits {
                flow = this;
            }
        }
        flow
    }

    fn stmt(&mut self, stmt: &Stmt, used: bool) -> Flow {
        match &stmt.kind {
            StmtKind::Expr(expr) => Flow { ty: self.expr(expr), exits: false },
            StmtKind::Assign { target, op, value, declare_mut } => {
                self.assign(target, *op, value, *declare_mut);
                Flow::unit()
            }
            StmtKind::If { branches, otherwise } => self.if_stmt(branches, otherwise.as_deref(), used),
            StmtKind::While { cond, body } => self.while_stmt(cond, body),
            StmtKind::For { names, is_mut, iter, body } => {
                self.for_stmt(names, *is_mut, iter, body);
                Flow::unit()
            }
            StmtKind::Return(value) => {
                let ty = value.as_ref().map_or(Type::Unit, |value| self.expr(value));
                if let (Some(declared), Some(value)) = (self.declared.clone(), value) {
                    self.check_return(&declared, &ty, value.span);
                }
                self.returns.push(ty);
                Flow::exit()
            }
            StmtKind::Fail(error) => {
                self.fail(error);
                Flow::exit()
            }
            StmtKind::Yield(value) => {
                let ty = self.expr(value);
                self.yields.push(ty);
                Flow::unit()
            }
            StmtKind::Assert(cond) => {
                self.condition(cond, "assert");
                Flow::unit()
            }
            StmtKind::With { value, name, is_mut, body } => {
                let ty = self.expr(value);
                self.push(false);
                if let Some(name) = name {
                    self.declare(name, ty, *is_mut);
                }
                let flow = self.stmts(body, used);
                self.pop();
                flow
            }
            StmtKind::Def(def) => {
                self.nested(def);
                Flow::unit()
            }
            StmtKind::Break | StmtKind::Continue => Flow::exit(),
            StmtKind::Pass => Flow::unit(),
        }
    }

    fn fail(&mut self, error: &Expr) {
        let ty = self.expr(error);
        if ty != Type::Unknown && !self.checker.is_error_type(&ty) {
            self.error(
                Diagnostic::new(
                    format!("`fail` needs an error, but this is {}", article(&ty.kind())),
                    error.span,
                )
                .with_help("declare error types with `error`, like `error NotFound:`"),
            );
        }
        self.errors.push(ty);
    }

    /// `if`, `elif` and `else`. Each branch sees what the conditions before it ruled out,
    /// and afterwards each name holds what it held on any branch that didn't leave early.
    fn if_stmt(&mut self, branches: &[(Expr, Vec<Stmt>)], otherwise: Option<&[Stmt]>, used: bool) -> Flow {
        for (cond, _) in branches {
            self.track(cond);
        }
        let before = self.snapshot();
        let mut ruled_out = Vec::new();
        let mut paths = Vec::new();
        let mut values = Vec::new();
        let mut arms: Vec<(Vec<String>, Option<&[Stmt]>)> = Vec::new();
        for (cond, body) in branches {
            self.narrow(&ruled_out);
            self.condition(cond, "if");
            let mut known = ruled_out.clone();
            known.extend(not_none(cond, true));
            arms.push((known, Some(body)));
            ruled_out.extend(not_none(cond, false));
            self.restore(&before);
        }
        arms.push((ruled_out, otherwise));
        for (known, body) in arms {
            self.narrow(&known);
            let flow = match body {
                Some(body) => self.block(body, used),
                None => Flow::unit(),
            };
            if !flow.exits {
                paths.push(self.snapshot());
                values.push(flow.ty);
            }
            self.restore(&before);
        }
        self.merge(&paths);
        let ty = if otherwise.is_some() && used { join_all(&values) } else { Type::Unit };
        Flow { ty, exits: paths.is_empty() }
    }

    fn while_stmt(&mut self, cond: &Expr, body: &[Stmt]) -> Flow {
        self.condition(cond, "while");
        self.track(cond);
        let before = self.snapshot();
        self.narrow(&not_none(cond, true));
        self.block(body, false);
        let after = self.snapshot();
        self.merge(&[before, after]);
        let forever = matches!(cond.kind, ExprKind::Bool(true)) && !effects::breaks(body);
        if forever { Flow::exit() } else { Flow::unit() }
    }

    fn for_stmt(&mut self, names: &[String], is_mut: bool, iter: &Expr, body: &[Stmt]) {
        if is_mut {
            self.mut_place(iter, "`for mut` changes each item, so loop over something held in a mut name");
        }
        let iter_ty = self.expr(iter);
        let item = self.item_type(&iter_ty, iter.span);
        let before = self.snapshot();
        self.push(false);
        self.bind_names(names, &item, is_mut, iter.span);
        self.stmts(body, false);
        self.pop();
        let after = self.snapshot();
        self.merge(&[before, after]);
    }

    /// Binds a loop's names to an item: one name, or several unpacking a tuple.
    pub(crate) fn bind_names(
        &mut self,
        names: &[String],
        item: &Type,
        is_mut: bool,
        span: fennec_syntax::Span,
    ) {
        if let [name] = names {
            self.declare_as(name, item.clone(), is_mut, Origin::Loop);
            return;
        }
        let parts = self.unpack(item, names.len(), span);
        for (name, ty) in names.iter().zip(parts) {
            self.declare_as(name, ty, is_mut, Origin::Loop);
        }
    }

    /// The types of the parts of a tuple being unpacked into `count` names.
    pub(crate) fn unpack(&mut self, ty: &Type, count: usize, span: fennec_syntax::Span) -> Vec<Type> {
        match ty {
            Type::Tuple(items) if items.len() == count => return items.clone(),
            Type::Tuple(items) => self.error(Diagnostic::new(
                format!("can't unpack {} values into {count} names", items.len()),
                span,
            )),
            Type::Unknown => {}
            other => self.error(Diagnostic::new(
                format!("can't unpack {} into {count} names", article(&other.kind())),
                span,
            )),
        }
        vec![Type::Unknown; count]
    }

    /// A function defined inside this one. It can read the names around it, but not change them.
    fn nested(&mut self, def: &FnDef) {
        let key = format!("{}.{}", self.key, def.name);
        let mut captured = self.visible();
        captured.push(Binding {
            name: def.name.clone(),
            ty: Type::Function(key.clone()),
            is_mut: false,
            origin: Origin::Captured,
        });
        let report = self.report;
        self.checker.check_nested(&key, def, captured, !report);
        self.declare(&def.name, Type::Function(key), false);
    }

    /// Checks a condition is a bool.
    pub(crate) fn condition(&mut self, cond: &Expr, keyword: &str) {
        let ty = self.expr(cond);
        if ty != Type::Bool && ty != Type::Unknown {
            self.error(crate::messages::not_bool(keyword, &ty, cond.span));
        }
    }
}

/// The names a condition shows aren't None, when it's `true` or when it's `false`:
/// `x is not None` when true, `x is None` when false, and `and`, `or` and `not` of those. A
/// field path like `player.weapon` counts as a name.
pub(crate) fn not_none(cond: &Expr, when: bool) -> Vec<String> {
    match &cond.kind {
        ExprKind::Binary { op: op @ (BinOp::Is | BinOp::IsNot), lhs, rhs } => match (path(lhs), &rhs.kind) {
            (Some(path), ExprKind::None) if (*op == BinOp::IsNot) == when => vec![path],
            _ => Vec::new(),
        },
        ExprKind::Binary { op: BinOp::And, lhs, rhs } if when => {
            not_none(lhs, true).into_iter().chain(not_none(rhs, true)).collect()
        }
        ExprKind::Binary { op: BinOp::Or, lhs, rhs } if !when => {
            not_none(lhs, false).into_iter().chain(not_none(rhs, false)).collect()
        }
        ExprKind::Unary { op: UnaryOp::Not, expr } => not_none(expr, !when),
        _ => Vec::new(),
    }
}

/// A name, or a path of fields from one: `player.weapon`.
pub(crate) fn path(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Name(name) => Some(name.clone()),
        ExprKind::Attr { object, name, optional: false } => {
            path(object).map(|object| format!("{object}.{name}"))
        }
        _ => None,
    }
}
