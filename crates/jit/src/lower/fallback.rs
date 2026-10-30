//! Handing one operation to the interpreter. Its inputs are computed natively first and
//! passed under hidden names, so only the operation itself runs there: a method call, a
//! field read, building a list. Parts that must stay as written, like the receiver of a
//! method that changes it, or an arrow function's body, see the native names they use
//! through a scope declared just for them.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{InstBuilder, Value};
use fennec_interp::embed::eval::input_name;
use fennec_syntax::{
    Arg, BinOp, Diagnostic, Expr, ExprKind, FStrPart, MatchArm, Pattern, Span, Stmt, StmtKind,
};

use super::Lowerer;
use super::expr::Lowered;
use crate::helpers::Helper;
use crate::types::Ty;

/// What an operation handed to the interpreter needs: its inputs, and the written parts it
/// keeps, whose names must be visible.
#[derive(Default)]
struct Handover {
    inputs: Vec<Value>,
    kept: Vec<String>,
}

impl Lowerer<'_, '_> {
    /// Runs an expression in the interpreter, with its inputs computed natively.
    pub(super) fn fallback(&mut self, expr: &Expr) -> Result<Lowered, Diagnostic> {
        let mut handover = Handover::default();
        let rebuilt = self.hoist(expr, &mut handover)?;
        let handle = self.run_expr(rebuilt, handover, expr.span)?;
        Ok((Some(handle), Ty::Obj))
    }

    /// Runs a method call in the interpreter, with its receiver and arguments already
    /// computed. A receiver that's a name, or a field of one, stays as written, so a method
    /// that changes it changes it in place.
    pub(super) fn interpret_method(
        &mut self,
        expr: &Expr,
        object: &Expr,
        receiver: Value,
        method: &str,
        args: &[Lowered],
    ) -> Result<Lowered, Diagnostic> {
        let mut handover = Handover::default();
        let object = if self.keeps_receiver(object) {
            Self::keep(object, &mut handover)
        } else {
            self.input_value(Some(receiver), Ty::Obj, object.span, &mut handover)?
        };
        let mut rebuilt = Vec::new();
        for (value, ty) in args {
            let value = self.input_value(*value, *ty, expr.span, &mut handover)?;
            rebuilt.push(Arg { name: None, is_mut: false, value });
        }
        let callee = Expr {
            kind: ExprKind::Attr { object: Box::new(object), name: method.to_owned(), optional: false },
            span: expr.span,
        };
        let kind = ExprKind::Call { callee: Box::new(callee), args: rebuilt };
        let handle = self.run_expr(Expr { kind, span: expr.span }, handover, expr.span)?;
        Ok((Some(handle), Ty::Obj))
    }

    /// Runs a call in the interpreter, with its plain arguments already computed.
    pub(super) fn interpret_call(&mut self, call: &Expr, args: &[Lowered]) -> Result<Lowered, Diagnostic> {
        let ExprKind::Call { callee, .. } = &call.kind else { return self.fallback(call) };
        let mut handover = Handover::default();
        let mut rebuilt = Vec::new();
        for (value, ty) in args {
            let value = self.input_value(*value, *ty, call.span, &mut handover)?;
            rebuilt.push(Arg { name: None, is_mut: false, value });
        }
        let callee = Self::keep(callee, &mut handover);
        let kind = ExprKind::Call { callee: Box::new(callee), args: rebuilt };
        let handle = self.run_expr(Expr { kind, span: call.span }, handover, call.span)?;
        Ok((Some(handle), Ty::Obj))
    }

    /// Assigns into a field or item, or through a type's `def []=`.
    pub(super) fn interpret_assign(
        &mut self,
        target: &Expr,
        op: Option<BinOp>,
        value: &Expr,
        declare_mut: bool,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let mut handover = Handover::default();
        let value = self.input(value, &mut handover)?;
        let target = Self::keep(target, &mut handover);
        let stmt = Stmt { kind: StmtKind::Assign { target, op, value, declare_mut }, span };
        self.run_stmt(stmt, handover, span)
    }

    /// Runs a whole statement in the interpreter.
    pub(super) fn interpret_stmt(&mut self, stmt: &Stmt) -> Result<(), Diagnostic> {
        let mut handover = Handover::default();
        stmt_names(stmt, &mut handover.kept);
        self.run_stmt(stmt.clone(), handover, stmt.span)
    }

