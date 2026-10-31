//! Statements: blocks, assignments, `if`, loops, `return`, `fail`, `break` and `continue`.
//! Assigning into a field or item, and the statements the JIT doesn't compile yet, run in
//! the interpreter as one step.

use cranelift_codegen::entity::EntityRef;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{InstBuilder, Value, types};
use fennec_syntax::{Arg, BinOp, Diagnostic, Expr, ExprKind, Span, Stmt, StmtKind};

use super::{Loop, Lowerer};
use crate::helpers::Helper;
use crate::runtime;
use crate::types::{Ty, unsupported};

impl Lowerer<'_, '_> {
    /// Lowers a block's statements, giving back the value of its last one when that's an
    /// expression, an `if` with an `else`, or a `match`.
    pub(super) fn tail(&mut self, stmts: &[Stmt]) -> Result<(Option<Value>, Ty), Diagnostic> {
        // What a statement's `if` proves for the statements after it ends with the block.
        let mark = self.narrow_mark();
        let result = self.tail_of(stmts);
        self.widen(mark);
        result
    }

    fn tail_of(&mut self, stmts: &[Stmt]) -> Result<(Option<Value>, Ty), Diagnostic> {
        let Some((last, rest)) = stmts.split_last() else { return Ok((None, Ty::Unit)) };
        for stmt in rest {
            self.stmt(stmt)?;
            self.drop_temps()?;
        }
        if self.is_dead() {
            return Ok((None, Ty::Unit));
        }
        match &last.kind {
            StmtKind::Expr(expr) => {
                // The block's names may go before its value does.
                let (value, ty) = self.expr(expr)?;
                Ok((value.map(|value| self.own(value, ty)), ty))
            }
            StmtKind::If { branches, otherwise: Some(otherwise) } => self.tail_if(branches, otherwise),
            _ => {
                self.stmt(last)?;
                self.drop_temps()?;
                Ok((None, Ty::Unit))
            }
        }
    }

    /// Lowers a block's statements when nothing wants a value from them: one its last
    /// statement gives is let go.
    pub(super) fn statements(&mut self, stmts: &[Stmt]) -> Result<(), Diagnostic> {
        let (value, ty) = self.tail(stmts)?;
        if self.is_dead() {
            return Ok(());
        }
        self.consume(value, ty)?;
        // A loop comes back here with nothing to end a statement for it, so what the last
        // statement left would pile up, and keep anything it holds from changing in place.
        self.drop_temps()
    }

    /// A nested block, with its own scope, when nothing wants a value from it.
    pub(super) fn block_statements(&mut self, stmts: &[Stmt]) -> Result<(), Diagnostic> {
        self.open_scope();
        self.statements(stmts)?;
        self.close_scope()
    }

    /// A nested block, with its own scope.
    pub(super) fn block_tail(&mut self, stmts: &[Stmt]) -> Result<(Option<Value>, Ty), Diagnostic> {
        self.open_scope();
        let result = self.tail(stmts);
        // A value the block gives back is a temporary, which outlives the block's names.
        let (value, ty) = result?;
        self.close_scope()?;
        Ok((value, ty))
    }

    /// An `if` whose branches all end in a value: the value of whichever branch runs.
    fn tail_if(
        &mut self,
        branches: &[(Expr, Vec<Stmt>)],
        otherwise: &[Stmt],
    ) -> Result<(Option<Value>, Ty), Diagnostic> {
        if let Some(((cond, body), rest)) = branches.split_first()
            && let Some(known) = self.known_test(cond)
        {
            return match (known, rest) {
                (true, _) => self.block_tail(body),
                (false, []) => self.block_tail(otherwise),
                (false, rest) => self.tail_if(rest, otherwise),
            };
        }
        let mut arms = Vec::new();
        let mark = self.narrow_mark();
        for (cond, body) in branches {
            let test = self.condition(cond)?;
            let then = self.b.create_block();
            let next = self.b.create_block();
            self.b.ins().brif(test, then, &[], next, &[]);
            self.b.switch_to_block(then);
            let inner = self.narrow_mark();
            self.narrow(cond, true);
            let (value, ty) = self.block_tail(body)?;
            self.widen(inner);
            arms.extend(self.arm_end(value, ty)?);
            self.b.switch_to_block(next);
            self.narrow(cond, false);
        }
        let (value, ty) = self.block_tail(otherwise)?;
        self.widen(mark);
        arms.extend(self.arm_end(value, ty)?);
        self.join(arms, otherwise.last().map_or(Span::default(), |stmt| stmt.span))
    }

