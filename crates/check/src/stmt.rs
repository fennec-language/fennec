//! Checking statements: blocks and their values, `if` and loops (and how they narrow
//! optionals), `return`, `fail`, `yield`, `with`, and nested functions.

use fennec_syntax::{BinOp, Diagnostic, Expr, ExprKind, FnDef, Span, Stmt, StmtKind, UnaryOp};

use crate::effects;
use crate::function::{Binding, FnCx, Origin};
use crate::messages::unhandled;
use crate::types::{Type, article};

/// How a block or statement finishes: its value, and whether it always leaves early
/// (`return`, `fail`, `break` or `continue`), so nothing after it runs.
pub(crate) struct Flow {
    pub(crate) ty: Type,
    pub(crate) exits: bool,
}

impl Flow {
    pub(crate) fn unit() -> Flow {
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
            if last && used && matches!(stmt.kind, StmtKind::Expr(_)) {
                self.expecting = self.tail.clone();
            }
            let this = self.stmt(stmt, last && used);
            if matches!(stmt.kind, StmtKind::Expr(_)) && !(last && used) && !this.exits {
                match self.checker.settled(&this.ty) {
                    Type::Result(..) => self.error(unhandled(stmt.span)),
                    ty if gives_something(&ty) => self.error(unused(&ty, stmt.span)),
                    _ => {}
                }
            }
            // Code after leaving early never runs, but it's still checked. A value it ends on
            // isn't the block's, but takes what's expected of it where it can, so `[]` there
            // knows what it holds.
            if !flow.exits {
                flow = this;
            } else if last
                && used
                && let Some(tail) = self.tail.clone()
            {
                _ = self.checker.fits(&tail, &this.ty);
            }
        }
        flow
    }

    fn stmt(&mut self, stmt: &Stmt, used: bool) -> Flow {
        match &stmt.kind {
            StmtKind::Expr(expr) => {
                if matches!(expr.kind, ExprKind::Match { .. }) {
                    self.statement_matches.push((expr.span.start, expr.span.end));
                }
                let ty = self.expr(expr);
                if always_leaves(expr) { Flow::exit() } else { Flow { ty, exits: false } }
            }
            StmtKind::Assign { target, value, ty: Some(written), .. }
                if written.name == fennec_syntax::REF && written.args.is_empty() =>
            {
                if let ExprKind::Name(name) = &target.kind {
                    self.local_ref(name, value, target.span);
                }
                Flow::unit()
            }
            StmtKind::Assign { target, value, ty: Some(written), .. }
                if written.name == crate::lock::SHARED && written.args.is_empty() =>
            {
                if let ExprKind::Name(name) = &target.kind {
                    self.shared_declaration(name, value, target.span);
                }
                Flow::unit()
            }
            StmtKind::Assign { target, op, value, declare_mut, ty } => {
                self.note_arrow(target, value);
                self.assign(target, *op, value, *declare_mut, ty.as_ref());
                Flow::unit()
            }
            StmtKind::If { branches, otherwise } => {
                self.if_stmt(branches, otherwise.as_deref(), used, stmt.span)
            }
            StmtKind::While { cond, body } => {
                self.loops += 1;
                let flow = self.while_stmt(cond, body);
                self.loops -= 1;
                flow
            }
            StmtKind::For { names, is_mut, iter, body } => {
                self.loops += 1;
                match names.as_slice() {
                    [at, item] if *is_mut => self.enumerate_ref((at, item), iter, body, stmt.span),
                    _ => self.for_stmt(names, *is_mut, iter, body, stmt.span),
                }
                self.loops -= 1;
                Flow::unit()
            }
            StmtKind::Return(value) => self.return_stmt(value.as_ref(), stmt.span),
            StmtKind::Fail(error) => {
                self.fail(error);
                Flow::exit()
            }
            StmtKind::Yield(value) => self.yield_stmt(value, stmt.span),
            StmtKind::Assert(cond) => {
                if let Some((value, pattern)) = cond.asserted_failure() {
                    self.assert_fails(value, pattern, stmt.span);
                    self.record(cond.span, &Type::Bool);
                } else {
                    // What an `assert` holds holds after it, as after `if not cond: fail`:
                    // `assert problem is not None` lets `problem.message` be read.
                    self.condition(cond, "assert");
                    self.track(cond);
                    self.narrow(&not_none(cond, true));
                    self.narrow_members(&members(cond, true));
                }
                Flow::unit()
            }
            StmtKind::OnFailure { name, cleanup, body } => {
                self.on_failure(name, cleanup, stmt.span);
                self.block(body, used)
            }
            StmtKind::With { value, names, body, lock: true } => self.lock_block(value, names, body, used),
            StmtKind::With { value, names, body, .. } => self.with_block(value, names, body, used, stmt.span),
            StmtKind::Def(def) => {
                self.nested(def);
                Flow::unit()
            }
            StmtKind::Break | StmtKind::Continue => self.leave_loop(stmt),
            StmtKind::Pass => Flow::unit(),
            StmtKind::Lend { value, otherwise } => self.lend(value, otherwise, stmt.span),
        }
    }

    /// `return`, with a value or not, against what the function says it gives back.
    fn return_stmt(&mut self, value: Option<&Expr>, span: Span) -> Flow {
        if self.lends.is_some() {
            if let Some(value) = value {
                self.expr(value);
            }
            self.error(
                Diagnostic::new(format!("`{}` is a `with` source, so it doesn't `return`", self.name), span)
                    .with_help(
                        "it gives its caller what it lends with `lend`; to stop before lending, `fail`",
                    ),
            );
            return Flow::exit();
        }
        let declared = self.declared.clone().filter(|_| !self.generator);
        // `return Err(...)` in a function that says `fails E` makes a result of both.
        let declared = match (value.as_ref().map(|value| &value.kind), &self.fails) {
            (Some(ExprKind::Call { callee, .. }), Some(error)) if matches!(&callee.kind, ExprKind::Name(name) if name == "Err" || name == "Ok") => {
                Some(Type::result(declared.unwrap_or(Type::Unit), error.clone()))
            }
            _ => declared,
        };
        let ty = match (value, &declared) {
            (Some(value), Some(declared)) => self.expr_as(value, declared),
            (Some(value), None) => self.expr(value),
            (None, _) => Type::Unit,
        };
        if let Some(value) = value {
            self.passes_up_unmarked(value, &ty);
        }
        // A function that says `fails E` returns nothing without an arrow, or fails.
        let checked = self.declared.clone().or_else(|| self.fails.as_ref().map(|_| Type::Unit));
        if let (Some(declared), Some(value)) = (checked, value) {
            self.check_return(&declared, &ty, value.span);
        }
        if let (Some(declared), None) = (declared.filter(|ty| *ty != Type::Unit), value) {
            let declared = crate::types::a(&declared);
            self.error(
                Diagnostic::new(
                    format!("`{}` returns {declared}, but this gives back nothing", self.name),
                    span,
                )
                .with_help(format!("return {declared}, or change the return type")),
            );
        }
        self.returns.push(ty);
        Flow::exit()
    }

    /// `yield`, whose items have to fit `-> Iterator[T]`.
    fn yield_stmt(&mut self, value: &Expr, span: Span) -> Flow {
        self.not_under_lock(
            "`yield` can't pause",
            span,
            "work out the items under the lock, then yield them after the block",
        );
        let ty = self.expr(value);
        // `-> Iterator[T]` says what each item it yields is.
        if let Some(Type::Iterator(item) | Type::List(item)) = self.declared.clone()
            && !self.checker.fits(&item, &ty)
        {
            let what = format!("what `{}` yields", self.name);
            self.error(crate::messages::mismatch(&what, &item, &ty, value.span));
        }
        self.yields.push(ty);
        Flow::unit()
    }

    /// `break` or `continue`, which only a loop can take.
    fn leave_loop(&mut self, stmt: &Stmt) -> Flow {
        if self.loops == 0 {
            let word = if matches!(stmt.kind, StmtKind::Break) { "break" } else { "continue" };
            self.error(
                Diagnostic::new(
                    format!("`{word}` is only for leaving a loop, and this isn't in one"),
                    stmt.span,
                )
                .with_help("to leave the function, use `return`"),
            );
        }
        Flow::exit()
    }

    /// A block's cleanup for what fails inside what it calls, with the failure in `name`. It
    /// runs instead of what follows, so what it learns goes no further.
    fn on_failure(&mut self, name: &str, cleanup: &[Stmt], at: Span) {
        let failure = self.fails.clone().unwrap_or(Type::Unknown);
        let before = self.snapshot();
        self.push(false);
        self.declare(name, failure, false, at);
        self.stmts(cleanup, false);
        self.pop();
        self.restore(&before);
    }

    fn fail(&mut self, error: &Expr) {
        // A leading-dot case here is a case of the error the function says it fails with.
        let ty = match self.fails.clone() {
            Some(fails) => self.expr_as(error, &fails),
            None => self.expr(error),
        };
        if self.checker.is_error_type(&ty) || matches!(ty, Type::Errors(_)) {
            self.errors.push(ty);
        } else if ty != Type::Unknown {
            self.error(
                Diagnostic::new(
                    format!("`fail` needs an error, but this is {}", article(&ty.kind())),
                    error.span,
                )
                .with_help("declare error types with `error`, like `error NotFound:`"),
            );
        }
    }

    /// `if`, `elif` and `else`. Each branch sees what the conditions before it ruled out,
    /// and afterwards each name holds what it held on any branch that didn't leave early.
    fn if_stmt(
        &mut self,
        branches: &[(Expr, Vec<Stmt>)],
        otherwise: Option<&[Stmt]>,
        used: bool,
        span: Span,
    ) -> Flow {
        for (cond, _) in branches {
            self.track(cond);
        }
        let before = self.snapshot();
        let mut ruled_out = Vec::new();
        let mut not_members = Vec::new();
        let mut paths = Vec::new();
        let mut values = Vec::new();
        let mut arms: Vec<Arm<'_>> = Vec::new();
        for (cond, body) in branches {
            self.narrow(&ruled_out);
            self.narrow_members(&not_members);
            self.condition(cond, "if");
            let mut known = ruled_out.clone();
            known.extend(not_none(cond, true));
            let mut held = not_members.clone();
            held.extend(members(cond, true));
            arms.push((known, held, Some(body)));
            ruled_out.extend(not_none(cond, false));
            not_members.extend(members(cond, false));
            self.restore(&before);
        }
        arms.push((ruled_out, not_members, otherwise));
        for (known, held, body) in arms {
            self.narrow(&known);
            self.narrow_members(&held);
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
        let ty = if otherwise.is_some() && used && !values.is_empty() {
            self.join_or_report(&values, span, "the branches of this `if` give")
        } else {
            Type::Unit
        };
        // An `if` that leaves on every path has no value, like any other way of leaving.
        if paths.is_empty() {
            return Flow::exit();
        }
        Flow { ty, exits: false }
    }

    fn while_stmt(&mut self, cond: &Expr, body: &[Stmt]) -> Flow {
        self.forget_changed_in(body);
        self.forget_changed_by(cond);
        self.condition(cond, "while");
        self.track(cond);
        let before = self.snapshot();
        self.narrow(&not_none(cond, true));
        self.narrow_members(&members(cond, true));
        self.block(body, false);
        let after = self.snapshot();
        self.merge(&[before, after]);
        let forever = matches!(cond.kind, ExprKind::Bool(true)) && !effects::breaks(body);
        if forever { Flow::exit() } else { Flow::unit() }
    }

    /// `for mut` over a type of its own goes through its positions: its `def len` and the
    /// `mut` form of its `def []`, which gives the item's type. A type with only `def iter` makes
    /// copies, so a change to one would be lost. `None` for any other type.
    fn positions_item(&mut self, ty: &Type, in_parallel: bool, span: Span) -> Option<Type> {
        let ty = self.checker.settled(ty);
        let (Type::Named(name) | Type::Generic(name, _)) = &ty else { return None };
        let give_positions = format!(
            "give `{name}` positions: `def len(self) -> int` and the `ref` form, `def [](ref self, i: int)`"
        );
        let Some((key, entry)) = self.checker.program.method(name, "[]mut") else {
            if self.checker.program.method(name, "iter").is_some() {
                let message = format!(
                    "`for ref` can't change the items of a `{name}`, since its `iter` makes copies of them"
                );
                self.error(Diagnostic::new(message, span).with_help(give_positions));
            }
            return None;
        };
        let own_items = crate::protocols::writes_one_item(&entry.def);
        if self.checker.program.method(name, "len").is_none() {
            let message = format!("`for ref` over a `{name}` needs its `def len`");
            self.error(Diagnostic::new(message, span).with_help(give_positions));
        } else if in_parallel && !own_items {
            let message = format!(
                "`for ref` over `parallel(...)` needs `{name}`'s `ref` form of `def []` to touch only its own item"
            );
            let help = "write it as `return self.field[i]`, or use a plain `for ref`";
            self.error(Diagnostic::new(message, span).with_help(help));
        }
        // The loop calls `len`, and `is_live` where there is one, so they're reached too, as the
        // ones of a library type have to be when only what's reached is checked.
        for counting in ["len", "is_live"] {
            if let Some((counted, _)) = self.checker.program.method(name, counting) {
                let _ = self.method_signature(&ty, &counted);
            }
        }
        self.method_signature(&ty, &key).map(|signature| signature.returns)
    }

    fn for_stmt(&mut self, names: &[String], is_mut: bool, iter: &Expr, body: &[Stmt], span: Span) {
        if !is_mut && self.fields_loop(names, iter, body, span).is_some() {
            return;
        }
        let parallel = crate::parallel::parallel_items(iter).filter(|_| self.lookup("parallel").is_none());
        if is_mut {
            let what = "`for ref` changes each item, so loop over something held in a mut name";
            self.mut_place(parallel.unwrap_or(iter), what);
        }
        if parallel.is_some() {
            self.note_wait("`parallel`", iter.span);
            self.writes_only_its_own(body, names);
        }
        let positions = match parallel.filter(|_| is_mut) {
            // What's split is `xs` itself, which needs no `def iter`.
            Some(items) => {
                let held = self.expr(items);
                let item = self.positions_item(&held, true, items.span);
                item.inspect(|item| self.record(iter.span, &Type::list(item.clone())))
            }
            None if is_mut => {
                let held = self.peek_type(iter);
                self.positions_item(&held, false, iter.span)
            }
            None => None,
        };
        let item = if let Some(item) = positions {
            if parallel.is_none() {
                self.expr(iter);
            }
            item
        } else {
            let iter_ty = self.expr(iter);
            self.item_type(&iter_ty, iter.span)
        };
        self.forget_changed_in(body);
        let before = self.snapshot();
        self.push(false);
        self.bind_names(names, &item, is_mut, (iter.span, span));
        self.stmts(body, false);
        self.pop();
        let after = self.snapshot();
        self.merge(&[before, after]);
    }

    /// `for at, ref item in enumerate(items):`: each item changed where it is, with its position.
    /// `items` needs positions, as a `for ref` over it does. Once checked, it's written in as a
    /// `for ref` that counts.
    fn enumerate_ref(&mut self, (at, item): (&str, &str), iter: &Expr, body: &[Stmt], span: Span) {
        let positions = match &iter.kind {
            ExprKind::Call { callee, args }
                if let [arg] = args.as_slice()
                    && matches!(&callee.kind, ExprKind::Name(name) if name == "enumerate")
                    && self.lookup("enumerate").is_none()
                    && !self.checker.program.functions.contains_key("enumerate")
                    && arg.name.is_none()
                    && !arg.is_mut =>
            {
                Some(&arg.value)
            }
            _ => None,
        };
        let Some(items) = positions else {
            self.error(
                Diagnostic::new("`ref` with two names is for `enumerate(items)`", iter.span)
                    .with_help("write `for at, ref item in enumerate(items):`, or `for ref item in items:`"),
            );
            return self.for_stmt(&[at.to_owned(), item.to_owned()], false, iter, body, span);
        };
        self.mut_place(items, "`for ref` changes each item, so loop over something held in a mut name");
        let held = self.peek_type(items);
        let item_ty = if let Some(item) = self.positions_item(&held, false, items.span) {
            self.expr(items);
            item
        } else {
            let ty = self.expr(items);
            self.item_type(&ty, items.span)
        };
        self.record(iter.span, &Type::Iterator(Box::new(Type::Tuple(vec![Type::Int, item_ty.clone()]))));
        self.forget_changed_in(body);
        let before = self.snapshot();
        self.push(false);
        self.declare_as(at, Type::Int, false, Origin::Loop, span);
        self.declare_as(item, item_ty, true, Origin::Loop, span);
        self.stmts(body, false);
        self.pop();
        let after = self.snapshot();
        self.merge(&[before, after]);
    }

    /// Binds a loop's names to an item: one name, or several unpacking a tuple. `spans`
    /// are what's looped over, for problems, and the loop, where the names are declared.
    pub(crate) fn bind_names(&mut self, names: &[String], item: &Type, is_mut: bool, spans: (Span, Span)) {
        let (span, at) = spans;
        if let [name] = names {
            self.declare_as(name, item.clone(), is_mut, Origin::Loop, at);
            return;
        }
        let parts = self.unpack(item, names.len(), span);
        for (name, ty) in names.iter().zip(parts) {
            self.declare_as(name, ty, is_mut, Origin::Loop, at);
        }
    }

    /// The types of the parts of a tuple being unpacked into `count` names.
    pub(crate) fn unpack(&mut self, ty: &Type, count: usize, span: Span) -> Vec<Type> {
        match &self.checker.settled(ty) {
            Type::Tuple(items) if items.len() == count => return items.clone(),
            Type::Tuple(items) => self.error(Diagnostic::new(
                format!("can't unpack {} values into {count} names", items.len()),
                span,
            )),
            Type::Unknown => {}
            Type::Var(_) => {
                let parts: Vec<Type> =
                    (0..count).map(|_| self.checker.fresh(span, crate::infer::Hole::Unpacked)).collect();
                let tuple = Type::Tuple(parts.clone());
                self.checker.fits(ty, &tuple);
                return parts;
            }
            other => self.error(Diagnostic::new(
                format!("can't unpack {} into {count} names", article(&other.kind())),
                span,
            )),
        }
        vec![Type::Unknown; count]
    }

    /// A function defined inside this one. It can read the names around it, but not change them.
    /// `work = () => ...`: an arrow function given to a name, which may capture a handle.
    fn note_arrow(&mut self, target: &Expr, value: &Expr) {
        let (ExprKind::Name(name), ExprKind::Lambda { params, body }) = (&target.kind, &value.kind) else {
            return;
        };
        let mut reads = Vec::new();
        fennec_syntax::visit::in_expr(&mut body.as_ref().clone(), &mut |seen| names_read(seen, &mut reads));
        self.note_closure(name, &reads, params);
    }

    fn nested(&mut self, def: &FnDef) {
        let mut reads = Vec::new();
        fennec_syntax::visit::in_function(&mut def.clone(), &mut |seen| names_read(seen, &mut reads));
        let own: Vec<String> = def.params.iter().map(|param| param.name.clone()).collect();
        self.note_closure(&def.name, &reads, &own);
        let key = format!("{}.{}", self.key, def.name);
        let mut captured = self.visible();
        let function = Type::Function(key.clone());
        captured.push(Binding {
            name: def.name.clone(),
            ty: function.clone(),
            declared: function.clone(),
            is_mut: false,
            origin: Origin::Captured,
            mut_at: None,
        });
        let report = self.report;
        let inherited = self.generics.clone();
        self.checker.check_nested(&key, def, captured, !report, inherited);
        self.declare(&def.name, function, false, def.span);
    }

    /// Checks a condition is a bool.
    pub(crate) fn condition(&mut self, cond: &Expr, keyword: &str) {
        let ty = self.expr(cond);
        if !self.checker.fits(&Type::Bool, &ty) {
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

/// A name, a member of its union by its written name, and whether the name holds that member.
pub(crate) type Member = (String, String, bool);

/// One way through an `if`: the names known not to be None there, what's known of the unions
/// names hold, and its body, if it has one.
type Arm<'s> = (Vec<String>, Vec<Member>, Option<&'s [Stmt]>);

/// What a condition shows about which member of a union a name holds, when it's `true` or when
/// it's `false`: `s is Circle` shows `s` holds a `Circle` when true and doesn't when false, and
/// `and`, `or` and `not` combine them as for `None`. Each is the path, the member's written name,
/// and whether it holds that member.
pub(crate) fn members(cond: &Expr, when: bool) -> Vec<Member> {
    match &cond.kind {
        ExprKind::Binary { op: op @ (BinOp::Is | BinOp::IsNot), lhs, rhs } => match (path(lhs), &rhs.kind) {
            (Some(path), ExprKind::Name(member)) => vec![(path, member.clone(), (*op == BinOp::Is) == when)],
            _ => Vec::new(),
        },
        ExprKind::Binary { op: BinOp::And, lhs, rhs } if when => {
            members(lhs, true).into_iter().chain(members(rhs, true)).collect()
        }
        ExprKind::Binary { op: BinOp::Or, lhs, rhs } if !when => {
            members(lhs, false).into_iter().chain(members(rhs, false)).collect()
        }
        ExprKind::Unary { op: UnaryOp::Not, expr } => members(expr, !when),
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

/// Whether a `match` never gets past itself, because every case leaves.
fn always_leaves(expr: &Expr) -> bool {
    let ExprKind::Match { arms, .. } = &expr.kind else { return false };
    arms.iter().all(|arm| block_leaves(&arm.body))
}

fn block_leaves(stmts: &[Stmt]) -> bool {
    match stmts.last().map(|stmt| &stmt.kind) {
        Some(StmtKind::Return(_) | StmtKind::Fail(_) | StmtKind::Break | StmtKind::Continue) => true,
        Some(StmtKind::If { branches, otherwise: Some(otherwise) }) => {
            branches.iter().all(|(_, body)| block_leaves(body)) && block_leaves(otherwise)
        }
        Some(StmtKind::Expr(inner)) => always_leaves(inner),
        _ => false,
    }
}

fn names_read(expr: &Expr, reads: &mut Vec<String>) {
    if let ExprKind::Name(name) = &expr.kind {
        reads.push(name.clone());
    }
}

/// Whether a value is something, so a statement that gives it and drops it loses it.
fn gives_something(ty: &Type) -> bool {
    !matches!(ty, Type::Unit | Type::Unknown | Type::Var(_) | Type::None)
}

/// A statement's value, thrown away without saying so. Throwing it away could hide a mistake,
/// like `sorted(scores)` for `scores.sort()`, so it's a fix to make by hand.
fn unused(ty: &Type, span: Span) -> Diagnostic {
    Diagnostic::new(format!("this gives back {}, and nothing uses it", crate::types::a(ty)), span).with_hint(
        "use it, or throw it away on purpose with `_ = ...`",
        Span { start: span.start, end: span.start },
        "_ = ",
    )
}