    /// Computes a part natively and hands it over as the next input. Literals stay as
    /// written: the interpreter treats values made only from literals specially.
    fn input(&mut self, expr: &Expr, handover: &mut Handover) -> Result<Expr, Diagnostic> {
        if matches!(
            expr.kind,
            ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Bool(_) | ExprKind::None
        ) {
            return Ok(expr.clone());
        }
        if let ExprKind::Name(name) = &expr.kind
            && self.local(name).is_some_and(|local| matches!(local.ty, Ty::Cap(_)))
        {
            return Ok(Self::keep(expr, handover));
        }
        let (value, ty) = self.expr(expr)?;
        self.input_value(value, ty, expr.span, handover)
    }

    fn input_value(
        &mut self,
        value: Option<Value>,
        ty: Ty,
        span: Span,
        handover: &mut Handover,
    ) -> Result<Expr, Diagnostic> {
        let handle = self.handle_of(value, ty, span)?;
        let name = input_name(handover.inputs.len());
        handover.inputs.push(handle);
        Ok(Expr { kind: ExprKind::Name(name), span })
    }

    /// Keeps a part as written, noting the names it uses.
    fn keep(expr: &Expr, handover: &mut Handover) -> Expr {
        expr_names(expr, &mut handover.kept);
        expr.clone()
    }

    /// Whether a method's receiver must stay as written: a name the frame holds, or a field
    /// or item of one, which a method that changes it changes in place; or a type's name.
    fn keeps_receiver(&self, object: &Expr) -> bool {
        match &object.kind {
            ExprKind::Name(name) => self.local(name).is_none_or(|local| local.ty == Ty::Obj),
            ExprKind::Attr { object, .. } | ExprKind::Index { object, .. } => self.keeps_receiver(object),
            _ => false,
        }
    }

    /// Whether a method call's receiver must stay as written: what `keeps_receiver` keeps,
    /// and a `mut` struct held natively, which a method that changes `self` changes.
    fn keeps_call_receiver(&self, object: &Expr) -> bool {
        match &object.kind {
            ExprKind::Name(name) => self.local(name).is_none_or(|local| {
                local.ty == Ty::Obj || (local.is_mut && matches!(local.ty, Ty::Struct(_)))
            }),
            ExprKind::Attr { object, .. } | ExprKind::Index { object, .. } => {
                self.keeps_call_receiver(object)
            }
            _ => false,
        }
    }

    /// Rebuilds an expression with its inputs computed natively.
    fn hoist(&mut self, expr: &Expr, handover: &mut Handover) -> Result<Expr, Diagnostic> {
        let kind = match &expr.kind {
            ExprKind::Unary { op, expr: inner } => {
                ExprKind::Unary { op: *op, expr: Box::new(self.input(inner, handover)?) }
            }
            ExprKind::Binary { op, lhs, rhs } if !matches!(op, BinOp::And | BinOp::Or) => {
                let lhs = self.input(lhs, handover)?;
                ExprKind::Binary { op: *op, lhs: Box::new(lhs), rhs: Box::new(self.input(rhs, handover)?) }
            }
            ExprKind::List(items) => ExprKind::List(self.inputs(items, handover)?),
            ExprKind::Tuple(items) => ExprKind::Tuple(self.inputs(items, handover)?),
            ExprKind::Set(items) => ExprKind::Set(self.inputs(items, handover)?),
            ExprKind::Dict(pairs) => {
                let mut rebuilt = Vec::with_capacity(pairs.len());
                for (key, value) in pairs {
                    let key = self.input(key, handover)?;
                    rebuilt.push((key, self.input(value, handover)?));
                }
                ExprKind::Dict(rebuilt)
            }
            ExprKind::Index { object, index } => {
                let object = self.input(object, handover)?;
                ExprKind::Index { object: Box::new(object), index: Box::new(self.input(index, handover)?) }
            }
            ExprKind::Slice { object, start, end } => {
                let object = Box::new(self.input(object, handover)?);
                let start =
                    start.as_deref().map(|start| self.input(start, handover)).transpose()?.map(Box::new);
                let end = end.as_deref().map(|end| self.input(end, handover)).transpose()?.map(Box::new);
                ExprKind::Slice { object, start, end }
            }
            ExprKind::Attr { object, name, optional } => {
                let object = if self.keeps_receiver(object) {
                    Self::keep(object, handover)
                } else {
                    self.input(object, handover)?
                };
                ExprKind::Attr { object: Box::new(object), name: name.clone(), optional: *optional }
            }
            ExprKind::Call { callee, args } => self.hoist_call(callee, args, handover)?,
            ExprKind::FStr(parts) => {
                let mut rebuilt = Vec::with_capacity(parts.len());
                for part in parts {
                    rebuilt.push(match part {
                        FStrPart::Text(text) => FStrPart::Text(text.clone()),
                        FStrPart::Expr { expr, spec } => {
                            FStrPart::Expr { expr: self.input(expr, handover)?, spec: spec.clone() }
                        }
                    });
                }
                ExprKind::FStr(rebuilt)
            }
            _ => return Ok(Self::keep(expr, handover)),
        };
        Ok(Expr { kind, span: expr.span })
    }