    pub(super) fn stmt(&mut self, stmt: &Stmt) -> Result<(), Diagnostic> {
        self.last_try = Self::last_try_in(stmt);
        match &stmt.kind {
            StmtKind::Expr(expr) => {
                let (value, ty) = self.expr(expr)?;
                self.consume(value, ty)
            }
            StmtKind::Assign { target, op, value, declare_mut, .. } => {
                self.assign(target, *op, value, *declare_mut)
            }
            StmtKind::If { branches, otherwise } => self.if_stmt(branches, otherwise.as_deref()),
            StmtKind::While { cond, body } => self.while_loop(cond, body),
            StmtKind::For { names, is_mut, iter, body } if self.for_packed(names, iter, body, *is_mut)? => {
                Ok(())
            }
            StmtKind::For { names, is_mut: false, iter, body } => self.for_loop(names, iter, body),
            StmtKind::Return(value) => {
                let (value, ty) = match value {
                    Some(value) => self.expr(value)?,
                    None => (None, Ty::Unit),
                };
                self.ret(value, ty, stmt.span)
            }
            StmtKind::Fail(error) => {
                let error = self.expr(error)?;
                let handle = self.handle_of(error.0, error.1, stmt.span)?;
                let [start, end] = self.span_args(stmt.span);
                let failed = self.help(Helper::Err, &[handle, start, end])?;
                self.pass_up(failed)
            }
            StmtKind::Break | StmtKind::Continue => {
                let target = self.loops.last().map(|lp| {
                    (if matches!(stmt.kind, StmtKind::Break) { lp.done } else { lp.next }, lp.depth)
                });
                let (target, depth) = target
                    .ok_or_else(|| Diagnostic::new("`break` or `continue` outside a loop", stmt.span))?;
                self.unwind_scopes(depth)?;
                self.b.ins().jump(target, &[]);
                self.after_terminator();
                Ok(())
            }
            StmtKind::Pass => Ok(()),
            StmtKind::For { names, is_mut: true, iter, body } if self.loops_inside(iter, body) => {
                self.for_mut_inside(names, iter, body)
            }
            StmtKind::For { names, is_mut: true, iter, body } => {
                match (names.as_slice(), self.field_path(iter)) {
                    ([name], Some(path)) => self.for_mut(name, path, iter, body),
                    _ => self.interpret_stmt(stmt),
                }
            }
            StmtKind::Assert(condition) => self.interpret_assert(condition, stmt.span),
            StmtKind::With { .. } => self.interpret_stmt(stmt),
            StmtKind::Yield(_) | StmtKind::Def(_) => Err(unsupported("this kind of statement", stmt.span)),
        }
    }