    fn inputs(&mut self, items: &[Expr], handover: &mut Handover) -> Result<Vec<Expr>, Diagnostic> {
        items.iter().map(|item| self.input(item, handover)).collect()
    }

    fn hoist_call(
        &mut self,
        callee: &Expr,
        args: &[Arg],
        handover: &mut Handover,
    ) -> Result<ExprKind, Diagnostic> {
        let callee = match &callee.kind {
            ExprKind::Attr { object, name, optional } => {
                let object = if self.keeps_call_receiver(object) {
                    Self::keep(object, handover)
                } else {
                    self.input(object, handover)?
                };
                Expr {
                    kind: ExprKind::Attr {
                        object: Box::new(object),
                        name: name.clone(),
                        optional: *optional,
                    },
                    span: callee.span,
                }
            }
            _ => Self::keep(callee, handover),
        };
        let mut rebuilt = Vec::with_capacity(args.len());
        for arg in args {
            let value =
                if arg.is_mut { Self::keep(&arg.value, handover) } else { self.input(&arg.value, handover)? };
            rebuilt.push(Arg { name: arg.name.clone(), is_mut: arg.is_mut, value });
        }
        Ok(ExprKind::Call { callee: Box::new(callee), args: rebuilt })
    }

    fn run_expr(&mut self, expr: Expr, handover: Handover, span: Span) -> Result<Value, Diagnostic> {
        let leaves = expr_leaves(&expr);
        let synced = self.sync(&handover.kept)?;
        for input in handover.inputs {
            self.call_helper(Helper::Input, &[input])?;
        }
        let id = i64::try_from(self.shared.tables.exprs.len()).unwrap_or(0);
        self.shared.tables.exprs.push(expr);
        let id = self.int(id);
        let [start, end] = self.span_args(span);
        let handle = self.help(Helper::Expr, &[id, start, end])?;
        self.unsync(synced, span)?;
        if leaves {
            self.carry_out_leaving(span)?;
        }
        Ok(handle)
    }

    fn run_stmt(&mut self, stmt: Stmt, handover: Handover, span: Span) -> Result<(), Diagnostic> {
        let leaves = stmt_leaves(&stmt);
        let synced = self.sync(&handover.kept)?;
        for input in handover.inputs {
            self.call_helper(Helper::Input, &[input])?;
        }
        let id = i64::try_from(self.shared.tables.stmts.len()).unwrap_or(0);
        self.shared.tables.stmts.push(stmt);
        let id = self.int(id);
        let [start, end] = self.span_args(span);
        self.call_helper(Helper::Stmt, &[id, start, end])?;
        self.unsync(synced, span)?;
        if leaves {
            self.carry_out_leaving(span)?;
        }
        Ok(())
    }

    /// After interpreted code that can `return` or pass a failure up, does it natively.
    fn carry_out_leaving(&mut self, span: Span) -> Result<(), Diagnostic> {
        let kind = self.help(Helper::Leaving, &[])?;
        let leave = self.b.create_block();
        let stay = self.b.create_block();
        self.b.ins().brif(kind, leave, &[], stay, &[]);
        self.b.switch_to_block(leave);
        self.b.seal_block(leave);
        let value = self.help(Helper::Left, &[])?;
        let returning = self.b.ins().icmp_imm_s(IntCC::Equal, kind, 1);
        let returns = self.b.create_block();
        let passes = self.b.create_block();
        self.b.ins().brif(returning, returns, &[], passes, &[]);
        self.b.switch_to_block(returns);
        self.b.seal_block(returns);
        self.ret(Some(value), Ty::Obj, span)?;
        self.b.switch_to_block(passes);
        self.b.seal_block(passes);
        self.pass_up(value)?;
        self.b.switch_to_block(stay);
        self.b.seal_block(stay);
        Ok(())
    }