    pub(super) fn assign(
        &mut self,
        target: &Expr,
        op: Option<BinOp>,
        value: &Expr,
        declare_mut: bool,
    ) -> Result<(), Diagnostic> {
        match &target.kind {
            ExprKind::Name(name) => match self.local(name) {
                Some(local @ super::Local { ty: Ty::Str, var: Some(var), .. })
                    if op == Some(BinOp::Add)
                        && !declare_mut
                        && !self.borrowed_names.contains(&var.index()) =>
                {
                    self.append(name, local, target, value)
                }
                Some(local) if !declare_mut => {
                    let (new, new_ty) = self.assigned(target, op, value)?;
                    self.set_local(name, local, new, new_ty, target.span)
                }
                _ if self.packed_comprehension(name, value, declare_mut)? => Ok(()),
                _ => {
                    let made = match self.literal_for(name, value)? {
                        Some(made) => Some(made),
                        None => self.list_literal_for(name, value)?,
                    };
                    let (new, new_ty) = match made {
                        Some(made) => made,
                        None => self.expr(value)?,
                    };
                    self.declare(name, new, new_ty, declare_mut, value)
                }
            },
            ExprKind::Tuple(names) => self.unpack(names, value, target.span),
            ExprKind::Index { object, index } if let Some((var, (key, item))) = self.dict_target(object) => {
                self.assign_dict((var, key, item), target, index, op, value)
            }
            ExprKind::Index { object, index } if let Some((name, item)) = self.packed(object) => {
                let span = target.span.to(value.span);
                let new = self.assigned(target, op, value)?;
                let address = self.item_address(&name, item, index, true, target.span)?;
                self.store_item(address, item, new, span)
            }
            ExprKind::Attr { object, name, optional: false }
                if let Some((var, field)) = self.boxed_target(object, name)? =>
            {
                let span = target.span.to(value.span);
                let new = self.assigned(target, op, value)?;
                self.write_boxed_field(var, field, new, span)
            }
            _ if self.changes_inside(target) => self.assign_inside(target, op, value),
            _ if let Some((place, id, field)) = self.struct_target(target)? => {
                let span = target.span.to(value.span);
                let new = self.assigned(target, op, value)?;
                self.write_field(&place, id, field, new, span)
            }
            _ => match self.field_path(target) {
                Some((root, fields)) if !fields.is_empty() => {
                    self.assign_path(&root, &fields, target, op, value)
                }
                _ => self.interpret_assign(target, op, value, declare_mut, target.span),
            },
        }
    }

    /// `name += value` for a string the name owns, appended in place when the block has
    /// room, so building a string a piece at a time isn't quadratic.
    fn append(
        &mut self,
        name: &str,
        local: super::Local,
        target: &Expr,
        value: &Expr,
    ) -> Result<(), Diagnostic> {
        let span = target.span.to(value.span);
        let more = self.expr(value)?;
        let (Some(more), Ty::Str, Some(var)) = (more.0, more.1, local.var) else {
            let text = self.expr(target)?;
            let (new, new_ty) = self.operate(BinOp::Add, text, more, span)?;
            return self.set_local(name, local, new, new_ty, span);
        };
        // The helper takes over the name's owner, so the old value isn't released here.
        let text = self.b.use_var(var);
        let joined = self.append_text(text, more)?;
        self.b.def_var(var, joined);
        self.consume(Some(more), Ty::Str)
    }

    /// The value an assignment stores: `value`, or `target op value` for `+=` and the like.
    pub(super) fn assigned(
        &mut self,
        target: &Expr,
        op: Option<BinOp>,
        value: &Expr,
    ) -> Result<super::expr::Lowered, Diagnostic> {
        if let (None, ExprKind::Name(name)) = (op, &target.kind)
            && let Some(made) = match self.literal_for(name, value)? {
                Some(made) => Some(made),
                None => self.list_literal_for(name, value)?,
            }
        {
            return Ok(made);
        }
        let Some(op) = op else { return self.expr(value) };
        let combined = Expr {
            kind: ExprKind::Binary { op, lhs: Box::new(target.clone()), rhs: Box::new(value.clone()) },
            span: target.span.to(value.span),
        };
        self.expr(&combined)
    }

    /// Creates a name. A name copied from another object name gets its own copy.
    pub(super) fn declare(
        &mut self,
        name: &str,
        value: Option<Value>,
        ty: Ty,
        is_mut: bool,
        source: &Expr,
    ) -> Result<(), Diagnostic> {
        // An object the checker knows is a plain struct is kept natively from here on.
        let (value, ty) = match (ty, self.checked_ty(name)) {
            (Ty::Obj, Some(to @ (Ty::Struct(_) | Ty::Boxed(_) | Ty::Str | Ty::Dict(..) | Ty::List(_)))) => {
                (Some(self.convert(value, ty, to, source.span)?), to)
            }
            _ => (value, ty),
        };
        match ty {
            Ty::Obj => {
                let handle = value.ok_or_else(|| unsupported("this value", source.span))?;
                self.declare_object(name, handle, is_mut)
            }
            Ty::Struct(id) => {
                let from = value.ok_or_else(|| unsupported("this value", source.span))?;
                self.declare_struct(name, from, id, is_mut)
            }
            _ => {
                let value = value.map(|value| self.own(value, ty));
                self.bind_native(name, ty, value, is_mut);
                Ok(())
            }
        }
    }

    /// How native code holds what the checker says a name in this function holds.
    pub(super) fn checked_ty(&self, name: &str) -> Option<Ty> {
        let signatures = self.signatures;
        signatures.local(self.current_key(), name).map(|ty| self.layouts.ty(ty))
    }

    /// Changes what a name holds.
    pub(super) fn set_local(
        &mut self,
        name: &str,
        local: super::Local,
        value: Option<Value>,
        ty: Ty,
        span: Span,
    ) -> Result<(), Diagnostic> {
        match (local.var, local.ty) {
            (_, Ty::Obj) => {
                let handle = self.handle_of(value, ty, span)?;
                let id = self.name_id(name);
                self.call_helper(Helper::Assign, &[id, handle]).map(|_| ())
            }
            (Some(_), Ty::Struct(id)) => {
                let converted = self.convert(value, ty, Ty::Struct(id), span)?;
                let place = self.local_place(local).ok_or_else(|| unsupported("this struct", span))?;
                self.write_place(&place, converted, id)
            }
            (Some(var), local_ty) if local_ty.counted() => {
                let converted = self.convert(value, ty, local_ty, span)?;
                let new = self.own(converted, local_ty);
                self.forget_var(var);
                let old = self.b.use_var(var);
                self.b.def_var(var, new);
                self.release(old)
            }
            (Some(var), local_ty) => {
                let converted = self.convert(value, ty, local_ty, span)?;
                self.forget_var(var);
                self.b.def_var(var, converted);
                Ok(())
            }
            (None, _) => Ok(()),
        }
    }

    /// `a, b = value`: takes a tuple apart into names.
    fn unpack(&mut self, names: &[Expr], value: &Expr, span: Span) -> Result<(), Diagnostic> {
        let (lowered, ty) = self.expr(value)?;
        let whole = self.handle_of(lowered, ty, value.span)?;
        let names: Vec<&str> = names
            .iter()
            .map(|name| match &name.kind {
                ExprKind::Name(name) => Ok(name.as_str()),
                _ => Err(unsupported("unpacking into this", span)),
            })
            .collect::<Result<_, _>>()?;
        self.unpack_into(&names, whole, false, value.span)
    }

    /// Declares or changes names from the items of a tuple.
    fn unpack_into(
        &mut self,
        names: &[&str],
        whole: Value,
        for_loop: bool,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let count = self.int(i64::try_from(names.len()).unwrap_or(0));
        let for_loop_names = for_loop;
        let for_loop = self.b.ins().iconst(types::I8, i64::from(for_loop));
        for (at, name) in names.iter().enumerate() {
            let at = self.int(i64::try_from(at).unwrap_or(0));
            let [start, end] = self.span_args(span);
            let item = self.help(Helper::Unpack, &[whole, at, count, for_loop, start, end])?;
            // A loop's names are new each time round, like the interpreter's.
            match self.local(name).filter(|_| !for_loop_names) {
                Some(local) => self.set_local(name, local, Some(item), Ty::Obj, span)?,
                None => self.declare_object(name, item, false)?,
            }
        }
        Ok(())
    }