    /// Declares the native names that kept parts use, in a scope of their own. Gives back
    /// the ones that can change, to read back after.
    fn sync(&mut self, kept: &[String]) -> Result<Option<Vec<String>>, Diagnostic> {
        let mut native: Vec<String> = kept
            .iter()
            .filter(|name| self.local(name).is_some_and(|local| local.ty != Ty::Obj && local.ty != Ty::Unit))
            .cloned()
            .collect();
        native.sort();
        native.dedup();
        if native.is_empty() {
            return Ok(None);
        }
        self.call_helper(Helper::PushScope, &[])?;
        let mut changeable = Vec::new();
        for name in native {
            let Some(local) = self.local(&name) else { continue };
            let id = self.name_id(&name);
            if let Ty::Cap(cap) = local.ty {
                let kind = self.int(cap.kind());
                self.call_helper(Helper::DeclareCapability, &[id, kind])?;
                continue;
            }
            let value = match self.local_place(local).filter(|_| matches!(local.ty, Ty::Struct(_))) {
                Some(place) => Some(self.read_place(&place)),
                None => local.var.map(|var| self.b.use_var(var)),
            };
            // The name keeps its own value.
            if let (true, Some(value)) = (local.ty.counted(), value) {
                self.borrow(value);
            }
            let handle = self.handle_of(value, local.ty, Span::default())?;
            let is_mut = self.b.ins().iconst(cranelift_codegen::ir::types::I8, i64::from(local.is_mut));
            self.call_helper(Helper::Declare, &[id, handle, is_mut])?;
            if local.is_mut {
                changeable.push(name);
            }
        }
        Ok(Some(changeable))
    }

    /// Reads back native names a kept part may have changed, and ends their scope.
    fn unsync(&mut self, synced: Option<Vec<String>>, span: Span) -> Result<(), Diagnostic> {
        let Some(changeable) = synced else { return Ok(()) };
        for name in changeable {
            let Some(local) = self.local(&name) else { continue };
            let Some(var) = local.var else { continue };
            let id = self.name_id(&name);
            let handle = self.help(Helper::Get, &[id])?;
            let value = self.convert(Some(handle), Ty::Obj, local.ty, span)?;
            if let (Ty::Struct(id), Some(place)) = (local.ty, self.local_place(local)) {
                self.write_place(&place, value, id)?;
            } else if local.ty.counted() {
                let old = self.b.use_var(var);
                self.b.def_var(var, value);
                self.release(old)?;
            } else {
                self.b.def_var(var, value);
            }
        }
        self.call_helper(Helper::PopScope, &[]).map(|_| ())
    }
}

/// Every name an expression mentions.
pub(super) fn expr_names(expr: &Expr, names: &mut Vec<String>) {
    let mut each = |expr: &Expr| expr_names(expr, names);
    match &expr.kind {
        ExprKind::Name(name) => names.push(name.clone()),
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Bool(_)
        | ExprKind::None
        | ExprKind::Variant(_) => {}
        ExprKind::FStr(parts) => {
            for part in parts {
                if let FStrPart::Expr { expr, .. } = part {
                    each(expr);
                }
            }
        }
        ExprKind::List(items) | ExprKind::Tuple(items) | ExprKind::Set(items) => {
            for item in items {
                each(item);
            }
        }
        ExprKind::Dict(pairs) => {
            for (key, value) in pairs {
                each(key);
                each(value);
            }
        }
        ExprKind::Comprehension { elem, value, iter, cond, .. } => {
            for part in [Some(elem), value.as_ref(), Some(iter), cond.as_ref()].into_iter().flatten() {
                each(part);
            }
        }
        ExprKind::Unary { expr, .. }
        | ExprKind::Attr { object: expr, .. }
        | ExprKind::Lambda { body: expr, .. } => each(expr),
        ExprKind::Binary { lhs, rhs, .. }
        | ExprKind::Index { object: lhs, index: rhs }
        | ExprKind::Else { value: lhs, default: rhs } => {
            each(lhs);
            each(rhs);
        }
        ExprKind::Cond { then, cond, otherwise } => {
            for part in [then, cond, otherwise] {
                each(part);
            }
        }
        ExprKind::Try { value, context } => {
            each(value);
            if let Some(context) = context {
                each(context);
            }
        }
        ExprKind::Call { callee, args } => {
            each(callee);
            for arg in args {
                each(&arg.value);
            }
        }
        ExprKind::Slice { object, start, end } => {
            each(object);
            for part in start.iter().chain(end.iter()) {
                each(part);
            }
        }
        ExprKind::Match { subject, arms } => {
            each(subject);
            for arm in arms {
                arm_names(arm, names);
            }
        }
    }
}

fn arm_names(arm: &MatchArm, names: &mut Vec<String>) {
    if let Pattern::Literal(expr) = &arm.pattern {
        expr_names(expr, names);
    }
    for stmt in &arm.body {
        stmt_names(stmt, names);
    }
}