    fn if_stmt(
        &mut self,
        branches: &[(Expr, Vec<Stmt>)],
        otherwise: Option<&[Stmt]>,
    ) -> Result<(), Diagnostic> {
        if let Some(((cond, body), rest)) = branches.split_first()
            && let Some(known) = self.known_test(cond)
        {
            return match (known, rest, otherwise) {
                (true, _, _) => self.block_statements(body),
                (false, [], Some(otherwise)) => self.block_statements(otherwise),
                (false, [], None) => Ok(()),
                (false, rest, otherwise) => self.if_stmt(rest, otherwise),
            };
        }
        let done = self.b.create_block();
        let mark = self.narrow_mark();
        let mut all_leave = true;
        for (cond, body) in branches {
            let test = self.condition(cond)?;
            let then = self.b.create_block();
            let next = self.b.create_block();
            self.b.ins().brif(test, then, &[], next, &[]);
            self.b.switch_to_block(then);
            let inner = self.narrow_mark();
            self.narrow(cond, true);
            self.block_statements(body)?;
            self.widen(inner);
            if !self.is_dead() {
                all_leave = false;
                self.b.ins().jump(done, &[]);
            }
            self.b.switch_to_block(next);
            self.narrow(cond, false);
        }
        if let Some(body) = otherwise {
            self.block_statements(body)?;
        }
        if !self.is_dead() {
            self.b.ins().jump(done, &[]);
        }
        self.b.switch_to_block(done);
        // When every branch left, what got past them is all that reaches the next statement.
        if !all_leave {
            self.widen(mark);
        }
        Ok(())
    }

    fn while_loop(&mut self, cond: &Expr, body: &[Stmt]) -> Result<(), Diagnostic> {
        let bottom = self.b.create_block();
        let inside = self.b.create_block();
        let exit = self.b.create_block();
        let hoisted = self.hoist_lists(&[], Some(cond), body)?;
        // The test comes first, then again at the bottom, so going round takes one jump.
        let test = self.condition(cond)?;
        self.drop_temps()?;
        self.b.ins().brif(test, inside, &[], exit, &[]);
        self.b.switch_to_block(inside);
        self.loops.push(Loop { next: bottom, done: exit, depth: self.scopes.len(), put_back: None });
        let lowered = self.block_statements(body);
        self.loops.pop();
        self.unhoist(hoisted);
        lowered?;
        self.drop_temps()?;
        if !self.is_dead() {
            self.b.ins().jump(bottom, &[]);
        }
        self.b.switch_to_block(bottom);
        let test = self.condition(cond)?;
        self.drop_temps()?;
        self.b.ins().brif(test, inside, &[], exit, &[]);
        self.b.switch_to_block(exit);
        Ok(())
    }

    fn for_loop(&mut self, names: &[String], iter: &Expr, body: &[Stmt]) -> Result<(), Diagnostic> {
        if let (ExprKind::Call { callee, args }, [name]) = (&iter.kind, names)
            && matches!(&callee.kind, ExprKind::Name(callee) if callee == "range" && self.local(callee).is_none())
            && args.iter().all(|arg| arg.name.is_none() && !arg.is_mut)
        {
            let mut lower = |lowerer: &mut Self| lowerer.for_range(name, args, iter.span, body);
            if self.unswitched(body, &mut lower)? {
                return Ok(());
            }
            return self.for_range(name, args, iter.span, body);
        }
        if self.for_paired(names, iter, body)? {
            return Ok(());
        }
        self.for_items(names, iter, body)
    }

    /// `for i, item in enumerate(xs)` and `for a, b in zip(xs, ys)` over native lists, as a
    /// count over positions. The lists have to be names the body doesn't mention, so nothing
    /// changes them, and the loop's names new, so nothing outside can see them.
    fn for_paired(&mut self, names: &[String], iter: &Expr, body: &[Stmt]) -> Result<bool, Diagnostic> {
        let ExprKind::Call { callee, args } = &iter.kind else { return Ok(false) };
        let ExprKind::Name(function) = &callee.kind else { return Ok(false) };
        let paired = matches!(
            (function.as_str(), names, args.as_slice()),
            ("enumerate", [_, _], [_]) | ("zip", [_, _], [_, _])
        );
        if !paired
            || self.local(function).is_some()
            || self.shared.compiled.contains_key(function.as_str())
            || names.iter().any(|name| self.local(name).is_some())
            || args.iter().any(|arg| arg.name.is_some() || arg.is_mut)
        {
            return Ok(false);
        }
        let mut lists = Vec::with_capacity(args.len());
        for arg in args {
            let Some((list, item)) = self.packed(&arg.value) else { return Ok(false) };
            if Self::body_names(body, &list) {
                return Ok(false);
            }
            lists.push((list, item));
        }
        let span = iter.span;
        let mut shortest = None;
        for (list, item) in &lists {
            let length = self.list_length(list, *item)?;
            shortest = Some(match shortest {
                Some(other) => self.b.ins().smin(other, length),
                None => length,
            });
        }
        let at = if function == "zip" {
            format!("\0at{}", span.start)
        } else {
            names.first().cloned().unwrap_or_default()
        };
        let count = format!("\0count{}", span.start);
        self.open_scope();
        self.bind_native(&count, Ty::Int, shortest, false);
        let name_of = |name: &str| Expr { kind: ExprKind::Name(name.to_owned()), span };
        let bound = names.iter().skip(usize::from(function == "enumerate"));
        let mut lowered: Vec<Stmt> = bound
            .zip(&lists)
            .map(|(name, (list, _))| {
                let index =
                    ExprKind::Index { object: Box::new(name_of(list)), index: Box::new(name_of(&at)) };
                let kind = StmtKind::Assign {
                    target: name_of(name),
                    op: None,
                    value: Expr { kind: index, span },
                    declare_mut: false,
                    ty: None,
                };
                Stmt { kind, span }
            })
            .collect();
        lowered.extend_from_slice(body);
        let bound = [Arg { name: None, is_mut: false, value: name_of(&count) }];
        self.for_range(&at, &bound, span, &lowered)?;
        self.close_scope()?;
        Ok(true)
    }

    /// `for name in range(...)`, counted natively.
    fn for_range(&mut self, name: &str, args: &[Arg], span: Span, body: &[Stmt]) -> Result<(), Diagnostic> {
        let mut bounds = Vec::new();
        for arg in args {
            let (value, ty) = self.expr(&arg.value)?;
            bounds.push(self.convert(value, ty, Ty::Int, arg.value.span)?);
        }
        self.drop_temps()?;
        let zero = self.int(0);
        let one = self.int(1);
        let (start, end, step) = match bounds.as_slice() {
            [end] => (zero, *end, one),
            [start, end] => (*start, *end, one),
            [start, end, step] => (*start, *end, *step),
            _ => return Err(unsupported("`range` with these arguments", span)),
        };
        let zero_step = self.b.ins().icmp_imm_s(IntCC::Equal, step, 0);
        self.fail_if(zero_step, runtime::ZERO_STEP, span);
        let counter = self.b.declare_var(types::I64);
        self.b.def_var(counter, start);
        let inside = self.b.create_block();
        let advance = self.b.create_block();
        let exit = self.b.create_block();
        let hoisted = self.hoist_lists(&[name.to_owned()], None, body)?;
        // The test comes first, then again at the bottom, so going round takes one jump.
        let first = self.in_range(start, end, step);
        self.b.ins().brif(first, inside, &[], exit, &[]);
        self.b.switch_to_block(inside);
        let at = self.b.use_var(counter);
        let counts_up = self.constant(step) == Some(1);
        if counts_up {
            let (low, high) = (self.range(start), self.range(end));
            self.note_range(at, (low.0, high.1.saturating_sub(1).max(low.1)));
        }
        let depth = self.scopes.len();
        self.open_scope();
        self.bind_native(name, Ty::Int, Some(at), false);
        self.loops.push(Loop { next: advance, done: exit, depth, put_back: None });
        if counts_up {
            self.push_below(at, end);
        }
        let lowered = self.statements(body);
        if counts_up {
            self.pop_below();
        }
        self.loops.pop();
        self.unhoist(hoisted);
        lowered?;
        self.close_scope()?;
        if !self.is_dead() {
            self.b.ins().jump(advance, &[]);
        }
        self.b.switch_to_block(advance);
        let at = self.b.use_var(counter);
        let next = self.b.ins().iadd(at, step);
        self.b.def_var(counter, next);
        let more = self.in_range(next, end, step);
        self.b.ins().brif(more, inside, &[], exit, &[]);
        self.b.switch_to_block(exit);
        Ok(())
    }