/// Every name a statement mentions.
pub(super) fn stmt_names(stmt: &Stmt, names: &mut Vec<String>) {
    match &stmt.kind {
        StmtKind::Expr(expr) | StmtKind::Fail(expr) | StmtKind::Yield(expr) | StmtKind::Assert(expr) => {
            expr_names(expr, names);
        }
        StmtKind::Return(value) => {
            if let Some(value) = value {
                expr_names(value, names);
            }
        }
        StmtKind::Assign { target, value, .. } => {
            expr_names(target, names);
            expr_names(value, names);
        }
        StmtKind::If { branches, otherwise } => {
            for (cond, body) in branches {
                expr_names(cond, names);
                for stmt in body {
                    stmt_names(stmt, names);
                }
            }
            for stmt in otherwise.iter().flatten() {
                stmt_names(stmt, names);
            }
        }
        StmtKind::While { cond: expr, body }
        | StmtKind::For { iter: expr, body, .. }
        | StmtKind::With { value: expr, body, .. } => {
            expr_names(expr, names);
            for stmt in body {
                stmt_names(stmt, names);
            }
        }
        StmtKind::Def(def) => {
            for stmt in &def.body {
                stmt_names(stmt, names);
            }
        }
        StmtKind::Break | StmtKind::Continue | StmtKind::Pass => {}
    }
}

/// Whether an expression the interpreter runs can leave the function: it has a `?`, or a
/// `match` arm that returns or fails. Arrow functions are functions of their own.
fn expr_leaves(expr: &Expr) -> bool {
    let mut leaves = false;
    visit(expr, &mut |expr| match &expr.kind {
        ExprKind::Try { .. } => leaves = true,
        ExprKind::Match { arms, .. } => leaves |= arms.iter().any(|arm| arm.body.iter().any(stmt_leaves)),
        _ => {}
    });
    leaves
}

fn stmt_leaves(stmt: &Stmt) -> bool {
    match &stmt.kind {
        StmtKind::Return(_) | StmtKind::Fail(_) => true,
        StmtKind::Expr(expr) | StmtKind::Assert(expr) | StmtKind::Yield(expr) => expr_leaves(expr),
        StmtKind::Assign { target, value, .. } => expr_leaves(target) || expr_leaves(value),
        StmtKind::If { branches, otherwise } => {
            branches.iter().any(|(cond, body)| expr_leaves(cond) || body.iter().any(stmt_leaves))
                || otherwise.iter().flatten().any(stmt_leaves)
        }
        StmtKind::While { cond: expr, body }
        | StmtKind::For { iter: expr, body, .. }
        | StmtKind::With { value: expr, body, .. } => expr_leaves(expr) || body.iter().any(stmt_leaves),
        StmtKind::Def(_) | StmtKind::Break | StmtKind::Continue | StmtKind::Pass => false,
    }
}

/// Calls `f` on an expression and each expression inside it, except inside arrow functions.
fn visit(expr: &Expr, f: &mut impl FnMut(&Expr)) {
    f(expr);
    let mut each = |inner: &Expr| visit(inner, f);
    match &expr.kind {
        ExprKind::FStr(parts) => {
            for part in parts {
                if let FStrPart::Expr { expr, .. } = part {
                    each(expr);
                }
            }
        }
        ExprKind::List(items) | ExprKind::Tuple(items) | ExprKind::Set(items) => {
            for item in items {
                each(item);
            }
        }
        ExprKind::Dict(pairs) => {
            for (key, value) in pairs {
                each(key);
                each(value);
            }
        }
        ExprKind::Comprehension { elem, value, iter, cond, .. } => {
            for part in [Some(elem), value.as_ref(), Some(iter), cond.as_ref()].into_iter().flatten() {
                each(part);
            }
        }
        ExprKind::Unary { expr, .. } | ExprKind::Attr { object: expr, .. } => each(expr),
        ExprKind::Binary { lhs, rhs, .. }
        | ExprKind::Index { object: lhs, index: rhs }
        | ExprKind::Else { value: lhs, default: rhs } => {
            each(lhs);
            each(rhs);
        }
        ExprKind::Cond { then, cond, otherwise } => {
            for part in [then, cond, otherwise] {
                each(part);
            }
        }
        ExprKind::Try { value, context } => {
            each(value);
            if let Some(context) = context {
                each(context);
            }
        }
        ExprKind::Call { callee, args } => {
            each(callee);
            for arg in args {
                each(&arg.value);
            }
        }
        ExprKind::Slice { object, start, end } => {
            each(object);
            for part in start.iter().chain(end.iter()) {
                each(part);
            }
        }
        ExprKind::Match { subject, .. } => each(subject),
        ExprKind::Lambda { .. }
        | ExprKind::Name(_)
        | ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Bool(_)
        | ExprKind::None
        | ExprKind::Variant(_) => {}
    }
}