    /// Whether a `range` going by `step` hasn't reached `end` at `at`.
    fn in_range(&mut self, at: Value, end: Value, step: Value) -> Value {
        if self.constant(step) == Some(1) {
            return self.b.ins().icmp(IntCC::SignedLessThan, at, end);
        }
        let up = self.b.ins().icmp_imm_s(IntCC::SignedGreaterThan, step, 0);
        let below = self.b.ins().icmp(IntCC::SignedLessThan, at, end);
        let above = self.b.ins().icmp(IntCC::SignedGreaterThan, at, end);
        self.b.ins().select(up, below, above)
    }

    /// `for item in items`: the runtime lists the items, which a hidden name holds while
    /// the loop counts through them.
    fn for_items(&mut self, names: &[String], iter: &Expr, body: &[Stmt]) -> Result<(), Diagnostic> {
        if let Some((object, walk)) = self.dict_walk(iter, names) {
            let (dict, ty) = self.expr(object)?;
            if let (Some(dict), Ty::Dict(key, value)) = (dict, ty) {
                return self.for_dict(names, (dict, key, value), walk, iter.span, body);
            }
        }
        let (value, ty) = self.expr(iter)?;
        if let (Some(text), Ty::Str, [name]) = (value, ty, names) {
            return self.for_text(name, text, iter.span, body);
        }
        self.note(iter.span, "the runtime lists the items a `for` loop goes over");
        let source = self.handle_of(value, ty, iter.span)?;
        let [start, end] = self.span_args(iter.span);
        let items = self.help(Helper::Items, &[source, start, end])?;
        self.open_scope();
        let hidden = format!("\0for{}", iter.span.start);
        self.declare_object(&hidden, items, false)?;
        let hidden_id = self.name_id(&hidden);
        let list = self.help(Helper::Get, &[hidden_id])?;
        let count = self.help(Helper::Count, &[list])?;
        self.drop_temps()?;
        let counter = self.b.declare_var(types::I64);
        let zero = self.int(0);
        self.b.def_var(counter, zero);
        let header = self.b.create_block();
        let inside = self.b.create_block();
        let advance = self.b.create_block();
        let exit = self.b.create_block();
        let hoisted = self.hoist_lists(names, None, body)?;
        self.b.ins().jump(header, &[]);
        self.b.switch_to_block(header);
        let at = self.b.use_var(counter);
        let test = self.b.ins().icmp(IntCC::SignedLessThan, at, count);
        self.b.ins().brif(test, inside, &[], exit, &[]);
        self.b.switch_to_block(inside);
        let depth = self.scopes.len();
        self.open_scope();
        let list = self.help(Helper::Get, &[hidden_id])?;
        let item = self.help(Helper::Item, &[list, at])?;
        if let [name] = names {
            self.declare(name, Some(item), Ty::Obj, false, iter)?;
        } else {
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            self.unpack_into(&names, item, true, iter.span)?;
        }
        self.drop_temps()?;
        self.loops.push(Loop { next: advance, done: exit, depth, put_back: None });
        let lowered = self.statements(body);
        self.loops.pop();
        self.unhoist(hoisted);
        lowered?;
        self.close_scope()?;
        if !self.is_dead() {
            self.b.ins().jump(advance, &[]);
        }
        self.b.switch_to_block(advance);
        let at = self.b.use_var(counter);
        let next = self.b.ins().iadd_imm_s(at, 1);
        self.b.def_var(counter, next);
        self.b.ins().jump(header, &[]);
        self.b.switch_to_block(exit);
        self.close_scope()
    }
}
